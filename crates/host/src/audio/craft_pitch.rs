//! SoundCraft-style two-head real-time pitch shifting; fixed 50 ms window.
use skylive_core::AudioFilter;
use super::AudioError;

#[derive(Clone)]
pub(super) struct Pitch {
    rate:u32, channels:usize, window:usize, cursor:usize, lines:Vec<f32>, phase:f32,
    shift:f32, shift_target:f32, mix:f32, mix_target:f32,
}
impl Pitch {
    pub(super) fn required_bytes(rate:u32,channels:u16)->Result<usize,AudioError>{
        if rate==0||channels==0{return Err(AudioError::InvalidRequest("invalid pitch format"));}
        let window=AudioFilter::craft_pitch_window_frames(rate) as usize;
        window.checked_add(8).and_then(|n|n.checked_mul(channels as usize)).and_then(|n|n.checked_mul(4)).ok_or(AudioError::Limit("pitch state overflow"))
    }
    pub(super) fn new(rate:u32,channels:u16,filter:AudioFilter)->Result<Self,AudioError>{
        let bytes=Self::required_bytes(rate,channels)?;
        let mut s=Self{rate,channels:channels as usize,window:AudioFilter::craft_pitch_window_frames(rate) as usize,cursor:0,lines:vec![0.;bytes/4],phase:0.,shift:0.,shift_target:0.,mix:1.,mix_target:1.};
        s.update(filter)?;s.shift=s.shift_target;s.mix=s.mix_target;Ok(s)
    }
    pub(super) fn update(&mut self,filter:AudioFilter)->Result<(),AudioError>{
        filter.validate().map_err(|_|AudioError::InvalidRequest("invalid pitch settings"))?;
        let AudioFilter::CraftPitch{semitones,cents,mix_pct,..}=filter else{return Err(AudioError::InvalidRequest("invalid pitch filter"));};
        self.shift_target=semitones+cents/100.;self.mix_target=mix_pct/100.;Ok(())
    }
    pub(super) fn reset(&mut self){self.lines.fill(0.);self.cursor=0;self.phase=0.;self.shift=self.shift_target;self.mix=self.mix_target;}
    pub(super) fn state_bytes(&self)->usize{self.lines.len()*4}
    pub(super) fn process(&mut self,samples:&mut [f32],channels:usize)->Result<(),AudioError>{
        if channels==0||channels!=self.channels||samples.len()%channels!=0{return Err(AudioError::InvalidRequest("pitch channel mismatch"));}
        let capacity=self.window+8;let shift_coeff=1.-(-1./(self.rate as f32*0.030).max(1.)).exp();let mix_coeff=1.-(-1./(self.rate as f32*0.020).max(1.)).exp();
        for frame in samples.chunks_exact_mut(channels){
            self.shift+=(self.shift_target-self.shift)*shift_coeff;self.mix+=(self.mix_target-self.mix)*mix_coeff;
            let ratio=2f32.powf(self.shift/12.);self.phase=(self.phase+(1.-ratio)/self.window as f32).rem_euclid(1.);
            let p2=(self.phase+0.5).rem_euclid(1.);let g1=(std::f32::consts::PI*self.phase).sin().powi(2);
            for (c,x) in frame.iter_mut().enumerate(){
                self.lines[self.cursor*channels+c]=if x.is_finite(){*x}else{0.};
                // SoundCraft advances its write position on push before reading.
                let read=|delay:f32|{let at=(self.cursor as f32+1.-delay).rem_euclid(capacity as f32);let a=at.floor() as usize;let frac=at-a as f32;let b=(a+1)%capacity;self.lines[a*channels+c]*(1.-frac)+self.lines[b*channels+c]*frac};
                let wet=g1*read(self.phase*self.window as f32+2.)+(1.-g1)*read(p2*self.window as f32+2.);
                let dry=read(self.window as f32*0.5+2.);*x=dry*(1.-self.mix)+wet*self.mix;
            }
            self.cursor=(self.cursor+1)%capacity;
        }Ok(())
    }
}
