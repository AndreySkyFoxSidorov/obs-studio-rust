//! Source-owned SoundCraft saturator, operating on interleaved samples.
use skylive_core::AudioFilter;

#[derive(Clone, Copy)]
struct Smooth { current: f32, target: f32, coefficient: f32 }
impl Smooth {
    fn new(value:f32, coefficient:f32)->Self {Self{current:value,target:value,coefficient}}
    fn next(&mut self)->f32 {
        if self.current!=self.target {
            self.current=self.target+(self.current-self.target)*self.coefficient;
            if (self.current-self.target).abs()<=1.0e-6*(1.0+self.target.abs()){self.current=self.target;}
        }
        self.current
    }
}
#[derive(Clone, Copy, Default)]
struct Channel { dc_x:f32, dc_y:f32, lowpass:f32 }
fn flush(value:f32)->f32 {if value.is_finite() && value.abs()>1.0e-25 {value} else {0.0}}

#[derive(Clone)]
pub(super) struct Saturator {
    rate:u32, channels:Vec<Channel>, drive:Smooth, bias:Smooth, mix:Smooth, output:Smooth,
    tone:f32, dc_coefficient:f32,
}
impl Saturator {
    pub(super) fn new(rate:u32,channels:u16,filter:AudioFilter)->Result<Self,super::AudioError> {
        if rate==0 || channels==0 {return Err(super::AudioError::InvalidRequest("invalid saturator format"));}
        filter.validate().map_err(|_|super::AudioError::InvalidRequest("invalid saturator parameters"))?;
        let coefficient=(-1.0/(0.020*rate as f32)).exp();
        let mut result=Self {rate,channels:vec![Channel::default();usize::from(channels)],
            drive:Smooth::new(1.0,coefficient),bias:Smooth::new(0.0,coefficient),
            mix:Smooth::new(1.0,coefficient),output:Smooth::new(1.0,coefficient),tone:0.0,
            dc_coefficient:(-std::f32::consts::TAU*10.0/rate as f32).exp()};
        result.update(filter)?;
        for value in [&mut result.drive,&mut result.bias,&mut result.mix,&mut result.output] {value.current=value.target;}
        Ok(result)
    }
    pub(super) fn update(&mut self,filter:AudioFilter)->Result<(),super::AudioError> {
        filter.validate().map_err(|_|super::AudioError::InvalidRequest("invalid saturator parameters"))?;
        let AudioFilter::CraftSaturator{drive_db,bias_pct,tone_hz,mix_pct,output_db,..}=filter else{return Err(super::AudioError::InvalidRequest("invalid saturator filter"));};
        self.drive.target=10.0_f32.powf(drive_db/20.0); self.bias.target=bias_pct/200.0;
        self.mix.target=mix_pct/100.0; self.output.target=10.0_f32.powf(output_db/20.0);
        self.tone=if tone_hz>=19_999.0 {0.0}else{(-std::f32::consts::TAU*tone_hz/self.rate as f32).exp()};
        Ok(())
    }
    pub(super) fn state_bytes(&self)->usize {self.channels.len()*std::mem::size_of::<Channel>()}
    pub(super) fn process(&mut self,samples:&mut [f32],channels:usize)->Result<(),super::AudioError>{
        if channels!=self.channels.len() || samples.len()%channels!=0 {return Err(super::AudioError::InvalidRequest("saturator channel mismatch"));}
        for frame in samples.chunks_exact_mut(channels) {
            let drive=self.drive.next();let bias=self.bias.next();let mix=self.mix.next();let out=self.output.next();
            let tb=bias.tanh();let norm=1.0/((drive+bias).tanh()-tb).max(1.0e-3);
            for (sample,state) in frame.iter_mut().zip(&mut self.channels) {
                let dry=if sample.is_finite(){*sample}else{0.0};let mut wet=((drive*dry+bias).tanh()-tb)*norm;
                if bias>0.0 {let y=wet-state.dc_x+self.dc_coefficient*state.dc_y;state.dc_x=wet;state.dc_y=flush(y);wet=state.dc_y;}
                if self.tone>0.0 {state.lowpass=flush(wet+(state.lowpass-wet)*self.tone);wet=state.lowpass;}
                *sample=(dry*(1.0-mix)+wet*mix)*out;
            }
        }
        Ok(())
    }
}
