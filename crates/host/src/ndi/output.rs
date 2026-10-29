//! Explicit Program A/V and Preview video-only NDI output owners. Construction,
//! preference load and module enable are inert. SDK/renderer work is worker-only.
use super::NdiError;
use crate::{audio_output::{self,OutputKey,OutputKind,StreamingAudioSink,StreamingAudioControl},recording::RecordingFrame,render_gpu::RenderGpu,transition::{FrameComposition,FrameScene}};
use skylive_core::{NdiOutputRoute,NdiOutputSettings,Session,ProfileId,VideoSettings,AudioSettings,StudioSnapshot,NDI_PLUGIN};
use skylive_media::MediaTime;
use std::{sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,AtomicU8,AtomicUsize,Ordering},mpsc},thread};
#[cfg(all(target_os="windows",target_arch="x86_64"))] mod native_windows;
mod worker;
const FRAME_BYTES:usize=40*1024*1024;
const ALL_OUTPUT_BYTES:usize=160*1024*1024;
static ROUTES:AtomicU8=AtomicU8::new(0);
static WORKING_BYTES:AtomicUsize=AtomicUsize::new(0);
static NEXT:AtomicU64=AtomicU64::new(1);
pub fn available()->bool{cfg!(all(target_os="windows",target_arch="x86_64"))}
#[derive(Clone,Copy,Debug,PartialEq,Eq,serde::Serialize,serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {pub session:u64,pub run:u64,pub route:NdiOutputRoute}
impl Key {pub fn audio(self)->OutputKey{OutputKey {kind:OutputKind::NdiProgram,session:self.session,id:self.run,epoch:self.run}}}
#[derive(Clone,Debug)]
pub struct Config {pub session:u64,pub profile:ProfileId,pub route:NdiOutputRoute,pub video:VideoSettings,pub audio:Option<AudioSettings>,pub settings:NdiOutputSettings}
impl Config {
    pub fn from_session(session:&Session,route:NdiOutputRoute)->Result<Self,NdiError>{
        if session.is_closed(){return Err(NdiError::Closed);}let model=session.snapshot();
        if !model.preferences().plugin_enabled(NDI_PLUGIN){return Err(NdiError::Unavailable("Enable the DistroAV module before starting NDI output"));}
        let settings=model.preferences().ndi_output().clone();let video=match route{NdiOutputRoute::Program=>model.output_video().map_err(detail)?,NdiOutputRoute::Preview=>model.video().clone()};
        let value=Self {session:session.identity(),profile:model.profiles().selected_id(),route,video,audio:(route==NdiOutputRoute::Program&&settings.program_audio()).then(||model.audio()),settings};value.validate()?;Ok(value)
    }
    pub fn validate(&self)->Result<(),NdiError>{
        self.settings.validate().map_err(detail)?;
        if self.session==0||self.video.width()>4096||self.video.height()>4096||u64::from(self.video.width())*u64::from(self.video.height())>8_847_360||self.video.fps_num()>i32::MAX as u32||self.video.fps_den()>i32::MAX as u32||self.video.fps_den()==0||self.video.fps_num()<self.video.fps_den()||u64::from(self.video.fps_num())>120*u64::from(self.video.fps_den()){return Err(NdiError::Invalid("NDI output requires progressive SDR up to4096 edges/8.8Mp and1..120FPS"));}
        if self.route==NdiOutputRoute::Preview&&self.audio.is_some(){return Err(NdiError::Invalid("NDI Preview is video-only"));}
        if let Some(audio)=self.audio{if ![44100,48000].contains(&audio.sample_rate())||!(1..=8).contains(&audio.speakers().channels()){return Err(NdiError::Invalid("NDI Program PCM requires44.1/48kHz and1..8channels"));}}
        let bytes=self.video.width() as usize*self.video.height() as usize*4;if bytes>FRAME_BYTES{return Err(NdiError::Limit("NDI output RGBA frame bytes"));}Ok(())
    }
    fn matches(&self,session:u64,model:&StudioSnapshot)->bool{session==self.session&&model.profiles().selected_id()==self.profile&&model.preferences().plugin_enabled(NDI_PLUGIN)&&model.preferences().ndi_output()==&self.settings&&(match self.route{NdiOutputRoute::Program=>model.output_video().is_ok_and(|v|v==self.video),NdiOutputRoute::Preview=>model.video()==&self.video})&&self.audio.is_none_or(|audio|audio_output::same_format(audio,model.audio()))}
    pub fn composition(&self,session:&Session,program:&FrameComposition,studio:bool)->Result<FrameComposition,NdiError>{
        if !self.matches(session.identity(),&session.snapshot()){return Err(NdiError::Stale);}
        match self.route {NdiOutputRoute::Program=>Ok(program.clone()),NdiOutputRoute::Preview=>{let scene=if studio{session.preview()}else{program.primary_scene()};if session.snapshot().scene(scene).is_none(){return Err(NdiError::Invalid("NDI Preview scene is unavailable"));}Ok(FrameComposition::Scene(FrameScene::Live(scene)))}}
    }
}
fn detail(error:impl std::fmt::Display)->NdiError{NdiError::Runtime(error.to_string())}
#[derive(Clone,Debug,Default,serde::Serialize)]
pub struct Status {pub admitted:bool,pub starting:bool,pub backend_ready:bool,pub sending:bool,pub stopping:bool,pub width:u32,pub height:u32,pub fps_num:u32,pub fps_den:u32,pub audio:bool,pub video_frames:u64,pub audio_frames:u64,pub skipped_ticks:u64,pub degraded_sources:usize,pub connections:u32,pub advertised_name:Option<String>,pub advertised_url:Option<String>,pub runtime_version:Option<String>,pub problem:Option<String>}
pub enum Event {Ready {key:Key},Sending {key:Key},Failed {key:Key,error:NdiError},Stopped {key:Key}}
enum Command {Activate(MediaTime)}
struct Lease {bit:u8,bytes:usize}
impl Lease {fn acquire(config:&Config)->Result<Self,NdiError>{let bit=1<<config.route.index();let bytes=config.video.width() as usize*config.video.height() as usize*4*2;
    ROUTES.fetch_update(Ordering::AcqRel,Ordering::Acquire,|old|(old&bit==0).then_some(old|bit)).map_err(|_|NdiError::Busy)?;
    if WORKING_BYTES.fetch_update(Ordering::AcqRel,Ordering::Acquire,|old|old.checked_add(bytes).filter(|n|*n<=ALL_OUTPUT_BYTES)).is_err(){ROUTES.fetch_and(!bit,Ordering::AcqRel);return Err(NdiError::Limit("NDI output working bytes"));}Ok(Self {bit,bytes})}}
impl Drop for Lease {fn drop(&mut self){WORKING_BYTES.fetch_sub(self.bytes,Ordering::AcqRel);ROUTES.fetch_and(!self.bit,Ordering::AcqRel);}}
struct Run {key:Key,config:Config,origin:Option<MediaTime>,last_time:Option<MediaTime>,commands:Option<mpsc::SyncSender<Command>>,frames:Option<mpsc::SyncSender<RecordingFrame>>,events:mpsc::Receiver<Event>,stop:Arc<AtomicBool>,closed:Arc<AtomicBool>,status:Arc<Mutex<Status>>,revision:Arc<AtomicU64>,resources:Arc<AtomicU64>,control:Option<StreamingAudioControl>,handle:Option<thread::JoinHandle<()>>}
pub struct OutputOwner {gpu:RenderGpu,runs:[Option<Run>;2]}
impl OutputOwner {
    pub fn new(gpu:RenderGpu)->Self{Self {gpu,runs:[None,None]}}
    pub fn active(&self,route:NdiOutputRoute)->bool{self.runs[route.index()].as_ref().is_some_and(|r|!r.closed.load(Ordering::Acquire))}
    pub fn any_active(&self)->bool{self.active(NdiOutputRoute::Program)||self.active(NdiOutputRoute::Preview)}
    pub fn key(&self,route:NdiOutputRoute)->Option<Key>{self.runs[route.index()].as_ref().map(|r|r.key)}
    pub fn config(&self,route:NdiOutputRoute)->Option<&Config>{self.runs[route.index()].as_ref().map(|r|&r.config)}
    pub fn status(&self,route:NdiOutputRoute)->Status{self.runs[route.index()].as_ref().and_then(|r|r.status.lock().ok().map(|s|s.clone())).unwrap_or_default()}
    pub fn start(&mut self,session:&Session,route:NdiOutputRoute)->Result<Option<StreamingAudioSink>,NdiError>{
        if !available(){return Err(NdiError::Unavailable("NDI output currently requires Windows x64 and the official NDI6 Runtime"));}if self.active(route){return Err(NdiError::Busy);}
        let config=Config::from_session(session,route)?;let lease=Lease::acquire(&config)?;let id=NEXT.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).map_err(|_|NdiError::Limit("NDI output identity"))?;let key=Key {session:session.identity(),run:id,route};
        let (sink,audio,control)=if let Some(settings)=config.audio {let (s,r,c)=audio_output::streaming_channel_track(key.audio(),settings,None,config.settings.audio_track()).map_err(detail)?;(Some(s),Some(r),Some(c))}else{(None,None,None)};
        let (input,frames)=mpsc::sync_channel(1);let (commands,receive)=mpsc::sync_channel(1);let (publish,events)=mpsc::sync_channel(8);
        let stop=Arc::new(AtomicBool::new(false));let closed=Arc::new(AtomicBool::new(false));let revision=Arc::new(AtomicU64::new(session.snapshot().revision()));let resources=Arc::new(AtomicU64::new(0));
        let status=Arc::new(Mutex::new(Status {admitted:true,starting:true,width:config.video.width(),height:config.video.height(),fps_num:config.video.fps_num(),fps_den:config.video.fps_den(),audio:config.audio.is_some(),..Default::default()}));
        let (c,g,s,t,d,v,f,a)=(config.clone(),self.gpu.clone(),status.clone(),stop.clone(),closed.clone(),revision.clone(),resources.clone(),control.clone());
        let handle=thread::Builder::new().name(format!("skylive-ndi-send-{route:?}")).spawn(move||worker::run(key,c,g,receive,frames,audio,a,publish,s,t,d,v,f,lease)).map_err(|e|NdiError::Runtime(e.to_string()))?;
        self.runs[route.index()]=Some(Run {key,config,origin:None,last_time:None,commands:Some(commands),frames:Some(input),events,stop,closed,status,revision,resources,control,handle:Some(handle)});Ok(sink)
    }
    pub fn synchronize(&mut self,session:&Session){for route in [NdiOutputRoute::Program,NdiOutputRoute::Preview]{if let Some(run)=&self.runs[route.index()]{run.revision.store(session.snapshot().revision(),Ordering::Release);if session.is_closed()||!run.config.matches(session.identity(),&session.snapshot()){self.stop(route);}}}}
    pub fn activate(&mut self,key:Key,origin:MediaTime)->Result<(),NdiError>{let r=self.runs[key.route.index()].as_mut().filter(|r|r.key==key&&!r.stop.load(Ordering::Acquire)&&!r.closed.load(Ordering::Acquire)).ok_or(NdiError::Stale)?;
        if r.origin.is_some(){return Err(NdiError::Invalid("NDI output is already activated"));}
        r.commands.as_ref().ok_or(NdiError::Closed)?.try_send(Command::Activate(origin)).map_err(|_|NdiError::Busy)?;if let Some(control)=&r.control{if let Err(e)=control.activate(key.audio(),origin){r.stop.store(true,Ordering::Release);return Err(detail(e));}}r.origin=Some(origin);Ok(())}
    pub fn submit(&mut self,key:Key,frame:RecordingFrame)->Result<(),(NdiError,RecordingFrame)>{
        let Some(r)=self.runs[key.route.index()].as_mut().filter(|r|r.key==key&&!r.stop.load(Ordering::Acquire)&&!r.closed.load(Ordering::Acquire))else{return Err((NdiError::Stale,frame));};
        if r.origin.is_none_or(|origin|frame.time<origin)||r.last_time.is_some_and(|last|frame.time<=last){return Err((NdiError::Invalid("NDI output video requires a monotonic activated run clock"),frame));}
        if !r.config.matches(frame.session,&frame.model)||frame.playback.session()!=key.session||frame.playback.revision()!=frame.model.revision()||frame.playback.now()!=frame.time{return Err((NdiError::Stale,frame));}
        let revision=r.revision.fetch_max(frame.model.revision(),Ordering::AcqRel);let generation=r.resources.fetch_max(frame.resource_generation,Ordering::AcqRel);if frame.model.revision()<revision||frame.resource_generation<generation{return Err((NdiError::Stale,frame));}
        match r.frames.as_ref().ok_or(NdiError::Closed){Ok(sender)=>{let time=frame.time;match sender.try_send(frame){Ok(())=>{r.last_time=Some(time);Ok(())},Err(e)=>Err(match e {mpsc::TrySendError::Full(frame)=>(NdiError::Busy,frame),mpsc::TrySendError::Disconnected(frame)=>(NdiError::Closed,frame)})}},Err(error)=>Err((error,frame))}
    }
    pub fn poll(&mut self)->Option<Event>{for run in self.runs.iter_mut().flatten(){if run.closed.load(Ordering::Acquire){run.control.take();}if let Ok(event)=run.events.try_recv(){return Some(event);}}None}
    pub fn stop(&mut self,route:NdiOutputRoute){if let Some(r)=&mut self.runs[route.index()]{r.stop.store(true,Ordering::Release);r.frames.take();r.commands.take();if let Some(c)=&r.control{c.request_stop();}if !r.closed.load(Ordering::Acquire){if let Ok(mut s)=r.status.lock(){s.stopping=true;s.sending=false;}}}}
    pub fn close(&mut self){self.stop(NdiOutputRoute::Program);self.stop(NdiOutputRoute::Preview);}
    /// CLI/background shutdown only; native SDK calls have no fictitious timeout.
    pub fn join(&mut self){self.close();for r in self.runs.iter_mut().flatten(){if let Some(h)=r.handle.take(){let _=h.join();}}}
}
impl Drop for OutputOwner {fn drop(&mut self){self.close();}}
pub fn blocks_command(command:&skylive_core::Command)->bool{crate::recording::blocks_format_change(command)||matches!(command,skylive_core::Command::SetNdiOutputSettings {..})}
