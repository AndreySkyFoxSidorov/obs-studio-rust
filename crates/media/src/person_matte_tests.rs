use super::*;

const BLENDS: [SourceBlend; 4] = [
    SourceBlend::Encoded,
    SourceBlend::LinearSrgb,
    SourceBlend::EncodedPremultipliedSrgb,
    SourceBlend::PremultipliedLinearSrgb,
];

fn prepare_limits(input: &RgbaFrame) -> PersonMatteLimits {
    PersonMatteLimits {
        max_input_bytes: input.byte_len(),
        max_output_bytes: PERSON_RGB_LEN * 4,
        max_pixel_visits: PERSON_MASK_LEN as u64 * 4,
    }
}

fn apply_limits(input: &RgbaFrame) -> PersonMatteLimits {
    PersonMatteLimits {
        max_input_bytes: input.byte_len() + PERSON_MASK_LEN * 4,
        max_output_bytes: input.byte_len(),
        max_pixel_visits: PERSON_MASK_LEN as u64 + input.byte_len() as u64 / 4 * 5,
    }
}

fn decode_reference(byte: u8) -> f64 {
    let value = f64::from(byte) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn encode_reference(linear: f64) -> f64 {
    let value = linear.clamp(0.0, 1.0);
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

#[test]
fn straight_tensor_is_rgb_nhwc_at_fixed_identity_centers_for_all_straight_policies() {
    let mut bytes = Vec::new();
    for y in 0..256u32 {
        for x in 0..256u32 {
            bytes.extend_from_slice(&[x as u8, y as u8, ((x + y) / 2) as u8, (x ^ y) as u8]);
        }
    }
    let input = RgbaFrame::new(256, 256, bytes).unwrap();
    for blend in &BLENDS[..3] {
        let output = prepare_person_rgb(&input, *blend, prepare_limits(&input)).unwrap();
        assert_eq!(PERSON_MATTE_EDGE, 256);
        assert_eq!(PERSON_MASK_LEN, 65536);
        assert_eq!(output.len(), 196608);
        for (source, actual) in input.pixels().chunks_exact(4).zip(output.chunks_exact(3)) {
            assert_eq!(
                actual,
                [
                    f32::from(source[0]) / 255.0,
                    f32::from(source[1]) / 255.0,
                    f32::from(source[2]) / 255.0,
                ]
            );
        }
    }
}

#[test]
fn one_pixel_preparation_retains_straight_hidden_rgb_and_unpremultiplies_only_lp() {
    let source = RgbaFrame::solid(1, 1, [17, 61, 239, 0]).unwrap();
    let before = source.clone();
    for blend in &BLENDS[..3] {
        let tensor = prepare_person_rgb(&source, *blend, prepare_limits(&source)).unwrap();
        let expected = [17.0 / 255.0, 61.0 / 255.0, 239.0 / 255.0];
        assert!(tensor.chunks_exact(3).all(|pixel| pixel == expected));
    }
    let transparent_lp = prepare_person_rgb(
        &source,
        SourceBlend::PremultipliedLinearSrgb,
        prepare_limits(&source),
    )
    .unwrap();
    assert!(transparent_lp.iter().all(|value| *value == 0.0));
    assert_eq!(source, before);

    for alpha in [1, 63, 128, 254, 255] {
        let lp = RgbaFrame::solid(1, 1, [188, 99, 19, alpha]).unwrap();
        let tensor = prepare_person_rgb(
            &lp,
            SourceBlend::PremultipliedLinearSrgb,
            prepare_limits(&lp),
        )
        .unwrap();
        let expected = [188, 99, 19]
            .map(|byte| encode_reference(decode_reference(byte) / (f64::from(alpha) / 255.0)));
        for pixel in tensor.chunks_exact(3) {
            for channel in 0..3 {
                // Independent f64 IEC transfer compared with the supported
                // f32 transfer. This is a float-tensor accuracy bound, not a
                // permission to weaken any RGBA-byte preservation assertion.
                assert!((f64::from(pixel[channel]) - expected[channel]).abs() < 0.000002);
                assert!((0.0..=1.0).contains(&pixel[channel]));
            }
        }
    }
}

#[test]
fn two_dimensional_preparation_uses_pixel_centers_and_clamped_borders() {
    let source = RgbaFrame::new(
        2,
        2,
        vec![
            255, 0, 0, 0, 0, 255, 0, 255, 0, 0, 255, 1, 255, 255, 255, 128,
        ],
    )
    .unwrap();
    let actual =
        prepare_person_rgb(&source, SourceBlend::LinearSrgb, prepare_limits(&source)).unwrap();
    for y in [0, 63, 64, 127, 128, 191, 192, 255] {
        for x in [0, 63, 64, 127, 128, 191, 192, 255] {
            let u = ((x as f64 + 0.5) / 128.0 - 0.5).clamp(0.0, 1.0);
            let v = ((y as f64 + 0.5) / 128.0 - 0.5).clamp(0.0, 1.0);
            let expected = [(1.0 - u) * (1.0 - v) + u * v, u, v];
            for channel in 0..3 {
                let value = actual[(y * 256 + x) * 3 + channel];
                assert!((f64::from(value) - expected[channel]).abs() <= 0.0000002);
            }
        }
    }
    assert_eq!(&actual[..3], &[1.0, 0.0, 0.0]);
    assert_eq!(&actual[actual.len() - 3..], &[1.0, 1.0, 1.0]);
}

#[test]
fn one_pixel_axis_is_clamped_in_preparation_and_application() {
    for (width, height) in [(1, 1), (1, 3), (3, 1), (1, 257), (257, 1)] {
        let source = RgbaFrame::solid(width, height, [91, 173, 29, 201]).unwrap();
        let tensor =
            prepare_person_rgb(&source, SourceBlend::Encoded, prepare_limits(&source)).unwrap();
        assert!(
            tensor
                .chunks_exact(3)
                .all(|pixel| { pixel == [91.0 / 255.0, 173.0 / 255.0, 29.0 / 255.0] })
        );
        let output = apply_person_matte(
            &source,
            SourceBlend::Encoded,
            &vec![0.5; PERSON_MASK_LEN],
            0.5,
            1.0,
            apply_limits(&source),
        )
        .unwrap();
        assert!(
            output
                .pixels()
                .chunks_exact(4)
                .all(|p| p == [91, 173, 29, 101])
        );
    }
}

#[test]
fn application_preserves_straight_rgb_and_scales_lp_emission_independently_of_alpha() {
    let bytes = [
        [255, 128, 0, 0],
        [255, 128, 0, 1],
        [255, 128, 0, 129],
        [255, 128, 0, 255],
    ]
    .concat();
    let input = RgbaFrame::new(4, 1, bytes).unwrap();
    let before = input.clone();
    for blend in BLENDS {
        let identity = apply_person_matte(
            &input,
            blend,
            &vec![1.0; PERSON_MASK_LEN],
            0.5,
            1.0,
            apply_limits(&input),
        )
        .unwrap();
        assert_eq!(identity, input);
        let half = apply_person_matte(
            &input,
            blend,
            &vec![0.5; PERSON_MASK_LEN],
            0.5,
            1.0,
            apply_limits(&input),
        )
        .unwrap();
        let rgb = if blend == SourceBlend::PremultipliedLinearSrgb {
            [188, 92, 0]
        } else {
            [255, 128, 0]
        };
        for (pixel, alpha) in half.pixels().chunks_exact(4).zip([0, 1, 65, 128]) {
            assert_eq!(&pixel[..3], &rgb);
            assert_eq!(pixel[3], alpha);
        }
        let zero = apply_person_matte(
            &input,
            blend,
            &vec![0.0; PERSON_MASK_LEN],
            0.5,
            1.0,
            apply_limits(&input),
        )
        .unwrap();
        for pixel in zero.pixels().chunks_exact(4) {
            assert_eq!(pixel[3], 0);
            assert_eq!(
                &pixel[..3],
                if blend == SourceBlend::PremultipliedLinearSrgb {
                    &[0, 0, 0]
                } else {
                    &[255, 128, 0]
                }
            );
        }
    }
    assert_eq!(input, before);
}

// Independent four-weight f64 oracle; fixture samples and coordinates are
// dyadic so the literal output-byte oracle is exact without a tolerance.
fn mask_oracle(mask: &[f32], x: usize, y: usize, width: usize, height: usize) -> f64 {
    let sx = ((x as f64 + 0.5) * 256.0 / width as f64 - 0.5).clamp(0.0, 255.0);
    let sy = ((y as f64 + 0.5) * 256.0 / height as f64 - 0.5).clamp(0.0, 255.0);
    let left = sx.floor() as usize;
    let top = sy.floor() as usize;
    let right = (left + 1).min(255);
    let bottom = (top + 1).min(255);
    let u = sx - left as f64;
    let v = sy - top as f64;
    f64::from(mask[top * 256 + left]) * (1.0 - u) * (1.0 - v)
        + f64::from(mask[top * 256 + right]) * u * (1.0 - v)
        + f64::from(mask[bottom * 256 + left]) * (1.0 - u) * v
        + f64::from(mask[bottom * 256 + right]) * u * v
}

#[test]
fn full_resolution_mask_uses_both_axes_and_exact_center_aligned_upsampling() {
    let mask: Vec<_> = (0..PERSON_MASK_LEN)
        .map(|i| (((i % 256) * 3 + (i / 256) * 2) % 5) as f32 * 0.25)
        .collect();
    let source = RgbaFrame::solid(512, 512, [19, 71, 139, 255]).unwrap();
    let output = apply_person_matte(
        &source,
        SourceBlend::LinearSrgb,
        &mask,
        0.5,
        1.0,
        apply_limits(&source),
    )
    .unwrap();
    for y in 0..512 {
        for x in 0..512 {
            let expected = (mask_oracle(&mask, x, y, 512, 512) * 255.0).round() as u8;
            let offset = (y * 512 + x) * 4;
            assert_eq!(
                &output.pixels()[offset..offset + 4],
                &[19, 71, 139, expected]
            );
        }
    }
    let mut center = vec![0.0; PERSON_MASK_LEN];
    center[127 * 256 + 128] = 0.25;
    center[128 * 256 + 127] = 0.75;
    center[128 * 256 + 128] = 1.0;
    let single = RgbaFrame::solid(1, 1, [19, 71, 139, 255]).unwrap();
    let sampled = apply_person_matte(
        &single,
        SourceBlend::Encoded,
        &center,
        0.5,
        1.0,
        apply_limits(&single),
    )
    .unwrap();
    assert_eq!(sampled.pixels(), &[19, 71, 139, 128]);
}

#[test]
fn threshold_softness_uses_canonical_linear_mapping_after_interpolation() {
    let source = RgbaFrame::solid(1, 1, [11, 23, 37, 255]).unwrap();
    for (probability, expected) in [
        (0.0, 0),
        (0.375, 0),
        (0.4375, 64),
        (0.5, 128),
        (0.5625, 191),
        (0.625, 255),
        (1.0, 255),
    ] {
        let output = apply_person_matte(
            &source,
            SourceBlend::Encoded,
            &vec![probability; PERSON_MASK_LEN],
            0.5,
            0.25,
            apply_limits(&source),
        )
        .unwrap();
        assert_eq!(output.pixels(), &[11, 23, 37, expected]);
    }
    let mut mask = vec![0.0; PERSON_MASK_LEN];
    mask[127 * 256 + 127] = 0.25;
    mask[127 * 256 + 128] = 0.5;
    mask[128 * 256 + 127] = 0.75;
    mask[128 * 256 + 128] = 1.0;
    let output = apply_person_matte(
        &source,
        SourceBlend::Encoded,
        &mask,
        0.5,
        0.5,
        apply_limits(&source),
    )
    .unwrap();
    // Bilinear probability=.625 maps to .75. Mapping each corner first would
    // instead average [0,.5,1,1] to .625 and produce alpha159.
    assert_eq!(output.pixels(), &[11, 23, 37, 191]);
}

#[test]
fn invalid_mask_and_parameters_reject_without_touching_borrowed_inputs() {
    let input = RgbaFrame::solid(1, 1, [23, 45, 67, 89]).unwrap();
    let before = input.clone();
    let mask = vec![0.5; PERSON_MASK_LEN];
    for (threshold, softness) in [
        (f32::NAN, 1.0),
        (f32::INFINITY, 1.0),
        (-0.001, 1.0),
        (1.001, 1.0),
        (0.5, f32::NAN),
        (0.5, f32::INFINITY),
        (0.5, 0.0),
        (0.5, 0.000999),
        (0.5, 1.001),
    ] {
        assert_eq!(
            apply_person_matte(
                &input,
                SourceBlend::Encoded,
                &mask,
                threshold,
                softness,
                apply_limits(&input),
            ),
            Err(MediaError::InvalidBuffer)
        );
    }
    for length in [0, PERSON_MASK_LEN - 1, PERSON_MASK_LEN + 1] {
        assert_eq!(
            apply_person_matte(
                &input,
                SourceBlend::Encoded,
                &vec![0.5; length],
                0.5,
                1.0,
                apply_limits(&input),
            ),
            Err(MediaError::InvalidBuffer)
        );
    }
    for invalid in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -0.000001,
        1.000001,
    ] {
        let mut bad = mask.clone();
        bad[PERSON_MASK_LEN - 1] = invalid;
        let original = bad.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        assert_eq!(
            apply_person_matte(
                &input,
                SourceBlend::Encoded,
                &bad,
                0.5,
                1.0,
                apply_limits(&input),
            ),
            Err(MediaError::InvalidBuffer)
        );
        assert_eq!(
            bad.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            original
        );
    }
    assert_eq!(input, before);
}

#[test]
fn exact_admission_boundaries_include_mask_input_and_all_output_work() {
    let input = RgbaFrame::solid(2, 3, [1, 2, 3, 4]).unwrap();
    let prepare = prepare_limits(&input);
    assert!(prepare_person_rgb(&input, SourceBlend::Encoded, prepare).is_ok());
    for limits in [
        PersonMatteLimits {
            max_input_bytes: prepare.max_input_bytes - 1,
            ..prepare
        },
        PersonMatteLimits {
            max_output_bytes: prepare.max_output_bytes - 1,
            ..prepare
        },
        PersonMatteLimits {
            max_pixel_visits: prepare.max_pixel_visits - 1,
            ..prepare
        },
    ] {
        assert_eq!(
            prepare_person_rgb(&input, SourceBlend::Encoded, limits),
            Err(MediaError::BudgetExceeded)
        );
    }
    let mask = vec![0.5; PERSON_MASK_LEN];
    let apply = apply_limits(&input);
    assert!(apply_person_matte(&input, SourceBlend::Encoded, &mask, 0.5, 1.0, apply).is_ok());
    for limits in [
        PersonMatteLimits {
            max_input_bytes: apply.max_input_bytes - 1,
            ..apply
        },
        PersonMatteLimits {
            max_output_bytes: apply.max_output_bytes - 1,
            ..apply
        },
        PersonMatteLimits {
            max_pixel_visits: apply.max_pixel_visits - 1,
            ..apply
        },
    ] {
        assert_eq!(
            apply_person_matte(&input, SourceBlend::Encoded, &mask, 0.5, 1.0, limits),
            Err(MediaError::BudgetExceeded)
        );
    }
    assert_eq!(input.pixels(), &[1, 2, 3, 4].repeat(6));
}
