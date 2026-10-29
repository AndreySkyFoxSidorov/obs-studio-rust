//! Canonical profile formats and user-global preferences, independent of scenes.
//! The Session owns publication/history; this module performs bounded metadata edits.
use crate::{
    AudioBuffering, AudioSettings, Error, RecordingSettings, SpeakerLayout, StreamingSettings,
    VideoSettings,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;

pub const MAX_PROFILES: usize = 32;
const MAX_NAME_UTF16: usize = 170;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ProfileId(u64);
impl ProfileId {
    pub fn new(value: u64) -> Result<Self, Error> {
        if value == 0 {
            return Err(Error::Invalid("profile ID must be nonzero"));
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u64 {
        self.0
    }
}
impl<'de> Deserialize<'de> for ProfileId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ProfileData")]
pub struct Profile {
    #[serde(skip_serializing_if="crate::OutputVideoSettings::is_default")]
    output_video:crate::OutputVideoSettings,
    #[serde(skip_serializing_if="crate::ReplayBufferSettings::is_default")]
    replay_buffer:crate::ReplayBufferSettings,
    id: ProfileId,
    name: String,
    video: VideoSettings,
    sample_rate: u32,
    speakers: SpeakerLayout,
    #[serde(skip_serializing_if = "RecordingSettings::is_default")]
    recording: RecordingSettings,
    #[serde(skip_serializing_if = "StreamingSettings::is_default")]
    streaming: StreamingSettings,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileData {
    #[serde(default)]
    output_video:crate::OutputVideoSettings,
    #[serde(default)]
    replay_buffer:crate::ReplayBufferSettings,
    id: ProfileId,
    name: String,
    video: VideoSettings,
    sample_rate: u32,
    speakers: SpeakerLayout,
    #[serde(default)]
    recording: RecordingSettings,
    #[serde(default)]
    streaming: StreamingSettings,
}
impl TryFrom<ProfileData> for Profile {
    type Error = Error;
    fn try_from(value: ProfileData) -> Result<Self, Error> {
        let mut profile = Self::new(
            value.id,
            value.name,
            value.video,
            value.sample_rate,
            value.speakers,
        )?;
        profile.output_video=value.output_video;
        profile.replay_buffer=value.replay_buffer;
        profile.recording = value.recording;
        profile.streaming = value.streaming;
        profile.validate()?;
        Ok(profile)
    }
}
impl Profile {
    pub fn new(
        id: ProfileId,
        name: String,
        video: VideoSettings,
        sample_rate: u32,
        speakers: SpeakerLayout,
    ) -> Result<Self, Error> {
        let value = Self {
            output_video:Default::default(),
            replay_buffer:Default::default(),
            id,
            name: normalize_name(&name)?,
            video,
            sample_rate,
            speakers,
            recording: RecordingSettings::default(),
            streaming: StreamingSettings::default(),
        };
        value.validate()?;
        Ok(value)
    }
    pub fn id(&self) -> ProfileId {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn output_video_settings(&self)->crate::OutputVideoSettings{self.output_video}
    pub fn output_video(&self)->Result<VideoSettings,Error>{self.output_video.resolve(&self.video)}
    pub fn video(&self) -> &VideoSettings {
        &self.video
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn speakers(&self) -> SpeakerLayout {
        self.speakers
    }
    pub fn streaming(&self) -> &StreamingSettings {
        &self.streaming
    }
    pub fn replay_buffer(&self)->crate::ReplayBufferSettings{self.replay_buffer}
    pub fn recording(&self) -> &RecordingSettings {
        &self.recording
    }
    fn validate(&self) -> Result<(), Error> {
        ProfileId::new(self.id.get())?;
        validate_name(&self.name)?;
        self.video.validate()?;
        self.output_video.resolve(&self.video)?;
        AudioSettings::new(self.sample_rate, self.speakers)?;
        self.replay_buffer.validate()?;
        self.recording.validate()?;
        self.streaming.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProfileCommand {
    SetReconnect {id:ProfileId,settings:crate::ReconnectSettings},
    SetReplayBuffer {id:ProfileId,settings:crate::ReplayBufferSettings},
    SetRecordingAndReplay {id:ProfileId,recording:RecordingSettings,replay:crate::ReplayBufferSettings},
    Import {
        profile: Profile,
    },
    New {
        name: String,
    },
    Duplicate {
        name: String,
    },
    Rename {
        id: ProfileId,
        name: String,
    },
    Remove {
        id: ProfileId,
    },
    Select {
        id: ProfileId,
    },
    SetStreaming {
        id: ProfileId,
        settings: StreamingSettings,
    },
    SetRecording {
        id: ProfileId,
        settings: RecordingSettings,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "CatalogData")]
pub struct ProfileCatalog {
    profiles: Vec<Profile>,
    selected: ProfileId,
    next_id: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogData {
    profiles: Vec<Profile>,
    selected: ProfileId,
    next_id: u64,
}
impl TryFrom<CatalogData> for ProfileCatalog {
    type Error = Error;
    fn try_from(value: CatalogData) -> Result<Self, Error> {
        let catalog = Self {
            profiles: value.profiles,
            selected: value.selected,
            next_id: value.next_id,
        };
        catalog.validate()?;
        Ok(Self {
            profiles: catalog.profiles.into_boxed_slice().into_vec(),
            ..catalog
        })
    }
}
impl Default for ProfileCatalog {
    fn default() -> Self {
        Self::from_legacy(VideoSettings::default(), AudioSettings::default())
            .expect("default profile settings valid")
    }
}
impl ProfileCatalog {
    /// Legacy buffering belongs to UserPreferences, so only its format is copied.
    pub fn from_legacy(video: VideoSettings, audio: AudioSettings) -> Result<Self, Error> {
        audio.validate()?;
        let id = ProfileId::new(1)?;
        Ok(Self {
            profiles: vec![Profile::new(
                id,
                "Untitled".into(),
                video,
                audio.sample_rate(),
                audio.speakers(),
            )?],
            selected: id,
            next_id: 2,
        })
    }
    pub fn selected(&self) -> &Profile {
        self.profiles
            .iter()
            .find(|profile| profile.id == self.selected)
            .expect("validated catalog contains selected profile")
    }
    pub fn entries(&self) -> &[Profile] {
        &self.profiles
    }
    pub fn selected_id(&self) -> ProfileId {
        self.selected
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.profiles.is_empty() || self.profiles.len() > MAX_PROFILES {
            return Err(Error::Limit("profile catalog must contain 1..=32 profiles"));
        }
        if self.next_id == 0 {
            return Err(Error::Invalid("profile next ID must be nonzero"));
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for profile in &self.profiles {
            profile.validate()?;
            if profile.id.get() >= self.next_id || !ids.insert(profile.id) {
                return Err(Error::Invalid("duplicate or unissued profile ID"));
            }
            if !names.insert(profile.name.as_str()) {
                return Err(Error::Invalid("duplicate profile name"));
            }
        }
        if !ids.contains(&self.selected) {
            return Err(Error::Invalid("selected profile does not exist"));
        }
        Ok(())
    }
    /// Stage the entire bounded catalog before publishing, including ID issuance.
    pub fn apply(&mut self, command: ProfileCommand) -> Result<(), Error> {
        self.validate()?;
        let mut candidate = self.clone();
        match command {
            ProfileCommand::Import { profile } => {
                profile.validate()?;
                let mut name = profile.name.clone();
                if candidate.profiles.iter().any(|existing| existing.name == name) {
                    let mut unique = None;
                    for index in 2..=MAX_PROFILES + 1 {
                        let suffix = format!(" {index}");
                        let mut base = profile.name.clone();
                        while base.encode_utf16().count() + suffix.encode_utf16().count() > MAX_NAME_UTF16 { base.pop(); }
                        let proposed = format!("{base}{suffix}");
                        if candidate.profiles.iter().all(|existing| existing.name != proposed) { unique = Some(proposed); break; }
                    }
                    name = unique.ok_or(Error::Limit("Cannot allocate an imported profile name"))?;
                }
                candidate.insert_new(name, profile.video, profile.sample_rate, profile.speakers)?;
                let selected = candidate.selected;
                candidate.find_mut(selected)?.output_video=profile.output_video;
                candidate.find_mut(selected)?.replay_buffer=profile.replay_buffer;
                candidate.find_mut(selected)?.recording = profile.recording;
                candidate.find_mut(selected)?.streaming = profile.streaming;
            }
            ProfileCommand::New { name } => {
                let audio = AudioSettings::default();
                candidate.insert_new(
                    name,
                    VideoSettings::default(),
                    audio.sample_rate(),
                    audio.speakers(),
                )?;
            }
            ProfileCommand::Duplicate { name } => {
                let selected = candidate.selected().clone();
                candidate.insert_new(
                    name,
                    selected.video,
                    selected.sample_rate,
                    selected.speakers,
                )?;
                candidate.find_mut(candidate.selected)?.output_video=selected.output_video;
                candidate.find_mut(candidate.selected)?.replay_buffer=selected.replay_buffer;
                candidate.find_mut(candidate.selected)?.recording = selected.recording;
                candidate.find_mut(candidate.selected)?.streaming = selected.streaming;
            }
            ProfileCommand::Rename { id, name } => {
                let name = normalize_name(&name)?;
                candidate.find_mut(id)?.name = name;
            }
            ProfileCommand::Remove { id } => {
                let position = candidate
                    .profiles
                    .iter()
                    .position(|profile| profile.id == id)
                    .ok_or(Error::Invalid("profile does not exist"))?;
                if candidate.profiles.len() == 1 {
                    return Err(Error::Invalid("cannot remove the last profile"));
                }
                candidate.profiles.remove(position);
                if candidate.selected == id {
                    candidate.selected = candidate
                        .profiles
                        .iter()
                        .min_by(|a, b| a.name.cmp(&b.name))
                        .expect("remaining profile exists")
                        .id;
                }
            }
            ProfileCommand::Select { id } => {
                candidate.find_mut(id)?;
                candidate.selected = id;
            }
            ProfileCommand::SetReconnect {id,settings}=>{settings.validate()?;let profile=candidate.find_mut(id)?;profile.streaming=profile.streaming.clone().with_reconnect(settings)?;},
            ProfileCommand::SetStreaming { id, settings } => {
                settings.validate()?;
                candidate.find_mut(id)?.streaming = settings;
            }
            ProfileCommand::SetReplayBuffer {id,settings}=>{settings.validate()?;candidate.find_mut(id)?.replay_buffer=settings;},
            ProfileCommand::SetRecordingAndReplay {id,recording,replay}=>{recording.validate()?;replay.validate()?;let target=candidate.find_mut(id)?;target.recording=recording;target.replay_buffer=replay;},
            ProfileCommand::SetRecording { id, settings } => {
                settings.validate()?;
                candidate.find_mut(id)?.recording = settings;
            }
        }
        candidate.validate()?;
        candidate.profiles = candidate.profiles.into_boxed_slice().into_vec();
        *self = candidate;
        Ok(())
    }
    pub(crate) fn configure_setup(
        &mut self,
        configuration: crate::SetupConfiguration,
    ) -> Result<(), Error> {
        configuration.validate()?;
        if self.selected != configuration.profile() {
            return Err(Error::Invalid("setup profile is no longer selected"));
        }
        let profile = self.find_mut(configuration.profile())?;
        profile.video = configuration.video().clone();
        // The reviewed canvas and actual encoder output are separate values.
        profile.output_video=configuration.output_video();
        profile.recording = configuration.recording().clone();
        profile.streaming = configuration.streaming().clone();
        Ok(())
    }
    pub(crate) fn configure_video(&mut self,canvas:VideoSettings,output:crate::OutputVideoSettings)->Result<(),Error>{
        canvas.validate()?;output.resolve(&canvas)?;
        let profile=self.find_mut(self.selected)?;profile.video=canvas;profile.output_video=output;Ok(())
    }
    pub(crate) fn set_video(&mut self, video: VideoSettings) -> Result<(), Error> {
        video.validate()?;
        let selected = self.selected;
        self.find_mut(selected)?.video = video;
        Ok(())
    }
    pub(crate) fn set_audio(&mut self, audio: AudioSettings) -> Result<(), Error> {
        audio.validate()?;
        let selected = self.selected;
        let profile = self.find_mut(selected)?;
        profile.sample_rate = audio.sample_rate();
        profile.speakers = audio.speakers();
        Ok(())
    }
    fn find_mut(&mut self, id: ProfileId) -> Result<&mut Profile, Error> {
        self.profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .ok_or(Error::Invalid("profile does not exist"))
    }
    fn insert_new(
        &mut self,
        name: String,
        video: VideoSettings,
        sample_rate: u32,
        speakers: SpeakerLayout,
    ) -> Result<(), Error> {
        if self.profiles.len() >= MAX_PROFILES {
            return Err(Error::Limit("profile catalog limit exceeded"));
        }
        let next = self
            .next_id
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        let id = ProfileId::new(self.next_id)?;
        let profile = Profile::new(id, name, video, sample_rate, speakers)?;
        self.profiles.push(profile);
        self.selected = id;
        self.next_id = next;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PreferencesData")]
pub struct UserPreferences {
    #[serde(default,skip_serializing_if="crate::PreviewSnapping::is_default")]
    preview_snapping:crate::PreviewSnapping,
    #[serde(default,skip_serializing_if="crate::ProjectorPreferences::is_default")]
    projectors:crate::ProjectorPreferences,
    #[serde(default,skip_serializing_if="crate::ProcessPriority::is_default")]
    process_priority:crate::ProcessPriority,
    #[serde(default,skip_serializing_if="crate::NdiOutputSettings::is_default")]
    ndi_output:crate::NdiOutputSettings,
    #[serde(default,skip_serializing_if="crate::AudioMonitoringDevice::is_default")]
    audio_monitoring_device:crate::AudioMonitoringDevice,
    #[serde(
        default,
        skip_serializing_if = "crate::MixerDisplayPreferences::is_default"
    )]
    mixer_display: crate::MixerDisplayPreferences,
    #[serde(default, skip_serializing_if = "crate::SetupState::is_unseen")]
    setup_state: crate::SetupState,
    #[serde(
        default = "default_setup_on_new_profile",
        skip_serializing_if = "is_true"
    )]
    setup_on_new_profile: bool,
    audio_buffering: AudioBuffering,
    #[serde(default, skip_serializing_if = "crate::Appearance::is_default")]
    appearance: crate::Appearance,
    #[serde(default,skip_serializing_if="crate::Accessibility::is_default")]
    accessibility:crate::Accessibility,
    #[serde(default, skip_serializing_if = "crate::HotkeyBindings::is_default")]
    hotkeys: crate::HotkeyBindings,
    #[serde(default,skip_serializing_if="crate::GlobalHotkeyBindings::is_empty")]
    global_hotkeys:crate::GlobalHotkeyBindings,
    #[serde(default)]
    enabled_plugins: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    disabled_bundled_plugins: Vec<String>,
    #[serde(default = "default_language")]
    language: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferencesData {
    #[serde(default)]
    preview_snapping:crate::PreviewSnapping,
    #[serde(default)]
    projectors:crate::ProjectorPreferences,
    #[serde(default)]
    process_priority:crate::ProcessPriority,
    #[serde(default)]
    ndi_output:crate::NdiOutputSettings,
    #[serde(default)]
    accessibility:crate::Accessibility,
    #[serde(default)]
    audio_monitoring_device:crate::AudioMonitoringDevice,
    #[serde(default)]
    mixer_display: crate::MixerDisplayPreferences,
    #[serde(default)]
    setup_state: crate::SetupState,
    #[serde(default = "default_setup_on_new_profile")]
    setup_on_new_profile: bool,
    audio_buffering: AudioBuffering,
    #[serde(default)]
    appearance: crate::Appearance,
    #[serde(default)]
    hotkeys: crate::HotkeyBindings,
    #[serde(default)]
    global_hotkeys:crate::GlobalHotkeyBindings,
    #[serde(default)]
    enabled_plugins: Vec<String>,
    #[serde(default)]
    disabled_bundled_plugins: Vec<String>,
    #[serde(default = "default_language")]
    language: String,
}
impl TryFrom<PreferencesData> for UserPreferences {
    type Error = Error;
    fn try_from(value: PreferencesData) -> Result<Self, Error> {
        let mut preferences = Self::new(value.audio_buffering)?;
        preferences.set_preview_snapping(value.preview_snapping)?;
        preferences.set_projectors(value.projectors);
        preferences.set_process_priority(value.process_priority);
        preferences.set_ndi_output(value.ndi_output)?;
        preferences.set_audio_monitoring_device(value.audio_monitoring_device)?;
        preferences.mixer_display = value.mixer_display;
        preferences.setup_state = value.setup_state;
        preferences.setup_on_new_profile = value.setup_on_new_profile;
        preferences.configure_hotkeys(value.hotkeys,value.global_hotkeys)?;
        preferences.set_appearance(value.appearance)?;
        preferences.set_accessibility(value.accessibility)?;
        preferences.set_language(value.language)?;
        crate::plugins::validate_enabled_plugins(&value.enabled_plugins)?;
        crate::plugins::validate_disabled_bundled_plugins(&value.disabled_bundled_plugins)?;
        preferences.enabled_plugins = value
            .enabled_plugins
            .into_iter()
            .map(|id| id.into_boxed_str().into_string())
            .collect::<Vec<_>>()
            .into_boxed_slice()
            .into_vec();
        preferences.enabled_plugins.sort();
        preferences.disabled_bundled_plugins = value
            .disabled_bundled_plugins
            .into_iter()
            .map(|id| id.into_boxed_str().into_string())
            .collect::<Vec<_>>()
            .into_boxed_slice()
            .into_vec();
        preferences.disabled_bundled_plugins.sort();
        Ok(preferences)
    }
}
fn default_setup_on_new_profile() -> bool {
    true
}
fn is_true(value: &bool) -> bool {
    *value
}
fn default_language() -> String {
    "en-US".into()
}
impl Default for UserPreferences {
    fn default() -> Self {
        Self::new(AudioBuffering::default()).expect("default preferences valid")
    }
}
fn validate_language(value: &str) -> Result<(), Error> {
    let mut segments = value.split('-');
    let first = segments.next().unwrap_or("");
    if value.len() > 35
        || !(2..=8).contains(&first.len())
        || !first.bytes().all(|ch| ch.is_ascii_alphabetic())
        || !segments.all(|part| {
            !part.is_empty() && part.len() <= 8 && part.bytes().all(|ch| ch.is_ascii_alphanumeric())
        })
    {
        return Err(Error::Invalid(
            "language must be a valid 2..35 byte locale code",
        ));
    }
    Ok(())
}

impl UserPreferences {
    pub fn preview_snapping(&self)->crate::PreviewSnapping{self.preview_snapping}
    pub(crate) fn set_preview_snapping(&mut self,value:crate::PreviewSnapping)->Result<(),Error>{value.validate()?;self.preview_snapping=value;Ok(())}
    pub fn projectors(&self)->crate::ProjectorPreferences{self.projectors}
    pub(crate) fn set_projectors(&mut self,value:crate::ProjectorPreferences){self.projectors=value;}

    pub fn process_priority(&self)->crate::ProcessPriority{self.process_priority}
    pub(crate) fn set_process_priority(&mut self,value:crate::ProcessPriority){self.process_priority=value;}

    pub fn ndi_output(&self)->&crate::NdiOutputSettings{&self.ndi_output}
    pub(crate) fn set_ndi_output(&mut self,value:crate::NdiOutputSettings)->Result<(),Error>{value.validate()?;self.ndi_output=value;Ok(())}
    pub fn audio_monitoring_device(&self)->&crate::AudioMonitoringDevice {&self.audio_monitoring_device}
    pub(crate) fn set_audio_monitoring_device(&mut self,device:crate::AudioMonitoringDevice)->Result<(),Error>{device.validate()?;self.audio_monitoring_device=device;Ok(())}
    pub fn mixer_display(&self) -> crate::MixerDisplayPreferences {
        self.mixer_display
    }
    pub(crate) fn set_mixer_display(&mut self, value: crate::MixerDisplayPreferences) {
        self.mixer_display = value;
    }
    pub fn setup_state(&self) -> crate::SetupState {
        self.setup_state
    }
    pub fn setup_on_new_profile(&self) -> bool {
        self.setup_on_new_profile
    }
    pub fn has_setup_metadata(&self) -> bool {
        !self.setup_state.is_unseen() || !self.setup_on_new_profile
    }
    pub(crate) fn set_setup_state(&mut self, state: crate::SetupState) {
        self.setup_state = state;
    }
    pub(crate) fn set_setup_on_new_profile(&mut self, enabled: bool) {
        self.setup_on_new_profile = enabled;
    }
    pub fn appearance(&self) -> crate::Appearance {
        self.appearance
    }
    pub fn accessibility(&self)->crate::Accessibility {self.accessibility}
    pub(crate) fn set_accessibility(&mut self,value:crate::Accessibility)->Result<(),Error>{value.validate()?;self.accessibility=value;Ok(())}
    pub(crate) fn set_appearance(&mut self, appearance: crate::Appearance) -> Result<(), Error> {
        appearance.validate()?;
        self.appearance = appearance;
        Ok(())
    }
    pub fn hotkeys(&self) -> &crate::HotkeyBindings {
        &self.hotkeys
    }
    pub fn global_hotkeys(&self)->&crate::GlobalHotkeyBindings{&self.global_hotkeys}
    pub(crate) fn set_hotkeys(&mut self,hotkeys:crate::HotkeyBindings)->Result<(),Error>{
        crate::validate_hotkey_scopes(&hotkeys,&self.global_hotkeys)?;self.hotkeys=hotkeys;Ok(())
    }
    pub(crate) fn configure_hotkeys(&mut self,local:crate::HotkeyBindings,global:crate::GlobalHotkeyBindings)->Result<(),Error>{
        crate::validate_hotkey_scopes(&local,&global)?;self.hotkeys=local;self.global_hotkeys=global;Ok(())
    }
    pub fn language(&self) -> &str {
        &self.language
    }
    pub(crate) fn set_language(&mut self, language: String) -> Result<(), Error> {
        validate_language(&language)?;
        self.language = language.into_boxed_str().into_string();
        Ok(())
    }
    pub fn new(audio_buffering: AudioBuffering) -> Result<Self, Error> {
        audio_buffering.validate()?;
        Ok(Self {
            preview_snapping:crate::PreviewSnapping::default(),
            projectors:crate::ProjectorPreferences::default(),
            process_priority:crate::ProcessPriority::Normal,
            ndi_output:crate::NdiOutputSettings::default(),
            audio_monitoring_device:crate::AudioMonitoringDevice::default(),
            mixer_display: crate::MixerDisplayPreferences::default(),
            setup_state: crate::SetupState::Unseen,
            setup_on_new_profile: true,
            audio_buffering,
            appearance: crate::Appearance::default(),
            accessibility:crate::Accessibility::default(),
            hotkeys: crate::HotkeyBindings::default(),
            global_hotkeys:crate::GlobalHotkeyBindings::default(),
            enabled_plugins: Vec::new(),
            disabled_bundled_plugins: Vec::new(),
            language: default_language(),
        })
    }
    pub fn audio_buffering(&self) -> AudioBuffering {
        self.audio_buffering
    }
    pub fn enabled_plugins(&self) -> &[String] {
        &self.enabled_plugins
    }
    pub fn plugin_enabled(&self, id: &str) -> bool {
        crate::plugin_descriptor(id).is_some_and(|provider| {
            crate::plugins::provider_enabled(
                provider,
                &self.enabled_plugins,
                &self.disabled_bundled_plugins,
            )
        })
    }
    pub fn disabled_bundled_plugins(&self) -> &[String] {
        &self.disabled_bundled_plugins
    }
    pub(crate) fn set_plugin_enabled(&mut self, id: String, enabled: bool) -> Result<(), Error> {
        let provider = crate::plugin_descriptor(&id).ok_or(Error::Invalid("unknown plugin ID"))?;
        if !provider.implemented || provider.origin == crate::PluginOrigin::BuiltIn {
            return Err(Error::Unavailable(
                "plugin has no activatable Rust provider",
            ));
        }
        let (entries, should_contain) = if provider.origin == crate::PluginOrigin::Bundled {
            (&mut self.disabled_bundled_plugins, !enabled)
        } else {
            (&mut self.enabled_plugins, enabled)
        };
        if should_contain && !entries.contains(&id) {
            entries.push(id.into_boxed_str().into_string());
            entries.sort();
        } else if !should_contain {
            entries.retain(|value| value != &id);
        }
        *entries = std::mem::take(entries).into_boxed_slice().into_vec();
        self.validate()
    }
    pub(crate) fn set_audio_buffering(&mut self, value: AudioBuffering) -> Result<(), Error> {
        value.validate()?;
        self.audio_buffering = value;
        Ok(())
    }
    pub fn validate(&self) -> Result<(), Error> {
        crate::validate_hotkey_scopes(&self.hotkeys,&self.global_hotkeys)?;
        self.preview_snapping.validate()?;
        self.audio_monitoring_device.validate()?;
        self.audio_buffering.validate()?;
        self.appearance.validate()?;
        self.accessibility.validate()?;
        validate_language(&self.language)?;
        crate::plugins::validate_enabled_plugins(&self.enabled_plugins)?;
        crate::plugins::validate_disabled_bundled_plugins(&self.disabled_bundled_plugins)
    }
}

fn normalize_name(name: &str) -> Result<String, Error> {
    let name = name.trim_matches([' ', '\t']);
    validate_name(name)?;
    Ok(name.to_owned().into_boxed_str().into_string())
}
fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || name.encode_utf16().take(MAX_NAME_UTF16 + 1).count() > MAX_NAME_UTF16 {
        return Err(Error::Invalid(
            "profile name must contain 1..=170 UTF-16 units",
        ));
    }
    if name.trim_matches([' ', '\t']) != name {
        return Err(Error::Invalid(
            "profile name must have normalized ASCII edges",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::{
        IntoDeserializer, Visitor,
        value::{Error as DeError, MapDeserializer, SeqDeserializer, U64Deserializer},
    };

    #[test]
    fn whole_preferences_command_rejects_invalid_typed_language_and_activation_without_publication()
    {
        use crate::{AUDIO_PAN_PLUGIN, Command, Session};
        let mut session = Session::new();
        let before = session.snapshot();
        for preferences in [
            UserPreferences {
                language: "invalid_locale".into(),
                ..UserPreferences::default()
            },
            UserPreferences {
                enabled_plugins: vec!["unknown/plugin".into()],
                ..UserPreferences::default()
            },
            UserPreferences {
                enabled_plugins: vec!["norihiro/obs-mute-filter".into()],
                ..UserPreferences::default()
            },
            UserPreferences {
                enabled_plugins: vec![AUDIO_PAN_PLUGIN.into(), AUDIO_PAN_PLUGIN.into()],
                ..UserPreferences::default()
            },
            UserPreferences {
                disabled_bundled_plugins: vec!["unknown/bundled".into()],
                ..UserPreferences::default()
            },
            UserPreferences {
                disabled_bundled_plugins: vec![AUDIO_PAN_PLUGIN.into()],
                ..UserPreferences::default()
            },
            UserPreferences {
                disabled_bundled_plugins: vec![crate::CRAFT_AUDIO_PLUGIN.into(); 2],
                ..UserPreferences::default()
            },
        ] {
            assert!(
                session
                    .apply(Command::SetUserPreferences { preferences })
                    .is_err()
            );
            assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
            assert_eq!(session.document_revision(), 0);
            assert!(!session.is_document_dirty());
            assert!(!session.is_dirty());
            assert!(!session.can_undo());
        }
    }

    #[test]
    fn create_duplicate_select_and_remove_preserve_formats_and_issue_fresh_ids() {
        let mut catalog = ProfileCatalog::default();
        let original = catalog.selected_id();
        let video = VideoSettings::new(1280, 720, 60000, 1001).unwrap();
        let audio = AudioSettings::new(44100, SpeakerLayout::FivePointOne)
            .unwrap()
            .with_buffering(AudioBuffering::low_latency())
            .unwrap();
        catalog.set_video(video.clone()).unwrap();
        catalog.set_audio(audio).unwrap();
        let preferences = UserPreferences::new(audio.buffering()).unwrap();

        catalog
            .apply(ProfileCommand::Duplicate {
                name: " \tCopy\t ".into(),
            })
            .unwrap();
        let copy = catalog.selected_id();
        assert_eq!(copy.get(), 2);
        assert_eq!(catalog.selected().name(), "Copy");
        assert_eq!(catalog.selected().video(), &video);
        assert_eq!(catalog.selected().sample_rate(), 44100);
        assert_eq!(catalog.selected().speakers(), SpeakerLayout::FivePointOne);
        catalog
            .apply(ProfileCommand::New { name: "New".into() })
            .unwrap();
        assert_eq!(catalog.selected_id().get(), 3);
        assert_eq!(catalog.selected().video(), &VideoSettings::default());
        assert_eq!(catalog.selected().sample_rate(), 48000);
        assert_eq!(catalog.selected().speakers(), SpeakerLayout::Stereo);
        assert_eq!(preferences.audio_buffering(), AudioBuffering::low_latency());

        catalog
            .apply(ProfileCommand::Select { id: original })
            .unwrap();
        assert_eq!(catalog.selected().video(), &video);
        catalog.apply(ProfileCommand::Remove { id: copy }).unwrap();
        assert_eq!(catalog.selected_id(), original);
        catalog
            .apply(ProfileCommand::Duplicate {
                name: "Fresh".into(),
            })
            .unwrap();
        assert_eq!(
            catalog.selected_id().get(),
            4,
            "deleted identities are never reissued"
        );
        catalog
            .apply(ProfileCommand::Rename {
                id: original,
                name: " \tAlpha\t ".into(),
            })
            .unwrap();
        let selected = catalog.selected_id();
        catalog
            .apply(ProfileCommand::Remove { id: selected })
            .unwrap();
        assert_eq!(
            catalog.selected_id(),
            original,
            "active removal selects lexical first name"
        );
        assert_eq!(catalog.selected().name(), "Alpha");
    }

    #[test]
    fn noops_rejections_capacity_and_id_exhaustion_preserve_the_catalog() {
        let mut catalog = ProfileCatalog::default();
        let original = catalog.clone();
        let id = catalog.selected_id();
        catalog.apply(ProfileCommand::Select { id }).unwrap();
        catalog
            .apply(ProfileCommand::Rename {
                id,
                name: " \tUntitled \t".into(),
            })
            .unwrap();
        assert_eq!(catalog, original);
        for command in [
            ProfileCommand::New {
                name: " \t ".into(),
            },
            ProfileCommand::Duplicate {
                name: "Untitled".into(),
            },
            ProfileCommand::Select {
                id: ProfileId::new(99).unwrap(),
            },
            ProfileCommand::Rename {
                id: ProfileId::new(99).unwrap(),
                name: "Unknown".into(),
            },
            ProfileCommand::Remove { id },
        ] {
            assert!(catalog.apply(command).is_err());
            assert_eq!(catalog, original);
        }
        for index in 1..MAX_PROFILES {
            catalog
                .apply(ProfileCommand::New {
                    name: format!("Profile {index}"),
                })
                .unwrap();
        }
        let full = catalog.clone();
        for command in [
            ProfileCommand::New {
                name: "Overflow".into(),
            },
            ProfileCommand::Duplicate {
                name: "Overflow".into(),
            },
        ] {
            assert!(matches!(catalog.apply(command), Err(Error::Limit(_))));
            assert_eq!(catalog, full);
        }
        let mut exhausted = ProfileCatalog {
            next_id: u64::MAX,
            ..ProfileCatalog::default()
        };
        let before = exhausted.clone();
        assert_eq!(
            exhausted.apply(ProfileCommand::New {
                name: "Never issued".into()
            }),
            Err(Error::IdentityExhausted)
        );
        assert_eq!(exhausted, before);
    }

    #[test]
    fn names_use_ascii_edges_utf16_units_and_case_sensitive_uniqueness() {
        let mut catalog = ProfileCatalog::default();
        for name in ["untitled", "\u{a0}", "\u{a0} padded \u{a0}"] {
            catalog
                .apply(ProfileCommand::New { name: name.into() })
                .unwrap();
            assert_eq!(catalog.selected().name(), name);
        }
        let valid = "😀".repeat(85);
        catalog
            .apply(ProfileCommand::New {
                name: format!(" \t{valid}\t "),
            })
            .unwrap();
        assert_eq!(catalog.selected().name(), valid);
        let before = catalog.clone();
        for name in ["😀".repeat(86), "x".repeat(171), " \tUntitled\t ".into()] {
            assert!(catalog.apply(ProfileCommand::New { name }).is_err());
            assert_eq!(catalog, before);
        }
        let source_id = catalog.entries()[1].id();
        assert!(
            catalog
                .apply(ProfileCommand::Rename {
                    id: source_id,
                    name: "Untitled".into()
                })
                .is_err()
        );
        assert_eq!(catalog, before);
    }

    #[test]
    fn profile_audio_formats_do_not_take_ownership_of_global_buffering() {
        let buffering = AudioBuffering::new(209, false).unwrap();
        let preferences = UserPreferences::new(buffering).unwrap();
        for rate in [44100, 48000] {
            for speakers in SpeakerLayout::ALL {
                let audio = AudioSettings::new(rate, speakers)
                    .unwrap()
                    .with_buffering(buffering)
                    .unwrap();
                let mut catalog =
                    ProfileCatalog::from_legacy(VideoSettings::default(), audio).unwrap();
                assert_eq!(catalog.selected().sample_rate(), rate);
                assert_eq!(catalog.selected().speakers(), speakers);
                catalog
                    .apply(ProfileCommand::New {
                        name: "Other".into(),
                    })
                    .unwrap();
                catalog
                    .set_audio(audio.with_buffering(AudioBuffering::low_latency()).unwrap())
                    .unwrap();
                assert_eq!(catalog.selected().sample_rate(), rate);
                assert_eq!(catalog.selected().speakers(), speakers);
                assert_eq!(preferences.audio_buffering(), buffering);
            }
        }
        assert!(
            Profile::new(
                ProfileId::new(1).unwrap(),
                "Bad rate".into(),
                VideoSettings::default(),
                96000,
                SpeakerLayout::Mono
            )
            .is_err()
        );
        let mut catalog = ProfileCatalog::default();
        let before = catalog.clone();
        let bad_video = VideoSettings {
            width: 0,
            ..VideoSettings::default()
        };
        assert!(catalog.set_video(bad_video).is_err());
        assert_eq!(catalog, before);
    }

    #[test]
    fn normalized_owned_names_and_decoded_profile_vectors_do_not_retain_spare_capacity() {
        let mut name = String::with_capacity(8192);
        name.push_str("\tSmall ");
        let profile = Profile::new(
            ProfileId::new(1).unwrap(),
            name,
            VideoSettings::default(),
            48000,
            SpeakerLayout::Stereo,
        )
        .unwrap();
        assert_eq!(profile.name, "Small");
        assert_eq!(profile.name.capacity(), profile.name.len());
        let mut profiles = Vec::with_capacity(512);
        profiles.push(profile);
        let catalog = ProfileCatalog::try_from(CatalogData {
            profiles,
            selected: ProfileId::new(1).unwrap(),
            next_id: 2,
        })
        .unwrap();
        assert_eq!(catalog.profiles.capacity(), 1);
    }

    // Serde-only fixtures keep JSON dependencies out of the domain package.
    #[test]
    fn bundled_preferences_migrate_strictly_and_keep_other_preferences_and_owned_capacity() {
        let buffering = || {
            Value::Map(vec![
                ("max_ms", Value::Number(209)),
                ("fixed", Value::Bool(false)),
            ])
        };
        let legacy = UserPreferences::deserialize(Value::Map(vec![
            ("audio_buffering", buffering()),
            ("language", Value::Text("uk-UA")),
        ]))
        .unwrap();
        assert!(legacy.disabled_bundled_plugins().is_empty());
        assert!(legacy.enabled_plugins().is_empty());
        assert!(!legacy.plugin_enabled("unknown/provider"));
        for provider in crate::plugin_catalog()
            .iter()
            .filter(|provider| provider.origin == crate::PluginOrigin::Bundled)
        {
            assert_eq!(legacy.plugin_enabled(provider.id), provider.implemented);
        }
        for bad in [
            Value::Text(crate::CRAFT_AUDIO_PLUGIN),
            Value::Seq(vec![Value::Text("unknown/provider")]),
            Value::Seq(vec![Value::Text(crate::AUDIO_PAN_PLUGIN)]),
            Value::Seq(vec![
                Value::Text(crate::CRAFT_AUDIO_PLUGIN),
                Value::Text(crate::CRAFT_AUDIO_PLUGIN),
            ]),
        ] {
            assert!(
                UserPreferences::deserialize(Value::Map(vec![
                    ("audio_buffering", buffering()),
                    ("disabled_bundled_plugins", bad)
                ]))
                .is_err()
            );
        }
        let mut id = String::with_capacity(8192);
        id.push_str(crate::CRAFT_AUDIO_PLUGIN);
        let mut disabled = Vec::with_capacity(512);
        disabled.push(id);
        let decoded = UserPreferences::try_from(PreferencesData {
            audio_buffering: legacy.audio_buffering(),
            appearance: legacy.appearance(),
            accessibility: legacy.accessibility(),
            hotkeys: crate::HotkeyBindings::default(),
            global_hotkeys:crate::GlobalHotkeyBindings::default(),
            language: legacy.language().into(),
            enabled_plugins: vec![crate::AUDIO_PAN_PLUGIN.into()],
            disabled_bundled_plugins: disabled,
            setup_state: legacy.setup_state(),
            setup_on_new_profile: legacy.setup_on_new_profile(),
        })
        .unwrap();
        assert_eq!(
            decoded.disabled_bundled_plugins.len(),
            decoded.disabled_bundled_plugins.capacity()
        );
        assert_eq!(
            decoded.disabled_bundled_plugins[0].len(),
            decoded.disabled_bundled_plugins[0].capacity()
        );
        assert_eq!(decoded.language(), "uk-UA");
        assert_eq!(decoded.audio_buffering(), legacy.audio_buffering());
        assert!(decoded.plugin_enabled(crate::AUDIO_PAN_PLUGIN));
        assert!(!decoded.plugin_enabled(crate::CRAFT_AUDIO_PLUGIN));
    }
    #[test]
    fn bundled_opt_out_is_user_global_survives_scene_undo_and_never_inserts_filters() {
        use crate::{CRAFT_AUDIO_PLUGIN, Command, Session};
        let mut session = Session::new();
        let initial = session.snapshot();
        assert!(initial.sources().is_empty());
        session
            .apply(Command::AddScene {
                name: "Undo only the scene".into(),
            })
            .unwrap();
        let mut preferences = session.snapshot().preferences().clone();
        preferences.disabled_bundled_plugins = vec![CRAFT_AUDIO_PLUGIN.into()];
        preferences.set_language("de-DE".into()).unwrap();
        session
            .apply(Command::SetUserPreferences {
                preferences: preferences.clone(),
            })
            .unwrap();
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().scenes(), initial.scenes());
        assert_eq!(session.snapshot().preferences(), &preferences);
        assert!(session.snapshot().sources().is_empty());
        let before = session.snapshot();
        let enabled = session.apply(Command::SetPluginEnabled {
            plugin: CRAFT_AUDIO_PLUGIN.into(),
            enabled: true,
        });
        if crate::plugin_descriptor(CRAFT_AUDIO_PLUGIN)
            .unwrap()
            .implemented
        {
            enabled.unwrap();
            assert!(
                session
                    .snapshot()
                    .preferences()
                    .plugin_enabled(CRAFT_AUDIO_PLUGIN)
            );
            assert!(
                session
                    .snapshot()
                    .preferences()
                    .enabled_plugins()
                    .is_empty()
            );
            session
                .apply(Command::SetPluginEnabled {
                    plugin: CRAFT_AUDIO_PLUGIN.into(),
                    enabled: false,
                })
                .unwrap();
            assert!(
                !session
                    .snapshot()
                    .preferences()
                    .plugin_enabled(CRAFT_AUDIO_PLUGIN)
            );
            let disabled = session.snapshot();
            assert!(
                !session
                    .apply(Command::SetPluginEnabled {
                        plugin: CRAFT_AUDIO_PLUGIN.into(),
                        enabled: false
                    })
                    .unwrap()
                    .changed
            );
            assert!(std::sync::Arc::ptr_eq(&disabled, &session.snapshot()));
        } else {
            assert!(matches!(enabled, Err(Error::Unavailable(_))));
            assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        }
        assert!(
            !session.can_undo(),
            "module toggles do not enter scene history"
        );
    }
    enum Value {
        Number(u64),
        Bool(bool),
        Text(&'static str),
        Map(Vec<(&'static str, Value)>),
        Seq(Vec<Value>),
    }
    impl<'de> IntoDeserializer<'de, DeError> for Value {
        type Deserializer = Self;
        fn into_deserializer(self) -> Self {
            self
        }
    }
    impl<'de> serde::Deserializer<'de> for Value {
        type Error = DeError;
        fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, DeError> {
            match self {
                Value::Number(value) => visitor.visit_u64(value),
                Value::Bool(value) => visitor.visit_bool(value),
                Value::Text(value) => visitor.visit_str(value),
                Value::Map(value) => visitor.visit_map(MapDeserializer::new(value.into_iter())),
                Value::Seq(value) => visitor.visit_seq(SeqDeserializer::new(value.into_iter())),
            }
        }
        fn deserialize_enum<V: Visitor<'de>>(
            self,
            name: &'static str,
            variants: &'static [&'static str],
            visitor: V,
        ) -> Result<V::Value, DeError> {
            match self {
                Value::Text(value) => serde::Deserializer::deserialize_enum(
                    value.into_deserializer(),
                    name,
                    variants,
                    visitor,
                ),
                other => other.deserialize_any(visitor),
            }
        }
        serde::forward_to_deserialize_any! { bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map struct identifier ignored_any }
    }
    fn profile_value(id: u64, name: &'static str, rate: u64, width: u64) -> Value {
        Value::Map(vec![
            ("id", Value::Number(id)),
            ("name", Value::Text(name)),
            (
                "video",
                Value::Map(vec![
                    ("width", Value::Number(width)),
                    ("height", Value::Number(1080)),
                    ("fps_num", Value::Number(30)),
                    ("fps_den", Value::Number(1)),
                ]),
            ),
            ("sample_rate", Value::Number(rate)),
            ("speakers", Value::Text("stereo")),
        ])
    }
    fn catalog_value(profiles: Vec<Value>, selected: u64, next: u64) -> Value {
        Value::Map(vec![
            ("profiles", Value::Seq(profiles)),
            ("selected", Value::Number(selected)),
            ("next_id", Value::Number(next)),
        ])
    }
    #[test]
    fn deserialization_validates_ids_formats_names_catalog_relationships_and_unknown_fields() {
        assert!(ProfileId::deserialize(U64Deserializer::<DeError>::new(0)).is_err());
        assert_eq!(
            ProfileId::deserialize(U64Deserializer::<DeError>::new(u64::MAX))
                .unwrap()
                .get(),
            u64::MAX
        );
        let valid = ProfileCatalog::deserialize(catalog_value(
            vec![profile_value(1, " \tDecoded ", 48000, 1920)],
            1,
            2,
        ))
        .unwrap();
        assert_eq!(valid.selected().name(), "Decoded");
        for value in [
            catalog_value(vec![], 1, 2),
            catalog_value(vec![profile_value(1, "A", 48000, 1920)], 1, 0),
            catalog_value(vec![profile_value(1, "A", 48000, 1920)], 1, 1),
            catalog_value(vec![profile_value(1, "A", 48000, 1920)], 2, 3),
            catalog_value(
                vec![
                    profile_value(1, "A", 48000, 1920),
                    profile_value(1, "B", 48000, 1920),
                ],
                1,
                2,
            ),
            catalog_value(
                vec![
                    profile_value(1, " A\t", 48000, 1920),
                    profile_value(2, "A", 48000, 1920),
                ],
                1,
                3,
            ),
            catalog_value(
                (1..=33)
                    .map(|id| profile_value(id, "Duplicate", 48000, 1920))
                    .collect(),
                1,
                34,
            ),
        ] {
            assert!(ProfileCatalog::deserialize(value).is_err());
        }
        for value in [
            profile_value(0, "A", 48000, 1920),
            profile_value(1, "A", 96000, 1920),
            profile_value(1, "A", 48000, 0),
        ] {
            assert!(Profile::deserialize(value).is_err());
        }
        let Value::Map(mut fields) = profile_value(1, "A", 48000, 1920) else {
            unreachable!()
        };
        fields.push(("buffering", Value::Number(80)));
        assert!(
            Profile::deserialize(Value::Map(fields)).is_err(),
            "a profile cannot acquire the global preference"
        );
        let Value::Map(mut fields) = catalog_value(vec![profile_value(1, "A", 48000, 1920)], 1, 2)
        else {
            unreachable!()
        };
        fields.push(("unknown", Value::Number(1)));
        assert!(ProfileCatalog::deserialize(Value::Map(fields)).is_err());
        let buffering = |max_ms| {
            Value::Map(vec![(
                "audio_buffering",
                Value::Map(vec![
                    ("max_ms", Value::Number(max_ms)),
                    ("fixed", Value::Bool(false)),
                ]),
            )])
        };
        assert_eq!(
            UserPreferences::deserialize(buffering(209))
                .unwrap()
                .audio_buffering(),
            AudioBuffering::new(209, false).unwrap()
        );
        let legacy = UserPreferences::deserialize(buffering(209)).unwrap();
        assert_eq!(legacy.language(), "en-US");
        assert!(legacy.enabled_plugins().is_empty());
        let mut session = crate::Session::new();
        session
            .apply(crate::Command::SetUserPreferences {
                preferences: legacy,
            })
            .unwrap();
        assert!(
            !session.is_document_dirty(),
            "legacy startup preferences do not dirty a saved document"
        );
        assert!(
            session.is_dirty(),
            "the native bundle still tracks the complete preference snapshot"
        );
        assert!(UserPreferences::deserialize(buffering(1001)).is_err());
        assert!(
            UserPreferences::deserialize(Value::Map(vec![("unknown", Value::Number(1))])).is_err()
        );
    }
}
