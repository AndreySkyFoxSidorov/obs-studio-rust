//! RNNoise inference frontend and synthesis, from the bundled denoise.c.
//! Copyright Jean-Marc Valin / Mozilla / Xiph; full notices in RNNOISE.txt.

use super::{FRAME_SIZE, fft::{Complex, Fft, FREQ, WINDOW}, network::{Network, BANDS, FEATURES}, pitch};

const CEPS_MEMORY: usize = 8;
const DELTA_CEPS: usize = 6;
const BANDS_5MS: [usize; BANDS] = [0,1,2,3,4,5,6,7,8,10,12,14,16,20,24,28,34,40,48,60,78,100];

#[derive(Clone)]
pub(super) struct Tables {
    fft: Fft,
    half_window: [f32; FRAME_SIZE],
    dct: [f32; BANDS * BANDS],
}

impl Tables {
    pub(super) fn new() -> Self {
        let mut result = Self { fft: Fft::new(), half_window: [0.; FRAME_SIZE], dct: [0.; BANDS * BANDS] };
        for (i, value) in result.half_window.iter_mut().enumerate() {
            let sine = (0.5 * std::f64::consts::PI * (i as f64 + 0.5) / FRAME_SIZE as f64).sin();
            *value = (0.5 * std::f64::consts::PI * sine * sine).sin() as f32;
        }
        for i in 0..BANDS {
            for j in 0..BANDS {
                let mut value = ((i as f64 + 0.5) * j as f64 * std::f64::consts::PI / BANDS as f64).cos() as f32;
                if j == 0 { value *= 0.5_f64.sqrt() as f32; }
                result.dct[i * BANDS + j] = value;
            }
        }
        result
    }

    fn window(&self, x: &mut [f32; WINDOW]) {
        for i in 0..FRAME_SIZE {
            x[i] *= self.half_window[i];
            x[WINDOW - 1 - i] *= self.half_window[i];
        }
    }

    fn dct(&self, input: &[f32; BANDS]) -> [f32; BANDS] {
        let mut result = [0.; BANDS];
        for (i, value) in result.iter_mut().enumerate() {
            let mut sum = 0.;
            for (j, &x) in input.iter().enumerate() { sum += x * self.dct[j * BANDS + i]; }
            *value = (f64::from(sum) * (2_f64 / 22.).sqrt()) as f32;
        }
        result
    }
}

#[derive(Clone)]
pub(super) struct State {
    analysis: [f32; FRAME_SIZE],
    cepstral: [[f32; BANDS]; CEPS_MEMORY],
    mem_id: usize,
    synthesis: [f32; FRAME_SIZE],
    pitch: [f32; pitch::PITCH_BUFFER],
    last_gain: f32,
    last_period: usize,
    hp: [f32; 2],
    last_gains: [f32; BANDS],
    network: Network,
}

impl State {
    pub(super) fn new() -> Self {
        Self { analysis: [0.; FRAME_SIZE], cepstral: [[0.; BANDS]; CEPS_MEMORY], mem_id: 0,
            synthesis: [0.; FRAME_SIZE], pitch: [0.; pitch::PITCH_BUFFER], last_gain: 0.,
            last_period: 0, hp: [0.; 2], last_gains: [0.; BANDS], network: Network::new() }
    }

    pub(super) fn process(&mut self, tables: &Tables, input: &[f32], out: &mut [f32; FRAME_SIZE])
        -> (f32, bool)
    {
        let mut x = [0.; FRAME_SIZE];
        // OBS presents normalized float audio; original RNNoise consumes PCM16-scale floats.
        for (i, value) in x.iter_mut().enumerate() {
            let xi = input[i] * 32768_f32;
            let yi = xi + self.hp[0];
            self.hp[0] = (f64::from(self.hp[1]) + (-2_f64 * f64::from(xi)
                - f64::from(-1.99599_f32) * f64::from(yi))) as f32;
            self.hp[1] = (f64::from(xi) - f64::from(0.99600_f32) * f64::from(yi)) as f32;
            *value = yi;
        }
        let mut spectrum = [Complex::default(); FREQ];
        let mut pitch_spectrum = [Complex::default(); FREQ];
        let mut energy = [0.; BANDS];
        let mut pitch_energy = [0.; BANDS];
        let mut correlation = [0.; BANDS];
        let mut features = [0.; FEATURES];
        let silence = self.features(tables, &x, &mut spectrum, &mut pitch_spectrum,
            &mut energy, &mut pitch_energy, &mut correlation, &mut features);
        let mut probability = 0.;
        if !silence {
            let (mut gains, vad) = self.network.infer(&features);
            probability = vad;
            pitch_filter(&mut spectrum, &pitch_spectrum, &energy, &pitch_energy, &correlation, &gains);
            for (i, value) in gains.iter_mut().enumerate() {
                let previous = 0.6_f32 * self.last_gains[i];
                if *value < previous { *value = previous; }
                self.last_gains[i] = *value;
            }
            let gains = interpolate(&gains);
            for (value, gain) in spectrum.iter_mut().zip(gains) { value.r *= gain; value.i *= gain; }
        }
        let mut synthesized = [0.; WINDOW];
        tables.fft.inverse(&spectrum, &mut synthesized);
        tables.window(&mut synthesized);
        for i in 0..FRAME_SIZE { out[i] = (synthesized[i] + self.synthesis[i]) / 32768_f32; }
        self.synthesis.copy_from_slice(&synthesized[FRAME_SIZE..]);
        (probability, silence)
    }

    pub(super) fn finite(&self) -> bool {
        self.analysis.iter().chain(self.cepstral.iter().flatten()).chain(&self.synthesis)
            .chain(&self.pitch).chain(&self.hp).chain(&self.last_gains).all(|v| v.is_finite())
            && self.last_gain.is_finite() && self.network.finite()
    }

    #[allow(clippy::too_many_arguments)]
    fn features(&mut self, tables: &Tables, input: &[f32; FRAME_SIZE],
        spectrum: &mut [Complex; FREQ], pitch_spectrum: &mut [Complex; FREQ],
        energy: &mut [f32; BANDS], pitch_energy: &mut [f32; BANDS],
        correlation: &mut [f32; BANDS], features: &mut [f32; FEATURES]) -> bool
    {
        let mut window = [0.; WINDOW];
        window[..FRAME_SIZE].copy_from_slice(&self.analysis);
        window[FRAME_SIZE..].copy_from_slice(input);
        self.analysis.copy_from_slice(input);
        tables.window(&mut window);
        tables.fft.forward(&window, spectrum);
        *energy = band_energy(spectrum);
        self.pitch.copy_within(FRAME_SIZE.., 0);
        let end = pitch::PITCH_BUFFER;
        self.pitch[end - FRAME_SIZE..].copy_from_slice(input);
        let (period, gain) = pitch::estimate(&self.pitch, self.last_period, self.last_gain);
        self.last_period = period;
        self.last_gain = gain;
        window.copy_from_slice(&self.pitch[end - WINDOW - period..end - period]);
        tables.window(&mut window);
        tables.fft.forward(&window, pitch_spectrum);
        *pitch_energy = band_energy(pitch_spectrum);
        *correlation = band_correlation(spectrum, pitch_spectrum);
        for i in 0..BANDS {
            correlation[i] = (f64::from(correlation[i])
                / (0.001_f64 + f64::from(energy[i] * pitch_energy[i])).sqrt()) as f32;
        }
        let tmp = tables.dct(correlation);
        features[BANDS + 2 * DELTA_CEPS..BANDS + 3 * DELTA_CEPS].copy_from_slice(&tmp[..DELTA_CEPS]);
        features[BANDS + 2 * DELTA_CEPS] -= 1.3_f32;
        features[BANDS + 2 * DELTA_CEPS + 1] -= 0.9_f32;
        features[BANDS + 3 * DELTA_CEPS] = (0.01_f64 * (period as f64 - 300.)) as f32;
        let mut logs = [0.; BANDS];
        let mut log_max = -2_f32;
        let mut follow = -2_f32;
        let mut total = 0.;
        for i in 0..BANDS {
            let value = (0.01_f64 + f64::from(energy[i])).log10() as f32;
            // Original MAX macros use a double follow-1.5 literal.
            logs[i] = f64::from(log_max - 7_f32).max((f64::from(follow) - 1.5).max(f64::from(value))) as f32;
            if logs[i] > log_max { log_max = logs[i]; }
            follow = (f64::from(follow) - 1.5).max(f64::from(logs[i])) as f32;
            total += energy[i];
        }
        if f64::from(total) < 0.04_f64 { features.fill(0.); return true; }
        let mut cepstral = tables.dct(&logs);
        cepstral[0] -= 12.; cepstral[1] -= 4.;
        features[..BANDS].copy_from_slice(&cepstral);
        let id = self.mem_id;
        self.cepstral[id] = cepstral;
        let previous = (id + CEPS_MEMORY - 1) % CEPS_MEMORY;
        let previous2 = (id + CEPS_MEMORY - 2) % CEPS_MEMORY;
        self.mem_id = (id + 1) % CEPS_MEMORY;
        for i in 0..DELTA_CEPS {
            features[i] = self.cepstral[id][i] + self.cepstral[previous][i] + self.cepstral[previous2][i];
            features[BANDS + i] = self.cepstral[id][i] - self.cepstral[previous2][i];
            features[BANDS + DELTA_CEPS + i] = self.cepstral[id][i] - 2. * self.cepstral[previous][i]
                + self.cepstral[previous2][i];
        }
        let mut variability = 0.;
        for i in 0..CEPS_MEMORY {
            let mut nearest = 1e15_f32;
            for j in 0..CEPS_MEMORY {
                let mut distance = 0.;
                for k in 0..BANDS {
                    let delta = self.cepstral[i][k] - self.cepstral[j][k];
                    distance += delta * delta;
                }
                if j != i && distance < nearest { nearest = distance; }
            }
            variability += nearest;
        }
        features[BANDS + 3 * DELTA_CEPS + 1] = (f64::from(variability / CEPS_MEMORY as f32) - 2.1) as f32;
        false
    }
}

fn band_energy(x: &[Complex; FREQ]) -> [f32; BANDS] {
    band_sum(|i| x[i].r * x[i].r + x[i].i * x[i].i)
}

fn band_correlation(x: &[Complex; FREQ], p: &[Complex; FREQ]) -> [f32; BANDS] {
    band_sum(|i| x[i].r * p[i].r + x[i].i * p[i].i)
}

fn band_sum(mut value: impl FnMut(usize) -> f32) -> [f32; BANDS] {
    let mut sum = [0.; BANDS];
    for band in 0..BANDS - 1 {
        let length = (BANDS_5MS[band + 1] - BANDS_5MS[band]) * 4;
        for j in 0..length {
            let fraction = j as f32 / length as f32;
            let v = value(BANDS_5MS[band] * 4 + j);
            sum[band] += (1. - fraction) * v;
            sum[band + 1] += fraction * v;
        }
    }
    sum[0] *= 2.; sum[BANDS - 1] *= 2.;
    sum
}

fn interpolate(bands: &[f32; BANDS]) -> [f32; FREQ] {
    let mut result = [0.; FREQ];
    for band in 0..BANDS - 1 {
        let length = (BANDS_5MS[band + 1] - BANDS_5MS[band]) * 4;
        for j in 0..length {
            let fraction = j as f32 / length as f32;
            result[BANDS_5MS[band] * 4 + j] = (1. - fraction) * bands[band] + fraction * bands[band + 1];
        }
    }
    // Original 20..24 kHz bins remain zero for a non-silent filtered frame.
    result
}

fn pitch_filter(x: &mut [Complex; FREQ], p: &[Complex; FREQ], energy: &[f32; BANDS],
    pitch_energy: &[f32; BANDS], correlation: &[f32; BANDS], gains: &[f32; BANDS])
{
    let mut bands = [0.; BANDS];
    for i in 0..BANDS {
        let c2 = correlation[i] * correlation[i];
        let g2 = gains[i] * gains[i];
        let ratio = if correlation[i] > gains[i] { 1. }
            else { (f64::from(c2 * (1. - g2)) / (0.001_f64 + f64::from(g2 * (1. - c2)))) as f32 };
        bands[i] = f64::from(ratio.clamp(0., 1.)).sqrt() as f32;
        bands[i] *= (f64::from(energy[i]) / (1e-8_f64 + f64::from(pitch_energy[i]))).sqrt() as f32;
    }
    let per_bin = interpolate(&bands);
    for i in 0..FREQ { x[i].r += per_bin[i] * p[i].r; x[i].i += per_bin[i] * p[i].i; }
    let new_energy = band_energy(x);
    for i in 0..BANDS {
        bands[i] = (f64::from(energy[i]) / (1e-8_f64 + f64::from(new_energy[i]))).sqrt() as f32;
    }
    let normalization = interpolate(&bands);
    for i in 0..FREQ { x[i].r *= normalization[i]; x[i].i *= normalization[i]; }
}
