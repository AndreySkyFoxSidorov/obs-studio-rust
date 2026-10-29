//! Bounded native audio acquisition; authored bindings remain in core.
use serde::Serialize;
use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget};
use std::fmt;

pub mod callback;
pub mod native;
mod pcm;
pub use pcm::{PcmEncoding, decode_pcm};
mod worker;
pub(crate) mod window_observation;
pub use window_observation::WindowAudioObservation;
mod embedded_transport;
pub use embedded_transport::{EmbeddedPcmPacket, EmbeddedPcmSink};
pub use worker::{
    AudioCaptureEvent, AudioCaptureProblem, AudioCaptureSnapshot, AudioCaptureWorker,
    AudioRequestKey, AudioTimingOrigin, CapturedAudioPacket, ProgramCaptureKey,
    ProgramCaptureOutput, ProgramTransportProblem, ProgramTransportStats, PushAudioSink,
    RecordingCameraAlignment,
};
#[cfg(test)]
mod continuous_tests;
#[cfg(test)]
mod worker_tests;

#[derive(Clone, Copy, Debug)]
pub struct AudioCaptureLimits {
    pub max_devices: usize,
    pub max_streams: usize,
    pub max_sources: usize,
    pub max_frames: usize,
    pub max_channels: usize,
    pub max_packet_bytes: usize,
    pub max_native_buffer_bytes: usize,
    pub max_batch_bytes: usize,
    pub max_resident_bytes: usize,
    pub callback_slots: usize,
}
impl Default for AudioCaptureLimits {
    fn default() -> Self {
        Self {
            max_devices: 64,
            max_streams: 16,
            max_sources: 3 * skylive_core::MAX_SOURCES,
            max_frames: 4096,
            max_channels: 8,
            max_packet_bytes: 128 * 1024,
            max_native_buffer_bytes: 4 * 1024 * 1024,
            max_batch_bytes: 8 * 1024 * 1024,
            max_resident_bytes: 32 * 1024 * 1024,
            callback_slots: 8,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct AudioCapabilities {
    pub backend: &'static str,
    pub input: bool,
    pub output: bool,
    pub device_enumeration: bool,
    pub exclude_own_audio: bool,
    pub note: Option<&'static str>,
}
/// Platform source defaults are host policy, never inferred by a view from an
/// OS name, backend label or device enumeration index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct AudioBindingDefaults {
    pub input_device_timing: bool,
    pub output_device_timing: bool,
    pub output_exclude_own_audio: bool,
}
impl Default for AudioBindingDefaults {
    fn default() -> Self {
        Self {
            input_device_timing: false,
            output_device_timing: true,
            output_exclude_own_audio: false,
        }
    }
}
impl AudioBindingDefaults {
    pub fn device_timing(self, flow: AudioFlow) -> bool {
        match flow {
            AudioFlow::Input => self.input_device_timing,
            AudioFlow::Output => self.output_device_timing,
        }
    }
    pub fn exclude_own_audio(self, flow: AudioFlow) -> bool {
        flow == AudioFlow::Output && self.output_exclude_own_audio
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AudioDeviceInfo {
    pub target: AudioTarget,
    pub name: String,
    pub flow: AudioFlow,
    pub is_default: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioCaptureError {
    Unsupported(&'static str),
    PermissionDenied(&'static str),
    DeviceUnavailable(String),
    Invalid(&'static str),
    Limit(&'static str),
    Backpressure(&'static str),
    Native {
        operation: &'static str,
        detail: String,
    },
    Busy,
    Closed,
    Disconnected,
    TimeOverflow,
    ProgramRetired(String),
}
impl AudioCaptureError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unsupported(_) => "audio_capture_unsupported",
            Self::PermissionDenied(_) => "audio_capture_permission_denied",
            Self::DeviceUnavailable(_) => "audio_capture_device_unavailable",
            Self::Invalid(_) => "audio_capture_invalid",
            Self::Limit(_) => "audio_capture_limit",
            Self::Backpressure(_) => "audio_capture_backpressure",
            Self::Native { .. } => "audio_capture_native",
            Self::Busy => "audio_capture_busy",
            Self::Closed => "audio_capture_closed",
            Self::Disconnected => "audio_capture_disconnected",
            Self::TimeOverflow => "audio_capture_time_overflow",
            Self::ProgramRetired(_) => "audio_program_retired",
        }
    }
}
impl fmt::Display for AudioCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(detail)
            | Self::PermissionDenied(detail)
            | Self::Invalid(detail)
            | Self::Limit(detail)
            | Self::Backpressure(detail) => write!(f, "{}: {detail}", self.code()),
            Self::DeviceUnavailable(detail) | Self::ProgramRetired(detail) => {
                write!(f, "{}: {detail}", self.code())
            }
            Self::Native { operation, detail } => {
                write!(f, "{}: {operation}: {detail}", self.code())
            }
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for AudioCaptureError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct NativeAudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub channel_mask: u32,
}
impl NativeAudioFormat {
    pub fn validate(self, limits: AudioCaptureLimits) -> Result<(), AudioCaptureError> {
        if !(8000..=192000).contains(&self.sample_rate)
            || self.channels == 0
            || usize::from(self.channels) > limits.max_channels
            || self.channels > 8
        {
            return Err(AudioCaptureError::Unsupported(
                "native audio rate/channels exceed 8k..192kHz/8ch",
            ));
        }
        if self.channel_mask & !0x3ffff != 0
            || (self.channel_mask != 0
                && self.channel_mask.count_ones() != u32::from(self.channels))
        {
            return Err(AudioCaptureError::Invalid(
                "channel mask does not match native channel count/standard positions",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NativePacketFlags {
    pub silent: bool,
    pub discontinuity: bool,
    pub timestamp_error: bool,
    pub queue_dropped: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct NativePcmBlock {
    pub format: NativeAudioFormat,
    pub frames: usize,
    pub samples: Vec<f32>,
    pub device_timestamp_ns: Option<u64>,
    /// DirectShow graph-relative stream time. Its epoch is the owning graph's
    /// Run generation; never interpret it as a host or WASAPI timestamp.
    pub graph_timestamp_ns: Option<i128>,
    #[serde(serialize_with = "serialize_directshow_clock")]
    pub directshow_clock: Option<crate::camera_capture::DirectShowClockSnapshot>,
    pub host_received_ns: u64,
    pub device_frame_position: Option<u64>,
    pub flags: NativePacketFlags,
}
// The media clock stays independent of serialization; expose its complete
// observation at this host diagnostic boundary, preserving ordinary Option JSON.
fn serialize_directshow_clock<S: serde::Serializer>(
    clock: &Option<crate::camera_capture::DirectShowClockSnapshot>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    struct ClockFields {
        graph_id: u64,
        audio_clock_generation: u64,
        video_clock_generation: u64,
        run_start_100ns: i64,
        reference_100ns: i64,
        qpc_before_ns: u64,
        qpc_after_ns: u64,
        previous: Option<(i64, u64, u64)>,
    }
    clock.as_ref().map(|clock| {
        let crate::camera_capture::DirectShowClockSnapshot {
            graph_id, audio_clock_generation, video_clock_generation,
            run_start_100ns, reference_100ns, qpc_before_ns, qpc_after_ns, previous,
        } = *clock;
        ClockFields {
            graph_id, audio_clock_generation, video_clock_generation,
            run_start_100ns, reference_100ns, qpc_before_ns, qpc_after_ns, previous,
        }
    }).serialize(serializer)
}
impl NativePcmBlock {
    pub fn validate(&self, limits: AudioCaptureLimits) -> Result<(), AudioCaptureError> {
        self.format.validate(limits)?;
        if self.frames == 0 || self.frames > limits.max_frames || self.frames > 4096 {
            return Err(AudioCaptureError::Limit("native packet frames"));
        }
        let count = self
            .frames
            .checked_mul(usize::from(self.format.channels))
            .ok_or(AudioCaptureError::Limit("native PCM count"))?;
        if self.samples.len() != count {
            return Err(AudioCaptureError::Invalid("native PCM sample count"));
        }
        if count
            .checked_mul(4)
            .is_none_or(|bytes| bytes > limits.max_packet_bytes)
            || self
                .samples
                .capacity()
                .checked_mul(4)
                .is_none_or(|bytes| bytes > limits.max_packet_bytes)
        {
            return Err(AudioCaptureError::Limit("native PCM packet bytes/capacity"));
        }
        if self.samples.iter().any(|sample| !sample.is_finite()) {
            return Err(AudioCaptureError::Invalid("non-finite native PCM"));
        }
        if self.flags.silent && self.samples.iter().any(|sample| *sample != 0.0) {
            return Err(AudioCaptureError::Invalid(
                "silent native packet contains signal",
            ));
        }
        Ok(())
    }
}
pub trait NativeAudioBackend {
    fn capabilities(&self) -> AudioCapabilities;
    fn host_now_ns(&self) -> Result<u64, AudioCaptureError>;
    fn enumerate(
        &mut self,
        flow: AudioFlow,
        limits: AudioCaptureLimits,
    ) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError>;
    /// Separate application discovery; never included in native playback endpoint lists.
    fn enumerate_processes(&mut self, _limits: AudioCaptureLimits) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        Err(AudioCaptureError::Unsupported("Windows application audio requires Windows build 20348 or later"))
    }
    fn open(
        &mut self,
        binding: &AudioCaptureBinding,
        limits: AudioCaptureLimits,
    ) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError>;
    /// Open the exact window observed by a live video capture owner. A
    /// caller must additionally fence the returned stream and its PCM by the
    /// video source/session generation; HWND and PID are ephemeral.
    fn open_window_observation(
        &mut self,
        _window: &WindowAudioObservation,
        _limits: AudioCaptureLimits,
    ) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
        Err(AudioCaptureError::Unsupported("exact window audio capture is unavailable on this backend"))
    }
}
pub trait NativeAudioStream {
    /// Resolved friendly label, queried once by the acquisition owner after open.
    fn description(&self) -> Option<std::sync::Arc<str>> {
        None
    }
    fn poll(&mut self) -> Result<Option<NativePcmBlock>, AudioCaptureError>;
    fn close(&mut self) -> Result<(), AudioCaptureError>;
}
pub type AudioBackendFactory =
    Box<dyn FnOnce() -> Result<Box<dyn NativeAudioBackend>, AudioCaptureError> + Send>;
