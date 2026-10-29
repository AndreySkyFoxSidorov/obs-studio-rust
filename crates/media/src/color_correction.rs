//! OBS Color Correction v2's SDR shader/matrix, expressed on bounded CPU pixels.
//!
//! Derived from obs-filters/color-correction-filter.c and its .effect (GPL-2.0-or-later).
//! Original reference copyright (C) 2016 c3r1c3; GPL-2.0-or-later.
//! Typed output is threaded through the source-effect cache and compositor.
use crate::{MediaError, RgbaFrame};
use std::sync::OnceLock;
#[path = "color_correction_coefficients.rs"]
mod coefficients;
pub use coefficients::{ColorCorrectionCoefficients, color_correction_decode_byte, color_correction_linear_alpha_byte};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorCorrectionSettings {
    pub gamma: f64,
    pub contrast: f32,
    pub brightness: f32,
    pub saturation: f32,
    pub hue_shift: f32,
    pub opacity: f32,
    pub color_multiply: [u8; 3],
    pub color_add: [u8; 3],
}

impl Default for ColorCorrectionSettings {
    fn default() -> Self {
        Self {
            gamma: 0.0,
            contrast: 0.0,
            brightness: 0.0,
            saturation: 0.0,
            hue_shift: 0.0,
            opacity: 1.0,
            color_multiply: [255; 3],
            color_add: [0; 3],
        }
    }
}

impl ColorCorrectionSettings {
    pub fn validate(self) -> Result<(), MediaError> {
        if !self.gamma.is_finite() || !(-3.0..=3.0).contains(&self.gamma) {
            return Err(MediaError::InvalidBuffer);
        }
        for (value, minimum, maximum) in [
            (self.contrast, -4.0, 4.0),
            (self.brightness, -1.0, 1.0),
            (self.saturation, -1.0, 5.0),
            (self.hue_shift, -180.0, 180.0),
            (self.opacity, 0.0, 1.0),
        ] {
            if !value.is_finite() || !(minimum..=maximum).contains(&value) {
                return Err(MediaError::InvalidBuffer);
            }
        }
        Ok(())
    }
}

/// How the existing source bytes reach the v2 shader's sRGB texture sampler.
/// The variants do not change alpha, and never infer a source type from pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorCorrectionInput {
    /// Straight encoded-sRGB colors rendered with linear-light alpha into the
    /// RGBA8 filter target, as for a Color Source or a rendered scene.
    LinearAlpha,
    /// Original straight image/text bytes with OBS's encoded-alpha policy:
    /// round encoded RGB * alpha to a texture byte, then sample as sRGB.
    EncodedAlpha,
    /// Bytes already contain sRGB-encoded linear-premultiplied RGB. RGB can
    /// exceed the alpha channel; no second premultiplication is permitted.
    PremultipliedLinearSrgb,
}

#[derive(Clone, Copy, Debug)]
pub struct ColorCorrectionLimits {
    pub max_output_bytes: usize,
    pub max_pixel_stages: usize,
}

/// Typed filter-target pixels, not straight RGBA. The RGB channels contain
/// E(linear_RGB * alpha) independently of alpha's UNORM value.
#[derive(Debug)]
pub struct PremultipliedLinearFrame {
    image: RgbaFrame,
}

impl PremultipliedLinearFrame {
    pub fn image(&self) -> &RgbaFrame {
        &self.image
    }

    pub fn into_image(self) -> RgbaFrame {
        self.image
    }
}

/// One admitted output allocation; coefficients are computed once per call.
/// Shader values remain unclamped until the SDR render-target write. In
/// particular RGB is not clamped to alpha, nor to one before multiplication.
pub fn color_correct_v2(
    input: &RgbaFrame,
    encoding: ColorCorrectionInput,
    settings: ColorCorrectionSettings,
    limits: ColorCorrectionLimits,
) -> Result<PremultipliedLinearFrame, MediaError> {
    settings.validate()?;
    if input.byte_len() > limits.max_output_bytes || input.byte_len() / 4 > limits.max_pixel_stages
    {
        return Err(MediaError::BudgetExceeded);
    }
    let prepared = Prepared::new(settings);
    let mut output = input.pixels().to_vec();
    for pixel in output.chunks_exact_mut(4) {
        let raw = [pixel[0], pixel[1], pixel[2], pixel[3]];
        pixel.copy_from_slice(&prepared.pixel(raw, encoding));
    }
    Ok(PremultipliedLinearFrame {
        image: RgbaFrame::new(input.width(), input.height(), output)?,
    })
}

type Matrix = [[f32; 4]; 4];

pub(crate) struct Prepared {
    gamma: f32,
    matrix: Matrix,
}

impl Prepared {
    pub(crate) fn new(settings: ColorCorrectionSettings) -> Self {
        let g = settings.gamma;
        let gamma = if g < 0.0 { 1.0 - g } else { 1.0 / (1.0 + g) } as f32;
        let contrast = if settings.contrast < 0.0 {
            1.0 / (1.0 - settings.contrast)
        } else {
            1.0 + settings.contrast
        };
        let mut con = identity();
        for (i, row) in con.iter_mut().enumerate().take(3) {
            row[i] = contrast;
        }
        let mut bright = identity();
        bright[3][..3].fill(settings.brightness);

        let saturation = settings.saturation + 1.0;
        let weights = [0.299_f32, 0.587, 0.114];
        let mut sat = identity();
        for (i, row) in sat.iter_mut().enumerate().take(3) {
            row[..3].fill((1.0 - saturation) * weights[i]);
            row[i] += saturation;
        }

        // These casts and the literal root3 follow the C precomputation. This
        // is RGB-axis quaternion rotation, not HSV/HSL hue adjustment.
        let half =
            0.5_f32 * (f64::from(settings.hue_shift) / (180.0 / std::f64::consts::PI)) as f32;
        let q = 0.57735_f32 * (f64::from(half).sin() as f32);
        let w = f64::from(half).cos() as f32;
        let square = q * q;
        let d = (0.5 - square * 2.0) * 2.0;
        let a = (square + q * w) * 2.0;
        let b = (square - q * w) * 2.0;
        let hue_opacity = [
            [d, b, a, 0.0],
            [a, d, b, 0.0],
            [b, a, d, 0.0],
            [0.0, 0.0, 0.0, settings.opacity],
        ];

        let mut color = identity();
        for (i, row) in color.iter_mut().enumerate().take(3) {
            row[i] = decode(settings.color_multiply[i]);
        }
        for (i, value) in color[3].iter_mut().enumerate().take(3) {
            *value = decode(settings.color_add[i]);
        }
        let matrix = multiply(
            multiply(multiply(multiply(con, bright), sat), hue_opacity),
            color,
        );
        Self { gamma, matrix }
    }

    pub(crate) fn pixel(&self, mut pixel: [u8; 4], encoding: ColorCorrectionInput) -> [u8; 4] {
        let alpha = f32::from(pixel[3]) / 255.0;
        let mut value = [0.0; 4];
        value[3] = alpha;
        if alpha > 0.0 {
            for channel in 0..3 {
                let premultiplied = match encoding {
                    ColorCorrectionInput::LinearAlpha if pixel[3] != 255 => {
                        // The custom-draw source cannot use filter bypass:
                        // observe its RGBA8 sRGB render-target quantization.
                        decode(encode(decode(pixel[channel]) * alpha))
                    }
                    ColorCorrectionInput::EncodedAlpha => {
                        let byte =
                            ((u32::from(pixel[channel]) * u32::from(pixel[3]) + 127) / 255) as u8;
                        decode(byte)
                    }
                    ColorCorrectionInput::LinearAlpha
                    | ColorCorrectionInput::PremultipliedLinearSrgb => decode(pixel[channel]),
                };
                let straight = premultiplied / alpha;
                value[channel] = if self.gamma == 1.0 {
                    straight
                } else {
                    straight.powf(self.gamma)
                };
            }
        }
        let transformed = self.transform(value);
        for channel in 0..3 {
            pixel[channel] = encode(transformed[channel] * transformed[3]);
        }
        pixel[3] = unorm(transformed[3]);
        pixel
    }

    fn transform(&self, value: [f32; 4]) -> [f32; 4] {
        std::array::from_fn(|column| {
            value[0] * self.matrix[0][column]
                + value[1] * self.matrix[1][column]
                + value[2] * self.matrix[2][column]
                + value[3] * self.matrix[3][column]
        })
    }
}

fn identity() -> Matrix {
    std::array::from_fn(|row| std::array::from_fn(|column| if row == column { 1.0 } else { 0.0 }))
}

fn multiply(left: Matrix, right: Matrix) -> Matrix {
    std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            left[row][0] * right[0][column]
                + left[row][1] * right[1][column]
                + left[row][2] * right[2][column]
                + left[row][3] * right[3][column]
        })
    })
}

#[inline]
pub(crate) fn decode(byte: u8) -> f32 {
    decode_table()[usize::from(byte)]
}

#[inline]
pub(crate) fn decode_table() -> &'static [f32; 256] {
    // Construct on this target with the original f32 expression. A baked or
    // approximate transfer table could alter downstream rounding thresholds.
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|byte| {
            let value = byte as f32 / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        })
    })
}

pub(crate) fn encode(linear: f32) -> u8 {
    let value = linear.clamp(0.0, 1.0);
    let encoded = if value <= 0.0031308 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    unorm(encoded)
}

pub(crate) fn unorm(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
#[path = "color_correction_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "color_correction_gamma_tests.rs"]
mod gamma_tests;
