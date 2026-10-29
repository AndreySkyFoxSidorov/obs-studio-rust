// Frozen original admission and mixed-chain evaluator.
use super::baseline_color::Prepared;
use crate::{
    ColorCorrectionInput, CraftVideoLimits, MediaError, RgbaFrame, SourceBlend, SourceEffectFrame,
    SourceEffectRequirements, SourceEffectsLimits, SourceVideoEffect,
};
pub fn source_effect_requirements(
    input: &RgbaFrame,
    effects: &[SourceVideoEffect],
) -> Result<SourceEffectRequirements, MediaError> {
    if effects.len() > 8 {
        return Err(MediaError::BudgetExceeded);
    }
    let mut chroma = 0u64;
    for effect in effects {
        match effect {
            SourceVideoEffect::CropPad(_) | SourceVideoEffect::Vignette(_) | SourceVideoEffect::GaussianBlur(_)=>return Err(MediaError::InvalidBuffer),
            SourceVideoEffect::LumaKey(_) | SourceVideoEffect::Sharpen(_)=>return Err(MediaError::InvalidBuffer),
            SourceVideoEffect::Craft(e) => e.validate()?,
            SourceVideoEffect::ColorCorrection(e) => e.validate()?,
            SourceVideoEffect::ColorKey(e) => e.validate()?,
            SourceVideoEffect::ChromaKey(e) => {
                e.validate()?;
                chroma += 1;
            }
        }
    }
    let pixels = input.byte_len() / 4;
    Ok(SourceEffectRequirements {
        output_bytes: input.byte_len(),
        max_frame_bytes:input.byte_len(),final_width:input.width(),final_height:input.height(),
        workspace_bytes: if chroma == 0 {
            0
        } else {
            (input.width() as usize)
                .checked_mul(3 * 16)
                .ok_or(MediaError::Overflow)?
        },
        pixel_stages: pixels
            .checked_mul(effects.len())
            .ok_or(MediaError::Overflow)?,
        texel_visits: (pixels as u64)
            .checked_mul(chroma)
            .and_then(|v| v.checked_mul(9))
            .ok_or(MediaError::Overflow)?,
    })
}
pub fn source_video_effects(
    input: &RgbaFrame,
    ingress: SourceBlend,
    effects: &[SourceVideoEffect],
    limits: impl Into<SourceEffectsLimits>,
) -> Result<SourceEffectFrame, MediaError> {
    let limits = limits.into();
    let required = source_effect_requirements(input, effects)?;
    if required.output_bytes > limits.max_output_bytes
        || required.workspace_bytes > limits.max_workspace_bytes
        || required.pixel_stages > limits.max_pixel_stages
        || required.texel_visits > limits.max_texel_visits
    {
        return Err(MediaError::BudgetExceeded);
    }
    if effects
        .iter()
        .all(|e| matches!(e, SourceVideoEffect::Craft(_)))
        && ingress != SourceBlend::PremultipliedLinearSrgb
    {
        let craft: Vec<_> = effects
            .iter()
            .filter_map(|e| {
                if let SourceVideoEffect::Craft(e) = e {
                    Some(*e)
                } else {
                    None
                }
            })
            .collect();
        return Ok(SourceEffectFrame {
            image: crate::craft_video_effects(
                input,
                &craft,
                CraftVideoLimits {
                    max_output_bytes: limits.max_output_bytes,
                    max_pixel_stages: limits.max_pixel_stages,
                },
            )?,
            blend: ingress,
        });
    }
    let mut output = input.pixels().to_vec();
    let mut workspace = vec![[0.0; 4]; required.workspace_bytes / 16];
    let mut mode = ingress;
    let mut begin = 0;
    for (index, effect) in effects.iter().enumerate() {
        if let SourceVideoEffect::ChromaKey(settings) = effect {
            mode = point_segment(&mut output, mode, &effects[begin..index])?;
            super::baseline_keyer::chroma_in_place(
                &mut output,
                input.width() as usize,
                input.height() as usize,
                encoding(mode),
                super::baseline_keyer::PreparedKeyer::chroma(*settings),
                &mut workspace,
            );
            mode = SourceBlend::PremultipliedLinearSrgb;
            begin = index + 1;
        }
    }
    mode = point_segment(&mut output, mode, &effects[begin..])?;
    Ok(SourceEffectFrame {
        image: RgbaFrame::new(input.width(), input.height(), output)?,
        blend: mode,
    })
}
fn encoding(mode: SourceBlend) -> ColorCorrectionInput {
    match mode {
        SourceBlend::LinearSrgb => ColorCorrectionInput::LinearAlpha,
        SourceBlend::Encoded | SourceBlend::EncodedPremultipliedSrgb => {
            ColorCorrectionInput::EncodedAlpha
        }
        SourceBlend::PremultipliedLinearSrgb => ColorCorrectionInput::PremultipliedLinearSrgb,
    }
}
fn point_segment(
    output: &mut [u8],
    ingress: SourceBlend,
    effects: &[SourceVideoEffect],
) -> Result<SourceBlend, MediaError> {
    if effects.is_empty() {
        return Ok(ingress);
    }
    let prepared: [Option<Prepared>; 8] = std::array::from_fn(|i| match effects.get(i) {
        Some(SourceVideoEffect::ColorCorrection(s)) => Some(Prepared::new(*s)),
        _ => None,
    });
    let keyers: [Option<super::baseline_keyer::PreparedKeyer>; 8] =
        std::array::from_fn(|i| match effects.get(i) {
            Some(SourceVideoEffect::ColorKey(s)) => {
                Some(super::baseline_keyer::PreparedKeyer::color(*s))
            }
            _ => None,
        });
    for pixel in output.chunks_exact_mut(4) {
        let mut bytes = [pixel[0], pixel[1], pixel[2], pixel[3]];
        let mut c = [0.0; 3];
        let mut alpha = 0.0;
        let mut straight = false;
        let mut mode = ingress;
        for (index, effect) in effects.iter().enumerate() {
            match effect {
            SourceVideoEffect::CropPad(_) | SourceVideoEffect::Vignette(_) | SourceVideoEffect::GaussianBlur(_)=>return Err(MediaError::InvalidBuffer),
                SourceVideoEffect::Craft(effect) => {
                    if !straight {
                        alpha = f32::from(bytes[3]) / 255.0;
                        c = std::array::from_fn(|i| {
                            if mode == SourceBlend::PremultipliedLinearSrgb {
                                if alpha > 0.0 {
                                    encode_extended(decode(f32::from(bytes[i]) / 255.0) / alpha)
                                } else {
                                    0.0
                                }
                            } else {
                                f32::from(bytes[i]) / 255.0
                            }
                        });
                        straight = true;
                    }
                    (c, alpha) = crate::craft_video::process_craft_pixel(c, alpha, effect);
                }
                SourceVideoEffect::ColorCorrection(_) | SourceVideoEffect::ColorKey(_) => {
                    if straight {
                        bytes = flush(c, alpha, mode);
                        straight = false;
                    }
                    let encoding = match mode {
                        SourceBlend::LinearSrgb => ColorCorrectionInput::LinearAlpha,
                        SourceBlend::Encoded | SourceBlend::EncodedPremultipliedSrgb => {
                            ColorCorrectionInput::EncodedAlpha
                        }
                        SourceBlend::PremultipliedLinearSrgb => {
                            ColorCorrectionInput::PremultipliedLinearSrgb
                        }
                    };
                    bytes = if let Some(keyer) = keyers[index] {
                        keyer.pixel(bytes, encoding)
                    } else {
                        prepared[index]
                            .as_ref()
                            .ok_or(MediaError::InvalidBuffer)?
                            .pixel(bytes, encoding)
                    };
                    mode = SourceBlend::PremultipliedLinearSrgb;
                }
                SourceVideoEffect::ChromaKey(_) | SourceVideoEffect::LumaKey(_) | SourceVideoEffect::Sharpen(_) => return Err(MediaError::InvalidBuffer),
            }
        }
        if straight {
            bytes = flush(c, alpha, mode);
        }
        pixel.copy_from_slice(&bytes);
    }
    Ok(
        if effects.iter().any(|e| {
            matches!(
                e,
                SourceVideoEffect::ColorCorrection(_) | SourceVideoEffect::ColorKey(_)
            )
        }) {
            SourceBlend::PremultipliedLinearSrgb
        } else {
            ingress
        },
    )
}

fn flush(c: [f32; 3], alpha: f32, mode: SourceBlend) -> [u8; 4] {
    let mut result = [0; 4];
    if mode == SourceBlend::PremultipliedLinearSrgb {
        for i in 0..3 {
            result[i] = unorm(encode_extended((decode(c[i]) * alpha).clamp(0.0, 1.0)));
        }
    } else if alpha != 0.0 {
        for i in 0..3 {
            result[i] = unorm(c[i]);
        }
    }
    result[3] = unorm(alpha);
    result
}
fn decode(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn encode_extended(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}
fn unorm(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}
