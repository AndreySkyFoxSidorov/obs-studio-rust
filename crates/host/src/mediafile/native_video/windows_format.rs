//! Negotiated system-memory NV12 layout, bounded samples and pure-plane copies.
use super::*;
use std::{mem::size_of,ptr};
#[derive(Clone,Copy)]
pub(super) struct Format {width:u32,height:u32,crop:(u32,u32,u32,u32),stride:usize,compact_stride:usize}
pub(super) fn alignment(value:u32)->Result<(),MediaFileError>{if value==0||(value.is_power_of_two()&&value<=4096){Ok(())}else{Err(MediaFileError::Unsupported("native decoder buffer alignment"))}}
pub(super) unsafe fn set_color(ty:&IMFMediaType,left:bool)->::windows::core::Result<()>{unsafe {
    ty.SetUINT32(&MF_MT_VIDEO_PRIMARIES,MFVideoPrimaries_BT709.0 as u32)?;
    ty.SetUINT32(&MF_MT_TRANSFER_FUNCTION,MFVideoTransFunc_709.0 as u32)?;
    ty.SetUINT32(&MF_MT_YUV_MATRIX,MFVideoTransferMatrix_BT709.0 as u32)?;
    ty.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE,MFNominalRange_16_235.0 as u32)?;
    ty.SetUINT32(&MF_MT_VIDEO_CHROMA_SITING,if left {MFVideoChromaSubsampling_MPEG2.0 as u32}else{MFVideoChromaSubsampling_MPEG1.0 as u32})
}}
fn optional_u32(ty:&IMFMediaType,key:&::windows::core::GUID)->Result<Option<u32>,MediaFileError>{match unsafe {ty.GetUINT32(key)} {Ok(v)=>Ok(Some(v)),Err(e) if e.code()==MF_E_ATTRIBUTENOTFOUND=>Ok(None),Err(e)=>Err(native("native decoder format attribute",e))}}
fn read(ty:&IMFMediaType,configuration:&Configuration)->Result<Format,MediaFileError>{
    if sdk("native decoder output major type",||unsafe {ty.GetGUID(&MF_MT_MAJOR_TYPE)})?!=MFMediaType_Video||sdk("native decoder output subtype",||unsafe {ty.GetGUID(&MF_MT_SUBTYPE)})?!=MFVideoFormat_NV12{return Err(MediaFileError::Unsupported("native decoder output must be system-memory NV12"));}
    let size=sdk("native decoder output dimensions",||unsafe {ty.GetUINT64(&MF_MT_FRAME_SIZE)})?;let width=(size>>32) as u32;let height=size as u32;
    if width==0||height==0||width%2!=0||height%2!=0||u64::from(width)*u64::from(height)>4_194_304{return Err(MediaFileError::Limit("native decoded coded dimensions"));}
    let crop=if (width,height)==(configuration.sps.width(),configuration.sps.height()){configuration.sps.crop_rect()}
        else if (width,height)==(configuration.width,configuration.height){(0,0,width,height)}else{return Err(MediaFileError::Unsupported("native decoded dimensions differ from admitted SPS"));};
    for key in [&MF_MT_MINIMUM_DISPLAY_APERTURE,&MF_MT_GEOMETRIC_APERTURE] {
        let count=match unsafe {ty.GetBlobSize(key)} {Ok(v)=>v as usize,Err(e) if e.code()==MF_E_ATTRIBUTENOTFOUND=>continue,Err(e)=>return Err(native("native decoder display aperture size",e))};
        if count!=size_of::<MFVideoArea>(){return Err(MediaFileError::Unsupported("native display aperture blob size"));}
        let mut bytes=vec![0;count];let mut actual=0;sdk("native decoder display aperture",||unsafe {ty.GetBlob(key,&mut bytes,Some(&mut actual))})?;
        if actual as usize!=count{return Err(MediaFileError::Unsupported("native aperture changed during inspection"));}
        let area=unsafe {ptr::read_unaligned(bytes.as_ptr().cast::<MFVideoArea>())};
        if area.OffsetX.fract!=0||area.OffsetY.fract!=0||area.OffsetX.value<0||area.OffsetY.value<0||area.Area.cx<=0||area.Area.cy<=0
            ||((area.OffsetX.value as u32,area.OffsetY.value as u32,area.Area.cx as u32,area.Area.cy as u32)!=crop && !(key==&MF_MT_GEOMETRIC_APERTURE&&(area.OffsetX.value as u32,area.OffsetY.value as u32,area.Area.cx as u32,area.Area.cy as u32)==(0,0,width,height))){return Err(MediaFileError::Unsupported("native aperture conflicts with SPS display crop"));}
    }
    for (key,expected) in [(&MF_MT_INTERLACE_MODE,MFVideoInterlace_Progressive.0 as u32),(&MF_MT_VIDEO_PRIMARIES,MFVideoPrimaries_BT709.0 as u32),(&MF_MT_TRANSFER_FUNCTION,MFVideoTransFunc_709.0 as u32),(&MF_MT_YUV_MATRIX,MFVideoTransferMatrix_BT709.0 as u32),(&MF_MT_VIDEO_NOMINAL_RANGE,MFNominalRange_16_235.0 as u32),(&MF_MT_VIDEO_CHROMA_SITING,if configuration.chroma_left {MFVideoChromaSubsampling_MPEG2.0 as u32}else{MFVideoChromaSubsampling_MPEG1.0 as u32})] {
        if optional_u32(ty,key)?!=Some(expected){return Err(MediaFileError::Unsupported("native output did not accept the exact progressive BT709 limited color contract"));}
    }
    if optional_u32(ty,&MF_MT_PAN_SCAN_ENABLED)?.is_some_and(|v|v!=0){return Err(MediaFileError::Unsupported("native pan-scan output"));}
    let stride=optional_u32(ty,&MF_MT_DEFAULT_STRIDE)?.ok_or(MediaFileError::Unsupported("native output has no explicit stride"))? as i32;
    if stride<=0||(stride as u32)<width||stride as usize*(height as usize+height as usize/2)>MAX_NATIVE_BUFFER{return Err(MediaFileError::Limit("native decoder output stride/capacity"));}
    let compact=sdk("native NV12 contiguous stride",||unsafe {MFGetStrideForBitmapInfoHeader(MFVideoFormat_NV12.data1,width)})?;
    if compact<=0||(compact as u32)<width{return Err(MediaFileError::Unsupported("native NV12 contiguous stride"));}
    Ok(Format {width,height,crop,stride:stride as usize,compact_stride:compact as usize})
}
pub(super) fn negotiate(transform:&IMFTransform,id:u32,configuration:&Configuration)->Result<(Format,MFT_OUTPUT_STREAM_INFO),MediaFileError>{
    for index in 0..32 {
        let ty=match unsafe {transform.GetOutputAvailableType(id,index)} {Ok(v)=>v,Err(e) if e.code()==MF_E_NO_MORE_TYPES=>break,Err(e)=>return Err(native("native decoder available output type",e))};
        if unsafe {ty.GetGUID(&MF_MT_SUBTYPE)}.ok()!=Some(MFVideoFormat_NV12){continue;}
        // These are negotiated requests, not inferred pixel metadata. Inspect
        // the accepted current type again before any sample can be published.
        let size=sdk("native candidate frame size",||unsafe {ty.GetUINT64(&MF_MT_FRAME_SIZE)})?;let width=(size>>32) as u32;
        let stride=sdk("native NV12 requested stride",||unsafe {MFGetStrideForBitmapInfoHeader(MFVideoFormat_NV12.data1,width)})?;
        sdk("native decoder requested color",||unsafe {set_color(&ty,configuration.chroma_left)?;ty.SetUINT32(&MF_MT_INTERLACE_MODE,MFVideoInterlace_Progressive.0 as u32)?;
            if optional_u32_native(&ty,&MF_MT_DEFAULT_STRIDE)?.is_none(){ty.SetUINT32(&MF_MT_DEFAULT_STRIDE,stride as u32)?;}Ok(())})?;
        match read(&ty,configuration){Ok(_)=>{},Err(MediaFileError::Unsupported(_)|MediaFileError::Limit(_))=>continue,Err(error)=>return Err(error)}
        match unsafe {transform.SetOutputType(id,&ty,0)} {Ok(())=>{},Err(e) if e.code()==MF_E_INVALIDMEDIATYPE=>continue,Err(e)=>return Err(native("native decoder accept NV12 output",e))}
        let current=sdk("native decoder current output type",||unsafe {transform.GetOutputCurrentType(id)})?;let format=read(&current,configuration)?;
        let info=sdk("native decoder output stream info",||unsafe {transform.GetOutputStreamInfo(id)})?;alignment(info.cbAlignment)?;
        if info.cbSize as usize>MAX_NATIVE_BUFFER||((info.dwFlags&(MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32|MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32))==0&&info.cbSize==0){return Err(MediaFileError::Limit("native decoder requested output allocation"));}
        return Ok((format,info));
    }
    Err(MediaFileError::Unsupported("native decoder has no admitted progressive system-memory NV12 output"))
}
fn optional_u32_native(ty:&IMFMediaType,key:&::windows::core::GUID)->::windows::core::Result<Option<u32>>{match unsafe {ty.GetUINT32(key)}{Ok(v)=>Ok(Some(v)),Err(e) if e.code()==MF_E_ATTRIBUTENOTFOUND=>Ok(None),Err(e)=>Err(e)}}
pub(super) fn empty_sample(bytes:u32,align:u32)->Result<IMFSample,MediaFileError>{
    alignment(align)?;if bytes==0||bytes as usize>MAX_NATIVE_BUFFER{return Err(MediaFileError::Limit("native sample allocation"));}
    let sample=sdk("native decoder sample",||unsafe {MFCreateSample()})?;
    let buffer=sdk("native decoder sample buffer",||unsafe {if align>1{MFCreateAlignedMemoryBuffer(bytes,align-1)}else{MFCreateMemoryBuffer(bytes)}})?;
    sdk("native decoder attach sample buffer",||unsafe {sample.AddBuffer(&buffer)})?;Ok(sample)
}
pub(super) fn input_sample(bytes:&[u8],align:u32,pts:i64,dts:i64,duration:i64,sync:bool,first:bool)->Result<IMFSample,MediaFileError>{
    let sample=empty_sample(bytes.len() as u32,align)?;let buffer=sdk("native AVC input buffer",||unsafe {sample.GetBufferByIndex(0)})?;
    let (mut data,mut capacity)=(ptr::null_mut(),0);sdk("native AVC input lock",||unsafe {buffer.Lock(&mut data,Some(&mut capacity),None)})?;
    let guard=Locked(buffer.clone());
    if data.is_null()||(capacity as usize)<bytes.len()||capacity as usize>MAX_NATIVE_BUFFER{return Err(MediaFileError::Limit("native AVC input buffer capacity"));}
    unsafe {ptr::copy_nonoverlapping(bytes.as_ptr(),data,bytes.len())};drop(guard);
    sdk("native AVC input metadata",||unsafe {buffer.SetCurrentLength(bytes.len() as u32)?;sample.SetSampleTime(pts)?;sample.SetSampleDuration(duration)?;
        sample.SetUINT64(&MFSampleExtension_DecodeTimestamp,dts as u64)?;sample.SetUINT32(&MFSampleExtension_CleanPoint,u32::from(sync))?;
        sample.SetUINT32(&MFSampleExtension_Discontinuity,u32::from(first))})?;Ok(sample)
}
struct Locked(IMFMediaBuffer);impl Drop for Locked {fn drop(&mut self){unsafe {let _=self.0.Unlock();}}}
pub(super) fn picture(sample:&IMFSample,format:&Format,configuration:&Configuration,pts:i64)->Result<NativePicture,MediaFileError>{
    if sdk("native decoded buffer count",||unsafe {sample.GetBufferCount()})?!=1{return Err(MediaFileError::Unsupported("native decoder output requires one buffer"));}
    let buffer=sdk("native decoded buffer",||unsafe {sample.GetBufferByIndex(0)})?;
    if let Ok(two)=buffer.cast::<IMF2DBuffer>() {
        let count=sdk("native2D contiguous byte count",||unsafe {two.GetContiguousLength()})? as usize;
        let expected=format.compact_stride*(format.height as usize+format.height as usize/2);
        if count!=expected||count>MAX_NATIVE_BUFFER{return Err(MediaFileError::Unsupported("native2D NV12 contiguous layout differs from admitted format"));}
        let mut bytes=vec![0;count];sdk("native2D contiguous copy",||unsafe {two.ContiguousCopyTo(&mut bytes)})?;
        return copy_planes(&bytes,format.compact_stride,format,configuration,pts);
    }
    let (mut data,mut capacity,mut length)=(ptr::null_mut(),0,0);sdk("native NV12 output lock",||unsafe {buffer.Lock(&mut data,Some(&mut capacity),Some(&mut length))})?;
    let guard=Locked(buffer);
    let expected=format.stride*(format.height as usize+format.height as usize/2);
    if data.is_null()||length>capacity||(length as usize)<expected||capacity as usize>MAX_NATIVE_BUFFER{return Err(MediaFileError::Limit("native NV12 output buffer bounds"));}
    let bytes=unsafe {std::slice::from_raw_parts(data,length as usize)};
    let result=copy_planes(bytes,format.stride,format,configuration,pts);drop(guard);result
}
fn copy_planes(bytes:&[u8],stride:usize,format:&Format,configuration:&Configuration,pts:i64)->Result<NativePicture,MediaFileError>{
    let (x,y,w,h)=format.crop;let area=w as usize*h as usize;
    if area>4_194_304||bytes.len()>MAX_NATIVE_BUFFER||bytes.len()<stride*(format.height as usize+format.height as usize/2)
        ||x.checked_add(w).is_none_or(|v|v>format.width)||y.checked_add(h).is_none_or(|v|v>format.height){return Err(MediaFileError::Limit("native display crop/raster copy"));}
    let mut out=NativePicture::new(w,h,pts,configuration.chroma_left)?;
    for row in 0..h as usize {let start=(row+y as usize)*stride+x as usize;out.y[row*w as usize..(row+1)*w as usize].copy_from_slice(&bytes[start..start+w as usize]);}
    for row in 0..h as usize/2 {let start=stride*format.height as usize+(row+y as usize/2)*stride+x as usize;for col in 0..w as usize/2 {out.u[row*w as usize/2+col]=bytes[start+col*2];out.v[row*w as usize/2+col]=bytes[start+col*2+1];}}
    Ok(out)
}
