use super::*;
use crate::audio_capture::{
    NativePacketFlags,
    callback::{self, CallbackSender, CallbackStamp},
};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
};
use objc2_core_graphics::CGPreflightScreenCaptureAccess;
use objc2_core_media::{
    CMAudioFormatDescriptionGetStreamBasicDescription, CMBlockBuffer, CMClock, CMFormatDescription,
    CMSampleBuffer, CMSyncConvertTime, CMTime, CMTimeFlags,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol, NSProcessInfo};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate,
    SCStreamOutput, SCStreamOutputType, SCWindow,
};
use std::{
    cell::UnsafeCell,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering},
        mpsc,
    },
};

// Borrowed Get APIs avoid a retain/release of format descriptions in the PCM
// callback. The SDK-owned sample buffer keeps all returned memory alive.
unsafe extern "C-unwind" {
    #[link_name = "CMSampleBufferGetFormatDescription"]
    fn sample_format(sample: &CMSampleBuffer) -> *const CMFormatDescription;
    #[link_name = "CMSampleBufferGetDataBuffer"]
    fn sample_data(sample: &CMSampleBuffer) -> *const CMBlockBuffer;
}
struct State {
    sender: UnsafeCell<CallbackSender>,
    limits: AudioCaptureLimits,
    active: AtomicBool,
    packet_error: AtomicU8,
    scratch: UnsafeCell<Box<[u8]>>,
}
struct StreamStatus {
    stopped: AtomicBool,
    error: AtomicI64,
}
struct OutputIvars {
    state: usize,
    status: Arc<StreamStatus>,
}
define_class!(
    // NSObject imposes no subclassing requirements. The pointer is owned by
    // Resources and outlives output registration, stop and both queue barriers.
    #[unsafe(super = NSObject)]
    #[name = "SkyLiveNativeAudioOutput"]
    #[thread_kind = AnyThread]
    #[ivars = OutputIvars]
    struct Output;
    unsafe impl NSObjectProtocol for Output {}
    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Audio {
                return;
            }
            let state = unsafe { &*(self.ivars().state as *const State) };
            if state.active.load(Ordering::Acquire) {
                unsafe { receive(state, sample) };
            }
        }
    }
    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn stopped(&self, _stream: &SCStream, error: &NSError) {
            // SCStream delegates need not run on the PCM output queue. Keep
            // their tiny atomic status owned by the delegate itself, never a
            // raw pointer into the PCM pool's independently retired state.
            self.ivars()
                .status
                .error
                .store(error.code() as i64, Ordering::Release);
            self.ivars().status.stopped.store(true, Ordering::Release);
        }
    }
);
impl Output {
    fn new(state: &State, status: Arc<StreamStatus>) -> Retained<Self> {
        let allocated = Self::alloc().set_ivars(OutputIvars {
            state: (state as *const State) as usize,
            status,
        });
        unsafe { msg_send![super(allocated), init] }
    }
}
struct Resources {
    stream: Retained<SCStream>,
    output: Retained<Output>,
    queue: DispatchRetained<DispatchQueue>,
    state: Box<State>,
    status: Arc<StreamStatus>,
    receiver: CallbackReceiver,
    clock: Option<Retained<CMClock>>,
    started: bool,
    audio_registered: bool,
    screen_registered: bool,
}
struct SystemAudio {
    resources: Option<Resources>,
    limits: AudioCaptureLimits,
}

pub(super) fn open(
    binding: &AudioCaptureBinding,
    limits: AudioCaptureLimits,
) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
    if NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        < 14
    {
        return Err(AudioCaptureError::Unsupported(
            "ScreenCaptureKit audio requires macOS 14 or later",
        ));
    }
    if !CGPreflightScreenCaptureAccess() {
        return Err(AudioCaptureError::PermissionDenied(
            "Screen Recording authorization must already be granted; system audio capture never prompts",
        ));
    }
    let mut pool_limits = limits;
    let scratch_bytes = limits.max_packet_bytes.min(128 * 1024);
    pool_limits.max_native_buffer_bytes = limits
        .max_native_buffer_bytes
        .checked_sub(scratch_bytes + 3 * 2 * 2 * 4)
        .ok_or(AudioCaptureError::Limit("SCK sidecar plus callback pool"))?;
    let (sender, receiver) = callback::pool(pool_limits)?;
    let state = Box::new(State {
        sender: UnsafeCell::new(sender),
        limits,
        active: AtomicBool::new(true),
        packet_error: AtomicU8::new(0),
        scratch: UnsafeCell::new(vec![0; scratch_bytes].into_boxed_slice()),
    });
    let status = Arc::new(StreamStatus {
        stopped: AtomicBool::new(false),
        error: AtomicI64::new(0),
    });
    let output = Output::new(&state, status.clone());
    let queue = DispatchQueue::new("org.skylive.audio.system", None);
    let filter = content_filter()?;
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setCapturesAudio(true);
        config.setSampleRate(48000);
        config.setChannelCount(if limits.max_channels >= 2 { 2 } else { 1 });
        config.setExcludesCurrentProcessAudio(binding.exclude_own_audio());
        // The SCStream requires a video output; it is a discarded 2x2, 1fps
        // BGRA sidecar, never a hidden full-resolution display capture.
        config.setWidth(2);
        config.setHeight(2);
        config.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
        config.setShowsCursor(false);
        config.setQueueDepth(3);
        config.setMinimumFrameInterval(CMTime {
            value: 1,
            timescale: 1,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        });
    }
    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &config,
            Some(ProtocolObject::from_ref(&*output)),
        )
    };
    let mut owner = SystemAudio {
        resources: Some(Resources {
            stream,
            output,
            queue,
            state,
            status,
            receiver,
            clock: None,
            started: false,
            audio_registered: false,
            screen_registered: false,
        }),
        limits,
    };
    let resources = owner.resources.as_mut().unwrap();
    unsafe {
        resources
            .stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*resources.output),
                SCStreamOutputType::Audio,
                Some(&resources.queue),
            )
    }
    .map_err(|e| native_error("SCStream.addAudioOutput", &e))?;
    resources.audio_registered = true;
    unsafe {
        resources
            .stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*resources.output),
                SCStreamOutputType::Screen,
                Some(&resources.queue),
            )
    }
    .map_err(|e| native_error("SCStream.addScreenOutput", &e))?;
    resources.screen_registered = true;
    completion(&resources.stream, true)?;
    resources.started = true;
    resources.clock = unsafe { resources.stream.synchronizationClock() };
    if resources.clock.is_none() {
        return Err(AudioCaptureError::Invalid(
            "SCStream has no synchronization clock",
        ));
    }
    Ok(Box::new(owner))
}

fn native_error(operation: &'static str, error: &NSError) -> AudioCaptureError {
    AudioCaptureError::Native {
        operation,
        detail: format!("NSError {}: {}", error.code(), error.localizedDescription()),
    }
}
fn content_filter() -> Result<Retained<SCContentFilter>, AudioCaptureError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = if let Some(error) = unsafe { error.as_ref() } {
                Err(native_error("SCShareableContent", error))
            } else {
                unsafe { Retained::retain(content) }
                    .map(|content| Retained::into_raw(content) as usize)
                    .ok_or(AudioCaptureError::Invalid("null SCShareableContent"))
            };
            // A retained, immutable content descriptor transfers ownership exactly
            // once to the waiting control owner; no SCStream or producer crosses.
            if let Err(mpsc::SendError(Ok(address))) = sender.send(result) {
                unsafe {
                    drop(Retained::from_raw(address as *mut SCShareableContent));
                }
            }
        },
    );
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true, true, &block);
    }
    let address = receiver
        .recv()
        .map_err(|_| AudioCaptureError::Disconnected)??;
    let content = unsafe { Retained::from_raw(address as *mut SCShareableContent) }
        .ok_or(AudioCaptureError::Invalid("content ownership transfer"))?;
    let displays = unsafe { content.displays() };
    let display = displays.firstObject().ok_or_else(|| {
        AudioCaptureError::DeviceUnavailable("no display for system-audio SCStream".into())
    })?;
    Ok(unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &display,
            &NSArray::<SCWindow>::new(),
        )
    })
}
fn completion(stream: &SCStream, start: bool) -> Result<(), AudioCaptureError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let operation = if start {
        "SCStream.startCapture"
    } else {
        "SCStream.stopCapture"
    };
    let block = RcBlock::new(move |error: *mut NSError| {
        let result =
            unsafe { error.as_ref() }.map_or(Ok(()), |error| Err(native_error(operation, error)));
        let _ = sender.send(result);
    });
    unsafe {
        if start {
            stream.startCaptureWithCompletionHandler(Some(&block));
        } else {
            stream.stopCaptureWithCompletionHandler(Some(&block));
        }
    }
    // A timeout must not release admission or free registered callbacks while
    // an uncancellable native completion is still pending.
    receiver
        .recv()
        .map_err(|_| AudioCaptureError::Disconnected)?
}

unsafe fn receive(state: &State, sample: &CMSampleBuffer) {
    // Only the selected serial sampleHandlerQueue may access this producer.
    let sender = unsafe { &mut *state.sender.get() };
    let description = unsafe { sample_format(sample).as_ref() };
    let Some(description) = description else {
        state.packet_error.store(1, Ordering::Release);
        sender.reject();
        return;
    };
    let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(description).as_ref() };
    let Some(asbd) = asbd else {
        state.packet_error.store(1, Ordering::Release);
        sender.reject();
        return;
    };
    let Ok((mut format, planar)) = inspect_asbd(asbd, state.limits) else {
        state.packet_error.store(1, Ordering::Release);
        sender.reject();
        return;
    };
    format.channel_mask = match format.channels {
        1 => 4,
        2 => 3,
        _ => 0,
    };
    let frames = unsafe { sample.num_samples() };
    if frames <= 0 || frames as usize > state.limits.max_frames || frames > 4096 {
        state.packet_error.store(3, Ordering::Release);
        sender.reject();
        return;
    }
    let frames = frames as usize;
    let channels = usize::from(format.channels);
    let Some(total_bytes) = frames.checked_mul(channels).and_then(|n| n.checked_mul(4)) else {
        state.packet_error.store(3, Ordering::Release);
        sender.reject();
        return;
    };
    if total_bytes > state.limits.max_packet_bytes || total_bytes > 128 * 1024 {
        state.packet_error.store(3, Ordering::Release);
        sender.reject();
        return;
    }
    let Some(data) = (unsafe { sample_data(sample).as_ref() }) else {
        state.packet_error.store(2, Ordering::Release);
        sender.reject();
        return;
    };
    if unsafe { data.data_length() } != total_bytes {
        state.packet_error.store(2, Ordering::Release);
        sender.reject();
        return;
    }
    let mut contiguous = 0;
    let mut total = 0;
    let mut bytes = ptr::null_mut();
    let result = unsafe { data.data_pointer(0, &mut contiguous, &mut total, &mut bytes) };
    if result != 0 || total != total_bytes {
        state.packet_error.store(2, Ordering::Release);
        sender.reject();
        return;
    }
    if contiguous < total_bytes || bytes.is_null() {
        // CoreMedia's bounded copy handles segmented storage without creating
        // or retaining a block wrapper. The scratch was allocated on the owner.
        let scratch = unsafe { &mut *state.scratch.get() };
        let result = unsafe {
            data.copy_data_bytes(
                0,
                total_bytes,
                NonNull::new(scratch.as_mut_ptr().cast()).unwrap(),
            )
        };
        if result != 0 {
            state.packet_error.store(2, Ordering::Release);
            sender.reject();
            return;
        }
        bytes = scratch.as_mut_ptr().cast();
    }
    let time = unsafe { sample.output_presentation_time_stamp() };
    let stamp = CallbackStamp {
        device: CallbackTime::CoreMedia {
            value: time.value,
            timescale: time.timescale,
            flags: time.flags.0,
            epoch: time.epoch,
        },
        received_ticks: unsafe { AudioGetCurrentHostTime() },
    };
    sender.submit(
        format,
        frames,
        stamp,
        NativePacketFlags::default(),
        |output| {
            // The exact-length non-interleaved representation has consecutive
            // channel planes, matching OBS's SCK source contract. No padding or
            // guessed channel stride is accepted.
            unsafe { copy_samples(bytes.cast(), frames, channels, planar, output) };
        },
    );
}
unsafe fn copy_samples(
    bytes: *const u8,
    frames: usize,
    channels: usize,
    planar: bool,
    output: &mut [f32],
) {
    for (index, target) in output.iter_mut().enumerate() {
        let offset = if planar {
            (index % channels) * frames + index / channels
        } else {
            index
        };
        *target = unsafe { bytes.cast::<f32>().add(offset).read_unaligned() };
    }
}
fn time_ns(time: CMTime) -> Option<u64> {
    if !time.flags.contains(CMTimeFlags::Valid)
        || time.flags.intersects(CMTimeFlags::ImpliedValueFlagsMask)
        || time.value < 0
        || time.timescale <= 0
        || time.epoch != 0
    {
        return None;
    }
    u64::try_from((time.value as u128).checked_mul(1_000_000_000)? / time.timescale as u128).ok()
}
impl NativeAudioStream for SystemAudio {
    fn description(&self) -> Option<std::sync::Arc<str>> {
        Some(std::sync::Arc::from("macOS System Audio"))
    }
    fn poll(&mut self) -> Result<Option<NativePcmBlock>, AudioCaptureError> {
        let resources = self.resources.as_mut().ok_or(AudioCaptureError::Closed)?;
        match resources.state.packet_error.load(Ordering::Acquire) {
            0 => {}
            1 => {
                return Err(AudioCaptureError::Unsupported(
                    "ScreenCaptureKit delivered an unsupported actual audio format",
                ));
            }
            3 => {
                return Err(AudioCaptureError::Limit(
                    "ScreenCaptureKit actual packet exceeds admitted frames/bytes",
                ));
            }
            _ => {
                return Err(AudioCaptureError::Invalid(
                    "ScreenCaptureKit PCM byte count/storage does not match its actual format",
                ));
            }
        }
        let error = resources.status.error.load(Ordering::Acquire);
        if error != 0 {
            return Err(AudioCaptureError::Native {
                operation: "SCStream delegate",
                detail: format!("NSError code {error}"),
            });
        }
        let clock = resources.clock.as_ref();
        poll_packet(&mut resources.receiver, self.limits, |time| {
            let CallbackTime::CoreMedia {
                value,
                timescale,
                flags,
                epoch,
            } = time
            else {
                return Ok(None);
            };
            let source = CMTime {
                value,
                timescale,
                flags: CMTimeFlags(flags),
                epoch,
            };
            if !source.flags.contains(CMTimeFlags::Valid)
                || source.flags.intersects(CMTimeFlags::ImpliedValueFlagsMask)
                || timescale <= 0
            {
                return Ok(None);
            }
            let Some(clock) = clock else {
                return Ok(None);
            };
            let host = unsafe { CMClock::host_time_clock() };
            let converted = unsafe { CMSyncConvertTime(source, clock.as_ref(), host.as_ref()) };
            Ok(time_ns(converted))
        })
    }
    fn close(&mut self) -> Result<(), AudioCaptureError> {
        let Some(resources) = self.resources.as_mut() else {
            return Ok(());
        };
        resources.state.active.store(false, Ordering::Release);
        if resources.started {
            let result = completion(&resources.stream, false);
            // Delegate didStop is also an actual SDK acknowledgment. A generic
            // stop error alone is never interpreted as quiescence.
            if result.is_err() && !resources.status.stopped.load(Ordering::Acquire) {
                return result;
            }
            resources.started = false;
        }
        if resources.audio_registered {
            unsafe {
                resources.stream.removeStreamOutput_type_error(
                    ProtocolObject::from_ref(&*resources.output),
                    SCStreamOutputType::Audio,
                )
            }
            .map_err(|e| native_error("SCStream.removeAudioOutput", &e))?;
            resources.audio_registered = false;
        }
        if resources.screen_registered {
            unsafe {
                resources.stream.removeStreamOutput_type_error(
                    ProtocolObject::from_ref(&*resources.output),
                    SCStreamOutputType::Screen,
                )
            }
            .map_err(|e| native_error("SCStream.removeScreenOutput", &e))?;
            resources.screen_registered = false;
        }
        resources.queue.exec_sync(|| {});
        // Explicitly destroy the SCStream/delegate before freeing State.
        let resources = self.resources.take().unwrap();
        drop(resources.stream);
        drop(resources.output);
        drop(resources.state);
        drop(resources.receiver);
        drop(resources.queue);
        Ok(())
    }
}
impl Drop for SystemAudio {
    fn drop(&mut self) {
        while self.close().is_err() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rational_pts_is_checked_without_a_clock_or_device() {
        let t = |value, timescale, flags, epoch| CMTime {
            value,
            timescale,
            flags,
            epoch,
        };
        assert_eq!(time_ns(t(1, 48000, CMTimeFlags::Valid, 0)), Some(20833));
        assert_eq!(
            time_ns(t(48000, 48000, CMTimeFlags::Valid, 0)),
            Some(1_000_000_000)
        );
        assert_eq!(time_ns(t(i64::MAX, 1, CMTimeFlags::Valid, 0)), None);
        assert_eq!(time_ns(t(-1, 48000, CMTimeFlags::Valid, 0)), None);
        assert_eq!(time_ns(t(1, 0, CMTimeFlags::Valid, 0)), None);
        assert_eq!(
            time_ns(t(1, 1, CMTimeFlags::Indefinite | CMTimeFlags::Valid, 0)),
            None
        );
        assert_eq!(time_ns(t(1, 1, CMTimeFlags::Valid, 1)), None);
    }
    #[test]
    fn actual_planar_and_interleaved_bytes_have_literal_channel_oracles() {
        let planar = [1.0f32, 0.5, -1.0, -0.5];
        let mut output = [0.0; 4];
        unsafe { copy_samples(planar.as_ptr().cast(), 2, 2, true, &mut output) };
        assert_eq!(output, [1.0, -1.0, 0.5, -0.5]);
        let interleaved = [0.25f32, -0.25, 0.75, -0.75];
        unsafe { copy_samples(interleaved.as_ptr().cast(), 2, 2, false, &mut output) };
        assert_eq!(output, [0.25, -0.25, 0.75, -0.75]);
        let mut unaligned = [0u8; 17];
        for (index, sample) in interleaved.iter().enumerate() {
            unaligned[1 + index * 4..5 + index * 4].copy_from_slice(&sample.to_le_bytes());
        }
        unsafe { copy_samples(unaligned.as_ptr().add(1), 2, 2, false, &mut output) };
        assert_eq!(output, [0.25, -0.25, 0.75, -0.75]);
    }
}
