//! SoundCraft Time Shift: bounded per-channel delay with crossfaded tap changes.
use skylive_core::AudioFilter;
use super::AudioError;

pub(super) struct TimeShift {
    rate:u32, channels:usize, ring:Vec<f32>, frames:usize, write:usize,
    current:usize, target:usize, fade:usize,
}
impl TimeShift {
    pub(super) fn required_bytes(rate:u32,channels:u16)->Result<usize,AudioError>{
        if rate==0 || channels==0{return Err(AudioError::InvalidRequest("invalid Time Shift format"));}
        (rate as usize).checked_add(10002).and_then(|n|n.checked_mul(usize::from(channels)))
            .and_then(|n|n.checked_mul(std::mem::size_of::<f32>()))
            .ok_or(AudioError::Limit("Time Shift state byte overflow"))
    }
    pub(super) fn new(rate:u32,channels:u16,filter:AudioFilter)->Result<Self,AudioError>{
        let bytes=Self::required_bytes(rate,channels)?;
        let frames=rate as usize+10002;
        let mut result=Self{rate,channels:usize::from(channels),ring:vec![0.0;bytes/4],frames,write:0,current:0,target:0,fade:0};
        result.update(filter)?;result.current=result.target;result.fade=0;Ok(result)
    }
    pub(super) fn update(&mut self,filter:AudioFilter)->Result<(),AudioError>{
        filter.validate().map_err(|_|AudioError::InvalidRequest("invalid Time Shift parameters"))?;
        let AudioFilter::CraftTimeShift{delay_ms,delay_samples,..}=filter else{return Err(AudioError::InvalidRequest("invalid Time Shift filter"));};
        let next=((f64::from(delay_ms)*f64::from(self.rate)/1000.0).round() as usize)
            .saturating_add(delay_samples as usize).min(self.frames-1);
        if next!=self.target {if self.fade==0{self.fade=256;}self.target=next;}
        Ok(())
    }
    pub(super) fn reset(&mut self){self.ring.fill(0.0);self.write=0;self.current=self.target;self.fade=0;}
    pub(super) fn state_bytes(&self)->usize{self.ring.len()*std::mem::size_of::<f32>()}
    pub(super) fn process(&mut self,samples:&mut [f32],channels:usize)->Result<(),AudioError>{
        if channels!=self.channels || channels==0 || samples.len()%channels!=0{return Err(AudioError::InvalidRequest("Time Shift channel mismatch"));}
        for frame in samples.chunks_exact_mut(channels){
            let t=if self.fade>0{1.0-self.fade as f32/256.0}else{1.0};
            for (c,value) in frame.iter_mut().enumerate(){
                self.ring[self.write*channels+c]=if value.is_finite(){*value}else{0.0};
                let a=(self.write+self.frames-self.current)%self.frames;
                let b=(self.write+self.frames-self.target)%self.frames;
                *value=self.ring[a*channels+c]*(1.0-t)+self.ring[b*channels+c]*t;
            }
            self.write=(self.write+1)%self.frames;
            if self.fade>0{self.fade-=1;if self.fade==0{self.current=self.target;}}
        }
        Ok(())
    }
}
