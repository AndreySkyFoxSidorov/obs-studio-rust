//! A complete MJPEG frame is decoded on the capture owner, never its callback.
//! Baseline8-bit, one interleaved scan,1/3 components, sampling factors≤2. Exact
//! SOF dimensions are checked before constructing image's locked JPEG decoder.
use super::{CameraError,CameraLimits,checked_frame_bytes};
use skylive_core::{VideoCaptureMode,VideoPixelFormat};
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
fn preflight(bytes:&[u8],mode:VideoCaptureMode)->Result<bool,CameraError>{
    if !bytes.starts_with(&[0xff,0xd8])||bytes.len()<4{return Err(invalid("MJPEG frame has no JPEG SOI"));}
    let mut position=2usize;let mut sof=None;let mut dht=false;let mut avi=false;
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
            0xc0=>{
                if sof.is_some()||data.len()<6||data[0]!=8{return Err(invalid("MJPEG requires one baseline8-bit SOF"));}
                let height=u32::from(u16::from_be_bytes([data[1],data[2]]));let width=u32::from(u16::from_be_bytes([data[3],data[4]]));
                if (width,height)!=(mode.width(),mode.height()){return Err(invalid("MJPEG JPEG dimensions disagree with the negotiated camera mode"));}
                let components=usize::from(data[5]);
                if !matches!(components,1|3)||data.len()!=6+3*components{return Err(invalid("MJPEG supports grayscale or three-component baseline JPEG"));}
                let mut ids=[0u8;3];
                for (i,component) in data[6..].chunks_exact(3).enumerate(){
                    let (h,v)=(component[1]>>4,component[1]&15);
                    if !(1..=2).contains(&h)||!(1..=2).contains(&v)||component[2]>3||ids[..i].contains(&component[0]){return Err(invalid("MJPEG component sampling, table or identity is unsupported"));}
                    ids[i]=component[0];
                }
                sof=Some((components,ids));
            },
            0xc1..=0xcf if !matches!(marker,0xc4|0xc8|0xcc)=>return Err(invalid("progressive, lossless or non-baseline camera JPEG is unsupported")),
            0xc8|0xcc|0xdc=>return Err(invalid("MJPEG hierarchical/arithmetic/DNL frame is unsupported")),
            0xc4=>dht=true,
            0xe0=>avi|=data.starts_with(b"AVI1\0"),
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
                    if bytes[p]!=0xff {p+=1;continue;}
                    p+=1;while bytes.get(p)==Some(&0xff){p+=1;}
                    let value=*bytes.get(p).ok_or(invalid("truncated MJPEG entropy marker"))?;p+=1;
                    match value {0|0xd0..=0xd7=>{},0xd9=>{
                        if bytes[p..].iter().any(|v|*v!=0&&*v!=0xff){return Err(invalid("multiple images or data after MJPEG EOI"));}
                        return Ok(!dht&&!avi);
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
    let insert_avi=preflight(bytes,mode)?;
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
