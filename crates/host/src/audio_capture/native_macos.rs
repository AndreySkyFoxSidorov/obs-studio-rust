//! macOS 14 AudioQueue input and ScreenCaptureKit system audio. All SDK setup,
//! clock conversion and retirement run on the owning audio worker.
use crate::audio_capture::callback::{CallbackReceiver, CallbackTime};
use crate::audio_capture::{
    AudioCapabilities, AudioCaptureError, AudioCaptureLimits, AudioDeviceInfo, NativeAudioBackend,
    NativeAudioFormat, NativeAudioStream, NativePcmBlock,
};
use objc2_core_audio::*;
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, kAudioFormatFlagIsBigEndian,
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, kAudioFormatFlagIsNonMixable,
    kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
};
use objc2_core_foundation::{CFRetained, CFString};
use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget};
use std::{
    marker::PhantomData,
    mem::{MaybeUninit, offset_of, size_of},
    ptr::{self, NonNull},
    rc::Rc,
};

#[path = "native_macos/microphone.rs"]
mod microphone;
#[path = "native_macos/system.rs"]
mod system;

pub fn capabilities() -> AudioCapabilities {
    AudioCapabilities {
        backend: "macos-audioqueue-screencapturekit",
        input: true,
        output: true,
        device_enumeration: true,
        exclude_own_audio: true,
        note: Some(
            "macOS 14+: existing microphone/Screen Recording authorization required; output DeviceUID loopback is unavailable",
        ),
    }
}
pub fn backend() -> Result<Box<dyn NativeAudioBackend>, AudioCaptureError> {
    host_timebase()?;
    // The marker deliberately keeps the SDK owner !Send and !Sync.
    Ok(Box::new(Backend {
        _owner: PhantomData,
    }))
}
struct Backend {
    _owner: PhantomData<Rc<()>>,
}
impl NativeAudioBackend for Backend {
    fn capabilities(&self) -> AudioCapabilities {
        capabilities()
    }
    fn host_now_ns(&self) -> Result<u64, AudioCaptureError> {
        host_nanoseconds(unsafe { AudioGetCurrentHostTime() })
    }
    fn enumerate(
        &mut self,
        flow: AudioFlow,
        limits: AudioCaptureLimits,
    ) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        if limits.max_devices == 0 {
            return Err(AudioCaptureError::Limit("audio device count"));
        }
        if flow == AudioFlow::Output {
            return Ok(vec![AudioDeviceInfo {
                target: AudioTarget::MacSystem,
                name: "macOS System Audio".into(),
                flow,
                is_default: true,
            }]);
        }
        let default: u32 = scalar(
            kAudioObjectSystemObject as u32,
            kAudioHardwarePropertyDefaultInputDevice,
        )?;
        let mut devices = Vec::new();
        for id in device_ids(limits)? {
            if input_channels(id, limits)? == 0 {
                continue;
            }
            if devices.len() == limits.max_devices {
                return Err(AudioCaptureError::Limit("input audio device count"));
            }
            devices.push(AudioDeviceInfo {
                target: AudioTarget::MacDevice {
                    uid: device_string(id, kAudioDevicePropertyDeviceUID)?,
                },
                name: device_string(id, kAudioObjectPropertyName)?,
                flow,
                is_default: id == default,
            });
        }
        Ok(devices)
    }
    fn open(
        &mut self,
        binding: &AudioCaptureBinding,
        limits: AudioCaptureLimits,
    ) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
        match (binding.flow(), binding.target()) {
            (AudioFlow::Input, AudioTarget::Default | AudioTarget::MacDevice { .. }) => {
                microphone::open(binding, limits)
            }
            (AudioFlow::Output, AudioTarget::Default | AudioTarget::MacSystem) => {
                system::open(binding, limits)
            }
            (AudioFlow::Output, AudioTarget::MacDevice { .. }) => {
                Err(AudioCaptureError::Unsupported(
                    "a CoreAudio output DeviceUID is not a ScreenCaptureKit loopback target",
                ))
            }
            _ => Err(AudioCaptureError::Unsupported(
                "audio binding belongs to another operating system",
            )),
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
struct HostTimebase {
    numer: u32,
    denom: u32,
}
unsafe extern "C-unwind" {
    fn mach_timebase_info(info: *mut HostTimebase) -> i32;
}
impl HostTimebase {
    fn nanoseconds(self, ticks: u64) -> Result<u64, AudioCaptureError> {
        if self.numer == 0 || self.denom == 0 {
            return Err(AudioCaptureError::Invalid("Mach host timebase ratio"));
        }
        let nanos = u128::from(ticks)
            .checked_mul(u128::from(self.numer))
            .ok_or(AudioCaptureError::TimeOverflow)?
            / u128::from(self.denom);
        u64::try_from(nanos).map_err(|_| AudioCaptureError::TimeOverflow)
    }
}
fn host_timebase() -> Result<HostTimebase, AudioCaptureError> {
    static RATIO: std::sync::OnceLock<Result<HostTimebase, AudioCaptureError>> =
        std::sync::OnceLock::new();
    RATIO
        .get_or_init(|| {
            let mut info = HostTimebase { numer: 0, denom: 0 };
            status("mach_timebase_info", unsafe {
                mach_timebase_info(&mut info)
            })?;
            info.nanoseconds(0)?;
            Ok(info)
        })
        .clone()
}
fn host_nanoseconds(ticks: u64) -> Result<u64, AudioCaptureError> {
    host_timebase()?.nanoseconds(ticks)
}

fn status(operation: &'static str, value: i32) -> Result<(), AudioCaptureError> {
    if value == 0 {
        Ok(())
    } else {
        Err(AudioCaptureError::Native {
            operation,
            detail: format!("OSStatus {value}"),
        })
    }
}
fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}
fn scalar<T: Copy>(id: u32, selector: u32) -> Result<T, AudioCaptureError> {
    let mut output = MaybeUninit::<T>::uninit();
    let mut bytes = u32::try_from(size_of::<T>())
        .map_err(|_| AudioCaptureError::Limit("CoreAudio scalar size"))?;
    let mut property = address(selector, kAudioObjectPropertyScopeGlobal);
    status("AudioObjectGetPropertyData", unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut property),
            0,
            ptr::null(),
            NonNull::from(&mut bytes),
            NonNull::new(output.as_mut_ptr().cast()).unwrap(),
        )
    })?;
    if bytes as usize != size_of::<T>() {
        return Err(AudioCaptureError::Invalid("CoreAudio scalar size changed"));
    }
    Ok(unsafe { output.assume_init() })
}
fn property_bytes(
    id: u32,
    selector: u32,
    scope: u32,
    cap: usize,
) -> Result<(Vec<usize>, usize), AudioCaptureError> {
    let mut property = address(selector, scope);
    let mut bytes = 0;
    status("AudioObjectGetPropertyDataSize", unsafe {
        AudioObjectGetPropertyDataSize(
            id,
            NonNull::from(&mut property),
            0,
            ptr::null(),
            NonNull::from(&mut bytes),
        )
    })?;
    if bytes as usize > cap {
        return Err(AudioCaptureError::Limit("CoreAudio property bytes"));
    }
    let words = (bytes as usize).div_ceil(size_of::<usize>());
    let mut output = vec![0usize; words.max(1)];
    let admitted = bytes;
    status("AudioObjectGetPropertyData", unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&mut property),
            0,
            ptr::null(),
            NonNull::from(&mut bytes),
            NonNull::new(output.as_mut_ptr().cast()).unwrap(),
        )
    })?;
    if bytes > admitted {
        return Err(AudioCaptureError::Invalid(
            "CoreAudio property grew during query",
        ));
    }
    Ok((output, bytes as usize))
}
fn device_ids(limits: AudioCaptureLimits) -> Result<Vec<u32>, AudioCaptureError> {
    // The system list includes output-only devices. Bound the entire query,
    // rather than truncating it and silently missing a selected input UID.
    let cap = limits
        .max_devices
        .checked_mul(size_of::<u32>())
        .ok_or(AudioCaptureError::Limit("audio device list"))?;
    let (data, bytes) = property_bytes(
        kAudioObjectSystemObject as u32,
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
        cap,
    )?;
    if bytes % size_of::<u32>() != 0 {
        return Err(AudioCaptureError::Invalid(
            "CoreAudio device array alignment",
        ));
    }
    Ok(
        unsafe {
            std::slice::from_raw_parts(data.as_ptr().cast::<u32>(), bytes / size_of::<u32>())
        }
        .to_vec(),
    )
}
fn device_string(id: u32, selector: u32) -> Result<String, AudioCaptureError> {
    let raw: *mut CFString = scalar(id, selector)?;
    // CoreAudio's CFString properties transfer a retain to their caller.
    let string = unsafe {
        CFRetained::from_raw(
            NonNull::new(raw).ok_or(AudioCaptureError::Invalid("null CoreAudio device string"))?,
        )
    };
    if string.length() < 0 || string.length() > 1024 {
        return Err(AudioCaptureError::Limit("CoreAudio device string"));
    }
    let text = string.to_string();
    if text.len() > 1024 || text.chars().any(char::is_control) {
        return Err(AudioCaptureError::Invalid("CoreAudio device string"));
    }
    Ok(text)
}
fn input_channels(id: u32, limits: AudioCaptureLimits) -> Result<u32, AudioCaptureError> {
    let (words, bytes) = property_bytes(
        id,
        kAudioDevicePropertyStreamConfiguration,
        kAudioObjectPropertyScopeInput,
        limits.max_native_buffer_bytes.min(64 * 1024),
    )?;
    if bytes < offset_of!(AudioBufferList, mBuffers) {
        return Err(AudioCaptureError::Invalid(
            "CoreAudio input buffer list header",
        ));
    }
    let list = words.as_ptr().cast::<AudioBufferList>();
    let count = unsafe { (*list).mNumberBuffers as usize };
    let required = count
        .checked_mul(size_of::<AudioBuffer>())
        .and_then(|n| n.checked_add(offset_of!(AudioBufferList, mBuffers)))
        .ok_or(AudioCaptureError::Limit("CoreAudio input buffer list"))?;
    if required > bytes {
        return Err(AudioCaptureError::Invalid(
            "CoreAudio input buffer list size",
        ));
    }
    let buffers = unsafe {
        std::slice::from_raw_parts(ptr::addr_of!((*list).mBuffers).cast::<AudioBuffer>(), count)
    };
    buffers.iter().try_fold(0u32, |sum, buffer| {
        sum.checked_add(buffer.mNumberChannels)
            .ok_or(AudioCaptureError::Limit("CoreAudio input channel count"))
    })
}
fn resolve_input(
    binding: &AudioCaptureBinding,
    limits: AudioCaptureLimits,
) -> Result<(u32, String, NativeAudioFormat), AudioCaptureError> {
    let id = match binding.target() {
        AudioTarget::Default => scalar(
            kAudioObjectSystemObject as u32,
            kAudioHardwarePropertyDefaultInputDevice,
        )?,
        AudioTarget::MacDevice { uid } => {
            let mut found = None;
            for id in device_ids(limits)? {
                if device_string(id, kAudioDevicePropertyDeviceUID)? == *uid {
                    found = Some(id);
                    break;
                }
            }
            found.ok_or_else(|| AudioCaptureError::DeviceUnavailable(uid.clone()))?
        }
        _ => {
            return Err(AudioCaptureError::Unsupported(
                "not a macOS microphone target",
            ));
        }
    };
    if id == 0 {
        return Err(AudioCaptureError::DeviceUnavailable(
            "no default input device".into(),
        ));
    }
    let rate: f64 = scalar(id, kAudioDevicePropertyNominalSampleRate)?;
    if !rate.is_finite() || rate.fract() != 0.0 || !(8000.0..=192000.0).contains(&rate) {
        return Err(AudioCaptureError::Unsupported(
            "non-integral or unsupported CoreAudio input rate",
        ));
    }
    let channels = u16::try_from(input_channels(id, limits)?)
        .map_err(|_| AudioCaptureError::Unsupported("CoreAudio input channel count"))?;
    let format = NativeAudioFormat {
        sample_rate: rate as u32,
        channels,
        channel_mask: 0,
    };
    format.validate(limits)?;
    Ok((
        id,
        device_string(id, kAudioDevicePropertyDeviceUID)?,
        format,
    ))
}
fn inspect_asbd(
    asbd: &AudioStreamBasicDescription,
    limits: AudioCaptureLimits,
) -> Result<(NativeAudioFormat, bool), AudioCaptureError> {
    let planar = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0;
    let format = NativeAudioFormat {
        sample_rate: asbd.mSampleRate as u32,
        channels: u16::try_from(asbd.mChannelsPerFrame)
            .map_err(|_| AudioCaptureError::Unsupported("native PCM channels"))?,
        channel_mask: 0,
    };
    format.validate(limits)?;
    let bytes_per_frame = if planar {
        4
    } else {
        u32::from(format.channels) * 4
    };
    if asbd.mFormatID != kAudioFormatLinearPCM
        || !asbd.mSampleRate.is_finite()
        || asbd.mSampleRate != f64::from(format.sample_rate)
        || asbd.mFormatFlags & (kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked)
            != (kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked)
        || asbd.mFormatFlags & kAudioFormatFlagIsBigEndian != 0
        || asbd.mBitsPerChannel != 32
        || asbd.mFormatFlags
            & !(kAudioFormatFlagIsFloat
                | kAudioFormatFlagIsPacked
                | kAudioFormatFlagIsNonInterleaved
                | kAudioFormatFlagIsNonMixable)
            != 0
        || asbd.mFramesPerPacket != 1
        || asbd.mBytesPerFrame != bytes_per_frame
        || asbd.mBytesPerPacket != bytes_per_frame
    {
        return Err(AudioCaptureError::Unsupported(
            "native audio is not packed little-endian Float32 PCM",
        ));
    }
    Ok((format, planar))
}
fn poll_packet(
    receiver: &mut CallbackReceiver,
    limits: AudioCaptureLimits,
    convert: impl FnOnce(CallbackTime) -> Result<Option<u64>, AudioCaptureError>,
) -> Result<Option<NativePcmBlock>, AudioCaptureError> {
    let Some(packet) = receiver.pop() else {
        return Ok(None);
    };
    let times = convert(packet.stamp.device).and_then(|device| {
        host_nanoseconds(packet.stamp.received_ticks).map(|received| (device, received))
    });
    let (device_timestamp_ns, host_received_ns) = match times {
        Ok(times) => times,
        Err(error) => {
            receiver.recycle(packet);
            return Err(error);
        }
    };
    let mut flags = packet.flags;
    flags.timestamp_error |= device_timestamp_ns.is_none();
    let mut samples = Vec::new();
    if samples.try_reserve_exact(packet.samples().len()).is_err()
        || samples.capacity() != packet.samples().len()
    {
        receiver.recycle(packet);
        return Err(AudioCaptureError::Limit("owned native PCM allocation"));
    }
    samples.extend_from_slice(packet.samples());
    let block = NativePcmBlock {
        format: packet.format,
        frames: packet.frames,
        samples,
        device_timestamp_ns,
        graph_timestamp_ns: None,
        directshow_clock: None,
        host_received_ns,
        device_frame_position: None,
        flags,
    };
    if !receiver.recycle(packet) {
        return Err(AudioCaptureError::Invalid(
            "callback packet recycle ownership",
        ));
    }
    block.validate(limits)?;
    Ok(Some(block))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mach_ratio_uses_checked_integer_math_without_a_native_clock() {
        assert_eq!(
            HostTimebase {
                numer: 125,
                denom: 3
            }
            .nanoseconds(24_000_000)
            .unwrap(),
            1_000_000_000
        );
        assert_eq!(
            HostTimebase {
                numer: 125,
                denom: 3
            }
            .nanoseconds(1)
            .unwrap(),
            41
        );
        assert_eq!(
            HostTimebase { numer: 1, denom: 1 }
                .nanoseconds(u64::MAX)
                .unwrap(),
            u64::MAX
        );
        assert_eq!(
            HostTimebase {
                numer: u32::MAX,
                denom: 1
            }
            .nanoseconds(u64::MAX),
            Err(AudioCaptureError::TimeOverflow)
        );
        assert!(HostTimebase { numer: 0, denom: 1 }.nanoseconds(1).is_err());
        assert!(HostTimebase { numer: 1, denom: 0 }.nanoseconds(1).is_err());
    }
    #[test]
    fn actual_pcm_layout_is_checked_without_native_calls() {
        let mut asbd = AudioStreamBasicDescription {
            mSampleRate: 48000.0,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 8,
            mFramesPerPacket: 1,
            mBytesPerFrame: 8,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        assert_eq!(
            inspect_asbd(&asbd, AudioCaptureLimits::default()).unwrap(),
            (
                NativeAudioFormat {
                    sample_rate: 48000,
                    channels: 2,
                    channel_mask: 0
                },
                false
            )
        );
        asbd.mFormatFlags |= kAudioFormatFlagIsNonInterleaved;
        assert!(inspect_asbd(&asbd, AudioCaptureLimits::default()).is_err());
        asbd.mBytesPerFrame = 4;
        asbd.mBytesPerPacket = 4;
        assert!(
            inspect_asbd(&asbd, AudioCaptureLimits::default())
                .unwrap()
                .1
        );
        asbd.mSampleRate = 48000.5;
        assert!(inspect_asbd(&asbd, AudioCaptureLimits::default()).is_err());
        asbd.mSampleRate = f64::NAN;
        assert!(inspect_asbd(&asbd, AudioCaptureLimits::default()).is_err());
    }
}
