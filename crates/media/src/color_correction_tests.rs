use super::*;

fn apply(
    pixel: [u8; 4],
    input: ColorCorrectionInput,
    settings: ColorCorrectionSettings,
) -> [u8; 4] {
    let image = RgbaFrame::new(1, 1, pixel.to_vec()).unwrap();
    color_correct_v2(
        &image,
        input,
        settings,
        ColorCorrectionLimits {
            max_output_bytes: 4,
            max_pixel_stages: 1,
        },
    )
    .unwrap()
    .image()
    .pixels()
    .try_into()
    .unwrap()
}

#[test]
fn literal_sdr_controls_use_linear_light_and_v2_contrast_without_a_midpoint() {
    // Independent scalar oracles: D(128)=0.2158605001; E(sqrt(D))=181,
    // E(D^2)=61, E(2D)=176, E(D/2)=92, E(.1)=89.
    for (settings, input, expected) in [
        (
            ColorCorrectionSettings {
                gamma: 1.0,
                ..Default::default()
            },
            [128, 128, 128, 255],
            [181, 181, 181, 255],
        ),
        (
            ColorCorrectionSettings {
                gamma: -1.0,
                ..Default::default()
            },
            [128, 128, 128, 255],
            [61, 61, 61, 255],
        ),
        (
            ColorCorrectionSettings {
                contrast: 1.0,
                ..Default::default()
            },
            [128, 128, 128, 255],
            [176, 176, 176, 255],
        ),
        (
            ColorCorrectionSettings {
                contrast: -1.0,
                ..Default::default()
            },
            [128, 128, 128, 255],
            [92, 92, 92, 255],
        ),
        (
            ColorCorrectionSettings {
                brightness: 0.1,
                ..Default::default()
            },
            [0, 0, 0, 255],
            [89, 89, 89, 255],
        ),
        // Contrast then brightness, not (D+.1)*2, and no encoded-space gamma.
        (
            ColorCorrectionSettings {
                contrast: 1.0,
                brightness: 0.1,
                ..Default::default()
            },
            [128, 128, 128, 255],
            [193, 193, 193, 255],
        ),
    ] {
        assert_eq!(
            apply(input, ColorCorrectionInput::LinearAlpha, settings),
            expected
        );
    }
}

#[test]
fn literal_luminance_quaternion_and_rgb_washes_are_not_hsv_or_encoded_multiply() {
    assert_eq!(
        apply(
            [255, 0, 0, 255],
            ColorCorrectionInput::LinearAlpha,
            ColorCorrectionSettings {
                saturation: -1.0,
                ..Default::default()
            }
        ),
        [149, 149, 149, 255], // E(.299), not an encoded luminance byte76.
    );
    assert_eq!(
        apply(
            [255, 0, 0, 255],
            ColorCorrectionInput::LinearAlpha,
            ColorCorrectionSettings {
                hue_shift: 120.0,
                ..Default::default()
            }
        ),
        [0, 0, 255, 255],
    );
    assert_eq!(
        apply(
            [255, 0, 0, 255],
            ColorCorrectionInput::LinearAlpha,
            ColorCorrectionSettings {
                hue_shift: -120.0,
                ..Default::default()
            }
        ),
        [0, 255, 0, 255],
    );
    assert_eq!(
        apply(
            [255; 4],
            ColorCorrectionInput::LinearAlpha,
            ColorCorrectionSettings {
                color_multiply: [128, 64, 32],
                ..Default::default()
            }
        ),
        [128, 64, 32, 255],
    );
    assert_eq!(
        apply(
            [0, 0, 0, 255],
            ColorCorrectionInput::LinearAlpha,
            ColorCorrectionSettings {
                color_add: [128, 64, 32],
                ..Default::default()
            }
        ),
        [128, 64, 32, 255],
    );
}

#[test]
fn homogeneous_alpha_translations_and_premultiplied_outputs_preserve_energy() {
    assert_eq!(
        apply(
            [255; 4],
            ColorCorrectionInput::LinearAlpha,
            ColorCorrectionSettings {
                opacity: 0.5,
                ..Default::default()
            }
        ),
        [188, 188, 188, 128],
    );
    // Shader leaves homogeneous w=alpha: brightness*alpha, then RGB*alpha.
    assert_eq!(
        apply(
            [0, 0, 0, 128],
            ColorCorrectionInput::LinearAlpha,
            ColorCorrectionSettings {
                brightness: 0.1,
                ..Default::default()
            }
        ),
        [44, 44, 44, 128],
    );
    // Color Add follows the opacity matrix, hence its translation is scaled
    // by the resulting alpha before the final premultiplication as well.
    assert_eq!(
        apply(
            [0, 0, 0, 255],
            ColorCorrectionInput::LinearAlpha,
            ColorCorrectionSettings {
                opacity: 0.5,
                color_add: [255; 3],
                ..Default::default()
            }
        ),
        [137, 137, 137, 128], // E(.5*.5), not E(.5).
    );
    assert_eq!(
        apply(
            [255, 255, 255, 128],
            ColorCorrectionInput::LinearAlpha,
            ColorCorrectionSettings {
                brightness: 1.0,
                ..Default::default()
            }
        ),
        [225, 225, 225, 128],
    );
    // LP RGB>alpha is intentional. A straight RGB clamp would cap this at188.
    assert_eq!(
        apply(
            [255, 0, 0, 1],
            ColorCorrectionInput::PremultipliedLinearSrgb,
            Default::default()
        ),
        [255, 0, 0, 1],
    );
}

#[test]
fn original_image_alpha_is_rounded_before_srgb_sampling_and_never_applied_twice() {
    assert_eq!(
        apply(
            [255, 255, 255, 128],
            ColorCorrectionInput::EncodedAlpha,
            ColorCorrectionSettings {
                opacity: 0.5,
                ..Default::default()
            }
        ),
        [92, 92, 92, 64], // E(D(128)/2), not a Color Source's137.
    );
    for rgb in 0..=255_u8 {
        let original = [rgb, 255 - rgb, rgb / 2, 83];
        let expected = original.map(|value| ((u32::from(value) * 83 + 127) / 255) as u8);
        let result = apply(
            original,
            ColorCorrectionInput::EncodedAlpha,
            Default::default(),
        );
        assert_eq!(&result[..3], &expected[..3]);
        assert_eq!(result[3], 83);
        assert_eq!(
            apply(
                result,
                ColorCorrectionInput::PremultipliedLinearSrgb,
                Default::default()
            ),
            result,
        );
    }
    for input in [
        ColorCorrectionInput::LinearAlpha,
        ColorCorrectionInput::EncodedAlpha,
        ColorCorrectionInput::PremultipliedLinearSrgb,
    ] {
        assert_eq!(
            apply(
                [255, 10, 220, 0],
                input,
                ColorCorrectionSettings {
                    brightness: 1.0,
                    color_add: [255; 3],
                    ..Default::default()
                }
            ),
            [0; 4]
        );
        assert_eq!(
            apply(
                [255; 4],
                input,
                ColorCorrectionSettings {
                    opacity: 0.0,
                    brightness: 1.0,
                    ..Default::default()
                }
            ),
            [0; 4]
        );
    }
}

#[test]
fn each_obs_unorm_target_clamps_before_the_next_filter_not_after_a_fused_chain() {
    let darken = ColorCorrectionSettings {
        brightness: -1.0,
        ..Default::default()
    };
    let brighten = ColorCorrectionSettings {
        brightness: 1.0,
        ..Default::default()
    };
    let intermediate = apply(
        [128, 128, 128, 255],
        ColorCorrectionInput::LinearAlpha,
        darken,
    );
    assert_eq!(intermediate, [0, 0, 0, 255]);
    assert_eq!(
        apply(
            intermediate,
            ColorCorrectionInput::PremultipliedLinearSrgb,
            brighten
        ),
        [255; 4]
    );
    // Fusing the signed brightness values would incorrectly return gray128.
}

#[test]
fn all_settings_and_limits_are_checked_before_allocation_and_input_is_immutable() {
    let image = RgbaFrame::solid(2, 2, [30, 40, 50, 60]).unwrap();
    for limits in [
        ColorCorrectionLimits {
            max_output_bytes: 15,
            max_pixel_stages: 4,
        },
        ColorCorrectionLimits {
            max_output_bytes: 16,
            max_pixel_stages: 3,
        },
    ] {
        assert!(matches!(
            color_correct_v2(
                &image,
                ColorCorrectionInput::LinearAlpha,
                Default::default(),
                limits
            ),
            Err(MediaError::BudgetExceeded)
        ));
    }
    for settings in [
        ColorCorrectionSettings {
            gamma: f64::NAN,
            ..Default::default()
        },
        ColorCorrectionSettings {
            gamma: 3.01,
            ..Default::default()
        },
        ColorCorrectionSettings {
            contrast: -4.01,
            ..Default::default()
        },
        ColorCorrectionSettings {
            brightness: f32::INFINITY,
            ..Default::default()
        },
        ColorCorrectionSettings {
            saturation: 5.01,
            ..Default::default()
        },
        ColorCorrectionSettings {
            hue_shift: -180.01,
            ..Default::default()
        },
        ColorCorrectionSettings {
            opacity: -f32::EPSILON,
            ..Default::default()
        },
    ] {
        assert!(settings.validate().is_err());
    }
    assert_eq!(image.pixels(), &[30, 40, 50, 60].repeat(4));
}
