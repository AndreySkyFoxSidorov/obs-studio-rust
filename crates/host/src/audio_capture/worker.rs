use super::*;
use crate::transition::FrameScene;
use crate::{audio::AudioRouteSnapshot, transition::FrameComposition};
use skylive_core::{Id, Session, SourceKind};
use skylive_media::MediaTime;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

#[path = "program.rs"]
mod program;
#[path = "push_controls.rs"]
mod push_controls;
use program::LiveProgram;
#[cfg(test)]
#[path = "poll_deadline_tests.rs"]
mod poll_deadline_tests;
pub use program::{
    ProgramCaptureKey, ProgramCaptureOutput, ProgramTransportProblem, ProgramTransportStats,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct AudioRequestKey {
    pub session: u64,
    pub revision: u64,
    pub epoch: u64,
    pub request: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioTimingOrigin {
    Device,
    ReceiveEstimate,
}
#[derive(Debug, Serialize)]
pub struct CapturedAudioPacket {
    /// Runtime label of the opened endpoint, never an authored identity.
    pub resolved_device: Option<Arc<str>>,
    pub binding: AudioCaptureBinding,
    pub sequence: u64,
    pub requested_at_ns: u64,
    pub stream_generation: u64,
    #[serde(serialize_with = "serialize_signed_time")]
    pub first_sample_media_ns: i128,
    pub timing_origin: AudioTimingOrigin,
    pub coalesced_packets: u32,
    /// Raw source channel meters; UI applies only scalar authored gain/mute.
    pub meters: Vec<crate::audio::ChannelMeter>,
    pub block: NativePcmBlock,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AudioCaptureProblem {
    pub source: Id,
    pub code: &'static str,
    pub detail: String,
}
#[derive(Clone, Debug)]
pub struct AudioCaptureSnapshot {
    session: u64,
    epoch: u64,
    generation: u64,
    packets: Arc<HashMap<Id, Arc<CapturedAudioPacket>>>,
    problems: Arc<Vec<AudioCaptureProblem>>,
    activation:Arc<crate::camera_activation::Snapshot>,
}
impl AudioCaptureSnapshot {
    fn empty(session: u64, epoch: u64, generation: u64) -> Self {
        Self {
            session,
            epoch,
            generation,
            packets: Arc::new(HashMap::new()),
            problems: Arc::new(Vec::new()),
            activation:Arc::new(Default::default()),
        }
    }
    pub fn source_runtime_active(&self,source:Id)->bool {self.activation.enabled(source)}
    pub fn session(&self) -> u64 {
        self.session
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn packet(&self, source: Id) -> Option<&CapturedAudioPacket> {
        self.packets.get(&source).map(Arc::as_ref)
    }
    pub fn packets(&self) -> impl Iterator<Item = (Id, &CapturedAudioPacket)> {
        self.packets
            .iter()
            .map(|(source, packet)| (*source, packet.as_ref()))
    }
    pub fn packet_count(&self) -> usize {
        self.packets.len()
    }
    pub fn problems(&self) -> &[AudioCaptureProblem] {
        &self.problems
    }
}
#[derive(Debug)]
pub enum AudioCaptureEvent {
    CameraActivation {key:AudioRequestKey},
    Processes { key: AudioRequestKey, result: Result<Vec<AudioDeviceInfo>, AudioCaptureError> },
    Devices {
        key: AudioRequestKey,
        flow: AudioFlow,
        result: Result<Vec<AudioDeviceInfo>, AudioCaptureError>,
    },
    Packets {
        key: AudioRequestKey,
        received: usize,
    },
    Program {
        key: AudioRequestKey,
        result: Result<ProgramTransportStats, AudioCaptureError>,
    },
}
enum Job {
    CameraActivation {key:AudioRequestKey,clock_epoch:u64,changes:Arc<[crate::camera_activation::SourceEpoch]>,policy:Arc<crate::camera_activation::Snapshot>,sources:Vec<(Id,AudioCaptureBinding)>,route:Option<Box<AudioRouteSnapshot>>,rate:u32,now:MediaTime,watch:crate::camera_activation::LegWatch},
    Processes { key: AudioRequestKey },
    Devices {
        key: AudioRequestKey,
        flow: AudioFlow,
    },
    Packets {
        key: AudioRequestKey,
        clock_epoch: u64,
        sources: Vec<(Id, AudioCaptureBinding)>,
        window_sources: Vec<Id>,
        window_observations: Vec<super::window_observation::WindowAudioEvidence>,
        now: MediaTime,
        sent: Instant,
    },
    Program {
        key: AudioRequestKey,
        clock_epoch: u64,
        generation: u64,
        sources: Vec<(Id, AudioCaptureBinding)>,
        window_sources: Vec<Id>,
        window_observations: Vec<super::window_observation::WindowAudioEvidence>,
        route: Box<AudioRouteSnapshot>,
        sent: Instant,
        attachment: Option<crate::audio_output::AudioOutput>,
    },
    StopProgram {
        key: AudioRequestKey,
    },
}
type PacketResult = Result<Option<Arc<CapturedAudioPacket>>, AudioCaptureError>;
enum Reply {
    CameraActivation {key:AudioRequestKey},
    Processes { key: AudioRequestKey, result: Result<Vec<AudioDeviceInfo>, AudioCaptureError> },
    Devices {
        key: AudioRequestKey,
        flow: AudioFlow,
        result: Result<Vec<AudioDeviceInfo>, AudioCaptureError>,
    },
    Packets {
        key: AudioRequestKey,
        results: Vec<(AudioCaptureBinding, PacketResult)>,
        window_results: Vec<(Id, PacketResult)>,
        program: Option<Result<ProgramTransportStats, AudioCaptureError>>,
    },
    ProgramStopped {
        key: AudioRequestKey,
    },
}
/// One admitted job includes its unread completion. Constructor/freshness
/// synchronization never invokes the backend or starts a native stream.
pub struct AudioCaptureWorker {
    push_sender:mpsc::SyncSender<push_controls::Message>,
    push_fault:Arc<std::sync::atomic::AtomicBool>,
    browser_sender:crate::browser::BrowserAudioSink,
    ndi_sender:crate::ndi::NdiAudioSink,
    media_sender: crate::media_source::MediaAudioSink,
    embedded_sender: super::EmbeddedPcmSink,
    sender: Option<mpsc::SyncSender<Job>>,
    receiver: mpsc::Receiver<Reply>,
    handle: Option<thread::JoinHandle<()>>,
    capabilities: AudioCapabilities,
    monitoring_available:bool,
    monitor_video_session:u64,
    monitor_video:Vec<crate::audio::MonitorVideoStamp>,
    defaults: AudioBindingDefaults,
    limits: AudioCaptureLimits,
    session: u64,
    epoch: u64,
    clock_epoch: u64,
    serial: u64,
    busy: bool,
    closed: bool,
    active: Vec<(Id, AudioCaptureBinding)>,
    window_sources: Vec<Id>,
    window_observations: Vec<super::window_observation::WindowAudioEvidence>,
    snapshot: AudioCaptureSnapshot,
    last_requested_at: Option<MediaTime>,
    program_receiver: mpsc::Receiver<ProgramCaptureOutput>,
    program_settings: Option<skylive_core::AudioSettings>,
    program_generation: u64,
    program_anchor: Option<(Instant, MediaTime)>,
    qpc_media_correlation: Option<skylive_media::QpcMediaCorrelation>,
    recording_camera_alignments: Vec<RecordingCameraAlignment>,
    // First verified source anchors survive temporary gaps in video status.
    // They remain scoped by recording/session/audio epoch and camera guard.
    recording_first_cache: Vec<RecordingCameraAlignment>,
    streaming_alignments: Vec<RecordingCameraAlignment>,
    streaming_first_cache: Vec<RecordingCameraAlignment>,
    pending_program_start: Option<AudioRequestKey>,
    program_status: Option<ProgramTransportStats>,
    camera_activation:Arc<crate::camera_activation::Snapshot>,
}
/// Cloneable bounded transport to the existing audio actor. Only the global
/// shortcut owner can submit actual captured edges through this port.
#[derive(Clone)]
pub struct PushAudioSink {sender:mpsc::SyncSender<push_controls::Message>,fault:Arc<std::sync::atomic::AtomicBool>}

/// Per-source alignment for one frame accepted by one recording owner. This
/// is not a Program-wide offset and does not itself mutate the audio mix.
#[derive(Clone, Debug)]
pub struct RecordingCameraAlignment {
    pub recording: u64,
    pub session: u64,
    pub revision: u64,
    pub clock_epoch: u64,
    pub sample_rate: u32,
    pub source: Id,
    pub video_index: u64,
    pub capture_sample: u64,
    /// Maximum graph-clock mapping error before audio-grid rounding.
    pub capture_uncertainty_ns: u64,
    /// Whether all capture-time bounds map to the same audio sample.
    pub capture_sample_exact: bool,
    pub presentation_sample: u64,
    /// Exclusive audio-grid bound covered by the latest accepted video frame.
    pub confirmed_until_sample: u64,
    pub delay_samples: u64,
    // A cross-thread consumer must be able to recheck camera generation after
    // the alignment was computed; the scalar offset alone is not a proof.
    receipt: RecordingVideoReceipt,
}
#[derive(Clone, Debug)]
enum RecordingVideoReceipt { Camera(crate::recording::CameraReceipt), Ndi(crate::recording::NdiVideoReceipt), StreamNdi(crate::streaming::NdiVideoReceipt), StreamCamera(crate::streaming::CameraVideoReceipt) }
impl RecordingVideoReceipt {
    fn current(&self)->bool {match self {Self::Camera(v)=>v.current(),Self::Ndi(v)=>v.current(),Self::StreamNdi(v)=>v.current(),Self::StreamCamera(v)=>v.current()}}
    fn same_scope(&self,other:&Self)->bool {match (self,other) {
        (Self::Camera(a),Self::Camera(b))=>a.same_scope(b),
        (Self::Ndi(a),Self::Ndi(b))=>a.same_scope(b),
        (Self::StreamNdi(a),Self::StreamNdi(b))=>a.same_scope(b),
        (Self::StreamCamera(a),Self::StreamCamera(b))=>a.same_scope(b),
        _=>false,
    }}
}
impl RecordingCameraAlignment {
    pub fn current(&self) -> bool { self.receipt.current() }
    pub(crate) fn is_ndi(&self)->bool {matches!(&self.receipt,RecordingVideoReceipt::Ndi(_)|RecordingVideoReceipt::StreamNdi(_))}
    pub(crate) fn streaming_connection_current(&self)->bool {
        match &self.receipt {RecordingVideoReceipt::StreamNdi(receipt)=>receipt.connection_current(),RecordingVideoReceipt::StreamCamera(receipt)=>receipt.connection_current(),_=>false}
    }
}
impl PushAudioSink {
    pub(crate) fn submit(&self,event:crate::global_hotkeys::Fired,epoch:skylive_core::SourceDefinitionEpoch,settings:skylive_core::PushAudioSettings)->Result<(),String>{
        self.sender.try_send(push_controls::Message{event,epoch,settings}).map_err(|error|{self.fault.store(true,std::sync::atomic::Ordering::Release);format!("push audio input queue: {error}")})
    }
    pub(crate) fn arm(&self){self.fault.store(false,std::sync::atomic::Ordering::Release);}
}
impl AudioCaptureWorker {
    /// Capture only AVC-confirmed video evidence from this streaming sink.
    /// It is never promoted to a Program, recording or network-delivery proof.
    pub fn observe_streaming_ndi_receipts(&mut self,session:&Session,status:&crate::streaming::Status)->Result<(),AudioCaptureError>{
        let current=status.key.session==session.identity()&&!session.is_closed()&&!self.closed;
        self.streaming_first_cache.retain(|old|current&&old.recording==status.key.output
            &&old.clock_epoch==self.clock_epoch&&old.current());
        let mut values=Vec::new();
        if current {
            let rate=session.snapshot().audio().sample_rate();
            for receipt in status.ndi_video_receipts(){
                if receipt.key!=status.key || receipt.connection_epoch!=status.connection_epoch || !receipt.current(){continue;}
                let snapshot=session.snapshot();
                if receipt.revision!=snapshot.revision() || !snapshot.source(receipt.source)
                    .is_some_and(|s|matches!(s.kind(),SourceKind::NdiSource{ndi} if ndi.audio())){continue;}
                let cached=self.streaming_first_cache.iter().find(|old|old.sample_rate==rate&&old.clock_epoch==self.clock_epoch
                    &&matches!(&old.receipt,RecordingVideoReceipt::StreamNdi(value) if value.same_scope(receipt))&&old.current());
                let first=status.first_ndi_video_receipts().iter().find(|first|first.same_scope(receipt)&&first.current());
                let (anchor,capture_sample)=if let Some(old)=cached {
                    (match &old.receipt {RecordingVideoReceipt::StreamNdi(value)=>value,_=>continue},old.capture_sample)
                } else {
                    let first=first.unwrap_or(receipt);
                    let Some(ns)=first.estimated_sender_media_ns.and_then(|ns|u128::try_from(ns).ok()) else{continue;};
                    let Some(sample)=ns.checked_mul(u128::from(rate)).and_then(|v|u64::try_from(v.div_ceil(1_000_000_000)).ok()) else{continue;};
                    (first,sample)
                };
                let Some(presentation_sample)=u64::try_from((u128::from(anchor.presentation_at.0)*u128::from(rate)).div_ceil(1_000_000_000)).ok() else{continue;};
                let Some(confirmed_until_sample)=u64::try_from((u128::from(receipt.presentation_end.0)*u128::from(rate)).div_ceil(1_000_000_000)).ok() else{continue;};
                let Some(delay_samples)=presentation_sample.checked_sub(capture_sample) else{continue;};
                if confirmed_until_sample<presentation_sample || delay_samples>u64::from(rate)*21 || !anchor.current(){continue;}
                let value=RecordingCameraAlignment {recording:status.key.output,session:status.key.session,revision:receipt.revision,
                    clock_epoch:self.clock_epoch,sample_rate:rate,source:receipt.source,video_index:anchor.video_index,
                    capture_sample,capture_uncertainty_ns:u64::MAX,capture_sample_exact:false,presentation_sample,confirmed_until_sample,
                    delay_samples,receipt:RecordingVideoReceipt::StreamNdi(anchor.clone())};
                if values.len()>=crate::audio::MAX_AUDIO_INPUTS{return Err(AudioCaptureError::Limit("streaming NDI proof count"));}
                if values.iter().any(|old:&RecordingCameraAlignment|old.source==value.source){return Err(AudioCaptureError::Invalid("duplicate streaming NDI proof"));}
                values.try_reserve(1).map_err(|_|AudioCaptureError::Limit("streaming NDI proof allocation"))?;
                if !self.streaming_first_cache.iter().any(|old|old.receipt.same_scope(&value.receipt)) {
                    if self.streaming_first_cache.len()>=crate::audio::MAX_AUDIO_INPUTS{return Err(AudioCaptureError::Limit("streaming NDI anchor count"));}
                    self.streaming_first_cache.try_reserve(1).map_err(|_|AudioCaptureError::Limit("streaming NDI anchor allocation"))?;
                    self.streaming_first_cache.push(value.clone());
                }
                values.push(value);
            }
            // Camera timestamps are mapped through the bounded QPC/media
            // correlation, not through NDI's arrival-time estimate. Keep the
            // first usable receipt for this connection and exact history.
            for receipt in status.camera_video_receipts() {
                if receipt.key!=status.key || receipt.connection_epoch!=status.connection_epoch || !receipt.current(){continue;}
                let snapshot=session.snapshot();
                if receipt.revision!=snapshot.revision() || !snapshot.source(receipt.source)
                    .is_some_and(|s|matches!(s.kind(),SourceKind::VideoCapture{binding} if binding.audio().is_some() || binding.embedded_audio())) {continue;}
                let map=|value:&crate::streaming::CameraVideoReceipt| self.qpc_media_correlation?
                    .estimate_interval(session.identity(),self.clock_epoch,value.capture_qpc_bounds_ns,rate,5_000_000);
                let cached=self.streaming_first_cache.iter().find(|old|old.sample_rate==rate && old.clock_epoch==self.clock_epoch
                    && matches!(&old.receipt,RecordingVideoReceipt::StreamCamera(value) if value.same_scope(receipt)) && old.current());
                let first=status.first_camera_video_receipts().iter().find(|first|first.same_scope(receipt) && first.current());
                let (anchor,estimate)=if let Some(old)=cached {
                    (match &old.receipt {RecordingVideoReceipt::StreamCamera(value)=>value,_=>continue},
                     skylive_media::SampleTimeEstimate {sample:old.capture_sample,uncertainty_ns:old.capture_uncertainty_ns,exact:old.capture_sample_exact})
                } else if let Some((first,estimate))=first.and_then(|first|map(first).map(|estimate|(first,estimate))) {
                    (first,estimate)
                } else {
                    let Some(estimate)=map(receipt) else{continue;}; (receipt,estimate)
                };
                let Some(presentation_sample)=u64::try_from((u128::from(anchor.presentation_at.0)*u128::from(rate)).div_ceil(1_000_000_000)).ok() else{continue;};
                let Some(confirmed_until_sample)=u64::try_from((u128::from(receipt.presentation_end.0)*u128::from(rate)).div_ceil(1_000_000_000)).ok() else{continue;};
                let Some(delay_samples)=presentation_sample.checked_sub(estimate.sample) else{continue;};
                if confirmed_until_sample<presentation_sample || delay_samples>u64::from(rate)*21 || !anchor.current(){continue;}
                let value=RecordingCameraAlignment {recording:status.key.output,session:status.key.session,revision:receipt.revision,
                    clock_epoch:self.clock_epoch,sample_rate:rate,source:receipt.source,video_index:anchor.video_index,
                    capture_sample:estimate.sample,capture_uncertainty_ns:estimate.uncertainty_ns,capture_sample_exact:estimate.exact,
                    presentation_sample,confirmed_until_sample,delay_samples,receipt:RecordingVideoReceipt::StreamCamera(anchor.clone())};
                if values.len()>=crate::audio::MAX_AUDIO_INPUTS{return Err(AudioCaptureError::Limit("streaming camera proof count"));}
                if values.iter().any(|old:&RecordingCameraAlignment|old.source==value.source){return Err(AudioCaptureError::Invalid("duplicate streaming camera proof"));}
                values.try_reserve(1).map_err(|_|AudioCaptureError::Limit("streaming camera proof allocation"))?;
                if !self.streaming_first_cache.iter().any(|old|old.receipt.same_scope(&value.receipt)) {
                    if self.streaming_first_cache.len()>=crate::audio::MAX_AUDIO_INPUTS{return Err(AudioCaptureError::Limit("streaming camera anchor count"));}
                    self.streaming_first_cache.try_reserve(1).map_err(|_|AudioCaptureError::Limit("streaming camera anchor allocation"))?;
                    self.streaming_first_cache.push(value.clone());
                }
                values.push(value);
            }
        }
        self.streaming_alignments=values;Ok(())
    }
    /// Capture only accepted recording-specific camera proofs. This does not
    /// change Program or any output; the audio owner receives them with the
    /// next validated routing snapshot for a future separate recording mix.
    pub fn observe_recording_camera_receipts(&mut self, session:&Session, status:&crate::recording::RecordingStatus)->Result<(),AudioCaptureError>{
        if !status.active || status.finishing || status.session!=session.identity() {
            self.recording_first_cache.clear();
        } else {
            let clock_epoch=self.clock_epoch;
            self.recording_first_cache.retain(|old|old.recording==status.recording && old.session==session.identity()
                && old.clock_epoch==clock_epoch && old.current());
        }
        let mut values=Vec::new();
        if status.active && !status.finishing && !status.paused && status.session==session.identity() {
            let receipts=status.camera_receipts();
            if receipts.len()>crate::audio::MAX_AUDIO_INPUTS {return Err(AudioCaptureError::Limit("recording camera receipt count"));}
            values.try_reserve_exact(receipts.len()).map_err(|_|AudioCaptureError::Limit("recording camera receipt allocation"))?;
            let rate=session.snapshot().audio().sample_rate();
            for receipt in receipts {
                if let Some(value)=self.recording_camera_alignment(session,status,receipt.source,rate) {
                    if values.iter().any(|old:&RecordingCameraAlignment|old.source==value.source) {return Err(AudioCaptureError::Invalid("duplicate recording camera receipt"));}
                    if !self.recording_first_cache.iter().any(|old|old.receipt.same_scope(&value.receipt)) {
                        if self.recording_first_cache.len()>=crate::audio::MAX_AUDIO_BLOCK_SOURCES {return Err(AudioCaptureError::Limit("recording camera proof cache"));}
                        self.recording_first_cache.try_reserve(1).map_err(|_|AudioCaptureError::Limit("recording camera proof allocation"))?;
                        self.recording_first_cache.push(value.clone());
                    }
                    values.push(value);
                }
            }
            for receipt in status.ndi_video_receipts() {
                if let Some(value)=self.recording_ndi_alignment(session,status,receipt.source,rate) {
                    if values.iter().any(|old:&RecordingCameraAlignment|old.source==value.source) {return Err(AudioCaptureError::Invalid("duplicate recording delayed source receipt"));}
                    if values.len()>=crate::audio::MAX_AUDIO_INPUTS {return Err(AudioCaptureError::Limit("recording delayed source count"));}
                    values.try_reserve(1).map_err(|_|AudioCaptureError::Limit("recording delayed source allocation"))?;
                    if !self.recording_first_cache.iter().any(|old|old.receipt.same_scope(&value.receipt)) {
                        if self.recording_first_cache.len()>=crate::audio::MAX_AUDIO_BLOCK_SOURCES {return Err(AudioCaptureError::Limit("recording source proof cache"));}
                        self.recording_first_cache.try_reserve(1).map_err(|_|AudioCaptureError::Limit("recording source proof allocation"))?;
                        self.recording_first_cache.push(value.clone());
                    }
                    values.push(value);
                }
            }
        }
        self.recording_camera_alignments=values;
        Ok(())
    }
    /// Refresh a bounded correlation with the desktop playback clock. This is
    /// only a capture-time mapping: it is not proof of Program presentation.
    pub fn observe_media_clock(&mut self, session: &Session, origin: Instant) {
        self.qpc_media_correlation = if self.closed || session.is_closed() || self.session != session.identity() {
            None
        } else {
            native::correlate_media_clock(origin, session.identity(), self.clock_epoch)
        };
    }

    /// Return a sample only when calibration, session, epoch and rounding are
    /// all unambiguous. Missing device timestamps must remain unavailable.
    pub fn calibrated_capture_sample(&self, session: &Session, qpc_ns: Option<u64>, sample_rate: u32) -> Option<u64> {
        if self.closed || session.is_closed() || self.session != session.identity() { return None; }
        self.qpc_media_correlation?.exact_sample(session.identity(), self.clock_epoch, qpc_ns?, sample_rate)
    }

    /// Map an actual recording-writer acceptance to this owner's sample grid.
    /// An absent/ambiguous clock, obsolete source revision, paused recording or
    /// stale camera generation yields no alignment. Each caller still needs a
    /// sink-specific source-history reader before applying the offset.
    pub fn recording_camera_alignment(&self, session:&Session, status:&crate::recording::RecordingStatus,
        source:Id, sample_rate:u32)->Option<RecordingCameraAlignment> {
        if !status.active || status.finishing || status.paused || status.session!=session.identity() {return None;}
        let snapshot=session.snapshot();
        let latest=status.camera_receipts().iter().find(|receipt|
            receipt.recording==status.recording && receipt.session==session.identity()
                && receipt.revision==snapshot.revision() && receipt.source==source && receipt.current())?;
        if !snapshot.source(source).is_some_and(|source|matches!(source.kind(),skylive_core::SourceKind::VideoCapture{binding} if binding.audio().is_some() || binding.embedded_audio())) {return None;}
        // Keep the receipt and bounded estimate of the first usable anchor. A
        // DirectShow interval can exceed the error budget for the first frame
        // but become usable later; in that case anchor at the later frame,
        // rather than mixing a cached sample with an unrelated receipt.
        let map_sample=|receipt:&crate::recording::CameraReceipt| {
            let (before,after)=receipt.capture_qpc_bounds_ns;
            if self.closed || session.is_closed() || self.session != session.identity() {return None;}
            self.qpc_media_correlation?.estimate_interval(session.identity(),self.clock_epoch,
                (before,after),sample_rate,5_000_000)
        };
        let cached=self.recording_first_cache.iter().find(|old|old.clock_epoch==self.clock_epoch
            && old.sample_rate==sample_rate && matches!(&old.receipt,RecordingVideoReceipt::Camera(receipt) if receipt.same_scope(latest)) && old.current());
        let first=status.first_camera_receipts().iter().find(|first|first.same_scope(latest) && first.current());
        let (receipt,estimate)=if let Some(old)=cached {
            (match &old.receipt {RecordingVideoReceipt::Camera(receipt)=>receipt,_=>return None},skylive_media::SampleTimeEstimate {sample:old.capture_sample,uncertainty_ns:old.capture_uncertainty_ns,exact:old.capture_sample_exact})
        } else if let Some((receipt,sample))=first.and_then(|receipt|map_sample(receipt).map(|sample|(receipt,sample))) {
            (receipt,sample)
        } else {
            (latest,map_sample(latest)?)
        };
        let presentation_sample=u64::try_from((u128::from(receipt.presentation_at.0)*u128::from(sample_rate)).div_ceil(1_000_000_000)).ok()?;
        let confirmed_until_sample=u64::try_from((u128::from(latest.presentation_end.0)*u128::from(sample_rate)).div_ceil(1_000_000_000)).ok()?;
        if confirmed_until_sample<presentation_sample{return None;}
        let capture_sample=estimate.sample;
        let delay_samples=presentation_sample.checked_sub(capture_sample)?;
        if delay_samples > u64::from(sample_rate).checked_mul(21)? {return None;}
        if !receipt.current() {return None;}
        Some(RecordingCameraAlignment {recording:receipt.recording,session:receipt.session,revision:receipt.revision,
            clock_epoch:self.clock_epoch,sample_rate,source,video_index:receipt.video_index,
            capture_sample,capture_uncertainty_ns:estimate.uncertainty_ns,capture_sample_exact:estimate.exact,
            presentation_sample,confirmed_until_sample,delay_samples,receipt:RecordingVideoReceipt::Camera(receipt.clone())})
    }

    /// A recording-only NDI alignment. Both NDI audio and video use the same
    /// established sender clock and arrival anchor. The local anchor is an
    /// estimate with unknown network error; it is never marked exact and does
    /// not authorize changing Program or another output's audio.
    fn recording_ndi_alignment(&self,session:&Session,status:&crate::recording::RecordingStatus,
        source:Id,sample_rate:u32)->Option<RecordingCameraAlignment> {
        if self.closed || session.is_closed() || self.session!=session.identity() || !status.active
            || status.finishing || status.paused || status.session!=session.identity() {return None;}
        let snapshot=session.snapshot();
        if !snapshot.source(source).is_some_and(|s|matches!(s.kind(),skylive_core::SourceKind::NdiSource{ndi} if ndi.audio())) {return None;}
        let latest=status.ndi_video_receipts().iter().find(|r|r.recording==status.recording
            && r.session==session.identity() && r.revision==snapshot.revision() && r.source==source && r.current())?;
        let cached=self.recording_first_cache.iter().find(|old|old.clock_epoch==self.clock_epoch
            && old.sample_rate==sample_rate && matches!(&old.receipt,RecordingVideoReceipt::Ndi(receipt) if receipt.same_scope(latest)) && old.current());
        let first=status.first_ndi_video_receipts().iter().find(|r|r.same_scope(latest)&&r.current());
        let (receipt,capture_sample)=if let Some(old)=cached {
            (match &old.receipt {RecordingVideoReceipt::Ndi(receipt)=>receipt,_=>return None},old.capture_sample)
        } else {
            let receipt=first.unwrap_or(latest);
            let estimate=u128::try_from(receipt.estimated_sender_media_ns?).ok()?;
            let sample=u64::try_from((estimate.checked_mul(u128::from(sample_rate))?).div_ceil(1_000_000_000)).ok()?;
            (receipt,sample)
        };
        let presentation_sample=u64::try_from((u128::from(receipt.presentation_at.0)*u128::from(sample_rate)).div_ceil(1_000_000_000)).ok()?;
        let confirmed_until_sample=u64::try_from((u128::from(latest.presentation_end.0)*u128::from(sample_rate)).div_ceil(1_000_000_000)).ok()?;
        if confirmed_until_sample<presentation_sample{return None;}
        let delay_samples=presentation_sample.checked_sub(capture_sample)?;
        if delay_samples>u64::from(sample_rate).checked_mul(21)? || !receipt.current(){return None;}
        Some(RecordingCameraAlignment {recording:receipt.recording,session:receipt.session,revision:receipt.revision,
            clock_epoch:self.clock_epoch,sample_rate,source,video_index:receipt.video_index,capture_sample,
            capture_uncertainty_ns:u64::MAX,capture_sample_exact:false,presentation_sample,confirmed_until_sample,
            delay_samples,receipt:RecordingVideoReceipt::Ndi(receipt.clone())})
    }

    pub fn native() -> std::io::Result<Self> {
        let mut worker = Self::with_monitoring_policy(native::factory(),native::capabilities(),AudioCaptureLimits::default(),true)?;
        worker.defaults = native::binding_defaults();
        Ok(worker)
    }
    pub fn new(
        factory: AudioBackendFactory,
        capabilities: AudioCapabilities,
    ) -> std::io::Result<Self> {
        Self::with_limits(factory, capabilities, AudioCaptureLimits::default())
    }
    pub fn with_limits(
        factory: AudioBackendFactory,
        capabilities: AudioCapabilities,
        limits: AudioCaptureLimits,
    ) -> std::io::Result<Self> {Self::with_monitoring_policy(factory,capabilities,limits,false)}
    fn with_monitoring_policy(factory:AudioBackendFactory,capabilities:AudioCapabilities,limits:AudioCaptureLimits,native_monitoring:bool)->std::io::Result<Self> {
        if limits.max_devices == 0
            || limits.max_devices > 64
            || limits.max_streams == 0
            || limits.max_streams > 16
            || limits.max_sources == 0
            || limits.max_sources > 3 * skylive_core::MAX_SOURCES
            || limits.max_frames == 0
            || limits.max_frames > 4096
            || limits.max_channels == 0
            || limits.max_channels > 8
            || limits.max_packet_bytes == 0
            || limits.max_packet_bytes > 128 * 1024
            || limits.max_native_buffer_bytes == 0
            || limits.max_native_buffer_bytes > 4 * 1024 * 1024
            || limits.max_batch_bytes == 0
            || limits.max_batch_bytes > 8 * 1024 * 1024
            || limits.max_resident_bytes < limits.max_packet_bytes
            || limits.max_resident_bytes > 32 * 1024 * 1024
            || limits.callback_slots == 0
            || limits.callback_slots > 32
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid audio capture limits",
            ));
        }
        let (sender, jobs) = mpsc::sync_channel(1);
        let (replies, receiver) = mpsc::sync_channel(1);
        let (program_outputs, program_receiver) = mpsc::sync_channel(8);
        let (media_sender, media_packets) = mpsc::sync_channel(64);
        let (ndi_sender,ndi_packets)=mpsc::sync_channel(64);
        let (browser_sender,browser_packets)=mpsc::sync_channel(64);
        let (embedded_sender,embedded_packets)=mpsc::sync_channel(16);
        let (push_sender,push_events)=mpsc::sync_channel(push_controls::CAPACITY);
        let push_fault=Arc::new(std::sync::atomic::AtomicBool::new(false));let actor_push_fault=push_fault.clone();
        let handle = thread::Builder::new()
            .name("skylive-audio-capture".into())
            .spawn(move || {
                run(
                    factory,
                    jobs,
                    replies,
                    program_outputs,
                    media_packets,
                    ndi_packets,
                    browser_packets,
                    embedded_packets,
                    push_events,
                    actor_push_fault,
                    limits,
                    native_monitoring,
                )
            })?;
        Ok(Self {
            push_sender,
            push_fault,
            ndi_sender:crate::ndi::NdiAudioSink {sender:ndi_sender},
            browser_sender:crate::browser::BrowserAudioSink {sender:browser_sender},
            embedded_sender:super::EmbeddedPcmSink {sender:embedded_sender},
            media_sender: crate::media_source::MediaAudioSink {
                sender: media_sender,
            },
            sender: Some(sender),
            receiver,
            handle: Some(handle),
            capabilities,
            monitoring_available:native_monitoring&&cfg!(target_os="windows"),
            monitor_video_session:0,
            monitor_video:Vec::new(),
            defaults: AudioBindingDefaults::default(),
            limits,
            session: 0,
            epoch: 1,
            clock_epoch: 1,
            serial: 0,
            busy: false,
            closed: false,
            active: Vec::new(),
            window_sources: Vec::new(),
            window_observations: Vec::new(),
            snapshot: AudioCaptureSnapshot::empty(0, 1, 0),
            last_requested_at: None,
            program_receiver,
            program_settings: None,
            program_generation: 0,
            program_anchor: None,
            qpc_media_correlation: None,
            recording_camera_alignments: Vec::new(),
            recording_first_cache: Vec::new(),
            streaming_alignments: Vec::new(),
            streaming_first_cache: Vec::new(),
            pending_program_start: None,
            program_status: None,
            camera_activation:Arc::new(Default::default()),
        })
    }
    pub fn push_audio_sink(&self)->PushAudioSink{PushAudioSink{sender:self.push_sender.clone(),fault:self.push_fault.clone()}}
    /// Metadata from actual decoded/captured video. Ordinary camera time is the
    /// first observation on the session clock. For an authenticated delayed
    /// camera frame use its original device clock only when the session clock
    /// correlation proves an unambiguous audio-grid position. Other providers
    /// retain their existing presentation stamp. This is monitor timing, not
    /// evidence that a frame has been published by a Program output.
    pub fn set_monitor_video_context(&mut self, session:&Session,
        capture:&crate::capture::CaptureSnapshot, media:&crate::media_source::MediaSnapshot, now:MediaTime)
        -> Result<(),AudioCaptureError>
    {
        let model=session.snapshot();
        let mut next=Vec::new();
        let mut bytes=crate::audio::MAX_AUDIO_INPUTS*std::mem::size_of::<crate::audio::MonitorVideoStamp>();
        for source in model.sources().iter().filter(|source| !source.audio_monitoring().is_off()
            || matches!(source.kind(),SourceKind::NdiSource {ndi} if ndi.audio()) && source.video_filters().iter().any(|filter|
                filter.enabled && model.preferences().plugin_enabled(filter.plugin_id())
                    && matches!(filter.effect,skylive_core::VideoEffect::AsyncVideoDelay {delay_ms:1..=u16::MAX}))
            || matches!(source.kind(),SourceKind::VideoCapture {binding} if binding.audio().is_some() || binding.embedded_audio()) && source.video_filters().iter().any(|filter|
                filter.enabled && model.preferences().plugin_enabled(filter.plugin_id())
                    && matches!(filter.effect,skylive_core::VideoEffect::AsyncVideoDelay {delay_ms:1..=u16::MAX}))) {
            // Inspect borrowed binding data first; no path/CSS is cloned before
            // both vector and string credit have been admitted.
            let mut camera_estimate_uncertainty_ns=None;
            let mut camera_delay_receipt=None;
            let (stamp,strings,camera_sample,camera_delay_scope)=match source.kind() {
                SourceKind::VideoCapture {binding} if binding.audio().is_some() || binding.embedded_audio()=>{
                    let Some(frame)=capture.frame(source.id()).filter(|frame|capture.session()==session.identity()&&frame.binding.matches_source(source.kind()))else{continue;};
                    let Some(origin @ crate::capture::NativeFrameOrigin::Camera {..})=frame.native_origin else {continue;};
                    let Some(clock_epoch)=frame.native_clock_epoch else {continue;};
                    let strings=if binding.embedded_audio(){match binding.target(){skylive_core::VideoCaptureTarget::Windows{symbolic_link}=>symbolic_link.len(),skylive_core::VideoCaptureTarget::Macos{unique_id}=>unique_id.len()}}else{let audio=binding.audio().ok_or(AudioCaptureError::Invalid("camera audio binding disappeared"))?;match audio.target(){skylive_core::AudioTarget::WindowsEndpoint {id}=>id.len(),_=>0}};
                    // For ordinary camera samples requested_at is the capture
                    // activation time, so observe each new sample once below.
                    // A delayed sample instead needs its original capture time:
                    // assigning `now` would make monitoring ignore its delay.
                    let (stamp,delay_scope)=if let Some(receipt)=frame.async_delay.as_ref() {
                        let providers=crate::video_filters::FilterProviders::from_snapshot(&model);
                        if !crate::video_filters::camera_delay_applied(source,session.identity(),frame,&frame.image,providers) {continue;}
                        // The monitor history is bounded to approximately 22 s.
                        // Larger/multiple delays cannot be represented honestly.
                        if receipt.elapsed_ns>21_000_000_000 {continue;}
                        let rate=model.audio().sample_rate();
                        let calibrated=self.calibrated_capture_sample(session,frame.device_qpc_ns,rate).or_else(|| {
                            let (before,after)=frame.directshow_clock?.qpc_interval(frame.native_timestamp_ns?)?;
                            let estimate=self.qpc_media_correlation?.estimate_interval(session.identity(),self.clock_epoch,
                                (before,after),rate,5_000_000)?;
                            if !estimate.exact {camera_estimate_uncertainty_ns=Some(estimate.uncertainty_ns);}
                            Some(estimate.sample)
                        });
                        let Some(sample)=calibrated else {continue;};
                        let ns=u64::try_from(u128::from(sample)*1_000_000_000/u128::from(rate))
                            .map_err(|_|AudioCaptureError::TimeOverflow)?;
                        camera_delay_receipt=Some(receipt.clone());
                        (MediaTime(ns),Some((receipt.owner,receipt.stream,receipt.clock_epoch,receipt.binding_generation,receipt.history_generation)))
                    } else {(now,None)};
                    (stamp,strings,Some((origin,clock_epoch,frame.sequence)),delay_scope)
                },
                SourceKind::MediaSource {media:binding} if binding.has_audio()&&binding.video_dimensions().is_some()=>{
                    let Some(frame)=media.frame(source.id()).filter(|frame| media.session()==session.identity()&&frame.media.as_ref()==binding)else{continue;};
                    (frame.requested_at,binding.path().len(),None,None)
                },
                SourceKind::NdiSource {ndi} if ndi.audio()=>{
                    let Some(frame)=capture.frame(source.id()).filter(|frame|capture.session()==session.identity()&&frame.binding.matches_source(source.kind()))else{continue;};
                    let strings=ndi.connection().sender.name.len()+ndi.connection().sender.url.as_ref().map_or(0,String::len);
                    if let Some(receipt)=frame.async_delay.as_ref() {
                        let providers=crate::video_filters::FilterProviders::from_snapshot(&model);
                        if !crate::video_filters::ndi_recording_delay_applied(source,session.identity(),frame,&frame.image,providers)
                            || receipt.elapsed_ns>21_000_000_000 {continue;}
                        let Some(ns)=receipt.ndi_media_time_estimate_ns.and_then(|ns|u64::try_from(ns).ok()) else {continue;};
                        let Some(origin @ crate::capture::NativeFrameOrigin::Ndi {..})=frame.native_origin else {continue;};
                        let Some(clock_epoch)=frame.native_clock_epoch else {continue;};
                        // The shared sender clock is anchored to packet arrival;
                        // its absolute network uncertainty is not bounded.
                        camera_estimate_uncertainty_ns=Some(u64::MAX);
                        camera_delay_receipt=Some(receipt.clone());
                        (MediaTime(ns),strings,Some((origin,clock_epoch,frame.sequence)),
                            Some((receipt.owner,receipt.stream,receipt.clock_epoch,receipt.binding_generation,receipt.history_generation)))
                    } else {(frame.requested_at,strings,None,None)}
                },
                SourceKind::WindowCapture {binding} if binding.capture_audio()=>{
                    let Some(frame)=capture.frame(source.id()).filter(|frame|capture.session()==session.identity()&&frame.binding.matches_source(source.kind()))else{continue;};
                    let strings=match binding.target(){skylive_core::WindowTarget::Windows{title,class,executable}=>title.len()+class.len()+executable.len(),skylive_core::WindowTarget::Macos{title,bundle_id}=>title.len()+bundle_id.len()};
                    (frame.requested_at,strings,None,None)
                },
                SourceKind::Browser {browser} if browser.control_audio()=>{
                    let Some(frame)=capture.frame(source.id()).filter(|frame|capture.session()==session.identity()&&frame.binding.matches_source(source.kind()))else{continue;};
                    (frame.requested_at,browser.css().len()+match browser.location(){skylive_core::BrowserLocation::Url {url}=>url.len(),skylive_core::BrowserLocation::LocalFile {path}=>path.len()},None,None)
                },
                _=>continue,
            };
            // Clock reset can leave a published old video snapshot temporarily.
            // Omit its stamp; never relabel that frame with the new clock time.
            if stamp.0>now.0 {continue;}
            bytes=bytes.checked_add(strings).filter(|bytes|*bytes<=512*1024)
                .ok_or(AudioCaptureError::Limit("monitor video binding metadata"))?;
            if next.len()==crate::audio::MAX_AUDIO_INPUTS {return Err(AudioCaptureError::Limit("monitor video source count"));}
            if next.is_empty(){next.try_reserve_exact(crate::audio::MAX_AUDIO_INPUTS).map_err(|_|AudioCaptureError::Limit("monitor video metadata allocation"))?;}
            let binding=match source.kind() {
                SourceKind::VideoCapture {binding} if binding.embedded_audio()=>crate::audio::AudioInputKey::EmbeddedCamera{source:source.id(),binding:binding.video_only()},
                SourceKind::VideoCapture {binding}=>crate::audio::AudioInputKey::Native(binding.audio()
                    .ok_or(AudioCaptureError::Invalid("camera audio binding disappeared"))?.clone()),
                SourceKind::WindowCapture {binding}=>crate::audio::AudioInputKey::Window {source:source.id(),binding:binding.audio()
                    .ok_or(AudioCaptureError::Invalid("window audio binding disappeared"))?.clone()},
                SourceKind::MediaSource {media}=>crate::audio::AudioInputKey::Media {source:source.id(),media:media.clone()},
                SourceKind::NdiSource {ndi}=>crate::audio::AudioInputKey::Ndi {source:source.id(),ndi:ndi.clone()},
                SourceKind::Browser {browser}=>crate::audio::AudioInputKey::Browser(crate::browser::BrowserAudioKey {source:source.id(),browser:Arc::new(browser.clone())}),
                _=>unreachable!("borrowed monitor video binding was admitted above"),
            };
            let stamp=if let Some(sample)=camera_sample {
                // A repeated UI snapshot does not make an old frame fresh.
                // On clock rollback also avoid assigning it a new timestamp.
                if self.monitor_video_session==session.identity() {
                    if let Some(old)=self.monitor_video.iter().find(|old|old.source==source.id()&&old.binding==binding&&old.camera_sample==Some(sample)&&old.camera_delay_scope==camera_delay_scope) {
                        // Keep even a future observation as an identity fence:
                        // with_monitor_video omits it until the clock catches up
                        // or an actually new sample replaces it.
                        camera_estimate_uncertainty_ns=old.camera_estimate_uncertainty_ns;
                        old.at
                    } else {stamp}
                } else {stamp}
            } else {stamp};
            next.push(crate::audio::MonitorVideoStamp {source:source.id(),binding,at:stamp,camera_sample,camera_delay_scope,camera_delay_receipt,camera_estimate_uncertainty_ns});
        }
        self.monitor_video=next;
        self.monitor_video_session=session.identity();
        Ok(())
    }
    pub(crate) fn set_camera_activation_policy(&mut self,policy:Arc<crate::camera_activation::Snapshot>)->Result<(),AudioCaptureError>{
        if self.closed {return Err(AudioCaptureError::Closed);}
        if Arc::ptr_eq(&self.camera_activation,&policy){return Ok(());}
        let packets=self.snapshot.packets.iter().filter(|(source,_)|policy.enabled(**source)&&self.camera_activation.epoch(**source)==policy.epoch(**source)).map(|(id,packet)|(*id,packet.clone())).collect();
        self.active.retain(|(source,_)|policy.enabled(*source));
        self.snapshot.packets=Arc::new(packets);self.snapshot.activation=policy.clone();self.camera_activation=policy;
        Ok(())
    }
    pub(crate) fn request_camera_activation(&mut self,session:&Session,changes:Arc<[crate::camera_activation::SourceEpoch]>,output:Option<&FrameComposition>,now:MediaTime,watch:crate::camera_activation::LegWatch)->Result<(),AudioCaptureError>{
        self.camera_activation.validate(session).map_err(|_|AudioCaptureError::Invalid("camera activation source binding changed"))?;
        let key=self.key(session)?;
        if output.is_none()&&self.program_settings.is_some()&&changes.iter().any(|e|e.enabled){return Err(AudioCaptureError::Busy);}
        let route=output.map(|output|AudioRouteSnapshot::capture(session,output,now).and_then(|route|route.with_camera_activation(self.camera_activation.clone()))).transpose().map_err(|e|AudioCaptureError::Native {operation:"camera AV Program route",detail:e.to_string()})?;
        self.admit(Job::CameraActivation {key,clock_epoch:self.clock_epoch,changes,policy:self.camera_activation.clone(),sources:self.active.clone(),route:route.map(Box::new),rate:session.snapshot().audio().sample_rate(),now,watch})
    }
    pub fn monitoring_available(&self)->bool{self.monitoring_available}
    pub fn capabilities(&self) -> AudioCapabilities {
        self.capabilities
    }
    pub fn binding_defaults(&self) -> AudioBindingDefaults {
        self.defaults
    }
    pub fn snapshot(&self) -> AudioCaptureSnapshot {
        self.snapshot.clone()
    }
    pub fn is_busy(&self) -> bool {
        if self.closed {
            self.handle
                .as_ref()
                .is_some_and(|handle| !handle.is_finished())
        } else {
            self.busy
        }
    }
    pub fn active_source_count(&self) -> usize {
        self.active.len()
    }
    pub fn last_requested_at(&self) -> Option<MediaTime> {
        self.last_requested_at
    }
    pub fn program_status(&self) -> Option<&ProgramTransportStats> {
        self.program_status.as_ref()
    }
    /// Nonblocking bounded file PCM transport into this same processing owner.
    /// Cloning this sink never initializes a native device/backend.
    pub fn browser_sink(&self)->crate::browser::BrowserAudioSink{self.browser_sender.clone()}
    pub fn ndi_sink(&self)->crate::ndi::NdiAudioSink{self.ndi_sender.clone()}
    pub fn embedded_sink(&self)->super::EmbeddedPcmSink{self.embedded_sender.clone()}
    pub fn embedded_clock(&self,session:&Session)->Option<skylive_media::QpcMediaCorrelation>{
        if self.closed || session.is_closed() || self.session!=session.identity(){None}else{self.qpc_media_correlation}
    }
    pub fn media_sink(&self) -> crate::media_source::MediaAudioSink {
        self.media_sender.clone()
    }
    pub fn program_key(&self) -> Option<ProgramCaptureKey> {
        self.program_settings.map(|settings| ProgramCaptureKey {
            session: self.session,
            clock_epoch: self.clock_epoch,
            generation: self.program_generation,
            settings,
        })
    }
    /// A status projected against the caller's current configuration. A busy
    /// old control must not label old-policy counters as the newly authored one.
    pub fn program_status_for(&self, session: &Session) -> Option<&ProgramTransportStats> {
        if self.closed
            || session.is_closed()
            || self.session != session.identity()
            || self.program_settings != Some(session.snapshot().audio())
        {
            None
        } else {
            self.program_status.as_ref()
        }
    }
    fn accept_program_stats(&mut self, stats: ProgramTransportStats) {
        if self.program_status.as_ref().is_none_or(|current| {
            stats.output_blocks >= current.output_blocks
                && stats.input_packets >= current.input_packets
                && stats.skipped_blocks >= current.skipped_blocks
                && stats.wall_ticks >= current.wall_ticks
        }) {
            self.program_status = Some(stats);
        }
    }
    /// Same supplied media/monotonic anchor as the processing owner. CLI
    /// controls can advance their controller without resetting live time.
    pub fn program_now(&self) -> Result<Option<MediaTime>, AudioCaptureError> {
        self.program_anchor
            .map(|(instant, media)| {
                let elapsed = u64::try_from(instant.elapsed().as_nanos())
                    .map_err(|_| AudioCaptureError::TimeOverflow)?;
                media
                    .0
                    .checked_add(elapsed)
                    .map(MediaTime)
                    .ok_or(AudioCaptureError::TimeOverflow)
            })
            .transpose()
    }
    /// Ordered CPU output; the processing owner retains a pool reference so a
    /// presentation reader never performs final PCM destruction.
    pub fn poll_program(
        &mut self,
        session: &Session,
    ) -> Result<Option<ProgramCaptureOutput>, AudioCaptureError> {
        self.ensure_open(session)?;
        for _ in 0..8 {
            match self.program_receiver.try_recv() {
                Ok(output) => {
                    if output.key.session == session.identity()
                        && output.key.clock_epoch == self.clock_epoch
                        && output.key.generation == self.program_generation
                        && output.key.settings == session.snapshot().audio()
                        && Some(output.key.settings) == self.program_settings
                    {
                        self.accept_program_stats(output.stats.clone());
                        return Ok(Some(output));
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(AudioCaptureError::Disconnected);
                }
            }
        }
        Ok(None)
    }
    pub fn synchronize(
        &mut self,
        session: &Session,
        views: &[FrameScene],
    ) -> Result<(), AudioCaptureError> {
        // Legacy callers have no explicit Program identity. Never acquire camera
        // audio from their potentially Preview-only view union.
        self.synchronize_with_program(session, views, &[])
    }
    pub fn synchronize_with_program(
        &mut self,
        session: &Session,
        views: &[FrameScene],
        program: &[FrameScene],
    ) -> Result<(), AudioCaptureError> {
        self.ensure_open(session)?;
        if views.len() > 3 || program.len() > 3 {
            return Err(AudioCaptureError::Limit("audio acquisition view count"));
        }
        let current = session.snapshot();
        let mut sources = HashMap::new();
        let mut window_ids=HashSet::new();
        let mut native_ids=HashSet::new();
        for view in views {
            view.validate_session(session.identity())
                .map_err(|_| AudioCaptureError::Invalid("foreign audio scene view"))?;
            for source in view
                .visible_sources(&current)
                .map_err(|_| AudioCaptureError::Invalid("audio scene view unavailable"))?
            {
                if let SourceKind::AudioCapture { binding } = source.kind() {
                    native_ids.insert(source.id());
                    insert_active(&mut sources, source.id(), binding, self.limits.max_sources)?;
                }
                if let SourceKind::WindowCapture { binding } = source.kind() {
                    if let Some(audio) = binding.audio() {
                        window_ids.insert(source.id());
                        insert_active(&mut sources, source.id(), audio, self.limits.max_sources)?;
                    }
                }
            }
        }
        // Camera custom audio follows actual Program, including retained
        // transition roots. Video-only scopes never reach this closure.
        for view in program {
            view.validate_session(session.identity())
                .map_err(|_| AudioCaptureError::Invalid("foreign camera audio Program view"))?;
            for source in view.visible_sources(&current)
                .map_err(|_| AudioCaptureError::Invalid("camera audio Program closure unavailable"))? {
                if let SourceKind::VideoCapture {binding} = source.kind() {
                    if let Some(audio) = binding.audio().filter(|_|self.camera_activation.session()!=session.identity()||self.camera_activation.enabled(source.id())) {
                        native_ids.insert(source.id());
                        insert_active(&mut sources, source.id(), audio, self.limits.max_sources)?;
                    }
                }
            }
        }
        for root in current.global_audio() {
            let source = current
                .source(root.source())
                .ok_or(AudioCaptureError::Invalid(
                    "global audio source unavailable",
                ))?;
            if let SourceKind::AudioCapture { binding } = source.kind() {
                native_ids.insert(source.id());
                insert_active(&mut sources, source.id(), binding, self.limits.max_sources)?;
            }
        }
        if sources.len() > self.limits.max_sources {
            return Err(AudioCaptureError::Limit("audio source references"));
        }
        if window_ids.iter().any(|id|native_ids.contains(id)) {
            return Err(AudioCaptureError::Invalid("conflicting retained window and native audio identities for one source"));
        }
        let mut active: Vec<_> = sources.into_iter().collect();
        active.sort_by_key(|(id, _)| id.get());
        let window_sources:Vec<Id>=active.iter().filter_map(|(id,_)|window_ids.contains(id).then_some(*id)).collect();
        if self.session != session.identity() || self.active != active || self.window_sources != window_sources {
            let epoch = next(self.epoch)?;
            let generation = next(self.snapshot.generation)?;
            let packets = active
                .iter()
                .filter_map(|(id, binding)| {
                    self.snapshot
                        .packets
                        .get(id)
                        .filter(|packet| {
                            self.session == session.identity() && packet.binding == *binding
                        })
                        .map(|packet| (*id, packet.clone()))
                })
                .collect();
            let problems = self
                .snapshot
                .problems
                .iter()
                .filter(|problem| {
                    active.iter().any(|(id, binding)| {
                        id == &problem.source
                            && self
                                .active
                                .iter()
                                .any(|(old, old_binding)| old == id && old_binding == binding)
                    }) && self.session == session.identity()
                })
                .cloned()
                .collect();
            if self.session != session.identity() {
                self.program_settings = None;
                self.program_status = None;
                self.program_anchor = None;
                self.pending_program_start = None;
                self.recording_camera_alignments.clear();
                self.recording_first_cache.clear();
            }
            self.session = session.identity();
            self.epoch = epoch;
            self.active = active;
            self.window_sources = window_sources;
            self.retire_window_observations();
            self.snapshot = AudioCaptureSnapshot {
                session: self.session,
                epoch,
                generation,
                packets: Arc::new(packets),
                problems: Arc::new(problems),
                activation:self.camera_activation.clone(),
            };
            self.last_requested_at = None;
        }
        Ok(())
    }
    pub fn reset_timing(&mut self) -> Result<(), AudioCaptureError> {
        if self.closed {
            return Err(AudioCaptureError::Closed);
        }
        let epoch = next(self.epoch)?;
        let clock_epoch = next(self.clock_epoch)?;
        let generation = next(self.snapshot.generation)?;
        self.epoch = epoch;
        self.clock_epoch = clock_epoch;
        self.snapshot = AudioCaptureSnapshot::empty(self.session, epoch, generation);
        self.last_requested_at = None;
        self.program_settings = None;
        self.program_status = None;
        self.program_anchor = None;
        self.pending_program_start = None;
        self.retire_window_observations();
        Ok(())
    }
    /// Stage source-scoped evidence from the video owner for the next native
    /// audio job. This is deliberately separate from the authored selector:
    /// absence or stale capture metadata never creates an observed window.
    /// Opening and PCM generation fencing are not yet driven by this evidence.
    pub fn observe_window_capture(&mut self, session: &Session, capture: &crate::capture::CaptureSnapshot)
        -> Result<(), AudioCaptureError> {
        self.ensure_open(session)?;
        let model = session.snapshot();
        let mut observations = Vec::new();
        observations.try_reserve(self.active.len()).map_err(|_|AudioCaptureError::Limit("window audio observation allocation"))?;
        for (id,audio) in &self.active {
            let Some(source) = model.source(*id) else {continue;};
            let SourceKind::WindowCapture {binding} = source.kind() else {continue;};
            if binding.audio() != Some(audio) {continue;}
            if let Some(observation) = super::WindowAudioObservation::from_snapshot(session.identity(),*id,binding,capture) {
                let evidence = self.window_observations.iter()
                    .find(|old| old.current() && old.observation() == &observation)
                    .cloned().unwrap_or_else(||super::window_observation::WindowAudioEvidence::new(observation));
                observations.push(evidence);
            }
        }
        let changed=self.window_observations.len()!=observations.len() || self.window_observations.iter().zip(&observations)
            .any(|(old,new)|!old.same_generation(new));
        for old in &self.window_observations {
            if !observations.iter().any(|new|new.current() && new.observation()==old.observation()) {
                old.retire();
            }
        }
        self.window_observations = observations;
        if changed {
            // Old queued replies and retained meter packets must not survive a
            // HWND/PID change even if the authored selector did not change.
            self.epoch=next(self.epoch)?;
            let generation=next(self.snapshot.generation)?;
            let mut packets=(*self.snapshot.packets).clone();
            for source in &self.window_sources {packets.remove(source);}
            self.snapshot=AudioCaptureSnapshot {session:self.session,epoch:self.epoch,generation,
                packets:Arc::new(packets),problems:Arc::new(Vec::new()),activation:self.camera_activation.clone()};
            self.last_requested_at=None;
        }
        Ok(())
    }
    fn retire_window_observations(&mut self) {
        for evidence in self.window_observations.drain(..) { evidence.retire(); }
    }
    pub fn request_processes(&mut self, session: &Session) -> Result<AudioRequestKey, AudioCaptureError> {
        let key=self.key(session)?; self.admit(Job::Processes {key})?; Ok(key)
    }
    pub fn request_devices(
        &mut self,
        session: &Session,
        flow: AudioFlow,
    ) -> Result<AudioRequestKey, AudioCaptureError> {
        let key = self.key(session)?;
        self.admit(Job::Devices { key, flow })?;
        Ok(key)
    }
    /// Empty active input is still submitted, so previously open streams stop.
    pub fn request_packets(
        &mut self,
        session: &Session,
        now: MediaTime,
    ) -> Result<bool, AudioCaptureError> {
        if unique_bindings(&self.active, &self.window_sources).len() > self.limits.max_streams {
            return Err(AudioCaptureError::Limit("native audio stream count"));
        }
        if self
            .last_requested_at
            .is_some_and(|previous| now < previous)
        {
            return Err(AudioCaptureError::Invalid(
                "audio request time moved backwards; reset timing",
            ));
        }
        let key = self.key(session)?;
        self.admit(Job::Packets {
            key,
            clock_epoch: self.clock_epoch,
            sources: self.active.clone(),
            window_sources: self.window_sources.clone(),
            window_observations: self.window_observations.clone(),
            now,
            sent: Instant::now(),
        })?;
        self.last_requested_at = Some(now);
        Ok(true)
    }
    /// Records owned routing metadata; DSP and periodic draining stay on the
    /// same native owner even while the frontend stops requesting snapshots.
    pub fn request_program(
        &mut self,
        session: &Session,
        output: &FrameComposition,
        now: MediaTime,
    ) -> Result<bool, AudioCaptureError> {
        self.request_program_inner(session, output, now, None, None)
    }
    /// Route a temporary fader gesture without publishing a model revision.
    pub fn request_program_with_gain_preview(
        &mut self, session:&Session, output:&FrameComposition, now:MediaTime,
        preview:crate::audio::MixerGainPreview,
    )->Result<bool,AudioCaptureError>{self.request_program_inner(session,output,now,None,Some(preview))}
    pub fn request_program_with_recording(
        &mut self,
        session: &Session,
        output: &FrameComposition,
        now: MediaTime,
        sink: crate::recording::RecordingAudioSink,
    ) -> Result<bool, AudioCaptureError> {
        self.request_program_inner(
            session,
            output,
            now,
            Some(crate::audio_output::AudioOutput::Recording(sink)),
            None,
        )
    }
    pub fn request_program_with_output(
        &mut self,
        session: &Session,
        output: &FrameComposition,
        now: MediaTime,
        sink: crate::audio_output::AudioOutput,
    ) -> Result<bool, AudioCaptureError> {
        self.request_program_inner(session, output, now, Some(sink), None)
    }
    fn request_program_inner(
        &mut self,
        session: &Session,
        output: &FrameComposition,
        now: MediaTime,
        attachment: Option<crate::audio_output::AudioOutput>,
        gain_preview: Option<crate::audio::MixerGainPreview>,
    ) -> Result<bool, AudioCaptureError> {
        let key = self.key(session)?;
        if self
            .last_requested_at
            .is_some_and(|previous| now < previous)
        {
            return Err(AudioCaptureError::Invalid(
                "audio request time moved backwards; reset timing",
            ));
        }
        let route = AudioRouteSnapshot::capture(session, output, now).map_err(|error| {
            AudioCaptureError::Native {
                operation: "prepare Program route",
                detail: error.to_string(),
            }
        })?;
        let route=if let Some(preview)=gain_preview {route.with_mixer_gain_preview(session,preview).map_err(|e|AudioCaptureError::Native {operation:"prepare mixer gain preview",detail:e.to_string()})?}else{route};
        let route=route.with_camera_activation(self.camera_activation.clone()).map_err(|e|AudioCaptureError::Native {operation:"camera runtime audio eligibility",detail:e.to_string()})?;
        let route=route.with_monitor_video(if self.monitor_video_session==session.identity(){&self.monitor_video}else{&[]});
        let mut current_alignments=Vec::new();
        current_alignments.try_reserve_exact(self.recording_camera_alignments.len())
            .map_err(|_|AudioCaptureError::Limit("recording camera alignment allocation"))?;
        current_alignments.extend(self.recording_camera_alignments.iter().filter(|value|
            value.session==session.identity()&&value.revision==route.revision()
                &&value.clock_epoch==self.clock_epoch&&value.current()).cloned());
        let route=route.with_recording_camera_alignments(&current_alignments).map_err(|e|AudioCaptureError::Native {operation:"prepare recording camera alignment",detail:e.to_string()})?;
        let mut current_streaming=Vec::new();
        current_streaming.try_reserve_exact(self.streaming_alignments.len()).map_err(|_|AudioCaptureError::Limit("streaming proof metadata"))?;
        current_streaming.extend(self.streaming_alignments.iter().filter(|value|
            value.session==session.identity()&&value.revision==route.revision()&&value.clock_epoch==self.clock_epoch&&value.current()).cloned());
        let route=route.with_streaming_alignments(&current_streaming).map_err(|e|AudioCaptureError::Native {operation:"prepare streaming alignment",detail:e.to_string()})?;
        let mut bindings = unique_bindings(&self.active, &self.window_sources);
        for (_, binding) in route.required_bindings() {
            if !bindings.contains(binding) {
                bindings.push(binding.clone());
            }
        }
        if bindings.len() > self.limits.max_streams {
            return Err(AudioCaptureError::Limit(
                "native audio Program stream count",
            ));
        }
        let settings = route.settings();
        let generation = if self.program_settings == Some(settings) {
            self.program_generation
        } else {
            next(self.program_generation)?
        };
        let sent = Instant::now();
        self.admit(Job::Program {
            key,
            clock_epoch: self.clock_epoch,
            generation,
            sources: self.active.clone(),
            window_sources: self.window_sources.clone(),
            window_observations: self.window_observations.clone(),
            route: Box::new(route),
            sent,
            attachment,
        })?;
        self.pending_program_start = if generation != self.program_generation {
            Some(key)
        } else {
            None
        };
        if generation != self.program_generation {
            self.program_anchor = Some((sent, now));
            self.program_status = None;
        }
        self.program_generation = generation;
        self.program_settings = Some(settings);
        self.last_requested_at = Some(now);
        Ok(true)
    }
    pub fn request_stop_program(
        &mut self,
        session: &Session,
    ) -> Result<AudioRequestKey, AudioCaptureError> {
        let key = self.key(session)?;
        let generation = next(self.program_generation)?;
        self.admit(Job::StopProgram { key })?;
        self.program_generation = generation;
        self.program_settings = None;
        self.program_anchor = None;
        self.pending_program_start = None;
        Ok(key)
    }
    pub fn poll(
        &mut self,
        session: &Session,
    ) -> Result<Option<AudioCaptureEvent>, AudioCaptureError> {
        self.ensure_open(session)?;
        let reply = match self.receiver.try_recv() {
            Ok(reply) => reply,
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => return Err(AudioCaptureError::Disconnected),
        };
        self.busy = false;
        match reply {
            Reply::CameraActivation {key}=>Ok(Some(AudioCaptureEvent::CameraActivation {key})),
            Reply::Processes {key,result} => {
                if key.session!=session.identity() || key.revision!=session.snapshot().revision() || key.epoch!=self.epoch { return Ok(None); }
                Ok(Some(AudioCaptureEvent::Processes {key,result}))
            }
            Reply::Devices { key, flow, result } => {
                if key.session != session.identity()
                    || key.revision != session.snapshot().revision()
                    || key.epoch != self.epoch
                {
                    return Ok(None);
                }
                Ok(Some(AudioCaptureEvent::Devices { key, flow, result }))
            }
            Reply::ProgramStopped { key } => {
                if key.session != session.identity() || key.epoch != self.epoch {
                    return Ok(None);
                }
                self.program_settings = None;
                self.program_status = None;
                self.program_anchor = None;
                self.pending_program_start = None;
                Ok(Some(AudioCaptureEvent::Program {
                    key,
                    result: Ok(ProgramTransportStats::default()),
                }))
            }
            Reply::Packets {
                key,
                results,
                window_results,
                program,
            } => {
                if key.session != session.identity() || key.epoch != self.epoch {
                    return Ok(None);
                }
                let mut packets = (*self.snapshot.packets).clone();
                let mut problems = Vec::new();
                let mut received = 0;
                let mut removed = false;
                for (source, binding) in &self.active {
                    let selected=if self.window_sources.contains(source) {
                        window_results.iter().find(|(candidate,_)|candidate==source).map(|(_,result)|result)
                    } else {
                        results.iter().find(|(candidate,_)|candidate==binding).map(|(_,result)|result)
                    };
                    match selected
                    {
                        Some(Ok(Some(packet))) => {
                            if packets.get(source).is_none_or(|previous| {
                                previous.sequence != packet.sequence
                                    || previous.stream_generation != packet.stream_generation
                                    || previous.binding != packet.binding
                            }) && self.camera_activation.sources().iter().find(|entry|entry.source==*source)
                                .is_none_or(|entry|entry.enabled&&packet.requested_at_ns>=entry.at.0) {
                                packets.insert(*source, packet.clone());
                                received += 1;
                            }
                        }
                        Some(Ok(None)) => {
                            if !packets.contains_key(source) {
                                problems.push(AudioCaptureProblem {
                                    source: *source,
                                    code: "audio_capture_waiting",
                                    detail: "native stream has not delivered a packet".into(),
                                });
                            }
                        }
                        Some(Err(error)) => {
                            if self.window_sources.contains(source) {removed |= packets.remove(source).is_some();}
                            problems.push(AudioCaptureProblem {
                                source: *source,
                                code: error.code(),
                                detail: bounded_detail(error.to_string()),
                            });
                        },
                        None => problems.push(AudioCaptureProblem {
                            source: *source,
                            code: "audio_capture_missing_result",
                            detail: "native capture result missing binding".into(),
                        }),
                    }
                }
                if received != 0 || removed || *self.snapshot.problems != problems {
                    let generation = next(self.snapshot.generation)?;
                    self.snapshot = AudioCaptureSnapshot {
                        session: self.session,
                        epoch: self.epoch,
                        generation,
                        packets: Arc::new(packets),
                        problems: Arc::new(problems),
                activation:self.camera_activation.clone(),
                    };
                }
                if let Some(result) = program {
                    if self.pending_program_start == Some(key) {
                        self.pending_program_start = None;
                        if result.is_err() {
                            self.program_settings = None;
                            self.program_anchor = None;
                            self.program_status = None;
                        }
                    }
                    if let Ok(stats) = &result {
                        self.accept_program_stats(stats.clone());
                        if !stats.running {
                            self.program_settings = None;
                            self.program_anchor = None;
                        }
                    } else if matches!(&result, Err(AudioCaptureError::ProgramRetired(_))) {
                        self.program_settings = None;
                        self.program_anchor = None;
                        self.program_status = None;
                    }
                    Ok(Some(AudioCaptureEvent::Program { key, result }))
                } else {
                    Ok(Some(AudioCaptureEvent::Packets { key, received }))
                }
            }
        }
    }
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.recording_camera_alignments.clear();
        self.recording_first_cache.clear();
        let (_, empty) = mpsc::sync_channel(1);
        self.receiver = empty;
        let (_, empty_program) = mpsc::sync_channel(1);
        self.program_receiver = empty_program;
        self.sender.take();
        self.active.clear();
        self.window_sources.clear();
        self.retire_window_observations();
        self.program_status = None;
        self.program_settings = None;
        self.program_anchor = None;
        self.pending_program_start = None;
        self.snapshot =
            AudioCaptureSnapshot::empty(self.session, self.epoch, self.snapshot.generation);
    }
    /// This is intentionally not a bounded GUI close operation. Release all
    /// snapshot readers first; native calls and callback stops may remain live.
    pub fn join(&mut self) -> thread::Result<()> {
        self.close();
        if let Some(handle) = self.handle.take() {
            let result = handle.join();
            self.busy = false;
            result
        } else {
            Ok(())
        }
    }
    fn ensure_open(&self, session: &Session) -> Result<(), AudioCaptureError> {
        if self.closed || session.is_closed() {
            Err(AudioCaptureError::Closed)
        } else {
            Ok(())
        }
    }
    fn key(&self, session: &Session) -> Result<AudioRequestKey, AudioCaptureError> {
        self.ensure_open(session)?;
        if self.busy {
            return Err(AudioCaptureError::Busy);
        }
        if self.session != session.identity() {
            return Err(AudioCaptureError::Invalid(
                "synchronize audio owner before request",
            ));
        }
        Ok(AudioRequestKey {
            session: self.session,
            revision: session.snapshot().revision(),
            epoch: self.epoch,
            request: next(self.serial)?,
        })
    }
    fn admit(&mut self, job: Job) -> Result<(), AudioCaptureError> {
        let serial = match &job {
            Job::CameraActivation {key,..} | Job::Processes { key } | Job::Devices { key, .. }
            | Job::Packets { key, .. }
            | Job::Program { key, .. }
            | Job::StopProgram { key } => key.request,
        };
        self.sender
            .as_ref()
            .ok_or(AudioCaptureError::Closed)?
            .try_send(job)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => AudioCaptureError::Busy,
                mpsc::TrySendError::Disconnected(_) => AudioCaptureError::Disconnected,
            })?;
        self.serial = serial;
        self.busy = true;
        Ok(())
    }
}
impl Drop for AudioCaptureWorker {
    fn drop(&mut self) {
        self.close();
    }
}
fn next(value: u64) -> Result<u64, AudioCaptureError> {
    value
        .checked_add(1)
        .ok_or(AudioCaptureError::Limit("audio identity exhausted"))
}
fn insert_active(
    sources: &mut HashMap<Id, AudioCaptureBinding>,
    id: Id,
    binding: &AudioCaptureBinding,
    maximum: usize,
) -> Result<(), AudioCaptureError> {
    if let Some(existing) = sources.get(&id) {
        if existing != binding {
            return Err(AudioCaptureError::Invalid(
                "conflicting retained audio bindings for one SourceID",
            ));
        }
        return Ok(());
    }
    if sources.len() >= maximum {
        return Err(AudioCaptureError::Limit("audio source references"));
    }
    sources.insert(id, binding.clone());
    Ok(())
}
fn bounded_detail(mut detail: String) -> String {
    if detail.len() > 8192 {
        let mut end = 8192;
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        detail.truncate(end);
    }
    detail
}
fn unique_bindings(sources: &[(Id, AudioCaptureBinding)], window_sources:&[Id]) -> Vec<AudioCaptureBinding> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for (source, binding) in sources {
        if window_sources.contains(source) {continue;}
        if seen.insert(binding) {
            result.push(binding.clone());
        }
    }
    result
}

fn serialize_signed_time<S: serde::Serializer>(
    value: &i128,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}
struct OpenStream {
    description: Option<Arc<str>>,
    binding: AudioCaptureBinding,
    stream: Box<dyn NativeAudioStream>,
    native_anchor: u64,
    media_anchor: i128,
    generation: u64,
    delivered: bool,
    closing: bool,
    retry_close_after: Option<Instant>,
    close_problem:Option<AudioCaptureError>,
    owner_session:u64,
    owner_sources:Vec<Id>,
}
/// Dedicated source/generation-owned native stream; never shares PCM with an
/// independently selected application that happens to have the same selector.
struct WindowSlot {
    source: Id,
    evidence: super::window_observation::WindowAudioEvidence,
    binding: AudioCaptureBinding,
    streams: Vec<OpenStream>,
    latest: PacketResult,
    invalidated: bool,
}
struct Resident {
    packet: Arc<CapturedAudioPacket>,
    bytes: usize,
}
fn reap(pool: &mut Vec<Resident>) {
    pool.retain(|entry| Arc::strong_count(&entry.packet) > 1);
}
fn charge(packet: &CapturedAudioPacket) -> usize {
    packet.block.samples.capacity() * 4
        + packet.meters.capacity() * std::mem::size_of::<crate::audio::ChannelMeter>()
        + std::mem::size_of::<CapturedAudioPacket>()
        + packet
            .resolved_device
            .as_ref()
            .map_or(0, |name| name.len() + 2 * std::mem::size_of::<usize>())
        + 2048
}
fn close_obsolete(streams: &mut Vec<OpenStream>, keep: &[AudioCaptureBinding]) {
    let mut index = 0;
    while index < streams.len() {
        if !streams[index].closing && keep.contains(&streams[index].binding) {
            index += 1;
            continue;
        }
        streams[index].closing = true;
        if streams[index]
            .retry_close_after
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            index += 1;
            continue;
        }
        match streams[index].stream.close(){
            Ok(())=>{streams.remove(index);},
            Err(error)=>{streams[index].close_problem=Some(error);streams[index].retry_close_after=Some(Instant::now()+Duration::from_millis(100));index+=1;},
        }
    }
}
struct PendingCameraAudio {changes:Arc<[crate::camera_activation::SourceEpoch]>,watch:crate::camera_activation::LegWatch,cut:crate::camera_activation::CameraAudioCut,cancelled:bool,ended:bool}
struct NativeOwner {
    camera_activation:Arc<crate::camera_activation::Snapshot>,
    camera_pending:Option<PendingCameraAudio>,
    camera_last:Option<PendingCameraAudio>,
    monitor:crate::audio_monitor::MonitorOwner,
    factory: Option<AudioBackendFactory>,
    backend: Option<Result<Box<dyn NativeAudioBackend>, AudioCaptureError>>,
    streams: Vec<OpenStream>,
    pool: Vec<Resident>,
    scope: Option<(u64, u64)>,
    sequence: u64,
    retries: Vec<(AudioCaptureBinding, Instant, AudioCaptureError)>,
    live: Option<LiveProgram>,
    retired: Vec<LiveProgram>,
    sources: Vec<(Id, AudioCaptureBinding)>,
    window_sources: Vec<Id>,
    window_observations: Vec<super::window_observation::WindowAudioEvidence>,
    window_slots: Vec<WindowSlot>,
    window_retries: HashMap<Id,Instant>,
    window_errors: HashMap<Id,AudioCaptureError>,
    latest: HashMap<AudioCaptureBinding, PacketResult>,
    rotate: usize,
    immediate: bool,
    worked: bool,
    outputs: mpsc::SyncSender<ProgramCaptureOutput>,
    limits: AudioCaptureLimits,
}
impl NativeOwner {
    fn poll_camera_activation(&mut self){
        use crate::camera_activation::{Phase,Problem};
        let pending=self.camera_pending.take().or_else(||{
            if self.camera_last.as_ref().is_some_and(|pending|pending.watch.cancellation().is_some_and(|cancel|!pending.cancelled||cancel.end_session&&!pending.ended)){self.camera_last.take()}else{None}
        });
        let Some(mut pending)=pending else{return;};
        if let Some(cancel)=pending.watch.cancellation(){if !pending.cancelled||cancel.end_session&&!pending.ended{
            if self.scope.is_some_and(|scope|scope.0!=pending.watch.key.session){
                pending.watch.publish(Phase::Failed,Some(Problem {code:"camera_av_obsolete",detail:"Old-session audio activation cannot affect the replacement session".into()}),Some(pending.cut));return;
            }
            if cancel.end_session {self.retire_live();self.sources.clear();}else{
                self.camera_activation=cancel.policy;self.sources.retain(|(id,_)|self.camera_activation.enabled(*id));
                if let Some(live)=&mut self.live {
                    match live.set_capture_bindings(unique_bindings(&self.sources, &self.window_sources))
                        .and_then(|()|live.set_camera_activation(self.camera_activation.sources().to_vec().into(),None)){
                        Ok(cut)=>pending.cut=cut,
                        Err(error)=>{pending.watch.publish(Phase::Failed,Some(Problem {code:error.code(),detail:error.to_string()}),Some(pending.cut));return;},
                    }
                }
            }
            pending.changes=cancel.changes;pending.cancelled=true;pending.ended=cancel.end_session;
        }}
        let keep=self.bindings();close_obsolete(&mut self.streams,&keep);
        let end_session=pending.watch.cancellation().is_some_and(|cancel|cancel.end_session);
        let mut retiring=end_session&&self.streams.iter().any(|stream|stream.owner_session==pending.watch.key.session);let mut problem=None;let mut failed=None;
        for entry in pending.changes.iter(){if let Some(binding)=entry.binding.audio(){
            if entry.enabled {
                if !self.sources.iter().any(|(id,_)|*id==entry.source){continue;}
                if let Some((_,_,error))=self.retries.iter().find(|(b,_,_)|b==binding){failed=Some(error.clone());break;}
                if self.streams.iter().find(|stream|&stream.binding==binding&&!stream.closing).is_none_or(|stream|!stream.delivered){retiring=true;}
            }else if let Some(stream)=self.streams.iter().find(|stream|stream.owner_session==pending.watch.key.session&&stream.closing&&(&stream.binding==binding||stream.owner_sources.contains(&entry.source))){retiring=true;problem=stream.close_problem.as_ref();}
            else if !keep.contains(binding){if let Some(stream)=self.streams.iter().find(|stream|&stream.binding==binding){retiring=true;problem=stream.close_problem.as_ref();}}
        }}
        if let Some(error)=failed {pending.watch.publish(Phase::Failed,Some(Problem {code:error.code(),detail:error.to_string()}),Some(pending.cut));self.camera_last=Some(pending);return;}
        let history=self.scope.is_some_and(|scope|scope.0!=pending.watch.key.session)||self.live.as_ref().is_none_or(|live|live.camera_activation_retired(&pending.changes,pending.cut));
        if !retiring&&history {pending.watch.publish(if pending.cancelled{Phase::Failed}else{Phase::Complete},pending.cancelled.then(||Problem {code:"camera_av_cancelled",detail:"Camera activation was superseded by Stop; its audio leg retired".into()}),Some(pending.cut));self.camera_last=Some(pending);}
        else {pending.watch.publish(Phase::Retiring,problem.map(|error|Problem {code:error.code(),detail:error.to_string()}),Some(pending.cut));self.camera_pending=Some(pending);}
    }
    fn output_retirement_full(&mut self) -> bool {
        let bytes: usize = self
            .retired
            .iter()
            .map(LiveProgram::resident_bytes)
            .sum::<usize>()
            + self.live.as_ref().map_or(0, LiveProgram::resident_bytes);
        let count: usize = self
            .retired
            .iter()
            .map(LiveProgram::resident_count)
            .sum::<usize>()
            + self.live.as_ref().map_or(0, LiveProgram::resident_count);
        self.retired.len() >= 5
            || self.retired.len() >= 4 && self.live.as_mut().is_some_and(|live| !live.can_retire())
            || bytes + 8 * 1024 * 1024 > 40 * 1024 * 1024
            || count + 256 > 1280
    }
    fn reap(&mut self) {
        reap(&mut self.pool);
        for slot in &mut self.window_slots {if slot.invalidated {close_obsolete(&mut slot.streams,&[]);}}
        self.window_slots.retain(|slot|!slot.invalidated || !slot.streams.is_empty());
        self.retired.retain_mut(LiveProgram::reap);
        if let Some(live) = &mut self.live {
            live.reap();
        }
    }
    fn retire_live(&mut self) {
        let _=self.monitor.configure(None);
        if let Some(mut live) = self.live.take() {
            live.retire();
            if live.reap() {
                self.retired.push(live);
            }
        }
    }
    fn initialize(&mut self) {
        if self.backend.is_none() {
            self.backend = Some(self.factory.take().expect("audio factory invoked once")());
        }
    }
    fn synchronize_scope(&mut self, session: u64, clock_epoch: u64) {
        if self.scope != Some((session, clock_epoch)) {
            self.retire_live();
            close_obsolete(&mut self.streams, &[]);
            self.retries.clear();
            self.latest.clear();
            for slot in &mut self.window_slots {
                if !slot.invalidated {if let Some(live)=&mut self.live {live.invalidate_window(slot.source);}slot.invalidated=true;}
                close_obsolete(&mut slot.streams,&[]);
            }
            self.window_observations.clear();
            self.window_retries.clear();
            self.window_errors.clear();
            self.scope = Some((session, clock_epoch));
        }
    }
    fn bindings(&self) -> Vec<AudioCaptureBinding> {
        let mut bindings = unique_bindings(&self.sources, &self.window_sources);
        if let Some(live) = &self.live {
            for binding in live.required_bindings() {
                if !bindings.contains(&binding) {
                    bindings.push(binding);
                }
            }
        }
        bindings
    }
    fn results(&self) -> Vec<(AudioCaptureBinding, PacketResult)> {
        unique_bindings(&self.sources, &self.window_sources)
            .into_iter()
            .map(|binding| {
                let result = self.latest.get(&binding).cloned().unwrap_or(Ok(None));
                (binding, result)
            })
            .collect()
    }
    fn window_results(&self) -> Vec<(Id,PacketResult)> {
        self.window_sources.iter().map(|source| {
            let result=self.window_slots.iter().find(|slot|slot.source==*source && slot.evidence.current() && !slot.invalidated)
                .map(|slot|slot.latest.clone()).unwrap_or_else(||Err(self.window_errors.get(source).cloned().unwrap_or_else(||AudioCaptureError::DeviceUnavailable("Waiting for a current captured window before opening its audio".into()))));
            (*source,result)
        }).collect()
    }
    fn drain_windows(&mut self,now:MediaTime,sent:Instant,continuous:bool,batch_bytes:&mut usize) {
        // Invalidate raw and processed PCM before the next mixer tick whenever
        // a video token is retired, including while the GUI job is still queued.
        for slot in &mut self.window_slots {
            let keep=slot.evidence.current() && self.window_sources.contains(&slot.source)
                && self.window_observations.iter().any(|e|e.current() && e.same_generation(&slot.evidence));
            if !keep {
                if !slot.invalidated {
                    if let Some(live)=&mut self.live {live.invalidate_window(slot.source);}
                    slot.latest=Ok(None);slot.invalidated=true;
                }
                close_obsolete(&mut slot.streams,&[]);
            }
        }
        self.window_slots.retain(|slot|!slot.invalidated || !slot.streams.is_empty());
        self.window_retries.retain(|source,_|self.window_sources.contains(source));
        self.window_errors.retain(|source,_|self.window_sources.contains(source));
        for evidence in &self.window_observations {
            let source=evidence.observation().source();
            if !evidence.current() || !self.window_sources.contains(&source)
                || self.window_slots.iter().any(|slot|slot.source==source)
                || self.window_retries.get(&source).is_some_and(|until|Instant::now()<*until) {continue;}
            let Some((_,binding))=self.sources.iter().find(|(id,_)|*id==source) else {continue;};
            self.window_slots.push(WindowSlot {source,evidence:evidence.clone(),binding:binding.clone(),
                streams:Vec::new(),latest:Ok(None),invalidated:false});
        }
        let mut count=self.streams.len()+self.window_slots.iter().map(|slot|slot.streams.len()).sum::<usize>();
        for slot in &mut self.window_slots {
            if slot.invalidated || !slot.evidence.current() {continue;}
            if slot.streams.is_empty() && count>=self.limits.max_streams {
                slot.latest=Err(AudioCaptureError::Limit("active/closing native streams"));continue;
            }
            let before=slot.streams.len();
            let mut drained=0;
            slot.latest=match self.backend.as_mut().expect("initialized audio owner") {
                Err(error)=>Err(error.clone()),
                Ok(backend)=>capture_latest(backend.as_mut(),&slot.binding,Some(&slot.evidence),&mut slot.streams,
                    &mut self.pool,&mut self.sequence,batch_bytes,now,sent,self.limits,self.live.as_mut(),
                    if continuous {8}else{16},&mut drained),
            };
            count=count.saturating_add(slot.streams.len().saturating_sub(before));
            if slot.latest.is_err() {
                if let Err(error)=&slot.latest {self.window_errors.insert(slot.source,error.clone());}
                if let Some(live)=&mut self.live {live.invalidate_window(slot.source);}
                self.window_retries.insert(slot.source,Instant::now()+Duration::from_secs(1));
                slot.invalidated=true;
                close_obsolete(&mut slot.streams,&[]);
                continue;
            }
            self.window_errors.remove(&slot.source);
            if !slot.evidence.current() {
                if let Some(live)=&mut self.live {live.invalidate_window(slot.source);}
                slot.latest=Ok(None);slot.invalidated=true;
                close_obsolete(&mut slot.streams,&[]);
            }
        }
    }
    fn drain(&mut self, now: MediaTime, sent: Instant, continuous: bool) {
        let bindings = self.bindings();
        self.retries
            .retain(|(binding, _, _)| bindings.contains(binding));
        self.latest.retain(|binding, _| bindings.contains(binding));
        close_obsolete(&mut self.streams, &bindings);
        let mut batch_bytes = 0;
        let mut total = 0;
        self.drain_windows(now,sent,continuous,&mut batch_bytes);
        let window_streams=self.window_slots.iter().map(|slot|slot.streams.len()).sum::<usize>();
        let mut native_limits=self.limits;
        native_limits.max_streams=self.limits.max_streams.saturating_sub(window_streams);
        self.immediate = false;
        let required = self
            .live
            .as_ref()
            .map(LiveProgram::required_bindings)
            .unwrap_or_default();
        for offset in 0..bindings.len() {
            if continuous && total >= 64 {
                self.immediate = true;
                break;
            }
            let binding = &bindings[(self.rotate + offset) % bindings.len()];
            let cooling = self
                .retries
                .iter()
                .find(|(candidate, until, _)| candidate == binding && *until > Instant::now())
                .map(|(_, _, error)| error.clone());
            let mut drained = 0;
            let quantum = if continuous { 16.min(64 - total) } else { 16 };
            let packet = if native_limits.max_streams==0 {
                Err(AudioCaptureError::Limit("active/closing native streams"))
            } else if let Some(error) = &cooling {
                Err(error.clone())
            } else {
                match self.backend.as_mut().expect("initialized audio owner") {
                    Err(error) => Err(error.clone()),
                    Ok(backend) => capture_latest(
                        backend.as_mut(),
                        binding,
                        None,
                        &mut self.streams,
                        &mut self.pool,
                        &mut self.sequence,
                        &mut batch_bytes,
                        now,
                        sent,
                        native_limits,
                        if required.contains(binding) {
                            self.live.as_mut()
                        } else {
                            None
                        },
                        quantum,
                        &mut drained,
                    ),
                }
            };
            if let Some(stream)=self.streams.iter_mut().find(|stream|stream.binding==*binding&&!stream.closing){
                stream.owner_session=self.scope.map_or(0,|scope|scope.0);
                let owners=self.sources.iter().filter(|(_,b)|b==binding).map(|(id,_)|*id);
                if !owners.clone().eq(stream.owner_sources.iter().copied()){stream.owner_sources.clear();stream.owner_sources.extend(owners);}
            }
            total += drained;
            if continuous && drained == quantum {
                self.immediate = true;
            }
            if let Err(error) = &packet {
                if cooling.is_none()
                    && !matches!(
                        error,
                        AudioCaptureError::Busy | AudioCaptureError::Backpressure(_)
                    )
                {
                    if let Some(stream) = self
                        .streams
                        .iter_mut()
                        .find(|stream| stream.binding == *binding)
                    {
                        stream.closing = true;
                    }
                    close_obsolete(&mut self.streams, &bindings);
                    self.retries
                        .retain(|(candidate, _, _)| candidate != binding);
                    self.retries.push((
                        binding.clone(),
                        Instant::now() + Duration::from_secs(1),
                        error.clone(),
                    ));
                }
            }
            match &packet {
                Ok(None) if continuous => {
                    self.latest.entry(binding.clone()).or_insert(Ok(None));
                }
                _ => {
                    self.latest.insert(binding.clone(), packet);
                }
            }
        }
        if continuous && !bindings.is_empty() {
            self.rotate = (self.rotate + 1) % bindings.len();
        } else {
            self.rotate = 0;
        }
    }
    fn periodic(&mut self) {
        self.periodic_at(Instant::now());
    }
    fn periodic_at(&mut self, instant: Instant) {
        if self.live.as_ref().is_none_or(|live| !live.is_active()) {
            return;
        }
        self.worked = true;
        let now = match self
            .live
            .as_ref()
            .expect("active Program")
            .media_now(instant)
        {
            Ok(now) => now,
            Err(error) => {
                if let Some(live) = &mut self.live {
                    live.fail(error);
                }
                close_obsolete(&mut self.streams, &[]);
                return;
            }
        };
        self.drain(now, instant, true);
        if let Some(live) = &mut self.live {
            // Render only deadlines due at the start of this input poll pass.
            // If a deadline crosses during native reads/metering, the actor's
            // zero timeout performs another poll before rendering that block.
            live.tick(instant);
            self.immediate |= live.needs_immediate();
            if !live.is_active() {
                close_obsolete(&mut self.streams, &[]);
            }
        }
    }
    fn handle(&mut self, job: Job) -> Reply {
        let needs_backend = match &job {
            Job::Processes { .. } | Job::Devices { .. } => true,
            Job::Packets { sources, .. } => !sources.is_empty(),
            Job::Program { sources, route, .. } => {
                !sources.is_empty() || route.required_bindings().next().is_some()
            }
            Job::CameraActivation {changes,sources,..}=>changes.iter().any(|entry|entry.enabled&&entry.binding.audio().is_some()&&sources.iter().any(|(id,_)|*id==entry.source)),
            Job::StopProgram { .. } => false,
        };
        if needs_backend {
            self.initialize();
        }
        match job {
            Job::CameraActivation {key,clock_epoch,changes,policy,sources,route,rate,now,watch}=>{
                use crate::camera_activation::{Phase,Problem,CameraAudioCut};
                if self.camera_pending.is_some(){watch.publish(Phase::Failed,Some(Problem {code:"camera_av_busy",detail:"Previous camera audio retirement is still pending".into()}),None);return Reply::CameraActivation {key};}
                self.synchronize_scope(key.session,clock_epoch);self.camera_activation=policy;
                self.sources=sources;
                self.sources.retain(|(id,_)|self.camera_activation.enabled(*id));
                let result=if let Some(live)=&mut self.live {
                    live.set_capture_bindings(unique_bindings(&self.sources, &self.window_sources))
                        .and_then(|()|live.set_camera_activation(self.camera_activation.sources().to_vec().into(),route.map(|route|*route)))
                }else{
                    let rate=route.as_ref().map_or(rate,|route|route.settings().sample_rate());
                    let at=changes.first().map_or(now,|entry|entry.at);
                    u64::try_from((u128::from(at.0)*u128::from(rate)).div_ceil(1_000_000_000)).map(|frame|CameraAudioCut {frame,sample_rate:rate}).map_err(|_|AudioCaptureError::TimeOverflow)
                };
                match result {Ok(cut)=>{watch.publish(Phase::Retiring,None,Some(cut));self.camera_pending=Some(PendingCameraAudio {changes,watch,cut,cancelled:false,ended:false});},Err(error)=>watch.publish(Phase::Failed,Some(Problem {code:error.code(),detail:error.to_string()}),None)}
                let keep=self.bindings();close_obsolete(&mut self.streams,&keep);
                Reply::CameraActivation {key}
            },
            Job::Processes {key} => Reply::Processes {key,result:match self.backend.as_mut().expect("initialized audio owner") {
                Ok(backend)=>backend.enumerate_processes(self.limits).and_then(|devices|validate_processes(devices,self.limits)), Err(error)=>Err(error.clone()),
            }},
            Job::Devices { key, flow } => Reply::Devices {
                key,
                flow,
                result: match self.backend.as_mut().expect("initialized audio owner") {
                    Ok(backend) => backend
                        .enumerate(flow, self.limits)
                        .and_then(|devices| {
                            if devices.iter().any(|device|matches!(device.target,AudioTarget::WindowsProcess {..}|AudioTarget::WindowsWindow {..})) {
                                return Err(AudioCaptureError::Invalid("native endpoint enumeration returned an application target"));
                            }
                            validate_devices(devices, flow, self.limits)
                        }),
                    Err(error) => Err(error.clone()),
                },
            },
            Job::Packets {
                key,
                clock_epoch,
                sources,
                window_sources,
                window_observations,
                now,
                sent,
            } => {
                self.synchronize_scope(key.session, clock_epoch);
                if let Some(live) = &mut self.live {
                    let capture = unique_bindings(&sources, &window_sources);
                    let mut combined = capture.clone();
                    for binding in live.retained_bindings() {
                        if !combined.contains(&binding) {
                            combined.push(binding);
                        }
                    }
                    let result = if combined.len() > self.limits.max_streams {
                        Err(AudioCaptureError::Limit("retained native Program bindings"))
                    } else {
                        live.set_capture_bindings(capture)
                    };
                    if let Err(error) = result {
                        return Reply::Packets {
                            key,
                            results: unique_bindings(&sources, &window_sources)
                                .into_iter()
                                .map(|binding| (binding, Err(error.clone())))
                                .collect(),
                            window_results: Vec::new(), program: None,
                        };
                    }
                }
                self.sources = sources;
                self.window_sources = window_sources;
                self.window_observations = window_observations;
                if self.live.is_none() {
                    self.drain(now, sent, false);
                } else {
                    self.periodic();
                }
                let results = self.results();
                if self.live.is_none() {
                    self.latest.clear();
                }
                Reply::Packets {
                    key,
                    results,
                    window_results:self.window_results(),
                    program: None,
                }
            }
            Job::Program {
                key,
                clock_epoch,
                generation,
                sources,
                window_sources,
                window_observations,
                route,
                sent,
                attachment,
            } => {
                let monitor_config=route.monitor_config();
                let mut bindings = unique_bindings(&sources, &window_sources);
                for (_, binding) in route.required_bindings() {
                    if !bindings.contains(binding) {
                        bindings.push(binding.clone());
                    }
                }
                let same = self.live.as_ref().is_some_and(|live| {
                    live.key()
                        == ProgramCaptureKey {
                            session: key.session,
                            clock_epoch,
                            generation,
                            settings: route.settings(),
                        }
                        && live.is_active()
                });
                if same {
                    for binding in self
                        .live
                        .as_ref()
                        .expect("matching Program")
                        .retained_bindings()
                    {
                        if !bindings.contains(&binding) {
                            bindings.push(binding);
                        }
                    }
                }
                let proposed_key = ProgramCaptureKey {
                    session: key.session,
                    clock_epoch,
                    generation,
                    settings: route.settings(),
                };
                let duplicate = attachment.as_ref().is_some_and(|sink| {
                    self.live
                        .as_ref()
                        .is_some_and(|live| live.has_output(sink.key().kind))
                });
                let format_conflict = self.live.as_ref().is_some_and(|live| {
                    live.has_outputs()
                        && (live.key().settings.sample_rate() != route.settings().sample_rate()
                            || live.key().settings.speakers() != route.settings().speakers())
                });
                let mut attachment = attachment;
                let attachment_error = if duplicate {
                    Some(AudioCaptureError::Busy)
                } else if format_conflict {
                    Some(AudioCaptureError::Invalid(
                        "Program audio format is guarded by an attached output",
                    ))
                } else {
                    attachment
                        .as_mut()
                        .and_then(|sink| sink.bind(proposed_key).err())
                        .map(|error| AudioCaptureError::Native {
                            operation: "prepare Program output attachment",
                            detail: error.to_string(),
                        })
                };
                let result = if let Some(error) = attachment_error {
                    Err(error)
                } else if self
                    .live
                    .as_ref()
                    .is_some_and(|live| !live.is_active() && live.key().generation == generation)
                {
                    Err(AudioCaptureError::ProgramRetired(
                        self.live
                            .as_ref()
                            .and_then(|live| live.stats().problem)
                            .map_or_else(
                                || "Program owner retired; restart with a new generation".into(),
                                |problem| format!("{}: {}", problem.code, problem.detail),
                            ),
                    ))
                } else if bindings.len() > self.limits.max_streams {
                    Err(AudioCaptureError::Limit("retained native Program bindings"))
                } else if !same && self.output_retirement_full() {
                    Err(AudioCaptureError::Backpressure(
                        "retired Program output readers",
                    ))
                } else if same {
                    self.live
                        .as_mut()
                        .expect("matching Program")
                        .update_route(*route)
                } else {
                    match LiveProgram::new(
                        *route,
                        clock_epoch,
                        generation,
                        sent,
                        self.outputs.clone(),
                    ) {
                        Ok(mut candidate) => {
                            // Move the unique producer before old retirement.
                            // bind validates same-clock buffering-only migration;
                            // an actual restart ends it with a concrete fault.
                            if let Some(sink) =
                                self.live.as_mut().and_then(LiveProgram::take_recording)
                            {
                                let _ = candidate.attach_recording(sink);
                            }
                            if let Some(sink) =
                                self.live.as_mut().and_then(LiveProgram::take_streaming)
                            {
                                let _ = candidate.attach_output(
                                    crate::audio_output::AudioOutput::Streaming(sink),
                                );
                            }
                            if let Some(sink)=self.live.as_mut().and_then(LiveProgram::take_ndi_output){let _=candidate.attach_output(crate::audio_output::AudioOutput::NdiProgram(sink));}
                            if let Some(sink)=self.live.as_mut().and_then(LiveProgram::take_replay){let _=candidate.attach_output(crate::audio_output::AudioOutput::ReplayBuffer(sink));}
                            self.synchronize_scope(key.session, clock_epoch);
                            self.retire_live();
                            self.live = Some(candidate);
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                };
                let result = result.and_then(|()| {
                    self.live
                        .as_mut()
                        .expect("installed Program")
                        .set_capture_bindings(unique_bindings(&sources, &window_sources))
                });
                let result = result.and_then(|()| {
                    if let Some(sink) = attachment {
                        self.live
                            .as_mut()
                            .expect("installed Program")
                            .attach_output(sink)
                    } else {
                        Ok(())
                    }
                });
                let result=result.and_then(|()| {
                    self.monitor.configure(monitor_config).map(|sink|self.live.as_mut().expect("installed Program").set_monitor_sink(sink))
                        .map_err(|detail|AudioCaptureError::Native {operation:"configure audio monitoring",detail})
                });
                if result.is_ok() {
                    self.sources = sources;
                    self.window_sources = window_sources;
                    self.window_observations = window_observations;
                }
                let result =
                    result.map(|()| self.live.as_ref().expect("installed Program").stats());
                Reply::Packets {
                    key,
                    results: self.results(),
                    window_results:self.window_results(),
                    program: Some(result),
                }
            }
            Job::StopProgram { key } => {
                self.retire_live();
                self.sources.clear();
                self.window_sources.clear();
                self.window_observations.clear();
                for slot in &mut self.window_slots {slot.invalidated=true;close_obsolete(&mut slot.streams,&[]);}
                self.window_retries.clear();
                self.window_errors.clear();
                self.latest.clear();
                self.retries.clear();
                close_obsolete(&mut self.streams, &[]);
                Reply::ProgramStopped { key }
            }
        }
    }
}
fn service_push_audio(holds:&mut push_controls::Holds,receiver:&mpsc::Receiver<push_controls::Message>,owner:&mut NativeOwner,fault:&std::sync::atomic::AtomicBool){
    if fault.load(std::sync::atomic::Ordering::Acquire){if let Some(live)=&mut owner.live{if live.is_active(){live.fail(AudioCaptureError::Invalid("push audio input stopped; reopen shortcut settings"));}}return;}
    let result=(||->Result<(),String>{
        holds.receive(receiver).map_err(str::to_owned)?;
        let now=Instant::now();
        holds.revoke(now,|session,source,epoch,action,at|{
            if let Some(live)=&mut owner.live{live.push_audio_edge(session,source,epoch,action,false,at,true).map_err(|error|error.to_string())?;}Ok(())
        })?;
        holds.process(|session,source,epoch,action,pressed,at|{
            if let Some(live)=&mut owner.live{live.push_audio_edge(session,source,epoch,action,pressed,at,false).map_err(|error|error.to_string())?;}Ok(())
        })?;
        holds.replay(|session,source,epoch,action,deadline,at|{
            if let Some(live)=&mut owner.live{if let Some(deadline)=deadline{live.push_audio_tail(session,source,epoch,action,deadline)}else{live.push_audio_edge(session,source,epoch,action,true,at,false)}.map_err(|error|error.to_string())?;}Ok(())
        })?;
        Ok(())
    })();
    if let Err(detail)=result {fault.store(true,std::sync::atomic::Ordering::Release);if let Some(live)=&mut owner.live{live.fail(AudioCaptureError::Native{operation:"push audio controls",detail});}}
}
fn run(
    factory: AudioBackendFactory,
    jobs: mpsc::Receiver<Job>,
    replies: mpsc::SyncSender<Reply>,
    outputs: mpsc::SyncSender<ProgramCaptureOutput>,
    media_packets: mpsc::Receiver<Arc<crate::media_source::MediaPcmPacket>>,
    ndi_packets:mpsc::Receiver<Arc<crate::ndi::NdiPcmPacket>>,
    browser_packets:mpsc::Receiver<Arc<crate::browser::BrowserPcmPacket>>,
    embedded_packets:mpsc::Receiver<super::EmbeddedPcmPacket>,
    push_events:mpsc::Receiver<push_controls::Message>,
    push_fault:Arc<std::sync::atomic::AtomicBool>,
    limits: AudioCaptureLimits,
    native_monitoring:bool,
) {
    let mut owner = NativeOwner {
        monitor:if native_monitoring {crate::audio_monitor::MonitorOwner::native()}else{Default::default()},
        camera_activation:Arc::new(Default::default()),camera_pending:None,camera_last:None,
        factory: Some(factory),
        backend: None,
        streams: Vec::new(),
        pool: Vec::new(),
        scope: None,
        sequence: 0,
        retries: Vec::new(),
        live: None,
        retired: Vec::new(),
        sources: Vec::new(),
        window_sources: Vec::new(),
        window_observations: Vec::new(),
        window_slots: Vec::new(),
        window_retries: HashMap::new(),
        window_errors: HashMap::new(),
        latest: HashMap::new(),
        rotate: 0,
        immediate: false,
        worked: false,
        outputs,
        limits,
    };
    let mut push_holds=push_controls::Holds::new();
    loop {
        owner.worked = false;
        owner.reap();
        service_push_audio(&mut push_holds,&push_events,&mut owner,&push_fault);
        let timeout = if owner.live.as_ref().is_some_and(LiveProgram::is_active) {
            if owner.immediate {
                Duration::ZERO
            } else {
                owner
                    .live
                    .as_ref()
                    .and_then(LiveProgram::next_deadline)
                    .map_or(Duration::from_millis(2), |deadline| {
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(2))
                    })
            }
        } else if owner.camera_pending.is_some(){Duration::from_millis(2)}else {
            Duration::from_millis(8)
        };
        match jobs.recv_timeout(timeout) {
            Ok(job) => {
                let reply = owner.handle(job);
                if replies.try_send(reply).is_err() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let keep = unique_open_bindings(&owner.streams);
                close_obsolete(&mut owner.streams, &keep);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        // Bounded fair pass before rendering due Program blocks. The packet's
        // complete transport key was captured before file decode admission.
        for _ in 0..16 {
            match media_packets.try_recv() {
                Ok(packet) => {
                    if let Some(live) = &mut owner.live {
                        live.ingest_media(&packet);
                    }
                }
                Err(_) => break,
            }
        }
        for _ in 0..16 {match browser_packets.try_recv(){Ok(packet)=>{if let Some(live)=&mut owner.live {live.ingest_browser(&packet);}},Err(_)=>break}}
        for _ in 0..16 {match ndi_packets.try_recv(){Ok(packet)=>{if let Some(live)=&mut owner.live {live.ingest_ndi(&packet);}},Err(_)=>break}}
        for _ in 0..16 {match embedded_packets.try_recv(){Ok(packet)=>{if let Some(live)=&mut owner.live {live.ingest_embedded(&packet,limits);}},Err(_)=>break}}
        service_push_audio(&mut push_holds,&push_events,&mut owner,&push_fault);
        if !owner.worked {owner.periodic();}
        if owner.camera_pending.is_some(){
            if owner.live.is_none()&&owner.camera_pending.as_ref().is_some_and(|pending|pending.changes.iter().any(|e|e.enabled&&e.binding.audio().is_some()&&owner.sources.iter().any(|(id,_)|*id==e.source))){owner.drain(MediaTime(owner.camera_pending.as_ref().unwrap().changes.first().map_or(0,|e|e.at.0)),Instant::now(),false);}
            let keep=owner.bindings();close_obsolete(&mut owner.streams,&keep);
        }
        owner.poll_camera_activation();
    }
    if let Some(pending)=owner.camera_pending.take(){pending.watch.publish(crate::camera_activation::Phase::Failed,Some(crate::camera_activation::Problem {code:"audio_capture_closed",detail:"Audio owner closed before camera AV retirement acknowledged".into()}),Some(pending.cut));}
    owner.retire_live();
    owner.monitor.join(); // background owner waits for actual native Stop/release
    drop(media_packets);
    drop(ndi_packets);
    drop(browser_packets);
    drop(embedded_packets);
    owner.latest.clear();
    // No GUI join: unacknowledged native stops and external output readers
    // remain charged on this owner until their resources really retire.
    loop {
        close_obsolete(&mut owner.streams, &[]);
        for slot in &mut owner.window_slots {slot.invalidated=true;close_obsolete(&mut slot.streams,&[]);}
        owner.reap();
        if owner.streams.is_empty() && owner.window_slots.is_empty() && owner.pool.is_empty() && owner.retired.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
}
fn unique_open_bindings(streams: &[OpenStream]) -> Vec<AudioCaptureBinding> {
    streams
        .iter()
        .filter(|stream| !stream.closing)
        .map(|stream| stream.binding.clone())
        .collect()
}
#[allow(clippy::too_many_arguments)]
fn capture_latest(
    backend: &mut dyn NativeAudioBackend,
    binding: &AudioCaptureBinding,
    window: Option<&super::window_observation::WindowAudioEvidence>,
    streams: &mut Vec<OpenStream>,
    pool: &mut Vec<Resident>,
    sequence: &mut u64,
    batch_bytes: &mut usize,
    now: MediaTime,
    sent: Instant,
    limits: AudioCaptureLimits,
    mut program: Option<&mut LiveProgram>,
    max_packets: usize,
    drained: &mut usize,
) -> PacketResult {
    // A Window caller owns a separate stream vector for that source and
    // observation generation; never pass the shared native-binding pool.
    if window.is_some_and(|evidence|evidence.observation().binding().audio()!=Some(binding)) {
        return Err(AudioCaptureError::Invalid("window audio binding does not match the observed video source"));
    }
    if window.is_some_and(|evidence|!evidence.current()) {
        return Err(AudioCaptureError::DeviceUnavailable("The observed video window changed before audio capture".into()));
    }
    if streams
        .iter()
        .any(|stream| stream.binding == *binding && stream.closing)
    {
        return Err(AudioCaptureError::Busy);
    }
    if !streams.iter().any(|stream| stream.binding == *binding) {
        if streams.len() >= limits.max_streams {
            return Err(AudioCaptureError::Limit("active/closing native streams"));
        }
        let native_anchor = backend.host_now_ns()?;
        let delay = u64::try_from(sent.elapsed().as_nanos())
            .map_err(|_| AudioCaptureError::TimeOverflow)?;
        let media_anchor = i128::from(
            now.0
                .checked_add(delay)
                .ok_or(AudioCaptureError::TimeOverflow)?,
        );
        let stream = if let Some(evidence)=window {
            backend.open_window_observation(evidence.observation(),limits)?
        } else {
            backend.open(binding, limits)?
        };
        let description = stream
            .description()
            .filter(|name| !name.is_empty() && name.len() <= 1024);
        let generation = next(*sequence)?;
        *sequence = generation;
        streams.push(OpenStream {
            description,
            binding: binding.clone(),
            stream,
            native_anchor,
            media_anchor,
            generation,
            delivered: false,
            closing: false,
            retry_close_after: None,
            close_problem:None,
            owner_session:0,owner_sources:Vec::new(),
        });
        // A native activation may have raced a video-window change. Retain
        // the stream as closing so owner-side retirement still accounts for
        // asynchronous WASAPI resources; never publish its PCM.
        if window.is_some_and(|evidence|!evidence.current()) {
            if let Some(stream)=streams.last_mut(){stream.closing=true;}
            return Err(AudioCaptureError::DeviceUnavailable("The observed video window changed while audio capture opened".into()));
        }
    }
    let stream = streams
        .iter_mut()
        .find(|stream| stream.binding == *binding && !stream.closing)
        .ok_or(AudioCaptureError::Busy)?;
    let resident_bytes: usize = pool.iter().map(|entry| entry.bytes).sum();
    let worst = limits.max_packet_bytes
        + std::mem::size_of::<CapturedAudioPacket>()
        + 8 * std::mem::size_of::<crate::audio::ChannelMeter>()
        + 1024
        + 2 * std::mem::size_of::<usize>()
        + 2048;
    let meter_admitted = pool.len() < 4096
        && resident_bytes.saturating_add(worst * 2) <= limits.max_resident_bytes
        && batch_bytes.saturating_add(worst) <= limits.max_batch_bytes;
    if !meter_admitted && program.is_none() {
        return Err(AudioCaptureError::Backpressure(
            "retained audio packet pool",
        ));
    }
    let mut latest = None;
    let mut coalesced = 0;
    let previously_delivered = stream.delivered;
    for _ in 0..max_packets {
        if window.is_some_and(|evidence|!evidence.current()) {
            stream.closing=true;
            return Err(AudioCaptureError::DeviceUnavailable("The observed video window changed during audio capture".into()));
        }
        let Some(mut block) = stream.stream.poll()? else {
            break;
        };
        if window.is_some_and(|evidence|!evidence.current()) {
            stream.closing=true;
            return Err(AudioCaptureError::DeviceUnavailable("The observed video window changed while reading audio".into()));
        }
        *drained += 1;
        block.validate(limits)?;
        block.samples = block.samples.into_boxed_slice().into_vec();
        let (native_first, timing_origin) =
            if binding.use_device_timing() && !block.flags.timestamp_error {
                if let Some(device) = block.device_timestamp_ns {
                    (i128::from(device), AudioTimingOrigin::Device)
                } else {
                    (estimated_first(&block)?, AudioTimingOrigin::ReceiveEstimate)
                }
            } else {
                (estimated_first(&block)?, AudioTimingOrigin::ReceiveEstimate)
            };
        let first_sample_media_ns = stream
            .media_anchor
            .checked_add(native_first - i128::from(stream.native_anchor))
            .ok_or(AudioCaptureError::TimeOverflow)?;
        let issued = next(*sequence)?;
        if !stream.delivered {
            block.flags.discontinuity = true;
        }
        let meters = raw_meters(&block);
        let packet = CapturedAudioPacket {
            resolved_device: stream.description.clone(),
            binding: binding.clone(),
            sequence: issued,
            requested_at_ns: now.0,
            stream_generation: stream.generation,
            first_sample_media_ns,
            timing_origin,
            coalesced_packets: 0,
            meters,
            block,
        };
        if let Some(program) = program.as_deref_mut() {
            if let Some(evidence)=window {program.ingest_window(evidence,&packet);} else {program.ingest(&packet);}
        }
        *sequence = issued;
        stream.delivered = true;
        if latest.replace(packet).is_some() {
            coalesced += 1;
        }
    }
    let Some(mut packet) = latest else {
        return Ok(None);
    };
    // Only the presentation view coalesces. Every admitted Program packet was
    // already processed with its own timestamp/sequence above.
    packet.coalesced_packets = coalesced;
    packet.block.flags.discontinuity |= !previously_delivered;
    if !meter_admitted {
        return Err(AudioCaptureError::Backpressure(
            "retained audio packet pool; Program packets processed",
        ));
    }
    let packet = Arc::new(packet);
    let bytes = charge(&packet);
    *batch_bytes = batch_bytes
        .checked_add(bytes)
        .ok_or(AudioCaptureError::Limit("audio batch bytes"))?;
    if *batch_bytes > limits.max_batch_bytes
        || resident_bytes.saturating_add(bytes) > limits.max_resident_bytes
    {
        return Err(AudioCaptureError::Backpressure(
            "audio packet byte accounting",
        ));
    }
    pool.push(Resident {
        packet: packet.clone(),
        bytes,
    });
    Ok(Some(packet))
}
fn raw_meters(block: &NativePcmBlock) -> Vec<crate::audio::ChannelMeter> {
    let channels = usize::from(block.format.channels);
    let mut peak = [0.0f64; 8];
    let mut squares = [0.0f64; 8];
    for frame in block.samples.chunks_exact(channels) {
        for (channel, value) in frame.iter().enumerate() {
            let value = f64::from(*value);
            peak[channel] = peak[channel].max(value.abs());
            squares[channel] += value * value;
        }
    }
    (0..channels)
        .map(|channel| crate::audio::ChannelMeter {
            peak: peak[channel],
            rms: (squares[channel] / block.frames as f64).sqrt(),
        })
        .collect()
}
fn estimated_first(block: &NativePcmBlock) -> Result<i128, AudioCaptureError> {
    let duration = (block.frames as u128)
        .checked_mul(1_000_000_000)
        .ok_or(AudioCaptureError::TimeOverflow)?
        / u128::from(block.format.sample_rate);
    Ok(i128::from(block.host_received_ns)
        - i128::try_from(duration).map_err(|_| AudioCaptureError::TimeOverflow)?)
}
fn validate_processes(devices:Vec<AudioDeviceInfo>,limits:AudioCaptureLimits)->Result<Vec<AudioDeviceInfo>,AudioCaptureError> {
    if devices.iter().any(|device| !matches!(device.target,AudioTarget::WindowsProcess {..}|AudioTarget::WindowsWindow {..}) || device.is_default) {
        return Err(AudioCaptureError::Invalid("application discovery returned a non-process target"));
    }
    validate_devices(devices,AudioFlow::Output,limits)
}
fn validate_devices(
    devices: Vec<AudioDeviceInfo>,
    flow: AudioFlow,
    limits: AudioCaptureLimits,
) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
    if devices.len() > limits.max_devices {
        return Err(AudioCaptureError::Limit("enumerated audio devices"));
    }
    let mut targets = Vec::new();
    let mut normalized = Vec::new();
    for mut device in devices {
        let binding = AudioCaptureBinding::new(device.flow, device.target.clone(), false, false)
            .map_err(|_| AudioCaptureError::Invalid("enumerated audio target"))?;
        device.target = binding.target().clone();
        if device.flow != flow
            || device.name.trim().is_empty()
            || device.name.len() > 1024
            || device.name.chars().any(char::is_control)
            || targets.contains(&device.target)
        {
            return Err(AudioCaptureError::Invalid("enumerated audio metadata"));
        }
        targets.push(device.target.clone());
        device.name = device.name.into_boxed_str().into_string();
        normalized.push(device);
    }
    Ok(normalized)
}
