use super::*;

#[path = "person_matte_scalar_reference.rs"]
mod scalar;

const BLENDS: [SourceBlend; 4] = [
    SourceBlend::Encoded,
    SourceBlend::LinearSrgb,
    SourceBlend::EncodedPremultipliedSrgb,
    SourceBlend::PremultipliedLinearSrgb,
];

fn fixture(width: u32, height: u32) -> RgbaFrame {
    let mut bytes = Vec::with_capacity(width as usize * height as usize * 4);
    for i in 0..width as usize * height as usize {
        bytes.extend_from_slice(&[
            i as u8,
            (i >> 3) as u8,
            i.wrapping_mul(37) as u8,
            i.wrapping_mul(13) as u8,
        ]);
    }
    bytes[..4].copy_from_slice(&[255, 128, 19, 0]);
    RgbaFrame::new(width, height, bytes).unwrap()
}

fn limits(input: &RgbaFrame) -> PersonMatteLimits {
    PersonMatteLimits {
        max_input_bytes: input.byte_len() + PERSON_MASK_LEN * 4,
        max_output_bytes: input.byte_len().max(PERSON_RGB_LEN * 4),
        max_pixel_visits: (4 * PERSON_MASK_LEN as u64)
            .max(5 * (input.byte_len() as u64 / 4) + PERSON_MASK_LEN as u64),
    }
}

fn old_limits(value: PersonMatteLimits) -> scalar::PersonMatteLimits {
    scalar::PersonMatteLimits {
        max_input_bytes: value.max_input_bytes,
        max_output_bytes: value.max_output_bytes,
        max_pixel_visits: value.max_pixel_visits,
    }
}

fn mask() -> Vec<f32> {
    let mut state = 0x9a71_2bc3_u32;
    (0..PERSON_MASK_LEN)
        .map(|index| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            match index % 11 {
                0 => 0.0,
                1 => 1.0,
                2 => 0.5,
                3 => f32::from_bits(0.5_f32.to_bits() + 1),
                4 => f32::from_bits(1),
                5 => -0.0,
                _ => (state & 0xffff) as f32 / 65535.0,
            }
        })
        .collect()
}

fn compare_apply(
    input: &RgbaFrame,
    blend: SourceBlend,
    mask: &[f32],
    threshold: f32,
    softness: f32,
) {
    let budget = limits(input);
    let expected =
        scalar::apply_person_matte(input, blend, mask, threshold, softness, old_limits(budget))
            .unwrap();
    let actual = apply_person_matte(input, blend, mask, threshold, softness, budget).unwrap();
    assert_eq!(
        actual,
        expected,
        "{}x{} {blend:?} threshold{threshold} softness{softness}",
        input.width(),
        input.height()
    );
}

#[test]
fn optimized_tensor_matches_every_frozen_scalar_f32_bit_at_thin_odd_and_fullhd_sizes() {
    for (width, height) in [
        (1, 1),
        (1, 257),
        (257, 1),
        (255, 257),
        (257, 259),
        (1920, 1080),
    ] {
        let input = fixture(width, height);
        for blend in BLENDS {
            let budget = limits(&input);
            let expected = scalar::prepare_person_rgb(&input, blend, old_limits(budget)).unwrap();
            let actual = prepare_person_rgb(&input, blend, budget).unwrap();
            assert_eq!(actual.len(), expected.len());
            for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "{width}x{height} {blend:?} tensor{index}"
                );
            }
        }
    }
}

#[test]
fn optimized_mask_matches_frozen_scalar_bytes_across_tiles_and_2d_borders() {
    let mask = mask();
    for (width, height) in [
        (1, 1),
        (1, 257),
        (255, 2),
        (256, 3),
        (257, 2),
        (513, 3),
        (8192, 1),
        (257, 259),
    ] {
        let input = fixture(width, height);
        for blend in BLENDS {
            for (threshold, softness) in [(0.5, 1.0), (0.5, 0.001), (0.17, 0.83)] {
                compare_apply(&input, blend, &mask, threshold, softness);
            }
        }
    }
    // Real output dimensions exercise multiple complete tiles and the final
    // partial tile. The oracle retains old per-pixel division/floor/transfer.
    for (width, height) in [(640, 480), (1920, 1080)] {
        let input = fixture(width, height);
        for blend in [
            SourceBlend::LinearSrgb,
            SourceBlend::PremultipliedLinearSrgb,
        ] {
            compare_apply(&input, blend, &mask, 0.5, 1.0);
        }
    }
}

#[test]
fn endpoint_shortcuts_match_all_channel_alpha_pairs_and_fractional_pm_emission() {
    let mut bytes = Vec::with_capacity(256 * 256 * 4);
    for alpha in 0..=255 {
        for channel in 0..=255 {
            bytes.extend_from_slice(&[channel, 255 - channel, channel / 2, alpha]);
        }
    }
    let input = RgbaFrame::new(256, 256, bytes).unwrap();
    for blend in BLENDS {
        for value in [0.0, 1.0] {
            compare_apply(&input, blend, &vec![value; PERSON_MASK_LEN], 0.5, 1.0);
        }
    }
    let emission = RgbaFrame::solid(257, 3, [255, 128, 19, 0]).unwrap();
    for value in [0.0, 0.5, 1.0] {
        compare_apply(
            &emission,
            SourceBlend::PremultipliedLinearSrgb,
            &vec![value; PERSON_MASK_LEN],
            0.5,
            1.0,
        );
    }
}

#[test]
fn optimized_helper_preserves_rejection_order_and_exact_admission_boundaries() {
    let input = fixture(257, 3);
    let mask = mask();
    let exact = PersonMatteLimits {
        max_input_bytes: input.byte_len() + PERSON_MASK_LEN * 4,
        max_output_bytes: input.byte_len(),
        max_pixel_visits: PERSON_MASK_LEN as u64 + input.byte_len() as u64 / 4 * 5,
    };
    for budget in [
        exact,
        PersonMatteLimits {
            max_input_bytes: exact.max_input_bytes - 1,
            ..exact
        },
        PersonMatteLimits {
            max_output_bytes: exact.max_output_bytes - 1,
            ..exact
        },
        PersonMatteLimits {
            max_pixel_visits: exact.max_pixel_visits - 1,
            ..exact
        },
    ] {
        for (threshold, softness) in [(0.5, 1.0), (f32::NAN, 1.0), (0.5, 0.0)] {
            let actual = apply_person_matte(
                &input,
                SourceBlend::PremultipliedLinearSrgb,
                &mask,
                threshold,
                softness,
                budget,
            );
            let expected = scalar::apply_person_matte(
                &input,
                SourceBlend::PremultipliedLinearSrgb,
                &mask,
                threshold,
                softness,
                old_limits(budget),
            );
            assert_eq!(actual, expected);
        }
    }
    let prep = PersonMatteLimits {
        max_input_bytes: input.byte_len(),
        max_output_bytes: PERSON_RGB_LEN * 4,
        max_pixel_visits: PERSON_MASK_LEN as u64 * 4,
    };
    for budget in [
        prep,
        PersonMatteLimits {
            max_input_bytes: prep.max_input_bytes - 1,
            ..prep
        },
        PersonMatteLimits {
            max_output_bytes: prep.max_output_bytes - 1,
            ..prep
        },
        PersonMatteLimits {
            max_pixel_visits: prep.max_pixel_visits - 1,
            ..prep
        },
    ] {
        let actual = prepare_person_rgb(&input, SourceBlend::LinearSrgb, budget);
        let expected =
            scalar::prepare_person_rgb(&input, SourceBlend::LinearSrgb, old_limits(budget));
        assert_eq!(actual, expected);
    }
    for invalid in [f32::NAN, f32::INFINITY, -0.000001, 1.000001] {
        let mut bad = mask.clone();
        bad[PERSON_MASK_LEN - 1] = invalid;
        assert_eq!(
            apply_person_matte(&input, SourceBlend::LinearSrgb, &bad, 0.5, 1.0, exact),
            scalar::apply_person_matte(
                &input,
                SourceBlend::LinearSrgb,
                &bad,
                0.5,
                1.0,
                old_limits(exact)
            )
        );
    }
    assert!(
        std::mem::size_of::<[Axis; 256]>() <= 6144,
        "axis scratch must stay fixed and bounded"
    );
}
