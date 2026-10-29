// Adapted from FilmCraft, copyright (c) 2026 ArtCraft Team and contributors.
// MIT-licensed source; preserve LICENSE-MIT, NOTICE and SOURCE.json with this code.
use std::{error::Error, f64::consts::PI, fmt};

pub const MAX_EQ_FRAMES: usize = 4096;
const COEFFICIENT_INTERVAL: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EqError {
    InvalidParameter(&'static str),
    InvalidFormat,
    InvalidBlock,
    InvalidSample,
}
impl fmt::Display for EqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameter(name) => write!(f, "invalid EQ parameter: {name}"),
            Self::InvalidFormat => f.write_str("EQ requires 8000..192000 Hz and 1..8 channels"),
            Self::InvalidBlock => f.write_str("EQ block shape or frame count is invalid"),
            Self::InvalidSample => {
                f.write_str("EQ sample or resulting output is not representable as finite f32")
            }
        }
    }
}
impl Error for EqError {}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimpleEqParameters {
    low_db: f32,
    low_freq_hz: f32,
    mid_db: f32,
    mid_freq_hz: f32,
    mid_q: f32,
    high_db: f32,
    high_freq_hz: f32,
    output_db: f32,
}
impl Default for SimpleEqParameters {
    fn default() -> Self {
        Self {
            low_db: 0.0,
            low_freq_hz: 200.0,
            mid_db: 0.0,
            mid_freq_hz: 1000.0,
            mid_q: 0.7,
            high_db: 0.0,
            high_freq_hz: 5000.0,
            output_db: 0.0,
        }
    }
}
impl SimpleEqParameters {
    #[allow(clippy::too_many_arguments)] // Exact audited three-band parameter set.
    pub fn new(
        low_db: f32,
        low_freq_hz: f32,
        mid_db: f32,
        mid_freq_hz: f32,
        mid_q: f32,
        high_db: f32,
        high_freq_hz: f32,
        output_db: f32,
    ) -> Result<Self, EqError> {
        for (name, value, min, max) in [
            ("low_db", low_db, -24.0, 24.0),
            ("low_freq_hz", low_freq_hz, 20.0, 1000.0),
            ("mid_db", mid_db, -24.0, 24.0),
            ("mid_freq_hz", mid_freq_hz, 100.0, 10000.0),
            ("mid_q", mid_q, 0.1, 10.0),
            ("high_db", high_db, -24.0, 24.0),
            ("high_freq_hz", high_freq_hz, 1000.0, 20000.0),
            ("output_db", output_db, -24.0, 24.0),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(EqError::InvalidParameter(name));
            }
        }
        Ok(Self {
            low_db,
            low_freq_hz,
            mid_db,
            mid_freq_hz,
            mid_q,
            high_db,
            high_freq_hz,
            output_db,
        })
    }
    pub fn low_db(self) -> f32 {
        self.low_db
    }
    pub fn low_freq_hz(self) -> f32 {
        self.low_freq_hz
    }
    pub fn mid_db(self) -> f32 {
        self.mid_db
    }
    pub fn mid_freq_hz(self) -> f32 {
        self.mid_freq_hz
    }
    pub fn mid_q(self) -> f32 {
        self.mid_q
    }
    pub fn high_db(self) -> f32 {
        self.high_db
    }
    pub fn high_freq_hz(self) -> f32 {
        self.high_freq_hz
    }
    pub fn output_db(self) -> f32 {
        self.output_db
    }
}

/// Three fixed RBJ bands with 30ms ramps and f64 TDF-II state. The parameter
/// schema is separate from this derived stream state. No heap allocation occurs
/// in construction, processing, reset, parameter changes or response queries.
#[derive(Clone, Debug, PartialEq)]
pub struct SimpleEq {
    parameters: SimpleEqParameters,
    sample_rate: u32,
    channels: u16,
    bands: [Band; 3],
    state: [[Biquad; 3]; 8],
    output: Smoothed,
    counter: u32,
}
impl SimpleEq {
    pub fn new(
        sample_rate: u32,
        channels: u16,
        parameters: SimpleEqParameters,
    ) -> Result<Self, EqError> {
        if !(8000..=192000).contains(&sample_rate) || !(1..=8).contains(&channels) {
            return Err(EqError::InvalidFormat);
        }
        let mut eq = Self {
            parameters,
            sample_rate,
            channels,
            bands: std::array::from_fn(|index| Band::new(index, sample_rate)),
            state: [[Biquad::default(); 3]; 8],
            output: Smoothed::new(1.0, sample_rate),
            counter: 0,
        };
        eq.set_parameters(parameters);
        eq.reset();
        Ok(eq)
    }
    pub fn parameters(&self) -> SimpleEqParameters {
        self.parameters
    }
    pub fn set_parameters(&mut self, parameters: SimpleEqParameters) {
        self.parameters = parameters;
        let q = std::f32::consts::FRAC_1_SQRT_2;
        for (band, (frequency, gain, q)) in self.bands.iter_mut().zip([
            (parameters.low_freq_hz, parameters.low_db, q),
            (parameters.mid_freq_hz, parameters.mid_db, parameters.mid_q),
            (parameters.high_freq_hz, parameters.high_db, q),
        ]) {
            band.frequency.set(frequency.max(1.0).log2());
            band.gain.set(gain);
            band.q.set(q);
            band.dirty = true;
        }
        self.output.set(10.0f32.powf(parameters.output_db / 20.0));
    }
    pub fn latency_frames(&self) -> usize {
        0
    }
    pub fn reset(&mut self) {
        for band in &mut self.bands {
            band.frequency.snap();
            band.gain.snap();
            band.q.snap();
            band.update(self.sample_rate);
        }
        self.output.snap();
        self.state = [[Biquad::default(); 3]; 8];
        self.counter = 0;
    }
    /// Target magnitude curve, including output gain. Values above Nyquist are
    /// rejected; authored band centers retain the source's 0.49*rate clamp.
    pub fn response_db(&self, frequency_hz: f64) -> Result<f64, EqError> {
        if !frequency_hz.is_finite()
            || frequency_hz < 0.0
            || frequency_hz > f64::from(self.sample_rate) / 2.0
        {
            return Err(EqError::InvalidParameter("frequency_hz"));
        }
        let mut db = f64::from(self.parameters.output_db);
        for band in &self.bands {
            let coefficients = Coefficients::design(
                band.kind,
                self.sample_rate,
                2.0f64.powf(f64::from(band.frequency.target())),
                f64::from(band.q.target()),
                f64::from(band.gain.target()),
            );
            db += coefficients.magnitude_db(frequency_hz, self.sample_rate);
        }
        Ok(db)
    }
    /// Preflight a complete block on an inline candidate before publishing any
    /// output or state. The second bounded pass avoids a scratch PCM allocation
    /// and guarantees a rejected overflowing sample cannot poison a later block.
    pub fn process_planar(&mut self, channels: &mut [&mut [f32]]) -> Result<(), EqError> {
        let frames = channels.first().map_or(0, |channel| channel.len());
        if channels.len() != usize::from(self.channels)
            || frames > MAX_EQ_FRAMES
            || channels.iter().any(|channel| channel.len() != frames)
        {
            return Err(EqError::InvalidBlock);
        }
        if channels
            .iter()
            .flat_map(|channel| channel.iter())
            .any(|sample| !sample.is_finite())
        {
            return Err(EqError::InvalidSample);
        }
        let mut candidate = self.clone();
        for frame in 0..frames {
            let gain = candidate.next_frame();
            for (index, channel) in channels.iter().enumerate() {
                candidate.sample(index, channel[frame], gain)?;
            }
        }
        for frame in 0..frames {
            let gain = self.next_frame();
            for (index, channel) in channels.iter_mut().enumerate() {
                channel[frame] = self.sample_unchecked(index, channel[frame], gain);
            }
        }
        Ok(())
    }
    pub fn process_interleaved(&mut self, samples: &mut [f32]) -> Result<(), EqError> {
        let channels = usize::from(self.channels);
        if samples.len() % channels != 0 || samples.len() / channels > MAX_EQ_FRAMES {
            return Err(EqError::InvalidBlock);
        }
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(EqError::InvalidSample);
        }
        let mut candidate = self.clone();
        for frame in samples.chunks_exact(channels) {
            let gain = candidate.next_frame();
            for (index, &sample) in frame.iter().enumerate() {
                candidate.sample(index, sample, gain)?;
            }
        }
        for frame in samples.chunks_exact_mut(channels) {
            let gain = self.next_frame();
            for (index, sample) in frame.iter_mut().enumerate() {
                *sample = self.sample_unchecked(index, *sample, gain);
            }
        }
        Ok(())
    }
    fn next_frame(&mut self) -> f32 {
        if self.counter == 0 {
            for band in &mut self.bands {
                if band.dirty {
                    band.update(self.sample_rate);
                }
            }
        }
        self.counter = (self.counter + 1) % COEFFICIENT_INTERVAL;
        for band in &mut self.bands {
            band.frequency.tick();
            band.gain.tick();
            band.q.tick();
            if band.frequency.smoothing() || band.gain.smoothing() || band.q.smoothing() {
                band.dirty = true;
            }
        }
        self.output.tick()
    }
    fn sample_unchecked(&mut self, channel: usize, sample: f32, gain: f32) -> f32 {
        let mut value = f64::from(sample);
        for (band, state) in self.bands.iter().zip(&mut self.state[channel]) {
            // Source band mixes are always one for SimpleEq. Retain its exact
            // subtraction/addition order instead of replacing with value=y.
            let filtered = state.tick(band.coefficients, value);
            value += filtered - value;
        }
        (value * f64::from(gain)) as f32
    }
    fn sample(&mut self, channel: usize, sample: f32, gain: f32) -> Result<(), EqError> {
        let output = self.sample_unchecked(channel, sample, gain);
        if !output.is_finite()
            || self.state[channel]
                .iter()
                .any(|state| !state.s1.is_finite() || !state.s2.is_finite())
        {
            return Err(EqError::InvalidSample);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    LowShelf,
    Peaking,
    HighShelf,
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Band {
    kind: Kind,
    frequency: Smoothed,
    gain: Smoothed,
    q: Smoothed,
    coefficients: Coefficients,
    dirty: bool,
}
impl Band {
    fn new(index: usize, sample_rate: u32) -> Self {
        Self {
            kind: [Kind::LowShelf, Kind::Peaking, Kind::HighShelf][index],
            frequency: Smoothed::new(1000.0f32.log2(), sample_rate),
            gain: Smoothed::new(0.0, sample_rate),
            q: Smoothed::new(1.0, sample_rate),
            coefficients: Coefficients {
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
                a1: 0.0,
                a2: 0.0,
            },
            dirty: true,
        }
    }
    fn update(&mut self, sample_rate: u32) {
        self.coefficients = Coefficients::design(
            self.kind,
            sample_rate,
            2.0f64.powf(f64::from(self.frequency.value())),
            f64::from(self.q.value()),
            f64::from(self.gain.value()),
        );
        self.dirty = self.frequency.smoothing() || self.gain.smoothing() || self.q.smoothing();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Smoothed {
    current: f64,
    target: f64,
    step: f64,
    remaining: u32,
    ramp: u32,
}
impl Smoothed {
    fn new(value: f32, sample_rate: u32) -> Self {
        Self {
            current: f64::from(value),
            target: f64::from(value),
            step: 0.0,
            remaining: 0,
            ramp: (sample_rate as f32 * 30.0 / 1000.0).round() as u32,
        }
    }
    fn set(&mut self, value: f32) {
        let value = f64::from(value);
        if value == self.target {
            return;
        }
        self.target = value;
        self.remaining = self.ramp;
        self.step = (self.target - self.current) / f64::from(self.ramp);
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
        self.value()
    }
    fn value(self) -> f32 {
        self.current as f32
    }
    fn target(self) -> f32 {
        self.target as f32
    }
    fn smoothing(self) -> bool {
        self.remaining > 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Coefficients {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}
impl Coefficients {
    fn normalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }
    fn design(kind: Kind, sample_rate: u32, frequency: f64, q: f64, gain_db: f64) -> Self {
        let frequency = frequency.clamp(1.0, f64::from(sample_rate) * 0.49);
        let w = 2.0 * PI * frequency / f64::from(sample_rate);
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q.max(1e-3));
        let a = 10.0f64.powf(gain_db / 40.0);
        let k = 2.0 * a.sqrt() * alpha;
        match kind {
            Kind::Peaking => Self::normalized(
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            Kind::LowShelf => Self::normalized(
                a * ((a + 1.0) - (a - 1.0) * cos + k),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - k),
                (a + 1.0) + (a - 1.0) * cos + k,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - k,
            ),
            Kind::HighShelf => Self::normalized(
                a * ((a + 1.0) + (a - 1.0) * cos + k),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - k),
                (a + 1.0) - (a - 1.0) * cos + k,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - k,
            ),
        }
    }
    fn magnitude_db(self, frequency: f64, sample_rate: u32) -> f64 {
        let w = 2.0 * PI * frequency / f64::from(sample_rate);
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        20.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt().log10()
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Biquad {
    s1: f64,
    s2: f64,
}
impl Biquad {
    fn tick(&mut self, c: Coefficients, x: f64) -> f64 {
        let y = c.b0 * x + self.s1;
        self.s1 = flush(c.b1 * x - c.a1 * y + self.s2);
        self.s2 = flush(c.b2 * x - c.a2 * y);
        y
    }
}
fn flush(value: f64) -> f64 {
    if value.abs() < 1e-30 { 0.0 } else { value }
}

#[cfg(test)]
#[path = "eq_tests.rs"]
mod tests;
