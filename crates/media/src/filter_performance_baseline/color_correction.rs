// Frozen original arithmetic for exact regression comparison.
use crate::{ColorCorrectionInput, ColorCorrectionSettings};
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
                    ColorCorrectionInput::LinearAlpha => {
                        // The custom-draw source cannot use filter bypass:
                        // observe its RGBA8 sRGB render-target quantization.
                        decode(encode(decode(pixel[channel]) * alpha))
                    }
                    ColorCorrectionInput::EncodedAlpha => {
                        let byte =
                            ((u32::from(pixel[channel]) * u32::from(pixel[3]) + 127) / 255) as u8;
                        decode(byte)
                    }
                    ColorCorrectionInput::PremultipliedLinearSrgb => decode(pixel[channel]),
                };
                value[channel] = (premultiplied / alpha).powf(self.gamma);
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

pub(crate) fn decode(byte: u8) -> f32 {
    let value = f32::from(byte) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
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
