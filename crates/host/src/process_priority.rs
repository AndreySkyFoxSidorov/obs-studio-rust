//! Explicit process scheduling publication. One native owner per process,
//! one active operation + one latest replacement; no GUI native call or join.
use skylive_core::{ProcessPriority,Session};
use serde::Serialize;
use std::sync::{Arc,Mutex,mpsc,atomic::{AtomicBool,AtomicU64,AtomicUsize,Ordering}};
#[cfg(target_os="windows")]mod native_windows;
static NEXT:AtomicU64=AtomicU64::new(1);
static OWNERS:AtomicUsize=AtomicUsize::new(0);
struct Permit;
impl Drop for Permit{fn drop(&mut self){OWNERS.fetch_sub(1,Ordering::AcqRel);}}
pub fn available()->bool{cfg!(target_os="windows")}
pub fn capabilities()->serde_json::Value{serde_json::json!({"available":available(),"scope":"current_process_only","choices":["high","above_normal","normal","below_normal","idle"],"default":"normal","automatic_default_set":false,"native_verified":false})}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize)]pub struct Key{pub owner:u64,pub session:u64,pub revision:u64,pub generation:u64}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize)]pub enum Phase{BeforeWrite,AfterWrite,ReadBack}
#[derive(Clone,Debug,Serialize)]pub struct Receipt{pub key:Key,pub requested:ProcessPriority,pub phase:Phase,pub written:bool,pub before:Option<u32>,pub readback:Option<u32>,pub error:Option<String>}
impl Receipt{pub fn verified(&self)->bool{self.phase==Phase::ReadBack&&self.error.is_none()&&self.readback.is_some()}}
#[derive(Clone,Debug,Serialize)]pub struct Restore{pub original:Option<u32>,pub owned:Option<u32>,pub observed:Option<u32>,pub restored:bool,pub skipped_external_change:bool,pub error:Option<String>}
#[derive(Clone,Debug,Default,Serialize)]pub struct Status{pub active:bool,pub closing:bool,pub closed:bool,pub last:Option<Receipt>,pub restore:Option<Restore>,pub failure:Option<String>}
#[derive(Clone,Copy,Debug)]struct Request{key:Key,priority:ProcessPriority}
struct Shared{pending:Mutex<Option<Request>>,fence:AtomicU64,close:AtomicBool,closed:AtomicBool,status:Mutex<Status>}
#[derive(Clone,Copy,Debug)]pub struct Submission{pub key:Key,pub replaced:Option<Key>}
pub struct Owner{instance:u64,generation:u64,scope:Option<(u64,u64)>,shared:Arc<Shared>,wake:mpsc::SyncSender<()>,results:mpsc::Receiver<Receipt>,latest:Status,_thread:std::thread::JoinHandle<()>}
impl Owner{
    pub fn new(wake_ui:Arc<dyn Fn()+Send+Sync>)->std::io::Result<Self>{
        OWNERS.compare_exchange(0,1,Ordering::AcqRel,Ordering::Acquire).map_err(|_|std::io::Error::other("process priority owner is active or restoring"))?;let permit=Permit;
        let instance=NEXT.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|n.checked_add(1)).map_err(|_|std::io::Error::other("process priority identity exhausted"))?;
        let shared=Arc::new(Shared{pending:Mutex::new(None),fence:AtomicU64::new(0),close:AtomicBool::new(false),closed:AtomicBool::new(false),status:Mutex::new(Status::default())});
        let(wake,receive)=mpsc::sync_channel(1);let(publish,results)=mpsc::sync_channel(2);let state=shared.clone();let final_state=shared.clone();let final_wake=wake_ui.clone();
        let thread=std::thread::Builder::new().name("skylive-process-priority".into()).spawn(move||{let _permit=permit;
            let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||run(state,receive,publish,wake_ui)));
            if let Err(payload)=result{let detail=if let Some(value)=payload.downcast_ref::<String>(){value.as_str()}else if let Some(value)=payload.downcast_ref::<&str>(){*value}else{"non-string panic payload"};eprintln!("process priority worker panicked: {detail}");put(&final_state,|s|{s.active=false;s.closing=false;s.closed=true;s.failure=Some("Process priority worker panicked; exact panic details are in the host log.".into());});final_state.closed.store(true,Ordering::Release);let _=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||final_wake()));}
        })?;
        Ok(Self{instance,generation:0,scope:None,shared,wake,results,latest:Status::default(),_thread:thread})
    }
    fn next(&mut self)->Result<u64,String>{self.generation=self.generation.checked_add(1).ok_or("process priority generation exhausted")?;Ok(self.generation)}
    pub fn observe(&mut self,session:&Session)->Result<(),String>{
        let scope=(!session.is_closed()).then(||(session.identity(),session.snapshot().revision()));
        if scope!=self.scope{let generation=self.next()?;self.shared.fence.store(generation,Ordering::Release);self.scope=scope;let _=self.wake.try_send(());}Ok(())
    }
    pub fn invalidate(&mut self)->Result<(),String>{let generation=self.next()?;self.shared.fence.store(generation,Ordering::Release);let _=self.wake.try_send(());Ok(())}
    pub fn request(&mut self,session:&Session,revision:u64,priority:ProcessPriority)->Result<Submission,String>{
        self.observe(session)?;if session.is_closed()||self.shared.close.load(Ordering::Acquire){return Err("process priority owner is closed".into());}
        if revision!=session.snapshot().revision(){return Err("process priority request belongs to an obsolete revision".into());}
        let generation=self.next()?;
        let mut pending=self.shared.pending.try_lock().map_err(|_|"process priority admission is busy")?;
        let key=Key{owner:self.instance,session:session.identity(),revision,generation};let replaced=pending.replace(Request{key,priority}).map(|r|r.key);
        self.shared.fence.store(generation,Ordering::Release);drop(pending);let _=self.wake.try_send(());Ok(Submission{key,replaced})
    }
    pub fn poll(&mut self)->Option<Receipt>{if let Ok(status)=self.shared.status.try_lock(){self.latest=status.clone();}self.results.try_recv().ok()}
    pub fn status(&mut self)->Status{if let Ok(status)=self.shared.status.try_lock(){self.latest=status.clone();}self.latest.clone()}
    pub fn accepts(&self,key:Key,session:&Session)->bool{!session.is_closed()&&key.owner==self.instance&&key.session==session.identity()&&key.revision==session.snapshot().revision()&&key.generation==self.shared.fence.load(Ordering::Acquire)&&!self.shared.close.load(Ordering::Acquire)}
    pub fn close(&mut self){self.shared.close.store(true,Ordering::Release);self.shared.fence.store(0,Ordering::Release);let _=self.wake.try_send(());}
    pub fn is_closed(&self)->bool{self.shared.closed.load(Ordering::Acquire)}
}
impl Drop for Owner{fn drop(&mut self){self.close();}}
fn put(shared:&Shared,update:impl FnOnce(&mut Status)){if let Ok(mut status)=shared.status.lock(){update(&mut status);}}
fn run(shared:Arc<Shared>,wake:mpsc::Receiver<()>,publish:mpsc::SyncSender<Receipt>,wake_ui:Arc<dyn Fn()+Send+Sync>){
    // No native API is used at construction/opening a Settings page. The
    // original is captured lazily before the first explicit/startup request.
    #[cfg(target_os="windows")]let mut native=None::<native_windows::Native>;
    while !shared.close.load(Ordering::Acquire){
        let request=match shared.pending.lock(){Ok(mut slot)=>slot.take(),Err(_)=>{shared.close.store(true,Ordering::Release);None}};
        let Some(request)=request else{if wake.recv().is_err(){break;}continue;};
        put(&shared,|s|s.active=true);
        let mut receipt=Receipt{key:request.key,requested:request.priority,phase:Phase::BeforeWrite,written:false,before:None,readback:None,error:None};
        let current=||!shared.close.load(Ordering::Acquire)&&shared.fence.load(Ordering::Acquire)==request.key.generation;
        if !current(){receipt.error=Some("Process priority request was superseded before native write.".into());}
        else{
            #[cfg(target_os="windows")]{
                if native.is_none(){match native_windows::Native::new(shared.clone()){Ok(value)=>native=Some(value),Err(error)=>receipt.error=Some(error)}}
                if let Some(native)=&mut native{native.apply(request.priority,&mut receipt,&current);}
            }
            #[cfg(not(target_os="windows"))]{receipt.error=Some("Windows process priority is unavailable on this platform.".into());}
        }
        put(&shared,|s|{s.active=false;s.last=Some(receipt.clone());});wake_ui();
        // Two bounded receipts retain real external-effect phases. While the
        // consumer is busy, native work waits here and only latest replacement
        // can be stored. Close/receiver loss never strands the restore owner.
        loop{match publish.try_send(receipt){Ok(())=>break,Err(mpsc::TrySendError::Full(value))=>{receipt=value;if shared.close.load(Ordering::Acquire){break;}std::thread::sleep(std::time::Duration::from_millis(4));},Err(mpsc::TrySendError::Disconnected(_))=>{shared.close.store(true,Ordering::Release);break;}}}
    }
    put(&shared,|s|{s.active=false;s.closing=true;});wake_ui();
    #[cfg(target_os="windows")]let restore=native.as_mut().map(native_windows::Native::restore);
    #[cfg(not(target_os="windows"))]let restore=None;
    if let Some(result)=&restore{if let Some(error)=&result.error{eprintln!("process priority restore: {error}; original={:?}; owned={:?}; observed={:?}",result.original,result.owned,result.observed);}}
    put(&shared,|s|{s.closing=false;s.closed=true;s.restore=restore;});shared.closed.store(true,Ordering::Release);wake_ui();
}
