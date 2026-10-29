//! Exact common-path proofs; old renderer modules remain independent and frozen.
use super::*;

#[test]
fn clamped_mask_endpoints_and_every_other_sample_keep_original_pow_bits() {
    let check = |value: f32| {
        assert_eq!(
            shape_mask(value).to_bits(),
            value.powf(1.5).to_bits(),
            "shaper {:08x}",
            value.to_bits(),
        );
    };
    for value in [
        -0.0,
        0.0,
        f32::from_bits(1),
        f32::MIN_POSITIVE,
        0.5,
        f32::from_bits(1.0f32.to_bits() - 1),
        1.0,
    ] {
        check(value);
    }
    let mut state = 0x8f32_17cbu32;
    for _ in 0..200_000 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        check(f32::from_bits(state % (1.0f32.to_bits() + 1)));
    }
}

fn original_identity_finish(value: f32) -> u8 {
    // Original finish(), including its original powf even for gamma1. Frozen
    // encode retains the target-local libm expression and UNORM conversion.
    let gamma = 1.0;
    let contrast = 1.0;
    let brightness = 0.0;
    let alpha = 1.0;
    baseline_color::encode((value.powf(gamma) * contrast + brightness) * alpha)
}

#[test]
fn every_possible_opaque_cancellation_output_roundtrips_to_the_sampled_byte() {
    let decoded: [f32; 256] = std::array::from_fn(|byte| baseline_color::decode(byte as u8));
    assert!(
        decoded
            .iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
    );
    // Positive weights and decoded values in[0,1] make this a monotone upper
    // bound for every original gray value. No NaN/overflow/subnormal terms arise.
    let gray_max = 1.0f32 * 0.2126 + 1.0 * 0.7152 + 1.0 * 0.0722;
    assert!(gray_max <= 2.0);
    // Let t=round(c-gray), result=round(gray+t). With c in[0,1] and gray in[0,2],
    // each rounding contributes at most epsilon absolute error. Four epsilon is
    // deliberately wider than the combined bound. We enumerate EVERY float in
    // that envelope, with outward neighboring endpoints: no output tolerance.
    let envelope = f32::EPSILON * 4.0;
    let mut checked = 0u32;
    for byte in 1..=255u8 {
        let center = decoded[usize::from(byte)];
        assert!(center > envelope);
        let first = (center - envelope).to_bits() - 1;
        let last = (center + envelope).to_bits() + 1;
        checked = checked.checked_add(last - first + 1).unwrap();
        // Keep the proof bounded even if an unrelated decode change is wrong.
        assert!(checked <= 262_144);
        for bits in first..=last {
            assert_eq!(
                original_identity_finish(f32::from_bits(bits)),
                byte,
                "opaque byte={byte} possible_original_bits={bits:08x}"
            );
        }
    }
    // c=0 has exact t=-gray followed by gray+(-gray), so its original output is
    // exactly zero. In contrast, a generic near-zero input must not be skipped.
    for gray in [0.0f32, 0.001, 0.2126, 0.7152, 1.0, 2.0] {
        let original_mixed = gray + (0.0 - gray) * 1.0;
        assert_eq!(original_identity_finish(original_mixed), 0);
    }
}

#[test]
fn one_borrowed_decode_table_is_identical_to_the_frozen_scalar_and_wrapper() {
    let table = crate::color_correction::decode_table();
    assert!(std::ptr::eq(table, crate::color_correction::decode_table()));
    for byte in 0..=255u8 {
        assert_eq!(
            table[usize::from(byte)].to_bits(),
            baseline_color::decode(byte).to_bits()
        );
        assert_eq!(
            table[usize::from(byte)].to_bits(),
            crate::color_correction::decode(byte).to_bits()
        );
    }
}

#[test]
fn opaque_shortcut_requires_every_prepared_identity_curve_field() {
    assert!(PreparedKeyer::chroma(ChromaKeySettings::default()).identity_curve());
    let variants = [
        ChromaKeySettings {
            gamma: 0.01,
            ..Default::default()
        },
        ChromaKeySettings {
            contrast: 0.01,
            ..Default::default()
        },
        ChromaKeySettings {
            brightness: f32::MIN_POSITIVE,
            ..Default::default()
        },
        ChromaKeySettings {
            opacity: f32::from_bits(1.0f32.to_bits() - 1),
            ..Default::default()
        },
    ];
    for settings in variants {
        assert!(
            !PreparedKeyer::chroma(settings).identity_curve(),
            "{settings:?}"
        );
    }
}

#[test]
fn opaque_foreground_and_every_nonidentity_guard_match_frozen_spatial_bytes() {
    let (width, height) = (256usize, 17usize);
    let mut input = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            input.extend_from_slice(&[
                x as u8,
                (y * 15) as u8,
                (x.wrapping_mul(37) ^ y.wrapping_mul(73)) as u8,
                255,
            ]);
        }
    }
    let settings = [
        ChromaKeySettings::default(),
        ChromaKeySettings {
            similarity: 1,
            smoothness: 1,
            spill: 1,
            ..Default::default()
        },
        ChromaKeySettings {
            similarity: 1,
            smoothness: 1000,
            spill: 1,
            ..Default::default()
        },
        ChromaKeySettings {
            similarity: 1,
            smoothness: 1,
            spill: 1000,
            ..Default::default()
        },
        ChromaKeySettings {
            gamma: 0.01,
            ..Default::default()
        },
        ChromaKeySettings {
            contrast: 0.01,
            ..Default::default()
        },
        ChromaKeySettings {
            brightness: f32::MIN_POSITIVE,
            ..Default::default()
        },
        ChromaKeySettings {
            opacity: f32::from_bits(1.0f32.to_bits() - 1),
            ..Default::default()
        },
    ];
    for encoding in ENCODINGS {
        for settings in settings {
            let mut expected = input.clone();
            let mut actual = input.clone();
            baseline_keyer::chroma_in_place(
                &mut expected,
                width,
                height,
                encoding,
                baseline_keyer::PreparedKeyer::chroma(settings),
                &mut vec![[0.0; 4]; width * 3],
            );
            chroma_in_place(
                &mut actual,
                width,
                height,
                encoding,
                PreparedKeyer::chroma(settings),
                &mut vec![0u32; width * 12],
            )
            .unwrap();
            assert_eq!(actual, expected, "common path {encoding:?} {settings:?}");
        }
    }
}
