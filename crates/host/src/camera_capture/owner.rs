//! One lazy persistent owner. SDK resources never cross to frontends; retained
//! output pixels have a worker-owned reference until their readers retire.
use super::*;
use skylive_core::{Id, Session};
use skylive_media::{MediaTime, SharedVideoFrame};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};
static NEXT_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
const MAX_REFERENCES: usize = 3 * skylive_core::MAX_SOURCES;
type Bindings = Vec<(Id, VideoCaptureBinding)>;
type DelaySettings = Vec<(Id, Vec<(skylive_core::VideoFilterId, u16)>)>;
type DelayUpstream = Vec<(Id,Vec<skylive_core::VideoFilter>)>;
#[derive(Clone)]
struct CameraMaskPublication {binding_epoch:u64,snapshot:crate::playback::CameraMaskSnapshot,live:Arc<std::sync::atomic::AtomicBool>,scroll_live:Arc<std::sync::atomic::AtomicBool>}

#[derive(Clone)]
struct EmbeddedRoute {
    sink: crate::audio_capture::EmbeddedPcmSink,
    program: crate::audio_capture::ProgramCaptureKey,
    correlation: skylive_media::QpcMediaCorrelation,
}
type ResourceKey = (VideoCaptureTarget, VideoCaptureMode, skylive_core::CameraFpsPolicy, skylive_core::CameraModePolicy, bool);
fn physical_target(target: &VideoCaptureTarget) -> VideoCaptureTarget {
    match target {
        VideoCaptureTarget::Windows { symbolic_link } => VideoCaptureTarget::Windows { symbolic_link: symbolic_link.to_ascii_lowercase() },
        _ => target.clone(),
    }
}
fn resource(binding: &VideoCaptureBinding) -> ResourceKey {
    (binding.target().clone(), binding.mode(), binding.fps_policy(), binding.mode_policy(), binding.embedded_audio())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraRequestKey {
    pub instance: u64,
    pub session: u64,
    pub revision: u64,
    pub epoch: u64,
    pub serial: u64,
}
#[derive(Clone, Debug)]
pub struct CameraFrame {
    pub binding: Arc<VideoCaptureBinding>,
    pub(crate) async_delay: Option<crate::capture::AsyncDelayReceipt>,
    /// Native mode selected for this stream. With an automatic FPS policy it
    /// can differ from the authored mode retained in `binding`.
    pub actual_mode: VideoCaptureMode,
    /// The last image is retained while the owner changes capture scope; its
    /// mode describes that earlier stream until a new sample is published.
    pub carried: bool,
    pub sequence: u64,
    pub stream_generation: u64,
    /// Advances on a native timestamp discontinuity, including one carried
    /// across callbacks without an image. Scoped by stream_generation.
    pub clock_epoch: u64,
    pub device_qpc_ns: Option<u64>,
    pub directshow_clock: Option<super::DirectShowClockSnapshot>,
    pub requested_at: MediaTime,
    pub presentation_ns: i128,
    pub dropped_frames: u64,
    pub image: SharedVideoFrame,
    /// Independent native sample for filter-draft preview; it owns its camera
    /// resident lease and never substitutes for a delayed output proof.
    pub(crate) draft_raw:Option<Arc<CameraFrame>>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CameraProblem {
    pub kind: crate::resource_issue::ResourceIssueKind,
    pub source: Id,
    pub code: &'static str,
    pub detail: Arc<str>,
}
#[derive(Clone, Debug)]
pub struct CameraSnapshot {
    instance: u64,
    session: u64,
    epoch: u64,
    generation: u64,
    frames: HashMap<Id, CameraFrame>,
    draft_frames: HashMap<Id, CameraFrame>,
    problems: Vec<CameraProblem>,
}
impl CameraSnapshot {
    pub fn empty(session: u64) -> Self {
        Self {
            instance: 0,
            session,
            epoch: 0,
            generation: 0,
            frames: HashMap::new(),
            draft_frames: HashMap::new(),
            problems: vec![],
        }
    }
    pub fn instance(&self) -> u64 {
        self.instance
    }
    pub fn session(&self) -> u64 {
        self.session
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn frame(&self, source: Id) -> Option<&CameraFrame> {
        self.frames.get(&source)
    }
    pub fn frames(&self) -> impl Iterator<Item = (Id, &CameraFrame)> {
        self.frames.iter().map(|(id, frame)| (*id, frame))
    }
    pub(crate) fn draft_frames(&self) -> impl Iterator<Item = (Id, &CameraFrame)> {
        self.draft_frames.iter().map(|(id, frame)| (*id, frame))
    }
    pub fn problems(&self) -> &[CameraProblem] {
        &self.problems
    }
}
#[derive(Debug)]
pub enum CameraEvent {
    Controls { key: CameraRequestKey, result: Result<ControlCatalog, CameraError> },
    ControlSet { key: CameraRequestKey, lease: ControlLease, receipt: ControlReceipt },
    Devices {
        key: CameraRequestKey,
        result: Result<Vec<CameraDeviceInfo>, CameraError>,
    },
    Modes {
        key: CameraRequestKey,
        target: VideoCaptureTarget,
        result: Result<Vec<CameraModeInfo>, CameraError>,
    },
    /// The capture pin default, before OBS's capability ranking.
    DirectShowDefault { key: CameraRequestKey, target: VideoCaptureTarget, result: Result<CameraModeInfo, CameraError> },
    DirectShowPreferred { key: CameraRequestKey, target: VideoCaptureTarget, result: Result<CameraModeInfo, CameraError> },
    DirectShowVideoOnly { key: CameraRequestKey, target: VideoCaptureTarget, result: Result<bool, CameraError> },
    Activated {
        key: CameraRequestKey,
        result: Result<(), CameraError>,
    },
    Stopped {
        key: CameraRequestKey,
    },
    Frames {
        generation: u64,
    },
}
enum Work {
    AvActivation {key:CameraRequestKey,bindings:Bindings,delays:DelaySettings,upstream:DelayUpstream,changes:Arc<[crate::camera_activation::SourceEpoch]>,now:MediaTime,output_fps:(u32,u32),watch:crate::camera_activation::LegWatch},
    Controls { key: CameraRequestKey, lease: ControlLease },
    SetControl { key: CameraRequestKey, lease: ControlLease, change: ControlChange, gate: ControlWriteGate },
    Devices(CameraRequestKey),
    Modes(CameraRequestKey, VideoCaptureTarget),
    DirectShowDefault(CameraRequestKey, VideoCaptureTarget),
    DirectShowPreferred(CameraRequestKey, VideoCaptureTarget),
    DirectShowVideoOnly(CameraRequestKey, VideoCaptureTarget),
    Activate {
        key: CameraRequestKey,
        bindings: Bindings,
        delays: DelaySettings,
        upstream: DelayUpstream,
        now: MediaTime,
        output_fps: (u32,u32),
        restart: Option<ResourceKey>,
    },
    Stop(CameraRequestKey),
}
pub struct CameraOwner {
    instance: u64,
    armed: bool,
    stop_pending: bool,
    sender: Option<mpsc::SyncSender<Work>>,
    receiver: Option<mpsc::Receiver<CameraEvent>>,
    frames: Option<mpsc::Receiver<Arc<CameraSnapshot>>>,
    thread: Option<thread::JoinHandle<()>>,
    capabilities: CameraCapabilities,
    limits: CameraLimits,
    busy: bool,
    serial: u64,
    epoch: u64,
    delay_epoch: Arc<std::sync::atomic::AtomicU64>,
    mask_publication:Arc<std::sync::Mutex<Option<CameraMaskPublication>>>,
    mask_owned:Option<CameraMaskPublication>,
    session: u64,
    bindings: Bindings,
    delay_settings: DelaySettings,
    // Effects before the final async delay belong to retained pixels. Changes
    // retire their capture epoch even when delay durations stay unchanged.
    delay_upstream: DelayUpstream,
    output_fps: (u32,u32),
    paused: HashMap<Id,VideoCaptureBinding>,
    activation:Arc<crate::camera_activation::Snapshot>,
    av_request:Option<(CameraRequestKey,Arc<[crate::camera_activation::SourceEpoch]>)>,
    last_av_key:Option<CameraRequestKey>,
    active_epoch: Option<u64>,
    latest: Arc<CameraSnapshot>,
    last_requested: Option<MediaTime>,
    control_catalog: Option<ControlCatalog>,
    control_gate: Option<ControlWriteGate>,
    embedded_route: Arc<std::sync::Mutex<Option<EmbeddedRoute>>>,
}
impl CameraOwner {
    pub(crate) fn delay_epoch_token(&self)->Arc<std::sync::atomic::AtomicU64> { self.delay_epoch.clone() }
    pub fn native() -> Result<Self, CameraError> {
        Self::new(
            native::factory(),
            native::capabilities(),
            CameraLimits::default(),
        )
    }
    pub fn new(
        factory: CameraBackendFactory,
        capabilities: CameraCapabilities,
        limits: CameraLimits,
    ) -> Result<Self, CameraError> {
        limits.validate()?;
        let instance = NEXT_OWNER
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |v| v.checked_add(1),
            )
            .map_err(|_| CameraError::IdentityExhausted)?;
        let (sender, input) = mpsc::sync_channel(1);
        let (output, receiver) = mpsc::sync_channel(1);
        let (frames, snapshots) = mpsc::sync_channel(1);
        let embedded_route=Arc::new(std::sync::Mutex::new(None));
        let worker_route=embedded_route.clone();
        let mask_publication=Arc::new(std::sync::Mutex::new(None));
        let worker_masks=mask_publication.clone();
        let handle = thread::Builder::new()
            .name("skylive-camera".into())
            .spawn(move || run(factory, input, output, frames, limits, worker_route,worker_masks))
            .map_err(|e| CameraError::Native {
                operation: "camera worker",
                detail: e.to_string(),
            })?;
        Ok(Self {
            instance,
            armed: false,
            stop_pending: false,
            sender: Some(sender),
            receiver: Some(receiver),
            frames: Some(snapshots),
            thread: Some(handle),
            capabilities,
            limits,
            busy: false,
            serial: 0,
            epoch: 0,
            delay_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            mask_publication,
            mask_owned:None,
            session: 0,
            bindings: vec![],
            delay_settings: vec![],
            delay_upstream: vec![],
            output_fps: (0,0),
            paused: HashMap::new(),
            activation:Arc::new(Default::default()),
            av_request:None,
            last_av_key:None,
            active_epoch: None,
            latest: Arc::new(CameraSnapshot::empty(0)),
            last_requested: None,
            control_catalog: None,
            control_gate: None,
            embedded_route,
        })
    }
    /// Publish a bounded immutable playback/resource snapshot for the capture
    /// owner. Capture code uses try_lock and never waits for UI/render work.
    pub fn set_mask_snapshot(&mut self,session:&Session,snapshot:crate::playback::CameraMaskSnapshot)->Result<(),CameraError> {
        if session.is_closed() || self.sender.is_none(){return Err(CameraError::Closed);}
        if session.identity()!=self.session || snapshot.session!=self.session {return Err(CameraError::ForeignSession);}
        if snapshot.revision!=session.snapshot().revision(){return Err(CameraError::InvalidFrame("camera mask snapshot revision"));}
        // Retire the prior guard before attempting the nonblocking publish.
        // A busy capture lock must not leave an old seek/revision live.
        let unchanged=self.mask_owned.as_ref().is_some_and(|old|old.binding_epoch==self.epoch
            && old.live.load(std::sync::atomic::Ordering::Acquire)
            && old.snapshot.session==snapshot.session && old.snapshot.revision==snapshot.revision
            && old.snapshot.playback_epoch==snapshot.playback_epoch
            && old.snapshot.clock.scope()==snapshot.clock.scope()
            && old.snapshot.clock.revision()==snapshot.clock.revision()
            && old.snapshot.clock.files_domain==snapshot.clock.files_domain
            && old.snapshot.clock.same_files(&snapshot.clock)
            && Arc::ptr_eq(&old.snapshot.versions,&snapshot.versions));
        let live=if unchanged {self.mask_owned.as_ref().unwrap().live.clone()} else {
            if let Some(old)=self.mask_owned.as_ref(){old.live.store(false,std::sync::atomic::Ordering::Release);}
            Arc::new(std::sync::atomic::AtomicBool::new(true))
        };
        let same_scroll=self.mask_owned.as_ref().is_some_and(|old|unchanged
            &&old.scroll_live.load(std::sync::atomic::Ordering::Acquire)&&old.snapshot.same_scroll_scope(&snapshot));
        let scroll_live=if same_scroll {self.mask_owned.as_ref().unwrap().scroll_live.clone()} else {
            if let Some(old)=self.mask_owned.as_ref(){old.scroll_live.store(false,std::sync::atomic::Ordering::Release);}
            Arc::new(std::sync::atomic::AtomicBool::new(true))
        };
        let publication=CameraMaskPublication {binding_epoch:self.epoch,snapshot,live,scroll_live};
        self.mask_owned=Some(publication.clone());
        let mut slot=self.mask_publication.try_lock().map_err(|_|CameraError::Busy)?;
        *slot=Some(publication);
        Ok(())
    }
    /// Publish the currently active Program identity and clock correlation.
    /// Missing clock/Program disables audio admission, never the camera video.
    pub fn set_embedded_audio_route(&mut self,sink:crate::audio_capture::EmbeddedPcmSink,
        program:Option<crate::audio_capture::ProgramCaptureKey>,
        correlation:Option<skylive_media::QpcMediaCorrelation>) -> Result<(),CameraError> {
        let route=program.zip(correlation).map(|(program,correlation)|EmbeddedRoute {sink,program,correlation});
        *self.embedded_route.lock().map_err(|_|CameraError::Unavailable("embedded camera audio route poisoned"))?=route;
        Ok(())
    }
    /// Cancel a queued external write, or refuse publication until a native
    /// Set/readback in progress finishes. Hardware values are never rolled back.
    pub fn before_session_change(&mut self) -> Result<(), CameraError> {
        if let Some(gate) = &self.control_gate { gate.before_change()?; }
        self.control_catalog = None;
        Ok(())
    }
    fn control_lease(&self, session: &Session, source: Id, key: CameraRequestKey) -> Result<ControlLease, CameraError> {
        if !self.armed || self.stop_pending || self.session != session.identity() || self.active_epoch != Some(self.epoch) {
            return Err(CameraError::Unavailable("start camera capture before reading driver controls"));
        }
        if !self.source_enabled(source) {return Err(CameraError::Unavailable("start this camera source before reading driver controls"));}
        let binding = self.bindings.iter().find(|(id,_)| *id == source).map(|(_,b)| b).ok_or(CameraError::DeviceNotFound)?;
        // Bindings are synchronized by the frontend before admission. Also check
        // the canonical source so a changed source cannot reuse an old handle.
        let model = session.snapshot();
        if !model.source(source).is_some_and(|s| matches!(s.kind(), skylive_core::SourceKind::VideoCapture {binding: current} if current.same_video(binding))) {
            return Err(CameraError::Unavailable("camera source changed"));
        }
        let frame = self.latest.frame(source).filter(|f| f.binding.as_ref() == binding).ok_or(CameraError::NoFrame)?;
        Ok(ControlLease {key, source, binding: binding.clone(), stream_generation: frame.stream_generation})
    }
    pub fn request_controls(&mut self, session: &Session, source: Id) -> Result<CameraRequestKey, CameraError> {
        let key = self.key(session)?;
        let lease = self.control_lease(session, source, key)?;
        self.send(Work::Controls {key, lease}, key)?;
        self.control_catalog = None;
        Ok(key)
    }
    pub fn request_control_change(&mut self, session: &Session, lease: ControlLease, change: ControlChange) -> Result<CameraRequestKey, CameraError> {
        let key = self.key(session)?;
        let current = self.control_lease(session, lease.source, lease.key)?;
        if current != lease || lease.key.instance != self.instance || lease.key.session != session.identity()
            || lease.key.revision != session.snapshot().revision() || lease.key.epoch != self.epoch {
            return Err(CameraError::Unavailable("driver control query is stale; refresh controls"));
        }
        if !self.control_catalog.as_ref().is_some_and(|c| c.lease == lease && c.query.controls.iter().any(|p| p.id == change.id)) {
            return Err(CameraError::Unavailable("driver did not report this control"));
        }
        let gate = ControlWriteGate::new();
        self.send(Work::SetControl {key, lease, change, gate: gate.clone()}, key)?;
        self.control_gate = Some(gate);
        Ok(key)
    }
    pub fn capabilities(&self) -> CameraCapabilities {
        self.capabilities
    }
    pub fn is_busy(&self) -> bool {
        self.busy
    }
    pub fn snapshot(&self) -> Arc<CameraSnapshot> {
        self.latest.clone()
    }
    pub fn is_active(&self) -> bool {
        self.armed
    }
    pub fn active_source_count(&self) -> usize {
        if !self.armed {return 0;}
        self.bindings.iter().filter(|(id,_)|self.activation.enabled(*id)&&!self.paused.contains_key(id)).count()
    }
    pub fn av_admission_reserved(&self)->bool{self.activation.control_pending()}
    pub fn is_av_activation_key(&self,key:CameraRequestKey)->bool{self.last_av_key==Some(key)||self.av_request.as_ref().is_some_and(|(expected,_)|*expected==key)}
    pub(crate) fn source_referenced(&self,source:Id)->bool {self.bindings.iter().any(|(id,_)|*id==source)||self.paused.contains_key(&source)}
    pub fn source_enabled(&self,source:Id)->bool {
        self.armed && self.activation.enabled(source) && !self.paused.contains_key(&source) && self.bindings.iter().any(|(id,_)|*id==source)
    }
    fn enabled_bindings(&self)->Bindings {
        self.bindings.iter().filter(|(id,_)|self.activation.enabled(*id)&&!self.paused.contains_key(id)).cloned().collect()
    }
    fn carried_snapshot(&self,epoch:u64,excluded:Option<&ResourceKey>)->Arc<CameraSnapshot> {
        let current:HashMap<_,_>=self.bindings.iter().map(|(id,binding)|(*id,binding)).collect();
        let frames=self.latest.frames.iter().filter_map(|(id,frame)| {
            (self.latest.session==self.session && !self.paused.contains_key(id)
                && current.get(id).is_some_and(|binding|*binding==frame.binding.as_ref())
                && !self.delay_settings.iter().any(|(source,_)|source==id)
                && frame.image.direct_arc().is_some()
                && excluded.is_none_or(|key|resource(&frame.binding)!=*key)).then(||{
                    let mut frame=frame.clone();frame.carried=true;(*id,frame)
                })
        }).collect();
        // Old frames retain their actual acquisition timestamp. Reusing metadata
        // across an epoch never manufactures a fresh frame or native restart.
        Arc::new(CameraSnapshot {instance:self.instance,session:self.session,epoch,generation:self.serial,frames,draft_frames:HashMap::new(),problems:vec![]})
    }
    pub fn last_requested_at(&self) -> Option<MediaTime> {
        self.last_requested
    }
    pub fn instance(&self) -> u64 {
        self.instance
    }
    /// Pure metadata synchronization. No factory or native method runs until an
    /// explicit request_frame; callers include authored hidden-live bindings.
    pub fn synchronize_bindings(
        &mut self,
        session: &Session,
        mut bindings: Bindings,
    ) -> Result<bool, CameraError> {
        if self.sender.is_none() || session.is_closed() {
            return Err(CameraError::Closed);
        }
        for (_, binding) in &mut bindings { *binding = binding.video_only(); }
        validate_bindings(&bindings, self.limits, false)?;
        bindings.sort_by_key(|(id, _)| id.get());
        let snapshot = session.snapshot();
        let mut delay_settings = Vec::new();
        let mut delay_upstream = Vec::new();
        for (id, _) in &bindings {
            let Some(source) = snapshot.source(*id) else { continue; };
            let mut filters = Vec::new();
            for filter in source.video_filters() {
                if filter.enabled && snapshot.preferences().plugin_enabled(filter.plugin_id()) {
                    if let skylive_core::VideoEffect::AsyncVideoDelay { delay_ms } = filter.effect {
                        if delay_ms != 0 {
                            filters.push((filter.id().ok_or(CameraError::InvalidFrame("asynchronous camera filter has no identity"))?, delay_ms));
                        }
                    }
                }
            }
            if !filters.is_empty() {
                let active:Vec<_>=source.video_filters().iter().filter(|filter|
                    filter.enabled && snapshot.preferences().plugin_enabled(filter.plugin_id())).collect();
                if let Some(last)=active.iter().rposition(|filter|matches!(filter.effect,
                    skylive_core::VideoEffect::AsyncVideoDelay {delay_ms:1..=u16::MAX})) {
                    delay_upstream.push((*id,active[..last].iter().map(|filter|(**filter).clone()).collect()));
                }
                delay_settings.push((*id, filters));
            }
        }
        let video = snapshot.video();
        let output_fps = (video.fps_num(),video.fps_den());
        let rate_changed = self.output_fps != output_fps && bindings.iter().any(|(_,binding)|binding.fps_policy()==skylive_core::CameraFpsPolicy::MatchOutput);
        if self.session == session.identity() && self.bindings == bindings && self.delay_settings == delay_settings && self.delay_upstream == delay_upstream && !rate_changed {
            return Ok(false);
        }
        self.before_session_change()?;
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or(CameraError::IdentityExhausted)?;
        if self.session != 0 && self.session != session.identity() {
            self.stop_pending = true;
            self.armed = false;
        }
        if self.session!=session.identity() {self.paused.clear();} else {
            let current=session.snapshot();
            self.paused.retain(|id,old| {
                let binding = bindings.iter().find(|(candidate,_)|candidate==id).map(|(_,binding)|binding)
                    .or_else(|| current.source(*id).and_then(|source| match source.kind() {
                        skylive_core::SourceKind::VideoCapture {binding} => Some(binding), _ => None,
                    }));
                if let Some(binding) = binding.filter(|binding|binding.same_device_mode(old)) {
                    // Presentation/custom audio edits do not undo an explicit
                    // source Pause, even while its showing lease is absent.
                    *old = binding.video_only();
                    true
                } else { false }
            });
        }
        self.session = session.identity();
        self.epoch = epoch;
        self.delay_epoch.store(epoch,std::sync::atomic::Ordering::Release);
        if let Some(old)=self.mask_owned.take(){old.live.store(false,std::sync::atomic::Ordering::Release);old.scroll_live.store(false,std::sync::atomic::Ordering::Release);}
        if let Ok(mut slot)=self.mask_publication.try_lock(){if let Some(old)=slot.take(){old.live.store(false,std::sync::atomic::Ordering::Release);old.scroll_live.store(false,std::sync::atomic::Ordering::Release);}}
        self.bindings = bindings;
        self.delay_settings = delay_settings;
        self.delay_upstream = delay_upstream;
        self.output_fps = output_fps;
        self.active_epoch = None;
        self.last_requested = None;
        self.latest = self.carried_snapshot(epoch,None);
        Ok(true)
    }
    pub fn reset_timing(&mut self, session: &Session) -> Result<(), CameraError> {
        if session.is_closed() || self.sender.is_none() {
            return Err(CameraError::Closed);
        }
        if session.identity() != self.session {
            return Err(CameraError::ForeignSession);
        }
        self.before_session_change()?;
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or(CameraError::IdentityExhausted)?;
        self.epoch = epoch;
        self.delay_epoch.store(epoch,std::sync::atomic::Ordering::Release);
        self.active_epoch = None;
        self.last_requested = None;
        self.latest = Arc::new(CameraSnapshot {
            instance: self.instance,
            session: self.session,
            epoch,
            generation: self.serial,
            frames: HashMap::new(),
            draft_frames: HashMap::new(),
            problems: vec![],
        });
        Ok(())
    }
    fn key(&self, session: &Session) -> Result<CameraRequestKey, CameraError> {
        if session.is_closed() || self.sender.is_none() {
            return Err(CameraError::Closed);
        }
        Ok(CameraRequestKey {
            instance: self.instance,
            session: session.identity(),
            revision: session.snapshot().revision(),
            epoch: self.epoch,
            serial: self
                .serial
                .checked_add(1)
                .ok_or(CameraError::IdentityExhausted)?,
        })
    }
    fn send(&mut self, work: Work, key: CameraRequestKey) -> Result<(), CameraError> {
        if self.busy {
            return Err(CameraError::Busy);
        }
        self.sender
            .as_ref()
            .ok_or(CameraError::Closed)?
            .try_send(work)
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => CameraError::Busy,
                mpsc::TrySendError::Disconnected(_) => CameraError::Disconnected,
            })?;
        self.serial = key.serial;
        self.busy = true;
        Ok(())
    }
    pub(crate) fn set_activation_policy(&mut self,policy:Arc<crate::camera_activation::Snapshot>)->Result<(),CameraError>{
        let changed=self.activation.session()!=policy.session()||self.activation.sources().len()!=policy.sources().len()
            ||self.activation.sources().iter().zip(policy.sources()).any(|(a,b)|a.source!=b.source||a.epoch!=b.epoch||a.enabled!=b.enabled);
        if changed {
            self.before_session_change()?;
            self.epoch=self.epoch.checked_add(1).ok_or(CameraError::IdentityExhausted)?;
            self.delay_epoch.store(self.epoch,std::sync::atomic::Ordering::Release);
            self.active_epoch=None;self.last_requested=None;
        }
        self.activation=policy;
        Ok(())
    }
    pub(crate) fn request_av_activation(&mut self,session:&Session,changes:Arc<[crate::camera_activation::SourceEpoch]>,now:MediaTime,watch:crate::camera_activation::LegWatch)->Result<(),CameraError>{
        if self.busy||self.av_request.is_some()||self.stop_pending {return Err(CameraError::Busy);}
        if session.identity()!=self.session {return Err(CameraError::ForeignSession);}
        let model=session.snapshot();
        if changes.is_empty()||changes.len()>skylive_core::MAX_SOURCES||changes.iter().any(|entry|!model.source(entry.source).is_some_and(|s|matches!(s.kind(),skylive_core::SourceKind::VideoCapture {binding} if binding.same_av_activation(&entry.binding)))){return Err(CameraError::InvalidFrame("camera AV source binding changed"));}
        self.before_session_change()?;
        let mut paused=self.paused.clone();
        let enabling=changes.iter().any(|entry|entry.enabled);
        if enabling&&!self.armed {for (id,binding) in &self.bindings {paused.insert(*id,binding.clone());}}
        for entry in changes.iter(){if entry.enabled {paused.remove(&entry.source);}else{paused.insert(entry.source,entry.binding.video_only());}}
        let bindings:Bindings=self.bindings.iter().filter(|(id,_)|self.activation.enabled(*id)&&!paused.contains_key(id)).cloned().collect();
        validate_bindings(&bindings,self.limits,false)?;
        let epoch=self.epoch.checked_add(1).ok_or(CameraError::IdentityExhausted)?;
        let mut key=self.key(session)?;key.epoch=epoch;
        self.send(Work::AvActivation {key,bindings,delays:self.delay_settings.clone(),upstream:self.delay_upstream.clone(),changes:changes.clone(),now,output_fps:self.output_fps,watch},key)?;
        self.paused=paused;self.epoch=epoch;self.delay_epoch.store(epoch,std::sync::atomic::Ordering::Release);self.active_epoch=None;self.last_requested=Some(now);
        if enabling {self.armed=!self.bindings.is_empty();}else if self.enabled_bindings().is_empty(){self.armed=false;}
        self.latest=self.carried_snapshot(epoch,None);self.av_request=Some((key,changes));
        Ok(())
    }
    pub fn request_devices(&mut self, session: &Session) -> Result<CameraRequestKey, CameraError> {
        let key = self.key(session)?;
        self.send(Work::Devices(key), key)?;
        Ok(key)
    }
    pub fn request_modes(
        &mut self,
        session: &Session,
        target: VideoCaptureTarget,
    ) -> Result<CameraRequestKey, CameraError> {
        let validation_mode =
            VideoCaptureMode::new(1, 1, 1, 1, skylive_core::VideoPixelFormat::Rgb32)
                .map_err(|_| CameraError::InvalidFrame("camera validation mode"))?;
        let target = VideoCaptureBinding::new(target, validation_mode, false)
            .map_err(|_| CameraError::InvalidFrame("camera mode-query identity"))?
            .target()
            .clone();
        let key = self.key(session)?;
        self.send(Work::Modes(key, target), key)?;
        Ok(key)
    }
    /// Runs on the same owner as capture. A busy physical device is reported
    /// without stopping it or opening a second concurrent native handle.
    pub fn request_directshow_default(&mut self, session:&Session, target:VideoCaptureTarget) -> Result<CameraRequestKey,CameraError> {
        let validation_mode=VideoCaptureMode::new(1,1,1,1,skylive_core::VideoPixelFormat::Rgb32)
            .map_err(|_|CameraError::InvalidFrame("camera validation mode"))?;
        let target=VideoCaptureBinding::new(target,validation_mode,false)
            .map_err(|_|CameraError::InvalidFrame("DirectShow default-query identity"))?.target().clone();
        let key=self.key(session)?;
        self.send(Work::DirectShowDefault(key,target),key)?;
        Ok(key)
    }
    /// Resolve the OBS-style DirectShow Preferred candidate without opening a
    /// second camera handle or changing the active capture configuration.
    pub fn request_directshow_preferred(&mut self,session:&Session,target:VideoCaptureTarget)->Result<CameraRequestKey,CameraError> {
        let validation_mode=VideoCaptureMode::new(1,1,1,1,skylive_core::VideoPixelFormat::Rgb32)
            .map_err(|_|CameraError::InvalidFrame("camera validation mode"))?;
        let target=VideoCaptureBinding::new(target,validation_mode,false)
            .map_err(|_|CameraError::InvalidFrame("DirectShow preferred-query identity"))?.target().clone();
        let key=self.key(session)?;
        self.send(Work::DirectShowPreferred(key,target),key)?;
        Ok(key)
    }
    pub fn request_directshow_video_only(&mut self,session:&Session,target:VideoCaptureTarget)->Result<CameraRequestKey,CameraError> {
        let validation=VideoCaptureMode::new(1,1,1,1,skylive_core::VideoPixelFormat::Rgb32).map_err(|_|CameraError::InvalidFrame("camera validation mode"))?;
        let target=VideoCaptureBinding::new(target,validation,false).map_err(|_|CameraError::InvalidFrame("DirectShow camera identity"))?.target().clone();
        let key=self.key(session)?;self.send(Work::DirectShowVideoOnly(key,target),key)?;Ok(key)
    }
    pub fn request_frame(
        &mut self,
        session: &Session,
        now: MediaTime,
    ) -> Result<bool, CameraError> {
        if session.is_closed() || self.sender.is_none() {
            return Err(CameraError::Closed);
        }
        if self.stop_pending {
            return Err(CameraError::Busy);
        }
        if session.identity() != self.session {
            return Err(CameraError::ForeignSession);
        }
        if self.last_requested.is_some_and(|old| now < old) {
            return Err(CameraError::TimeDiscontinuity);
        }
        now.0
            .checked_add(1_000_000_000)
            .ok_or(CameraError::TimeOverflow)?;
        if self.bindings.is_empty() && !self.armed {
            return Ok(false);
        }
        if self.active_epoch == Some(self.epoch) {
            self.last_requested = Some(now);
            return Ok(false);
        }
        let key = self.key(session)?;
        let bindings=self.enabled_bindings();
        validate_bindings(&bindings,self.limits,false)?;
        self.send(Work::Activate {key,bindings,delays:self.delay_settings.clone(),upstream:self.delay_upstream.clone(),now,output_fps:self.output_fps,restart:None},key)?;
        self.last_requested = Some(now);
        self.armed = true;
        Ok(true)
    }
    pub fn request_start_source(&mut self,session:&Session,source:Id,now:MediaTime)->Result<bool,CameraError> {
        self.request_source_state(session,source,now,true,false)
    }
    pub fn request_pause_source(&mut self,session:&Session,source:Id,now:MediaTime)->Result<bool,CameraError> {
        self.request_source_state(session,source,now,false,false)
    }
    pub fn request_restart(&mut self,session:&Session,source:Id,now:MediaTime)->Result<bool,CameraError> {
        self.request_source_state(session,source,now,true,true)
    }
    fn request_source_state(&mut self,session:&Session,source:Id,now:MediaTime,enabled:bool,restart:bool)->Result<bool,CameraError> {
        if self.busy || self.stop_pending {return Err(CameraError::Busy);}
        if session.is_closed() || self.sender.is_none() {return Err(CameraError::Closed);}
        if self.session!=session.identity() {return Err(CameraError::ForeignSession);}
        let binding=self.bindings.iter().find(|(id,_)|*id==source).map(|(_,binding)|binding.clone()).ok_or(CameraError::Unavailable("show this camera source in an active scene before starting capture"))?;
        if !session.snapshot().source(source).is_some_and(|item|matches!(item.kind(),skylive_core::SourceKind::VideoCapture {binding:current} if current.same_video(&binding))) {
            return Err(CameraError::Unavailable("camera source changed"));
        }
        if !restart && self.source_enabled(source)==enabled {return Ok(false);}
        now.0.checked_add(1_000_000_000).ok_or(CameraError::TimeOverflow)?;
        self.before_session_change()?;
        let mut paused=self.paused.clone();
        // Starting one source after Stop All does not resurrect other devices.
        if enabled && !self.armed {for (id,binding) in &self.bindings {paused.insert(*id,binding.clone());}}
        if enabled {paused.remove(&source);} else {paused.insert(source,binding.clone());}
        let bindings:Bindings=self.bindings.iter().filter(|(id,_)|self.activation.enabled(*id)&&!paused.contains_key(id)).cloned().collect();
        validate_bindings(&bindings,self.limits,false)?;
        let epoch=self.epoch.checked_add(1).ok_or(CameraError::IdentityExhausted)?;
        let mut key=self.key(session)?;key.epoch=epoch;
        let restart=restart.then(||resource(&binding));
        self.send(Work::Activate {key,bindings,delays:self.delay_settings.clone(),upstream:self.delay_upstream.clone(),now,output_fps:self.output_fps,restart:restart.clone()},key)?;
        self.paused=paused;self.epoch=epoch;self.delay_epoch.store(epoch,std::sync::atomic::Ordering::Release);self.active_epoch=None;self.last_requested=Some(now);self.armed=true;
        self.latest=self.carried_snapshot(epoch,restart.as_ref());
        Ok(true)
    }
    pub fn request_stop(&mut self, session: &Session) -> Result<CameraRequestKey, CameraError> {
        self.before_session_change()?;
        let mut key = self.key(session)?;
        key.epoch = self
            .epoch
            .checked_add(1)
            .ok_or(CameraError::IdentityExhausted)?;
        self.send(Work::Stop(key), key)?;
        self.epoch = key.epoch;
        self.delay_epoch.store(key.epoch,std::sync::atomic::Ordering::Release);
        self.active_epoch = None;
        self.armed = false;
        self.stop_pending = false;
        self.latest = Arc::new(CameraSnapshot {
            instance: self.instance,
            session: session.identity(),
            epoch: key.epoch,
            generation: key.serial,
            frames: HashMap::new(),
            draft_frames: HashMap::new(),
            problems: vec![],
        });
        Ok(key)
    }
    pub fn poll(&mut self, session: &Session) -> Result<Option<CameraEvent>, CameraError> {
        if session.is_closed() {
            self.close();
            return Err(CameraError::Closed);
        }
        if self.sender.is_none() {
            return Err(CameraError::Closed);
        }
        let mut event = None;
        match self
            .receiver
            .as_ref()
            .ok_or(CameraError::Closed)?
            .try_recv()
        {
            Ok(reply) => {
                self.busy = false;
                let key = match &reply {
                    CameraEvent::Controls { key, .. }
                    | CameraEvent::ControlSet { key, .. }
                    | CameraEvent::Devices { key, .. }
                    | CameraEvent::Modes { key, .. }
                    | CameraEvent::DirectShowDefault { key, .. }
                    | CameraEvent::DirectShowPreferred { key, .. }
                    | CameraEvent::DirectShowVideoOnly { key, .. }
                    | CameraEvent::Activated { key, .. }
                    | CameraEvent::Stopped { key } => *key,
                    CameraEvent::Frames { .. } => unreachable!(),
                };
                if let Some((expected,changes))=&self.av_request {
                    if *expected==key {
                        self.last_av_key=Some(key);
                        if matches!(&reply,CameraEvent::Activated {result:Err(_),..}) {for entry in changes.iter().filter(|entry|entry.enabled){self.paused.insert(entry.source,entry.binding.video_only());}}
                        self.av_request=None;
                    }
                }
                if matches!(&reply, CameraEvent::ControlSet { .. }) { self.control_gate = None; }
                if let CameraEvent::Controls { result: Ok(catalog), .. } = &reply {
                    if key.instance == self.instance && key.session == session.identity()
                        && key.revision == session.snapshot().revision() && key.epoch == self.epoch {
                        self.control_catalog = Some(catalog.clone());
                    }
                }
                if key.instance == self.instance && key.session == session.identity() {
                    match &reply {
                        CameraEvent::Activated { .. } if key.epoch == self.epoch => {
                            self.active_epoch = Some(key.epoch)
                        }
                        CameraEvent::Stopped { .. } => self.active_epoch = None,
                        _ => {}
                    }
                    if matches!(
                        reply,
                        CameraEvent::Activated { .. } | CameraEvent::Stopped { .. } | CameraEvent::ControlSet { .. }
                    ) || key.revision == session.snapshot().revision()
                    {
                        event = Some(reply);
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => return Err(CameraError::Disconnected),
        }
        let frames = self.frames.as_ref().ok_or(CameraError::Closed)?;
        while let Ok(snapshot) = frames.try_recv() {
            if snapshot.instance == self.instance
                && snapshot.session == session.identity()
                && snapshot.session == self.session
                && snapshot.epoch == self.epoch
            {
                let generation = snapshot.generation;
                self.latest = snapshot;
                if event.is_none() {
                    event = Some(CameraEvent::Frames { generation });
                }
            }
        }
        if self.stop_pending && !self.busy {
            self.request_stop(session)?;
        }
        Ok(event)
    }
    pub fn close(&mut self) {
        if let Ok(mut route) = self.embedded_route.lock() { *route = None; }
        self.delay_epoch.store(0,std::sync::atomic::Ordering::Release);
        if let Some(old)=self.mask_owned.take(){old.live.store(false,std::sync::atomic::Ordering::Release);old.scroll_live.store(false,std::sync::atomic::Ordering::Release);}
        if let Ok(mut slot)=self.mask_publication.try_lock(){if let Some(old)=slot.take(){old.live.store(false,std::sync::atomic::Ordering::Release);old.scroll_live.store(false,std::sync::atomic::Ordering::Release);}}
        if let Some(gate) = &self.control_gate { gate.cancel_queued(); }
        self.control_catalog = None;
        self.latest = Arc::new(CameraSnapshot::empty(0));
        self.bindings.clear();
        self.delay_settings.clear();
        self.delay_upstream.clear();
        self.paused.clear();
        self.receiver.take();
        self.frames.take();
        self.sender.take();
        self.active_epoch = None;
        self.armed = false;
        self.stop_pending = false;
        self.av_request=None;
    }
    pub fn join(&mut self) -> Result<(), CameraError> {
        self.close();
        if let Some(handle) = self.thread.take() {
            handle.join().map_err(|_| CameraError::Native {
                operation: "camera worker",
                detail: "worker panicked".into(),
            })?;
        }
        self.busy = false;
        Ok(())
    }
}
impl Drop for CameraOwner {
    fn drop(&mut self) {
        self.close();
    }
}
fn validate_bindings(
    bindings: &Bindings,
    limits: CameraLimits,
    hardware: bool,
) -> Result<(), CameraError> {
    if bindings.len() > MAX_REFERENCES {
        return Err(CameraError::BudgetExceeded("camera reference count"));
    }
    let mut sources = HashSet::new();
    let mut keys = HashSet::new();
    let mut targets = HashMap::new();
    let mut pixels = 0usize;
    let mut native_bytes = 0usize;
    for (id, binding) in bindings {
        if id.get() == 0 {
            return Err(CameraError::InvalidFrame("zero camera source identity"));
        }
        if !sources.insert(*id) {
            return Err(CameraError::InvalidFrame(
                "duplicate camera source reference",
            ));
        }
        let key = resource(binding);
        if hardware {
            if targets
                .insert(binding.target(), binding.mode())
                .is_some_and(|old| old != binding.mode())
            {
                return Err(CameraError::ConflictingModes);
            }
            if keys.insert(key) {
                native_bytes = native_bytes
                    .checked_add(checked_mode_raw_bytes(binding.mode(),limits)?)
                    .ok_or(CameraError::BudgetExceeded("camera aggregate native bytes"))?;
                pixels = pixels
                    .checked_add(checked_frame_bytes(
                        binding.width(),
                        binding.height(),
                        limits,
                    )?)
                    .ok_or(CameraError::BudgetExceeded("camera aggregate bytes"))?;
            }
        }
    }
    if hardware
        && (keys.len() > limits.max_streams
            || pixels > limits.max_total_frame_bytes
            || native_bytes > limits.max_raw_bytes)
    {
        return Err(CameraError::BudgetExceeded(
            "active camera stream/raw/output budget",
        ));
    }
    Ok(())
}

struct StreamState {
    stream: Box<dyn NativeCameraStream>,
    actual_mode: VideoCaptureMode,
    generation: u64,
    clock_epoch: u64,
    last_presentation_ns: Option<i128>,
    clock_discontinuity_pending: bool,
    latest: Option<CameraFrame>,
    // Opposite row orientation of the SAME sample, only when demanded by
    // another source sharing this physical target+mode. Charged in resident.
    opposite: Option<SharedVideoFrame>,
    error: Option<CameraError>,
    bytes: usize,
    raw_bytes: usize,
    audio_sequence:u64,
    audio_discontinuity:bool,
}
impl StreamState {
    fn image_for(&self, flip: bool) -> Option<&SharedVideoFrame> {
        let frame = self.latest.as_ref()?;
        if frame.binding.flip_vertically() == flip { Some(&frame.image) }
        else { self.opposite.as_ref() }
    }
    fn frame_for(&self, binding: &Arc<VideoCaptureBinding>) -> Option<CameraFrame> {
        let image = self.image_for(binding.flip_vertically())?.clone();
        let mut frame = self.latest.as_ref()?.clone();
        frame.binding = binding.clone();
        frame.image = image;
        Some(frame)
    }
}
struct Closing {
    sources:Vec<Id>,
    session:u64,
    problem:Option<CameraError>,
    key: ResourceKey,
    state: StreamState,
}
struct AvOperation {reply_sent:bool,key:CameraRequestKey,changes:Arc<[crate::camera_activation::SourceEpoch]>,watch:crate::camera_activation::LegWatch,floor:u64,cancelled:bool,ended:bool}
struct Runtime {
    factory: Option<CameraBackendFactory>,
    backend: Option<Box<dyn NativeCameraBackend>>,
    backend_error: Option<CameraError>,
    streams: HashMap<ResourceKey, StreamState>,
    closing: Vec<Closing>,
    bindings: HashMap<ResourceKey, Vec<(Id, Arc<VideoCaptureBinding>)>>,
    delay_settings: DelaySettings,
    delay_upstream: DelayUpstream,
    mask_publication:Arc<std::sync::Mutex<Option<CameraMaskPublication>>>,
    stage_budget: skylive_media::AsyncDelayBudget,
    stage_workers: HashMap<Id,(super::async_delay::Scope,super::staged_delay_worker::Worker,u64)>,
    stage_pending:HashMap<Id,(CameraFrame,Instant)>,
    stage_timeline:HashMap<Id,Arc<std::sync::atomic::AtomicBool>>,
    stage_timed_masks:Option<(Arc<std::sync::Mutex<crate::mask_live_versions::Book>>,Arc<std::sync::Mutex<crate::video_filters::StagedMaskStore>>)>,
    stage_masks: Arc<std::sync::Mutex<crate::video_filters::StagedMaskStore>>,
    stage_luts:Arc<std::sync::Mutex<crate::video_filters::StagedLutStore>>,
    delays: super::async_delay::SourceDelays,
    delayed_frames: HashMap<Id, (super::async_delay::Scope, CameraFrame)>,
    delay_processed: HashMap<Id, (super::async_delay::Scope, u64)>,
    delay_histories: HashMap<Id, (super::async_delay::Scope, u64, Arc<std::sync::atomic::AtomicBool>)>,
    delay_history_serial: u64,
    output_fps: (u32,u32),
    dirty: bool,
    scope: Option<(CameraRequestKey, MediaTime, Instant)>,
    stop: Option<CameraRequestKey>,
    av:Option<AvOperation>,
    last_av:Option<AvOperation>,
    generation: u64,
    sequence: u64,
    publication: u64,
    resident: Vec<Arc<RgbaFrame>>,
    retry: HashMap<ResourceKey, (Instant, CameraError)>,
    blocked: HashMap<ResourceKey,CameraError>,
    embedded_route:Arc<std::sync::Mutex<Option<EmbeddedRoute>>>,
}
impl Runtime {
    /// Read only an exactly time-matched playback snapshot. A later capture
    /// sample cannot extrapolate it across an unseen seek or resource change.
    /// The retained snapshot and version book carry their existing permits.
    fn mask_snapshot_at(&self,key:CameraRequestKey,at:MediaTime)
        ->Option<(crate::mask_animation_clock::Snapshot,Arc<std::sync::Mutex<crate::mask_live_versions::Book>>)> {
        let publication=self.mask_publication.try_lock().ok()?;
        let value=publication.as_ref()?;
        if value.binding_epoch!=key.epoch || value.snapshot.session!=key.session
            || value.snapshot.clock.now()!=at {return None;}
        Some((value.snapshot.clock.clone(),value.snapshot.versions.clone()))
    }
    /// Map this source observation only through an admitted QPC/media
    /// correlation; DirectShow's interval remains an interval. A later
    /// playback publication must cover its complete upper bound.
    fn mask_snapshot_window(&self,key:CameraRequestKey,frame:&CameraFrame)
        ->Option<(crate::mask_animation_clock::Snapshot,Arc<std::sync::Mutex<crate::mask_live_versions::Book>>,skylive_media::MediaTimeBounds,Arc<std::sync::atomic::AtomicBool>)> {
        let (qpc_first,qpc_last)=frame.device_qpc_ns.map(|qpc|(qpc,qpc))
            .or_else(||frame.directshow_clock?.qpc_interval(frame.presentation_ns))?;
        let publication=self.mask_publication.try_lock().ok()?;
        let value=publication.as_ref()?;
        if value.binding_epoch!=key.epoch || value.snapshot.session!=key.session
            || !value.live.load(std::sync::atomic::Ordering::Acquire) {return None;}
        let correlation=value.snapshot.correlation?;
        let first=correlation.media_bounds(key.session,value.snapshot.playback_epoch,qpc_first)?;
        let last=correlation.media_bounds(key.session,value.snapshot.playback_epoch,qpc_last)?;
        let bounds=skylive_media::MediaTimeBounds {earliest:first.earliest,latest:last.latest};
        if bounds.earliest.0>bounds.latest.0 || bounds.latest.0>value.snapshot.clock.now().0 {return None;}
        Some((value.snapshot.clock.clone(),value.snapshot.versions.clone(),bounds,value.live.clone()))
    }
    /// Resolve the estimated midpoint phase for a narrow uncertainty interval.
    /// Consumers must keep the returned guard through publication; timing
    /// uncertainty is at most 5 ms and is never represented as an exact time.
    fn scroll_snapshot_window(&self,key:CameraRequestKey,frame:&CameraFrame,source:Id,filters:&[skylive_core::VideoFilter])
        ->Option<(crate::scroll_animation::Context,Arc<std::sync::atomic::AtomicBool>)> {
        let (qpc_first,qpc_last)=frame.device_qpc_ns.map(|qpc|(qpc,qpc))
            .or_else(||frame.directshow_clock?.qpc_interval(frame.presentation_ns))?;
        let publication=self.mask_publication.try_lock().ok()?;
        let value=publication.as_ref()?;
        if value.binding_epoch!=key.epoch || value.snapshot.session!=key.session
            || !value.scroll_live.load(std::sync::atomic::Ordering::Acquire) {return None;}
        let correlation=value.snapshot.correlation?;
        let first=correlation.media_bounds(key.session,value.snapshot.playback_epoch,qpc_first)?;
        let last=correlation.media_bounds(key.session,value.snapshot.playback_epoch,qpc_last)?;
        let bounds=skylive_media::MediaTimeBounds {earliest:first.earliest,latest:last.latest};
        if bounds.earliest.0>bounds.latest.0 || bounds.latest.0>value.snapshot.clock.now().0 {return None;}
        let scroll=value.snapshot.scroll.as_ref()?;
        let context=scroll.history.estimated_window_filters(key.session,scroll.epoch,source,filters,0,bounds)?;
        Some((context,value.scroll_live.clone()))
    }
    fn retire_delay_history(&mut self, source: Id) {
        if let Some((_, _, live)) = self.delay_histories.remove(&source) {
            live.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    fn retire_all_delay_histories(&mut self) {
        for (_, (_, _, live)) in self.delay_histories.drain() {
            live.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    /// Capture each native sample before snapshots coalesce to the latest
    /// frame. Histories belong to authored sources, not physical devices.
    fn advance_delays(&mut self, key: CameraRequestKey) -> (HashMap<Id, CameraError>,bool) {
        if self.delay_settings.is_empty() { return (HashMap::new(),false); }
        let mut stage_changed=false;
        use super::async_delay::{Plan, Scope};
        let mut plans = Vec::new();
        let mut inputs = Vec::new();
        let mut errors = HashMap::new();
        for (source, filters) in &self.delay_settings {
            let Some((state, binding)) = self.bindings.iter().find_map(|(resource, bindings)| {
                let binding = bindings.iter().find(|(id, _)| id == source)?.1.clone();
                Some((self.streams.get(resource)?, binding))
            }) else { continue; };
            let Some(frame) = state.frame_for(&binding) else { continue; };
            let scope = Scope { session:key.session, source:*source, binding_generation:key.epoch,
                camera_owner:key.instance, stream:frame.stream_generation, clock_epoch:frame.clock_epoch };
            let mut valid = true;
            for (filter, delay_ms) in filters {
                let num = u128::from(*delay_ms) * u128::from(state.actual_mode.fps_num());
                let den = 1000u128 * u128::from(state.actual_mode.fps_den());
                let pending = (num + den - 1) / den + 2;
                if pending > 4096 { valid = false; break; }
                plans.push(Plan {scope, filter:*filter, chain_revision:key.epoch,
                    delay_ms:u32::from(*delay_ms), max_pending:pending as usize});
            }
            if valid { inputs.push((*source, scope, filters.iter().map(|(id,_)|*id).collect::<Vec<_>>(), frame)); }
            else {
                plans.retain(|plan| plan.scope.source != *source);
                errors.insert(*source, CameraError::BudgetExceeded("asynchronous camera delay frame capacity"));
            }
        }
        if self.delays.reconcile(&plans).is_err() {
            self.delayed_frames.clear();
            self.delays.reset_all();
            self.retire_all_delay_histories();
            self.stage_workers.clear();self.stage_pending.clear();self.stage_timeline.clear();
            self.delay_processed.clear();
            // Reconciliation leaves old lanes intact. These lanes did not
            // receive the current sample, so they must warm up again even if
            // the same plans can be admitted on a subsequent poll.
            for (source, _, _, _) in inputs {
                errors.insert(source, CameraError::BudgetExceeded("asynchronous camera delay configuration"));
            }
            return (errors,true);
        }
        self.stage_workers.retain(|source,(scope,_,_)|inputs.iter().any(|(id,expected,_,_)|id==source && expected==scope));
        self.stage_pending.retain(|source,_|self.stage_workers.contains_key(source));
        self.stage_timeline.retain(|source,_|self.stage_workers.contains_key(source));
        self.delayed_frames.retain(|source, (scope, _)| inputs.iter().any(|(id, expected, _, _)| id == source && *expected == *scope));
        self.delay_processed.retain(|source, _| inputs.iter().any(|(id, _, _, _)| id == source));
        self.delay_histories.retain(|source, (scope, _, live)| {
            let keep = inputs.iter().any(|(id, expected, _, _)| id == source && *expected == *scope);
            if !keep { live.store(false, std::sync::atomic::Ordering::Release); }
            keep
        });
        for (source, scope, filters, mut frame) in inputs {
            let staged=self.delay_upstream.iter().find(|(id,_)|*id==source).is_some_and(|(_,chain)|chain.iter().any(|filter|
                matches!(filter.effect,skylive_core::VideoEffect::PersonBackgroundRemoval {..}|skylive_core::VideoEffect::ScaleFilter {..}|skylive_core::VideoEffect::Scroll {..})
                || matches!(filter.effect,skylive_core::VideoEffect::ApplyLut {..}) && filter.lut_asset().is_some()
                || matches!(filter.effect,skylive_core::VideoEffect::ImageMask {..}) && filter.mask_image().is_some()));
            if staged {
                let source_plans:Vec<_>=plans.iter().filter(|plan|plan.scope.source==source).copied().collect();
                match self.advance_staged(source,scope,&source_plans,&frame) {
                    Ok(changed)=>stage_changed|=changed,
                    Err(error)=>{self.retire_delay_history(source);self.delayed_frames.remove(&source);self.stage_workers.remove(&source);self.stage_pending.remove(&source);self.stage_timeline.remove(&source);self.delay_processed.remove(&source);errors.insert(source,error);stage_changed=true;}
                }
                continue;
            }
            if self.delay_processed.get(&source) == Some(&(scope, frame.sequence)) { continue; }
            self.delay_processed.insert(source, (scope, frame.sequence));
            if !self.delay_histories.contains_key(&source) {
                let Some(generation) = self.delay_history_serial.checked_add(1) else {
                    self.delays.reset_source(source);
                    self.delayed_frames.remove(&source);
                    errors.insert(source, CameraError::IdentityExhausted);
                    continue;
                };
                self.delay_history_serial = generation;
                self.delay_histories.insert(source, (scope, generation, Arc::new(std::sync::atomic::AtomicBool::new(true))));
            }
            match self.delays.push_chain(&filters, scope, frame.sequence, frame.presentation_ns, frame.device_qpc_ns, frame.directshow_clock, &frame.image) {
                Ok(Some(output)) => {
                    let elapsed = frame.presentation_ns.checked_sub(output.timestamp_ns())
                        .and_then(|value| u64::try_from(value).ok());
                    let Some(elapsed_ns) = elapsed else {
                        self.delays.reset_source(source);
                        self.retire_delay_history(source);
                        self.delayed_frames.remove(&source);
                        errors.insert(source, CameraError::InvalidFrame("asynchronous camera delay timestamp"));
                        continue;
                    };
                    frame.draft_raw=Some(Arc::new(frame.clone()));
                    frame.sequence = output.sequence().expect("camera delay preserves native sequence");
                    frame.presentation_ns = output.timestamp_ns();
                    frame.image = output.shared_image();
                    frame.device_qpc_ns = output.device_qpc_ns();
                    // The observation follows the original delayed sample, not the newest arrival.
                    frame.directshow_clock = output.directshow_clock();
                    let Some((_, settings)) = self.delay_settings.iter().find(|(id, _)| *id == source) else {
                        self.delays.reset_source(source);
                        self.retire_delay_history(source);
                        self.delayed_frames.remove(&source);
                        errors.insert(source, CameraError::InvalidFrame("asynchronous camera delay settings"));
                        continue;
                    };
                    frame.async_delay = Some(crate::capture::AsyncDelayReceipt {
                        session:scope.session, binding_generation:scope.binding_generation, live_generation:None, source, owner:scope.camera_owner,
                        history_generation:self.delay_histories[&source].1, history_live:self.delay_histories[&source].2.clone(),
                        stream:scope.stream, clock_epoch:scope.clock_epoch,
                        sequence:frame.sequence, timestamp_ns:frame.presentation_ns, ndi_native_clock:None, ndi_media_time_estimate_ns:None,
                        filters:settings.clone().into(), staged_camera:None, staged_ndi:None, elapsed_ns,
                    });
                    self.delayed_frames.insert(source, (scope, frame));
                }
                Ok(None) => { self.delayed_frames.remove(&source); }
                Err(_) => {
                    self.delays.reset_source(source);
                    self.retire_delay_history(source);
                    self.delayed_frames.remove(&source);
                    errors.insert(source, CameraError::BudgetExceeded("asynchronous camera delay admission"));
                }
            }
        }
        (errors,stage_changed)
    }
    /// Process a resource-backed camera chain on its own bounded sequential
    /// worker. Poll completed events even when the native camera did not
    /// deliver a new frame, so the last asynchronous result is not stranded.
    fn advance_staged(&mut self,source:Id,scope:super::async_delay::Scope,
        plans:&[super::async_delay::Plan],frame:&CameraFrame)->Result<bool,CameraError> {
        use super::staged_delay_worker::{self,Event,Sample,Worker};
        let upstream=self.delay_upstream.iter().find(|(id,_)|*id==source)
            .map(|(_,filters)|filters.clone()).ok_or(CameraError::InvalidFrame("camera staged settings absent"))?;
        if !self.stage_workers.contains_key(&source) {
            let worker=Worker::new(source,self.stage_budget.clone()).map_err(|_|CameraError::BudgetExceeded("camera stage worker"))?;
            let generation=worker.generation();
            self.stage_workers.insert(source,(scope,worker,generation));
        }
        let (_,worker,generation)=self.stage_workers.get(&source).ok_or(CameraError::Closed)?;
        if worker.generation()!=*generation {return Err(CameraError::InvalidFrame("camera staged generation retired"));}
        if !self.delay_histories.contains_key(&source) {
            let generation=self.delay_history_serial.checked_add(1).ok_or(CameraError::IdentityExhausted)?;
            self.delay_history_serial=generation;
            self.delay_histories.insert(source,(scope,generation,Arc::new(std::sync::atomic::AtomicBool::new(true))));
        }
        if self.stage_timeline.get(&source).is_some_and(|live|!live.load(std::sync::atomic::Ordering::Acquire)) {
            return Err(CameraError::InvalidFrame("camera mask timeline retired"));
        }
        let event=self.stage_workers.get(&source).and_then(|(_,worker,_)|worker.try_recv());
        let mut changed=false;
        if let Some(event)=event {
            changed=true;
            match event {
                Event::Failed {..}=>return Err(CameraError::InvalidFrame("camera staged processing failed")),
                Event::Skipped {..}=>{},
                Event::Output(output)=>match output.frame {
                    None=>{self.delayed_frames.remove(&source);},
                    Some(delayed)=>{
                        let elapsed_ns=frame.presentation_ns.checked_sub(delayed.timestamp_ns()).and_then(|value|u64::try_from(value).ok())
                            .ok_or(CameraError::InvalidFrame("camera staged timestamp"))?;
                        let mut emitted=frame.clone();
                        emitted.draft_raw=Some(Arc::new(frame.clone()));
                        emitted.sequence=delayed.sequence().ok_or(CameraError::InvalidFrame("camera staged sequence"))?;
                        emitted.presentation_ns=delayed.timestamp_ns();emitted.device_qpc_ns=delayed.device_qpc_ns();
                        emitted.requested_at=output.requested_at;emitted.dropped_frames=output.dropped_frames;
                        emitted.directshow_clock=delayed.directshow_clock();emitted.image=delayed.shared_image();
                        let settings=self.delay_settings.iter().find(|(id,_)|*id==source).map(|(_,value)|value.clone())
                            .ok_or(CameraError::InvalidFrame("camera staged delay settings"))?;
                        let (_,history_generation,history_live)=self.delay_histories.get(&source)
                            .ok_or(CameraError::InvalidFrame("camera staged history"))?;
                        emitted.async_delay=Some(crate::capture::AsyncDelayReceipt {
                            session:scope.session,binding_generation:scope.binding_generation,live_generation:None,source,owner:scope.camera_owner,
                            history_generation:*history_generation,history_live:history_live.clone(),stream:scope.stream,clock_epoch:scope.clock_epoch,
                            sequence:emitted.sequence,timestamp_ns:emitted.presentation_ns,ndi_native_clock:None,ndi_media_time_estimate_ns:None,
                            filters:settings.into(),staged_camera:Some(crate::capture::StagedCameraProof {
                                upstream:upstream.clone().into(),processed:output.processed,
                                blend:output.blend.ok_or(CameraError::InvalidFrame("camera staged color proof"))?,
                                generation:output.generation,live_generation:self.stage_workers.get(&source)
                                    .ok_or(CameraError::Closed)?.1.generation_token(),
                                mask_live:output.mask_live,mask_resources:output.mask_resources,scroll_live:output.scroll_live,
                            }),staged_ndi:None,elapsed_ns,
                        });
                        self.delayed_frames.insert(source,(scope,emitted));
                    }
                },
            }
        }
        let timed=upstream.iter().any(|filter|filter.mask_image().is_some_and(|asset|crate::assets::mask_animation_candidate(asset.path())));
        let scroll_timed=upstream.iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::Scroll{..}));
        let pending=if timed || scroll_timed {
            // At most one native resident sample per source waits for the
            // next playback publication. No queue of raw pixels can grow.
            if self.stage_pending.get(&source).is_some_and(|(_,started)|started.elapsed()>Duration::from_secs(1)) {
                return Err(CameraError::Unavailable("camera animated filter requires an unambiguous covering playback snapshot and QPC clock correlation"));
            }
            if !self.stage_pending.contains_key(&source)
                && self.delay_processed.get(&source)!=Some(&(scope,frame.sequence)) {
                self.stage_pending.insert(source,(frame.clone(),Instant::now()));
            }
            let Some((pending,_))=self.stage_pending.get(&source).cloned() else{return Ok(changed);};
            pending
        } else {
            if self.delay_processed.get(&source)==Some(&(scope,frame.sequence)) {return Ok(changed);}
            frame.clone()
        };
        let person=self.stage_workers.get(&source).ok_or(CameraError::Closed)?.1.person();
        let stages=staged_delay_worker::static_mask_stages(plans,&upstream,(pending.image.width(),pending.image.height()),person,self.stage_budget.clone())
            .map_err(|_|CameraError::InvalidFrame("unsupported camera staged chain"))?;
        let scroll=if scroll_timed {
            let key=self.scope.as_ref().ok_or(CameraError::Closed)?.0;
            let Some(value)=self.scroll_snapshot_window(key,&pending,source,&upstream) else{return Ok(changed);};
            Some(value)
        }else{None};
        // This permit covers snapshots, filter identities and per-stage marker
        // arrays. The mask store separately admits the decoded clip bytes.
        let (prepare,timeline)=if timed {
            let key=self.scope.as_ref().ok_or(CameraError::Closed)?.0;
            let Some((clock,versions,bounds,live))=self.mask_snapshot_window(key,&pending) else{return Ok(changed);};
            if self.stage_timeline.get(&source).is_some_and(|old|!Arc::ptr_eq(old,&live)) {
                return Err(CameraError::InvalidFrame("camera mask timeline changed"));
            }
            if self.stage_timed_masks.as_ref().is_none_or(|(old,_)|!Arc::ptr_eq(old,&versions)) {
                self.stage_timed_masks=Some((versions.clone(),Arc::new(std::sync::Mutex::new(crate::video_filters::StagedMaskStore::with_versions(versions)))));
            }
            let store=self.stage_timed_masks.as_ref().ok_or(CameraError::Closed)?.1.clone();
            (staged_delay_worker::bounded_mask_resources(store,upstream,source,clock,bounds,live.clone(),self.stage_luts.clone(),
                self.stage_budget.clone(),0,scope.binding_generation),Some(live))
        } else {
            (staged_delay_worker::static_mask_resources(self.stage_masks.clone(),upstream,self.stage_luts.clone(),
                self.stage_budget.clone(),0,scope.binding_generation),None)
        };
        let metadata_credit=Arc::new(self.stage_budget.reserve_workspace(1024*1024)
            .map_err(|_|CameraError::BudgetExceeded("camera staged metadata"))?);
        let sample=Sample {scope,sequence:pending.sequence,timestamp_ns:pending.presentation_ns,qpc_ns:pending.device_qpc_ns,
            directshow_clock:pending.directshow_clock,requested_at:pending.requested_at,dropped_frames:pending.dropped_frames,image:pending.image.clone(),ingress:skylive_media::SourceBlend::LinearSrgb,
            revision:scope.binding_generation,metadata_credit,scroll,prepare,stages};
        match self.stage_workers.get(&source).ok_or(CameraError::Closed)?.1.try_submit(sample) {
            Ok(())=>{},
            Err("camera stage input queue is full")=>return Ok(changed),
            Err(_)=>return Err(CameraError::BudgetExceeded("camera staged input queue")),
        }
        if let Some(live)=timeline {self.stage_timeline.insert(source,live);}
        if timed || scroll_timed {self.stage_pending.remove(&source);}
        self.delay_processed.insert(source,(scope,pending.sequence));
        Ok(changed)
    }

    fn control_stream(&mut self, lease: &ControlLease) -> Result<&mut StreamState, CameraError> {
        let scope = self.scope.as_ref().ok_or(CameraError::Closed)?.0;
        if scope.instance != lease.key.instance || scope.session != lease.key.session || scope.epoch != lease.key.epoch {
            return Err(CameraError::Unavailable("camera control scope changed"));
        }
        let key = resource(&lease.binding);
        if !self.bindings.get(&key).is_some_and(|v| v.iter().any(|(id,b)| *id == lease.source && b.as_ref() == &lease.binding)) {
            return Err(CameraError::Unavailable("camera control source binding changed"));
        }
        let stream = self.streams.get_mut(&key).ok_or(CameraError::DeviceLost)?;
        if stream.generation != lease.stream_generation { return Err(CameraError::Unavailable("camera was restarted; refresh controls")); }
        Ok(stream)
    }
    fn backend(&mut self) -> Result<&mut Box<dyn NativeCameraBackend>, CameraError> {
        if let Some(error) = &self.backend_error {
            return Err(error.clone());
        }
        if self.backend.is_none() {
            let factory = self.factory.take().ok_or(CameraError::Disconnected)?;
            match factory() {
                Ok(backend) => self.backend = Some(backend),
                Err(error) => {
                    self.backend_error = Some(error.clone());
                    return Err(error);
                }
            }
        }
        Ok(self.backend.as_mut().unwrap())
    }
    fn deactivate(&mut self) {
        self.retire_all_delay_histories();
        if let Some(operation)=self.av.take(){operation.watch.publish(crate::camera_activation::Phase::Failed,Some(crate::camera_activation::Problem {code:"camera_capture_closed",detail:"Camera owner stopped before AV operation completed".into()}),None);}
        let owners=self.bindings.clone();let session=self.scope.as_ref().map_or(0,|scope|scope.0.session);
        self.bindings.clear();
        self.delay_settings.clear();
        self.delay_upstream.clear();
        self.stage_workers.clear();
        self.stage_pending.clear();self.stage_timeline.clear();self.stage_timed_masks=None;
        self.delayed_frames.clear();
        self.delay_processed.clear();
        let _ = self.delays.reconcile(&[]);
        self.scope = None;
        self.retry.clear();
        self.blocked.clear();
        for (key, state) in self.streams.drain() {
            self.closing.push(Closing { sources:owners.get(&key).map_or_else(Vec::new,|sources|sources.iter().map(|(id,_)|*id).collect()),session,key, state,problem:None });
        }
    }
    fn reap(&mut self) {
        self.resident.retain(|frame| Arc::strong_count(frame) > 1);
        let mut index = 0;
        while index < self.closing.len() {
            match self.closing[index].state.stream.stop() {
                Ok(true)=>{self.closing.swap_remove(index);},
                Ok(false)=>index+=1,
                Err(error)=>{self.closing[index].problem=Some(error);index+=1;},
            }
        }
    }
    fn activate(
        &mut self,
        key: CameraRequestKey,
        bindings: Bindings,
        delays: DelaySettings,
        upstream: DelayUpstream,
        now: MediaTime,
        restart: Option<ResourceKey>,
        output_fps: (u32,u32),
        limits: CameraLimits,
    ) -> Result<(), CameraError> {
        validate_bindings(&bindings,limits,false)?;
        self.retire_all_delay_histories();
        self.delay_settings = delays;
        self.delay_upstream = upstream;
        self.stage_workers.clear();
        self.stage_pending.clear();self.stage_timeline.clear();self.stage_timed_masks=None;
        self.delayed_frames.clear();
        self.delay_processed.clear();
        let _ = self.delays.reconcile(&[]);
        let keys: HashSet<_> = bindings.iter().map(|(_, b)| resource(b)).collect();
        let mut preferred:HashMap<VideoCaptureTarget,ResourceKey>=HashMap::new();
        // Prefer a requested already running mode. A new conflicting source
        // cannot silently reconfigure or stop the physical camera it shares.
        for resource in self.streams.keys().filter(|key|keys.contains(*key)) {preferred.insert(physical_target(&resource.0),resource.clone());}
        for (_,binding) in &bindings {preferred.entry(physical_target(binding.target())).or_insert_with(||resource(binding));}
        self.blocked.clear();
        for (_,binding) in &bindings {if preferred.get(&physical_target(binding.target()))!=Some(&resource(binding)) {self.blocked.insert(resource(binding),CameraError::ConflictingModes);}}
        let conflict=!self.blocked.is_empty();
        let output_fps_changed = self.output_fps != output_fps;
        self.output_fps = output_fps;
        let stale: Vec<_> = self
            .streams
            .keys()
            .filter(|key| !keys.contains(*key) || restart.as_ref() == Some(*key)
                || (output_fps_changed && key.2 == skylive_core::CameraFpsPolicy::MatchOutput))
            .cloned()
            .collect();
        for key in stale {
            self.closing.push(Closing {
                sources:self.bindings.get(&key).map_or_else(Vec::new,|sources|sources.iter().map(|(id,_)|*id).collect()),session:self.scope.as_ref().map_or(0,|scope|scope.0.session),
                problem:None,
                key: key.clone(),
                state: self.streams.remove(&key).unwrap(),
            });
        }
        // A manual restart and a new output rate supersede the failed open
        // attempt. Do not let the old attempt's retry deadline or diagnostic
        // delay the newly requested mode; unrelated cameras keep their backoff.
        self.retry.retain(|key, _| keys.contains(key)
            && restart.as_ref() != Some(key)
            && !(output_fps_changed && key.2 == skylive_core::CameraFpsPolicy::MatchOutput));
        self.bindings.clear();
        for (id, binding) in bindings {
            self.bindings
                .entry(resource(&binding))
                .or_default()
                .push((id, Arc::new(binding)));
        }
        self.dirty = true;
        self.scope = if self.bindings.is_empty() {
            None
        } else {
            Some((key, now, Instant::now()))
        };
        self.reap();
        self.open_missing(limits);
        if conflict {Err(CameraError::ConflictingModes)} else {Ok(())}
    }
    fn poll_av(&mut self,output:&mpsc::SyncSender<CameraEvent>,limits:CameraLimits){
        use crate::camera_activation::{Phase,Problem};
        let operation=self.av.take().or_else(||{
            if self.last_av.as_ref().is_some_and(|operation|operation.watch.cancellation().is_some_and(|cancel|!operation.cancelled||cancel.end_session&&!operation.ended)){self.last_av.take()}else{None}
        });
        let Some(mut operation)=operation else{return;};
        if let Some(cancel)=operation.watch.cancellation(){if !operation.cancelled||cancel.end_session&&!operation.ended{
            if self.scope.as_ref().is_some_and(|scope|scope.0.session!=operation.key.session){
                operation.watch.publish(Phase::Failed,Some(Problem {code:"camera_av_obsolete",detail:"Old-session camera activation cannot affect the replacement session".into()}),None);
                if !operation.reply_sent {let _=output.try_send(CameraEvent::Activated {key:operation.key,result:Err(CameraError::ForeignSession)});}
                return;
            }
            if cancel.end_session {self.deactivate();}else{
                let bindings=self.bindings.values().flat_map(|sources|sources.iter()).filter(|(id,_)|cancel.policy.enabled(*id)).map(|(id,binding)|(*id,binding.as_ref().clone())).collect();
                let at=cancel.changes.first().map_or(MediaTime(0),|entry|entry.at);
                let output_fps=self.output_fps;
                if let Err(error)=self.activate(operation.key,bindings,self.delay_settings.clone(),self.delay_upstream.clone(),at,None,output_fps,limits){operation.watch.publish(Phase::Retiring,Some(Problem {code:error.code(),detail:error.to_string()}),None);}
            }
            operation.changes=cancel.changes;operation.cancelled=true;operation.ended=cancel.end_session;
        }}
        let end_session=operation.watch.cancellation().is_some_and(|cancel|cancel.end_session);
        let mut ready=!end_session||!self.closing.iter().any(|closing|closing.session==operation.key.session);let mut failure=None;let mut retiring_problem=None;
        for entry in operation.changes.iter(){
            let key=resource(&entry.binding);
            if entry.enabled {
                let current = self.bindings.get(&key).and_then(|sources|sources.iter().find(|(id,_)|*id==entry.source));
                let Some((_,binding)) = current else {continue;};
                let error=self.blocked.get(&key).or_else(||self.retry.get(&key).map(|(_,error)|error))
                    .or_else(||self.streams.get(&key).and_then(|state|state.error.as_ref()));
                if let Some(error)=error {failure=Some(error.clone());break;}
                if self.streams.get(&key).is_none_or(|state|
                    state.image_for(binding.flip_vertically()).is_none()
                        || state.latest.as_ref().is_none_or(|frame|frame.sequence<=operation.floor)) {ready=false;}
            }else if let Some(closing)=self.closing.iter().find(|closing|closing.session==operation.key.session&&(closing.key==key||closing.sources.contains(&entry.source))){ready=false;retiring_problem=closing.problem.as_ref();}
        }
        if let Some(error)=failure {
            let changed:HashSet<_>=operation.changes.iter().filter(|e|e.enabled).map(|e|e.source).collect();
            self.bindings.retain(|_,sources|{sources.retain(|(id,_)|!changed.contains(id));!sources.is_empty()});
            let unused:Vec<_>=self.streams.keys().filter(|key|!self.bindings.contains_key(*key)).cloned().collect();
            for key in unused {if let Some(state)=self.streams.remove(&key){self.closing.push(Closing {sources:operation.changes.iter().map(|e|e.source).collect(),session:operation.key.session,key,state,problem:None});}}
            if operation.reply_sent||output.try_send(CameraEvent::Activated {key:operation.key,result:Err(error.clone())}).is_ok(){operation.reply_sent=true;operation.watch.publish(Phase::Failed,Some(Problem {code:error.code(),detail:error.to_string()}),None);self.last_av=Some(operation);}else{self.av=Some(operation);}
        }else if ready {
            let result=if operation.cancelled{Err(CameraError::Unavailable("camera activation superseded by stop"))}else{Ok(())};
            if operation.reply_sent||output.try_send(CameraEvent::Activated {key:operation.key,result}).is_ok(){
                operation.reply_sent=true;
                operation.watch.publish(if operation.cancelled{Phase::Failed}else{Phase::Complete},operation.cancelled.then(||Problem {code:"camera_av_cancelled",detail:"Camera activation was superseded by Stop; its native leg retired".into()}),None);
                self.last_av=Some(operation);
            }else{self.av=Some(operation);}
        }else{
            operation.watch.publish(if operation.changes.iter().all(|e|!e.enabled){Phase::Retiring}else{Phase::Working},retiring_problem.map(|e|Problem {code:e.code(),detail:e.to_string()}),None);self.av=Some(operation);
        }
    }
    fn open_missing(&mut self, limits: CameraLimits) {
        let mut bindings: Vec<_> = self
            .bindings
            .values()
            .filter_map(|sources| sources.first().map(|(id, b)| (*id,b.clone())))
            .collect();
        bindings.sort_by_key(|(id,_)|id.get());
        for (source,binding) in bindings {
            let key = resource(&binding);
            if self.blocked.contains_key(&key) {continue;}
            if self.streams.contains_key(&key)
                || self
                    .retry
                    .get(&key)
                    .is_some_and(|(at, _)| Instant::now() < *at)
            {
                continue;
            }
            // Device enumeration must wait for an old native stream to close;
            // otherwise probing can itself collide with a restart.
            if self.closing.iter().any(|closing|physical_target(&closing.key.0) == physical_target(binding.target())) { continue; }
            // Mode discovery also opens a native source. Do not bypass the
            // stream-count admission limit just to choose an automatic rate.
            if self.streams.len()+self.closing.len()>=limits.max_streams {
                self.retry.insert(key,(Instant::now()+Duration::from_secs(1),CameraError::BudgetExceeded("camera shutdown still occupies native admission")));
                self.dirty=true;
                continue;
            }
            let native_binding = if binding.mode_policy()==skylive_core::CameraModePolicy::DirectShowPreferred {
                // Resolve on each open, after the prior stream has retired.
                // The DirectShow choice must exist as an exact MF native mode;
                // otherwise report it rather than silently choosing a different one.
                let resolved=self.backend().and_then(|backend|backend.directshow_preferred(binding.target()))
                    .and_then(|info|info.mode.ok_or(CameraError::ModeNotFound))
                    .and_then(|preferred|self.backend().and_then(|backend|backend.enumerate_modes(binding.target(),limits))
                        .and_then(|modes|validate_modes(modes,limits))
                        .and_then(|modes|modes.into_iter().filter_map(|item|item.mode).find(|mode|super::directshow_mode_matches(*mode,preferred)).ok_or(CameraError::ModeNotFound)))
                    .and_then(|mode|binding.as_ref().clone().with_mode(mode).map_err(|_|CameraError::ModeNotFound));
                match resolved {
                    Ok(binding)=>binding,
                    Err(error)=>{self.retry.insert(key,(Instant::now()+Duration::from_secs(1),error));self.dirty=true;continue;}
                }
            } else if binding.fps_policy()==skylive_core::CameraFpsPolicy::Fixed {
                binding.as_ref().clone()
            } else {
                let output_fps=self.output_fps;
                let resolved=self.backend().and_then(|backend|backend.enumerate_modes(binding.target(),limits))
                    .and_then(|modes|validate_modes(modes,limits))
                    .and_then(|modes|super::selection::resolve_camera_fps_policy(&modes,binding.mode(),binding.fps_policy(),output_fps))
                    .and_then(|mode|mode.ok_or(CameraError::ModeNotFound))
                    .and_then(|mode|binding.as_ref().clone().with_mode(mode).map_err(|_|CameraError::ModeNotFound));
                match resolved {
                    Ok(binding)=>binding,
                    Err(error)=>{self.retry.insert(key,(Instant::now()+Duration::from_secs(1),error));self.dirty=true;continue;}
                }
            };
            // Keep the authored mode as the scale basis while capture publishes
            // actual pixels. Preview, transform editing and snapping evaluate
            // those dimensions against that basis, preserving the item's scale
            // when Preferred selects a different resolution on reopening.
            let bytes = match checked_frame_bytes(native_binding.width(), native_binding.height(), limits) {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.retry
                        .insert(key, (Instant::now() + Duration::from_secs(1), error));
                    continue;
                }
            };
            let reserved: usize = self
                .streams
                .values()
                .map(|s| s.raw_bytes)
                .chain(self.closing.iter().map(|s| s.state.raw_bytes))
                .sum();
            let raw_bytes = match checked_mode_raw_bytes(native_binding.mode(), limits) {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.retry
                        .insert(key, (Instant::now() + Duration::from_secs(1), error));
                    continue;
                }
            };
            let reserved_frames:usize=self.streams.values().map(|stream|stream.bytes).chain(self.closing.iter().map(|closing|closing.state.bytes)).sum();
            let count = self.streams.len() + self.closing.len();
            let generation = self.generation.checked_add(1);
            let result = if count >= limits.max_streams
                || reserved_frames.checked_add(bytes).is_none_or(|total|total>limits.max_total_frame_bytes)
                || self
                    .closing
                    .iter()
                    .any(|closing| physical_target(&closing.key.0) == physical_target(binding.target()))
                || reserved
                    .checked_add(raw_bytes)
                    .is_none_or(|n| n > limits.max_raw_bytes)
            {
                Err(CameraError::BudgetExceeded(
                    "camera shutdown still occupies native admission",
                ))
            } else if generation.is_none() {
                Err(CameraError::IdentityExhausted)
            } else {
                let scope=self.scope.as_ref().map(|scope|scope.0);
                let admitted=CameraLimits {max_raw_bytes:raw_bytes,max_frame_bytes:bytes,..limits};
                self.backend().and_then(|backend| {
                    if native_binding.embedded_audio() {
                        let scope=scope.ok_or(CameraError::Closed)?;
                        backend.open_embedded(&native_binding,scope.session,source,scope.epoch,admitted)
                    } else { backend.open(&native_binding,admitted) }
                })
            };
            match result {
                Ok(stream) => {
                    self.generation = generation.unwrap();
                    self.streams.insert(
                        key,
                        StreamState {
                            stream,
                            actual_mode: native_binding.mode(),
                            generation: self.generation,
                            clock_epoch: 1,
                            last_presentation_ns: None,
                            clock_discontinuity_pending: false,
                            latest: None,
                            opposite: None,
                            error: None,
                            bytes,
                            raw_bytes,
                            audio_sequence:0,
                            audio_discontinuity:true,
                        },
                    );
                }
                Err(error) => {
                    self.retry
                        .insert(key, (Instant::now() + Duration::from_secs(1), error));
                    self.dirty = true;
                }
            }
        }
    }
    fn produce(&mut self, limits: CameraLimits) -> Option<Arc<CameraSnapshot>> {
        let (key, origin, sent) = self.scope?;
        self.open_missing(limits);
        let now = match u64::try_from(sent.elapsed().as_nanos())
            .ok()
            .and_then(|elapsed| origin.0.checked_add(elapsed))
        {
            Some(now) => MediaTime(now),
            None => {
                self.publication = self.publication.checked_add(1)?;
                let detail = problem_detail(&CameraError::TimeOverflow);
                let problems = self
                    .bindings
                    .values()
                    .flat_map(|sources| {
                        sources.iter().map(|(source, _)| CameraProblem {
                            source: *source,
                            kind: CameraError::TimeOverflow.issue_kind(),
                            code: CameraError::TimeOverflow.code(),
                            detail: detail.clone(),
                        })
                    })
                    .collect();
                let snapshot = Arc::new(CameraSnapshot {
                    instance: key.instance,
                    session: key.session,
                    epoch: key.epoch,
                    generation: self.publication,
                    frames: HashMap::new(),
                    draft_frames: HashMap::new(),
                    problems,
                });
                self.deactivate();
                return Some(snapshot);
            }
        };
        let mut changed = std::mem::take(&mut self.dirty);
        let mut resident_bytes: usize = self.resident.iter().map(|f| f.byte_len()).sum();
        let keys: Vec<_> = self.streams.keys().cloned().collect();
        let mut failed = Vec::new();
        for resource_key in keys {
            let state = self.streams.get_mut(&resource_key).unwrap();
            let Some(sources) = self.bindings.get(&resource_key) else { continue; };
            let mut embedded_failed=false;
            if resource_key.4 {
                // Both pins belong to this one native graph and are drained on
                // its owner thread. Never retain its callback pool in the GUI.
                let route=self.embedded_route.lock().ok().and_then(|route|route.clone());
                for _ in 0..4 {
                    let mut block=match state.stream.poll_embedded_audio() {
                        Ok(Some(block))=>block,
                        Ok(None)=>break,
                        Err(error)=>{state.error=Some(error);failed.push(resource_key.clone());changed=true;embedded_failed=true;break;}
                    };
                    let Some(sequence)=state.audio_sequence.checked_add(1) else {
                        state.error=Some(CameraError::IdentityExhausted);failed.push(resource_key.clone());changed=true;embedded_failed=true;break;
                    };
                    state.audio_sequence=sequence;
                    let Some(route)=route.as_ref().filter(|route|route.program.session==key.session) else {
                        state.audio_discontinuity=true;continue;
                    };
                    block.flags.discontinuity |= state.audio_discontinuity;
                    let mut dropped=false;
                    for (source,binding) in sources {
                        // Each queued packet owns its allocation. Reserve it
                        // fallibly before copying; a failed transfer is marked
                        // as a discontinuity on the next packet.
                        let mut samples=Vec::new();
                        if samples.try_reserve_exact(block.samples.len()).is_err(){dropped=true;continue;}
                        samples.extend_from_slice(&block.samples);
                        let copy=crate::audio_capture::NativePcmBlock {
                            format:block.format,frames:block.frames,samples,
                            device_timestamp_ns:block.device_timestamp_ns,
                            graph_timestamp_ns:block.graph_timestamp_ns,
                            directshow_clock:block.directshow_clock,
                            host_received_ns:block.host_received_ns,
                            device_frame_position:block.device_frame_position,flags:block.flags,
                        };
                        let packet=crate::audio_capture::EmbeddedPcmPacket {
                            program:route.program,source:*source,binding:binding.clone(),
                            stream_generation:state.generation,sequence,block:copy,correlation:route.correlation,
                        };
                        if route.sink.submit(packet).is_err(){dropped=true;}
                    }
                    state.audio_discontinuity=dropped;
                }
            }
            if embedded_failed {continue;}
            let plain = sources.iter().any(|(_,binding)|!binding.flip_vertically());
            let flipped = sources.iter().any(|(_,binding)|binding.flip_vertically());
            let image_count = usize::from(plain) + usize::from(flipped);
            if image_count == 0 { continue; }
            // This is the existing current+retiring output ledger. Count both
            // demanded orientations BEFORE native poll or any variant allocation.
            let remaining = (limits.max_total_frame_bytes * 4).saturating_sub(resident_bytes);
            let required = state.bytes.checked_mul(image_count);
            if required.is_none_or(|bytes|bytes > remaining)
                || self.resident.len().checked_add(image_count).is_none_or(|count|count > MAX_REFERENCES * 4) {
                if state.error
                    != Some(CameraError::BudgetExceeded(
                        "camera resident output readers",
                    ))
                {
                    state.error = Some(CameraError::BudgetExceeded(
                        "camera resident output readers",
                    ));
                    changed = true;
                }
                continue;
            }
            let Some(sequence) = self.sequence.checked_add(1) else {
                state.error = Some(CameraError::IdentityExhausted);
                changed = true;
                continue;
            };
            match state.stream.poll(CameraLimits {
                max_frame_bytes: state.bytes,
                max_raw_bytes: state.raw_bytes,
                ..limits
            }) {
                Ok(Some(frame)) => {
                    if frame.image.width() != state.actual_mode.width()
                        || frame.image.height() != state.actual_mode.height()
                        || frame.image.byte_len() != state.bytes
                    {
                        state.error = Some(CameraError::InvalidFrame(
                            "camera changed its exact authored mode",
                        ));
                        failed.push(resource_key);
                        changed = true;
                        continue;
                    }
                    // Check the native clock per physical stream, not request
                    // time or another source sharing the camera. A skipped UI
                    // snapshot must not hide a backward or >1s timestamp jump.
                    let timestamp_jump = state.last_presentation_ns.is_some_and(|last| {
                        frame.presentation_ns.checked_sub(last)
                            .is_none_or(|delta| !(0..=1_000_000_000).contains(&delta))
                    });
                    state.clock_discontinuity_pending |= frame.discontinuity || timestamp_jump;
                    let next_clock_epoch = if state.clock_discontinuity_pending {
                        let Some(epoch) = state.clock_epoch.checked_add(1) else {
                            state.error = Some(CameraError::IdentityExhausted);
                            failed.push(resource_key);
                            changed = true;
                            continue;
                        };
                        epoch
                    } else { state.clock_epoch };
                    // NativeVideoFrame owns its Box pixels exclusively. A sole
                    // flipped demand is in-place; there is no Arc::make_mut clone.
                    let mut pixels = frame.image;
                    let primary_flip = !plain;
                    if primary_flip { pixels.flip_vertically(); }
                    let opposite = if plain && flipped {
                        match pixels.try_flipped_vertically() {
                            Ok(image) => Some(Arc::new(image)),
                            Err(_) => {
                                state.error = Some(CameraError::BudgetExceeded("camera orientation image allocation"));
                                changed = true;
                                continue;
                            }
                        }
                    } else { None };
                    let image = Arc::new(pixels);
                    resident_bytes += image.byte_len();
                    self.resident.push(image.clone());
                    if let Some(opposite) = &opposite {
                        resident_bytes += opposite.byte_len();
                        self.resident.push(opposite.clone());
                    }
                    self.sequence = sequence;
                    state.clock_epoch = next_clock_epoch;
                    state.last_presentation_ns = Some(frame.presentation_ns);
                    state.clock_discontinuity_pending = false;
                    state.opposite = opposite.map(SharedVideoFrame::direct);
                    state.latest = Some(CameraFrame {
                        async_delay: None,
                        device_qpc_ns: frame.device_qpc_ns,
                        directshow_clock: frame.directshow_clock,
                        binding: sources.iter().find(|(_,binding)|binding.flip_vertically()==primary_flip).unwrap().1.clone(),
                        actual_mode: state.actual_mode,
                        carried: false,
                        sequence,
                        stream_generation: state.generation,
                        clock_epoch: state.clock_epoch,
                        requested_at: now,
                        presentation_ns: frame.presentation_ns,
                        dropped_frames: frame.dropped_frames,
                        image: SharedVideoFrame::direct(image),
                        draft_raw:None,
                    });
                    state.error = None;
                    changed = true;
                }
                Ok(None) => {}
                Err(error) => {
                    state.error = Some(error);
                    failed.push(resource_key);
                    changed = true;
                }
            }
        }
        for resource_key in failed {
            if let Some(state) = self.streams.remove(&resource_key) {
                let error = state.error.clone().unwrap_or(CameraError::DeviceLost);
                self.retry.insert(
                    resource_key.clone(),
                    (Instant::now() + Duration::from_secs(1), error),
                );
                self.closing.push(Closing {
                    sources:self.bindings.get(&resource_key).map_or_else(Vec::new,|sources|sources.iter().map(|(id,_)|*id).collect()),session:self.scope.as_ref().map_or(0,|scope|scope.0.session),
                    problem:None,
                    key: resource_key,
                    state,
                });
            }
        }
        let (delay_errors,stage_changed) = self.advance_delays(key);
        if !changed && !stage_changed && delay_errors.is_empty() {
            return None;
        }
        self.publication = self.publication.checked_add(1)?;
        let mut frames = HashMap::new();
        let mut draft_frames = HashMap::new();
        let mut problems = Vec::new();
        for (key, sources) in &self.bindings {
            let state = self.streams.get(key);
            let error = self.blocked.get(key).or_else(||state.and_then(|s| s.error.as_ref()))
                .or_else(|| self.retry.get(key).map(|(_, e)| e));
            let pending = CameraError::NoFrame;
            for (source, binding) in sources {
                let frame = if self.delay_settings.iter().any(|(id,_)| id == source) {
                    self.delayed_frames.get(source).map(|(_, frame)|frame.clone())
                } else { state.and_then(|state|state.frame_for(binding)) };
                // Keep a separate native sample even after the first delayed
                // output: a later filter edit may revoke its receipt before a
                // replacement Program frame is ready. This sample is modal-only
                // and retains the existing native resident lease.
                if error.is_none() && self.delay_settings.iter().any(|(id,_)| id == source) {
                    if let Some(raw)=state.and_then(|state|state.frame_for(binding)) {draft_frames.insert(*source,raw);}
                }
                let error = delay_errors.get(source).or(error).or_else(||frame.is_none().then_some(&pending));
                let detail = error.map(problem_detail);
                if let Some(frame) = frame { frames.insert(*source, frame); }
                if let (Some(error), Some(detail)) = (error, &detail) {
                    problems.push(CameraProblem {
                        source: *source,
                        kind: error.issue_kind(),
                        code: error.code(),
                        detail: detail.clone(),
                    });
                }
            }
        }

        Some(Arc::new(CameraSnapshot {
            instance: key.instance,
            session: key.session,
            epoch: key.epoch,
            generation: self.publication,
            frames,
            draft_frames,
            problems,
        }))
    }
}
fn problem_detail(error: &CameraError) -> Arc<str> {
    let detail = error.to_string();
    if detail.len() > 8192 {
        Arc::from("camera backend error detail exceeds 8192 bytes")
    } else {
        Arc::from(detail)
    }
}
fn run(
    factory: CameraBackendFactory,
    input: mpsc::Receiver<Work>,
    output: mpsc::SyncSender<CameraEvent>,
    frames: mpsc::SyncSender<Arc<CameraSnapshot>>,
    limits: CameraLimits,
    embedded_route:Arc<std::sync::Mutex<Option<EmbeddedRoute>>>,
    mask_publication:Arc<std::sync::Mutex<Option<CameraMaskPublication>>>,
) {
    let delay_budget=skylive_media::AsyncDelayBudget::new(2usize * 1024 * 1024 * 1024, 4096).expect("valid camera delay budget");
    let mut state = Runtime {
        factory: Some(factory),
        backend: None,
        backend_error: None,
        streams: HashMap::new(),
        closing: vec![],
        bindings: HashMap::new(),
        delay_settings: vec![],
        delay_upstream: vec![],
        mask_publication,
        stage_budget:delay_budget.clone(),
        stage_workers:HashMap::new(),
        stage_pending:HashMap::new(),
        stage_timeline:HashMap::new(),
        stage_timed_masks:None,
        stage_masks:Arc::new(std::sync::Mutex::new(Default::default())),
        stage_luts:Arc::new(std::sync::Mutex::new(Default::default())),
        delays: super::async_delay::SourceDelays::new(delay_budget),
        delayed_frames: HashMap::new(),
        delay_processed: HashMap::new(),
        delay_histories: HashMap::new(),
        delay_history_serial: 0,
        output_fps: (0,0),
        dirty: false,
        scope: None,
        stop: None,
        av:None,
        last_av:None,
        generation: 0,
        sequence: 0,
        publication: 0,
        resident: vec![],
        retry: HashMap::new(),
        blocked: HashMap::new(),
        embedded_route,
    };
    let mut disconnected = false;
    // A full UI channel must not discard the last publication: after a device
    // error or a final sample there may be no later frame to publish it again.
    // Keep only the newest unsent snapshot and retry without blocking capture.
    let mut pending_snapshot: Option<Arc<CameraSnapshot>> = None;
    loop {
        state.reap();
        if disconnected {
            pending_snapshot = None;
            if state.closing.is_empty() && state.resident.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        let work = if state.scope.is_some()
            || state.stop.is_some()
            || state.av.is_some()
            || pending_snapshot.is_some()
            || !state.closing.is_empty()
            || !state.resident.is_empty()
        {
            match input.recv_timeout(Duration::from_millis(5)) {
                Ok(work) => Some(work),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    state.deactivate();
                    None
                }
            }
        } else {
            match input.recv() {
                Ok(work) => Some(work),
                Err(_) => {
                    disconnected = true;
                    state.deactivate();
                    None
                }
            }
        };
        if let Some(work) = work {
            let reply = match work {
                Work::AvActivation {key,bindings,delays,upstream,changes,now,output_fps,watch}=>{
                    pending_snapshot = None;
                    watch.publish(crate::camera_activation::Phase::Working,None,None);
                    let result=state.activate(key,bindings,delays,upstream,now,None,output_fps,limits);
                    match result {
                        Ok(())=>{state.av=Some(AvOperation {reply_sent:false,key,changes,watch,floor:state.sequence,cancelled:false,ended:false});None},
                        Err(error)=>{watch.publish(crate::camera_activation::Phase::Failed,Some(crate::camera_activation::Problem {code:error.code(),detail:error.to_string()}),None);Some(CameraEvent::Activated {key,result:Err(error)})},
                    }
                }
                Work::Controls { key, lease } => {
                    let result = state.control_stream(&lease).and_then(|s| s.stream.query_controls())
                        .map(|query| ControlCatalog {lease, query});
                    Some(CameraEvent::Controls {key, result})
                }
                Work::SetControl {key, lease, change, gate} => {
                    let receipt = match state.control_stream(&lease) {
                        Ok(stream) => stream.stream.set_control(change, &gate),
                        Err(_) => ControlReceipt::before(change, "camera owner, source or generation changed before write"),
                    };
                    gate.finish();
                    Some(CameraEvent::ControlSet {key, lease, receipt})
                }
                Work::Devices(key) => Some(CameraEvent::Devices {
                    key,
                    result: state
                        .backend()
                        .and_then(|b| b.enumerate_devices(limits))
                        .and_then(|v| validate_devices(v, limits)),
                }),
                Work::Modes(key, target) => {
                    let result=if state.closing.iter().any(|closing|physical_target(&closing.key.0)==physical_target(&target)) {
                        Err(CameraError::DeviceBusy)
                    } else if let Some(stream)=state.streams.iter_mut().find(|(resource,_)|physical_target(&resource.0)==physical_target(&target)).map(|(_,stream)|stream) {
                        stream.stream.enumerate_modes(limits).and_then(|modes|validate_modes(modes,limits))
                    } else if state.streams.len()+state.closing.len()>=limits.max_streams {
                        Err(CameraError::BudgetExceeded("camera mode query native admission"))
                    } else {
                        state.backend().and_then(|backend|backend.enumerate_modes(&target,limits)).and_then(|modes|validate_modes(modes,limits))
                    };
                    Some(CameraEvent::Modes {
                        key,
                        target,
                        result,
                    })
                }
                Work::DirectShowDefault(key,target) => {
                    let same_device=|other:&VideoCaptureTarget|match (other,&target) {
                        (VideoCaptureTarget::Windows{symbolic_link:a},VideoCaptureTarget::Windows{symbolic_link:b})=>a.eq_ignore_ascii_case(b),
                        _=>other==&target,
                    };
                    let result=if state.closing.iter().any(|closing|same_device(&closing.key.0))
                        || state.streams.keys().any(|resource|same_device(&resource.0)) {
                        Err(CameraError::DeviceBusy)
                    } else if state.streams.len()+state.closing.len()>=limits.max_streams {
                        Err(CameraError::BudgetExceeded("DirectShow default query native admission"))
                    } else {
                        state.backend().and_then(|backend|backend.directshow_default(&target))
                    };
                    Some(CameraEvent::DirectShowDefault {key,target,result})
                }
                Work::DirectShowPreferred(key,target) => {
                    let same_device=|other:&VideoCaptureTarget|match (other,&target) {
                        (VideoCaptureTarget::Windows{symbolic_link:a},VideoCaptureTarget::Windows{symbolic_link:b})=>a.eq_ignore_ascii_case(b),
                        _=>other==&target,
                    };
                    let result=if state.closing.iter().any(|closing|same_device(&closing.key.0))
                        || state.streams.keys().any(|resource|same_device(&resource.0)) {
                        Err(CameraError::DeviceBusy)
                    } else if state.streams.len()+state.closing.len()>=limits.max_streams {
                        Err(CameraError::BudgetExceeded("DirectShow preferred query native admission"))
                    } else {
                        state.backend().and_then(|backend|backend.directshow_preferred(&target))
                    };
                    Some(CameraEvent::DirectShowPreferred {key,target,result})
                }
                Work::DirectShowVideoOnly(key,target) => {
                    let same_device=|other:&VideoCaptureTarget|match (other,&target) {
                        (VideoCaptureTarget::Windows{symbolic_link:a},VideoCaptureTarget::Windows{symbolic_link:b})=>a.eq_ignore_ascii_case(b),
                        _=>other==&target,
                    };
                    let result=if state.closing.iter().any(|closing|same_device(&closing.key.0)) || state.streams.keys().any(|resource|same_device(&resource.0)) {
                        Err(CameraError::DeviceBusy)
                    } else if state.streams.len()+state.closing.len()>=limits.max_streams {
                        Err(CameraError::BudgetExceeded("DirectShow audio query native admission"))
                    } else {state.backend().and_then(|backend|backend.directshow_video_only(&target))};
                    Some(CameraEvent::DirectShowVideoOnly {key,target,result})
                }
                Work::Activate {
                    key,
                    bindings,
                    delays,
                    upstream,
                    now,
                    output_fps,
                    restart,
                } => {
                    pending_snapshot = None;
                    Some(CameraEvent::Activated {
                        key,
                        result: state.activate(key, bindings, delays, upstream, now, restart, output_fps, limits),
                    })
                },
                Work::Stop(key) => {
                    pending_snapshot = None;
                    state.deactivate();
                    state.stop = Some(key);
                    None
                }
            };
            if let Some(reply) = reply {
                let _ = output.try_send(reply);
            }
        }
        state.reap();
        if let Some(key) = state.stop {
            if state.closing.is_empty() {
                let _ = output.try_send(CameraEvent::Stopped { key });
                state.stop = None;
            }
        }
        if let Some(snapshot) = state.produce(limits) {
            pending_snapshot = Some(snapshot);
        }
        if let Some(snapshot) = pending_snapshot.take() {
            match frames.try_send(snapshot) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(snapshot)) => pending_snapshot = Some(snapshot),
                Err(mpsc::TrySendError::Disconnected(_)) => {}
            }
        }
        state.poll_av(&output,limits);
    }
    // All streams acknowledged stop and all output readers retired on this
    // owner before the native backend performs MF/AVFoundation shutdown.
    drop(state);
}
fn validate_devices(
    mut values: Vec<CameraDeviceInfo>,
    limits: CameraLimits,
) -> Result<Vec<CameraDeviceInfo>, CameraError> {
    if values.len() > limits.max_devices {
        return Err(CameraError::BudgetExceeded("camera device metadata count"));
    }
    let mode = VideoCaptureMode::new(1, 1, 1, 1, skylive_core::VideoPixelFormat::Rgb32)
        .map_err(|_| CameraError::InvalidFrame("camera validation mode"))?;
    let mut targets = HashSet::new();
    for value in &mut values {
        if value.name.is_empty()
            || value.name.len() > 1024
            || value.name.chars().any(char::is_control)
        {
            return Err(CameraError::InvalidFrame("camera device name"));
        }
        value.target = VideoCaptureBinding::new(value.target.clone(), mode, false)
            .map_err(|_| CameraError::InvalidFrame("camera device identity"))?
            .target()
            .clone();
        value.name = std::mem::take(&mut value.name)
            .into_boxed_str()
            .into_string();
        if !targets.insert(value.target.clone()) {
            return Err(CameraError::InvalidFrame("duplicate camera device"));
        }
    }
    Ok(values.into_boxed_slice().into_vec())
}
fn validate_modes(
    mut values: Vec<CameraModeInfo>,
    limits: CameraLimits,
) -> Result<Vec<CameraModeInfo>, CameraError> {
    if values.len() > limits.max_modes {
        return Err(CameraError::BudgetExceeded("camera mode metadata count"));
    }
    for value in &mut values {
        if value.native_subtype.is_empty()
            || value.native_subtype.len() > 128
            || value.native_subtype.chars().any(char::is_control)
        {
            return Err(CameraError::InvalidFrame("camera native subtype"));
        }
        value.native_subtype = std::mem::take(&mut value.native_subtype)
            .into_boxed_str()
            .into_string();
        if let Some(mode) = value.mode {
            if (mode.width(), mode.height(), mode.fps_num(), mode.fps_den())
                != (value.width, value.height, value.fps_num, value.fps_den)
                || value.unsupported_reason.is_some()
            {
                return Err(CameraError::InvalidFrame(
                    "camera native mode metadata mismatch",
                ));
            }
        } else if value.unsupported_reason.is_none() {
            return Err(CameraError::InvalidFrame(
                "unsupported camera mode lacks reason",
            ));
        }
    }
    Ok(values.into_boxed_slice().into_vec())
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
