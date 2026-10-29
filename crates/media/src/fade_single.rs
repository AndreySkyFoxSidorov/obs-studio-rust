//! SDR OBS FadeSingle gain in linear sRGB, flattened onto opaque black.
use crate::{CompositeLimits, MediaError, RgbaFrame};

fn linear(value: u8) -> f64 {
    let value = f64::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}
fn encoded(value: f64) -> u8 {
    let value = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Apply a rational linear-light gain to a complete SDR scene. Source alpha is
/// multiplied in linear light, and output is always opaque black-backed RGBA8.
/// This is distinct from an encoded-space crossfade with a black scene. Counts
/// one input layer and two visits per pixel; all limits apply at endpoints too.
pub fn fade_single_on_black(
    frame: &RgbaFrame,
    weight_num: u64,
    weight_den: u64,
    limits: CompositeLimits,
) -> Result<RgbaFrame, MediaError> {
    if weight_den == 0 || weight_num > weight_den {
        return Err(MediaError::InvalidTransition);
    }
    let visits = u64::from(frame.width())
        .checked_mul(u64::from(frame.height()))
        .and_then(|n| n.checked_mul(2))
        .ok_or(MediaError::Overflow)?;
    if limits.max_layers < 1
        || frame.byte_len() > limits.max_input_bytes
        || frame.byte_len() > limits.max_output_bytes
        || visits > limits.max_pixel_visits
    {
        return Err(MediaError::BudgetExceeded);
    }
    let weight = weight_num as f64 / weight_den as f64;
    let mut output = vec![0; frame.byte_len()];
    for (source, dest) in frame
        .pixels()
        .chunks_exact(4)
        .zip(output.chunks_exact_mut(4))
    {
        dest[3] = 255;
        if weight_num == weight_den && source[3] == 255 {
            dest[..3].copy_from_slice(&source[..3]);
        } else if weight_num != 0 && source[3] != 0 {
            let gain = weight * f64::from(source[3]) / 255.0;
            for channel in 0..3 {
                dest[channel] = encoded(linear(source[channel]) * gain);
            }
        }
    }
    RgbaFrame::new(frame.width(), frame.height(), output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linear_srgb_oracles_and_alpha_flattening_differ_from_encoded_fade() {
        let white = RgbaFrame::solid(1, 1, [255, 255, 255, 255]).unwrap();
        for (num, den, expected) in [
            (0, 1, 0),
            (1, 4, 137),
            (1, 2, 188),
            (1, 1, 255),
            (u64::MAX, u64::MAX, 255),
        ] {
            assert_eq!(
                fade_single_on_black(&white, num, den, CompositeLimits::default())
                    .unwrap()
                    .pixels(),
                &[expected, expected, expected, 255]
            );
        }
        let black = RgbaFrame::solid(1, 1, [0, 0, 0, 255]).unwrap();
        assert_eq!(
            crate::crossfade_rgba(&black, &white, 1, 2, CompositeLimits::default())
                .unwrap()
                .pixels(),
            &[128, 128, 128, 255]
        );
        let alpha = RgbaFrame::new(
            3,
            1,
            vec![255, 0, 0, 128, 255, 255, 255, 0, 7, 69, 247, 255],
        )
        .unwrap();
        assert_eq!(
            fade_single_on_black(&alpha, 1, 1, CompositeLimits::default())
                .unwrap()
                .pixels(),
            &[188, 0, 0, 255, 0, 0, 0, 255, 7, 69, 247, 255]
        );
        assert_eq!(
            fade_single_on_black(&alpha, 0, 1, CompositeLimits::default())
                .unwrap()
                .pixels(),
            &[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]
        );
    }
    #[test]
    fn endpoint_input_output_layers_and_work_are_admitted_before_allocation() {
        let frame = RgbaFrame::solid(1, 1, [255, 0, 0, 255]).unwrap();
        for (num, den) in [(1, 0), (2, 1), (u64::MAX, u64::MAX - 1)] {
            assert_eq!(
                fade_single_on_black(&frame, num, den, CompositeLimits::default()),
                Err(MediaError::InvalidTransition)
            );
        }
        let exact = CompositeLimits {
            max_layers: 1,
            max_input_bytes: 4,
            max_output_bytes: 4,
            max_pixel_visits: 2,
        };
        assert!(fade_single_on_black(&frame, 1, 1, exact).is_ok());
        for limits in [
            CompositeLimits {
                max_layers: 0,
                ..exact
            },
            CompositeLimits {
                max_input_bytes: 3,
                ..exact
            },
            CompositeLimits {
                max_output_bytes: 3,
                ..exact
            },
            CompositeLimits {
                max_pixel_visits: 1,
                ..exact
            },
        ] {
            for (num, den) in [(0, 1), (1, 1), (1, 2)] {
                assert_eq!(
                    fade_single_on_black(&frame, num, den, limits),
                    Err(MediaError::BudgetExceeded)
                );
            }
        }
    }
}
