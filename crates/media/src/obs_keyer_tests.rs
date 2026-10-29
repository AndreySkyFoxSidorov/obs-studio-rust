use super::*;
use crate::{
    RgbaFrame, SourceBlend, SourceEffectsLimits, SourceVideoEffect, source_effect_requirements,
    source_video_effects,
};
fn apply(input: &RgbaFrame, effects: &[SourceVideoEffect]) -> crate::SourceEffectFrame {
    source_video_effects(
        input,
        SourceBlend::LinearSrgb,
        effects,
        SourceEffectsLimits {
            max_output_bytes: input.byte_len(),
            max_workspace_bytes: 3 * input.width() as usize * 16,
            max_pixel_stages: input.byte_len() * 2,
            max_texel_visits: 100000,
        },
    )
    .unwrap()
}
#[test]
fn independent_scalar_distance_and_spatial_staggered_oracles() {
    let green = RgbaFrame::solid(1, 1, [0, 230, 0, 255]).unwrap();
    let got = apply(
        &green,
        &[SourceVideoEffect::ColorKey(ColorKeySettings::default())],
    );
    assert_eq!(got.image.pixels(), &[0, 146, 0, 92]);
    assert_eq!(got.blend, SourceBlend::PremultipliedLinearSrgb);
    let mut pixels = [0, 255, 0, 255].repeat(9);
    pixels[0..4].copy_from_slice(&[255, 0, 0, 255]);
    let input = RgbaFrame::new(3, 3, pixels).unwrap();
    let settings = ChromaKeySettings {
        similarity: 80,
        smoothness: 80,
        spill: 1,
        ..Default::default()
    };
    let got = apply(&input, &[SourceVideoEffect::ChromaKey(settings)]);
    // Independent scalar result: d=.09529806470, mask=.08362184773.
    assert_eq!(&got.image.pixels()[16..20], &[0, 82, 0, 21]);
    let got = apply(
        &input,
        &[SourceVideoEffect::ChromaKey(ChromaKeySettings {
            spill: 100,
            ..settings
        })],
    );
    assert_eq!(&got.image.pixels()[16..20], &[67, 70, 67, 21]);
    assert_eq!(&input.pixels()[16..20], &[0, 255, 0, 255]);
}
#[test]
fn chroma_is_not_point_key_and_blue_presets_are_different() {
    let mut pixels = [255, 0, 0, 255].repeat(9);
    pixels[16..20].copy_from_slice(&[0, 255, 0, 255]);
    let input = RgbaFrame::new(3, 3, pixels).unwrap();
    assert_eq!(
        apply(&input, &[SourceVideoEffect::ChromaKey(Default::default())])
            .image
            .pixels()[19],
        255
    );
    assert_eq!(
        apply(&input, &[SourceVideoEffect::ColorKey(Default::default())])
            .image
            .pixels()[19],
        0
    );
    let blue = RgbaFrame::solid(1, 1, [0, 153, 255, 255]).unwrap();
    assert_eq!(
        apply(
            &blue,
            &[SourceVideoEffect::ChromaKey(ChromaKeySettings {
                key_color_type: ChromaKeyType::Blue,
                ..Default::default()
            })]
        )
        .image
        .pixels()[3],
        0
    );
    assert_eq!(
        apply(
            &blue,
            &[SourceVideoEffect::ColorKey(ColorKeySettings {
                key_color_type: ColorKeyType::Blue,
                ..Default::default()
            })]
        )
        .image
        .pixels()[3],
        255
    );
}
#[test]
fn three_row_ring_matches_independent_out_of_place_f64_sampling_at_clamped_edges() {
    // Opaque pixels isolate spatial geometry; the oracle does not use production
    // preparation, row slots, paired sampler, transfer helpers, or source curve.
    let mut pixels = Vec::new();
    for i in 0..35u16 {
        pixels.extend_from_slice(&[
            (i * 37 % 256) as u8,
            (i * 71 % 256) as u8,
            (i * 13 % 256) as u8,
            255,
        ]);
    }
    let input = RgbaFrame::new(7, 5, pixels).unwrap();
    let got = apply(
        &input,
        &[SourceVideoEffect::ChromaKey(ChromaKeySettings {
            similarity: 120,
            smoothness: 700,
            spill: 600,
            ..Default::default()
        })],
    );
    let d = |v: f64| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let e = |v: f64| {
        if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    let cbcr = |rgb: [f64; 3]| {
        [
            rgb[0] * -0.100644 + rgb[1] * -0.338572 + rgb[2] * 0.439216 + 0.501961,
            rgb[0] * 0.439216 + rgb[1] * -0.398942 + rgb[2] * -0.040274 + 0.501961,
        ]
    };
    let key = cbcr([0.0, 1.0, 0.0]);
    let distance = |rgb: [f64; 3]| {
        let c = cbcr(rgb.map(e));
        ((c[0] - key[0]).powi(2) + (c[1] - key[1]).powi(2)).sqrt()
    };
    let get = |x: isize, y: isize| {
        let x = x.clamp(0, 6) as usize;
        let y = y.clamp(0, 4) as usize;
        std::array::from_fn::<_, 3, _>(
            |c| d(f64::from(input.pixels()[(y * 7 + x) * 4 + c]) / 255.0),
        )
    };
    for y in 0..5isize {
        for x in 0..7isize {
            let mut sum = 0.0;
            for (ax, ay, bx, by) in [(-1, -1, -1, 0), (1, 0, 1, 1), (-1, 1, 0, 1), (0, -1, 1, -1)] {
                let a = get(x + ax, y + ay);
                let b = get(x + bx, y + by);
                sum += distance(std::array::from_fn(|c| (a[c] + b[c]) * 0.5));
            }
            let rgb = get(x, y);
            let base = (2.0 * sum + distance(rgb)) / 9.0 - 0.120;
            let alpha = (base / 0.700).clamp(0.0, 1.0).powf(1.5);
            let spill = (base / 0.600).clamp(0.0, 1.0).powf(1.5);
            let gray = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
            let mut want = [0u8; 4];
            for c in 0..3 {
                want[c] = (e(((gray + (rgb[c] - gray) * spill) * alpha).clamp(0.0, 1.0)) * 255.0)
                    .round() as u8;
            }
            want[3] = (alpha * 255.0).round() as u8;
            let at = (y as usize * 7 + x as usize) * 4;
            for (c, expected) in want.iter().enumerate() {
                assert!(
                    got.image.pixels()[at + c].abs_diff(*expected) <= 1,
                    "{x},{y} channel{c}: {:?} vs {want:?}",
                    &got.image.pixels()[at..at + 4]
                );
            }
        }
    }
}
#[test]
fn all_admission_happens_before_work_and_stages_sample_previous_stage() {
    let input = RgbaFrame::solid(2, 2, [255, 0, 0, 255]).unwrap();
    let effects = [SourceVideoEffect::ChromaKey(Default::default())];
    let r = source_effect_requirements(&input, &effects).unwrap();
    assert_eq!(
        (
            r.output_bytes,
            r.workspace_bytes,
            r.pixel_stages,
            r.texel_visits
        ),
        (16, 96, 4, 36)
    );
    for limits in [
        SourceEffectsLimits {
            max_output_bytes: 15,
            max_workspace_bytes: 96,
            max_pixel_stages: 4,
            max_texel_visits: 36,
        },
        SourceEffectsLimits {
            max_output_bytes: 16,
            max_workspace_bytes: 95,
            max_pixel_stages: 4,
            max_texel_visits: 36,
        },
        SourceEffectsLimits {
            max_output_bytes: 16,
            max_workspace_bytes: 96,
            max_pixel_stages: 3,
            max_texel_visits: 36,
        },
        SourceEffectsLimits {
            max_output_bytes: 16,
            max_workspace_bytes: 96,
            max_pixel_stages: 4,
            max_texel_visits: 35,
        },
    ] {
        assert!(matches!(
            source_video_effects(&input, SourceBlend::LinearSrgb, &effects, limits),
            Err(crate::MediaError::BudgetExceeded)
        ));
    }
    let invalid = SourceVideoEffect::ChromaKey(ChromaKeySettings {
        spill: 0,
        ..Default::default()
    });
    assert!(source_effect_requirements(&input, &[invalid]).is_err());
    let color = SourceVideoEffect::ColorKey(ColorKeySettings {
        key_color_type: ColorKeyType::Red,
        ..Default::default()
    });
    let sequential = apply(&apply(&input, &[color]).image, &effects);
    let combined = apply(&input, &[color, effects[0]]);
    assert_eq!(sequential.image.pixels(), combined.image.pixels());
    assert_eq!(combined.image.pixels(), &[0; 16]);
}
#[test]
fn keyer_curves_alpha_and_double_preparation_do_not_collapse_to_craft_controls() {
    let input = RgbaFrame::solid(1, 1, [255, 255, 255, 128]).unwrap();
    let got = apply(
        &input,
        &[SourceVideoEffect::ColorKey(ColorKeySettings {
            brightness: 0.1,
            opacity: 0.5,
            ..Default::default()
        })],
    );
    // Offscreen LP input is decode(188), then unpremul, then brightness,
    // then opacity. Alpha is64; LP can contain RGB energy independently.
    assert_eq!(got.image.pixels()[3], 64);
    assert!(got.image.pixels()[0] > 128);
    for gamma in [f64::NAN, -1.0001, 1.0001] {
        assert!(
            ColorKeySettings {
                gamma,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    let contrast = 0.123456789012345;
    let prepared = PreparedKeyer::color(ColorKeySettings {
        contrast,
        ..Default::default()
    });
    assert_eq!(
        prepared.contrast.to_bits(),
        ((contrast + 1.0) as f32).to_bits()
    );
}
