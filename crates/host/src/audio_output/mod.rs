//! Staged owner-to-owner Program PCM fanout. No native handles or GUI PCM work.
/// Producer wall-clock skew only: original Program sample timestamps do not
/// move. Includes negative source sync lookahead plus user buffering <=1s.
pub const MAX_AUDIO_EMISSION_SKEW_NS:u64=2_000_000_000;
mod router;
mod streaming;
#[cfg(test)]
mod tests;
use crate::{audio_capture::ProgramCaptureKey, audio_program::ProgramAudioBlock};
pub use router::{AudioOutput, ProgramOutputRouter};
use skylive_core::AudioSettings;
use skylive_media::MediaTime;
use std::{fmt, sync::Arc};
#[cfg(test)]
pub(crate) use streaming::channel_with_pool as channel_for_test;
#[cfg(test)]
pub(crate) use streaming::channel_with_pool_track as channel_for_test_track;
pub use streaming::{
    AudioDelivery, StreamingAudioControl, StreamingAudioReceiver, StreamingAudioSink,
    StreamingAudioStats, streaming_channel, streaming_channel_track,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    Recording,
    Streaming,
    ReplayBuffer,
    NdiProgram,
}
impl OutputKind {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Recording => 0,
            Self::Streaming => 1,
            Self::ReplayBuffer=>2,
            Self::NdiProgram=>3,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputKey {
    pub kind: OutputKind,
    pub session: u64,
    pub id: u64,
    pub epoch: u64,
}
impl OutputKey {
    pub fn validate(self) -> Result<(), OutputError> {
        if self.session == 0 || self.id == 0 || self.epoch == 0 {
            Err(OutputError::Invalid("zero output identity"))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputAnchor {
    media_origin_ns: u64,
    first_sample_frame: u64,
    sample_rate: u32,
}
impl OutputAnchor {
    pub fn new(media_origin: MediaTime, settings: AudioSettings) -> Result<Self, OutputError> {
        let sample_rate = settings.sample_rate();
        let frame = (u128::from(media_origin.0) * u128::from(sample_rate)).div_ceil(1_000_000_000);
        let first_sample_frame = u64::try_from(frame)
            .map_err(|_| OutputError::Invalid("output sample anchor overflow"))?;
        Ok(Self {
            media_origin_ns: media_origin.0,
            first_sample_frame,
            sample_rate,
        })
    }
    pub fn frame_at_or_after(self,at:MediaTime)->Result<u64,OutputError>{
        u64::try_from((u128::from(at.0)*u128::from(self.sample_rate)).div_ceil(1_000_000_000)).map_err(|_|OutputError::Invalid("output cutoff grid overflow"))
    }
    pub fn media_origin_ns(self) -> u64 {
        self.media_origin_ns
    }
    pub fn first_sample_frame(self) -> u64 {
        self.first_sample_frame
    }
    pub fn sample_rate(self) -> u32 {
        self.sample_rate
    }
    /// Observed sample-grid position relative to the SAME media origin as video.
    /// Native AAC priming remains a separate signed encoder timestamp offset.
    pub fn sample_pts_100ns(self, absolute_frame: u64) -> Result<i64, OutputError> {
        let absolute = u128::from(absolute_frame) * 10_000_000 / u128::from(self.sample_rate);
        let relative = i128::try_from(absolute)
            .map_err(|_| OutputError::Invalid("sample PTS overflow"))?
            - i128::from(self.media_origin_ns / 100);
        i64::try_from(relative).map_err(|_| OutputError::Invalid("sample PTS overflow"))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFault {
    Scope,
    Order,
    Backpressure,
    Invalid,
    Disconnected,
    Gap,
}
#[derive(Debug)]
pub enum OutputError {
    Invalid(&'static str),
    Limit(&'static str),
    DuplicateKind(OutputKind),
    Fault(OutputFault),
    Recording(crate::recording::RecordingError),
}
impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(s) | Self::Limit(s) => f.write_str(s),
            Self::DuplicateKind(kind) => write!(f, "{kind:?} Program output is already attached"),
            Self::Fault(fault) => write!(f, "Program output fault: {fault:?}"),
            Self::Recording(error) => fmt::Display::fmt(error, f),
        }
    }
}
impl std::error::Error for OutputError {}
pub(crate) fn same_format(a: AudioSettings, b: AudioSettings) -> bool {
    a.sample_rate() == b.sample_rate() && a.speakers() == b.speakers()
}
pub(crate) fn accepts(
    old: Option<ProgramCaptureKey>,
    new: ProgramCaptureKey,
    session: u64,
    settings: AudioSettings,
) -> bool {
    new.session == session
        && new.clock_epoch != 0
        && new.generation != 0
        && same_format(new.settings, settings)
        && old.is_none_or(|old| {
            old == new
                || (old.session == new.session
                    && old.clock_epoch == new.clock_epoch
                    && old.generation < new.generation
                    && same_format(old.settings, new.settings)
                    && old.settings.buffering() != new.settings.buffering())
        })
}
pub(crate) fn block_charge(block: &ProgramAudioBlock) -> Result<usize, OutputError> {
    crate::recording::program_block_charge(block).map_err(OutputError::Recording)
}
pub(crate) fn shared_identity(block: &Arc<ProgramAudioBlock>) -> (u64, u64) {
    (block.instance, block.epoch)
}
