//! Factories are inert. SDK initialization occurs only when the worker invokes
//! the factory after an explicit enumerate/query/capture request.
use super::*;
#[cfg(target_os = "windows")]
#[path = "native_windows.rs"]
mod platform;
#[cfg(target_os = "windows")]
pub(super) use platform::{decode_directshow_buffer,directshow_video_layout};
#[cfg(target_os = "macos")]
#[path = "native_macos.rs"]
mod platform;
pub fn capabilities() -> CameraCapabilities {
    #[cfg(target_os = "windows")]
    {
        CameraCapabilities {
            available: true,
            backend: "windows-media-foundation",
            video: true,
            audio: true,
            note: Some(
                "Source-level vertical flip before filters. Optional WASAPI input or supported embedded DirectShow PCM audio; DirectSound/WaveOut playback is unavailable. RGB24/RGB32/ARGB32, YUY2/UYVY, NV12, I420/IYUV, YV12 and MJPEG modes are supported. H.264 uses a bounded progressive 8-bit 4:2:0 decoder; unsupported bitstream and color metadata are reported explicitly.",
            ),
        }
    }
    #[cfg(target_os = "macos")]
    {
        CameraCapabilities {
            available: true,
            backend: "macos-avfoundation",
            video: true,
            audio: false,
            note: Some(
                "macOS 14+, existing Camera permission; exact native BGRA mode; no camera audio",
            ),
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        CameraCapabilities {
            available: false,
            backend: "unsupported",
            video: false,
            audio: false,
            note: Some("camera capture is unavailable on this platform"),
        }
    }
}
pub fn factory() -> CameraBackendFactory {
    Box::new(|| {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            Ok(Box::new(platform::NativeBackend::new()?))
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            Err(CameraError::Unavailable(
                "camera capture is unavailable on this platform",
            ))
        }
    })
}
