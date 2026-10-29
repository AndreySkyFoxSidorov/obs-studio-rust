//! Worker-owned file/decoder foundation. Native alternative is explicitly admitted on its background owner.
use serde::Serialize;
mod native_video;
pub use native_video::{FileVideoFrame,VideoDecoderBackend,VideoDecoderInfo,decoder_capabilities};
use skylive_mp4::{ByteSource, CodecConfig, Mp4File, Track, TrackKind};
use std::{
    cell::Cell,
    collections::VecDeque,
    fmt,
    fs::File,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
    time::SystemTime,
};

// Process-wide cooperative admission for live and prepared H.264 decoders.
// The reservation accounts for a worst-profile DPB, decoded output queue and
// decoder scratch using padded dimensions. Native codec internals are opaque;
// this does not claim to be a hard process-wide memory limit.
const VIDEO_DECODER_BUDGET: usize = 2 * 1024 * 1024 * 1024;
const VIDEO_DECODER_LIMIT: usize = 10; // eight readers plus two preparations/seeks
static VIDEO_DECODER_BYTES: AtomicUsize = AtomicUsize::new(0);
static VIDEO_DECODER_OWNERS: AtomicUsize = AtomicUsize::new(0);
static RUST_OUTPUT_BYTES: AtomicUsize = AtomicUsize::new(0);
const RUST_OUTPUT_BUDGET: usize = 128 * 1024 * 1024;
/// Decoded Rust picture with a process-wide lease retained after its reader
/// closes. The underlying image can be borrowed for the existing raster API.
pub struct RustPicture { picture: skylive_h264::Picture, _credit: OutputCredit }
impl std::ops::Deref for RustPicture {
    type Target = skylive_h264::Picture;
    fn deref(&self) -> &Self::Target { &self.picture }
}
struct OutputCredit(usize);
impl OutputCredit {
    fn acquire(picture: &skylive_h264::Picture) -> Result<Self, MediaFileError> {
        let bytes = picture.y.capacity().checked_add(picture.u.capacity())
            .and_then(|n| n.checked_add(picture.v.capacity())).ok_or(MediaFileError::Limit("decoded Rust picture bytes"))?;
        if bytes > RUST_OUTPUT_BUDGET { return Err(MediaFileError::Limit("decoded Rust picture exceeds output budget")); }
        RUST_OUTPUT_BYTES.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |used| used.checked_add(bytes).filter(|n| *n <= RUST_OUTPUT_BUDGET))
            .map_err(|_| MediaFileError::DecoderBusy("decoded Rust output pool is occupied"))?;
        Ok(Self(bytes))
    }
}
impl Drop for OutputCredit { fn drop(&mut self) { RUST_OUTPUT_BYTES.fetch_sub(self.0, Ordering::AcqRel); } }
struct VideoCredit { bytes: usize }
fn padded_pixels(width: u32, height: u32) -> Result<usize, MediaFileError> {
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(MediaFileError::Limit("media decoder dimensions"));
    }
    let w=(width as usize).checked_add(15).map(|n| n & !15).ok_or(MediaFileError::Limit("media decoder reservation"))?;
    let h=(height as usize).checked_add(15).map(|n| n & !15).ok_or(MediaFileError::Limit("media decoder reservation"))?;
    w.checked_mul(h).ok_or(MediaFileError::Limit("media decoder reservation"))
}
fn sps_dimensions(nal:&[u8])->Result<(u32,u32),MediaFileError>{
    if nal.len()<4 || nal.len()>8192 || nal[0]&31!=7 {return Err(MediaFileError::Unsupported("invalid bounded AVC sequence header"));}
    let sps=catch_unwind(AssertUnwindSafe(||skylive_h264::params::Sps::parse(&skylive_codec_bits::unescape_rbsp(&nal[1..]))))
        .map_err(|_|MediaFileError::Decode("AVC sequence parser invariant failed".into()))?
        .map_err(|error|MediaFileError::Decode(error.to_string()))?;
    let dimensions=(sps.width(),sps.height());padded_pixels(dimensions.0,dimensions.1)?;Ok(dimensions)
}
impl VideoCredit {
    fn acquire(width: u32, height: u32) -> Result<Self, MediaFileError> {
        let bytes = padded_pixels(width,height)?.checked_mul(96)
            .and_then(|n| n.checked_add(32 * 1024 * 1024)).ok_or(MediaFileError::Limit("media decoder reservation"))?;
        Self::acquire_bytes(bytes)
    }
    fn acquire_bytes(bytes:usize) -> Result<Self, MediaFileError> {
        if bytes > VIDEO_DECODER_BUDGET { return Err(MediaFileError::Limit("media decoder exceeds reservation budget")); }
        VIDEO_DECODER_OWNERS.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |used| (used < VIDEO_DECODER_LIMIT).then_some(used + 1))
            .map_err(|_| MediaFileError::DecoderBusy("media decoder owner capacity including seek/preparation"))?;
        if VIDEO_DECODER_BYTES.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |used| used.checked_add(bytes).filter(|n| *n <= VIDEO_DECODER_BUDGET)).is_err() {
            VIDEO_DECODER_OWNERS.fetch_sub(1, Ordering::AcqRel);
            return Err(MediaFileError::DecoderBusy("media decoder reservation budget including seek/preparation"));
        }
        Ok(Self { bytes })
    }
}
impl Drop for VideoCredit {
    fn drop(&mut self) {
        VIDEO_DECODER_BYTES.fetch_sub(self.bytes, Ordering::AcqRel);
        VIDEO_DECODER_OWNERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MediaFileLimits {
    pub max_packet_bytes: usize,
    pub max_video_queue_bytes: usize,
    pub max_packets_per_call: usize,
}
impl Default for MediaFileLimits {
    fn default() -> Self {
        Self {
            max_packet_bytes: 2 * 1024 * 1024,
            max_video_queue_bytes: 128 * 1024 * 1024,
            max_packets_per_call: 64,
        }
    }
}
impl MediaFileLimits {
    fn validate(self) -> Result<(), MediaFileError> {
        if self.max_packet_bytes == 0
            || self.max_packet_bytes > 2 * 1024 * 1024
            || self.max_video_queue_bytes == 0
            || self.max_video_queue_bytes > 128 * 1024 * 1024
            || self.max_packets_per_call == 0
            || self.max_packets_per_call > 64
        {
            Err(MediaFileError::Limit("media file limits"))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct VideoFileInfo {
    pub decoder:VideoDecoderInfo,
    pub width: u32,
    pub height: u32,
    pub samples: usize,
    pub duration_ns: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct AudioFileInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub channel_mask: u32,
    pub samples: usize,
    pub duration_ns: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct MediaFileInfo {
    pub path: String,
    pub bytes: u64,
    pub video: Option<VideoFileInfo>,
    pub audio: Option<AudioFileInfo>,
    pub ignored_non_av_tracks: usize,
}
#[derive(Debug)]
pub enum MediaFileError {
    DecoderBusy(&'static str),
    Native {operation:&'static str,hresult:i32,detail:String},
    Io(io::Error),
    Container(String),
    Unsupported(&'static str),
    Limit(&'static str),
    Decode(String),
    TimeOverflow,
    ResourceChanged,
    Poisoned,
}
impl MediaFileError {
    pub fn issue_kind(&self) -> crate::resource_issue::ResourceIssueKind {
        use crate::resource_issue::ResourceIssueKind::*;
        match self {
            Self::DecoderBusy(_) => Unavailable,
            Self::Limit(_) => Limit,
            Self::TimeOverflow => Invalid,
            _ => Unavailable,
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            Self::DecoderBusy(_) => "media_decoder_busy",
            Self::Native {..}=>"media_native_decoder",
            Self::Io(_) => "media_file_io",
            Self::Container(_) => "media_container",
            Self::Unsupported(_) => "media_unsupported",
            Self::Limit(_) => "media_limit",
            Self::Decode(_) => "media_decode",
            Self::TimeOverflow => "media_time_overflow",
            Self::ResourceChanged => "media_resource_changed",
            Self::Poisoned => "media_decoder_poisoned",
        }
    }
}
impl fmt::Display for MediaFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DecoderBusy(e) => f.write_str(e),
            Self::Native {operation,hresult,detail}=>write!(f,"{operation}: HRESULT {hresult:#010x}: {detail}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Container(e) | Self::Decode(e) => f.write_str(e),
            Self::Unsupported(e) | Self::Limit(e) => f.write_str(e),
            Self::TimeOverflow => f.write_str("media timestamp overflow"),
            Self::ResourceChanged => f.write_str("media file changed during decoding"),
            Self::Poisoned => f.write_str("decoder failed; reopen the media source"),
        }
    }
}
impl std::error::Error for MediaFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Io(e) = self {
            Some(e)
        } else {
            None
        }
    }
}
impl From<io::Error> for MediaFileError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
fn container(e: skylive_mp4::Error) -> MediaFileError {
    // Preserve the I/O kind so a future nonblocking byte source can distinguish
    // temporary unavailability from a corrupt MP4 container. Local I/O errors
    // remain distinguishable from parser errors for the existing callers too.
    match e {
        skylive_mp4::Error::Io(error) => MediaFileError::Io(error),
        other => MediaFileError::Container(other.to_string()),
    }
}
fn timestamp(value: i64, scale: u32) -> Result<i64, MediaFileError> {
    if scale == 0 {
        return Err(MediaFileError::Unsupported("zero media timescale"));
    }
    i64::try_from((i128::from(value) * 1_000_000_000).div_euclid(i128::from(scale)))
        .map_err(|_| MediaFileError::TimeOverflow)
}
fn duration(value: u64, scale: u32) -> Result<u64, MediaFileError> {
    if scale == 0 {
        return Err(MediaFileError::Unsupported("zero media timescale"));
    }
    u64::try_from(u128::from(value) * 1_000_000_000 / u128::from(scale))
        .map_err(|_| MediaFileError::TimeOverflow)
}
fn check_track(track: &Track) -> Result<(), MediaFileError> {
    if !track.self_contained_data || track.entries.iter().any(|e| e.data_reference_index != 1) {
        return Err(MediaFileError::Unsupported(
            "media profile requires one self-contained data reference; external references are not followed",
        ));
    }
    if track.timescale == 0 || track.entries.len() != 1 {
        return Err(MediaFileError::Unsupported(
            "one sample description and a nonzero track timescale required",
        ));
    }
    if track.edits.len() > 2
        || track.edits.iter().any(|e| e.media_rate != 65536)
        || track.edits.iter().skip(1).any(|e| e.media_time < 0)
        || (track.edits.len() == 1 && track.edits[0].media_time < 0)
        || (track.edits.len() == 2 && track.edits[0].media_time != -1)
    {
        return Err(MediaFileError::Unsupported(
            "complex media edit lists are not supported",
        ));
    }
    if track.samples.iter().any(|s| s.description_index != 1) {
        return Err(MediaFileError::Unsupported(
            "dynamic sample description switches are unsupported",
        ));
    }
    Ok(())
}
/// Cumulative actual encoded-byte reads across sequential worker preparations.
/// Shared Cell stays on the calling worker and is never sent to the GUI.
pub(crate) struct MediaReadBudget(Rc<Cell<u64>>);
impl MediaReadBudget {
    pub(crate) fn new(bytes:u64)->Self { Self(Rc::new(Cell::new(bytes))) }
    pub(crate) fn reserve(&self, bytes:u64) -> Result<(),MediaFileError> {
        let left = self.0.get().checked_sub(bytes).ok_or(MediaFileError::Limit("cumulative media read budget"))?;
        self.0.set(left);
        Ok(())
    }
}
struct Source {
    data: SourceData,
    len: u64,
    modified: Option<SystemTime>,
    remaining: Cell<u64>,
    shared_remaining: Option<Rc<Cell<u64>>>,
}
const MEMORY_MEDIA_BUDGET: usize = 64 * 1024 * 1024;
static MEMORY_MEDIA_BYTES: AtomicUsize = AtomicUsize::new(0);
struct MemoryMedia { bytes: Box<[u8]> }
impl MemoryMedia {
    fn new(bytes: Vec<u8>) -> Result<Self, MediaFileError> {
        let size = bytes.len();
        MEMORY_MEDIA_BYTES.fetch_update(Ordering::AcqRel, Ordering::Acquire, |used|
            used.checked_add(size).filter(|total| *total <= MEMORY_MEDIA_BUDGET))
            .map_err(|_| MediaFileError::Limit("in-memory media budget"))?;
        Ok(Self { bytes: bytes.into_boxed_slice() })
    }
}
impl Drop for MemoryMedia {
    fn drop(&mut self) { MEMORY_MEDIA_BYTES.fetch_sub(self.bytes.as_ref().len(), Ordering::AcqRel); }
}
enum SourceData { File(File), Bytes(MemoryMedia), Growing(crate::media_source::network_progress::Reader) }
impl Source {
    fn unchanged(&self) -> Result<(), MediaFileError> {
        let SourceData::File(file) = &self.data else { return Ok(()); };
        let meta = file.metadata()?;
        if meta.len() != self.len || meta.modified().ok() != self.modified {
            Err(MediaFileError::ResourceChanged)
        } else {
            Ok(())
        }
    }
}
impl ByteSource for Source {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let remaining = self
            .remaining
            .get()
            .checked_sub(buf.len() as u64)
            .ok_or_else(|| io::Error::other("media read budget exhausted"))?;
        let shared = self.shared_remaining.as_ref().map(|credit| {
            credit.get().checked_sub(buf.len() as u64)
                .ok_or_else(|| io::Error::other("cumulative media read budget exhausted"))
        }).transpose()?;
        self.remaining.set(remaining);
        if let (Some(credit),Some(value))=(&self.shared_remaining,shared) { credit.set(value); }
        if offset
            .checked_add(buf.len() as u64)
            .is_none_or(|end| end > self.len)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "media read outside file",
            ));
        }
        match &self.data {
            SourceData::File(file) => ByteSource::read_at(file, offset, buf),
            SourceData::Bytes(bytes) => ByteSource::read_at(bytes.bytes.as_ref(), offset, buf),
            SourceData::Growing(bytes) => ByteSource::read_at(bytes, offset, buf),
        }
    }
}
pub struct DecodedFileAudio {
    pub presentation_ns: i64,
    pub sample_rate: u32,
    pub channels: u16,
    pub channel_mask: u32,
    pub samples: Vec<f32>,
}
/// One owner holds this decoder and file descriptor. Outputs own their bytes.
pub(crate) struct PreparedSeek {
    #[cfg(target_os="windows")] native:Option<native_video::Decoder>,
    video: Option<skylive_h264::Decoder>,
    audio: Option<skylive_aac::Decoder>,
    vi: usize,
    ai: usize,
    _video_credit: Option<VideoCredit>,
}
pub struct MediaFileReader {
    #[cfg(target_os="windows")] native:Option<native_video::Decoder>,
    #[cfg(target_os="windows")] native_pending:VecDeque<native_video::NativePicture>,
    source: Source,
    file: Mp4File,
    info: MediaFileInfo,
    limits: MediaFileLimits,
    video_track: Option<usize>,
    audio_track: Option<usize>,
    video: Option<skylive_h264::Decoder>,
    audio: Option<skylive_aac::Decoder>,
    video_index: usize,
    audio_index: usize,
    pending: VecDeque<skylive_h264::Picture>,
    video_drained: bool,
    poisoned: bool,
    // Last field so the decoder and retained frames retire before its credit.
    video_credit: Option<VideoCredit>,
    video_credit_pixels: usize,
}
impl MediaFileReader {
    pub fn open(path: &Path, limits: MediaFileLimits) -> Result<Self, MediaFileError> {
        Self::open_with_native(path,limits,false,None)
    }
    /// Explicit production media owner path. Creates native codec only here,
    /// after a user prepares or activates a local source, never at app startup.
    pub fn open_native_preferred(path:&Path,limits:MediaFileLimits)->Result<Self,MediaFileError>{Self::open_with_native(path,limits,true,None)}
    pub(crate) fn open_native_preferred_with_budget(path:&Path,limits:MediaFileLimits,budget:&MediaReadBudget)->Result<Self,MediaFileError>{
        Self::open_with_native(path,limits,true,Some(budget.0.clone()))
    }
    fn open_with_native(path:&Path,limits:MediaFileLimits,prefer_native:bool,shared_remaining:Option<Rc<Cell<u64>>>)->Result<Self,MediaFileError>{
        #[cfg(not(target_os="windows"))] let _=prefer_native;
        limits.validate()?;
        let path = path.canonicalize()?;
        if !std::fs::metadata(&path)?.is_file() {
            return Err(MediaFileError::Unsupported(
                "media input must be a regular local file",
            ));
        }
        let handle = File::open(&path)?;
        let meta = handle.metadata()?;
        let source = Source {
            data: SourceData::File(handle),
            len: meta.len(),
            modified: meta.modified().ok(),
            remaining: Cell::new(80 * 1024 * 1024),
            shared_remaining,
        };
        let path = path.to_str().ok_or(MediaFileError::Unsupported("media path must be UTF-8"))?.to_owned();
        Self::open_source(source, path, limits, prefer_native)
    }
    /// Decode a finite, already acquired media object without writing a temporary
    /// file. The caller owns network policy, cancellation and document identity.
    pub fn open_bytes_native_preferred(bytes: Vec<u8>, identity: String, limits: MediaFileLimits) -> Result<Self, MediaFileError> {
        limits.validate()?;
        if bytes.is_empty() || bytes.len() > 32 * 1024 * 1024 || identity.is_empty() || identity.len() > 16_384 {
            return Err(MediaFileError::Limit("in-memory media input"));
        }
        let len = bytes.len() as u64;
        let source = Source { data: SourceData::Bytes(MemoryMedia::new(bytes)?), len, modified: None,
            remaining: Cell::new(80 * 1024 * 1024), shared_remaining: None };
        Self::open_source(source, identity, limits, true)
    }
    /// A not-yet-complete source is admitted only by a fenced background owner.
    /// Missing bytes are reported as `Io(WouldBlock)` without advancing a decoder.
    pub(crate) fn open_growing_native_preferred(bytes: crate::media_source::network_progress::Reader, identity: String, limits: MediaFileLimits, budget:&MediaReadBudget) -> Result<Self,MediaFileError> {
        limits.validate()?;
        if identity.is_empty() || identity.len()>16_384 {return Err(MediaFileError::Limit("progressive media identity"));}
        let len=ByteSource::len(&bytes);
        let source=Source {data:SourceData::Growing(bytes),len,modified:None,remaining:Cell::new(80*1024*1024),shared_remaining:Some(budget.0.clone())};
        let mut reader=Self::open_source(source,identity,limits,true)?;
        // Preparation retries share the budget; ordinary packet reads retain
        // the existing per-packet limit after a decoder has been admitted.
        reader.source.shared_remaining=None;
        Ok(reader)
    }
    fn open_source(source: Source, path: String, limits: MediaFileLimits, prefer_native: bool) -> Result<Self, MediaFileError> {
        #[cfg(not(target_os="windows"))] let _=prefer_native;
        let file = catch_unwind(AssertUnwindSafe(|| skylive_mp4::open(&source)))
            .map_err(|_| MediaFileError::Container("container parser invariant failed".into()))?
            .map_err(container)?;
        let mut video_track = None;
        let mut audio_track = None;
        #[cfg(target_os="windows")] let mut native=None;
        let mut video = None;
        let mut audio = None;
        let mut video_info = None;
        let mut video_credit = None;
        let mut video_credit_pixels = 0usize;
        let mut audio_info = None;
        let mut ignored = 0;
        for (index, track) in file.tracks.iter().enumerate() {
            if !track.enabled {
                continue;
            }
            match track.kind {
                TrackKind::Video => {
                    if video_track.is_some() {
                        return Err(MediaFileError::Unsupported(
                            "multiple enabled video tracks require explicit track selection",
                        ));
                    }
                    check_track(track)?;
                    if track.matrix != [65536, 0, 0, 0, 65536, 0, 0, 0, 1 << 30] {
                        return Err(MediaFileError::Unsupported(
                            "media display rotation or transform is unsupported",
                        ));
                    }
                    let params = track.video().ok_or(MediaFileError::Unsupported(
                        "video sample parameters missing",
                    ))?;
                    if params.pixel_aspect.is_some_and(|(a, b)| a == 0 || a != b)
                        || params.clean_aperture.is_some()
                        || params.field_info.is_some()
                        || params.mastering_display.is_some()
                        || params.content_light.is_some()
                    {
                        return Err(MediaFileError::Unsupported(
                            "video aspect/aperture/interlace/HDR metadata requires unsupported processing",
                        ));
                    }
                    let CodecConfig::Avc(config) = track
                        .codec()
                        .ok_or(MediaFileError::Unsupported("video codec missing"))?
                    else {
                        return Err(MediaFileError::Unsupported(
                            "only H.264 video is supported by this file profile",
                        ));
                    };
                    let configuration_bytes = config
                        .sps
                        .iter()
                        .chain(&config.pps)
                        .try_fold(config.ext.len() + 7, |sum, set| {
                            sum.checked_add(set.len())?.checked_add(2)
                        })
                        .ok_or(MediaFileError::Limit("AVC configuration"))?;
                    if configuration_bytes > 65536 || !matches!(config.length_size, 1 | 2 | 4) {
                        return Err(MediaFileError::Limit("AVC configuration"));
                    }
                    // AVC may pad beyond the container dimensions. Reserve
                    // against actual coded SPS dimensions, not only its crop.
                    let mut reserve = (u32::from(params.width), u32::from(params.height));
                    let mut pixels = padded_pixels(reserve.0, reserve.1)?;
                    for sps in &config.sps {
                        let dims = sps_dimensions(sps)?;
                        let candidate = padded_pixels(dims.0, dims.1)?;
                        if candidate > pixels { reserve = dims; pixels = candidate; }
                    }
                    video_credit = Some(VideoCredit::acquire(reserve.0, reserve.1)?);
                    video_credit_pixels = pixels;
                    let mut backend=VideoDecoderBackend::Rust;
                    #[cfg(target_os="windows")] if prefer_native {native=native_video::Decoder::try_new(config,track)?;if native.is_some(){backend=VideoDecoderBackend::WindowsMfSoftware;}}
                    let use_rust=match backend {VideoDecoderBackend::Rust=>true,VideoDecoderBackend::WindowsMfSoftware=>false};
                    if use_rust {
                    let configuration = config.to_bytes();
                    let decoder = catch_unwind(AssertUnwindSafe(|| {
                        skylive_h264::Decoder::from_avcc(&configuration)
                    }))
                    .map_err(|_| {
                        MediaFileError::Decode("H.264 configuration invariant failed".into())
                    })?
                    .map_err(|e| MediaFileError::Decode(e.to_string()))?;
                        video=Some(decoder);
                    }
                    video_info = Some(VideoFileInfo {
                        decoder:VideoDecoderInfo {backend,profile:config.profile,level:config.level,reordered_timestamps:track.samples.iter().any(|s|s.pts!=s.dts)},
                        width: params.width.into(),
                        height: params.height.into(),
                        samples: track.samples.len(),
                        duration_ns: duration(track.tkhd_duration, file.timescale)?,
                    });
                    video_track = Some(index);
                }
                TrackKind::Audio => {
                    if audio_track.is_some() {
                        return Err(MediaFileError::Unsupported(
                            "multiple enabled audio tracks require explicit track selection",
                        ));
                    }
                    check_track(track)?;
                    let CodecConfig::Aac(config) = track
                        .codec()
                        .ok_or(MediaFileError::Unsupported("audio codec missing"))?
                    else {
                        return Err(MediaFileError::Unsupported(
                            "only AAC-LC audio is supported by this file profile",
                        ));
                    };
                    if config.asc.len() > 4096 {
                        return Err(MediaFileError::Limit("AAC configuration"));
                    }
                    let config = skylive_aac::AudioSpecificConfig::parse(&config.asc)
                        .map_err(|e| MediaFileError::Decode(e.to_string()))?;
                    if config.sbr
                        || config.ps
                        || config.frame_length_flag
                        || !matches!(config.channel_config, 1 | 2)
                    {
                        return Err(MediaFileError::Unsupported(
                            "only AAC-LC mono/stereo without SBR/PS is supported",
                        ));
                    }
                    let CodecConfig::Aac(raw) = track
                        .codec()
                        .ok_or(MediaFileError::Unsupported("AAC configuration missing"))?
                    else {
                        return Err(MediaFileError::Unsupported("AAC configuration missing"));
                    };
                    let decoder = skylive_aac::Decoder::new(&raw.asc)
                        .map_err(|e| MediaFileError::Decode(e.to_string()))?;
                    if decoder.sample_rate() != track.timescale {
                        return Err(MediaFileError::Unsupported(
                            "AAC media timescale must equal sample rate",
                        ));
                    }
                    let channels = decoder.channels() as u16;
                    audio_info = Some(AudioFileInfo {
                        sample_rate: decoder.sample_rate(),
                        channels,
                        channel_mask: if channels == 1 { 4 } else { 3 },
                        samples: track.samples.len(),
                        duration_ns: duration(track.tkhd_duration, file.timescale)?,
                    });
                    audio_track = Some(index);
                    audio = Some(decoder);
                }
                _ => ignored += 1,
            }
        }
        if video_track.is_none() && audio.is_none() {
            return Err(MediaFileError::Unsupported(
                "file contains no enabled supported audio/video track",
            ));
        }
        let info = MediaFileInfo {
            path,
            bytes: source.len,
            video: video_info,
            audio: audio_info,
            ignored_non_av_tracks: ignored,
        };
        Ok(Self {
            #[cfg(target_os="windows")] native,
            #[cfg(target_os="windows")] native_pending:VecDeque::new(),
            source,
            file,
            info,
            limits,
            video_track,
            audio_track,
            video,
            audio,
            video_index: 0,
            audio_index: 0,
            pending: VecDeque::new(),
            video_drained: false,
            poisoned: false,
            video_credit,
            video_credit_pixels,
        })
    }
    /// Revalidate the opened resource and its current linked path before a
    /// prepared candidate publishes metadata. This is not a content hash.
    pub(crate) fn ensure_unchanged(&self)->Result<(),MediaFileError>{
        self.source.unchanged()?;
        if !matches!(&self.source.data, SourceData::File(_)) { return Ok(()); }
        let linked=std::fs::metadata(&self.info.path)?;
        if !linked.is_file() || linked.len()!=self.source.len || linked.modified().ok()!=self.source.modified {
            return Err(MediaFileError::ResourceChanged);
        }
        Ok(())
    }
    pub fn info(&self) -> &MediaFileInfo {
        &self.info
    }
    /// Both next compressed packets must be available before resuming an
    /// automatically buffered source. No decoder index is advanced here.
    pub(crate) fn growing_ready(&self, video:bool, audio:bool) -> Result<bool,MediaFileError> {
        let SourceData::Growing(bytes)=&self.source.data else{return Ok(true);};
        let mut end=0u64;
        for (track,index,enabled) in [(self.video_track,self.video_index,video),(self.audio_track,self.audio_index,audio)] {
            if !enabled {continue;}
            // One owner tick can request four frames and each decoder call
            // can consume up to max_packets_per_call compressed packets.
            // Check that complete bounded window before publishing any output.
            if let Some(track)=track {
                let samples=&self.file.tracks[track].samples;
                for sample in samples.iter().skip(index).take(self.limits.max_packets_per_call.saturating_mul(4)) {
                    end=end.max(sample.offset.checked_add(u64::from(sample.size)).ok_or(MediaFileError::TimeOverflow)?);
                }
            }
        }
        bytes.ready_through(end).map_err(MediaFileError::Io)
    }
    pub(crate) fn is_growing(&self)->bool { matches!(&self.source.data,SourceData::Growing(_)) }
    /// Recreate codec state at a preceding random-access point. Packet work
    /// remains bounded by next_video/next_audio; no decoded seek loop occurs here.
    pub fn seek(&mut self, position_ns: u64) -> Result<(), MediaFileError> {
        let prepared = self.prepare_seek(position_ns)?;
        self.commit_seek(prepared);
        Ok(())
    }
    pub(crate) fn prepare_seek(&self, position_ns: u64) -> Result<PreparedSeek, MediaFileError> {
        self.source.unchanged()?;
        #[cfg(target_os="windows")] let mut native=None;
        let mut video = None;
        let mut audio = None;
        let mut vi = 0;
        let mut ai = 0;
        let mut video_credit = None;
        if let Some(index) = self.video_track {
            let previous = self.video_credit.as_ref().ok_or(MediaFileError::Poisoned)?;
            video_credit = Some(VideoCredit::acquire_bytes(previous.bytes)?);
            let track = &self.file.tracks[index];
            let units = i64::try_from(
                u128::from(position_ns) * u128::from(track.timescale) / 1_000_000_000,
            )
            .map_err(|_| MediaFileError::TimeOverflow)?;
            vi = track.sync_sample_before(track.sample_at_presentation_time(units).unwrap_or(0));
            let Some(CodecConfig::Avc(config)) = track.codec() else {
                return Err(MediaFileError::Poisoned);
            };
            #[cfg(target_os="windows")] if let Some(decoder)=&self.native {native=Some(decoder.recreate()?);}
            let use_rust=self.video.is_some();
            if use_rust {video = Some(
                catch_unwind(AssertUnwindSafe(|| {
                    skylive_h264::Decoder::from_avcc(&config.to_bytes())
                }))
                .map_err(|_| {
                    MediaFileError::Decode("H.264 seek configuration invariant failed".into())
                })?
                .map_err(|e| MediaFileError::Decode(e.to_string()))?,
            );}
        }
        if let Some(index) = self.audio_track {
            let track = &self.file.tracks[index];
            let units = i64::try_from(
                u128::from(position_ns) * u128::from(track.timescale) / 1_000_000_000,
            )
            .map_err(|_| MediaFileError::TimeOverflow)?;
            // One preceding AAC frame restores the IMDCT overlap before the
            // requested frame. The consumer discards this explicit preroll.
            ai = track
                .sample_at_presentation_time(units)
                .unwrap_or(0)
                .saturating_sub(1);
            let Some(CodecConfig::Aac(config)) = track.codec() else {
                return Err(MediaFileError::Poisoned);
            };
            audio = Some(
                catch_unwind(AssertUnwindSafe(|| skylive_aac::Decoder::new(&config.asc)))
                    .map_err(|_| {
                        MediaFileError::Decode("AAC seek configuration invariant failed".into())
                    })?
                    .map_err(|e| MediaFileError::Decode(e.to_string()))?,
            );
        }
        Ok(PreparedSeek {
            #[cfg(target_os="windows")] native,
            video,
            audio,
            vi,
            ai,
            _video_credit: video_credit,
        })
    }
    pub(crate) fn commit_seek(
        &mut self,
        PreparedSeek {
            #[cfg(target_os="windows")] native,
            video,
            audio,
            vi,
            ai,
            _video_credit,
        }: PreparedSeek,
    ) {
        #[cfg(target_os="windows")] {self.native=native;self.native_pending.clear();}
        self.video = video;
        self.audio = audio;
        self.video_index = vi;
        self.audio_index = ai;
        self.pending.clear();
        self.video_credit = _video_credit;
        self.video_drained = false;
        self.poisoned = false;
    }
    fn packet(&self, track: usize, index: usize) -> Result<Vec<u8>, MediaFileError> {
        self.source.unchanged()?;
        let sample = self.file.tracks[track].samples[index];
        if sample.size as usize > self.limits.max_packet_bytes {
            return Err(MediaFileError::Limit("compressed media packet"));
        }
        self.source
            .remaining
            .set(self.limits.max_packet_bytes as u64);
        self.file
            .read_sample(&self.source, track, index)
            .map_err(container)
    }
    pub fn next_frame(&mut self)->Result<Option<FileVideoFrame>,MediaFileError>{
        #[cfg(target_os="windows")] if self.native.is_some(){return self.next_native_frame().map(|p|p.map(FileVideoFrame::Native));}
        self.next_video().map(|p|p.map(FileVideoFrame::Rust))
    }
    pub fn rgba_frame(&self,frame:&FileVideoFrame)->Result<skylive_media::RgbaFrame,MediaFileError>{match frame{
        FileVideoFrame::Rust(p)=>self.rgba(p),
        #[cfg(target_os="windows")] FileVideoFrame::Native(p)=>skylive_media::ingest_planar_420(&p.y,&p.u,&p.v,skylive_media::Planar420Layout {width:p.width,height:p.height,y_stride:p.width as usize,uv_stride:p.width as usize/2,matrix:skylive_media::CameraMatrix::Bt709,range:skylive_media::CameraRange::Limited,chroma_left:p.chroma_left},skylive_media::PackedFrameLimits {max_input_bytes:64*1024*1024,max_output_bytes:16*1024*1024,max_pixel_visits:6*4_194_304}).map_err(|e|MediaFileError::Decode(format!("native video raster conversion: {e}"))),
    }}
    #[cfg(target_os="windows")]
    fn next_native_frame(&mut self)->Result<Option<native_video::NativePicture>,MediaFileError>{
        if self.poisoned{return Err(MediaFileError::Poisoned);}
        if let Some(frame)=self.native_pending.pop_front(){return Ok(Some(frame));}
        let Some(track)=self.video_track else{return Ok(None);};
        for _ in 0..self.limits.max_packets_per_call {
            let result=if self.video_index<self.file.tracks[track].samples.len(){
                let sample=self.file.tracks[track].samples[self.video_index];let data=self.packet(track,self.video_index)?;
                let t=&self.file.tracks[track];
                let pts=timestamp(sample.pts.checked_add(t.edit_offset).ok_or(MediaFileError::TimeOverflow)?,t.timescale)?;
                let dts=timestamp(sample.dts.checked_add(t.edit_offset).ok_or(MediaFileError::TimeOverflow)?,t.timescale)?;
                let span=duration(u64::from(sample.duration),t.timescale)?;
                let decoder=self.native.as_mut().ok_or(MediaFileError::Poisoned)?;
                let output=catch_unwind(AssertUnwindSafe(||decoder.decode(&data,pts,dts,span,sample.is_sync))).map_err(|_|MediaFileError::Decode("native AVC decoder invariant failed".into())).and_then(|r|r);
                self.video_index+=1;output
            }else if !self.video_drained {
                let decoder=self.native.as_mut().ok_or(MediaFileError::Poisoned)?;
                let output=decoder.drain();self.video_drained=decoder.is_drained();output
            }else{return Ok(None);};
            let frames=match result {Ok(v)=>v,Err(e)=>{self.poisoned=true;return Err(e);}};
            let bytes=frames.iter().try_fold(0usize,|sum,p|sum.checked_add(p.byte_len())).ok_or(MediaFileError::Limit("native decoded video queue"))?;
            if bytes>self.limits.max_video_queue_bytes||frames.len()>4 {self.poisoned=true;return Err(MediaFileError::Limit("native decoded video queue"));}
            self.native_pending.extend(frames);if let Some(frame)=self.native_pending.pop_front(){return Ok(Some(frame));}
        }
        self.poisoned=true;Err(MediaFileError::Limit("native video packet work per call"))
    }
    /// Deterministic Rust picture API; native readers use next_frame instead.
    fn take_pending_picture(&mut self) -> Result<Option<RustPicture>, MediaFileError> {
        let Some(picture) = self.pending.front() else { return Ok(None); };
        let credit = OutputCredit::acquire(picture)?;
        let picture = self.pending.pop_front().ok_or(MediaFileError::Poisoned)?;
        Ok(Some(RustPicture { picture, _credit: credit }))
    }
    pub fn next_video(&mut self) -> Result<Option<RustPicture>, MediaFileError> {
        #[cfg(target_os="windows")] if self.native.is_some(){return Err(MediaFileError::Unsupported("native media reader requires next_frame / rgba_frame"));}
        if self.poisoned {
            return Err(MediaFileError::Poisoned);
        }
        if !self.pending.is_empty() { return self.take_pending_picture(); }
        let Some(track) = self.video_track else {
            return Ok(None);
        };
        for _ in 0..self.limits.max_packets_per_call {
            let result = if self.video_index < self.file.tracks[track].samples.len() {
                let sample = self.file.tracks[track].samples[self.video_index];
                let data = self.packet(track, self.video_index)?;
                let prefix = self
                    .video
                    .as_ref()
                    .and_then(|d| d.nal_length_size())
                    .ok_or(MediaFileError::Unsupported("AVC sample framing missing"))?;
                check_access_unit_with_limit(&data, prefix, self.video_credit_pixels)?;
                let units = sample
                    .pts
                    .checked_add(self.file.tracks[track].edit_offset)
                    .ok_or(MediaFileError::TimeOverflow)?;
                let pts = timestamp(units, self.file.tracks[track].timescale)?;
                self.video_index += 1;
                let decoder = self.video.as_mut().ok_or(MediaFileError::Poisoned)?;
                catch_unwind(AssertUnwindSafe(|| decoder.decode(&data, pts)))
                    .map_err(|_| MediaFileError::Decode("H.264 decoder invariant failed".into()))
                    .and_then(|r| r.map_err(|e| MediaFileError::Decode(e.to_string())))
            } else if !self.video_drained {
                self.video_drained = true;
                let decoder = self.video.as_mut().ok_or(MediaFileError::Poisoned)?;
                catch_unwind(AssertUnwindSafe(|| {
                    let pictures = decoder.flush();
                    if let Some(error) = decoder.take_error() {
                        Err(MediaFileError::Decode(error.to_string()))
                    } else {
                        Ok(pictures)
                    }
                }))
                .map_err(|_| MediaFileError::Decode("H.264 flush invariant failed".into()))
                .and_then(|result| result)
            } else {
                return Ok(None);
            };
            match result {
                Ok(pictures) => {
                    let bytes = pictures
                        .iter()
                        .try_fold(0usize, |sum, p| {
                            sum.checked_add(p.y.len())?
                                .checked_add(p.u.len())?
                                .checked_add(p.v.len())
                        })
                        .ok_or(MediaFileError::Limit("decoded video queue"))?;
                    if bytes > self.limits.max_video_queue_bytes {
                        self.poisoned = true;
                        return Err(MediaFileError::Limit("decoded video queue"));
                    }
                    self.pending.extend(pictures);
                }
                Err(error) => {
                    self.poisoned = true;
                    return Err(error);
                }
            }
            if !self.pending.is_empty() { return self.take_pending_picture(); }
        }
        self.poisoned = true;
        Err(MediaFileError::Limit("video decode packet work per call"))
    }
    pub fn next_audio(&mut self) -> Result<Option<DecodedFileAudio>, MediaFileError> {
        if self.poisoned {
            return Err(MediaFileError::Poisoned);
        }
        let Some(track) = self.audio_track else {
            return Ok(None);
        };
        for _ in 0..self.limits.max_packets_per_call {
            let Some(sample) = self.file.tracks[track]
                .samples
                .get(self.audio_index)
                .copied()
            else {
                return Ok(None);
            };
            let data = self.packet(track, self.audio_index)?;
            self.audio_index += 1;
            let decoder = self.audio.as_mut().ok_or(MediaFileError::Poisoned)?;
            let result = catch_unwind(AssertUnwindSafe(|| decoder.decode(&data)))
                .map_err(|_| MediaFileError::Decode("AAC decoder invariant failed".into()))
                .and_then(|r| r.map_err(|e| MediaFileError::Decode(e.to_string())));
            let pcm = match result {
                Ok(pcm) => pcm,
                Err(error) => {
                    self.poisoned = true;
                    return Err(error);
                }
            };
            if decoder.sbr() {
                self.poisoned = true;
                return Err(MediaFileError::Unsupported(
                    "implicit AAC SBR requires a complete HE-AAC decoder",
                ));
            }
            let channels = decoder.channels();
            if !matches!(channels, 1 | 2)
                || pcm.len() != channels
                || pcm
                    .iter()
                    .any(|p| p.len() != 1024 || p.iter().any(|v| !v.is_finite()))
            {
                self.poisoned = true;
                return Err(MediaFileError::Decode(
                    "invalid AAC decoded channel layout or samples".into(),
                ));
            }
            let first = sample
                .pts
                .checked_add(self.file.tracks[track].edit_offset)
                .ok_or(MediaFileError::TimeOverflow)?;
            let trim = usize::try_from(-i128::from(first).min(0))
                .map_err(|_| MediaFileError::TimeOverflow)?
                .min(1024);
            let count = (sample.duration as usize).min(1024);
            if trim >= count {
                continue;
            }
            let mut samples = Vec::with_capacity((count - trim) * channels);
            for frame in trim..count {
                for plane in &pcm {
                    samples.push(plane[frame]);
                }
            }
            let sample_rate = decoder.sample_rate();
            return Ok(Some(DecodedFileAudio {
                presentation_ns: timestamp(
                    first
                        .checked_add(trim as i64)
                        .ok_or(MediaFileError::TimeOverflow)?,
                    sample_rate,
                )?,
                sample_rate,
                channels: channels as u16,
                channel_mask: if channels == 1 { 4 } else { 3 },
                samples,
            }));
        }
        self.poisoned = true;
        Err(MediaFileError::Limit("audio decode packet work per call"))
    }
}
#[cfg(test)]
fn check_access_unit(data: &[u8], prefix: usize) -> Result<(), MediaFileError> {
    check_access_unit_with_limit(data,prefix,usize::MAX)
}
fn check_access_unit_with_limit(data: &[u8], prefix: usize, max_pixels:usize) -> Result<(), MediaFileError> {
    if !matches!(prefix, 1 | 2 | 4) {
        return Err(MediaFileError::Unsupported("NAL prefix width"));
    }
    let mut at = 0usize;
    let mut nals = 0usize;
    let mut pictures = 0usize;
    while at < data.len() {
        let end = at
            .checked_add(prefix)
            .ok_or(MediaFileError::Limit("NAL offset"))?;
        let head = data
            .get(at..end)
            .ok_or_else(|| MediaFileError::Decode("truncated AVC length prefix".into()))?;
        let size = head
            .iter()
            .fold(0usize, |value, b| (value << 8) | usize::from(*b));
        at = end;
        let end = at
            .checked_add(size)
            .ok_or(MediaFileError::Limit("NAL size"))?;
        let nal = data
            .get(at..end)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| MediaFileError::Decode("invalid AVC NAL bounds".into()))?;
        if nal[0]&31==7 {
            let (width,height)=sps_dimensions(nal)?;
            if padded_pixels(width,height)?>max_pixels {
                return Err(MediaFileError::Unsupported("in-band AVC dimensions exceed the admitted decoder reservation"));
            }
        }
        at = end;
        nals += 1;
        if matches!(nal[0] & 31, 1..=5) {
            pictures += 1;
        }
        if nals > 1024 || pictures > 1 {
            return Err(MediaFileError::Unsupported(
                "file profile requires one H.264 slice per access unit",
            ));
        }
    }
    Ok(())
}
mod raster;
#[cfg(test)]
#[path = "mediafile/tests.rs"]
mod tests;
