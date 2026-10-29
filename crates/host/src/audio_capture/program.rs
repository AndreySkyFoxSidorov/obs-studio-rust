//! Continuous Program CPU owner with independent wall/emission clocks and
//! OBS-style fixed/adaptive source buffering. No native calls, callbacks, UI
//! work or blocking sends occur.
//! One owner retains the final reference to every published PCM allocation;
//! retirement drops DSP immediately and keeps only the bounded reader pool.
//! Beyond 45 overdue blocks, suspension recovery skips unrendered history to
//! leave four due callbacks. Skipped audio is never reported as rendered.

use super::{AudioCaptureBinding, AudioCaptureError, CapturedAudioPacket};
use crate::audio::AudioRouteSnapshot;
use crate::audio_program::{
    AudioProgram, ProgramAudioBlock, ProgramAudioError, ProgramAudioLimits,
};
use serde::Serialize;
use skylive_core::{AudioSettings,Id};
use skylive_media::MediaTime;
use std::{collections::{HashSet, VecDeque}, mem::size_of};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const BLOCK: u64 = 1024;
const MAX_CATCHUP_BLOCKS: usize = 4;
const SUSPENSION_BLOCKS: u64 = 45;
const MAX_RESIDENT_BLOCKS: usize = 256;
const MAX_RESIDENT_BYTES: usize = crate::audio_program::PROGRAM_RESIDENT_BYTES;
const MAX_PROBLEM_BYTES: usize = 8192;

#[path = "buffering.rs"]
mod buffering;
use buffering::BufferClock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ProgramCaptureKey {
    pub session: u64,
    pub clock_epoch: u64,
    pub generation: u64,
    pub settings: AudioSettings,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProgramTransportProblem {
    pub code: &'static str,
    pub detail: String,
}
/// Estimated peaks for PCM submitted to an output queue, per selected track.
/// This is before encoder acceptance and recording pause trimming.
#[derive(Clone, Debug, Serialize)]
pub struct SubmittedOutputPeak {
    pub track: u8,
    pub revision: u64,
    pub start_frame: u64,
    pub frames: usize,
    pub channels: usize,
    pub estimated_peaks: [f64; 8],
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct ProgramTransportStats {
    pub recording_submitted_true_peaks: Vec<SubmittedOutputPeak>,
    pub streaming_submitted_true_peaks: Vec<SubmittedOutputPeak>,
    pub monitor:crate::audio_monitor::MonitorStatus,
    pub running: bool,
    pub input_packets: u64,
    pub input_frames: u64,
    pub input_errors: u64,
    /// Successfully rendered blocks, including explicitly dropped publication.
    pub output_blocks: u64,
    pub dropped_blocks: u64,
    pub dropped_frames: u64,
    /// Whole overdue blocks bypassed without running the PCM mixer.
    pub skipped_blocks: u64,
    pub skipped_frames: u64,
    pub skipped_wall_ticks: u64,
    /// Converted project frames arriving behind the emitted sample cursor.
    pub late_frames: u64,
    pub buffer_blocks: u32,
    pub max_buffer_blocks: u32,
    pub waiting_blocks: u32,
    pub wall_ticks: u64,
    pub suppressed_blocks: u64,
    pub buffering_growths: u64,
    pub reset_inputs: u64,
    pub stopped_inputs: u64,
    pub discarded_source_frames: u64,
    pub pending_inputs: u32,
    pub problem: Option<ProgramTransportProblem>,
}
#[derive(Debug, Serialize)]
pub struct ProgramCaptureOutput {
    pub key: ProgramCaptureKey,
    pub block: Arc<ProgramAudioBlock>,
    pub stats: ProgramTransportStats,
}

// Keep only bounded block identities while recording waits for the first
// accepted video receipt. PCM and route history remain in AudioProgram's
// separately budgeted recording lease, not in this queue.
#[derive(Clone, Copy)]
struct RecordingStamp {
    instance:u64, epoch:u64, session:u64, data_generation:u64, routing_serial:u64,
    settings:AudioSettings, start_frame:u64, frames:usize, revision:Option<u64>,
}
impl RecordingStamp {
    fn from_block(block:&ProgramAudioBlock)->Self {
        Self {instance:block.instance,epoch:block.epoch,session:block.session,
            data_generation:block.data_generation,routing_serial:block.routing_serial,
            settings:block.settings,start_frame:block.start_frame,frames:block.frames,
            revision:block.source_meter_revision}
    }
    fn finish(self,mix:crate::audio_program::RecordingAudioMix)->ProgramAudioBlock {
        ProgramAudioBlock {instance:self.instance,epoch:self.epoch,session:self.session,
            data_generation:self.data_generation,routing_serial:self.routing_serial,
            settings:self.settings,start_frame:self.start_frame,frames:self.frames,
            samples:mix.samples,tracks:mix.tracks,program_meters:Vec::new(),source_meters:Vec::new(),
            source_meter_revision:None,clipped_samples:Vec::new(),gaps:Vec::new()}
    }
}
struct RecordingReader {
    recording:u64,
    peak_meters: [crate::audio::true_peak::MasterTruePeak; 6],
    pending:VecDeque<RecordingStamp>,
    max_blocks:usize,
    // The first receipt anchors warmup. Later receipts must not move the
    // beginning of video forward while old blocks are still being drained.
    alignments:Vec<crate::audio_capture::RecordingCameraAlignment>,
    /// Sources whose old proof retired; only these may be silent while the
    /// replacement history warms up, leaving every other source intact.
    recovering:Vec<Id>,
}
impl RecordingReader {
    // Candidate state is committed only after the sink accepts the block.
    // Each reader and track owns its own sample-grid interpolation history.
    fn submitted_peaks(&self, stamp: RecordingStamp, mix: &crate::audio_program::RecordingAudioMix,
        selected: skylive_core::SourceAudioTrackMask)
        -> ([crate::audio::true_peak::MasterTruePeak; 6], Vec<SubmittedOutputPeak>) {
        let mut meters = self.peak_meters.clone();
        let mut result = Vec::new();
        let Some(revision) = mix.true_peak_revision else {
            for meter in &mut meters { meter.clear(); }
            return (meters, result);
        };
        let channels = usize::from(stamp.settings.channels());
        for number in 1..=6u8 {
            let track = match skylive_core::AudioTrackId::new(number) { Ok(track) => track, Err(_) => continue };
            if !selected.contains(track) { meters[track.index()].clear(); continue; }
            let samples = if number == 1 { Some(mix.samples.as_slice()) }
                else { mix.tracks.iter().find(|output| output.track == track).map(|output| output.samples.as_slice()) };
            let Some(samples) = samples else { meters[track.index()].clear(); continue; };
            match meters[track.index()].observe((stamp.instance, stamp.epoch), stamp.settings.sample_rate(),
                channels, stamp.start_frame, samples) {
                Ok(estimated_peaks) => result.push(SubmittedOutputPeak {track:number, revision,
                    start_frame:stamp.start_frame, frames:stamp.frames, channels, estimated_peaks}),
                Err(_) => { meters[track.index()].clear(); }
            }
        }
        (meters, result)
    }
    fn fence_retired_ndi(&mut self)->Result<(),AudioCaptureError>{
        for proof in &self.alignments {
            if proof.is_ndi() && !proof.current() && !self.recovering.contains(&proof.source) {
                if self.recovering.len()>=crate::audio::MAX_AUDIO_BLOCK_SOURCES{return Err(AudioCaptureError::Limit("retired NDI history count"));}
                self.recovering.try_reserve(1).map_err(|_|AudioCaptureError::Limit("retired NDI history allocation"))?;
                self.recovering.push(proof.source);
            }
        }
        self.alignments.retain(|proof|!proof.is_ndi()||proof.current());
        Ok(())
    }
}

pub(super) struct LiveProgram {
    monitor_sink:Option<crate::audio_monitor::MonitorSink>,
    key: ProgramCaptureKey,
    program: Option<AudioProgram>,
    anchor: Instant,
    media_anchor: MediaTime,
    initial_cursor: u64,
    clock: BufferClock,
    deadline: Option<Instant>,
    immediate: bool,
    outputs: Option<mpsc::SyncSender<ProgramCaptureOutput>>,
    residents: Vec<Arc<ProgramAudioBlock>>,
    resident_bytes: usize,
    stats: ProgramTransportStats,
    output_sinks: crate::audio_output::ProgramOutputRouter,
    recording_reader: Option<RecordingReader>,
    streaming_reader: Option<RecordingReader>,
    camera_activation:Arc<[crate::camera_activation::SourceEpoch]>,
    embedded_discontinuities: HashSet<Id>,
}
impl LiveProgram {
    pub(super) fn push_audio_tail(&mut self,session:u64,source:Id,epoch:skylive_core::SourceDefinitionEpoch,action:skylive_core::SourceAudioAction,until:Instant)->Result<(),AudioCaptureError>{
        if session!=self.key.session||until<=self.anchor{return Ok(());}
        let until=self.media_now(until)?;
        if let Some(program)=&mut self.program{program.push_audio_tail(session,source,epoch,action,until).map_err(control_error)?;}Ok(())
    }
    pub(super) fn push_audio_edge(&mut self,session:u64,source:Id,epoch:skylive_core::SourceDefinitionEpoch,action:skylive_core::SourceAudioAction,pressed:bool,observed:Instant,revoked:bool)->Result<(),AudioCaptureError>{
        if session!=self.key.session{return Ok(());}
        let at=if observed<self.anchor{self.media_anchor}else{self.media_now(observed)?};
        if let Some(program)=&mut self.program{program.push_audio_edge(session,source,epoch,action,pressed,at,revoked&&action==skylive_core::SourceAudioAction::PushToTalk).map_err(control_error)?;}
        Ok(())
    }
    pub(super) fn new(
        route: AudioRouteSnapshot,
        clock_epoch: u64,
        generation: u64,
        sent: Instant,
        outputs: mpsc::SyncSender<ProgramCaptureOutput>,
    ) -> Result<Self, AudioCaptureError> {
        if clock_epoch == 0 || generation == 0 {
            return Err(AudioCaptureError::Invalid(
                "zero Program clock epoch or generation",
            ));
        }
        let camera_activation:Arc<[_]>=route.camera_activation().to_vec().into();
        let settings = route.settings();
        let media_anchor = route.at();
        let cursor = u64::try_from(
            (u128::from(media_anchor.0) * u128::from(settings.sample_rate()))
                .div_ceil(1_000_000_000),
        )
        .map_err(|_| AudioCaptureError::TimeOverflow)?;
        let mut clock = BufferClock::new(cursor, settings.sample_rate(), settings.buffering())?;
        let deadline = wall_deadline(
            sent,
            media_anchor,
            cursor,
            clock.wall_ticks(),
            settings.sample_rate(),
        )?;
        let key = ProgramCaptureKey {
            session: route.session(),
            clock_epoch,
            generation,
            settings,
        };
        let mut program = AudioProgram::new_buffered(settings, ProgramAudioLimits::default())
            .map_err(control_error)?;
        program.route_snapshot(route).map_err(control_error)?;
        // Real fanout readers are attached after construction. Meters and the
        // independent monitor do not create an imaginary Program-track lease.
        program.program_output_tracks(skylive_core::SourceAudioTrackMask::NONE);
        program.camera_program_reader(false);
        clock.ensure_sync_lookahead(program.sync_lookahead_frames())?;
        clock.ensure_source_lookahead(program.sync_lookahead_frames(),program.source_processing_lookahead_frames())?;
        let mut residents = Vec::new();
        residents
            .try_reserve_exact(MAX_RESIDENT_BLOCKS)
            .map_err(|_| AudioCaptureError::Limit("Program resident metadata allocation"))?;
        Ok(Self {
            monitor_sink:None,
            camera_activation,
            embedded_discontinuities: HashSet::new(),
            key,
            program: Some(program),
            anchor: sent,
            media_anchor,
            initial_cursor: cursor,
            clock: clock.clone(),
            deadline: Some(deadline),
            immediate: false,
            outputs: Some(outputs),
            residents,
            resident_bytes: 0,
            output_sinks: crate::audio_output::ProgramOutputRouter::default(),
            recording_reader: None,
            streaming_reader: None,
            stats: ProgramTransportStats {
                running: true,
                buffer_blocks: clock.total_ticks(),
                max_buffer_blocks: clock.max_ticks(),
                waiting_blocks: clock.waiting_ticks(),
                ..Default::default()
            },
        })
    }
    pub(super) fn set_monitor_sink(&mut self,sink:crate::audio_monitor::MonitorSink){self.monitor_sink=Some(sink);}
    pub(super) fn key(&self) -> ProgramCaptureKey {
        self.key
    }
    pub(super) fn attach_recording(
        &mut self,
        sink: crate::recording::RecordingAudioSink,
    ) -> Result<(), AudioCaptureError> {
        self.attach_output(crate::audio_output::AudioOutput::Recording(sink))
    }
    fn refresh_output_readers(&mut self){
        let tracks=self.output_sinks.selected_tracks();let readers=!tracks.is_empty();
        if let Some(program)=&mut self.program{program.program_output_tracks(tracks);program.camera_program_reader(readers);}
    }
    pub(super) fn attach_output(&mut self,sink:crate::audio_output::AudioOutput)->Result<(),AudioCaptureError>{
        let output=sink.key();
        if self.output_sinks.has(output.kind) {return Err(AudioCaptureError::Busy);}
        let use_reader=output.kind==crate::audio_output::OutputKind::Recording
            && self.program.as_ref().is_some_and(AudioProgram::needs_recording_delay_reader);
        let use_streaming=output.kind==crate::audio_output::OutputKind::Streaming
            && self.program.as_ref().is_some_and(AudioProgram::needs_streaming_delay_reader);
        let mut prepared=None;
        if use_reader {
            let frames=(self.key.settings.sample_rate() as usize).checked_mul(21)
                .ok_or(AudioCaptureError::Limit("recording history frame overflow"))?;
            let max_blocks=frames.div_ceil(BLOCK as usize).checked_add(2)
                .ok_or(AudioCaptureError::Limit("recording pending block overflow"))?;
            let mut pending=VecDeque::new();
            pending.try_reserve_exact(max_blocks).map_err(|_|AudioCaptureError::Limit("recording pending block allocation"))?;
            let mut alignments=Vec::new();
            alignments.try_reserve_exact(crate::audio::MAX_AUDIO_BLOCK_SOURCES)
                .map_err(|_|AudioCaptureError::Limit("recording camera proof allocation"))?;
            self.program.as_mut().ok_or(AudioCaptureError::Closed)?
                .begin_recording_history(output.id,self.clock.cursor(),frames).map_err(control_error)?;
            prepared=Some(RecordingReader{recording:output.id,peak_meters:std::array::from_fn(|_|Default::default()),pending,max_blocks,alignments,recovering:Vec::new()});
        }
        let mut streaming_prepared=None;
        if use_streaming {
            let frames=(self.key.settings.sample_rate() as usize).checked_mul(21).ok_or(AudioCaptureError::Limit("streaming history frame overflow"))?;
            let max_blocks=frames.div_ceil(BLOCK as usize).checked_add(2).ok_or(AudioCaptureError::Limit("streaming pending block overflow"))?;
            let mut pending=VecDeque::new();pending.try_reserve_exact(max_blocks).map_err(|_|AudioCaptureError::Limit("streaming pending block allocation"))?;
            let mut alignments=Vec::new();alignments.try_reserve_exact(crate::audio::MAX_AUDIO_BLOCK_SOURCES).map_err(|_|AudioCaptureError::Limit("streaming proof allocation"))?;
            self.program.as_mut().ok_or(AudioCaptureError::Closed)?.begin_streaming_history(output.id,self.clock.cursor(),frames).map_err(control_error)?;
            streaming_prepared=Some(RecordingReader{recording:output.id,peak_meters:std::array::from_fn(|_|Default::default()),pending,max_blocks,alignments,recovering:Vec::new()});
        }
        if let Err(error)=self.output_sinks.attach(self.key,sink) {
            if let Some(reader)=prepared {if let Some(program)=&mut self.program {program.stop_recording_history(reader.recording);}}
            if let Some(reader)=streaming_prepared {if let Some(program)=&mut self.program {program.stop_streaming_history(reader.recording);}}
            return Err(match error{
            crate::audio_output::OutputError::DuplicateKind(_)=>AudioCaptureError::Busy,
            error=>AudioCaptureError::Native{operation:"attach Program output",detail:error.to_string()},
            });
        }
        if output.kind==crate::audio_output::OutputKind::Recording {self.recording_reader=prepared;}
        if output.kind==crate::audio_output::OutputKind::Streaming {self.streaming_reader=streaming_prepared;}
        self.refresh_output_readers();Ok(())
    }
    fn take_output(&mut self,kind:crate::audio_output::OutputKind)->Option<crate::audio_output::AudioOutput>{
        if kind==crate::audio_output::OutputKind::Recording && self.recording_reader.is_some() {
            // A new Program owner has no claim on retained PCM or receipts.
            // Stop only this recording rather than migrate an incomplete audio prefix.
            self.fail_delayed_recording();
            return None;
        }
        if kind==crate::audio_output::OutputKind::Streaming && self.streaming_reader.is_some(){self.fail_delayed_streaming();return None;}
        let sink=self.output_sinks.take(kind);self.refresh_output_readers();sink
    }
    fn release_recording_reader(&mut self) {
        self.stats.recording_submitted_true_peaks.clear();
        if let Some(reader)=self.recording_reader.take() {
            if let Some(program)=&mut self.program {program.stop_recording_history(reader.recording);}
        }
    }
    fn fail_delayed_recording(&mut self) {
        self.release_recording_reader();
        self.output_sinks.fail_recording_backpressure();
        self.refresh_output_readers();
    }
    fn release_streaming_reader(&mut self){
        self.stats.streaming_submitted_true_peaks.clear();
        if let Some(reader)=self.streaming_reader.take(){if let Some(program)=&mut self.program{program.stop_streaming_history(reader.recording);}}
    }
    fn fail_delayed_streaming(&mut self){self.release_streaming_reader();self.output_sinks.fail_streaming_backpressure();self.refresh_output_readers();}
    /// Streaming owns its own cursor, input history lease and AVC proofs.
    /// Never use an encoder receipt as proof for recording or another sink.
    fn drain_streaming_reader(&mut self){
        let Some(mut reader)=self.streaming_reader.take() else{return;};
        if !self.output_sinks.has(crate::audio_output::OutputKind::Streaming){
            self.stats.streaming_submitted_true_peaks.clear();
            if let Some(program)=&mut self.program{program.stop_streaming_history(reader.recording);}return;
        }
        let mut failed=false;
        // Reconnection ends the old encoder owner and invalidates its video
        // receipts. Its undelivered PCM prefix cannot be reassigned to the new
        // connection: retire only this streaming reader's prefix, preserving
        // the independent recording lease and all other outputs.
        if reader.alignments.iter().any(|proof|!proof.streaming_connection_current()) {
            if let Some(last)=reader.pending.back() {
                match last.start_frame.checked_add(last.frames as u64) {
                    Some(end)=>if self.program.as_mut().is_none_or(|program|program.advance_streaming_history(reader.recording,end).is_err()){failed=true;},
                    None=>failed=true,
                }
            }
            if !failed {
                reader.pending.clear(); reader.alignments.clear();
                for meter in &mut reader.peak_meters { meter.clear(); }
                self.stats.streaming_submitted_true_peaks.clear();
            }
        }
        if reader.fence_retired_ndi().is_err(){failed=true;}
        let no_pending=reader.pending.is_empty();
        if no_pending {reader.alignments.retain(|proof|proof.current());}
        if let Some(program)=&self.program {
            if no_pending {reader.recovering.retain(|source|program.delay_source_active(*source));}
            if !matches!(program.streaming_history_status(),Some(crate::audio_program::RecordingHistoryStatus::Holding{recording,..}) if recording==reader.recording){failed=true;}
            for proof in program.streaming_alignments(){
                if proof.recording!=reader.recording {continue;}
                if let Some(old)=reader.alignments.iter_mut().find(|old|old.source==proof.source && old.revision==proof.revision){
                    if !old.current(){if no_pending {*old=proof.clone();continue;}failed=true;break;}
                    old.confirmed_until_sample=old.confirmed_until_sample.max(proof.confirmed_until_sample);continue;
                }
                if reader.alignments.len()>=crate::audio::MAX_AUDIO_BLOCK_SOURCES {failed=true;break;}
                reader.alignments.push(proof.clone());
            }
        } else {failed=true;}
        for _ in 0..MAX_CATCHUP_BLOCKS {
            if failed {break;}let Some(stamp)=reader.pending.front().copied() else{break;};
            let mut proofs=Vec::new();if proofs.try_reserve_exact(reader.alignments.len()).is_err(){failed=true;break;}
            proofs.extend(reader.alignments.iter().cloned());
            let Some(tracks)=self.output_sinks.streaming_tracks() else{failed=true;break;};
            let result=self.program.as_ref().expect("streaming Program present")
                .mix_streaming_sources(reader.recording,self.key.clock_epoch,stamp.start_frame,stamp.frames,tracks,&proofs,&reader.recovering);
            let mix=match result{Ok(mix)=>mix,Err(ProgramAudioError::Backpressure(_))=>break,
                Err(ProgramAudioError::Invalid(_)) if reader.alignments.iter().any(|proof|proof.is_ndi()&&!proof.current())=>{
                    if reader.fence_retired_ndi().is_err(){failed=true;}break;
                },Err(_)=>{failed=true;break;}};
            let Some(end)=stamp.start_frame.checked_add(stamp.frames as u64) else{failed=true;break;};
            let (peak_meters, peaks)=reader.submitted_peaks(stamp,&mix,tracks);
            let block=stamp.finish(mix);let bytes=output_charge(&block);self.reap();
            if self.residents.len()>=MAX_RESIDENT_BLOCKS || self.resident_bytes.checked_add(bytes).is_none_or(|n|n>MAX_RESIDENT_BYTES){break;}
            let block=Arc::new(block);self.resident_bytes+=bytes;self.residents.push(block.clone());
            if !self.output_sinks.submit_streaming(self.key,block){failed=true;break;}
            reader.peak_meters=peak_meters;
            self.stats.streaming_submitted_true_peaks=peaks;
            reader.pending.pop_front();
            if self.program.as_mut().expect("streaming Program present").advance_streaming_history(reader.recording,end).is_err(){failed=true;break;}
        }
        self.streaming_reader=Some(reader);if failed{self.fail_delayed_streaming();}
    }
    /// Drain only a bounded number of retained blocks per owner turn. The
    /// recording receipt proves the output's own camera timeline; it must not
    /// be used to alter Program, streaming, replay or NDI audio.
    fn drain_recording_reader(&mut self) {
        let Some(mut reader)=self.recording_reader.take() else{return;};
        if !self.output_sinks.has(crate::audio_output::OutputKind::Recording) {
            self.stats.recording_submitted_true_peaks.clear();
            if let Some(program)=&mut self.program {program.stop_recording_history(reader.recording);}
            return;
        }
        let mut failed=false;
        // Fully paused blocks contain no samples on any output track. Drop
        // them only in queue order; a block crossing the cut still goes to the
        // recording owner for exact trimming. The shared pause lock ensures a
        // concurrent resume cannot put an unpaused sample in this discarded
        // range: its effective resume is advanced past the acknowledged end.
        for _ in 0..MAX_CATCHUP_BLOCKS {
            let Some(stamp)=reader.pending.front().copied() else {break;};
            let Some(end)=stamp.start_frame.checked_add(stamp.frames as u64) else {failed=true;break;};
            if !self.output_sinks.discard_fully_paused_recording(stamp.start_frame,end) {break;}
            if self.program.as_mut().is_none_or(|program|program.advance_recording_history(reader.recording,end).is_err()) {failed=true;break;}
            reader.pending.pop_front();
        }
        if reader.fence_retired_ndi().is_err(){failed=true;}
        let no_pending=reader.pending.is_empty();
        // Once the old prefix is fully retired, forget invalid proofs even if
        // the replacement camera has not produced its first accepted frame.
        // New blocks can then wait for that proof instead of failing merely
        // because a stale proof was retained from the previous stream.
        if no_pending {reader.alignments.retain(|proof|proof.current());}
        if let Some(program)=&self.program {
            if no_pending {reader.recovering.retain(|source|program.delay_source_active(*source));}
            if !matches!(program.recording_history_status(),Some(crate::audio_program::RecordingHistoryStatus::Holding{recording,..}) if recording==reader.recording) {failed=true;}
            for proof in program.recording_camera_alignments() {
                if proof.recording!=reader.recording {continue;}
                if let Some(old)=reader.alignments.iter_mut().find(|old|old.source==proof.source && old.revision==proof.revision) {
                    // A reconnect is a different capture history even if the
                    // authored revision is unchanged. Do not apply its new
                    // proof retroactively to queued blocks from the old epoch.
                    if !old.current(){
                        // A new capture epoch may be adopted only after every
                        // block from the old epoch has been accepted or proven
                        // wholly paused. Otherwise its earlier samples would
                        // be retroactively assigned the new camera clock.
                        if no_pending {*old=proof.clone();continue;}
                        failed=true;break;
                    }
                    old.confirmed_until_sample=old.confirmed_until_sample.max(proof.confirmed_until_sample);
                    continue;
                }
                if reader.alignments.len()>=crate::audio::MAX_AUDIO_BLOCK_SOURCES {failed=true;break;}
                reader.alignments.push(proof.clone());
            }
        } else {failed=true;}
        for _ in 0..MAX_CATCHUP_BLOCKS {
            if failed {break;}
            let Some(stamp)=reader.pending.front().copied() else {break;};
            if !self.output_sinks.recording_may_submit(){break;}
            let mut proofs=Vec::new();
            if proofs.try_reserve_exact(reader.alignments.len()).is_err(){failed=true;break;}
            proofs.extend(reader.alignments.iter().cloned());
            let Some(tracks)=self.output_sinks.recording_tracks() else {failed=true;break;};
            let result=self.program.as_ref().expect("recording Program present").mix_recording_sources(
                reader.recording,self.key.clock_epoch,stamp.start_frame,stamp.frames,tracks,&proofs,&reader.recovering);
            let mix=match result {
                Ok(mix)=>mix,
                Err(ProgramAudioError::Backpressure(_))=>{
                    if self.output_sinks.recording_stop_requested(){failed=true;}
                    break;
                },
                Err(ProgramAudioError::Invalid(_)) if reader.alignments.iter().any(|proof|proof.is_ndi()&&!proof.current())=>{
                    if reader.fence_retired_ndi().is_err(){failed=true;}break;
                }
                Err(_)=>{failed=true;break;}
            };
            let end=match stamp.start_frame.checked_add(stamp.frames as u64){Some(end)=>end,None=>{failed=true;break;}};
            let (peak_meters, peaks)=reader.submitted_peaks(stamp,&mix,tracks);
            let block=stamp.finish(mix);
            if !self.output_sinks.recording_can_submit(&block) {break;}
            let bytes=output_charge(&block);
            self.reap();
            if self.residents.len()>=MAX_RESIDENT_BLOCKS || self.resident_bytes.checked_add(bytes).is_none_or(|n|n>MAX_RESIDENT_BYTES) {break;}
            reader.pending.pop_front();
            let block=Arc::new(block);
            self.resident_bytes+=bytes;self.residents.push(block.clone());
            if !self.output_sinks.submit_recording(self.key,block) {failed=true;break;}
            reader.peak_meters=peak_meters;
            self.stats.recording_submitted_true_peaks=peaks;
            if self.program.as_mut().expect("recording Program present").advance_recording_history(reader.recording,end).is_err() {failed=true;break;}
        }
        self.recording_reader=Some(reader);
        if failed {self.fail_delayed_recording();}
    }
    pub(super) fn take_recording(&mut self)->Option<crate::recording::RecordingAudioSink>{
        match self.take_output(crate::audio_output::OutputKind::Recording){Some(crate::audio_output::AudioOutput::Recording(sink))=>Some(sink),_=>None}
    }
    pub(super) fn take_ndi_output(&mut self)->Option<crate::audio_output::StreamingAudioSink>{
        match self.take_output(crate::audio_output::OutputKind::NdiProgram){Some(crate::audio_output::AudioOutput::NdiProgram(sink))=>Some(sink),_=>None}
    }
    pub(super) fn take_replay(&mut self)->Option<crate::audio_output::StreamingAudioSink>{
        match self.take_output(crate::audio_output::OutputKind::ReplayBuffer){Some(crate::audio_output::AudioOutput::ReplayBuffer(sink))=>Some(sink),_=>None}
    }
    pub(super) fn take_streaming(&mut self)->Option<crate::audio_output::StreamingAudioSink>{
        match self.take_output(crate::audio_output::OutputKind::Streaming){Some(crate::audio_output::AudioOutput::Streaming(sink))=>Some(sink),_=>None}
    }
    #[cfg(test)]
    pub(super) fn has_recording(&self) -> bool {
        self.has_output(crate::audio_output::OutputKind::Recording)
    }
    pub(super) fn has_output(&self, kind: crate::audio_output::OutputKind) -> bool {
        self.output_sinks.has(kind)
    }
    pub(super) fn has_outputs(&self) -> bool {
        self.has_output(crate::audio_output::OutputKind::Recording)
            || self.has_output(crate::audio_output::OutputKind::Streaming)
            || self.has_output(crate::audio_output::OutputKind::ReplayBuffer)
            || self.has_output(crate::audio_output::OutputKind::NdiProgram)
    }
    pub(super) fn stats(&self) -> ProgramTransportStats {
        let mut stats=self.stats.clone();
        if let Some(sink)=&self.monitor_sink {stats.monitor=sink.status();}
        stats
    }
    pub(super) fn is_active(&self) -> bool {
        self.program.is_some()
    }
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.deadline
    }
    pub(super) fn resident_count(&self) -> usize {
        self.residents.len()
    }
    pub(super) fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }
    pub(super) fn update_route(
        &mut self,
        route: AudioRouteSnapshot,
    ) -> Result<(), AudioCaptureError> {
        if route.session() != self.key.session || route.settings() != self.key.settings {
            return Err(AudioCaptureError::Invalid(
                "Program route belongs to another session or audio format",
            ));
        }
        self.camera_activation=route.camera_activation().to_vec().into();
        self.program
            .as_mut()
            .ok_or(AudioCaptureError::Closed)?
            .route_snapshot(route)
            .map_err(control_error)?;
        Ok(())
    }
    pub(super) fn set_camera_activation(&mut self,epochs:Arc<[crate::camera_activation::SourceEpoch]>,route:Option<AudioRouteSnapshot>)->Result<crate::camera_activation::CameraAudioCut,AudioCaptureError>{
        self.camera_activation=epochs.clone();
        let readers=self.has_outputs();
        let program=self.program.as_mut().ok_or(AudioCaptureError::Closed)?;
        program.program_output_tracks(self.output_sinks.selected_tracks());
        program.camera_program_reader(readers);
        let cut=program.set_camera_activation(&epochs).map_err(control_error)?;
        if let Some(route)=route {program.route_snapshot(route.at_least_cut(cut).map_err(|e|AudioCaptureError::Native {operation:"camera AV route cut",detail:e.to_string()})?).map_err(control_error)?;}
        Ok(cut)
    }
    pub(super) fn camera_activation_retired(&self,epochs:&[crate::camera_activation::SourceEpoch],cut:crate::camera_activation::CameraAudioCut)->bool{
        self.program.as_ref().is_none_or(|program|program.camera_activation_retired_tracks(epochs,cut,self.has_outputs(),self.output_sinks.selected_tracks()))
    }
    fn source_capture_enabled(&self,source:Id)->bool{self.camera_activation.iter().find(|entry|entry.source==source).is_none_or(|entry|entry.enabled)}
    pub(super) fn required_bindings(&self) -> Vec<AudioCaptureBinding> {
        let mut bindings = Vec::new();
        if let Some(program) = &self.program {
            bindings.extend_from_slice(program.capture_bindings());
            for required in program.required_bindings() {
                if !self.source_capture_enabled(required.source){continue;}
                if let Some(binding) = required.binding.as_native() {
                    if !bindings.contains(binding) {
                        bindings.push(binding.clone());
                    }
                }
            }
        }
        bindings
    }
    pub(super) fn retained_bindings(&self) -> Vec<AudioCaptureBinding> {
        let mut bindings = Vec::new();
        if let Some(program) = &self.program {
            for required in program.required_bindings() {
                if !self.source_capture_enabled(required.source){continue;}
                if let Some(binding) = required.binding.as_native() {
                    if !bindings.contains(binding) {
                        bindings.push(binding.clone());
                    }
                }
            }
        }
        bindings
    }
    pub(super) fn set_capture_bindings(
        &mut self,
        bindings: Vec<AudioCaptureBinding>,
    ) -> Result<(), AudioCaptureError> {
        self.program
            .as_mut()
            .ok_or(AudioCaptureError::Closed)?
            .set_capture_bindings(bindings)
            .map_err(control_error)
    }
    pub(super) fn media_now(&self, now: Instant) -> Result<MediaTime, AudioCaptureError> {
        if now < self.anchor {
            return Err(AudioCaptureError::Invalid(
                "Program wall clock moved before its anchor",
            ));
        }
        let duration =
            now.checked_duration_since(self.anchor)
                .ok_or(AudioCaptureError::Invalid(
                    "Program wall clock moved before its anchor",
                ))?;
        let elapsed =
            u64::try_from(duration.as_nanos()).map_err(|_| AudioCaptureError::TimeOverflow)?;
        Ok(MediaTime(
            self.media_anchor
                .0
                .checked_add(elapsed)
                .ok_or(AudioCaptureError::TimeOverflow)?,
        ))
    }
    pub(super) fn ingest(&mut self, packet: &CapturedAudioPacket) {
        let Some(program) = &mut self.program else {
            return;
        };
        // Acquisition may also keep Preview-only inputs warm for its meters.
        // Such a packet is not a Program error or Program input occurrence.
        if !program
            .required_bindings()
            .iter()
            .any(|required| required.binding == packet.binding)
            && !program.capture_bindings().contains(&packet.binding)
        {
            return;
        }
        let result = program
            .identity()
            .and_then(|identity| program.ingest_captured(identity, packet));
        match result {
            Ok(report) => {
                if !self.add(Counter::InputPackets, 1)
                    || !self.add(Counter::InputFrames, report.accepted_frames as u64)
                    || !self.add(Counter::LateFrames, report.late_frames as u64)
                {
                    self.retire();
                }
            }
            Err(error) => {
                if !self.add(Counter::InputErrors, 1) {
                    self.retire();
                }
                self.problem(error.code(), error.to_string());
            }
        }
    }
    /// Separate source-scoped ingress for exact observed-window streams. It is
    /// deliberately not called for the existing selector-resolved path.
    pub(super) fn ingest_window(&mut self, evidence:&super::super::window_observation::WindowAudioEvidence,
        packet:&CapturedAudioPacket) {
        if !evidence.current() {return;}
        let observation=evidence.observation();
        let key=crate::audio::AudioInputKey::Window {source:observation.source(),binding:packet.binding.clone()};
        let Some(program)=&mut self.program else{return;};
        if !program.required_bindings().iter().any(|required|required.binding==key) {return;}
        let result=program.identity().and_then(|identity|program.ingest_window_captured(identity,evidence,packet));
        match result {
            Ok(report)=>{if !self.add(Counter::InputPackets,1)||!self.add(Counter::InputFrames,report.accepted_frames as u64)||!self.add(Counter::LateFrames,report.late_frames as u64){self.retire();}},
            Err(error)=>{if !self.add(Counter::InputErrors,1){self.retire();}self.problem(error.code(),error.to_string());}
        }
    }
    pub(super) fn invalidate_window(&mut self,source:Id) {
        if let Some(Err(error))=self.program.as_mut().map(|program|program.invalidate_window_input(source)) {
            self.problem(error.code(),error.to_string());self.retire();
        }
    }
    pub(super) fn ingest_browser(&mut self,packet:&crate::browser::BrowserPcmPacket){
        if packet.program!=self.key{return;}let Some(program)=&mut self.program else{return;};
        let input=crate::audio::AudioInputKey::Browser(packet.key.clone());
        if !program.required_bindings().iter().any(|r|r.binding==input){return;}
        let result=program.identity().and_then(|identity|program.ingest_browser(crate::audio_program::BrowserProgramInputPacket{
            identity,key:&packet.key,stream_generation:packet.stream_generation,sequence:packet.sequence,
            first_media_ns:packet.first_media_ns,received_media_ns:packet.received_media_ns,
            format:packet.format,samples:&packet.samples,flags:packet.flags,
        }));
        match result{Ok(report)=>{if !self.add(Counter::InputPackets,1)||!self.add(Counter::InputFrames,report.accepted_frames as u64)||!self.add(Counter::LateFrames,report.late_frames as u64){self.retire();}},Err(error)=>{if !self.add(Counter::InputErrors,1){self.retire();}self.problem(error.code(),error.to_string());}}
    }
    pub(super) fn ingest_embedded(&mut self, packet: &super::EmbeddedPcmPacket, limits: super::AudioCaptureLimits) {
        if packet.program != self.key || !packet.binding.embedded_audio() { return; }
        let Some(program) = &mut self.program else { return; };
        // Retain only current routes, so failed packets cannot accumulate unbounded state.
        self.embedded_discontinuities.retain(|source| program.required_bindings().iter().any(|required|
            matches!(&required.binding, crate::audio::AudioInputKey::EmbeddedCamera { source: id, .. } if id == source)));
        let input = crate::audio::AudioInputKey::EmbeddedCamera { source: packet.source, binding: packet.binding.video_only() };
        if !program.required_bindings().iter().any(|required| required.binding == input) { return; }
        let rate = self.key.settings.sample_rate();
        let times = (|| {
            packet.block.validate(limits).ok()?;
            // Receipt time is itself bracketed by the session calibration. A
            // sub-sample exact mapping is often unavailable on real devices;
            // accept a bounded estimate instead of dropping every audio block.
            let receipt = packet.correlation.estimate_interval(self.key.session, self.key.clock_epoch,
                (packet.block.host_received_ns, packet.block.host_received_ns), rate, 5_000_000)?;
            let received = receipt.sample;
            let duration = u64::try_from((u128::from(packet.block.frames as u64) * u128::from(rate)).div_ceil(u128::from(packet.block.format.sample_rate))).ok()?;
            // Up to 5 ms uncertainty is usable for live audio, but it remains
            // marked estimated. Out-of-budget intervals keep the receipt-time
            // fallback rather than pretending to have a device timestamp.
            let estimate = packet.block.directshow_clock.and_then(|clock| {
                let bounds=clock.qpc_interval(packet.block.graph_timestamp_ns?)?;
                let estimate=packet.correlation.estimate_interval(self.key.session,self.key.clock_epoch,bounds,rate,5_000_000)?;
                (estimate.sample<=received).then_some(estimate)
            });
            let first = match estimate {Some(value)=>value.sample,None=>received.checked_sub(duration)?};
            let ns = |sample: u64| i128::try_from(u128::from(sample) * 1_000_000_000 / u128::from(rate)).ok();
            Some((ns(first)?, ns(received)?,receipt.exact && estimate.is_some_and(|value|value.exact)))
        })();
        let Some((first, received, exact)) = times else {
            self.embedded_discontinuities.insert(packet.source);
            return;
        };
        let mut flags = packet.block.flags;
        if exact {flags.timestamp_error=false;}
        flags.discontinuity |= self.embedded_discontinuities.contains(&packet.source);
        let result = program.identity().and_then(|identity| program.ingest_embedded(identity, packet.source, &packet.binding,
            packet.stream_generation, packet.sequence, first, received, &packet.block, flags));
        match result {
            Ok(report) => {
                self.embedded_discontinuities.remove(&packet.source);
                if !self.add(Counter::InputPackets, 1) || !self.add(Counter::InputFrames, report.accepted_frames as u64)
                    || !self.add(Counter::LateFrames, report.late_frames as u64) { self.retire(); }
            }
            Err(error) => {
                self.embedded_discontinuities.insert(packet.source);
                if !self.add(Counter::InputErrors, 1) { self.retire(); }
                self.problem(error.code(), error.to_string());
            }
        }
    }
    pub(super) fn ingest_ndi(&mut self,packet:&Arc<crate::ndi::NdiPcmPacket>){
        if packet.program!=self.key || !packet.scope_current(){return;}let Some(program)=&mut self.program else{return;};
        let input=crate::audio::AudioInputKey::Ndi {source:packet.source,ndi:(*packet.ndi).clone()};
        if !program.required_bindings().iter().any(|r|r.binding==input){return;}
        let result=program.identity().and_then(|identity|program.ingest_ndi_scoped(identity,packet));
        match result {Ok(report)=>{if !self.add(Counter::InputPackets,1)||!self.add(Counter::InputFrames,report.accepted_frames as u64)||!self.add(Counter::LateFrames,report.late_frames as u64){self.retire();}},Err(error)=>{if !self.add(Counter::InputErrors,1){self.retire();}self.problem(error.code(),error.to_string());}}
    }
    pub(super) fn ingest_media(&mut self, packet: &crate::media_source::MediaPcmPacket) {
        if let Some(barrier) = &packet.barrier {
            let result = if packet.program != self.key {
                Err(crate::audio_program::ProgramAudioError::Invalid(
                    "stale Program media barrier",
                ))
            } else if let Some(program) = &mut self.program {
                program
                    .identity()
                    .and_then(|identity| program.control_media(identity, barrier))
            } else {
                Err(crate::audio_program::ProgramAudioError::Invalid(
                    "Program media barrier has no running CPU owner",
                ))
            };
            let _ = barrier.reply.try_send(result);
            return;
        }
        if packet.program != self.key {
            return;
        }
        let Some(program) = &mut self.program else {
            return;
        };
        let input = crate::audio::AudioInputKey::Media {
            source: packet.source,
            media: (*packet.media).clone(),
        };
        if !program
            .required_bindings()
            .iter()
            .any(|r| r.binding == input)
        {
            return;
        }
        let result = program.identity().and_then(|identity| {
            if packet.end {
                program
                    .finish_media(
                        identity,
                        packet.source,
                        &packet.media,
                        packet.stream_generation,
                        packet.sequence,
                    )
                    .map(|_| None)
            } else {
                program
                    .ingest_media(crate::audio_program::MediaProgramInputPacket {
                        identity,
                        source: packet.source,
                        media: &packet.media,
                        stream_generation: packet.stream_generation,
                        sequence: packet.sequence,
                        first_media_ns: packet.first_media_ns,
                        received_media_ns: packet.first_media_ns,
                        format: packet.format,
                        samples: packet.samples.get(packet.sample_offset..).ok_or(
                            crate::audio_program::ProgramAudioError::Invalid(
                                "media PCM sample offset",
                            ),
                        )?,
                        flags: crate::audio_capture::NativePacketFlags::default(),
                    })
                    .map(Some)
            }
        });
        match result {
            Ok(Some(report)) => {
                if !self.add(Counter::InputPackets, 1)
                    || !self.add(Counter::InputFrames, report.accepted_frames as u64)
                    || !self.add(Counter::LateFrames, report.late_frames as u64)
                {
                    self.retire();
                }
            }
            Ok(None) => {}
            Err(error) => {
                if !self.add(Counter::InputErrors, 1) {
                    self.retire();
                }
                self.problem(error.code(), error.to_string());
            }
        }
    }
    /// At most four blocks per turn. If still due, the acquisition owner must
    /// service another fair control/native pass before calling tick again.
    pub(super) fn tick(&mut self, now: Instant) {
        if let Some(Err(error))=self.program.as_mut().map(AudioProgram::purge_retired_window_inputs) {
            self.problem(error.code(),error.to_string());self.retire();return;
        }
        if let Some(Err(error))=self.program.as_mut().map(AudioProgram::purge_retired_ndi_inputs) {
            self.problem(error.code(),error.to_string());self.retire();return;
        }
        self.output_sinks.prune();
        if self.recording_reader.is_some() && !self.output_sinks.has(crate::audio_output::OutputKind::Recording) {
            self.release_recording_reader();
        }
        if self.streaming_reader.is_some() && !self.output_sinks.has(crate::audio_output::OutputKind::Streaming){self.release_streaming_reader();}
        if self.recording_reader.is_none() && self.output_sinks.has(crate::audio_output::OutputKind::Recording)
            && self.program.as_ref().is_some_and(AudioProgram::needs_recording_delay_reader) {
            // Enabling delay in the middle of an already-published recording
            // cannot retroactively establish a retained recording prefix.
            self.output_sinks.fail_recording_backpressure();
        }
        if self.streaming_reader.is_none() && self.output_sinks.has(crate::audio_output::OutputKind::Streaming)
            && self.program.as_ref().is_some_and(AudioProgram::needs_streaming_delay_reader) {
            self.output_sinks.fail_streaming_backpressure();
        }
        self.refresh_output_readers();
        self.reap();
        self.drain_recording_reader();
        self.drain_streaming_reader();
        self.immediate = false;
        let recovery =
            if self.program.is_some() && self.deadline.is_some_and(|deadline| deadline <= now) {
                self.recover_suspension(now)
            } else {
                Ok(())
            };
        if let Err(error) = recovery {
            self.problem(error.code(), error.to_string());
            self.retire();
            return;
        }
        for _ in 0..MAX_CATCHUP_BLOCKS {
            if self.program.is_none() || self.deadline.is_none_or(|deadline| now < deadline) {
                break;
            }
            let block = match self.process_callback() {
                Ok(Some(block)) => block,
                Ok(None) => continue,
                Err(error) => {
                    self.problem(error.code(), error.to_string());
                    self.retire();
                    break;
                }
            };
            if let Some(sink)=&self.monitor_sink {self.stats.monitor=sink.status();}
            if !self.add(Counter::OutputBlocks, 1) {
                self.retire();
                break;
            }
            let bytes = output_charge(&block);
            if self.residents.len() >= MAX_RESIDENT_BLOCKS
                || self
                    .resident_bytes
                    .checked_add(bytes)
                    .is_none_or(|bytes| bytes > MAX_RESIDENT_BYTES)
            {
                self.problem(
                    "audio_capture_backpressure",
                    "Program output readers pin the bounded resident pool".into(),
                );
                self.output_sinks.fail_backpressure();
                self.drop_block();
                continue;
            }
            let block = Arc::new(block);
            // The worker's resident reference outlives the publication, so a
            // GUI/consumer dropping its last visible Arc cannot free PCM.
            self.resident_bytes += bytes;
            self.residents.push(block.clone());
            if let Some(reader)=&mut self.recording_reader {
                if reader.pending.len()>=reader.max_blocks {
                    self.fail_delayed_recording();
                } else {
                    reader.pending.push_back(RecordingStamp::from_block(&block));
                }
            }
            if let Some(reader)=&mut self.streaming_reader {
                if reader.pending.len()>=reader.max_blocks {self.fail_delayed_streaming();}
                else {reader.pending.push_back(RecordingStamp::from_block(&block));}
            }
            self.output_sinks.submit_live_except(self.key,block.clone(),self.recording_reader.is_some(),self.streaming_reader.is_some());
            self.drain_recording_reader();self.drain_streaming_reader();
            self.refresh_output_readers();
            let output = ProgramCaptureOutput {
                key: self.key,
                block,
                stats: self.stats.clone(),
            };
            let result = self
                .outputs
                .as_ref()
                .expect("active Program output sender")
                .try_send(output);
            match result {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(output)) => {
                    drop(output);
                    self.problem(
                        "audio_capture_backpressure",
                        "Program output queue is full".into(),
                    );
                    self.drop_block();
                }
                Err(mpsc::TrySendError::Disconnected(output)) => {
                    drop(output);
                    self.drop_block();
                    self.problem(
                        "audio_capture_disconnected",
                        "Program output receiver disconnected".into(),
                    );
                    self.retire();
                    break;
                }
            }
            // A failed nonblocking send leaves only our own Arc. Reclaim it
            // immediately instead of growing the pool when the sink is full.
            self.reap();
        }
        self.immediate =
            self.program.is_some() && self.deadline.is_some_and(|deadline| deadline <= now);
    }
    pub(super) fn needs_immediate(&self) -> bool {
        self.immediate
    }
    fn process_callback(&mut self) -> Result<Option<ProgramAudioBlock>, AudioCaptureError> {
        let readers=self.has_outputs();let tracks=self.output_sinks.selected_tracks();
        if let Some(program)=&mut self.program{program.program_output_tracks(tracks);program.camera_program_reader(readers);}
        let program=self.program.as_ref().ok_or(AudioCaptureError::Closed)?;
        self.clock.ensure_source_lookahead(program.sync_lookahead_frames(),program.source_processing_lookahead_frames())?;
        self.program.as_mut().ok_or(AudioCaptureError::Closed)?.service_source_processing().map_err(control_error)?;
        let selected = self.clock.next_interval()?;
        if self.clock.maxed() {
            let report: crate::audio_program::BufferedDiscardReport = self
                .program
                .as_mut()
                .ok_or(AudioCaptureError::Closed)?
                .buffered_drop_late(selected)
                .map_err(control_error)?;
            self.account_source_discard(
                report.discarded_frames,
                report.reset_inputs,
                report.stopped_inputs,
                true,
            )?;
        }
        let mut readiness: crate::audio_program::BufferedReadiness = self
            .program
            .as_mut()
            .ok_or(AudioCaptureError::Closed)?
            .buffered_prepare(selected)
            .map_err(control_error)?;
        let mut next_clock = self.clock.clone();
        let previous_decision = next_clock.observe_minimum(readiness.minimum_frame)?;
        let shortfall=self.program.as_ref().ok_or(AudioCaptureError::Closed)?
            .sync_source_shortfall(self.clock.cursor(),BLOCK as usize);
        let mut decision=next_clock.observe_source_shortfall(shortfall)?;
        decision.added_ticks=decision.added_ticks.checked_add(previous_decision.added_ticks)
            .ok_or(AudioCaptureError::TimeOverflow)?;
        if !decision.waiting
            && decision.added_ticks == 0
            && readiness.minimum_frame.is_some_and(|head| head < selected)
        {
            // A ceil output origin and nearest input origin may differ by one
            // sample while OBS's floor-nanosecond lag conversion yields zero.
            // Normalize that prefix now, rather than leave an unconsumable
            // ready head which would ratchet delay on each later callback.
            let report = self
                .program
                .as_mut()
                .ok_or(AudioCaptureError::Closed)?
                .buffered_normalize_ready(selected)
                .map_err(control_error)?;
            self.account_source_discard(
                report.discarded_frames,
                report.reset_inputs,
                report.stopped_inputs,
                true,
            )?;
            readiness = self
                .program
                .as_mut()
                .ok_or(AudioCaptureError::Closed)?
                .buffered_prepare(selected)
                .map_err(control_error)?;
        }
        readiness.pending_inputs=readiness.pending_inputs.checked_add(
            self.program.as_ref().ok_or(AudioCaptureError::Closed)?.sync_pending_sources(self.clock.cursor(),BLOCK as usize))
            .ok_or(AudioCaptureError::TimeOverflow)?;
        next_clock.complete_tick()?;
        let next_deadline = wall_deadline(
            self.anchor,
            self.media_anchor,
            self.initial_cursor,
            next_clock.wall_ticks(),
            self.key.settings.sample_rate(),
        )?;
        let suppressed = self
            .stats
            .suppressed_blocks
            .checked_add(u64::from(decision.waiting))
            .ok_or(AudioCaptureError::Limit(
                "Program suppressed callback counter exhausted",
            ))?;
        let growths = self
            .stats
            .buffering_growths
            .checked_add(u64::from(decision.added_ticks != 0))
            .ok_or(AudioCaptureError::Limit(
                "Program buffer growth counter exhausted",
            ))?;
        // Monitoring follows its own live wall grid, including Program's
        // discard-only lookahead waits. Audio-only monitor ignores sync offset.
        self.program.as_mut().ok_or(AudioCaptureError::Closed)?.monitor_reader_active(self.monitor_sink.is_some());
        if let Some(sink)=&self.monitor_sink {
            let wall_frame=self.initial_cursor.checked_add(self.clock.wall_ticks().checked_mul(BLOCK)
                .ok_or(AudioCaptureError::TimeOverflow)?).ok_or(AudioCaptureError::TimeOverflow)?;
            if let Some(frame)=self.program.as_mut().ok_or(AudioCaptureError::Closed)?
                .render_live_monitor(wall_frame,sink.status().device_padding_ns).map_err(control_error)? {sink.submit(frame);}
        }
        let block = if decision.waiting {
            None
        } else {
            Some(
                self.program
                    .as_mut()
                    .ok_or(AudioCaptureError::Closed)?
                    .render_tracks(self.clock.cursor(), BLOCK as usize,tracks)
                    .map_err(control_error)?,
            )
        };
        let _=self.program.as_mut().and_then(|program|program.take_monitor_frame());
        let discarded = self
            .program
            .as_mut()
            .ok_or(AudioCaptureError::Closed)?
            .buffered_discard(decision.start_frame)
            .map_err(control_error)?;
        self.account_source_discard(
            discarded.discarded_frames,
            discarded.reset_inputs,
            discarded.stopped_inputs,
            false,
        )?;
        self.clock = next_clock;
        self.deadline = Some(next_deadline);
        self.stats.buffer_blocks = self.clock.total_ticks();
        self.stats.waiting_blocks = self.clock.waiting_ticks();
        self.stats.wall_ticks = self.clock.wall_ticks();
        self.stats.suppressed_blocks = suppressed;
        self.stats.buffering_growths = growths;
        self.stats.pending_inputs = readiness.pending_inputs;
        Ok(block)
    }
    fn account_source_discard(
        &mut self,
        frames: u64,
        resets: u32,
        stopped: u32,
        late: bool,
    ) -> Result<(), AudioCaptureError> {
        self.stats.discarded_source_frames = self
            .stats
            .discarded_source_frames
            .checked_add(frames)
            .ok_or(AudioCaptureError::Limit(
                "Program source discard counter exhausted",
            ))?;
        self.stats.reset_inputs = self
            .stats
            .reset_inputs
            .checked_add(u64::from(resets))
            .ok_or(AudioCaptureError::Limit(
                "Program source reset counter exhausted",
            ))?;
        self.stats.stopped_inputs = self
            .stats
            .stopped_inputs
            .checked_add(u64::from(stopped))
            .ok_or(AudioCaptureError::Limit(
                "Program source stopped counter exhausted",
            ))?;
        if late {
            self.stats.late_frames =
                self.stats
                    .late_frames
                    .checked_add(frames)
                    .ok_or(AudioCaptureError::Limit(
                        "Program late frame counter exhausted",
                    ))?;
        }
        Ok(())
    }
    fn recover_suspension(&mut self, now: Instant) -> Result<(), AudioCaptureError> {
        let media_now = self.media_now(now)?;
        let latest_frame =
            ((u128::from(media_now.0) + 1) * u128::from(self.key.settings.sample_rate()) - 1)
                / 1_000_000_000;
        let first_deadline_frame = u128::from(self.initial_cursor)
            + (u128::from(self.clock.wall_ticks()) + 1) * u128::from(BLOCK);
        let due = if latest_frame < first_deadline_frame {
            0
        } else {
            u64::try_from((latest_frame - first_deadline_frame) / u128::from(BLOCK) + 1)
                .map_err(|_| AudioCaptureError::TimeOverflow)?
        };
        if due <= SUSPENSION_BLOCKS {
            return Ok(());
        }
        let skipped_wall = due - MAX_CATCHUP_BLOCKS as u64;
        let target = self
            .clock
            .wall_ticks()
            .checked_add(skipped_wall)
            .ok_or(AudioCaptureError::TimeOverflow)?;
        let mut clock = self.clock.clone();
        let skipped = clock.skip_wall_to(target)?;
        let deadline = wall_deadline(
            self.anchor,
            self.media_anchor,
            self.initial_cursor,
            clock.wall_ticks(),
            self.key.settings.sample_rate(),
        )?;
        let skipped_blocks = self
            .stats
            .skipped_blocks
            .checked_add(skipped.media_frames / BLOCK)
            .ok_or(AudioCaptureError::Limit(
                "Program skipped block counter exhausted",
            ))?;
        let skipped_frames = self
            .stats
            .skipped_frames
            .checked_add(skipped.media_frames)
            .ok_or(AudioCaptureError::Limit(
                "Program skipped frame counter exhausted",
            ))?;
        let skipped_wall_ticks = self
            .stats
            .skipped_wall_ticks
            .checked_add(skipped.wall_ticks)
            .ok_or(AudioCaptureError::Limit(
                "Program skipped wall tick counter exhausted",
            ))?;
        let suppressed = self
            .stats
            .suppressed_blocks
            .checked_add(skipped.waiting_ticks)
            .ok_or(AudioCaptureError::Limit(
                "Program suppressed callback counter exhausted",
            ))?;
        let report = self
            .program
            .as_mut()
            .ok_or(AudioCaptureError::Closed)?
            .buffered_skip_to(clock.cursor())
            .map_err(control_error)?;
        self.account_source_discard(
            report.discarded_frames,
            report.reset_inputs,
            report.stopped_inputs,
            true,
        )?;
        self.clock = clock;
        self.deadline = Some(deadline);
        self.stats.skipped_blocks = skipped_blocks;
        self.stats.skipped_frames = skipped_frames;
        self.stats.skipped_wall_ticks = skipped_wall_ticks;
        self.stats.wall_ticks = self.clock.wall_ticks();
        self.stats.waiting_blocks = self.clock.waiting_ticks();
        self.stats.suppressed_blocks = suppressed;
        Ok(())
    }
    /// Returns true while external readers still pin resident output blocks.
    pub(super) fn reap(&mut self) -> bool {
        self.residents.retain(|block| Arc::strong_count(block) > 1);
        self.resident_bytes = self
            .residents
            .iter()
            .map(|block| output_charge(block))
            .sum();
        !self.residents.is_empty()
    }
    pub(super) fn can_retire(&mut self) -> bool {
        !self.reap()
    }
    pub(super) fn retire(&mut self) {
        if let Some(sink)=self.monitor_sink.take(){sink.stop();}
        self.release_recording_reader();
        self.release_streaming_reader();
        self.output_sinks.fail_scope();
        self.program = None;
        self.outputs = None;
        self.deadline = None;
        self.immediate = false;
        self.stats.running = false;
        self.reap();
    }
    pub(super) fn fail(&mut self, error: AudioCaptureError) {
        self.problem(error.code(), error.to_string());
        self.retire();
    }
    fn drop_block(&mut self) {
        if !self.add(Counter::DroppedBlocks, 1) || !self.add(Counter::DroppedFrames, BLOCK) {
            self.retire();
        }
    }
    fn problem(&mut self, code: &'static str, mut detail: String) {
        if detail.len() > MAX_PROBLEM_BYTES {
            let mut end = MAX_PROBLEM_BYTES;
            while !detail.is_char_boundary(end) {
                end -= 1;
            }
            detail.truncate(end);
        }
        self.stats.problem = Some(ProgramTransportProblem { code, detail });
    }
    fn add(&mut self, counter: Counter, amount: u64) -> bool {
        let target = match counter {
            Counter::InputPackets => &mut self.stats.input_packets,
            Counter::InputFrames => &mut self.stats.input_frames,
            Counter::InputErrors => &mut self.stats.input_errors,
            Counter::OutputBlocks => &mut self.stats.output_blocks,
            Counter::DroppedBlocks => &mut self.stats.dropped_blocks,
            Counter::DroppedFrames => &mut self.stats.dropped_frames,
            Counter::LateFrames => &mut self.stats.late_frames,
        };
        if let Some(value) = target.checked_add(amount) {
            *target = value;
            true
        } else {
            self.problem(
                "audio_capture_limit",
                "Program transport counter exhausted".into(),
            );
            false
        }
    }
}
enum Counter {
    InputPackets,
    InputFrames,
    InputErrors,
    OutputBlocks,
    DroppedBlocks,
    DroppedFrames,
    LateFrames,
}

fn control_error(error: ProgramAudioError) -> AudioCaptureError {
    AudioCaptureError::Native {
        operation: "Program audio",
        detail: error.to_string(),
    }
}
fn sample_ns(frame: u64, rate: u32) -> Result<u64, AudioCaptureError> {
    u64::try_from(u128::from(frame) * 1_000_000_000 / u128::from(rate))
        .map_err(|_| AudioCaptureError::TimeOverflow)
}
fn wall_deadline(
    anchor: Instant,
    media: MediaTime,
    initial_cursor: u64,
    wall_ticks: u64,
    rate: u32,
) -> Result<Instant, AudioCaptureError> {
    let end = u128::from(initial_cursor) + (u128::from(wall_ticks) + 1) * u128::from(BLOCK);
    let end = u64::try_from(end).map_err(|_| AudioCaptureError::TimeOverflow)?;
    let offset = sample_ns(end, rate)?
        .checked_sub(media.0)
        .ok_or(AudioCaptureError::TimeOverflow)?;
    anchor
        .checked_add(Duration::from_nanos(offset))
        .ok_or(AudioCaptureError::TimeOverflow)
}
fn output_charge(block: &ProgramAudioBlock) -> usize {
    size_of::<ProgramAudioBlock>()
        // Each published envelope can also retain a bounded diagnostic string
        // beside its Arc. Charge its maximum, not only the PCM allocation.
        + size_of::<ProgramCaptureOutput>() + MAX_PROBLEM_BYTES
        + 2 * size_of::<usize>()
        + block.samples.capacity() * size_of::<f32>()
        + block.additional_track_heap_bytes().unwrap_or(MAX_RESIDENT_BYTES+1)
        + block.program_meters.capacity() * size_of::<crate::audio::ChannelMeter>()
        + block.source_meters.capacity() * size_of::<crate::audio::SourceMeter>()
        + block
            .source_meters
            .iter()
            .map(|meter| meter.channels.capacity() * size_of::<crate::audio::ChannelMeter>())
            .sum::<usize>()
        + block.clipped_samples.capacity() * size_of::<usize>()
        + block.gaps.capacity() * size_of::<crate::audio_program::ProgramGap>()
}

#[cfg(test)]
#[path = "program_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "adaptive_program_tests.rs"]
mod adaptive_tests;
