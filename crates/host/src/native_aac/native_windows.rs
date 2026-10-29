//! Synchronous Microsoft AAC MFT. All COM calls/retirement stay on one worker.
use super::pcm::{Assembler, PcmBlock};
use super::{
    AccessUnit, Adapter, CodecConfiguration, Config, Error, FRAME_LEN, MAX_AU_BYTES,
    MAX_BATCH_BYTES, MAX_NATIVE_HEADER_BYTES, MAX_PACKETS, MAX_PENDING_BLOCKS, PcmInput,
    StreamStats,
};
use std::{mem::ManuallyDrop, rc::Rc};
use windows::{
    Win32::{Foundation::E_NOTIMPL, Media::MediaFoundation::*, System::Com::*},
    core::GUID,
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
    _permit:super::admission::Permit,
}
impl Context {
    fn new(permit:super::admission::Permit) -> Result<Self, Error> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| native("AAC CoInitializeEx", e))?;
        if let Err(error) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            unsafe { CoUninitialize() };
            return Err(native("AAC MFStartup", error));
        }
        Ok(Self {
            _not_send: Rc::new(()),
            _permit:permit,
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
pub(super) struct Encoder {
    transform: IMFTransform,
    config: Config,
    configuration: CodecConfiguration,
    input_id: u32,
    output_id: u32,
    input_alignment: u32,
    output_info: MFT_OUTPUT_STREAM_INFO,
    assembler: Assembler,
    accepted_blocks: u64,
    packets: u64,
    first_pts: Option<i64>,
    last_pts: Option<i64>,
    last_end: Option<i64>,
    eos: bool,
    drained: bool,
    // Last field: native interfaces and sample storage retire before COM/MF.
    _context: Context,
}
impl Encoder {
    pub(super) fn new(config: Config) -> Result<Self, Error> {
        Self::with_permit(config,super::admission::one()?)
    }
    pub(super) fn with_permit(config:Config,permit:super::admission::Permit)->Result<Self,Error>{
        let context = Context::new(permit)?;
        let transform: IMFTransform =
            unsafe { CoCreateInstance(&AACMFTEncoder, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| native("create Microsoft AAC encoder", e))?;
        match unsafe { transform.GetAttributes() } {
            Ok(attributes) => match unsafe { attributes.GetUINT32(&MF_TRANSFORM_ASYNC) } {
                Ok(0) => {}
                Ok(_) => return Err(Error::Unavailable("asynchronous AAC MFT unsupported")),
                Err(e) if e.code() == MF_E_ATTRIBUTENOTFOUND => {}
                Err(e) => return Err(native("AAC synchronous attribute", e)),
            },
            Err(e) if e.code() == E_NOTIMPL => {}
            Err(e) => return Err(native("AAC transform attributes", e)),
        }
        let (mut inputs, mut outputs) = (0, 0);
        unsafe { transform.GetStreamCount(&mut inputs, &mut outputs) }
            .map_err(|e| native("AAC stream count", e))?;
        if inputs != 1 || outputs != 1 {
            return Err(Error::Unavailable("AAC MFT requires one input/output"));
        }
        let (mut input_ids, mut output_ids) = ([0], [0]);
        match unsafe { transform.GetStreamIDs(&mut input_ids, &mut output_ids) } {
            Ok(()) => {}
            Err(e) if e.code() == E_NOTIMPL => {}
            Err(e) => return Err(native("AAC stream IDs", e)),
        }
        let (input_id, output_id) = (input_ids[0], output_ids[0]);
        let output = audio_type(&config, &MFAudioFormat_AAC)?;
        sdk("configure AAC output", || unsafe {
            output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, config.bitrate() / 8)?;
            output.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 1)?;
            output.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            output.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
            transform.SetOutputType(output_id, &output, 0)
        })?;
        let input = audio_type(&config, &MFAudioFormat_PCM)?;
        sdk("configure PCM16 input", || unsafe {
            input.SetUINT32(
                &MF_MT_AUDIO_BLOCK_ALIGNMENT,
                u32::from(config.channels()) * 2,
            )?;
            input.SetUINT32(
                &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                config.sample_rate() * u32::from(config.channels()) * 2,
            )?;
            transform.SetInputType(input_id, &input, 0)
        })?;
        let mut input_info = MFT_INPUT_STREAM_INFO::default();
        unsafe { transform.GetInputStreamInfo(input_id, &mut input_info) }
            .map_err(|e| native("AAC input info", e))?;
        validate_alignment(input_info.cbAlignment)?;
        if input_info.cbSize as usize > FRAME_LEN * usize::from(config.channels()) * 2 {
            return Err(Error::Unavailable(
                "AAC input buffer exceeds one1024-frame block",
            ));
        }
        let output_info = unsafe { transform.GetOutputStreamInfo(output_id) }
            .map_err(|e| native("AAC output info", e))?;
        validate_output_info(&output_info)?;
        let current = unsafe { transform.GetOutputCurrentType(output_id) }
            .map_err(|e| native("AAC output type", e))?;
        let configuration = read_configuration(&current, &config)?;
        sdk("begin AAC streaming", || unsafe {
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
        })?;
        Ok(Self {
            transform,
            assembler: Assembler::new(config.clone()),
            config,
            configuration,
            input_id,
            output_id,
            input_alignment: input_info.cbAlignment,
            output_info,
            accepted_blocks: 0,
            packets: 0,
            first_pts: None,
            last_pts: None,
            last_end: None,
            eos: false,
            drained: false,
            _context: context,
        })
    }
    fn pump(&mut self, packets: &mut Vec<AccessUnit>) -> Result<(), Error> {
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
            // Take each COM reference once on every HRESULT path.
            let sample = unsafe { ManuallyDrop::take(&mut output.pSample) };
            let events = unsafe { ManuallyDrop::take(&mut output.pEvents) };
            drop(events);
            match result {
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    drop(sample);
                    self.renegotiate()?;
                    continue;
                }
                Err(e) => return Err(native("AAC ProcessOutput", e)),
                Ok(()) => {}
            }
            if packets.len() >= MAX_PACKETS {
                return Err(Error::Limit("AAC output packet count"));
            }
            if output.dwStatus & MFT_OUTPUT_DATA_BUFFER_NO_SAMPLE.0 as u32
                == MFT_OUTPUT_DATA_BUFFER_NO_SAMPLE.0 as u32
            {
                return Err(Error::Protocol("AAC success without sample"));
            }
            let sample = sample.ok_or(Error::Protocol("AAC output sample missing"))?;
            let packet = self.packet(&sample)?;
            let total = self.configuration.retained_bytes()
                + std::mem::size_of::<super::PacketBatch>()
                + (packets.len() + 1) * std::mem::size_of::<AccessUnit>()
                + packets
                    .iter()
                    .map(|packet| packet.bytes().len())
                    .sum::<usize>()
                + packet.bytes().len();
            if total > MAX_BATCH_BYTES {
                return Err(Error::Limit("AAC batch bytes"));
            }
            packets.push(packet);
        }
        Err(Error::Limit("AAC output pump calls"))
    }
    fn renegotiate(&mut self) -> Result<(), Error> {
        for index in 0..16 {
            let candidate =
                match unsafe { self.transform.GetOutputAvailableType(self.output_id, index) } {
                    Ok(candidate) => candidate,
                    Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                    Err(e) => return Err(native("AAC stream change candidates", e)),
                };
            if validate_type(&candidate, &self.config).is_err() {
                continue;
            }
            unsafe { self.transform.SetOutputType(self.output_id, &candidate, 0) }
                .map_err(|e| native("AAC set changed output", e))?;
            let current = unsafe { self.transform.GetOutputCurrentType(self.output_id) }
                .map_err(|e| native("AAC changed current type", e))?;
            let configuration = read_configuration(&current, &self.config)?;
            if configuration != self.configuration {
                return Err(Error::Protocol("AAC ASC changed in an encoder generation"));
            }
            self.output_info = unsafe { self.transform.GetOutputStreamInfo(self.output_id) }
                .map_err(|e| native("AAC changed output info", e))?;
            validate_output_info(&self.output_info)?;
            return Ok(());
        }
        Err(Error::Unavailable(
            "AAC stream change has no matching output profile",
        ))
    }
    fn packet(&mut self, sample: &IMFSample) -> Result<AccessUnit, Error> {
        let pts = unsafe { sample.GetSampleTime() }.map_err(|e| native("AAC packet PTS", e))?;
        let dts = match unsafe { sample.GetUINT64(&MFSampleExtension_DecodeTimestamp) } {
            Ok(value) => i64::try_from(value).map_err(|_| Error::Protocol("AAC DTS overflow"))?,
            Err(e) if e.code() == MF_E_ATTRIBUTENOTFOUND => pts,
            Err(e) => return Err(native("AAC packet DTS", e)),
        };
        let duration =
            unsafe { sample.GetSampleDuration() }.map_err(|e| native("AAC packet duration", e))?;
        if super::validate_native_timing(pts, dts, duration, self.last_pts).is_err() {
            return Err(Error::Timing {
                pts,
                dts,
                duration,
                previous_pts: self.last_pts,
                accepted_blocks: self.accepted_blocks,
                packets: self.packets,
                native_bytes: unsafe { sample.GetTotalLength() }
                    .map_err(|e| native("AAC timing diagnostic length", e))?,
                native_buffers: unsafe { sample.GetBufferCount() }
                    .map_err(|e| native("AAC timing diagnostic buffer count", e))?,
            });
        }
        // LC frame content and bounded lookahead/drain accounting are
        // independent of the SDK's observed presentation-duration rounding.
        let next = super::next_packet_count(self.packets, self.accepted_blocks, self.eos)?;
        let length = unsafe { sample.GetTotalLength() }
            .map_err(|e| native("AAC packet length", e))? as usize;
        if length == 0 || length > MAX_AU_BYTES {
            return Err(Error::Limit("native AAC AU"));
        }
        #[cfg(test)]
        if super::timing_trace_enabled() {
            let buffers = unsafe { sample.GetBufferCount() }
                .map_err(|e| native("AAC timing probe buffer count", e))?;
            eprintln!(
                "AAC_NATIVE_TIMING_TRACE channels={} seq={} pts={} dts={} duration={} previous_pts={:?} accepted_blocks={} packets={} native_bytes={} native_buffers={}",
                self.config.channels(),
                self.packets,
                pts,
                dts,
                duration,
                self.last_pts,
                self.accepted_blocks,
                self.packets,
                length,
                buffers
            );
        }
        let buffer = unsafe { sample.ConvertToContiguousBuffer() }
            .map_err(|e| native("AAC contiguous output", e))?;
        let bytes = read_buffer(&buffer, length)?;
        let packet = AccessUnit::new(
            self.config.generation(),
            self.packets,
            bytes,
            pts,
            dts,
            duration,
        )?;
        self.packets = next;
        self.first_pts.get_or_insert(pts);
        self.last_pts = Some(pts);
        self.last_end = Some(pts + duration);
        Ok(packet)
    }
    fn send_block(&mut self, block: PcmBlock, packets: &mut Vec<AccessUnit>) -> Result<(), Error> {
        if self.accepted_blocks.saturating_sub(self.packets) >= MAX_PENDING_BLOCKS {
            self.pump(packets)?;
            if self.accepted_blocks.saturating_sub(self.packets) >= MAX_PENDING_BLOCKS {
                return Err(Error::Limit("AAC accepted input backlog"));
            }
        }
        let next = self
            .accepted_blocks
            .checked_add(1)
            .ok_or(Error::Limit("AAC accepted counter"))?;
        let sample = input_sample(&block, &self.config, self.input_alignment)?;
        match unsafe { self.transform.ProcessInput(self.input_id, &sample, 0) } {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_NOTACCEPTING => {
                let before = packets.len();
                self.pump(packets)?;
                if packets.len() == before {
                    return Err(Error::Protocol("AAC NOTACCEPTING without output progress"));
                }
                unsafe { self.transform.ProcessInput(self.input_id, &sample, 0) }
                    .map_err(|e| native("AAC retry ProcessInput", e))?;
            }
            Err(e) => return Err(native("AAC ProcessInput", e)),
        }
        self.accepted_blocks = next;
        self.pump(packets)
    }
}
impl Adapter for Encoder {
    fn configuration(&self) -> &CodecConfiguration {
        &self.configuration
    }
    fn stats(&self) -> StreamStats {
        StreamStats {
            input_frames: self.assembler.received,
            submitted_frames: self.accepted_blocks * FRAME_LEN as u64,
            explicit_padding_frames: self.assembler.padding,
            packets: self.packets,
            first_native_pts: self.first_pts,
            last_native_end: self.last_end,
        }
    }
    fn encode(&mut self, input: PcmInput) -> Result<Vec<AccessUnit>, Error> {
        if self.eos {
            return Err(Error::Protocol("AAC input after EOS"));
        }
        let mut packets = Vec::with_capacity(MAX_PACKETS);
        if let Some(block) = self.assembler.push(input)? {
            self.send_block(block, &mut packets)?;
        }
        Ok(packets)
    }
    fn drain(&mut self) -> Result<Vec<AccessUnit>, Error> {
        if self.eos {
            return Err(Error::Protocol("AAC EOS already sent"));
        }
        let mut packets = Vec::with_capacity(MAX_PACKETS);
        if let Some(block) = self.assembler.finish()? {
            self.send_block(block, &mut packets)?;
        }
        self.eos = true;
        sdk("AAC EOS/drain", || unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, self.input_id as usize)?;
            self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
        })?;
        self.pump(&mut packets)?;
        if self.packets < self.accepted_blocks {
            return Err(Error::Protocol(
                "AAC drain ended before coded output covered submitted PCM",
            ));
        }
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
        }
        .map_err(|e| native("AAC end streaming", e))?;
        self.drained = true;
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
fn audio_type(config: &Config, subtype: &GUID) -> Result<IMFMediaType, Error> {
    let ty = unsafe { MFCreateMediaType() }.map_err(|e| native("create AAC media type", e))?;
    sdk("AAC audio attributes", || unsafe {
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        ty.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        ty.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, config.sample_rate())?;
        ty.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(config.channels()))?;
        ty.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        ty.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)
    })?;
    Ok(ty)
}
fn validate_type(ty: &IMFMediaType, config: &Config) -> Result<(), Error> {
    let valid = sdk("inspect AAC media type", || unsafe {
        Ok(ty.GetGUID(&MF_MT_MAJOR_TYPE)? == MFMediaType_Audio
            && ty.GetGUID(&MF_MT_SUBTYPE)? == MFAudioFormat_AAC
            && ty.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)? == config.sample_rate()
            && ty.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)? == u32::from(config.channels())
            && ty.GetUINT32(&MF_MT_AAC_PAYLOAD_TYPE)? == 0
            && ty.GetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND)? == config.bitrate() / 8)
    })?;
    if valid {
        Ok(())
    } else {
        Err(Error::Unavailable(
            "negotiated AAC format differs from requested raw LC profile",
        ))
    }
}
fn read_configuration(ty: &IMFMediaType, config: &Config) -> Result<CodecConfiguration, Error> {
    validate_type(ty, config)?;
    let size = unsafe { ty.GetBlobSize(&MF_MT_USER_DATA) }
        .map_err(|e| native("AAC config size", e))? as usize;
    if !(14..=MAX_NATIVE_HEADER_BYTES).contains(&size) {
        return Err(Error::Limit("native AAC config header"));
    }
    let mut bytes = vec![0; size];
    let mut actual = 0;
    unsafe { ty.GetBlob(&MF_MT_USER_DATA, &mut bytes, Some(&mut actual)) }
        .map_err(|e| native("AAC config blob", e))?;
    if actual as usize != size {
        return Err(Error::Protocol("AAC config size changed"));
    }
    CodecConfiguration::from_native(&bytes, config)
}
fn validate_alignment(value: u32) -> Result<(), Error> {
    if value == 0 || (value.is_power_of_two() && value <= 4096) {
        Ok(())
    } else {
        Err(Error::Unavailable("unsupported AAC buffer alignment"))
    }
}
fn validate_output_info(info: &MFT_OUTPUT_STREAM_INFO) -> Result<(), Error> {
    validate_alignment(info.cbAlignment)?;
    if info.cbSize as usize > MAX_AU_BYTES {
        return Err(Error::Limit("native AAC output buffer"));
    }
    if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 && info.cbSize == 0 {
        return Err(Error::Unavailable("AAC caller output buffer is zero"));
    }
    Ok(())
}
fn empty_sample(size: u32, alignment: u32) -> Result<IMFSample, Error> {
    validate_alignment(alignment)?;
    let sample = unsafe { MFCreateSample() }.map_err(|e| native("AAC create sample", e))?;
    let buffer = if alignment > 1 {
        unsafe { MFCreateAlignedMemoryBuffer(size, alignment - 1) }
    } else {
        unsafe { MFCreateMemoryBuffer(size) }
    }
    .map_err(|e| native("AAC allocate sample", e))?;
    unsafe { sample.AddBuffer(&buffer) }.map_err(|e| native("AAC attach buffer", e))?;
    Ok(sample)
}
fn input_sample(block: &PcmBlock, config: &Config, alignment: u32) -> Result<IMFSample, Error> {
    let bytes = block.samples.len() * 2;
    let sample = empty_sample(bytes as u32, alignment)?;
    let buffer =
        unsafe { sample.GetBufferByIndex(0) }.map_err(|e| native("AAC input buffer", e))?;
    let (mut ptr, mut maximum) = (std::ptr::null_mut(), 0);
    unsafe { buffer.Lock(&mut ptr, Some(&mut maximum), None) }
        .map_err(|e| native("AAC lock input", e))?;
    let valid = !ptr.is_null() && maximum as usize >= bytes;
    if valid {
        for (index, sample) in block.samples.iter().enumerate() {
            let encoded = sample.to_le_bytes();
            unsafe { std::ptr::copy_nonoverlapping(encoded.as_ptr(), ptr.add(index * 2), 2) };
        }
    }
    let unlock = unsafe { buffer.Unlock() };
    if !valid {
        return Err(Error::Protocol("AAC native input buffer bounds"));
    }
    unlock.map_err(|e| native("AAC unlock input", e))?;
    let (pts, duration) = config.sample_timing(block.first_frame, FRAME_LEN as u64)?;
    sdk("AAC input timing", || unsafe {
        buffer.SetCurrentLength(bytes as u32)?;
        sample.SetSampleTime(pts)?;
        sample.SetSampleDuration(duration)
    })?;
    Ok(sample)
}
fn read_buffer(buffer: &IMFMediaBuffer, expected: usize) -> Result<Vec<u8>, Error> {
    let (mut ptr, mut maximum, mut actual) = (std::ptr::null_mut(), 0, 0);
    unsafe { buffer.Lock(&mut ptr, Some(&mut maximum), Some(&mut actual)) }
        .map_err(|e| native("AAC lock output", e))?;
    let valid = !ptr.is_null() && actual as usize == expected && actual <= maximum;
    let bytes = if valid {
        unsafe { std::slice::from_raw_parts(ptr, actual as usize) }.to_vec()
    } else {
        Vec::new()
    };
    let unlock = unsafe { buffer.Unlock() };
    if !valid {
        return Err(Error::Protocol("AAC native output buffer bounds"));
    }
    unlock.map_err(|e| native("AAC unlock output", e))?;
    Ok(bytes)
}
