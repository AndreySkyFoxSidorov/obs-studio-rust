//! SoundCraft dynamic high-shelf de-esser; interleaved audio, linked detector.
use skylive_core::AudioFilter;
use super::AudioError;
#[derive(Clone,Copy,Default)]
struct Biquad { b:[f32;3], a:[f32;2], z:[f32;2] }
impl Biquad {
 fn set(&mut self,b:[f64;3],a:[f64;3]) {self.b=b.map(|v|(v/a[0]) as f32);self.a=[(a[1]/a[0]) as f32,(a[2]/a[0]) as f32];}
 fn sample(&mut self,x:f32)->f32 {let x=if x.is_finite(){x}else{0.};let y=self.b[0]*x+self.z[0];self.z=[self.b[1]*x-self.a[0]*y+self.z[1],self.b[2]*x-self.a[1]*y];if y.is_finite()&&self.z.iter().all(|x|x.is_finite()){y}else{self.z=[0.;2];0.}}
}
#[derive(Clone)]
pub(super) struct DeEsser {rate:u32,frequency:f32,range:f32,threshold:f32,listen:bool,env:f32,attack:f32,release:f32,hp:Vec<Biquad>,shelf:Vec<Biquad>,gain:f32}
impl DeEsser {
 pub(super) fn new(rate:u32,channels:u16,filter:AudioFilter)->Result<Self,AudioError>{
  if rate==0||channels==0{return Err(AudioError::InvalidRequest("invalid de-esser format"));}
  let mut s=Self{rate,frequency:6000.,range:10.,threshold:-30.,listen:false,env:0.,attack:(-1./(0.0005*rate as f32)).exp(),release:(-1./(0.060*rate as f32)).exp(),hp:vec![Biquad::default();channels as usize],shelf:vec![Biquad::default();channels as usize],gain:0.};s.update(filter)?;Ok(s)
 }
 pub(super) fn update(&mut self,filter:AudioFilter)->Result<(),AudioError>{
  filter.validate().map_err(|_|AudioError::InvalidRequest("invalid de-esser parameters"))?;
  let AudioFilter::CraftDeEsser{frequency_hz,range_db,threshold_db,listen,..}=filter else{return Err(AudioError::InvalidRequest("invalid de-esser filter"));};
  self.frequency=frequency_hz;self.range=range_db;self.threshold=threshold_db;self.listen=listen;
  let w=std::f64::consts::TAU*f64::from(frequency_hz).clamp(1.,self.rate as f64*0.49)/self.rate as f64;let (sin,cos)=w.sin_cos();let alpha=sin/(2.*std::f64::consts::FRAC_1_SQRT_2);
  for b in &mut self.hp {b.set([(1.+cos)/2.,-(1.+cos),(1.+cos)/2.],[1.+alpha,-2.*cos,1.-alpha]);}self.set_shelf(self.gain);Ok(())
 }
 fn set_shelf(&mut self,gr:f32){
  self.gain=gr;
  if gr<0.01 {for b in &mut self.shelf {b.set([1.,0.,0.],[1.,0.,0.]);}return;}
  let w=std::f64::consts::TAU*(f64::from(self.frequency)*0.8).clamp(1.,self.rate as f64*0.49)/self.rate as f64;let (sin,cos)=w.sin_cos();let alpha=sin/(2.*0.8);let a=10f64.powf(-f64::from(gr)/40.);let s=2.*a.sqrt()*alpha;
  let b=[a*((a+1.)+(a-1.)*cos+s),-2.*a*((a-1.)+(a+1.)*cos),a*((a+1.)+(a-1.)*cos-s)];let d=[(a+1.)-(a-1.)*cos+s,2.*((a-1.)-(a+1.)*cos),(a+1.)-(a-1.)*cos-s];for q in &mut self.shelf {q.set(b,d);}
 }
 pub(super) fn reset(&mut self){self.env=0.;for b in self.hp.iter_mut().chain(self.shelf.iter_mut()){b.z=[0.;2];}self.set_shelf(0.);}
 pub(super) fn state_bytes(&self)->usize{(self.hp.len()+self.shelf.len())*std::mem::size_of::<Biquad>()}
 pub(super) fn process(&mut self,samples:&mut [f32],channels:usize)->Result<(),AudioError>{
  if channels==0||channels!=self.hp.len()||samples.len()%channels!=0{return Err(AudioError::InvalidRequest("de-esser channel mismatch"));}
  for block in samples.chunks_mut(channels*16){
   for frame in block.chunks_exact_mut(channels){let mut level=0f32;for (x,hp) in frame.iter_mut().zip(&mut self.hp){let h=hp.sample(*x);level=level.max(h.abs());if self.listen{*x=h;}}let coef=if level>self.env{self.attack}else{self.release};self.env=coef*self.env+(1.-coef)*level;if !self.env.is_finite(){self.env=0.;}}
   let gr=(20.*self.env.max(1e-15).log10()-self.threshold).clamp(0.,self.range);if (gr-self.gain).abs()>0.05||(gr==0.&&self.gain!=0.){self.set_shelf(gr);}
   if !self.listen{for frame in block.chunks_exact_mut(channels){for (x,shelf) in frame.iter_mut().zip(&mut self.shelf){*x=shelf.sample(*x);}}}
  }Ok(())
 }
}
