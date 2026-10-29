//! Bounded Windows H.264 packet port. macOS/Linux adapters are unavailable.
mod bitstream;
#[cfg(any(target_os = "windows", test))]
#[path = "../h264_color/mod.rs"]
mod color;
// native_macos.rs is retained as audited source only. Do not compile/select it
// until refcon quiescence after failed CompleteFrames is independently proven.
#[cfg(all(test, target_os = "windows"))]
mod native_tests;
#[cfg(target_os = "windows")]
mod native_windows;
#[cfg(test)]
mod tests;
mod worker;
#[cfg(all(target_os="windows",target_arch="x86_64"))]
mod nvenc;
mod encoder_probe;
pub use encoder_probe::{NvencAdapter,NvencCatalog,probe_nvenc,encoder_capabilities};

pub use bitstream::CodecConfiguration;
#[cfg(any(target_os = "windows", test))]
pub use color::Error as ColorError;
use std::{error::Error as StdError, fmt};
pub use worker::{EncoderWorker, Event, PacketBatch, RejectedInput};

pub const MAX_AU_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PACKETS: usize = 4;
pub const MAX_HEADER_BYTES: usize = 32 * 1024;
#[cfg(target_os = "windows")]
pub(crate) const MAX_PENDING: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation {
    pub session: u64,
    pub output: u64,
    pub encoder: u64,
}
impl Generation {
    fn validate(self) -> Result<(), Error> {
        if self.session == 0 || self.output == 0 || self.encoder == 0 {
            Err(Error::Invalid("zero generation"))
        } else {
            Ok(())
        }
    }
}

/// Explicit native bitstream contract. Recording retains High; packet streaming
/// and Replay keep their existing Baseline default. Both forbid reordered AUs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum H264Profile { Baseline, High }
impl H264Profile {pub(crate) fn idc(self)->u32{match self{Self::Baseline=>66,Self::High=>100}}}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    generation: Generation,
    encoder: skylive_core::VideoEncoder,
    profile: H264Profile,
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    bitrate: u32,
    gop: u32,
}
impl Config {
    pub fn new(
        generation: Generation,
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
        bitrate: u32,
        gop: u32,
    ) -> Result<Self, Error> {
        generation.validate()?;
        if !(16..=4096).contains(&width)
            || !(16..=4096).contains(&height)
            || width % 2 != 0
            || height % 2 != 0
            || u64::from(width) * u64::from(height) > 8_847_360
        {
            return Err(Error::Invalid("NV12 dimensions"));
        }
        if fps_den == 0
            || fps_num == 0
            || fps_num > 120_000
            || fps_den > 100_000
            || fps_num < fps_den
            || u64::from(fps_num) > 120 * u64::from(fps_den)
        {
            return Err(Error::Invalid("frame rate must be 1..120 FPS"));
        }
        if !(64_000..=100_000_000).contains(&bitrate) || !(1..=1200).contains(&gop) {
            return Err(Error::Invalid("bitrate or GOP"));
        }
        Ok(Self {
            generation,
            encoder: skylive_core::VideoEncoder::Software,
            profile:H264Profile::Baseline,
            width,
            height,
            fps_num,
            fps_den,
            bitrate,
            gop,
        })
    }
    /// Recording's existing software bitrate range and High/no-B profile.
    /// The ordinary packet constructor deliberately keeps its100Mbps ceiling.
    pub(crate) fn recording(generation:Generation,width:u32,height:u32,fps_num:u32,fps_den:u32,bitrate:u32,gop:u32,encoder:skylive_core::VideoEncoder)->Result<Self,Error>{
        if !(100_000..=200_000_000).contains(&bitrate){return Err(Error::Invalid("recording bitrate"));}
        let mut config=Self::new(generation,width,height,fps_num,fps_den,bitrate.min(100_000_000),gop)?;
        config.bitrate=bitrate;
        if encoder.is_default(){config.profile=H264Profile::High;}
        config.with_encoder(encoder)
    }
    pub fn profile(&self)->H264Profile{self.profile}
    pub fn encoder(&self)->skylive_core::VideoEncoder{self.encoder}
    pub fn with_encoder(mut self,value:skylive_core::VideoEncoder)->Result<Self,Error>{value.validate().map_err(|_|Error::Invalid("video encoder identity"))?;if !value.is_default()&&(self.bitrate>100_000_000||self.profile!=H264Profile::Baseline){return Err(Error::Invalid("NVENC port requires Baseline and bitrate<=100Mbps"));}self.encoder=value;Ok(self)}
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn fps(&self) -> (u32, u32) {
        (self.fps_num, self.fps_den)
    }
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }
    pub fn gop(&self) -> u32 {
        self.gop
    }
    pub fn nv12_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 3 / 2
    }
    /// Exact rational boundaries, explicitly rounded down in native 100 ns units.
    pub fn frame_timing(&self, index: u64) -> Result<(i64, i64), Error> {
        let tick = |i: u128| -> Result<i64, Error> {
            i.checked_mul(10_000_000)
                .and_then(|n| n.checked_mul(u128::from(self.fps_den)))
                .and_then(|n| i64::try_from(n / u128::from(self.fps_num)).ok())
                .ok_or(Error::Invalid("timestamp overflow"))
        };
        let start = tick(u128::from(index))?;
        let end = tick(u128::from(index) + 1)?;
        Ok((start, end - start))
    }
}

#[derive(Debug)]
pub struct Nv12Input {
    generation: Generation,
    width: u32,
    height: u32,
    pts: i64,
    duration: i64,
    bytes: Box<[u8]>,
}
impl Nv12Input {
    pub fn new(config: &Config, pts: i64, duration: i64, bytes: Box<[u8]>) -> Result<Self, Error> {
        if bytes.len() != config.nv12_bytes()
            || pts < 0
            || duration <= 0
            || pts.checked_add(duration).is_none()
        {
            return Err(Error::Invalid("NV12 byte length or timestamp"));
        }
        Ok(Self {
            generation: config.generation,
            width: config.width,
            height: config.height,
            pts,
            duration,
            bytes,
        })
    }
    pub fn pts(&self) -> i64 {
        self.pts
    }
    pub fn duration(&self) -> i64 {
        self.duration
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn validate(&self, config: &Config) -> Result<(), Error> {
        if self.generation != config.generation {
            return Err(Error::Generation);
        }
        if self.width != config.width
            || self.height != config.height
            || self.bytes.len() != config.nv12_bytes()
        {
            return Err(Error::Invalid("input format differs from encoder"));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct AccessUnit {
    generation: Generation,
    bytes: Box<[u8]>,
    keyframe: bool,
    pts: i64,
    dts: i64,
    duration: i64,
}
impl AccessUnit {
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn new(
        generation: Generation,
        bytes: Vec<u8>,
        pts: i64,
        dts: i64,
        duration: i64,
    ) -> Result<Self, Error> {
        if bytes.len() > MAX_AU_BYTES {
            return Err(Error::Limit("access unit"));
        }
        if pts < 0 || dts < 0 || pts != dts || duration <= 0 {
            return Err(Error::Reordered);
        }
        let keyframe = bitstream::validate_access_unit(&bytes)?;
        Ok(Self {
            generation,
            bytes: bytes.into_boxed_slice(),
            keyframe,
            pts,
            dts,
            duration,
        })
    }
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn keyframe(&self) -> bool {
        self.keyframe
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    #[cfg(any(target_os = "windows", test))]
    Color(ColorError),
    Invalid(&'static str),
    Unavailable(&'static str),
    Limit(&'static str),
    Protocol(&'static str),
    Reordered,
    Generation,
    Busy,
    Closed,
    Native {
        operation: &'static str,
        hresult: i32,
        detail: String,
    },
    Platform {
        operation: &'static str,
        code: i32,
    },
    Worker(String),
    Driver {operation:&'static str,code:i32,detail:String},
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(any(target_os = "windows", test))]
            Self::Color(error) => fmt::Display::fmt(error, f),
            Self::Native {
                operation,
                hresult,
                detail,
            } => write!(f, "{operation}: HRESULT {hresult:#010x}: {detail}"),
            Self::Driver {operation,code,detail}=>write!(f,"{operation}: driver status {code}: {detail}"),
            Self::Platform { operation, code } => write!(f, "{operation}: native status {code}"),
            Self::Invalid(s) | Self::Unavailable(s) | Self::Limit(s) | Self::Protocol(s) => {
                f.write_str(s)
            }
            Self::Worker(s) => f.write_str(s),
            Self::Reordered => {
                f.write_str("encoder produced reordered timestamps outside Baseline contract")
            }
            Self::Generation => f.write_str("encoder generation mismatch"),
            Self::Busy => f.write_str("encoder command or unread reply is pending"),
            Self::Closed => f.write_str("encoder is closed"),
        }
    }
}
impl StdError for Error {}
#[cfg(any(target_os = "windows", test))]
impl From<ColorError> for Error {
    fn from(error: ColorError) -> Self {
        Self::Color(error)
    }
}

pub(crate) trait Adapter {
    fn configuration(&self) -> Option<&CodecConfiguration>;
    fn encode(&mut self, input: Nv12Input) -> Result<Vec<AccessUnit>, Error>;
    fn drain(&mut self) -> Result<Vec<AccessUnit>, Error>;
}
pub(crate) type Factory = Box<dyn FnOnce(&Config) -> Result<Box<dyn Adapter>, Error> + Send>;
pub(crate) fn create_adapter(config:&Config)->Result<Box<dyn Adapter>,Error>{native_factory()(config)}
fn native_factory() -> Factory {
    Box::new(|config| {
        #[cfg(target_os = "windows")]
        {
            match config.encoder() {
                skylive_core::VideoEncoder::Software=>Ok(Box::new(native_windows::Encoder::new(config.clone())?)),
                skylive_core::VideoEncoder::NvencH264 {adapter_luid}=>{
                    #[cfg(target_arch="x86_64")] {Ok(Box::new(nvenc::Encoder::new(config.clone(),adapter_luid)?))}
                    #[cfg(not(target_arch="x86_64"))] {let _=adapter_luid;Err(Error::Unavailable("NVENC requires Windows x64"))}
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = config;
            Err(Error::Unavailable(
                "native H264 packet adapter is not verified on this platform",
            ))
        }
    })
}
