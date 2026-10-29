//! Presentation policy injected by the composition root; no device access.
use skylive_core::{AudioFlow, AudioTarget, DisplayTarget, VideoCaptureTarget, WindowTarget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapturePlatform {
    Windows,
    Macos,
    Unavailable,
}
impl CapturePlatform {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Unavailable
        }
    }
    pub fn audio(self, flow: AudioFlow, target: &AudioTarget) -> bool {
        match target {
            AudioTarget::Default => self != Self::Unavailable,
            AudioTarget::WindowsProcess { .. } | AudioTarget::WindowsWindow { .. } => self == Self::Windows && flow == AudioFlow::Output,
            AudioTarget::WindowsEndpoint { .. } => self == Self::Windows,
            AudioTarget::MacDevice { .. } => self == Self::Macos && flow == AudioFlow::Input,
            AudioTarget::MacSystem => self == Self::Macos && flow == AudioFlow::Output,
        }
    }
    pub fn display(self, target: &DisplayTarget) -> bool {
        matches!(
            (self, target),
            (Self::Windows, DisplayTarget::Windows { .. })
                | (Self::Macos, DisplayTarget::Macos { .. })
        )
    }
    pub fn camera(self, target: &VideoCaptureTarget) -> bool {
        matches!(
            (self, target),
            (Self::Windows, VideoCaptureTarget::Windows { .. })
                | (Self::Macos, VideoCaptureTarget::Macos { .. })
        )
    }
    pub fn window(self, target: &WindowTarget) -> bool {
        matches!(
            (self, target),
            (Self::Windows, WindowTarget::Windows { .. })
                | (Self::Macos, WindowTarget::Macos { .. })
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_platform_policy_never_accepts_foreign_identifiers_or_wrong_audio_flow() {
        let windows = CapturePlatform::Windows;
        let macos = CapturePlatform::Macos;
        for flow in [AudioFlow::Input, AudioFlow::Output] {
            assert!(windows.audio(flow, &AudioTarget::Default));
            assert!(windows.audio(flow, &AudioTarget::WindowsEndpoint { id: "exact".into() }));
            assert!(!windows.audio(
                flow,
                &AudioTarget::MacDevice {
                    uid: "exact".into()
                }
            ));
            assert!(!windows.audio(flow, &AudioTarget::MacSystem));
            assert!(!macos.audio(flow, &AudioTarget::WindowsEndpoint { id: "exact".into() }));
        }
        assert!(macos.audio(
            AudioFlow::Input,
            &AudioTarget::MacDevice {
                uid: "exact".into()
            }
        ));
        assert!(!macos.audio(
            AudioFlow::Output,
            &AudioTarget::MacDevice {
                uid: "exact".into()
            }
        ));
        assert!(macos.audio(AudioFlow::Output, &AudioTarget::MacSystem));
        assert!(!macos.audio(AudioFlow::Input, &AudioTarget::MacSystem));
        assert!(windows.display(&DisplayTarget::Windows {
            device_id: "exact".into()
        }));
        assert!(!windows.display(&DisplayTarget::Macos {
            uuid: "exact".into()
        }));
        assert!(macos.camera(&VideoCaptureTarget::Macos {
            unique_id: "exact".into()
        }));
        assert!(!macos.camera(&VideoCaptureTarget::Windows {
            symbolic_link: "exact".into()
        }));
        assert!(windows.window(&WindowTarget::Windows {
            title: "Title".into(),
            class: "Class".into(),
            executable: "app.exe".into()
        }));
        assert!(!windows.window(&WindowTarget::Macos {
            title: "Title".into(),
            bundle_id: "org.app".into()
        }));
    }
}
