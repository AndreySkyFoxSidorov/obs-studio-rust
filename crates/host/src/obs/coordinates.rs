//! Normalize supported OBS32 v2 items at the explicit import profile canvas.
//! Exact reference: libobs/obs-scene.c:350..439,1176..1207,1341..1355.
use super::{ImportVideo, ObsError, issue, known, object, vector};
use serde_json::{Map, Value};

#[derive(Clone, Copy)]
pub(super) enum Mode { Absolute, Relative }

impl Mode {
    pub(super) fn parse(root: &Map<String, Value>) -> Result<Self, ObsError> {
        match root.get("version") {
            None => Ok(Self::Absolute),
            Some(value) => match value.as_u64() {
                Some(1) => Ok(Self::Absolute),
                Some(2) => Ok(Self::Relative),
                _ => Err(issue("$.version", "unsupported_coordinates", "Only OBS collection coordinate versions 1 and 2 are supported.")),
            },
        }
    }

    pub(super) fn item(self, map: &Map<String, Value>, path: &str, video: ImportVideo,
        scene_source: bool, precise_group_position:bool) -> Result<((f64, f64), (f64, f64)), ObsError> {
        // OBS32 writes scale_ref even for absolute items. Accept its bounded
        // shape independently of whether relative scaling actually uses it.
        for key in ["pos_rel", "scale_rel", "scale_ref", "bounds_rel"] {
            if map.contains_key(key) { finite_vector(map, key, path)?; }
        }
        let absolute_position = finite_vector(map, "pos", path)?;
        let absolute_scale = finite_vector(map, "scale", path)?;
        if let Self::Absolute = self {
            return Ok((absolute_position, absolute_scale));
        }
        let width = video.width as f32;
        let height = video.height as f32;
        // The outer importer validates the profile before asset preparation.
        // Guard this earlier coordinate calculation against division by zero.
        if width <= 0.0 || height <= 0.0 {
            return Err(issue(path, "invalid_coordinates", "Relative coordinates require a positive profile canvas."));
        }
        let complete = ["pos_rel", "scale_rel", "scale_ref"].iter().all(|key| map.contains_key(*key));
        let (position, scale) = if complete {
            let position = finite_vector(map, "pos_rel", path)?;
            let scale = finite_vector(map, "scale_rel", path)?;
            let reference = finite_vector(map, "scale_ref", path)?;
            if reference.1 <= 0.0 {
                return Err(issue(&format!("{path}.scale_ref.y"), "invalid_coordinates", "Relative scale reference height must be positive."));
            }
            let factor = if scene_source { 1.0 } else { height / reference.1 as f32 };
            (position, ((scale.0 as f32 * factor) as f64, (scale.1 as f32 * factor) as f64))
        } else {
            // Exact original fallback: read absolute position, convert into
            // relative scene space, then use the ordinary absolute readback.
            let x = (2.0 * absolute_position.0 as f32 - width) / height;
            let y = 2.0 * absolute_position.1 as f32 / height - 1.0;
            (((x as f64), (y as f64)),
                ((absolute_scale.0 as f32) as f64, (absolute_scale.1 as f32) as f64))
        };
        let x = (position.0 as f32 * height + width) / 2.0;
        let y = (position.1 as f32 * height + height) / 2.0;
        // Group wrappers and every Group child retain the original float
        // precision. Ordinary root scene items use
        // floorf(v*2+.5)/2, including negative positions; round() is different.
        let position=if precise_group_position{(x as f64,y as f64)}else{
            (((x*2.0+0.5).floor()/2.0)as f64,((y*2.0+0.5).floor()/2.0)as f64)
        };
        if ![position.0, position.1, scale.0, scale.1].iter().all(|value| value.is_finite()) {
            return Err(issue(path, "invalid_coordinates", "Relative coordinate conversion exceeds the representable transform range."));
        }
        Ok((position, scale))
    }

    pub(super) fn bounds(self, map: &Map<String, Value>, path: &str) -> Result<(), ObsError> {
        // Bounds transforms remain outside the represented native subset.
        // Validate both stored forms so ignored stale nonzero bounds cannot
        // be carried into a supposedly faithful exported absolute collection.
        for key in ["bounds", "bounds_rel"] {
            if map.contains_key(key) && finite_vector(map, key, path)? != (0.0, 0.0) {
                return Err(issue(&format!("{path}.{key}"), "unsupported", "Nonzero bounds are outside the supported subset."));
            }
        }
        Ok(())
    }
}

fn finite_vector(map: &Map<String, Value>, key: &str, path: &str) -> Result<(f64, f64), ObsError> {
    let value = vector(map, key, (0.0, 0.0), path)?;
    if ![value.0, value.1].iter().all(|value| value.is_finite() && (*value as f32).is_finite()) {
        return Err(issue(&format!("{path}.{key}"), "invalid_coordinates", "Coordinate components must be finite and representable as OBS floats."));
    }
    Ok(value)
}

pub(super) fn migration_metadata(root: &Map<String, Value>) -> Result<(), ObsError> {
    if let Some(value) = root.get("migration_resolution") {
        let value = object(value, "$.migration_resolution")?;
        known(value, &["x", "y"], "$.migration_resolution")?;
        for key in ["x", "y"] {
            if value.get(key).is_none_or(|value| value.as_u64().is_none_or(|v| v == 0 || v > u64::from(u32::MAX))) {
                return Err(issue(&format!("$.migration_resolution.{key}"), "invalid_coordinates", "Migration resolution must contain positive 32-bit pixel dimensions."));
            }
        }
    }
    Ok(())
}

pub(super) fn absolute_export(item: &mut Map<String, Value>) {
    for key in ["pos_rel", "scale_rel", "scale_ref", "bounds_rel"] { item.remove(key); }
}
