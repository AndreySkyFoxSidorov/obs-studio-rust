//! Direct installed SDK sender. The private async frame is owned through the
//! next documented synchronization event, including Stop's null flush/destroy.
use super::*;
use super::super::{abi,runtime_windows::{self,Api,SendApi}};
use skylive_media::RgbaFrame;
use std::{ffi::CString,ptr};
struct Held {_image:RgbaFrame,frame:Box<abi::Video>}
pub(super) struct Sender {api:Arc<Api>,calls:SendApi,handle:abi::Instance,_name:CString,_groups:CString,held:Option<Held>,pub name:String,pub url:Option<String>}
impl Sender {
    pub fn new(config:&Config,stop:&AtomicBool)->Result<Self,NdiError>{
        if stop.load(Ordering::Acquire){return Err(NdiError::Closed);}
        let api=runtime_windows::load()?;let calls=api.sender()?;
        let name=CString::new(config.settings.name(config.route)).map_err(|_|NdiError::Invalid("NDI output name contains NUL"))?;let groups=CString::new(config.settings.groups()).map_err(|_|NdiError::Invalid("NDI output groups contain NUL"))?;
        let descriptor=abi::SendCreate {name:name.as_ptr(),groups:if config.settings.groups().is_empty(){ptr::null()}else{groups.as_ptr()},clock_video:false,clock_audio:false};
        if stop.load(Ordering::Acquire){return Err(NdiError::Closed);}
        let handle=unsafe{(calls.create)(&descriptor)};if handle.is_null(){return Err(NdiError::Runtime("NDI send_create failed".into()));}
        let mut sender=Self {api,calls,handle,_name:name,_groups:groups,held:None,name:String::new(),url:None};
        let source=unsafe{(sender.calls.source)(sender.handle)};if source.is_null(){return Err(NdiError::Runtime("NDI sender returned no advertised source".into()));}
        let source=unsafe{&*source};sender.name=unsafe{runtime_windows::text(source.name,false)}?.ok_or(NdiError::Invalid("NDI advertised source name is empty"))?;sender.url=unsafe{runtime_windows::text(source.url,true)}?;Ok(sender)
    }
    pub fn version(&self)->&str{&self.api.version}
    pub fn connections(&self)->Result<u32,NdiError>{u32::try_from(unsafe{(self.calls.connections)(self.handle,0)}).map_err(|_|NdiError::Runtime("NDI sender connection query failed".into()))}
    pub fn video(&mut self,image:RgbaFrame,pts:i64,config:&Config)->Result<(),NdiError>{
        if pts<0||image.width()!=config.video.width()||image.height()!=config.video.height()||image.byte_len()>FRAME_BYTES{return Err(NdiError::Invalid("NDI output video format/timestamp changed"));}
        let frame=Box::new(abi::Video {width:image.width() as i32,height:image.height() as i32,fourcc:u32::from_le_bytes(*b"RGBA"),fps_num:config.video.fps_num() as i32,fps_den:config.video.fps_den() as i32,aspect:0.0,frame_format:1,timecode:pts,data:image.pixels().as_ptr() as *mut u8,stride:(image.width()*4) as i32,metadata:ptr::null(),timestamp:0});
        let held=Held {_image:image,frame};
        // SAFETY: two private, bounded immutable allocations remain live until
        // this SDK synchronization call returns and until the subsequent call.
        unsafe{(self.calls.video)(self.handle,&*held.frame);}self.held=Some(held);Ok(())
    }
    pub fn audio(&mut self,samples:&[f32],frames:usize,pts:i64,format:AudioSettings,scratch:&mut Vec<f32>)->Result<(),NdiError>{
        let channels=usize::from(format.speakers().channels());if frames==0||frames>4096||channels==0||channels>8||samples.len()!=frames*channels||pts<0||samples.iter().any(|v|!v.is_finite()){return Err(NdiError::Invalid("NDI Program PCM shape/timestamp"));}
        scratch.resize(frames*channels,0.0);for c in 0..channels{for f in 0..frames{scratch[c*frames+f]=samples[f*channels+c];}}
        let frame=abi::Audio {rate:format.sample_rate() as i32,channels:channels as i32,samples:frames as i32,timecode:pts,fourcc:u32::from_le_bytes(*b"FLTp"),data:scratch.as_mut_ptr().cast(),channel_stride:(frames*4) as i32,metadata:ptr::null(),timestamp:0};
        // Audio v3 is synchronous; the planar scratch is not re-used until return.
        unsafe{(self.calls.audio)(self.handle,&frame);}Ok(())
    }
}
impl Drop for Sender {fn drop(&mut self){unsafe{(self.calls.video)(self.handle,ptr::null());(self.calls.free)(self.handle);}self.held.take();}}
