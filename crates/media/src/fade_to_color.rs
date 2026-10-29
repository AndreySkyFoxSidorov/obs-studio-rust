//! OBS Fade to Color's GS_CS_SRGB path. Encoded premultiplied shader output
//! blends over black with framebuffer sRGB disabled; this API returns opaque
//! output directly. It must not undergo another linear-light alpha flatten.
use crate::{CompositeLimits, MediaError, RgbaFrame, directional::progress};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FadeToColorSettings {
    rgb: [u8; 3],
    switch_point: u8,
}
impl FadeToColorSettings {
    pub fn new(rgb: [u8; 3], switch_point: u8) -> Result<Self, MediaError> {
        let settings = Self { rgb, switch_point };
        settings.validate()?;
        Ok(settings)
    }
    pub fn rgb(self) -> [u8; 3] {
        self.rgb
    }
    pub fn switch_point(self) -> u8 {
        self.switch_point
    }
    fn validate(self) -> Result<(), MediaError> {
        if self.switch_point > 100 {
            Err(MediaError::InvalidTransition)
        } else {
            Ok(())
        }
    }
    fn point(self) -> f32 {
        f32::from(self.switch_point) / 100.0
    }
}

fn smoothstep(min: f32, max: f32, t: f32) -> f32 {
    // Upstream divides0/0 at a collapsed interval's endpoint. Define that
    // otherwise undefined case as a right-continuous step; all ordinary source
    // intervals retain the exact original operation order and f32 arithmetic.
    if min == max {
        return if t < min { 0.0 } else { 1.0 };
    }
    let x = ((t - min) / (max - min)).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}
fn video_mix(t: f32, settings: FadeToColorSettings) -> (bool, f32) {
    let sp = settings.point();
    if t < sp {
        (false, smoothstep(0.0, sp, t))
    } else {
        (true, 1.0 - smoothstep(sp, 1.0, t))
    }
}

/// Numeric factors from the OBS audio callback, not a sum-to-one crossmix.
/// A zero-width smoothstep interval is defined as a right-continuous step:
/// switch0/time0 returns(0,0), switch100/time1 returns(0,1). These finite values
/// replace upstream's undefined0/0 endpoint only; no audio device is involved.
pub fn fade_to_color_audio_gains(
    elapsed_ns: u64,
    duration_ns: u64,
    settings: FadeToColorSettings,
) -> Result<(f32, f32), MediaError> {
    settings.validate()?;
    let t = progress(elapsed_ns, duration_ns)?;
    let sp = settings.point();
    let sa = smoothstep(0.0, sp, t);
    let sb = smoothstep(sp, 1.0, t);
    Ok(((1.0 - sa) * (1.0 - t) + sa * 0.0, (1.0 - sb) * 0.0 + sb * t))
}

/// Fade the selected source toward an opaque color with source-order f32
/// smoothstep. Before the switch only A is sampled; at/after it only B is.
/// Straight-alpha input adapts to rounded encoded premultiplied texture bytes;
/// the encoded shader output is already black-backed with opaque output alpha.
/// Admits both inputs and three conservative visits/pixel even at endpoints.
pub fn fade_to_color_rgba(
    a: &RgbaFrame,
    b: &RgbaFrame,
    elapsed_ns: u64,
    duration_ns: u64,
    settings: FadeToColorSettings,
    limits: CompositeLimits,
) -> Result<RgbaFrame, MediaError> {
    settings.validate()?;
    let t = progress(elapsed_ns, duration_ns)?;
    if a.width() != b.width() || a.height() != b.height() {
        return Err(MediaError::InvalidDimensions);
    }
    let input_bytes = if a.pixels().as_ptr() == b.pixels().as_ptr() {
        a.byte_len()
    } else {
        a.byte_len()
            .checked_add(b.byte_len())
            .ok_or(MediaError::Overflow)?
    };
    let visits = u64::from(a.width())
        .checked_mul(u64::from(a.height()))
        .and_then(|n| n.checked_mul(3))
        .ok_or(MediaError::Overflow)?;
    if limits.max_layers < 2
        || input_bytes > limits.max_input_bytes
        || a.byte_len() > limits.max_output_bytes
        || visits > limits.max_pixel_visits
    {
        return Err(MediaError::BudgetExceeded);
    }
    let (incoming, weight) = video_mix(t, settings);
    let source = if incoming { b } else { a };
    if weight == 1.0 {
        return RgbaFrame::solid(
            a.width(),
            a.height(),
            [settings.rgb[0], settings.rgb[1], settings.rgb[2], 255],
        );
    }
    let mut pixels = vec![0; a.byte_len()];
    for (source, out) in source
        .pixels()
        .chunks_exact(4)
        .zip(pixels.chunks_exact_mut(4))
    {
        for channel in 0..3 {
            let premultiplied =
                ((u32::from(source[channel]) * u32::from(source[3]) + 127) / 255) as u8;
            out[channel] = if weight == 0.0 {
                premultiplied
            } else {
                let src = f32::from(premultiplied) / 255.0;
                let color = f32::from(settings.rgb[channel]) / 255.0;
                let mixed = src + (color - src) * weight;
                (mixed.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
            };
        }
        out[3] = 255;
    }
    RgbaFrame::new(a.width(), a.height(), pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings(rgb: [u8; 3], switch: u8) -> FadeToColorSettings {
        FadeToColorSettings::new(rgb, switch).unwrap()
    }
    fn run(a: &RgbaFrame, b: &RgbaFrame, n: u64, d: u64, s: FadeToColorSettings) -> RgbaFrame {
        fade_to_color_rgba(a, b, n, d, s, CompositeLimits::default()).unwrap()
    }
    #[test]
    fn patterned_sources_select_one_side_and_pass_opaque_color_without_ab_crossmix() {
        let a =
            RgbaFrame::new(3, 1, vec![255, 0, 0, 255, 0, 255, 0, 255, 23, 45, 67, 255]).unwrap();
        let b = RgbaFrame::new(
            3,
            1,
            vec![0, 0, 255, 255, 255, 255, 0, 255, 91, 109, 127, 255],
        )
        .unwrap();
        let s = settings([255; 3], 50);
        assert_eq!(run(&a, &b, 0, 4, s), a);
        assert_eq!(
            run(&a, &b, 1, 4, s).pixels(),
            &[255, 128, 128, 255, 128, 255, 128, 255, 139, 150, 161, 255]
        );
        assert_eq!(run(&a, &b, 2, 4, s).pixels(), &[255; 12]);
        assert_eq!(
            run(&a, &b, 3, 4, s).pixels(),
            &[128, 128, 255, 255, 255, 255, 128, 255, 173, 182, 191, 255]
        );
        assert_eq!(run(&a, &b, 4, 4, s), b);
        let replacement = RgbaFrame::solid(3, 1, [17, 29, 43, 255]).unwrap();
        assert_eq!(run(&a, &b, 1, 4, s), run(&a, &replacement, 1, 4, s));
        assert_eq!(run(&a, &b, 3, 4, s), run(&replacement, &b, 3, 4, s));
        let red = RgbaFrame::solid(1, 1, [255, 0, 0, 255]).unwrap();
        let blue = RgbaFrame::solid(1, 1, [0, 0, 255, 255]).unwrap();
        assert_eq!(run(&red, &blue, 1, 8, s).pixels(), &[255, 40, 40, 255]);
        assert_eq!(run(&red, &blue, 3, 8, s).pixels(), &[255, 215, 215, 255]);
    }
    #[test]
    fn switch_zero_starts_on_color_and_switch_hundred_hands_off_to_b_at_the_end() {
        let a = RgbaFrame::solid(1, 1, [255, 0, 0, 255]).unwrap();
        let b = RgbaFrame::solid(1, 1, [0, 0, 255, 255]).unwrap();
        let zero = settings([255; 3], 0);
        assert_eq!(run(&a, &b, 0, 4, zero).pixels(), &[255; 4]);
        assert_eq!(run(&a, &b, 1, 4, zero).pixels(), &[215, 215, 255, 255]);
        assert_eq!(run(&a, &b, 4, 4, zero), b);
        let hundred = settings([255; 3], 100);
        assert_eq!(run(&a, &b, 0, 4, hundred), a);
        assert_eq!(run(&a, &b, 3, 4, hundred).pixels(), &[255, 215, 215, 255]);
        assert_eq!(run(&a, &b, 4, 4, hundred), b);
        let nondefault = settings([17, 89, 203], 25);
        assert_eq!(run(&a, &b, 1, 4, nondefault).pixels(), &[17, 89, 203, 255]);
        assert_eq!(
            run(&a, &b, u64::MAX - 1, u64::MAX, hundred),
            b,
            "rounded f32 endpoint remains finite"
        );
    }
    #[test]
    fn null_and_partial_alpha_flatten_in_encoded_space_without_an_extra_linear_gain() {
        let null = RgbaFrame::solid(1, 1, [0; 4]).unwrap();
        let white = RgbaFrame::solid(1, 1, [255; 4]).unwrap();
        let s = settings([255; 3], 50);
        assert_eq!(run(&null, &white, 1, 4, s).pixels(), &[128, 128, 128, 255]);
        assert_eq!(run(&white, &null, 3, 4, s).pixels(), &[128, 128, 128, 255]);
        assert_eq!(run(&null, &null, 0, 4, s).pixels(), &[0, 0, 0, 255]);
        assert_eq!(run(&null, &null, 4, 4, s).pixels(), &[0, 0, 0, 255]);
        let alpha = RgbaFrame::new(
            3,
            1,
            vec![255, 255, 255, 128, 255, 19, 97, 0, 1, 37, 249, 255],
        )
        .unwrap();
        let empty = RgbaFrame::solid(3, 1, [0; 4]).unwrap();
        assert_eq!(
            run(&alpha, &empty, 0, 4, s).pixels(),
            &[128, 128, 128, 255, 0, 0, 0, 255, 1, 37, 249, 255]
        );
        assert_eq!(
            run(&alpha, &empty, 1, 4, settings([0; 3], 50)).pixels(),
            &[64, 64, 64, 255, 0, 0, 0, 255, 1, 19, 125, 255]
        );
    }
    #[test]
    fn smoothstep_bits_and_finite_audio_policy_match_source_independent_factors() {
        for (t, bits) in [
            (0.0, 0),
            (0.25, 0x3e20_0000),
            (0.5, 0x3f00_0000),
            (0.75, 0x3f58_0000),
            (1.0, 0x3f80_0000),
        ] {
            assert_eq!(smoothstep(0.0, 1.0, t).to_bits(), bits);
        }
        let s = settings([0; 3], 50);
        for (n, d, expected) in [
            (0, 4, (1.0, 0.0)),
            (1, 4, (0.375, 0.0)),
            (2, 4, (0.0, 0.0)),
            (3, 4, (0.0, 0.375)),
            (4, 4, (0.0, 1.0)),
        ] {
            assert_eq!(fade_to_color_audio_gains(n, d, s).unwrap(), expected);
        }
        assert_eq!(
            fade_to_color_audio_gains(0, 1, settings([0; 3], 0)).unwrap(),
            (0.0, 0.0)
        );
        assert_eq!(
            fade_to_color_audio_gains(1, 1, settings([0; 3], 100)).unwrap(),
            (0.0, 1.0)
        );
        for switch in [0, 1, 25, 50, 99, 100] {
            for time in 0..=100 {
                let (a, b) =
                    fade_to_color_audio_gains(time, 100, settings([0; 3], switch)).unwrap();
                assert!(
                    a.is_finite()
                        && b.is_finite()
                        && (0.0..=1.0).contains(&a)
                        && (0.0..=1.0).contains(&b)
                );
            }
        }
    }
    #[test]
    fn settings_dimensions_time_and_all_endpoint_budgets_are_admitted_before_output() {
        assert_eq!(
            FadeToColorSettings::new([1, 2, 3], 101),
            Err(MediaError::InvalidTransition)
        );
        assert_eq!(
            FadeToColorSettings::new([1, 2, 3], 255),
            Err(MediaError::InvalidTransition)
        );
        let s = settings([1, 2, 3], 50);
        assert_eq!(s.rgb(), [1, 2, 3]);
        assert_eq!(s.switch_point(), 50);
        let a = RgbaFrame::solid(1, 1, [255; 4]).unwrap();
        let b = RgbaFrame::solid(1, 1, [0, 0, 0, 255]).unwrap();
        let exact = CompositeLimits {
            max_layers: 2,
            max_input_bytes: 8,
            max_output_bytes: 4,
            max_pixel_visits: 3,
        };
        for time in [0, 1, 2] {
            assert!(fade_to_color_rgba(&a, &b, time, 2, s, exact).is_ok());
        }
        for limits in [
            CompositeLimits {
                max_layers: 1,
                ..exact
            },
            CompositeLimits {
                max_input_bytes: 7,
                ..exact
            },
            CompositeLimits {
                max_output_bytes: 3,
                ..exact
            },
            CompositeLimits {
                max_pixel_visits: 2,
                ..exact
            },
        ] {
            for time in [0, 1, 2] {
                assert_eq!(
                    fade_to_color_rgba(&a, &b, time, 2, s, limits),
                    Err(MediaError::BudgetExceeded)
                );
            }
        }
        assert!(
            fade_to_color_rgba(
                &a,
                &a,
                1,
                2,
                s,
                CompositeLimits {
                    max_input_bytes: 4,
                    ..exact
                }
            )
            .is_ok()
        );
        let other = RgbaFrame::solid(2, 1, [0; 4]).unwrap();
        assert_eq!(
            fade_to_color_rgba(&a, &other, 0, 2, s, exact),
            Err(MediaError::InvalidDimensions)
        );
        for (n, d) in [(0, 0), (1, 0), (2, 1), (u64::MAX, u64::MAX - 1)] {
            assert_eq!(
                fade_to_color_rgba(&a, &b, n, d, s, exact),
                Err(MediaError::InvalidTransition)
            );
            assert_eq!(
                fade_to_color_audio_gains(n, d, s),
                Err(MediaError::InvalidTransition)
            );
        }
    }
}
