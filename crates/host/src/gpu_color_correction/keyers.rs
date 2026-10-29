//! Fixed validated uniforms for OBS32 Color/Chroma/Luma Key and Sharpen v2. No native calls in
//! metadata admission, and no CPU keyer implementation is changed or invoked.
use super::*;
use skylive_media::{ColorKeySettings, ColorKeyType, ChromaKeySettings, ChromaKeyType,
    source_effect_requirements_for_dimensions};

#[derive(Clone, Copy)]
pub(super) enum Keyer { Color(Prepared), Chroma(Prepared), Luma(skylive_media::LumaKeySettings), Sharpen(skylive_media::SharpenSettings) }
#[derive(Clone, Copy)]
pub(super) struct Prepared {
    key:[f32;3], chroma:[f32;2], similarity:f32, smoothness:f32, spill:f32,
    opacity:f32, contrast:f32, brightness:f32, gamma:f32, authored:[u32;14],
}
impl Keyer {
    pub(super) fn from_effect(effect:SourceVideoEffect)->Result<Self,Error> {
        match effect {
            SourceVideoEffect::ColorKey(settings)=>{
                settings.validate().map_err(Error::Invalid)?;
                let key=match settings.key_color_type {
                    ColorKeyType::Green=>[0,255,0],ColorKeyType::Blue=>[0,0,255],
                    ColorKeyType::Red=>[255,0,0],ColorKeyType::Magenta=>[255,0,255],
                    ColorKeyType::Custom=>settings.key_color,
                };
                Ok(Self::Color(Prepared::new(key,settings.similarity,settings.smoothness,1000,
                    settings.opacity,settings.contrast,settings.brightness,settings.gamma,color_bits(settings))))
            }
            SourceVideoEffect::ChromaKey(settings)=>{
                settings.validate().map_err(Error::Invalid)?;
                let key=match settings.key_color_type {
                    ChromaKeyType::Green=>[0,255,0],ChromaKeyType::Blue=>[0,153,255],
                    ChromaKeyType::Magenta=>[255,0,255],ChromaKeyType::Custom=>settings.key_color,
                };
                Ok(Self::Chroma(Prepared::new(key,settings.similarity,settings.smoothness,settings.spill,
                    settings.opacity,settings.contrast,settings.brightness,settings.gamma,chroma_bits(settings))))
            }
            SourceVideoEffect::LumaKey(settings)=>{settings.validate().map_err(Error::Invalid)?;if settings.legacy{return Err(Error::Unsupported("GPU Luma Key v1 is not supported"));}Ok(Self::Luma(settings))},
            SourceVideoEffect::Sharpen(settings)=>{settings.validate().map_err(Error::Invalid)?;Ok(Self::Sharpen(settings))},
            _=>Err(Error::Unsupported("GPU keyer operation requires an OBS v2 keyer or sharpen stage")),
        }
    }
    pub(super) fn operation_id(&self)->u32{match self{Self::Color(_)=>2,Self::Chroma(_)=>3,Self::Luma(_)=>4,Self::Sharpen(_)=>5}}
    pub(super) fn authored(&self)->[u32;14] {match self {
        Self::Color(value)|Self::Chroma(value)=>value.authored,
        Self::Luma(s)=>[s.luma_max.to_bits(),s.luma_min.to_bits(),s.luma_max_smooth.to_bits(),s.luma_min_smooth.to_bits(),0,0,0,0,0,0,0,0,0,0],
        Self::Sharpen(s)=>[s.sharpness.to_bits(),0,0,0,0,0,0,0,0,0,0,0,0,0],
    }}
    pub(super) fn parameters(&self,image:&RgbaFrame,blend:SourceBlend)->[u8;PARAM_BYTES] {
        let mut bytes=[0u8;PARAM_BYTES];
        let policy=match blend {SourceBlend::LinearSrgb=>0u32,
            SourceBlend::Encoded|SourceBlend::EncodedPremultipliedSrgb=>1,
            SourceBlend::PremultipliedLinearSrgb=>2};
        for (index,value)in [(image.byte_len()/4)as u32,policy,image.width(),image.height()].into_iter().enumerate(){
            bytes[index*4..index*4+4].copy_from_slice(&value.to_le_bytes());
        }
        let mut values=[0.0f32;20];
        match self {
            Self::Color(p)|Self::Chroma(p)=>{values=[p.key[0],p.key[1],p.key[2],0.0,
                p.chroma[0],p.chroma[1],p.similarity,p.smoothness,
                p.opacity,p.contrast,p.brightness,p.gamma,
                p.spill,0.0,0.0,0.0,0.0,0.0,0.0,0.0];},
            Self::Luma(s)=>values[..4].copy_from_slice(&[s.luma_min,s.luma_min+s.luma_min_smooth,s.luma_max-s.luma_max_smooth,s.luma_max]),
            Self::Sharpen(s)=>values[0]=s.sharpness,
        }
        for (index,value)in values.into_iter().enumerate(){bytes[16+index*4..20+index*4].copy_from_slice(&value.to_le_bytes());}
        bytes
    }
}
impl Prepared {
    fn new(key:[u8;3],similarity:u16,smoothness:u16,spill:u16,opacity:f32,contrast:f64,brightness:f32,gamma:f64,authored:[u32;14])->Self{
        let key=key.map(|value|f32::from(value)/255.0);
        // Preserve the CPU/key-uniform matrix grouping; source chroma uses the
        // distinct left-associative RGB dot expression in the shader.
        let chroma=[(key[0]*-0.100644+key[2]*0.439216)+(key[1]*-0.338572+0.501961),
            (key[0]*0.439216+key[2]*-0.040274)+(key[1]*-0.398942+0.501961)];
        Self {key,chroma,similarity:f32::from(similarity)/1000.0,smoothness:f32::from(smoothness)/1000.0,
            spill:f32::from(spill)/1000.0,opacity,
            contrast:if contrast<0.0{1.0/(1.0-contrast)}else{contrast+1.0}as f32,brightness,
            gamma:if gamma<0.0{1.0-gamma}else{1.0/(gamma+1.0)}as f32,authored}
    }
}
fn bits(kind:u32,color:[u8;3],similarity:u16,smoothness:u16,spill:u16,opacity:f32,contrast:f64,brightness:f32,gamma:f64)->[u32;14]{
    let c=contrast.to_bits();let g=gamma.to_bits();
    [kind,u32::from(color[0])|(u32::from(color[1])<<8)|(u32::from(color[2])<<16),
        u32::from(similarity),u32::from(smoothness),u32::from(spill),opacity.to_bits(),
        c as u32,(c>>32)as u32,brightness.to_bits(),g as u32,(g>>32)as u32,0,0,0]
}
fn color_bits(s:ColorKeySettings)->[u32;14]{bits(s.key_color_type as u32,s.key_color,s.similarity,s.smoothness,0,s.opacity,s.contrast,s.brightness,s.gamma)}
fn chroma_bits(s:ChromaKeySettings)->[u32;14]{bits(s.key_color_type as u32,s.key_color,s.similarity,s.smoothness,s.spill,s.opacity,s.contrast,s.brightness,s.gamma)}

/// Complete metadata-only footprint for one supported point operation. This
/// runs for the whole ordered source chain before allocations or NN inference.
fn admit_point(dimensions:(u32,u32),effect:SourceVideoEffect,cpu:SourceEffectsLimits,free_cpu_bytes:usize,profile:Limits)->Result<usize,Error>{
    if !cfg!(any(target_os="windows",target_os="macos"))||!cfg!(target_endian="little"){
        return Err(Error::Unsupported("DX12/Metal little-endian native targets only"));
    }
    if matches!(effect,SourceVideoEffect::Craft(_)){return craft::admit(dimensions,&[effect],cpu,free_cpu_bytes,profile);}
    match effect {
        SourceVideoEffect::ColorCorrection(settings)=>{ColorCorrectionCoefficients::new(settings).map_err(Error::Invalid)?;}
        SourceVideoEffect::ColorKey(_)|SourceVideoEffect::ChromaKey(_)|SourceVideoEffect::LumaKey(_)|SourceVideoEffect::Sharpen(_)=>{Keyer::from_effect(effect)?;}
        _=>return Err(Error::Unsupported("point stage has no GPU compute kernel")),
    }
    let required=source_effect_requirements_for_dimensions(dimensions,&[effect]).map_err(Error::Invalid)?;
    if required.output_bytes>cpu.max_output_bytes||required.workspace_bytes>cpu.max_workspace_bytes
        ||required.pixel_stages>cpu.max_pixel_stages||required.texel_visits>cpu.max_texel_visits{
        return Err(Error::Limit("existing CPU keyer/point-stage admission"));
    }
    let bytes=(dimensions.0 as usize).checked_mul(dimensions.1 as usize).and_then(|n|n.checked_mul(4)).ok_or(Error::Limit("GPU point size overflow"))?;
    let peak=bytes.checked_mul(2).and_then(|n|n.checked_add(PARAM_BYTES)).ok_or(Error::Limit("GPU point staging overflow"))?.max(2*TABLE_BYTES);
    let device=bytes.checked_mul(3).and_then(|n|n.checked_add(TABLE_BYTES+PARAM_BYTES+MATTE_BYTES)).ok_or(Error::Limit("GPU point device overflow"))?;
    let invocations=(bytes as u64/4).div_ceil(64)*64;
    if dimensions.0==0||dimensions.1==0||dimensions.0>4096||dimensions.1>4096||bytes>profile.max_frame_bytes
        ||peak>profile.max_cpu_transient_bytes||peak>free_cpu_bytes||device>profile.max_device_bytes||invocations>profile.max_dispatch_invocations{
        return Err(Error::Limit("GPU point frame/staging/device/dispatch admission"));
    }
    Ok(peak)
}
impl Runtime {
    pub fn preflight_point(&self,dimensions:(u32,u32),effect:SourceVideoEffect,cpu:SourceEffectsLimits,free_cpu_bytes:usize)->Result<usize,Error>{
        let peak=admit_point(dimensions,effect,cpu,free_cpu_bytes,self.limits)?;
        let state=self.state.lock().map_err(|_|Error::Closed)?;
        if let Some(error)=&state.failure{return Err(error.clone());}
        if let Some(owner)=&state.owner{owner.client.preflight_point(dimensions,effect,cpu,free_cpu_bytes)?;}
        Ok(peak)
    }
    pub fn apply_keyer(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,effect:SourceVideoEffect,cpu:SourceEffectsLimits,free_cpu_bytes:usize)->Result<(SourceEffectFrame,Report),Error>{
        let input = input.into();
        self.preflight_point((input.width(),input.height()),effect,cpu,free_cpu_bytes)?;
        // Validate the operation before native startup, including direct API use.
        Keyer::from_effect(effect)?;
        self.client()?.apply_keyer(input,blend,effect,cpu,free_cpu_bytes)
    }
}
impl Client {
    pub fn preflight_point(&self,dimensions:(u32,u32),effect:SourceVideoEffect,cpu:SourceEffectsLimits,free_cpu_bytes:usize)->Result<usize,Error>{
        let peak=admit_point(dimensions,effect,cpu,free_cpu_bytes,self.shared.limits)?;
        // Initialization or device failure closes the worker as well. Report
        // the cause first so PreferGpu can distinguish a safe pre-submission
        // initialization failure from an explicitly closed owner.
        if let Some(error)=self.shared.failure.lock().map_err(|_|Error::Closed)?.clone(){return Err(error);}
        if self.shared.closed.load(Ordering::Acquire){return Err(Error::Closed);}
        Ok(peak)
    }
    pub fn apply_keyer(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,effect:SourceVideoEffect,cpu:SourceEffectsLimits,free_cpu_bytes:usize)->Result<(SourceEffectFrame,Report),Error>{
        let input = input.into();
        self.preflight_point((input.width(),input.height()),effect,cpu,free_cpu_bytes)?;
        let keyer=Keyer::from_effect(effect)?;let authored=keyer.authored();
        let permit=self.reserve()?;
        self.submit(input,blend,authored,Operation::Keyer(keyer),[0;32],permit)
    }
}

// A pair is admitted before either dispatch; all normal per-stage limits remain.
fn admit_pair(dimensions:(u32,u32),effects:&[SourceVideoEffect],cpu:SourceEffectsLimits,free:usize,profile:Limits)->Result<usize,Error>{
    if effects.len()!=2{return Err(Error::Unsupported("GPU keyer pair requires exactly two stages"));}
    let first=Keyer::from_effect(effects[0])?;
    let second=Keyer::from_effect(effects[1])?;
    let _=(first,second);
    let required=source_effect_requirements_for_dimensions(dimensions,effects).map_err(Error::Invalid)?;
    if required.output_bytes>cpu.max_output_bytes||required.workspace_bytes>cpu.max_workspace_bytes||required.pixel_stages>cpu.max_pixel_stages||required.texel_visits>cpu.max_texel_visits{return Err(Error::Limit("GPU keyer pair ordered admission"));}
    let mut peak=0;
    for effect in effects {let one=source_effect_requirements_for_dimensions(dimensions,&[*effect]).map_err(Error::Invalid)?;
        let limits=SourceEffectsLimits {max_output_bytes:one.output_bytes,max_workspace_bytes:one.workspace_bytes,max_pixel_stages:one.pixel_stages,max_texel_visits:one.texel_visits};
        peak=peak.max(admit_point(dimensions,*effect,limits,free,profile)?);
    }
    peak=peak.checked_add(PARAM_BYTES).ok_or(Error::Limit("GPU pair staging overflow"))?;
    if peak>free||peak>profile.max_cpu_transient_bytes{return Err(Error::Limit("GPU pair uniform upload staging"));}
    Ok(peak)
}
impl Runtime {
    pub(crate) fn apply_keyer_pair(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,effects:&[SourceVideoEffect],cpu:SourceEffectsLimits,free:usize)->Result<(SourceEffectFrame,Report),Error>{
        let input = input.into();
        admit_pair((input.width(),input.height()),effects,cpu,free,self.limits)?;
        self.client()?.apply_keyer_pair(input,blend,effects,cpu,free)
    }
}
impl Client {
    pub(crate) fn apply_keyer_pair(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,effects:&[SourceVideoEffect],cpu:SourceEffectsLimits,free:usize)->Result<(SourceEffectFrame,Report),Error>{
        let input = input.into();
        admit_pair((input.width(),input.height()),effects,cpu,free,self.shared.limits)?;
        let first=Keyer::from_effect(effects[0])?;let second=Keyer::from_effect(effects[1])?;
        let mut digest=Sha256::new();
        for keyer in [first,second] {digest.update(keyer.operation_id().to_le_bytes());for value in keyer.authored(){digest.update(value.to_le_bytes());}}
        let digest=digest.finalize().into();let permit=self.reserve()?;
        self.submit(input,blend,[0;14],Operation::KeyerPair(first,second),digest,permit)
    }
}
#[derive(Clone,Copy)]
pub(super) enum RunStage {Keyer(Keyer),Correction(ColorCorrectionCoefficients)}
impl RunStage {
    fn prepare(effect:SourceVideoEffect)->Result<Self,Error>{match effect {
        SourceVideoEffect::ColorCorrection(settings)=>Ok(Self::Correction(ColorCorrectionCoefficients::new(settings).map_err(Error::Invalid)?)),
        _=>Ok(Self::Keyer(Keyer::from_effect(effect)?)),
    }}
    pub(super) fn parameters(&self,image:&RgbaFrame,blend:SourceBlend)->[u8;PARAM_BYTES]{match self{
        Self::Keyer(keyer)=>keyer.parameters(image,blend),
        Self::Correction(coefficients)=>super::parameters(image,blend,coefficients),
    }}
    fn digest(&self,hash:&mut Sha256){match self{
        Self::Keyer(keyer)=>{hash.update(keyer.operation_id().to_le_bytes());for value in keyer.authored(){hash.update(value.to_le_bytes());}},
        Self::Correction(coefficients)=>{hash.update(0u32.to_le_bytes());for value in coefficients.authored_bits(){hash.update(value.to_le_bytes());}},
    }}
}
pub(super) struct PointRun {pub stages:[RunStage;skylive_core::MAX_VIDEO_FILTERS],pub count:usize}
fn admit_run(dimensions:(u32,u32),effects:&[SourceVideoEffect],cpu:SourceEffectsLimits,free:usize,profile:Limits)->Result<usize,Error>{
    if !(2..=skylive_core::MAX_VIDEO_FILTERS).contains(&effects.len()){return Err(Error::Unsupported("GPU point run exceeds the supported filter count"));}
    let required=source_effect_requirements_for_dimensions(dimensions,effects).map_err(Error::Invalid)?;
    if required.output_bytes>cpu.max_output_bytes||required.workspace_bytes>cpu.max_workspace_bytes||required.pixel_stages>cpu.max_pixel_stages||required.texel_visits>cpu.max_texel_visits{return Err(Error::Limit("GPU point run ordered admission"));}
    let mut peak=0usize;
    for effect in effects {
        RunStage::prepare(*effect)?;
        let one=source_effect_requirements_for_dimensions(dimensions,&[*effect]).map_err(Error::Invalid)?;
        let limits=SourceEffectsLimits{max_output_bytes:one.output_bytes,max_workspace_bytes:one.workspace_bytes,max_pixel_stages:one.pixel_stages,max_texel_visits:one.texel_visits};
        peak=peak.max(admit_point(dimensions,*effect,limits,free,profile)?);
    }
    let staging=(effects.len()-1).checked_mul(PARAM_BYTES).ok_or(Error::Limit("GPU point run staging overflow"))?;
    peak=peak.checked_add(staging).ok_or(Error::Limit("GPU point run staging overflow"))?;
    if peak>free||peak>profile.max_cpu_transient_bytes{return Err(Error::Limit("GPU point run uniform upload staging"));}
    Ok(peak)
}
impl Runtime {
    pub(crate) fn apply_point_run(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,effects:&[SourceVideoEffect],cpu:SourceEffectsLimits,free:usize)->Result<(SourceEffectFrame,Report),Error>{
        let input = input.into();
        admit_run((input.width(),input.height()),effects,cpu,free,self.limits)?;
        self.client()?.apply_point_run(input,blend,effects,cpu,free)
    }
}
impl Client {
    pub(crate) fn apply_point_run(&self,input:impl Into<SharedVideoFrame>,blend:SourceBlend,effects:&[SourceVideoEffect],cpu:SourceEffectsLimits,free:usize)->Result<(SourceEffectFrame,Report),Error>{
        let input = input.into();
        admit_run((input.width(),input.height()),effects,cpu,free,self.shared.limits)?;
        let first=RunStage::prepare(effects[0])?;let mut run=PointRun{stages:[first;skylive_core::MAX_VIDEO_FILTERS],count:effects.len()};
        let mut digest=Sha256::new();digest.update((effects.len() as u32).to_le_bytes());
        for (index,effect) in effects.iter().enumerate(){let stage=RunStage::prepare(*effect)?;run.stages[index]=stage;stage.digest(&mut digest);}
        let digest=digest.finalize().into();let permit=self.reserve()?;
        self.submit(input,blend,[0;14],Operation::PointRun(run),digest,permit)
    }
}
