//! Construction is inert; enumeration and capture occur only on the owner.
use super::WindowCaptureBackend;
#[cfg(target_os = "windows")]
#[path = "native_windows.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "native_macos.rs"]
mod platform;
#[cfg(target_os = "windows")]
pub(crate) fn with_budget(
    budget: crate::capture::native_budget::NativeCopyBudget,
) -> Box<dyn WindowCaptureBackend> {
    Box::new(platform::NativeBackend::with_budget(budget))
}
pub fn backend() -> Box<dyn WindowCaptureBackend> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        Box::new(platform::NativeBackend::new())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Box::new(super::UnsupportedBackend)
    }
}
