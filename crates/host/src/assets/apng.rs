//! Bounded APNG decoding into the shared full-frame animation timeline.
//!
//! The PNG decoder composes SOURCE/OVER and disposal operations. Scan all
//! frame-control rectangles and admission costs before allowing pixel decode;
//! retain the encoded snapshot and charge all logical-frame work on failure.
use super::{dimensions, limits, malformed, decoder_error, AssetError, DecodeBudget, MAX_ANIMATION_BYTES, MAX_ANIMATION_FRAMES};
use image::{AnimationDecoder, ImageDecoder};
use skylive_media::{AnimationClip, AnimationFrame, AnimationLimits, AnimationLoop, RgbaFrame};
use std::{io::Cursor, sync::Arc};

struct Plan { width:u32, height:u32, durations:Vec<u64>, loops:AnimationLoop, admitted_bytes:usize }
fn be(bytes:&[u8])->Result<u32,AssetError> {
    Ok(u32::from_be_bytes(bytes.try_into().map_err(|_|malformed())?))
}
fn scan(bytes:&[u8], expected:Option<(u32,u32)>, max_decoded_bytes:usize)->Result<Plan,AssetError> {
    if bytes.get(..8)!=Some(b"\x89PNG\r\n\x1a\n") {return Err(malformed());}
    let mut pos=8usize;
    let mut canvas=None;
    let mut control=None;
    let mut durations=Vec::new();
    let mut seen_data=false;
    let mut seen_frame_data=false;
    let mut frame_open=false;
    let mut first_frame_uses_idat=false;
    let mut seen_fdat=false;
    let mut next_sequence=0u32;
    let mut ended=false;
    while pos<bytes.len() {
        let end_header=pos.checked_add(8).ok_or_else(malformed)?;
        let header=bytes.get(pos..end_header).ok_or_else(malformed)?;
        let size=usize::try_from(be(&header[..4])?).map_err(|_|malformed())?;
        let end=end_header.checked_add(size).and_then(|n|n.checked_add(4)).ok_or_else(malformed)?;
        let data=bytes.get(end_header..end.checked_sub(4).ok_or_else(malformed)?).ok_or_else(malformed)?;
        if end>bytes.len(){return Err(malformed());}
        match &header[4..8] {
            b"IHDR" if pos==8 && size==13 => {let w=be(&data[..4])?;let h=be(&data[4..8])?;dimensions(w,h)?;canvas=Some((w,h));},
            b"acTL" if size==8 && !seen_data && control.is_none()=>{
                let count=be(&data[..4])?;
                if count==0 || count as usize>MAX_ANIMATION_FRAMES {return Err(AssetError::limit("image_limit","APNG frame count exceeds the supported limit"));}
                control=Some((count,be(&data[4..8])?));
            },
            b"fcTL" if size==26 && control.is_some()=>{
                if frame_open && !seen_frame_data {return Err(malformed());}
                if be(&data[..4])?!=next_sequence {return Err(malformed());}
                next_sequence=next_sequence.checked_add(1).ok_or_else(malformed)?;
                if durations.is_empty() {first_frame_uses_idat=!seen_data;}
                frame_open=true;seen_frame_data=false;
                let (cw,ch)=canvas.ok_or_else(malformed)?;
                let w=be(&data[4..8])?;let h=be(&data[8..12])?;let x=be(&data[12..16])?;let y=be(&data[16..20])?;
                if w==0 || h==0 || x.checked_add(w).is_none_or(|v|v>cw) || y.checked_add(h).is_none_or(|v|v>ch)
                    || data[24]>2 || data[25]>1 {return Err(malformed());}
                if durations.len()>=MAX_ANIMATION_FRAMES {return Err(AssetError::limit("image_limit","APNG frame count exceeds the supported limit"));}
                let num=u16::from_be_bytes([data[20],data[21]]) as u64;
                let den=u16::from_be_bytes([data[22],data[23]]) as u64;
                // Zero is a valid APNG delay. A 1 ns duration preserves an
                // effectively immediate frame while keeping the timeline finite.
                durations.push((num*1_000_000_000/if den==0{100}else{den}).max(1));
            },
            b"IDAT"=>{if seen_fdat || (frame_open && (!first_frame_uses_idat || durations.len()>1)){return Err(malformed());}seen_data=true;if frame_open{seen_frame_data=true;}},
            b"fdAT" if size>=4 && seen_data && frame_open=>{
                if be(&data[..4])?!=next_sequence {return Err(malformed());}
                next_sequence=next_sequence.checked_add(1).ok_or_else(malformed)?;
                seen_fdat=true;seen_frame_data=true;
            },
            b"IEND" if size==0=>{if !seen_frame_data{return Err(malformed());}ended=true;pos=end;break;},
            b"IHDR"|b"acTL"|b"fcTL"|b"fdAT"|b"IEND"=>return Err(malformed()),
            _=>{},
        }
        pos=end;
    }
    let (width,height)=canvas.ok_or_else(malformed)?;
    if expected.is_some_and(|v|v!=(width,height)){return Err(AssetError::rejected("image_dimensions_changed","linked APNG dimensions disagree with the recorded asset"));}
    let (count,plays)=control.ok_or_else(malformed)?;
    if !ended || pos!=bytes.len() || durations.len()!=count as usize {return Err(malformed());}
    let canvas_bytes=(width as usize).checked_mul(height as usize).and_then(|v|v.checked_mul(4)).ok_or_else(malformed)?;
    let decoded_bytes=canvas_bytes.checked_mul(durations.len()).ok_or_else(malformed)?;
    // image 0.25's APNG compositor retains current and previous canvases.
    // Its Background disposal also builds a Vec<(u32,u32,Rgba<u8>)> for the
    // disposed rectangle (up to three canvas-sized buffers). Other paths may
    // instead overlap native frame data and its RGBA conversion. Account for
    // the five-canvas worst case in addition to retained full-frame output.
    // The PNG codec's own small/internal scratch remains subject to the
    // library's best-effort allocation limit, not a process-wide guarantee.
    // The 16-bit compositor also needs two RGBA16 canvases and one decoded
    // RGBA16 frame, or six RGBA8 canvas equivalents.
    let scratch=canvas_bytes.checked_mul(if bytes[24]==16 {6} else {5}).ok_or_else(malformed)?;
    let admitted_bytes=decoded_bytes.checked_add(scratch).ok_or_else(malformed)?;
    if admitted_bytes>max_decoded_bytes.min(MAX_ANIMATION_BYTES){return Err(AssetError::decoded_capacity("APNG frames and compositor scratch exceed the remaining decoded byte budget"));}
    Ok(Plan {width,height,durations,loops:if plays==0{AnimationLoop::Infinite}else{AnimationLoop::Finite(plays)},admitted_bytes})
}

pub(super) fn decode_if_animated(bytes:&[u8],expected:Option<(u32,u32)>,max_decoded_bytes:usize,decoded_work:&mut DecodeBudget)->Result<Option<Arc<AnimationClip>>,AssetError> {
    let decoder=image::codecs::png::PngDecoder::with_limits(Cursor::new(bytes),limits()).map_err(decoder_error)?;
    if !decoder.is_apng().map_err(decoder_error)? {return Ok(None);}
    let plan=scan(bytes,expected,max_decoded_bytes)?;
    if decoder.dimensions()!=(plan.width,plan.height){return Err(malformed());}
    decoded_work.reserve(plan.admitted_bytes)?;
    if bytes[24]==16 {return decode_16(bytes,plan,max_decoded_bytes).map(Some);}
    let decoder=decoder.apng().map_err(decoder_error)?;
    let mut frames=Vec::with_capacity(plan.durations.len());
    let mut decoded=decoder.into_frames();
    for duration_ns in plan.durations {
        let frame=decoded.next().ok_or_else(malformed)?.map_err(decoder_error)?;
        let image=frame.into_buffer();
        if image.dimensions()!=(plan.width,plan.height){return Err(malformed());}
        let image=RgbaFrame::new(plan.width,plan.height,image.into_raw()).map_err(|e|AssetError::caused("image_decode","invalid APNG frame",e))?;
        frames.push(AnimationFrame {image:Arc::new(image),duration_ns});
    }
    let clip=if frames.len()==1 {AnimationClip::static_frame(frames.remove(0).image)} else {
        AnimationClip::new(frames,plan.loops,AnimationLimits {max_frames:MAX_ANIMATION_FRAMES,max_bytes:max_decoded_bytes.min(MAX_ANIMATION_BYTES)})
            .map_err(|e|AssetError::caused("image_decode","invalid APNG animation",e))?
    };
    Ok(Some(Arc::new(clip)))
}

/// Compose in 16-bit straight-alpha space before reducing to the application's
/// RGBA8 frame format. PNG samples remain big endian in the decoder output.
fn decode_16(bytes:&[u8],plan:Plan,max_decoded_bytes:usize)->Result<Arc<AnimationClip>,AssetError>{
    let mut decoder=png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    decoder.set_limits(png::Limits {bytes:plan.admitted_bytes});
    let mut reader=decoder.read_info().map_err(|e|AssetError::caused("image_decode","invalid APNG",e))?;
    let size=reader.output_buffer_size().ok_or_else(malformed)?;
    let canvas_len=(plan.width as usize).checked_mul(plan.height as usize).ok_or_else(malformed)?;
    if size>canvas_len.checked_mul(8).ok_or_else(malformed)? {return Err(malformed());}
    let mut raw=vec![0u8;size];
    let mut canvas=vec![[0u16;4];canvas_len];
    let mut previous=vec![[0u16;4];canvas_len];
    let mut disposal:Option<png::FrameControl>=None;
    // A default PNG image may be a non-animated thumbnail before the first
    // fcTL. It is decoded and discarded, as in image's APNG decoder.
    if reader.info().frame_control().is_none(){reader.next_frame(&mut raw).map_err(|e|AssetError::caused("image_decode","invalid APNG default image",e))?;}
    let mut frames=Vec::with_capacity(plan.durations.len());
    for duration_ns in plan.durations {
        if let Some(old)=disposal {
            match old.dispose_op {
                png::DisposeOp::None=>{},
                png::DisposeOp::Previous=>canvas.copy_from_slice(&previous),
                png::DisposeOp::Background=>for y in old.y_offset..old.y_offset+old.height {
                    let start=y as usize*plan.width as usize+old.x_offset as usize;
                    canvas[start..start+old.width as usize].fill([0;4]);
                },
            }
        }
        let output=reader.next_frame(&mut raw).map_err(|e|AssetError::caused("image_decode","invalid APNG frame",e))?;
        let control=*reader.info().frame_control().ok_or_else(malformed)?;
        if output.bit_depth!=png::BitDepth::Sixteen || output.width!=control.width || output.height!=control.height {return Err(malformed());}
        let channels=match output.color_type {png::ColorType::Grayscale=>1,png::ColorType::GrayscaleAlpha=>2,png::ColorType::Rgb=>3,png::ColorType::Rgba=>4,_=>return Err(malformed())};
        let expected=(control.width as usize).checked_mul(control.height as usize).and_then(|v|v.checked_mul(channels*2)).ok_or_else(malformed)?;
        if output.buffer_size()!=expected || expected>raw.len(){return Err(malformed());}
        if control.dispose_op==png::DisposeOp::Previous {previous.copy_from_slice(&canvas);}
        let max=65535u64;
        for (index,sample) in raw[..expected].chunks_exact(channels*2).enumerate(){
            let v=|i:usize|u16::from_be_bytes([sample[i*2],sample[i*2+1]]);
            let source=match channels {1=>[v(0),v(0),v(0),u16::MAX],2=>[v(0),v(0),v(0),v(1)],3=>[v(0),v(1),v(2),u16::MAX],4=>[v(0),v(1),v(2),v(3)],_=>unreachable!()};
            let x=index%(control.width as usize)+control.x_offset as usize;
            let y=index/(control.width as usize)+control.y_offset as usize;
            let dst=&mut canvas[y*plan.width as usize+x];
            if control.blend_op==png::BlendOp::Source {*dst=source;continue;}
            let sa=u64::from(source[3]);if sa==0 {continue;}
            let da=u64::from(dst[3]);let alpha=sa*max+da*(max-sa);
            for channel in 0..3 {let num=u64::from(source[channel])*sa*max+u64::from(dst[channel])*da*(max-sa);dst[channel]=((num+alpha/2)/alpha) as u16;}
            dst[3]=((alpha+max/2)/max) as u16;
        }
        let mut rgba=Vec::with_capacity(canvas_len.checked_mul(4).ok_or_else(malformed)?);
        for pixel in &canvas {for value in pixel {rgba.push(((u32::from(*value)+128)/257) as u8);}}
        let image=RgbaFrame::new(plan.width,plan.height,rgba).map_err(|e|AssetError::caused("image_decode","invalid APNG frame",e))?;
        frames.push(AnimationFrame {image:Arc::new(image),duration_ns});
        disposal=Some(control);
    }
    let clip=if frames.len()==1 {AnimationClip::static_frame(frames.remove(0).image)} else {
        AnimationClip::new(frames,plan.loops,AnimationLimits {max_frames:MAX_ANIMATION_FRAMES,max_bytes:max_decoded_bytes.min(MAX_ANIMATION_BYTES)})
            .map_err(|e|AssetError::caused("image_decode","invalid APNG animation",e))?
    };
    Ok(Arc::new(clip))
}
