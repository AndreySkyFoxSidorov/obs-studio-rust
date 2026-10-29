//! EffectCraft Gaussian Blur, pinned991904888a20b7da72b2a4880b21c7d912154736.
//! Adapted from effects/misc.rs and raster/blur.rs. SPDX-License-Identifier: MIT.
//! Copyright (c) 2026 ArtCraft Team and the EffectCraft contributors.
//! Selected linear working-space SDR adaptation; no adjustment-layer mode.
use crate::{ColorCorrectionInput,MediaError,SourceBlend};
use crate::color_correction::{encode,unorm};

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub enum GaussianBlurDimensions {#[default] Both,Horizontal,Vertical}
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct GaussianBlurSettings {pub blurriness:f32,pub dimensions:GaussianBlurDimensions,pub repeat_edge:bool}
impl Default for GaussianBlurSettings {fn default()->Self{Self{blurriness:0.0,dimensions:GaussianBlurDimensions::Both,repeat_edge:false}}}
impl GaussianBlurSettings {
    pub fn validate(self)->Result<(),MediaError>{if self.blurriness.is_finite()&&(0.0..=3000.0).contains(&self.blurriness){Ok(())}else{Err(MediaError::InvalidBuffer)}}
    pub fn is_identity(self)->bool{self.blurriness==0.0}
}
#[derive(Clone,Copy,Default)]
struct Pass {vertical:bool,radius:usize,sigma:f64,sampled:bool}
pub(crate) struct Plan {
    pub dimensions:(u32,u32),pub workspace_bytes:usize,pub pixel_stages:usize,pub texel_visits:u64,
    pad:usize,passes:[Pass;6],count:usize,
}
fn add(a:u64,b:u64)->Result<u64,MediaError>{a.checked_add(b).ok_or(MediaError::Overflow)}
fn mul(a:u64,b:u64)->Result<u64,MediaError>{a.checked_mul(b).ok_or(MediaError::Overflow)}
/// Metadata-only: no raster, kernel Vec, transcendental tap evaluation or thread.
pub(crate) fn plan(input:(u32,u32),settings:GaussianBlurSettings)->Result<Plan,MediaError>{
    settings.validate()?;
    if input.0==0||input.1==0||input.0>8192||input.1>8192{return Err(MediaError::InvalidDimensions);}
    let sigma=f64::from(settings.blurriness)*0.5;
    let pad=if settings.repeat_edge||settings.is_identity(){0}else{(sigma*3.0).ceil() as usize};
    let w=(input.0 as usize).checked_add(pad*2).ok_or(MediaError::Overflow)?;
    let h=(input.1 as usize).checked_add(pad*2).ok_or(MediaError::Overflow)?;
    let pixels=w.checked_mul(h).ok_or(MediaError::Overflow)?;
    if w>8192||h>8192||pixels>32*1024*1024{return Err(MediaError::BudgetExceeded);}
    let mut result=Plan{dimensions:(w as u32,h as u32),workspace_bytes:0,pixel_stages:0,texel_visits:0,pad,passes:[Pass::default();6],count:0};
    if settings.is_identity(){return Ok(result);}
    let sampled=sigma<3.0;
    for (vertical,active) in [(false,settings.dimensions!=GaussianBlurDimensions::Vertical),(true,settings.dimensions!=GaussianBlurDimensions::Horizontal)]{
        if !active{continue;}
        if sampled {
            // The wrapper pads even when this inner donor pass is the identity.
            if sigma>0.001{result.passes[result.count]=Pass{vertical,radius:(sigma*3.0).ceil() as usize,sigma,sampled:true};result.count+=1;}
        }else if sigma>0.05 {
            let wl0=(12.0*sigma*sigma/3.0+1.0).sqrt().floor() as i64;
            let wl=if wl0%2==0{wl0-1}else{wl0};let wu=wl+2;
            let m=((12.0*sigma*sigma-(3*wl*wl) as f64-12.0*wl as f64-9.0)/(-4.0*wl as f64-4.0)).round() as i64;
            for i in 0..3 {let radius=(((if i<m{wl}else{wu})-1)/2).max(0) as usize;if radius!=0{result.passes[result.count]=Pass{vertical,radius,sigma,sampled:false};result.count+=1;}}
        }
    }
    // One existing u32 workspace; each word stores an exact f32 bit pattern.
    // The fixed <=19 f64 taps live on the stack and are conservatively charged.
    result.workspace_bytes=pixels.checked_mul(32).and_then(|v|v.checked_add(w*16+19*8)).ok_or(MediaError::Overflow)?;
    let n=pixels as u64;let input_pixels=u64::from(input.0)*u64::from(input.1);
    let mut visits=add(mul(n,3)?,input_pixels)?; // both-plane initialization, ingress, egress
    let mut work=visits;
    for pass in &result.passes[..result.count]{
        work=add(work,n)?;
        let window=(pass.radius*2+1) as u64;
        let samples=if pass.sampled{mul(n,window)?}else if pass.vertical{
            let block=(4*pass.radius).clamp(32,256);let blocks=h.div_ceil(block) as u64;
            // Initial accumulator + two samples/update + one accumulator output.
            add(mul(mul(blocks,window)?,w as u64)?,mul(n,3)?)?
        }else{add(mul(h as u64,window)?,mul(n,3)?)?};
        visits=add(visits,samples)?;
    }
    result.pixel_stages=usize::try_from(work).map_err(|_|MediaError::Overflow)?;result.texel_visits=visits;Ok(result)
}
fn load(src:&[u32],pixel:usize)->[f32;4]{std::array::from_fn(|c|f32::from_bits(src[pixel*4+c]))}
fn store(dst:&mut[u32],pixel:usize,value:[f32;4]){for c in 0..4{dst[pixel*4+c]=value[c].to_bits();}}
fn sample(src:&[u32],w:usize,h:usize,x:isize,y:isize,repeat:bool)->[f32;4]{
    if repeat{return load(src,y.clamp(0,h as isize-1) as usize*w+x.clamp(0,w as isize-1) as usize);}
    if x<0||y<0||x>=w as isize||y>=h as isize{[0.0;4]}else{load(src,y as usize*w+x as usize)}
}
fn sampled(src:&[u32],dst:&mut[u32],w:usize,h:usize,pass:Pass,repeat:bool){
    let mut kernel=[0.0f64;19];let radius=pass.radius as isize;let count=pass.radius*2+1;
    let mut total=0.0;
    for (index,tap) in kernel[..count].iter_mut().enumerate(){let i=index as isize-radius;*tap=(-((i*i) as f64)/(2.0*pass.sigma*pass.sigma)).exp();total+=*tap;}
    for tap in &mut kernel[..count]{*tap/=total;}
    for y in 0..h{for x in 0..w{
        let mut sum=[0.0f64;4];
        for (i,weight) in kernel[..count].iter().enumerate(){let d=i as isize-radius;let (sx,sy)=if pass.vertical{(x as isize,y as isize+d)}else{(x as isize+d,y as isize)};
            let p=sample(src,w,h,sx,sy,repeat);for c in 0..4{sum[c]+=p[c] as f64*weight;}}
        store(dst,y*w+x,sum.map(|v|v as f32));
    }}
}
fn box_horizontal(src:&[u32],dst:&mut[u32],w:usize,h:usize,r:usize,repeat:bool){
    let norm=1.0/(2*r+1) as f32;let radius=r as isize;
    for y in 0..h{
        let mut acc=[0.0f32;4];
        for i in -radius..=radius{let p=sample(src,w,h,i,y as isize,repeat);for c in 0..4{acc[c]+=p[c];}}
        for x in 0..w{
            store(dst,y*w+x,acc.map(|v|v*norm));
            let a=sample(src,w,h,x as isize+radius+1,y as isize,repeat);let b=sample(src,w,h,x as isize-radius,y as isize,repeat);
            for c in 0..4{acc[c]+=a[c]-b[c];}
        }
    }
}
fn row_index(y:isize,h:usize,repeat:bool)->Option<usize>{if repeat{Some(y.clamp(0,h as isize-1) as usize)}else if y>=0&&y<h as isize{Some(y as usize)}else{None}}
fn box_vertical(src:&[u32],dst:&mut[u32],row:&mut[u32],w:usize,h:usize,r:usize,repeat:bool){
    let norm=1.0/(2*r+1) as f32;let radius=r as isize;let block=(4*r).clamp(32,256);
    for ya in (0..h).step_by(block){
        row.fill(0);
        for y in ya as isize-radius..=ya as isize+radius{if let Some(y)=row_index(y,h,repeat){for x in 0..w{let mut acc=load(row,x);let p=load(src,y*w+x);for c in 0..4{acc[c]+=p[c];}store(row,x,acc);}}}
        for y in ya..h.min(ya+block){
            for x in 0..w{store(dst,y*w+x,load(row,x).map(|v|v*norm));}
            let a=row_index(y as isize+radius+1,h,repeat);let b=row_index(y as isize-radius,h,repeat);
            for x in 0..w{let mut acc=load(row,x);match(a,b){
                (Some(a),Some(b))=>{let p=load(src,a*w+x);let q=load(src,b*w+x);for c in 0..4{acc[c]+=p[c]-q[c];}},
                (Some(a),None)=>{let p=load(src,a*w+x);for c in 0..4{acc[c]+=p[c];}},
                (None,Some(b))=>{let q=load(src,b*w+x);for c in 0..4{acc[c]-=q[c];}},
                (None,None)=>{},
            }store(row,x,acc);}
        }
    }
}
/// Consumes no native frame and allocates only the admitted final RGBA8 raster.
/// The caller owns both float planes/row through the existing shared workspace.
pub(crate) fn apply(input:&[u8],width:u32,height:u32,ingress:SourceBlend,settings:GaussianBlurSettings,workspace:&mut[u32])->Result<(Vec<u8>,u32,u32),MediaError>{
    let plan=plan((width,height),settings)?;
    if settings.is_identity(){return Err(MediaError::InvalidBuffer);} // caller skips before conversion
    let input_len=(width as usize).checked_mul(height as usize).and_then(|v|v.checked_mul(4)).ok_or(MediaError::Overflow)?;
    if input.len()!=input_len||workspace.len()<plan.workspace_bytes/4{return Err(MediaError::InvalidBuffer);}
    let (w,h)=(plan.dimensions.0 as usize,plan.dimensions.1 as usize);let words=w.checked_mul(h).and_then(|v|v.checked_mul(4)).ok_or(MediaError::Overflow)?;
    let (mut a,rest)=workspace.split_at_mut(words);let(mut b,row)=rest.split_at_mut(words);let row=&mut row[..w*4];
    a.fill(0);b.fill(0);
    let encoding=match ingress{SourceBlend::LinearSrgb=>ColorCorrectionInput::LinearAlpha,SourceBlend::Encoded|SourceBlend::EncodedPremultipliedSrgb=>ColorCorrectionInput::EncodedAlpha,SourceBlend::PremultipliedLinearSrgb=>ColorCorrectionInput::PremultipliedLinearSrgb};
    for (i,p) in input.chunks_exact(4).enumerate(){let value=crate::obs_keyer::sample_lp([p[0],p[1],p[2],p[3]],encoding);let x=i%width as usize+plan.pad;let y=i/width as usize+plan.pad;store(a,y*w+x,value);}
    for pass in &plan.passes[..plan.count]{
        if pass.sampled{sampled(a,b,w,h,*pass,settings.repeat_edge);}else if pass.vertical{box_vertical(a,b,row,w,h,pass.radius,settings.repeat_edge);}else{box_horizontal(a,b,w,h,pass.radius,settings.repeat_edge);}
        std::mem::swap(&mut a,&mut b);
    }
    let mut output=Vec::new();output.try_reserve_exact(words).map_err(|_|MediaError::BudgetExceeded)?;output.resize(words,0);
    for (i,p) in output.chunks_exact_mut(4).enumerate(){let value=load(a,i);for c in 0..3{p[c]=encode(value[c]);}p[3]=unorm(value[3]);}
    Ok((output,plan.dimensions.0,plan.dimensions.1))
}
