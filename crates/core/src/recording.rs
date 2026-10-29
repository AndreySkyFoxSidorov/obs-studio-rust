//! Authored recording choices belong to profiles, never native writer handles.
use crate::Error;
use serde::{Deserialize, Serialize};

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum RecordingContainer {#[default] Mp4,Matroska}
impl RecordingContainer {
    pub fn is_default(&self)->bool{*self==Self::Mp4}
    pub fn extension(self)->&'static str{match self{Self::Mp4=>"mp4",Self::Matroska=>"mkv"}}
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SettingsData")]
pub struct RecordingSettings {
    #[serde(default,skip_serializing_if="crate::RecordingTracks::is_default")]
    audio_tracks:crate::RecordingTracks,
    #[serde(default,skip_serializing_if="RecordingContainer::is_default")]
    container:RecordingContainer,
    #[serde(default="default_keyframe_seconds")]
    keyframe_interval_seconds:u32,
    directory: String,
    #[serde(default)]
    encoder: crate::VideoEncoder,
    video_bitrate_kbps: u32,
    audio_bitrate_kbps: u32,
    audio_enabled: bool,
    prefer_hardware: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsData {
    #[serde(default)]audio_tracks:crate::RecordingTracks,
    #[serde(default)]
    container:RecordingContainer,
    #[serde(default="default_keyframe_seconds")]
    keyframe_interval_seconds:u32,
    directory: String,
    #[serde(default)]
    encoder: crate::VideoEncoder,
    video_bitrate_kbps: u32,
    audio_bitrate_kbps: u32,
    audio_enabled: bool,
    prefer_hardware: bool,
}
fn default_keyframe_seconds()->u32{2}
impl TryFrom<SettingsData> for RecordingSettings {
    type Error = Error;
    fn try_from(value: SettingsData) -> Result<Self, Error> {
        Self::new(
            value.directory,
            value.video_bitrate_kbps,
            value.audio_bitrate_kbps,
            value.audio_enabled,
            value.prefer_hardware,
        )?.with_container(value.container)?.with_encoder(value.encoder)?.with_keyframe_interval(value.keyframe_interval_seconds)?.with_audio_tracks(value.audio_tracks)
    }
}
impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            audio_tracks:Default::default(),
            container:RecordingContainer::Mp4,
            keyframe_interval_seconds:2,
            directory: String::new(),
            encoder: crate::VideoEncoder::Software,
            video_bitrate_kbps: 6000,
            audio_bitrate_kbps: 160,
            audio_enabled: true,
            prefer_hardware: true,
        }
    }
}
impl RecordingSettings {
    pub fn new(
        directory: String,
        video_bitrate_kbps: u32,
        audio_bitrate_kbps: u32,
        audio_enabled: bool,
        prefer_hardware: bool,
    ) -> Result<Self, Error> {
        let value = Self {
            audio_tracks:Default::default(),
            container:RecordingContainer::Mp4,
            keyframe_interval_seconds:2,
            directory: directory.into_boxed_str().into_string(),
            encoder: crate::VideoEncoder::Software,
            video_bitrate_kbps,
            audio_bitrate_kbps,
            audio_enabled,
            prefer_hardware,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn audio_tracks(&self)->&crate::RecordingTracks{&self.audio_tracks}
    pub fn with_audio_tracks(mut self,tracks:crate::RecordingTracks)->Result<Self,Error>{tracks.validate()?;self.audio_tracks=tracks;self.validate()?;Ok(self)}
    pub fn container(&self)->RecordingContainer{self.container}
    pub fn with_container(mut self,container:RecordingContainer)->Result<Self,Error>{self.container=container;self.validate()?;Ok(self)}
    pub fn keyframe_interval_seconds(&self)->u32{self.keyframe_interval_seconds}
    pub fn with_keyframe_interval(mut self,seconds:u32)->Result<Self,Error>{self.keyframe_interval_seconds=seconds;self.validate()?;Ok(self)}
    pub fn directory(&self) -> &str {
        &self.directory
    }
    pub fn encoder(&self)->crate::VideoEncoder{self.encoder}
    pub fn with_encoder(mut self,encoder:crate::VideoEncoder)->Result<Self,Error>{encoder.validate()?;self.encoder=encoder;self.validate()?;Ok(self)}
    pub fn video_bitrate_kbps(&self) -> u32 {
        self.video_bitrate_kbps
    }
    pub fn audio_bitrate_kbps(&self) -> u32 {
        self.audio_bitrate_kbps
    }
    pub fn audio_enabled(&self) -> bool {
        self.audio_enabled
    }
    pub fn prefer_hardware(&self) -> bool {
        self.prefer_hardware
    }
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub fn validate(&self) -> Result<(), Error> {
        self.audio_tracks.validate()?;
        self.encoder.validate()?;
        if !self.encoder.is_default()&&self.video_bitrate_kbps>100000{return Err(Error::Invalid("NVENC recording bitrate must be100..100000kbit/s"));}
        if self.encoder.is_default()&&self.keyframe_interval_seconds!=2{return Err(Error::Invalid("custom recording GOP requires explicit NVENC"));}
        if !(1..=10).contains(&self.keyframe_interval_seconds){return Err(Error::Invalid("recording keyframe interval must be1..10seconds"));}
        if self.directory.len() > 4096 || self.directory.chars().any(char::is_control) {
            return Err(Error::Invalid(
                "recording directory exceeds4096 UTF-8 bytes or contains controls",
            ));
        }
        if !(100..=200_000).contains(&self.video_bitrate_kbps) {
            return Err(Error::Invalid(
                "recording H264 bitrate must be100..=200000 kbit/s",
            ));
        }
        // These are the discrete AAC encoder bitrates supported by the selected
        // native profile. Disabled audio retains its validated next-use choice.
        if ![96, 128, 160, 192].contains(&self.audio_bitrate_kbps) {
            return Err(Error::Invalid(
                "recording AAC bitrate must be96,128,160 or192 kbit/s",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_settings_preserve_paths_and_reject_unsupported_rates_before_publication() {
        assert!(RecordingSettings::default().is_default());
        let path = "C:/Videos/Живий ефір".to_owned();
        let value = RecordingSettings::new(path.clone(), 6000, 160, true, false).unwrap();
        assert_eq!(value.directory(), path);
        for bitrate in [0, 99, 200001, u32::MAX] {
            assert!(RecordingSettings::new(path.clone(), bitrate, 160, true, false).is_err());
        }
        for bitrate in [0, 127, 256] {
            assert!(RecordingSettings::new(path.clone(), 6000, bitrate, true, false).is_err());
        }
        assert!(
            RecordingSettings::new("C:/Videos\nsecret".into(), 6000, 160, false, true).is_err()
        );
    }
    #[test]
    fn recording_profile_choices_survive_scene_undo_and_duplicate_without_history_injection() {
        use crate::{Command, ProfileCommand, Session};
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddColorSource {
                scene,
                name: "Scene history".into(),
                rgba: [12, 34, 56, 255],
            })
            .unwrap();
        let settings =
            RecordingSettings::new("D:/Recordings/Прямий ефір".into(), 12000, 192, false, true)
                .unwrap();
        let id = session.snapshot().profiles().selected_id();
        session
            .apply(Command::Profile {
                command: ProfileCommand::SetRecording {
                    id,
                    settings: settings.clone(),
                },
            })
            .unwrap();
        session.apply(Command::Undo).unwrap();
        assert!(session.snapshot().scene(scene).unwrap().items().is_empty());
        assert_eq!(
            session.snapshot().profiles().selected().recording(),
            &settings
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(session.snapshot().scene(scene).unwrap().items().len(), 1);
        session
            .apply(Command::Profile {
                command: ProfileCommand::Duplicate {
                    name: "Recording copy".into(),
                },
            })
            .unwrap();
        assert_eq!(
            session.snapshot().profiles().selected().recording(),
            &settings
        );
        assert_ne!(session.snapshot().profiles().selected_id(), id);
        assert!(session.can_undo());
    }
}
