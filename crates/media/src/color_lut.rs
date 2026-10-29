//! OBS32.2.2 color_grade_filter.effect SDR equations (GPL-2.0-or-later).
//! CUBE binary16 conversion follows OBS half.h / Microsoft DirectXMath (MIT).
//! No I/O, native resources, parsing or canonical mutation in this kernel.
use crate::{MediaError,RgbaFrame,SourceBlend,ColorCorrectionInput};
use crate::color_correction::{decode,encode};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum LutKind {OneD,ThreeD}
pub struct ColorLut {kind:LutKind,size:u32,domain_min:[f32;3],domain_max:[f32;3],scale:[f32;3],offset:[f32;3],values:Vec<[f32;3]>,shaper:Option<Shaper>,resolve_single:bool}
struct Shaper {values:Vec<[f32;3]>,min:f32,max:f32}
impl ColorLut {
    pub fn new(kind:LutKind,size:u32,domain_min:[f32;3],domain_max:[f32;3],values:Vec<[f32;3]>)->Result<Self,MediaError>{
        let count=match kind {LutKind::OneD if (2..=65536).contains(&size)=>size as usize,LutKind::ThreeD if (2..=128).contains(&size)=>(size as usize).pow(3),_=>return Err(MediaError::InvalidBuffer)};
        if values.len()!=count||values.capacity().checked_mul(12).is_none_or(|bytes|bytes>24*1024*1024)||values.iter().flatten().any(|v|!v.is_finite()){return Err(MediaError::InvalidBuffer);}
        let mut scale=[0.0;3];let mut offset=[0.0;3];
        for i in 0..3 {
            if !domain_min[i].is_finite()||!domain_max[i].is_finite()||domain_min[i]>=domain_max[i]{return Err(MediaError::InvalidBuffer);}
            let raw=(size-1) as f32/(domain_max[i]-domain_min[i]);
            scale[i]=raw/size as f32;offset[i]=(-domain_min[i]*raw+0.5)/size as f32;
            if !scale[i].is_finite()||!offset[i].is_finite(){return Err(MediaError::InvalidBuffer);}
        }
        Ok(Self {kind,size,domain_min,domain_max,scale,offset,values,shaper:None,resolve_single:false})
    }
    /// Resolve single-table values are interpolated in their stored nonlinear
    /// domain, with the input clamped to the declared scalar range.
    pub fn new_resolve_single(kind:LutKind,size:u32,min:f32,max:f32,values:Vec<[f32;3]>)->Result<Self,MediaError>{
        let mut lut=Self::new(kind,size,[min;3],[max;3],values)?;
        lut.resolve_single=true;Ok(lut)
    }
    /// Resolve CUBE: both tables contain nonlinear values; the 3D output is
    /// linearized after interpolation, before mixing with the source.
    pub fn new_shaper_3d(size:u32,min:f32,max:f32,values:Vec<[f32;3]>,shaper_min:f32,shaper_max:f32,shaper_values:Vec<[f32;3]>)->Result<Self,MediaError>{
        if !(2..=65536).contains(&shaper_values.len()) || shaper_values.iter().flatten().any(|v|!v.is_finite())
            || !shaper_min.is_finite() || !shaper_max.is_finite() || shaper_min>=shaper_max
            || !(shaper_max-shaper_min).is_finite() {return Err(MediaError::InvalidBuffer);}
        let mut lut=Self::new(LutKind::ThreeD,size,[min;3],[max;3],values)?;
        let total=lut.values.capacity().checked_add(shaper_values.capacity()).and_then(|n|n.checked_mul(12)).ok_or(MediaError::BudgetExceeded)?;
        if total>24*1024*1024{return Err(MediaError::BudgetExceeded);}
        lut.shaper=Some(Shaper {values:shaper_values,min:shaper_min,max:shaper_max});Ok(lut)
    }
    pub fn has_shaper(&self)->bool{self.shaper.is_some()}
    pub fn needs_cpu_resolve(&self)->bool{self.has_shaper()||self.resolve_single}
    pub fn kind(&self)->LutKind{self.kind}
    pub fn size(&self)->u32{self.size}
    pub fn byte_len(&self)->usize{(self.values.capacity()+self.shaper.as_ref().map_or(0,|s|s.values.capacity()))*12}
    /// Borrow validated immutable table data; evaluators own their native copy.
    pub fn entries(&self)->&[[f32;3]]{&self.values}
    pub fn domain(&self)->([f32;3],[f32;3]){(self.domain_min,self.domain_max)}
    pub fn texture_transform(&self)->([f32;3],[f32;3]){(self.scale,self.offset)}
    fn axis(&self,value:f32,channel:usize)->(usize,usize,f32){
        let uv=value*self.scale[channel]+self.offset[channel];
        let point=(uv*self.size as f32-0.5).clamp(0.0,(self.size-1) as f32);
        let lo=point.floor() as usize;(lo,(lo+1).min(self.size as usize-1),point-lo as f32)
    }
    fn channel(&self,value:f32,channel:usize)->f32{let(a,b,t)=self.axis(value,channel);self.values[a][channel]*(1.0-t)+self.values[b][channel]*t}
    fn sample(&self,rgb:[f32;3])->[f32;3]{
        let(x0,x1,tx)=self.axis(rgb[0],0);let(y0,y1,ty)=self.axis(rgb[1],1);let(z0,z1,tz)=self.axis(rgb[2],2);let n=self.size as usize;
        let read=|x,y,z|self.values[(z*n+y)*n+x];
        let a=read(x0,y0,z0);let b=read(x1,y0,z0);let c=read(x0,y1,z0);let d=read(x1,y1,z0);
        let e=read(x0,y0,z1);let f=read(x1,y0,z1);let g=read(x0,y1,z1);let h=read(x1,y1,z1);
        std::array::from_fn(|i|{let near=(a[i]*(1.0-tx)+b[i]*tx)*(1.0-ty)+(c[i]*(1.0-tx)+d[i]*tx)*ty;let far=(e[i]*(1.0-tx)+f[i]*tx)*(1.0-ty)+(g[i]*(1.0-tx)+h[i]*tx)*ty;near*(1.0-tz)+far*tz})
    }
    fn shape(&self,rgb:[f32;3])->[f32;3]{
        let Some(shaper)=&self.shaper else{return rgb;};
        std::array::from_fn(|channel|{
            let point=((rgb[channel]-shaper.min)/(shaper.max-shaper.min)).clamp(0.0,1.0)*(shaper.values.len()-1) as f32;
            let lo=point.floor() as usize;let hi=(lo+1).min(shaper.values.len()-1);let t=point-lo as f32;
            // Interpolate in f64 so large but finite file values cannot
            // overflow before clamping to the 3D input range.
            let value=f64::from(shaper.values[lo][channel])*(1.0-f64::from(t))+f64::from(shaper.values[hi][channel])*f64::from(t);
            value.clamp(f64::from(self.domain_min[channel]),f64::from(self.domain_max[channel])) as f32
        })
    }
}
#[derive(Clone,Copy,Debug)]
pub struct ColorLutSettings {pub amount:f32,pub passthrough_alpha:bool}
#[derive(Clone,Copy,Debug)]
pub struct ColorLutRequirements {pub input_bytes:usize,pub output_bytes:usize,pub texel_visits:u64}
#[derive(Clone,Copy,Debug)]
pub struct ColorLutLimits {pub max_input_bytes:usize,pub max_output_bytes:usize,pub max_texel_visits:u64}
pub fn color_lut_requirements(dimensions:(u32,u32),table_bytes:usize,settings:ColorLutSettings)->Result<ColorLutRequirements,MediaError>{
    if !settings.amount.is_finite()||!(0.0..=1.0).contains(&settings.amount)||dimensions.0==0||dimensions.1==0||dimensions.0>8192||dimensions.1>8192||table_bytes>24*1024*1024{return Err(MediaError::InvalidBuffer);}
    let pixels=(dimensions.0 as usize).checked_mul(dimensions.1 as usize).ok_or(MediaError::BudgetExceeded)?;
    let output_bytes=pixels.checked_mul(4).ok_or(MediaError::BudgetExceeded)?;
    // Reserve for the combined path: source + up to three 1D pairs + eight
    // 3D neighbors. The single-table paths simply use less than this bound.
    Ok(ColorLutRequirements {input_bytes:output_bytes.checked_add(table_bytes).ok_or(MediaError::BudgetExceeded)?,output_bytes,texel_visits:(pixels as u64).checked_mul(15).ok_or(MediaError::BudgetExceeded)?})
}
pub fn apply_color_lut(source:&RgbaFrame,ingress:SourceBlend,lut:&ColorLut,settings:ColorLutSettings,limits:ColorLutLimits)->Result<RgbaFrame,MediaError>{
    let r=color_lut_requirements((source.width(),source.height()),lut.byte_len(),settings)?;
    if r.input_bytes>limits.max_input_bytes||r.output_bytes>limits.max_output_bytes||r.texel_visits>limits.max_texel_visits{return Err(MediaError::BudgetExceeded);}
    let encoding=match ingress {SourceBlend::LinearSrgb=>ColorCorrectionInput::LinearAlpha,SourceBlend::Encoded|SourceBlend::EncodedPremultipliedSrgb=>ColorCorrectionInput::EncodedAlpha,SourceBlend::PremultipliedLinearSrgb=>ColorCorrectionInput::PremultipliedLinearSrgb};
    let domain=lut.domain_min.iter().any(|v|*v>0.0)||lut.domain_max.iter().any(|v|*v<1.0);
    let mut pixels=Vec::new();pixels.try_reserve_exact(r.output_bytes).map_err(|_|MediaError::BudgetExceeded)?;pixels.resize(r.output_bytes,0);
    for (src,dst) in source.pixels().chunks_exact(4).zip(pixels.chunks_exact_mut(4)){
        let lp=crate::obs_keyer::sample_lp([src[0],src[1],src[2],src[3]],encoding);
        let inverse=if lp[3]>0.0{1.0/lp[3]}else{0.0};
        let mut rgb=[lp[0],lp[1],lp[2]];
        if lut.resolve_single {
            for value in &mut rgb {*value*=inverse;}
            let non=rgb.map(nonlinear);
            let mapped=match lut.kind {
                LutKind::OneD=>std::array::from_fn(|i|to_linear(lut.channel(non[i],i))),
                LutKind::ThreeD=>lut.sample(non).map(to_linear),
            };
            for i in 0..3 {rgb[i]=(rgb[i]*(1.0-settings.amount)+mapped[i]*settings.amount)*lp[3];}
        }else if lut.kind==LutKind::OneD {
            for i in 0..3 {rgb[i]*=inverse;let nonlinear=nonlinear(rgb[i]);if nonlinear>=lut.domain_min[i]&&nonlinear<=lut.domain_max[i]{rgb[i]=rgb[i]*(1.0-settings.amount)+lut.channel(nonlinear,i)*settings.amount;}}
            // Exact OBS Draw1D branch returns straight RGB without repremultiply.
        }else{
            let straight=lut.has_shaper()||domain||settings.amount<1.0||!settings.passthrough_alpha;
            if straight {for value in &mut rgb{*value*=inverse;}}
            let non=rgb.map(nonlinear);
            if lut.has_shaper()||!domain||(0..3).all(|i|non[i]>=lut.domain_min[i]&&non[i]<=lut.domain_max[i]){
                let mut mapped=lut.sample(lut.shape(non));
                if lut.has_shaper(){mapped=mapped.map(to_linear);}
                for i in 0..3{rgb[i]=if domain||settings.amount<1.0{rgb[i]*(1.0-settings.amount)+mapped[i]*settings.amount}else{mapped[i]};}
            }
            if straight {for value in &mut rgb{*value*=lp[3];}}
        }
        for i in 0..3{dst[i]=encode(rgb[i]);}dst[3]=src[3];
    }
    RgbaFrame::new(source.width(),source.height(),pixels)
}
fn nonlinear(value:f32)->f32 {if value<=0.0031308{12.92*value}else{1.055*value.powf(1.0/2.4)-0.055}}
fn to_linear(value:f32)->f32 {if value<=0.04045{value/12.92}else{((value+0.055)/1.055).powf(2.4)}}
/// Nonlinear CUBE entries are linearized and quantized to finite binary16.
/// Adapted from OBS half.h / Microsoft DirectXMath; see bundled MIT notice.
pub fn cube_lut_linear_value(value:f32)->Result<f32,MediaError>{
    if !value.is_finite(){return Err(MediaError::InvalidBuffer);}
    let linear=if value<=0.04045{value/12.92}else{((value+0.055)/1.055).powf(2.4)};
    let mut bits=linear.to_bits();let sign=(bits&0x80000000)>>16;bits&=0x7fffffff;
    if bits>0x477fe000{return Err(MediaError::InvalidBuffer);}
    let rounded=if bits==0{0}else{
        if bits<0x38800000{let shift=113-(bits>>23);bits=if shift>=32{0}else{(0x800000|(bits&0x7fffff))>>shift};}else{bits=bits.wrapping_add(0xc8000000);}
        ((bits+0x0fff+((bits>>13)&1))>>13)&0x7fff
    }|sign;
    let exponent=(rounded>>10)&31;let mantissa=rounded&1023;
    Ok(if exponent==0{let value=mantissa as f32*(1.0/16777216.0);if sign!=0{-value}else{value}}else{f32::from_bits(((rounded&0x8000)<<16)|((exponent+112)<<23)|(mantissa<<13))})
}
/// PNG uses the existing float32 sRGB texture decode, ignoring LUT alpha.
pub fn png_lut_linear_value(value:u8)->f32{decode(value)}
/// Preserve the full normalized 16-bit input precision before sRGB decoding.
pub fn png_lut_linear_value_16(value:u16)->f32{to_linear(f32::from(value)/65535.0)}
