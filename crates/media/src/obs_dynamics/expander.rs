// SPDX-License-Identifier: GPL-2.0-or-later
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObsDetector {
    Rms,
    Peak,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObsExpanderParameters {
    threshold_db: f32,
    ratio: f32,
    attack_ms: u32,
    release_ms: u32,
    output_db: f32,
    detector: ObsDetector,
}
impl Default for ObsExpanderParameters {
    fn default() -> Self {
        Self {
            threshold_db: -40.,
            ratio: 2.,
            attack_ms: 10,
            release_ms: 50,
            output_db: 0.,
            detector: ObsDetector::Rms,
        }
    }
}
impl ObsExpanderParameters {
    pub fn gate_preset() -> Self {
        Self {
            ratio: 10.,
            release_ms: 125,
            ..Self::default()
        }
    }
    pub fn new(
        threshold_db: f32,
        ratio: f32,
        attack_ms: u32,
        release_ms: u32,
        output_db: f32,
        detector: ObsDetector,
    ) -> Result<Self, ObsDynamicsError> {
        range("threshold_db", threshold_db, -60., 0.)?;
        range("ratio", ratio, 1., 20.)?;
        time("attack_ms", attack_ms, 100)?;
        time("release_ms", release_ms, 1000)?;
        range("output_db", output_db, -32., 32.)?;
        Ok(Self {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            output_db,
            detector,
        })
    }
    pub fn threshold_db(self) -> f32 {
        self.threshold_db
    }
    pub fn ratio(self) -> f32 {
        self.ratio
    }
    pub fn attack_ms(self) -> u32 {
        self.attack_ms
    }
    pub fn release_ms(self) -> u32 {
        self.release_ms
    }
    pub fn output_db(self) -> f32 {
        self.output_db
    }
    pub fn detector(self) -> ObsDetector {
        self.detector
    }
    pub fn transfer_db(self, input_db: f32) -> Result<f32, ObsDynamicsError> {
        if !input_db.is_finite() {
            return Err(ObsDynamicsError::InvalidParameter("input_db"));
        }
        let diff = self.threshold_db - input_db;
        let gain = if diff > 0. {
            ((1.0f32 - self.ratio) * diff).max(-60.)
        } else {
            0.
        };
        Ok(input_db + gain + self.output_db)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ObsExpander {
    rate: u32,
    channels: usize,
    parameters: ObsExpanderParameters,
    attack: f32,
    release: f32,
    output: f32,
    slope: f32,
    rms_coefficient: f32,
    runave: [f32; 8],
    gain_db: [f32; 8],
}
impl ObsExpander {
    pub fn new(
        rate: u32,
        channels: u16,
        parameters: ObsExpanderParameters,
    ) -> Result<Self, ObsDynamicsError> {
        let channels = format(rate, channels)?;
        let mut value = Self {
            rate,
            channels,
            parameters,
            attack: 0.,
            release: 0.,
            output: 1.,
            slope: 0.,
            rms_coefficient: (-100.0f32 / rate as f32).exp2(),
            runave: [0.; 8],
            gain_db: [0.; 8],
        };
        value.set_parameters(parameters);
        Ok(value)
    }
    pub fn parameters(&self) -> ObsExpanderParameters {
        self.parameters
    }
    pub fn set_parameters(&mut self, p: ObsExpanderParameters) {
        self.parameters = p;
        self.attack = (-1.0f32 / (self.rate as f32 * (p.attack_ms as f32 / 1000.0f32))).exp();
        self.release = (-1.0f32 / (self.rate as f32 * (p.release_ms as f32 / 1000.0f32))).exp();
        self.output = db_to_mul(p.output_db);
        self.slope = 1.0f32 - p.ratio;
    }
    pub fn reset(&mut self) {
        self.runave = [0.; 8];
        self.gain_db = [0.; 8];
    }
    fn sample(&mut self, input: f32, channel: usize) -> Result<f32, ObsDynamicsError> {
        // powf(x,2) is literal upstream arithmetic, not a different RMS window.
        let square = input.powf(2.0f32);
        if !square.is_finite() {
            return Err(ObsDynamicsError::NonRepresentable);
        }
        let envelope = match self.parameters.detector {
            ObsDetector::Rms => {
                let average = self.rms_coefficient * self.runave[channel]
                    + (1.0f32 - self.rms_coefficient) * square;
                if !average.is_finite() {
                    return Err(ObsDynamicsError::NonRepresentable);
                }
                self.runave[channel] = average;
                average.max(0.).sqrt()
            }
            ObsDetector::Peak => {
                self.runave[channel] = square;
                input.abs()
            }
        };
        let diff = self.parameters.threshold_db - mul_to_db(envelope);
        let gain = if diff > 0. {
            (self.slope * diff).max(-60.)
        } else {
            0.
        };
        let previous = self.gain_db[channel];
        let next = if gain > previous {
            self.attack * previous + (1.0f32 - self.attack) * gain
        } else {
            self.release * previous + (1.0f32 - self.release) * gain
        };
        if !next.is_finite() {
            return Err(ObsDynamicsError::NonRepresentable);
        }
        self.gain_db[channel] = next;
        let output = input * (db_to_mul(next.min(0.)) * self.output);
        if !output.is_finite() {
            return Err(ObsDynamicsError::NonRepresentable);
        }
        Ok(output)
    }
    pub fn process_interleaved(
        &mut self,
        samples: &mut [f32],
        limits: DynamicsLimits,
    ) -> Result<(), ObsDynamicsError> {
        block(samples, self.channels, limits, 0, 24)?;
        // Small Copy state preflight makes whole-buffer+history rejection atomic
        // without allocating a second PCM raster or changing source operation order.
        let mut candidate = *self;
        for frame in samples.chunks_exact(self.channels) {
            for (channel, sample) in frame.iter().enumerate() {
                candidate.sample(*sample, channel)?;
            }
        }
        let mut committed = *self;
        for frame in samples.chunks_exact_mut(self.channels) {
            for (channel, sample) in frame.iter_mut().enumerate() {
                *sample = committed.sample(*sample, channel)?;
            }
        }
        *self = candidate;
        Ok(())
    }
}
