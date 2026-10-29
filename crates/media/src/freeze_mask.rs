//! Exeldro Freeze Filter0.3.5 freeze_part.effect (GPLv2 reference): exact
//! percentage-region equations with the application's typed SDR source-over.
use crate::{MediaError,RgbaFrame,SourceBlend};
#[derive(Clone,Copy,Debug,PartialEq)]pub struct FreezeMaskSettings{pub percent:[f32;4]}
#[derive(Clone,Copy,Debug)]pub struct FreezeMaskRequirements{pub input_bytes:usize,pub output_bytes:usize,pub pixel_visits:u64}
#[derive(Clone,Copy,Debug)]pub struct FreezeMaskLimits{pub max_input_bytes:usize,pub max_output_bytes:usize,pub max_pixel_visits:u64}
pub fn freeze_mask_requirements(dimensions:(u32,u32),settings:FreezeMaskSettings)->Result<FreezeMaskRequirements,MediaError>{if settings.percent.iter().any(|v|!v.is_finite()||!(0.0..=100.0).contains(v)){return Err(MediaError::InvalidBuffer);}let output_bytes=crate::image_bytes(dimensions.0,dimensions.1)?;let input_bytes=output_bytes.checked_mul(2).ok_or(MediaError::Overflow)?;let pixel_visits=(output_bytes as u64/4).checked_mul(6).ok_or(MediaError::Overflow)?;Ok(FreezeMaskRequirements{input_bytes,output_bytes,pixel_visits})}
fn axis(uv:f32,left:f32,right:f32)->bool{if left+right>1.0{!(uv>left||1.0-uv>right)}else if left>0.0||right>0.0{!((left<=0.0||uv>left)&&(right<=0.0||1.0-uv>right))}else{false}}
fn selected(uv:[f32;2],percent:[f32;4])->bool{let [left,right,top,bottom]=percent.map(|v|v/100.0);let horizontal=axis(uv[0],left,right);let vertical=axis(uv[1],top,bottom);if top+bottom>1.0&&left+right>1.0{horizontal&&vertical}else{horizontal||vertical}}
/// Output is always PremultipliedLinearSrgb. Full unmasked freeze is a host
/// held-frame return and never passes through this partial-overlay operation.
pub fn freeze_mask_rgba(live:&RgbaFrame,live_blend:SourceBlend,held:&RgbaFrame,held_blend:SourceBlend,settings:FreezeMaskSettings,limits:FreezeMaskLimits)->Result<RgbaFrame,MediaError>{
 if live.width()!=held.width()||live.height()!=held.height(){return Err(MediaError::InvalidDimensions);}let required=freeze_mask_requirements((live.width(),live.height()),settings)?;if required.input_bytes>limits.max_input_bytes||required.output_bytes>limits.max_output_bytes||required.pixel_visits>limits.max_pixel_visits{return Err(MediaError::BudgetExceeded);}
 let mut pixels=Vec::new();pixels.try_reserve_exact(required.output_bytes).map_err(|_|MediaError::BudgetExceeded)?;pixels.resize(required.output_bytes,0);let width=live.width() as usize;let w=live.width() as f32;let h=live.height() as f32;
 for(index,((live,held),out))in live.pixels().chunks_exact(4).zip(held.pixels().chunks_exact(4)).zip(pixels.chunks_exact_mut(4)).enumerate(){
  let region=selected([((index%width)as f32+0.5)/w,((index/width)as f32+0.5)/h],settings.percent);
  if !region{if live_blend==SourceBlend::PremultipliedLinearSrgb||live[3]==255{out.copy_from_slice(live);}else{crate::source_transition::store(crate::source_transition::lp(live,live_blend),out);}continue;}
  // In every supported SDR ingress mode alpha255 has identical framebuffer
  // bytes. Avoid pow/encode for the common opaque camera region, without
  // changing alpha-zero premultiplied emission or partial-alpha equations.
  if held[3]==255{out.copy_from_slice(held);continue;}
  let background=crate::source_transition::lp(live,live_blend);let foreground=crate::source_transition::lp(held,held_blend);let remain=1.0-foreground[3];let mut value=[0.0;4];for c in 0..4{value[c]=foreground[c]+background[c]*remain;}crate::source_transition::store(value,out);
 }
 RgbaFrame::new(live.width(),live.height(),pixels)
}
