//! Native NDI handles and borrowed SDK buffers never leave this worker module.
use super::{abi::*,runtime_windows::{Api,text},NdiError,VideoMetadata};
use skylive_core::{NdiBandwidth,NdiConnection,NdiSender};
use skylive_media::{RgbaFrame,PackedFrameLayout,PackedFrameLimits,PackedPixelFormat,RowOrder,AlphaMode,ingest_packed_frame};
use std::{ffi::CString,sync::Arc};
pub(super) struct Finder {api:Arc<Api>,handle:Instance}
impl Finder {
    pub(super) fn new(api:Arc<Api>,local:bool)->Result<Self,NdiError>{
        let settings=FindCreate {local,groups:std::ptr::null(),extra_ips:std::ptr::null()};
        let handle=unsafe{(api.find_new)(&settings)};if handle.is_null(){return Err(NdiError::Runtime("NDI finder creation failed".into()));}Ok(Self {api,handle})
    }
    pub(super) fn sources(&mut self)->Result<Vec<NdiSender>,NdiError>{
        unsafe{(self.api.find_wait)(self.handle,0);}
        let mut count=0;let sources=unsafe{(self.api.find_sources)(self.handle,&mut count)};
        if count>128{return Err(NdiError::Limit("NDI discovery exceeds128 senders"));}
        if count!=0&&sources.is_null(){return Err(NdiError::Invalid("NDI finder returned a null source list"));}
        let mut output=Vec::with_capacity(count as usize);
        for i in 0..count as usize {let source=unsafe{&*sources.add(i)};
            let sender=NdiSender {name:unsafe{text(source.name,false)}?.unwrap(),url:unsafe{text(source.url,true)}?};sender.validate().map_err(|e|NdiError::Runtime(e.to_string()))?;
            if !output.contains(&sender){output.push(sender);}
        }
        output.sort_by(|a,b|a.name.cmp(&b.name).then_with(||a.url.cmp(&b.url)));Ok(output)
    }
}
impl Drop for Finder {fn drop(&mut self){unsafe{(self.api.find_free)(self.handle);}}}
pub(super) enum Packet {None,Video {metadata:VideoMetadata,image:Option<RgbaFrame>},Audio {rate:u32,channels:u16,timecode:i64,timestamp:Option<i64>,samples:Box<[f32]>},Status}
pub(super) struct Receiver {api:Arc<Api>,handle:Instance,_name:CString,_url:Option<CString>,_receiver_name:CString,audio:bool}
impl Drop for Receiver {fn drop(&mut self){unsafe{(self.api.recv_free)(self.handle);}}}
impl Receiver {
    pub(super) fn new(api:Arc<Api>,connection:&NdiConnection)->Result<Self,NdiError>{
        connection.validate().map_err(|e|NdiError::Runtime(e.to_string()))?;
        let name=CString::new(connection.sender.name.as_str()).map_err(|_|NdiError::Invalid("NDI source name contains NUL"))?;
        let url=connection.sender.url.as_ref().map(|s|CString::new(s.as_str())).transpose().map_err(|_|NdiError::Invalid("NDI source URL contains NUL"))?;
        let receiver_name=CString::new("Sky Live Rust NDI receive").unwrap();
        let settings=RecvCreate {source:Source {name:name.as_ptr(),url:url.as_ref().map_or(std::ptr::null(),|s|s.as_ptr())},color:2,bandwidth:match connection.bandwidth {NdiBandwidth::Highest=>100,NdiBandwidth::Lowest=>0},fields:false,name:receiver_name.as_ptr()};
        let handle=unsafe{(api.recv_new)(&settings)};if handle.is_null(){return Err(NdiError::Runtime("NDI receiver creation failed".into()));}
        Ok(Self {api,handle,_name:name,_url:url,_receiver_name:receiver_name,audio:connection.audio})
    }
    pub(super) fn connected(&self)->bool{(unsafe{(self.api.connections)(self.handle)})>0}
    pub(super) fn capture(&mut self,max_output:usize,probe:bool)->Result<Packet,NdiError>{
        let mut video:Video=unsafe{std::mem::zeroed()};let mut audio:Audio=unsafe{std::mem::zeroed()};let mut metadata:Metadata=unsafe{std::mem::zeroed()};
        let kind=unsafe{(self.api.capture)(self.handle,&mut video,if self.audio&&!probe {&mut audio}else{std::ptr::null_mut()},&mut metadata,0)};
        match kind {
            0=>Ok(Packet::None),
            1=>{
                struct Free<'a>(&'a Api,Instance,&'a Video);impl Drop for Free<'_>{fn drop(&mut self){unsafe{(self.0.free_video)(self.1,self.2);}}}
                let _guard=Free(&self.api,self.handle,&video);
                let information=video_metadata(&video)?;
                let row=(information.width as usize).checked_mul(4).ok_or(NdiError::Limit("NDI pixel row overflow"))?;
                let stride=if video.stride==0 {row}else{usize::try_from(video.stride).map_err(|_|NdiError::Invalid("NDI negative pixel stride is unsupported"))?};
                let bytes=stride.checked_mul(information.height as usize).ok_or(NdiError::Limit("NDI pixel length overflow"))?;
                if stride<row||bytes>128*1024*1024||video.data.is_null(){return Err(NdiError::Limit("NDI native RGB frame layout/128MiB"));}
                let (format,alpha)=match video.fourcc {
                    v if v==u32::from_le_bytes(*b"RGBA")=>(PackedPixelFormat::Rgba8,AlphaMode::Straight),
                    v if v==u32::from_le_bytes(*b"RGBX")=>(PackedPixelFormat::Rgba8,AlphaMode::Opaque),
                    v if v==u32::from_le_bytes(*b"BGRA")=>(PackedPixelFormat::Bgra8,AlphaMode::Straight),
                    v if v==u32::from_le_bytes(*b"BGRX")=>(PackedPixelFormat::Bgra8,AlphaMode::Opaque),
                    _=>return Err(NdiError::Invalid("NDI SDK did not deliver requested RGBX/RGBA/BGRX/BGRA")),
                };
                if probe{return Ok(Packet::Video {metadata:information,image:None});}
                let image=ingest_packed_frame(unsafe{std::slice::from_raw_parts(video.data,bytes)},PackedFrameLayout {width:information.width,height:information.height,stride_bytes:stride,format,row_order:RowOrder::TopDown,alpha},PackedFrameLimits {max_input_bytes:128*1024*1024,max_output_bytes:max_output.min(128*1024*1024),max_pixel_visits:2*33_554_432}).map_err(|_|NdiError::Limit("NDI packed-frame conversion budget/layout"))?;
                Ok(Packet::Video {metadata:information,image:Some(image)})
            },
            2=>{
                struct Free<'a>(&'a Api,Instance,&'a Audio);impl Drop for Free<'_>{fn drop(&mut self){unsafe{(self.0.free_audio)(self.1,self.2);}}}
                let _guard=Free(&self.api,self.handle,&audio);
                if audio.fourcc!=u32::from_le_bytes(*b"FLTp")||!(8000..=192000).contains(&audio.rate)||!(1..=8).contains(&audio.channels)||!(1..=4096).contains(&audio.samples)||audio.data.is_null(){return Err(NdiError::Invalid("NDI audio requires actual8k..192kHz/1..8ch/1..4096 sample FLTP"));}
                let (channels,frames)=(audio.channels as usize,audio.samples as usize);
                let stride=usize::try_from(audio.channel_stride).map_err(|_|NdiError::Invalid("NDI audio channel stride is negative"))?;
                let native=stride.checked_mul(channels-1).and_then(|n|n.checked_add(frames*4)).ok_or(NdiError::Limit("NDI planar audio size"))?;
                if (channels>1&&stride<frames*4)||native>128*1024{return Err(NdiError::Limit("NDI planar audio128KiB/stride"));}
                let mut samples=Vec::new();samples.try_reserve_exact(frames*channels).map_err(|_|NdiError::Limit("NDI PCM allocation"))?;samples.resize(frames*channels,0.0);
                for channel in 0..channels {for frame in 0..frames {let value=unsafe{audio.data.add(channel*stride+frame*4).cast::<f32>().read_unaligned()};if !value.is_finite(){return Err(NdiError::Invalid("non-finite NDI PCM"));}samples[frame*channels+channel]=value;}}
                Ok(Packet::Audio {rate:audio.rate as u32,channels:audio.channels as u16,timecode:audio.timecode,timestamp:(audio.timestamp!=i64::MAX).then_some(audio.timestamp),samples:samples.into_boxed_slice()})
            },
            3=>{unsafe{(self.api.free_metadata)(self.handle,&metadata);}Ok(Packet::Status)},
            4=>Err(NdiError::Runtime("NDI receiver reported a connection error".into())),
            100|101=>Ok(Packet::Status),
            _=>Err(NdiError::Invalid("NDI capture returned an unknown frame type")),
        }
    }
}
fn video_metadata(video:&Video)->Result<VideoMetadata,NdiError>{
    let (width,height)=(u32::try_from(video.width).map_err(|_|NdiError::Invalid("negative NDI width"))?,u32::try_from(video.height).map_err(|_|NdiError::Invalid("negative NDI height"))?);
    if width==0||height==0||width>8192||height>8192||u64::from(width)*u64::from(height)>33_554_432||video.frame_format!=1||video.fps_num<=0||video.fps_den<=0{return Err(NdiError::Invalid("NDI progressive frame dimensions/rate exceed the admitted profile"));}
    if !video.aspect.is_finite()||video.aspect<0.0||(video.aspect>0.0&&(f64::from(video.aspect)-f64::from(width)/f64::from(height)).abs()>0.005){return Err(NdiError::Invalid("NDI non-square pixel/aspect conversion is unsupported"));}
    Ok(VideoMetadata {width,height,fps_num:video.fps_num as u32,fps_den:video.fps_den as u32,timecode_100ns:video.timecode,timestamp_100ns:(video.timestamp!=i64::MAX).then_some(video.timestamp)})
}
