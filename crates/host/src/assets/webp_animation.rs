//! Bounded animated WebP decoding for linked image sources.
//!
//! Inspect every frame's coded dimensions before invoking the codec: its VP8
//! path otherwise checks them only after allocating the decoded raster. The
//! complete full-canvas animation is admitted before any frame is decoded.
use super::*;
use image::{AnimationDecoder, ImageDecoder};
use skylive_media::{AnimationFrame, AnimationLimits, AnimationLoop};

pub(super) fn is_animated(bytes: &[u8]) -> bool {
    bytes.get(12..16).is_some_and(|signature| signature == b"VP8X")
        && bytes.get(20).is_some_and(|flags| flags & 2 != 0)
}

fn u24(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16)
}

struct Plan { width: u32, height: u32, durations: Vec<u64>, loops: AnimationLoop, bytes: usize, peak_raster_bytes: usize }

fn scan(bytes: &[u8], expected: Option<(u32,u32)>, limit: usize) -> Result<Plan, AssetError> {
    let mut pos = 0;
    let header = take(bytes, &mut pos, 12)?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WEBP"
        || u64::from(u32::from_le_bytes(header[4..8].try_into().map_err(|_|malformed())?)) + 8 != bytes.len() as u64 {
        return Err(malformed());
    }
    let mut canvas = None;
    let mut loops = None;
    let mut durations = Vec::new();
    while pos < bytes.len() {
        let header = take(bytes, &mut pos, 8)?;
        let size = u32::from_le_bytes(header[4..8].try_into().map_err(|_|malformed())?) as usize;
        let payload = take(bytes, &mut pos, size)?;
        match &header[..4] {
            b"VP8X" => {
                if canvas.is_some() || payload.len()!=10 || payload[0]&2==0 { return Err(malformed()); }
                let size=(u24(&payload[4..7])+1,u24(&payload[7..10])+1);
                dimensions(size.0,size.1)?;
                if expected.is_some_and(|e| e!=size) {return Err(AssetError::rejected("image_dimensions_changed","WebP canvas differs from recorded asset"));}
                canvas=Some(size);
            }
            b"ANIM" => {
                if canvas.is_none() || loops.is_some() || payload.len()!=6 {return Err(malformed());}
                let count=u16::from_le_bytes([payload[4],payload[5]]);
                loops=Some(if count==0 {AnimationLoop::Infinite} else {AnimationLoop::Finite(u32::from(count))});
            }
            b"ANMF" => {
                let (width,height)=canvas.ok_or_else(malformed)?;
                if loops.is_none() || payload.len()<16 || durations.len()>=MAX_ANIMATION_FRAMES {return Err(malformed());}
                let x=u24(&payload[0..3])*2; let y=u24(&payload[3..6])*2;
                let w=u24(&payload[6..9])+1; let h=u24(&payload[9..12])+1;
                if x.checked_add(w).is_none_or(|v|v>width) || y.checked_add(h).is_none_or(|v|v>height) || payload[15]&!3!=0 {return Err(malformed());}
                // A zero duration has no usable frame interval; normalize it
                // to 100 ms, as with the existing GIF zero-delay policy.
                let duration=u24(&payload[12..15]);
                durations.push(u64::from(if duration==0 {100}else{duration})*1_000_000);
                let mut frame_pos=16;
                let mut coded=false;
                let mut alpha=false;
                while frame_pos<payload.len() {
                    let chunk=take(payload,&mut frame_pos,8)?;
                    let len=u32::from_le_bytes(chunk[4..8].try_into().map_err(|_|malformed())?) as usize;
                    let data=take(payload,&mut frame_pos,len)?;
                    match &chunk[..4] {
                        b"ALPH" if !alpha && !coded => alpha=true,
                        b"VP8 " | b"VP8L" if !coded => {
                            if alpha && &chunk[..4]!=b"VP8 " {return Err(malformed());}
                            if webp_coded_dimensions(&chunk[..4],data)? != (w,h) {return Err(AssetError::rejected("image_decode","WebP coded frame dimensions differ from its animation rectangle"));}
                            coded=true;
                        }
                        _=>return Err(malformed()),
                    }
                    if len%2!=0 {take(payload,&mut frame_pos,1)?;}
                }
                if !coded {return Err(malformed());}
            }
            b"VP8 " | b"VP8L" => return Err(malformed()),
            _ => {}
        }
        if size%2!=0 {take(bytes,&mut pos,1)?;}
    }
    let (width,height)=canvas.ok_or_else(malformed)?;
    let loops=loops.ok_or_else(malformed)?;
    if durations.is_empty() {return Err(malformed());}
    let canvas=(width as usize).checked_mul(height as usize).and_then(|v|v.checked_mul(4)).ok_or_else(malformed)?;
    let bytes=canvas.checked_mul(durations.len()).ok_or_else(malformed)?;
    // image-webp retains a full RGBA composition canvas and can hold a frame
    // raster and a RGB conversion buffer while producing a full output frame.
    // Count three additional full RGBA canvases as a conservative raster
    // workspace, before allocating any of them. Codec-internal scratch is not
    // included in this bound (as with the existing static WebP path).
    let peak_raster_bytes=canvas.checked_mul(3).and_then(|v|v.checked_add(bytes)).ok_or_else(malformed)?;
    if peak_raster_bytes>limit.min(MAX_ANIMATION_BYTES) {return Err(AssetError::decoded_capacity("animated WebP retained frames and raster workspace exceed decoded-byte budget"));}
    Ok(Plan{width,height,durations,loops,bytes,peak_raster_bytes})
}

pub(super) fn decode_clip(bytes:&[u8],expected:Option<(u32,u32)>,limit:usize,work:&mut DecodeBudget)->Result<Arc<AnimationClip>,AssetError> {
    let plan=scan(bytes,expected,limit)?;
    if plan.peak_raster_bytes>work.remaining() {return Err(AssetError::limit("image_decode_budget","animated WebP frames and raster workspace exceed remaining aggregate decoded budget"));}
    work.reserve(plan.bytes)?;
    let decoder=image::codecs::webp::WebPDecoder::new(Cursor::new(bytes)).map_err(decoder_error)?;
    if !decoder.has_animation() || decoder.dimensions()!=(plan.width,plan.height) {return Err(malformed());}
    let mut frames=Vec::with_capacity(plan.durations.len());
    let mut iterator=decoder.into_frames();
    for duration_ns in plan.durations {
        let frame=iterator.next().ok_or_else(malformed)?.map_err(decoder_error)?;
        let image=frame.into_buffer();
        if image.dimensions()!=(plan.width,plan.height) {return Err(malformed());}
        let raster=RgbaFrame::new(plan.width,plan.height,image.into_raw())
            .map_err(|error|AssetError::caused("image_decode","invalid WebP animation frame",error))?;
        frames.push(AnimationFrame{image:Arc::new(raster),duration_ns});
    }
    let clip=if frames.len()==1 {AnimationClip::static_frame(frames.remove(0).image)} else {
        AnimationClip::new(frames,plan.loops,AnimationLimits{max_frames:MAX_ANIMATION_FRAMES,max_bytes:limit.min(MAX_ANIMATION_BYTES)})
            .map_err(|error|AssetError::caused("image_decode","invalid WebP animation",error))?
    };
    Ok(Arc::new(clip))
}
