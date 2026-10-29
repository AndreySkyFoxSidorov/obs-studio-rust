//! Bounded DirectShow video sample handoff for the future shared camera graph.
//!
//! A callback copies into preallocated slots, without retaining a driver-owned
//! sample. Pixel conversion and JPEG decoding run on the graph owner. The
//! DirectShow stream-relative timestamp is not a QPC device timestamp.
use super::{CameraError, CameraLimits, NativeVideoFrame};
use skylive_core::VideoCaptureMode;
use rtrb::{Consumer, Producer, PushError, RingBuffer};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, TryLockError, atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering}};

struct Slot { bytes: Box<[u8]> }
struct Packet { slot:Slot, len:usize, timestamp_100ns:i64, discontinuity:bool, clock_generation:Option<u64> }
struct CallbackState {free:Consumer<Slot>,ready:Producer<Packet>,parked:Option<Slot>}

/// A single callback producer. Concurrent callbacks are rejected rather than
/// waiting; the next accepted sample carries a discontinuity and drop count.
pub(crate) struct VideoGate {
    state:AtomicU8,
    callback:Mutex<CallbackState>,
    dropped:AtomicU64,
    discontinuity:AtomicBool,
    format_changed:AtomicBool,
    end_of_stream:AtomicBool,
    flush_drained:AtomicBool,
    flush_ended:AtomicBool,
    sample_cap:usize,
    clock:Arc<super::embedded_audio::directshow_clock::Binding>,
}
pub(crate) struct VideoReceiver {
    ready:Consumer<Packet>,
    free:Producer<Slot>,
    gate:Arc<VideoGate>,
    mode:VideoCaptureMode,
    pitch:i32,
    limits:CameraLimits,
    admitted_bytes:usize,
    avc:Option<super::avc::CameraAvcDecoder>,
    avc_ready:VecDeque<(NativeVideoFrame,Option<u64>)>,
}
impl VideoGate {
    const OPEN:u8=0;
    const FLUSHING:u8=1;
    const STOPPED:u8=2;
    fn reject(&self) {self.dropped.fetch_add(1,Ordering::Relaxed);self.discontinuity.store(true,Ordering::Release);}
    fn end_of_stream(&self) {self.end_of_stream.store(true,Ordering::Release);}
    fn new_segment(&self) {
        self.end_of_stream.store(false,Ordering::Release);
        self.discontinuity.store(true,Ordering::Release);
    }
    /// A dynamic media type invalidates all subsequent samples until the
    /// owner retires the graph and negotiates a new bounded pool.
    pub(crate) fn format_changed(&self) {self.format_changed.store(true,Ordering::Release);self.reject();}
    pub(crate) fn begin_flush(&self) {
        if self.state.load(Ordering::Acquire)==Self::OPEN {
            self.flush_ended.store(false,Ordering::Release);
            self.flush_drained.store(false,Ordering::Release);
            let _=self.state.compare_exchange(Self::OPEN,Self::FLUSHING,Ordering::AcqRel,Ordering::Acquire);
        }
    }
    pub(crate) fn stop(&self) {self.state.store(Self::STOPPED,Ordering::Release);}
    pub(crate) fn quiesced(&self)->bool {
        self.state.load(Ordering::Acquire)!=Self::OPEN && matches!(self.callback.try_lock(),Ok(_))
    }
    /// Only the graph owner may reopen after draining stale samples.
    pub(crate) fn end_flush(&self)->bool {
        if self.state.load(Ordering::Acquire)!=Self::FLUSHING {return false;}
        self.flush_ended.store(true,Ordering::Release);
        self.ensure_open();
        true
    }
    fn ensure_open(&self)->bool {
        self.state.load(Ordering::Acquire)==Self::OPEN || (self.flush_ended.load(Ordering::Acquire)
            && self.flush_drained.load(Ordering::Acquire) && self.quiesced()
            && self.state.compare_exchange(Self::FLUSHING,Self::OPEN,Ordering::AcqRel,Ordering::Acquire).is_ok())
    }
    /// Copy one immutable sample before the DirectShow callback returns.
    /// `timestamp_100ns` must be supplied by the sample, not fabricated from
    /// a host clock. A missing timestamp must be rejected by the caller.
    pub(crate) fn submit(&self,bytes:&[u8],timestamp_100ns:i64,discontinuity:bool,clock_generation:Option<u64>)->bool {
        if self.state.load(Ordering::Acquire)!=Self::OPEN || self.format_changed.load(Ordering::Acquire) {return false;}
        if bytes.is_empty() || bytes.len()>self.sample_cap {self.reject();return false;}
        let mut state=match self.callback.try_lock() {
            Ok(state)=>state,
            Err(TryLockError::WouldBlock)=>{self.reject();return false;},
            Err(TryLockError::Poisoned(_))=>{self.reject();return false;},
        };
        if self.state.load(Ordering::Acquire)!=Self::OPEN || self.format_changed.load(Ordering::Acquire) {return false;}
        let Some(mut slot)=state.parked.take().or_else(||state.free.pop().ok()) else {self.reject();return false;};
        slot.bytes[..bytes.len()].copy_from_slice(bytes);
        let packet=Packet{slot,len:bytes.len(),timestamp_100ns,
            discontinuity:discontinuity||self.discontinuity.swap(false,Ordering::AcqRel),
            clock_generation:clock_generation.filter(|g|self.clock.generation()==Some(*g))};
        match state.ready.push(packet) {
            Ok(())=>true,
            Err(PushError::Full(packet))=>{state.parked=Some(packet.slot);self.reject();false}
        }
    }
}

/// Rust DirectShow filter containing the video input pin. The internal shared
/// graph drains after flush and disconnects both pins before retirement.
pub(crate) mod filter {
    use super::{media_type::Owned,memory_input::{self,PinState},VideoGate};
    use super::super::embedded_audio::directshow_pin_enumerator;
    use std::sync::{Arc,Mutex};
    use windows::{core::{implement,GUID,PCWSTR,PWSTR},Win32::{Foundation::{E_FAIL,E_NOTIMPL,E_POINTER},
        Media::{IReferenceClock,DirectShow::{IBaseFilter,IBaseFilter_Impl,IEnumPins,IFilterGraph,IMediaFilter_Impl,
            IPin,FILTER_INFO,FILTER_STATE,VFW_E_NO_CLOCK,VFW_E_NOT_FOUND,VFW_E_WRONG_STATE}},System::Com::IPersist_Impl}};
    const CLASS_ID:GUID=GUID::from_u128(0x9b103cdf_0b79_4d41_8aa8_209acef3b945);
    const FILTER_NAME:&str="SkyLive Video Capture";
    const PIN_ID:&str="SkyLive Video";
    #[implement(IBaseFilter)]
    struct SinkFilter {pin:IPin,pin_state:Arc<PinState>,gate:Arc<VideoGate>,graph:Mutex<Option<IFilterGraph>>,clock:Arc<super::super::embedded_audio::directshow_clock::Binding>}
    impl IPersist_Impl for SinkFilter_Impl {fn GetClassID(&self)->windows::core::Result<GUID>{Ok(CLASS_ID)}}
    impl IMediaFilter_Impl for SinkFilter_Impl {
        fn Stop(&self)->windows::core::Result<()> {self.gate.begin_flush();
            if !self.gate.quiesced(){return Err(windows::core::Error::from_hresult(VFW_E_WRONG_STATE));}self.clock.stop()?;self.pin_state.set_state(0);Ok(())}
        fn Pause(&self)->windows::core::Result<()> {if self.pin_state.state()==2 {self.clock.stop()?;}if self.pin_state.state()==0 && !self.gate.ensure_open(){return Err(windows::core::Error::from_hresult(VFW_E_WRONG_STATE));}self.pin_state.set_state(1);Ok(())}
        fn Run(&self,start:i64)->windows::core::Result<()> {if self.pin_state.state()==0 && !self.gate.ensure_open(){return Err(windows::core::Error::from_hresult(VFW_E_WRONG_STATE));}self.clock.run(start)?;self.pin_state.set_state(2);Ok(())}
        fn GetState(&self,_timeout:u32)->windows::core::Result<FILTER_STATE>{Ok(FILTER_STATE(i32::from(self.pin_state.state())))}
        fn SetSyncSource(&self,clock:Option<&IReferenceClock>)->windows::core::Result<()> {self.clock.set(clock)}
        fn GetSyncSource(&self)->windows::core::Result<IReferenceClock>{self.clock.get()?.ok_or_else(||windows::core::Error::from_hresult(VFW_E_NO_CLOCK))}
    }
    impl IBaseFilter_Impl for SinkFilter_Impl {
        fn EnumPins(&self)->windows::core::Result<IEnumPins>{Ok(directshow_pin_enumerator::create(self.pin.clone()))}
        fn FindPin(&self,id:&PCWSTR)->windows::core::Result<IPin>{if id.is_null(){return Err(windows::core::Error::from_hresult(E_POINTER));}
            if unsafe {id.to_string()}?==PIN_ID{Ok(self.pin.clone())}else{Err(windows::core::Error::from_hresult(VFW_E_NOT_FOUND))}}
        fn QueryFilterInfo(&self,out:*mut FILTER_INFO)->windows::core::Result<()> {if out.is_null(){return Err(windows::core::Error::from_hresult(E_POINTER));}
            let graph=self.graph.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?.clone();let mut info=FILTER_INFO::default();
            info.pGraph=std::mem::ManuallyDrop::new(graph);for(dst,src)in info.achName.iter_mut().zip(FILTER_NAME.encode_utf16()){*dst=src;}unsafe {out.write(info);}Ok(())}
        fn JoinFilterGraph(&self,graph:Option<&IFilterGraph>,_name:&PCWSTR)->windows::core::Result<()> {*self.graph.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?=graph.cloned();
            if graph.is_none(){self.pin_state.detach_parent();}Ok(())}
        fn QueryVendorInfo(&self)->windows::core::Result<PWSTR>{Err(windows::core::Error::from_hresult(E_NOTIMPL))}
    }
    pub(crate) struct FilterHandle {pub filter:IBaseFilter,pin:IPin,pin_state:Arc<PinState>,gate:Arc<VideoGate>,pub clock:Arc<super::super::embedded_audio::directshow_clock::Binding>}
    impl FilterHandle {
        pub(crate) fn pin(&self)->&IPin{&self.pin}
        pub(crate) fn retire(&self)->bool {self.gate.stop();if !self.gate.quiesced(){return false;}self.pin_state.detach_parent();true}
    }
    impl Drop for FilterHandle {fn drop(&mut self){self.gate.stop();self.pin_state.detach_parent();}}
    pub(crate) fn create(gate:Arc<VideoGate>,media:Arc<Owned>)->Option<FilterHandle> {
        let (pin,pin_state)=memory_input::create(gate.clone(),media);
        let clock=gate.clock.clone();
        let filter:IBaseFilter=SinkFilter{pin:pin.clone(),pin_state:pin_state.clone(),gate:gate.clone(),graph:Mutex::new(None),clock:clock.clone()}.into();
        pin_state.attach_parent(filter.clone()).ok()?;Some(FilterHandle{filter,pin,pin_state,gate,clock})
    }
}

/// Allocate the complete sample pool before connecting the DirectShow pin.
/// The caller accounts `admitted_bytes` for the entire owner lifetime.
pub(crate) fn pool(mode:VideoCaptureMode,pitch:i32,sample_cap:usize,slots:usize,
    budget:usize,limits:CameraLimits,configuration:Option<Vec<u8>>)->Result<(Arc<VideoGate>,VideoReceiver),CameraError> {
    limits.validate()?;
    super::checked_frame_bytes(mode.width(),mode.height(),limits)?;
    if !(1..=8).contains(&slots) || sample_cap==0 || sample_cap>limits.max_raw_bytes
        || (pitch==0 && !matches!(mode.format(),skylive_core::VideoPixelFormat::Mjpeg|skylive_core::VideoPixelFormat::H264)) {
        return Err(CameraError::InvalidFrame("DirectShow video sample pool layout"));
    }
    let bytes=sample_cap.checked_mul(slots).ok_or(CameraError::BudgetExceeded("DirectShow video pool overflow"))?;
    if bytes>budget {return Err(CameraError::BudgetExceeded("DirectShow video pool bytes"));}
    let (mut free_write,free_read)=RingBuffer::new(slots);
    let (ready_write,ready_read)=RingBuffer::new(slots);
    for _ in 0..slots {
        let mut bytes=Vec::new();bytes.try_reserve_exact(sample_cap).map_err(|_|CameraError::BudgetExceeded("DirectShow video pool allocation"))?;
        if bytes.capacity()>sample_cap {return Err(CameraError::BudgetExceeded("DirectShow video pool allocation size"));}
        bytes.resize(sample_cap,0);
        free_write.push(Slot{bytes:bytes.into_boxed_slice()}).map_err(|_|CameraError::InvalidFrame("DirectShow video pool initialization"))?;
    }
    let gate=Arc::new(VideoGate{state:AtomicU8::new(VideoGate::OPEN),callback:Mutex::new(CallbackState{free:free_read,ready:ready_write,parked:None}),
        dropped:AtomicU64::new(0),discontinuity:AtomicBool::new(false),format_changed:AtomicBool::new(false),end_of_stream:AtomicBool::new(false),flush_drained:AtomicBool::new(false),flush_ended:AtomicBool::new(false),sample_cap,clock:Arc::new(super::embedded_audio::directshow_clock::Binding::default())});
    let avc=if mode.format()==skylive_core::VideoPixelFormat::H264 {
        Some(super::avc::CameraAvcDecoder::new(mode.width(),mode.height(),limits,configuration)?)
    } else {None};
    Ok((gate.clone(),VideoReceiver{ready:ready_read,free:free_write,gate,mode,pitch,limits,admitted_bytes:bytes,avc,avc_ready:VecDeque::new()}))
}
impl VideoReceiver {
    pub(crate) fn admitted_bytes(&self)->usize {self.admitted_bytes}
    /// Convert a complete packet on the graph owner, then recycle its slot even
    /// when conversion fails. No DirectShow COM buffer crosses this boundary.
    pub(crate) fn poll(&mut self)->Result<Option<(NativeVideoFrame,Option<u64>)>,CameraError> {
        if self.gate.format_changed.load(Ordering::Acquire) {return Err(CameraError::InvalidFrame("DirectShow video format changed"));}
        if self.gate.state.load(Ordering::Acquire)==VideoGate::FLUSHING {
            if self.gate.quiesced(){self.clear()?;self.gate.ensure_open();}
            return Ok(None);
        }
        if let Some(frame)=self.avc_ready.pop_front(){return Ok(Some(frame));}
        if let Some(avc)=&mut self.avc {
            self.avc_ready.extend(avc.drain()?);
            if let Some(frame)=self.avc_ready.pop_front(){return Ok(Some(frame));}
        }
        let Ok(packet)=self.ready.pop() else{
            return if self.gate.end_of_stream.load(Ordering::Acquire){Err(CameraError::DeviceLost)}else{Ok(None)};
        };
        if let Some(avc)=&mut self.avc {
            let dropped=self.gate.dropped.swap(0,Ordering::AcqRel);
            let result=avc.submit(&packet.slot.bytes[..packet.len],packet.timestamp_100ns,None,packet.clock_generation,
                packet.discontinuity || dropped!=0,dropped);
            if self.free.push(packet.slot).is_err(){return Err(CameraError::InvalidFrame("DirectShow video pool ownership"));}
            self.avc_ready.extend(result?);
            return Ok(self.avc_ready.pop_front());
        }
        let image=super::native::decode_directshow_buffer(&packet.slot.bytes[..packet.len],self.mode,self.pitch,self.limits);
        let frame=image.map(|image|(NativeVideoFrame{image,presentation_ns:i128::from(packet.timestamp_100ns)*100,
            device_qpc_ns:None,directshow_clock:None,discontinuity:packet.discontinuity,dropped_frames:self.gate.dropped.swap(0,Ordering::AcqRel)},packet.clock_generation));
        if self.free.push(packet.slot).is_err(){return Err(CameraError::InvalidFrame("DirectShow video pool ownership"));}
        frame.map(Some)
    }
    /// Drain and recycle queued samples before acknowledging a flush.
    pub(crate) fn clear(&mut self)->Result<(),CameraError> {
        if !self.gate.quiesced(){return Err(CameraError::Busy);}
        while let Ok(packet)=self.ready.pop() {
            if self.free.push(packet.slot).is_err(){return Err(CameraError::InvalidFrame("DirectShow video pool ownership"));}
        }
        self.avc_ready.clear();
        // Reinitialize the decoder on the first sample after flush so no old
        // reference pictures or PTS can cross the DirectShow graph boundary.
        if self.avc.is_some(){self.gate.discontinuity.store(true,Ordering::Release);}
        self.gate.flush_drained.store(true,Ordering::Release);
        Ok(())
    }
}

/// A bounded, owned copy of the exact video media type selected on the graph
/// owner. The future graph must still verify that this type describes `mode`
/// and the pool's pitch before connecting the pin.
pub(crate) mod media_type {
    use super::CameraError;
    use windows::{core::GUID,Win32::{Foundation::BOOL,Media::MediaFoundation::{AM_MEDIA_TYPE,MEDIATYPE_Video},System::Com::CoTaskMemAlloc}};
    pub(crate) struct Owned {
        major:GUID,subtype:GUID,fixed:BOOL,temporal:BOOL,sample_size:u32,format_type:GUID,format:Vec<u8>,
    }
    impl Owned {
        /// # Safety
        /// The media type and its format bytes must be valid for this call.
        pub(crate) unsafe fn copy_from(raw:*const AM_MEDIA_TYPE)->Result<Self,CameraError> {
            if raw.is_null(){return Err(CameraError::InvalidFrame("DirectShow video media type"));}
            let raw=unsafe {&*raw};
            if raw.majortype!=MEDIATYPE_Video || raw.cbFormat==0 || raw.cbFormat>65_536
                || raw.pbFormat.is_null() || raw.pUnk.is_some(){return Err(CameraError::InvalidFrame("unsupported DirectShow video media type"));}
            let bytes=unsafe {std::slice::from_raw_parts(raw.pbFormat,raw.cbFormat as usize)};
            let mut format=Vec::new();format.try_reserve_exact(bytes.len()).map_err(|_|CameraError::BudgetExceeded("DirectShow video media type allocation"))?;
            format.extend_from_slice(bytes);
            Ok(Self{major:raw.majortype,subtype:raw.subtype,fixed:raw.bFixedSizeSamples,
                temporal:raw.bTemporalCompression,sample_size:raw.lSampleSize,format_type:raw.formattype,format})
        }
        pub(crate) fn matches(&self,other:&Self)->bool {
            self.major==other.major && self.subtype==other.subtype && self.fixed==other.fixed
                && self.temporal==other.temporal && self.sample_size==other.sample_size
                && self.format_type==other.format_type && self.format==other.format
        }
        pub(crate) fn to_com(&self)->Result<AM_MEDIA_TYPE,CameraError> {
            let ptr=unsafe {CoTaskMemAlloc(self.format.len())}.cast::<u8>();
            if ptr.is_null(){return Err(CameraError::BudgetExceeded("DirectShow video media type COM allocation"));}
            unsafe {std::ptr::copy_nonoverlapping(self.format.as_ptr(),ptr,self.format.len());}
            Ok(AM_MEDIA_TYPE{majortype:self.major,subtype:self.subtype,bFixedSizeSamples:self.fixed,
                bTemporalCompression:self.temporal,lSampleSize:self.sample_size,formattype:self.format_type,
                pUnk:std::mem::ManuallyDrop::new(None),cbFormat:self.format.len() as u32,pbFormat:ptr})
        }
    }
}

mod media_enumerator {
    use super::media_type::Owned;
    use std::sync::{Arc,Mutex};
    use windows::{core::{implement,HRESULT},Win32::{Foundation::{E_FAIL,E_INVALIDARG,E_OUTOFMEMORY,E_POINTER,S_FALSE,S_OK},
        Media::{DirectShow::{IEnumMediaTypes,IEnumMediaTypes_Impl},MediaFoundation::AM_MEDIA_TYPE},System::Com::{CoTaskMemAlloc,CoTaskMemFree}}};
    #[implement(IEnumMediaTypes)]
    struct MediaEnum {media:Arc<Owned>,position:Mutex<bool>}
    impl IEnumMediaTypes_Impl for MediaEnum_Impl {
        fn Next(&self,count:u32,output:*mut *mut AM_MEDIA_TYPE,fetched:*mut u32)->HRESULT {
            if count!=1 && fetched.is_null(){return E_INVALIDARG;}if count!=0 && output.is_null(){return E_POINTER;}
            if !fetched.is_null(){unsafe {fetched.write(0);}}if count==0{return S_OK;}
            let Ok(mut used)=self.position.lock() else{return E_FAIL;};if *used{return S_FALSE;}
            let raw=unsafe {CoTaskMemAlloc(std::mem::size_of::<AM_MEDIA_TYPE>())}.cast::<AM_MEDIA_TYPE>();
            if raw.is_null(){return E_OUTOFMEMORY;}
            let copy=match self.media.to_com(){Ok(copy)=>copy,Err(_)=>{unsafe {CoTaskMemFree(Some(raw.cast()));}return E_OUTOFMEMORY;}};
            unsafe {raw.write(copy);output.write(raw);}*used=true;if !fetched.is_null(){unsafe {fetched.write(1);}}
            if count==1 {S_OK}else{S_FALSE}
        }
        fn Skip(&self,count:u32)->windows::core::Result<()> {let mut used=self.position.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            if count==0{return Ok(());}if *used{return Err(windows::core::Error::from_hresult(S_FALSE));}*used=true;
            if count==1{Ok(())}else{Err(windows::core::Error::from_hresult(S_FALSE))}}
        fn Reset(&self)->windows::core::Result<()> {*self.position.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?=false;Ok(())}
        fn Clone(&self)->windows::core::Result<IEnumMediaTypes> {let used=*self.position.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            Ok(MediaEnum{media:self.media.clone(),position:Mutex::new(used)}.into())}
    }
    pub(super) fn create(media:Arc<Owned>)->IEnumMediaTypes {MediaEnum{media,position:Mutex::new(false)}.into()}
}

/// DirectShow's video pin and memory-input callback. A containing filter and
/// graph owner must still establish format identity and teardown ordering.
mod memory_input {
    use super::{media_enumerator,media_type::Owned,VideoGate};
    use std::sync::{Arc,Mutex,atomic::{AtomicU8,Ordering}};
    use windows::{core::{implement,HRESULT,PWSTR},Win32::{
        Foundation::{BOOL,E_FAIL,E_INVALIDARG,E_NOTIMPL,E_OUTOFMEMORY,E_POINTER,S_FALSE,S_OK},
        Media::DirectShow::{ALLOCATOR_PROPERTIES,IBaseFilter,IEnumMediaTypes,IMediaSample,IMemAllocator,IMemInputPin,IMemInputPin_Impl,
            IPin,IPin_Impl,PIN_DIRECTION,PIN_INFO,PINDIR_INPUT,VFW_E_ALREADY_CONNECTED,VFW_E_INVALID_DIRECTION,
            VFW_E_NOT_CONNECTED,VFW_E_NOT_STOPPED,VFW_E_NO_ALLOCATOR,VFW_E_TYPE_NOT_ACCEPTED,VFW_E_WRONG_STATE},
        Media::MediaFoundation::AM_MEDIA_TYPE,
        System::Com::{CoTaskMemAlloc,CoTaskMemFree},
    }};

    pub(super) struct PinState {parent:Mutex<Option<IBaseFilter>>,peer:Mutex<Option<IPin>>,state:AtomicU8}
    impl PinState {
        pub(super) fn attach_parent(&self,parent:IBaseFilter)->windows::core::Result<()> {*self.parent.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?=Some(parent);Ok(())}
        pub(super) fn detach_parent(&self){if let Ok(mut parent)=self.parent.lock(){parent.take();}}
        pub(super) fn set_state(&self,state:u8){self.state.store(state,Ordering::Release);}
        pub(super) fn state(&self)->u8{self.state.load(Ordering::Acquire)}
    }
    #[implement(IPin,IMemInputPin)]
    struct VideoMemoryInput { gate:Arc<VideoGate>,expected:Arc<Owned>,pin:Arc<PinState> }
    impl IPin_Impl for VideoMemoryInput_Impl {
        fn Connect(&self,_peer:Option<&IPin>,_media:*const AM_MEDIA_TYPE)->windows::core::Result<()> {Err(windows::core::Error::from_hresult(VFW_E_INVALID_DIRECTION))}
        fn ReceiveConnection(&self,peer:Option<&IPin>,media:*const AM_MEDIA_TYPE)->windows::core::Result<()> {
            if self.pin.state()!=0{return Err(windows::core::Error::from_hresult(VFW_E_NOT_STOPPED));}
            let peer=peer.ok_or_else(||windows::core::Error::from_hresult(E_POINTER))?;
            let candidate=unsafe {Owned::copy_from(media)}.map_err(|_|windows::core::Error::from_hresult(VFW_E_TYPE_NOT_ACCEPTED))?;
            if !self.expected.matches(&candidate){return Err(windows::core::Error::from_hresult(VFW_E_TYPE_NOT_ACCEPTED));}
            let mut slot=self.pin.peer.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            if slot.is_some(){return Err(windows::core::Error::from_hresult(VFW_E_ALREADY_CONNECTED));}*slot=Some(peer.clone());Ok(())
        }
        fn Disconnect(&self)->windows::core::Result<()> {if self.pin.state()!=0{return Err(windows::core::Error::from_hresult(VFW_E_NOT_STOPPED));}
            let mut peer=self.pin.peer.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            if peer.take().is_none(){return Err(windows::core::Error::from_hresult(S_FALSE));}Ok(())}
        fn ConnectedTo(&self)->windows::core::Result<IPin>{self.pin.peer.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?.clone().ok_or_else(||windows::core::Error::from_hresult(VFW_E_NOT_CONNECTED))}
        fn ConnectionMediaType(&self,out:*mut AM_MEDIA_TYPE)->windows::core::Result<()> {
            if out.is_null(){return Err(windows::core::Error::from_hresult(E_POINTER));}
            if self.pin.peer.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?.is_none(){return Err(windows::core::Error::from_hresult(VFW_E_NOT_CONNECTED));}
            let copy=self.expected.to_com().map_err(|_|windows::core::Error::from_hresult(E_OUTOFMEMORY))?;unsafe {out.write(copy);}Ok(())
        }
        fn QueryPinInfo(&self,out:*mut PIN_INFO)->windows::core::Result<()> {if out.is_null(){return Err(windows::core::Error::from_hresult(E_POINTER));}
            let parent=self.pin.parent.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?.clone();let mut info=PIN_INFO::default();
            info.pFilter=std::mem::ManuallyDrop::new(parent);info.dir=PINDIR_INPUT;for (dst,src) in info.achName.iter_mut().zip("SkyLive video input".encode_utf16()){*dst=src;}unsafe {out.write(info);}Ok(())}
        fn QueryDirection(&self)->windows::core::Result<PIN_DIRECTION>{Ok(PINDIR_INPUT)}
        fn QueryId(&self)->windows::core::Result<PWSTR>{let name:Vec<u16>="SkyLive Video\0".encode_utf16().collect();let ptr=unsafe {CoTaskMemAlloc(name.len()*2)}.cast::<u16>();
            if ptr.is_null(){return Err(windows::core::Error::from_hresult(E_OUTOFMEMORY));}unsafe {std::ptr::copy_nonoverlapping(name.as_ptr(),ptr,name.len());}Ok(PWSTR(ptr))}
        fn QueryAccept(&self,media:*const AM_MEDIA_TYPE)->HRESULT {match unsafe {Owned::copy_from(media)} {Ok(value) if self.expected.matches(&value)=>S_OK,_=>S_FALSE}}
        fn EnumMediaTypes(&self)->windows::core::Result<IEnumMediaTypes>{Ok(media_enumerator::create(self.expected.clone()))}
        fn QueryInternalConnections(&self,_pins:*mut Option<IPin>,_count:*mut u32)->windows::core::Result<()>{Err(windows::core::Error::from_hresult(E_NOTIMPL))}
        fn EndOfStream(&self)->windows::core::Result<()>{self.gate.end_of_stream();Ok(())}
        fn BeginFlush(&self)->windows::core::Result<()>{self.gate.begin_flush();Ok(())}
        fn EndFlush(&self)->windows::core::Result<()>{if self.gate.end_flush(){Ok(())}else{Err(windows::core::Error::from_hresult(VFW_E_WRONG_STATE))}}
        fn NewSegment(&self,_start:i64,_stop:i64,_rate:f64)->windows::core::Result<()>{self.gate.new_segment();Ok(())}
    }
    impl IMemInputPin_Impl for VideoMemoryInput_Impl {
        fn GetAllocator(&self)->windows::core::Result<IMemAllocator> {
            Err(windows::core::Error::from_hresult(VFW_E_NO_ALLOCATOR))
        }
        fn NotifyAllocator(&self,allocator:Option<&IMemAllocator>,_read_only:BOOL)->windows::core::Result<()> {
            if allocator.is_none(){return Err(windows::core::Error::from_hresult(E_POINTER));} Ok(())
        }
        fn GetAllocatorRequirements(&self)->windows::core::Result<ALLOCATOR_PROPERTIES> {
            Err(windows::core::Error::from_hresult(E_NOTIMPL))
        }
        fn Receive(&self,sample:Option<&IMediaSample>)->windows::core::Result<()> {
            let sample=sample.ok_or_else(||windows::core::Error::from_hresult(E_POINTER))?;
            // Bind the sample to the clock generation before inspecting it.
            // A concurrent Run, Stop or SetSyncSource invalidates the stamp.
            let clock_generation=self.gate.clock.generation();
            // GetMediaType can return a new allocation at a format boundary.
            // Do not interpret that sample using the old layout.
            let media=unsafe {sample.GetMediaType()}?;
            if !media.is_null() {
                unsafe {let value=&mut *media;if !value.pbFormat.is_null(){CoTaskMemFree(Some(value.pbFormat.cast()));}
                    std::mem::ManuallyDrop::drop(&mut value.pUnk);CoTaskMemFree(Some(media.cast()));}
                self.gate.format_changed();return Err(windows::core::Error::from_hresult(VFW_E_TYPE_NOT_ACCEPTED));
            }
            let actual=unsafe {sample.GetActualDataLength()};let capacity=unsafe {sample.GetSize()};
            if actual<=0 || capacity<actual || actual as usize>self.gate.sample_cap {self.gate.reject();return Ok(());}
            let pointer=unsafe {sample.GetPointer()}?;
            if pointer.is_null(){self.gate.reject();return Ok(());}
            let (mut start,mut end)=(0i64,0i64);
            if unsafe {sample.GetTime(&mut start,&mut end)}.is_err(){self.gate.reject();return Ok(());}
            let bytes=unsafe {std::slice::from_raw_parts(pointer,actual as usize)};
            let discontinuity=unsafe {sample.IsDiscontinuity()}==S_OK;
            self.gate.submit(bytes,start,discontinuity,clock_generation);
            Ok(())
        }
        fn ReceiveMultiple(&self,samples:*const Option<IMediaSample>,count:i32)->windows::core::Result<i32> {
            if !(0..=256).contains(&count){return Err(windows::core::Error::from_hresult(E_INVALIDARG));}
            if count==0{return Ok(0);}
            if samples.is_null(){return Err(windows::core::Error::from_hresult(E_POINTER));}
            let samples=unsafe {std::slice::from_raw_parts(samples,count as usize)};
            for (index,sample) in samples.iter().enumerate() {
                if let Err(error)=self.Receive(sample.as_ref()) {return if index==0 {Err(error)}else{Ok(index as i32)};}
            }
            Ok(count)
        }
        fn ReceiveCanBlock(&self)->windows::core::Result<()> {Ok(())}
    }
    pub(super) fn create(gate:Arc<VideoGate>,expected:Arc<Owned>)->(IPin,Arc<PinState>) {
        let pin=Arc::new(PinState{parent:Mutex::new(None),peer:Mutex::new(None),state:AtomicU8::new(0)});
        (VideoMemoryInput{gate,expected,pin:pin.clone()}.into(),pin)
    }
}
