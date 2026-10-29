//! Typed SDR transition ingress/output. No source ownership or clock lives here.
use crate::{CompositeLimits,MediaError,RgbaFrame,SourceBlend};
pub(crate) fn lp(pixel:&[u8],blend:SourceBlend)->[f64;4]{
    let alpha=f64::from(pixel[3])/255.0;let mut value=[0.0;4];value[3]=alpha;
    for channel in 0..3{value[channel]=match blend{
        SourceBlend::Encoded|SourceBlend::EncodedPremultipliedSrgb=>crate::source_color::decode(((u32::from(pixel[channel])*u32::from(pixel[3])+127)/255)as u8),
        SourceBlend::LinearSrgb=>crate::source_color::decode(pixel[channel])*alpha,
        SourceBlend::PremultipliedLinearSrgb=>crate::source_color::decode(pixel[channel]),
    };}value
}
pub(crate) fn store(value:[f64;4],output:&mut[u8]){for channel in 0..3{output[channel]=crate::source_color::encode(value[channel]);}output[3]=(value[3]*255.0).round().clamp(0.0,255.0)as u8;}
pub(crate) fn admit(a:&RgbaFrame,b:&RgbaFrame,limits:CompositeLimits,per_pixel:u64)->Result<(),MediaError>{
    if a.width()!=b.width()||a.height()!=b.height(){return Err(MediaError::InvalidDimensions);}
    let bytes=if a.pixels().as_ptr()==b.pixels().as_ptr(){a.byte_len()}else{a.byte_len().checked_add(b.byte_len()).ok_or(MediaError::Overflow)?};
    let visits=u64::from(a.width()).checked_mul(u64::from(a.height())).and_then(|n|n.checked_mul(per_pixel)).ok_or(MediaError::Overflow)?;
    if limits.max_layers<2||bytes>limits.max_input_bytes||a.byte_len()>limits.max_output_bytes||visits>limits.max_pixel_visits{return Err(MediaError::BudgetExceeded);}Ok(())
}
fn texture(pixel:&[u8],blend:SourceBlend)->[u8;4]{let mut output=[0;4];output[3]=pixel[3];for channel in 0..3{output[channel]=match blend{SourceBlend::Encoded|SourceBlend::EncodedPremultipliedSrgb=>((u32::from(pixel[channel])*u32::from(pixel[3])+127)/255)as u8,SourceBlend::PremultipliedLinearSrgb=>pixel[channel],SourceBlend::LinearSrgb=>crate::source_color::encode(crate::source_color::decode(pixel[channel])*f64::from(pixel[3])/255.0)};}output}
/// Encoded-SDR texture interpolation, with an explicit premultiplied-linear
/// framebuffer result. All4 ingress modes preserve their source policy;
/// alpha-zero LP energy is never discarded or unpremultiplied.
pub fn crossfade_rgba_with_blend(a:&RgbaFrame,a_blend:SourceBlend,b:&RgbaFrame,b_blend:SourceBlend,elapsed:u64,duration:u64,limits:CompositeLimits)->Result<RgbaFrame,MediaError>{
    if duration==0||elapsed>duration{return Err(MediaError::InvalidTransition);}admit(a,b,limits,3)?;
    let wa=u128::from(duration-elapsed);let wb=u128::from(elapsed);let denominator=u128::from(duration);let mut pixels=Vec::new();pixels.try_reserve_exact(a.byte_len()).map_err(|_|MediaError::BudgetExceeded)?;pixels.resize(a.byte_len(),0);
    for((a,b),out)in a.pixels().chunks_exact(4).zip(b.pixels().chunks_exact(4)).zip(pixels.chunks_exact_mut(4)){let a=texture(a,a_blend);let b=texture(b,b_blend);for c in 0..4{out[c]=((u128::from(a[c])*wa+u128::from(b[c])*wb+denominator/2)/denominator)as u8;}}
    RgbaFrame::new(a.width(),a.height(),pixels)
}
