//! Modal drafts are presentation data. Cancel never submits a model mutation.
use super::{
    AudioEdit, DisplayEdit, ImageEdit, Intent, transition_kind_editor, transition_reference_editor,
};
use eframe::egui;
use skylive_core::{
    AudioBuffering, AudioCaptureBinding, AudioFilter, AudioFlow, AudioTarget, Command,
    DisplayTarget, Id, ProfileCommand, ProfileId, QuickTransitionId, SpeakerLayout, StudioSnapshot,
    Transform, TransitionKind, TransitionPresetId, TransitionRef, TransitionSelection,
    VideoSettings,
};
use skylive_host::audio_capture::{AudioCapabilities, AudioDeviceInfo};
use skylive_host::capture::{CaptureCapabilities, DisplayInfo};

pub struct AudioDevicePicker {
    pub platform: crate::platform::CapturePlatform,
    pub token: u64,
    pub session: u64,
    pub revision: u64,
    pub capabilities: AudioCapabilities,
    pub devices: Vec<AudioDeviceInfo>,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AudioTargetChoice {
    Default,
    WindowsEndpoint,
    WindowsProcess,
    MacDevice,
    MacSystem,
}

#[derive(Clone, Copy)]
pub enum ProfileEdit {
    New,
    Duplicate,
    Rename(ProfileId),
}

pub enum Dialog {
    SourceOutputResize(super::source_output_resize::Draft),
    SourceColorTag(super::source_color_tags::Draft),
    SourcePlaybackHotkeys(super::source_playback_hotkeys::Draft),
    SourceAudioHotkeys(super::source_audio_hotkeys::Draft),
    Appearance(super::appearance::AppearanceDraft),
    Accessibility(super::accessibility::Draft),
    AdvancedProcess(super::advanced_process::Draft),
    General {
        snapping:super::preview_snapping::Draft,
        projectors:super::projector_settings::Draft,
        language: String,
    },
    AddScene {
        name: String,
    },
    RenameScene {
        scene: Id,
        name: String,
    },
    DuplicateScene {
        scene: Id,
        name: String,
    },
    ColorSource(super::color_source::ColorDraft),
    AddImage {
        settings:skylive_core::ImageSourceSettings,
        scene: Id,
        name: String,
        path: String,
    },
    ImageFile {
        settings:skylive_core::ImageSourceSettings,
        source: Id,
        path: String,
    },
    Text(super::text_source::TextDraft),
    MediaSource(super::media_source::MediaDraft),
    GameCapture(super::game_capture::Draft),
    NdiSource(super::ndi_source::NdiDraft),
    BrowserSource(super::browser_source::Draft),
    ImageSlideShow(super::image_slideshow::SlideShowDraft),
    SceneSource(super::scene_source::SceneDraft),
    Group(super::group_source::GroupDraft),
    WindowCapture(super::window_capture::WindowDraft),
    CameraCapture(super::camera_capture::CameraDraft),
    DisplayCapture {
        platform: crate::platform::CapturePlatform,
        token: u64,
        session: u64,
        revision: u64,
        edit: DisplayEdit,
        name: String,
        selected: Option<DisplayTarget>,
        capture_cursor: bool,
        method: skylive_core::DisplayCaptureMethod,
        displays: Vec<DisplayInfo>,
        capabilities: CaptureCapabilities,
        loading: bool,
        error: Option<String>,
    },
    AudioCapture {
        edit: AudioEdit,
        name: String,
        flow: AudioFlow,
        target: AudioTargetChoice,
        identity: String,
        use_device_timing: bool,
        exclude_own_audio: bool,
        device_picker: Option<AudioDevicePicker>,
    },
    RenameSource {
        source: Id,
        name: String,
    },
    Transform {
        scene: Id,
        item: Id,
        draft: Transform,
        capture_context: Option<skylive_host::capture::CaptureTransformContext>,
    },
    Video {
        output:super::video_settings::OutputDraft,
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
    },
    Recording(super::recording_settings::RecordingDraft),
    Streaming(super::streaming_settings::StreamingDraft),
    Hotkeys(super::hotkeys::HotkeyDraft),
    AudioSettings {
        sample_rate: u32,
        speakers: SpeakerLayout,
        buffering: AudioBuffering,
    },
    Collection {
        name: String,
    },
    ProfileName {
        edit: ProfileEdit,
        name: String,
        run_setup: bool,
    },
    RemoveProfile {
        id: ProfileId,
        name: String,
    },
    ProfilePath { import: bool, path: String },
    Plugins {
        query: String,
        session: u64,
        token: u64,
    },
    AudioFilters {
        binding: Option<super::plugin_routes::PluginEditorToken>,
        source: Id,
        name: String,
        filters: Vec<AudioFilter>,
    },
    AdvancedAudio(super::advanced_audio::AdvancedAudioDraft),
    GlobalAudioDevices(super::global_audio_devices::GlobalAudioDraft),
    VideoFilters(super::video_filters::VideoFilterDraft),
    NativeMigration(std::sync::Arc<skylive_host::storage::MigrationReport>),
    SceneNotes {
        binding: Option<super::plugin_routes::PluginEditorToken>,
        scene: Id,
        name: String,
        text: String,
    },
    QuickRow {
        id: Option<QuickTransitionId>,
        reference: TransitionRef,
        duration_ms: u32,
        fade_to_black: bool,
    },
    SceneTransition {
        scene: Id,
        enabled: bool,
        reference: TransitionRef,
        duration_ms: u32,
    },
    TransitionPreset {
        id: Option<TransitionPresetId>,
        name: String,
        kind: TransitionKind,
    },
    RemoveTransitionPreset {
        id: TransitionPresetId,
        name: String,
        quick_count: usize,
        scene_count: usize,
        fallback: String,
    },
    About,
    ObsPath {
        import: bool,
        path: String,
        canvas: String,
    },
}
impl Dialog {
    pub fn general(model: &StudioSnapshot) -> Self {
        Self::General {
            snapping:super::preview_snapping::Draft::new(model.preferences().preview_snapping()),
            projectors:super::projector_settings::Draft::new(model.preferences().projectors()),
            language: model.preferences().language().to_owned(),
        }
    }
    pub fn audio_filters(source: &skylive_core::Source) -> Self {
        Self::AudioFilters {
            binding: None,
            source: source.id(),
            name: source.name().into(),
            filters: source.audio_filters().to_vec(),
        }
    }
    pub fn scene_notes(scene: &skylive_core::Scene) -> Self {
        Self::SceneNotes {
            binding: None,
            scene: scene.id(),
            name: scene.name().into(),
            text: scene.notes().into(),
        }
    }
    pub fn audio(edit: AudioEdit, name: String, flow: AudioFlow) -> Self {
        Self::AudioCapture {
            edit,
            name,
            flow,
            target: AudioTargetChoice::Default,
            identity: String::new(),
            use_device_timing: false,
            exclude_own_audio: false,
            device_picker: None,
        }
    }
    pub fn application_audio(edit:AudioEdit,name:String)->Self {
        let mut draft=Self::audio(edit,name,AudioFlow::Output);
        if let Self::AudioCapture {target,..}=&mut draft {*target=AudioTargetChoice::WindowsProcess;}
        draft
    }
    /// Prefill a separate application-audio source from a Windows window
    /// capture. The selector is copied; the sources are not linked.
    pub fn application_audio_for_window(edit:AudioEdit,name:String,binding:&skylive_core::WindowCaptureBinding)->Option<Self> {
        use skylive_core::{WindowTarget,WindowMatchPriority,WindowsAudioWindowPriority,WindowsAudioWindowSelector};
        let WindowTarget::Windows {title,class,executable}=binding.target() else {return None;};
        let priority=match binding.priority() {
            WindowMatchPriority::Title=>WindowsAudioWindowPriority::Title,
            WindowMatchPriority::Class=>WindowsAudioWindowPriority::Class,
            WindowMatchPriority::Executable=>WindowsAudioWindowPriority::Executable,
        };
        let selector=WindowsAudioWindowSelector::new(title.clone(),class.clone(),executable.clone(),priority).ok()?;
        let mut draft=Self::application_audio(edit,name);
        if let Self::AudioCapture {identity,..}=&mut draft {*identity=serde_json::to_string(&selector).ok()?;}
        Some(draft)
    }
    pub fn audio_properties(source: Id, name: String, binding: &AudioCaptureBinding) -> Self {
        let (target, identity) = match binding.target() {
            AudioTarget::Default => (AudioTargetChoice::Default, String::new()),
            // Process selection is typed Core metadata, encoded only in this
            // private presentation string slot; never an editable device ID.
            AudioTarget::WindowsWindow { selector } => (AudioTargetChoice::WindowsProcess,serde_json::to_string(selector).unwrap_or_default()),
            AudioTarget::WindowsProcess { identity } => (AudioTargetChoice::WindowsProcess,serde_json::to_string(identity).unwrap_or_default()),
            AudioTarget::WindowsEndpoint { id } => (AudioTargetChoice::WindowsEndpoint, id.clone()),
            AudioTarget::MacDevice { uid } => (AudioTargetChoice::MacDevice, uid.clone()),
            AudioTarget::MacSystem => (AudioTargetChoice::MacSystem, String::new()),
        };
        Self::AudioCapture {
            edit: AudioEdit::Rebind { source },
            name,
            flow: binding.flow(),
            target,
            identity,
            use_device_timing: binding.use_device_timing(),
            exclude_own_audio: binding.exclude_own_audio(),
            device_picker: None,
        }
    }
    pub fn remove_transition_preset(
        model: &StudioSnapshot,
        id: TransitionPresetId,
    ) -> Result<Self, String> {
        let fallback = model
            .transition_after_removing_preset(id)
            .map_err(|error| error.to_string())?;
        let preset = model
            .transition_preset(id)
            .ok_or_else(|| "Transition preset no longer exists".to_owned())?;
        let reference = TransitionRef::Preset(id);
        Ok(Self::RemoveTransitionPreset {
            id,
            name: preset.name().to_owned(),
            quick_count: model
                .quick_transitions()
                .iter()
                .filter(|row| row.config().reference() == reference)
                .count(),
            scene_count: model
                .scenes()
                .iter()
                .filter(|scene| {
                    scene
                        .transition_override()
                        .is_some_and(|selection| selection.reference() == reference)
                })
                .count(),
            fallback: super::transition_reference_description(model, fallback),
        })
    }
    pub fn streaming(model: &StudioSnapshot) -> Self {
        Self::Streaming(super::streaming_settings::StreamingDraft::from_model(model))
    }
    pub fn recording(model: &StudioSnapshot) -> Self {
        Self::Recording(super::recording_settings::RecordingDraft::from_profile(
            model.profiles().selected(),
        ))
    }
    pub fn video(settings: &VideoSettings) -> Self {
        Self::Video {
            output:super::video_settings::OutputDraft::new(Default::default(),settings),
            width: settings.width(),
            height: settings.height(),
            fps_num: settings.fps_num(),
            fps_den: settings.fps_den(),
        }
    }
    pub fn video_profile(model:&StudioSnapshot)->Self {
        let mut draft=Self::video(model.video());
        if let Self::Video {output,..}=&mut draft {*output=super::video_settings::OutputDraft::new(model.output_video_settings(),model.video());}
        draft
    }
    pub fn audio_settings(settings: &skylive_core::AudioSettings) -> Self {
        Self::AudioSettings {
            sample_rate: settings.sample_rate(),
            speakers: settings.speakers(),
            buffering: settings.buffering(),
        }
    }
    fn title(&self) -> &'static str {
        match self {
            Self::SourceOutputResize(_)=>"Resize output (source size)",
            Self::SourceColorTag(_)=>"Custom Color",
            Self::SourcePlaybackHotkeys(_)=>"Source Playback Shortcuts",
            Self::SourceAudioHotkeys(_)=>"Source Audio Shortcuts",
            Self::General { .. } => "Settings — General",
            Self::Appearance(_) => "Settings — Appearance",
            Self::Accessibility(_) => "Settings — Accessibility",
            Self::AdvancedProcess(_)=>"Settings — Advanced",
            Self::GlobalAudioDevices(_) => "Global Audio Devices",
            Self::Recording(_) => "Settings — Output",
            Self::Streaming(_) => "Settings — Stream",
            Self::Hotkeys(_) => "Settings — Hotkeys",
            Self::AddScene { .. } => "Add Scene",
            Self::RenameScene { .. } => "Rename Scene",
            Self::DuplicateScene { .. } => "Duplicate Scene",
            Self::ColorSource(draft) => draft.title(),
            Self::AddImage { .. } => "Add Image Source",
            Self::ImageFile { .. } => "Properties: Image",
            Self::Text(draft) => draft.title(),
            Self::MediaSource(draft) => draft.title(),
            Self::GameCapture(draft)=>draft.title(),
            Self::NdiSource(draft)=>draft.title(),
            Self::BrowserSource(draft)=>draft.title(),
            Self::ImageSlideShow(draft) => draft.title(),
            Self::SceneSource(_) => "Scene Source",
            Self::Group(_) => "Group",
            Self::CameraCapture(draft) => {
                if matches!(draft.edit, super::CameraEdit::Add { .. }) {
                    "Add Video Capture Device"
                } else {
                    "Properties: Video Capture Device"
                }
            }
            Self::WindowCapture(draft) => {
                if matches!(draft.edit, super::WindowEdit::Add { .. }) {
                    "Add Window Capture"
                } else {
                    "Properties: Window Capture"
                }
            }
            Self::DisplayCapture {
                edit: DisplayEdit::Add { .. },
                ..
            } => "Add Display Capture",
            Self::DisplayCapture { .. } => "Properties: Display Capture",
            Self::AudioCapture {edit:AudioEdit::Rebind {..},target:AudioTargetChoice::WindowsProcess,..} => "Properties: Application Audio Capture",
            Self::AudioCapture {
                edit: AudioEdit::Rebind { .. },
                ..
            } => "Properties: Audio Capture",
            Self::AudioCapture {
                flow: AudioFlow::Input,
                ..
            } => "Add Audio Input Capture",
            Self::AudioCapture {target:AudioTargetChoice::WindowsProcess,..} => "Application Audio Capture",
            Self::AudioCapture { .. } => "Add Audio Output Capture",
            Self::RenameSource { .. } => "Rename Source",
            Self::Transform { .. } => "Edit Transform",
            Self::Video { .. } => "Settings — Video",
            Self::AudioSettings { .. } => "Settings — Audio",
            Self::Collection { .. } => "Rename Scene Collection",
            Self::ProfileName {
                edit: ProfileEdit::New,
                ..
            } => "New Profile",
            Self::ProfileName {
                edit: ProfileEdit::Duplicate,
                ..
            } => "Duplicate Profile",
            Self::ProfileName { .. } => "Rename Profile",
            Self::RemoveProfile { .. } => "Remove Profile",
            Self::ProfilePath { import: true, .. } => "Import Profile",
            Self::ProfilePath { .. } => "Export Profile",
            Self::Plugins { .. } => "Plugins",
            Self::AudioFilters { .. } => "Audio Filters",
            Self::VideoFilters(_) => "Video Filters",
            Self::SceneNotes { .. } => "Scene Notes",
            Self::QuickRow { id: None, .. } => "Add Quick Transition",
            Self::QuickRow { .. } => "Edit Quick Transition",
            Self::SceneTransition { .. } => "Scene Transition Override",
            Self::TransitionPreset { id: None, .. } => "Add Transition Preset",
            Self::TransitionPreset { .. } => "Transition Preset Properties",
            Self::RemoveTransitionPreset { .. } => "Remove Transition Preset",
            Self::NativeMigration(_) => "Scene filter compatibility",
            Self::About => "About SkyLive",
            Self::AdvancedAudio(_) => "Advanced Audio Properties",
            Self::ObsPath { import: true, .. } => "Import OBS Scene Collection",
            Self::ObsPath { .. } => "Export OBS Scene Collection",
        }
    }
    pub(crate) fn command(&self) -> Result<Option<Command>, String> {
        Ok(match self {
            Self::SourceOutputResize(_) | Self::SourcePlaybackHotkeys(_) | Self::SourceAudioHotkeys(_) | Self::SourceColorTag(_) | Self::VideoFilters(_) | Self::ProfilePath { .. } => None,
            Self::Recording(draft) => Some(draft.command()?),
            Self::Streaming(_) => None,
            Self::Hotkeys(_) | Self::Appearance(_) | Self::Accessibility(_) | Self::AdvancedProcess(_) => None,
            Self::Text(draft) => Some(draft.command()?),
            Self::SceneSource(draft) => Some(draft.command()?),
            Self::Group(draft) => Some(draft.command()?),
            Self::AddScene { name } => Some(Command::AddScene { name: name.clone() }),
            Self::RenameScene { scene, name } => Some(Command::RenameScene {
                scene: *scene,
                name: name.clone(),
            }),
            Self::DuplicateScene { scene, name } => Some(Command::DuplicateScene {
                scene: *scene,
                name: name.clone(),
            }),
            Self::ColorSource(_) => None,
            Self::RenameSource { source, name } => Some(Command::RenameSource {
                source: *source,
                name: name.clone(),
            }),
            Self::Transform {
                scene,
                item,
                draft,
                capture_context: None,
            } => Some(Command::SetTransform {
                scene: *scene,
                item: *item,
                transform: draft.clone(),
            }),
            Self::Transform {
                capture_context: Some(_),
                ..
            } => None,
            Self::Video {width,height,fps_num,fps_den,output}=>Some(Command::ConfigureVideo {
                canvas:VideoSettings::new(*width,*height,*fps_num,*fps_den).map_err(|error|error.to_string())?,
                output:output.settings()?,
            }),
            Self::AudioSettings {
                sample_rate,
                speakers,
                buffering,
            } => Some(Command::SetAudioSettings {
                settings: skylive_core::AudioSettings::new(*sample_rate, *speakers)
                    .and_then(|settings| settings.with_buffering(*buffering))
                    .map_err(|error| error.to_string())?,
            }),
            Self::Collection { name } => Some(Command::RenameCollection { name: name.clone() }),
            Self::AudioFilters {
                source, filters, ..
            } => {
                for filter in filters {
                    filter.validate().map_err(|error| error.to_string())?;
                }
                Some(Command::SetSourceAudioFilters {
                    source: *source,
                    filters: filters.clone(),
                })
            }
            Self::SceneNotes { scene, text, .. } => Some(Command::SetSceneNotes {
                scene: *scene,
                text: text.clone(),
            }),
            Self::ProfileName {
                edit: ProfileEdit::New,
                name,
                run_setup,
            } => Some(Command::Setup {
                command: skylive_core::SetupCommand::CreateProfile {
                    name: name.clone(),
                    run_setup: *run_setup,
                },
            }),
            Self::ProfileName { edit, name, .. } => Some(Command::Profile {
                command: match edit {
                    ProfileEdit::New => ProfileCommand::New { name: name.clone() },
                    ProfileEdit::Duplicate => ProfileCommand::Duplicate { name: name.clone() },
                    ProfileEdit::Rename(id) => ProfileCommand::Rename {
                        id: *id,
                        name: name.clone(),
                    },
                },
            }),
            Self::RemoveProfile { id, .. } => Some(Command::Profile {
                command: ProfileCommand::Remove { id: *id },
            }),
            Self::AudioCapture {
                edit,
                name,
                flow,
                target,
                identity,
                use_device_timing,
                exclude_own_audio,
                ..
            } => {
                let target = match target {
                    AudioTargetChoice::WindowsProcess => if let Ok(selector)=serde_json::from_str::<skylive_core::WindowsAudioWindowSelector>(identity){AudioTarget::WindowsWindow{selector}}else{AudioTarget::WindowsProcess {identity:serde_json::from_str(identity).map_err(|_|"Choose a running application before applying".to_owned())?}},
                    AudioTargetChoice::Default => AudioTarget::Default,
                    AudioTargetChoice::WindowsEndpoint => AudioTarget::WindowsEndpoint {
                        id: identity.clone(),
                    },
                    AudioTargetChoice::MacDevice => AudioTarget::MacDevice {
                        uid: identity.clone(),
                    },
                    AudioTargetChoice::MacSystem => AudioTarget::MacSystem,
                };
                let binding =
                    AudioCaptureBinding::new(*flow, target, *use_device_timing, *exclude_own_audio)
                        .map_err(|error| error.to_string())?;
                Some(match edit {
                    AudioEdit::Add { scene } => Command::AddAudioCaptureSource {
                        scene: *scene,
                        name: name.clone(),
                        binding,
                    },
                    AudioEdit::Rebind { source } => Command::RebindAudioCaptureSource {
                        source: *source,
                        binding,
                    },
                    AudioEdit::AddGlobal { slot } => Command::AddGlobalAudioCaptureSource {
                        slot: *slot,
                        name: name.clone(),
                        binding,
                    },
                })
            }
            Self::QuickRow {
                id,
                reference,
                duration_ms,
                fade_to_black,
            } => {
                let selection = TransitionSelection::new(*reference, *duration_ms)
                    .map_err(|error| error.to_string())?;
                Some(match id {
                    Some(id) => Command::UpdateQuickTransitionSelection {
                        id: *id,
                        selection,
                        fade_to_black: *fade_to_black,
                    },
                    None => Command::AddQuickTransitionSelection {
                        selection,
                        fade_to_black: *fade_to_black,
                    },
                })
            }
            Self::SceneTransition {
                scene,
                enabled,
                reference,
                duration_ms,
            } => {
                let selection = TransitionSelection::new(*reference, *duration_ms)
                    .map_err(|error| error.to_string())?;
                Some(Command::ConfigureSceneTransitionSelection {
                    scene: *scene,
                    reference: enabled.then_some(selection.reference()),
                    duration_ms: selection.duration_ms(),
                })
            }
            Self::TransitionPreset { id, name, kind } => Some(match id {
                Some(id) => Command::ConfigureTransitionPreset {
                    id: *id,
                    name: name.clone(),
                    kind: *kind,
                },
                None => Command::AddTransitionPreset {
                    name: name.clone(),
                    kind: *kind,
                },
            }),
            Self::RemoveTransitionPreset { id, .. } => {
                Some(Command::RemoveTransitionPresetAndReferences { id: *id })
            }
            Self::NativeMigration(_)
            | Self::About
            | Self::GlobalAudioDevices(_)
            | Self::AdvancedAudio(_)
            | Self::General { .. }
            | Self::Plugins { .. }
            | Self::ObsPath { .. }
            | Self::AddImage { .. }
            | Self::ImageFile { .. }
            | Self::GameCapture(_)
            | Self::NdiSource(_)
            | Self::BrowserSource(_)
            | Self::MediaSource(_)
            | Self::ImageSlideShow(_)
            | Self::DisplayCapture { .. }
            | Self::WindowCapture(_)
            | Self::CameraCapture(_) => None,
        })
    }
    fn image_intent(&self) -> Option<Intent> {
        match self {
            Self::AddImage { scene, name, path,.. } => Some(Intent::PrepareImage {
                path: path.clone(),
                edit: ImageEdit::Add {
                    scene: *scene,
                    name: name.clone(),
                },
            }),
            Self::ImageFile { source, path,.. } => Some(Intent::PrepareImage {
                path: path.clone(),
                edit: ImageEdit::Replace { source: *source },
            }),
            _ => None,
        }
    }
    fn capture_transform_intent(&self) -> Option<Intent> {
        match self {
            Self::Transform {
                scene,
                item,
                draft,
                capture_context: Some(context),
            } => Some(Intent::ApplyCaptureTransform {
                scene: *scene,
                item: *item,
                transform: draft.clone(),
                context: context.clone(),
            }),
            _ => None,
        }
    }
    pub(super) fn audio_intent(&self) -> Result<Option<Intent>, String> {
        let Self::AudioCapture {
            device_picker: Some(picker),
            ..
        } = self
        else {
            return Ok(None);
        };
        if let Self::AudioCapture {target:AudioTargetChoice::WindowsProcess,identity,..}=self {
            if serde_json::from_str::<skylive_core::WindowsAudioWindowSelector>(identity).is_err(){
                if picker.error.is_some() {return Err("Refresh the application list successfully before applying".into());}
                let selected:skylive_core::WindowsProcessIdentity=serde_json::from_str(identity).map_err(|_|"Choose a running application before applying".to_owned())?;
                if !picker.devices.iter().any(|device|matches!(&device.target,AudioTarget::WindowsProcess {identity} if identity==&selected)) {return Err("The selected application is unavailable. Refresh and choose a running application.".into());}
            }
        }
        if picker.loading {
            return Err("Wait for audio device enumeration before applying".into());
        }
        if let Self::AudioCapture {
            flow,
            target,
            exclude_own_audio,
            ..
        } = self
        {
            if !match flow {
                AudioFlow::Input => picker.capabilities.input,
                AudioFlow::Output => picker.capabilities.output,
            } {
                return Err("This audio capture type is unavailable on this computer.".into());
            }
            if *exclude_own_audio
                && (!picker.capabilities.exclude_own_audio || *flow != AudioFlow::Output)
            {
                return Err("This capture device cannot exclude this application's audio. Clear the option before applying.".into());
            }
            if !super::audio_device::choice_allowed(picker.platform, *flow, *target) {
                return Err("This saved audio device is unavailable for this operating system or capture type. Choose an available device or Default device.".into());
            }
        }
        if matches!(self,Self::AudioCapture {target:AudioTargetChoice::WindowsProcess,..}) {
            return Ok(self.command()?.map(|command|Intent::ApplyApplicationAudioBinding {token:picker.token,session:picker.session,revision:picker.revision,command}));
        }
        Ok(self.command()?.map(|command| Intent::ApplyAudioBinding {
            session: picker.session,
            revision: picker.revision,
            command,
        }))
    }
    fn display_intent(&self) -> Result<Option<Intent>, String> {
        let Self::DisplayCapture {
            session,
            revision,
            edit,
            name,
            selected,
            capture_cursor,
            method,
            displays,
            loading,
            error,
            platform,
            ..
        } = self
        else {
            return Ok(None);
        };
        if *loading {
            return Err("Wait for display enumeration before applying".into());
        }
        if let Some(error) = error {
            return Err(error.clone());
        }
        let display = displays
            .iter()
            .find(|display| Some(&display.target) == selected.as_ref())
            .ok_or("Select an available display")?;
        if !platform.display(&display.target) {
            return Err(
                "This saved display belongs to another platform. Choose an available display."
                    .into(),
            );
        }
        let binding = display
            .binding(*capture_cursor)
            .and_then(|binding| {
                binding
                    .with_method(*method)
                    .map_err(skylive_host::capture::CaptureError::Engine)
            })
            .map_err(|error| error.to_string())?;
        Ok(Some(Intent::CommitDisplay {
            session: *session,
            revision: *revision,
            edit: edit.clone(),
            name: name.clone(),
            binding,
        }))
    }
}
#[cfg(test)]
pub fn show(
    context: &egui::Context,
    slot: &mut Option<Dialog>,
    intents: &mut Vec<Intent>,
    enabled: bool,
    model: &StudioSnapshot,
) {
    show_with_settings(context, slot, intents, enabled, model, None);
}
pub fn show_with_settings(
    context: &egui::Context,
    slot: &mut Option<Dialog>,
    intents: &mut Vec<Intent>,
    enabled: bool,
    model: &StudioSnapshot,
    settings_token: Option<super::settings::SettingsToken>,
) {
    let Some(mut dialog) = slot.take() else {
        return;
    };
    let mut keep = true;
    let response = egui::Modal::new(egui::Id::new("studio-dialog")).show(context, |ui| {
        let width=if super::settings::page(&dialog).is_some(){920.0_f32}else if matches!(dialog,Dialog::AdvancedAudio(_)){940.0_f32}else if matches!(dialog,Dialog::Plugins{..}|Dialog::VideoFilters(_)|Dialog::AudioFilters{..}|Dialog::Hotkeys(_)|Dialog::SourceAudioHotkeys(_)|Dialog::GlobalAudioDevices(_)) {760.0_f32}else{520.0_f32};
        ui.set_width(width.min((context.screen_rect().width()-48.0).max(120.0)));
        ui.heading(crate::i18n::tr(ui,if super::settings::page(&dialog).is_some(){"Settings"}else{dialog.title()}));
        ui.add_enabled_ui(enabled,|ui| {
        let close_only=matches!(dialog,Dialog::NativeMigration(_)|Dialog::About|Dialog::Plugins{..}|Dialog::AdvancedAudio(_)|Dialog::GlobalAudioDevices(_));
        let action=match &dialog {Dialog::SourceOutputResize(_)=>"Yes",Dialog::ObsPath{import:true,..}|Dialog::ProfilePath{import:true,..}=>"Import",Dialog::ObsPath{..}|Dialog::ProfilePath{..}=>"Export",Dialog::RemoveTransitionPreset{..}|Dialog::RemoveProfile{..}=>"Delete",_=>"Apply"};
        let waiting=matches!(&dialog,Dialog::GameCapture(d)if d.loading||d.selected.is_none())||matches!(&dialog,Dialog::NdiSource(d)if d.loading||d.selected.is_none())||matches!(&dialog,Dialog::ImageSlideShow(draft) if draft.loading)||matches!(&dialog,Dialog::Hotkeys(draft)if draft.is_listening())||matches!(&dialog,Dialog::SourcePlaybackHotkeys(draft)if draft.is_listening())||matches!(&dialog,Dialog::SourceAudioHotkeys(draft)if draft.is_listening())||matches!(&dialog,Dialog::MediaSource(draft)if draft.loading||draft.worker_busy)||matches!(&dialog,Dialog::CameraCapture(draft)if draft.loading())||matches!(&dialog,Dialog::WindowCapture(draft) if draft.loading)||matches!(dialog,Dialog::DisplayCapture{loading:true,..})||matches!(&dialog,Dialog::AudioCapture{device_picker:Some(picker),..} if picker.loading);
        let settings_page=super::settings::page(&dialog);
        let empty_profile=matches!(&dialog,Dialog::ProfilePath {path,..} if path.trim().is_empty());
        let apply_help=if empty_profile {"Choose a profile file."}
            else if matches!(&dialog,Dialog::NdiSource(d) if d.selected.is_none()) {"Search and choose an actual NDI sender first."}
            else if waiting {
                match &dialog {
                    Dialog::GameCapture(_)=>"Choose an actual game window and wait for discovery to finish.",
                    Dialog::NdiSource(_)=>"Wait for NDI discovery or video preparation to finish.",
                    Dialog::Hotkeys(_)|Dialog::SourcePlaybackHotkeys(_)|Dialog::SourceAudioHotkeys(_)=>"Finish or cancel the shortcut capture before applying.",
                    Dialog::MediaSource(_)=>super::media_source::WAIT_MEDIA,
                    _=>"Wait for device enumeration to finish",
                }
            }else {"Apply these settings and close the dialog."};
        let mut actions=if let Some(actions)=super::settings::actions(&dialog,model) {actions}
            else if close_only {vec![super::modal_footer::Action::new("Close",true,"Close this dialog.")]}
            else {vec![super::modal_footer::Action::new(action,!waiting&&!empty_profile,apply_help),
                super::modal_footer::Action::new("Cancel",true,"Discard these changes and close the dialog.")]};
        let body_height=(context.screen_rect().height()-super::modal_footer::required_height(ui,&actions)-112.0).max(64.0);
        let mut settings_content=None;
        if let Some(page)=settings_page {
            let unchanged=super::settings::unchanged(&dialog,model);
            let (area,_)=ui.allocate_exact_size(egui::vec2(ui.available_width(),body_height),egui::Sense::hover());
            let sidebar_width=(area.width()*0.23).clamp(120.0,188.0).min(area.width()*0.4);
            let side=egui::Rect::from_min_max(area.min,egui::pos2(area.left()+sidebar_width,area.bottom()));
            let body=egui::Rect::from_min_max(egui::pos2(side.right()+20.0,area.top()),area.max);
            let mut sidebar=ui.new_child(egui::UiBuilder::new().id_salt("settings-sidebar").max_rect(side).layout(egui::Layout::top_down(egui::Align::Min)));
            sidebar.set_clip_rect(side.intersect(ui.clip_rect()));sidebar.set_width(side.width());
            egui::ScrollArea::vertical().id_salt("settings-sidebar-list").show(&mut sidebar,|ui|settings_sidebar(ui,model,page,unchanged,intents));
            ui.painter().line_segment([egui::pos2(side.right()+10.0,area.top()),egui::pos2(side.right()+10.0,area.bottom())],ui.visuals().widgets.noninteractive.bg_stroke);
            let mut content=ui.new_child(egui::UiBuilder::new().id_salt("settings-content").max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
            content.set_clip_rect(body.intersect(ui.clip_rect()));content.set_width(body.width());
            settings_content=Some(content);
        }
        let scroll_parent=settings_content.as_mut().unwrap_or(&mut *ui);
        egui::ScrollArea::vertical().id_salt(("modal-body",dialog.title())).max_height(body_height).auto_shrink([false,true]).show(scroll_parent,|ui| {
        ui.style_mut().wrap_mode=Some(egui::TextWrapMode::Wrap);
        if settings_page.is_some(){ui.data_mut(|data|data.insert_temp(egui::Id::new("settings-sidebar-active"),true));}
        match &mut dialog {
            Dialog::SourceOutputResize(draft)=>super::source_output_resize::editor(ui,model,draft),
            Dialog::SourceColorTag(draft)=>super::source_color_tags::editor(ui,model,draft),
            Dialog::SourceAudioHotkeys(draft)=>super::source_audio_hotkeys::editor(ui,model,draft),
            Dialog::SourcePlaybackHotkeys(draft)=>super::source_playback_hotkeys::editor(ui,model,draft),
            Dialog::General {language,projectors,snapping}=> {
                settings_navigation_page(ui,model,"generalPage",language==model.preferences().language()&&projectors.unchanged(model.preferences().projectors())&&snapping.unchanged(model.preferences().preview_snapping()),intents);
                ui.label(crate::i18n::tr(ui,"Language"));
                let selected=crate::i18n::catalog(ui.ctx()).and_then(|catalog|catalog.languages().iter().find(|locale|locale.code==*language).map(|locale|format!("{} ({})",locale.name,locale.code))).unwrap_or_else(||language.clone());
                egui::ComboBox::from_id_salt("ui-language").selected_text(&selected).show_ui(ui,|ui| {
                    if let Some(catalog)=crate::i18n::catalog(ui.ctx()) {
                        for locale in catalog.languages() {ui.selectable_value(language,locale.code.clone(),crate::i18n::tf(ui,"{} ({})",&[("{}",(locale.name).to_string()),("{}",(locale.code).to_string())])).on_hover_text(crate::hints::text(ui,"Language")).on_disabled_hover_text(crate::hints::text(ui,"Language"));}
                    }
                }).response.on_hover_text(crate::hints::text(ui,"Menu")).on_disabled_hover_text(crate::hints::text(ui,"Menu"));
                ui.label(crate::i18n::tr(ui,"Localization CSV"));ui.add(egui::Label::new(crate::i18n::source(ui.ctx())).wrap());
                ui.label(crate::i18n::tr(ui,"CSV columns are key followed by locale codes."));ui.label(crate::i18n::tr(ui,"Empty translations fall back to English."));
                if let Some(catalog)=crate::i18n::catalog(ui.ctx()) {ui.label(crate::i18n::tf(ui,"{} languages · {} keys · {} missing in the current language",&[("{}",(catalog.languages().len()).to_string()),("{}",(catalog.key_count()).to_string()),("{}",(catalog.missing()).to_string())]));}
                if ui.button(crate::i18n::tr(ui,"Reload localization CSV")).on_hover_text(crate::hints::text(ui,"Reload localization CSV")).on_disabled_hover_text(crate::hints::text(ui,"Reload localization CSV")).clicked(){intents.push(Intent::SubmitSettings{page:super::settings::SettingsPage::General,action:super::settings::SettingsAction::ReloadLanguage,token:settings_token});}
                ui.label(crate::i18n::tr(ui,"Localization changes are separate from scene Undo."));
                ui.separator();super::preview_snapping::editor(ui,snapping);
                ui.separator();super::projector_settings::editor(ui,projectors);
            },
            Dialog::Appearance(draft)=>{
                settings_navigation_page(ui,model,"appearancePage",draft.unchanged(model.preferences().appearance()),intents);
                super::appearance::editor(ui,draft);
            }
            Dialog::AdvancedProcess(draft)=>{
                settings_navigation_page(ui,model,"advancedPage",draft.unchanged(model)&&!draft.busy,intents);
                super::advanced_process::show(ui,draft);
            }
            Dialog::Accessibility(draft)=>{
                settings_navigation_page(ui,model,"accessPage",draft.unchanged(model.preferences().accessibility()),intents);
                super::accessibility::show(ui,draft);
            }
            Dialog::Plugins {query,session,token}=>super::plugin_manager::show(ui,model,query,(*session,*token),intents),
            Dialog::AdvancedAudio(draft)=>super::advanced_audio::editor(ui,model,draft,intents),
            Dialog::GlobalAudioDevices(draft)=>super::global_audio_devices::editor(ui,model,draft,intents),
            Dialog::AudioFilters {binding,source,name,filters}=>super::source_audio_filters::editor(ui,model,*source,*binding,name,filters),
            Dialog::VideoFilters(draft)=>{super::video_filters::editor(ui,model,draft);if let Some(intent)=draft.mask_intent.take(){intents.push(*intent);}if let Some(intent)=draft.lut_intent.take(){intents.push(*intent);}},
            Dialog::NativeMigration(report)=>super::native_migration::show(ui,report),
            Dialog::SceneNotes {name,text,..}=>{
                ui.label(crate::i18n::tf(ui,"Scene: {name}",&[("{name}",(name).to_string())]));
                ui.label(crate::i18n::tr(ui,"Plain-text notes only. Formatted notes, linked files, auto-scroll and hotkeys are pending."));
                let enabled=model.preferences().plugin_enabled(skylive_core::SCENE_NOTES_PLUGIN);
                if !enabled {ui.label(crate::i18n::tr(ui,"Module disabled · notes retained. Enable Scene Notes Dock in Tools > Plugins to edit."));}
                egui::ScrollArea::vertical().id_salt("scene-notes-scroll").max_height(180.0).show(ui,|ui| {
                    ui.add_enabled(enabled,egui::TextEdit::multiline(text).desired_width(440.0).desired_rows(8).char_limit(skylive_core::MAX_SCENE_NOTES_BYTES).id_salt("scene-notes-text")).on_hover_text(crate::hints::text(ui,"scene-notes-text")).on_disabled_hover_text(crate::hints::text(ui,"scene-notes-text"));
                });
                ui.label(crate::i18n::tf(ui,"{} / {} UTF-8 bytes",&[("{}",(text.len()).to_string()),("{}",(skylive_core::MAX_SCENE_NOTES_BYTES).to_string())]));
                ui.label(crate::i18n::tr(ui,"Apply commits this scene's note; Cancel discards the draft."));
            }
            Dialog::AddScene { name } | Dialog::RenameScene { name,.. } | Dialog::DuplicateScene { name,.. } | Dialog::RenameSource { name,.. } => {
                ui.label(crate::i18n::tr(ui,"Name")); ui.add(egui::TextEdit::singleline(name).char_limit(256).id_salt("name")).on_hover_text(crate::hints::text(ui,"name")).on_disabled_hover_text(crate::hints::text(ui,"name"));
            }
            Dialog::Collection{name}=>{
                ui.label(crate::i18n::tr(ui,"Name"));ui.add(egui::TextEdit::singleline(name).char_limit(256).id_salt("name")).on_hover_text(crate::hints::text(ui,"name"));
                ui.label(crate::i18n::tr(ui,"Apply changes the current document name. The managed library name updates only after a successful native Save; external files are not adopted or deleted."));
            }
            Dialog::ProfileName {name,edit,run_setup}=>{
                ui.label(crate::i18n::tr(ui,"Profile name"));
                ui.add(egui::TextEdit::singleline(name).char_limit(170).id_salt("profile-name")).on_hover_text(crate::hints::text(ui,"profile-name")).on_disabled_hover_text(crate::hints::text(ui,"profile-name"));
                if matches!(edit, ProfileEdit::New) {
                    let label = crate::i18n::tr_key(ui,"AddProfile.WizardCheckbox");
                    ui.checkbox(run_setup,label)
                        .on_hover_text(crate::i18n::tr(ui,"Open guided setup for this new profile after creating it."))
                        .on_disabled_hover_text(crate::i18n::tr(ui,"Open guided setup for this new profile after creating it."));
                }
                ui.label(crate::i18n::tr(ui,"Profiles keep their own video and audio format. Scene collection content and global audio sources are preserved. Profile edits are separate from scene Undo."));
            }
            Dialog::RemoveProfile {name,..}=>{
                ui.label(crate::i18n::tf(ui,"Remove profile “{name}”?",&[("{name}",(name).to_string())]));
                ui.label(crate::i18n::tr(ui,"Another profile will become selected. Scene collection content, source bindings and user preferences are preserved. Profile deletion is separate from scene Undo."));
            }
            Dialog::ProfilePath {import,path}=>{
                if let Some(intent)=crate::native_browse::button(ui,"Browse…",crate::native_browse::Target::Profile,true) {intents.push(intent);}
                ui.label(crate::i18n::tr(ui,"SkyLive profile (*.skyliveprofile)"));
                ui.label(crate::i18n::tr(ui,if *import {"Import video, audio and output settings into a separate profile."} else {"Export the selected profile's video, audio and output settings."}));
                ui.text_edit_singleline(path).on_hover_text(crate::i18n::tr(ui,"Enter a profile file path."));
            }
            Dialog::Text(draft) => super::text_source::editor(ui,draft,intents),
            Dialog::ImageSlideShow(draft)=>{
                ui.horizontal_wrapped(|ui| {
                    if let Some(intent)=crate::native_browse::button(ui,"Browse…",crate::native_browse::Target::SlideFiles,!draft.loading) {intents.push(intent);}
                    if let Some(intent)=crate::native_browse::button(ui,"Add folder…",crate::native_browse::Target::SlideFolder,!draft.loading) {intents.push(intent);}
                });
                super::image_slideshow::editor(ui,draft,intents);
            },
            Dialog::GameCapture(draft)=>super::game_capture::editor(ui,draft,intents),
            Dialog::NdiSource(draft)=>super::ndi_source::editor(ui,draft,intents),
            Dialog::BrowserSource(draft)=>super::browser_source::editor(ui,draft,intents),
            Dialog::MediaSource(draft) => {
                if let Some(intent)=crate::native_browse::button(ui,"Browse…",crate::native_browse::Target::Media,!draft.loading&&!draft.worker_busy) {intents.push(intent);}
                super::media_source::editor(ui,draft);
            },
            Dialog::SceneSource(draft) => super::scene_source::editor(ui,model,draft),
            Dialog::Group(draft) => super::group_source::editor(ui,draft),
            Dialog::ColorSource(draft)=>super::color_source::editor(ui,draft),
            Dialog::AddImage { name,path,settings,.. } => {
                ui.label(crate::i18n::tr(ui,"Name"));
                ui.add(egui::TextEdit::singleline(name).char_limit(256).id_salt("image-name")).on_hover_text(crate::hints::text(ui,"image-name")).on_disabled_hover_text(crate::hints::text(ui,"image-name"));
                image_path_editor(ui,path);
                super::image_source::options(ui,settings);
                if let Some(intent)=crate::native_browse::button(ui,"Browse…",crate::native_browse::Target::Image,true) {intents.push(intent);}
            }
            Dialog::ImageFile { path,settings,.. } => {
                image_path_editor(ui,path);
                super::image_source::options(ui,settings);
                if let Some(intent)=crate::native_browse::button(ui,"Browse…",crate::native_browse::Target::Image,true) {intents.push(intent);}
                ui.label(crate::i18n::tr(ui,"Apply reloads file content even when the path is unchanged. All scene references update together after validation."));
            }
            Dialog::CameraCapture(draft)=>super::camera_capture::editor(ui,draft,intents,(model.video().fps_num(),model.video().fps_den())),
            Dialog::WindowCapture(draft)=>super::window_capture::editor(ui,draft,intents),
            Dialog::DisplayCapture {edit,name,selected,capture_cursor,method,displays,capabilities,loading,error,platform,..}=>{
                if matches!(edit,DisplayEdit::Add {..}) {ui.label(crate::i18n::tr(ui,"Name"));ui.add(egui::TextEdit::singleline(name).char_limit(256).id_salt("display-source-name")).on_hover_text(crate::hints::text(ui,"display-source-name")).on_disabled_hover_text(crate::hints::text(ui,"display-source-name"));}
                ui.label(crate::i18n::tr(ui,"Display"));
                if *loading {ui.horizontal(|ui|{ui.spinner();ui.label(crate::i18n::tr(ui,"Enumerating displays…"));});}
                if let Some(error)=error {ui.colored_label(ui.visuals().error_fg_color,error.as_str());}
                let selected_label=displays.iter().find(|display|platform.display(&display.target)&&Some(&display.target)==selected.as_ref()).map_or("Select display",|display|display.name.as_str());
                ui.add_enabled_ui(!*loading,|ui|egui::ComboBox::from_id_salt("display-target").selected_text(selected_label).width(ui.available_width().min(450.0)).truncate().show_ui(ui,|ui|{
                    ui.set_width(ui.available_width().min(450.0));ui.style_mut().wrap_mode=Some(egui::TextWrapMode::Wrap);
                    for display in displays.iter().filter(|display|platform.display(&display.target)){ui.selectable_value(selected,Some(display.target.clone()),crate::i18n::tf(ui,"{} — {}×{}{}",&[("{}",(display.name).to_string()),("{}",(display.width).to_string()),("{}",(display.height).to_string()),("{}",(if display.primary {" · Primary"}else{""}).to_string())])).on_hover_text(&display.name).on_hover_text(crate::hints::text(ui,"Display")).on_disabled_hover_text(crate::hints::text(ui,"Display"));}
                }).response.on_hover_text(selected_label).on_hover_text(crate::hints::text(ui,"Menu")).on_disabled_hover_text(crate::hints::text(ui,"Menu")));
                if !*loading&&displays.is_empty()&&error.is_none(){ui.label(crate::i18n::tr(ui,"No displays are available."));}
                if selected.as_ref().is_some_and(|target|!platform.display(target)){ui.colored_label(ui.visuals().warn_fg_color,crate::i18n::tr(ui,"This saved display belongs to another platform. Choose an available display."));}
                ui.label(crate::i18n::tr(ui,"Capture Method"));
                let windows=*platform==crate::platform::CapturePlatform::Windows;
                let label=if !windows&&*method!=skylive_core::DisplayCaptureMethod::Auto{"Unavailable capture method"}else{match method {skylive_core::DisplayCaptureMethod::Auto=>"Automatic",skylive_core::DisplayCaptureMethod::Gdi=>"GDI compatibility",skylive_core::DisplayCaptureMethod::Dxgi=>"DXGI Desktop Duplication",skylive_core::DisplayCaptureMethod::Wgc=>"Windows Graphics Capture"}};
                if !windows&&*method!=skylive_core::DisplayCaptureMethod::Auto{ui.colored_label(ui.visuals().warn_fg_color,crate::i18n::tr(ui,"This saved capture method is unavailable on this platform. Choose Automatic."));}
                egui::ComboBox::from_id_salt("display-method").selected_text(crate::i18n::tr(ui,label)).show_ui(ui,|ui| {
                    ui.selectable_value(method,skylive_core::DisplayCaptureMethod::Auto,crate::i18n::tr(ui,"Automatic"))
                        .on_hover_text(crate::i18n::tr(ui,"Prefer DXGI with optional cursor. Use GDI compatibility after a reported DXGI failure; waiting for a new frame never changes methods."))
                        .on_disabled_hover_text(crate::i18n::tr(ui,"Wait for the active display operation before changing the capture method."));
                    if windows {
                        ui.selectable_value(method,skylive_core::DisplayCaptureMethod::Gdi,crate::i18n::tr(ui,"GDI compatibility"))
                            .on_hover_text(crate::i18n::tr(ui,"Capture this display through GDI, including the optional cursor. Protected content and hardware overlays may be unavailable."))
                            .on_disabled_hover_text(crate::i18n::tr(ui,"GDI compatibility requires a Windows display target."));
                        ui.selectable_value(method,skylive_core::DisplayCaptureMethod::Dxgi,crate::i18n::tr(ui,"DXGI Desktop Duplication"))
                            .on_hover_text(crate::i18n::tr(ui,"Use real DXGI Desktop Duplication with optional color, masked-color or monochrome cursor. An ordinary no-new-frame wait retains the last admitted image."))
                            .on_disabled_hover_text(crate::i18n::tr(ui,"DXGI Desktop Duplication requires a Windows display target and a supported graphics adapter."));
                    }
                    if windows {ui.selectable_value(method,skylive_core::DisplayCaptureMethod::Wgc,crate::i18n::tr(ui,"Windows Graphics Capture"))
                        .on_hover_text(crate::i18n::tr(ui,"Capture the full monitor with Windows Graphics Capture. Requires Windows 10 1903 or newer; disabling the cursor requires runtime cursor-toggle support."))
                        .on_disabled_hover_text(crate::i18n::tr(ui,"Windows Graphics Capture requires a Windows display target and runtime support."));}
                }).response.on_hover_text(crate::i18n::tr(ui,"Choose the actual display capture provider. Automatic reports its effective backend and any compatibility fallback."))
                    .on_disabled_hover_text(crate::i18n::tr(ui,"Wait for the active display operation before changing the capture method."));
                ui.add_enabled(capabilities.cursor,egui::Checkbox::new(capture_cursor,crate::i18n::tr(ui,"Capture cursor"))).on_hover_text(crate::hints::text(ui,"Capture cursor")).on_disabled_hover_text(crate::i18n::tr(ui,"This backend does not support cursor composition."));
                if ui.add_enabled(!*loading,egui::Button::new(crate::i18n::tr(ui,"Refresh Displays"))).on_hover_text(crate::hints::text(ui,"Refresh Displays")).on_disabled_hover_text(crate::hints::text(ui,"Refresh Displays")).clicked(){intents.push(Intent::RefreshDisplays);}
                egui::CollapsingHeader::new(crate::i18n::tr(ui,"Advanced")).id_salt("display-capture-advanced").show(ui,|ui| {
                    ui.label(crate::i18n::tf(ui,"Backend: {} · SDR",&[("{}",(capabilities.backend).to_string())]));
                    if let Some(note)=capabilities.note {ui.label(crate::i18n::tr(ui,note));}
                    if selected.as_ref().is_some_and(|target|!platform.display(target)){ui.colored_label(ui.visuals().warn_fg_color,crate::i18n::tr(ui,"This saved display belongs to another platform. Choose an available display."));}
                    if windows {ui.label(crate::i18n::tr(ui,"Automatic prefers DXGI with optional cursor; reported DXGI failures can use GDI compatibility. Select WGC explicitly to use Windows Graphics Capture. Ordinary no-new-frame waits never switch methods."));}
                    else if *platform==crate::platform::CapturePlatform::Macos {ui.label(crate::i18n::tr(ui,"macOS Automatic uses ScreenCaptureKit."));}else{ui.label(crate::i18n::tr(ui,"Display capture is unavailable on this platform."));}
                });
            }
            Dialog::AudioCapture {edit,name,flow,target,identity,use_device_timing,exclude_own_audio,device_picker}=>{
                super::audio_device::editor(ui,super::audio_device::AudioDeviceFields{edit,name,flow:*flow,target,identity,use_device_timing,exclude_own_audio,picker:device_picker.as_mut()},intents);
            }
            Dialog::QuickRow {reference,duration_ms,fade_to_black,..}=>{
                transition_fields(ui,model,reference,duration_ms);
                ui.checkbox(fade_to_black,crate::i18n::tr(ui,"Fade To Black")).on_hover_text(crate::hints::text(ui,"Fade To Black")).on_disabled_hover_text(crate::hints::text(ui,"Fade To Black"));
                ui.label(crate::i18n::tr(ui,"This quick button uses its own kind/duration and bypasses the scene override without changing global configuration."));
            }
            Dialog::SceneTransition {enabled,reference,duration_ms,..}=>{
                ui.checkbox(enabled,crate::i18n::tr(ui,"Override global transition for this scene")).on_hover_text(crate::hints::text(ui,"Override global transition for this scene")).on_disabled_hover_text(crate::hints::text(ui,"Override global transition for this scene"));
                ui.add_enabled_ui(*enabled,|ui|transition_reference_editor(ui,"scene-reference",model,reference));
                ui.horizontal(|ui|{ui.label(crate::i18n::tr(ui,"Duration (ms)"));ui.add_enabled(!*enabled||*reference!=TransitionRef::Cut,egui::DragValue::new(duration_ms).speed(1)).on_hover_text(crate::hints::text(ui,"Duration (ms)")).on_disabled_hover_text(crate::hints::text(ui,"Duration (ms)"));});
                ui.label(crate::i18n::tr(ui,"The scene duration is remembered even while its override is disabled. Valid range: 50–20000 ms."));
                ui.label(crate::i18n::tr(ui,"Automatic Transition uses the destination scene override. Quick buttons keep their own configuration. Cancel discards all draft fields."));
            }
            Dialog::TransitionPreset {name,kind,..}=>{
                ui.label(crate::i18n::tr(ui,"Preset name"));ui.add(egui::TextEdit::singleline(name).char_limit(256).id_salt("preset-name")).on_hover_text(crate::hints::text(ui,"preset-name")).on_disabled_hover_text(crate::hints::text(ui,"preset-name"));
                transition_kind_editor(ui,"preset-provider-properties",kind,true);
                ui.label(crate::i18n::tr(ui,"Properties belong to this shared named preset. Applying updates every global, quick-row and scene reference together; duration remains per selection."));
            }
            Dialog::RemoveTransitionPreset {name,quick_count,scene_count,fallback,..}=>{
                ui.label(crate::i18n::tf(ui,"Remove preset “{name}”?",&[("{name}",(name).to_string())]));
                ui.label(crate::i18n::tf(ui,"Quick transition rows removed: {quick_count}",&[("{quick_count}",(quick_count).to_string())]));
                ui.label(crate::i18n::tf(ui,"Scene overrides cleared: {scene_count} (their durations are retained)",&[("{scene_count}",(scene_count).to_string())]));
                ui.label(crate::i18n::tf(ui,"Global transition after removal: {fallback}",&[("{fallback}",(fallback).to_string())]));
                ui.label(crate::i18n::tr(ui,"Delete commits the preset and all affected references as one undoable edit. Cancel keeps them unchanged."));
            }
            Dialog::Transform { scene,draft,capture_context,.. } => {
                if model.group_parent(*scene).is_some(){ui.label(crate::i18n::tr(ui,"These positions and extents are local to the parent group."));}
                if capture_context.is_some() {ui.label(crate::i18n::tr(ui,"Width and height are the full filtered-source extents in canvas pixels. If source dimensions or filters change, reopen this dialog before applying."));}
                egui::Grid::new("transform-fields").show(ui,|ui| {
                    for (name,value) in [("Position X (canvas px)",&mut draft.x),("Position Y (canvas px)",&mut draft.y),("Full Width (canvas px)",&mut draft.width),("Full Height (canvas px)",&mut draft.height),("Rotation (clockwise degrees)",&mut draft.rotation_degrees),("Opacity (0–1)",&mut draft.opacity)] {
                        ui.label(crate::i18n::tr(ui,name)); ui.add(egui::DragValue::new(value).speed(0.1)).on_hover_text(crate::hints::text(ui,"Numeric value")).on_disabled_hover_text(crate::hints::text(ui,"Numeric value")); ui.end_row();
                    }
                });
                ui.checkbox(&mut draft.flip_x, crate::i18n::tr(ui,"Flip Horizontally")).on_hover_text(crate::hints::text(ui,"Flip Horizontally")).on_disabled_hover_text(crate::hints::text(ui,"Flip Horizontally"));
                ui.checkbox(&mut draft.flip_y, crate::i18n::tr(ui,"Flip Vertically")).on_hover_text(crate::hints::text(ui,"Flip Vertically")).on_disabled_hover_text(crate::hints::text(ui,"Flip Vertically"));
                ui.separator();
                ui.strong(crate::i18n::tr(ui,"Crop (native source pixels)"));
                egui::Grid::new("transform-crop-fields").show(ui,|ui| {
                    for (name,value) in [("Left",&mut draft.crop.left),("Top",&mut draft.crop.top),("Right",&mut draft.crop.right),("Bottom",&mut draft.crop.bottom)] {
                        ui.label(crate::i18n::tr(ui,name)); ui.add(egui::DragValue::new(value).speed(1.0)).on_hover_text(crate::hints::text(ui,"Numeric value")).on_disabled_hover_text(crate::hints::text(ui,"Numeric value")); ui.end_row();
                    }
                });
                ui.label(crate::i18n::tr(ui,"Width and height describe the full uncropped source after scaling. Position is the OBS top-left-alignment pivot; rotated bounds may start elsewhere."));
                ui.label(crate::i18n::tr(ui,"Apply validates and commits all fields as one edit. Cancel discards the draft."));
            }
            Dialog::Streaming(draft)=>{settings_navigation_page(ui,model,"streamPage",draft.unchanged(model),intents);super::streaming_settings::editor(ui,draft);if std::mem::take(&mut draft.encoder.request){intents.push(Intent::ProbeNvenc {token:settings_token});}}
            Dialog::Recording(draft)=>{settings_navigation_page(ui,model,"outputPage",draft.unchanged(model),intents);super::recording_settings::editor(ui,draft,intents);if std::mem::take(&mut draft.encoder.request){intents.push(Intent::ProbeNvenc {token:settings_token});}}
            Dialog::Hotkeys(draft)=>{settings_navigation_page(ui,model,"hotkeyPage",draft.unchanged_preferences(model.preferences()),intents);super::hotkeys::editor(ui,draft);super::source_playback_hotkeys::settings_sources(ui,model,draft.source_scope,!draft.is_listening()&&draft.unchanged_preferences(model.preferences()),intents);super::source_audio_hotkeys::settings_sources(ui,model,draft.source_scope,!draft.is_listening()&&draft.unchanged_preferences(model.preferences()),intents);}
            Dialog::Video {width,height,fps_num,fps_den,output}=>{
                let unchanged=(*width,*height,*fps_num,*fps_den)==(model.video().width(),model.video().height(),model.video().fps_num(),model.video().fps_den())&&output.unchanged(model.output_video_settings());
                settings_navigation_page(ui,model,"videoPage",unchanged,intents);
                super::video_settings::editor(ui,width,height,fps_num,fps_den,output);
            }
            Dialog::AudioSettings {sample_rate,speakers,buffering}=>{
                settings_navigation(ui,model,true,(*sample_rate,*speakers,*buffering)==(model.audio().sample_rate(),model.audio().speakers(),model.audio().buffering()),intents);
                let unchanged=(*sample_rate,*speakers,*buffering)==(model.audio().sample_rate(),model.audio().speakers(),model.audio().buffering());
                super::source_audio_hotkeys::settings_sources(ui,model,settings_token.map(|token|(token.session,token.revision)),unchanged,intents);
                let devices=ui.add_enabled(unchanged,egui::Button::new(crate::i18n::tr(ui,"Global Audio Devices"))).on_hover_text(crate::i18n::tr(ui,"Manage the six global audio slots. Confirm or cancel the format draft before opening device settings.")).on_disabled_hover_text(crate::i18n::tr(ui,"Apply or Cancel the current draft before changing Settings pages."));
                #[cfg(test)] ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new("settings-audio-devices"),(devices.rect,devices.enabled())));
                if devices.clicked(){intents.push(Intent::OpenGlobalAudioDevices);}
                ui.separator();
                ui.add(egui::Label::new(crate::i18n::tr(ui,"Sample Rate")).wrap());
                egui::ComboBox::from_id_salt("project-audio-sample-rate").selected_text(audio_rate_label(*sample_rate)).show_ui(ui,|ui|{for (rate,label) in [(44100,"44.1 kHz"),(48000,"48 kHz")] {ui.selectable_value(sample_rate,rate,label).on_hover_text(crate::hints::text(ui,"Item")).on_disabled_hover_text(crate::hints::text(ui,"Item"));}}).response.on_hover_text(crate::hints::text(ui,"Menu")).on_disabled_hover_text(crate::hints::text(ui,"Menu"));
                ui.add(egui::Label::new(crate::i18n::tr(ui,"Channels")).wrap());
                egui::ComboBox::from_id_salt("project-audio-speakers").selected_text(crate::i18n::tr(ui,speakers.label())).show_ui(ui,|ui|{for layout in SpeakerLayout::ALL {ui.push_id(layout,|ui|ui.selectable_value(speakers,layout,crate::i18n::tr(ui,layout.label())).on_hover_text(crate::hints::text(ui,"Item")).on_disabled_hover_text(crate::hints::text(ui,"Item")));}}).response.on_hover_text(crate::hints::text(ui,"Menu")).on_disabled_hover_text(crate::hints::text(ui,"Menu"));
                ui.add(egui::Label::new(crate::i18n::tf(ui,"Chosen Program format: {} · {} · {} channels",&[("{}",(audio_rate_label(*sample_rate)).to_string()),("{}",crate::i18n::tr(ui,speakers.label())),("{}",(speakers.channels()).to_string())])).wrap());
                let mut low_latency=buffering.is_low_latency();
                if ui.checkbox(&mut low_latency,crate::i18n::tr(ui,"Low Latency Audio Buffering")).on_hover_text(crate::hints::text(ui,"Low Latency Audio Buffering")).on_disabled_hover_text(crate::hints::text(ui,"Low Latency Audio Buffering")).changed(){*buffering=if low_latency {AudioBuffering::low_latency()}else{AudioBuffering::default()};}
                ui.add(egui::Label::new(crate::i18n::tr(ui,"Enable to reduce the buffer limit; this can be less reliable on busy devices. Turning this option off restores automatic buffering.")).wrap());
                if *buffering!=AudioBuffering::default()&&!buffering.is_low_latency(){ui.add(egui::Label::new(if buffering.max_ms()==0 {crate::i18n::tr(ui,"Custom buffering uses the default maximum and is preserved.")}else{crate::i18n::tf(ui,if buffering.fixed(){"Custom fixed buffering, maximum {max_ms} ms, is preserved."}else{"Custom automatic buffering, maximum {max_ms} ms, is preserved."},&[("{max_ms}",buffering.max_ms().to_string())])}).wrap());ui.add(egui::Label::new(crate::i18n::tr(ui,"Changing this option replaces the custom buffering preference.")).wrap());}
                let monitoring_session=super::audio_monitoring::session(ui);
                super::audio_monitoring::device(ui,model,monitoring_session,unchanged,intents);
                ui.add(egui::Label::new(crate::i18n::tr(ui,"Devices can use a different input format; audio is converted to these project settings. Windows monitoring uses a separate selected playback endpoint.")).wrap());
                ui.add(egui::Label::new(crate::i18n::tr(ui,"Apply updates the selected profile's audio format and user buffering preference together. These settings are separate from scene Undo. Source bindings, gain and mute stay unchanged.")).wrap());
            }
            Dialog::About => {
                ui.heading(crate::i18n::tr(ui,"SkyLive")); ui.label(crate::i18n::tr(ui,"Rust rewrite of OBS Studio — development build"));
                ui.label(crate::i18n::tr(ui,"GPL-2.0-or-later. OBS source and licenses are retained in doc/obs-studio."));
                ui.label(crate::i18n::tr(ui,"Scene editing, Text, Images/GIF, display/window capture, camera video, audio mixing, transitions and source effects are supported."));
                ui.label(crate::i18n::tr(ui,"Complete OBS parity remains unfinished. Browser Source requires its matching external CEF runtime. Recording uses the available native platform encoder."));
                ui.label(crate::i18n::tr(ui,"Craft kernels: MIT. Bundled Noto fonts: SIL Open Font License. Third-party notices accompany this application."));
            }
            Dialog::ObsPath { import,path,canvas } => {
                ui.label(crate::i18n::tr(ui,"OBS collection JSON path"));
                ui.add(egui::TextEdit::singleline(path).desired_width(440.0).char_limit(4096).id_salt("obs-path")).on_hover_text(crate::hints::text(ui,"obs-path")).on_disabled_hover_text(crate::hints::text(ui,"obs-path"));
                ui.label(crate::i18n::tr(ui,"Import supports represented OBS v1 absolute and v2 relative coordinates at the selected base canvas, including private Groups. Export uses v1 absolute coordinates. Custom canvases, bounds modes and unsupported providers or filters are rejected."));
                if *import { ui.label(crate::i18n::tf(ui,"Import canvas comes from current profile: {canvas}. Adjust Video settings before import if required.",&[("{canvas}",(canvas).to_string())])); }
                ui.label(crate::i18n::tr(ui,"Imported OBS UUIDs and item IDs are retained only in this session. Native save/reopen does not retain this interchange sidecar."));
                ui.label(crate::i18n::tr(ui,"OBS export does not mark native edits as saved."));
            }
        }
        });
        context.data_mut(|data|data.remove::<bool>(egui::Id::new("settings-sidebar-active")));
        // Shortcut capture is processed by the editor in this same frame.
        // Reflect its final state rather than retaining a stale disabled Apply.
        if settings_page.is_some(){actions=super::settings::actions(&dialog,model).unwrap_or_default();}
        else if let Dialog::Hotkeys(draft)=&dialog {
            actions[0].enabled = !draft.is_listening();
            actions[0].hint=if draft.is_listening(){"Finish or cancel the shortcut capture before applying."}else{"Apply these settings and close the dialog."};
        }
        if let Dialog::SourceOutputResize(draft)=&dialog{actions[0].enabled=draft.ready;actions[0].hint=if draft.ready{"Resize canvas and output, then fit the selected unlocked item."}else{"Resize output is unavailable. Resolve the shown reason or cancel."};if actions.len()>1{actions[1].label="No";}}
        if let Dialog::SourceAudioHotkeys(draft)=&dialog{actions[0].enabled=draft.current&&draft.prepared(model).is_ok();actions[0].hint=if actions[0].enabled{"Save mute shortcuts for this exact source."}else{"Resolve source shortcut errors or cancel this draft."};}
        if let Dialog::SourcePlaybackHotkeys(draft)=&dialog{actions[0].enabled=draft.current&&draft.prepared(model).is_ok();actions[0].hint=if actions[0].enabled{"Save playback shortcuts for this exact source."}else{"Resolve source shortcut errors or cancel this draft."};}
        if let Dialog::SourceColorTag(draft)=&dialog{actions[0].enabled=super::source_color_tags::fresh(context,model,draft);actions[0].hint=if actions[0].enabled{"Commit this selected item row color. Source pixels and audio are unchanged."}else{"This source color draft is obsolete. Cancel and reopen Set Color."};}
        ui.separator();
        let clicked=super::modal_footer::show(ui,&actions);
        if let Some(page)=settings_page {
            match clicked {
                Some(0)=>match super::settings::preflight(&dialog,model){Ok(())=>intents.push(Intent::SubmitSettings{page,action:super::settings::SettingsAction::Ok,token:settings_token}),Err(error)=>intents.push(Intent::DialogError(error))},
                Some(1)=>keep=false,
                Some(2)=>match super::settings::preflight(&dialog,model){Ok(())=>intents.push(Intent::SubmitSettings{page,action:super::settings::SettingsAction::Apply,token:settings_token}),Err(error)=>intents.push(Intent::DialogError(error))},
                _=>{},
            }
        } else {match clicked {
            Some(0) if close_only=>keep=false,
            Some(0)=>{
                if let Dialog::SourceOutputResize(draft)=&dialog{intents.push(Intent::ApplySourceOutputResize{token:draft.token()});}
                else if let Dialog::ProfilePath {import,path}=&dialog {intents.push(if *import {Intent::ImportProfile(path.clone())} else {Intent::ExportProfile(path.clone())});}
                else if let Dialog::SourceAudioHotkeys(draft)=&dialog{match draft.intent(model){Ok(intent)=>intents.push(intent),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if let Dialog::SourcePlaybackHotkeys(draft)=&dialog{match draft.intent(model){Ok(intent)=>intents.push(intent),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if let Dialog::SourceColorTag(draft)=&dialog{intents.push(Intent::SourceColorTag(draft.action()));}
                else if let Dialog::General{language,..}=&dialog {intents.push(Intent::ApplyLocalization{language:language.clone()});}
                else if let Dialog::ColorSource(draft)=&dialog{match draft.intent(){Ok(intent)=>intents.push(intent),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if let Dialog::Appearance(draft)=&dialog{match draft.command(model.preferences().appearance()){Ok(command)=>intents.push(Intent::Command(command)),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if let Dialog::Streaming(draft)=&dialog{match draft.request(){Ok(request)=>intents.push(Intent::ApplyStreamingSettings(request)),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if let Dialog::Hotkeys(draft)=&dialog{match draft.command_preferences(model.preferences()){Ok(command)=>intents.push(Intent::Command(command)),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if let Dialog::ObsPath{import,path,..}=&dialog {intents.push(if *import{Intent::ImportObs{path:path.clone()}}else{Intent::ExportObs{path:path.clone()}});keep=false;}
                else if let Dialog::Text(draft)=&dialog{match draft.font_key(){Ok(key)=>intents.push(Intent::ApplyTextSource(key)),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if let Dialog::CameraCapture(draft)=&dialog{match draft.intent(){Ok(intent)=>intents.push(intent),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if let Dialog::WindowCapture(draft)=&dialog {match draft.intent(){Ok(intent)=>intents.push(intent),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if matches!(dialog,Dialog::DisplayCapture{..}) {match dialog.display_intent(){Ok(Some(intent))=>intents.push(intent),Ok(None)=>{},Err(error)=>intents.push(Intent::DialogError(error))}}
                else if matches!(dialog,Dialog::AudioCapture{..}) {match dialog.audio_intent(){Ok(Some(intent))=>intents.push(intent),Ok(None)=>{},Err(error)=>intents.push(Intent::DialogError(error))}}
                else if matches!(&dialog,Dialog::AudioFilters{..}|Dialog::SceneNotes{..}) {
                    let token=match &dialog {Dialog::AudioFilters{binding,..}|Dialog::SceneNotes{binding,..}=>*binding,_=>None};
                    match dialog.command(){Ok(Some(command))=>intents.push(Intent::ApplyPluginEditor{token,command}),Ok(None)=>{},Err(error)=>intents.push(Intent::DialogError(error))}
                }
                else if let Dialog::VideoFilters(draft)=&dialog{match draft.intent(){Ok(intent)=>intents.push(intent),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if let Dialog::ImageSlideShow(draft)=&dialog{match draft.request(){Ok(request)=>intents.push(Intent::PrepareSlideShow(request)),Err(error)=>intents.push(Intent::DialogError(crate::i18n::tr(ui,error)))}}
                else if let Dialog::BrowserSource(draft)=&dialog {match draft.request(){Ok(request)=>intents.push(Intent::CommitBrowser(request)),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if let Dialog::GameCapture(draft)=&dialog {match draft.request(){Ok(request)=>intents.push(Intent::ApplyGameCapture(request)),Err(error)=>intents.push(Intent::DialogError(crate::i18n::tr(ui,error)))}}
                else if let Dialog::NdiSource(draft)=&dialog {match draft.request(){Ok(request)=>intents.push(Intent::PrepareNdi(request)),Err(error)=>intents.push(Intent::DialogError(crate::i18n::tr(ui,error)))}}
                else if let Dialog::MediaSource(draft)=&dialog{match draft.request(){Ok(request)=>intents.push(Intent::PrepareMedia(request)),Err(error)=>intents.push(Intent::DialogError(error))}}
                else if let Some(intent)=dialog.capture_transform_intent(){intents.push(intent);}
                else if let Some(intent)=dialog.image_intent(){intents.push(intent);}
                else {match dialog.command(){Ok(Some(command))=>intents.push(Intent::Command(command)),Ok(None)=>{},Err(error)=>intents.push(Intent::DialogError(error))}}
            }
            Some(1)=>keep=false,
            _=>{}
        }}
        });
        if !enabled { ui.horizontal(|ui| {ui.spinner(); ui.label(crate::i18n::tr(ui,"Preparing operation…"));}); }
    });
    if enabled && response.should_close() {
        keep = false;
    }
    if keep {
        *slot = Some(dialog);
    }else if let Dialog::SourceColorTag(draft)=&dialog{super::source_color_tags::clear(context,draft.token);}
}
const SETTINGS_PAGES: [(&str, &str); 9] = [
    ("generalPage", "General"),
    ("appearancePage", "Appearance"),
    ("streamPage", "Stream"),
    ("outputPage", "Output"),
    ("audioPage", "Audio"),
    ("videoPage", "Video"),
    ("hotkeyPage", "Hotkeys"),
    ("accessPage", "Accessibility"),
    ("advancedPage", "Advanced"),
];

fn settings_sidebar(ui:&mut egui::Ui,model:&StudioSnapshot,current:super::settings::SettingsPage,unchanged:bool,intents:&mut Vec<Intent>) {
    use super::settings::SettingsPage::*;
    for (id,label) in SETTINGS_PAGES {
        let page=match id {"generalPage"=>Some(General),"appearancePage"=>Some(Appearance),"streamPage"=>Some(Stream),"outputPage"=>Some(Output),
            "audioPage"=>Some(Audio),"videoPage"=>Some(Video),"hotkeyPage"=>Some(Hotkeys),"accessPage"=>Some(Accessibility),"advancedPage"=>Some(Advanced),_=>None};
        let selected=page==Some(current);
        let ready=page.is_some()&&(selected||unchanged);
        let response=ui.add_enabled_ui(ready,|ui|ui.add_sized([ui.available_width(),34.0],egui::Button::new(crate::i18n::tr(ui,label)).selected(selected).wrap())).inner
            .on_hover_text(crate::i18n::tr(ui,"Open this Settings page."))
            .on_disabled_hover_text(crate::i18n::tr(ui,if page.is_none(){"This Settings page is not implemented yet."}else{"Apply or Cancel this draft before changing Settings pages."}));
        if response.clicked()&&!selected {if let Some(page)=page {intents.push(Intent::OpenDialog(super::settings::fresh(page,model)));}}
    }
}

#[cfg(test)]
mod shared_modal_layout_tests {
    use super::*;
    #[test]
    fn long_property_content_scrolls_above_reachable_equal_footer_actions() {
        for viewport in [
            egui::vec2(360.0, 360.0),
            egui::vec2(800.0, 560.0),
            egui::vec2(1280.0, 800.0),
        ] {
            for kind in 0..3 {
                let context = egui::Context::default();
                crate::theme::install(&context);
                let session = skylive_core::Session::new();
                let before = session.snapshot();
                let dialog = match kind {
                    0 => Dialog::SceneNotes {
                        binding: None,
                        scene: session.preview(),
                        name: "Notes".into(),
                        text: "A long line of retained notes.\n".repeat(400),
                    },
                    1 => Dialog::Text(super::super::text_source::TextDraft::add(
                        session.preview(),
                        "Text".into(),
                    )),
                    _ => Dialog::Plugins {
                        query: String::new(),
                        session: 0,
                        token: 0,
                    },
                };
                let mut slot = Some(dialog);
                let mut intents = vec![];
                let mut output = egui::FullOutput::default();
                for _ in 0..3 {
                    output = context.run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                viewport,
                            )),
                            ..Default::default()
                        },
                        |ctx| show(ctx, &mut slot, &mut intents, true, &session.snapshot()),
                    );
                }
                let action = context.data(|data| {
                    data.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", 0)))
                        .unwrap()
                });
                assert!(
                    egui::Rect::from_min_size(egui::Pos2::ZERO, viewport).contains_rect(action),
                    "footer stays inside viewport {viewport:?}"
                );
                if kind != 2 {
                    let cancel = context.data(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", 1)))
                            .unwrap()
                    });
                    assert!(
                        egui::Rect::from_min_size(egui::Pos2::ZERO, viewport).contains_rect(cancel)
                    );
                    assert!((action.width() - cancel.width()).abs() < 0.1);
                    assert!((action.height() - cancel.height()).abs() < 0.1);
                }
                fn check(shape: &egui::epaint::Shape, clip: egui::Rect) {
                    match shape {
                        egui::epaint::Shape::Text(text)
                            if ["Apply", "Cancel", "Close"].contains(&text.galley.text()) =>
                        {
                            assert!(clip.contains_rect(egui::Rect::from_min_size(
                                text.pos,
                                text.galley.size()
                            )))
                        }
                        egui::epaint::Shape::Vec(shapes) => {
                            for shape in shapes {
                                check(shape, clip)
                            }
                        }
                        _ => {}
                    }
                }
                for clipped in &output.shapes {
                    check(&clipped.shape, clipped.clip_rect);
                }
                assert!(intents.is_empty());
                assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
            }
        }
    }
}
fn settings_navigation(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    audio: bool,
    unchanged: bool,
    intents: &mut Vec<Intent>,
) {
    settings_navigation_page(
        ui,
        model,
        if audio { "audioPage" } else { "videoPage" },
        unchanged,
        intents,
    );
}
fn settings_navigation_page(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    page: &str,
    unchanged: bool,
    intents: &mut Vec<Intent>,
) {
    if ui.data(|data|data.get_temp::<bool>(egui::Id::new("settings-sidebar-active"))).unwrap_or(false) {
        if !unchanged {ui.label(crate::i18n::tr(ui,"Apply or Cancel this draft before changing Settings pages."));ui.separator();}
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (id, label) in SETTINGS_PAGES {
            let implemented = matches!(
                id,
                "generalPage"
                    | "appearancePage"
                    | "audioPage"
                    | "videoPage"
                    | "outputPage"
                    | "streamPage"
                    | "hotkeyPage"
                    | "accessPage"
                    | "advancedPage"
            );
            let selected = page == id;
            // Advance the wrapping parent before creating a child. Scoped UI
            // allocation can otherwise keep the child's original row origin.
            let text: egui::WidgetText = crate::i18n::tr(ui, label).into();
            let galley = text.into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Button,
            );
            let size =
                (galley.size() + 2.0 * ui.spacing().button_padding).max(ui.spacing().interact_size);
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            let mut button_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("settings-page", id))
                    .max_rect(rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            let response = button_ui
                .add_enabled(
                    implemented && (selected || unchanged),
                    egui::Button::new(galley).selected(selected),
                )
                .on_hover_text(crate::hints::text(ui, "Item"))
                .on_disabled_hover_text(crate::hints::text(ui, "Item"));
            let response = response.on_disabled_hover_text(crate::i18n::tr(
                ui,
                if implemented {
                    "Apply or Cancel the current draft before changing Settings pages."
                } else {
                    "This Settings page is pending Rust implementation."
                },
            ));
            if response.clicked() && !selected {
                intents.push(Intent::OpenDialog(if id == "generalPage" {
                    Dialog::general(model)
                } else if id == "appearancePage" {
                    Dialog::Appearance(super::appearance::AppearanceDraft::new(
                        model.preferences().appearance(),
                    ))
                } else if id == "streamPage" {
                    Dialog::streaming(model)
                } else if id == "outputPage" {
                    Dialog::recording(model)
                } else if id == "audioPage" {
                    Dialog::audio_settings(&model.audio())
                } else if id == "hotkeyPage" {
                    Dialog::Hotkeys(super::hotkeys::HotkeyDraft::from_preferences(model.preferences()))
                } else if id == "advancedPage" {
                    Dialog::AdvancedProcess(super::advanced_process::Draft::new(model))
                } else if id == "accessPage" {
                    Dialog::Accessibility(super::accessibility::Draft::new(model.preferences().accessibility()))
                } else {
                    Dialog::video_profile(model)
                }));
            }
        }
    });
    if !unchanged {
        ui.label(crate::i18n::tr(
            ui,
            "Apply or Cancel this draft before changing Settings pages.",
        ));
    }
    ui.separator();
}
fn audio_rate_label(rate: u32) -> String {
    match rate {
        44100 => "44.1 kHz".into(),
        48000 => "48 kHz".into(),
        other => format!("Unsupported rate: {other} Hz"),
    }
}
pub(super) fn audio_target_label(choice: AudioTargetChoice) -> &'static str {
    match choice {
        AudioTargetChoice::Default => "Default device",
        AudioTargetChoice::WindowsProcess => "Application process",
        AudioTargetChoice::WindowsEndpoint => "Windows endpoint",
        AudioTargetChoice::MacDevice => "macOS device",
        AudioTargetChoice::MacSystem => "macOS system audio",
    }
}
fn transition_fields(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    reference: &mut TransitionRef,
    duration_ms: &mut u32,
) {
    transition_reference_editor(ui, "quick-reference", model, reference);
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr(ui, "Duration (ms)"));
        ui.add_enabled(
            *reference != TransitionRef::Cut,
            egui::DragValue::new(duration_ms).speed(1),
        )
        .on_hover_text(crate::hints::text(ui, "Duration (ms)"))
        .on_disabled_hover_text(crate::hints::text(ui, "Duration (ms)"));
    });
    ui.label(crate::i18n::tr(
        ui,
        "Valid duration: 50–20000 ms. Apply validates all fields as one edit.",
    ));
}
fn image_path_editor(ui: &mut egui::Ui, path: &mut String) {
    ui.label(crate::i18n::tr(ui, "Image file"));
    ui.add(
        egui::TextEdit::singleline(path)
            .desired_width(440.0)
            .char_limit(4096)
            .id_salt("image-file"),
    )
    .on_hover_text(crate::hints::text(ui, "image-file"))
    .on_disabled_hover_text(crate::hints::text(ui, "image-file"));
    ui.label(crate::i18n::tr_key_or(ui,"SkyLive.ImageSource.AnimationFormats","PNG/APNG, JPEG, BMP, TGA, GIF and WebP. Animated GIF, APNG and WebP are supported. Reading and decoding happen in the background; rejected files leave this draft unchanged."));
}
#[cfg(test)]
mod settings_layout_tests {
    use super::*;

    #[test]
    fn navigation_wraps_complete_buttons_at_default_and_narrow_widths() {
        fn text_shapes<'a>(
            shape: &'a egui::epaint::Shape,
            found: &mut Vec<&'a egui::epaint::TextShape>,
        ) {
            match shape {
                egui::epaint::Shape::Text(text) => found.push(text),
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text_shapes(shape, found);
                    }
                }
                _ => {}
            }
        }
        for width in [480.0, 280.0] {
            for audio in [false, true] {
                let context = egui::Context::default();
                let session = skylive_core::Session::new();
                let original = session.snapshot();
                let mut intents = Vec::new();
                let mut left = 0.0;
                let mut height = 0.0;
                let mut output = egui::FullOutput::default();
                for _ in 0..2 {
                    output = context.run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(800.0, 600.0),
                            )),
                            ..Default::default()
                        },
                        |context| {
                            egui::CentralPanel::default().show(context, |ui| {
                                // CentralPanel already owns the viewport's
                                // minimum width; set_width cannot shrink it.
                                // Give the navigation a genuinely narrow rect.
                                let rect = egui::Rect::from_min_size(
                                    ui.cursor().min,
                                    egui::vec2(width, 500.0),
                                );
                                let mut navigation = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(rect)
                                        .layout(egui::Layout::top_down(egui::Align::Min)),
                                );
                                left = rect.left();
                                let top = rect.top();
                                settings_navigation(
                                    &mut navigation,
                                    &original,
                                    audio,
                                    true,
                                    &mut intents,
                                );
                                height = navigation.cursor().top() - top;
                            });
                        },
                    );
                }
                let mut painted = Vec::new();
                for clipped in &output.shapes {
                    text_shapes(&clipped.shape, &mut painted);
                }
                for (_, label) in SETTINGS_PAGES {
                    let text = painted
                        .iter()
                        .find(|text| text.galley.text() == label)
                        .unwrap_or_else(|| panic!("{label} remains visible at {width}px"));
                    assert_eq!(
                        text.galley.rows.len(),
                        1,
                        "{label} must never wrap inside its button at {width}px"
                    );
                    assert!(
                        text.pos.x >= left - 1.0
                            && text.pos.x + text.galley.size().x <= left + width + 1.0,
                        "{label} stays within {width}px: x={}, text_width={}, left={left}",
                        text.pos.x,
                        text.galley.size().x
                    );
                }
                assert!(
                    height <= 128.0,
                    "navigation must stay compact at {width}px, got {height}px"
                );
                assert!(
                    intents.is_empty(),
                    "layout alone cannot change Settings pages"
                );
                assert!(std::sync::Arc::ptr_eq(&original, &session.snapshot()));
            }
        }
    }
}
