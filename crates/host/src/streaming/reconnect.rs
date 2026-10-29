//! Sequential connection retirement/backoff on the existing service worker.
//! No native replacement before old codec/socket owners have actually returned.
use super::*;
pub(super) fn discard(work:&Receiver<Work>,pump:&mut Option<AudioPump>,status:&mut Status)->Result<(),Error>{
    for _ in 0..2{match work.try_recv(){Ok(_)=>add(&mut status.reconnect_video_discarded,1)?,Err(TryRecvError::Empty)=>break,Err(TryRecvError::Disconnected)=>return Err(Error::Disconnected)}}
    if let Some(p)=pump.as_mut(){p.discard_reconnecting(status)?;}Ok(())
}
pub(super) fn retire(ports:&mut Ports,work:&Receiver<Work>,pump:&mut Option<AudioPump>,status:&mut Status,state:&Mutex<Status>,controls:&Controls)->Result<(),Error>{
    if let Some(p)=&mut ports.publisher{p.cancel();}if let Some(v)=&mut ports.video{v.close();}if let Some(a)=&mut ports.audio{a.close();}
    let mut fault=None;
    loop{
        if controls.stop.load(Ordering::Acquire)||controls.cancel.load(Ordering::Acquire){while work.try_recv().is_ok(){}if let Some(p)=pump.as_ref(){p.stop();}if let Some(p)=pump.as_mut(){p.retire_discard();}}
        else if let Err(error)=discard(work,pump,status){if fault.is_none(){fault=Some(error);}}
        let v=ports.video.as_mut().is_none_or(|v|{if let Err(error)=v.poll(){if fault.is_none(){fault=Some(error);}}v.closed()});
        let a=ports.audio.as_mut().is_none_or(|a|{if let Err(error)=a.poll(){if fault.is_none(){fault=Some(error);}}a.closed()});
        let p=ports.publisher.as_mut().is_none_or(|p|{if let Err(error)=p.poll(){if !matches!(error,Error::Cancelled|Error::RetryableTransport(_))&&fault.is_none(){fault=Some(error);}}p.finished()});
        publish(state,status);if v&&a&&p{break;}thread::sleep(Duration::from_millis(5));
    }
    if let Some(p)=ports.publisher.take(){if let Err(error)=p.join(){if !matches!(error,Error::Cancelled|Error::RetryableTransport(_))&&fault.is_none(){fault=Some(error);}}}
    // Destruction completes inside the same service/codec/publisher credits.
    ports.video=None;ports.audio=None;if let Some(error)=fault{Err(error)}else{Ok(())}
}
pub(super) struct Backoff{pub attempts:u32,delay:u32,factor:f32}
impl Backoff{
    pub fn new(key:StreamKey)->Self{let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0,|d|d.subsec_nanos());let hash=key.output.wrapping_mul(0x9e3779b97f4a7c15)^u64::from(now);Self{attempts:0,delay:0,factor:1.5+(hash%1_000_000) as f32/1_000_000.0*0.05}}
    pub fn reset(&mut self){self.attempts=0;self.delay=0;}
    pub fn next(&mut self,policy:skylive_core::ReconnectSettings)->Option<u32>{if !policy.enabled()||self.attempts>=policy.max_attempts(){return None;}self.delay=if self.attempts==0{policy.delay_seconds()*1000}else{((self.delay as f32*self.factor) as u32).min(15*60*1000)};self.attempts+=1;Some(self.delay)}
}
pub(super) fn wait(delay:u32,work:&Receiver<Work>,pump:&mut Option<AudioPump>,status:&mut Status,state:&Mutex<Status>,controls:&Controls)->Result<bool,Error>{
    let until=std::time::Instant::now().checked_add(Duration::from_millis(u64::from(delay))).ok_or(Error::Limit("reconnect deadline"))?;
    loop{if controls.stop.load(Ordering::Acquire)||controls.cancel.load(Ordering::Acquire){status.reconnect_wait_ms=0;return Ok(false);}discard(work,pump,status)?;let left=until.saturating_duration_since(std::time::Instant::now());status.reconnect_wait_ms=u32::try_from(left.as_millis()).unwrap_or(u32::MAX);publish(state,status);if left.is_zero(){return Ok(true);}thread::sleep(left.min(Duration::from_millis(5)));}
}
