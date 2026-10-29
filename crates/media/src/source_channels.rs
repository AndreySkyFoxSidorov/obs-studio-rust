//! Literal OBS process_audio channel controls, before the source filter chain.
//! Fixed stack state; no device calls, history, allocation or clipping.
use crate::{AudioFormat, MediaError};
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourceChannelSettings {
    pub force_mono: bool,
    pub balance: f32,
}
impl Default for SourceChannelSettings {
    fn default() -> Self {
        Self {
            force_mono: false,
            balance: 0.5,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct SourceChannelTransform {
    channels: usize,
    mono: bool,
    balance: Option<(f32, f32)>,
}
impl SourceChannelTransform {
    pub fn new(
        settings: SourceChannelSettings,
        original: Option<AudioFormat>,
        output_channels: u16,
    ) -> Result<Self, MediaError> {
        if !settings.balance.is_finite()
            || !(0.0..=1.0).contains(&settings.balance)
            || !(1..=8).contains(&output_channels)
        {
            return Err(MediaError::InvalidAudio);
        }
        let original_stereo = original
            .is_some_and(|format| format.channels() == 2 && matches!(format.channel_mask(), 0 | 3));
        let balance = if output_channels != 1
            && original_stereo
            && (settings.balance < 0.49 || settings.balance > 0.51)
        {
            // C computes the f32 subtraction first, multiplies by double M_PI/2,
            // converts the argument to f32 and then calls sinf.
            let left = (((1.0f32 - settings.balance) as f64) * (std::f64::consts::PI / 2.0)) as f32;
            let right = ((settings.balance as f64) * (std::f64::consts::PI / 2.0)) as f32;
            Some((left.sin(), right.sin()))
        } else {
            None
        };
        Ok(Self {
            channels: usize::from(output_channels),
            mono: output_channels != 1 && settings.force_mono,
            balance,
        })
    }
    pub fn is_bypassed(self) -> bool {
        !self.mono && self.balance.is_none()
    }
    fn transformed(self, input: &[f32]) -> Result<[f32; 8], MediaError> {
        if input.len() != self.channels || input.iter().any(|value| !value.is_finite()) {
            return Err(MediaError::InvalidAudio);
        }
        let mut result = [0.0; 8];
        result[..self.channels].copy_from_slice(input);
        if let Some((left, right)) = self.balance {
            result[0] *= left;
            result[1] *= right;
        }
        if self.mono {
            let mut sum = result[0];
            for value in &result[1..self.channels] {
                sum += *value;
            }
            let mean = sum * (1.0 / self.channels as f32);
            result[..self.channels].fill(mean);
        }
        if result[..self.channels]
            .iter()
            .any(|value| !value.is_finite())
        {
            return Err(MediaError::InvalidAudio);
        }
        Ok(result)
    }
    pub fn process_frame(self, frame: &mut [f32]) -> Result<(), MediaError> {
        let result = self.transformed(frame)?;
        if !self.is_bypassed() {
            frame.copy_from_slice(&result[..self.channels]);
        }
        Ok(())
    }
    pub fn process_interleaved(self, samples: &mut [f32]) -> Result<(), MediaError> {
        if samples.is_empty()
            || !samples.len().is_multiple_of(self.channels)
            || samples.len() / self.channels > 4096
        {
            return Err(MediaError::InvalidAudio);
        }
        // Validate every arithmetic result first. Even a late overflow must leave
        // this in-place public buffer unchanged, without allocating a shadow Vec.
        for frame in samples.chunks_exact(self.channels) {
            self.transformed(frame)?;
        }
        if self.is_bypassed() {
            return Ok(());
        }
        for frame in samples.chunks_exact_mut(self.channels) {
            self.process_frame(frame)?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn transform(
        mono: bool,
        balance: f32,
        original: u16,
        mask: u32,
        output: u16,
    ) -> SourceChannelTransform {
        SourceChannelTransform::new(
            SourceChannelSettings {
                force_mono: mono,
                balance,
            },
            Some(AudioFormat::new(48000, original, mask).unwrap()),
            output,
        )
        .unwrap()
    }
    #[test]
    fn literal_balance_deadzone_endpoints_and_sine_law() {
        for balance in [0.49, 0.5, 0.51] {
            let kernel = transform(false, balance, 2, 3, 2);
            assert!(kernel.is_bypassed());
            let mut frame = [1.0, -0.0];
            kernel.process_frame(&mut frame).unwrap();
            assert_eq!(frame[1].to_bits(), (-0.0f32).to_bits());
            assert_eq!(frame[0], 1.0);
        }
        for (balance, expected) in [
            (0.0, [1.0, 0.0]),
            (1.0, [0.0, 1.0]),
            (0.25, [0.9238795, 0.38268343]),
        ] {
            let mut frame = [1.0; 2];
            transform(false, balance, 2, 3, 2)
                .process_frame(&mut frame)
                .unwrap();
            for (value, expected) in frame.into_iter().zip(expected) {
                assert!((value - expected).abs() < 0.000001);
            }
        }
    }
    #[test]
    fn original_layout_gates_balance_and_mono_averages_all_output_channels_after_balance() {
        let mut native_mono = [1.0, 1.0];
        transform(false, 0.0, 1, 4, 2)
            .process_frame(&mut native_mono)
            .unwrap();
        assert_eq!(native_mono, [1.0, 1.0]);
        let mut projected_mono = [0.75];
        assert!(transform(true, 0.0, 2, 3, 1).is_bypassed());
        transform(true, 0.0, 2, 3, 1)
            .process_frame(&mut projected_mono)
            .unwrap();
        assert_eq!(projected_mono, [0.75]);
        let mut frame = [1.0, -1.0];
        transform(true, 0.25, 2, 3, 2)
            .process_frame(&mut frame)
            .unwrap();
        assert!((frame[0] - 0.27059805).abs() < 0.000001);
        assert_eq!(frame[0], frame[1]);
        let mut surround = [8.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        transform(true, 0.5, 8, 0x63f, 8)
            .process_frame(&mut surround)
            .unwrap();
        assert_eq!(surround, [1.0; 8]);
    }
    #[test]
    fn zero_heap_copy_state_bypass_and_invalid_late_frame_are_transactional() {
        assert!(!std::mem::needs_drop::<SourceChannelTransform>());
        let kernel = transform(true, 0.5, 2, 3, 2);
        let mut samples = [1.0, -1.0, 0.5, 0.25];
        let pointer = samples.as_ptr();
        kernel.process_interleaved(&mut samples).unwrap();
        assert_eq!(pointer, samples.as_ptr());
        assert_eq!(samples, [0.0, 0.0, 0.375, 0.375]);
        let mut overflow = [0.25, 0.5, f32::MAX, f32::MAX];
        let old = overflow;
        assert!(kernel.process_interleaved(&mut overflow).is_err());
        assert_eq!(overflow, old);
        for balance in [f32::NAN, -0.1, 1.1] {
            assert!(
                SourceChannelTransform::new(
                    SourceChannelSettings {
                        force_mono: false,
                        balance
                    },
                    Some(AudioFormat::new(48000, 2, 3).unwrap()),
                    2
                )
                .is_err()
            );
        }
        let mut invalid = [0.25, f32::INFINITY];
        assert!(
            transform(false, 0.5, 2, 3, 2)
                .process_frame(&mut invalid)
                .is_err()
        );
    }
}
