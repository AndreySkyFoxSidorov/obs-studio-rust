// Adapted from FilmCraft, copyright (c) 2026 ArtCraft Team and contributors.
// MIT-licensed source; preserve LICENSE-MIT, NOTICE and SOURCE.json with this code.
use std::{error::Error, fmt};

pub const MAX_COMPRESSOR_FRAMES: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detector {
    Peak,
    Rms,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressorError {
    InvalidParameter(&'static str),
    InvalidFormat,
    InvalidBlock,
    InvalidSample,
}
impl fmt::Display for CompressorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameter(name) => write!(f, "invalid compressor parameter: {name}"),
            Self::InvalidFormat => {
                f.write_str("compressor requires 8000..192000 Hz and 1..8 channels")
            }
            Self::InvalidBlock => f.write_str("compressor block shape or frame count is invalid"),
            Self::InvalidSample => {
                f.write_str("compressor sample is nonfinite or exceeds safe arithmetic bounds")
            }
        }
    }
}
impl Error for CompressorError {}

/// Validated source settings, not envelope/runtime state. Values are never
/// silently clamped, including NaN/inf. Detector is ordinary peak or RMS,
/// not intersample true peak and not a look-ahead limiter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompressorParameters {
    threshold_db: f32,
    ratio: f32,
    knee_db: f32,
    attack_ms: f32,
    release_ms: f32,
    makeup_db: f32,
    detector: Detector,
}
impl Default for CompressorParameters {
    fn default() -> Self {
        Self {
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 6.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 0.0,
            detector: Detector::Peak,
        }
    }
}
impl CompressorParameters {
    pub fn new(
        threshold_db: f32,
        ratio: f32,
        knee_db: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
        detector: Detector,
    ) -> Result<Self, CompressorError> {
        for (name, value, min, max) in [
            ("threshold_db", threshold_db, -60.0, 0.0),
            ("ratio", ratio, 1.0, 30.0),
            ("knee_db", knee_db, 0.0, 24.0),
            ("attack_ms", attack_ms, 0.01, 300.0),
            ("release_ms", release_ms, 1.0, 3000.0),
            ("makeup_db", makeup_db, -40.0, 40.0),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(CompressorError::InvalidParameter(name));
            }
        }
        Ok(Self {
            threshold_db,
            ratio,
            knee_db,
            attack_ms,
            release_ms,
            makeup_db,
            detector,
        })
    }
    pub fn threshold_db(self) -> f32 {
        self.threshold_db
    }
    pub fn ratio(self) -> f32 {
        self.ratio
    }
    pub fn knee_db(self) -> f32 {
        self.knee_db
    }
    pub fn attack_ms(self) -> f32 {
        self.attack_ms
    }
    pub fn release_ms(self) -> f32 {
        self.release_ms
    }
    pub fn makeup_db(self) -> f32 {
        self.makeup_db
    }
    pub fn detector(self) -> Detector {
        self.detector
    }
    /// Static output level including make-up gain, for the same DSP's UI curve.
    pub fn transfer_db(self, input_db: f32) -> Result<f32, CompressorError> {
        if !input_db.is_finite() {
            return Err(CompressorError::InvalidParameter("input_db"));
        }
        Ok(curve(input_db, self) + self.makeup_db)
    }
}

/// One persistent processor per source/filter/format stream. Constant-size
/// scalar state; construction, parameter updates, reset and processing allocate
/// nothing. Shape and every input sample are checked before state/output edits.
#[derive(Clone, Debug)]
pub struct Compressor {
    parameters: CompressorParameters,
    sample_rate: u32,
    channels: u16,
    attack: f32,
    release: f32,
    rms_coefficient: f32,
    makeup: Smoothed,
    mean_square: f32,
    envelope_db: f32,
    gain_reduction_db: f32,
}
impl Compressor {
    pub fn new(
        sample_rate: u32,
        channels: u16,
        parameters: CompressorParameters,
    ) -> Result<Self, CompressorError> {
        if !(8000..=192000).contains(&sample_rate) || !(1..=8).contains(&channels) {
            return Err(CompressorError::InvalidFormat);
        }
        Ok(Self {
            parameters,
            sample_rate,
            channels,
            attack: coefficient(parameters.attack_ms, sample_rate),
            release: coefficient(parameters.release_ms, sample_rate),
            rms_coefficient: coefficient(10.0, sample_rate),
            makeup: Smoothed::new(
                db_to_gain(parameters.makeup_db),
                (sample_rate as f32 * 20.0 / 1000.0).round() as u32,
            ),
            mean_square: 0.0,
            envelope_db: 0.0,
            gain_reduction_db: 0.0,
        })
    }
    pub fn parameters(&self) -> CompressorParameters {
        self.parameters
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }
    pub fn latency_frames(&self) -> usize {
        0
    }
    /// FilmCraft convention: negative dB attenuation; zero means no reduction.
    pub fn gain_reduction_db(&self) -> f32 {
        self.gain_reduction_db
    }
    /// Preserve the detector/envelope, beginning only a make-up ramp and changing
    /// coefficients for subsequent samples. Equal settings are a true no-op.
    pub fn set_parameters(&mut self, parameters: CompressorParameters) -> bool {
        if self.parameters == parameters {
            return false;
        }
        self.parameters = parameters;
        self.attack = coefficient(parameters.attack_ms, self.sample_rate);
        self.release = coefficient(parameters.release_ms, self.sample_rate);
        self.makeup.set(db_to_gain(parameters.makeup_db));
        true
    }
    /// Explicit discontinuity/new-stream reset. Never call once per PCM block.
    pub fn reset(&mut self) {
        self.mean_square = 0.0;
        self.envelope_db = 0.0;
        self.gain_reduction_db = 0.0;
        self.makeup.snap();
    }
    pub fn process_planar(&mut self, channels: &mut [&mut [f32]]) -> Result<(), CompressorError> {
        if channels.len() != usize::from(self.channels) {
            return Err(CompressorError::InvalidBlock);
        }
        let frames = channels.first().map_or(0, |channel| channel.len());
        if frames > MAX_COMPRESSOR_FRAMES || channels.iter().any(|channel| channel.len() != frames)
        {
            return Err(CompressorError::InvalidBlock);
        }
        if channels
            .iter()
            .flat_map(|channel| channel.iter())
            .any(|&sample| !valid_sample(sample))
        {
            return Err(CompressorError::InvalidSample);
        }
        for frame in 0..frames {
            let mut peak = 0.0f32;
            let mut square = 0.0f32;
            for channel in channels.iter() {
                let sample = channel[frame];
                peak = peak.max(sample.abs());
                square = square.max(sample * sample);
            }
            let gain = self.next_gain(peak, square);
            for channel in channels.iter_mut() {
                channel[frame] *= gain;
            }
        }
        Ok(())
    }
    /// Identical linked algorithm without allocating a deinterleave scratch.
    pub fn process_interleaved(&mut self, samples: &mut [f32]) -> Result<(), CompressorError> {
        let channels = usize::from(self.channels);
        if samples.len() % channels != 0 || samples.len() / channels > MAX_COMPRESSOR_FRAMES {
            return Err(CompressorError::InvalidBlock);
        }
        if samples.iter().any(|&sample| !valid_sample(sample)) {
            return Err(CompressorError::InvalidSample);
        }
        for frame in samples.chunks_exact_mut(channels) {
            let mut peak = 0.0f32;
            let mut square = 0.0f32;
            for &sample in frame.iter() {
                peak = peak.max(sample.abs());
                square = square.max(sample * sample);
            }
            let gain = self.next_gain(peak, square);
            for sample in frame {
                *sample *= gain;
            }
        }
        Ok(())
    }
    fn next_gain(&mut self, peak: f32, square: f32) -> f32 {
        let level_db = match self.parameters.detector {
            Detector::Peak => 20.0 * peak.max(1e-10).log10(),
            Detector::Rms => {
                self.mean_square =
                    self.rms_coefficient * self.mean_square + (1.0 - self.rms_coefficient) * square;
                if self.mean_square < 1e-30 {
                    self.mean_square = 0.0;
                }
                10.0 * self.mean_square.max(1e-20).log10()
            }
        };
        let target = curve(level_db, self.parameters) - level_db;
        let coefficient = if target < self.envelope_db {
            self.attack
        } else {
            self.release
        };
        self.envelope_db = coefficient * self.envelope_db + (1.0 - coefficient) * target;
        if self.envelope_db.abs() < 1e-9 {
            self.envelope_db = 0.0;
        }
        self.gain_reduction_db = self.envelope_db;
        db_to_gain(self.envelope_db) * self.makeup.tick()
    }
}

fn valid_sample(sample: f32) -> bool {
    sample.is_finite() && (sample * sample) <= f32::MAX / 4.0
}
fn coefficient(ms: f32, sample_rate: u32) -> f32 {
    (-1.0 / (ms * 0.001 * sample_rate as f32)).exp()
}
fn db_to_gain(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}
fn curve(input: f32, parameters: CompressorParameters) -> f32 {
    let delta = input - parameters.threshold_db;
    if parameters.knee_db > 0.0 && 2.0 * delta.abs() <= parameters.knee_db {
        input
            + (1.0 / parameters.ratio - 1.0) * (delta + parameters.knee_db / 2.0).powi(2)
                / (2.0 * parameters.knee_db)
    } else if delta > 0.0 {
        parameters.threshold_db + delta / parameters.ratio
    } else {
        input
    }
}

// FilmCraft's linear f64 make-up smoother, narrowed to the operations used here.
#[derive(Clone, Debug)]
struct Smoothed {
    current: f64,
    target: f64,
    step: f64,
    remaining: u32,
    ramp: u32,
}
impl Smoothed {
    fn new(value: f32, ramp: u32) -> Self {
        Self {
            current: f64::from(value),
            target: f64::from(value),
            step: 0.0,
            remaining: 0,
            ramp,
        }
    }
    fn set(&mut self, value: f32) {
        let target = f64::from(value);
        if target == self.target {
            return;
        }
        self.target = target;
        if self.ramp == 0 {
            self.snap();
        } else {
            self.remaining = self.ramp;
            self.step = (target - self.current) / f64::from(self.ramp);
        }
    }
    fn snap(&mut self) {
        self.current = self.target;
        self.remaining = 0;
    }
    fn tick(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            if self.remaining == 0 {
                self.current = self.target;
            } else {
                self.current += self.step;
            }
        }
        self.current as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parameters(detector: Detector) -> CompressorParameters {
        CompressorParameters::new(-20.0, 4.0, 0.0, 10.0, 100.0, 0.0, detector).unwrap()
    }
    #[test]
    fn static_curve_has_literal_oracles_and_parameters_reject_nonfinite_or_outside_ranges() {
        let params = parameters(Detector::Peak);
        for (input, output) in [(-40.0, -40.0), (-20.0, -20.0), (-10.0, -17.5), (0.0, -15.0)] {
            assert_eq!(params.transfer_db(input).unwrap(), output);
        }
        assert_eq!(
            CompressorParameters::default().transfer_db(-20.0).unwrap(),
            -20.5625
        );
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -60.01, 0.01] {
            assert!(
                CompressorParameters::new(value, 4.0, 6.0, 10.0, 100.0, 0.0, Detector::Peak)
                    .is_err()
            );
        }
        for (ratio, knee, attack, release, makeup) in [
            (0.9, 6.0, 10.0, 100.0, 0.0),
            (30.01, 6.0, 10.0, 100.0, 0.0),
            (4.0, 24.01, 10.0, 100.0, 0.0),
            (4.0, 6.0, 0.0, 100.0, 0.0),
            (4.0, 6.0, 10.0, 3001.0, 0.0),
            (4.0, 6.0, 10.0, 100.0, 40.01),
        ] {
            assert!(
                CompressorParameters::new(
                    -20.0,
                    ratio,
                    knee,
                    attack,
                    release,
                    makeup,
                    Detector::Peak
                )
                .is_err()
            );
        }
        for (rate, channels) in [(7999, 2), (192001, 2), (48000, 0), (48000, 9)] {
            assert!(Compressor::new(rate, channels, params).is_err());
        }
        assert!(params.transfer_db(f32::NAN).is_err());
    }
    #[test]
    fn linked_planar_interleaved_and_arbitrary_partitions_are_identical() {
        for detector in [Detector::Peak, Detector::Rms] {
            let input: Vec<f32> = (0..8192)
                .map(|index| (index as f32 * 0.071).sin() * 0.8)
                .collect();
            let mut whole_left = input.clone();
            let mut whole_right: Vec<_> = input.iter().map(|x| x * 0.25).collect();
            let mut split_left = whole_left.clone();
            let mut split_right = whole_right.clone();
            let mut whole = Compressor::new(48000, 2, parameters(detector)).unwrap();
            let mut split = whole.clone();
            let mut interleaved = whole.clone();
            for (left, right) in whole_left
                .chunks_mut(4096)
                .zip(whole_right.chunks_mut(4096))
            {
                whole.process_planar(&mut [left, right]).unwrap();
            }
            for (left, right) in split_left.chunks_mut(17).zip(split_right.chunks_mut(17)) {
                split.process_planar(&mut [left, right]).unwrap();
            }
            assert_eq!(whole_left, split_left);
            assert_eq!(whole_right, split_right);
            assert_eq!(whole.gain_reduction_db(), split.gain_reduction_db());
            let mut packed: Vec<_> = input.iter().flat_map(|&x| [x, x * 0.25]).collect();
            for chunk in packed.chunks_mut(2048) {
                interleaved.process_interleaved(chunk).unwrap();
            }
            for (frame, (left, right)) in packed
                .chunks_exact(2)
                .zip(whole_left.iter().zip(&whole_right))
            {
                assert_eq!(frame, &[*left, *right]);
                assert_eq!(*right, *left * 0.25);
            }
            assert!(whole.gain_reduction_db() < 0.0);
            assert_eq!(whole.latency_frames(), 0);
        }
    }
    #[test]
    fn attack_is_sample_rate_aware_and_parameter_edits_preserve_stream_state() {
        for rate in [8000, 44100, 48000, 192000] {
            let mut processor = Compressor::new(rate, 1, parameters(Detector::Peak)).unwrap();
            let mut samples = vec![1.0; rate as usize / 100];
            processor.process_interleaved(&mut samples).unwrap();
            let expected = -15.0 * (1.0 - (-1.0f32).exp());
            assert!((processor.gain_reduction_db() - expected).abs() < 0.005);
            let before = processor.gain_reduction_db();
            assert!(!processor.set_parameters(processor.parameters()));
            assert_eq!(processor.gain_reduction_db(), before);
            let mut changed = processor.parameters();
            changed.makeup_db = 12.0;
            assert!(processor.set_parameters(changed));
            assert_eq!(processor.gain_reduction_db(), before);
            processor.process_interleaved(&mut [1.0]).unwrap();
            assert!(processor.gain_reduction_db() <= before);
            processor.reset();
            assert_eq!(processor.gain_reduction_db(), 0.0);
            assert_eq!(processor.parameters(), changed);
        }
    }
    #[test]
    fn rejected_blocks_leave_every_sample_and_envelope_untouched() {
        let mut processor = Compressor::new(48000, 2, parameters(Detector::Rms)).unwrap();
        processor.process_interleaved(&mut [0.5; 64]).unwrap();
        let before = processor.clone();
        let mut invalid = [0.2, f32::NAN];
        assert_eq!(
            processor.process_interleaved(&mut invalid),
            Err(CompressorError::InvalidSample)
        );
        assert_eq!(invalid[0], 0.2);
        assert!(invalid[1].is_nan());
        assert_eq!(
            processor.process_interleaved(&mut [0.2]),
            Err(CompressorError::InvalidBlock)
        );
        assert_eq!(
            processor.process_interleaved(&mut [f32::MAX, 0.1]),
            Err(CompressorError::InvalidSample)
        );
        let (mut left, mut right) = ([0.3; 2], [0.4; 1]);
        assert_eq!(
            processor.process_planar(&mut [&mut left, &mut right]),
            Err(CompressorError::InvalidBlock)
        );
        assert_eq!(left, [0.3; 2]);
        let mut too_long = vec![0.1; (MAX_COMPRESSOR_FRAMES + 1) * 2];
        assert_eq!(
            processor.process_interleaved(&mut too_long),
            Err(CompressorError::InvalidBlock)
        );
        let mut expected = before;
        let mut actual = [0.3; 128];
        let mut oracle = actual;
        processor.process_interleaved(&mut actual).unwrap();
        expected.process_interleaved(&mut oracle).unwrap();
        assert_eq!(actual, oracle);
        assert_eq!(processor.gain_reduction_db(), expected.gain_reduction_db());
        assert!(
            std::mem::size_of::<Compressor>() <= 128,
            "processor retains only constant scalar state"
        );
    }
}
