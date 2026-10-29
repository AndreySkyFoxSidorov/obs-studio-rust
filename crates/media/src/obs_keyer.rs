//! OBS Color/Chroma Key v2 SDR equations. OBS Studio contributors, GPL-2.0-or-later.
//! Packed sampled rows and bounded scoped stripes preserve the exact staggered
//! bilinear neighborhood in the existing 48-byte-per-column scratch budget.
use crate::color_correction::{decode, decode_table, encode, unorm};
use crate::{ColorCorrectionInput, MediaError};
use std::sync::OnceLock;
#[path = "obs_keyer_lut.rs"]
mod lut;
#[path = "obs_keyer_packed.rs"]
mod packed;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorKeyType {
    Green,
    Blue,
    Red,
    Magenta,
    Custom,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromaKeyType {
    Green,
    Blue,
    Magenta,
    Custom,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorKeySettings {
    pub key_color_type: ColorKeyType,
    pub key_color: [u8; 3],
    pub similarity: u16,
    pub smoothness: u16,
    pub opacity: f32,
    pub contrast: f64,
    pub brightness: f32,
    pub gamma: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromaKeySettings {
    pub key_color_type: ChromaKeyType,
    pub key_color: [u8; 3],
    pub similarity: u16,
    pub smoothness: u16,
    pub spill: u16,
    pub opacity: f32,
    pub contrast: f64,
    pub brightness: f32,
    pub gamma: f64,
}
impl Default for ColorKeySettings {
    fn default() -> Self {
        Self {
            key_color_type: ColorKeyType::Green,
            key_color: [0, 255, 0],
            similarity: 80,
            smoothness: 50,
            opacity: 1.0,
            contrast: 0.0,
            brightness: 0.0,
            gamma: 0.0,
        }
    }
}
impl Default for ChromaKeySettings {
    fn default() -> Self {
        Self {
            key_color_type: ChromaKeyType::Green,
            key_color: [0, 255, 0],
            similarity: 400,
            smoothness: 80,
            spill: 100,
            opacity: 1.0,
            contrast: 0.0,
            brightness: 0.0,
            gamma: 0.0,
        }
    }
}
fn curve_valid(opacity: f32, contrast: f64, brightness: f32, gamma: f64) -> bool {
    opacity.is_finite()
        && (0.0..=1.0).contains(&opacity)
        && contrast.is_finite()
        && (-4.0..=4.0).contains(&contrast)
        && brightness.is_finite()
        && (-1.0..=1.0).contains(&brightness)
        && gamma.is_finite()
        && (-1.0..=1.0).contains(&gamma)
}
impl ColorKeySettings {
    pub fn validate(self) -> Result<(), MediaError> {
        if curve_valid(self.opacity, self.contrast, self.brightness, self.gamma)
            && (1..=1000).contains(&self.similarity)
            && (1..=1000).contains(&self.smoothness)
        {
            Ok(())
        } else {
            Err(MediaError::InvalidBuffer)
        }
    }
}
impl ChromaKeySettings {
    pub fn validate(self) -> Result<(), MediaError> {
        if curve_valid(self.opacity, self.contrast, self.brightness, self.gamma)
            && (1..=1000).contains(&self.similarity)
            && (1..=1000).contains(&self.smoothness)
            && (1..=1000).contains(&self.spill)
        {
            Ok(())
        } else {
            Err(MediaError::InvalidBuffer)
        }
    }
}

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
        self.pixel_with_opaque_table(pixel, encoding, opaque_color_table())
    }
    /// Opaque input uses exactly the same decode and nonlinear values as the
    /// general path, but can reuse the 256 possible transfer values. This
    /// avoids three exponentiations per camera pixel without quantising the
    /// color-key distance or changing the partially transparent path.
    pub(crate) fn pixel_with_opaque_table(self, pixel: [u8; 4], encoding: ColorCorrectionInput, table: &[f32; 256]) -> [u8; 4] {
        if pixel[3] == 255 {
            let decoded = decode_table();
            let rgb = [decoded[usize::from(pixel[0])], decoded[usize::from(pixel[1])], decoded[usize::from(pixel[2])]];
            let encoded = [table[usize::from(pixel[0])], table[usize::from(pixel[1])], table[usize::from(pixel[2])]];
            let distance = ((encoded[0] - self.key[0]).powi(2)
                + (encoded[1] - self.key[1]).powi(2)
                + (encoded[2] - self.key[2]).powi(2)).sqrt();
            let mask = ((distance - self.similarity).max(0.0) / self.smoothness).clamp(0.0, 1.0);
            return self.finish(rgb, (1.0 * self.opacity) * mask);
        }
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
        self.distance_nonlinear(linear.map(nonlinear))
    }
    fn distance_nonlinear(self, encoded: [f32; 3]) -> f32 {
        let chroma = source_chroma(encoded);
        ((chroma[0] - self.chroma[0]).powi(2) + (chroma[1] - self.chroma[1]).powi(2)).sqrt()
    }
    fn finish(self, rgb: [f32; 3], alpha: f32) -> [u8; 4] {
        if alpha == 0.0 {
            return [0; 4];
        }
        let rgb = if self.gamma == 1.0 {
            rgb
        } else {
            rgb.map(|c| c.powf(self.gamma))
        };
        [
            encode((rgb[0] * self.contrast + self.brightness) * alpha),
            encode((rgb[1] * self.contrast + self.brightness) * alpha),
            encode((rgb[2] * self.contrast + self.brightness) * alpha),
            unorm(alpha),
        ]
    }
    fn identity_curve(self) -> bool {
        self.gamma == 1.0 && self.contrast == 1.0 && self.brightness == 0.0 && self.opacity == 1.0
    }
    fn keyed_chroma(
        self,
        raw: [f32; 4],
        distance: f32,
        decoded: &[f32; 256],
        opaque_identity: bool,
    ) -> [u8; 4] {
        let base = distance - self.similarity;
        if base <= 0.0 || raw[3] == 0.0 || self.opacity == 0.0 {
            return [0; 4];
        }
        let mask = shape_mask((base / self.smoothness).clamp(0.0, 1.0));
        let spill = shape_mask((base / self.spill).clamp(0.0, 1.0));
        if opaque_identity && raw[3] == 1.0 && mask == 1.0 && spill == 1.0 {
            // The exhaustive frozen-encode interval oracle proves the original
            // gray+(c-gray) roundtrip preserves every sampled opaque byte.
            return [raw[0] as u8, raw[1] as u8, raw[2] as u8, 255];
        }
        let lp = row_lp_with(raw, decoded);
        let rgb = straight(lp);
        let gray = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        self.finish(
            rgb.map(|c| gray + (c - gray) * spill),
            (lp[3] * self.opacity) * mask,
        )
    }
}
/// The same functions and f32 operations used by the ordinary pixel path.
pub(crate) fn opaque_color_table() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|index| nonlinear(decode(index as u8))))
}
#[inline]
fn shape_mask(value: f32) -> f32 {
    if value == 0.0 {
        0.0
    } else if value == 1.0 {
        1.0
    } else {
        value.powf(1.5)
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
            ColorCorrectionInput::LinearAlpha if pixel[3] != 255 => {
                decode(encode(decode(pixel[i]) * alpha))
            }
            ColorCorrectionInput::EncodedAlpha => {
                decode(((u32::from(pixel[i]) * u32::from(pixel[3]) + 127) / 255) as u8)
            }
            ColorCorrectionInput::LinearAlpha | ColorCorrectionInput::PremultipliedLinearSrgb => {
                decode(pixel[i])
            }
        };
    }
    result
}
pub(crate) fn sampled_bytes(pixel: [u8; 4], encoding: ColorCorrectionInput) -> [u8; 4] {
    sampled_bytes_with(pixel, encoding, decode_table())
}
#[inline]
fn sampled_bytes_with(
    pixel: [u8; 4],
    encoding: ColorCorrectionInput,
    decoded: &[f32; 256],
) -> [u8; 4] {
    // All three encodings preserve opaque bytes exactly. Premultiplied ingress
    // also preserves RGB even with alpha zero or RGB greater than alpha.
    if pixel[3] == 255 || encoding == ColorCorrectionInput::PremultipliedLinearSrgb {
        return pixel;
    }
    let alpha = f32::from(pixel[3]) / 255.0;
    let mut bytes = pixel;
    for i in 0..3 {
        bytes[i] = match encoding {
            ColorCorrectionInput::LinearAlpha if pixel[3] != 255 => {
                encode(decoded[usize::from(pixel[i])] * alpha)
            }
            ColorCorrectionInput::EncodedAlpha => {
                ((u32::from(pixel[i]) * u32::from(pixel[3]) + 127) / 255) as u8
            }
            ColorCorrectionInput::LinearAlpha | ColorCorrectionInput::PremultipliedLinearSrgb => {
                pixel[i]
            }
        };
    }
    bytes
}
#[cfg(test)]
fn row_lp(raw: [f32; 4]) -> [f32; 4] {
    row_lp_with(raw, decode_table())
}
#[inline]
fn row_lp_with(raw: [f32; 4], decoded: &[f32; 256]) -> [f32; 4] {
    [
        decoded[usize::from(raw[0] as u8)],
        decoded[usize::from(raw[1] as u8)],
        decoded[usize::from(raw[2] as u8)],
        raw[3],
    ]
}
pub(crate) fn chroma_in_place(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    encoding: ColorCorrectionInput,
    prepared: PreparedKeyer,
    workspace: &mut [u32],
) -> Result<(), MediaError> {
    packed::apply(pixels, width, height, encoding, prepared, workspace)
}

#[cfg(test)]
#[path = "obs_keyer_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "obs_keyer_performance_tests.rs"]
mod performance_tests;
