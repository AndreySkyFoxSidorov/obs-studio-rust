//! Authored local-media binding. Playback clocks/decoders/duration are host state.
use crate::Error;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "MediaSourceData")]
pub struct MediaSource {
    path: String,
    width: u32,
    height: u32,
    has_audio: bool,
    looping: bool,
    restart_on_activate: bool,
    close_when_inactive: bool,
    clear_on_end: bool,
    #[serde(default = "normal_speed", skip_serializing_if = "is_normal_speed")]
    speed_percent: u16,
    #[serde(default = "default_reconnect", skip_serializing_if = "is_default_reconnect")]
    reconnect_delay_sec: u8,
    /// OBS retains the network input while local; its format is also retained
    /// while network input is active. The active input itself remains `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    obs_inactive_network: Option<(String, String)>,
    /// Local OBS file path retained while a network URL is active; never opened in that mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    obs_inactive_local: Option<String>,
    /// OBS network seekable flag; retained while a local file is selected.
    #[serde(default, skip_serializing_if = "is_false")]
    obs_seekable: bool,
}
fn normal_speed() -> u16 { 100 }
fn is_false(value:&bool)->bool {!*value}
fn is_normal_speed(value: &u16) -> bool { *value == 100 }
fn default_reconnect() -> u8 { 10 }
fn is_default_reconnect(value: &u8) -> bool { *value == 10 }
fn enabled() -> bool {
    true
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaSourceData {
    path: String,
    width: u32,
    height: u32,
    has_audio: bool,
    #[serde(default)]
    looping: bool,
    #[serde(default = "enabled")]
    restart_on_activate: bool,
    #[serde(default)]
    close_when_inactive: bool,
    #[serde(default = "enabled")]
    clear_on_end: bool,
    #[serde(default = "normal_speed")]
    speed_percent: u16,
    #[serde(default = "default_reconnect")]
    reconnect_delay_sec: u8,
    #[serde(default)]
    obs_inactive_network: Option<(String, String)>,
    #[serde(default)]
    obs_inactive_local: Option<String>,
    #[serde(default)]
    obs_seekable: bool,
}
impl TryFrom<MediaSourceData> for MediaSource {
    type Error = Error;
    fn try_from(value: MediaSourceData) -> Result<Self, Error> {
        Ok(
            Self::new(value.path, value.width, value.height, value.has_audio)?.with_playback(
                value.looping,
                value.restart_on_activate,
                value.close_when_inactive,
                value.clear_on_end,
            ).with_speed(value.speed_percent)?.with_reconnect_delay(value.reconnect_delay_sec)?.with_obs_inactive_network(value.obs_inactive_network)?.with_obs_inactive_local(value.obs_inactive_local)?.with_obs_seekable(value.obs_seekable),
        )
    }
}
impl MediaSource {
    pub fn new(path: String, width: u32, height: u32, has_audio: bool) -> Result<Self, Error> {
        let result = Self {
            path: path.into_boxed_str().into_string(),
            width,
            height,
            has_audio,
            looping: false,
            restart_on_activate: true,
            close_when_inactive: false,
            clear_on_end: true,
            speed_percent: 100,
            reconnect_delay_sec: 10,
            obs_inactive_network: None,
            obs_inactive_local: None,
            obs_seekable: false,
        };
        result.validate()?;
        Ok(result)
    }
    pub fn with_playback(
        mut self,
        looping: bool,
        restart_on_activate: bool,
        close_when_inactive: bool,
        clear_on_end: bool,
    ) -> Self {
        self.looping = looping;
        self.restart_on_activate = restart_on_activate;
        self.close_when_inactive = close_when_inactive;
        self.clear_on_end = clear_on_end;
        self
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn has_audio(&self) -> bool {
        self.has_audio
    }
    pub fn video_dimensions(&self) -> Option<(u32, u32)> {
        if self.width == 0 {
            None
        } else {
            Some((self.width, self.height))
        }
    }
    pub fn looping(&self) -> bool {
        self.looping
    }
    pub fn restart_on_activate(&self) -> bool {
        self.restart_on_activate
    }
    pub fn close_when_inactive(&self) -> bool {
        self.close_when_inactive
    }
    pub fn clear_on_end(&self) -> bool {
        self.clear_on_end
    }
    /// Authored playback rate, in percent of the original media timeline.
    /// The transport must independently validate whether its audio converter
    /// supports the resulting sample rate before accepting a non-default rate.
    pub fn speed_percent(&self) -> u16 { self.speed_percent }
    /// Delay before retrying a failed network stream, in seconds.
    pub fn reconnect_delay_sec(&self) -> u8 { self.reconnect_delay_sec }
    pub fn obs_seekable(&self)->bool {self.obs_seekable}
    pub fn obs_inactive_local(&self)->Option<&str>{self.obs_inactive_local.as_deref()}
    /// Preserve inactive OBS text without accessing or validating it as an active file.
    pub fn with_obs_inactive_local(mut self,value:Option<String>)->Result<Self,Error>{
        if value.as_ref().is_some_and(|path|path.len()>16_384){return Err(Error::Invalid("Retained OBS local media path exceeds 16384 bytes."));}
        self.obs_inactive_local=value.filter(|path|!path.is_empty());Ok(self)
    }
    pub fn with_obs_seekable(mut self,value:bool)->Self {self.obs_seekable=value;self}
    /// Retained OBS network input and demuxer name. The input is inactive;
    /// while network is active, the format is validated by the host importer.
    pub fn obs_inactive_network(&self) -> Option<(&str,&str)> {
        self.obs_inactive_network.as_ref().map(|(input,format)|(input.as_str(),format.as_str()))
    }
    pub fn with_obs_inactive_network(mut self, value:Option<(String,String)>)->Result<Self,Error>{
        if value.as_ref().is_some_and(|(input,format)|input.len()>16_384||format.len()>16_384) {
            return Err(Error::Invalid("Retained OBS network fields exceed 16384 bytes."));
        }
        self.obs_inactive_network=value.filter(|(input,format)|!input.is_empty()||!format.is_empty());
        Ok(self)
    }
    pub fn with_reconnect_delay(mut self, seconds:u8)->Result<Self,Error>{
        if !(1..=60).contains(&seconds) {return Err(Error::Invalid("Media reconnect delay must be 1..=60 seconds."));}
        self.reconnect_delay_sec=seconds;
        Ok(self)
    }
    pub fn with_speed(mut self, speed_percent: u16) -> Result<Self, Error> {
        if !(1..=200).contains(&speed_percent) {
            return Err(Error::Invalid("Media playback speed must be 1..=200 percent."));
        }
        self.speed_percent = speed_percent;
        Ok(self)
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.path.trim().is_empty()
            || self.path.len() > 16_384
            || self.path.chars().any(char::is_control)
        {
            return Err(Error::Invalid(
                "media path requires1..=16384 UTF-8 bytes without controls",
            ));
        }
        if (self.width == 0) != (self.height == 0)
            || (self.width == 0 && !self.has_audio)
            || self.width > 8192
            || self.height > 8192
            || u64::from(self.width) * u64::from(self.height) > 4_194_304
        {
            return Err(Error::Invalid(
                "media source requires supported video dimensions or actual audio",
            ));
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_defaults_match_local_obs_media_flags_without_runtime_state() {
        let media = MediaSource::new("movie.mp4".into(), 1920, 1080, true).unwrap();
        assert!(!media.looping());
        assert!(media.restart_on_activate());
        assert!(!media.close_when_inactive());
        assert!(media.clear_on_end());
        assert_eq!(media.video_dimensions(), Some((1920, 1080)));
        let changed = media.clone().with_playback(true, false, true, false);
        assert_ne!(media, changed);
        assert_eq!(changed.path(), media.path());
    }
    #[test]
    fn audio_only_is_explicit_and_inconsistent_or_unsupported_profiles_reject() {
        assert!(
            MediaSource::new("tone.m4a".into(), 0, 0, true)
                .unwrap()
                .video_dimensions()
                .is_none()
        );
        for (w, h, audio) in [
            (0, 0, false),
            (0, 16, true),
            (16, 0, true),
            (7680, 4320, true),
        ] {
            assert!(MediaSource::new("movie.mp4".into(), w, h, audio).is_err());
        }
        assert!(MediaSource::new("bad\0path".into(), 16, 16, false).is_err());
    }
}
