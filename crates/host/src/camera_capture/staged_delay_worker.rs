//! Bounded sequential execution for one camera's pre-delay filter segments.
//! The capture owner must supply immutable, already-admitted resource snapshots
//! in each operation and reject results from an obsolete worker generation.
//! Camera Runtime submits supported mask/person chains before publication.
use super::async_delay::{Frame,Plan,Scope,SourceDelays,StageTransformError};
use skylive_core::{Id,VideoFilterId,MAX_VIDEO_FILTERS};
use skylive_media::{AsyncDelayBudget,DirectShowClockSnapshot,RgbaFrame,SharedVideoFrame};
use std::{collections::HashMap,sync::{Arc,Mutex,mpsc,atomic::{AtomicBool,AtomicU64,Ordering}},thread,time::Duration};

/// Trusted internal filter code must keep its resource snapshots and any
/// additional allocations within `scratch_bytes`. Input and output pixels are
/// separately charged by their owners; the callback must not retain input.
/// Metadata belongs to the pixels being transformed, which may be an older
/// sample released by a preceding delay rather than the current capture.
/// Animation and temporal effects must use this observation, never the job's
/// newer capture time. References cannot escape the admitted callback.
pub(crate) struct TransformInput<'a> {
    pub image: &'a RgbaFrame,
    pub scope: Scope,
    pub sequence: u64,
    pub timestamp_ns: i128,
    pub qpc_ns: Option<u64>,
    pub directshow_clock: Option<DirectShowClockSnapshot>,
    /// Snapshot for these pixels, including its resource-budget permit.
    pub snapshot: &'a ResourceSnapshot,
    pub ingress: skylive_media::SourceBlend,
}
pub(crate) struct Transform {
    pub ids:Vec<VideoFilterId>,
    pub scratch_bytes:usize,
    pub dimensions:(u32,u32),
    /// Encoding after this segment, as produced by the actual filters.
    pub output_blend:skylive_media::SourceBlend,
    pub apply:Box<dyn for<'a> FnOnce(TransformInput<'a>)->Result<RgbaFrame,String>+Send>,
}
pub(crate) struct Stage {pub plan:Plan,pub before:Option<Transform>}
/// The source owner supplies immutable resources for the whole filter chain.
/// The permit must cover this value and resources retained by `resources`.
pub(crate) struct ResourceSnapshot {
    pub resources: Arc<dyn std::any::Any + Send + Sync>,
    /// Phase resolved for this particular captured frame, with its revocation
    /// guard. The metadata follows these pixels through every delay queue.
    pub scroll: Option<(crate::scroll_animation::Context,Arc<AtomicBool>)>,
    pub revision: u64,
    pub requested_at:skylive_media::MediaTime,
    pub dropped_frames:u64,
    _credit: Arc<skylive_media::AsyncWorkspacePermit>,
}
struct FrameMetadata {
    snapshot:Arc<ResourceSnapshot>,
    blend:skylive_media::SourceBlend,
    processed:Arc<[VideoFilterId]>,
}
pub(crate) struct Sample {
    pub scope:Scope,pub sequence:u64,pub timestamp_ns:i128,
    pub qpc_ns:Option<u64>,pub directshow_clock:Option<DirectShowClockSnapshot>,
    pub requested_at:skylive_media::MediaTime,pub dropped_frames:u64,
    /// The camera resident ledger must remain charged while this wrapper lives.
    pub image:SharedVideoFrame,
    pub ingress:skylive_media::SourceBlend,
    /// Must change whenever an upstream effect or resource snapshot changes.
    pub revision:u64,
    /// Held across queueing and execution; covers immutable request metadata
    /// and resource snapshots owned by this request, excluding capture pixels.
    pub metadata_credit:Arc<skylive_media::AsyncWorkspacePermit>,
    pub scroll:Option<(crate::scroll_animation::Context,Arc<AtomicBool>)>,
    /// Prepared on the sequential worker, never on the capture owner. The
    /// resulting immutable resource object must retain all resource permits.
    pub prepare:Box<dyn FnOnce()->Result<Arc<dyn std::any::Any + Send + Sync>,String>+Send>,
    pub stages:Vec<Stage>,
}
fn attach_luts(resources:&mut crate::video_filters::StagedMaskSet,filters:&[skylive_core::VideoFilter],
    store:&Arc<Mutex<crate::video_filters::StagedLutStore>>,budget:&AsyncDelayBudget,epoch:u64,generation:u64)->Result<(),String> {
    let mut workspace=0usize;
    for asset in filters.iter().filter(|filter|filter.enabled).filter_map(|filter|filter.lut_asset()) {
        let bytes=usize::try_from(asset.encoded_bytes()).map_err(|_|"camera LUT workspace overflow")?
            .checked_add(1).and_then(|bytes|bytes.checked_add(if asset.format()==skylive_core::LutFormat::ObsPng64 {8*1024*1024}else{0}))
            .ok_or("camera LUT workspace overflow")?;
        workspace=workspace.max(bytes);
    }
    if workspace==0{return Ok(());}
    let _workspace=budget.reserve_workspace(workspace).map_err(|_|"camera LUT workspace budget")?;
    let mut store=store.lock().map_err(|_|"camera LUT store poisoned")?;
    let luts=crate::video_filters::StagedLutSet::prepare(&mut store,filters,epoch,generation).map_err(|error|error.detail)?;
    resources.set_luts(luts);Ok(())
}

/// Build the resource factory for a static mask chain. The caller admits the
/// cloned filter metadata in its sample permit before queueing this closure.
pub(crate) fn static_mask_resources(store:Arc<std::sync::Mutex<crate::video_filters::StagedMaskStore>>,
    filters:Vec<skylive_core::VideoFilter>,lut_store:Arc<Mutex<crate::video_filters::StagedLutStore>>,
    budget:AsyncDelayBudget,epoch:u64,generation:u64)
    ->Box<dyn FnOnce()->Result<Arc<dyn std::any::Any + Send + Sync>,String>+Send> {
    Box::new(move || {
        let mut store=store.lock().map_err(|_|"camera staged mask store poisoned".to_string())?;
        let mut resources=crate::video_filters::StagedMaskSet::prepare_static(&mut store,&filters,epoch,generation)
            .map_err(|error|error.detail)?;
        drop(store);
        attach_luts(&mut resources,&filters,&lut_store,&budget,epoch,generation)?;
        Ok(Arc::new(resources))
    })
}

/// Snapshot-specific resource preparation for animated masks. The caller is
/// responsible for supplying the capture sample's exact playback snapshot
/// and constructing the store with that snapshot's shared revision book.
/// This factory executes on the bounded source worker, not the camera owner.
pub(crate) fn timed_mask_resources(store:Arc<std::sync::Mutex<crate::video_filters::StagedMaskStore>>,
    filters:Vec<skylive_core::VideoFilter>,source:Id,clock:crate::mask_animation_clock::Snapshot,
    epoch:u64,generation:u64)
    ->Box<dyn FnOnce()->Result<Arc<dyn std::any::Any + Send + Sync>,String>+Send> {
    Box::new(move || {
        let mut store=store.lock().map_err(|_|"camera staged mask store poisoned".to_string())?;
        let resources=crate::video_filters::StagedMaskSet::prepare_at(&mut store,&filters,source,&clock,epoch,generation)
            .map_err(|error|error.detail)?;
        Ok(Arc::new(resources))
    })
}

/// Prepare an animation only when the entire proven capture-time interval
/// maps to one animation frame. Ambiguous boundaries are reported to the
/// owner so it can skip only that sample without inventing a frame.
pub(crate) fn bounded_mask_resources(store:Arc<std::sync::Mutex<crate::video_filters::StagedMaskStore>>,
    filters:Vec<skylive_core::VideoFilter>,source:Id,clock:crate::mask_animation_clock::Snapshot,
    bounds:skylive_media::MediaTimeBounds,live:Arc<AtomicBool>,lut_store:Arc<Mutex<crate::video_filters::StagedLutStore>>,
    budget:AsyncDelayBudget,epoch:u64,generation:u64)
    ->Box<dyn FnOnce()->Result<Arc<dyn std::any::Any + Send + Sync>,String>+Send> {
    Box::new(move || {
        let mut store=store.lock().map_err(|_|"camera staged mask store poisoned".to_string())?;
        let mut resources=crate::video_filters::StagedMaskSet::prepare_window(&mut store,&filters,source,&clock,bounds,live.clone(),epoch,generation)
            .map_err(|error|if error.detail=="camera mask animation frame is ambiguous in clock bounds" {
                "camera_mask_ambiguous".to_string()
            } else {
                // A revision/decoder failure cannot leave an earlier output
                // authorizing old resources while the owner receives failure.
                live.store(false,Ordering::Release);
                error.detail
            })?;
        drop(store);
        if let Err(error)=attach_luts(&mut resources,&filters,&lut_store,&budget,epoch,generation) {
            live.store(false,Ordering::Release);return Err(error);
        }
        Ok(Arc::new(resources))
    })
}

struct PersonEntry {
    scope:Scope,revision:u64,dimensions:(u32,u32),blend:skylive_media::SourceBlend,
    timestamp_ns:i128, probabilities:Vec<f32>,_credit:skylive_media::AsyncWorkspacePermit,
}
#[derive(Default)]
pub(crate) struct PersonState {entries:HashMap<VideoFilterId,PersonEntry>}
enum ResourceOp {Mask(VideoFilterId),Lut(VideoFilterId),Scale(skylive_media::ScaleFilterPlan),Scroll(skylive_core::VideoFilter),Person {id:VideoFilterId,threshold:f32,softness:f32,interval_ms:u16}}
fn apply_person(input:&RgbaFrame,blend:skylive_media::SourceBlend,scope:Scope,revision:u64,timestamp:i128,
    id:VideoFilterId,threshold:f32,softness:f32,interval_ms:u16,
    state:&Arc<Mutex<PersonState>>,budget:&AsyncDelayBudget)->Result<RgbaFrame,String> {
    let frame_bytes=input.byte_len();
    let visits=(input.width() as u64).checked_mul(input.height() as u64).and_then(|n|n.checked_mul(5))
        .and_then(|n|n.checked_add(5*skylive_media::PERSON_MASK_LEN as u64)).ok_or("person work overflow")?;
    if visits>128*1024*1024 {return Err("person work budget exceeded".into());}
    let limits=skylive_media::PersonMatteLimits {max_input_bytes:frame_bytes.checked_add(skylive_media::PERSON_MASK_LEN*4).ok_or("person input overflow")?,
        max_output_bytes:frame_bytes.max(skylive_media::PERSON_RGB_LEN*4),max_pixel_visits:visits};
    let mut cache=state.lock().map_err(|_|"camera person cache poisoned")?;
    let reuse=interval_ms!=0 && cache.entries.get(&id).is_some_and(|entry|entry.scope==scope&&entry.revision==revision
        && entry.dimensions==(input.width(),input.height())&&entry.blend==blend
        && timestamp.checked_sub(entry.timestamp_ns).is_some_and(|age|age>=0&&age<i128::from(interval_ms)*1_000_000));
    if !reuse {
        // Reserve mask retention before the model allocates its output. The
        // calling stage separately reserves RGB tensor and raster workspace.
        let credit=budget.reserve_workspace(skylive_media::PERSON_MASK_LEN*4).map_err(|_|"camera person mask budget")?;
        let rgb=skylive_media::prepare_person_rgb(input,blend,limits).map_err(|error|error.to_string())?;
        let inference=skylive_person_matte::evaluate_wait(&rgb).map_err(|error|error.to_string())?;
        if inference.probabilities.len()!=skylive_media::PERSON_MASK_LEN {return Err("invalid person matte dimensions".into());}
        if !cache.entries.contains_key(&id) && cache.entries.len()>=MAX_VIDEO_FILTERS {return Err("camera person cache limit".into());}
        cache.entries.try_reserve(1).map_err(|_|"camera person metadata allocation")?;
        cache.entries.insert(id,PersonEntry {scope,revision,dimensions:(input.width(),input.height()),blend,timestamp_ns:timestamp,
            probabilities:inference.probabilities,_credit:credit});
    }
    let entry=cache.entries.get(&id).ok_or("camera person mask unavailable")?;
    skylive_media::apply_person_matte(input,blend,&entry.probabilities,threshold,softness,limits).map_err(|error|error.to_string())
}

/// Build ordered mask and person-matte segments interleaved with delay lanes.
/// Other effects are rejected so they cannot be silently reordered. File preparation is submitted
/// separately via `Sample::prepare` and happens on the same worker.
pub(crate) fn static_mask_stages(plans:&[Plan],upstream:&[skylive_core::VideoFilter],
    dimensions:(u32,u32),person:Arc<Mutex<PersonState>>,budget:AsyncDelayBudget)->Result<Vec<Stage>,String> {
    if plans.is_empty() || plans.len()>MAX_VIDEO_FILTERS || upstream.len()>MAX_VIDEO_FILTERS {
        return Err("invalid camera staged chain".into());
    }
    let _frame_bytes=(dimensions.0 as usize).checked_mul(dimensions.1 as usize)
        .and_then(|v|v.checked_mul(4)).filter(|_|dimensions.0>0 && dimensions.1>0)
        .ok_or("camera staged dimensions invalid")?;
    let mut result=Vec::new();result.try_reserve_exact(plans.len()).map_err(|_|"camera staged plan allocation")?;
    let mut index=0;
    let mut current_dimensions=dimensions;
    let mut chain_blend=skylive_media::SourceBlend::LinearSrgb;
    for (stage_index,plan) in plans.iter().enumerate() {
        let mut ids=Vec::new();ids.try_reserve_exact(upstream.len()).map_err(|_|"camera staged marker allocation")?;
        let mut operations=Vec::new();operations.try_reserve_exact(upstream.len()).map_err(|_|"camera stage operation allocation")?;
        let mut outputs=Vec::new();outputs.try_reserve_exact(upstream.len()).map_err(|_|"camera stage dimension allocation")?;
        let mut scale_work=0u64;
        let mut found_delay=false;
        while index<upstream.len() {
            let filter=&upstream[index];
            if matches!(filter.effect,skylive_core::VideoEffect::AsyncVideoDelay {delay_ms:1..=u16::MAX}) {
                if stage_index+1==plans.len() || filter.id()!=Some(plan.filter) {return Err("camera staged delay order changed".into());}
                index+=1;found_delay=true;break;
            }
            match filter.effect {
                skylive_core::VideoEffect::ImageMask {..}=>if filter.mask_image().is_some(){
                    let id=filter.id().ok_or("camera mask identity missing")?;
                    ids.push(id);operations.push(ResourceOp::Mask(id));outputs.push(current_dimensions);chain_blend=skylive_media::SourceBlend::PremultipliedLinearSrgb;
                },
                skylive_core::VideoEffect::ApplyLut {..}=>if filter.lut_asset().is_some(){
                    let id=filter.id().ok_or("camera LUT identity missing")?;
                    ids.push(id);operations.push(ResourceOp::Lut(id));outputs.push(current_dimensions);chain_blend=skylive_media::SourceBlend::PremultipliedLinearSrgb;
                },
                skylive_core::VideoEffect::ScaleFilter {target:skylive_core::ScaleFilterTarget::Resolution {..},..}=>{
                    let id=filter.id().ok_or("camera scale identity missing")?;
                    let scale=crate::video_filters::staged_scale_plan(filter.effect,current_dimensions,None)
                        .map_err(|error|error.detail)?.ok_or("camera scale plan absent")?;
                    let required=scale.requirements();
                    scale_work=scale_work.checked_add(required.texel_visits).ok_or("camera scale work overflow")?;
                    if scale_work>128*1024*1024{return Err("camera scale work budget exceeded".into());}
                    current_dimensions=(required.width,required.height);
                    ids.push(id);operations.push(ResourceOp::Scale(scale));outputs.push(current_dimensions);
                    chain_blend=skylive_media::SourceBlend::PremultipliedLinearSrgb;
                },
                skylive_core::VideoEffect::Scroll {limit_width,limit_height,width,height,looping,..}=>{
                    let id=filter.id().ok_or("camera scroll identity missing")?;
                    let settings=skylive_media::ScrollSettings {width:limit_width.then_some(width),height:limit_height.then_some(height),looping};
                    let required=skylive_media::scroll_requirements(current_dimensions,settings,skylive_media::ScrollPhase::default()).map_err(|error|error.to_string())?;
                    scale_work=scale_work.checked_add(required.texel_visits).ok_or("camera scroll work overflow")?;
                    if scale_work>128*1024*1024{return Err("camera scroll work budget exceeded".into());}
                    current_dimensions=(required.width,required.height);
                    ids.push(id);operations.push(ResourceOp::Scroll(filter.clone()));outputs.push(current_dimensions);
                    chain_blend=skylive_media::SourceBlend::PremultipliedLinearSrgb;
                },
                skylive_core::VideoEffect::PersonBackgroundRemoval {threshold,softness}=>{
                    let id=filter.id().ok_or("camera person identity missing")?;
                    ids.push(id);operations.push(ResourceOp::Person {id,threshold,softness,interval_ms:filter.person_interval_ms()});outputs.push(current_dimensions);
                },
                _=>return Err("camera staged chain contains an unsupported upstream filter".into()),
            }
            index+=1;
        }
        if stage_index+1<plans.len() && !found_delay {return Err("camera staged delay missing".into());}
        let before=if ids.is_empty(){None}else{
            let tensor=if operations.iter().any(|op|matches!(op,ResourceOp::Person {..})) {skylive_media::PERSON_RGB_LEN*4} else {0};
            let scratch_bytes=outputs.iter().take(outputs.len().saturating_sub(1)).try_fold(tensor,|bytes,(width,height)| {
                (*width as usize).checked_mul(*height as usize).and_then(|pixels|pixels.checked_mul(4)).and_then(|next|bytes.checked_add(next))
            }).ok_or("camera stage scratch overflow")?;
            let person=person.clone();let budget=budget.clone();
            Some(Transform {ids,scratch_bytes,dimensions:current_dimensions,output_blend:chain_blend,
                apply:Box::new(move |input| {
                    let masks=input.snapshot.resources.downcast_ref::<crate::video_filters::StagedMaskSet>()
                        .ok_or("camera staged mask snapshot missing")?;
                    let mut current:Option<RgbaFrame>=None;
                    let mut blend=input.ingress;
                    for operation in operations {
                        let pixels=current.as_ref().unwrap_or(input.image);
                        let next=match operation {
                            ResourceOp::Mask(id)=>{
                                let mask=masks.get(id).ok_or("camera staged mask resource missing")?;
                                let output=mask.apply(pixels,blend).map_err(|error|error.detail)?;
                                blend=skylive_media::SourceBlend::PremultipliedLinearSrgb;output
                            },
                            ResourceOp::Lut(id)=>{
                                let output=masks.lut().ok_or("camera staged LUT snapshot missing")?
                                    .apply(id,pixels,blend).map_err(|error|error.detail)?;
                                blend=skylive_media::SourceBlend::PremultipliedLinearSrgb;output
                            },
                            ResourceOp::Scale(plan)=>{
                                let required=plan.requirements();
                                let visits=required.pixel_visits.checked_add(required.texel_visits).ok_or("camera scale work overflow")?;
                                let output=plan.apply(pixels,blend,skylive_media::CompositeLimits {max_layers:1,
                                    max_input_bytes:required.input_bytes,max_output_bytes:required.output_bytes,max_pixel_visits:visits})
                                    .map_err(|error|error.to_string())?;
                                blend=skylive_media::SourceBlend::PremultipliedLinearSrgb;output
                            },
                            ResourceOp::Scroll(filter)=>{
                                let (context,live)=input.snapshot.scroll.as_ref().ok_or("camera scroll snapshot missing")?;
                                if !live.load(Ordering::Acquire){return Err("camera scroll timeline retired".into());}
                                let skylive_core::VideoEffect::Scroll {limit_width,limit_height,width,height,looping,..}=filter.effect else{return Err("invalid camera scroll".into());};
                                let settings=skylive_media::ScrollSettings {width:limit_width.then_some(width),height:limit_height.then_some(height),looping};
                                let phase=context.phase(&filter);
                                let required=skylive_media::scroll_requirements((pixels.width(),pixels.height()),settings,phase).map_err(|error|error.to_string())?;
                                let output=skylive_media::scroll_rgba(pixels,blend,settings,phase,skylive_media::ScrollLimits {max_output_bytes:required.output_bytes,max_texel_visits:required.texel_visits}).map_err(|error|error.to_string())?;
                                blend=skylive_media::SourceBlend::PremultipliedLinearSrgb;output
                            },
                            ResourceOp::Person {id,threshold,softness,interval_ms}=>apply_person(pixels,blend,input.scope,
                                input.snapshot.revision,input.timestamp_ns,id,threshold,softness,interval_ms,&person,&budget)?,
                        };
                        current=Some(next);
                    }
                    current.ok_or_else(||"empty camera filter stage".to_string())
                })})
        };
        result.push(Stage {plan:*plan,before});
    }
    if index!=upstream.len(){return Err("camera staged filter order changed".into());}
    Ok(result)
}

/// The marker remains coupled to its metadata credit when transferred to a
/// downstream receipt; callers cannot detach a separately cloned ID list.
#[derive(Clone)]
pub(crate) struct ProcessedStages {ids:Arc<[VideoFilterId]>,_credit:Arc<skylive_media::AsyncWorkspacePermit>}
impl ProcessedStages {pub fn ids(&self)->&[VideoFilterId]{&self.ids}}
impl std::fmt::Debug for ProcessedStages {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.debug_tuple("ProcessedStages").field(&self.ids).finish()}
}
pub(crate) struct Output {pub generation:u64,pub frame:Option<Frame>,pub processed:ProcessedStages,pub blend:Option<skylive_media::SourceBlend>,pub requested_at:skylive_media::MediaTime,pub dropped_frames:u64,pub mask_live:Option<Arc<AtomicBool>>,pub mask_resources:Option<Arc<crate::video_filters::StagedMaskSet>>,pub scroll_live:Option<Arc<AtomicBool>>}
pub(crate) enum Event {Output(Output),Skipped {generation:u64},Failed {generation:u64,detail:String}}
struct Job {generation:u64,sample:Sample}
#[derive(PartialEq,Eq)]
struct StageIdentity {plan:Plan,ids:Vec<VideoFilterId>,dimensions:Option<(u32,u32)>,blend:Option<skylive_media::SourceBlend>}
impl StageIdentity {
    fn matches(&self,stage:&Stage)->bool {
        self.plan==stage.plan && match (&stage.before,self.dimensions,self.blend) {
            (None,None,None)=>self.ids.is_empty(),
            (Some(op),Some(dimensions),Some(blend))=>self.ids==op.ids && dimensions==op.dimensions && blend==op.output_blend,
            _=>false,
        }
    }
    fn capture(stage:&Stage)->Result<Self,String> {
        let mut ids=Vec::new();
        if let Some(op)=&stage.before {
            ids.try_reserve_exact(op.ids.len()).map_err(|_|"camera stage identity allocation")?;
            ids.extend_from_slice(&op.ids);
        }
        Ok(Self {plan:stage.plan,ids,dimensions:stage.before.as_ref().map(|op|op.dimensions),
            blend:stage.before.as_ref().map(|op|op.output_blend)})
    }
}

/// A single-slot input and output keep retained camera frames bounded. A full
/// input queue drops the new sample without discarding the running inference;
/// output overflow retires its generation and starts new history.
/// Dropping the owner cancels publication without blocking the camera thread;
/// any in-flight work retains its own permits until it actually returns.
pub(crate) struct Worker {
    source:Id, sender:Option<mpsc::SyncSender<Job>>,receiver:mpsc::Receiver<Event>,
    generation:Arc<AtomicU64>,cancel:Arc<AtomicBool>,person:Arc<Mutex<PersonState>>,
}
impl Worker {
    pub fn new(source:Id,budget:AsyncDelayBudget)->std::io::Result<Self>{
        let (sender,input)=mpsc::sync_channel::<Job>(1);
        let (output,receiver)=mpsc::sync_channel::<Event>(1);
        let generation=Arc::new(AtomicU64::new(1));let cancel=Arc::new(AtomicBool::new(false));
        let live=generation.clone();let stopped=cancel.clone();
        let person=Arc::new(Mutex::new(PersonState::default()));let worker_person=person.clone();
        thread::Builder::new().name(format!("camera-stages-{}",source.get())).spawn(move||run(source,budget,input,output,live,stopped,worker_person))?;
        Ok(Self {source,sender:Some(sender),receiver,generation,cancel,person})
    }
    pub fn person(&self)->Arc<Mutex<PersonState>>{self.person.clone()}
    pub fn generation(&self)->u64{self.generation.load(Ordering::Acquire)}
    pub fn generation_token(&self)->Arc<AtomicU64>{self.generation.clone()}
    fn retire(&self)->Result<(), &'static str>{
        self.generation.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1))
            .map(|_|()).map_err(|_|"camera stage generation exhausted")
    }
    pub fn try_submit(&self,sample:Sample)->Result<(), &'static str>{
        if self.cancel.load(Ordering::Acquire) || sample.scope.source!=self.source{return Err("camera stage owner is closed or source changed");}
        if sample.revision==0 || sample.stages.is_empty() || sample.stages.len()>MAX_VIDEO_FILTERS{return Err("invalid camera stage plan");}
        let sender=self.sender.as_ref().ok_or("camera stage owner is closed")?;
        match sender.try_send(Job {generation:self.generation(),sample}) {
            Ok(())=>Ok(()),
            Err(mpsc::TrySendError::Full(_))=>Err("camera stage input queue is full"),
            Err(mpsc::TrySendError::Disconnected(_))=>{self.retire()?;Err("camera stage worker disconnected")},
        }
    }
    pub fn try_recv(&self)->Option<Event>{
        loop {let event=self.receiver.try_recv().ok()?;
            let generation=match &event {Event::Output(output)=>output.generation,Event::Skipped {generation}|Event::Failed {generation,..}=>*generation};
            if generation==self.generation(){return Some(event);}
        }
    }
    pub fn reset(&self)->Result<(), &'static str>{self.retire()}
}
impl Drop for Worker {fn drop(&mut self){self.cancel.store(true,Ordering::Release);let _=self.retire();self.sender.take();}}

fn run(source:Id,budget:AsyncDelayBudget,input:mpsc::Receiver<Job>,output:mpsc::SyncSender<Event>,
    live:Arc<AtomicU64>,stopped:Arc<AtomicBool>,person:Arc<Mutex<PersonState>>) {
    let _model_owner=skylive_person_matte::retain_background_owner().ok();
    let mut delays=SourceDelays::new(budget.clone());
    let mut applied_generation=0;let mut revision=0;let mut last_sequence=0;let mut active_plans:Vec<StageIdentity>=Vec::new();
    let mut plan_credit:Option<Arc<skylive_media::AsyncWorkspacePermit>>=None;
    while !stopped.load(Ordering::Acquire) {
        let job=match input.recv_timeout(Duration::from_millis(50)){
            Ok(job)=>job,Err(mpsc::RecvTimeoutError::Timeout)=>continue,Err(mpsc::RecvTimeoutError::Disconnected)=>break,
        };
        if job.generation!=live.load(Ordering::Acquire){continue;}
        let Sample {scope,sequence,timestamp_ns,qpc_ns,directshow_clock,requested_at,dropped_frames,image,ingress,revision:next_revision,metadata_credit,scroll,prepare,stages}=job.sample;
        if scope.source!=source {continue;}
        let changed_generation=applied_generation!=job.generation;
        if changed_generation {
            delays.reset_source(source);if let Ok(mut state)=person.lock(){state.entries.clear();}
            applied_generation=job.generation;revision=next_revision;last_sequence=0;active_plans=Vec::new();plan_credit=None;
        }
        // A source/resource revision must retire all earlier output tokens.
        // Never reset an occupied queue silently under the same generation.
        let compatible=revision==next_revision && (active_plans.is_empty()
            || active_plans.len()==stages.len() && active_plans.iter().zip(&stages).all(|(a,b)|a.matches(b)));
        let outcome=if !compatible {Err("camera stage plan changed without generation reset".into())} else if active_plans.is_empty() && active_plans.try_reserve_exact(stages.len()).is_err() {
            Err("camera stage plan allocation failed".into())
        } else {
            let identities=if active_plans.is_empty() {
                let mut identities=Vec::new();
                if identities.try_reserve_exact(stages.len()).is_err() {Err("camera stage identity allocation".to_string())}
                else {
                    let mut result=Ok(());
                    for stage in &stages {
                        match StageIdentity::capture(stage) {Ok(identity)=>identities.push(identity),Err(error)=>{result=Err(error);break;}}
                    }
                    result.map(|()|identities)
                }
            } else {Ok(Vec::new())};
            match identities {
                Err(error)=>Err(error),
                Ok(identities)=>{
                    if active_plans.is_empty(){active_plans=identities;plan_credit=Some(metadata_credit.clone());}
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let resources=prepare()?;
                        let snapshot=Arc::new(ResourceSnapshot {resources,scroll,revision:next_revision,requested_at,dropped_frames,_credit:metadata_credit.clone()});
                        execute(&mut delays,&budget,scope,sequence,timestamp_ns,qpc_ns,directshow_clock,&image,snapshot,ingress,stages,last_sequence)
                    })).unwrap_or_else(|_|Err("camera stage worker panicked".into()))
                }
            }
        };
        last_sequence=sequence;
        if stopped.load(Ordering::Acquire)||job.generation!=live.load(Ordering::Acquire){delays.reset_source(source);continue;}
        let event=match outcome {
            Ok((frame,processed,blend,credit,requested_at,dropped_frames,mask_live,mask_resources,scroll_live))=>Event::Output(Output {generation:job.generation,frame,processed:ProcessedStages {ids:processed,_credit:credit},blend,requested_at,dropped_frames,mask_live,mask_resources,scroll_live}),
            Err(detail) if detail=="camera_mask_ambiguous"=>Event::Skipped {generation:job.generation},
            Err(detail)=>{
                delays.reset_source(source);
                let Ok(old)=live.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)) else {break;};
                Event::Failed {generation:old+1,detail}
            },
        };
        if output.try_send(event).is_err(){
            delays.reset_source(source);
            if live.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(1)).is_err(){break;}
        }
    }
    delays.reset_source(source);
    drop(plan_credit);
}

fn execute(delays:&mut SourceDelays,budget:&AsyncDelayBudget,scope:Scope,sequence:u64,timestamp_ns:i128,
    qpc_ns:Option<u64>,clock:Option<DirectShowClockSnapshot>,image:&SharedVideoFrame,snapshot:Arc<ResourceSnapshot>,ingress:skylive_media::SourceBlend,stages:Vec<Stage>,last:u64)
    ->Result<(Option<Frame>,Arc<[VideoFilterId]>,Option<skylive_media::SourceBlend>,Arc<skylive_media::AsyncWorkspacePermit>,skylive_media::MediaTime,u64,Option<Arc<AtomicBool>>,Option<Arc<crate::video_filters::StagedMaskSet>>,Option<Arc<AtomicBool>>),String>{
    if sequence==0 || sequence<=last || stages.is_empty() || stages.len()>MAX_VIDEO_FILTERS{return Err("stale camera stage sequence or plan".into());}
    let mut plans=Vec::new();let mut processed=Vec::new();
    plans.try_reserve_exact(stages.len()).map_err(|_|"camera stage plan allocation")?;
    processed.try_reserve_exact(MAX_VIDEO_FILTERS).map_err(|_|"camera stage marker allocation")?;
    for stage in &stages {
        if stage.plan.scope!=scope{return Err("camera stage scope changed".into());}
        plans.push(stage.plan);
        if let Some(op)=&stage.before {
            if op.ids.len()>MAX_VIDEO_FILTERS || processed.len()+op.ids.len()>MAX_VIDEO_FILTERS{return Err("camera stage marker limit".into());}
            for &id in &op.ids {if processed.contains(&id){return Err("duplicate camera stage marker".into());}processed.push(id);}
        }
    }
    delays.reconcile(&plans).map_err(|error|format!("camera stage plan: {error:?}"))?;
    let empty:Arc<[VideoFilterId]>=Arc::from([]);
    let mut frame:Option<Frame>=None;
    for (index,stage) in stages.into_iter().enumerate() {
        let output=match (index,frame.take(),stage.before) {
            (0,_,Some(op))=>{
                let _scratch=budget.reserve_workspace(op.scratch_bytes).map_err(|error|format!("camera stage scratch: {error:?}"))?;
                let meta=Arc::new(FrameMetadata {snapshot:snapshot.clone(),blend:op.output_blend,processed:op.ids.clone().into()});
                delays.push_transformed_capture_with_context(stage.plan.filter,scope,sequence,timestamp_ns,qpc_ns,clock,
                    Some(meta),image,op.dimensions,|image| {
                    (op.apply)(TransformInput {image,scope,sequence,timestamp_ns,qpc_ns,directshow_clock:clock,snapshot:&snapshot,ingress})
                })
                    .map_err(stage_error)?
            },
            (0,_,None)=>delays.push_transformed_capture_with_context(stage.plan.filter,scope,sequence,timestamp_ns,qpc_ns,clock,
                Some(Arc::new(FrameMetadata {snapshot:snapshot.clone(),blend:ingress,processed:empty.clone()})),image,(image.width(),image.height()),|image| {
                    let mut pixels=Vec::new();
                    pixels.try_reserve_exact(image.byte_len()).map_err(|_|"camera stage pixel allocation".to_string())?;
                    pixels.extend_from_slice(image.pixels());
                    RgbaFrame::new(image.width(),image.height(),pixels).map_err(|_|"invalid camera stage pixels".to_string())
                }).map_err(stage_error)?,
            (_,Some(previous),Some(op))=>{
                let _scratch=budget.reserve_workspace(op.scratch_bytes).map_err(|error|format!("camera stage scratch: {error:?}"))?;
                let old_sequence=previous.sequence().ok_or("camera delayed sequence absent")?;
                let old_timestamp=previous.timestamp_ns();
                let old_qpc=previous.device_qpc_ns();
                let old_clock=previous.directshow_clock();
                let old_meta=previous.context().and_then(|context|context.downcast_ref::<FrameMetadata>())
                    .ok_or("camera delayed resource snapshot missing")?;
                if old_meta.snapshot.revision!=snapshot.revision {return Err("camera delayed resource revision changed".into());}
                let mut ids=Vec::new();
                ids.try_reserve_exact(old_meta.processed.len()+op.ids.len()).map_err(|_|"camera stage marker allocation")?;
                ids.extend_from_slice(&old_meta.processed);
                ids.extend_from_slice(&op.ids);
                let next_meta=Arc::new(FrameMetadata {snapshot:old_meta.snapshot.clone(),blend:op.output_blend,processed:ids.into()});
                delays.push_transformed_with_context(stage.plan.filter,&previous,Some(next_meta),op.dimensions,|image| {
                    (op.apply)(TransformInput {image,scope,sequence:old_sequence,timestamp_ns:old_timestamp,qpc_ns:old_qpc,directshow_clock:old_clock,snapshot:&old_meta.snapshot,ingress:old_meta.blend})
                }).map_err(stage_error)?
            },
            (_,Some(previous),None)=>{
                // A pass-through delay must keep the released sample's
                // provenance, filter markers and resource/scroll guards.
                let meta=previous.context().and_then(|context|context.downcast_ref::<FrameMetadata>())
                    .ok_or("camera delayed resource snapshot missing")?;
                let context=Arc::new(FrameMetadata {snapshot:meta.snapshot.clone(),blend:meta.blend,processed:meta.processed.clone()});
                let dimensions=(previous.shared_image().width(),previous.shared_image().height());
                delays.push_transformed_with_context(stage.plan.filter,&previous,Some(context),dimensions,|image| {
                    let mut pixels=Vec::new();
                    pixels.try_reserve_exact(image.byte_len()).map_err(|_|"camera stage pixel allocation".to_string())?;
                    pixels.extend_from_slice(image.pixels());
                    RgbaFrame::new(image.width(),image.height(),pixels).map_err(|_|"invalid camera stage pixels".to_string())
                }).map_err(stage_error)?
            },
            (_,None,_)=>return Ok((None,empty,None,snapshot._credit.clone(),snapshot.requested_at,snapshot.dropped_frames,None,None,None)),
        };
        frame=output;
    }
    if let Some(ref released)=frame {
        let meta=released.context().and_then(|context|context.downcast_ref::<FrameMetadata>())
            .ok_or("camera delayed output metadata missing")?;
        let masks=meta.snapshot.resources.clone().downcast::<crate::video_filters::StagedMaskSet>()
            .map_err(|_|"camera staged mask snapshot missing")?;
        let live=masks.live();
        if live.as_ref().is_some_and(|token|!token.load(Ordering::Acquire)) {return Err("camera mask timeline retired".into());}
        let scroll_live=meta.snapshot.scroll.as_ref().map(|(_,live)|live.clone());
        if scroll_live.as_ref().is_some_and(|token|!token.load(Ordering::Acquire)) {return Err("camera scroll timeline retired".into());}
        // A temporarily busy revision owner cannot justify a frame; discard
        // this sample and keep the bounded history for a later attempt.
        if !masks.current(){return Err("camera_mask_ambiguous".into());}
        let resources=live.as_ref().map(|_|masks.clone());
        Ok((frame.clone(),meta.processed.clone(),Some(meta.blend),meta.snapshot._credit.clone(),meta.snapshot.requested_at,meta.snapshot.dropped_frames,live,resources,scroll_live))
    } else {Ok((None,empty,None,snapshot._credit.clone(),snapshot.requested_at,snapshot.dropped_frames,None,None,None))}
}
fn stage_error(error:StageTransformError<String>)->String{match error {
    StageTransformError::Source(error)=>format!("camera delay: {error:?}"),StageTransformError::Transform(error)=>error,
}}
