//! Bounded SoundCraft-style Ring, FM and positive/negative resonator.
use skylive_core::AudioFilter;
use super::AudioError;

#[derive(Clone)]
struct Delay {data:Vec<f32>, pos:usize}
impl Delay {
    fn push(&mut self,value:f32){self.data[self.pos]=value;self.pos=(self.pos+1)%self.data.len();}
    fn read(&self,delay:f32)->f32 {
        let d=delay.clamp(2.0,(self.data.len()-3) as f32);
        let i=d as usize;let f=d-i as f32;
        let tap=|n:usize|self.data[(self.pos+self.data.len()-n)%self.data.len()];
        let (a,b,c,d)=(tap(i-1),tap(i),tap(i+1),tap(i+2));
        let c1=0.5*(c-a);let c2=a-2.5*b+2.0*c-0.5*d;let c3=0.5*(d-a)+1.5*(b-c);
        ((c3*f+c2)*f+c1)*f+b
    }
}
#[derive(Clone)]
pub(super) struct RingMod {
    rate:f32,channels:usize,phase:f32,lfo:f32,envelope:f32,held:f32,hold_left:usize,rng:u32,
    frequency:f32,frequency_target:f32,mix:f32,mix_target:f32,
    resonance:f32,resonance_target:f32,depth:f32,depth_target:f32,
    mod_rate:f32,source:u8,mode:u8,smoothing:f32,lines:Vec<Delay>,
}
impl RingMod {
    fn line_len(rate:u32)->Result<usize,AudioError>{
        (usize::try_from(rate).map_err(|_|AudioError::Limit("ring mod rate overflow"))?/5).checked_add(16).ok_or(AudioError::Limit("ring mod state overflow"))
    }
    pub(super) fn required_bytes(rate:u32,channels:u16)->Result<usize,AudioError>{
        if rate==0||channels==0{return Err(AudioError::InvalidRequest("invalid ring mod format"));}
        Self::line_len(rate)?.checked_mul(std::mem::size_of::<f32>()).and_then(|v|v.checked_add(std::mem::size_of::<Delay>())).and_then(|v|v.checked_mul(usize::from(channels))).ok_or(AudioError::Limit("ring mod state overflow"))
    }
    pub(super) fn state_bytes(&self)->usize{self.lines.capacity()*std::mem::size_of::<Delay>()+self.lines.iter().map(|line|line.data.capacity()*std::mem::size_of::<f32>()).sum::<usize>()}
    pub(super) fn new(rate:u32,channels:u16,filter:AudioFilter)->Result<Self,AudioError>{
        Self::required_bytes(rate,channels)?;
        let len=Self::line_len(rate)?;
        let mut value=Self{rate:rate as f32,channels:usize::from(channels),phase:0.0,lfo:0.0,envelope:0.0,held:0.0,hold_left:0,rng:0x5eed_1234,frequency:440.0,frequency_target:440.0,mix:1.0,mix_target:1.0,resonance:0.5,resonance_target:0.5,depth:0.0,depth_target:0.0,mod_rate:1.0,source:0,mode:0,smoothing:(-1.0/(0.020*rate as f32)).exp(),lines:(0..channels).map(|_|Delay{data:vec![0.0;len],pos:0}).collect()};
        value.update(filter)?;value.reset();Ok(value)
    }
    pub(super) fn update(&mut self,filter:AudioFilter)->Result<(),AudioError>{
        filter.validate().map_err(|_|AudioError::InvalidRequest("invalid ring mod parameters"))?;
        let AudioFilter::CraftRingMod{frequency_hz,mix_pct,mode,resonance_pct,mod_source,mod_rate_hz,mod_depth_pct,..}=filter else{return Err(AudioError::InvalidRequest("invalid ring mod filter"));};
        if self.mode!=mode {for line in &mut self.lines{line.data.fill(0.0);line.pos=0;}}
        self.mode=mode;self.source=mod_source;self.mod_rate=mod_rate_hz;
        self.frequency_target=frequency_hz;self.mix_target=mix_pct/100.0;self.resonance_target=resonance_pct/100.0;self.depth_target=mod_depth_pct/100.0;Ok(())
    }
    pub(super) fn reset(&mut self){
        self.phase=0.0;self.lfo=0.0;self.envelope=0.0;self.held=0.0;self.hold_left=0;self.rng=0x5eed_1234;
        self.frequency=self.frequency_target;self.mix=self.mix_target;self.resonance=self.resonance_target;self.depth=self.depth_target;
        for line in &mut self.lines{line.data.fill(0.0);line.pos=0;}
    }
    pub(super) fn process(&mut self,samples:&mut[f32],channels:usize)->Result<(),AudioError>{
        if channels==0||channels!=self.channels||samples.len()%channels!=0{return Err(AudioError::InvalidRequest("ring mod channel mismatch"));}
        let attack=(-1.0/(0.005*self.rate)).exp();let release=(-1.0/(0.100*self.rate)).exp();
        for frame in samples.chunks_exact_mut(channels){
            self.frequency=self.frequency_target+(self.frequency-self.frequency_target)*self.smoothing;
            self.mix=self.mix_target+(self.mix-self.mix_target)*self.smoothing;
            self.resonance=self.resonance_target+(self.resonance-self.resonance_target)*self.smoothing;
            self.depth=self.depth_target+(self.depth-self.depth_target)*self.smoothing;
            let level=frame.iter().filter(|v|v.is_finite()).fold(0.0f32,|m,v|m.max(v.abs()));
            let k=if level>self.envelope{attack}else{release};self.envelope=level+(self.envelope-level)*k;
            // SoundCraft advances its LFO before sampling it; do this for all
            // modulation modes so switching sources preserves its phase.
            self.lfo=(self.lfo+self.mod_rate/self.rate).fract();
            let modulation=match self.source{
                1=>self.envelope.min(1.0),
                2=>{if self.hold_left==0{let mut x=self.rng;x^=x<<13;x^=x>>17;x^=x<<5;self.rng=x;self.held=(x>>8) as f32*(2.0/16_777_216.0)-1.0;self.hold_left=((self.rate/self.mod_rate) as usize).max(1);}self.hold_left-=1;self.held},
                _=>(self.lfo*std::f32::consts::TAU).sin(),
            };
            let frequency=(self.frequency*2.0f32.powf(2.0*self.depth*modulation)).clamp(1.0,(self.rate*0.45).max(1.0));
            self.phase=(self.phase+frequency/self.rate).fract();let carrier=(self.phase*std::f32::consts::TAU).sin();
            for (sample,line) in frame.iter_mut().zip(&mut self.lines){
                let dry=if sample.is_finite(){*sample}else{0.0};
                let wet=match self.mode{
                    1=>{line.push(dry);line.read(2.0+self.rate*0.001*(1.0+carrier))},
                    2|3=>{let sign=if self.mode==2{1.0}else{-1.0};let y=dry+sign*self.resonance*line.read(self.rate/frequency);let y=if y.is_finite(){y}else{0.0};line.push(y);y*(1.0-self.resonance)},
                    _=>dry*carrier,
                };
                *sample=dry*(1.0-self.mix)+wet*self.mix;
            }
        }Ok(())
    }
}
