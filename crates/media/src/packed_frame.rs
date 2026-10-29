//! Bounded packed SDR frame ingestion. Native resources, clocks, cursor AND/XOR
//! composition and color-space conversion stay with the capture backend.
use crate::{MediaError, RgbaFrame};

const MAX_CAPTURE_PIXELS: u64 = 33_554_432;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackedPixelFormat {
    Rgba8,
    Bgra8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowOrder {
    TopDown,
    BottomUp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    /// Capture is opaque; the fourth source byte is ignored, including BGRX.
    Opaque,
    Straight,
    /// Copy stored framebuffer RGB/A without unpremultiplication. The caller
    /// must retain its explicit PremultipliedLinearSrgb compositing metadata.
    StoredPremultipliedSrgb,
    /// Encoded RGB was premultiplied by alpha into bytes. Invalid channels above
    /// alpha reject; conversion unpremultiplies with integer nearest rounding.
    Premultiplied,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackedFrameLayout {
    pub width: u32,
    pub height: u32,
    pub stride_bytes: usize,
    pub format: PackedPixelFormat,
    pub row_order: RowOrder,
    pub alpha: AlphaMode,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackedFrameLimits {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_pixel_visits: u64,
}
impl Default for PackedFrameLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 384 * 1024 * 1024,
            max_output_bytes: 128 * 1024 * 1024,
            max_pixel_visits: 2 * MAX_CAPTURE_PIXELS,
        }
    }
}

/// Convert borrowed full padded rows into owned, tightly packed RGBA8.
/// StoredPremultipliedSrgb preserves the caller-tagged framebuffer representation.
/// All layout arithmetic, byte/work budgets and premultiplied data validation
/// complete before output allocation. Extra backing bytes are accepted but
/// count against raw admission. Opaque mode ignores source alpha garbage;
/// straight mode preserves hidden RGB; premultiplied alpha zero produces RGB0.
pub fn ingest_packed_frame(
    bytes: &[u8],
    layout: PackedFrameLayout,
    limits: PackedFrameLimits,
) -> Result<RgbaFrame, MediaError> {
    if layout.width == 0 || layout.height == 0 || layout.width > 8192 || layout.height > 8192 {
        return Err(MediaError::InvalidDimensions);
    }
    let pixels = u64::from(layout.width) * u64::from(layout.height);
    if pixels > MAX_CAPTURE_PIXELS {
        return Err(MediaError::BudgetExceeded);
    }
    let row_bytes = (layout.width as usize)
        .checked_mul(4)
        .ok_or(MediaError::Overflow)?;
    if layout.stride_bytes < row_bytes {
        return Err(MediaError::InvalidBuffer);
    }
    let required = layout
        .stride_bytes
        .checked_mul(layout.height as usize)
        .ok_or(MediaError::Overflow)?;
    let output_bytes = row_bytes
        .checked_mul(layout.height as usize)
        .ok_or(MediaError::Overflow)?;
    let visits = pixels.checked_mul(2).ok_or(MediaError::Overflow)?;
    if bytes.len() < required {
        return Err(MediaError::InvalidBuffer);
    }
    if bytes.len() > limits.max_input_bytes
        || output_bytes > limits.max_output_bytes
        || visits > limits.max_pixel_visits
    {
        return Err(MediaError::BudgetExceeded);
    }
    if layout.alpha == AlphaMode::Premultiplied {
        for row in 0..layout.height as usize {
            let offset = row * layout.stride_bytes;
            for pixel in bytes[offset..offset + row_bytes].chunks_exact(4) {
                if pixel[..3].iter().any(|color| *color > pixel[3]) {
                    return Err(MediaError::InvalidBuffer);
                }
            }
        }
    }
    let mut output = vec![0; output_bytes];
    for row in 0..layout.height {
        let source_row = match layout.row_order {
            RowOrder::TopDown => row,
            RowOrder::BottomUp => layout.height - 1 - row,
        };
        let source_offset = source_row as usize * layout.stride_bytes;
        let dest_offset = row as usize * row_bytes;
        let source = &bytes[source_offset..source_offset + row_bytes];
        let dest = &mut output[dest_offset..dest_offset + row_bytes];
        match (layout.format, layout.alpha) {
            (PackedPixelFormat::Rgba8, AlphaMode::Straight | AlphaMode::StoredPremultipliedSrgb) => {
                dest.copy_from_slice(source);
            }
            (PackedPixelFormat::Bgra8, AlphaMode::Opaque) => {
                for (src, dst) in source.chunks_exact(4).zip(dest.chunks_exact_mut(4)) {
                    dst.copy_from_slice(&[src[2], src[1], src[0], 255]);
                }
            }
            (PackedPixelFormat::Bgra8, AlphaMode::Straight | AlphaMode::StoredPremultipliedSrgb) => {
                for (src, dst) in source.chunks_exact(4).zip(dest.chunks_exact_mut(4)) {
                    dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
                }
            }
            _ => {
                for (source, dest) in source.chunks_exact(4).zip(dest.chunks_exact_mut(4)) {
                    let channels = match layout.format {
                        PackedPixelFormat::Rgba8 => [0, 1, 2],
                        PackedPixelFormat::Bgra8 => [2, 1, 0],
                    };
                    let alpha = match layout.alpha {
                        AlphaMode::Opaque => 255,
                        _ => source[3],
                    };
                    dest[3] = alpha;
                    for (channel, index) in channels.into_iter().enumerate() {
                        dest[channel] = if layout.alpha == AlphaMode::Premultiplied {
                            if alpha == 0 {
                                0
                            } else {
                                ((u32::from(source[index]) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                                    as u8
                            }
                        } else {
                            source[index]
                        };
                    }
                }
            }
        }
    }
    RgbaFrame::new(layout.width, layout.height, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_five_six_eight_k_layouts_fit_resource_math_before_any_pixel_allocation() {
        let limits = PackedFrameLimits::default();
        for (width, height) in [(5120, 2880), (6016, 3384), (7680, 4320), (8192, 4096)] {
            let pixels = u64::from(width) * u64::from(height);
            let frame_bytes = pixels * 4;
            assert!(frame_bytes <= limits.max_output_bytes as u64);
            assert!(
                frame_bytes * 3 <= limits.max_input_bytes as u64,
                "macOS three-buffer native expansion"
            );
            assert!(pixels * 2 <= limits.max_pixel_visits);
            let layout = PackedFrameLayout {
                width,
                height,
                stride_bytes: width as usize * 4,
                format: PackedPixelFormat::Bgra8,
                row_order: RowOrder::TopDown,
                alpha: AlphaMode::Opaque,
            };
            assert_eq!(
                ingest_packed_frame(&[], layout, limits),
                Err(MediaError::InvalidBuffer),
                "accepted dimensions still require complete backing rows"
            );
        }
        assert_eq!(
            (
                limits.max_output_bytes,
                limits.max_input_bytes,
                limits.max_pixel_visits
            ),
            (128 * 1024 * 1024, 384 * 1024 * 1024, 2 * 33_554_432)
        );
    }
    fn layout() -> PackedFrameLayout {
        PackedFrameLayout {
            width: 2,
            height: 2,
            stride_bytes: 12,
            format: PackedPixelFormat::Bgra8,
            row_order: RowOrder::TopDown,
            alpha: AlphaMode::Opaque,
        }
    }
    #[test]
    fn padded_bgra_orientation_and_bgrx_alpha_are_real_byte_oracles() {
        let bytes = [
            0, 0, 255, 0, 0, 255, 0, 17, 99, 98, 97, 96, 255, 0, 0, 255, 255, 255, 255, 1, 95, 94,
            93, 92,
        ];
        let top = ingest_packed_frame(&bytes, layout(), PackedFrameLimits::default()).unwrap();
        assert_eq!(
            top.pixels(),
            &[
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255
            ]
        );
        let bottom = ingest_packed_frame(
            &bytes,
            PackedFrameLayout {
                row_order: RowOrder::BottomUp,
                ..layout()
            },
            PackedFrameLimits::default(),
        )
        .unwrap();
        assert_eq!(
            bottom.pixels(),
            &[
                0, 0, 255, 255, 255, 255, 255, 255, 255, 0, 0, 255, 0, 255, 0, 255
            ]
        );
    }
    #[test]
    fn rgba_straight_preserves_hidden_rgb_and_premultiplied_rounds_without_fringe() {
        let raw = [255, 19, 97, 0, 1, 37, 249, 255];
        let frame = ingest_packed_frame(
            &raw,
            PackedFrameLayout {
                width: 2,
                height: 1,
                stride_bytes: 8,
                format: PackedPixelFormat::Rgba8,
                row_order: RowOrder::TopDown,
                alpha: AlphaMode::Straight,
            },
            PackedFrameLimits::default(),
        )
        .unwrap();
        assert_eq!(frame.pixels(), raw);
        let premul = [32, 64, 128, 128, 0, 0, 0, 0, 1, 1, 1, 2];
        let frame = ingest_packed_frame(
            &premul,
            PackedFrameLayout {
                width: 3,
                height: 1,
                stride_bytes: 12,
                format: PackedPixelFormat::Bgra8,
                row_order: RowOrder::TopDown,
                alpha: AlphaMode::Premultiplied,
            },
            PackedFrameLimits::default(),
        )
        .unwrap();
        assert_eq!(
            frame.pixels(),
            &[255, 128, 64, 128, 0, 0, 0, 0, 128, 128, 128, 2]
        );
        for pixel in [[1, 0, 0, 0], [129, 0, 0, 128]] {
            assert_eq!(
                ingest_packed_frame(
                    &pixel,
                    PackedFrameLayout {
                        width: 1,
                        height: 1,
                        stride_bytes: 4,
                        alpha: AlphaMode::Premultiplied,
                        ..layout()
                    },
                    PackedFrameLimits::default()
                ),
                Err(MediaError::InvalidBuffer)
            );
        }
    }
    #[test]
    fn truncated_full_rows_strides_and_overflow_reject_without_reading_pixels() {
        let bytes = [0; 24];
        assert_eq!(
            ingest_packed_frame(&bytes[..23], layout(), PackedFrameLimits::default()),
            Err(MediaError::InvalidBuffer)
        );
        assert_eq!(
            ingest_packed_frame(
                &bytes,
                PackedFrameLayout {
                    stride_bytes: 7,
                    ..layout()
                },
                PackedFrameLimits::default()
            ),
            Err(MediaError::InvalidBuffer)
        );
        assert_eq!(
            ingest_packed_frame(
                &[],
                PackedFrameLayout {
                    stride_bytes: usize::MAX,
                    ..layout()
                },
                PackedFrameLimits::default()
            ),
            Err(MediaError::Overflow)
        );
        for (width, height) in [(0, 1), (1, 0), (8193, 1), (1, 8193), (u32::MAX, u32::MAX)] {
            assert_eq!(
                ingest_packed_frame(
                    &[],
                    PackedFrameLayout {
                        width,
                        height,
                        ..layout()
                    },
                    PackedFrameLimits::default()
                ),
                Err(MediaError::InvalidDimensions)
            );
        }
        assert_eq!(
            ingest_packed_frame(
                &[],
                PackedFrameLayout {
                    width: 8192,
                    height: 4097,
                    ..layout()
                },
                PackedFrameLimits::default()
            ),
            Err(MediaError::BudgetExceeded)
        );
    }
    #[test]
    fn encoded_padding_output_and_pixel_work_are_independently_bounded() {
        let bytes = [0; 25];
        let exact = PackedFrameLimits {
            max_input_bytes: 24,
            max_output_bytes: 16,
            max_pixel_visits: 8,
        };
        assert!(ingest_packed_frame(&bytes[..24], layout(), exact).is_ok());
        for limits in [
            PackedFrameLimits {
                max_input_bytes: 23,
                ..exact
            },
            PackedFrameLimits {
                max_output_bytes: 15,
                ..exact
            },
            PackedFrameLimits {
                max_pixel_visits: 7,
                ..exact
            },
        ] {
            assert_eq!(
                ingest_packed_frame(&bytes[..24], layout(), limits),
                Err(MediaError::BudgetExceeded)
            );
        }
        assert_eq!(
            ingest_packed_frame(&bytes, layout(), exact),
            Err(MediaError::BudgetExceeded)
        );
        assert!(
            ingest_packed_frame(
                &bytes,
                layout(),
                PackedFrameLimits {
                    max_input_bytes: 25,
                    ..exact
                }
            )
            .is_ok()
        );
    }
}

#[cfg(test)]
mod stored_framebuffer_tests {
    use super::*;
    #[test]
    fn native_framebuffer_swizzle_does_not_unpremultiply_or_drop_zero_alpha_energy() {
        let image = ingest_packed_frame(
            &[127, 5, 0, 128, 10, 20, 30, 0],
            PackedFrameLayout {
                width: 2,
                height: 1,
                stride_bytes: 8,
                format: PackedPixelFormat::Bgra8,
                row_order: RowOrder::TopDown,
                alpha: AlphaMode::StoredPremultipliedSrgb,
            },
            PackedFrameLimits::default(),
        )
        .unwrap();
        assert_eq!(image.pixels(), &[0, 5, 127, 128, 30, 20, 10, 0]);
    }
}
