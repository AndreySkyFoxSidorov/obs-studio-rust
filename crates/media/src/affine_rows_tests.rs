//! Byte-exact row mapping versus the unchanged generic inverse-map compositor.
use super::*;

fn compare(width: u32, height: u32, layers: &[AffineLayer<'_>], premultiplied: bool) {
    let background = if premultiplied {
        [0; 4]
    } else {
        [11, 37, 89, 173]
    };
    let fast = composite_affine_target_policy(
        width,
        height,
        background,
        layers,
        CompositeLimits::default(),
        premultiplied,
        true,
    );
    let raw = composite_affine_target_policy(
        width,
        height,
        background,
        layers,
        CompositeLimits::default(),
        premultiplied,
        false,
    );
    assert_eq!(
        fast, raw,
        "{width}x{height} premultiplied={premultiplied} layers={layers:?}"
    );
}
fn stats() -> [usize; 3] {
    AXIS_ROW_STATS.with(std::cell::Cell::get)
}
fn reset() {
    AXIS_ROW_STATS.with(|value| value.set([0; 3]));
}

#[test]
fn solid_stretch_and_real_image_copy_rows_are_exercised_not_only_generic_fallback() {
    reset();
    let solid = RgbaFrame::solid(1, 1, [220, 35, 70, 255]).unwrap();
    let stretch = AffineLayer::new(
        &solid,
        Affine2 {
            xx: 19.0,
            yy: 11.0,
            tx: -2.0,
            ty: 1.0,
            ..Affine2::IDENTITY
        },
        UvRect::FULL,
        1.0,
    )
    .with_blend_mode(SourceBlend::LinearSrgb);
    compare(17, 13, &[stretch], false);
    assert!(stats()[0] > 0 && stats()[1] > 0);
    reset();
    let image = RgbaFrame::new(
        9,
        7,
        (0..63)
            .flat_map(|i| [i as u8, (i * 3) as u8, (i * 7) as u8, 255])
            .collect(),
    )
    .unwrap();
    let layer = AffineLayer::from_texels(
        &image,
        Affine2 {
            xx: 9.0,
            yy: 7.0,
            tx: 2.0,
            ty: 3.0,
            ..Affine2::IDENTITY
        },
        TexelRect {
            x: 0,
            y: 0,
            width: 9,
            height: 7,
        },
        1.0,
    )
    .with_blend_mode(SourceBlend::EncodedPremultipliedSrgb);
    compare(17, 13, &[layer], false);
    assert!(
        stats()[2] > 0,
        "actual opaque image rows must use contiguous copy"
    );
    reset();
    compare(17, 13, &[layer], true);
    assert!(stats()[0] > 0);
    assert_eq!(
        stats()[2],
        0,
        "LP targets keep their exact existing blend path"
    );
}

#[test]
fn literal_fractional_clip_reflection_and_excluded_49_edge_match_generic() {
    let image = RgbaFrame::new(
        3,
        2,
        vec![
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 11, 31, 71, 1, 255, 255, 255, 254, 1, 2,
            3, 255,
        ],
    )
    .unwrap();
    for transform in [
        Affine2 {
            xx: 13.0,
            yy: 7.0,
            xy: -0.0,
            yx: -0.0,
            tx: 0.25,
            ty: -0.25,
        },
        Affine2 {
            xx: 49.0,
            yy: 7.0,
            tx: -0.5,
            ty: -0.5,
            ..Affine2::IDENTITY
        },
        Affine2 {
            xx: -49.0,
            yy: 7.0,
            tx: 49.5,
            ty: -0.5,
            ..Affine2::IDENTITY
        },
        Affine2 {
            xx: 8.25,
            yy: -11.75,
            tx: -1.125,
            ty: 12.5,
            ..Affine2::IDENTITY
        },
        Affine2 {
            xx: 1e308,
            yy: 1e-308,
            ..Affine2::IDENTITY
        },
        Affine2 {
            xx: 1e-308,
            yy: 1e308,
            ..Affine2::IDENTITY
        },
        Affine2 {
            xx: -1e-308,
            yy: -1e308,
            tx: 1.0,
            ty: 1e308,
            ..Affine2::IDENTITY
        },
    ] {
        for mode in [
            SourceBlend::Encoded,
            SourceBlend::LinearSrgb,
            SourceBlend::EncodedPremultipliedSrgb,
            SourceBlend::PremultipliedLinearSrgb,
        ] {
            for opacity in [0.0, 0.25, 0.5, 1.0] {
                for lp in [false, true] {
                    compare(
                        51,
                        17,
                        &[AffineLayer::new(&image, transform, UvRect::FULL, opacity)
                            .with_blend_mode(mode)],
                        lp,
                    );
                }
            }
        }
    }
    let white = RgbaFrame::solid(1, 1, [255; 4]).unwrap();
    let layer = AffineLayer::new(
        &white,
        Affine2 {
            xx: 49.0,
            yy: 1.0,
            tx: -0.5,
            ..Affine2::IDENTITY
        },
        UvRect::FULL,
        1.0,
    );
    let output = composite_affine_target_policy(
        50,
        1,
        [0, 0, 0, 255],
        &[layer],
        CompositeLimits::default(),
        false,
        true,
    )
    .unwrap();
    assert_eq!(
        &output.pixels()[48 * 4..49 * 4],
        &[0, 0, 0, 255],
        "exact excluded edge must not become a copied source pixel"
    );
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 32) as u32
    }
    fn fraction(&mut self) -> f64 {
        (f64::from(self.next() % 257) - 128.0) / 8.0
    }
}
#[test]
fn seeded_random_full_uv_crops_shear_transparency_and_modes_are_byte_exact() {
    let mut random = Rng(0x736b796c697665);
    for case in 0..1800 {
        let (w, h) = (1 + random.next() % 17, 1 + random.next() % 13);
        let mut pixels = vec![];
        for _ in 0..w * h {
            pixels.extend([
                random.next() as u8,
                random.next() as u8,
                random.next() as u8,
                [0, 1, 127, 128, 254, 255][random.next() as usize % 6],
            ]);
        }
        let image = RgbaFrame::new(w, h, pixels).unwrap();
        let sign = if random.next() % 2 == 0 { 1.0 } else { -1.0 };
        let mut transform = Affine2 {
            xx: sign * (1.0 + f64::from(random.next() % 600)) / 16.0,
            yy: (1.0 + f64::from(random.next() % 500)) / 16.0,
            tx: random.fraction(),
            ty: random.fraction(),
            ..Affine2::IDENTITY
        };
        if case % 7 == 0 {
            transform.xy = 0.375;
            transform.yx = -0.5;
        }
        if case % 3 == 0 {
            transform.yy = -transform.yy;
        }
        let opacity = [0.0, 0.1, 0.5, 1.0][random.next() as usize % 4];
        let layer = match case % 4 {
            0 => AffineLayer::new(&image, transform, UvRect::FULL, opacity),
            1 => AffineLayer::from_texels(
                &image,
                transform,
                TexelRect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                },
                opacity,
            ),
            2 => AffineLayer::new(
                &image,
                transform,
                UvRect {
                    left: 0.125,
                    top: 0.25,
                    right: 0.875,
                    bottom: 0.75,
                },
                opacity,
            ),
            _ => AffineLayer::from_texels(
                &image,
                transform,
                TexelRect {
                    x: w / 3,
                    y: h / 3,
                    width: w - w / 3,
                    height: h - h / 3,
                },
                opacity,
            ),
        }
        .with_blend_mode(
            [
                SourceBlend::Encoded,
                SourceBlend::LinearSrgb,
                SourceBlend::EncodedPremultipliedSrgb,
                SourceBlend::PremultipliedLinearSrgb,
            ][random.next() as usize % 4],
        );
        compare(
            1 + random.next() % 31,
            1 + random.next() % 29,
            &[layer],
            case % 2 == 0,
        );
    }
}

#[test]
fn budget_and_invalid_input_rejection_precede_any_optimized_pixel_work() {
    let image = RgbaFrame::solid(1, 1, [255; 4]).unwrap();
    let valid = AffineLayer::new(
        &image,
        Affine2 {
            xx: 16.0,
            yy: 16.0,
            ..Affine2::IDENTITY
        },
        UvRect::FULL,
        1.0,
    );
    for limits in [
        CompositeLimits {
            max_layers: 0,
            ..Default::default()
        },
        CompositeLimits {
            max_input_bytes: 3,
            ..Default::default()
        },
        CompositeLimits {
            max_output_bytes: 1023,
            ..Default::default()
        },
        CompositeLimits {
            max_pixel_visits: 511,
            ..Default::default()
        },
    ] {
        reset();
        let fast = composite_affine_target_policy(16, 16, [0; 4], &[valid], limits, false, true);
        let raw = composite_affine_target_policy(16, 16, [0; 4], &[valid], limits, false, false);
        assert_eq!(fast, raw);
        assert!(fast.is_err());
        assert_eq!(stats(), [0; 3]);
    }
    for invalid in [
        AffineLayer {
            opacity: f32::NAN,
            ..valid
        },
        AffineLayer {
            transform: Affine2 {
                xx: f64::NAN,
                ..Affine2::IDENTITY
            },
            ..valid
        },
        AffineLayer::new(
            &image,
            valid.transform,
            UvRect {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 1.0,
            },
            1.0,
        ),
    ] {
        reset();
        let fast = composite_affine_target_policy(
            16,
            16,
            [0; 4],
            &[invalid],
            CompositeLimits::default(),
            false,
            true,
        );
        let raw = composite_affine_target_policy(
            16,
            16,
            [0; 4],
            &[invalid],
            CompositeLimits::default(),
            false,
            false,
        );
        assert_eq!(fast, raw);
        assert!(fast.is_err());
        assert_eq!(stats(), [0; 3]);
    }
    assert_eq!(MAX_AXIS_ROW_INDICES * std::mem::size_of::<u16>(), 16 * 1024);
}
