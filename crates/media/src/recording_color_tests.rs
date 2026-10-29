//! Independent frozen two-pass oracle for the fused conversion. No native I/O.
use super::*;

#[test]
fn all_normalization_lut_entries_match_runtime_division_bits() {
    for byte in 0..=255u8 {
        let runtime = std::hint::black_box(byte) as f64 / std::hint::black_box(255.0);
        assert_eq!(
            NORMALIZED_RGB[byte as usize].to_bits(),
            runtime.to_bits(),
            "normalization byte {byte}"
        );
    }
    assert_eq!(std::mem::size_of_val(&NORMALIZED_RGB), 2048);
    assert_eq!(NORMALIZED_RGB[0].to_bits(), 0.0f64.to_bits());
    assert_eq!(NORMALIZED_RGB[255].to_bits(), 1.0f64.to_bits());
}

fn assert_quantizer_exact(value: f64) {
    assert!(value.is_finite() && (1.0..=255.0).contains(&value));
    for (min, max) in [(1, 255), (16, 235), (16, 240)] {
        let value = std::hint::black_box(value);
        let old = value.round().clamp(f64::from(min), f64::from(max)) as u8;
        assert_eq!(
            quantize_positive_limited(value, min, max),
            old,
            "quantizer x={value:?} bits={:#018x} limits={min}..{max}",
            value.to_bits()
        );
    }
}

#[test]
fn positive_quantizer_half_integer_neighbors_and_endpoints_are_exact() {
    // Includes ALL half-integers around the 16..240 output range and the
    // power-of-two transitions; wider bounds also expose any hidden mismatch
    // that the limited-range clamp might otherwise conceal.
    for integer in 1..255 {
        let threshold = (f64::from(integer) + 0.5).to_bits();
        for offset in -256i64..=256 {
            assert_quantizer_exact(f64::from_bits((threshold as i64 + offset) as u64));
        }
    }
    for endpoint in [
        1.0f64, 2.0, 4.0, 8.0, 15.0, 16.0, 32.0, 64.0, 128.0, 235.0, 240.0, 241.0, 255.0,
    ] {
        for offset in -256i64..=256 {
            let value = f64::from_bits((endpoint.to_bits() as i64 + offset) as u64);
            if (1.0..=255.0).contains(&value) {
                assert_quantizer_exact(value);
            }
        }
    }
    let below_half = f64::from_bits(0.5f64.to_bits() - 1);
    assert_eq!(below_half.round() as u8, 0);
    assert_eq!(
        (below_half + 0.5) as u8,
        1,
        "known counterexample outside the proved domain"
    );
}

#[test]
fn positive_quantizer_random_f64_domain_and_actual_chroma_values_match_old_round() {
    let mut seed = 0x8502_73a4_4ff9_68bd;
    let first = 1.0f64.to_bits();
    let last = 255.0f64.to_bits();
    for _ in 0..20_000 {
        let _ = next(&mut seed);
        assert_quantizer_exact(f64::from_bits(first + seed % (last - first + 1)));
        let mut mean = [0.0; 3];
        for _dy in 0..2 {
            for _dx in 0..2 {
                let packed = next(&mut seed);
                let rgb = [
                    NORMALIZED_RGB[(packed & 255) as usize],
                    NORMALIZED_RGB[((packed >> 8) & 255) as usize],
                    NORMALIZED_RGB[((packed >> 16) & 255) as usize],
                ];
                let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
                assert_quantizer_exact(16.0 + 219.0 * luma);
                for c in 0..3 {
                    mean[c] += rgb[c] * 0.25;
                }
            }
        }
        let yy = 0.2126 * mean[0] + 0.7152 * mean[1] + 0.0722 * mean[2];
        assert_quantizer_exact(128.0 + 224.0 * (mean[2] - yy) / (2.0 * (1.0 - 0.0722)));
        assert_quantizer_exact(128.0 + 224.0 * (mean[0] - yy) / (2.0 * (1.0 - 0.2126)));
    }
}

/// The pre-fusion algorithm, including its original preflight and expressions.
/// Do not route this through the optimized helper: it is a forced reference.
fn reference(
    frame: &RgbaFrame,
    max_output_bytes: usize,
    max_pixel_visits: u64,
) -> Result<Vec<u8>, MediaError> {
    let (width, height) = (frame.width() as usize, frame.height() as usize);
    if width % 2 != 0 || height % 2 != 0 {
        return Err(MediaError::InvalidDimensions);
    }
    let pixels = width.checked_mul(height).ok_or(MediaError::Overflow)?;
    let bytes = pixels.checked_add(pixels / 2).ok_or(MediaError::Overflow)?;
    if bytes > max_output_bytes
        || (pixels as u64).checked_mul(2).ok_or(MediaError::Overflow)? > max_pixel_visits
    {
        return Err(MediaError::BudgetExceeded);
    }
    if frame.pixels().chunks_exact(4).any(|p| p[3] != 255) {
        return Err(MediaError::InvalidBuffer);
    }
    let mut output = vec![0; bytes];
    let rgb = |x: usize, y: usize| {
        let p = &frame.pixels()[(y * width + x) * 4..];
        [
            p[0] as f64 / 255.0,
            p[1] as f64 / 255.0,
            p[2] as f64 / 255.0,
        ]
    };
    let luma = |p: [f64; 3]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    for y in 0..height {
        for x in 0..width {
            output[y * width + x] =
                (16.0 + 219.0 * luma(rgb(x, y))).round().clamp(16.0, 235.0) as u8;
        }
    }
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let mut mean = [0.0; 3];
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = rgb(x + dx, y + dy);
                    for c in 0..3 {
                        mean[c] += p[c] * 0.25;
                    }
                }
            }
            let yy = luma(mean);
            let at = pixels + (y / 2) * width + x;
            output[at] = (128.0 + 224.0 * (mean[2] - yy) / (2.0 * (1.0 - 0.0722)))
                .round()
                .clamp(16.0, 240.0) as u8;
            output[at + 1] = (128.0 + 224.0 * (mean[0] - yy) / (2.0 * (1.0 - 0.2126)))
                .round()
                .clamp(16.0, 240.0) as u8;
        }
    }
    Ok(output)
}

fn assert_exact(frame: &RgbaFrame) {
    let pixels = u64::from(frame.width()) * u64::from(frame.height());
    assert_eq!(
        recording_nv12(frame, usize::MAX, pixels * 2),
        reference(frame, usize::MAX, pixels * 2),
        "exact NV12 for {}×{}",
        frame.width(),
        frame.height()
    );
}

#[test]
fn boundary_rgb_tiles_and_mixed_chroma_means_are_byte_exact() {
    let values = [
        0, 1, 15, 16, 17, 63, 64, 127, 128, 129, 234, 235, 239, 240, 254, 255,
    ];
    for mixed in [false, true] {
        let mut rgba = vec![255; 128 * 128 * 4];
        for tile in 0..4096 {
            let rgb = [
                values[(tile >> 8) & 15],
                values[(tile >> 4) & 15],
                values[tile & 15],
            ];
            let x = (tile % 64) * 2;
            let y = (tile / 64) * 2;
            for dy in 0..2 {
                for dx in 0..2 {
                    let at = ((y + dy) * 128 + x + dx) * 4;
                    let p = if mixed {
                        match dy * 2 + dx {
                            0 => rgb,
                            1 => [rgb[1], rgb[2], rgb[0]],
                            2 => [255 - rgb[2], rgb[0], rgb[1]],
                            _ => [rgb[2], 255 - rgb[1], rgb[0]],
                        }
                    } else {
                        rgb
                    };
                    rgba[at..at + 3].copy_from_slice(&p);
                }
            }
        }
        assert_exact(&RgbaFrame::new(128, 128, rgba).unwrap());
    }
    // Chroma accumulation is deliberately dy then dx, not separable averages.
    for rotation in 0..4 {
        let colors = [
            [0, 1, 254, 255],
            [255, 127, 17, 255],
            [16, 240, 128, 255],
            [235, 64, 129, 255],
        ];
        let bytes = (0..4).flat_map(|i| colors[(i + rotation) % 4]).collect();
        assert_exact(&RgbaFrame::new(2, 2, bytes).unwrap());
    }
}

fn next(seed: &mut u64) -> u32 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (*seed >> 32) as u32
}

#[test]
fn varied_even_images_and_random_pixels_match_forced_two_pass_reference() {
    let mut seed = 0x5c3a_4862_719e_d104;
    for case in 0..400 {
        let (width, height) = match case {
            0 => (2, 2),
            1 => (2, 258),
            2 => (258, 2),
            3 => (1920, 2),
            4 => (2, 1080),
            5 => (640, 360),
            _ => (
                (next(&mut seed) % 63 + 1) * 2,
                (next(&mut seed) % 47 + 1) * 2,
            ),
        };
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..width * height {
            let rgb = next(&mut seed);
            pixels.extend_from_slice(&[rgb as u8, (rgb >> 8) as u8, (rgb >> 16) as u8, 255]);
        }
        assert_exact(&RgbaFrame::new(width, height, pixels).unwrap());
    }
}

#[test]
fn validation_and_budget_precedence_remain_identical() {
    for (width, height) in [(1, 1), (3, 2), (2, 3), (2, 2), (4, 8)] {
        for alpha in [0, 128, 254, 255] {
            let mut bytes = [12, 128, 244, 255].repeat(width * height);
            let last = bytes.len() - 1;
            bytes[last] = alpha;
            let frame = RgbaFrame::new(width as u32, height as u32, bytes).unwrap();
            let output = width * height * 3 / 2;
            for max_bytes in [0, output.saturating_sub(1), output, output + 1] {
                for visits in [
                    0,
                    (width * height * 2 - 1) as u64,
                    (width * height * 2) as u64,
                ] {
                    assert_eq!(
                        recording_nv12(&frame, max_bytes, visits),
                        reference(&frame, max_bytes, visits)
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "opt-in exhaustive 16,777,216 RGB values in uniform 2×2 chroma tiles; no devices"]
fn exhaustive_rgb_cube_uniform_tiles_match_full_nv12_reference() {
    for r in 0..=255u8 {
        let mut pixels = vec![255; 512 * 512 * 4];
        for g in 0..256usize {
            for b in 0..256usize {
                for dy in 0..2 {
                    for dx in 0..2 {
                        let at = ((b * 2 + dy) * 512 + g * 2 + dx) * 4;
                        pixels[at..at + 3].copy_from_slice(&[r, g as u8, b as u8]);
                    }
                }
            }
        }
        assert_exact(&RgbaFrame::new(512, 512, pixels).unwrap());
    }
}

#[test]
#[ignore = "opt-in isolated release-mode 1080p NV12 stage measurement; no encoder/device/GUI"]
fn measure_fused_vs_two_pass_1080p_conversion_stage() {
    use std::{hint::black_box, time::Instant};
    assert!(
        !cfg!(debug_assertions),
        "run this measurement in release mode"
    );
    let mut seed = 0x3405_12a7_948b_a330;
    let mut pixels = Vec::with_capacity(1920 * 1080 * 4);
    for _ in 0..1920 * 1080 {
        let value = next(&mut seed);
        pixels.extend_from_slice(&[value as u8, (value >> 8) as u8, (value >> 16) as u8, 255]);
    }
    let frame = RgbaFrame::new(1920, 1080, pixels).unwrap();
    let bytes = 1920 * 1080 * 3 / 2;
    let visits = 1920 * 1080 * 2;
    let expected = reference(&frame, bytes, visits).unwrap();
    assert_eq!(recording_nv12(&frame, bytes, visits).unwrap(), expected);
    let mut fused = Vec::new();
    let mut old = Vec::new();
    for iteration in 0..30 {
        // Alternate ordering so neither implementation always follows a cold
        // input; this is one stage measurement, not an end-to-end FPS claim.
        for optimized in [iteration % 2 == 0, iteration % 2 != 0] {
            let start = Instant::now();
            let output = if optimized {
                recording_nv12(black_box(&frame), bytes, visits)
            } else {
                reference(black_box(&frame), bytes, visits)
            }
            .unwrap();
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(output, expected);
            black_box(output);
            if optimized {
                fused.push(elapsed);
            } else {
                old.push(elapsed);
            }
        }
    }
    fused.sort_by(f64::total_cmp);
    old.sort_by(f64::total_cmp);
    eprintln!(
        "NV12 stage only,1920x1080,opaque randomRGB,30 samples each; fused median_ms={:.6} p95_ms={:.6}; two_pass median_ms={:.6} p95_ms={:.6}",
        fused[15], fused[28], old[15], old[28]
    );
}
