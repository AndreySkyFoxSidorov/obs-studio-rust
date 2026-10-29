//! Per-session Camera AV policy. This owner contains metadata, never native objects.
use skylive_core::{Id, Session, SourceKind, VideoCaptureBinding, MAX_SOURCES};
use skylive_media::MediaTime;
use std::sync::{Arc, Mutex};
use crate::{camera_capture::CameraOwner, audio_capture::AudioCaptureWorker, transition::FrameComposition};
const MAX_KEY_BYTES:usize=8*1024*1024;
#[derive(Clone,Debug)]
pub struct SourceEpoch {
    pub source:Id,
    pub epoch:u64,
    pub enabled:bool,
    pub binding:Arc<VideoCaptureBinding>,
    pub at:MediaTime,
}
#[derive(Clone,Copy,Debug,serde::Serialize)]
pub struct CameraAudioCut {pub frame:u64,pub sample_rate:u32}
#[derive(Clone,Copy,Debug,PartialEq,Eq,serde::Serialize)]
pub struct RequestKey {pub session:u64,pub revision:u64,pub serial:u64}
#[derive(Clone,Copy,Debug,PartialEq,Eq,serde::Serialize)]
#[serde(rename_all="snake_case")]
pub enum Phase {Idle,Queued,Working,Retiring,Complete,Failed}
impl Phase {fn terminal(self)->bool {matches!(self,Self::Complete|Self::Failed)}}
#[derive(Clone,Debug,serde::Serialize)]
pub struct Problem {pub code:&'static str,pub detail:String}
#[derive(Clone,Debug,serde::Serialize)]
pub struct LegStatus {pub phase:Phase,pub problem:Option<Problem>,pub cut:Option<CameraAudioCut>}
impl LegStatus {fn queued()->Self {Self {phase:Phase::Queued,problem:None,cut:None}}}
#[derive(Clone)]
pub(crate) struct LegWatch {pub key:RequestKey,state:Arc<Mutex<LegStatus>>,cancel:Arc<Mutex<Option<Cancel>>>,end_session:Arc<std::sync::atomic::AtomicBool>}
#[derive(Clone)]
pub(crate) struct Cancel {pub end_session:bool,pub changes:Arc<[SourceEpoch]>,pub policy:Arc<Snapshot>}
impl LegWatch {
    fn new(key:RequestKey)->Self {Self {key,state:Arc::new(Mutex::new(LegStatus::queued())),cancel:Arc::new(Mutex::new(None)),end_session:Arc::new(std::sync::atomic::AtomicBool::new(false))}}
    pub(crate) fn publish(&self,phase:Phase,problem:Option<Problem>,cut:Option<CameraAudioCut>) {
        // Only the background leg writes; GUI readers use try_lock exclusively.
        if let Ok(mut state)=self.state.lock(){*state=LegStatus {phase,problem,cut};}
    }
    fn cancel(&self,value:Cancel){if value.end_session{self.end_session.store(true,std::sync::atomic::Ordering::Release);}if let Ok(mut slot)=self.cancel.try_lock(){*slot=Some(value);}}
    pub(crate) fn cancellation(&self)->Option<Cancel>{
        let ending=self.end_session.load(std::sync::atomic::Ordering::Acquire);
        let value=self.cancel.try_lock().ok().and_then(|slot|slot.clone());
        value.map(|mut value|{value.end_session|=ending;value}).or_else(||ending.then(||Cancel {end_session:true,changes:Arc::from([]),policy:Arc::new(Snapshot::default())}))
    }
    fn read(&self)->Option<LegStatus>{self.state.try_lock().ok().map(|s|s.clone())}
}
#[derive(Clone,Debug)]
pub struct Snapshot {session:u64,serial:u64,control_pending:bool,sources:Arc<[SourceEpoch]>,withheld:Arc<[Id]>}
impl Default for Snapshot {fn default()->Self {Self {session:0,serial:0,control_pending:false,sources:Arc::from([]),withheld:Arc::from([])}}}
impl Snapshot {
    pub fn session(&self)->u64 {self.session}
    pub fn serial(&self)->u64 {self.serial}
    pub fn control_pending(&self)->bool{self.control_pending}
    pub fn sources(&self)->&[SourceEpoch] {&self.sources}
    pub fn enabled(&self,source:Id)->bool {self.sources.iter().find(|s|s.source==source).is_none_or(|s|s.enabled)}
    pub fn publish_video(&self,source:Id)->bool {self.enabled(source)&&!self.withheld.contains(&source)}
    pub fn epoch(&self,source:Id)->u64 {self.sources.iter().find(|s|s.source==source).map_or(0,|s|s.epoch)}
    pub(crate) fn validate(&self,session:&Session)->Result<(),Error>{
        if self.session!=session.identity(){return Err(Error::Invalid("camera activation belongs to another session"));}
        validate_entries(session,&self.sources)
    }
}
#[derive(Clone,Debug,serde::Serialize)]
pub struct Status {
    pub audio_buffer_blocks:Option<u32>,pub audio_max_buffer_blocks:Option<u32>,pub monitor_padding_ns:Option<u64>,
    pub problem:Option<Problem>,
    pub queued_stop:Option<RequestKey>,
    pub key:Option<RequestKey>,pub pending:bool,pub desired_enabled:bool,
    pub sources:Vec<Id>,pub video:LegStatus,pub audio:LegStatus,
    pub fully_acknowledged:bool,
}
impl Default for Status {fn default()->Self {Self {audio_buffer_blocks:None,audio_max_buffer_blocks:None,monitor_padding_ns:None,problem:None,queued_stop:None,key:None,pending:false,desired_enabled:true,sources:vec![],video:LegStatus {phase:Phase::Idle,problem:None,cut:None},audio:LegStatus {phase:Phase::Idle,problem:None,cut:None},fully_acknowledged:false}}}
#[derive(Debug)]
pub enum Error {Busy,Invalid(&'static str),Camera(crate::camera_capture::CameraError),Audio(crate::audio_capture::AudioCaptureError)}
impl Error {pub fn code(&self)->&'static str {match self {Self::Busy=>"camera_av_busy",Self::Invalid(_)=>"camera_av_invalid",Self::Camera(e)=>e.code(),Self::Audio(e)=>e.code()}}}
impl std::fmt::Display for Error {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {match self {Self::Busy=>f.write_str("A camera AV activation is still pending"),Self::Invalid(s)=>f.write_str(s),Self::Camera(e)=>e.fmt(f),Self::Audio(e)=>e.fmt(f)}}}
impl std::error::Error for Error {}
struct Pending {key:RequestKey,changes:Arc<[SourceEpoch]>,video:LegWatch,audio:LegWatch,video_sent:bool,audio_sent:bool}
pub struct Owner {snapshot:Arc<Snapshot>,revision:u64,serial:u64,pending:Option<Pending>,queued_stop:Option<Pending>,completed:Option<Pending>,status:Status}
impl Default for Owner {fn default()->Self {Self {snapshot:Arc::new(Snapshot::default()),revision:u64::MAX,serial:0,pending:None,queued_stop:None,completed:None,status:Status::default()}}}
impl Owner {
    pub fn snapshot(&self)->Arc<Snapshot>{self.snapshot.clone()}
    pub fn status(&self)->Status {self.status.clone()}
    pub fn enabled(&self,session:&Session,source:Id)->bool {self.snapshot.session!=session.identity()||self.snapshot.enabled(source)}
    pub fn pending(&self)->bool {self.pending.is_some()}
    pub fn synchronize(&mut self,session:&Session,camera:&mut CameraOwner,audio:&mut AudioCaptureWorker)->Result<(),Error>{
        if session.is_closed(){return Err(Error::Invalid("camera activation session is closed"));}
        let model=session.snapshot();
        if self.snapshot.session!=session.identity(){
            // Old-session asynchronous legs must retire even if they are waiting
            // for a first frame/PCM forever. Never apply their IDs to a new session.
            for old in [&self.pending,&self.queued_stop,&self.completed].into_iter().flatten(){
                let cancel=Cancel {end_session:true,changes:old.changes.clone(),policy:self.snapshot.clone()};
                old.video.cancel(cancel.clone());old.audio.cancel(cancel);
            }
            self.pending=None;self.completed=None;self.queued_stop=None;self.status=Status::default();self.revision=u64::MAX;
            self.bump()?;self.snapshot=Arc::new(Snapshot {session:session.identity(),serial:self.serial,..Snapshot::default()});
        }
        if self.revision!=model.revision(){
            let mut entries=Vec::new();
            let previous=self.snapshot.sources.clone();
            for old in previous.iter(){
                if let Some(source)=model.source(old.source) {
                    if let SourceKind::VideoCapture {binding}=source.kind(){
                        let mut entry=old.clone();
                        if entry.binding.as_ref()!=binding {
                            // A source presentation edit changes the video frame
                            // identity, not an explicit AV Start/Stop or PCM scope.
                            if !entry.binding.same_av_activation(binding) {self.bump()?;entry.epoch=self.serial;}
                            entry.binding=Arc::new(binding.clone());
                        }
                        entries.push(entry);
                    }
                }else{entries.push(old.clone());}
            }
            validate_entries(session,&entries)?;
            let withheld=self.snapshot.withheld.iter().copied().filter(|id|entries.iter().any(|s|s.source==*id)).collect::<Vec<_>>();
            self.snapshot=Arc::new(Snapshot {serial:self.serial,sources:entries.into(),withheld:withheld.into(),..self.snapshot.as_ref().clone()});self.revision=model.revision();
        }
        let audio_result=audio.set_camera_activation_policy(self.snapshot.clone());
        let camera_result=camera.set_activation_policy(self.snapshot.clone());
        audio_result.map_err(Error::Audio)?;
        match camera_result {Ok(())|Err(crate::camera_capture::CameraError::Busy)=>Ok(()),Err(error)=>Err(Error::Camera(error))}
    }
    pub fn request(&mut self,session:&Session,source:Id,enabled:bool,at:MediaTime)->Result<RequestKey,Error>{self.request_many(session,Some(source),enabled,at)}
    pub fn stop_all(&mut self,session:&Session,at:MediaTime)->Result<RequestKey,Error>{self.request_many(session,None,false,at)}
    fn request_many(&mut self,session:&Session,source:Option<Id>,enabled:bool,at:MediaTime)->Result<RequestKey,Error>{
        if let Some(pending)=&self.pending {
            if enabled||self.queued_stop.is_some()||source.is_some_and(|source|pending.changes.iter().any(|entry|entry.source!=source)){return Err(Error::Busy);}
        }
        if session.is_closed(){return Err(Error::Invalid("camera activation session is closed"));}
        let model=session.snapshot();let mut changes=Vec::new();
        self.bump()?;let key=RequestKey {session:session.identity(),revision:model.revision(),serial:self.serial};
        for item in model.sources(){if source.is_none_or(|id|id==item.id()) {if let SourceKind::VideoCapture {binding}=item.kind(){changes.push(SourceEpoch {source:item.id(),epoch:key.serial,enabled,binding:Arc::new(binding.clone()),at});}}}
        if changes.is_empty(){return Err(Error::Invalid("camera activation source is unavailable"));}
        validate_entries(session,&changes)?;
        let mut entries=if self.snapshot.session==session.identity(){self.snapshot.sources.iter().filter(|e|model.source(e.source).is_none_or(|s|matches!(s.kind(),SourceKind::VideoCapture{..}))).cloned().collect::<Vec<_>>()}else{Vec::new()};
        for change in &changes {if let Some(old)=entries.iter_mut().find(|old|old.source==change.source){*old=change.clone();}else{entries.push(change.clone());}}
        validate_entries(session,&entries)?;
        let mut withheld=self.snapshot.withheld.iter().copied().filter(|id|entries.iter().any(|s|s.source==*id)&&!changes.iter().any(|s|s.source==*id)).collect::<Vec<_>>();
        withheld.extend(changes.iter().map(|s|s.source));
        self.snapshot=Arc::new(Snapshot {session:session.identity(),serial:self.serial,control_pending:true,sources:entries.into(),withheld:withheld.into()});
        let video=LegWatch::new(key);let audio=LegWatch::new(key);
        let next=Pending {key,changes:changes.into(),video,audio,video_sent:false,audio_sent:false};
        if let Some(active)=&self.pending {
            let cancel=Cancel {end_session:false,changes:next.changes.clone(),policy:self.snapshot.clone()};
            active.video.cancel(cancel.clone());active.audio.cancel(cancel);
            self.queued_stop=Some(next);self.status.queued_stop=Some(key);self.status.desired_enabled=false;
        }else{
            self.status=Status {audio_buffer_blocks:None,audio_max_buffer_blocks:None,monitor_padding_ns:None,problem:None,queued_stop:None,key:Some(key),pending:true,desired_enabled:enabled,sources:next.changes.iter().map(|s|s.source).collect(),video:LegStatus::queued(),audio:LegStatus::queued(),fully_acknowledged:false};
            self.pending=Some(next);
        }
        Ok(key)
    }
    pub fn drive(&mut self,session:&Session,camera:&mut CameraOwner,audio:&mut AudioCaptureWorker,output:Option<&FrameComposition>,now:MediaTime)->Result<Status,Error>{
        if let Err(error)=self.synchronize(session,camera,audio){
            if let Some(pending)=&mut self.pending {
                let problem=Problem {code:error.code(),detail:error.to_string()};
                match error {Error::Camera(_)=>{pending.video.publish(Phase::Failed,Some(problem),None);pending.video_sent=true;},Error::Audio(_)=>{pending.audio.publish(Phase::Failed,Some(problem),None);pending.audio_sent=true;},_=>return Err(error)}
            }else{return Err(error);}
        }
        if let Some(stats)=audio.program_status_for(session){
            self.status.audio_buffer_blocks=Some(stats.buffer_blocks);self.status.audio_max_buffer_blocks=Some(stats.max_buffer_blocks);self.status.monitor_padding_ns=Some(stats.monitor.device_padding_ns);
        }else{self.status.audio_buffer_blocks=None;self.status.audio_max_buffer_blocks=None;self.status.monitor_padding_ns=None;}
        if let Some(output)=output {
            let model=session.snapshot();let mut alive=std::collections::HashSet::new();
            for view in output.scenes(){for source in view.visible_sources(&model).map_err(|_|Error::Invalid("camera activation Program closure unavailable"))?{alive.insert(source.id());}}
            let entries=self.snapshot.sources.iter().filter(|entry|model.source(entry.source).is_some()||alive.contains(&entry.source)||camera.source_referenced(entry.source)||self.pending.as_ref().is_some_and(|pending|pending.changes.iter().any(|s|s.source==entry.source))).cloned().collect::<Vec<_>>();
            if entries.len()!=self.snapshot.sources.len(){self.bump()?;let withheld=self.snapshot.withheld.iter().copied().filter(|id|entries.iter().any(|s|s.source==*id)).collect::<Vec<_>>();self.snapshot=Arc::new(Snapshot {serial:self.serial,sources:entries.into(),withheld:withheld.into(),..self.snapshot.as_ref().clone()});}
        }
        let Some(pending)=&mut self.pending else{return Ok(self.status());};
        let current=session.snapshot();
        let valid=pending.key.session==session.identity()&&pending.changes.iter().all(|entry|current.source(entry.source).is_some_and(|source|matches!(source.kind(),SourceKind::VideoCapture {binding} if binding.same_av_activation(&entry.binding)))&&now.0>=entry.at.0);
        if !valid {
            let problem=Problem {code:"camera_av_obsolete",detail:"The camera source changed while AV activation was pending".into()};
            self.status.problem=Some(problem.clone());
            if !pending.video_sent {pending.video.publish(Phase::Failed,Some(problem.clone()),None);pending.video_sent=true;}
            if !pending.audio_sent {pending.audio.publish(Phase::Failed,Some(problem),None);pending.audio_sent=true;}
        }
        if valid&&!pending.video_sent&&!camera.is_busy(){match camera.request_av_activation(session,pending.changes.clone(),now,pending.video.clone()){
            Ok(())=>pending.video_sent=true,Err(crate::camera_capture::CameraError::Busy)=>{},Err(error)=>{pending.video.publish(Phase::Failed,Some(Problem {code:error.code(),detail:error.to_string()}),None);pending.video_sent=true;}
        }}
        if valid&&!pending.audio_sent&&!audio.is_busy(){match audio.request_camera_activation(session,pending.changes.clone(),output,now,pending.audio.clone()){
            Ok(())=>pending.audio_sent=true,Err(crate::audio_capture::AudioCaptureError::Busy)=>{},Err(error)=>{pending.audio.publish(Phase::Failed,Some(Problem {code:error.code(),detail:error.to_string()}),None);pending.audio_sent=true;}
        }}
        if let Some(stop)=&mut self.queued_stop {
            let cancel=Cancel {end_session:false,changes:stop.changes.clone(),policy:self.snapshot.clone()};
            pending.video.cancel(cancel.clone());pending.audio.cancel(cancel);
            // A completed video leg can stop immediately while the old audio
            // prefix retires. Never restart that completed leg to retry audio.
            if !stop.video_sent&&!camera.is_busy(){match camera.request_av_activation(session,stop.changes.clone(),now,stop.video.clone()){
                Ok(())=>stop.video_sent=true,Err(crate::camera_capture::CameraError::Busy)=>{},Err(error)=>{stop.video.publish(Phase::Failed,Some(Problem {code:error.code(),detail:error.to_string()}),None);stop.video_sent=true;}
            }}
        }
        if let Some(value)=pending.video.read(){self.status.video=value;}
        if let Some(value)=pending.audio.read(){self.status.audio=value;}
        let done=self.status.video.phase.terminal()&&self.status.audio.phase.terminal();
        if valid&&self.status.video.phase==Phase::Complete&&self.status.desired_enabled&&pending.changes.iter().any(|e|self.snapshot.withheld.contains(&e.source)){
            self.serial=self.serial.checked_add(1).ok_or(Error::Invalid("camera activation identity exhausted"))?;
            let withheld=self.snapshot.withheld.iter().copied().filter(|id|!pending.changes.iter().any(|s|s.source==*id)).collect::<Vec<_>>();
            self.snapshot=Arc::new(Snapshot {serial:self.serial,withheld:withheld.into(),..self.snapshot.as_ref().clone()});
            camera.set_activation_policy(self.snapshot.clone()).map_err(Error::Camera)?;
            audio.set_camera_activation_policy(self.snapshot.clone()).map_err(Error::Audio)?;
        }
        self.status.pending=!done;self.status.fully_acknowledged=done&&valid&&self.queued_stop.is_none()&&self.status.video.phase==Phase::Complete&&self.status.audio.phase==Phase::Complete;
        if done {
            self.completed=self.pending.take();
            if let Some(next)=self.queued_stop.take(){
                self.status=Status {audio_buffer_blocks:None,audio_max_buffer_blocks:None,monitor_padding_ns:None,problem:None,queued_stop:None,key:Some(next.key),pending:true,desired_enabled:false,sources:next.changes.iter().map(|s|s.source).collect(),video:next.video.read().unwrap_or_else(LegStatus::queued),audio:next.audio.read().unwrap_or_else(LegStatus::queued),fully_acknowledged:false};
                self.pending=Some(next);
            }else{
                self.bump()?;self.snapshot=Arc::new(Snapshot {serial:self.serial,control_pending:false,..self.snapshot.as_ref().clone()});
            }
        }
        Ok(self.status())
    }
    fn bump(&mut self)->Result<(),Error>{self.serial=self.serial.checked_add(1).ok_or(Error::Invalid("camera activation identity exhausted"))?;Ok(())}
}
fn validate_entries(session:&Session,entries:&[SourceEpoch])->Result<(),Error>{
    if entries.len()>3*MAX_SOURCES{return Err(Error::Invalid("camera activation source count exceeds bound"));}
    let model=session.snapshot();let mut bytes=entries.len()*std::mem::size_of::<SourceEpoch>();
    let mut ids=std::collections::HashSet::new();
    for entry in entries {
        if entry.epoch==0||!ids.insert(entry.source)||model.source(entry.source).is_some_and(|source|!matches!(source.kind(),SourceKind::VideoCapture {binding} if binding.same_av_activation(&entry.binding))){return Err(Error::Invalid("camera activation source identity changed"));}
        bytes=bytes.checked_add(match entry.binding.target(){skylive_core::VideoCaptureTarget::Windows {symbolic_link}=>symbolic_link.len(),skylive_core::VideoCaptureTarget::Macos {unique_id}=>unique_id.len()}).and_then(|v|v.checked_add(entry.binding.audio().map_or(0,|a|match a.target(){skylive_core::AudioTarget::WindowsEndpoint {id}=>id.len(),_=>0}))).filter(|n|*n<=MAX_KEY_BYTES).ok_or(Error::Invalid("camera activation metadata byte bound"))?;
    }
    Ok(())
}
