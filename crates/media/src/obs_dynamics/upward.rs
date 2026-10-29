// SPDX-License-Identifier: GPL-2.0-or-later
//! OBS32.2.2 expander-filter.c upward-compressor branch, including its noise floor.
use super::*;
#[derive(Clone,Copy,Debug,PartialEq)]
pub struct ObsUpwardParameters {
    threshold_db:f32,ratio:f32,attack_ms:u32,release_ms:u32,output_db:f32,knee_db:u32,detector:ObsDetector,
}
impl Default for ObsUpwardParameters {
    fn default()->Self{Self {threshold_db:-20.,ratio:0.5,attack_ms:10,release_ms:50,output_db:0.,knee_db:10,detector:ObsDetector::Rms}}
}
impl ObsUpwardParameters {
    pub fn new(threshold_db:f32,ratio:f32,attack_ms:u32,release_ms:u32,output_db:f32,knee_db:u32,detector:ObsDetector)->Result<Self,ObsDynamicsError>{
        range("threshold_db",threshold_db,-60.,0.)?;range("ratio",ratio,0.,1.)?;
        time("attack_ms",attack_ms,100)?;time("release_ms",release_ms,1000)?;
        range("output_db",output_db,-32.,32.)?;
        if knee_db>20{return Err(ObsDynamicsError::InvalidParameter("upward knee outside 0..20 dB"));}
        Ok(Self {threshold_db,ratio,attack_ms,release_ms,output_db,knee_db,detector})
    }
    pub fn threshold_db(self)->f32{self.threshold_db}pub fn ratio(self)->f32{self.ratio}
    pub fn attack_ms(self)->u32{self.attack_ms}pub fn release_ms(self)->u32{self.release_ms}
    pub fn output_db(self)->f32{self.output_db}pub fn knee_db(self)->u32{self.knee_db}
    pub fn detector(self)->ObsDetector{self.detector}
    fn gain_db(self,envelope_db:f32)->f32{
        let mut diff=self.threshold_db-envelope_db;
        if envelope_db<=(self.threshold_db-60.)/2. {
            diff=if envelope_db+60.>0.{envelope_db+60.}else{0.};
        }
        let knee=self.knee_db as f32;let slope=1.-self.ratio;let mut gain=0.;
        if envelope_db>=self.threshold_db+knee/2.{gain=0.;}
        if self.threshold_db-knee/2.>=envelope_db{gain=slope*diff;}
        // At knee0 the open interval is empty; no division by zero occurs.
        if envelope_db>self.threshold_db-knee/2.&&self.threshold_db+knee/2.>envelope_db{
            gain=slope*(diff+knee/2.).powf(2.)/(2.*knee);
        }
        gain
    }
    pub fn transfer_db(self,input_db:f32)->Result<f32,ObsDynamicsError>{
        if !input_db.is_finite(){return Err(ObsDynamicsError::InvalidParameter("input_db"));}
        Ok(input_db+self.gain_db(input_db)+self.output_db)
    }
}
#[derive(Clone,Copy,Debug)]
pub struct ObsUpwardCompressor {
    rate:u32,channels:usize,parameters:ObsUpwardParameters,
    attack:f32,release:f32,output:f32,rms_coefficient:f32,runave:[f32;8],gain_db:[f32;8],
}
impl ObsUpwardCompressor {
    pub fn new(rate:u32,channels:u16,parameters:ObsUpwardParameters)->Result<Self,ObsDynamicsError>{
        let channels=format(rate,channels)?;
        let mut value=Self {rate,channels,parameters,attack:0.,release:0.,output:1.,rms_coefficient:(-100.0f32/rate as f32).exp2(),runave:[0.;8],gain_db:[0.;8]};
        value.set_parameters(parameters);Ok(value)
    }
    pub fn parameters(&self)->ObsUpwardParameters{self.parameters}
    pub fn set_parameters(&mut self,parameters:ObsUpwardParameters){
        self.parameters=parameters;
        self.attack=(-1.0f32/(self.rate as f32*(parameters.attack_ms as f32/1000.0f32))).exp();
        self.release=(-1.0f32/(self.rate as f32*(parameters.release_ms as f32/1000.0f32))).exp();
        self.output=db_to_mul(parameters.output_db);
    }
    pub fn reset(&mut self){self.runave=[0.;8];self.gain_db=[0.;8];}
    pub fn gain_increase_db(&self)->f32{self.gain_db[..self.channels].iter().copied().fold(0.,f32::max)}
    fn sample(&mut self,input:f32,channel:usize)->Result<f32,ObsDynamicsError>{
        let square=input.powf(2.0f32);
        if !square.is_finite(){return Err(ObsDynamicsError::NonRepresentable);}
        let envelope=match self.parameters.detector {
            ObsDetector::Rms=>{
                let average=self.rms_coefficient*self.runave[channel]+(1.0f32-self.rms_coefficient)*square;
                if !average.is_finite(){return Err(ObsDynamicsError::NonRepresentable);}
                self.runave[channel]=average;average.max(0.).sqrt()
            }
            ObsDetector::Peak=>{self.runave[channel]=square;input.abs()}
        };
        // mul_to_db(0) is -inf, and OBS's -60dB floor branch makes its gain0.
        let gain=self.parameters.gain_db(mul_to_db(envelope));
        let previous=self.gain_db[channel].max(0.);
        let next=if gain>previous{self.attack*previous+(1.0f32-self.attack)*gain}
            else{self.release*previous+(1.0f32-self.release)*gain};
        let output=input*(db_to_mul(next)*self.output);
        if !next.is_finite()||!output.is_finite(){return Err(ObsDynamicsError::NonRepresentable);}
        self.gain_db[channel]=next;Ok(output)
    }
    pub fn process_interleaved(&mut self,samples:&mut[f32],limits:DynamicsLimits)->Result<(),ObsDynamicsError>{
        block(samples,self.channels,limits,0,24)?;
        // All state is small Copy data. A dry arithmetic pass admits the whole
        // block before PCM/history mutation; it allocates no second audio block.
        let mut candidate=*self;
        for frame in samples.chunks_exact(self.channels){for (channel,value) in frame.iter().enumerate(){candidate.sample(*value,channel)?;}}
        let mut committed=*self;
        for frame in samples.chunks_exact_mut(self.channels){for (channel,value) in frame.iter_mut().enumerate(){*value=committed.sample(*value,channel)?;}}
        *self=candidate;Ok(())
    }
}
