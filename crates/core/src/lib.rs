//! Headless studio scopes and single-owner command engine (guide A1–A8).
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashSet, VecDeque},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

pub const MAX_SCENES: usize = 256;
pub const MAX_SOURCES: usize = 1024;
pub const MAX_ITEMS: usize = 4096;
pub const HISTORY_LIMIT: usize = 32;
pub const MAX_SCENE_DEPTH: usize = 16;
static SESSION_SERIAL: AtomicU64 = AtomicU64::new(1);

mod virtual_camera_route;
pub use virtual_camera_route::VirtualCameraRoute;
mod output_video;
pub use output_video::{OutputVideoSettings,OutputResolution};
mod item_compositing;
mod source_item_color;
pub use source_item_color::{SourceItemColorTag,SourceItemColorPreset};
pub use item_compositing::{BlendMode,BlendingMethod,ScaleFiltering,ItemCompositing};
mod geometry;
mod group;
mod group_tree;
pub use group_tree::{GroupTree,GroupTreeEntry,GroupTreeItem};
mod hotkeys;
mod source_playback_hotkeys;
mod source_audio_hotkeys;
mod source_shortcuts;
pub use source_audio_hotkeys::{SourceAudioAction,SourceAudioBinding,SourceAudioHotkeys};
pub use source_shortcuts::MAX_SOURCE_HOTKEYS;
pub use source_playback_hotkeys::{SourcePlaybackAction,SourcePlaybackBinding,SourcePlaybackHotkeys,MAX_SOURCE_PLAYBACK_HOTKEYS};
pub use hotkeys::{HotkeyAction,HotkeyBinding,HotkeyBindings,HotkeyChord,GlobalHotkeyBindings,MAX_GLOBAL_HOTKEYS,global_key_supported,validate_hotkey_scopes};
#[cfg(test)]
mod scene_order_tests;
mod scene_source;
mod scene_canvas;
use scene_canvas::{CanvasBasis,ScaleAnchor};
pub use geometry::{Crop, ItemBounds, ItemGeometry, Transform};
pub use group::GroupSource;
mod font_asset;
pub use font_asset::{FontAsset,FontFace,FontFaceStyle,MAX_FONT_FILE_BYTES,MAX_FONT_FACES};
mod text_source;
mod text_case;
pub use text_case::TextCaseTransform;
pub use text_source::{
    TextFile,
    MAX_TEXT_BYTES, MAX_TEXT_CHARS, TextAlignment, TextGradient, TextOutline, TextSource, TextStyle, TextVerticalAlignment,
};
mod slideshow_entries;
mod image_source;
pub use image_source::{ImageSourceSettings,ImageSourceCommand};
pub use slideshow_entries::{SlideShowEntry,SlideShowEntryKind};
mod image_slideshow;
pub use image_slideshow::{ImageSlideShow,SlideTransition,SlideShowCanvas,SlideShowBehavior,SlideShowMode,SlideShowRandomCompatibility,MAX_SLIDES};
mod lut_asset;
pub use lut_asset::{LutAsset,LutFormat};
mod resource_relink;
pub use resource_relink::{ResourceSlot,ResourceValue,ResourceRelink,ResourceRelinks,MAX_RESOURCE_RELINKS,MAX_RELINK_METADATA};
mod image_asset;
pub use image_asset::ImageAsset;
mod display_capture;
pub use display_capture::{
    DisplayCaptureBinding, DisplayCaptureMethod, DisplayTarget, MAX_DISPLAY_PIXELS,
};
mod window_capture;
pub use window_capture::{
    WindowCaptureBinding, WindowCaptureMethod, WindowMatchPriority, WindowTarget,
};
mod game_capture;
pub use game_capture::{GameCaptureBinding,GameCaptureTarget};
mod ndi_source;
mod browser_source;
pub use browser_source::{BrowserSource,BrowserLocation,BROWSER_PLUGIN};
pub use ndi_source::{NdiSource,NdiConnection,NdiSender,NdiBandwidth,NDI_PLUGIN};
mod media_source;
mod video_capture;
pub use media_source::MediaSource;
pub use video_capture::{
    CameraFpsPolicy, CameraModePolicy, VideoCaptureBinding, VideoCaptureMode, VideoCaptureTarget, VideoColorMatrix, VideoColorRange,
    VideoPixelFormat,
};
mod audio_capture;
mod mixer_state;
mod projectors;
mod preview_snapping;
pub use preview_snapping::PreviewSnapping;
mod projector_windows;
pub use projector_windows::{SavedProjectorTarget,SavedProjectorOptions,ProjectorWindowGeometry,SavedProjectorWindow,SavedProjectorWindows,MAX_SAVED_PROJECTORS};
pub use projectors::{ProjectorPreferences,MultiviewLayout};
mod audio_monitoring;
pub use audio_monitoring::{AudioMonitoringMode,AudioMonitoringDevice};
mod source_audio;
mod audio_tracks;
pub use audio_tracks::SourceAudioTrackMask;
#[cfg(test)]
mod source_audio_tests;
mod source_filter_paste;
mod source_paste;
pub use source_paste::SourcePasteItem;
pub use audio_capture::{
    AudioCaptureBinding, AudioFlow, AudioSlot, AudioTarget, GlobalAudioSource, WindowsProcessIdentity, WindowsAudioWindowSelector, WindowsAudioWindowPriority,
};
pub use mixer_state::{MixerDisplayPreferences, MixerLayout, MixerPeakMode, MixerSourceAction, MixerSourceState};
pub use source_audio::{SourceAudioProperties, PushAudioSettings, MIN_AUDIO_SYNC_OFFSET_MS, MAX_AUDIO_SYNC_OFFSET_MS};
mod source_creation;
pub use source_creation::{SourceCreationKind,SourceCreationPlatform,SourceCreation,plugin_source_creations};
mod plugin_entry_points;
pub use plugin_entry_points::{PluginEntryPoint, PluginOperation, plugin_entry_points};
mod filter_catalog;
pub use filter_catalog::{AudioFilterFactory,VideoFilterFactory,FilterFactory,plugin_filter_factories};
mod plugins;
mod audio_sidechain;
pub use plugins::{
    AUDIO_PAN_PLUGIN, AudioFilter, CRAFT_AUDIO_PLUGIN, CompressorDetector, MAX_AUDIO_FILTERS,
    MAX_PLUGINS, MAX_SCENE_NOTES_BYTES, ObsExpanderDetector, ObsExpanderPreset, PluginDescriptor,
    PluginOrigin, SCENE_NOTES_PLUGIN, plugin_catalog, plugin_descriptor,
};
mod appearance;
pub use appearance::{Appearance, InterfaceTheme};
mod accessibility;
pub use accessibility::{Accessibility,ColorPreset,InterfaceColor};
mod profiles;
mod replay_buffer;
pub use replay_buffer::ReplayBufferSettings;
mod process_priority;
pub use process_priority::ProcessPriority;
mod reconnect;
pub use reconnect::ReconnectSettings;
mod recording;
mod ndi_output;
pub use ndi_output::{NdiOutputSettings,NdiOutputRoute};
mod video_encoder;
pub use video_encoder::VideoEncoder;
mod setup;
pub use setup::{SetupCommand, SetupConfiguration, SetupDefaultAudio, SetupState};
mod streaming;
pub use profiles::{
    MAX_PROFILES, Profile, ProfileCatalog, ProfileCommand, ProfileId, UserPreferences,
};
mod recording_tracks;
pub use recording_tracks::{AudioTrackId,RecordingTrack,RecordingTracks};
pub use recording::{RecordingContainer,RecordingSettings};
pub use streaming::{StreamingEndpoint, StreamingSettings};
pub mod stream_service_presets;
mod audio_settings;
pub use audio_settings::{AudioBuffering, AudioSettings, SpeakerLayout};
mod scale_filter;
pub use scale_filter::{ScaleFilterTarget,ScaleFilterSampling};
mod luma_wipe;
pub use luma_wipe::{LumaWipePattern,LumaWipeSoftness};
mod transition;
pub use transition::{
    MAX_TRANSITION_DURATION_MS, MIN_TRANSITION_DURATION_MS, TransitionConfig, TransitionDirection,
    TransitionKind,
};
mod quick_transition;
pub use quick_transition::{MAX_QUICK_TRANSITIONS, QuickTransition, QuickTransitionId};
mod transition_registry;
pub use transition_registry::{
    MAX_TRANSITION_PRESETS, TransitionPreset, TransitionPresetId, TransitionRef,
    TransitionSelection,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(u64);
impl Id {
    pub fn get(self) -> u64 {
        self.0
    }
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceKind {
    Group {
        group: GroupSource,
    },
    /// Shared scene reference; native dimensions require StudioSnapshot context.
    Scene {
        scene: Id,
    },
    Color {
        rgba: [u8; 4],
        width: u32,
        height: u32,
    },
    Image {
        asset: ImageAsset,
    },
    ImageSlideShow { settings:ImageSlideShow },
    Text {
        settings: TextSource,
    },
    DisplayCapture {
        binding: DisplayCaptureBinding,
    },
    WindowCapture {
        binding: WindowCaptureBinding,
    },
    VideoCapture {
        binding: VideoCaptureBinding,
    },
    MediaSource {
        media: MediaSource,
    },
    GameCapture {binding:GameCaptureBinding},
    NdiSource {ndi:NdiSource},
    Browser {browser:BrowserSource},
    AudioCapture {
        binding: AudioCaptureBinding,
    },
}
impl SourceKind {
    /// Authored native endpoint shared by audio-only and camera sources.
    pub fn audio_capture_binding(&self) -> Option<&AudioCaptureBinding> {
        match self {
            Self::AudioCapture {binding} => Some(binding),
            Self::VideoCapture {binding} => binding.audio(),
            Self::WindowCapture {binding} => binding.audio(),
            _ => None,
        }
    }
    /// Capability to retain authored controls while the optional input is off.
    /// Runtime routing must continue to use has_audio, not this predicate.
    pub fn supports_audio_settings(&self) -> bool {
        self.has_audio() || matches!(self, Self::VideoCapture {binding}
            if matches!(binding.target(), VideoCaptureTarget::Windows {..}))
            || matches!(self, Self::WindowCapture {binding}
                if matches!(binding.target(), WindowTarget::Windows {..}))
    }
    pub fn has_audio(&self) -> bool {
        self.audio_capture_binding().is_some()
            || matches!(self,Self::VideoCapture {binding} if binding.embedded_audio())
            || matches!(self,Self::MediaSource{media}if media.has_audio())
            || matches!(self,Self::NdiSource {ndi} if ndi.audio())
            || matches!(self,Self::Browser {browser} if browser.control_audio())
    }
    /// Shared Scene or private Group child container; roots remain separately selectable.
    pub fn nested_scene(&self) -> Option<Id> {
        match self {
            Self::Scene { scene } => Some(*scene),
            Self::Group { group } => Some(group.scene()),
            _ => None,
        }
    }
    /// Legacy nonvideo sentinel: audio returns (0,0), never a fabricated pixel.
    /// New consumers should use video_dimensions() to distinguish video sources.
    pub fn dimensions(&self) -> (u32, u32) {
        self.video_dimensions().unwrap_or((0, 0))
    }
    /// Context-free visual dimensions. Scene references require
    /// StudioSnapshot::source_video_dimensions; audio-only sources return None.
    /// Display Capture returns its basis; actual frames are derived host data.
    pub fn video_dimensions(&self) -> Option<(u32, u32)> {
        match self {
            Self::Group { group } => Some(group.basis()),
            Self::Scene { .. } => None,
            Self::Color { width, height, .. } => Some((*width, *height)),
            Self::Image { asset } => Some((asset.width(), asset.height())),
            Self::ImageSlideShow {settings} => Some(settings.dimensions()),
            Self::Text { settings } => Some((settings.width(), settings.height())),
            Self::DisplayCapture { binding } => Some((binding.width(), binding.height())),
            Self::WindowCapture { binding } => Some((binding.width(), binding.height())),
            Self::VideoCapture { binding } => Some((binding.width(), binding.height())),
            Self::MediaSource { media } => media.video_dimensions(),
            Self::GameCapture {binding}=>Some((binding.width(),binding.height())),
            Self::NdiSource {ndi}=>Some((ndi.width(),ndi.height())),
            Self::Browser {browser}=>Some((browser.width(),browser.height())),
            Self::AudioCapture { .. } => None,
        }
    }
    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Group { group } => group.validate(),
            Self::Scene { scene } if scene.get() == 0 => {
                Err(Error::Invalid("invalid scene source target"))
            }
            Self::Scene { .. } => Ok(()),
            Self::Color { width, height, .. } => {
                if *width == 0 || *height == 0 || *width > 8192 || *height > 8192 {
                    Err(Error::Invalid("invalid source dimensions"))
                } else {
                    Ok(())
                }
            }
            Self::Image { asset } => asset.validate(),
            Self::ImageSlideShow {settings} => settings.validate(),
            Self::Text { settings } => settings.validate(),
            Self::DisplayCapture { binding } => binding.validate(),
            Self::WindowCapture { binding } => binding.validate(),
            Self::VideoCapture { binding } => binding.validate(),
            Self::MediaSource { media } => media.validate(),
            Self::GameCapture {binding}=>binding.validate(),
            Self::NdiSource {ndi}=>ndi.validate(),
            Self::Browser {browser}=>browser.validate(),
            Self::AudioCapture { binding } => binding.validate(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    #[serde(default,skip_serializing_if="Option::is_none")]
    image_settings:Option<ImageSourceSettings>,
    #[serde(default,skip_serializing_if="SourcePlaybackHotkeys::is_empty")]
    playback_hotkeys:SourcePlaybackHotkeys,
    #[serde(default,skip_serializing_if="SourceAudioHotkeys::is_empty")]
    audio_hotkeys:SourceAudioHotkeys,
    #[serde(default,skip_serializing_if="AudioMonitoringMode::is_off")]
    audio_monitoring:AudioMonitoringMode,
    #[serde(default, skip_serializing_if = "MixerSourceState::is_default")]
    mixer: MixerSourceState,
    #[serde(default, skip_serializing_if = "SourceAudioProperties::is_default")]
    audio_properties: SourceAudioProperties,
    id: Id,
    name: String,
    kind: SourceKind,
    gain: f32,
    muted: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    audio_filters: Vec<AudioFilter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    video_filters: Vec<VideoFilter>,
}
impl Source {
    pub fn playback_hotkeys(&self)->&SourcePlaybackHotkeys{&self.playback_hotkeys}
    pub fn audio_hotkeys(&self)->&SourceAudioHotkeys{&self.audio_hotkeys}
    pub fn audio_monitoring(&self)->AudioMonitoringMode {self.audio_monitoring}
    pub fn mixer_state(&self) -> MixerSourceState {
        self.mixer
    }
    pub fn audio_properties(&self) -> &SourceAudioProperties {
        &self.audio_properties
    }
    pub fn video_filters(&self) -> &[VideoFilter] {
        &self.video_filters
    }
    pub fn audio_filters(&self) -> &[AudioFilter] {
        &self.audio_filters
    }
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn kind(&self) -> &SourceKind {
        &self.kind
    }
    pub fn gain(&self) -> f32 {
        self.gain
    }
    pub fn muted(&self) -> bool {
        self.muted
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneItem {
    #[serde(default,skip_serializing_if="Option::is_none")]
    scale_anchor:Option<ScaleAnchor>,
    #[serde(default,skip_serializing_if="SourceItemColorTag::is_clear")]
    color_tag:SourceItemColorTag,
    #[serde(default,skip_serializing_if="ItemCompositing::is_default")]
    compositing: ItemCompositing,
    id: Id,
    source: Id,
    visible: bool,
    locked: bool,
    transform: Transform,
}
impl SceneItem {
    pub fn color_tag(&self)->SourceItemColorTag{self.color_tag}
    pub fn compositing(&self)->ItemCompositing {self.compositing}
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn source(&self) -> Id {
        self.source
    }
    pub fn visible(&self) -> bool {
        self.visible
    }
    pub fn locked(&self) -> bool {
        self.locked
    }
    pub fn transform(&self) -> &Transform {
        &self.transform
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    #[serde(default="default_show_in_multiview",skip_serializing_if="is_show_in_multiview")]
    show_in_multiview:bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    notes: String,
    id: Id,
    name: String,
    items: Vec<SceneItem>,
    #[serde(default)]
    transition_override: Option<TransitionSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inactive_transition_duration_ms: Option<u32>,
}
fn default_show_in_multiview()->bool{true}
fn is_show_in_multiview(value:&bool)->bool{*value}
impl Scene {
    pub fn show_in_multiview(&self)->bool{self.show_in_multiview}
    pub fn notes(&self) -> &str {
        &self.notes
    }
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Back to front: the last item is visually on top.
    pub fn items(&self) -> &[SceneItem] {
        &self.items
    }
    pub fn transition_override(&self) -> Option<&TransitionSelection> {
        self.transition_override.as_ref()
    }
    /// OBS retains the per-scene duration even while its override is disabled.
    /// Exactly one of the active config or inactive metadata owns this value.
    pub fn transition_duration_ms(&self) -> u32 {
        self.transition_override
            .as_ref()
            .map(TransitionSelection::duration_ms)
            .unwrap_or_else(|| self.inactive_transition_duration_ms.unwrap_or(300))
    }
    fn configure_transition(
        &mut self,
        reference: Option<TransitionRef>,
        duration_ms: u32,
    ) -> Result<(), Error> {
        let config =
            TransitionSelection::new(reference.unwrap_or(TransitionRef::Fade), duration_ms)?;
        if self
            .transition_override
            .as_ref()
            .map(TransitionSelection::reference)
            == reference
            && self.transition_duration_ms() == duration_ms
        {
            return Ok(());
        }
        self.transition_override = reference.map(|_| config);
        self.inactive_transition_duration_ms =
            (reference.is_none() && duration_ms != 300).then_some(duration_ms);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoSettings {
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
}
impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps_num: 30,
            fps_den: 1,
        }
    }
}
impl VideoSettings {
    pub fn new(width: u32, height: u32, fps_num: u32, fps_den: u32) -> Result<Self, Error> {
        let value = Self {
            width,
            height,
            fps_num,
            fps_den,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn fps_num(&self) -> u32 {
        self.fps_num
    }
    pub fn fps_den(&self) -> u32 {
        self.fps_den
    }
    fn validate(&self) -> Result<(), Error> {
        if !(16..=4096).contains(&self.width)
            || !(16..=4096).contains(&self.height)
            || u64::from(self.width) * u64::from(self.height) > 8_847_360
            || self.fps_num == 0
            || self.fps_den == 0
            || self.fps_den > 100_000
            || u64::from(self.fps_num) > 240 * u64::from(self.fps_den)
        {
            Err(Error::Invalid("unsupported video dimensions or frame rate"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudioSnapshot {
    revision: u64,
    collection: Collection,
    profiles: ProfileCatalog,
    preferences: UserPreferences,
}
impl std::ops::Deref for StudioSnapshot {
    type Target = Collection;
    fn deref(&self) -> &Collection {
        &self.collection
    }
}
impl std::ops::DerefMut for StudioSnapshot {
    fn deref_mut(&mut self) -> &mut Collection {
        &mut self.collection
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collection {
    #[serde(default,skip_serializing_if="Option::is_none")]
    scene_canvas_basis:Option<CanvasBasis>,
    #[serde(default,skip_serializing_if="SavedProjectorWindows::is_empty")]
    saved_projectors:SavedProjectorWindows,
    name: String,
    next_id: u64,
    #[serde(default)]
    transition: TransitionSelection,
    #[serde(default)]
    transition_presets: Vec<TransitionPreset>,
    #[serde(default = "transition_registry::next_default")]
    next_transition_preset_id: u32,
    #[serde(default = "quick_transition::defaults")]
    quick_transitions: Vec<QuickTransition>,
    #[serde(default = "quick_transition::next_default")]
    next_quick_id: u32,
    scenes: Vec<Scene>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    groups: Vec<Scene>,
    sources: Vec<Source>,
    #[serde(default)]
    global_audio: Vec<GlobalAudioSource>,
}
impl Collection{pub fn saved_projectors(&self)->&SavedProjectorWindows{&self.saved_projectors}}
impl Default for Collection {
    fn default() -> Self {
        Self {
            scene_canvas_basis:None,
            saved_projectors:SavedProjectorWindows::default(),
            name: "Untitled".into(),
            next_id: 2,
            transition: TransitionSelection::default(),
            transition_presets: Vec::new(),
            next_transition_preset_id: 1,
            quick_transitions: quick_transition::defaults(),
            next_quick_id: quick_transition::next_default(),
            scenes: vec![Scene {
                show_in_multiview:true,
                notes: String::new(),
                id: Id(1),
                name: "Scene".into(),
                items: vec![],
                transition_override: None,
                inactive_transition_duration_ms: None,
            }],
            sources: vec![],
            groups: vec![],
            global_audio: vec![],
        }
    }
}
impl StudioSnapshot {
    pub fn from_parts(
        revision: u64,
        mut collection: Collection,
        profiles: ProfileCatalog,
        preferences: UserPreferences,
    ) -> Result<Self, Error> {
        collection.migrate_video_filters()?;
        let mut value = Self {
            revision,
            collection,
            profiles,
            preferences,
        };
        value.materialize_scene_canvas()?;
        value.validate()?;
        Ok(value)
    }
    pub fn collection(&self) -> &Collection {
        &self.collection
    }
    pub fn profiles(&self) -> &ProfileCatalog {
        &self.profiles
    }
    pub fn preferences(&self) -> &UserPreferences {
        &self.preferences
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn video(&self) -> &VideoSettings {
        self.profiles.selected().video()
    }
    pub fn audio(&self) -> AudioSettings {
        AudioSettings::effective(
            self.profiles.selected().sample_rate(),
            self.profiles.selected().speakers(),
            self.preferences.audio_buffering(),
        )
    }
    /// Validated presentation-only scene layout; never publishes a Session revision/history.
    pub fn scene_with_item_transform_preview(
        &self,
        scene: Id,
        item: Id,
        transform: Transform,
    ) -> Result<Scene, Error> {
        let mut layout = self.scene(scene).ok_or(Error::NotFound(scene))?.clone();
        let selected = layout
            .items
            .iter_mut()
            .find(|value| value.id == item)
            .ok_or(Error::NotFound(item))?;
        if selected.locked {
            return Err(Error::Locked(item));
        }
        let source = self
            .source(selected.source)
            .ok_or(Error::NotFound(selected.source))?;
        self.source_item_geometry(source,&transform)?;
        if selected.transform.width!=transform.width||selected.transform.height!=transform.height {
            selected.scale_anchor=self.scale_anchor_for(source,&transform)?;
        }
        selected.transform = transform;
        Ok(layout)
    }
    /// Produce a validated presentation-only layout for one atomic group drag.
    pub fn scene_with_item_transforms_preview(&self, scene: Id, transforms: &[(Id, Transform)]) -> Result<Scene, Error> {
        let mut layout = self.scene(scene).ok_or(Error::NotFound(scene))?.clone();
        if transforms.is_empty() || transforms.len() > MAX_SOURCES { return Err(Error::Invalid("Invalid group transform count")); }
        let mut seen = std::collections::HashSet::new();
        for (item, transform) in transforms {
            if !seen.insert(*item) { return Err(Error::Invalid("Duplicate group transform item")); }
            let selected = layout.items.iter_mut().find(|value| value.id == *item).ok_or(Error::NotFound(*item))?;
            if selected.locked { return Err(Error::Locked(*item)); }
            let source = self.source(selected.source).ok_or(Error::NotFound(selected.source))?;
            self.source_item_geometry(source, transform)?;
            if selected.transform.width != transform.width || selected.transform.height != transform.height {
                selected.scale_anchor = self.scale_anchor_for(source, transform)?;
            }
            selected.transform = transform.clone();
        }
        Ok(layout)
    }
    /// Build the same normalized hierarchy as an atomic group move, without
    /// publishing it into the session or recording history.
    pub fn snapshot_with_item_transforms_preview(&self, container:Id, transforms:&[(Id,Transform)])->Result<Self,Error> {
        if transforms.is_empty() || transforms.len()>MAX_SOURCES {return Err(Error::Invalid("Invalid group transform count"));}
        let mut candidate=self.clone();
        candidate.ensure_container_unlocked(container)?;
        let mut seen=std::collections::HashSet::new();
        for (id,transform) in transforms {
            if !seen.insert(*id) {return Err(Error::Invalid("Duplicate group transform item"));}
            let original=candidate.item_mut(container,*id)?.clone();
            if original.locked {return Err(Error::Locked(*id));}
            let source=candidate.source(original.source).ok_or(Error::NotFound(original.source))?;
            if candidate.source_video_dimensions(&source.kind)?.is_none() {return Err(Error::Invalid("audio-only sources do not support video geometry commands"));}
            candidate.source_item_geometry(source,transform)?;
            candidate.item_mut(container,*id)?.transform=transform.clone();
        }
        candidate.prepare_scale_anchors(Some(self))?;
        let canvas_changed=candidate.rebase_scene_canvas()?;
        candidate.normalize_groups(if canvas_changed{None}else{Some(self)})?;
        candidate.validate()?;
        Ok(candidate)
    }
    /// Build a derived scene projection for one resolved automatic text frame.
    /// This never publishes a command or changes authored settings. The caller
    /// must retain and revalidate the worker's frame/revision guard while the
    /// projection is used, and must pair the projection with that same raster.
    pub fn snapshot_with_auto_text_geometry(&self,source:Id,expected:&TextSource,dimensions:(u32,u32))->Result<Self,Error>{
        self.snapshot_with_auto_text_geometries(&[(source,expected,dimensions)])
    }
    /// Project all automatic text bases together, then normalize the group
    /// hierarchy once. All references are verified before any derived state
    /// is returned; this is not an authored edit.
    pub fn snapshot_with_auto_text_geometries(&self,frames:&[(Id,&TextSource,(u32,u32))])->Result<Self,Error>{
        if frames.is_empty(){return Ok(self.clone());}
        if frames.len()>MAX_SOURCES{return Err(Error::Limit("Too many automatic text geometries"));}
        let mut rows=std::collections::HashMap::new();
        let mut candidate=self.clone();
        for (source,expected,dimensions) in frames {
            let original=self.source(*source).ok_or(Error::NotFound(*source))?;
            let SourceKind::Text{settings}=&original.kind else{return Err(Error::Invalid("Automatic text geometry belongs to a text source"));};
            if settings!=*expected{return Err(Error::Invalid("Automatic text geometry is stale"));}
            if rows.insert(*source,((settings.width(),settings.height()),*dimensions)).is_some(){return Err(Error::Invalid("Duplicate automatic text geometry"));}
            let derived=settings.clone().with_derived_auto_extent(dimensions.0,dimensions.1)?;
            candidate.source_mut(*source)?.kind=SourceKind::Text{settings:derived};
        }
        // Item extents encode scale against the authored source basis. A
        // dynamic intrinsic extent must preserve that scale, not squeeze
        // every newly measured text raster into the authored placeholder box.
        for scene in candidate.collection.scenes.iter_mut().chain(&mut candidate.collection.groups){for item in &mut scene.items{
            let Some((old,dimensions))=rows.get(&item.source) else{continue;};
            let width=f64::from(item.transform.width)*f64::from(dimensions.0)/f64::from(old.0);
            let height=f64::from(item.transform.height)*f64::from(dimensions.1)/f64::from(old.1);
            if !width.is_finite()||!height.is_finite()||width>f64::from(f32::MAX)||height>f64::from(f32::MAX){return Err(Error::Limit("Automatic text item geometry overflow"));}
            item.transform.width=width as f32;item.transform.height=height as f32;
        }}
        candidate.prepare_scale_anchors(Some(self))?;
        let canvas_changed=candidate.rebase_scene_canvas()?;
        candidate.normalize_groups(if canvas_changed{None}else{Some(self)})?;
        candidate.validate()?;
        Ok(candidate)
    }
    pub fn validate(&self) -> Result<(), Error> {
        self.profiles.validate()?;
        self.preferences.validate()?;
        self.collection.validate()?;
        self.validate_current_scene_canvas()?;
        self.validate_source_playback_hotkeys()?;
        for source in self.sources() {
            self.source_filtered_dimensions(source)?;
            if let SourceKind::Group { group } = source.kind() {
                if self
                    .group_native_dimensions(group.scene())?
                    .is_some_and(|dimensions| dimensions != group.basis())
                {
                    return Err(Error::Invalid(
                        "group scale basis does not match its nonempty bounds",
                    ));
                }
            }
        }
        for scene in self.scenes().iter().chain(self.groups()) {
            for item in scene.items() {
                let source = self
                    .source(item.source())
                    .ok_or(Error::NotFound(item.source()))?;
                if self.source_video_dimensions(source.kind())?.is_some() {
                    self.source_item_geometry(source,item.transform())?;
                }
            }
        }
        Ok(())
    }
}
impl Collection {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn transition(&self) -> &TransitionSelection {
        &self.transition
    }
    pub fn transition_presets(&self) -> &[TransitionPreset] {
        &self.transition_presets
    }
    pub fn transition_preset(&self, id: TransitionPresetId) -> Option<&TransitionPreset> {
        self.transition_presets
            .iter()
            .find(|preset| preset.id() == id)
    }
    /// OBS removes the selected configured instance, then selects the last
    /// remaining configured instance, or its last inherent built-in (Fade).
    /// This projection is shared with staged UI confirmation; no state changes.
    pub fn transition_after_removing_preset(
        &self,
        id: TransitionPresetId,
    ) -> Result<TransitionRef, Error> {
        self.transition_preset(id)
            .ok_or(Error::TransitionPresetNotFound(id))?;
        Ok(self
            .transition_presets
            .iter()
            .rev()
            .find(|preset| preset.id() != id)
            .map(|preset| TransitionRef::Preset(preset.id()))
            .unwrap_or(TransitionRef::Fade))
    }
    pub fn resolve_transition(
        &self,
        selection: &TransitionSelection,
    ) -> Result<TransitionConfig, Error> {
        let kind = match selection.reference() {
            TransitionRef::Cut => TransitionKind::Cut,
            TransitionRef::Fade => TransitionKind::Fade,
            TransitionRef::Preset(id) => self
                .transition_preset(id)
                .ok_or(Error::TransitionPresetNotFound(id))?
                .kind(),
        };
        TransitionConfig::try_new(kind, selection.duration_ms())
    }
    pub fn quick_transitions(&self) -> &[QuickTransition] {
        &self.quick_transitions
    }
    pub fn scenes(&self) -> &[Scene] {
        &self.scenes
    }
    pub fn sources(&self) -> &[Source] {
        &self.sources
    }
    pub fn global_audio(&self) -> &[GlobalAudioSource] {
        &self.global_audio
    }
    pub fn scene(&self, id: Id) -> Option<&Scene> {
        self.scenes.iter().find(|v| v.id == id)
    }
    pub fn source(&self, id: Id) -> Option<&Source> {
        self.sources.iter().find(|v| v.id == id)
    }
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_with_legacy_filters(false)
    }
    pub(crate) fn validate_with_legacy_filters(&self, legacy: bool) -> Result<(), Error> {
        validate_name(&self.name)?;
        self.validate_scene_canvas_metadata()?;
        for window in self.saved_projectors.entries(){let target=match window.target(){SavedProjectorTarget::Source{source}=>Some(source),SavedProjectorTarget::Scene{scene}=>Some(scene),_=>None};if target.is_some_and(|id|id.get()>=self.next_id){return Err(Error::Invalid("saved projector target identity has not been issued by this collection"));}}
        self.transition.validate()?;
        transition_registry::validate(&self.transition_presets, self.next_transition_preset_id)?;
        self.resolve_transition(&self.transition)?;
        quick_transition::validate(&self.quick_transitions, self.next_quick_id)?;
        for row in &self.quick_transitions {
            self.resolve_transition(row.config())?;
        }
        if self.scenes.is_empty()
            || self.scenes.len() > MAX_SCENES
            || self.sources.len() > MAX_SOURCES
            || self.groups.len() > MAX_SOURCES
            || self
                .scenes
                .iter()
                .chain(&self.groups)
                .map(|s| s.items.len())
                .sum::<usize>()
                > MAX_ITEMS
        {
            return Err(Error::Limit("collection entity budget"));
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        let mut check_id = |id: Id| -> Result<(), Error> {
            if id.0 == 0 || id.0 >= self.next_id || !ids.insert(id) {
                return Err(Error::Invalid("invalid, duplicate or unissued identity"));
            }
            Ok(())
        };
        for source in &self.sources {
            check_id(source.id)?;
            validate_name(&source.name)?;
            source.kind.validate()?;
            source.playback_hotkeys.validate_kind(&source.kind)?;
            source.audio_hotkeys.validate_kind(&source.kind)?;
            source.mixer.validate()?;
            if source.has_image_source_metadata()&&!matches!(source.kind(),SourceKind::Image{..}){
                return Err(Error::Invalid("Image Source properties require an image source."));
            }
            source.audio_properties.validate()?;
            if !source.kind.supports_audio_settings()&&!source.audio_monitoring.is_off(){return Err(Error::Invalid("monitoring requires an audio-capable source"));}
            if !source.kind.supports_audio_settings() && !source.audio_properties.is_default() {
                return Err(Error::Invalid(
                    "source audio properties require an audio-capable source",
                ));
            }
            if source.audio_filters.len() > MAX_AUDIO_FILTERS {
                return Err(Error::Limit("audio filter chain limit exceeded"));
            }
            if !source.audio_filters.is_empty() && !source.kind.supports_audio_settings() {
                return Err(Error::Invalid("audio filters require an audio source"));
            }
            if source.video_filters.len() > MAX_VIDEO_FILTERS {
                return Err(Error::Limit("video filter stack exceeds 32 effects"));
            }
            if (matches!(source.kind, SourceKind::AudioCapture { .. })
                || matches!(&source.kind,SourceKind::MediaSource{media}if media.video_dimensions().is_none()))
                && !source.video_filters.is_empty()
            {
                return Err(Error::Invalid(
                    "audio-only sources cannot contain video effects",
                ));
            }
            self.validate_filter_metadata(source, legacy, &mut check_id)?;
            for filter in &source.audio_filters {
                filter.validate()?;
            }
            if !names.insert(&source.name) {
                return Err(Error::Invalid("duplicate source name"));
            }
            if !source.gain.is_finite() || !(0.0..=20.0).contains(&source.gain) {
                return Err(Error::Invalid("invalid audio gain"));
            }
        }
        self.validate_audio_sidechains()?;
        if self.groups.iter().any(|group|!group.show_in_multiview){return Err(Error::Invalid("private groups cannot have Multiview scene metadata"));}
        let mut scene_names = HashSet::new();
        for scene in self.scenes.iter().chain(&self.groups) {
            if scene.notes.len() > MAX_SCENE_NOTES_BYTES {
                return Err(Error::Limit("scene notes exceed 16384 UTF-8 bytes"));
            }
            check_id(scene.id)?;
            validate_name(&scene.name)?;
            if let Some(config) = scene.transition_override {
                self.resolve_transition(&config)?;
            }
            if let Some(duration_ms) = scene.inactive_transition_duration_ms {
                if scene.transition_override.is_some() {
                    return Err(Error::Invalid(
                        "scene transition duration cannot have active and inactive owners",
                    ));
                }
                TransitionConfig::try_new(TransitionKind::Fade, duration_ms)?;
            }
            if !scene_names.insert(&scene.name) || (!names.insert(&scene.name) && !self.sources.iter().any(|source|
                source.name==scene.name && (matches!(source.kind,SourceKind::Scene {scene:target} if target==scene.id)||matches!(&source.kind,SourceKind::Group {group} if group.scene()==scene.id)))) {
                return Err(Error::Invalid("duplicate scene name"));
            }
            for item in &scene.items {
                check_id(item.id)?;
                let kind = &self
                    .source(item.source)
                    .ok_or(Error::NotFound(item.source))?
                    .kind;
                if matches!(kind, SourceKind::Scene { .. } | SourceKind::Group { .. }) {
                    item.transform.geometry(8192, 8192)?;
                } else if let Some((width, height)) = kind.video_dimensions() {
                    let source=self.source(item.source).ok_or(Error::NotFound(item.source))?;
                    if source.requires_video_canvas(){item.transform.geometry(8192,8192)?;}else{let frame=source.filtered_video_dimensions((width,height))?;item.transform.geometry_for_frame(width,height,frame.0,frame.1)?;}
                } else if item.transform != Transform::full(1, 1) {
                    return Err(Error::Invalid(
                        "audio-only scene items cannot contain a video transform",
                    ));
                }
            }
        }
        self.validate_scene_sources()?;
        if self.global_audio.len() > 6 {
            return Err(Error::Limit("global audio slot limit"));
        }
        let mut slots = HashSet::new();
        for root in &self.global_audio {
            if !slots.insert(root.slot()) {
                return Err(Error::Invalid("duplicate global audio slot"));
            }
            let source = self
                .source(root.source())
                .ok_or(Error::NotFound(root.source()))?;
            let SourceKind::AudioCapture { binding } = &source.kind else {
                return Err(Error::Invalid(
                    "global audio slots require an audio capture source",
                ));
            };
            if binding.flow() != root.slot().flow() {
                return Err(Error::Invalid(
                    "global audio slot flow does not match source flow",
                ));
            }
        }
        let referenced: HashSet<_> = self
            .scenes
            .iter()
            .chain(&self.groups)
            .flat_map(|scene| scene.items.iter().map(|item| item.source))
            .chain(self.global_audio.iter().map(GlobalAudioSource::source))
            .collect();
        if self.sources.iter().any(|source| {
            matches!(source.kind, SourceKind::AudioCapture { .. })
                && !referenced.contains(&source.id)
        }) {
            return Err(Error::Invalid(
                "audio capture source has no scene or global owner",
            ));
        }
        Ok(())
    }
    fn issue(&mut self) -> Result<Id, Error> {
        let id = Id(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        Ok(id)
    }
    fn issue_quick(&mut self) -> Result<QuickTransitionId, Error> {
        if self.next_quick_id >= quick_transition::QUICK_ID_SENTINEL {
            return Err(Error::IdentityExhausted);
        }
        let id = QuickTransitionId::new(self.next_quick_id)?;
        self.next_quick_id += 1;
        Ok(id)
    }
    fn issue_transition_preset(&mut self) -> Result<TransitionPresetId, Error> {
        if self.next_transition_preset_id >= transition_registry::PRESET_ID_SENTINEL {
            return Err(Error::IdentityExhausted);
        }
        let id = TransitionPresetId::new(self.next_transition_preset_id)?;
        self.next_transition_preset_id += 1;
        Ok(id)
    }
    fn preset_referenced(&self, id: TransitionPresetId) -> bool {
        let reference = TransitionRef::Preset(id);
        self.transition.reference() == reference
            || self
                .quick_transitions
                .iter()
                .any(|row| row.config().reference() == reference)
            || self.scenes.iter().any(|scene| {
                scene
                    .transition_override
                    .as_ref()
                    .is_some_and(|selection| selection.reference() == reference)
            })
    }
    fn configure_preset(
        &mut self,
        id: TransitionPresetId,
        name: String,
        kind: TransitionKind,
    ) -> Result<(), Error> {
        let index = self
            .transition_presets
            .iter()
            .position(|preset| preset.id() == id)
            .ok_or(Error::TransitionPresetNotFound(id))?;
        self.transition_presets[index] = TransitionPreset::new(id, name, kind)?;
        Ok(())
    }
    fn prune_sources(&mut self) {
        self.prune_group_sources();
    }
    fn configure_color_source(
        &mut self,
        source: Id,
        rgba: [u8; 4],
        width: u32,
        height: u32,
    ) -> Result<(), Error> {
        let replacement = SourceKind::Color {
            rgba,
            width,
            height,
        };
        replacement.validate()?;
        let original = self.source(source).ok_or(Error::NotFound(source))?;
        let SourceKind::Color {
            width: old_w,
            height: old_h,
            ..
        } = original.kind()
        else {
            return Err(Error::Invalid("source is not a color"));
        };
        let (old_w, old_h) = (*old_w, *old_h);
        self.source_mut(source)?.kind = replacement;
        if (old_w, old_h) != (width, height) {
            self.resize_reference_items(source, old_w, old_h, width, height);
        }
        Ok(())
    }
    fn resize_reference_items(
        &mut self,
        source: Id,
        old_w: u32,
        old_h: u32,
        new_w: u32,
        new_h: u32,
    ) {
        for scene in self.scenes.iter_mut().chain(&mut self.groups) {
            for item in &mut scene.items {
                if item.source == source {
                    item.transform.width = (f64::from(item.transform.width) * f64::from(new_w)
                        / f64::from(old_w)) as f32;
                    item.transform.height = (f64::from(item.transform.height) * f64::from(new_h)
                        / f64::from(old_h)) as f32;
                }
            }
        }
    }
    fn add_source(&mut self, scene: Id, name: String, kind: SourceKind) -> Result<Id, Error> {
        self.scene_mut(scene)?;
        let (width, height) = kind.video_dimensions().unwrap_or((1, 1));
        let source = self.add_source_owner(name, kind)?;
        let item = self.issue()?;
        self.scene_mut(scene)?.items.push(SceneItem {
            scale_anchor:None,
            color_tag:SourceItemColorTag::Clear,
            compositing: ItemCompositing::default(),
            id: item,
            source,
            visible: true,
            locked: false,
            transform: Transform::full(width, height),
        });
        Ok(item)
    }
    fn add_source_owner(&mut self, name: String, kind: SourceKind) -> Result<Id, Error> {
        kind.validate()?;
        validate_name(&name)?;
        let source = self.issue()?;
        self.sources.push(Source {
            image_settings:None,
            playback_hotkeys:SourcePlaybackHotkeys::default(),
            audio_hotkeys:SourceAudioHotkeys::default(),
            audio_monitoring:AudioMonitoringMode::Off,
            mixer: MixerSourceState::default(),
            audio_properties: SourceAudioProperties::default(),
            id: source,
            name: name.into_boxed_str().into_string(),
            kind,
            gain: 1.0,
            muted: false,
            audio_filters: Vec::new(),
            video_filters: Vec::new(),
        });
        Ok(source)
    }
    fn set_global_audio(&mut self, slot: AudioSlot, source: Option<Id>) -> Result<(), Error> {
        if let Some(id) = source {
            let selected = self.source(id).ok_or(Error::NotFound(id))?;
            let SourceKind::AudioCapture { binding } = selected.kind() else {
                return Err(Error::Invalid(
                    "global audio slots require an audio capture source",
                ));
            };
            if binding.flow() != slot.flow() {
                return Err(Error::Invalid(
                    "global audio slot flow does not match source flow",
                ));
            }
        }
        if self
            .global_audio
            .iter()
            .find(|root| root.slot() == slot)
            .map(GlobalAudioSource::source)
            == source
        {
            return Ok(());
        }
        self.global_audio.retain(|root| root.slot() != slot);
        if let Some(source) = source {
            self.global_audio.push(GlobalAudioSource::new(slot, source));
            self.global_audio.sort_by_key(GlobalAudioSource::slot);
        }
        self.prune_sources();
        Ok(())
    }
    fn scene_mut(&mut self, id: Id) -> Result<&mut Scene, Error> {
        self.scenes
            .iter_mut()
            .chain(&mut self.groups)
            .find(|s| s.id == id)
            .ok_or(Error::NotFound(id))
    }
    fn source_mut(&mut self, id: Id) -> Result<&mut Source, Error> {
        self.sources
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(Error::NotFound(id))
    }
    fn item_mut(&mut self, scene: Id, item: Id) -> Result<&mut SceneItem, Error> {
        self.scene_mut(scene)?
            .items
            .iter_mut()
            .find(|i| i.id == item)
            .ok_or(Error::NotFound(item))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformAction {
    Reset,
    FitToCanvas,
    StretchToCanvas,
    Center,
    CenterHorizontal,
    CenterVertical,
    #[serde(rename = "rotate_90_cw")]
    Rotate90Clockwise,
    #[serde(rename = "rotate_90_ccw")]
    Rotate90CounterClockwise,
    #[serde(rename = "rotate_180")]
    Rotate180,
    FlipHorizontal,
    FlipVertical,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    RelinkResources { session:u64,revision:u64,batch:ResourceRelinks },
    Setup {
        command: SetupCommand,
    },
    AddGroup {
        scene: Id,
        name: String,
    },
    GroupItems {
        scene: Id,
        name: String,
        items: Vec<Id>,
    },
    Ungroup {
        scene: Id,
        item: Id,
    },
    MoveIntoGroup {
        scene: Id,
        item: Id,
        group: Id,
    },
    MoveOutOfGroup {
        scene: Id,
        group: Id,
        item: Id,
        index: usize,
    },
    MoveGroupItem {
        from: Id,
        into: Id,
        item: Id,
        index: usize,
    },
    ConfigureGeneralSettings {language:String,projectors:ProjectorPreferences,#[serde(default,skip_serializing_if="Option::is_none")] preview_snapping:Option<PreviewSnapping>},
    SetPreviewSnapping {settings:PreviewSnapping},
    SetSavedProjectors {windows:SavedProjectorWindows},
    SetProjectorPreferences {preferences:ProjectorPreferences},
    SetSceneShowInMultiview {scene:Id,show:bool},
    SetUserPreferences {
        preferences: UserPreferences,
    },
    SetAppearance {
        appearance: Appearance,
    },
    SetNdiOutputSettings {settings:NdiOutputSettings},
    SetAccessibility {settings:Accessibility},
    SetProcessPriority {priority:ProcessPriority},
    ConfigureAdvancedSettings {profile:ProfileId,priority:ProcessPriority,reconnect:ReconnectSettings},
    SetHotkeys {
        hotkeys: HotkeyBindings,
    },
    ConfigureHotkeys {local:HotkeyBindings,global:GlobalHotkeyBindings},
    SetSourcePlaybackHotkeys {source:Id,bindings:SourcePlaybackHotkeys},
    ImageSource {command:ImageSourceCommand},
    SetSourceAudioHotkeys {source:Id,bindings:SourceAudioHotkeys},
    ConfigureSourceAudioHotkeys {source:Id,bindings:SourceAudioHotkeys,push:PushAudioSettings},
    SetMixerDisplay {
        preferences: MixerDisplayPreferences,
    },
    SetSourceMixerState {
        source: Id,
        action: MixerSourceAction,
    },
    UnhideAllMixerSources,
    SetAudioMonitoringDevice {device:AudioMonitoringDevice},
    SetSourceAudioMonitoring {source:Id,mode:AudioMonitoringMode},
    SetLanguage {
        language: String,
    },
    SetPluginEnabled {
        plugin: String,
        enabled: bool,
    },
    SetSourceVideoFilters {
        source: Id,
        filters: Vec<VideoFilter>,
    },
    SetSceneVideoFilters {
        scene: Id,
        filters: Vec<VideoFilter>,
    },
    PasteSourceFilters {
        from: Id,
        to: Id,
    },
    SetSourceAudioFilters {
        source: Id,
        filters: Vec<AudioFilter>,
    },
    SetSceneNotes {
        scene: Id,
        text: String,
    },
    Profile {
        command: ProfileCommand,
    },
    AddTransitionPreset {
        name: String,
        kind: TransitionKind,
    },
    ConfigureTransitionPreset {
        id: TransitionPresetId,
        name: String,
        kind: TransitionKind,
    },
    RenameTransitionPreset {
        id: TransitionPresetId,
        name: String,
    },
    SetTransitionPresetKind {
        id: TransitionPresetId,
        kind: TransitionKind,
    },
    RemoveTransitionPreset {
        id: TransitionPresetId,
    },
    RemoveTransitionPresetAndReferences {
        id: TransitionPresetId,
    },
    SelectTransition {
        selection: TransitionSelection,
    },
    AddQuickTransitionSelection {
        selection: TransitionSelection,
        fade_to_black: bool,
    },
    UpdateQuickTransitionSelection {
        id: QuickTransitionId,
        selection: TransitionSelection,
        fade_to_black: bool,
    },
    ConfigureSceneTransitionSelection {
        scene: Id,
        reference: Option<TransitionRef>,
        duration_ms: u32,
    },
    AddScene {
        name: String,
    },
    RenameScene {
        scene: Id,
        name: String,
    },
    MoveScene {
        scene: Id,
        index: usize,
    },
    RemoveScene {
        scene: Id,
    },
    DuplicateScene {
        scene: Id,
        name: String,
    },
    AddGroupTree {scene:Id,tree:GroupTree},
    AddSceneSource {
        scene: Id,
        target: Id,
    },
    RebindSceneSource {
        source: Id,
        target: Id,
    },
    RenameCollection {
        name: String,
    },
    AddColorSource {
        scene: Id,
        name: String,
        rgba: [u8; 4],
    },
    AddColorSourceSized {
        scene: Id,
        name: String,
        rgba: [u8; 4],
        width: u32,
        height: u32,
    },
    ConfigureColorSource {
        source: Id,
        rgba: [u8; 4],
        width: u32,
        height: u32,
    },
    AddImageSource {
        scene: Id,
        name: String,
        asset: ImageAsset,
    },
    SetImageSource {
        source: Id,
        asset: ImageAsset,
    },
    AddTextSource {
        scene: Id,
        name: String,
        settings: TextSource,
    },
    SetTextSource {
        source: Id,
        settings: TextSource,
    },
    AddDisplayCaptureSource {
        scene: Id,
        name: String,
        binding: DisplayCaptureBinding,
    },
    RebindDisplayCapture {
        source: Id,
        binding: DisplayCaptureBinding,
    },
    AddWindowCaptureSource {
        scene: Id,
        name: String,
        binding: WindowCaptureBinding,
    },
    SetWindowCaptureSource {
        source: Id,
        binding: WindowCaptureBinding,
    },
    AddVideoCaptureSource {
        scene: Id,
        name: String,
        binding: VideoCaptureBinding,
    },
    RebindVideoCapture {
        source: Id,
        binding: VideoCaptureBinding,
    },
    AddImageSlideShow {scene:Id,name:String,settings:ImageSlideShow},
    SetImageSlideShow {source:Id,settings:ImageSlideShow},
    AddBrowserSource {scene:Id,name:String,browser:BrowserSource},
    SetBrowserSource {source:Id,browser:BrowserSource},
    AddGameCapture {scene:Id,name:String,binding:GameCaptureBinding},
    SetGameCapture {source:Id,binding:GameCaptureBinding},
    AddNdiSource {scene:Id,name:String,ndi:NdiSource},
    SetNdiSource {source:Id,ndi:NdiSource},
    AddMediaSource {
        scene: Id,
        name: String,
        media: MediaSource,
    },
    RebindMediaSource {
        source: Id,
        media: MediaSource,
    },
    AddAudioCaptureSource {
        scene: Id,
        name: String,
        binding: AudioCaptureBinding,
    },
    RebindAudioCaptureSource {
        source: Id,
        binding: AudioCaptureBinding,
    },
    SetGlobalAudio {
        slot: AudioSlot,
        source: Option<Id>,
    },
    AddGlobalAudioCaptureSource {
        slot: AudioSlot,
        name: String,
        binding: AudioCaptureBinding,
    },
    AddExistingSource {
        scene: Id,
        source: Id,
    },
    PasteSources {
        scene: Id,
        items: Vec<SourcePasteItem>,
        duplicate: bool,
    },
    RenameSource {
        source: Id,
        name: String,
    },
    RemoveItem {
        scene: Id,
        item: Id,
    },
    /// Remove sibling scene items as one undoable operation.
    RemoveItems {
        scene: Id,
        items: Vec<Id>,
    },
    SetColor {
        source: Id,
        rgba: [u8; 4],
    },
    SetColorSize {
        source: Id,
        width: u32,
        height: u32,
    },
    SetVisible {
        scene: Id,
        item: Id,
        visible: bool,
    },
    SetLocked {
        scene: Id,
        item: Id,
        locked: bool,
    },
    SetSceneItemColorTag {scene:Id,item:Id,tag:SourceItemColorTag},
    SetItemCompositing {scene:Id,item:Id,settings:ItemCompositing},
    SetTransform {
        scene: Id,
        item: Id,
        transform: Transform,
    },
    SetTransforms { scene: Id, transforms: Vec<(Id, Transform)> },
    TransformItem {
        scene: Id,
        item: Id,
        action: TransformAction,
    },
    TransformItemForFrame {
        scene: Id,
        item: Id,
        action: TransformAction,
        frame_width: u32,
        frame_height: u32,
    },
    MoveItem {
        scene: Id,
        item: Id,
        index: usize,
    },
    /// Mixer fader and optional mute gesture, committed as one routing edit.
    SetMixerAudio {source:Id,gain:f32,muted:Option<bool>},
    SetAudio {
        source: Id,
        gain: f32,
        muted: bool,
    },
    SetSourceAudioTracks {source:Id,tracks:SourceAudioTrackMask},
    SetSourceAudioProperties {
        source: Id,
        properties: SourceAudioProperties,
    },
    SetAudioSettings {
        settings: AudioSettings,
    },
    ResizeOutputToSource {scene:Id,item:Id,raw_width:u32,raw_height:u32},
    ConfigureVideo {canvas:VideoSettings,output:OutputVideoSettings},
    SetVideo {
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
    },
    SetTransition {
        kind: TransitionKind,
        duration_ms: u32,
    },
    AddQuickTransition {
        kind: TransitionKind,
        duration_ms: u32,
        fade_to_black: bool,
    },
    UpdateQuickTransition {
        id: QuickTransitionId,
        kind: TransitionKind,
        duration_ms: u32,
        fade_to_black: bool,
    },
    RemoveQuickTransition {
        id: QuickTransitionId,
    },
    MoveQuickTransition {
        id: QuickTransitionId,
        index: usize,
    },
    ReplaceQuickTransitions {
        rows: Vec<QuickTransition>,
    },
    SetSceneTransition {
        scene: Id,
        config: Option<TransitionConfig>,
    },
    ConfigureSceneTransition {
        scene: Id,
        kind: Option<TransitionKind>,
        duration_ms: u32,
    },
    Undo,
    Redo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Unavailable(&'static str),
    Limit(&'static str),
    NotFound(Id),
    Locked(Id),
    Conflict { expected: u64, actual: u64 },
    Closed,
    IdentityExhausted,
    EmptyHistory,
    QuickTransitionNotFound(QuickTransitionId),
    TransitionPresetNotFound(TransitionPresetId),
    TransitionPresetReferenced(TransitionPresetId),
}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_input",
            Self::Unavailable(_) => "unavailable",
            Self::Limit(_) => "limit_exceeded",
            Self::NotFound(_) => "not_found",
            Self::Locked(_) => "item_locked",
            Self::Conflict { .. } => "revision_conflict",
            Self::Closed => "closed",
            Self::IdentityExhausted => "identity_exhausted",
            Self::EmptyHistory => "empty_history",
            Self::QuickTransitionNotFound(_) => "quick_transition_not_found",
            Self::TransitionPresetNotFound(_) => "transition_preset_not_found",
            Self::TransitionPresetReferenced(_) => "transition_preset_referenced",
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(s) | Self::Limit(s) | Self::Unavailable(s) => f.write_str(s),
            Self::NotFound(id) => write!(f, "entity {} not found", id.0),
            Self::Locked(id) => write!(f, "item {} is locked", id.0),
            Self::Conflict { expected, actual } => {
                write!(f, "expected revision {expected}, actual {actual}")
            }
            Self::Closed => f.write_str("session closed"),
            Self::IdentityExhausted => f.write_str("identity exhausted"),
            Self::EmptyHistory => f.write_str("no history entry"),
            Self::QuickTransitionNotFound(id) => {
                write!(f, "quick transition {} not found", id.get())
            }
            Self::TransitionPresetNotFound(id) => {
                write!(f, "transition preset {} not found", id.get())
            }
            Self::TransitionPresetReferenced(id) => {
                write!(f, "transition preset {} is still referenced", id.get())
            }
        }
    }
}
impl std::error::Error for Error {}
fn validate_name(name: &str) -> Result<(), Error> {
    if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        Err(Error::Invalid(
            "name must contain 1–256 UTF-8 bytes and no controls",
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Change {
    pub changed: bool,
    pub revision: u64,
    pub created: Option<Id>,
    pub created_transition_preset: Option<TransitionPresetId>,
}

/// Session serializes application publication; scene history contains only Collection.
/// Frontends and workers receive immutable StudioSnapshot values.
#[derive(Clone,Debug)]
pub struct SessionLifetime {identity:u64,live:Arc<AtomicBool>}
impl SessionLifetime {
    pub fn is_live_for(&self,identity:u64)->bool{self.identity==identity&&self.live.load(Ordering::Acquire)}
}
pub struct Session {
    source_definitions:source_definitions::SourceDefinitions,
    image_refresh_overrides:Vec<ImageAsset>,
    slideshow_refresh_overrides:Vec<(Id,ImageSlideShow)>,
    current: Arc<StudioSnapshot>,
    undo: VecDeque<Arc<Collection>>,
    redo: VecDeque<Arc<Collection>>,
    identity: u64,
    lifetime:SessionLifetime,
    high_water: u64,
    quick_high_water: u32,
    preset_high_water: u32,
    saved_revision: Option<u64>,
    document_revision: u64,
    saved_document_revision: Option<u64>,
    closed: bool,
    preview: Id,
    program: Id,
}
mod source_definitions;
mod source_output_resize;
pub use source_output_resize::SourceOutputSize;
pub use source_definitions::SourceDefinitionEpoch;
impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for Session {fn drop(&mut self){self.lifetime.live.store(false,Ordering::Release);}}
impl Session {
    pub fn new() -> Self {
        Self::from_snapshot(StudioSnapshot::default(), true).expect("default collection valid")
    }
    pub fn from_snapshot(mut collection: StudioSnapshot, saved: bool) -> Result<Self, Error> {
        collection.collection.migrate_video_filters()?;
        collection.materialize_scene_canvas()?;
        collection.validate()?;
        let source_definitions=source_definitions::SourceDefinitions::empty().prepare(None,&collection)?;
        let identity = SESSION_SERIAL
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| Error::IdentityExhausted)?;
        let initial = collection.scenes[0].id;
        Ok(Self {
            source_definitions,
            image_refresh_overrides:Vec::new(),
            slideshow_refresh_overrides:Vec::new(),
            high_water: collection.next_id,
            quick_high_water: collection.next_quick_id,
            preset_high_water: collection.next_transition_preset_id,
            saved_revision: saved.then_some(collection.revision),
            document_revision: 0,
            saved_document_revision: saved.then_some(0),
            current: Arc::new(collection),
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            identity,
            lifetime:SessionLifetime{identity,live:Arc::new(AtomicBool::new(true))},
            closed: false,
            preview: initial,
            program: initial,
        })
    }
    pub fn new_collection(&self) -> Result<Self, Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        let snapshot = StudioSnapshot::from_parts(
            0,
            Collection::default(),
            self.current.profiles.clone(),
            self.current.preferences.clone(),
        )?;
        Self::from_snapshot(snapshot, true)
    }
    pub fn adopt_collection(&self, imported: Session) -> Result<Self, Error> {
        if self.closed || imported.closed {
            return Err(Error::Closed);
        }
        let snapshot = StudioSnapshot::from_parts(
            0,
            imported.current.collection.clone(),
            self.current.profiles.clone(),
            self.current.preferences.clone(),
        )?;
        let mut candidate = Self::from_snapshot(snapshot, false)?;
        candidate.preview = imported.preview;
        candidate.program = imported.program;
        Ok(candidate)
    }
    /// Open a saved document without replacing the application's user scope.
    /// Native imports restore the collection/profiles and runtime scene choices;
    /// preferences remain owned by the current application session.
    pub fn adopt_document_preferences(&self,imported:Session)->Result<Self,Error>{self.adopt_document_preferences_ref(&imported)}
    /// Borrowed incoming document; no incoming history or receiver model clone.
    pub fn adopt_document_preferences_ref(&self,imported:&Session)->Result<Self,Error>{
        if self.closed{return Err(Error::Closed);}
        Self::prepare_document_with_preferences(imported,self.current.preferences())
    }
    /// Shared IO/UI projection using the exact incoming document and validated
    /// bounded application preferences. It never manufactures a receiver Session.
    pub fn prepare_document_with_preferences(imported:&Session,preferences:&UserPreferences)->Result<Self,Error>{
        if imported.closed{return Err(Error::Closed);}
        for scene in [imported.preview,imported.program]{if imported.current.scene(scene).is_none(){return Err(Error::NotFound(scene));}}
        let snapshot=StudioSnapshot::from_parts(0,imported.current.collection.clone(),imported.current.profiles.clone(),preferences.clone())?;
        let scene_saved=!imported.is_dirty();let document_saved=!imported.is_document_dirty();
        let mut candidate=Self::from_snapshot(snapshot,scene_saved)?;
        candidate.saved_document_revision=document_saved.then_some(candidate.document_revision);
        candidate.preview=imported.preview;candidate.program=imported.program;Ok(candidate)
    }
    pub fn snapshot(&self) -> Arc<StudioSnapshot> {
        self.current.clone()
    }
    pub fn identity(&self) -> u64 {
        self.identity
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }
    pub fn is_dirty(&self) -> bool {
        self.saved_revision != Some(self.current.revision)
    }
    pub fn document_revision(&self) -> u64 {
        self.document_revision
    }
    pub fn saved_document_revision(&self) -> Option<u64> {
        self.saved_document_revision
    }
    pub fn is_document_dirty(&self) -> bool {
        self.saved_document_revision != Some(self.document_revision)
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn preview(&self) -> Id {
        self.preview
    }
    pub fn program(&self) -> Id {
        self.program
    }
    pub fn select_preview(&mut self, scene: Id) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        if self.current.scene(scene).is_none() {
            return Err(Error::NotFound(scene));
        }
        self.preview = scene;
        Ok(())
    }
    /// Runtime output selection; transition scheduling is a host responsibility.
    pub fn select_program(&mut self, scene: Id) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        if self.current.scene(scene).is_none() {
            return Err(Error::NotFound(scene));
        }
        self.program = scene;
        Ok(())
    }
    /// Immediate cut to the current preview scene.
    pub fn cut(&mut self) -> Result<(), Error> {
        self.select_program(self.preview)
    }
    pub fn mark_saved(&mut self, session: u64, revision: u64) -> bool {
        if !self.closed && session == self.identity && revision == self.current.revision {
            self.saved_revision = Some(revision);
            self.saved_document_revision = Some(self.document_revision);
            true
        } else {
            false
        }
    }
    /// Cheap definition identity for native callbacks; unrelated model edits do
    /// not force a rebind, while Undo/restoration never reuses a retired epoch.
    pub fn source_definition_epoch(&self,source:Id)->Option<SourceDefinitionEpoch>{
        if self.closed{None}else{self.source_definitions.epoch(source)}
    }
    pub fn close(&mut self) {
        self.lifetime.live.store(false,Ordering::Release);
        self.source_definitions.clear();
        self.closed = true;
    }
    pub fn lifetime(&self)->SessionLifetime{self.lifetime.clone()}
    pub fn apply(&mut self, command: Command) -> Result<Change, Error> {
        self.apply_at(None, command)
    }
    pub fn apply_at(&mut self, expected: Option<u64>, command: Command) -> Result<Change, Error> {
        self.apply_at_with_gate(expected,command,||true)?.ok_or(Error::Unavailable("Command publication was declined."))
    }
    /// Synchronous final admission only: scalar/atomic work, no I/O, waits,
    /// Session mutation or retained callbacks. False publishes nothing, even
    /// for an otherwise accepted no-op/history cleanup. Preparation errors do
    /// not call the gate; a true receipt is separate from backend readiness.
    pub fn apply_at_with_gate<F:FnOnce()->bool>(&mut self,expected:Option<u64>,command:Command,gate:F)->Result<Option<Change>,Error>{
        if self.closed {
            return Err(Error::Closed);
        }
        if let Some(expected) = expected {
            if expected != self.current.revision {
                return Err(Error::Conflict {
                    expected,
                    actual: self.current.revision,
                });
            }
        }
        let image_override=image_source::manual_override(&self.image_refresh_overrides,&command)?;
        let slideshow_override=match &command {
            Command::SetImageSlideShow{source,settings} if self.slideshow_refresh_overrides.iter().any(|(id,old)|id==source && old.entries()==settings.entries())=>Some((*source,settings.clone())),
            _=>None,
        };
        let source_output_resize=matches!(&command,Command::ResizeOutputToSource{..});
        let scene_history = !matches!(
            &command,
            Command::Setup { .. }
                | Command::ConfigureGeneralSettings { .. }
                | Command::SetPreviewSnapping { .. }
                | Command::SetSavedProjectors { .. }
                | Command::SetProjectorPreferences { .. }
                | Command::SetSceneShowInMultiview { .. }
                | Command::SetSceneItemColorTag { .. }
                | Command::SetUserPreferences { .. }
                | Command::SetAppearance { .. }
                | Command::SetNdiOutputSettings { .. }
                | Command::SetAccessibility { .. }
                | Command::SetProcessPriority { .. }
                | Command::ConfigureAdvancedSettings { .. }
                | Command::SetHotkeys { .. }
                | Command::ConfigureHotkeys {..}
                | Command::SetSourcePlaybackHotkeys {..}
                | Command::SetSourceAudioHotkeys {..}
                | Command::ConfigureSourceAudioHotkeys {..}
                | Command::SetMixerDisplay { .. }
                | Command::SetSourceMixerState { .. }
                | Command::UnhideAllMixerSources
                | Command::SetPluginEnabled { .. }
                | Command::SetAudioMonitoringDevice {..}
                | Command::SetLanguage { .. }
                | Command::Profile { .. }
                | Command::SetAudioSettings { .. }
                | Command::SetVideo { .. }
                | Command::ConfigureVideo { .. }
        );
        let mut candidate = (*self.current).clone();
        candidate.next_id = candidate.next_id.max(self.high_water);
        candidate.next_quick_id = candidate.next_quick_id.max(self.quick_high_water);
        candidate.next_transition_preset_id = candidate
            .next_transition_preset_id
            .max(self.preset_high_water);
        let mut created = None;
        let mut created_transition_preset = None;
        let mut history_consumed=0usize;
        let mut resized_scene_edit=false;
        let history = match command {
            Command::Undo => {
                history_consumed=self.prepare_scene_history(&mut candidate,false)?;
                -1
            }
            Command::Redo => {
                history_consumed=self.prepare_scene_history(&mut candidate,true)?;
                1
            }
            command => {
                match command {
                    Command::Setup { command } => setup::apply(&mut candidate, command)?,
                    Command::SetAudioMonitoringDevice {device}=>candidate.preferences.set_audio_monitoring_device(device)?,
                    Command::SetSourceAudioMonitoring {source,mode}=>{
                        let target=candidate.source_mut(source)?;
                        if !target.kind.supports_audio_settings(){return Err(Error::Invalid("monitoring requires an audio-capable source"));}
                        target.audio_monitoring=mode;
                    }
                    Command::ConfigureGeneralSettings{language,projectors,preview_snapping}=>{candidate.preferences.set_language(language)?;candidate.preferences.set_projectors(projectors);if let Some(settings)=preview_snapping{candidate.preferences.set_preview_snapping(settings)?;}},
                    Command::SetPreviewSnapping{settings}=>candidate.preferences.set_preview_snapping(settings)?,
                    Command::SetSavedProjectors {windows}=>candidate.collection.saved_projectors=windows,
                    Command::SetProjectorPreferences {preferences}=>candidate.preferences.set_projectors(preferences),
                    Command::SetSceneItemColorTag {scene,item,tag}=>{candidate.scene_mut(scene)?.items.iter_mut().find(|entry|entry.id==item).ok_or(Error::NotFound(item))?.color_tag=tag;},
                    Command::SetSceneShowInMultiview {scene,show}=>{candidate.collection.scenes.iter_mut().find(|value|value.id==scene).ok_or(Error::NotFound(scene))?.show_in_multiview=show;},
                    Command::SetUserPreferences { preferences } => {
                        preferences.validate()?;
                        candidate.preferences = preferences;
                    }
                    Command::SetAppearance { appearance } => {
                        candidate.preferences.set_appearance(appearance)?;
                    }
                    Command::SetNdiOutputSettings {settings}=>candidate.preferences.set_ndi_output(settings)?,
                    Command::SetAccessibility {settings}=>candidate.preferences.set_accessibility(settings)?,
                    Command::ConfigureAdvancedSettings {profile,priority,reconnect}=>{if candidate.profiles.selected_id()!=profile{return Err(Error::Invalid("Advanced settings belong to another selected profile"));}candidate.profiles.apply(ProfileCommand::SetReconnect{id:profile,settings:reconnect})?;candidate.preferences.set_process_priority(priority);},
                    Command::SetProcessPriority {priority}=>candidate.preferences.set_process_priority(priority),
                    Command::SetMixerDisplay { preferences } => {
                        candidate.preferences.set_mixer_display(preferences)
                    }
                    Command::SetSourceMixerState { source, action } => {
                        candidate.set_source_mixer_state(source, action)?
                    }
                    Command::UnhideAllMixerSources => candidate.unhide_all_mixer_sources(),
                    Command::ConfigureHotkeys {local,global}=>candidate.preferences.configure_hotkeys(local,global)?,
                    Command::SetSourceAudioHotkeys {source,bindings}=>{let target=candidate.source_mut(source)?;bindings.validate_kind(&target.kind)?;target.audio_hotkeys=bindings;},
                    Command::ConfigureSourceAudioHotkeys {source,bindings,push}=>{let target=candidate.source_mut(source)?;bindings.validate_kind(&target.kind)?;if !target.kind.supports_audio_settings(){return Err(Error::Invalid("Push controls require an audio-capable source."));}target.audio_hotkeys=bindings;target.audio_properties=target.audio_properties.with_push(push);},
                    Command::SetSourcePlaybackHotkeys {source,bindings}=>{let target=candidate.source_mut(source)?;bindings.validate_kind(&target.kind)?;target.playback_hotkeys=bindings;},
                    Command::SetHotkeys { hotkeys } => {
                        candidate.preferences.set_hotkeys(hotkeys)?;
                    }
                    Command::SetLanguage { language } => {
                        candidate.preferences.set_language(language)?
                    }
                    Command::SetPluginEnabled { plugin, enabled } => {
                        candidate.preferences.set_plugin_enabled(plugin, enabled)?
                    }
                    Command::SetSceneNotes { scene, text } => {
                        if !candidate.preferences.plugin_enabled(SCENE_NOTES_PLUGIN) {
                            return Err(Error::Unavailable("Scene Notes module is disabled"));
                        }
                        if text.len() > MAX_SCENE_NOTES_BYTES {
                            return Err(Error::Limit("scene notes exceed 16384 UTF-8 bytes"));
                        }
                        candidate.scene_mut(scene)?.notes = text.into_boxed_str().into_string();
                    }
                    Command::SetSceneVideoFilters { scene, filters } => {
                        candidate.set_scene_video_filters(scene,filters)?;
                    }
                    Command::SetSourceVideoFilters { source, filters } => {
                        candidate.set_source_video_filters(source,filters)?;
                    }
                    Command::PasteSourceFilters { from, to } => {
                        candidate.paste_source_filters(from, to)?
                    }
                    Command::SetSourceAudioFilters { source, filters } => {
                        if filters.len() > MAX_AUDIO_FILTERS {
                            return Err(Error::Limit("audio filter chain limit exceeded"));
                        }
                        let old = candidate.source(source).ok_or(Error::NotFound(source))?;
                        if !old.kind.supports_audio_settings() {
                            return Err(Error::Invalid("audio filters require an audio source"));
                        }
                        let mut retained = old.audio_filters.clone();
                        for filter in &filters {
                            filter.validate()?;
                            if !candidate.preferences.plugin_enabled(filter.plugin_id()) {
                                if let Some(index) = retained.iter().position(|old| old == filter) {
                                    retained.remove(index);
                                } else {
                                    return Err(Error::Unavailable(
                                        "enable the filter module before adding or changing its settings",
                                    ));
                                }
                            }
                        }
                        candidate.validate_sidechain_assignment(source,&filters,&candidate.source(source).ok_or(Error::NotFound(source))?.audio_filters)?;
                        candidate.source_mut(source)?.audio_filters =
                            filters.into_boxed_slice().into_vec();
                    }
                    Command::Profile { command } => candidate.profiles.apply(command)?,
                    Command::AddScene { name } => {
                        let id = candidate.issue()?;
                        candidate.scenes.push(Scene {
                            show_in_multiview:true,
                            notes: String::new(),
                            id,
                            name,
                            items: vec![],
                            transition_override: None,
                            inactive_transition_duration_ms: None,
                        });
                        created = Some(id);
                    }
                    Command::RenameScene { scene, name } => {
                        candidate.scene_mut(scene)?.name = name.clone();
                        for source in &mut candidate.collection.sources {
                            if matches!(source.kind,SourceKind::Scene {scene:target} if target==scene)
                            {
                                source.name = name.clone();
                            }
                        }
                    }
                    Command::MoveScene { scene, index } => {
                        let position = candidate
                            .scenes
                            .iter()
                            .position(|entry| entry.id == scene)
                            .ok_or(Error::NotFound(scene))?;
                        if index >= candidate.scenes.len() {
                            return Err(Error::Invalid("scene order index out of range"));
                        }
                        let value = candidate.scenes.remove(position);
                        candidate.scenes.insert(index, value);
                    }
                    Command::RemoveScene { scene } => {
                        candidate.scene(scene).ok_or(Error::NotFound(scene))?;
                        candidate.scenes.retain(|s| s.id != scene);
                        let incoming:HashSet<_>=candidate.sources.iter().filter(|source|matches!(source.kind,SourceKind::Scene {scene:target} if target==scene)).map(|source|source.id).collect();
                        for remaining in candidate
                            .collection
                            .scenes
                            .iter_mut()
                            .chain(&mut candidate.collection.groups)
                        {
                            remaining
                                .items
                                .retain(|item| !incoming.contains(&item.source));
                        }
                        candidate.prune_sources();
                    }
                    Command::DuplicateScene { scene, name } => {
                        let original = candidate
                            .scene(scene)
                            .ok_or(Error::NotFound(scene))?
                            .clone();
                        let root_filters = candidate
                            .scene_source(scene)
                            .map(|source| {
                                source
                                    .video_filters
                                    .iter()
                                    .map(VideoFilter::as_new)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        let id = candidate.issue()?;
                        let mut items = original.items;
                        candidate.duplicate_private_groups(&mut items)?;
                        candidate.scenes.push(Scene {
                            show_in_multiview:original.show_in_multiview,
                            notes: original.notes,
                            id,
                            name,
                            items,
                            transition_override: original.transition_override,
                            inactive_transition_duration_ms: original
                                .inactive_transition_duration_ms,
                        });
                        if !root_filters.is_empty() {
                            let owner = candidate.ensure_scene_source(id)?;
                            candidate.set_video_filters(owner, root_filters)?;
                        }
                        created = Some(id);
                    }
                    Command::AddGroup { scene, name } => {
                        created = Some(candidate.create_group(scene, name, vec![])?);
                    }
                    Command::GroupItems { scene, name, items } => {
                        created = Some(candidate.create_group(scene, name, items)?);
                    }
                    Command::Ungroup { scene, item } => candidate.ungroup(scene, item)?,
                    Command::MoveIntoGroup { scene, item, group } => {
                        candidate.move_into_group(scene, item, group)?
                    }
                    Command::MoveOutOfGroup {
                        scene,
                        group,
                        item,
                        index,
                    } => candidate.move_out_of_group(scene, group, item, index)?,
                    Command::MoveGroupItem {
                        from,
                        into,
                        item,
                        index,
                    } => candidate.move_group_item(from, into, item, index)?,
                    Command::AddGroupTree {scene,tree}=>{created=Some(candidate.add_group_tree(scene,tree)?);}
                    Command::AddSceneSource { scene, target } => {
                        candidate
                            .scene_container(scene)
                            .ok_or(Error::NotFound(scene))?;
                        let source = candidate.ensure_scene_source(target)?;
                        let id = candidate.issue()?;
                        let transform =
                            Transform::full(candidate.video().width(), candidate.video().height());
                        candidate.scene_mut(scene)?.items.push(SceneItem {
            scale_anchor:None,
            color_tag:SourceItemColorTag::Clear,
            compositing: ItemCompositing::default(),
                            id,
                            source,
                            visible: true,
                            locked: false,
                            transform,
                        });
                        created = Some(id);
                    }
                    Command::RebindSceneSource { source, target } => {
                        let original = candidate.source(source).ok_or(Error::NotFound(source))?;
                        if !matches!(original.kind, SourceKind::Scene { .. }) {
                            return Err(Error::Invalid("source is not a scene reference"));
                        }
                        let replacement = candidate.ensure_scene_source(target)?;
                        for scene in candidate
                            .collection
                            .scenes
                            .iter_mut()
                            .chain(&mut candidate.collection.groups)
                        {
                            for item in &mut scene.items {
                                if item.source == source {
                                    item.source = replacement;
                                }
                            }
                        }
                    }
                    Command::RenameCollection { name } => candidate.name = name,
                    Command::AddColorSource { scene, name, rgba } => {
                        let kind = SourceKind::Color {
                            rgba,
                            width: candidate.video().width,
                            height: candidate.video().height,
                        };
                        created = Some(candidate.add_source(scene, name, kind)?);
                    }
                    Command::AddColorSourceSized {
                        scene,
                        name,
                        rgba,
                        width,
                        height,
                    } => {
                        created = Some(candidate.add_source(
                            scene,
                            name,
                            SourceKind::Color {
                                rgba,
                                width,
                                height,
                            },
                        )?);
                    }
                    Command::ConfigureColorSource {
                        source,
                        rgba,
                        width,
                        height,
                    } => {
                        candidate.configure_color_source(source, rgba, width, height)?;
                    }
                    Command::AddImageSlideShow {scene,name,settings} => {
                        settings.validate()?;
                        created=Some(candidate.add_source(scene,name,SourceKind::ImageSlideShow {settings})?);
                    }
                    Command::SetImageSlideShow {source,settings} => {
                        settings.validate()?;
                        let SourceKind::ImageSlideShow {settings:old}=&candidate.source_mut(source)?.kind else {return Err(Error::Invalid("source is not an image slideshow"));};
                        let (ow,oh)=old.dimensions();let (nw,nh)=settings.dimensions();
                        candidate.source_mut(source)?.kind=SourceKind::ImageSlideShow {settings};
                        candidate.resize_reference_items(source,ow,oh,nw,nh);
                    }
                    Command::AddImageSource { scene, name, asset } => {
                        created=candidate.image_source_command(ImageSourceCommand::Add{scene,name,asset,settings:Default::default()})?;
                    }
                    Command::ImageSource{command}=>{created=candidate.image_source_command(command)?;}
                    Command::RelinkResources {session,revision,batch}=>{if session!=self.identity(){return Err(Error::Invalid("resource relink belongs to another Session"));}if revision!=self.current.revision{return Err(Error::Conflict{expected:revision,actual:self.current.revision});}candidate.relink_resources(batch)?;},
                    Command::SetImageSource { source, asset } => {
                        let settings=candidate.source(source).ok_or(Error::NotFound(source))?.image_settings();
                        candidate.image_source_command(ImageSourceCommand::Configure{source,asset,settings})?;
                    }
                    Command::AddTextSource {
                        scene,
                        name,
                        settings,
                    } => {
                        created = Some(candidate.add_source(
                            scene,
                            name,
                            SourceKind::Text { settings },
                        )?);
                    }
                    Command::SetTextSource { source, settings } => {
                        settings.validate()?;
                        let old = candidate.source_mut(source)?;
                        let SourceKind::Text { settings: previous } = &old.kind else {
                            return Err(Error::Invalid("source is not text"));
                        };
                        let (old_w, old_h) = (previous.width(), previous.height());
                        let (new_w, new_h) = (settings.width(), settings.height());
                        candidate.source_mut(source)?.kind = SourceKind::Text { settings };
                        candidate.resize_reference_items(source, old_w, old_h, new_w, new_h);
                    }
                    Command::AddDisplayCaptureSource {
                        scene,
                        name,
                        binding,
                    } => {
                        created = Some(candidate.add_source(
                            scene,
                            name,
                            SourceKind::DisplayCapture { binding },
                        )?);
                    }
                    Command::RebindDisplayCapture { source, binding } => {
                        binding.validate()?;
                        let previous = candidate.source_mut(source)?;
                        let SourceKind::DisplayCapture { binding: old } = &previous.kind else {
                            return Err(Error::Invalid("source is not a display capture"));
                        };
                        let (old_w, old_h) = (old.width(), old.height());
                        let (new_w, new_h) = (binding.width(), binding.height());
                        candidate.source_mut(source)?.kind = SourceKind::DisplayCapture { binding };
                        candidate.resize_reference_items(source, old_w, old_h, new_w, new_h);
                    }
                    Command::AddWindowCaptureSource {
                        scene,
                        name,
                        binding,
                    } => {
                        created = Some(candidate.add_source(
                            scene,
                            name,
                            SourceKind::WindowCapture { binding },
                        )?);
                    }
                    Command::SetWindowCaptureSource { source, binding } => {
                        binding.validate()?;
                        let previous = candidate.source_mut(source)?;
                        let SourceKind::WindowCapture { binding: old } = &previous.kind else {
                            return Err(Error::Invalid("source is not a window capture"));
                        };
                        let (old_w, old_h) = (old.width(), old.height());
                        let (new_w, new_h) = (binding.width(), binding.height());
                        candidate.source_mut(source)?.kind = SourceKind::WindowCapture { binding };
                        candidate.resize_reference_items(source, old_w, old_h, new_w, new_h);
                    }
                    Command::AddVideoCaptureSource {
                        scene,
                        name,
                        binding,
                    } => {
                        created = Some(candidate.add_source(
                            scene,
                            name,
                            SourceKind::VideoCapture { binding },
                        )?);
                    }
                    Command::RebindVideoCapture { source, binding } => {
                        binding.validate()?;
                        let previous = candidate.source_mut(source)?;
                        let SourceKind::VideoCapture { binding: old } = &previous.kind else {
                            return Err(Error::Invalid("source is not a video capture"));
                        };
                        let (old_w, old_h) = (old.width(), old.height());
                        let (new_w, new_h) = (binding.width(), binding.height());
                        candidate.source_mut(source)?.kind = SourceKind::VideoCapture { binding };
                        candidate.resize_reference_items(source, old_w, old_h, new_w, new_h);
                    }
                    Command::AddAudioCaptureSource {
                        scene,
                        name,
                        binding,
                    } => {
                        created = Some(candidate.add_source(
                            scene,
                            name,
                            SourceKind::AudioCapture { binding },
                        )?);
                    }
                    Command::AddBrowserSource {scene,name,browser}=>{browser.validate()?;created=Some(candidate.add_source(scene,name,SourceKind::Browser {browser})?);}
                    Command::SetBrowserSource {source,browser}=>{
                        browser.validate()?;let old=candidate.source_mut(source)?;if !browser.control_audio()&&(!old.audio_filters.is_empty()||!old.audio_monitoring.is_off()||!old.audio_properties.is_default()){return Err(Error::Invalid("remove browser audio filters, monitoring and audio properties before returning audio to native CEF playback"));}let SourceKind::Browser {browser:previous}=&old.kind else{return Err(Error::Invalid("source is not a browser"));};
                        let (ow,oh)=(previous.width(),previous.height());let (nw,nh)=(browser.width(),browser.height());candidate.source_mut(source)?.kind=SourceKind::Browser {browser};candidate.resize_reference_items(source,ow,oh,nw,nh);
                    }
                    Command::AddGameCapture {scene,name,binding}=>{
                        binding.validate()?;created=Some(candidate.add_source(scene,name,SourceKind::GameCapture {binding})?);
                    }
                    Command::SetGameCapture {source,binding}=>{
                        binding.validate()?;
                        let SourceKind::GameCapture {binding:old}=&candidate.source(source).ok_or(Error::NotFound(source))?.kind else{return Err(Error::Invalid("source is not game capture"));};
                        let (ow,oh)=(old.width(),old.height());let (nw,nh)=(binding.width(),binding.height());
                        candidate.source_mut(source)?.kind=SourceKind::GameCapture {binding};candidate.resize_reference_items(source,ow,oh,nw,nh);
                    }
                    Command::AddNdiSource {scene,name,ndi}=>{
                        if !candidate.preferences.plugin_enabled(NDI_PLUGIN){return Err(Error::Unavailable("enable DistroAV NDI receive module before adding this source"));}
                        ndi.validate()?;created=Some(candidate.add_source(scene,name,SourceKind::NdiSource {ndi})?);
                    }
                    Command::SetNdiSource {source,ndi}=>{
                        if !candidate.preferences.plugin_enabled(NDI_PLUGIN){return Err(Error::Unavailable("enable DistroAV NDI receive module before editing this source"));}
                        ndi.validate()?;
                        let SourceKind::NdiSource {ndi:old}=&candidate.source_mut(source)?.kind else{return Err(Error::Invalid("source is not NDI"));};
                        let (ow,oh)=(old.width(),old.height());let (nw,nh)=(ndi.width(),ndi.height());
                        candidate.source_mut(source)?.kind=SourceKind::NdiSource {ndi};candidate.resize_reference_items(source,ow,oh,nw,nh);
                    }
                    Command::AddMediaSource { scene, name, media } => {
                        created = Some(candidate.add_source(
                            scene,
                            name,
                            SourceKind::MediaSource { media },
                        )?);
                    }
                    Command::RebindMediaSource { source, media } => {
                        media.validate()?;
                        let previous = candidate.source_mut(source)?;
                        let SourceKind::MediaSource { media: old } = &previous.kind else {
                            return Err(Error::Invalid("source is not a media source"));
                        };
                        if !media.has_audio() && !previous.audio_filters.is_empty() {
                            return Err(Error::Invalid(
                                "remove audio filters before rebinding to video-only media",
                            ));
                        }
                        if media.video_dimensions().is_none() && !previous.video_filters.is_empty()
                        {
                            return Err(Error::Invalid(
                                "remove video filters before rebinding to audio-only media",
                            ));
                        }
                        let (old_w, old_h) = old.video_dimensions().unwrap_or((1, 1));
                        let (new_w, new_h) = media.video_dimensions().unwrap_or((1, 1));
                        candidate.source_mut(source)?.kind = SourceKind::MediaSource { media };
                        candidate.resize_reference_items(source, old_w, old_h, new_w, new_h);
                    }
                    Command::RebindAudioCaptureSource { source, binding } => {
                        binding.validate()?;
                        let selected = candidate.source_mut(source)?;
                        if !matches!(selected.kind, SourceKind::AudioCapture { .. }) {
                            return Err(Error::Invalid("source is not an audio capture"));
                        }
                        selected.kind = SourceKind::AudioCapture { binding };
                    }
                    Command::SetGlobalAudio { slot, source } => {
                        candidate.set_global_audio(slot, source)?
                    }
                    Command::AddGlobalAudioCaptureSource {
                        slot,
                        name,
                        binding,
                    } => {
                        if binding.flow() != slot.flow() {
                            return Err(Error::Invalid(
                                "global audio slot flow does not match source flow",
                            ));
                        }
                        let source = candidate
                            .add_source_owner(name, SourceKind::AudioCapture { binding })?;
                        candidate.set_global_audio(slot, Some(source))?;
                        created = Some(source);
                    }
                    Command::AddExistingSource { scene, source } => {
                        if matches!(
                            candidate.source(source).map(|source| source.kind()),
                            Some(SourceKind::Group { .. })
                        ) {
                            return Err(Error::Invalid(
                                "groups cannot be pasted as shared existing sources",
                            ));
                        }
                        let (width, height) = candidate
                            .source_video_dimensions(
                                &candidate
                                    .source(source)
                                    .ok_or(Error::NotFound(source))?
                                    .kind,
                            )?
                            .unwrap_or((1, 1));
                        let id = candidate.issue()?;
                        let transform = Transform::full(width, height);
                        candidate.scene_mut(scene)?.items.push(SceneItem {
            scale_anchor:None,
            color_tag:SourceItemColorTag::Clear,
            compositing: ItemCompositing::default(),
                            id,
                            source,
                            visible: true,
                            locked: false,
                            transform,
                        });
                        created = Some(id);
                    }
                    Command::PasteSources { scene, items, duplicate } => {
                        created = candidate.paste_sources(scene, items, duplicate)?;
                    }
                    Command::RenameSource { source, name } => {
                        if let Some(SourceKind::Group { group }) =
                            candidate.source(source).map(|source| &source.kind)
                        {
                            let scene = group.scene();
                            candidate.scene_mut(scene)?.name = name.clone();
                        }
                        if let SourceKind::Scene { scene } = candidate
                            .source(source)
                            .ok_or(Error::NotFound(source))?
                            .kind
                        {
                            candidate.scene_mut(scene)?.name = name.clone();
                        }
                        candidate.source_mut(source)?.name = name
                    }
                    Command::RemoveItem { scene, item } => {
                        candidate.ensure_container_unlocked(scene)?;
                        if candidate.item_mut(scene, item)?.locked {
                            return Err(Error::Locked(item));
                        }
                        candidate.scene_mut(scene)?.items.retain(|i| i.id != item);
                        candidate.prune_sources();
                    }
                    Command::RemoveItems { scene, items } => {
                        candidate.ensure_container_unlocked(scene)?;
                        if items.is_empty() || items.len() > MAX_ITEMS {
                            return Err(Error::Invalid("Select at least one source to remove"));
                        }
                        let mut unique = std::collections::HashSet::with_capacity(items.len());
                        for item in &items {
                            if !unique.insert(*item) {
                                return Err(Error::Invalid("Duplicate source in removal selection"));
                            }
                            if candidate.item_mut(scene, *item)?.locked {
                                return Err(Error::Locked(*item));
                            }
                        }
                        candidate.scene_mut(scene)?.items.retain(|item| !unique.contains(&item.id));
                        candidate.prune_sources();
                    }
                    Command::SetColor { source, rgba } => {
                        let SourceKind::Color { rgba: color, .. } =
                            &mut candidate.source_mut(source)?.kind
                        else {
                            return Err(Error::Invalid("source is not a color"));
                        };
                        *color = rgba;
                    }
                    Command::SetColorSize {
                        source,
                        width,
                        height,
                    } => {
                        let original = candidate.source(source).ok_or(Error::NotFound(source))?;
                        let SourceKind::Color { rgba, .. } = original.kind() else {
                            return Err(Error::Invalid("source is not a color"));
                        };
                        let rgba = *rgba;
                        candidate.configure_color_source(source, rgba, width, height)?;
                    }
                    Command::SetVisible {
                        scene,
                        item,
                        visible,
                    } => candidate.item_mut(scene, item)?.visible = visible,
                    Command::SetLocked {
                        scene,
                        item,
                        locked,
                    } => candidate.item_mut(scene, item)?.locked = locked,
                    Command::SetItemCompositing {scene,item,settings} => candidate.set_item_compositing(scene,item,settings)?,
                    Command::SetTransform {
                        scene,
                        item,
                        transform,
                    } => {
                        candidate.ensure_container_unlocked(scene)?;
                        let original = candidate.item_mut(scene, item)?.clone();
                        if original.locked {
                            return Err(Error::Locked(item));
                        }
                        if candidate
                            .source_video_dimensions(
                                &candidate
                                    .source(original.source)
                                    .ok_or(Error::NotFound(original.source))?
                                    .kind,
                            )?
                            .is_none()
                        {
                            return Err(Error::Invalid(
                                "audio-only sources do not support video geometry commands",
                            ));
                        }
                        candidate.item_mut(scene, item)?.transform = transform;
                    }
                    Command::SetTransforms { scene, transforms } => {
                        candidate.ensure_container_unlocked(scene)?;
                        if transforms.is_empty() || transforms.len() > MAX_SOURCES { return Err(Error::Invalid("Invalid group transform count")); }
                        let mut seen = std::collections::HashSet::new();
                        for (item, transform) in transforms {
                            if !seen.insert(item) { return Err(Error::Invalid("Duplicate group transform item")); }
                            let original = candidate.item_mut(scene, item)?.clone();
                            if original.locked { return Err(Error::Locked(item)); }
                            let source = candidate.source(original.source).ok_or(Error::NotFound(original.source))?;
                            if candidate.source_video_dimensions(&source.kind)?.is_none() { return Err(Error::Invalid("audio-only sources do not support video geometry commands")); }
                            candidate.source_item_geometry(source, &transform)?;
                            candidate.item_mut(scene, item)?.transform = transform;
                        }
                    }
                    Command::TransformItem {
                        scene,
                        item,
                        action,
                    } => {
                        candidate.ensure_container_unlocked(scene)?;
                        let original = candidate.item_mut(scene, item)?.clone();
                        if original.locked {
                            return Err(Error::Locked(item));
                        }
                        let (width, height) = candidate
                            .source_video_dimensions(
                                &candidate
                                    .source(original.source)
                                    .ok_or(Error::NotFound(original.source))?
                                    .kind,
                            )?
                            .ok_or(Error::Invalid(
                                "audio-only sources do not support video geometry commands",
                            ))?;
                        let mut transform = original.transform;
                        let source=candidate.source(original.source).ok_or(Error::NotFound(original.source))?;
                        let frame=candidate.effective_filtered_video_dimensions(source,(width,height),Some((candidate.video().width(),candidate.video().height())))?;
                        transform.apply_action_for_frame(action,(width,height),frame,(candidate.video().width,candidate.video().height))?;
                        candidate.item_mut(scene, item)?.transform = transform;
                    }
                    Command::TransformItemForFrame {
                        scene,
                        item,
                        action,
                        frame_width,
                        frame_height,
                    } => {
                        candidate.ensure_container_unlocked(scene)?;
                        let original = candidate.item_mut(scene, item)?.clone();
                        if original.locked {
                            return Err(Error::Locked(item));
                        }
                        let source = candidate
                            .source(original.source)
                            .ok_or(Error::NotFound(original.source))?;
                        let basis = match &source.kind {
                            SourceKind::DisplayCapture { binding } => {
                                (binding.width(), binding.height())
                            }
                            SourceKind::WindowCapture { binding } => {
                                (binding.width(), binding.height())
                            }
                            SourceKind::VideoCapture { binding } => {
                                (binding.width(), binding.height())
                            }
                            SourceKind::GameCapture {binding}=>(binding.width(),binding.height()),
                            SourceKind::NdiSource {ndi}=>(ndi.width(),ndi.height()),
                            SourceKind::Browser {browser}=>(browser.width(),browser.height()),
                            SourceKind::MediaSource { media } => media
                                .video_dimensions()
                                .ok_or(Error::Invalid("audio-only media has no video frame"))?,
                            _ => {
                                return Err(Error::Invalid(
                                    "runtime-frame transform actions require a live capture source",
                                ));
                            }
                        };
                        if frame_width == 0
                            || frame_height == 0
                            || frame_width > 8192
                            || frame_height > 8192
                            || u64::from(frame_width) * u64::from(frame_height) > MAX_DISPLAY_PIXELS
                        {
                            return Err(Error::Invalid(
                                "runtime capture frame dimensions exceed 8192-edge/32M-pixel limits",
                            ));
                        }
                        let frame=candidate.effective_filtered_video_dimensions(source,(frame_width,frame_height),Some((candidate.video().width(),candidate.video().height())))?;
                        let mut transform = original.transform;
                        transform.apply_action_for_frame(
                            action,
                            basis,
                            frame,
                            (candidate.video().width, candidate.video().height),
                        )?;
                        candidate.item_mut(scene, item)?.transform = transform;
                    }
                    Command::MoveItem { scene, item, index } => {
                        candidate.ensure_container_unlocked(scene)?;
                        let target = candidate.scene_mut(scene)?;
                        let position = target
                            .items
                            .iter()
                            .position(|i| i.id == item)
                            .ok_or(Error::NotFound(item))?;
                        if target.items[position].locked {
                            return Err(Error::Locked(item));
                        }
                        if index >= target.items.len() {
                            return Err(Error::Invalid("item order index out of range"));
                        }
                        let value = target.items.remove(position);
                        target.items.insert(index, value);
                    }
                    Command::SetMixerAudio {source,gain,muted}=>{
                        let target=candidate.source_mut(source)?;
                        if !target.kind.supports_audio_settings(){return Err(Error::Invalid("mixer audio requires an audio-capable source"));}
                        target.gain=gain;
                        if let Some(muted)=muted {
                            target.muted=muted;
                            if !muted&&target.audio_monitoring==AudioMonitoringMode::MonitorOnly {
                                target.audio_monitoring=AudioMonitoringMode::MonitorAndOutput;
                            }
                        }
                    }
                    Command::SetAudio {
                        source,
                        gain,
                        muted,
                    } => {
                        let target = candidate.source_mut(source)?;
                        target.gain = gain;
                        target.muted = muted;
                    }
                    Command::SetSourceAudioTracks{source,tracks}=>{
                        let target=candidate.source_mut(source)?;
                        if !target.kind.supports_audio_settings(){return Err(Error::Invalid("audio track routing requires an audio-capable source"));}
                        target.audio_properties=target.audio_properties.with_audio_tracks(tracks);
                    }
                    Command::SetSourceAudioProperties { source, properties } => {
                        properties.validate()?;
                        let target = candidate.source_mut(source)?;
                        if !target.kind.supports_audio_settings() {
                            return Err(Error::Invalid(
                                "source audio properties require an audio-capable source",
                            ));
                        }
                        target.audio_properties = properties;
                    }
                    Command::SetAudioSettings { settings } => {
                        settings.validate()?;
                        candidate.profiles.set_audio(settings)?;
                        candidate
                            .preferences
                            .set_audio_buffering(settings.buffering())?;
                    }
                    Command::ResizeOutputToSource{scene,item,raw_width,raw_height}=>resized_scene_edit=candidate.resize_output_to_source(scene,item,raw_width,raw_height)?,
                    Command::ConfigureVideo {canvas,output}=>candidate.profiles.configure_video(canvas,output)?,
                    Command::SetVideo {
                        width,
                        height,
                        fps_num,
                        fps_den,
                    } => {
                        candidate.profiles.set_video(VideoSettings {
                            width,
                            height,
                            fps_num,
                            fps_den,
                        })?;
                    }
                    Command::SetTransition { kind, duration_ms } => {
                        candidate.transition = TransitionSelection::legacy(kind, duration_ms)?;
                    }
                    Command::SelectTransition { selection } => candidate.transition = selection,
                    Command::AddTransitionPreset { name, kind } => {
                        if candidate.transition_presets.len() >= MAX_TRANSITION_PRESETS {
                            return Err(Error::Limit("transition preset catalog limit exceeded"));
                        }
                        let id = candidate.issue_transition_preset()?;
                        candidate
                            .transition_presets
                            .push(TransitionPreset::new(id, name, kind)?);
                        created_transition_preset = Some(id);
                    }
                    Command::ConfigureTransitionPreset { id, name, kind } => {
                        candidate.configure_preset(id, name, kind)?
                    }
                    Command::RenameTransitionPreset { id, name } => {
                        let kind = candidate
                            .transition_preset(id)
                            .ok_or(Error::TransitionPresetNotFound(id))?
                            .kind();
                        candidate.configure_preset(id, name, kind)?;
                    }
                    Command::SetTransitionPresetKind { id, kind } => {
                        let name = candidate
                            .transition_preset(id)
                            .ok_or(Error::TransitionPresetNotFound(id))?
                            .name()
                            .to_owned();
                        candidate.configure_preset(id, name, kind)?;
                    }
                    Command::RemoveTransitionPreset { id } => {
                        candidate
                            .transition_preset(id)
                            .ok_or(Error::TransitionPresetNotFound(id))?;
                        if candidate.preset_referenced(id) {
                            return Err(Error::TransitionPresetReferenced(id));
                        }
                        candidate
                            .transition_presets
                            .retain(|preset| preset.id() != id);
                    }
                    Command::RemoveTransitionPresetAndReferences { id } => {
                        let fallback = candidate.transition_after_removing_preset(id)?;
                        let reference = TransitionRef::Preset(id);
                        candidate
                            .quick_transitions
                            .retain(|row| row.config().reference() != reference);
                        for scene in &mut candidate.scenes {
                            if scene
                                .transition_override
                                .as_ref()
                                .is_some_and(|selection| selection.reference() == reference)
                            {
                                scene.configure_transition(None, scene.transition_duration_ms())?;
                            }
                        }
                        candidate
                            .transition_presets
                            .retain(|preset| preset.id() != id);
                        candidate.transition =
                            TransitionSelection::new(fallback, candidate.transition.duration_ms())?;
                    }
                    Command::AddQuickTransitionSelection {
                        selection,
                        fade_to_black,
                    } => {
                        if candidate.quick_transitions.len() >= MAX_QUICK_TRANSITIONS {
                            return Err(Error::Limit("quick transition row limit exceeded"));
                        }
                        let id = candidate.issue_quick()?;
                        candidate
                            .quick_transitions
                            .push(QuickTransition::from_selection(
                                id,
                                selection,
                                fade_to_black,
                            )?);
                    }
                    Command::UpdateQuickTransitionSelection {
                        id,
                        selection,
                        fade_to_black,
                    } => {
                        let index = candidate
                            .quick_transitions
                            .iter()
                            .position(|row| row.id() == id)
                            .ok_or(Error::QuickTransitionNotFound(id))?;
                        candidate.quick_transitions[index] =
                            QuickTransition::from_selection(id, selection, fade_to_black)?;
                    }
                    Command::ConfigureSceneTransitionSelection {
                        scene,
                        reference,
                        duration_ms,
                    } => candidate
                        .scene_mut(scene)?
                        .configure_transition(reference, duration_ms)?,
                    Command::AddQuickTransition {
                        kind,
                        duration_ms,
                        fade_to_black,
                    } => {
                        if candidate.quick_transitions.len() >= MAX_QUICK_TRANSITIONS {
                            return Err(Error::Limit("quick transition row limit exceeded"));
                        }
                        let id = candidate.issue_quick()?;
                        candidate.quick_transitions.push(QuickTransition::try_new(
                            id,
                            kind,
                            duration_ms,
                            fade_to_black,
                        )?);
                    }
                    Command::UpdateQuickTransition {
                        id,
                        kind,
                        duration_ms,
                        fade_to_black,
                    } => {
                        let index = candidate
                            .quick_transitions
                            .iter()
                            .position(|row| row.id() == id)
                            .ok_or(Error::QuickTransitionNotFound(id))?;
                        candidate.quick_transitions[index] =
                            QuickTransition::try_new(id, kind, duration_ms, fade_to_black)?;
                    }
                    Command::RemoveQuickTransition { id } => {
                        let index = candidate
                            .quick_transitions
                            .iter()
                            .position(|row| row.id() == id)
                            .ok_or(Error::QuickTransitionNotFound(id))?;
                        candidate.quick_transitions.remove(index);
                    }
                    Command::MoveQuickTransition { id, index } => {
                        let old = candidate
                            .quick_transitions
                            .iter()
                            .position(|row| row.id() == id)
                            .ok_or(Error::QuickTransitionNotFound(id))?;
                        if index >= candidate.quick_transitions.len() {
                            return Err(Error::Invalid(
                                "quick transition order index out of range",
                            ));
                        }
                        let row = candidate.quick_transitions.remove(old);
                        candidate.quick_transitions.insert(index, row);
                    }
                    Command::ReplaceQuickTransitions { rows } => {
                        if rows.len() > MAX_QUICK_TRANSITIONS {
                            return Err(Error::Limit("quick transition row limit exceeded"));
                        }
                        let next = rows.iter().map(|row| row.id().get() + 1).max().unwrap_or(0);
                        candidate.next_quick_id = candidate.next_quick_id.max(next);
                        // Retained snapshots count bounded rows, not arbitrary
                        // spare capacity supplied by a typed command caller.
                        candidate.quick_transitions = rows.into_boxed_slice().into_vec();
                    }
                    Command::SetSceneTransition { scene, config } => {
                        let target = candidate.scene_mut(scene)?;
                        let duration_ms = config
                            .as_ref()
                            .map(TransitionConfig::duration_ms)
                            .unwrap_or_else(|| target.transition_duration_ms());
                        target.configure_transition(
                            config
                                .as_ref()
                                .map(|config| TransitionRef::builtin(config.kind()))
                                .transpose()?,
                            duration_ms,
                        )?;
                    }
                    Command::ConfigureSceneTransition {
                        scene,
                        kind,
                        duration_ms,
                    } => candidate.scene_mut(scene)?.configure_transition(
                        kind.map(TransitionRef::builtin).transpose()?,
                        duration_ms,
                    )?,
                    Command::Undo | Command::Redo => unreachable!(),
                }
                0
            }
        };
        // Representation-only scale materialization must not create a Scene
        // history entry. Resize reports its real selected-Fit edit separately.
        let meaningful_scene_edit=if source_output_resize{resized_scene_edit}else{candidate.collection!=self.current.collection};
        candidate.next_id=candidate.next_id.max(self.high_water);
        if history==0 {
            if !source_output_resize{candidate.prepare_scale_anchors(Some(&self.current))?;}
            let canvas_changed=candidate.rebase_scene_canvas()?;
            candidate.normalize_groups(if canvas_changed{None}else{Some(&self.current)})?;
        }
        candidate.next_quick_id = candidate.next_quick_id.max(self.quick_high_water);
        candidate.next_transition_preset_id = candidate
            .next_transition_preset_id
            .max(self.preset_high_water);
        if candidate == *self.current {
            if !gate(){return Ok(None);}
            if let Some(overrides)=image_override {self.image_refresh_overrides=overrides;}
            if let Some((source,settings))=slideshow_override {if let Some((_,old))=self.slideshow_refresh_overrides.iter_mut().find(|(id,_)|*id==source){*old=settings;}}
            if history!=0{self.consume_scene_history(history>0,history_consumed);}
            return Ok(Some(Change {
                changed: false,
                revision: candidate.revision,
                created,
                created_transition_preset,
            }));
        }
        candidate.revision = self
            .current
            .revision
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        let document_changed = candidate.collection != self.current.collection
            || candidate.profiles != self.current.profiles;
        let document_revision = if document_changed {
            self.document_revision
                .checked_add(1)
                .ok_or(Error::IdentityExhausted)?
        } else {
            self.document_revision
        };
        candidate.validate()?;
        let source_definitions=self.source_definitions.prepare(Some(self.current.as_ref()),&candidate)?;
        let needs_history=history!=0||scene_history&&meaningful_scene_edit;
        // Prepare an owned bounded Arc-only destination before acceptance. A
        // declined gate does not change even the live stack's storage/capacity.
        let history_destination=if needs_history {
            let source=if history==-1{&self.redo}else{&self.undo};
            Some(prepare_history_push(source,Arc::new(self.current.collection.clone()))?)
        }else{None};
        let candidate=Arc::new(candidate);
        if !gate(){return Ok(None);}
        if let Some(overrides)=image_override {self.image_refresh_overrides=overrides;}
        if let Some((source,settings))=slideshow_override {if let Some((_,old))=self.slideshow_refresh_overrides.iter_mut().find(|(id,_)|*id==source){*old=settings;}}
        match history {
            -1 => {
                self.consume_scene_history(false,history_consumed);
                self.redo=history_destination.expect("prepared history destination");
            }
            1 => {
                self.consume_scene_history(true,history_consumed);
                self.undo=history_destination.expect("prepared history destination");
            }
            _ if scene_history&&meaningful_scene_edit => {
                self.undo=history_destination.expect("prepared history destination");
                self.redo.clear();
            }
            _ => {}
        }
        self.high_water = self.high_water.max(candidate.next_id);
        self.quick_high_water = self.quick_high_water.max(candidate.next_quick_id);
        self.preset_high_water = self
            .preset_high_water
            .max(candidate.next_transition_preset_id);
        self.source_definitions=source_definitions;
        self.current = candidate;
        self.document_revision = document_revision;
        if self.current.scene(self.preview).is_none() {
            self.preview = self.current.scenes[0].id;
        }
        if self.current.scene(self.program).is_none() {
            self.program = self.current.scenes[0].id;
        }
        Ok(Some(Change {
            changed: true,
            revision: self.current.revision,
            created,
            created_transition_preset,
        }))
    }
}
fn prepare_history_push(history:&VecDeque<Arc<Collection>>,value:Arc<Collection>)->Result<VecDeque<Arc<Collection>>,Error>{
    if history.len()>HISTORY_LIMIT{return Err(Error::Limit("scene history budget"));}
    let mut candidate=VecDeque::new();candidate.try_reserve(HISTORY_LIMIT).map_err(|_|Error::Limit("Cannot allocate bounded scene history."))?;
    candidate.extend(history.iter().cloned());push_history(&mut candidate,value);Ok(candidate)
}
fn push_history(history: &mut VecDeque<Arc<Collection>>, value: Arc<Collection>) {
    if history.len() == HISTORY_LIMIT {
        history.pop_front();
    }
    history.push_back(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn language_and_complete_preferences_are_validated_global_changes_outside_scene_history() {
        let mut session = Session::new();
        let initial = session.snapshot();
        assert_eq!(
            (
                session.document_revision(),
                session.saved_document_revision()
            ),
            (0, Some(0))
        );
        for language in ["", "x", "bad_locale", "en--US", "uk UA", "123"] {
            assert!(
                session
                    .apply(Command::SetLanguage {
                        language: language.into()
                    })
                    .is_err()
            );
            assert!(Arc::ptr_eq(&initial, &session.snapshot()));
        }
        assert!(
            !session
                .apply(Command::SetLanguage {
                    language: "en-US".into()
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&initial, &session.snapshot()));
        session
            .apply(Command::SetLanguage {
                language: "uk-UA".into(),
            })
            .unwrap();
        assert!(session.is_dirty());
        assert!(!session.is_document_dirty());
        assert!(!session.can_undo());
        let mut preferences =
            UserPreferences::new(AudioBuffering::new(137, true).unwrap()).unwrap();
        preferences.set_language("de-DE".into()).unwrap();
        preferences
            .set_plugin_enabled(AUDIO_PAN_PLUGIN.into(), true)
            .unwrap();
        session
            .apply(Command::SetUserPreferences {
                preferences: preferences.clone(),
            })
            .unwrap();
        assert_eq!(session.snapshot().preferences(), &preferences);
        assert_eq!(
            session.snapshot().audio().buffering(),
            preferences.audio_buffering()
        );
        assert_eq!(session.document_revision(), 0);
        assert!(!session.is_document_dirty());
        assert!(!session.can_undo());
        let before = session.snapshot();
        assert!(
            !session
                .apply(Command::SetUserPreferences {
                    preferences: preferences.clone()
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        assert!(matches!(
            session.apply_at(Some(0), Command::SetUserPreferences { preferences }),
            Err(Error::Conflict { .. })
        ));
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        session
            .apply(Command::AddScene {
                name: "Scene history".into(),
            })
            .unwrap();
        session
            .apply(Command::SetLanguage {
                language: "es-ES".into(),
            })
            .unwrap();
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().scenes().len(), 1);
        assert_eq!(session.snapshot().preferences().language(), "es-ES");
        assert!(session.can_redo());
        let mut preferences = session.snapshot().preferences().clone();
        preferences.set_language("fr-FR".into()).unwrap();
        session
            .apply(Command::SetUserPreferences { preferences })
            .unwrap();
        assert!(session.can_redo());
        session.apply(Command::Redo).unwrap();
        assert_eq!(session.snapshot().scenes().len(), 2);
        assert_eq!(session.snapshot().preferences().language(), "fr-FR");
        assert!(
            session
                .snapshot()
                .preferences()
                .plugin_enabled(AUDIO_PAN_PLUGIN)
        );
        assert_eq!(session.document_revision(), 3);
    }
    #[test]
    fn document_dirty_tracks_collection_and_profile_changes_with_exact_full_save_receipts() {
        let mut session = Session::new();
        let identity = session.identity();
        session
            .apply(Command::SetLanguage {
                language: "de-DE".into(),
            })
            .unwrap();
        let preferences_revision = session.snapshot().revision();
        assert!(session.is_dirty());
        assert!(!session.is_document_dirty());
        session
            .apply(Command::Profile {
                command: ProfileCommand::New {
                    name: "Other profile".into(),
                },
            })
            .unwrap();
        assert_eq!(session.document_revision(), 1);
        assert!(session.is_document_dirty());
        assert!(!session.mark_saved(identity, preferences_revision));
        assert_eq!(session.saved_document_revision(), Some(0));
        assert!(session.mark_saved(identity, session.snapshot().revision()));
        assert!(!session.is_dirty());
        assert!(!session.is_document_dirty());
        assert_eq!(session.saved_document_revision(), Some(1));
        let buffering = session
            .snapshot()
            .audio()
            .with_buffering(AudioBuffering::low_latency())
            .unwrap();
        session
            .apply(Command::SetAudioSettings {
                settings: buffering,
            })
            .unwrap();
        assert!(session.is_dirty());
        assert!(!session.is_document_dirty());
        assert_eq!(session.document_revision(), 1);
        let format = AudioSettings::new(44100, SpeakerLayout::Mono)
            .unwrap()
            .with_buffering(buffering.buffering())
            .unwrap();
        session
            .apply(Command::SetAudioSettings { settings: format })
            .unwrap();
        assert_eq!(session.document_revision(), 2);
        assert!(session.is_document_dirty());
        let before = session.snapshot();
        assert!(
            session
                .apply(Command::SetVideo {
                    width: 0,
                    height: 1080,
                    fps_num: 30,
                    fps_den: 1
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        assert_eq!(session.document_revision(), 2);
        assert!(!session.mark_saved(identity + 1, before.revision()));
        assert!(session.mark_saved(identity, before.revision()));
        session
            .apply(Command::RenameCollection {
                name: "Document edit".into(),
            })
            .unwrap();
        let document_receipt = session.snapshot().revision();
        session
            .apply(Command::SetLanguage {
                language: "uk-UA".into(),
            })
            .unwrap();
        assert_eq!(session.document_revision(), 3);
        assert!(
            !session.mark_saved(identity, document_receipt),
            "even an equal-document receipt cannot mark a newer full snapshot saved"
        );
        assert_eq!(session.saved_document_revision(), Some(2));
        assert!(session.is_document_dirty());
        assert!(session.mark_saved(identity, session.snapshot().revision()));
        assert_eq!(session.saved_document_revision(), Some(3));
        assert!(!session.is_document_dirty());
        assert!(!session.is_dirty());
        session.close();
        assert!(!session.mark_saved(identity, session.snapshot().revision()));
    }
    #[test]
    fn document_revision_exhaustion_rejects_transactionally_but_allows_preference_only_changes() {
        let mut session = Session::new();
        session.document_revision = u64::MAX;
        session.saved_document_revision = Some(u64::MAX);
        session
            .apply(Command::SetLanguage {
                language: "es-ES".into(),
            })
            .unwrap();
        assert!(!session.is_document_dirty());
        assert_eq!(session.document_revision(), u64::MAX);
        let before = session.snapshot();
        let counters = (
            session.high_water,
            session.quick_high_water,
            session.preset_high_water,
        );
        for command in [
            Command::AddScene {
                name: "Cannot publish".into(),
            },
            Command::Profile {
                command: ProfileCommand::New {
                    name: "Cannot publish".into(),
                },
            },
        ] {
            assert_eq!(
                session.apply(command).unwrap_err(),
                Error::IdentityExhausted
            );
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
            assert!(!session.can_undo());
            assert!(!session.can_redo());
            assert_eq!(session.document_revision(), u64::MAX);
            assert_eq!(
                counters,
                (
                    session.high_water,
                    session.quick_high_water,
                    session.preset_high_water
                )
            );
        }
    }
    #[test]
    fn adopted_document_restores_imported_scopes_and_selections_with_current_saved_preferences() {
        let mut owner = Session::new();
        owner
            .apply(Command::Profile {
                command: ProfileCommand::New {
                    name: "Owner profile".into(),
                },
            })
            .unwrap();
        let mut preferences = UserPreferences::new(AudioBuffering::low_latency()).unwrap();
        preferences.set_language("uk-UA".into()).unwrap();
        preferences
            .set_plugin_enabled(AUDIO_PAN_PLUGIN.into(), true)
            .unwrap();
        owner
            .apply(Command::SetUserPreferences {
                preferences: preferences.clone(),
            })
            .unwrap();
        let owner_before = owner.snapshot();
        let mut imported = Session::new();
        let first = imported.preview();
        let second = imported
            .apply(Command::AddScene {
                name: "Imported scene".into(),
            })
            .unwrap()
            .created
            .unwrap();
        imported
            .apply(Command::Profile {
                command: ProfileCommand::New {
                    name: "Imported profile".into(),
                },
            })
            .unwrap();
        imported
            .apply(Command::SetAudioSettings {
                settings: AudioSettings::new(44100, SpeakerLayout::Mono).unwrap(),
            })
            .unwrap();
        imported
            .apply(Command::SetLanguage {
                language: "es-ES".into(),
            })
            .unwrap();
        imported.select_preview(second).unwrap();
        imported.select_program(first).unwrap();
        // This fixture represents a saved native load; authored construction
        // commands alone are not evidence that a document has been saved.
        assert!(imported.mark_saved(imported.identity(), imported.snapshot().revision()));
        let imported_before = imported.snapshot();
        let imported_identity = imported.identity();
        let candidate = owner.adopt_document_preferences(imported).unwrap();
        let restored = candidate.snapshot();
        assert_ne!(candidate.identity(), owner.identity());
        assert_ne!(candidate.identity(), imported_identity);
        assert_eq!(restored.collection(), imported_before.collection());
        assert_eq!(restored.profiles(), imported_before.profiles());
        assert_eq!(restored.preferences(), &preferences);
        assert_eq!((candidate.preview(), candidate.program()), (second, first));
        assert_eq!(restored.revision(), 0);
        assert_eq!(
            (
                candidate.document_revision(),
                candidate.saved_document_revision()
            ),
            (0, Some(0))
        );
        assert!(!candidate.is_dirty());
        assert!(!candidate.is_document_dirty());
        assert!(!candidate.can_undo());
        assert!(Arc::ptr_eq(&owner_before, &owner.snapshot()));
        let mut closed = Session::new();
        closed.close();
        assert!(matches!(
            owner.adopt_document_preferences(closed),
            Err(Error::Closed)
        ));
        assert!(Arc::ptr_eq(&owner_before, &owner.snapshot()));
        owner.close();
        assert!(matches!(
            owner.adopt_document_preferences(Session::new()),
            Err(Error::Closed)
        ));
    }
    #[test]
    fn adopted_native_document_preserves_unsaved_scene_and_profile_flags_without_fake_history() {
        let mut owner = Session::new();
        owner
            .apply(Command::SetLanguage {
                language: "uk-UA".into(),
            })
            .unwrap();
        let owner_before = owner.snapshot();
        for profile_only in [false, true] {
            let mut imported = Session::new();
            assert!(imported.mark_saved(imported.identity(), imported.snapshot().revision()));
            if profile_only {
                imported
                    .apply(Command::Profile {
                        command: ProfileCommand::New {
                            name: "Unsaved profile".into(),
                        },
                    })
                    .unwrap();
                assert!(imported.is_dirty());
                assert!(imported.is_document_dirty());
            } else {
                // Storage's semantic-migration/recovery receipt sets these
                // unsaved flags while preserving the loaded authored graph.
                imported = Session::from_snapshot((*imported.snapshot()).clone(), false).unwrap();
                assert!(imported.is_dirty());
                assert!(imported.is_document_dirty());
            }
            let graph = imported.snapshot();
            let was_scene_dirty = imported.is_dirty();
            let adopted = owner.adopt_document_preferences(imported).unwrap();
            assert_eq!(adopted.is_dirty(), was_scene_dirty);
            assert!(adopted.is_document_dirty());
            assert_eq!(adopted.document_revision(), 0);
            assert_eq!(adopted.saved_document_revision(), None);
            assert_eq!(adopted.snapshot().collection(), graph.collection());
            assert_eq!(adopted.snapshot().profiles(), graph.profiles());
            assert_eq!(adopted.snapshot().preferences(), owner_before.preferences());
            assert!(!adopted.can_undo());
            assert!(!adopted.can_redo());
        }
        // The legacy revision flag also tracks preference changes, whereas
        // native-document cleanliness excludes those separate user settings.
        let mut preferences_only = Session::new();
        assert!(preferences_only.mark_saved(
            preferences_only.identity(),
            preferences_only.snapshot().revision()
        ));
        preferences_only
            .apply(Command::SetLanguage {
                language: "es-ES".into(),
            })
            .unwrap();
        assert!(preferences_only.is_dirty());
        assert!(!preferences_only.is_document_dirty());
        let adopted = owner.adopt_document_preferences(preferences_only).unwrap();
        assert!(adopted.is_dirty());
        assert!(!adopted.is_document_dirty());
        assert_eq!(adopted.saved_document_revision(), Some(0));
        assert!(!adopted.can_undo());
        assert!(Arc::ptr_eq(&owner_before, &owner.snapshot()));
    }
    #[test]
    fn document_counters_are_session_local_and_unsaved_imports_keep_their_dirty_state() {
        let snapshot = StudioSnapshot {
            revision: 37,
            ..StudioSnapshot::default()
        };
        let saved = Session::from_snapshot(snapshot.clone(), true).unwrap();
        assert_eq!(
            (saved.document_revision(), saved.saved_document_revision()),
            (0, Some(0))
        );
        assert!(!saved.is_dirty());
        assert!(!saved.is_document_dirty());
        let mut unsaved = Session::from_snapshot(snapshot, false).unwrap();
        assert_eq!(
            (
                unsaved.document_revision(),
                unsaved.saved_document_revision()
            ),
            (0, None)
        );
        assert!(unsaved.is_dirty());
        assert!(unsaved.is_document_dirty());
        unsaved
            .apply(Command::SetLanguage {
                language: "uk-UA".into(),
            })
            .unwrap();
        assert_eq!(unsaved.document_revision(), 0);
        assert!(unsaved.is_document_dirty());
    }
    #[test]
    fn image_rebind_is_atomic_across_shared_items_and_rejects_type_or_crop_damage() {
        let mut session = Session::new();
        let scene = session.preview();
        let first = ImageAsset::new("C:/images/linked.png".into(), [1; 32], 4, 2).unwrap();
        let item = session
            .apply(Command::AddImageSource {
                scene,
                name: "Image".into(),
                asset: first.clone(),
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        let other = session
            .apply(Command::AddExistingSource { scene, source })
            .unwrap()
            .created
            .unwrap();
        let transform = Transform {
            width: 8.0,
            height: 2.0,
            crop: Crop {
                left: 1,
                right: 1,
                ..Default::default()
            },
            rotation_degrees: 90.0,
            ..Transform::full(4, 2)
        };
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: transform.clone(),
            })
            .unwrap();
        let next = ImageAsset::new("C:/images/linked.png".into(), [2; 32], 8, 4).unwrap();
        session
            .apply(Command::SetImageSource {
                source,
                asset: next.clone(),
            })
            .unwrap();
        let updated = session.snapshot();
        let items = updated.scene(scene).unwrap().items();
        assert_eq!(items[0].transform().width, 16.0);
        assert_eq!(items[0].transform().height, 4.0);
        assert_eq!(items[0].transform().crop, transform.crop);
        assert_eq!(items[1].id(), other);
        assert_eq!(items[1].transform(), &Transform::full(8, 4));
        assert_eq!(
            updated.source(source).unwrap().kind(),
            &SourceKind::Image {
                asset: next.clone()
            }
        );
        assert!(
            !session
                .apply(Command::SetImageSource {
                    source,
                    asset: next
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&updated, &session.snapshot()));
        let too_small = ImageAsset::new("C:/images/linked.png".into(), [3; 32], 1, 1).unwrap();
        assert!(
            session
                .apply(Command::SetImageSource {
                    source,
                    asset: too_small
                })
                .is_err()
        );
        assert!(
            session
                .apply(Command::SetColor {
                    source,
                    rgba: [0; 4]
                })
                .is_err()
        );
        assert!(
            session
                .apply(Command::SetColorSize {
                    source,
                    width: 2,
                    height: 2
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&updated, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        let reverted = session.snapshot();
        assert_eq!(
            reverted.source(source).unwrap().kind(),
            &SourceKind::Image { asset: first }
        );
        assert_eq!(
            reverted.scene(scene).unwrap().items()[0].transform(),
            &transform
        );
        assert_eq!(
            reverted.scene(scene).unwrap().items()[1].transform(),
            &Transform::full(4, 2)
        );
    }
    #[test]
    fn source_resize_roundtrip_at_minimum_extent_does_not_reject_valid_geometry() {
        let mut session = Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddColorSource {
                scene,
                name: "Resize".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        session
            .apply(Command::SetColorSize {
                source,
                width: 1,
                height: 1,
            })
            .unwrap();
        let minimum = Transform {
            width: 0.001,
            height: 0.001,
            ..Transform::full(1, 1)
        };
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: minimum.clone(),
            })
            .unwrap();
        session
            .apply(Command::SetColorSize {
                source,
                width: 115,
                height: 115,
            })
            .unwrap();
        session
            .apply(Command::SetColorSize {
                source,
                width: 1,
                height: 1,
            })
            .unwrap();
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0].transform(),
            &minimum
        );
    }
    #[test]
    fn existing_source_starts_at_native_size_with_independent_item_transform() {
        let mut session = Session::new();
        let scene = session.preview();
        let original = session
            .apply(Command::AddColorSource {
                scene,
                name: "Shared".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        session
            .apply(Command::SetColorSize {
                source,
                width: 40,
                height: 20,
            })
            .unwrap();
        let changed = Transform {
            rotation_degrees: 90.0,
            crop: Crop {
                left: 5,
                ..Crop::default()
            },
            ..Transform::full(80, 40)
        };
        session
            .apply(Command::SetTransform {
                scene,
                item: original,
                transform: changed.clone(),
            })
            .unwrap();
        let added = session
            .apply(Command::AddExistingSource { scene, source })
            .unwrap()
            .created
            .unwrap();
        let state = session.snapshot();
        let items = state.scene(scene).unwrap().items();
        assert_eq!(items[0].transform(), &changed);
        assert_eq!(items[1].id(), added);
        assert_eq!(items[1].transform(), &Transform::full(40, 20));
        assert_eq!(items[1].source(), items[0].source());
    }
    #[test]
    fn affine_edit_rejection_preserves_model_history_and_source_sizes() {
        let mut session = Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddColorSource {
                scene,
                name: "Geometry".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        let mut transform = Transform::full(1920, 1080);
        transform.rotation_degrees = 37.0;
        transform.flip_x = true;
        transform.crop.left = 100;
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: transform.clone(),
            })
            .unwrap();
        let before = session.snapshot();
        let mut invalid = transform.clone();
        invalid.crop.right = u32::MAX;
        assert!(
            session
                .apply(Command::SetTransform {
                    scene,
                    item,
                    transform: invalid
                })
                .is_err()
        );
        assert!(
            session
                .apply(Command::SetColorSize {
                    source,
                    width: 50,
                    height: 1080
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        session
            .apply(Command::TransformItem {
                scene,
                item,
                action: TransformAction::Rotate90Clockwise,
            })
            .unwrap();
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0].transform(),
            &transform
        );
        session
            .apply(Command::SetLocked {
                scene,
                item,
                locked: true,
            })
            .unwrap();
        let locked = session.snapshot();
        for action in [
            TransformAction::Rotate90Clockwise,
            TransformAction::Rotate90CounterClockwise,
            TransformAction::Rotate180,
            TransformAction::FlipHorizontal,
            TransformAction::FlipVertical,
        ] {
            assert_eq!(
                session
                    .apply(Command::TransformItem {
                        scene,
                        item,
                        action
                    })
                    .unwrap_err(),
                Error::Locked(item)
            );
            assert!(Arc::ptr_eq(&locked, &session.snapshot()));
        }
        session.apply(Command::Undo).unwrap();
        assert!(!session.snapshot().scene(scene).unwrap().items()[0].locked());
    }
    #[test]
    fn invalid_edit_preserves_snapshot_history_and_revision() {
        let mut s = Session::new();
        let before = s.snapshot();
        assert!(s.apply(Command::RemoveScene { scene: Id(1) }).is_err());
        assert!(
            s.apply(Command::AddColorSource {
                scene: Id(1),
                name: "Scene".into(),
                rgba: [255; 4]
            })
            .is_err()
        );
        assert!(
            s.apply(Command::AddScene {
                name: "Scene".into()
            })
            .is_err()
        );
        assert!(Arc::ptr_eq(&before, &s.snapshot()));
        assert!(!s.can_undo());
    }
    #[test]
    fn no_op_and_conflict_do_not_publish() {
        let mut s = Session::new();
        assert!(
            !s.apply(Command::RenameScene {
                scene: Id(1),
                name: "Scene".into()
            })
            .unwrap()
            .changed
        );
        assert!(matches!(
            s.apply_at(Some(9), Command::Undo),
            Err(Error::Conflict { .. })
        ));
        assert_eq!(s.snapshot().revision(), 0);
        assert!(!s.can_undo());
    }
    #[test]
    fn undo_redo_never_reuse_identity_or_revision() {
        let mut s = Session::new();
        let first = s
            .apply(Command::AddScene { name: "Two".into() })
            .unwrap()
            .created
            .unwrap();
        s.apply(Command::Undo).unwrap();
        assert_eq!(s.snapshot().revision(), 2);
        s.apply(Command::Redo).unwrap();
        assert_eq!(s.snapshot().scenes()[1].id(), first);
        s.apply(Command::Undo).unwrap();
        let second = s
            .apply(Command::AddScene {
                name: "Three".into(),
            })
            .unwrap()
            .created
            .unwrap();
        assert!(second.get() > first.get());
        assert!(!s.can_redo());
    }
    #[test]
    fn source_reference_lock_and_nonfinite_validation() {
        let mut s = Session::new();
        let scene = s.preview();
        let item = s
            .apply(Command::AddColorSource {
                scene,
                name: "Red".into(),
                rgba: [255, 0, 0, 255],
            })
            .unwrap()
            .created
            .unwrap();
        s.apply(Command::SetLocked {
            scene,
            item,
            locked: true,
        })
        .unwrap();
        let before = s.snapshot();
        assert_eq!(
            s.apply(Command::RemoveItem { scene, item }).unwrap_err(),
            Error::Locked(item)
        );
        assert!(
            s.apply(Command::SetAudio {
                source: before.sources()[0].id(),
                gain: f32::NAN,
                muted: false
            })
            .is_err()
        );
        assert!(Arc::ptr_eq(&before, &s.snapshot()));
        let duplicate = s
            .apply(Command::DuplicateScene {
                scene,
                name: "Copy".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let current = s.snapshot();
        assert_eq!(
            current.scene(duplicate).unwrap().items()[0].source(),
            current.sources()[0].id()
        );
        assert_ne!(current.scene(duplicate).unwrap().items()[0].id(), item);
    }
    #[test]
    fn stale_save_and_closed_session_cannot_publish() {
        let mut s = Session::new();
        let old = s.identity();
        s.apply(Command::AddScene { name: "Two".into() }).unwrap();
        assert!(!s.mark_saved(old, 0));
        assert!(s.is_dirty());
        assert!(s.mark_saved(old, 1));
        assert!(!s.is_dirty());
        s.close();
        assert_eq!(s.apply(Command::Undo).unwrap_err(), Error::Closed);
        assert!(!s.mark_saved(old, 1));
        assert!(s.select_preview(Id(1)).is_err());
    }
    #[test]
    fn deleting_last_reference_releases_source_and_undo_restores_it() {
        let mut s = Session::new();
        let scene = s.preview();
        let item = s
            .apply(Command::AddColorSource {
                scene,
                name: "Red".into(),
                rgba: [255, 0, 0, 255],
            })
            .unwrap()
            .created
            .unwrap();
        let copy = s
            .apply(Command::DuplicateScene {
                scene,
                name: "Copy".into(),
            })
            .unwrap()
            .created
            .unwrap();
        s.apply(Command::RemoveItem { scene, item }).unwrap();
        assert_eq!(s.snapshot().sources().len(), 1);
        s.apply(Command::RemoveScene { scene: copy }).unwrap();
        assert!(s.snapshot().sources().is_empty());
        s.apply(Command::Undo).unwrap();
        assert_eq!(s.snapshot().sources().len(), 1);
        s.apply(Command::Redo).unwrap();
        assert!(
            s.apply(Command::AddColorSource {
                scene,
                name: "Red".into(),
                rgba: [255, 0, 0, 255]
            })
            .is_ok()
        );
    }
    #[test]
    fn intrinsic_size_and_shared_transform_operations_follow_source_scale() {
        let mut s = Session::new();
        let scene = s.preview();
        let item = s
            .apply(Command::AddColorSource {
                scene,
                name: "Square".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        let source = s.snapshot().sources()[0].id();
        s.apply(Command::SetColorSize {
            source,
            width: 100,
            height: 100,
        })
        .unwrap();
        s.apply(Command::TransformItem {
            scene,
            item,
            action: TransformAction::FitToCanvas,
        })
        .unwrap();
        let image = s.snapshot();
        let t = image.scene(scene).unwrap().items()[0].transform();
        assert_eq!((t.x, t.y, t.width, t.height), (420.0, 0.0, 1080.0, 1080.0));
        s.apply(Command::TransformItem {
            scene,
            item,
            action: TransformAction::Reset,
        })
        .unwrap();
        assert_eq!(
            s.snapshot().scene(scene).unwrap().items()[0].transform(),
            &Transform::full(100, 100)
        );
        s.apply(Command::SetColorSize {
            source,
            width: 200,
            height: 50,
        })
        .unwrap();
        assert_eq!(
            s.snapshot().scene(scene).unwrap().items()[0].transform(),
            &Transform::full(200, 50)
        );
    }
}

mod freeze_filter;
pub use freeze_filter::{EXELDRO_FREEZE_PLUGIN,FreezeAction,FreezeSettings};
mod photo_filter;
pub use photo_filter::PhotoFilterPreset;
mod video_filters;
mod filtered_dimensions;
pub use filtered_dimensions::VideoFilterMapping;
mod video_filter_preview;
pub use video_filter_preview::VideoFilterPreviewTarget;
pub use video_filters::{
    CRAFT_VIDEO_PLUGIN, ChromaKeyType, ColorKeyType, GaussianBlurDimensions, ImageMaskMethod, KeyMatch, MAX_VIDEO_FILTERS, MatteView,
    OBS_VIDEO_FILTERS_PLUGIN, PERSON_BACKGROUND_PLUGIN, PERSON_DEFAULT_INTERVAL_MS, SpillChannel, VideoEffect, VideoFilter,
    VideoFilterId,
};

#[cfg(test)]
mod color_properties_tests;
