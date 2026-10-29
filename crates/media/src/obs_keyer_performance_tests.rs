//! Exact pre-optimization regression oracles. These frozen modules deliberately
//! retain the original transfer, gamma, and three-LP-row arithmetic.
#[path = "filter_performance_baseline/color_correction.rs"]
mod baseline_color;
#[path = "filter_performance_baseline/obs_keyer.rs"]
mod baseline_keyer;
#[path = "filter_performance_baseline/source_effects.rs"]
mod baseline_source;

use super::*;
use crate::{
    ColorCorrectionSettings, CraftVideoEffect, RgbaFrame, SourceBlend, SourceEffectsLimits,
    SourceVideoEffect, source_effect_requirements, source_video_effects,
};

const ENCODINGS: [ColorCorrectionInput; 3] = [
    ColorCorrectionInput::LinearAlpha,
    ColorCorrectionInput::EncodedAlpha,
    ColorCorrectionInput::PremultipliedLinearSrgb,
];
const INGRESS: [SourceBlend; 4] = [
    SourceBlend::LinearSrgb,
    SourceBlend::Encoded,
    SourceBlend::EncodedPremultipliedSrgb,
    SourceBlend::PremultipliedLinearSrgb,
];

fn pixels(width: usize, height: usize, seed: u32) -> Vec<u8> {
    let mut state = seed;
    let mut result = Vec::with_capacity(width * height * 4);
    for n in 0..width * height {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let alpha = [0, 1, 128, 254, 255][n % 5];
        let p = match n % 11 {
            0 => [0, 255, 0, alpha],
            1 => [255, 0, 255, alpha],
            2 => [0, 153, 255, alpha],
            3 => [255, 255, 255, alpha],
            4 => [0, 0, 0, alpha],
            _ => [state as u8, (state >> 8) as u8, (state >> 16) as u8, alpha],
        };
        result.extend_from_slice(&p);
    }
    result
}

#[test]
fn byte_decode_table_and_opaque_render_target_roundtrip_are_bit_exact() {
    for byte in 0..=255u8 {
        assert_eq!(
            crate::color_correction::decode(byte).to_bits(),
            baseline_color::decode(byte).to_bits(),
            "decode {byte}"
        );
        assert_eq!(baseline_color::encode(baseline_color::decode(byte)), byte);
        let pixel = [byte, 255 - byte, byte.rotate_left(3), 255];
        for encoding in ENCODINGS {
            assert_eq!(
                sample_lp(pixel, encoding).map(f32::to_bits),
                baseline_keyer::sample_lp(pixel, encoding).map(f32::to_bits),
                "opaque sample {pixel:?} {encoding:?}"
            );
        }
    }
}

#[test]
fn sample_lp_is_bit_exact_for_every_channel_and_alpha_byte() {
    // This exhausts all 65,536 channel/alpha pairs for each input encoding.
    // Premultiplied RGB deliberately includes values greater than alpha.
    for alpha in 0..=255u8 {
        for byte in 0..=255u8 {
            let p = [byte, byte.wrapping_add(77), 255 - byte, alpha];
            for encoding in ENCODINGS {
                assert_eq!(
                    sample_lp(p, encoding).map(f32::to_bits),
                    baseline_keyer::sample_lp(p, encoding).map(f32::to_bits),
                    "sample {p:?} {encoding:?}"
                );
            }
        }
    }
}

#[test]
fn identity_gamma_matches_original_powf_for_finite_representatives() {
    // Exercise raw exponents/mantissas and signed zero on the target libm. The
    // direct pixel/chain oracles below also test actual generated filter values.
    for value in [-0.0f32, 0.0, -1.0, 1.0, f32::MIN_POSITIVE, f32::MAX] {
        assert_eq!(value.to_bits(), value.powf(1.0).to_bits());
    }
    let mut bits = 0x128f_ab73u32;
    for _ in 0..200_000 {
        bits = bits.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let value = f32::from_bits(bits);
        if value.is_finite() {
            assert_eq!(value.to_bits(), value.powf(1.0).to_bits(), "{bits:08x}");
        }
    }
}

fn chroma_settings() -> [ChromaKeySettings; 7] {
    [
        ChromaKeySettings::default(),
        ChromaKeySettings {
            key_color_type: ChromaKeyType::Blue,
            ..Default::default()
        },
        ChromaKeySettings {
            key_color_type: ChromaKeyType::Magenta,
            gamma: 1.0,
            similarity: 1,
            smoothness: 1000,
            spill: 1,
            ..Default::default()
        },
        ChromaKeySettings {
            key_color_type: ChromaKeyType::Custom,
            key_color: [113, 53, 211],
            gamma: -1.0,
            similarity: 777,
            smoothness: 1,
            spill: 1000,
            opacity: 0.37,
            contrast: -3.7,
            brightness: 0.23,
        },
        ChromaKeySettings {
            gamma: 0.37,
            similarity: 120,
            smoothness: 700,
            spill: 600,
            contrast: 0.123456789012345,
            brightness: -0.1,
            ..Default::default()
        },
        ChromaKeySettings {
            gamma: -0.73,
            similarity: 1000,
            smoothness: 1000,
            spill: 1000,
            opacity: 0.0,
            contrast: 4.0,
            brightness: 1.0,
            ..Default::default()
        },
        ChromaKeySettings {
            similarity: 80,
            smoothness: 1,
            spill: 1,
            opacity: 0.999,
            contrast: -4.0,
            brightness: -1.0,
            ..Default::default()
        },
    ]
}

#[test]
fn rolling_distance_cache_matches_frozen_three_row_oracle_exactly() {
    for (width, height) in [
        (1, 1),
        (1, 19),
        (17, 1),
        (2, 2),
        (2, 13),
        (23, 2),
        (3, 7),
        (31, 23),
        (64, 3),
        (127, 5),
        (1920, 3),
        (4096, 2),
    ] {
        for seed in [1, 0x834f_1189] {
            let original = pixels(width, height, seed);
            for encoding in ENCODINGS {
                for settings in chroma_settings() {
                    let mut expected = original.clone();
                    let mut actual = original.clone();
                    let mut before_scratch = vec![[0.0; 4]; width * 3];
                    let mut after_scratch = vec![0u32; width * 12];
                    baseline_keyer::chroma_in_place(
                        &mut expected,
                        width,
                        height,
                        encoding,
                        baseline_keyer::PreparedKeyer::chroma(settings),
                        &mut before_scratch,
                    );
                    chroma_in_place(
                        &mut actual,
                        width,
                        height,
                        encoding,
                        PreparedKeyer::chroma(settings),
                        &mut after_scratch,
                    )
                    .unwrap();
                    assert_eq!(
                        actual, expected,
                        "{width}x{height} seed={seed} {encoding:?} {settings:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn all_admission_dimensions_match_the_original_and_rejection_keeps_the_input() {
    let input = RgbaFrame::new(13, 11, pixels(13, 11, 0x28f3_9b71)).unwrap();
    let original = input.pixels().to_vec();
    let effects = [
        SourceVideoEffect::ColorCorrection(ColorCorrectionSettings::default()),
        SourceVideoEffect::ChromaKey(ChromaKeySettings::default()),
    ];
    let required = source_effect_requirements(&input, &effects).unwrap();
    let exact = SourceEffectsLimits {
        max_output_bytes: required.output_bytes,
        max_workspace_bytes: required.workspace_bytes,
        max_pixel_stages: required.pixel_stages,
        max_texel_visits: required.texel_visits,
    };
    let limits = [
        SourceEffectsLimits {
            max_output_bytes: required.output_bytes - 1,
            ..exact
        },
        SourceEffectsLimits {
            max_workspace_bytes: required.workspace_bytes - 1,
            ..exact
        },
        SourceEffectsLimits {
            max_pixel_stages: required.pixel_stages - 1,
            ..exact
        },
        SourceEffectsLimits {
            max_texel_visits: required.texel_visits - 1,
            ..exact
        },
    ];
    for limit in limits {
        assert!(matches!(
            source_video_effects(&input, SourceBlend::LinearSrgb, &effects, limit),
            Err(crate::MediaError::BudgetExceeded)
        ));
        assert!(matches!(
            baseline_source::source_video_effects(&input, SourceBlend::LinearSrgb, &effects, limit),
            Err(crate::MediaError::BudgetExceeded)
        ));
        assert_eq!(input.pixels(), original);
    }
}

#[test]
fn correction_and_point_key_match_original_for_every_byte_and_edge_alpha() {
    let corrections = [
        ColorCorrectionSettings::default(),
        ColorCorrectionSettings {
            gamma: -3.0,
            contrast: -4.0,
            brightness: 0.13,
            saturation: -1.0,
            hue_shift: 179.0,
            opacity: 0.37,
            color_multiply: [137, 53, 251],
            color_add: [1, 3, 27],
        },
        ColorCorrectionSettings {
            gamma: 3.0,
            contrast: 4.0,
            brightness: -0.73,
            saturation: 5.0,
            hue_shift: -179.0,
            opacity: 0.001,
            color_multiply: [0, 255, 128],
            color_add: [255, 0, 3],
        },
        ColorCorrectionSettings {
            gamma: 0.31,
            brightness: 0.2,
            contrast: 0.3,
            ..Default::default()
        },
        ColorCorrectionSettings {
            gamma: 0.0,
            brightness: -0.2,
            saturation: 0.7,
            opacity: 0.6,
            ..Default::default()
        },
    ];
    let keys = [
        ColorKeySettings::default(),
        ColorKeySettings {
            key_color_type: ColorKeyType::Blue,
            gamma: 0.37,
            smoothness: 1,
            similarity: 1000,
            ..Default::default()
        },
        ColorKeySettings {
            key_color_type: ColorKeyType::Red,
            gamma: -1.0,
            contrast: -4.0,
            brightness: 0.13,
            opacity: 0.5,
            ..Default::default()
        },
        ColorKeySettings {
            key_color_type: ColorKeyType::Magenta,
            gamma: 1.0,
            similarity: 1,
            smoothness: 1000,
            ..Default::default()
        },
        ColorKeySettings {
            key_color_type: ColorKeyType::Custom,
            key_color: [33, 201, 145],
            gamma: 0.0,
            contrast: 4.0,
            brightness: -1.0,
            opacity: 0.001,
            ..Default::default()
        },
    ];
    for encoding in ENCODINGS {
        for alpha in [0, 1, 128, 254, 255] {
            for byte in 0..=255u8 {
                let p = [byte, byte.wrapping_mul(37), 255 - byte, alpha];
                for settings in corrections {
                    assert_eq!(
                        crate::color_correction::Prepared::new(settings).pixel(p, encoding),
                        baseline_color::Prepared::new(settings).pixel(p, encoding),
                        "correction {p:?} {encoding:?} {settings:?}"
                    );
                }
                for settings in keys {
                    assert_eq!(
                        PreparedKeyer::color(settings).pixel(p, encoding),
                        baseline_keyer::PreparedKeyer::color(settings).pixel(p, encoding),
                        "color key {p:?} {encoding:?} {settings:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn ordered_mixed_effect_chains_match_original_bytes_blend_and_admission() {
    let chroma = SourceVideoEffect::ChromaKey(chroma_settings()[4]);
    let default_chroma = SourceVideoEffect::ChromaKey(ChromaKeySettings::default());
    let correction = SourceVideoEffect::ColorCorrection(ColorCorrectionSettings {
        gamma: 0.31,
        brightness: 0.12,
        opacity: 0.57,
        ..Default::default()
    });
    let default_correction = SourceVideoEffect::ColorCorrection(ColorCorrectionSettings::default());
    let color = SourceVideoEffect::ColorKey(ColorKeySettings {
        key_color_type: ColorKeyType::Custom,
        key_color: [113, 211, 53],
        gamma: -0.27,
        contrast: 0.17,
        brightness: 0.05,
        ..Default::default()
    });
    let craft = SourceVideoEffect::Craft(CraftVideoEffect::BrightnessContrast {
        brightness: 12.5,
        contrast: -31.0,
        legacy: false,
    });
    let chains: &[&[SourceVideoEffect]] = &[
        &[default_chroma],
        &[chroma, default_chroma],
        &[correction, chroma],
        &[chroma, correction],
        &[color, chroma, default_correction],
        &[
            craft,
            correction,
            chroma,
            craft,
            color,
            default_chroma,
            correction,
        ],
        &[
            default_correction,
            default_chroma,
            craft,
            default_correction,
        ],
    ];
    for (width, height) in [(1, 1), (1, 17), (19, 1), (2, 2), (13, 11)] {
        let input = RgbaFrame::new(
            width,
            height,
            pixels(width as usize, height as usize, 0x2f3c_9587),
        )
        .unwrap();
        for ingress in INGRESS {
            for effects in chains {
                let required = source_effect_requirements(&input, effects).unwrap();
                assert_eq!(
                    required,
                    baseline_source::source_effect_requirements(&input, effects).unwrap()
                );
                let limits = SourceEffectsLimits {
                    max_output_bytes: required.output_bytes,
                    max_workspace_bytes: required.workspace_bytes,
                    max_pixel_stages: required.pixel_stages,
                    max_texel_visits: required.texel_visits,
                };
                let expected =
                    baseline_source::source_video_effects(&input, ingress, effects, limits)
                        .unwrap();
                let actual = source_video_effects(&input, ingress, effects, limits).unwrap();
                assert_eq!(actual.blend, expected.blend);
                assert_eq!(
                    actual.image.pixels(),
                    expected.image.pixels(),
                    "chain {width}x{height} {ingress:?} {effects:?}"
                );
                assert_eq!(required.workspace_bytes, width as usize * 48);
            }
        }
    }
}

#[path = "obs_keyer_alpha_tests.rs"]
mod alpha_tests;

#[path = "obs_keyer_common_tests.rs"]
mod common_tests;

#[path = "obs_keyer_packed_tests.rs"]
mod packed_tests;
