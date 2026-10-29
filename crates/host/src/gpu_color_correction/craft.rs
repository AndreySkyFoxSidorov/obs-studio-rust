//! One floating-point run of the supported MIT Craft kernels. The fixed
//! payload borrows the native matte buffer only while this unique job owns it.
//! No new owner/device/frame slot or per-pixel host work is introduced.
use super::*;
use skylive_media::{CraftVideoEffect, ColorKeyMatch, ColorKeyView,
    source_effect_requirements_for_dimensions};

const STAGES:usize=skylive_core::MAX_VIDEO_FILTERS;
const WORDS:usize=16;
pub(super) const CRAFT_BYTES:usize=STAGES*WORDS*4;
// The payload shares the already-admitted matte buffer on the GPU worker.
const _: () = assert!(CRAFT_BYTES <= MATTE_BYTES);
pub(super) struct Craft {pub payload:[u8;CRAFT_BYTES],count:u32}
impl Craft {
    fn prepare(effects:&[SourceVideoEffect])->Result<Self,Error> {
        if effects.is_empty()||effects.len()>STAGES{return Err(Error::Limit("Craft GPU run exceeds the supported filter count"));}
        // The same pure dimension planner validates all authored coefficients.
        source_effect_requirements_for_dimensions((1,1),effects).map_err(Error::Invalid)?;
        let mut result=Self {payload:[0;CRAFT_BYTES],count:effects.len() as u32};
        for (index,effect) in effects.iter().enumerate(){
            let SourceVideoEffect::Craft(effect)=effect else{return Err(Error::Unsupported("Craft GPU run contains another provider"));};
            let mut words=[0u32;WORDS];
            match *effect {
                CraftVideoEffect::GammaPedestalGain{gamma,pedestal,gain,black_stretch}=>{
                    words[0]=15; words[4]=black_stretch.to_bits();
                    for i in 0..3 {words[5+i]=gamma[i].to_bits();words[8+i]=pedestal[i].to_bits();words[11+i]=gain[i].to_bits();}
                },
                CraftVideoEffect::ChannelMixer{matrix,monochrome}=>{
                    words[0]=13;words[1]=u32::from(monochrome);
                    for row in 0..3 {for column in 0..4 {
                        words[4+row*4+column]=(matrix[row][column]/100.0).to_bits();
                    }}
                },
                CraftVideoEffect::PhotoFilter{color,density,preserve_luminosity}=>{
                    words[0]=14;words[1]=u32::from(preserve_luminosity);words[4]=(density/100.0).to_bits();
                    for i in 0..3 {words[5+i]=color[i].to_bits();}
                },
                CraftVideoEffect::ColorBalance{shadows,midtones,highlights,preserve_luminosity}=>{
                    words[0]=12;words[1]=u32::from(preserve_luminosity);
                    for i in 0..3{words[4+i]=(shadows[i]/100.0).to_bits();words[7+i]=(midtones[i]/100.0).to_bits();words[10+i]=(highlights[i]/100.0).to_bits();}
                },
                CraftVideoEffect::Tritone{shadows,midtones,highlights,blend}=>{words[0]=11;words[4]=(1.0-blend/100.0).to_bits();for i in 0..3{words[5+i]=(f32::from(shadows[i])/255.0).to_bits();words[8+i]=(f32::from(midtones[i])/255.0).to_bits();words[11+i]=(f32::from(highlights[i])/255.0).to_bits();}},
                CraftVideoEffect::Tint{black,white,amount}=>{words[0]=10;words[4]=(amount/100.0).to_bits();for i in 0..3{words[5+i]=(f32::from(black[i])/255.0).to_bits();words[8+i]=(f32::from(white[i])/255.0).to_bits();}},
                CraftVideoEffect::Vibrance{vibrance,saturation}=>{
                    words[0]=9;words[4]=(vibrance/100.0).to_bits();words[5]=(saturation/100.0).to_bits();},
                CraftVideoEffect::Sepia{density,preserve_luminosity}=>{
                    words[0]=14;words[1]=u32::from(preserve_luminosity);words[4]=(density/100.0).to_bits();
                    for (i,channel) in [170.0f32/255.0,120.0f32/255.0,55.0f32/255.0].into_iter().enumerate(){words[5+i]=channel.to_bits();}
                },
                CraftVideoEffect::Invert{amount}=>{words[0]=0;words[4]=(amount/100.0).to_bits();},
                CraftVideoEffect::Grayscale{amount}=>{words[0]=1;words[4]=(amount/100.0).to_bits();},
                CraftVideoEffect::Posterize{levels}=>{words[0]=2;words[4]=f32::from(levels).to_bits();},
                CraftVideoEffect::Exposure{gain,offset,inverse_gamma,bypass_linear}=>{
                    words[0]=3;words[1]=u32::from(bypass_linear);
                    words[4]=gain.to_bits();words[5]=offset.to_bits();words[6]=inverse_gamma.to_bits();},
                CraftVideoEffect::TemperatureTint{temperature,tint}=>{
                    words[0]=4;words[4]=(temperature/100.0).to_bits();words[5]=(tint/100.0).to_bits();},
                CraftVideoEffect::Threshold{level}=>{words[0]=5;words[4]=f32::from(level).to_bits();},
                CraftVideoEffect::LinearColorKey{color,matching,tolerance,softness,keep,view}=>{
                    words[0]=6;words[1]=match matching{ColorKeyMatch::Rgb=>0,ColorKeyMatch::Hue=>1,ColorKeyMatch::Chroma=>2};
                    words[2]=u32::from(keep);words[3]=match view{ColorKeyView::Final=>0,ColorKeyView::Source=>1,ColorKeyView::Matte=>2};
                    for channel in 0..3{words[4+channel]=(f32::from(color[channel])/255.0).to_bits();}
                    words[7]=(tolerance/100.0).to_bits();words[8]=(softness/100.0).to_bits();},
                CraftVideoEffect::BrightnessContrast{brightness,contrast,legacy}=>{
                    words[0]=7;words[1]=u32::from(legacy);words[4]=(brightness/100.0).to_bits();words[5]=(contrast/100.0).to_bits();},
                CraftVideoEffect::SpillSuppression{blue,balance,amount}=>{
                    words[0]=8;words[1]=u32::from(blue);words[4]=(balance/100.0).to_bits();words[5]=(amount/100.0).to_bits();},
            };
            for (word,value)in words.into_iter().enumerate(){let offset=(index*WORDS+word)*4;result.payload[offset..offset+4].copy_from_slice(&value.to_le_bytes());}
        }
        Ok(result)
    }
    pub(super) fn parameters(&self,image:&RgbaFrame,blend:SourceBlend)->[u8;PARAM_BYTES]{
        let mut bytes=[0;PARAM_BYTES];
        let policy=u32::from(blend==SourceBlend::PremultipliedLinearSrgb);
        for (index,value)in [(image.byte_len()/4)as u32,policy,image.width(),image.height(),self.count].into_iter().enumerate(){bytes[index*4..index*4+4].copy_from_slice(&value.to_le_bytes());}
        bytes
    }
}

pub(super) fn admit(dimensions:(u32,u32),effects:&[SourceVideoEffect],cpu:SourceEffectsLimits,free:usize,profile:Limits)->Result<usize,Error>{
    if !cfg!(any(target_os="windows",target_os="macos"))||!cfg!(target_endian="little"){return Err(Error::Unsupported("DX12/Metal little-endian native targets only"));}
    let _=Craft::prepare(effects)?;
    let required=source_effect_requirements_for_dimensions(dimensions,effects).map_err(Error::Invalid)?;
    if required.output_bytes>cpu.max_output_bytes||required.workspace_bytes>cpu.max_workspace_bytes||required.pixel_stages>cpu.max_pixel_stages||required.texel_visits>cpu.max_texel_visits{return Err(Error::Limit("existing CPU Craft run admission"));}
    let bytes=(dimensions.0 as usize).checked_mul(dimensions.1 as usize).and_then(|v|v.checked_mul(4)).ok_or(Error::Limit("Craft GPU frame size overflow"))?;
    let peak=bytes.checked_mul(2).and_then(|v|v.checked_add(2*CRAFT_BYTES+PARAM_BYTES)).ok_or(Error::Limit("Craft GPU staging overflow"))?.max(2*TABLE_BYTES);
    let device=bytes.checked_mul(3).and_then(|v|v.checked_add(TABLE_BYTES+PARAM_BYTES+MATTE_BYTES)).ok_or(Error::Limit("Craft GPU device overflow"))?;
    if bytes>profile.max_frame_bytes||dimensions.0>4096||dimensions.1>4096||peak>free||peak>profile.max_cpu_transient_bytes||device>profile.max_device_bytes||(bytes as u64/4).div_ceil(64)*64>profile.max_dispatch_invocations{return Err(Error::Limit("Craft GPU frame/staging/device/dispatch admission"));}
    Ok(peak)
}
impl Runtime {
    pub fn apply_craft(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,effects:&[SourceVideoEffect],cpu:SourceEffectsLimits,free:usize)->Result<(SourceEffectFrame,Report),Error>{
        let input = input.into();
        admit((input.width(),input.height()),effects,cpu,free,self.limits)?;
        self.client()?.apply_craft(input,blend,effects,cpu,free)
    }
}
impl Client {
    pub fn apply_craft(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,effects:&[SourceVideoEffect],cpu:SourceEffectsLimits,free:usize)->Result<(SourceEffectFrame,Report),Error>{
        let input = input.into();
        admit((input.width(),input.height()),effects,cpu,free,self.shared.limits)?;
        let craft=Craft::prepare(effects)?;
        let digest=Sha256::digest(craft.payload).into();
        let mut authored=[0;14];authored[0]=craft.count;
        let permit=self.reserve()?;
        self.submit(input,blend,authored,Operation::Craft(craft),digest,permit)
    }
}
