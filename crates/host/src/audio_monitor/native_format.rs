//! Shared-mode native mix format validation mirrors the existing capture port.
use crate::audio_capture::{AudioCaptureError,AudioCaptureLimits,NativeAudioFormat,PcmEncoding};
use windows::{Win32::Media::Audio::{WAVEFORMATEX,WAVEFORMATEXTENSIBLE},core::GUID};
use std::mem::size_of;
const WAVE_PCM:u16=1;const WAVE_FLOAT:u16=3;const WAVE_EXTENSIBLE:u16=0xfffe;
const EXTENSION_BYTES:u16=(size_of::<WAVEFORMATEXTENSIBLE>()-size_of::<WAVEFORMATEX>())as u16;
const SUBTYPE_PCM:GUID=GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const SUBTYPE_FLOAT:GUID=GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
#[derive(Clone, Copy)]
pub(super) struct MixFormat {
    pub(super) format: NativeAudioFormat,
    pub(super) encoding: PcmEncoding,
    pub(super) block_align: usize,
}
pub(super) fn parse_format(
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
