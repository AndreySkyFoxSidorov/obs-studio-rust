//! OBS Studio 32.2.2 mask-filter v2 SDR operations (GPL-2.0-or-later).
//! Mask textures are STRAIGHT sRGB, sampled bilinearly with clamped edges.
//! Source ingress goes through the same RGBA8 filter-target rounding as keyers.
use crate::{ColorCorrectionInput, MediaError, RgbaFrame, SourceBlend};
use crate::color_correction::{decode, decode_table, encode, unorm};

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ImageMaskMethod { MaskColor,MaskAlpha,BlendMultiply,BlendAddition,BlendSubtraction }
#[derive(Clone,Copy,Debug)]
pub struct ImageMaskSettings {pub method:ImageMaskMethod,pub color:[u8;3],pub opacity:f32,pub stretch:bool}
#[derive(Clone,Copy,Debug)]
pub struct ImageMaskRequirements {pub input_bytes:usize,pub output_bytes:usize,pub texel_visits:u64}
#[derive(Clone,Copy,Debug)]
pub struct ImageMaskLimits {pub max_input_bytes:usize,pub max_output_bytes:usize,pub max_texel_visits:u64}
pub fn image_mask_requirements(source:(u32,u32),mask:(u32,u32),settings:ImageMaskSettings)->Result<ImageMaskRequirements,MediaError>{
    if !settings.opacity.is_finite() || !(0.0..=1.0).contains(&settings.opacity){return Err(MediaError::InvalidBuffer);}
    let bytes=|d:(u32,u32)|->Result<usize,MediaError>{
        if d.0==0 || d.1==0 || d.0>8192 || d.1>8192 {return Err(MediaError::InvalidBuffer);}
        (d.0 as usize).checked_mul(d.1 as usize).and_then(|v|v.checked_mul(4)).ok_or(MediaError::BudgetExceeded)
    };
    let output_bytes=bytes(source)?;let mask_bytes=bytes(mask)?;
    let input_bytes=output_bytes.checked_add(mask_bytes).ok_or(MediaError::BudgetExceeded)?;
    let texel_visits=(output_bytes as u64/4).checked_mul(5).ok_or(MediaError::BudgetExceeded)?;
    Ok(ImageMaskRequirements {input_bytes,output_bytes,texel_visits})
}
pub fn image_mask_v2(source:&RgbaFrame,ingress:SourceBlend,mask:&RgbaFrame,settings:ImageMaskSettings,limits:ImageMaskLimits)->Result<RgbaFrame,MediaError>{
    let required=image_mask_requirements((source.width(),source.height()),(mask.width(),mask.height()),settings)?;
    if required.input_bytes>limits.max_input_bytes || required.output_bytes>limits.max_output_bytes || required.texel_visits>limits.max_texel_visits{return Err(MediaError::BudgetExceeded);}
    let encoding=match ingress {SourceBlend::LinearSrgb=>ColorCorrectionInput::LinearAlpha,
        SourceBlend::Encoded|SourceBlend::EncodedPremultipliedSrgb=>ColorCorrectionInput::EncodedAlpha,
        SourceBlend::PremultipliedLinearSrgb=>ColorCorrectionInput::PremultipliedLinearSrgb};
    let (w,h)=(source.width() as f32,source.height() as f32);
    let (mw,mh)=(mask.width() as f32,mask.height() as f32);
    let (mul,add)=if settings.stretch {([1.0,1.0],[0.0,0.0])}else{
        let scale=if w/h<mw/mh {w/mw}else{h/mh};
        let size=[mw*scale,mh*scale];
        ([w/size[0],h/size[1]],[-(w*0.5-size[0]*0.5)/size[0],-(h*0.5-size[1]*0.5)/size[1]])
    };
    let tint=settings.color.map(decode);
    // The transfer table is process-wide and immutable; acquire it once per
    // frame rather than once for each of the mask's twelve color samples.
    let decoded=decode_table();
    // A single-pixel mask samples the same clamped texel at every position,
    // including when aspect preservation produces UVs outside the image.
    let constant_mask=(mask.width()==1&&mask.height()==1).then(||sample(mask,[0.5,0.5],decoded));
    let mut pixels=vec![0;required.output_bytes];
    for (index,(src,dst)) in source.pixels().chunks_exact(4).zip(pixels.chunks_exact_mut(4)).enumerate(){
        let lp=crate::obs_keyer::sample_lp([src[0],src[1],src[2],src[3]],encoding);
        let inverse=if lp[3]>0.0 {1.0/lp[3]}else{0.0};
        let mut rgb=[lp[0]*inverse,lp[1]*inverse,lp[2]*inverse];
        for i in 0..3 {rgb[i]*=tint[i];}
        let mut alpha=lp[3]*settings.opacity;
        let target=if let Some(value)=constant_mask {value} else {
            let uv=[((index%source.width() as usize) as f32+0.5)/w,((index/source.width() as usize) as f32+0.5)/h];
            let mapped=[uv[0]*mul[0]+add[0],uv[1]*mul[1]+add[1]];
            if settings.method==ImageMaskMethod::MaskAlpha {
                // This mode uses only target alpha. Avoid decoding and
                // interpolating twelve unused RGB components per pixel.
                [0.0,0.0,0.0,sample_alpha(mask,mapped)]
            } else { sample(mask,mapped,decoded) }
        };
        match settings.method {
            ImageMaskMethod::MaskColor=>alpha*=(target[0]+target[1]+target[2])/3.0,
            ImageMaskMethod::MaskAlpha=>alpha*=target[3],
            ImageMaskMethod::BlendMultiply=>for i in 0..3 {rgb[i]=(rgb[i]*target[i]).clamp(0.0,1.0);},
            ImageMaskMethod::BlendAddition=>for i in 0..3 {rgb[i]=(rgb[i]+target[i]).clamp(0.0,1.0);},
            ImageMaskMethod::BlendSubtraction=>for i in 0..3 {rgb[i]=(rgb[i]-target[i]).clamp(0.0,1.0);},
        }
        for i in 0..3 {dst[i]=encode(rgb[i]*alpha);}
        dst[3]=unorm(alpha);
    }
    RgbaFrame::new(source.width(),source.height(),pixels)
}
/// The fourth component of `sample`, retaining its interpolation order.
fn sample_alpha(image:&RgbaFrame,uv:[f32;2])->f32 {
    let x=(uv[0]*image.width() as f32-0.5).clamp(0.0,(image.width()-1) as f32);
    let y=(uv[1]*image.height() as f32-0.5).clamp(0.0,(image.height()-1) as f32);
    let x0=x.floor() as u32;let y0=y.floor() as u32;
    let x1=(x0+1).min(image.width()-1);let y1=(y0+1).min(image.height()-1);
    let read=|x:u32,y:u32| {let at=(y as usize*image.width() as usize+x as usize)*4;image.pixels()[at+3] as f32/255.0};
    let a=read(x0,y0);let b=read(x1,y0);let c=read(x0,y1);let d=read(x1,y1);
    let top=a*(1.0-(x-x0 as f32))+b*(x-x0 as f32);
    let bottom=c*(1.0-(x-x0 as f32))+d*(x-x0 as f32);
    top*(1.0-(y-y0 as f32))+bottom*(y-y0 as f32)
}
fn sample(image:&RgbaFrame,uv:[f32;2],decoded:&[f32;256])->[f32;4]{
    let x=(uv[0]*image.width() as f32-0.5).clamp(0.0,(image.width()-1) as f32);
    let y=(uv[1]*image.height() as f32-0.5).clamp(0.0,(image.height()-1) as f32);
    let x0=x.floor() as u32;let y0=y.floor() as u32;
    let x1=(x0+1).min(image.width()-1);let y1=(y0+1).min(image.height()-1);
    let read=|x:u32,y:u32|{let at=(y as usize*image.width() as usize+x as usize)*4;let p=&image.pixels()[at..at+4];[decoded[usize::from(p[0])],decoded[usize::from(p[1])],decoded[usize::from(p[2])],p[3] as f32/255.0]};
    let a=read(x0,y0);let b=read(x1,y0);let c=read(x0,y1);let d=read(x1,y1);
    std::array::from_fn(|i|{let top=a[i]*(1.0-(x-x0 as f32))+b[i]*(x-x0 as f32);let bottom=c[i]*(1.0-(x-x0 as f32))+d[i]*(x-x0 as f32);top*(1.0-(y-y0 as f32))+bottom*(y-y0 as f32)})
}
