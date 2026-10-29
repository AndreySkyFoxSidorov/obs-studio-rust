//! Genuine D3D11 game backbuffer owner, separate from WGC/window/desktop capture.
//! Constructing is inert; injection requires explicit Session-only authorization.
use skylive_core::{GameCaptureBinding,GameCaptureTarget,Id,Session,SourceKind};
use skylive_media::{MediaTime,RgbaFrame};
use crate::transition::FrameScene;
use std::{collections::HashSet,sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,Ordering},mpsc},thread,time::{Duration,Instant}};
#[cfg(all(target_os="windows",target_arch="x86_64"))]mod native;
const MAX_REFS:usize=16;
static NEXT:AtomicU64=AtomicU64::new(1);
#[derive(Clone,Debug)]
pub enum GameError {Unsupported(&'static str),Invalid(&'static str),Limit(&'static str),AuthorizationRequired,TargetChanged,
    Native(String),AfterInjection(String),Retiring(String),Busy,Closed,Stale}
impl GameError {pub fn code(&self)->&'static str{match self {
    Self::Unsupported(_)=>"game_capture_unsupported",Self::Invalid(_)=>"game_capture_invalid",Self::Limit(_)=>"game_capture_limit",
    Self::AuthorizationRequired=>"game_capture_authorization_required",Self::TargetChanged=>"game_capture_target_changed",
    Self::Native(_)=>"game_capture_native",Self::AfterInjection(_)=>"game_capture_after_injection",Self::Retiring(_)=>"game_capture_retiring",
    Self::Busy=>"game_capture_busy",Self::Closed=>"game_capture_closed",Self::Stale=>"game_capture_stale",
}}}
impl std::fmt::Display for GameError {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {write!(f,"{}: {self:?}",self.code())}}
impl std::error::Error for GameError {}
#[derive(Clone,Debug,PartialEq,Eq,serde::Serialize)]
pub struct GameTargetInfo {pub target:GameCaptureTarget,pub width:u32,pub height:u32}
#[derive(Clone,Copy,Debug,PartialEq,Eq,serde::Serialize)]
pub struct GameRequestKey {pub owner:u64,pub session:u64,pub revision:u64,pub serial:u64}
pub enum GameEvent {Targets {key:GameRequestKey,result:Result<Vec<GameTargetInfo>,GameError>}}
#[derive(Clone,Debug)]
pub struct GameFrame {pub binding:Arc<GameCaptureBinding>,pub image:Arc<RgbaFrame>,pub sequence:u64,pub requested_at:MediaTime,
    pub generation:u64,pub qpc:u64,pub dropped:u64,pub throttled:u64}
#[derive(Clone,Debug)]
pub struct GameProblem {pub source:Id,pub error:GameError}
#[derive(Clone,Debug,PartialEq,Eq,serde::Serialize)]
pub struct GameStreamStatus {pub target:GameCaptureTarget,pub state:u64,pub copies_submitted:u64,pub callbacks:u64,pub dropped:u64,pub throttled:u64}
#[derive(Clone,Debug,Default)]
pub struct GameSnapshot {instance:u64,session:u64,epoch:u64,generation:u64,frames:Vec<(Id,GameFrame)>,problems:Vec<GameProblem>,streams:Vec<GameStreamStatus>}
impl GameSnapshot {
    pub fn instance(&self)->u64{self.instance}pub fn session(&self)->u64{self.session}pub fn epoch(&self)->u64{self.epoch}pub fn generation(&self)->u64{self.generation}
    pub fn frames(&self)->impl Iterator<Item=(Id,&GameFrame)>{self.frames.iter().map(|(id,frame)|(*id,frame))}
    pub fn problems(&self)->&[GameProblem]{&self.problems}
    pub fn streams(&self)->&[GameStreamStatus]{&self.streams}
}
#[derive(Clone)]
struct Desired {session:u64,revision:u64,epoch:u64,bindings:Vec<(Id,Arc<GameCaptureBinding>)>,authorized:Vec<GameCaptureTarget>,authorization_generation:u64,at:MediaTime,sent:Instant}
pub struct GameOwner {instance:u64,serial:u64,desired:Arc<Mutex<Desired>>,closed:Arc<AtomicBool>,retired:Arc<AtomicBool>,
    query:Option<mpsc::SyncSender<GameRequestKey>>,events:mpsc::Receiver<GameEvent>,snapshots:Arc<Mutex<Arc<GameSnapshot>>>,latest:Arc<GameSnapshot>,handle:Option<thread::JoinHandle<()>>}
impl GameOwner {
    pub fn available()->bool{cfg!(all(target_os="windows",target_arch="x86_64"))}
    pub fn new()->std::io::Result<Self>{
        let instance=NEXT.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).map_err(|_|std::io::Error::other("game owner identity exhausted"))?;
        let desired=Arc::new(Mutex::new(Desired {session:0,revision:0,epoch:1,bindings:Vec::new(),authorized:Vec::new(),authorization_generation:1,at:MediaTime(0),sent:Instant::now()}));
        let closed=Arc::new(AtomicBool::new(false));let retired=Arc::new(AtomicBool::new(false));
        let (query,jobs)=mpsc::sync_channel(1);let (reply,events)=mpsc::sync_channel(1);let snapshots=Arc::new(Mutex::new(Arc::new(GameSnapshot::default())));let publish=snapshots.clone();
        let (wanted,close,done)=(desired.clone(),closed.clone(),retired.clone());
        let handle=thread::Builder::new().name("skylive-game-capture".into()).spawn(move||{
            struct Done(Arc<AtomicBool>);impl Drop for Done{fn drop(&mut self){self.0.store(true,Ordering::Release);}}
            let _done=Done(done);worker(instance,wanted,close,jobs,reply,publish);
        })?;
        Ok(Self {instance,serial:0,desired,closed,retired,query:Some(query),events,snapshots,latest:Arc::new(GameSnapshot::default()),handle:Some(handle)})
    }
    pub fn targets(&mut self,session:&Session)->Result<GameRequestKey,GameError>{
        if self.closed.load(Ordering::Acquire)||session.is_closed(){return Err(GameError::Closed);}
        self.serial=self.serial.checked_add(1).ok_or(GameError::Limit("game request identity"))?;
        let key=GameRequestKey {owner:self.instance,session:session.identity(),revision:session.snapshot().revision(),serial:self.serial};
        self.query.as_ref().ok_or(GameError::Closed)?.try_send(key).map_err(|_|GameError::Busy)?;Ok(key)
    }
    /// Explicit selected-target UI Apply/Start or CLI Start only. Never call
    /// from collection load/import, discovery, ordinary synchronize or render.
    pub fn authorize(&mut self,session:&Session,target:&GameCaptureTarget)->Result<(),GameError>{
        if session.is_closed()||self.closed.load(Ordering::Acquire){return Err(GameError::Closed);}
        target.validate().map_err(|_|GameError::Invalid("invalid game authorization target"))?;
        if !session.snapshot().sources().iter().any(|source|matches!(source.kind(),SourceKind::GameCapture {binding}if binding.target().same_process_window(target))){return Err(GameError::Invalid("select an authored game source first"));}
        let mut desired=self.desired.try_lock().map_err(|error|match error{std::sync::TryLockError::WouldBlock=>GameError::Busy,std::sync::TryLockError::Poisoned(_)=>GameError::Closed})?;
        if desired.session!=session.identity(){desired.authorized.clear();desired.bindings.clear();desired.session=session.identity();}
        let model=session.snapshot();
        let retained_targets:Vec<_>=desired.bindings.iter().map(|(_,binding)|binding.target().clone()).collect();
        desired.authorized.retain(|old|retained_targets.iter().any(|target|target.same_process_window(old))||model.sources().iter().any(|source|matches!(source.kind(),SourceKind::GameCapture {binding}if binding.target().same_process_window(old))));
        if !desired.authorized.iter().any(|old|old.same_process_window(target)) {
            if desired.authorized.len()>=2{return Err(GameError::Limit("at most two explicitly authorized game targets per session"));}
            desired.authorized.push(target.clone());
        }
        desired.authorization_generation=desired.authorization_generation.checked_add(1).ok_or(GameError::Limit("game authorization identity"))?;
        desired.epoch=desired.epoch.checked_add(1).ok_or(GameError::Limit("game scope identity"))?;
        self.latest=Arc::new(GameSnapshot {instance:self.instance,session:desired.session,epoch:desired.epoch,..Default::default()});
        Ok(())
    }
    pub fn revoke(&mut self,session:&Session,source:Id)->Result<(),GameError>{
        let model=session.snapshot();let SourceKind::GameCapture {binding}=model.source(source).ok_or(GameError::TargetChanged)?.kind() else{return Err(GameError::Invalid("source is not game capture"));};
        let mut desired=self.desired.try_lock().map_err(|error|match error{std::sync::TryLockError::WouldBlock=>GameError::Busy,std::sync::TryLockError::Poisoned(_)=>GameError::Closed})?;
        desired.authorized.retain(|old|!old.same_process_window(binding.target()));desired.authorization_generation=desired.authorization_generation.checked_add(1).ok_or(GameError::Limit("game authorization identity"))?;desired.epoch=desired.epoch.checked_add(1).ok_or(GameError::Limit("game scope identity"))?;
        self.latest=Arc::new(GameSnapshot {instance:self.instance,session:session.identity(),epoch:desired.epoch,..Default::default()});Ok(())
    }
    pub fn synchronize(&mut self,session:&Session,views:&[FrameScene],at:MediaTime)->Result<(),GameError>{
        let result=self.synchronize_inner(session,views,at);
        if result.as_ref().is_err_and(|error|!matches!(error,GameError::Busy)){if let Ok(mut desired)=self.desired.try_lock(){desired.bindings.clear();desired.session=session.identity();desired.revision=session.snapshot().revision();
            if let Some(epoch)=desired.epoch.checked_add(1){desired.epoch=epoch;}else{self.closed.store(true,Ordering::Release);}
            self.latest=Arc::new(GameSnapshot {instance:self.instance,session:desired.session,epoch:desired.epoch,..Default::default()});
        }}result
    }
    fn synchronize_inner(&mut self,session:&Session,views:&[FrameScene],at:MediaTime)->Result<(),GameError>{
        if self.closed.load(Ordering::Acquire)||session.is_closed(){return Err(GameError::Closed);}
        if views.len()>crate::projector::MAX_VIDEO_VIEWS{return Err(GameError::Limit("game capture video view union40"));}
        let model=session.snapshot();let mut bindings=Vec::new();let mut seen=HashSet::new();
        for view in views {
            view.validate_session(session.identity()).map_err(|_|GameError::Stale)?;
            for source in view.visible_sources(&model).map_err(|_|GameError::Invalid("game view source closure"))? {
                if let SourceKind::GameCapture {binding}=source.kind(){if seen.insert(source.id()){
                    if bindings.len()>=MAX_REFS{return Err(GameError::Limit("game capture source references16"));}
                    bindings.push((source.id(),Arc::new(binding.clone())));
                }}
            }
        }
        bindings.sort_by_key(|(id,_)|id.get());let mut desired=self.desired.try_lock().map_err(|error|match error{std::sync::TryLockError::WouldBlock=>GameError::Busy,std::sync::TryLockError::Poisoned(_)=>GameError::Closed})?;
        if desired.session!=session.identity(){desired.authorized.clear();}
        if desired.session!=session.identity()||desired.revision!=model.revision()||desired.bindings!=bindings {
            desired.epoch=desired.epoch.checked_add(1).ok_or(GameError::Limit("game capture scope identity"))?;
            desired.session=session.identity();desired.revision=model.revision();desired.bindings=bindings;
            self.latest=Arc::new(GameSnapshot {instance:self.instance,session:desired.session,epoch:desired.epoch,..Default::default()});
        }
        desired.at=at;desired.sent=Instant::now();Ok(())
    }
    pub fn poll(&mut self,session:&Session)->Result<Option<GameEvent>,GameError>{
        let epoch=self.desired.try_lock().map_err(|error|match error{std::sync::TryLockError::WouldBlock=>GameError::Busy,std::sync::TryLockError::Poisoned(_)=>GameError::Closed})?.epoch;
        let snapshot=self.snapshots.try_lock().map_err(|error|match error{std::sync::TryLockError::WouldBlock=>GameError::Busy,std::sync::TryLockError::Poisoned(_)=>GameError::Closed})?.clone();if snapshot.session==session.identity()&&snapshot.epoch==epoch{self.latest=snapshot;}
        match self.events.try_recv(){Ok(event)=>{let GameEvent::Targets {key,..}=&event;if key.session!=session.identity()||key.revision!=session.snapshot().revision(){return Err(GameError::Stale);}Ok(Some(event))},Err(mpsc::TryRecvError::Empty)=>Ok(None),Err(_)=>{self.closed.store(true,Ordering::Release);self.latest=Arc::new(GameSnapshot::default());Err(GameError::Closed)}}
    }
    pub fn snapshot(&self)->Arc<GameSnapshot>{self.latest.clone()}
    pub fn has_active_sources(&self)->bool{!self.closed.load(Ordering::Acquire)&&self.desired.try_lock().is_ok_and(|s|!s.bindings.is_empty())}
    pub fn retired(&self)->bool{self.retired.load(Ordering::Acquire)}
    pub fn close(&mut self){self.closed.store(true,Ordering::Release);self.query.take();self.latest=Arc::new(GameSnapshot::default());if let Ok(mut published)=self.snapshots.try_lock(){*published=self.latest.clone();}}
    pub fn join(&mut self){self.close();if let Some(handle)=self.handle.take(){let _=handle.join();}}
}
impl Drop for GameOwner {fn drop(&mut self){self.close();}}
#[cfg(not(all(target_os="windows",target_arch="x86_64")))]
fn worker(instance:u64,desired:Arc<Mutex<Desired>>,closed:Arc<AtomicBool>,jobs:mpsc::Receiver<GameRequestKey>,reply:mpsc::SyncSender<GameEvent>,publish:Arc<Mutex<Arc<GameSnapshot>>>){
    let mut last=0;while !closed.load(Ordering::Acquire){
        if let Ok(key)=jobs.try_recv(){let _=reply.try_send(GameEvent::Targets {key,result:Err(GameError::Unsupported("game capture requires Windows x64 D3D11"))});}
        if let Ok(wanted)=desired.lock(){if wanted.epoch!=last{last=wanted.epoch;if let Ok(mut published)=publish.lock(){*published=Arc::new(GameSnapshot {instance,session:wanted.session,epoch:wanted.epoch,generation:last,frames:Vec::new(),streams:Vec::new(),problems:wanted.bindings.iter().map(|(source,_)|GameProblem {source:*source,error:GameError::Unsupported("game capture requires Windows x64 D3D11")}).collect()});}}}
        thread::sleep(Duration::from_millis(20));
    }
}
#[cfg(all(target_os="windows",target_arch="x86_64"))]
fn worker(instance:u64,desired:Arc<Mutex<Desired>>,closed:Arc<AtomicBool>,jobs:mpsc::Receiver<GameRequestKey>,reply:mpsc::SyncSender<GameEvent>,publish:Arc<Mutex<Arc<GameSnapshot>>>){
    native::run(instance,desired,closed,jobs,reply,publish);
}
