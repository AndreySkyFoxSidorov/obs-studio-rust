//! Supported setup operations share the Session candidate/publication boundary.
//! No OS detection, benchmark, filesystem, network or frontend dependencies.
use crate::{
    Error, ProfileCommand, ProfileId, RecordingSettings, StreamingSettings, StudioSnapshot,
    VideoSettings,OutputVideoSettings,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupState {
    #[default]
    Unseen,
    Skipped,
    Completed,
}
impl SetupState {
    pub fn is_unseen(&self) -> bool {
        *self == Self::Unseen
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ConfigurationData")]
pub struct SetupConfiguration {
    profile: ProfileId,
    video: VideoSettings,
    recording: RecordingSettings,
    streaming: StreamingSettings,
    #[serde(default,skip_serializing_if="OutputVideoSettings::is_default")]
    output_video:OutputVideoSettings,
    #[serde(default,skip_serializing_if="Option::is_none")]
    default_audio:Option<SetupDefaultAudio>,
}
/// An explicit setup recipe, not a native device snapshot. Existing occupied
/// slots are never replaced. Source identities/filter meaning remain canonical.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupDefaultAudio {pub desktop:bool,pub microphone:bool}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigurationData {
    profile: ProfileId,
    video: VideoSettings,
    recording: RecordingSettings,
    streaming: StreamingSettings,
    #[serde(default)]
    output_video:OutputVideoSettings,
    #[serde(default)]
    default_audio:Option<SetupDefaultAudio>,
}
impl TryFrom<ConfigurationData> for SetupConfiguration {
    type Error = Error;
    fn try_from(value: ConfigurationData) -> Result<Self, Error> {
        Self::new(value.profile, value.video, value.recording, value.streaming)
            .and_then(|configuration|configuration.with_output_video(value.output_video))
            .map(|configuration|configuration.with_default_audio(value.default_audio.unwrap_or_default()))
    }
}
impl SetupConfiguration {
    pub fn new(
        profile: ProfileId,
        video: VideoSettings,
        recording: RecordingSettings,
        streaming: StreamingSettings,
    ) -> Result<Self, Error> {
        let configuration = Self {
            profile,
            video,
            recording,
            streaming,
            output_video:OutputVideoSettings::default(),
            default_audio:None,
        };
        configuration.validate()?;
        Ok(configuration)
    }
    pub fn profile(&self) -> ProfileId {
        self.profile
    }
    pub fn video(&self) -> &VideoSettings {
        &self.video
    }
    pub fn recording(&self) -> &RecordingSettings {
        &self.recording
    }
    pub fn streaming(&self) -> &StreamingSettings {
        &self.streaming
    }
    pub fn with_default_audio(mut self,defaults:SetupDefaultAudio)->Self{
        self.default_audio=(defaults.desktop||defaults.microphone).then_some(defaults);self
    }
    pub fn default_audio(&self)->Option<SetupDefaultAudio>{self.default_audio}
    pub fn output_video(&self)->OutputVideoSettings{self.output_video}
    pub fn with_output_video(mut self,output:OutputVideoSettings)->Result<Self,Error>{
        output.resolve(&self.video)?;self.output_video=output;Ok(self)
    }
    pub fn validate(&self) -> Result<(), Error> {
        ProfileId::new(self.profile.get())?;
        self.video.validate()?;
        self.output_video.resolve(&self.video)?;
        self.recording.validate()?;
        self.streaming.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SetupCommand {
    Apply { configuration: SetupConfiguration },
    Skip,
    CreateProfile { name: String, run_setup: bool },
}
impl SetupCommand {
    pub fn changes_format(&self) -> bool {
        !matches!(self, Self::Skip)
    }
}
pub(crate) fn apply(candidate: &mut StudioSnapshot, command: SetupCommand) -> Result<(), Error> {
    match command {
        SetupCommand::Apply { configuration } => {
            let audio=configuration.default_audio();
            candidate.profiles.configure_setup(configuration)?;
            if let Some(defaults)=audio{apply_default_audio(candidate,defaults)?;}
            candidate.preferences.set_setup_state(SetupState::Completed);
        }
        SetupCommand::Skip => candidate.preferences.set_setup_state(SetupState::Skipped),
        SetupCommand::CreateProfile { name, run_setup } => {
            candidate.profiles.apply(ProfileCommand::New { name })?;
            candidate.preferences.set_setup_on_new_profile(run_setup);
        }
    }
    Ok(())
}

fn apply_default_audio(candidate:&mut StudioSnapshot,defaults:SetupDefaultAudio)->Result<(),Error>{
    use crate::{AudioCaptureBinding,AudioSlot,AudioTarget,SourceKind};
    for (enabled,slot,name) in [(defaults.desktop,AudioSlot::Desktop1,"Desktop Audio"),(defaults.microphone,AudioSlot::Mic1,"Mic/Aux")]{
        if !enabled{continue;}
        if candidate.global_audio().iter().any(|root|root.slot()==slot){return Err(Error::Invalid("Setup default audio slot is occupied. Reopen setup and keep the existing binding."));}
        let binding=AudioCaptureBinding::new(slot.flow(),AudioTarget::Default,slot.flow()==crate::AudioFlow::Output,false)?;
        // Reuse the authored source when it already has this exact default
        // binding, rather than creating a second independently mixed copy.
        let existing=candidate.sources().iter().find_map(|source|matches!(source.kind(),SourceKind::AudioCapture{binding:current}if current==&binding).then_some(source.id()));
        let id=match existing{Some(id)=>id,None=>candidate.add_source_owner(name.into(),SourceKind::AudioCapture{binding})?};
        candidate.set_global_audio(slot,Some(id))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
