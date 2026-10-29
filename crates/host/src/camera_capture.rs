//! Persistent camera ports. Native backends/streams stay worker-local; device
//! enumeration and stream activation are distinct explicit operations.
use skylive_core::{Id, VideoCaptureBinding, VideoCaptureMode, VideoCaptureTarget};
use skylive_media::RgbaFrame;
use std::fmt;
pub mod native;
mod owner;
// Source-scoped delay is connected to camera capture. Audio synchronization and
// public filter availability remain separate, unfinished integration steps.
#[allow(dead_code)]
pub(crate) mod async_delay;
#[allow(dead_code)] // The camera owner still needs to submit source/resource snapshots.
pub(crate) mod staged_delay_worker;
#[cfg(target_os = "windows")]
#[allow(dead_code)]
pub(crate) mod embedded_audio;
#[cfg(target_os = "windows")]
#[allow(dead_code)]
pub(crate) mod embedded_video;
mod mjpeg;
#[cfg(target_os = "windows")]
#[allow(dead_code)] // Native mode negotiation and the owning stream are not connected yet.
mod avc;
mod controls;
pub use controls::*;
mod selection;
pub(crate) use selection::directshow_mode_matches;
#[cfg(target_os = "windows")]
mod directshow_rank;
pub use selection::{CameraModeRecommendations, arrange_camera_modes, best_camera_resolution_mode, highest_camera_frame_rate, camera_frame_rate_matching_project};
mod scope;
pub use owner::{
    CameraEvent, CameraFrame, CameraOwner, CameraProblem, CameraRequestKey, CameraSnapshot,
};

#[derive(Clone, Copy, Debug)]
pub struct CameraLimits {
    pub max_devices: usize,
    pub max_modes: usize,
    pub max_streams: usize,
    pub max_frame_bytes: usize,
    pub max_raw_bytes: usize,
    pub max_total_frame_bytes: usize,
}
impl Default for CameraLimits {
    fn default() -> Self {
        Self {
            max_devices: 64,
            max_modes: 1024,
            max_streams: 4,
            max_frame_bytes: 128 * 1024 * 1024,
            max_raw_bytes: 384 * 1024 * 1024,
            max_total_frame_bytes: 256 * 1024 * 1024,
        }
    }
}
impl CameraLimits {
    pub fn validate(self) -> Result<(), CameraError> {
        if self.max_devices == 0
            || self.max_devices > 64
            || self.max_modes == 0
            || self.max_modes > 1024
            || self.max_streams == 0
            || self.max_streams > 8
            || self.max_frame_bytes == 0
            || self.max_frame_bytes > 128 * 1024 * 1024
            || self.max_raw_bytes == 0
            || self.max_raw_bytes > 384 * 1024 * 1024
            || self.max_total_frame_bytes == 0
            || self.max_total_frame_bytes > 256 * 1024 * 1024
        {
            return Err(CameraError::BudgetExceeded("invalid camera limits"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct CameraCapabilities {
    pub available: bool,
    pub backend: &'static str,
    pub video: bool,
    pub audio: bool,
    pub note: Option<&'static str>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CameraDeviceInfo {
    pub target: VideoCaptureTarget,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CameraModeInfo {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    /// The actual native media subtype, never an inferred converted format.
    pub native_subtype: String,
    pub mode: Option<VideoCaptureMode>,
    pub unsupported_reason: Option<&'static str>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CameraError {
    Unavailable(&'static str),
    PermissionRequired,
    PermissionDenied,
    DeviceNotFound,
    ModeNotFound,
    DeviceBusy,
    DeviceLost,
    NoFrame,
    InvalidFrame(&'static str),
    BudgetExceeded(&'static str),
    Native {
        operation: &'static str,
        detail: String,
    },
    Busy,
    Closed,
    Disconnected,
    IdentityExhausted,
    ForeignSession,
    TimeOverflow,
    TimeDiscontinuity,
    ConflictingModes,
}
impl CameraError {
    pub fn issue_kind(&self) -> crate::resource_issue::ResourceIssueKind {
        use crate::resource_issue::ResourceIssueKind::*;
        match self {
            Self::BudgetExceeded(_) => Limit,
            Self::IdentityExhausted
            | Self::ForeignSession
            | Self::TimeOverflow
            | Self::TimeDiscontinuity
            | Self::ConflictingModes => Invalid,
            _ => Unavailable,
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "camera_unavailable",
            Self::PermissionRequired => "camera_permission_required",
            Self::PermissionDenied => "camera_permission_denied",
            Self::DeviceNotFound => "camera_device_not_found",
            Self::ModeNotFound => "camera_mode_not_found",
            Self::DeviceBusy => "camera_device_busy",
            Self::DeviceLost => "camera_device_lost",
            Self::NoFrame => "camera_frame_pending",
            Self::InvalidFrame(_) => "camera_invalid_frame",
            Self::BudgetExceeded(_) => "camera_budget_exceeded",
            Self::Native { .. } => "camera_native_error",
            Self::Busy => "camera_busy",
            Self::Closed => "camera_closed",
            Self::Disconnected => "camera_disconnected",
            Self::IdentityExhausted => "camera_identity_exhausted",
            Self::ForeignSession => "camera_foreign_session",
            Self::TimeOverflow => "camera_time_overflow",
            Self::TimeDiscontinuity => "camera_time_discontinuity",
            Self::ConflictingModes => "camera_conflicting_modes",
        }
    }
}
impl fmt::Display for CameraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(detail)
            | Self::InvalidFrame(detail)
            | Self::BudgetExceeded(detail) => write!(f, "{}: {detail}", self.code()),
            Self::Native { operation, detail } => {
                write!(f, "{} ({operation}): {detail}", self.code())
            }
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for CameraError {}

pub use skylive_media::DirectShowClockSnapshot;

pub struct NativeVideoFrame {
    pub image: RgbaFrame,
    pub presentation_ns: i128,
    /// Windows MF device timestamp in QPC's epoch, if provided by the device.
    /// This is distinct from the stream-relative presentation timestamp.
    pub device_qpc_ns: Option<u64>,
    pub directshow_clock: Option<DirectShowClockSnapshot>,
    pub discontinuity: bool,
    pub dropped_frames: u64,
}
/// Intentionally lacks Send. Construct and destroy on the acquisition owner.
pub trait NativeCameraBackend {
    fn enumerate_devices(
        &mut self,
        limits: CameraLimits,
    ) -> Result<Vec<CameraDeviceInfo>, CameraError>;
    /// Explicit mode query can instantiate a native media source. It is not part
    /// of passive device enumeration and remains admitted until actual shutdown.
    fn enumerate_modes(
        &mut self,
        target: &VideoCaptureTarget,
        limits: CameraLimits,
    ) -> Result<Vec<CameraModeInfo>, CameraError>;
    /// Read the DirectShow capture pin's current default on the owner thread.
    /// This is not the final OBS Preferred choice: OBS also ranks pin caps.
    fn directshow_default(&mut self, _target: &VideoCaptureTarget) -> Result<CameraModeInfo, CameraError> {
        Err(CameraError::Unavailable("DirectShow default query is unavailable on this backend"))
    }
    /// Compute the OBS-preferred candidate without changing the device format.
    fn directshow_preferred(&mut self, _target: &VideoCaptureTarget) -> Result<CameraModeInfo, CameraError> {
        Err(CameraError::Unavailable("DirectShow preferred query is unavailable on this backend"))
    }
    /// True only when DirectShow proves that OBS's automatic camera audio is absent.
    fn directshow_video_only(&mut self, _target: &VideoCaptureTarget) -> Result<bool, CameraError> {
        Err(CameraError::Unavailable("DirectShow camera audio inspection is unavailable on this backend"))
    }
    fn open(
        &mut self,
        binding: &VideoCaptureBinding,
        limits: CameraLimits,
    ) -> Result<Box<dyn NativeCameraStream>, CameraError>;
    /// Open a single native graph that owns both video and the device's
    /// embedded audio pin. The camera owner must have exclusive authority.
    fn open_embedded(&mut self, _binding:&VideoCaptureBinding, _session:u64, _source:Id,
        _camera_epoch:u64, _limits:CameraLimits)->Result<Box<dyn NativeCameraStream>,CameraError> {
        Err(CameraError::Unavailable("embedded camera audio is unavailable on this backend"))
    }
}
pub trait NativeCameraStream {
    /// Query the already owned physical device, never open a second handle.
    fn enumerate_modes(&mut self, _limits:CameraLimits)->Result<Vec<CameraModeInfo>,CameraError> {
        Err(CameraError::Unavailable("mode enumeration on the active camera is unavailable"))
    }
    fn query_controls(&mut self) -> Result<ControlQuery, CameraError> {
        Err(CameraError::Unavailable("driver controls unavailable on this backend"))
    }
    fn set_control(&mut self, change: ControlChange, _gate: &ControlWriteGate) -> ControlReceipt {
        ControlReceipt::before(change, "driver controls unavailable on this backend")
    }
    /// Nonblocking queue drain plus bounded owner-thread pixel conversion.
    fn poll(&mut self, limits: CameraLimits) -> Result<Option<NativeVideoFrame>, CameraError>;
    /// Drain one embedded PCM packet on the graph's own acquisition thread.
    /// Ordinary video-only backends never fabricate a corresponding device.
    fn poll_embedded_audio(&mut self)->Result<Option<crate::audio_capture::NativePcmBlock>,CameraError> { Ok(None) }
    /// false means the stop/flush callback has not acknowledged quiescence yet.
    /// The caller must retain this stream and its admission until true.
    fn stop(&mut self) -> Result<bool, CameraError>;
}
pub type CameraBackendFactory =
    Box<dyn FnOnce() -> Result<Box<dyn NativeCameraBackend>, CameraError> + Send>;

pub(crate) fn checked_frame_bytes(
    width: u32,
    height: u32,
    limits: CameraLimits,
) -> Result<usize, CameraError> {
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > skylive_core::MAX_DISPLAY_PIXELS
    {
        return Err(CameraError::InvalidFrame(
            "camera dimensions exceed capture profile",
        ));
    }
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(CameraError::BudgetExceeded("camera size overflow"))?;
    if bytes > limits.max_frame_bytes {
        return Err(CameraError::BudgetExceeded("camera RGBA frame bytes"));
    }
    Ok(bytes)
}
/// Cooperative native sample reservation includes 256-byte row alignment for
/// SDK padding plus queued/processing/upstream frame ownership. Larger native
/// layouts return an explicit budget error rather than exceeding admission.
pub(crate) fn checked_mode_raw_bytes(mode:VideoCaptureMode,limits:CameraLimits)->Result<usize,CameraError>{
    match mode.format() {
        skylive_core::VideoPixelFormat::Mjpeg => mjpeg::reservation(mode,limits),
        skylive_core::VideoPixelFormat::H264 => {
            #[cfg(target_os="windows")]
            { avc::reservation(mode.width(),mode.height(),limits) }
            #[cfg(not(target_os="windows"))]
            { Err(CameraError::Unavailable("AVC camera capture is available only on Windows")) }
        }
        _ => checked_raw_bytes(mode.width(),mode.height(),limits),
    }
}
pub(crate) fn checked_raw_bytes(
    width: u32,
    height: u32,
    limits: CameraLimits,
) -> Result<usize, CameraError> {
    checked_frame_bytes(width, height, limits)?;
    let row = (width as usize)
        .checked_mul(4)
        .and_then(|n| n.checked_add(255))
        .map(|n| n / 256 * 256)
        .ok_or(CameraError::BudgetExceeded("camera aligned native row"))?;
    let bytes = row
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(3))
        .ok_or(CameraError::BudgetExceeded("camera aligned native buffers"))?;
    if bytes > limits.max_raw_bytes {
        return Err(CameraError::BudgetExceeded("camera aligned native buffers"));
    }
    Ok(bytes)
}
