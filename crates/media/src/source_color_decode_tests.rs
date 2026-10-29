use super::*;

// Frozen original f64 scalar expression. Do not route this oracle through
// either production table, and do not replace it with an f32 approximation.
fn scalar_decode(value: u8) -> f64 {
    let value = f64::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[test]
fn source_f64_decode_table_matches_every_original_scalar_bit() {
    for value in 0..=255 {
        assert_eq!(
            decode(value).to_bits(),
            scalar_decode(value).to_bits(),
            "byte{value}"
        );
    }
}

#[test]
fn transparent_pm_over_straight_destination_keeps_original_hidden_rgb_policy() {
    for source in [[0, 0, 0, 0], [255, 128, 19, 0]] {
        let mut zero_alpha = [19, 47, 83, 0];
        blend(
            &mut zero_alpha,
            &source,
            1.0,
            SourceBlend::PremultipliedLinearSrgb,
        );
        assert_eq!(
            zero_alpha, [0; 4],
            "straight alpha0 output cannot retain emission"
        );
        let mut zero_opacity = [19, 47, 83, 0];
        blend(
            &mut zero_opacity,
            &source,
            0.0,
            SourceBlend::PremultipliedLinearSrgb,
        );
        assert_eq!(
            zero_opacity,
            [19, 47, 83, 0],
            "opacity0 remains a complete no-op"
        );
    }
    for mode in [
        SourceBlend::LinearSrgb,
        SourceBlend::EncodedPremultipliedSrgb,
    ] {
        let mut destination = [19, 47, 83, 0];
        blend(&mut destination, &[255, 128, 19, 0], 1.0, mode);
        assert_eq!(
            destination,
            [19, 47, 83, 0],
            "straight source alpha0 keeps the existing early return"
        );
    }
}

#[test]
fn zero_alpha_pm_emission_is_scaled_and_retained_in_supported_destinations() {
    let source = [255, 128, 0, 0];
    let mut premultiplied = [0; 4];
    blend_premultiplied(
        &mut premultiplied,
        &source,
        0.5,
        SourceBlend::PremultipliedLinearSrgb,
    );
    assert_eq!(
        premultiplied,
        [188, 92, 0, 0],
        "PM alpha0 emission is not transparent black"
    );
    let mut opaque_straight = [0, 0, 0, 255];
    blend(
        &mut opaque_straight,
        &source,
        0.5,
        SourceBlend::PremultipliedLinearSrgb,
    );
    assert_eq!(opaque_straight, [188, 92, 0, 255]);
    let mut existing_emission = [19, 47, 83, 0];
    blend_premultiplied(
        &mut existing_emission,
        &[0; 4],
        1.0,
        SourceBlend::PremultipliedLinearSrgb,
    );
    assert_eq!(existing_emission, [19, 47, 83, 0]);
}

#[test]
fn every_destination_byte_and_alpha_roundtrips_under_zero_pm_source() {
    // This does not add a shortcut. It guards the original byte behavior for
    // every destination channel/alpha pair, including hidden PM emission and
    // straight alpha0 canonicalization, if a later optimization is considered.
    for alpha in 0..=255 {
        for value in 0..=255 {
            let original = [value, 255 - value, value / 2, alpha];
            for opacity in [0.0, f32::from_bits(1), 0.5, 1.0] {
                let mut straight = original;
                blend(
                    &mut straight,
                    &[0; 4],
                    opacity,
                    SourceBlend::PremultipliedLinearSrgb,
                );
                let expected = if alpha == 0 && opacity > 0.0 {
                    [0; 4]
                } else {
                    original
                };
                assert_eq!(
                    straight, expected,
                    "straight byte{value}/alpha{alpha}/opacity{opacity}"
                );
                let mut premultiplied = original;
                blend_premultiplied(
                    &mut premultiplied,
                    &[0; 4],
                    opacity,
                    SourceBlend::PremultipliedLinearSrgb,
                );
                assert_eq!(
                    premultiplied, original,
                    "PM byte{value}/alpha{alpha}/opacity{opacity}"
                );
            }
        }
    }
}
