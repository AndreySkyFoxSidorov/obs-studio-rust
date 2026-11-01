//! SoundCraft room/plate reverb: pre-delay, diffusion and modulated 8-line FDN.
//! Adapted from SoundCraft `crates/dsp/src/plugins/reverb.rs` (MIT OR Apache-2.0).
use super::AudioError;
use skylive_core::AudioFilter;

const ROOM:[f32;8]=[23.13,27.71,31.87,36.29,41.33,45.67,51.09,56.93];
const PLATE:[f32;8]=[13.71,17.93,21.07,24.73,28.31,32.89,36.13,40.31];
const ROOM_DIFF:[f32;4]=[4.71,3.59,12.73,9.31];
const PLATE_DIFF:[f32;4]=[3.13,4.27,7.93,11.29];

#[derive(Clone)] struct Delay { data:Vec<f32>, pos:usize }
impl Delay {
 fn length(samples:f32)->Result<usize,AudioError>{
  let n=(samples.max(1.) as usize).checked_add(8).ok_or(AudioError::Limit("reverb delay overflow"))?;
  n.checked_next_power_of_two().ok_or(AudioError::Limit("reverb delay overflow"))
 }
 fn new(samples:f32)->Result<Self,AudioError>{let len=Self::length(samples)?;Ok(Self{data:vec![0.;len],pos:0})}
 fn push(&mut self,x:f32){self.data[self.pos]=if x.is_finite(){x}else{0.};self.pos=(self.pos+1)&(self.data.len()-1);}
 fn read(&self,n:f32)->f32 {let n=n.clamp(1.,(self.data.len()-3)as f32);let i=n as usize;let f=n-i as f32;let mask=self.data.len()-1;let a=self.data[self.pos.wrapping_sub(i)&mask];let b=self.data[self.pos.wrapping_sub(i+1)&mask];a+(b-a)*f}
 fn clear(&mut self){self.data.fill(0.);self.pos=0;}
}
#[derive(Clone)] struct Allpass{data:Vec<f32>,pos:usize}
impl Allpass {
 fn new(n:usize)->Self{Self{data:vec![0.;n.max(1)],pos:0}}
 fn process(&mut self,x:f32,g:f32)->f32{let d=self.data[self.pos];self.data[self.pos]=x+g*d;self.pos=(self.pos+1)%self.data.len();d-g*(x+g*d)}
 fn clear(&mut self){self.data.fill(0.);self.pos=0;}
}
#[derive(Clone,Copy,Default)] struct Highpass{b:[f32;3],a:[f32;2],z:[f32;2]}
impl Highpass {
 fn configure(&mut self,rate:f32,hz:f32){let w=std::f32::consts::TAU*hz.clamp(1.,rate*0.49)/rate;let(s,c)=w.sin_cos();let a=s/(2.*std::f32::consts::FRAC_1_SQRT_2);let d=1.+a;self.b=[(1.+c)*0.5/d,-(1.+c)/d,(1.+c)*0.5/d];self.a=[-2.*c/d,(1.-a)/d];}
 fn sample(&mut self,x:f32)->f32{let y=self.b[0]*x+self.z[0];self.z=[self.b[1]*x-self.a[0]*y+self.z[1],self.b[2]*x-self.a[1]*y];if y.is_finite()&&self.z.iter().all(|v|v.is_finite()){y}else{self.z=[0.;2];0.}}
}
#[derive(Clone,Copy)]struct Smooth{value:f32,target:f32,coef:f32}
impl Smooth{fn new(v:f32,ms:f32,rate:f32)->Self{Self{value:v,target:v,coef:(-1./(ms*0.001*rate).max(1.)).exp()}} fn next(&mut self)->f32{self.value=self.target+(self.value-self.target)*self.coef;self.value} fn snap(&mut self){self.value=self.target;}}

#[derive(Clone)]pub(super) struct SoundReverb {
 rate:f32, channels:usize, plate:bool, pre:[Delay;2], diff:[[Allpass;4];2], lines:[Delay;8],
 lowcut:[Highpass;2], lp:[f32;8], gains:[f32;8], mix:Smooth, size:Smooth, predelay:Smooth, width:Smooth,
 decay:f32,damping:f32,diffusion:f32,lfo:f32,gain_counter:u8,bytes:usize,
}
impl SoundReverb {
 pub(super) fn required_bytes(rate:u32,channels:u16)->Result<usize,AudioError>{
  if rate==0||channels==0{return Err(AudioError::InvalidRequest("invalid sound reverb format"));}let r=rate as f32;
  // Reserve for the larger Room and Plate geometries, including 2x diffusion.
  let mut samples=Delay::length(250.*0.001*r)?.checked_mul(2).ok_or(AudioError::Limit("reverb state overflow"))?;
  for i in 0..8{samples=samples.checked_add(Delay::length(ROOM[i].max(PLATE[i])*1.6*0.001*r+0.4*0.001*r+4.)?).ok_or(AudioError::Limit("reverb state overflow"))?;}
  for i in 0..4{for side in [1.,1.071]{samples=samples.checked_add((ROOM_DIFF[i].max(PLATE_DIFF[i])*side*0.001*r)as usize+1).ok_or(AudioError::Limit("reverb state overflow"))?;}}
  samples.checked_mul(std::mem::size_of::<f32>()).and_then(|v|v.checked_add(std::mem::size_of::<Self>())).ok_or(AudioError::Limit("reverb state overflow"))
 }
 pub(super) fn new(rate:u32,channels:u16,filter:AudioFilter)->Result<Self,AudioError>{
  let bytes=Self::required_bytes(rate,channels)?;let r=rate as f32;
  // Allocate room maximum once so changing room/plate never allocates in process().
  let pre=[Delay::new(250.*0.001*r)?,Delay::new(250.*0.001*r)?];
  let lines:[Delay;8]=std::array::from_fn(|i|Delay::new(ROOM[i].max(PLATE[i])*1.6*0.001*r+0.4*0.001*r+4.).expect("bounded reverb line"));
  let diff=std::array::from_fn(|side|std::array::from_fn(|i|Allpass::new((ROOM_DIFF[i].max(PLATE_DIFF[i])*(if side==0{1.}else{1.071})*0.001*r)as usize+1)));
  let mut s=Self{rate:r,channels:usize::from(channels),plate:false,pre,diff,lines,lowcut:[Highpass::default();2],lp:[0.;8],gains:[0.;8],mix:Smooth::new(0.,30.,r),size:Smooth::new(1.,200.,r),predelay:Smooth::new(0.,100.,r),width:Smooth::new(1.,30.,r),decay:1.,damping:0.,diffusion:0.,lfo:0.,gain_counter:0,bytes};s.update(filter)?;s.reset();Ok(s)
 }
 pub(super) fn state_bytes(&self)->usize{self.bytes}
 pub(super) fn update(&mut self,filter:AudioFilter)->Result<(),AudioError>{
  filter.validate().map_err(|_|AudioError::InvalidRequest("invalid SoundCraft reverb parameters"))?;
  let AudioFilter::CraftSoundReverb{plate,predelay_ms,decay_seconds,size_pct,damping_hz,diffusion_pct,width_pct,lowcut_hz,mix_pct,..}=filter else{return Err(AudioError::InvalidRequest("invalid SoundCraft reverb filter"));};
  if self.plate!=plate{self.plate=plate;self.reset();}self.mix.target=mix_pct/100.;self.size.target=0.35+1.25*size_pct/100.;self.predelay.target=predelay_ms;self.width.target=width_pct/100.;self.decay=decay_seconds;self.damping=(-std::f32::consts::TAU*damping_hz/self.rate).exp();self.diffusion=0.7*diffusion_pct/100.;for hp in &mut self.lowcut{hp.configure(self.rate,lowcut_hz);}Ok(())
 }
 pub(super) fn reset(&mut self){for d in &mut self.pre{d.clear();}for side in &mut self.diff{for d in side{d.clear();}}for d in &mut self.lines{d.clear();}self.lp=[0.;8];for hp in &mut self.lowcut{hp.z=[0.;2];}self.mix.snap();self.size.snap();self.predelay.snap();self.width.snap();self.lfo=0.;self.gain_counter=0;}
 pub(super) fn process(&mut self,samples:&mut[f32],channels:usize)->Result<(),AudioError>{
  if channels==0||channels!=self.channels||samples.len()%channels!=0{return Err(AudioError::InvalidRequest("sound reverb channel mismatch"));}
  let bases=if self.plate{PLATE}else{ROOM};let diffs=if self.plate{PLATE_DIFF}else{ROOM_DIFF};let modulation=if self.plate{0.4}else{0.25};let lfo_hz=if self.plate{1.1}else{0.7};
  for frame in samples.chunks_exact_mut(channels){
   let left=if frame[0].is_finite(){frame[0]}else{0.};let right=if channels>1&&frame[1].is_finite(){frame[1]}else{left};let pd=self.predelay.next()*0.001*self.rate;let mut input=[left,right];
   for side in 0..2{self.pre[side].push(input[side]);let mut x=if pd<1.{input[side]}else{self.pre[side].read(pd)};for i in 0..4{
    // Distinct room/plate diffusion lengths are addressed within the fixed buffers.
    let n=(diffs[i]*(if side==0{1.}else{1.071})*0.001*self.rate)as usize;let d=&mut self.diff[side][i];let active=n.clamp(1,d.data.len());d.pos%=active;let old=d.data[d.pos];let next=x+self.diffusion*old;d.data[d.pos]=next;d.pos=(d.pos+1)%active;x=old-self.diffusion*next;
   }input[side]=x;}
   let size=self.size.next();self.lfo=(self.lfo+lfo_hz/self.rate).fract();let(s,c)=(self.lfo*std::f32::consts::TAU).sin_cos();let mods=[0.,s,0.,c,0.,-s,0.,-c];let mut raw=[0.;8];let mut feedback=[0.;8];
   if self.gain_counter==0{for i in 0..8{self.gains[i]=10f32.powf(-3.*bases[i]*size*0.001/self.decay.max(0.05)).min(0.9999);}}self.gain_counter=(self.gain_counter+1)%32;
   for i in 0..8{let delay=(bases[i]*size+modulation*(1.+mods[i]))*0.001*self.rate;raw[i]=self.lines[i].read(delay);self.lp[i]=raw[i]+(self.lp[i]-raw[i])*self.damping;feedback[i]=self.lp[i]*self.gains[i];}
   let mut step=1;while step<8{for start in (0..8).step_by(step*2){for j in start..start+step{let(a,b)=(feedback[j],feedback[j+step]);feedback[j]=a+b;feedback[j+step]=a-b;}}step*=2;}
   for i in 0..8{self.lines[i].push(feedback[i]*std::f32::consts::FRAC_1_SQRT_2*0.5+input[i%2]*0.5);}
   let wl=self.lowcut[0].sample(0.4*(raw[0]-raw[2]+raw[4]-raw[6]));let wr=self.lowcut[1].sample(0.4*(raw[1]-raw[3]+raw[5]-raw[7]));let mid=(wl+wr)*0.5;let side=(wl-wr)*0.5*self.width.next();let mix=self.mix.next();
   for(c,x)in frame.iter_mut().enumerate(){let dry=if x.is_finite(){*x}else{0.};let wet=if channels==1{mid}else if c%2==0{mid+side}else{mid-side};*x=dry*(1.-mix)+wet*mix;}
  }Ok(())
 }
}
