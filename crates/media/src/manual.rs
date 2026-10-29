//! OBS manual-transition torque in its original f32 arithmetic, without a clock.
use crate::MediaError;

fn validate(progress: f32) -> Result<(), MediaError> {
    if !progress.is_finite() || !(0.0..=1.0).contains(&progress) {
        return Err(MediaError::InvalidTransition);
    }
    Ok(())
}

/// Perform one numerical step. The host owns rational video ticks and caps the
/// number of catch-up steps. Small differences snap even for a zero delta,
/// matching OBS's source arithmetic rather than introducing a duration clamp.
pub fn manual_torque_step(current: f32, target: f32, delta_ns: u64) -> Result<f32, MediaError> {
    validate(current)?;
    validate(target)?;
    let difference = target - current;
    if difference.abs() < 0.0001 {
        return Ok(target);
    }
    let mut velocity = difference * 8.0;
    if velocity.abs() < 0.05 {
        velocity = if difference < 0.0 { -0.05 } else { 0.05 };
    }
    // obs-video.c divides the integer nanoseconds in double precision before
    // converting seconds to float; casting nanoseconds first changes NTSC ticks.
    let seconds = (delta_ns as f64 / 1_000_000_000.0) as f32;
    let next = current + velocity * seconds;
    Ok(if difference < 0.0 {
        next.max(target)
    } else {
        next.min(target)
    })
}

/// Exact reduced binary rational for a finite f32 in [0,1]. Very small values
/// whose exact denominator requires 2^64 or more are rejected, never rounded.
/// All progress values produced by normal manual-transition movement fit.
pub fn progress_rational(progress: f32) -> Result<(u64, u64), MediaError> {
    validate(progress)?;
    if progress == 0.0 {
        return Ok((0, 1));
    }
    let bits = progress.to_bits();
    let exponent = ((bits >> 23) & 255) as i32;
    let fraction = bits & 0x7f_ffff;
    let (mantissa, binary_exponent) = if exponent == 0 {
        (fraction, -149)
    } else {
        (fraction | 0x80_0000, exponent - 127 - 23)
    };
    let trailing = mantissa.trailing_zeros();
    let numerator = u64::from(mantissa >> trailing);
    let power = binary_exponent + trailing as i32;
    if power >= 0 {
        return Ok((
            numerator
                .checked_shl(power as u32)
                .ok_or(MediaError::InvalidTransition)?,
            1,
        ));
    }
    let shift = (-power) as u32;
    let denominator = 1u64
        .checked_shl(shift)
        .ok_or(MediaError::InvalidTransition)?;
    Ok((numerator, denominator))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ntsc_tick_divides_in_double_before_float_torque_arithmetic() {
        // Source-format oracle: 33,366,667ns / 1e9 in f64, then f32, has
        // bits 3D08AB7C. Velocity is exactly1, so the torque result is dt.
        assert_eq!(
            manual_torque_step(0.0, 0.125, 33_366_667)
                .unwrap()
                .to_bits(),
            0x3D08_AB7C
        );
    }

    #[test]
    fn torque_uses_source_velocity_floor_snap_and_overshoot() {
        assert_eq!(manual_torque_step(0.0, 1.0, 125_000_000).unwrap(), 1.0);
        assert_eq!(manual_torque_step(0.0, 1.0, 62_500_000).unwrap(), 0.5);
        assert_eq!(manual_torque_step(1.0, 0.0, 62_500_000).unwrap(), 0.5);
        // 0.001*8 is below the 0.05 minimum velocity.
        assert_eq!(
            manual_torque_step(0.0, 0.001, 10_000_000).unwrap(),
            0.05f32 * 0.01f32
        );
        assert_eq!(
            manual_torque_step(0.001, 0.0, 10_000_000).unwrap(),
            0.001f32 - 0.05f32 * 0.01f32
        );
        assert_eq!(manual_torque_step(0.0, 0.00005, 0).unwrap(), 0.00005);
        assert_eq!(manual_torque_step(0.25, 0.75, 0).unwrap(), 0.25);
        assert_eq!(manual_torque_step(0.25, 0.75, u64::MAX).unwrap(), 0.75);
        assert_eq!(manual_torque_step(0.75, 0.25, u64::MAX).unwrap(), 0.25);
    }

    #[test]
    fn progress_is_an_exact_binary_fraction_including_its_capacity_boundary() {
        for (progress, expected) in [
            (0.0, (0, 1)),
            (-0.0, (0, 1)),
            (1.0, (1, 1)),
            (0.25, (1, 4)),
            (0.1, (13_421_773, 134_217_728)),
            (2f32.powi(-63), (1, 1u64 << 63)),
        ] {
            assert_eq!(progress_rational(progress).unwrap(), expected);
            assert_eq!((expected.0 as f64 / expected.1 as f64) as f32, progress);
        }
        for progress in [
            2f32.powi(-64),
            f32::from_bits(1),
            f32::NAN,
            f32::INFINITY,
            -0.1,
            1.1,
        ] {
            assert_eq!(
                progress_rational(progress),
                Err(MediaError::InvalidTransition)
            );
        }
        for progress in [f32::NAN, f32::NEG_INFINITY, -0.1, 1.1] {
            assert_eq!(
                manual_torque_step(progress, 0.5, 1),
                Err(MediaError::InvalidTransition)
            );
            assert_eq!(
                manual_torque_step(0.5, progress, 1),
                Err(MediaError::InvalidTransition)
            );
        }
    }
}
