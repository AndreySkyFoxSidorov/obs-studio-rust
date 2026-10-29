//! Bounded, versioned candidate decoding and recoverable file publication (A8).
use serde::{Deserialize, Serialize};
use skylive_core::{
    AudioSettings, Collection, Id, ProfileCatalog, Session, StudioSnapshot, UserPreferences,
    VideoSettings,
};
use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const CURRENT_SCHEMA: u32 = 9;
#[path = "storage/person_background_guard.rs"]
mod person_background_guard;
#[cfg(test)]
#[path = "storage/person_background_tests.rs"]
mod person_background_tests;
#[path = "storage/streaming_legacy_guard.rs"]
mod streaming_legacy_guard;
#[cfg(test)]
#[path = "storage/streaming_tests.rs"]
mod streaming_tests;
pub const MAX_MIGRATION_REPORT_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct SceneSourceMigration {
    pub scene: Id,
    pub source: Id,
    pub name: String,
    pub filter_count: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct MigrationReport {
    pub from_schema: u32,
    pub to_schema: u32,
    pub scene_source_filters: Vec<SceneSourceMigration>,
}
impl MigrationReport {
    pub fn has_semantic_changes(&self) -> bool {
        !self.scene_source_filters.is_empty()
    }
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.scene_source_filters.capacity() * std::mem::size_of::<SceneSourceMigration>()
            + self
                .scene_source_filters
                .iter()
                .map(|entry| entry.name.capacity())
                .sum::<usize>()
    }
    fn for_snapshot(schema: u32, snapshot: &StudioSnapshot) -> Result<Self, StorageError> {
        let mut changes = Vec::new();
        if schema < 4 {
            // Scene-owner semantics changed at4, not every later additive schema.
            for source in snapshot.sources() {
                if let skylive_core::SourceKind::Scene { scene } = source.kind() {
                    if !source.video_filters().is_empty() {
                        changes.push(SceneSourceMigration {
                            scene: *scene,
                            source: source.id(),
                            name: source.name().to_owned().into_boxed_str().into_string(),
                            filter_count: source.video_filters().len(),
                        });
                    }
                }
            }
        }
        let report = Self {
            from_schema: schema,
            to_schema: CURRENT_SCHEMA,
            scene_source_filters: changes.into_boxed_slice().into_vec(),
        };
        if report.scene_source_filters.len() > skylive_core::MAX_SCENES
            || report.retained_bytes() > MAX_MIGRATION_REPORT_BYTES
        {
            return Err(rejected(
                "native migration report exceeds bounded admission",
            ));
        }
        Ok(report)
    }
}
pub struct LoadedDocument {
    pub session: Session,
    pub migration: MigrationReport,
}
static TEMP_SERIAL: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    BeforePublication,
    AfterPublication,
}
#[derive(Debug)]
pub struct StorageError {
    pub phase: Phase,
    pub detail: String,
    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}
impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.phase, self.detail)
    }
}
impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}
fn before(error: impl std::error::Error + Send + Sync + 'static) -> StorageError {
    StorageError {
        phase: Phase::BeforePublication,
        detail: error.to_string(),
        cause: Some(Box::new(error)),
    }
}
fn after(error: impl std::error::Error + Send + Sync + 'static) -> StorageError {
    StorageError {
        phase: Phase::AfterPublication,
        detail: error.to_string(),
        cause: Some(Box::new(error)),
    }
}
pub(crate) fn slideshow_refresh_error(detail:impl Into<String>)->StorageError{rejected(detail)}
fn rejected(detail: impl Into<String>) -> StorageError {
    StorageError {
        phase: Phase::BeforePublication,
        detail: detail.into(),
        cause: None,
    }
}

#[cfg(test)]
#[path = "storage/filter_migration_tests.rs"]
mod filter_migration_tests;

#[cfg(test)]
#[path = "storage/scene_owner_migration_tests.rs"]
mod scene_owner_migration_tests;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema: u32,
    revision: u64,
    collection: Collection,
    profiles: ProfileCatalog,
    preferences: UserPreferences,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selection: Option<Selection>,
}
#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct Selection {
    preview: Id,
    program: Id,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyEnvelope {
    schema: u32,
    collection: LegacyCollection,
    #[serde(default)]
    selection: Option<Selection>,
}
// Explicit DTO retains strict duplicate/unknown-field rejection before migration.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCollection {
    name: String,
    revision: u64,
    next_id: u64,
    video: VideoSettings,
    #[serde(default)]
    audio: AudioSettings,
    #[serde(default)]
    transition: skylive_core::TransitionSelection,
    #[serde(default)]
    transition_presets: Vec<skylive_core::TransitionPreset>,
    #[serde(default = "one")]
    next_transition_preset_id: u32,
    #[serde(default = "legacy_quick")]
    quick_transitions: Vec<skylive_core::QuickTransition>,
    #[serde(default = "legacy_next_quick")]
    next_quick_id: u32,
    scenes: Vec<skylive_core::Scene>,
    sources: Vec<skylive_core::Source>,
    #[serde(default)]
    global_audio: Vec<skylive_core::GlobalAudioSource>,
}
fn one() -> u32 {
    1
}
fn legacy_quick() -> Vec<skylive_core::QuickTransition> {
    StudioSnapshot::default().quick_transitions().to_vec()
}
fn legacy_next_quick() -> u32 {
    4
}
#[derive(Deserialize)]
struct Schema {
    schema: u32,
}

pub fn decode(bytes: &[u8], saved: bool) -> Result<Session, StorageError> {
    decode_with_report(bytes, saved).map(|loaded| loaded.session)
}
/// Complete user-facing load receipt; migration metadata is not authored state.
pub fn decode_with_report(bytes: &[u8], saved: bool) -> Result<LoadedDocument, StorageError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(rejected("document exceeds 16 MiB"));
    }
    let schema: Schema = serde_json::from_slice(bytes).map_err(before)?;
    let (snapshot, selection) = match schema.schema {
        1 => {
            let old: LegacyEnvelope = serde_json::from_slice(bytes).map_err(before)?;
            if old.schema != 1 {
                return Err(rejected("invalid legacy schema"));
            }
            let revision = old.collection.revision;
            let profiles =
                ProfileCatalog::from_legacy(old.collection.video.clone(), old.collection.audio)
                    .map_err(before)?;
            let preferences =
                UserPreferences::new(old.collection.audio.buffering()).map_err(before)?;
            let mut value = serde_json::to_value(old.collection).map_err(before)?;
            let fields = value
                .as_object_mut()
                .ok_or_else(|| rejected("invalid legacy collection"))?;
            fields.remove("revision");
            fields.remove("video");
            fields.remove("audio");
            let collection: Collection = serde_json::from_value(value).map_err(before)?;
            if collection.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::MediaSource{media} if media.speed_percent()!=100)){return Err(rejected("Media playback speed requires native schema9"));}
            if collection.has_scene_canvas_metadata(){return Err(rejected("Scene canvas interpretation and stable scale anchors require native schema9"));}
            if collection.has_image_source_metadata(){return Err(rejected("Image Source properties require native schema9"));}
            if collection.sources().iter().any(|source|source.video_filters().iter().any(|filter|filter.person_interval_ms()!=0)){return Err(rejected("Person inference interval requires native schema9"));}
            if collection.has_text_gradient_metadata(){return Err(rejected("Text gradient requires native schema9"));}
            if collection.has_text_antialiasing_metadata(){return Err(rejected("Text antialiasing and decorations require native schema9"));}
            if collection.has_text_outline_metadata(){return Err(rejected("Text outline requires native schema9"));}
            if collection.has_text_font_metadata(){return Err(rejected("Text font files require native schema9"));}
            if collection.has_text_file_metadata(){return Err(rejected("Text file sources require native schema9"));}
            if collection.has_text_case_metadata(){return Err(rejected("Text case transform requires native schema9"));}
            let collection = collection.upgrade_legacy_filter_order().map_err(before)?;
            (
                StudioSnapshot::from_parts(revision, collection, profiles, preferences)
                    .map_err(before)?,
                old.selection,
            )
        }
        2..=CURRENT_SCHEMA => {
            if schema.schema < 5 {
                streaming_legacy_guard::check(bytes).map_err(before)?;
            }
            let envelope: Envelope = serde_json::from_slice(bytes).map_err(before)?;
            if schema.schema<9&&envelope.collection.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::MediaSource{media} if media.speed_percent()!=100)){return Err(rejected("Media playback speed requires native schema9"));}
            // Inspect authored metadata BEFORE from_parts adopts missing legacy
            // context or materializes existing anchors into the selected canvas.
            if schema.schema<9&&envelope.collection.has_scene_canvas_metadata(){return Err(rejected("Scene canvas interpretation and stable scale anchors require native schema9"));}
            if schema.schema<9&&envelope.collection.has_image_source_metadata(){return Err(rejected("Image Source properties require native schema9"));}
            if schema.schema<9&&envelope.collection.sources().iter().any(|source|source.video_filters().iter().any(|filter|filter.person_interval_ms()!=0)){return Err(rejected("Person inference interval requires native schema9"));}
            if schema.schema<9&&envelope.collection.has_text_gradient_metadata(){return Err(rejected("Text gradient requires native schema9"));}
            if schema.schema<9&&envelope.collection.has_text_antialiasing_metadata(){return Err(rejected("Text antialiasing and decorations require native schema9"));}
            if schema.schema<9&&envelope.collection.has_text_outline_metadata(){return Err(rejected("Text outline requires native schema9"));}
            if schema.schema<9&&envelope.collection.has_text_font_metadata(){return Err(rejected("Text font files require native schema9"));}
            if schema.schema<9&&envelope.collection.has_text_file_metadata(){return Err(rejected("Text file sources require native schema9"));}
            if schema.schema<9&&envelope.collection.has_text_case_metadata(){return Err(rejected("Text case transform requires native schema9"));}
            let collection = if schema.schema == 2 {
                envelope
                    .collection
                    .upgrade_legacy_filter_order()
                    .map_err(before)?
            } else {
                envelope.collection.validate().map_err(before)?;
                envelope.collection
            };
            (
                StudioSnapshot::from_parts(
                    envelope.revision,
                    collection,
                    envelope.profiles,
                    envelope.preferences,
                )
                .map_err(before)?,
                envelope.selection,
            )
        }
        other => return Err(rejected(format!("unsupported schema {other}"))),
    };
    if schema.schema < 6 {
        for source in snapshot.sources() {
            if source.audio_filters().iter().any(|filter| {
                matches!(
                    filter,
                    skylive_core::AudioFilter::ObsLimiter { .. }
                        | skylive_core::AudioFilter::ObsCompressor { .. }
                        | skylive_core::AudioFilter::ObsExpander { .. }
                )
            }) {
                return Err(rejected(format!(
                    "OBS dynamics requires native schema6 (source {})",
                    source.id().get()
                )));
            }
        }
    }
    if schema.schema < 7 && snapshot.sources().iter().any(|source| matches!(source.kind(), skylive_core::SourceKind::WindowCapture{binding} if matches!(binding.target(),skylive_core::WindowTarget::Windows{..}) && binding.method()!=skylive_core::WindowCaptureMethod::Gdi)) {
        return Err(rejected("WGC/automatic Windows window capture requires native schema7"));
    }
    if schema.schema<9 && snapshot.profiles().entries().iter().any(|profile|u64::from(profile.video().width())*u64::from(profile.video().height())>8_388_608) {
        return Err(rejected("expanded4K canvas dimensions require native schema9"));
    }
    if schema.schema<9 && snapshot.transition_presets().iter().any(|preset|matches!(preset.kind(),skylive_core::TransitionKind::LumaWipe{..})){return Err(rejected("Luma Wipe transitions require native schema9"));}
    if schema.schema<9&&snapshot.profiles().entries().iter().any(|profile|!profile.recording().audio_tracks().is_default()){return Err(rejected("recording track settings require native schema9"));}
    if schema.schema<9 && snapshot.profiles().entries().iter().any(|profile|!profile.streaming().reconnect().is_default()){return Err(rejected("stream reconnect settings require native schema9"));}
    if schema.schema<9 && snapshot.profiles().entries().iter().any(|profile|!profile.output_video_settings().is_default()) {
        return Err(rejected("independent output video settings require native schema9"));
    }
    if schema.schema<9&&!snapshot.preferences().preview_snapping().is_default(){return Err(rejected("Preview snapping settings require native schema9"));}
    if schema.schema < 9 && snapshot.preferences().appearance().theme().requires_extended_theme_format() {
        return Err(rejected("extended OBS theme variants require native schema9"));
    }
    if schema.schema < 9 && snapshot.sources().iter().any(|source| source.audio_properties().sync_offset_ms() != 0) {
        return Err(rejected("source audio sync offset requires native schema9"));
    }
    if schema.schema < 9 && snapshot.sources().iter().any(|source| !source.audio_properties().push().is_default()) {
        return Err(rejected("source push-to-mute/talk settings require native schema9"));
    }
    if schema.schema<9&&(snapshot.sources().iter().any(|source|!source.audio_properties().audio_tracks().is_all())
        ||snapshot.profiles().entries().iter().any(|profile|profile.streaming().audio_track()!=skylive_core::AudioTrackId::ONE
            ||profile.replay_buffer().audio_track()!=skylive_core::AudioTrackId::ONE)
        ||snapshot.preferences().ndi_output().audio_track()!=skylive_core::AudioTrackId::ONE){
        return Err(rejected("source/output audio track routing requires native schema9"));
    }
    if schema.schema<9&&snapshot.sources().iter().any(|source|!source.audio_hotkeys().is_empty()){return Err(rejected("Source mute shortcuts require native schema9."));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|!source.playback_hotkeys().is_empty()){return Err(rejected("source playback shortcuts require native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.audio_filters().iter().any(|filter|matches!(filter,skylive_core::AudioFilter::CraftDither{..}|skylive_core::AudioFilter::CraftRectifier{..}|skylive_core::AudioFilter::CraftDcRemoval{..}|skylive_core::AudioFilter::CraftLoFi{..}|skylive_core::AudioFilter::CraftSaturator{..}|skylive_core::AudioFilter::CraftModDelay{..}|skylive_core::AudioFilter::CraftFdnReverb{..}))){
        return Err(rejected("Craft Mod Delay/FDN Reverb requires native schema9"));
    }
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.audio_filters().iter().any(|filter|filter.sidechain_source().is_some())){
        return Err(rejected("OBS compressor sidechain requires native schema9"));
    }
    if schema.schema<9&&!snapshot.saved_projectors().is_empty(){return Err(rejected("saved native projector windows require native schema9"));}
    if schema.schema<9&&(!snapshot.preferences().projectors().is_default()||snapshot.scenes().iter().any(|scene|!scene.show_in_multiview())){return Err(rejected("projector defaults and scene Multiview visibility require native schema9"));}
    if schema.schema < 9 && snapshot.has_mixer_metadata() {
        return Err(rejected("mixer view metadata requires native schema9"));
    }
    if schema.schema < 9 && snapshot.preferences().has_setup_metadata() {
        return Err(rejected("setup preferences require native schema9"));
    }
    if schema.schema<9&&snapshot.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::ImageSlideShow{settings}if !settings.entries().is_empty()||!settings.entry_image_counts().is_empty())){return Err(rejected("slideshow file and folder entries require native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::ImageSlideShow{settings}if settings.randomize()||!settings.random_compatibility().is_default())){return Err(rejected("slideshow random playback requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::ImageSlideShow{settings}if !settings.mode().is_default())){return Err(rejected("slideshow manual mode requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::ImageSlideShow{settings}if !settings.canvas().is_default()||!settings.behavior().is_default())){return Err(rejected("slideshow canvas and visibility settings require native schema9"));}
    if schema.schema < 9 && snapshot.sources().iter().any(|source| matches!(source.kind(),skylive_core::SourceKind::ImageSlideShow {..})) {
        return Err(rejected("image slideshow requires native schema9"));
    }
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::GaussianBlur{..}))){return Err(rejected("Craft Gaussian Blur requires native schema9"));}
    if schema.schema < 9 && snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::Vignette {..}))) {
        return Err(rejected("Craft Vignette requires native schema9"));
    }
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::PhotoFilter{..}))){return Err(rejected("Craft Photo Filter requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::Sepia{..}))){return Err(rejected("Craft Sepia Photo Filter requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::Vibrance{..}))){return Err(rejected("Craft Vibrance requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::CraftTint{..}|skylive_core::VideoEffect::Tritone{..}|skylive_core::VideoEffect::CraftColorBalance{..}|skylive_core::VideoEffect::CraftChannelMixer{..}|skylive_core::VideoEffect::CraftGammaPedestalGain{..}))){return Err(rejected("Craft video effects require native schema9"));}
    if schema.schema < 9 && snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::Invert{..}|skylive_core::VideoEffect::Grayscale{..}|skylive_core::VideoEffect::Posterize{..}|skylive_core::VideoEffect::Exposure{..}|skylive_core::VideoEffect::TemperatureTint{..}|skylive_core::VideoEffect::Threshold{..}))) {
        return Err(rejected("expanded Craft video effects require native schema9"));
    }
    if schema.schema<9 && (!snapshot.preferences().audio_monitoring_device().is_default()||snapshot.sources().iter().any(|s|!s.audio_monitoring().is_off())) {
        return Err(rejected("audio monitoring requires native schema9"));
    }
    if schema.schema<9 && snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::CropPad {..}))) {
        return Err(rejected("OBS Crop/Pad requires native schema9"));
    }
    if schema.schema < 9 && snapshot.sources().iter().any(|source| matches!(source.kind(),
        skylive_core::SourceKind::VideoCapture {binding} if binding.flip_vertically())) {
        return Err(rejected("camera vertical flip requires native schema9"));
    }
    if schema.schema<9 && snapshot.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::VideoCapture {binding} if matches!(binding.mode().format(),skylive_core::VideoPixelFormat::Uyvy {..}|skylive_core::VideoPixelFormat::I420 {..}|skylive_core::VideoPixelFormat::Yv12 {..}))) {
        return Err(rejected("additional raw camera formats require native schema9"));
    }
    if schema.schema < 9 && snapshot.sources().iter().any(|source| matches!(source.kind(),
        skylive_core::SourceKind::VideoCapture {binding} if binding.audio().is_some()
            || !source.audio_filters().is_empty() || !source.audio_monitoring().is_off()
            || !source.audio_properties().is_default())) {
        return Err(rejected("camera custom input audio requires native schema9"));
    }
    if schema.schema<9 && snapshot.sources().iter().any(|s|matches!(s.kind(),skylive_core::SourceKind::VideoCapture {binding} if matches!(binding.mode().format(),skylive_core::VideoPixelFormat::Mjpeg|skylive_core::VideoPixelFormat::H264))) {
        return Err(rejected("Compressed camera transport requires native schema9"));
    }
    if schema.schema<9 && snapshot.sources().iter().any(|s|matches!(s.kind(),skylive_core::SourceKind::GameCapture {..})) {return Err(rejected("Game Capture requires native schema9"));}
    if schema.schema<9 && snapshot.sources().iter().any(|s|matches!(s.kind(),skylive_core::SourceKind::NdiSource {..})) {return Err(rejected("NDI source requires native schema9"));}
    if schema.schema<9 && snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::LumaKey{..}|skylive_core::VideoEffect::Sharpen{..}))) {
        return Err(rejected("OBS Luma Key and Sharpen require native schema9"));
    }
    if schema.schema<9&&snapshot.scenes().iter().chain(snapshot.groups()).any(|scene|scene.items().iter().any(|item|!item.color_tag().is_clear())){return Err(rejected("Sources list item color tags require native schema9"));}
    if schema.schema<9 && snapshot.scenes().iter().chain(snapshot.groups()).any(|scene|scene.items().iter().any(|item|!item.compositing().is_default())) {
        return Err(rejected("scene item compositing requires native schema9"));
    }
    if schema.schema<9 && !snapshot.preferences().accessibility().is_default() {
        return Err(rejected("accessibility colors require native schema9"));
    }
    if schema.schema<9&&snapshot.preferences().hotkeys().bindings().iter().any(|binding|binding.action.requires_extended_format()) {
        return Err(rejected("extended output shortcuts require native schema9"));
    }
    if schema.schema < 9 && snapshot.sources().iter().any(|source| matches!(source.kind(),skylive_core::SourceKind::AudioCapture {binding} if matches!(binding.target(),skylive_core::AudioTarget::WindowsProcess {..}|skylive_core::AudioTarget::WindowsWindow {..}))) {
        return Err(rejected("application process audio capture requires native schema9"));
    }
    if schema.schema<9&&snapshot.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::MediaSource{media} if media.speed_percent()!=100)){return Err(rejected("Media playback speed requires native schema9"));}
    if schema.schema<9&&snapshot.profiles().entries().iter().any(|p|!p.replay_buffer().is_default()){return Err(rejected("replay buffer settings require native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.audio_filters().iter().any(|filter|matches!(filter,skylive_core::AudioFilter::ObsNoiseGate {..}))){return Err(rejected("OBS Noise Gate requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.audio_filters().iter().any(|filter|matches!(filter,skylive_core::AudioFilter::ObsUpwardCompressor {..}))){return Err(rejected("OBS Upward Compressor requires native schema9"));}
    if schema.schema<9&&snapshot.profiles().entries().iter().any(|profile|profile.streaming().endpoint().ok().flatten().is_some_and(|endpoint|endpoint.is_secure())){return Err(rejected("RTMPS streaming requires native schema9"));}
    if schema.schema<9 && snapshot.sources().iter().any(|s|matches!(s.kind(),skylive_core::SourceKind::Browser {..})){return Err(rejected("Browser Source requires native schema9"));}
    if schema.schema < 9 && snapshot.sources().iter().any(|source| matches!(source.kind(),skylive_core::SourceKind::DisplayCapture {binding} if binding.method()==skylive_core::DisplayCaptureMethod::Wgc)) {
        return Err(rejected("Windows Graphics Capture for displays requires native schema9"));
    }
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.audio_filters().iter().any(|filter|matches!(filter,skylive_core::AudioFilter::ObsNoiseSuppression{..}))){return Err(rejected("RNNoise noise suppression requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.audio_filters().iter().any(|filter|matches!(filter,skylive_core::AudioFilter::ObsThreeBandEq {..}))){return Err(rejected("OBS 3-Band Equalizer requires native schema9"));}
    if schema.schema<9&&snapshot.profiles().entries().iter().any(|p|!p.recording().container().is_default()){return Err(rejected("Matroska recording requires native schema9"));}
    if schema.schema<9&&snapshot.profiles().entries().iter().any(|p|!p.recording().encoder().is_default()||p.recording().keyframe_interval_seconds()!=2||!p.streaming().encoder().is_default()) {return Err(rejected("explicit video encoder settings require native schema9"));}
    if schema.schema<9&&!snapshot.preferences().process_priority().is_default(){return Err(rejected("process priority requires native schema9"));}
    if schema.schema<9&&!snapshot.preferences().ndi_output().is_default(){return Err(rejected("NDI output settings require native schema9"));}
    if schema.schema<9&&snapshot.preferences().global_hotkeys().requires_extended_format(){return Err(rejected("global output shortcuts require native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::ScaleFilter{..}))) {return Err(rejected("Scaling/Aspect Ratio requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::Freeze{..}))){return Err(rejected("Freeze requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::RenderDelay{..}))) {return Err(rejected("Render Delay requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::AsyncVideoDelay{..}))) {return Err(rejected("Async Video Delay requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::VideoCapture{binding} if binding.fps_policy()!=skylive_core::CameraFpsPolicy::Fixed)) {return Err(rejected("automatic camera FPS selection requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::VideoCapture{binding} if binding.embedded_audio())) {return Err(rejected("embedded camera audio requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::VideoCapture{binding} if binding.mode_policy()!=skylive_core::CameraModePolicy::Fixed)) {return Err(rejected("automatic camera mode selection requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::Scroll{..}))) {return Err(rejected("Scroll requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::ApplyLut{..}))) {return Err(rejected("Apply LUT requires native schema9"));}
    if schema.schema<9&&snapshot.sources().iter().any(|source|source.video_filters().iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::ImageMask{..}))){return Err(rejected("Image Mask/Blend requires native schema9"));}
    person_background_guard::check(schema.schema, &snapshot)?;
    let migration = MigrationReport::for_snapshot(schema.schema, &snapshot)?;
    let mut session = Session::from_snapshot(snapshot, saved && !migration.has_semantic_changes())
        .map_err(before)?;
    if let Some(selection) = selection {
        session.select_program(selection.program).map_err(before)?;
        session.select_preview(selection.preview).map_err(before)?;
    }
    Ok(LoadedDocument { session, migration })
}
pub fn encode(collection: &StudioSnapshot) -> Result<Vec<u8>, StorageError> {
    encode_snapshot(collection, None)
}
fn encode_snapshot(
    collection: &StudioSnapshot,
    selection: Option<Selection>,
) -> Result<Vec<u8>, StorageError> {
    collection.validate().map_err(before)?;
    #[derive(Serialize)]
    struct BorrowedEnvelope<'a>{schema:u32,revision:u64,collection:&'a Collection,profiles:&'a ProfileCatalog,preferences:&'a UserPreferences,#[serde(skip_serializing_if="Option::is_none")]selection:Option<Selection>}
    struct Bounded(Vec<u8>);
    impl Write for Bounded{fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{if bytes.len()>(MAX_FILE_BYTES as usize).saturating_sub(self.0.len()){return Err(std::io::Error::other("encoded native document exceeds 16 MiB"));}let needed=self.0.len()+bytes.len();if needed>self.0.capacity(){let target=needed.max(self.0.capacity().saturating_mul(2).max(4096)).min(MAX_FILE_BYTES as usize);self.0.try_reserve_exact(target-self.0.len()).map_err(std::io::Error::other)?;}self.0.extend_from_slice(bytes);Ok(bytes.len())}fn flush(&mut self)->std::io::Result<()>{Ok(())}}
    let mut bytes=Bounded(Vec::new());
    serde_json::to_writer_pretty(&mut bytes,&BorrowedEnvelope{schema:CURRENT_SCHEMA,revision:collection.revision(),collection:collection.collection(),profiles:collection.profiles(),preferences:collection.preferences(),selection}).map_err(before)?;
    Ok(bytes.0)
}
pub fn read_bytes(path: &Path) -> Result<Vec<u8>, StorageError> {
    let file = fs::File::open(path).map_err(before)?;
    if file.metadata().map_err(before)?.len() > MAX_FILE_BYTES {
        return Err(rejected("document exceeds 16 MiB"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(before)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(rejected("document exceeds 16 MiB"));
    }
    Ok(bytes)
}
fn read(path: &Path, saved: bool) -> Result<Session, StorageError> {
    let loaded=decode_with_report(&read_bytes(path)?,saved)?;crate::image_slideshow::directories::refresh_loaded(loaded).map(|loaded|loaded.session)
}
pub fn load(path: &Path) -> Result<Session, StorageError> {
    read(path, true)
}
pub fn load_with_report(path: &Path) -> Result<LoadedDocument, StorageError> {
    crate::image_slideshow::directories::refresh_loaded(decode_with_report(&read_bytes(path)?, true)?)
}
pub fn recover_with_report(path: &Path) -> Result<LoadedDocument, StorageError> {
    crate::image_slideshow::directories::refresh_loaded(decode_with_report(&read_bytes(&recovery_path(path))?, false)?)
}

/// Recovery stays present until a successful explicit save, including repeated recovery.
pub fn recover(path: &Path) -> Result<Session, StorageError> {
    read(&recovery_path(path), false)
}
pub fn recovery_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_os_string();
    p.push(".recovery");
    p.into()
}

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct SavedFileStamp{revision:u64,sha256:[u8;32]}
impl SavedFileStamp{
    pub(crate) fn from_bytes(revision:u64,bytes:&[u8])->Self{use sha2::{Digest,Sha256};Self{revision,sha256:Sha256::digest(bytes).into()}}
    pub fn revision(self)->u64{self.revision}
    pub fn matches(self,bytes:&[u8])->bool{use sha2::{Digest,Sha256};let actual:[u8;32]=Sha256::digest(bytes).into();actual==self.sha256}
}
#[derive(Debug)]
pub struct SaveReceipt {
    stamp:SavedFileStamp,
    session: u64,
    revision: u64,
    selection: Selection,
}
impl SaveReceipt {
    pub fn file_stamp(&self)->SavedFileStamp{self.stamp}
    pub fn accept(self, owner: &mut Session) -> bool {
        self.selection.preview == owner.preview()
            && self.selection.program == owner.program()
            && owner.mark_saved(self.session, self.revision)
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}
pub struct SaveJob {
    snapshot: Arc<StudioSnapshot>,
    session: u64,
    path: PathBuf,
    selection: Selection,
}
impl SaveJob {
    pub fn path(&self)->&Path{&self.path}
    pub fn collection_name(&self)->&str{self.snapshot.name()}
    pub fn new(owner: &Session, path: PathBuf) -> Self {
        Self {
            snapshot: owner.snapshot(),
            session: owner.identity(),
            path,
            selection: Selection {
                preview: owner.preview(),
                program: owner.program(),
            },
        }
    }
    pub fn checkpoint(self) -> Result<(), StorageError> {
        publish(
            &recovery_path(&self.path),
            &encode_snapshot(&self.snapshot, Some(self.selection))?,
        )
    }
    fn receipt_for(&self,bytes:&[u8])->SaveReceipt{use sha2::{Digest,Sha256};SaveReceipt{session:self.session,revision:self.snapshot.revision(),selection:self.selection,stamp:SavedFileStamp{revision:self.snapshot.revision(),sha256:Sha256::digest(bytes).into()}}}
    /// Create an independent native artifact without overwriting any path.
    /// Used by explicit managed collection creation on its existing I/O owner.
    pub fn run_new(self)->Result<SaveReceipt,StorageError>{let bytes=encode_snapshot(&self.snapshot,Some(self.selection))?;publish_new_bytes(&self.path,&bytes)?;Ok(self.receipt_for(&bytes))}
    pub fn run(self) -> Result<SaveReceipt, StorageError> {
        let bytes = encode_snapshot(&self.snapshot, Some(self.selection))?;
        publish_bytes(&self.path, &bytes)?;
        Ok(self.receipt_for(&bytes))
    }
}

/// No-overwrite publication of one new artifact, retaining normal phase truth.
/// Private sibling payload + hard link: a pre-existing final path always rejects.
pub fn publish_new_bytes(path:&Path,bytes:&[u8])->Result<(),StorageError>{
    if !path.is_absolute()||path.file_name().is_none()||bytes.len()as u64>MAX_FILE_BYTES{return Err(rejected("new native publication requires an absolute filename and at most 16 MiB"));}
    let parent=path.parent().ok_or_else(||rejected("new native publication parent"))?;
    let serial=TEMP_SERIAL.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|value|value.checked_add(1)).map_err(|_|rejected("native temporary identity exhausted"))?;
    let mut name=path.file_name().ok_or_else(||rejected("new native filename"))?.to_os_string();name.push(format!(".{}.{serial}.new",std::process::id()));let folder=parent.join(name);fs::create_dir(&folder).map_err(before)?;
    let temporary=folder.join("payload");let prepare=(||{let mut file=OpenOptions::new().create_new(true).write(true).open(&temporary)?;file.write_all(bytes)?;file.sync_all()})();
    if let Err(error)=prepare{let _=fs::remove_file(&temporary);let _=fs::remove_dir(&folder);return Err(before(error));}
    if let Err(error)=fs::hard_link(&temporary,path){let _=fs::remove_file(&temporary);let _=fs::remove_dir(&folder);return Err(before(error));}
    fs::remove_file(&temporary).and_then(|()|fs::remove_dir(&folder)).map_err(after)?;
    #[cfg(unix)]fs::File::open(parent).and_then(|directory|directory.sync_all()).map_err(after)?;
    Ok(())
}

/// Publish already-validated codec output with recovery and explicit failure phase.
pub fn publish_bytes(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(rejected("output exceeds 16 MiB"));
    }
    publish(&recovery_path(path), bytes).map_err(|e| StorageError {
        phase: Phase::BeforePublication,
        detail: format!("recovery preparation: {e}"),
        cause: Some(Box::new(e)),
    })?;
    publish(path, bytes)?;
    fs::remove_file(recovery_path(path)).map_err(after)?;
    Ok(())
}

fn publish(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let serial = TEMP_SERIAL.fetch_add(1, Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".{}.{serial}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    // create_new refuses stale/colliding temporary files; never truncate one.
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp)
        .map_err(before)?;
    let prepare = file.write_all(bytes).and_then(|_| file.sync_all());
    drop(file);
    if let Err(error) = prepare {
        let _ = fs::remove_file(&tmp);
        return Err(before(error));
    }
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(before(error));
    }
    #[cfg(unix)]
    fs::File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )
    .and_then(|directory| directory.sync_all())
    .map_err(after)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quick_rows_scene_override_defaults_and_explicit_empty_native_documents_roundtrip() {
        use skylive_core::{Command, QuickTransitionId, TransitionConfig, TransitionKind};
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::UpdateQuickTransition {
                id: QuickTransitionId::new(2).unwrap(),
                kind: TransitionKind::Fade,
                duration_ms: 650,
                fade_to_black: true,
            })
            .unwrap();
        session
            .apply(Command::MoveQuickTransition {
                id: QuickTransitionId::new(3).unwrap(),
                index: 0,
            })
            .unwrap();
        session
            .apply(Command::SetSceneTransition {
                scene,
                config: Some(TransitionConfig::new(TransitionKind::Cut, 1250).unwrap()),
            })
            .unwrap();
        let bytes = encode(&session.snapshot()).unwrap();
        let reopened = decode(&bytes, true).unwrap();
        assert_eq!(
            reopened.snapshot().quick_transitions(),
            session.snapshot().quick_transitions()
        );
        assert_eq!(
            reopened
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override(),
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override()
        );
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let collection = value["collection"].as_object_mut().unwrap();
        collection.remove("quick_transitions");
        collection.remove("next_quick_id");
        value["collection"]["scenes"][0]
            .as_object_mut()
            .unwrap()
            .remove("transition_override");
        let legacy = decode(&serde_json::to_vec(&value).unwrap(), true).unwrap();
        assert_eq!(
            legacy
                .snapshot()
                .quick_transitions()
                .iter()
                .map(|row| row.id().get())
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            legacy
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override(),
            None
        );
        value["collection"]["quick_transitions"] = serde_json::json!([]);
        let empty = decode(&serde_json::to_vec(&value).unwrap(), true).unwrap();
        assert!(empty.snapshot().quick_transitions().is_empty());
        value["collection"]["quick_transitions"] = serde_json::json!([{"id":1,"config":{"kind":"fade","duration_ms":300},"fade_to_black":false,"future":true}]);
        assert!(decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
    }
    #[test]
    fn transition_configuration_roundtrips_and_legacy_documents_default_without_runtime_state() {
        use skylive_core::{Command, TransitionKind};
        let mut session = Session::new();
        session
            .apply(Command::SetTransition {
                kind: TransitionKind::Cut,
                duration_ms: 1250,
            })
            .unwrap();
        let encoded = encode(&session.snapshot()).unwrap();
        let reopened = decode(&encoded, true).unwrap();
        assert_eq!(
            reopened.snapshot().transition(),
            session.snapshot().transition()
        );
        let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        value["collection"]
            .as_object_mut()
            .unwrap()
            .remove("transition");
        let legacy = decode(&serde_json::to_vec(&value).unwrap(), true).unwrap();
        assert_eq!(
            legacy
                .snapshot()
                .resolve_transition(legacy.snapshot().transition())
                .unwrap()
                .kind(),
            TransitionKind::Fade
        );
        assert_eq!(legacy.snapshot().transition().duration_ms(), 300);
        value["collection"]["transition"] = serde_json::json!({"kind":"fade","duration_ms":0});
        assert!(decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
        value["collection"]["transition"] =
            serde_json::json!({"kind":"fade","duration_ms":300,"elapsed_ns":1});
        assert!(decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
    }
    #[test]
    fn native_image_descriptors_reopen_and_recover_without_opening_linked_files() {
        let mut session = Session::new();
        let asset = skylive_core::ImageAsset::new(
            "C:/unavailable/project/image.png".into(),
            [19; 32],
            4,
            2,
        )
        .unwrap();
        session
            .apply(skylive_core::Command::AddImageSource {
                scene: session.preview(),
                name: "Linked".into(),
                asset: asset.clone(),
            })
            .unwrap();
        let dir = Temp::new();
        let path = dir.0.join("image.json");
        SaveJob::new(&session, path.clone()).run().unwrap();
        SaveJob::new(&session, path.clone()).checkpoint().unwrap();
        for reopened in [load(&path).unwrap(), recover(&path).unwrap()] {
            assert_eq!(
                reopened.snapshot().sources()[0].kind(),
                &skylive_core::SourceKind::Image {
                    asset: asset.clone()
                }
            );
        }
    }
    use skylive_core::{Command, Id};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "skylive-storage-{}-{}",
                std::process::id(),
                TEMP_SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn affine_storage_roundtrip_and_legacy_transform_defaults() {
        let mut owner = Session::new();
        let scene = owner.preview();
        let item = owner
            .apply(Command::AddColorSource {
                scene,
                name: "Affine".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        let transform = skylive_core::Transform {
            x: 5.0,
            y: 7.0,
            rotation_degrees: 450.25,
            flip_x: true,
            flip_y: true,
            crop: skylive_core::Crop {
                left: 12,
                top: 5,
                right: 7,
                bottom: 3,
            },
            ..skylive_core::Transform::full(1920, 1080)
        };
        owner
            .apply(Command::SetTransform {
                scene,
                item,
                transform: transform.clone(),
            })
            .unwrap();
        let dir = Temp::new();
        let path = dir.0.join("affine.json");
        SaveJob::new(&owner, path.clone()).run().unwrap();
        SaveJob::new(&owner, path.clone()).checkpoint().unwrap();
        for reopened in [load(&path).unwrap(), recover(&path).unwrap()] {
            assert_eq!(
                reopened.snapshot().scenes()[0].items()[0].transform(),
                &transform
            );
        }
        let mut legacy: serde_json::Value =
            serde_json::from_slice(&read_bytes(&path).unwrap()).unwrap();
        let value = legacy["collection"]["scenes"][0]["items"][0]["transform"]
            .as_object_mut()
            .unwrap();
        for key in ["rotation_degrees", "flip_x", "flip_y", "crop"] {
            value.remove(key);
        }
        let restored = decode(&serde_json::to_vec(&legacy).unwrap(), true).unwrap();
        let expected = skylive_core::Transform {
            x: 5.0,
            y: 7.0,
            ..skylive_core::Transform::full(1920, 1080)
        };
        assert_eq!(
            restored.snapshot().scenes()[0].items()[0].transform(),
            &expected
        );
    }
    #[test]
    fn native_storage_preserves_separate_preview_and_program_and_rejects_bad_selection() {
        let dir = Temp::new();
        let path = dir.0.join("selection.json");
        let mut session = Session::new();
        let other = session
            .apply(Command::AddScene {
                name: "Other".into(),
            })
            .unwrap()
            .created
            .unwrap();
        session.select_preview(other).unwrap();
        session.cut().unwrap();
        session.select_preview(Id::new(1)).unwrap();
        SaveJob::new(&session, path.clone()).run().unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.preview(), Id::new(1));
        assert_eq!(loaded.program(), other);
        SaveJob::new(&session, path.clone()).checkpoint().unwrap();
        let recovered = recover(&path).unwrap();
        assert_eq!(recovered.program(), other);
        let mut value: serde_json::Value =
            serde_json::from_slice(&read_bytes(&path).unwrap()).unwrap();
        value["selection"]["preview"] = 999.into();
        assert!(decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
        let receipt = SaveJob::new(&session, path).run().unwrap();
        session.select_preview(other).unwrap();
        assert!(!receipt.accept(&mut session));
    }
    #[test]
    fn save_replace_reopen_and_stale_receipt() {
        let dir = Temp::new();
        let path = dir.0.join("show.json");
        let mut s = Session::new();
        s.apply(Command::AddScene { name: "Two".into() }).unwrap();
        let old = SaveJob::new(&s, path.clone());
        s.apply(Command::RenameScene {
            scene: Id::new(1),
            name: "Main".into(),
        })
        .unwrap();
        assert!(!old.run().unwrap().accept(&mut s));
        assert!(s.is_dirty());
        assert!(SaveJob::new(&s, path.clone()).run().unwrap().accept(&mut s));
        assert!(!s.is_dirty());
        assert_eq!(*s.snapshot(), *load(&path).unwrap().snapshot());
        assert!(!recovery_path(&path).exists());
    }
    #[test]
    fn recovery_is_repeatable_and_remains_dirty() {
        let dir = Temp::new();
        let path = dir.0.join("show.json");
        let mut s = Session::new();
        s.apply(Command::RenameCollection {
            name: "Recovered".into(),
        })
        .unwrap();
        SaveJob::new(&s, path.clone()).checkpoint().unwrap();
        for _ in 0..2 {
            let r = recover(&path).unwrap();
            assert_eq!(r.snapshot().name(), "Recovered");
            assert!(r.is_dirty());
        }
        assert!(recovery_path(&path).exists());
    }
    #[test]
    fn malformed_and_unknown_schema_are_rejected() {
        let mut bytes = encode(&Session::new().snapshot()).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["schema"] = (CURRENT_SCHEMA + 1).into();
        assert!(decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
        value["schema"] = 1.into();
        value["collection"]["next_id"] = 1.into();
        assert!(decode(&serde_json::to_vec(&value).unwrap(), true).is_err());
        bytes.truncate(12);
        assert!(decode(&bytes, true).is_err());
        assert!(decode(&vec![b' '; MAX_FILE_BYTES as usize + 1], true).is_err());
    }
    #[test]
    fn failed_publication_preserves_previous_data_and_recovery() {
        let dir = Temp::new();
        let path = dir.0.join("show.json");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"previous data").unwrap();
        let error = SaveJob::new(&Session::new(), path.clone())
            .run()
            .unwrap_err();
        assert_eq!(error.phase, Phase::BeforePublication);
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"previous data");
        assert!(recover(&path).is_ok());
    }
}

#[cfg(test)]
#[path = "storage/obs_dynamics_tests.rs"]
mod obs_dynamics_tests;

#[cfg(test)]
#[path = "storage/window_wgc_tests.rs"]
mod window_wgc_tests;

#[cfg(test)]
#[path = "storage/setup_tests.rs"]
mod setup_tests;
