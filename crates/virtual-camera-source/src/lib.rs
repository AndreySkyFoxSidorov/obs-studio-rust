//! Rust FrameServer COM DLL. Loaded only through an explicitly installed class.
#![cfg(target_os="windows")]
#![allow(non_snake_case)]
use std::{ffi::c_void,sync::{Arc,Mutex,atomic::{AtomicBool,AtomicIsize,AtomicU64,AtomicU8,AtomicUsize,Ordering},mpsc::{self,SyncSender}},thread,time::{Duration,Instant}};
use windows::{core::{implement,AgileReference,Error,GUID,HRESULT,Interface,IUnknown,PCWSTR,PROPVARIANT,PWSTR,Result},Win32::{Foundation::{BOOL,CLASS_E_CLASSNOTAVAILABLE,CLASS_E_NOAGGREGATION,E_INVALIDARG,E_NOTIMPL,E_POINTER,E_UNEXPECTED,S_FALSE,S_OK},Media::{MediaFoundation::*,KernelStreaming::{PINNAME_VIDEO_CAPTURE,IKsControl,IKsControl_Impl,KSIDENTIFIER}},System::Com::{IClassFactory,IClassFactory_Impl,CoInitializeEx,CoUninitialize,COINIT_MULTITHREADED}}};
use skylive_virtual_camera_protocol::{Profile,ids,transport::{Info,Mapping}};
mod attributes;
static MODULE:AtomicIsize=AtomicIsize::new(0);
static OBJECTS:AtomicUsize=AtomicUsize::new(0);
static LOCKS:AtomicUsize=AtomicUsize::new(0);
struct Workers {creating:usize,handles:Vec<thread::JoinHandle<()>>}
static WORKERS:Mutex<Workers>=Mutex::new(Workers {creating:0,handles:Vec::new()});
#[link(name="kernel32")]
unsafe extern "system"{fn WaitForSingleObject(handle:*mut c_void,timeout:u32)->u32;}
fn retire(workers:&mut Workers){
    use std::os::windows::io::AsRawHandle;
    // Rust is_finished may precede actual OS-thread termination. DLL unloading
    // requires the kernel handle to be signaled, after the whole trampoline.
    workers.handles.retain(|handle|unsafe{WaitForSingleObject(handle.as_raw_handle(),0)}!=0);
}
struct ObjectCredit;
impl ObjectCredit{fn new()->Self{OBJECTS.fetch_add(1,Ordering::AcqRel);Self}}
impl Drop for ObjectCredit{fn drop(&mut self){OBJECTS.fetch_sub(1,Ordering::AcqRel);}}
struct SourceCredit {pending:bool}
impl SourceCredit {
    fn new()->Result<Self>{let mut workers=lock(&WORKERS)?;retire(&mut workers);if workers.creating+workers.handles.len()>=2{return Err(MF_E_NOTACCEPTING.into());}workers.creating+=1;Ok(Self {pending:true})}
    fn register(mut self,handle:thread::JoinHandle<()>)->Result<()>{let mut workers=lock(&WORKERS)?;workers.creating-=1;workers.handles.push(handle);self.pending=false;Ok(())}
}
impl Drop for SourceCredit{fn drop(&mut self){if self.pending{if let Ok(mut workers)=WORKERS.lock(){workers.creating=workers.creating.saturating_sub(1);}}}}
fn lock<T>(value:&Mutex<T>)->Result<std::sync::MutexGuard<'_,T>>{value.lock().map_err(|_|Error::from(E_UNEXPECTED))}
fn io_error(error:std::io::Error)->Error{Error::new(E_UNEXPECTED,format!("virtual camera transport: {error}"))}
unsafe fn query(value:&IUnknown,iid:*const GUID,output:*mut *mut c_void)->Result<()> {
    if iid.is_null() || output.is_null(){return Err(E_POINTER.into());}
    unsafe{*output=std::ptr::null_mut();(value.vtable().QueryInterface)(value.as_raw(),iid,output).ok()}
}
fn attributes()->Result<IMFAttributes>{let mut value=None;unsafe{MFCreateAttributes(&mut value,12)?;}value.ok_or_else(||E_UNEXPECTED.into())}
fn text(store:&IMFAttributes,key:&GUID)->Result<String>{
    let n=unsafe{store.GetStringLength(key)?};if n==0 || n>1024{return Err(E_INVALIDARG.into());}
    let mut value=vec![0u16;n as usize+1];unsafe{store.GetString(key,&mut value,None)?;}
    String::from_utf16(&value[..n as usize]).map_err(|_|E_INVALIDARG.into())
}
struct Events {queue:AgileReference<IMFMediaEventQueue>,queued:AtomicUsize}
impl Events {
    fn new()->Result<Self>{Ok(Self {queue:AgileReference::new(&unsafe{MFCreateEventQueue()?})?,queued:AtomicUsize::new(0)})}
    fn reserve(&self)->Result<()> {self.queued.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|(v<16).then_some(v+1)).map_err(|_|Error::from(MF_E_NOTACCEPTING))?;Ok(())}
    fn var(&self,event:u32,guid:*const GUID,status:HRESULT,value:*const PROPVARIANT)->Result<()> {
        self.reserve()?;let result=unsafe{self.queue.resolve()?.QueueEventParamVar(event,guid,status,value)};
        if result.is_err(){self.queued.fetch_sub(1,Ordering::AcqRel);}result
    }
    fn unk(&self,event:u32,value:&IUnknown)->Result<()> {
        self.reserve()?;let result=unsafe{self.queue.resolve()?.QueueEventParamUnk(event,&GUID::zeroed(),S_OK,value)};
        if result.is_err(){self.queued.fetch_sub(1,Ordering::AcqRel);}result
    }
    fn take(&self,event:IMFMediaEvent)->IMFMediaEvent {let _=self.queued.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|Some(v.saturating_sub(1)));event}
    fn get(&self,flags:MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS)->Result<IMFMediaEvent>{let event=unsafe{self.queue.resolve()?.GetEvent(flags.0 as u32)?};Ok(self.take(event))}
    fn begin(&self,callback:Option<&IMFAsyncCallback>,state:Option<&IUnknown>)->Result<()>{unsafe{self.queue.resolve()?.BeginGetEvent(callback,state)}}
    fn end(&self,result:Option<&IMFAsyncResult>)->Result<IMFMediaEvent>{let event=unsafe{self.queue.resolve()?.EndGetEvent(result)?};Ok(self.take(event))}
    fn shutdown(&self){if let Ok(queue)=self.queue.resolve(){let _=unsafe{queue.Shutdown()};}}
}
enum Work {State {active:bool,reply:SyncSender<Result<()>>},Sample {epoch:u64,token:Option<AgileReference<IUnknown>>}}
struct Shared {
    source_events:Events,stream_events:Events,source_attributes:AgileReference<IMFAttributes>,stream_attributes:AgileReference<IMFAttributes>,
    descriptor:AgileReference<IMFStreamDescriptor>,presentation:AgileReference<IMFPresentationDescriptor>,
    parent:Mutex<Option<AgileReference<IMFMediaSource>>>,stream:Mutex<Option<AgileReference<IMFMediaStream2>>>,
    allocator_status:Mutex<Option<AgileReference<IMFVideoSampleAllocatorCallback>>>,
    control:Mutex<()>,state:AtomicU8,stream_state:AtomicU8,ever_started:AtomicBool,epoch:AtomicU64,close:AtomicBool,inflight:AtomicBool,
    sender:SyncSender<Work>,profile:Profile,
}
struct SourceInit(Option<Arc<Shared>>);
impl Drop for SourceInit{fn drop(&mut self){if let Some(shared)=self.0.take(){let _=shared.shutdown();}}}
impl Shared {
    fn check(&self)->Result<()> {if self.state.load(Ordering::Acquire)==2{Err(MF_E_SHUTDOWN.into())}else{Ok(())}}
    fn event(&self,event:IMFMediaEvent)->IMFMediaEvent {
        if unsafe{event.GetType()}.ok()==Some(MEMediaSample.0 as u32){self.inflight.store(false,Ordering::Release);}event
    }
    fn state_work(&self,active:bool)->Result<()> {
        let (send,receive)=mpsc::sync_channel(1);
        self.sender.try_send(Work::State {active,reply:send}).map_err(|_|Error::from(MF_E_NOTACCEPTING))?;
        receive.recv_timeout(Duration::from_secs(2)).map_err(|_|Error::from(E_UNEXPECTED))?
    }
    fn shutdown(&self)->Result<()> {
        if self.state.swap(2,Ordering::AcqRel)==2{return Ok(());}
        self.close.store(true,Ordering::Release);self.epoch.fetch_add(1,Ordering::AcqRel);
        self.source_events.shutdown();self.stream_events.shutdown();lock(&self.parent)?.take();lock(&self.stream)?.take();lock(&self.allocator_status)?.take();Ok(())
    }
}
#[implement(IMFActivate)]
struct Activate {attributes:AgileReference<IMFAttributes>,source:Mutex<Option<AgileReference<IMFMediaSourceEx>>>,_credit:ObjectCredit}
impl Activate {fn new()->Result<Self>{Ok(Self {attributes:AgileReference::new(&attributes()?)?,source:Mutex::new(None),_credit:ObjectCredit::new()})}}
impl IMFActivate_Impl for Activate_Impl {
    fn ActivateObject(&self,iid:*const GUID,output:*mut *mut c_void)->Result<()> {
        if iid.is_null() || output.is_null(){return Err(E_POINTER.into());}
        let mut current=lock(&self.source)?;
        let source=match &*current {Some(source)=>source.resolve()?,None=>{
            let source=Source::create(self.attributes.resolve()?)?;*current=Some(AgileReference::new(&source)?);source
        }};
        unsafe{query(&source.cast()?,iid,output)}
    }
    fn ShutdownObject(&self)->Result<()> {if let Some(source)=lock(&self.source)?.take(){unsafe{source.resolve()?.Shutdown()?;}}Ok(())}
    fn DetachObject(&self)->Result<()> {lock(&self.source)?.take();Ok(())}
}
#[implement(IMFMediaSourceEx,IMFGetService,IKsControl,IMFSampleAllocatorControl)]
struct Source {shared:Arc<Shared>,_credit:ObjectCredit}
#[implement(IMFMediaStream2)]
struct Stream {shared:Arc<Shared>,_credit:ObjectCredit}
impl Source {
    fn create(source_attributes:IMFAttributes)->Result<IMFMediaSourceEx> {
        let profile=Profile::new(unsafe{source_attributes.GetUINT32(&ids::WIDTH)?},unsafe{source_attributes.GetUINT32(&ids::HEIGHT)?},unsafe{source_attributes.GetUINT32(&ids::FPS)?}).ok_or_else(||Error::from(E_INVALIDARG))?;
        let info=Info {file:text(&source_attributes,&ids::FILE)?.into(),mutex:text(&source_attributes,&ids::MUTEX)?,token:unsafe{source_attributes.GetGUID(&ids::TOKEN)?},profile};
        let credit=SourceCredit::new()?;
        let media=unsafe{MFCreateMediaType()?};unsafe{
            media.SetGUID(&MF_MT_MAJOR_TYPE,&MFMediaType_Video)?;media.SetGUID(&MF_MT_SUBTYPE,&MFVideoFormat_NV12)?;
            media.SetUINT64(&MF_MT_FRAME_SIZE,(u64::from(profile.width())<<32)|u64::from(profile.height()))?;
            media.SetUINT64(&MF_MT_FRAME_RATE,(u64::from(profile.fps())<<32)|1)?;media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO,(1u64<<32)|1)?;
            media.SetUINT32(&MF_MT_INTERLACE_MODE,2)?;media.SetUINT32(&MF_MT_DEFAULT_STRIDE,profile.width())?;
            media.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES,1)?;media.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT,1)?;media.SetUINT32(&MF_MT_SAMPLE_SIZE,profile.bytes() as u32)?;
            media.SetUINT32(&MF_MT_YUV_MATRIX,MFVideoTransferMatrix_BT709.0 as u32)?;
            media.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE,MFNominalRange_16_235.0 as u32)?;
            media.SetUINT32(&MF_MT_VIDEO_PRIMARIES,MFVideoPrimaries_BT709.0 as u32)?;
            media.SetUINT32(&MF_MT_TRANSFER_FUNCTION,MFVideoTransFunc_709.0 as u32)?;
        }
        let descriptor=unsafe{MFCreateStreamDescriptor(0,&[Some(media.clone())])?};unsafe{descriptor.GetMediaTypeHandler()?.SetCurrentMediaType(&media)?;}
        let stream_attributes=attributes()?;unsafe{
            stream_attributes.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY,&PINNAME_VIDEO_CAPTURE)?;
            stream_attributes.SetUINT32(&MF_DEVICESTREAM_STREAM_ID,0)?;stream_attributes.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED,1)?;
            stream_attributes.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,1)?;
        }
        let presentation=unsafe{MFCreatePresentationDescriptor(Some(&[Some(descriptor.clone())]))?};unsafe{presentation.SelectStream(0)?;}
        let (sender,receiver)=mpsc::sync_channel(2);
        let shared=Arc::new(Shared {source_events:Events::new()?,stream_events:Events::new()?,source_attributes:AgileReference::new(&source_attributes)?,
            stream_attributes:AgileReference::new(&stream_attributes)?,descriptor:AgileReference::new(&descriptor)?,presentation:AgileReference::new(&presentation)?,
            parent:Mutex::new(None),stream:Mutex::new(None),allocator_status:Mutex::new(None),control:Mutex::new(()),state:AtomicU8::new(0),stream_state:AtomicU8::new(0),ever_started:AtomicBool::new(false),epoch:AtomicU64::new(1),close:AtomicBool::new(false),inflight:AtomicBool::new(false),sender,profile});
        let mut initialization=SourceInit(Some(shared.clone()));
        let source:IMFMediaSourceEx=Self {shared:shared.clone(),_credit:ObjectCredit::new()}.into();
        let stream:IMFMediaStream2=Stream {shared:shared.clone(),_credit:ObjectCredit::new()}.into();
        *lock(&shared.parent)?=Some(AgileReference::new(&source.cast::<IMFMediaSource>()?)?);
        *lock(&shared.stream)?=Some(AgileReference::new(&stream)?);
        let state=shared.clone();let (ready,result)=mpsc::sync_channel(1);let worker_media=AgileReference::new(&media)?;
        let handle=match thread::Builder::new().name("skylive-vcam-source".into()).spawn(move||sample_worker(state,info,worker_media,receiver,ready)) {
            Ok(handle)=>handle,Err(error)=>{shared.shutdown()?;return Err(Error::new(E_UNEXPECTED,error.to_string()));}
        };
        credit.register(handle)?;
        match result.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(()))=>{initialization.0.take();Ok(source)},Ok(Err(error))=>{shared.shutdown()?;Err(error)},Err(_)=>{shared.shutdown()?;Err(E_UNEXPECTED.into())}
        }
    }
}
impl IMFMediaSource_Impl for Source_Impl {
    fn GetCharacteristics(&self)->Result<u32>{self.shared.check()?;Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)}
    fn CreatePresentationDescriptor(&self)->Result<IMFPresentationDescriptor>{self.shared.check()?;unsafe{self.shared.presentation.resolve()?.Clone()}}
    fn Start(&self,descriptor:Option<&IMFPresentationDescriptor>,format:*const GUID,position:*const PROPVARIANT)->Result<()> {
        let _control=lock(&self.shared.control)?;self.shared.check()?;
        if position.is_null() || (!format.is_null() && unsafe{*format}!=GUID::zeroed()){return Err(E_INVALIDARG.into());}
        // Live starts accept VT_EMPTY or the conventional zero VT_I8 origin.
        // Samples/events use correlated QPC time; nonzero seeking is unsupported.
        let variant=unsafe{position.cast::<u16>().read_unaligned()};
        if variant!=0 && (variant!=20 || i64::try_from(unsafe{&*position})?!=0){return Err(MF_E_INVALIDREQUEST.into());}
        let descriptor=descriptor.ok_or_else(||Error::from(E_POINTER))?;
        if unsafe{descriptor.GetStreamDescriptorCount()?}!=1{return Err(E_INVALIDARG.into());}
        let mut selected=BOOL(0);let mut stream_descriptor=None;unsafe{descriptor.GetStreamDescriptorByIndex(0,&mut selected,&mut stream_descriptor)?;}
        if !selected.as_bool(){return Err(E_INVALIDARG.into());}
        let stream_descriptor=stream_descriptor.ok_or_else(||Error::from(E_INVALIDARG))?;
        if unsafe{stream_descriptor.GetStreamIdentifier()?}!=0{return Err(MF_E_INVALIDSTREAMNUMBER.into());}
        let media=unsafe{stream_descriptor.GetMediaTypeHandler()?.GetCurrentMediaType()?};validate_media(&media,self.shared.profile)?;
        if self.shared.state.load(Ordering::Acquire)==1{return Err(MF_E_INVALID_STATE_TRANSITION.into());}
        self.shared.state_work(true)?;self.shared.epoch.fetch_add(1,Ordering::AcqRel);self.shared.stream_state.store(1,Ordering::Release);self.shared.state.store(1,Ordering::Release);
        let stream=lock(&self.shared.stream)?.as_ref().ok_or_else(||Error::from(MF_E_SHUTDOWN))?.resolve()?;
        let event=if self.shared.ever_started.swap(true,Ordering::AcqRel){MEUpdatedStream}else{MENewStream};
        self.shared.source_events.unk(event.0 as u32,&stream.cast()?)?;
        let start=PROPVARIANT::from(unsafe{MFGetSystemTime()});
        self.shared.stream_events.var(MEStreamStarted.0 as u32,&GUID::zeroed(),S_OK,&start)?;
        self.shared.source_events.var(MESourceStarted.0 as u32,&GUID::zeroed(),S_OK,&start)
    }
    fn Stop(&self)->Result<()> {
        let _control=lock(&self.shared.control)?;self.shared.check()?;
        if self.shared.state.swap(0,Ordering::AcqRel)!=1{return Err(MF_E_INVALID_STATE_TRANSITION.into());}
        self.shared.stream_state.store(0,Ordering::Release);self.shared.epoch.fetch_add(1,Ordering::AcqRel);self.shared.state_work(false)?;self.shared.inflight.store(false,Ordering::Release);
        let stop=PROPVARIANT::from(unsafe{MFGetSystemTime()});
        self.shared.stream_events.var(MEStreamStopped.0 as u32,&GUID::zeroed(),S_OK,&stop)?;
        self.shared.source_events.var(MESourceStopped.0 as u32,&GUID::zeroed(),S_OK,&stop)
    }
    fn Pause(&self)->Result<()> {Err(MF_E_INVALID_STATE_TRANSITION.into())}
    fn Shutdown(&self)->Result<()> {self.shared.shutdown()}
}
impl IMFMediaSourceEx_Impl for Source_Impl {
    fn GetSourceAttributes(&self)->Result<IMFAttributes>{self.shared.check()?;self.shared.source_attributes.resolve()}
    fn GetStreamAttributes(&self,stream:u32)->Result<IMFAttributes>{self.shared.check()?;if stream!=0{return Err(MF_E_INVALIDSTREAMNUMBER.into());}self.shared.stream_attributes.resolve()}
    fn SetD3DManager(&self,_manager:Option<&IUnknown>)->Result<()> {Err(E_NOTIMPL.into())}
}
impl IMFGetService_Impl for Source_Impl {
    fn GetService(&self,service:*const GUID,iid:*const GUID,output:*mut *mut c_void)->Result<()> {
        self.shared.check()?;if service.is_null() || iid.is_null() || output.is_null(){return Err(E_POINTER.into());}
        unsafe{*output=std::ptr::null_mut();}
        if unsafe{*service}!=MF_MEDIASOURCE_SERVICE{return Err(MF_E_UNSUPPORTED_SERVICE.into());}
        let parent=lock(&self.shared.parent)?.as_ref().ok_or_else(||Error::from(MF_E_SHUTDOWN))?.resolve()?;
        unsafe{query(&parent.cast()?,iid,output)}
    }
}
fn unsupported_control(returned:*mut u32)->Result<()> {
    if returned.is_null(){return Err(E_POINTER.into());}unsafe{*returned=0;}Err(HRESULT::from_win32(1170).into())
}
impl IKsControl_Impl for Source_Impl {
    fn KsProperty(&self,_property:*const KSIDENTIFIER,_propertylength:u32,_data:*mut c_void,_length:u32,returned:*mut u32)->Result<()> {self.shared.check()?;unsupported_control(returned)}
    fn KsMethod(&self,_method:*const KSIDENTIFIER,_methodlength:u32,_data:*mut c_void,_length:u32,returned:*mut u32)->Result<()> {self.shared.check()?;unsupported_control(returned)}
    fn KsEvent(&self,_event:*const KSIDENTIFIER,_eventlength:u32,_data:*mut c_void,_length:u32,returned:*mut u32)->Result<()> {self.shared.check()?;unsupported_control(returned)}
}
impl IMFSampleAllocatorControl_Impl for Source_Impl {
    fn SetDefaultAllocator(&self,stream:u32,_allocator:Option<&IUnknown>)->Result<()> {self.shared.check()?;if stream!=0{return Err(MF_E_INVALIDSTREAMNUMBER.into());}Ok(())}
    fn GetAllocatorUsage(&self,stream:u32,input:*mut u32,usage:*mut MFSampleAllocatorUsage)->Result<()> {
        self.shared.check()?;if stream!=0{return Err(MF_E_INVALIDSTREAMNUMBER.into());}if input.is_null() || usage.is_null(){return Err(E_POINTER.into());}
        unsafe{*input=0;*usage=MFSampleAllocatorUsage_UsesCustomAllocator;}Ok(())
    }
}
fn validate_media(media:&IMFMediaType,profile:Profile)->Result<()> {
    if unsafe{media.GetGUID(&MF_MT_MAJOR_TYPE)?}!=MFMediaType_Video || unsafe{media.GetGUID(&MF_MT_SUBTYPE)?}!=MFVideoFormat_NV12
        || unsafe{media.GetUINT64(&MF_MT_FRAME_SIZE)?}!=((u64::from(profile.width())<<32)|u64::from(profile.height()))
        || unsafe{media.GetUINT64(&MF_MT_FRAME_RATE)?}!=((u64::from(profile.fps())<<32)|1) {return Err(MF_E_INVALIDMEDIATYPE.into());}Ok(())
}
impl IMFMediaStream_Impl for Stream_Impl {
    fn GetMediaSource(&self)->Result<IMFMediaSource>{self.shared.check()?;lock(&self.shared.parent)?.as_ref().ok_or_else(||Error::from(MF_E_SHUTDOWN))?.resolve()}
    fn GetStreamDescriptor(&self)->Result<IMFStreamDescriptor>{self.shared.check()?;self.shared.descriptor.resolve()}
    fn RequestSample(&self,token:Option<&IUnknown>)->Result<()> {
        self.shared.check()?;if self.shared.state.load(Ordering::Acquire)!=1 || self.shared.stream_state.load(Ordering::Acquire)!=1{return Err(MF_E_MEDIA_SOURCE_WRONGSTATE.into());}
        // Reject before retaining another caller token. The native allocator
        // has exactly four tracked samples, including downstream-held samples.
        let free=lock(&self.shared.allocator_status)?.as_ref().ok_or_else(||Error::from(MF_E_SHUTDOWN))?.resolve()?;
        if unsafe{free.GetFreeSampleCount()?}<=0{return Err(MF_E_NOTACCEPTING.into());}
        self.shared.inflight.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).map_err(|_|Error::from(MF_E_NOTACCEPTING))?;
        let token=match token.map(AgileReference::new).transpose(){Ok(value)=>value,Err(error)=>{self.shared.inflight.store(false,Ordering::Release);return Err(error);}};
        let work=Work::Sample {epoch:self.shared.epoch.load(Ordering::Acquire),token};
        if self.shared.sender.try_send(work).is_err(){self.shared.inflight.store(false,Ordering::Release);return Err(MF_E_NOTACCEPTING.into());}Ok(())
    }
}
impl IMFMediaStream2_Impl for Stream_Impl {
    fn SetStreamState(&self,state:MF_STREAM_STATE)->Result<()> {self.shared.check()?;if state==MF_STREAM_STATE_RUNNING {self.shared.stream_state.store(1,Ordering::Release);Ok(())}else if state==MF_STREAM_STATE_STOPPED {self.shared.stream_state.store(0,Ordering::Release);Ok(())}else{Err(MF_E_INVALID_STATE_TRANSITION.into())}}
    fn GetStreamState(&self)->Result<MF_STREAM_STATE>{self.shared.check()?;Ok(if self.shared.stream_state.load(Ordering::Acquire)==1 {MF_STREAM_STATE_RUNNING}else{MF_STREAM_STATE_STOPPED})}
}
macro_rules! events {
    ($owner:ty,$field:ident,$stream:expr)=>{
        impl IMFMediaEventGenerator_Impl for $owner {
            fn GetEvent(&self,flags:MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS)->Result<IMFMediaEvent>{self.shared.check()?;let event=self.shared.$field.get(flags)?;Ok(if $stream{self.shared.event(event)}else{event})}
            fn BeginGetEvent(&self,callback:Option<&IMFAsyncCallback>,state:Option<&IUnknown>)->Result<()>{self.shared.check()?;self.shared.$field.begin(callback,state)}
            fn EndGetEvent(&self,result:Option<&IMFAsyncResult>)->Result<IMFMediaEvent>{self.shared.check()?;let event=self.shared.$field.end(result)?;Ok(if $stream{self.shared.event(event)}else{event})}
            fn QueueEvent(&self,event:u32,guid:*const GUID,status:HRESULT,value:*const PROPVARIANT)->Result<()>{self.shared.check()?;self.shared.$field.var(event,guid,status,value)}
        }
    }
}
events!(Source_Impl,source_events,false);events!(Stream_Impl,stream_events,true);
fn sample_worker(shared:Arc<Shared>,info:Info,media:AgileReference<IMFMediaType>,receiver:mpsc::Receiver<Work>,ready:SyncSender<Result<()>>) {
    let initialized=unsafe{CoInitializeEx(None,COINIT_MULTITHREADED)};
    if initialized.is_err(){let _=ready.try_send(Err(initialized.into()));return;}
    let initialized=(||->Result<(Mapping,IMFVideoSampleAllocatorEx,IMFVideoSampleAllocatorCallback)> {
        let mapping=Mapping::open(info).map_err(io_error)?;let mut pointer=std::ptr::null_mut();
        unsafe{MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID,&mut pointer)?;}
        if pointer.is_null(){return Err(E_UNEXPECTED.into());}
        let allocator=unsafe{IMFVideoSampleAllocatorEx::from_raw(pointer)};
        unsafe{allocator.InitializeSampleAllocatorEx(4,4,None,&media.resolve()?)?;}
        let callback:IMFVideoSampleAllocatorCallback=allocator.cast()?;
        *lock(&shared.allocator_status)?=Some(AgileReference::new(&callback)?);
        Ok((mapping,allocator,callback))
    })();
    drop(media);
    let (mapping,allocator,allocator_status)=match initialized {Ok(value)=>{let _=ready.try_send(Ok(()));value},Err(error)=>{let _=ready.try_send(Err(error));drop(shared);unsafe{CoUninitialize();}return;}};
    let mut active=false;let mut buffer=vec![0u8;shared.profile.bytes()];let mut deadline=Instant::now();
    while !shared.close.load(Ordering::Acquire) {
        let work=match receiver.recv_timeout(Duration::from_millis(10)){Ok(work)=>work,Err(mpsc::RecvTimeoutError::Timeout)=>continue,Err(_)=>break};
        match work {
            Work::State {active:requested,reply}=>{
                let result=if requested!=active {mapping.consumer(requested).map_err(io_error)}else{Ok(())};
                if result.is_ok(){active=requested;deadline=Instant::now();}let _=reply.try_send(result);
            }
            Work::Sample {epoch,token}=>{
                let result=(||->Result<()> {
                    while Instant::now()<deadline {if shared.close.load(Ordering::Acquire){return Err(MF_E_SHUTDOWN.into());}thread::sleep(Duration::from_millis(1));}
                    let stop=Instant::now()+Duration::from_secs(5);
                    loop {
                        if shared.close.load(Ordering::Acquire) || shared.state.load(Ordering::Acquire)!=1 || shared.stream_state.load(Ordering::Acquire)!=1 || shared.epoch.load(Ordering::Acquire)!=epoch {return Err(MF_E_MEDIA_SOURCE_WRONGSTATE.into());}
                        match mapping.copy_latest(&mut buffer) {
                            Ok(_)=>break,Err(error) if error.kind()==std::io::ErrorKind::WouldBlock && Instant::now()<stop=>thread::sleep(Duration::from_millis(1)),Err(error)=>return Err(io_error(error)),
                        }
                    }
                    let sample=unsafe{allocator.AllocateSample()?};let media=unsafe{sample.GetBufferByIndex(0)?};
                    if let Ok(image)=media.cast::<IMF2DBuffer>() {unsafe{image.ContiguousCopyFrom(&buffer)?;}}
                    else {
                        let mut pointer=std::ptr::null_mut();let mut capacity=0;unsafe{media.Lock(&mut pointer,Some(&mut capacity),None)?;}
                        let copy=if pointer.is_null() || capacity<(buffer.len() as u32){Err(Error::from(E_UNEXPECTED))}else{unsafe{std::ptr::copy_nonoverlapping(buffer.as_ptr(),pointer,buffer.len());}Ok(())};
                        let unlocked=unsafe{media.Unlock()};copy?;unlocked?;unsafe{media.SetCurrentLength(buffer.len() as u32)?;}
                    }
                    unsafe{sample.SetSampleTime(MFGetSystemTime())?;sample.SetSampleDuration(shared.profile.duration())?;
                        if let Some(token)=token {sample.SetUnknown(&MFSampleExtension_Token,&token.resolve()?)?;}
                        else {match sample.GetItemType(&MFSampleExtension_Token) {Ok(_)=>sample.DeleteItem(&MFSampleExtension_Token)?,Err(error) if error.code()==MF_E_ATTRIBUTENOTFOUND=>{},Err(error)=>return Err(error)}}}
                    if shared.state.load(Ordering::Acquire)!=1 || shared.stream_state.load(Ordering::Acquire)!=1 || shared.epoch.load(Ordering::Acquire)!=epoch {return Err(MF_E_MEDIA_SOURCE_WRONGSTATE.into());}
                    shared.stream_events.unk(MEMediaSample.0 as u32,&sample.cast()?)?;
                    deadline=Instant::now()+Duration::from_nanos(1_000_000_000/u64::from(shared.profile.fps()));Ok(())
                })();
                if let Err(error)=result {
                    shared.inflight.store(false,Ordering::Release);
                    if shared.state.load(Ordering::Acquire)==1 && !shared.close.load(Ordering::Acquire){let _=shared.stream_events.var(MEError.0 as u32,&GUID::zeroed(),error.code(),std::ptr::null());}
                }
            }
        }
    }
    if active{let _=mapping.consumer(false);}drop(mapping);drop(buffer);drop(receiver);drop(ready);
    // A downstream application may retain its last sample after Shutdown.
    // Keep this worker/allocator admission until all four real samples return;
    // native resource retirement has no fabricated finite join deadline.
    loop {match unsafe{allocator_status.GetFreeSampleCount()} {Ok(4)=>break,Ok(_)=>thread::sleep(Duration::from_millis(10)),Err(_)=>thread::sleep(Duration::from_millis(100))}}
    let _=unsafe{allocator.UninitializeSampleAllocator()};drop(allocator_status);drop(allocator);drop(shared);unsafe{CoUninitialize();}
}
#[implement(IClassFactory)]
struct Factory {_credit:ObjectCredit}
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self,outer:Option<&IUnknown>,iid:*const GUID,output:*mut *mut c_void)->Result<()> {
        if outer.is_some(){return Err(CLASS_E_NOAGGREGATION.into());}
        let activation:IMFActivate=Activate::new()?.into();unsafe{query(&activation.cast()?,iid,output)}
    }
    fn LockServer(&self,lock:BOOL)->Result<()> {if lock.as_bool(){LOCKS.fetch_add(1,Ordering::AcqRel);}else{LOCKS.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_sub(1)).map_err(|_|Error::from(E_UNEXPECTED))?;}Ok(())}
}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(module:isize,reason:u32,_reserved:*mut c_void)->i32 {if reason==1{MODULE.store(module,Ordering::Release);}1}
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(class:*const GUID,iid:*const GUID,output:*mut *mut c_void)->HRESULT {
    if class.is_null() || iid.is_null() || output.is_null(){return E_POINTER;}
    unsafe{*output=std::ptr::null_mut();}
    if unsafe{*class}!=ids::CLASS{return CLASS_E_CLASSNOTAVAILABLE;}
    let factory:IClassFactory=Factory {_credit:ObjectCredit::new()}.into();
    factory.cast::<IUnknown>().and_then(|value|unsafe{query(&value,iid,output)}).map_or_else(|error|error.code(),|()|S_OK)
}
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow()->HRESULT {
    let Ok(mut workers)=WORKERS.lock() else{return S_FALSE;};retire(&mut workers);
    if OBJECTS.load(Ordering::Acquire)==0 && LOCKS.load(Ordering::Acquire)==0 && workers.creating==0 && workers.handles.is_empty(){S_OK}else{S_FALSE}
}
#[unsafe(no_mangle)]
pub extern "system" fn DllRegisterServer()->HRESULT {register(false)}
#[unsafe(no_mangle)]
pub extern "system" fn DllUnregisterServer()->HRESULT {register(true)}
fn register(remove:bool)->HRESULT {
    let result=(||->std::io::Result<()> {let path=skylive_virtual_camera_protocol::installation::module_path(MODULE.load(Ordering::Acquire) as *mut c_void)?;
        if remove{skylive_virtual_camera_protocol::installation::unregister_module(&path)}else{skylive_virtual_camera_protocol::installation::register_module(&path)}})();
    result.map_or_else(|error|HRESULT::from_win32(error.raw_os_error().unwrap_or(5) as u32),|()|S_OK)
}
