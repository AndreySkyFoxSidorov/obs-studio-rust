//! Worker-local WASAPI shared capture. No device or COM work in the factory.
//! See doc/migration/windows-audio-capture.md before changing ownership/limits.
use super::super::{
    AudioCapabilities, AudioCaptureError, AudioCaptureLimits, AudioDeviceInfo, NativeAudioBackend,
    NativeAudioFormat, NativeAudioStream, NativePacketFlags, NativePcmBlock, PcmEncoding,
    decode_pcm,
};
use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget};
use std::{cell::Cell, ffi::c_void, mem::size_of, ptr, rc::Rc};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::E_ACCESSDENIED,
        Media::Audio::{
            AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
            AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_E_DEVICE_INVALIDATED,
            AUDCLNT_E_NOT_INITIALIZED, AUDCLNT_E_RESOURCES_INVALIDATED,
            AUDCLNT_E_SERVICE_NOT_RUNNING, AUDCLNT_E_UNSUPPORTED_FORMAT, AUDCLNT_S_BUFFER_EMPTY,
            AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE_ACTIVE, EDataFlow,
            IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, IMMEndpoint,
            MMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture, eCommunications,
            eConsole, eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize, STGM_READ,
            },
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        },
    },
    core::{GUID, Interface, PCWSTR, PROPVARIANT},
};

#[path = "process_loopback.rs"]
mod process_loopback;
#[path = "obs_window_rating.rs"]
mod obs_window_rating;

const MAX_ID_BYTES: usize = 1024;
const MAX_NAME_BYTES: usize = 1024;
const WAVE_PCM: u16 = 1;
const WAVE_FLOAT: u16 = 3;
const WAVE_EXTENSIBLE: u16 = 0xfffe;
const EXTENSION_BYTES: u16 = (size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()) as u16;
const SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
const ENDPOINT_NOT_FOUND: i32 = 0x80070490u32 as i32;
// A bounded worker polls this shared buffer; no event/render helper is opened.
const CLIENT_DURATION_100NS: i64 = 1_000_000; // 100 ms

pub(super) fn capabilities() -> AudioCapabilities {
    AudioCapabilities {
        backend: "windows-wasapi",
        input: true,
        output: true,
        device_enumeration: true,
        exclude_own_audio: false,
        note: Some(
            "Shared-mode microphone/endpoint loopback; separate Windows build 20348+ application process-tree capture, explicit 48 kHz stereo float with mixer conversion; no process exclusion or automatic application rebinding",
        ),
    }
}
pub(super) fn backend() -> Result<Box<dyn NativeAudioBackend>, AudioCaptureError> {
    Ok(Box::new(WasapiBackend {
        enumerator: None,
        apartment: None,
        frequency: Cell::new(None),
        process_activations: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    }))
}

struct ComApartment;
impl ComApartment {
    fn new() -> Result<Rc<Self>, AudioCaptureError> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|error| native_error("CoInitializeEx(MTA)", error))?;
        // S_FALSE still acquired a COM initialization reference.
        Ok(Rc::new(Self))
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

// Field order releases interfaces before the final COM reference. Rc makes
// this type !Send without an unsafe Send implementation or apartment marshal.
struct WasapiBackend {
    enumerator: Option<IMMDeviceEnumerator>,
    apartment: Option<Rc<ComApartment>>,
    frequency: Cell<Option<u64>>,
    process_activations: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl WasapiBackend {
    fn initialize(&mut self) -> Result<(), AudioCaptureError> {
        if self.enumerator.is_none() {
            let apartment = ComApartment::new()?;
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                    .map_err(|error| native_error("CoCreateInstance(MMDeviceEnumerator)", error))?;
            self.enumerator = Some(enumerator);
            self.apartment = Some(apartment);
        }
        Ok(())
    }
    fn frequency(&self) -> Result<u64, AudioCaptureError> {
        if let Some(frequency) = self.frequency.get() {
            return Ok(frequency);
        }
        let mut frequency = 0i64;
        unsafe { QueryPerformanceFrequency(&mut frequency) }
            .map_err(|error| native_error("QueryPerformanceFrequency", error))?;
        let frequency = u64::try_from(frequency)
            .ok()
            .filter(|f| *f != 0)
            .ok_or(AudioCaptureError::Invalid("QPC frequency is not positive"))?;
        self.frequency.set(Some(frequency));
        Ok(frequency)
    }
}
impl NativeAudioBackend for WasapiBackend {
    fn capabilities(&self) -> AudioCapabilities {
        capabilities()
    }
    fn host_now_ns(&self) -> Result<u64, AudioCaptureError> {
        host_now_ns(self.frequency()?)
    }
    fn enumerate(
        &mut self,
        flow: AudioFlow,
        limits: AudioCaptureLimits,
    ) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        if limits.max_devices == 0 || limits.max_devices > 64 {
            return Err(AudioCaptureError::Limit("audio endpoint enumeration count"));
        }
        self.initialize()?;
        let enumerator = self
            .enumerator
            .as_ref()
            .ok_or(AudioCaptureError::Invalid("missing WASAPI enumerator"))?;
        let default =
            match unsafe { enumerator.GetDefaultAudioEndpoint(data_flow(flow), role(flow)) } {
                // Failure to read the default's identifier should not prevent
                // selecting another endpoint; its default marker is omitted.
                Ok(device) => device_id(&device).ok(),
                Err(error) if error.code().0 == ENDPOINT_NOT_FOUND => None,
                Err(error) => return Err(native_error("GetDefaultAudioEndpoint", error)),
            };
        let collection =
            unsafe { enumerator.EnumAudioEndpoints(data_flow(flow), DEVICE_STATE_ACTIVE) }
                .map_err(|error| native_error("EnumAudioEndpoints", error))?;
        let count = unsafe { collection.GetCount() }
            .map_err(|error| native_error("IMMDeviceCollection::GetCount", error))?
            as usize;
        if count > limits.max_devices {
            return Err(AudioCaptureError::Limit("audio endpoint enumeration count"));
        }
        let mut devices = Vec::new();
        devices
            .try_reserve_exact(count)
            .map_err(|_| AudioCaptureError::Limit("audio device metadata allocation"))?;
        for index in 0..count {
            // Endpoints can disappear between EnumAudioEndpoints and Item, or
            // expose unreadable metadata. Do not hide every other working
            // device because one entry changed while the list was being read.
            let Ok(device) = (unsafe { collection.Item(index as u32) }) else { continue; };
            let Ok(id) = device_id(&device) else { continue; };
            if devices.iter().any(|entry: &AudioDeviceInfo| matches!(&entry.target, AudioTarget::WindowsEndpoint { id: other } if other == &id)) {
                continue;
            }
            // Keep the actual endpoint selectable when only its friendly name
            // is unavailable. The identifier is real, bounded metadata, not a
            // fabricated device or an implicit switch to the default endpoint.
            let name = device_name(&device).unwrap_or_else(|_| id.clone());
            let is_default = default.as_deref() == Some(id.as_str());
            devices.push(AudioDeviceInfo {
                target: AudioTarget::WindowsEndpoint { id },
                name,
                flow,
                is_default,
            });
        }
        Ok(devices)
    }
    fn enumerate_processes(&mut self, limits: AudioCaptureLimits) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        validate_limits(limits)?; self.initialize()?; process_loopback::enumerate(limits)
    }
    fn open_window_observation(
        &mut self,
        window: &super::super::WindowAudioObservation,
        limits: AudioCaptureLimits,
    ) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
        validate_limits(limits)?;
        self.initialize()?;
        process_loopback::open_observed_window(window.native(), limits, self.frequency()?,
            self.apartment.clone().ok_or(AudioCaptureError::Invalid("missing COM owner"))?,
            self.process_activations.clone())
    }
    fn open(
        &mut self,
        binding: &AudioCaptureBinding,
        limits: AudioCaptureLimits,
    ) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
        if let AudioTarget::WindowsWindow { selector } = binding.target() {
            validate_limits(limits)?;
            if binding.flow()!=AudioFlow::Output || binding.exclude_own_audio(){return Err(AudioCaptureError::Invalid("window audio requires output flow without exclusion"));}
            self.initialize()?;
            return process_loopback::open_window(selector,limits,self.frequency()?,self.apartment.clone().ok_or(AudioCaptureError::Invalid("missing COM owner"))?,self.process_activations.clone());
        }
        if let AudioTarget::WindowsProcess { identity } = binding.target() {
            validate_limits(limits)?;
            if binding.flow()!=AudioFlow::Output || binding.exclude_own_audio() { return Err(AudioCaptureError::Invalid("process audio requires output flow without own-output exclusion")); }
            self.initialize()?;
            return process_loopback::open(identity,limits,self.frequency()?,self.apartment.clone().ok_or(AudioCaptureError::Invalid("missing COM owner"))?,self.process_activations.clone());
        }
        if binding.exclude_own_audio() {
            return Err(AudioCaptureError::Unsupported(
                "WASAPI endpoint capture cannot exclude this process",
            ));
        }
        if !matches!(
            binding.target(),
            AudioTarget::Default | AudioTarget::WindowsEndpoint { .. }
        ) {
            return Err(AudioCaptureError::Unsupported(
                "audio target requires a different operating system",
            ));
        }
        validate_limits(limits)?;
        self.initialize()?;
        let enumerator = self
            .enumerator
            .as_ref()
            .ok_or(AudioCaptureError::Invalid("missing WASAPI enumerator"))?;
        let device = match binding.target() {
            AudioTarget::Default => unsafe {
                enumerator.GetDefaultAudioEndpoint(data_flow(binding.flow()), role(binding.flow()))
            }
            .map_err(|error| native_error("GetDefaultAudioEndpoint", error))?,
            AudioTarget::WindowsEndpoint { id } => {
                // Canonical values already validate this, but keep the native
                // pointer-construction boundary independently bounded.
                validate_text(id, MAX_ID_BYTES)?;
                let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
                unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }
                    .map_err(|error| native_error("GetDevice(endpoint ID)", error))?
            }
            _ => return Err(AudioCaptureError::Unsupported("foreign audio target")),
        };
        let endpoint: IMMEndpoint = device
            .cast()
            .map_err(|error| native_error("IMMEndpoint cast", error))?;
        let actual_flow = unsafe { endpoint.GetDataFlow() }
            .map_err(|error| native_error("IMMEndpoint::GetDataFlow", error))?;
        if actual_flow != data_flow(binding.flow()) {
            return Err(AudioCaptureError::DeviceUnavailable(
                "endpoint data flow does not match the authored audio binding".into(),
            ));
        }
        let state = unsafe { device.GetState() }
            .map_err(|error| native_error("IMMDevice::GetState", error))?;
        if state != DEVICE_STATE_ACTIVE {
            return Err(AudioCaptureError::DeviceUnavailable(
                "audio endpoint is not active".into(),
            ));
        }
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
            .map_err(|error| native_error("IMMDevice::Activate(IAudioClient)", error))?;
        let allocation = TaskMem(
            unsafe { client.GetMixFormat() }
                .map_err(|error| native_error("IAudioClient::GetMixFormat", error))?,
        );
        if allocation.0.is_null() {
            return Err(AudioCaptureError::Invalid("GetMixFormat returned null"));
        }
        // GetMixFormat guarantees a complete WAVEFORMATEX + declared extension.
        let header = unsafe { ptr::read_unaligned(allocation.0) };
        let extension = if header.wFormatTag == WAVE_EXTENSIBLE {
            if header.cbSize < EXTENSION_BYTES {
                return Err(AudioCaptureError::Unsupported(
                    "truncated WAVEFORMATEXTENSIBLE",
                ));
            }
            Some(unsafe { ptr::read_unaligned(allocation.0.cast::<WAVEFORMATEXTENSIBLE>()) })
        } else {
            None
        };
        let mix = parse_format(header, extension, limits)?;
        let flags = if binding.flow() == AudioFlow::Output {
            AUDCLNT_STREAMFLAGS_LOOPBACK
        } else {
            0
        };
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                CLIENT_DURATION_100NS,
                0,
                allocation.0,
                None,
            )
        }
        .map_err(|error| native_error("IAudioClient::Initialize(shared)", error))?;
        let buffer_frames = unsafe { client.GetBufferSize() }
            .map_err(|error| native_error("IAudioClient::GetBufferSize", error))?
            as usize;
        if buffer_frames == 0
            || buffer_frames
                .checked_mul(mix.block_align)
                .is_none_or(|n| n > limits.max_native_buffer_bytes)
        {
            return Err(AudioCaptureError::Limit(
                "WASAPI native endpoint buffer bytes",
            ));
        }
        let capture: IAudioCaptureClient = unsafe { client.GetService() }.map_err(|error| {
            native_error("IAudioClient::GetService(IAudioCaptureClient)", error)
        })?;
        let frequency = self.frequency()?;
        let default_watch = if matches!(binding.target(), AudioTarget::Default) {
            let resolved_id = device_id(&device)?;
            let next_check_ns = next_default_check(host_now_ns(frequency)?)?;
            Some(DefaultWatch {
                enumerator: enumerator.clone(),
                flow: binding.flow(),
                resolved_id,
                next_check_ns,
                invalidated: false,
            })
        } else {
            None
        };
        let apartment = self
            .apartment
            .clone()
            .ok_or(AudioCaptureError::Invalid("missing COM apartment owner"))?;
        let mut stream = WasapiStream {
            description: device_name(&device).ok().map(std::sync::Arc::from),
            capture,
            client,
            default_watch,
            _apartment: apartment,
            mix,
            limits,
            frequency,
            started: false,
        };
        unsafe { stream.client.Start() }
            .map_err(|error| native_error("IAudioClient::Start", error))?;
        stream.started = true;
        Ok(Box::new(stream))
    }
}

struct WasapiStream {
    description: Option<std::sync::Arc<str>>,
    capture: IAudioCaptureClient,
    client: IAudioClient,
    default_watch: Option<DefaultWatch>,
    // Retains the apartment until both interfaces above have been released.
    _apartment: Rc<ComApartment>,
    mix: MixFormat,
    limits: AudioCaptureLimits,
    frequency: u64,
    started: bool,
}
struct DefaultWatch {
    enumerator: IMMDeviceEnumerator,
    flow: AudioFlow,
    resolved_id: String,
    next_check_ns: u64,
    invalidated: bool,
}
impl DefaultWatch {
    fn refresh(&mut self, now_ns: u64) -> Result<(), AudioCaptureError> {
        if self.invalidated {
            return Err(AudioCaptureError::DeviceUnavailable(format!(
                "default endpoint {} changed or became unavailable; reopen capture",
                self.resolved_id
            )));
        }
        if now_ns < self.next_check_ns {
            return Ok(());
        }
        self.next_check_ns = next_default_check(now_ns)?;
        // At most one bounded resolution per host-clock second. This executes
        // on the normal owner even when no audio packets arrive, not a callback.
        let current = (|| {
            let device = unsafe {
                self.enumerator
                    .GetDefaultAudioEndpoint(data_flow(self.flow), role(self.flow))
            }
            .map_err(|error| native_error("GetDefaultAudioEndpoint(refresh)", error))?;
            device_id(&device)
        })();
        match current {
            Ok(id) if id == self.resolved_id => Ok(()),
            Ok(id) => {
                self.invalidated = true;
                Err(AudioCaptureError::DeviceUnavailable(format!(
                    "default endpoint changed from {} to {}; reopen capture",
                    self.resolved_id, id
                )))
            }
            Err(error) => {
                self.invalidated = true;
                Err(error)
            }
        }
    }
}
impl NativeAudioStream for WasapiStream {
    fn description(&self) -> Option<std::sync::Arc<str>> {
        self.description.clone()
    }
    fn poll(&mut self) -> Result<Option<NativePcmBlock>, AudioCaptureError> {
        if !self.started {
            return Err(AudioCaptureError::Closed);
        }
        if let Some(watch) = &mut self.default_watch {
            watch.refresh(host_now_ns(self.frequency)?)?;
        }
        let next = unsafe { self.capture.GetNextPacketSize() }
            .map_err(|error| native_error("IAudioCaptureClient::GetNextPacketSize", error))?;
        if next == 0 {
            return Ok(None);
        }
        let (mut data, mut frames, mut flags, mut position, mut timestamp) =
            (ptr::null_mut(), 0u32, 0u32, 0u64, 0u64);
        // Preserve S_BUFFER_EMPTY explicitly; the generated Result<()> wrapper
        // otherwise loses that success HRESULT and cannot distinguish no lease.
        let status = unsafe {
            (Interface::vtable(&self.capture).GetBuffer)(
                Interface::as_raw(&self.capture),
                &mut data,
                &mut frames,
                &mut flags,
                &mut position,
                &mut timestamp,
            )
        };
        if status == AUDCLNT_S_BUFFER_EMPTY {
            return Ok(None);
        }
        status
            .ok()
            .map_err(|error| native_error("IAudioCaptureClient::GetBuffer", error))?;
        let packet = PacketLease {
            capture: &self.capture,
            frames,
            released: false,
        };
        let result = (|| {
            let frames = frames as usize;
            if frames == 0 || frames > self.limits.max_frames || frames > 4096 {
                return Err(AudioCaptureError::Limit("WASAPI packet frames"));
            }
            let byte_count = frames
                .checked_mul(self.mix.block_align)
                .ok_or(AudioCaptureError::Limit("WASAPI packet byte overflow"))?;
            if byte_count > self.limits.max_packet_bytes {
                return Err(AudioCaptureError::Limit("WASAPI raw packet bytes"));
            }
            let native_flags = NativePacketFlags {
                silent: flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0,
                discontinuity: flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0,
                timestamp_error: flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0,
                queue_dropped: 0,
            };
            let host_received_ns = host_now_ns(self.frequency)?;
            let device_timestamp_ns = packet_timestamp_ns(timestamp, native_flags.timestamp_error)?;
            let bytes = if native_flags.silent {
                &[][..]
            } else {
                if data.is_null() {
                    return Err(AudioCaptureError::Invalid(
                        "non-silent WASAPI packet has null data",
                    ));
                }
                unsafe { std::slice::from_raw_parts(data, byte_count) }
            };
            let samples = decode_pcm(
                bytes,
                self.mix.encoding,
                frames,
                self.mix.format,
                self.limits,
                native_flags.silent,
            )?;
            let block = NativePcmBlock {
                format: self.mix.format,
                frames,
                samples,
                device_timestamp_ns,
                graph_timestamp_ns: None,
                directshow_clock: None,
                host_received_ns,
                device_frame_position: Some(position),
                flags: native_flags,
            };
            block.validate(self.limits)?;
            Ok(block)
        })();
        let released = packet.release();
        match result {
            Ok(block) => {
                released?;
                Ok(Some(block))
            }
            Err(error) => {
                let _ = released;
                Err(error)
            }
        }
    }
    fn close(&mut self) -> Result<(), AudioCaptureError> {
        if self.started {
            if let Err(error) = unsafe { self.client.Stop() } {
                if !retired_stop_error(error.code()) {
                    return Err(native_error("IAudioClient::Stop", error));
                }
                // Invalidation has already closed the WASAPI stream. There is
                // no Rust callback; interface release completes this lifetime.
            }
            self.started = false;
        }
        Ok(())
    }
}
impl Drop for WasapiStream {
    fn drop(&mut self) {
        // No Rust callback exists. COM interface destruction stays on the worker.
        if self.started {
            let _ = unsafe { self.client.Stop() };
        }
    }
}
fn retired_stop_error(code: windows::core::HRESULT) -> bool {
    code == AUDCLNT_E_DEVICE_INVALIDATED
        || code == AUDCLNT_E_RESOURCES_INVALIDATED
        || code == AUDCLNT_E_SERVICE_NOT_RUNNING
        || code == AUDCLNT_E_NOT_INITIALIZED
}
struct PacketLease<'a> {
    capture: &'a IAudioCaptureClient,
    frames: u32,
    released: bool,
}
impl PacketLease<'_> {
    fn release(mut self) -> Result<(), AudioCaptureError> {
        self.released = true;
        unsafe { self.capture.ReleaseBuffer(self.frames) }
            .map_err(|error| native_error("IAudioCaptureClient::ReleaseBuffer", error))
    }
}
impl Drop for PacketLease<'_> {
    fn drop(&mut self) {
        if !self.released {
            let _ = unsafe { self.capture.ReleaseBuffer(self.frames) };
        }
    }
}
struct TaskMem<T>(*mut T);
impl<T> Drop for TaskMem<T> {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0.cast::<c_void>()));
        }
    }
}

#[derive(Clone, Copy)]
struct MixFormat {
    format: NativeAudioFormat,
    encoding: PcmEncoding,
    block_align: usize,
}
fn parse_format(
    header: WAVEFORMATEX,
    extension: Option<WAVEFORMATEXTENSIBLE>,
    limits: AudioCaptureLimits,
) -> Result<MixFormat, AudioCaptureError> {
    let (tag, bits, channels, sample_rate, block_align, average) = (
        header.wFormatTag,
        header.wBitsPerSample,
        header.nChannels,
        header.nSamplesPerSec,
        header.nBlockAlign,
        header.nAvgBytesPerSec,
    );
    let (subtype, valid_bits, mask) = if tag == WAVE_EXTENSIBLE {
        if header.cbSize < EXTENSION_BYTES {
            return Err(AudioCaptureError::Unsupported(
                "truncated WAVEFORMATEXTENSIBLE",
            ));
        }
        let ext = extension.ok_or(AudioCaptureError::Unsupported(
            "missing WAVEFORMATEXTENSIBLE",
        ))?;
        (
            ext.SubFormat,
            unsafe { ext.Samples.wValidBitsPerSample },
            ext.dwChannelMask,
        )
    } else {
        let mask = match channels {
            1 => 4,
            2 => 3,
            _ => 0,
        };
        (
            if tag == WAVE_PCM {
                SUBTYPE_PCM
            } else if tag == WAVE_FLOAT {
                SUBTYPE_FLOAT
            } else {
                return Err(AudioCaptureError::Unsupported(
                    "unsupported WASAPI wave format tag",
                ));
            },
            bits,
            mask,
        )
    };
    let encoding = if subtype == SUBTYPE_FLOAT {
        if bits != 32 || valid_bits != 32 {
            return Err(AudioCaptureError::Unsupported(
                "WASAPI float capture requires 32-bit samples",
            ));
        }
        PcmEncoding::Float32
    } else if subtype == SUBTYPE_PCM {
        if valid_bits == 0 || valid_bits > bits {
            return Err(AudioCaptureError::Unsupported(
                "invalid WASAPI PCM valid bits",
            ));
        }
        match (bits, valid_bits) {
            (8, 8) => PcmEncoding::Unsigned8,
            (8, valid_bits) => PcmEncoding::UnsignedIn8 { valid_bits },
            (16, 16) => PcmEncoding::Signed16,
            (16, valid_bits) => PcmEncoding::SignedIn16 { valid_bits },
            (24, 24) => PcmEncoding::Signed24,
            (32, 32) => PcmEncoding::Signed32,
            (24, valid_bits) => PcmEncoding::SignedIn24 { valid_bits },
            (32, valid_bits) => PcmEncoding::SignedIn32 { valid_bits },
            _ => {
                return Err(AudioCaptureError::Unsupported(
                    "unsupported WASAPI PCM container/valid bits",
                ));
            }
        }
    } else {
        return Err(AudioCaptureError::Unsupported(
            "unsupported WASAPI extensible subtype",
        ));
    };
    let format = NativeAudioFormat {
        sample_rate,
        channels,
        channel_mask: mask,
    };
    format.validate(limits)?;
    let expected = usize::from(channels)
        .checked_mul(usize::from(bits / 8))
        .ok_or(AudioCaptureError::Limit("WASAPI block alignment overflow"))?;
    if block_align as usize != expected
        || expected == 0
        || average as u64 != u64::from(sample_rate) * expected as u64
    {
        return Err(AudioCaptureError::Unsupported(
            "inconsistent WASAPI PCM block alignment or byte rate",
        ));
    }
    Ok(MixFormat {
        format,
        encoding,
        block_align: expected,
    })
}
fn validate_limits(limits: AudioCaptureLimits) -> Result<(), AudioCaptureError> {
    if limits.max_frames == 0
        || limits.max_frames > 4096
        || limits.max_channels == 0
        || limits.max_channels > 8
        || limits.max_packet_bytes == 0
        || limits.max_packet_bytes > 128 * 1024
        || limits.max_native_buffer_bytes == 0
        || limits.max_native_buffer_bytes > 4 * 1024 * 1024
    {
        return Err(AudioCaptureError::Limit("invalid WASAPI capture limits"));
    }
    Ok(())
}
fn data_flow(flow: AudioFlow) -> EDataFlow {
    match flow {
        AudioFlow::Input => eCapture,
        AudioFlow::Output => eRender,
    }
}
fn role(flow: AudioFlow) -> windows::Win32::Media::Audio::ERole {
    match flow {
        AudioFlow::Input => eCommunications,
        AudioFlow::Output => eConsole,
    }
}
fn device_id(device: &IMMDevice) -> Result<String, AudioCaptureError> {
    let value =
        unsafe { device.GetId() }.map_err(|error| native_error("IMMDevice::GetId", error))?;
    let allocation = TaskMem(value.0);
    unsafe { bounded_utf16(allocation.0, MAX_ID_BYTES) }
}
fn device_name(device: &IMMDevice) -> Result<String, AudioCaptureError> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }
        .map_err(|error| native_error("IMMDevice::OpenPropertyStore", error))?;
    let value: PROPVARIANT = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }
        .map_err(|error| native_error("IPropertyStore::GetValue(FriendlyName)", error))?;
    // PROPVARIANT already owns PropVariantClear in windows-core 0.58.
    let raw = unsafe { value.as_raw().Anonymous.Anonymous };
    if raw.vt != 31 {
        return Err(AudioCaptureError::Invalid(
            "audio friendly name is not VT_LPWSTR",
        ));
    }
    unsafe { bounded_utf16(raw.Anonymous.pwszVal, MAX_NAME_BYTES) }
}
// Safety: pointer is a live OS-owned nul-terminated UTF-16 string. The maximum
// bounds scanning/copying; it cannot validate arbitrary foreign memory.
unsafe fn bounded_utf16(
    pointer: *const u16,
    max_bytes: usize,
) -> Result<String, AudioCaptureError> {
    if pointer.is_null() {
        return Err(AudioCaptureError::Invalid("null audio endpoint metadata"));
    }
    let mut length = 0usize;
    while length <= max_bytes {
        if unsafe { pointer.add(length).read() } == 0 {
            let units = unsafe { std::slice::from_raw_parts(pointer, length) };
            let result = String::from_utf16(units).map_err(|_| {
                AudioCaptureError::Invalid("invalid UTF-16 audio endpoint metadata")
            })?;
            validate_text(&result, max_bytes)?;
            return Ok(result.into_boxed_str().into_string());
        }
        length += 1;
    }
    Err(AudioCaptureError::Limit("audio endpoint metadata bytes"))
}
fn validate_text(value: &str, max_bytes: usize) -> Result<(), AudioCaptureError> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(AudioCaptureError::Invalid(
            "audio endpoint metadata is empty, oversized or contains controls",
        ));
    }
    Ok(())
}
/// Bracket one QPC observation with readings from the desktop playback clock.
/// On Windows `Instant` is QPC-based; retain a conservative 100 ns envelope
/// for Media Foundation's quantized device timestamps and integer conversion.
pub(super) fn correlate_media_clock(origin: std::time::Instant, session: u64, epoch: u64)
    -> Option<skylive_media::QpcMediaCorrelation> {
    let mut frequency = 0i64;
    unsafe { QueryPerformanceFrequency(&mut frequency) }.ok()?;
    let frequency = u64::try_from(frequency).ok().filter(|value| *value != 0)?;
    let before = u64::try_from(origin.elapsed().as_nanos()).ok()?;
    let qpc = host_now_ns(frequency).ok()?;
    let after = u64::try_from(origin.elapsed().as_nanos()).ok()?;
    let earliest = before.saturating_sub(101);
    let latest = after.checked_add(101)?;
    skylive_media::QpcMediaCorrelation::new(session, epoch, qpc,
        skylive_media::MediaTimeBounds { earliest: skylive_media::MediaTime(earliest), latest: skylive_media::MediaTime(latest) },
        qpc.saturating_sub(21_000_000_000), qpc.checked_add(20_000_000_000)?, 1_000_000)
}

fn host_now_ns(frequency: u64) -> Result<u64, AudioCaptureError> {
    let mut counter = 0i64;
    unsafe { QueryPerformanceCounter(&mut counter) }
        .map_err(|error| native_error("QueryPerformanceCounter", error))?;
    qpc_ns(counter, frequency)
}
fn qpc_ns(counter: i64, frequency: u64) -> Result<u64, AudioCaptureError> {
    if counter < 0 || frequency == 0 {
        return Err(AudioCaptureError::Invalid("invalid QPC clock value"));
    }
    u64::try_from(counter as u128 * 1_000_000_000 / u128::from(frequency))
        .map_err(|_| AudioCaptureError::TimeOverflow)
}
fn packet_timestamp_ns(
    timestamp_100ns: u64,
    timestamp_error: bool,
) -> Result<Option<u64>, AudioCaptureError> {
    if timestamp_error {
        Ok(None)
    } else {
        timestamp_100ns
            .checked_mul(100)
            .map(Some)
            .ok_or(AudioCaptureError::TimeOverflow)
    }
}
fn next_default_check(now_ns: u64) -> Result<u64, AudioCaptureError> {
    now_ns
        .checked_add(1_000_000_000)
        .ok_or(AudioCaptureError::TimeOverflow)
}
fn native_error(operation: &'static str, error: windows::core::Error) -> AudioCaptureError {
    if error.code() == E_ACCESSDENIED {
        return AudioCaptureError::PermissionDenied(operation);
    }
    if error.code() == AUDCLNT_E_DEVICE_INVALIDATED
        || error.code() == AUDCLNT_E_SERVICE_NOT_RUNNING
        || error.code().0 == ENDPOINT_NOT_FOUND
    {
        return AudioCaptureError::DeviceUnavailable(format!("{operation}: {error}"));
    }
    if error.code() == AUDCLNT_E_UNSUPPORTED_FORMAT {
        return AudioCaptureError::Unsupported("WASAPI rejected the native shared-mode format");
    }
    AudioCaptureError::Native {
        operation,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wave(tag: u16, bits: u16, channels: u16) -> WAVEFORMATEX {
        let align = channels * (bits / 8);
        WAVEFORMATEX {
            wFormatTag: tag,
            nChannels: channels,
            nSamplesPerSec: 48000,
            nAvgBytesPerSec: 48000 * u32::from(align),
            nBlockAlign: align,
            wBitsPerSample: bits,
            cbSize: if tag == WAVE_EXTENSIBLE {
                EXTENSION_BYTES
            } else {
                0
            },
        }
    }
    fn extensible(
        bits: u16,
        valid: u16,
        channels: u16,
        mask: u32,
        subtype: GUID,
    ) -> WAVEFORMATEXTENSIBLE {
        let mut ext = WAVEFORMATEXTENSIBLE {
            Format: wave(WAVE_EXTENSIBLE, bits, channels),
            ..Default::default()
        };
        ext.Samples.wValidBitsPerSample = valid;
        ext.SubFormat = subtype;
        ext.dwChannelMask = mask;
        ext
    }
    #[test]
    fn default_roles_match_obs_and_factory_has_no_native_state() {
        assert_eq!(data_flow(AudioFlow::Input), eCapture);
        assert_eq!(role(AudioFlow::Input), eCommunications);
        assert_eq!(data_flow(AudioFlow::Output), eRender);
        assert_eq!(role(AudioFlow::Output), eConsole);
        let backend = backend().unwrap();
        let caps = backend.capabilities();
        assert!(caps.input && caps.output && caps.device_enumeration);
        assert!(!caps.exclude_own_audio);
        // Do not enumerate/open/query clocks: factory/capability/drop alone are inert.
        drop(backend);
    }
    #[test]
    fn foreign_targets_and_process_exclusion_reject_before_com_initialization() {
        let mut backend = WasapiBackend {
            enumerator: None,
            apartment: None,
            frequency: Cell::new(None),
            process_activations: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        for binding in [
            AudioCaptureBinding::new(AudioFlow::Output, AudioTarget::MacSystem, true, false)
                .unwrap(),
            AudioCaptureBinding::new(AudioFlow::Output, AudioTarget::Default, true, true).unwrap(),
        ] {
            assert!(matches!(
                backend.open(&binding, AudioCaptureLimits::default()),
                Err(AudioCaptureError::Unsupported(_))
            ));
            assert!(backend.enumerator.is_none());
            assert!(backend.apartment.is_none());
            assert_eq!(backend.frequency.get(), None);
        }
    }
    #[test]
    fn wave_integer_and_float_metadata_feed_the_shared_pcm_decoder() {
        let limits = AudioCaptureLimits::default();
        for (bits, encoding) in [
            (8, PcmEncoding::Unsigned8),
            (16, PcmEncoding::Signed16),
            (24, PcmEncoding::Signed24),
            (32, PcmEncoding::Signed32),
        ] {
            let mix = parse_format(wave(WAVE_PCM, bits, 1), None, limits).unwrap();
            assert_eq!(mix.encoding, encoding);
            assert_eq!(mix.block_align, usize::from(bits / 8));
            assert_eq!(
                mix.format,
                NativeAudioFormat {
                    sample_rate: 48000,
                    channels: 1,
                    channel_mask: 4
                }
            );
        }
        let ext = extensible(32, 24, 1, 4, SUBTYPE_PCM);
        let mix = parse_format(ext.Format, Some(ext), limits).unwrap();
        assert_eq!(mix.encoding, PcmEncoding::SignedIn32 { valid_bits: 24 });
        assert_eq!(
            decode_pcm(
                &[0, 0, 0, 128, 0, 0, 0, 0, 0, 0, 0, 64],
                mix.encoding,
                3,
                mix.format,
                limits,
                false
            )
            .unwrap(),
            [-1.0, 0.0, 0.5]
        );
        let mix = parse_format(wave(WAVE_FLOAT, 32, 1), None, limits).unwrap();
        assert_eq!(
            decode_pcm(
                &0.25f32.to_le_bytes(),
                mix.encoding,
                1,
                mix.format,
                limits,
                false
            )
            .unwrap(),
            [0.25]
        );
        assert!(
            decode_pcm(
                &f32::NAN.to_le_bytes(),
                mix.encoding,
                1,
                mix.format,
                limits,
                false
            )
            .is_err()
        );
        assert_eq!(
            decode_pcm(&[], mix.encoding, 2, mix.format, limits, true).unwrap(),
            [0.0, 0.0]
        );
    }
    #[test]
    fn extensible_layout_and_valid_bits_are_preserved_without_guessing_surround_order() {
        let limits = AudioCaptureLimits::default();
        let ext = extensible(32, 32, 6, 0x3f, SUBTYPE_FLOAT);
        let mix = parse_format(ext.Format, Some(ext), limits).unwrap();
        assert_eq!(mix.format.channel_mask, 0x3f);
        assert_eq!(mix.format.channels, 6);
        let ext = extensible(24, 20, 2, 0, SUBTYPE_PCM);
        let mix = parse_format(ext.Format, Some(ext), limits).unwrap();
        assert_eq!(mix.format.channel_mask, 0);
        assert_eq!(mix.encoding, PcmEncoding::SignedIn24 { valid_bits: 20 });
        assert_eq!(
            parse_format(wave(WAVE_PCM, 16, 2), None, limits)
                .unwrap()
                .format
                .channel_mask,
            3
        );
        assert_eq!(
            parse_format(wave(WAVE_PCM, 16, 6), None, limits)
                .unwrap()
                .format
                .channel_mask,
            0
        );
    }
    #[test]
    fn malformed_or_unsupported_wave_formats_reject_before_native_stream_start() {
        let limits = AudioCaptureLimits::default();
        for mut header in [wave(WAVE_PCM, 16, 1), wave(WAVE_FLOAT, 32, 1)] {
            header.nBlockAlign += 1;
            assert!(parse_format(header, None, limits).is_err());
        }
        let mut header = wave(WAVE_PCM, 16, 1);
        header.nAvgBytesPerSec = 1;
        assert!(parse_format(header, None, limits).is_err());
        let mut header = wave(WAVE_EXTENSIBLE, 32, 1);
        header.cbSize = EXTENSION_BYTES - 1;
        assert!(parse_format(header, None, limits).is_err());
        for header in [
            wave(2, 16, 1),
            wave(WAVE_FLOAT, 64, 1),
            wave(WAVE_PCM, 16, 9),
            wave(WAVE_PCM, 16, 0),
        ] {
            assert!(parse_format(header, None, limits).is_err());
        }
        for ext in [
            extensible(32, 0, 1, 4, SUBTYPE_PCM),
            extensible(32, 33, 1, 4, SUBTYPE_PCM),
            extensible(32, 24, 1, 4, SUBTYPE_FLOAT),
            extensible(32, 32, 2, 4, SUBTYPE_FLOAT),
            extensible(32, 32, 1, 4, GUID::zeroed()),
        ] {
            assert!(parse_format(ext.Format, Some(ext), limits).is_err());
        }
        let mut header = wave(WAVE_PCM, 16, 1);
        header.nSamplesPerSec = 7999;
        assert!(parse_format(header, None, limits).is_err());
    }
    #[test]
    fn packet_and_host_qpc_conversions_share_nanoseconds_and_reject_overflow() {
        assert_eq!(qpc_ns(1, 3).unwrap(), 333_333_333);
        assert_eq!(qpc_ns(3, 3).unwrap(), 1_000_000_000);
        assert_eq!(
            qpc_ns(123_456, 10_000_000).unwrap(),
            packet_timestamp_ns(123_456, false).unwrap().unwrap()
        );
        assert!(qpc_ns(-1, 1).is_err());
        assert!(qpc_ns(1, 0).is_err());
        assert_eq!(qpc_ns(i64::MAX, 1), Err(AudioCaptureError::TimeOverflow));
        assert_eq!(
            packet_timestamp_ns(u64::MAX, false),
            Err(AudioCaptureError::TimeOverflow)
        );
        assert_eq!(packet_timestamp_ns(u64::MAX, true).unwrap(), None);
    }
    #[test]
    fn default_refresh_deadline_uses_one_host_second_without_saturating() {
        assert_eq!(next_default_check(123).unwrap(), 1_000_000_123);
        assert_eq!(
            next_default_check(u64::MAX - 1_000_000_000).unwrap(),
            u64::MAX
        );
        assert_eq!(
            next_default_check(u64::MAX - 999_999_999),
            Err(AudioCaptureError::TimeOverflow)
        );
    }
    #[test]
    fn endpoint_utf16_metadata_is_exact_bounded_and_rejects_controls() {
        let text: Vec<u16> = "Endpoint-мікрофон-{Aa}😀"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        assert_eq!(
            unsafe { bounded_utf16(text.as_ptr(), 1024) }.unwrap(),
            "Endpoint-мікрофон-{Aa}😀"
        );
        for units in [vec![0], vec![65, 10, 0], vec![0xd800, 0]] {
            assert!(unsafe { bounded_utf16(units.as_ptr(), 1024) }.is_err());
        }
        let unterminated = [65u16; 1025];
        assert!(matches!(
            unsafe { bounded_utf16(unterminated.as_ptr(), 1024) },
            Err(AudioCaptureError::Limit(_))
        ));
        let utf8_expansion: Vec<u16> = "я".repeat(513).encode_utf16().chain(Some(0)).collect();
        assert!(unsafe { bounded_utf16(utf8_expansion.as_ptr(), 1024) }.is_err());
    }
    #[test]
    fn invalidated_streams_release_interfaces_instead_of_waiting_for_an_impossible_stop() {
        for code in [
            AUDCLNT_E_DEVICE_INVALIDATED,
            AUDCLNT_E_RESOURCES_INVALIDATED,
            AUDCLNT_E_SERVICE_NOT_RUNNING,
            AUDCLNT_E_NOT_INITIALIZED,
        ] {
            assert!(retired_stop_error(code));
        }
        assert!(!retired_stop_error(windows::Win32::Foundation::E_FAIL));
        assert!(!retired_stop_error(E_ACCESSDENIED));
    }
}
