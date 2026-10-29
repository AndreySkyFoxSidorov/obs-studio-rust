//! Portable authored display binding. OS enumeration, permissions, handles,
//! capture timestamps and current frame dimensions belong to the host.
use crate::Error;
use serde::{Deserialize, Serialize};

/// Native Display Capture profile: includes 5K/6K/8K displays, independent of
/// image-file decode limits and preview output dimensions.
pub const MAX_DISPLAY_PIXELS: u64 = 33_554_432;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayCaptureMethod {
    Auto,
    Gdi,
    Dxgi,
    Wgc,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum DisplayTarget {
    Windows { device_id: String },
    Macos { uuid: String },
}
impl DisplayTarget {
    fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Windows { device_id } => {
                if device_id.trim().is_empty()
                    || device_id.len() > 1024
                    || device_id.chars().any(char::is_control)
                {
                    return Err(Error::Invalid(
                        "display device ID requires 1..=1024 UTF-8 bytes without controls",
                    ));
                }
            }
            Self::Macos { uuid } => {
                if uuid.len() != 36
                    || !uuid.bytes().enumerate().all(|(i, c)| {
                        if matches!(i, 8 | 13 | 18 | 23) {
                            c == b'-'
                        } else {
                            c.is_ascii_hexdigit()
                        }
                    })
                {
                    return Err(Error::Invalid(
                        "macOS display UUID must contain 36 hexadecimal/hyphen bytes",
                    ));
                }
            }
        }
        Ok(())
    }
    fn normalized(self) -> Self {
        match self {
            Self::Windows { device_id } => Self::Windows {
                device_id: device_id.into_boxed_str().into_string(),
            },
            Self::Macos { uuid } => Self::Macos {
                uuid: uuid.into_boxed_str().into_string(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "BindingData")]
pub struct DisplayCaptureBinding {
    method: DisplayCaptureMethod,
    target: DisplayTarget,
    capture_cursor: bool,
    width: u32,
    height: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingData {
    #[serde(default)]
    method: Option<DisplayCaptureMethod>,
    target: DisplayTarget,
    capture_cursor: bool,
    width: u32,
    height: u32,
}
impl TryFrom<BindingData> for DisplayCaptureBinding {
    type Error = Error;
    fn try_from(value: BindingData) -> Result<Self, Error> {
        let binding = Self::new(
            value.target,
            value.capture_cursor,
            value.width,
            value.height,
        )?;
        if let Some(method) = value.method {
            binding.with_method(method)
        } else {
            Ok(binding)
        }
    }
}
impl DisplayCaptureBinding {
    /// Width/height are the initial native capture-space scale basis, never a
    /// requested output size. A later live frame may have different dimensions.
    pub fn new(
        target: DisplayTarget,
        capture_cursor: bool,
        width: u32,
        height: u32,
    ) -> Result<Self, Error> {
        let binding = Self {
            method: match &target {
                DisplayTarget::Windows { .. } => DisplayCaptureMethod::Gdi,
                DisplayTarget::Macos { .. } => DisplayCaptureMethod::Auto,
            },
            target,
            capture_cursor,
            width,
            height,
        };
        binding.validate()?;
        Ok(Self {
            target: binding.target.normalized(),
            ..binding
        })
    }
    pub fn target(&self) -> &DisplayTarget {
        &self.target
    }
    pub fn method(&self) -> DisplayCaptureMethod {
        self.method
    }
    pub fn with_method(mut self, method: DisplayCaptureMethod) -> Result<Self, Error> {
        self.method = method;
        self.validate()?;
        Ok(self)
    }
    pub fn capture_cursor(&self) -> bool {
        self.capture_cursor
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        self.target.validate()?;
        if matches!(self.target, DisplayTarget::Macos { .. })
            && self.method != DisplayCaptureMethod::Auto
        {
            return Err(Error::Invalid(
                "GDI, DXGI and WGC capture methods require a Windows display target",
            ));
        }
        if self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || u64::from(self.width) * u64::from(self.height) > MAX_DISPLAY_PIXELS
        {
            return Err(Error::Invalid(
                "display capture basis exceeds 8192-edge/32M-pixel resource limits",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, Crop, Session, SourceKind, Transform};
    use std::sync::Arc;
    #[test]
    fn capture_method_defaults_preserve_legacy_platform_and_invalid_choices_reject() {
        let windows = DisplayTarget::Windows {
            device_id: "monitor-interface".into(),
        };
        let mac = DisplayTarget::Macos {
            uuid: "01234567-89AB-cdef-0123-456789abcdef".into(),
        };
        for (target, expected) in [
            (windows, DisplayCaptureMethod::Gdi),
            (mac, DisplayCaptureMethod::Auto),
        ] {
            let legacy = DisplayCaptureBinding::try_from(BindingData {
                method: None,
                target,
                capture_cursor: true,
                width: 2,
                height: 2,
            })
            .unwrap();
            assert_eq!(legacy.method(), expected);
            if matches!(legacy.target(), DisplayTarget::Macos { .. }) {
                assert!(
                    legacy
                        .clone()
                        .with_method(DisplayCaptureMethod::Gdi)
                        .is_err()
                );
                assert!(legacy.with_method(DisplayCaptureMethod::Dxgi).is_err());
            } else {
                assert_eq!(
                    legacy
                        .with_method(DisplayCaptureMethod::Auto)
                        .unwrap()
                        .method(),
                    DisplayCaptureMethod::Auto
                );
            }
        }
        let unknown = serde::de::value::StrDeserializer::<serde::de::value::Error>::new("wgc");
        assert!(DisplayCaptureMethod::deserialize(unknown).is_err());
    }
    #[test]
    fn method_rebind_is_one_undo_preserving_item_identity_and_authored_geometry() {
        let mut session = Session::new();
        let scene = session.preview();
        let original = binding(8, 4);
        let item = session
            .apply(Command::AddDisplayCaptureSource {
                scene,
                name: "Method choice".into(),
                binding: original.clone(),
            })
            .unwrap()
            .created
            .unwrap();
        let before = session.snapshot();
        let source = before.scene(scene).unwrap().items()[0].source();
        let changed = original.with_method(DisplayCaptureMethod::Dxgi).unwrap();
        session
            .apply(Command::RebindDisplayCapture {
                source,
                binding: changed.clone(),
            })
            .unwrap();
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0].id(),
            item
        );
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0].transform(),
            before.scene(scene).unwrap().items()[0].transform()
        );
        assert_eq!(
            session.snapshot().source(source).unwrap().kind(),
            &SourceKind::DisplayCapture { binding: changed }
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().source(source).unwrap().kind(),
            before.source(source).unwrap().kind()
        );
    }
    fn binding(width: u32, height: u32) -> DisplayCaptureBinding {
        DisplayCaptureBinding::new(
            DisplayTarget::Windows {
                device_id: r"MONITOR\DISPLAY#1".into(),
            },
            true,
            width,
            height,
        )
        .unwrap()
    }
    #[test]
    fn native_five_six_eight_k_profiles_admit_metadata_without_allocating_pixels() {
        for (width, height) in [(5120, 2880), (6016, 3384), (7680, 4320), (8192, 4096)] {
            let target = DisplayTarget::Macos {
                uuid: "aBcDeF01-2345-6789-aBcD-Ef0123456789".into(),
            };
            let bound = DisplayCaptureBinding::new(target, true, width, height).unwrap();
            assert_eq!((bound.width(), bound.height()), (width, height));
            assert!(u64::from(width) * u64::from(height) <= MAX_DISPLAY_PIXELS);
        }
        assert_eq!(MAX_DISPLAY_PIXELS, 33_554_432);
        assert!(
            DisplayCaptureBinding::new(
                DisplayTarget::Windows {
                    device_id: "id".into()
                },
                true,
                8192,
                4097
            )
            .is_err()
        );
    }
    #[test]
    fn portable_binding_validates_exact_identity_bounds_and_retained_allocations() {
        let mut device_id = String::with_capacity(4096);
        device_id.push_str(r"gdi:\\.\DISPLAY1");
        let expected = device_id.clone();
        let bound =
            DisplayCaptureBinding::new(DisplayTarget::Windows { device_id }, true, 8192, 2048)
                .unwrap();
        assert_eq!(
            bound.target(),
            &DisplayTarget::Windows {
                device_id: expected
            }
        );
        assert!(bound.capture_cursor());
        assert_eq!((bound.width(), bound.height()), (8192, 2048));
        let DisplayTarget::Windows { device_id } = bound.target() else {
            panic!()
        };
        assert_eq!(device_id.capacity(), device_id.len());
        let mut uuid = String::with_capacity(4096);
        uuid.push_str("aBcDeF01-2345-6789-aBcD-Ef0123456789");
        let expected = uuid.clone();
        let bound =
            DisplayCaptureBinding::new(DisplayTarget::Macos { uuid }, false, 16, 9).unwrap();
        assert_eq!(bound.target(), &DisplayTarget::Macos { uuid: expected });
        let DisplayTarget::Macos { uuid } = bound.target() else {
            panic!()
        };
        assert_eq!(uuid.capacity(), 36);
        for target in [
            DisplayTarget::Windows {
                device_id: "".into(),
            },
            DisplayTarget::Windows {
                device_id: "a\0b".into(),
            },
            DisplayTarget::Windows {
                device_id: "a".repeat(1025),
            },
            DisplayTarget::Macos {
                uuid: "not-a-uuid".into(),
            },
            DisplayTarget::Macos {
                uuid: "aBcDeF01_2345-6789-aBcD-Ef0123456789".into(),
            },
        ] {
            assert!(DisplayCaptureBinding::new(target, false, 1, 1).is_err());
        }
        for (w, h) in [
            (0, 1),
            (1, 0),
            (8193, 1),
            (1, 8193),
            (8192, 4097),
            (u32::MAX, u32::MAX),
        ] {
            assert!(
                DisplayCaptureBinding::new(
                    DisplayTarget::Windows {
                        device_id: "id".into()
                    },
                    false,
                    w,
                    h
                )
                .is_err()
            );
        }
    }
    #[test]
    fn add_rebind_preserves_source_identity_scale_crop_shared_refs_and_undo() {
        let mut session = Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddDisplayCaptureSource {
                scene,
                name: "Display".into(),
                binding: binding(8, 4),
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().scene(scene).unwrap().items()[0].source();
        let second = session
            .apply(Command::AddScene {
                name: "Shared".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let other = session
            .apply(Command::AddExistingSource {
                scene: second,
                source,
            })
            .unwrap()
            .created
            .unwrap();
        let transformed = Transform {
            width: 16.0,
            height: 8.0,
            rotation_degrees: 90.0,
            flip_x: true,
            crop: Crop {
                left: 1,
                right: 2,
                top: 1,
                bottom: 0,
            },
            ..Transform::full(8, 4)
        };
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: transformed.clone(),
            })
            .unwrap();
        let before = session.snapshot();
        session
            .apply(Command::RebindDisplayCapture {
                source,
                binding: binding(16, 8),
            })
            .unwrap();
        let after = session.snapshot();
        assert_eq!(after.sources().len(), 1);
        assert_eq!(after.sources()[0].id(), source);
        assert_eq!(after.sources()[0].kind().dimensions(), (16, 8));
        let current = after.scene(scene).unwrap().items()[0].transform();
        assert_eq!((current.width, current.height), (32.0, 16.0));
        assert_eq!(current.crop, transformed.crop);
        assert_eq!(current.rotation_degrees, 90.0);
        assert!(current.flip_x);
        let shared = after
            .scene(second)
            .unwrap()
            .items()
            .iter()
            .find(|i| i.id() == other)
            .unwrap()
            .transform();
        assert_eq!((shared.width, shared.height), (16.0, 8.0));
        assert!(
            !session
                .apply(Command::RebindDisplayCapture {
                    source,
                    binding: binding(16, 8)
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&after, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().source(source).unwrap().kind(),
            before.source(source).unwrap().kind()
        );
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0].transform(),
            &transformed
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session.snapshot().source(source).unwrap().kind(),
            after.source(source).unwrap().kind()
        );
    }
    #[test]
    fn invalid_rebind_and_wrong_source_preserve_entire_snapshot_history_and_basis() {
        let mut session = Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddDisplayCaptureSource {
                scene,
                name: "Display".into(),
                binding: binding(8, 4),
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().scene(scene).unwrap().items()[0].source();
        let transform = Transform {
            crop: Crop {
                left: 3,
                right: 2,
                ..Default::default()
            },
            ..Transform::full(8, 4)
        };
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform,
            })
            .unwrap();
        let before = session.snapshot();
        assert!(
            session
                .apply(Command::RebindDisplayCapture {
                    source,
                    binding: binding(5, 4)
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        assert!(
            session
                .apply(Command::RebindDisplayCapture {
                    source: crate::Id::new(u64::MAX),
                    binding: binding(8, 4)
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0]
                .transform()
                .crop,
            Crop::default()
        );
        let mut plain = Session::new();
        let scene = plain.preview();
        plain
            .apply(Command::AddColorSource {
                scene,
                name: "Color".into(),
                rgba: [255; 4],
            })
            .unwrap();
        let source = plain.snapshot().sources()[0].id();
        let before = plain.snapshot();
        assert!(
            plain
                .apply(Command::RebindDisplayCapture {
                    source,
                    binding: binding(8, 4)
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &plain.snapshot()));
        assert!(matches!(
            plain.snapshot().sources()[0].kind(),
            SourceKind::Color { .. }
        ));
    }
    #[test]
    fn cursor_preference_is_authored_and_capture_sources_follow_existing_reference_lifecycle() {
        let mut session = Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddDisplayCaptureSource {
                scene,
                name: "Display".into(),
                binding: binding(8, 4),
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().scene(scene).unwrap().items()[0].source();
        let before = session.snapshot();
        let mut changed = binding(8, 4);
        changed.capture_cursor = false;
        let change = session
            .apply(Command::RebindDisplayCapture {
                source,
                binding: changed.clone(),
            })
            .unwrap();
        assert!(change.changed);
        assert_eq!(change.revision, before.revision() + 1);
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items()[0].transform(),
            before.scene(scene).unwrap().items()[0].transform()
        );
        assert_eq!(
            session.snapshot().source(source).unwrap().kind(),
            &SourceKind::DisplayCapture {
                binding: changed.clone()
            }
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().source(source).unwrap().kind(),
            before.source(source).unwrap().kind()
        );
        session.apply(Command::Redo).unwrap();
        session.apply(Command::RemoveItem { scene, item }).unwrap();
        assert!(session.snapshot().source(source).is_none());
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().source(source).unwrap().kind(),
            &SourceKind::DisplayCapture { binding: changed }
        );
    }
}
