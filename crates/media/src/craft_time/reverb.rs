// SPDX-License-Identifier: MIT
//! FilmCraft algorithmic8-line Householder FDN, original smoothing and gain grid.
use super::*;use super::storage::*;
const FDN:usize=8;
const BASE_MS:[f32;8]=[29.7,37.1,41.1,43.7,53.9,59.3,67.1,73.3];
const SIGN_L:[f32;8]=[1.,-1.,1.,-1.,1.,1.,-1.,-1.];
const SIGN_R:[f32;8]=[1.,1.,-1.,-1.,1.,-1.,1.,-1.];
#[derive(Clone,Copy,Debug,PartialEq)]pub struct FdnReverbParameters{pub predelay_ms:f32,pub decay_seconds:f32,pub damping_pct:f32,pub size_pct:f32,pub mix_pct:f32}
impl Default for FdnReverbParameters{fn default()->Self{Self{predelay_ms:20.,decay_seconds:2.,damping_pct:40.,size_pct:50.,mix_pct:25.}}}
impl FdnReverbParameters{pub fn validate(self)->Result<(),CraftTimeError>{for(name,value,min,max)in[("predelay_ms",self.predelay_ms,0.,200.),("decay_seconds",self.decay_seconds,0.1,20.),("damping_pct",self.damping_pct,0.,100.),("size_pct",self.size_pct,0.,100.),("mix_pct",self.mix_pct,0.,100.)]{range(name,value,min,max)?;}Ok(())}}
#[derive(Clone)]pub struct FdnReverb{
 rate:f32,channels:usize,params:FdnReverbParameters,lines:Vec<Buffer>,w:usize,len:[Linear;FDN],lp:[f32;FDN],gains:[f32;FDN],
 gain_key:([f32;FDN],f32),damp:Linear,rt60:Linear,mix:Linear,pre:Buffer,pre_w:usize,pre_len:Linear,counter:u32,bytes:usize,poisoned:bool,
}
impl FdnReverb{
 fn lengths(rate:u32)->(usize,usize){(((BASE_MS[FDN-1]*2.0*0.001*rate as f32)as usize)+4,((0.2*rate as f32)as usize)+4)}
 pub fn required_bytes(rate:u32,channels:u16)->Result<usize,CraftTimeError>{
  format(rate,channels)?;let(line,pre)=Self::lengths(rate);
  Buffer::bytes(line)?.checked_mul(FDN).and_then(|n|n.checked_add(Buffer::bytes(pre).ok()?))
   .and_then(|n|n.checked_add(FDN*std::mem::size_of::<Buffer>()+std::mem::size_of::<Self>())).ok_or(CraftTimeError::Budget)
 }
 pub fn new(rate:u32,channels:u16,params:FdnReverbParameters,limits:CraftTimeLimits)->Result<Self,CraftTimeError>{
  params.validate()?;let bytes=Self::required_bytes(rate,channels)?;if bytes>limits.max_state_bytes{return Err(CraftTimeError::Budget);}
  let(line,pre)=Self::lengths(rate);let mut lines=Vec::new();lines.try_reserve_exact(FDN).map_err(|_|CraftTimeError::Budget)?;
  for _ in 0..FDN{lines.push(Buffer::new(line)?);}
  let rate=rate as f32;let sm=|value|Linear::new(value,rate,50.);
  let mut value=Self{rate,channels:usize::from(channels),params,lines,w:0,len:[sm(1.);FDN],lp:[0.;FDN],gains:[0.;FDN],gain_key:([0.;FDN],0.),
   damp:sm(0.),rt60:sm(1.),mix:sm(0.),pre:Buffer::new(pre)?,pre_w:0,pre_len:Linear::new(1.,rate,100.),counter:0,bytes,poisoned:false};
  value.set_parameters(params)?;for length in &mut value.len{length.snap();}value.pre_len.snap();value.damp.snap();value.rt60.snap();value.mix.snap();value.update_gains();Ok(value)
 }
 pub fn set_parameters(&mut self,p:FdnReverbParameters)->Result<(),CraftTimeError>{
  p.validate()?;self.params=p;let scale=0.35+(2.0-0.35)*p.size_pct/100.;
  for (index,length) in self.len.iter_mut().enumerate(){length.set(BASE_MS[index]*scale*0.001*self.rate);}
  self.pre_len.set((p.predelay_ms*0.001*self.rate).max(1.));self.damp.set(p.damping_pct/100.*0.85);self.rt60.set(p.decay_seconds);self.mix.set(p.mix_pct/100.);Ok(())
 }
 fn update_gains(&mut self){
  let rt=self.rt60.value().max(0.01);let lengths=self.len.map(Linear::value);
  if self.gain_key==(lengths,rt){return;}self.gain_key=(lengths,rt);
  for k in 0..FDN{self.gains[k]=10f32.powf(-3.*self.len[k].value()/(rt*self.rate));}
 }
 pub fn state_bytes(&self)->usize{self.bytes}
 pub fn reset(&mut self){for line in &mut self.lines{line.clear();}self.pre.clear();self.lp=[0.;FDN];self.w=0;self.pre_w=0;self.counter=0;self.poisoned=false;}
 pub fn nominal_tail_samples(&self)->u64{((self.params.decay_seconds+self.params.predelay_ms*0.001)*self.rate)as u64}
 pub fn process(&mut self,samples:&mut[f32],limits:CraftTimeLimits)->Result<(),CraftTimeError>{
  if self.poisoned{return Err(CraftTimeError::Poisoned);}let frames=admit(samples,self.channels,self.bytes,256+self.channels*16,limits)?;
  let line_len=self.lines[0].len();let pre_len=self.pre.len();let hh=2.0/FDN as f32;
  let fixed=self.len.iter().all(|length|!length.smoothing());let delays=self.len.map(|length|delay_parts(line_len,length.value()));
  let predelay=(!self.pre_len.smoothing()).then(||delay_parts(pre_len,self.pre_len.value()));
  for i in 0..frames{
   if self.counter==0{self.update_gains();}self.counter=(self.counter+1)%32;
   if !fixed{for length in &mut self.len{length.tick();}}
   self.rt60.tick();let damp=self.damp.tick();let mix=self.mix.tick();let pd=self.pre_len.tick();
   let mut mono=0.;for ch in 0..self.channels{mono+=samples[i*self.channels+ch];}mono/=self.channels as f32;
   self.pre.set(self.pre_w,mono);let input=read(&self.pre,self.pre_w,predelay.unwrap_or_else(||delay_parts(pre_len,pd)));
   self.pre_w=if self.pre_w+1==pre_len{0}else{self.pre_w+1};
   let mut v=[0.;FDN];let mut sum=0.;
   for k in 0..FDN{let part=if fixed{delays[k]}else{delay_parts(line_len,self.len[k].value())};
    let d=read(&self.lines[k],self.w,part);self.lp[k]=flush_fc(d+(self.lp[k]-d)*damp);v[k]=self.lp[k]*self.gains[k];sum+=v[k];
   }
   let(mut wl,mut wr)=(0.,0.);
   for k in 0..FDN{let fbk=v[k]-hh*sum;self.lines[k].set(self.w,flush_fc(fbk+input*0.5));wl+=SIGN_L[k]*v[k];wr+=SIGN_R[k]*v[k];}
   self.w=if self.w+1==line_len{0}else{self.w+1};let(wl,wr)=(wl*0.35,wr*0.35);
   for c in 0..self.channels{let x=samples[i*self.channels+c];let wet=if self.channels==1{0.5*(wl+wr)}else if c%2==0{wl}else{wr};samples[i*self.channels+c]=x+mix*(wet-x);}
  }
  if samples.iter().any(|v|!v.is_finite()){self.poisoned=true;return Err(CraftTimeError::NonFinite);}Ok(())
 }
}
