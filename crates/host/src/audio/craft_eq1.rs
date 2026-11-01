//! SoundCraft EQ 1-Band: RBJ biquad shapes with independent channel state.
use skylive_core::AudioFilter;
use super::AudioError;

#[derive(Clone)]
pub(super) struct Eq1 {
    rate: u32,
    states: Vec<(f32, f32)>,
    current: [f64; 6],
    target: [f64; 6],
    smoothing: f64,
}
impl Eq1 {
    pub(super) fn new(rate: u32, channels: u16, filter: AudioFilter) -> Result<Self, AudioError> {
        if rate == 0 || channels == 0 { return Err(AudioError::InvalidRequest("invalid EQ format")); }
        let mut value = Self { rate, states: vec![(0.0, 0.0); channels as usize], current: [1.0,0.0,0.0,0.0,0.0,1.0], target: [1.0,0.0,0.0,0.0,0.0,1.0], smoothing: (-1.0/(0.02*rate as f64)).exp() };
        value.update(filter)?;
        value.current = value.target;
        Ok(value)
    }
    pub(super) fn update(&mut self, filter: AudioFilter) -> Result<(), AudioError> {
        filter.validate().map_err(|_| AudioError::InvalidRequest("invalid EQ parameters"))?;
        let AudioFilter::CraftEq1 {kind, frequency_hz, gain_db, q, output_db, ..} = filter else { return Err(AudioError::InvalidRequest("invalid EQ filter")); };
        let w = std::f64::consts::TAU * (frequency_hz as f64).min(self.rate as f64 * 0.49) / self.rate as f64;
        let (s,c) = w.sin_cos();
        let alpha = s / (2.0 * q as f64);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let (b0,b1,b2,a0,a1,a2) = match kind {
            0 => (1.0+alpha*a,-2.0*c,1.0-alpha*a,1.0+alpha/a,-2.0*c,1.0-alpha/a),
            1 => { let t=2.0*a.sqrt()*alpha; (a*((a+1.0)-(a-1.0)*c+t),2.0*a*((a-1.0)-(a+1.0)*c),a*((a+1.0)-(a-1.0)*c-t),(a+1.0)+(a-1.0)*c+t,-2.0*((a-1.0)+(a+1.0)*c),(a+1.0)+(a-1.0)*c-t) },
            2 => { let t=2.0*a.sqrt()*alpha; (a*((a+1.0)+(a-1.0)*c+t),-2.0*a*((a-1.0)+(a+1.0)*c),a*((a+1.0)+(a-1.0)*c-t),(a+1.0)-(a-1.0)*c+t,2.0*((a-1.0)-(a+1.0)*c),(a+1.0)-(a-1.0)*c-t) },
            3 => ((1.0+c)/2.0,-(1.0+c),(1.0+c)/2.0,1.0+alpha,-2.0*c,1.0-alpha),
            4 => ((1.0-c)/2.0,1.0-c,(1.0-c)/2.0,1.0+alpha,-2.0*c,1.0-alpha),
            5 => (1.0,-2.0*c,1.0,1.0+alpha,-2.0*c,1.0-alpha),
            _ => (alpha,0.0,-alpha,1.0+alpha,-2.0*c,1.0-alpha),
        };
        self.target = [b0/a0,b1/a0,b2/a0,a1/a0,a2/a0,10f64.powf(output_db as f64/20.0)];
        Ok(())
    }
    pub(super) fn reset(&mut self) { self.states.fill((0.0,0.0)); self.current=self.target; }
    pub(super) fn state_bytes(&self) -> usize { self.states.len() * std::mem::size_of::<(f32,f32)>() }
    pub(super) fn process(&mut self, samples:&mut [f32], channels:usize) -> Result<(),AudioError> {
        if channels==0 || channels!=self.states.len() || samples.len()%channels!=0 {return Err(AudioError::InvalidRequest("EQ channel mismatch"));}
        for frame in samples.chunks_exact_mut(channels) {
            for (current,target) in self.current.iter_mut().zip(self.target) { *current=target+(*current-target)*self.smoothing; }
            let [b0,b1,b2,a1,a2,output]=self.current;
            for (sample,state) in frame.iter_mut().zip(&mut self.states) {
                let x=if sample.is_finite(){*sample as f64}else{0.0};
                let y=b0*x+state.0 as f64;
                let z1=b1*x-a1*y+state.1 as f64;
                let z2=b2*x-a2*y;
                if y.is_finite() && (y*output).is_finite() && (y*output).abs()<f32::MAX as f64 && z1.is_finite() && z2.is_finite() && y.abs()<f32::MAX as f64 && z1.abs()<f32::MAX as f64 && z2.abs()<f32::MAX as f64 {
                    *state=(z1 as f32,z2 as f32); *sample=(y*output) as f32;
                } else { *state=(0.0,0.0); *sample=0.0; }
            }
        }
        Ok(())
    }
}
