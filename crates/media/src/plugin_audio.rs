//! Bounded stateless Rust ports of Norihiro Kamae's audio filter algorithms.
//!
//! Reference: obs-audio-pan-filter/src/stereo-pan-filter.c at
//! 7a4594a27aeee51718b9bb0b3e6e12f75109d83f (GPL-2.0-or-later).
//! Pan processes the first two project-format channels; any further channels
//! pass unchanged. The separate Mute plugin is not implemented here: its packet
//! discard/readiness semantics cannot be represented by zeroing mixed samples.
use crate::MediaError;
use std::mem::size_of;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StereoPanSettings {
    pan_left: f32,
    pan_right: f32,
    gain_left_db: f32,
    gain_right_db: f32,
    invert_left: bool,
    invert_right: bool,
}
impl Default for StereoPanSettings {
    fn default() -> Self {
        Self {
            pan_left: -100.0,
            pan_right: 100.0,
            gain_left_db: 0.0,
            gain_right_db: 0.0,
            invert_left: false,
            invert_right: false,
        }
    }
}
impl StereoPanSettings {
    pub fn new(
        pan_left: f32,
        pan_right: f32,
        gain_left_db: f32,
        gain_right_db: f32,
        invert_left: bool,
        invert_right: bool,
    ) -> Result<Self, MediaError> {
        if ![pan_left, pan_right]
            .iter()
            .all(|value| value.is_finite() && (-100.0..=100.0).contains(value))
            || ![gain_left_db, gain_right_db]
                .iter()
                .all(|value| value.is_finite() && (-80.0..=30.0).contains(value))
        {
            return Err(MediaError::InvalidAudio);
        }
        Ok(Self {
            pan_left,
            pan_right,
            gain_left_db,
            gain_right_db,
            invert_left,
            invert_right,
        })
    }
}

/// Precomputed source matrix. No state or allocation is needed per frame.
#[derive(Clone, Copy, Debug)]
pub struct StereoPan {
    // Rows are output L/R, columns are input L/R.
    matrix: [[f32; 2]; 2],
}
impl StereoPan {
    pub fn new(settings: StereoPanSettings) -> Self {
        let left = coefficients(
            settings.pan_left,
            settings.gain_left_db,
            settings.invert_left,
        );
        let right = coefficients(
            settings.pan_right,
            settings.gain_right_db,
            settings.invert_right,
        );
        Self {
            matrix: [[left.0, right.0], [left.1, right.1]],
        }
    }
    /// Reject invalid/overflowing values before changing even the first channel.
    pub fn process_frame(&self, frame: &mut [f32]) -> Result<(), MediaError> {
        let (left, right) = self.evaluate(frame)?;
        frame[0] = left;
        if frame.len() > 1 {
            frame[1] = right;
        }
        Ok(())
    }
    fn evaluate(&self, frame: &[f32]) -> Result<(f32, f32), MediaError> {
        if frame.is_empty() || frame.len() > 8 || frame.iter().any(|value| !value.is_finite()) {
            return Err(MediaError::InvalidAudio);
        }
        let left = frame[0];
        let right = frame.get(1).copied().unwrap_or(0.0);
        // Keep the source's f32 multiplies and addition, without fused mul_add.
        let output_left = self.matrix[0][0] * left + self.matrix[0][1] * right;
        let output_right = self.matrix[1][0] * left + self.matrix[1][1] * right;
        if !output_left.is_finite() || (frame.len() > 1 && !output_right.is_finite()) {
            return Err(MediaError::InvalidAudio);
        }
        Ok((output_left, output_right))
    }
}
fn coefficients(pan: f32, gain_db: f32, inverted: bool) -> (f32, f32) {
    // libobs db_to_mul takes/returns float before the plugin's double math.
    let mut gain = f64::from(10.0f32.powf(gain_db / 20.0)) * std::f64::consts::FRAC_1_SQRT_2;
    if inverted {
        gain = -gain;
    }
    let angle = f64::from(pan) * (std::f64::consts::FRAC_PI_4 / 100.0);
    (
        (gain * (angle.cos() - angle.sin())) as f32,
        (gain * (angle.cos() + angle.sin())) as f32,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginAudioLimits {
    pub max_frames: usize,
    pub max_bytes: usize,
    pub max_sample_visits: usize,
}
impl Default for PluginAudioLimits {
    fn default() -> Self {
        Self {
            max_frames: 4096,
            max_bytes: 128 * 1024,
            max_sample_visits: 3 * 4096 * 8,
        }
    }
}
fn validate_block(
    samples: &[f32],
    channels: u16,
    limits: PluginAudioLimits,
) -> Result<usize, MediaError> {
    let channels = usize::from(channels);
    if channels == 0
        || channels > 8
        || samples.is_empty()
        || !samples.len().is_multiple_of(channels)
    {
        return Err(MediaError::InvalidAudio);
    }
    let frames = samples.len() / channels;
    if frames > 4096
        || frames > limits.max_frames
        || samples
            .len()
            .checked_mul(size_of::<f32>())
            .ok_or(MediaError::Overflow)?
            > limits.max_bytes
        || samples.len().checked_mul(3).ok_or(MediaError::Overflow)? > limits.max_sample_visits
    {
        return Err(MediaError::BudgetExceeded);
    }
    Ok(channels)
}

/// In-place interleaved PCM, after rate/layout conversion. A complete dry run
/// rejects malformed or overflowing input before any sample is changed.
pub fn stereo_pan_interleaved(
    samples: &mut [f32],
    channels: u16,
    settings: StereoPanSettings,
    limits: PluginAudioLimits,
) -> Result<(), MediaError> {
    let channels = validate_block(samples, channels, limits)?;
    let kernel = StereoPan::new(settings);
    for frame in samples.chunks_exact(channels) {
        kernel.evaluate(frame)?;
    }
    for frame in samples.chunks_exact_mut(channels) {
        kernel.process_frame(frame)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn close(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 0.000002,
                "{actual} != {expected}"
            );
        }
    }
    #[test]
    fn independent_input_pans_center_sum_and_inversion_match_literal_sample_oracles() {
        let limits = PluginAudioLimits::default();
        let mut unchanged = [0.25, -0.75];
        stereo_pan_interleaved(&mut unchanged, 2, StereoPanSettings::default(), limits).unwrap();
        close(&unchanged, &[0.25, -0.75]);
        let mut centered = [0.25, 0.75, 1.0, -1.0];
        stereo_pan_interleaved(
            &mut centered,
            2,
            StereoPanSettings::new(0.0, 0.0, 0.0, 0.0, false, false).unwrap(),
            limits,
        )
        .unwrap();
        close(&centered, &[0.70710677, 0.70710677, 0.0, 0.0]);
        let mut swapped = [0.25, -0.75];
        stereo_pan_interleaved(
            &mut swapped,
            2,
            StereoPanSettings::new(100.0, -100.0, 0.0, 0.0, true, false).unwrap(),
            limits,
        )
        .unwrap();
        close(&swapped, &[-0.75, -0.25]);
        let mut gained = [0.5, 0.25];
        stereo_pan_interleaved(
            &mut gained,
            2,
            StereoPanSettings::new(-100.0, 100.0, 20.0, -20.0, false, true).unwrap(),
            limits,
        )
        .unwrap();
        close(&gained, &[5.0, -0.025]);
    }
    #[test]
    fn mono_uses_zero_right_input_and_multichannel_tail_passes_bit_exactly() {
        let kernel =
            StereoPan::new(StereoPanSettings::new(100.0, -100.0, 0.0, 0.0, false, false).unwrap());
        let mut mono = [0.5];
        kernel.process_frame(&mut mono).unwrap();
        close(&mono, &[0.0]);
        let mut surround = [0.25, 0.75, -0.0, 0.125, -0.25, 0.5, -0.75, 1.0];
        let tail: Vec<_> = surround[2..]
            .iter()
            .map(|value| f32::to_bits(*value))
            .collect();
        kernel.process_frame(&mut surround).unwrap();
        close(&surround[..2], &[0.75, 0.25]);
        assert_eq!(
            surround[2..]
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            tail
        );
    }
    #[test]
    fn invalid_controls_shape_overflow_and_work_budgets_reject_before_block_mutation() {
        for pan in [f32::NAN, f32::INFINITY, -100.001, 100.001] {
            assert!(StereoPanSettings::new(pan, 0.0, 0.0, 0.0, false, false).is_err());
        }
        for db in [f32::NAN, f32::NEG_INFINITY, -80.001, 30.001] {
            assert!(StereoPanSettings::new(0.0, 0.0, 0.0, db, false, false).is_err());
        }
        let settings = StereoPanSettings::new(0.0, 0.0, 30.0, 30.0, false, false).unwrap();
        let original = [0.25, 0.75, f32::MAX, f32::MAX];
        let mut samples = original;
        assert_eq!(
            stereo_pan_interleaved(&mut samples, 2, settings, PluginAudioLimits::default()),
            Err(MediaError::InvalidAudio)
        );
        assert_eq!(samples, original);
        for limits in [
            PluginAudioLimits {
                max_frames: 1,
                ..Default::default()
            },
            PluginAudioLimits {
                max_bytes: 15,
                ..Default::default()
            },
            PluginAudioLimits {
                max_sample_visits: 11,
                ..Default::default()
            },
        ] {
            let mut samples = [0.25; 4];
            assert_eq!(
                stereo_pan_interleaved(&mut samples, 2, settings, limits),
                Err(MediaError::BudgetExceeded)
            );
            assert_eq!(samples, [0.25; 4]);
        }
        for channels in [0, 3, 9] {
            let mut samples = [0.25; 4];
            assert_eq!(
                stereo_pan_interleaved(
                    &mut samples,
                    channels,
                    settings,
                    PluginAudioLimits::default()
                ),
                Err(MediaError::InvalidAudio)
            );
            assert_eq!(samples, [0.25; 4]);
        }
    }
}
