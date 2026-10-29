//! Bounded metadata polling and linked-image preparation; no UI or model writes.
use std::{collections::HashMap,path::Path,sync::{mpsc,Arc,atomic::{AtomicBool,Ordering}},thread,time::{Duration,Instant,SystemTime}};
use eframe::egui;
use skylive_core::{Id,ImageAsset,Session,SourceKind};

#[derive(Clone,PartialEq,Eq)]
struct Stamp {modified:SystemTime,len:u64}
fn stamp(path:&str)->Result<Stamp,String>{
    let metadata=std::fs::metadata(Path::new(path)).map_err(|e|format!("{path}: {e}"))?;
    if !metadata.is_file(){return Err(format!("{path}: not a regular file"));}
    Ok(Stamp {modified:metadata.modified().map_err(|e|format!("{path}: {e}"))?,len:metadata.len()})
}
// Metadata can be preserved by an editor or filesystem, so it cannot prove
// that linked content is unchanged indefinitely. Re-read one watched image at
// most once per interval, on the existing bounded worker, even when the stamp
// is identical. The normal one-second polling limit still applies.
const CONTENT_RECHECK:Duration=Duration::from_secs(30);
struct Entry {expected:ImageAsset,stamp:Option<Stamp>,failed:bool,last_checked:Option<Instant>,last_verified:Option<Instant>}
struct Task {session:u64,source:Id,expected:ImageAsset}
pub struct ResultItem {pub session:u64,pub source:Id,pub expected:ImageAsset,pub result:Result<Option<(ImageAsset,bool)>,String>}
pub struct Watch {send:Option<mpsc::SyncSender<Task>>,receive:mpsc::Receiver<ResultItem>,busy:bool,cursor:usize,next:Instant,closed:Arc<AtomicBool>}
impl Watch {
    pub fn new(wake:egui::Context)->std::io::Result<Self>{
        let (send,requests)=mpsc::sync_channel::<Task>(1);let (reply,receive)=mpsc::sync_channel::<ResultItem>(1);
        let closed=Arc::new(AtomicBool::new(false));let worker_closed=closed.clone();
        thread::Builder::new().name("skylive-image-watch".into()).spawn(move||{
            let mut entries:HashMap<Id,Entry>=HashMap::new();let mut session=0;
            while let Ok(task)=requests.recv(){
                if worker_closed.load(Ordering::Acquire){break;}
                if session!=task.session{entries.clear();session=task.session;}
                if entries.len()>=skylive_core::MAX_SOURCES&&!entries.contains_key(&task.source){entries.clear();}
                let entry=entries.entry(task.source).or_insert_with(||Entry{expected:task.expected.clone(),stamp:None,failed:false,last_checked:None,last_verified:None});
                if entry.expected!=task.expected{*entry=Entry{expected:task.expected.clone(),stamp:None,failed:false,last_checked:None,last_verified:None};}
                let result=(||{
                    let now=Instant::now();
                    if entry.last_checked.is_some_and(|last|now.saturating_duration_since(last)<Duration::from_secs(1)){return Ok(None);}
                    entry.last_checked=Some(now);
                    let before=stamp(task.expected.path())?;
                    if entry.stamp.as_ref()==Some(&before)&&!entry.failed&&entry.last_verified.is_some_and(|last|now.saturating_duration_since(last)<CONTENT_RECHECK){return Ok(Some((task.expected.clone(),false)));}
                    let prepared=skylive_host::assets::prepare_image(Path::new(task.expected.path())).map_err(|e|e.to_string())?;
                    let asset=prepared.asset;drop(prepared.frame);
                    let after=stamp(task.expected.path())?;
                    if before!=after{return Err(format!("{}: file changed while image was loading",task.expected.path()));}
                    if asset.path()!=task.expected.path(){return Err("image refresh requires the original absolute path".into());}
                    let recovered=entry.failed;
                    if asset==task.expected {let first=entry.stamp.is_none();entry.stamp=Some(after);entry.last_verified=Some(now);entry.failed=false;Ok(if recovered||first{Some((asset,recovered))}else{None})}
                    else {entry.stamp=None;entry.failed=false;Ok(Some((asset,recovered)))}
                })();
                if result.is_err(){entry.failed=true;entry.stamp=None;}
                if worker_closed.load(Ordering::Acquire){break;}
                if reply.try_send(ResultItem{session:task.session,source:task.source,expected:task.expected,result}).is_err(){break;}
                wake.request_repaint();
            }
        })?;
        Ok(Self{send:Some(send),receive,busy:false,cursor:0,next:Instant::now(),closed})
    }
    pub fn poll(&mut self)->Option<ResultItem>{match self.receive.try_recv(){Ok(item)=>{self.busy=false;Some(item)},Err(mpsc::TryRecvError::Disconnected)=>{self.busy=false;self.send=None;None},Err(_)=>None}}
    pub fn schedule(&mut self,session:&Session,context:&egui::Context){
        if self.send.is_none(){return;}
        if self.busy{context.request_repaint_after(Duration::from_millis(20));return;}
        let snapshot=session.snapshot();let sources=snapshot.sources();
        if sources.is_empty(){return;}
        let now=Instant::now();if now<self.next{context.request_repaint_after(self.next-now);return;}
        let mut chosen=None;
        for _ in 0..sources.len(){let index=self.cursor%sources.len();self.cursor=(index+1)%sources.len();
            if let SourceKind::Image{asset}=sources[index].kind(){chosen=Some((sources[index].id(),asset.clone()));break;}}
        if let Some((source,expected))=chosen{
            if self.send.as_ref().is_some_and(|send|send.try_send(Task{session:session.identity(),source,expected}).is_ok()){self.busy=true;}
            self.next=now+Duration::from_millis(125);context.request_repaint_after(Duration::from_millis(20));
        }else{self.next=now+Duration::from_secs(1);context.request_repaint_after(Duration::from_secs(1));}
    }
    pub fn close(&mut self){self.closed.store(true,Ordering::Release);self.send.take();}
}
