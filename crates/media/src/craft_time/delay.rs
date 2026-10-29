// SPDX-License-Identifier: MIT
//! Original SoundCraft Mod Delay equations and units; storage is paged COW.
use super::*;use super::storage::*;
#[derive(Clone,Copy,Debug,PartialEq)]pub struct ModDelayParameters{pub time_ms:f32,pub feedback_pct:f32,pub mix_pct:f32,pub mod_rate_hz:f32,pub mod_depth_pct:f32,pub lowpass_hz:f32}
impl Default for ModDelayParameters{fn default()->Self{Self{time_ms:250.,feedback_pct:30.,mix_pct:30.,mod_rate_hz:0.5,mod_depth_pct:0.,lowpass_hz:12000.}}}
impl ModDelayParameters{pub fn validate(self)->Result<(),CraftTimeError>{for(name,value,min,max)in[("time_ms",self.time_ms,1.,2000.),("feedback_pct",self.feedback_pct,-99.,99.),("mix_pct",self.mix_pct,0.,100.),("mod_rate_hz",self.mod_rate_hz,0.01,10.),("mod_depth_pct",self.mod_depth_pct,0.,100.),("lowpass_hz",self.lowpass_hz,500.,20000.)]{range(name,value,min,max)?;}Ok(())}}
#[derive(Clone)]pub struct ModDelay{rate:f32,channels:usize,params:ModDelayParameters,lines:Vec<Buffer>,lp:Vec<f32>,w:usize,phase:f32,time:Exponential,fb:Exponential,mix:Exponential,depth:Exponential,lp_coef:f32,bytes:usize,poisoned:bool}
impl ModDelay{
 fn len(rate:u32)->Result<usize,CraftTimeError>{let n=(2010.0f32*0.001*rate as f32)as usize; n.checked_add(12).and_then(usize::checked_next_power_of_two).ok_or(CraftTimeError::Budget)}
 pub fn required_bytes(rate:u32,channels:u16)->Result<usize,CraftTimeError>{format(rate,channels)?;Buffer::bytes(Self::len(rate)?)?.checked_mul(usize::from(channels)).and_then(|n|n.checked_add(usize::from(channels)*(std::mem::size_of::<Buffer>()+4)+std::mem::size_of::<Self>())).ok_or(CraftTimeError::Budget)}
 pub fn new(rate:u32,channels:u16,params:ModDelayParameters,limits:CraftTimeLimits)->Result<Self,CraftTimeError>{
  params.validate()?;let bytes=Self::required_bytes(rate,channels)?;if bytes>limits.max_state_bytes{return Err(CraftTimeError::Budget);}
  let mut lines=Vec::new();lines.try_reserve_exact(usize::from(channels)).map_err(|_|CraftTimeError::Budget)?;
  for _ in 0..channels{lines.push(Buffer::new(Self::len(rate)?)?);}
  let rate=rate as f32;let mut value=Self{rate,channels:usize::from(channels),params,lines,lp:vec![0.;usize::from(channels)],w:0,phase:0.,
   time:Exponential::new(params.time_ms,120.,rate),fb:Exponential::new(params.feedback_pct/100.,20.,rate),mix:Exponential::new(params.mix_pct/100.,20.,rate),depth:Exponential::new(params.mod_depth_pct/100.*5.,20.,rate),lp_coef:0.,bytes,poisoned:false};
  value.set_parameters(params)?;Ok(value)
 }
 pub fn set_parameters(&mut self,p:ModDelayParameters)->Result<(),CraftTimeError>{p.validate()?;self.params=p;self.time.set(p.time_ms);self.fb.set(p.feedback_pct/100.);self.mix.set(p.mix_pct/100.);self.depth.set(p.mod_depth_pct/100.*5.);self.lp_coef=(-std::f32::consts::TAU*p.lowpass_hz/self.rate).exp();Ok(())}
 pub fn state_bytes(&self)->usize{self.bytes}
 pub fn reset(&mut self){for line in &mut self.lines{line.clear();}self.lp.fill(0.);self.w=0;self.phase=0.;self.time.snap();self.fb.snap();self.mix.snap();self.depth.snap();self.poisoned=false;}
 pub fn tail_samples(&self)->u64{let fb=(self.params.feedback_pct.abs()/100.).clamp(0.,0.99);let repeats=if fb<1e-3{1.}else{(-60./(20.*fb.log10())).min(500.)+1.};(self.params.time_ms*repeats*0.001*self.rate)as u64}
 pub fn process(&mut self,samples:&mut[f32],limits:CraftTimeLimits)->Result<(),CraftTimeError>{
  if self.poisoned{return Err(CraftTimeError::Poisoned);}let frames=admit(samples,self.channels,self.bytes,self.channels*48+8,limits)?;
  let len=self.lines[0].len();let mask=len-1;let inc=(self.params.mod_rate_hz/self.rate).clamp(0.,0.5);let a=self.lp_coef;
  for n in 0..frames{
   self.phase+=inc;if self.phase>=1.{self.phase-=1.;}
   let t=self.time.next();let fb=self.fb.next();let mix=self.mix.next();let depth=self.depth.next();
   for c in 0..self.channels{
    let dry=samples[n*self.channels+c];let mut p=self.phase+if c%2==1{0.25}else{0.};if p>=1.{p-=1.;}
    let modv=depth*0.5*(1.+(p*std::f32::consts::TAU).sin());let d=((t+modv)*0.001*self.rate).clamp(2.,(len-4)as f32);
    let i=d as usize;let f=d-i as f32;let tap=|distance:usize|self.lines[c].at(self.w.wrapping_sub(distance)&mask);
    let xm1=tap(i-1);let x0=tap(i);let x1=tap(i+1);let x2=tap(i+2);
    let c1=0.5*(x1-xm1);let c2=xm1-2.5*x0+2.*x1-0.5*x2;let c3=0.5*(x2-xm1)+1.5*(x0-x1);
    let y=((c3*f+c2)*f+c1)*f+x0;
    self.lp[c]=flush_sc(y+(self.lp[c]-y)*a);self.lines[c].set(self.w,flush_sc(dry+fb*self.lp[c]));
    samples[n*self.channels+c]=dry*(1.-mix)+self.lp[c]*mix;
   }self.w=(self.w+1)&mask;
  }
  if samples.iter().any(|v|!v.is_finite()){self.poisoned=true;return Err(CraftTimeError::NonFinite);}Ok(())
 }
}
