//! Stateful native AVC camera access-unit decoder.
//!
//! The camera owner feeds complete Annex-B access units; the decoder may emit
//! pictures later or out of submission order.  Device timestamps therefore
//! follow the original PTS rather than the latest native sample. No device or
//! UI code is called here. Native capture selects it for Windows MF H.264.
use super::{CameraError, CameraLimits, NativeVideoFrame, checked_frame_bytes};
use skylive_h264::{Decoder, DecoderLimits, Picture};
use skylive_media::{CameraMatrix, CameraRange, PackedFrameLimits, Planar420Layout, Chroma420Vertical, Chroma420Halo, ingest_planar_420_with_halo};
use std::collections::VecDeque;

const MAX_ACCESS_UNIT: usize = 8 * 1024 * 1024;
const MAX_CONFIGURATION: usize = 64 * 1024;
const MAX_PENDING: usize = 64;
const MAX_OUTPUTS: usize = 1;
const MAX_NALS: usize = 256;

/// Logical admission used by both the camera owner and the stream. The codec
/// has dimensional/reference bounds but its allocator is not a hard heap cap.
fn admission(width:u32, height:u32, limits:CameraLimits)->Result<(usize,usize),CameraError> {
    checked_frame_bytes(width,height,limits)?;
    if width%2!=0 || height%2!=0 {return Err(CameraError::InvalidFrame("AVC 4:2:0 camera dimensions must be even"));}
    let coded_width=width.checked_add(15).ok_or(CameraError::BudgetExceeded("AVC width"))?/16*16;
    let coded_height=height.checked_add(15).ok_or(CameraError::BudgetExceeded("AVC height"))?/16*16;
    for dpb in (1..=16).rev() {
        let Some(estimate)=skylive_h264::camera_memory_reservation(coded_width,coded_height,dpb,MAX_ACCESS_UNIT,MAX_NALS) else {
            return Err(CameraError::BudgetExceeded("AVC camera reservation overflow"));
        };
        if estimate<=limits.max_raw_bytes{return Ok((estimate,dpb));}
    }
    Err(CameraError::BudgetExceeded("AVC camera reservation"))
}

pub(super) fn reservation(width:u32,height:u32,limits:CameraLimits)->Result<usize,CameraError> {
    admission(width,height,limits).map(|(bytes,_)|bytes)
}

pub(super) fn compressed_cap()->usize {MAX_ACCESS_UNIT}

#[derive(Clone, Copy)]
struct Stamp { pts: i64, device_qpc_ns: Option<u64>, clock_generation: Option<u64>, discontinuity: bool, dropped: u64 }

/// A single camera stream owns this state; no decoded picture crosses epochs.
pub(super) struct CameraAvcDecoder {
    width: u32,
    height: u32,
    decoder: Decoder,
    decoder_limits: DecoderLimits,
    configuration: Option<Vec<u8>>,
    limits: CameraLimits,
    pending: VecDeque<Stamp>,
    /// Retained across parameter-set-only samples until a picture is submitted.
    pending_discontinuity: bool,
    pending_dropped: u64,
}

impl CameraAvcDecoder {
    pub(super) fn new(width: u32, height: u32, limits: CameraLimits, configuration: Option<Vec<u8>>) -> Result<Self, CameraError> {
        let (_,max_dpb_frames)=admission(width,height,limits)?;
        let coded_width = width.checked_add(15).ok_or(CameraError::BudgetExceeded("AVC width"))? / 16 * 16;
        let coded_height = height.checked_add(15).ok_or(CameraError::BudgetExceeded("AVC height"))? / 16 * 16;
        let coded_pixels = u64::from(coded_width) * u64::from(coded_height);
        let decoder_limits = DecoderLimits { max_access_unit_bytes: MAX_ACCESS_UNIT,
            max_nal_units: MAX_NALS, max_pictures_per_access_unit: 1, max_output_pictures_per_decode: MAX_OUTPUTS,
            max_coded_width: coded_width, max_coded_height: coded_height,
            max_coded_pixels: coded_pixels, max_reference_frames: max_dpb_frames as u32, max_dpb_frames };
        let decoder = configured_decoder(decoder_limits, configuration.as_deref())?;
        Ok(Self {width, height, decoder, decoder_limits, configuration, limits, pending: VecDeque::new(),
            pending_discontinuity:false, pending_dropped:0})
    }

    /// Decode one complete Annex-B access unit. A discontinuity drops the
    /// decoder and timestamps rather than matching pictures to a new stream.
    pub(super) fn submit(&mut self, bytes: &[u8], pts_100ns: i64, device_qpc_ns: Option<u64>, clock_generation:Option<u64>,
        discontinuity: bool, dropped: u64) -> Result<Vec<(NativeVideoFrame,Option<u64>)>, CameraError> {
        if bytes.is_empty() || bytes.len() > MAX_ACCESS_UNIT {
            return Err(CameraError::BudgetExceeded("AVC access unit bytes"));
        }
        if discontinuity {
            self.decoder = configured_decoder(self.decoder_limits,self.configuration.as_deref())?;
            self.pending.clear();
            self.pending_discontinuity = true;
        }
        self.pending_dropped = self.pending_dropped.saturating_add(dropped);
        let slice_count=scan_slices(bytes,self.decoder.nal_length_size())?;
        // Multiple slices can form one picture. The decoder checks actual
        // H.264 picture boundaries before allocation and rejects a second
        // picture so one sample PTS cannot be assigned to multiple pictures.
        if slice_count > 0 {
            if self.pending.len() >= MAX_PENDING || self.pending.iter().any(|stamp|stamp.pts == pts_100ns) {
                return Err(CameraError::InvalidFrame("AVC camera timestamps are duplicate or pending metadata is full"));
            }
            self.pending.push_back(Stamp {pts:pts_100ns, device_qpc_ns, clock_generation,
                discontinuity:std::mem::take(&mut self.pending_discontinuity),
                dropped:std::mem::take(&mut self.pending_dropped)});
        }
        let pictures = self.decoder.decode(bytes, pts_100ns).map_err(codec_error)?;
        self.frames(pictures)
    }

    /// A single input may make several reordered pictures ready. Materialize
    /// only the admitted batch and drain further batches on subsequent polls.
    pub(super) fn drain(&mut self)->Result<Vec<(NativeVideoFrame,Option<u64>)>,CameraError> {
        let pictures=self.decoder.decode(&[],0).map_err(codec_error)?;
        self.frames(pictures)
    }

    fn frames(&mut self,pictures:Vec<Picture>)->Result<Vec<(NativeVideoFrame,Option<u64>)>,CameraError> {
        if pictures.len() > MAX_OUTPUTS { return Err(CameraError::BudgetExceeded("AVC decoded picture count")); }
        let mut output = Vec::with_capacity(pictures.len());
        for picture in pictures {
            let index = self.pending.iter().position(|stamp| stamp.pts == picture.pts)
                .ok_or(CameraError::InvalidFrame("AVC decoded picture has no matching input timestamp"))?;
            let stamp = self.pending.remove(index).ok_or(CameraError::InvalidFrame("AVC timestamp queue changed"))?;
            let image = self.convert(picture)?;
            output.push((NativeVideoFrame { image, presentation_ns:i128::from(stamp.pts)*100,
                device_qpc_ns:stamp.device_qpc_ns, directshow_clock:None,
                discontinuity:stamp.discontinuity, dropped_frames:stamp.dropped },stamp.clock_generation));
        }
        Ok(output)
    }

    fn convert(&self, picture: Picture) -> Result<skylive_media::RgbaFrame, CameraError> {
        if (picture.width,picture.height)!=(self.width,self.height) || picture.draft
            || picture.sar != (0,0) && picture.sar.0 != picture.sar.1 {
            return Err(CameraError::InvalidFrame("AVC decoded geometry, aspect ratio or draft mode is unsupported"));
        }
        if matches!(picture.color.transfer, 16 | 18) {
            return Err(CameraError::InvalidFrame("HDR AVC camera requires tone mapping"));
        }
        let matrix = match picture.color.matrix {
            1 | 2 => CameraMatrix::Bt709, // unspecified uses OBS SDR default
            4 => CameraMatrix::Fcc,
            5 | 6 => CameraMatrix::Bt601,
            7 => CameraMatrix::Smpte240M,
            _ => return Err(CameraError::InvalidFrame("AVC camera color matrix is unsupported")),
        };
        // For progressive frames H.264 uses the top-field location; the
        // bottom-field value only describes interlaced field sampling.
        let (chroma_left, vertical) = match picture.chroma_location.map(|(top, _)|top).unwrap_or(0) {
            0 => (true, Chroma420Vertical::Center),
            1 => (false, Chroma420Vertical::Center),
            2 => (true, Chroma420Vertical::Top),
            3 => (false, Chroma420Vertical::Top),
            4 => (true, Chroma420Vertical::Bottom),
            5 => (false, Chroma420Vertical::Bottom),
            _ => return Err(CameraError::InvalidFrame("AVC camera chroma location is unsupported")),
        };
        let range = if picture.color.full_range {CameraRange::Full} else {CameraRange::Limited};
        let halo = picture.chroma_halo.as_ref().map(|h| Chroma420Halo {
            u: &h.u, v: &h.v, stride: h.stride, width: h.width, height: h.height,
            origin_x: h.origin_x, origin_y: h.origin_y,
        });
        ingest_planar_420_with_halo(&picture.y, &picture.u, &picture.v,
            Planar420Layout {width:self.width,height:self.height,y_stride:picture.y_stride,
                uv_stride:picture.uv_stride,matrix,range,chroma_left},
            PackedFrameLimits {max_input_bytes:self.limits.max_raw_bytes,
                max_output_bytes:self.limits.max_frame_bytes,
                max_pixel_visits:u64::from(self.width)*u64::from(self.height)*6}, vertical, halo)
            .map_err(|_|CameraError::InvalidFrame("AVC camera planar conversion failed"))
    }
}

/// Inspect only access-unit framing and NAL kinds; picture boundaries are
/// established by the codec using SPS/PPS and slice headers. No allocation.
fn scan_slices(bytes:&[u8], length_size:Option<usize>)->Result<usize,CameraError> {
    let mut count=0usize;
    let mut nals=0usize;
    if let Some(size)=length_size {
        if !(1..=4).contains(&size){return Err(CameraError::InvalidFrame("invalid AVC NAL length size"));}
        let mut offset=0usize;
        while offset<bytes.len() {
            if size>bytes.len()-offset{return Err(CameraError::InvalidFrame("truncated AVC NAL length"));}
            let mut length=0usize;
            for byte in &bytes[offset..offset+size] {length=(length<<8)|usize::from(*byte);}
            offset+=size;
            if length==0 || length>bytes.len()-offset{return Err(CameraError::InvalidFrame("invalid AVC NAL length"));}
            nals+=1;
            if nals>MAX_NALS{return Err(CameraError::BudgetExceeded("AVC access unit NAL count"));}
            if matches!(bytes[offset]&0x1f,1|5){count+=1;}
            offset+=length;
        }
    } else {
        for index in 0..bytes.len().saturating_sub(3) {
            if bytes[index..index+3]==[0,0,1] {
                nals+=1;
                if nals>MAX_NALS{return Err(CameraError::BudgetExceeded("AVC access unit NAL count"));}
                if matches!(bytes[index+3]&0x1f,1|5){count+=1;}
            }
        }
    }
    if nals==0 || nals>MAX_NALS{return Err(CameraError::InvalidFrame("AVC access unit has no valid NALs or too many NALs"));}
    Ok(count)
}

fn configured_decoder(limits:DecoderLimits,configuration:Option<&[u8]>)->Result<Decoder,CameraError> {
    let mut decoder=Decoder::with_limits(limits).map_err(codec_error)?;
    decoder.set_output_chroma_halo(true);
    if let Some(bytes)=configuration {
        if bytes.is_empty() || bytes.len()>MAX_CONFIGURATION {return Err(CameraError::InvalidFrame("invalid AVC sequence header size"));}
        if bytes[0]==1 {
            decoder.configure_avcc(bytes).map_err(codec_error)?;
        } else {
            if scan_slices(bytes,None)?!=0{return Err(CameraError::InvalidFrame("AVC sequence header contains video slices"));}
            if !decoder.decode(bytes,0).map_err(codec_error)?.is_empty(){return Err(CameraError::InvalidFrame("AVC sequence header unexpectedly produced video"));}
        }
    }
    Ok(decoder)
}

fn codec_error(error: skylive_h264::Error) -> CameraError {
    CameraError::Native { operation:"AVC camera decoder", detail:error.to_string() }
}
