//! Factories are inert; platform resources are constructed on the audio owner.
use super::{
    AudioBackendFactory, AudioBindingDefaults, AudioCapabilities, AudioCaptureError,
    NativeAudioBackend,
};
use std::time::Instant;
#[cfg(target_os = "windows")]
#[path = "native_windows.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "native_macos.rs"]
mod platform;

pub fn capabilities() -> AudioCapabilities {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        platform::capabilities()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        AudioCapabilities {
            backend: "unsupported",
            input: false,
            output: false,
            device_enumeration: false,
            exclude_own_audio: false,
            note: Some("Native audio capture is not implemented on this target"),
        }
    }
}
pub fn factory() -> AudioBackendFactory {
    Box::new(backend)
}
/// Sample the desktop's monotonic media origin around one QPC read. This does
/// not establish when a frame was displayed or alter the audio route.
pub(crate) fn correlate_media_clock(origin: Instant, session: u64, clock_epoch: u64)
    -> Option<skylive_media::QpcMediaCorrelation> {
    #[cfg(target_os = "windows")]
    { platform::correlate_media_clock(origin, session, clock_epoch) }
    #[cfg(not(target_os = "windows"))]
    { let _ = (origin, session, clock_epoch); None }
}
pub fn binding_defaults() -> AudioBindingDefaults {
    #[cfg(target_os = "macos")]
    {
        AudioBindingDefaults {
            input_device_timing: true,
            output_device_timing: true,
            output_exclude_own_audio: true,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        AudioBindingDefaults::default()
    }
}
pub fn backend() -> Result<Box<dyn NativeAudioBackend>, AudioCaptureError> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        platform::backend()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Err(AudioCaptureError::Unsupported(
            "native audio capture target",
        ))
    }
}
