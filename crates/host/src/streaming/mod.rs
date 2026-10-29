//! Keyed worker-owned streaming, bounded native codecs and RTMP publication.
mod audio;
mod backend;
mod config;
#[cfg(all(test, target_os = "windows"))]
mod native_tests;
mod owner;
#[cfg(test)]
mod tests;
mod timing;
mod video;
mod ndi_receipt;
pub use ndi_receipt::NdiVideoReceipt;
mod camera_receipt;
pub use camera_receipt::CameraVideoReceipt;
use crate::audio_output::StreamingAudioSink;
pub use config::Config;
pub use owner::StreamingOwner;
use skylive_core::{AudioSettings, ProfileId, VideoSettings};
use skylive_media::MediaTime;
use std::{fmt, sync::Arc};
pub use timing::{InterleaveState, InterleaveWait};
/// Integration extracts this immutable DTO once; this alias preserves recording.
pub type ProgramVideoFrame = crate::recording::RecordingFrame;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamKey {
    pub session: u64,
    pub output: u64,
}
impl StreamKey {
    pub(crate) fn audio(self) -> crate::audio_output::OutputKey {
        crate::audio_output::OutputKey {
            kind: crate::audio_output::OutputKind::Streaming,
            session: self.session,
            id: self.output,
            epoch: self.output,
        }
    }
    pub(crate) fn generation(self, audio: bool) -> crate::rtmp::Generation {
        crate::rtmp::Generation {
            session: self.session,
            output: self.output,
            encoder: if audio { 2 } else { 1 },
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Initializing,
    Connecting,
    ReconnectRetiring,
    ReconnectWaiting,
    ReadyAwaitingInput,
    Publishing,
    Finishing,
    Retiring,
    Closed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Limit(&'static str),
    Scope,
    Format,
    State(&'static str),
    Cancelled,
    Disconnected,
    Configuration(skylive_core::Error),
    H264(String),
    Aac(String),
    Transport(String),
    RetryableTransport(String),
    Reconnecting,
    AutoTextRetry,
    Render(String),
    Audio(String),
    Worker(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.code())?;
        match self {
            Self::Invalid(text) | Self::Limit(text) | Self::State(text) => f.write_str(text),
            Self::Configuration(error) => fmt::Display::fmt(error, f),
            Self::H264(text)
            | Self::Aac(text)
            | Self::RetryableTransport(text)
            | Self::Transport(text)
            | Self::Render(text)
            | Self::Audio(text)
            | Self::Worker(text) => f.write_str(text),
            Self::Scope => f.write_str("The output identity changed"),
            Self::Format => f.write_str("The active profile or audio/video format changed"),
            Self::Cancelled => f.write_str("The output was cancelled"),
            Self::Reconnecting=>f.write_str("The output is reconnecting; wait for fresh ReadyForInputs"),
            Self::AutoTextRetry=>f.write_str("Automatic text is being prepared; retry this frame"),
            Self::Disconnected => f.write_str("The output worker disconnected"),
        }
    }
}
impl std::error::Error for Error {}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "streaming_invalid",
            Self::Configuration(_) => "streaming_configuration",
            Self::Limit(_) => "streaming_limit",
            Self::Scope => "streaming_scope",
            Self::Format => "streaming_format_changed",
            Self::State(_) => "streaming_state",
            Self::Cancelled => "streaming_cancelled",
            Self::Disconnected => "streaming_disconnected",
            Self::H264(_) => "streaming_h264",
            Self::Aac(_) => "streaming_aac",
            Self::RetryableTransport(_)=>"streaming_transport_disconnected",
            Self::Reconnecting=>"streaming_reconnecting",
            Self::AutoTextRetry=>"streaming_auto_text_retry",
            Self::Transport(_) => "streaming_transport",
            Self::Render(_) => "streaming_render",
            Self::Audio(_) => "streaming_audio",
            Self::Worker(_) => "streaming_worker",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Status {
    pub key: StreamKey,
    pub connection_epoch:u64,
    pub reconnect_attempt:u32,
    pub reconnect_total:u64,
    pub reconnect_wait_ms:u32,
    pub reconnect_last_error:Option<Arc<Error>>,
    pub reconnect_video_discarded:u64,
    pub reconnect_audio_discarded_input:u64,
    pub ever_published:bool,
    pub ready_for_inputs:bool,
    pub connection_started:bool,
    pub phase: Phase,
    pub origin: Option<MediaTime>,
    pub audio_offset_100ns:Option<i64>,
    pub video_frames: u64,
    pub skipped_video_frames: u64,
    pub audio_input_frames: u64,
    pub audio_converted_frames: u64,
    pub audio_gap_frames: u64,
    pub video_packets: u64,
    pub audio_packets: u64,
    pub packets_admitted: u64,
    pub packets_published: u64,
    pub encoded_bytes: u64,
    pub degraded_frames: u64,
    pub warnings: Option<Arc<crate::recording::RecordingWarnings>>,
    pub failure: Option<Arc<Error>>,
    pub native_aac: crate::native_aac::StreamStats,
    pub video_sealed: bool,
    pub audio_sealed: bool,
    pub interleave: InterleaveState,
    /// NDI sources in the latest frame for which this encoder returned the
    /// matching access unit. This does not assert network publication.
    pub(crate) ndi_video_receipts:Vec<NdiVideoReceipt>,
    pub(crate) first_ndi_video_receipts:Vec<NdiVideoReceipt>,
    pub(crate) camera_video_receipts:Vec<CameraVideoReceipt>,
    pub(crate) first_camera_video_receipts:Vec<CameraVideoReceipt>,
}
impl Status {
    pub fn ndi_video_receipts(&self)->&[NdiVideoReceipt]{&self.ndi_video_receipts}
    pub(crate) fn first_ndi_video_receipts(&self)->&[NdiVideoReceipt]{&self.first_ndi_video_receipts}
    pub fn camera_video_receipts(&self)->&[CameraVideoReceipt]{&self.camera_video_receipts}
    pub(crate) fn first_camera_video_receipts(&self)->&[CameraVideoReceipt]{&self.first_camera_video_receipts}
    fn new(key: StreamKey) -> Self {
        Self {
            key,
            connection_epoch:key.output,reconnect_attempt:0,reconnect_total:0,reconnect_wait_ms:0,reconnect_last_error:None,reconnect_video_discarded:0,reconnect_audio_discarded_input:0,ever_published:false,ready_for_inputs:false,connection_started:false,
            phase: Phase::Initializing,
            origin: None,audio_offset_100ns:None,
            video_frames: 0,
            skipped_video_frames: 0,
            audio_input_frames: 0,
            audio_converted_frames: 0,
            audio_gap_frames: 0,
            video_packets: 0,
            audio_packets: 0,
            packets_admitted: 0,
            packets_published: 0,
            encoded_bytes: 0,
            degraded_frames: 0,
            warnings: None,
            failure: None,
            native_aac: Default::default(),
            video_sealed: false,
            audio_sealed: false,
            interleave: InterleaveState::default(),
            ndi_video_receipts:Vec::new(),first_ndi_video_receipts:Vec::new(),
            camera_video_receipts:Vec::new(),first_camera_video_receipts:Vec::new(),
        }
    }
}
#[derive(Clone, Debug)]
pub enum Event {
    ReadyForInputs {
        key: StreamKey,
        epoch: u64,
    },
    Started {
        key: StreamKey,
        epoch: u64,
        origin: MediaTime,
    },
    Stopped(Status),
    Failed(Status),
}
#[derive(Debug)]
pub struct RejectedFrame {
    pub error: Error,
    pub frame: Box<ProgramVideoFrame>,
}
pub struct StartedOwner {
    pub owner: StreamingOwner,
    pub audio_sink: Option<StreamingAudioSink>,
}
fn add(counter: &mut u64, value: u64) -> Result<(), Error> {
    *counter = counter
        .checked_add(value)
        .ok_or(Error::Limit("stream counter overflow"))?;
    Ok(())
}
