//! One lazy physical-output owner, independent of recording/streaming Program.
//! All native calls and final native releases happen on its MTA worker.
use serde::Serialize;
use skylive_core::{AudioMonitoringDevice,AudioSettings};
use std::{sync::{Arc,Mutex,mpsc,atomic::{AtomicU64,AtomicBool,Ordering}},thread,time::Duration};
#[cfg(target_os="windows")]
#[path="audio_monitor/native_windows.rs"]
mod native;
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct MonitorConfig {pub session:u64,pub settings:AudioSettings,pub device:AudioMonitoringDevice}
pub struct MonitorFrame {pub settings:AudioSettings,pub start_frame:u64,pub samples:Vec<f32>}
#[derive(Clone,Debug,Default,Serialize)]
pub struct MonitorStatus {
    pub requested:bool,pub active:bool,pub stopping:bool,pub generation:u64,
    pub native_format:Option<crate::audio_capture::NativeAudioFormat>,
    pub device_id:Option<String>,pub device_name:Option<String>,pub frames_written:u64,
    pub dropped_blocks:u64,pub device_padding_ns:u64,pub problem:Option<String>,
}
struct Desired {generation:u64,config:Option<MonitorConfig>}
struct Shared {desired:Mutex<Desired>,generation:AtomicU64,stop:AtomicBool,status:Mutex<MonitorStatus>}
struct Queued {generation:u64,frame:MonitorFrame}
#[derive(Clone)]
pub struct MonitorSink {shared:Arc<Shared>,sender:Option<mpsc::SyncSender<Queued>>,generation:u64,enabled:bool}
impl MonitorSink {
    pub fn status(&self)->MonitorStatus {self.shared.status.lock().unwrap_or_else(|e|e.into_inner()).clone()}
    pub fn submit(&self,frame:MonitorFrame){
        if !self.enabled||self.shared.generation.load(Ordering::Acquire)!=self.generation{return;}
        let channels=usize::from(frame.settings.channels());
        if frame.samples.is_empty()||frame.samples.len()>1024*channels||frame.samples.capacity()>1024*channels||frame.samples.len()%channels!=0||frame.samples.iter().any(|s|!s.is_finite()) {self.shared.status.lock().unwrap_or_else(|e|e.into_inner()).problem=Some("invalid monitoring PCM block".into());return;}
        if self.sender.as_ref().is_none_or(|tx|tx.try_send(Queued {generation:self.generation,frame}).is_err()) {
            let mut status=self.shared.status.lock().unwrap_or_else(|e|e.into_inner());status.dropped_blocks=status.dropped_blocks.saturating_add(1);status.problem=Some("monitor output queue is full or disconnected; block was dropped".into());
        }
    }
    pub fn stop(&self){
        let mut desired=self.shared.desired.lock().unwrap_or_else(|e|e.into_inner());
        if desired.generation==self.generation {
            if let Some(generation)=desired.generation.checked_add(1){desired.generation=generation;desired.config=None;self.shared.generation.store(generation,Ordering::Release);let mut status=self.shared.status.lock().unwrap_or_else(|e|e.into_inner());status.requested=false;status.stopping=status.active;status.generation=generation;}
        }
    }
}
pub struct MonitorOwner {allow_native:bool,shared:Arc<Shared>,sender:Option<mpsc::SyncSender<Queued>>,thread:Option<thread::JoinHandle<()>>}
impl Default for MonitorOwner {
    fn default()->Self {Self {allow_native:false,shared:Arc::new(Shared {desired:Mutex::new(Desired {generation:0,config:None}),generation:AtomicU64::new(0),stop:AtomicBool::new(false),status:Mutex::new(MonitorStatus::default())}),sender:None,thread:None}}
}
impl MonitorOwner {
    pub fn native()->Self {let mut owner=Self::default();owner.allow_native=cfg!(target_os="windows");owner}
    pub fn configure(&mut self,config:Option<MonitorConfig>)->Result<MonitorSink,String>{
        if config.is_some()&&!self.allow_native{return Err("physical audio monitoring is available only on Windows".into());}
        if let Some(config)=&config{if config.session==0{return Err("monitoring requires a live Session identity".into());}config.device.validate().map_err(|e|e.to_string())?;}
        if config.is_some()&&self.thread.is_none(){
            let (tx,rx)=mpsc::sync_channel(8);let shared=self.shared.clone();
            let handle=thread::Builder::new().name("skylive-wasapi-monitor".into()).spawn(move||run(shared,rx)).map_err(|e|e.to_string())?;
            self.sender=Some(tx);self.thread=Some(handle);
        }
        let mut desired=self.shared.desired.lock().unwrap_or_else(|e|e.into_inner());
        if desired.config!=config {
            let generation=desired.generation.checked_add(1).ok_or_else(||{self.shared.stop.store(true,Ordering::Release);"monitor generation exhausted; physical output is stopping"})?;
            desired.config=config;desired.generation=generation;
            self.shared.generation.store(generation,Ordering::Release);
            let mut status=self.shared.status.lock().unwrap_or_else(|e|e.into_inner());status.requested=desired.config.is_some();status.stopping=status.active;status.generation=generation;
        }
        Ok(MonitorSink {shared:self.shared.clone(),sender:self.sender.clone(),generation:desired.generation,enabled:desired.config.is_some()})
    }
    pub fn join(&mut self){self.shared.stop.store(true,Ordering::Release);self.sender.take();if let Some(handle)=self.thread.take(){let _=handle.join();}}
}
impl Drop for MonitorOwner {fn drop(&mut self){self.shared.stop.store(true,Ordering::Release);self.sender.take();}}
#[cfg(not(target_os="windows"))]
fn run(shared:Arc<Shared>,_rx:mpsc::Receiver<Queued>){shared.status.lock().unwrap_or_else(|e|e.into_inner()).problem=Some("Windows audio monitoring backend unavailable".into());}
#[cfg(target_os="windows")]
fn run(shared:Arc<Shared>,rx:mpsc::Receiver<Queued>){
    let mut generation=0;let mut output:Option<native::Render>=None;let mut faulted=false;
    loop {
        let stopping=shared.stop.load(Ordering::Acquire);
        let target_generation=shared.generation.load(Ordering::Acquire);
        if stopping||generation!=target_generation {
            if let Some(render)=&mut output {
                match render.close(){Ok(())=>output=None,Err(error)=>{
                    let mut status=shared.status.lock().unwrap_or_else(|e|e.into_inner());status.stopping=true;status.problem=Some(error);drop(status);thread::sleep(Duration::from_millis(5));continue;
                }}
            }
            {let mut status=shared.status.lock().unwrap_or_else(|e|e.into_inner());status.active=false;status.stopping=false;}
            if stopping {break;}
            let (next,config)={let d=shared.desired.lock().unwrap_or_else(|e|e.into_inner());(d.generation,d.config.clone())};
            generation=next;faulted=false;
            if let Some(config)=config {
                match native::Render::open(&config){
                    Ok(render)=>{let mut status=shared.status.lock().unwrap_or_else(|e|e.into_inner());status.active=true;status.problem=None;status.device_id=Some(render.device_id().to_owned());status.device_name=Some(render.device_name().to_owned());status.native_format=Some(render.native_format());output=Some(render);},
                    Err(error)=>{shared.status.lock().unwrap_or_else(|e|e.into_inner()).problem=Some(error);faulted=true;},
                }
            }
        }
        // Control generation is checked before every queue admission and native
        // render pass. Old queued PCM cannot migrate to a newly chosen endpoint.
        let queued=match rx.recv_timeout(Duration::from_millis(5)){Ok(q)=>Some(q),Err(mpsc::RecvTimeoutError::Timeout)=>None,Err(mpsc::RecvTimeoutError::Disconnected)=>break};
        if shared.generation.load(Ordering::Acquire)!=generation||shared.stop.load(Ordering::Acquire){continue;}
        if let Some(render)=&mut output {
            let result=if faulted {render.close().map(|()|0)}else{
                if let Some(q)=queued.filter(|q|q.generation==generation){render.push(q.frame)}else{Ok(())}.and_then(|()|render.pump())
            };
            match result {
                Ok(frames)=>{let mut status=shared.status.lock().unwrap_or_else(|e|e.into_inner());status.frames_written=status.frames_written.saturating_add(frames as u64);status.device_padding_ns=render.padding_ns();if faulted {status.active=false;status.stopping=false;output=None;}},
                Err(error)=>{let mut status=shared.status.lock().unwrap_or_else(|e|e.into_inner());status.problem=Some(error);status.stopping=true;faulted=true;},
            }
        }
    }
    if let Some(render)=&mut output {while render.close().is_err(){thread::sleep(Duration::from_millis(5));}}
    let mut status=shared.status.lock().unwrap_or_else(|e|e.into_inner());status.active=false;status.stopping=false;
}
