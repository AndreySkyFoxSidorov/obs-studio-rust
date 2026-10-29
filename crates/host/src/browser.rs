//! Browser source ownership. No CEF/DLL/helper call runs on the desktop thread.
use skylive_core::{BrowserSource,Id,Session,SourceKind};
use skylive_media::{MediaTime,RgbaFrame};
use crate::{transition::FrameScene,audio_capture::{ProgramCaptureKey,NativeAudioFormat,NativePacketFlags}};
use std::{collections::{HashMap,HashSet},sync::{Arc,Mutex,mpsc,atomic::{AtomicBool,AtomicU64,Ordering}},thread,time::Instant};
pub use skylive_browser_protocol::Input as BrowserInput;
pub mod interaction;
mod reload;
pub use reload::{BrowserReloadTarget,BrowserReloadReceipt,BrowserReloadDisposition,BrowserReloadProgress,MAX_BROWSER_RELOAD_TARGETS,MAX_BROWSER_RELOAD_METADATA};
#[cfg(all(target_os="windows",target_arch="x86_64"))]mod process;
static NEXT:AtomicU64=AtomicU64::new(1);
#[derive(Clone,Debug)]pub enum BrowserError {Unavailable(&'static str),Invalid(&'static str),Runtime(String),Busy,Closed,Stale}
impl BrowserError {pub fn code(&self)->&'static str{match self {Self::Unavailable(_)=>"browser_unavailable",Self::Invalid(_)=>"browser_invalid",Self::Runtime(_)=>"browser_runtime",Self::Busy=>"browser_busy",Self::Closed=>"browser_closed",Self::Stale=>"browser_stale"}}}
impl std::fmt::Display for BrowserError {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self {Self::Unavailable(s)|Self::Invalid(s)=>write!(f,"{}: {s}",self.code()),Self::Runtime(s)=>write!(f,"{}: {s}",self.code()),_=>f.write_str(self.code())}}}impl std::error::Error for BrowserError{}
#[derive(Clone,Debug,PartialEq,Eq,Hash,serde::Serialize)]
pub struct BrowserAudioKey {pub source:Id,pub browser:Arc<BrowserSource>}
pub struct BrowserPcmPacket {pub program:ProgramCaptureKey,pub key:BrowserAudioKey,pub stream_generation:u64,pub sequence:u64,pub first_media_ns:i128,pub received_media_ns:i128,pub format:NativeAudioFormat,pub samples:Box<[f32]>,pub flags:NativePacketFlags}
#[derive(Clone)]pub struct BrowserAudioSink {pub(crate) sender:mpsc::SyncSender<Arc<BrowserPcmPacket>>}
/// Release-only lease fence; it cannot synthesize input against a new page.
#[derive(Clone,Copy,Debug)]pub struct BrowserInputLease {pub instance:u64,pub session:u64,pub native_epoch:u64,pub source:Id,pub lease:u64,pub binding_lease:u64}
#[derive(Clone,Debug)]pub struct BrowserFrame {pub browser:Arc<BrowserSource>,pub lease:u64,pub binding_lease:u64,pub sequence:u64,pub requested_at:MediaTime,pub image:Arc<RgbaFrame>}
#[derive(Clone,Debug)]pub struct BrowserProblem {pub source:Id,pub code:&'static str,pub detail:String}
#[derive(Clone,Debug,serde::Serialize)]pub struct NativePlaybackReceipt{pub session:u64,pub native_epoch:u64,pub lease:u64,pub serial:u64,pub muted:bool}
#[derive(Clone,Debug,Default)]pub struct BrowserSnapshot {instance:u64,session:u64,epoch:u64,native_epoch:u64,generation:u64,frames:HashMap<Id,BrowserFrame>,native_audio:HashMap<Id,NativePlaybackReceipt>,formats:HashMap<Id,(Arc<BrowserSource>,NativeAudioFormat)>,problems:Vec<BrowserProblem>}
impl BrowserSnapshot {pub fn native_playback(&self)->impl Iterator<Item=(Id,&NativePlaybackReceipt)>{self.native_audio.iter().map(|(source,receipt)|(*source,receipt))}pub fn instance(&self)->u64{self.instance}pub fn session(&self)->u64{self.session}pub fn epoch(&self)->u64{self.epoch}pub fn native_epoch(&self)->u64{self.native_epoch}pub fn generation(&self)->u64{self.generation}pub fn frame(&self,id:Id)->Option<&BrowserFrame>{self.frames.get(&id)}pub fn frames(&self)->impl Iterator<Item=(Id,&BrowserFrame)>{self.frames.iter().map(|(id,f)|(*id,f))}pub fn formats(&self)->impl Iterator<Item=(Id,&BrowserSource,NativeAudioFormat)>{self.formats.iter().map(|(id,(binding,format))|(*id,binding.as_ref(),*format))}pub fn problems(&self)->&[BrowserProblem]{&self.problems}}
#[derive(Clone,PartialEq,Eq)]struct Row {source:Id,browser:Arc<BrowserSource>,visible:bool,active:bool,audio_active:bool,native_audio:bool,lease:u64}
// One policy for actual helper admission and reload receipts. Program `active`
// always implies visible in synchronize; native_audio grants speaker permission
// only to an admitted row. Neither flag independently awakens a hidden page.
impl Row{fn helper_eligible(&self)->bool{self.visible||self.audio_active||!self.browser.shutdown_when_hidden()}}
#[derive(Clone)]struct Desired {session:u64,epoch:u64,rows:Vec<Row>,program:Option<ProgramCaptureKey>,at:MediaTime,sent:Instant}
struct InputRequest {session:u64,source:Id,lease:u64,event:BrowserInput}
pub struct BrowserOwner {camera_activation:Arc<crate::camera_activation::Snapshot>,instance:u64,epoch:u64,next_lease:u64,desired:Arc<Mutex<Desired>>,closed:Arc<AtomicBool>,inputs:Option<mpsc::SyncSender<InputRequest>>,receiver:Option<mpsc::Receiver<Arc<BrowserSnapshot>>>,latest:Arc<BrowserSnapshot>,handle:Option<thread::JoinHandle<()>>}
impl BrowserOwner {
 pub fn available()->bool{cfg!(all(target_os="windows",target_arch="x86_64"))}
 pub fn new(audio:BrowserAudioSink)->std::io::Result<Self>{let instance=NEXT.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|n|n.checked_add(1)).map_err(|_|std::io::Error::other("browser owner identity exhausted"))?;let desired=Arc::new(Mutex::new(Desired {session:0,epoch:1,rows:Vec::new(),program:None,at:MediaTime(0),sent:Instant::now()}));let closed=Arc::new(AtomicBool::new(false));let (inputs,requests)=mpsc::sync_channel(16);let (publish,receiver)=mpsc::sync_channel(1);let (wanted,stop)=(desired.clone(),closed.clone());let handle=thread::Builder::new().name("skylive-browser-owner".into()).spawn(move||{
  #[cfg(all(target_os="windows",target_arch="x86_64"))]process::run(instance,wanted,stop,requests,publish,audio);
  #[cfg(not(all(target_os="windows",target_arch="x86_64")))] {let _=(requests,audio);while !stop.load(Ordering::Acquire){if let Ok(d)=wanted.lock(){let snapshot=BrowserSnapshot {instance,session:d.session,epoch:d.epoch,native_epoch:0,generation:d.epoch,frames:HashMap::new(),native_audio:HashMap::new(),formats:HashMap::new(),problems:d.rows.iter().map(|r|BrowserProblem {source:r.source,code:"browser_unavailable",detail:"CEF Browser Source currently requires Windowsx64".into()}).collect()};let _=publish.try_send(Arc::new(snapshot));}thread::sleep(std::time::Duration::from_millis(30));}}
 })?;Ok(Self {camera_activation:Arc::new(Default::default()),instance,epoch:1,next_lease:0,desired,closed,inputs:Some(inputs),receiver:Some(receiver),latest:Arc::new(BrowserSnapshot::default()),handle:Some(handle)})}
 /// Video scopes include Program, Studio Preview and explicitly routed virtual
 /// camera scenes/sources. Program scopes are separate: no PCM is fabricated.
 pub fn set_camera_activation_policy(&mut self,policy:Arc<crate::camera_activation::Snapshot>){self.camera_activation=policy;}
 /// Compatibility path: audio_views is original independently selected audio
 /// membership, before derived sidechain/video-only resource expansion.
 pub fn synchronize(&mut self,session:&Session,video:&[FrameScene],program:&[FrameScene],audio_views:&[FrameScene],at:MediaTime,program_key:Option<ProgramCaptureKey>)->Result<bool,BrowserError>{
  self.synchronize_scoped_audio(session,video,program,audio_views,audio_views,at,program_key)
 }
 /// Controlled PCM may need extra detector membership. Native speakers use
 /// ONLY independently selected original audio membership supplied separately.
 /// Never pass projector, VC, Interact or sidechain-only views as native views.
 pub fn synchronize_scoped_audio(&mut self,session:&Session,video:&[FrameScene],program:&[FrameScene],audio_views:&[FrameScene],native_playback_views:&[FrameScene],at:MediaTime,program_key:Option<ProgramCaptureKey>)->Result<bool,BrowserError>{
  if self.closed.load(Ordering::Acquire)||session.is_closed(){return Err(BrowserError::Closed);}if video.len()>crate::projector::MAX_VIDEO_VIEWS||program.len()>3||audio_views.len()>3||native_playback_views.len()>3{return Err(BrowserError::Invalid("browser view union bound"));}let model=session.snapshot();let mut values:HashMap<Id,(Arc<BrowserSource>,bool,bool)>=HashMap::new();
  for source in model.sources(){if let SourceKind::Browser {browser}=source.kind(){if !browser.shutdown_when_hidden(){values.insert(source.id(),(Arc::new(browser.clone()),false,false));}}}
  for (views,is_program) in [(video,false),(program,true)] {for view in views {view.validate_session(session.identity()).map_err(|_|BrowserError::Stale)?;for source in view.visible_sources(&model).map_err(|_|BrowserError::Invalid("browser visible scene closure"))? {if let SourceKind::Browser {browser}=source.kind(){if let Some((old,visible,active))=values.get_mut(&source.id()){if old.as_ref()!=browser{return Err(BrowserError::Invalid("browser source has conflicting frozen/live bindings"));}*visible=true;*active|=is_program;}else{values.insert(source.id(),(Arc::new(browser.clone()),true,is_program));}if values.len()>crate::capture::MAX_CAPTURE_REFERENCES{return Err(BrowserError::Invalid("browser source union bound"));}}}}}
  let mut audio_sources=HashSet::new();for view in audio_views{view.validate_session(session.identity()).map_err(|_|BrowserError::Stale)?;for source in view.visible_sources(&model).map_err(|_|BrowserError::Invalid("browser audio scene closure"))?{if matches!(source.kind(),SourceKind::Browser{browser}if browser.control_audio()){audio_sources.insert(source.id());}}}
  let mut native_sources=HashMap::new();for view in native_playback_views{view.validate_session(session.identity()).map_err(|_|BrowserError::Stale)?;for source in view.visible_sources(&model).map_err(|_|BrowserError::Invalid("browser native playback scene closure"))?{if let SourceKind::Browser{browser}=source.kind(){if native_sources.insert(source.id(),Arc::new(browser.clone())).is_some_and(|old|old.as_ref()!=browser){return Err(BrowserError::Invalid("browser native playback has conflicting frozen/live bindings"));}}}}
  for (source,browser) in &native_sources{if values.get(source).is_some_and(|(value,_,_)|value!=browser){return Err(BrowserError::Invalid("browser native playback binding conflicts with video scope"));}}
  // Interaction/VC/projector video scopes never grant detector or native audio.
  for id in crate::audio::sidechain_view_keys(&model,audio_views,(self.camera_activation.session()==session.identity()).then_some(self.camera_activation.as_ref())).map_err(|_|BrowserError::Invalid("browser sidechain graph"))?{
   if let Some(source)=model.source(id){if let SourceKind::Browser{browser}=source.kind(){if browser.control_audio(){
    if let Some((old,_,_))=values.get(&id){if old.as_ref()!=browser{return Err(BrowserError::Invalid("browser sidechain binding conflicts with frozen source"));}}
    else{values.insert(id,(Arc::new(browser.clone()),false,false));}
    audio_sources.insert(id);
   }}}
  }
  if values.len()>crate::capture::MAX_CAPTURE_REFERENCES{return Err(BrowserError::Invalid("browser source union bound"));}
  let program_key=program_key.filter(|key|key.session==session.identity());
  let mut desired=self.desired.try_lock().map_err(|_|BrowserError::Busy)?;let mut rows=Vec::with_capacity(values.len());let mut changed=desired.session!=session.identity()||desired.rows.len()!=values.len()||desired.program!=program_key;
  for (source,(browser,visible,active)) in values {let previous=desired.rows.iter().find(|row|desired.session==session.identity()&&row.source==source);let refresh=previous.is_some_and(|row|!row.active&&active&&browser.refresh_when_active());let same=previous.filter(|row|row.browser==browser&&!refresh);let lease=if let Some(row)=same {row.lease}else{self.next_lease=self.next_lease.checked_add(1).ok_or(BrowserError::Invalid("browser lease identity exhausted"))?;self.next_lease};let audio_active=program_key.is_some()&&audio_sources.contains(&source)&&browser.control_audio();let native_audio=native_sources.get(&source)==Some(&browser)&&!browser.control_audio();let row=Row {source,browser,visible,active,audio_active,native_audio,lease};changed|=previous!=Some(&row);rows.push(row);}
  rows.sort_by_key(|row|row.source.get());if changed {self.epoch=self.epoch.checked_add(1).ok_or(BrowserError::Invalid("browser epoch exhausted"))?;desired.session=session.identity();desired.epoch=self.epoch;desired.rows=rows;let mut snapshot=(*self.latest).clone();snapshot.instance=self.instance;snapshot.session=session.identity();snapshot.epoch=self.epoch;snapshot.frames.retain(|source,frame|desired.rows.iter().any(|row|row.source==*source&&row.lease==frame.binding_lease&&row.browser==frame.browser));snapshot.formats.retain(|source,(browser,_)|desired.rows.iter().any(|row|row.source==*source&&&row.browser==browser));snapshot.native_audio.clear();snapshot.problems.clear();self.latest=Arc::new(snapshot);}desired.program=program_key;desired.at=at;desired.sent=Instant::now();Ok(changed)
 }
 pub fn stop(&mut self,session:&Session)->Result<(),BrowserError>{if self.closed.load(Ordering::Acquire){return Err(BrowserError::Closed);}let mut d=self.desired.try_lock().map_err(|_|BrowserError::Busy)?;if d.session==session.identity()&&d.rows.is_empty(){return Ok(());}self.epoch=self.epoch.checked_add(1).ok_or(BrowserError::Invalid("browser epoch exhausted"))?;d.session=session.identity();d.epoch=self.epoch;d.rows.clear();d.program=None;let mut latest=BrowserSnapshot::default();latest.instance=self.instance;latest.session=session.identity();latest.epoch=self.epoch;self.latest=Arc::new(latest);Ok(())}
 /// Compatibility facade: the typed receipt distinguishes desired admission
 /// from actual native navigation/first paint. Dormant sources remain dormant.
 pub fn refresh(&mut self,session:&Session,source:Id)->Result<(),BrowserError>{let target=BrowserReloadTarget::new(session,source)?;self.reload(session,&target).map(|_|())}

 /// Release remains valid after a load error removes the latest paint. It
 /// targets only the exact still-desired native binding, even when the current
 /// document has already changed; the worker never retags its old wire lease.
 pub fn release_input(&mut self,scope:BrowserInputLease,browser:&BrowserSource)->Result<(),BrowserError>{
  if self.closed.load(Ordering::Acquire){return Err(BrowserError::Closed);}
  if scope.instance!=self.instance||scope.native_epoch!=self.latest.native_epoch||scope.lease==0{return Err(BrowserError::Stale);}
  let desired=self.desired.try_lock().map_err(|_|BrowserError::Busy)?;
  if desired.session!=scope.session||!desired.rows.iter().any(|row|row.source==scope.source&&row.lease==scope.binding_lease&&row.browser.as_ref()==browser){return Err(BrowserError::Stale);}
  self.inputs.as_ref().ok_or(BrowserError::Closed)?.try_send(InputRequest {session:scope.session,source:scope.source,lease:scope.lease,event:BrowserInput::Focus(false)}).map_err(|error|match error {mpsc::TrySendError::Full(_)=>BrowserError::Busy,mpsc::TrySendError::Disconnected(_)=>BrowserError::Closed})
 }
 pub fn input(&mut self,session:&Session,source:Id,lease:u64,event:BrowserInput)->Result<(),BrowserError>{if self.closed.load(Ordering::Acquire){return Err(BrowserError::Closed);}let snapshot=self.latest.frame(source).filter(|frame|frame.lease==lease&&self.latest.session==session.identity()).ok_or(BrowserError::Stale)?;if !session.snapshot().source(source).is_some_and(|s|matches!(s.kind(),SourceKind::Browser {browser} if browser==snapshot.browser.as_ref())){return Err(BrowserError::Stale);}self.inputs.as_ref().ok_or(BrowserError::Closed)?.try_send(InputRequest {session:session.identity(),source,lease,event}).map_err(|_|BrowserError::Busy)}
 pub fn poll(&mut self,session:&Session)->Result<bool,BrowserError>{let Some(receiver)=&self.receiver else{return Err(BrowserError::Closed);};let mut changed=false;for _ in 0..4 {let Ok(snapshot)=receiver.try_recv()else{break;};if snapshot.instance==self.instance&&snapshot.session==session.identity()&&snapshot.epoch==self.epoch {self.latest=snapshot;changed=true;}}Ok(changed)}
 pub fn snapshot(&self)->Arc<BrowserSnapshot>{self.latest.clone()}pub fn has_sources(&self)->bool{self.desired.try_lock().is_ok_and(|d|!d.rows.is_empty())}
 pub fn close(&mut self){self.closed.store(true,Ordering::Release);self.inputs.take();self.receiver.take();self.latest=Arc::new(BrowserSnapshot::default());}
}
impl Drop for BrowserOwner {fn drop(&mut self){self.close();}}
