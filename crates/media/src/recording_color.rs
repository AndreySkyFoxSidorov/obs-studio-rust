//! Opaque encoded SDR Program RGBA to BT.709 limited-range NV12 for encoders.
use crate::{MediaError, RgbaFrame};

const fn normalization_table() -> [f64; 256] {
    let mut table = [0.0; 256];
    let mut byte = 0;
    while byte < table.len() {
        // Preserve division itself, rather than multiply by an approximate
        // reciprocal. All 256 bits are checked against runtime IEEE division.
        table[byte] = byte as f64 / 255.0;
        byte += 1;
    }
    table
}
static NORMALIZED_RGB: [f64; 256] = normalization_table();

/// Only the finite [1,255] domain proved for these Y/U/V expressions.
/// Half-integer predecessor spacing matches the next integer for n>=1;
/// monotone IEEE addition cannot move a below-threshold value over it.
/// This is NOT a general f64 round helper (notably, not for 0.5 - ULP).
#[inline]
fn quantize_positive_limited(value: f64, min: u8, max: u8) -> u8 {
    ((value + 0.5) as u8).clamp(min, max)
}

pub fn recording_nv12(
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
    // The shared Program compositor must flatten transparency before encoding.
    // Reject a caller supplying a different color contract instead of producing
    // a dark/transparent fringe in a format with no alpha channel.
    if frame.pixels().chunks_exact(4).any(|p| p[3] != 255) {
        return Err(MediaError::InvalidBuffer);
    }
    let mut output = vec![0; bytes];
    let rgb = |x: usize, y: usize| {
        let p = &frame.pixels()[(y * width + x) * 4..];
        [
            NORMALIZED_RGB[p[0] as usize],
            NORMALIZED_RGB[p[1] as usize],
            NORMALIZED_RGB[p[2] as usize],
        ]
    };
    let luma = |p: [f64; 3]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let mut mean = [0.0; 3];
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = rgb(x + dx, y + dy);
                    output[(y + dy) * width + x + dx] =
                        quantize_positive_limited(16.0 + 219.0 * luma(p), 16, 235);
                    for c in 0..3 {
                        mean[c] += p[c] * 0.25;
                    }
                }
            }
            let yy = luma(mean);
            let at = pixels + (y / 2) * width + x;
            output[at] = quantize_positive_limited(
                128.0 + 224.0 * (mean[2] - yy) / (2.0 * (1.0 - 0.0722)),
                16,
                240,
            );
            output[at + 1] = quantize_positive_limited(
                128.0 + 224.0 * (mean[0] - yy) / (2.0 * (1.0 - 0.2126)),
                16,
                240,
            );
        }
    }
    Ok(output)
}

#[cfg(test)]
#[path = "recording_color_tests.rs"]
mod equivalence;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_709_limited_primaries_and_chroma_average() {
        for (rgb, y, u, v) in [
            ([0, 0, 0], 16, 128, 128),
            ([255, 255, 255], 235, 128, 128),
            ([255, 0, 0], 63, 102, 240),
            ([0, 255, 0], 173, 42, 26),
            ([0, 0, 255], 32, 240, 118),
        ] {
            let f = RgbaFrame::new(2, 2, [rgb[0], rgb[1], rgb[2], 255].repeat(4)).unwrap();
            assert_eq!(recording_nv12(&f, 6, 8).unwrap(), [y, y, y, y, u, v]);
        }
        let f = RgbaFrame::new(
            2,
            2,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
        )
        .unwrap();
        assert_eq!(
            recording_nv12(&f, 6, 8).unwrap(),
            [63, 173, 32, 235, 128, 128]
        );
    }
    #[test]
    fn rejects_nonopaque_odd_and_budget_inputs() {
        let f = RgbaFrame::new(2, 2, [255, 255, 255, 128].repeat(4)).unwrap();
        assert_eq!(recording_nv12(&f, 6, 8), Err(MediaError::InvalidBuffer));
        let f = RgbaFrame::new(3, 2, [0, 0, 0, 255].repeat(6)).unwrap();
        assert_eq!(
            recording_nv12(&f, 9, 12),
            Err(MediaError::InvalidDimensions)
        );
        let f = RgbaFrame::new(2, 2, [0, 0, 0, 255].repeat(4)).unwrap();
        assert_eq!(recording_nv12(&f, 5, 8), Err(MediaError::BudgetExceeded));
        assert_eq!(recording_nv12(&f, 6, 7), Err(MediaError::BudgetExceeded));
    }
}
