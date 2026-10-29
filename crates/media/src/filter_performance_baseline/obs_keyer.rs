// Frozen original arithmetic and three-row sampler; never optimize this oracle.
use super::baseline_color::{decode, encode, unorm};
use crate::{
    ChromaKeySettings, ChromaKeyType, ColorCorrectionInput, ColorKeySettings, ColorKeyType,
};
#[derive(Clone, Copy)]
pub(crate) struct PreparedKeyer {
    key: [f32; 3],
    chroma: [f32; 2],
    similarity: f32,
    smoothness: f32,
    spill: f32,
    opacity: f32,
    contrast: f32,
    brightness: f32,
    gamma: f32,
}
impl PreparedKeyer {
    fn curve(key: [u8; 3], similarity: u16, smoothness: u16, curve: (f32, f64, f32, f64)) -> Self {
        let (opacity, contrast, brightness, gamma) = curve;
        let key = key.map(|v| f32::from(v) / 255.0);
        Self {
            key,
            chroma: key_chroma(key),
            similarity: f32::from(similarity) / 1000.0,
            smoothness: f32::from(smoothness) / 1000.0,
            spill: 1.0,
            opacity,
            contrast: if contrast < 0.0 {
                1.0 / (1.0 - contrast)
            } else {
                contrast + 1.0
            } as f32,
            brightness,
            gamma: if gamma < 0.0 {
                1.0 - gamma
            } else {
                1.0 / (gamma + 1.0)
            } as f32,
        }
    }
    pub(crate) fn color(s: ColorKeySettings) -> Self {
        let key = match s.key_color_type {
            ColorKeyType::Green => [0, 255, 0],
            ColorKeyType::Blue => [0, 0, 255],
            ColorKeyType::Red => [255, 0, 0],
            ColorKeyType::Magenta => [255, 0, 255],
            ColorKeyType::Custom => s.key_color,
        };
        Self::curve(
            key,
            s.similarity,
            s.smoothness,
            (s.opacity, s.contrast, s.brightness, s.gamma),
        )
    }
    pub(crate) fn chroma(s: ChromaKeySettings) -> Self {
        let key = match s.key_color_type {
            ChromaKeyType::Green => [0, 255, 0],
            ChromaKeyType::Blue => [0, 153, 255],
            ChromaKeyType::Magenta => [255, 0, 255],
            ChromaKeyType::Custom => s.key_color,
        };
        let mut result = Self::curve(
            key,
            s.similarity,
            s.smoothness,
            (s.opacity, s.contrast, s.brightness, s.gamma),
        );
        result.spill = f32::from(s.spill) / 1000.0;
        result
    }
    pub(crate) fn pixel(self, pixel: [u8; 4], encoding: ColorCorrectionInput) -> [u8; 4] {
        let lp = sample_lp(pixel, encoding);
        let rgb = straight(lp);
        let encoded = rgb.map(nonlinear);
        let distance = ((encoded[0] - self.key[0]).powi(2)
            + (encoded[1] - self.key[1]).powi(2)
            + (encoded[2] - self.key[2]).powi(2))
        .sqrt();
        let mask = ((distance - self.similarity).max(0.0) / self.smoothness).clamp(0.0, 1.0);
        self.finish(rgb, (lp[3] * self.opacity) * mask)
    }
    fn distance(self, linear: [f32; 3]) -> f32 {
        let chroma = source_chroma(linear.map(nonlinear));
        ((chroma[0] - self.chroma[0]).powi(2) + (chroma[1] - self.chroma[1]).powi(2)).sqrt()
    }
    fn finish(self, rgb: [f32; 3], alpha: f32) -> [u8; 4] {
        [
            encode((rgb[0].powf(self.gamma) * self.contrast + self.brightness) * alpha),
            encode((rgb[1].powf(self.gamma) * self.contrast + self.brightness) * alpha),
            encode((rgb[2].powf(self.gamma) * self.contrast + self.brightness) * alpha),
            unorm(alpha),
        ]
    }
    fn keyed_chroma(self, lp: [f32; 4], distance: f32) -> [u8; 4] {
        let base = distance - self.similarity;
        let mask = (base / self.smoothness).clamp(0.0, 1.0).powf(1.5);
        let spill = (base / self.spill).clamp(0.0, 1.0).powf(1.5);
        let rgb = straight(lp);
        let gray = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        self.finish(
            rgb.map(|c| gray + (c - gray) * spill),
            (lp[3] * self.opacity) * mask,
        )
    }
}
fn straight(lp: [f32; 4]) -> [f32; 3] {
    if lp[3] > 0.0 {
        {
            let reciprocal = 1.0 / lp[3];
            [lp[0] * reciprocal, lp[1] * reciprocal, lp[2] * reciprocal]
        }
    } else {
        [0.0; 3]
    }
}
fn nonlinear(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}
fn key_chroma(c: [f32; 3]) -> [f32; 2] {
    [
        (c[0] * -0.100644 + c[2] * 0.439216) + (c[1] * -0.338572 + 0.501961),
        (c[0] * 0.439216 + c[2] * -0.040274) + (c[1] * -0.398942 + 0.501961),
    ]
}
fn source_chroma(c: [f32; 3]) -> [f32; 2] {
    [
        c[0] * -0.100644 + c[1] * -0.338572 + c[2] * 0.439216 + 0.501961,
        c[0] * 0.439216 + c[1] * -0.398942 + c[2] * -0.040274 + 0.501961,
    ]
}
pub(crate) fn sample_lp(pixel: [u8; 4], encoding: ColorCorrectionInput) -> [f32; 4] {
    let alpha = f32::from(pixel[3]) / 255.0;
    let mut result = [0.0, 0.0, 0.0, alpha];
    for i in 0..3 {
        result[i] = match encoding {
            ColorCorrectionInput::LinearAlpha => decode(encode(decode(pixel[i]) * alpha)),
            ColorCorrectionInput::EncodedAlpha => {
                decode(((u32::from(pixel[i]) * u32::from(pixel[3]) + 127) / 255) as u8)
            }
            ColorCorrectionInput::PremultipliedLinearSrgb => decode(pixel[i]),
        };
    }
    result
}
pub(crate) fn chroma_in_place(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    encoding: ColorCorrectionInput,
    prepared: PreparedKeyer,
    workspace: &mut [[f32; 4]],
) {
    fn load(
        pixels: &[u8],
        width: usize,
        y: usize,
        encoding: ColorCorrectionInput,
        row: &mut [[f32; 4]],
    ) {
        for (output, pixel) in row
            .iter_mut()
            .zip(pixels[y * width * 4..(y + 1) * width * 4].chunks_exact(4))
        {
            *output = sample_lp([pixel[0], pixel[1], pixel[2], pixel[3]], encoding);
        }
    }
    let mut slots = [0, 1, 2];
    for (slot, y) in [(0, 0), (1, 0), (2, 1.min(height - 1))] {
        load(
            pixels,
            width,
            y,
            encoding,
            &mut workspace[slot * width..(slot + 1) * width],
        );
    }
    for y in 0..height {
        for x in 0..width {
            let get = |slot: usize, x: usize| workspace[slots[slot] * width + x];
            let left = x.saturating_sub(1);
            let right = (x + 1).min(width - 1);
            let pair = |a: [f32; 4], b: [f32; 4]| {
                prepared.distance([
                    (a[0] + b[0]) * 0.5,
                    (a[1] + b[1]) * 0.5,
                    (a[2] + b[2]) * 0.5,
                ])
            };
            let mut distance = pair(get(0, left), get(1, left));
            distance += pair(get(1, right), get(2, right));
            distance += pair(get(2, left), get(2, x));
            distance += pair(get(0, x), get(0, right));
            distance *= 2.0;
            let lp = get(1, x);
            distance += prepared.distance(straight(lp));
            distance /= 9.0;
            pixels[(y * width + x) * 4..(y * width + x + 1) * 4]
                .copy_from_slice(&prepared.keyed_chroma(lp, distance));
        }
        if y + 1 < height {
            slots.rotate_left(1);
            let slot = slots[2];
            load(
                pixels,
                width,
                (y + 2).min(height - 1),
                encoding,
                &mut workspace[slot * width..(slot + 1) * width],
            );
        }
    }
}
