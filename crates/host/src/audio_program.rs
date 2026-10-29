//! Deterministic timestamped Program PCM evaluation. No device, UI or callback.
use crate::{
    audio::{
        AudioDspState, AudioError, AudioInputKey, AudioMixRequest, AudioRouteSnapshot,
        AudioRouteTimeline, ChannelMeter, MAX_AUDIO_BLOCK_SOURCES, PcmInput, RequiredAudioBinding,
        SourceMeter,
    },
    audio_capture::{AudioTimingOrigin, CapturedAudioPacket, NativeAudioFormat, NativePacketFlags},
    transition::FrameComposition,
};
use serde::Serialize;
use skylive_core::{AudioCaptureBinding, AudioSettings, Id, Session};
use skylive_media::{AudioConvertLimits, AudioConverter, AudioFormat, MediaError, MediaTime};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc,atomic::{AtomicU64, Ordering}},
};

mod buffer;
mod buffered;
mod media_controls;
mod sync_offset;
mod post_dsp;
mod camera_gate;
mod monitoring;
mod push_gate;
use buffer::SampleRing;
pub(crate) use buffered::{BufferedDiscardReport, BufferedReadiness};
#[cfg(test)]
mod media_tests;
#[cfg(test)]
mod tests;
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
const SMOOTH_NS: i128 = 70_000_000;
const JUMP_NS: i128 = 2_000_000_000;

/// Exact maximum payload of the render-only source ID/metric/gap vectors and
/// their output vectors. No HashMap load-factor estimate is needed. The PCM
/// workspace still contains at most64 sources for one span at a time.
fn render_metadata_bytes(channels: usize) -> Result<usize, ProgramAudioError> {
    let per_source = std::mem::size_of::<Id>()
        + std::mem::size_of::<(Id, ([f64; 8], [f64; 8]))>()
        + std::mem::size_of::<(Id, usize)>()
        + std::mem::size_of::<SourceMeter>()
        + channels
            .checked_mul(std::mem::size_of::<ChannelMeter>())
            .ok_or(ProgramAudioError::Limit("Program meter byte overflow"))?
        + std::mem::size_of::<ProgramGap>();
    MAX_AUDIO_BLOCK_SOURCES
        .checked_mul(per_source)
        .ok_or(ProgramAudioError::Limit(
            "Program render metadata byte overflow",
        ))
}

#[derive(Clone, Copy, Debug)]
pub struct ProgramAudioLimits {
    pub max_groups: usize,
    pub max_buffer_ms: usize,
    pub max_bytes: usize,
    pub max_block_frames: usize,
}
impl Default for ProgramAudioLimits {
    fn default() -> Self {
        Self {
            max_groups: 32,
            max_buffer_ms: 2000,
            max_bytes: 128 * 1024 * 1024,
            max_block_frames: 1024,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgramAudioError {
    Invalid(&'static str),
    Limit(&'static str),
    Backpressure(&'static str),
    TimeOverflow,
    FormatChanged,
    ClockRewind,
    Routing(AudioError),
    Conversion(MediaError),
}
impl ProgramAudioError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "program_audio_invalid",
            Self::Limit(_) => "program_audio_limit",
            Self::Backpressure(_) => "program_audio_backpressure",
            Self::TimeOverflow => "program_audio_time_overflow",
            Self::FormatChanged => "program_audio_format_changed",
            Self::ClockRewind => "program_audio_clock_rewind",
            Self::Routing(_) => "program_audio_routing",
            Self::Conversion(_) => "program_audio_conversion",
        }
    }
}
impl fmt::Display for ProgramAudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {self:?}", self.code())
    }
}
impl std::error::Error for ProgramAudioError {}
impl From<AudioError> for ProgramAudioError {
    fn from(error: AudioError) -> Self {
        Self::Routing(error)
    }
}
impl From<MediaError> for ProgramAudioError {
    fn from(error: MediaError) -> Self {
        Self::Conversion(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ProgramInputIdentity {
    pub instance: u64,
    pub epoch: u64,
    pub session: u64,
}
pub struct ProgramInputPacket<'a> {
    pub identity: ProgramInputIdentity,
    pub binding: &'a AudioCaptureBinding,
    pub stream_generation: u64,
    pub sequence: u64,
    pub first_media_ns: i128,
    pub received_media_ns: i128,
    pub format: NativeAudioFormat,
    pub samples: &'a [f32],
    pub flags: NativePacketFlags,
}
pub struct MediaProgramInputPacket<'a> {
    pub identity: ProgramInputIdentity,
    pub source: Id,
    pub media: &'a skylive_core::MediaSource,
    pub stream_generation: u64,
    pub sequence: u64,
    pub first_media_ns: i128,
    pub received_media_ns: i128,
    pub format: NativeAudioFormat,
    pub samples: &'a [f32],
    pub flags: NativePacketFlags,
}
pub struct NdiProgramInputPacket<'a> {
    pub identity: ProgramInputIdentity,
    pub source: Id,
    pub ndi: &'a skylive_core::NdiSource,
    pub stream_generation: u64,
    pub sequence: u64,
    pub first_media_ns: i128,
    pub received_media_ns: i128,
    pub format: NativeAudioFormat,
    pub samples: &'a [f32],
    pub flags: NativePacketFlags,
}
pub struct BrowserProgramInputPacket<'a> {
    pub identity:ProgramInputIdentity,pub key:&'a crate::browser::BrowserAudioKey,
    pub stream_generation:u64,pub sequence:u64,pub first_media_ns:i128,pub received_media_ns:i128,
    pub format:NativeAudioFormat,pub samples:&'a[f32],pub flags:NativePacketFlags,
}
struct KeyInputPacket<'a> {
    identity: ProgramInputIdentity,
    binding: &'a AudioInputKey,
    stream_generation: u64,
    sequence: u64,
    first_media_ns: i128,
    received_media_ns: i128,
    format: NativeAudioFormat,
    samples: &'a [f32],
    flags: NativePacketFlags,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ProgramInputCursor {
    pub generation: u64,
    pub sequence: u64,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct AudioIngestReport {
    pub accepted_frames: usize,
    pub output_frames: usize,
    pub timestamp_smoothed: bool,
    pub timing_reanchored: bool,
    pub discontinuity: bool,
    pub gap_frames: usize,
    pub overlap_frames: usize,
    /// Logical converted frames discarded because their output was already emitted.
    pub late_frames: usize,
    pub inferred_channel_layout: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProgramGap {
    pub source: Id,
    pub missing_frames: usize,
}
#[derive(Debug,Serialize)]
pub struct ProgramAudioTrack{
    pub track:skylive_core::AudioTrackId,
    pub samples:Vec<f32>,
}
pub(crate) const PROGRAM_RESIDENT_BYTES:usize=8*1024*1024;
#[derive(Debug, Serialize)]
pub struct ProgramAudioBlock {
    pub instance: u64,
    pub epoch: u64,
    pub session: u64,
    pub data_generation: u64,
    pub routing_serial: u64,
    pub settings: AudioSettings,
    pub start_frame: u64,
    pub frames: usize,
    pub samples: Vec<f32>,
    #[serde(skip_serializing_if="Vec::is_empty")]
    pub tracks:Vec<ProgramAudioTrack>,
    pub program_meters: Vec<ChannelMeter>,
    pub source_meters: Vec<SourceMeter>,
    /// Present only when every routing span contributing to these meters used
    /// the same authored revision. Mixed-control blocks have no current-config claim.
    pub source_meter_revision: Option<u64>,
    pub clipped_samples: Vec<usize>,
    pub gaps: Vec<ProgramGap>,
}

/// A sink-specific mix built before the shared Program timeline is retired.
/// It intentionally has no Program meter or routing identity: it must be
/// paired with the recording owner that supplied the live camera alignments.
pub(crate) struct RecordingAudioMix {
    pub true_peak_revision: Option<u64>,
    pub start_frame: u64,
    pub frames: usize,
    pub samples: Vec<f32>,
    pub tracks: Vec<ProgramAudioTrack>,
}

/// Retention for an independent recording reader. Overflow or a skipped
/// Program interval retires this lease only; it does not stop other sinks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordingHistoryStatus {
    Holding { recording:u64, floor:u64, max_frames:usize },
    Overflow { recording:u64 },
    Interrupted { recording:u64 },
}
impl ProgramAudioBlock{
    pub fn track_samples(&self,track:skylive_core::AudioTrackId)->Option<&[f32]>{
        if track==skylive_core::AudioTrackId::ONE{Some(&self.samples)}else{
            self.tracks.iter().find(|entry|entry.track==track).map(|entry|entry.samples.as_slice())
        }
    }
    pub(crate) fn additional_track_heap_bytes(&self)->Option<usize>{
        self.tracks.iter().try_fold(self.tracks.capacity().checked_mul(std::mem::size_of::<ProgramAudioTrack>())?,
            |bytes,entry|bytes.checked_add(entry.samples.capacity().checked_mul(4)?))
    }
    pub(crate) fn additional_track_bytes(&self)->Result<usize,&'static str>{
        if self.tracks.len()>5||self.tracks.capacity()>5{return Err("audio bundle contains more than five additional tracks");}
        let count=self.frames.checked_mul(usize::from(self.settings.channels())).ok_or("audio bundle frame overflow")?;
        let mut seen=1u8;let mut bytes=self.tracks.capacity()*std::mem::size_of::<ProgramAudioTrack>();
        for entry in &self.tracks{
            if seen&entry.track.bit()!=0||entry.samples.len()!=count||entry.samples.iter().any(|value|!value.is_finite()){return Err("invalid, duplicate or incomplete audio bundle track");}
            seen|=entry.track.bit();
            bytes=bytes.checked_add(entry.samples.capacity().checked_mul(4).ok_or("audio track byte overflow")?).ok_or("audio bundle byte overflow")?;
        }Ok(bytes)
    }
    pub fn track_mask(&self)->skylive_core::SourceAudioTrackMask{
        self.tracks.iter().fold(skylive_core::SourceAudioTrackMask::NONE.with(skylive_core::AudioTrackId::ONE,true),|mask,entry|mask.with(entry.track,true))
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ProgramAudioStatus {
    pub instance: u64,
    pub epoch: u64,
    pub data_generation: u64,
    pub session: Option<u64>,
    pub settings: AudioSettings,
    pub cursor: Option<u64>,
    pub routing_events: usize,
    pub input_groups: usize,
    pub known_input_streams: usize,
    pub accounted_bytes: usize,
    pub continuous_native_transport: bool,
}

struct InputGroup {
    converter: AudioConverter,
    ring: SampleRing,
    format: NativeAudioFormat,
    generation: u64,
    dsp_epoch: u64,
    last_sequence: u64,
    input_anchor: i128,
    input_frames: u64,
    adjust: i128,
    output_anchor: i128,
    output_frames: u64,
    replace_next: bool,
    scratch: Vec<f32>,
    buffered: buffered::InputState,
}
impl InputGroup {
    fn bytes(&self) -> usize {
        self.converter.stats().accounted_bytes
            + self.ring.bytes()
            + self.scratch.capacity() * 4
            + std::mem::size_of::<Self>()
    }
}
struct InputFence {
    cursor: ProgramInputCursor,
    sealed: bool,
}
pub struct AudioProgram {
    monitor_frame:Option<crate::audio_monitor::MonitorFrame>,
    true_peak_meters:crate::audio::true_peak::SourceTruePeaks,
    master_true_peak:crate::audio::true_peak::MasterTruePeak,
    settings: AudioSettings,
    output: AudioFormat,
    limits: ProgramAudioLimits,
    timeline: AudioRouteTimeline,
    groups: HashMap<AudioInputKey, InputGroup>,
    fences: HashMap<AudioInputKey, InputFence>,
    window_guards: HashMap<Id, crate::audio_capture::window_observation::WindowAudioEvidence>,
    ndi_guards: HashMap<Id, Arc<crate::ndi::NdiPcmPacket>>,
    media_controls: media_controls::MediaControls,
    cursor: Option<u64>,
    instance: u64,
    epoch: u64,
    data_generation: u64,
    reserved_bytes: usize,
    buffered: bool,
    capture_bindings: Vec<AudioCaptureBinding>,
    dsp: AudioDspState,
    dsp_streams: HashMap<Id, (AudioInputKey, u64, i32)>,
    sync: sync_offset::SourceSync,
    histories:post_dsp::Histories,
    push:push_gate::Bank,
    push_epochs:Vec<(Id,Option<skylive_core::SourceDefinitionEpoch>)>,
    camera_gates:camera_gate::Gates,
    camera_program_reader:bool,
    active_output_tracks:skylive_core::SourceAudioTrackMask,
    monitor_video: Vec<crate::audio::MonitorVideoStamp>,
    camera_delay_history: Vec<(Id,usize)>,
    output_delay: bool,
    recording_camera_alignments: Vec<crate::audio_capture::RecordingCameraAlignment>,
    streaming_alignments: Vec<crate::audio_capture::RecordingCameraAlignment>,
    recording_history: Option<RecordingHistoryStatus>,
    // Independent streaming PCM lease: it must not advance or retire the
    // recording reader's floor and vice versa.
    streaming_history: Option<RecordingHistoryStatus>,
    monitor_dsp: AudioDspState,
    monitor_lanes: Vec<monitoring::MonitorLane>,
}
impl AudioProgram {
    /// Reserve processed PCM for every current source and retain subsequent
    /// routes for one independent recording reader. The caller owns the sink
    /// identity and must stop its sink if the lease reports overflow.
    pub(crate) fn begin_recording_history(&mut self, recording:u64, start:u64, max_frames:usize)->Result<(),ProgramAudioError>{
        if recording==0 || max_frames==0 || max_frames>self.settings.sample_rate() as usize*21
            || self.cursor.is_some_and(|cursor|cursor!=start)
            || self.timeline.session().is_none() || matches!(self.recording_history,Some(RecordingHistoryStatus::Holding{..})) {
            return Err(ProgramAudioError::Invalid("recording history lease scope"));
        }
        // Ensure this exact start remains a retained routing point before any
        // large allocation, then reserve old+candidate ring memory atomically.
        drop(self.timeline.segments(start,1)?);
        let resident=self.bytes();
        let existing=match self.streaming_history {Some(RecordingHistoryStatus::Holding{max_frames,..})=>max_frames,_=>0};
        self.histories.reserve_recording_reader(max_frames.max(existing),&self.sync,&self.camera_delay_history,self.limits,resident)?;
        self.recording_history=Some(RecordingHistoryStatus::Holding{recording,floor:start,max_frames});
        Ok(())
    }
    pub(crate) fn recording_history_status(&self)->Option<RecordingHistoryStatus>{self.recording_history}
    /// Release a completed prefix only after the recording sink accepted it.
    pub(crate) fn advance_recording_history(&mut self,recording:u64,floor:u64)->Result<(),ProgramAudioError>{
        let Some(RecordingHistoryStatus::Holding{recording:id,floor:old,max_frames})=self.recording_history else {
            return Err(ProgramAudioError::Invalid("recording history lease is unavailable"));
        };
        if id!=recording || floor<old || self.cursor.is_some_and(|cursor|floor>cursor) {
            return Err(ProgramAudioError::Invalid("recording history acknowledgement is outside its scope"));
        }
        self.recording_history=Some(RecordingHistoryStatus::Holding{recording:id,floor,max_frames});
        Ok(())
    }
    pub(crate) fn stop_recording_history(&mut self,recording:u64)->bool {
        if self.recording_history.is_some_and(|state|match state {
            RecordingHistoryStatus::Holding{recording:id,..}|RecordingHistoryStatus::Overflow{recording:id}|RecordingHistoryStatus::Interrupted{recording:id}=>id==recording,
        }) {self.recording_history=None;true}else{false}
    }
    pub(crate) fn begin_streaming_history(&mut self,output:u64,start:u64,max_frames:usize)->Result<(),ProgramAudioError>{
        if output==0 || max_frames==0 || max_frames>self.settings.sample_rate() as usize*21
            || self.cursor.is_some_and(|cursor|cursor!=start) || self.timeline.session().is_none()
            || matches!(self.streaming_history,Some(RecordingHistoryStatus::Holding{..})) {
            return Err(ProgramAudioError::Invalid("streaming history lease scope"));
        }
        drop(self.timeline.segments(start,1)?);
        let existing=match self.recording_history {Some(RecordingHistoryStatus::Holding{max_frames,..})=>max_frames,_=>0};
        let resident=self.bytes();
        self.histories.reserve_recording_reader(max_frames.max(existing),&self.sync,&self.camera_delay_history,self.limits,resident)?;
        self.streaming_history=Some(RecordingHistoryStatus::Holding{recording:output,floor:start,max_frames});Ok(())
    }
    pub(crate) fn streaming_history_status(&self)->Option<RecordingHistoryStatus>{self.streaming_history}
    pub(crate) fn advance_streaming_history(&mut self,output:u64,floor:u64)->Result<(),ProgramAudioError>{
        let Some(RecordingHistoryStatus::Holding{recording:id,floor:old,max_frames})=self.streaming_history else{return Err(ProgramAudioError::Invalid("streaming history lease unavailable"));};
        if id!=output || floor<old || self.cursor.is_some_and(|cursor|floor>cursor){return Err(ProgramAudioError::Invalid("streaming history acknowledgement outside scope"));}
        self.streaming_history=Some(RecordingHistoryStatus::Holding{recording:id,floor,max_frames});Ok(())
    }
    pub(crate) fn stop_streaming_history(&mut self,output:u64)->bool {
        if self.streaming_history.is_some_and(|state|match state {RecordingHistoryStatus::Holding{recording:id,..}|RecordingHistoryStatus::Overflow{recording:id}|RecordingHistoryStatus::Interrupted{recording:id}=>id==output}) {self.streaming_history=None;true}else{false}
    }
    pub(crate) fn recording_camera_alignments(&self)->&[crate::audio_capture::RecordingCameraAlignment] {
        &self.recording_camera_alignments
    }
    pub(crate) fn streaming_alignments(&self)->&[crate::audio_capture::RecordingCameraAlignment]{&self.streaming_alignments}
    /// A recording needs its own PCM reader when an active camera contributes
    /// asynchronous video delay. Other Program consumers keep the live grid.
    pub(crate) fn needs_recording_delay_reader(&self)->bool {
        !self.camera_delay_history.is_empty()
    }
    pub(crate) fn delay_source_active(&self,source:Id)->bool{self.camera_delay_history.iter().any(|(id,_)|*id==source)}
    pub(crate) fn needs_streaming_delay_reader(&self)->bool {
        self.output_delay
    }
    /// Remix recording tracks from retained per-source PCM without advancing
    /// the Program cursor. Other outputs keep their ordinary Program mix.
    /// Historical blocks are permitted only while this recording owns the
    /// corresponding history lease. A live recording receipt is required for
    /// every camera whose timing is shifted.
    pub(crate) fn mix_recording_sources(
        &self,
        recording: u64,
        clock_epoch: u64,
        start_frame: u64,
        frames: usize,
        requested: skylive_core::SourceAudioTrackMask,
        alignments: &[crate::audio_capture::RecordingCameraAlignment],
        recovering: &[Id],
    ) -> Result<RecordingAudioMix, ProgramAudioError> {
        self.mix_retained_sources(recording,clock_epoch,start_frame,frames,requested,alignments,recovering,false)
    }
    pub(crate) fn mix_streaming_sources(&self,output:u64,clock_epoch:u64,start_frame:u64,frames:usize,
        requested:skylive_core::SourceAudioTrackMask,alignments:&[crate::audio_capture::RecordingCameraAlignment],recovering:&[Id])
        ->Result<RecordingAudioMix,ProgramAudioError> {
        self.mix_retained_sources(output,clock_epoch,start_frame,frames,requested,alignments,recovering,true)
    }
    fn mix_retained_sources(&self,recording:u64,clock_epoch:u64,start_frame:u64,frames:usize,
        requested:skylive_core::SourceAudioTrackMask,alignments:&[crate::audio_capture::RecordingCameraAlignment],recovering:&[Id],streaming:bool)
        ->Result<RecordingAudioMix,ProgramAudioError> {
        if self.has_retired_window_input() {return Err(ProgramAudioError::Invalid("window audio generation changed before recording read"));}
        if self.has_retired_ndi_input() {return Err(ProgramAudioError::Invalid("NDI audio generation changed before recording read"));}
        if frames == 0 || frames > self.limits.max_block_frames
            || alignments.len() > crate::audio::MAX_AUDIO_INPUTS {
            return Err(ProgramAudioError::Limit("recording mix admission"));
        }
        let end_frame=start_frame.checked_add(frames as u64).ok_or(ProgramAudioError::TimeOverflow)?;
        if self.cursor.is_some_and(|cursor| cursor != start_frame) {
            // A separately buffered recording reader may lag Program, but it
            // must never read ahead or outside its retained routing/PCM range.
            let history=if streaming {self.streaming_history}else{self.recording_history};
            let retained=matches!(history,
                Some(RecordingHistoryStatus::Holding{recording:id,floor,..})
                    if id==recording && start_frame>=floor)
                && self.cursor.is_some_and(|cursor|end_frame<=cursor);
            if !retained {return Err(ProgramAudioError::ClockRewind);}
        }
        for (index, alignment) in alignments.iter().enumerate() {
            if alignments[..index].iter().any(|old| old.source == alignment.source && old.revision == alignment.revision)
                || recording == 0 || alignment.recording != recording
                || self.timeline.session() != Some(alignment.session)
                || alignment.clock_epoch != clock_epoch
                || alignment.sample_rate != self.settings.sample_rate()
                || !alignment.current() {
                return Err(ProgramAudioError::Invalid("recording mix alignment is stale, duplicate or outside its scope"));
            }
        }
        let mask=requested.with(skylive_core::AudioTrackId::ONE,true);
        let count=frames.checked_mul(usize::from(self.settings.channels()))
            .ok_or(ProgramAudioError::Limit("recording mix sample overflow"))?;
        let track_count=mask.bits().count_ones() as usize;
        let bytes=count.checked_mul(std::mem::size_of::<f32>())
            .and_then(|n|n.checked_mul(crate::audio::MAX_AUDIO_INPUTS+track_count+12))
            .ok_or(ProgramAudioError::Limit("recording mix workspace overflow"))?;
        if bytes>16*1024*1024 {return Err(ProgramAudioError::Limit("recording mix workspace"));}
        let spans=self.timeline.segments(start_frame,frames)?;
        let true_peak_revision = spans.first().map(|span| span.plan.revision()).filter(|revision|
            spans.iter().all(|span| span.plan.peak_mode() == skylive_core::MixerPeakMode::TruePeakEstimate
                && span.plan.revision() == *revision));
        // Never mix one delayed camera on the ordinary grid just because a
        // receipt for a different camera arrived first. The recording owner
        // must keep waiting (or report overload) until every active delayed
        // source in these spans has its own validated alignment.
        for span in &spans {
            for source in span.plan.sources() {
                let audible=(source.routes.global||source.routes.from||source.routes.to||source.routes.duplicated)
                    &&source.monitoring!=skylive_core::AudioMonitoringMode::MonitorOnly
                    &&source.properties.audio_tracks().intersects(mask);
                let source_recovering=recovering.contains(&source.source)&&matches!(&source.binding,AudioInputKey::Ndi {..});
                if !streaming && audible && !source_recovering && !matches!(&source.binding,AudioInputKey::Ndi {..}) && self.camera_delay_history.iter().any(|(id,_)|*id==source.source)
                    && !alignments.iter().any(|alignment|alignment.source==source.source && alignment.revision==span.plan.revision()) {
                    return Err(ProgramAudioError::Backpressure("recording is waiting for delayed camera alignment"));
                }
                if audible {
                    if let Some(alignment)=alignments.iter().find(|alignment|alignment.source==source.source && alignment.revision==span.plan.revision()) {
                        let end=start_frame.checked_add(span.offset as u64).and_then(|at|at.checked_add(span.frames as u64))
                            .ok_or(ProgramAudioError::TimeOverflow)?;
                        if !alignment.is_ndi() && end>alignment.presentation_sample && end>alignment.confirmed_until_sample {
                            return Err(ProgramAudioError::Backpressure("recording is waiting for accepted video coverage"));
                        }
                    }
                }
            }
        }
        // A proof for another camera in the same revision can coexist while
        // scenes switch. It is harmless unless that source occurs in a span.
        let work=spans.iter().try_fold(0usize,|total,span|span.plan.sources().len()
            .checked_mul(span.frames)?.checked_mul(usize::from(self.settings.channels()))?
            .checked_mul(track_count*3+1)?.checked_add(total))
            .ok_or(ProgramAudioError::Limit("recording mix work overflow"))?;
        if work>64*1024*1024 {return Err(ProgramAudioError::Limit("recording mix work"));}
        let mut samples=vec![0.0;count];
        let mut tracks=Vec::new();
        tracks.try_reserve_exact(track_count.saturating_sub(1))
            .map_err(|_|ProgramAudioError::Limit("recording track allocation"))?;
        for number in 2..=6 {
            let track=skylive_core::AudioTrackId::new(number).map_err(|_|ProgramAudioError::Invalid("audio track ID"))?;
            if mask.contains(track) {tracks.push(ProgramAudioTrack{track,samples:vec![0.0;count]});}
        }
        let channels=usize::from(self.settings.channels());
        for span in spans {
            let at=start_frame.checked_add(span.offset as u64).ok_or(ProgramAudioError::TimeOverflow)?;
            let mut buffers=Vec::new();
            buffers.try_reserve_exact(span.plan.sources().len()).map_err(|_|ProgramAudioError::Limit("recording input allocation"))?;
            for source in span.plan.sources() {
                let presents=(source.routes.global||source.routes.from||source.routes.to||source.routes.duplicated)
                    &&source.monitoring!=skylive_core::AudioMonitoringMode::MonitorOnly
                    &&source.properties.audio_tracks().intersects(mask);
                let data=if presents {
                    if let Some(alignment)=alignments.iter().find(|a|a.source==source.source && a.revision==span.plan.revision()) {
                        // Before the first confirmed delayed video frame, OBS
                        // has no corresponding camera picture. Preserve the
                        // other sources while the camera's audio warms up.
                        let prefix=usize::try_from(alignment.presentation_sample.saturating_sub(at))
                            .unwrap_or(usize::MAX).min(span.frames);
                        let mut data=vec![0.0;span.frames*channels];
                        let available=if alignment.is_ndi() {
                            usize::try_from(alignment.confirmed_until_sample.saturating_sub(at))
                                .unwrap_or(usize::MAX).min(span.frames)
                        } else {span.frames};
                        if prefix<available {
                            let begin=at.checked_add(prefix as u64).ok_or(ProgramAudioError::TimeOverflow)?;
                            let (tail,missing)=self.read_recording_camera_source(alignment,recording,clock_epoch,begin,available-prefix)?;
                            if missing!=0 && !alignment.is_ndi() {return Err(ProgramAudioError::Backpressure("recording camera history is incomplete"));}
                            if missing==0 {data[prefix*channels..available*channels].copy_from_slice(&tail);}
                        }
                        data
                    } else if matches!(&source.binding,AudioInputKey::Ndi {..})
                            && (recovering.contains(&source.source)||self.camera_delay_history.iter().any(|(id,_)|*id==source.source))
                        || streaming && self.camera_delay_history.iter().any(|(id,_)|*id==source.source) {
                        // Before an AVC receipt arrives, emit only this delayed
                        // source as silence. Other sources continue; waiting for
                        // audio here would deadlock the mux before video proof.
                        vec![0.0;span.frames*channels]
                    } else {
                        let mut data=vec![0.0;span.frames*channels];
                        self.sync.read(source.source,&source.binding,i128::from(at),span.frames,channels,
                            &self.histories,&self.media_controls,&mut data)?;
                        data
                    }
                } else {vec![0.0;span.frames*channels]};
                buffers.push(data);
            }
            let inputs:Vec<_>=span.plan.sources().iter().zip(&buffers).map(|(source,data)|PcmInput::Converted {
                source:source.source,sample_rate:self.settings.sample_rate(),channels:self.settings.channels(),samples:data,
                original:self.groups.get(&source.binding).map(|group|group.converter.input_format()),
            }).collect();
            let request=||AudioMixRequest{sample_rate:self.settings.sample_rate(),channels:self.settings.channels(),frames:span.frames,advance_ns:span.advance_ns};
            let gain=|source,frame|{let blocked=self.push.silent(source,frame);self.histories.gain_at(source,frame).map(|(gain,muted)|(gain,muted||blocked)).or(blocked.then_some((0.0,true)))};
            let mixed=span.plan.mix_processed_on_grid(request(),&inputs,at,&gain)?;
            samples[span.offset*channels..(span.offset+span.frames)*channels].copy_from_slice(&mixed.samples);
            for output in &mut tracks {
                let mixed=span.plan.mix_processed_on_grid_track(request(),&inputs,at,&gain,output.track)?;
                output.samples[span.offset*channels..(span.offset+span.frames)*channels].copy_from_slice(&mixed.samples);
            }
        }
        if alignments.iter().any(|alignment|!alignment.current()) {
            return Err(ProgramAudioError::Invalid("recording camera changed during mix"));
        }
        Ok(RecordingAudioMix{true_peak_revision,start_frame,frames,samples,tracks})
    }
    /// Read one camera's processed PCM for a recording without advancing the
    /// shared Program cursor or mutating its source history. This is a narrow
    /// building block for a separate recording mix: callers must still route
    /// the returned samples through that recording's mixer and own its output
    /// cursor. A recording acknowledgement applies only at and after its
    /// presentation sample; it cannot retroactively justify earlier blocks.
    pub(crate) fn read_recording_camera_source(
        &self,
        alignment: &crate::audio_capture::RecordingCameraAlignment,
        recording: u64,
        clock_epoch: u64,
        start_frame: u64,
        frames: usize,
    ) -> Result<(Vec<f32>, usize), ProgramAudioError> {
        if self.has_retired_window_input() {return Err(ProgramAudioError::Invalid("window audio generation changed before recording camera read"));}
        if self.has_retired_ndi_input() {return Err(ProgramAudioError::Invalid("NDI audio generation changed before recording source read"));}
        if frames == 0 || frames > self.limits.max_block_frames {
            return Err(ProgramAudioError::Limit("recording camera reader block frames"));
        }
        if recording == 0 || alignment.recording != recording
            || self.timeline.session() != Some(alignment.session)
            || alignment.clock_epoch != clock_epoch
            || alignment.sample_rate != self.settings.sample_rate()
            || start_frame < alignment.presentation_sample
            || !alignment.current() {
            return Err(ProgramAudioError::Invalid("recording camera alignment is stale or outside its scope"));
        }
        let reserved = self.camera_delay_history.iter()
            .find_map(|(source, frames)| (*source == alignment.source).then_some(*frames))
            .ok_or(ProgramAudioError::Invalid("recording camera history is unavailable"))?;
        if u128::from(alignment.delay_samples) > reserved as u128 {
            return Err(ProgramAudioError::Limit("recording camera delay exceeds retained source history"));
        }
        let channels = usize::from(self.settings.channels());
        let samples = frames.checked_mul(channels)
            .ok_or(ProgramAudioError::Limit("recording camera reader sample overflow"))?;
        let mut result = Vec::new();
        result.try_reserve_exact(samples)
            .map_err(|_| ProgramAudioError::Limit("recording camera reader allocation"))?;
        result.resize(samples, 0.0);
        let spans = self.timeline.segments(start_frame, frames)?;
        let mut missing = 0usize;
        for span in spans {
            if span.plan.revision() != alignment.revision {
                return Err(ProgramAudioError::Invalid("recording camera route revision changed"));
            }
            let Some(source) = span.plan.sources().iter().find(|source| source.source == alignment.source) else {
                continue;
            };
            let at = i128::from(start_frame)
                .checked_add(span.offset as i128)
                .and_then(|frame| frame.checked_sub(i128::from(alignment.delay_samples)))
                .ok_or(ProgramAudioError::TimeOverflow)?;
            let slice = &mut result[span.offset * channels..(span.offset + span.frames) * channels];
            let gap = self.sync.read(source.source, &source.binding, at, span.frames, channels,
                &self.histories, &self.media_controls, slice)?;
            missing = missing.checked_add(gap).ok_or(ProgramAudioError::TimeOverflow)?;
        }
        if !alignment.current() {
            return Err(ProgramAudioError::Invalid("recording camera generation changed during read"));
        }
        Ok((result, missing))
    }
    pub(crate) fn push_audio_tail(&mut self,session:u64,source:Id,epoch:skylive_core::SourceDefinitionEpoch,operation:skylive_core::SourceAudioAction,until:MediaTime)->Result<(),ProgramAudioError>{
        if self.timeline.session()!=Some(session){return Err(ProgramAudioError::Invalid("push audio session changed"));}
        if !self.push_epochs.iter().any(|(id,proof)|*id==source&&*proof==Some(epoch)){return Ok(());}
        let action=match operation{skylive_core::SourceAudioAction::PushToMute=>push_gate::PushAction::Mute,skylive_core::SourceAudioAction::PushToTalk=>push_gate::PushAction::Talk,_=>return Err(ProgramAudioError::Invalid("not a push audio action"))};
        let until=u64::try_from((u128::from(until.0)*u128::from(self.settings.sample_rate())).div_ceil(1_000_000_000)).map_err(|_|ProgramAudioError::TimeOverflow)?;
        let frame=self.cursor.unwrap_or(0);if until<=frame{return Ok(());}
        let changed=self.push.restore_tail(source,frame,action,until).map_err(ProgramAudioError::Limit)?;
        if changed{self.histories.cut_push_sidechain(source,frame);}Ok(())
    }
    pub(crate) fn push_audio_edge(&mut self,session:u64,source:Id,epoch:skylive_core::SourceDefinitionEpoch,operation:skylive_core::SourceAudioAction,pressed:bool,at:MediaTime,revoked:bool)->Result<(),ProgramAudioError>{
        if self.timeline.session()!=Some(session){return Err(ProgramAudioError::Invalid("push audio session changed"));}
        if !self.push_epochs.iter().any(|(id,proof)|*id==source&&*proof==Some(epoch)){return Ok(());}
        let action=match operation{skylive_core::SourceAudioAction::PushToMute=>push_gate::PushAction::Mute,skylive_core::SourceAudioAction::PushToTalk=>push_gate::PushAction::Talk,_=>return Err(ProgramAudioError::Invalid("not a push audio action"))};
        let frame=u64::try_from(u128::from(at.0)*u128::from(self.settings.sample_rate())/1_000_000_000).map_err(|_|ProgramAudioError::TimeOverflow)?.max(self.cursor.unwrap_or(0));
        let affects=self.push.settings(source).is_some_and(|settings|match operation{skylive_core::SourceAudioAction::PushToMute=>settings.mute_enabled(),skylive_core::SourceAudioAction::PushToTalk=>settings.talk_enabled(),_=>false});
        let changed=if revoked{self.push.revoke(source,frame,action)}else{self.push.edge(source,frame,action,pressed)}.map_err(ProgramAudioError::Limit)?;
        if changed&&affects{self.histories.cut_push_sidechain(source,frame);}Ok(())
    }
    pub fn new(
        settings: AudioSettings,
        limits: ProgramAudioLimits,
    ) -> Result<Self, ProgramAudioError> {
        if limits.max_groups == 0
            || limits.max_groups > 32
            || limits.max_buffer_ms == 0
            || limits.max_buffer_ms > 2000
            || limits.max_block_frames == 0
            || limits.max_block_frames > 1024
            || limits.max_bytes > 128 * 1024 * 1024
        {
            return Err(ProgramAudioError::Invalid("Program audio limits"));
        }
        let channels = usize::from(settings.channels());
        let reserved_bytes = 24 * 1024 * 1024 + PROGRAM_RESIDENT_BYTES + sync_offset::WORKSPACE_BYTES + push_gate::RESERVATION
            + 64 * limits.max_block_frames * channels * 4
            + limits.max_block_frames * channels * 40
            + 2 * MAX_AUDIO_BLOCK_SOURCES * std::mem::size_of::<(Id,usize)>()
            + render_metadata_bytes(channels)?;
        let media_controls = media_controls::MediaControls::new()?;
        if reserved_bytes
            .checked_add(media_controls.bytes())
            .is_none_or(|bytes| bytes > limits.max_bytes)
        {
            return Err(ProgramAudioError::Limit(
                "routing and render workspace reservation",
            ));
        }
        let output = AudioFormat::new(
            settings.sample_rate(),
            settings.channels(),
            settings.channel_mask(),
        )?;
        let instance = NEXT_INSTANCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| ProgramAudioError::Limit("Program instance identity"))?;
        Ok(Self {
            settings,
            output,
            limits,
            timeline: AudioRouteTimeline::new(settings.sample_rate())?,
            groups: HashMap::new(),
            fences: HashMap::new(),
            window_guards: HashMap::new(),
            true_peak_meters:crate::audio::true_peak::SourceTruePeaks::default(),
            master_true_peak:crate::audio::true_peak::MasterTruePeak::default(),
            ndi_guards: HashMap::new(),
            media_controls,
            cursor: None,
            instance,
            epoch: 1,
            data_generation: 0,
            reserved_bytes,
            buffered: false,
            capture_bindings: Vec::new(),
            monitor_frame:None,
            dsp: AudioDspState::default(),
            dsp_streams: HashMap::new(),
            sync: sync_offset::SourceSync::default(),
            histories:post_dsp::Histories::default(),
            push:push_gate::Bank::default(),
            push_epochs:Vec::new(),
            camera_gates:camera_gate::Gates::default(),
            camera_program_reader:true,active_output_tracks:skylive_core::SourceAudioTrackMask::NONE.with(skylive_core::AudioTrackId::ONE,true),
            monitor_video: Vec::new(),
            camera_delay_history: Vec::new(),
            output_delay: false,
            recording_camera_alignments: Vec::new(),
            streaming_alignments: Vec::new(),
            recording_history: None,
            streaming_history: None,
            monitor_dsp: AudioDspState::default(),
            monitor_lanes: Vec::new(),
        })
    }
    pub fn take_monitor_frame(&mut self)->Option<crate::audio_monitor::MonitorFrame>{self.monitor_frame.take()}
    pub fn status(&self) -> ProgramAudioStatus {
        ProgramAudioStatus {
            instance: self.instance,
            epoch: self.epoch,
            data_generation: self.data_generation,
            session: self.timeline.session(),
            settings: self.settings,
            cursor: self.cursor,
            routing_events: self.timeline.event_count(),
            input_groups: self.groups.len(),
            known_input_streams: self.fences.len(),
            accounted_bytes: self.bytes(),
            continuous_native_transport: false,
        }
    }
    pub fn identity(&self) -> Result<ProgramInputIdentity, ProgramAudioError> {
        Ok(ProgramInputIdentity {
            instance: self.instance,
            epoch: self.epoch,
            session: self.timeline.session().ok_or(ProgramAudioError::Invalid(
                "capture a Program route before admitting PCM",
            ))?,
        })
    }
    pub fn input_cursor(&self, binding: &AudioCaptureBinding) -> Option<ProgramInputCursor> {
        self.fences
            .get(&AudioInputKey::Native(binding.clone()))
            .map(|fence| fence.cursor)
    }
    pub fn media_input_cursor(
        &self,
        source: Id,
        media: &skylive_core::MediaSource,
    ) -> Option<ProgramInputCursor> {
        self.fences
            .get(&AudioInputKey::Media {
                source,
                media: media.clone(),
            })
            .map(|fence| fence.cursor)
    }
    pub fn required_bindings(&self) -> &[RequiredAudioBinding] {
        self.timeline.required_bindings()
    }
    pub fn route(
        &mut self,
        session: &Session,
        output: &FrameComposition,
        at: MediaTime,
    ) -> Result<bool, ProgramAudioError> {
        self.route_snapshot(AudioRouteSnapshot::capture(session, output, at)?)
    }
    pub fn route_snapshot(
        &mut self,
        snapshot: AudioRouteSnapshot,
    ) -> Result<bool, ProgramAudioError> {
        if snapshot.settings() != self.settings {
            return Err(ProgramAudioError::FormatChanged);
        }
        if self.timeline.session().is_some_and(|session|session!=snapshot.session()){
            return Err(ProgramAudioError::Invalid("camera/audio route belongs to another session"));
        }
        let at = snapshot.at();
        if self.cursor.is_some_and(|cursor| {
            (u128::from(at.0) * u128::from(self.settings.sample_rate())).div_ceil(1_000_000_000)
                < u128::from(cursor)
        }) {
            return Err(ProgramAudioError::ClockRewind);
        }
        self.set_camera_activation_inner(snapshot.camera_activation(),true)?;
        // Every allocation is prepared before either the route or source read
        // windows become visible. Old/new rings both count against128MiB.
        let rate=self.settings.sample_rate();
        let frame=u64::try_from((u128::from(snapshot.at().0)*u128::from(rate)).div_ceil(1_000_000_000)).map_err(|_|ProgramAudioError::TimeOverflow)?.max(self.cursor.unwrap_or(0));
        let next_epochs=snapshot.source_epochs().to_vec();
        let update_push=next_epochs!=self.push_epochs||snapshot.processing_controls().any(|source|self.push.settings(source.source)!=Some(source.properties.push()));
        let mut push_changed=Vec::new();
        let push=if update_push{
            let mut candidate=self.push.copy().map_err(ProgramAudioError::Limit)?;
            for source in snapshot.processing_controls(){let settings=source.properties.push();let previous=self.push_epochs.iter().find(|(id,_)|*id==source.source).map(|(_,epoch)|*epoch);let next=next_epochs.iter().find(|(id,_)|*id==source.source).map(|(_,epoch)|*epoch);if previous!=next{candidate.revoke(source.source,frame,push_gate::PushAction::Mute).map_err(ProgramAudioError::Limit)?;candidate.revoke(source.source,frame,push_gate::PushAction::Talk).map_err(ProgramAudioError::Limit)?;push_changed.push(source.source);}else if candidate.settings(source.source).is_some_and(|old|old!=settings){push_changed.push(source.source);}candidate.configure(source.source,rate,frame,settings).map_err(ProgramAudioError::Limit)?;}
            Some(candidate)
        }else{None};
        let sync = self.sync.prepare(&snapshot, self.settings.sample_rate(), self.cursor.unwrap_or(0),&self.camera_gates)?;
        let resident=self.bytes();
        let recording_hold=match self.recording_history {Some(RecordingHistoryStatus::Holding{max_frames,..})=>max_frames,_=>0};
        let streaming_hold=match self.streaming_history {Some(RecordingHistoryStatus::Holding{max_frames,..})=>max_frames,_=>0};
        let mut admission=self.histories.prepare(&snapshot,&sync,self.limits,resident,self.cursor.unwrap_or(0),&self.camera_gates,&self.groups,recording_hold.max(streaming_hold));
        if matches!(&admission,Err(ProgramAudioError::Limit(_))) && streaming_hold!=0 {
            // A new stream reader must not take down an existing recording.
            if let Some(RecordingHistoryStatus::Holding{recording,..})=self.streaming_history {
                self.streaming_history=Some(RecordingHistoryStatus::Overflow{recording});
            }
            admission=self.histories.prepare(&snapshot,&sync,self.limits,resident,self.cursor.unwrap_or(0),&self.camera_gates,&self.groups,recording_hold);
        }
        let history=match admission {
            Ok(history)=>history,
            Err(ProgramAudioError::Limit(_)) if recording_hold!=0=>{
                // Recording retention must not take down the shared Program:
                // abandon that reader and retry ordinary route admission. The
                // recording owner must observe Overflow and stop that sink.
                if let Some(RecordingHistoryStatus::Holding{recording,..})=self.recording_history {
                    self.recording_history=Some(RecordingHistoryStatus::Overflow{recording});
                }
                self.histories.prepare(&snapshot,&sync,self.limits,resident,self.cursor.unwrap_or(0),&self.camera_gates,&self.groups,0)?
            },
            Err(error)=>return Err(error),
        };
        let monitor_video = snapshot.monitor_video().to_vec();
        let mut camera_delay_history=Vec::new();
        camera_delay_history.try_reserve_exact(snapshot.camera_delay_history().len())
            .map_err(|_|ProgramAudioError::Limit("camera delay history metadata"))?;
        camera_delay_history.extend_from_slice(snapshot.camera_delay_history());
        let output_delay=snapshot.plan().sources().iter().any(|source|
            source.monitoring!=skylive_core::AudioMonitoringMode::MonitorOnly
                && (source.routes.global||source.routes.from||source.routes.to||source.routes.duplicated)
                && camera_delay_history.iter().any(|(id,_)|*id==source.source));
        let mut recording_camera_alignments=Vec::new();
        recording_camera_alignments.try_reserve_exact(snapshot.recording_camera_alignments().len())
            .map_err(|_|ProgramAudioError::Limit("recording camera alignment metadata"))?;
        recording_camera_alignments.extend_from_slice(snapshot.recording_camera_alignments());
        let mut streaming_alignments=Vec::new();
        streaming_alignments.try_reserve_exact(snapshot.streaming_alignments().len()).map_err(|_|ProgramAudioError::Limit("streaming alignment metadata"))?;
        streaming_alignments.extend_from_slice(snapshot.streaming_alignments());
        let changed = self.timeline.push_snapshot(snapshot)?;
        self.monitor_video = monitor_video;
        self.camera_delay_history = camera_delay_history;
        self.output_delay = output_delay;
        self.recording_camera_alignments = recording_camera_alignments;
        self.streaming_alignments = streaming_alignments;
        self.sync = sync;
        self.histories.apply(history);
        if let Some(push)=push{self.push=push;}
        self.push_epochs=next_epochs;
        for source in push_changed{self.histories.cut_push_sidechain(source,frame);}
        Ok(changed)
    }
    pub fn reset(&mut self) -> Result<(), ProgramAudioError> {
        let epoch = next(self.epoch)?;
        let generation = next(self.data_generation)?;
        self.timeline.reset()?;
        self.groups.clear();
        self.sync.clear();
        self.histories.clear();
        self.push=push_gate::Bank::default();
        self.push_epochs.clear();
        self.camera_gates.clear();
        self.monitor_video.clear();
        self.camera_delay_history.clear();
        self.output_delay=false;
        self.recording_camera_alignments.clear();
        self.streaming_alignments.clear();
        self.recording_history=None;
        self.streaming_history=None;
        self.monitor_lanes.clear();
        self.monitor_dsp.reset();
        self.fences.clear();
        self.window_guards.clear();
        self.ndi_guards.clear();
        self.media_controls.clear();
        self.capture_bindings.clear();
        self.dsp.reset();
        self.dsp_streams.clear();
        self.cursor = None;
        self.epoch = epoch;
        self.data_generation = generation;
        Ok(())
    }
    pub fn ingest_captured(
        &mut self,
        identity: ProgramInputIdentity,
        packet: &CapturedAudioPacket,
    ) -> Result<AudioIngestReport, ProgramAudioError> {
        if packet.coalesced_packets != 0 {
            return Err(ProgramAudioError::Invalid(
                "coalesced meter packets cannot represent continuous Program input",
            ));
        }
        if packet
            .block
            .frames
            .checked_mul(usize::from(packet.block.format.channels))
            != Some(packet.block.samples.len())
        {
            return Err(ProgramAudioError::Invalid(
                "captured audio frame/sample count mismatch",
            ));
        }
        let duration = duration_ns(packet.block.frames as u64, packet.block.format.sample_rate)?;
        let receive = match packet.timing_origin {
            AudioTimingOrigin::Device => packet.first_sample_media_ns.checked_add(
                i128::from(packet.block.host_received_ns)
                    - i128::from(
                        packet
                            .block
                            .device_timestamp_ns
                            .ok_or(ProgramAudioError::Invalid("device timing has no timestamp"))?,
                    ),
            ),
            AudioTimingOrigin::ReceiveEstimate => {
                packet.first_sample_media_ns.checked_add(duration)
            }
        }
        .ok_or(ProgramAudioError::TimeOverflow)?;
        self.ingest(ProgramInputPacket {
            identity,
            binding: &packet.binding,
            stream_generation: packet.stream_generation,
            sequence: packet.sequence,
            first_media_ns: packet.first_sample_media_ns,
            received_media_ns: receive,
            format: packet.block.format,
            samples: &packet.block.samples,
            flags: packet.block.flags,
        })
    }
    pub fn ingest(
        &mut self,
        packet: ProgramInputPacket<'_>,
    ) -> Result<AudioIngestReport, ProgramAudioError> {
        let binding = AudioInputKey::Native(packet.binding.clone());
        self.ingest_key(KeyInputPacket {
            identity: packet.identity,
            binding: &binding,
            stream_generation: packet.stream_generation,
            sequence: packet.sequence,
            first_media_ns: packet.first_media_ns,
            received_media_ns: packet.received_media_ns,
            format: packet.format,
            samples: packet.samples,
            flags: packet.flags,
        })
    }
    /// Window PCM is keyed by source in addition to the durable selector.
    /// The native owner must open the observed HWND and retain the evidence
    /// token with the stream; never feed independently resolved packets here.
    pub(crate) fn ingest_window_captured(&mut self, identity:ProgramInputIdentity,
        evidence:&crate::audio_capture::window_observation::WindowAudioEvidence,
        packet:&CapturedAudioPacket)->Result<AudioIngestReport,ProgramAudioError> {
        if !evidence.current() {return Err(ProgramAudioError::Invalid("retired window audio observation"));}
        let observation=evidence.observation();
        let source=observation.source();
        if self.window_guards.get(&source).is_some_and(|old|!old.same_generation(evidence)) {
            self.invalidate_window_input(source)?;
        }
        if observation.binding().audio()!=Some(&packet.binding) || packet.coalesced_packets!=0
            || packet.block.frames.checked_mul(usize::from(packet.block.format.channels))!=Some(packet.block.samples.len()) {
            return Err(ProgramAudioError::Invalid("window audio packet does not match its observation"));
        }
        let duration=duration_ns(packet.block.frames as u64,packet.block.format.sample_rate)?;
        let receive=match packet.timing_origin {
            AudioTimingOrigin::Device=>packet.first_sample_media_ns.checked_add(i128::from(packet.block.host_received_ns)
                -i128::from(packet.block.device_timestamp_ns.ok_or(ProgramAudioError::Invalid("device timing has no timestamp"))?)),
            AudioTimingOrigin::ReceiveEstimate=>packet.first_sample_media_ns.checked_add(duration),
        }.ok_or(ProgramAudioError::TimeOverflow)?;
        let key=AudioInputKey::Window {source,binding:packet.binding.clone()};
        if !evidence.current() {return Err(ProgramAudioError::Invalid("retired window audio observation"));}
        if !self.window_guards.contains_key(&source) && self.window_guards.len()>=crate::audio::MAX_AUDIO_INPUTS {
            return Err(ProgramAudioError::Limit("window audio generation guards"));
        }
        self.window_guards.insert(source,evidence.clone());
        self.ingest_key(KeyInputPacket {identity,binding:&key,stream_generation:packet.stream_generation,
            sequence:packet.sequence,first_media_ns:packet.first_sample_media_ns,received_media_ns:receive,
            format:packet.block.format,samples:&packet.block.samples,flags:packet.block.flags})
    }
    /// Called by the audio owner at a window-observation generation change,
    /// before accepting any PCM for the replacement process. This invalidates
    /// source-owned raw and post-DSP history; already submitted output blocks
    /// are governed separately by the output owner's lifecycle.
    pub(crate) fn invalidate_window_input(&mut self,source:Id)->Result<(),ProgramAudioError> {
        let generation=next(self.data_generation)?;
        self.groups.retain(|key,_| !matches!(key,AudioInputKey::Window {source:id,..} if *id==source));
        self.fences.retain(|key,_| !matches!(key,AudioInputKey::Window {source:id,..} if *id==source));
        self.histories.invalidate_window(source,self.cursor.unwrap_or(0));
        self.dsp.reset_source(source);
        self.dsp_streams.remove(&source);
        self.window_guards.remove(&source);
        self.data_generation=generation;
        Ok(())
    }
    /// Recheck tokens at the actual render boundary, not just at capture.
    /// This cannot revoke a block already submitted to an external sink.
    pub(crate) fn purge_retired_window_inputs(&mut self)->Result<(),ProgramAudioError> {
        let retired:Vec<_>=self.window_guards.iter().filter_map(|(source,guard)|(!guard.current()).then_some(*source)).collect();
        for source in retired {self.invalidate_window_input(source)?;}
        Ok(())
    }
    fn has_retired_window_input(&self)->bool {self.window_guards.values().any(|guard|!guard.current())}
    fn has_retired_ndi_input(&self)->bool {self.ndi_guards.values().any(|guard|!guard.scope_current())}
    fn invalidate_ndi_input(&mut self,source:Id)->Result<(),ProgramAudioError> {
        let generation=next(self.data_generation)?;
        self.groups.retain(|key,_| !matches!(key,AudioInputKey::Ndi {source:id,..} if *id==source));
        self.fences.retain(|key,_| !matches!(key,AudioInputKey::Ndi {source:id,..} if *id==source));
        self.histories.invalidate_ndi(source,self.cursor.unwrap_or(0));
        self.dsp.reset_source(source);self.dsp_streams.remove(&source);self.ndi_guards.remove(&source);
        self.data_generation=generation;Ok(())
    }
    pub(crate) fn purge_retired_ndi_inputs(&mut self)->Result<(),ProgramAudioError> {
        let retired:Vec<_>=self.ndi_guards.iter().filter_map(|(source,guard)|(!guard.scope_current()).then_some(*source)).collect();
        for source in retired {self.invalidate_ndi_input(source)?;}Ok(())
    }
    pub fn ingest_media(
        &mut self,
        packet: MediaProgramInputPacket<'_>,
    ) -> Result<AudioIngestReport, ProgramAudioError> {
        let binding = AudioInputKey::Media {
            source: packet.source,
            media: packet.media.clone(),
        };
        self.ingest_key(KeyInputPacket {
            identity: packet.identity,
            binding: &binding,
            stream_generation: packet.stream_generation,
            sequence: packet.sequence,
            first_media_ns: packet.first_media_ns,
            received_media_ns: packet.received_media_ns,
            format: packet.format,
            samples: packet.samples,
            flags: packet.flags,
        })
    }
    pub fn ingest_ndi(
        &mut self,
        packet: NdiProgramInputPacket<'_>,
    ) -> Result<AudioIngestReport, ProgramAudioError> {
        let binding = AudioInputKey::Ndi {
            source: packet.source,
            ndi: packet.ndi.clone(),
        };
        self.ingest_key(KeyInputPacket {
            identity: packet.identity,
            binding: &binding,
            stream_generation: packet.stream_generation,
            sequence: packet.sequence,
            first_media_ns: packet.first_media_ns,
            received_media_ns: packet.received_media_ns,
            format: packet.format,
            samples: packet.samples,
            flags: packet.flags,
        })
    }
    /// Live NDI packets retain the sender epoch token through raw and
    /// post-DSP history. Never reuse old PCM after that token is retired.
    pub(crate) fn ingest_ndi_scoped(&mut self,identity:ProgramInputIdentity,
        packet:&Arc<crate::ndi::NdiPcmPacket>)->Result<AudioIngestReport,ProgramAudioError> {
        if !packet.scope_current(){return Err(ProgramAudioError::Invalid("retired NDI audio scope"));}
        if self.ndi_guards.get(&packet.source).is_some_and(|old|!old.same_history(packet)) {
            self.invalidate_ndi_input(packet.source)?;
        }
        if !self.ndi_guards.contains_key(&packet.source) && self.ndi_guards.len()>=crate::audio::MAX_AUDIO_INPUTS {
            return Err(ProgramAudioError::Limit("NDI audio generation guards"));
        }
        self.ndi_guards.insert(packet.source,packet.clone());
        self.ingest_ndi(NdiProgramInputPacket {identity,source:packet.source,ndi:&packet.ndi,
            stream_generation:packet.stream_generation,sequence:packet.sequence,
            first_media_ns:packet.first_media_ns,received_media_ns:packet.received_media_ns,
            format:packet.format,samples:&packet.samples,
            flags:NativePacketFlags {timestamp_error:packet.timestamp_estimated,..Default::default()}})
    }
    pub(crate) fn ingest_embedded(&mut self, identity:ProgramInputIdentity, source:Id,
        binding:&skylive_core::VideoCaptureBinding, stream_generation:u64, sequence:u64,
        first_media_ns:i128, received_media_ns:i128, block:&crate::audio_capture::NativePcmBlock, flags:crate::audio_capture::NativePacketFlags)
        ->Result<AudioIngestReport,ProgramAudioError> {
        if !binding.embedded_audio(){return Err(ProgramAudioError::Invalid("camera embedded audio is disabled"));}
        let key=AudioInputKey::EmbeddedCamera {source,binding:binding.video_only()};
        self.ingest_key(KeyInputPacket {identity,binding:&key,stream_generation,sequence,first_media_ns,received_media_ns,
            format:block.format,samples:&block.samples,flags})
    }
    pub fn ingest_browser(&mut self,packet:BrowserProgramInputPacket<'_>)->Result<AudioIngestReport,ProgramAudioError>{
        let binding=AudioInputKey::Browser(packet.key.clone());
        self.ingest_key(KeyInputPacket{identity:packet.identity,binding:&binding,stream_generation:packet.stream_generation,sequence:packet.sequence,first_media_ns:packet.first_media_ns,received_media_ns:packet.received_media_ns,format:packet.format,samples:packet.samples,flags:packet.flags})
    }
    fn ingest_key(
        &mut self,
        packet: KeyInputPacket<'_>,
    ) -> Result<AudioIngestReport, ProgramAudioError> {
        if packet.identity != self.identity()? {
            return Err(ProgramAudioError::Invalid("stale Program PCM identity"));
        }
        let admitted = self.groups.contains_key(packet.binding)
            || self
                .timeline
                .required_bindings()
                .iter()
                .any(|required| &required.binding == packet.binding)
            || self.buffered
                && packet
                    .binding
                    .as_native()
                    .is_some_and(|binding| self.capture_bindings.contains(binding));
        if !admitted {
            return Err(ProgramAudioError::Invalid(
                "PCM binding is not required by retained Program routes",
            ));
        }
        let channels = usize::from(packet.format.channels);
        let effective_media = matches!(packet.binding, AudioInputKey::Media {..});
        let input = (if effective_media { AudioFormat::new_media_effective } else { AudioFormat::new })(
            packet.format.sample_rate,
            packet.format.channels,
            packet.format.channel_mask,
        )?;
        if packet.stream_generation == 0
            || packet.sequence == 0
            || packet.samples.is_empty()
            || channels == 0
            || !packet.samples.len().is_multiple_of(channels)
            || packet.samples.len() / channels > 4096
            || packet.samples.iter().any(|sample| !sample.is_finite())
            || packet.flags.silent && packet.samples.iter().any(|sample| *sample != 0.0)
        {
            return Err(ProgramAudioError::Invalid("native Program input packet"));
        }
        if self.media_controls.suspended(packet.binding) {
            return Err(ProgramAudioError::Invalid(
                "PCM source is intentionally suspended",
            ));
        }
        let control_reset = self
            .media_controls
            .reset_frame(packet.binding, packet.stream_generation);
        let frames = packet.samples.len() / channels;
        let issued = next(self.data_generation)?;
        let direct = packet.first_media_ns.abs_diff(packet.received_media_ns) < JUMP_NS as u128;
        let previous = self.groups.get(packet.binding);
        if let Some(fence) = self.fences.get(packet.binding) {
            if packet.stream_generation < fence.cursor.generation
                || packet.stream_generation == fence.cursor.generation
                    && (packet.sequence <= fence.cursor.sequence || fence.sealed)
            {
                return Err(ProgramAudioError::Invalid(
                    "stale, duplicate or sealed native Program stream",
                ));
            }
        } else if self.fences.len() >= 128 {
            return Err(ProgramAudioError::Limit(
                "retained input identity fences; reset the Program epoch",
            ));
        }
        if previous.is_some_and(|group| {
            packet.stream_generation < group.generation
                || packet.stream_generation == group.generation
                    && (packet.sequence <= group.last_sequence || group.converter.stats().finished)
        }) {
            return Err(ProgramAudioError::Invalid(
                "stale, duplicate or sealed native Program stream",
            ));
        }
        let new_group = previous.is_none();
        if new_group && self.groups.len() >= self.limits.max_groups {
            return Err(ProgramAudioError::Limit("conversion groups"));
        }
        let mut adjust = if direct {
            0
        } else {
            previous.map_or(
                packet
                    .received_media_ns
                    .checked_sub(packet.first_media_ns)
                    .ok_or(ProgramAudioError::TimeOverflow)?,
                |group| group.adjust,
            )
        };
        let mut timestamp = packet
            .first_media_ns
            .checked_add(adjust)
            .ok_or(ProgramAudioError::TimeOverflow)?;
        let mut report = AudioIngestReport {
            accepted_frames: frames,
            inferred_channel_layout: packet.format.channel_mask == 0 && channels <= 2,
            ..Default::default()
        };
        let mut reset = previous.is_none_or(|group| {
            group.generation != packet.stream_generation || group.format != packet.format
        }) || packet.flags.discontinuity
            || packet.flags.queue_dropped != 0;
        if let Some(group) = previous {
            let expected = group
                .input_anchor
                .checked_add(duration_ns(group.input_frames, group.format.sample_rate)?)
                .ok_or(ProgramAudioError::TimeOverflow)?;
            let difference = timestamp.abs_diff(expected);
            if !reset && difference < SMOOTH_NS as u128 {
                report.timestamp_smoothed = timestamp != expected;
                timestamp = expected;
            } else if difference > JUMP_NS as u128 && !direct {
                adjust = packet
                    .received_media_ns
                    .checked_sub(packet.first_media_ns)
                    .ok_or(ProgramAudioError::TimeOverflow)?;
                timestamp = packet.received_media_ns;
                reset = true;
                report.timing_reanchored = true;
            } else if difference >= SMOOTH_NS as u128 {
                reset = true;
            }
        }
        let anchor = nearest_frame(timestamp, self.settings.sample_rate())?
            .max(control_reset.map_or(i128::MIN, i128::from));
        let existing_bytes = previous.map_or(0, InputGroup::bytes);
        let remaining = self
            .limits
            .max_bytes
            .checked_sub(self.bytes() - existing_bytes)
            .ok_or(ProgramAudioError::Limit("Program source budget"))?;
        // Any new/reconfigured processor is admitted before replacing the old
        // group. Its old and new buffers both count during this preparation.
        let mut replacement = if reset {
            let capacity = (self.settings.sample_rate() as usize)
                .checked_mul(self.limits.max_buffer_ms)
                .ok_or(ProgramAudioError::Limit("source timeline frames"))?
                / 1000;
            if capacity < self.limits.max_block_frames + 1024 {
                return Err(ProgramAudioError::Limit(
                    "source timeline below two DSP blocks",
                ));
            }
            let ring_bytes =
                SampleRing::required_bytes(capacity, usize::from(self.settings.channels()))?;
            let scratch_bytes = 1024 * usize::from(self.settings.channels()) * 4;
            let available = self
                .limits
                .max_bytes
                .checked_sub(self.bytes())
                .and_then(|bytes| {
                    bytes
                        .checked_sub(ring_bytes + scratch_bytes + std::mem::size_of::<InputGroup>())
                })
                .ok_or(ProgramAudioError::Backpressure(
                    "converter replacement needs bounded preparation workspace",
                ))?;
            let converter = AudioConverter::new(
                input,
                self.output,
                AudioConvertLimits {
                    max_bytes: available.min(16 * 1024 * 1024),
                    max_buffered_input_frames: if effective_media {16384} else {12288},
                    ..Default::default()
                },
            )?;
            let ring = SampleRing::new(capacity, usize::from(self.settings.channels()))?;
            Some(InputGroup {
                converter,
                ring,
                format: packet.format,
                generation: packet.stream_generation,
                dsp_epoch: issued,
                last_sequence: packet.sequence,
                input_anchor: timestamp,
                input_frames: 0,
                adjust,
                output_anchor: anchor,
                output_frames: 0,
                replace_next: true,
                scratch: vec![0.0; 1024 * usize::from(self.settings.channels())],
                buffered: buffered::InputState::default(),
            })
        } else {
            None
        };
        self.data_generation = issued;
        self.fences.insert(
            packet.binding.clone(),
            InputFence {
                cursor: ProgramInputCursor {
                    generation: packet.stream_generation,
                    sequence: packet.sequence,
                },
                sealed: false,
            },
        );
        let mut group = if let Some(mut candidate) = replacement.take() {
            if let Some(mut old) = self.groups.remove(packet.binding) {
                // Complete the old filter tail on its historical timestamp grid
                // before switching this logical input to a new native epoch.
                if control_reset.is_none() {
                    old.converter.finish()?;
                    pump(&mut old, &mut report, self.raw_cursor(packet.binding), self.buffered && !self.histories.has_binding(packet.binding))?;
                }
                candidate.ring = old.ring;
            }
            candidate
        } else {
            self.groups
                .remove(packet.binding)
                .ok_or(ProgramAudioError::Invalid("missing Program input group"))?
        };
        report.discontinuity = reset;
        if report.timing_reanchored {
            group.ring.clear();
        }
        group.adjust = adjust;
        let result = (|| {
            if group.converter.stats().finished {
                return Err(ProgramAudioError::Invalid(
                    "input already sealed; use a new stream generation",
                ));
            }
            let mut consumed = 0;
            while consumed < frames {
                pump(&mut group, &mut report, self.raw_cursor(packet.binding), self.buffered && !self.histories.has_binding(packet.binding))?;
                let take = (frames - consumed)
                    .min(group.converter.input_capacity_frames())
                    .min(4096);
                if take == 0 {
                    return Err(ProgramAudioError::Backpressure("converter input FIFO"));
                }
                group
                    .converter
                    .push(&packet.samples[consumed * channels..(consumed + take) * channels])?;
                consumed += take;
            }
            group.input_frames = group
                .input_frames
                .checked_add(frames as u64)
                .ok_or(ProgramAudioError::TimeOverflow)?;
            group.last_sequence = packet.sequence;
            pump(&mut group, &mut report, self.raw_cursor(packet.binding), self.buffered && !self.histories.has_binding(packet.binding))?;
            Ok(())
        })();
        self.data_generation = issued;
        result?;
        if group.bytes() > remaining {
            return Err(ProgramAudioError::Limit("converted source storage"));
        }
        self.groups.insert(packet.binding.clone(), group);
        self.media_controls
            .accepted(packet.binding, packet.stream_generation);
        self.service_source_processing()?;
        Ok(report)
    }
    pub fn finish_input(
        &mut self,
        identity: ProgramInputIdentity,
        binding: &AudioCaptureBinding,
        generation: u64,
        last_sequence: u64,
    ) -> Result<AudioIngestReport, ProgramAudioError> {
        self.finish_key(
            identity,
            &AudioInputKey::Native(binding.clone()),
            generation,
            last_sequence,
        )
    }
    pub fn finish_media(
        &mut self,
        identity: ProgramInputIdentity,
        source: Id,
        media: &skylive_core::MediaSource,
        generation: u64,
        last_sequence: u64,
    ) -> Result<AudioIngestReport, ProgramAudioError> {
        self.finish_key(
            identity,
            &AudioInputKey::Media {
                source,
                media: media.clone(),
            },
            generation,
            last_sequence,
        )
    }
    fn finish_key(
        &mut self,
        identity: ProgramInputIdentity,
        binding: &AudioInputKey,
        generation: u64,
        last_sequence: u64,
    ) -> Result<AudioIngestReport, ProgramAudioError> {
        if identity != self.identity()?
            || self.fences.get(binding).map(|fence| fence.cursor)
                != Some(ProgramInputCursor {
                    generation,
                    sequence: last_sequence,
                })
        {
            return Err(ProgramAudioError::Invalid(
                "stale Program end-of-input identity",
            ));
        }
        if self.fences.get(binding).is_some_and(|fence| fence.sealed) {
            return Ok(AudioIngestReport::default());
        }
        let issued = next(self.data_generation)?;
        let mut group = self
            .groups
            .remove(binding)
            .ok_or(ProgramAudioError::Invalid("unknown Program input"))?;
        let mut report = AudioIngestReport::default();
        self.data_generation = issued;
        if let Some(fence) = self.fences.get_mut(binding) {
            fence.sealed = true;
        }
        group.converter.finish()?;
        pump(&mut group, &mut report, self.raw_cursor(binding), self.buffered && !self.histories.has_binding(binding))?;
        self.groups.insert(binding.clone(), group);
        self.service_source_processing()?;
        Ok(report)
    }
    pub fn render(
        &mut self,
        start_frame: u64,
        frames: usize,
    ) -> Result<ProgramAudioBlock, ProgramAudioError> {
        self.render_tracks(start_frame,frames,skylive_core::SourceAudioTrackMask::NONE.with(skylive_core::AudioTrackId::ONE,true))
    }
    pub fn render_tracks(&mut self,start_frame:u64,frames:usize,requested:skylive_core::SourceAudioTrackMask)->Result<ProgramAudioBlock,ProgramAudioError>{
        self.purge_retired_window_inputs()?;
        self.purge_retired_ndi_inputs()?;
        let render_mask=requested.with(skylive_core::AudioTrackId::ONE,true);
        if frames == 0 || frames > self.limits.max_block_frames {
            return Err(ProgramAudioError::Limit("Program block frames"));
        }
        if self.cursor.is_some_and(|cursor| cursor != start_frame) {
            return Err(ProgramAudioError::ClockRewind);
        }
        self.service_source_processing()?;
        let end = start_frame
            .checked_add(frames as u64)
            .ok_or(ProgramAudioError::TimeOverflow)?;
        let spans = self.timeline.segments(start_frame, frames)?;
        let source_meter_revision = spans
            .first()
            .map(|span| span.plan.revision())
            .filter(|revision| spans.iter().all(|span| span.plan.revision() == *revision));
        // Metadata can span multiple individually bounded plans. Keep its
        // payload exact and pre-reserved; do not turn the per-plan64 PCM input
        // bound into a false rejection at an otherwise valid scene cut.
        let mut sources = Vec::new();
        sources
            .try_reserve_exact(MAX_AUDIO_BLOCK_SOURCES)
            .map_err(|_| ProgramAudioError::Limit("Program source union allocation"))?;
        for source in spans
            .iter()
            .flat_map(|span| span.plan.sources().iter().map(|source| source.source))
        {
            if !sources.contains(&source) {
                if sources.len() == MAX_AUDIO_BLOCK_SOURCES {
                    return Err(ProgramAudioError::Limit("Program block source union"));
                }
                sources.push(source);
            }
        }
        let sources = sources.into_boxed_slice().into_vec();
        let channels = usize::from(self.settings.channels());
        let track_count=render_mask.bits().count_ones()as usize;
        let count=frames.checked_mul(channels).ok_or(ProgramAudioError::Limit("track bundle sample overflow"))?;
        let output_bytes=count.checked_mul(4).and_then(|n|n.checked_mul(track_count)).ok_or(ProgramAudioError::Limit("track bundle byte overflow"))?;
        let work=spans.iter().try_fold(0usize,|work,span|span.plan.sources().len().checked_mul(span.frames)
            .and_then(|n|n.checked_mul(channels)).and_then(|n|n.checked_mul(track_count*3+1+if span.plan.peak_mode()==skylive_core::MixerPeakMode::TruePeakEstimate {16}else{0})).and_then(|n|work.checked_add(n)))
            .ok_or(ProgramAudioError::Limit("track mix work overflow"))?;
        //64source input buffers + six outputs + one sequential mixer workspace
        // fit the existing16MiB render reservation. No sixfold source/DSP copies.
        let meter_enabled=spans.iter().any(|span|span.plan.peak_mode()==skylive_core::MixerPeakMode::TruePeakEstimate);
        let meter_workspace=spans.iter().filter(|span|span.plan.peak_mode()==skylive_core::MixerPeakMode::TruePeakEstimate)
            .try_fold(0usize,|maximum,span|span.plan.sources().len().checked_mul(span.frames)
                .and_then(|n|n.checked_mul(channels)).and_then(|n|n.checked_mul(8)).map(|n|maximum.max(n)))
            .ok_or(ProgramAudioError::Limit("true-peak workspace overflow"))?;
        let workspace=count.checked_mul(4).and_then(|n|n.checked_mul(crate::audio::MAX_AUDIO_INPUTS+track_count+12))
            .and_then(|n|n.checked_add(meter_workspace))
            .and_then(|n|n.checked_add(if meter_enabled {crate::audio::true_peak::MAX_METER_STATE_BYTES}else{0}))
            .and_then(|n|n.checked_add(render_metadata_bytes(channels).ok()?))
            .ok_or(ProgramAudioError::Limit("track bundle workspace overflow"))?;
        if work>64*1024*1024||workspace>16*1024*1024||output_bytes>PROGRAM_RESIDENT_BYTES{
            return Err(ProgramAudioError::Limit("six-track Program work/workspace profile"));
        }
        let mut tracks=Vec::new();tracks.try_reserve_exact(track_count-1).map_err(|_|ProgramAudioError::Limit("track bundle descriptor allocation"))?;
        for number in 2..=6{let track=skylive_core::AudioTrackId::new(number).map_err(|_|ProgramAudioError::Invalid("audio track ID"))?;
            if render_mask.contains(track){tracks.push(ProgramAudioTrack{track,samples:vec![0.0;count]});}
        }
        let mut samples = vec![0.0; frames * channels];
        let monitoring=spans.iter().any(|s|s.plan.has_monitoring());
        let estimated_true_peak=spans.iter().any(|s|s.plan.peak_mode()==skylive_core::MixerPeakMode::TruePeakEstimate);
        let master_true_peak_enabled=!spans.is_empty() && spans.iter().all(|s|s.plan.peak_mode()==skylive_core::MixerPeakMode::TruePeakEstimate) && source_meter_revision.is_some();
        let mut monitor_samples=if monitoring {vec![0.0;frames*channels]}else{Vec::new()};
        type SourceMetricTotals = (Id, ([f64; 8], [f64; 8]));
        let mut source_metrics: Vec<SourceMetricTotals> = Vec::new();
        source_metrics
            .try_reserve_exact(sources.len())
            .map_err(|_| ProgramAudioError::Limit("Program source metrics allocation"))?;
        let mut missing: Vec<(Id, usize)> = Vec::new();
        missing
            .try_reserve_exact(sources.len())
            .map_err(|_| ProgramAudioError::Limit("Program source gaps allocation"))?;
        let mut clipped = vec![0usize; channels];
        let mut ndi_delay_guards=Vec::new();
        // Meter state is private until every route span, output and history
        // operation succeeds. A failed render must not advance interpolation.
        let mut true_peak_meters=if meter_enabled {self.true_peak_meters.clone()} else {crate::audio::true_peak::SourceTruePeaks::default()};
        true_peak_meters.retain(&sources);

        for span in spans {
            let mut buffers = Vec::with_capacity(span.plan.sources().len());
            for source in span.plan.sources() {
                let mut data = vec![0.0; span.frames * channels];
                let presents=(source.routes.global||source.routes.from||source.routes.to||source.routes.duplicated)
                    &&source.monitoring!=skylive_core::AudioMonitoringMode::MonitorOnly
                    &&source.properties.audio_tracks().intersects(render_mask);
                let required=source.properties.audio_tracks().intersects(requested);
                let at=i128::from(start_frame)+span.offset as i128;
                let delayed_ndi=presents
                    && self.camera_delay_history.iter().any(|(id,_)|*id==source.source);
                let gap=if delayed_ndi {
                    // Delayed camera and NDI PCM require a live frame proof.
                    // The observed age is a source-PTS difference, not an
                    // assertion about output presentation or network latency.
                    // Sink-specific recording/streaming readers still use
                    // their own video acknowledgements and do not use this mix.
                    let program_ns=(u128::from(start_frame)+span.offset as u128).checked_mul(1_000_000_000)
                        .map(|value|value/u128::from(self.settings.sample_rate())).ok_or(ProgramAudioError::TimeOverflow)?;
                    let proof=self.monitor_video.iter().find(|stamp|stamp.source==source.source && stamp.binding==source.binding)
                        .and_then(|stamp|stamp.camera_delay_receipt.as_ref().filter(|receipt| {
                            // A frozen last frame must not keep live PCM going
                            // indefinitely. Network timing remains an estimate;
                            // this only bounds how long old evidence is reused.
                            let expires=u128::from(stamp.at.0)+u128::from(receipt.elapsed_ns)+2_000_000_000;
                            receipt.current() && receipt.elapsed_ns<=21_000_000_000 && program_ns<=expires
                        }));
                    if let Some(receipt)=proof {
                        let shift=i128::try_from((u128::from(receipt.elapsed_ns)*u128::from(self.settings.sample_rate())).div_ceil(1_000_000_000))
                            .map_err(|_|ProgramAudioError::TimeOverflow)?;
                        let delayed_at=at.checked_sub(shift).ok_or(ProgramAudioError::TimeOverflow)?;
                        if required&&self.sync.processing_pending(source.source,&source.binding,delayed_at,span.frames,&self.histories,&self.groups) {
                            return Err(ProgramAudioError::Backpressure("NDI delayed source processing is pending"));
                        }
                        if !ndi_delay_guards.iter().any(|old:&crate::capture::AsyncDelayReceipt|std::sync::Arc::ptr_eq(&old.history_live,&receipt.history_live)) {
                            if ndi_delay_guards.len()>=crate::audio::MAX_AUDIO_INPUTS{return Err(ProgramAudioError::Limit("Program NDI delay proof count"));}
                            ndi_delay_guards.try_reserve(1).map_err(|_|ProgramAudioError::Limit("Program NDI delay proof allocation"))?;
                            ndi_delay_guards.push(receipt.clone());
                        }
                        self.sync.read(source.source,&source.binding,delayed_at,span.frames,channels,
                            &self.histories,&self.media_controls,&mut data)?
                    } else {span.frames}
                } else if presents {
                    if required&&self.sync.processing_pending(source.source,&source.binding,at,span.frames,&self.histories,&self.groups){
                        return Err(ProgramAudioError::Backpressure("source post-DSP catch-up is pending; output prefix is unchanged"));
                    }
                    self.sync.read(source.source,&source.binding,at,span.frames,channels,
                        &self.histories,&self.media_controls,&mut data)?
                }else{
                    // Monitor-only/non-Program roots cannot require future
                    // Program PCM or inject missing-capture gaps for an unused
                    // offset. Live phones consume their separate wall grid.
                    let ring=self.histories.ring_epoch(source.source,&source.binding,self.camera_gates.scope(source.source));
                    let gap=if let Some(ring)=ring{ring.read(at,span.frames,&mut data)?}else{span.frames};
                    self.media_controls.apply(&source.binding,ring,at,span.frames,channels,&mut data,gap)
                };
                if gap != 0 && required {
                    if let Some((_, count)) =
                        missing.iter_mut().find(|(id, _)| *id == source.source)
                    {
                        *count += gap;
                    } else {
                        missing.push((source.source, gap));
                    }
                }
                buffers.push(data);
            }
            let inputs: Vec<_> = span
                .plan
                .sources()
                .iter()
                .zip(&buffers)
                .map(|(source, data)| PcmInput::Converted {
                    source: source.source,
                    sample_rate: self.settings.sample_rate(),
                    channels: self.settings.channels(),
                    samples: data,
                    original: self
                        .groups
                        .get(&source.binding)
                        .map(|group| group.converter.input_format()),
                })
                .collect();
            let request = AudioMixRequest {
                    sample_rate: self.settings.sample_rate(),
                    channels: self.settings.channels(),
                    frames: span.frames,
                    advance_ns: span.advance_ns,
                };
            let at_grid=start_frame.checked_add(span.offset as u64).ok_or(ProgramAudioError::TimeOverflow)?;
            let gain_at=|source,frame|{let blocked=self.push.silent(source,frame);self.histories.gain_at(source,frame).map(|(gain,muted)|(gain,muted||blocked)).or(blocked.then_some((0.0,true)))};
            let mut true_peaks:Vec<(Id,[f64;crate::audio::MAX_AUDIO_CHANNELS])>=Vec::new();
            let mut mixed=if span.plan.peak_mode()==skylive_core::MixerPeakMode::TruePeakEstimate {
                // Tap data is bounded by the Program workspace admission above.
                // The mixing loop emits source-major, frame-major interleaved
                // samples, already post-DSP and post-gain.
                let mut tapped:Vec<(Id,Vec<f64>)>=Vec::new();
                tapped.try_reserve_exact(span.plan.sources().len()).map_err(|_|ProgramAudioError::Limit("true-peak source tap allocation"))?;
                let count=span.frames.checked_mul(channels).ok_or(ProgramAudioError::TimeOverflow)?;
                for source in span.plan.sources(){
                    let mut values=Vec::new();values.try_reserve_exact(count).map_err(|_|ProgramAudioError::Limit("true-peak sample tap allocation"))?;
                    if values.capacity()!=count {return Err(ProgramAudioError::Limit("true-peak sample allocation exceeds admitted workspace"));}
                    tapped.push((source.source,values));
                }
                let mut current:Option<(Id,usize)>=None;
                let mut tap=|source:Id,_frame:u64,_channel:usize,value:f64| {
                    let index=match current {Some((id,index)) if id==source=>Some(index),_=>{
                        let found=tapped.iter().position(|(id,_)|*id==source);current=found.map(|index|(source,index));found
                    }};
                    if let Some(index)=index {tapped[index].1.push(value);}
                };
                let mixed=span.plan.mix_processed_on_grid_metered(request,&inputs,at_grid,&gain_at,&mut tap)?;
                drop(tap);
                true_peaks.try_reserve_exact(tapped.len()).map_err(|_|ProgramAudioError::Limit("true-peak result allocation"))?;
                for (source,values) in tapped {
                    if values.len()!=count {return Err(ProgramAudioError::Invalid("incomplete post-gain meter samples"));}
                    let generation=(span.plan.revision(),span.plan.sources().iter().find(|entry|entry.source==source)
                        .and_then(|entry|self.groups.get(&entry.binding)).map_or(0,|group|group.generation));
                    let peaks=true_peak_meters.observe(source,generation,self.settings.sample_rate(),channels,at_grid,&values)?;
                    true_peaks.push((source,peaks));
                }
                mixed
            }else{
                // Do not interpolate across time in which the meter was off.
                true_peak_meters.clear();
                span.plan.mix_processed_on_grid(request,&inputs,at_grid,&gain_at)?
            };
            for meter in &mut mixed.source_meters {
                if let Some((_,peaks))=true_peaks.iter().find(|(source,_)|*source==meter.source) {
                    for (channel,channel_meter) in meter.channels.iter_mut().enumerate(){channel_meter.peak=peaks[channel];}
                }
            }
            samples[span.offset * channels..(span.offset + span.frames) * channels]
                .copy_from_slice(&mixed.samples);
            if !mixed.monitor_samples.is_empty() {monitor_samples[span.offset*channels..(span.offset+span.frames)*channels].copy_from_slice(&mixed.monitor_samples);}
            for track in &mut tracks{
                let mixed_track=span.plan.mix_processed_on_grid_track(AudioMixRequest{
                    sample_rate:self.settings.sample_rate(),channels:self.settings.channels(),frames:span.frames,advance_ns:span.advance_ns},
                    &inputs,start_frame+span.offset as u64,&|source,frame|{let blocked=self.push.silent(source,frame);self.histories.gain_at(source,frame).map(|(gain,muted)|(gain,muted||blocked)).or(blocked.then_some((0.0,true)))},track.track)?;
                track.samples[span.offset*channels..(span.offset+span.frames)*channels].copy_from_slice(&mixed_track.samples);
            }
            for meter in mixed.source_meters {
                let index = if let Some(index) = source_metrics
                    .iter()
                    .position(|(id, _)| *id == meter.source)
                {
                    index
                } else {
                    source_metrics.push((meter.source, ([0.0; 8], [0.0; 8])));
                    source_metrics.len() - 1
                };
                let metrics = &mut source_metrics[index].1;
                for (channel, meter) in meter.channels.iter().enumerate() {
                    metrics.0[channel] = metrics.0[channel].max(meter.peak);
                    metrics.1[channel] += meter.rms * meter.rms * span.frames as f64;
                }
            }
            for (count, addition) in clipped.iter_mut().zip(mixed.clipped_samples) {
                *count += addition;
            }
        }
        let mut peaks = [0.0f64; 8];
        let mut squares = [0.0f64; 8];
        for frame in samples.chunks_exact(channels) {
            for (channel, value) in frame.iter().enumerate() {
                let value = f64::from(*value);
                peaks[channel] = peaks[channel].max(value.abs());
                squares[channel] += value * value;
            }
        }
        let mut master_true_peak = self.master_true_peak.clone();
        if master_true_peak_enabled {
            peaks = master_true_peak.observe((self.instance, self.epoch), self.settings.sample_rate(),
                channels, start_frame, &samples)?;
        } else {
            master_true_peak.clear();
        }
        let program_meters = (0..channels)
            .map(|channel| ChannelMeter {
                peak: peaks[channel],
                rms: (squares[channel] / frames as f64).sqrt(),
            })
            .collect();
        let mut source_meters: Vec<_> = source_metrics
            .into_iter()
            .map(|(source, (peaks, squares))| SourceMeter {
                source,
                channels: (0..channels)
                    .map(|channel| ChannelMeter {
                        peak: peaks[channel],
                        rms: (squares[channel] / frames as f64).sqrt(),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
            .into_vec();
        // The neural-meter shortcut exposes aggregate sample peaks, not PCM;
        // it cannot replace the intersample estimate in true-peak mode.
        if !estimated_true_peak {self.histories.update_neural_meters(&mut source_meters,&self.push);}
        source_meters.sort_by_key(|meter| meter.source.get());
        let mut gaps: Vec<_> = missing
            .into_iter()
            .map(|(source, missing_frames)| ProgramGap {
                source,
                missing_frames,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
            .into_vec();
        gaps.sort_by_key(|gap| gap.source.get());
        self.active_output_tracks=requested;
        if ndi_delay_guards.iter().any(|receipt|!receipt.current()) || self.has_retired_ndi_input() {
            return Err(ProgramAudioError::Invalid("NDI delay generation changed while rendering Program audio"));
        }
        self.retire_output_to(end, false)?;
        self.true_peak_meters=true_peak_meters;
        self.master_true_peak=master_true_peak;

        self.monitor_frame=monitoring.then(||crate::audio_monitor::MonitorFrame {settings:self.settings,start_frame,samples:monitor_samples});
        Ok(ProgramAudioBlock {
            instance: self.instance,
            epoch: self.epoch,
            session: self
                .timeline
                .session()
                .ok_or(ProgramAudioError::Invalid("Program has no routing session"))?,
            data_generation: self.data_generation,
            routing_serial: self.timeline.routing_serial(),
            settings: self.settings,
            start_frame,
            frames,
            samples,
            tracks,
            program_meters,
            source_meters,
            source_meter_revision,
            clipped_samples: clipped,
            gaps,
        })
    }
    /// Explicitly retire skipped output after suspension. Work is bounded by
    /// retained route/group counts, not the number of skipped sample frames.
    /// No PCM output buffers or intermediate render blocks are produced.
    pub fn skip_to(&mut self, frame: u64) -> Result<(), ProgramAudioError> {
        if self.cursor.is_some_and(|cursor| frame < cursor) {
            return Err(ProgramAudioError::ClockRewind);
        }
        if self.cursor == Some(frame) {
            return Ok(());
        }
        let generation = next(self.data_generation)?;
        // Reuse exact route/time/curve validation for this one destination.
        // segments allocates only bounded routing metadata, never PCM buffers.
        drop(self.timeline.segments(frame, 1)?);
        self.retire_output_to(frame, true)?;
        self.dsp.reset();
        self.dsp_streams.clear();
        self.data_generation = generation;
        Ok(())
    }
    fn retire_output_to(&mut self, frame: u64, force_pcm: bool) -> Result<(), ProgramAudioError> {
        if let Some(RecordingHistoryStatus::Holding{recording,floor,max_frames})=self.recording_history {
            if force_pcm {self.recording_history=Some(RecordingHistoryStatus::Interrupted{recording});}
            else if frame.saturating_sub(floor)>max_frames as u64 {self.recording_history=Some(RecordingHistoryStatus::Overflow{recording});}
        }
        if let Some(RecordingHistoryStatus::Holding{recording,floor,max_frames})=self.streaming_history {
            if force_pcm {self.streaming_history=Some(RecordingHistoryStatus::Interrupted{recording});}
            else if frame.saturating_sub(floor)>max_frames as u64 {self.streaming_history=Some(RecordingHistoryStatus::Overflow{recording});}
        }
        let recording_floor=match self.recording_history {Some(RecordingHistoryStatus::Holding{floor,..})=>floor,_=>frame};
        let streaming_floor=match self.streaming_history {Some(RecordingHistoryStatus::Holding{floor,..})=>floor,_=>frame};
        let retain=recording_floor.min(streaming_floor).min(frame);
        self.timeline.retire_before(retain)?;
        self.retire_sources_to(frame, force_pcm,retain);
        let histories=&self.histories;
        self.push.retire(retain,|source|histories.has_source(source)).map_err(ProgramAudioError::Limit)?;
        Ok(())
    }
    fn retire_sources_to(&mut self, frame: u64, force_pcm: bool,retain:u64) {
        for (binding, group) in &mut self.groups {
            if self.histories.has_binding(binding) {
                if let Some(cursor)=self.histories.raw_floor(binding){group.ring.discard_before(i128::from(cursor));}
            } else if !self.buffered||force_pcm {group.ring.discard_before(i128::from(frame));}
        }
        let monitor=&self.monitor_lanes;
        let camera_delay_history=&self.camera_delay_history;
        self.histories.retire(retain,&self.sync,self.timeline.required_bindings(),&|source,binding,epoch|
            {
                let monitor_floor=monitor.iter().find_map(|lane|lane.reader_floor(source,binding,epoch));
                let delay_floor=camera_delay_history.iter().find_map(|(id,frames)|
                    (*id==source).then(||i128::from(retain).saturating_sub(*frames as i128)));
                match (monitor_floor,delay_floor) {(Some(a),Some(b))=>Some(a.min(b)),(Some(a),None)=>Some(a),(None,Some(b))=>Some(b),_=>None}
            });
        self.histories.retire_camera_unread_tracks(&self.sync,&self.timeline,self.active_output_tracks,retain);
        let required: HashSet<_> = self
            .timeline
            .required_bindings()
            .iter()
            .map(|required| &required.binding)
            .collect();
        self.media_controls.retire_with_sync(retain, &required, &self.sync);
        self.sync.retire(retain,&self.camera_delay_history);
        let rate = self.settings.sample_rate();
        let buffered = self.buffered;
        let capture = &self.capture_bindings;
        self.groups.retain(|binding, group| {
            required.contains(binding)
                || buffered
                    && binding
                        .as_native()
                        .is_some_and(|native| capture.contains(native))
                || !buffered
                    && (group.ring.end() > i128::from(frame)
                        || duration_ns(group.input_frames, group.format.sample_rate)
                            .ok()
                            .and_then(|duration| group.input_anchor.checked_add(duration))
                            .and_then(|time| nearest_frame(time, rate).ok())
                            // Uncertain retirement cannot release a processor which
                            // might still own future samples. Keep its bounded charge.
                            .is_none_or(|pending| pending > i128::from(frame)))
        });
        self.cursor = Some(frame);
    }
    pub(crate) fn sync_pending_sources(&self,cursor:u64,frames:usize)->u32 {
        self.sync.pending_sources_routed(cursor,frames,&self.histories,&self.groups,&self.fences,
            &|source,binding|self.timeline.source_routes_between(source,binding,cursor,cursor.saturating_add(1),self.active_output_tracks))
    }
    pub(crate) fn sync_source_shortfall(&self, cursor:u64, frames:usize)->bool {
        self.sync.future_shortfall_routed(cursor,frames,&self.histories,&self.groups,&self.fences,
            &|source,binding|self.timeline.source_routes_between(source,binding,cursor,cursor.saturating_add(1),self.active_output_tracks))
    }
    pub(crate) fn program_output_tracks(&mut self,tracks:skylive_core::SourceAudioTrackMask){
        self.active_output_tracks=tracks;let cursor=self.camera_cursor();
        self.histories.retire_camera_unread_tracks(&self.sync,&self.timeline,tracks,cursor);
    }
    pub(crate) fn sync_lookahead_frames(&self) -> u64 {
        let cursor=self.camera_cursor();self.sync.negative_frames_routed(&|source,binding|
            self.timeline.source_routes_between(source,binding,cursor,cursor.saturating_add(1),self.active_output_tracks))
    }
    pub(crate) fn source_processing_lookahead_frames(&self)->u64{
        let cursor=self.camera_cursor();
        self.sync.source_lookahead_routed(&|source,binding|
            self.timeline.source_routes_between(source,binding,cursor,cursor.saturating_add(1),self.active_output_tracks),
            &|source,binding,epoch|self.histories.processing_lookahead(source,binding,epoch))
    }
    pub(crate) fn service_source_processing(&mut self)->Result<(),ProgramAudioError>{
        self.histories.service(&self.groups,&self.fences,&self.push)?;
        for(binding,group)in &mut self.groups{
            if self.histories.has_binding(binding){
                let floor=self.histories.raw_floor(binding).map(i128::from).unwrap_or_else(||group.ring.end());
                group.ring.discard_before(floor);
            }
        }
        Ok(())
    }
    fn raw_cursor(&self,binding:&AudioInputKey)->Option<u64>{
        self.histories.raw_floor(binding).or(self.cursor)
    }
    fn bytes(&self) -> usize {
        self.reserved_bytes
            + self.media_controls.bytes()
            + self.histories.bytes()
            + self.camera_gates.bytes()
            + self.groups.values().map(InputGroup::bytes).sum::<usize>()
    }
}
fn next(value: u64) -> Result<u64, ProgramAudioError> {
    value
        .checked_add(1)
        .ok_or(ProgramAudioError::Limit("Program identity exhausted"))
}
fn duration_ns(frames: u64, rate: u32) -> Result<i128, ProgramAudioError> {
    if rate == 0 {
        return Err(ProgramAudioError::Invalid("zero input sample rate"));
    }
    Ok((u128::from(frames) * 1_000_000_000 / u128::from(rate)) as i128)
}
fn nearest_frame(time: i128, rate: u32) -> Result<i128, ProgramAudioError> {
    let numerator = time
        .checked_mul(i128::from(rate))
        .ok_or(ProgramAudioError::TimeOverflow)?;
    if numerator >= 0 {
        Ok(numerator
            .checked_add(500_000_000)
            .ok_or(ProgramAudioError::TimeOverflow)?
            / 1_000_000_000)
    } else {
        Ok(-numerator
            .checked_neg()
            .and_then(|value| value.checked_add(500_000_000))
            .ok_or(ProgramAudioError::TimeOverflow)?
            / 1_000_000_000)
    }
}
fn pump(
    group: &mut InputGroup,
    report: &mut AudioIngestReport,
    cursor: Option<u64>,
    buffered: bool,
) -> Result<(), ProgramAudioError> {
    let channels = usize::from(group.converter.output_format().channels());
    for _ in 0..64 {
        let frames = group.converter.pull(&mut group.scratch)?;
        if frames == 0 {
            return Ok(());
        }
        let start = group
            .output_anchor
            .checked_add(i128::from(group.output_frames))
            .ok_or(ProgramAudioError::TimeOverflow)?;
        let late = if let Some(cursor) =
            cursor.filter(|cursor| !buffered && i128::from(*cursor) > start)
        {
            usize::try_from(
                i128::from(cursor)
                    .checked_sub(start)
                    .ok_or(ProgramAudioError::TimeOverflow)?
                    .min(frames as i128),
            )
            .map_err(|_| ProgramAudioError::TimeOverflow)?
        } else {
            0
        };
        if late < frames {
            let placement = if buffered && !group.replace_next {
                group
                    .ring
                    .queue_append(start, &group.scratch[..frames * channels])?
            } else {
                group.ring.place(
                    start
                        .checked_add(late as i128)
                        .ok_or(ProgramAudioError::TimeOverflow)?,
                    &group.scratch[late * channels..frames * channels],
                    group.replace_next,
                )?
            };
            group.replace_next = false;
            report.gap_frames += placement.gap;
            report.overlap_frames += placement.overlap;
        }
        group.output_frames = group
            .output_frames
            .checked_add(frames as u64)
            .ok_or(ProgramAudioError::TimeOverflow)?;
        report.output_frames += frames;
        report.late_frames += late;
    }
    Err(ProgramAudioError::Limit("converter drain work"))
}

#[cfg(test)]
#[path = "audio_program/media_control_tests.rs"]
mod media_control_tests;
