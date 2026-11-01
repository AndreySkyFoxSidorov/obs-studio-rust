//! SoundCraft-style linked lookahead maximizer for interleaved source audio.
use skylive_core::AudioFilter;
use super::AudioError;

#[derive(Clone)]
pub(super) struct Maximizer {
 rate:u32, channels:usize, lookahead:usize, index:usize,
 delay:Vec<f32>, minima:Vec<(u64,f32)>, min_head:usize, min_len:usize, sequence:u64, smooth:Vec<f32>, sum:f64,
 held:f32, release:f32, drive:f32, drive_target:f32, drive_step:f32, ceiling:f32,
}
impl Maximizer {
 pub(super) fn required_bytes(rate:u32,channels:u16)->Result<usize,AudioError> {
  if rate==0 || channels==0 {return Err(AudioError::InvalidRequest("invalid maximizer format"));}
  let n=AudioFilter::craft_maximizer_lookahead_frames(rate) as usize;
  n.checked_mul(usize::from(channels)+1).and_then(|v|v.checked_mul(std::mem::size_of::<f32>()))
   .and_then(|v|n.checked_mul(std::mem::size_of::<(u64,f32)>()).and_then(|m|v.checked_add(m)))
   .ok_or(AudioError::Limit("maximizer state overflow"))
 }
 pub(super) fn new(rate:u32,channels:u16,filter:AudioFilter)->Result<Self,AudioError>{
  let _=Self::required_bytes(rate,channels)?;let n=AudioFilter::craft_maximizer_lookahead_frames(rate) as usize;
  let mut s=Self{rate,channels:channels as usize,lookahead:n,index:0,delay:vec![0.;n*channels as usize],minima:vec![(0,1.);n],min_head:0,min_len:0,sequence:0,smooth:vec![1.;n],sum:n as f64,held:1.,release:0.,drive:1.,drive_target:1.,drive_step:0.,ceiling:1.};
  s.update(filter)?;s.drive=s.drive_target;Ok(s)
 }
 pub(super) fn update(&mut self,filter:AudioFilter)->Result<(),AudioError>{
  filter.validate().map_err(|_|AudioError::InvalidRequest("invalid maximizer parameters"))?;
  let AudioFilter::CraftMaximizer{threshold_db,ceiling_db,release_ms,..}=filter else{return Err(AudioError::InvalidRequest("invalid maximizer filter"));};
  self.drive_target=10f32.powf(-threshold_db/20.);self.drive_step=(self.drive_target-self.drive)/(self.rate as f32*0.020).max(1.);
  self.ceiling=10f32.powf(ceiling_db/20.);self.release=(-1.0/(release_ms*self.rate as f32/1000.).max(1.)).exp();Ok(())
 }
 pub(super) fn reset(&mut self){self.delay.fill(0.);self.smooth.fill(1.);self.index=0;self.min_head=0;self.min_len=0;self.sequence=0;self.sum=self.lookahead as f64;self.held=1.;self.drive=self.drive_target;}
 pub(super) fn state_bytes(&self)->usize{(self.delay.len()+self.smooth.len())*std::mem::size_of::<f32>()+self.minima.len()*std::mem::size_of::<(u64,f32)>()}
 pub(super) fn process(&mut self,samples:&mut [f32],channels:usize)->Result<(),AudioError>{
  if channels!=self.channels || samples.len()%channels!=0{return Err(AudioError::InvalidRequest("maximizer channel mismatch"));}
  for frame in samples.chunks_exact_mut(channels){
   if (self.drive-self.drive_target).abs()<=self.drive_step.abs(){self.drive=self.drive_target;}else{self.drive+=self.drive_step;}
   let mut peak=0f32;let base=self.index*channels;
   for (c,x) in frame.iter_mut().enumerate(){let input=if x.is_finite(){*x*self.drive}else{0.};peak=peak.max(input.abs());let delayed=self.delay[base+c];self.delay[base+c]=input;*x=delayed;}
   let required=if peak>self.ceiling{self.ceiling/peak}else{1.};
   // A bounded monotonic deque computes the same sliding-window minimum as
   // scanning every lookahead sample, with amortized constant work per frame.
   if self.sequence>=self.lookahead as u64 {
    let expired=self.sequence-self.lookahead as u64;
    while self.min_len>0 && self.minima[self.min_head].0<=expired {self.min_head=(self.min_head+1)%self.lookahead;self.min_len-=1;}
   }
   while self.min_len>0 {let tail=(self.min_head+self.min_len-1)%self.lookahead;if self.minima[tail].1<required{break;}self.min_len-=1;}
   let tail=(self.min_head+self.min_len)%self.lookahead;self.minima[tail]=(self.sequence,required);self.min_len+=1;
   self.sequence=self.sequence.checked_add(1).ok_or(AudioError::Limit("maximizer timeline exhausted"))?;
   let hold=self.minima[self.min_head].1;self.held=if hold<self.held{hold}else{hold+(self.held-hold)*self.release};
   self.sum+=f64::from(self.held)-f64::from(self.smooth[self.index]);self.smooth[self.index]=self.held;
   self.index=(self.index+1)%self.lookahead;if self.index==0{self.sum=self.smooth.iter().map(|v|f64::from(*v)).sum();}
   let gain=(self.sum/self.lookahead as f64).clamp(0.,1.) as f32;for x in frame{*x=(*x*gain).clamp(-self.ceiling,self.ceiling);}
  }Ok(())
 }
}
