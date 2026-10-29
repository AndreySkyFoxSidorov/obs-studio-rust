//! Bounded, revision-guarded preparation of auto-sized text snapshots.
//! This owner is not yet attached to the render graph or exposed in the UI.
use std::{collections::HashMap, sync::{Arc,Mutex,OnceLock,mpsc,atomic::{AtomicBool,AtomicU64,AtomicUsize,Ordering}},thread,time::{Duration,Instant}};
use skylive_core::{Id,TextSource};
use super::{AutoTextFrame,TextError,prepare_auto_frame};

const OUTPUT_LIMIT:usize=128*1024*1024;
const MAX_ENTRIES:usize=64;
// Reserve the high bit for a short, nonblocking UI commit barrier. Worker
// publication cannot advance a generation while its displayed geometry is
// being committed to the authored model.
const COMMIT_LOCK:u64=1<<63;

struct Credit { bytes:usize, used:Arc<AtomicUsize> }
impl Drop for Credit {fn drop(&mut self){self.used.fetch_sub(self.bytes,Ordering::AcqRel);}}
struct Published {frame:AutoTextFrame,_credit:Credit}
impl skylive_media::LeasedVideoPixels for Published {
    fn image(&self)->&skylive_media::RgbaFrame{&self.frame.raster}
}
struct Task {session:u64,source:Id,authored:TextSource,revision:u64,live:Arc<AtomicU64>}
struct Failure {session:u64,revision:u64,live:Arc<AtomicU64>,error:TextError}

/// An owned handle pins its memory credit even if a newer result replaces it.
/// Callers must recheck `current` before every publication of derived pixels.
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct Snapshot(Arc<Published>,Arc<Mutex<HashMap<Id,Failure>>>,Option<skylive_core::SessionLifetime>);
impl std::fmt::Debug for Snapshot {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{
        f.debug_struct("AutoTextSnapshot").field("session",&self.0.frame.session)
            .field("source",&self.0.frame.source).field("revision",&self.0.frame.revision).finish()
    }
}
#[allow(dead_code)]
impl Snapshot {
    pub(crate) fn source(&self)->Id{self.0.frame.source}
    pub(crate) fn authored(&self)->&TextSource{&self.0.frame.authored}
    pub(crate) fn is_pinned(&self)->bool{self.2.is_some()}
    pub(crate) fn same(&self,other:&Self)->bool{self.2.is_some()==other.2.is_some()&&Arc::ptr_eq(&self.0,&other.0)}
    pub(crate) fn is_live(&self)->bool{match &self.2 {Some(lifetime)=>lifetime.is_live_for(self.0.frame.session),None=>self.0.frame.live.load(Ordering::Acquire)==self.0.frame.revision}}
    pub(crate) fn failure(&self)->Option<String>{
        let errors=self.1.try_lock().ok()?;let failure=errors.get(&self.0.frame.source)?;
        (failure.session==self.0.frame.session&&failure.live.load(Ordering::Acquire)==failure.revision)
            .then(||failure.error.to_string())
    }
    pub(crate) fn current(&self,session:u64,source:Id,authored:&TextSource)->bool{
        self.0.frame.session==session && self.0.frame.source==source && &self.0.frame.authored==authored && self.is_live()
    }
    pub(crate) fn pin(&self,lifetime:&skylive_core::SessionLifetime)->Option<Self>{
        lifetime.is_live_for(self.0.frame.session).then(||Self(self.0.clone(),self.1.clone(),Some(lifetime.clone())))
    }
    pub(crate) fn try_commit_lock(&self)->Option<CommitLock>{
        if self.2.is_some()||self.0.frame.revision==0||self.0.frame.revision>=COMMIT_LOCK{return None;}
        let live=&self.0.frame.live;let revision=self.0.frame.revision;
        live.compare_exchange(revision,revision|COMMIT_LOCK,Ordering::AcqRel,Ordering::Acquire).ok()?;
        Some(CommitLock{live:live.clone(),revision})
    }
    pub(crate) fn matches_model(&self,session:u64,model:&skylive_core::StudioSnapshot)->bool{
        model.source(self.source()).is_some_and(|source|matches!(source.kind(),skylive_core::SourceKind::Text{settings} if self.current(session,source.id(),settings)))
    }
    pub(crate) fn dimensions(&self)->(u32,u32){self.0.frame.dimensions()}
    pub(crate) fn raster(&self)->&skylive_media::RgbaFrame{&self.0.frame.raster}
    pub(crate) fn shared_frame(&self)->skylive_media::SharedVideoFrame{
        skylive_media::SharedVideoFrame::leased(self.0.clone())
    }
    /// Pair the guarded frame with a normalized, non-persistent projection.
    /// The returned projection pins the same output credit as the raster.
    /// A consumer must call `current` immediately before using either result.
    pub(crate) fn project(&self,session:u64,model:&skylive_core::StudioSnapshot)->Result<Projection,skylive_core::Error>{
        let frame=&self.0.frame;
        if !self.current(session,frame.source,&frame.authored){return Err(skylive_core::Error::Invalid("Automatic text geometry is stale"));}
        let model=model.snapshot_with_auto_text_geometry(frame.source,&frame.authored,frame.dimensions())?;
        if !self.current(session,frame.source,&frame.authored){return Err(skylive_core::Error::Invalid("Automatic text geometry changed during projection"));}
        Ok(Projection{model,snapshot:self.clone()})
    }
}
/// One coherent runtime projection for all participating sources. The
/// scene's group hierarchy is normalized once, and every generation is
/// checked again after that work before any caller can publish the result.
pub(crate) fn project_all(session:u64,model:&skylive_core::StudioSnapshot,snapshots:&[Snapshot])->Result<skylive_core::StudioSnapshot,skylive_core::Error>{
    let mut frames=Vec::with_capacity(snapshots.len());
    for snapshot in snapshots {
        if !snapshot.matches_model(session,model){return Err(skylive_core::Error::Invalid("Automatic text geometry is stale"));}
        frames.push((snapshot.source(),snapshot.authored(),snapshot.dimensions()));
    }
    let result=model.snapshot_with_auto_text_geometries(&frames)?;
    if snapshots.iter().any(|snapshot|!snapshot.is_live()){return Err(skylive_core::Error::Invalid("Automatic text geometry changed during projection"));}
    Ok(result)
}
pub(crate) struct CommitLock{live:Arc<AtomicU64>,revision:u64}
impl Drop for CommitLock {fn drop(&mut self){let _=self.live.compare_exchange(self.revision|COMMIT_LOCK,self.revision,Ordering::AcqRel,Ordering::Acquire);}}
fn revoke(live:&AtomicU64)->bool{
    let mut value=live.load(Ordering::Acquire);
    loop {
        if value&COMMIT_LOCK!=0{return false;}
        if value==0{return true;}
        match live.compare_exchange_weak(value,0,Ordering::AcqRel,Ordering::Acquire){Ok(_)=>return true,Err(next)=>value=next}
    }
}

#[allow(dead_code)]
pub(crate) struct Projection { pub(crate) model:skylive_core::StudioSnapshot, pub(crate) snapshot:Snapshot }

#[allow(dead_code)]
pub(crate) struct Owner {
    sender:Option<mpsc::SyncSender<Task>>,
    closed:Arc<AtomicBool>,
    frames:Arc<Mutex<HashMap<Id,Arc<Published>>>>,
    errors:Arc<Mutex<HashMap<Id,Failure>>>,
}

fn reserve(used:&Arc<AtomicUsize>,bytes:usize)->Option<Credit>{
    let mut old=used.load(Ordering::Acquire);
    loop {
        let new=old.checked_add(bytes)?;
        if new>OUTPUT_LIMIT{return None;}
        match used.compare_exchange_weak(old,new,Ordering::AcqRel,Ordering::Acquire){
            Ok(_)=>return Some(Credit{bytes,used:used.clone()}),Err(next)=>old=next,
        }
    }
}

#[allow(dead_code)]
impl Owner {
    pub(crate) fn new()->std::io::Result<Self>{
        let (sender,receiver)=mpsc::sync_channel::<Task>(1);
        let frames=Arc::new(Mutex::new(HashMap::<Id,Arc<Published>>::new()));
        let errors=Arc::new(Mutex::new(HashMap::<Id,Failure>::new()));
        let closed=Arc::new(AtomicBool::new(false));
        // Separate session workers still share a single process pixel budget;
        // snapshots retained by old output owners keep their credits.
        static USED:OnceLock<Arc<AtomicUsize>>=OnceLock::new();
        let used=USED.get_or_init(||Arc::new(AtomicUsize::new(0))).clone();
        let worker_frames=frames.clone();let worker_errors=errors.clone();let worker_closed=closed.clone();
        thread::Builder::new().name("skylive-auto-text".into()).spawn(move||{
            while let Ok(task)=receiver.recv(){
                if worker_closed.load(Ordering::Acquire){break;}
                let mut fonts=super::crate_font_budget_for_auto();
                let mut text_reads=16*1024*1024;
                let result=prepare_auto_frame(task.session,task.source,&task.authored,task.revision,task.live.clone(),
                    &mut fonts,&mut text_reads,OUTPUT_LIMIT);
                let mut frame=match result {Ok(frame)=>frame,Err(error)=>{
                    if task.live.load(Ordering::Acquire)==task.revision {
                        if let Ok(mut errors)=worker_errors.lock(){
                            errors.retain(|_,item|item.live.load(Ordering::Acquire)==item.revision);
                            if errors.contains_key(&task.source)||errors.len()<MAX_ENTRIES{
                                errors.insert(task.source,Failure{session:task.session,revision:task.revision,live:task.live,error});
                            }
                        }
                    }
                    continue;
                }};
                if let Ok(mut errors)=worker_errors.lock(){errors.remove(&task.source);}
                if worker_closed.load(Ordering::Acquire)||!frame.current(task.session,task.source,&task.authored){continue;}
                // Credit includes the cloned authored text and conservative metadata;
                // readers holding old snapshots keep their credit until dropped.
                let Some(bytes)=frame.raster.byte_len().checked_add(frame.authored.text().len()).and_then(|n|n.checked_add(8192)) else{continue;};
                let Some(credit)=reserve(&used,bytes) else{continue;};
                let Ok(mut map)=worker_frames.lock() else{break;};
                if worker_closed.load(Ordering::Acquire)||!frame.current(task.session,task.source,&task.authored){continue;}
                if let Some(previous)=map.get(&task.source).filter(|entry|entry.frame.current(task.session,task.source,&task.authored)){
                    if previous.frame.dimensions()==frame.dimensions()&&previous.frame.raster.pixels()==frame.raster.pixels(){continue;}
                    // Keep the previous result usable while the new file is
                    // prepared. Revoke it only at publication, and reject work
                    // that raced a source/session invalidation.
                    let Some(next)=frame.revision.checked_add(1).filter(|next|*next<COMMIT_LOCK) else{continue;};
                    if frame.live.compare_exchange(frame.revision,next,Ordering::AcqRel,Ordering::Acquire).is_err(){continue;}
                    frame.revision=next;
                }
                // Retire stale entries before admitting another source. A current
                // result is never silently evicted to admit an unrelated one.
                map.retain(|_,entry|entry.frame.live.load(Ordering::Acquire)==entry.frame.revision);
                if !map.contains_key(&task.source)&&map.len()>=MAX_ENTRIES{continue;}
                map.insert(task.source,Arc::new(Published{frame,_credit:credit}));
            }
        })?;
        Ok(Self{sender:Some(sender),closed,frames,errors})
    }
    /// The revision owner invalidates `live` on file, font, session or source
    /// changes before scheduling a replacement. Queue overflow is explicit.
    pub(crate) fn request(&self,session:u64,source:Id,authored:TextSource,revision:u64,live:Arc<AtomicU64>)->Result<(),&'static str>{
        if revision==0||revision&COMMIT_LOCK!=0||live.load(Ordering::Acquire)!=revision{return Err("stale auto-text request");}
        let sender=self.sender.as_ref().ok_or("auto-text worker closed")?;
        sender.try_send(Task{session,source,authored,revision,live}).map_err(|error|match error{
            mpsc::TrySendError::Full(_)=>"auto-text worker busy",
            mpsc::TrySendError::Disconnected(_)=>"auto-text worker closed",
        })
    }
    pub(crate) fn snapshot(&self,session:u64,source:Id,authored:&TextSource)->Option<Snapshot>{
        let map=self.frames.try_lock().ok()?;
        let frame=map.get(&source)?;
        frame.frame.current(session,source,authored).then(||Snapshot(frame.clone(),self.errors.clone(),None))
    }
    fn failure(&self,session:u64,source:Id)->Option<String>{
        let errors=self.errors.try_lock().ok()?;let failure=errors.get(&source)?;
        (failure.session==session&&failure.live.load(Ordering::Acquire)==failure.revision)
            .then(||failure.error.to_string())
    }
}
impl Drop for Owner {fn drop(&mut self){self.closed.store(true,Ordering::Release);self.sender.take();}}

struct Tracked {authored:TextSource,live:Arc<AtomicU64>,next:Instant}
/// Session-scoped activation and bounded polling. This lifecycle has to be
/// shared by every output owner; constructing one per renderer is incorrect.
#[allow(dead_code)]
pub(crate) struct Lifecycle {owner:Owner,session:u64,tracked:HashMap<Id,Tracked>,cursor:usize}
#[allow(dead_code)]
impl Lifecycle {
    pub(crate) fn new()->std::io::Result<Self>{Ok(Self{owner:Owner::new()?,session:0,tracked:HashMap::new(),cursor:0})}
    /// Reconcile authored source definitions without filesystem I/O. At most
    /// one request is admitted per call; worker queue capacity is one.
    pub(crate) fn sync(&mut self,session:u64,model:&skylive_core::StudioSnapshot){
        if self.session!=session{
            if self.tracked.values().any(|entry|entry.live.load(Ordering::Acquire)&COMMIT_LOCK!=0){return;}
            for entry in self.tracked.values(){let _=revoke(&entry.live);}self.tracked.clear();self.session=session;self.cursor=0;
        }
        self.tracked.retain(|id,entry|{
            let keep=model.source(*id).is_some_and(|source|matches!(source.kind(),skylive_core::SourceKind::Text{settings} if settings.auto_extents() && settings==&entry.authored));
            if !keep&&!revoke(&entry.live){return true;}keep
        });
        let now=Instant::now();
        for source in model.sources(){
            let skylive_core::SourceKind::Text{settings}=source.kind() else{continue;};
            if !settings.auto_extents()||self.tracked.contains_key(&source.id())||self.tracked.len()>=MAX_ENTRIES{continue;}
            self.tracked.insert(source.id(),Tracked{authored:settings.clone(),live:Arc::new(AtomicU64::new(1)),next:now});
        }
        let sources=model.sources();if sources.is_empty(){return;}
        for _ in 0..sources.len(){
            let index=self.cursor%sources.len();self.cursor=(index+1)%sources.len();
            let id=sources[index].id();let Some(entry)=self.tracked.get_mut(&id) else{continue;};
            if now<entry.next{continue;}
            let ready=self.owner.snapshot(session,id,&entry.authored).is_some();
            if ready&&entry.authored.file().is_none(){entry.next=now+Duration::from_secs(60);continue;}
            let revision=entry.live.load(Ordering::Acquire);
            if revision==0||revision&COMMIT_LOCK!=0{continue;}
            if self.owner.request(session,id,entry.authored.clone(),revision,entry.live.clone()).is_ok(){
                entry.next=now+if entry.authored.file().is_some(){Duration::from_secs(1)}else{Duration::from_millis(250)};
                break;
            }
        }
    }
    pub(crate) fn snapshot(&self,source:Id,settings:&TextSource)->Option<Snapshot>{self.owner.snapshot(self.session,source,settings)}
    fn failure(&self,source:Id)->Option<String>{self.owner.failure(self.session,source)}
}
impl Drop for Lifecycle {fn drop(&mut self){for entry in self.tracked.values(){let _=revoke(&entry.live);}}}

struct SessionEntry {lifecycle:Lifecycle,last_seen:Instant}
struct Registry {sessions:HashMap<u64,SessionEntry>}
pub(crate) enum AcquireError {Pending,Unavailable(String)}
/// One shared registry is sampled by all render/interaction owners. A busy
/// registry or an unprepared source fails closed instead of blocking a frame.
/// Dormant sessions are retired, revoking their guards; live snapshots retain
/// their credits until their final consumer drops them.
#[allow(dead_code)]
pub(crate) fn shared_snapshots(session:u64,model:&skylive_core::StudioSnapshot)->Result<Vec<Snapshot>,AcquireError>{
    static REGISTRY:OnceLock<Mutex<Registry>>=OnceLock::new();
    let mut registry=REGISTRY.get_or_init(||Mutex::new(Registry{sessions:HashMap::new()})).try_lock().map_err(|_|AcquireError::Pending)?;
    let now=Instant::now();
    registry.sessions.retain(|id,entry|*id==session||now.saturating_duration_since(entry.last_seen)<Duration::from_secs(60)
        ||entry.lifecycle.tracked.values().any(|tracked|tracked.live.load(Ordering::Acquire)&COMMIT_LOCK!=0));
    if !registry.sessions.contains_key(&session){
        if registry.sessions.len()>=4{return Err(AcquireError::Unavailable("automatic text session limit".into()));}
        let lifecycle=Lifecycle::new().map_err(|error|AcquireError::Unavailable(format!("automatic text worker unavailable: {error}")))?;
        registry.sessions.insert(session,SessionEntry{lifecycle,last_seen:now});
    }
    let entry=registry.sessions.get_mut(&session).ok_or_else(||AcquireError::Unavailable("automatic text session unavailable".into()))?;
    entry.last_seen=now;entry.lifecycle.sync(session,model);
    let mut result=Vec::new();
    for source in model.sources(){
        let skylive_core::SourceKind::Text{settings}=source.kind() else{continue;};
        if !settings.auto_extents(){continue;}
        if result.len()>=MAX_ENTRIES{return Err(AcquireError::Unavailable("automatic text source limit".into()));}
        let frame=entry.lifecycle.snapshot(source.id(),settings).ok_or_else(||entry.lifecycle.failure(source.id()).map_or(AcquireError::Pending,AcquireError::Unavailable))?;
        result.push(frame);
    }
    Ok(result)
}
