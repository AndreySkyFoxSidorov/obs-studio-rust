//! Root-owned exact source-chain single flight. No UI or global locator.
use super::*;
#[path="temporal.rs"] mod temporal;
#[path="freeze_timing_service.rs"] mod freeze_timing_service;
#[path="person_cache.rs"] pub(super) mod person_cache;
pub(crate) use temporal::{Observer as TemporalObserver,Snapshot as TemporalSnapshot,Context as TemporalContext,DraftRows};
pub(super) use temporal::active as has_delay;
use skylive_core::VideoFilter;
use std::{collections::HashMap,sync::{Mutex,Condvar,Weak,atomic::{AtomicBool,AtomicU64,AtomicUsize,Ordering},mpsc::{self,SyncSender}},thread,time::{Duration,Instant}};
const MAX_BYTES:usize=128*1024*1024;
// Source dimensions may exceed the preview viewport. The existing aggregate
// credit admits input + complete planned peak + metadata before queueing; do
// not reject a legal4K->1080 Scale solely because its input exceeds1080p.
const MAX_INPUT:usize=MAX_BYTES;
// A legal chain can contain 32 resource paths of up to 4096 UTF-8 bytes.
// JSON may escape each ASCII byte (notably Windows path separators), and the
// filter names and settings also contribute to the serialized identity.
// Keep one bounded limit for all filter-chain identity writers.
pub(super) const MAX_KEY:usize=512*1024;
const MAX_ENTRIES:usize=8;
const MAX_FLIGHTS:usize=2;
const MAX_WAITERS:usize=8;
const ALGORITHM:&str="skylive-source-chain-source13-v15-gaussian-craft11-photo-filter-sepia-vignette-source-scale-lut-render-delay-freeze-manual";
/// Identity of one real camera stream. A new capture owner, stream or binding
/// must never inherit pixels from an earlier session.
#[derive(Clone,Hash,PartialEq,Eq)]
pub(crate) struct CameraScope {
    pub session:u64,
    pub source:skylive_core::Id,
    pub binding:crate::capture::CaptureBinding,
    pub origin:crate::capture::NativeFrameOrigin,
}
fn static_camera_chain(filters:&[VideoFilter],providers:FilterProviders)->bool {
    filters.iter().any(|f|f.enabled&&providers.enabled(f.plugin_id()))
        && filters.iter().all(|f|!f.enabled||!providers.enabled(f.plugin_id())||!matches!(f.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{..}|VideoEffect::AsyncVideoDelay{..}
            |VideoEffect::Scroll{..}|VideoEffect::ScaleFilter{..}|VideoEffect::ApplyLut{..}
            |VideoEffect::ImageMask{..}|VideoEffect::PersonBackgroundRemoval{..}))
}
// A previous fully processed camera frame can remain visible while a cosmetic
// adjustment is being recomputed. Preserve every filter identity, ordering,
// enabled state and resource, and never reuse a frame when a changed stage is
// upstream of a keyer: that change can alter which pixels the keyer removes.
fn cosmetic_camera_edit(previous:&[u8],current:&[VideoFilter],providers:FilterProviders)->bool {
    let Ok(previous)=serde_json::from_slice::<Vec<VideoFilter>>(previous) else{return false;};
    if previous.len()!=current.len()||!static_camera_chain(&previous,providers){return false;}
    let mut changed=false;
    for (old,new) in previous.iter().zip(current) {
        if old==new { 
            if changed && new.enabled && providers.enabled(new.plugin_id()) && matches!(new.effect,VideoEffect::LumaKey{..}|VideoEffect::ColorKey{..}|VideoEffect::ChromaKey{..}|VideoEffect::LinearColorKey{..}) {return false;}
            continue;
        }
        // Color Correction's multiply/add colors are RGB-only. Keep its
        // alpha-controlling opacity bit-identical before allowing an older
        // frame; the unchanged-keyer check below still applies downstream.
        let color_correction_safe=matches!((old.effect,new.effect),
            (VideoEffect::ColorCorrection {opacity:before,..},VideoEffect::ColorCorrection {opacity:after,..})
                if before.to_bits()==after.to_bits());
        if !old.enabled||!new.enabled||old.id().is_none()||old.id()!=new.id()
            ||std::mem::discriminant(&old.effect)!=std::mem::discriminant(&new.effect)
            ||!(color_correction_safe||matches!(new.effect,VideoEffect::Sharpen{..}|VideoEffect::Invert{..}|VideoEffect::Grayscale{..}
                |VideoEffect::Posterize{..}|VideoEffect::Exposure{..}|VideoEffect::TemperatureTint{..}|VideoEffect::Vibrance{..}|VideoEffect::CraftTint{..}|VideoEffect::Tritone{..}|VideoEffect::CraftColorBalance{..}|VideoEffect::CraftChannelMixer{..}|VideoEffect::CraftGammaPedestalGain{..}
                |VideoEffect::Threshold{..}|VideoEffect::Vignette{..}|VideoEffect::Sepia{..}
                |VideoEffect::PhotoFilter{..}|VideoEffect::BrightnessContrast{..}|VideoEffect::SpillSuppression{..})) {return false;}
        let mut normalized=old.clone();normalized.effect=new.effect;
        if normalized!=*new{return false;}
        changed=true;
    }
    changed
}
// A person fallback is a previously completed full chain, never a raw camera
// frame or a mask applied to a different frame. External resources and
// time-dependent stages need their own provenance and remain fail-closed.
fn person_camera_chain(filters:&[VideoFilter],providers:FilterProviders)->bool {
    providers.person && filters.iter().any(|f|f.enabled&&matches!(f.effect,VideoEffect::PersonBackgroundRemoval{..})&&f.person_interval_ms()!=0)
        && filters.iter().all(|f|!f.enabled||!providers.enabled(f.plugin_id())||!matches!(f.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{..}|VideoEffect::AsyncVideoDelay{..}
            |VideoEffect::Scroll{..}|VideoEffect::ScaleFilter{..}|VideoEffect::ApplyLut{..}|VideoEffect::ImageMask{..}))
}
fn same_person_fallback_scope(mut previous:TemporalContext,mut current:TemporalContext)->bool {
    // Frame sequence and clock may advance; the stream, binding, configuration,
    // observer, grant and model inputs' geometry must not. Never use a result
    // from a future sample after a clock or capture discontinuity.
    if !previous.camera||!current.camera||!previous.input_complete||!current.input_complete
        ||previous.at>current.at||previous.stamp.sequence>current.stamp.sequence
        ||previous.stamp.at>current.stamp.at{return false;}
    previous.at=0;current.at=0;previous.tick=0;current.tick=0;
    previous.stamp.capture_epoch=0;current.stamp.capture_epoch=0;
    previous.stamp.sequence=0;current.stamp.sequence=0;
    previous.stamp.at=0;current.stamp.at=0;
    previous==current
}
#[derive(Clone,Default)]
struct Backend {
    #[cfg(feature="gpu-filters")]
    gpu:Option<GpuPointConfig>,
}
impl Backend {
    fn from_cache(cache:&FilterCache)->Self {
        #[cfg(feature="gpu-filters")]
        {Self {gpu:cache.gpu.clone()}}
        #[cfg(not(feature="gpu-filters"))]
        {let _=cache;Self {}}
    }
    fn signature(&self)->[u64;3] {
        #[cfg(feature="gpu-filters")]
        if let Some(gpu)=&self.gpu {let [kind,identity]=gpu.client.identity();return [kind,identity,match gpu.policy {GpuFailurePolicy::RequireGpu=>0,GpuFailurePolicy::PreferGpu=>1}];}
        [0;3]
    }
    fn cache(&self)->FilterCache {
        #[cfg(feature="gpu-filters")]
        if let Some(gpu)=&self.gpu {return FilterCache::with_gpu(gpu.client.clone(),gpu.policy);}
        FilterCache::default()
    }
    fn pending(&self)->usize {
        #[cfg(feature="gpu-filters")]
        if let Some(gpu)=&self.gpu {return gpu.client.inflight();}
        0
    }
}
#[derive(Clone,Hash,PartialEq,Eq)]
struct SharedKey {
    camera:Option<CameraScope>,
    temporal:Option<TemporalContext>,
    freeze_requests:[[u64;4];skylive_core::MAX_VIDEO_FILTERS],
    person_scope:Option<TemporalContext>,
    canvas:Option<(u32,u32)>,
    scroll:crate::scroll_animation::ScrollKey,
    input:usize,pixels:usize,width:u32,height:u32,blend:SourceBlend,filters:Vec<u8>,
    mask_epoch:u64,mask_stamp:Option<(u64,u64,u64,u64,u64,u64)>,resource_generation:u64,craft:bool,person:bool,freeze:bool,backend:[u64;3],algorithm:&'static str,model:&'static str,model_sha256:&'static str,
}
#[derive(Clone,Copy)]
pub(super) struct Admission {pub peak:usize,pub work:usize,pub texels:u64,pub width:u32,pub height:u32}
struct Budget {bytes:AtomicUsize}
pub(super) struct Lease {budget:Arc<Budget>,bytes:usize,frames:Vec<temporal::Frame>}
impl Lease {
    fn acquire(budget:&Arc<Budget>,bytes:usize)->Option<Self>{
        budget.bytes.fetch_update(Ordering::AcqRel,Ordering::Acquire,|value|value.checked_add(bytes).filter(|next|*next<=MAX_BYTES)).ok()?;
        Some(Self {budget:budget.clone(),bytes,frames:Vec::new()})
    }
    fn split(&mut self,bytes:usize)->Result<Self,FilterError>{if bytes>self.bytes{return Err(limited("Render Delay split credit overflow"));}self.bytes-=bytes;Ok(Self{budget:self.budget.clone(),bytes,frames:Vec::new()})}
    // Consolidate independently preadmitted metadata after its old allocation
    // has retired. Raster-bearing leases cannot cross this boundary.
    fn merge_metadata(&mut self,mut other:Self)->Result<(),FilterError>{
        if !Arc::ptr_eq(&self.budget,&other.budget)||!self.frames.is_empty()||!other.frames.is_empty(){return Err(limited("Render Delay scope metadata capacity"));}
        self.bytes=self.bytes.checked_add(other.bytes).ok_or_else(||limited("Render Delay scope metadata capacity"))?;
        other.bytes=0;Ok(())
    }
    fn shrink(&mut self,bytes:usize){debug_assert!(bytes<=self.bytes);self.budget.bytes.fetch_sub(self.bytes-bytes,Ordering::AcqRel);self.bytes=bytes;}
}
impl Drop for Lease {fn drop(&mut self){self.budget.bytes.fetch_sub(self.bytes,Ordering::AcqRel);}}
#[derive(Clone)]
// Weak retains the Arc allocation's identity, so the address cannot be reused,
// without pinning old capture pixels/native resident credits in the global LRU.
// The actual Job and each active renderer still own their strong input lease.
struct Value {input:skylive_media::WeakSharedVideoFrame,output:Arc<RgbaFrame>,blend:SourceBlend,
    #[cfg(feature="gpu-filters")] report:Option<crate::gpu_color_correction::Report>,
    #[cfg(feature="gpu-filters")] gpu_error:Option<crate::gpu_color_correction::Error>,
    #[cfg(feature="gpu-filters")] gpu_calls:usize,
    lease:Arc<Lease>,
}
struct Retained {value:Value,touched:u64}
struct FreezeFallback{key:SharedKey,value:Value,targets:[Option<crate::freeze_timing_book::Target>;skylive_core::MAX_VIDEO_FILTERS],runs:[u64;skylive_core::MAX_VIDEO_FILTERS],_credit:Arc<dyn Send+Sync>}
#[derive(Clone)]
struct Reply {value:Result<Value,FilterError>,computed:bool}
struct Flight {result:Mutex<Option<Reply>>,ready:Condvar,waiters:AtomicUsize}
struct Waiter(Arc<Flight>);
impl Drop for Waiter {fn drop(&mut self){self.0.waiters.fetch_sub(1,Ordering::AcqRel);}}
struct Job {temporal:Option<TemporalContext>,person:Option<TemporalContext>,scroll:crate::scroll_animation::Context,mask_context:Option<(skylive_core::Id,crate::mask_animation_clock::Snapshot)>,key:SharedKey,input:skylive_media::SharedVideoFrame,ordinary_input:Option<Arc<RgbaFrame>>,async_guard:Option<AsyncCameraGuard>,filters:Vec<VideoFilter>,providers:FilterProviders,blend:SourceBlend,
    free_bytes:usize,free_work:usize,backend:Backend,flight:Arc<Flight>,credit:Lease,dimensions:(u32,u32)}
struct State {freeze_timers:crate::freeze_timing_book::Book,freeze_timer_bytes:Arc<AtomicUsize>,freeze_fallbacks:Vec<FreezeFallback>,freeze_fallback_table:Option<Arc<dyn Send+Sync>>,desired:[Option<Arc<temporal::ScopeSet>>;2],observers:[u64;2],entries:HashMap<SharedKey,Retained>,flights:HashMap<SharedKey,Arc<Flight>>,sender:Option<SyncSender<Job>>,
    _thread:Option<thread::JoinHandle<()>>,clock:u64}
pub(super) struct Shared {mask_versions:Arc<Mutex<crate::mask_live_versions::Book>>,luts:Arc<Mutex<lut_chain::LutStore>>,masks:Arc<Mutex<image_mask::MaskStore>>,person_masks:std::sync::OnceLock<Arc<Mutex<person_cache::Store>>>,person_inference_gate:Arc<Mutex<()>>,state:Mutex<State>,closed:AtomicBool,budget:Arc<Budget>,computed:AtomicU64,hits:AtomicU64,joined:AtomicU64,rejected:AtomicU64}
fn freeze_requests(state:&State,context:Option<TemporalContext>,filters:&[VideoFilter])->[[u64;4];skylive_core::MAX_VIDEO_FILTERS]{
    let mut result=[[0;4];skylive_core::MAX_VIDEO_FILTERS];
    if let Some(context)=context{for(index,filter)in filters.iter().enumerate().take(result.len()){
        if filter.enabled&&matches!(filter.effect,skylive_core::VideoEffect::Freeze{..}){
            if let Some(signature)=temporal::freeze_target(context,index,filter).and_then(|target|state.freeze_timers.cache_signature(target)){
                result[index]=signature;
            }
        }
    }}result
}
fn same_freeze_scope(a:TemporalContext,b:TemporalContext)->bool{
    a.domain==b.domain&&a.session==b.session&&a.source==b.source&&a.owner==b.owner&&a.grant==b.grant
}
// A completed fallback after the final active Freeze already contains every
// upstream mask. A downstream animated mask still needs the current time.
fn mask_fallback_matches(old:Option<(u64,u64,u64,u64,u64,u64)>,now:Option<(u64,u64,u64,u64,u64,u64)>,filters:&[VideoFilter],providers:FilterProviders)->bool{
    let last=filters.iter().rposition(|filter|filter.enabled&&providers.freeze&&providers.enabled(filter.plugin_id())&&matches!(filter.effect,VideoEffect::Freeze{..}));
    let downstream=last.is_none_or(|index|image_mask::has_animated_mask(&filters[index+1..],None));
    if downstream{return old==now;}
    match (old,now){(None,None)=>true,(Some((a,b,c,d,e,_)),Some((f,g,h,i,j,_)))=>(a,b,c,d,e)==(f,g,h,i,j),_=>false}
}
fn same_fallback_input(key:&SharedKey,context:TemporalContext,filters:&[u8],mask_filters:&[VideoFilter],canvas:Option<(u32,u32)>,input:&RgbaFrame,blend:SourceBlend,providers:FilterProviders,backend:&Backend,mask_epoch:u64,mask_stamp:Option<(u64,u64,u64,u64,u64,u64)>,resource_generation:u64)->bool{
    let Some(old)=key.temporal else{return false;};
    same_freeze_scope(old,context)&&old.binding==context.binding&&old.rows==context.rows&&old.rate==context.rate&&old.interval==context.interval&&old.input_complete&&key.freeze&&key.width==input.width()&&key.height==input.height()&&key.blend==blend&&key.filters.as_slice()==filters&&key.canvas==canvas&&key.mask_epoch==mask_epoch&&mask_fallback_matches(key.mask_stamp,mask_stamp,mask_filters,providers)&&key.resource_generation==resource_generation&&key.craft==providers.craft&&key.person==providers.person&&key.freeze==providers.freeze&&key.backend==backend.signature()
}
fn pin_freeze_fallback(state:&mut State,shared:&Shared,key:&SharedKey,filters:&[VideoFilter],value:&Value)->Result<Option<FreezeFallback>,FilterError>{
    let Some(context)=key.temporal.filter(|_|key.freeze)else{return Ok(None);};
    let position=state.freeze_fallbacks.iter().position(|entry|entry.key.temporal.is_some_and(|old|same_freeze_scope(old,context)));
    if position.is_none()&&state.freeze_fallbacks.len()>=crate::freeze_timing_book::MAX_TARGETS{return Err(limited("Freeze fallback source count128"));}
    let bytes=std::mem::size_of::<FreezeFallback>().checked_add(key.filters.len()).and_then(|v|v.checked_add(128)).ok_or_else(||limited("Freeze fallback metadata overflow"))?;
    let credit=freeze_timing_service::reserve(&state.freeze_timer_bytes,&shared.budget,bytes).ok_or_else(||limited("Freeze fallback metadata budget"))?;
    if position.is_none()&&state.freeze_fallbacks.len()==state.freeze_fallbacks.capacity(){
        let capacity=state.freeze_fallbacks.capacity().max(2).saturating_mul(2).min(crate::freeze_timing_book::MAX_TARGETS);
        let admitted=capacity.checked_mul(2).ok_or_else(||limited("Freeze fallback table overflow"))?;
        let bytes=admitted.checked_mul(std::mem::size_of::<FreezeFallback>()).ok_or_else(||limited("Freeze fallback table overflow"))?;
        let table=freeze_timing_service::reserve(&state.freeze_timer_bytes,&shared.budget,bytes).ok_or_else(||limited("Freeze fallback table budget"))?;
        state.freeze_fallbacks.try_reserve_exact(capacity-state.freeze_fallbacks.len()).map_err(|_|limited("Freeze fallback table allocation"))?;
        if state.freeze_fallbacks.capacity()>admitted{return Err(limited("Freeze fallback table capacity"));}
        state.freeze_fallback_table=Some(table);
    }
    let mut targets=[None;skylive_core::MAX_VIDEO_FILTERS];let mut runs=[0;skylive_core::MAX_VIDEO_FILTERS];for(index,filter)in filters.iter().enumerate().take(targets.len()){if filter.enabled&&matches!(filter.effect,skylive_core::VideoEffect::Freeze{..}){targets[index]=temporal::freeze_target(context,index,filter);runs[index]=targets[index].and_then(|target|state.freeze_timers.run(target)).unwrap_or(0);}}
    if targets.iter().enumerate().any(|(index,target)|target.is_some()&&runs[index]==0){return Err(temporal::stale("Freeze fallback run is not registered"));}
    let replacement=FreezeFallback{key:key.clone(),value:value.clone(),targets,runs,_credit:credit};
    if let Some(index)=position{Ok(Some(std::mem::replace(&mut state.freeze_fallbacks[index],replacement)))}else{state.freeze_fallbacks.push(replacement);Ok(None)}
}
struct Life {shared:Arc<Shared>}
impl Drop for Life {fn drop(&mut self){self.shared.closed.store(true,Ordering::Release);}}
#[derive(Clone)]
pub(crate) struct Runtime {life:Arc<Life>}
#[derive(Clone,Copy,Debug,Default)]
pub struct Stats {pub computations:u64,pub hits:u64,pub joined:u64,pub rejected:u64,pub charged_bytes:usize,pub entries:usize,pub flights:usize,pub occupancy_available:bool}
impl Runtime {
    pub(crate) fn new()->Self {let mask_versions=Arc::new(Mutex::new(crate::mask_live_versions::Book::default()));Self {life:Arc::new(Life {shared:Arc::new(Shared {mask_versions:mask_versions.clone(),luts:Arc::new(Mutex::new(lut_chain::LutStore::default())),masks:Arc::new(Mutex::new(image_mask::MaskStore::with_versions(mask_versions))),person_masks:std::sync::OnceLock::new(),person_inference_gate:Arc::new(Mutex::new(())),state:Mutex::new(State {freeze_timers:Default::default(),freeze_timer_bytes:Arc::new(AtomicUsize::new(0)),freeze_fallbacks:Vec::new(),freeze_fallback_table:None,desired:[None,None],observers:[0;2],entries:HashMap::new(),flights:HashMap::new(),sender:None,_thread:None,clock:0}),
        closed:AtomicBool::new(false),budget:Arc::new(Budget {bytes:AtomicUsize::new(0)}),computed:AtomicU64::new(0),hits:AtomicU64::new(0),joined:AtomicU64::new(0),rejected:AtomicU64::new(0)})})}}
    #[allow(clippy::too_many_arguments)]
    fn freeze_fallback(&self,filters:&[VideoFilter],input:&RgbaFrame,blend:SourceBlend,providers:FilterProviders,context:TemporalContext,backend:&Backend,mask_epoch:u64,mask_stamp:Option<(u64,u64,u64,u64,u64,u64)>,resource_generation:u64)->Option<Value>{
        let encoded=serde_json::to_vec(filters).ok()?;if encoded.len()>MAX_KEY{return None;}
        let mut pending=[None;skylive_core::MAX_VIDEO_FILTERS];let mut affected=[None;skylive_core::MAX_VIDEO_FILTERS];let mut count=0usize;let mut affected_count=0usize;let mut installed_this_sample=false;
        let value={let state=self.life.shared.state.lock().ok()?;
            if !temporal::History::current_filters_in(&state,context,filters,providers.freeze){return None;}
            if !temporal::History::exact_in(&state,context){return None;}
            for(index,filter)in filters.iter().enumerate().take(pending.len()){
                if !filter.enabled||!matches!(filter.effect,skylive_core::VideoEffect::Freeze{..}){continue;}
                if let Some(target)=temporal::freeze_target(context,index,filter){affected[affected_count]=Some(target);affected_count+=1;if let Some(plan)=state.freeze_timers.capture_plan(target){pending[count]=Some((target,plan));count+=1;}installed_this_sample|=state.freeze_timers.captured_at(target)==Some(context.at);}
            }
            if count==0&&!installed_this_sample{return None;}
            let canvas=scale::canvas_key(filters,providers);
            state.freeze_fallbacks.iter().find(|entry|same_fallback_input(&entry.key,context,&encoded,filters,canvas,input,blend,providers,backend,mask_epoch,mask_stamp,resource_generation)&&entry.targets.iter().enumerate().all(|(index,target)|target.is_none_or(|target|state.freeze_timers.run(target)==Some(entry.runs[index])))).map(|entry|entry.value.clone())
        };
        for(target,plan)in pending[..count].iter().flatten().copied(){self.life.shared.freeze_refresh_failed(target,plan);}
        if installed_this_sample{for target in affected[..affected_count].iter().flatten().copied(){self.life.shared.freeze_draw_failed(target);}}
        value
    }
    pub(super) fn lut_store(&self)->Arc<Mutex<lut_chain::LutStore>> {self.life.shared.luts.clone()}
    pub(super) fn person_store(&self)->Arc<Mutex<person_cache::Store>> {
        self.life.shared.person_masks.get_or_init(||Arc::new(Mutex::new(person_cache::Store::new(&self.life.shared)))).clone()
    }
    pub(super) fn person_inference_gate(&self)->Arc<Mutex<()>> {self.life.shared.person_inference_gate.clone()}
    pub(crate) fn mask_versions(&self)->Arc<Mutex<crate::mask_live_versions::Book>>{self.life.shared.mask_versions.clone()}
    // Read only an already completed, lease-accounted result. Input allocation
    // and sequence intentionally differ. Cosmetic changes can use the last
    // processed frame under the restrictions in cosmetic_camera_edit; identity,
    // geometry and privacy-sensitive settings still have to match. Animated and
    // resource chains, and raw camera images without active filters, are excluded.
    fn camera_fallback(&self,scope:&CameraScope,filters:&[VideoFilter],input:&RgbaFrame,blend:SourceBlend,
        providers:FilterProviders,backend:&Backend,resource_generation:u64,mask_epoch:u64,
        context:Option<TemporalContext>,free_bytes:usize)->Option<Value> {
        let person=person_camera_chain(filters,providers);
        if !person&&!static_camera_chain(filters,providers){return None;}
        let context=if person{Some(context.filter(|c|c.camera&&c.input_complete)?)}else{None};
        let encoded=serde_json::to_vec(filters).ok()?;
        if encoded.len()>MAX_KEY{return None;}
        let shared=&self.life.shared;
        if shared.closed.load(Ordering::Acquire){return None;}
        let state=shared.state.lock().ok()?;
        if context.is_some_and(|context|!temporal::History::current_person_in(&state,context)){return None;}
        state.entries.iter().filter(|(key,entry)|{
            key.camera.as_ref()==Some(scope)&&key.temporal.is_none()
                &&(match (key.person_scope,context){(None,None)=>true,(Some(old),Some(now))=>same_person_fallback_scope(old,now)
                    &&temporal::History::current_person_in(&state,old),_=>false})
                &&(key.filters==encoded||(!person&&cosmetic_camera_edit(&key.filters,filters,providers)))
                &&key.width==input.width()&&key.height==input.height()
                &&key.blend==blend&&key.canvas==scale::canvas_key(filters,providers)
                &&key.resource_generation==resource_generation&&key.mask_epoch==mask_epoch&&key.craft==providers.craft
                &&key.person==providers.person&&key.freeze==false&&key.backend==backend.signature()
                &&entry.value.output.byte_len()<=free_bytes
        }).max_by_key(|(key,entry)|(key.filters==encoded,entry.touched)).map(|(_,entry)|entry.value.clone())
    }
    pub(super) fn mask_store(&self)->Arc<Mutex<image_mask::MaskStore>> {self.life.shared.masks.clone()}
    pub(crate) fn stats(&self)->Stats {
        let shared=&self.life.shared;
        // Statistics are sampled by the GUI. Cache eviction may release a large
        // raster on its owner; observing counters must never wait for that work.
        let occupancy=shared.state.try_lock().ok().map(|state|(state.entries.len(),state.flights.len()));
        let (entries,flights)=occupancy.unwrap_or_default();
        Stats {computations:shared.computed.load(Ordering::Acquire),hits:shared.hits.load(Ordering::Acquire),joined:shared.joined.load(Ordering::Acquire),rejected:shared.rejected.load(Ordering::Acquire),charged_bytes:shared.budget.bytes.load(Ordering::Acquire),entries,flights,occupancy_available:occupancy.is_some()}
    }
    #[allow(clippy::too_many_arguments)]
    fn apply(&self,filters:&[VideoFilter],input:&Arc<RgbaFrame>,blend:SourceBlend,providers:FilterProviders,admission:Admission,
        free_bytes:usize,free_work:usize,backend:Backend,mask_epoch:u64,resource_generation:u64,scroll:crate::scroll_animation::Context,mask_context:Option<(skylive_core::Id,crate::mask_animation_clock::Snapshot)>,temporal:Option<TemporalContext>,person:Option<TemporalContext>,camera:Option<CameraScope>)->Result<Option<(Value,bool)>,FilterError> {
        let frame=skylive_media::SharedVideoFrame::from(input.clone());
        self.apply_frame(filters,&frame,Some(input.clone()),None,blend,providers,admission,free_bytes,free_work,backend,mask_epoch,resource_generation,scroll,mask_context,temporal,person,camera)
    }
    // The optional ordinary input is present only for frames that entered as a
    // plain Arc. A delayed frame never yields its inner Arc: the job and all
    // native requests carry the owning wrapper until real completion.
    #[allow(clippy::too_many_arguments)]
    fn apply_frame(&self,filters:&[VideoFilter],input:&skylive_media::SharedVideoFrame,ordinary_input:Option<Arc<RgbaFrame>>,async_guard:Option<AsyncCameraGuard>,blend:SourceBlend,providers:FilterProviders,admission:Admission,
        free_bytes:usize,free_work:usize,backend:Backend,mask_epoch:u64,resource_generation:u64,scroll:crate::scroll_animation::Context,mask_context:Option<(skylive_core::Id,crate::mask_animation_clock::Snapshot)>,temporal:Option<TemporalContext>,person:Option<TemporalContext>,camera:Option<CameraScope>)->Result<Option<(Value,bool)>,FilterError> {
        if async_guard.as_ref().is_some_and(|guard|!guard.current()){return Err(temporal::stale("delayed camera generation changed before admission"));}
        let temporal_picture=filters.iter().any(|filter|filter.enabled&&matches!(filter.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{delay_ms:1..=500}));
        let person=if temporal_picture{None}else{person.or(temporal.filter(|context|context.camera&&context.input_complete&&filters.iter().any(|filter|filter.enabled&&filter.person_interval_ms()!=0)))};
        // Frozen private scene instances own their effective timer state. They
        // cannot mutate the authored canonical source or draft checkbox.
        let mut disabled=[false;skylive_core::MAX_VIDEO_FILTERS];let mut any_disabled=false;
        if let Some(context)=temporal{for(index,filter)in filters.iter().enumerate().take(disabled.len()){
            if filter.enabled&&matches!(filter.effect,skylive_core::VideoEffect::Freeze{..}){
                if let Some(target)=temporal::freeze_target(context,index,filter){
                    if matches!(target.instance,crate::freeze_timing_book::Instance::Private{..}|crate::freeze_timing_book::Instance::DraftPrivate{..})&&self.life.shared.freeze_effective(target)==Some(false){disabled[index]=true;any_disabled=true;}
                }
            }
        }}
        let normalized=any_disabled.then(||{let mut values=filters.to_vec();for(index,filter)in values.iter_mut().enumerate().take(disabled.len()){if disabled[index]{filter.enabled=false;}}values});
        let filters=normalized.as_deref().unwrap_or(filters);
        if input.byte_len()>MAX_INPUT {return Ok(None);}
        let encoded=serde_json::to_vec(filters).map_err(|error|fail(error.to_string()))?;
        if encoded.len()>MAX_KEY{return Ok(None);}
        let active_bytes=input.byte_len().checked_add(admission.peak)
            .and_then(|value|value.checked_add(MAX_KEY*2))
            .and_then(|value|value.checked_add(std::mem::size_of::<RgbaFrame>()+2*std::mem::size_of::<usize>()))
            .ok_or_else(||limited("shared source-chain byte overflow"))?;
        if active_bytes>MAX_BYTES{return Ok(None);}
        if temporal.is_some_and(|context|!temporal::History::current_filters(&self.life.shared,context,filters,providers.freeze)){return Err(temporal::stale("Render Delay desired scope is obsolete"));}
        if person.is_some_and(|context|!temporal::History::current_person(&self.life.shared,context)){return Err(temporal::stale("Person mask camera scope is obsolete"));}
        let temporal_key=temporal.map(|mut context|{context.at=0;context.stamp=Default::default();context});
        let request_signature={let state=self.life.shared.state.lock().map_err(|_|unavailable("Freeze timer state poisoned"))?;freeze_requests(&state,temporal,filters)};
        let camera=if temporal.is_none()&&((person.is_none()&&static_camera_chain(filters,providers))
            ||(person.is_some()&&person_camera_chain(filters,providers))){camera}else{None};
        let key=SharedKey {camera,temporal:temporal_key,freeze_requests:request_signature,person_scope:person,canvas:scale::canvas_key(filters,providers),scroll:if temporal.is_some(){Default::default()}else{scroll.key},input:if temporal.is_some(){0}else{input.image() as *const RgbaFrame as usize},pixels:if temporal.is_some(){0}else{input.pixels().as_ptr() as usize},width:input.width(),height:input.height(),blend,
            filters:encoded,mask_epoch,mask_stamp:if image_mask::has_live_mask(filters){mask_context.as_ref().map(|(source,clock)|(source.get(),clock.session(),clock.scope(),clock.revision(),clock.files_generation(),if image_mask::has_animated_mask(filters,Some(clock)){clock.now().0}else{0}))}else{None},resource_generation,craft:providers.craft,person:providers.person,freeze:providers.freeze&&filters.iter().any(|filter|filter.enabled&&matches!(filter.effect,VideoEffect::Freeze{..})),backend:backend.signature(),algorithm:ALGORITHM,model:skylive_person_matte::MODEL_ID,model_sha256:skylive_person_matte::MODEL_SHA256};
        let shared=&self.life.shared;
        if shared.closed.load(Ordering::Acquire){return Err(unavailable("shared source filter service closed"));}
        let (flight,leader)=loop {
            let mut state=shared.state.lock().map_err(|_|unavailable("shared source filter lock poisoned"))?;
            if shared.closed.load(Ordering::Acquire){return Err(unavailable("shared source filter service closed"));}
            if temporal.is_some_and(|context|!temporal::History::current_filters_in(&state,context,filters,providers.freeze)){return Err(temporal::stale("Render Delay desired scope changed before admission"));}
            if freeze_requests(&state,temporal,filters)!=key.freeze_requests{return Err(temporal::stale("Freeze capture request changed before admission"));}
            if person.is_some_and(|context|!temporal::History::current_person_in(&state,context)){return Err(temporal::stale("Person camera scope changed before admission"));}
            state.clock=state.clock.checked_add(1).ok_or_else(||limited("shared source filter LRU identity exhausted"))?;let clock=state.clock;
            if let Some(retained)=state.entries.get_mut(&key) {
                if temporal.is_none()&&!retained.value.input.ptr_eq(input) {
                    return Err(fail("shared source filter input allocation changed"));
                }
                retained.touched=clock;shared.hits.fetch_add(1,Ordering::Relaxed);return Ok(Some((retained.value.clone(),false)));
            }
            if let Some(flight)=state.flights.get(&key) {
                flight.waiters.fetch_update(Ordering::AcqRel,Ordering::Acquire,|value|(value<MAX_WAITERS).then_some(value+1)).map_err(|_|{shared.rejected.fetch_add(1,Ordering::Relaxed);busy("shared source filter waiter capacity")})?;
                shared.joined.fetch_add(1,Ordering::Relaxed);break (flight.clone(),false);
            }else{
                if state.flights.len()>=MAX_FLIGHTS {shared.rejected.fetch_add(1,Ordering::Relaxed);return Err(busy("shared source filter queue full"));}
                let credit=if let Some(credit)=Lease::acquire(&shared.budget,active_bytes){credit}else{
                    let victim=state.entries.iter().min_by_key(|(_,entry)|entry.touched).map(|(key,_)|key.clone());
                    if let Some(victim)=victim {let retired=state.entries.remove(&victim);drop(state);drop(retired);continue;}
                    shared.rejected.fetch_add(1,Ordering::Relaxed);return Err(limited("shared source filter128MiB includes retained render leases"));
                };
                if state.sender.is_none() {
                    let (send,receive)=mpsc::sync_channel(MAX_FLIGHTS);let worker_shared=shared.clone();
                    let handle=thread::Builder::new().name("skylive-shared-source-filters".into()).spawn(move||worker(worker_shared,receive))
                        .map_err(|error|unavailable(format!("shared source filter worker: {error}")))?;
                    state.sender=Some(send);state._thread=Some(handle);
                }
                let flight=Arc::new(Flight {result:Mutex::new(None),ready:Condvar::new(),waiters:AtomicUsize::new(1)});
                let job=Job {temporal,person,scroll,mask_context:mask_context.clone(),key:key.clone(),input:input.clone(),ordinary_input:ordinary_input.clone(),async_guard:async_guard.clone(),filters:filters.to_vec(),providers,blend,free_bytes,free_work,backend,flight:flight.clone(),credit,dimensions:(admission.width,admission.height)};
                state.flights.insert(key.clone(),flight.clone());
                if state.sender.as_ref().ok_or_else(||unavailable("shared source filter channel absent"))?.try_send(job).is_err(){state.flights.remove(&key);return Err(unavailable("shared source filter worker unavailable"));}
                break (flight,true);
            }
        };
        let _waiter=Waiter(flight.clone());let started=Instant::now();let mut result=flight.result.lock().map_err(|_|unavailable("shared source filter result poisoned"))?;
        loop {
            if let Some(reply)=&*result {if temporal.is_some_and(|context|!temporal::History::current_filters(&self.life.shared,context,filters,providers.freeze)){return Err(temporal::stale("Render Delay scope changed while waiting"));}if person.is_some_and(|context|!temporal::History::current_person(&self.life.shared,context)){return Err(temporal::stale("Person camera scope changed while waiting"));}return reply.value.clone().map(|value|Some((value,leader && reply.computed)));}
            let remaining=Duration::from_secs(10).checked_sub(started.elapsed()).ok_or_else(||unavailable("shared source filter wait expired; computation remains owned"))?;
            let (next,timeout)=flight.ready.wait_timeout(result,remaining).map_err(|_|unavailable("shared source filter wait poisoned"))?;result=next;
            if timeout.timed_out() && result.is_none(){return Err(unavailable("shared source filter wait expired; computation remains owned"));}
        }
    }
}
// Delayed wrappers can already use the point-effect pipeline without copying
// their pixels. Stateful and resource-backed chains remain explicitly rejected
// until their own input/result contracts also retain the wrapper.
fn apply_delayed_points(cache:&mut FilterCache,filters:&[VideoFilter],input:skylive_media::SharedVideoFrame,blend:SourceBlend,providers:FilterProviders,free_bytes:usize,free_work:usize)->Result<FilterResult,FilterError>{
    let mut effects=Vec::new();
    for filter in filters.iter().filter(|filter|filter.enabled&&providers.enabled(filter.plugin_id())) {
        filter.validate().map_err(|error|fail(error.to_string()))?;
        if matches!(filter.effect,VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX}|VideoEffect::Freeze{..}|VideoEffect::RenderDelay{..}|VideoEffect::ScaleFilter{..}|VideoEffect::Scroll{..}|VideoEffect::ApplyLut{..}|VideoEffect::ImageMask{..}|VideoEffect::PersonBackgroundRemoval{..}) {
            return Err(unavailable("delayed input source chain contains an unsupported stateful or resource-backed filter"));
        }
        match prepare_filter(filter).1 {Some(FilterStage::Point(effect))=>effects.push(effect),Some(FilterStage::Person{..})=>return Err(unavailable("delayed input person filter is not yet supported")),None=>{}}
    }
    if effects.is_empty(){return Err(unavailable("delayed passthrough requires a lease-bearing result"));}
    let required=skylive_media::source_effect_requirements(&input,&effects).map_err(media_fail)?;
    let limits=SourceEffectsLimits{max_output_bytes:free_bytes.saturating_sub(required.workspace_bytes),max_workspace_bytes:required.workspace_bytes,max_pixel_stages:free_work,max_texel_visits:MAX_TEXEL_VISITS};
    let output=cache.apply_points(input,blend,&effects,limits,free_bytes)?;
    let image=Arc::new(output.image);let new_bytes=image.byte_len();
    Ok(FilterResult{image,blend:output.blend,new_bytes,pixel_stages:required.pixel_stages})
}
fn unavailable(detail:impl Into<String>)->FilterError {FilterError {kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"shared_source_filter_unavailable",detail:detail.into()}}
fn busy(detail:impl Into<String>)->FilterError {FilterError {kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"shared_source_filter_busy",detail:detail.into()}}
fn finish(flight:&Flight,reply:Reply){let mut result=flight.result.lock().unwrap_or_else(|error|error.into_inner());*result=Some(reply);flight.ready.notify_all();}
struct Cleanup(Arc<Shared>);
impl Drop for Cleanup {fn drop(&mut self){self.0.closed.store(true,Ordering::Release);let mut state=self.0.state.lock().unwrap_or_else(|error|error.into_inner());
    for (_,flight) in state.flights.drain(){finish(&flight,Reply {value:Err(unavailable("shared source filter owner retired")),computed:false});}state.entries.clear();state.sender.take();}}
fn worker(shared:Arc<Shared>,receive:mpsc::Receiver<Job>){
    let _cleanup=Cleanup(shared.clone());
    let _services=crate::preview::retain_render_services();
    let mut history=temporal::History::default();
    let person_store=shared.person_masks.get_or_init(||Arc::new(Mutex::new(person_cache::Store::new(&shared)))).clone();
    while !shared.closed.load(Ordering::Acquire) {
        history.reconcile(&shared);
        person_store.lock().unwrap_or_else(|e|e.into_inner()).reconcile();
        let job=match receive.recv_timeout(Duration::from_millis(10)){Ok(job)=>job,Err(mpsc::RecvTimeoutError::Timeout)=>continue,Err(_)=>break};
        let Job {temporal,person,scroll,mask_context,key,input,ordinary_input,async_guard,filters,providers,blend,free_bytes,free_work,backend,flight,mut credit,dimensions}=job;
        let mut cache=backend.cache();if temporal.is_none()&&!filters.iter().any(|filter|filter.enabled&&matches!(filter.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{delay_ms:1..=500})){cache.person_store=Some(person_store.clone());cache.person_inference_gate=Some(shared.person_inference_gate.clone());}cache.temporal_context=temporal.or(person);cache.scroll_context=scroll;cache.mask_context=mask_context;cache.mask_epoch=key.mask_epoch;cache.resource_generation=key.resource_generation;cache.mask_store=Some(shared.masks.clone());cache.lut_store=Some(shared.luts.clone());
        cache.temporal_async_guard=async_guard.clone();
        let result=if async_guard.as_ref().is_some_and(|guard|!guard.current()) {
            Err(temporal::stale("delayed camera generation changed before worker execution"))
        }else if let Some(ordinary)=ordinary_input {
            if let Some(context)=temporal {history.apply(&shared,&mut cache,&filters,ordinary,blend,providers,context,free_bytes,free_work).map(|(result,frames)|{credit.frames=frames;result})}
            else{cache.apply_filters(&filters,ordinary,blend,providers,free_bytes,free_work)}
        }else if let (Some(context),Some(_))=(temporal,async_guard.as_ref()) {
            // The Job's original delayed input owns its capture credit. The
            // separately budgeted copy stays on this worker or in a result
            // whose shared lease includes its full allocation.
            temporal::copy_frame(&input).and_then(|copy|
                history.apply(&shared,&mut cache,&filters,Arc::new(copy),blend,providers,context,free_bytes,free_work)
                    .map(|(result,frames)|{credit.frames=frames;result}))
        }else if temporal.is_some() || person.is_some() {
            Err(unavailable("delayed input is not yet supported by temporal or person source filters"))
        }else{
            apply_delayed_points(&mut cache,&filters,input.clone(),blend,providers,free_bytes,free_work)
        };
        // A timed-out GPU call can still own the input and readback. Preserve
        // this admission through real native retirement, including on Close.
        while backend.pending()!=0 {thread::park_timeout(Duration::from_millis(10));}
        shared.computed.fetch_add(1,Ordering::Relaxed);
        let mut value=result.and_then(|result|{
            if async_guard.as_ref().is_some_and(|guard|!guard.current()){return Err(temporal::stale("delayed camera generation changed during processing"));}
            if temporal.is_some_and(|context|!temporal::History::current_filters(&shared,context,&filters,providers.freeze)){return Err(temporal::stale("Render Delay scope obsolete after backend retirement"));}
            if person.is_some_and(|context|!temporal::History::current_person(&shared,context)){return Err(temporal::stale("Person mask scope changed after inference"));}
            if (result.image.width(),result.image.height())!=dimensions{return Err(fail("shared source filter dimensions changed"));}
            let bytes=result.image.byte_len().checked_add(MAX_KEY*2)
                .and_then(|value|value.checked_add(std::mem::size_of::<RgbaFrame>()+2*std::mem::size_of::<usize>()))
                .ok_or_else(||limited("shared source filter retained overflow"))?;
            if bytes>credit.bytes{return Err(limited("shared source filter retained exceeds admitted footprint"));}
            // Keep the detached input charged while worker-local cache entries
            // or a passthrough result may still own it. This is conservative
            // after they retire, but never releases credit before destruction.
            let retained=if async_guard.is_some(){bytes.checked_add(input.byte_len()).ok_or_else(||limited("delayed temporal retained overflow"))?}else{bytes};
            if retained>credit.bytes{return Err(limited("delayed temporal retained exceeds admission"));}
            credit.shrink(retained);
            Ok(Value {input:input.downgrade(),output:result.image,blend:result.blend,lease:Arc::new(credit),
                #[cfg(feature="gpu-filters")] report:cache.gpu_report.clone(),
                #[cfg(feature="gpu-filters")] gpu_error:cache.gpu_last_error.clone(),
                #[cfg(feature="gpu-filters")] gpu_calls:cache.gpu_calls,
            })
        });
        drop(cache);
        let mut evicted=Vec::new();let mut retired_fallback=None;let mut retired_value=None;
        {let mut state=shared.state.lock().unwrap_or_else(|error|error.into_inner());
            if !shared.closed.load(Ordering::Acquire)&&async_guard.as_ref().is_none_or(|guard|guard.current())&&temporal.is_none_or(|context|temporal::History::current_filters_in(&state,context,&filters,providers.freeze))&&person.is_none_or(|context|temporal::History::current_person_in(&state,context)) {
                let tracked=temporal.is_some_and(|context|filters.iter().enumerate().any(|(index,filter)|filter.enabled&&matches!(filter.effect,skylive_core::VideoEffect::Freeze{..})&&temporal::freeze_target(context,index,filter).is_some_and(|target|state.freeze_timers.run(target).is_some())));
                let exact=temporal.is_some_and(|context|temporal::History::exact_in(&state,context));
                let pin=if key.freeze&&tracked&&exact&&temporal.is_some_and(|context|context.input_complete){if let Ok(value)=&value{
                    // Pin the last real complete source result independently of
                    // the eight-entry LRU. Old/new metadata and pixel leases are
                    // admitted while both remain alive. A failed pin keeps the
                    // old fallback; no full raster is cloned.
                    pin_freeze_fallback(&mut state,&shared,&key,&filters,value)
                }else{Ok(None)}}else{Ok(None)};
                match pin{
                    Ok(retired)=>retired_fallback=retired,
                    Err(error)=>{
                        let previous=temporal.and_then(|context|state.freeze_fallbacks.iter().find(|entry|same_fallback_input(&entry.key,context,&key.filters,&filters,key.canvas,&input,blend,providers,&backend,key.mask_epoch,key.mask_stamp,key.resource_generation)&&entry.targets.iter().enumerate().all(|(index,target)|target.is_none_or(|target|state.freeze_timers.run(target)==Some(entry.runs[index])))).map(|entry|entry.value.clone()));
                        retired_value=Some(std::mem::replace(&mut value,previous.ok_or(error)));
                    }
                }
                if let Ok(value)=&value {
                // A passthrough result is the original capture allocation.
                // Memoizing it would defeat weak-key retirement for no work.
                if !std::ptr::eq(input.image(),value.output.as_ref()) {
                while state.entries.len()>=MAX_ENTRIES {let victim=state.entries.iter().min_by_key(|(_,value)|value.touched).map(|(key,_)|key.clone());if let Some(victim)=victim{if let Some(value)=state.entries.remove(&victim){evicted.push(value);}}else{break;}}
                let touched=state.clock;state.entries.insert(key.clone(),Retained {value:value.clone(),touched});
                }
            }}
            state.flights.remove(&key);
        }
        drop(evicted);drop(retired_fallback);drop(retired_value);
        finish(&flight,Reply {value,computed:true});
    }
    // Pending jobs/input Arcs and cache entries retire on this owner.
    drop(receive);
}
impl FilterCache {
    pub(super) fn person_backend_signature(&self)->[u64;3]{Backend::from_cache(self).signature()}
    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_shared(&mut self,filters:&[VideoFilter],key:Key,input:&Arc<RgbaFrame>,blend:SourceBlend,providers:FilterProviders,
        admission:Admission,free_bytes:usize,free_work:usize)->Result<Option<FilterResult>,FilterError> {
        let Some(runtime)=self.shared.clone() else{return Ok(None);};
        let Some((value,computed))=runtime.apply(filters,input,blend,providers,admission,free_bytes,free_work,Backend::from_cache(self),self.mask_epoch,self.resource_generation,self.scroll_context,self.mask_context.clone(),None,self.temporal_context.filter(|context|context.camera&&context.input_complete&&filters.iter().any(|filter|filter.enabled&&filter.person_interval_ms()!=0)),self.camera_scope.clone())? else{return Ok(None);};
        let new_bytes=value.output.byte_len();self.bytes+=new_bytes;self.texel_visits+=admission.texels;
        #[cfg(feature="gpu-filters")]
        {if computed {self.gpu_calls+=value.gpu_calls;}self.gpu_report=value.report.clone();if value.gpu_error.is_some(){self.gpu_last_error=value.gpu_error.clone();}}
        #[cfg(not(feature="gpu-filters"))]
        let _=computed;
        let output=value.output.clone();let blend=value.blend;
        self.entries.insert(key,Entry {_input:input.clone(),output:output.clone(),blend,_shared_lease:Some(value.lease)});
        Ok(Some(FilterResult {image:output,blend,new_bytes,pixel_stages:admission.work}))
    }
    pub(crate) fn camera_fallback(&mut self,source:&Source,input:&Arc<RgbaFrame>,blend:SourceBlend,
        providers:FilterProviders,free_bytes:usize)->Option<FilterResult> {
        let runtime=self.shared.as_ref()?;
        let scope=self.camera_scope.as_ref()?;
        let value=runtime.camera_fallback(scope,source.video_filters(),input,blend,providers,
            &Backend::from_cache(self),self.resource_generation,self.mask_epoch,self.temporal_context,free_bytes)?;
        let output=value.output.clone();
        self.camera_fallback_leases.push(value.lease);
        Some(FilterResult {new_bytes:output.byte_len(),image:output,blend:value.blend,pixel_stages:0})
    }
}

#[cfg(test)]
mod person_fallback_scope_tests {
    use super::*;

    fn scope()->TemporalContext {
        TemporalContext {domain:1,session:2,source:skylive_core::Id::new(3),owner:4,grant:5,
            at:100,tick:6,interval:33,rate:[30,1],config:[7;32],binding:[8;32],
            stamp:temporal::Stamp {capture_epoch:9,sequence:10,at:90,native:[11;32]},
            draft:false,camera:true,input_complete:true,rows:Default::default()}
    }

    #[test]
    fn same_camera_scope_accepts_only_forward_frames_and_identical_configuration() {
        let old=scope();let mut next=old;next.at=200;next.tick=7;
        next.stamp.capture_epoch=10;next.stamp.sequence=11;next.stamp.at=190;
        assert!(same_person_fallback_scope(old,next));
        let mut changed=next;changed.config[0]^=1;assert!(!same_person_fallback_scope(old,changed));
        changed=next;changed.binding[0]^=1;assert!(!same_person_fallback_scope(old,changed));
        changed=next;changed.stamp.native[0]^=1;assert!(!same_person_fallback_scope(old,changed));
        changed=next;changed.session+=1;assert!(!same_person_fallback_scope(old,changed));
        changed=next;changed.source=skylive_core::Id::new(99);assert!(!same_person_fallback_scope(old,changed));
        changed=next;changed.owner+=1;assert!(!same_person_fallback_scope(old,changed));
        changed=next;changed.grant+=1;assert!(!same_person_fallback_scope(old,changed));
        changed=next;changed.input_complete=false;assert!(!same_person_fallback_scope(old,changed));
        changed=next;changed.stamp.sequence=9;assert!(!same_person_fallback_scope(old,changed));
        changed=next;changed.at=99;assert!(!same_person_fallback_scope(old,changed));
    }

    #[test]
    fn person_fallback_excludes_external_and_temporal_stages() {
        let person=VideoFilter::new(VideoEffect::person_background_removal()).unwrap();
        let providers=FilterProviders {canvas:None,craft:true,person:true,freeze:true};
        assert!(person_camera_chain(&[person.clone()],providers));
        let mut disabled=providers;disabled.person=false;
        assert!(!person_camera_chain(&[person.clone()],disabled));
        let mut every_frame=person.clone();every_frame.set_person_interval_ms(0).unwrap();
        assert!(!person_camera_chain(&[every_frame],providers));
        for effect in [VideoEffect::freeze_filter(),VideoEffect::render_delay(),VideoEffect::scroll(),
            VideoEffect::scale_filter(),VideoEffect::apply_lut(),VideoEffect::image_mask_v2()] {
            let filter=VideoFilter::new(effect).unwrap();
            assert!(!person_camera_chain(&[person.clone(),filter],providers));
        }
    }
}
