//! OBS window-item geometry before the selected window has a known size.
//!
//! The OBS scale is relative to source pixels.  Keeping it separately avoids
//! inventing a capture size when a scene is imported with the window closed.
use crate::{Crop, Error, ItemGeometry, Transform};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Data")]
pub struct DeferredWindowTransform {
    x: f32,
    y: f32,
    scale_x: f64,
    scale_y: f64,
    rotation_degrees: f32,
    crop: Crop,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    x: f32,
    y: f32,
    scale_x: f64,
    scale_y: f64,
    rotation_degrees: f32,
    crop: Crop,
}

impl TryFrom<Data> for DeferredWindowTransform {
    type Error = Error;
    fn try_from(value: Data) -> Result<Self, Error> {
        Self::new(value.x, value.y, value.scale_x, value.scale_y, value.rotation_degrees, value.crop)
    }
}

impl DeferredWindowTransform {
    pub fn new(x: f32, y: f32, scale_x: f64, scale_y: f64, rotation_degrees: f32, crop: Crop) -> Result<Self, Error> {
        if !x.is_finite() || !y.is_finite() || x.abs() > 67_108_864.0 || y.abs() > 67_108_864.0
            || !scale_x.is_finite() || !scale_y.is_finite()
            || scale_x == 0.0 || scale_y == 0.0 || !rotation_degrees.is_finite()
            || rotation_degrees.abs() > 360_000.0
            || u64::from(crop.left) + u64::from(crop.right) >= 8192
            || u64::from(crop.top) + u64::from(crop.bottom) >= 8192 {
            return Err(Error::Invalid("Invalid deferred window transform"));
        }
        Ok(Self { x, y, scale_x, scale_y, rotation_degrees, crop })
    }

    pub fn scale(&self) -> (f64, f64) { (self.scale_x, self.scale_y) }
    pub fn position(&self) -> (f32, f32) { (self.x, self.y) }
    pub fn rotation_degrees(&self) -> f32 { self.rotation_degrees }
    pub fn crop(&self) -> Crop { self.crop }

    /// Move an unresolved item without assuming a native window size. This is
    /// also needed when a Group changes its local coordinate origin.
    pub fn translated(&self, dx: f32, dy: f32) -> Result<Self, Error> {
        Self::new(self.x + dx, self.y + dy, self.scale_x, self.scale_y,
            self.rotation_degrees, self.crop)
    }

    /// Resolve only against an observed source size. Geometry validation also
    /// rejects a crop larger than the real frame instead of truncating it.
    fn transform_for_raw_size(&self, width: u32, height: u32) -> Transform {
        Transform {
            x: self.x,
            y: self.y,
            width: (f64::from(width) * self.scale_x.abs()) as f32,
            height: (f64::from(height) * self.scale_y.abs()) as f32,
            opacity: 1.0,
            rotation_degrees: self.rotation_degrees,
            flip_x: self.scale_x.is_sign_negative(),
            flip_y: self.scale_y.is_sign_negative(),
            crop: self.crop,
        }
    }

    pub fn materialize(&self, width: u32, height: u32) -> Result<Transform, Error> {
        let transform = self.transform_for_raw_size(width, height);
        transform.geometry(width, height)?;
        Ok(transform)
    }

    /// Apply the saved scale to the observed raw size while sampling the
    /// filtered output. This follows the same raw-basis rule as live items.
    pub fn geometry_for_frame(&self, raw: (u32, u32), filtered: (u32, u32)) -> Result<ItemGeometry, Error> {
        let transform = self.transform_for_raw_size(raw.0, raw.1);
        transform.geometry_for_frame(raw.0, raw.1, filtered.0, filtered.1)
    }
}
