//! Source-aware SDR policies derived from OBS color-source/image-source and
//! libobs/graphics/srgb.h. Original sources use straight RGBA8; explicitly typed
//! filter/nested framebuffer output uses sRGB-encoded premultiplied linear RGB.

use std::sync::OnceLock;

/// How one source enters the SDR scene framebuffer. The default preserves the
/// generic compositor's historical encoded-space straight-alpha behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SourceBlend {
    #[default]
    Encoded,
    /// Color source: decode RGB to linear light, then premultiply by alpha.
    LinearSrgb,
    /// OBS image source with linear_alpha=false: round encoded RGB*alpha into
    /// texture bytes first, then decode that premultiplied texture to linear.
    EncodedPremultipliedSrgb,
    /// Actual sRGB framebuffer bytes: decode RGB directly as premultiplied
    /// linear light. RGB may exceed alpha, including alpha rounded to zero.
    PremultipliedLinearSrgb,
}

/// Transfer an owned SDR render result to the straight-alpha PNG boundary.
/// No additional raster allocation. Alpha-zero RGB cannot be represented in a
/// straight-alpha file and is explicitly zeroed.
pub fn straighten_rgba(mut frame: crate::RgbaFrame, blend: SourceBlend) -> crate::RgbaFrame {
    if blend == SourceBlend::PremultipliedLinearSrgb {
        for pixel in frame.pixels.chunks_exact_mut(4) {
            let alpha = f64::from(pixel[3]) / 255.0;
            for value in &mut pixel[..3] {
                *value = if alpha > 0.0 { encode(decode(*value) / alpha) } else { 0 };
            }
        }
    }
    frame
}

pub(super) fn decode(value: u8) -> f64 {
    // Compute on this target from the unchanged f64 scalar expression.
    // This is independent of color_correction's lower-precision f32 table.
    static TABLE: OnceLock<[f64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|byte| {
            let value = f64::from(byte as u8) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        })
    })[usize::from(value)]
}
pub(super) fn encode(value: f64) -> u8 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

pub(crate) fn blend(destination: &mut [u8], source: &[u8], opacity: f32, mode: SourceBlend) {
    if (source[3] == 0 && mode != SourceBlend::PremultipliedLinearSrgb) || opacity == 0.0 {
        return;
    }
    if source[3] == 255 && opacity == 1.0 {
        destination.copy_from_slice(source);
        return;
    }
    let opacity = f64::from(opacity);
    let raw_alpha = f64::from(source[3]) / 255.0;
    let alpha = raw_alpha * opacity;
    let remainder = f64::from(destination[3]) / 255.0 * (1.0 - alpha);
    let output_alpha = alpha + remainder;
    for channel in 0..3 {
        let source_linear = match mode {
            SourceBlend::PremultipliedLinearSrgb => decode(source[channel]) * opacity,
            SourceBlend::LinearSrgb => decode(source[channel]) * alpha,
            SourceBlend::EncodedPremultipliedSrgb => {
                // 255 is odd, so c*a/255 can never land exactly on a half.
                // This integer nearest rounding matches gs_premultiply_xyza.
                let byte = ((u32::from(source[channel]) * u32::from(source[3]) + 127) / 255) as u8;
                decode(byte) * opacity
            }
            SourceBlend::Encoded => {
                unreachable!("legacy encoded mode uses its original compositor")
            }
        };
        destination[channel] = if output_alpha > 0.0 {
            encode((source_linear + decode(destination[channel]) * remainder) / output_alpha)
        } else {
            0
        };
    }
    destination[3] = (output_alpha * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// SDR offscreen framebuffer, preserving premultiplied energy independently
/// of its alpha UNORM. Used by nested scenes containing typed LP filter output.
pub(crate) fn blend_premultiplied(
    destination: &mut [u8],
    source: &[u8],
    opacity: f32,
    mode: SourceBlend,
) {
    if opacity == 0.0 || (source[3] == 0 && mode != SourceBlend::PremultipliedLinearSrgb) {
        return;
    }
    let item = f64::from(opacity);
    let a = f64::from(source[3]) / 255.0 * item;
    for i in 0..3 {
        let lp = match mode {
            SourceBlend::PremultipliedLinearSrgb => decode(source[i]) * item,
            SourceBlend::LinearSrgb => decode(source[i]) * a,
            SourceBlend::EncodedPremultipliedSrgb | SourceBlend::Encoded => {
                let byte = ((u32::from(source[i]) * u32::from(source[3]) + 127) / 255) as u8;
                decode(byte) * item
            }
        };
        destination[i] = encode(lp + decode(destination[i]) * (1.0 - a));
    }
    destination[3] = ((a + f64::from(destination[3]) / 255.0 * (1.0 - a)) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Affine2, AffineLayer, CompositeLimits, MediaError, RgbaFrame, TexelRect, UvRect,
        composite_affine,
    };
    fn layer(image: &RgbaFrame, mode: SourceBlend) -> AffineLayer<'_> {
        AffineLayer::new(image, Affine2::IDENTITY, UvRect::FULL, 1.0).with_blend_mode(mode)
    }
    fn pixel(image: &RgbaFrame, background: [u8; 4], mode: SourceBlend) -> Vec<u8> {
        composite_affine(
            1,
            1,
            background,
            &[layer(image, mode)],
            CompositeLimits::default(),
        )
        .unwrap()
        .pixels()
        .to_vec()
    }

    #[test]
    fn actual_color_and_image_sdr_policies_have_independent_literal_oracles() {
        let white = RgbaFrame::solid(1, 1, [255, 255, 255, 128]).unwrap();
        // IEC sRGB transfer, independently evaluated: Color decodes white
        // before alpha; Image decodes the rounded premultiplied byte128.
        assert_eq!(
            pixel(&white, [0, 0, 0, 255], SourceBlend::LinearSrgb),
            [188, 188, 188, 255]
        );
        assert_eq!(
            pixel(
                &white,
                [0, 0, 0, 255],
                SourceBlend::EncodedPremultipliedSrgb
            ),
            [128, 128, 128, 255]
        );
        assert_eq!(
            pixel(&white, [128, 128, 128, 255], SourceBlend::LinearSrgb),
            [205, 205, 205, 255]
        );
        assert_eq!(
            pixel(
                &white,
                [128, 128, 128, 255],
                SourceBlend::EncodedPremultipliedSrgb
            ),
            [154, 154, 154, 255]
        );
        assert_eq!(
            pixel(&white, [0, 0, 0, 255], SourceBlend::Encoded),
            [128, 128, 128, 255]
        );
        let legacy = AffineLayer::new(&white, Affine2::IDENTITY, UvRect::FULL, 1.0);
        assert_eq!(
            composite_affine(1, 1, [0, 0, 0, 255], &[legacy], CompositeLimits::default())
                .unwrap()
                .pixels(),
            &[128, 128, 128, 255]
        );
    }

    #[test]
    fn mixed_source_order_opacity_and_straight_destination_alpha_are_explicit() {
        let white = RgbaFrame::solid(1, 1, [255, 255, 255, 128]).unwrap();
        let black = RgbaFrame::solid(1, 1, [0, 0, 0, 128]).unwrap();
        let color = layer(&white, SourceBlend::LinearSrgb);
        let image = layer(&black, SourceBlend::EncodedPremultipliedSrgb);
        assert_eq!(
            composite_affine(
                1,
                1,
                [0, 0, 0, 255],
                &[color, image],
                CompositeLimits::default()
            )
            .unwrap()
            .pixels(),
            &[137, 137, 137, 255]
        );
        assert_eq!(
            composite_affine(
                1,
                1,
                [0, 0, 0, 255],
                &[image, color],
                CompositeLimits::default()
            )
            .unwrap()
            .pixels(),
            &[188, 188, 188, 255]
        );
        let opaque = RgbaFrame::solid(1, 1, [255; 4]).unwrap();
        for mode in [
            SourceBlend::LinearSrgb,
            SourceBlend::EncodedPremultipliedSrgb,
        ] {
            let half = AffineLayer::new(&opaque, Affine2::IDENTITY, UvRect::FULL, 0.5)
                .with_blend_mode(mode);
            assert_eq!(
                composite_affine(1, 1, [0, 0, 0, 255], &[half], CompositeLimits::default())
                    .unwrap()
                    .pixels(),
                &[188, 188, 188, 255]
            );
            assert_eq!(pixel(&opaque, [17, 29, 31, 0], mode), [255, 255, 255, 255]);
        }
        assert_eq!(
            pixel(&white, [0, 0, 0, 0], SourceBlend::LinearSrgb),
            [255, 255, 255, 128]
        );
        assert_eq!(
            pixel(&white, [0, 0, 0, 0], SourceBlend::EncodedPremultipliedSrgb),
            [175, 175, 175, 128]
        );
        for (mode, expected) in [
            (SourceBlend::LinearSrgb, 137),
            (SourceBlend::EncodedPremultipliedSrgb, 92),
        ] {
            let half = AffineLayer::new(&white, Affine2::IDENTITY, UvRect::FULL, 0.5)
                .with_blend_mode(mode);
            assert_eq!(
                composite_affine(1, 1, [0, 0, 0, 255], &[half], CompositeLimits::default())
                    .unwrap()
                    .pixels(),
                &[expected, expected, expected, 255]
            );
        }
        let invisible = RgbaFrame::solid(1, 1, [255, 19, 97, 0]).unwrap();
        for mode in [
            SourceBlend::LinearSrgb,
            SourceBlend::EncodedPremultipliedSrgb,
        ] {
            assert_eq!(pixel(&invisible, [11, 23, 79, 0], mode), [11, 23, 79, 0]);
        }
    }

    #[test]
    fn native_crop_reflection_and_alpha_extremes_preserve_exact_bytes() {
        let image = RgbaFrame::new(
            4,
            1,
            vec![
                9, 8, 7, 255, 1, 37, 249, 255, 255, 255, 255, 0, 6, 5, 4, 255,
            ],
        )
        .unwrap();
        let transform = Affine2 {
            xx: -2.0,
            tx: 2.0,
            ..Affine2::IDENTITY
        };
        for mode in [
            SourceBlend::LinearSrgb,
            SourceBlend::EncodedPremultipliedSrgb,
        ] {
            let layer = AffineLayer::from_texels(
                &image,
                transform,
                TexelRect {
                    x: 1,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                1.0,
            )
            .with_blend_mode(mode);
            let out =
                composite_affine(2, 1, [23, 45, 67, 89], &[layer], CompositeLimits::default())
                    .unwrap();
            assert_eq!(out.pixels(), &[23, 45, 67, 89, 1, 37, 249, 255]);
            let invisible = AffineLayer::from_texels(
                &image,
                transform,
                TexelRect {
                    x: 1,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                0.0,
            )
            .with_blend_mode(mode);
            assert_eq!(
                composite_affine(
                    2,
                    1,
                    [23, 45, 67, 89],
                    &[invisible],
                    CompositeLimits::default()
                )
                .unwrap()
                .pixels(),
                &[23, 45, 67, 89, 23, 45, 67, 89]
            );
        }
    }

    #[test]
    fn source_rounding_matches_the_obs_f32_byte_premultiply_for_every_byte_pair() {
        for color in 0..=255u32 {
            for alpha in 0..=255u32 {
                let integer = (color * alpha + 127) / 255;
                let original =
                    ((color as f32 / 255.0) * (alpha as f32 / 255.0) * 255.0 + 0.5) as u32;
                assert_eq!(integer, original, "c={color},a={alpha}");
            }
        }
    }

    #[test]
    fn source_modes_keep_original_preallocation_work_and_byte_admission() {
        let frame = RgbaFrame::solid(1, 1, [255, 255, 255, 128]).unwrap();
        let exact = CompositeLimits {
            max_layers: 1,
            max_input_bytes: 4,
            max_output_bytes: 4,
            max_pixel_visits: 2,
        };
        for mode in [
            SourceBlend::LinearSrgb,
            SourceBlend::EncodedPremultipliedSrgb,
        ] {
            let layer = layer(&frame, mode);
            assert!(composite_affine(1, 1, [0; 4], &[layer], exact).is_ok());
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
                assert_eq!(
                    composite_affine(1, 1, [0; 4], &[layer], limits),
                    Err(MediaError::BudgetExceeded)
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "source_color_decode_tests.rs"]
mod decode_tests;
