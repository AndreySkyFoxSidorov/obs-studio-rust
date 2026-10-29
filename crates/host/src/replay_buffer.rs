//! Encoded Program replay buffer. Windows packet codecs, one shared renderer,
//! bounded GOP ring and a separate atomic MP4 save owner. No continuous file.
use crate::{audio_output::{self,OutputKey,OutputKind,StreamingAudioControl,StreamingAudioSink},native_aac,native_h264,recording::RecordingFrame,render_gpu::RenderGpu};
use skylive_core::{AudioSettings,ReplayBufferSettings,Session,VideoSettings};
use skylive_media::MediaTime;
use std::{path::PathBuf,sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,AtomicUsize,Ordering},mpsc},thread};
mod ring;
mod save;
mod worker;
pub const MAX_SAMPLES:usize=131_072;
static NEXT:AtomicU64=AtomicU64::new(1);
static OWNERS:AtomicUsize=AtomicUsize::new(0);
pub fn available()->bool{cfg!(target_os="windows")}
#[derive(Clone,Debug)]
pub struct ReplayConfig {pub encoder:skylive_core::VideoEncoder,pub keyframe_seconds:u32,pub video:VideoSettings,pub audio:AudioSettings,pub audio_enabled:bool,pub video_bitrate:u32,pub audio_bitrate:u32,pub buffer:ReplayBufferSettings}
impl ReplayConfig {
    pub fn from_session(session:&Session)->Result<Self,ReplayError>{let model=session.snapshot();let profile=model.profiles().selected();let recording=profile.recording();let value=Self {encoder:recording.encoder(),keyframe_seconds:recording.keyframe_interval_seconds(),video:model.output_video().map_err(|e|ReplayError::Detail(e.to_string()))?,audio:model.audio(),audio_enabled:recording.audio_enabled(),video_bitrate:recording.video_bitrate_kbps().checked_mul(1000).ok_or(ReplayError::Invalid("replay bitrate overflow"))?,audio_bitrate:recording.audio_bitrate_kbps()*1000,buffer:profile.replay_buffer()};value.validate()?;Ok(value)}
    pub fn validate(&self)->Result<(),ReplayError>{self.encoder.validate().map_err(|e|ReplayError::Detail(e.to_string()))?;if !(1..=10).contains(&self.keyframe_seconds){return Err(ReplayError::Invalid("Replay keyframe interval must be1..10seconds"));}if !(100_000..=100_000_000).contains(&self.video_bitrate){return Err(ReplayError::Invalid("Replay H264 packet encoding requires a recording video bitrate of100..100000 kbit/s"));}self.buffer.validate().map_err(|e|ReplayError::Detail(e.to_string()))?;self.h264(OutputKey {kind:OutputKind::ReplayBuffer,session:1,id:1,epoch:1})?;if self.audio_enabled {self.aac(OutputKey {kind:OutputKind::ReplayBuffer,session:1,id:1,epoch:1})?;}Ok(())}
    fn h264(&self,key:OutputKey)->Result<native_h264::Config,ReplayError>{let fps=(u64::from(self.video.fps_num())*u64::from(self.keyframe_seconds)/u64::from(self.video.fps_den())).clamp(1,1200) as u32;native_h264::Config::new(native_h264::Generation {session:key.session,output:key.id,encoder:key.epoch},self.video.width(),self.video.height(),self.video.fps_num(),self.video.fps_den(),self.video_bitrate,fps).and_then(|config|config.with_encoder(self.encoder)).map_err(|e|ReplayError::Detail(e.to_string()))}
    fn aac(&self,key:OutputKey)->Result<native_aac::Config,ReplayError>{native_aac::Config::new(native_aac::Generation {session:key.session,output:key.id,encoder:key.epoch},if self.audio.speakers().channels()==1{1}else{2},self.audio_bitrate).map_err(|e|ReplayError::Detail(e.to_string()))}
}
#[derive(Clone,Debug)]
pub enum ReplayError {Invalid(&'static str),Unavailable(&'static str),Detail(String),AfterPublication {path:PathBuf,detail:String},Busy,Closed,Cancelled}
impl ReplayError {pub fn code(&self)->&'static str{match self {Self::Invalid(_)=>"replay_invalid",Self::Unavailable(_)=>"replay_unavailable",Self::Detail(_)=>"replay_failed",Self::AfterPublication {..}=>"replay_published_cleanup",Self::Busy=>"replay_busy",Self::Closed=>"replay_closed",Self::Cancelled=>"replay_cancelled"}}}
impl std::fmt::Display for ReplayError {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self {Self::Invalid(s)|Self::Unavailable(s)=>write!(f,"{}: {s}",self.code()),Self::Detail(s)=>write!(f,"{}: {s}",self.code()),Self::AfterPublication {path,detail}=>write!(f,"{}: {}: {detail}",self.code(),path.display()),_=>f.write_str(self.code())}}}
impl std::error::Error for ReplayError {}
#[derive(Clone,Debug,Default,serde::Serialize)]
pub struct ReplayStatus {pub active:bool,pub starting:bool,pub stopping:bool,pub saving:bool,pub ready:bool,pub stored_bytes:u64,pub charged_bytes:u64,pub duration_100ns:u64,pub video_packets:u64,pub audio_packets:u64,pub dropped_gops:u64,pub memory_limited:bool,pub degraded_sources:usize,pub problem:Option<String>}
#[derive(Clone,Debug,serde::Serialize)]
pub struct ReplayReceipt {pub key_session:u64,pub key_id:u64,pub path:PathBuf,pub bytes:u64,pub duration_100ns:u64,pub video_samples:usize,pub audio_samples:usize}
pub enum ReplayEvent {ReadyForInputs {key:OutputKey},Started {key:OutputKey},Saved {key:OutputKey,result:Result<ReplayReceipt,ReplayError>},Failed {key:OutputKey,error:ReplayError},Stopped {key:OutputKey}}
enum Command {Activate(MediaTime),Save {path:PathBuf,cancel:Arc<AtomicBool>}}
struct Permit;
impl Drop for Permit {fn drop(&mut self){OWNERS.fetch_sub(1,Ordering::AcqRel);}}
struct Run {save_pending:bool,resources:Arc<AtomicU64>,key:OutputKey,commands:Option<mpsc::SyncSender<Command>>,frames:Option<mpsc::SyncSender<RecordingFrame>>,events:mpsc::Receiver<ReplayEvent>,status:Arc<Mutex<ReplayStatus>>,stop:Arc<AtomicBool>,save_cancel:Arc<AtomicBool>,closed:Arc<AtomicBool>,control:Option<StreamingAudioControl>,handle:Option<thread::JoinHandle<()>>,config:ReplayConfig}
pub struct ReplayOwner {gpu:RenderGpu,run:Option<Run>}
impl ReplayOwner {
    pub fn new(gpu:RenderGpu)->Self{Self {gpu,run:None}}
    pub fn key(&self)->Option<OutputKey>{self.run.as_ref().map(|r|r.key)}
    pub fn config(&self)->Option<&ReplayConfig>{self.run.as_ref().map(|r|&r.config)}
    pub fn active(&self)->bool{self.run.as_ref().is_some_and(|r|!r.closed.load(Ordering::Acquire))}
    pub fn status(&self)->ReplayStatus{self.run.as_ref().and_then(|r|r.status.lock().ok().map(|s|{let mut status=s.clone();status.saving|=r.save_pending;status})).unwrap_or_default()}
    pub fn start(&mut self,session:&Session,config:ReplayConfig)->Result<Option<StreamingAudioSink>,ReplayError>{
        if !available(){return Err(ReplayError::Unavailable("Replay packet encoders currently require Windows"));}if session.is_closed(){return Err(ReplayError::Closed);}if self.active()||self.run.as_ref().is_some_and(|r|r.save_pending){return Err(ReplayError::Busy);}self.run.take();config.validate()?;
        OWNERS.compare_exchange(0,1,Ordering::AcqRel,Ordering::Acquire).map_err(|_|ReplayError::Busy)?;let permit=Permit;
        let id=NEXT.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).map_err(|_|ReplayError::Invalid("replay identity exhausted"))?;
        let key=OutputKey {kind:OutputKind::ReplayBuffer,session:session.identity(),id,epoch:1};
        let (sink,receiver,control)=if config.audio_enabled {let (s,r,c)=audio_output::streaming_channel_track(key,config.audio,None,config.buffer.audio_track()).map_err(|e|ReplayError::Detail(e.to_string()))?;(Some(s),Some(r),Some(c))}else{(None,None,None)};
        let (send,commands)=mpsc::sync_channel(1);let (input,frames)=mpsc::sync_channel(1);let (events,receive)=mpsc::sync_channel(8);
        let status=Arc::new(Mutex::new(ReplayStatus {starting:true,..Default::default()}));let stop=Arc::new(AtomicBool::new(false));let closed=Arc::new(AtomicBool::new(false));let cancel=Arc::new(AtomicBool::new(false));
        let resources=Arc::new(AtomicU64::new(0));let resource_fence=resources.clone();
        let (s,t,c,g,cf,ac)=(status.clone(),stop.clone(),closed.clone(),self.gpu.clone(),config.clone(),control.clone());
        let handle=thread::Builder::new().name("skylive-replay-buffer".into()).spawn(move||worker::run(key,cf,g,commands,frames,events,s,t,c,receiver,ac,resource_fence,permit)).map_err(|e|ReplayError::Detail(e.to_string()))?;
        self.run=Some(Run {save_pending:false,resources,key,commands:Some(send),frames:Some(input),events:receive,status,stop,save_cancel:cancel,closed,control,handle:Some(handle),config});Ok(sink)
    }
    pub fn activate(&mut self,key:OutputKey,origin:MediaTime)->Result<(),ReplayError>{let r=self.run.as_ref().filter(|r|r.key==key&&!r.stop.load(Ordering::Acquire)&&!r.closed.load(Ordering::Acquire)).ok_or(ReplayError::Closed)?;r.commands.as_ref().ok_or(ReplayError::Closed)?.try_send(Command::Activate(origin)).map_err(|_|ReplayError::Busy)?;if let Some(c)=&r.control {if let Err(e)=c.activate(key,origin){r.stop.store(true,Ordering::Release);return Err(ReplayError::Detail(e.to_string()));}}Ok(())}
    pub fn submit(&mut self,key:OutputKey,frame:RecordingFrame)->Result<(),(ReplayError,RecordingFrame)>{
        let Some(r)=self.run.as_ref().filter(|r|r.key==key&&!r.stop.load(Ordering::Acquire))else{return Err((ReplayError::Closed,frame));};
        if frame.session!=key.session||!frame.model.output_video().is_ok_and(|video|video==r.config.video)||!audio_output::same_format(frame.model.audio(),r.config.audio){return Err((ReplayError::Invalid("replay frame session or format changed"),frame));}
        let previous=r.resources.fetch_max(frame.resource_generation,Ordering::AcqRel);if frame.resource_generation<previous{return Err((ReplayError::Invalid("obsolete replay resource generation"),frame));}
        match r.frames.as_ref().ok_or(ReplayError::Closed){Ok(sender)=>sender.try_send(frame).map_err(|e|match e {mpsc::TrySendError::Full(f)=>(ReplayError::Busy,f),mpsc::TrySendError::Disconnected(f)=>(ReplayError::Closed,f)}),Err(e)=>Err((e,frame))}
    }
    pub fn save(&mut self,key:OutputKey,path:PathBuf)->Result<(),ReplayError>{let r=self.run.as_mut().filter(|r|r.key==key&&!r.stop.load(Ordering::Acquire)).ok_or(ReplayError::Closed)?;let mut status=r.status.lock().map_err(|_|ReplayError::Closed)?;if !status.ready{return Err(ReplayError::Unavailable("Replay buffer has no complete keyframe-aligned clip yet"));}if status.saving||r.save_pending{return Err(ReplayError::Busy);}let cancel=Arc::new(AtomicBool::new(false));status.saving=true;if r.commands.as_ref().ok_or(ReplayError::Closed)?.try_send(Command::Save {path,cancel:cancel.clone()}).is_err(){status.saving=false;return Err(ReplayError::Busy);}r.save_cancel=cancel;r.save_pending=true;Ok(())}
    pub fn cancel_save(&self){if let Some(r)=&self.run{r.save_cancel.store(true,Ordering::Release);}}
    pub fn stop(&mut self){if let Some(r)=&mut self.run{if r.closed.load(Ordering::Acquire){return;}r.stop.store(true,Ordering::Release);r.frames.take();
        // Worker seals PCM at actual video cutoff, then waits for producer Drop.
        if let Ok(mut s)=r.status.lock(){s.stopping=true;s.ready=false;}}}
    pub fn poll(&mut self)->Option<ReplayEvent>{self.run.as_mut().and_then(|r|{if r.closed.load(Ordering::Acquire){r.control.take();}let event=r.events.try_recv().ok()?;if matches!(&event,ReplayEvent::Saved {..}){r.save_pending=false;}Some(event)})}
    pub fn close(&mut self){self.cancel_save();self.stop();if let Some(r)=&mut self.run{r.commands.take();}}
    pub fn join(&mut self){self.close();if let Some(r)=&mut self.run {if let Some(handle)=r.handle.take(){let _=handle.join();}}}
}
impl Drop for ReplayOwner {fn drop(&mut self){self.close();}}
