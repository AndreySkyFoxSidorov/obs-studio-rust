use super::audio_sink::{AudioPermit, AudioState, RecordingAudioSink, block_charge};
use super::{RecordingConfig, RecordingError, RecordingFactory, RecordingWriter, native_factory};
use crate::{
    audio_program::ProgramAudioBlock, capture::CaptureSnapshot, playback::PlaybackSnapshot,
    preview::OutputRenderer, transition::FrameComposition,
};
use serde::Serialize;
use super::pause::{PauseClock,RecordingControlKey,RecordingPauseReceipt};
use skylive_core::StudioSnapshot;
use skylive_media::{AudioConvertLimits, AudioConverter, AudioFormat, MediaTime};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

static NEXT_FILE: AtomicU64 = AtomicU64::new(1);
static NEXT_RECORDING: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Debug)]
pub struct RecordingFrame {
    pub session: u64,
    pub time: MediaTime,
    pub model: Arc<StudioSnapshot>,
    pub output: FrameComposition,
    pub playback: PlaybackSnapshot,
    pub capture: Arc<CaptureSnapshot>,
    pub resource_generation: u64,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct RecordingStatus {
    pub recording:u64,
    pub active: bool,
    pub finishing: bool,
    pub starting:bool,
    pub paused:bool,
    pub pause_pending:bool,
    pub removed_ns:u64,
    pub pause_cut_ns:Option<u64>,
    pub session: u64,
    pub video_frames: u64,
    pub audio_frames: u64,
    pub audio_gap_frames: u64,
    pub skipped_video_frames: u64,
    pub degraded_video_frames: u64,
    pub prepared_video_cache_hits: u64,
    pub prepared_video_cache_misses: u64,
    pub warnings: Option<Arc<super::RecordingWarnings>>,
    pub path: Option<PathBuf>,
    pub problem: Option<RecordingError>,
    /// Latest fully rendered and writer-accepted direct-scene camera samples.
    /// They do not claim network delivery, file finalization or a shared
    /// Program publication across independent output owners.
    #[serde(skip)]
    pub(crate) camera_receipts: Vec<super::camera_receipt::CameraReceipt>,
    /// Earliest still-current accepted frame for each active camera scope.
    /// It is metadata only and bounded by the number of scene sources.
    #[serde(skip)]
    pub(crate) first_camera_receipts: Vec<super::camera_receipt::CameraReceipt>,
    #[serde(skip)]
    pub(crate) ndi_video_receipts: Vec<super::ndi_receipt::NdiVideoReceipt>,
    /// First writer-accepted NDI frame per current history; independent of
    /// camera anchors and of other output owners.
    #[serde(skip)]
    pub(crate) first_ndi_video_receipts: Vec<super::ndi_receipt::NdiVideoReceipt>,
}
impl RecordingStatus {
    /// Metadata for the most recent accepted direct-scene frame. Entries are
    /// sink-specific and must not be used as acknowledgements by other sinks.
    pub fn camera_receipts(&self) -> &[super::camera_receipt::CameraReceipt] { &self.camera_receipts }
    pub(crate) fn first_camera_receipts(&self) -> &[super::camera_receipt::CameraReceipt] { &self.first_camera_receipts }
    pub fn ndi_video_receipts(&self) -> &[super::ndi_receipt::NdiVideoReceipt] { &self.ndi_video_receipts }
    pub(crate) fn first_ndi_video_receipts(&self) -> &[super::ndi_receipt::NdiVideoReceipt] { &self.first_ndi_video_receipts }
}
#[derive(Clone, Debug, Serialize)]
pub enum RecordingEvent {
    PauseChanged {key:RecordingControlKey,result:Result<RecordingPauseReceipt,RecordingError>},
    Started,
    Finished {
        path: PathBuf,
        video_frames: u64,
        audio_frames: u64,
    },
    Failed(RecordingError),
}
pub(super) struct VideoPermit(Arc<AtomicUsize>);
impl Drop for VideoPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
pub(super) enum Work {
    Pause {key:RecordingControlKey,paused:bool,at:MediaTime},
    Start {
        session: u64,
        config: RecordingConfig,
        origin: MediaTime,
        factory: RecordingFactory,
        audio: Arc<AudioState>,
    },
    Video {
        frame: RecordingFrame,
        _permit: VideoPermit,
    },
    Audio {
        token: u64,
        block: Arc<ProgramAudioBlock>,
        _permit: AudioPermit,
    },
    Stop,
}
#[derive(Default)]
struct Admission {
    recording:u64,session:u64,paused:bool,finishing:bool,audio_rate:Option<u32>,audio_end:Option<u64>,
    audio_tracks:u8,
}
pub(super) struct Ingress {
    pub sender: mpsc::SyncSender<Work>,
    gate:Arc<Mutex<Admission>>,
}
impl Ingress {
    /// FIFO barrier shared by UI video/control and the unique Program producer.
    /// Validate/allocate outside this lock; no native/render/file call is made here.
    pub(super) fn send(&self,work:Work)->Result<bool,RecordingError> {
        let gate=self.gate.lock().map_err(|_|RecordingError::invalid("recording admission gate poisoned"))?;
        self.send_locked(gate,work)
    }
    fn try_send(&self,work:Work)->Result<bool,RecordingError> {
        let gate=self.gate.try_lock().map_err(|_|RecordingError {code:"recording_backpressure",detail:"recording admission is busy".into(),recovery_path:None})?;
        self.send_locked(gate,work)
    }
    fn send_locked(&self,mut gate:std::sync::MutexGuard<'_,Admission>,work:Work)->Result<bool,RecordingError> {
        let mut audio_end=None;
        match &work {
            Work::Video {frame,..} => {
                if gate.finishing || frame.session!=gate.session {return Err(RecordingError::invalid("recording video admission scope changed"));}
                if gate.paused {return Err(RecordingError {code:"recording_paused",detail:"Recording video is paused".into(),recovery_path:None});}
            }
            Work::Audio {token,block,..} => {
                if *token!=gate.recording || block.session!=gate.session || gate.audio_rate.is_some_and(|rate|rate!=block.settings.sample_rate()) {
                    return Err(RecordingError::invalid("recording audio admission scope changed"));
                }
                block_charge(&block)?;
                if block.track_mask().bits()&gate.audio_tracks!=gate.audio_tracks {
                    return Err(RecordingError::invalid("recording PCM bundle is missing a selected track"));
                }
                let end=block.start_frame.checked_add(block.frames as u64).ok_or_else(||RecordingError::invalid("recording audio admission grid overflow"))?;
                if gate.audio_end.is_some_and(|previous|block.start_frame<previous) {return Err(RecordingError::invalid("recording original PCM watermark moved backwards"));}
                audio_end=Some(end);
                // Pause affects original-clock ranges on Active, not delivery
                // wall time. Pre-pause samples can arrive after this control.
                if gate.finishing {gate.audio_end=audio_end;return Ok(false);}
            }
            Work::Pause {key,..} => {
                if key.recording!=gate.recording || key.session!=gate.session || gate.finishing {return Err(RecordingError::invalid("recording pause admission scope changed"));}
            }
            _=>{}
        }
        let next=match &work {
            Work::Start {session,audio,config,..}=>Some((audio.id,*session,config.audio.map(|format|format.sample_rate),audio.tracks.bits())),_=>None,
        };
        let pause=match &work {Work::Pause {paused,..}=>Some(*paused),_=>None};
        let stop=matches!(&work,Work::Stop);
        match self.sender.try_send(work) {
            Ok(())=>{
                if let Some((recording,session,audio_rate,audio_tracks))=next {*gate=Admission {recording,session,audio_rate,audio_tracks,..Default::default()};}
                if let Some(paused)=pause {gate.paused=paused;}
                if stop {gate.finishing=true;}
                if audio_end.is_some(){gate.audio_end=audio_end;}
                Ok(true)
            }
            Err(mpsc::TrySendError::Full(_))=>Err(RecordingError {code:"recording_backpressure",detail:"recording input queue is full".into(),recovery_path:None}),
            Err(mpsc::TrySendError::Disconnected(_))=>Err(RecordingError::invalid("recording worker disconnected")),
        }
    }
}
pub struct RecordingOwner {
    sender: Option<Arc<Ingress>>,
    audio: Option<Arc<AudioState>>,
    audio_format: Option<super::RecordingAudio>,
    receiver: Option<mpsc::Receiver<RecordingEvent>>,
    queued_videos: Arc<AtomicUsize>,
    state: Arc<Mutex<RecordingStatus>>,
    latest: RecordingStatus,
    pending: bool,
    started:bool,
    paused_ack:bool,
    origin:MediaTime,
    control_serial:u64,
    control_pending:Option<RecordingControlKey>,
    stop_pending: bool,
    stop_sent: bool,
    thread: Option<thread::JoinHandle<()>>,
}
impl RecordingOwner {
    pub fn new() -> std::io::Result<Self> {
        Self::new_with_render_gpu(crate::render_gpu::RenderGpu::cpu())
    }
    pub fn new_with_render_gpu(render_gpu:crate::render_gpu::RenderGpu) -> std::io::Result<Self> {
        // PCM and video have independent permits; reserve control admission so
        // a saturated data queue cannot strand Stop behind GUI polling.
        //40 audio +3 video +one Pause/Resume +one Stop control credit.
        let (sender, input) = mpsc::sync_channel(45);
        let admission=Arc::new(Mutex::new(Admission::default()));
        let worker_admission=admission.clone();
        // Started must be consumed before one Pause/Resume is admitted.
        // Capacity2 retains one control acknowledgement plus one terminal;
        // its pending control credit is released only by frontend consumption.
        let (output, receiver) = mpsc::sync_channel(2);
        let state = Arc::new(Mutex::new(RecordingStatus::default()));
        let published = state.clone();
        let handle = thread::Builder::new()
            .name("skylive-recording".into())
            .spawn(move || {
                let _render_services =
                    crate::preview::retain_render_services().inspect_err(|error| {
                        eprintln!("recording render service {}: {}", error.code(), error);
                    });
                let mut active: Option<Active> = None;
                loop {
                    let received = if active.is_some() {
                        input.recv_timeout(std::time::Duration::from_millis(5))
                    } else {
                        match input.recv() {
                            Ok(work) => Ok(work),
                            Err(_) => Err(mpsc::RecvTimeoutError::Disconnected),
                        }
                    };
                    let work = match received {
                        Ok(work) => work,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let finishing = active.as_ref().is_some_and(|writer| {
                                writer.audio_state.finish.load(Ordering::Acquire)
                                    && writer.audio_state.ended.load(Ordering::Acquire)
                            });
                            if finishing {
                                if let Some(mut writer) = active.take() {
                                    let mut status = writer.status.clone();
                                    status.active = false;
                                    status.finishing = false;
                                    status.camera_receipts.clear();status.first_camera_receipts.clear();status.ndi_video_receipts.clear();status.first_ndi_video_receipts.clear();
                                    let fault = writer.audio_state.fault();
                                    let event=if let Some(mut error)=fault {
                                        if let Err(native)=writer.writer.finalize(){error.detail.push_str(&format!("; temporary prefix finalization failed: {}",native.detail));}
                                        error.recovery_path=Some(writer.file.temporary.clone());status.problem=Some(error.clone());
                                        eprintln!("recording Program tail failed {}: {}; recovery={}",error.code,error.detail,writer.file.temporary.display());
                                        RecordingEvent::Failed(error)
                                    }else{match writer.finish(){Ok(event)=>event,Err(error)=>{status.problem=Some(error.clone());RecordingEvent::Failed(error)}}};
                                    // No native resources remain held across event publication.
                                    drop(writer);
                                    set_state(&published, status);
                                    emit_event(&output, &published, event);
                                }
                            }
                            continue;
                        }
                    };
                    let result = match work {
                        Work::Pause {key,paused,at}=>{
                            if let Some(writer)=active.as_mut().filter(|writer|writer.audio_state.id==key.recording && writer.session==key.session) {
                                let result=writer.change_pause(key,paused,at);
                                if let Ok(mut gate)=worker_admission.lock() {if gate.recording==key.recording {gate.paused=writer.status.paused;}}
                                set_state(&published,writer.status.clone());
                                emit_event(&output,&published,RecordingEvent::PauseChanged {key,result});
                            }
                            Ok(())
                        }
                        Work::Start {
                            session,
                            config,
                            origin,
                            factory,
                            audio,
                        } => {
                            if active.is_some() {
                                Err(RecordingError::invalid("recording already active"))
                            } else {
                                match Active::new(session, config, origin, factory, audio.clone()) {
                                    Ok(mut writer) => {
                                        writer.render_gpu=render_gpu.clone();
                                        writer.renderer=writer.render_gpu.output_renderer();
                                        set_state(&published, writer.status.clone());
                                        active = Some(writer);
                                        emit_event(&output, &published, RecordingEvent::Started);
                                        Ok(())
                                    }
                                    Err(error) => {
                                        audio.finish.store(true, Ordering::Release);
                                        Err(error)
                                    }
                                }
                            }
                        }
                        Work::Video { frame, _permit } => {
                            if let Some(writer) = active.as_mut() {
                                writer.video(frame)
                            } else {
                                Ok(())
                            }
                        }
                        Work::Audio {
                            token,
                            block,
                            _permit,
                        } => {
                            let _hold = _permit;
                            if let Some(writer) = active.as_mut() {
                                if token != writer.audio_state.id {
                                    Ok(())
                                } else {
                                    writer.audio(block)
                                }
                            } else {
                                Ok(())
                            }
                        }
                        Work::Stop => {
                            if let Some(mut writer) = active.take() {
                                let mut status = writer.status.clone();
                                status.finishing = true;
                                status.camera_receipts.clear();status.first_camera_receipts.clear();status.ndi_video_receipts.clear();status.first_ndi_video_receipts.clear();
                                set_state(&published, status);
                                let event = match writer.finish() {
                                    Ok(event) => event,
                                    Err(error) => RecordingEvent::Failed(error),
                                };
                                let mut status = writer.status.clone();
                                status.active = false;
                                status.finishing = false;
                                status.camera_receipts.clear();status.first_camera_receipts.clear();status.ndi_video_receipts.clear();status.first_ndi_video_receipts.clear();
                                if let RecordingEvent::Failed(error) = &event {
                                    status.problem = Some(error.clone());
                                }
                                drop(writer);
                                set_state(&published, status);
                                emit_event(&output, &published, event);
                            }
                            Ok(())
                        }
                    };
                    if let Err(mut error) = result {
                        if let Some(mut writer) = active.take() {
                            writer.audio_state.finish.store(true, Ordering::Release);
                            error.recovery_path = Some(writer.file.temporary.clone());
                            let _ = writer.writer.finalize();
                        }
                        let status = RecordingStatus {
                            problem: Some(error.clone()),
                            ..Default::default()
                        };
                        set_state(&published, status);
                        emit_event(&output, &published, RecordingEvent::Failed(error));
                    } else if let Some(writer) = active.as_ref() {
                        set_state(&published, writer.status.clone());
                    }
                }
                // Closing the UI does not perform native finalization on its thread.
                // The admitted worker retains every encoder/file handle until done.
                if let Some(mut writer)=active {
                    let fault=writer.audio_state.fault();
                    let event=if let Some(mut error)=fault {
                        // An abruptly destroyed Program producer cannot prove
                        // its requested cutoff. Finalize only the recoverable
                        // private prefix; never publish the requested final URL.
                        if let Err(native)=writer.writer.finalize(){error.detail.push_str(&format!("; temporary prefix finalization failed: {}",native.detail));}
                        error.recovery_path=Some(writer.file.temporary.clone());
                        eprintln!("recording interrupted {}: {}; recovery={}",error.code,error.detail,writer.file.temporary.display());
                        RecordingEvent::Failed(error)
                    }else{match writer.finish(){Ok(event)=>event,Err(error)=>{eprintln!("recording retirement {}: {}; recovery={:?}",error.code,error.detail,error.recovery_path);RecordingEvent::Failed(error)}}};
                    let mut status=writer.status.clone();status.active=false;status.finishing=false;status.camera_receipts.clear();status.first_camera_receipts.clear();status.ndi_video_receipts.clear();status.first_ndi_video_receipts.clear();
                    if let RecordingEvent::Failed(error)=&event{status.problem=Some(error.clone());}
                    drop(writer);set_state(&published,status);emit_event(&output,&published,event);
                }
            })?;
        Ok(Self {
            sender: Some(Arc::new(Ingress { sender,gate:admission })),
            audio: None,
            audio_format: None,
            receiver: Some(receiver),
            queued_videos: Default::default(),
            state,
            latest: Default::default(),
            pending: false,
            started:false,
            paused_ack:false,
            origin:MediaTime(0),
            control_serial:0,
            control_pending:None,
            stop_pending: false,
            stop_sent: false,
            thread: Some(handle),
        })
    }
    pub fn status(&mut self) -> RecordingStatus {
        if let Ok(value) = self.state.try_lock() {
            self.latest = value.clone();
        }
        let mut value = self.latest.clone();
        value.finishing |= self.stop_pending;
        value.starting=self.pending && !self.started;
        value.paused=self.pending && self.paused_ack;
        value.recording=self.audio.as_ref().map_or(0,|audio|audio.id);
        value.pause_pending=self.control_pending.is_some();
        value
    }
    pub fn is_active(&self) -> bool {
        self.pending
    }
    pub fn start(
        &mut self,
        session: u64,
        config: RecordingConfig,
        origin: MediaTime,
    ) -> Result<(), RecordingError> {
        self.start_with(session, config, origin, native_factory())
    }
    pub fn start_with(
        &mut self,
        session: u64,
        config: RecordingConfig,
        origin: MediaTime,
        factory: RecordingFactory,
    ) -> Result<(), RecordingError> {
        let mask=if config.audio.is_some(){1}else{0};
        self.start_tracks(session,config,origin,factory,mask)
    }
    /// One admitted original-grid bundle supplies every frozen selected track.
    pub fn start_tracks(&mut self,session:u64,config:RecordingConfig,origin:MediaTime,factory:RecordingFactory,mask:u8)->Result<(),RecordingError>{
        let tracks=skylive_core::SourceAudioTrackMask::new(mask).map_err(|_|RecordingError::invalid("recording audio track mask"))?;
        if config.audio.is_some()==tracks.is_empty(){return Err(RecordingError::invalid("recording audio requires selected tracks; video-only requires no audio tracks"));}
        config.validate()?;
        if self.pending {
            return Err(RecordingError::invalid(
                "wait for recording to finish before starting again",
            ));
        }
        if session == 0 {
            return Err(RecordingError::invalid("recording session must be nonzero"));
        }
        let id = NEXT_RECORDING
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
            .map_err(|_| RecordingError::invalid("recording identity exhausted"))?;
        let audio = Arc::new(if tracks.bits()==1{AudioState::new(id,session)}else{AudioState::new_tracks(id,session,tracks)});
        let audio_format = config.audio;
        self.try_work(Work::Start {
            session,
            config,
            origin,
            factory,
            audio: audio.clone(),
        })?;
        self.audio = Some(audio);
        self.audio_format = audio_format;
        self.pending = true;
        self.started=false;
        self.paused_ack=false;
        self.origin=origin;
        self.control_pending=None;
        self.stop_sent = false;
        Ok(())
    }
    fn try_work(&self,work:Work)->Result<(),RecordingError> {
        self.sender.as_ref().ok_or_else(||RecordingError::invalid("recording owner closed"))?.try_send(work).map(|_|())
    }
    pub fn set_paused(&mut self,recording:u64,session:u64,at:MediaTime,paused:bool)->Result<Option<RecordingControlKey>,RecordingError> {
        if !self.pending || !self.started || self.stop_pending || self.control_pending.is_some() {
            return Err(RecordingError::invalid("wait for recording start or pause acknowledgement; Stop remains available"));
        }
        let audio=self.audio.as_ref().ok_or_else(||RecordingError::invalid("recording control scope unavailable"))?;
        if audio.id!=recording || audio.session!=session || audio.finish.load(Ordering::Acquire) {return Err(RecordingError::invalid("recording pause session changed or output is finishing"));}
        if at<self.origin || at.0.saturating_sub(self.origin.0)>24*60*60*1_000_000_000 {return Err(RecordingError::invalid("recording pause time is outside the active recording timeline"));}
        if self.paused_ack==paused {return Ok(None);}
        let serial=self.control_serial.checked_add(1).ok_or_else(||RecordingError::invalid("recording control identity exhausted"))?;
        let key=RecordingControlKey {recording:self.audio.as_ref().expect("current recording").id,session,serial};
        self.try_work(Work::Pause {key,paused,at})?;self.control_serial=serial;self.control_pending=Some(key);Ok(Some(key))
    }
    pub fn video(&mut self, frame: RecordingFrame) -> Result<(), RecordingError> {
        if !self.pending
            || self.stop_pending
            || self
                .audio
                .as_ref()
                .is_some_and(|state| state.finish.load(Ordering::Acquire))
        {
            return Err(RecordingError::invalid(
                "recording does not accept new frames",
            ));
        }
        if frame
            .capture
            .frames()
            .map(|(_, f)| f.image.byte_len())
            .sum::<usize>()
            > 256 * 1024 * 1024
        {
            return Err(RecordingError::invalid(
                "recording capture input exceeds256MiB",
            ));
        }
        self.queued_videos
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                (value < 3).then_some(value + 1)
            })
            .map_err(|_| RecordingError {
                code: "recording_backpressure",
                detail: "recording video work limit reached".into(),
                recovery_path: None,
            })?;
        let permit=VideoPermit(self.queued_videos.clone());
        let end=frame.time.0.checked_add(u64::try_from((1_000_000_000u128*u128::from(frame.model.output_video().map_err(|e|RecordingError::native("recording output format",e))?.fps_den())).div_ceil(u128::from(frame.model.output_video().map_err(|e|RecordingError::native("recording output format",e))?.fps_num()))).map_err(|_|RecordingError::invalid("recording frame end overflow"))?).ok_or_else(||RecordingError::invalid("recording frame end overflow"))?;
        self.try_work(Work::Video {frame,_permit:permit})?;
        if let Some(state)=&self.audio{state.video_end_ns.fetch_max(end,Ordering::AcqRel);}Ok(())
    }
    pub fn audio(&mut self, block: Arc<ProgramAudioBlock>) -> Result<(), RecordingError> {
        if !self.pending
            || self.stop_pending
            || self
                .audio
                .as_ref()
                .is_some_and(|state| state.finish.load(Ordering::Acquire))
        {
            return Err(RecordingError::invalid(
                "recording does not accept new audio",
            ));
        }
        let state = self
            .audio
            .as_ref()
            .ok_or_else(|| RecordingError::invalid("recording audio state unavailable"))?;
        if state.registered.load(Ordering::Acquire) {
            return Err(RecordingError::invalid(
                "manual audio is disabled while live Program recording is attached",
            ));
        }
        if block.track_mask().bits()&state.tracks.bits()!=state.tracks.bits(){return Err(RecordingError::invalid("manual recording bundle is missing a selected track"));}
        let bytes = block_charge(&block)?;
        self.try_work(Work::Audio {
            token: state.id,
            block,
            _permit: state.permit(bytes)?,
        })?;
        state.manual.store(true, Ordering::Release);
        Ok(())
    }
    pub fn audio_sink(
        &mut self,
        settings: skylive_core::AudioSettings,
    ) -> Result<RecordingAudioSink, RecordingError> {
        if !self.pending || self.stop_pending {
            return Err(RecordingError::invalid(
                "recording is not accepting a live audio producer",
            ));
        }
        if self
            .audio_format
            .is_none_or(|format| format.sample_rate != settings.sample_rate())
        {
            return Err(RecordingError::invalid(
                "recording live audio is disabled or has a different sample rate",
            ));
        }
        let ingress = self
            .sender
            .as_ref()
            .ok_or_else(|| RecordingError::invalid("recording owner closed"))?;
        let state = self
            .audio
            .as_ref()
            .ok_or_else(|| RecordingError::invalid("recording audio state unavailable"))?;
        if state.manual.load(Ordering::Acquire) {
            return Err(RecordingError::invalid(
                "cannot attach live recording after manual PCM admission",
            ));
        }
        state
            .registered
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| RecordingError::invalid("recording already has a live audio producer"))?;
        Ok(RecordingAudioSink::new(
            ingress.clone(),
            state.clone(),
            settings,
        ))
    }
    pub fn live_audio_attached(&self) -> bool {
        self.audio
            .as_ref()
            .is_some_and(|state| state.registered.load(Ordering::Acquire))
    }
    pub fn audio_enabled(&self) -> bool {
        self.pending && self.audio_format.is_some()
    }
    pub fn stop(&mut self) -> Result<(), RecordingError> {
        if !self.pending {
            return Ok(());
        }
        self.stop_pending = true;
        if let Some(state)=&self.audio {
            if state.registered.load(Ordering::Acquire){
                let mut cutoff=state.video_end_ns.load(Ordering::Acquire).max(self.origin.0);
                // Already-emitted PCM is immutable. Never pretend a requested
                // cutoff can retract it; finish the actually accepted prefix.
                if let Some(format)=self.audio_format {cutoff=cutoff.max(PauseClock::audio_ns(state.observed_end.load(Ordering::Acquire),format.sample_rate)?);}
                state.cutoff_ns.fetch_min(cutoff,Ordering::AcqRel);
            }else{state.finish.store(true,Ordering::Release);}
        }
        self.flush_stop()
    }
    fn flush_stop(&mut self) -> Result<(), RecordingError> {
        if self.stop_pending && !self.stop_sent {
            if self
                .audio
                .as_ref()
                .is_some_and(|state| state.registered.load(Ordering::Acquire))
            {
                // The unique source producer's Drop is the end barrier. The
                // worker finalizes after observing it and draining accepted PCM.
                return Ok(());
            }
            match self.try_work(Work::Stop) {
                Ok(()) => self.stop_sent = true,
                Err(error) if error.code == "recording_backpressure" => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    pub fn poll(&mut self) -> Result<Option<RecordingEvent>, RecordingError> {
        self.flush_stop()?;
        match self
            .receiver
            .as_ref()
            .ok_or_else(|| RecordingError::invalid("recording owner closed"))?
            .try_recv()
        {
            Ok(event) => {
                if matches!(event,RecordingEvent::Started) {self.started=true;}
                if let RecordingEvent::PauseChanged {key,result}=&event {
                    if self.control_pending!=Some(*key) || self.audio.as_ref().is_none_or(|audio|audio.id!=key.recording || audio.session!=key.session) {return Ok(None);}
                    if let Ok(receipt)=result {self.paused_ack=receipt.paused;}
                    self.control_pending=None;
                }
                if matches!(event,RecordingEvent::Finished {..}|RecordingEvent::Failed(_)) {
                    self.pending=false;self.started=false;self.paused_ack=false;self.stop_pending=false;self.control_pending=None;self.audio=None;
                }
                Ok(Some(event))
            }
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(RecordingError::invalid("recording worker disconnected"))
            }
        }
    }
    pub fn close(&mut self) {
        // The unique Program sink retains bounded ingress through its cutoff.
        // Closing the UI requests graceful tail; no UI join/codec call occurs.
        if self.pending {if let Err(error)=self.stop(){if let Some(state)=&self.audio{state.finish.store(true,Ordering::Release);}eprintln!("recording close Stop {}: {}",error.code,error.detail);}}
        self.receiver.take();
        self.sender.take();
    }
    pub fn join(&mut self) -> Result<(), RecordingError> {
        self.close();
        if let Some(handle) = self.thread.take() {
            handle
                .join()
                .map_err(|_| RecordingError::invalid("recording worker panicked"))?;
        }
        Ok(())
    }
}
impl Drop for RecordingOwner {
    fn drop(&mut self) {
        self.close();
    }
}
fn set_state(state: &Mutex<RecordingStatus>, value: RecordingStatus) {
    if let Ok(mut target) = state.lock() {
        *target = value;
    }
}
fn emit_event(
    output: &mpsc::SyncSender<RecordingEvent>,
    state: &Mutex<RecordingStatus>,
    event: RecordingEvent,
) {
    // Receiver disconnect does not abandon accepted input. One pending control
    // may publish one acknowledgement plus a terminal. Full is an invariant
    // fault, never permission to drop a terminal or claim successful pause.
    if let Err(mpsc::TrySendError::Full(_)) = output.try_send(event) {
        if let Ok(mut status) = state.lock() {
            status.problem = Some(RecordingError::invalid(
                "recording terminal event capacity invariant",
            ));
        }
    }
}

struct Active {
    audio_state: Arc<AudioState>,
    session: u64,
    config: RecordingConfig,
    origin: MediaTime,
    file: OwnedFile,
    writer: Box<dyn RecordingWriter>,
    renderer: OutputRenderer,
    render_gpu: crate::render_gpu::RenderGpu,
    prepared: super::prepared_video::FrameCache,
    last_video: Option<u64>,
    resource_generation: Option<u64>,
    next_audio: u64,
    converters:[Option<(AudioFormat,AudioConverter)>;6],
    status: RecordingStatus,
    pause:PauseClock,
}
impl Active {
    fn new(
        session: u64,
        config: RecordingConfig,
        origin: MediaTime,
        factory: RecordingFactory,
        audio_state: Arc<AudioState>,
    ) -> Result<Self, RecordingError> {
        let file = OwnedFile::prepare(&config.path)?;
        let writer = factory(&config, &file.temporary).map_err(|mut error| {
            error.recovery_path = Some(file.temporary.clone());
            error
        })?;
        let status = RecordingStatus {
            recording:audio_state.id,
            active: true,
            session,
            path: Some(file.final_path.clone()),
            ..Default::default()
        };
        Ok(Self {
            audio_state,
            session,
            config,
            origin,
            file,
            writer,
            renderer: Default::default(),
            render_gpu: crate::render_gpu::RenderGpu::cpu(),
            prepared: Default::default(),
            last_video: None,
            resource_generation: None,
            next_audio: 0,
            converters:std::array::from_fn(|_|None),
            status,
            pause:PauseClock::new(origin.0),
        })
    }
    fn change_pause(&mut self,key:RecordingControlKey,paused:bool,at:MediaTime)->Result<RecordingPauseReceipt,RecordingError> {
        let mut coverage=self.audio_state.pause_coverage.lock().map_err(|_|RecordingError::invalid("recording pause coverage lock poisoned"))?;
        let requested=at.0;
        let at=if !paused {if let Some(rate)=self.config.audio.map(|format|format.sample_rate){requested.max(PauseClock::audio_ns(coverage.discarded_end,rate)?)}else{requested}}else{requested};
        let mut receipt=self.pause.change(key,paused,at,self.config.audio.map(|format|format.sample_rate))?;
        receipt.requested_ns=requested;
        self.audio_state.video_end_ns.fetch_max(receipt.effective_ns,Ordering::AcqRel);
        coverage.cut_ns=if paused {Some(receipt.effective_ns)}else{None};
        coverage.discarded_end=0;
        self.status.paused=paused;self.status.removed_ns=self.pause.removed_ns;self.status.pause_cut_ns=self.pause.paused_at;
        self.status.camera_receipts.clear();
        self.status.first_camera_receipts.clear();
        self.status.ndi_video_receipts.clear();self.status.first_ndi_video_receipts.clear();
        self.prepared.clear();
        Ok(receipt)
    }
    fn video(&mut self, frame: RecordingFrame) -> Result<(), RecordingError> {
        if frame.session != self.session
            || frame.playback.session() != self.session
            || frame.time < self.origin
        {
            return Err(RecordingError::invalid(
                "recording video belongs to another session or timeline",
            ));
        }
        if !frame.model.output_video().is_ok_and(|video|video==self.config.video) {
            return Err(RecordingError::invalid(
                "stop recording before changing the video format",
            ));
        }
        if self.status.paused || frame.time.0<self.pause.resume_floor_ns {return Ok(());}
        let elapsed = (frame.time.0-self.origin.0).checked_sub(self.pause.removed_ns).ok_or_else(||RecordingError::invalid("recording resumed video precedes removed interval"))?;
        if elapsed > 24 * 60 * 60 * 1_000_000_000 {
            return Err(RecordingError::invalid("recording duration exceeds24hours"));
        }
        let index = (u128::from(elapsed) * u128::from(self.config.video.fps_num())
            / (1_000_000_000 * u128::from(self.config.video.fps_den()))) as u64;
        if self.last_video.is_some_and(|old| index <= old) {
            return Ok(());
        }
        if self.resource_generation != Some(frame.resource_generation) {
            self.prepared.clear();
            self.renderer = self.render_gpu.output_renderer();
            self.resource_generation = Some(frame.resource_generation);
        }
        let prepared = match self.prepared.frame(
            &frame,
            (self.config.video.width(), self.config.video.height()),
            &mut self.renderer,
        ){Ok(prepared)=>prepared,Err(error) if error.code=="recording_auto_text_retry"=>return Ok(()),Err(error)=>return Err(error)};
        if !prepared.video.get().auto_text_current(){return Ok(());}
        self.writer.video_prepared(prepared.video.get(), index)?;
        // Convert the accepted frame index back to the recording's original
        // media timeline; frame.time is admission time, not its encoded PTS.
        let pts_ns = (u128::from(index) * 1_000_000_000 * u128::from(self.config.video.fps_den()))
            / u128::from(self.config.video.fps_num());
        let pts = u64::try_from(pts_ns).ok().and_then(|pts|self.origin.0.checked_add(self.pause.removed_ns)?.checked_add(pts));
        let frame_ns=(1_000_000_000u128*u128::from(self.config.video.fps_den())).div_ceil(u128::from(self.config.video.fps_num()));
        let end_ns=u64::try_from(frame_ns).ok().and_then(|duration|pts?.checked_add(duration));
        let mut receipts = if prepared.diagnostics.is_empty() {
            match (pts,end_ns) {(Some(pts),Some(end))=>super::camera_receipt::accepted_samples(&frame,prepared.video.get().camera_trace(),self.status.recording,index,MediaTime(pts),MediaTime(end)),_=>Vec::new()}
        } else { Vec::new() };
        self.status.first_camera_receipts.retain(|first|receipts.iter().any(|current|first.same_scope(current)));
        let mut admission_failed=false;
        for receipt in &receipts {
            if !self.status.first_camera_receipts.iter().any(|first|first.same_scope(receipt)) {
                if self.status.first_camera_receipts.try_reserve(1).is_err(){admission_failed=true;break;}
                self.status.first_camera_receipts.push(receipt.clone());
            }
        }
        if admission_failed {self.status.first_camera_receipts.clear();receipts.clear();}
        self.status.camera_receipts=receipts;
        let mut ndi_receipts=if prepared.diagnostics.is_empty() {
            match (pts,end_ns) {(Some(pts),Some(end))=>super::ndi_receipt::accepted_samples(&frame,prepared.video.get().camera_trace(),self.status.recording,index,MediaTime(pts),MediaTime(end)),_=>Vec::new()}
        } else {Vec::new()};
        self.status.first_ndi_video_receipts.retain(|first|ndi_receipts.iter().any(|current|first.same_scope(current)));
        let mut ndi_admission_failed=false;
        for receipt in &ndi_receipts {
            if !self.status.first_ndi_video_receipts.iter().any(|first|first.same_scope(receipt)) {
                if self.status.first_ndi_video_receipts.try_reserve(1).is_err(){ndi_admission_failed=true;break;}
                self.status.first_ndi_video_receipts.push(receipt.clone());
            }
        }
        if ndi_admission_failed {self.status.first_ndi_video_receipts.clear();ndi_receipts.clear();}
        self.status.ndi_video_receipts=ndi_receipts;
        if prepared.hit {
            self.status.prepared_video_cache_hits =
                self.status.prepared_video_cache_hits.saturating_add(1);
        } else {
            self.status.prepared_video_cache_misses =
                self.status.prepared_video_cache_misses.saturating_add(1);
        }
        if !prepared.diagnostics.is_empty() {
            self.status.degraded_video_frames = self.status.degraded_video_frames.saturating_add(1);
        }
        self.status.warnings = super::RecordingWarnings::from_frame(&frame, prepared.diagnostics);
        let expected = self.last_video.map_or(0, |old| old + 1);
        self.status.skipped_video_frames += index.saturating_sub(expected);
        self.status.video_frames += 1;
        self.last_video = Some(index);
        let end=(u128::from(index+1)*1_000_000_000*u128::from(self.config.video.fps_den())).div_ceil(u128::from(self.config.video.fps_num()));
        self.pause.video_end_ns=self.origin.0.checked_add(self.pause.removed_ns).and_then(|base|u64::try_from(end).ok().and_then(|end|base.checked_add(end))).ok_or_else(||RecordingError::invalid("recording video end watermark overflow"))?;
        Ok(())
    }
    fn audio(&mut self, block: Arc<ProgramAudioBlock>) -> Result<(), RecordingError> {
        let Some(format) = self.config.audio else {
            return Ok(());
        };
        if block.session != self.session
            || block.settings.sample_rate() != format.sample_rate
            || block.samples.len()
                != block.frames * usize::from(block.settings.speakers().channels())
        {
            return Err(RecordingError::invalid(
                "recording audio scope/format changed",
            ));
        }
        let end=block.start_frame.checked_add(block.frames as u64).ok_or_else(||RecordingError::invalid("recording audio grid overflow"))?;
        let original_end_ns=PauseClock::audio_ns(end,format.sample_rate)?;
        if original_end_ns.saturating_sub(self.origin.0)>24*60*60*1_000_000_000{return Err(RecordingError::invalid("recording audio timeline exceeds24hours"));}
        let cutoff=self.audio_state.cutoff_ns.load(Ordering::Acquire);
        let end=if cutoff==u64::MAX{end}else{end.min(PauseClock::audio_grid(cutoff,format.sample_rate)?)};
        let spans=self.pause.audio_spans(block.start_frame,end,format.sample_rate)?;
        for span in spans {
            let mut skip=usize::try_from(span.first-block.start_frame).map_err(|_|RecordingError::invalid("recording paused PCM trim overflow"))?;
            let mut count=span.frames;let mut first=span.output_first;
            if first<self.next_audio {
                // Cumulative common-clock quantization can overlap a resume
                // boundary by one sample; trim that single repeated grid cell.
                let overlap=self.next_audio-first;
                if overlap>1{return Err(RecordingError::invalid("recording audio overlap exceeds pause-grid rounding"));}
                let trim=count.min(overlap as usize);skip+=trim;count-=trim;first+=trim as u64;
            }
            if count==0{continue;}
            self.audio_span(&block,format,skip,count,first)?;
        }
        self.pause.audio_end_ns=original_end_ns;
        Ok(())
    }
    fn audio_span(&mut self,block:&ProgramAudioBlock,format:super::RecordingAudio,skip:usize,count:usize,first:u64)->Result<(),RecordingError>{
        let input=AudioFormat::new(format.sample_rate,block.settings.speakers().channels(),block.settings.speakers().channel_mask())
            .map_err(|e|RecordingError::native("recording audio input",e))?;
        let output=AudioFormat::new(format.sample_rate,u16::from(format.channels),if format.channels==1{4}else{3})
            .map_err(|e|RecordingError::native("recording audio output",e))?;
        let channels=usize::from(block.settings.speakers().channels());
        let selected=self.audio_state.tracks;
        let pcm_bytes=count.checked_mul(usize::from(format.channels)).and_then(|n|n.checked_mul(2))
            .and_then(|n|n.checked_mul(selected.bits().count_ones()as usize))
            .ok_or_else(||RecordingError::invalid("recording track PCM bytes"))?;
        if count>4096||pcm_bytes>6*4096*2*2{return Err(RecordingError::invalid("recording track conversion profile"));}
        // Every converter is distinct even when the rematrix is same-rate.
        // Allocate/configure all selected states before converting any track.
        for number in 1..=6{let track=skylive_core::AudioTrackId::new(number).map_err(|_|RecordingError::invalid("recording track ID"))?;
            if !selected.contains(track){continue;}
            let state=&mut self.converters[track.index()];
            if state.as_ref().is_some_and(|(old,_)|*old!=input){return Err(RecordingError::invalid("recording speaker layout changed"));}
            if state.is_none(){*state=Some((input,AudioConverter::new(input,output,AudioConvertLimits{max_buffered_input_frames:4096,max_buffered_output_frames:1024,max_bytes:256*1024,..Default::default()})
                .map_err(|e|RecordingError::native("recording track rematrix",e))?));}
        }
        let mut batches=Vec::new();batches.try_reserve_exact(selected.bits().count_ones()as usize)
            .map_err(|_|RecordingError::invalid("recording track conversion allocation"))?;
        for number in 1..=6{let track=skylive_core::AudioTrackId::new(number).map_err(|_|RecordingError::invalid("recording track ID"))?;
            if !selected.contains(track){continue;}
            let samples=block.track_samples(track).ok_or_else(||RecordingError::invalid("missing recording track PCM"))?;
            let samples=&samples[skip*channels..(skip+count)*channels];
            let converter=&mut self.converters[track.index()].as_mut().expect("admitted track converter").1;
            converter.push(samples).map_err(|e|RecordingError::native("recording track conversion",e))?;
            let mut remaining=count;let mut pcm=Vec::with_capacity(count*usize::from(format.channels));
            while remaining!=0{
                let requested=remaining.min(1024);let mut converted=vec![0.0;requested*usize::from(format.channels)];
                let got=converter.pull(&mut converted).map_err(|e|RecordingError::native("recording track conversion output",e))?;
                if got!=requested{return Err(RecordingError::invalid("same-rate recording track conversion changed its sample grid"));}
                pcm.extend(converted.iter().map(|sample|{let value=sample.clamp(-1.0,1.0);(value*if value<0.0{32768.0}else{32767.0}).round()as i16}));
                remaining-=got;
            }
            batches.push((track,pcm));
        }
        // Native/container failure after a partial dispatch is a real output
        // fault; paired writers reject incomplete final track prefixes.
        for (track,pcm) in batches{self.writer.audio_track(track,&pcm,first)?;}
        self.status.audio_gap_frames=self.status.audio_gap_frames.checked_add(first-self.next_audio)
            .ok_or_else(||RecordingError::invalid("recording audio gap counter overflow"))?;
        self.next_audio=first.checked_add(count as u64).ok_or_else(||RecordingError::invalid("recording audio grid overflow"))?;
        self.status.audio_frames=self.status.audio_frames.checked_add(count as u64)
            .ok_or_else(||RecordingError::invalid("recording audio frame counter overflow"))?;
        Ok(())
    }
    fn finish(&mut self) -> Result<RecordingEvent, RecordingError> {
        if self.status.video_frames == 0 {
            return Err(RecordingError {
                code: "recording_empty",
                detail: "No complete Program video frame was accepted".into(),
                recovery_path: Some(self.file.temporary.clone()),
            });
        }
        self.writer.finalize().map_err(|mut error| {
            error.recovery_path = Some(self.file.temporary.clone());
            error
        })?;
        self.file.publish_with_limit(self.writer.container_packet_limit())?;
        Ok(RecordingEvent::Finished {
            path: self.file.final_path.clone(),
            video_frames: self.status.video_frames,
            audio_frames: self.status.audio_frames,
        })
    }
}

struct OwnedFile {
    temporary: PathBuf,
    directory: PathBuf,
    final_path: PathBuf,
}
impl OwnedFile {
    fn prepare(path: &Path) -> Result<Self, RecordingError> {
        let absolute = std::path::absolute(path)
            .map_err(|error| RecordingError::io("resolve recording destination", path, error))?;
        let path = absolute.as_path();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|e| RecordingError::io("create recording directory", parent, e))?;
        if path
            .try_exists()
            .map_err(|e| RecordingError::io("inspect recording destination", path, e))?
        {
            return Err(RecordingError::io(
                "reserve recording destination",
                path,
                "file already exists",
            ));
        }
        // AVAssetWriter requires a nonexistent output URL. Reserve its private
        // parent atomically instead of creating/unlinking a file before open.
        // Both native adapters exclusively create the nested output themselves.
        for _ in 0..32 {
            let nonce = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let directory =
                parent.join(format!(".skylive-recording-{}-{nonce}", std::process::id()));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    return Ok(Self {
                        temporary: directory.join(if path.extension().and_then(|s|s.to_str()).is_some_and(|s|s.eq_ignore_ascii_case("mkv")){"recording.part.mkv"}else{"recording.part.mp4"}),
                        directory,
                        final_path: path.to_owned(),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(RecordingError::io(
                        "reserve recording directory",
                        &directory,
                        error,
                    ));
                }
            }
        }
        Err(RecordingError::invalid(
            "recording temporary-file collision limit",
        ))
    }
    fn publish(&self) -> Result<(), RecordingError> {self.publish_with_limit(20_000_000)}
    fn publish_with_limit(&self,max_packets:u64) -> Result<(), RecordingError> {
        if self.temporary.extension().is_some_and(|s|s.eq_ignore_ascii_case("mkv")){
            let mut file=OpenOptions::new().read(true).write(true).open(&self.temporary).map_err(|e|RecordingError::io("open finalized Matroska recording",&self.temporary,e))?;
            skylive_mp4::matroska::verify(&mut file,max_packets).map_err(|e|RecordingError::native("verify finalized Matroska recording",e))?;
            file.sync_all().map_err(|e|RecordingError::io("flush finalized Matroska recording",&self.temporary,e))?;
        }else{verify_mp4(&self.temporary)?;}
        // Windows uses a same-volume move without replacement; other targets
        // retain the hard-link publication policy. Failure preserves recovery.
        let publication=crate::private_output::publish(&self.temporary, &self.final_path).map_err(|error| RecordingError {
            recovery_path: Some(self.temporary.clone()),
            ..RecordingError::io(
                "publish recording without replacement",
                &self.final_path,
                error,
            )
        })?;
        publication.remove_temporary(&self.temporary).map_err(|error| RecordingError {
            code: "recording_after_publication",
            detail: format!(
                "Recording saved at{}; temporary link cleanup failed: {error}",
                self.final_path.display()
            ),
            recovery_path: Some(self.temporary.clone()),
        })?;
        fs::remove_dir(&self.directory).map_err(|error| RecordingError {
            code: "recording_after_publication",
            detail: format!(
                "Recording saved at{}; empty temporary directory cleanup failed: {error}",
                self.final_path.display()
            ),
            recovery_path: Some(self.directory.clone()),
        })?;
        Ok(())
    }
}
fn verify_mp4(path: &Path) -> Result<(), RecordingError> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| RecordingError::io("open finalized recording", path, e))?;
    let length = file
        .metadata()
        .map_err(|e| RecordingError::io("inspect finalized recording", path, e))?
        .len();
    if length > 64 * 1024 * 1024 * 1024 {
        return Err(RecordingError::invalid(
            "recording exceeds64GiB publication profile",
        ));
    }
    let (mut at, mut boxes, mut ftyp, mut moov, mut mdat) = (0u64, 0usize, false, false, false);
    while at < length {
        boxes += 1;
        if boxes > 65536 || length - at < 8 {
            return Err(RecordingError::invalid(
                "invalid finalized MP4 top-level layout",
            ));
        }
        let mut header = [0u8; 8];
        file.read_exact(&mut header)
            .map_err(|e| RecordingError::io("read MP4 header", path, e))?;
        let mut size = u64::from(u32::from_be_bytes(
            header[..4].try_into().expect("four byte size"),
        ));
        let mut header_size = 8;
        if size == 1 {
            let mut extended = [0; 8];
            file.read_exact(&mut extended)
                .map_err(|e| RecordingError::io("read MP4 extended size", path, e))?;
            size = u64::from_be_bytes(extended);
            header_size = 16;
        } else if size == 0 {
            size = length - at;
        }
        if size < header_size || size > length - at {
            return Err(RecordingError::invalid("invalid finalized MP4 box extent"));
        }
        match &header[4..] {
            b"ftyp" => ftyp = true,
            b"moov" => moov = true,
            b"mdat" => mdat = true,
            _ => {}
        }
        at += size;
        file.seek(SeekFrom::Start(at))
            .map_err(|e| RecordingError::io("seek MP4 box", path, e))?;
    }
    if !ftyp || !moov || !mdat {
        return Err(RecordingError::invalid(
            "recording finalization did not produce ftyp/moov/mdat",
        ));
    }
    file.sync_all()
        .map_err(|e| RecordingError::io("flush finalized recording", path, e))?;
    Ok(())
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
