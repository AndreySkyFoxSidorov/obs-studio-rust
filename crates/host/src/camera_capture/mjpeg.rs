//! A complete MJPEG frame is decoded on the capture owner, never its callback.
//! Baseline or extended-sequential 8-bit, one interleaved scan,1/3 components, including bounded
//! 4:1:1 and 4:1:0 sampling. Exact
//! SOF dimensions are checked before constructing image's locked JPEG decoder.
use super::{CameraError,CameraLimits,checked_frame_bytes};
use skylive_core::{VideoCaptureMode,VideoPixelFormat,VideoColorMatrix,VideoColorRange};
use skylive_media::RgbaFrame;
use image::{ImageDecoder,DynamicImage,Limits,codecs::jpeg::JpegDecoder};
use std::io::Cursor;
const MAX_COMPRESSED:usize=16*1024*1024;
const MAX_HEADER:usize=1024*1024;
pub(super) fn compressed_cap(mode:VideoCaptureMode,limits:CameraLimits)->Result<usize,CameraError>{
    checked_frame_bytes(mode.width(),mode.height(),limits)?.checked_add(65536).map(|n|n.min(MAX_COMPRESSED)).ok_or(CameraError::BudgetExceeded("MJPEG input bound"))
}
pub(super) fn native_cap(mode:VideoCaptureMode,limits:CameraLimits)->Result<usize,CameraError>{
    checked_frame_bytes(mode.width(),mode.height(),limits)?.checked_add(65536).ok_or(CameraError::BudgetExceeded("MJPEG native buffer capacity bound"))
}
pub(super) fn reservation(mode:VideoCaptureMode,limits:CameraLimits)->Result<usize,CameraError>{
    let output=checked_frame_bytes(mode.width(),mode.height(),limits)?;
    let encoded=compressed_cap(mode,limits)?;
    // Cooperative SDK slots, optional AVI1 normalization and image decoder's
    // owned encoded snapshot, metadata, native raster/RGBA and row scratch. The
    // codec's max_alloc is best-effort; structural preflight bounds dimensions,
    // components/sampling/scan type and headers before either decoder creation.
    let bytes=native_cap(mode,limits)?.checked_mul(3).and_then(|n|n.checked_add(encoded.checked_mul(3)?)).and_then(|n|n.checked_add(output.checked_mul(4)?)).and_then(|n|n.checked_add(4*1024*1024)).ok_or(CameraError::BudgetExceeded("MJPEG reservation overflow"))?;
    if bytes>limits.max_raw_bytes{return Err(CameraError::BudgetExceeded("MJPEG native/decode reservation"));}Ok(bytes)
}
fn invalid(detail:&'static str)->CameraError{CameraError::InvalidFrame(detail)}
/// Return whether the standard MJPEG default-table path needs its AVI1 tag.
#[derive(Default)]
pub(super) struct ColorState { itu601: bool }

// FFmpeg compares COM as a C string after stripping a final LF. Its encoder
// writes a terminating NUL; comments with trailing bytes after NUL compare
// by the same prefix as `strcmp`.
fn is_itu601_comment(data:&[u8])->bool {
    let data=if data.last()==Some(&b'\n') {&data[..data.len()-1]} else {data};
    data.split(|byte|*byte==0).next()==Some(b"CS=ITU601".as_slice())
}

fn preflight(bytes:&[u8],mode:VideoCaptureMode)->Result<(bool,bool,bool,usize),CameraError>{
    if !bytes.starts_with(&[0xff,0xd8])||bytes.len()<4{return Err(invalid("MJPEG frame has no JPEG SOI"));}
    let mut position=2usize;let mut sof=None;let mut dht=false;let mut avi=false;let mut before_sof=false;let mut after_sof=false;
    for _ in 0..4096 {
        if position>MAX_HEADER||bytes.get(position)!=Some(&0xff){return Err(invalid("MJPEG header marker or header budget"));}
        while bytes.get(position)==Some(&0xff){position+=1;}
        let marker=*bytes.get(position).ok_or(invalid("truncated MJPEG marker"))?;position+=1;
        if marker==0xd9||marker==0xd8||marker==0||marker==1||(0xd0..=0xd7).contains(&marker){return Err(invalid("unexpected standalone MJPEG header marker"));}
        let length=bytes.get(position..position+2).ok_or(invalid("truncated MJPEG segment length"))?;
        let length=usize::from(u16::from_be_bytes([length[0],length[1]]));
        if length<2{return Err(invalid("invalid MJPEG segment length"));}
        let end=position.checked_add(length).ok_or(invalid("MJPEG segment overflow"))?;
        let data=bytes.get(position+2..end).ok_or(invalid("truncated MJPEG segment"))?;
        if end>MAX_HEADER{return Err(CameraError::BudgetExceeded("MJPEG header exceeds1MiB"));}
        match marker {
            0xc0|0xc1=>{
                if sof.is_some()||data.len()<6||data[0]!=8{return Err(invalid("MJPEG requires one 8-bit sequential SOF"));}
                let height=u32::from(u16::from_be_bytes([data[1],data[2]]));let width=u32::from(u16::from_be_bytes([data[3],data[4]]));
                if (width,height)!=(mode.width(),mode.height()){return Err(invalid("MJPEG JPEG dimensions disagree with the negotiated camera mode"));}
                let components=usize::from(data[5]);
                if !matches!(components,1|3)||data.len()!=6+3*components{return Err(invalid("MJPEG supports grayscale or three-component baseline JPEG"));}
                let mut ids=[0u8;3];let mut factors=[(0u8,0u8);3];let mut blocks=0u32;
                for (i,component) in data[6..].chunks_exact(3).enumerate(){
                    let (h,v)=(component[1]>>4,component[1]&15);
                    if !matches!(h,1|2|4)||!(1..=2).contains(&v)||component[2]>3||ids[..i].contains(&component[0]){return Err(invalid("MJPEG component sampling, table or identity is unsupported"));}
                    blocks+=u32::from(h)*u32::from(v);factors[i]=(h,v);ids[i]=component[0];
                }
                // Retain the bounded baseline decoder contract. The only new
                // four-wide layouts are standard YCbCr 4:1:1 and 4:1:0.
                if factors[..components].iter().any(|(h,_)|*h==4)
                    && !(blocks<=10 && components==3 && matches!(factors[0],(4,1)|(4,2)) && factors[1]==(1,1) && factors[2]==(1,1)) {
                    return Err(invalid("MJPEG component sampling exceeds supported MCU layout"));
                }
                sof=Some((components,ids));
            },
            0xc2..=0xcf if !matches!(marker,0xc4|0xc8|0xcc)=>return Err(invalid("progressive, lossless or non-sequential camera JPEG is unsupported")),
            0xc8|0xcc|0xdc=>return Err(invalid("MJPEG hierarchical/arithmetic/DNL frame is unsupported")),
            0xc4=>dht=true,
            0xe0=>avi|=data.starts_with(b"AVI1\0"),
            0xfe if is_itu601_comment(data)=>{if sof.is_some(){after_sof=true;}else{before_sof=true;}},
            0xda=>{
                let (components,ids)=sof.ok_or(invalid("MJPEG scan precedes its SOF"))?;
                if data.len()!=1+2*components+3||usize::from(data[0])!=components||data[data.len()-3..]!=[0,63,0]{return Err(invalid("MJPEG requires one complete interleaved baseline scan"));}
                let mut seen=[0u8;3];
                for (i,scan) in data[1..1+2*components].chunks_exact(2).enumerate(){
                    if !ids[..components].contains(&scan[0])||seen[..i].contains(&scan[0])||scan[1]>>4>3||scan[1]&15>3{return Err(invalid("MJPEG scan component/table identity is unsupported"));}
                    if !dht&&((scan[1]>>4)>1||(scan[1]&15)>1){return Err(invalid("abbreviated MJPEG requires standard Huffman table selectors0/1"));}
                    seen[i]=scan[0];
                }
                // Structurally scan entropy bytes. No hidden second SOF/scan
                // may bypass the pre-allocation baseline/component checks.
                let mut p=end;
                while p<bytes.len(){
                    // Most entropy bytes are not markers. Scan contiguous data
                    // with the optimized byte search, then validate every 0xff
                    // escape/restart/EOI exactly as before.
                    let Some(next)=memchr::memchr(0xff,&bytes[p..]) else {return Err(invalid("MJPEG frame has no complete EOI"));};
                    p+=next+1;while bytes.get(p)==Some(&0xff){p+=1;}
                    let value=*bytes.get(p).ok_or(invalid("truncated MJPEG entropy marker"))?;p+=1;
                    match value {0|0xd0..=0xd7=>{},0xd9=>{
                        // A camera sample may have nonzero allocation padding
                        // after the JPEG end marker. Decode only the validated
                        // first image, as the OBS/FFmpeg decoder does, rather
                        // than treating those unrelated bytes as image data.
                        return Ok((!dht&&!avi,before_sof,after_sof,p));
                    },_=>return Err(invalid("additional MJPEG scans/frames are unsupported")),}
                }
                return Err(invalid("MJPEG frame has no complete EOI"));
            },
            _=>{},
        }
        position=end;
    }
    Err(CameraError::BudgetExceeded("MJPEG marker count"))
}
pub(super) fn decode(bytes:&[u8],mode:VideoCaptureMode,limits:CameraLimits)->Result<RgbaFrame,CameraError>{
    if mode.format()!=VideoPixelFormat::Mjpeg{return Err(invalid("compressed JPEG decoder requires the MJPEG transport"));}
    if bytes.is_empty()||bytes.len()>compressed_cap(mode,limits)?{return Err(CameraError::BudgetExceeded("compressed MJPEG sample bytes"));}
    reservation(mode,limits)?;
    let (insert_avi,_,_,end)=preflight(bytes,mode)?;
    decode_preflighted(&bytes[..end],mode,limits,insert_avi)
}
fn decode_preflighted(bytes:&[u8],mode:VideoCaptureMode,limits:CameraLimits,insert_avi:bool)->Result<RgbaFrame,CameraError>{
    // Locked zune-jpeg already handles AVI1 MJPEG's default DHT. Tag only an
    // actually abbreviated standard-table MJPEG frame; preserve all image data.
    let normalized;
    let input=if insert_avi {
        let mut value=Vec::new();value.try_reserve_exact(bytes.len()+9).map_err(|_|CameraError::BudgetExceeded("MJPEG AVI1 snapshot allocation"))?;
        value.extend_from_slice(&bytes[..2]);value.extend_from_slice(b"\xff\xe0\x00\x07AVI1\0");value.extend_from_slice(&bytes[2..]);normalized=value;&normalized[..]
    }else{bytes};
    let mut decoder=JpegDecoder::new(Cursor::new(input)).map_err(|e|CameraError::Native {operation:"MJPEG JPEG header",detail:e.to_string()})?;
    if decoder.dimensions()!=(mode.width(),mode.height()){return Err(invalid("MJPEG decoder dimensions changed after preflight"));}
    let output=checked_frame_bytes(mode.width(),mode.height(),limits)?;
    if decoder.total_bytes()>output as u64{return Err(CameraError::BudgetExceeded("MJPEG native decoded raster"));}
    let mut budget=Limits::no_limits();budget.max_image_width=Some(mode.width());budget.max_image_height=Some(mode.height());budget.max_alloc=Some((output*3+4*1024*1024)as u64);
    decoder.set_limits(budget).map_err(|e|CameraError::Native {operation:"MJPEG decoder limits",detail:e.to_string()})?;
    let rgba=DynamicImage::from_decoder(decoder).map_err(|e|CameraError::Native {operation:"MJPEG JPEG pixels",detail:e.to_string()})?.into_rgba8();
    if rgba.dimensions()!=(mode.width(),mode.height())||rgba.as_raw().len()!=output{return Err(invalid("MJPEG decoded image disagrees with the negotiated mode"));}
    RgbaFrame::new(mode.width(),mode.height(),rgba.into_raw()).map_err(|_|invalid("MJPEG decoded RGBA layout"))
}

/// Explicit SDR interpretation is applied to decoded JPEG components before
/// RGB clipping. For an RGB-coded JPEG there is no YCbCr matrix to override.
pub(super) fn decode_with_color(bytes:&[u8],mode:VideoCaptureMode,limits:CameraLimits,matrix:Option<VideoColorMatrix>,range:Option<VideoColorRange>,state:&mut ColorState)->Result<RgbaFrame,CameraError>{
    if mode.format()!=VideoPixelFormat::Mjpeg{return Err(invalid("compressed JPEG decoder requires the MJPEG transport"));}
    if bytes.is_empty()||bytes.len()>compressed_cap(mode,limits)?{return Err(CameraError::BudgetExceeded("compressed MJPEG sample bytes"));}
    reservation(mode,limits)?;
    let (insert_avi,before_sof,after_sof,end)=preflight(bytes,mode)?;
    let bytes=&bytes[..end];
    // FFmpeg resolves pixel format/range at SOF. A later COM is sticky for
    // subsequent frames but does not retroactively change this frame.
    let itu601=state.itu601||before_sof;
    state.itu601|=before_sof||after_sof;
    if matrix.is_none()&&range.is_none()&&!itu601{return decode_preflighted(bytes,mode,limits,insert_avi);}
    let normalized;
    let input=if insert_avi {let mut value=Vec::new();value.try_reserve_exact(bytes.len()+9).map_err(|_|CameraError::BudgetExceeded("MJPEG AVI1 snapshot allocation"))?;value.extend_from_slice(&bytes[..2]);value.extend_from_slice(b"\xff\xe0\x00\x07AVI1\0");value.extend_from_slice(&bytes[2..]);normalized=value;&normalized[..]}else{bytes};
    use zune_jpeg::zune_core::{bytestream::ZCursor,colorspace::ColorSpace,options::DecoderOptions};
    let options=DecoderOptions::default().set_max_width(mode.width() as usize).set_max_height(mode.height() as usize).jpeg_set_out_colorspace(ColorSpace::YCbCr);
    let mut decoder=zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(input),options);
    decoder.decode_headers().map_err(|error|CameraError::Native{operation:"MJPEG JPEG header",detail:error.to_string()})?;
    let colorspace=decoder.input_colorspace().ok_or(invalid("MJPEG color metadata unavailable"))?;
    if colorspace==ColorSpace::RGB{return decode_preflighted(bytes,mode,limits,insert_avi);}
    let luma=colorspace==ColorSpace::Luma;
    if !luma&&colorspace!=ColorSpace::YCbCr{return Err(invalid("MJPEG color override requires YCbCr or grayscale"));}
    if luma {decoder.set_options(options.jpeg_set_out_colorspace(ColorSpace::Luma));}
    let expected=checked_frame_bytes(mode.width(),mode.height(),limits)?/4*3;
    if decoder.output_buffer_size()!=Some(if luma {expected/3}else{expected}){return Err(invalid("MJPEG YCbCr output layout"));}
    let decoded=decoder.decode().map_err(|error|CameraError::Native{operation:"MJPEG YCbCr pixels",detail:error.to_string()})?;
    let components=if luma {let mut values=Vec::new();values.try_reserve_exact(expected).map_err(|_|CameraError::BudgetExceeded("MJPEG grayscale expansion"))?;for y in decoded {values.extend_from_slice(&[y,128,128]);}values}else{decoded};
    let matrix=match matrix.unwrap_or(VideoColorMatrix::Bt601){VideoColorMatrix::Bt601=>skylive_media::CameraMatrix::Bt601,VideoColorMatrix::Bt709=>skylive_media::CameraMatrix::Bt709};
    let range=match range {Some(VideoColorRange::Limited)=>skylive_media::CameraRange::Limited,Some(VideoColorRange::Full)=>skylive_media::CameraRange::Full,None if itu601=>skylive_media::CameraRange::Limited,None=>skylive_media::CameraRange::Full};
    skylive_media::ingest_ycbcr444(&components,mode.width(),mode.height(),matrix,range,skylive_media::PackedFrameLimits{max_input_bytes:limits.max_raw_bytes,max_output_bytes:limits.max_frame_bytes,max_pixel_visits:u64::from(mode.width())*u64::from(mode.height())}).map_err(|_|invalid("MJPEG YCbCr conversion failed"))
}
