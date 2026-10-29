use super::{AudioCaptureError, AudioCaptureLimits, NativeAudioFormat};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmEncoding {
    Unsigned8,
    UnsignedIn8 { valid_bits: u16 },
    Signed16,
    SignedIn16 { valid_bits: u16 },
    Signed24,
    Signed32,
    SignedIn32 { valid_bits: u16 },
    SignedIn24 { valid_bits: u16 },
    Float32,
}
impl PcmEncoding {
    fn layout(self) -> Result<(usize, u16), AudioCaptureError> {
        Ok(match self {
            Self::Unsigned8 => (1, 8),
            Self::UnsignedIn8 { valid_bits } if (1..=8).contains(&valid_bits) => (1, valid_bits),
            Self::Signed16 => (2, 16),
            Self::SignedIn16 { valid_bits } if (1..=16).contains(&valid_bits) => (2, valid_bits),
            Self::Signed24 => (3, 24),
            Self::Signed32 | Self::Float32 => (4, 32),
            Self::SignedIn32 { valid_bits } if (1..=32).contains(&valid_bits) => (4, valid_bits),
            Self::SignedIn24 { valid_bits } if (1..=24).contains(&valid_bits) => (3, valid_bits),
            _ => {
                return Err(AudioCaptureError::Unsupported(
                    "native PCM valid-bit layout",
                ));
            }
        })
    }
}
/// Packed little-endian native PCM. Valid bits occupy the most significant
/// container bits, as in WAVEFORMATEXTENSIBLE. Silent packets never read bytes.
pub fn decode_pcm(
    bytes: &[u8],
    encoding: PcmEncoding,
    frames: usize,
    format: NativeAudioFormat,
    limits: AudioCaptureLimits,
    silent: bool,
) -> Result<Vec<f32>, AudioCaptureError> {
    format.validate(limits)?;
    let (width, valid) = encoding.layout()?;
    if frames == 0 || frames > limits.max_frames || frames > 4096 {
        return Err(AudioCaptureError::Limit("native PCM frames"));
    }
    let count = frames
        .checked_mul(usize::from(format.channels))
        .ok_or(AudioCaptureError::Limit("native PCM samples"))?;
    let encoded = count
        .checked_mul(width)
        .ok_or(AudioCaptureError::Limit("native PCM bytes"))?;
    let decoded = count
        .checked_mul(4)
        .ok_or(AudioCaptureError::Limit("decoded PCM bytes"))?;
    if encoded > limits.max_native_buffer_bytes || decoded > limits.max_packet_bytes {
        return Err(AudioCaptureError::Limit("native PCM byte admission"));
    }
    if !silent && bytes.len() != encoded {
        return Err(AudioCaptureError::Invalid("packed PCM byte count"));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|_| AudioCaptureError::Limit("PCM allocation"))?;
    if silent {
        output.resize(count, 0.0);
        return Ok(output);
    }
    for sample in bytes.chunks_exact(width) {
        let value = match encoding {
            PcmEncoding::Unsigned8 => (f64::from(sample[0]) - 128.0) / 128.0,
            PcmEncoding::UnsignedIn8 { valid_bits } => {
                // WAVEFORMATEXTENSIBLE stores valid PCM bits left-aligned;
                // eight-bit PCM is unsigned even when the precision is lower.
                let level = 1u16 << (valid_bits - 1);
                let value = u16::from(sample[0]) >> (8 - valid_bits);
                (f64::from(value) - f64::from(level)) / f64::from(level)
            }
            PcmEncoding::Float32 => {
                let value = f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
                if !value.is_finite() {
                    return Err(AudioCaptureError::Invalid("non-finite native float PCM"));
                }
                f64::from(value)
            }
            _ => {
                let signed = match width {
                    2 => i32::from(i16::from_le_bytes([sample[0], sample[1]])),
                    3 => i32::from_le_bytes([
                        sample[0],
                        sample[1],
                        sample[2],
                        if sample[2] & 0x80 != 0 { 255 } else { 0 },
                    ]),
                    4 => i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]),
                    _ => return Err(AudioCaptureError::Unsupported("PCM container")),
                };
                let shift = (width as u16 * 8) - valid;
                f64::from(signed >> shift) / ((1u64 << (valid - 1)) as f64)
            }
        };
        output.push(value as f32);
    }
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn format() -> NativeAudioFormat {
        NativeAudioFormat {
            sample_rate: 48000,
            channels: 1,
            channel_mask: 4,
        }
    }
    #[test]
    fn integer_formats_have_signed_and_left_aligned_literal_oracles() {
        let limits = AudioCaptureLimits::default();
        for (encoding, bytes) in [
            (PcmEncoding::Unsigned8, vec![0, 128, 192]),
            (PcmEncoding::Signed16, vec![0, 128, 0, 0, 0, 64]),
            (PcmEncoding::Signed24, vec![0, 0, 128, 0, 0, 0, 0, 0, 64]),
            (
                PcmEncoding::Signed32,
                vec![0, 0, 0, 128, 0, 0, 0, 0, 0, 0, 0, 64],
            ),
            (
                PcmEncoding::SignedIn32 { valid_bits: 24 },
                vec![0, 0, 0, 128, 0, 0, 0, 0, 0, 0, 0, 64],
            ),
            (
                PcmEncoding::SignedIn24 { valid_bits: 20 },
                vec![0, 0, 128, 0, 0, 0, 0, 0, 64],
            ),
        ] {
            assert_eq!(
                decode_pcm(&bytes, encoding, 3, format(), limits, false).unwrap(),
                [-1.0, 0.0, 0.5]
            );
        }
    }
    #[test]
    fn silence_ignores_unreadable_native_data_and_float_errors_are_strict() {
        let limits = AudioCaptureLimits::default();
        assert_eq!(
            decode_pcm(&[], PcmEncoding::Float32, 2, format(), limits, true).unwrap(),
            [0.0; 2]
        );
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(
                decode_pcm(
                    &value.to_le_bytes(),
                    PcmEncoding::Float32,
                    1,
                    format(),
                    limits,
                    false
                )
                .is_err()
            );
        }
        assert_eq!(
            decode_pcm(
                &1.5f32.to_le_bytes(),
                PcmEncoding::Float32,
                1,
                format(),
                limits,
                false
            )
            .unwrap(),
            [1.5]
        );
        assert!(decode_pcm(&[0; 3], PcmEncoding::Signed16, 1, format(), limits, false).is_err());
        assert!(
            decode_pcm(
                &[],
                PcmEncoding::SignedIn32 { valid_bits: 0 },
                1,
                format(),
                limits,
                true
            )
            .is_err()
        );
        assert!(decode_pcm(&[], PcmEncoding::Float32, 4097, format(), limits, true).is_err());
        assert!(
            decode_pcm(
                &[],
                PcmEncoding::Float32,
                2,
                format(),
                AudioCaptureLimits {
                    max_packet_bytes: 4,
                    ..limits
                },
                true
            )
            .is_err()
        );
    }
}
