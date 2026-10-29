//! Unique owner-to-owner PCM producer. It owns no native handle. Its bounded
//! ingress survives GUI close until the real delayed Program cutoff/Drop;
//! native/file finalization remains entirely on the recording worker.
use super::{
    RecordingError,
    owner::{Ingress, Work},
};
use crate::{audio_capture::ProgramCaptureKey, audio_program::ProgramAudioBlock};
use skylive_core::AudioSettings;
use std::{
    mem::size_of,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
};

pub const MAX_QUEUED_AUDIO_BLOCKS: usize = 40;
pub const MAX_QUEUED_AUDIO_BYTES: usize = 5 * 1024 * 1024;
// A block may cross historical route spans: the timeline retains128 source
// bindings, although each individual AudioPlan mixes at most64 source IDs.
const MAX_BLOCK_SOURCE_METADATA: usize = crate::audio::MAX_AUDIO_BLOCK_SOURCES;

pub(super) struct AudioState {
    pub tracks:skylive_core::SourceAudioTrackMask,
    pub id: u64,
    pub session: u64,
    pub registered: AtomicBool,
    pub manual: AtomicBool,
    pub finish: AtomicBool,
    pub cutoff_ns:AtomicU64,
    pub pause_coverage:Mutex<PauseCoverage>,
    pub observed_end:AtomicU64,
    pub video_end_ns:AtomicU64,
    pub ended: AtomicBool,
    fault: AtomicU8,
    count: AtomicUsize,
    bytes: AtomicUsize,
}
impl AudioState {
    pub fn new(id:u64,session:u64)->Self{Self::new_tracks(id,session,skylive_core::SourceAudioTrackMask::NONE.with(skylive_core::AudioTrackId::ONE,true))}
    pub fn new_tracks(id:u64,session:u64,tracks:skylive_core::SourceAudioTrackMask)->Self{
        Self {
            tracks,
            id,
            session,
            registered: AtomicBool::new(false),
            manual: AtomicBool::new(false),
            finish: AtomicBool::new(false),
            cutoff_ns:AtomicU64::new(u64::MAX),pause_coverage:Mutex::new(PauseCoverage::default()),observed_end:AtomicU64::new(0),video_end_ns:AtomicU64::new(0),
            ended: AtomicBool::new(false),
            fault: AtomicU8::new(0),
            count: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
        }
    }
    pub fn fault(&self) -> Option<RecordingError> {
        let (code, detail) = match self.fault.load(Ordering::Acquire) {
            0 => return None,
            1 => (
                "recording_audio_backpressure",
                "Recording audio ingress is full; the accepted prefix was stopped",
            ),
            2 => (
                "recording_audio_scope",
                "Recording Program audio session, format or timeline changed",
            ),
            3 => (
                "recording_audio_order",
                "Recording Program audio is overlapping or out of order",
            ),
            4 => (
                "recording_audio_invalid",
                "Recording Program audio admission is invalid",
            ),
            _ => (
                "recording_audio_disconnected",
                "Recording audio producer disconnected",
            ),
        };
        Some(RecordingError {
            code,
            detail: detail.into(),
            recovery_path: None,
        })
    }
    fn fail(&self, code: u8) {
        let _ = self
            .fault
            .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
        self.finish.store(true, Ordering::Release);
    }
    pub fn permit(self: &Arc<Self>, bytes: usize) -> Result<AudioPermit, RecordingError> {
        self.count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_QUEUED_AUDIO_BLOCKS).then_some(count + 1)
            })
            .map_err(|_| backpressure())?;
        if self
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|&total| total <= MAX_QUEUED_AUDIO_BYTES)
            })
            .is_err()
        {
            self.count.fetch_sub(1, Ordering::AcqRel);
            return Err(backpressure());
        }
        Ok(AudioPermit {
            state: self.clone(),
            bytes,
        })
    }
}
/// Shared by the recording owner and its one PCM producer. Resume holds the
/// same lock as a discard and extends its effective cut past discarded frames.
#[derive(Default)]
pub(super) struct PauseCoverage {pub cut_ns:Option<u64>,pub discarded_end:u64}
pub(super) struct AudioPermit {
    state: Arc<AudioState>,
    bytes: usize,
}
impl Drop for AudioPermit {
    fn drop(&mut self) {
        self.state.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        self.state.count.fetch_sub(1, Ordering::AcqRel);
    }
}
fn backpressure() -> RecordingError {
    RecordingError {
        code: "recording_backpressure",
        detail: "recording audio count or byte queue is full".into(),
        recovery_path: None,
    }
}

/// Deliberately not Clone: Drop on the one audio owner is the end barrier after
/// its final accepted send. An Arc of control state is not another producer.
pub struct RecordingAudioSink {
    ingress: Arc<Ingress>,
    pub(super) state: Arc<AudioState>,
    settings: AudioSettings,
    key: Option<ProgramCaptureKey>,
    identity: Option<(u64, u64)>,
    end: Option<u64>,
}
impl RecordingAudioSink {
    pub(super) fn new(
        ingress: Arc<Ingress>,
        state: Arc<AudioState>,
        settings: AudioSettings,
    ) -> Self {
        Self {
            ingress,
            state,
            settings,
            key: None,
            identity: None,
            end: None,
        }
    }
    pub(crate) fn selected_tracks(&self)->skylive_core::SourceAudioTrackMask{self.state.tracks}
    pub(crate) fn stop_requested(&self)->bool {self.state.cutoff_ns.load(Ordering::Acquire)!=u64::MAX}
    /// Only frames at or after this sample are certainly inside an acknowledged
    /// pause. A block crossing the cut must still reach the recording owner so
    /// it can retain the prefix on all selected tracks.
    pub(crate) fn discard_fully_paused(&self,start:u64,end:u64)->bool {
        let Ok(mut coverage)=self.state.pause_coverage.lock() else{return false;};
        let Some(cut)=coverage.cut_ns else{return false;};
        let Ok(grid)=u64::try_from((u128::from(cut)*u128::from(self.settings.sample_rate())).div_ceil(1_000_000_000)) else{return false;};
        if start<grid || end<=start {return false;}
        coverage.discarded_end=coverage.discarded_end.max(end);
        self.state.observed_end.fetch_max(end,Ordering::AcqRel);
        true
    }
    pub(crate) fn may_submit(&self)->bool {
        !self.finishing() && self.state.count.load(Ordering::Acquire)<MAX_QUEUED_AUDIO_BLOCKS
            && self.state.bytes.load(Ordering::Acquire)<MAX_QUEUED_AUDIO_BYTES
    }
    /// The single audio producer can wait for the recording worker to retire
    /// accepted blocks before submitting a retained backlog. Only the worker
    /// releases these permits, so capacity cannot decrease between this check
    /// and the next submission by this producer.
    pub(crate) fn can_submit(&self,block:&ProgramAudioBlock)->bool {
        if !self.may_submit(){return false;}
        let Ok(bytes)=block_charge(block) else{return false;};
        self.state.count.load(Ordering::Acquire)<MAX_QUEUED_AUDIO_BLOCKS
            && self.state.bytes.load(Ordering::Acquire).checked_add(bytes)
                .is_some_and(|total|total<=MAX_QUEUED_AUDIO_BYTES)
    }
    pub(crate) fn output_key(&self) -> crate::audio_output::OutputKey {
        crate::audio_output::OutputKey {
            kind: crate::audio_output::OutputKind::Recording,
            session: self.state.session,
            id: self.state.id,
            epoch: 1,
        }
    }
    pub(crate) fn finishing(&self) -> bool {
        let cutoff=self.state.cutoff_ns.load(Ordering::Acquire);
        if cutoff!=u64::MAX {
            let grid=(u128::from(cutoff)*u128::from(self.settings.sample_rate())).div_ceil(1_000_000_000);
            if u128::from(self.state.observed_end.load(Ordering::Acquire))>=grid {self.state.finish.store(true,Ordering::Release);}
        }
        self.state.finish.load(Ordering::Acquire)
    }
    pub(crate) fn bind(&mut self, key: ProgramCaptureKey) -> Result<(), RecordingError> {
        let same_format = |a: AudioSettings, b: AudioSettings| {
            a.sample_rate() == b.sample_rate() && a.speakers() == b.speakers()
        };
        let accepted = key.session == self.state.session
            && same_format(key.settings, self.settings)
            && self.key.is_none_or(|old| {
                old == key
                    || old.session == key.session
                        && old.clock_epoch == key.clock_epoch
                        && old.generation < key.generation
                        && same_format(old.settings, key.settings)
                        && old.settings.buffering() != key.settings.buffering()
            });
        if !accepted {
            self.state.fail(2);
            return Err(self
                .state
                .fault()
                .unwrap_or_else(|| RecordingError::invalid("recording Program scope changed")));
        }
        if self.key != Some(key) {
            self.identity = None;
        }
        self.key = Some(key);
        Ok(())
    }
    pub(crate) fn submit(&mut self, key: ProgramCaptureKey, block: Arc<ProgramAudioBlock>) -> bool {
        if self.finishing() {
            return false;
        }
        if self.bind(key).is_err() {
            return false;
        }
        if block.track_mask().bits()&self.state.tracks.bits()!=self.state.tracks.bits(){self.state.fail(4);return false;}
        let identity = (block.instance, block.epoch);
        if block.session != self.state.session
            || block.settings != key.settings
            || self.identity.is_some_and(|old| old != identity)
        {
            self.state.fail(2);
            return false;
        }
        let Some(end) = block.start_frame.checked_add(block.frames as u64) else {
            self.state.fail(4);
            return false;
        };
        if self.end.is_some_and(|old| block.start_frame < old) {
            self.state.fail(3);
            return false;
        }
        let bytes = match block_charge(&block) {
            Ok(bytes) => bytes,
            Err(_) => {
                self.state.fail(4);
                return false;
            }
        };
        let permit = match self.state.permit(bytes) {
            Ok(permit) => permit,
            Err(_) => {
                self.state.fail(1);
                return false;
            }
        };
        let ingress=&self.ingress;
        let work = Work::Audio {
            token: self.state.id,
            block,
            _permit: permit,
        };
        if ingress.send(work).is_err() {
            self.state.fail(1);
            return false;
        }
        self.identity = Some(identity);
        self.end = Some(end);
        self.state.observed_end.store(end,Ordering::Release);
        !self.finishing()
    }
    pub(crate) fn fail_scope(&self) {
        if !self.finishing() {
            self.state.fail(2);
        }
    }
    pub(crate) fn fail_backpressure(&self) {
        self.state.fail(1);
    }
}
impl Drop for RecordingAudioSink {
    fn drop(&mut self) {
        if !self.finishing() {
            self.state.fail(5);
        }
        self.state.ended.store(true, Ordering::Release);
    }
}

pub(crate) fn block_charge(block: &ProgramAudioBlock) -> Result<usize, RecordingError> {
    let channels = usize::from(block.settings.speakers().channels());
    if block.frames == 0
        || block.frames > 4096
        || block.instance == 0
        || block.epoch == 0
        || block.session == 0
        || block.samples.len() != block.frames.checked_mul(channels).unwrap_or(usize::MAX)
        || block.samples.capacity() > 4096 * 8
        || block.source_meters.len() > MAX_BLOCK_SOURCE_METADATA
        || block.source_meters.capacity() > MAX_BLOCK_SOURCE_METADATA
        || block.gaps.len() > MAX_BLOCK_SOURCE_METADATA
        || block.gaps.capacity() > MAX_BLOCK_SOURCE_METADATA
        || block.program_meters.len() > 8
        || block.program_meters.capacity() > 8
        || block.clipped_samples.len() > 8
        || block.clipped_samples.capacity() > 8
        || block
            .source_meters
            .iter()
            .any(|source| source.channels.len() > 8 || source.channels.capacity() > 8)
        || block.samples.iter().any(|sample| !sample.is_finite())
    {
        return Err(RecordingError::invalid(
            "recording Program PCM or metadata admission",
        ));
    }
    let extra=block.additional_track_bytes().map_err(RecordingError::invalid)?;
    let mut bytes = size_of::<ProgramAudioBlock>() + 2 * size_of::<usize>() + extra;
    for (count, size) in [
        (block.samples.capacity(), size_of::<f32>()),
        (
            block.source_meters.capacity(),
            size_of::<crate::audio::SourceMeter>(),
        ),
        (
            block.program_meters.capacity(),
            size_of::<crate::audio::ChannelMeter>(),
        ),
        (
            block.gaps.capacity(),
            size_of::<crate::audio_program::ProgramGap>(),
        ),
        (block.clipped_samples.capacity(), size_of::<usize>()),
    ] {
        bytes = bytes
            .checked_add(
                count
                    .checked_mul(size)
                    .ok_or_else(|| RecordingError::invalid("recording audio byte overflow"))?,
            )
            .ok_or_else(|| RecordingError::invalid("recording audio byte overflow"))?;
    }
    for source in &block.source_meters {
        bytes = bytes
            .checked_add(source.channels.capacity() * size_of::<crate::audio::ChannelMeter>())
            .ok_or_else(|| RecordingError::invalid("recording audio byte overflow"))?;
    }
    Ok(bytes)
}
