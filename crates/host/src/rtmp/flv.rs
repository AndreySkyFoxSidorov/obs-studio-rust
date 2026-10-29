//! Explicit AVC/AAC FLV payloads. No Annex-B/ADTS guesses or encoder dependencies.
use super::{Error, chunk::Message};
use std::sync::Arc;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation {
    pub session: u64,
    pub output: u64,
    pub encoder: u64,
}
impl Generation {
    pub fn validate(self) -> Result<(), Error> {
        if self.session == 0 || self.output == 0 || self.encoder == 0 {
            Err(Error::Invalid("generation components must be nonzero"))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug)]
pub struct VideoConfig {
    bytes: Arc<[u8]>,
}
impl VideoConfig {
    pub fn new(bytes: Arc<[u8]>) -> Result<Self, Error> {
        if bytes.len() > 32768 {
            return Err(Error::Limit("AVC configuration bytes"));
        }
        if bytes.len() < 7
            || bytes[0] != 1
            || bytes[1] != 66
            || bytes[4] != 0xff
            || bytes[5] & 0xe0 != 0xe0
        {
            return Err(Error::Unsupported(
                "initial AVC profile requires Baseline avcC and4-byte lengths",
            ));
        }
        let mut at = 6;
        let sps = bytes[5] & 31;
        if sps == 0 {
            return Err(Error::Invalid("missing SPS"));
        }
        fn units(bytes: &[u8], at: &mut usize, count: u8, kind: u8) -> Result<(), Error> {
            for _ in 0..count {
                let len = u16::from_be_bytes(
                    bytes
                        .get(*at..*at + 2)
                        .ok_or(Error::Invalid("truncated AVC configuration"))?
                        .try_into()
                        .unwrap(),
                ) as usize;
                *at += 2;
                let unit = bytes
                    .get(
                        *at..at
                            .checked_add(len)
                            .ok_or(Error::Limit("AVC config overflow"))?,
                    )
                    .ok_or(Error::Invalid("truncated AVC parameter set"))?;
                if unit.is_empty() || unit[0] & 31 != kind {
                    return Err(Error::Invalid("wrong AVC parameter set type"));
                }
                *at += len;
            }
            Ok(())
        }
        units(&bytes, &mut at, sps, 7)?;
        let pps = *bytes.get(at).ok_or(Error::Invalid("missing PPS count"))?;
        at += 1;
        if pps == 0 || pps > 32 {
            return Err(Error::Limit("PPS count"));
        }
        units(&bytes, &mut at, pps, 8)?;
        if at != bytes.len() {
            return Err(Error::Unsupported("AVC configuration extension"));
        }
        Ok(Self { bytes })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Clone, Debug)]
pub struct AudioConfig {
    bytes: Arc<[u8]>,
    channels: u16,
    sample_rate: u32,
}
impl AudioConfig {
    pub fn new(bytes: Arc<[u8]>) -> Result<Self, Error> {
        if bytes.len() < 2 || bytes.len() > 64 {
            return Err(Error::Limit("AAC configuration bytes"));
        }
        // Initial GA AAC-LC ASC: explicit SBR, PCE and960-frame profiles are not inferred.
        let aot = bytes[0] >> 3;
        let index = ((bytes[0] & 7) << 1) | (bytes[1] >> 7);
        let channels = (bytes[1] >> 3) & 15;
        if aot != 2
            || !matches!(index, 3 | 4)
            || !matches!(channels, 1 | 2)
            || bytes[1] & 7 != 0
            || bytes[2..].iter().any(|b| *b != 0)
        {
            return Err(Error::Unsupported(
                "AAC-LC 44.1/48k mono/stereo ASC required",
            ));
        }
        Ok(Self {
            bytes,
            channels: u16::from(channels),
            sample_rate: if index == 3 { 48000 } else { 44100 },
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn channels(&self) -> u16 {
        self.channels
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}
#[derive(Clone, Debug)]
pub struct VideoPacket {
    generation: Generation,
    dts_ns: u64,
    pts_ns: i128,
    bytes: Arc<[u8]>,
    keyframe: bool,
}
impl VideoPacket {
    pub fn new(
        generation: Generation,
        dts_ns: u64,
        pts_ns: i128,
        bytes: Arc<[u8]>,
    ) -> Result<Self, Error> {
        generation.validate()?;
        if bytes.is_empty() || bytes.len() > 2 * 1024 * 1024 {
            return Err(Error::Limit("AVC access unit bytes"));
        }
        let mut at = 0;
        let mut count = 0;
        let mut vcl = false;
        let mut keyframe = false;
        while at < bytes.len() {
            let len = u32::from_be_bytes(
                bytes
                    .get(at..at + 4)
                    .ok_or(Error::Invalid("truncated AVCC length"))?
                    .try_into()
                    .unwrap(),
            ) as usize;
            at += 4;
            let end = at
                .checked_add(len)
                .ok_or(Error::Limit("AVCC length overflow"))?;
            let nal = bytes
                .get(at..end)
                .ok_or(Error::Invalid("truncated AVCC NAL"))?;
            count += 1;
            if count > 128 {
                return Err(Error::Limit("AVCC NAL count"));
            }
            if nal.is_empty() || nal[0] & 128 != 0 {
                return Err(Error::Invalid("invalid NAL header"));
            }
            let kind = nal[0] & 31;
            vcl |= matches!(kind, 1 | 5);
            keyframe |= kind == 5;
            at = end;
        }
        if !vcl {
            return Err(Error::Invalid("AVC AU has no VCL NAL"));
        }
        Ok(Self {
            generation,
            dts_ns,
            pts_ns,
            bytes,
            keyframe,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn keyframe(&self) -> bool {
        self.keyframe
    }
}
#[derive(Clone, Debug)]
pub struct AudioPacket {
    generation: Generation,
    dts_ns: u64,
    bytes: Arc<[u8]>,
}
impl AudioPacket {
    pub fn new(generation: Generation, dts_ns: u64, bytes: Arc<[u8]>) -> Result<Self, Error> {
        generation.validate()?;
        if bytes.is_empty() || bytes.len() > 65536 {
            return Err(Error::Limit("raw AAC access unit bytes"));
        }
        Ok(Self {
            generation,
            dts_ns,
            bytes,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Clone, Debug)]
pub enum EncodedPacket {
    Video(VideoPacket),
    Audio(AudioPacket),
}
impl EncodedPacket {
    pub fn generation(&self) -> Generation {
        match self {
            Self::Video(p) => p.generation,
            Self::Audio(p) => p.generation,
        }
    }
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Video(p) => p.bytes.len(),
            Self::Audio(p) => p.bytes.len(),
        }
    }
    pub fn dts_ns(&self) -> u64 {
        match self {
            Self::Video(p) => p.dts_ns,
            Self::Audio(p) => p.dts_ns,
        }
    }
    pub(crate) fn message(
        &self,
        origin: u64,
        stream: u32,
        audio: Option<&AudioConfig>,
    ) -> Result<Message, Error> {
        let dts = self
            .dts_ns()
            .checked_sub(origin)
            .ok_or(Error::Invalid("packet precedes publisher timestamp origin"))?
            / 1_000_000;
        let timestamp = u32::try_from(dts)
            .map_err(|_| Error::Limit("RTMP timestamp exceedsu32 milliseconds"))?;
        let (type_id, data) = match self {
            Self::Video(p) => {
                let pts = p
                    .pts_ns
                    .checked_sub(i128::from(origin))
                    .ok_or(Error::Invalid("PTS origin overflow"))?
                    .div_euclid(1_000_000);
                let offset = pts - i128::from(dts);
                if !(-8388608..=8388607).contains(&offset) {
                    return Err(Error::Limit("AVC signed24 composition time"));
                }
                let cts = (offset as i32).to_be_bytes();
                let mut data = Vec::with_capacity(p.bytes.len() + 5);
                data.extend_from_slice(&[
                    if p.keyframe { 0x17 } else { 0x27 },
                    1,
                    cts[1],
                    cts[2],
                    cts[3],
                ]);
                data.extend_from_slice(&p.bytes);
                (9, data)
            }
            Self::Audio(p) => {
                let config = audio.ok_or(Error::Invalid("AAC packet without AAC configuration"))?;
                let mut data = Vec::with_capacity(p.bytes.len() + 2);
                data.extend_from_slice(&[audio_header(config), 1]);
                data.extend_from_slice(&p.bytes);
                (8, data)
            }
        };
        Ok(Message {
            timestamp,
            type_id,
            stream,
            data,
        })
    }
}
fn audio_header(config: &AudioConfig) -> u8 {
    0xae | u8::from(config.channels > 1)
}
pub(crate) fn video_header(config: &VideoConfig, stream: u32) -> Message {
    let mut data = vec![0x17, 0, 0, 0, 0];
    data.extend_from_slice(config.bytes());
    Message {
        timestamp: 0,
        type_id: 9,
        stream,
        data,
    }
}
pub(crate) fn audio_sequence(config: &AudioConfig, stream: u32) -> Message {
    let mut data = vec![audio_header(config), 0];
    data.extend_from_slice(config.bytes());
    Message {
        timestamp: 0,
        type_id: 8,
        stream,
        data,
    }
}
pub(crate) fn video_end(stream: u32, timestamp: u32) -> Message {
    Message {
        timestamp,
        type_id: 9,
        stream,
        data: vec![0x17, 2, 0, 0, 0],
    }
}
