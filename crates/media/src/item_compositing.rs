//! Scalar SDR implementation of OBS32.2.2 obs_blend_mode_params[] factors.
//! Source: libobs/obs-scene.c@ba2f32bdf791005443988a4955e963663e16b1ed,
//! GPL-2.0-or-later; source/license receipt retained with the staging references.
//! Alpha uses the original separate GPU factors, including MIN/MAX, reverse
//! subtraction and Multiply retaining destination alpha. This is not W3C blend.
use super::{AffineLayer,SamplingWindow,SourceBlend,source_color};

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub enum ItemBlendMode { #[default] Normal, Additive, Subtract, Screen, Multiply, Lighten, Darken }
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub enum ItemBlendingMethod { #[default] Default, SrgbOff }
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub enum SamplingFilter { #[default] Nearest, Bilinear, Bicubic, Lanczos, Area }

pub(super) struct Sample {pub(super) rgb:[f64;3],pub(super) alpha:f64}
pub(super) fn tap(pixel:&[u8],ingress:SourceBlend,method:ItemBlendingMethod)->Sample {
    let a=f64::from(pixel[3])/255.0;
    let rgb=std::array::from_fn(|i| {
        // Item render targets are encoded UNORM SDR premultiplied framebuffers.
        // Preserve the existing source-specific image versus color alpha policy.
        let byte=match ingress {
            SourceBlend::PremultipliedLinearSrgb=>pixel[i],
            // Opaque UNORM ingress is already the encoded target byte; this
            // exact round-trip identity avoids repeated transfer powers per tap.
            SourceBlend::LinearSrgb if pixel[3]==255=>pixel[i],
            SourceBlend::LinearSrgb=>source_color::encode(source_color::decode(pixel[i])*a),
            SourceBlend::EncodedPremultipliedSrgb|SourceBlend::Encoded=>
                ((u32::from(pixel[i])*u32::from(pixel[3])+127)/255) as u8,
        };
        match method {ItemBlendingMethod::Default=>source_color::decode(byte),ItemBlendingMethod::SrgbOff=>f64::from(byte)/255.0}
    });
    Sample {rgb,alpha:a}
}
pub(super) fn sample(layer:&AffineLayer<'_>,window:&SamplingWindow,u:f64,v:f64)->Sample {
    if layer.sampling_filter==SamplingFilter::Nearest {
        let (x,y)=window.sample(u,v,layer.image);
        let index=(y*layer.image.width as usize+x)*4;
        return tap(&layer.image.pixels[index..index+4],layer.blend_mode,layer.item_blending_method);
    }
    let (left,top,extent_x,extent_y,min_x,min_y,max_x,max_y)=match window {
        SamplingWindow::Texels(rect)=>(f64::from(rect.x),f64::from(rect.y),f64::from(rect.width),f64::from(rect.height),
            rect.x as usize,rect.y as usize,(rect.x+rect.width-1) as usize,(rect.y+rect.height-1) as usize),
        SamplingWindow::Uv {uv,last_x,last_y}=>{
            let w=f64::from(layer.image.width);let h=f64::from(layer.image.height);
            (uv.left*w,uv.top*h,(uv.right-uv.left)*w,(uv.bottom-uv.top)*h,
                ((uv.left*w).floor() as usize).min(*last_x),((uv.top*h).floor() as usize).min(*last_y),*last_x,*last_y)
        },
    };
    // Pixel-center convention. Clamp all taps to the selected crop window, not
    // the complete source, so down/upscaled crops do not leak excluded texels.
    let x=(left+u*extent_x-0.5).clamp(min_x as f64,max_x as f64);
    let y=(top+v*extent_y-0.5).clamp(min_y as f64,max_y as f64);
    let x0=x.floor() as usize;let y0=y.floor() as usize;
    let x1=(x0+1).min(max_x);let y1=(y0+1).min(max_y);
    let fx=x-x0 as f64;let fy=y-y0 as f64;
    let mut output=Sample {rgb:[0.0;3],alpha:0.0};
    for (sx,sy,weight) in [(x0,y0,(1.0-fx)*(1.0-fy)),(x1,y0,fx*(1.0-fy)),(x0,y1,(1.0-fx)*fy),(x1,y1,fx*fy)] {
        let index=(sy*layer.image.width as usize+sx)*4;
        let value=tap(&layer.image.pixels[index..index+4],layer.blend_mode,layer.item_blending_method);
        for channel in 0..3 {output.rgb[channel]+=value.rgb[channel]*weight;}
        output.alpha+=value.alpha*weight;
    }
    output.alpha=output.alpha.clamp(0.0,1.0);
    for channel in &mut output.rgb {*channel=channel.clamp(0.0,1.0);}
    output
}
fn encoded(value:f64)->f64 {
    let v=value.clamp(0.0,1.0);
    if v<=0.0031308 {v*12.92}else{1.055*v.powf(1.0/2.4)-0.055}
}
fn decoded(value:f64)->f64 {
    let v=value.clamp(0.0,1.0);
    if v<=0.04045 {v/12.92}else{((v+0.055)/1.055).powf(2.4)}
}
fn byte(value:f64)->u8 {(value.clamp(0.0,1.0)*255.0).round() as u8}
pub(super) fn blend(destination:&mut[u8],source:Sample,layer:&AffineLayer<'_>,premultiplied:bool) {
    let opacity=f64::from(layer.opacity);
    let sa=source.alpha*opacity;let da=f64::from(destination[3])/255.0;
    let mode=layer.item_blend_mode;let linear=layer.item_blending_method==ItemBlendingMethod::Default;
    let a=match mode {
        ItemBlendMode::Normal|ItemBlendMode::Screen=>sa+da*(1.0-sa),
        ItemBlendMode::Additive=>sa+da,
        ItemBlendMode::Subtract=>da-sa,
        ItemBlendMode::Multiply=>sa*da+da*(1.0-sa),
        ItemBlendMode::Lighten=>sa.max(da),ItemBlendMode::Darken=>sa.min(da),
    }.clamp(0.0,1.0);
    for (i,channel) in destination[..3].iter_mut().enumerate() {
        let s=source.rgb[i]*opacity;
        let d=if premultiplied {
            if linear {source_color::decode(*channel)}else{f64::from(*channel)/255.0}
        }else{
            let lp=source_color::decode(*channel)*da;
            if linear {lp}else{encoded(lp)}
        };
        let result=match mode {
            ItemBlendMode::Normal=>s+d*(1.0-sa),ItemBlendMode::Additive=>s+d,
            ItemBlendMode::Subtract=>d-s,ItemBlendMode::Screen=>s+d*(1.0-s),
            ItemBlendMode::Multiply=>s*d+d*(1.0-sa),ItemBlendMode::Lighten=>s.max(d),ItemBlendMode::Darken=>s.min(d),
        }.clamp(0.0,1.0);
        *channel=if premultiplied {if linear {source_color::encode(result)}else{byte(result)}}
            else if a>0.0 {source_color::encode((if linear {result}else{decoded(result)})/a)}else{0};
    }
    destination[3]=byte(a);
}
