//! A single bounded worker polls authored folder-backed slide shows. The
//! worker owns filesystem access; the UI validates the receipt before commit.
use std::{collections::HashMap,sync::{mpsc,Arc,atomic::{AtomicBool,Ordering}},thread,time::{Duration,Instant}};
use eframe::egui;
use skylive_core::{Id,ImageSlideShow,Session,SourceKind};
use skylive_host::image_slideshow::directories::{self,Budget,WatchSignature};

struct Task{session:u64,source:Id,expected:ImageSlideShow}
struct Observed{expected:ImageSlideShow,signature:Option<WatchSignature>}
pub struct ResultItem{pub session:u64,pub source:Id,pub expected:ImageSlideShow,pub result:Result<Option<ImageSlideShow>,String>}
pub struct Watch{send:Option<mpsc::SyncSender<Task>>,receive:mpsc::Receiver<ResultItem>,busy:bool,cursor:usize,next:Instant,closed:Arc<AtomicBool>}
impl Watch{
    pub fn new(wake:egui::Context)->std::io::Result<Self>{
        let(send,requests)=mpsc::sync_channel::<Task>(1);let(reply,receive)=mpsc::sync_channel::<ResultItem>(1);
        let closed=Arc::new(AtomicBool::new(false));let worker_closed=closed.clone();
        thread::Builder::new().name("skylive-slideshow-watch".into()).spawn(move||{
            let mut entries:HashMap<Id,Observed>=HashMap::new();let mut session=0;
            while let Ok(task)=requests.recv(){
                if worker_closed.load(Ordering::Acquire){break;}
                if session!=task.session{entries.clear();session=task.session;}
                // Bound retained filesystem stamps and source snapshots across
                // arbitrarily many sources. Evicted sources are observed anew.
                if entries.len()>=8&&!entries.contains_key(&task.source){entries.clear();}
                let entry=entries.entry(task.source).or_insert_with(||Observed{expected:task.expected.clone(),signature:None});
                if entry.expected!=task.expected{*entry=Observed{expected:task.expected.clone(),signature:None};}
                let result=(||{
                    let mut budget=Budget::default();let expansion=directories::expand(task.expected.entries(),&mut budget)?;
                    let signature=expansion.watch_signature();
                    if entry.signature.as_ref()==Some(&signature){return Ok(None);}
                    let mut reads=skylive_host::assets::ReadBudget::new(128*1024*1024);let mut decoded=0;
                    let prepared=expansion.prepare(&mut budget,&mut reads,&mut decoded,skylive_host::assets::MAX_ANIMATION_BYTES)?;
                    let refreshed=prepared.into_settings(&task.expected)?;
                    if refreshed==task.expected{entry.signature=Some(signature);Ok(None)}else{entry.signature=None;Ok(Some(refreshed))}
                })();
                if result.is_err(){entry.signature=None;}
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
        if self.busy{context.request_repaint_after(Duration::from_millis(50));return;}
        let snapshot=session.snapshot();let sources=snapshot.sources();if sources.is_empty(){return;}
        let now=Instant::now();if now<self.next{context.request_repaint_after(self.next-now);return;}
        let mut chosen=None;
        for _ in 0..sources.len(){let index=self.cursor%sources.len();self.cursor=(index+1)%sources.len();if let SourceKind::ImageSlideShow{settings}=sources[index].kind(){if settings.has_directories(){chosen=Some((sources[index].id(),settings.clone()));break;}}}
        if let Some((source,expected))=chosen{if self.send.as_ref().is_some_and(|send|send.try_send(Task{session:session.identity(),source,expected}).is_ok()){self.busy=true;}self.next=now+Duration::from_secs(2);context.request_repaint_after(Duration::from_millis(50));}
        else{self.next=now+Duration::from_secs(2);context.request_repaint_after(Duration::from_secs(2));}
    }
    pub fn close(&mut self){self.closed.store(true,Ordering::Release);self.send.take();}
}
