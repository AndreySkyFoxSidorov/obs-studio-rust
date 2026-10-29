//! Bounded GIF metadata scan followed by explicit OBS canvas composition.
//! OBS timing: libobs/graphics/image-file.c:get_time/calculate_new_frame;
//! loop counts: libobs/graphics/libnsgif/gif.c application-extension parser.
use super::{
    AssetError, DecodeBudget, MAX_ANIMATION_BYTES, MAX_ANIMATION_FRAMES, dimensions, malformed,
    sub_blocks, take,
};
use ::gif::{ColorOutput, DecodeOptions, DecodingError, DisposalMethod, MemoryLimit};
use skylive_media::{AnimationClip, AnimationFrame, AnimationLimits, AnimationLoop, RgbaFrame};
use std::{io::Cursor, num::NonZeroU64, sync::Arc};

struct Plan {
    width: u32,
    height: u32,
    durations: Vec<u64>,
    loops: AnimationLoop,
    decoded_bytes: usize,
    background: [u8; 4],
}

fn loop_policy(raw: u16) -> AnimationLoop {
    let cycles = u32::from(raw) + 1;
    if raw == 0 || cycles >= 0xffff {
        AnimationLoop::Infinite
    } else {
        AnimationLoop::Finite(cycles)
    }
}

fn scan(
    bytes: &[u8],
    expected: Option<(u32, u32)>,
    max_decoded_bytes: usize,
) -> Result<Plan, AssetError> {
    let mut position = 0;
    let header = take(bytes, &mut position, 13)?;
    if &header[..6] != b"GIF87a" && &header[..6] != b"GIF89a" {
        return Err(malformed());
    }
    let width = u32::from(u16::from_le_bytes([header[6], header[7]]));
    let height = u32::from(u16::from_le_bytes([header[8], header[9]]));
    dimensions(width, height)?;
    if expected.is_some_and(|value| value != (width, height)) {
        return Err(AssetError::rejected(
            "image_dimensions_changed",
            "GIF logical-screen dimensions disagree with the recorded asset before frame decoding",
        ));
    }
    let frame_bytes = u64::from(width) * u64::from(height) * 4;
    let admitted_bytes = max_decoded_bytes.min(MAX_ANIMATION_BYTES) as u64;
    let mut background = [0, 0, 0, 255];
    if header[10] & 0x80 != 0 {
        let palette = take(bytes, &mut position, 3 * (1usize << ((header[10] & 7) + 1)))?;
        let index = if usize::from(header[11]) < palette.len() / 3 {
            usize::from(header[11])
        } else {
            0
        };
        background[..3].copy_from_slice(&palette[index * 3..index * 3 + 3]);
    }
    let mut durations = Vec::new();
    let mut duration = 100_000_000u64;
    let mut loops = AnimationLoop::Finite(1);
    loop {
        match take(bytes, &mut position, 1)?[0] {
            0x2c => {
                let descriptor = take(bytes, &mut position, 9)?;
                let left = u32::from(u16::from_le_bytes([descriptor[0], descriptor[1]]));
                let top = u32::from(u16::from_le_bytes([descriptor[2], descriptor[3]]));
                let frame_w = u32::from(u16::from_le_bytes([descriptor[4], descriptor[5]]));
                let frame_h = u32::from(u16::from_le_bytes([descriptor[6], descriptor[7]]));
                dimensions(frame_w, frame_h)?;
                if left + frame_w > width || top + frame_h > height {
                    return Err(AssetError::rejected(
                        "image_decode",
                        format!(
                            "GIF frame at {left},{top} with dimensions {frame_w}x{frame_h} exceeds logical screen {width}x{height}"
                        ),
                    ));
                }
                let count = durations.len() + 1;
                if count > MAX_ANIMATION_FRAMES {
                    return Err(AssetError::limit(
                        "image_limit",
                        "GIF clip exceeds 512 frames before frame iteration",
                    ));
                }
                if frame_bytes.checked_mul(count as u64).is_none_or(|bytes| bytes > admitted_bytes) {
                    return Err(AssetError::decoded_capacity(
                        "GIF clip exceeds the admitted decoded-byte budget (maximum 128 MiB) before frame iteration",
                    ));
                }
                durations.push(duration);
                duration = 100_000_000;
                if descriptor[8] & 0x80 != 0 {
                    take(
                        bytes,
                        &mut position,
                        3 * (1usize << ((descriptor[8] & 7) + 1)),
                    )?;
                }
                take(bytes, &mut position, 1)?;
                sub_blocks(bytes, &mut position)?;
            }
            0x21 => match take(bytes, &mut position, 1)?[0] {
                0xf9 => {
                    if take(bytes, &mut position, 1)?[0] != 4 {
                        return Err(malformed());
                    }
                    let control = take(bytes, &mut position, 4)?;
                    if control[0] >> 5 != 0 || ((control[0] >> 2) & 7) > 3 {
                        return Err(malformed());
                    }
                    let delay = u64::from(u16::from_le_bytes([control[1], control[2]]));
                    duration = if delay == 0 {
                        100_000_000
                    } else {
                        delay * 10_000_000
                    };
                    if take(bytes, &mut position, 1)?[0] != 0 {
                        return Err(malformed());
                    }
                }
                0xff => {
                    let count = usize::from(take(bytes, &mut position, 1)?[0]);
                    let identifier = take(bytes, &mut position, count)?;
                    if identifier == b"NETSCAPE2.0" || identifier == b"ANIMEXTS1.0" {
                        if take(bytes, &mut position, 1)?[0] != 3 {
                            return Err(malformed());
                        }
                        let data = take(bytes, &mut position, 3)?;
                        if data[0] != 1 {
                            return Err(malformed());
                        }
                        loops = loop_policy(u16::from_le_bytes([data[1], data[2]]));
                    }
                    sub_blocks(bytes, &mut position)?;
                }
                0x01 => {
                    return Err(AssetError::rejected(
                        "image_format",
                        "GIF plain-text graphic blocks are unsupported",
                    ));
                }
                _ => sub_blocks(bytes, &mut position)?,
            },
            0x3b if !durations.is_empty() && position == bytes.len() => {
                let decoded_bytes = (frame_bytes * durations.len() as u64) as usize;
                return Ok(Plan {
                    width,
                    height,
                    durations,
                    loops,
                    decoded_bytes,
                    background,
                });
            }
            _ => return Err(malformed()),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) fn header(width: u16, height: u16, repeats: Option<u16>) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&[0x81, 0, 0]);
        // Global palette: black/transparent index zero, red, green, blue.
        bytes.extend_from_slice(&[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]);
        if let Some(repeats) = repeats {
            bytes.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01");
            bytes.extend_from_slice(&repeats.to_le_bytes());
            bytes.push(0);
        }
        bytes
    }

    pub(crate) fn frame(
        bytes: &mut Vec<u8>,
        rectangle: (u16, u16, u16, u16),
        indices: &[u8],
        control: Option<(u16, u8, bool)>,
        local: bool,
    ) {
        if let Some((delay, disposal, transparent)) = control {
            bytes.extend_from_slice(&[0x21, 0xf9, 4, (disposal << 2) | u8::from(transparent)]);
            bytes.extend_from_slice(&delay.to_le_bytes());
            bytes.extend_from_slice(&[0, 0]);
        }
        bytes.push(0x2c);
        for value in [rectangle.0, rectangle.1, rectangle.2, rectangle.3] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.push(if local { 0x80 } else { 0 });
        if local {
            bytes.extend_from_slice(&[0, 0, 0, 255, 255, 0]);
        }
        bytes.push(2); // Fixed 3-bit codes; clearing per pixel avoids code growth.
        let mut data = Vec::new();
        let (mut accumulator, mut bits) = (0u32, 0u32);
        for code in indices.iter().flat_map(|index| [4u8, *index]).chain([5]) {
            accumulator |= u32::from(code) << bits;
            bits += 3;
            while bits >= 8 {
                data.push(accumulator as u8);
                accumulator >>= 8;
                bits -= 8;
            }
        }
        if bits > 0 {
            data.push(accumulator as u8);
        }
        for block in data.chunks(255) {
            bytes.push(block.len() as u8);
            bytes.extend_from_slice(block);
        }
        bytes.push(0);
    }

    pub(crate) fn two_frames(repeats: Option<u16>) -> Vec<u8> {
        let mut bytes = header(1, 1, repeats);
        frame(&mut bytes, (0, 0, 1, 1), &[1], Some((0, 1, false)), false);
        frame(&mut bytes, (0, 0, 1, 1), &[2], Some((5, 1, false)), false);
        bytes.push(0x3b);
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{frame, header, two_frames};
    use super::*;
    use skylive_media::MediaTime;

    fn decode_clip(
        bytes: &[u8],
        expected: Option<(u32, u32)>,
        max_bytes: usize,
    ) -> Result<Arc<AnimationClip>, AssetError> {
        super::decode_clip(
            bytes,
            expected,
            max_bytes,
            &mut DecodeBudget::new(max_bytes),
        )
    }

    #[test]
    fn real_frames_zero_delay_and_obs_repeat_counts_have_independent_pixel_oracles() {
        let clip = decode_clip(&two_frames(Some(1)), None, 128).unwrap();
        assert_eq!(
            (
                clip.width(),
                clip.height(),
                clip.frame_count(),
                clip.byte_len()
            ),
            (1, 1, 2, 8)
        );
        assert!(clip.is_animated());
        for (time, pixel) in [
            (0, [255, 0, 0, 255]),
            (100_000_000, [255, 0, 0, 255]),
            (100_000_001, [0, 255, 0, 255]),
            (150_000_000, [0, 255, 0, 255]),
            (150_000_001, [255, 0, 0, 255]),
            (300_000_001, [0, 255, 0, 255]),
        ] {
            assert_eq!(clip.sample(MediaTime(time)).image.pixels(), &pixel);
        }
        let once = decode_clip(&two_frames(None), None, 128).unwrap();
        assert_eq!(
            once.sample(MediaTime(150_000_001)).image.pixels(),
            &[0, 255, 0, 255]
        );
        for raw in [0, 65534, 65535] {
            let clip = decode_clip(&two_frames(Some(raw)), None, 128).unwrap();
            assert_eq!(
                clip.sample(MediaTime(65_536 * 150_000_000 + 1))
                    .image
                    .pixels(),
                &[255, 0, 0, 255]
            );
        }
    }

    #[test]
    fn absent_control_delay_defaults_and_one_frame_loop_has_no_visible_animation() {
        let mut bytes = header(1, 1, Some(0));
        frame(&mut bytes, (0, 0, 1, 1), &[1], None, false);
        bytes.push(0x3b);
        assert_eq!(scan(&bytes, None, 4).unwrap().durations, [100_000_000]);
        let clip = decode_clip(&bytes, None, 4).unwrap();
        assert!(!clip.is_animated());
        assert_eq!(
            clip.sample(MediaTime(u64::MAX)).image.pixels(),
            &[255, 0, 0, 255]
        );
    }

    #[test]
    fn disposal_previous_background_transparency_and_local_palettes_are_composited() {
        for disposal in [2, 3] {
            let mut bytes = header(2, 1, None);
            frame(
                &mut bytes,
                (0, 0, 2, 1),
                &[1, 1],
                Some((1, 1, false)),
                false,
            );
            frame(
                &mut bytes,
                (0, 0, 1, 1),
                &[2],
                Some((1, disposal, false)),
                false,
            );
            frame(&mut bytes, (1, 0, 1, 1), &[3], Some((1, 1, false)), false);
            bytes.push(0x3b);
            let clip = decode_clip(&bytes, None, 24).unwrap();
            assert_eq!(
                clip.sample(MediaTime(10_000_001)).image.pixels(),
                &[0, 255, 0, 255, 255, 0, 0, 255]
            );
            let expected = if disposal == 2 {
                [0, 0, 0, 255, 0, 0, 255, 255]
            } else {
                [255, 0, 0, 255, 0, 0, 255, 255]
            };
            assert_eq!(clip.sample(MediaTime(20_000_001)).image.pixels(), &expected);
        }
        let mut bytes = header(2, 1, None);
        frame(
            &mut bytes,
            (0, 0, 2, 1),
            &[1, 1],
            Some((1, 1, false)),
            false,
        );
        frame(&mut bytes, (0, 0, 2, 1), &[0, 3], Some((1, 1, true)), false);
        frame(&mut bytes, (0, 0, 1, 1), &[1], Some((1, 1, false)), true);
        bytes.push(0x3b);
        let clip = decode_clip(&bytes, None, 24).unwrap();
        assert_eq!(
            clip.sample(MediaTime(10_000_001)).image.pixels(),
            &[255, 0, 0, 255, 0, 0, 255, 255]
        );
        assert_eq!(
            clip.sample(MediaTime(20_000_001)).image.pixels(),
            &[255, 255, 0, 255, 0, 0, 255, 255]
        );
    }

    #[test]
    fn obs_background_disposal_uses_global_palette_and_gce_transparency_over_lower_layer() {
        use skylive_media::{Affine2, AffineLayer, CompositeLimits, UvRect, composite_affine};
        for transparent in [false, true] {
            let mut bytes = header(2, 1, None);
            bytes[11] = 2; // Opaque green GLOBAL background, not local yellow.
            frame(
                &mut bytes,
                (0, 0, 2, 1),
                &[1, 1],
                Some((1, 1, false)),
                false,
            );
            frame(
                &mut bytes,
                (0, 0, 1, 1),
                &[1],
                Some((1, 2, transparent)),
                true,
            );
            frame(&mut bytes, (1, 0, 1, 1), &[1], Some((1, 1, false)), false);
            bytes.push(0x3b);
            let clip = decode_clip(&bytes, None, 24).unwrap();
            let sample = clip.sample(MediaTime(20_000_001));
            let expected = if transparent {
                [0, 0, 0, 0]
            } else {
                [0, 255, 0, 255]
            };
            assert_eq!(&sample.image.pixels()[..4], &expected);
            // A blue scene layer below the GIF must show through only when the
            // disposed frame's transparency flag requests transparent restore.
            let layer = AffineLayer::new(
                &sample.image,
                Affine2 {
                    xx: 2.0,
                    ..Affine2::IDENTITY
                },
                UvRect::FULL,
                1.0,
            );
            let output =
                composite_affine(2, 1, [0, 0, 255, 255], &[layer], CompositeLimits::default())
                    .unwrap();
            let expected = if transparent {
                [0, 0, 255, 255]
            } else {
                [0, 255, 0, 255]
            };
            assert_eq!(&output.pixels()[..4], &expected);
        }
    }

    #[test]
    fn complete_count_and_canvas_bytes_are_admitted_before_pixel_decode() {
        assert_eq!(
            decode_clip(&two_frames(None), None, 7).unwrap_err().code(),
            "image_limit"
        );
        let mut bytes = header(1, 1, None);
        for _ in 0..513 {
            frame(&mut bytes, (0, 0, 1, 1), &[1], None, false);
        }
        bytes.push(0x3b);
        assert_eq!(
            decode_clip(&bytes, None, MAX_ANIMATION_BYTES)
                .unwrap_err()
                .code(),
            "image_limit"
        );
        let mut bytes = header(8192, 2048, None);
        for _ in 0..3 {
            frame(&mut bytes, (0, 0, 1, 1), &[1], None, false);
        }
        bytes.push(0x3b);
        assert_eq!(
            decode_clip(&bytes, None, MAX_ANIMATION_BYTES)
                .unwrap_err()
                .code(),
            "image_limit"
        );
        assert_eq!(
            decode_clip(&bytes, Some((1, 1)), MAX_ANIMATION_BYTES)
                .unwrap_err()
                .code(),
            "image_dimensions_changed"
        );
    }

    #[test]
    fn truncated_late_frame_keeps_complete_logical_decode_work_charge() {
        let mut bytes = header(1, 1, None);
        frame(&mut bytes, (0, 0, 1, 1), &[1], None, false);
        let last = bytes.len();
        frame(&mut bytes, (0, 0, 1, 1), &[2], None, false);
        bytes[last + 10] = 13; // Structurally present but invalid LZW code size.
        bytes.push(0x3b);
        let mut work = DecodeBudget::new(16);
        for remaining in [8, 0] {
            assert_eq!(
                super::decode_clip(&bytes, None, 16, &mut work)
                    .unwrap_err()
                    .code(),
                "image_decode"
            );
            assert_eq!(work.remaining(), remaining);
        }
        assert_eq!(
            super::decode_clip(&bytes, None, 16, &mut work)
                .unwrap_err()
                .code(),
            "image_decode_budget"
        );
    }
}

pub(super) fn decode_clip(
    bytes: &[u8],
    expected: Option<(u32, u32)>,
    max_decoded_bytes: usize,
    decoded_work: &mut DecodeBudget,
) -> Result<Arc<AnimationClip>, AssetError> {
    let plan = scan(bytes, expected, max_decoded_bytes)?;
    decoded_work.reserve(plan.decoded_bytes)?;
    let frame_bytes = (u64::from(plan.width) * u64::from(plan.height) * 4) as usize;
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    options.set_memory_limit(MemoryLimit::Bytes(
        NonZeroU64::new(frame_bytes as u64).ok_or_else(malformed)?,
    ));
    let mut decoder = options
        .read_info(Cursor::new(bytes))
        .map_err(decode_error)?;
    // OBS starts with a transparent canvas. A current raw rectangle plus our
    // mutable canvas and optional Previous backup use at most three canvases of
    // scratch; retained immutable output frames have a separate exact budget.
    let mut canvas = vec![0; frame_bytes];
    let mut frames = Vec::with_capacity(plan.durations.len());
    let mut actual_bytes = 0usize;
    for &duration_ns in &plan.durations {
        let frame = decoder
            .read_next_frame()
            .map_err(decode_error)?
            .ok_or_else(malformed)?;
        let (left, top, width, height) = (
            u32::from(frame.left),
            u32::from(frame.top),
            u32::from(frame.width),
            u32::from(frame.height),
        );
        if width == 0
            || height == 0
            || left + width > plan.width
            || top + height > plan.height
            || frame.buffer.len() != (u64::from(width) * u64::from(height) * 4) as usize
        {
            return Err(malformed());
        }
        let restore = if frame.dispose == DisposalMethod::Previous {
            Some(canvas.clone())
        } else {
            None
        };
        for y in 0..height as usize {
            for x in 0..width as usize {
                let input = (y * width as usize + x) * 4;
                let output = ((y + top as usize) * plan.width as usize + x + left as usize) * 4;
                let pixel = &frame.buffer[input..input + 4];
                if pixel[3] != 0 {
                    canvas[output..output + 4].copy_from_slice(pixel);
                }
            }
        }
        let image = RgbaFrame::new(plan.width, plan.height, canvas.clone()).map_err(|error| {
            AssetError::caused("image_decode", "invalid composited GIF frame", error)
        })?;
        actual_bytes = actual_bytes
            .checked_add(image.byte_len())
            .ok_or_else(malformed)?;
        if actual_bytes > plan.decoded_bytes {
            return Err(AssetError::limit(
                "image_limit",
                "decoded GIF exceeded its admitted logical-frame byte count",
            ));
        }
        frames.push(AnimationFrame {
            image: Arc::new(image),
            duration_ns,
        });
        match frame.dispose {
            DisposalMethod::Background => {
                // libnsgif restores the GLOBAL opaque background for a frame
                // without GCE transparency; browsers/image's high-level GIF
                // iterator instead always clears alpha, which is not OBS.
                let background = if frame.transparent.is_some() {
                    [0, 0, 0, 0]
                } else {
                    plan.background
                };
                for y in top..top + height {
                    for x in left..left + width {
                        let output = (y as usize * plan.width as usize + x as usize) * 4;
                        canvas[output..output + 4].copy_from_slice(&background);
                    }
                }
            }
            DisposalMethod::Previous => canvas = restore.ok_or_else(malformed)?,
            DisposalMethod::Any | DisposalMethod::Keep => {}
        }
    }
    // Do not allocate an unadmitted extra frame merely to prove EOF. The owned
    // scanner already proved exact descriptor count, trailer and byte bounds.
    if actual_bytes != plan.decoded_bytes {
        return Err(malformed());
    }
    let clip = if frames.len() == 1 {
        AnimationClip::static_frame(frames.remove(0).image)
    } else {
        AnimationClip::new(
            frames,
            plan.loops,
            AnimationLimits {
                max_frames: MAX_ANIMATION_FRAMES,
                max_bytes: max_decoded_bytes.min(MAX_ANIMATION_BYTES),
            },
        )
        .map_err(|error| {
            let kind = if error == skylive_media::MediaError::BudgetExceeded {
                crate::resource_issue::ResourceIssueKind::Limit
            } else {
                crate::resource_issue::ResourceIssueKind::Unavailable
            };
            AssetError::caused("image_decode", "invalid GIF animation clip", error).with_kind(kind)
        })?
    };
    Ok(Arc::new(clip))
}

fn decode_error(error: DecodingError) -> AssetError {
    let limited = matches!(
        &error,
        DecodingError::MemoryLimit | DecodingError::OutOfMemory
    );
    let code = if limited {
        "image_limit"
    } else {
        "image_decode"
    };
    AssetError::caused(code, "GIF frame decoder", error).with_kind(if limited {
        crate::resource_issue::ResourceIssueKind::Limit
    } else {
        crate::resource_issue::ResourceIssueKind::Unavailable
    })
}
