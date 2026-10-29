//! One admitted pixel allocation for mixed source-provider chains. Craft-only
//! calls preserve their historical arithmetic and final straight-byte boundary.
use crate::color_correction::Prepared;
use crate::{
    ChromaKeySettings, ColorCorrectionInput, ColorCorrectionSettings, ColorKeySettings,
    CraftVideoEffect, CraftVideoLimits, MediaError, RgbaFrame, SourceBlend,
};

/// Maximum number of ordered source-effect stages admitted by the media engine.
/// The application model uses the same limit without depending on this crate.
pub const MAX_SOURCE_VIDEO_EFFECTS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SourceVideoEffect {
    CropPad(crate::CropPadSettings),
    Vignette(crate::VignetteSettings),
    GaussianBlur(crate::GaussianBlurSettings),
    LumaKey(crate::LumaKeySettings),
    Sharpen(crate::SharpenSettings),
    Craft(CraftVideoEffect),
    ColorCorrection(ColorCorrectionSettings),
    ColorKey(ColorKeySettings),
    ChromaKey(ChromaKeySettings),
}

pub struct SourceEffectFrame {
    pub image: RgbaFrame,
    pub blend: SourceBlend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceEffectRequirements {
    /// Maximum simultaneously live owned pixel buffers, excluding borrowed input.
    pub output_bytes: usize,
    pub max_frame_bytes: usize,
    pub final_width:u32,
    pub final_height:u32,
    pub workspace_bytes: usize,
    pub pixel_stages: usize,
    pub texel_visits: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct SourceEffectsLimits {
    pub max_output_bytes: usize,
    pub max_workspace_bytes: usize,
    pub max_pixel_stages: usize,
    pub max_texel_visits: u64,
}
impl From<CraftVideoLimits> for SourceEffectsLimits {
    fn from(value: CraftVideoLimits) -> Self {
        Self {
            max_output_bytes: value.max_output_bytes,
            max_workspace_bytes: 0,
            max_pixel_stages: value.max_pixel_stages,
            max_texel_visits: 0,
        }
    }
}
pub fn source_effect_requirements(
    input: &RgbaFrame,
    effects: &[SourceVideoEffect],
) -> Result<SourceEffectRequirements, MediaError> {
    source_effect_requirements_for_dimensions((input.width(),input.height()),effects)
}
/// Dimension-only admission uses the same ordered plan as actual execution.
pub fn source_effect_requirements_for_dimensions(
    input:(u32,u32),effects:&[SourceVideoEffect],
)->Result<SourceEffectRequirements,MediaError>{
    if effects.len()>MAX_SOURCE_VIDEO_EFFECTS || input.0==0 || input.1==0 || input.0>8192 || input.1>8192 {return Err(MediaError::BudgetExceeded);}
    let frame_bytes=|d:(u32,u32)| (d.0 as usize).checked_mul(d.1 as usize).and_then(|v|v.checked_mul(4)).ok_or(MediaError::Overflow);
    let mut dimensions=input;
    let mut output_bytes=frame_bytes(input)?;
    let mut max_frame_bytes=output_bytes;
    let mut workspace_bytes=0usize;
    let mut pixel_stages=0usize;
    let mut texel_visits=0u64;
    for effect in effects {
        let pixels=u64::from(dimensions.0)*u64::from(dimensions.1);
        let mut work=pixels;
        let mut visits=0u64;
        match effect {
            SourceVideoEffect::CropPad(settings)=>{
                let next=settings.dimensions(dimensions)?;
                let bytes=frame_bytes(next)?;
                output_bytes=output_bytes.max(frame_bytes(dimensions)?.checked_add(bytes).ok_or(MediaError::Overflow)?);
                max_frame_bytes=max_frame_bytes.max(bytes);
                dimensions=next;
                work=u64::from(next.0)*u64::from(next.1);visits=work;
            }
            SourceVideoEffect::GaussianBlur(settings)=>{
                let plan=crate::craft_gaussian::plan(dimensions,*settings)?;
                if !settings.is_identity(){
                    let bytes=frame_bytes(plan.dimensions)?;
                    output_bytes=output_bytes.max(frame_bytes(dimensions)?.checked_add(bytes).ok_or(MediaError::Overflow)?);
                    max_frame_bytes=max_frame_bytes.max(bytes);dimensions=plan.dimensions;
                    workspace_bytes=workspace_bytes.max(plan.workspace_bytes);work=plan.pixel_stages as u64;visits=plan.texel_visits;
                }else{work=0;}
            }
            SourceVideoEffect::LumaKey(e)=>e.validate()?,
            SourceVideoEffect::Vignette(e)=>{e.validate()?;if e.is_identity(){work=0;}}
            SourceVideoEffect::Sharpen(e)=>{
                e.validate()?;
                if e.sharpness!=0.0 {
                    visits=pixels.checked_mul(9).and_then(|v|v.checked_add((u64::from(dimensions.1)+2)*u64::from(dimensions.0))).ok_or(MediaError::Overflow)?;
                    workspace_bytes=workspace_bytes.max(dimensions.0 as usize*48);
                }
            }
            SourceVideoEffect::Craft(e)=>e.validate()?,
            SourceVideoEffect::ColorCorrection(e)=>e.validate()?,
            SourceVideoEffect::ColorKey(e)=>e.validate()?,
            SourceVideoEffect::ChromaKey(e)=>{
                e.validate()?;visits=pixels.checked_mul(9).ok_or(MediaError::Overflow)?;
                workspace_bytes=workspace_bytes.max(dimensions.0 as usize*48);
            }
        }
        pixel_stages=pixel_stages.checked_add(usize::try_from(work).map_err(|_|MediaError::Overflow)?).ok_or(MediaError::Overflow)?;
        texel_visits=texel_visits.checked_add(visits).ok_or(MediaError::Overflow)?;
    }
    Ok(SourceEffectRequirements {output_bytes,max_frame_bytes,final_width:dimensions.0,final_height:dimensions.1,workspace_bytes,pixel_stages,texel_visits})
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
    // An empty or neutral-only chain must preserve the complete input, including
    // straight RGB hidden under zero alpha. Going through craft_video_effects
    // with no active stages clears those bytes and makes a neutral setting
    // unexpectedly destructive for later stages that inspect straight RGB.
    if effects.iter().all(|effect|match effect {
        SourceVideoEffect::Craft(value)=>value.is_passthrough(),
        SourceVideoEffect::Vignette(value)=>value.is_identity(),
        SourceVideoEffect::GaussianBlur(value)=>value.is_identity(),
        _=>false,
    }) {return Ok(SourceEffectFrame{image:input.clone(),blend:ingress});}
    if effects
        .iter()
        .all(|e| match e {
            SourceVideoEffect::Craft(_)=>true,
            SourceVideoEffect::Vignette(settings)=>settings.is_identity(),
            SourceVideoEffect::GaussianBlur(settings)=>settings.is_identity(),
            _=>false,
        })
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
    let mut workspace = vec![0u32; required.workspace_bytes / 4];
    let mut mode = ingress;
    let mut width=input.width();let mut height=input.height();
    let mut begin = 0;
    for (index, effect) in effects.iter().enumerate() {
        if let SourceVideoEffect::CropPad(settings)=effect {
            mode=point_segment(&mut output,mode,&effects[begin..index])?;
            let (next,w,h)=crate::obs_crop_pad::apply(&output,width,height,encoding(mode),*settings)?;
            output=next;width=w;height=h;mode=SourceBlend::PremultipliedLinearSrgb;begin=index+1;
        } else if let SourceVideoEffect::GaussianBlur(settings)=effect {
            if settings.is_identity(){continue;}
            mode=point_segment(&mut output,mode,&effects[begin..index])?;
            let (next,w,h)=crate::craft_gaussian::apply(&output,width,height,mode,*settings,&mut workspace)?;
            output=next;width=w;height=h;mode=SourceBlend::PremultipliedLinearSrgb;begin=index+1;
        } else if let SourceVideoEffect::Vignette(settings)=effect {
            // An explicit neutral spatial stage must not insert a new byte
            // rounding boundary inside adjacent floating-point Craft stages.
            if settings.is_identity(){continue;}
            mode=point_segment(&mut output,mode,&effects[begin..index])?;
            mode=crate::craft_vignette::in_place(&mut output,width,height,mode,*settings)?;
            begin=index+1;
        } else if let SourceVideoEffect::ChromaKey(settings) = effect {
            mode = point_segment(&mut output, mode, &effects[begin..index])?;
            crate::obs_keyer::chroma_in_place(
                &mut output,
                width as usize,
                height as usize,
                encoding(mode),
                crate::obs_keyer::PreparedKeyer::chroma(*settings),
                &mut workspace,
            )?;
            mode = SourceBlend::PremultipliedLinearSrgb;
            begin = index + 1;
        } else if let SourceVideoEffect::Sharpen(settings)=effect {
            mode=point_segment(&mut output,mode,&effects[begin..index])?;
            crate::obs_luma_sharpen::sharpen_in_place(&mut output,width as usize,height as usize,encoding(mode),*settings,&mut workspace)?;
            mode=SourceBlend::PremultipliedLinearSrgb;begin=index+1;
        }
    }
    mode = point_segment(&mut output, mode, &effects[begin..])?;
    Ok(SourceEffectFrame {
        image: RgbaFrame::new(width, height, output)?,
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
    let prepared: [Option<Prepared>; MAX_SOURCE_VIDEO_EFFECTS] = std::array::from_fn(|i| match effects.get(i) {
        Some(SourceVideoEffect::ColorCorrection(s)) => Some(Prepared::new(*s)),
        _ => None,
    });
    let keyers: [Option<crate::obs_keyer::PreparedKeyer>; MAX_SOURCE_VIDEO_EFFECTS] =
        std::array::from_fn(|i| match effects.get(i) {
            Some(SourceVideoEffect::ColorKey(s)) => {
                Some(crate::obs_keyer::PreparedKeyer::color(*s))
            }
            _ => None,
        });
    // Initialize once per segment rather than checking the static table for
    // every opaque camera pixel.
    let opaque_key_table = keyers.iter().any(Option::is_some).then(crate::obs_keyer::opaque_color_table);
    let lumas:[Option<crate::obs_luma_sharpen::PreparedLuma>;MAX_SOURCE_VIDEO_EFFECTS]=std::array::from_fn(|i|match effects.get(i){
        Some(SourceVideoEffect::LumaKey(settings))=>Some(crate::obs_luma_sharpen::PreparedLuma::new(*settings)),_=>None,
    });
    // Every stage in this segment is pointwise and its prepared parameters
    // remain fixed for the whole frame. Reuse the immediately preceding
    // result only for identical RGBA input: this preserves all rounding and
    // alpha behavior while avoiding repeated shader work in flat image areas.
    let mut previous: Option<([u8; 4], [u8; 4])> = None;
    for pixel in output.chunks_exact_mut(4) {
        let input = [pixel[0], pixel[1], pixel[2], pixel[3]];
        if let Some((last_input, last_output)) = previous {
            if input == last_input {
                pixel.copy_from_slice(&last_output);
                continue;
            }
        }
        let mut bytes = input;
        let mut c = [0.0; 3];
        let mut alpha = 0.0;
        let mut straight = false;
        let mut mode = ingress;
        for (index, effect) in effects.iter().enumerate() {
            match effect {
                SourceVideoEffect::Vignette(settings) if settings.is_identity()=>{},
                SourceVideoEffect::GaussianBlur(settings) if settings.is_identity()=>{},
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
                SourceVideoEffect::ColorCorrection(_) | SourceVideoEffect::ColorKey(_) | SourceVideoEffect::LumaKey(_) => {
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
                        keyer.pixel_with_opaque_table(bytes, encoding, opaque_key_table.expect("color-key table follows prepared keyer"))
                    } else if let Some(luma)=lumas[index] {
                        luma.pixel(bytes,encoding)
                    } else {
                        prepared[index]
                            .as_ref()
                            .ok_or(MediaError::InvalidBuffer)?
                            .pixel(bytes, encoding)
                    };
                    mode = SourceBlend::PremultipliedLinearSrgb;
                }
                SourceVideoEffect::CropPad(_) | SourceVideoEffect::Vignette(_) | SourceVideoEffect::GaussianBlur(_) | SourceVideoEffect::ChromaKey(_) | SourceVideoEffect::Sharpen(_) => return Err(MediaError::InvalidBuffer),
            }
        }
        if straight {
            bytes = flush(c, alpha, mode);
        }
        pixel.copy_from_slice(&bytes);
        previous = Some((input, bytes));
    }
    Ok(
        if effects.iter().any(|e| {
            matches!(
                e,
                SourceVideoEffect::ColorCorrection(_) | SourceVideoEffect::ColorKey(_) | SourceVideoEffect::LumaKey(_)
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

#[cfg(test)]
#[path = "source_effects_tests.rs"]
mod tests;
