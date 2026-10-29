//! Source-native crop and canvas-space affine evaluation. No UI/OS/media dependency.
use crate::{Error, TransformAction};
use serde::{Deserialize, Serialize};

/// Native source pixels, independent of item scale, preview resolution and DPI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Crop {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}
impl Crop {
    fn remaining(self, width: u32, height: u32) -> Result<(u32, u32), Error> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err(Error::Invalid("invalid source dimensions"));
        }
        let horizontal = u64::from(self.left) + u64::from(self.right);
        let vertical = u64::from(self.top) + u64::from(self.bottom);
        if horizontal >= u64::from(width) || vertical >= u64::from(height) {
            return Err(Error::Invalid(
                "crop must leave at least one native source pixel on each axis",
            ));
        }
        Ok((width - horizontal as u32, height - vertical as u32))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    /// OBS top-left-aligned item pivot in canvas pixels, not the rotated AABB.
    pub x: f32,
    pub y: f32,
    /// Positive scaled dimensions of the FULL source before crop.
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    #[serde(default)]
    pub rotation_degrees: f32,
    #[serde(default)]
    pub flip_x: bool,
    #[serde(default)]
    pub flip_y: bool,
    #[serde(default)]
    pub crop: Crop,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemBounds {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}
impl ItemBounds {
    pub fn width(self) -> f64 {
        self.right - self.left
    }
    pub fn height(self) -> f64 {
        self.bottom - self.top
    }
}
/// Unit-local cropped rectangle to canvas, plus normalized source sampling bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemGeometry {
    pub origin: [f64; 2],
    pub axis_x: [f64; 2],
    pub axis_y: [f64; 2],
    /// left, top, right, bottom in normalized full-source texture coordinates.
    pub uv: [f64; 4],
}
impl ItemGeometry {
    pub fn bounds(self) -> ItemBounds {
        let x = [
            self.origin[0],
            self.origin[0] + self.axis_x[0],
            self.origin[0] + self.axis_y[0],
            self.origin[0] + self.axis_x[0] + self.axis_y[0],
        ];
        let y = [
            self.origin[1],
            self.origin[1] + self.axis_x[1],
            self.origin[1] + self.axis_y[1],
            self.origin[1] + self.axis_x[1] + self.axis_y[1],
        ];
        ItemBounds {
            left: x.into_iter().fold(f64::INFINITY, f64::min),
            top: y.into_iter().fold(f64::INFINITY, f64::min),
            right: x.into_iter().fold(f64::NEG_INFINITY, f64::max),
            bottom: y.into_iter().fold(f64::NEG_INFINITY, f64::max),
        }
    }
}
impl Transform {
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
            opacity: 1.0,
            rotation_degrees: 0.0,
            flip_x: false,
            flip_y: false,
            crop: Crop::default(),
        }
    }
    fn validate(&self) -> Result<(), Error> {
        // Full-source extents can be large when fitting a one-pixel crop to a
        // 4096-pixel canvas: 8192*4096. Raster work is bounded after clipping.
        if ![
            self.x,
            self.y,
            self.width,
            self.height,
            self.opacity,
            self.rotation_degrees,
        ]
        .iter()
        .all(|v| v.is_finite())
            || self.x.abs() > 67_108_864.0
            || self.y.abs() > 67_108_864.0
            || !(0.001..=33_554_432.0).contains(&self.width)
            || !(0.001..=33_554_432.0).contains(&self.height)
            || !(0.0..=1.0).contains(&self.opacity)
            || self.rotation_degrees.abs() > 360_000.0
        {
            return Err(Error::Invalid("invalid transform"));
        }
        Ok(())
    }
    pub fn geometry(&self, source_width: u32, source_height: u32) -> Result<ItemGeometry, Error> {
        self.geometry_for_frame(source_width, source_height, source_width, source_height)
    }
    /// Evaluate current native pixels against the authored scale basis. Frozen
    /// scene layouts supply their captured basis; live captures use the binding
    /// basis. Derived f64 axes may exceed authored extent limits or be tiny.
    pub fn geometry_for_frame(
        &self,
        basis_width: u32,
        basis_height: u32,
        frame_width: u32,
        frame_height: u32,
    ) -> Result<ItemGeometry, Error> {
        self.validate()?;
        if basis_width==0 || basis_height==0 || basis_width>8192 || basis_height>8192 {return Err(Error::Invalid("invalid source scale basis"));}
        let (remaining_w, remaining_h) = self.crop.remaining(frame_width, frame_height)?;
        let angle = f64::from(self.rotation_degrees).rem_euclid(360.0);
        // Exact quarter turns prevent trigonometric edge drift at pixel centers.
        let (sin, cos) = match angle {
            0.0 => (0.0, 1.0),
            90.0 => (1.0, 0.0),
            180.0 => (0.0, -1.0),
            270.0 => (-1.0, 0.0),
            _ => (angle.to_radians().sin(), angle.to_radians().cos()),
        };
        let w = f64::from(self.width) * f64::from(remaining_w) / f64::from(basis_width)
            * if self.flip_x { -1.0 } else { 1.0 };
        let h = f64::from(self.height) * f64::from(remaining_h) / f64::from(basis_height)
            * if self.flip_y { -1.0 } else { 1.0 };
        Ok(ItemGeometry {
            origin: [f64::from(self.x), f64::from(self.y)],
            axis_x: [cos * w, sin * w],
            axis_y: [-sin * h, cos * h],
            uv: [
                f64::from(self.crop.left) / f64::from(frame_width),
                f64::from(self.crop.top) / f64::from(frame_height),
                f64::from(frame_width - self.crop.right) / f64::from(frame_width),
                f64::from(frame_height - self.crop.bottom) / f64::from(frame_height),
            ],
        })
    }
    pub(crate) fn apply_action(
        &mut self,
        action: TransformAction,
        source_w: u32,
        source_h: u32,
        canvas_w: u32,
        canvas_h: u32,
    ) -> Result<(), Error> {
        self.apply_action_for_frame(
            action,
            (source_w, source_h),
            (source_w, source_h),
            (canvas_w, canvas_h),
        )
    }
    pub(crate) fn apply_action_for_frame(
        &mut self,
        action: TransformAction,
        basis: (u32, u32),
        frame: (u32, u32),
        canvas: (u32, u32),
    ) -> Result<(), Error> {
        let (basis_w, basis_h) = basis;
        let (frame_w, frame_h) = frame;
        let (canvas_w, canvas_h) = canvas;
        self.validate()?;
        if basis_w==0 || basis_h==0 || basis_w>8192 || basis_h>8192 {return Err(Error::Invalid("invalid source scale basis"));}
        Crop::default().remaining(frame_w, frame_h)?;
        // Reset also repairs a crop that is valid in the authored basis but no
        // longer fits a smaller runtime frame; it does not need the old bounds.
        if matches!(action, TransformAction::Reset) {
            let opacity = self.opacity;
            *self = Self::full(basis_w, basis_h);
            self.opacity = opacity;
            self.geometry(basis_w, basis_h)?;
            return Ok(());
        }
        let old_bounds = self
            .geometry_for_frame(basis_w, basis_h, frame_w, frame_h)?
            .bounds();
        match action {
            TransformAction::Reset => unreachable!("reset returned before bounds evaluation"),
            TransformAction::FitToCanvas | TransformAction::StretchToCanvas => {
                let (remaining_w, remaining_h) = self.crop.remaining(frame_w, frame_h)?;
                let mut sx = f64::from(canvas_w) / f64::from(remaining_w);
                let mut sy = f64::from(canvas_h) / f64::from(remaining_h);
                if matches!(action, TransformAction::FitToCanvas) {
                    let scale = sx.min(sy);
                    sx = scale;
                    sy = scale;
                }
                self.width = (f64::from(basis_w) * sx) as f32;
                self.height = (f64::from(basis_h) * sy) as f32;
                self.rotation_degrees = 0.0;
                self.flip_x = false;
                self.flip_y = false;
                self.x = ((f64::from(canvas_w) - f64::from(remaining_w) * sx) / 2.0) as f32;
                self.y = ((f64::from(canvas_h) - f64::from(remaining_h) * sy) / 2.0) as f32;
            }
            TransformAction::Center
            | TransformAction::CenterHorizontal
            | TransformAction::CenterVertical => {
                if !matches!(action, TransformAction::CenterVertical) {
                    self.x = (f64::from(self.x) + (f64::from(canvas_w) - old_bounds.width()) / 2.0
                        - old_bounds.left) as f32;
                }
                if !matches!(action, TransformAction::CenterHorizontal) {
                    self.y = (f64::from(self.y) + (f64::from(canvas_h) - old_bounds.height()) / 2.0
                        - old_bounds.top) as f32;
                }
            }
            TransformAction::Rotate90Clockwise
            | TransformAction::Rotate90CounterClockwise
            | TransformAction::Rotate180
            | TransformAction::FlipHorizontal
            | TransformAction::FlipVertical => {
                match action {
                    TransformAction::FlipHorizontal => self.flip_x = !self.flip_x,
                    TransformAction::FlipVertical => self.flip_y = !self.flip_y,
                    _ => {
                        let delta = match action {
                            TransformAction::Rotate90Clockwise => 90.0,
                            TransformAction::Rotate90CounterClockwise => -90.0,
                            _ => 180.0,
                        };
                        self.rotation_degrees += delta;
                        if self.rotation_degrees >= 360.0 {
                            self.rotation_degrees -= 360.0;
                        } else if self.rotation_degrees <= -360.0 {
                            self.rotation_degrees += 360.0;
                        }
                    }
                }
                let new_bounds = self
                    .geometry_for_frame(basis_w, basis_h, frame_w, frame_h)?
                    .bounds();
                self.x = (f64::from(self.x) + old_bounds.left - new_bounds.left) as f32;
                self.y = (f64::from(self.y) + old_bounds.top - new_bounds.top) as f32;
            }
        }
        self.geometry_for_frame(basis_w,basis_h,frame_w,frame_h)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, DisplayCaptureBinding, DisplayTarget, Id, Session};
    use std::sync::Arc;
    fn capture_session(basis: (u32, u32), canvas: (u32, u32)) -> (Session, Id, Id) {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::SetVideo {
                width: canvas.0,
                height: canvas.1,
                fps_num: 30,
                fps_den: 1,
            })
            .unwrap();
        let binding = DisplayCaptureBinding::new(
            DisplayTarget::Windows {
                device_id: "test-display".into(),
            },
            true,
            basis.0,
            basis.1,
        )
        .unwrap();
        let item = session
            .apply(Command::AddDisplayCaptureSource {
                scene,
                name: "Display".into(),
                binding,
            })
            .unwrap()
            .created
            .unwrap();
        (session, scene, item)
    }
    fn item_transform(session: &Session, scene: Id, item: Id) -> Transform {
        session
            .snapshot()
            .scene(scene)
            .unwrap()
            .items()
            .iter()
            .find(|i| i.id() == item)
            .unwrap()
            .transform()
            .clone()
    }
    #[test]
    fn runtime_frame_fit_stretch_and_history_use_actual_size_with_authored_basis() {
        let (mut session, scene, item) = capture_session((8, 4), (32, 16));
        let original = Transform {
            crop: Crop {
                left: 1,
                right: 1,
                ..Default::default()
            },
            ..Transform::full(8, 4)
        };
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: original.clone(),
            })
            .unwrap();
        let before = session.snapshot();
        session
            .apply(Command::TransformItemForFrame {
                scene,
                item,
                action: TransformAction::FitToCanvas,
                frame_width: 16,
                frame_height: 8,
            })
            .unwrap();
        let fit = item_transform(&session, scene, item);
        assert_eq!((fit.width, fit.height, fit.x, fit.y), (16.0, 8.0, 2.0, 0.0));
        assert_eq!(fit.crop, original.crop);
        assert_eq!(
            fit.geometry_for_frame(8, 4, 16, 8).unwrap().bounds(),
            ItemBounds {
                left: 2.0,
                top: 0.0,
                right: 30.0,
                bottom: 16.0
            }
        );
        let fitted = session.snapshot();
        assert!(
            !session
                .apply(Command::TransformItemForFrame {
                    scene,
                    item,
                    action: TransformAction::FitToCanvas,
                    frame_width: 16,
                    frame_height: 8
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&fitted, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        assert_eq!(item_transform(&session, scene, item), original);
        assert_eq!(
            session
                .snapshot()
                .source(session.snapshot().scene(scene).unwrap().items()[0].source())
                .unwrap()
                .kind(),
            before.sources()[0].kind()
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(item_transform(&session, scene, item), fit);
        session
            .apply(Command::TransformItemForFrame {
                scene,
                item,
                action: TransformAction::StretchToCanvas,
                frame_width: 16,
                frame_height: 8,
            })
            .unwrap();
        let stretched = item_transform(&session, scene, item);
        let bounds = stretched.geometry_for_frame(8, 4, 16, 8).unwrap().bounds();
        assert!((bounds.width() - 32.0).abs() < 0.00001);
        assert_eq!((bounds.left, bounds.top, bounds.height()), (0.0, 0.0, 16.0));
    }
    #[test]
    fn runtime_frame_rotation_flip_center_and_reset_use_actual_rendered_bounds() {
        let (mut session, scene, item) = capture_session((8, 4), (32, 16));
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: Transform {
                    x: 10.0,
                    y: 20.0,
                    opacity: 0.4,
                    ..Transform::full(8, 4)
                },
            })
            .unwrap();
        for action in [
            TransformAction::Rotate90Clockwise,
            TransformAction::FlipHorizontal,
        ] {
            session
                .apply(Command::TransformItemForFrame {
                    scene,
                    item,
                    action,
                    frame_width: 16,
                    frame_height: 8,
                })
                .unwrap();
            let t = item_transform(&session, scene, item);
            let b = t.geometry_for_frame(8, 4, 16, 8).unwrap().bounds();
            assert_eq!(
                (b.left, b.top, b.width(), b.height()),
                (10.0, 20.0, 8.0, 16.0)
            );
        }
        session
            .apply(Command::TransformItemForFrame {
                scene,
                item,
                action: TransformAction::Center,
                frame_width: 16,
                frame_height: 8,
            })
            .unwrap();
        assert_eq!(
            item_transform(&session, scene, item)
                .geometry_for_frame(8, 4, 16, 8)
                .unwrap()
                .bounds(),
            ItemBounds {
                left: 12.0,
                top: 0.0,
                right: 20.0,
                bottom: 16.0
            }
        );
        session
            .apply(Command::TransformItemForFrame {
                scene,
                item,
                action: TransformAction::Reset,
                frame_width: 16,
                frame_height: 8,
            })
            .unwrap();
        assert_eq!(
            item_transform(&session, scene, item),
            Transform {
                opacity: 0.4,
                ..Transform::full(8, 4)
            }
        );
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: Transform {
                    crop: Crop {
                        left: 7,
                        ..Default::default()
                    },
                    ..Transform::full(8, 4)
                },
            })
            .unwrap();
        session
            .apply(Command::TransformItemForFrame {
                scene,
                item,
                action: TransformAction::Reset,
                frame_width: 4,
                frame_height: 4,
            })
            .unwrap();
        assert_eq!(item_transform(&session, scene, item), Transform::full(8, 4));
    }
    #[test]
    fn runtime_actions_handle_tiny_and_huge_derived_sizes_without_temporary_authored_extents() {
        let (mut tiny, scene, item) = capture_session((8192, 1), (16, 16));
        tiny.apply(Command::SetTransform {
            scene,
            item,
            transform: Transform {
                width: 0.001,
                ..Transform::full(8192, 1)
            },
        })
        .unwrap();
        tiny.apply(Command::TransformItemForFrame {
            scene,
            item,
            action: TransformAction::Center,
            frame_width: 1,
            frame_height: 1,
        })
        .unwrap();
        let b = item_transform(&tiny, scene, item)
            .geometry_for_frame(8192, 1, 1, 1)
            .unwrap()
            .bounds();
        assert!((b.left + b.right - 16.0).abs() < 0.000001);
        assert_eq!(b.top + b.bottom, 16.0);
        tiny.apply(Command::TransformItemForFrame {
            scene,
            item,
            action: TransformAction::FitToCanvas,
            frame_width: 1,
            frame_height: 1,
        })
        .unwrap();
        assert_eq!(
            (
                item_transform(&tiny, scene, item).width,
                item_transform(&tiny, scene, item).height
            ),
            (131_072.0, 16.0)
        );
        let (mut huge, scene, item) = capture_session((1, 1), (16, 16));
        huge.apply(Command::SetTransform {
            scene,
            item,
            transform: Transform {
                width: 33_554_432.0,
                ..Transform::full(1, 1)
            },
        })
        .unwrap();
        let before = huge.snapshot();
        assert!(
            huge.apply(Command::TransformItemForFrame {
                scene,
                item,
                action: TransformAction::Center,
                frame_width: 8192,
                frame_height: 1
            })
            .is_err()
        );
        assert!(Arc::ptr_eq(&before, &huge.snapshot()));
        huge.apply(Command::TransformItemForFrame {
            scene,
            item,
            action: TransformAction::FitToCanvas,
            frame_width: 8192,
            frame_height: 1,
        })
        .unwrap();
        let t = item_transform(&huge, scene, item);
        assert_eq!(t.width, 0.001953125);
        assert_eq!(t.height, 0.001953125);
        assert_eq!(
            t.geometry_for_frame(1, 1, 8192, 1)
                .unwrap()
                .bounds()
                .width(),
            16.0
        );
    }
    #[test]
    fn invalid_frame_context_wrong_source_and_locked_actions_preserve_canonical_state() {
        let (mut session, scene, item) = capture_session((8, 4), (16, 16));
        let before = session.snapshot();
        for (frame_width, frame_height) in [(0, 1), (1, 0), (8193, 1), (8192, 4097)] {
            assert!(
                session
                    .apply(Command::TransformItemForFrame {
                        scene,
                        item,
                        action: TransformAction::Center,
                        frame_width,
                        frame_height
                    })
                    .is_err()
            );
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
        session
            .apply(Command::SetLocked {
                scene,
                item,
                locked: true,
            })
            .unwrap();
        let locked = session.snapshot();
        assert_eq!(
            session.apply(Command::TransformItemForFrame {
                scene,
                item,
                action: TransformAction::Center,
                frame_width: 16,
                frame_height: 8
            }),
            Err(Error::Locked(item))
        );
        assert!(Arc::ptr_eq(&locked, &session.snapshot()));
        let mut color = Session::new();
        let scene = color.preview();
        let item = color
            .apply(Command::AddColorSource {
                scene,
                name: "Color".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        let before = color.snapshot();
        assert!(
            color
                .apply(Command::TransformItemForFrame {
                    scene,
                    item,
                    action: TransformAction::FitToCanvas,
                    frame_width: 16,
                    frame_height: 16
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &color.snapshot()));
    }
    #[test]
    fn runtime_native_size_uses_the_captured_basis_once_and_keeps_exact_crop_units() {
        let t = Transform {
            x: 3.0,
            y: 5.0,
            width: 100.0,
            height: 40.0,
            rotation_degrees: 90.0,
            crop: Crop {
                left: 1,
                right: 2,
                top: 1,
                bottom: 0,
            },
            ..Transform::full(10, 4)
        };
        let geometry = t.geometry_for_frame(10, 4, 20, 8).unwrap();
        assert_eq!(geometry.origin, [3.0, 5.0]);
        assert_eq!(geometry.axis_x, [0.0, 170.0]);
        assert_eq!(geometry.axis_y, [-70.0, 0.0]);
        assert_eq!(geometry.uv, [0.05, 0.125, 0.9, 1.0]);
        let reflected = Transform {
            flip_x: true,
            flip_y: true,
            ..t.clone()
        }
        .geometry_for_frame(10, 4, 20, 8)
        .unwrap();
        assert_eq!(reflected.axis_x, [0.0, -170.0]);
        assert_eq!(reflected.axis_y, [70.0, 0.0]);
        assert!(
            t.geometry_for_frame(10, 4, 3, 8).is_err(),
            "native crop cannot silently consume a smaller frame"
        );
        assert_eq!(
            (t.width, t.height, t.crop),
            (
                100.0,
                40.0,
                Crop {
                    left: 1,
                    right: 2,
                    top: 1,
                    bottom: 0
                }
            )
        );
    }
    #[test]
    fn derived_runtime_axes_can_cross_authored_extent_limits_without_mutation_or_clamp() {
        let tiny = Transform {
            width: 0.001,
            ..Transform::full(8192, 1)
        };
        let geometry = tiny.geometry_for_frame(8192, 1, 1, 1).unwrap();
        assert!(geometry.axis_x[0] > 0.0 && geometry.axis_x[0] < 0.000001);
        assert_eq!(geometry.axis_x[0], f64::from(tiny.width) / 8192.0);
        let large = Transform {
            width: 33_554_432.0,
            ..Transform::full(1, 1)
        };
        assert_eq!(
            large.geometry_for_frame(1, 1, 8192, 1).unwrap().axis_x,
            [274_877_906_944.0, 0.0]
        );
        for (bw, bh, fw, fh) in [
            (0, 1, 1, 1),
            (1, 0, 1, 1),
            (8193, 1, 1, 1),
            (1, 1, 0, 1),
            (1, 1, 1, 0),
            (1, 1, 8193, 1),
        ] {
            assert!(large.geometry_for_frame(bw, bh, fw, fh).is_err());
        }
        assert!(
            Transform {
                width: 0.0,
                ..large
            }
            .geometry_for_frame(1, 1, 8192, 1)
            .is_err(),
            "runtime evaluation does not bypass authored validation"
        );
    }
    #[test]
    fn quarter_turn_and_native_crop_have_independent_axes_and_uv() {
        let t = Transform {
            x: 12.0,
            y: 8.0,
            width: 200.0,
            height: 80.0,
            rotation_degrees: 90.0,
            flip_x: true,
            crop: Crop {
                left: 10,
                right: 20,
                top: 5,
                bottom: 5,
            },
            ..Transform::full(100, 40)
        };
        let g = t.geometry(100, 40).unwrap();
        assert_eq!(g.origin, [12.0, 8.0]);
        assert_eq!(g.axis_x, [0.0, -140.0]);
        assert_eq!(g.axis_y, [-60.0, 0.0]);
        assert_eq!(g.uv, [0.1, 0.125, 0.8, 0.875]);
        assert_eq!(
            g.bounds(),
            ItemBounds {
                left: -48.0,
                top: -132.0,
                right: 12.0,
                bottom: 8.0
            }
        );
    }
    #[test]
    fn arbitrary_rotation_bounds_match_independent_corners() {
        let t = Transform {
            x: 40.0,
            y: 30.0,
            width: 10.0,
            height: 20.0,
            rotation_degrees: 45.0,
            ..Transform::full(10, 20)
        };
        let bounds = t.geometry(10, 20).unwrap().bounds();
        let root = 2.0f64.sqrt();
        assert!((bounds.left - (40.0 - 10.0 * root)).abs() < 1e-10);
        assert!((bounds.right - (40.0 + 5.0 * root)).abs() < 1e-10);
        assert!((bounds.bottom - (30.0 + 15.0 * root)).abs() < 1e-10);
        assert_eq!(bounds.top, 30.0);
    }
    #[test]
    fn obs_rotate_and_flip_keep_the_old_bounding_top_left() {
        let initial = Transform {
            x: 25.0,
            y: 50.0,
            width: 200.0,
            height: 100.0,
            ..Transform::full(200, 100)
        };
        let mut t = initial.clone();
        t.apply_action(TransformAction::Rotate90Clockwise, 200, 100, 1920, 1080)
            .unwrap();
        assert_eq!((t.x, t.y, t.rotation_degrees), (125.0, 50.0, 90.0));
        assert_eq!(
            t.geometry(200, 100).unwrap().bounds(),
            ItemBounds {
                left: 25.0,
                top: 50.0,
                right: 125.0,
                bottom: 250.0
            }
        );
        t.apply_action(
            TransformAction::Rotate90CounterClockwise,
            200,
            100,
            1920,
            1080,
        )
        .unwrap();
        assert_eq!(t, initial);
        t.apply_action(TransformAction::FlipHorizontal, 200, 100, 1920, 1080)
            .unwrap();
        assert_eq!(t.x, 225.0);
        t.apply_action(TransformAction::FlipHorizontal, 200, 100, 1920, 1080)
            .unwrap();
        assert_eq!(t, initial);
    }
    #[test]
    fn fit_crop_and_center_use_visible_rotated_bounds() {
        let mut t = Transform {
            crop: Crop {
                left: 8191,
                ..Crop::default()
            },
            ..Transform::full(8192, 1)
        };
        t.apply_action(TransformAction::FitToCanvas, 8192, 1, 4096, 4096)
            .unwrap();
        assert_eq!(t.width, 33_554_432.0);
        assert_eq!(t.geometry(8192, 1).unwrap().bounds().width(), 4096.0);
        t.rotation_degrees = 90.0;
        t.apply_action(TransformAction::Center, 8192, 1, 1920, 1080)
            .unwrap();
        let b = t.geometry(8192, 1).unwrap().bounds();
        assert_eq!((b.left + b.right, b.top + b.bottom), (1920.0, 1080.0));
        t.apply_action(TransformAction::Reset, 8192, 1, 1920, 1080)
            .unwrap();
        assert_eq!(t, Transform::full(8192, 1));
    }
    #[test]
    fn invalid_values_and_overflowing_crop_are_rejected() {
        let mut t = Transform::full(20, 20);
        t.rotation_degrees = f32::NAN;
        assert!(t.geometry(20, 20).is_err());
        t.rotation_degrees = 0.0;
        t.crop = Crop {
            left: u32::MAX,
            right: u32::MAX,
            ..Crop::default()
        };
        assert!(t.geometry(20, 20).is_err());
        t.crop = Crop {
            left: 20,
            ..Crop::default()
        };
        assert!(t.geometry(20, 20).is_err());
    }
}
