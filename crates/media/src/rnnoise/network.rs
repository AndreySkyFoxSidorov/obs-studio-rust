//! Scalar transcription of RNNoise rnn.c (Octasic / Jean-Marc Valin).
//! Full original notices: assets/licenses/RNNOISE.txt.

use super::weights::*;

pub(super) const FEATURES: usize = 42;
pub(super) const BANDS: usize = 22;

#[derive(Clone)]
pub(super) struct Network {
    vad: [f32; 24],
    noise: [f32; 48],
    denoise: [f32; 96],
}

impl Network {
    pub(super) fn new() -> Self {
        Self { vad: [0.; 24], noise: [0.; 48], denoise: [0.; 96] }
    }

    pub(super) fn infer(&mut self, input: &[f32; FEATURES]) -> ([f32; BANDS], f32) {
        let mut dense_out = [0.; 24];
        dense(&INPUT_DENSE_WEIGHTS, &INPUT_DENSE_BIAS, input, &mut dense_out, Activation::Tanh);
        gru(&VAD_GRU_WEIGHTS, &VAD_GRU_RECURRENT_WEIGHTS, &VAD_GRU_BIAS,
            &dense_out, &mut self.vad);
        let mut vad = [0.; 1];
        dense(&VAD_OUTPUT_WEIGHTS, &VAD_OUTPUT_BIAS, &self.vad, &mut vad, Activation::Sigmoid);
        let mut noise_input = [0.; 90];
        noise_input[..24].copy_from_slice(&dense_out);
        noise_input[24..48].copy_from_slice(&self.vad);
        noise_input[48..].copy_from_slice(input);
        gru(&NOISE_GRU_WEIGHTS, &NOISE_GRU_RECURRENT_WEIGHTS, &NOISE_GRU_BIAS,
            &noise_input, &mut self.noise);
        let mut denoise_input = [0.; 114];
        denoise_input[..24].copy_from_slice(&self.vad);
        denoise_input[24..72].copy_from_slice(&self.noise);
        denoise_input[72..].copy_from_slice(input);
        gru(&DENOISE_GRU_WEIGHTS, &DENOISE_GRU_RECURRENT_WEIGHTS, &DENOISE_GRU_BIAS,
            &denoise_input, &mut self.denoise);
        let mut gains = [0.; BANDS];
        dense(&DENOISE_OUTPUT_WEIGHTS, &DENOISE_OUTPUT_BIAS, &self.denoise,
            &mut gains, Activation::Sigmoid);
        (gains, vad[0])
    }

    pub(super) fn finite(&self) -> bool {
        self.vad.iter().chain(&self.noise).chain(&self.denoise).all(|x| x.is_finite())
    }
}

#[derive(Clone, Copy)]
enum Activation { Tanh, Sigmoid }

// All shapes and the exact input-major / three-gate weight layout are private,
// fixed by weights.rs. No caller-supplied matrices or model parser are accepted.
fn dense(weights: &[i8], bias: &[i8], input: &[f32], output: &mut [f32], activation: Activation) {
    let n = output.len();
    for (i, out) in output.iter_mut().enumerate() {
        let mut sum = f32::from(bias[i]);
        for (j, &value) in input.iter().enumerate() {
            sum += f32::from(weights[j * n + i]) * value;
        }
        let value = (1. / 256.) * sum;
        *out = match activation {
            Activation::Tanh => tansig(value),
            Activation::Sigmoid => sigmoid(value),
        };
    }
}

fn gru(weights: &[i8], recurrent: &[i8], bias: &[i8], input: &[f32], state: &mut [f32]) {
    let n = state.len();
    let stride = 3 * n;
    let mut z = [0.; 96];
    let mut r = [0.; 96];
    let mut h = [0.; 96];
    for gate in 0..2 {
        for i in 0..n {
            let mut sum = f32::from(bias[gate * n + i]);
            for (j, &value) in input.iter().enumerate() {
                sum += f32::from(weights[gate * n + j * stride + i]) * value;
            }
            for (j, &value) in state.iter().enumerate() {
                sum += f32::from(recurrent[gate * n + j * stride + i]) * value;
            }
            let value = sigmoid((1. / 256.) * sum);
            if gate == 0 { z[i] = value; } else { r[i] = value; }
        }
    }
    for i in 0..n {
        let mut sum = f32::from(bias[2 * n + i]);
        for (j, &value) in input.iter().enumerate() {
            sum += f32::from(weights[2 * n + j * stride + i]) * value;
        }
        for (j, &value) in state.iter().enumerate() {
            // Original reset-before GRU: r[j], not r[i], and bias is inside
            // the one 1/256 scaling. Replacing this with a library GRU changes it.
            sum += f32::from(recurrent[2 * n + j * stride + i]) * value * r[j];
        }
        // Every GRU in rnnoise_model_orig has the RELU candidate activation.
        let value = (1. / 256.) * sum;
        let candidate = if value < 0. { 0. } else { value };
        h[i] = z[i] * state[i] + (1. - z[i]) * candidate;
    }
    state.copy_from_slice(&h[..n]);
}

fn tansig(mut x: f32) -> f32 {
    // Reversed comparisons are intentional; preserve original NaN behavior.
    if !(x < 8.) { return 1.; }
    if !(x > -8.) { return -1.; }
    if x.is_nan() { return 0.; }
    let sign = if x < 0. { x = -x; -1. } else { 1. };
    let index = (0.5_f32 + 25. * x).floor() as usize;
    x -= 0.04_f32 * index as f32;
    let y = TANSIG[index];
    let dy = 1. - y * y;
    sign * (y + x * dy * (1. - y * x))
}

fn sigmoid(x: f32) -> f32 {
    // The C literals .5 and .5 are double, unlike its .5f input multiply.
    (0.5_f64 + 0.5 * f64::from(tansig(0.5_f32 * x))) as f32
}
