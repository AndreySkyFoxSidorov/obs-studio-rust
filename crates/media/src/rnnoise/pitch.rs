//! Floating-point RNNoise pitch.c / celt_lpc.c (Xiph / Jean-Marc Valin).
//! Full upstream notices: assets/licenses/RNNOISE.txt.

pub(super) const PITCH_BUFFER: usize = 1728;
const HALF_BUFFER: usize = PITCH_BUFFER / 2;
const MAX_PERIOD: usize = 768;
const MIN_PERIOD: usize = 60;
const PITCH_FRAME: usize = 960;
const SECOND_CHECK: [usize; 16] = [0, 0, 3, 2, 3, 2, 5, 2, 3, 2, 3, 2, 5, 2, 3, 2];

pub(super) fn estimate(input: &[f32; PITCH_BUFFER], previous: usize, gain: f32) -> (usize, f32) {
    let mut down = [0.; HALF_BUFFER];
    downsample(input, &mut down);
    let candidate = MAX_PERIOD - search(&down[MAX_PERIOD / 2..], &down);
    remove_doubling(&down, candidate, previous, gain)
}

fn dot(x: &[f32], y: &[f32], count: usize) -> f32 {
    let mut sum = 0.;
    for i in 0..count { sum += x[i] * y[i]; }
    sum
}

fn downsample(x: &[f32; PITCH_BUFFER], out: &mut [f32; HALF_BUFFER]) {
    for i in 1..HALF_BUFFER {
        out[i] = 0.5 * (0.5 * (x[2 * i - 1] + x[2 * i + 1]) + x[2 * i]);
    }
    out[0] = 0.5 * (0.5 * x[1] + x[0]);
    let mut ac = [0.; 5];
    // Preserve the original autocorr's split fast-prefix + lag-tail sums.
    let fast = HALF_BUFFER - 4;
    for (k, value) in ac.iter_mut().enumerate() {
        *value = dot(&out[..], &out[k..], fast);
        let mut tail = 0.;
        for i in k + fast..HALF_BUFFER { tail += out[i] * out[i - k]; }
        *value += tail;
    }
    ac[0] *= 1.0001_f32;
    for (i, value) in ac.iter_mut().enumerate().skip(1) {
        *value -= *value * (0.008_f32 * i as f32) * (0.008_f32 * i as f32);
    }
    let mut lpc = lpc(&ac);
    let mut attenuation = 1.;
    for value in &mut lpc {
        attenuation = 0.9_f32 * attenuation;
        *value *= attenuation;
    }
    let coeffs = [lpc[0] + 0.8, lpc[1] + 0.8 * lpc[0],
        lpc[2] + 0.8 * lpc[1], lpc[3] + 0.8 * lpc[2], 0.8 * lpc[3]];
    let mut memory = [0.; 5];
    for value in out {
        let input = *value;
        let mut sum = input;
        for k in 0..5 { sum += coeffs[k] * memory[k]; }
        memory.copy_within(..4, 1);
        memory[0] = input;
        *value = sum;
    }
}

fn lpc(ac: &[f32; 5]) -> [f32; 4] {
    let mut result = [0.; 4];
    let mut error = ac[0];
    if ac[0] != 0. {
        for i in 0..4 {
            let mut rr = 0.;
            for j in 0..i { rr += result[j] * ac[i - j]; }
            rr += ac[i + 1];
            let r = -rr / error;
            result[i] = r;
            for j in 0..(i + 1) / 2 {
                let first = result[j];
                let second = result[i - 1 - j];
                result[j] = first + r * second;
                result[i - 1 - j] = second + r * first;
            }
            error = error - (r * r) * error;
            if error < 0.001_f32 * ac[0] { break; }
        }
    }
    result
}

fn best_pitch(correlation: &[f32], y: &[f32], length: usize) -> [usize; 2] {
    let mut energy = 1.;
    for &value in &y[..length] { energy += value * value; }
    let mut nums = [-1.; 2];
    let mut dens = [0.; 2];
    let mut best = [0, 1];
    for (i, &corr) in correlation.iter().enumerate() {
        if corr > 0. {
            let scaled = corr * 1e-12_f32;
            let num = scaled * scaled;
            if num * dens[1] > nums[1] * energy {
                if num * dens[0] > nums[0] * energy {
                    nums[1] = nums[0]; dens[1] = dens[0]; best[1] = best[0];
                    nums[0] = num; dens[0] = energy; best[0] = i;
                } else {
                    nums[1] = num; dens[1] = energy; best[1] = i;
                }
            }
        }
        energy += y[i + length] * y[i + length] - y[i] * y[i];
        if energy < 1. { energy = 1.; }
    }
    best
}

fn interpolation(a: f32, b: f32, c: f32) -> isize {
    if c - a > 0.7_f32 * (b - a) { 1 }
    else if a - c > 0.7_f32 * (b - c) { -1 }
    else { 0 }
}

fn search(x: &[f32], y: &[f32; HALF_BUFFER]) -> usize {
    const SEARCH: usize = MAX_PERIOD - 3 * MIN_PERIOD; // 588
    let mut x4 = [0.; PITCH_FRAME / 4];
    let mut y4 = [0.; (PITCH_FRAME + SEARCH) / 4];
    let mut correlations = [0.; SEARCH / 2];
    for (j, value) in x4.iter_mut().enumerate() { *value = x[2 * j]; }
    for (j, value) in y4.iter_mut().enumerate() { *value = y[2 * j]; }
    for (i, value) in correlations[..SEARCH / 4].iter_mut().enumerate() {
        *value = dot(&x4, &y4[i..], PITCH_FRAME / 4);
    }
    let coarse = best_pitch(&correlations[..SEARCH / 4], &y4, PITCH_FRAME / 4);
    for (i, value) in correlations.iter_mut().enumerate() {
        *value = 0.;
        if i.abs_diff(2 * coarse[0]) > 2 && i.abs_diff(2 * coarse[1]) > 2 { continue; }
        let sum = dot(x, &y[i..], PITCH_FRAME / 2);
        *value = if sum > -1. { sum } else { -1. };
    }
    let best = best_pitch(&correlations, y, PITCH_FRAME / 2)[0];
    let offset = if best > 0 && best < SEARCH / 2 - 1 {
        interpolation(correlations[best - 1], correlations[best], correlations[best + 1])
    } else { 0 };
    ((2 * best) as isize - offset) as usize
}

fn pitch_gain(xy: f32, xx: f32, yy: f32) -> f32 {
    // xx*yy is float before the C double addition/sqrt/division.
    (f64::from(xy) / (1. + f64::from(xx * yy)).sqrt()) as f32
}

fn remove_doubling(x: &[f32; HALF_BUFFER], candidate: usize, previous: usize,
    previous_gain: f32) -> (usize, f32)
{
    const MAX: usize = MAX_PERIOD / 2;
    const MIN: usize = MIN_PERIOD / 2;
    const N: usize = PITCH_FRAME / 2;
    let t0 = (candidate / 2).min(MAX - 1);
    let mut t = t0;
    let xx = dot(&x[MAX..], &x[MAX..], N);
    let mut xy = dot(&x[MAX..], &x[MAX - t0..], N);
    let mut lookup = [0.; MAX + 1];
    lookup[0] = xx;
    let mut yy = xx;
    for i in 1..=MAX {
        yy = yy + x[MAX - i] * x[MAX - i] - x[MAX + N - i] * x[MAX + N - i];
        lookup[i] = if yy > 0. { yy } else { 0. };
    }
    yy = lookup[t0];
    let mut best_xy = xy;
    let mut best_yy = yy;
    let g0 = pitch_gain(xy, xx, yy);
    let mut gain = g0;
    for (k, &second) in SECOND_CHECK.iter().enumerate().skip(2) {
        let t1 = (2 * t0 + k) / (2 * k);
        if t1 < MIN { break; }
        let t1b = if k == 2 {
            if t1 + t0 > MAX { t0 } else { t0 + t1 }
        } else { (2 * second * t0 + k) / (2 * k) };
        let first = dot(&x[MAX..], &x[MAX - t1..], N);
        let other = dot(&x[MAX..], &x[MAX - t1b..], N);
        xy = 0.5 * (first + other);
        yy = 0.5 * (lookup[t1] + lookup[t1b]);
        let g1 = pitch_gain(xy, xx, yy);
        let continuity = if t1.abs_diff(previous / 2) <= 1 { previous_gain }
            else if t1.abs_diff(previous / 2) <= 2 && 5 * k * k < t0 { 0.5 * previous_gain }
            else { 0. };
        let mut threshold = 0.3_f32.max(0.7_f32 * g0 - continuity);
        // Keep the original branch order, including its else-if condition.
        if t1 < 3 * MIN { threshold = 0.4_f32.max(0.85_f32 * g0 - continuity); }
        else if t1 < 2 * MIN { threshold = 0.5_f32.max(0.9_f32 * g0 - continuity); }
        if g1 > threshold { best_xy = xy; best_yy = yy; t = t1; gain = g1; }
    }
    if best_xy < 0. { best_xy = 0.; }
    let pg = if best_yy <= best_xy { 1. } else { best_xy / (best_yy + 1.) };
    let correlations = [dot(&x[MAX..], &x[MAX - (t - 1)..], N),
        dot(&x[MAX..], &x[MAX - t..], N), dot(&x[MAX..], &x[MAX - (t + 1)..], N)];
    let offset = interpolation(correlations[0], correlations[1], correlations[2]);
    let period = ((2 * t) as isize + offset).max(MIN_PERIOD as isize) as usize;
    (period, if pg > gain { gain } else { pg })
}
