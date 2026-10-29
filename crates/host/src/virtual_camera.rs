//! Windows virtual-device output. Native registration and rendering stay on one
//! worker. Construction never starts MF, loads drivers or installs a COM class.
use crate::{render_gpu::RenderGpu,streaming::ProgramVideoFrame};
use skylive_virtual_camera_protocol::Profile;
mod routing;
pub use routing::{RouteFactory,RoutedVideoFrame};
mod installer;
pub use installer::request as request_install;
use std::{fmt,sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,AtomicUsize,Ordering},mpsc::{self,Receiver,SyncSender}},thread,time::Duration};
#[cfg(all(target_os="windows",target_arch="x86_64"))]
mod native;
#[derive(Clone,Debug)]
pub enum Error {Unsupported(&'static str),NotInstalled(String),AdminRequired(String),Invalid(&'static str),RouteUnavailable(String),RouteWaiting(String),Native(String),Backpressure,Closed,Limit(&'static str)}
impl Error {pub fn code(&self)->&'static str{match self {
    Self::Unsupported(_)=>"virtual_camera_unsupported",Self::NotInstalled(_)=>"virtual_camera_installation_required",
    Self::RouteUnavailable(_)=>"virtual_camera_route_unavailable",Self::RouteWaiting(_)=>"virtual_camera_route_waiting",
    Self::AdminRequired(_)=>"virtual_camera_admin_required",Self::Invalid(_)=>"virtual_camera_invalid",Self::Native(_)=>"virtual_camera_native",
    Self::Backpressure=>"virtual_camera_backpressure",Self::Closed=>"virtual_camera_closed",Self::Limit(_)=>"virtual_camera_limit",
}}}
impl fmt::Display for Error {fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result{write!(f,"{}: {self:?}",self.code())}}
impl std::error::Error for Error {}
/// The native run leases the selected video profile. Audio settings remain
/// independent because this producer does not own a microphone/Program sink.
pub fn blocks_format_change(command:&skylive_core::Command)->bool {
    crate::recording::blocks_format_change(command)&&!matches!(command,skylive_core::Command::SetAudioSettings {..})
}
#[derive(Clone,Debug)]
pub struct Config {session:u64,profile:Profile,route:skylive_core::VirtualCameraRoute}
impl Config {
    pub fn new(session:u64,width:u32,height:u32,fps:u32)->Result<Self,Error>{
        if session==0{return Err(Error::Invalid("session must be nonzero"));}
        let profile=Profile::new(width,height,fps).ok_or(Error::Invalid("supported output profiles are1280x720@30 and1920x1080@30 NV12"))?;
        Ok(Self {session,profile,route:skylive_core::VirtualCameraRoute::Program})
    }
    pub fn for_route(session:&skylive_core::Session,route:skylive_core::VirtualCameraRoute,width:u32,height:u32,fps:u32)->Result<Self,Error>{
        if session.is_closed(){return Err(Error::Closed);}
        route.validate(&session.snapshot()).map_err(|error|Error::RouteUnavailable(error.to_string()))?;
        let mut config=Self::new(session.identity(),width,height,fps)?;config.route=route;Ok(config)
    }
    pub fn route(&self)->skylive_core::VirtualCameraRoute{self.route}
    pub fn session(&self)->u64{self.session}
    pub fn width(&self)->u32{self.profile.width()}
    pub fn height(&self)->u32{self.profile.height()}
    pub fn fps(&self)->u32{self.profile.fps()}
}
#[derive(Clone,Debug,Default)]
pub struct Status {pub active:bool,pub starting:bool,pub stopping:bool,pub frames:u64,pub source_starts:u64,pub consumer_active:bool,pub waiting_for_frame:bool,pub problem:Option<Error>}
pub enum Event {Started,Stopped,Failed(Error)}
static SERVICE:AtomicBool=AtomicBool::new(false);
struct ServiceCredit;
impl ServiceCredit {fn acquire()->Result<Self,Error>{SERVICE.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).map_err(|_|Error::Backpressure)?;Ok(Self)}}
impl Drop for ServiceCredit {fn drop(&mut self){SERVICE.store(false,Ordering::Release);}}
struct Signals {close:AtomicBool,stop:AtomicBool,latest:AtomicU64,frames:AtomicUsize,status:Mutex<Status>}
struct FrameCredit(Arc<Signals>);
impl Drop for FrameCredit{fn drop(&mut self){self.0.frames.fetch_sub(1,Ordering::AcqRel);}}
enum Work {Start(Config),Video {frame:ProgramVideoFrame,identity:u64,_credit:FrameCredit}}
#[derive(PartialEq,Eq)]
struct FrameScope {revision:u64,resources:u64,capture_epoch:u64,playback_epoch:u64,output:crate::transition::OutputKey}
pub struct Owner {sender:Option<SyncSender<Work>>,events:Receiver<Event>,signals:Arc<Signals>,config:Option<Config>,scope:Option<FrameScope>,pending:bool,_thread:thread::JoinHandle<()>}
impl Owner {
    pub fn new(render_gpu:RenderGpu)->std::io::Result<Self>{
        let signals=Arc::new(Signals {close:AtomicBool::new(false),stop:AtomicBool::new(false),latest:AtomicU64::new(0),frames:AtomicUsize::new(0),status:Mutex::new(Status::default())});
        let (sender,input)=mpsc::sync_channel(2);let (output,events)=mpsc::sync_channel(2);let state=signals.clone();
        let handle=thread::Builder::new().name("skylive-virtual-camera".into()).spawn(move||worker(input,output,state,render_gpu))?;
        Ok(Self {sender:Some(sender),events,signals,config:None,scope:None,pending:false,_thread:handle})
    }
    pub fn start(&mut self,config:Config)->Result<(),Error>{
        if !cfg!(all(target_os="windows",target_arch="x86_64")){return Err(Error::Unsupported("Windows11 x64 build22000 required"));}
        let mut status=self.signals.status.lock().map_err(|_|Error::Closed)?;
        if self.pending || status.active || status.starting || status.stopping{return Err(Error::Invalid("virtual camera already owned; consume its terminal event first"));}
        self.signals.stop.store(false,Ordering::Release);
        self.sender.as_ref().ok_or(Error::Closed)?.try_send(Work::Start(config.clone())).map_err(|_|Error::Backpressure)?;
        *status=Status {starting:true,..Default::default()};self.config=Some(config);self.scope=None;self.pending=true;Ok(())
    }
    /// Legacy Program-only port cannot silently inject another configured route.
    pub fn video(&mut self,frame:ProgramVideoFrame)->Result<(),Error>{
        if self.config.as_ref().is_some_and(|c|c.route()!=skylive_core::VirtualCameraRoute::Program){return Err(Error::Invalid("submit the configured route through RouteFactory"));}
        self.video_inner(frame)
    }
    pub fn routed_video(&mut self,frame:RoutedVideoFrame)->Result<(),Error>{
        if self.config.as_ref().is_none_or(|c|c.route()!=frame.route){return Err(Error::Invalid("virtual-camera route does not match this run"));}
        self.video_inner(frame.frame)
    }
    fn video_inner(&mut self,frame:ProgramVideoFrame)->Result<(),Error>{
        let config=self.config.as_ref().ok_or(Error::Invalid("start virtual camera first"))?;let status=self.status();
        if !status.active || status.starting || status.stopping || self.signals.stop.load(Ordering::Acquire){return Err(Error::Invalid("virtual camera is not accepting selected video frames"));}
        if frame.session!=config.session || frame.playback.session()!=config.session || frame.capture.session()!=config.session
            || frame.playback.revision()!=frame.model.revision() || frame.time!=frame.playback.now() {return Err(Error::Invalid("foreign or stale Program frame"));}
        self.signals.frames.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|(v<2).then_some(v+1)).map_err(|_|Error::Backpressure)?;
        let credit=FrameCredit(self.signals.clone());
        let scope=FrameScope {revision:frame.model.revision(),resources:frame.resource_generation,capture_epoch:frame.capture.epoch(),playback_epoch:frame.playback.epoch(),output:frame.output.identity()};
        if self.scope.as_ref().is_none_or(|old|old!=&scope) {
            self.signals.latest.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).map_err(|_|Error::Limit("Program identity exhausted"))?;
            self.scope=Some(scope);
        }
        // Advancing live time alone must not starve a bounded renderer. An
        // admitted older sample remains publishable within the exact same view;
        // new authored/output/resource generations revoke that publication.
        let identity=self.signals.latest.load(Ordering::Acquire);
        self.sender.as_ref().ok_or(Error::Closed)?.try_send(Work::Video {frame,identity,_credit:credit})
            .map_err(|error|match error {mpsc::TrySendError::Full(_)=>Error::Backpressure,mpsc::TrySendError::Disconnected(_)=>Error::Closed})
    }
    pub fn stop(&mut self){self.signals.stop.store(true,Ordering::Release);if let Ok(mut status)=self.signals.status.lock(){if status.active || status.starting{status.stopping=true;}}}
    pub fn close(&mut self){self.stop();self.signals.close.store(true,Ordering::Release);self.sender.take();}
    pub fn poll(&mut self)->Option<Event>{let event=self.events.try_recv().ok()?;if matches!(&event,Event::Stopped|Event::Failed(_)){self.pending=false;self.config=None;self.scope=None;}Some(event)}
    pub fn status(&self)->Status{self.signals.status.lock().map(|v|v.clone()).unwrap_or_else(|_|Status {problem:Some(Error::Closed),..Default::default()})}
}
impl Drop for Owner {fn drop(&mut self){self.close();}}
fn failed(state:&Signals,events:&SyncSender<Event>,error:Error){if let Ok(mut status)=state.status.lock(){*status=Status {problem:Some(error.clone()),..Default::default()};}let _=events.try_send(Event::Failed(error));}
fn stopped(state:&Signals,events:&SyncSender<Event>){if let Ok(mut status)=state.status.lock(){status.active=false;status.starting=false;status.stopping=false;status.consumer_active=false;status.waiting_for_frame=false;status.problem=None;}let _=events.try_send(Event::Stopped);}
#[cfg(not(all(target_os="windows",target_arch="x86_64")))]
fn worker(input:Receiver<Work>,events:SyncSender<Event>,state:Arc<Signals>,_gpu:RenderGpu){while let Ok(work)=input.recv(){if matches!(work,Work::Start(_)){failed(&state,&events,Error::Unsupported("Windows11 x64 build22000 required"));}if state.close.load(Ordering::Acquire){break;}}}
#[cfg(all(target_os="windows",target_arch="x86_64"))]
fn worker(input:Receiver<Work>,events:SyncSender<Event>,state:Arc<Signals>,gpu:RenderGpu){
    let mut active:Option<native::Active>=None;
    while !state.close.load(Ordering::Acquire) {
        if state.stop.load(Ordering::Acquire) && active.is_some(){
            state.stop.store(false,Ordering::Release);
            if let Some(mut owner)=active.take(){match owner.stop(){Ok(())=>stopped(&state,&events),Err(error)=>failed(&state,&events,error)}}
        }
        let work=match input.recv_timeout(Duration::from_millis(10)) {Ok(value)=>Some(value),Err(mpsc::RecvTimeoutError::Timeout)=>None,Err(_)=>break};
        match work {
            Some(Work::Start(config))=>{
                if state.stop.load(Ordering::Acquire){state.stop.store(false,Ordering::Release);stopped(&state,&events);continue;}
                if active.is_some(){failed(&state,&events,Error::Invalid("virtual camera already active"));continue;}
                match native::Active::start(config,gpu.clone()) {
                    Ok(owner)=>{
                        if state.close.load(Ordering::Acquire) || state.stop.load(Ordering::Acquire) {let mut owner=owner;let _=owner.stop();stopped(&state,&events);}
                        else{active=Some(owner);if let Ok(mut status)=state.status.lock(){status.starting=false;status.active=true;}let _=events.try_send(Event::Started);}
                    }
                    Err(error)=>failed(&state,&events,error),
                }
            }
            Some(Work::Video {frame,identity,..})=>{
                if let Some(owner)=active.as_mut(){
                    match owner.video(frame,identity,&state) {
                        Ok(())=>{if let Ok(mut status)=state.status.lock(){status.waiting_for_frame=false;status.problem=None;}},
                        Err(error@Error::RouteWaiting(_))=>{if let Ok(mut status)=state.status.lock(){status.waiting_for_frame=true;status.problem=Some(error);}},
                        Err(Error::Backpressure)=>{},Err(error)=>{let _=owner.stop();active=None;failed(&state,&events,error);}}
                }
            }
            None=>{},
        }
        if let Some(owner)=active.as_ref(){if let Ok(telemetry)=owner.telemetry(){if let Ok(mut status)=state.status.lock(){status.frames=telemetry.frames;status.source_starts=telemetry.source_starts;status.consumer_active=telemetry.consumers>0;}}}
    }
    if let Some(mut owner)=active.take(){let result=owner.stop();match result{Ok(())=>stopped(&state,&events),Err(error)=>failed(&state,&events,error)}}
    else if state.status.lock().is_ok_and(|status|status.starting || status.stopping){stopped(&state,&events);}
}
