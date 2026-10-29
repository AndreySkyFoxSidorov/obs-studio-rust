// SPDX-License-Identifier: GPL-2.0-or-later
//! OBS Studio32.2.2 plugins/obs-filters/eq-filter.c, basic_eq_filter.
//! Exact source commit ba2f32bdf791005443988a4955e963663e16b1ed.
//! Interleaved adapter; channel-major source ordering and f32 arithmetic remain.
//! Source/license receipt: staging/obs-three-band-eq/references/obs32.
use std::{fmt, mem::size_of};

const LOW_FREQ: f32 = 800.0;
const HIGH_FREQ: f32 = 5000.0;
// The original C denominator is a FLOAT literal, rounded to2^32 before division.
const EQ_EPSILON: f32 = 1.0f32 / 4_294_967_295.0f32;
const MAX_FRAMES: usize = 4096;
const MAX_CHANNELS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObsThreeBandEqError {
    InvalidParameter,
    InvalidFormat,
    InvalidBlock,
    BudgetExceeded,
    InvalidSample(usize),
    NonRepresentable { channel: usize, frame: usize },
}
impl fmt::Display for ObsThreeBandEqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ObsThreeBandEqError {}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ObsThreeBandEqParameters {
    low_db: f32,
    mid_db: f32,
    high_db: f32,
}
impl ObsThreeBandEqParameters {
    pub fn new(low_db: f32, mid_db: f32, high_db: f32) -> Result<Self, ObsThreeBandEqError> {
        if ![low_db, mid_db, high_db]
            .iter()
            .all(|value| value.is_finite() && (-20.0..=20.0).contains(value))
        {
            return Err(ObsThreeBandEqError::InvalidParameter);
        }
        Ok(Self { low_db, mid_db, high_db })
    }
    pub fn low_db(self) -> f32 { self.low_db }
    pub fn mid_db(self) -> f32 { self.mid_db }
    pub fn high_db(self) -> f32 { self.high_db }
}

#[derive(Clone, Copy, Debug)]
pub struct ObsThreeBandEqLimits {
    pub max_frames: usize,
    /// Borrowed PCM bytes plus the single fixed candidate history array.
    /// The processor's retained payload is separately charged by its host owner.
    pub max_bytes: usize,
    /// Finite input validation, representability pass and deterministic commit.
    pub max_sample_visits: usize,
}
impl Default for ObsThreeBandEqLimits {
    fn default() -> Self {
        Self {
            max_frames: MAX_FRAMES,
            max_bytes: MAX_FRAMES * MAX_CHANNELS * size_of::<f32>()
                + size_of::<[ChannelState; MAX_CHANNELS]>(),
            max_sample_visits: MAX_FRAMES * MAX_CHANNELS * 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ChannelState {
    lf_delay0: f32,
    lf_delay1: f32,
    lf_delay2: f32,
    lf_delay3: f32,
    hf_delay0: f32,
    hf_delay1: f32,
    hf_delay2: f32,
    hf_delay3: f32,
    sample_delay1: f32,
    sample_delay2: f32,
    sample_delay3: f32,
}
impl ChannelState {
    fn finite(self) -> bool {
        [self.lf_delay0, self.lf_delay1, self.lf_delay2, self.lf_delay3,
         self.hf_delay0, self.hf_delay1, self.hf_delay2, self.hf_delay3,
         self.sample_delay1, self.sample_delay2, self.sample_delay3]
            .iter().all(|value| value.is_finite())
    }
}

#[derive(Clone, Debug)]
pub struct ObsThreeBandEq {
    rate: u32,
    channels: usize,
    parameters: ObsThreeBandEqParameters,
    lf: f32,
    hf: f32,
    low_gain: f32,
    mid_gain: f32,
    high_gain: f32,
    history: [ChannelState; MAX_CHANNELS],
}
impl ObsThreeBandEq {
    pub fn new(rate: u32, channels: u16, parameters: ObsThreeBandEqParameters)
        -> Result<Self, ObsThreeBandEqError>
    {
        if !(8000..=192000).contains(&rate) || !(1..=8).contains(&channels) {
            return Err(ObsThreeBandEqError::InvalidFormat);
        }
        // C: float freq = sample_rate; 2.0f * sinf(M_PI * LOW_FREQ / freq).
        // M_PI promotes that argument expression to double, then sinf receives
        // one rounded f32. Do not substitute a wholly-f32 PI expression/FMA.
        let freq = rate as f32;
        let coefficient = |edge: f32| {
            let argument = (std::f64::consts::PI * f64::from(edge) / f64::from(freq)) as f32;
            2.0f32 * argument.sin()
        };
        Ok(Self {
            rate, channels: usize::from(channels), parameters,
            lf: coefficient(LOW_FREQ), hf: coefficient(HIGH_FREQ),
            low_gain: db_to_mul(parameters.low_db),
            mid_gain: db_to_mul(parameters.mid_db),
            high_gain: db_to_mul(parameters.high_db),
            history: [ChannelState::default(); MAX_CHANNELS],
        })
    }
    pub fn sample_rate(&self) -> u32 { self.rate }
    pub fn channels(&self) -> u16 { self.channels as u16 }
    pub fn parameters(&self) -> ObsThreeBandEqParameters { self.parameters }
    pub fn set_parameters(&mut self, parameters: ObsThreeBandEqParameters) {
        if parameters == self.parameters { return; }
        // OBS eq_update changes only gains; preserve all channel history.
        self.low_gain = db_to_mul(parameters.low_db);
        self.mid_gain = db_to_mul(parameters.mid_db);
        self.high_gain = db_to_mul(parameters.high_db);
        self.parameters = parameters;
    }
    pub fn reset(&mut self) { self.history = [ChannelState::default(); MAX_CHANNELS]; }

    pub fn process_interleaved(&mut self, samples: &mut [f32], limits: ObsThreeBandEqLimits)
        -> Result<(), ObsThreeBandEqError>
    {
        if !samples.len().is_multiple_of(self.channels) {
            return Err(ObsThreeBandEqError::InvalidBlock);
        }
        let frames = samples.len() / self.channels;
        let bytes = samples.len().checked_mul(size_of::<f32>())
            .and_then(|bytes| bytes.checked_add(size_of::<[ChannelState; MAX_CHANNELS]>()))
            .ok_or(ObsThreeBandEqError::BudgetExceeded)?;
        if frames > MAX_FRAMES || frames > limits.max_frames || bytes > limits.max_bytes
            || samples.len().checked_mul(3).is_none_or(|visits| visits > limits.max_sample_visits)
        {
            return Err(ObsThreeBandEqError::BudgetExceeded);
        }
        if let Some(index) = samples.iter().position(|value| !value.is_finite()) {
            return Err(ObsThreeBandEqError::InvalidSample(index));
        }
        // No allocation. Dry-run every channel/sample before modifying caller
        // PCM or persistent history. Overflow is rejected, never hard-clipped.
        let mut candidate = self.history;
        for channel in 0..self.channels {
            for frame in 0..frames {
                let output = self.sample(&mut candidate[channel], samples[frame * self.channels + channel]);
                if !output.is_finite() || !candidate[channel].finite() {
                    return Err(ObsThreeBandEqError::NonRepresentable { channel, frame });
                }
            }
        }
        // Identical pure arithmetic and the same exclusively borrowed input:
        // no fallible operation remains after the first output write.
        candidate = self.history;
        for channel in 0..self.channels {
            for frame in 0..frames {
                let index = frame * self.channels + channel;
                samples[index] = self.sample(&mut candidate[channel], samples[index]);
            }
        }
        self.history = candidate;
        Ok(())
    }
    fn sample(&self, c: &mut ChannelState, sample: f32) -> f32 {
        c.lf_delay0 += self.lf * (sample - c.lf_delay0) + EQ_EPSILON;
        c.lf_delay1 += self.lf * (c.lf_delay0 - c.lf_delay1);
        c.lf_delay2 += self.lf * (c.lf_delay1 - c.lf_delay2);
        c.lf_delay3 += self.lf * (c.lf_delay2 - c.lf_delay3);
        let mut l = c.lf_delay3;
        c.hf_delay0 += self.hf * (sample - c.hf_delay0) + EQ_EPSILON;
        c.hf_delay1 += self.hf * (c.hf_delay0 - c.hf_delay1);
        c.hf_delay2 += self.hf * (c.hf_delay1 - c.hf_delay2);
        c.hf_delay3 += self.hf * (c.hf_delay2 - c.hf_delay3);
        let mut h = c.sample_delay3 - c.hf_delay3;
        let mut m = c.sample_delay3 - (h + l);
        l *= self.low_gain;
        m *= self.mid_gain;
        h *= self.high_gain;
        c.sample_delay3 = c.sample_delay2;
        c.sample_delay2 = c.sample_delay1;
        c.sample_delay1 = sample;
        l + m + h
    }
}
fn db_to_mul(db: f32) -> f32 { 10.0f32.powf(db / 20.0f32) }
