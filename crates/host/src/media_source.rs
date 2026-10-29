//! Persistent, bounded local-file playback. Rust codecs and the explicit Windows
//! software MF alternative run on this owner. No subprocess or GUI codec work.
//! Authored settings contain no decoder/runtime data.
use crate::{
    audio_capture::{NativeAudioFormat, ProgramCaptureKey},
    mediafile::{MediaFileError, MediaFileInfo},
    transition::FrameComposition,
};
mod reload;
pub mod network_http;
pub(crate) mod network_progress;
pub use reload::{MediaReloadTarget,MAX_MEDIA_RELOAD_TARGETS,MAX_MEDIA_RELOAD_METADATA};
use serde::Serialize;
use skylive_core::{Id, MediaSource, Session, StudioSnapshot};
use skylive_media::{MediaTime, RgbaFrame};
use std::{
    collections::HashMap,
    fmt,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};
pub(crate) mod audio;
pub(crate) mod control;
mod owner;
pub use audio::MediaAudioSink;
pub(crate) use audio::MediaPcmPacket;
pub use control::{MediaAppliedTime, MediaControl, MediaControlTarget, MediaTransportStatus};
// Authored sources are inexpensive metadata; decoded frames and live decoders
// are admitted separately. A ninth dormant source must not disable all media.
const MAX_SOURCES: usize = skylive_core::MAX_SOURCES;
const MAX_RESIDENT_BYTES: usize = 128 * 1024 * 1024;
const MAX_RESIDENT_FRAMES: usize = 64;
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
type Wake = Arc<dyn Fn() + Send + Sync>;
#[derive(Debug)]
pub enum MediaSourceError {
    File(MediaFileError),
    ProgramAudio(crate::audio_program::ProgramAudioError),
    Invalid(&'static str),
    ControlRetired(&'static str),
    Limit(&'static str),
    Busy,
    /// The audio barrier was not enqueued; no control effect was published.
    BarrierBusy,
    Closed,
    Disconnected,
    IdentityExhausted,
    TimeOverflow,
    Io(std::io::Error),
}
impl MediaSourceError {
    pub fn issue_kind(&self) -> crate::resource_issue::ResourceIssueKind {
        use crate::resource_issue::ResourceIssueKind::*;
        match self {
            Self::File(e) => e.issue_kind(),
            Self::ProgramAudio(
                crate::audio_program::ProgramAudioError::Limit(_)
                | crate::audio_program::ProgramAudioError::Backpressure(_),
            ) => Limit,
            Self::ProgramAudio(_) => Invalid,
            Self::Limit(_) => Limit,
            Self::Invalid(_)
            | Self::ControlRetired(_)
            | Self::IdentityExhausted
            | Self::TimeOverflow => Invalid,
            _ => Unavailable,
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::File(e) => e.code(),
            Self::ProgramAudio(e) => e.code(),
            Self::Invalid(_) => "media_source_invalid",
            Self::ControlRetired(_) => "media_source_control_retired",
            Self::Limit(_) => "media_source_limit",
            Self::Busy => "media_source_busy",
            Self::BarrierBusy => "media_source_barrier_busy",
            Self::Closed => "media_source_closed",
            Self::Disconnected => "media_source_disconnected",
            Self::IdentityExhausted => "media_source_identity_exhausted",
            Self::TimeOverflow => "media_source_time_overflow",
            Self::Io(_) => "media_source_io",
        }
    }
}
impl fmt::Display for MediaSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?}", self.code(), self)
    }
}
impl std::error::Error for MediaSourceError {}
impl From<MediaFileError> for MediaSourceError {
    fn from(e: MediaFileError) -> Self {
        Self::File(e)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct MediaRequestKey {
    pub instance: u64,
    pub session: u64,
    pub revision: u64,
    pub epoch: u64,
    pub request: u64,
}
#[derive(Debug)]
pub struct PreparedMedia {
    pub media: MediaSource,
    pub info: MediaFileInfo,
}
/// Existing preparation path, called only by host file-operation workers.
/// No new owner or decoder profile; receipts are published by the caller's
/// session/request fence, never directly into the document.
pub(crate) fn prepare_local_with_budget(path:&std::path::Path,budget:&crate::mediafile::MediaReadBudget)->Result<PreparedMedia,MediaSourceError>{
    owner::prepare_with_budget(path,budget)
}
/// Import runs on the file worker; reserve the worst-case transfer and parser
/// reads before starting, so multiple network sources share the import budget.
pub(crate) fn prepare_finite_http_with_budget(url:&str,budget:&crate::mediafile::MediaReadBudget)->Result<PreparedMedia,MediaSourceError>{
    budget.reserve(112 * 1024 * 1024)?;
    let request = network_http::Request::http_url(url).map_err(MediaSourceError::Io)?;
    let download = network_http::Owner::start(request).map_err(|e| if e.kind()==std::io::ErrorKind::WouldBlock {MediaSourceError::Busy}else{MediaSourceError::Io(e)})?;
    loop {
        if let Some(result) = download.poll() {
            let bytes = result.map_err(MediaSourceError::Io)?;
            let reader = crate::mediafile::MediaFileReader::open_bytes_native_preferred(bytes,url.into(),crate::mediafile::MediaFileLimits::default())?;
            return owner::prepared_reader(reader);
        }
        thread::sleep(std::time::Duration::from_millis(25));
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct MediaProblem {
    pub kind: crate::resource_issue::ResourceIssueKind,
    pub source: Id,
    pub code: &'static str,
    pub detail: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaPlaybackState {
    Paused,
    Stopped,
    Playing,
    Ended,
    Failed,
}
#[derive(Clone, Debug)]
pub struct MediaFrame {
    pub stream_generation:u64,
    pub media: Arc<MediaSource>,
    pub image: Arc<RgbaFrame>,
    pub sequence: u64,
    pub requested_at: MediaTime,
}
#[derive(Clone, Debug)]
pub struct MediaSnapshot {
    video_decoders:Arc<HashMap<Id,(Arc<MediaSource>,crate::mediafile::VideoDecoderInfo)>>,
    audio_formats: Arc<HashMap<Id, (Arc<MediaSource>, NativeAudioFormat)>>,
    empty: Arc<HashMap<Id, Arc<MediaSource>>>,
    session: u64,
    instance: u64,
    epoch: u64,
    generation: u64,
    frames: Arc<HashMap<Id, MediaFrame>>,
    problems: Arc<Vec<MediaProblem>>,
    states: Arc<HashMap<Id, MediaPlaybackState>>,
    controls: Arc<HashMap<Id, control::Entry>>,
    pub control_late_frames: u64,
    pub control_late_frames_saturated: bool,
    pub playing: bool,
}
impl MediaSnapshot {
    fn empty(session: u64, instance: u64, epoch: u64) -> Self {
        Self {
            video_decoders:Arc::new(HashMap::new()),
            audio_formats: Arc::new(HashMap::new()),
            session,
            instance,
            epoch,
            generation: 0,
            frames: Arc::new(HashMap::new()),
            empty: Arc::new(HashMap::new()),
            problems: Arc::new(Vec::new()),
            states: Arc::new(HashMap::new()),
            controls: Arc::new(HashMap::new()),
            control_late_frames: 0,
            control_late_frames_saturated: false,
            playing: false,
        }
    }
    pub fn session(&self) -> u64 {
        self.session
    }
    pub fn instance(&self) -> u64 {
        self.instance
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn frame(&self, id: Id) -> Option<&MediaFrame> {
        self.frames.get(&id)
    }
    pub fn frames(&self) -> impl Iterator<Item = (Id, &MediaFrame)> {
        self.frames.iter().map(|(id, f)| (*id, f))
    }
    pub fn empty_sources(&self) -> impl Iterator<Item = (Id, &Arc<MediaSource>)> {
        self.empty.iter().map(|(id, media)| (*id, media))
    }
    pub fn problems(&self) -> &[MediaProblem] {
        &self.problems
    }
    pub fn state(&self, id: Id) -> Option<MediaPlaybackState> {
        self.states.get(&id).copied()
    }
    pub fn source_count(&self) -> usize {
        self.states.len()
    }
    pub fn video_decoder_for(&self,session:u64,source:Id,media:&MediaSource)->Option<crate::mediafile::VideoDecoderInfo>{
        if self.session!=session{return None;}let (binding,info)=self.video_decoders.get(&source)?;
        (binding.as_ref()==media).then_some(*info)
    }
    pub fn audio_format(&self, source: Id) -> Option<NativeAudioFormat> {
        self.audio_formats.get(&source).map(|(_, format)| *format)
    }
    pub fn audio_format_for(
        &self,
        session: u64,
        source: Id,
        media: &MediaSource,
    ) -> Option<NativeAudioFormat> {
        if self.session != session {
            return None;
        }
        let (binding, format) = self.audio_formats.get(&source)?;
        if binding.path() != media.path()
            || binding.video_dimensions() != media.video_dimensions()
            || binding.has_audio() != media.has_audio()
        {
            return None;
        }
        Some(*format)
    }
}
#[derive(Debug)]
pub enum MediaEvent {
    Controlled {
        key: MediaRequestKey,
        source: Id,
        result: Result<MediaTransportStatus, MediaSourceError>,
    },
    Prepared {
        key: MediaRequestKey,
        result: Result<PreparedMedia, MediaSourceError>,
    },
    Updated {
        key: MediaRequestKey,
    },
}
enum ControlRequest {
    Direct(MediaControl),
    SourceHotkey {operation:skylive_core::SourcePlaybackAction,callback:crate::global_hotkeys::Callback},
}
// A deferred Restart remains cancellable until the audio barrier is admitted.
// A short mutex makes the nonblocking queue admission and the commit decision
// atomic with respect to cancellation. No blocking send or callback belongs in
// this critical section.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlGateState { Pending, Cancelled, Committed }
struct ControlGate(Mutex<ControlGateState>);
impl ControlGate {
    fn new() -> Self { Self(Mutex::new(ControlGateState::Pending)) }
    fn cancel(&self) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if *state == ControlGateState::Pending { *state = ControlGateState::Cancelled; }
    }
    fn cancelled(&self) -> bool {
        self.0.lock().map_or(true, |state| *state == ControlGateState::Cancelled)
    }
    fn commit(&self) -> bool {
        let Ok(mut state) = self.0.lock() else { return false; };
        if *state != ControlGateState::Pending { return false; }
        *state = ControlGateState::Committed;
        true
    }
    /// `publish` must be a single nonblocking queue attempt. On failure the
    /// gate remains pending, so scope invalidation may cancel before a retry.
    fn admit<T, E>(&self, publish: impl FnOnce() -> Result<T, E>) -> Result<Result<T, E>, ()> {
        let mut state = self.0.lock().map_err(|_| ())?;
        if *state != ControlGateState::Pending { return Err(()); }
        let result = publish();
        if result.is_ok() { *state = ControlGateState::Committed; }
        Ok(result)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct ControlScope {
    session:u64, revision:u64, output:crate::transition::OutputKey,
    video:[Option<crate::transition::SceneKey>;crate::projector::MAX_VIDEO_VIEWS], program:Option<ProgramCaptureKey>,
}
enum Job {
    Stop {
        key: MediaRequestKey,
    },
    Prepare {
        key: MediaRequestKey,
        path: PathBuf,
    },
    Scope {
        key: MediaRequestKey,
        current: Arc<StudioSnapshot>,
        output: FrameComposition,
        video_views:Vec<crate::transition::FrameScene>,
        sidechain_keys:Vec<Id>,
        now: MediaTime,
        sent: Instant,
        program: Option<ProgramCaptureKey>,
    },
    Control {
        key: MediaRequestKey,
        target: MediaControlTarget,
        request: ControlRequest,
        now: MediaTime,
        sent: Instant,
        gate: Option<Arc<ControlGate>>,
    },
}
struct Update {
    key: MediaRequestKey,
    snapshot: Arc<MediaSnapshot>,
}
pub struct MediaOwner {
    camera_activation:Arc<crate::camera_activation::Snapshot>,
    sink: Arc<OnceLock<MediaAudioSink>>,
    wake: Arc<OnceLock<Wake>>,
    sender: Option<mpsc::SyncSender<Job>>,
    events: mpsc::Receiver<MediaEvent>,
    updates: mpsc::Receiver<Update>,
    handle: Option<thread::JoinHandle<()>>,
    instance: u64,
    session: u64,
    epoch: u64,
    serial: u64,
    busy: bool,
    closed: bool,
    last_now: Option<MediaTime>,
    anchor: Option<(MediaTime, Instant)>,
    snapshot: Arc<MediaSnapshot>,
    last_scope: Option<ControlScope>,
    pending_gate: Option<(MediaRequestKey, Arc<ControlGate>)>,
}
impl MediaOwner {
    pub fn new() -> Result<Self, MediaSourceError> {
        Self::build(None)
    }
    pub fn with_audio_sink(sink: MediaAudioSink) -> Result<Self, MediaSourceError> {
        Self::build(Some(sink))
    }
    fn build(initial_sink: Option<MediaAudioSink>) -> Result<Self, MediaSourceError> {
        let instance = NEXT_INSTANCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| MediaSourceError::IdentityExhausted)?;
        let (sender, jobs) = mpsc::sync_channel(1);
        let (replies, events) = mpsc::sync_channel(1);
        let (outputs, updates) = mpsc::sync_channel(1);
        let sink = Arc::new(OnceLock::new());
        if let Some(initial) = initial_sink {
            let _ = sink.set(initial);
        }
        let wake = Arc::new(OnceLock::new());
        let worker_sink = sink.clone();
        let worker_wake = wake.clone();
        let handle = thread::Builder::new()
            .name("skylive-media-source".into())
            .spawn(move || owner::run(instance, jobs, replies, outputs, worker_sink, worker_wake))
            .map_err(MediaSourceError::Io)?;
        Ok(Self {
            camera_activation:Arc::new(Default::default()),
            sink,
            wake,
            sender: Some(sender),
            events,
            updates,
            handle: Some(handle),
            instance,
            session: 0,
            epoch: 1,
            serial: 0,
            busy: false,
            closed: false,
            last_now: None,
            anchor: None,
            snapshot: Arc::new(MediaSnapshot::empty(0, instance, 1)),
            last_scope: None,
            pending_gate: None,
        })
    }
    /// Single assignment before admission; never replaces or spawns an owner.
    pub fn connect_audio_sink(&mut self, sink: MediaAudioSink) -> Result<(), MediaSourceError> {
        if self.closed {
            return Err(MediaSourceError::Closed);
        }
        if self.serial != 0 {
            return Err(MediaSourceError::Invalid(
                "connect media audio before admission",
            ));
        }
        self.sink
            .set(sink)
            .map_err(|_| MediaSourceError::Invalid("media audio sink already connected"))
    }
    pub fn set_waker(&mut self, wake: Arc<dyn Fn() + Send + Sync>) -> Result<(), MediaSourceError> {
        if self.closed {
            return Err(MediaSourceError::Closed);
        }
        if self.serial != 0 {
            return Err(MediaSourceError::Invalid(
                "set media notifier before admission",
            ));
        }
        self.wake
            .set(wake)
            .map_err(|_| MediaSourceError::Invalid("media notifier already installed"))
    }
    /// Rebase a caller-controlled time domain without admitting a second job.
    /// Busy work remains charged until its real reply, which is fenced by epoch.
    pub fn reset_timing(&mut self, session: &Session) -> Result<(), MediaSourceError> {
        if self.closed || session.is_closed() {
            return Err(MediaSourceError::Closed);
        }
        if let Some((_,gate))=&self.pending_gate {gate.cancel();}
        self.last_scope=None;
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or(MediaSourceError::IdentityExhausted)?;
        self.epoch = epoch;
        self.last_now = None;
        self.anchor = None;
        self.snapshot = Arc::new(MediaSnapshot::empty(
            session.identity(),
            self.instance,
            epoch,
        ));
        Ok(())
    }
    pub fn instance(&self) -> u64 {
        self.instance
    }
    pub fn is_busy(&self) -> bool {
        self.busy
    }
    /// Cancel only a deferred control that has not crossed its audio barrier.
    pub fn invalidate_pending_control(&self) {if let Some((_,gate))=&self.pending_gate{gate.cancel();}}
    pub fn snapshot(&self) -> Arc<MediaSnapshot> {
        self.snapshot.clone()
    }
    pub fn is_playing(&self) -> bool {
        self.snapshot.playing
    }
    pub fn media_now(&self) -> Result<Option<MediaTime>, MediaSourceError> {
        self.anchor
            .map(|(now, instant)| {
                let elapsed = u64::try_from(instant.elapsed().as_nanos())
                    .map_err(|_| MediaSourceError::TimeOverflow)?;
                now.0
                    .checked_add(elapsed)
                    .map(MediaTime)
                    .ok_or(MediaSourceError::TimeOverflow)
            })
            .transpose()
    }
    fn key(&self, session: &Session) -> Result<MediaRequestKey, MediaSourceError> {
        if self.closed || session.is_closed() {
            return Err(MediaSourceError::Closed);
        }
        if self.busy {
            return Err(MediaSourceError::Busy);
        }
        Ok(MediaRequestKey {
            instance: self.instance,
            session: session.identity(),
            revision: session.snapshot().revision(),
            epoch: self.epoch,
            request: self
                .serial
                .checked_add(1)
                .ok_or(MediaSourceError::IdentityExhausted)?,
        })
    }
    fn send(&mut self, key: MediaRequestKey, job: Job) -> Result<(), MediaSourceError> {
        self.sender
            .as_ref()
            .ok_or(MediaSourceError::Closed)?
            .try_send(job)
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => MediaSourceError::Busy,
                mpsc::TrySendError::Disconnected(_) => MediaSourceError::Disconnected,
            })?;
        self.serial = key.request;
        self.busy = true;
        Ok(())
    }
    pub fn prepare(
        &mut self,
        session: &Session,
        path: PathBuf,
    ) -> Result<MediaRequestKey, MediaSourceError> {
        let key = self.key(session)?;
        self.send(key, Job::Prepare { key, path })?;
        Ok(key)
    }
    pub fn synchronize(
        &mut self,
        session: &Session,
        output: &FrameComposition,
        now: MediaTime,
        program: Option<ProgramCaptureKey>,
    ) -> Result<bool, MediaSourceError> {
        self.synchronize_video(session,output,&[],now,program)
    }
    pub fn set_camera_activation_policy(&mut self,policy:Arc<crate::camera_activation::Snapshot>)->bool{
        let changed=!Arc::ptr_eq(&self.camera_activation,&policy);if changed {if let Some((_,gate))=&self.pending_gate{gate.cancel();}}self.camera_activation=policy;changed
    }
    /// Additional video activation never grants Program audio admission. The
    /// original output alone determines PCM routing; video metadata union is bounded40.
    pub fn synchronize_video(&mut self,session:&Session,output:&FrameComposition,
        video_views:&[crate::transition::FrameScene],now:MediaTime,program:Option<ProgramCaptureKey>)->Result<bool,MediaSourceError> {
        if video_views.len()>crate::projector::MAX_VIDEO_VIEWS{return Err(MediaSourceError::Limit("media video view union limit"));}
        for view in video_views {view.validate_session(session.identity()).map_err(|_|MediaSourceError::Invalid("foreign media video view"))?;}
        let scope=ControlScope {session:session.identity(),revision:session.snapshot().revision(),output:output.identity(),
            video:std::array::from_fn(|index|video_views.get(index).map(crate::transition::FrameScene::key)),program};
        if self.last_scope.is_some_and(|old|old!=scope) {if let Some((_,gate))=&self.pending_gate {gate.cancel();}}
        now.0
            .checked_add(1_000_000_000)
            .ok_or(MediaSourceError::TimeOverflow)?;
        if self.busy {
            return Ok(false);
        }
        let changed =
            self.session != session.identity() || self.last_now.is_some_and(|old| now < old);
        let epoch = if changed {
            self.epoch
                .checked_add(1)
                .ok_or(MediaSourceError::IdentityExhausted)?
        } else {
            self.epoch
        };
        let mut key = self.key(session)?;
        key.epoch = epoch;
        let sidechain_keys=crate::audio::sidechain_view_keys(&session.snapshot(),&output.scenes(),
            (self.camera_activation.session()==session.identity()).then_some(self.camera_activation.as_ref()))
            .map_err(|_|MediaSourceError::Limit("media sidechain graph"))?;
        self.send(
            key,
            Job::Scope {
                key,
                current: session.snapshot(),
                output: output.clone(),
                video_views:video_views.to_vec(),
                sidechain_keys,
                now,
                sent: Instant::now(),
                program,
            },
        )?;
        self.last_scope=Some(scope);
        self.epoch = epoch;
        self.session = session.identity();
        self.last_now = Some(now);
        self.anchor = Some((now, Instant::now()));
        if changed {
            self.snapshot = Arc::new(MediaSnapshot::empty(self.session, self.instance, epoch));
        }
        Ok(true)
    }
    pub fn restart(
        &mut self,
        session: &Session,
        source: Id,
        now: MediaTime,
    ) -> Result<MediaRequestKey, MediaSourceError> {
        let target = self.control_target(session, source)?;
        self.control(session, target, MediaControl::Restart, now)
    }
    pub fn stop(&mut self, session: &Session) -> Result<MediaRequestKey, MediaSourceError> {
        let mut key = self.key(session)?;
        key.epoch = self
            .epoch
            .checked_add(1)
            .ok_or(MediaSourceError::IdentityExhausted)?;
        self.send(key, Job::Stop { key })?;
        self.anchor = None;
        self.epoch = key.epoch;
        self.snapshot = Arc::new(MediaSnapshot::empty(
            session.identity(),
            self.instance,
            key.epoch,
        ));
        Ok(key)
    }
    pub fn poll(&mut self, session: &Session) -> Result<Option<MediaEvent>, MediaSourceError> {
        if let Some((key,gate))=&self.pending_gate {if session.is_closed()||key.session!=session.identity()||key.revision!=session.snapshot().revision(){gate.cancel();}}
        // Prepared is always delivered: modal owner must release even stale receipts.
        match self.events.try_recv() {
            Ok(event) => {
                if let MediaEvent::Controlled {key,..}=&event {if self.pending_gate.as_ref().is_some_and(|(pending,_)|pending==key){self.pending_gate=None;}}
                self.busy = false;
                return Ok(Some(event));
            }
            Err(mpsc::TryRecvError::Disconnected) if !self.closed => {
                return Err(MediaSourceError::Disconnected);
            }
            _ => {}
        }
        match self.updates.try_recv() {
            Ok(update) => {
                if update.key.request == self.serial {
                    self.busy = false;
                }
                if update.key.session == session.identity()
                    && update.key.epoch == self.epoch
                    && update.key.instance == self.instance
                {
                    self.snapshot = update.snapshot;
                    return Ok(Some(MediaEvent::Updated { key: update.key }));
                }
                Ok(None)
            }
            Err(mpsc::TryRecvError::Disconnected) if !self.closed => {
                Err(MediaSourceError::Disconnected)
            }
            _ => Ok(None),
        }
    }
    pub fn close(&mut self) {
        if let Some((_,gate))=&self.pending_gate {gate.cancel();}
        self.closed = true;
        self.anchor = None;
        self.snapshot = Arc::new(MediaSnapshot::empty(0, self.instance, self.epoch));
        let (_, events) = mpsc::sync_channel(1);
        self.events = events;
        let (_, updates) = mpsc::sync_channel(1);
        self.updates = updates;
        self.sender = None;
    }
    pub fn join(&mut self) -> thread::Result<()> {
        self.close();
        self.handle.take().map_or(Ok(()), thread::JoinHandle::join)
    }
}
impl Drop for MediaOwner {
    fn drop(&mut self) {
        self.close();
    }
}
#[cfg(test)]
mod tests;
