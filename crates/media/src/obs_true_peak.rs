//! Streaming approximation of OBS's five-times true-peak meter.
//!
//! OBS interpolates four consecutive samples with four normalized-sinc
//! coefficient vectors. This is a meter estimate, not a true-peak limiter.
//! The caller owns one state per audio stream and must reset it on a format,
//! stream-generation, discontinuity, or sample-rate change.
//!
//! Algorithm and coefficients derived from OBS Studio's obs-audio-controls.c
//! (GPL-2.0-or-later). Memory is bounded to four f64s per channel, with at
//! most eight channels. Inputs are interleaved and rejected before state is
//! mutated when their shape or values are invalid. Arithmetic uses f64 so
//! extreme finite PCM and gain cannot overflow the diagnostic meter.

use crate::MediaError;

const MAX_CHANNELS: usize = 8;
// `_mm_set_ps` lists most-significant lane first; these arrays are in the
// chronological order of the four samples loaded into SIMD memory.
const INTERPOLATION: [[f64; 4]; 4] = [
    [-0.103943, 0.233872, 0.935489, -0.155915],
    [-0.189207, 0.504551, 0.756827, -0.216236],
    [-0.216236, 0.756827, 0.504551, -0.189207],
    [-0.155915, 0.935489, 0.233872, -0.103943],
];

#[derive(Clone, Debug)]
pub struct ObsTruePeakMeter {
    channels: usize,
    previous: [[f64; 4]; MAX_CHANNELS],
}

impl ObsTruePeakMeter {
    pub fn new(channels: usize) -> Result<Self, MediaError> {
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return Err(MediaError::InvalidBuffer);
        }
        Ok(Self { channels, previous: [[0.0; 4]; MAX_CHANNELS] })
    }

    pub fn reset(&mut self) { self.previous = [[0.0; 4]; MAX_CHANNELS]; }

    /// Returns one non-negative estimated peak per channel. The returned array
    /// is fixed-size; only `channels()` entries are meaningful. This reports
    /// the sample peaks too, even before enough input arrives for interpolation.
    pub fn process(&mut self, interleaved: &[f64]) -> Result<[f64; MAX_CHANNELS], MediaError> {
        if interleaved.len() % self.channels != 0 || interleaved.iter().any(|v| !v.is_finite()) {
            return Err(MediaError::InvalidBuffer);
        }
        let mut peaks = [0.0f64; MAX_CHANNELS];
        for frame in interleaved.chunks_exact(self.channels) {
            for (channel, &value) in frame.iter().enumerate() {
                let history = &mut self.previous[channel];
                history.copy_within(1..4, 0);
                history[3] = value;
                peaks[channel] = peaks[channel].max(value.abs());
                for weights in INTERPOLATION {
                    let x = ((history[0] * weights[0] + history[1] * weights[1])
                        + history[2] * weights[2]) + history[3] * weights[3];
                    peaks[channel] = peaks[channel].max(x.abs());
                }
            }
        }
        Ok(peaks)
    }

    pub fn channels(&self) -> usize { self.channels }
}
