//! Direct borrowed progressive8-bit4:2:0 planes to bounded opaque SDR RGBA.
use crate::{
    CameraMatrix, CameraRange, MediaError, PackedFrameLimits, RgbaFrame,
    camera_color::{Coefficients, chroma_axis},
};
#[derive(Clone, Copy, Debug)]
pub struct Planar420Layout {
    pub width: u32,
    pub height: u32,
    pub y_stride: usize,
    pub uv_stride: usize,
    pub matrix: CameraMatrix,
    pub range: CameraRange,
    pub chroma_left: bool,
}
/// Vertical siting of progressive 4:2:0 chroma samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chroma420Vertical { Center, Top, Bottom }

pub fn ingest_planar_420(
    y: &[u8], u: &[u8], v: &[u8], layout: Planar420Layout, limits: PackedFrameLimits,
) -> Result<RgbaFrame, MediaError> {
    ingest_planar_420_sited(y, u, v, layout, limits, Chroma420Vertical::Center)
}

/// Convert 4:2:0 with explicit vertical sample siting.
pub fn ingest_planar_420_sited(
    y: &[u8], u: &[u8], v: &[u8], layout: Planar420Layout, limits: PackedFrameLimits,
    vertical: Chroma420Vertical,
) -> Result<RgbaFrame, MediaError> {
    ingest_planar_420_with_halo(y, u, v, layout, limits, vertical, None)
}

/// Expanded chroma planes contain the crop plus at most one adjacent sample
/// on each side. Origin is measured in chroma samples.
#[derive(Clone, Copy, Debug)]
pub struct Chroma420Halo<'a> {
    pub u: &'a [u8], pub v: &'a [u8], pub stride: usize,
    pub width: usize, pub height: usize, pub origin_x: usize, pub origin_y: usize,
}

pub fn ingest_planar_420_with_halo(
    y: &[u8], u: &[u8], v: &[u8], layout: Planar420Layout, limits: PackedFrameLimits,
    vertical: Chroma420Vertical, halo: Option<Chroma420Halo<'_>>,
) -> Result<RgbaFrame, MediaError> {
    let Planar420Layout {
        width,
        height,
        y_stride,
        uv_stride,
        matrix,
        range,
        chroma_left,
    } = layout;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || width % 2 != 0
        || height % 2 != 0
    {
        return Err(MediaError::InvalidDimensions);
    }
    if y_stride < width as usize || uv_stride < width as usize / 2 {
        return Err(MediaError::InvalidBuffer);
    }
    let pixels = u64::from(width) * u64::from(height);
    let y_bytes = y_stride
        .checked_mul(height as usize)
        .ok_or(MediaError::Overflow)?;
    let uv_bytes = uv_stride
        .checked_mul(height as usize / 2)
        .ok_or(MediaError::Overflow)?;
    let backing = y
        .len()
        .checked_add(u.len())
        .and_then(|n| n.checked_add(v.len()))
        .ok_or(MediaError::Overflow)?;
    let output = (pixels as usize)
        .checked_mul(4)
        .ok_or(MediaError::Overflow)?;
    if y.len() < y_bytes || u.len() < uv_bytes || v.len() < uv_bytes {
        return Err(MediaError::InvalidBuffer);
    }
    if pixels > 33_554_432
        || backing > limits.max_input_bytes
        || output > limits.max_output_bytes
        || pixels * 6 > limits.max_pixel_visits
    {
        return Err(MediaError::BudgetExceeded);
    }
    let (cu, cv, cstride, cwidth, cheight, origin_x, origin_y) = if let Some(halo) = halo {
        let crop_w = width as usize / 2;
        let crop_h = height as usize / 2;
        let extent_x = halo.origin_x.checked_add(crop_w).ok_or(MediaError::Overflow)?;
        let extent_y = halo.origin_y.checked_add(crop_h).ok_or(MediaError::Overflow)?;
        let bytes = halo.stride.checked_mul(halo.height).ok_or(MediaError::Overflow)?;
        if halo.origin_x > 1 || halo.origin_y > 1 || halo.width < extent_x
            || halo.height < extent_y || halo.width - extent_x > 1
            || halo.height - extent_y > 1 || halo.stride < halo.width
            || halo.u.len() < bytes || halo.v.len() < bytes
            || backing.checked_add(bytes.checked_mul(2).ok_or(MediaError::Overflow)?)
                .is_none_or(|total| total > limits.max_input_bytes) {
            return Err(MediaError::InvalidBuffer);
        }
        (halo.u, halo.v, halo.stride, halo.width, halo.height, halo.origin_x, halo.origin_y)
    } else { (u, v, uv_stride, width as usize / 2, height as usize / 2, 0, 0) };
    let mut rgba = vec![0u8; output];
    let coefficients = Coefficients::new(matrix, range);
    // Height was validated even; each pair has independent vertical weights.
    for row in (0..height as usize).step_by(2) {
        let (c0, d0, ty0) = vertical_axis(row + origin_y * 2, cheight, vertical);
        let (c1, d1, ty1) = vertical_axis(row + 1 + origin_y * 2, cheight, vertical);
        for col in 0..width as usize {
            let (a, b, tx) = chroma_axis(col + origin_x * 2, cwidth, chroma_left);
            let sample_pair = |plane: &[u8]| {
                let horizontal = |r| {
                    let first = plane[r * cstride + a] as f64;
                    let second = plane[r * cstride + b] as f64;
                    first + (second - first) * tx
                };
                let top0 = horizontal(c0);
                let bottom0 = if d0 == c0 { top0 } else { horizontal(d0) };
                let top1 = if c1 == c0 { top0 } else if c1 == d0 { bottom0 } else { horizontal(c1) };
                let bottom1 = if d1 == c0 { top0 } else if d1 == d0 { bottom0 } else if d1 == c1 { top1 } else { horizontal(d1) };
                (top0 + (bottom0 - top0) * ty0, top1 + (bottom1 - top1) * ty1)
            };
            let (u0, u1) = sample_pair(cu);
            let (v0, v1) = sample_pair(cv);
            let first = coefficients.convert(y[row * y_stride + col] as f64, u0, v0);
            let second = coefficients.convert(y[(row + 1) * y_stride + col] as f64, u1, v1);
            let dest0 = (row * width as usize + col) * 4;
            let dest1 = ((row + 1) * width as usize + col) * 4;
            rgba[dest0..dest0 + 4].copy_from_slice(&[first[0], first[1], first[2], 255]);
            rgba[dest1..dest1 + 4].copy_from_slice(&[second[0], second[1], second[2], 255]);
        }
    }
    RgbaFrame::new(width, height, rgba)
}
fn vertical_axis(pixel: usize, count: usize, vertical: Chroma420Vertical) -> (usize, usize, f64) {
    match vertical {
        Chroma420Vertical::Center => chroma_axis(pixel, count, false),
        Chroma420Vertical::Top => chroma_axis(pixel, count, true),
        // Bottom samples are one luma line below top-sited samples.
        Chroma420Vertical::Bottom => chroma_axis(pixel.saturating_sub(1), count, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_padded_planar_black_white_and_color_fixture() {
        let frame = ingest_planar_420(
            &[16, 235, 9, 16, 235, 9],
            &[128, 9],
            &[128, 9],
            Planar420Layout {
                width: 2,
                height: 2,
                y_stride: 3,
                uv_stride: 2,
                matrix: CameraMatrix::Bt709,
                range: CameraRange::Limited,
                chroma_left: false,
            },
            PackedFrameLimits {
                max_pixel_visits: 24,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            frame.pixels(),
            &[
                0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255
            ]
        );
    }
    #[test]
    fn reject_short_planes_and_output_work_before_allocation() {
        let l = Planar420Layout {
            width: 2,
            height: 2,
            y_stride: 2,
            uv_stride: 1,
            matrix: CameraMatrix::Bt601,
            range: CameraRange::Full,
            chroma_left: false,
        };
        assert!(ingest_planar_420(&[0; 4], &[], &[128], l, Default::default()).is_err());
        assert!(
            ingest_planar_420(
                &[0; 4],
                &[128],
                &[128],
                l,
                PackedFrameLimits {
                    max_pixel_visits: 23,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
}
