//! DirectShow camera audio PCM admission and allocation-free callback handoff.
//!
//! This module prepares the sample boundary only. It does not open a graph,
//! attach a capture pin, or claim that a DirectShow stream timestamp shares a
//! clock with WASAPI. The graph owner must establish identity, quiesce its
//! callback before retiring the pool, and provide an honest callback stamp.
use crate::audio_capture::{
    callback::{CallbackSender, CallbackStamp}, AudioCaptureError, AudioCaptureLimits,
    NativeAudioFormat, NativePacketFlags,
};

/// Runtime identity of one embedded-audio graph. A new source binding or
/// camera-owner epoch requires a new scope; an endpoint name alone is not
/// sufficient to identify the audio pin. This value does not itself grant
/// exclusive access to the physical device.
#[cfg(target_os = "windows")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EmbeddedAudioScope {
    pub session: u64,
    pub source: skylive_core::Id,
    pub camera_epoch: u64,
    pub target: skylive_core::VideoCaptureTarget,
}
#[cfg(target_os = "windows")]
impl EmbeddedAudioScope {
    pub(crate) fn new(session:u64,source:skylive_core::Id,camera_epoch:u64,
        binding:&skylive_core::VideoCaptureBinding)->Result<Self,AudioCaptureError> {
        if session==0 || source.get()==0 || camera_epoch==0
            || !matches!(binding.target(),skylive_core::VideoCaptureTarget::Windows{..}) {
            return Err(AudioCaptureError::Invalid("embedded camera audio scope"));
        }
        Ok(Self {session,source,camera_epoch,target:binding.target().clone()})
    }
}
#[cfg(target_os = "windows")]
use std::sync::{atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering}, Arc, Mutex, TryLockError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding { U8, Integer { bytes: usize, valid_bits: u16 }, Float32 }

/// Validated packed PCM layout negotiated outside the callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Layout { pub format: NativeAudioFormat, encoding: Encoding, block: usize }

impl Layout {
    /// Parse little-endian WAVEFORMATEX/WAVEFORMATEXTENSIBLE data. Reject
    /// compressed or ambiguous layouts rather than treating bytes as PCM.
    pub fn from_wave(bytes: &[u8], limits: AudioCaptureLimits) -> Result<Self, AudioCaptureError> {
        fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
            Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
        }
        fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        }
        let invalid = || AudioCaptureError::Invalid("DirectShow audio format buffer");
        let tag = u16_at(bytes, 0).ok_or_else(invalid)?;
        let channels = u16_at(bytes, 2).ok_or_else(invalid)?;
        let rate = u32_at(bytes, 4).ok_or_else(invalid)?;
        let average = u32_at(bytes, 8).ok_or_else(invalid)?;
        let block = usize::from(u16_at(bytes, 12).ok_or_else(invalid)?);
        let bits = u16_at(bytes, 14).ok_or_else(invalid)?;
        let (kind, valid, mask) = if tag == 0xfffe {
            let extra = usize::from(u16_at(bytes, 16).ok_or_else(invalid)?);
            if extra < 22 || bytes.len() < 18usize.checked_add(extra).ok_or_else(invalid)? { return Err(invalid()); }
            let valid = u16_at(bytes, 18).ok_or_else(invalid)?;
            let mask = u32_at(bytes, 20).ok_or_else(invalid)?;
            // KSDATAFORMAT_SUBTYPE_PCM / IEEE_FLOAT, serialized as a GUID.
            let subtype = bytes.get(24..40).ok_or_else(invalid)?;
            let tail = [0,0,0,0,0x10,0,0x80,0,0,0xaa,0,0x38,0x9b,0x71];
            if subtype[2..] != tail { return Err(AudioCaptureError::Unsupported("DirectShow extensible audio subtype")); }
            let kind = u16::from_le_bytes([subtype[0], subtype[1]]);
            (kind, valid, mask)
        } else { (tag, bits, 0) };
        let format = NativeAudioFormat { sample_rate: rate, channels, channel_mask: mask };
        format.validate(limits)?;
        let width = usize::from(bits / 8);
        if bits % 8 != 0 || width == 0 || block != width.checked_mul(usize::from(channels)).ok_or_else(invalid)?
            || u64::from(average) != u64::from(rate) * block as u64 { return Err(invalid()); }
        let encoding = match (kind, bits, valid) {
            (1, 8, 8) => Encoding::U8,
            (1, 16 | 24 | 32, n) if n > 0 && n <= bits => Encoding::Integer { bytes: width, valid_bits: n },
            (3, 32, 32) => Encoding::Float32,
            _ => return Err(AudioCaptureError::Unsupported("DirectShow audio PCM encoding")),
        };
        Ok(Self {format, encoding, block})
    }

    /// Admit a complete packet to a preallocated single-producer callback
    /// pool. Neither conversion nor copying allocates on the callback thread.
    pub fn submit(&self, sender: &mut CallbackSender, bytes: &[u8], stamp: CallbackStamp,
        flags: NativePacketFlags, limits: AudioCaptureLimits) -> bool {
        if bytes.is_empty() || bytes.len() % self.block != 0 || bytes.len() > limits.max_native_buffer_bytes {
            sender.reject(); return false;
        }
        let frames = bytes.len() / self.block;
        if frames > limits.max_frames { sender.reject(); return false; }
        if !flags.silent && matches!(self.encoding, Encoding::Float32)
            && bytes.chunks_exact(4).any(|s| !f32::from_le_bytes([s[0],s[1],s[2],s[3]]).is_finite()) {
            sender.reject(); return false;
        }
        let encoding = self.encoding;
        sender.submit(self.format, frames, stamp, flags, |output| {
            // Silent packets may carry undefined payload bytes. Honor the
            // native flag so downstream validation never sees nonzero PCM.
            if flags.silent {
                output.fill(0.0);
                return;
            }
            let width = self.block / usize::from(self.format.channels);
            for (out, sample) in output.iter_mut().zip(bytes.chunks_exact(width)) {
                *out = match encoding {
                    Encoding::U8 => (f32::from(sample[0]) - 128.0) / 128.0,
                    Encoding::Float32 => f32::from_le_bytes([sample[0],sample[1],sample[2],sample[3]]),
                    Encoding::Integer { bytes: n, valid_bits } => {
                        let raw = match n {
                            2 => i32::from(i16::from_le_bytes([sample[0],sample[1]])),
                            3 => i32::from_le_bytes([sample[0],sample[1],sample[2],if sample[2] & 0x80 != 0 {255} else {0}]),
                            4 => i32::from_le_bytes([sample[0],sample[1],sample[2],sample[3]]),
                            _ => unreachable!("validated PCM layout"),
                        };
                        let value = raw >> (n as u16 * 8 - valid_bits);
                        (f64::from(value) / ((1u64 << (valid_bits - 1)) as f64)) as f32
                    }
                };
            }
        })
    }

    /// Consume one DirectShow sample on its producer callback. The graph owner
    /// serializes calls, quiesces this callback before retiring the pool, and
    /// supplies a host-clock observation in the pool's `received_ticks` domain.
    /// DirectShow stream-relative sample times are deliberately not treated as
    /// QPC timestamps. A dynamic format change must be renegotiated by the
    /// owner before admitting any more samples.
    ///
    /// # Safety
    /// The sample and the buffer reported by its `IMediaSample2` properties
    /// must remain valid and immutable throughout this call, as required by the
    /// DirectShow receive callback contract.
    #[cfg(target_os = "windows")]
    pub(crate) unsafe fn submit_directshow_sample(
        &self,
        sender: &mut CallbackSender,
        sample: &windows::Win32::Media::DirectShow::IMediaSample2,
        received_ticks: u64,
        clock: &directshow_clock::Binding,
        limits: AudioCaptureLimits,
    ) -> DirectShowSampleOutcome {
        use windows::Win32::Media::DirectShow::{
            AM_SAMPLE2_PROPERTIES, AM_SAMPLE_DATADISCONTINUITY, AM_SAMPLE_TIMEVALID, AM_SAMPLE_TYPECHANGED,
        };
        let generation = clock.generation();
        let mut props = AM_SAMPLE2_PROPERTIES::default();
        let size = std::mem::size_of::<AM_SAMPLE2_PROPERTIES>();
        let bytes = unsafe { std::slice::from_raw_parts_mut((&mut props as *mut AM_SAMPLE2_PROPERTIES).cast::<u8>(), size) };
        if unsafe { sample.GetProperties(bytes) }.is_err() || props.cbData as usize != size {
            sender.reject();
            return DirectShowSampleOutcome::Rejected;
        }
        if props.dwSampleFlags & AM_SAMPLE_TYPECHANGED.0 as u32 != 0 || !props.pMediaType.is_null() {
            sender.reject();
            return DirectShowSampleOutcome::FormatChanged;
        }
        if props.dwStreamId != 0 || props.lActual <= 0 || props.cbBuffer < props.lActual
            || props.pbBuffer.is_null() || props.lActual as usize > limits.max_native_buffer_bytes {
            sender.reject();
            return DirectShowSampleOutcome::Rejected;
        }
        let payload = unsafe { std::slice::from_raw_parts(props.pbBuffer, props.lActual as usize) };
        let device = if props.dwSampleFlags & AM_SAMPLE_TIMEVALID.0 as u32 != 0 {
            crate::audio_capture::callback::CallbackTime::DirectShow100ns {value:props.tStart,generation:generation.filter(|g|clock.generation()==Some(*g))}
        } else { crate::audio_capture::callback::CallbackTime::Unavailable };
        let stamp = CallbackStamp { device, received_ticks };
        let flags = NativePacketFlags { discontinuity: props.dwSampleFlags & AM_SAMPLE_DATADISCONTINUITY.0 as u32 != 0,
            timestamp_error: true, ..Default::default() };
        if self.submit(sender, payload, stamp, flags, limits) {
            DirectShowSampleOutcome::Submitted
        } else {
            DirectShowSampleOutcome::Rejected
        }
    }
}

/// Format changes require owner-thread renegotiation, not interpretation with
/// the previous layout on the callback thread.
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DirectShowSampleOutcome { Submitted, Rejected, FormatChanged }

/// Owns the single-producer callback pool until the DirectShow graph has
/// stopped delivering samples. The graph owner changes the admission state;
/// Receive never waits for a concurrent callback or a flush operation.
/// This is not a COM sink or a running graph by itself.
#[cfg(target_os = "windows")]
pub(super) struct ReceiveGate {
    state: AtomicU8,
    sender: Mutex<CallbackSender>,
    layout: Layout,
    limits: AudioCaptureLimits,
    contended: AtomicU64,
    dropped: AtomicBool,
    pub clock: Arc<directshow_clock::Binding>,
}

#[cfg(target_os = "windows")]
impl ReceiveGate {
    const OPEN: u8 = 0;
    const FLUSHING: u8 = 1;
    const STOPPED: u8 = 2;

    pub fn new(layout: Layout, sender: CallbackSender, limits: AudioCaptureLimits) -> Self {
        Self {state: AtomicU8::new(Self::OPEN), sender: Mutex::new(sender), layout, limits,
            contended: AtomicU64::new(0), dropped: AtomicBool::new(false),clock:Arc::new(directshow_clock::Binding::default())}
    }

    /// Reject further submissions. Existing Receive calls may still be in
    /// progress; the owner must observe `quiesced()` before retiring state.
    pub fn begin_flush(&self) {
        let _ = self.state.compare_exchange(Self::OPEN, Self::FLUSHING, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Permanently close admission. Unlike flush, stop cannot be reopened.
    pub fn stop(&self) { self.state.store(Self::STOPPED, Ordering::Release); }

    /// Nonblocking observation: once stopped/flushing, no Receive can begin
    /// after the mutex is obtained because Receive rechecks state under it.
    pub fn quiesced(&self) -> bool {
        if self.state.load(Ordering::Acquire) == Self::OPEN { return false; }
        matches!(self.sender.try_lock(), Ok(_))
    }

    /// Reopen after the owner has observed callback quiescence and discarded
    /// any stale queued packets. Call only from the serialized graph owner.
    pub fn end_flush(&self) -> bool {
        if !self.quiesced() { return false; }
        self.state.compare_exchange(Self::FLUSHING, Self::OPEN, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    /// The graph's Pause/Run transition may occur without a preceding flush.
    fn ensure_open(&self)->bool {
        self.state.load(Ordering::Acquire)==Self::OPEN || self.end_flush()
    }
    fn finish_flush(&self)->bool {
        match self.state.load(Ordering::Acquire) {
            Self::STOPPED | Self::OPEN => true,
            _ => self.end_flush(),
        }
    }

    pub fn contention_drops(&self) -> u64 { self.contended.load(Ordering::Relaxed) }

    /// # Safety
    /// The sample and its buffer must remain valid and immutable for the
    /// entire callback. `received_ticks` must be from the owner's host clock;
    /// DirectShow stream-relative sample time is not assumed to be QPC.
    pub unsafe fn receive(&self, sample: &windows::Win32::Media::DirectShow::IMediaSample2,
        received_ticks: u64, format_changed: &AtomicBool) -> DirectShowSampleOutcome {
        if self.state.load(Ordering::Acquire) != Self::OPEN { return DirectShowSampleOutcome::Rejected; }
        let mut sender = match self.sender.try_lock() {
            Ok(sender) => sender,
            Err(TryLockError::WouldBlock) => { self.contended.fetch_add(1, Ordering::Relaxed); self.dropped.store(true, Ordering::Release); return DirectShowSampleOutcome::Rejected; }
            Err(TryLockError::Poisoned(_)) => return DirectShowSampleOutcome::Rejected,
        };
        if self.state.load(Ordering::Acquire) != Self::OPEN { return DirectShowSampleOutcome::Rejected; }
        if format_changed.load(Ordering::Acquire) { return DirectShowSampleOutcome::FormatChanged; }
        // Preserve a discontinuity for the next accepted packet without
        // waiting for the producer that currently owns the pool.
        if self.dropped.swap(false, Ordering::AcqRel) { sender.reject(); }
        let outcome = unsafe { self.layout.submit_directshow_sample(&mut sender, sample, received_ticks, &self.clock, self.limits) };
        if matches!(outcome, DirectShowSampleOutcome::FormatChanged) {
            format_changed.store(true, Ordering::Release);
        }
        outcome
    }

    /// Accept the older IMediaSample ABI when a driver does not expose
    /// IMediaSample2. Any dynamic media type is freed on this owner and
    /// requires renegotiation before further packets are admitted.
    ///
    /// # Safety
    /// `sample` and the buffer it reports must remain valid and immutable
    /// until this call returns, per the DirectShow Receive contract.
    pub unsafe fn receive_base(&self, sample:&windows::Win32::Media::DirectShow::IMediaSample,
        received_ticks:u64, format_changed:&AtomicBool)->DirectShowSampleOutcome {
        use windows::Win32::{Foundation::S_OK, System::Com::CoTaskMemFree};
        if self.state.load(Ordering::Acquire) != Self::OPEN { return DirectShowSampleOutcome::Rejected; }
        let mut sender=match self.sender.try_lock() {
            Ok(sender)=>sender,
            Err(TryLockError::WouldBlock)=>{self.contended.fetch_add(1,Ordering::Relaxed);self.dropped.store(true,Ordering::Release);return DirectShowSampleOutcome::Rejected;}
            Err(TryLockError::Poisoned(_))=>return DirectShowSampleOutcome::Rejected,
        };
        if self.state.load(Ordering::Acquire)!=Self::OPEN {return DirectShowSampleOutcome::Rejected;}
        if format_changed.load(Ordering::Acquire) {return DirectShowSampleOutcome::FormatChanged;}
        if self.dropped.swap(false,Ordering::AcqRel) {sender.reject();}
        let generation=self.clock.generation();
        let media_type=match unsafe {sample.GetMediaType()} {
            Ok(value)=>value,
            Err(_)=>{sender.reject();return DirectShowSampleOutcome::Rejected;}
        };
        if !media_type.is_null() {
            unsafe {
                let media=&mut *media_type;
                if !media.pbFormat.is_null(){CoTaskMemFree(Some(media.pbFormat.cast()));}
                std::mem::ManuallyDrop::drop(&mut media.pUnk);
                CoTaskMemFree(Some(media_type.cast()));
            }
            sender.reject();format_changed.store(true,Ordering::Release);
            return DirectShowSampleOutcome::FormatChanged;
        }
        let actual=unsafe {sample.GetActualDataLength()};
        let capacity=unsafe {sample.GetSize()};
        if actual<=0 || capacity<actual || actual as usize>self.limits.max_native_buffer_bytes {
            sender.reject();return DirectShowSampleOutcome::Rejected;
        }
        let ptr=match unsafe {sample.GetPointer()} {
            Ok(ptr) if !ptr.is_null()=>ptr,
            _=>{sender.reject();return DirectShowSampleOutcome::Rejected;}
        };
        let bytes=unsafe {std::slice::from_raw_parts(ptr,actual as usize)};
        let mut start=0i64;let mut end=0i64;
        let device=if unsafe {sample.GetTime(&mut start,&mut end)}.is_ok() {
            crate::audio_capture::callback::CallbackTime::DirectShow100ns {value:start,generation:generation.filter(|g|self.clock.generation()==Some(*g))}
        }else{crate::audio_capture::callback::CallbackTime::Unavailable};
        let stamp=CallbackStamp {device,received_ticks};
        let flags=NativePacketFlags {discontinuity:unsafe {sample.IsDiscontinuity()}==S_OK,
            timestamp_error:true,..Default::default()};
        if self.layout.submit(&mut sender,bytes,stamp,flags,self.limits) {DirectShowSampleOutcome::Submitted}
        else {DirectShowSampleOutcome::Rejected}
    }
}

/// The memory-input half of a DirectShow capture pin. This object is not yet
/// an `IPin` or an `IBaseFilter`, so it cannot be connected to a graph by
/// itself. A future graph owner must retain the returned interface, negotiate
/// `Layout` before streaming, and stop/flush the shared gate before release.
#[cfg(target_os = "windows")]
mod directshow_memory_input {
    use super::{directshow_media_enumerator, directshow_media_type::OwnedMediaType,DirectShowSampleOutcome, ReceiveGate};
    use std::sync::{atomic::{AtomicBool, AtomicU8, Ordering}, Arc, Mutex};
    use windows::{core::{implement, Interface, HRESULT, PWSTR}, Win32::{
        Foundation::{BOOL, E_FAIL, E_INVALIDARG, E_NOTIMPL, E_OUTOFMEMORY, E_POINTER, S_FALSE, S_OK},
        Media::DirectShow::{ALLOCATOR_PROPERTIES, IMediaSample, IMediaSample2, IMemAllocator,
            IMemInputPin, IMemInputPin_Impl, IBaseFilter, IEnumMediaTypes, IPin, IPin_Impl,
            PIN_DIRECTION, PIN_INFO, PINDIR_INPUT, VFW_E_ALREADY_CONNECTED, VFW_E_INVALID_DIRECTION,
            VFW_E_NOT_CONNECTED, VFW_E_NOT_STOPPED, VFW_E_NO_ALLOCATOR, VFW_E_TYPE_NOT_ACCEPTED,
            VFW_E_WRONG_STATE},
        Media::MediaFoundation::AM_MEDIA_TYPE,
        System::Com::CoTaskMemAlloc,
        System::Performance::QueryPerformanceCounter,
    }};

    struct Connection {peer:Option<IPin>,media:Option<Arc<OwnedMediaType>>}
    /// Shared with the future filter owner. The filter must clear `parent`
    /// when removed from its graph to break its filter↔pin ownership cycle.
    pub(super) struct PinState {
        connection:Mutex<Connection>,parent:Mutex<Option<IBaseFilter>>,state:AtomicU8,
    }
    impl PinState {
        pub fn attach_parent(&self,parent:IBaseFilter)->windows::core::Result<()> {
            *self.parent.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?=Some(parent);Ok(())
        }
        pub fn detach_parent(&self) {
            if let Ok(mut parent)=self.parent.lock(){parent.take();}
        }
        pub fn set_state(&self,state:u8){self.state.store(state,Ordering::Release);}
        pub fn state(&self)->u8{self.state.load(Ordering::Acquire)}
    }

    #[implement(IPin, IMemInputPin)]
    struct MemoryInput {
        gate: Arc<ReceiveGate>,
        frequency: u64,
        format_changed: Arc<AtomicBool>,
        pin:Arc<PinState>,
    }

    impl IPin_Impl for MemoryInput_Impl {
        fn Connect(&self,_peer:Option<&IPin>,_media:*const AM_MEDIA_TYPE)->windows::core::Result<()> {
            Err(windows::core::Error::from_hresult(VFW_E_INVALID_DIRECTION))
        }
        fn ReceiveConnection(&self,peer:Option<&IPin>,media:*const AM_MEDIA_TYPE)->windows::core::Result<()> {
            if self.pin.state.load(Ordering::Acquire)!=0{return Err(windows::core::Error::from_hresult(VFW_E_NOT_STOPPED));}
            let peer=peer.ok_or_else(||windows::core::Error::from_hresult(E_POINTER))?;
            let media=unsafe {OwnedMediaType::copy_from(media,self.gate.limits)}
                .map_err(|_|windows::core::Error::from_hresult(VFW_E_TYPE_NOT_ACCEPTED))?;
            if media.layout!=self.gate.layout{return Err(windows::core::Error::from_hresult(VFW_E_TYPE_NOT_ACCEPTED));}
            let mut connection=self.pin.connection.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            if connection.peer.is_some(){return Err(windows::core::Error::from_hresult(VFW_E_ALREADY_CONNECTED));}
            connection.peer=Some(peer.clone());connection.media=Some(Arc::new(media));Ok(())
        }
        fn Disconnect(&self)->windows::core::Result<()> {
            if self.pin.state.load(Ordering::Acquire)!=0{return Err(windows::core::Error::from_hresult(VFW_E_NOT_STOPPED));}
            let mut connection=self.pin.connection.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            if connection.peer.take().is_none(){return Err(windows::core::Error::from_hresult(S_FALSE));}
            connection.media=None;Ok(())
        }
        fn ConnectedTo(&self)->windows::core::Result<IPin> {
            self.pin.connection.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?.peer.clone()
                .ok_or_else(||windows::core::Error::from_hresult(VFW_E_NOT_CONNECTED))
        }
        fn ConnectionMediaType(&self,out:*mut AM_MEDIA_TYPE)->windows::core::Result<()> {
            if out.is_null(){return Err(windows::core::Error::from_hresult(E_POINTER));}
            let connection=self.pin.connection.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            let media=connection.media.as_ref().ok_or_else(||windows::core::Error::from_hresult(VFW_E_NOT_CONNECTED))?;
            let copy=media.to_com().map_err(|_|windows::core::Error::from_hresult(E_OUTOFMEMORY))?;
            unsafe {out.write(copy);}Ok(())
        }
        fn QueryPinInfo(&self,out:*mut PIN_INFO)->windows::core::Result<()> {
            if out.is_null(){return Err(windows::core::Error::from_hresult(E_POINTER));}
            let parent=self.pin.parent.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?.clone();
            let mut info=PIN_INFO::default();info.pFilter=std::mem::ManuallyDrop::new(parent);info.dir=PINDIR_INPUT;
            for (dst,src) in info.achName.iter_mut().zip("SkyLive audio input".encode_utf16()){*dst=src;}
            unsafe {out.write(info);}Ok(())
        }
        fn QueryDirection(&self)->windows::core::Result<PIN_DIRECTION>{Ok(PINDIR_INPUT)}
        fn QueryId(&self)->windows::core::Result<PWSTR> {
            const NAME:&[u16]=&[83,107,121,76,105,118,101,32,65,117,100,105,111,0];
            let ptr=unsafe {CoTaskMemAlloc(std::mem::size_of_val(NAME))}.cast::<u16>();
            if ptr.is_null(){return Err(windows::core::Error::from_hresult(E_OUTOFMEMORY));}
            unsafe {std::ptr::copy_nonoverlapping(NAME.as_ptr(),ptr,NAME.len());}Ok(PWSTR(ptr))
        }
        fn QueryAccept(&self,media:*const AM_MEDIA_TYPE)->HRESULT {
            match unsafe {OwnedMediaType::copy_from(media,self.gate.limits)} {
                Ok(media) if media.layout==self.gate.layout=>S_OK,_=>S_FALSE,
            }
        }
        fn EnumMediaTypes(&self)->windows::core::Result<IEnumMediaTypes> {
            let media=self.pin.connection.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?.media.clone();
            Ok(directshow_media_enumerator::create(media))
        }
        fn QueryInternalConnections(&self,_pins:*mut Option<IPin>,_count:*mut u32)->windows::core::Result<()> {
            Err(windows::core::Error::from_hresult(E_NOTIMPL))
        }
        fn EndOfStream(&self)->windows::core::Result<()>{Ok(())}
        fn BeginFlush(&self)->windows::core::Result<()>{self.gate.begin_flush();Ok(())}
        fn EndFlush(&self)->windows::core::Result<()> {
            if self.gate.finish_flush(){Ok(())}else{Err(windows::core::Error::from_hresult(VFW_E_WRONG_STATE))}
        }
        fn NewSegment(&self,_start:i64,_stop:i64,_rate:f64)->windows::core::Result<()>{Ok(())}
    }

    impl IMemInputPin_Impl for MemoryInput_Impl {
        fn GetAllocator(&self) -> windows::core::Result<IMemAllocator> {
            Err(windows::core::Error::from_hresult(VFW_E_NO_ALLOCATOR))
        }
        fn NotifyAllocator(&self, allocator: Option<&IMemAllocator>, _read_only: BOOL) -> windows::core::Result<()> {
            if allocator.is_none() { return Err(windows::core::Error::from_hresult(E_POINTER)); }
            Ok(())
        }
        fn GetAllocatorRequirements(&self) -> windows::core::Result<ALLOCATOR_PROPERTIES> {
            Err(windows::core::Error::from_hresult(E_NOTIMPL))
        }
        fn Receive(&self, sample: Option<&IMediaSample>) -> windows::core::Result<()> {
            if self.format_changed.load(Ordering::Acquire) {
                return Err(windows::core::Error::from_hresult(VFW_E_TYPE_NOT_ACCEPTED));
            }
            let sample = sample.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
            let mut ticks = 0i64;
            unsafe { QueryPerformanceCounter(&mut ticks) }?;
            let ticks = u64::try_from(ticks).map_err(|_| windows::core::Error::from_hresult(E_INVALIDARG))?;
            let ns = u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(self.frequency))
                .map_err(|_| windows::core::Error::from_hresult(E_INVALIDARG))?;
            let outcome=match sample.cast::<IMediaSample2>() {
                Ok(extended)=>unsafe {self.gate.receive(&extended,ns,&self.format_changed)},
                Err(_)=>unsafe {self.gate.receive_base(sample,ns,&self.format_changed)},
            };
            match outcome {
                DirectShowSampleOutcome::Submitted | DirectShowSampleOutcome::Rejected => Ok(()),
                DirectShowSampleOutcome::FormatChanged => {
                    Err(windows::core::Error::from_hresult(VFW_E_TYPE_NOT_ACCEPTED))
                }
            }
        }
        fn ReceiveMultiple(&self, samples: *const Option<IMediaSample>, count: i32) -> windows::core::Result<i32> {
            if !(0..=256).contains(&count) { return Err(windows::core::Error::from_hresult(E_INVALIDARG)); }
            if count == 0 { return Ok(0); }
            if samples.is_null() { return Err(windows::core::Error::from_hresult(E_POINTER)); }
            let samples = unsafe { std::slice::from_raw_parts(samples, count as usize) };
            for (index, sample) in samples.iter().enumerate() {
                if let Err(error) = self.Receive(sample.as_ref()) {
                    // The ABI exposes a processed count only for successful
                    // returns. Preserve a completed prefix rather than
                    // reporting a failure that could cause it to be retried.
                    return if index == 0 { Err(error) } else { Ok(index as i32) };
                }
            }
            Ok(count)
        }
        fn ReceiveCanBlock(&self) -> windows::core::Result<()> {
            // Conservatively return S_OK: COM and the upstream driver may
            // block even though the callback pool itself uses try_lock.
            Ok(())
        }
    }

    pub(super) fn create(gate: Arc<ReceiveGate>, frequency: u64,
        format_changed: Arc<AtomicBool>) -> Option<(IPin,Arc<PinState>)> {
        if frequency == 0 { return None; }
        let pin=Arc::new(PinState {connection:Mutex::new(Connection {peer:None,media:None}),
            parent:Mutex::new(None),state:AtomicU8::new(0)});
        Some((MemoryInput {gate, frequency, format_changed,pin:pin.clone()}.into(),pin))
    }
}

/// One-pin DirectShow enumerator for a future sink filter. COM owns a clone
/// of the pin for the lifetime of each enumeration, including cloned cursors.
#[cfg(target_os = "windows")]
pub(super) mod directshow_pin_enumerator {
    use std::sync::Mutex;
    use windows::{core::{implement, HRESULT}, Win32::{Foundation::{E_INVALIDARG, E_POINTER, E_FAIL, S_FALSE, S_OK},
        Media::DirectShow::{IEnumPins, IEnumPins_Impl, IPin}}};

    #[implement(IEnumPins)]
    struct SinglePinEnum { pin: IPin, position: Mutex<bool> }

    impl IEnumPins_Impl for SinglePinEnum_Impl {
        fn Next(&self, count:u32, output:*mut Option<IPin>, fetched:*mut u32)->HRESULT {
            if count != 1 && fetched.is_null() { return E_INVALIDARG; }
            if count != 0 && output.is_null() { return E_POINTER; }
            if !fetched.is_null() { unsafe { fetched.write(0); } }
            if count == 0 { return S_OK; }
            let Ok(mut used)=self.position.lock() else { return E_FAIL; };
            if *used { return S_FALSE; }
            unsafe { output.write(Some(self.pin.clone())); }
            *used=true;
            if !fetched.is_null() { unsafe { fetched.write(1); } }
            if count == 1 {S_OK} else {S_FALSE}
        }
        fn Skip(&self,count:u32)->windows::core::Result<()> {
            let mut used=self.position.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            if count==0 {return Ok(());}
            if *used {return Err(windows::core::Error::from_hresult(S_FALSE));}
            *used=true;
            if count==1 {Ok(())} else {Err(windows::core::Error::from_hresult(S_FALSE))}
        }
        fn Reset(&self)->windows::core::Result<()> {
            *self.position.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?=false;Ok(())
        }
        fn Clone(&self)->windows::core::Result<IEnumPins> {
            let used=*self.position.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            Ok(SinglePinEnum {pin:self.pin.clone(),position:Mutex::new(used)}.into())
        }
    }

    pub(in crate::camera_capture) fn create(pin:IPin)->IEnumPins {
        SinglePinEnum {pin,position:Mutex::new(false)}.into()
    }
}

/// A negotiated AM_MEDIA_TYPE with Rust-owned format bytes. DirectShow hands
/// out format buffers with COM allocation ownership; a sink must never retain
/// the borrowed pointer passed to ReceiveConnection. Unknown pUnk payloads are
/// rejected until their lifetime has a supported contract.
#[cfg(target_os = "windows")]
mod directshow_media_type {
    use super::{AudioCaptureError, AudioCaptureLimits, Encoding, Layout};
    use windows::{core::GUID, Win32::{Foundation::BOOL,
        Media::MediaFoundation::{AM_MEDIA_TYPE, FORMAT_WaveFormatEx, MEDIATYPE_Audio, MEDIASUBTYPE_PCM, MEDIASUBTYPE_IEEE_FLOAT},
        System::Com::CoTaskMemAlloc}};

    pub(super) struct OwnedMediaType {
        major:GUID, subtype:GUID, fixed:BOOL, temporal:BOOL, sample_size:u32,
        format_type:GUID, format:Vec<u8>, pub layout:Layout,
    }
    impl OwnedMediaType {
        /// # Safety
        /// `raw` and its format buffer must remain readable for this call.
        pub unsafe fn copy_from(raw:*const AM_MEDIA_TYPE,limits:AudioCaptureLimits)
            ->Result<Self,AudioCaptureError> {
            if raw.is_null(){return Err(AudioCaptureError::Invalid("DirectShow audio media type"));}
            let raw=unsafe {&*raw};
            if raw.majortype!=MEDIATYPE_Audio || raw.formattype!=FORMAT_WaveFormatEx
                || raw.cbFormat<16 || raw.cbFormat>65_536 || raw.pbFormat.is_null()
                || raw.pUnk.is_some() {
                return Err(AudioCaptureError::Unsupported("DirectShow audio media type"));
            }
            let bytes=unsafe {std::slice::from_raw_parts(raw.pbFormat,raw.cbFormat as usize)};
            let layout=Layout::from_wave(bytes,limits)?;
            let subtype=match layout.encoding {Encoding::Float32=>MEDIASUBTYPE_IEEE_FLOAT,_=>MEDIASUBTYPE_PCM};
            if raw.subtype!=subtype{return Err(AudioCaptureError::Unsupported("DirectShow audio subtype and PCM format disagree"));}
            let mut format=Vec::new();
            format.try_reserve_exact(bytes.len()).map_err(|_|AudioCaptureError::Limit("DirectShow audio media type allocation"))?;
            format.extend_from_slice(bytes);
            Ok(Self {major:raw.majortype,subtype:raw.subtype,fixed:raw.bFixedSizeSamples,
                temporal:raw.bTemporalCompression,sample_size:raw.lSampleSize,
                format_type:raw.formattype,format,layout})
        }
        /// Allocate a COM-owned copy for IPin::ConnectionMediaType. The caller
        /// owns the resulting pbFormat and must release it using CoTaskMemFree.
        pub fn to_com(&self)->Result<AM_MEDIA_TYPE,AudioCaptureError> {
            let ptr=unsafe {CoTaskMemAlloc(self.format.len())}.cast::<u8>();
            if ptr.is_null(){return Err(AudioCaptureError::Limit("DirectShow media type COM allocation"));}
            unsafe {std::ptr::copy_nonoverlapping(self.format.as_ptr(),ptr,self.format.len());}
            Ok(AM_MEDIA_TYPE {majortype:self.major,subtype:self.subtype,bFixedSizeSamples:self.fixed,
                bTemporalCompression:self.temporal,lSampleSize:self.sample_size,formattype:self.format_type,
                pUnk:std::mem::ManuallyDrop::new(None),cbFormat:self.format.len() as u32,pbFormat:ptr})
        }
    }
}

/// Enumerates zero or one already-negotiated format. Allocation returned to
/// DirectShow uses CoTaskMem ownership, including the nested format buffer.
#[cfg(target_os = "windows")]
mod directshow_media_enumerator {
    use super::directshow_media_type::OwnedMediaType;
    use std::sync::{Arc,Mutex};
    use windows::{core::{implement,HRESULT},Win32::{Foundation::{E_FAIL,E_INVALIDARG,E_OUTOFMEMORY,E_POINTER,S_FALSE,S_OK},
        Media::{DirectShow::{IEnumMediaTypes,IEnumMediaTypes_Impl},MediaFoundation::AM_MEDIA_TYPE},
        System::Com::{CoTaskMemAlloc,CoTaskMemFree}}};
    #[implement(IEnumMediaTypes)]
    struct MediaEnum {media:Option<Arc<OwnedMediaType>>,position:Mutex<bool>}
    impl IEnumMediaTypes_Impl for MediaEnum_Impl {
        fn Next(&self,count:u32,output:*mut *mut AM_MEDIA_TYPE,fetched:*mut u32)->HRESULT {
            if count!=1 && fetched.is_null(){return E_INVALIDARG;}
            if count!=0 && output.is_null(){return E_POINTER;}
            if !fetched.is_null(){unsafe {fetched.write(0);}}
            if count==0{return S_OK;}
            let Ok(mut used)=self.position.lock() else{return E_FAIL;};
            let Some(media)=self.media.as_ref().filter(|_|!*used) else{return S_FALSE;};
            let raw=unsafe {CoTaskMemAlloc(std::mem::size_of::<AM_MEDIA_TYPE>())}.cast::<AM_MEDIA_TYPE>();
            if raw.is_null(){return E_OUTOFMEMORY;}
            let copy=match media.to_com(){Ok(copy)=>copy,Err(_)=>{unsafe {CoTaskMemFree(Some(raw.cast()));}return E_OUTOFMEMORY;}};
            unsafe {raw.write(copy);output.write(raw);}
            *used=true;
            if !fetched.is_null(){unsafe {fetched.write(1);}}
            if count==1 {S_OK}else{S_FALSE}
        }
        fn Skip(&self,count:u32)->windows::core::Result<()> {
            let mut used=self.position.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            if count==0{return Ok(());}
            if self.media.is_none()||*used{return Err(windows::core::Error::from_hresult(S_FALSE));}
            *used=true;if count==1{Ok(())}else{Err(windows::core::Error::from_hresult(S_FALSE))}
        }
        fn Reset(&self)->windows::core::Result<()> {
            *self.position.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?=false;Ok(())
        }
        fn Clone(&self)->windows::core::Result<IEnumMediaTypes> {
            let used=*self.position.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            Ok(MediaEnum {media:self.media.clone(),position:Mutex::new(used)}.into())
        }
    }
    pub(super) fn create(media:Option<Arc<OwnedMediaType>>)->IEnumMediaTypes {
        MediaEnum {media,position:Mutex::new(false)}.into()
    }
}

/// A DirectShow sink filter with one PCM input pin. Construction does not
/// create or run a graph. The graph owner must stop streaming, wait for
/// callback quiescence, disconnect pins, remove the filter, and call `retire`.
#[cfg(target_os = "windows")]
mod directshow_filter {
    use super::{directshow_memory_input::{self,PinState},directshow_pin_enumerator,ReceiveGate};
    use std::sync::{atomic::AtomicBool,Arc,Mutex};
    use windows::{core::{implement,GUID,PCWSTR,PWSTR},Win32::{
        Foundation::{E_FAIL,E_NOTIMPL,E_POINTER},
        Media::{IReferenceClock,DirectShow::{IBaseFilter,IBaseFilter_Impl,IEnumPins,IFilterGraph,IMediaFilter_Impl,
            IPin,FILTER_INFO,FILTER_STATE,VFW_E_NO_CLOCK,VFW_E_NOT_FOUND,VFW_E_WRONG_STATE}},
        System::Com::IPersist_Impl,
    }};

    const CLASS_ID:GUID=GUID::from_u128(0x8e544a92_3a83_4d11_9a87_251ef0a79833);
    const FILTER_NAME:&str="SkyLive Audio Capture";
    const PIN_ID:&str="SkyLive Audio";

    #[implement(IBaseFilter)]
    struct SinkFilter {
        pin:IPin,pin_state:Arc<PinState>,gate:Arc<ReceiveGate>,
        graph:Mutex<Option<IFilterGraph>>,clock:Arc<super::directshow_clock::Binding>,
    }
    impl IPersist_Impl for SinkFilter_Impl {
        fn GetClassID(&self)->windows::core::Result<GUID>{Ok(CLASS_ID)}
    }
    impl IMediaFilter_Impl for SinkFilter_Impl {
        fn Stop(&self)->windows::core::Result<()> {
            self.gate.begin_flush();
            if !self.gate.quiesced(){return Err(windows::core::Error::from_hresult(VFW_E_WRONG_STATE));}
            self.clock.stop()?;self.pin_state.set_state(0);Ok(())
        }
        fn Pause(&self)->windows::core::Result<()> {
            if self.pin_state.state()==2 {self.clock.stop()?;}
            if self.pin_state.state()==0 && !self.gate.ensure_open(){return Err(windows::core::Error::from_hresult(VFW_E_WRONG_STATE));}
            self.pin_state.set_state(1);Ok(())
        }
        fn Run(&self,start:i64)->windows::core::Result<()> {
            if self.pin_state.state()==0 && !self.gate.ensure_open(){return Err(windows::core::Error::from_hresult(VFW_E_WRONG_STATE));}
            self.clock.run(start)?;self.pin_state.set_state(2);Ok(())
        }
        fn GetState(&self,_timeout:u32)->windows::core::Result<FILTER_STATE>{Ok(FILTER_STATE(i32::from(self.pin_state.state())))}
        fn SetSyncSource(&self,clock:Option<&IReferenceClock>)->windows::core::Result<()> {
            self.clock.set(clock)
        }
        fn GetSyncSource(&self)->windows::core::Result<IReferenceClock> {
            self.clock.get()?
                .ok_or_else(||windows::core::Error::from_hresult(VFW_E_NO_CLOCK))
        }
    }
    impl IBaseFilter_Impl for SinkFilter_Impl {
        fn EnumPins(&self)->windows::core::Result<IEnumPins>{Ok(directshow_pin_enumerator::create(self.pin.clone()))}
        fn FindPin(&self,id:&PCWSTR)->windows::core::Result<IPin> {
            if id.is_null(){return Err(windows::core::Error::from_hresult(E_POINTER));}
            let name=unsafe {id.to_string()}?;
            if name==PIN_ID {Ok(self.pin.clone())}else{Err(windows::core::Error::from_hresult(VFW_E_NOT_FOUND))}
        }
        fn QueryFilterInfo(&self,out:*mut FILTER_INFO)->windows::core::Result<()> {
            if out.is_null(){return Err(windows::core::Error::from_hresult(E_POINTER));}
            let graph=self.graph.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?.clone();
            let mut info=FILTER_INFO::default();info.pGraph=std::mem::ManuallyDrop::new(graph);
            for (dst,src) in info.achName.iter_mut().zip(FILTER_NAME.encode_utf16()){*dst=src;}
            unsafe {out.write(info);}Ok(())
        }
        fn JoinFilterGraph(&self,graph:Option<&IFilterGraph>,_name:&PCWSTR)->windows::core::Result<()> {
            *self.graph.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?=graph.cloned();
            if graph.is_none(){self.pin_state.detach_parent();}
            Ok(())
        }
        fn QueryVendorInfo(&self)->windows::core::Result<PWSTR>{Err(windows::core::Error::from_hresult(E_NOTIMPL))}
    }

    pub(super) struct FilterHandle {pub filter:IBaseFilter,pin:IPin,pin_state:Arc<PinState>,gate:Arc<ReceiveGate>,pub clock:Arc<super::directshow_clock::Binding>}
    impl FilterHandle {
        pub fn pin(&self)->&IPin{&self.pin}
        /// Must follow graph Stop, pin disconnect and graph RemoveFilter.
        /// Returns false while a callback still owns the PCM pool.
        pub fn retire(&self)->bool {
            self.gate.stop();
            if !self.gate.quiesced(){return false;}
            self.pin_state.detach_parent();true
        }
    }
    impl Drop for FilterHandle {
        fn drop(&mut self){
            // A failed graph construction must not leave a filter↔pin cycle.
            // The pin and each callback retain their own Arc<ReceiveGate>, so
            // closing admission does not free an in-flight callback buffer.
            self.gate.stop();self.pin_state.detach_parent();
        }
    }
    pub(super) fn create(gate:Arc<ReceiveGate>,frequency:u64,format_changed:Arc<AtomicBool>)->Option<FilterHandle> {
        let (pin,pin_state)=directshow_memory_input::create(gate.clone(),frequency,format_changed)?;
        let clock=gate.clock.clone();
        let filter:IBaseFilter=SinkFilter {pin:pin.clone(),pin_state:pin_state.clone(),gate:gate.clone(),
            graph:Mutex::new(None),clock:clock.clone()}.into();
        pin_state.attach_parent(filter.clone()).ok()?;
        Some(FilterHandle {filter,pin,pin_state,gate,clock})
    }
}

// Kept separately for each sink so graph clock changes invalidate outstanding observations.
#[cfg(target_os = "windows")]
pub(crate) mod directshow_clock {
    use std::sync::{Mutex,atomic::{AtomicU64,Ordering}};
    use windows::{Win32::{Foundation::E_FAIL,Media::IReferenceClock},core::Interface};
    #[derive(Default)]
    pub(crate) struct Binding(Mutex<State>,AtomicU64);
    #[derive(Default)]
    struct State { clock: Option<IReferenceClock>, start: Option<i64>, generation:u64 }
    impl Binding {
        pub(crate) fn set(&self,clock:Option<&IReferenceClock>)->windows::core::Result<()> {
            let mut s=self.0.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            s.clock=clock.cloned();s.start=None;s.generation=s.generation.wrapping_add(1);self.1.store(0,Ordering::Release);Ok(())
        }
        pub(crate) fn get(&self)->windows::core::Result<Option<IReferenceClock>> {
            Ok(self.0.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?.clock.clone())
        }
        pub(crate) fn run(&self,start:i64)->windows::core::Result<()> {
            let mut s=self.0.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            s.start=Some(start);s.generation=s.generation.wrapping_add(1);self.1.store(if s.clock.is_some(){s.generation}else{0},Ordering::Release);Ok(())
        }
        pub(crate) fn stop(&self)->windows::core::Result<()> {
            let mut s=self.0.lock().map_err(|_|windows::core::Error::from_hresult(E_FAIL))?;
            s.start=None;s.generation=s.generation.wrapping_add(1);self.1.store(0,Ordering::Release);Ok(())
        }
        pub(crate) fn generation(&self)->Option<u64> {let n=self.1.load(Ordering::Acquire);(n!=0).then_some(n)}
        pub(crate) fn snapshot(&self)->Option<(IReferenceClock,i64,u64)> {
            let s=self.0.lock().ok()?;Some((s.clock.clone()?,s.start?,s.generation))
        }
    }
    pub(crate) fn same(a:&(IReferenceClock,i64,u64),b:&(IReferenceClock,i64,u64))->bool {
        a.0.as_raw()==b.0.as_raw() && a.1==b.1 && a.2==b.2
    }
}

/// Private graph owner for an embedded PCM capture pin. The caller must hold
/// exclusive authority over this physical camera and an initialized MTA for
/// the owner's entire lifetime; this type cannot be moved to another thread.
/// It is not yet connected to CameraOwner or AudioProgram.
#[cfg(target_os = "windows")]
mod directshow_graph {
    use super::{directshow_filter::{self,FilterHandle},directshow_media_type::OwnedMediaType,
        AudioCaptureError,AudioCaptureLimits,EmbeddedAudioScope,ReceiveGate};
    use crate::audio_capture::{NativeAudioStream,NativePcmBlock};
    use crate::camera_capture::{CameraError,CameraLimits,NativeCameraStream,NativeVideoFrame};
    use crate::camera_capture::embedded_video::{self,VideoGate,VideoReceiver};
    use crate::audio_capture::callback::{self,CallbackPacket,CallbackReceiver};
    use skylive_core::{VideoCaptureBinding,VideoCaptureTarget,VideoCaptureMode};
    use std::{collections::VecDeque,marker::PhantomData,rc::Rc,sync::{atomic::{AtomicBool,Ordering},Arc}};
    use windows::{core::{Interface,GUID,w},Win32::{
        Media::{DirectShow::{IBaseFilter,ICreateDevEnum,IFilterGraph,IMediaControl,IPin,PINDIR_OUTPUT,AMPROPERTY_PIN_CATEGORY},
            KernelStreaming::IKsPropertySet,
            MediaFoundation::{AM_MEDIA_TYPE,AMPROPSETID_Pin,CLSID_FilterGraph,CLSID_SystemDeviceEnum,
                CLSID_VideoInputDeviceCategory,PIN_CATEGORY_CAPTURE}},
        System::{Com::{CoCreateInstance,CoTaskMemFree,CreateBindCtx,IMoniker,CLSCTX_INPROC_SERVER},
            Performance::{QueryPerformanceFrequency,QueryPerformanceCounter}},
    }};

    fn native(operation:&'static str,error:impl std::fmt::Display)->AudioCaptureError {
        AudioCaptureError::Native {operation,detail:error.to_string()}
    }
    struct MediaType(*mut AM_MEDIA_TYPE);
    impl Drop for MediaType {fn drop(&mut self){unsafe {
        if !self.0.is_null(){let value=&mut *self.0;if !value.pbFormat.is_null(){CoTaskMemFree(Some(value.pbFormat.cast()));}
            std::mem::ManuallyDrop::drop(&mut value.pUnk);CoTaskMemFree(Some(self.0.cast()));}
    }}}
    struct ComFormat(AM_MEDIA_TYPE);
    impl Drop for ComFormat {fn drop(&mut self){unsafe {
        if !self.0.pbFormat.is_null(){CoTaskMemFree(Some(self.0.pbFormat.cast()));}
        std::mem::ManuallyDrop::drop(&mut self.0.pUnk);
    }}}
    // The graph is still stopped during construction. Undo each successful
    // mutation in reverse order if any later allocation or COM call fails.
    struct BuildGuard<'a> {
        graph:&'a IFilterGraph, source:&'a IBaseFilter, audio:&'a FilterHandle,
        audio_output:&'a IPin, video:Option<(&'a IPin,&'a embedded_video::filter::FilterHandle)>,
        source_added:bool,audio_added:bool,audio_connected:bool,video_added:bool,video_connected:bool,armed:bool,
    }
    impl Drop for BuildGuard<'_> {
        fn drop(&mut self) {
            if !self.armed{return;}
            unsafe {
                if self.video_connected {if let Some((output,sink))=self.video {let _=self.graph.Disconnect(output);let _=self.graph.Disconnect(sink.pin());}}
                if self.audio_connected {let _=self.graph.Disconnect(self.audio_output);let _=self.graph.Disconnect(self.audio.pin());}
                if self.video_added {if let Some((_,sink))=self.video {let _=self.graph.RemoveFilter(&sink.filter);}}
                if self.audio_added {let _=self.graph.RemoveFilter(&self.audio.filter);}
                if self.source_added {let _=self.graph.RemoveFilter(self.source);}
            }
        }
    }
    struct VideoParts {output:IPin,sink:embedded_video::filter::FilterHandle}
    struct Parts {graph:IFilterGraph,control:IMediaControl,source:IBaseFilter,output:IPin,sink:FilterHandle,video:Option<VideoParts>,close_stage:u8}
    pub(crate) struct GraphOwner {
        parts:Option<Parts>,receiver:CallbackReceiver,gate:Arc<ReceiveGate>,format_changed:Arc<AtomicBool>,
        limits:AudioCaptureLimits,
        frequency:u64,graph_id:u64,clock_history:VecDeque<super::super::DirectShowClockSnapshot>,
        scope:EmbeddedAudioScope,
        closing:bool,
        video:Option<VideoReceiver>,video_gate:Option<Arc<VideoGate>>,
        _same_thread:PhantomData<Rc<()>>,
    }
    impl GraphOwner {
        /// # Safety
        /// The caller must already own the camera exclusively, initialize COM
        /// MTA on this thread, and keep the apartment alive until close succeeds.
        /// In particular, do not open this graph alongside an MF camera reader.
        pub unsafe fn open_exclusive(scope:EmbeddedAudioScope,limits:AudioCaptureLimits)->Result<Self,AudioCaptureError>{
            unsafe {Self::open_internal(scope,limits,None)}
        }
        /// Open video and embedded PCM audio on the *same* DirectShow source.
        /// The caller must hold exclusive camera authority, initialize an MTA
        /// and Media Foundation on this thread, and retain the graph until
        /// both callback pools have stopped. Only exactly matching advertised
        /// video media types are accepted; no mode or audio identity is guessed.
        pub unsafe fn open_shared(scope:EmbeddedAudioScope,binding:&VideoCaptureBinding,
            audio_limits:AudioCaptureLimits,video_limits:CameraLimits)->Result<Self,AudioCaptureError>{
            if binding.target()!=&scope.target{return Err(AudioCaptureError::Invalid("shared DirectShow camera binding"));}
            unsafe {Self::open_internal(scope,audio_limits,Some((binding.mode(),video_limits)))}
        }
        unsafe fn open_internal(scope:EmbeddedAudioScope,limits:AudioCaptureLimits,
            video_request:Option<(VideoCaptureMode,CameraLimits)>)->Result<Self,AudioCaptureError>{
            // The callback queues may hold older samples. Keep a small, bounded
            // history of clock observations on the owner, never in callbacks.
            let mut clock_history=VecDeque::new();
            clock_history.try_reserve_exact(128).map_err(|_|AudioCaptureError::Limit("DirectShow clock history allocation"))?;
            let VideoCaptureTarget::Windows {symbolic_link}=&scope.target else{return Err(AudioCaptureError::Unsupported("embedded DirectShow audio requires Windows"));};
            let enumerator:ICreateDevEnum=unsafe {CoCreateInstance(&CLSID_SystemDeviceEnum,None,CLSCTX_INPROC_SERVER)}
                .map_err(|e|native("DirectShow device enumerator",e))?;
            let mut monikers=None;
            unsafe {enumerator.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory,&mut monikers,0)}
                .map_err(|e|native("DirectShow video category",e))?;
            let monikers=monikers.ok_or_else(||AudioCaptureError::DeviceUnavailable(symbolic_link.clone()))?;
            let bind=unsafe {CreateBindCtx(0)}.map_err(|e|native("DirectShow bind context",e))?;
            let mut chosen:Option<IMoniker>=None;
            for index in 0..=256 {
                let mut slot=[None];let status=unsafe {monikers.Next(&mut slot,None)};
                if status.is_err(){return Err(native("DirectShow moniker enumeration",format!("{status:?}")));}
                let Some(moniker)=slot[0].take() else{break;};
                if index==256{return Err(AudioCaptureError::Limit("DirectShow camera count"));}
                let name=unsafe {moniker.GetDisplayName(&bind,None::<&IMoniker>)}.map_err(|e|native("DirectShow camera identity",e))?;
                if name.0.is_null(){return Err(AudioCaptureError::Invalid("DirectShow camera identity"));}
                let mut len=0usize;while len<=8192 && unsafe {*name.0.add(len)}!=0 {len+=1;}
                let value=if len<=8192 {String::from_utf16(unsafe {std::slice::from_raw_parts(name.0,len)}).ok()}else{None};
                unsafe {CoTaskMemFree(Some(name.0.cast()));}
                let value=value.ok_or(AudioCaptureError::Invalid("DirectShow camera identity"))?;
                let prefix="@device:pnp:";
                if value.get(..prefix.len()).is_some_and(|head|head.eq_ignore_ascii_case(prefix))
                    && value.get(prefix.len()..).is_some_and(|path|path.eq_ignore_ascii_case(symbolic_link)) {
                    if chosen.is_some(){return Err(AudioCaptureError::Invalid("ambiguous DirectShow camera identity"));}
                    chosen=Some(moniker);
                }
            }
            let source:IBaseFilter=unsafe {chosen.ok_or_else(||AudioCaptureError::DeviceUnavailable(symbolic_link.clone()))?
                .BindToObject(&bind,None::<&IMoniker>)}.map_err(|e|native("DirectShow camera source",e))?;
            let pins=unsafe {source.EnumPins()}.map_err(|e|native("DirectShow camera pins",e))?;
            let mut selected:Option<(IPin,OwnedMediaType)>=None;
            let mut selected_video:Option<(IPin,Arc<embedded_video::media_type::Owned>,i32,usize,Option<Vec<u8>>)>=None;
            for index in 0..=64 {
                let mut slot=[None];let status=unsafe {pins.Next(&mut slot,None)};
                if status.is_err(){return Err(native("DirectShow pin enumeration",format!("{status:?}")));}
                let Some(pin)=slot[0].take() else{break;};
                if index==64{return Err(AudioCaptureError::Limit("DirectShow pin count"));}
                if unsafe {pin.QueryDirection()}.map_err(|e|native("DirectShow pin direction",e))?!=PINDIR_OUTPUT {continue;}
                let Ok(properties)=pin.cast::<IKsPropertySet>() else{continue;};
                let mut category=GUID::zeroed();let mut bytes=0u32;
                if unsafe {properties.Get(&AMPROPSETID_Pin,AMPROPERTY_PIN_CATEGORY.0 as u32,std::ptr::null(),0,
                    (&mut category as *mut GUID).cast(),std::mem::size_of::<GUID>() as u32,&mut bytes)}.is_err()
                    || bytes as usize!=std::mem::size_of::<GUID>() || category!=PIN_CATEGORY_CAPTURE {continue;}
                let types=unsafe {pin.EnumMediaTypes()}.map_err(|e|native("DirectShow pin formats",e))?;
                let mut candidate=None;
                let mut video_candidate=None;
                for index in 0..=1024 {
                    let mut raw=std::ptr::null_mut();let mut fetched=0;
                    let status=unsafe {types.Next(std::slice::from_mut(&mut raw),Some(&mut fetched))};
                    if status.is_err(){if !raw.is_null(){drop(MediaType(raw));}return Err(native("DirectShow format enumeration",format!("{status:?}")));}
                    if raw.is_null(){break;}
                    let media=MediaType(raw);
                    if index==1024{return Err(AudioCaptureError::Limit("DirectShow format count"));}
                    if fetched!=1{return Err(AudioCaptureError::Invalid("DirectShow format count"));}
                    if candidate.is_none() {candidate=unsafe {OwnedMediaType::copy_from(media.0,limits)}.ok();}
                    if let Some((expected,video_limits))=video_request {
                        if video_candidate.is_none() {
                            if let Ok((pitch,cap,configuration))=unsafe {crate::camera_capture::native::directshow_video_layout(media.0,expected,video_limits)} {
                                let owned=unsafe {embedded_video::media_type::Owned::copy_from(media.0)}
                                    .map_err(|_|AudioCaptureError::Invalid("DirectShow video media type"))?;
                                video_candidate=Some((Arc::new(owned),pitch,cap,configuration));
                            }
                        }
                    }
                }
                if let Some(media)=candidate {
                    if selected.is_some(){return Err(AudioCaptureError::Invalid("ambiguous embedded audio capture pin"));}
                    selected=Some((pin,media));
                } else if let Some((media,pitch,cap,configuration))=video_candidate {
                    if selected_video.is_some(){return Err(AudioCaptureError::Invalid("ambiguous DirectShow video capture pin"));}
                    selected_video=Some((pin,media,pitch,cap,configuration));
                }
            }
            let (output,media)=selected.ok_or(AudioCaptureError::Unsupported("camera has no supported embedded DirectShow PCM capture pin"))?;
            if video_request.is_some() && selected_video.is_none(){return Err(AudioCaptureError::Unsupported("camera has no exact shared DirectShow video mode"));}
            let prepared_video=if let (Some((mode,video_limits)),Some((output,media,pitch,cap,configuration)))=(video_request,selected_video) {
                let (gate,receiver)=embedded_video::pool(mode,pitch,cap,3,video_limits.max_raw_bytes,video_limits,configuration)
                    .map_err(|e|native("DirectShow video pool",e))?;
                let sink=embedded_video::filter::create(gate.clone(),media.clone())
                    .ok_or(AudioCaptureError::Limit("DirectShow video sink creation"))?;
                Some((output,sink,media,gate,receiver))
            } else {None};
            let (sender,receiver)=callback::pool(limits)?;
            let gate=Arc::new(ReceiveGate::new(media.layout,sender,limits));
            let mut frequency=0i64;unsafe {QueryPerformanceFrequency(&mut frequency)}.map_err(|e|native("QueryPerformanceFrequency",e))?;
            let frequency=u64::try_from(frequency).ok().filter(|v|*v!=0).ok_or(AudioCaptureError::Invalid("QPC frequency"))?;
            let changed=Arc::new(AtomicBool::new(false));
            let sink=directshow_filter::create(gate.clone(),frequency,changed.clone()).ok_or(AudioCaptureError::Limit("DirectShow sink creation"))?;
            let graph:IFilterGraph=unsafe {CoCreateInstance(&CLSID_FilterGraph,None,CLSCTX_INPROC_SERVER)}.map_err(|e|native("DirectShow filter graph",e))?;
            let control:IMediaControl=graph.cast().map_err(|e|native("DirectShow graph control",e))?;
            let mut guard=BuildGuard {graph:&graph,source:&source,audio:&sink,audio_output:&output,
                video:prepared_video.as_ref().map(|(output,sink,_,_,_)|(output,sink)),
                source_added:false,audio_added:false,audio_connected:false,video_added:false,video_connected:false,armed:true};
            guard.source_added=true;
            unsafe {graph.AddFilter(&source,w!("SkyLive camera"))}.map_err(|e|native("DirectShow add camera",e))?;
            guard.audio_added=true;
            unsafe {graph.AddFilter(&sink.filter,w!("SkyLive audio sink"))}.map_err(|e|native("DirectShow add audio sink",e))?;
            let media=ComFormat(media.to_com()?);
            guard.audio_connected=true;
            unsafe {graph.ConnectDirect(&output,sink.pin(),Some(&media.0))}.map_err(|e|native("DirectShow connect audio",e))?;
            if let Some((video_output,video_sink,video_media,_,_))=&prepared_video {
                guard.video_added=true;
                unsafe {graph.AddFilter(&video_sink.filter,w!("SkyLive video sink"))}.map_err(|e|native("DirectShow add video sink",e))?;
                let video_type=ComFormat(video_media.to_com().map_err(|e|native("DirectShow video media type",e))?);
                guard.video_connected=true;
                unsafe {graph.ConnectDirect(video_output,video_sink.pin(),Some(&video_type.0))}.map_err(|e|native("DirectShow connect video",e))?;
            }
            guard.armed=false;drop(guard);
            let (video,video_gate,video_receiver)=if let Some((video_output,video_sink,_,video_gate,video_receiver))=prepared_video {
                (Some(VideoParts {output:video_output,sink:video_sink}),Some(video_gate),Some(video_receiver))
            } else {(None,None,None)};
            static NEXT_GRAPH_ID:std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(1);
            let graph_id=NEXT_GRAPH_ID.fetch_add(1,Ordering::Relaxed);
            let mut owner=Self {parts:Some(Parts {graph,control,source,output,sink,video,close_stage:0}),receiver,gate,format_changed:changed,limits,frequency,graph_id,clock_history,scope,closing:false,video:video_receiver,video_gate,_same_thread:PhantomData};
            if let Err(error)=unsafe {owner.parts.as_ref().expect("graph parts").control.Run()} {
                let _=owner.close();return Err(native("DirectShow run audio graph",error));
            }
            Ok(owner)
        }
        /// The stream times are comparable only while both sinks have received
        /// the same Run start and reference clock. Bracket GetTime with QPC;
        /// the bracket measures observation uncertainty, not clock rate drift.
        fn clock_observation(&mut self,stream_ns:Option<i128>)->Option<super::super::DirectShowClockSnapshot> {
            let parts=self.parts.as_ref()?;
            let video=parts.video.as_ref()?;
            let audio_before=parts.sink.clock.snapshot()?;
            let video_before=video.sink.clock.snapshot()?;
            if audio_before.0.as_raw()!=video_before.0.as_raw() || audio_before.1!=video_before.1 {return None;}
            let mut before=0i64;let mut after=0i64;
            unsafe {QueryPerformanceCounter(&mut before)}.ok()?;
            let reference=unsafe {audio_before.0.GetTime()}.ok()?;
            unsafe {QueryPerformanceCounter(&mut after)}.ok()?;
            if before<0 || after<before {return None;}
            let to_ns=|ticks:i64,ceil:bool|->Option<u64> {
                let product=u128::try_from(ticks).ok()?.checked_mul(1_000_000_000)?;
                let value=if ceil {product.checked_add(u128::from(self.frequency)-1)?} else {product};
                u64::try_from(value/u128::from(self.frequency)).ok()
            };
            let qpc_before_ns=to_ns(before,false)?;let qpc_after_ns=to_ns(after,true)?;
            if qpc_after_ns.checked_sub(qpc_before_ns)?>1_000_000 {return None;}
            let audio_after=parts.sink.clock.snapshot()?;
            let video_after=video.sink.clock.snapshot()?;
            if !super::directshow_clock::same(&audio_before,&audio_after)
                || !super::directshow_clock::same(&video_before,&video_after) {return None;}
            if self.clock_history.back().is_some_and(|old|old.graph_id!=self.graph_id
                || old.audio_clock_generation!=audio_before.2 || old.video_clock_generation!=video_before.2
                || old.run_start_100ns!=audio_before.1 || old.reference_100ns>reference
                || old.qpc_after_ns>qpc_before_ns) {self.clock_history.clear();}
            while self.clock_history.front().is_some_and(|old|qpc_after_ns.saturating_sub(old.qpc_before_ns)>100_000_000) {
                self.clock_history.pop_front();
            }
            if self.clock_history.len()==128 {self.clock_history.pop_front();}
            let snapshot=super::super::DirectShowClockSnapshot {graph_id:self.graph_id,
                audio_clock_generation:audio_before.2,video_clock_generation:video_before.2,
                run_start_100ns:audio_before.1,reference_100ns:reference,qpc_before_ns,qpc_after_ns,previous:None};
            self.clock_history.push_back(snapshot);
            let target=i128::from(audio_before.1).checked_mul(100)?.checked_add(stream_ns?)?;
            // Find the tightest retained adjacent bracket that strictly spans
            // this sample. A queued packet can predate the last observation.
            // Never interpolate/extrapolate or assume a reference-clock rate.
            for (old,new) in self.clock_history.iter().zip(self.clock_history.iter().skip(1)).rev() {
                if i128::from(old.reference_100ns)*100 < target && target < i128::from(new.reference_100ns)*100 {
                    let mut matched=*new;
                    matched.previous=Some((old.reference_100ns,old.qpc_before_ns,old.qpc_after_ns));
                    return Some(matched);
                }
            }
            None
        }
        pub fn pop(&mut self)->Option<CallbackPacket>{self.receiver.pop()}
        pub fn scope(&self)->&EmbeddedAudioScope {&self.scope}
        pub fn recycle(&mut self,packet:CallbackPacket)->bool{self.receiver.recycle(packet)}
        pub fn format_changed(&self)->bool{self.format_changed.load(Ordering::Acquire)}
        /// Stop admission first; the caller retains and retries on false or
        /// error. Never free a live callback pool merely because stop failed.
        pub fn close(&mut self)->Result<bool,AudioCaptureError>{
            self.closing=true;
            let Some(parts)=self.parts.as_mut() else{return Ok(true);};
            self.gate.stop();
            if let Some(gate)=&self.video_gate {gate.stop();}
            if parts.close_stage==0 {
                unsafe {parts.control.Stop()}.map_err(|e|native("DirectShow stop audio graph",e))?;
                // Stop can be asynchronous. Do not disconnect or retire a
                // callback merely because the graph accepted the request.
                let state=unsafe {parts.control.GetState(0)}.map_err(|e|native("DirectShow audio graph state",e))?;
                if state!=windows::Win32::Media::DirectShow::State_Stopped.0{return Ok(false);}
                parts.close_stage=1;
            }
            if parts.close_stage==1 {if !self.gate.quiesced() || self.video_gate.as_ref().is_some_and(|gate|!gate.quiesced()){return Ok(false);}parts.close_stage=2;}
            if parts.close_stage==2 {unsafe {parts.graph.Disconnect(&parts.output)}.map_err(|e|native("DirectShow disconnect audio output",e))?;parts.close_stage=3;}
            if parts.close_stage==3 {unsafe {parts.graph.Disconnect(parts.sink.pin())}.map_err(|e|native("DirectShow disconnect audio sink",e))?;parts.close_stage=4;}
            if parts.close_stage==4 {if let Some(video)=&parts.video {unsafe {parts.graph.Disconnect(&video.output)}.map_err(|e|native("DirectShow disconnect video output",e))?;}parts.close_stage=5;}
            if parts.close_stage==5 {if let Some(video)=&parts.video {unsafe {parts.graph.Disconnect(video.sink.pin())}.map_err(|e|native("DirectShow disconnect video sink",e))?;}parts.close_stage=6;}
            if parts.close_stage==6 {unsafe {parts.graph.RemoveFilter(&parts.sink.filter)}.map_err(|e|native("DirectShow remove audio sink",e))?;parts.close_stage=7;}
            if parts.close_stage==7 {if let Some(video)=&parts.video {unsafe {parts.graph.RemoveFilter(&video.sink.filter)}.map_err(|e|native("DirectShow remove video sink",e))?;}parts.close_stage=8;}
            if parts.close_stage==8 {unsafe {parts.graph.RemoveFilter(&parts.source)}.map_err(|e|native("DirectShow remove camera source",e))?;parts.close_stage=9;}
            if !parts.sink.retire() || parts.video.as_ref().is_some_and(|video|!video.sink.retire()){return Ok(false);}
            self.parts.take();Ok(true)
        }
    }
    // The graph and this adapter remain on the same native owner thread. The
    // callback pool is copied only here, never from the DirectShow callback.
    // Received time is QPC-derived host time, not a device presentation clock.
    impl NativeAudioStream for GraphOwner {
        fn poll(&mut self)->Result<Option<NativePcmBlock>,AudioCaptureError> {
            if self.closing||self.parts.is_none(){return Err(AudioCaptureError::Closed);}
            if self.format_changed(){return Err(AudioCaptureError::Unsupported("DirectShow embedded audio format changed"));}
            let Some(packet)=self.receiver.pop() else{return Ok(None);};
            let mut samples=Vec::new();
            if samples.try_reserve_exact(packet.samples().len()).is_err() {
                self.receiver.recycle(packet);
                return Err(AudioCaptureError::Limit("embedded audio packet allocation"));
            }
            samples.extend_from_slice(packet.samples());
            let graph_timestamp_ns=match packet.stamp.device {crate::audio_capture::callback::CallbackTime::DirectShow100ns {value,..}=>Some(i128::from(value)*100),_=>None};
            let block=NativePcmBlock {format:packet.format,frames:packet.frames,samples,
                device_timestamp_ns:None,
                graph_timestamp_ns,
                directshow_clock:self.clock_observation(graph_timestamp_ns).filter(|clock|matches!(packet.stamp.device,crate::audio_capture::callback::CallbackTime::DirectShow100ns {generation:Some(g),..} if g==clock.audio_clock_generation)),
                host_received_ns:packet.stamp.received_ticks,
                device_frame_position:None,flags:packet.flags};
            if !self.receiver.recycle(packet){return Err(AudioCaptureError::Invalid("embedded audio callback packet ownership"));}
            block.validate(self.limits)?;
            Ok(Some(block))
        }
        fn close(&mut self)->Result<(),AudioCaptureError> {
            if GraphOwner::close(self)? {Ok(())} else {Err(AudioCaptureError::Busy)}
        }
    }
    // Both streams must be polled by the same graph-owner thread. This is an
    // internal adapter, not a connection to the separate camera/audio owners.
    impl NativeCameraStream for GraphOwner {
        fn poll_embedded_audio(&mut self)->Result<Option<NativePcmBlock>,CameraError> {
            NativeAudioStream::poll(self).map_err(|error|CameraError::Native {
                operation:"read embedded DirectShow audio",detail:error.to_string(),
            })
        }
        fn poll(&mut self,limits:CameraLimits)->Result<Option<NativeVideoFrame>,CameraError> {
            if self.closing || self.parts.is_none(){return Err(CameraError::Closed);}
            limits.validate()?;
            let receiver=self.video.as_mut().ok_or(CameraError::Unavailable("shared DirectShow video is not open"))?;
            let packet=receiver.poll()?;
            let frame=packet.map(|(mut frame,generation)|{
                frame.directshow_clock=self.clock_observation(Some(frame.presentation_ns)).filter(|clock|generation==Some(clock.video_clock_generation));
                frame
            });
            if let Some(frame)=&frame {super::super::checked_frame_bytes(frame.image.width(),frame.image.height(),limits)?;}
            Ok(frame)
        }
        fn stop(&mut self)->Result<bool,CameraError> {
            GraphOwner::close(self).map_err(|e|CameraError::Native {operation:"stop shared DirectShow graph",detail:e.to_string()})
        }
    }
    impl Drop for GraphOwner {
        fn drop(&mut self){
            // Explicit close belongs to the owning worker: an arbitrary driver
            // can block inside Stop, so never call native graph methods in Drop.
            // If the owner violates that contract, retain COM resources rather
            // than freeing a live callback's pin or pool.
            self.gate.stop();
            if let Some(gate)=&self.video_gate {gate.stop();}
            if let Some(parts)=self.parts.take(){std::mem::forget(parts);}
        }
    }
}

#[cfg(target_os = "windows")]
pub(crate) use directshow_graph::GraphOwner;
