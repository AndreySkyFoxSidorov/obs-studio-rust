//! Durable window selectors. Native handles and process IDs belong to the host.
use crate::{Error, MAX_DISPLAY_PIXELS};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowMatchPriority {
    Title,
    Class,
    Executable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowCaptureMethod {
    Auto,
    Gdi,
    Wgc,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum WindowTarget {
    Windows {
        title: String,
        class: String,
        executable: String,
    },
    Macos {
        title: String,
        bundle_id: String,
    },
}
impl WindowTarget {
    fn validate(&self) -> Result<(), Error> {
        let required = |s: &str| !s.trim().is_empty() && valid_text(s);
        let valid = match self {
            Self::Windows {
                title,
                class,
                executable,
            } => {
                valid_text(title)
                    && required(class)
                    && required(executable)
                    && !executable.contains(['/', '\\'])
            }
            Self::Macos { title, bundle_id } => required(title) && required(bundle_id),
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Invalid(
                "window selector requires bounded title/class/application strings without control characters",
            ))
        }
    }
    fn normalized(self) -> Self {
        let shrink = |s: String| s.into_boxed_str().into_string();
        match self {
            Self::Windows {
                title,
                class,
                executable,
            } => Self::Windows {
                title: shrink(title),
                class: shrink(class),
                executable: shrink(executable),
            },
            Self::Macos { title, bundle_id } => Self::Macos {
                title: shrink(title),
                bundle_id: shrink(bundle_id),
            },
        }
    }
}
fn valid_text(value: &str) -> bool {
    value.len() <= 1024 && !value.chars().any(char::is_control)
}

/// Durable Window Capture settings before the target's dimensions are known.
/// Unlike a binding, this value does not assert that the window is open.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "WindowCaptureRequestData")]
pub struct WindowCaptureRequest {
    target: WindowTarget,
    priority: WindowMatchPriority,
    capture_cursor: bool,
    client_area: bool,
    method: WindowCaptureMethod,
    capture_audio: bool,
    force_sdr: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowCaptureRequestData {
    target: WindowTarget,
    priority: WindowMatchPriority,
    capture_cursor: bool,
    client_area: bool,
    method: WindowCaptureMethod,
    capture_audio: bool,
    force_sdr: bool,
}
impl TryFrom<WindowCaptureRequestData> for WindowCaptureRequest {
    type Error = Error;
    fn try_from(value: WindowCaptureRequestData) -> Result<Self, Error> {
        Self::new(value.target, value.priority, value.capture_cursor, value.client_area,
            value.method, value.capture_audio, value.force_sdr)
    }
}
impl WindowCaptureRequest {
    pub fn new(target: WindowTarget, priority: WindowMatchPriority, capture_cursor: bool,
        client_area: bool, method: WindowCaptureMethod, capture_audio: bool, force_sdr: bool) -> Result<Self, Error> {
        target.validate()?;
        if matches!(&target, WindowTarget::Macos { .. }) && method != WindowCaptureMethod::Auto {
            return Err(Error::Invalid("macOS window capture requires the ScreenCaptureKit Auto method"));
        }
        if capture_audio {
            let WindowTarget::Windows { title, class, executable } = &target else {
                return Err(Error::Invalid("window audio capture requires Windows"));
            };
            let audio_priority = match priority {
                WindowMatchPriority::Title => crate::WindowsAudioWindowPriority::Title,
                WindowMatchPriority::Class => crate::WindowsAudioWindowPriority::Class,
                WindowMatchPriority::Executable => crate::WindowsAudioWindowPriority::Executable,
            };
            crate::WindowsAudioWindowSelector::new(title.clone(), class.clone(), executable.clone(), audio_priority)?;
        }
        Ok(Self { target: target.normalized(), priority, capture_cursor, client_area, method, capture_audio, force_sdr })
    }
    pub fn target(&self) -> &WindowTarget { &self.target }
    pub fn priority(&self) -> WindowMatchPriority { self.priority }
    pub fn capture_cursor(&self) -> bool { self.capture_cursor }
    pub fn client_area(&self) -> bool { self.client_area }
    pub fn method(&self) -> WindowCaptureMethod { self.method }
    pub fn capture_audio(&self) -> bool { self.capture_audio }
    pub fn force_sdr(&self) -> bool { self.force_sdr }
    pub fn resolve(self, width: u32, height: u32) -> Result<WindowCaptureBinding, Error> {
        WindowCaptureBinding::new(self.target, self.priority, self.capture_cursor, self.client_area, width, height)?
            .with_method(self.method)?.with_capture_audio(self.capture_audio).map(|binding| binding.with_force_sdr(self.force_sdr))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "BindingData")]
pub struct WindowCaptureBinding {
    method: WindowCaptureMethod,
    target: WindowTarget,
    priority: WindowMatchPriority,
    capture_cursor: bool,
    client_area: bool,
    width: u32,
    height: u32,
    #[serde(default)]
    capture_audio: bool,
    #[serde(default = "default_force_sdr", skip_serializing_if = "is_force_sdr_default")]
    force_sdr: bool,
    // Derived from the durable window selector; it is regenerated on load.
    #[serde(skip)]
    audio: Option<crate::AudioCaptureBinding>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingData {
    #[serde(default)]
    method: Option<WindowCaptureMethod>,
    target: WindowTarget,
    priority: WindowMatchPriority,
    capture_cursor: bool,
    client_area: bool,
    width: u32,
    height: u32,
    #[serde(default)]
    capture_audio: bool,
    #[serde(default = "default_force_sdr")]
    force_sdr: bool,
}
fn default_force_sdr() -> bool { true }
fn is_force_sdr_default(value:&bool) -> bool { *value }
impl TryFrom<BindingData> for WindowCaptureBinding {
    type Error = Error;
    fn try_from(v: BindingData) -> Result<Self, Error> {
        let binding = Self::new(
            v.target,
            v.priority,
            v.capture_cursor,
            v.client_area,
            v.width,
            v.height,
        )?;
        let binding = binding.with_capture_audio(v.capture_audio)?.with_force_sdr(v.force_sdr);
        match v.method {
            Some(method) => binding.with_method(method),
            None => Ok(binding),
        }
    }
}
impl WindowCaptureBinding {
    /// Preserve the selector and options independently of the last observed size.
    pub fn request(&self) -> WindowCaptureRequest {
        WindowCaptureRequest {
            target: self.target.clone(),
            priority: self.priority,
            capture_cursor: self.capture_cursor,
            client_area: self.client_area,
            method: self.method,
            capture_audio: self.capture_audio,
            force_sdr: self.force_sdr,
        }
    }
    pub fn new(
        target: WindowTarget,
        priority: WindowMatchPriority,
        capture_cursor: bool,
        client_area: bool,
        width: u32,
        height: u32,
    ) -> Result<Self, Error> {
        let result = Self {
            method: if matches!(&target, WindowTarget::Windows { .. }) {
                WindowCaptureMethod::Gdi
            } else {
                WindowCaptureMethod::Auto
            },
            target,
            priority,
            capture_cursor,
            client_area,
            width,
            height,
            capture_audio: false,
            force_sdr: true,
            audio: None,
        };
        result.validate()?;
        Ok(Self {
            target: result.target.normalized(),
            ..result
        })
    }
    pub fn method(&self) -> WindowCaptureMethod {
        self.method
    }
    pub fn with_method(mut self, method: WindowCaptureMethod) -> Result<Self, Error> {
        self.method = method;
        self.validate()?;
        Ok(self)
    }
    pub fn target(&self) -> &WindowTarget {
        &self.target
    }
    pub fn priority(&self) -> WindowMatchPriority {
        self.priority
    }
    pub fn capture_cursor(&self) -> bool {
        self.capture_cursor
    }
    pub fn client_area(&self) -> bool {
        self.client_area
    }
    pub fn force_sdr(&self) -> bool { self.force_sdr }
    pub fn with_force_sdr(mut self, force_sdr: bool) -> Self { self.force_sdr = force_sdr; self }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn capture_audio(&self) -> bool { self.capture_audio }
    pub fn audio(&self) -> Option<&crate::AudioCaptureBinding> { self.audio.as_ref() }
    /// The audio target follows the same durable selector and priority as the
    /// video source, so editing the window changes both bindings atomically.
    pub fn with_capture_audio(mut self, enabled: bool) -> Result<Self, Error> {
        self.audio = if enabled {
            let WindowTarget::Windows {title, class, executable} = &self.target else {
                return Err(Error::Invalid("window audio capture requires Windows"));
            };
            let priority = match self.priority {
                WindowMatchPriority::Title => crate::WindowsAudioWindowPriority::Title,
                WindowMatchPriority::Class => crate::WindowsAudioWindowPriority::Class,
                WindowMatchPriority::Executable => crate::WindowsAudioWindowPriority::Executable,
            };
            let selector = crate::WindowsAudioWindowSelector::new(title.clone(), class.clone(), executable.clone(), priority)?;
            Some(crate::AudioCaptureBinding::new(crate::AudioFlow::Output,
                crate::AudioTarget::WindowsWindow { selector }, false, false)?)
        } else { None };
        self.capture_audio = enabled;
        self.validate()?;
        Ok(self)
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        self.target.validate()?;
        if self.capture_audio != self.audio.is_some() || self.capture_audio && !matches!(&self.target,WindowTarget::Windows {..}) {
            return Err(Error::Invalid("window audio binding is inconsistent"));
        }
        if matches!(&self.target, WindowTarget::Macos { .. })
            && self.method != WindowCaptureMethod::Auto
        {
            return Err(Error::Invalid(
                "macOS window capture requires the ScreenCaptureKit Auto method",
            ));
        }
        if self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || u64::from(self.width) * u64::from(self.height) > MAX_DISPLAY_PIXELS
        {
            return Err(Error::Invalid(
                "window capture basis exceeds 8192-edge/32M-pixel limits",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target() -> WindowTarget {
        WindowTarget::Windows {
            title: "Project вЂ” Editor".into(),
            class: "EditorWindow".into(),
            executable: "editor.exe".into(),
        }
    }
    #[test]
    fn durable_selector_preserves_unicode_and_exact_basis() {
        let binding =
            WindowCaptureBinding::new(target(), WindowMatchPriority::Class, true, true, 7680, 4320)
                .unwrap();
        assert_eq!(binding.width(), 7680);
        assert_eq!(binding.target(), &target());
        assert_eq!(binding.priority(), WindowMatchPriority::Class);
        assert!(binding.capture_cursor());
        assert!(binding.client_area());
    }
    #[test]
    fn invalid_or_oversized_identity_and_basis_reject_without_pixels() {
        for target in [
            WindowTarget::Windows {
                title: "a\0b".into(),
                class: "x".into(),
                executable: "x.exe".into(),
            },
            WindowTarget::Windows {
                title: "".into(),
                class: "x".into(),
                executable: "C:\\x.exe".into(),
            },
            WindowTarget::Macos {
                title: "x".into(),
                bundle_id: "".into(),
            },
            WindowTarget::Macos {
                title: "x".repeat(1025),
                bundle_id: "org.example".into(),
            },
        ] {
            assert!(
                WindowCaptureBinding::new(target, WindowMatchPriority::Title, true, true, 1, 1)
                    .is_err()
            );
        }
        assert!(
            WindowCaptureBinding::new(target(), WindowMatchPriority::Title, true, true, 8192, 4097)
                .is_err()
        );
    }
    #[test]
    fn live_window_fit_uses_runtime_dimensions_and_undo_preserves_binding_basis() {
        use crate::{Command, Session, TransformAction};
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::SetVideo {
                width: 16,
                height: 16,
                fps_num: 30,
                fps_den: 1,
            })
            .unwrap();
        let binding =
            WindowCaptureBinding::new(target(), WindowMatchPriority::Class, false, true, 2, 2)
                .unwrap();
        let item = session
            .apply(Command::AddWindowCaptureSource {
                scene,
                name: "Window".into(),
                binding,
            })
            .unwrap()
            .created
            .unwrap();
        let before = session.snapshot();
        session
            .apply(Command::TransformItemForFrame {
                scene,
                item,
                action: TransformAction::FitToCanvas,
                frame_width: 4,
                frame_height: 2,
            })
            .unwrap();
        let current = session.snapshot();
        let t = current.scene(scene).unwrap().items()[0].transform();
        assert_eq!((t.width, t.height, t.x, t.y), (8.0, 8.0, 0.0, 4.0));
        let rejected = session.snapshot();
        assert!(
            session
                .apply(Command::TransformItemForFrame {
                    scene,
                    item,
                    action: TransformAction::FitToCanvas,
                    frame_width: 0,
                    frame_height: 2
                })
                .is_err()
        );
        assert!(std::sync::Arc::ptr_eq(&rejected, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0].transform(),
            before.scene(scene).unwrap().items()[0].transform()
        );
    }
}

#[cfg(test)]
mod method_tests {
    use super::*;
    #[test]
    fn platform_defaults_are_legacy_backend_defaults_not_automatic_upgrade() {
        let b = WindowCaptureBinding::new(
            WindowTarget::Windows {
                title: "x".into(),
                class: "x".into(),
                executable: "x.exe".into(),
            },
            WindowMatchPriority::Title,
            true,
            true,
            16,
            16,
        )
        .unwrap();
        assert_eq!(b.method(), WindowCaptureMethod::Gdi);
        assert_eq!(
            b.clone()
                .with_method(WindowCaptureMethod::Wgc)
                .unwrap()
                .width(),
            16
        );
        let mac = WindowCaptureBinding::new(
            WindowTarget::Macos {
                title: "x".into(),
                bundle_id: "org.x".into(),
            },
            WindowMatchPriority::Title,
            true,
            false,
            16,
            16,
        )
        .unwrap();
        assert_eq!(mac.method(), WindowCaptureMethod::Auto);
        assert!(mac.with_method(WindowCaptureMethod::Wgc).is_err());
    }
}
