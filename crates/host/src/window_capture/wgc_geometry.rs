//! Integer physical-pixel geometry derived from OBS winrt-capture.cpp:39-77.
//! OBS Studio GPL-2.0-or-later; see doc/migration/display-capture-methods.md for revision/attribution.
use super::WindowCaptureError as Error;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
pub(crate) fn client_region(
    texture: (u32, u32),
    client: (i64, i64, u32, u32),
    frame_origin: (i64, i64),
) -> Result<Region, Error> {
    let x = client
        .0
        .checked_sub(frame_origin.0)
        .ok_or(Error::InvalidFrame("client offset overflow"))?
        .max(0);
    let y = client
        .1
        .checked_sub(frame_origin.1)
        .ok_or(Error::InvalidFrame("client offset overflow"))?
        .max(0);
    let x = u32::try_from(x).map_err(|_| Error::InvalidFrame("client x overflow"))?;
    let y = u32::try_from(y).map_err(|_| Error::InvalidFrame("client y overflow"))?;
    let width = texture.0.saturating_sub(x).min(client.2);
    let height = texture.1.saturating_sub(y).min(client.3);
    if width == 0 || height == 0 {
        return Err(Error::InvalidFrame("empty or off-texture client crop"));
    }
    Ok(Region {
        x,
        y,
        width,
        height,
    })
}
/// Three conservative logical backings: two SDK pool textures and one staging.
/// RowPitch is unknown until Map; this is not a physical allocation guarantee.
pub(crate) fn backing_bytes(width: u32, height: u32) -> Result<usize, Error> {
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > 33_554_432
    {
        return Err(Error::InvalidFrame("WGC dimensions exceed capture profile"));
    }
    (width as usize)
        .checked_mul(4)
        .and_then(|n| n.checked_add(255))
        .map(|n| n & !255)
        .and_then(|n| n.checked_mul(height as usize))
        .and_then(|n| n.checked_mul(3))
        .ok_or(Error::BudgetExceeded("WGC backing arithmetic"))
}
pub(crate) fn mapped_extent(
    width: u32,
    height: u32,
    pitch: usize,
    limit: usize,
) -> Result<usize, Error> {
    if pitch
        < (width as usize)
            .checked_mul(4)
            .ok_or(Error::InvalidFrame("row overflow"))?
    {
        return Err(Error::InvalidFrame("WGC short mapped row"));
    }
    let n = pitch
        .checked_mul(height as usize)
        .ok_or(Error::BudgetExceeded("WGC mapped extent overflow"))?;
    if n > limit || n > isize::MAX as usize {
        return Err(Error::BudgetExceeded("WGC mapped read budget"));
    }
    Ok(n)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negative_monitor_origin_and_dwm_client_offsets_are_physical_pixels() {
        assert_eq!(
            client_region((800, 600), (-1912, -70, 784, 562), (-1920, -100)).unwrap(),
            Region {
                x: 8,
                y: 30,
                width: 784,
                height: 562
            }
        );
        assert_eq!(
            client_region((100, 80), (12, 8, 120, 100), (0, 0)).unwrap(),
            Region {
                x: 12,
                y: 8,
                width: 88,
                height: 72
            }
        );
        assert!(client_region((100, 80), (100, 0, 1, 1), (0, 0)).is_err());
        assert!(client_region((1, 1), (i64::MAX, 0, 1, 1), (i64::MIN, 0)).is_err());
    }
    #[test]
    fn large_capture_math_and_unknown_pitch_are_independently_checked() {
        assert_eq!(backing_bytes(5120, 2880).unwrap(), 176_947_200);
        assert_eq!(backing_bytes(6016, 3384).unwrap(), 244_297_728);
        assert_eq!(backing_bytes(7680, 4320).unwrap(), 398_131_200);
        assert!(backing_bytes(8192, 4097).is_err());
        assert_eq!(mapped_extent(2, 2, 256, 512).unwrap(), 512);
        assert!(mapped_extent(2, 2, 256, 511).is_err());
        assert!(mapped_extent(2, 2, 7, 512).is_err());
    }
}
