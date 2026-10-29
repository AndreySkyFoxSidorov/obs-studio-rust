//! SoundCraft TPDF requantization with optional first-order error feedback.
use skylive_core::AudioFilter;

#[derive(Clone)]
pub(super) struct Dither { errors: Vec<f32>, rng:u32, bits:u8, shaping:bool }
impl Dither {
    pub(super) fn new(channels:u16, filter:AudioFilter)->Result<Self,super::AudioError>{
        if channels==0 {return Err(super::AudioError::InvalidRequest("invalid dither channel count"));}
        let mut value=Self{errors:vec![0.0;usize::from(channels)],rng:0xD17E,bits:16,shaping:false};
        value.update(filter)?;Ok(value)
    }
    pub(super) fn update(&mut self,filter:AudioFilter)->Result<(),super::AudioError>{
        filter.validate().map_err(|_|super::AudioError::InvalidRequest("invalid dither parameters"))?;
        let AudioFilter::CraftDither{bits,noise_shaping,..}=filter else{return Err(super::AudioError::InvalidRequest("invalid dither filter"));};
        self.bits=bits;self.shaping=noise_shaping;Ok(())
    }
    pub(super) fn reset(&mut self){self.errors.fill(0.0);}
    pub(super) fn state_bytes(&self)->usize{self.errors.len()*std::mem::size_of::<f32>()}
    fn bipolar(&mut self)->f32 {let mut x=self.rng;x^=x<<13;x^=x>>17;x^=x<<5;self.rng=x;(x>>8) as f32*(2.0/16_777_216.0)-1.0}
    pub(super) fn process(&mut self,samples:&mut [f32],channels:usize)->Result<(),super::AudioError>{
        if channels==0 || channels!=self.errors.len() || samples.len()%channels!=0 {return Err(super::AudioError::InvalidRequest("dither channel mismatch"));}
        let lsb=2f32.powi(1-i32::from(self.bits));
        // SoundCraft processes channel-major and carries one RNG across channels.
        for channel in 0..channels {let mut error=self.errors[channel];
            for index in (channel..samples.len()).step_by(channels) {
                let sample=samples[index];
                if sample==0.0 || !sample.is_finite(){samples[index]=0.0;error=0.0;continue;}
                let value=if self.shaping {sample-error}else{sample};
                let noise=(self.bipolar()+self.bipolar())*0.5*lsb;
                let quantized=((value+noise)/lsb).round()*lsb;
                error=quantized-value;samples[index]=quantized.clamp(-1.0,1.0-lsb);
            }
            self.errors[channel]=error;
        } Ok(())
    }
}
