//! Windows x64 / NVENC API12.1 / D3D11 / NV12 / H264 Baseline, CBR, no B.
//! All driver calls, buffers, COM interfaces and library lifetime stay on one
//! packet/recording/explicit-probe worker. No work occurs during capabilities().
mod ffi;
use ffi::*;
use super::{Adapter,AccessUnit,CodecConfiguration,Config,Error,Nv12Input,MAX_AU_BYTES,MAX_HEADER_BYTES,MAX_PENDING,NvencAdapter,NvencCatalog};
use std::{collections::VecDeque,ffi::c_void,mem::{self,ManuallyDrop},ptr,sync::atomic::{AtomicUsize,Ordering}};
use windows::{core::{Interface,PCSTR,w},Win32::{Foundation::{HMODULE,FreeLibrary},Graphics::{Direct3D::D3D_DRIVER_TYPE_UNKNOWN,Direct3D11::*,Dxgi::*},System::{LibraryLoader::*,Com::{CoInitializeEx,CoUninitialize,COINIT_MULTITHREADED}}}};
const MAX_ADAPTERS:u32=16;
const MAX_CAP_ITEMS:u32=64;
const MAX_PITCH:usize=4096;
const MAX_NATIVE_BYTES:usize=256*1024*1024;
static OWNERS:AtomicUsize=AtomicUsize::new(0);
static BYTES:AtomicUsize=AtomicUsize::new(0);
struct Lease {bytes:usize}
impl Lease {
    fn new(bytes:usize)->Result<Self,Error>{
        if bytes>MAX_NATIVE_BYTES{return Err(Error::Limit("NVENC native working bytes"));}
        OWNERS.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|(n<3).then_some(n+1)).map_err(|_|Error::Limit("NVENC active/retiring owners (three)"))?;
        if BYTES.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|n.checked_add(bytes).filter(|v|*v<=MAX_NATIVE_BYTES)).is_err(){OWNERS.fetch_sub(1,Ordering::AcqRel);return Err(Error::Limit("NVENC aggregate native working bytes"));}
        Ok(Self {bytes})
    }
}
impl Drop for Lease {fn drop(&mut self){BYTES.fetch_sub(self.bytes,Ordering::AcqRel);OWNERS.fetch_sub(1,Ordering::AcqRel);}}
struct Api {list:NV_ENCODE_API_FUNCTION_LIST,module:HMODULE}
impl Drop for Api {fn drop(&mut self){unsafe{let _=FreeLibrary(self.module);}}}
fn os(operation:&'static str,e:windows::core::Error)->Error{Error::Native {operation,hresult:e.code().0,detail:e.to_string()}}
fn required<T>(value:Option<T>)->Result<T,Error>{value.ok_or(Error::Unavailable("NVENC driver function table is incomplete"))}
impl Api {
    fn load()->Result<Self,Error>{
        // SAFETY: fixed system driver name; no search in cwd, PATH or project files.
        let module=unsafe{LoadLibraryExW(w!("nvEncodeAPI64.dll"),None,LOAD_LIBRARY_SEARCH_SYSTEM32)}.map_err(|e|os("load NVENC system driver",e))?;
        let result=(||{
            // SAFETY: exported SDK entry point signatures from API12.1.
            let max:MaxSupportedVersionFn=unsafe{mem::transmute(GetProcAddress(module,PCSTR(c"NvEncodeAPIGetMaxSupportedVersion".as_ptr().cast())).ok_or(Error::Unavailable("NVENC version export missing"))?)};
            let create:CreateInstanceFn=unsafe{mem::transmute(GetProcAddress(module,PCSTR(c"NvEncodeAPICreateInstance".as_ptr().cast())).ok_or(Error::Unavailable("NVENC create-instance export missing"))?)};
            let mut version=0;let status=unsafe{max(&mut version)};
            if status!=0{return Err(Error::Platform {operation:"NVENC maximum API version",code:status});}
            if version<((12<<4)|1){return Err(Error::Unavailable("NVIDIA driver does not support NVENC API12.1"));}
            // SAFETY: the SDK table is repr(C), all unused entries/reserved fields zero.
            let mut list:NV_ENCODE_API_FUNCTION_LIST=unsafe{mem::zeroed()};list.version=NV_ENCODE_API_FUNCTION_LIST_VER;
            let status=unsafe{create(&mut list)};if status!=0{return Err(Error::Platform {operation:"NVENC create API12.1 table",code:status});}
            // Check teardown BEFORE opening native resources.
            required(list.nvEncDestroyEncoder)?;required(list.nvEncDestroyInputBuffer)?;required(list.nvEncDestroyBitstreamBuffer)?;
            required(list.nvEncUnlockInputBuffer)?;required(list.nvEncUnlockBitstream)?;
            Ok(list)
        })();
        match result {Ok(list)=>Ok(Self {list,module}),Err(e)=>{unsafe{let _=FreeLibrary(module);}Err(e)}}
    }
    fn status(&self,session:Handle,operation:&'static str,status:NvStatus)->Error{
        let mut detail=format!("NVENC status {status}");
        if !session.is_null(){if let Some(get)=self.list.nvEncGetLastErrorString {
            // SAFETY: native diagnostic remains session-owned, scan at most4096 bytes.
            let pointer=unsafe{get(session)};if !pointer.is_null(){let mut text=Vec::new();for at in 0..4096{let byte=unsafe{*pointer.add(at)} as u8;if byte==0{break;}text.push(byte);}detail.push_str(": ");detail.push_str(&String::from_utf8_lossy(&text));}
        }}
        Error::Driver {operation,code:status,detail}
    }
}
struct Apartment(std::marker::PhantomData<std::rc::Rc<()>>);
impl Apartment{fn new()->Result<Self,Error>{unsafe{CoInitializeEx(None,COINIT_MULTITHREADED)}.ok().map_err(|e|os("NVENC MTA initialization",e))?;Ok(Self(std::marker::PhantomData))}}
impl Drop for Apartment{fn drop(&mut self){unsafe{CoUninitialize();}}}
struct Slot {input:Handle,output:Handle}
struct Stamp {slot:usize,pts:i64,duration:i64}
struct Native {api:Api,enc:Handle,device:ID3D11Device,slots:Vec<Slot>,pending:VecDeque<Stamp>,eos:bool,apartment:Apartment,lease:Lease}
impl Native {
    fn open(luid:u64,bytes:usize)->Result<Owned,Error>{
        let lease=Lease::new(bytes)?;let apartment=Apartment::new()?;
        let (_,device)=device(luid)?;let api=Api::load()?;
        let mut value=Owned(ManuallyDrop::new(Self {api,enc:ptr::null_mut(),device,slots:Vec::with_capacity(MAX_PENDING),pending:VecDeque::with_capacity(MAX_PENDING),eos:false,lease,apartment}));
        let mut p:NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS=unsafe{mem::zeroed()};p.version=NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;p.deviceType=NV_ENC_DEVICE_TYPE_DIRECTX;p.device=value.device.as_raw();p.apiVersion=NVENCAPI_VERSION;
        let mut opened=ptr::null_mut();let status=unsafe{required(value.api.list.nvEncOpenEncodeSessionEx)?(&mut p,&mut opened)};value.enc=opened;
        if status!=0||value.enc.is_null(){return Err(value.api.status(value.enc,"open NVENC D3D11 session",status));}Ok(value)
    }
    fn cap(&self,id:u32)->Result<u32,Error>{let mut p:NV_ENC_CAPS_PARAM=unsafe{mem::zeroed()};p.version=NV_ENC_CAPS_PARAM_VER;p.capsToQuery=id;let mut value=0;
        let status=unsafe{required(self.api.list.nvEncGetEncodeCaps)?(self.enc,NV_ENC_CODEC_H264_GUID,&mut p,&mut value)};
        if status!=0{return Err(self.api.status(self.enc,"query NVENC H264 capability",status));}u32::try_from(value).map_err(|_|Error::Protocol("negative NVENC capability"))}
    fn caps(&self,luid:u64,name:String)->Result<NvencAdapter,Error>{
        let(mut count,mut written)=(0,0);
        let status=unsafe{required(self.api.list.nvEncGetEncodeProfileGUIDCount)?(self.enc,NV_ENC_CODEC_H264_GUID,&mut count)};
        if status!=0{return Err(self.api.status(self.enc,"query NVENC H264 profile count",status));}if count==0||count>MAX_CAP_ITEMS{return Err(Error::Limit("NVENC profile count"));}
        let mut profiles=vec![NV_ENC_CODEC_H264_GUID;count as usize];let status=unsafe{required(self.api.list.nvEncGetEncodeProfileGUIDs)?(self.enc,NV_ENC_CODEC_H264_GUID,profiles.as_mut_ptr(),count,&mut written)};
        if status!=0{return Err(self.api.status(self.enc,"query NVENC H264 profiles",status));}if written>count{return Err(Error::Protocol("NVENC profile count overflow"));}
        let baseline=profiles[..written as usize].contains(&NV_ENC_H264_PROFILE_BASELINE_GUID);
        let status=unsafe{required(self.api.list.nvEncGetInputFormatCount)?(self.enc,NV_ENC_CODEC_H264_GUID,&mut count)};
        if status!=0{return Err(self.api.status(self.enc,"query NVENC format count",status));}if count==0||count>MAX_CAP_ITEMS{return Err(Error::Limit("NVENC format count"));}
        let mut formats=vec![0;count as usize];written=0;let status=unsafe{required(self.api.list.nvEncGetInputFormats)?(self.enc,NV_ENC_CODEC_H264_GUID,formats.as_mut_ptr(),count,&mut written)};
        if status!=0{return Err(self.api.status(self.enc,"query NVENC input formats",status));}if written>count{return Err(Error::Protocol("NVENC format count overflow"));}
        let nv12=formats[..written as usize].contains(&NV_ENC_BUFFER_FORMAT_NV12);let rc=self.cap(NV_ENC_CAPS_SUPPORTED_RATECONTROL_MODES)?;
        let available=baseline&&nv12&&(rc&NV_ENC_PARAMS_RC_CBR)!=0;
        Ok(NvencAdapter {luid,name,available,reason:(!available).then(||"The driver does not advertise H264 Baseline / NV12 / CBR".into()),min_width:self.cap(NV_ENC_CAPS_WIDTH_MIN)?,min_height:self.cap(NV_ENC_CAPS_HEIGHT_MIN)?,max_width:self.cap(NV_ENC_CAPS_WIDTH_MAX)?,max_height:self.cap(NV_ENC_CAPS_HEIGHT_MAX)?,rate_control_mask:rc,baseline,nv12})
    }
    fn end(&mut self)->Result<(),Error>{if !self.eos&&!self.enc.is_null(){let mut p:NV_ENC_PIC_PARAMS=unsafe{mem::zeroed()};p.version=NV_ENC_PIC_PARAMS_VER;p.encodePicFlags=NV_ENC_PIC_FLAG_EOS;let status=unsafe{required(self.api.list.nvEncEncodePicture)?(self.enc,&mut p)};if status!=0{return Err(self.api.status(self.enc,"drain NVENC",status));}self.eos=true;}Ok(())}
    fn read(&mut self)->Result<Option<(Vec<u8>,i64,i64,u32)>,Error>{
        let Some(stamp)=self.pending.front()else{return Ok(None)};let output=self.slots[stamp.slot].output;
        let mut lock:NV_ENC_LOCK_BITSTREAM=unsafe{mem::zeroed()};lock.version=NV_ENC_LOCK_BITSTREAM_VER;lock.outputBitstream=output;
        // Synchronous SDK lock stays on owner. Stop never claims this has quiesced.
        let status=unsafe{required(self.api.list.nvEncLockBitstream)?(self.enc,&mut lock)};
        if status!=0{return Err(self.api.status(self.enc,"lock NVENC bitstream",status));}
        let result=(||{
            if lock.bitstreamBufferPtr.is_null()||lock.bitstreamSizeInBytes==0||lock.bitstreamSizeInBytes as usize>MAX_AU_BYTES{return Err(Error::Limit("NVENC actual encoded access unit"));}
            if lock.outputTimeStamp!=stamp.pts as u64||lock.outputDuration!=stamp.duration as u64||matches!(lock.pictureType,NV_ENC_PIC_TYPE_B){return Err(Error::Reordered);}
            if lock.hwEncodeStatus!=0{return Err(Error::Platform {operation:"NVENC hardware encode status",code:lock.hwEncodeStatus as i32});}
            // SAFETY: checked native buffer borrowed only until successful Unlock.
            let bytes=unsafe{std::slice::from_raw_parts(lock.bitstreamBufferPtr.cast::<u8>(),lock.bitstreamSizeInBytes as usize)}.to_vec();
            Ok((bytes,stamp.pts,stamp.duration,lock.pictureType))
        })();
        let status=unsafe{required(self.api.list.nvEncUnlockBitstream)?(self.enc,output)};
        if status!=0{return Err(self.api.status(self.enc,"unlock NVENC bitstream",status));}
        self.pending.pop_front();result.map(Some)
    }
    fn retire(&mut self)->Result<(),Error>{
        if self.enc.is_null(){return Ok(());}if !self.slots.is_empty(){self.end()?;while self.read()?.is_some(){}}
        for slot in &mut self.slots {if !slot.input.is_null(){let status=unsafe{required(self.api.list.nvEncDestroyInputBuffer)?(self.enc,slot.input)};if status!=0{return Err(self.api.status(self.enc,"retire NVENC input",status));}slot.input=ptr::null_mut();}if !slot.output.is_null(){let status=unsafe{required(self.api.list.nvEncDestroyBitstreamBuffer)?(self.enc,slot.output)};if status!=0{return Err(self.api.status(self.enc,"retire NVENC bitstream",status));}slot.output=ptr::null_mut();}}
        let status=unsafe{required(self.api.list.nvEncDestroyEncoder)?(self.enc)};if status!=0{return Err(self.api.status(self.enc,"retire NVENC session",status));}self.enc=ptr::null_mut();Ok(())
    }
}
struct Owned(ManuallyDrop<Native>);
impl std::ops::Deref for Owned {type Target=Native;fn deref(&self)->&Native{&self.0}}
impl std::ops::DerefMut for Owned {fn deref_mut(&mut self)->&mut Native{&mut self.0}}
impl Drop for Owned {fn drop(&mut self){
    // SAFETY: take exactly once; failed retirement deliberately retains every
    // library/device/lease. Bounded quarantine is preferable to a native UAF.
    let mut native=unsafe{ManuallyDrop::take(&mut self.0)};
    if let Err(error)=native.retire(){eprintln!("{error}; NVENC resources remain quarantined until process exit");let _=Box::leak(Box::new(native));}
}}
fn adapters()->Result<Vec<(u64,String)>,Error>{
    let factory:IDXGIFactory1=unsafe{CreateDXGIFactory1()}.map_err(|e|os("NVENC DXGI factory",e))?;let mut result=Vec::new();
    for index in 0..MAX_ADAPTERS{let a=match unsafe{factory.EnumAdapters1(index)}{Ok(v)=>v,Err(e)if e.code()==DXGI_ERROR_NOT_FOUND=>break,Err(e)=>return Err(os("enumerate NVENC adapter",e))};let d=unsafe{a.GetDesc1()}.map_err(|e|os("NVENC adapter description",e))?;if d.VendorId!=0x10de||d.Flags&DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32!=0{continue;}
        let end=d.Description.iter().position(|c|*c==0).unwrap_or(d.Description.len());let luid=u64::from(d.AdapterLuid.LowPart)|((d.AdapterLuid.HighPart as u32 as u64)<<32);result.push((luid,String::from_utf16_lossy(&d.Description[..end])));}
    Ok(result)
}
fn device(luid:u64)->Result<(String,ID3D11Device),Error>{
    let factory:IDXGIFactory1=unsafe{CreateDXGIFactory1()}.map_err(|e|os("NVENC DXGI factory",e))?;
    for index in 0..MAX_ADAPTERS{let a=match unsafe{factory.EnumAdapters1(index)}{Ok(v)=>v,Err(e)if e.code()==DXGI_ERROR_NOT_FOUND=>break,Err(e)=>return Err(os("find NVENC adapter",e))};let d=unsafe{a.GetDesc1()}.map_err(|e|os("NVENC adapter identity",e))?;let actual=u64::from(d.AdapterLuid.LowPart)|((d.AdapterLuid.HighPart as u32 as u64)<<32);if actual!=luid{continue;}
        if d.VendorId!=0x10de||d.Flags&DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32!=0{return Err(Error::Unavailable("selected adapter is not an NVIDIA hardware adapter"));}
        let adapter:IDXGIAdapter=a.cast().map_err(|e|os("NVENC adapter interface",e))?;let mut device=None;
        unsafe{D3D11CreateDevice(&adapter,D3D_DRIVER_TYPE_UNKNOWN,HMODULE::default(),D3D11_CREATE_DEVICE_FLAG(0),None,D3D11_SDK_VERSION,Some(&mut device),None,None)}.map_err(|e|os("create selected NVENC D3D11 device",e))?;
        let end=d.Description.iter().position(|c|*c==0).unwrap_or(d.Description.len());return Ok((String::from_utf16_lossy(&d.Description[..end]),device.ok_or(Error::Protocol("NVENC D3D11 device is null"))?));
    }Err(Error::Unavailable("selected NVIDIA adapter LUID is no longer present; choose an available adapter explicitly"))
}
pub(super) fn probe()->Result<NvencCatalog,Error>{
    let mut results=Vec::new();for (luid,name) in adapters()? {
        let result=(||{let owner=Native::open(luid,0)?;owner.caps(luid,name.clone())})();
        results.push(match result {Ok(value)=>value,Err(error)=>NvencAdapter {luid,name,available:false,reason:Some(error.to_string()),min_width:0,min_height:0,max_width:0,max_height:0,rate_control_mask:0,baseline:false,nv12:false}});
    }Ok(NvencCatalog {adapters:results,required_api_major:12,required_api_minor:1})
}
pub(super) struct Encoder {native:Owned,config:Config,configuration:CodecConfiguration,next:usize,submitted:u64,drained:bool}
impl Encoder {
    pub(super) fn new(config:Config,luid:u64)->Result<Self,Error>{
        // Bound the maximum accepted native pitch before DLL/device admission.
        let charge=MAX_PENDING.checked_mul(MAX_PITCH.checked_mul(config.height() as usize*3/2).ok_or(Error::Limit("NVENC input bytes"))?+MAX_AU_BYTES).ok_or(Error::Limit("NVENC working bytes"))?;
        let mut native=Native::open(luid,charge)?;let caps=native.caps(luid,String::new())?;
        if !caps.available||config.width()<caps.min_width||config.width()>caps.max_width||config.height()<caps.min_height||config.height()>caps.max_height{return Err(Error::Unavailable("selected NVENC adapter does not support this Baseline/NV12/CBR format"));}
        let mut preset:Box<NV_ENC_PRESET_CONFIG>=Box::new(unsafe{mem::zeroed()});preset.version=NV_ENC_PRESET_CONFIG_VER;preset.presetCfg.version=NV_ENC_CONFIG_VER;
        let status=unsafe{required(native.api.list.nvEncGetEncodePresetConfigEx)?(native.enc,NV_ENC_CODEC_H264_GUID,NV_ENC_PRESET_P4_GUID,NV_ENC_TUNING_INFO_HIGH_QUALITY,&mut *preset)};
        if status!=0{return Err(native.api.status(native.enc,"read NVENC P4 preset",status));}
        let mut cfg=Box::new(preset.presetCfg);cfg.version=NV_ENC_CONFIG_VER;cfg.profileGUID=NV_ENC_H264_PROFILE_BASELINE_GUID;cfg.gopLength=config.gop();cfg.frameIntervalP=1;cfg.frameFieldMode=1;
        cfg.rcParams=unsafe{mem::zeroed()};cfg.rcParams.version=NV_ENC_RC_PARAMS_VER;cfg.rcParams.rateControlMode=NV_ENC_PARAMS_RC_CBR;cfg.rcParams.averageBitRate=config.bitrate();cfg.rcParams.maxBitRate=config.bitrate();cfg.rcParams.vbvBufferSize=config.bitrate();cfg.rcParams.vbvInitialDelay=config.bitrate();cfg.rcParams.flags=RC_ZERO_REORDER_DELAY;
        let h=unsafe{&mut cfg.encodeCodecConfig.h264Config};h.flags=H264_REPEAT_SPSPPS;h.idrPeriod=config.gop();h.entropyCodingMode=2;h.chromaFormatIDC=1;h.useBFramesAsRef=0;h.adaptiveTransformMode=0;h.fmoMode=0;h.bdirectMode=0;h.maxNumRefFrames=1;
        h.h264VUIParameters=unsafe{mem::zeroed()};let v=&mut h.h264VUIParameters;v.videoSignalTypePresentFlag=1;v.videoFormat=5;v.colourDescriptionPresentFlag=1;v.colourPrimaries=1;v.transferCharacteristics=1;v.colourMatrix=1;
        let mut init:Box<NV_ENC_INITIALIZE_PARAMS>=Box::new(unsafe{mem::zeroed()});init.version=NV_ENC_INITIALIZE_PARAMS_VER;init.encodeGUID=NV_ENC_CODEC_H264_GUID;init.presetGUID=NV_ENC_PRESET_P4_GUID;init.encodeWidth=config.width();init.encodeHeight=config.height();init.darWidth=config.width();init.darHeight=config.height();init.frameRateNum=config.fps().0;init.frameRateDen=config.fps().1;init.enablePTD=1;init.encodeConfig=&mut *cfg;init.tuningInfo=NV_ENC_TUNING_INFO_HIGH_QUALITY;init.bufferFormat=NV_ENC_BUFFER_FORMAT_NV12;
        let status=unsafe{required(native.api.list.nvEncInitializeEncoder)?(native.enc,&mut *init)};if status!=0{return Err(native.api.status(native.enc,"initialize NVENC Baseline CBR",status));}
        for _ in 0..MAX_PENDING{
            let mut i:NV_ENC_CREATE_INPUT_BUFFER=unsafe{mem::zeroed()};i.version=NV_ENC_CREATE_INPUT_BUFFER_VER;i.width=config.width();i.height=config.height();i.bufferFmt=NV_ENC_BUFFER_FORMAT_NV12;i.memoryHeap=0;
            let status=unsafe{required(native.api.list.nvEncCreateInputBuffer)?(native.enc,&mut i)};if status!=0||i.inputBuffer.is_null(){return Err(native.api.status(native.enc,"create NVENC NV12 input",status));}
            native.slots.push(Slot {input:i.inputBuffer,output:ptr::null_mut()});let index=native.slots.len()-1;
            let mut o:NV_ENC_CREATE_BITSTREAM_BUFFER=unsafe{mem::zeroed()};o.version=NV_ENC_CREATE_BITSTREAM_BUFFER_VER;o.size=0;o.memoryHeap=0;
            let status=unsafe{required(native.api.list.nvEncCreateBitstreamBuffer)?(native.enc,&mut o)};native.slots[index].output=o.bitstreamBuffer;if status!=0||o.bitstreamBuffer.is_null(){return Err(native.api.status(native.enc,"create NVENC bitstream",status));}
        }
        let mut header=vec![0;MAX_HEADER_BYTES];let mut written=0;let mut p:NV_ENC_SEQUENCE_PARAM_PAYLOAD=unsafe{mem::zeroed()};p.version=NV_ENC_SEQUENCE_PARAM_PAYLOAD_VER;p.inBufferSize=header.len() as u32;p.spsppsBuffer=header.as_mut_ptr().cast();p.outSPSPPSPayloadSize=&mut written;
        let status=unsafe{required(native.api.list.nvEncGetSequenceParams)?(native.enc,&mut p)};if status!=0{return Err(native.api.status(native.enc,"read actual NVENC SPS/PPS",status));}if written==0||written as usize>header.len(){return Err(Error::Limit("NVENC sequence header"));}header.truncate(written as usize);let configuration=CodecConfiguration::parse(&header)?;
        Ok(Self {native,config,configuration,next:0,submitted:0,drained:false})
    }
    fn outputs(&mut self)->Result<Vec<AccessUnit>,Error>{let mut outputs=Vec::new();while let Some((bytes,pts,duration,picture))=self.native.read()?{
        let bytes=super::bitstream::normalize_access_unit(&bytes)?;let bytes=self.configuration.rewrite_sample(&bytes)?;let unit=AccessUnit::new(self.config.generation(),bytes,pts,pts,duration)?;
        if unit.keyframe()!=(picture==NV_ENC_PIC_TYPE_IDR){return Err(Error::Protocol("NVENC IDR metadata differs from actual VCL NAL"));}outputs.push(unit);
    }Ok(outputs)}
}
impl Adapter for Encoder {
    fn configuration(&self)->Option<&CodecConfiguration>{Some(&self.configuration)}
    fn encode(&mut self,input:Nv12Input)->Result<Vec<AccessUnit>,Error>{
        input.validate(&self.config)?;if self.drained{return Err(Error::Closed);}if self.native.pending.len()>=MAX_PENDING{return Err(Error::Limit("NVENC pending frames"));}
        if self.submitted>u64::from(u32::MAX){return Err(Error::Limit("NVENC native frame index exhausted"));}
        let slot=self.next;if self.native.pending.iter().any(|s|s.slot==slot){return Err(Error::Protocol("NVENC input reused before output retirement"));}let handle=self.native.slots[slot].input;
        let mut p:NV_ENC_LOCK_INPUT_BUFFER=unsafe{mem::zeroed()};p.version=NV_ENC_LOCK_INPUT_BUFFER_VER;p.inputBuffer=handle;
        let status=unsafe{required(self.native.api.list.nvEncLockInputBuffer)?(self.native.enc,&mut p)};if status!=0{return Err(self.native.api.status(self.native.enc,"lock NVENC NV12 input",status));}
        let result=(||{let pitch=p.pitch as usize;let width=self.config.width() as usize;let rows=self.config.height() as usize*3/2;
            if p.bufferDataPtr.is_null()||pitch<width||pitch>MAX_PITCH{return Err(Error::Limit("NVENC native NV12 pitch"));}
            // SAFETY: checked native locked pitch, fixed NV12 row count. CPU
            // source owns exactly width*height*3/2 bytes, padding is initialized.
            let dest=unsafe{std::slice::from_raw_parts_mut(p.bufferDataPtr.cast::<u8>(),pitch*rows)};for row in 0..rows{dest[row*pitch..(row+1)*pitch].fill(0);dest[row*pitch..row*pitch+width].copy_from_slice(&input.bytes()[row*width..(row+1)*width]);}Ok(())})();
        let unlock=unsafe{required(self.native.api.list.nvEncUnlockInputBuffer)?(self.native.enc,handle)};if unlock!=0{return Err(self.native.api.status(self.native.enc,"unlock NVENC NV12 input",unlock));}result?;
        let mut pic:NV_ENC_PIC_PARAMS=unsafe{mem::zeroed()};pic.version=NV_ENC_PIC_PARAMS_VER;pic.inputWidth=self.config.width();pic.inputHeight=self.config.height();pic.inputPitch=p.pitch;pic.inputBuffer=handle;pic.outputBitstream=self.native.slots[slot].output;pic.bufferFmt=NV_ENC_BUFFER_FORMAT_NV12;pic.pictureStruct=NV_ENC_PIC_STRUCT_FRAME;pic.inputTimeStamp=input.pts() as u64;pic.inputDuration=input.duration() as u64;pic.frameIdx=self.submitted as u32;
        // Force the first access unit to be an actual native IDR. Thereafter the
        // configured GOP drives the codec, without relabeling keyframe packets.
        if self.submitted==0{pic.encodePicFlags=2;}
        let status=unsafe{required(self.native.api.list.nvEncEncodePicture)?(self.native.enc,&mut pic)};
        if status!=NV_ENC_SUCCESS&&status!=NV_ENC_ERR_NEED_MORE_INPUT{return Err(self.native.api.status(self.native.enc,"encode NVENC picture",status));}
        self.native.pending.push_back(Stamp {slot,pts:input.pts(),duration:input.duration()});self.next=(slot+1)%MAX_PENDING;self.submitted=self.submitted.checked_add(1).ok_or(Error::Limit("NVENC submitted frame counter"))?;
        if status==NV_ENC_ERR_NEED_MORE_INPUT{Ok(Vec::new())}else{self.outputs()}
    }
    fn drain(&mut self)->Result<Vec<AccessUnit>,Error>{if self.drained{return Ok(Vec::new());}self.native.end()?;self.drained=true;self.outputs()}
}
