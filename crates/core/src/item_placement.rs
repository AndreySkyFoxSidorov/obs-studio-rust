//! Authored item placement, including window sources without an observed size.
use crate::{DeferredWindowTransform, Error, ItemGeometry, Transform};
use serde::{Deserialize, Serialize};

/// A resolved transform stores extents in canvas pixels; an unresolved window
/// stores OBS's signed source-pixel scale until a real frame supplies a basis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum ItemPlacement {
    Resolved(Transform),
    DeferredWindow(DeferredWindowTransform),
}
impl ItemPlacement {
    /// Evaluate against the raw frame and the actual output of its filter chain.
    pub fn geometry_for_frame(&self, basis: (u32, u32), raw: (u32, u32), filtered: (u32, u32)) -> Result<ItemGeometry, Error> {
        match self {
            Self::Resolved(transform) => transform.geometry_for_frame(basis.0, basis.1, filtered.0, filtered.1),
            Self::DeferredWindow(transform) => transform.geometry_for_frame(raw, filtered),
        }
    }
    /// A UI operation requiring extents must supply observed window dimensions;
    /// it must not infer them from the scale or a presentation placeholder.
    pub fn materialize(&self, raw: (u32, u32)) -> Result<Transform, Error> {
        match self {
            Self::Resolved(transform) => Ok(transform.clone()),
            Self::DeferredWindow(transform) => transform.materialize(raw.0, raw.1),
        }
    }
}
