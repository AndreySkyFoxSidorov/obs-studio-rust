//! Independent bit-pattern oracles for OBS's double-before-float gamma rule.
use super::*;

#[test]
fn modern_authored_double_and_legacy_promoted_float_have_distinct_literal_exponents() {
    for (authored, modern, legacy) in [
        (-1.93_f64, 0x403b_851f, 0x403b_851e),
        (-0.84, 0x3feb_851f, 0x3feb_851e),
        (0.49, 0x3f2b_cfe5, 0x3f2b_cfe4),
        (0.8, 0x3f0e_38e4, 0x3f0e_38e3),
    ] {
        assert_eq!(
            Prepared::new(ColorCorrectionSettings {
                gamma: authored,
                ..Default::default()
            })
            .gamma
            .to_bits(),
            modern
        );
        assert_eq!(
            Prepared::new(ColorCorrectionSettings {
                gamma: f64::from(authored as f32),
                ..Default::default()
            })
            .gamma
            .to_bits(),
            legacy
        );
    }
}

#[test]
fn exact_legacy_promotion_preserves_both_signs_every_valid_exponent_family_and_mantissa_boundaries()
{
    // Release3 source SHA6da83204928b0300dec9c88de17649772ae4139840756f4043ae4833435713f2
    // used f64::from(g32), then this double expression, then one f32 cast.
    // Every finite f32 has an exact f64 representation; replacing its stored
    // type by that promoted value therefore preserves every arithmetic operand,
    // sign predicate and rounding point, not just these sampled boundaries.
    for sign in [0, 0x8000_0000] {
        for exponent in 0..=128 {
            for mantissa in [
                0,
                1,
                2,
                3,
                0x0000_ffff,
                0x0001_0000,
                0x003f_fffe,
                0x003f_ffff,
                0x0040_0000,
                0x0040_0001,
                0x007f_fffd,
                0x007f_fffe,
                0x007f_ffff,
            ] {
                let old = f32::from_bits(sign | (exponent << 23) | mantissa);
                if !(-3.0..=3.0).contains(&old) {
                    continue;
                }
                let promoted = f64::from(old);
                let reference = if promoted < 0.0 {
                    1.0 - promoted
                } else {
                    1.0 / (1.0 + promoted)
                } as f32;
                let actual = Prepared::new(ColorCorrectionSettings {
                    gamma: promoted,
                    ..Default::default()
                })
                .gamma;
                assert_eq!(
                    actual.to_bits(),
                    reference.to_bits(),
                    "gamma bits {:08x}",
                    old.to_bits()
                );
            }
        }
    }
}
