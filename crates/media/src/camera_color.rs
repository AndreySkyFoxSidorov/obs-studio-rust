//! Borrowed native camera bytes to opaque encoded SDR RGBA. No OS dependencies.
use crate::{MediaError, PackedFrameLimits, RgbaFrame, RowOrder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraMatrix {
    Bt601,
    Bt709,
    Fcc,
    Smpte240M,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraRange {
    Limited,
    Full,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraPixelFormat {
    Bgr24,
    Yuy2 {
        matrix: CameraMatrix,
        range: CameraRange,
        chroma_left: bool,
    },
    Uyvy {
        matrix: CameraMatrix,
        range: CameraRange,
        chroma_left: bool,
    },
    I420 {
        matrix: CameraMatrix,
        range: CameraRange,
        chroma_left: bool,
    },
    Yv12 {
        matrix: CameraMatrix,
        range: CameraRange,
        chroma_left: bool,
    },
    Nv12 {
        matrix: CameraMatrix,
        range: CameraRange,
        chroma_left: bool,
    },
}
#[derive(Clone, Copy, Debug)]
pub struct CameraFrameLayout {
    pub width: u32,
    pub height: u32,
    /// NV12 uses this pitch for Y and UV. I420/YV12 use this pitch for Y,
    /// half this even pitch for each following U/V or V/U plane.
    pub stride_bytes: usize,
    pub row_order: RowOrder,
    pub format: CameraPixelFormat,
}

/// All backing bytes, including padding and extras, count against input limits.
/// YUV is top-down only. Chroma uses linear/clamp sampling; horizontally
/// cosited chroma has x/2 position, centered chroma x/2-1/4. 4:2:0 vertical chroma
/// is centered y/2-1/4. These are the OBS SDR reverse-shader sample coordinates.
pub fn ingest_camera_frame(
    bytes: &[u8],
    layout: CameraFrameLayout,
    limits: PackedFrameLimits,
) -> Result<RgbaFrame, MediaError> {
    let CameraFrameLayout {
        width,
        height,
        stride_bytes: stride,
        row_order,
        format,
    } = layout;
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(MediaError::InvalidDimensions);
    }
    let pixels = u64::from(width) * u64::from(height);
    let (row, rows) = match format {
        CameraPixelFormat::Bgr24 => (width as usize * 3, height as usize),
        CameraPixelFormat::Yuy2 { .. } | CameraPixelFormat::Uyvy { .. } => {
            if width % 2 != 0 || row_order != RowOrder::TopDown {
                return Err(MediaError::InvalidBuffer);
            }
            (width as usize * 2, height as usize)
        }
        CameraPixelFormat::Nv12 { .. } => {
            if width % 2 != 0 || height % 2 != 0 || row_order != RowOrder::TopDown {
                return Err(MediaError::InvalidBuffer);
            }
            (width as usize, height as usize + height as usize / 2)
        }
        CameraPixelFormat::I420 { .. } | CameraPixelFormat::Yv12 { .. } => {
            if width % 2 != 0 || height % 2 != 0 || stride % 2 != 0 || row_order != RowOrder::TopDown {
                return Err(MediaError::InvalidBuffer);
            }
            (width as usize, height as usize + height as usize / 2)
        }
    };
    if stride < row {
        return Err(MediaError::InvalidBuffer);
    }
    let backing = stride.checked_mul(rows).ok_or(MediaError::Overflow)?;
    let output_bytes = (pixels as usize)
        .checked_mul(4)
        .ok_or(MediaError::Overflow)?;
    if bytes.len() < backing {
        return Err(MediaError::InvalidBuffer);
    }
    // Worst case four chroma taps plus one luma read and one output write.
    if pixels > 33_554_432
        || bytes.len() > limits.max_input_bytes
        || output_bytes > limits.max_output_bytes
        || pixels.checked_mul(6).ok_or(MediaError::Overflow)? > limits.max_pixel_visits
    {
        return Err(MediaError::BudgetExceeded);
    }
    if let CameraPixelFormat::I420 { matrix, range, chroma_left } | CameraPixelFormat::Yv12 { matrix, range, chroma_left } = format {
        let y_end = stride * height as usize;
        let chroma_stride = stride / 2;
        let chroma_bytes = chroma_stride * (height as usize / 2);
        let first = &bytes[y_end..y_end + chroma_bytes];
        let second = &bytes[y_end + chroma_bytes..backing];
        let (u, v) = if matches!(format, CameraPixelFormat::Yv12 { .. }) { (second, first) } else { (first, second) };
        return crate::ingest_planar_420(&bytes[..y_end], u, v, crate::Planar420Layout {
            width, height, y_stride: stride, uv_stride: chroma_stride, matrix, range, chroma_left,
        }, limits);
    }
    let mut output = vec![0; output_bytes];
    let coefficients = match format {
        CameraPixelFormat::Bgr24 => None,
        CameraPixelFormat::Yuy2 { matrix, range, .. }
        | CameraPixelFormat::Uyvy { matrix, range, .. }
        | CameraPixelFormat::I420 { matrix, range, .. }
        | CameraPixelFormat::Yv12 { matrix, range, .. }
        | CameraPixelFormat::Nv12 { matrix, range, .. } => Some(Coefficients::new(matrix, range)),
    };
    if let CameraPixelFormat::Nv12 { chroma_left, .. } = format {
        // The vertical samples and interpolation factor depend only on the
        // output row. Preserve the scalar expression order for every pixel.
        let plane = stride * height as usize;
        let coefficients = coefficients.as_ref().unwrap();
        // Height is already validated as even. Each pair shares horizontal
        // coordinates while retaining distinct vertical interpolation weights.
        for y in (0..height as usize).step_by(2) {
            let (c0, d0, ty0) = chroma_axis(y, height as usize / 2, false);
            let (c1, d1, ty1) = chroma_axis(y + 1, height as usize / 2, false);
            for x in 0..width as usize {
                let (a, b, tx) = chroma_axis(x, width as usize / 2, chroma_left);
                let uv_pair = |channel| {
                    let get = |row, col| bytes[plane + row * stride + col * 2 + channel] as f64;
                    let horizontal = |row| get(row, a) + (get(row, b) - get(row, a)) * tx;
                    let top0 = horizontal(c0);
                    let bottom0 = if d0 == c0 { top0 } else { horizontal(d0) };
                    let top1 = if c1 == c0 { top0 } else if c1 == d0 { bottom0 } else { horizontal(c1) };
                    let bottom1 = if d1 == c0 { top0 } else if d1 == d0 { bottom0 } else if d1 == c1 { top1 } else { horizontal(d1) };
                    (top0 + (bottom0 - top0) * ty0, top1 + (bottom1 - top1) * ty1)
                };
                let (u0, u1) = uv_pair(0);
                let (v0, v1) = uv_pair(1);
                let first = coefficients.convert(bytes[y * stride + x] as f64, u0, v0);
                let second = coefficients.convert(bytes[(y + 1) * stride + x] as f64, u1, v1);
                let at0 = (y * width as usize + x) * 4;
                let at1 = ((y + 1) * width as usize + x) * 4;
                output[at0..at0 + 4].copy_from_slice(&[first[0], first[1], first[2], 255]);
                output[at1..at1 + 4].copy_from_slice(&[second[0], second[1], second[2], 255]);
            }
        }
        return RgbaFrame::new(width, height, output);
    }
    for y in 0..height as usize {
        for x in 0..width as usize {
            let rgb = match format {
                CameraPixelFormat::Bgr24 => {
                    let sy = if row_order == RowOrder::TopDown {
                        y
                    } else {
                        height as usize - 1 - y
                    };
                    let i = sy * stride + x * 3;
                    [bytes[i + 2], bytes[i + 1], bytes[i]]
                }
                CameraPixelFormat::Yuy2 { chroma_left, .. } | CameraPixelFormat::Uyvy { chroma_left, .. } => {
                    let uyvy = matches!(format, CameraPixelFormat::Uyvy { .. });
                    let yy = bytes[y * stride + x * 2 + usize::from(uyvy)] as f64;
                    let (a, b, t) = chroma_axis(x, width as usize / 2, chroma_left);
                    let uv = |channel| {
                        let first = bytes[y * stride + a * 4 + channel] as f64;
                        first + (bytes[y * stride + b * 4 + channel] as f64 - first) * t
                    };
                    let u = if uyvy { 0 } else { 1 };
                    coefficients.as_ref().unwrap().convert(yy, uv(u), uv(u + 2))
                }
                CameraPixelFormat::Nv12 { .. } => unreachable!("NV12 was handled by the row-specialized path"),
                CameraPixelFormat::I420 { .. } | CameraPixelFormat::Yv12 { .. } => {
                    unreachable!("planar camera layout was delegated before packed conversion")
                }
            };
            let at = (y * width as usize + x) * 4;
            output[at..at + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    RgbaFrame::new(width, height, output)
}

/// Callers admit positive chroma dimensions and integral luma coordinates.
/// Half and quarter positions are exact in f64 for all admitted dimensions.
pub(crate) fn chroma_axis(pixel: usize, count: usize, left: bool) -> (usize, usize, f64) {
    let last = count - 1;
    if !left && pixel == 0 {
        return (0, 1.min(last), 0.0);
    }
    let half = pixel / 2;
    let (first, fraction) = if left {
        (half, if pixel % 2 == 0 { 0.0 } else { 0.5 })
    } else if pixel % 2 == 0 {
        (half - 1, 0.75)
    } else {
        (half, 0.25)
    };
    if first >= last {
        (last, last, 0.0)
    } else {
        (first, first + 1, fraction)
    }
}
pub(crate) struct Coefficients {
    ys: f64,
    cs: f64,
    y0: f64,
    ymin: f64,
    ymax: f64,
    cmin: f64,
    cmax: f64,
    rv: f64,
    gu: f64,
    gv: f64,
    bu: f64,
}
impl Coefficients {
    pub(crate) fn new(matrix: CameraMatrix, range: CameraRange) -> Self {
        let (kr, kb) = match matrix {
            CameraMatrix::Bt601 => (0.299, 0.114),
            CameraMatrix::Bt709 => (0.2126, 0.0722),
            CameraMatrix::Fcc => (0.30, 0.11),
            CameraMatrix::Smpte240M => (0.212, 0.087),
        };
        let kg = 1.0 - kr - kb;
        let (ys, cs, y0, ymin, ymax, cmin, cmax) = match range {
            CameraRange::Limited => (255.0 / 219.0, 255.0 / 224.0, 16.0, 16.0, 235.0, 16.0, 240.0),
            CameraRange::Full => (1.0, 1.0, 0.0, 0.0, 255.0, 0.0, 255.0),
        };
        Self {
            ys,
            cs,
            y0,
            ymin,
            ymax,
            cmin,
            cmax,
            rv: 2.0 * (1.0 - kr),
            gu: -2.0 * kb * (1.0 - kb) / kg,
            gv: -2.0 * kr * (1.0 - kr) / kg,
            bu: 2.0 * (1.0 - kb),
        }
    }
    pub(crate) fn convert(&self, y: f64, u: f64, v: f64) -> [u8; 3] {
        let y = (y.clamp(self.ymin, self.ymax) - self.y0) * self.ys;
        let u = (u.clamp(self.cmin, self.cmax) - 128.0) * self.cs;
        let v = (v.clamp(self.cmin, self.cmax) - 128.0) * self.cs;
        [
            y + self.rv * v,
            y + self.gu * u + self.gv * v,
            y + self.bu * u,
        ]
        .map(|c| c.clamp(0.0, 255.0).round() as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> PackedFrameLimits {
        PackedFrameLimits {
            max_pixel_visits: 6 * 33_554_432,
            ..Default::default()
        }
    }
    fn decode(bytes: &[u8], w: u32, h: u32, stride: usize, format: CameraPixelFormat) -> RgbaFrame {
        ingest_camera_frame(
            bytes,
            CameraFrameLayout {
                width: w,
                height: h,
                stride_bytes: stride,
                row_order: RowOrder::TopDown,
                format,
            },
            limits(),
        )
        .unwrap()
    }
    #[test]
    fn literal_601_color_bars_and_limited_black_white() {
        let f = CameraPixelFormat::Yuy2 {
            matrix: CameraMatrix::Bt601,
            range: CameraRange::Limited,
            chroma_left: true,
        };
        // Independent studio-range BT.601 color triplets: red81/90/240,
        // green145/54/34, blue41/240/110. Constant chroma removes interpolation.
        for (raw, expected) in [
            ([81, 90, 81, 240], [254, 0, 0, 255]),
            ([145, 54, 145, 34], [0, 255, 1, 255]),
            ([41, 240, 41, 110], [0, 0, 255, 255]),
            ([16, 128, 235, 128], [0, 0, 0, 255]),
        ] {
            let image = decode(&raw, 2, 1, 4, f);
            assert_eq!(&image.pixels()[..4], &expected);
        }
        assert_eq!(
            &decode(&[16, 128, 235, 128], 2, 1, 4, f).pixels()[4..],
            [255, 255, 255, 255]
        );
    }
    #[test]
    fn matrix_and_full_range_are_not_interchangeable() {
        let f = |matrix, range| CameraPixelFormat::Yuy2 {
            matrix,
            range,
            chroma_left: true,
        };
        assert_eq!(
            &decode(
                &[63, 102, 63, 240],
                2,
                1,
                4,
                f(CameraMatrix::Bt709, CameraRange::Limited)
            )
            .pixels()[..4],
            &[255, 1, 0, 255]
        );
        assert_eq!(
            &decode(
                &[76, 85, 76, 255],
                2,
                1,
                4,
                f(CameraMatrix::Bt601, CameraRange::Full)
            )
            .pixels()[..4],
            &[254, 0, 0, 255]
        );
        assert_ne!(
            decode(
                &[81, 90, 81, 240],
                2,
                1,
                4,
                f(CameraMatrix::Bt601, CameraRange::Limited)
            )
            .pixels(),
            decode(
                &[81, 90, 81, 240],
                2,
                1,
                4,
                f(CameraMatrix::Bt709, CameraRange::Limited)
            )
            .pixels()
        );
    }
    #[test]
    fn nv12_planes_padding_and_linear_chroma_have_literal_oracle() {
        let f = CameraPixelFormat::Nv12 {
            matrix: CameraMatrix::Bt601,
            range: CameraRange::Full,
            chroma_left: false,
        };
        // Y=128; U changes128->192, V128, stride6. Centered horizontal
        // samples128,144,176,192 give blue128,156,213,241 independently.
        let raw = [
            128, 128, 128, 128, 9, 9, 128, 128, 128, 128, 9, 9, 128, 128, 192, 128, 9, 9,
        ];
        let image = decode(&raw, 4, 2, 6, f);
        assert_eq!(
            image.pixels(),
            &[
                128, 128, 128, 255, 128, 122, 156, 255, 128, 111, 213, 255, 128, 106, 241, 255,
                128, 128, 128, 255, 128, 122, 156, 255, 128, 111, 213, 255, 128, 106, 241, 255
            ]
        );
    }
    #[test]
    fn bgr24_padding_bottom_up_and_opaque_alpha() {
        let raw = [255, 0, 0, 9, 0, 0, 255, 9];
        let image = ingest_camera_frame(
            &raw,
            CameraFrameLayout {
                width: 1,
                height: 2,
                stride_bytes: 4,
                row_order: RowOrder::BottomUp,
                format: CameraPixelFormat::Bgr24,
            },
            limits(),
        )
        .unwrap();
        assert_eq!(image.pixels(), &[255, 0, 0, 255, 0, 0, 255, 255]);
    }
    #[test]
    fn native_chroma_siting_and_vertical_nv12_sampling_are_distinct() {
        let packed = decode(
            &[128, 128, 128, 128, 128, 192, 128, 128],
            4,
            1,
            8,
            CameraPixelFormat::Yuy2 {
                matrix: CameraMatrix::Bt601,
                range: CameraRange::Full,
                chroma_left: true,
            },
        );
        assert_eq!(
            packed.pixels(),
            &[
                128, 128, 128, 255, 128, 117, 185, 255, 128, 106, 241, 255, 128, 106, 241, 255
            ]
        );
        let frame = decode(
            &[128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 192, 128],
            2,
            4,
            2,
            CameraPixelFormat::Nv12 {
                matrix: CameraMatrix::Bt601,
                range: CameraRange::Full,
                chroma_left: false,
            },
        );
        assert_eq!(
            frame.pixels(),
            &[
                128, 128, 128, 255, 128, 128, 128, 255, 128, 122, 156, 255, 128, 122, 156, 255,
                128, 111, 213, 255, 128, 111, 213, 255, 128, 106, 241, 255, 128, 106, 241, 255
            ]
        );
    }
    #[test]
    fn malformed_planes_even_dimensions_and_work_admission_reject() {
        let f = CameraPixelFormat::Nv12 {
            matrix: CameraMatrix::Bt709,
            range: CameraRange::Limited,
            chroma_left: false,
        };
        let mut layout = CameraFrameLayout {
            width: 2,
            height: 2,
            stride_bytes: 2,
            row_order: RowOrder::TopDown,
            format: f,
        };
        assert!(ingest_camera_frame(&[0; 5], layout, limits()).is_err());
        assert!(
            ingest_camera_frame(
                &[0; 6],
                layout,
                PackedFrameLimits {
                    max_output_bytes: 15,
                    ..limits()
                }
            )
            .is_err()
        );
        assert!(
            ingest_camera_frame(
                &[0; 6],
                layout,
                PackedFrameLimits {
                    max_pixel_visits: 23,
                    ..limits()
                }
            )
            .is_err()
        );
        layout.row_order = RowOrder::BottomUp;
        assert!(ingest_camera_frame(&[0; 6], layout, limits()).is_err());
        layout.row_order = RowOrder::TopDown;
        layout.width = 1;
        assert!(ingest_camera_frame(&[0; 6], layout, limits()).is_err());
    }
}
