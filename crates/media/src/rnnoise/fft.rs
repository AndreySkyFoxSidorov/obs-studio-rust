//! Fixed-960 float KISS/Opus FFT transcription (Mark Borgerding / Xiph / Valin).
//! Source notices: assets/licenses/RNNOISE.txt. No dynamic plans or SIMD FFI.

pub(super) const WINDOW: usize = 960;
pub(super) const FREQ: usize = 481;

#[derive(Clone, Copy, Default)]
pub(super) struct Complex { pub r: f32, pub i: f32 }
impl Complex {
    fn add(self, b: Self) -> Self { Self { r: self.r + b.r, i: self.i + b.i } }
    fn sub(self, b: Self) -> Self { Self { r: self.r - b.r, i: self.i - b.i } }
    fn mul(self, b: Self) -> Self {
        Self { r: self.r * b.r - self.i * b.i, i: self.r * b.i + self.i * b.r }
    }
    fn scale(self, s: f32) -> Self { Self { r: self.r * s, i: self.i * s } }
}

#[derive(Clone)]
pub(super) struct Fft {
    twiddles: [Complex; WINDOW],
    bitrev: [u16; WINDOW],
}

impl Fft {
    pub(super) fn new() -> Self {
        let mut result = Self { twiddles: [Complex::default(); WINDOW], bitrev: [0; WINDOW] };
        for (i, value) in result.twiddles.iter_mut().enumerate() {
            let phase = (-2. * std::f64::consts::PI / WINDOW as f64) * i as f64;
            *value = Complex { r: phase.cos() as f32, i: phase.sin() as f32 };
        }
        // kf_factor(960) reverses [4,4,4,3,5] to [5,3,4,4,4].
        // Static factorization removes input-dependent plans/recursion.
        for source in 0..WINDOW {
            let mut digits = source;
            let mut destination = 0;
            let mut remaining = WINDOW;
            for radix in [5, 3, 4, 4, 4] {
                remaining /= radix;
                destination += (digits % radix) * remaining;
                digits /= radix;
            }
            result.bitrev[source] = destination as u16;
        }
        result
    }

    fn transform(&self, input: &[Complex; WINDOW], out: &mut [Complex; WINDOW]) {
        let scale = 1_f32 / WINDOW as f32;
        for (i, x) in input.iter().copied().enumerate() {
            out[usize::from(self.bitrev[i])] = x.scale(scale);
        }
        // Original reverse-stage butterfly order, including degenerate radix4.
        for (m, groups, group_stride) in [(1, 240, 4), (4, 60, 16), (16, 15, 64)] {
            self.butterfly4(out, m, groups, group_stride);
        }
        self.butterfly3(out, 64, 5, 192);
        self.butterfly5(out, 192, 1, 1);
    }

    pub(super) fn forward(&self, input: &[f32; WINDOW], out: &mut [Complex; FREQ]) {
        let mut x = [Complex::default(); WINDOW];
        let mut y = [Complex::default(); WINDOW];
        for (to, &value) in x.iter_mut().zip(input) { to.r = value; }
        self.transform(&x, &mut y);
        out.copy_from_slice(&y[..FREQ]);
    }

    pub(super) fn inverse(&self, input: &[Complex; FREQ], out: &mut [f32; WINDOW]) {
        let mut x = [Complex::default(); WINDOW];
        let mut y = [Complex::default(); WINDOW];
        x[..FREQ].copy_from_slice(input);
        for i in FREQ..WINDOW {
            x[i] = Complex { r: x[WINDOW - i].r, i: -x[WINDOW - i].i };
        }
        self.transform(&x, &mut y);
        out[0] = WINDOW as f32 * y[0].r;
        for i in 1..WINDOW { out[i] = WINDOW as f32 * y[WINDOW - i].r; }
    }

    fn butterfly4(&self, out: &mut [Complex; WINDOW], m: usize, groups: usize, mm: usize) {
        for group in 0..groups {
            let base = group * mm;
            if m == 1 {
                let s0 = out[base].sub(out[base + 2]);
                let a = out[base].add(out[base + 2]);
                let s1 = out[base + 1].add(out[base + 3]);
                out[base + 2] = a.sub(s1);
                out[base] = a.add(s1);
                let s1 = out[base + 1].sub(out[base + 3]);
                out[base + 1] = Complex { r: s0.r + s1.i, i: s0.i - s1.r };
                out[base + 3] = Complex { r: s0.r - s1.i, i: s0.i + s1.r };
            } else {
                for j in 0..m {
                    let i = base + j;
                    let s0 = out[i + m].mul(self.twiddles[j * groups]);
                    let s1 = out[i + 2 * m].mul(self.twiddles[j * groups * 2]);
                    let s2 = out[i + 3 * m].mul(self.twiddles[j * groups * 3]);
                    let s5 = out[i].sub(s1);
                    let a = out[i].add(s1);
                    let s3 = s0.add(s2);
                    let s4 = s0.sub(s2);
                    out[i + 2 * m] = a.sub(s3);
                    out[i] = a.add(s3);
                    out[i + m] = Complex { r: s5.r + s4.i, i: s5.i - s4.r };
                    out[i + 3 * m] = Complex { r: s5.r - s4.i, i: s5.i + s4.r };
                }
            }
        }
    }

    fn butterfly3(&self, out: &mut [Complex; WINDOW], m: usize, groups: usize, mm: usize) {
        let epi = self.twiddles[groups * m].i;
        for group in 0..groups {
            for j in 0..m {
                let i = group * mm + j;
                let s1 = out[i + m].mul(self.twiddles[j * groups]);
                let s2 = out[i + 2 * m].mul(self.twiddles[j * groups * 2]);
                let s3 = s1.add(s2);
                let s0 = s1.sub(s2).scale(epi);
                let a = out[i].sub(s3.scale(0.5));
                out[i] = out[i].add(s3);
                out[i + 2 * m] = Complex { r: a.r + s0.i, i: a.i - s0.r };
                out[i + m] = Complex { r: a.r - s0.i, i: a.i + s0.r };
            }
        }
    }

    fn butterfly5(&self, out: &mut [Complex; WINDOW], m: usize, groups: usize, mm: usize) {
        let ya = self.twiddles[groups * m];
        let yb = self.twiddles[groups * 2 * m];
        for group in 0..groups {
            for u in 0..m {
                let i = group * mm + u;
                let s0 = out[i];
                let s1 = out[i + m].mul(self.twiddles[u * groups]);
                let s2 = out[i + 2 * m].mul(self.twiddles[2 * u * groups]);
                let s3 = out[i + 3 * m].mul(self.twiddles[3 * u * groups]);
                let s4 = out[i + 4 * m].mul(self.twiddles[4 * u * groups]);
                let s7 = s1.add(s4);
                let s10 = s1.sub(s4);
                let s8 = s2.add(s3);
                let s9 = s2.sub(s3);
                out[i] = s0.add(s7.add(s8));
                let s5 = s0.add(s7.scale(ya.r).add(s8.scale(yb.r)));
                let s6 = Complex { r: s10.i * ya.i + s9.i * yb.i,
                    i: -(s10.r * ya.i + s9.r * yb.i) };
                out[i + m] = s5.sub(s6);
                out[i + 4 * m] = s5.add(s6);
                let s11 = s0.add(s7.scale(yb.r).add(s8.scale(ya.r)));
                let s12 = Complex { r: s9.i * ya.i - s10.i * yb.i,
                    i: s10.r * yb.i - s9.r * ya.i };
                out[i + 2 * m] = s11.add(s12);
                out[i + 3 * m] = s11.sub(s12);
            }
        }
    }
}
