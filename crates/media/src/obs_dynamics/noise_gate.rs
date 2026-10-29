// SPDX-License-Identifier: GPL-2.0-or-later
//! Literal OBS32.2.2 noise-gate-filter.c state/ordering, interleaved PCM adapter.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObsNoiseGateParameters {
    open_db: f32, close_db: f32, attack_ms: u32, hold_ms: u32, release_ms: u32,
}
impl Default for ObsNoiseGateParameters {
    fn default() -> Self { Self {open_db:-26.,close_db:-32.,attack_ms:25,hold_ms:200,release_ms:150} }
}
impl ObsNoiseGateParameters {
    pub fn new(open_db:f32,close_db:f32,attack_ms:u32,hold_ms:u32,release_ms:u32)->Result<Self,ObsDynamicsError> {
        range("open_threshold_db",open_db,-96.,0.)?;
        range("close_threshold_db",close_db,-96.,0.)?;
        if attack_ms>10000||hold_ms>10000||release_ms>10000 {
            return Err(ObsDynamicsError::InvalidParameter("noise gate time outside 0..10000 ms"));
        }
        // OBS exposes both thresholds independently, including reversed ones.
        // Do not silently swap them or substitute the Craft gate algorithm.
        Ok(Self {open_db,close_db,attack_ms,hold_ms,release_ms})
    }
    pub fn open_db(self)->f32{self.open_db}
    pub fn close_db(self)->f32{self.close_db}
    pub fn attack_ms(self)->u32{self.attack_ms}
    pub fn hold_ms(self)->u32{self.hold_ms}
    pub fn release_ms(self)->u32{self.release_ms}
}
#[derive(Clone, Copy, Debug)]
pub struct ObsNoiseGate {
    rate:u32, channels:usize, parameters:ObsNoiseGateParameters,
    sample_rate_i:f32, open:f32, close:f32, decay:f32, attack:f32, release:f32, hold:f32,
    is_open:bool, attenuation:f32, level:f32, held_time:f32,
}
impl ObsNoiseGate {
    pub fn new(rate:u32,channels:u16,parameters:ObsNoiseGateParameters)->Result<Self,ObsDynamicsError>{
        let channels=format(rate,channels)?;
        let mut value=Self {rate,channels,parameters,sample_rate_i:0.,open:0.,close:0.,decay:0.,attack:0.,release:0.,hold:0.,is_open:false,attenuation:0.,level:0.,held_time:0.};
        value.configure();Ok(value)
    }
    fn configure(&mut self){
        let rate=self.rate as f32;
        let p=self.parameters;
        self.sample_rate_i=1./rate;
        self.open=db_to_mul(p.open_db);self.close=db_to_mul(p.close_db);
        // A zero Attack/Release intentionally yields +infinity. The original
        // fmin/fmax then opens/closes immediately; it never multiplies by inf.
        self.attack=1./((p.attack_ms as f32/1000.)*rate);
        self.release=1./((p.release_ms as f32/1000.)*rate);
        self.decay=(self.open-self.close)/((1.0f32/75.0f32)*rate);
        self.hold=p.hold_ms as f32/1000.;self.reset();
    }
    pub fn parameters(&self)->ObsNoiseGateParameters{self.parameters}
    pub fn set_parameters(&mut self,parameters:ObsNoiseGateParameters){
        // Host preparation runs each mix quantum. Only an authored parameter
        // update corresponds to OBS update(), which resets all gate history.
        if self.parameters!=parameters{self.parameters=parameters;self.configure();}
    }
    pub fn reset(&mut self){self.is_open=false;self.attenuation=0.;self.level=0.;self.held_time=0.;}
    pub fn gain_reduction_db(&self)->f32{mul_to_db(self.attenuation)}
    pub fn process_interleaved(&mut self,samples:&mut[f32],limits:DynamicsLimits)->Result<(),ObsDynamicsError>{
        block(samples,self.channels,limits,0,12)?;
        for frame in samples.chunks_exact_mut(self.channels){
            let current=frame.iter().fold(0.0f32,|v,x|v.max(x.abs()));
            if current>self.open&&!self.is_open{self.is_open=true;}
            // Deliberately use the PREVIOUS envelope here, before its decay.
            // Moving this below the level update changes OBS attack behavior.
            if self.level<self.close&&self.is_open{self.held_time=0.;self.is_open=false;}
            self.level=self.level.max(current)-self.decay;
            if self.is_open{self.attenuation=(self.attenuation+self.attack).min(1.);}
            else{self.held_time+=self.sample_rate_i;if self.held_time>self.hold{self.attenuation=(self.attenuation-self.release).max(0.);}}
            for sample in frame{*sample*=self.attenuation;}
        }
        Ok(())
    }
}
