// SPDX-License-Identifier: MIT
//! SoundCraft Mod Delay, Chorus, Flanger and Phaser; delay storage is paged COW.
use super::*;use super::storage::*;
#[derive(Clone,Copy,Debug,PartialEq)]pub struct ModDelayParameters{pub time_ms:f32,pub feedback_pct:f32,pub mix_pct:f32,pub mod_rate_hz:f32,pub mod_depth_pct:f32,pub lowpass_hz:f32,pub mode:u8,pub stereo_spread:f32,pub phaser_stages:u8}
impl Default for ModDelayParameters{fn default()->Self{Self{time_ms:250.,feedback_pct:30.,mix_pct:30.,mod_rate_hz:0.5,mod_depth_pct:0.,lowpass_hz:12000.,mode:0,stereo_spread:100.,phaser_stages:6}}}
impl ModDelayParameters{pub fn validate(self)->Result<(),CraftTimeError>{if self.mode>3{return Err(CraftTimeError::InvalidParameter("mode"));}if ![2,4,6,8,12].contains(&self.phaser_stages){return Err(CraftTimeError::InvalidParameter("phaser_stages"));}for(name,value,min,max)in[("time_ms",self.time_ms,if self.mode==2{0.1}else{1.},2000.),("feedback_pct",self.feedback_pct,-99.,99.),("mix_pct",self.mix_pct,0.,100.),("mod_rate_hz",self.mod_rate_hz,0.01,10.),("mod_depth_pct",self.mod_depth_pct,0.,100.),("lowpass_hz",self.lowpass_hz,if self.mode==3{100.}else{500.},20000.),("stereo_spread",self.stereo_spread,0.,100.)]{range(name,value,min,max)?;}Ok(())}}
#[derive(Clone)]pub struct ModDelay{rate:f32,channels:usize,params:ModDelayParameters,lines:Vec<Buffer>,lp:Vec<f32>,phaser:Vec<[f32;12]>,w:usize,phase:f32,time:Exponential,fb:Exponential,mix:Exponential,depth:Exponential,center:Exponential,lp_coef:f32,bytes:usize,poisoned:bool}
impl ModDelay{
 fn len(rate:u32)->Result<usize,CraftTimeError>{let n=(2010.0f32*0.001*rate as f32)as usize; n.checked_add(12).and_then(usize::checked_next_power_of_two).ok_or(CraftTimeError::Budget)}
 pub fn required_bytes(rate:u32,channels:u16)->Result<usize,CraftTimeError>{format(rate,channels)?;Buffer::bytes(Self::len(rate)?)?.checked_mul(usize::from(channels)).and_then(|n|n.checked_add(usize::from(channels)*(std::mem::size_of::<Buffer>()+4+std::mem::size_of::<[f32;12]>())+std::mem::size_of::<Self>())).ok_or(CraftTimeError::Budget)}
 pub fn new(rate:u32,channels:u16,params:ModDelayParameters,limits:CraftTimeLimits)->Result<Self,CraftTimeError>{
  params.validate()?;let bytes=Self::required_bytes(rate,channels)?;if bytes>limits.max_state_bytes{return Err(CraftTimeError::Budget);}
  let mut lines=Vec::new();lines.try_reserve_exact(usize::from(channels)).map_err(|_|CraftTimeError::Budget)?;
  for _ in 0..channels{lines.push(Buffer::new(Self::len(rate)?)?);}
  let rate=rate as f32;let mut value=Self{rate,channels:usize::from(channels),params,lines,lp:vec![0.;usize::from(channels)],phaser:vec![[0.;12];usize::from(channels)],w:0,phase:0.,
   time:Exponential::new(params.time_ms,if params.mode==0{120.}else{30.},rate),fb:Exponential::new(params.feedback_pct/100.,if params.mode==0{20.}else{30.},rate),mix:Exponential::new(params.mix_pct/100.,if params.mode==0{20.}else{30.},rate),depth:Exponential::new(params.mod_depth_pct/100.,if params.mode==0{20.}else{30.},rate),center:Exponential::new(params.lowpass_hz,30.,rate),lp_coef:0.,bytes,poisoned:false};
  value.set_parameters(params)?;Ok(value)
 }
 pub fn set_parameters(&mut self,p:ModDelayParameters)->Result<(),CraftTimeError>{p.validate()?;let reset=self.params.mode!=p.mode;self.params=p;if reset{let time=if p.mode==0{120.}else{30.};let other=if p.mode==0{20.}else{30.};self.time=Exponential::new(p.time_ms,time,self.rate);self.fb=Exponential::new(p.feedback_pct/100.,other,self.rate);self.mix=Exponential::new(p.mix_pct/100.,other,self.rate);self.depth=Exponential::new(p.mod_depth_pct/100.,other,self.rate);}self.time.set(p.time_ms);self.fb.set(p.feedback_pct/100.);self.mix.set(p.mix_pct/100.);self.depth.set(p.mod_depth_pct/100.);self.center.set(p.lowpass_hz);self.lp_coef=(-std::f32::consts::TAU*p.lowpass_hz/self.rate).exp();if reset{self.reset();}Ok(())}
 pub fn state_bytes(&self)->usize{self.bytes}
 pub fn reset(&mut self){for line in &mut self.lines{line.clear();}self.lp.fill(0.);self.phaser.fill([0.;12]);self.w=0;self.phase=0.;self.time.snap();self.fb.snap();self.mix.snap();self.depth.snap();self.center.snap();self.poisoned=false;}
 pub fn tail_samples(&self)->u64{let fb=(self.params.feedback_pct.abs()/100.).clamp(0.,0.99);let repeats=if fb<1e-3{1.}else{(-60./(20.*fb.log10())).min(500.)+1.};let longest=if self.params.mode==1{self.params.time_ms+self.params.mod_depth_pct/100.*6.}else{self.params.time_ms};let ms=if self.params.mode==0{longest*repeats}else{(longest*repeats).max(200.)};(ms*0.001*self.rate)as u64}
 pub fn process(&mut self,samples:&mut[f32],limits:CraftTimeLimits)->Result<(),CraftTimeError>{
  if self.poisoned{return Err(CraftTimeError::Poisoned);}let frames=admit(samples,self.channels,self.bytes,self.channels*72+8,limits)?;
  let len=self.lines[0].len();let mask=len-1;let inc=(self.params.mod_rate_hz/self.rate).clamp(0.,0.5);let a=self.lp_coef;
  for n in 0..frames{
   self.phase+=inc;if self.phase>=1.{self.phase-=1.;}
   let t=self.time.next();let fb=self.fb.next();let mix=self.mix.next();let depth=self.depth.next();let center=self.center.next();
   for c in 0..self.channels{
    let dry=samples[n*self.channels+c];let spread=if self.params.mode==1{0.25*self.params.stereo_spread/100.}else{0.25};let mut p=self.phase+if c%2==1{spread}else{0.};if p>=1.{p-=1.;}
    let sine=(p*std::f32::consts::TAU).sin();
    if self.params.mode==3 {
     let frequency=(center*2f32.powf(2.*depth*sine)).clamp(20.,self.rate*0.45);
     let t=(std::f32::consts::PI*frequency/self.rate).tan();let coefficient=(t-1.)/(t+1.);
     let mut wet=dry+fb*self.lp[c];
     for state in self.phaser[c].iter_mut().take(usize::from(self.params.phaser_stages)) {let output=coefficient*wet+*state;*state=flush_sc(wet-coefficient*output);wet=output;}
     self.lp[c]=flush_sc(wet);samples[n*self.channels+c]=dry*(1.-mix)+wet*mix;continue;
    }
    let ms=match self.params.mode{1=>t+depth*6.*0.5*(1.+sine),2=>(t*(1.-depth*0.5*(1.-sine))).max(0.05),_=>t+depth*5.*0.5*(1.+sine)};
    let d=(ms*0.001*self.rate).clamp(2.,(len-4)as f32);
    if self.params.mode!=0{self.lines[c].set(self.w,flush_sc(dry+fb*self.lp[c]));}
    let i=d as usize;let f=d-i as f32;let read_head=if self.params.mode==0{self.w}else{self.w.wrapping_add(1)};let tap=|distance:usize|self.lines[c].at(read_head.wrapping_sub(distance)&mask);
    let xm1=tap(i-1);let x0=tap(i);let x1=tap(i+1);let x2=tap(i+2);
    let c1=0.5*(x1-xm1);let c2=xm1-2.5*x0+2.*x1-0.5*x2;let c3=0.5*(x2-xm1)+1.5*(x0-x1);
    let y=((c3*f+c2)*f+c1)*f+x0;
    if self.params.mode==0{self.lp[c]=flush_sc(y+(self.lp[c]-y)*a);self.lines[c].set(self.w,flush_sc(dry+fb*self.lp[c]));}else{self.lp[c]=flush_sc(y);}
    samples[n*self.channels+c]=dry*(1.-mix)+self.lp[c]*mix;
   }self.w=(self.w+1)&mask;
  }
  if samples.iter().any(|v|!v.is_finite()){self.poisoned=true;return Err(CraftTimeError::NonFinite);}Ok(())
 }
}
