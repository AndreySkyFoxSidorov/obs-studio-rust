//! Bounded Windows AAC-LC packet port. macOS/Linux adapters are unavailable.
#[cfg(all(test, target_os = "windows"))]
mod native_tests;
#[cfg(target_os = "windows")]
mod native_windows;
mod pcm;
pub(crate) mod admission;
#[cfg(test)]
mod tests;
mod worker;
pub use pcm::{PcmInput, pcm16_interleaved};
use std::{error::Error as StdError, fmt};
pub use worker::{EncoderWorker, Event, PacketBatch, RejectedInput};
pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_LEN: usize = 1024;
pub const MAX_INPUT_FRAMES: usize = 1024;
pub const MAX_AU_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PACKETS: usize = 4;
pub const MAX_HEADER_BYTES: usize = 64;
#[cfg(any(test, target_os = "windows"))]
pub(crate) const MAX_NATIVE_HEADER_BYTES: usize = 128;
#[cfg(target_os = "windows")]
pub(crate) const MAX_PENDING_BLOCKS: u64 = 4;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation {
    pub session: u64,
    pub output: u64,
    pub encoder: u64,
}
impl Generation {
    fn validate(self) -> Result<(), Error> {
        if self.session == 0 || self.output == 0 || self.encoder == 0 {
            Err(Error::Invalid("zero encoder generation"))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    generation: Generation,
    channels: u16,
    sample_rate:u32,
    bitrate: u32,
}
impl Config {
    pub fn new(generation: Generation, channels: u16, bitrate: u32) -> Result<Self, Error> {
        Self::with_sample_rate(generation,SAMPLE_RATE,channels,bitrate)
    }
    pub(crate) fn with_sample_rate(generation:Generation,sample_rate:u32,channels:u16,bitrate:u32)->Result<Self,Error>{
        generation.validate()?;
        if ![44_100,48_000].contains(&sample_rate){return Err(Error::Invalid("AAC sample rate must be44.1/48kHz"));}
        if !(1..=2).contains(&channels) || ![96_000, 128_000, 160_000, 192_000].contains(&bitrate) {
            return Err(Error::Invalid("AAC44.1/48kHz mono/stereo bitrate profile"));
        }
        Ok(Self {
            generation,
            sample_rate,
            channels,
            bitrate,
        })
    }
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }
    /// Exact boundaries rounded down in100ns; never accumulate a rounded duration.
    pub fn sample_timing(&self, first_frame: u64, frames: u64) -> Result<(i64, i64), Error> {
        if frames == 0 {
            return Err(Error::Invalid("zero PCM duration"));
        }
        let end = first_frame
            .checked_add(frames)
            .ok_or(Error::Invalid("PCM frame overflow"))?;
        let tick = |frame: u64| {
            i64::try_from(u128::from(frame) * 10_000_000 / u128::from(self.sample_rate))
                .map_err(|_| Error::Invalid("PCM timestamp overflow"))
        };
        let start = tick(first_frame)?;
        let end = tick(end)?;
        Ok((start, end - start))
    }
}
/// Validated, preserved native AudioSpecificConfig. This profile accepts the
/// ordinary two-byte AAC-LC44.1/48kHz mono/stereo GASpecificConfig only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecConfiguration {
    asc: Box<[u8]>,
    sample_rate:u32,
    channels: u16,
}
impl CodecConfiguration {
    pub fn from_asc(bytes: &[u8], config: &Config) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > MAX_HEADER_BYTES {
            return Err(Error::Limit("AAC AudioSpecificConfig"));
        }
        let expected=if config.sample_rate()==48_000 {[0x11,0x80|((config.channels() as u8)<<3)]}else{[0x12,(config.channels() as u8)<<3]};
        if bytes != expected {
            return Err(Error::Unavailable(
                "AAC configuration differs from the requested ordinary44.1/48kHz LC mono/stereo",
            ));
        }
        Ok(Self {
            asc: bytes.to_vec().into_boxed_slice(),
            sample_rate:config.sample_rate(),
            channels: config.channels(),
        })
    }
    #[cfg(any(test, target_os = "windows"))]
    pub(crate) fn from_native(bytes: &[u8], config: &Config) -> Result<Self, Error> {
        if !(14..=MAX_NATIVE_HEADER_BYTES).contains(&bytes.len()) {
            return Err(Error::Limit("native HEAACWAVEINFO/ASC"));
        }
        // MF_MT_USER_DATA excludes WAVEFORMATEX. Its12-byte HEAACWAVEINFO
        // suffix is LE payload/profile/struct/reserved1/reserved2 then ASC.
        if u16::from_le_bytes([bytes[0], bytes[1]]) != 0 || bytes[4..12].iter().any(|b| *b != 0) {
            return Err(Error::Unavailable("unsupported HEAACWAVEINFO fields"));
        }
        Self::from_asc(&bytes[12..], config)
    }
    pub fn asc(&self) -> &[u8] {
        &self.asc
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.asc.len()
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub input_frames: u64,
    pub submitted_frames: u64,
    pub explicit_padding_frames: u16,
    pub packets: u64,
    pub first_native_pts: Option<i64>,
    pub last_native_end: Option<i64>,
}
/// Raw AAC AU. Native timing is observed and signed; no guessed priming trim.
#[derive(Debug)]
pub struct AccessUnit {
    generation: Generation,
    sequence: u64,
    bytes: Box<[u8]>,
    pts: i64,
    dts: i64,
    duration: i64,
}
impl AccessUnit {
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn new(
        generation: Generation,
        sequence: u64,
        bytes: Vec<u8>,
        pts: i64,
        dts: i64,
        duration: i64,
    ) -> Result<Self, Error> {
        if bytes.is_empty() || bytes.len() > MAX_AU_BYTES {
            return Err(Error::Limit("AAC access unit"));
        }
        // Audio carries no frame reordering. Preserve negative native priming
        // time if observed; downstream transport must explicitly anchor it.
        validate_native_timing(pts, dts, duration, None)?;
        if bytes.len() >= 2 && bytes[0] == 0xff && (bytes[1] & 0xf6) == 0xf0 {
            return Err(Error::Protocol("ADTS returned for raw AAC output"));
        }
        Ok(Self {
            generation,
            sequence,
            bytes: bytes.into_boxed_slice(),
            pts,
            dts,
            duration,
        })
    }
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn pts(&self) -> i64 {
        self.pts
    }
    pub fn dts(&self) -> i64 {
        self.dts
    }
    pub fn duration(&self) -> i64 {
        self.duration
    }
}
/// IMFSample duration is an observed presentation interval, not the AAC
/// coded-frame count. The LC ASC fixes1024 decoded frames independently. Keep
/// positive checked duration verbatim, even when native rounding overlaps the
/// next PTS by a tick; never reconstruct it from an assumed rounding quantum.
#[cfg(any(target_os = "windows", test))]
fn validate_native_timing(
    pts: i64,
    dts: i64,
    duration: i64,
    previous_pts: Option<i64>,
) -> Result<(), Error> {
    if pts != dts
        || duration <= 0
        || pts.checked_add(duration).is_none()
        || previous_pts.is_some_and(|previous| pts <= previous)
    {
        return Err(Error::Protocol("AAC native timestamp/duration/order"));
    }
    Ok(())
}
#[cfg(any(test, target_os = "windows"))]
fn next_packet_count(packets: u64, accepted_blocks: u64, eos: bool) -> Result<u64, Error> {
    if accepted_blocks == 0 {
        return Err(Error::Protocol("AAC output without accepted PCM"));
    }
    let next = packets
        .checked_add(1)
        .ok_or(Error::Limit("AAC packet counter"))?;
    let maximum = accepted_blocks
        .checked_add(if eos { 2 } else { 0 })
        .ok_or(Error::Limit("AAC accepted counter"))?;
    if next > maximum {
        return Err(Error::Protocol(
            "AAC output exceeds accepted blocks and bounded drain allowance",
        ));
    }
    Ok(next)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Unavailable(&'static str),
    Limit(&'static str),
    Protocol(&'static str),
    Timing {
        pts: i64,
        dts: i64,
        duration: i64,
        previous_pts: Option<i64>,
        accepted_blocks: u64,
        packets: u64,
        native_bytes: u32,
        native_buffers: u32,
    },
    Generation,
    Busy,
    Closed,
    Native {
        operation: &'static str,
        hresult: i32,
        detail: String,
    },
    Worker(String),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(s) | Self::Unavailable(s) | Self::Limit(s) | Self::Protocol(s) => {
                f.write_str(s)
            }
            Self::Timing {
                pts,
                dts,
                duration,
                previous_pts,
                accepted_blocks,
                packets,
                native_bytes,
                native_buffers,
            } => write!(
                f,
                "AAC native timing: pts={pts}, dts={dts}, duration={duration}, previous_pts={previous_pts:?}, accepted_blocks={accepted_blocks}, packets={packets}, native_bytes={native_bytes}, native_buffers={native_buffers}"
            ),
            Self::Generation => f.write_str("AAC encoder generation mismatch"),
            Self::Busy => f.write_str("AAC command or unread reply pending"),
            Self::Closed => f.write_str("AAC encoder closed"),
            Self::Native {
                operation,
                hresult,
                detail,
            } => write!(f, "{operation}: HRESULT {hresult:#010x}: {detail}"),
            Self::Worker(s) => f.write_str(s),
        }
    }
}
impl StdError for Error {}
pub(crate) trait Adapter {
    fn configuration(&self) -> &CodecConfiguration;
    fn stats(&self) -> StreamStats;
    fn encode(&mut self, input: PcmInput) -> Result<Vec<AccessUnit>, Error>;
    fn drain(&mut self) -> Result<Vec<AccessUnit>, Error>;
}
pub(crate) type Factory = Box<dyn FnOnce(&Config) -> Result<Box<dyn Adapter>, Error> + Send>;
pub(crate) fn create_adapter(config:&Config)->Result<Box<dyn Adapter>,Error>{native_factory()(config)}
pub(crate) fn create_reserved(config:&Config,permit:admission::Permit)->Result<Box<dyn Adapter>,Error>{#[cfg(target_os="windows")]{Ok(Box::new(native_windows::Encoder::with_permit(config.clone(),permit)?))}#[cfg(not(target_os="windows"))]{let _=(config,permit);Err(Error::Unavailable("Windows AAC adapter is unavailable"))}}
fn native_factory() -> Factory {
    Box::new(|config| {
        #[cfg(target_os = "windows")]
        {
            Ok(Box::new(native_windows::Encoder::new(config.clone())?))
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = config;
            Err(Error::Unavailable(
                "native AAC packets unavailable on this platform; AudioConverter adapter not verified",
            ))
        }
    })
}

/// Explicit test-only timing trace. Logging never changes validation/acceptance.
#[cfg(all(test, target_os = "windows"))]
fn timing_trace_enabled() -> bool {
    std::env::var("SKYLIVE_NATIVE_AAC_ACCEPTANCE").as_deref() == Ok("1")
        && std::env::var("SKYLIVE_NATIVE_AAC_TIMING_PROBE").as_deref() == Ok("1")
}
