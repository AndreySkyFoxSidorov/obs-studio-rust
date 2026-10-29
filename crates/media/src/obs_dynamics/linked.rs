// SPDX-License-Identifier: GPL-2.0-or-later
use super::*;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObsLimiterParameters {
    threshold_db: f32,
    release_ms: u32,
}
impl Default for ObsLimiterParameters {
    fn default() -> Self {
        Self {
            threshold_db: -6.,
            release_ms: 60,
        }
    }
}
impl ObsLimiterParameters {
    pub fn new(threshold_db: f32, release_ms: u32) -> Result<Self, ObsDynamicsError> {
        range("threshold_db", threshold_db, -60., 0.)?;
        time("release_ms", release_ms, 1000)?;
        Ok(Self {
            threshold_db,
            release_ms,
        })
    }
    pub fn threshold_db(self) -> f32 {
        self.threshold_db
    }
    pub fn release_ms(self) -> u32 {
        self.release_ms
    }
    pub fn transfer_db(self, input_db: f32) -> Result<f32, ObsDynamicsError> {
        if !input_db.is_finite() {
            return Err(ObsDynamicsError::InvalidParameter("input_db"));
        }
        Ok(input_db.min(self.threshold_db))
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObsCompressorParameters {
    threshold_db: f32,
    ratio: f32,
    attack_ms: u32,
    release_ms: u32,
    output_db: f32,
}
impl Default for ObsCompressorParameters {
    fn default() -> Self {
        Self {
            threshold_db: -18.,
            ratio: 10.,
            attack_ms: 6,
            release_ms: 60,
            output_db: 0.,
        }
    }
}
impl ObsCompressorParameters {
    pub fn new(
        threshold_db: f32,
        ratio: f32,
        attack_ms: u32,
        release_ms: u32,
        output_db: f32,
    ) -> Result<Self, ObsDynamicsError> {
        range("threshold_db", threshold_db, -60., 0.)?;
        range("ratio", ratio, 1., 32.)?;
        time("attack_ms", attack_ms, 500)?;
        time("release_ms", release_ms, 1000)?;
        range("output_db", output_db, -32., 32.)?;
        Ok(Self {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            output_db,
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
    pub fn transfer_db(self, input_db: f32) -> Result<f32, ObsDynamicsError> {
        if !input_db.is_finite() {
            return Err(ObsDynamicsError::InvalidParameter("input_db"));
        }
        Ok(input_db
            + ((1. - 1. / self.ratio) * (self.threshold_db - input_db)).min(0.)
            + self.output_db)
    }
}
#[derive(Clone, Copy, Debug)]
struct Linked {
    threshold: f32,
    attack: f32,
    release: f32,
    output: f32,
    slope: f32,
    envelope: f32,
}
fn coefficient(rate: u32, time_seconds: f32) -> f32 {
    f64::from(-1.0f32 / (rate as f32 * time_seconds)).exp() as f32
}
impl Linked {
    fn process(
        &mut self,
        samples: &mut [f32],
        channels: usize,
        scratch: &mut [f32],
        limits: DynamicsLimits,
    ) -> Result<(), ObsDynamicsError> {
        self.process_detector(samples,None,channels,scratch,limits)
    }
    fn process_detector(&mut self,samples:&mut[f32],detector:Option<&[f32]>,channels:usize,
        scratch:&mut[f32],limits:DynamicsLimits)->Result<(),ObsDynamicsError>{
        if let Some(detector)=detector{
            if detector.len()!=samples.len(){return Err(ObsDynamicsError::InvalidBlock);}
            let frames=detector.len()/channels;
            let bytes=samples.len().checked_add(detector.len()).and_then(|n|n.checked_add(frames)).and_then(|n|n.checked_mul(4))
                .ok_or(ObsDynamicsError::BudgetExceeded)?;
            if bytes>limits.max_bytes{return Err(ObsDynamicsError::BudgetExceeded);}
            block(detector,channels,limits,frames,14)?;
        }
        let frames = samples.len() / channels;
        if scratch.len() < frames {
            return Err(ObsDynamicsError::InvalidBlock);
        }
        let frames = block(samples, channels, limits, frames, 12)?;
        if frames == 0 {
            return Ok(());
        }
        let scratch = &mut scratch[..frames];
        scratch.fill(0.);
        // Literal OBS callback rule: each channel begins from the SAME stored
        // global envelope, not a new frame-wise max across channel samples.
        for channel in 0..channels {
            let mut env = self.envelope;
            for (i, value) in scratch.iter_mut().enumerate() {
                let input = detector.unwrap_or(samples)[i * channels + channel].abs();
                let c = if env < input {
                    self.attack
                } else {
                    self.release
                };
                env = input + c * (env - input);
                *value = value.max(env);
            }
        }
        let next = scratch[frames - 1];
        // Scratch becomes gain only AFTER detector completion. Preflight every
        // output; a late nonrepresentable result cannot mutate PCM or history.
        for (i, value) in scratch.iter_mut().enumerate() {
            let db = mul_to_db(*value);
            let gain = db_to_mul((self.slope * (self.threshold - db)).min(0.));
            let factor = gain * self.output;
            if !factor.is_finite()
                || (0..channels).any(|c| !(samples[i * channels + c] * factor).is_finite())
            {
                return Err(ObsDynamicsError::NonRepresentable);
            }
            *value = factor;
        }
        for (i, frame) in samples.chunks_exact_mut(channels).enumerate() {
            for sample in frame {
                *sample *= scratch[i];
            }
        }
        self.envelope = next;
        Ok(())
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ObsLimiter {
    rate: u32,
    channels: usize,
    parameters: ObsLimiterParameters,
    linked: Linked,
}
impl ObsLimiter {
    pub fn new(
        rate: u32,
        channels: u16,
        parameters: ObsLimiterParameters,
    ) -> Result<Self, ObsDynamicsError> {
        let channels = format(rate, channels)?;
        let mut value = Self {
            rate,
            channels,
            parameters,
            linked: Linked {
                threshold: 0.,
                attack: 0.,
                release: 0.,
                output: 1.,
                slope: 1.,
                envelope: 0.,
            },
        };
        value.set_parameters(parameters);
        Ok(value)
    }
    pub fn parameters(&self) -> ObsLimiterParameters {
        self.parameters
    }
    pub fn set_parameters(&mut self, p: ObsLimiterParameters) {
        self.parameters = p;
        self.linked.threshold = p.threshold_db;
        self.linked.attack = coefficient(self.rate, 0.001f32 / 1000.0f32);
        self.linked.release = coefficient(self.rate, p.release_ms as f32 / 1000.0f32);
    }
    pub fn reset(&mut self) {
        self.linked.envelope = 0.;
    }
    pub fn envelope(&self) -> f32 {
        self.linked.envelope
    }
    pub fn process_callback(
        &mut self,
        samples: &mut [f32],
        scratch: &mut [f32],
        limits: DynamicsLimits,
    ) -> Result<(), ObsDynamicsError> {
        self.linked.process(samples, self.channels, scratch, limits)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ObsCompressor {
    rate: u32,
    channels: usize,
    parameters: ObsCompressorParameters,
    linked: Linked,
}
impl ObsCompressor {
    pub fn new(
        rate: u32,
        channels: u16,
        parameters: ObsCompressorParameters,
    ) -> Result<Self, ObsDynamicsError> {
        let channels = format(rate, channels)?;
        let mut value = Self {
            rate,
            channels,
            parameters,
            linked: Linked {
                threshold: 0.,
                attack: 0.,
                release: 0.,
                output: 1.,
                slope: 1.,
                envelope: 0.,
            },
        };
        value.set_parameters(parameters);
        Ok(value)
    }
    pub fn channels(&self)->u16{self.channels as u16}
    pub fn parameters(&self) -> ObsCompressorParameters {
        self.parameters
    }
    pub fn set_parameters(&mut self, p: ObsCompressorParameters) {
        self.parameters = p;
        self.linked.threshold = p.threshold_db;
        self.linked.attack = coefficient(self.rate, p.attack_ms as f32 / 1000.0f32);
        self.linked.release = coefficient(self.rate, p.release_ms as f32 / 1000.0f32);
        self.linked.output = db_to_mul(p.output_db);
        self.linked.slope = 1.0f32 - 1.0f32 / p.ratio;
    }
    pub fn reset(&mut self) {
        self.linked.envelope = 0.;
    }
    pub fn envelope(&self) -> f32 {
        self.linked.envelope
    }
    /// External OBS32.2.2 pre-fader key PCM (ba2f32bdf791005443988a4955e963663e16b1ed).
    /// Program samples remain the gain target; no timestamp/own-detector fallback.
    pub fn process_sidechain_callback(&mut self,samples:&mut[f32],detector:&[f32],
        scratch:&mut[f32],limits:DynamicsLimits)->Result<(),ObsDynamicsError>{
        self.linked.process_detector(samples,Some(detector),self.channels,scratch,limits)
    }
    pub fn process_callback(
        &mut self,
        samples: &mut [f32],
        scratch: &mut [f32],
        limits: DynamicsLimits,
    ) -> Result<(), ObsDynamicsError> {
        self.linked.process(samples, self.channels, scratch, limits)
    }
}
