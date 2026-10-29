//! Child of media_source::owner. All PCM ownership/destruction stays here.
use super::super::control::{
    MediaAppliedTime, MediaBarrier, MediaBarrierReceipt, MediaControl, MediaControlTarget,
    MediaTransportStatus,
};
use super::*;
use std::collections::VecDeque;
pub(super) struct Paused {
    at: u64,
    future: VecDeque<Replay>,
}
pub(super) struct Replay {
    packet: Arc<MediaPcmPacket>,
    cut: u64,
    delta: u64,
    grid: Option<GridCut>,
}
#[derive(Clone, Copy)]
struct GridCut {
    frame: u64,
    rate: u32,
    shift_ns: u64,
}
#[derive(Clone, Copy)]
pub(super) struct AudioCursor {
    pub program: ProgramCaptureKey,
    pub generation: u64,
    pub sequence: u64,
    pub suspended: bool,
}
pub(super) struct AudioSync {
    cursor: AudioCursor,
    receipt: mpsc::Receiver<Result<MediaBarrierReceipt, crate::audio_program::ProgramAudioError>>,
}
pub(super) struct Pending {
    pub(super) internal_buffer: bool,
    key: MediaRequestKey,
    target: MediaControlTarget,
    operation: MediaControl,
    suspend:bool,
    at: u64,
    requested_ns: u64,
    generation: u64,
    paused: Option<Paused>,
    seek: Option<crate::mediafile::PreparedSeek>,
    receipt: Option<
        mpsc::Receiver<Result<MediaBarrierReceipt, crate::audio_program::ProgramAudioError>>,
    >,
    program: Option<ProgramCaptureKey>,
}
impl SourceState {
    pub(super) fn trim_replay_before(&mut self, now: u64) {
        self.replay
            .retain(|r| packet_end(&r.packet) + i128::from(r.delta) > i128::from(now));
    }
    pub(super) fn transport_status(&self) -> MediaTransportStatus {
        MediaTransportStatus {
            state: if self.problem.is_some() {
                MediaPlaybackState::Failed
            } else if self.paused.is_some() {
                MediaPlaybackState::Paused
            } else if self.running {
                MediaPlaybackState::Playing
            } else if self.ended {
                MediaPlaybackState::Ended
            } else {
                MediaPlaybackState::Stopped
            },
            stream_generation: self.stream_generation,
            ready: matches!(self.buffering,Buffering::None|Buffering::Paused) && self.audio_sync.is_none() && (!self.audio_blocked || self.problem.is_some()),
            last_control: self.last_control,
        }
    }
}
impl Owner {
    pub(super) fn record_control_late(&mut self, frames: u64) {
        match self.control_late_frames.checked_add(frames) {
            Some(total) => self.control_late_frames = total,
            None => {
                self.control_late_frames = u64::MAX;
                self.control_late_frames_saturated = true;
            }
        }
    }
    pub(super) fn source_pending(&self, id: Id) -> bool {
        self.control.as_ref().is_some_and(|p| p.target.source == id)
    }
    pub(super) fn begin_source_hotkey(&mut self,key:MediaRequestKey,target:MediaControlTarget,operation:skylive_core::SourcePlaybackAction,callback:crate::global_hotkeys::Callback,now:MediaTime,sent:Instant,gate:Option<&ControlGate>)->(Result<Option<MediaTransportStatus>,MediaSourceError>,Option<MediaControl>){
        use skylive_core::{SourcePlaybackAction as Hotkey,HotkeyAction};
        let mut admitted_control=None;
        let result=(||{
            if !callback.pending(){return Err(MediaSourceError::ControlRetired("shortcut callback was retired before media admission"));}
            if self.control.is_some(){return Err(MediaSourceError::Busy);}
            if key.instance!=self.instance||key.session!=self.key.session||key.epoch!=self.key.epoch||key.request<=self.key.request||target.instance!=self.instance||target.session!=key.session||target.epoch!=key.epoch||target.revision!=key.revision{return Err(MediaSourceError::Invalid("stale media control scope"));}
            let state=self.states.get(&target.source).filter(|state|state.media==target.media&&state.stream_generation==target.status.stream_generation).ok_or(MediaSourceError::Invalid("stale or absent media control source"))?;
            if state.pending_scope_seek.is_some()||state.audio_sync.is_some()||state.audio_blocked&&state.problem.is_none(){return Err(MediaSourceError::Busy);}
            let status=state.transport_status();
            if !state.showing {callback.reject();self.key=key;return Ok(Some(status));}
            let resolved=if operation==Hotkey::PlayPause {if status.state==MediaPlaybackState::Playing{Hotkey::Pause}else{Hotkey::Play}}else{operation};
            let control=match resolved {
                Hotkey::Play=>match status.state{MediaPlaybackState::Playing=>None,MediaPlaybackState::Paused=>Some(MediaControl::Resume),_=>Some(MediaControl::Restart)},
                Hotkey::Pause=>(status.state==MediaPlaybackState::Playing).then_some(MediaControl::Pause),
                Hotkey::Restart=>Some(MediaControl::Restart),Hotkey::Stop=>Some(MediaControl::Stop),
                Hotkey::PlayPause|Hotkey::Previous|Hotkey::Next=>return Err(MediaSourceError::Invalid("Playback shortcuts require a matching Media Source or Slideshow control.")),
            };
            let Some(control)=control else{callback.reject();self.key=key;return Ok(Some(status));};
            // This atomic receipt is semantic callback acceptance, before codec/
            // audio barrier completion; a later failure is still Controlled Err.
            if !callback.accept_action(HotkeyAction::SourcePlayback{source:target.source,operation:resolved}){return Err(MediaSourceError::ControlRetired("shortcut callback was retired before media admission"));}
            admitted_control=Some(control);
            self.begin_control_guarded(key,target,control,now,sent,gate)
        })();
        if result.is_err(){callback.reject();}(result,admitted_control)
    }
    pub(super) fn begin_control(
        &mut self,
        key: MediaRequestKey,
        target: MediaControlTarget,
        operation: MediaControl,
        now: MediaTime,
        sent: Instant,
    ) -> Result<Option<MediaTransportStatus>, MediaSourceError> {
        self.begin_control_guarded(key,target,operation,now,sent,None)
    }
    pub(super) fn begin_control_guarded(
        &mut self,key:MediaRequestKey,target:MediaControlTarget,operation:MediaControl,
        now:MediaTime,sent:Instant,gate:Option<&ControlGate>,
    ) -> Result<Option<MediaTransportStatus>, MediaSourceError> {
        let elapsed =
            u64::try_from(sent.elapsed().as_nanos()).map_err(|_| MediaSourceError::TimeOverflow)?;
        let at = now
            .0
            .checked_add(elapsed)
            .ok_or(MediaSourceError::TimeOverflow)?
            .max(self.now()?.0);
        let result = self.begin_control_at_guarded(key, target, operation, at,gate,false);
        if let Some(p) = self.control.as_mut().filter(|p| p.key == key) {
            p.requested_ns = now.0;
        }
        result
    }
    pub(super) fn begin_control_at(
        &mut self,
        key: MediaRequestKey,
        target: MediaControlTarget,
        operation: MediaControl,
        at: u64,
    ) -> Result<Option<MediaTransportStatus>, MediaSourceError> {
        self.begin_control_at_guarded(key,target,operation,at,None,false)
    }
    /// Internal buffering reuses the current scope identity and never consumes
    /// a user request number. Its receipt is handled separately by the owner.
    pub(super) fn begin_buffer_control(&mut self,id:Id,operation:MediaControl,at:u64)->Result<Option<MediaTransportStatus>,MediaSourceError>{
        let state=self.states.get(&id).ok_or(MediaSourceError::Invalid("buffering source retired"))?;
        let target=MediaControlTarget {instance:self.instance,session:self.key.session,epoch:self.key.epoch,
            revision:self.key.revision,source:id,media:state.media.clone(),status:state.transport_status()};
        self.begin_control_at_guarded(self.key,target,operation,at,None,true)
    }
    fn begin_control_at_guarded(
        &mut self,key:MediaRequestKey,target:MediaControlTarget,operation:MediaControl,
        at:u64,gate:Option<&ControlGate>,internal_buffer:bool,
    ) -> Result<Option<MediaTransportStatus>, MediaSourceError> {
        if self.control.is_some() {
            return Err(MediaSourceError::Busy);
        }
        // Reload of a genuinely absent current-session reader is an actual
        // worker no-op receipt. Do not construct a hidden state or relabel any
        // retained old-session state/snapshot with the caller's identity.
        if operation==MediaControl::Reload&&key.instance==self.instance&&target.instance==self.instance
            &&target.session==key.session&&target.epoch==key.epoch&&target.revision==key.revision&&key.request>self.key.request
            &&(self.key.session!=key.session||self.key.epoch!=key.epoch||!self.states.contains_key(&target.source)){
            return Ok(Some(MediaTransportStatus{state:MediaPlaybackState::Stopped,stream_generation:0,ready:false,last_control:None}));
        }
        if key.instance != self.instance
            || key.session != self.key.session
            || key.epoch != self.key.epoch
            || (!internal_buffer && key.request <= self.key.request)
            || target.instance != self.instance
            || target.session != key.session
            || target.epoch != key.epoch
            || target.revision != key.revision
        {
            return Err(MediaSourceError::Invalid("stale media control scope"));
        }
        let state = self
            .states
            .get(&target.source)
            .filter(|s| {
                s.media == target.media && s.stream_generation == target.status.stream_generation
            })
            .ok_or(MediaSourceError::Invalid(
                "stale or absent media control source",
            ))?;
        if !internal_buffer && state.buffering != Buffering::None && operation != MediaControl::Stop {return Err(MediaSourceError::Busy);}
        if state.pending_scope_seek.is_some() || state.audio_sync.is_some() || state.audio_blocked && state.problem.is_none() {
            return Err(MediaSourceError::Busy);
        }
        let status = state.transport_status();
        let noop = match operation {
            MediaControl::Pause => status.state == MediaPlaybackState::Paused,
            MediaControl::Resume => status.state == MediaPlaybackState::Playing,
            MediaControl::Stop => state.explicitly_stopped,
            MediaControl::Restart | MediaControl::Reload => false,
        };
        if noop {
            self.key = key;
            return Ok(Some(status));
        }
        if operation == MediaControl::Resume && status.state != MediaPlaybackState::Paused {
            return Err(MediaSourceError::Invalid(
                "Resume requires a paused Media Source; use Restart after Stop or End",
            ));
        }
        if operation == MediaControl::Pause && status.state != MediaPlaybackState::Playing {
            return Err(MediaSourceError::Invalid(
                "Pause requires a playing Media Source",
            ));
        }
        let generation = state
            .stream_generation
            .checked_add(1)
            .ok_or(MediaSourceError::IdentityExhausted)?;
        self.generation
            .checked_add(2)
            .ok_or(MediaSourceError::IdentityExhausted)?;
        if at < state.start_ns {
            return Err(MediaSourceError::Invalid(
                "media control time precedes source origin",
            ));
        }
        if let Some(pause) = &state.paused {
            if state.start_ns > pause.at || pause.future.iter().any(|p| p.delta > pause.at) {
                return Err(MediaSourceError::Invalid("paused source clock invariant"));
            }
            let delta = at
                .checked_sub(pause.at)
                .ok_or(MediaSourceError::TimeOverflow)?;
            state
                .start_ns
                .checked_add(delta)
                .ok_or(MediaSourceError::TimeOverflow)?;
            for replay in &pause.future {
                replay
                    .delta
                    .checked_add(delta)
                    .ok_or(MediaSourceError::TimeOverflow)?;
            }
        }
        let suspend=matches!(operation,MediaControl::Pause|MediaControl::Stop)||(operation==MediaControl::Reload&&(!state.active||state.explicitly_stopped||state.paused.is_some()));
        let paused = if operation == MediaControl::Pause {
            let mut future = VecDeque::new();
            future
                .try_reserve_exact(MAX_PCM_PACKETS)
                .map_err(|_| MediaSourceError::Limit("paused media PCM references"))?;
            for packet in &self.pcm {
                if packet.source == target.source
                    && packet.media == target.media
                    && packet.stream_generation == state.stream_generation
                    && !packet.end
                    && packet.barrier.is_none()
                    && packet_end(packet) > i128::from(at)
                {
                    future.push_back(Replay {
                        packet: packet.clone(),
                        cut: at,
                        delta: 0,
                        grid: None,
                    });
                }
            }
            // A second Pause can arrive before bounded replay completed. Those
            // buffers already belong to the same resident pool; copy only refs.
            for replay in &state.replay {
                if future
                    .iter()
                    .any(|p| Arc::ptr_eq(&p.packet, &replay.packet))
                {
                    continue;
                }
                let cut = at
                    .checked_sub(replay.delta)
                    .ok_or(MediaSourceError::TimeOverflow)?
                    .max(replay.cut);
                if packet_end(&replay.packet) + i128::from(replay.delta) > i128::from(at) {
                    if future.len() == MAX_PCM_PACKETS {
                        return Err(MediaSourceError::Limit("paused media PCM references"));
                    }
                    future.push_back(Replay {
                        packet: replay.packet.clone(),
                        cut,
                        delta: replay.delta,
                        grid: replay.grid,
                    });
                }
            }
            future.make_contiguous().sort_by_key(|p| {
                (
                    p.packet.first_media_ns + i128::from(p.delta),
                    p.packet.sequence,
                )
            });
            Some(Paused { at, future })
        } else {
            None
        };
        // Decoder seek preparation is independent and bounded; committing it is
        // infallible after the audio barrier succeeds. No file IO after commit.
        let seek = if operation == MediaControl::Restart {
            state
                .reader
                .as_ref()
                .map(|r| r.prepare_seek(0))
                .transpose()?
        } else {
            None
        };
        let receipt = if state.audio_active && state.media.has_audio() {
            if let Some(program) = self.program {
                let cursor = state.audio_cursor.filter(|c| c.program == program);
                let (reply, receipt) = mpsc::sync_channel(1);
                let barrier = Arc::new(MediaBarrier {
                    source: target.source,
                    media: state.media.clone(),
                    generation: cursor.map_or(0, |c| c.generation),
                    next_generation: generation,
                    last_sequence: cursor.map_or(0, |c| c.sequence),
                    effective_ns: at,
                    suspend,
                    reply,
                });
                let packet = Arc::new(MediaPcmPacket {
                    program,
                    source: target.source,
                    media: state.media.clone(),
                    stream_generation: state.stream_generation,
                    sequence: state.sequence,
                    first_media_ns: i128::from(at),
                    format: NativeAudioFormat {
                        sample_rate: 0,
                        channels: 0,
                        channel_mask: 0,
                    },
                    samples: Box::new([]),
                    sample_offset: 0,
                    end: false,
                    barrier: Some(barrier),
                });
                let sink = self.sink.get().ok_or(MediaSourceError::Disconnected)?;
                let publish = || sink.send(packet).map_err(|e| match e {
                    mpsc::TrySendError::Full(_) if gate.is_some() => MediaSourceError::BarrierBusy,
                    mpsc::TrySendError::Full(_) => MediaSourceError::Busy,
                    mpsc::TrySendError::Disconnected(_) => MediaSourceError::Disconnected,
                });
                // Cancellation and try_send share the gate lock. A full queue
                // leaves it pending; after a successful send only the actual
                // barrier receipt may settle the command.
                if let Some(gate) = gate {
                    gate.admit(publish).map_err(|_| MediaSourceError::ControlRetired("media control scope changed before audio barrier"))??;
                } else { publish()?; }
                Some(receipt)
            } else {
                None
            }
        } else {
            None
        };
        if receipt.is_none() && gate.is_some_and(|gate|!gate.commit()) {
            return Err(MediaSourceError::ControlRetired("media control scope changed before media commit"));
        }
        self.control = Some(Pending {
            internal_buffer,
            key,
            target,
            operation,
            suspend,
            at,
            requested_ns: at,
            generation,
            paused,
            seek,
            receipt,
            program: self.program,
        });
        Ok(None)
    }
    pub(super) fn poll_control(
        &mut self,
    ) -> Option<(
        MediaRequestKey,
        Id,
        Result<MediaTransportStatus, MediaSourceError>,
    )> {
        let pending = self.control.as_ref()?;
        let receipt = match &pending.receipt {
            None => Ok(None),
            Some(rx) => match rx.try_recv() {
                Ok(Ok(receipt)) => Ok(Some(receipt)),
                Ok(Err(error)) => Err(MediaSourceError::ProgramAudio(error)),
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => Err(MediaSourceError::Disconnected),
            },
        };
        let mut p = self.control.take().unwrap();
        let id = p.target.source;
        if receipt.is_ok()
            && (self.generation > u64::MAX - 2
                || self.states.get(&id).is_none_or(|s| {
                    s.media != p.target.media
                        || s.stream_generation != p.target.status.stream_generation
                }))
        {
            self.states.clear();
            self.anchor = None;
            if !p.internal_buffer {self.key = p.key;}
            self.generation = u64::MAX;
            self.global_problem=Some(MediaProblem{source:id,kind:crate::resource_issue::ResourceIssueKind::Invalid,code:"media_source_control_retired",detail:"Media runtime retired after an applied audio barrier: reserved identity or source invariant was violated".into()});
            return Some((
                p.key,
                id,
                Err(MediaSourceError::ControlRetired(
                    "applied audio barrier; derived Media runtime retired",
                )),
            ));
        }
        let result = receipt.map(|receipt| {
            let visual_generation = self.generation + 1;
            let late = receipt.map_or(0, |r| r.late_frames);
            self.record_control_late(late);
            if let Some(receipt) = receipt {
                p.at = p.at.max(receipt.effective_ns);
                if let Some(pause) = &mut p.paused {
                    pause.at = p.at;
                    for replay in &mut pause.future {
                        replay.grid = Some(GridCut {
                            frame: receipt.effective_frame,
                            rate: receipt.sample_rate,
                            shift_ns: replay.delta,
                        });
                    }
                }
            }
            // The single-owner invariant was checked immediately above; no
            // source mutation or callback occurs between that guard and here.
            let state = self.states.get_mut(&id).unwrap();
            match p.operation {
                MediaControl::Pause => {
                    state.paused = p.paused.take();
                    state.replay.clear();
                    state.running = false;
                    state.explicitly_stopped = false;
                }
                MediaControl::Resume => {
                    state.reload_deferred=false;
                    let mut pause = state.paused.take().unwrap();
                    let delta = p.at - pause.at;
                    state.start_ns += delta;
                    for replay in &mut pause.future {
                        replay.delta += delta;
                    }
                    state.replay = pause.future;
                    state.running = true;
                    state.explicitly_stopped = false;
                    state.eos_sent = false;
                }
                MediaControl::Reload => {
                    // The accepted audio barrier fences old queued PCM. Drop the
                    // old decoder before any new open; no second decoder is held.
                    let paused=state.paused.is_some();
                    state.reader=None;state.frame=None;state.audio_format=None;
                    state.reload_deferred=true;state.start_ns=p.at;state.loop_base=0;
                    state.paused=paused.then(||Paused{at:p.at,future:std::collections::VecDeque::new()});
                    state.replay.clear();state.running=state.active&&!state.explicitly_stopped&&!paused;
                    state.ended=false;state.next_video=None;state.next_audio=None;
                    state.video_end=false;state.audio_end=false;state.eos_sent=false;
                    state.problem=None;state.retry_at=None;
                }
                MediaControl::Restart => {
                    state.reload_deferred=false;
                    if let (Some(reader), Some(seek)) = (&mut state.reader, p.seek.take()) {
                        reader.commit_seek(seek);
                    }
                    state.start_ns = p.at;
                    state.loop_base = 0;
                    state.paused = None;
                    state.replay.clear();
                    state.running = true;
                    state.ended = false;
                    state.explicitly_stopped = false;
                    state.next_video = None;
                    state.next_audio = None;
                    state.video_end = false;
                    state.audio_end = false;
                    state.eos_sent = false;
                    state.problem = None;
                    state.retry_at = None;
                }
                MediaControl::Stop => {
                    state.running = false;
                    state.network_download = None;
                    state.progress_probe_budget = None;
                    state.buffering = Buffering::None;
                    state.ended = false;
                    state.explicitly_stopped = true;
                    state.paused = None;
                    state.replay.clear();
                    state.frame = None;
                    state.next_video = None;
                    state.next_audio = None;
                    state.problem = None;
                    state.retry_at = None;
                    if state.media.close_when_inactive() {
                        state.reader = None;
                    }
                }
            }
            state.stream_generation = p.generation;
            state.sequence = 0;
            state.audio_sync_at = p.at;
            state.last_control = Some(MediaAppliedTime {
                requested_ns: p.requested_ns,
                effective_ns: p.at,
                audio_frame: receipt.map(|r| r.effective_frame),
                audio_sample_rate: receipt.map(|r| r.sample_rate),
                late_frames: late,
            });
            if receipt.is_some() {
                state.audio_cursor = p.program.map(|program| AudioCursor {
                    program,
                    generation: p.generation,
                    sequence: 0,
                    suspended: p.suspend,
                });
            }
            if !p.internal_buffer {self.key = p.key;}
            self.generation = visual_generation;
            state.transport_status()
        });
        Some((p.key, id, result))
    }
    pub(super) fn replay_pcm(
        &mut self,
        id: Id,
        state: &mut SourceState,
        work: &mut usize,
        now: MediaTime,
    ) -> Result<bool, MediaSourceError> {
        let mut source_work = 0;
        while *work > 0 && source_work < 4 {
            if !state.audio_active || self.program.is_none() {
                if state.replay.front().is_some_and(|r| {
                    packet_end(&r.packet) + i128::from(r.delta) > i128::from(now.0)
                }) {
                    return Ok(false);
                }
                if state.replay.pop_front().is_none() {
                    return Ok(true);
                }
                *work -= 1;
                source_work += 1;
                continue;
            }
            let Some(replay) = state.replay.pop_front() else {
                return Ok(true);
            };
            if let Some(index) = self.pcm.iter().position(|p| Arc::ptr_eq(p, &replay.packet)) {
                self.pcm.swap_remove(index);
            }
            let mut packet = match Arc::try_unwrap(replay.packet) {
                Ok(packet) => packet,
                Err(packet) => {
                    self.pcm.push(packet.clone());
                    state.replay.push_front(Replay {
                        packet,
                        cut: replay.cut,
                        delta: replay.delta,
                        grid: replay.grid,
                    });
                    return Ok(false);
                }
            };
            let channels = usize::from(packet.format.channels);
            if channels == 0
                || packet.format.sample_rate == 0
                || packet.sample_offset > packet.samples.len()
            {
                return Err(MediaSourceError::Invalid("paused PCM metadata"));
            }
            let frames = (packet.samples.len() - packet.sample_offset) / channels;
            let trim = u64::try_from((i128::from(replay.cut) - packet.first_media_ns).max(0))
                .map_err(|_| MediaSourceError::TimeOverflow)?;
            let mut skip = usize::try_from(
                (u128::from(trim) * u128::from(packet.format.sample_rate)).div_ceil(1_000_000_000),
            )
            .map_err(|_| MediaSourceError::TimeOverflow)?
            .min(frames);
            if let Some(grid) = replay.grid {
                let shifted = packet
                    .first_media_ns
                    .checked_add(i128::from(grid.shift_ns))
                    .ok_or(MediaSourceError::TimeOverflow)?;
                let numerator = i128::from(grid.frame)
                    .checked_mul(1_000_000_000)
                    .and_then(|v| {
                        shifted
                            .checked_mul(i128::from(grid.rate))
                            .and_then(|s| v.checked_sub(s))
                    })
                    .and_then(|v| v.max(0).checked_mul(i128::from(packet.format.sample_rate)))
                    .ok_or(MediaSourceError::TimeOverflow)?;
                let denominator = 1_000_000_000_i128 * i128::from(grid.rate);
                let exact = usize::try_from(
                    numerator
                        .checked_add(denominator - 1)
                        .ok_or(MediaSourceError::TimeOverflow)?
                        / denominator,
                )
                .map_err(|_| MediaSourceError::TimeOverflow)?
                .min(frames);
                skip = skip.max(exact);
            }
            if skip == frames {
                *work -= 1;
                source_work += 1;
                continue;
            }
            let first = packet.first_media_ns
                + i128::try_from(
                    (skip as u128) * 1_000_000_000 / u128::from(packet.format.sample_rate),
                )
                .map_err(|_| MediaSourceError::TimeOverflow)?
                + i128::from(replay.delta);
            packet.sample_offset += skip * channels;
            packet.first_media_ns = first;
            packet.media = state.media.clone();
            packet.source = id;
            packet.program = self.program.ok_or(MediaSourceError::Invalid(
                "resumed audio has no Program sink",
            ))?;
            packet.stream_generation = state.stream_generation;
            packet.sequence = state
                .sequence
                .checked_add(1)
                .ok_or(MediaSourceError::IdentityExhausted)?;
            let packet = Arc::new(packet);
            match self
                .sink
                .get()
                .ok_or(MediaSourceError::Disconnected)?
                .send(packet.clone())
            {
                Ok(()) => {
                    state.sequence = packet.sequence;
                    state.audio_cursor = Some(AudioCursor {
                        program: packet.program,
                        generation: packet.stream_generation,
                        sequence: packet.sequence,
                        suspended: false,
                    });
                    self.pcm.push(packet);
                    *work -= 1;
                    source_work += 1;
                }
                Err(mpsc::TrySendError::Full(_)) => {
                    self.pcm.push(packet.clone());
                    state.replay.push_front(Replay {
                        packet,
                        cut: 0,
                        delta: 0,
                        grid: None,
                    });
                    return Ok(false);
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(MediaSourceError::Disconnected);
                }
            }
        }
        Ok(state.replay.is_empty())
    }
    /// Reconcile implicit activation/new-Program controls before producing PCM.
    /// At most one bounded acknowledgment is retained per admitted source.
    pub(super) fn sync_source_audio(
        &mut self,
        id: Id,
        state: &mut SourceState,
        now: MediaTime,
    ) -> Result<(), MediaSourceError> {
        if !state.audio_active {
            // Losing the Program route does not stop a VC-only video decoder.
            // Obsolete acknowledgments are bounded and have no canonical state.
            state.audio_sync=None;state.audio_cursor=None;state.audio_blocked=false;
            return Ok(());
        }
        if state
            .retry_at
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            return Ok(());
        }
        if let Some(pending) = state.audio_sync.take() {
            match pending.receipt.try_recv() {
                Ok(Ok(receipt)) => {
                    self.record_control_late(receipt.late_frames);
                    state.audio_cursor = Some(pending.cursor);
                }
                Ok(Err(error)) => return Err(MediaSourceError::ProgramAudio(error)),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(MediaSourceError::Disconnected);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    state.audio_sync = Some(pending);
                    state.audio_blocked = true;
                    return Ok(());
                }
            }
        }
        let Some(program) = self
            .program
            .filter(|_| state.audio_active && state.media.has_audio())
        else {
            state.audio_blocked = false;
            return Ok(());
        };
        let cursor = state.audio_cursor.filter(|c| c.program == program);
        let suspended = state.paused.is_some() || state.explicitly_stopped;
        if cursor
            .is_some_and(|c| c.generation == state.stream_generation && c.suspended == suspended)
            || cursor.is_none() && !suspended
        {
            state.audio_blocked = false;
            return Ok(());
        }
        state.audio_blocked = true;
        let (reply, receipt) = mpsc::sync_channel(1);
        let barrier = Arc::new(MediaBarrier {
            source: id,
            media: state.media.clone(),
            generation: cursor.map_or(0, |c| c.generation),
            next_generation: state.stream_generation,
            last_sequence: cursor.map_or(0, |c| c.sequence),
            effective_ns: state.audio_sync_at.min(now.0),
            suspend: suspended,
            reply,
        });
        let packet = Arc::new(MediaPcmPacket {
            program,
            source: id,
            media: state.media.clone(),
            stream_generation: state.stream_generation,
            sequence: state.sequence,
            first_media_ns: i128::from(now.0),
            format: NativeAudioFormat {
                sample_rate: 0,
                channels: 0,
                channel_mask: 0,
            },
            samples: Box::new([]),
            sample_offset: 0,
            end: false,
            barrier: Some(barrier),
        });
        match self
            .sink
            .get()
            .ok_or(MediaSourceError::Disconnected)?
            .send(packet)
        {
            Ok(()) => {
                state.audio_sync = Some(AudioSync {
                    cursor: AudioCursor {
                        program,
                        generation: state.stream_generation,
                        sequence: 0,
                        suspended,
                    },
                    receipt,
                })
            }
            Err(mpsc::TrySendError::Full(_)) => {} // bounded retry; no PCM overtakes it
            Err(mpsc::TrySendError::Disconnected(_)) => return Err(MediaSourceError::Disconnected),
        }
        Ok(())
    }
}
pub(super) fn packet_end(packet: &MediaPcmPacket) -> i128 {
    if packet.format.sample_rate == 0 || packet.format.channels == 0 {
        return packet.first_media_ns;
    }
    let frames = (packet.samples.len().saturating_sub(packet.sample_offset))
        / usize::from(packet.format.channels);
    packet.first_media_ns + (frames as i128) * 1_000_000_000 / i128::from(packet.format.sample_rate)
}
