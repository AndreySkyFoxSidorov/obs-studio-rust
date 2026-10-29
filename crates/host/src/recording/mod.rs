//! Owned recording output ports. All native calls and file publication run on
//! the recording worker; UI/CLI send bounded frames and control requests.
use serde::Serialize;
use skylive_core::{Session, VideoSettings};
use skylive_media::RgbaFrame;
use std::{
    fmt,
    path::{Path, PathBuf},
};

mod audio_sink;
mod camera_receipt;
mod ndi_receipt;
pub use ndi_receipt::NdiVideoReceipt;
pub use camera_receipt::CameraReceipt;
#[cfg(all(test, not(target_os = "macos")))]
#[path = "native_macos/layout.rs"]
mod macos_layout_contract;
#[cfg(any(target_os = "windows", test))]
mod mp4_color;
#[cfg(target_os="windows")]
mod metadata_spool;
#[cfg(target_os="windows")]
mod packet_mux;
#[cfg(target_os = "macos")]
mod native_macos;
#[cfg(target_os = "windows")]
mod native_windows;
mod owner;
mod multitrack;
pub use multitrack::{MultitrackRecordingConfig,RecordingTrackConfig,native_multitrack_factory};
mod pause;
pub use pause::{RecordingControlKey,RecordingPauseReceipt};
mod prepared_video;
pub use prepared_video::{MAX_PREPARED_VIDEO_BYTES, PreparedVideo};
mod warnings;
pub use audio_sink::RecordingAudioSink;
pub(crate) use audio_sink::block_charge as program_block_charge;
pub use owner::{RecordingEvent, RecordingFrame, RecordingOwner, RecordingStatus};
pub use warnings::{
    MAX_RECORDING_WARNING_BYTES, MAX_RECORDING_WARNINGS, RecordingSourceName, RecordingWarnings,
};
pub const fn available() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
}
/// Resolve the platform default without filesystem IO or storing an OS path in
/// the portable profile. Creation/validation remains on the recording owner.
pub fn default_directory() -> PathBuf {
    default_directory_for(
        std::env::consts::OS,
        std::env::var_os("USERPROFILE")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    )
}
fn default_directory_for(os: &str, profile: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    let (base, folder) = match os {
        "windows" => (profile.or(home), "Videos"),
        "macos" => (home, "Movies"),
        _ => (home, "Videos"),
    };
    base.map(|path| path.join(folder).join("SkyLive"))
        .unwrap_or_else(|| PathBuf::from("recordings"))
}
#[cfg(test)]
mod default_directory_tests {
    use super::*;
    #[test]
    fn platform_default_uses_movies_on_mac_and_ignores_foreign_windows_environment() {
        let profile = Some(PathBuf::from("C:/Users/test"));
        let home = Some(PathBuf::from("/Users/test"));
        assert_eq!(
            default_directory_for("windows", profile.clone(), home.clone()),
            PathBuf::from("C:/Users/test").join("Videos/SkyLive")
        );
        assert_eq!(
            default_directory_for("macos", profile, home.clone()),
            PathBuf::from("/Users/test").join("Movies/SkyLive")
        );
        assert_eq!(
            default_directory_for("linux", None, home),
            PathBuf::from("/Users/test").join("Videos/SkyLive")
        );
        assert_eq!(
            default_directory_for("macos", None, None),
            PathBuf::from("recordings")
        );
    }
}
pub fn container_available(container:skylive_core::RecordingContainer)->bool{match container{skylive_core::RecordingContainer::Mp4=>available(),skylive_core::RecordingContainer::Matroska=>cfg!(target_os="windows")}}
pub fn capabilities() -> serde_json::Value {
    serde_json::json!({"available":available(),"container":"mp4","containers":if cfg!(target_os="windows"){vec!["mp4","matroska"]}else if available(){vec!["mp4"]}else{Vec::<&str>::new()},"matroska":{"compiled":cfg!(target_os="windows"),"extension":"mkv","scope":"h264_8bit_no_b_aac_lc_sdr","timebase_ns":100,"input_demux":false,"remux":false,"native_verified":false},"video":"h264_sdr_bt709","profiles":["software_high","nvenc_baseline"],"audio":"aac_lc_mono_or_stereo","rate_control":"average_bitrate","hardware_preference":false,"encoder_default":"software","nvenc_h264":{"compiled":cfg!(all(target_os="windows",target_arch="x86_64")),"availability":"explicit_probe_or_start","input":"nv12","profile":"baseline","rate_control":"cbr","b_frames":0,"adapter":"exact_luid"},"multitrack":{"native_backend_compiled":cfg!(target_os="windows"),"maximum_audio_tracks":6,"default_track":1,"program_producer_attached":true,"typed_pcm_feed":true,"physical_aac_context_limit":8,"replay_audio_track_limit":1,"remux_audio_track_limit":1,"native_verified":false},"native_verified":false,"replay_buffer":crate::replay_buffer::available()})
}
/// Frontends reject a format/profile swap before publishing it while a writer
/// is admitted. Source gain/mute/effects and next-use recording choices stay live.
pub fn blocks_format_change(command: &skylive_core::Command) -> bool {
    if matches!(command, skylive_core::Command::Setup { command } if command.changes_format()) {
        return true;
    }
    use skylive_core::{Command, ProfileCommand};
    matches!(
        command,
        Command::SetVideo { .. }
            | Command::ConfigureVideo { .. }
            | Command::ResizeOutputToSource { .. }
            | Command::SetAudioSettings { .. }
            | Command::Profile {
                command: ProfileCommand::Select { .. }
                    | ProfileCommand::Import { .. }
                    | ProfileCommand::New { .. }
                    | ProfileCommand::Duplicate { .. }
                    | ProfileCommand::Remove { .. }
            }
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordingConfig {
    pub encoder:skylive_core::VideoEncoder,
    pub keyframe_interval_seconds:u32,
    pub path: PathBuf,
    pub video: VideoSettings,
    pub video_bitrate_kbps: u32,
    pub audio: Option<RecordingAudio>,
    pub prefer_hardware: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingAudio {
    pub sample_rate: u32,
    pub channels: u8,
    pub bitrate_kbps: u32,
}
impl RecordingConfig {
    pub fn from_session(session: &Session, path: PathBuf) -> Result<Self, RecordingError> {
        let model=session.snapshot();let settings=model.profiles().selected().recording();if settings.audio_enabled()&&!settings.audio_tracks().is_default(){return Err(RecordingError::invalid("selected recording tracks require the typed multitrack producer; legacy Program recording supports default Track1 only"));}Self::from_session_base(session,path)
    }
    pub(crate) fn from_session_base(session:&Session,path:PathBuf)->Result<Self,RecordingError>{
        let model = session.snapshot();
        let selected = model.profiles().selected();
        let settings = selected.recording();
        if !path.extension().and_then(std::ffi::OsStr::to_str).is_some_and(|extension|extension.eq_ignore_ascii_case(settings.container().extension())){return Err(RecordingError::invalid("recording path extension differs from the selected profile container"));}
        let value = Self {
            path,
            encoder:settings.encoder(),
            keyframe_interval_seconds:settings.keyframe_interval_seconds(),
            video: model.output_video().map_err(|error|RecordingError::native("profile output format",error))?,
            video_bitrate_kbps: settings.video_bitrate_kbps(),
            audio: settings.audio_enabled().then_some(RecordingAudio {
                sample_rate: model.audio().sample_rate(),
                channels: if model.audio().speakers().channels() == 1 {
                    1
                } else {
                    2
                },
                bitrate_kbps: settings.audio_bitrate_kbps(),
            }),
            prefer_hardware: settings.prefer_hardware(),
        };
        value.validate()?;
        Ok(value)
    }
    /// Preserve existing public config constructors/literals: their explicit
    /// path identifies the actual container, while from_session also fences it
    /// against the canonical selected profile setting.
    pub fn container(&self)->Result<skylive_core::RecordingContainer,RecordingError>{
        match self.path.extension().and_then(|s|s.to_str()).map(|s|s.to_ascii_lowercase()).as_deref(){Some("mp4")=>Ok(skylive_core::RecordingContainer::Mp4),Some("mkv")=>Ok(skylive_core::RecordingContainer::Matroska),_=>Err(RecordingError::invalid("recording path requires the selected .mp4 or .mkv extension"))}
    }
    pub fn validate(&self) -> Result<(), RecordingError> {
        self.encoder.validate().map_err(|e|RecordingError::native("recording encoder",e))?;
        if !(1..=10).contains(&self.keyframe_interval_seconds){return Err(RecordingError::invalid("recording GOP seconds must be1..10"));}
        if matches!(self.encoder,skylive_core::VideoEncoder::NvencH264 {..})&&(!cfg!(all(target_os="windows",target_arch="x86_64"))||self.video_bitrate_kbps>100000){return Err(RecordingError::invalid("NVENC requires Windows x64 and100..100000kbit/s"));}
        if self.path.as_os_str().is_empty()
            || self.path.as_os_str().len() > 32768
            || self.path.to_string_lossy().chars().any(char::is_control)
        {
            return Err(RecordingError::invalid(
                "recording file path is empty or invalid",
            ));
        }
        if self.container()?==skylive_core::RecordingContainer::Matroska&&!cfg!(target_os="windows"){return Err(RecordingError::invalid("selected recording container is unavailable on this platform; Matroska requires Windows"));}
        VideoSettings::new(
            self.video.width(),
            self.video.height(),
            self.video.fps_num(),
            self.video.fps_den(),
        )
        .map_err(|_| RecordingError::invalid("invalid recording video format"))?;
        let (w, h) = (self.video.width(), self.video.height());
        if w % 2 != 0
            || h % 2 != 0
            || w > 4096
            || h > 4096
            || u64::from(w) * u64::from(h) > 8_847_360
        {
            return Err(RecordingError::invalid(
                "recording requires even dimensions up to4096 edges and8.8Mpixels",
            ));
        }
        if !(100..=200000).contains(&self.video_bitrate_kbps) {
            return Err(RecordingError::invalid("recording video bitrate range"));
        }
        if let Some(audio) = self.audio {
            if ![44100, 48000].contains(&audio.sample_rate)
                || ![1, 2].contains(&audio.channels)
                || ![96, 128, 160, 192].contains(&audio.bitrate_kbps)
            {
                return Err(RecordingError::invalid(
                    "AAC recording profile requires44.1/48kHz mono/stereo and96/128/160/192kbit/s",
                ));
            }
        }
        Ok(())
    }
    pub fn frame_time_100ns(&self, index: u64) -> Result<i64, RecordingError> {
        let value = u128::from(index) * 10_000_000 * u128::from(self.video.fps_den())
            / u128::from(self.video.fps_num());
        i64::try_from(value).map_err(|_| RecordingError::invalid("recording timestamp overflow"))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RecordingError {
    pub code: &'static str,
    pub detail: String,
    pub recovery_path: Option<PathBuf>,
}
impl RecordingError {
    pub(super) fn auto_text_retry()->Self{Self{code:"recording_auto_text_retry",detail:"Automatic text is being prepared; retry the next frame".into(),recovery_path:None}}
    pub fn invalid(detail: &'static str) -> Self {
        Self {
            code: "recording_invalid",
            detail: detail.into(),
            recovery_path: None,
        }
    }
    fn io(operation: &str, path: &Path, error: impl fmt::Display) -> Self {
        Self {
            code: "recording_io",
            detail: format!("{operation} {}: {error}", path.display()),
            recovery_path: None,
        }
    }
    fn native(operation: &str, error: impl fmt::Display) -> Self {
        Self {
            code: "recording_native",
            detail: format!("{operation}: {error}"),
            recovery_path: None,
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn unsupported() -> Self {
        Self {
            code: "recording_unavailable",
            detail: "No native MP4 recording adapter is available for this platform".into(),
            recovery_path: None,
        }
    }
}
impl fmt::Display for RecordingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}
impl std::error::Error for RecordingError {}

pub trait RecordingWriter {
    fn video(&mut self, frame: &RgbaFrame, index: u64) -> Result<(), RecordingError>;
    /// The owner already validated/converted this immutable visual sample.
    /// Default fake/custom writers preserve their original raw-frame contract.
    fn video_prepared(&mut self, frame: &PreparedVideo, index: u64) -> Result<(), RecordingError> {
        self.video(frame.image(), index)
    }
    fn audio(&mut self, samples: &[i16], first_frame: u64) -> Result<(), RecordingError>;
    /// PCM uses the SAME post-pause recording sample grid as the legacy input.
    /// Unimplemented writers reject extra tracks rather than cloning Track1.
    fn audio_track(&mut self,track:skylive_core::AudioTrackId,samples:&[i16],first_frame:u64)->Result<(),RecordingError>{if track==skylive_core::AudioTrackId::ONE{self.audio(samples,first_frame)}else{Err(RecordingError::invalid("this recording writer supports Track1 only"))}}
    fn container_packet_limit(&self)->u64{20_000_000}
    fn finalize(&mut self) -> Result<(), RecordingError>;
}
pub type RecordingFactory = Box<
    dyn FnOnce(&RecordingConfig, &Path) -> Result<Box<dyn RecordingWriter>, RecordingError> + Send,
>;
pub fn native_factory() -> RecordingFactory {
    #[cfg(target_os = "windows")]
    {
        Box::new(|config, path| Ok(Box::new(native_windows::Writer::new(config, path)?)))
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(|config, path| Ok(Box::new(native_macos::Writer::new(config, path)?)))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Box::new(|_, _| Err(RecordingError::unsupported()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamp_grid_uses_absolute_rational_positions() {
        let session = Session::new();
        let mut config = RecordingConfig::from_session(&session, "example.mp4".into()).unwrap();
        config.video = VideoSettings::new(1920, 1080, 30000, 1001).unwrap();
        assert_eq!(config.frame_time_100ns(30000).unwrap(), 10_010_000_000);
        assert_eq!(config.frame_time_100ns(1).unwrap(), 333666);
        assert_eq!(config.frame_time_100ns(2).unwrap(), 667333);
        assert!(config.frame_time_100ns(u64::MAX).is_err());
    }
}
