//! Pure, budgeted pixel conversion around a host-owned person segmentation model.
//! No model loading, inference, threads, devices or publication happens here.
use crate::color_correction::{decode_table, encode, unorm};
use crate::{MediaError, RgbaFrame, SourceBlend};

pub const PERSON_MATTE_EDGE: usize = 256;
pub const PERSON_MASK_LEN: usize = PERSON_MATTE_EDGE * PERSON_MATTE_EDGE;
pub const PERSON_RGB_LEN: usize = PERSON_MASK_LEN * 3;

/// Per-call admission. Retained tensors, model state and asynchronous queues
/// belong to the host's additional cumulative memory/work accounting.
#[derive(Clone, Copy, Debug)]
pub struct PersonMatteLimits {
    /// Prepare: source RGBA bytes. Apply: source RGBA plus all mask f32 bytes.
    pub max_input_bytes: usize,
    /// Prepare: 196608 f32 values. Apply: the same number of bytes as source RGBA.
    pub max_output_bytes: usize,
    /// Prepare: four source taps per model pixel. Apply: one validation per mask
    /// value plus four mask taps and one source RGBA visit per output pixel.
    pub max_pixel_visits: u64,
}

/// Center-resample into normalized encoded-sRGB RGB NHWC (batch size one).
///
/// Three straight-byte blend modes use RGB/255 directly, including hidden RGB
/// at alpha zero. Premultiplied-linear bytes are decoded, divided by nonzero
/// alpha, clamped and continuously sRGB-encoded; alpha zero yields model RGB0.
/// Interpolation occurs after this conversion, with clamp-to-edge borders.
pub fn prepare_person_rgb(
    input: &RgbaFrame,
    blend: SourceBlend,
    limits: PersonMatteLimits,
) -> Result<Vec<f32>, MediaError> {
    let bytes = PERSON_RGB_LEN
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or(MediaError::Overflow)?;
    admit(
        input.byte_len(),
        bytes,
        (PERSON_MASK_LEN as u64) * 4,
        limits,
    )?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(PERSON_RGB_LEN)
        .map_err(|_| MediaError::BudgetExceeded)?;
    output.resize(PERSON_RGB_LEN, 0.0);
    let width = input.width() as usize;
    let height = input.height() as usize;
    let decoded = match blend {
        SourceBlend::PremultipliedLinearSrgb => Some(decode_table()),
        SourceBlend::Encoded | SourceBlend::LinearSrgb | SourceBlend::EncodedPremultipliedSrgb => {
            None
        }
    };
    let horizontal: [Axis; PERSON_MATTE_EDGE] =
        std::array::from_fn(|x| axis(x, PERSON_MATTE_EDGE, width));
    for y in 0..PERSON_MATTE_EDGE {
        let vertical = axis(y, PERSON_MATTE_EDGE, height);
        for (x, horizontal) in horizontal.iter().enumerate() {
            let a = straight_rgb(input, decoded, horizontal.first, vertical.first);
            let b = straight_rgb(input, decoded, horizontal.second, vertical.first);
            let c = straight_rgb(input, decoded, horizontal.first, vertical.second);
            let d = straight_rgb(input, decoded, horizontal.second, vertical.second);
            let target = (y * PERSON_MATTE_EDGE + x) * 3;
            for channel in 0..3 {
                output[target + channel] = bilinear(
                    a[channel],
                    b[channel],
                    c[channel],
                    d[channel],
                    horizontal.weight,
                    vertical.weight,
                )
                .clamp(0.0, 1.0);
            }
        }
    }
    Ok(output)
}

/// Apply a normalized 256x256 probability mask without changing `SourceBlend`.
///
/// Raw probabilities are center-resampled to source resolution before mapping
/// `clamp((p - threshold) / softness + 0.5, 0, 1)`. Threshold is in [0,1] and
/// softness in [0.001,1]; threshold0.5/softness1 preserves raw probabilities.
/// Straight RGB bytes stay exact. Premultiplied-linear RGB scales in linear
/// light independently of alpha, including existing emission at alpha zero.
pub fn apply_person_matte(
    input: &RgbaFrame,
    blend: SourceBlend,
    mask: &[f32],
    threshold: f32,
    softness: f32,
    limits: PersonMatteLimits,
) -> Result<RgbaFrame, MediaError> {
    if mask.len() != PERSON_MASK_LEN
        || !threshold.is_finite()
        || !(0.0..=1.0).contains(&threshold)
        || !softness.is_finite()
        || !(0.001..=1.0).contains(&softness)
    {
        return Err(MediaError::InvalidBuffer);
    }
    let mask_bytes = mask
        .len()
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or(MediaError::Overflow)?;
    let input_bytes = input
        .byte_len()
        .checked_add(mask_bytes)
        .ok_or(MediaError::Overflow)?;
    let visits = u64::try_from(input.byte_len() / 4)
        .map_err(|_| MediaError::Overflow)?
        .checked_mul(5)
        .and_then(|n| n.checked_add(PERSON_MASK_LEN as u64))
        .ok_or(MediaError::Overflow)?;
    admit(input_bytes, input.byte_len(), visits, limits)?;
    // Validate the complete borrowed mask before creating the full-size output.
    // Runtime numerical tolerance/clamping, if any, belongs to the model port.
    if mask
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
    {
        return Err(MediaError::InvalidBuffer);
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(input.byte_len())
        .map_err(|_| MediaError::BudgetExceeded)?;
    output.extend_from_slice(input.pixels());
    let width = input.width() as usize;
    let height = input.height() as usize;
    let decoded = match blend {
        SourceBlend::PremultipliedLinearSrgb => Some(decode_table()),
        SourceBlend::Encoded | SourceBlend::LinearSrgb | SourceBlend::EncodedPremultipliedSrgb => {
            None
        }
    };
    // A fixed tile keeps axis scratch independent of width (6KiB on64-bit).
    // Each horizontal coefficient uses exactly the original f64 expression.
    let mut horizontal = [Axis {
        first: 0,
        second: 0,
        weight: 0.0,
    }; PERSON_MATTE_EDGE];
    for start in (0..width).step_by(PERSON_MATTE_EDGE) {
        let count = (width - start).min(PERSON_MATTE_EDGE);
        for (x, value) in horizontal[..count].iter_mut().enumerate() {
            *value = axis(start + x, width, PERSON_MATTE_EDGE);
        }
        for y in 0..height {
            let vertical = axis(y, height, PERSON_MATTE_EDGE);
            for (x, horizontal) in horizontal[..count].iter().enumerate() {
                let a = mask[vertical.first * PERSON_MATTE_EDGE + horizontal.first];
                let b = mask[vertical.first * PERSON_MATTE_EDGE + horizontal.second];
                let c = mask[vertical.second * PERSON_MATTE_EDGE + horizontal.first];
                let d = mask[vertical.second * PERSON_MATTE_EDGE + horizontal.second];
                let probability = bilinear(a, b, c, d, horizontal.weight, vertical.weight);
                let matte = ((probability - threshold) / softness + 0.5).clamp(0.0, 1.0);
                let offset = (y * width + start + x) * 4;
                let pixel = &mut output[offset..offset + 4];
                if matte == 1.0 {
                    // Original byte-alpha normalization/requantization is an
                    // identity for all256 values; no transfer write is needed.
                    continue;
                }
                if matte == 0.0 {
                    if decoded.is_some() {
                        pixel.fill(0);
                    } else {
                        pixel[3] = 0;
                    }
                    continue;
                }
                if let Some(decoded) = decoded {
                    for channel in &mut pixel[..3] {
                        *channel = encode(decoded[usize::from(*channel)] * matte);
                    }
                }
                pixel[3] = unorm((f32::from(pixel[3]) / 255.0) * matte);
            }
        }
    }
    RgbaFrame::new(input.width(), input.height(), output)
}

fn admit(
    input_bytes: usize,
    output_bytes: usize,
    visits: u64,
    limits: PersonMatteLimits,
) -> Result<(), MediaError> {
    if input_bytes > limits.max_input_bytes
        || output_bytes > limits.max_output_bytes
        || visits > limits.max_pixel_visits
    {
        Err(MediaError::BudgetExceeded)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Axis {
    first: usize,
    second: usize,
    weight: f32,
}

fn axis(destination: usize, destination_len: usize, source_len: usize) -> Axis {
    let coordinate = ((destination as f64 + 0.5) * source_len as f64 / destination_len as f64
        - 0.5)
        .clamp(0.0, (source_len - 1) as f64);
    let first = coordinate.floor() as usize;
    Axis {
        first,
        second: (first + 1).min(source_len - 1),
        weight: (coordinate - first as f64) as f32,
    }
}

fn bilinear(a: f32, b: f32, c: f32, d: f32, x: f32, y: f32) -> f32 {
    let top = a + (b - a) * x;
    let bottom = c + (d - c) * x;
    top + (bottom - top) * y
}

fn straight_rgb(input: &RgbaFrame, decoded: Option<&[f32; 256]>, x: usize, y: usize) -> [f32; 3] {
    let offset = (y * input.width() as usize + x) * 4;
    let pixel = &input.pixels()[offset..offset + 4];
    match decoded {
        None => std::array::from_fn(|channel| f32::from(pixel[channel]) / 255.0),
        Some(decoded) => {
            let alpha = f32::from(pixel[3]) / 255.0;
            if alpha == 0.0 {
                [0.0; 3]
            } else {
                std::array::from_fn(|channel| {
                    encode_unit(decoded[usize::from(pixel[channel])] / alpha)
                })
            }
        }
    }
}

fn encode_unit(linear: f32) -> f32 {
    let value = linear.clamp(0.0, 1.0);
    if value <= 0.0031308 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
#[path = "person_matte_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "person_matte_exact_tests.rs"]
mod exact_tests;
