//! Staged VideoToolbox adapter. SDK signatures/lifecycle audited; native runtime
//! and cross-compilation remain acceptance gates. No additional binding crate.
use super::{
    AccessUnit, Adapter, CodecConfiguration, Config, Error, MAX_AU_BYTES, MAX_HEADER_BYTES,
    Nv12Input,
};
use objc2::rc::autoreleasepool;
use objc2_core_foundation::{CFRetained, CFString};
use objc2_core_media::{
    CMSampleBuffer, CMTime, CMTimeFlags, CMVideoFormatDescriptionGetDimensions,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex, kCMVideoCodecType_H264,
};
use objc2_core_video::*;
use objc2_foundation::NSNumber;
use std::{
    ffi::c_void,
    ptr::{self, NonNull},
    rc::Rc,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicUsize, Ordering},
    },
    thread,
};

type SessionRef = *mut c_void;
type OutputCallback =
    unsafe extern "C-unwind" fn(*mut c_void, *mut c_void, i32, u32, *mut CMSampleBuffer);
// Apple VTCompressionSession.h / VTSession.h; all opaque CF objects remain
// pointers and CMTime uses the existing generated packed(4) CoreMedia layout.
#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C-unwind" {
    fn VTCompressionSessionCreate(
        allocator: *const c_void,
        width: i32,
        height: i32,
        codec_type: u32,
        specification: *const c_void,
        image_attributes: *const c_void,
        compressed_allocator: *const c_void,
        callback: Option<OutputCallback>,
        refcon: *mut c_void,
        session: *mut SessionRef,
    ) -> i32;
    fn VTSessionSetProperty(session: SessionRef, key: &CFString, value: *const c_void) -> i32;
    fn VTCompressionSessionPrepareToEncodeFrames(session: SessionRef) -> i32;
    fn VTCompressionSessionEncodeFrame(
        session: SessionRef,
        image: &CVPixelBuffer,
        pts: CMTime,
        duration: CMTime,
        properties: *const c_void,
        source_refcon: *mut c_void,
        flags: *mut u32,
    ) -> i32;
    fn VTCompressionSessionCompleteFrames(session: SessionRef, until: CMTime) -> i32;
    fn VTCompressionSessionInvalidate(session: SessionRef);
    static kVTCompressionPropertyKey_RealTime: &'static CFString;
    static kVTCompressionPropertyKey_AllowFrameReordering: &'static CFString;
    static kVTCompressionPropertyKey_ProfileLevel: &'static CFString;
    static kVTProfileLevel_H264_Baseline_AutoLevel: &'static CFString;
    static kVTCompressionPropertyKey_AverageBitRate: &'static CFString;
    static kVTCompressionPropertyKey_ExpectedFrameRate: &'static CFString;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: &'static CFString;
    static kVTCompressionPropertyKey_MaxFrameDelayCount: &'static CFString;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C-unwind" {
    fn CFRelease(value: *const c_void);
}

fn status(operation: &'static str, code: i32) -> Result<(), Error> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::Platform { operation, code })
    }
}
fn time(value: i64) -> CMTime {
    CMTime {
        value,
        timescale: 10_000_000,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}
fn all_frames() -> CMTime {
    CMTime {
        value: 0,
        timescale: 0,
        flags: CMTimeFlags::empty(),
        epoch: 0,
    }
}
/// Exact conversion only. Invalid DTS is permitted exclusively as the explicit
/// no-reordering fallback to the checked input PTS on the owner.
fn ticks(value: CMTime) -> Option<i64> {
    let CMTime {
        value,
        timescale,
        flags,
        epoch,
    } = value;
    if !flags.contains(CMTimeFlags::Valid)
        || flags.intersects(CMTimeFlags::ImpliedValueFlagsMask)
        || timescale <= 0
        || epoch != 0
    {
        return None;
    }
    let numerator = i128::from(value) * 10_000_000;
    if numerator % i128::from(timescale) != 0 {
        return None;
    }
    i64::try_from(numerator / i128::from(timescale)).ok()
}
struct Slot {
    bytes: Box<[u8]>,
    header: Box<[u8]>,
    length: usize,
    header_length: usize,
    pts: Option<i64>,
    dts: Option<i64>,
    duration: Option<i64>,
    ready: bool,
}
struct CallbackState {
    width: i32,
    height: i32,
    slot: Mutex<Slot>,
    closing: AtomicBool,
    active: AtomicUsize,
    problem: AtomicU8,
    native_status: AtomicI32,
}
impl CallbackState {
    fn new(config: &Config) -> Self {
        Self {
            width: config.width() as i32,
            height: config.height() as i32,
            slot: Mutex::new(Slot {
                bytes: vec![0; MAX_AU_BYTES].into_boxed_slice(),
                header: vec![0; MAX_HEADER_BYTES].into_boxed_slice(),
                length: 0,
                header_length: 0,
                pts: None,
                dts: None,
                duration: None,
                ready: false,
            }),
            closing: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            problem: AtomicU8::new(0),
            native_status: AtomicI32::new(0),
        }
    }
    fn fail(&self, code: u8) {
        let _ = self
            .problem
            .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
    }
    fn check(&self) -> Result<(), Error> {
        let native = self.native_status.load(Ordering::Acquire);
        if native != 0 {
            return Err(Error::Platform {
                operation: "VideoToolbox compression callback",
                code: native,
            });
        }
        match self.problem.load(Ordering::Acquire) {
            0 => Ok(()),
            1 => Err(Error::Protocol(
                "concurrent/poisoned VideoToolbox callback slot",
            )),
            2 => Err(Error::Protocol(
                "VideoToolbox dropped or omitted an admitted frame",
            )),
            3 => Err(Error::Protocol(
                "VideoToolbox sample format/count differs from contract",
            )),
            4 => Err(Error::Limit("VideoToolbox callback data/header bounds")),
            5 => Err(Error::Protocol(
                "VideoToolbox parameter-set or block-buffer copy failed",
            )),
            _ => Err(Error::Protocol(
                "duplicate VideoToolbox callback without new input",
            )),
        }
    }
}
struct ActiveCallback<'a>(&'a CallbackState);
impl Drop for ActiveCallback<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    _source: *mut c_void,
    code: i32,
    flags: u32,
    sample: *mut CMSampleBuffer,
) {
    let Some(state) = (unsafe { refcon.cast::<CallbackState>().as_ref() }) else {
        return;
    };
    state.active.fetch_add(1, Ordering::AcqRel);
    let _active = ActiveCallback(state);
    if state.closing.load(Ordering::Acquire) {
        return;
    }
    if code != 0 {
        let _ = state
            .native_status
            .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
        return;
    }
    // kVTEncodeInfo_FrameDropped is UInt32 bit1; asynchronous bit0 is harmless.
    let Some(sample) = (unsafe { sample.as_ref() }) else {
        state.fail(2);
        return;
    };
    if flags & 2 != 0 {
        state.fail(2);
        return;
    }
    let Ok(mut slot) = state.slot.try_lock() else {
        state.fail(1);
        return;
    };
    if slot.ready {
        state.fail(6);
        return;
    }
    let Some(format) = (unsafe { sample.format_description() }) else {
        state.fail(3);
        return;
    };
    let dimensions = unsafe { CMVideoFormatDescriptionGetDimensions(&format) };
    if unsafe { sample.num_samples() } != 1
        || unsafe { format.media_sub_type() } != kCMVideoCodecType_H264
        || dimensions.width != state.width
        || dimensions.height != state.height
    {
        state.fail(3);
        return;
    }
    let Some(data) = (unsafe { sample.data_buffer() }) else {
        state.fail(3);
        return;
    };
    let length = unsafe { data.data_length() };
    if length == 0 || length > slot.bytes.len() {
        state.fail(4);
        return;
    }
    let mut count = 0;
    let mut length_width = 0;
    let query = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            &format,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut count,
            &mut length_width,
        )
    };
    if query != 0 {
        let _ = state
            .native_status
            .compare_exchange(0, query, Ordering::AcqRel, Ordering::Acquire);
        return;
    }
    if count == 0 || count > 128 || length_width != 4 {
        state.fail(3);
        return;
    }
    let mut header_length = 0usize;
    for index in 0..count {
        let mut pointer = ptr::null();
        let mut size = 0;
        let mut observed_count = 0;
        let mut observed_width = 0;
        let result = unsafe {
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                &format,
                index,
                &mut pointer,
                &mut size,
                &mut observed_count,
                &mut observed_width,
            )
        };
        if result != 0 {
            let _ = state.native_status.compare_exchange(
                0,
                result,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return;
        }
        if pointer.is_null() || size == 0 || observed_count != count || observed_width != 4 {
            state.fail(5);
            return;
        }
        let Some(end) = header_length
            .checked_add(4)
            .and_then(|n| n.checked_add(size))
        else {
            state.fail(4);
            return;
        };
        if end > slot.header.len() {
            state.fail(4);
            return;
        }
        slot.header[header_length..header_length + 4].copy_from_slice(&[0, 0, 0, 1]);
        unsafe {
            ptr::copy_nonoverlapping(
                pointer,
                slot.header.as_mut_ptr().add(header_length + 4),
                size,
            )
        };
        header_length = end;
    }
    let Some(destination) = NonNull::new(slot.bytes.as_mut_ptr().cast()) else {
        state.fail(5);
        return;
    };
    let copied = unsafe { data.copy_data_bytes(0, length, destination) };
    if copied != 0 {
        let _ =
            state
                .native_status
                .compare_exchange(0, copied, Ordering::AcqRel, Ordering::Acquire);
        return;
    }
    slot.length = length;
    slot.header_length = header_length;
    let pts = unsafe { sample.presentation_time_stamp() };
    let dts = unsafe { sample.decode_time_stamp() };
    let duration = unsafe { sample.duration() };
    slot.pts = ticks(pts);
    slot.dts = ticks(dts);
    slot.duration = ticks(duration);
    if slot.pts.is_none()
        || (dts.flags.contains(CMTimeFlags::Valid) && slot.dts.is_none())
        || (duration.flags.contains(CMTimeFlags::Valid) && slot.duration.is_none())
    {
        state.fail(3);
        return;
    }
    slot.ready = true;
    // Retained CoreMedia getters above balance their retains while the original
    // sample is still callback-owned. No native sample crosses to the GUI.
}

struct Session {
    raw: NonNull<c_void>,
    state: *const CallbackState,
    _not_send: Rc<()>,
}
impl Drop for Session {
    fn drop(&mut self) {
        autoreleasepool(|_| {
            // State Box outlives this guard on construction failure and EncoderDrop.
            let state = unsafe { &*self.state };
            state.closing.store(true, Ordering::Release);
            unsafe {
                let _ = VTCompressionSessionCompleteFrames(self.raw.as_ptr(), all_frames());
                VTCompressionSessionInvalidate(self.raw.as_ptr());
            }
            // Invalidate shuts off new callbacks. Existing callback work is bounded,
            // but no elapsed-time guess is used to free a live callback reference.
            while state.active.load(Ordering::Acquire) != 0 {
                thread::yield_now();
            }
            unsafe { CFRelease(self.raw.as_ptr()) };
        });
    }
}
pub(super) struct Encoder {
    session: Session,
    config: Config,
    configuration: Option<CodecConfiguration>,
    eos: bool,
    // Session's Drop completes and invalidates before this Box is destroyed.
    callback: Box<CallbackState>,
}
impl Encoder {
    pub(super) fn new(config: Config) -> Result<Self, Error> {
        autoreleasepool(|_| Self::create(config))
    }
    fn create(config: Config) -> Result<Self, Error> {
        let callback = Box::new(CallbackState::new(&config));
        let mut raw = ptr::null_mut();
        let result = unsafe {
            VTCompressionSessionCreate(
                ptr::null(),
                config.width() as i32,
                config.height() as i32,
                kCMVideoCodecType_H264,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                Some(output_callback),
                (&*callback as *const CallbackState).cast_mut().cast(),
                &mut raw,
            )
        };
        // Every non-null create-rule object is retired even if creation returns
        // an error; the callback context has already been allocated and is live.
        let session = NonNull::new(raw).map(|raw| Session {
            raw,
            state: &*callback,
            _not_send: Rc::new(()),
        });
        status("VTCompressionSessionCreate", result)?;
        let session = session.ok_or(Error::Protocol("VideoToolbox created a null session"))?;
        unsafe {
            set_number(
                &session,
                kVTCompressionPropertyKey_RealTime,
                &NSNumber::new_bool(true),
            )?;
            set_number(
                &session,
                kVTCompressionPropertyKey_AllowFrameReordering,
                &NSNumber::new_bool(false),
            )?;
            set_number(
                &session,
                kVTCompressionPropertyKey_MaxFrameDelayCount,
                &NSNumber::new_u32(0),
            )?;
            set_number(
                &session,
                kVTCompressionPropertyKey_AverageBitRate,
                &NSNumber::new_u32(config.bitrate()),
            )?;
            set_number(
                &session,
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &NSNumber::new_u32(config.gop()),
            )?;
            let (num, den) = config.fps();
            set_number(
                &session,
                kVTCompressionPropertyKey_ExpectedFrameRate,
                &NSNumber::new_f64(f64::from(num) / f64::from(den)),
            )?;
            status(
                "VideoToolbox Baseline profile",
                VTSessionSetProperty(
                    session.raw.as_ptr(),
                    kVTCompressionPropertyKey_ProfileLevel,
                    (kVTProfileLevel_H264_Baseline_AutoLevel as *const CFString).cast(),
                ),
            )?;
            status(
                "VTCompressionSessionPrepareToEncodeFrames",
                VTCompressionSessionPrepareToEncodeFrames(session.raw.as_ptr()),
            )?;
        }
        Ok(Self {
            session,
            config,
            configuration: None,
            eos: false,
            callback,
        })
    }
    fn encode_native(&mut self, input: Nv12Input) -> Result<Vec<AccessUnit>, Error> {
        input.validate(&self.config)?;
        if self.eos {
            return Err(Error::Protocol("VideoToolbox input after EOS"));
        }
        self.callback.check()?;
        {
            let slot = self
                .callback
                .slot
                .lock()
                .map_err(|_| Error::Protocol("poisoned VideoToolbox owner slot"))?;
            if slot.ready {
                return Err(Error::Protocol("unconsumed VideoToolbox frame"));
            }
        }
        let buffer = pixel_buffer(&self.config, input.bytes())?;
        let mut flags = 0;
        status("VTCompressionSessionEncodeFrame", unsafe {
            VTCompressionSessionEncodeFrame(
                self.session.raw.as_ptr(),
                &buffer,
                time(input.pts()),
                time(input.duration()),
                ptr::null(),
                ptr::null_mut(),
                &mut flags,
            )
        })?;
        if flags & 2 != 0 {
            return Err(Error::Protocol(
                "VideoToolbox synchronously dropped a frame",
            ));
        }
        // Do not hold slot locks across either call; callback may be inline or
        // asynchronous. Numeric completion emits this frame before returning.
        status("VTCompressionSessionCompleteFrames(input PTS)", unsafe {
            VTCompressionSessionCompleteFrames(self.session.raw.as_ptr(), time(input.pts()))
        })?;
        self.callback.check()?;
        let mut slot = self
            .callback
            .slot
            .lock()
            .map_err(|_| Error::Protocol("poisoned VideoToolbox owner slot"))?;
        if !slot.ready || slot.pts != Some(input.pts()) {
            return Err(Error::Protocol(
                "VideoToolbox completion omitted/mismatched frame",
            ));
        }
        let dts = slot.dts.unwrap_or(input.pts());
        let duration = slot.duration.unwrap_or(input.duration());
        if duration != input.duration() {
            return Err(Error::Protocol("VideoToolbox changed frame duration"));
        }
        let configuration = CodecConfiguration::parse(&slot.header[..slot.header_length])?;
        let packet = AccessUnit::new(
            self.config.generation(),
            slot.bytes[..slot.length].to_vec(),
            input.pts(),
            dts,
            duration,
        )?;
        self.configuration = Some(configuration);
        slot.ready = false;
        slot.length = 0;
        slot.header_length = 0;
        Ok(vec![packet])
    }
}
impl Adapter for Encoder {
    fn configuration(&self) -> Option<&CodecConfiguration> {
        self.configuration.as_ref()
    }
    fn encode(&mut self, input: Nv12Input) -> Result<Vec<AccessUnit>, Error> {
        autoreleasepool(|_| self.encode_native(input))
    }
    fn drain(&mut self) -> Result<Vec<AccessUnit>, Error> {
        autoreleasepool(|_| {
            if self.eos {
                return Err(Error::Protocol("VideoToolbox EOS already sent"));
            }
            self.eos = true;
            status("VideoToolbox EOS CompleteFrames", unsafe {
                VTCompressionSessionCompleteFrames(self.session.raw.as_ptr(), all_frames())
            })?;
            self.callback.check()?;
            let slot = self
                .callback
                .slot
                .lock()
                .map_err(|_| Error::Protocol("poisoned VideoToolbox drain slot"))?;
            if slot.ready {
                return Err(Error::Protocol(
                    "unexpected pending frame after per-input completion",
                ));
            }
            Ok(Vec::new())
        })
    }
}
unsafe fn set_number(session: &Session, key: &CFString, value: &NSNumber) -> Result<(), Error> {
    status("VideoToolbox numeric property", unsafe {
        VTSessionSetProperty(session.raw.as_ptr(), key, (value as *const NSNumber).cast())
    })
}
fn pixel_buffer(config: &Config, packed: &[u8]) -> Result<CFRetained<CVPixelBuffer>, Error> {
    let mut raw = ptr::null_mut();
    let created = unsafe {
        CVPixelBufferCreate(
            None,
            config.width() as usize,
            config.height() as usize,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            None,
            NonNull::from(&mut raw),
        )
    };
    let buffer = NonNull::new(raw).map(|raw| unsafe { CFRetained::from_raw(raw) });
    status("CVPixelBufferCreate", created)?;
    let buffer = buffer.ok_or(Error::Protocol("CoreVideo created null NV12 buffer"))?;
    status("CVPixelBufferLockBaseAddress", unsafe {
        CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::empty())
    })?;
    let result = copy_planes(config, packed, &buffer);
    let unlock =
        unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
    result?;
    status("CVPixelBufferUnlockBaseAddress", unlock)?;
    Ok(buffer)
}
fn copy_planes(config: &Config, packed: &[u8], buffer: &CVPixelBuffer) -> Result<(), Error> {
    let width = config.width() as usize;
    let height = config.height() as usize;
    if CVPixelBufferGetPlaneCount(buffer) != 2
        || CVPixelBufferGetWidth(buffer) != width
        || CVPixelBufferGetHeight(buffer) != height
        || CVPixelBufferGetWidthOfPlane(buffer, 0) != width
        || CVPixelBufferGetHeightOfPlane(buffer, 0) != height
        || CVPixelBufferGetWidthOfPlane(buffer, 1) != width / 2
        || CVPixelBufferGetHeightOfPlane(buffer, 1) != height / 2
    {
        return Err(Error::Protocol("CoreVideo NV12 plane dimensions"));
    }
    let data_size = CVPixelBufferGetDataSize(buffer);
    if data_size == 0 || data_size > 16 * 1024 * 1024 {
        return Err(Error::Limit("native NV12 allocation"));
    }
    let mut extents = [(0usize, 0usize); 2];
    for plane in 0..2 {
        let rows = if plane == 0 { height } else { height / 2 };
        let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, plane);
        let pointer = CVPixelBufferGetBaseAddressOfPlane(buffer, plane).cast::<u8>();
        let bytes = stride
            .checked_mul(rows)
            .ok_or(Error::Limit("native NV12 row extent"))?;
        let end = (pointer as usize)
            .checked_add(bytes)
            .ok_or(Error::Limit("native NV12 pointer extent"))?;
        if pointer.is_null() || stride < width || bytes > data_size {
            return Err(Error::Protocol("native NV12 plane bounds"));
        }
        extents[plane] = (pointer as usize, end);
    }
    if extents[0].0 < extents[1].1 && extents[1].0 < extents[0].1 {
        return Err(Error::Protocol("native NV12 planes overlap"));
    }
    if (extents[0].1 - extents[0].0) + (extents[1].1 - extents[1].0) > data_size {
        return Err(Error::Protocol("native NV12 total plane extent"));
    }
    for plane in 0..2 {
        let rows = if plane == 0 { height } else { height / 2 };
        let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, plane);
        let pointer = CVPixelBufferGetBaseAddressOfPlane(buffer, plane).cast::<u8>();
        let offset = if plane == 0 { 0 } else { width * height };
        // The successfully locked CoreVideo object owns disjoint planes; checked
        // stride/row extents bound writes and leave padding deterministically zero.
        unsafe {
            ptr::write_bytes(pointer, 0, stride * rows);
        }
        for row in 0..rows {
            unsafe {
                ptr::copy_nonoverlapping(
                    packed.as_ptr().add(offset + row * width),
                    pointer.add(row * stride),
                    width,
                )
            };
        }
    }
    unsafe {
        buffer.set_attachment(
            kCVImageBufferColorPrimariesKey,
            kCVImageBufferColorPrimaries_ITU_R_709_2.as_ref(),
            CVAttachmentMode::ShouldPropagate,
        );
        buffer.set_attachment(
            kCVImageBufferTransferFunctionKey,
            kCVImageBufferTransferFunction_ITU_R_709_2.as_ref(),
            CVAttachmentMode::ShouldPropagate,
        );
        buffer.set_attachment(
            kCVImageBufferYCbCrMatrixKey,
            kCVImageBufferYCbCrMatrix_ITU_R_709_2.as_ref(),
            CVAttachmentMode::ShouldPropagate,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cmtime_conversion_is_exact_and_rejects_nonnumeric_or_foreign_epoch() {
        assert_eq!(ticks(time(333_333)), Some(333_333));
        assert_eq!(ticks(all_frames()), None);
        assert_eq!(
            ticks(CMTime {
                value: 1,
                timescale: 3,
                flags: CMTimeFlags::Valid,
                epoch: 0
            }),
            None
        );
        assert_eq!(
            ticks(CMTime {
                value: 1,
                timescale: 1,
                flags: CMTimeFlags::Valid,
                epoch: 1
            }),
            None
        );
    }
    #[test]
    fn callback_error_and_closing_paths_keep_exact_status_and_retire_counter_without_sdk_calls() {
        let config = Config::new(
            super::super::Generation {
                session: 1,
                output: 1,
                encoder: 1,
            },
            64,
            48,
            30,
            1,
            800_000,
            30,
        )
        .unwrap();
        let state = CallbackState::new(&config);
        let pointer = (&state as *const CallbackState).cast_mut().cast();
        unsafe { output_callback(pointer, ptr::null_mut(), -12903, 0, ptr::null_mut()) };
        assert_eq!(
            state.check(),
            Err(Error::Platform {
                operation: "VideoToolbox compression callback",
                code: -12903
            })
        );
        assert_eq!(state.active.load(Ordering::Acquire), 0);
        state.closing.store(true, Ordering::Release);
        unsafe { output_callback(pointer, ptr::null_mut(), -1, 2, ptr::null_mut()) };
        assert_eq!(state.native_status.load(Ordering::Acquire), -12903);
        assert_eq!(state.active.load(Ordering::Acquire), 0);
    }
}
