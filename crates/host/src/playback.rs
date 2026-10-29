//! Runtime activation clocks; explicit monotonic media time, no OS clock access.
mod slideshow_control;
use crate::transition::{FrameScene, SceneKey};
use skylive_core::{Id, ImageAsset, MAX_SCENES, Session, SourceKind};
use skylive_media::MediaTime;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc,atomic::{AtomicUsize,Ordering}},
};

const MAX_CLOCK_METADATA_BYTES:usize=64*1024*1024;
static CLOCK_METADATA_BYTES:AtomicUsize=AtomicUsize::new(0);
#[derive(Debug)]pub(crate) struct MetadataPermit{bytes:usize}
impl MetadataPermit{pub(crate) fn acquire(bytes:usize)->Result<Self,PlaybackError>{CLOCK_METADATA_BYTES.fetch_update(Ordering::AcqRel,Ordering::Acquire,|used|used.checked_add(bytes).filter(|n|*n<=MAX_CLOCK_METADATA_BYTES)).map_err(|_|PlaybackError::ResourceLimit)?;Ok(Self{bytes})}}
impl Drop for MetadataPermit{fn drop(&mut self){CLOCK_METADATA_BYTES.fetch_sub(self.bytes,Ordering::AcqRel);}}
#[derive(Debug)]struct OrderLease{order:crate::image_slideshow::Order,_permit:MetadataPermit}
#[derive(Clone,Debug)]struct SourceTime{order:Option<Arc<OrderLease>>,slideshow:Option<crate::image_slideshow::Sample>,elapsed:MediaTime,visible_elapsed:Option<MediaTime>,advancing:bool,transition_extra:u64}
impl SourceTime{fn cold(elapsed:MediaTime)->Self{Self{order:None,slideshow:None,elapsed,visible_elapsed:None,advancing:true,transition_extra:0}}fn advances(&self,old:&Self,delta:u64)->bool{self.slideshow.is_none()&&old.slideshow.is_none()&&self.advancing==old.advancing&&self.transition_extra==old.transition_extra&&if self.advancing{self.elapsed.0.checked_sub(old.elapsed.0)==Some(delta)&&match(self.visible_elapsed,old.visible_elapsed){(Some(a),Some(b))=>a.0.checked_sub(b.0)==Some(delta),(None,None)=>true,_=>false}}else{self.elapsed==old.elapsed&&self.visible_elapsed==old.visible_elapsed}}}
#[derive(Debug)]struct SnapshotTimes{entries:HashMap<Id,SourceTime>,_permit:Option<MetadataPermit>}
impl std::ops::Deref for SnapshotTimes{type Target=HashMap<Id,SourceTime>;fn deref(&self)->&Self::Target{&self.entries}}
impl SnapshotTimes{fn cold(entries:HashMap<Id,SourceTime>)->Self{Self{entries,_permit:None}}}
fn table_charge<T>(count:usize)->Result<usize,PlaybackError>{count.checked_add(1).and_then(|n|n.checked_mul(2)).and_then(|n|n.checked_mul(std::mem::size_of::<T>()+1)).and_then(|n|n.checked_add(128)).ok_or(PlaybackError::ResourceLimit)}
fn asset_charge(kind:&SourceKind)->Result<usize,PlaybackError>{let mut bytes=0usize;match kind{SourceKind::Image{asset}=>bytes=std::mem::size_of_val(asset)+asset.path().len(),SourceKind::ImageSlideShow{settings}=>{for asset in settings.images(){bytes=bytes.checked_add(std::mem::size_of_val(asset)+asset.path().len()).ok_or(PlaybackError::ResourceLimit)?;}for entry in settings.entries(){bytes=bytes.checked_add(std::mem::size_of_val(entry)+entry.path().len()).ok_or(PlaybackError::ResourceLimit)?;}bytes=bytes.checked_add(settings.entry_image_counts().len()*std::mem::size_of::<u16>()).ok_or(PlaybackError::ResourceLimit)?;},_=>{}}Ok(bytes)}
#[derive(Clone, Debug)]
pub struct PlaybackSnapshot {
    session: u64,
    revision: u64,
    epoch: u64,
    now: MediaTime,
    elapsed: Arc<SnapshotTimes>,
    pub(crate) scroll:crate::scroll_animation::Snapshot,
    pub(crate) mask_animation:crate::mask_animation_clock::Snapshot,
    pub(crate) temporal:crate::video_filters::TemporalSnapshot,
}
/// Immutable mask timeline and the revision owner that produced it. A capture
/// consumer must additionally bind it to the source epoch and sample time.
#[derive(Clone)]
pub struct CameraMaskSnapshot {
    pub(crate) session:u64,
    pub(crate) revision:u64,
    pub(crate) playback_epoch:u64,
    pub(crate) clock:crate::mask_animation_clock::Snapshot,
    pub(crate) versions:Arc<std::sync::Mutex<crate::mask_live_versions::Book>>,
    pub(crate) correlation:Option<skylive_media::QpcMediaCorrelation>,
    pub(crate) scroll:Option<Arc<CameraScrollHistory>>,
}
/// Carries the scroll clock's own epoch separately from playback epoch. The
/// metadata permit stays live as long as a capture owner retains the history.
pub(crate) struct CameraScrollHistory {
    pub(crate) epoch:u64,
    pub(crate) history:crate::scroll_animation::HistorySnapshot,
    _permit:MetadataPermit,
}
impl CameraMaskSnapshot {
    pub(crate) fn same_scroll_scope(&self,old:&Self)->bool {
        match (&self.scroll,&old.scroll) {
            (None,None)=>true,
            (Some(a),Some(b))=>a.epoch==b.epoch && a.history.same_origin(&b.history),
            _=>false,
        }
    }
}
impl PlaybackSnapshot {
    pub fn session(&self) -> u64 {
        self.session
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn now(&self) -> MediaTime {
        self.now
    }
    /// Sampling-equivalent forward advancement; epoch alone is not a globally
    /// unique clock domain. Every source must retain the same time offset.
    pub(crate) fn advances_from(&self, previous: &Self) -> bool {
        let Some(delta) = self.now.0.checked_sub(previous.now.0) else {
            return false;
        };
        self.session == previous.session
            && self.revision == previous.revision
            && self.epoch == previous.epoch
            && self.scroll.same_origin(&previous.scroll)
            && self.mask_animation.reusable_from(&previous.mask_animation)
            && self.mask_animation.same_files(&previous.mask_animation)
            && self.temporal.reusable_from(&previous.temporal)
            && self.elapsed.len() == previous.elapsed.len()
            && self.elapsed.iter().all(|(source, elapsed)| {
                previous
                    .elapsed
                    .get(source)
                    .is_some_and(|old|elapsed.advances(old,delta))
            })
    }
    pub(crate) fn retained_clock_bytes(&self) -> usize {
        // Conservative bucket/control-byte allowance using actual capacity,
        // not just live entries, plus the Arc and HashMap headers.
        self.elapsed.values().filter(|time|time.order.is_some()).count().saturating_mul(std::mem::size_of::<OrderLease>()+2*std::mem::size_of::<usize>())+self.scroll.bytes()+self.mask_animation.files_bytes()+std::mem::size_of_val(&*self.elapsed)
            + 2 * std::mem::size_of::<usize>()
            + (self.elapsed.capacity() + 1)
                .saturating_mul(2)
                .saturating_mul(std::mem::size_of::<(Id, SourceTime)>() + 1)
    }
    pub(crate) fn with_scroll_sample(mut self,sample:crate::scroll_animation::Snapshot)->Self{self.scroll=sample;self}
    pub(crate) fn with_mask_sample(mut self,sample:crate::mask_animation_clock::Snapshot)->Self{self.mask_animation=sample;self}
    pub fn elapsed(&self, source: Id) -> MediaTime {
        self.elapsed.get(&source).map(|time|time.elapsed).unwrap_or_default()
    }
    pub(crate) fn slideshow_order(&self,source:Id)->Option<&crate::image_slideshow::Order>{self.elapsed.get(&source).and_then(|time|time.order.as_ref()).map(|lease|&lease.order)}
    pub(crate) fn slideshow_sample(&self,source:Id)->Option<crate::image_slideshow::Sample>{self.elapsed.get(&source).and_then(|time|time.slideshow)}
    pub(crate) fn visible_elapsed(&self,source:Id)->Option<MediaTime>{self.elapsed.get(&source).and_then(|time|time.visible_elapsed)}
    pub(crate) fn slideshow_transition_extra(&self,source:Id)->u64{self.elapsed.get(&source).map_or(0,|time|time.transition_extra)}
    /// Headless explicit timeline: every image has the requested elapsed time.
    pub fn at_time(session: &Session, now: MediaTime) -> Self {
        let current = session.snapshot();
        Self {
            temporal:Default::default(),
            scroll:crate::scroll_animation::Snapshot::cold(session.identity(),&current,now),
            mask_animation:crate::mask_animation_clock::Snapshot::cold(session.identity(),&current,now),
            session: session.identity(),
            revision: current.revision(),
            epoch: 0,
            now,
            elapsed: Arc::new(SnapshotTimes::cold(
                current
                    .sources()
                    .iter()
                    .filter(|s| matches!(s.kind(), SourceKind::Image { .. } | SourceKind::ImageSlideShow {..}))
                    .map(|s| (s.id(),SourceTime::cold(now)))
                    .collect(),
            )),
        }
    }
    pub(crate) fn for_collection(
        collection: &skylive_core::StudioSnapshot,
        now: MediaTime,
    ) -> Self {
        Self {
            temporal:Default::default(),
            scroll:crate::scroll_animation::Snapshot::cold(0,collection,now),
            mask_animation:crate::mask_animation_clock::Snapshot::cold(0,collection,now),
            session: 0,
            revision: collection.revision(),
            epoch: 0,
            now,
            elapsed: Arc::new(SnapshotTimes::cold(
                collection
                    .sources()
                    .iter()
                    .filter(|s| matches!(s.kind(), SourceKind::Image { .. } | SourceKind::ImageSlideShow {..}))
                    .map(|s| (s.id(),SourceTime::cold(now)))
                    .collect(),
            )),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaybackError {
    Closed,
    Control(&'static str),
    InvalidScene(Id),
    SceneLimit,
    IdentityExhausted,
    ResourceLimit,
    ConflictingResource(Id),
    Transition(crate::transition::TransitionError),
    Scroll(crate::scroll_animation::Error),
    Temporal(String),
}
impl fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::Control(message)=self{f.write_str(message)}else{write!(f, "{self:?}")}
    }
}
impl std::error::Error for PlaybackError {}
impl PlaybackError{pub fn is_random_work_limit(&self)->bool{matches!(self,Self::Control(message)if *message==crate::image_slideshow::random::WORK_LIMIT)}}
struct SourceDescriptor{kind:SourceKind,image_settings:skylive_core::ImageSourceSettings,order:Option<Arc<OrderLease>>,random_seed:u64,_permit:MetadataPermit}
#[derive(Clone)]struct ActiveSource{
    transport:Option<crate::image_slideshow::transport::Runtime>,asset:Arc<SourceDescriptor>,started:MediaTime,paused_elapsed:u64,visible:bool,shown_elapsed:u64,transition_extra:u64,transition_cycle:u64,hidden_at:Option<MediaTime>,
}
impl ActiveSource{
    fn behavior(&self)->skylive_core::SlideShowBehavior{match &self.asset.kind{SourceKind::ImageSlideShow{settings}=>settings.behavior(),_=>skylive_core::SlideShowBehavior::StopRestart}}
    fn advances(&self)->bool{self.visible||self.behavior().runs_hidden()}
    fn elapsed(&self,now:MediaTime)->u64{if self.advances(){now.0-self.started.0}else{self.paused_elapsed}}
    fn sample(&self,now:MediaTime)->SourceTime{let elapsed=self.elapsed(now);let cycle=match &self.asset.kind{SourceKind::ImageSlideShow{settings}=>elapsed/settings.period_ns(),_=>0};let extra=if self.visible&&self.behavior()==skylive_core::SlideShowBehavior::PauseUnpause&&cycle==self.transition_cycle{self.transition_extra}else{0};SourceTime{order:self.asset.order.clone(),slideshow:self.transport.and_then(|runtime|match &self.asset.kind{SourceKind::ImageSlideShow{settings}=>Some(runtime.sample(settings,now.0)),_=>None}),elapsed:MediaTime(elapsed),visible_elapsed:self.visible.then_some(MediaTime(elapsed.saturating_sub(self.shown_elapsed))),advancing:self.advances(),transition_extra:extra}}
    fn random_work(&self,now:MediaTime)->u64{match &self.asset.kind{SourceKind::ImageSlideShow{settings}if settings.randomize()&&!settings.images().is_empty()=>self.transport.map_or(if settings.random_compatibility()==skylive_core::SlideShowRandomCompatibility::LegacyV1{1}else{11},|runtime|runtime.due_random_advances(settings,self.elapsed(now))),_=>0}}
    fn tick_transport(&mut self,now:MediaTime)->Result<(),PlaybackError>{
        let logical=self.elapsed(now);if let SourceKind::ImageSlideShow{settings}=&self.asset.kind{if settings.images().is_empty(){self.transport=None;return Ok(());}let order=&self.asset.order.as_ref().expect("slideshow descriptor order").order;
            if self.transport.is_none(){if settings.randomize(){self.transport=Some(crate::image_slideshow::transport::Runtime::random_start(settings,order,self.asset.random_seed,now.0,logical).map_err(PlaybackError::Control)?);}else if settings.mode()==skylive_core::SlideShowMode::Manual{let sample=crate::image_slideshow::sample_ordered(settings,0,0,order);self.transport=Some(crate::image_slideshow::transport::Runtime::from_sample(settings,order,now.0,logical,sample));}}
            if let Some(runtime)=&mut self.transport{runtime.tick(settings,order,logical,now.0).map_err(PlaybackError::Control)?;}
        }Ok(())
    }
    fn reset(&mut self,now:MediaTime){self.transport=None;self.started=now;self.paused_elapsed=0;self.shown_elapsed=0;self.transition_extra=0;self.transition_cycle=0;self.hidden_at=(!self.visible).then_some(now);}
}
#[derive(Default)]struct ActiveSources{entries:HashMap<Id,ActiveSource>,_permit:Option<MetadataPermit>}
impl std::ops::Deref for ActiveSources{type Target=HashMap<Id,ActiveSource>;fn deref(&self)->&Self::Target{&self.entries}}
impl std::ops::DerefMut for ActiveSources{fn deref_mut(&mut self)->&mut Self::Target{&mut self.entries}}

fn source_seed(seed:u64,source:Id,kind:&SourceKind)->u64{use std::hash::{Hash,Hasher};let mut hash=std::collections::hash_map::DefaultHasher::new();"skylive-slideshow-random-splitmix64-v1".hash(&mut hash);seed.hash(&mut hash);source.hash(&mut hash);if let SourceKind::ImageSlideShow{settings}=kind{settings.hash(&mut hash);}hash.finish()}
type Scope = (u64, u64, Vec<SceneKey>);
static NEXT_MASK_DOMAIN:std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(1);
fn next_mask_domain()->Result<u64,PlaybackError>{NEXT_MASK_DOMAIN.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|n.checked_add(1)).map_err(|_|PlaybackError::IdentityExhausted)}
pub struct PlaybackClock {
    mask_versions:Option<Arc<std::sync::Mutex<crate::mask_live_versions::Book>>>,
    mask_domain:u64,
    temporal_observer:Option<crate::video_filters::TemporalObserver>,
    random_seed:u64,
    scroll:crate::scroll_animation::Clock,
    mask_animation:crate::mask_animation_clock::Clock,
    scope: Option<Scope>,
    active: ActiveSources,
    epoch: u64,
    last_now: Option<MediaTime>,
}
impl Default for PlaybackClock {
    fn default() -> Self {
        Self::new()
    }
}
impl PlaybackClock {
    pub(crate) fn with_render_filters(observer:Option<crate::video_filters::TemporalObserver>,versions:Option<Arc<std::sync::Mutex<crate::mask_live_versions::Book>>>)->Self{let mut clock=Self::new();clock.temporal_observer=observer;clock.mask_versions=versions;clock}
    /// Retain the exact mask timeline sample with its matching revision book.
    /// Returning None prevents a caller from inventing a live resource owner.
    pub fn camera_mask_snapshot(&self,snapshot:&PlaybackSnapshot)->Option<CameraMaskSnapshot> {
        let versions=self.mask_versions.as_ref()?.clone();
        if snapshot.session!=snapshot.mask_animation.session()
            || snapshot.mask_animation.files_domain!=self.mask_domain
            || snapshot.epoch!=self.epoch || snapshot.now!=self.last_now? {return None;}
        // Failure to reserve this optional history must not interrupt unrelated
        // animated masks. Upstream Scroll stays unavailable without a lease.
        let scroll=self.scroll.history_for(&snapshot.scroll).ok().flatten().and_then(|(epoch,history)|{
            let permit=MetadataPermit::acquire(history.bytes()).ok()?;
            Some(Arc::new(CameraScrollHistory{epoch,history,_permit:permit}))
        });
        Some(CameraMaskSnapshot {session:snapshot.session,revision:snapshot.revision,
            playback_epoch:snapshot.epoch,clock:snapshot.mask_animation.clone(),versions,correlation:None,scroll})
    }
    /// Correlate QPC with the very same monotonic origin used to create the
    /// playback sample. This does not depend on Program audio being active.
    /// An incompatible or stale origin cannot produce a usable correlation.
    pub fn camera_mask_snapshot_calibrated(&self,snapshot:&PlaybackSnapshot,origin:std::time::Instant)->Option<CameraMaskSnapshot> {
        let mut result=self.camera_mask_snapshot(snapshot)?;
        let current=u64::try_from(origin.elapsed().as_nanos()).ok()?;
        if current.checked_sub(snapshot.now.0)? > 1_000_000_000 {return None;}
        result.correlation=crate::audio_capture::native::correlate_media_clock(origin,snapshot.session,snapshot.epoch);
        Some(result)
    }
    pub fn new() -> Self {use std::hash::{BuildHasher,Hasher};let seed=std::collections::hash_map::RandomState::new().build_hasher().finish();Self::with_random_seed(seed)}
    /// Deterministic private run identity; never persisted as authored data.
    pub fn with_random_seed(seed:u64)->Self{
        Self {random_seed:seed,temporal_observer:None,mask_versions:None,mask_domain:0,
            scroll:Default::default(),
            mask_animation:Default::default(),
            scope: None,
            active: ActiveSources::default(),
            epoch: 0,
            last_now: None,
        }
    }
    pub fn reset(&mut self, now: MediaTime) -> Result<(), PlaybackError> {
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or(PlaybackError::IdentityExhausted)?;
        if let Some(observer)=&self.temporal_observer{observer.reset();}
        self.scroll.reset();
        self.mask_animation.reset();
        self.mask_domain=0;
        for source in self.active.values_mut() {
            source.reset(now);
        }
        self.epoch = epoch;
        self.last_now = Some(now);
        Ok(())
    }
    /// Restart one currently active source after an explicit image Reload,
    /// including a same-descriptor canonical no-op. OBS image_source_update
    /// reloads/reinitializes the image-file clock; unrelated sources keep time.
    /// A backward timestamp is a global discontinuity, as it is in update().
    pub fn restart_source(&mut self, source: Id, now: MediaTime) -> Result<(), PlaybackError> {
        if let Some(observer)=&self.temporal_observer{observer.reset_source(source).map_err(|e|PlaybackError::Temporal(e.detail))?;}
        if self.last_now.is_some_and(|old| now < old) {
            return self.reset(now);
        }
        if let Some(active) = self.active.get_mut(&source) {
            let epoch = self
                .epoch
                .checked_add(1)
                .ok_or(PlaybackError::IdentityExhausted)?;
            active.reset(now);
            self.epoch = epoch;
        }
        // Even an inactive no-op establishes the latest observed monotonic
        // point, so a subsequent update cannot subtract a future start time.
        self.last_now = Some(now);
        Ok(())
    }
    pub fn update(
        &mut self,
        session: &Session,
        active_scenes: &[Id],
        now: MediaTime,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.update_outputs(
            session,
            &active_scenes
                .iter()
                .copied()
                .map(FrameScene::Live)
                .collect::<Vec<_>>(),
            now,
        )
    }
    /// Evaluate visible live/captured layouts together. Retained Program-only
    /// sources remain active; sharing source identity does not duplicate clocks.
    pub fn update_outputs(
        &mut self,
        session: &Session,
        outputs: &[FrameScene],
        now: MediaTime,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.update_outputs_observed(session,outputs,now,None)
    }
    /// Uses only already-owned, exact-binding capture metadata; never opens a source.
    pub fn update_outputs_with_capture(&mut self,session:&Session,outputs:&[FrameScene],now:MediaTime,capture:&crate::capture::CaptureSnapshot)->Result<PlaybackSnapshot,PlaybackError>{
        self.update_outputs_observed(session,outputs,now,Some(capture))
    }
    fn update_outputs_observed(&mut self,session:&Session,outputs:&[FrameScene],now:MediaTime,capture:Option<&crate::capture::CaptureSnapshot>)->Result<PlaybackSnapshot,PlaybackError>{
        if session.is_closed() {
            if let Some(observer)=&self.temporal_observer{observer.reset();}
            self.active=ActiveSources::default();self.scope=None;self.last_now=None;self.scroll.reset();self.mask_animation.reset();
            return Err(PlaybackError::Closed);
        }
        if outputs.len() > MAX_SCENES {
            return Err(PlaybackError::SceneLimit);
        }
        let current = session.snapshot();
        let scope = (
            session.identity(),
            current.revision(),
            outputs.iter().map(FrameScene::key).collect(),
        );
        let new_session = self
            .scope
            .as_ref()
            .is_some_and(|old| old.0 != session.identity());
        let backward = self.last_now.is_some_and(|old| now < old);
        let mut changed = new_session || backward;
        let mut desired = None;
        if self.scope.as_ref() != Some(&scope) {
            let mut wanted:HashMap<Id,(&SourceKind,skylive_core::ImageSourceSettings)>=HashMap::new();let mut visible=HashSet::new();
            let mut seen = HashSet::new();
            for view in outputs {
                if !seen.insert(view.key()) {
                    continue;
                }
                view.validate_session(session.identity())
                    .map_err(PlaybackError::Transition)?;
                if matches!(view,FrameScene::Live(id) if current.scene(*id).is_none()) {
                    return Err(PlaybackError::InvalidScene(view.scene()));
                }
                for source in view
                    .visible_sources(&current)
                    .map_err(PlaybackError::Transition)?
                {
                    if matches!(source.kind(),SourceKind::Image {..}|SourceKind::ImageSlideShow {..}) {
                        let asset=source.kind();visible.insert(source.id());
                        if let Some(old) = wanted.get(&source.id()) {
                            if old.0 != asset||old.1!=source.image_settings() {
                                return Err(PlaybackError::ConflictingResource(source.id()));
                            }
                        } else {
                            if wanted.len() >= 3 * skylive_core::MAX_SOURCES {
                                return Err(PlaybackError::ResourceLimit);
                            }
                            wanted.insert(source.id(),(asset,source.image_settings()));
                        }
                    }
                }
            }
            // Hidden Pause/Always metadata remains, but no image is read,
            // pinned, decoded, rendered or acquired merely to advance its time.
            for source in current.sources(){if matches!(source.kind(),SourceKind::ImageSlideShow{settings}if settings.behavior().retains_hidden()||settings.randomize()||settings.mode()==skylive_core::SlideShowMode::Manual||self.active.get(&source.id()).is_some_and(|old|old.transport.is_some()&&&old.asset.kind==source.kind()))&&!wanted.contains_key(&source.id()){
                if wanted.len()>=3*skylive_core::MAX_SOURCES{return Err(PlaybackError::ResourceLimit);}wanted.insert(source.id(),(source.kind(),source.image_settings()));
            }}
            changed |= wanted.len()!=self.active.len()||wanted.iter().any(|(id,(asset,settings))|self.active.get(id).is_none_or(|old|&old.asset.kind!=*asset||old.asset.image_settings!=*settings||old.visible!=visible.contains(id)));
            desired=Some((wanted,visible));
        }
        // Prepare the independent Scroll owner transaction before any clock mutation.
        let mut scroll=self.scroll.clone();let scroll_sample=scroll.sample(session.identity(),&current,outputs,now,capture).map_err(PlaybackError::Scroll)?;
        let mut mask_animation=self.mask_animation.clone();let mut mask_sample=mask_animation.sample(session.identity(),&current,outputs,now).map_err(PlaybackError::Control)?;
        let mask_domain=if self.mask_versions.is_none(){0}else if new_session||backward||self.mask_domain==0{next_mask_domain()?}else{self.mask_domain};
        if let Some(book)=&self.mask_versions{
            let mut versions=book.lock().map_err(|_|PlaybackError::Control("mask revision owner poisoned"))?;
            let files=crate::mask_live_versions::sample_scene(&mut versions,mask_domain,&current,outputs).map_err(PlaybackError::Control)?;
            mask_sample=mask_sample.with_files(files,mask_domain);
        }
        // All validation and identity-overflow checks precede mutation.
        let next_epoch = if changed {
            self.epoch
                .checked_add(1)
                .ok_or(PlaybackError::IdentityExhausted)?
        } else {
            self.epoch
        };
        // Retire ownership that the validated current closure no longer uses,
        // even if admission of a replacement later fails. Surviving clocks and
        // canonical data remain unchanged; historical time snapshots own no assets.
        if let Some((wanted,_))=&desired{self.active.entries.retain(|id,_|wanted.contains_key(id));}
        let count=desired.as_ref().map_or(self.active.len(),|(wanted,_)|wanted.len());
        // Old snapshots retain their own physical lease until their final Arc
        // drops; cancellation never returns these bytes before actual retirement.
        let snapshot_permit=MetadataPermit::acquire(table_charge::<(Id,SourceTime)>(count)?)?;
        let mut random_preparation_work=0u64;
        let mut candidate=if let Some((wanted,visible))=desired{
            let permit=MetadataPermit::acquire(table_charge::<(Id,ActiveSource)>(wanted.len())?)?;let mut entries=HashMap::new();entries.try_reserve(wanted.len()).map_err(|_|PlaybackError::ResourceLimit)?;
            for(id,(asset,image_settings))in wanted{let showing=visible.contains(&id);let previous=if new_session||backward{None}else{self.active.get(&id).filter(|old|&old.asset.kind==asset&&old.asset.image_settings==image_settings)};
                let elapsed=previous.map_or(0,|old|old.elapsed(now));let runs_hidden=matches!(asset,SourceKind::ImageSlideShow{settings}if settings.behavior().runs_hidden());
                let started=if showing||runs_hidden{MediaTime(now.0.checked_sub(elapsed).ok_or(PlaybackError::ResourceLimit)?)}else{now};
                let shown_elapsed=if showing{previous.filter(|old|old.visible).map_or(elapsed,|old|old.shown_elapsed)}else{previous.map_or(elapsed,|old|old.shown_elapsed)};
                let descriptor=if let Some(old)=previous{old.asset.clone()}else{let charge=asset_charge(asset)?.checked_add(std::mem::size_of::<SourceDescriptor>()+2*std::mem::size_of::<usize>()).ok_or(PlaybackError::ResourceLimit)?;let permit=MetadataPermit::acquire(charge)?;let order=if let SourceKind::ImageSlideShow{settings}=asset{let bytes=std::mem::size_of::<OrderLease>()+2*std::mem::size_of::<usize>();let order_permit=MetadataPermit::acquire(bytes)?;Some(Arc::new(OrderLease{order:crate::image_slideshow::Order::new(settings),_permit:order_permit}))}else{None};let random_seed=source_seed(self.random_seed,id,asset);Arc::new(SourceDescriptor{kind:asset.clone(),image_settings,order,random_seed,_permit:permit})};
                let pause=matches!(asset,SourceKind::ImageSlideShow{settings}if settings.behavior()==skylive_core::SlideShowBehavior::PauseUnpause);
                let cycle=match asset{SourceKind::ImageSlideShow{settings}=>elapsed/settings.period_ns(),_=>0};let mut extra=previous.filter(|old|old.transition_cycle==cycle).map_or(0,|old|old.transition_extra);
                if pause&&showing{if let Some(hidden)=previous.and_then(|old|old.hidden_at){extra=extra.checked_add(now.0-hidden.0).ok_or(PlaybackError::ResourceLimit)?;}}
                let hidden_at=if pause&&!showing{Some(previous.and_then(|old|old.hidden_at).unwrap_or(now))}else{None};
                let mut transport=previous.and_then(|old|old.transport);
                if showing&&previous.is_some_and(|old|!old.visible)&&matches!(asset,SourceKind::ImageSlideShow{settings}if settings.behavior()==skylive_core::SlideShowBehavior::StopRestart){if let (Some(runtime),SourceKind::ImageSlideShow{settings})=(&mut transport,asset){if settings.randomize(){random_preparation_work=random_preparation_work.checked_add(11).filter(|work|*work<=crate::image_slideshow::random::MAX_UPDATE_ADVANCES).ok_or(PlaybackError::Control(crate::image_slideshow::random::WORK_LIMIT))?;}runtime.visibility_restart(settings,&descriptor.order.as_ref().expect("slideshow descriptor order").order,now.0,elapsed).map_err(PlaybackError::Control)?;}}
                entries.insert(id,ActiveSource{transport,asset:descriptor,started,paused_elapsed:elapsed,visible:showing,shown_elapsed,transition_extra:if pause{extra}else{0},transition_cycle:cycle,hidden_at});
            }Some(ActiveSources{entries,_permit:Some(permit)})
        }else{None};
        // RNG errors must not publish a partially advanced group of sources.
        // Clone only fixed clock state and Arcs, charging actual old+new tables.
        if candidate.is_none()&&self.active.values().any(|state|matches!(&state.asset.kind,SourceKind::ImageSlideShow{settings}if settings.randomize())){
            let permit=MetadataPermit::acquire(table_charge::<(Id,ActiveSource)>(self.active.len())?)?;let mut entries=HashMap::new();entries.try_reserve(self.active.len()).map_err(|_|PlaybackError::ResourceLimit)?;for(id,state)in self.active.iter(){entries.insert(*id,state.clone());}candidate=Some(ActiveSources{entries,_permit:Some(permit)});
        }
        // Allocate/admit the immutable time map before changing an owner. The
        // candidate source descriptors and preceding version are both charged.
        let mut times=HashMap::new();times.try_reserve(count).map_err(|_|PlaybackError::ResourceLimit)?;
        let reset_existing=backward;
        let states=candidate.as_mut().unwrap_or(&mut self.active);
        let mut random_work=random_preparation_work;
        for state in states.values_mut(){if reset_existing{state.reset(now);}let work=state.random_work(now);if work>crate::image_slideshow::random::MAX_SOURCE_ADVANCES{return Err(PlaybackError::Control(crate::image_slideshow::random::WORK_LIMIT));}random_work=random_work.checked_add(work).filter(|work|*work<=crate::image_slideshow::random::MAX_UPDATE_ADVANCES).ok_or(PlaybackError::Control(crate::image_slideshow::random::WORK_LIMIT))?;}
        for(id,state)in states.iter_mut(){state.tick_transport(now)?;times.insert(*id,state.sample(now));}
        let temporal=if let Some(observer)=&self.temporal_observer{match observer.sample(session.identity(),&current,outputs,now,capture){Ok(sample)=>sample,Err(error)=>{observer.reset();return Err(PlaybackError::Temporal(error.detail));}}}else{Default::default()};
        if let Some(candidate)=candidate{self.active=candidate;}
        self.scroll=scroll;
        self.mask_animation=mask_animation;
        self.mask_domain=mask_domain;
        self.scope = Some(scope);
        self.epoch = next_epoch;
        self.last_now = Some(now);
        Ok(PlaybackSnapshot {
            temporal,
            scroll:scroll_sample,
            mask_animation:mask_sample,
            session: session.identity(),
            revision: current.revision(),
            epoch: self.epoch,
            now,
            elapsed:Arc::new(SnapshotTimes{entries:times,_permit:Some(snapshot_permit)}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylive_core::Command;
    fn fixture() -> (Session, Id, Id) {
        let mut s = Session::new();
        let scene = s.preview();
        let asset = ImageAsset::new("C:/gif/a.gif".into(), [1; 32], 1, 1).unwrap();
        let item = s
            .apply(Command::AddImageSource {
                scene,
                name: "GIF".into(),
                asset,
            })
            .unwrap()
            .created
            .unwrap();
        let source = s.snapshot().sources()[0].id();
        (s, item, source)
    }
    #[test]
    fn source_clock_survives_unrelated_edits_and_shared_scene_switches() {
        let (mut s, _, source) = fixture();
        let first = s.preview();
        let second = s
            .apply(Command::DuplicateScene {
                scene: first,
                name: "Second".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let mut clock = PlaybackClock::new();
        let a = clock.update(&s, &[first], MediaTime(100)).unwrap();
        assert_eq!(a.elapsed(source), MediaTime(0));
        s.apply(Command::RenameCollection {
            name: "Changed".into(),
        })
        .unwrap();
        let b = clock.update(&s, &[first, second], MediaTime(150)).unwrap();
        assert_eq!(b.elapsed(source), MediaTime(50));
        assert_eq!(b.epoch(), a.epoch());
        let c = clock.update(&s, &[second], MediaTime(200)).unwrap();
        assert_eq!(c.elapsed(source), MediaTime(100));
        assert_eq!(c.epoch(), a.epoch());
    }
    #[test]
    fn hide_reload_backward_time_and_reopen_reset_with_new_epoch() {
        let (mut s, item, source) = fixture();
        let scene = s.preview();
        let mut clock = PlaybackClock::new();
        let a = clock.update(&s, &[scene], MediaTime(0)).unwrap();
        s.apply(Command::SetVisible {
            scene,
            item,
            visible: false,
        })
        .unwrap();
        let hidden = clock.update(&s, &[scene], MediaTime(10)).unwrap();
        assert!(hidden.epoch() > a.epoch());
        s.apply(Command::SetVisible {
            scene,
            item,
            visible: true,
        })
        .unwrap();
        let shown = clock.update(&s, &[scene], MediaTime(20)).unwrap();
        assert_eq!(shown.elapsed(source), MediaTime(0));
        s.apply(Command::SetImageSource {
            source,
            asset: ImageAsset::new("C:/gif/a.gif".into(), [2; 32], 1, 1).unwrap(),
        })
        .unwrap();
        let reload = clock.update(&s, &[scene], MediaTime(40)).unwrap();
        assert_eq!(reload.elapsed(source), MediaTime(0));
        assert!(reload.epoch() > shown.epoch());
        let back = clock.update(&s, &[scene], MediaTime(30)).unwrap();
        assert!(back.epoch() > reload.epoch());
        assert_eq!(back.elapsed(source), MediaTime(0));
        let reopened = Session::from_snapshot((*s.snapshot()).clone(), true).unwrap();
        let next = clock.update(&reopened, &[scene], MediaTime(50)).unwrap();
        assert_ne!(next.session(), back.session());
        assert!(next.epoch() > back.epoch());
    }
    #[test]
    fn invalid_updates_do_not_change_clock_and_explicit_reset_is_a_discontinuity() {
        let (s, _, source) = fixture();
        let mut clock = PlaybackClock::new();
        let a = clock.update(&s, &[s.preview()], MediaTime(100)).unwrap();
        assert_eq!(
            clock
                .update(&s, &[Id::new(999)], MediaTime(90))
                .unwrap_err(),
            PlaybackError::InvalidScene(Id::new(999))
        );
        let b = clock.update(&s, &[s.preview()], MediaTime(150)).unwrap();
        assert_eq!(b.epoch(), a.epoch());
        assert_eq!(b.elapsed(source), MediaTime(50));
        clock.reset(MediaTime(160)).unwrap();
        let c = clock.update(&s, &[s.preview()], MediaTime(170)).unwrap();
        assert!(c.epoch() > b.epoch());
        assert_eq!(c.elapsed(source), MediaTime(10));
    }
    #[test]
    fn no_op_reload_restarts_only_target_gif_without_authored_revision_or_other_clock_reset() {
        use skylive_media::{
            AnimationClip, AnimationFrame, AnimationLimits, AnimationLoop, RgbaFrame,
        };
        let (mut session, _, first) = fixture();
        let scene = session.preview();
        let asset = match session.snapshot().source(first).unwrap().kind() {
            SourceKind::Image { asset } => asset.clone(),
            _ => unreachable!(),
        };
        session
            .apply(Command::AddImageSource {
                scene,
                name: "Second GIF".into(),
                asset: asset.clone(),
            })
            .unwrap();
        let second = session.snapshot().sources()[1].id();
        let clip = AnimationClip::new(
            vec![
                AnimationFrame {
                    image: Arc::new(RgbaFrame::solid(1, 1, [255, 0, 0, 255]).unwrap()),
                    duration_ns: 100_000_000,
                },
                AnimationFrame {
                    image: Arc::new(RgbaFrame::solid(1, 1, [0, 255, 0, 255]).unwrap()),
                    duration_ns: 100_000_000,
                },
            ],
            AnimationLoop::Infinite,
            AnimationLimits::default(),
        )
        .unwrap();
        let mut clock = PlaybackClock::new();
        clock.update(&session, &[scene], MediaTime(0)).unwrap();
        let before = clock
            .update(&session, &[scene], MediaTime(150_000_000))
            .unwrap();
        assert_eq!(
            clip.sample(before.elapsed(first)).image.pixels(),
            &[0, 255, 0, 255]
        );
        assert_eq!(
            clip.sample(before.elapsed(second)).image.pixels(),
            &[0, 255, 0, 255]
        );
        let authored = session.snapshot();
        assert!(
            !session
                .apply(Command::SetImageSource {
                    source: first,
                    asset
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&authored, &session.snapshot()));
        clock.restart_source(first, MediaTime(150_000_000)).unwrap();
        let restarted = clock
            .update(&session, &[scene], MediaTime(150_000_000))
            .unwrap();
        assert_eq!(restarted.epoch(), before.epoch() + 1);
        assert_eq!(restarted.elapsed(first), MediaTime(0));
        assert_eq!(restarted.elapsed(second), MediaTime(150_000_000));
        assert_eq!(
            clip.sample(restarted.elapsed(first)).image.pixels(),
            &[255, 0, 0, 255]
        );
        assert_eq!(
            clip.sample(restarted.elapsed(second)).image.pixels(),
            &[0, 255, 0, 255]
        );
        let later = clock
            .update(&session, &[scene], MediaTime(175_000_000))
            .unwrap();
        assert_eq!(later.elapsed(first), MediaTime(25_000_000));
        assert_eq!(later.elapsed(second), MediaTime(175_000_000));
        assert_eq!(later.epoch(), restarted.epoch());
        assert_eq!(session.snapshot().revision(), authored.revision());
    }
    #[test]
    fn targeted_restart_inactive_backward_and_overflow_have_explicit_atomic_outcomes() {
        let (session, _, source) = fixture();
        let scene = session.preview();
        let mut clock = PlaybackClock::new();
        let before = clock.update(&session, &[scene], MediaTime(100)).unwrap();
        clock.restart_source(Id::new(999), MediaTime(160)).unwrap();
        assert_eq!(clock.epoch, before.epoch());
        assert_eq!(clock.last_now, Some(MediaTime(160)));
        // This update is backward relative to the inactive restart. Resetting
        // all starts is necessary even though it follows the earlier update.
        let backwards = clock.update(&session, &[scene], MediaTime(150)).unwrap();
        assert_eq!(backwards.elapsed(source), MediaTime(0));
        assert!(backwards.epoch() > before.epoch());
        clock.restart_source(source, MediaTime(140)).unwrap();
        let after = clock.update(&session, &[scene], MediaTime(145)).unwrap();
        assert_eq!(after.elapsed(source), MediaTime(5));
        let previous_start = clock.active[&source].started;
        let previous_now = clock.last_now;
        clock.epoch = u64::MAX;
        assert_eq!(
            clock.restart_source(source, MediaTime(200)),
            Err(PlaybackError::IdentityExhausted)
        );
        assert_eq!(clock.active[&source].started, previous_start);
        assert_eq!(clock.last_now, previous_now);
        assert_eq!(
            clock.restart_source(source, MediaTime(100)),
            Err(PlaybackError::IdentityExhausted)
        );
        assert_eq!(clock.active[&source].started, previous_start);
        assert_eq!(clock.last_now, previous_now);
    }
}
