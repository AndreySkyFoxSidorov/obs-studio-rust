//! Synchronous Microsoft software H264 MFT. Never constructed on the GUI.
use super::*;
use super::config::Configuration;
use std::{sync::{Arc,atomic::{AtomicUsize,Ordering}},rc::Rc,mem::ManuallyDrop};
use ::windows::{Win32::{Foundation::{E_NOTIMPL,REGDB_E_CLASSNOTREG},Media::MediaFoundation::*,System::Com::*},core::Interface};
#[path="windows_format.rs"]
mod format;
const MAX_PENDING:usize=32;
const MAX_BATCH:usize=4;
const MAX_BATCH_BYTES:usize=128*1024*1024;
const MAX_NATIVE_BUFFER:usize=32*1024*1024;
static OWNERS:AtomicUsize=AtomicUsize::new(0);
struct Permit;
impl Permit {fn new()->Result<Self,MediaFileError>{OWNERS.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|(v<10).then_some(v+1)).map_err(|_|MediaFileError::DecoderBusy("native media decoder owner capacity including preparation/retirement"))?;Ok(Self)}}
impl Drop for Permit {fn drop(&mut self){OWNERS.fetch_sub(1,Ordering::AcqRel);}}
struct Context { _not_send:Rc<()> }
impl Context {
    fn new()->Result<Self,MediaFileError>{
        unsafe {CoInitializeEx(None,COINIT_MULTITHREADED)}.ok().map_err(|e|native("media decoder MTA",e))?;
        if let Err(error)=unsafe {MFStartup(MF_VERSION,MFSTARTUP_FULL)}{unsafe {CoUninitialize()};return Err(native("media decoder MFStartup",error));}
        Ok(Self {_not_send:Rc::new(())})
    }
}
impl Drop for Context {fn drop(&mut self){unsafe {let _=MFShutdown();CoUninitialize();}}}
fn native(operation:&'static str,error: ::windows::core::Error)->MediaFileError {MediaFileError::Native {operation,hresult:error.code().0,detail:error.to_string()}}
fn sdk<T>(operation:&'static str,f:impl FnOnce()->::windows::core::Result<T>)->Result<T,MediaFileError>{f().map_err(|e|native(operation,e))}
struct Stamp {time:i64,original_ns:i64}
pub(crate) struct Decoder {
    transform:IMFTransform,configuration:Arc<Configuration>,input_id:u32,output_id:u32,input_alignment:u32,
    output_info:MFT_OUTPUT_STREAM_INFO,format:format::Format,pending:VecDeque<Stamp>,last_dts:Option<i64>,last_output:Option<i64>,first:bool,drained:bool,draining:bool,
    // Declaration order matters: native interfaces retire before MF/COM and
    // the global owner permit. Rc prevents accidental Send to GUI/finalizers.
    _context:Context,_permit:Permit,
}
impl Decoder {
    pub(crate) fn try_new(raw:&skylive_mp4::AvcConfig,track:&Track)->Result<Option<Self>,MediaFileError>{
        let Some(configuration)=Configuration::admit(raw,track)? else{return Ok(None);};
        match Self::new(Arc::new(configuration)) {
            Ok(decoder)=>Ok(Some(decoder)),
            Err(MediaFileError::Native {hresult,..}) if [REGDB_E_CLASSNOTREG.0,MF_E_TOPO_CODEC_NOT_FOUND.0,MF_E_INVALIDMEDIATYPE.0,MF_E_INVALIDTYPE.0,0x8007007eu32 as i32].contains(&hresult)=>Ok(None),
            Err(MediaFileError::Unsupported(_))=>Ok(None),
            Err(error)=>Err(error),
        }
    }
    fn new(configuration:Arc<Configuration>)->Result<Self,MediaFileError>{
        let permit=Permit::new()?;let context=Context::new()?;
        let transform:IMFTransform=sdk("create Microsoft software H264 decoder",||unsafe {CoCreateInstance(&CLSID_MSH264DecoderMFT,None,CLSCTX_INPROC_SERVER)})?;
        match unsafe {transform.GetAttributes()} {
            Ok(attributes)=>match unsafe {attributes.GetUINT32(&MF_TRANSFORM_ASYNC)} {Ok(0)=>{},Err(e) if e.code()==MF_E_ATTRIBUTENOTFOUND=>{},Ok(_)=>return Err(MediaFileError::Unsupported("asynchronous media transform is not this software adapter")),Err(e)=>return Err(native("media decoder async attribute",e))},
            Err(e) if e.code()==E_NOTIMPL=>{},Err(e)=>return Err(native("media decoder attributes",e)),
        }
        let (mut inputs,mut outputs)=(0,0);sdk("media decoder stream counts",||unsafe {transform.GetStreamCount(&mut inputs,&mut outputs)})?;
        if inputs!=1||outputs!=1{return Err(MediaFileError::Unsupported("native decoder must have one input and output"));}
        let (mut input_ids,mut output_ids)=([0],[0]);match unsafe {transform.GetStreamIDs(&mut input_ids,&mut output_ids)}{Ok(())=>{},Err(e) if e.code()==E_NOTIMPL=>{},Err(e)=>return Err(native("media decoder stream IDs",e))}
        let input_id=input_ids[0];let output_id=output_ids[0];
        let ty=sdk("create native AVC input type",||unsafe {MFCreateMediaType()})?;
        sdk("configure native AVC input type",||unsafe {
            ty.SetGUID(&MF_MT_MAJOR_TYPE,&MFMediaType_Video)?;ty.SetGUID(&MF_MT_SUBTYPE,&MFVideoFormat_H264)?;
            ty.SetUINT64(&MF_MT_FRAME_SIZE,(u64::from(configuration.width)<<32)|u64::from(configuration.height))?;
            ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO,(1u64<<32)|1)?;
            ty.SetUINT32(&MF_MT_INTERLACE_MODE,MFVideoInterlace_Progressive.0 as u32)?;
            ty.SetUINT32(&MF_MT_MPEG2_PROFILE,u32::from(configuration.sps.profile_idc))?;
            ty.SetUINT32(&MF_MT_MPEG2_LEVEL,u32::from(configuration.sps.level_idc))?;
            ty.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER,&configuration.header)?;
            format::set_color(&ty,configuration.chroma_left)?;transform.SetInputType(input_id,&ty,0)
        })?;
        let mut input_info=MFT_INPUT_STREAM_INFO::default();sdk("native decoder input info",||unsafe {transform.GetInputStreamInfo(input_id,&mut input_info)})?;
        format::alignment(input_info.cbAlignment)?;
        if input_info.cbSize as usize>2*1024*1024+65536{return Err(MediaFileError::Limit("native compressed input buffer"));}
        let (format,output_info)=format::negotiate(&transform,output_id,&configuration)?;
        sdk("begin native media decoder",||unsafe {transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,0)?;transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM,0)})?;
        Ok(Self {transform,configuration,input_id,output_id,input_alignment:input_info.cbAlignment,output_info,format,pending:VecDeque::new(),last_dts:None,last_output:None,first:true,drained:false,draining:false,_context:context,_permit:permit})
    }
    pub(crate) fn recreate(&self)->Result<Self,MediaFileError>{Self::new(self.configuration.clone())}
    pub(crate) fn decode(&mut self,data:&[u8],pts_ns:i64,dts_ns:i64,duration_ns:u64,sync:bool)->Result<Vec<NativePicture>,MediaFileError>{
        if self.draining||self.drained||self.pending.len()>=MAX_PENDING{return Err(MediaFileError::Limit("native decoder pending input bound"));}
        let pts=pts_ns.div_euclid(100);let dts=dts_ns.div_euclid(100);let duration=i64::try_from(duration_ns/100).map_err(|_|MediaFileError::TimeOverflow)?;
        if duration<=0||self.last_dts.is_some_and(|old|dts<=old)||self.pending.iter().any(|p|p.time==pts){return Err(MediaFileError::Unsupported("native decoder requires distinct100ns timestamps and increasing decode time"));}
        let unit=self.configuration.access_unit(data,self.first,sync)?;
        let sample=format::input_sample(&unit.bytes,self.input_alignment,pts,dts,duration,sync,self.first)?;
        let mut pictures=Vec::new();
        match unsafe {self.transform.ProcessInput(self.input_id,&sample,0)} {
            Ok(())=>{},
            Err(e) if e.code()==MF_E_NOTACCEPTING=>{
                self.pump(&mut pictures)?;
                sdk("native decoder retry same input",||unsafe {self.transform.ProcessInput(self.input_id,&sample,0)})?;
            },
            Err(e)=>return Err(native("native decoder ProcessInput",e)),
        }
        // The bitstream may explicitly suppress pictures retained before IDR.
        // Retire those identities only after this exact input was accepted.
        if unit.discard_prior {self.pending.clear();}
        self.pending.push_back(Stamp {time:pts,original_ns:pts_ns});self.last_dts=Some(dts);self.first=false;
        self.pump(&mut pictures)?;Ok(pictures)
    }
    pub(crate) fn drain(&mut self)->Result<Vec<NativePicture>,MediaFileError>{
        if self.drained{return Ok(Vec::new());}
        if !self.draining {sdk("drain native decoder",||unsafe {self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM,0)?;self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN,0)})?;self.draining=true;}
        let mut pictures=Vec::new();let finished=self.pump(&mut pictures)?;
        if finished&&!self.pending.is_empty(){return Err(MediaFileError::Decode("native decoder drain omitted accepted PTS identities".into()));}
        self.drained=finished;Ok(pictures)
    }
    pub(crate) fn is_drained(&self)->bool{self.drained}
    fn pump(&mut self,pictures:&mut Vec<NativePicture>)->Result<bool,MediaFileError>{
        for _ in 0..64 {
            let bytes=self.configuration.width as usize*self.configuration.height as usize*3/2;
            if pictures.len()>=MAX_BATCH||pictures.iter().map(|p|p.byte_len()).sum::<usize>().saturating_add(bytes)>MAX_BATCH_BYTES{return Ok(false);}
            let sample=if self.output_info.dwFlags&(MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32|MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)!=0 {None}else{Some(format::empty_sample(self.output_info.cbSize,self.output_info.cbAlignment)?)};
            let mut output=MFT_OUTPUT_DATA_BUFFER {dwStreamID:self.output_id,pSample:ManuallyDrop::new(sample),dwStatus:0,pEvents:ManuallyDrop::new(None)};let mut status=0;
            let result=unsafe {self.transform.ProcessOutput(0,std::slice::from_mut(&mut output),&mut status)};
            let sample=unsafe {ManuallyDrop::take(&mut output.pSample)};let events=unsafe {ManuallyDrop::take(&mut output.pEvents)};drop(events);
            match result {
                Err(e) if e.code()==MF_E_TRANSFORM_NEED_MORE_INPUT=>return Ok(true),
                Err(e) if e.code()==MF_E_TRANSFORM_STREAM_CHANGE=>{drop(sample);let (format,info)=format::negotiate(&self.transform,self.output_id,&self.configuration)?;self.format=format;self.output_info=info;continue;},
                Err(e)=>return Err(native("native decoder ProcessOutput",e)),Ok(())=>{},
            }
            let sample=sample.ok_or_else(||MediaFileError::Decode("native decoder successful output has no sample".into()))?;
            if match unsafe {sample.GetUINT32(&MFSampleExtension_Interlaced)} {Ok(v)=>v!=0,Err(e) if e.code()==MF_E_ATTRIBUTENOTFOUND=>false,Err(e)=>return Err(native("native decoded interlace flag",e))}{return Err(MediaFileError::Unsupported("native decoded interlaced sample"));}
            let time=sdk("native decoded sample PTS",||unsafe {sample.GetSampleTime()})?;
            let at=self.pending.iter().position(|p|p.time==time).ok_or_else(||MediaFileError::Decode("native decoded PTS has no accepted input identity".into()))?;
            let pts=self.pending[at].original_ns;
            if self.last_output.is_some_and(|old|pts<=old){return Err(MediaFileError::Decode("native decoded presentation time is not increasing".into()));}
            let picture=format::picture(&sample,&self.format,&self.configuration,pts)?;
            self.pending.remove(at);self.last_output=Some(pts);pictures.push(picture);
        }
        Err(MediaFileError::Limit("native decoder output/stream-change calls"))
    }
}
impl Drop for Decoder {
    fn drop(&mut self){unsafe {let _=self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH,0);let _=self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING,0);}}
}
