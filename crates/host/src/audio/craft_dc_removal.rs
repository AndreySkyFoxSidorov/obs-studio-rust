//! SoundCraft DC offset removal: one-pole high-pass, independent per channel.
use skylive_core::AudioFilter;
#[derive(Clone)]
pub(super) struct DcRemoval { rate:u32, r:f32, state:Vec<(f32,f32)> }
impl DcRemoval {
    pub(super) fn new(rate:u32,channels:u16,filter:AudioFilter)->Result<Self,super::AudioError>{
        if rate==0 || channels==0 {return Err(super::AudioError::InvalidRequest("invalid DC removal format"));}
        let mut result=Self {rate,r:0.0,state:vec![(0.0,0.0);usize::from(channels)]}; result.update(filter)?; Ok(result)
    }
    pub(super) fn update(&mut self,filter:AudioFilter)->Result<(),super::AudioError>{
        filter.validate().map_err(|_|super::AudioError::InvalidRequest("invalid DC removal parameters"))?;
        let AudioFilter::CraftDcRemoval{cutoff_hz,..}=filter else{return Err(super::AudioError::InvalidRequest("invalid DC removal filter"));};
        self.r=(-std::f32::consts::TAU*cutoff_hz/self.rate as f32).exp(); Ok(())
    }
    pub(super) fn reset(&mut self){self.state.fill((0.0,0.0));}
    pub(super) fn state_bytes(&self)->usize{self.state.len()*std::mem::size_of::<(f32,f32)>()}
    pub(super) fn process(&mut self,samples:&mut [f32],channels:usize)->Result<(),super::AudioError>{
        if channels==0 || channels!=self.state.len() || samples.len()%channels!=0{return Err(super::AudioError::InvalidRequest("DC removal channel mismatch"));}
        for frame in samples.chunks_exact_mut(channels){for (sample,state) in frame.iter_mut().zip(&mut self.state){
            let input=if sample.is_finite(){*sample}else{0.0};
            let value=input-state.0+self.r*state.1;
            state.0=input; state.1=if value.is_finite() && value.abs()>1.0e-25{value}else{0.0}; *sample=state.1;
        }} Ok(())
    }
}
