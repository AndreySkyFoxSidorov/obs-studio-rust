//! Bounded clip variants and actual render-worker residency. No GUI owner.
use super::*;
use skylive_core::{ImageAsset,Source};
use std::sync::{OnceLock, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}, mpsc::{self, SyncSender}};
const MAX_KEYS:usize=3*skylive_core::MAX_SOURCES;
const MAX_METADATA:usize=8*1024*1024;

#[derive(Default)]
pub(super) struct Entry {
    pub(super) slots:[Option<ResourceResult>;2],
    pub(super) visible:u8,
}
impl Entry {
    fn bytes(&self)->usize{self.slots.iter().flatten().filter_map(|value|value.as_ref().ok()).map(|clip|clip.byte_len()).sum()}
    fn error_bytes(&self)->usize{self.slots.iter().flatten().filter_map(|value|value.as_ref().err()).map(|(_,_,detail)|detail.len().saturating_mul(2)).sum()}
    fn empty(&self)->bool{self.slots.iter().all(Option::is_none)}
}
#[derive(Default)]struct Pin {resident:u8,visible:u8}
#[derive(Default)]pub(super) struct Pins {rows:HashMap<ImageAsset,Pin>,paths:usize}
impl Pins {
    pub(super) fn insert(&mut self,asset:&ImageAsset,linear:bool,visible:bool)->Result<(),MediaError>{
        let bit=1u8<<usize::from(linear);
        if let Some(pin)=self.rows.get_mut(asset){pin.resident|=bit;if visible{pin.visible|=bit;}return Ok(());}
        let count=self.rows.len().checked_add(1).filter(|count|*count<=MAX_KEYS).ok_or(MediaError::BudgetExceeded)?;
        let paths=self.paths.checked_add(asset.path().len()).ok_or(MediaError::Overflow)?;
        let buckets=self.rows.capacity().max(count).checked_add(1).and_then(|n|n.checked_mul(4))
            .and_then(|n|n.checked_mul(std::mem::size_of::<(ImageAsset,Pin)>()+32)).ok_or(MediaError::Overflow)?;
        if paths.checked_add(buckets).is_none_or(|n|n>MAX_METADATA){return Err(MediaError::BudgetExceeded);}
        self.rows.try_reserve(1).map_err(|_|MediaError::BudgetExceeded)?;
        self.rows.insert(asset.clone(),Pin{resident:bit,visible:if visible{bit}else{0}});self.paths=paths;Ok(())
    }
    pub(super) fn persistent(&mut self,model:&StudioSnapshot,_cache:&ClipCache)->Result<(),MediaError>{
        for source in model.sources(){if let SourceKind::Image{asset}=source.kind(){let settings=source.image_settings();
            if !settings.unload_when_hidden {
                self.insert(asset,settings.linear_alpha,false)?;
            }
        }}Ok(())
    }
    pub(super) fn image(&mut self,source:&Source)->Result<(),MediaError>{
        if let SourceKind::Image{asset}=source.kind(){self.insert(asset,source.image_settings().linear_alpha,true)?;}Ok(())
    }
}

// One process-wide background decode and one bounded result. The worker
// expires unclaimed results so an inactive renderer cannot starve others.
static PRELOAD_BUSY: AtomicBool = AtomicBool::new(false);
struct Ticket {pending:Arc<AtomicBool>}
impl Drop for Ticket {fn drop(&mut self){self.pending.store(false,Ordering::Release);PRELOAD_BUSY.store(false,Ordering::Release);}}
struct Job {asset:ImageAsset,linear:bool,epoch:u64,live:Arc<AtomicU64>,ticket:Ticket}
struct Completed {asset:ImageAsset,linear:bool,epoch:u64,live:Arc<AtomicU64>,value:ResourceResult,expires:std::time::Instant,_ticket:Ticket}
fn slot()->&'static Mutex<Option<Completed>> {static SLOT:OnceLock<Mutex<Option<Completed>>>=OnceLock::new();SLOT.get_or_init(||Mutex::new(None))}
fn worker()->Option<&'static SyncSender<Job>> {
    static SERVICE:OnceLock<Option<SyncSender<Job>>>=OnceLock::new();
    SERVICE.get_or_init(||{
        let (send,receive)=mpsc::sync_channel::<Job>(1);
        std::thread::Builder::new().name("skylive-image-preload".into()).spawn(move||{
            loop {
                let job=match receive.recv_timeout(std::time::Duration::from_millis(500)) {
                    Ok(job)=>job,
                    Err(mpsc::RecvTimeoutError::Timeout)=>{
                        if let Ok(mut value)=slot().lock(){if value.as_ref().is_some_and(|done|done.expires<=std::time::Instant::now()||done.live.load(Ordering::Acquire)!=done.epoch){*value=None;}}
                        continue;
                    },
                    Err(mpsc::RecvTimeoutError::Disconnected)=>break,
                };
                if job.live.load(Ordering::Acquire)!=job.epoch {continue;}
                let mut reads=crate::assets::ReadBudget::new(crate::assets::MAX_IMAGE_FILE_BYTES);
                let mut work=crate::assets::DecodeBudget::new(MAX_RENDER_IMAGE_BYTES);
                let mut value=crate::assets::load_clip_with_budgets(&job.asset,&mut reads,&mut work,MAX_RENDER_IMAGE_BYTES)
                    .map_err(|error|(error.kind(),error.code(),error.to_string()));
                if job.linear {value=value.and_then(|clip|{
                    let clip=Arc::try_unwrap(clip).map_err(|_|(crate::resource_issue::ResourceIssueKind::Unavailable,"image_alpha_ownership","A new linear-alpha clip must have exclusive ownership.".into()))?;
                    clip.into_linear_alpha().map(Arc::new).map_err(|error|(crate::resource_issue::ResourceIssueKind::Unavailable,"image_alpha_ownership",error.to_string()))
                });}
                if job.live.load(Ordering::Acquire)==job.epoch {
                    if let Ok(mut result)=slot().lock(){*result=Some(Completed {asset:job.asset,linear:job.linear,epoch:job.epoch,live:job.live,value,expires:std::time::Instant::now()+std::time::Duration::from_secs(2),_ticket:job.ticket});}
                }
            }
        }).ok().map(|_|send)
    }).as_ref()
}
pub(super) struct Preload {epoch:Arc<AtomicU64>,pending:Arc<AtomicBool>,retry_at:Option<std::time::Instant>}
impl Preload {
    fn new()->Self {Self{epoch:Arc::new(AtomicU64::new(1)),pending:Arc::new(AtomicBool::new(false)),retry_at:None}}
    pub(super) fn invalidate(&mut self){self.epoch.fetch_add(1,Ordering::AcqRel);self.retry_at=None;}
}
impl Drop for Preload {fn drop(&mut self){self.epoch.fetch_add(1,Ordering::AcqRel);}}
fn failure(kind:crate::resource_issue::ResourceIssueKind,code:&'static str,detail:&str)->ResourceResult{Err((kind,code,detail.into()))}
impl ClipCache {
    pub(super) fn preload_hidden(&mut self,pins:&Pins){
        if self.preload.is_none(){self.preload=Some(Preload::new());}
        let completed=self.preload.as_ref().and_then(|state|slot().try_lock().ok().and_then(|mut value|{
            if value.as_ref().is_some_and(|done|Arc::ptr_eq(&done.live,&state.epoch)){value.take()}else{None}
        }));
        if let Some(done)=completed {
            let current=self.preload.as_ref().unwrap().epoch.load(Ordering::Acquire);
            if done.epoch==current {
                let index=usize::from(done.linear);let bit=1u8<<index;
                let desired=pins.rows.get(&done.asset).is_some_and(|pin|pin.resident&bit!=0);
                if let Ok(clip)=&done.value {
                    let free=MAX_RENDER_IMAGE_BYTES.saturating_sub(self.bytes).min(MAX_RENDER_RESOURCE_BYTES.saturating_sub(self.bytes.saturating_add(self.text.bytes)));
                    let vacant=self.entries.get(&done.asset).is_none_or(|entry|entry.slots[index].is_none());
                    if desired&&vacant&&clip.byte_len()<=free&&self.admit_image_entry(&done.asset,&done.value)
                        &&(self.entries.contains_key(&done.asset)||self.entries.try_reserve(1).is_ok()) {
                        self.bytes+=clip.byte_len();
                        self.entries.entry(done.asset.clone()).or_default().slots[index]=Some(done.value.clone());
                    }else if desired&&vacant {
                        self.preload.as_mut().unwrap().retry_at=Some(std::time::Instant::now()+std::time::Duration::from_secs(5));
                    }
                }else{self.preload.as_mut().unwrap().retry_at=Some(std::time::Instant::now()+std::time::Duration::from_secs(5));}
            }
        }
        let state=self.preload.as_ref().unwrap();
        if state.pending.load(Ordering::Acquire)||state.retry_at.is_some_and(|at|std::time::Instant::now()<at){return;}
        let candidate=pins.rows.iter().find_map(|(asset,pin)|{
            (0..2).find_map(|index|{let bit=1u8<<index;
                if pin.resident&bit!=0&&pin.visible&bit==0&&self.entries.get(asset).is_none_or(|entry|entry.slots[index].is_none()) {Some((asset.clone(),index!=0))}else{None}
            })
        });
        let Some((asset,linear))=candidate else{return;};
        let Some(send)=worker() else{return;};
        if PRELOAD_BUSY.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).is_err(){return;}
        let state=self.preload.as_mut().unwrap();let epoch=state.epoch.load(Ordering::Acquire);
        state.pending.store(true,Ordering::Release);
        let job=Job{asset,linear,epoch,live:state.epoch.clone(),ticket:Ticket{pending:state.pending.clone()}};
        let _=send.try_send(job);
    }
    pub(super) fn retain_images(&mut self,pins:&Pins){
        self.entries.retain(|asset,entry|{
            let Some(pin)=pins.rows.get(asset)else{return false;};entry.visible=pin.visible;
            for(index,slot)in entry.slots.iter_mut().enumerate(){if pin.resident&(1<<index)==0{*slot=None;}}
            !entry.empty()
        });
        self.bytes=self.entries.values().map(Entry::bytes).sum();
    }
    fn evict_idle_images(&mut self){
        self.entries.retain(|_,entry|{
            for(index,slot)in entry.slots.iter_mut().enumerate(){if entry.visible&(1<<index)==0{*slot=None;}}
            !entry.empty()
        });self.bytes=self.entries.values().map(Entry::bytes).sum();
    }
    fn admit_image_entry(&self,asset:&ImageAsset,value:&ResourceResult)->bool{
        let new=usize::from(!self.entries.contains_key(asset));
        let count=self.entries.len().saturating_add(new);if count>MAX_KEYS{return false;}
        let paths=self.entries.iter().fold(0usize,|sum,(asset,entry)|sum.saturating_add(asset.path().len()).saturating_add(entry.error_bytes()))
            .saturating_add(if new!=0{asset.path().len()}else{0})
            .saturating_add(value.as_ref().err().map_or(0,|(_,_,detail)|detail.len().saturating_mul(2)));
        let buckets=self.entries.capacity().max(count).saturating_add(1).saturating_mul(4)
            .saturating_mul(std::mem::size_of::<(ImageAsset,Entry)>()+32);
        paths.saturating_add(buckets)<=MAX_METADATA
    }
    pub(super) fn load(&mut self,asset:&ImageAsset,reads:&mut crate::assets::ReadBudget,work:&mut crate::assets::DecodeBudget,live_bytes:usize)->ResourceResult{
        self.load_image(asset,false,reads,work,live_bytes)
    }
    pub(super) fn load_image(&mut self,asset:&ImageAsset,linear:bool,reads:&mut crate::assets::ReadBudget,work:&mut crate::assets::DecodeBudget,live_bytes:usize)->ResourceResult{
        let index=usize::from(linear);let bit=1<<index;
        if let Some(entry)=self.entries.get_mut(asset){if let Some(value)=&entry.slots[index]{entry.visible|=bit;return value.clone();}}
        // Cold visible demand wins over old persistent pictures. Their exact
        // variants may be decoded again when shown; no quality is reduced.
        self.evict_idle_images();
        let empty=Err((crate::resource_issue::ResourceIssueKind::Limit,"image_cache_metadata",String::new()));
        if !self.admit_image_entry(asset,&empty){return failure(crate::resource_issue::ResourceIssueKind::Limit,"image_cache_metadata","Image clip metadata exceeds its bounded cache allowance.");}
        let mut result=crate::assets::load_clip_with_budgets(asset,reads,work,
            MAX_RENDER_IMAGE_BYTES.saturating_sub(self.bytes).min(MAX_RENDER_RESOURCE_BYTES.saturating_sub(self.bytes.saturating_add(self.text.bytes).saturating_add(live_bytes))))
            .map_err(|error|(error.kind(),error.code(),error.to_string()));
        if linear {result=result.and_then(|clip|{
            let clip=Arc::try_unwrap(clip).map_err(|_|(crate::resource_issue::ResourceIssueKind::Unavailable,"image_alpha_ownership","A new linear-alpha clip must have exclusive ownership.".into()))?;
            clip.into_linear_alpha().map(Arc::new).map_err(|error|(crate::resource_issue::ResourceIssueKind::Unavailable,"image_alpha_ownership",error.to_string()))
        });}
        if !self.admit_image_entry(asset,&result){
            if let Err((_,code,detail))=&result{eprintln!("image resource {code}: {detail}");}
            return failure(crate::resource_issue::ResourceIssueKind::Limit,"image_cache_metadata","Image clip metadata exceeds its bounded cache allowance.");
        }
        if !self.entries.contains_key(asset)&&self.entries.try_reserve(1).is_err(){return failure(crate::resource_issue::ResourceIssueKind::Limit,"image_cache_metadata","Image clip metadata allocation failed.");}
        if let Ok(clip)=&result{self.bytes+=clip.byte_len();}
        if let Some(entry)=self.entries.get_mut(asset){entry.visible|=bit;entry.slots[index]=Some(result.clone());}
        else{let mut entry=Entry::default();entry.visible=bit;entry.slots[index]=Some(result.clone());self.entries.insert(asset.clone(),entry);}result
    }
}
