use super::*;
use crate::{
    audio_capture::NativeAudioFormat,
    mediafile::{DecodedFileAudio, MediaFileLimits, MediaFileReader},
};
use std::{collections::HashSet, time::Duration};
#[path = "owner_control.rs"]
mod control;
const AUDIO_AHEAD_NS: u64 = 200_000_000;
const MAX_PCM_BYTES: usize = 4 * 1024 * 1024;
const MAX_PCM_PACKETS: usize = 256;
// Open readers are additionally charged by mediafile's process-wide decoder
// budget, shared with prepared seeks and other media owners.
const MAX_VIDEO_STATES: usize = 8;
// Content-Length MP4 uses a bounded growing source; underruns go through the
// same fenced Program audio pause/replay barrier as an explicit pause.
const PROGRESSIVE_ENABLED: bool = true;
type VisualState = Vec<(
    Id,
    Arc<MediaSource>,
    Option<u64>,
    bool,
    bool,
    Option<&'static str>,
    bool,
    bool,
    u64,
    bool,
    bool,
    bool,
)>;
fn insert_binding(
    bindings: &mut HashMap<Id, Arc<MediaSource>>,
    id: Id,
    media: &MediaSource,
) -> Result<(), MediaSourceError> {
    if let Some(existing) = bindings.get(&id) {
        if existing.as_ref() != media {
            return Err(MediaSourceError::Invalid(
                "conflicting retained media source settings",
            ));
        }
        return Ok(());
    }
    if bindings.len() >= MAX_SOURCES {
        return Err(MediaSourceError::Limit(
            "media source catalog exceeds the document source limit",
        ));
    }
    // Admission precedes cloning the potentially16KiB authored path.
    bindings.insert(id, Arc::new(media.clone()));
    Ok(())
}
struct PreparedVideo {
    pts: i64,
    image: Arc<RgbaFrame>,
}
struct StagedNetwork {
    url: String,
    session: u64,
    download: super::network_http::Owner,
    expires: Instant,
}
// These are source-owned obligations, not outstanding decoder jobs. They are
// retried only while the source and its current scope still exist.
#[derive(Clone, Copy)]
enum PendingScopeSeek { Restart, Program }
#[derive(Clone,Copy,PartialEq,Eq)]
enum Buffering { None, NeedPause, Paused, NeedResume }
struct SourceState {
    pending_scope_seek: Option<PendingScopeSeek>,
    reload_deferred:bool,
    audio_format: Option<NativeAudioFormat>,
    media: Arc<MediaSource>,
    reader: Option<MediaFileReader>,
    network_download: Option<super::network_http::Owner>,
    progress_probe_budget: Option<crate::mediafile::MediaReadBudget>,
    buffering: Buffering,
    active: bool,
    audio_active:bool,
    showing:bool,
    running: bool,
    paused: Option<control::Paused>,
    replay: std::collections::VecDeque<control::Replay>,
    explicitly_stopped: bool,
    audio_cursor: Option<control::AudioCursor>,
    audio_sync: Option<control::AudioSync>,
    audio_sync_at: u64,
    audio_blocked: bool,
    last_control: Option<super::control::MediaAppliedTime>,
    ended: bool,
    start_ns: u64,
    loop_base: u64,
    stream_generation: u64,
    sequence: u64,
    frame: Option<MediaFrame>,
    next_video: Option<PreparedVideo>,
    next_audio: Option<DecodedFileAudio>,
    // Cursor into a decoded block. Slow playback feeds bounded portions to
    // Program instead of admitting several seconds of output at once.
    next_audio_offset: usize,
    video_end: bool,
    audio_end: bool,
    eos_sent: bool,
    problem: Option<MediaProblem>,
    retry_at: Option<Instant>,
}
impl SourceState {
    fn new(media: Arc<MediaSource>, active: bool, now: u64) -> Self {
        Self {
            pending_scope_seek: None,
            reload_deferred:false,
            audio_format: None,
            running: active || !media.restart_on_activate(),
            paused: None,
            replay: std::collections::VecDeque::new(),
            explicitly_stopped: false,
            audio_cursor: None,
            audio_sync: None,
            audio_sync_at: now,
            audio_blocked: false,
            last_control: None,
            media,
            reader: None,
            network_download: None,
            progress_probe_budget: None,
            buffering: Buffering::None,
            active,
            audio_active:active,
            showing:false,
            ended: false,
            start_ns: now,
            loop_base: 0,
            stream_generation: 1,
            sequence: 0,
            frame: None,
            next_video: None,
            next_audio: None,
            next_audio_offset: 0,
            video_end: false,
            audio_end: false,
            eos_sent: false,
            problem: None,
            retry_at: None,
        }
    }
    fn restart(&mut self, now: u64) -> Result<(), MediaSourceError> {
        let generation = self
            .stream_generation
            .checked_add(1)
            .ok_or(MediaSourceError::IdentityExhausted)?;
        if let Some(reader) = &mut self.reader {
            reader.seek(0)?;
        }
        // OBS clears the previous output when playback restarts without a
        // preloaded first frame. Keeping the last frame here shows a stale
        // image from the previous scene until decoding catches up.
        if !self.media.clear_on_end() && !self.media.looping() {
            self.frame = None;
        }
        self.reload_deferred=false;
        self.buffering=Buffering::None;
        self.pending_scope_seek = None;
        self.stream_generation = generation;
        self.sequence = 0;
        self.start_ns = now;
        self.loop_base = 0;
        self.running = true;
        self.paused = None;
        self.replay.clear();
        self.explicitly_stopped = false;
        self.audio_sync_at = now;
        self.ended = false;
        self.video_end = false;
        self.audio_end = false;
        self.eos_sent = false;
        self.next_video = None;
        self.next_audio = None;
        self.problem = None;
        self.retry_at = None;
        Ok(())
    }
    fn fail(&mut self, id: Id, error: MediaSourceError) {
        let transient = matches!(&error, MediaSourceError::Busy | MediaSourceError::File(MediaFileError::DecoderBusy(_)));
        let code = error.code();
        let mut detail = error.to_string();
        while detail.len() > 8192 {
            detail.pop();
        }
        self.problem = Some(MediaProblem {
            source: id,
            kind: error.issue_kind(),
            code,
            detail,
        });
        // Busy admission is not an end-of-media event and does not invalidate
        // an existing decoder, held frame, timing or prepared source state.
        if transient {
            self.retry_at = Instant::now().checked_add(Duration::from_secs(1));
            return;
        }
        self.pending_scope_seek = None;
        self.reader = None;
        self.network_download = None;
        self.progress_probe_budget = None;
        self.buffering = Buffering::None;
        self.next_video = None;
        self.next_audio = None;
        if self.media.clear_on_end() {
            self.frame = None;
        }
        self.retry_at = Instant::now().checked_add(Duration::from_secs(if super::network_http::is_url(self.media.path()) {10} else {1}));
    }
    fn duration(&self) -> u64 {
        self.reader
            .as_ref()
            .map(|r| {
                r.info()
                    .video
                    .as_ref()
                    .map_or(0, |v| v.duration_ns)
                    .max(r.info().audio.as_ref().map_or(0, |a| a.duration_ns))
            })
            .unwrap_or(0)
    }
}
struct Owner {
    instance: u64,
    key: MediaRequestKey,
    states: HashMap<Id, SourceState>,
    staged_network: Option<StagedNetwork>,
    auto_control: Option<(Id, super::control::MediaControl)>,
    anchor: Option<(MediaTime, Instant)>,
    program: Option<ProgramCaptureKey>,
    generation: u64,
    sequence: u64,
    residents: Vec<Arc<RgbaFrame>>,
    pcm: Vec<Arc<MediaPcmPacket>>,
    sink: Arc<OnceLock<MediaAudioSink>>,
    global_problem: Option<MediaProblem>,
    control: Option<control::Pending>,
    control_late_frames: u64,
    control_late_frames_saturated: bool,
    video_round_robin_after: Option<Id>,
    prefer_video_round_robin: bool,
}
impl Owner {
    fn now(&self) -> Result<MediaTime, MediaSourceError> {
        let Some((now, anchor)) = self.anchor else {
            return Ok(MediaTime(0));
        };
        let delta = u64::try_from(anchor.elapsed().as_nanos())
            .map_err(|_| MediaSourceError::TimeOverflow)?;
        now.0
            .checked_add(delta)
            .map(MediaTime)
            .ok_or(MediaSourceError::TimeOverflow)
    }
    fn reap(&mut self) {
        self.residents.retain(|f| Arc::strong_count(f) > 1);
        let now = self.now().map_or(u64::MAX, |time| time.0);
        let states = &self.states;
        self.pcm.retain(|p| {
            Arc::strong_count(p) > 1
                || (!p.end
                    && p.barrier.is_none()
                    && states
                        .get(&p.source)
                        .is_some_and(|s| s.running && s.stream_generation == p.stream_generation)
                    && control::packet_end(p) > i128::from(now))
        });
    }
    fn scope(
        &mut self,
        key: MediaRequestKey,
        current: Arc<StudioSnapshot>,
        output: FrameComposition,
        video_views:Vec<crate::transition::FrameScene>,
        sidechain_keys:Vec<Id>,
        now: MediaTime,
        sent: Instant,
        program: Option<ProgramCaptureKey>,
    ) -> Result<(), MediaSourceError> {
        if self.generation >= u64::MAX - 1 {
            return Err(MediaSourceError::IdentityExhausted);
        }
        let before = self.visual_state();
        let mut bindings = HashMap::new();
        let mut active = HashSet::new();
        // The frozen Program owns retained source objects even after authored removal.
        for view in output.scenes() {
            view.validate_session(key.session)
                .map_err(|_| MediaSourceError::Invalid("foreign Program scene"))?;
            for source in view
                .visible_sources(&current)
                .map_err(|_| MediaSourceError::Invalid("media Program scene closure"))?
            {
                if let skylive_core::SourceKind::MediaSource { media } = source.kind() {
                    active.insert(source.id());
                    insert_binding(&mut bindings, source.id(), media)?;
                }
            }
        }
        let mut showing=active.clone();
        let mut audio_active=active.clone();
        for id in sidechain_keys{
            if let Some(source)=current.source(id){if let skylive_core::SourceKind::MediaSource{media}=source.kind(){
                active.insert(id);audio_active.insert(id);insert_binding(&mut bindings,id,media)?;
            }}
        }
        for view in video_views {
            view.validate_session(key.session).map_err(|_|MediaSourceError::Invalid("foreign media video view"))?;
            for source in view.visible_sources(&current).map_err(|_|MediaSourceError::Invalid("media video view closure"))? {
                if let skylive_core::SourceKind::MediaSource {media}=source.kind() {
                    active.insert(source.id());showing.insert(source.id());insert_binding(&mut bindings,source.id(),media)?;
                }
            }
        }
        for source in current.sources() {
            if let skylive_core::SourceKind::MediaSource { media } = source.kind() {
                insert_binding(&mut bindings, source.id(), media)?;
            }
        }
        if bindings.len() > MAX_SOURCES {
            return Err(MediaSourceError::Limit(
                "media source catalog exceeds the document source limit",
            ));
        }
        if self.key.session != key.session || self.key.epoch != key.epoch {
            self.states.clear();
        }
        self.states.retain(|id, _| bindings.contains_key(id));
        for (id, media) in bindings {
            let is_active = active.contains(&id);
            if let Some(state) = self.states.get_mut(&id) {
                let changed_file = state.media.path() != media.path()
                    || state.media.video_dimensions() != media.video_dimensions()
                    || state.media.has_audio() != media.has_audio();
                let became_active = is_active && !state.active;
                let enabled_loop =
                    !state.media.looping() && media.looping() && state.ended && is_active;
                state.media = media;
                if changed_file {
                    let old_generation = state.stream_generation;
                    *state = SourceState::new(state.media.clone(), is_active, now.0);
                    state.stream_generation = old_generation
                        .checked_add(1)
                        .ok_or(MediaSourceError::IdentityExhausted)?;
                } else if became_active && state.media.restart_on_activate() || enabled_loop {
                    if let Err(error) = state.restart(now.0) {
                        if matches!(&error, MediaSourceError::Busy | MediaSourceError::File(MediaFileError::DecoderBusy(_))) {
                            state.pending_scope_seek = Some(PendingScopeSeek::Restart);
                            state.fail(id, error);
                        } else { return Err(error); }
                    }
                } else if !is_active && state.active && state.media.restart_on_activate() {
                    state.pending_scope_seek = None;
                    state.running = false;
                    state.network_download = None;
                    state.progress_probe_budget = None;
                    state.buffering = Buffering::None;
                    state.paused = None;
                    state.replay.clear();
                    if state.media.clear_on_end() {
                        state.frame = None;
                    }
                    if state.media.close_when_inactive() {
                        state.reader = None;
                        state.next_video = None;
                        state.next_audio = None;
                    }
                }
                state.active = is_active;
                state.audio_active=audio_active.contains(&id);state.showing=showing.contains(&id);
                // Refresh binding metadata even when pixels do not change.
                if let Some(frame) = &mut state.frame {
                    frame.media = state.media.clone();
                }
            } else {
                let mut state=SourceState::new(media,is_active,now.0);
                state.audio_active=audio_active.contains(&id);state.showing=showing.contains(&id);
                self.states.insert(id,state);
            }
        }
        // A new Program transport cannot accept old asynchronous packets. Seek audio
        // to the current clip time on replacement; video timeline remains continuous.
        if self.program != program {
            for (&id, state) in self.states.iter_mut() {
                state.audio_cursor = None;
                state.audio_sync = None;
                state.audio_blocked = false;
                state.audio_sync_at = now.0;
                state.trim_replay_before(now.0);
                let held =
                    state.paused.is_some() || !state.replay.is_empty() || state.explicitly_stopped;
                if !held {
                    state.next_audio = None;
                }
                state.eos_sent = false;
                let needs_seek = !held && state.pending_scope_seek.is_none();
                if let Some(reader) = state.reader.as_mut().filter(|_| needs_seek) {
                    let result = reader.seek(
                        now.0.saturating_sub(state.start_ns).saturating_sub(state.loop_base)
                            .saturating_mul(u64::from(state.media.speed_percent())) / 100,
                    );
                    match result {
                        Ok(()) => { state.next_video = None; state.video_end = false; state.audio_end = false; }
                        Err(MediaFileError::DecoderBusy(detail)) => {
                            state.pending_scope_seek = Some(PendingScopeSeek::Program);
                            state.fail(id, MediaSourceError::File(MediaFileError::DecoderBusy(detail)));
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                state.stream_generation = state
                    .stream_generation
                    .checked_add(1)
                    .ok_or(MediaSourceError::IdentityExhausted)?;
                state.sequence = 0;
            }
        }
        self.program = program;
        self.key = key;
        self.anchor = Some((now, sent));
        let had_problem = self.global_problem.take().is_some();
        if had_problem || before != self.visual_state() {
            self.generation = self
                .generation
                .checked_add(1)
                .ok_or(MediaSourceError::IdentityExhausted)?;
        }
        Ok(())
    }
    fn step(&mut self) -> Result<(), MediaSourceError> {
        if self.generation == u64::MAX {
            return Ok(());
        }
        if self.control.is_some() && self.generation >= u64::MAX - 2 {
            return Ok(());
        }
        if self.control.is_none() && self.generation >= u64::MAX - 1 {
            let source = self.states.keys().copied().next().unwrap_or(Id::new(0));
            self.states.clear();
            self.anchor = None;
            self.generation = u64::MAX;
            self.global_problem = Some(MediaProblem {
                source,
                kind: crate::resource_issue::ResourceIssueKind::Invalid,
                code: "media_source_identity_exhausted",
                detail: "Media runtime identity space exhausted; reconstruct the derived owner"
                    .into(),
            });
            return Ok(());
        }
        self.reap();
        let now = self.now()?;
        let mut audio_work = 32usize;
        let mut video_work = 4usize;
        let before = self.visual_state();
        let mut ids = self.states.keys().copied().collect::<Vec<_>>();
        ids.sort_by_key(|id| id.get());
        if let Some(after) = self.video_round_robin_after {
            let start = ids.partition_point(|id| id.get() <= after.get());
            ids.rotate_left(start);
        }
        let mut last_video = None;
        let mut last_audio = None;
        for id in ids {
            if self.source_pending(id) {
                continue;
            }
            let Some(mut state) = self.states.remove(&id) else {
                continue;
            };
            if state.buffering != Buffering::None {
                self.states.insert(id,state);
                continue;
            }
            if PROGRESSIVE_ENABLED && state.running {
                if let Some(reader)=state.reader.as_ref().filter(|reader|reader.is_growing()) {
                    match reader.growing_ready(true,true) {
                        Ok(false)=>{state.buffering=Buffering::NeedPause;self.states.insert(id,state);continue;},
                        Err(error)=>{state.fail(id,MediaSourceError::File(error));self.states.insert(id,state);continue;},
                        Ok(true)=>{},
                    }
                }
            }
            let previous_video_work = video_work;
            let previous_audio_work = audio_work;
            let result = Self::retry_scope_seek(&mut state, now).and_then(|ready| {
                if !ready { return Ok(()); }
                self.sync_source_audio(id, &mut state, now).and_then(|_| {
                self.step_source(id, &mut state, now, &mut audio_work, &mut video_work)
                })
            });
            if video_work < previous_video_work { last_video = Some(id); }
            if audio_work < previous_audio_work { last_audio = Some(id); }
            if let Err(error) = result {
                let pending=matches!(&error,MediaSourceError::File(MediaFileError::Io(error)) if error.kind()==std::io::ErrorKind::WouldBlock)
                    && state.reader.as_ref().is_some_and(MediaFileReader::is_growing);
                if pending {state.buffering=Buffering::NeedPause;} else {state.fail(id,error);}
            }
            self.states.insert(id, state);
        }
        self.video_round_robin_after = if self.prefer_video_round_robin {
            last_video.or(last_audio)
        } else {
            last_audio.or(last_video)
        }.or(self.video_round_robin_after);
        self.prefer_video_round_robin = !self.prefer_video_round_robin;
        if before != self.visual_state() {
            self.generation = self
                .generation
                .checked_add(1)
                .ok_or(MediaSourceError::IdentityExhausted)?;
        }
        Ok(())
    }
    fn service_buffering(&mut self) -> Result<(),MediaSourceError> {
        if !PROGRESSIVE_ENABLED {return Ok(());}
        let mut ids=self.states.keys().copied().collect::<Vec<_>>();ids.sort_by_key(|id|id.get());
        let now=self.now()?;
        for id in &ids {
            let Some(mut state)=self.states.remove(id) else {continue;};
            if state.buffering==Buffering::None {self.states.insert(*id,state);continue;}
            // A route/epoch sync started just before the underrun must settle;
            // otherwise an internal pause would wait forever on audio_blocked.
            if !self.source_pending(*id) && (state.audio_sync.is_some()||state.audio_blocked) {
                if let Err(error)=self.sync_source_audio(*id,&mut state,now) {state.fail(*id,error);}
            }
            if let Some(result)=state.network_download.as_ref().and_then(|download|download.poll()) {
                state.network_download=None;
                if let Err(error)=result {state.fail(*id,MediaSourceError::Io(error));self.states.insert(*id,state);continue;}
            }
            if state.buffering==Buffering::Paused {
                let ready=state.reader.as_ref().map(|reader|reader.growing_ready(true,true));
                match ready {
                    Some(Ok(true))=>state.buffering=Buffering::NeedResume,
                    Some(Ok(false))=>{},
                    Some(Err(error))=>state.fail(*id,MediaSourceError::File(error)),
                    None=>state.buffering=Buffering::None,
                }
            }
            self.states.insert(*id,state);
        }
        if self.control.is_some() || self.auto_control.is_some() {return Ok(());}
        for id in ids {
            let Some(state)=self.states.get(&id) else {continue;};
            let operation=match state.buffering {
                Buffering::NeedPause=>super::control::MediaControl::Pause,
                Buffering::NeedResume=>super::control::MediaControl::Resume,
                _=>continue,
            };
            let at=self.now()?.0;
            match self.begin_buffer_control(id,operation,at) {
                Ok(None)=>self.auto_control=Some((id,operation)),
                Ok(Some(_))=>if let Some(state)=self.states.get_mut(&id) {
                    state.buffering=if operation==super::control::MediaControl::Pause {Buffering::Paused}else{Buffering::None};
                },
                Err(MediaSourceError::Busy|MediaSourceError::BarrierBusy)=>{},
                Err(error)=>if let Some(state)=self.states.get_mut(&id){state.fail(id,error);},
            }
            break;
        }
        Ok(())
    }
    fn finish_buffer_control(&mut self,source:Id,result:Result<super::control::MediaTransportStatus,MediaSourceError>) {
        let Some((id,operation))=self.auto_control.take() else{return;};
        if id!=source {return;}
        let Some(state)=self.states.get_mut(&id) else{return;};
        match result {
            Ok(_) if operation==super::control::MediaControl::Pause && state.buffering==Buffering::NeedPause => state.buffering=Buffering::Paused,
            Ok(_) if operation==super::control::MediaControl::Resume && state.buffering==Buffering::NeedResume => state.buffering=Buffering::None,
            Ok(_) => {},
            Err(error)=>state.fail(id,error),
        }
    }
    fn retry_scope_seek(state: &mut SourceState, now: MediaTime) -> Result<bool, MediaSourceError> {
        let Some(pending) = state.pending_scope_seek else { return Ok(true); };
        if state.retry_at.is_some_and(|retry| Instant::now() < retry) { return Ok(false); }
        match pending {
            PendingScopeSeek::Restart => state.restart(now.0)?,
            PendingScopeSeek::Program => {
                if let Some(reader) = &mut state.reader {
                    reader.seek(now.0.saturating_sub(state.start_ns).saturating_sub(state.loop_base).saturating_mul(u64::from(state.media.speed_percent())) / 100)?;
                }
                state.pending_scope_seek = None;
                state.next_video = None;
                state.video_end = false;
                state.audio_end = false;
                state.problem = None;
                state.retry_at = None;
            }
        }
        Ok(true)
    }
    fn visual_state(&self) -> VisualState {
        let mut rows = self
            .states
            .iter()
            .map(|(id, s)| {
                (
                    *id,
                    s.media.clone(),
                    s.frame.as_ref().map(|f| f.sequence),
                    s.running,
                    s.ended,
                    s.problem.as_ref().map(|p| p.code),
                    s.paused.is_some(),
                    s.explicitly_stopped,
                    s.stream_generation,
                    s.audio_blocked,
                    s.showing,
                    s.buffering!=Buffering::None,
                )
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|r| r.0.get());
        rows
    }
    fn prefetch_video(
        &mut self,
        reader: &mut MediaFileReader,
        next: &mut Option<PreparedVideo>,
        end: &mut bool,
        work: &mut usize,
        source_work: &mut usize,
    ) -> Result<bool, MediaSourceError> {
        if *end || *work == 0 || *source_work >= 2 {
            return Ok(false);
        }
        let Some(picture) = reader.next_frame()? else {
            *end = true;
            return Ok(false);
        };
        let bytes = self
            .residents
            .iter()
            .try_fold(0usize, |sum, f| sum.checked_add(f.byte_len()))
            .ok_or(MediaSourceError::Limit("media resident bytes"))?;
        let expected = (picture.width() as usize)
            .checked_mul(picture.height() as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(MediaSourceError::Limit("media frame size"))?;
        if self.residents.len() >= MAX_RESIDENT_FRAMES
            || bytes
                .checked_add(expected)
                .is_none_or(|n| n > MAX_RESIDENT_BYTES)
        {
            return Err(MediaSourceError::Limit(
                "media readers pin bounded RGBA resident pool",
            ));
        }
        let image = Arc::new(reader.rgba_frame(&picture)?);
        self.residents.push(image.clone());
        *next = Some(PreparedVideo {
            pts: picture.presentation_ns(),
            image,
        });
        *work -= 1;
        *source_work += 1;
        Ok(true)
    }
    fn step_source(
        &mut self,
        id: Id,
        state: &mut SourceState,
        now: MediaTime,
        audio_work: &mut usize,
        video_work: &mut usize,
    ) -> Result<(), MediaSourceError> {
        if state.explicitly_stopped {
            return Ok(());
        }
        if state.reload_deferred{if !state.active||state.paused.is_some(){return Ok(());}state.running=true;}
        if state.retry_at.is_some_and(|retry| Instant::now() < retry) {
            return Ok(());
        }
        if state.reader.is_none()
            && !state.ended
            && (state.running || state.active)
        {
            let network = super::network_http::is_url(state.media.path());
            if !network && !std::path::Path::new(state.media.path()).is_absolute() {
                return Err(MediaSourceError::Invalid(
                    "prepare an absolute local media path before playback",
                ));
            }
            // Authored sources in other scenes do not consume a decoder until
            // they actually start. When a newly active video needs a slot,
            // release an inactive restart-on-activation reader under pressure;
            // its next activation already restarts from the beginning.
            if state.media.video_dimensions().is_some() {
                let open = self.states.values().filter(|other|other.reader.is_some() && other.media.video_dimensions().is_some()).count();
                if open >= MAX_VIDEO_STATES {
                    let idle = self.states.iter().filter(|(_,other)|other.reader.is_some() && other.media.video_dimensions().is_some()
                        && !other.active && !other.audio_active && !other.running && other.paused.is_none()
                        && other.media.restart_on_activate())
                        .map(|(id,_)|*id).min_by_key(|id|id.get());
                    if let Some(id)=idle {
                        if let Some(other)=self.states.get_mut(&id) {
                            other.reader=None;other.next_video=None;other.next_audio=None;
                        }
                    } else {
                        return Err(MediaSourceError::Busy);
                    }
                }
            }
            let reader = if network {
                if state.network_download.is_none() {
                    if PROGRESSIVE_ENABLED && self.staged_network.as_ref().is_some_and(|stage|
                        stage.session==self.key.session && stage.url==state.media.path() && Instant::now()<stage.expires) {
                        state.network_download = self.staged_network.take().map(|stage|stage.download);
                    } else {
                        let request = super::network_http::Request::http_url(state.media.path()).map_err(MediaSourceError::Io)?;
                        state.network_download = Some((if PROGRESSIVE_ENABLED {super::network_http::Owner::start_progressive(request)} else {super::network_http::Owner::start(request)}).map_err(|error|
                            if error.kind() == std::io::ErrorKind::WouldBlock { MediaSourceError::Busy } else { MediaSourceError::Io(error) })?);
                    }
                }
                let growing = if PROGRESSIVE_ENABLED {
                    let bytes=state.network_download.as_ref().and_then(|download|download.progressive());
                    if let Some(bytes)=bytes {
                        let budget=state.progress_probe_budget.get_or_insert_with(||crate::mediafile::MediaReadBudget::new(80*1024*1024));
                        MediaFileReader::open_growing_native_preferred(bytes,state.media.path().into(),MediaFileLimits::default(),budget).ok()
                    } else {None}
                } else {None};
                let reader = if let Some(reader)=growing {reader} else {
                    let Some(result) = state.network_download.as_ref().and_then(|download| download.poll()) else { return Ok(()); };
                    state.network_download = None;
                    let bytes = result.map_err(MediaSourceError::Io)?;
                    MediaFileReader::open_bytes_native_preferred(bytes, state.media.path().into(), MediaFileLimits::default())?
                };
                state.progress_probe_budget=None;
                // Network acquisition has no media timeline; start playback
                // when the fully downloaded object becomes available.
                state.start_ns = now.0;
                state.loop_base = 0;
                reader
            } else {
                MediaFileReader::open_native_preferred(std::path::Path::new(state.media.path()), MediaFileLimits::default())?
            };
            let info = reader.info();
            state.audio_format = info.audio.as_ref().map(|audio| NativeAudioFormat {
                sample_rate: ((u64::from(audio.sample_rate) * u64::from(state.media.speed_percent())) / 100) as u32,
                channels: audio.channels,
                channel_mask: audio.channel_mask,
            });
            let actual = info.video.as_ref().map(|v| (v.width, v.height));
            if actual != state.media.video_dimensions()
                || info.audio.is_some() != state.media.has_audio()
            {
                return Err(MediaSourceError::Invalid(
                    "file metadata changed; prepare and rebind this source",
                ));
            }
            state.reader = Some(reader);
            state.reload_deferred=false;
            state.retry_at = None;
            state.problem = None;
        }
        if PROGRESSIVE_ENABLED && state.running {
            if let Some(reader)=state.reader.as_ref().filter(|reader|reader.is_growing()) {
                if !reader.growing_ready(true,true)? {state.buffering=Buffering::NeedPause;return Ok(());}
            }
        }
        if PROGRESSIVE_ENABLED && state.reader.is_some() {
            if let Some(result)=state.network_download.as_ref().and_then(|download|download.poll()) {
                state.network_download=None;
                // The growing reader retains its own shared bytes; the final
                // transfer Vec is not published to the render/audio paths.
                drop(result.map_err(MediaSourceError::Io)?);
            }
        }
        // Program's converter has a bounded input sample-rate range. Refuse
        // unsupported rates instead of silently misplaying or dropping audio.
        if state.audio_format.as_ref().is_some_and(|f| !(80..=384_000).contains(&f.sample_rate)) {
            return Err(MediaSourceError::Invalid("Media speed produces an unsupported audio sample rate"));
        }
        if !state.running {
            return Ok(());
        }
        let speed = u64::from(state.media.speed_percent());
        let duration = state.duration().saturating_mul(100).saturating_add(speed-1) / speed;
        if duration == 0 {
            return Err(MediaSourceError::Invalid(
                "zero media presentation duration",
            ));
        }
        let elapsed = now.0.saturating_sub(state.start_ns);
        let local_wall = elapsed.saturating_sub(state.loop_base);
        if state.media.looping() && local_wall >= duration {
            // Whole-loop suspension recovery is O(1); ordinary loops keep generation and
            // sequence so the converter does not reset at every seamless boundary.
            let loops = local_wall / duration;
            let next_loop_base = state
                .loop_base
                .checked_add(
                    loops
                        .checked_mul(duration)
                        .ok_or(MediaSourceError::TimeOverflow)?,
                )
                .ok_or(MediaSourceError::TimeOverflow)?;
            if let Some(reader) = &mut state.reader {
                reader.seek(0)?;
            }
            state.loop_base = next_loop_base;
            state.next_video = None;
            state.next_audio = None;
            state.video_end = false;
            state.audio_end = false;
            state.eos_sent = false;
        }
        let local = elapsed.saturating_sub(state.loop_base).saturating_mul(speed) / 100;
        let reader = state
            .reader
            .as_mut()
            .ok_or(MediaSourceError::Invalid("playing source has no reader"))?;
        // Conversion/preload work is charged once, even for a future picture.
        // A known future absence is exposed only after real pixels validated.
        let mut source_video_work = 0;
        loop {
            if state.next_video.is_none()
                && !self.prefetch_video(
                    reader,
                    &mut state.next_video,
                    &mut state.video_end,
                    video_work,
                    &mut source_video_work,
                )?
            {
                break;
            }
            let Some(frame) = state.next_video.as_ref() else {
                break;
            };
            if frame.pts > i64::try_from(local).unwrap_or(i64::MAX) {
                break;
            }
            let frame = state
                .next_video
                .take()
                .ok_or(MediaSourceError::Invalid("prepared video disappeared"))?;
            self.sequence = self
                .sequence
                .checked_add(1)
                .ok_or(MediaSourceError::IdentityExhausted)?;
            state.frame = Some(MediaFrame {
                stream_generation:state.stream_generation,
                media: state.media.clone(),
                image: frame.image,
                sequence: self.sequence,
                requested_at: now,
            });
        }
        if state.audio_blocked {
            return Ok(());
        }
        let before_replay = *audio_work;
        if !state.replay.is_empty() && !self.replay_pcm(id, state, audio_work, now)? {
            return Ok(());
        }
        let reader = state
            .reader
            .as_mut()
            .ok_or(MediaSourceError::Invalid("playing source has no reader"))?;
        let mut source_audio_work = before_replay - *audio_work;
        while *audio_work > 0 && source_audio_work < 4 && !state.audio_end {
            // Video may play without an audio consumer. Do not consume file
            // PCM until an explicitly admitted Program owner can receive it.
            if state.audio_active && state.media.has_audio() && self.program.is_none() {
                break;
            }
            if state.next_audio.is_none() {
                state.next_audio = reader.next_audio()?;
                state.next_audio_offset = 0;
                if state.next_audio.is_none() {
                    state.audio_end = true;
                    break;
                }
            }
            let Some(audio) = state.next_audio.as_ref() else {
                break;
            };
            let offset_ns=(state.next_audio_offset as u128).saturating_mul(1_000_000_000) / u128::from(audio.sample_rate);
            let remaining_pts=i128::from(audio.presentation_ns)+i128::try_from(offset_ns).map_err(|_|MediaSourceError::TimeOverflow)?;
            if remaining_pts
                > i128::from(local.saturating_add(AUDIO_AHEAD_NS.saturating_mul(speed)/100))
            {
                break;
            }
            if !state.audio_active || self.program.is_none() {
                // A non-restarting hidden source continues its clock. Keep the
                // packet overlapping now, rather than discarding lookahead
                // which would introduce a200ms hole on activation.
                let count = audio.samples.len() / usize::from(audio.channels);
                let end = i128::from(audio.presentation_ns)
                    + i128::try_from(
                        (count as u128) * 1_000_000_000 / u128::from(audio.sample_rate),
                    )
                    .map_err(|_| MediaSourceError::TimeOverflow)?;
                if end > i128::from(local) {
                    break;
                }
                state.next_audio = None;
                state.next_audio_offset = 0;
                *audio_work -= 1;
                source_audio_work += 1;
                continue;
            }
            let bytes = self
                .pcm
                .iter()
                .try_fold(0usize, |sum, p| sum.checked_add(p.samples.len() * 4))
                .ok_or(MediaSourceError::Limit("media PCM resident bytes"))?;
            if self.pcm.len() >= MAX_PCM_PACKETS
                || bytes
                    .checked_add(audio.samples.len() * 4)
                    .is_none_or(|n| n > MAX_PCM_BYTES)
            {
                break;
            }
            let sequence = state
                .sequence
                .checked_add(1)
                .ok_or(MediaSourceError::IdentityExhausted)?;
            let Some(program) = self.program else {
                break;
            };
            let audio = state.next_audio.as_ref().ok_or(MediaSourceError::Invalid("media audio packet disappeared"))?;
            let channels=usize::from(audio.channels);
            let total_frames=audio.samples.len()/channels;
            if state.next_audio_offset>=total_frames {return Err(MediaSourceError::Invalid("invalid media audio split offset"));}
            let effective_rate=((u64::from(audio.sample_rate)*speed)/100) as u32;
            // At most 100 ms in presentation time, even at 1% speed.
            let chunk_frames=((effective_rate as usize)/10).clamp(1,4096).min(total_frames-state.next_audio_offset);
            let start=state.next_audio_offset*channels;
            let end=(state.next_audio_offset+chunk_frames)*channels;
            let first = i128::from(state.start_ns)
                + i128::from(state.loop_base)
                + remaining_pts * 100 / i128::from(speed);
            let packet = Arc::new(MediaPcmPacket {
                program,
                source: id,
                media: state.media.clone(),
                stream_generation: state.stream_generation,
                sequence,
                first_media_ns: first,
                format: NativeAudioFormat {
                    sample_rate: effective_rate,
                    channels: audio.channels,
                    channel_mask: audio.channel_mask,
                },
                samples: audio.samples[start..end].to_vec().into_boxed_slice(),
                end: false,
                sample_offset: 0,
                barrier: None,
            });
            let sink = self.sink.get().ok_or(MediaSourceError::Disconnected)?;
            match sink.send(packet.clone()) {
                Ok(()) => {
                    state.sequence = sequence;
                    state.audio_cursor = Some(control::AudioCursor {
                        program: packet.program,
                        generation: packet.stream_generation,
                        sequence,
                        suspended: false,
                    });
                    self.pcm.push(packet);
                    state.next_audio_offset+=chunk_frames;
                    if state.next_audio_offset==total_frames {state.next_audio=None;state.next_audio_offset=0;}
                    *audio_work -= 1;
                    source_audio_work += 1;
                }
                Err(mpsc::TrySendError::Full(_)) => {
                    break;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(MediaSourceError::Disconnected);
                }
            }
        }
        if state.audio_end
            && !state.media.looping()
            && state.audio_active
            && !state.eos_sent
            && state.sequence != 0
            && self.pcm.len() < MAX_PCM_PACKETS
        {
            if let Some(program) = self.program {
                let packet = Arc::new(MediaPcmPacket {
                    program,
                    source: id,
                    media: state.media.clone(),
                    stream_generation: state.stream_generation,
                    sequence: state.sequence,
                    first_media_ns: 0,
                    format: NativeAudioFormat {
                        sample_rate: 0,
                        channels: 0,
                        channel_mask: 0,
                    },
                    samples: Box::new([]),
                    end: true,
                    sample_offset: 0,
                    barrier: None,
                });
                if self
                    .sink
                    .get()
                    .is_some_and(|sink| sink.send(packet.clone()).is_ok())
                {
                    self.pcm.push(packet);
                    state.eos_sent = true;
                }
            }
        }
        if !state.media.looping()
            && local >= state.duration()
            && state.video_end
            && (state.audio_end || self.program.is_none() || !state.audio_active)
        {
            state.running = false;
            state.ended = true;
            if state.media.clear_on_end() {
                state.frame = None;
            }
            if state.media.close_when_inactive() {
                state.reader = None;
            }
        }
        Ok(())
    }
    fn snapshot(&self) -> Arc<MediaSnapshot> {
        let now = self.now().map_or(0, |time| time.0);
        let frames = self
            .states
            .iter()
            .filter_map(|(id, s)| s.frame.as_ref().map(|f| (*id, f.clone())))
            .collect();
        let mut problems = self
            .states
            .values()
            .filter_map(|s| s.problem.clone())
            .collect::<Vec<_>>();
        let empty = self
            .states
            .iter()
            .filter(|(_, state)| {
                state.media.video_dimensions().is_some()
                    && state.frame.is_none()
                    && state.problem.is_none()
                    && (!state.running
                        || state.ended
                        || state.next_video.as_ref().is_some_and(|frame| {
                            i128::from(frame.pts)
                                > i128::from(
                                    now.saturating_sub(state.start_ns)
                                        .saturating_sub(state.loop_base)
                                        .saturating_mul(u64::from(state.media.speed_percent())) / 100,
                                )
                        }))
            })
            .map(|(id, state)| (*id, state.media.clone()))
            .collect();
        problems.extend(self.global_problem.clone());
        let states = self
            .states
            .iter()
            .map(|(id, s)| {
                (
                    *id,
                    if s.paused.is_some() {
                        MediaPlaybackState::Paused
                    } else if s.problem.is_some() {
                        MediaPlaybackState::Failed
                    } else if s.running {
                        MediaPlaybackState::Playing
                    } else if s.ended {
                        MediaPlaybackState::Ended
                    } else {
                        MediaPlaybackState::Stopped
                    },
                )
            })
            .collect();
        Arc::new(MediaSnapshot {
            video_decoders:Arc::new(self.states.iter().filter(|(_,s)|s.problem.is_none()).filter_map(|(id,s)|s.reader.as_ref()?.info().video.as_ref().map(|v|(*id,(s.media.clone(),v.decoder)))).collect()),
            audio_formats: Arc::new(
                self.states
                    .iter()
                    .filter(|(_, state)| state.problem.is_none())
                    .filter_map(|(id, state)| {
                        state
                            .audio_format
                            .map(|format| (*id, (state.media.clone(), format)))
                    })
                    .collect(),
            ),
            empty: Arc::new(empty),
            session: self.key.session,
            instance: self.instance,
            epoch: self.key.epoch,
            generation: self.generation,
            frames: Arc::new(frames),
            problems: Arc::new(problems),
            states: Arc::new(states),
            controls: Arc::new(
                self.states
                    .iter()
                    .map(|(id, state)| {
                        (
                            *id,
                            super::control::Entry {
                                media: state.media.clone(),
                                status: state.transport_status(),
                                showing:state.showing,
                            },
                        )
                    })
                    .collect(),
            ),
            control_late_frames: self.control_late_frames,
            control_late_frames_saturated: self.control_late_frames_saturated,
            playing: self.states.values().any(|s| s.running),
        })
    }
}
fn prepare(path: PathBuf) -> Result<PreparedMedia, MediaSourceError> {
    let reader = MediaFileReader::open_native_preferred(&path, MediaFileLimits::default())?;
    prepared_reader(reader)
}
pub(super) fn prepare_with_budget(path:&std::path::Path,budget:&crate::mediafile::MediaReadBudget)->Result<PreparedMedia,MediaSourceError>{
    let reader=MediaFileReader::open_native_preferred_with_budget(path,MediaFileLimits::default(),budget)?;
    prepared_reader(reader)
}
pub(super) fn prepared_reader(mut reader:MediaFileReader)->Result<PreparedMedia,MediaSourceError>{
    let info = reader.info().clone();
    if let Some(frame) = reader.next_frame()? {
        let _validated = reader.rgba_frame(&frame)?;
    }
    let _validated_pcm = reader.next_audio()?;
    let (w, h) = info.video.as_ref().map_or((0, 0), |v| (v.width, v.height));
    let media = MediaSource::new(info.path.clone(), w, h, info.audio.is_some()).map_err(|_| {
        MediaSourceError::Invalid("media dimensions or local path outside supported profile")
    })?;
    reader.ensure_unchanged()?;
    Ok(PreparedMedia { media, info })
}
pub(super) fn run(
    instance: u64,
    jobs: mpsc::Receiver<Job>,
    events: mpsc::SyncSender<MediaEvent>,
    updates: mpsc::SyncSender<Update>,
    sink: Arc<OnceLock<MediaAudioSink>>,
    wake: Arc<OnceLock<Wake>>,
) {
    struct DeferredControl {key:MediaRequestKey,target:MediaControlTarget,operation:MediaControl,now:MediaTime,sent:Instant,gate:Arc<ControlGate>,callback:Option<crate::global_hotkeys::Callback>,retry:Instant}
    let mut deferred:Option<DeferredControl>=None;
    let mut owner = Owner {
        instance,
        key: MediaRequestKey {
            instance,
            session: 0,
            revision: 0,
            epoch: 1,
            request: 0,
        },
        states: HashMap::new(),
        staged_network: None,
        auto_control: None,
        anchor: None,
        program: None,
        generation: 0,
        sequence: 0,
        residents: Vec::new(),
        pcm: Vec::new(),
        sink,
        global_problem: None,
        control: None,
        control_late_frames: 0,
        control_late_frames_saturated: false,
        video_round_robin_after: None,
        prefer_video_round_robin: true,
    };
    let mut published = None;
    let mut pending_network_prepare: Option<(MediaRequestKey, String, super::network_http::Owner, crate::mediafile::MediaReadBudget)> = None;
    loop {
        owner.reap();
        if owner.staged_network.as_ref().is_some_and(|stage|Instant::now()>=stage.expires) {owner.staged_network=None;}
        let timeout = if owner.control.is_some()
            || owner.states.values().any(|s| s.running || s.audio_blocked)
        {
            Duration::from_millis(5)
        } else {
            Duration::from_millis(100)
        };
        let incoming =
            if deferred.is_none() && pending_network_prepare.is_none() && owner.staged_network.is_none() && owner.states.is_empty() && owner.residents.is_empty() && owner.pcm.is_empty() {
                jobs.recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            } else {
                jobs.recv_timeout(timeout)
            };
        match incoming {
            Ok(Job::Stop { key }) => {
                owner.staged_network = None;
                if let Some((pending, _, download, _)) = pending_network_prepare.take() {
                    drop(download);
                    if events.send(MediaEvent::Prepared { key: pending, result: Err(MediaSourceError::Closed) }).is_err() { break; }
                }
                owner.key = key;
                owner.states.clear();
                owner.anchor = None;
                owner.program = None;
                owner.global_problem = None;
                owner.generation = owner.generation.saturating_add(1);
                if events.send(MediaEvent::Updated { key }).is_err() {
                    break;
                }
                if let Some(notify) = wake.get() {
                    notify();
                }
            }
            Ok(Job::Prepare { key, path }) => {
                if let Some(url) = path.to_str().filter(|value| super::network_http::is_url(value)) {
                    let result = super::network_http::Request::http_url(url)
                        .and_then(|request| if PROGRESSIVE_ENABLED {super::network_http::Owner::start_progressive(request)} else {super::network_http::Owner::start(request)});
                    match result {
                        Ok(download) if pending_network_prepare.is_none() => {
                            pending_network_prepare = Some((key, url.to_owned(), download, crate::mediafile::MediaReadBudget::new(80*1024*1024)));
                            continue;
                        }
                        Ok(_) => {
                            if events.send(MediaEvent::Prepared { key, result: Err(MediaSourceError::Busy) }).is_err() { break; }
                        }
                        Err(error) => {
                            let error = if error.kind() == std::io::ErrorKind::WouldBlock { MediaSourceError::Busy } else { MediaSourceError::Io(error) };
                            if events.send(MediaEvent::Prepared { key, result: Err(error) }).is_err() { break; }
                        }
                    }
                    if let Some(notify) = wake.get() { notify(); }
                    continue;
                }
                if events
                    .send(MediaEvent::Prepared {
                        key,
                        result: prepare(path),
                    })
                    .is_err()
                {
                    break;
                }
                if let Some(notify) = wake.get() {
                    notify();
                }
            }
            Ok(Job::Scope {
                key,
                current,
                output,
                video_views,
                sidechain_keys,
                now,
                sent,
                program,
            }) => {
                let logical = output.primary_scene();
                let old_scope = (owner.key.session, owner.key.epoch);
                if let Err(error) = owner.scope(key, current, output, video_views, sidechain_keys, now, sent, program) {
                    owner.key = key;
                    if old_scope != (key.session, key.epoch)
                        || matches!(&error, MediaSourceError::IdentityExhausted)
                    {
                        owner.states.clear();
                    }
                    owner.global_problem = Some(MediaProblem {
                        source: logical,
                        kind: error.issue_kind(),
                        code: error.code(),
                        detail: error.to_string(),
                    });
                    owner.generation = owner.generation.saturating_add(1);
                }
                if events.send(MediaEvent::Updated { key }).is_err() {
                    break;
                }
                if let Some(notify) = wake.get() {
                    notify();
                }
            }
            Ok(Job::Control {
                key,
                target,
                request,
                now,
                sent,
                gate,
            }) => {
                let source = target.source();
                let callback=match &request {ControlRequest::SourceHotkey{callback,..}=>Some(callback.clone()),_=>None};
                let retained=gate.as_ref().map(|gate|(target.clone(),gate.clone(),callback));
                let (result,admitted)=match request {
                    ControlRequest::Direct(operation)=>(owner.begin_control_guarded(key,target,operation,now,sent,gate.as_deref()),Some(operation)),
                    ControlRequest::SourceHotkey{operation,callback}=>owner.begin_source_hotkey(key,target,operation,callback,now,sent,gate.as_deref()),
                };
                match result {
                    Ok(None) => {}
                    Ok(Some(status)) => {
                        if events
                            .send(MediaEvent::Controlled {
                                key,
                                source,
                                result: Ok(status),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(MediaSourceError::File(MediaFileError::DecoderBusy(_))) if retained.is_some() && admitted==Some(MediaControl::Restart) => {
                        let (target,gate,callback)=retained.unwrap();
                        deferred=Some(DeferredControl {key,target,operation:MediaControl::Restart,now,sent,gate,callback,retry:Instant::now()+Duration::from_secs(1)});
                    }
                    Err(MediaSourceError::BarrierBusy) if retained.is_some() && admitted.is_some() => {
                        let (target,gate,callback)=retained.unwrap();
                        deferred=Some(DeferredControl {key,target,operation:admitted.unwrap(),now,sent,gate,callback,retry:Instant::now()+Duration::from_millis(250)});
                    }
                    Err(error) => {
                        if events
                            .send(MediaEvent::Controlled {
                                key,
                                source,
                                result: Err(error),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some((key, url, download, budget)) = pending_network_prepare.take() {
            let progressive = if PROGRESSIVE_ENABLED {
                download.progressive().and_then(|bytes|
                    MediaFileReader::open_growing_native_preferred(bytes,url.clone(),MediaFileLimits::default(),&budget).ok())
                    .and_then(|reader|prepared_reader(reader).ok())
            } else {None};
            if let Some(prepared) = progressive {
                owner.staged_network = Some(StagedNetwork {url,session:key.session,download,expires:Instant::now()+Duration::from_secs(10)});
                if events.send(MediaEvent::Prepared {key,result:Ok(prepared)}).is_err(){break;}
                if let Some(notify)=wake.get(){notify();}
            } else if let Some(result) = download.poll() {
                let result = result.map_err(MediaSourceError::Io).and_then(|bytes|
                    MediaFileReader::open_bytes_native_preferred(bytes, url, MediaFileLimits::default())
                        .map_err(MediaSourceError::File).and_then(prepared_reader));
                if events.send(MediaEvent::Prepared { key, result }).is_err() { break; }
                if let Some(notify) = wake.get() { notify(); }
            } else {
                pending_network_prepare = Some((key, url, download, budget));
            }
        }
        if let Some(pending)=deferred.take() {
            if pending.callback.as_ref().is_some_and(|callback|!callback.is_current()) {pending.gate.cancel();}
            if pending.gate.cancelled() {
                if events.send(MediaEvent::Controlled {key:pending.key,source:pending.target.source(),result:Err(MediaSourceError::ControlRetired("media scope changed before deferred control"))}).is_err(){break;}
                if let Some(notify)=wake.get(){notify();}
            } else if Instant::now() < pending.retry {
                deferred=Some(pending);
            } else {
                let result=owner.begin_control_guarded(pending.key,pending.target.clone(),pending.operation,pending.now,pending.sent,Some(&pending.gate));
                match result {
                    Err(MediaSourceError::File(MediaFileError::DecoderBusy(_))) if pending.operation==MediaControl::Restart=>deferred=Some(DeferredControl {retry:Instant::now()+Duration::from_secs(1),..pending}),
                    Err(MediaSourceError::BarrierBusy)=>deferred=Some(DeferredControl {retry:Instant::now()+Duration::from_millis(250),..pending}),
                    Ok(None)=>{},
                    other=>{
                        let result=match other {Ok(Some(status))=>Ok(status),Err(error)=>Err(error),Ok(None)=>unreachable!()};
                        if events.send(MediaEvent::Controlled {key:pending.key,source:pending.target.source(),result}).is_err(){break;}
                        if let Some(notify)=wake.get(){notify();}
                    }
                }
            }
        }
        let internal_buffer=owner.control.as_ref().is_some_and(|pending|pending.internal_buffer);
        if let Some((key, source, result)) = owner.poll_control() {
            if internal_buffer {owner.finish_buffer_control(source,result);} else if events
                .send(MediaEvent::Controlled {key,source,result}).is_err() {break;}
            if let Some(notify) = wake.get() {
                notify();
            }
        }
        if let Err(error) = owner.step() {
            let source = owner.states.keys().next().copied().unwrap_or(Id::new(0));
            owner.global_problem = Some(MediaProblem {
                source,
                kind: error.issue_kind(),
                code: error.code(),
                detail: error.to_string(),
            });
            owner.states.clear();
        }
        if let Err(error)=owner.service_buffering() {
            let source=owner.states.keys().next().copied().unwrap_or(Id::new(0));
            owner.global_problem=Some(MediaProblem {source,kind:error.issue_kind(),code:error.code(),detail:error.to_string()});
            owner.states.clear();
        }
        let identity = (owner.key.session, owner.key.epoch, owner.generation);
        if owner.key.request != 0
            && published != Some(identity)
            && updates
                .try_send(Update {
                    key: owner.key,
                    snapshot: owner.snapshot(),
                })
                .is_ok()
        {
            published = Some(identity);
            if let Some(notify) = wake.get() {
                notify();
            }
        }
    }
    owner.control = None;
    owner.states.clear();
    while !owner.residents.is_empty() || !owner.pcm.is_empty() {
        owner.reap();
        thread::sleep(Duration::from_millis(20));
    }
}
#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
