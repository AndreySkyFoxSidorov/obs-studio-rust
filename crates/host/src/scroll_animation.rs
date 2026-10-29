//! Source/filter-owned normalized Scroll phase. Only explicit media time and
//! already-owned metadata enter here; no wallclock, native calls or pixel reads.
use crate::{capture::CaptureSnapshot,transition::{FrameScene,SceneKey}};
use skylive_core::{Id,Source,StudioSnapshot,VideoEffect,MAX_SOURCES,MAX_VIDEO_FILTERS};
use skylive_media::{MediaTime,ScrollPhase};
use std::{collections::{HashMap,VecDeque},sync::{Arc,atomic::{AtomicU64,Ordering}}};
const LIMIT:usize=3*MAX_SOURCES*MAX_VIDEO_FILTERS;
// A short, bounded history is kept only by the playback clock. It is not a
// substitute for correlating camera QPC with media time or a pixel receipt.
const HISTORY_BYTES:usize=8*1024*1024;
const HISTORY_SEGMENTS:usize=8;
static NEXT:AtomicU64=AtomicU64::new(1);
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]
struct Key {source:Id,owner:u64,filter:Option<u64>,slot:u8}
#[derive(Clone,Copy,Debug,PartialEq)]
struct Settings {speed:[f32;2],looping:bool}
fn settings(effect:VideoEffect)->Option<Settings>{if let VideoEffect::Scroll {speed_x,speed_y,looping,..}=effect{Some(Settings {speed:[speed_x,speed_y],looping})}else{None}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
struct Stamp {epoch:u64,sequence:u64,at:MediaTime}
fn stale(candidate:Stamp,old:Stamp)->bool{candidate.at<old.at||candidate.epoch<old.epoch||candidate.epoch==old.epoch&&candidate.sequence<old.sequence}
#[derive(Clone,Copy,Debug)]
struct Plan {settings:Settings,dimensions:Option<(u32,u32)>,enabled:bool,stamp:Option<Stamp>}
#[derive(Clone,Copy,Debug)]
struct Lane {phase:[f32;2],settings:Settings,dimensions:Option<(u32,u32)>,enabled:bool,stamp:Option<Stamp>}
#[derive(Clone,Debug,PartialEq,Eq)]
pub enum Error {Limit,IdentityExhausted,ForeignCapture,ConflictingSource,InvalidSource(String)}
impl std::fmt::Display for Error {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{write!(f,"Scroll clock: {self:?}")}}
impl std::error::Error for Error{}
/// Collision-free identity for every authored filter slot. Header and mask are
/// separate so adding slots cannot overlap a phase or truncate its identity.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,serde::Serialize)]
pub(crate) struct ScrollKey {header:[u64;5],phases:[u64;MAX_VIDEO_FILTERS],mask:u64}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub(crate) struct Context {pub key:ScrollKey,phases:[[u32;2];MAX_VIDEO_FILTERS],filters:[u64;MAX_VIDEO_FILTERS],pub moving:bool}
impl Context {pub fn phase(self,filter:&skylive_core::VideoFilter)->ScrollPhase{let Some(id)=filter.id()else{return ScrollPhase::default();};self.filters.iter().position(|found|*found==id.get()).map_or(ScrollPhase::default(),|index|ScrollPhase {x:f32::from_bits(self.phases[index][0]),y:f32::from_bits(self.phases[index][1])})}}
#[derive(Clone,Debug,Default)]
pub struct Snapshot {session:u64,clock:u64,epoch:u64,lanes:Arc<HashMap<Key,Lane>>}
impl Snapshot {
    pub(crate) fn same_origin(&self,old:&Self)->bool{self.lanes.is_empty()&&old.lanes.is_empty()||self.session==old.session&&self.clock==old.clock&&self.epoch==old.epoch}
    pub(crate) fn bytes(&self)->usize{std::mem::size_of_val(&*self.lanes)+2*std::mem::size_of::<usize>()+(self.lanes.capacity()+1).saturating_mul(2).saturating_mul(std::mem::size_of::<(Key,Lane)>()+1)}
    pub(crate) fn context(&self,source:&Source,owner:u64)->Context{
        if !source.video_filters().iter().any(|f|f.enabled&&matches!(f.effect,VideoEffect::Scroll{..})){return Context::default();}
        let mut context=Context::default();context.key.header=[self.clock,self.epoch,self.session,source.id().get(),owner];
        for(index,filter)in source.video_filters().iter().enumerate(){if !filter.enabled{continue;}let Some(current)=settings(filter.effect)else{continue;};let key=Key {source:source.id(),owner,filter:filter.id().map(|id|id.get()),slot:if filter.id().is_some(){0}else{index as u8}};
            let lane=self.lanes.get(&key).or_else(||self.lanes.get(&Key {owner:0,..key}));
            let phase=lane.filter(|lane|lane.settings==current).map_or([0.0;2],|lane|lane.phase);
            context.filters[index]=filter.id().map_or(0,|id|id.get());context.phases[index]=phase.map(f32::to_bits);context.key.phases[index]=u64::from(context.phases[index][0])|(u64::from(context.phases[index][1])<<32);context.key.mask|=1u64<<index;
            context.moving|=(0..2).any(|i|current.speed[i]!=0.0&&(current.looping||current.speed[i]<0.0||phase[i]<1.0));
        }context
    }
    /// Explicit cold timeline from time0 for headless rendering. Canonical
    /// dimensions seed the sampler; no implicit clock/native observation.
    pub(crate) fn cold(session:u64,model:&StudioSnapshot,now:MediaTime)->Self{
        let mut lanes=HashMap::new();for source in model.sources(){let mut dimensions=model.source_video_dimensions(source.kind()).ok().flatten();for(index,filter)in source.video_filters().iter().enumerate(){
            if let Some(settings)=settings(filter.effect){let mut lane=Lane {phase:[0.0;2],settings,dimensions:if filter.enabled{dimensions}else{None},enabled:filter.enabled,stamp:None};tick(&mut lane,now.0);if lanes.len()<LIMIT{lanes.insert(Key {source:source.id(),owner:0,filter:filter.id().map(|id|id.get()),slot:if filter.id().is_some(){0}else{index as u8}},lane);}}
            if filter.enabled{dimensions=dimensions.and_then(|d|filter.effect.output_dimensions_with_canvas(d,Some((model.video().width(),model.video().height()))).ok());}
        }}Self {session,clock:0,epoch:0,lanes:Arc::new(lanes)}
    }
}
#[derive(Clone)]
struct Segment {session:u64,epoch:u64,start:MediaTime,end:MediaTime,lanes:Arc<HashMap<Key,Lane>>,bytes:usize}
/// Immutable, bounded history for a capture owner. Retaining this value does
/// not itself establish that a camera frame belongs to the history: callers
/// must bind session, playback epoch, source revision and QPC correlation to
/// the frame and revoke that binding when any of them changes.
#[derive(Clone)]
pub(crate) struct HistorySnapshot {identity:u64,segments:VecDeque<Segment>,bytes:usize}
impl HistorySnapshot {
    pub(crate) fn bytes(&self)->usize{self.bytes}
    pub(crate) fn same_origin(&self,other:&Self)->bool{self.identity==other.identity}
    /// Return endpoint phases only when the *entire* inclusive uncertainty
    /// interval lies within one unchanged observation segment. Endpoints do
    /// not prove that the interval renders identically: looping can cross a
    /// boundary in between. Consumers must preserve that uncertainty.
    pub(crate) fn window(&self,session:u64,epoch:u64,source:&Source,owner:u64,
        bounds:skylive_media::MediaTimeBounds)->Option<(Context,Context)> {
        if bounds.earliest>bounds.latest{return None;}
        let segment=self.segments.iter().rev().find(|s|s.session==session&&s.epoch==epoch
            &&s.start<=bounds.earliest&&bounds.latest<=s.end)?;
        Some((segment_context(segment,self.identity,source,owner,bounds.earliest)?,
              segment_context(segment,self.identity,source,owner,bounds.latest)?))
    }
    /// Return a phase only if it is constant for the *whole* uncertainty
    /// interval, not merely equal at its endpoints. A loop can complete any
    /// number of revolutions between equal endpoints. A non-looping positive
    /// lane is constant once clamped at one; negative lanes are never assumed
    /// clamped. An exact instant is safe even when the phase is moving.
    pub(crate) fn constant_window(&self,session:u64,epoch:u64,source:&Source,owner:u64,
        bounds:skylive_media::MediaTimeBounds)->Option<Context> {
        self.constant_window_filters(session,epoch,source.id(),source.video_filters(),owner,bounds)
    }
    /// Resolve an admitted source's filter snapshot without retaining an entire
    /// authored Source in the capture worker. The caller owns revision and
    /// timeline guards; this lookup alone does not prove frame provenance.
    pub(crate) fn constant_window_filters(&self,session:u64,epoch:u64,source:Id,filters:&[skylive_core::VideoFilter],owner:u64,
        bounds:skylive_media::MediaTimeBounds)->Option<Context> {
        if filters.len()>MAX_VIDEO_FILTERS{return None;}
        if bounds.earliest>bounds.latest{return None;}
        let segment=self.segments.iter().rev().find(|s|s.session==session&&s.epoch==epoch
            &&s.start<=bounds.earliest&&bounds.latest<=s.end)?;
        let context=segment_context_filters(segment,self.identity,source,filters,owner,bounds.earliest)?;
        if bounds.earliest==bounds.latest{return Some(context);}
        for(index,filter) in filters.iter().enumerate(){
            if !filter.enabled {continue;}let Some(current)=settings(filter.effect)else{continue;};
            let id=filter.id()?;
            let key=Key {source,owner,filter:Some(id.get()),slot:0};
            let lane=segment.lanes.get(&key).or_else(||segment.lanes.get(&Key {owner:0,..key}))?;
            let dims=lane.dimensions?;
            let phases=context.phases[index];
            for axis in 0..2 {
                let dimension=if axis==0{dims.0}else{dims.1};
                let speed=current.speed[axis];
                if dimension!=0 && speed!=0.0 && !(speed>0.0&&!current.looping&&f32::from_bits(phases[axis])>=1.0) {return None;}
            }
        }
        Some(context)
    }
    /// Camera timestamps may be an interval rather than a precise instant.
    /// Use its midpoint only within a single unchanged segment and a bounded
    /// 10 ms interval. The phase then has at most 5 ms timing error; callers
    /// must present it as an estimate and retain the epoch/revision guard.
    pub(crate) fn estimated_window_filters(&self,session:u64,epoch:u64,source:Id,filters:&[skylive_core::VideoFilter],owner:u64,
        bounds:skylive_media::MediaTimeBounds)->Option<Context> {
        if filters.len()>MAX_VIDEO_FILTERS{return None;}
        let span=bounds.latest.0.checked_sub(bounds.earliest.0)?;
        if span>10_000_000{return None;}
        let segment=self.segments.iter().rev().find(|s|s.session==session&&s.epoch==epoch
            &&s.start<=bounds.earliest&&bounds.latest<=s.end)?;
        let midpoint=MediaTime(bounds.earliest.0.checked_add(span/2)?);
        segment_context_filters(segment,self.identity,source,filters,owner,midpoint)
    }
}
#[derive(Clone,Default)]
pub struct Clock {session:Option<u64>,last:Option<MediaTime>,identity:u64,epoch:u64,lanes:HashMap<Key,Lane>,
    last_lanes:Arc<HashMap<Key,Lane>>,history:VecDeque<Segment>,history_bytes:usize}
fn tick(lane:&mut Lane,delta:u64){
    let dimensions=lane.dimensions.map_or([0,0],|d|[d.0,d.1]);let seconds=(delta as f64/1_000_000_000.0) as f32;
    for i in 0..2{if dimensions[i]!=0{lane.phase[i]+=(1.0/dimensions[i] as f32)*lane.settings.speed[i]*seconds;}if lane.phase[i]>1.0{if lane.settings.looping{lane.phase[i]-=1.0;}else{lane.phase[i]=1.0;}}}
}
fn observe(plans:&mut HashMap<Key,Plan>,source:&Source,owner:u64,mut dimensions:Option<(u32,u32)>,stamp:Option<Stamp>,canvas:(u32,u32))->Result<(),Error>{
    for(index,filter)in source.video_filters().iter().enumerate(){
        if let Some(settings)=settings(filter.effect){filter.validate().map_err(|error|Error::InvalidSource(error.to_string()))?;let key=Key {source:source.id(),owner,filter:filter.id().map(|id|id.get()),slot:if filter.id().is_some(){0}else{index as u8}};let plan=Plan {settings,dimensions,enabled:filter.enabled,stamp};
            if let Some(old)=plans.get(&key){if old.settings!=settings||old.dimensions!=dimensions||old.enabled!=filter.enabled{return Err(Error::ConflictingSource);}}
            else{if plans.len()>=LIMIT{return Err(Error::Limit);}plans.try_reserve(1).map_err(|_|Error::Limit)?;plans.insert(key,plan);}
        }
        if filter.enabled{dimensions=dimensions.map(|d|filter.effect.output_dimensions_with_canvas(d,Some(canvas))).transpose().map_err(|error|Error::InvalidSource(error.to_string()))?;}
    }Ok(())
}
pub(crate) fn root_owner(view:&FrameScene)->u64{match view.key(){SceneKey::Live(_)=>0,SceneKey::Frozen {instance,..}=>instance}}
/// Frozen input may depend on a nested scene/group's captured canvas even
/// without a direct Canvas filter. Scope private render timelines conservatively;
/// live shared leaf sources retain owner0. Existing three-context cap remains.
pub(crate) fn source_owner(view:&FrameScene,_source:&Source)->u64{root_owner(view)}
fn segment_context(segment:&Segment,identity:u64,source:&Source,owner:u64,at:MediaTime)->Option<Context>{
    segment_context_filters(segment,identity,source.id(),source.video_filters(),owner,at)
}
fn segment_context_filters(segment:&Segment,identity:u64,source:Id,filters:&[skylive_core::VideoFilter],owner:u64,at:MediaTime)->Option<Context>{
    if filters.len()>MAX_VIDEO_FILTERS{return None;}
    let mut context=Context::default();context.key.header=[identity,segment.epoch,segment.session,source.get(),owner];
    for(index,filter) in filters.iter().enumerate(){
        if !filter.enabled{continue;}let Some(current)=settings(filter.effect)else{continue;};
        let id=filter.id()?;
        let key=Key {source,owner,filter:Some(id.get()),slot:0};
        let mut lane=*segment.lanes.get(&key).or_else(||segment.lanes.get(&Key {owner:0,..key}))?;
        if !lane.enabled||lane.settings!=current||lane.dimensions.is_none(){return None;}
        tick(&mut lane,at.0.checked_sub(segment.start.0)?);
        context.filters[index]=id.get();context.phases[index]=lane.phase.map(f32::to_bits);
        context.key.phases[index]=u64::from(context.phases[index][0])|(u64::from(context.phases[index][1])<<32);
        context.key.mask|=1u64<<index;
        context.moving|=(0..2).any(|i|current.speed[i]!=0.0&&(current.looping||current.speed[i]<0.0||lane.phase[i]<1.0));
    }Some(context)
}
impl Clock {
    pub fn reset(&mut self){self.last=None;self.lanes.clear();self.last_lanes=Arc::new(HashMap::new());self.history.clear();self.history_bytes=0;}
    /// Read an exact media-time phase from an unchanged, retained interval.
    /// A caller must independently prove the session, epoch, source revision,
    /// timestamp correlation and lifetime of the frame before using it.
    /// There is deliberately no interpolation across a settings/size change.
    pub(crate) fn historical_context(&self,session:u64,epoch:u64,source:&Source,owner:u64,at:MediaTime)->Option<Context>{
        let segment=self.history.iter().rev().find(|s|s.session==session&&s.epoch==epoch&&s.start<=at&&at<=s.end)?;
        segment_context(segment,self.identity,source,owner,at)
    }
    /// Copies only bounded metadata; lane tables remain shared. Capture owners
    /// must account `bytes()` for every retained snapshot and bound their count.
    pub(crate) fn history_snapshot(&self)->Result<HistorySnapshot,Error>{
        let mut segments=VecDeque::new();segments.try_reserve(self.history.len()).map_err(|_|Error::Limit)?;
        segments.extend(self.history.iter().cloned());
        let bytes=self.history_bytes.checked_add(segments.capacity().checked_mul(std::mem::size_of::<Segment>()).ok_or(Error::Limit)?)
            .and_then(|bytes|bytes.checked_add(std::mem::size_of::<HistorySnapshot>()))
            .ok_or(Error::Limit)?;
        Ok(HistorySnapshot {identity:self.identity,segments,bytes})
    }
    /// Refuse a history detached from the exact published scroll clock. Playback
    /// and scroll epochs are independent and must never be interchanged.
    pub(crate) fn history_for(&self,sample:&Snapshot)->Result<Option<(u64,HistorySnapshot)>,Error>{
        if self.session!=Some(sample.session)||self.identity!=sample.clock||self.epoch!=sample.epoch||self.history.is_empty(){return Ok(None);}
        Ok(Some((self.epoch,self.history_snapshot()?)))
    }
    /// Transactional bounded metadata observation. Retained normalized offsets
    /// advance with LAST upstream dimensions, never retroactive new-size division.
    pub fn sample(&mut self,session:u64,model:&StudioSnapshot,views:&[FrameScene],now:MediaTime,capture:Option<&CaptureSnapshot>)->Result<Snapshot,Error>{
        if capture.is_some_and(|capture|capture.session()!=session){return Err(Error::ForeignCapture);}
        if views.len()>skylive_core::MAX_SCENES{return Err(Error::Limit);}
        let mut plans=HashMap::new();for view in views{
            view.validate_session(session).map_err(|error|Error::InvalidSource(error.to_string()))?;
            if let Some(source)=view.root_scene_source(model){observe(&mut plans,source,root_owner(view),Some(view.filter_canvas_dimensions(model)),None,view.filter_canvas_dimensions(model))?;}
            for source in view.visible_sources(model).map_err(|error|Error::InvalidSource(error.to_string()))?{
                let actual=capture.and_then(|capture|capture.frame(source.id())).filter(|frame|frame.requested_at<=now&&frame.binding.matches_source(source.kind())).map(|frame|((frame.image.width(),frame.image.height()),Stamp {epoch:capture.unwrap().epoch(),sequence:frame.sequence,at:frame.requested_at}));
                let stamp=actual.map(|(_,stamp)|stamp);let dimensions=actual.map(|(dimensions,_)|dimensions).or(view.source_video_dimensions(model,source).map_err(|error|Error::InvalidSource(error.to_string()))?);observe(&mut plans,source,source_owner(view,source),dimensions,stamp,view.filter_canvas_dimensions(model))?;
            }
        }
        let reset=self.session!=Some(session)||self.last.is_none_or(|last|now<last);
        let changed=reset||plans.len()!=self.lanes.len()||plans.iter().any(|(key,plan)|self.lanes.get(key).is_none_or(|old|old.settings!=plan.settings||old.dimensions!=plan.dimensions||old.enabled!=plan.enabled));
        let epoch=if changed{self.epoch.checked_add(1).ok_or(Error::IdentityExhausted)?}else{self.epoch};
        let identity=if self.identity==0&&!plans.is_empty(){NEXT.fetch_update(Ordering::AcqRel,Ordering::Acquire,|value|value.checked_add(1)).map_err(|_|Error::IdentityExhausted)?}else{self.identity};
        let delta=if reset{0}else{now.0-self.last.unwrap().0};let mut lanes=HashMap::new();lanes.try_reserve(plans.len()).map_err(|_|Error::Limit)?;
        for(key,plan)in plans{let previous=if reset{None}else{self.lanes.get(&key).or_else(||(key.owner!=0).then(||self.lanes.get(&Key{owner:0,..key})).flatten()).copied()};let mut lane=previous.unwrap_or(Lane {phase:[0.0;2],settings:plan.settings,dimensions:if plan.enabled{plan.dimensions}else{None},enabled:plan.enabled,stamp:plan.stamp});
            // Inherit the last live normalized phase when capturing a private
            // view, advance with its old input dimensions, then adopt its new
            // explicit canvas. Never borrow another arbitrary private timeline.
            if previous.is_some(){tick(&mut lane,delta);}for i in 0..2{if plan.settings.speed[i]==0.0{lane.phase[i]=0.0;}}
            // A disabled filter still ticks in OBS, but its target's last render
            // size remains the most recent enabled observation.
            if plan.enabled&&!plan.stamp.zip(lane.stamp).is_some_and(|(candidate,old)|stale(candidate,old)){lane.dimensions=plan.dimensions;if plan.stamp.is_some(){lane.stamp=plan.stamp;}}lane.settings=plan.settings;lane.enabled=plan.enabled;lanes.insert(key,lane);
        }
        if reset {self.history.clear();self.history_bytes=0;}
        // Record only intervals whose lane definitions were unchanged. The
        // lane table is shared with the previous snapshot rather than copied.
        if !reset&&!changed&&delta>0&&!self.last_lanes.is_empty() {
            let bytes=self.last_lanes.capacity().saturating_add(1).saturating_mul(2)
                .saturating_mul(std::mem::size_of::<(Key,Lane)>()+1).saturating_add(std::mem::size_of::<Segment>());
            if bytes<=HISTORY_BYTES {
                while self.history.len()>=HISTORY_SEGMENTS||self.history_bytes>HISTORY_BYTES-bytes {
                    if let Some(old)=self.history.pop_front(){self.history_bytes=self.history_bytes.saturating_sub(old.bytes);}else{break;}
                }
                if self.history.try_reserve(1).is_ok(){self.history.push_back(Segment {session,epoch:self.epoch,start:self.last.unwrap(),end:now,lanes:self.last_lanes.clone(),bytes});self.history_bytes+=bytes;}
            }
        }
        let shared=Arc::new(lanes.clone());let snapshot=Snapshot {session,clock:identity,epoch,lanes:shared.clone()};self.session=Some(session);self.last=Some(now);self.identity=identity;self.epoch=epoch;self.lanes=lanes;self.last_lanes=shared;Ok(snapshot)
    }
}
