//! Installed SDK receive adapter. One owned worker holds DLL, finder, receivers,
//! SDK buffers and final published payload references. Constructing is inert;
//! only explicit discovery/preparation or an enabled authored source uses NDI.
use skylive_core::{Id,NdiConnection,NdiSender,NdiSource,Session,NDI_PLUGIN,SourceKind};
use skylive_media::{MediaTime,RgbaFrame};
use crate::{audio_capture::{NativeAudioFormat,ProgramCaptureKey},transition::FrameScene};
use std::{collections::HashMap,sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,Ordering},mpsc},thread,time::{Duration,Instant}};
#[cfg(target_os="windows")] mod abi;
#[cfg(target_os="windows")] mod runtime_windows;
#[cfg(target_os="windows")] mod receiver;
#[cfg(target_os="windows")] mod worker;
#[cfg(target_os="windows")] mod async_delay;
#[cfg(target_os="windows")] mod staged_delay;
pub mod output;
const MAX_RECEIVERS:usize=4;
static NEXT:AtomicU64=AtomicU64::new(1);
#[derive(Clone,Debug)]
pub enum NdiError {Unavailable(&'static str),Runtime(String),Invalid(&'static str),Limit(&'static str),Busy,Closed,Stale}
impl NdiError {pub fn code(&self)->&'static str {match self {Self::Unavailable(_)=>"ndi_unavailable",Self::Runtime(_)=>"ndi_runtime",Self::Invalid(_)=>"ndi_invalid",Self::Limit(_)=>"ndi_limit",Self::Busy=>"ndi_busy",Self::Closed=>"ndi_closed",Self::Stale=>"ndi_stale"}}}
impl std::fmt::Display for NdiError {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {match self {Self::Unavailable(s)|Self::Invalid(s)|Self::Limit(s)=>write!(f,"{}: {s}",self.code()),Self::Runtime(s)=>write!(f,"{}: {s}",self.code()),_=>f.write_str(self.code())}}}
impl std::error::Error for NdiError {}
#[derive(Clone,Copy,Debug,PartialEq,Eq,serde::Serialize)]
pub struct NdiRequestKey {pub instance:u64,pub session:u64,pub revision:u64,pub serial:u64}
#[derive(Clone,Debug,serde::Serialize)]
pub struct VideoMetadata {pub width:u32,pub height:u32,pub fps_num:u32,pub fps_den:u32,pub timecode_100ns:i64,pub timestamp_100ns:Option<i64>}
/// Which original sender clock supplied a media PTS. These domains are
/// never interchangeable, even if their numeric timestamps coincide.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum NdiVideoClock { SdkTimestamp, SenderTimecode }
#[derive(Clone,Debug)]
pub struct NdiFrame {pub stream_generation:u64,pub ndi:Arc<NdiSource>,pub sequence:u64,pub requested_at:MediaTime,pub metadata:VideoMetadata,pub image:skylive_media::SharedVideoFrame,
    /// Raw sender time, not packet arrival or the local timeline estimate.
    pub native_timestamp_ns:Option<i128>,pub native_clock:Option<NdiVideoClock>,pub native_clock_epoch:u64,
    /// The existing receive-clock mapping is anchored to packet arrival and
    /// is an estimate, not evidence of capture time or network latency.
    pub mapped_media_ns:i128,pub mapped_time_estimated:bool,
    /// Local interval bracketing the receive call, not sender capture time.
    pub(crate) receive_qpc_ns:Option<(u64,u64)>,pub(crate) async_delay:Option<crate::capture::AsyncDelayReceipt>,pub(crate) draft_raw:Option<Arc<NdiFrame>>}
#[derive(Clone,Debug)]
pub struct NdiProblem {pub source:Id,pub code:&'static str,pub detail:String}
#[derive(Clone,Debug,Default)]
pub struct NdiSnapshot {pub runtime_version:Option<String>,instance:u64,session:u64,epoch:u64,generation:u64,frames:HashMap<Id,NdiFrame>,draft_frames:HashMap<Id,(NdiFrame,Arc<AtomicU64>)>,formats:HashMap<Id,NativeAudioFormat>,problems:Vec<NdiProblem>}
impl NdiSnapshot {
    pub fn instance(&self)->u64{self.instance} pub fn session(&self)->u64{self.session} pub fn epoch(&self)->u64{self.epoch} pub fn generation(&self)->u64{self.generation}
    pub fn frames(&self)->impl Iterator<Item=(Id,&NdiFrame)>{self.frames.iter().map(|(id,f)|(*id,f))}
    pub(crate) fn draft_frames(&self)->impl Iterator<Item=(Id,&NdiFrame,&Arc<AtomicU64>)>{self.draft_frames.iter().map(|(id,(frame,live))|(*id,frame,live))}
    pub fn frame(&self,id:Id)->Option<&NdiFrame>{self.frames.get(&id)}
    pub fn formats(&self)->impl Iterator<Item=(Id,NativeAudioFormat)>+'_ {self.formats.iter().map(|(id,f)|(*id,*f))}
    pub fn problems(&self)->&[NdiProblem]{&self.problems} pub fn source_count(&self)->usize{self.frames.len()}
}
pub enum NdiEvent {Discovered {key:NdiRequestKey,result:Result<Vec<NdiSender>,NdiError>},Prepared {key:NdiRequestKey,result:Result<NdiSource,NdiError>}}
pub struct NdiPcmPacket {pub program:ProgramCaptureKey,pub source:Id,pub ndi:Arc<NdiSource>,pub stream_generation:u64,pub sequence:u64,pub first_media_ns:i128,pub received_media_ns:i128,pub format:NativeAudioFormat,pub samples:Box<[f32]>,pub timestamp_estimated:bool,pub sender_timecode_100ns:i64,pub sender_timestamp_100ns:Option<i64>,
    /// Original sender timestamp in the established shared clock domain.
    /// Absent clock readings are never replaced by local packet arrival time.
    pub native_timestamp_ns:Option<i128>,pub native_clock:Option<NdiVideoClock>,
    /// Audio-track discontinuity generation; it is not a video epoch and
    /// equality of these counters alone does not establish synchronization.
    pub native_clock_epoch:u64,
    pub(crate) owner:u64,pub(crate) scope_epoch:u64,pub(crate) live_scope:Arc<AtomicU64>,pub(crate) live_stream:Arc<AtomicU64>,pub(crate) live_audio_clock:Arc<AtomicU64>}
impl NdiPcmPacket {
    /// Packets can outlive the worker's enqueue call; retire them when the
    /// owner changes its source/filter scope or shuts down.
    pub(crate) fn scope_current(&self)->bool {
        self.live_scope.load(Ordering::Acquire)==self.scope_epoch
            && self.live_stream.load(Ordering::Acquire)==self.stream_generation
            && self.live_audio_clock.load(Ordering::Acquire)==self.native_clock_epoch
    }
    pub(crate) fn same_history(&self,other:&Self)->bool {
        self.source==other.source && self.owner==other.owner && self.scope_epoch==other.scope_epoch
            && self.stream_generation==other.stream_generation && self.native_clock_epoch==other.native_clock_epoch
            && Arc::ptr_eq(&self.live_scope,&other.live_scope) && Arc::ptr_eq(&self.live_stream,&other.live_stream)
            && Arc::ptr_eq(&self.live_audio_clock,&other.live_audio_clock)
    }
}
#[derive(Clone)]
pub struct NdiAudioSink {pub(crate) sender:mpsc::SyncSender<Arc<NdiPcmPacket>>}
#[derive(Clone,PartialEq)]
struct Scope {session:u64,epoch:u64,sources:Vec<(Id,Arc<NdiSource>)>,delays:Vec<(Id,Vec<(skylive_core::VideoFilterId,u16)>)>,
    // Frozen enabled chain preceding the final delay. A future staged worker
    // must never reuse pixels or proof after any of these settings change.
    delay_upstream:Vec<(Id,Vec<skylive_core::VideoFilter>)>}
#[derive(Clone)]
struct NdiMaskPublication {binding_epoch:u64,snapshot:crate::playback::CameraMaskSnapshot,live:Arc<AtomicBool>,scroll_live:Arc<AtomicBool>}
#[derive(Clone)]
struct Desired {scope:Scope,audio_sources:Vec<Id>,program:Option<ProgramCaptureKey>,at:MediaTime,sent:Instant,mask:Option<NdiMaskPublication>}
enum Query {Discover {key:NdiRequestKey,local:bool},Prepare {key:NdiRequestKey,connection:NdiConnection}}
pub struct NdiOwner {camera_activation:Arc<crate::camera_activation::Snapshot>,instance:u64,serial:u64,epoch:u64,delay_epoch:Arc<AtomicU64>,desired:Arc<Mutex<Desired>>,mask_owned:Option<NdiMaskPublication>,closed:Arc<AtomicBool>,queries:Option<mpsc::SyncSender<Query>>,events:mpsc::Receiver<NdiEvent>,snapshots:Option<mpsc::Receiver<Arc<NdiSnapshot>>>,latest:Arc<NdiSnapshot>,pending:Option<NdiRequestKey>,handle:Option<thread::JoinHandle<()>>}
impl NdiOwner {
    pub fn available()->bool{cfg!(all(target_os="windows",target_arch="x86_64"))}
    pub fn new(audio:NdiAudioSink)->std::io::Result<Self>{
        let instance=NEXT.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|v|v.checked_add(1)).map_err(|_|std::io::Error::other("NDI owner identity exhausted"))?;
        let desired=Arc::new(Mutex::new(Desired {scope:Scope {session:0,epoch:1,sources:Vec::new(),delays:Vec::new(),delay_upstream:Vec::new()},audio_sources:Vec::new(),program:None,at:MediaTime(0),sent:Instant::now(),mask:None}));let closed=Arc::new(AtomicBool::new(false));
        let (queries,jobs)=mpsc::sync_channel(1);let (reply,events)=mpsc::sync_channel(1);let (publication,snapshots)=mpsc::sync_channel(1);
        let delay_epoch=Arc::new(AtomicU64::new(1));
        let (input,stop,worker_delay_epoch)=(desired.clone(),closed.clone(),delay_epoch.clone());
        let handle=thread::Builder::new().name("skylive-ndi-receive".into()).spawn(move||{
            #[cfg(target_os="windows")] worker::run(instance,input,stop,jobs,reply,publication,audio,worker_delay_epoch);
            #[cfg(not(target_os="windows"))] {
                let _=(audio,worker_delay_epoch);let mut last=None;let mut dirty=false;let mut latest=Arc::new(NdiSnapshot::default());
                while !stop.load(Ordering::Acquire){
                    if let Ok(wanted)=input.lock(){let key=(wanted.scope.session,wanted.scope.epoch);if last!=Some(key){last=Some(key);dirty=true;latest=Arc::new(NdiSnapshot {instance,session:key.0,epoch:key.1,generation:key.1,problems:wanted.scope.sources.iter().map(|(source,_)|NdiProblem {source:*source,code:"ndi_unavailable",detail:"NDI receive currently requires Windows x64".into()}).collect(),..Default::default()});}}
                    if dirty {match publication.try_send(latest.clone()){Ok(())=>dirty=false,Err(mpsc::TrySendError::Disconnected(_))=>break,Err(mpsc::TrySendError::Full(_))=>{}}}
                    match jobs.recv_timeout(Duration::from_millis(25)){Ok(Query::Discover{key,..})=>{let _=reply.try_send(NdiEvent::Discovered {key,result:Err(NdiError::Unavailable("NDI receive currently requires Windows x64"))});},Ok(Query::Prepare{key,..})=>{let _=reply.try_send(NdiEvent::Prepared {key,result:Err(NdiError::Unavailable("NDI receive currently requires Windows x64"))});},Err(mpsc::RecvTimeoutError::Disconnected)=>break,Err(_)=>{}}
                }
            }
        })?;
        Ok(Self {camera_activation:Arc::new(Default::default()),instance,serial:0,epoch:1,delay_epoch,desired,mask_owned:None,closed,queries:Some(queries),events,snapshots:Some(snapshots),latest:Arc::new(NdiSnapshot::default()),pending:None,handle:Some(handle)})
    }
    fn admit(&mut self,session:&Session)->Result<NdiRequestKey,NdiError>{
        if self.closed.load(Ordering::Acquire)||session.is_closed(){return Err(NdiError::Closed);}if self.pending.is_some(){return Err(NdiError::Busy);}
        if !session.snapshot().preferences().plugin_enabled(NDI_PLUGIN){return Err(NdiError::Unavailable("Enable the DistroAV NDI source module first"));}
        self.serial=self.serial.checked_add(1).ok_or(NdiError::Limit("NDI request identity"))?;Ok(NdiRequestKey {instance:self.instance,session:session.identity(),revision:session.snapshot().revision(),serial:self.serial})
    }
    pub fn discover(&mut self,session:&Session,include_local:bool)->Result<NdiRequestKey,NdiError>{let key=self.admit(session)?;self.queries.as_ref().ok_or(NdiError::Closed)?.try_send(Query::Discover {key,local:include_local}).map_err(|_|NdiError::Busy)?;self.pending=Some(key);Ok(key)}
    pub fn prepare(&mut self,session:&Session,connection:NdiConnection)->Result<NdiRequestKey,NdiError>{connection.validate().map_err(|e|NdiError::Runtime(e.to_string()))?;let key=self.admit(session)?;self.queries.as_ref().ok_or(NdiError::Closed)?.try_send(Query::Prepare {key,connection}).map_err(|_|NdiError::Busy)?;self.pending=Some(key);Ok(key)}
    pub fn synchronize(&mut self,session:&Session,views:&[FrameScene],at:MediaTime,program:Option<ProgramCaptureKey>)->Result<(),NdiError>{
        self.synchronize_video(session,views,views,at,program)
    }
    /// Receive the video union while granting PCM only to the pre-existing
    /// Program/Preview audio union. A VC-only receiver never feeds Program PCM.
    pub fn set_camera_activation_policy(&mut self,policy:Arc<crate::camera_activation::Snapshot>){self.camera_activation=policy;}
    /// Publish the actual playback and file-version snapshot; it carries no
    /// claim that sender PTS is a local capture timestamp.
    pub fn set_mask_snapshot(&mut self,session:&Session,snapshot:crate::playback::CameraMaskSnapshot)->Result<(),NdiError> {
        if session.is_closed()||self.closed.load(Ordering::Acquire){return Err(NdiError::Closed);}
        if snapshot.session!=session.identity()||snapshot.revision!=session.snapshot().revision(){return Err(NdiError::Stale);}
        let unchanged=self.mask_owned.as_ref().is_some_and(|old|old.binding_epoch==self.epoch
            && old.live.load(Ordering::Acquire) && old.snapshot.session==snapshot.session
            && old.snapshot.revision==snapshot.revision && old.snapshot.playback_epoch==snapshot.playback_epoch
            && old.snapshot.clock.scope()==snapshot.clock.scope() && old.snapshot.clock.revision()==snapshot.clock.revision()
            && old.snapshot.clock.files_domain==snapshot.clock.files_domain && old.snapshot.clock.same_files(&snapshot.clock)
            && Arc::ptr_eq(&old.snapshot.versions,&snapshot.versions));
        let live=if unchanged {self.mask_owned.as_ref().ok_or(NdiError::Stale)?.live.clone()} else {
            if let Some(old)=&self.mask_owned {old.live.store(false,Ordering::Release);}
            Arc::new(AtomicBool::new(true))
        };
        let same_scroll=self.mask_owned.as_ref().is_some_and(|old|unchanged
            &&old.scroll_live.load(Ordering::Acquire)&&old.snapshot.same_scroll_scope(&snapshot));
        let scroll_live=if same_scroll {self.mask_owned.as_ref().ok_or(NdiError::Stale)?.scroll_live.clone()} else {
            if let Some(old)=&self.mask_owned {old.scroll_live.store(false,Ordering::Release);}
            Arc::new(AtomicBool::new(true))
        };
        let publication=NdiMaskPublication {binding_epoch:self.epoch,snapshot,live,scroll_live};
        self.mask_owned=Some(publication.clone());
        let mut desired=self.desired.try_lock().map_err(|_|NdiError::Busy)?;
        if desired.scope.session!=session.identity()||desired.scope.epoch!=self.epoch{return Err(NdiError::Stale);}
        desired.mask=Some(publication);Ok(())
    }
    pub fn synchronize_video(&mut self,session:&Session,views:&[FrameScene],audio_views:&[FrameScene],at:MediaTime,program:Option<ProgramCaptureKey>)->Result<(),NdiError>{
        if self.closed.load(Ordering::Acquire)||session.is_closed(){return Err(NdiError::Closed);}
        if views.len()>crate::projector::MAX_VIDEO_VIEWS||audio_views.len()>3{return Err(NdiError::Limit("NDI video/audio view union supports at most40/three views"));}
        let model=session.snapshot();let mut sources:Vec<(Id,Arc<NdiSource>)>=Vec::new();
        if model.preferences().plugin_enabled(NDI_PLUGIN){for view in views {view.validate_session(session.identity()).map_err(|_|NdiError::Stale)?;for source in view.visible_sources(&model).map_err(|_|NdiError::Invalid("NDI scene is unavailable"))?{if let SourceKind::NdiSource {ndi}=source.kind(){
            if let Some((_,existing))=sources.iter().find(|(id,_)|*id==source.id()){if existing.as_ref()!=ndi{return Err(NdiError::Invalid("NDI source has conflicting frame-view bindings"));}}
            else {sources.push((source.id(),Arc::new(ndi.clone())));if sources.len()>MAX_RECEIVERS{self.stop();return Err(NdiError::Limit("NDI supports at most four active receivers"));}}
        }}}}
        sources.sort_by_key(|(id,_)|id.get());
        let mut audio_sources=Vec::new();
        for view in audio_views {
            view.validate_session(session.identity()).map_err(|_|NdiError::Stale)?;
            for source in view.visible_sources(&model).map_err(|_|NdiError::Invalid("NDI audio scene is unavailable"))? {
                if matches!(source.kind(),SourceKind::NdiSource {..})&&!audio_sources.contains(&source.id()){audio_sources.push(source.id());if audio_sources.len()>MAX_RECEIVERS{return Err(NdiError::Limit("NDI audio source union"));}}
            }
        }
        if model.preferences().plugin_enabled(NDI_PLUGIN){
            for id in crate::audio::sidechain_view_keys(&model,audio_views,(self.camera_activation.session()==session.identity()).then_some(self.camera_activation.as_ref())).map_err(|_|NdiError::Invalid("NDI sidechain graph"))?{
                if let Some(source)=model.source(id){if let SourceKind::NdiSource{ndi}=source.kind(){
                    if let Some((_,old))=sources.iter().find(|(old,_)|*old==id){if old.as_ref()!=ndi{return Err(NdiError::Invalid("NDI sidechain binding conflicts with frozen source"));}}
                    else{if sources.len()==MAX_RECEIVERS{return Err(NdiError::Limit("NDI sidechain exceeds four receivers"));}sources.push((id,Arc::new(ndi.clone())));}
                    if !audio_sources.contains(&id){audio_sources.push(id);}
                }}
            }
        }
        sources.sort_by_key(|(id,_)|id.get());
        if audio_sources.len()>MAX_RECEIVERS{return Err(NdiError::Limit("NDI audio source union"));}
        let mut delays=Vec::new();
        let mut delay_upstream=Vec::new();
        for (id,_) in &sources {
            let Some(source)=model.source(*id) else {continue;};
            let mut filters=Vec::new();
            for filter in source.video_filters() {
                if filter.enabled && model.preferences().plugin_enabled(filter.plugin_id()) {
                    if let skylive_core::VideoEffect::AsyncVideoDelay {delay_ms:1..=u16::MAX}=filter.effect {
                        let skylive_core::VideoEffect::AsyncVideoDelay {delay_ms}=filter.effect else {unreachable!()};
                        filters.push((filter.id().ok_or(NdiError::Invalid("NDI async delay filter identity"))?,delay_ms));
                    }
                }
            }
            if !filters.is_empty(){
                let active:Vec<_>=source.video_filters().iter().filter(|filter|filter.enabled && model.preferences().plugin_enabled(filter.plugin_id())).collect();
                if let Some(last)=active.iter().rposition(|filter|matches!(filter.effect,skylive_core::VideoEffect::AsyncVideoDelay {delay_ms:1..=u16::MAX})) {
                    delay_upstream.push((*id,active[..last].iter().map(|filter|(**filter).clone()).collect()));
                }
                delays.push((*id,filters));
            }
        }
        let mut desired=self.desired.lock().map_err(|_|NdiError::Closed)?;
        if desired.scope.session!=session.identity()||desired.scope.sources!=sources||desired.scope.delays!=delays||desired.scope.delay_upstream!=delay_upstream {
            if let Some(old)=self.mask_owned.take(){old.live.store(false,Ordering::Release);old.scroll_live.store(false,Ordering::Release);}
            desired.mask=None;
            self.epoch=self.epoch.checked_add(1).ok_or(NdiError::Limit("NDI scope identity"))?;
            self.delay_epoch.store(self.epoch,Ordering::Release);
            desired.scope=Scope {session:session.identity(),epoch:self.epoch,sources,delays,delay_upstream};
            self.latest=Arc::new(NdiSnapshot {instance:self.instance,session:session.identity(),epoch:self.epoch,..Default::default()});
        }
        desired.audio_sources=audio_sources;
        desired.program=program.filter(|p|p.session==session.identity());desired.at=at;desired.sent=Instant::now();Ok(())
    }
    pub fn poll(&mut self,session:&Session)->Result<Option<NdiEvent>,NdiError>{
        if let Some(snapshots)=&self.snapshots {while let Ok(snapshot)=snapshots.try_recv(){if snapshot.session==session.identity()&&snapshot.epoch==self.epoch{self.latest=snapshot;}}}
        match self.events.try_recv(){Ok(event)=>{let key=match &event{NdiEvent::Discovered{key,..}|NdiEvent::Prepared{key,..}=>*key};if self.pending==Some(key){self.pending=None;}if key.session!=session.identity()||key.revision!=session.snapshot().revision()||!session.snapshot().preferences().plugin_enabled(NDI_PLUGIN){return Err(NdiError::Stale);}Ok(Some(event))},Err(mpsc::TryRecvError::Empty)=>Ok(None),Err(_)=>Err(NdiError::Closed)}
    }
    pub fn snapshot(&self)->Arc<NdiSnapshot>{self.latest.clone()}
    pub(crate) fn delay_epoch_token(&self)->Arc<AtomicU64>{self.delay_epoch.clone()}
    pub fn is_busy(&self)->bool{self.pending.is_some()}
    pub fn has_active_sources(&self)->bool{self.desired.lock().is_ok_and(|d|!d.scope.sources.is_empty())}
    pub fn stop(&mut self){self.delay_epoch.store(0,Ordering::Release);if let Some(old)=self.mask_owned.take(){old.live.store(false,Ordering::Release);old.scroll_live.store(false,Ordering::Release);}if let Ok(mut state)=self.desired.lock(){self.epoch=self.epoch.saturating_add(1);state.scope=Scope {session:state.scope.session,epoch:self.epoch,sources:Vec::new(),delays:Vec::new(),delay_upstream:Vec::new()};state.mask=None;state.program=None;state.audio_sources.clear();self.latest=Arc::new(NdiSnapshot::default());}}
    pub fn close(&mut self){self.delay_epoch.store(0,Ordering::Release);if let Some(old)=self.mask_owned.take(){old.live.store(false,Ordering::Release);old.scroll_live.store(false,Ordering::Release);}self.closed.store(true,Ordering::Release);self.queries.take();self.snapshots.take();self.latest=Arc::new(NdiSnapshot::default());}
    /// For a background/CLI shutdown coordinator only, after releasing all
    /// external capture snapshots. SDK calls are not hard timed.
    pub fn join(&mut self){self.close();if let Some(handle)=self.handle.take(){let _=handle.join();}}
}
impl Drop for NdiOwner {fn drop(&mut self){self.close();}}
