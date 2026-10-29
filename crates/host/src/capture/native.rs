//! Native adapters are constructed without enumerating or opening any display.

use super::CaptureBackend;

#[cfg(target_os = "windows")]
#[path = "native_windows.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "native_macos.rs"]
mod platform;

pub fn backend() -> Box<dyn CaptureBackend> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        #[cfg(target_os = "windows")]
        {
            let budget = super::native_budget::NativeCopyBudget::default();
            Box::new(NativeBackend {
                display: platform::NativeBackend::with_budget(budget.clone()),
                window: crate::window_capture::native::with_budget(budget),
            })
        }
        #[cfg(target_os = "macos")]
        {
            Box::new(NativeBackend {
                display: platform::NativeBackend::new(),
                window: crate::window_capture::native::backend(),
            })
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Box::new(super::UnsupportedBackend)
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
struct NativeBackend {
    display: platform::NativeBackend,
    window: Box<dyn crate::window_capture::WindowCaptureBackend>,
}
#[cfg(any(target_os = "windows", target_os = "macos"))]
impl CaptureBackend for NativeBackend {
    fn retain_display_bindings(
        &mut self,
        session: u64,
        epoch: u64,
        bindings: &[skylive_core::DisplayCaptureBinding],
    ) {
        self.display
            .retain_display_bindings(session, epoch, bindings)
    }
    fn capture_display(
        &mut self,
        binding: &skylive_core::DisplayCaptureBinding,
        limits: super::CaptureLimits,
    ) -> Result<super::DisplayCaptureOutcome, super::CaptureError> {
        #[cfg(target_os = "windows")]
        {
            self.display.capture_display(binding, limits)
        }
        #[cfg(target_os = "macos")]
        {
            self.display
                .capture(binding, limits)
                .map(|image| super::DisplayCaptureOutcome::Frame {
                    image,
                    metadata: Some(super::DisplayFrameMetadata {
                        backend: super::DisplayCaptureBackend::ScreenCaptureKit,
                        fallback_reason: None,
                        generation: None,
                    }),
                })
        }
    }
    fn retain_window_bindings(
        &mut self,
        session: u64,
        bindings: &[skylive_core::WindowCaptureBinding],
    ) {
        self.window.retain_bindings(session, bindings)
    }
    fn retain_window_epoch(
        &mut self,
        session: u64,
        epoch: u64,
        bindings: &[skylive_core::WindowCaptureBinding],
    ) {
        self.window.retain_epoch(session, epoch, bindings)
    }
    fn reap_native_resources(&mut self) -> bool {
        self.window.reap_resources() | self.display.reap_native_resources()
    }
    fn capture_window_outcome(
        &mut self,
        binding: &skylive_core::WindowCaptureBinding,
        limits: super::CaptureLimits,
    ) -> Result<crate::window_capture::WindowCaptureOutcome, super::CaptureError> {
        self.window
            .capture_outcome(binding, limits)
            .map_err(Into::into)
    }
    fn capabilities(&self) -> super::CaptureCapabilities {
        self.display.capabilities()
    }
    fn window_capabilities(&self) -> super::CaptureCapabilities {
        self.window.capabilities()
    }
    fn enumerate(
        &mut self,
        limits: super::CaptureLimits,
    ) -> Result<Vec<super::DisplayInfo>, super::CaptureError> {
        self.display.enumerate(limits)
    }
    fn capture(
        &mut self,
        binding: &skylive_core::DisplayCaptureBinding,
        limits: super::CaptureLimits,
    ) -> Result<skylive_media::RgbaFrame, super::CaptureError> {
        self.display.capture(binding, limits)
    }
    fn enumerate_windows(
        &mut self,
        limits: super::CaptureLimits,
    ) -> Result<Vec<crate::window_capture::WindowInfo>, super::CaptureError> {
        self.window.enumerate(limits).map_err(Into::into)
    }
    fn capture_window(
        &mut self,
        binding: &skylive_core::WindowCaptureBinding,
        limits: super::CaptureLimits,
    ) -> Result<skylive_media::RgbaFrame, super::CaptureError> {
        self.window.capture(binding, limits).map_err(Into::into)
    }
}
