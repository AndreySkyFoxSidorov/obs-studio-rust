//! OBS Studio32.2.2 v2 SDR kernels. SPDX-License-Identifier: GPL-2.0-or-later.
//! OBS contributors; Sharpen: libretro common-shaders, OBS conversion by Nibbles.
use crate::{ColorCorrectionInput,MediaError};
use crate::color_correction::{encode,unorm};
use crate::obs_keyer::{sample_lp,sampled_bytes};
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct LumaKeySettings {pub luma_max:f32,pub luma_min:f32,pub luma_max_smooth:f32,pub luma_min_smooth:f32,pub legacy:bool}
impl Default for LumaKeySettings {fn default()->Self{Self {luma_max:1.0,luma_min:0.0,luma_max_smooth:0.0,luma_min_smooth:0.0,legacy:false}}}
impl LumaKeySettings {pub fn validate(self)->Result<(),MediaError>{
    if [self.luma_max,self.luma_min,self.luma_max_smooth,self.luma_min_smooth].iter().all(|v|v.is_finite() && (0.0..=1.0).contains(v)){Ok(())}else{Err(MediaError::InvalidBuffer)}
}}
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct SharpenSettings {pub sharpness:f32}
impl Default for SharpenSettings {fn default()->Self{Self {sharpness:0.08}}}
impl SharpenSettings {pub fn validate(self)->Result<(),MediaError>{if self.sharpness.is_finite() && (0.0..=1.0).contains(&self.sharpness){Ok(())}else{Err(MediaError::InvalidBuffer)}}}
#[derive(Clone,Copy)]
pub(crate) struct PreparedLuma {low:f32,low_end:f32,high_begin:f32,high:f32,legacy:bool}
impl PreparedLuma {
    pub(crate) fn new(settings:LumaKeySettings)->Self{Self {low:settings.luma_min,low_end:settings.luma_min+settings.luma_min_smooth,
        high_begin:settings.luma_max-settings.luma_max_smooth,high:settings.luma_max,legacy:settings.legacy}}
    pub(crate) fn pixel(self,pixel:[u8;4],encoding:ColorCorrectionInput)->[u8;4]{
        let sampled=sample_lp(pixel,encoding);let inverse=if sampled[3]>0.0 {1.0/sampled[3]}else{0.0};
        let rgb=[sampled[0]*inverse,sampled[1]*inverse,sampled[2]*inverse];
        let luminance=if self.legacy {rgb[0]*0.2989+rgb[1]*0.5870+rgb[2]*0.1140} else {rgb[0]*0.2126+rgb[1]*0.7152+rgb[2]*0.0722};
        let low=smoothstep(self.low,self.low_end,luminance);
        let high=1.0-smoothstep(self.high_begin,self.high,luminance);
        let alpha=if self.legacy {low*high} else {sampled[3]*(low*high)};
        [encode(rgb[0]*alpha),encode(rgb[1]*alpha),encode(rgb[2]*alpha),unorm(alpha)]
    }
}
#[allow(clippy::manual_clamp)]
fn smoothstep(low:f32,high:f32,value:f32)->f32 {
    // HLSL saturate(NaN)=0. clamp() propagates NaN and would change the
    // authored zero-smooth defaults exactly at their threshold endpoints.
    let t=((value-low)/(high-low)).max(0.0).min(1.0);t*t*(3.0-2.0*t)
}
pub(crate) fn sharpen_in_place(pixels:&mut [u8],width:usize,height:usize,encoding:ColorCorrectionInput,
    settings:SharpenSettings,workspace:&mut [u32])->Result<(),MediaError>{
    settings.validate()?;
    if width==0 || height==0 || width.checked_mul(height).and_then(|v|v.checked_mul(4))!=Some(pixels.len()){return Err(MediaError::InvalidBuffer);}
    if settings.sharpness==0.0 {
        for pixel in pixels.chunks_exact_mut(4){let value=sampled_bytes([pixel[0],pixel[1],pixel[2],pixel[3]],encoding);pixel.copy_from_slice(&value);}return Ok(());
    }
    let row_words=width.checked_mul(4).ok_or(MediaError::Overflow)?;
    let words=row_words.checked_mul(3).ok_or(MediaError::Overflow)?;
    if workspace.len()<words{return Err(MediaError::BudgetExceeded);}
    fn load(pixels:&[u8],width:usize,row:usize,encoding:ColorCorrectionInput,output:&mut [u32]){
        for x in 0..width {let i=(row*width+x)*4;let value=sample_lp([pixels[i],pixels[i+1],pixels[i+2],pixels[i+3]],encoding);
            for channel in 0..4 {output[x*4+channel]=value[channel].to_bits();}}
    }
    fn at(rows:&[u32],row_words:usize,row:usize,x:usize)->[f32;4]{std::array::from_fn(|channel|f32::from_bits(rows[row*row_words+x*4+channel]))}
    load(pixels,width,0,encoding,&mut workspace[..row_words]);
    load(pixels,width,0,encoding,&mut workspace[row_words..2*row_words]);
    load(pixels,width,1.min(height-1),encoding,&mut workspace[2*row_words..3*row_words]);
    let (mut top,mut middle,mut bottom)=(0,1,2);
    for y in 0..height {
        for x in 0..width {
            let left=x.saturating_sub(1);let right=(x+1).min(width-1);
            let e=at(workspace,row_words,middle,x);let b=at(workspace,row_words,top,x);let d=at(workspace,row_words,middle,left);
            let f=at(workspace,row_words,middle,right);let h=at(workspace,row_words,bottom,x);
            let i=(y*width+x)*4;
            // distance(float4)!=0 is equivalent to any component differing:
            // sampled UNORM values are finite and nonzero differences cannot underflow.
            if !((e!=f && e!=d)||(e!=b && e!=h)) {
                let value=sampled_bytes([pixels[i],pixels[i+1],pixels[i+2],pixels[i+3]],encoding);pixels[i..i+4].copy_from_slice(&value);continue;
            }
            let a=at(workspace,row_words,top,left);let c=at(workspace,row_words,top,right);
            let g=at(workspace,row_words,bottom,left);let last=at(workspace,row_words,bottom,right);
            let result:[f32;4]=std::array::from_fn(|channel|{
                let mut color=8.0*e[channel];color-=a[channel];color-=b[channel];color-=c[channel];color-=d[channel];color-=f[channel];color-=g[channel];color-=h[channel];color-=last[channel];
                (e[channel]+color*settings.sharpness).clamp(0.0,1.0)
            });
            pixels[i..i+4].copy_from_slice(&[encode(result[0]),encode(result[1]),encode(result[2]),unorm(result[3])]);
        }
        if y+1<height {
            let reuse=top;top=middle;middle=bottom;bottom=reuse;
            load(pixels,width,(y+2).min(height-1),encoding,&mut workspace[bottom*row_words..(bottom+1)*row_words]);
        }
    }
    Ok(())
}
