//! Shared-mode WASAPI render. This object is created, used, stopped and released
//! on one dedicated MTA owner. No COM pointer crosses the PCM queue.
use super::{MonitorConfig,MonitorFrame};
use crate::audio_capture::{AudioCaptureLimits,NativeAudioFormat,PcmEncoding};
use skylive_core::AudioMonitoringDevice;
use skylive_media::{AudioConverter,AudioConvertLimits,AudioFormat};
use std::{ffi::c_void,ptr,time::{Duration,Instant}};
use windows::{core::{Interface,PCWSTR,PROPVARIANT},Win32::{
    Media::Audio::{IAudioClient,IAudioRenderClient,IMMDevice,IMMDeviceEnumerator,IMMEndpoint,MMDeviceEnumerator,WAVEFORMATEX,WAVEFORMATEXTENSIBLE,eRender,eConsole,DEVICE_STATE_ACTIVE,AUDCLNT_SHAREMODE_SHARED,AUDCLNT_E_DEVICE_INVALIDATED},
    Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
    System::Com::{CoCreateInstance,CoInitializeEx,CoUninitialize,CoTaskMemFree,COINIT_MULTITHREADED,CLSCTX_ALL,STGM_READ},
}};
#[path="native_format.rs"] mod format;
struct Apartment;
impl Drop for Apartment {fn drop(&mut self){unsafe{CoUninitialize();}}}
struct Allocation(*mut WAVEFORMATEX);
impl Drop for Allocation {fn drop(&mut self){unsafe{CoTaskMemFree(Some(self.0.cast::<c_void>()));}}}
fn error(operation:&str,e:windows::core::Error)->String {format!("{operation}: HRESULT {}: {e}",e.code().0)}
pub(super) struct Render {
    client:IAudioClient,render:IAudioRenderClient,converter:AudioConverter,
    config:MonitorConfig,mix:format::MixFormat,buffer_frames:usize,started:bool,
    pending:Vec<f32>,pending_frames:usize,offset:usize,expected:Option<u64>,padding_ns:u64,
    id:String,name:String,default_watch:Option<(IMMDeviceEnumerator,Instant)>,_apartment:Apartment,
}
impl Render {
    pub(super) fn open(config:&MonitorConfig)->Result<Self,String>{
        unsafe{CoInitializeEx(None,COINIT_MULTITHREADED)}.ok().map_err(|e|error("monitor CoInitializeEx",e))?;
        let apartment=Apartment;
        let enumerator:IMMDeviceEnumerator=unsafe{CoCreateInstance(&MMDeviceEnumerator,None,CLSCTX_ALL)}.map_err(|e|error("monitor MMDeviceEnumerator",e))?;
        let device=match &config.device {
            AudioMonitoringDevice::Default=>unsafe{enumerator.GetDefaultAudioEndpoint(eRender,eConsole)}.map_err(|e|error("monitor default output",e))?,
            AudioMonitoringDevice::WindowsEndpoint {id}=>{config.device.validate().map_err(|e|e.to_string())?;let wide:Vec<u16>=id.encode_utf16().chain(Some(0)).collect();unsafe{enumerator.GetDevice(PCWSTR(wide.as_ptr()))}.map_err(|e|error("monitor selected endpoint",e))?},
        };
        let endpoint:IMMEndpoint=device.cast().map_err(|e|error("monitor IMMEndpoint",e))?;
        if unsafe{endpoint.GetDataFlow()}.map_err(|e|error("monitor endpoint flow",e))?!=eRender{return Err("monitoring requires a playback output endpoint".into());}
        if unsafe{device.GetState()}.map_err(|e|error("monitor endpoint state",e))?!=DEVICE_STATE_ACTIVE{return Err("monitor output endpoint is not active".into());}
        let id=device_id(&device)?;let name=device_name(&device)?;
        let client:IAudioClient=unsafe{device.Activate(CLSCTX_ALL,None)}.map_err(|e|error("monitor activate audio client",e))?;
        let allocation=Allocation(unsafe{client.GetMixFormat()}.map_err(|e|error("monitor GetMixFormat",e))?);
        if allocation.0.is_null(){return Err("monitor GetMixFormat returned null".into());}
        let header=unsafe{ptr::read_unaligned(allocation.0)};
        let extension=if header.wFormatTag==0xfffe {
            if header.cbSize<(std::mem::size_of::<WAVEFORMATEXTENSIBLE>()-std::mem::size_of::<WAVEFORMATEX>())as u16{return Err("monitor native format extension is truncated".into());}
            Some(unsafe{ptr::read_unaligned(allocation.0.cast::<WAVEFORMATEXTENSIBLE>())})
        }else{None};
        let mix=format::parse_format(header,extension,AudioCaptureLimits::default()).map_err(|e|e.to_string())?;
        unsafe{client.Initialize(AUDCLNT_SHAREMODE_SHARED,0,1_000_000,0,allocation.0,None)}.map_err(|e|error("monitor Initialize shared",e))?;
        let buffer_frames=unsafe{client.GetBufferSize()}.map_err(|e|error("monitor GetBufferSize",e))?as usize;
        if buffer_frames==0||buffer_frames.checked_mul(mix.block_align).is_none_or(|n|n>4*1024*1024){return Err("monitor native output buffer exceeds 4 MiB".into());}
        let render:IAudioRenderClient=unsafe{client.GetService()}.map_err(|e|error("monitor render service",e))?;
        let input=AudioFormat::new(config.settings.sample_rate(),config.settings.channels(),config.settings.channel_mask()).map_err(|e|e.to_string())?;
        let output=AudioFormat::new(mix.format.sample_rate,mix.format.channels,mix.format.channel_mask).map_err(|e|e.to_string())?;
        let converter=AudioConverter::new(input,output,AudioConvertLimits::default()).map_err(|e|format!("monitor converter: {e}"))?;
        let pending=vec![0.0;1024*usize::from(mix.format.channels)];
        unsafe{client.Start()}.map_err(|e|error("monitor Start",e))?;
        Ok(Self {client,render,converter,config:config.clone(),mix,buffer_frames,started:true,pending,pending_frames:0,offset:0,expected:None,padding_ns:0,id,name,default_watch:matches!(config.device,AudioMonitoringDevice::Default).then(||(enumerator,Instant::now()+Duration::from_secs(1))),_apartment:apartment})
    }
    pub(super) fn device_id(&self)->&str{&self.id}
    pub(super) fn device_name(&self)->&str{&self.name}
    pub(super) fn padding_ns(&self)->u64{self.padding_ns}
    pub(super) fn native_format(&self)->NativeAudioFormat{self.mix.format}
    pub(super) fn push(&mut self,frame:MonitorFrame)->Result<(),String>{
        let channels=usize::from(self.config.settings.channels());
        if frame.settings!=self.config.settings||frame.samples.is_empty()||frame.samples.len()%channels!=0||frame.samples.len()>1024*channels{return Err("monitor frame format or size changed".into());}
        if self.expected.is_some_and(|expected|frame.start_frame!=expected){self.converter.reset();self.pending_frames=0;self.offset=0;}
        self.expected=frame.start_frame.checked_add((frame.samples.len()/channels)as u64);
        if self.expected.is_none(){return Err("monitor sample cursor overflow".into());}
        self.converter.push(&frame.samples).map_err(|e|format!("monitor conversion/backpressure: {e}"))
    }
    pub(super) fn pump(&mut self)->Result<usize,String>{
        if let Some((enumerator,check))=&mut self.default_watch {if Instant::now()>=*check {
            let current=unsafe{enumerator.GetDefaultAudioEndpoint(eRender,eConsole)}.map_err(|e|error("monitor default-device watch",e))?;
            if device_id(&current)?!=self.id{return Err("Default monitoring device changed. Select Monitor Off and then the desired monitor mode to reopen the current default.".into());}
            *check=Instant::now()+Duration::from_secs(1);
        }}
        let padding=unsafe{self.client.GetCurrentPadding()}.map_err(|e|error("monitor output padding",e))?as usize;
        self.padding_ns=(padding as u64).saturating_mul(1_000_000_000)/u64::from(self.mix.format.sample_rate);
        if padding>self.buffer_frames{return Err("monitor output padding exceeds native buffer".into());}
        let available=self.buffer_frames-padding;
        if available==0{return Ok(0);}
        if self.offset==self.pending_frames {
            self.pending_frames=self.converter.pull(&mut self.pending).map_err(|e|format!("monitor converter pull: {e}"))?;self.offset=0;
        }
        let frames=available.min(self.pending_frames-self.offset).min(1024);
        if frames==0{return Ok(0);}
        let pointer=unsafe{self.render.GetBuffer(frames as u32)}.map_err(|e|error("monitor GetBuffer",e))?;
        if pointer.is_null(){let _=unsafe{self.render.ReleaseBuffer(0,0)};return Err("monitor render buffer is null".into());}
        let bytes=unsafe{std::slice::from_raw_parts_mut(pointer,frames*self.mix.block_align)};
        let channels=usize::from(self.mix.format.channels);let width=self.mix.block_align/channels;
        for (sample,destination) in self.pending[self.offset*channels..(self.offset+frames)*channels].iter().zip(bytes.chunks_exact_mut(width)){encode(*sample,self.mix.encoding,destination);}
        unsafe{self.render.ReleaseBuffer(frames as u32,0)}.map_err(|e|error("monitor ReleaseBuffer",e))?;
        self.offset+=frames;Ok(frames)
    }
    pub(super) fn close(&mut self)->Result<(),String>{
        if !self.started{return Ok(());}
        match unsafe{self.client.Stop()} {
            Ok(())=>{self.started=false;Ok(())},
            Err(e) if e.code()==AUDCLNT_E_DEVICE_INVALIDATED=>{self.started=false;Ok(())},
            Err(e)=>Err(error("monitor Stop pending",e)),
        }
    }
}
fn encode(sample:f32,encoding:PcmEncoding,destination:&mut [u8]){
    let sample=f64::from(sample).clamp(-1.0,1.0);
    if encoding==PcmEncoding::Float32 {destination.copy_from_slice(&(sample as f32).to_le_bytes());return;}
    if encoding==PcmEncoding::Unsigned8 {destination[0]=((sample+1.0)*127.5).round().clamp(0.0,255.0)as u8;return;}
    let (bits,valid)=match encoding {PcmEncoding::Signed16=>(16,16),PcmEncoding::SignedIn16 {valid_bits}=>(16,valid_bits),PcmEncoding::Signed24=>(24,24),PcmEncoding::Signed32=>(32,32),PcmEncoding::SignedIn24 {valid_bits}=>(24,valid_bits),PcmEncoding::SignedIn32 {valid_bits}=>(32,valid_bits),_=>unreachable!()};
    let scale=(1u64<<(valid-1))as f64;let value=(sample*scale).round().clamp(-scale,scale-1.0)as i32;
    let bytes=(value<<(bits-valid)).to_le_bytes();destination.copy_from_slice(&bytes[..destination.len()]);
}
fn text(pointer:*const u16)->Result<String,String>{
    if pointer.is_null(){return Err("null monitor endpoint metadata".into());}
    for count in 0..=1024 {if unsafe{pointer.add(count).read()}==0 {
        let value=String::from_utf16(unsafe{std::slice::from_raw_parts(pointer,count)}).map_err(|e|e.to_string())?;
        if value.trim().is_empty()||value.len()>1024||value.chars().any(char::is_control){return Err("invalid monitor endpoint metadata".into());}return Ok(value);
    }}Err("oversized monitor endpoint metadata".into())
}
fn device_id(device:&IMMDevice)->Result<String,String>{let pointer=unsafe{device.GetId()}.map_err(|e|error("monitor endpoint ID",e))?;let value=text(pointer.0);unsafe{CoTaskMemFree(Some(pointer.0.cast::<c_void>()));}value}
fn device_name(device:&IMMDevice)->Result<String,String>{
    let store=unsafe{device.OpenPropertyStore(STGM_READ)}.map_err(|e|error("monitor endpoint properties",e))?;
    let value:PROPVARIANT=unsafe{store.GetValue(&PKEY_Device_FriendlyName)}.map_err(|e|error("monitor endpoint friendly name",e))?;
    let raw=unsafe{value.as_raw().Anonymous.Anonymous};if raw.vt!=31{return Err("monitor device friendly name is not a string".into());}text(unsafe{raw.Anonymous.pwszVal})
}
