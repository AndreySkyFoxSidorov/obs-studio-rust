//! Window-specific OS port. The shared capture owner controls scheduling,
//! result freshness and resident-frame budgets; adapters own transient handles.
use crate::capture::{CaptureCapabilities, CaptureLimits};
use skylive_core::{WindowCaptureBinding, WindowMatchPriority, WindowTarget};
use skylive_media::RgbaFrame;
use std::fmt;

pub(crate) mod matching;
pub mod native;
#[cfg(any(target_os = "windows", test))]
#[path = "window_capture/wgc_geometry.rs"]
pub(crate) mod wgc_geometry;
pub const MAX_WINDOWS: usize = 1024;

/// Resolve an authored window selector against a completed inventory using the
/// same Windows priority policy as capture. The returned item is presentation
/// metadata; the caller must keep the authored selector as the binding.
pub fn matching_window<'a>(windows:&'a [WindowInfo],target:&WindowTarget,priority:WindowMatchPriority)->Option<&'a WindowInfo>{
    match target {
        WindowTarget::Windows {..}=>windows.iter().filter_map(|window|matching::windows_rating(target,&window.target,priority).map(|score|(score,window)))
            .min_by_key(|(score,_)|*score).map(|(_,window)|window),
        WindowTarget::Macos {..}=>windows.iter().find(|window|&window.target==target),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct WindowInfo {
    /// Worker-enumerated physical DWM extent; absent if unavailable.
    pub full_window_size: Option<(u32, u32)>,
    pub target: WindowTarget,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub minimized: bool,
}
impl WindowInfo {
    pub fn binding_with_method(
        &self,
        priority: WindowMatchPriority,
        cursor: bool,
        client_area: bool,
        method: skylive_core::WindowCaptureMethod,
    ) -> Result<WindowCaptureBinding, WindowCaptureError> {
        self.binding_for_target(&self.target,priority,cursor,client_area,method)
    }
    /// Use the observed window's dimensions while retaining the authored
    /// selector. Auto's Windows method choice is based on that selector.
    pub fn binding_for_target(
        &self,
        target:&WindowTarget,
        priority:WindowMatchPriority,
        cursor:bool,
        client_area:bool,
        method:skylive_core::WindowCaptureMethod,
    )->Result<WindowCaptureBinding,WindowCaptureError>{
        let full_window=match (target,method){
            (WindowTarget::Windows {..},skylive_core::WindowCaptureMethod::Wgc)=>!client_area,
            (WindowTarget::Windows {class,..},skylive_core::WindowCaptureMethod::Auto)=>!client_area&&matching::auto_uses_wgc(class),
            _=>false,
        };
        let (width, height) = if full_window {
            self.full_window_size
                .ok_or(WindowCaptureError::Unavailable(
                    "full-window geometry is unavailable; refresh the window list",
                ))?
        } else {
            (self.width, self.height)
        };
        WindowCaptureBinding::new(
            target.clone(),
            priority,
            cursor,
            client_area,
            width,
            height,
        )
        .and_then(|b| b.with_method(method))
        .map_err(|_| WindowCaptureError::InvalidFrame("invalid window capture method or geometry"))
    }

    pub fn binding(
        &self,
        priority: WindowMatchPriority,
        cursor: bool,
        client_area: bool,
    ) -> Result<WindowCaptureBinding, WindowCaptureError> {
        WindowCaptureBinding::new(
            self.target.clone(),
            priority,
            cursor,
            client_area,
            self.width,
            self.height,
        )
        .map_err(|_| WindowCaptureError::InvalidFrame("invalid window binding"))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowCaptureError {
    Unavailable(&'static str),
    PermissionDenied,
    TargetNotFound,
    TargetHidden,
    TargetMinimized,
    AmbiguousTarget,
    InvalidFrame(&'static str),
    BudgetExceeded(&'static str),
    Backend(String),
    DeviceLost(String),
    Disconnected,
}
impl WindowCaptureError {
    pub fn issue_kind(&self) -> crate::resource_issue::ResourceIssueKind {
        use crate::resource_issue::ResourceIssueKind::*;
        match self {
            Self::BudgetExceeded(_) => Limit,
            _ => Unavailable,
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "window_capture_unavailable",
            Self::PermissionDenied => "window_capture_permission_denied",
            Self::TargetNotFound => "window_capture_target_not_found",
            Self::TargetHidden => "window_capture_target_hidden",
            Self::TargetMinimized => "window_capture_target_minimized",
            Self::AmbiguousTarget => "window_capture_target_ambiguous",
            Self::InvalidFrame(_) => "window_capture_invalid_frame",
            Self::BudgetExceeded(_) => "window_capture_budget_exceeded",
            Self::Backend(_) => "window_capture_backend_error",
            Self::DeviceLost(_) => "window_capture_device_lost",
            Self::Disconnected => "window_capture_disconnected",
        }
    }
}
impl fmt::Display for WindowCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(s) | Self::InvalidFrame(s) | Self::BudgetExceeded(s) => {
                write!(f, "{}: {s}", self.code())
            }
            Self::Backend(s) | Self::DeviceLost(s) => write!(f, "{}: {s}", self.code()),
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for WindowCaptureError {}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowCaptureBackendKind {
    Gdi,
    Wgc,
    ScreenCaptureKit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowFrameEncoding {
    OpaqueSrgb,
    PremultipliedLinearSrgb,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct WindowFrameMetadata {
    pub backend: WindowCaptureBackendKind,
    pub generation: u64,
    pub encoding: WindowFrameEncoding,
    /// Ephemeral Windows identity of the window that supplied this frame.
    /// HWND/PID are process-local observations, never durable source identity;
    /// consumers must revalidate them and the capture session before use.
    pub native_window: Option<NativeWindowIdentity>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct NativeWindowIdentity {
    pub handle: usize,
    pub process_id: u32,
    /// Windows process creation time, measured from an open process handle.
    /// Without it a reused PID cannot authorize linked application audio.
    pub process_creation_time_100ns: Option<u64>,
}
pub enum WindowCaptureOutcome {
    Frame {
        image: RgbaFrame,
        metadata: WindowFrameMetadata,
    },
    Pending {
        generation: u64,
    },
}
pub trait WindowCaptureBackend: Send + 'static {
    fn retain_epoch(&mut self, session: u64, _epoch: u64, bindings: &[WindowCaptureBinding]) {
        self.retain_bindings(session, bindings)
    }
    fn reap_resources(&mut self) -> bool {
        false
    }
    fn capture_outcome(
        &mut self,
        binding: &WindowCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<WindowCaptureOutcome, WindowCaptureError> {
        if matches!(binding.target(), WindowTarget::Windows { .. })
            && binding.method() == skylive_core::WindowCaptureMethod::Wgc
        {
            return Err(WindowCaptureError::Unavailable(
                "this backend does not implement WGC",
            ));
        }
        self.capture(binding, limits)
            .map(|image| WindowCaptureOutcome::Frame {
                image,
                metadata: WindowFrameMetadata {
                    backend: if matches!(binding.target(), WindowTarget::Windows { .. }) {
                        WindowCaptureBackendKind::Gdi
                    } else {
                        WindowCaptureBackendKind::ScreenCaptureKit
                    },
                    generation: 1,
                    encoding: WindowFrameEncoding::OpaqueSrgb,
                    native_window: None,
                },
            })
    }
    /// Called by the acquisition owner before a frame batch. It bounds transient
    /// selector caches to the currently admitted union, including frozen views.
    fn retain_bindings(&mut self, _session: u64, _bindings: &[WindowCaptureBinding]) {}
    fn capabilities(&self) -> CaptureCapabilities;
    /// At most 1024 top-level candidates; an exceeded bound is an error rather
    /// than a truncated list that could select a different application.
    fn enumerate(&mut self, limits: CaptureLimits) -> Result<Vec<WindowInfo>, WindowCaptureError>;
    fn capture(
        &mut self,
        binding: &WindowCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<RgbaFrame, WindowCaptureError>;
}

pub struct UnsupportedBackend;
impl WindowCaptureBackend for UnsupportedBackend {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            available: false,
            backend: "unsupported-window",
            cursor: false,
            hdr: false,
            note: Some("window capture is unavailable on this host"),
        }
    }
    fn enumerate(&mut self, _: CaptureLimits) -> Result<Vec<WindowInfo>, WindowCaptureError> {
        Err(WindowCaptureError::Unavailable(
            "window capture is unavailable on this host",
        ))
    }
    fn capture(
        &mut self,
        _: &WindowCaptureBinding,
        _: CaptureLimits,
    ) -> Result<RgbaFrame, WindowCaptureError> {
        Err(WindowCaptureError::Unavailable(
            "window capture is unavailable on this host",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serialized_binding_rejects_transient_handles_and_invalid_dimensions() {
        let info = WindowInfo {
            full_window_size: None,
            target: WindowTarget::Windows {
                title: "Editor".into(),
                class: "EditorClass".into(),
                executable: "editor.exe".into(),
            },
            name: "Editor".into(),
            width: 640,
            height: 480,
            minimized: false,
        };
        let binding = info
            .binding(WindowMatchPriority::Class, true, true)
            .unwrap();
        let value = serde_json::to_value(&binding).unwrap();
        assert_eq!(
            serde_json::from_value::<WindowCaptureBinding>(value.clone()).unwrap(),
            binding
        );
        for (key, field) in [
            ("hwnd", serde_json::json!(17)),
            ("width", serde_json::json!(0)),
            ("height", serde_json::json!(8193)),
        ] {
            let mut bad = value.clone();
            bad[key] = field;
            assert!(serde_json::from_value::<WindowCaptureBinding>(bad).is_err());
        }
    }
}

#[cfg(test)]
mod full_geometry_tests {
    use super::*;
    #[test]
    fn complete_window_uses_its_own_basis_and_missing_metadata_rejects() {
        let mut info = WindowInfo {
            full_window_size: Some((656, 520)),
            target: WindowTarget::Windows {
                title: "x".into(),
                class: "x".into(),
                executable: "x.exe".into(),
            },
            name: "x".into(),
            width: 640,
            height: 480,
            minimized: false,
        };
        let full = info
            .binding_with_method(
                WindowMatchPriority::Title,
                false,
                false,
                skylive_core::WindowCaptureMethod::Wgc,
            )
            .unwrap();
        assert_eq!((full.width(), full.height()), (656, 520));
        let client = info
            .binding_with_method(
                WindowMatchPriority::Title,
                false,
                true,
                skylive_core::WindowCaptureMethod::Wgc,
            )
            .unwrap();
        assert_eq!((client.width(), client.height()), (640, 480));
        info.full_window_size = None;
        assert!(
            info.binding_with_method(
                WindowMatchPriority::Title,
                false,
                false,
                skylive_core::WindowCaptureMethod::Wgc
            )
            .is_err()
        );
    }
}
