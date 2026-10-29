//! All COM interfaces live exclusively on the encoding worker thread.
use super::{
    AccessUnit, Adapter, CodecConfiguration, Config, Error, MAX_AU_BYTES, MAX_BATCH_BYTES,
    MAX_HEADER_BYTES, MAX_PACKETS, MAX_PENDING, Nv12Input,
};
use std::{collections::VecDeque, mem::ManuallyDrop, rc::Rc};
use windows::{
    Win32::{Foundation::E_NOTIMPL, Media::MediaFoundation::*, System::Com::*},
    core::{Interface, VARIANT},
};

fn native(operation: &'static str, error: windows::core::Error) -> Error {
    Error::Native {
        operation,
        hresult: error.code().0,
        detail: error.to_string(),
    }
}
fn sdk<T>(
    operation: &'static str,
    call: impl FnOnce() -> windows::core::Result<T>,
) -> Result<T, Error> {
    call().map_err(|error| native(operation, error))
}
struct Context {
    _not_send: Rc<()>,
}
impl Context {
    fn new() -> Result<Self, Error> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| native("H264 CoInitializeEx", e))?;
        if let Err(error) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            unsafe { CoUninitialize() };
            return Err(native("H264 MFStartup", error));
        }
        Ok(Self {
            _not_send: Rc::new(()),
        })
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}
struct Stamp {
    pts: i64,
    duration: i64,
}

pub(super) struct Encoder {
    transform: IMFTransform,
    config: Config,
    configuration: CodecConfiguration,
    input_id: u32,
    output_id: u32,
    input_alignment: u32,
    output_info: MFT_OUTPUT_STREAM_INFO,
    pending: VecDeque<Stamp>,
    eos: bool,
    drained: bool,
    // COM/context teardown occurs after transform and buffered samples.
    _context: Context,
}
impl Encoder {
    pub(super) fn new(config: Config) -> Result<Self, Error> {
        let context = Context::new()?;
        // Direct documented Microsoft software encoder: no arbitrary hardware
        // enumeration or async-transform unlock is performed by this profile.
        let transform: IMFTransform =
            unsafe { CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| native("create Microsoft software H264 encoder", e))?;
        match unsafe { transform.GetAttributes() } {
            Ok(attributes) => match unsafe { attributes.GetUINT32(&MF_TRANSFORM_ASYNC) } {
                Ok(0) => {}
                Ok(_) => {
                    return Err(Error::Unavailable(
                        "asynchronous H264 transform is outside this adapter",
                    ));
                }
                Err(error) if error.code() == MF_E_ATTRIBUTENOTFOUND => {}
                Err(error) => return Err(native("inspect H264 transform synchronization", error)),
            },
            Err(error) if error.code() == E_NOTIMPL => {}
            Err(error) => return Err(native("H264 transform attributes", error)),
        }
        let (mut inputs, mut outputs) = (0, 0);
        unsafe { transform.GetStreamCount(&mut inputs, &mut outputs) }
            .map_err(|e| native("H264 stream count", e))?;
        if inputs != 1 || outputs != 1 {
            return Err(Error::Unavailable(
                "H264 transform does not have exactly one input and output",
            ));
        }
        let (mut input_ids, mut output_ids) = ([0], [0]);
        match unsafe { transform.GetStreamIDs(&mut input_ids, &mut output_ids) } {
            Ok(()) => {}
            Err(error) if error.code() == E_NOTIMPL => {}
            Err(error) => return Err(native("H264 stream IDs", error)),
        }
        let input_id = input_ids[0];
        let output_id = output_ids[0];
        let codec: ICodecAPI = transform
            .cast()
            .map_err(|e| native("H264 codec properties", e))?;
        // Average bitrate, not a claim of hard CBR, hardware or actual throughput.
        set_u32(&codec, &CODECAPI_AVEncCommonMeanBitRate, config.bitrate())?;
        set_u32(&codec, &CODECAPI_AVEncMPVGOPSize, config.gop())?;
        // High permits B pictures, so its no-B property is mandatory. Native
        // PTS/DTS and bounded slice parsing independently reject reordered AUs.
        if config.profile()==super::H264Profile::High {
            unsafe {codec.IsSupported(&CODECAPI_AVEncMPVDefaultBPictureCount)}
                .map_err(|e|native("High profile requires native no-B control",e))?;
            set_u32(&codec,&CODECAPI_AVEncMPVDefaultBPictureCount,0)?;
        }
        // Baseline itself forbids B frames. This optional property is also set
        // when supported; failure from an advertised property remains visible.
        if unsafe { codec.IsSupported(&CODECAPI_AVEncMPVDefaultBPictureCount) }.is_ok() {
            set_u32(&codec, &CODECAPI_AVEncMPVDefaultBPictureCount, 0)?;
        }
        if unsafe { codec.IsSupported(&CODECAPI_AVLowLatencyMode) }.is_ok() {
            unsafe { codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true)) }
                .map_err(|e| native("H264 low latency", e))?;
        }
        let output = media_type(&config, &MFVideoFormat_H264)?;
        sdk("configure H264 output", || unsafe {
            output.SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate())?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, config.profile().idc())?;
            transform.SetOutputType(output_id, &output, 0)
        })?;
        let input = media_type(&config, &MFVideoFormat_NV12)?;
        sdk("configure NV12 input", || unsafe {
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, config.width())?;
            input.SetUINT32(&MF_MT_SAMPLE_SIZE, config.nv12_bytes() as u32)?;
            input.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
            transform.SetInputType(input_id, &input, 0)
        })?;
        let mut input_info = MFT_INPUT_STREAM_INFO::default();
        unsafe { transform.GetInputStreamInfo(input_id, &mut input_info) }
            .map_err(|e| native("H264 input stream info", e))?;
        validate_alignment(input_info.cbAlignment)?;
        if input_info.cbSize as usize > config.nv12_bytes() {
            return Err(Error::Unavailable(
                "H264 input stream requires a larger format buffer",
            ));
        }
        let output_info = unsafe { transform.GetOutputStreamInfo(output_id) }
            .map_err(|e| native("H264 output stream info", e))?;
        validate_output_info(&output_info)?;
        let current = unsafe { transform.GetOutputCurrentType(output_id) }
            .map_err(|e| native("H264 current output type", e))?;
        let configuration = read_configuration(&current, &config)?;
        sdk("start H264 streaming", || unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
        })?;
        Ok(Self {
            transform,
            config,
            configuration,
            input_id,
            output_id,
            input_alignment: input_info.cbAlignment,
            output_info,
            pending: VecDeque::with_capacity(MAX_PENDING),
            eos: false,
            drained: false,
            _context: context,
        })
    }
    fn pump(&mut self, packets: &mut Vec<AccessUnit>) -> Result<bool, Error> {
        // Bounded even when a broken MFT reports perpetual stream changes.
        for _ in 0..64 {
            let provides =
                self.output_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            let sample = if provides {
                None
            } else {
                Some(empty_sample(
                    self.output_info.cbSize,
                    self.output_info.cbAlignment,
                )?)
            };
            let mut output = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.output_id,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0;
            let result = unsafe {
                self.transform
                    .ProcessOutput(0, std::slice::from_mut(&mut output), &mut status)
            };
            // windows0.58 represents out COM references as ManuallyDrop. Take
            // exactly once on every HRESULT path, including failure/change.
            let sample = unsafe { ManuallyDrop::take(&mut output.pSample) };
            let events = unsafe { ManuallyDrop::take(&mut output.pEvents) };
            drop(events);
            match result {
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(true),
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    drop(sample);
                    let previous = self.configuration.clone();
                    self.renegotiate()?;
                    if !packets.is_empty() && self.configuration != previous {
                        return Err(Error::Protocol(
                            "sequence config changed within one output batch",
                        ));
                    }
                    continue;
                }
                Err(error) => return Err(native("H264 ProcessOutput", error)),
                Ok(()) => {}
            }
            let sample = sample.ok_or(Error::Protocol("successful H264 output has no sample"))?;
            if packets.len() >= MAX_PACKETS {
                return Err(Error::Limit("encoded packet count"));
            }
            let packet = self.packet(&sample)?;
            let total = self.configuration.retained_bytes()
                + packets.iter().map(|p| p.bytes().len()).sum::<usize>()
                + packet.bytes().len();
            if total > MAX_BATCH_BYTES {
                return Err(Error::Limit("encoded packet batch"));
            }
            packets.push(packet);
        }
        Err(Error::Limit("H264 output pump calls"))
    }
    fn renegotiate(&mut self) -> Result<(), Error> {
        // Inspect bounded available types and never accept dimensions/profile
        // changes just because native ProcessOutput requested renegotiation.
        for index in 0..16 {
            let candidate =
                match unsafe { self.transform.GetOutputAvailableType(self.output_id, index) } {
                    Ok(candidate) => candidate,
                    Err(error) if error.code() == MF_E_NO_MORE_TYPES => break,
                    Err(error) => return Err(native("H264 changed output type", error)),
                };
            if validate_media_type(&candidate, &self.config).is_err() {
                continue;
            }
            unsafe { self.transform.SetOutputType(self.output_id, &candidate, 0) }
                .map_err(|e| native("accept checked H264 output change", e))?;
            let current = unsafe { self.transform.GetOutputCurrentType(self.output_id) }
                .map_err(|e| native("changed H264 output current type", e))?;
            let mut configuration = read_configuration(&current, &self.config)?;
            configuration.inherit_color_provenance(&self.configuration);
            self.configuration = configuration;
            self.output_info = unsafe { self.transform.GetOutputStreamInfo(self.output_id) }
                .map_err(|e| native("changed H264 output info", e))?;
            validate_output_info(&self.output_info)?;
            return Ok(());
        }
        Err(Error::Unavailable(
            "H264 stream change has no matching bounded requested-profile type",
        ))
    }
    fn packet(&mut self, sample: &IMFSample) -> Result<AccessUnit, Error> {
        let pts = unsafe { sample.GetSampleTime() }.map_err(|e| native("H264 packet PTS", e))?;
        let expected = self
            .pending
            .front()
            .ok_or(Error::Protocol("H264 output without accepted input"))?;
        if expected.pts != pts {
            return Err(Error::Reordered);
        }
        let dts = match unsafe { sample.GetUINT64(&MFSampleExtension_DecodeTimestamp) } {
            Ok(value) => i64::try_from(value).map_err(|_| Error::Reordered)?,
            Err(error) if error.code() == MF_E_ATTRIBUTENOTFOUND => pts,
            Err(error) => return Err(native("H264 packet DTS", error)),
        };
        let duration = match unsafe { sample.GetSampleDuration() } {
            Ok(value) => value,
            Err(error) if error.code() == MF_E_NO_SAMPLE_DURATION => expected.duration,
            Err(error) => return Err(native("H264 packet duration", error)),
        };
        if duration != expected.duration {
            return Err(Error::Protocol("encoder changed submitted frame duration"));
        }
        // Check total length BEFORE ConvertToContiguousBuffer can allocate.
        let total = unsafe { sample.GetTotalLength() }
            .map_err(|e| native("H264 packet length", e))? as usize;
        if total == 0 || total > MAX_AU_BYTES {
            return Err(Error::Limit("native access unit"));
        }
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }
            .map_err(|e| native("H264 output contiguous buffer", e))?;
        let bytes = read_buffer(&buffer, total)?;
        let normalized = super::bitstream::normalize_access_unit(&bytes)?;
        let signaled = self.configuration.rewrite_sample(&normalized)?;
        let packet = AccessUnit::new(self.config.generation(), signaled, pts, dts, duration)?;
        // CleanPoint is advisory; the actual IDR NAL determines our keyframe.
        self.pending.pop_front();
        Ok(packet)
    }
}
impl Adapter for Encoder {
    fn configuration(&self) -> Option<&CodecConfiguration> {
        Some(&self.configuration)
    }
    fn encode(&mut self, input: Nv12Input) -> Result<Vec<AccessUnit>, Error> {
        input.validate(&self.config)?;
        if self.eos {
            return Err(Error::Protocol("H264 input after EOS"));
        }
        if self.pending.len() >= MAX_PENDING {
            return Err(Error::Limit("native accepted-frame backlog"));
        }
        let sample = input_sample(&input, self.input_alignment)?;
        let mut packets = Vec::new();
        match unsafe { self.transform.ProcessInput(self.input_id, &sample, 0) } {
            Ok(()) => {}
            Err(error) if error.code() == MF_E_NOTACCEPTING => {
                self.pump(&mut packets)?;
                if packets.is_empty() {
                    return Err(Error::Protocol("H264 NOTACCEPTING without output progress"));
                }
                unsafe { self.transform.ProcessInput(self.input_id, &sample, 0) }
                    .map_err(|e| native("H264 retry ProcessInput after output progress", e))?;
            }
            Err(error) => return Err(native("H264 ProcessInput", error)),
        }
        self.pending.push_back(Stamp {
            pts: input.pts(),
            duration: input.duration(),
        });
        self.pump(&mut packets)?;
        Ok(packets)
    }
    fn drain(&mut self) -> Result<Vec<AccessUnit>, Error> {
        if self.eos {
            return Err(Error::Protocol("H264 EOS already sent"));
        }
        self.eos = true;
        sdk("H264 EOS/drain", || unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, self.input_id as usize)?;
            self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
        })?;
        let mut packets = Vec::new();
        if self.pump(&mut packets)? && !self.pending.is_empty() {
            return Err(Error::Protocol(
                "H264 drain ended with unmatched accepted frames",
            ));
        }
        self.drained = true;
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
        }
        .map_err(|e| native("H264 end streaming", e))?;
        Ok(packets)
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        if !self.drained {
            unsafe {
                let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
                let _ = self
                    .transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            }
        }
    }
}

fn set_u32(codec: &ICodecAPI, key: &windows::core::GUID, value: u32) -> Result<(), Error> {
    unsafe { codec.SetValue(key, &VARIANT::from(value)) }
        .map_err(|e| native("set H264 codec property", e))?;
    let observed =
        unsafe { codec.GetValue(key) }.map_err(|e| native("read H264 codec property", e))?;
    if u32::try_from(&observed).map_err(|e| native("H264 codec property type", e))? != value {
        return Err(Error::Unavailable(
            "H264 codec did not retain requested property",
        ));
    }
    Ok(())
}
fn media_type(config: &Config, subtype: &windows::core::GUID) -> Result<IMFMediaType, Error> {
    let ty = unsafe { MFCreateMediaType() }.map_err(|e| native("create H264 media type", e))?;
    sdk("H264 media attributes", || unsafe {
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        ty.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        ty.SetUINT64(
            &MF_MT_FRAME_SIZE,
            (u64::from(config.width()) << 32) | u64::from(config.height()),
        )?;
        let (num, den) = config.fps();
        ty.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(num) << 32) | u64::from(den))?;
        ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        ty.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        ty.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        ty.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        ty.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        ty.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)
    })?;
    Ok(ty)
}
fn validate_media_type(ty: &IMFMediaType, config: &Config) -> Result<(), Error> {
    let matches = sdk("inspect H264 negotiated media type", || unsafe {
        Ok(ty.GetGUID(&MF_MT_MAJOR_TYPE)? == MFMediaType_Video
            && ty.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_H264
            && ty.GetUINT64(&MF_MT_FRAME_SIZE)?
                == ((u64::from(config.width()) << 32) | u64::from(config.height()))
            && ty.GetUINT32(&MF_MT_MPEG2_PROFILE)? == config.profile().idc()
            && ty.GetUINT64(&MF_MT_FRAME_RATE)?
                == ((u64::from(config.fps().0) << 32) | u64::from(config.fps().1))
            && ty.GetUINT32(&MF_MT_INTERLACE_MODE)? == MFVideoInterlace_Progressive.0 as u32)
    })?;
    if matches {
        Ok(())
    } else {
        Err(Error::Unavailable(
            "H264 negotiated format differs from requested profile",
        ))
    }
}
fn read_configuration(ty: &IMFMediaType, config: &Config) -> Result<CodecConfiguration, Error> {
    validate_media_type(ty, config)?;
    let size = unsafe { ty.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) }
        .map_err(|e| native("H264 sequence header size", e))? as usize;
    if size == 0 || size > MAX_HEADER_BYTES {
        return Err(Error::Limit("native sequence header"));
    }
    let mut bytes = vec![0; size];
    let mut actual = 0;
    unsafe { ty.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut bytes, Some(&mut actual)) }
        .map_err(|e| native("H264 sequence header", e))?;
    if actual as usize != size {
        return Err(Error::Protocol("H264 sequence header size changed"));
    }
    CodecConfiguration::parse_profile(&bytes,config.profile())
}
fn validate_alignment(value: u32) -> Result<(), Error> {
    if value == 0 || (value.is_power_of_two() && value <= 4096) {
        Ok(())
    } else {
        Err(Error::Unavailable("unsupported H264 buffer alignment"))
    }
}
fn validate_output_info(info: &MFT_OUTPUT_STREAM_INFO) -> Result<(), Error> {
    validate_alignment(info.cbAlignment)?;
    if info.cbSize as usize > MAX_AU_BYTES {
        return Err(Error::Limit("native requested output buffer"));
    }
    if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 && info.cbSize == 0 {
        return Err(Error::Unavailable(
            "H264 transform requests zero-sized caller output",
        ));
    }
    Ok(())
}
fn empty_sample(size: u32, alignment: u32) -> Result<IMFSample, Error> {
    validate_alignment(alignment)?;
    let sample = unsafe { MFCreateSample() }.map_err(|e| native("create H264 sample", e))?;
    let buffer = if alignment > 1 {
        unsafe { MFCreateAlignedMemoryBuffer(size, alignment - 1) }
    } else {
        unsafe { MFCreateMemoryBuffer(size) }
    }
    .map_err(|e| native("allocate H264 sample buffer", e))?;
    unsafe { sample.AddBuffer(&buffer) }.map_err(|e| native("attach H264 sample buffer", e))?;
    Ok(sample)
}
fn input_sample(input: &Nv12Input, alignment: u32) -> Result<IMFSample, Error> {
    let sample = empty_sample(input.bytes().len() as u32, alignment)?;
    let buffer =
        unsafe { sample.GetBufferByIndex(0) }.map_err(|e| native("H264 input buffer", e))?;
    let mut ptr = std::ptr::null_mut();
    let mut maximum = 0;
    unsafe { buffer.Lock(&mut ptr, Some(&mut maximum), None) }
        .map_err(|e| native("lock H264 input", e))?;
    let valid = !ptr.is_null() && maximum as usize >= input.bytes().len();
    if valid {
        unsafe { std::ptr::copy_nonoverlapping(input.bytes().as_ptr(), ptr, input.bytes().len()) };
    }
    let unlock = unsafe { buffer.Unlock() };
    if !valid {
        return Err(Error::Protocol("native NV12 buffer bounds"));
    }
    unlock.map_err(|e| native("unlock H264 input", e))?;
    sdk("H264 input sample metadata", || unsafe {
        buffer.SetCurrentLength(input.bytes().len() as u32)?;
        sample.SetSampleTime(input.pts())?;
        sample.SetSampleDuration(input.duration())
    })?;
    Ok(sample)
}
fn read_buffer(buffer: &IMFMediaBuffer, expected: usize) -> Result<Vec<u8>, Error> {
    let mut ptr = std::ptr::null_mut();
    let mut actual = 0;
    let mut maximum = 0;
    unsafe { buffer.Lock(&mut ptr, Some(&mut maximum), Some(&mut actual)) }
        .map_err(|e| native("lock H264 output", e))?;
    let valid = !ptr.is_null() && actual as usize == expected && actual <= maximum;
    let bytes = if valid {
        unsafe { std::slice::from_raw_parts(ptr, actual as usize) }.to_vec()
    } else {
        Vec::new()
    };
    let unlock = unsafe { buffer.Unlock() };
    if !valid {
        return Err(Error::Protocol("native encoded buffer bounds"));
    }
    unlock.map_err(|e| native("unlock H264 output", e))?;
    Ok(bytes)
}
