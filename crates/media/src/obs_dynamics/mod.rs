// SPDX-License-Identifier: GPL-2.0-or-later
//! Source translation: OBS obs-filters limiter/compressor/expander at
//! 72e86655715d9a25d69ce446aa4dd44ba58e10d2. Callback identity is part of the
//! linked detector's contract. See doc/migration/obs-dynamics.md and OBS COPYING.
mod expander;
mod linked;
mod noise_gate;
mod upward;
pub use upward::{ObsUpwardCompressor, ObsUpwardParameters};
pub use noise_gate::{ObsNoiseGate, ObsNoiseGateParameters};
pub use expander::{ObsDetector, ObsExpander, ObsExpanderParameters};
pub use linked::{ObsCompressor, ObsCompressorParameters, ObsLimiter, ObsLimiterParameters};
use std::fmt;
pub const MAX_DYNAMICS_FRAMES: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObsDynamicsError {
    InvalidParameter(&'static str),
    InvalidFormat,
    InvalidBlock,
    BudgetExceeded,
    InvalidSample(usize),
    NonRepresentable,
}
impl fmt::Display for ObsDynamicsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ObsDynamicsError {}
#[derive(Clone, Copy, Debug)]
pub struct DynamicsLimits {
    pub max_frames: usize,
    pub max_bytes: usize,
    pub max_sample_visits: usize,
}
impl Default for DynamicsLimits {
    fn default() -> Self {
        Self {
            max_frames: MAX_DYNAMICS_FRAMES,
            max_bytes: MAX_DYNAMICS_FRAMES * 9 * 4,
            max_sample_visits: MAX_DYNAMICS_FRAMES * 8 * 24,
        }
    }
}
fn format(rate: u32, channels: u16) -> Result<usize, ObsDynamicsError> {
    if !(8000..=192000).contains(&rate) || !(1..=8).contains(&channels) {
        return Err(ObsDynamicsError::InvalidFormat);
    }
    Ok(usize::from(channels))
}
fn range(name: &'static str, value: f32, min: f32, max: f32) -> Result<(), ObsDynamicsError> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(ObsDynamicsError::InvalidParameter(name))
    }
}
fn time(name: &'static str, value: u32, max: u32) -> Result<(), ObsDynamicsError> {
    if (1..=max).contains(&value) {
        Ok(())
    } else {
        Err(ObsDynamicsError::InvalidParameter(name))
    }
}
fn block(
    samples: &[f32],
    channels: usize,
    limits: DynamicsLimits,
    scratch: usize,
    visits: usize,
) -> Result<usize, ObsDynamicsError> {
    if !samples.len().is_multiple_of(channels) {
        return Err(ObsDynamicsError::InvalidBlock);
    }
    let frames = samples.len() / channels;
    let bytes = samples
        .len()
        .checked_add(scratch)
        .and_then(|v| v.checked_mul(4))
        .ok_or(ObsDynamicsError::BudgetExceeded)?;
    if frames > MAX_DYNAMICS_FRAMES
        || frames > limits.max_frames
        || bytes > limits.max_bytes
        || samples
            .len()
            .checked_mul(visits)
            .is_none_or(|v| v > limits.max_sample_visits)
    {
        return Err(ObsDynamicsError::BudgetExceeded);
    }
    if let Some(i) = samples.iter().position(|v| !v.is_finite()) {
        return Err(ObsDynamicsError::InvalidSample(i));
    }
    Ok(frames)
}
fn mul_to_db(value: f32) -> f32 {
    if value == 0. {
        f32::NEG_INFINITY
    } else {
        20.0f32 * value.log10()
    }
}
fn db_to_mul(value: f32) -> f32 {
    if value.is_finite() {
        10.0f32.powf(value / 20.0f32)
    } else {
        0.0
    }
}
#[cfg(test)]
mod tests;
