//! Portable derived coefficients/byte preparation for independent SDR adapters.
//! No GPU, OS, UI, resource handles or alternate coefficient arithmetic.
use super::{ColorCorrectionSettings, Prepared, decode, encode};
use crate::MediaError;

#[derive(Clone, Copy, Debug)]
pub struct ColorCorrectionCoefficients {
    gamma: f32,
    matrix: [[f32; 4]; 4],
    authored_bits: [u32; 10],
}

impl ColorCorrectionCoefficients {
    pub fn new(settings: ColorCorrectionSettings) -> Result<Self, MediaError> {
        settings.validate()?;
        let prepared = Prepared::new(settings);
        let gamma = settings.gamma.to_bits();
        Ok(Self {
            gamma: prepared.gamma,
            matrix: prepared.matrix,
            authored_bits: [
                3, gamma as u32, (gamma >> 32) as u32,
                settings.contrast.to_bits(), settings.brightness.to_bits(),
                settings.saturation.to_bits(), settings.hue_shift.to_bits(),
                settings.opacity.to_bits(),
                u32::from_le_bytes([settings.color_multiply[0], settings.color_multiply[1], settings.color_multiply[2], 0]),
                u32::from_le_bytes([settings.color_add[0], settings.color_add[1], settings.color_add[2], 0]),
            ],
        })
    }
    pub fn gamma(&self) -> f32 { self.gamma }
    /// Row-major coefficients. CPU output column j is the left-associated sum
    /// value0*row0[j] + value1*row1[j] + value2*row2[j] + value3*row3[j].
    pub fn matrix(&self) -> &[[f32;4];4] { &self.matrix }
    /// Every authored setting bit, including both halves of the original f64.
    pub fn authored_bits(&self) -> [u32;10] { self.authored_bits }
}

pub fn color_correction_decode_byte(byte: u8) -> f32 { decode(byte) }

/// Exact byte written by the Color/linear-alpha source's intermediate target.
/// This finite byte-pair mapping is independent of authored correction settings.
pub fn color_correction_linear_alpha_byte(color: u8, alpha: u8) -> u8 {
    if alpha == 255 { color }
    else { encode(decode(color) * (f32::from(alpha) / 255.0)) }
}

