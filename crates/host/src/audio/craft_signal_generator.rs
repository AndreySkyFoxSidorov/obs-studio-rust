//! Bounded SoundCraft-style signal generator. Replaces the input block.
use skylive_core::AudioFilter;

#[derive(Clone)]
pub(super) struct SignalGenerator { rate:u32, channels:usize, phase:f32, rng:u32, pink:[f32;7], wave:u8, freq:f32, target_freq:f32, level:f32, target_level:f32, smooth:f32 }
impl SignalGenerator {
    pub(super) fn new(rate:u32,channels:u16,filter:AudioFilter)->Result<Self,super::AudioError>{
        if rate==0||channels==0{return Err(super::AudioError::InvalidRequest("invalid generator format"));}
        let mut state=Self {rate,channels:usize::from(channels),phase:0.,rng:0x00c0ffee,pink:[0.;7],wave:0,freq:1000.,target_freq:1000.,level:0.,target_level:0.,smooth:1.0-(-1.0/(0.02*rate as f32)).exp()};
        state.update(filter)?;state.freq=state.target_freq;state.level=state.target_level;Ok(state)
    }
    pub(super) fn update(&mut self,filter:AudioFilter)->Result<(),super::AudioError>{
        filter.validate().map_err(|_|super::AudioError::InvalidRequest("invalid signal generator parameters"))?;
        let AudioFilter::CraftSignalGenerator{wave,frequency_hz,level_db,..}=filter else{return Err(super::AudioError::InvalidRequest("invalid generator filter"));};
        self.wave=wave;self.target_freq=frequency_hz;self.target_level=if level_db<=-96.0 {0.0}else{10.0f32.powf(level_db/20.0)};Ok(())
    }
    pub(super) fn reset(&mut self){self.phase=0.;self.pink=[0.;7];self.rng=0x00c0ffee;self.freq=self.target_freq;self.level=self.target_level;}
    pub(super) fn process(&mut self,samples:&mut[f32],channels:usize)->Result<(),super::AudioError>{
        if channels!=self.channels||channels==0||samples.len()%channels!=0{return Err(super::AudioError::InvalidRequest("generator channel mismatch"));}
        for frame in samples.chunks_exact_mut(channels){
            self.freq+=(self.target_freq-self.freq)*self.smooth;self.level+=(self.target_level-self.level)*self.smooth;
            let dt=(self.freq/self.rate as f32).clamp(0.,0.49);let t=self.phase;
            let blep=|x:f32| if dt<=0.0 {0.0} else if x<dt {let v=x/dt;2.0*v-v*v-1.0} else if x>1.0-dt {let v=(x-1.0)/dt;v*v+2.0*v+1.0} else {0.0};
            let value=match self.wave {
                0=>(t*std::f32::consts::TAU).sin(),
                1=>{let other=(t+0.5).fract();(if t<0.5 {1.0}else{-1.0})+blep(t)-blep(other)},
                2=>2.0*t-1.0-blep(t),
                3=>1.0-4.0*((t+0.25).fract()-0.5).abs(),
                _=>{self.rng^=self.rng<<13;self.rng^=self.rng>>17;self.rng^=self.rng<<5;let white=(self.rng as f64/u32::MAX as f64*2.0-1.0) as f32;
                    if self.wave==4 {white} else {let b=&mut self.pink;b[0]=0.99886*b[0]+white*0.0555179;b[1]=0.99332*b[1]+white*0.0750759;b[2]=0.969*b[2]+white*0.153852;b[3]=0.8665*b[3]+white*0.3104856;b[4]=0.55*b[4]+white*0.5329522;b[5]= -0.7616*b[5]-white*0.016898;let out=b[0]+b[1]+b[2]+b[3]+b[4]+b[5]+b[6]+white*0.5362;b[6]=white*0.115926;(out*0.11).clamp(-1.0,1.0)}},
            };
            self.phase=(t+dt).fract();frame.fill(value*self.level);
        } Ok(())
    }
}
