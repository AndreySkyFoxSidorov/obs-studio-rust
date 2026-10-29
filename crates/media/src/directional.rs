//! Pure CPU SDR Swipe/Slide from the pinned OBS transition callbacks/effects.
//! Input/output are straight RGBA8. General partial-alpha inputs adapt texture
//! filtering by premultiplying linear RGB before interpolation; opaque scenes
//! and transparent-black null textures match the supported OBS shader path.
use crate::{CompositeLimits, MediaError, RgbaFrame};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DirectionalTransition {
    Swipe {
        direction: Direction,
        swipe_in: bool,
    },
    Slide {
        direction: Direction,
    },
}
impl Direction {
    fn vector(self) -> (f32, f32) {
        match self {
            Self::Left => (1.0, 0.0),
            Self::Right => (-1.0, 0.0),
            Self::Up => (0.0, 1.0),
            Self::Down => (0.0, -1.0),
        }
    }
}

// Every positive u64 ratio >=1/u64::MAX is a normal f32. Round its 24-bit
// significand directly with integer division, including ties to even, rather
// than passing through f64 (which can double-round an exact near-halfway ratio).
pub(crate) fn progress(elapsed: u64, duration: u64) -> Result<f32, MediaError> {
    if duration == 0 || elapsed > duration {
        return Err(MediaError::InvalidTransition);
    }
    if elapsed == 0 {
        return Ok(0.0);
    }
    if elapsed == duration {
        return Ok(1.0);
    }
    let mut exponent =
        (64 - elapsed.leading_zeros()) as i32 - (64 - duration.leading_zeros()) as i32;
    if (u128::from(elapsed) << (-exponent) as u32) < u128::from(duration) {
        exponent -= 1;
    }
    let scaled = u128::from(elapsed) << (23 - exponent) as u32;
    let denominator = u128::from(duration);
    let mut significand = scaled / denominator;
    let remainder = scaled % denominator;
    if remainder * 2 > denominator || (remainder * 2 == denominator && significand & 1 != 0) {
        significand += 1;
    }
    if significand == 1 << 24 {
        significand >>= 1;
        exponent += 1;
    }
    Ok(f32::from_bits(
        ((exponent + 127) as u32) << 23 | ((significand as u32) & 0x7f_ffff),
    ))
}
fn cubic(t: f32) -> f32 {
    // Keep operation order and f32 intermediates from easings.h. In particular,
    // the upper branch is not rewritten as 1-4*(1-t)^3.
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let temp = 2.0 * t - 2.0;
        (t - 1.0) * temp * temp + 1.0
    }
}
/// Numeric OBS Swipe/Slide audio factors (1-ease(t),ease(t)); no devices/mixing.
pub fn directional_audio_gains(
    elapsed_ns: u64,
    duration_ns: u64,
) -> Result<(f32, f32), MediaError> {
    let eased = cubic(progress(elapsed_ns, duration_ns)?);
    Ok((1.0 - eased, eased))
}

fn decode(value: u8) -> f64 {
    // The input is an 8-bit channel, so there are only 256 possible sRGB
    // values. Swipe/slide sampling reads four texels per output pixel; doing
    // the same power function for every channel on every frame makes these
    // transitions unnecessarily expensive at camera and stream resolutions.
    static SRGB_TO_LINEAR: std::sync::OnceLock<[f64; 256]> = std::sync::OnceLock::new();
    SRGB_TO_LINEAR.get_or_init(|| std::array::from_fn(|index| {
        let v = index as f64 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }))[usize::from(value)]
}
fn encode(value: f64) -> u8 {
    let v = value.clamp(0.0, 1.0);
    let v = if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}
fn inside((u, v): (f32, f32)) -> bool {
    (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)
}

fn sample(image: &RgbaFrame, (u, v): (f32, f32), output: &mut [u8]) {
    // GPU normalized linear sampling places texel centers at (index+.5)/size.
    // Clamp each tap, including when the selected UV lands exactly at0 or1.
    let x = u * image.width() as f32 - 0.5;
    let y = v * image.height() as f32 - 0.5;
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let fx = f64::from(x - x.floor());
    let fy = f64::from(y - y.floor());
    let mut accumulated = [0.0f64; 4];
    for (tx, ty, weight) in [
        (x0, y0, (1.0 - fx) * (1.0 - fy)),
        (x0 + 1, y0, fx * (1.0 - fy)),
        (x0, y0 + 1, (1.0 - fx) * fy),
        (x0 + 1, y0 + 1, fx * fy),
    ] {
        let tx = tx.clamp(0, i64::from(image.width()) - 1) as usize;
        let ty = ty.clamp(0, i64::from(image.height()) - 1) as usize;
        let index = (ty * image.width() as usize + tx) * 4;
        let pixel = &image.pixels()[index..index + 4];
        let alpha = f64::from(pixel[3]) / 255.0;
        accumulated[3] += alpha * weight;
        for channel in 0..3 {
            accumulated[channel] += decode(pixel[channel]) * alpha * weight;
        }
    }
    output[3] = (accumulated[3] * 255.0).round().clamp(0.0, 255.0) as u8;
    if accumulated[3] == 0.0 {
        output[..3].fill(0);
    } else {
        for channel in 0..3 {
            output[channel] = encode(accumulated[channel] / accumulated[3]);
        }
    }
}

fn select(effect:DirectionalTransition,eased:f32,uv:(f32,f32))->(bool,(f32,f32)){
match effect {
                DirectionalTransition::Swipe {
                    direction,
                    swipe_in,
                } => {
                    let (dx, dy) = direction.vector();
                    let amount = if swipe_in { 1.0 - eased } else { eased };
                    let direction = if swipe_in { (-dx, -dy) } else { (dx, dy) };
                    let shifted = (uv.0 + direction.0 * amount, uv.1 + direction.1 * amount);
                    let (first, second) = if swipe_in { (false, true) } else { (true, false) };
                    if inside(shifted) {
                        (first, shifted)
                    } else {
                        (second, uv)
                    }
                }
                DirectionalTransition::Slide { direction } => {
                    let (dx, dy) = direction.vector();
                    let shifted = (uv.0 + dx * eased, uv.1 + dy * eased);
                    if inside(shifted) {
                        (true, shifted)
                    } else {
                        (false, (uv.0 - dx * (1.0 - eased), uv.1 - dy * (1.0 - eased)))
                    }
                }
            }
}

/// Select and bilinearly sample two equal-sized SDR scenes using OBS's f32
/// cubic easing, pixel-center UVs, inclusive bounds and clamp addressing.
/// Validates two layers, unique input bytes, output bytes and five conservative
/// visits per output pixel before allocation, even at an exact endpoint.
pub fn directional_rgba(
    a: &RgbaFrame,
    b: &RgbaFrame,
    elapsed_ns: u64,
    duration_ns: u64,
    effect: DirectionalTransition,
    limits: CompositeLimits,
) -> Result<RgbaFrame, MediaError> {
    let eased = cubic(progress(elapsed_ns, duration_ns)?);
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
        .and_then(|n| n.checked_mul(5))
        .ok_or(MediaError::Overflow)?;
    if limits.max_layers < 2
        || input_bytes > limits.max_input_bytes
        || a.byte_len() > limits.max_output_bytes
        || visits > limits.max_pixel_visits
    {
        return Err(MediaError::BudgetExceeded);
    }
    if elapsed_ns == 0 {
        return Ok(a.clone());
    }
    if elapsed_ns == duration_ns {
        return Ok(b.clone());
    }
    let mut output = vec![0; a.byte_len()];
    for y in 0..a.height() {
        for x in 0..a.width() {
            let uv = (
                (x as f32 + 0.5) / a.width() as f32,
                (y as f32 + 0.5) / a.height() as f32,
            );
            let (first,sample_uv)=select(effect,eased,uv);let image=if first{a}else{b};
            let index = (y as usize * a.width() as usize + x as usize) * 4;
            sample(image, sample_uv, &mut output[index..index + 4]);
        }
    }
    RgbaFrame::new(a.width(), a.height(), output)
}

/// Same OBS easing/UV callback as directional_rgba, with explicit all4 source
/// alpha ingress modes and PremultipliedLinearSrgb output. Original API keeps
/// its straight-alpha contract; callers carry this result's actual blend tag.
pub fn directional_rgba_with_blend(a:&RgbaFrame,a_blend:crate::SourceBlend,b:&RgbaFrame,b_blend:crate::SourceBlend,elapsed:u64,duration:u64,effect:DirectionalTransition,limits:CompositeLimits)->Result<RgbaFrame,MediaError>{
    let eased=cubic(progress(elapsed,duration)?);crate::source_transition::admit(a,b,limits,5)?;let mut output=Vec::new();output.try_reserve_exact(a.byte_len()).map_err(|_|MediaError::BudgetExceeded)?;output.resize(a.byte_len(),0);
    for y in 0..a.height(){for x in 0..a.width(){let uv=((x as f32+0.5)/a.width()as f32,(y as f32+0.5)/a.height()as f32);let(first,uv)=select(effect,eased,uv);let(image,blend)=if first{(a,a_blend)}else{(b,b_blend)};
        let px=uv.0*image.width()as f32-0.5;let py=uv.1*image.height()as f32-0.5;let x0=px.floor()as i64;let y0=py.floor()as i64;let fx=f64::from(px-px.floor());let fy=f64::from(py-py.floor());let mut sum=[0.0;4];
        for(tx,ty,weight)in[(x0,y0,(1.0-fx)*(1.0-fy)),(x0+1,y0,fx*(1.0-fy)),(x0,y0+1,(1.0-fx)*fy),(x0+1,y0+1,fx*fy)]{let tx=tx.clamp(0,i64::from(image.width())-1)as usize;let ty=ty.clamp(0,i64::from(image.height())-1)as usize;let at=(ty*image.width()as usize+tx)*4;let value=crate::source_transition::lp(&image.pixels()[at..at+4],blend);for c in 0..4{sum[c]+=value[c]*weight;}}
        let at=(y as usize*a.width()as usize+x as usize)*4;crate::source_transition::store(sum,&mut output[at..at+4]);
    }}RgbaFrame::new(a.width(),a.height(),output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pattern(vertical: bool) -> (RgbaFrame, RgbaFrame) {
        let (a, b) = (
            [10, 20, 30, 40]
                .into_iter()
                .flat_map(|v| [v, 0, 0, 255])
                .collect(),
            [50, 60, 70, 80]
                .into_iter()
                .flat_map(|v| [0, 0, v, 255])
                .collect(),
        );
        let (w, h) = if vertical { (1, 4) } else { (4, 1) };
        (
            RgbaFrame::new(w, h, a).unwrap(),
            RgbaFrame::new(w, h, b).unwrap(),
        )
    }
    fn run(
        a: &RgbaFrame,
        b: &RgbaFrame,
        effect: DirectionalTransition,
        elapsed: u64,
        duration: u64,
    ) -> RgbaFrame {
        directional_rgba(a, b, elapsed, duration, effect, CompositeLimits::default()).unwrap()
    }
    #[test]
    fn every_direction_has_patterned_slide_swipe_in_and_swipe_out_oracles() {
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            let (a, b) = pattern(matches!(direction, Direction::Up | Direction::Down));
            let positive = matches!(direction, Direction::Left | Direction::Up);
            let slide = if positive {
                [
                    [30, 0, 0, 255],
                    [40, 0, 0, 255],
                    [0, 0, 50, 255],
                    [0, 0, 60, 255],
                ]
            } else {
                [
                    [0, 0, 70, 255],
                    [0, 0, 80, 255],
                    [10, 0, 0, 255],
                    [20, 0, 0, 255],
                ]
            };
            let out = if positive {
                [
                    [30, 0, 0, 255],
                    [40, 0, 0, 255],
                    [0, 0, 70, 255],
                    [0, 0, 80, 255],
                ]
            } else {
                [
                    [0, 0, 50, 255],
                    [0, 0, 60, 255],
                    [10, 0, 0, 255],
                    [20, 0, 0, 255],
                ]
            };
            let incoming = if positive {
                [
                    [10, 0, 0, 255],
                    [20, 0, 0, 255],
                    [0, 0, 50, 255],
                    [0, 0, 60, 255],
                ]
            } else {
                [
                    [0, 0, 70, 255],
                    [0, 0, 80, 255],
                    [30, 0, 0, 255],
                    [40, 0, 0, 255],
                ]
            };
            for (effect, expected) in [
                (DirectionalTransition::Slide { direction }, slide),
                (
                    DirectionalTransition::Swipe {
                        direction,
                        swipe_in: false,
                    },
                    out,
                ),
                (
                    DirectionalTransition::Swipe {
                        direction,
                        swipe_in: true,
                    },
                    incoming,
                ),
            ] {
                let expected: Vec<u8> = expected.into_iter().flatten().collect();
                assert_eq!(run(&a, &b, effect, 1, 2).pixels(), expected, "{effect:?}");
                assert_eq!(run(&a, &b, effect, 0, 2), a);
                assert_eq!(run(&a, &b, effect, 2, 2), b);
            }
        }
    }
    #[test]
    fn linear_light_bilinear_subpixel_sampling_and_generalized_alpha_have_literal_oracles() {
        let a = RgbaFrame::new(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 255]).unwrap();
        let b = RgbaFrame::solid(2, 1, [255, 0, 0, 255]).unwrap();
        for effect in [
            DirectionalTransition::Slide {
                direction: Direction::Left,
            },
            DirectionalTransition::Swipe {
                direction: Direction::Left,
                swipe_in: false,
            },
        ] {
            // ease(1/4)=1/16; source texel coordinate1/8 has linear white gain1/8.
            assert_eq!(
                run(&a, &b, effect, 1, 4).pixels(),
                &[99, 99, 99, 255, 255, 255, 255, 255]
            );
        }
        let alpha = RgbaFrame::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 0]).unwrap();
        assert_eq!(
            run(
                &alpha,
                &b,
                DirectionalTransition::Slide {
                    direction: Direction::Left
                },
                1,
                4
            )
            .pixels(),
            &[255, 0, 0, 223, 0, 0, 0, 0]
        );
    }
    #[test]
    fn inclusive_uv_edges_clamp_to_a_texel_instead_of_switching_early() {
        let a = RgbaFrame::solid(1, 1, [7, 19, 31, 255]).unwrap();
        let b = RgbaFrame::solid(1, 1, [61, 73, 89, 255]).unwrap();
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            assert_eq!(
                run(&a, &b, DirectionalTransition::Slide { direction }, 1, 2),
                a
            );
            assert_eq!(
                run(
                    &a,
                    &b,
                    DirectionalTransition::Swipe {
                        direction,
                        swipe_in: false
                    },
                    1,
                    2
                ),
                a
            );
            assert_eq!(
                run(
                    &a,
                    &b,
                    DirectionalTransition::Swipe {
                        direction,
                        swipe_in: true
                    },
                    1,
                    2
                ),
                b
            );
        }
        let (a, _) = pattern(false);
        let mut left = [0; 4];
        let mut right = [0; 4];
        sample(&a, (0.0, 0.5), &mut left);
        sample(&a, (1.0, 0.5), &mut right);
        assert_eq!(left, [10, 0, 0, 255]);
        assert_eq!(right, [40, 0, 0, 255]);
    }
    #[test]
    fn exact_ratio_and_source_order_f32_easing_have_stable_bit_oracles() {
        for (n, d, bits) in [
            (0, 8, 0),
            (1, 8, 0x3c00_0000),
            (2, 8, 0x3d80_0000),
            (4, 8, 0x3f00_0000),
            (6, 8, 0x3f70_0000),
            (7, 8, 0x3f7e_0000),
            (8, 8, 0x3f80_0000),
        ] {
            let (a, b) = directional_audio_gains(n, d).unwrap();
            assert_eq!(b.to_bits(), bits);
            assert_eq!(a, 1.0 - b);
        }
        assert_eq!(progress(1, 3).unwrap().to_bits(), 0x3eaa_aaab);
        assert_eq!(cubic(f32::from_bits(0x3eff_ffff)).to_bits(), 0x3eff_fffd);
        assert_eq!(cubic(f32::from_bits(0x3f00_0001)).to_bits(), 0x3f00_0003);
        // Exact halfway between1/2 and its next f32; the adjacent rational below
        // it would double-round incorrectly if first rounded to f64.
        let halfway = (1u64 << 63) + (1u64 << 39);
        assert_eq!(
            progress(halfway - 1, u64::MAX).unwrap().to_bits(),
            0x3f00_0000
        );
        assert_eq!(progress(halfway, u64::MAX).unwrap().to_bits(), 0x3f00_0001);
        assert_eq!(progress(1, u64::MAX).unwrap().to_bits(), 0x1f80_0000);
    }
    #[test]
    fn time_dimensions_unique_allocations_and_endpoint_budgets_validate_before_allocation() {
        let (a, b) = pattern(false);
        let effect = DirectionalTransition::Slide {
            direction: Direction::Left,
        };
        let exact = CompositeLimits {
            max_layers: 2,
            max_input_bytes: 32,
            max_output_bytes: 16,
            max_pixel_visits: 20,
        };
        for elapsed in [0, 1, 2] {
            assert!(directional_rgba(&a, &b, elapsed, 2, effect, exact).is_ok());
        }
        for limits in [
            CompositeLimits {
                max_layers: 1,
                ..exact
            },
            CompositeLimits {
                max_input_bytes: 31,
                ..exact
            },
            CompositeLimits {
                max_output_bytes: 15,
                ..exact
            },
            CompositeLimits {
                max_pixel_visits: 19,
                ..exact
            },
        ] {
            for elapsed in [0, 1, 2] {
                assert_eq!(
                    directional_rgba(&a, &b, elapsed, 2, effect, limits),
                    Err(MediaError::BudgetExceeded)
                );
            }
        }
        assert!(
            directional_rgba(
                &a,
                &a,
                1,
                2,
                effect,
                CompositeLimits {
                    max_input_bytes: 16,
                    ..exact
                }
            )
            .is_ok()
        );
        let other = RgbaFrame::solid(1, 1, [0; 4]).unwrap();
        assert_eq!(
            directional_rgba(&a, &other, 0, 2, effect, exact),
            Err(MediaError::InvalidDimensions)
        );
        for (n, d) in [(0, 0), (1, 0), (2, 1), (u64::MAX, u64::MAX - 1)] {
            assert_eq!(
                directional_rgba(&a, &b, n, d, effect, exact),
                Err(MediaError::InvalidTransition)
            );
            assert_eq!(
                directional_audio_gains(n, d),
                Err(MediaError::InvalidTransition)
            );
        }
        assert!(directional_rgba(&a, &b, u64::MAX / 2, u64::MAX, effect, exact).is_ok());
    }
}
