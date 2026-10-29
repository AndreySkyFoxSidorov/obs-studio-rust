//! SoundCraft Lo-Fi: bounded per-channel sample-and-hold and bit reduction.
use skylive_core::AudioFilter;

#[derive(Clone)]
pub(super) struct LoFi {
    rate:u32, held:Vec<f32>, phase:f32, rng:u32,
    step:f32, increment:f32, mix:f32, mix_target:f32,
    noise:f32, noise_target:f32, smoothing:f32,
}
impl LoFi {
    pub(super) fn new(rate:u32, channels:u16, filter:AudioFilter)->Result<Self,super::AudioError> {
        if rate==0 || channels==0 {return Err(super::AudioError::InvalidRequest("invalid Lo-Fi format"));}
        let mut result=Self{rate,held:vec![0.;usize::from(channels)],phase:1.,rng:77,step:1.,increment:1.,mix:0.,mix_target:0.,noise:0.,noise_target:0.,smoothing:(-1.0/(0.020*rate as f32)).exp()};
        result.update(filter)?;result.mix=result.mix_target;result.noise=result.noise_target;Ok(result)
    }
    pub(super) fn update(&mut self, filter:AudioFilter)->Result<(),super::AudioError> {
        filter.validate().map_err(|_|super::AudioError::InvalidRequest("invalid Lo-Fi parameters"))?;
        let AudioFilter::CraftLoFi{bits,sample_rate,noise_pct,mix_pct,..}=filter else{return Err(super::AudioError::InvalidRequest("invalid Lo-Fi filter"));};
        self.step=2f32.powf(1.0-bits);self.increment=(sample_rate/self.rate as f32).min(1.0);
        self.mix_target=mix_pct/100.;let noise=noise_pct/100.;self.noise_target=noise*noise*0.1;Ok(())
    }
    pub(super) fn state_bytes(&self)->usize {self.held.len()*std::mem::size_of::<f32>()}
    pub(super) fn process(&mut self,samples:&mut [f32],channels:usize)->Result<(),super::AudioError>{
        if channels==0 || channels!=self.held.len() || samples.len()%channels!=0 {return Err(super::AudioError::InvalidRequest("Lo-Fi channel mismatch"));}
        for frame in samples.chunks_exact_mut(channels){
            self.mix=self.mix_target+(self.mix-self.mix_target)*self.smoothing;
            self.noise=self.noise_target+(self.noise-self.noise_target)*self.smoothing;
            if (self.mix-self.mix_target).abs()<=1.0e-6*(1.0+self.mix_target.abs()){self.mix=self.mix_target;}
            if (self.noise-self.noise_target).abs()<=1.0e-6*(1.0+self.noise_target.abs()){self.noise=self.noise_target;}
            self.phase+=self.increment;let grab=self.phase>=1.;if grab{self.phase-=1.;}
            for (sample,held) in frame.iter_mut().zip(self.held.iter_mut()){
                let dry=if sample.is_finite(){*sample}else{0.};
                if grab {let mut noisy=dry;if self.noise>0. {let mut x=self.rng;x^=x<<13;x^=x>>17;x^=x<<5;self.rng=x;noisy+=((x>>8) as f32*(2.0/16_777_216.0)-1.)*self.noise;}*held=(noisy/self.step).round()*self.step;}
                *sample=dry*(1.-self.mix)+*held*self.mix;
            }
        }Ok(())
    }
}
