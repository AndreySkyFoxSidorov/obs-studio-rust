// SPDX-License-Identifier: GPL-2.0-or-later
//! Pure Rust translation of OBS Gain and Invert Polarity at OBS HEAD
//! 72e86655715d9a25d69ce446aa4dd44ba58e10d2. See the source SHA256/license
//! record in doc/migration/obs-audio-filters.md and the supplied OBS COPYING.
//! Interleaved layout adaptation, with source f32 arithmetic and no clipping.
use crate::{MediaError, PluginAudioLimits};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObsGain {
    db: f32,
    multiplier: f32,
}
impl ObsGain {
    pub fn new(db: f32) -> Result<Self, MediaError> {
        if !db.is_finite() || !(-30.0..=30.0).contains(&db) {
            return Err(MediaError::InvalidAudio);
        }
        Ok(Self {
            db,
            multiplier: 10.0f32.powf(db / 20.0),
        })
    }
    pub fn db(&self) -> f32 {
        self.db
    }
    pub fn update(&mut self, db: f32) -> Result<(), MediaError> {
        *self = Self::new(db)?;
        Ok(())
    }
    pub fn process_interleaved(
        &self,
        samples: &mut [f32],
        channels: u16,
        limits: PluginAudioLimits,
    ) -> Result<(), MediaError> {
        scale(samples, channels, self.multiplier, limits)
    }
    pub fn process_frame(&self, frame: &mut [f32]) -> Result<(), MediaError> {
        let channels = u16::try_from(frame.len()).map_err(|_| MediaError::InvalidAudio)?;
        self.process_interleaved(frame, channels, PluginAudioLimits::default())
    }
}

pub fn invert_polarity_interleaved(
    samples: &mut [f32],
    channels: u16,
    limits: PluginAudioLimits,
) -> Result<(), MediaError> {
    scale(samples, channels, -1.0, limits)
}

fn scale(
    samples: &mut [f32],
    channels: u16,
    multiplier: f32,
    limits: PluginAudioLimits,
) -> Result<(), MediaError> {
    let channels = usize::from(channels);
    if !(1..=8).contains(&channels) || !samples.len().is_multiple_of(channels) {
        return Err(MediaError::InvalidAudio);
    }
    let frames = samples.len() / channels;
    if frames > 4096
        || frames > limits.max_frames
        || samples
            .len()
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or(MediaError::Overflow)?
            > limits.max_bytes
        || samples.len().checked_mul(2).ok_or(MediaError::Overflow)? > limits.max_sample_visits
    {
        return Err(MediaError::BudgetExceeded);
    }
    // Validate the complete block before writing even its first sample. OBS
    // accepts float over-unity; only nonrepresentable/invalid data is rejected.
    if samples
        .iter()
        .any(|value| !value.is_finite() || !(*value * multiplier).is_finite())
    {
        return Err(MediaError::InvalidAudio);
    }
    for value in samples {
        *value *= multiplier;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_gain_unity_boost_attenuation_and_signed_polarity() {
        let mut input = [0.25, -0.25, 0.0, -0.0];
        ObsGain::new(20.0)
            .unwrap()
            .process_interleaved(&mut input, 2, PluginAudioLimits::default())
            .unwrap();
        assert_eq!(input, [2.5, -2.5, 0.0, -0.0]);
        ObsGain::new(-20.0)
            .unwrap()
            .process_interleaved(&mut input, 2, PluginAudioLimits::default())
            .unwrap();
        assert_eq!(input, [0.25, -0.25, 0.0, -0.0]);
        let original = input.map(f32::to_bits);
        ObsGain::new(0.0)
            .unwrap()
            .process_interleaved(&mut input, 2, PluginAudioLimits::default())
            .unwrap();
        assert_eq!(input.map(f32::to_bits), original);
        invert_polarity_interleaved(&mut input, 2, PluginAudioLimits::default()).unwrap();
        assert_eq!(
            input.map(f32::to_bits),
            [(-0.25f32).to_bits(), 0.25f32.to_bits(), 0x8000_0000, 0]
        );
        invert_polarity_interleaved(&mut input, 2, PluginAudioLimits::default()).unwrap();
        assert_eq!(input.map(f32::to_bits), original);
    }
    #[test]
    fn arbitrary_partition_and_channel_counts_are_bit_exact() {
        for channels in 1..=8 {
            let input: Vec<_> = (0..97 * channels)
                .map(|i| ((i % 29) as f32 - 14.0) / 23.0)
                .collect();
            let gain = ObsGain::new(6.0).unwrap();
            let mut whole = input.clone();
            gain.process_interleaved(&mut whole, channels as u16, PluginAudioLimits::default())
                .unwrap();
            invert_polarity_interleaved(&mut whole, channels as u16, PluginAudioLimits::default())
                .unwrap();
            let mut split = input;
            for chunk in split.chunks_mut(channels * 7) {
                gain.process_interleaved(chunk, channels as u16, PluginAudioLimits::default())
                    .unwrap();
                invert_polarity_interleaved(chunk, channels as u16, PluginAudioLimits::default())
                    .unwrap();
            }
            assert_eq!(whole, split);
            assert!((whole[0] - 1.2145075).abs() < 0.000001);
        }
    }
    #[test]
    fn invalid_late_sample_and_budgets_leave_every_sample_unchanged() {
        for value in [f32::NAN, f32::INFINITY, -30.1, 30.1] {
            assert!(ObsGain::new(value).is_err());
        }
        let mut gain = ObsGain::new(20.0).unwrap();
        assert!(gain.update(f32::NAN).is_err());
        assert_eq!(gain.db(), 20.0);
        let mut late = [0.25, f32::MAX];
        let before = late;
        assert_eq!(
            gain.process_interleaved(&mut late, 1, PluginAudioLimits::default()),
            Err(MediaError::InvalidAudio)
        );
        assert_eq!(late, before);
        let mut samples = [0.25, -0.25];
        for limits in [
            PluginAudioLimits {
                max_frames: 0,
                ..Default::default()
            },
            PluginAudioLimits {
                max_bytes: 7,
                ..Default::default()
            },
            PluginAudioLimits {
                max_sample_visits: 3,
                ..Default::default()
            },
        ] {
            assert_eq!(
                gain.process_interleaved(&mut samples, 2, limits),
                Err(MediaError::BudgetExceeded)
            );
            assert_eq!(samples, [0.25, -0.25]);
        }
        for channels in [0, 3, 9] {
            assert_eq!(
                invert_polarity_interleaved(&mut samples, channels, PluginAudioLimits::default()),
                Err(MediaError::InvalidAudio)
            );
        }
        let mut bad = [0.5, f32::INFINITY];
        assert_eq!(
            invert_polarity_interleaved(&mut bad, 1, PluginAudioLimits::default()),
            Err(MediaError::InvalidAudio)
        );
        assert_eq!(bad[0], 0.5);
    }
}
