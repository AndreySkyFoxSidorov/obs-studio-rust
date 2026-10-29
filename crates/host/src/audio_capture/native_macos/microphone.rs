use super::*;
use crate::audio_capture::callback::{self, CallbackSender, CallbackStamp};
use block2::{DynBlock, RcBlock};
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2_audio_toolbox::*;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_core_audio_types::{AudioStreamPacketDescription, AudioTimeStamp, AudioTimeStampFlags};
use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

type InputBlock = dyn Fn(
    AudioQueueRef,
    AudioQueueBufferRef,
    NonNull<AudioTimeStamp>,
    u32,
    *const AudioStreamPacketDescription,
);
struct State {
    // Exactly one serial AudioQueue dispatch queue accesses this cell. Control
    // only releases it after Dispose(true) and that queue's synchronous barrier.
    sender: UnsafeCell<CallbackSender>,
    active: AtomicBool,
    error: AtomicI32,
    format: NativeAudioFormat,
    bytes_per_frame: usize,
    packet_bytes: usize,
}
struct Resources {
    queue: AudioQueueRef,
    state: Box<State>,
    _block: RcBlock<InputBlock>,
    dispatch: DispatchRetained<DispatchQueue>,
    receiver: CallbackReceiver,
}
struct Microphone {
    description: Option<std::sync::Arc<str>>,
    resources: Option<Resources>,
    limits: AudioCaptureLimits,
    device_id: u32,
    uid: String,
    follows_default: bool,
    next_device_check: std::time::Instant,
}

pub(super) fn open(
    binding: &AudioCaptureBinding,
    limits: AudioCaptureLimits,
) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
    let media_type = unsafe { AVMediaTypeAudio }.ok_or(AudioCaptureError::Unsupported(
        "AVMediaTypeAudio unavailable",
    ))?;
    if unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) }
        != AVAuthorizationStatus::Authorized
    {
        return Err(AudioCaptureError::PermissionDenied(
            "microphone authorization must already be granted; capture never prompts",
        ));
    }
    let (device_id, uid, format) = resolve_input(binding, limits)?;
    // The admission maximum is not the requested callback duration. Ask for
    // approximately 10 ms at the actual device rate, capped by the caller.
    let frames = (format.sample_rate.div_ceil(100) as usize)
        .min(limits.max_frames)
        .min(4096);
    let bytes_per_frame = usize::from(format.channels) * 4;
    let buffer_bytes = frames
        .checked_mul(bytes_per_frame)
        .ok_or(AudioCaptureError::Limit("AudioQueue buffer bytes"))?;
    if frames == 0 || buffer_bytes > limits.max_packet_bytes || buffer_bytes > 128 * 1024 {
        return Err(AudioCaptureError::Limit("AudioQueue buffer frames/bytes"));
    }
    let native_bytes = buffer_bytes
        .checked_mul(3)
        .ok_or(AudioCaptureError::Limit("AudioQueue native buffers"))?;
    let mut pool_limits = limits;
    pool_limits.max_native_buffer_bytes = limits
        .max_native_buffer_bytes
        .checked_sub(native_bytes)
        .ok_or(AudioCaptureError::Limit(
            "AudioQueue buffers plus callback pool",
        ))?;
    let (sender, receiver) = callback::pool(pool_limits)?;
    let state = Box::new(State {
        sender: UnsafeCell::new(sender),
        active: AtomicBool::new(true),
        error: AtomicI32::new(0),
        format,
        bytes_per_frame,
        packet_bytes: buffer_bytes,
    });
    let state_address = (&*state as *const State) as usize;
    let block: RcBlock<InputBlock> = RcBlock::new(
        move |queue, buffer, time: NonNull<AudioTimeStamp>, _packets, _descriptions| {
            // Raw address is stable for the entire registered callback lifetime.
            // Only this serial dispatch queue reads/mutates the producer.
            unsafe {
                receive(
                    &*(state_address as *const State),
                    queue,
                    buffer,
                    time.as_ref(),
                )
            };
        },
    );
    let dispatch = DispatchQueue::new("org.skylive.audio.microphone", None);
    let mut asbd = AudioStreamBasicDescription {
        mSampleRate: f64::from(format.sample_rate),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        mBytesPerPacket: bytes_per_frame as u32,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes_per_frame as u32,
        mChannelsPerFrame: u32::from(format.channels),
        mBitsPerChannel: 32,
        mReserved: 0,
    };
    let mut queue = ptr::null_mut();
    status("AudioQueueNewInputWithDispatchQueue", unsafe {
        AudioQueueNewInputWithDispatchQueue(
            NonNull::from(&mut queue),
            NonNull::from(&mut asbd),
            0,
            &dispatch,
            RcBlock::as_ptr(&block) as *mut DynBlock<InputBlock>,
        )
    })?;
    if queue.is_null() {
        return Err(AudioCaptureError::Invalid(
            "AudioQueue returned a null queue",
        ));
    }
    // Install the retirement guard before any subsequent fallible native call.
    let mut stream = Microphone {
        description: device_string(device_id, kAudioObjectPropertyName)
            .ok()
            .map(std::sync::Arc::from),
        resources: Some(Resources {
            queue,
            state,
            _block: block,
            dispatch,
            receiver,
        }),
        limits,
        device_id,
        uid: uid.clone(),
        follows_default: matches!(binding.target(), AudioTarget::Default),
        next_device_check: std::time::Instant::now() + std::time::Duration::from_secs(1),
    };
    let resources = stream.resources.as_mut().unwrap();
    let device = CFString::from_str(&uid);
    let mut device_ptr = &*device as *const CFString;
    status("AudioQueueSetProperty(CurrentDevice)", unsafe {
        AudioQueueSetProperty(
            queue,
            kAudioQueueProperty_CurrentDevice,
            NonNull::from(&mut device_ptr).cast(),
            size_of::<*const CFString>() as u32,
        )
    })?;
    let mut actual = MaybeUninit::<AudioStreamBasicDescription>::uninit();
    let mut size = size_of::<AudioStreamBasicDescription>() as u32;
    status("AudioQueueGetProperty(StreamDescription)", unsafe {
        AudioQueueGetProperty(
            queue,
            kAudioQueueProperty_StreamDescription,
            NonNull::new(actual.as_mut_ptr().cast()).unwrap(),
            NonNull::from(&mut size),
        )
    })?;
    if size as usize != size_of::<AudioStreamBasicDescription>() {
        return Err(AudioCaptureError::Invalid("AudioQueue actual ASBD size"));
    }
    let (actual_format, planar) = inspect_asbd(&unsafe { actual.assume_init() }, limits)?;
    if actual_format != format || planar {
        return Err(AudioCaptureError::Unsupported(
            "AudioQueue changed the requested input format",
        ));
    }
    for _ in 0..3 {
        let mut buffer = ptr::null_mut();
        status("AudioQueueAllocateBuffer", unsafe {
            AudioQueueAllocateBuffer(queue, buffer_bytes as u32, NonNull::from(&mut buffer))
        })?;
        if buffer.is_null() {
            return Err(AudioCaptureError::Invalid("null AudioQueue input buffer"));
        }
        status("AudioQueueEnqueueBuffer", unsafe {
            AudioQueueEnqueueBuffer(queue, buffer, 0, ptr::null())
        })?;
    }
    resources.state.active.store(true, Ordering::Release);
    status("AudioQueueStart", unsafe {
        AudioQueueStart(queue, ptr::null())
    })?;
    Ok(Box::new(stream))
}

unsafe fn receive(
    state: &State,
    queue: AudioQueueRef,
    buffer: AudioQueueBufferRef,
    time: &AudioTimeStamp,
) {
    if !state.active.load(Ordering::Acquire) {
        return;
    }
    let sender = unsafe { &mut *state.sender.get() };
    if buffer.is_null() {
        sender.reject();
        return;
    }
    let buffer_ref = unsafe { &*buffer };
    let bytes = buffer_ref.mAudioDataByteSize as usize;
    if bytes == 0
        || bytes > state.packet_bytes
        || bytes > buffer_ref.mAudioDataBytesCapacity as usize
        || bytes % state.bytes_per_frame != 0
    {
        sender.reject();
    } else {
        let frames = bytes / state.bytes_per_frame;
        let valid_time = time.mFlags.contains(AudioTimeStampFlags::HostTimeValid);
        let stamp = CallbackStamp {
            device: if valid_time {
                CallbackTime::MachTicks(time.mHostTime)
            } else {
                CallbackTime::Unavailable
            },
            received_ticks: unsafe { AudioGetCurrentHostTime() },
        };
        sender.submit(
            state.format,
            frames,
            stamp,
            crate::audio_capture::NativePacketFlags {
                timestamp_error: !valid_time,
                ..Default::default()
            },
            |output| {
                // read_unaligned also supports devices whose data address does not
                // meet Rust's f32 reference alignment. No borrowed f32 slice exists.
                let samples = buffer_ref.mAudioData.as_ptr().cast::<f32>();
                for (index, sample) in output.iter_mut().enumerate() {
                    *sample = unsafe { samples.add(index).read_unaligned() };
                }
            },
        );
    }
    if state.active.load(Ordering::Acquire) {
        let result = unsafe { AudioQueueEnqueueBuffer(queue, buffer, 0, ptr::null()) };
        if result != 0 {
            state.error.store(result, Ordering::Release);
        }
    }
}
impl NativeAudioStream for Microphone {
    fn description(&self) -> Option<std::sync::Arc<str>> {
        self.description.clone()
    }
    fn poll(&mut self) -> Result<Option<NativePcmBlock>, AudioCaptureError> {
        let resources = self.resources.as_mut().ok_or(AudioCaptureError::Closed)?;
        status(
            "AudioQueue input callback",
            resources.state.error.load(Ordering::Acquire),
        )?;
        if std::time::Instant::now() >= self.next_device_check {
            self.next_device_check = std::time::Instant::now() + std::time::Duration::from_secs(1);
            if self.follows_default {
                let default: u32 = scalar(
                    kAudioObjectSystemObject as u32,
                    kAudioHardwarePropertyDefaultInputDevice,
                )?;
                if default == 0
                    || device_string(default, kAudioDevicePropertyDeviceUID)? != self.uid
                {
                    return Err(AudioCaptureError::DeviceUnavailable(
                        "default microphone changed; reopen the default binding".into(),
                    ));
                }
            }
            if device_string(self.device_id, kAudioDevicePropertyDeviceUID)? != self.uid {
                return Err(AudioCaptureError::DeviceUnavailable(
                    "selected microphone identity changed".into(),
                ));
            }
            let mut actual = MaybeUninit::<AudioStreamBasicDescription>::uninit();
            let mut size = size_of::<AudioStreamBasicDescription>() as u32;
            status("AudioQueueGetProperty(StreamDescription)", unsafe {
                AudioQueueGetProperty(
                    resources.queue,
                    kAudioQueueProperty_StreamDescription,
                    NonNull::new(actual.as_mut_ptr().cast()).unwrap(),
                    NonNull::from(&mut size),
                )
            })?;
            if size as usize != size_of::<AudioStreamBasicDescription>() {
                return Err(AudioCaptureError::Invalid(
                    "AudioQueue actual ASBD size changed",
                ));
            }
            let (actual, planar) = inspect_asbd(&unsafe { actual.assume_init() }, self.limits)?;
            if actual != resources.state.format || planar {
                return Err(AudioCaptureError::Unsupported(
                    "AudioQueue stream format changed; reopen capture",
                ));
            }
        }
        poll_packet(&mut resources.receiver, self.limits, |time| match time {
            CallbackTime::MachTicks(ticks) => host_nanoseconds(ticks).map(Some),
            _ => Ok(None),
        })
    }
    fn close(&mut self) -> Result<(), AudioCaptureError> {
        let Some(resources) = self.resources.as_mut() else {
            return Ok(());
        };
        resources.state.active.store(false, Ordering::Release);
        // Stop failure is not permission to free a callback context. Dispose is
        // the final synchronous ownership barrier and may succeed after Stop
        // reports an already-stopped queue. Retain everything if it fails.
        let stop = unsafe { AudioQueueStop(resources.queue, true) };
        let dispose = unsafe { AudioQueueDispose(resources.queue, true) };
        if dispose != 0 {
            return status("AudioQueueDispose(true)", dispose);
        }
        resources.dispatch.exec_sync(|| {});
        self.resources.take();
        // Successful Dispose proves retirement, even if Stop was redundant.
        let _ = stop;
        Ok(())
    }
}
impl Drop for Microphone {
    fn drop(&mut self) {
        // Only the owner can drop this !Send stream. A native failure retains
        // its admission and lifetime, rather than freeing a live SDK callback.
        while self.close().is_err() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
