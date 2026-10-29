//! OBS32.2.2 scroll-filter.c + crop_filter.effect SDR linear sampler (GPL-2.0+).
//! Explicit normalized offset; no clocks, I/O, worker or mutation of authored state.
use crate::{MediaError,RgbaFrame,SourceBlend,ColorCorrectionInput};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ScrollSettings {pub width:Option<u32>,pub height:Option<u32>,pub looping:bool}
#[derive(Clone,Copy,Debug,Default,PartialEq)]
pub struct ScrollPhase {pub x:f32,pub y:f32}
#[derive(Clone,Copy,Debug)]
pub struct ScrollRequirements {pub width:u32,pub height:u32,pub output_bytes:usize,pub texel_visits:u64}
#[derive(Clone,Copy,Debug)]
pub struct ScrollLimits {pub max_output_bytes:usize,pub max_texel_visits:u64}
pub fn scroll_requirements(input:(u32,u32),settings:ScrollSettings,phase:ScrollPhase)->Result<ScrollRequirements,MediaError>{
    let w=settings.width.unwrap_or(input.0);let h=settings.height.unwrap_or(input.1);
    if input.0==0||input.1==0||input.0>8192||input.1>8192||w==0||h==0||w>8192||h>8192||!phase.x.is_finite()||!phase.y.is_finite(){return Err(MediaError::InvalidBuffer);}
    let pixels=(w as usize).checked_mul(h as usize).filter(|n|*n<=32*1024*1024).ok_or(MediaError::BudgetExceeded)?;
    Ok(ScrollRequirements {width:w,height:h,output_bytes:pixels.checked_mul(4).ok_or(MediaError::Overflow)?,texel_visits:(pixels as u64).checked_mul(4).ok_or(MediaError::Overflow)?})
}
/// Sampling is in the original linear premultiplied target, including fractional
/// transparent border taps. Loop wraps each tap, never a clamped output rectangle.
pub fn scroll_rgba(input:&RgbaFrame,ingress:SourceBlend,settings:ScrollSettings,phase:ScrollPhase,limits:ScrollLimits)->Result<RgbaFrame,MediaError>{
    let r=scroll_requirements((input.width(),input.height()),settings,phase)?;
    if r.output_bytes>limits.max_output_bytes||r.texel_visits>limits.max_texel_visits{return Err(MediaError::BudgetExceeded);}
    let mode=match ingress {SourceBlend::LinearSrgb=>ColorCorrectionInput::LinearAlpha,SourceBlend::Encoded|SourceBlend::EncodedPremultipliedSrgb=>ColorCorrectionInput::EncodedAlpha,SourceBlend::PremultipliedLinearSrgb=>ColorCorrectionInput::PremultipliedLinearSrgb};
    let mut pixels=Vec::new();pixels.try_reserve_exact(r.output_bytes).map_err(|_|MediaError::BudgetExceeded)?;pixels.resize(r.output_bytes,0);
    let width=i64::from(input.width());let height=i64::from(input.height());
    let read=|x:i64,y:i64|->[f32;4]{let(x,y)=if settings.looping{(x.rem_euclid(width),y.rem_euclid(height))}else if x<0||y<0||x>=width||y>=height{return[0.0;4]}else{(x,y)};let at=(y as usize*width as usize+x as usize)*4;let b=&input.pixels()[at..at+4];crate::obs_keyer::sample_lp([b[0],b[1],b[2],b[3]],mode)};
    // Equivalent to uv*output/base + normalized offset at output texel centers.
    // f64 coordinate multiplication avoids i64 saturation for long negative phases;
    // the canonical offset is still the original f32 tick state.
    let mut ox=f64::from(phase.x);let mut oy=f64::from(phase.y);
    if settings.looping{ox=ox.rem_euclid(1.0);oy=oy.rem_euclid(1.0);}
    for y in 0..r.height {let sy=f64::from(y)+oy*height as f64;let y0=sy.floor() as i64;let ty=(sy-y0 as f64) as f32;
        for x in 0..r.width {let sx=f64::from(x)+ox*width as f64;let x0=sx.floor() as i64;let tx=(sx-x0 as f64) as f32;
            let a=read(x0,y0);let b=read(x0+1,y0);let c=read(x0,y0+1);let d=read(x0+1,y0+1);
            let value:[f32;4]=std::array::from_fn(|i|(a[i]*(1.0-tx)+b[i]*tx)*(1.0-ty)+(c[i]*(1.0-tx)+d[i]*tx)*ty);
            let at=(y as usize*r.width as usize+x as usize)*4;for i in 0..3{pixels[at+i]=crate::color_correction::encode(value[i]);}pixels[at+3]=(value[3]*255.0).round().clamp(0.0,255.0) as u8;
        }
    }
    RgbaFrame::new(r.width,r.height,pixels)
}
