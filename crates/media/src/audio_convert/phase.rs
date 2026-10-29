//! Pinned Rubato's integer output_delay omits a fractional starting phase.
//! Correct it with a bounded full-band windowed-sinc fractional delay, rather
//! than relabeling advanced PCM as an integer-aligned output sample grid.
use super::{MediaError, OVERSAMPLING, SINC_LEN, zeros};
pub(super) const CENTER: usize = 64;
pub(super) const TAPS: usize = 129;
pub(super) struct PhaseAlignment {
    coefficients: Box<[f64]>,
    history: Box<[f64]>,
    head: usize,
    channels: usize,
}
pub(super) fn correction(
    input_rate: u32,
    output_rate: u32,
    integer_delay: usize,
) -> Result<f64, MediaError> {
    // In Async: initial index=-(N-1), advance BEFORE sampling, and reversed
    // sinc table row p samples a center shifted by +1/oversampling. Therefore
    // true delay = ratio*(N/2 - 1/oversampling)-1 output frames.
    let denominator = u128::from(input_rate) * OVERSAMPLING as u128;
    let rounded = (integer_delay as u128 + 1)
        .checked_mul(denominator)
        .ok_or(MediaError::Overflow)?;
    let exact = u128::from(output_rate)
        .checked_mul((SINC_LEN / 2 * OVERSAMPLING - 1) as u128)
        .ok_or(MediaError::Overflow)?;
    let numerator = rounded.checked_sub(exact).ok_or(MediaError::InvalidAudio)?;
    let correction = numerator as f64 / denominator as f64;
    if !correction.is_finite() || !(0.0..=2.0).contains(&correction) {
        return Err(MediaError::InvalidAudio);
    }
    Ok(correction)
}
impl PhaseAlignment {
    pub(super) fn new(
        input_rate: u32,
        output_rate: u32,
        integer_delay: usize,
        channels: usize,
    ) -> Result<Self, MediaError> {
        let delta = correction(input_rate, output_rate, integer_delay)?;
        let mut coefficients = zeros(TAPS)?;
        let mut sum = 0.0;
        for (index, coefficient) in coefficients.iter_mut().enumerate() {
            let offset = index as f64 - CENTER as f64 - delta;
            let sinc = if offset == 0.0 {
                1.0
            } else {
                (std::f64::consts::PI * offset).sin() / (std::f64::consts::PI * offset)
            };
            let angle = std::f64::consts::TAU * index as f64 / (TAPS - 1) as f64;
            let window = 0.35875 - 0.48829 * angle.cos() + 0.14128 * (2.0 * angle).cos()
                - 0.01168 * (3.0 * angle).cos();
            *coefficient = sinc * window * window;
            sum += *coefficient;
        }
        if !sum.is_finite() || sum.abs() < 1e-12 {
            return Err(MediaError::InvalidAudio);
        }
        for coefficient in coefficients.iter_mut() {
            *coefficient /= sum;
        }
        Ok(Self {
            coefficients,
            history: zeros(channels.checked_mul(TAPS).ok_or(MediaError::Overflow)?)?,
            head: 0,
            channels,
        })
    }
    pub(super) fn process(&mut self, samples: &mut [Vec<f64>]) {
        for frame in 0..samples[0].len() {
            for (channel, output) in samples.iter_mut().enumerate().take(self.channels) {
                let history = &mut self.history[channel * TAPS..(channel + 1) * TAPS];
                history[self.head] = output[frame];
                let mut sum = 0.0;
                for (index, coefficient) in self.coefficients.iter().enumerate() {
                    sum += coefficient * history[(self.head + TAPS - index) % TAPS];
                }
                output[frame] = sum;
            }
            self.head = (self.head + 1) % TAPS;
        }
    }
    pub(super) fn reset(&mut self) {
        self.history.fill(0.0);
        self.head = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pinned_starting_phase_matches_the_independent_16k_to_44100_failure() {
        assert_eq!(correction(16000, 44100, 352).unwrap(), 0.221533203125);
        assert_eq!(correction(8000, 48000, 768).unwrap(), 1.046875);
    }
}
