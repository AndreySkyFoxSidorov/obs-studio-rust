//! Global application hotkeys: one coalescing command slot, bounded events/status,
//! configured-key polling and press state owned exclusively by the worker.
use skylive_core::{GlobalHotkeyBindings,HotkeyAction,Session};
use std::sync::{Arc,Mutex,mpsc,atomic::{AtomicBool,AtomicU64,Ordering}};
#[cfg(target_os="windows")]mod native_windows;
mod press;
pub use press::{Callback,CallbackDecision};
mod bindings;
use bindings::Bindings;
static NEXT:AtomicU64=AtomicU64::new(1);
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub struct Scope {pub instance:u64,pub session:u64,pub generation:u64}
#[derive(Clone,Debug)]pub struct Fired {pub scope:Scope,pub action:HotkeyAction,pub press:u64,pub pressed:bool,pub observed:std::time::Instant,pub(super) binding:usize,pub(super) callback:Callback}
impl Fired {pub fn callback(&self)->Callback{self.callback.clone()}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub enum Phase {Idle,Paused,WaitingForRelease,Active,AwaitingCallback,Retiring,Failed,Unavailable,Closed}
#[derive(Clone,Debug,PartialEq,Eq)]pub struct Status {pub scope:Option<Scope>,pub phase:Phase,pub registered:usize,pub detail:Option<String>}
impl Default for Status {fn default()->Self{Self {scope:None,phase:Phase::Idle,registered:0,detail:None}}}
#[derive(Clone,Debug)]pub enum Error {Busy,Closed,IdentityExhausted,InvalidBindings(String)}
impl std::fmt::Display for Error {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str(match self{Self::Busy=>"Global shortcut owner is busy.",Self::Closed=>"Global shortcut owner is closed.",Self::IdentityExhausted=>"Global shortcut identity exhausted. Restart SkyLive.",Self::InvalidBindings(detail)=>detail})}}
impl std::error::Error for Error{}
#[derive(Clone)]struct Desired {scope:Scope,bindings:Bindings,paused:bool,live:Arc<AtomicBool>}
impl PartialEq for Desired{fn eq(&self,other:&Self)->bool{self.scope==other.scope&&self.bindings==other.bindings&&self.paused==other.paused}}
impl Eq for Desired{}
pub struct Owner {compiled:Option<(u64,u64,Bindings)>,instance:u64,generation:u64,current:Option<Desired>,pending:bool,wanted:Arc<Mutex<Option<Desired>>>,closed:Arc<AtomicBool>,events:mpsc::Receiver<Fired>,statuses:mpsc::Receiver<Status>,latest:Status,dropped:Arc<AtomicU64>}
impl Owner {
 pub fn available()->bool{cfg!(target_os="windows")}
 pub fn new()->std::io::Result<Self>{Self::with_wake(Arc::new(||{}))}
 pub fn with_wake(wake:Arc<dyn Fn()+Send+Sync>)->std::io::Result<Self>{
  Self::with_push_audio(wake,None)
 }
 pub fn with_push_audio(wake:Arc<dyn Fn()+Send+Sync>,push:Option<crate::audio_capture::PushAudioSink>)->std::io::Result<Self>{
  let instance=NEXT.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|n|n.checked_add(1)).map_err(|_|std::io::Error::other("global hotkey owner identity exhausted"))?;
  let wanted=Arc::new(Mutex::new(None));let closed=Arc::new(AtomicBool::new(false));let dropped=Arc::new(AtomicU64::new(0));
  let (send,events)=mpsc::sync_channel(16);let (publish,statuses)=mpsc::sync_channel(1);
  let (request,stop,overflow)=(wanted.clone(),closed.clone(),dropped.clone());
  std::thread::Builder::new().name("skylive-global-hotkeys".into()).spawn(move||run(instance,request,stop,send,publish,overflow,wake,push))?;
  Ok(Self {compiled:None,instance,generation:0,current:None,pending:false,wanted,closed,events,statuses,latest:Status::default(),dropped})
 }
 pub fn synchronize(&mut self,session:&Session,bindings:&GlobalHotkeyBindings,paused:bool)->Result<Scope,Error>{
  if self.closed.load(Ordering::Acquire)||session.is_closed(){return Err(Error::Closed);}
  if bindings!=session.snapshot().preferences().global_hotkeys(){return Err(Error::InvalidBindings("Global shortcut preparation no longer matches the session settings.".into()));}
  let revision=session.snapshot().revision();
  let compiled=if let Some((_,_,value))=self.compiled.as_ref().filter(|(scope,old,_)|*scope==session.identity()&&*old==revision){value.clone()}else{
   let value=Bindings::prepare(session,bindings)?;self.compiled=Some((session.identity(),revision,value.clone()));value
  };
  if !self.current.as_ref().is_some_and(|old|old.scope.session==session.identity()&&old.bindings==compiled&&old.paused==paused){
   self.generation=self.generation.checked_add(1).ok_or(Error::IdentityExhausted)?;
   if let Some(old)=&self.current{old.live.store(false,Ordering::Release);}
   self.current=Some(Desired {scope:Scope {instance:self.instance,session:session.identity(),generation:self.generation},bindings:compiled,paused,live:Arc::new(AtomicBool::new(true))});self.pending=true;
  }
  if self.pending {let mut slot=match self.wanted.try_lock(){Ok(slot)=>slot,Err(std::sync::TryLockError::WouldBlock)=>return Err(Error::Busy),Err(std::sync::TryLockError::Poisoned(_))=>{self.closed.store(true,Ordering::Release);return Err(Error::Closed);}};*slot=self.current.clone();self.pending=false;}
  Ok(self.current.as_ref().expect("current global hotkey request").scope)
 }
 pub fn poll(&mut self)->Vec<Fired>{for _ in 0..4{match self.statuses.try_recv(){Ok(status)=>self.latest=status,Err(_)=>break}}let mut fired=Vec::with_capacity(16);for _ in 0..16{match self.events.try_recv(){Ok(event)=>fired.push(event),Err(_)=>break}}fired}
 pub fn status(&self)->&Status{&self.latest}
 pub fn desired_scope(&self)->Option<Scope>{self.current.as_ref().map(|r|r.scope)}
 pub fn suspended(&self)->bool{self.current.as_ref().is_some_and(|r|r.paused&&self.latest.scope==Some(r.scope)&&self.latest.phase==Phase::Paused&&self.latest.registered==0)}
 pub fn accepts(&self,event:&Fired,session:&Session,paused:bool)->bool{
  !paused&&!session.is_closed()&&!self.closed.load(Ordering::Acquire)&&event.callback.pending()
   &&self.current.as_ref().is_some_and(|r|!r.paused&&r.scope==event.scope&&r.scope.session==session.identity()&&r.bindings.matches(session)&&r.bindings.accepts(event.binding,event.action))
 }
 /// GUI/application publication fence. Only scalar epochs, bounded binding
 /// metadata and modal/session state are read; no OS work or model retention.
 pub fn invalidate_if_stale(&mut self,session:&Session,paused:bool){
  let stale=self.current.as_ref().is_some_and(|current|paused||session.is_closed()||current.scope.session!=session.identity()||!current.bindings.matches(session));
  if stale {if let Some(current)=&self.current{current.live.store(false,Ordering::Release);}self.current=None;self.compiled=None;self.pending=true;
   if let Ok(mut slot)=self.wanted.try_lock(){*slot=None;}
  }
 }
 pub fn take_dropped(&self)->u64{self.dropped.swap(0,Ordering::AcqRel)}
 /// Revoke held-action leases when the bounded audio event transport refuses
 /// an edge. The next synchronization retires/re-registers the scope.
 pub fn revoke_input_scope(&mut self){
  if let Some(current)=&self.current{current.live.store(false,Ordering::Release);}
  self.current=None;self.compiled=None;self.pending=true;
  if let Ok(mut slot)=self.wanted.try_lock(){*slot=None;}
 }
 pub fn close(&mut self){if let Some(current)=&self.current{current.live.store(false,Ordering::Release);}self.compiled=None;self.current=None;self.closed.store(true,Ordering::Release);}
}
impl Drop for Owner {fn drop(&mut self){self.close();}}
fn run(instance:u64,wanted:Arc<Mutex<Option<Desired>>>,closed:Arc<AtomicBool>,events:mpsc::SyncSender<Fired>,publish:mpsc::SyncSender<Status>,dropped:Arc<AtomicU64>,wake:Arc<dyn Fn()+Send+Sync>,push:Option<crate::audio_capture::PushAudioSink>){
 #[cfg(target_os="windows")]let mut native=native_windows::Registry::new(instance);
 #[cfg(not(target_os="windows"))]let _=(instance,events,dropped);
 let mut current=None::<Desired>;let mut attempted=false;let mut failure=None::<String>;let mut pending=None::<Status>;let mut last=None::<Status>;
 loop {
  let stop=closed.load(Ordering::Acquire);let next=match wanted.lock(){Ok(value)=>value.clone(),Err(_)=>{closed.store(true,Ordering::Release);None}};
  if next!=current {current=next;attempted=false;failure=None;}
  let mut status=Status {scope:current.as_ref().map(|r|r.scope),..Status::default()};let mut done=false;
  #[cfg(target_os="windows")]{
   // A changed scope must first acknowledge every old ID's unregister. A
   // failed partial new set is also retired completely before Failed/none.
   let need_retire=stop||current.as_ref().is_none_or(|r|r.paused||r.bindings.is_empty())||native.scope()!=current.as_ref().map(|r|r.scope)||failure.is_some();
   if need_retire {if let Err(error)=native.retire(){status.phase=Phase::Retiring;status.detail=Some(if let Some(original)=&failure{format!("{original}; unregister: {error}")}else{error});}}
   status.registered=native.count();
   if status.phase!=Phase::Retiring {
    if stop {match native.close(){Ok(())=>{status.phase=Phase::Closed;done=true;},Err(error)=>{status.phase=Phase::Retiring;status.detail=Some(error);}}}
    else if let Some(request)=&current {
     if request.paused {status.phase=Phase::Paused;}
     else if request.bindings.is_empty(){if let Some(sink)=&push{sink.arm();}status.phase=Phase::Idle;}
     else if let Some(error)=&failure {status.phase=Phase::Failed;status.detail=Some(error.clone());}
     else if !attempted {
      if native.keys_released(&request.bindings){attempted=true;match native.register(request.scope,&request.bindings,request.live.clone()){Ok(())=>{if let Some(sink)=&push{sink.arm();}status.phase=Phase::Active;},Err(error)=>{eprintln!("global hotkeys: {error}");failure=Some(error.clone());match native.retire(){Ok(())=>{status.phase=Phase::Failed;status.detail=Some(error);},Err(cleanup)=>{status.phase=Phase::Retiring;status.detail=Some(format!("{error}; unregister: {cleanup}"));}}}}}
      else{status.phase=Phase::WaitingForRelease;}
     }else{status.phase=Phase::Active;}
    }
   }
   status.registered=native.count();
   if let Err(error)=native.messages(status.phase==Phase::Active,&events,&dropped,&closed,&*wake,push.as_ref()){
    failure=Some(error.clone());let _=native.retire();status.phase=Phase::Failed;status.registered=0;status.detail=Some(error);
   }else if status.phase==Phase::Active&&current.as_ref().is_some_and(|request|!request.live.load(Ordering::Acquire)){
    let error="Configured shortcut input was revoked after audio control delivery failed.".to_owned();failure=Some(error.clone());let _=native.retire();status.phase=Phase::Failed;status.registered=0;status.detail=Some(error);
   }else if status.phase==Phase::Active&&native.waiting(){status.phase=Phase::AwaitingCallback;}
  }
  #[cfg(not(target_os="windows"))]{
   let _=&push;
   if stop{status.phase=Phase::Closed;done=true;}else if let Some(request)=&current{status.phase=if request.paused{Phase::Paused}else if request.bindings.is_empty(){Phase::Idle}else{Phase::Unavailable};}
   let _=(&mut attempted,&mut failure);
  }
  if last.as_ref()!=Some(&status){pending=Some(status.clone());last=Some(status);}
  if let Some(status)=pending.take(){match publish.try_send(status){Ok(())=>wake(),Err(mpsc::TrySendError::Full(status))=>pending=Some(status),Err(mpsc::TrySendError::Disconnected(_))=>closed.store(true,Ordering::Release)}}
  if done&&pending.is_none(){break;}std::thread::sleep(std::time::Duration::from_millis(8));
 }
}
