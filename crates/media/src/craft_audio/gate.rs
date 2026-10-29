// Adapted from FilmCraft, copyright (c) 2026 ArtCraft Team and contributors.
// MIT-licensed source; preserve LICENSE-MIT, NOTICE and SOURCE.json with this code.
use std::{error::Error, fmt};

pub const MAX_GATE_FRAMES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateError {
    InvalidParameter(&'static str),
    InvalidFormat,
    InvalidBlock,
    InvalidSample,
}
impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameter(name) => write!(f, "invalid gate parameter: {name}"),
            Self::InvalidFormat => f.write_str("gate requires 8000..192000 Hz and 1..8 channels"),
            Self::InvalidBlock => f.write_str("gate block shape or frame count is invalid"),
            Self::InvalidSample => f.write_str("gate sample must be finite"),
        }
    }
}
impl Error for GateError {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateParameters {
    threshold_db: f32,
    ratio: f32,
    range_db: f32,
    attack_ms: f32,
    hold_ms: f32,
    release_ms: f32,
}
impl Default for GateParameters {
    fn default() -> Self {
        Self {
            threshold_db: -40.0,
            ratio: 10.0,
            range_db: 60.0,
            attack_ms: 1.0,
            hold_ms: 50.0,
            release_ms: 150.0,
        }
    }
}
impl GateParameters {
    pub fn new(
        threshold_db: f32,
        ratio: f32,
        range_db: f32,
        attack_ms: f32,
        hold_ms: f32,
        release_ms: f32,
    ) -> Result<Self, GateError> {
        for (name, value, min, max) in [
            ("threshold_db", threshold_db, -90.0, 0.0),
            ("ratio", ratio, 1.0, 100.0),
            ("range_db", range_db, 0.0, 90.0),
            ("attack_ms", attack_ms, 0.01, 100.0),
            ("hold_ms", hold_ms, 0.0, 2000.0),
            ("release_ms", release_ms, 1.0, 3000.0),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(GateError::InvalidParameter(name));
            }
        }
        Ok(Self {
            threshold_db,
            ratio,
            range_db,
            attack_ms,
            hold_ms,
            release_ms,
        })
    }
    pub fn threshold_db(self) -> f32 {
        self.threshold_db
    }
    pub fn ratio(self) -> f32 {
        self.ratio
    }
    pub fn range_db(self) -> f32 {
        self.range_db
    }
    pub fn attack_ms(self) -> f32 {
        self.attack_ms
    }
    pub fn hold_ms(self) -> f32 {
        self.hold_ms
    }
    pub fn release_ms(self) -> f32 {
        self.release_ms
    }
    /// Static output level, with attenuation limited to the configured range.
    pub fn transfer_db(self, input_db: f32) -> Result<f32, GateError> {
        if !input_db.is_finite() {
            return Err(GateError::InvalidParameter("input_db"));
        }
        Ok(self.curve(input_db))
    }
    fn curve(self, input: f32) -> f32 {
        if input >= self.threshold_db {
            input
        } else {
            input + ((input - self.threshold_db) * (self.ratio - 1.0)).max(-self.range_db)
        }
    }
}

/// Linked downward expander. It attenuates samples, never discards a packet or
/// changes stream readiness. All state is inline and all operations allocate nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct Gate {
    parameters: GateParameters,
    sample_rate: u32,
    channels: u16,
    attack: f32,
    release: f32,
    hold: u32,
    hold_left: u32,
    envelope_db: f32,
}
impl Gate {
    pub fn new(
        sample_rate: u32,
        channels: u16,
        parameters: GateParameters,
    ) -> Result<Self, GateError> {
        if !(8000..=192000).contains(&sample_rate) || !(1..=8).contains(&channels) {
            return Err(GateError::InvalidFormat);
        }
        let mut processor = Self {
            parameters,
            sample_rate,
            channels,
            attack: 0.0,
            release: 0.0,
            hold: 0,
            hold_left: 0,
            envelope_db: 0.0,
        };
        processor.set_parameters(parameters);
        Ok(processor)
    }
    pub fn parameters(&self) -> GateParameters {
        self.parameters
    }
    pub fn set_parameters(&mut self, parameters: GateParameters) {
        self.parameters = parameters;
        self.attack = coefficient(parameters.attack_ms, self.sample_rate);
        self.release = coefficient(parameters.release_ms, self.sample_rate);
        // Preserve upstream f32 ordering and truncation, including hold=0.
        self.hold = (parameters.hold_ms * 0.001 * self.sample_rate as f32) as u32;
    }
    pub fn latency_frames(&self) -> usize {
        0
    }
    pub fn gain_reduction_db(&self) -> f32 {
        self.envelope_db
    }
    pub fn transfer_db(&self, input_db: f32) -> Result<f32, GateError> {
        self.parameters.transfer_db(input_db)
    }
    pub fn reset(&mut self) {
        self.envelope_db = 0.0;
        self.hold_left = 0;
    }
    pub fn process_planar(&mut self, channels: &mut [&mut [f32]]) -> Result<(), GateError> {
        let frames = channels.first().map_or(0, |channel| channel.len());
        if channels.len() != usize::from(self.channels)
            || frames > MAX_GATE_FRAMES
            || channels.iter().any(|channel| channel.len() != frames)
        {
            return Err(GateError::InvalidBlock);
        }
        if channels
            .iter()
            .flat_map(|channel| channel.iter())
            .any(|sample| !sample.is_finite())
        {
            return Err(GateError::InvalidSample);
        }
        for frame in 0..frames {
            let peak = channels
                .iter()
                .fold(0.0f32, |peak, channel| peak.max(channel[frame].abs()));
            let gain = self.next_gain(peak);
            for channel in channels.iter_mut() {
                channel[frame] *= gain;
            }
        }
        Ok(())
    }
    pub fn process_interleaved(&mut self, samples: &mut [f32]) -> Result<(), GateError> {
        let channels = usize::from(self.channels);
        if samples.len() % channels != 0 || samples.len() / channels > MAX_GATE_FRAMES {
            return Err(GateError::InvalidBlock);
        }
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(GateError::InvalidSample);
        }
        for frame in samples.chunks_exact_mut(channels) {
            let gain = self.next_gain(
                frame
                    .iter()
                    .fold(0.0f32, |peak, sample| peak.max(sample.abs())),
            );
            for sample in frame {
                *sample *= gain;
            }
        }
        Ok(())
    }
    fn next_gain(&mut self, peak: f32) -> f32 {
        let level = 20.0 * peak.max(1e-10).log10();
        let target = self.parameters.curve(level) - level;
        if target >= self.envelope_db {
            self.envelope_db = self.attack * self.envelope_db + (1.0 - self.attack) * target;
            if target >= 0.0 {
                self.hold_left = self.hold;
            }
        } else if self.hold_left > 0 {
            self.hold_left -= 1;
        } else {
            self.envelope_db = self.release * self.envelope_db + (1.0 - self.release) * target;
        }
        10.0f32.powf(self.envelope_db / 20.0)
    }
}
fn coefficient(ms: f32, rate: u32) -> f32 {
    (-1.0 / (ms * 0.001 * rate as f32)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_curve_range_and_parameter_boundaries() {
        let p = GateParameters::new(-40.0, 3.0, 60.0, 1.0, 0.0, 150.0).unwrap();
        for (input, output) in [
            (0.0, 0.0),
            (-40.0, -40.0),
            (-50.0, -70.0),
            (-70.0, -130.0),
            (-100.0, -160.0),
        ] {
            assert_eq!(p.transfer_db(input).unwrap(), output);
        }
        for (threshold, ratio, range, attack, hold, release) in [
            (f32::NAN, 3.0, 60.0, 1.0, 0.0, 150.0),
            (-90.01, 3.0, 60.0, 1.0, 0.0, 150.0),
            (-40.0, 100.01, 60.0, 1.0, 0.0, 150.0),
            (-40.0, 3.0, 90.01, 1.0, 0.0, 150.0),
            (-40.0, 3.0, 60.0, 0.0, 0.0, 150.0),
            (-40.0, 3.0, 60.0, 1.0, -0.1, 150.0),
            (-40.0, 3.0, 60.0, 1.0, 0.0, f32::INFINITY),
        ] {
            assert!(GateParameters::new(threshold, ratio, range, attack, hold, release).is_err());
        }
        assert!(p.transfer_db(f32::NEG_INFINITY).is_err());
        for (rate, channels) in [(7999, 2), (192001, 2), (48000, 0), (48000, 9)] {
            assert!(Gate::new(rate, channels, p).is_err());
        }
    }
    #[test]
    fn hold_counts_exact_samples_and_closing_has_independent_time_constant_oracle() {
        let p = GateParameters::new(-20.0, 2.0, 60.0, 1.0, 1.0, 1.0).unwrap();
        for rate in [8000, 48000, 192000] {
            let mut gate = Gate::new(rate, 1, p).unwrap();
            gate.process_interleaved(&mut [1.0]).unwrap();
            let hold = (rate as f32 * 0.001) as usize;
            let mut quiet = vec![0.01; hold];
            gate.process_interleaved(&mut quiet).unwrap();
            assert!(quiet.iter().all(|&sample| sample == 0.01));
            let mut one = [0.01];
            gate.process_interleaved(&mut one).unwrap();
            assert!(one[0] < 0.01);
            // After one release time constant, attenuation reaches 1-exp(-1)
            // of the target -20dB; independent f64 exponential reference.
            gate.reset();
            let mut quiet = vec![0.01; hold];
            gate.process_interleaved(&mut quiet).unwrap();
            let expected_db = -20.0 * (1.0 - (-1.0f64).exp());
            assert!((f64::from(gate.gain_reduction_db()) - expected_db).abs() < 0.002);
            assert_eq!(gate.latency_frames(), 0);
        }
    }
    #[test]
    fn linked_channels_partitions_parameter_carry_and_reset_are_exact() {
        let p = GateParameters::new(-12.0, 4.0, 60.0, 0.1, 2.0, 5.0).unwrap();
        let mut packed: Vec<f32> = (0..8192)
            .flat_map(|i| {
                let x = (i as f32 * 0.013).sin() * if i < 4000 { 0.5 } else { 0.02 };
                [x, -x * 0.25]
            })
            .collect();
        let mut left: Vec<_> = packed.chunks_exact(2).map(|f| f[0]).collect();
        let mut right: Vec<_> = packed.chunks_exact(2).map(|f| f[1]).collect();
        let mut a = Gate::new(48000, 2, p).unwrap();
        let mut b = a.clone();
        for chunk in packed.chunks_mut(2048) {
            a.process_interleaved(chunk).unwrap();
        }
        for (l, r) in left.chunks_mut(17).zip(right.chunks_mut(17)) {
            b.process_planar(&mut [l, r]).unwrap();
        }
        for (f, (l, r)) in packed.chunks_exact(2).zip(left.iter().zip(&right)) {
            assert_eq!(f, &[*l, *r]);
            assert_eq!(*r, -*l * 0.25);
        }
        assert_eq!(a, b);
        assert!(a.gain_reduction_db() < 0.0);
        a.set_parameters(GateParameters::default());
        assert!(a.gain_reduction_db() < 0.0);
        a.reset();
        assert_eq!(a, Gate::new(48000, 2, GateParameters::default()).unwrap());
    }
    #[test]
    fn bad_blocks_leave_state_and_output_unchanged() {
        let mut gate = Gate::new(48000, 2, GateParameters::default()).unwrap();
        let before = gate.clone();
        let mut bad = [0.1, 0.2, f32::NAN, 0.3];
        let bits = bad.map(f32::to_bits);
        assert_eq!(
            gate.process_interleaved(&mut bad),
            Err(GateError::InvalidSample)
        );
        assert_eq!(bad.map(f32::to_bits), bits);
        assert_eq!(gate, before);
        assert_eq!(
            gate.process_interleaved(&mut [0.1]),
            Err(GateError::InvalidBlock)
        );
        assert_eq!(
            gate.process_planar(&mut [&mut [0.0; 1], &mut [0.0; 2]]),
            Err(GateError::InvalidBlock)
        );
        assert_eq!(
            gate.process_interleaved(&mut vec![0.0; 8194]),
            Err(GateError::InvalidBlock)
        );
        assert_eq!(gate, before);
        gate.process_interleaved(&mut [f32::MAX, -f32::MAX])
            .unwrap();
    }
}
