use super::*;
use skylive_core::{Session, StreamingSettings, StudioSnapshot};
use std::net::ToSocketAddrs;

// A DNS outage after a successful connection should enter the same bounded
// reconnect loop as a transient socket failure. Windows' resolver reports
// WSATRY_AGAIN (11002) as a raw Winsock error, which is not necessarily
// classified as TimedOut by std::io. Keep permanent lookup failures terminal.
fn resolve_error(error: std::io::Error) -> Error {
    let retryable = matches!(error.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted)
        || cfg!(target_os = "windows") && error.raw_os_error() == Some(11002);
    if retryable { Error::RetryableTransport(error.to_string()) }
    else { Error::Transport(error.to_string()) }
}
/// Immutable captured profile-format/output-settings lease. Core is the only
/// URL/settings/GOP parser; no DNS/socket or native call occurs in construction.
#[derive(Clone, Debug)]
pub struct Config {
    pub(crate) session: u64,
    pub(crate) profile: ProfileId,
    pub(crate) video: VideoSettings,
    pub(crate) audio: Option<AudioSettings>,
    pub(crate) settings: StreamingSettings,
    pub(crate) stop:Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl Config {
    pub fn from_session(session: &Session) -> Result<Self, Error> {
        Self::from_snapshot(session.identity(), &session.snapshot())
    }
    pub fn from_snapshot(session: u64, snapshot: &StudioSnapshot) -> Result<Self, Error> {
        Self::new(
            session,
            snapshot.profiles().selected_id(),
            snapshot.output_video().map_err(Error::Configuration)?,
            snapshot.audio(),
            snapshot.profiles().selected().streaming().clone(),
        )
    }
    pub fn new(
        session: u64,
        profile: ProfileId,
        video: VideoSettings,
        project_audio: AudioSettings,
        settings: StreamingSettings,
    ) -> Result<Self, Error> {
        if session == 0 {
            return Err(Error::Invalid("zero session"));
        }
        settings.validate().map_err(Error::Configuration)?;
        if !settings.is_configured() {
            return Err(Error::Invalid(
                "configure a streaming server and key before Start",
            ));
        }
        let value = Self {
            session,
            profile,
            video,
            stop:None,
            audio: settings.audio_enabled().then_some(project_audio),
            settings,
        };
        value.video_config(StreamKey { session, output: 1 })?;
        value.audio_config(StreamKey { session, output: 1 })?;
        let pixels = u64::from(value.video.width()) * u64::from(value.video.height());
        let rgba = pixels
            .checked_mul(4)
            .ok_or(Error::Limit("RGBA byte arithmetic"))?;
        let nv12 = pixels
            .checked_add(pixels / 2)
            .ok_or(Error::Limit("NV12 byte arithmetic"))?;
        if rgba.checked_add(nv12).is_none_or(|n| n > 50 * 1024 * 1024) {
            return Err(Error::Limit("serial RGBA+NV12 exceeds50MiB"));
        }
        Ok(value)
    }
    pub fn blocks_command(&self,command:&skylive_core::Command)->bool{use skylive_core::{Command,ProfileCommand};match command{
        Command::ConfigureAdvancedSettings{profile,reconnect,..}=>*profile!=self.profile||*reconnect!=self.settings.reconnect(),
        Command::Profile{command:ProfileCommand::SetStreaming{..}|ProfileCommand::SetReconnect{..}}=>true,
        _=>crate::recording::blocks_format_change(command),
    }}
    pub fn session(&self) -> u64 {
        self.session
    }
    pub fn profile(&self) -> ProfileId {
        self.profile
    }
    pub fn video(&self) -> &VideoSettings {
        &self.video
    }
    pub fn audio(&self) -> Option<AudioSettings> {
        self.audio
    }
    pub fn settings(&self) -> &StreamingSettings {
        &self.settings
    }
    pub(crate) fn resolve(&self) -> Result<crate::rtmp::ResolvedTarget, Error> {
        let endpoint = self
            .settings
            .endpoint()
            .map_err(|e| Error::Transport(e.to_string()))?
            .ok_or(Error::Invalid("streaming endpoint missing"))?;
        // A platform DNS resolver can block. The service lease stays held until
        // it returns; cancellation never claims this work has already retired.
        let resolved = (endpoint.host(), endpoint.port())
            .to_socket_addrs()
            .map_err(resolve_error)?;
        let mut addresses = Vec::new();
        for address in resolved.take(64) {
            if !addresses.contains(&address) { addresses.push(address); }
            if addresses.len()==16 { break; }
        }
        let address = *addresses.first().ok_or(Error::Transport("DNS returned no address".into()))?;
        crate::rtmp::ResolvedTarget::new(
            address,
            endpoint.app().to_owned(),
            self.settings.server_url().to_owned(),
            self.settings.stream_key().to_owned(),
            None,
            None,
        )
        .and_then(|target|target.with_addresses(addresses))
        .map_err(|e| Error::Transport(e.to_string()))
    }
    pub(crate) fn video_config(&self, key: StreamKey) -> Result<crate::native_h264::Config, Error> {
        let g = crate::native_h264::Generation {
            session: key.session,
            output: key.output,
            encoder: 1,
        };
        let bits = self
            .settings
            .video_bitrate_kbps()
            .checked_mul(1000)
            .ok_or(Error::Invalid("video bitrate"))?;
        let gop = self
            .settings
            .gop_frames(self.video.fps_num(), self.video.fps_den())
            .map_err(|e| Error::H264(e.to_string()))?;
        crate::native_h264::Config::new(
            g,
            self.video.width(),
            self.video.height(),
            self.video.fps_num(),
            self.video.fps_den(),
            bits,
            gop,
        )
        .and_then(|config|config.with_encoder(self.settings.encoder()))
        .map_err(|e| Error::H264(e.to_string()))
    }
    pub(crate) fn audio_config(
        &self,
        key: StreamKey,
    ) -> Result<Option<crate::native_aac::Config>, Error> {
        self.audio
            .map(|settings| {
                let channels = if settings.channels() == 1 { 1 } else { 2 };
                let bits = self
                    .settings
                    .audio_bitrate_kbps()
                    .checked_mul(1000)
                    .ok_or(Error::Invalid("audio bitrate"))?;
                crate::native_aac::Config::new(
                    crate::native_aac::Generation {
                        session: key.session,
                        output: key.output,
                        encoder: 2,
                    },
                    channels,
                    bits,
                )
                .map_err(|e| Error::Aac(e.to_string()))
            })
            .transpose()
    }
    pub(crate) fn validate_frame(
        &self,
        frame: &ProgramVideoFrame,
        origin: Option<MediaTime>,
    ) -> Result<u64, Error> {
        if frame.session != self.session || frame.playback.session() != self.session {
            return Err(Error::Scope);
        }
        if frame.model.profiles().selected().streaming()!=&self.settings
            || frame.model.profiles().selected_id() != self.profile
            || !frame.model.output_video().is_ok_and(|video|video==self.video)
            || self.audio.is_some_and(|a| {
                let b = frame.model.audio();
                a.sample_rate() != b.sample_rate() || a.speakers() != b.speakers()
            })
        {
            return Err(Error::Format);
        }
        crate::preview::OutputRenderer::validate_profile_output_inputs(
            &frame.model,
            &frame.output,
            &frame.playback,
            &frame.capture,
        )
        .map_err(|e| Error::Render(e.to_string()))?;
        let elapsed = frame
            .time
            .0
            .checked_sub(origin.unwrap_or(frame.time).0)
            .ok_or(Error::Invalid("video before common origin"))?;
        u64::try_from(
            u128::from(elapsed) * u128::from(self.video.fps_num())
                / (1_000_000_000 * u128::from(self.video.fps_den())),
        )
        .map_err(|_| Error::Limit("video index"))
    }
}
