//! Bounded encoded-space SDR fade between two complete scene rasters.
//!
//! OBS transition-fade.c selects nonlinear encoded RGB interpolation for two
//! SDR scene textures. This pure CPU primitive applies that policy to opaque
//! frames, and uses premultiplied-alpha interpolation for general straight-alpha
//! input. The separate fade_single_on_black primitive covers a scene fading to
//! black; this function does not implement HDR, null textures or audio devices.
use crate::{CompositeLimits, MediaError, RgbaFrame};

fn weights(elapsed_ns: u64, duration_ns: u64) -> Result<(u128, u128, u128), MediaError> {
    if duration_ns == 0 || elapsed_ns > duration_ns {
        return Err(MediaError::InvalidTransition);
    }
    Ok((
        u128::from(duration_ns - elapsed_ns),
        u128::from(elapsed_ns),
        u128::from(duration_ns),
    ))
}

fn round_ratio(numerator: u128, denominator: u128) -> u8 {
    // Pixel numerators are at most 255*255*u64::MAX (<2^80), so adding
    // half a denominator cannot overflow u128 and the result is <=255.
    ((numerator + denominator / 2) / denominator) as u8
}

/// Blend complete equal-sized straight-alpha RGBA8 images in encoded RGB space.
/// Validates both inputs and all budgets even at an exact endpoint. Counts two
/// layers, unique source allocations, output bytes and three visits per pixel
/// (both source reads plus output fill). No floating-point progress or minimum
/// duration changes integer rounding. The caller owns scheduling and lifecycle.
pub fn crossfade_rgba(
    a: &RgbaFrame,
    b: &RgbaFrame,
    elapsed_ns: u64,
    duration_ns: u64,
    limits: CompositeLimits,
) -> Result<RgbaFrame, MediaError> {
    let (weight_a, weight_b, duration) = weights(elapsed_ns, duration_ns)?;
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
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or(MediaError::Overflow)?;
    if limits.max_layers < 2
        || input_bytes > limits.max_input_bytes
        || a.byte_len() > limits.max_output_bytes
        || visits > limits.max_pixel_visits
    {
        return Err(MediaError::BudgetExceeded);
    }
    // Preserve exact bytes at both endpoints, including hidden RGB in a fully
    // transparent source. All validation above still applies to the whole call.
    if elapsed_ns == 0 {
        return Ok(a.clone());
    }
    if elapsed_ns == duration_ns {
        return Ok(b.clone());
    }
    let mut output = vec![0; a.byte_len()];
    for ((a, b), output) in a
        .pixels()
        .chunks_exact(4)
        .zip(b.pixels().chunks_exact(4))
        .zip(output.chunks_exact_mut(4))
    {
        let alpha = u128::from(a[3]) * weight_a + u128::from(b[3]) * weight_b;
        output[3] = round_ratio(alpha, duration);
        if alpha == 0 || output[3] == 0 {
            continue;
        }
        for channel in 0..3 {
            let premultiplied = u128::from(a[channel]) * u128::from(a[3]) * weight_a
                + u128::from(b[channel]) * u128::from(b[3]) * weight_b;
            output[channel] = round_ratio(premultiplied, alpha);
        }
    }
    RgbaFrame::new(a.width(), a.height(), output)
}

/// OBS's numeric fade factors (1-t,t), without any audio/device operation.
pub fn fade_audio_gains(elapsed_ns: u64, duration_ns: u64) -> Result<(f32, f32), MediaError> {
    let (_, b, duration) = weights(elapsed_ns, duration_ns)?;
    let t = (b as f64 / duration as f64) as f32;
    Ok((1.0 - t, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_sdr_fade_has_exact_endpoints_midpoint_and_nearest_integer_rounding() {
        let red = RgbaFrame::solid(1, 1, [255, 0, 0, 255]).unwrap();
        let blue = RgbaFrame::solid(1, 1, [0, 0, 255, 255]).unwrap();
        for (elapsed, duration, expected) in [
            (0, 2, [255, 0, 0, 255]),
            (1, 2, [128, 0, 128, 255]),
            (2, 2, [0, 0, 255, 255]),
            (1, 3, [170, 0, 85, 255]),
            (2, 3, [85, 0, 170, 255]),
        ] {
            assert_eq!(
                crossfade_rgba(&red, &blue, elapsed, duration, CompositeLimits::default())
                    .unwrap()
                    .pixels(),
                &expected
            );
        }
        assert_eq!(
            crossfade_rgba(
                &red,
                &blue,
                u64::MAX / 2,
                u64::MAX,
                CompositeLimits::default()
            )
            .unwrap()
            .pixels(),
            &[128, 0, 127, 255]
        );
    }

    #[test]
    fn straight_alpha_uses_premultiplied_mix_without_transparent_color_fringe() {
        let invisible_blue = RgbaFrame::solid(1, 1, [0, 0, 255, 0]).unwrap();
        let opaque_red = RgbaFrame::solid(1, 1, [255, 0, 0, 255]).unwrap();
        assert_eq!(
            crossfade_rgba(
                &invisible_blue,
                &opaque_red,
                1,
                2,
                CompositeLimits::default()
            )
            .unwrap()
            .pixels(),
            &[255, 0, 0, 128]
        );
        assert_eq!(
            crossfade_rgba(
                &invisible_blue,
                &opaque_red,
                0,
                2,
                CompositeLimits::default()
            )
            .unwrap()
            .pixels(),
            invisible_blue.pixels()
        );
        let red = RgbaFrame::solid(1, 1, [255, 0, 0, 128]).unwrap();
        let blue = RgbaFrame::solid(1, 1, [0, 0, 255, 64]).unwrap();
        assert_eq!(
            crossfade_rgba(&red, &blue, 1, 2, CompositeLimits::default())
                .unwrap()
                .pixels(),
            &[170, 0, 85, 96]
        );
        let transparent_red = RgbaFrame::solid(1, 1, [255, 0, 0, 0]).unwrap();
        assert_eq!(
            crossfade_rgba(
                &transparent_red,
                &invisible_blue,
                1,
                2,
                CompositeLimits::default()
            )
            .unwrap()
            .pixels(),
            &[0, 0, 0, 0]
        );
        let barely_red = RgbaFrame::solid(1, 1, [255, 0, 0, 1]).unwrap();
        assert_eq!(
            crossfade_rgba(
                &barely_red,
                &invisible_blue,
                999,
                1000,
                CompositeLimits::default()
            )
            .unwrap()
            .pixels(),
            &[0, 0, 0, 0]
        );
    }

    #[test]
    fn every_pixel_is_blended_with_its_corresponding_input() {
        let a = RgbaFrame::new(2, 1, vec![0, 0, 0, 255, 200, 100, 50, 255]).unwrap();
        let b = RgbaFrame::new(2, 1, vec![100, 200, 50, 255, 0, 0, 0, 255]).unwrap();
        assert_eq!(
            crossfade_rgba(&a, &b, 1, 4, CompositeLimits::default())
                .unwrap()
                .pixels(),
            &[25, 50, 13, 255, 150, 75, 38, 255]
        );
    }

    #[test]
    fn invalid_time_dimensions_and_budgets_reject_even_at_endpoints() {
        let a = RgbaFrame::solid(1, 1, [0; 4]).unwrap();
        let b = a.clone();
        assert_eq!(
            crossfade_rgba(&a, &b, 0, 0, CompositeLimits::default()),
            Err(MediaError::InvalidTransition)
        );
        assert_eq!(
            crossfade_rgba(&a, &b, 2, 1, CompositeLimits::default()),
            Err(MediaError::InvalidTransition)
        );
        let wide = RgbaFrame::solid(2, 1, [0; 4]).unwrap();
        assert_eq!(
            crossfade_rgba(&a, &wide, 0, 1, CompositeLimits::default()),
            Err(MediaError::InvalidDimensions)
        );
        for limits in [
            CompositeLimits {
                max_layers: 1,
                ..CompositeLimits::default()
            },
            CompositeLimits {
                max_input_bytes: 7,
                ..CompositeLimits::default()
            },
            CompositeLimits {
                max_output_bytes: 3,
                ..CompositeLimits::default()
            },
            CompositeLimits {
                max_pixel_visits: 2,
                ..CompositeLimits::default()
            },
        ] {
            assert_eq!(
                crossfade_rgba(&a, &b, 0, 1, limits),
                Err(MediaError::BudgetExceeded)
            );
        }
        let limits = CompositeLimits {
            max_input_bytes: 4,
            max_output_bytes: 4,
            max_pixel_visits: 3,
            ..CompositeLimits::default()
        };
        assert!(
            crossfade_rgba(&a, &a, 1, 2, limits).is_ok(),
            "one retained input allocation is counted once"
        );
    }

    #[test]
    fn audio_factors_are_numeric_only_with_checked_time_and_full_u64_range() {
        assert_eq!(fade_audio_gains(0, 100), Ok((1.0, 0.0)));
        assert_eq!(fade_audio_gains(25, 100), Ok((0.75, 0.25)));
        assert_eq!(fade_audio_gains(100, 100), Ok((0.0, 1.0)));
        assert_eq!(fade_audio_gains(u64::MAX, u64::MAX), Ok((0.0, 1.0)));
        assert_eq!(fade_audio_gains(1, 0), Err(MediaError::InvalidTransition));
        assert_eq!(fade_audio_gains(2, 1), Err(MediaError::InvalidTransition));
    }
}
