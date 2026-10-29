//! One bounded owner for display enumeration and live SDR frame acquisition.
//! OS adapters run only on its worker. Closing invalidates publication without
//! claiming a finite cancellation/join bound for an outstanding OS call.
use crate::transition::FrameScene;
use crate::window_capture::{WindowCaptureError, WindowInfo};
use skylive_core::{
    DisplayCaptureBinding, DisplayTarget, Id, Session, SourceKind, VideoCaptureBinding,
    WindowCaptureBinding,
};
use skylive_media::{MediaTime, RgbaFrame};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, mpsc},
    thread,
};

#[cfg(target_os = "windows")]
pub(crate) mod wgc_engine;
#[cfg(target_os = "windows")]
pub(crate) mod dxgi_cursor;
mod media_union;
pub mod native;
#[cfg(any(target_os = "windows", test))]
pub(crate) mod native_budget;
mod transform;
mod union;
mod browser_union;
pub use browser_union::BrowserCaptureUnion;
pub use media_union::MediaCaptureUnion;
mod game_union;
pub use game_union::GameCaptureUnion;
mod ndi_union;
pub use ndi_union::NdiCaptureUnion;
pub use transform::{
    CaptureTransformContext, CaptureTransformDraft, prepare_capture_transform,
    project_capture_transform,
};
pub use union::CaptureUnion;
/// Program A/B plus Preview can each retain a complete source generation.
pub const MAX_CAPTURE_REFERENCES: usize = 3 * skylive_core::MAX_SOURCES;

#[derive(Clone, Copy, Debug)]
pub struct CaptureLimits {
    pub max_displays: usize,
    pub max_sources: usize,
    pub max_frame_bytes: usize,
    pub max_total_frame_bytes: usize,
    pub max_raw_bytes: usize,
}
impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            max_displays: 64,
            max_sources: MAX_CAPTURE_REFERENCES,
            max_frame_bytes: 128 * 1024 * 1024,
            max_total_frame_bytes: 256 * 1024 * 1024,
            max_raw_bytes: 384 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct CaptureCapabilities {
    pub available: bool,
    pub backend: &'static str,
    pub cursor: bool,
    pub hdr: bool,
    pub note: Option<&'static str>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DisplayInfo {
    pub target: DisplayTarget,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}
impl DisplayInfo {
    pub fn binding(&self, cursor: bool) -> Result<DisplayCaptureBinding, CaptureError> {
        DisplayCaptureBinding::new(self.target.clone(), cursor, self.width, self.height)
            .map_err(|_| CaptureError::InvalidFrame("invalid display binding"))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureError {
    Unavailable(&'static str),
    PermissionDenied,
    TargetNotFound(DisplayTarget),
    InvalidFrame(&'static str),
    BudgetExceeded(&'static str),
    Backend(String),
    Busy,
    Closed,
    Disconnected,
    IdentityExhausted,
    ForeignSession,
    TimeDiscontinuity,
    TimeOverflow,
    Engine(skylive_core::Error),
    Window(WindowCaptureError),
    Camera(crate::camera_capture::CameraError),
}
impl CaptureError {
    pub fn issue_kind(&self) -> crate::resource_issue::ResourceIssueKind {
        use crate::resource_issue::ResourceIssueKind::*;
        match self {
            Self::BudgetExceeded(_) => Limit,
            Self::IdentityExhausted
            | Self::ForeignSession
            | Self::TimeDiscontinuity
            | Self::TimeOverflow
            | Self::Engine(_) => Invalid,
            Self::Window(e) => e.issue_kind(),
            Self::Camera(e) => e.issue_kind(),
            _ => Unavailable,
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "capture_unavailable",
            Self::PermissionDenied => "capture_permission_denied",
            Self::TargetNotFound(_) => "capture_target_not_found",
            Self::InvalidFrame(_) => "capture_invalid_frame",
            Self::BudgetExceeded(_) => "capture_budget_exceeded",
            Self::Backend(_) => "capture_backend_error",
            Self::Busy => "capture_busy",
            Self::Closed => "capture_closed",
            Self::Disconnected => "capture_disconnected",
            Self::IdentityExhausted => "capture_identity_exhausted",
            Self::ForeignSession => "capture_foreign_session",
            Self::TimeDiscontinuity => "capture_time_discontinuity",
            Self::TimeOverflow => "capture_time_overflow",
            Self::Engine(error) => error.code(),
            Self::Window(error) => error.code(),
            Self::Camera(error) => error.code(),
        }
    }
}
impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(detail)
            | Self::InvalidFrame(detail)
            | Self::BudgetExceeded(detail) => write!(f, "{}: {detail}", self.code()),
            Self::Backend(detail) => write!(f, "{}: {detail}", self.code()),
            Self::TargetNotFound(target) => write!(f, "{}: {target:?}", self.code()),
            Self::Engine(error) => error.fmt(f),
            Self::Window(error) => error.fmt(f),
            Self::Camera(error) => error.fmt(f),
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for CaptureError {}
impl From<WindowCaptureError> for CaptureError {
    fn from(error: WindowCaptureError) -> Self {
        Self::Window(error)
    }
}
impl From<crate::camera_capture::CameraError> for CaptureError {
    fn from(error: crate::camera_capture::CameraError) -> Self {
        Self::Camera(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(tag = "kind", content = "binding", rename_all = "snake_case")]
pub enum CaptureBinding {
    Display(DisplayCaptureBinding),
    Window(WindowCaptureBinding),
    Video(Arc<VideoCaptureBinding>),
    Media(Arc<skylive_core::MediaSource>),
    Game(Arc<skylive_core::GameCaptureBinding>),
    Ndi(Arc<skylive_core::NdiSource>),
    Browser(Arc<skylive_core::BrowserSource>),
}
impl From<DisplayCaptureBinding> for CaptureBinding {
    fn from(value: DisplayCaptureBinding) -> Self {
        Self::Display(value)
    }
}
impl From<WindowCaptureBinding> for CaptureBinding {
    fn from(value: WindowCaptureBinding) -> Self {
        Self::Window(value)
    }
}
impl From<VideoCaptureBinding> for CaptureBinding {
    fn from(value: VideoCaptureBinding) -> Self {
        Self::Video(Arc::new(value.video_only()))
    }
}
impl PartialEq<DisplayCaptureBinding> for CaptureBinding {
    fn eq(&self, other: &DisplayCaptureBinding) -> bool {
        matches!(self,Self::Display(value) if value==other)
    }
}
impl PartialEq<WindowCaptureBinding> for CaptureBinding {
    fn eq(&self, other: &WindowCaptureBinding) -> bool {
        matches!(self,Self::Window(value) if value==other)
    }
}
impl CaptureBinding {
    pub fn from_source(kind: &SourceKind) -> Option<Self> {
        match kind {
            SourceKind::GameCapture {binding}=>Some(Self::Game(Arc::new(binding.clone()))),
            SourceKind::NdiSource {ndi}=>Some(Self::Ndi(Arc::new(ndi.clone()))),
            SourceKind::Browser {browser}=>Some(Self::Browser(Arc::new(browser.clone()))),
            SourceKind::DisplayCapture { binding } => Some(Self::Display(binding.clone())),
            SourceKind::WindowCapture { binding } => Some(Self::Window(binding.clone())),
            SourceKind::VideoCapture { binding } => Some(Self::Video(Arc::new(binding.video_only()))),
            SourceKind::MediaSource { media } if media.video_dimensions().is_some() => {
                Some(Self::Media(Arc::new(media.clone())))
            }
            _ => None,
        }
    }
    pub fn matches_source(&self, kind: &SourceKind) -> bool {
        match (self, kind) {
            (Self::Display(a), SourceKind::DisplayCapture { binding: b }) => a == b,
            (Self::Window(a), SourceKind::WindowCapture { binding: b }) => a == b,
            (Self::Video(a), SourceKind::VideoCapture { binding: b }) => a.same_video(b),
            (Self::Media(a), SourceKind::MediaSource { media: b }) => a.as_ref() == b,
            (Self::Game(a),SourceKind::GameCapture {binding:b})=>a.as_ref()==b,
            (Self::Ndi(a),SourceKind::NdiSource {ndi:b})=>a.as_ref()==b,
            (Self::Browser(a),SourceKind::Browser {browser:b})=>a.as_ref()==b,
            _ => false,
        }
    }
    pub fn width(&self) -> u32 {
        match self {
            Self::Display(v) => v.width(),
            Self::Window(v) => v.width(),
            Self::Video(v) => v.width(),
            Self::Media(v) => v.width(),
            Self::Game(v)=>v.width(),
            Self::Ndi(v)=>v.width(),
            Self::Browser(v)=>v.width(),
        }
    }
    pub fn height(&self) -> u32 {
        match self {
            Self::Display(v) => v.height(),
            Self::Window(v) => v.height(),
            Self::Video(v) => v.height(),
            Self::Media(v) => v.height(),
            Self::Game(v)=>v.height(),
            Self::Ndi(v)=>v.height(),
            Self::Browser(v)=>v.height(),
        }
    }
    pub fn capture_cursor(&self) -> bool {
        match self {
            Self::Display(v) => v.capture_cursor(),
            Self::Window(v) => v.capture_cursor(),
            Self::Video(_) | Self::Media(_) | Self::Ndi(_) | Self::Browser(_) | Self::Game(_) => false,
        }
    }
    fn resource_key(&self) -> Self {
        match self {
            Self::Video(value) => Self::Video(value.clone()),
            Self::Media(value) => Self::Media(value.clone()),
            Self::Game(value)=>Self::Game(value.clone()),
            Self::Ndi(value)=>Self::Ndi(value.clone()),
            Self::Browser(value)=>Self::Browser(value.clone()),
            Self::Display(v) => Self::Display(
                DisplayCaptureBinding::new(v.target().clone(), v.capture_cursor(), 1, 1)
                    .and_then(|binding| binding.with_method(v.method()))
                    .expect("validated display key"),
            ),
            Self::Window(v) => Self::Window(
                WindowCaptureBinding::new(
                    v.target().clone(),
                    v.priority(),
                    v.capture_cursor(),
                    v.client_area(),
                    1,
                    1,
                )
                .and_then(|value| value.with_method(v.method()))
                .expect("validated window key"),
            ),
        }
    }
}

pub trait CaptureBackend: Send + 'static {
    fn retain_display_bindings(
        &mut self,
        _session: u64,
        _epoch: u64,
        _bindings: &[DisplayCaptureBinding],
    ) {
    }
    fn capture_display(
        &mut self,
        binding: &DisplayCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<DisplayCaptureOutcome, CaptureError> {
        if matches!(binding.method(), skylive_core::DisplayCaptureMethod::Dxgi | skylive_core::DisplayCaptureMethod::Wgc) {
            return Err(CaptureError::Unavailable(
                "this backend does not implement the selected hardware display method",
            ));
        }
        self.capture(binding, limits)
            .map(|image| DisplayCaptureOutcome::Frame {
                image,
                metadata: None,
            })
    }
    fn retain_window_bindings(&mut self, _session: u64, _bindings: &[WindowCaptureBinding]) {}
    fn retain_window_epoch(
        &mut self,
        session: u64,
        _epoch: u64,
        bindings: &[WindowCaptureBinding],
    ) {
        self.retain_window_bindings(session, bindings)
    }
    fn reap_native_resources(&mut self) -> bool {
        false
    }
    fn capture_window_outcome(
        &mut self,
        binding: &WindowCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<crate::window_capture::WindowCaptureOutcome, CaptureError> {
        if matches!(binding.target(), skylive_core::WindowTarget::Windows { .. })
            && binding.method() == skylive_core::WindowCaptureMethod::Wgc
        {
            return Err(
                WindowCaptureError::Unavailable("this backend does not implement WGC").into(),
            );
        }
        self.capture_window(binding, limits).map(|image| {
            crate::window_capture::WindowCaptureOutcome::Frame {
                image,
                metadata: crate::window_capture::WindowFrameMetadata {
                    backend: if matches!(
                        binding.target(),
                        skylive_core::WindowTarget::Windows { .. }
                    ) {
                        crate::window_capture::WindowCaptureBackendKind::Gdi
                    } else {
                        crate::window_capture::WindowCaptureBackendKind::ScreenCaptureKit
                    },
                    generation: 1,
                    encoding: crate::window_capture::WindowFrameEncoding::OpaqueSrgb,
                    native_window: None,
                },
            }
        })
    }
    fn window_capabilities(&self) -> CaptureCapabilities {
        crate::window_capture::WindowCaptureBackend::capabilities(
            &crate::window_capture::UnsupportedBackend,
        )
    }
    fn enumerate_windows(&mut self, _: CaptureLimits) -> Result<Vec<WindowInfo>, CaptureError> {
        Err(WindowCaptureError::Unavailable("window capture is unavailable on this backend").into())
    }
    fn capture_window(
        &mut self,
        _: &WindowCaptureBinding,
        _: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError> {
        Err(WindowCaptureError::Unavailable("window capture is unavailable on this backend").into())
    }
    fn capabilities(&self) -> CaptureCapabilities;
    fn enumerate(&mut self, limits: CaptureLimits) -> Result<Vec<DisplayInfo>, CaptureError>;
    fn capture(
        &mut self,
        binding: &DisplayCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError>;
}
pub struct UnsupportedBackend;
impl CaptureBackend for UnsupportedBackend {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            available: false,
            backend: "unsupported",
            cursor: false,
            hdr: false,
            note: Some("display capture is not available on this host"),
        }
    }
    fn enumerate(&mut self, _: CaptureLimits) -> Result<Vec<DisplayInfo>, CaptureError> {
        Err(CaptureError::Unavailable(
            "display capture is not available on this host",
        ))
    }
    fn capture(
        &mut self,
        _: &DisplayCaptureBinding,
        _: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError> {
        Err(CaptureError::Unavailable(
            "display capture is not available on this host",
        ))
    }
}
/// Actual source-local provider identity. Aggregate union epochs are not a
/// substitute: adding an unrelated source must not reset this source's history.
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,serde::Serialize)]
pub enum NativeFrameOrigin {
    Camera{owner:u64,stream:u64},
    Media{owner:u64,stream:u64},
    Ndi{owner:u64,stream:u64},
    Browser{owner:u64,native:u64,lease:u64,binding:u64},
    Game{owner:u64,generation:u64},
}
/// Source-owned evidence for the resource stages already applied to these
/// pixels. The stage marker retains its admitted metadata budget.
#[derive(Clone, Debug)]
pub(crate) struct StagedCameraProof {
    pub(crate) upstream: Arc<[skylive_core::VideoFilter]>,
    pub(crate) processed: crate::camera_capture::staged_delay_worker::ProcessedStages,
    pub(crate) blend: skylive_media::SourceBlend,
    pub(crate) generation:u64,
    pub(crate) live_generation:Arc<std::sync::atomic::AtomicU64>,
    pub(crate) mask_live:Option<Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) scroll_live:Option<Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) mask_resources:Option<Arc<crate::video_filters::StagedMaskSet>>,
}
/// Evidence for NDI stages, separately scoped to the sender clock and stream
/// carried by the enclosing receipt. It is never a camera timing proof.
#[derive(Clone, Debug)]
pub(crate) struct StagedNdiProof {
    pub(crate) upstream: Arc<[skylive_core::VideoFilter]>,
    pub(crate) processed: crate::camera_capture::staged_delay_worker::ProcessedStages,
    pub(crate) blend: skylive_media::SourceBlend,
    pub(crate) generation:u64,
    pub(crate) live_generation:Arc<std::sync::atomic::AtomicU64>,
    pub(crate) mask_live:Option<Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) scroll_live:Option<Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) mask_resources:Option<Arc<crate::video_filters::StagedMaskSet>>,
}
/// Evidence that the camera worker applied this exact ordered async chain to
/// this sample. The age is measured only in the camera's native PTS domain.
#[derive(Clone, Debug)]
pub struct AsyncDelayReceipt {
    pub(crate) session: u64,
    pub(crate) binding_generation: u64,
    pub(crate) live_generation: Option<Arc<std::sync::atomic::AtomicU64>>,
    pub(crate) history_generation: u64,
    pub(crate) history_live: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) source: Id,
    pub(crate) owner: u64,
    pub(crate) stream: u64,
    pub(crate) clock_epoch: u64,
    pub(crate) sequence: u64,
    pub(crate) timestamp_ns: i128,
    /// Sender clock domain for NDI; camera timestamps do not use this domain.
    pub(crate) ndi_native_clock: Option<crate::ndi::NdiVideoClock>,
    /// NDI's sender clock mapped to the local media clock using packet arrival.
    /// This is an estimate with unbounded network uncertainty, not capture-time proof.
    pub(crate) ndi_media_time_estimate_ns: Option<i128>,
    pub(crate) filters: Arc<[(skylive_core::VideoFilterId, u16)]>,
    pub(crate) staged_camera: Option<StagedCameraProof>,
    pub(crate) staged_ndi: Option<StagedNdiProof>,
    pub elapsed_ns: u64,
}
impl AsyncDelayReceipt {
    pub(crate) fn current(&self) -> bool {
        self.history_live.load(std::sync::atomic::Ordering::Acquire)
            && self.staged_camera.as_ref().is_none_or(|proof|proof.scroll_live.as_ref().is_none_or(|live|live.load(std::sync::atomic::Ordering::Acquire)))
            && self.staged_ndi.as_ref().is_none_or(|proof|proof.scroll_live.as_ref().is_none_or(|live|live.load(std::sync::atomic::Ordering::Acquire)))
            && self.live_generation.as_ref().is_some_and(|live|
                live.load(std::sync::atomic::Ordering::Acquire) == self.binding_generation)
    }
}
#[derive(Clone, Debug)]
pub struct CapturedFrame {
    /// None is the stateless display/window path or an explicitly synthetic fixture.
    pub native_origin:Option<NativeFrameOrigin>,
    pub binding: CaptureBinding,
    pub sequence: u64,
    pub requested_at: MediaTime,
    /// Original presentation timestamp in nanoseconds in the clock domain of
    /// `native_origin`; it is not necessarily project time or unsigned. `None`
    /// means the acquisition adapter has not supplied a native timestamp.
    pub native_timestamp_ns: Option<i128>,
    /// Generation of the native source clock, scoped by `native_origin`.
    /// A changed value invalidates timestamp-based history even if a snapshot
    /// skipped the frame which first reported the discontinuity.
    pub native_clock_epoch: Option<u64>,
    pub device_qpc_ns: Option<u64>,
    pub directshow_clock: Option<skylive_media::DirectShowClockSnapshot>,
    pub async_delay: Option<AsyncDelayReceipt>,
    /// Raw native camera sample for the bounded modal filter preview only.
    pub(crate) draft_raw:Option<Arc<CapturedFrame>>,
    /// A delayed frame retains its queue credit for as long as this snapshot
    /// or any consumer owns its pixels.
    pub image: skylive_media::SharedVideoFrame,
    pub display: Option<DisplayFrameMetadata>,
    pub window: Option<crate::window_capture::WindowFrameMetadata>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayCaptureBackend {
    Gdi,
    Dxgi,
    Wgc,
    ScreenCaptureKit,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DisplayFrameMetadata {
    pub backend: DisplayCaptureBackend,
    pub fallback_reason: Option<String>,
    /// Native WGC lifetime; pending frames may only retain this generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
}
pub enum DisplayCaptureOutcome {
    Frame {
        image: RgbaFrame,
        metadata: Option<DisplayFrameMetadata>,
    },
    NoNewFrame,
    Pending { generation: u64 },
}
#[derive(Clone)]
enum CapturedOutcome {
    Frame {
        image: Arc<RgbaFrame>,
        display: Option<DisplayFrameMetadata>,
        window: Option<crate::window_capture::WindowFrameMetadata>,
    },
    NoNewFrame,
    WindowPending {
        generation: u64,
    },
    DisplayPending { generation: u64 },
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CaptureProblem {
    pub kind: crate::resource_issue::ResourceIssueKind,
    pub source: Id,
    pub code: &'static str,
    pub detail: String,
}
#[derive(Clone, Debug)]
struct DraftCapture {frame: CapturedFrame, epoch:u64, live:Arc<std::sync::atomic::AtomicU64>, stream:Option<(u64,Arc<std::sync::atomic::AtomicU64>)>}
// Admit optional modal samples only after live inputs: memory pressure may
// remove a draft, but must not make Program or a recording fail admission.
fn admit_draft_frames(candidates:&HashMap<Id,DraftCapture>,frames:&HashMap<Id,CapturedFrame>,
    seen:&mut std::collections::HashSet<usize>,bytes:&mut usize)->HashMap<Id,DraftCapture> {
    let mut admitted=HashMap::new();
    for (id,raw) in candidates {
        if raw.live.load(std::sync::atomic::Ordering::Acquire)!=raw.epoch || raw.stream.as_ref().is_some_and(|(generation,live)|live.load(std::sync::atomic::Ordering::Acquire)!=*generation) {continue;}
        let ptr=raw.frame.image.pixels().as_ptr() as usize;
        let extra=if seen.contains(&ptr){0}else{raw.frame.image.byte_len()};
        let Some(next)=bytes.checked_add(extra) else {continue;};
        if next>CaptureLimits::default().max_total_frame_bytes || frames.len()+admitted.len()>=MAX_CAPTURE_REFERENCES {continue;}
        *bytes=next;seen.insert(ptr);admitted.insert(*id,raw.clone());
    }
    admitted
}
#[derive(Clone, Debug)]
pub struct CaptureSnapshot {
    media_empty: HashMap<Id, Arc<skylive_core::MediaSource>>,
    session: u64,
    epoch: u64,
    generation: u64,
    frames: HashMap<Id, CapturedFrame>,
    draft_frames: HashMap<Id, DraftCapture>,
    problems: Vec<CaptureProblem>,
}
impl CaptureSnapshot {
    #[cfg(test)]
    pub(crate) fn test_camera_frame(session: u64, source: Id, frame: CapturedFrame) -> Self {
        Self { session, epoch: 1, generation: frame.sequence, frames: HashMap::from([(source, frame)]),
            media_empty: HashMap::new(), draft_frames: HashMap::new(), problems: Vec::new() }
    }
    /// Only a completely empty semantic envelope may alias another allocation.
    /// Exhaustive destructuring makes newly added metadata require a review.
    pub(crate) fn is_semantically_empty(&self) -> bool {
        let Self {
            media_empty,
            session: _,
            epoch: _,
            generation: _,
            frames,
            draft_frames,
            problems,
        } = self;
        media_empty.is_empty() && frames.is_empty() && draft_frames.is_empty() && problems.is_empty()
    }
    pub fn empty(session: u64) -> Self {
        Self {
            session,
            epoch: 0,
            generation: 0,
            frames: HashMap::new(),
            draft_frames: HashMap::new(),
            media_empty: HashMap::new(),
            problems: Vec::new(),
        }
    }
    pub fn session(&self) -> u64 {
        self.session
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn frame(&self, source: Id) -> Option<&CapturedFrame> {
        self.frames.get(&source)
    }
    /// Construct an isolated modal-preview snapshot using an admitted raw
    /// camera sample. Never return a delayed image as though it were raw.
    pub(crate) fn with_camera_draft(&self,source:Id)->Option<Self> {
        let separate_raw=|| {
            let raw=self.draft_frames.get(&source)?;
            if raw.live.load(std::sync::atomic::Ordering::Acquire)!=raw.epoch || raw.stream.as_ref().is_some_and(|(generation,live)|live.load(std::sync::atomic::Ordering::Acquire)!=*generation) {return None;}
            let mut draft=self.clone();
            draft.frames.insert(source,raw.frame.clone());
            // The delay-warming diagnostic belongs to Program, not this raw
            // modal projection. Preserve all unrelated source problems.
            draft.problems.retain(|problem|problem.source!=source || problem.code!="camera_frame_pending");
            Some(draft)
        };
        if !self.frames.contains_key(&source) {return separate_raw();}
        if !self.frames.get(&source).is_some_and(|frame|matches!(frame.native_origin,Some(NativeFrameOrigin::Camera {..}|NativeFrameOrigin::Ndi {..}))) {return None;}
        let delayed_raw=|| {
        let frame=self.frames.get(&source)?;
        if !matches!(frame.native_origin,Some(NativeFrameOrigin::Camera {..}|NativeFrameOrigin::Ndi {..})) {return None;}
        let raw=if frame.async_delay.is_some() {
            let receipt=frame.async_delay.as_ref()?;
            if !receipt.current() || receipt.source!=source {return None;}
            if receipt.staged_camera.as_ref().is_some_and(|proof|
                proof.live_generation.load(std::sync::atomic::Ordering::Acquire)!=proof.generation
                    || proof.mask_live.as_ref().is_some_and(|live|!live.load(std::sync::atomic::Ordering::Acquire))
                    || proof.scroll_live.as_ref().is_some_and(|live|!live.load(std::sync::atomic::Ordering::Acquire))
                    || proof.mask_resources.as_ref().is_some_and(|resources|!resources.current())) {return None;}
            if receipt.staged_ndi.as_ref().is_some_and(|proof|
                proof.live_generation.load(std::sync::atomic::Ordering::Acquire)!=proof.generation
                    || proof.mask_live.as_ref().is_some_and(|live|!live.load(std::sync::atomic::Ordering::Acquire))
                    || proof.scroll_live.as_ref().is_some_and(|live|!live.load(std::sync::atomic::Ordering::Acquire))
                    || proof.mask_resources.as_ref().is_some_and(|resources|!resources.current())) {return None;}
            let raw=frame.draft_raw.as_ref()?;
            if raw.binding!=frame.binding || raw.native_origin!=frame.native_origin || raw.native_clock_epoch!=frame.native_clock_epoch {return None;}
            raw.as_ref().clone()
        } else {frame.clone()};
        let mut draft=self.clone();
        draft.frames.insert(source,raw);
        Some(draft)
        };
        delayed_raw().or_else(separate_raw)
    }

    /// Stable native stream identity for a modal raw sample. The sequence may
    /// advance while rendering; an owner, stream or clock change must instead
    /// discard the old preview result.
    pub fn modal_raw_identity(&self,source:Id)->Option<(u64,u64,Option<u64>)> {
        let draft=self.with_camera_draft(source)?;
        let frame=draft.frame(source)?;
        let (owner,stream)=match frame.native_origin? {
            NativeFrameOrigin::Camera {owner,stream}|NativeFrameOrigin::Ndi {owner,stream}=>(owner,stream),
            _=>return None,
        };
        Some((owner,stream,frame.native_clock_epoch))
    }

    /// A validated normal Media lifecycle absence, never a decoder error or an
    /// unvalidated pending resource. Full authored binding prevents stale EOF
    /// markers from concealing a later rebind's missing frame.
    pub fn media_is_empty(&self, source: Id, media: &skylive_core::MediaSource) -> bool {
        !self.frames.contains_key(&source)
            && self.problem(source).is_none()
            && self
                .media_empty
                .get(&source)
                .is_some_and(|binding| binding.as_ref() == media)
    }
    pub fn problem(&self, source: Id) -> Option<&CaptureProblem> {
        self.problems
            .iter()
            .find(|problem| problem.source == source)
    }
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
    pub fn frames(&self) -> impl Iterator<Item = (Id, &CapturedFrame)> {
        self.frames.iter().map(|(source, frame)| (*source, frame))
    }
    pub fn problems(&self) -> &[CaptureProblem] {
        &self.problems
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureRequestKey {
    pub session: u64,
    pub revision: u64,
    pub serial: u64,
}
#[derive(Debug)]
pub enum CaptureEvent {
    Displays {
        key: CaptureRequestKey,
        result: Result<Vec<DisplayInfo>, CaptureError>,
    },
    Windows {
        key: CaptureRequestKey,
        result: Result<Vec<WindowInfo>, CaptureError>,
    },
    Frames {
        generation: u64,
    },
}
type Bindings = Vec<(Id, CaptureBinding)>;
enum Work {
    Displays(CaptureRequestKey),
    Windows(CaptureRequestKey),
    Frames {
        session: u64,
        epoch: u64,
        serial: u64,
        requested_at: MediaTime,
        period: u64,
        bindings: Bindings,
    },
}
enum Completion {
    Displays {
        key: CaptureRequestKey,
        result: Result<Vec<DisplayInfo>, CaptureError>,
    },
    Windows {
        key: CaptureRequestKey,
        result: Result<Vec<WindowInfo>, CaptureError>,
    },
    Frames {
        session: u64,
        epoch: u64,
        serial: u64,
        requested_at: MediaTime,
        period: u64,
        results: Vec<(Id, CaptureBinding, Result<CapturedOutcome, CaptureError>)>,
    },
}
pub struct CaptureWorker {
    sender: Option<mpsc::SyncSender<Work>>,
    receiver: Option<mpsc::Receiver<Completion>>,
    thread: Option<thread::JoinHandle<()>>,
    capabilities: CaptureCapabilities,
    window_capabilities: CaptureCapabilities,
    limits: CaptureLimits,
    busy: bool,
    serial: u64,
    epoch: u64,
    session: u64,
    bindings: Bindings,
    latest: Arc<CaptureSnapshot>,
    next_at: Option<u64>,
    last_requested_at: Option<MediaTime>,
    retire_pending: bool,
}
impl CaptureWorker {
    pub fn new(backend: Box<dyn CaptureBackend>) -> Result<Self, CaptureError> {
        Self::with_limits(backend, CaptureLimits::default())
    }
    pub fn with_limits(
        mut backend: Box<dyn CaptureBackend>,
        limits: CaptureLimits,
    ) -> Result<Self, CaptureError> {
        if limits.max_displays == 0
            || limits.max_displays > 64
            || limits.max_sources == 0
            || limits.max_sources > MAX_CAPTURE_REFERENCES
            || limits.max_frame_bytes == 0
            || limits.max_frame_bytes > 128 * 1024 * 1024
            || limits.max_total_frame_bytes == 0
            || limits.max_total_frame_bytes > 256 * 1024 * 1024
            || limits.max_raw_bytes == 0
            || limits.max_raw_bytes > 384 * 1024 * 1024
        {
            return Err(CaptureError::BudgetExceeded("invalid capture owner limits"));
        }
        let capabilities = backend.capabilities();
        let window_capabilities = backend.window_capabilities();
        let (sender, input) = mpsc::sync_channel(1);
        let (output, receiver) = mpsc::sync_channel(1);
        let handle = thread::Builder::new()
            .name("skylive-display-capture".into())
            .spawn(move || {
                // Keep a worker-owned reference until each buffer's readers
                // retire. Its expensive final drop therefore stays off GUI
                // frames. Slow or indefinitely pinned readers consume this
                // fixed resident budget and backpressure capture admission.
                let mut resident = Vec::<Arc<RgbaFrame>>::new();
                loop {
                    resident.retain(|frame| Arc::strong_count(frame) > 1);
                    let retiring=backend.reap_native_resources();
                    let work = if resident.is_empty() && !retiring {
                        match input.recv() {
                            Ok(work) => work,
                            Err(_) => break,
                        }
                    } else {
                        match input.recv_timeout(std::time::Duration::from_millis(100)) {
                            Ok(work) => work,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    };
                    resident.retain(|frame| Arc::strong_count(frame) > 1);
                    let resident_bytes: usize = resident.iter().map(|frame| frame.byte_len()).sum();
                    let mut resident_remaining =
                        (limits.max_total_frame_bytes * 4).saturating_sub(resident_bytes);
                    let completion = match work {
                        Work::Windows(key) => {
                            let result = backend
                                .enumerate_windows(limits)
                                .and_then(validate_windows)
                                .map_err(normalize_backend_error);
                            Completion::Windows { key, result }
                        }
                        Work::Displays(key) => {
                            let result = backend
                                .enumerate(limits)
                                .and_then(|displays| validate_displays(displays, limits))
                                .map_err(normalize_backend_error);
                            Completion::Displays { key, result }
                        }
                        Work::Frames {
                            session,
                            epoch,
                            serial,
                            requested_at,
                            period,
                            bindings,
                        } => {
                            let mut remaining = limits.max_total_frame_bytes;
                            let window_bindings: Vec<_> = bindings
                                .iter()
                                .filter_map(|(_, binding)| match binding {
                                    CaptureBinding::Window(value) => Some(value.clone()),
                                    _ => None,
                                })
                                .collect();
                            backend.retain_window_epoch(session, epoch, &window_bindings);
                            let display_bindings: Vec<_> = bindings
                                .iter()
                                .filter_map(|(_, binding)| match binding {
                                    CaptureBinding::Display(value) => Some(value.clone()),
                                    _ => None,
                                })
                                .collect();
                            backend.retain_display_bindings(session, epoch, &display_bindings);
                            let mut shared = HashMap::<
                                CaptureBinding,
                                Result<CapturedOutcome, CaptureError>,
                            >::new();
                            let mut results = Vec::with_capacity(bindings.len());
                            for (source, binding) in bindings {
                                let identity = binding.resource_key();
                                let result = if let Some(frame) = shared.get(&identity) {
                                    frame.clone()
                                } else if remaining == 0
                                    || resident_remaining == 0
                                    || resident.len() >= limits.max_sources * 4
                                {
                                    Err(CaptureError::BudgetExceeded("capture batch byte budget"))
                                } else {
                                    let frame_limits = CaptureLimits {
                                        max_frame_bytes: limits
                                            .max_frame_bytes
                                            .min(remaining)
                                            .min(resident_remaining),
                                        ..limits
                                    };
                                    enum Acquired { Frame(RgbaFrame,Option<DisplayFrameMetadata>,Option<crate::window_capture::WindowFrameMetadata>),Pending,WindowPending(u64),DisplayPending(u64) }
                                    match &binding {
                                        CaptureBinding::Display(value) => backend.capture_display(value,frame_limits).map(|outcome|match outcome{DisplayCaptureOutcome::Frame{image,metadata}=>Acquired::Frame(image,metadata,None),DisplayCaptureOutcome::NoNewFrame=>Acquired::Pending,DisplayCaptureOutcome::Pending{generation}=>Acquired::DisplayPending(generation)}),
                                        CaptureBinding::Window(value) => backend.capture_window_outcome(value,frame_limits).map(|outcome|match outcome{crate::window_capture::WindowCaptureOutcome::Frame{image,metadata}=>Acquired::Frame(image,None,Some(metadata)),crate::window_capture::WindowCaptureOutcome::Pending{generation}=>Acquired::WindowPending(generation)}),
                                        CaptureBinding::Video(_) | CaptureBinding::Media(_) | CaptureBinding::Ndi(_) | CaptureBinding::Browser(_) | CaptureBinding::Game(_) => Err(CaptureError::Unavailable(
                                            "camera frames require the dedicated persistent owner",
                                        )),
                                    }
                                    .and_then(|outcome| {
                                        let (frame,display,window)=match outcome {Acquired::Frame(frame,display,window)=>{if display.as_ref().is_some_and(|m|m.backend==DisplayCaptureBackend::Wgc && m.generation.is_none_or(|generation|generation==0)){return Err(CaptureError::InvalidFrame("invalid WGC display generation"));}if window.is_some_and(|m|m.generation==0){return Err(CaptureError::InvalidFrame("zero window native generation"));}(frame,display,window)},Acquired::Pending=>return Ok(CapturedOutcome::NoNewFrame),Acquired::WindowPending(generation)=>{if generation==0{return Err(CaptureError::InvalidFrame("zero pending window native generation"));}return Ok(CapturedOutcome::WindowPending{generation});},Acquired::DisplayPending(generation)=>{if generation==0{return Err(CaptureError::InvalidFrame("zero pending display native generation"));}return Ok(CapturedOutcome::DisplayPending{generation});}};
                                        if display
                                            .as_ref()
                                            .and_then(|metadata| metadata.fallback_reason.as_ref())
                                            .is_some_and(|reason| reason.len() > 4096)
                                        {
                                            return Err(CaptureError::BudgetExceeded(
                                                "display fallback diagnostic bytes",
                                            ));
                                        }
                                        validate_frame(&frame, frame_limits)?;
                                        remaining -= frame.byte_len();
                                        resident_remaining -= frame.byte_len();
                                        let frame = Arc::new(frame);
                                        resident.push(frame.clone());
                                        Ok(CapturedOutcome::Frame {
                                            image: frame,
                                            display,
                                            window,
                                        })
                                    })
                                    .map_err(normalize_backend_error)
                                };
                                shared.entry(identity).or_insert_with(|| result.clone());
                                results.push((source, binding, result));
                            }
                            Completion::Frames {
                                session,
                                epoch,
                                serial,
                                requested_at,
                                period,
                                results,
                            }
                        }
                    };
                    if output.send(completion).is_err() {
                        break;
                    }
                }
                drop(backend);
                // Admission is closed, but a renderer may still read an old
                // immutable snapshot. Transfer no final pixel-buffer free to
                // that reader: this bounded retirement owner remains until
                // the last reader retires. No finite join is claimed.
                while !resident.is_empty() {
                    resident.retain(|frame| Arc::strong_count(frame) > 1);
                    if !resident.is_empty() {
                        thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
                // Backend/OS resource destruction remains on this worker.
            })
            .map_err(|error| CaptureError::Backend(error.to_string()))?;
        Ok(Self {
            sender: Some(sender),
            receiver: Some(receiver),
            thread: Some(handle),
            capabilities,
            window_capabilities,
            limits,
            busy: false,
            serial: 0,
            epoch: 0,
            session: 0,
            bindings: Vec::new(),
            latest: Arc::new(CaptureSnapshot::empty(0)),
            next_at: None,
            last_requested_at: None,
            retire_pending: false,
        })
    }
    pub fn capabilities(&self) -> CaptureCapabilities {
        self.capabilities
    }
    pub fn window_capabilities(&self) -> CaptureCapabilities {
        self.window_capabilities
    }
    pub fn is_busy(&self) -> bool {
        self.busy
    }
    pub fn active_source_count(&self) -> usize {
        self.bindings.len()
    }
    pub fn snapshot(&self) -> Arc<CaptureSnapshot> {
        self.latest.clone()
    }
    pub fn last_requested_at(&self) -> Option<MediaTime> {
        self.last_requested_at
    }
    /// Explicit clock rebase invalidates results, never cancels an OS call or
    /// frees its admitted slot. The application must poll that call to settle.
    pub fn reset_timing(&mut self) -> Result<(), CaptureError> {
        if self.sender.is_none() {
            return Err(CaptureError::Closed);
        }
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or(CaptureError::IdentityExhausted)?;
        self.epoch = epoch;
        self.next_at = None;
        self.last_requested_at = None;
        self.latest = Arc::new(CaptureSnapshot {
            media_empty: HashMap::new(),
            session: self.session,
            epoch,
            generation: self.serial,
            frames: HashMap::new(),
            draft_frames: HashMap::new(),
            problems: Vec::new(),
        });
        Ok(())
    }
    pub fn synchronize(
        &mut self,
        session: &Session,
        outputs: &[FrameScene],
    ) -> Result<bool, CaptureError> {
        if self.sender.is_none() || session.is_closed() {
            return Err(CaptureError::Closed);
        }
        if outputs.len() > crate::projector::MAX_VIDEO_VIEWS {
            return Err(CaptureError::BudgetExceeded("capture view union limit"));
        }
        let current = session.snapshot();
        let mut bindings = HashMap::new();
        for view in outputs {
            view.validate_session(session.identity())
                .map_err(|_| CaptureError::ForeignSession)?;
            let sources = view
                .visible_sources(&current)
                .map_err(|_| CaptureError::InvalidFrame("capture scene is unavailable"))?;
            for source in sources {
                if matches!(
                    source.kind(),
                    SourceKind::VideoCapture { .. } | SourceKind::MediaSource { .. } | SourceKind::NdiSource {..} | SourceKind::Browser {..} | SourceKind::GameCapture {..}
                ) {
                    continue;
                }
                if let Some(binding) = CaptureBinding::from_source(source.kind()) {
                    if !bindings.contains_key(&source.id())
                        && bindings.len() >= self.limits.max_sources
                    {
                        return Err(CaptureError::BudgetExceeded("visible display source limit"));
                    }
                    bindings.insert(source.id(), binding);
                }
            }
        }
        let mut bindings: Bindings = bindings.into_iter().collect();
        bindings.sort_by_key(|(id, _)| id.get());
        if self.session == session.identity() && self.bindings == bindings {
            return Ok(false);
        }
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or(CaptureError::IdentityExhausted)?;
        let same_session = self.session == session.identity();
        let frames = if same_session {
            bindings
                .iter()
                .filter_map(|(source, binding)| {
                    self.latest
                        .frame(*source)
                        .filter(|frame| &frame.binding == binding)
                        .cloned()
                        .map(|frame| (*source, frame))
                })
                .collect()
        } else {
            HashMap::new()
        };
        let problems = if same_session {
            self.latest
                .problems
                .iter()
                .filter(|problem| {
                    bindings.iter().any(|(source, binding)| {
                        *source == problem.source
                            && self
                                .bindings
                                .iter()
                                .any(|(old, previous)| old == source && previous == binding)
                    })
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        self.retire_pending = !self.bindings.is_empty();
        self.session = session.identity();
        self.epoch = epoch;
        self.bindings = bindings;
        self.next_at = None;
        self.last_requested_at = None;
        self.latest = Arc::new(CaptureSnapshot {
            media_empty: HashMap::new(),
            session: self.session,
            epoch,
            generation: self.serial,
            frames,
            draft_frames: HashMap::new(),
            problems,
        });
        Ok(true)
    }
    fn send(&mut self, work: Work, serial: u64) -> Result<(), CaptureError> {
        if self.busy {
            return Err(CaptureError::Busy);
        }
        let sender = self.sender.as_ref().ok_or(CaptureError::Closed)?;
        sender.try_send(work).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => CaptureError::Busy,
            mpsc::TrySendError::Disconnected(_) => CaptureError::Disconnected,
        })?;
        self.busy = true;
        self.serial = serial;
        Ok(())
    }
    pub fn request_displays(
        &mut self,
        session: &Session,
    ) -> Result<CaptureRequestKey, CaptureError> {
        if session.is_closed() {
            return Err(CaptureError::Closed);
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(CaptureError::IdentityExhausted)?;
        let key = CaptureRequestKey {
            session: session.identity(),
            revision: session.snapshot().revision(),
            serial,
        };
        self.send(Work::Displays(key), serial)?;
        Ok(key)
    }
    pub fn request_windows(
        &mut self,
        session: &Session,
    ) -> Result<CaptureRequestKey, CaptureError> {
        if session.is_closed() {
            return Err(CaptureError::Closed);
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(CaptureError::IdentityExhausted)?;
        let key = CaptureRequestKey {
            session: session.identity(),
            revision: session.snapshot().revision(),
            serial,
        };
        self.send(Work::Windows(key), serial)?;
        Ok(key)
    }
    pub fn request_frame(
        &mut self,
        session: &Session,
        now: MediaTime,
    ) -> Result<bool, CaptureError> {
        if session.is_closed() {
            return Err(CaptureError::Closed);
        }
        if self.sender.is_none() {
            return Err(CaptureError::Closed);
        }
        if self.session != session.identity() {
            return Err(CaptureError::ForeignSession);
        }
        if self
            .last_requested_at
            .is_some_and(|previous| now < previous)
        {
            return Err(CaptureError::TimeDiscontinuity);
        }
        if self.bindings.is_empty() && !self.retire_pending {
            return Ok(false);
        }
        if self.busy {
            return Err(CaptureError::Busy);
        }
        if self.next_at.is_some_and(|deadline| now.0 < deadline) {
            return Ok(false);
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(CaptureError::IdentityExhausted)?;
        let video = session.snapshot();
        let period = u64::try_from(
            (1_000_000_000u128 * u128::from(video.video().fps_den()))
                .div_ceil(u128::from(video.video().fps_num())),
        )
        .map_err(|_| CaptureError::InvalidFrame("capture frame period overflow"))?;
        now.0
            .checked_add(period.max(1_000_000_000))
            .ok_or(CaptureError::TimeOverflow)?;
        self.send(
            Work::Frames {
                session: self.session,
                epoch: self.epoch,
                serial,
                requested_at: now,
                period,
                bindings: self.bindings.clone(),
            },
            serial,
        )?;
        self.last_requested_at = Some(now);
        self.retire_pending = false;
        Ok(true)
    }
    pub fn next_after_ns(&self, now: MediaTime) -> Option<u64> {
        if self.sender.is_none() || (self.bindings.is_empty() && !self.retire_pending) {
            None
        } else if self.busy {
            Some(10_000_000)
        } else {
            Some(
                self.next_at
                    .map_or(1, |deadline| deadline.saturating_sub(now.0).max(1)),
            )
        }
    }
    pub fn poll(
        &mut self,
        session: &Session,
        now: MediaTime,
    ) -> Result<Option<CaptureEvent>, CaptureError> {
        if self.sender.is_some()
            && self
                .last_requested_at
                .is_some_and(|previous| now < previous)
        {
            return Err(CaptureError::TimeDiscontinuity);
        }
        let receiver = self.receiver.as_ref().ok_or(CaptureError::Closed)?;
        let completion = match receiver.try_recv() {
            Ok(done) => done,
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.busy = false;
                return Err(CaptureError::Disconnected);
            }
        };
        self.busy = false;
        if session.is_closed() {
            return Ok(None);
        }
        match completion {
            Completion::Displays { key, result } => {
                if key.session == session.identity() {
                    Ok(Some(CaptureEvent::Displays { key, result }))
                } else {
                    Ok(None)
                }
            }
            Completion::Windows { key, result } => {
                if key.session == session.identity()
                    && key.revision == session.snapshot().revision()
                {
                    Ok(Some(CaptureEvent::Windows { key, result }))
                } else {
                    Ok(None)
                }
            }
            Completion::Frames {
                session: owner,
                epoch,
                serial,
                requested_at,
                period,
                results,
            } => {
                if owner != session.identity() || owner != self.session || epoch != self.epoch {
                    return Ok(None);
                }
                let mut frames = HashMap::new();
                let mut problems = Vec::new();
                let mut buffers = HashMap::new();
                let mut retained = 0usize;
                let mut succeeded = false;
                let mut unchanged = false;
                let mut failed = false;
                for (source, binding, result) in results {
                    let candidate = match result {
                        Ok(CapturedOutcome::Frame {
                            image,
                            display,
                            window,
                        }) => {
                            succeeded = true;
                            Some(CapturedFrame {
                                native_origin:None,
                                binding: binding.clone(),
                                sequence: serial,
                                requested_at,
                                native_timestamp_ns: None,
                                native_clock_epoch: None,
                                device_qpc_ns: None, directshow_clock: None,
                                async_delay: None,
                                draft_raw:None,
                                image: image.into(),
                                display,
                                window,
                            })
                        }
                        Ok(CapturedOutcome::DisplayPending { generation }) => {
                            unchanged = true;
                            let old = self.latest.frame(source).filter(|old| {
                                old.binding == binding && old.display.as_ref().is_some_and(|metadata| {
                                    metadata.backend == DisplayCaptureBackend::Wgc
                                        && metadata.generation == Some(generation)
                                })
                            }).cloned();
                            if old.is_none() {
                                failed = true;
                                problems.push(CaptureProblem { source,
                                    kind: crate::resource_issue::ResourceIssueKind::Unavailable,
                                    code: "display_capture_frame_pending",
                                    detail: "WGC has not produced an admitted monitor frame for this native generation".into() });
                            }
                            old
                        }
                        Ok(CapturedOutcome::WindowPending { generation }) => {
                            unchanged = true;
                            let old = self
                                .latest
                                .frame(source)
                                .filter(|old| {
                                    old.binding == binding
                                        && old.window.is_some_and(|metadata| {
                                            metadata.backend
                                            == crate::window_capture::WindowCaptureBackendKind::Wgc
                                            && metadata.generation == generation
                                        })
                                })
                                .cloned();
                            if old.is_none() {
                                failed = true;
                                problems.push(CaptureProblem{source,kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"window_capture_frame_pending",detail:"WGC has not produced an admitted frame for this native generation".into()});
                            }
                            old
                        }
                        Ok(CapturedOutcome::NoNewFrame) => {
                            unchanged = true;
                            let old = self
                                .latest
                                .frame(source)
                                .filter(|old| old.binding == binding)
                                .cloned();
                            if old.is_none() {
                                problems.push(CaptureProblem {source,kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"capture_frame_unavailable",detail:"Display capture has not produced an admitted frame for this binding".into()});
                            } else {
                                problems.extend(
                                    self.latest
                                        .problems
                                        .iter()
                                        .filter(|problem| problem.source == source)
                                        .cloned(),
                                );
                            }
                            old
                        }
                        Err(error) => {
                            failed = true;
                            problems.push(CaptureProblem {
                                source,
                                kind: error.issue_kind(),
                                code: error.code(),
                                detail: error.to_string(),
                            });
                            // A failed Window lease cannot establish that the same
                            // native HWND generation remains live. Never retain its
                            // stale pixels across close/recreate/error.
                            if matches!(&binding, CaptureBinding::Window(_)) {
                                None
                            } else {
                                self.latest
                                    .frame(source)
                                    .filter(|old| old.binding == binding)
                                    .cloned()
                            }
                        }
                    };
                    if let Some(frame) = candidate {
                        let pointer = frame.image.pixels().as_ptr() as usize;
                        let new_bytes = if buffers.contains_key(&pointer) {
                            0
                        } else {
                            frame.image.byte_len()
                        };
                        if retained
                            .checked_add(new_bytes)
                            .is_some_and(|bytes| bytes <= self.limits.max_total_frame_bytes)
                        {
                            retained += new_bytes;
                            buffers.insert(pointer, ());
                            frames.insert(source, frame);
                        } else {
                            problems.push(CaptureProblem {
                                source,
                                kind: crate::resource_issue::ResourceIssueKind::Limit,
                                code: "capture_budget_exceeded",
                                detail: "retained display frame byte budget".into(),
                            });
                        }
                    }
                }
                let retry = if succeeded || unchanged {
                    period
                } else {
                    period.max(1_000_000_000)
                };
                self.next_at = Some(
                    requested_at
                        .0
                        .checked_add(retry)
                        .unwrap_or(u64::MAX)
                        .max(now.0),
                );
                if !succeeded
                    && problems == self.latest.problems
                    && frames.len() == self.latest.frames.len()
                    && frames.iter().all(|(id, frame)| {
                        self.latest.frames.get(id).is_some_and(|old| {
                            old.sequence == frame.sequence
                                && old.window == frame.window
                                && old.image.shares_pixels_with(&frame.image)
                        })
                    })
                {
                    return Ok(None);
                }
                let generation = if succeeded || failed {
                    serial
                } else {
                    self.latest.generation
                };
                self.latest = Arc::new(CaptureSnapshot {
                    media_empty: HashMap::new(),
                    session: owner,
                    epoch,
                    generation,
                    frames,
                    draft_frames: HashMap::new(),
                    problems,
                });
                Ok(Some(CaptureEvent::Frames { generation }))
            }
        }
    }
    pub fn close(&mut self) {
        self.bindings.clear();
        self.latest = Arc::new(CaptureSnapshot::empty(0));
        self.receiver.take();
        // Wake worker destruction only after releasing our frame references.
        self.sender.take();
        self.next_at = None;
        self.last_requested_at = None;
        self.retire_pending = false;
    }
    /// Only a cooperating backend with finished OS work has a bounded join.
    /// GUI shutdown uses close, never this method.
    pub fn join(&mut self) -> Result<(), CaptureError> {
        self.close();
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| CaptureError::Backend("capture worker panicked".into()))?;
        }
        self.busy = false;
        Ok(())
    }
}
impl Drop for CaptureWorker {
    fn drop(&mut self) {
        self.close();
    }
}
fn normalize_backend_error(error: CaptureError) -> CaptureError {
    match error {
        CaptureError::Window(WindowCaptureError::Backend(detail)) if detail.len() > 8192 => {
            CaptureError::Window(WindowCaptureError::BudgetExceeded(
                "window backend error detail exceeds 8192 bytes",
            ))
        }
        CaptureError::Window(WindowCaptureError::Backend(detail)) => CaptureError::Window(
            WindowCaptureError::Backend(detail.into_boxed_str().into_string()),
        ),
        CaptureError::Backend(detail) if detail.len() > 8192 => {
            CaptureError::BudgetExceeded("capture backend error detail exceeds 8192 bytes")
        }
        CaptureError::Backend(detail) => {
            CaptureError::Backend(detail.into_boxed_str().into_string())
        }
        other => other,
    }
}
fn validate_windows(mut windows: Vec<WindowInfo>) -> Result<Vec<WindowInfo>, CaptureError> {
    if windows.len() > crate::window_capture::MAX_WINDOWS {
        return Err(WindowCaptureError::BudgetExceeded("window enumeration count").into());
    }
    let mut bytes = 0usize;
    for window in &mut windows {
        if window.name.is_empty()
            || window.name.len() > 2076
            || window.name.chars().any(char::is_control)
        {
            return Err(WindowCaptureError::InvalidFrame("invalid window display name").into());
        }
        let binding = window.binding(
            skylive_core::WindowMatchPriority::Title,
            false,
            matches!(window.target, skylive_core::WindowTarget::Windows { .. }),
        )?;
        if let Some((width, height)) = window.full_window_size {
            skylive_core::WindowCaptureBinding::new(
                window.target.clone(),
                skylive_core::WindowMatchPriority::Title,
                false,
                false,
                width,
                height,
            )
            .map_err(|_| WindowCaptureError::InvalidFrame("invalid full-window metadata"))?;
        }
        window.target = binding.target().clone();
        window.name = std::mem::take(&mut window.name)
            .into_boxed_str()
            .into_string();
        let identity = match &window.target {
            skylive_core::WindowTarget::Windows {
                title,
                class,
                executable,
            } => title.len() + class.len() + executable.len(),
            skylive_core::WindowTarget::Macos { title, bundle_id } => title.len() + bundle_id.len(),
        };
        bytes = bytes.checked_add(identity + window.name.len()).ok_or(
            WindowCaptureError::BudgetExceeded("window metadata overflow"),
        )?;
        if bytes > 4 * 1024 * 1024 {
            return Err(WindowCaptureError::BudgetExceeded("window metadata exceeds 4MiB").into());
        }
    }
    Ok(windows.into_boxed_slice().into_vec())
}
fn validate_displays(
    mut displays: Vec<DisplayInfo>,
    limits: CaptureLimits,
) -> Result<Vec<DisplayInfo>, CaptureError> {
    if displays.len() > limits.max_displays {
        return Err(CaptureError::BudgetExceeded("display enumeration limit"));
    }
    let mut targets = std::collections::HashSet::new();
    for display in &mut displays {
        if display.name.is_empty()
            || display.name.len() > 1024
            || display.name.chars().any(char::is_control)
        {
            return Err(CaptureError::InvalidFrame("invalid display name"));
        }
        let binding = display.binding(false)?;
        display.target = binding.target().clone();
        display.name = std::mem::take(&mut display.name)
            .into_boxed_str()
            .into_string();
        if !targets.insert(display.target.clone()) {
            return Err(CaptureError::InvalidFrame("duplicate display target"));
        }
    }
    Ok(displays.into_boxed_slice().into_vec())
}
fn validate_frame(frame: &RgbaFrame, limits: CaptureLimits) -> Result<(), CaptureError> {
    if frame.width() > 8192
        || frame.height() > 8192
        || u64::from(frame.width()) * u64::from(frame.height()) > skylive_core::MAX_DISPLAY_PIXELS
        || frame.byte_len() > limits.max_frame_bytes
    {
        return Err(CaptureError::BudgetExceeded("native display frame limit"));
    }
    Ok(())
}

#[cfg(test)]
mod display_method_tests;
#[cfg(test)]
mod window_tests;
#[cfg(test)]
mod worker_tests;

/// Prepare live-size-sensitive actions in the shared host facade. This does not
/// publish; the Session validates and commits the returned typed command once.
pub fn prepare_transform_command(
    session: &Session,
    capture: &CaptureSnapshot,
    scene: Id,
    item: Id,
    action: skylive_core::TransformAction,
) -> Result<skylive_core::Command, CaptureError> {
    let current = session.snapshot();
    let selected = current
        .scene_container(scene)
        .ok_or(CaptureError::Engine(skylive_core::Error::NotFound(scene)))?
        .items()
        .iter()
        .find(|candidate| candidate.id() == item)
        .ok_or(CaptureError::Engine(skylive_core::Error::NotFound(item)))?;
    let source = current
        .source(selected.source())
        .ok_or(CaptureError::Engine(skylive_core::Error::NotFound(
            selected.source(),
        )))?;
    if let Some(binding) = CaptureBinding::from_source(source.kind()) {
        if capture.session() != session.identity() {
            return Err(CaptureError::ForeignSession);
        }
        if let Some(problem) = capture.problem(source.id()) {
            return Err(CaptureError::Backend(problem.detail.clone()));
        }
        let frame = capture
            .frame(source.id())
            .filter(|frame| frame.binding == binding)
            .ok_or(CaptureError::Unavailable(
                "a matching display frame is required for this transform action",
            ))?;
        Ok(skylive_core::Command::TransformItemForFrame {
            scene,
            item,
            action,
            frame_width: frame.image.width(),
            frame_height: frame.image.height(),
        })
    } else {
        Ok(skylive_core::Command::TransformItem {
            scene,
            item,
            action,
        })
    }
}

#[cfg(test)]
mod prepared_snapshot_identity_tests {
    use super::*;
    #[test]
    fn semantic_empty_identity_includes_media_empty_metadata_and_problems() {
        let mut snapshot = CaptureSnapshot::empty(1);
        assert!(snapshot.is_semantically_empty());
        snapshot.media_empty.insert(
            Id::new(1),
            Arc::new(
                skylive_core::MediaSource::new("metadata-only.mp4".into(), 16, 16, false).unwrap(),
            ),
        );
        assert!(
            !snapshot.is_semantically_empty(),
            "absence-of-media pixels is meaningful metadata"
        );
        snapshot.media_empty.clear();
        snapshot.problems.push(CaptureProblem {
            kind: crate::resource_issue::ResourceIssueKind::Unavailable,
            source: Id::new(1),
            code: "fixture",
            detail: "Exact failure".into(),
        });
        assert!(!snapshot.is_semantically_empty());
    }
}

#[cfg(test)]
#[path = "capture/wgc_tests.rs"]
mod wgc_tests;
