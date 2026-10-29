//! Complete encoded access units, bounded across both ring and pinned save.
use super::*;
use std::collections::VecDeque;
pub(super) enum Unit {Video(native_h264::AccessUnit),Audio {unit:native_aac::AccessUnit,offset:i64}}
impl Unit {
    pub fn bytes(&self)->&[u8]{match self {Self::Video(v)=>v.bytes(),Self::Audio {unit,..}=>unit.bytes()}}
    pub fn pts(&self)->i64{match self {Self::Video(v)=>v.pts(),Self::Audio {unit,offset}=>unit.pts()+offset}}
    pub fn end(&self)->i64{self.pts()+match self {Self::Video(v)=>v.duration(),Self::Audio {unit,..}=>unit.duration()}}
    pub fn key(&self)->bool{matches!(self,Self::Video(v)if v.keyframe())}
    fn charge(&self)->usize{self.bytes().len()+256}
}
pub(super) struct Clip {pub key:OutputKey,pub config:ReplayConfig,pub video:Vec<Arc<Unit>>,pub audio:Vec<Arc<Unit>>,pub avc:native_h264::CodecConfiguration,pub aac:Option<native_aac::CodecConfiguration>,pub start:i64,pub end:i64}
pub(super) struct Ring {config:ReplayConfig,video:VecDeque<Arc<Unit>>,audio:VecDeque<Arc<Unit>>,residents:Vec<Arc<Unit>>,avc:Option<native_h264::CodecConfiguration>,aac:Option<native_aac::CodecConfiguration>,pub dropped:u64,pub video_count:u64,pub audio_count:u64}
impl Ring {
    pub fn new(config:ReplayConfig)->Result<Self,ReplayError>{
        let mut residents=Vec::new();residents.try_reserve_exact(MAX_SAMPLES).map_err(|_|ReplayError::Invalid("replay metadata allocation"))?;
        Ok(Self {config,video:VecDeque::new(),audio:VecDeque::new(),residents,avc:None,aac:None,dropped:0,video_count:0,audio_count:0})
    }
    pub fn configuration_video(&mut self,config:native_h264::CodecConfiguration)->Result<(),ReplayError>{if self.avc.as_ref().is_some_and(|old|old.avcc()!=config.avcc()){return Err(ReplayError::Invalid("replay AVC configuration changed within an encoder epoch"));}self.avc=Some(config);Ok(())}
    pub fn configuration_audio(&mut self,config:native_aac::CodecConfiguration)->Result<(),ReplayError>{if self.aac.as_ref().is_some_and(|old|old!=&config){return Err(ReplayError::Invalid("replay AAC configuration changed within an encoder epoch"));}self.aac=Some(config);Ok(())}
    pub fn reap(&mut self){self.residents.retain(|p|Arc::strong_count(p)>1);}
    pub fn bytes(&self)->usize{self.residents.iter().map(|u|u.charge()).sum::<usize>()+8*1024*1024}
    fn trim_audio(&mut self){if let Some(first)=self.video.front(){while self.audio.len()>1&&self.audio.get(1).is_some_and(|a|a.end()<=first.pts()){self.audio.pop_front();}}}
    fn evict_gop(&mut self)->bool{
        let next=self.video.iter().enumerate().skip(1).find_map(|(i,v)|v.key().then_some(i));
        if let Some(next)=next{for _ in 0..next{self.video.pop_front();}self.trim_audio();true}else{false}
    }
    fn trim_duration(&mut self){let Some((_,end))=self.range()else{self.trim_audio();return;};let cutoff=end-i64::from(self.config.buffer.duration_seconds())*10_000_000;loop {let second=self.video.iter().skip(1).find(|v|v.key()).map(|v|v.pts());if second.is_some_and(|pts|pts<=cutoff){self.evict_gop();}else{break;}}self.trim_audio();}
    pub fn insert(&mut self,unit:Unit)->Result<(),ReplayError>{
        let is_video=matches!(unit,Unit::Video(_));if unit.end()<=unit.pts(){return Err(ReplayError::Invalid("replay access-unit time overflow"));}
        let previous=if is_video{self.video.back()}else{self.audio.back()};if previous.is_some_and(|p|if is_video{unit.pts()<p.end()}else{unit.pts()<=p.pts()}){return Err(ReplayError::Invalid("replay access-unit timestamps overlap or move backwards"));}
        if is_video&&self.video.is_empty()&&!unit.key(){return Ok(());}
        self.trim_duration();self.reap();let cap=usize::try_from(self.config.buffer.memory_bytes()).map_err(|_|ReplayError::Invalid("replay memory address space"))?;
        while self.residents.len()>=MAX_SAMPLES||self.bytes().checked_add(unit.charge()).is_none_or(|b|b>cap){if self.evict_gop(){self.dropped=self.dropped.saturating_add(1);self.reap();continue;}
            // An oversized live GOP cannot be truncated into undecodable P frames.
            // Drop that GOP and resume only at the next actual IDR. Pinned saves
            // remain charged; they are never made invisible to admission.
            if !self.video.is_empty(){self.dropped=self.dropped.saturating_add(1);}self.video.clear();self.audio.clear();self.reap();
            if !unit.key()||self.residents.len()>=MAX_SAMPLES||self.bytes().checked_add(unit.charge()).is_none_or(|b|b>cap){return Ok(());}break;
        }
        let unit=Arc::new(unit);self.residents.push(unit.clone());
        if is_video{self.video_count=self.video_count.saturating_add(1);self.video.push_back(unit);}else{self.audio_count=self.audio_count.saturating_add(1);self.audio.push_back(unit);}
        self.trim_duration();Ok(())
    }
    pub fn range(&self)->Option<(i64,i64)>{let start=self.video.front()?.pts();if !self.video.front()?.key()||self.avc.is_none(){return None;}let mut end=self.video.back()?.end();if self.config.audio_enabled {if self.aac.is_none(){return None;}end=end.min(self.audio.back()?.end());if self.audio.front()?.pts()>=end{return None;}}if end>start{Some((start,end))}else{None}}
    pub fn clip(&self,key:OutputKey)->Result<Clip,ReplayError>{self.clip_until(key,None)}
    pub fn clip_until(&self,key:OutputKey,cutoff:Option<i64>)->Result<Clip,ReplayError>{
        let (start,available)=self.range().ok_or(ReplayError::Unavailable("Replay buffer has no complete AVC/AAC range"))?;
        let end=if let Some(cutoff)=cutoff{if cutoff>available{return Err(ReplayError::Busy);}if cutoff<=start{return Err(ReplayError::Unavailable("Replay target interval was evicted before delayed audio arrived"));}cutoff}else{available};
        let video:Vec<_>=self.video.iter().filter(|p|p.pts()<end).cloned().collect();let first_audio=self.audio.iter().position(|p|p.end()>start).unwrap_or(self.audio.len()).saturating_sub(1);let audio:Vec<_>=self.audio.iter().skip(first_audio).take_while(|p|p.pts()<end).cloned().collect();
        if video.len()+audio.len()>MAX_SAMPLES||self.config.audio_enabled&&audio.is_empty(){return Err(ReplayError::Invalid("replay snapshot samples"));}
        Ok(Clip {key,config:self.config.clone(),video,audio,avc:self.avc.clone().ok_or(ReplayError::Unavailable("replay AVC headers unavailable"))?,aac:self.aac.clone(),start,end})
    }
    pub fn status(&self,status:&mut ReplayStatus){let range=self.range();status.ready=range.is_some();status.stored_bytes=self.residents.iter().map(|u|u.bytes().len() as u64).sum();status.charged_bytes=self.bytes() as u64;status.duration_100ns=range.map_or(0,|(s,e)|(e-s) as u64);status.video_packets=self.video_count;status.audio_packets=self.audio_count;status.dropped_gops=self.dropped;status.memory_limited=self.dropped!=0&&status.duration_100ns<u64::from(self.config.buffer.duration_seconds())*10_000_000;}
}
