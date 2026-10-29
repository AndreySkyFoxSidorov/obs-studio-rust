//! macOS 14+ AVFoundation persistent camera. Passive discovery never opens an
//! input, and authorization is checked without requesting a system prompt.
use super::super::*;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::{AnyObject, ProtocolObject},
};
use objc2_av_foundation::*;
use objc2_core_foundation::CFRetained;
use objc2_core_media::{
    CMSampleBuffer, CMTime, CMTimeFlags, CMVideoFormatDescriptionGetDimensions,
};
use objc2_core_video::*;
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSProcessInfo, NSString,
};
use rtrb::{Consumer, Producer, RingBuffer};
use skylive_core::VideoPixelFormat;
use skylive_media::{
    AlphaMode, PackedFrameLayout, PackedFrameLimits, PackedPixelFormat, RowOrder,
    ingest_packed_frame,
};
use std::{
    collections::HashSet,
    ptr::NonNull,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
    },
};
const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
pub(super) struct NativeBackend {
    _owner: Rc<()>,
}
impl NativeBackend {
    pub(super) fn new() -> Result<Self, CameraError> {
        if NSProcessInfo::processInfo()
            .operatingSystemVersion()
            .majorVersion
            < 14
        {
            return Err(CameraError::Unavailable(
                "camera adapter requires macOS14 or later",
            ));
        }
        Ok(Self {
            _owner: Rc::new(()),
        })
    }
}
fn preflight() -> Result<(), CameraError> {
    let video = unsafe { AVMediaTypeVideo }.ok_or(CameraError::Unavailable(
        "AVFoundation video media type is unavailable",
    ))?;
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(video) };
    if status == AVAuthorizationStatus::Authorized {
        let key = NSString::from_str("NSCameraUsageDescription");
        if NSBundle::mainBundle()
            .objectForInfoDictionaryKey(&key)
            .as_ref()
            .and_then(|value| value.downcast_ref::<NSString>())
            .is_none_or(|value| value.length() == 0)
        {
            return Err(CameraError::Unavailable(
                "macOS camera access requires an application bundle with NSCameraUsageDescription",
            ));
        }
        Ok(())
    } else if status == AVAuthorizationStatus::NotDetermined {
        Err(CameraError::PermissionRequired)
    } else {
        Err(CameraError::PermissionDenied)
    }
}
fn device(target: &VideoCaptureTarget) -> Result<Retained<AVCaptureDevice>, CameraError> {
    let VideoCaptureTarget::Macos { unique_id } = target else {
        return Err(CameraError::Unavailable(
            "camera identity requires another OS",
        ));
    };
    let id = NSString::from_str(unique_id);
    unsafe { AVCaptureDevice::deviceWithUniqueID(&id) }.ok_or(CameraError::DeviceNotFound)
}
impl NativeCameraBackend for NativeBackend {
    fn enumerate_devices(
        &mut self,
        limits: CameraLimits,
    ) -> Result<Vec<CameraDeviceInfo>, CameraError> {
        autoreleasepool(|_| {
            let types = NSArray::from_slice(&[
                unsafe { AVCaptureDeviceTypeBuiltInWideAngleCamera },
                unsafe { AVCaptureDeviceTypeExternal },
                unsafe { AVCaptureDeviceTypeContinuityCamera },
                unsafe { AVCaptureDeviceTypeDeskViewCamera },
            ]);
            let mut output = Vec::new();
            let mut seen = HashSet::new();
            for media in [unsafe { AVMediaTypeVideo }, unsafe { AVMediaTypeMuxed }] {
                let media = media.ok_or(CameraError::Unavailable(
                    "AVFoundation camera media type is unavailable",
                ))?;
                let discovery = unsafe {
                    AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(&types,Some(media),AVCaptureDevicePosition::Unspecified)
                };
                let devices = unsafe { discovery.devices() };
                if devices.count() > limits.max_devices {
                    return Err(CameraError::BudgetExceeded("camera device count"));
                }
                for i in 0..devices.count() {
                    let device = devices.objectAtIndex(i);
                    let id = unsafe { device.uniqueID() }.to_string();
                    if seen.insert(id.clone()) {
                        if output.len() >= limits.max_devices {
                            return Err(CameraError::BudgetExceeded("camera device union"));
                        }
                        output.push(CameraDeviceInfo {
                            target: VideoCaptureTarget::Macos { unique_id: id },
                            name: unsafe { device.localizedName() }.to_string(),
                        });
                    }
                }
            }
            Ok(output)
        })
    }
    fn enumerate_modes(
        &mut self,
        target: &VideoCaptureTarget,
        limits: CameraLimits,
    ) -> Result<Vec<CameraModeInfo>, CameraError> {
        preflight()?;
        autoreleasepool(|_| {
            let device = device(target)?;
            Ok(formats(&device, limits)?
                .into_iter()
                .map(|(_, info)| info)
                .collect())
        })
    }
    fn open(
        &mut self,
        binding: &VideoCaptureBinding,
        limits: CameraLimits,
    ) -> Result<Box<dyn NativeCameraStream>, CameraError> {
        preflight()?;
        if binding.mode().format() != VideoPixelFormat::Bgra8 {
            return Err(CameraError::Unavailable(
                "AVFoundation profile requires a native BGRA mode",
            ));
        }
        let bytes = checked_frame_bytes(binding.width(), binding.height(), limits)?;
        if bytes
            .checked_mul(3)
            .is_none_or(|n| n > limits.max_raw_bytes)
        {
            return Err(CameraError::BudgetExceeded("camera SDK/conversion buffers"));
        }
        autoreleasepool(|_| {
            let device = device(binding.target())?;
            let format = formats(&device, limits)?
                .into_iter()
                .find(|(_, info)| info.mode == Some(binding.mode()))
                .map(|(format, _)| format)
                .ok_or(CameraError::ModeNotFound)?;
            let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
                .map_err(|e| native_error("camera device input", &e))?;
            let session = unsafe { AVCaptureSession::new() };
            let output = unsafe { AVCaptureVideoDataOutput::new() };
            if !unsafe { session.canAddInput(&input) } || !unsafe { session.canAddOutput(&output) }
            {
                return Err(CameraError::Unavailable(
                    "camera input/output cannot join capture session",
                ));
            }
            unsafe {
                session.beginConfiguration();
                session.addInput(&input);
                session.addOutput(&output);
            }
            let configure = (|| {
                unsafe { device.lockForConfiguration() }
                    .map_err(|e| native_error("camera device configuration", &e))?;
                let guard = DeviceLock(device.clone());
                let duration = CMTime {
                    value: i64::from(binding.mode().fps_den()),
                    timescale: binding.mode().fps_num() as i32,
                    flags: CMTimeFlags::Valid,
                    epoch: 0,
                };
                unsafe {
                    device.setActiveFormat(&format);
                    device.setActiveVideoMinFrameDuration(duration);
                    device.setActiveVideoMaxFrameDuration(duration);
                }
                drop(guard);
                let actual = unsafe { device.activeFormat() };
                let description = unsafe { actual.formatDescription() };
                let dimensions = unsafe { CMVideoFormatDescriptionGetDimensions(&description) };
                if dimensions.width != binding.width() as i32
                    || dimensions.height != binding.height() as i32
                    || unsafe { description.media_sub_type() } != BGRA
                    || !same_time(unsafe { device.activeVideoMinFrameDuration() }, duration)
                    || !same_time(unsafe { device.activeVideoMaxFrameDuration() }, duration)
                {
                    return Err(CameraError::ModeNotFound);
                }
                Ok(())
            })();
            unsafe {
                session.commitConfiguration();
            }
            configure?;
            let number = NSNumber::numberWithUnsignedInt(BGRA);
            let key = NSString::from_str(&unsafe { kCVPixelBufferPixelFormatTypeKey }.to_string());
            let settings = NSDictionary::<NSString, AnyObject>::from_slices(&[&*key], &[&*number]);
            unsafe {
                output.setVideoSettings(Some(&settings));
                output.setAlwaysDiscardsLateVideoFrames(true);
            }
            let (producer, consumer) = RingBuffer::new(3);
            let state = Arc::new(CallbackState {
                producer: Mutex::new(producer),
                stopping: AtomicBool::new(false),
                error: AtomicI32::new(0),
                dropped: AtomicU64::new(0),
                max_bytes: limits.max_raw_bytes / 3,
                width: binding.width(),
                height: binding.height(),
            });
            let delegate = Delegate::new(state.clone());
            let queue = DispatchQueue::new("skylive.camera.video", None);
            unsafe {
                output.setSampleBufferDelegate_queue(
                    Some(ProtocolObject::from_ref(&*delegate)),
                    Some(&queue),
                );
                session.startRunning();
            }
            if !unsafe { session.isRunning() } {
                unsafe {
                    output.setSampleBufferDelegate_queue(None, None);
                }
                queue.exec_sync(|| {});
                unsafe {
                    session.stopRunning();
                }
                let mut consumer = consumer;
                while consumer.pop().is_ok() {}
                return Err(CameraError::DeviceBusy);
            }
            Ok(Box::new(NativeStream {
                session,
                output,
                _input: input,
                _device: device,
                _delegate: delegate,
                queue,
                state,
                consumer,
                mode: binding.mode(),
                stopped: false,
                _owner: Rc::new(()),
            }) as Box<dyn NativeCameraStream>)
        })
    }
}
struct DeviceLock(Retained<AVCaptureDevice>);
impl Drop for DeviceLock {
    fn drop(&mut self) {
        unsafe {
            self.0.unlockForConfiguration();
        }
    }
}
fn same_time(a: CMTime, b: CMTime) -> bool {
    a.timescale > 0
        && b.timescale > 0
        && i128::from(a.value) * i128::from(b.timescale)
            == i128::from(b.value) * i128::from(a.timescale)
}
fn formats(
    device: &AVCaptureDevice,
    limits: CameraLimits,
) -> Result<Vec<(Retained<AVCaptureDeviceFormat>, CameraModeInfo)>, CameraError> {
    let formats = unsafe { device.formats() };
    if formats.count() > limits.max_modes {
        return Err(CameraError::BudgetExceeded("camera native format count"));
    }
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for i in 0..formats.count() {
        let format = formats.objectAtIndex(i);
        let description = unsafe { format.formatDescription() };
        let dimensions = unsafe { CMVideoFormatDescriptionGetDimensions(&description) };
        let subtype = unsafe { description.media_sub_type() };
        let ranges = unsafe { format.videoSupportedFrameRateRanges() };
        if ranges.count() > 64 {
            return Err(CameraError::BudgetExceeded("camera FPS range count"));
        }
        for r in 0..ranges.count() {
            let range = ranges.objectAtIndex(r);
            for time in [unsafe { range.minFrameDuration() }, unsafe {
                range.maxFrameDuration()
            }] {
                let fps_num = u32::try_from(time.timescale).unwrap_or(0);
                let fps_den = u32::try_from(time.value).unwrap_or(0);
                let width = u32::try_from(dimensions.width).unwrap_or(0);
                let height = u32::try_from(dimensions.height).unwrap_or(0);
                if !seen.insert((width, height, fps_num, fps_den, subtype)) {
                    continue;
                }
                if output.len() >= limits.max_modes {
                    return Err(CameraError::BudgetExceeded("camera native mode count"));
                }
                let mode = if subtype == BGRA {
                    VideoCaptureMode::new(width, height, fps_num, fps_den, VideoPixelFormat::Bgra8)
                        .ok()
                } else {
                    None
                };
                output.push((format.clone(),CameraModeInfo{width,height,fps_num,fps_den,native_subtype:format!("0x{subtype:08X}"),mode,unsupported_reason:if mode.is_some(){None}else{Some("native camera format is outside the BGRA profile; no implicit conversion")}}));
            }
        }
    }
    Ok(output)
}

struct Sample(CFRetained<CMSampleBuffer>);
// AVFoundation explicitly permits retaining CMSampleBuffer beyond its serial
// delegate callback. CVPixelBuffer access is locked on the consumer owner.
unsafe impl Send for Sample {}
struct CallbackState {
    producer: Mutex<Producer<Sample>>,
    stopping: AtomicBool,
    error: AtomicI32,
    dropped: AtomicU64,
    max_bytes: usize,
    width: u32,
    height: u32,
}
struct DelegateIvars {
    state: Arc<CallbackState>,
}
define_class!(
    #[unsafe(super=NSObject)]
    #[name = "SkyLiveCameraVideoDelegate"]
    #[ivars=DelegateIvars]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for Delegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        unsafe fn receive(
            &self,
            _output: &AVCaptureOutput,
            sample: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            let state = &self.ivars().state;
            if state.stopping.load(Ordering::Acquire) {
                return;
            }
            let Some(image) = (unsafe { sample.image_buffer() }) else {
                state.error.store(1, Ordering::Release);
                return;
            };
            // The SDK's borrowed sample still owns this image; this temporary retain
            // cannot be its final release on the callback.
            if CVPixelBufferGetWidth(&image) != state.width as usize
                || CVPixelBufferGetHeight(&image) != state.height as usize
                || CVPixelBufferGetPixelFormatType(&image) != BGRA
                || CVPixelBufferGetDataSize(&image) > state.max_bytes
            {
                state.error.store(1, Ordering::Release);
                return;
            }
            let Ok(mut producer) = state.producer.try_lock() else {
                state.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            };
            let Ok(mut chunk) = producer.write_chunk_uninit(1) else {
                state.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            };
            chunk.as_mut_slices().0[0]
                .write(Sample(unsafe { CFRetained::retain(NonNull::from(sample)) }));
            unsafe {
                chunk.commit_all();
            }
        }
        #[unsafe(method(captureOutput:didDropSampleBuffer:fromConnection:))]
        unsafe fn dropped(
            &self,
            _output: &AVCaptureOutput,
            _sample: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            self.ivars().state.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
);
impl Delegate {
    fn new(state: Arc<CallbackState>) -> Retained<Self> {
        let allocated = Self::alloc().set_ivars(DelegateIvars { state });
        unsafe { msg_send![super(allocated), init] }
    }
}
struct NativeStream {
    session: Retained<AVCaptureSession>,
    output: Retained<AVCaptureVideoDataOutput>,
    _input: Retained<AVCaptureDeviceInput>,
    _device: Retained<AVCaptureDevice>,
    _delegate: Retained<Delegate>,
    queue: DispatchRetained<DispatchQueue>,
    state: Arc<CallbackState>,
    consumer: Consumer<Sample>,
    mode: VideoCaptureMode,
    stopped: bool,
    _owner: Rc<()>,
}
impl NativeCameraStream for NativeStream {
    fn enumerate_modes(&mut self,limits:CameraLimits)->Result<Vec<CameraModeInfo>,CameraError> {
        if self.stopped {return Err(CameraError::Closed);}
        autoreleasepool(|_|formats(&self._device,limits).map(|types|types.into_iter().map(|(_,info)|info).collect()))
    }
    fn poll(&mut self, limits: CameraLimits) -> Result<Option<NativeVideoFrame>, CameraError> {
        if self.stopped {
            return Err(CameraError::Closed);
        }
        if self.state.error.swap(0, Ordering::AcqRel) != 0 {
            return Err(CameraError::InvalidFrame(
                "camera callback changed native mode or exceeded its byte budget",
            ));
        }
        if !unsafe { self.session.isRunning() } || !unsafe { self._device.isConnected() } {
            return Err(CameraError::DeviceLost);
        }
        let Ok(sample) = self.consumer.pop() else {
            return Ok(None);
        };
        let time = unsafe { sample.0.output_presentation_time_stamp() };
        if time.timescale <= 0
            || time.epoch != 0
            || !time.flags.contains(CMTimeFlags::Valid)
            || time.flags.intersects(CMTimeFlags::ImpliedValueFlagsMask)
        {
            return Err(CameraError::InvalidFrame("camera sample timestamp"));
        }
        let presentation_ns =
            (i128::from(time.value) * 1_000_000_000).div_euclid(i128::from(time.timescale));
        let image = copy_sample(&sample.0, self.mode, limits)?;
        Ok(Some(NativeVideoFrame {
            image,
            presentation_ns,
            device_qpc_ns: None,
            directshow_clock: None,
            discontinuity: false,
            dropped_frames: self.state.dropped.swap(0, Ordering::AcqRel),
        }))
    }
    fn stop(&mut self) -> Result<bool, CameraError> {
        if self.stopped {
            return Ok(true);
        }
        self.state.stopping.store(true, Ordering::Release);
        unsafe {
            self.output.setSampleBufferDelegate_queue(None, None);
            self.session.stopRunning();
        }
        self.queue.exec_sync(|| {});
        while self.consumer.pop().is_ok() {}
        self.stopped = true;
        Ok(true)
    }
}
impl Drop for NativeStream {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
struct PixelLock(CFRetained<CVImageBuffer>);
impl Drop for PixelLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CVPixelBufferUnlockBaseAddress(&self.0, CVPixelBufferLockFlags::ReadOnly);
        }
    }
}
fn copy_sample(
    sample: &CMSampleBuffer,
    mode: VideoCaptureMode,
    limits: CameraLimits,
) -> Result<RgbaFrame, CameraError> {
    let image = unsafe { sample.image_buffer() }.ok_or(CameraError::InvalidFrame(
        "camera sample has no image buffer",
    ))?;
    if CVPixelBufferGetWidth(&image) != mode.width() as usize
        || CVPixelBufferGetHeight(&image) != mode.height() as usize
        || CVPixelBufferGetPixelFormatType(&image) != BGRA
        || CVPixelBufferIsPlanar(&image)
    {
        return Err(CameraError::InvalidFrame("camera BGRA dimensions/format"));
    }
    if unsafe { CVPixelBufferLockBaseAddress(&image, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return Err(CameraError::InvalidFrame("camera pixel lock"));
    }
    let guard = PixelLock(image);
    let stride = CVPixelBufferGetBytesPerRow(&guard.0);
    let length = CVPixelBufferGetDataSize(&guard.0);
    let ptr = CVPixelBufferGetBaseAddress(&guard.0);
    let bytes = stride
        .checked_mul(mode.height() as usize)
        .ok_or(CameraError::BudgetExceeded("camera stride overflow"))?;
    if ptr.is_null()
        || length > limits.max_raw_bytes
        || bytes > length
        || stride < mode.width() as usize * 4
    {
        return Err(CameraError::InvalidFrame("camera pixel buffer bounds"));
    }
    let pixels = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), bytes) };
    ingest_packed_frame(
        pixels,
        PackedFrameLayout {
            width: mode.width(),
            height: mode.height(),
            stride_bytes: stride,
            format: PackedPixelFormat::Bgra8,
            row_order: RowOrder::TopDown,
            alpha: AlphaMode::Straight,
        },
        PackedFrameLimits {
            max_input_bytes: limits.max_raw_bytes,
            max_output_bytes: limits.max_frame_bytes,
            max_pixel_visits: u64::from(mode.width()) * u64::from(mode.height()) * 2,
        },
    )
    .map_err(|_| CameraError::InvalidFrame("camera BGRA pixels"))
}
fn native_error(operation: &'static str, error: &objc2_foundation::NSError) -> CameraError {
    CameraError::Native {
        operation,
        detail: format!(
            "{} ({}): {}",
            &*error.domain(),
            error.code(),
            &*error.localizedDescription()
        ),
    }
}
