//! Canonical custom RTMP/RTMPS profile settings; no DNS, sockets, encoders or UI.
use crate::Error;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SettingsData")]
pub struct StreamingSettings {
    #[serde(default,skip_serializing_if="crate::ReconnectSettings::is_default")]
    reconnect:crate::ReconnectSettings,
    server_url: String,
    stream_key: String,
    #[serde(default)]
    encoder: crate::VideoEncoder,
    video_bitrate_kbps: u32,
    keyframe_interval_seconds: u32,
    #[serde(default="crate::audio_tracks::track_one",skip_serializing_if="crate::audio_tracks::is_track_one")]
    audio_track:crate::AudioTrackId,
    audio_enabled: bool,
    audio_bitrate_kbps: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsData {
    #[serde(default)]reconnect:crate::ReconnectSettings,
    server_url: String,
    stream_key: String,
    #[serde(default)]
    encoder: crate::VideoEncoder,
    video_bitrate_kbps: u32,
    keyframe_interval_seconds: u32,
    #[serde(default="crate::audio_tracks::track_one")]
    audio_track:crate::AudioTrackId,
    audio_enabled: bool,
    audio_bitrate_kbps: u32,
}
impl TryFrom<SettingsData> for StreamingSettings {
    type Error = Error;
    fn try_from(v: SettingsData) -> Result<Self, Error> {
        Self::new(
            v.server_url,
            v.stream_key,
            v.video_bitrate_kbps,
            v.keyframe_interval_seconds,
            v.audio_enabled,
            v.audio_bitrate_kbps,
        )?.with_encoder(v.encoder)?.with_reconnect(v.reconnect).map(|settings|settings.with_audio_track(v.audio_track))
    }
}
impl Default for StreamingSettings {
    fn default() -> Self {
        Self {
            reconnect:Default::default(),
            server_url: String::new(),
            stream_key: String::new(),
            encoder: crate::VideoEncoder::Software,
            video_bitrate_kbps: 6000,
            keyframe_interval_seconds: 2,
            audio_track:crate::AudioTrackId::ONE,
            audio_enabled: true,
            audio_bitrate_kbps: 160,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamingEndpoint<'a> {
    host: &'a str,
    port: u16,
    app: &'a str,
    secure: bool,
}
impl<'a> StreamingEndpoint<'a> {
    /// Parse authored URLs once for settings, DNS and certificate identity.
    pub fn parse(url: &'a str) -> Result<Self, Error> {
        parse_endpoint(url)
    }
    pub fn host(self) -> &'a str {
        self.host
    }
    pub fn port(self) -> u16 {
        self.port
    }
    pub fn app(self) -> &'a str {
        self.app
    }
    pub fn is_secure(self) -> bool {
        self.secure
    }
}
impl StreamingSettings {
    pub fn audio_track(&self)->crate::AudioTrackId{self.audio_track}
    pub fn with_audio_track(mut self,track:crate::AudioTrackId)->Self{self.audio_track=track;self}
    pub fn new(
        server_url: String,
        stream_key: String,
        video_bitrate_kbps: u32,
        keyframe_interval_seconds: u32,
        audio_enabled: bool,
        audio_bitrate_kbps: u32,
    ) -> Result<Self, Error> {
        let mut value = Self {
            reconnect:Default::default(),
            server_url,
            stream_key,
            encoder: crate::VideoEncoder::Software,
            video_bitrate_kbps,
            keyframe_interval_seconds,
            audio_track:crate::AudioTrackId::ONE,
            audio_enabled,
            audio_bitrate_kbps,
        };
        value.validate()?;
        value.server_url = value.server_url.into_boxed_str().into_string();
        value.stream_key = value.stream_key.into_boxed_str().into_string();
        Ok(value)
    }
    pub fn reconnect(&self)->crate::ReconnectSettings{self.reconnect}
    pub fn with_reconnect(mut self,settings:crate::ReconnectSettings)->Result<Self,Error>{settings.validate()?;self.reconnect=settings;self.validate()?;Ok(self)}
    pub fn server_url(&self) -> &str {
        &self.server_url
    }
    pub fn stream_key(&self) -> &str {
        &self.stream_key
    }
    pub fn encoder(&self)->crate::VideoEncoder{self.encoder}
    pub fn with_encoder(mut self,encoder:crate::VideoEncoder)->Result<Self,Error>{encoder.validate()?;self.encoder=encoder;self.validate()?;Ok(self)}
    pub fn video_bitrate_kbps(&self) -> u32 {
        self.video_bitrate_kbps
    }
    pub fn keyframe_interval_seconds(&self) -> u32 {
        self.keyframe_interval_seconds
    }
    pub fn audio_enabled(&self) -> bool {
        self.audio_enabled
    }
    pub fn audio_bitrate_kbps(&self) -> u32 {
        self.audio_bitrate_kbps
    }
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub fn is_configured(&self) -> bool {
        !self.server_url.is_empty() && !self.stream_key.is_empty()
    }
    pub fn endpoint(&self) -> Result<Option<StreamingEndpoint<'_>>, Error> {
        if self.server_url.is_empty() {
            if self.stream_key.is_empty() {
                Ok(None)
            } else {
                Err(Error::Invalid(
                    "Server URL and stream key must both be configured or both be empty.",
                ))
            }
        } else {
            if self.stream_key.is_empty() {
                return Err(Error::Invalid(
                    "Server URL and stream key must both be configured or both be empty.",
                ));
            }
            parse_endpoint(&self.server_url).map(Some)
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        self.reconnect.validate()?;
        self.encoder.validate()?;
        if self.server_url.len() > 4096
            || self.stream_key.len() > 4096
            || self
                .server_url
                .chars()
                .chain(self.stream_key.chars())
                .any(char::is_control)
        {
            return Err(Error::Invalid(
                "Streaming URL/key exceed 4096 UTF-8 bytes or contain controls.",
            ));
        }
        self.endpoint()?;
        if !(64..=100000).contains(&self.video_bitrate_kbps) {
            return Err(Error::Invalid(
                "Streaming video bitrate must be 64 to 100000 kbit/s.",
            ));
        }
        if !(1..=10).contains(&self.keyframe_interval_seconds) {
            return Err(Error::Invalid(
                "Streaming keyframe interval must be 1 to 10 seconds.",
            ));
        }
        if ![96, 128, 160, 192].contains(&self.audio_bitrate_kbps) {
            return Err(Error::Invalid(
                "Streaming AAC bitrate must be 96, 128, 160 or 192 kbit/s.",
            ));
        }
        Ok(())
    }
    /// OBS's integer keyint_sec*fps_num/fps_den. Runtime profile, not authored
    /// duplicated state; unsupported project formats fail before native Start.
    pub fn gop_frames(&self, fps_num: u32, fps_den: u32) -> Result<u32, Error> {
        if fps_den == 0
            || u64::from(fps_num) < u64::from(fps_den)
            || u64::from(fps_num) > 120 * u64::from(fps_den)
        {
            return Err(Error::Invalid(
                "Streaming native frame rate must be 1 to 120 FPS.",
            ));
        }
        let frames =
            u64::from(self.keyframe_interval_seconds) * u64::from(fps_num) / u64::from(fps_den);
        if !(1..=1200).contains(&frames) {
            return Err(Error::Invalid(
                "Streaming native GOP must be 1 to 1200 frames.",
            ));
        }
        Ok(frames as u32)
    }
}
fn parse_endpoint(url: &str) -> Result<StreamingEndpoint<'_>, Error> {
    let (rest, secure) = if let Some(rest) = url.strip_prefix("rtmps://") {
        (rest, true)
    } else {
        (url.strip_prefix("rtmp://")
            .ok_or(Error::Invalid("Use an rtmp:// or rtmps:// server URL."))?, false)
    };
    if url.len() > 4096 || url.chars().any(char::is_control) {
        return Err(Error::Invalid("Streaming server URL exceeds 4096 UTF-8 bytes or contains controls."));
    }
    if url.chars().any(char::is_whitespace) || url.contains(['#', '\\']) {
        return Err(Error::Invalid(
            "Streaming URL cannot contain whitespace, fragments or backslashes.",
        ));
    }
    let (authority, app) = rest.split_once('/').ok_or(Error::Invalid(
        "Streaming server URL needs an application path.",
    ))?;
    if authority.contains('@') {
        return Err(Error::Invalid(
            "URL userinfo is unsupported; use the explicit stream key.",
        ));
    }
    if app.split('?').next().is_none_or(str::is_empty) {
        return Err(Error::Invalid(
            "Streaming server URL needs a nonempty application path.",
        ));
    }
    let bytes = app.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if !bytes
                .get(i + 1..i + 3)
                .is_some_and(|v| v.iter().all(u8::is_ascii_hexdigit))
            {
                return Err(Error::Invalid(
                    "Streaming URL contains an invalid percent escape.",
                ));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (end, tail) = inner
            .split_once(']')
            .ok_or(Error::Invalid("Streaming IPv6 host needs closing bracket."))?;
        end.parse::<std::net::Ipv6Addr>()
            .map_err(|_| Error::Invalid("Streaming IPv6 host is invalid."))?;
        (
            end,
            if tail.is_empty() {
                None
            } else {
                Some(tail.strip_prefix(':').ok_or(Error::Invalid(
                    "Streaming IPv6 authority suffix is invalid.",
                ))?)
            },
        )
    } else {
        let (host, port) = authority
            .split_once(':')
            .map_or((authority, None), |(h, p)| (h, Some(p)));
        if host.is_empty()
            || host.len() > 253
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
        {
            return Err(Error::Invalid(
                "Streaming host must be a bounded ASCII name or bracketed IPv6 address.",
            ));
        }
        (host, port)
    };
    let port = match port {
        None => if secure { 443 } else { 1935 },
        Some(p) => {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::Invalid("Streaming port must be 1 to 65535."));
            }
            let value = p
                .parse::<u16>()
                .map_err(|_| Error::Invalid("Streaming port must be 1 to 65535."))?;
            if value == 0 {
                return Err(Error::Invalid("Streaming port must be 1 to 65535."));
            }
            value
        }
    };
    Ok(StreamingEndpoint { host, port, app, secure })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unconfigured_defaults_exact_credentials_and_literal_gop_are_validated_without_io() {
        let default = StreamingSettings::default();
        assert!(default.is_default());
        assert!(!default.is_configured());
        assert_eq!(default.endpoint().unwrap(), None);
        assert_eq!(default.gop_frames(30000, 1001).unwrap(), 59);
        let url = "rtmp://[::1]:01935/live/path?token=a%2Fb";
        let key = "  exact/Ключ?x=a%2Bb  ";
        let value = StreamingSettings::new(url.into(), key.into(), 6000, 2, true, 160).unwrap();
        assert_eq!((value.server_url(), value.stream_key()), (url, key));
        let e = value.endpoint().unwrap().unwrap();
        assert_eq!(
            (e.host(), e.port(), e.app()),
            ("::1", 1935, "live/path?token=a%2Fb")
        );
        assert_eq!(value.gop_frames(120, 1).unwrap(), 240);
        assert_eq!(value.gop_frames(1, 1).unwrap(), 2);
        assert!(value.gop_frames(240, 1).is_err());
        assert!(value.gop_frames(1, 0).is_err());
    }
    #[test]
    fn unsupported_url_partial_controls_and_encoder_rates_reject() {
        for url in [
            "rtmpsx://127.0.0.1/live",
            "http://127.0.0.1/live",
            "rtmp://127.0.0.1",
            "rtmp://127.0.0.1/",
            "rtmp://127.0.0.1:0/live",
            "rtmp://127.0.0.1:65536/live",
            "rtmp://[::1/live",
            "rtmp://user:pass@127.0.0.1/live",
            "rtmp://127.0.0.1/live#fragment",
            "rtmp://127.0.0.1/li ve",
            "rtmp://127.0.0.1/live%ZZ",
        ] {
            assert!(
                StreamingSettings::new(url.into(), "public-test".into(), 6000, 2, true, 160)
                    .is_err(),
                "{url}"
            );
        }
        assert!(
            StreamingSettings::new(
                "rtmp://127.0.0.1/live".into(),
                String::new(),
                6000,
                2,
                true,
                160
            )
            .is_err()
        );
        assert!(StreamingSettings::new(String::new(), "key".into(), 6000, 2, true, 160).is_err());
        assert!(
            StreamingSettings::new(
                "rtmp://127.0.0.1/live".into(),
                "line\nkey".into(),
                6000,
                2,
                true,
                160
            )
            .is_err()
        );
        assert!(StreamingSettings::new(String::new(), String::new(), 63, 2, false, 160).is_err());
        assert!(StreamingSettings::new(String::new(), String::new(), 6000, 0, false, 160).is_err());
        assert!(StreamingSettings::new(String::new(), String::new(), 6000, 2, false, 256).is_err());
        assert!(
            StreamingSettings::new("x".repeat(4097), "key".into(), 6000, 2, true, 160).is_err()
        );
    }
    #[test]
    fn selected_profile_streaming_is_not_scene_history_and_rejection_and_duplicate_are_atomic() {
        use crate::{Command, ProfileCommand, Session};
        let mut session = Session::new();
        let id = session.snapshot().profiles().selected_id();
        let scene = session.preview();
        session
            .apply(Command::AddScene {
                name: "History".into(),
            })
            .unwrap();
        let settings = StreamingSettings::new(
            "rtmp://127.0.0.1/live".into(),
            "public-test-key".into(),
            8000,
            2,
            true,
            128,
        )
        .unwrap();
        session
            .apply(Command::Profile {
                command: ProfileCommand::SetStreaming {
                    id,
                    settings: settings.clone(),
                },
            })
            .unwrap();
        let stable = session.snapshot();
        session
            .apply(Command::Profile {
                command: ProfileCommand::SetStreaming {
                    id,
                    settings: settings.clone(),
                },
            })
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&stable, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().scenes().len(), 1);
        assert!(session.snapshot().scene(scene).is_some());
        assert_eq!(
            session.snapshot().profiles().selected().streaming(),
            &settings
        );
        let prior = session.snapshot();
        assert!(
            session
                .apply(Command::Profile {
                    command: ProfileCommand::SetStreaming {
                        id: crate::ProfileId::new(999999).unwrap(),
                        settings: settings.clone()
                    }
                })
                .is_err()
        );
        assert!(std::sync::Arc::ptr_eq(&prior, &session.snapshot()));
        session
            .apply(Command::Profile {
                command: ProfileCommand::Duplicate {
                    name: "Stream copy".into(),
                },
            })
            .unwrap();
        assert_eq!(
            session.snapshot().profiles().selected().streaming(),
            &settings
        );
        assert_ne!(session.snapshot().profiles().selected_id(), id);
    }
}
