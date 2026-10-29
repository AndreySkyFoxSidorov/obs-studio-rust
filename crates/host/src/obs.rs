//! Strict, source-traced OBS scene-collection interchange, not general OBS parity.
//!
//! The accepted subset includes absolute v1 and relative v2 coordinates normalized
//! at the explicit profile canvas, acyclic nested
//! scenes/private Groups, represented source providers with top-left pivot, rotation, flips and source-pixel
//! crop (no bounds modes). See OBS frontend/models/SceneCollection.hpp,
//! frontend/widgets/OBSBasic_SceneCollections.cpp, libobs/obs.c:2443,
//! libobs/obs-scene.c:1120..1458, plugins/image-source/color-source.c and
//! plugins/image-source/image-source.c (unversioned image_source).
//! Unsupported authored data is rejected before a candidate Session is built.
//! No existing session or file is ever mutated by this module.
use crate::assets::MAX_IMAGE_FILE_BYTES;
use crate::storage::MAX_FILE_BYTES;
use serde_json::{Map, Value, json};
use skylive_core::{
    Command, Crop, Id, ImageAsset, Session, SourceKind, StudioSnapshot, Transform,
    ItemCompositing,BlendMode,BlendingMethod,ScaleFiltering,
    TransitionPresetId, TransitionSelection, VideoFilterId,
};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod display;
mod camera;
#[cfg(target_os="windows")]
mod camera_audio_windows;
#[cfg(target_os="windows")]
mod camera_export_windows;
pub use camera::{CameraAudioMapping,CameraImportDevice};
mod window;
mod wasapi;
mod application_audio;
mod slideshow;
mod source_hotkeys;
mod media;
mod text;
mod filters;
mod audio_filters;
mod audio_mixers;
#[cfg(test)]
mod filters_tests;
#[cfg(test)]
mod scene_source_tests;
mod strict_json;
mod item_color_tags;
mod transitions;
mod coordinates;
mod groups;
#[cfg(test)]
use skylive_core::TransitionKind;

const MAX_JSON_STRUCTURE: usize = 65_536;
const MAX_IMPORT_SCENES: usize = 64;
const MAX_IMPORT_SOURCES: usize = 256;
const MAX_IMPORT_ITEMS: usize = 512;
const MAX_IMPORT_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// Canvas configuration comes from the OBS profile, not its scene collection.
#[derive(Clone, Copy, Debug)]
pub struct ImportVideo {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObsIssue {
    pub path: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug)]
pub struct ObsError {
    pub issues: Vec<ObsIssue>,
}
impl fmt::Display for ObsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, issue) in self.issues.iter().enumerate() {
            if index != 0 {
                writeln!(formatter)?;
            }
            write!(
                formatter,
                "{} [{}]: {}",
                issue.path, issue.code, issue.message
            )?;
        }
        Ok(())
    }
}
impl std::error::Error for ObsError {}

fn issue(path: &str, code: &'static str, message: impl Into<String>) -> ObsError {
    ObsError {
        issues: vec![ObsIssue {
            path: path.to_owned(),
            code,
            message: message.into(),
        }],
    }
}

/// In-memory interchange sidecar. Retain it alongside the imported session to
/// preserve accepted OBS UUIDs and item IDs on export.
/// It is bound to a session identity; use with another session is rejected.
pub struct ObsIdentities {
    session: u64,
    root: Map<String, Value>,
    sources: HashMap<Id, Map<String, Value>>,
    items: HashMap<Id, Map<String, Value>>,
    presets: HashMap<TransitionPresetId, Map<String, Value>>,
    filters: HashMap<VideoFilterId, Map<String, Value>>,
}

pub struct ImportedCollection {
    pub session: Session,
    pub identities: ObsIdentities,
}
impl ImportedCollection {
    /// Adopt only the imported graph; preserve the application's selected profile
    /// and user preferences. Retarget UUID metadata only after validated adoption.
    pub fn adopt_into(mut self, owner: &Session) -> Result<Self, skylive_core::Error> {
        self.session = owner.adopt_collection(self.session)?;
        self.identities.session = self.session.identity();
        Ok(self)
    }
}

/// Captures immutable authored data and selection in constant time. The host
/// moves this job to its bounded I/O worker and calls `run` there. Later edits
/// or selection changes cannot affect these export bytes. The host still owns
/// file publication and must distinguish exporting this revision from saving
/// the current document. Closing after capture does not invalidate a read-only
/// snapshot export; a job captured from an already closed session is rejected.
pub struct ExportJob {
    collection: Arc<StudioSnapshot>,
    session: u64,
    preview: Id,
    program: Id,
    closed: bool,
    identities: Option<Arc<ObsIdentities>>,
}

impl ExportJob {
    pub fn new(session: &Session, identities: Option<Arc<ObsIdentities>>) -> Self {
        Self {
            collection: session.snapshot(),
            session: session.identity(),
            preview: session.preview(),
            program: session.program(),
            closed: session.is_closed(),
            identities,
        }
    }

    pub fn run(self) -> Result<Vec<u8>, ObsError> {
        export_snapshot(
            &self.collection,
            self.session,
            self.preview,
            self.program,
            self.closed,
            self.identities.as_deref(),
        )
    }
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Map<String, Value>, ObsError> {
    value
        .as_object()
        .ok_or_else(|| issue(path, "invalid", "expected an object"))
}
fn array<'a>(value: &'a Value, path: &str) -> Result<&'a Vec<Value>, ObsError> {
    value
        .as_array()
        .ok_or_else(|| issue(path, "invalid", "expected an array"))
}
fn required<'a>(map: &'a Map<String, Value>, key: &str, path: &str) -> Result<&'a Value, ObsError> {
    map.get(key).ok_or_else(|| {
        issue(
            &format!("{path}.{key}"),
            "invalid",
            "required field missing",
        )
    })
}
fn string<'a>(map: &'a Map<String, Value>, key: &str, path: &str) -> Result<&'a str, ObsError> {
    required(map, key, path)?
        .as_str()
        .ok_or_else(|| issue(&format!("{path}.{key}"), "invalid", "expected a string"))
}
fn number(map: &Map<String, Value>, key: &str, default: f64, path: &str) -> Result<f64, ObsError> {
    match map.get(key) {
        None => Ok(default),
        Some(value) => value.as_f64().filter(|v| v.is_finite()).ok_or_else(|| {
            issue(
                &format!("{path}.{key}"),
                "invalid",
                "expected a finite number",
            )
        }),
    }
}
fn boolean(
    map: &Map<String, Value>,
    key: &str,
    default: bool,
    path: &str,
) -> Result<bool, ObsError> {
    match map.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| issue(&format!("{path}.{key}"), "invalid", "expected a boolean")),
    }
}
fn known(map: &Map<String, Value>, keys: &[&str], path: &str) -> Result<(), ObsError> {
    if let Some(key) = map.keys().find(|key| !keys.contains(&key.as_str())) {
        return Err(issue(
            &format!("{path}.{key}"),
            "unsupported",
            "field is outside the supported OBS subset",
        ));
    }
    Ok(())
}
fn neutral(
    map: &Map<String, Value>,
    key: &str,
    expected: Value,
    path: &str,
) -> Result<(), ObsError> {
    if let Some(value) = map.get(key) {
        let equal = if value.is_number() && expected.is_number() {
            value.as_f64() == expected.as_f64()
        } else {
            *value == expected
        };
        if !equal {
            return Err(issue(
                &format!("{path}.{key}"),
                "unsupported",
                "nondefault value is not representable",
            ));
        }
    }
    Ok(())
}
fn dimension(
    map: &Map<String, Value>,
    key: &str,
    default: u32,
    path: &str,
) -> Result<u32, ObsError> {
    let value = number(map, key, f64::from(default), path)?;
    if value.fract() != 0.0 || !(1.0..=4096.0).contains(&value) {
        return Err(issue(
            &format!("{path}.{key}"),
            "invalid",
            "color source dimensions must be integral and in 1..=4096",
        ));
    }
    Ok(value as u32)
}
fn vector(
    map: &Map<String, Value>,
    key: &str,
    default: (f64, f64),
    path: &str,
) -> Result<(f64, f64), ObsError> {
    let Some(value) = map.get(key) else {
        return Ok(default);
    };
    let path = format!("{path}.{key}");
    let value = object(value, &path)?;
    known(value, &["x", "y"], &path)?;
    Ok((
        number(value, "x", default.0, &path)?,
        number(value, "y", default.1, &path)?,
    ))
}
fn uuid_valid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}
fn structural_budget(bytes: &[u8]) -> Result<(), ObsError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(issue("$", "limit", "OBS file exceeds 16 MiB"));
    }
    let (mut quoted, mut escaped, mut depth, mut structure) = (false, false, 0usize, 0usize);
    for &byte in bytes {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && byte == b'\\' {
            escaped = true;
            continue;
        }
        if byte == b'"' {
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        match byte {
            b'{' | b'[' => {
                depth += 1;
                structure += 1;
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            b',' | b':' => structure += 1,
            _ => {}
        }
        if depth > 32 || structure > MAX_JSON_STRUCTURE {
            return Err(issue(
                "$",
                "limit",
                "JSON exceeds depth 32 or structural-token budget 65536",
            ));
        }
    }
    Ok(())
}

const GLOBAL_AUDIO_SLOTS:[(&str,skylive_core::AudioSlot);6]=[
    ("DesktopAudioDevice1",skylive_core::AudioSlot::Desktop1),("DesktopAudioDevice2",skylive_core::AudioSlot::Desktop2),
    ("AuxAudioDevice1",skylive_core::AudioSlot::Mic1),("AuxAudioDevice2",skylive_core::AudioSlot::Mic2),("AuxAudioDevice3",skylive_core::AudioSlot::Mic3),("AuxAudioDevice4",skylive_core::AudioSlot::Mic4),
];
struct InputSource {
    path:String,
    name: String,
    raw: Map<String, Value>,
    filters: Vec<filters::InputFilter>,
    audio_filters:Vec<audio_filters::Input>,
    sync_offset_ms: i32,
    force_mono:bool,
    balance:f32,
    audio_tracks:skylive_core::SourceAudioTrackMask,
    playback_hotkeys:skylive_core::SourcePlaybackHotkeys,
    audio_hotkeys:skylive_core::SourceAudioHotkeys,
    push_audio:skylive_core::PushAudioSettings,
    monitoring:skylive_core::AudioMonitoringMode,
    kind: InputKind,
}
enum InputKind {
    Camera {binding:skylive_core::VideoCaptureBinding,gain:f32,muted:bool},
    Text {settings:skylive_core::TextSource,system_family:String,gain:f32,muted:bool},
    Group {items:Vec<InputItem>,width:u32,height:u32},
    Media {input:media::Input,gain:f32,muted:bool},
    Wasapi {binding:skylive_core::AudioCaptureBinding,gain:f32,muted:bool},
    SlideShow{input:slideshow::Input,gain:f32,muted:bool},
    Browser {browser:skylive_core::BrowserSource,gain:f32,muted:bool},
    Display {binding:skylive_core::DisplayCaptureBinding,gain:f32,muted:bool},
    Window {binding:skylive_core::WindowCaptureBinding,gain:f32,muted:bool},
    Color {
        rgba: [u8; 4],
        width: u32,
        height: u32,
        gain: f32,
        muted: bool,
    },
    Image {
        file: PathBuf,
        asset: Option<ImageAsset>,
        settings: skylive_core::ImageSourceSettings,
        gain: f32,
        muted: bool,
    },
    Scene {
        items: Vec<InputItem>,
    },
}
impl InputKind {
    fn items(&self)->Option<&[InputItem]>{match self{Self::Scene{items}|Self::Group{items,..}=>Some(items),_=>None}}
    fn items_mut(&mut self)->Option<&mut Vec<InputItem>>{match self{Self::Scene{items}|Self::Group{items,..}=>Some(items),_=>None}}
    fn container(&self)->bool{self.items().is_some()}
    fn dimensions(&self) -> Option<(u32, u32)> {
        match self {
            Self::Text {settings,..}=>Some((settings.width(),settings.height())),
            Self::Media {input,..}=>input.dimensions(),
            Self::SlideShow{input,..}=>input.settings().map(skylive_core::ImageSlideShow::dimensions),
            Self::Browser {browser,..}=>Some((browser.width(),browser.height())),
            Self::Color { width, height, .. } => Some((*width, *height)),
            Self::Display {binding,..} => Some((binding.width(),binding.height())),
            Self::Window {binding,..} => Some((binding.width(),binding.height())),
            Self::Camera {binding,..} => Some((binding.width(),binding.height())),
            Self::Image {
                asset: Some(asset), ..
            } => Some((asset.width(), asset.height())),
            Self::Group{width,height,..}=>Some(((*width).max(1),(*height).max(1))),
            Self::Image { asset: None, .. } | Self::Scene { .. } | Self::Wasapi { .. } => None,
        }
    }
}
struct InputItem {
    color_tag:skylive_core::SourceItemColorTag,
    compositing: ItemCompositing,
    source: usize,
    visible: bool,
    locked: bool,
    transform: Transform,
    scale: (f64, f64),
    raw: Map<String, Value>,
}

fn check_source_common(
    map: &Map<String, Value>,
    path: &str,
    is_scene: bool,
    catalog: &transitions::Catalog,
) -> Result<(), ObsError> {
    known(
        map,
        &[
            "prev_ver",
            "name",
            "uuid",
            "canvas_uuid",
            "id",
            "versioned_id",
            "settings",
            "mixers",
            "sync",
            "flags",
            "volume",
            "balance",
            "enabled",
            "muted",
            "push-to-mute",
            "push-to-mute-delay",
            "push-to-talk",
            "push-to-talk-delay",
            "hotkeys",
            "deinterlace_mode",
            "deinterlace_field_order",
            "monitoring_type",
            "monitoring_enabled",
            "private_settings",
            "filters",
        ],
        path,
    )?;
    groups::main_canvas(map,path)?;
    for key in [
        "deinterlace_mode",
        "deinterlace_field_order",
    ] {
        neutral(map, key, json!(0), path)?;
    }
    for key in ["monitoring_enabled"] {
        neutral(map, key, json!(false), path)?;
    }

    if is_scene {
        neutral(map, "sync", json!(0), path)?;
        transitions::scene_override(map, path, catalog)?;
    } else {
        neutral(map, "private_settings", json!({}), path)?;
    }
    let audio_tracks=audio_mixers::parse(map,path)?;
    if is_scene&&!audio_tracks.is_all(){
        return Err(issue(path,"unsupported","Separate Scene or Group audio-track routing is not represented by this importer."));
    }
    neutral(map, "enabled", json!(true), path)?;
    if is_scene {
        neutral(map, "volume", json!(1), path)?;
        neutral(map, "muted", json!(false), path)?;
    }
    if let Some(value) = map.get("prev_ver") {
        if value.as_u64().is_none() {
            return Err(issue(
                path,
                "invalid",
                "prev_ver must be a nonnegative integer",
            ));
        }
    }
    Ok(())
}

fn check_transition(map: &Map<String, Value>, key: &str, path: &str) -> Result<(), ObsError> {
    if let Some(value) = map.get(key) {
        let path = format!("{path}.{key}");
        let value = object(value, &path)?;
        // A duration without a transition source has no visible effect in OBS.
        known(value, &["duration"], &path)?;
        if let Some(duration) = value.get("duration") {
            if duration.as_u64().is_none() {
                return Err(issue(
                    &path,
                    "invalid",
                    "transition duration must be nonnegative",
                ));
            }
        }
    }
    Ok(())
}

fn parse_item(
    value: &Value,
    path: &str,
    sources: &[InputSource],
    names: &HashMap<String, usize>,
    uuids: &HashMap<String, usize>,
    video: ImportVideo,
    coordinates: coordinates::Mode,
    parent_group:bool,
) -> Result<InputItem, ObsError> {
    let map = object(value, path)?;
    known(
        map,
        &[
            "name",
            "source_uuid",
            "visible",
            "locked",
            "rot",
            "pos",
            "scale",
            "pos_rel",
            "scale_rel",
            "scale_ref",
            "bounds_rel",
            "align",
            "bounds_type",
            "bounds_align",
            "bounds_crop",
            "bounds",
            "crop_left",
            "crop_top",
            "crop_right",
            "crop_bottom",
            "id",
            "group_item_backup",
            "scale_filter",
            "blend_method",
            "blend_type",
            "show_transition",
            "hide_transition",
            "private_settings",
        ],
        path,
    )?;
    neutral(map, "bounds_type", json!(0), path)?;
    neutral(map, "align", json!(5), path)?; // OBS_ALIGN_LEFT (1) | OBS_ALIGN_TOP (4).
    neutral(map, "bounds_crop", json!(false), path)?;
    if map.get("group_item_backup").is_some_and(|value|!value.is_boolean()){return Err(issue(path,"invalid","group_item_backup must be a boolean"));}
    let text=|key:&str,default:&str|->Result<String,ObsError> {
        match map.get(key) {None=>Ok(default.into()),Some(value)=>value.as_str().map(str::to_owned)
            .ok_or_else(||issue(&format!("{path}.{key}"),"invalid","expected a compositing string"))}
    };
    let blend_mode=match text("blend_type","normal")?.as_str() {
        "normal"=>BlendMode::Normal,"additive"=>BlendMode::Additive,"subtract"=>BlendMode::Subtract,
        "screen"=>BlendMode::Screen,"multiply"=>BlendMode::Multiply,"lighten"=>BlendMode::Lighten,"darken"=>BlendMode::Darken,
        _=>return Err(issue(&format!("{path}.blend_type"),"unsupported","unsupported OBS blending mode")),
    };
    let blending_method=match text("blend_method","default")?.as_str() {
        "default"=>BlendingMethod::Default,"srgb_off"=>BlendingMethod::SrgbOff,
        _=>return Err(issue(&format!("{path}.blend_method"),"unsupported","unsupported OBS blending method")),
    };
    let scale_filter=match text("scale_filter","disable")?.to_ascii_lowercase().as_str() {
        "disable"=>ScaleFiltering::Disabled,"point"=>ScaleFiltering::Point,"bilinear"=>ScaleFiltering::Bilinear,
        "bicubic"=>ScaleFiltering::Bicubic,"lanczos"=>ScaleFiltering::Lanczos,"area"=>ScaleFiltering::Area,
        _=>return Err(issue(&format!("{path}.scale_filter"),"unsupported","unsupported OBS scale filtering")),
    };
    let compositing=ItemCompositing {blend_mode,blending_method,scale_filter};
    if let Some(value) = map.get("bounds_align") {
        if value.as_u64().is_none() {
            return Err(issue(path, "invalid", "bounds_align must be an integer"));
        }
    }
    coordinates.bounds(map,path)?;
    check_transition(map, "show_transition", path)?;
    check_transition(map, "hide_transition", path)?;
    if let Some(value) = map.get("id") {
        if value.as_i64().is_none_or(|id| id <= 0) {
            return Err(issue(
                path,
                "invalid",
                "scene item ID must be a positive signed integer",
            ));
        }
    }
    let name = string(map, "name", path)?;
    let source = *names.get(name).ok_or_else(|| {
        issue(
            path,
            "invalid_reference",
            "scene item source name is unresolved",
        )
    })?;
    if let Some(value) = map.get("source_uuid") {
        let value = value
            .as_str()
            .ok_or_else(|| issue(path, "invalid", "source_uuid must be a string"))?;
        if !value.is_empty() && uuids.get(value) != Some(&source) {
            return Err(issue(
                path,
                "invalid_reference",
                "source UUID is unresolved or disagrees with source name",
            ));
        }
    }
    // Scene intrinsic size is the profile canvas; image dimensions are decoded
    // only after the complete acyclic graph and authored metadata are valid.
    let group_source=matches!(sources[source].kind,InputKind::Group{..});
    let color_tag=item_color_tags::parse_group(map,path,group_source)?;
    let scene_source=matches!(sources[source].kind, InputKind::Scene { .. });
    let dimensions = if scene_source {
        Some((video.width, video.height))
    } else {
        sources[source].kind.dimensions()
    };
    let (width, height) = dimensions.unwrap_or((1, 1));
    // OBS's absent vec2 scale is zero; require an explicit usable scale rather
    // than silently converting an invisible item into unit scale.
    let (position,scale)=coordinates.item(map,path,video,scene_source||group_source,group_source||parent_group)?;
    let rotation = number(map, "rot", 0.0, path)?;
    if !(-360000.0..=360000.0).contains(&rotation) {
        return Err(issue(
            &format!("{path}.rot"),
            "invalid_transform",
            "rotation must be finite and within -360000..=360000 degrees",
        ));
    }
    let crop_value = |key: &str| -> Result<u32, ObsError> {
        match map.get(key) {
            None => Ok(0),
            Some(value) => value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    issue(
                        &format!("{path}.{key}"),
                        "invalid_crop",
                        "crop must be an unsigned 32-bit integer in source pixels",
                    )
                }),
        }
    };
    let crop = Crop {
        left: crop_value("crop_left")?,
        top: crop_value("crop_top")?,
        right: crop_value("crop_right")?,
        bottom: crop_value("crop_bottom")?,
    };
    if dimensions.is_some()
        && (u64::from(crop.left) + u64::from(crop.right) >= u64::from(width)
            || u64::from(crop.top) + u64::from(crop.bottom) >= u64::from(height))
    {
        return Err(issue(
            path,
            "invalid_crop",
            "combined crop must leave at least one source pixel on both axes",
        ));
    }
    // obs-scene.c:628..661 and :938..939: crop creates a reduced local
    // texture; signed scale and rotation act about the top-left pivot. Store
    // full-source positive extents, not the cropped AABB or a shifted pivot.
    let transform = Transform {
        x: position.0 as f32,
        y: position.1 as f32,
        width: (f64::from(width) * scale.0.abs()) as f32,
        height: (f64::from(height) * scale.1.abs()) as f32,
        opacity: 1.0,
        rotation_degrees: rotation as f32,
        flip_x: scale.0.is_sign_negative(),
        flip_y: scale.1.is_sign_negative(),
        crop,
    };
    if scale.0 == 0.0
        || scale.1 == 0.0
        || ![transform.x, transform.y, transform.width, transform.height]
            .iter()
            .all(|v| v.is_finite())
    {
        return Err(issue(
            path,
            "unsupported",
            "scale must be finite and nonzero with representable positive extents",
        ));
    }
    Ok(InputItem {
        color_tag,
        compositing,
        source,
        visible: boolean(map, "visible", false, path)?,
        locked: boolean(map, "locked", false, path)?,
        transform,
        scale,
        raw: map.clone(),
    })
}

/// Builds an isolated candidate exclusively with the public command engine.
/// Unsupported input returns structured issues; no caller state is published.
pub fn import_collection(bytes: &[u8], video: ImportVideo) -> Result<ImportedCollection, ObsError> {
    import_collection_inner(bytes, video, None, None, &[])
}

/// Filesystem composition port shared by CLI and desktop I/O workers. Resolve
/// the physical collection file first so collection-relative image references
/// use the same directory even when the requested JSON path is a symlink.
/// Reads are bounded by the native storage byte limit; no caller state changes.
pub fn load_collection(path: &Path, video: ImportVideo) -> Result<ImportedCollection, ObsError> {
    load_collection_in(path,video,None,&[])
}
/// Explicit application context; no installed-settings IO is performed here.
/// Hotkey conflicts are checked before linked asset/codec preparation.
pub fn load_collection_with_preferences(path:&Path,video:ImportVideo,preferences:&skylive_core::UserPreferences)->Result<ImportedCollection,ObsError>{
    load_collection_in(path,video,Some(preferences),&[])
}
/// Filesystem import with capabilities supplied by a camera owner. Enumeration
/// and ownership are the caller's responsibility and never run on this parser.
pub fn load_collection_with_camera_catalog(path:&Path,video:ImportVideo,preferences:&skylive_core::UserPreferences,devices:&[CameraImportDevice])->Result<ImportedCollection,ObsError>{
    load_collection_in(path,video,Some(preferences),devices)
}
fn load_collection_in(path:&Path,video:ImportVideo,preferences:Option<&skylive_core::UserPreferences>,devices:&[CameraImportDevice])->Result<ImportedCollection,ObsError>{
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| issue("$", "file_io", format!("{}: {error}", path.display())))?;
    let base = canonical.parent().ok_or_else(|| {
        issue(
            "$",
            "input_base",
            format!(
                "OBS collection has no parent directory: {}",
                canonical.display()
            ),
        )
    })?;
    let bytes = crate::storage::read_bytes(&canonical)
        .map_err(|error| issue("$", "file_io", format!("{}: {error}", canonical.display())))?;
    import_collection_inner(&bytes, video, Some(base), preferences, devices)
}

/// Resolve relative image references against an explicit absolute collection
/// directory. No process-current-directory fallback is used by either API.
/// The caller runs this filesystem/decoder boundary on its bounded I/O worker.
pub fn import_collection_at(
    bytes: &[u8],
    video: ImportVideo,
    base_directory: &Path,
) -> Result<ImportedCollection, ObsError> {
    if !base_directory.is_absolute() {
        return Err(issue(
            "$",
            "invalid_base_directory",
            "image import base directory must be absolute",
        ));
    }
    import_collection_inner(bytes, video, Some(base_directory), None, &[])
}

/// Raw-input form of the same explicit caller context. Relative references need
/// a supplied absolute base, just as in the standalone import boundary.
pub fn import_collection_with_preferences(bytes:&[u8],video:ImportVideo,base_directory:Option<&Path>,preferences:&skylive_core::UserPreferences)->Result<ImportedCollection,ObsError>{
    if base_directory.is_some_and(|path|!path.is_absolute()){return Err(issue("$","invalid_base_directory","image import base directory must be absolute"));}
    import_collection_inner(bytes,video,base_directory,Some(preferences),&[])
}

/// Import a collection with device capabilities already resolved by the camera
/// owner. The catalog is an immutable snapshot; this function never opens a
/// camera. Callers must reject stale owner results before publishing the model.
pub fn import_collection_with_camera_catalog(bytes:&[u8],video:ImportVideo,base_directory:Option<&Path>,preferences:&skylive_core::UserPreferences,devices:&[CameraImportDevice])->Result<ImportedCollection,ObsError>{
    if base_directory.is_some_and(|path|!path.is_absolute()){return Err(issue("$","invalid_base_directory","image import base directory must be absolute"));}
    import_collection_inner(bytes,video,base_directory,Some(preferences),devices)
}

/// Collect exact DirectShow device paths for asynchronous CameraOwner mode
/// queries before attempting the import. This is discovery only: it does not
/// establish device existence, supported modes, or attached audio, and must
/// never be used as a substitute for full import validation. No device is
/// opened and no application state is changed here.
pub fn camera_targets_for_import(bytes:&[u8])->Result<Vec<skylive_core::VideoCaptureTarget>,ObsError>{
    camera_catalog_requests(bytes).map(|(targets,_)|targets)
}
fn camera_catalog_requests(bytes:&[u8])->Result<(Vec<skylive_core::VideoCaptureTarget>,Vec<skylive_core::VideoCaptureTarget>),ObsError>{
    structural_budget(bytes)?;
    let value=strict_json::parse(bytes)?;
    let root=object(&value,"$")?;
    let sources=array(required(root,"sources","$")?,"$.sources")?;
    if sources.len()>MAX_IMPORT_SCENES+MAX_IMPORT_SOURCES{return Err(issue("$.sources","limit","source admission budget exceeded"));}
    let mut targets=Vec::new();
    let mut preferred=Vec::new();
    for (index,value) in sources.iter().enumerate(){
        let path=format!("$.sources[{index}]");
        let source=object(value,&path)?;
        if source.get("id").and_then(Value::as_str)!=Some("dshow_input"){continue;}
        let settings=object(required(source,"settings",&path)?,&format!("{path}.settings"))?;
        let target=camera::target(settings,&format!("{path}.settings"))?;
        let skylive_core::VideoCaptureTarget::Windows{symbolic_link}= &target else {unreachable!("DirectShow target is Windows")};
        let is_preferred=match settings.get("res_type"){None=>true,Some(v) if v.as_i64()==Some(0)=>true,Some(v) if v.as_i64()==Some(1)=>false,_=>return Err(issue(&format!("{path}.settings.res_type"),"camera_mode_unsupported","Unknown OBS camera resolution policy"))};
        if is_preferred && !preferred.iter().any(|existing|matches!(existing,skylive_core::VideoCaptureTarget::Windows{symbolic_link:old} if old.eq_ignore_ascii_case(symbolic_link))){preferred.push(target.clone());}
        if targets.iter().any(|existing|matches!(existing,skylive_core::VideoCaptureTarget::Windows{symbolic_link:old} if old.eq_ignore_ascii_case(symbolic_link))){continue;}
        if targets.len()>=64{return Err(issue("$.sources","camera_catalog_limit","more than 64 distinct camera devices"));}
        targets.push(target);
    }
    Ok((targets,preferred))
}

/// One immutable, bounded filesystem snapshot for camera discovery and the
/// subsequent parse. The same bytes and canonical base directory are used in
/// both phases, even if the user or another process changes the original file.
/// Run `prepare` and `import` on the document I/O worker, and perform device
/// queries through the existing CameraOwner between these calls.
pub struct PreparedCameraImport {
    bytes: Vec<u8>,
    base_directory: std::path::PathBuf,
    targets: Vec<skylive_core::VideoCaptureTarget>,
    preferred: Vec<skylive_core::VideoCaptureTarget>,
    video_only_targets: Vec<skylive_core::VideoCaptureTarget>,
    audio_mappings: Arc<[CameraAudioMapping]>,
}
impl PreparedCameraImport {
    pub fn prepare(path:&Path)->Result<Self,ObsError>{
        let canonical=std::fs::canonicalize(path).map_err(|error|issue("$","file_io",format!("{}: {error}",path.display())))?;
        let base_directory=canonical.parent().ok_or_else(||issue("$","input_base","OBS collection has no parent directory"))?.to_path_buf();
        let bytes=crate::storage::read_bytes(&canonical).map_err(|error|issue("$","file_io",format!("{}: {error}",canonical.display())))?;
        let (targets,preferred)=camera_catalog_requests(&bytes)?;
        let mut requested_audio=Vec::new();
        let mut video_only_targets=Vec::new();
        if !targets.is_empty(){
            let value=strict_json::parse(&bytes)?;
            let root=object(&value,"$")?;
            for (index,value) in array(required(root,"sources","$")?,"$.sources")?.iter().enumerate(){
                let path=format!("$.sources[{index}]");let source=object(value,&path)?;
                if source.get("id").and_then(Value::as_str)!=Some("dshow_input"){continue;}
                let settings=object(required(source,"settings",&path)?,&format!("{path}.settings"))?;
                if !boolean(settings,"use_custom_audio_device",false,&path)? {
                    let target=camera::target(settings,&path)?;
                    if !video_only_targets.contains(&target){video_only_targets.push(target);}
                }
                if let Some(identity)=camera::requested_audio(settings,&format!("{path}.settings"))? {
                    if !requested_audio.contains(&identity){requested_audio.push(identity);}
                    if requested_audio.len()>64{return Err(issue("$.sources","camera_audio_limit","More than 64 camera audio identities"));}
                }
            }
        }
        #[cfg(target_os="windows")]
        let audio_mappings=Arc::from(camera_audio_windows::resolve(&requested_audio)?);
        #[cfg(not(target_os="windows"))]
        let audio_mappings=Arc::from([]);
        Ok(Self{bytes,base_directory,targets,preferred,video_only_targets,audio_mappings})
    }
    pub fn targets(&self)->&[skylive_core::VideoCaptureTarget]{&self.targets}
    pub fn needs_video_only(&self,target:&skylive_core::VideoCaptureTarget)->bool {
        self.video_only_targets.iter().any(|old|match(old,target){(skylive_core::VideoCaptureTarget::Windows{symbolic_link:a},skylive_core::VideoCaptureTarget::Windows{symbolic_link:b})=>a.eq_ignore_ascii_case(b),_=>false})
    }
    pub fn needs_preferred(&self,target:&skylive_core::VideoCaptureTarget)->bool {
        self.preferred.iter().any(|old|match (old,target){(skylive_core::VideoCaptureTarget::Windows{symbolic_link:a},skylive_core::VideoCaptureTarget::Windows{symbolic_link:b})=>a.eq_ignore_ascii_case(b),_=>false})
    }
    pub fn import(self,video:ImportVideo,preferences:&skylive_core::UserPreferences,devices:&[CameraImportDevice])->Result<ImportedCollection,ObsError>{
        let devices:Vec<_>=devices.iter().cloned().map(|mut device|{device.audio_mappings=self.audio_mappings.clone();device}).collect();
        import_collection_with_camera_catalog(&self.bytes,video,Some(&self.base_directory),preferences,&devices)
    }
}

fn import_collection_inner(
    bytes: &[u8],
    video: ImportVideo,
    base_directory: Option<&Path>,
    preferences:Option<&skylive_core::UserPreferences>,
    camera_devices:&[CameraImportDevice],
) -> Result<ImportedCollection, ObsError> {
    structural_budget(bytes)?;
    let value = strict_json::parse(bytes)?;
    let root = object(&value, "$")?;
    known(
        root,
        &[
            "name",
            "version",
            "sources",
            "groups",
            "scene_order",
            "current_scene",
            "current_program_scene",
            "canvases",
            "current_transition",
            "transition_duration",
            "transitions",
            "quick_transitions",
            "saved_projectors",
            "preview_locked",
            "scaling_enabled",
            "scaling_level",
            "scaling_off_x",
            "scaling_off_y",
            "modules",
            "resolution",
            "migration_resolution",
            "virtual-camera",
            "DesktopAudioDevice1","DesktopAudioDevice2","AuxAudioDevice1","AuxAudioDevice2","AuxAudioDevice3","AuxAudioDevice4",
        ],
        "$",
    )?;
    let coordinates=coordinates::Mode::parse(root)?;
    coordinates::migration_metadata(root)?;
    // OBS32 Windows saves this default whenever the output capability exists,
    // even when no virtual camera is running. Other authored route metadata
    // is not represented by this document interchange subset.
    if let Some(value)=root.get("virtual-camera") {
        let settings=object(value,"$.virtual-camera")?;
        known(settings,&["type2"],"$.virtual-camera")?;
        neutral(settings,"type2",json!(3),"$.virtual-camera")?;
    }
    for key in ["canvases", "saved_projectors"] {
        neutral(root, key, json!([]), "$")?;
    }
    for key in ["preview_locked", "scaling_enabled"] {
        neutral(root, key, json!(false), "$")?;
    }
    for key in ["scaling_level", "scaling_off_x", "scaling_off_y"] {
        neutral(root, key, json!(0), "$")?;
    }
    neutral(root, "modules", json!({}), "$")?;
    let catalog = transitions::Catalog::parse(root)?;
    let quick_rows = transitions::quick_rows(root, &catalog)?;
    let transition_reference = catalog.reference(
        root.get("current_transition")
            .unwrap_or(&Value::String("Fade".into())),
        "$.current_transition",
    )?;
    let transition_duration = match root.get("transition_duration") {
        None => 300,
        Some(value) => match value.as_u64() {
            Some(0) => 300, // OBS missing/zero collection duration means its 300ms default.
            Some(value) if (50..=20_000).contains(&value) => value as u32,
            _ => {
                return Err(issue(
                    "$.transition_duration",
                    "unsupported",
                    "duration must be zero/default or in the supported UI range 50..20000 ms",
                ));
            }
        },
    };
    let transition = TransitionSelection::new(transition_reference, transition_duration)
        .map_err(|error| issue("$.transition_duration", "invalid", error.to_string()))?;
    if let Some(resolution) = root.get("resolution") {
        let resolution = object(resolution, "$.resolution")?;
        known(resolution, &["x", "y"], "$.resolution")?;
        neutral(resolution, "x", json!(video.width), "$.resolution")?;
        neutral(resolution, "y", json!(video.height), "$.resolution")?;
    }
    let collection_name = string(root, "name", "$")?.to_owned();
    let main_input=array(required(root,"sources","$")?,"$.sources")?;
    let group_input:&[Value]=match root.get("groups"){Some(value)=>array(value,"$.groups")?.as_slice(),None=>&[]};
    let globals:Vec<_>=GLOBAL_AUDIO_SLOTS.iter().filter_map(|(key,slot)|root.get(*key).map(|value|(*key,*slot,value))).collect();
    if main_input.len().checked_add(group_input.len()).and_then(|count|count.checked_add(globals.len())).is_none_or(|count|count>MAX_IMPORT_SCENES+MAX_IMPORT_SOURCES){return Err(issue("$","limit","source admission budget exceeded"));}
    let ordinary_end=main_input.len()+group_input.len();
    let input:Vec<_>=main_input.iter().chain(group_input).chain(globals.iter().map(|(_,_,value)|*value)).collect();
    if input.len() > MAX_IMPORT_SCENES + MAX_IMPORT_SOURCES {
        return Err(issue(
            "$.sources",
            "limit",
            "source admission budget exceeded",
        ));
    }
    let mut sources = Vec::with_capacity(input.len());
    let mut display_importer=display::Importer::default();
    let mut window_importer=window::Importer::default();
    let mut filter_uuids = HashSet::new();
    let (mut names, mut uuids) = (HashMap::new(), HashMap::new());
    for (index, value) in input.iter().enumerate() {
        let path=if index<main_input.len(){format!("$.sources[{index}]")}else if index<ordinary_end{format!("$.groups[{}]",index-main_input.len())}else{format!("$.{}",globals[index-ordinary_end].0)};
        let map = object(value, &path)?;
        let id = string(map, "id", &path)?;
        let versioned = map
            .get("versioned_id")
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| issue(&path, "invalid", "versioned_id must be a string"))
            })
            .transpose()?
            .unwrap_or(id);
        let is_group=id=="group";
        if is_group!=(index>=main_input.len()&&index<ordinary_end){return Err(issue(&path,"group_location","Group sources must be in the OBS groups array; ordinary sources belong in sources."));}
        let is_scene = id == "scene";
        let is_image = id == "image_source";
        let is_slideshow=id=="slideshow";
        let is_browser=id=="browser_source";
        let is_media=id=="ffmpeg_source";
        let is_wasapi=id=="wasapi_input_capture"||id=="wasapi_output_capture"||id=="wasapi_process_output_capture";
        if index>=ordinary_end&&(!is_wasapi||id=="wasapi_process_output_capture"){return Err(issue(&path,"global_audio_provider","Global audio root requires a supported Windows WASAPI device source"));}
        let is_text=id=="text_gdiplus";
        let is_display = id == "monitor_capture";
        let is_window = id == "window_capture";
        let is_camera = id == "dshow_input";
        if (is_group && versioned!="group")
            || (is_scene && versioned != "scene")
            || (is_image && versioned != "image_source")
            || (is_slideshow&&!["slideshow","slideshow_v2"].contains(&versioned))
            || (is_browser && versioned != "browser_source")
            || (is_media && versioned != "ffmpeg_source")
            || (is_wasapi && versioned != id)
            || (is_text && versioned != "text_gdiplus_v3")
            || (is_display && versioned != "monitor_capture")
            || (is_window && versioned != "window_capture")
            || (is_camera && versioned != "dshow_input")
            || (!is_group && !is_scene && !is_image && !is_slideshow && !is_browser && !is_display && !is_window && !is_media && !is_text && !is_wasapi && !is_camera
                && (id != "color_source"
                    || !["color_source", "color_source_v2", "color_source_v3"]
                        .contains(&versioned)))
        {
            return Err(issue(
                &format!("{path}.id"),
                "unsupported_source",
                format!("unsupported source id={id}, versioned_id={versioned}"),
            ));
        }
        check_source_common(map, &path, is_scene, &catalog)?;
        if is_group{neutral(map,"volume",json!(1),&path)?;neutral(map,"muted",json!(false),&path)?;}
        let playback_hotkeys=source_hotkeys::parse(map,&path,if is_slideshow{source_hotkeys::Provider::Slideshow}else if is_media{source_hotkeys::Provider::Media}else{source_hotkeys::Provider::Other})?;
        let audio_filters=audio_filters::parse(map,&path,&mut filter_uuids)?;
        let filters = filters::parse(map, &path, is_scene, &mut filter_uuids,base_directory)?;
        let name = string(map, "name", &path)?.to_owned();
        if names.insert(name.clone(), index).is_some() {
            return Err(issue(
                &path,
                "invalid",
                "OBS source names must be globally unique",
            ));
        }
        if let Some(value) = map.get("uuid") {
            let value = value
                .as_str()
                .ok_or_else(|| issue(&path, "invalid", "uuid must be a string"))?;
            if !uuid_valid(value) || uuids.insert(value.to_owned(), index).is_some() {
                return Err(issue(&path, "invalid", "invalid or duplicate source UUID"));
            }
        }
        let settings = object(
            required(map, "settings", &path)?,
            &format!("{path}.settings"),
        )?;
        let kind = if is_group {
            let(width,height)=groups::parse_settings(settings,&format!("{path}.settings"))?;InputKind::Group{items:Vec::new(),width,height}
        } else if is_scene {
            known(
                settings,
                &["items", "id_counter", "custom_size"],
                &format!("{path}.settings"),
            )?;
            neutral(settings, "custom_size", json!(false), &path)?;
            if let Some(value) = settings.get("id_counter") {
                if value.as_i64().is_none_or(|value| value < 0) {
                    return Err(issue(
                        &path,
                        "invalid",
                        "scene id_counter must be nonnegative",
                    ));
                }
            }
            InputKind::Scene { items: Vec::new() }
        } else if is_camera {
            let binding=camera::parse(settings,&format!("{path}.settings"),camera_devices)?;
            let gain=number(map,"volume",1.0,&path)? as f32;
            if !gain.is_finite()||!(0.0..=20.0).contains(&gain){return Err(issue(&path,"invalid","volume is outside the engine range"));}
            InputKind::Camera {binding,gain,muted:boolean(map,"muted",false,&path)?}
        } else if is_text {
            let (settings,system_family)=text::parse(settings,&format!("{path}.settings"),base_directory)?;
            let gain=number(map,"volume",1.0,&path)? as f32;
            if !gain.is_finite()||!(0.0..=20.0).contains(&gain){return Err(issue(&path,"invalid","volume is outside the engine range"));}
            InputKind::Text{settings,system_family,gain,muted:boolean(map,"muted",false,&path)?}
        } else if is_wasapi {
            let flow=if id=="wasapi_input_capture"{skylive_core::AudioFlow::Input}else{skylive_core::AudioFlow::Output};
            let settings_path=format!("{path}.settings");
            let binding=if id=="wasapi_process_output_capture"{
                if !cfg!(target_os="windows"){return Err(issue(&settings_path,"wasapi_platform_unsupported","Windows application audio requires Windows"));}
                let selector=application_audio::parse(settings,&settings_path)?;
                skylive_core::AudioCaptureBinding::new(flow,skylive_core::AudioTarget::WindowsWindow{selector},boolean(settings,"use_device_timing",false,&settings_path)?,false).map_err(|error|issue(&settings_path,"unsupported",error.to_string()))?
            }else{wasapi::parse(settings,&settings_path,flow)?};
            let gain=number(map,"volume",1.0,&path)? as f32;
            if !gain.is_finite()||!(0.0..=20.0).contains(&gain){return Err(issue(&path,"invalid","volume is outside the engine range"));}
            InputKind::Wasapi{binding,gain,muted:boolean(map,"muted",false,&path)?}
        } else if is_media {
            let input=media::Input::parse(settings,&format!("{path}.settings"),base_directory)?;
            let gain=number(map,"volume",1.0,&path)? as f32;
            if !gain.is_finite()||!(0.0..=20.0).contains(&gain){return Err(issue(&path,"invalid","volume is outside the engine range"));}
            InputKind::Media {input,gain,muted:boolean(map,"muted",false,&path)?}
        } else if is_browser {
            let settings_path=format!("{path}.settings");
            known(settings,&["is_local_file","local_file","url","width","height","fps","fps_custom","css","shutdown","restart_when_active","reroute_audio","webpage_control_level"],&settings_path)?;
            let control_audio=boolean(settings,"reroute_audio",false,&settings_path)?;
            neutral(settings,"webpage_control_level",json!(0),&settings_path)?;
            let local=boolean(settings,"is_local_file",false,&settings_path)?;
            let location=if local {
                let path=PathBuf::from(string(settings,"local_file",&settings_path)?);
                let path=if path.is_absolute(){path}else{base_directory.ok_or_else(||issue(&settings_path,"relative_browser_path","relative browser HTML requires an explicit absolute import base"))?.join(path)};
                skylive_core::BrowserLocation::LocalFile {path:path.to_str().ok_or_else(||issue(&settings_path,"invalid_browser_path","browser HTML path is not UTF8"))?.into()}
            } else {skylive_core::BrowserLocation::Url {url:settings.get("url").map(|v|v.as_str().ok_or_else(||issue(&settings_path,"invalid","browser URL must be a string"))).transpose()?.unwrap_or("https://obsproject.com/browser-source").into()}};
            let fps=if boolean(settings,"fps_custom",true,&settings_path)? {dimension(settings,"fps",30,&settings_path)?}else{video.fps_num.div_ceil(video.fps_den)};
            let css=settings.get("css").map(|v|v.as_str().ok_or_else(||issue(&settings_path,"invalid","browser CSS must be a string"))).transpose()?.unwrap_or("body { background-color: rgba(0,0,0,0); margin: 0; overflow: hidden; }").to_owned();
            let browser=skylive_core::BrowserSource::new(location,css,dimension(settings,"width",800,&settings_path)?,dimension(settings,"height",600,&settings_path)?,fps,boolean(settings,"shutdown",false,&settings_path)?,boolean(settings,"restart_when_active",false,&settings_path)?).map(|value|value.with_control_audio(control_audio)).map_err(|e|issue(&settings_path,"unsupported_browser",e.to_string()))?;
            let gain=number(map,"volume",1.0,&path)? as f32;if !gain.is_finite()||!(0.0..=20.0).contains(&gain){return Err(issue(&path,"invalid","browser source gain is outside engine range"));}
            InputKind::Browser {browser,gain,muted:boolean(map,"muted",false,&path)?}
        } else if is_window {
            let binding=window_importer.parse(settings,&format!("{path}.settings"))?;
            let gain=number(map,"volume",1.0,&path)? as f32;
            if !gain.is_finite()||!(0.0..=20.0).contains(&gain){return Err(issue(&path,"invalid","volume is outside the engine range"));}
            InputKind::Window{binding,gain,muted:boolean(map,"muted",false,&path)?}
        } else if is_display {
            let binding=display_importer.parse(settings,&format!("{path}.settings"))?;
            let gain=number(map,"volume",1.0,&path)? as f32;
            if !gain.is_finite() || !(0.0..=20.0).contains(&gain) {return Err(issue(&path,"invalid","volume is outside the engine range"));}
            InputKind::Display {binding,gain,muted:boolean(map,"muted",false,&path)?}
        } else if is_slideshow{
            let settings_path=format!("{path}.settings");let input=slideshow::Input::parse(settings,&settings_path,base_directory,versioned=="slideshow_v2")?;let gain=number(map,"volume",1.0,&path)?as f32;if !gain.is_finite()||!(0.0..=20.0).contains(&gain){return Err(issue(&path,"invalid","volume is outside the engine range"));}InputKind::SlideShow{input,gain,muted:boolean(map,"muted",false,&path)?}
        } else if is_image {
            let settings_path = format!("{path}.settings");
            known(
                settings,
                &["file", "unload", "linear_alpha", "is_slide"],
                &settings_path,
            )?;
            neutral(settings, "is_slide", json!(false), &settings_path)?;
            let image_settings=skylive_core::ImageSourceSettings {
                unload_when_hidden:boolean(settings,"unload",false,&settings_path)?,
                linear_alpha:boolean(settings,"linear_alpha",false,&settings_path)?,
            };
            let file = PathBuf::from(string(settings, "file", &settings_path)?);
            if file.as_os_str().is_empty() {
                return Err(issue(
                    &format!("{settings_path}.file"),
                    "invalid_image_path",
                    "image file path must not be empty",
                ));
            }
            let file = if file.is_absolute() {
                file
            } else {
                base_directory.ok_or_else(|| issue(&format!("{settings_path}.file"), "relative_image_path", "relative image reference requires import_collection_at with an explicit absolute base directory"))?.join(file)
            };
            let gain = number(map, "volume", 1.0, &path)? as f32;
            if !gain.is_finite() || !(0.0..=20.0).contains(&gain) {
                return Err(issue(
                    &path,
                    "invalid",
                    "volume is outside the engine range",
                ));
            }
            InputKind::Image {
                file,
                asset: None,
                settings: image_settings,
                gain,
                muted: boolean(map, "muted", false, &path)?,
            }
        } else {
            known(
                settings,
                &["color", "width", "height"],
                &format!("{path}.settings"),
            )?;
            let default_color = if versioned == "color_source_v3" {
                0xffd1d1d1u32
            } else {
                u32::MAX
            };
            let color = match settings.get("color") {
                None => default_color,
                Some(value) => value
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| {
                        issue(&path, "invalid", "color must be an unsigned RGBA32 integer")
                    })?,
            };
            let default_size = if versioned == "color_source" {
                (400, 400)
            } else {
                (1920, 1080)
            };
            let gain = number(map, "volume", 1.0, &path)? as f32;
            if !gain.is_finite() || !(0.0..=20.0).contains(&gain) {
                return Err(issue(
                    &path,
                    "invalid",
                    "volume is outside the engine range",
                ));
            }
            InputKind::Color {
                rgba: color.to_le_bytes(),
                width: dimension(settings, "width", default_size.0, &path)?,
                height: dimension(settings, "height", default_size.1, &path)?,
                gain,
                muted: boolean(map, "muted", false, &path)?,
            }
        };
        let flags=match map.get("flags"){None=>0,Some(value)=>value.as_u64().and_then(|value|u32::try_from(value).ok()).ok_or_else(||issue(&format!("{path}.flags"),"invalid","source flags must be an unsigned 32-bit integer"))?};
        if flags&!2!=0{return Err(issue(&format!("{path}.flags"),"unsupported","Only the OBS force-mono source flag is represented"));}
        let force_mono=flags&2!=0;
        let balance=number(map,"balance",0.5,&path)?;
        if !(0.0..=1.0).contains(&balance){return Err(issue(&format!("{path}.balance"),"unsupported","Source balance must be in 0..1"));}
        // OBS loads a JSON double and passes it to the source as float.
        let balance=balance as f32;
        if (force_mono||balance!=0.5)&&!is_media&&!is_wasapi&&!matches!(&kind,InputKind::Browser{browser,..}if browser.control_audio()){return Err(issue(&path,"unsupported","Force mono and balance require a represented audio-capable source"));}
        let monitoring=match map.get("monitoring_type"){
            None=>skylive_core::AudioMonitoringMode::Off,
            Some(value)=>match value.as_u64(){Some(0)=>skylive_core::AudioMonitoringMode::Off,Some(1)=>skylive_core::AudioMonitoringMode::MonitorOnly,Some(2)=>skylive_core::AudioMonitoringMode::MonitorAndOutput,_=>return Err(issue(&format!("{path}.monitoring_type"),"unsupported","OBS monitoring type must be 0, 1 or 2"))},
        };
        if !monitoring.is_off()&&!cfg!(target_os="windows"){return Err(issue(&format!("{path}.monitoring_type"),"monitoring_platform_unsupported","The represented physical monitoring backend currently requires Windows"));}
        if !monitoring.is_off()&&!is_media&&!is_wasapi&&!matches!(&kind,InputKind::Browser{browser,..}if browser.control_audio()){return Err(issue(&format!("{path}.monitoring_type"),"monitoring_source_unsupported","Monitoring requires a represented audio-capable source"));}
        let audio_hotkeys=source_hotkeys::parse_audio(map,&path,match &kind{InputKind::Media{..}|InputKind::Wasapi{..}=>true,InputKind::Browser{browser,..}=>browser.control_audio(),_=>false})?;
        if matches!(&kind,InputKind::Wasapi{binding,..}if binding.flow()==skylive_core::AudioFlow::Output)&&!monitoring.is_off(){return Err(issue(&path,"wasapi_monitoring_unsupported","OBS output capture self-monitoring suppression is not represented"));}
        if is_wasapi&&!filters.is_empty(){return Err(issue(&path,"wasapi_video_unsupported","WASAPI audio sources cannot carry video filters"));}
        let delay=|key:&str|->Result<u32,ObsError>{match map.get(key){None=>Ok(0),Some(value)=>value.as_u64().and_then(|v|u32::try_from(v).ok()).filter(|v|*v<=i32::MAX as u32).ok_or_else(||issue(&format!("{path}.{key}"),"unsupported","Push delay requires 0..2147483647 milliseconds"))}};
        let push_audio=skylive_core::PushAudioSettings::new(boolean(map,"push-to-mute",false,&path)?,delay("push-to-mute-delay")?,boolean(map,"push-to-talk",false,&path)?,delay("push-to-talk-delay")?).map_err(|error|issue(&path,"unsupported",error.to_string()))?;
        if !push_audio.is_default()&&!is_media&&!is_wasapi&&!matches!(&kind,InputKind::Browser{browser,..}if browser.control_audio()){return Err(issue(&path,"unsupported","Push-to-mute/talk settings require an audio-capable source"));}
        let sync_ns=match map.get("sync") {
            None=>0,
            Some(value)=>value.as_i64().ok_or_else(||issue(&format!("{path}.sync"),"invalid","source sync requires integral nanoseconds"))?,
        };
        if sync_ns%1_000_000!=0 || !(-950_000_000..=20_000_000_000).contains(&sync_ns) {
            return Err(issue(&format!("{path}.sync"),"unsupported","source sync must be whole milliseconds in-950..20000ms"));
        }
        let sync_offset_ms=(sync_ns/1_000_000)as i32;
        if sync_offset_ms!=0&&!is_media&&!is_wasapi&&!matches!(&kind,InputKind::Browser {browser,..} if browser.control_audio()) {
            return Err(issue(&format!("{path}.sync"),"unsupported","nonzero sync requires a supported audio-capable source"));
        }
        if !audio_filters.is_empty()&&!is_media&&!is_wasapi&&!matches!(&kind,InputKind::Browser{browser,..}if browser.control_audio()){
            return Err(issue(&path,"unsupported","audio filter import requires a supported audio-capable source"));
        }
        let audio_tracks=audio_mixers::parse(map,&path)?;
        if !audio_tracks.is_all()&&!is_media&&!is_wasapi&&!matches!(&kind,InputKind::Browser{browser,..}if browser.control_audio()){
            return Err(issue(&path,"unsupported","nondefault source audio mixes require a supported audio-controlled source"));
        }
        sources.push(InputSource {
            path,
            name,
            raw: map.clone(),
            filters,
            audio_filters,
            sync_offset_ms,
            force_mono,
            balance,
            audio_tracks,
            playback_hotkeys,
            audio_hotkeys,
            push_audio,
            monitoring,
            kind,
        });
    }
    if filter_uuids.iter().any(|uuid| uuids.contains_key(uuid)) {
        return Err(issue(
            "$.sources",
            "identity_conflict",
            "filter UUID collides with a source UUID",
        ));
    }
    let scene_count = sources
        .iter()
        .filter(|source| matches!(source.kind, InputKind::Scene { .. }))
        .count();
    if scene_count == 0
        || scene_count > MAX_IMPORT_SCENES
        || sources.len() - scene_count > MAX_IMPORT_SOURCES
    {
        return Err(issue(
            "$.sources",
            "limit",
            "requires 1..=64 root scenes and at most256 nonroot providers including Groups",
        ));
    }
    let mut order = Vec::new();
    for (index, value) in array(required(root, "scene_order", "$")?, "$.scene_order")?
        .iter()
        .enumerate()
    {
        let path = format!("$.scene_order[{index}]");
        let entry = object(value, &path)?;
        known(entry, &["name"], &path)?;
        let name = string(entry, "name", &path)?;
        let source = *names.get(name).ok_or_else(|| {
            issue(
                &path,
                "invalid_reference",
                "scene_order entry is unresolved",
            )
        })?;
        if !matches!(sources[source].kind, InputKind::Scene { .. }) || order.contains(&source) {
            return Err(issue(
                &path,
                "invalid_reference",
                "scene_order must contain each scene exactly once",
            ));
        }
        order.push(source);
    }
    if order.len() != scene_count {
        return Err(issue(
            "$.scene_order",
            "invalid_reference",
            "scene_order must list every scene",
        ));
    }
    let preview_name = string(root, "current_scene", "$")?;
    let program_name = root
        .get("current_program_scene")
        .map(|v| {
            v.as_str()
                .ok_or_else(|| issue("$.current_program_scene", "invalid", "expected a string"))
        })
        .transpose()?
        .unwrap_or(preview_name);
    let preview = *names
        .get(preview_name)
        .filter(|index| order.contains(index))
        .ok_or_else(|| {
            issue(
                "$.current_scene",
                "invalid_reference",
                "current preview scene is unresolved",
            )
        })?;
    let program = *names
        .get(program_name)
        .filter(|index| order.contains(index))
        .ok_or_else(|| {
            issue(
                "$.current_program_scene",
                "invalid_reference",
                "current program scene is unresolved",
            )
        })?;
    let mut used = HashSet::new();
    let mut item_count = 0usize;
    let mut wire_items=0usize;
    let containers:Vec<_>=sources.iter().enumerate().filter_map(|(index,source)|source.kind.container().then_some(index)).collect();
    for &index in &containers {
        let path = format!("{}.settings.items",sources[index].path);
        let settings = object(required(&sources[index].raw, "settings", &path)?, &path)?;
        let values = array(required(settings, "items", &path)?, &path)?;
        wire_items=wire_items.checked_add(values.len()).ok_or_else(||issue(&path,"limit","Group wire item count overflow"))?;
        if wire_items>skylive_core::MAX_ITEMS{return Err(issue(&path,"limit","Active and backup OBS items exceed the 4096 wire-row budget"));}
        if item_count > MAX_IMPORT_ITEMS {
            return Err(issue(
                &path,
                "limit",
                "import admits at most 512 items because command publication clones the collection",
            ));
        }
        let mut item_ids = HashSet::new();
        let mut parsed = Vec::with_capacity(values.len());
        for (item_index, value) in values.iter().enumerate() {
            let item = parse_item(
                value,
                &format!("{path}[{item_index}]"),
                &sources,
                &names,
                &uuids,
                video,
                coordinates,
                matches!(sources[index].kind,InputKind::Group{..}),
            )?;
            if item.raw.get("group_item_backup").and_then(Value::as_bool)==Some(true){continue;}
            item_count+=1;if item_count>MAX_IMPORT_ITEMS{return Err(issue(&path,"limit","OBS interchange admits at most 512 active items"));}
            if let Some(id) = item.raw.get("id").and_then(Value::as_i64) {
                if !item_ids.insert(id) {
                    return Err(issue(&path, "invalid", "duplicate item ID within a scene"));
                }
            }
            used.insert(item.source);
            parsed.push(item);
        }
        *sources[index].kind.items_mut().ok_or_else(||issue(&path,"invalid","container source missing"))?=parsed;
    }
    if sources.iter().enumerate().any(|(index, source)| {
        !matches!(source.kind, InputKind::Scene { .. }) && index<ordinary_end && !used.contains(&index)
    }) {
        return Err(issue(
            "$.sources",
            "unsupported",
            "unreferenced sources cannot be represented by this engine",
        ));
    }

    groups::preflight(&sources,item_count)?;

    // Iterative postorder validates cycles/depth before any linked image I/O.
    // Memoized depths bound DAG validation by nodes+edges rather than fanout.
    let mut color = vec![0u8; sources.len()];
    let mut depth = vec![0usize; sources.len()];
    for &root in &order {
        if color[root] == 2 {
            continue;
        }
        let mut stack = vec![(root, false)];
        while let Some((node, exit)) = stack.pop() {
            if exit {
                let items=sources[node].kind.items().ok_or_else(||issue("$","invalid_reference","container source missing"))?;
                let longest = items
                    .iter()
                    .filter(|item| sources[item.source].kind.container())
                    .map(|item| depth[item.source])
                    .max()
                    .unwrap_or(0);
                depth[node] = longest + 1;
                if depth[node] > skylive_core::MAX_SCENE_DEPTH + 1 {
                    return Err(issue(
                        "$.sources",
                        "limit",
                        "nested scene depth exceeds engine limit",
                    ));
                }
                color[node] = 2;
            } else {
                if color[node] == 1 {
                    return Err(issue(
                        "$.sources",
                        "invalid_reference",
                        "scene graph contains a cycle",
                    ));
                }
                if color[node] == 2 {
                    continue;
                }
                color[node] = 1;
                stack.push((node, true));
                let items=sources[node].kind.items().ok_or_else(||issue("$","invalid_reference","container source missing"))?;
                for item in items.iter().rev() {
                    if sources[item.source].kind.container() {
                        stack.push((item.source, false));
                    }
                }
            }
        }
    }

    let mut session = Session::new();
    if let Some(preferences)=preferences{session.apply(Command::SetUserPreferences{preferences:preferences.clone()}).map_err(|error|issue("$","import_preferences",error.to_string()))?;}
    source_hotkeys::preflight(sources.iter().map(|source|(&source.playback_hotkeys,&source.audio_hotkeys)),session.snapshot().preferences())?;
    catalog.install(&mut session)?;
    let apply = |session: &mut Session, command| {
        session
            .apply(command)
            .map_err(|error| issue("$", "engine_validation", error.to_string()))
    };
    apply(
        &mut session,
        Command::SelectTransition {
            selection: transition,
        },
    )?;
    apply(
        &mut session,
        Command::ReplaceQuickTransitions { rows: quick_rows },
    )?;
    apply(
        &mut session,
        Command::SetVideo {
            width: video.width,
            height: video.height,
            fps_num: video.fps_num,
            fps_den: video.fps_den,
        },
    )?;
    apply(
        &mut session,
        Command::RenameCollection {
            name: collection_name,
        },
    )?;
    // Decode only after graph/order/source references and all unsupported
    // metadata have been checked. One prepared frame is retained at a time;
    // canonical authored data stores its immutable reference/hash, never pixels.
    // Metadata preflight is advisory under file replacement. The shared asset
    // reader charges actual bytes and enforces the cumulative 64-MiB read limit.
    // Resolve the imported authored family only after structural/graph validation.
    // DirectWrite resolves only the requested families. A missing, unsupported
    // or ambiguous face rejects the candidate before collection publication.
    if sources.iter().any(|source|matches!(source.kind,InputKind::Text{..})){
        let mut fonts=crate::obs_text_fonts::Resolver::new().map_err(|error|issue("$.sources",error.code,error.to_string()))?;
        for source in &mut sources{if let InputKind::Text{settings,system_family,..}=&mut source.kind{
            let font=fonts.resolve(system_family,settings.bold(),settings.italic()).map_err(|error|issue(&source.path,error.code,error.to_string()))?;
            *settings=settings.clone().with_system_font(font).map_err(|error|issue(&source.path,"text_font_invalid",error.to_string()))?;
        }}
    }
    let media_count=sources.iter().filter(|source|matches!(&source.kind,InputKind::Media{..})).count();
    if media_count>media::MAX_SOURCES{return Err(issue("$.sources","media_source_limit","OBS import exceeds the document source limit for local Media Sources"));}
    let media_reads=crate::mediafile::MediaReadBudget::new(media::MAX_READ_BYTES);
    let image_refs=sources.iter().try_fold(0usize,|sum,source|sum.checked_add(match &source.kind{InputKind::Image{..}=>1,InputKind::SlideShow{input,..}=>input.file_count(),_=>0})).ok_or_else(||issue("$.sources","image_limit","image-reference count overflow"))?;
    if image_refs>1024{return Err(issue("$.sources","image_limit","OBS image/slideshow import exceeds 1024 image references"));}
    let mut directory_budget=crate::image_slideshow::directories::Budget::default();let mut resolved_image_refs=0usize;
    for (index, source) in sources.iter_mut().enumerate() {
        if let InputKind::SlideShow{input,..}=&mut source.kind{input.preflight(&format!("{}.settings",source.path),&mut directory_budget)?;resolved_image_refs=resolved_image_refs.checked_add(input.file_count()).ok_or_else(||issue("$.sources","image_limit","expanded image count overflow"))?;}else if matches!(&source.kind,InputKind::Image{..}){resolved_image_refs+=1;}
        if resolved_image_refs>1024{return Err(issue("$.sources","image_limit","OBS image/slideshow expansion exceeds 1024 resolved images"));}
        if let InputKind::Image { file, .. } = &source.kind {
            let path = format!("{}.settings.file",source.path);
            let metadata = std::fs::metadata(file).map_err(|error| {
                issue(&path, "image_io", format!("{}: {error}", file.display()))
            })?;
            if metadata.len() > MAX_IMAGE_FILE_BYTES {
                return Err(issue(&path, "image_limit", "image file exceeds 32 MiB"));
            }
        }
    }
    let mut decoded_bytes = 0usize;
    let mut read_budget = crate::assets::ReadBudget::new(MAX_IMPORT_IMAGE_BYTES as u64);
    for (index, source) in sources.iter_mut().enumerate() {
        if let InputKind::SlideShow{input,..}=&mut source.kind{input.prepare(&format!("{}.settings",source.path),&mut read_budget,&mut decoded_bytes,&mut directory_budget)?;}
        if let InputKind::Image { file, asset, .. } = &mut source.kind {
            let path = format!("{}.settings.file",source.path);
            let prepared = crate::assets::prepare_image_with_limits(
                file,
                &mut read_budget,
                MAX_IMPORT_IMAGE_BYTES.saturating_sub(decoded_bytes),
            )
            .map_err(|error| issue(&path, error.code(), error.to_string()))?;
            decoded_bytes = decoded_bytes
                .checked_add(prepared.decoded_bytes)
                .ok_or_else(|| issue(&path, "limit", "decoded image byte accounting overflow"))?;
            if decoded_bytes > MAX_IMPORT_IMAGE_BYTES {
                return Err(issue(
                    &path,
                    "limit",
                    "OBS image import exceeds cumulative decoded-image budget 64 MiB",
                ));
            }
            *asset = Some(prepared.asset);
            drop(prepared.frame);
        }
    }
    for(index,source)in sources.iter_mut().enumerate(){
        if let InputKind::Media{input,..}=&mut source.kind {
            let path=format!("{}.settings.local_file",source.path);input.prepare(&path,&media_reads)?;
            if !input.has_audio() && (source.sync_offset_ms!=0 || source.force_mono || source.balance!=0.5 || !source.audio_filters.is_empty() || !source.audio_tracks.is_all() || !source.audio_hotkeys.is_empty() || !source.push_audio.is_default() || !source.monitoring.is_off()) {
                return Err(issue(&path,"media_audio_absent","OBS media audio controls, filters, sync offsets, shortcuts and nondefault track routing require an actual decoded audio track"));
            }
            if input.audio_only()&&!source.filters.is_empty(){return Err(issue(&path,"media_video_absent","Audio-only Media Sources cannot carry video filters or nonneutral video geometry"));}
        }
    }
    let audio_only:Vec<bool>=sources.iter().map(|source|matches!(&source.kind,InputKind::Media{input,..} if input.audio_only())||matches!(&source.kind,InputKind::Wasapi{..})).collect();
    let mut lut_reads=crate::lut_assets::LutReadBudget::new(64*1024*1024);
    let mut lut_assets:std::collections::HashMap<std::path::PathBuf,skylive_core::LutAsset>=std::collections::HashMap::new();let mut lut_bytes=0usize;
    for source in &mut sources{
        for (filter_index,filter) in source.filters.iter_mut().enumerate(){
            if let Some(file)=&filter.lut_path {
                let path=format!("{}.filters[{filter_index}].settings.image_path",source.path);
                let asset=if let Some(asset)=lut_assets.get(file){asset.clone()}else{
                    let asset=crate::lut_assets::prepare_lut_with_budget(file,&mut lut_reads).map_err(|error|issue(&path,error.code(),error.to_string()))?;
                    lut_bytes=lut_bytes.checked_add(asset.table_bytes()).filter(|bytes|*bytes<=64*1024*1024).ok_or_else(||issue(&path,"limit","OBS LUT import table metadata exceeds 64MiB"))?;
                    lut_assets.insert(file.clone(),asset.clone());asset
                };
                filter.filter=filter.filter.clone().with_lut_asset(Some(asset)).map_err(|error|issue(&path,"engine_validation",error.to_string()))?;
            }
            if let Some(file)=&filter.mask_path {
                let path=format!("{}.filters[{filter_index}].settings.image_path",source.path);
                let prepared=crate::assets::prepare_mask_image_with_limits(file,&mut read_budget,MAX_IMPORT_IMAGE_BYTES.saturating_sub(decoded_bytes)).map_err(|error|issue(&path,error.code(),error.to_string()))?;
                decoded_bytes=decoded_bytes.checked_add(prepared.decoded_bytes).ok_or_else(||issue(&path,"limit","mask import decoded byte overflow"))?;
                filter.filter=filter.filter.clone().with_mask_image(Some(prepared.asset)).map_err(|error|issue(&path,"engine_validation",error.to_string()))?;
                drop(prepared.frame);
            }
        }
    }
    let dimensions: Vec<_> = sources
        .iter()
        .map(|source| {
            if matches!(source.kind, InputKind::Scene { .. }) {
                Some((video.width, video.height))
            } else {
                source.kind.dimensions()
            }
        })
        .collect();
    for source in &mut sources {
        if let Some(items)=source.kind.items_mut() {
            for (item_index, item) in items.iter_mut().enumerate() {
                let path = format!("{}.settings.items[{item_index}]",source.path);
                if audio_only[item.source] {
                    // OBS retains inert geometry for audio inputs. This subset
                    // accepts only neutral data; it does not invent video extents.
                    let t=&item.transform;
                    if t.x!=0.0||t.y!=0.0||t.rotation_degrees!=0.0||item.scale!=(1.0,1.0)||t.crop!=Crop::default()||item.compositing!=ItemCompositing::default(){return Err(issue(&path,"media_video_absent","Audio-only Media Sources cannot carry video filters or nonneutral video geometry"));}
                    continue;
                }
                let (width, height) = dimensions[item.source].ok_or_else(|| {
                    issue(&path, "invalid_reference", "source dimensions unavailable")
                })?;
                let crop = item.transform.crop;
                if u64::from(crop.left) + u64::from(crop.right) >= u64::from(width)
                    || u64::from(crop.top) + u64::from(crop.bottom) >= u64::from(height)
                {
                    return Err(issue(
                        &path,
                        "invalid_crop",
                        "combined crop must leave at least one source pixel on both axes",
                    ));
                }
                item.transform.width = (f64::from(width) * item.scale.0.abs()) as f32;
                item.transform.height = (f64::from(height) * item.scale.1.abs()) as f32;
                item.transform
                    .geometry(width, height)
                    .map_err(|error| issue(&path, "invalid_transform", error.to_string()))?;
            }
        }
    }
    let first_scene = session.preview();
    // Free the default name before adding scenes in the actual OBS order.
    apply(
        &mut session,
        Command::RenameScene {
            scene: first_scene,
            name: sources[order[0]].name.clone(),
        },
    )?;
    let mut mapping = HashMap::new();
    mapping.insert(order[0], first_scene);
    for &index in order.iter().skip(1) {
        let created = apply(
            &mut session,
            Command::AddScene {
                name: sources[index].name.clone(),
            },
        )?
        .created
        .ok_or_else(|| issue("$", "engine_validation", "AddScene returned no identity"))?;
        mapping.insert(index, created);
    }
    let mut identities = ObsIdentities {
        session: session.identity(),
        root: root.clone(),
        sources: HashMap::new(),
        items: HashMap::new(),
        presets: catalog.metadata(),
        filters: HashMap::new(),
    };
    for &index in &order {
        let scene = mapping[&index];
        let show=sources[index].raw.get("private_settings").and_then(Value::as_object).and_then(|settings|settings.get("show_in_multiview")).and_then(Value::as_bool).unwrap_or(true);
        apply(&mut session,Command::SetSceneShowInMultiview {scene,show})?;
        let (reference, duration_ms) = transitions::scene_override(
            &sources[index].raw,
            &format!("$.sources[{index}]"),
            &catalog,
        )?;
        apply(
            &mut session,
            Command::ConfigureSceneTransitionSelection {
                scene,
                reference,
                duration_ms,
            },
        )?;
    }
    for (offset,(_,slot,_)) in globals.iter().enumerate(){
        let index=ordinary_end+offset;let id=groups::create_global(&mut session,*slot,&sources[index],&mut identities)?;mapping.insert(index,id);
    }
    groups::bootstrap(&mut session,&sources,&order,&mut mapping,&mut identities,&audio_only)?;
    // Resolve real weak keys only after every retained source ID exists.
    for (index,source) in sources.iter().enumerate(){
        if source.audio_filters.is_empty(){continue;}
        let id=*mapping.get(&index).ok_or_else(||issue("$.sources.filters","unsupported","compressor owner is not retained by this imported scene graph"))?;
        let filters=audio_filters::resolve(&source.audio_filters,&names,&mapping)?;
        apply(&mut session,Command::SetSourceAudioFilters{source:id,filters})?;
    }
    // Source controls belong to retained source identities, not item aliases.
    // Apply once after all IDs are known; Core rechecks every scope/limit.
    for(index,source)in sources.iter().enumerate(){if source.playback_hotkeys.is_empty(){continue;}let id=*mapping.get(&index).ok_or_else(||issue(&format!("$.sources[{index}].hotkeys"),"source_hotkey_identity","Playback hotkey owner was not retained by the imported scene graph"))?;session.apply(Command::SetSourcePlaybackHotkeys{source:id,bindings:source.playback_hotkeys.clone()}).map_err(|error|issue(&format!("$.sources[{index}].hotkeys"),"source_hotkey_scope_conflict",error.to_string()))?;}
    for(index,source)in sources.iter().enumerate(){if source.audio_hotkeys.is_empty(){continue;}
        let id=*mapping.get(&index).ok_or_else(||issue(&source.path,"source_audio_hotkey_identity","Mute shortcut owner was not retained by the imported scene graph"))?;
        session.apply(Command::SetSourceAudioHotkeys{source:id,bindings:source.audio_hotkeys.clone()}).map_err(|error|issue(&source.path,"source_hotkey_scope_conflict",error.to_string()))?;
    }
    // The construction commands are not user-edit history. Reopen the validated
    // candidate as one new unsaved document before selecting preview/program.
    session = Session::from_snapshot((*session.snapshot()).clone(), false)
        .map_err(|e| issue("$", "engine_validation", e.to_string()))?;
    identities.session = session.identity();
    session
        .select_preview(mapping[&program])
        .map_err(|e| issue("$", "engine_validation", e.to_string()))?;
    session
        .cut()
        .map_err(|e| issue("$", "engine_validation", e.to_string()))?;
    session
        .select_preview(mapping[&preview])
        .map_err(|e| issue("$", "engine_validation", e.to_string()))?;
    Ok(ImportedCollection {
        session,
        identities,
    })
}

fn generated_uuid(id: Id) -> String {
    // OBS uses collection-local UUID lookup. A stable custom UUID (version 8)
    // supplies identity for newly authored entities; no random UUID claim.
    format!(
        "00000000-0000-8000-{:04x}-{:012x}",
        0x8000u64 | (id.get() >> 48),
        id.get() & 0xffffffffffff
    )
}

/// Exports the supported subset. Imported sidecar metadata is optional; without
/// it new collection-local UUIDs are emitted. Intrinsic source dimensions remain
/// canonical core data and survive native storage independently of this sidecar.
/// Per-item opacity cannot be encoded by an OBS scene item and is rejected.
pub fn export_collection(
    session: &Session,
    identities: Option<&ObsIdentities>,
) -> Result<Vec<u8>, ObsError> {
    let collection = session.snapshot();
    export_snapshot(
        &collection,
        session.identity(),
        session.preview(),
        session.program(),
        session.is_closed(),
        identities,
    )
}

fn export_snapshot(
    collection: &StudioSnapshot,
    session: u64,
    preview: Id,
    program: Id,
    closed: bool,
    identities: Option<&ObsIdentities>,
) -> Result<Vec<u8>, ObsError> {
    if closed {
        return Err(issue("$", "closed", "cannot export a closed session"));
    }
    if identities.is_some_and(|ids| ids.session != session) {
        return Err(issue(
            "$",
            "identity_conflict",
            "OBS sidecar belongs to another session",
        ));
    }
    if !collection.saved_projectors().is_empty(){return Err(issue("$.saved_projectors","unsupported","native saved projector windows/physical geometry are not Qt saved_projectors interchange; use native persistence"));}
    collection
        .validate()
        .map_err(|error| issue("$", "engine_validation", error.to_string()))?;
    if collection
        .sources()
        .iter()
        .any(|source| !source.mixer_state().is_default())
    {
        return Err(issue(
            "$.sources",
            "unsupported",
            "mixer source view metadata OBS interchange is not implemented; use native persistence",
        ));
    }
    if collection
        .sources()
        .iter()
        .any(|source| source.audio_filters().iter().any(|filter|!matches!(filter,skylive_core::AudioFilter::ObsCompressor{..}|skylive_core::AudioFilter::ObsNoiseSuppression{..}|skylive_core::AudioFilter::ObsGain{..}|skylive_core::AudioFilter::ObsPolarityInvert{..}|skylive_core::AudioFilter::ObsNoiseGate{..}|skylive_core::AudioFilter::ObsLimiter{..}|skylive_core::AudioFilter::ObsExpander{..})))
        || collection
            .scenes()
            .iter().chain(collection.groups())
            .any(|scene| !scene.notes().is_empty())
    {
        return Err(issue(
            "$.plugins",
            "unsupported",
            "notes and unsupported source audio filters require native persistence",
        ));
    }
    let mut global_sources=HashMap::new();
    for root in collection.global_audio(){
        let key=GLOBAL_AUDIO_SLOTS.iter().find_map(|(key,slot)|(*slot==root.slot()).then_some(*key)).ok_or_else(||issue("$.global_audio","invalid_reference","Unknown global audio slot"))?;
        if global_sources.insert(root.source(),key).is_some(){return Err(issue("$.global_audio","global_audio_alias_unsupported","OBS loads each global slot as a separate source and cannot preserve one source shared across slots"));}
    }
    let referenced: HashSet<_> = collection
        .scenes()
        .iter().chain(collection.groups())
        .flat_map(|scene| scene.items().iter().map(|item| item.source()))
        .chain(collection.global_audio().iter().map(|root|root.source()))
        .collect();
    if collection.sources().iter().any(|source| {
        !matches!(source.kind(), SourceKind::Scene { .. }) && !referenced.contains(&source.id())
    }) {
        return Err(issue(
            "$.sources",
            "unsupported",
            "unreferenced sources are outside the interchange subset",
        ));
    }
    if collection.scenes().len() > MAX_IMPORT_SCENES
        || collection
            .sources()
            .iter()
            .filter(|source| !matches!(source.kind(), SourceKind::Scene { .. }))
            .count()
            > MAX_IMPORT_SOURCES
        || collection
            .scenes()
            .iter().chain(collection.groups())
            .map(|scene| scene.items().len())
            .sum::<usize>()
            > MAX_IMPORT_ITEMS
    {
        return Err(issue(
            "$",
            "limit",
            "collection exceeds the bounded interchange subset",
        ));
    }
    let mut root = identities.map(|ids| ids.root.clone()).unwrap_or_default();
    for (key,_) in GLOBAL_AUDIO_SLOTS {root.remove(key);}
    let mut names = HashSet::new();
    let mut uuids = HashMap::new();
    let mut seen_uuids = HashSet::new();
    let mut source_objects = HashMap::new();
    for (id, name) in collection
        .sources()
        .iter()
        .filter(|s| !matches!(s.kind(), SourceKind::Scene { .. }))
        .map(|s| (s.id(), s.name()))
        .chain(collection.scenes().iter().map(|s| (s.id(), s.name())))
    {
        if !names.insert(name) {
            return Err(issue(
                "$.sources",
                "unsupported",
                "OBS scene and source names must be globally unique",
            ));
        }
        let mut object = identities
            .and_then(|ids| ids.sources.get(&id))
            .cloned()
            .unwrap_or_default();
        let uuid = object
            .get("uuid")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| generated_uuid(id));
        if !uuid_valid(&uuid) || !seen_uuids.insert(uuid.clone()) {
            return Err(issue(
                "$.sources",
                "identity_conflict",
                "duplicate or invalid output UUID",
            ));
        }
        object.insert("name".into(), json!(name));
        object.insert("uuid".into(), json!(uuid));
        uuids.insert(id, uuid);
        source_objects.insert(id, object);
    }
    for source in collection.sources() {
        if let SourceKind::Scene { scene } = source.kind() {
            let uuid = uuids
                .get(scene)
                .ok_or_else(|| {
                    issue(
                        "$.sources",
                        "invalid_reference",
                        "scene source target missing",
                    )
                })?
                .clone();
            uuids.insert(source.id(), uuid);
        }
    }
    let mut output = Vec::new();
    let mut group_output=Vec::new();
    for source in collection
        .sources()
        .iter()
        .filter(|source| !matches!(source.kind(), SourceKind::Scene { .. }))
    {
        let mut object = source_objects
            .remove(&source.id())
            .ok_or_else(|| issue("$", "identity_conflict", "source identity missing"))?;
        source_hotkeys::export(source,&mut object,"$.sources")?;
        match source.kind() {
            SourceKind::Color {
                rgba,
                width,
                height,
            } => {
                if !(1..=4096).contains(width) || !(1..=4096).contains(height) {
                    return Err(issue(
                        "$.sources.settings",
                        "unsupported_dimensions",
                        "color source dimensions exceed the interchange subset 1..=4096",
                    ));
                }
                let versioned = if object.get("id").and_then(Value::as_str) == Some("color_source")
                {
                    object
                        .get("versioned_id")
                        .or_else(|| object.get("id"))
                        .cloned()
                        .unwrap_or(json!("color_source_v3"))
                } else {
                    json!("color_source_v3")
                };
                object.insert("id".into(), json!("color_source"));
                object.insert("versioned_id".into(), versioned);
                object.insert(
                    "settings".into(),
                    json!({"color":u32::from_le_bytes(*rgba),"width":width,"height":height}),
                );
            }
            SourceKind::MediaSource{media}=>{
                object.insert("id".into(),json!("ffmpeg_source"));object.insert("versioned_id".into(),json!("ffmpeg_source"));
                object.insert("settings".into(),media::export(media,"$.sources.settings")?);
            }
            SourceKind::Text{settings}=>{
                object.insert("id".into(),json!("text_gdiplus"));object.insert("versioned_id".into(),json!("text_gdiplus_v3"));
                object.insert("settings".into(),text::export(settings,"$.sources.settings")?);
            }
            SourceKind::Browser {browser}=>{
                let (local,path,url)=match browser.location(){skylive_core::BrowserLocation::Url {url}=>(false,String::new(),url.clone()),skylive_core::BrowserLocation::LocalFile {path}=>{if !Path::new(path).is_absolute(){return Err(issue("$.sources.settings.local_file","browser_path","OBS browser HTML export requires an absolute path on this host"));}(true,path.clone(),String::new())}};
                object.insert("id".into(),json!("browser_source"));object.insert("versioned_id".into(),json!("browser_source"));
                object.insert("settings".into(),json!({"is_local_file":local,"local_file":path,"url":url,"width":browser.width(),"height":browser.height(),"fps":browser.fps(),"fps_custom":true,"css":browser.css(),"shutdown":browser.shutdown_when_hidden(),"restart_when_active":browser.refresh_when_active(),"reroute_audio":browser.control_audio(),"webpage_control_level":0}));
            }
            SourceKind::ImageSlideShow{settings}=>{object.insert("id".into(),json!("slideshow"));object.insert("versioned_id".into(),json!(slideshow::versioned_id(settings)));object.insert("settings".into(),slideshow::export(settings,"$.sources.settings")?);}
            SourceKind::Image { asset } => {
                if !Path::new(asset.path()).is_absolute() {
                    return Err(issue(
                        "$.sources.settings.file",
                        "image_path",
                        format!(
                            "OBS image export requires an absolute path on this host: {}",
                            asset.path()
                        ),
                    ));
                }
                object.insert("id".into(), json!("image_source"));
                object.insert("versioned_id".into(), json!("image_source"));
                // Resolve from current canonical data, never the original raw
                // file setting: a source may have been rebound since import.
                let settings=source.image_settings();
                object.insert("settings".into(), json!({"file":asset.path(),"unload":settings.unload_when_hidden,"linear_alpha":settings.linear_alpha,"is_slide":false}));
            }
            SourceKind::DisplayCapture {binding} => {
                object.insert("id".into(),json!("monitor_capture"));
                object.insert("versioned_id".into(),json!("monitor_capture"));
                object.insert("settings".into(),display::export(binding,"$.sources.settings")?);
            }
            SourceKind::WindowCapture {binding} => {
                object.insert("id".into(),json!("window_capture"));
                object.insert("versioned_id".into(),json!("window_capture"));
                object.insert("settings".into(),window::export(binding,"$.sources.settings")?);
            }
            SourceKind::GameCapture {..}=>{
                return Err(issue("$.sources","game_capture_exact_target","Game Capture exact process creation-time/HWND identity and session authorization cannot be represented by OBS window-name matching; use native persistence and explicitly select the target in OBS."));
            }
            SourceKind::Group {group}=>{
                let scene=collection.scene_container(group.scene()).ok_or_else(||issue("$.groups","invalid_reference","Group container missing"))?;
                if scene.transition_override().is_some()||scene.transition_duration_ms()!=300{return Err(issue("$.groups.private_settings","unsupported","Private Group transition overrides are outside the represented OBS subset"));}
                object=groups::encode_container(collection,scene,object,identities,&uuids,true)?;
                object.insert("private_settings".into(),json!({}));
            }
            SourceKind::VideoCapture {binding}=>{
                object.insert("id".into(),json!("dshow_input"));
                object.insert("versioned_id".into(),json!("dshow_input"));
                object.insert("settings".into(),camera::export(binding,"$.sources.settings")?);
            }
            SourceKind::Scene { .. }
            | SourceKind::NdiSource {..} => {
                return Err(issue(
                    "$.sources",
                    "unsupported",
                    "this source provider is not supported by strict OBS interchange; use native persistence",
                ));
            }
            SourceKind::AudioCapture { binding } => {
                if binding.flow()==skylive_core::AudioFlow::Output&&!source.audio_monitoring().is_off(){return Err(issue("$.sources.monitoring_type","wasapi_monitoring_unsupported","OBS output capture self-monitoring suppression is not represented"));}
                if !source.video_filters().is_empty(){return Err(issue("$.sources.filters","wasapi_video_unsupported","WASAPI audio sources cannot carry video filters"));}
                if global_sources.contains_key(&source.id())&&matches!(binding.target(),skylive_core::AudioTarget::WindowsWindow{..}){return Err(issue("$.global_audio","global_audio_provider","OBS global audio slots require a device source"));}
                let (id,settings)=wasapi::export(binding,"$.sources.settings")?;
                object.insert("id".into(),json!(id));object.insert("versioned_id".into(),json!(id));object.insert("settings".into(),settings);
            }
        }
        let properties=source.audio_properties();
        let monitoring=source.audio_monitoring();
        if !monitoring.is_off()&&!cfg!(target_os="windows"){return Err(issue("$.sources.monitoring_type","monitoring_platform_unsupported","The represented physical monitoring backend currently requires Windows"));}
        if !monitoring.is_off()&&!source.kind().has_audio(){return Err(issue("$.sources.monitoring_type","monitoring_source_unsupported","Monitoring requires a represented audio-capable source"));}
        object.insert("monitoring_type".into(),json!(match monitoring{skylive_core::AudioMonitoringMode::Off=>0,skylive_core::AudioMonitoringMode::MonitorOnly=>1,skylive_core::AudioMonitoringMode::MonitorAndOutput=>2}));
        object.insert("flags".into(),json!(if properties.force_mono(){2}else{0}));
        object.insert("balance".into(),json!(properties.balance()));
        if !matches!(source.kind(),SourceKind::Group{..}){object.remove("canvas_uuid");}
        object.insert("mixers".into(),json!(source.audio_properties().audio_tracks().bits()));
        object.insert("sync".into(),json!(source.audio_properties().sync_offset_ns()));
        let push=source.audio_properties().push();
        object.insert("push-to-mute".into(),json!(push.mute_enabled()));object.insert("push-to-mute-delay".into(),json!(push.mute_delay_ms()));
        object.insert("push-to-talk".into(),json!(push.talk_enabled()));object.insert("push-to-talk-delay".into(),json!(push.talk_delay_ms()));
        object.insert("volume".into(), json!(source.gain()));
        object.insert("muted".into(), json!(source.muted()));
        object.insert(
            "filters".into(),
            filters::export(
                source.video_filters(),
                identities.map(|ids| &ids.filters),
                &mut seen_uuids,
                "$.sources",
            )?,
        );
        let mut filter_values=object.remove("filters").unwrap_or_else(||json!([]));
        audio_filters::append(&mut filter_values,source.audio_filters(),collection)?;
        object.insert("filters".into(),filter_values);
        if let Some(key)=global_sources.get(&source.id()){root.insert((*key).into(),Value::Object(object));}else if matches!(source.kind(),SourceKind::Group{..}){group_output.push(Value::Object(object));}else{output.push(Value::Object(object));}
    }
    for scene in collection.scenes() {
        let mut object = source_objects
            .remove(&scene.id())
            .ok_or_else(|| issue("$", "identity_conflict", "scene identity missing"))?;
        object=groups::encode_container(collection,scene,object,identities,&uuids,false)?;
        object.insert(
            "filters".into(),
            filters::export(
                collection
                    .scene_source(scene.id())
                    .map(|owner| owner.video_filters())
                    .unwrap_or(&[]),
                identities.map(|ids| &ids.filters),
                &mut seen_uuids,
                "$.sources.scene.filters",
            )?,
        );
        transitions::encode_override(&mut object, scene, collection)?;
        output.push(Value::Object(object));
    }
    root.insert("name".into(), json!(collection.name()));
    root.insert("version".into(), json!(1));
    root.remove("migration_resolution");
    root.insert("sources".into(), json!(output));
    root.insert("groups".into(),json!(group_output));
    root.insert(
        "quick_transitions".into(),
        transitions::encode_rows(collection)?,
    );
    root.insert(
        "current_transition".to_owned(),
        json!(transitions::reference_name(
            collection,
            collection.transition().reference()
        )?),
    );
    root.insert(
        "transitions".into(),
        transitions::encode_catalog(collection, identities.map(|identities| &identities.presets))?,
    );
    root.insert(
        "transition_duration".to_owned(),
        json!(collection.transition().duration_ms()),
    );
    root.insert(
        "scene_order".into(),
        json!(
            collection
                .scenes()
                .iter()
                .map(|scene| json!({"name":scene.name()}))
                .collect::<Vec<_>>()
        ),
    );
    root.insert(
        "current_scene".into(),
        json!(
            collection
                .scene(preview)
                .ok_or_else(|| issue("$", "invalid_reference", "preview scene missing"))?
                .name()
        ),
    );
    root.insert(
        "current_program_scene".into(),
        json!(
            collection
                .scene(program)
                .ok_or_else(|| issue("$", "invalid_reference", "program scene missing"))?
                .name()
        ),
    );
    if root.contains_key("resolution") {
        root.insert(
            "resolution".into(),
            json!({"x":collection.video().width(),"y":collection.video().height()}),
        );
    }
    let bytes = serde_json::to_vec_pretty(&Value::Object(root))
        .map_err(|error| issue("$", "encoding", error.to_string()))?;
    structural_budget(&bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn video() -> ImportVideo {
        ImportVideo {
            width: 1920,
            height: 1080,
            fps_num: 30,
            fps_den: 1,
        }
    }
    fn fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/obs-color-absolute.json")).unwrap()
    }
    fn encoded(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }
    #[test]
    fn builtin_transition_settings_import_export_and_captured_export_follow_shared_config() {
        for (name, kind) in [("Cut", TransitionKind::Cut), ("Fade", TransitionKind::Fade)] {
            let mut input = fixture();
            input["current_transition"] = json!(name);
            input["transition_duration"] = json!(750);
            let mut imported = import_collection(&encoded(&input), video()).unwrap();
            assert_eq!(
                imported
                    .session
                    .snapshot()
                    .resolve_transition(imported.session.snapshot().transition())
                    .unwrap()
                    .kind(),
                kind
            );
            assert_eq!(imported.session.snapshot().transition().duration_ms(), 750);
            let identities = Arc::new(imported.identities);
            let job = ExportJob::new(&imported.session, Some(identities.clone()));
            imported
                .session
                .apply(Command::SetTransition {
                    kind: TransitionKind::Fade,
                    duration_ms: 1250,
                })
                .unwrap();
            let captured: Value = serde_json::from_slice(&job.run().unwrap()).unwrap();
            assert_eq!(captured["current_transition"], name);
            assert_eq!(captured["transition_duration"], 750);
            let changed: Value = serde_json::from_slice(
                &export_collection(&imported.session, Some(identities.as_ref())).unwrap(),
            )
            .unwrap();
            assert_eq!(changed["current_transition"], "Fade");
            assert_eq!(changed["transition_duration"], 1250);
            let reopened = import_collection(&encoded(&changed), video()).unwrap();
            assert_eq!(
                reopened.session.snapshot().transition(),
                imported.session.snapshot().transition()
            );
        }
    }
    #[test]
    fn absent_zero_transition_defaults_and_unsupported_configuration_are_explicit() {
        let default = import_collection(&encoded(&fixture()), video()).unwrap();
        assert_eq!(
            default
                .session
                .snapshot()
                .resolve_transition(default.session.snapshot().transition())
                .unwrap()
                .kind(),
            TransitionKind::Fade
        );
        assert_eq!(default.session.snapshot().transition().duration_ms(), 300);
        let mut input = fixture();
        input["current_transition"] = json!("Cut");
        input["transition_duration"] = json!(0);
        let imported = import_collection(&encoded(&input), video()).unwrap();
        assert_eq!(
            imported
                .session
                .snapshot()
                .resolve_transition(imported.session.snapshot().transition())
                .unwrap()
                .kind(),
            TransitionKind::Cut
        );
        assert_eq!(imported.session.snapshot().transition().duration_ms(), 300);
        for (field, value) in [
            ("current_transition", json!("Stinger")),
            ("current_transition", json!(42)),
            ("transition_duration", json!(20_001)),
            ("transition_duration", json!(-1)),
            ("transition_duration", json!(49)),
        ] {
            let mut invalid = fixture();
            invalid[field] = value;
            let error = import_collection(&encoded(&invalid), video())
                .err()
                .unwrap();
            assert_eq!(error.issues[0].path, format!("$.{field}"));
        }
    }

    fn affine_fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/obs-color-affine.json")).unwrap()
    }

    fn image_fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/obs-image-absolute.json")).unwrap()
    }

    struct ImageDirectory(PathBuf);
    impl ImageDirectory {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SERIAL: AtomicU64 = AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "skylive-obs-image-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }

        fn png(&self, name: &str) -> PathBuf {
            // Valid 4x2 opaque-red RGBA8 PNG, embedded to avoid test-side
            // dependencies or any GUI/image-generating process.
            const PNG: &[u8] = &[
                137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 4, 0, 0, 0,
                2, 8, 6, 0, 0, 0, 127, 168, 125, 99, 0, 0, 0, 18, 73, 68, 65, 84, 120, 156, 99,
                248, 207, 192, 240, 31, 25, 51, 160, 11, 0, 0, 15, 33, 15, 241, 4, 55, 198, 159, 0,
                0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
            ];
            let path = self.0.join(name);
            std::fs::write(&path, PNG).unwrap();
            path
        }

        fn animated_gif(&self) -> PathBuf {
            let path = self.0.join("animated.gif");
            let mut bytes = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff".to_vec();
            for index in 0..2 {
                bytes.extend_from_slice(b"\x21\xf9\x04\x00\x0a\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02");
                bytes.extend_from_slice(&[if index == 0 { 0x44 } else { 0x4c }, 0x01, 0x00]);
            }
            bytes.push(b';');
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for ImageDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn source_derived_fixture_roundtrips_shared_references_order_and_identity() {
        let imported = import_collection(&encoded(&fixture()), video()).unwrap();
        let snapshot = imported.session.snapshot();
        assert_eq!(
            snapshot
                .scenes()
                .iter()
                .map(|s| s.name())
                .collect::<Vec<_>>(),
            ["Second", "First"]
        );
        assert_eq!(snapshot.sources().len(), 1);
        assert!(!imported.session.can_undo());
        assert!(imported.session.is_dirty());
        let source = snapshot.sources()[0].id();
        assert!(
            snapshot
                .scenes()
                .iter()
                .flat_map(|s| s.items())
                .all(|item| item.source() == source)
        );
        assert_eq!(
            snapshot.scenes()[0].items()[0].transform(),
            &Transform {
                x: 25.0,
                y: 40.0,
                width: 640.0,
                height: 240.0,
                opacity: 1.0,
                ..Transform::full(1920, 1080)
            }
        );
        assert!(snapshot.scenes()[0].items()[0].locked());
        assert!(!snapshot.scenes()[1].items()[0].visible());
        assert_eq!(
            snapshot.scene(imported.session.preview()).unwrap().name(),
            "First"
        );
        assert_eq!(
            snapshot.scene(imported.session.program()).unwrap().name(),
            "Second"
        );
        let bytes = export_collection(&imported.session, Some(&imported.identities)).unwrap();
        let exported: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            exported["sources"][0]["uuid"],
            fixture()["sources"][0]["uuid"]
        );
        assert_eq!(exported["sources"][1]["settings"]["items"][0]["id"], 7);
        let reopened = import_collection(&bytes, video()).unwrap();
        assert_eq!(
            reopened.session.snapshot().scenes()[0].items()[0].transform(),
            snapshot.scenes()[0].items()[0].transform()
        );
        assert_eq!(
            reopened.session.snapshot().sources()[0].kind(),
            snapshot.sources()[0].kind()
        );
    }

    #[test]
    fn duplicate_keys_are_rejected_before_obs_feature_validation() {
        let fixture = serde_json::to_string(&fixture()).unwrap();
        for replacement in [
            r#""filters":[{"id":"color_filter"}],"filters":[]"#,
            r#""filters":[{"id":"color_filter"}],"\u0066ilters":[]"#,
        ] {
            let bytes = fixture.replace(r#""filters":[]"#, replacement);
            let error = import_collection(bytes.as_bytes(), video()).err().unwrap();
            assert_eq!(error.issues[0].code, "duplicate_key");
            assert_eq!(error.issues[0].path, "$.sources[0].filters");
        }
    }

    #[test]
    fn shared_file_loader_resolves_collection_relative_images_and_preserves_io_failure() {
        let directory = ImageDirectory::new();
        directory.png("tiny.png");
        let path = directory.0.join("collection.json");
        std::fs::write(&path, encoded(&image_fixture())).unwrap();
        let imported = load_collection(&path, video()).unwrap();
        let snapshot = imported.session.snapshot();
        let SourceKind::Image { asset } = snapshot.sources()[0].kind() else {
            panic!("expected image")
        };
        assert_eq!(
            Path::new(asset.path()),
            directory.0.join("tiny.png").as_path()
        );
        let missing = directory.0.join("missing.json");
        let error = load_collection(&missing, video()).err().unwrap();
        assert_eq!(error.issues[0].code, "file_io");
        assert!(error.issues[0].message.contains("missing.json"));
    }

    #[cfg(unix)]
    #[test]
    fn shared_file_loader_uses_physical_collection_directory_through_symlink() {
        let directory = ImageDirectory::new();
        let physical = directory.0.join("physical");
        let aliases = directory.0.join("aliases");
        std::fs::create_dir(&physical).unwrap();
        std::fs::create_dir(&aliases).unwrap();
        directory.png("physical/tiny.png");
        let target = physical.join("collection.json");
        std::fs::write(&target, encoded(&image_fixture())).unwrap();
        let alias = aliases.join("collection.json");
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        let imported = load_collection(&alias, video()).unwrap();
        let snapshot = imported.session.snapshot();
        let SourceKind::Image { asset } = snapshot.sources()[0].kind() else {
            panic!("expected image")
        };
        assert_eq!(Path::new(asset.path()), physical.join("tiny.png").as_path());
    }

    #[test]
    fn obs_export_refuses_relative_native_asset_without_reinterpreting_cwd() {
        let mut session = Session::new();
        let scene = session.preview();
        let asset = ImageAsset::new("relative.png".into(), [7; 32], 4, 2).unwrap();
        session
            .apply(Command::AddImageSource {
                scene,
                name: "Relative native image".into(),
                asset,
            })
            .unwrap();
        let before = session.snapshot();
        let error = export_collection(&session, None).unwrap_err();
        assert_eq!(error.issues[0].code, "image_path");
        assert!(error.issues[0].message.contains("relative.png"));
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
    }

    #[test]
    fn actual_png_image_import_export_preserves_shared_refs_dimensions_and_crop() {
        let directory = ImageDirectory::new();
        directory.png("tiny.png");
        let imported =
            import_collection_at(&encoded(&image_fixture()), video(), &directory.0).unwrap();
        let snapshot = imported.session.snapshot();
        assert_eq!(snapshot.sources().len(), 1);
        let source = &snapshot.sources()[0];
        let SourceKind::Image { asset } = source.kind() else {
            panic!("expected image source")
        };
        assert_eq!((asset.width(), asset.height()), (4, 2));
        assert!(Path::new(asset.path()).is_absolute());
        assert_eq!(snapshot.scenes()[0].items().len(), 2);
        assert!(
            snapshot.scenes()[0]
                .items()
                .iter()
                .all(|item| item.source() == source.id())
        );
        let transform = snapshot.scenes()[0].items()[0].transform();
        assert_eq!(
            (
                transform.x,
                transform.y,
                transform.width,
                transform.height,
                transform.rotation_degrees,
                transform.flip_x
            ),
            (30.0, 20.0, 8.0, 6.0, 90.0, true)
        );
        assert_eq!(
            transform.crop,
            Crop {
                left: 1,
                right: 1,
                top: 0,
                bottom: 0
            }
        );
        let bytes = export_collection(&imported.session, Some(&imported.identities)).unwrap();
        let exported: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(exported["sources"][0]["id"], "image_source");
        assert_eq!(exported["sources"][0]["versioned_id"], "image_source");
        assert_eq!(
            exported["sources"][0]["uuid"],
            image_fixture()["sources"][0]["uuid"]
        );
        assert_eq!(exported["sources"][0]["settings"]["file"], asset.path());
        let reopened = import_collection(&bytes, video()).unwrap();
        assert_eq!(
            reopened.session.snapshot().sources()[0].kind(),
            source.kind()
        );
        assert_eq!(
            reopened.session.snapshot().scenes()[0].items()[0].transform(),
            transform
        );
        assert!(!imported.session.can_undo());
    }

    #[test]
    fn image_paths_require_explicit_base_and_export_uses_rebound_canonical_path() {
        let directory = ImageDirectory::new();
        directory.png("tiny.png");
        let bytes = encoded(&image_fixture());
        assert_eq!(
            import_collection(&bytes, video()).err().unwrap().issues[0].code,
            "relative_image_path"
        );
        assert_eq!(
            import_collection_at(&bytes, video(), Path::new("relative-base"))
                .err()
                .unwrap()
                .issues[0]
                .code,
            "invalid_base_directory"
        );
        let mut imported = import_collection_at(&bytes, video(), &directory.0).unwrap();
        let source = imported.session.snapshot().sources()[0].id();
        let replacement = crate::assets::prepare_image(&directory.png("replacement.png")).unwrap();
        let expected_path = replacement.asset.path().to_owned();
        imported
            .session
            .apply(Command::SetImageSource {
                source,
                asset: replacement.asset,
            })
            .unwrap();
        drop(replacement.frame);
        let exported: Value = serde_json::from_slice(
            &export_collection(&imported.session, Some(&imported.identities)).unwrap(),
        )
        .unwrap();
        assert_eq!(exported["sources"][0]["settings"]["file"], expected_path);
        assert_ne!(exported["sources"][0]["settings"]["file"], "tiny.png");
    }

    #[test]
    fn missing_images_unsupported_settings_and_failed_graph_do_not_publish() {
        let directory = ImageDirectory::new();
        let caller = Session::new();
        let before = caller.snapshot();
        let missing = import_collection_at(&encoded(&image_fixture()), video(), &directory.0)
            .err()
            .unwrap();
        assert_eq!(missing.issues[0].code, "image_io");
        assert_eq!(missing.issues[0].path, "$.sources[0].settings.file");
        for setting in ["linear_alpha", "unload", "is_slide"] {
            let mut value = image_fixture();
            value["sources"][0]["settings"][setting] = json!(true);
            assert_eq!(
                import_collection_at(&encoded(&value), video(), &directory.0)
                    .err()
                    .unwrap()
                    .issues[0]
                    .code,
                "unsupported"
            );
        }
        let mut invalid = image_fixture();
        invalid["sources"][1]["settings"]["items"][0]["blend_type"] = json!("additive");
        let error = import_collection_at(&encoded(&invalid), video(), &directory.0)
            .err()
            .unwrap();
        assert_eq!(error.issues[0].code, "unsupported");
        assert!(error.issues[0].path.ends_with(".blend_type"));
        assert!(Arc::ptr_eq(&before, &caller.snapshot()));
        assert!(!caller.can_undo());
    }

    #[test]
    fn animated_gif_obs_import_roundtrip_keeps_full_animation_resource() {
        let directory = ImageDirectory::new();
        let file = directory.animated_gif();
        let prepared = crate::assets::prepare_image(&file).unwrap();
        assert_eq!(prepared.decoded_bytes, 8);
        let mut value = image_fixture();
        value["sources"][0]["settings"]["file"] = json!("animated.gif");
        for key in ["crop_left", "crop_right"] {
            value["sources"][1]["settings"]["items"][0][key] = json!(0);
        }
        let imported = import_collection_at(&encoded(&value), video(), &directory.0).unwrap();
        let snapshot = imported.session.snapshot();
        let SourceKind::Image { asset } = snapshot.sources()[0].kind() else {
            panic!("expected GIF asset")
        };
        let clip = crate::assets::load_clip_with_budget(
            asset,
            &mut crate::assets::ReadBudget::new(1024),
            8,
        )
        .unwrap();
        assert_eq!(clip.frame_count(), 2);
        assert_eq!(
            clip.sample(skylive_media::MediaTime(0)).image.pixels(),
            &[0, 0, 0, 255]
        );
        assert_eq!(
            clip.sample(skylive_media::MediaTime(100_000_001))
                .image
                .pixels(),
            &[255, 255, 255, 255]
        );
        let bytes = export_collection(&imported.session, Some(&imported.identities)).unwrap();
        let reopened = import_collection(&bytes, video()).unwrap();
        assert_eq!(
            reopened.session.snapshot().sources()[0].kind(),
            snapshot.sources()[0].kind()
        );
    }

    #[test]
    fn unsupported_authored_data_is_rejected_without_touching_caller() {
        let caller = Session::new();
        let before = caller.snapshot();
        for (path, replacement) in [
            ("/sources/0/id", json!("image_source")),
            ("/sources/0/filters", json!([{"id":"color_filter"}])),
            ("/sources/1/settings/items/0/bounds_type", json!(1)),
            ("/sources/1/settings/items/0/blend_type", json!("additive")),
            ("/version", json!(3)),
        ] {
            let mut value = fixture();
            if let Some(target) = value.pointer_mut(path) {
                *target = replacement;
            } else {
                panic!("fixture field missing: {path}");
            }
            let error = import_collection(&encoded(&value), video()).err().unwrap();
            assert!(error.issues[0].code.starts_with("unsupported"));
            assert!(std::sync::Arc::ptr_eq(&before, &caller.snapshot()));
            assert!(!caller.can_undo());
        }
    }

    #[test]
    fn obs_rotation_signed_scale_and_crop_roundtrip_preserves_pivot_and_version() {
        for (flip_x, flip_y) in [(true, false), (false, true), (true, true), (false, false)] {
            let mut value = affine_fixture();
            value["sources"][1]["settings"]["items"][0]["scale"] =
                json!({"x":if flip_x {-2.0} else {2.0},"y":if flip_y {-1.5} else {1.5}});
            let imported = import_collection(&encoded(&value), video()).unwrap();
            let snapshot = imported.session.snapshot();
            let transform = snapshot.scenes()[0].items()[0].transform();
            assert_eq!(
                transform,
                &Transform {
                    x: 100.0,
                    y: 200.0,
                    width: 640.0,
                    height: 360.0,
                    opacity: 1.0,
                    rotation_degrees: 450.25,
                    flip_x,
                    flip_y,
                    crop: Crop {
                        left: 10,
                        top: 20,
                        right: 30,
                        bottom: 40
                    },
                }
            );
            // Cropping reduces local geometry without changing the stored
            // full-source extents or the authored top-left alignment pivot.
            assert_eq!(transform.width * (320.0 - 10.0 - 30.0) / 320.0, 560.0);
            assert_eq!(transform.height * (240.0 - 20.0 - 40.0) / 240.0, 270.0);
            let bytes = export_collection(&imported.session, Some(&imported.identities)).unwrap();
            let exported: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(exported["sources"][0]["versioned_id"], "color_source_v3");
            let item = &exported["sources"][1]["settings"]["items"][0];
            assert_eq!(item["rot"], 450.25);
            assert_eq!(
                item["scale"],
                value["sources"][1]["settings"]["items"][0]["scale"]
            );
            assert_eq!(item["crop_left"], 10);
            assert_eq!(item["crop_top"], 20);
            assert_eq!(item["crop_right"], 30);
            assert_eq!(item["crop_bottom"], 40);
            let reopened = import_collection(&bytes, video()).unwrap();
            assert_eq!(
                reopened.session.snapshot().scenes()[0].items()[0].transform(),
                transform
            );
        }
    }

    #[test]
    fn invalid_crop_zero_scale_and_out_of_range_rotation_are_rejected() {
        for (path, replacement, expected_code) in [
            (
                "/sources/1/settings/items/0/crop_left",
                json!(-1),
                "invalid_crop",
            ),
            (
                "/sources/1/settings/items/0/crop_left",
                json!(290),
                "invalid_crop",
            ),
            (
                "/sources/1/settings/items/0/crop_top",
                json!(200),
                "invalid_crop",
            ),
            (
                "/sources/1/settings/items/0/crop_right",
                json!(1.5),
                "invalid_crop",
            ),
            (
                "/sources/1/settings/items/0/scale/x",
                json!(0),
                "unsupported",
            ),
            (
                "/sources/1/settings/items/0/scale/y",
                json!(-0.0),
                "unsupported",
            ),
            (
                "/sources/1/settings/items/0/rot",
                json!(360000.1),
                "invalid_transform",
            ),
        ] {
            let mut value = affine_fixture();
            *value.pointer_mut(path).unwrap() = replacement;
            let error = import_collection(&encoded(&value), video()).err().unwrap();
            assert_eq!(error.issues[0].code, expected_code, "{path}");
        }
        let text = serde_json::to_string(&affine_fixture())
            .unwrap()
            .replace("450.25", "1e400");
        assert_eq!(
            import_collection(text.as_bytes(), video())
                .err()
                .unwrap()
                .issues[0]
                .code,
            "invalid_json"
        );
    }

    #[test]
    fn unknown_meaningful_values_and_broken_uuid_are_not_dropped() {
        let mut value = fixture();
        value["sources"][0]["settings"]["new_feature"] = json!({"important":true});
        assert_eq!(
            import_collection(&encoded(&value), video())
                .err()
                .unwrap()
                .issues[0]
                .code,
            "unsupported"
        );
        let mut value = fixture();
        value["sources"][1]["settings"]["items"][0]["source_uuid"] =
            json!("00000000-0000-4000-8000-000000000099");
        assert_eq!(
            import_collection(&encoded(&value), video())
                .err()
                .unwrap()
                .issues[0]
                .code,
            "invalid_reference"
        );
    }

    #[test]
    fn export_rejects_per_item_opacity_and_wrong_session_sidecar() {
        let mut imported = import_collection(&encoded(&fixture()), video()).unwrap();
        let snapshot = imported.session.snapshot();
        let scene = &snapshot.scenes()[1];
        let item = &scene.items()[0];
        let mut transform = item.transform().clone();
        transform.opacity = 0.5;
        imported
            .session
            .apply(Command::SetTransform {
                scene: scene.id(),
                item: item.id(),
                transform,
            })
            .unwrap();
        assert_eq!(
            export_collection(&imported.session, Some(&imported.identities))
                .unwrap_err()
                .issues[0]
                .code,
            "unsupported"
        );
        assert_eq!(
            export_collection(&Session::new(), Some(&imported.identities))
                .unwrap_err()
                .issues[0]
                .code,
            "identity_conflict"
        );
    }

    #[test]
    fn malformed_and_over_budget_input_fails_at_boundary() {
        assert_eq!(
            import_collection(b"{", video()).err().unwrap().issues[0].code,
            "invalid_json"
        );
        assert_eq!(
            import_collection(&[b'['; 33], video())
                .err()
                .unwrap()
                .issues[0]
                .code,
            "limit"
        );
        assert_eq!(
            import_collection(&vec![b' '; MAX_FILE_BYTES as usize + 1], video())
                .err()
                .unwrap()
                .issues[0]
                .code,
            "limit"
        );
    }

    #[test]
    fn color_versions_use_obs_defaults_and_do_not_upgrade_on_export() {
        for (version, expected_size, expected_rgba) in [
            (None, (400, 400), [255, 255, 255, 255]),
            (Some("color_source_v2"), (1920, 1080), [255, 255, 255, 255]),
            (Some("color_source_v3"), (1920, 1080), [209, 209, 209, 255]),
        ] {
            let mut value = fixture();
            value["sources"][0]["settings"] = json!({});
            if let Some(version) = version {
                value["sources"][0]["versioned_id"] = json!(version);
            } else {
                value["sources"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("versioned_id");
            }
            let imported = import_collection(&encoded(&value), video()).unwrap();
            assert_eq!(
                imported.session.snapshot().sources()[0].kind(),
                &SourceKind::Color {
                    rgba: expected_rgba,
                    width: expected_size.0,
                    height: expected_size.1
                }
            );
            let output: Value = serde_json::from_slice(
                &export_collection(&imported.session, Some(&imported.identities)).unwrap(),
            )
            .unwrap();
            assert_eq!(
                output["sources"][0]["versioned_id"],
                version.unwrap_or("color_source")
            );
        }
    }

    #[test]
    fn new_native_collection_exports_as_an_obs_subset() {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddColorSource {
                scene,
                name: "Color".into(),
                rgba: [1, 2, 3, 255],
            })
            .unwrap();
        let bytes = export_collection(&session, None).unwrap();
        let imported = import_collection(&bytes, video()).unwrap();
        assert_eq!(
            imported.session.snapshot().sources()[0].kind(),
            &SourceKind::Color {
                rgba: [1, 2, 3, 255],
                width: 1920,
                height: 1080
            }
        );
    }

    #[test]
    fn export_job_keeps_captured_revision_and_selection_across_later_edits() {
        let mut imported = import_collection(&encoded(&fixture()), video()).unwrap();
        let before = imported.session.snapshot();
        let revision = before.revision();
        let job = ExportJob::new(&imported.session, Some(Arc::new(imported.identities)));
        // Capturing is an Arc clone, not an O(document size) model copy.
        assert!(Arc::ptr_eq(&before, &job.collection));
        imported
            .session
            .apply(Command::RenameCollection {
                name: "Later revision".into(),
            })
            .unwrap();
        imported
            .session
            .select_preview(before.scenes()[0].id())
            .unwrap();
        assert!(imported.session.snapshot().revision() > revision);
        let output: Value = serde_json::from_slice(&job.run().unwrap()).unwrap();
        assert_eq!(output["name"], "OBS color subset fixture");
        assert_eq!(output["current_scene"], "First");
        assert_eq!(imported.session.snapshot().name(), "Later revision");
    }

    #[test]
    fn export_job_rejects_foreign_session_sidecar_on_worker() {
        let imported = import_collection(&encoded(&fixture()), video()).unwrap();
        let job = ExportJob::new(&Session::new(), Some(Arc::new(imported.identities)));
        assert_eq!(job.run().unwrap_err().issues[0].code, "identity_conflict");
    }
    #[test]
    fn export_rejects_dimensions_outside_its_import_subset() {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddColorSource {
                scene,
                name: "Wide".into(),
                rgba: [255; 4],
            })
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        session
            .apply(Command::SetColorSize {
                source,
                width: 8192,
                height: 1080,
            })
            .unwrap();
        assert_eq!(
            export_collection(&session, None).unwrap_err().issues[0].code,
            "unsupported_dimensions"
        );
    }
    #[test]
    fn export_scale_roundtrip_preserves_transform_range_boundaries() {
        let mut session = Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddColorSource {
                scene,
                name: "Boundary".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        session
            .apply(Command::SetColorSize {
                source,
                width: 2077,
                height: 2077,
            })
            .unwrap();
        for extent in [0.001f32, 16384.0f32] {
            session
                .apply(Command::SetTransform {
                    scene,
                    item,
                    transform: Transform {
                        x: 0.0,
                        y: 0.0,
                        width: extent,
                        height: extent,
                        opacity: 1.0,
                        ..Transform::full(1920, 1080)
                    },
                })
                .unwrap();
            let encoded = export_collection(&session, None).unwrap();
            let decoded = import_collection(&encoded, video()).unwrap();
            assert_eq!(
                decoded.session.snapshot().scenes()[0].items()[0]
                    .transform()
                    .width,
                extent
            );
            assert_eq!(
                decoded.session.snapshot().scenes()[0].items()[0]
                    .transform()
                    .height,
                extent
            );
        }
    }
}
