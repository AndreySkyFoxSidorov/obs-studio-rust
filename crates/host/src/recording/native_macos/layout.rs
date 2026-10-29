//! Pure timestamp and bounded planar-copy admission shared by the SDK adapter tests.
use super::RecordingError;

pub(super) const MAX_BUFFER_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Copy, Debug)]
pub(super) struct PlaneLayout {
    pub width: usize,
    pub height: usize,
    pub y_stride: usize,
    pub uv_stride: usize,
    pub y_bytes: usize,
    pub uv_bytes: usize,
    pub packed_bytes: usize,
}
pub(super) fn plane_layout(
    width: usize,
    height: usize,
    y_stride: usize,
    uv_stride: usize,
    native_bytes: usize,
) -> Result<PlaneLayout, RecordingError> {
    if width == 0
        || height == 0
        || width % 2 != 0
        || height % 2 != 0
        || width > 4096
        || height > 4096
        || width
            .checked_mul(height)
            .is_none_or(|pixels| pixels > 8_847_360)
        || y_stride < width
        || uv_stride < width
    {
        return Err(RecordingError::invalid(
            "invalid recording NV12 plane dimensions or strides",
        ));
    }
    let y_bytes = y_stride
        .checked_mul(height)
        .ok_or_else(|| RecordingError::invalid("recording NV12 luma stride overflow"))?;
    let uv_bytes = uv_stride
        .checked_mul(height / 2)
        .ok_or_else(|| RecordingError::invalid("recording NV12 chroma stride overflow"))?;
    let extent = y_bytes
        .checked_add(uv_bytes)
        .ok_or_else(|| RecordingError::invalid("recording NV12 plane extent overflow"))?;
    if native_bytes > MAX_BUFFER_BYTES || extent > native_bytes {
        return Err(RecordingError::invalid(
            "recording NV12 native buffer exceeds admission",
        ));
    }
    let packed_bytes = width * height + width * (height / 2);
    Ok(PlaneLayout {
        width,
        height,
        y_stride,
        uv_stride,
        y_bytes,
        uv_bytes,
        packed_bytes,
    })
}
pub(super) fn copy_planes(
    packed: &[u8],
    y: &mut [u8],
    uv: &mut [u8],
    layout: PlaneLayout,
) -> Result<(), RecordingError> {
    if packed.len() != layout.packed_bytes
        || y.len() != layout.y_bytes
        || uv.len() != layout.uv_bytes
    {
        return Err(RecordingError::invalid(
            "recording NV12 owned plane shape changed",
        ));
    }
    y.fill(0);
    uv.fill(0);
    for row in 0..layout.height {
        y[row * layout.y_stride..][..layout.width]
            .copy_from_slice(&packed[row * layout.width..][..layout.width]);
    }
    let chroma = &packed[layout.width * layout.height..];
    for row in 0..layout.height / 2 {
        uv[row * layout.uv_stride..][..layout.width]
            .copy_from_slice(&chroma[row * layout.width..][..layout.width]);
    }
    Ok(())
}
pub(super) fn video_time(
    index: u64,
    numerator: u32,
    denominator: u32,
) -> Result<(i64, i32), RecordingError> {
    let timescale = i32::try_from(numerator)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            RecordingError::invalid("recording video timescale exceeds CoreMedia range")
        })?;
    if denominator == 0 {
        return Err(RecordingError::invalid(
            "recording video frame denominator is zero",
        ));
    }
    let value = i64::try_from(u128::from(index) * u128::from(denominator)).map_err(|_| {
        RecordingError::invalid("recording video timestamp exceeds CoreMedia range")
    })?;
    Ok((value, timescale))
}
pub(super) fn audio_time(frame: u64, rate: u32) -> Result<(i64, i32), RecordingError> {
    let scale = i32::try_from(rate)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            RecordingError::invalid("recording audio timescale exceeds CoreMedia range")
        })?;
    Ok((
        i64::try_from(frame).map_err(|_| {
            RecordingError::invalid("recording audio timestamp exceeds CoreMedia range")
        })?,
        scale,
    ))
}
pub(super) fn later(a: (i64, i32), b: (i64, i32)) -> (i64, i32) {
    if i128::from(a.0) * i128::from(b.1) >= i128::from(b.0) * i128::from(a.1) {
        a
    } else {
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rational_pts_preserves_ntsc_sample_grid_and_rejects_overflow() {
        assert_eq!(video_time(0, 30000, 1001).unwrap(), (0, 30000));
        assert_eq!(video_time(1, 30000, 1001).unwrap(), (1001, 30000));
        assert_eq!(video_time(30000, 30000, 1001).unwrap(), (30_030_000, 30000));
        assert_eq!(audio_time(48000, 48000).unwrap(), (48000, 48000));
        assert_eq!(later((1001, 30000), (1024, 48000)), (1001, 30000));
        assert_eq!(later((1, 1), (48000, 48000)), (1, 1));
        assert!(video_time(u64::MAX, 30000, 1001).is_err());
        assert!(video_time(1, u32::MAX, 1).is_err());
        assert!(video_time(1, 30, 0).is_err());
        assert!(audio_time(u64::MAX, 48000).is_err());
        assert!(audio_time(1, 0).is_err());
    }
    #[test]
    fn planar_copy_respects_independent_native_padding_with_literal_bytes() {
        let layout = plane_layout(4, 2, 8, 8, 24).unwrap();
        let mut y = vec![99; 16];
        let mut uv = vec![99; 8];
        copy_planes(
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            &mut y,
            &mut uv,
            layout,
        )
        .unwrap();
        assert_eq!(y, [1, 2, 3, 4, 0, 0, 0, 0, 5, 6, 7, 8, 0, 0, 0, 0]);
        assert_eq!(uv, [9, 10, 11, 12, 0, 0, 0, 0]);
        let old = y.clone();
        assert!(copy_planes(&[0; 11], &mut y, &mut uv, layout).is_err());
        assert_eq!(y, old);
    }
    #[test]
    fn malformed_native_planes_reject_without_large_allocations() {
        assert!(plane_layout(3840, 2160, 3840, 3840, 12_441_600).is_ok());
        for (width, height, y, uv, bytes) in [
            (0, 2, 8, 8, 24),
            (3, 2, 8, 8, 24),
            (4, 3, 8, 8, 32),
            (4, 2, 3, 8, 24),
            (4, 2, 8, 3, 24),
            (4, 2, usize::MAX, 8, 24),
            (4096, 4096, 4096, 4096, 32 * 1024 * 1024),
            (4, 2, 8, 8, 23),
            (4, 2, 8, 8, MAX_BUFFER_BYTES + 1),
        ] {
            assert!(plane_layout(width, height, y, uv, bytes).is_err());
        }
    }
}
