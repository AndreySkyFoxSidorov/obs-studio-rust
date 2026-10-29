//! SoundCraft half/full-wave rectifier with per-channel DC removal.
use skylive_core::AudioFilter;

#[derive(Clone)]
pub(super) struct Rectifier {
    channels: Vec<(f32, f32)>, dc: f32, smoothing: f32,
    mix: f32, mix_target: f32, output: f32, output_target: f32, full: bool,
}
impl Rectifier {
    pub(super) fn new(rate:u32, channels:u16, filter:AudioFilter)->Result<Self,super::AudioError> {
        if rate==0 || channels==0 {return Err(super::AudioError::InvalidRequest("invalid rectifier format"));}
        let mut value=Self {channels:vec![(0.0,0.0);usize::from(channels)],dc:(-std::f32::consts::TAU*10.0/rate as f32).exp(),
            smoothing:(-1.0/(0.020*rate as f32)).exp(),mix:1.0,mix_target:1.0,output:1.0,output_target:1.0,full:true};
        value.update(filter)?;value.mix=value.mix_target;value.output=value.output_target;Ok(value)
    }
    pub(super) fn update(&mut self,filter:AudioFilter)->Result<(),super::AudioError> {
        filter.validate().map_err(|_|super::AudioError::InvalidRequest("invalid rectifier parameters"))?;
        let AudioFilter::CraftRectifier{full_wave,mix_pct,output_db,..}=filter else{return Err(super::AudioError::InvalidRequest("invalid rectifier filter"));};
        self.full=full_wave;self.mix_target=mix_pct/100.0;self.output_target=10.0_f32.powf(output_db/20.0);Ok(())
    }
    pub(super) fn reset(&mut self){self.channels.fill((0.0,0.0));self.mix=self.mix_target;self.output=self.output_target;}
    pub(super) fn state_bytes(&self)->usize {self.channels.len()*std::mem::size_of::<(f32,f32)>()}
    pub(super) fn process(&mut self,samples:&mut [f32],channels:usize)->Result<(),super::AudioError> {
        if channels==0 || channels!=self.channels.len() || samples.len()%channels!=0 {return Err(super::AudioError::InvalidRequest("rectifier channel mismatch"));}
        for frame in samples.chunks_exact_mut(channels) {
            self.mix=self.mix_target+(self.mix-self.mix_target)*self.smoothing;
            self.output=self.output_target+(self.output-self.output_target)*self.smoothing;
            for (sample,state) in frame.iter_mut().zip(&mut self.channels) {
                let dry=if sample.is_finite(){*sample}else{0.0};
                let input=if self.full {dry.abs()} else {dry.max(0.0)};
                let wet=input-state.0+self.dc*state.1;state.0=input;
                state.1=if wet.is_finite() && wet.abs()>1.0e-25 {wet}else{0.0};
                *sample=(dry*(1.0-self.mix)+state.1*self.mix)*self.output;
            }
        } Ok(())
    }
}
