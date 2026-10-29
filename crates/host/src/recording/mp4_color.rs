//! Annotate our native SDR output with ISO nclx metadata. Some native encoders
//! ignore IMFMediaType color attributes. Only a bounded, trailing moov is
//! rewritten; encoded media and all absolute sample/chunk offsets stay intact.
use super::RecordingError;
use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
const MAX_MOOV: usize = 8 * 1024 * 1024;
const NCLX: [u8; 19] = [
    0, 0, 0, 19, b'c', b'o', b'l', b'r', b'n', b'c', b'l', b'x', 0, 1, 0, 1, 0, 1, 0,
];

pub(super) fn annotate(path: &Path) -> Result<(), RecordingError> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| RecordingError::io("open MP4 color metadata", path, e))?;
    let length = file
        .metadata()
        .map_err(|e| RecordingError::io("read MP4 color metadata length", path, e))?
        .len();
    let (mut at, mut boxes, mut found) = (0u64, 0usize, None);
    while at < length {
        boxes += 1;
        if boxes > 65536 || length - at < 8 {
            return Err(RecordingError::invalid(
                "native MP4 top-level box admission",
            ));
        }
        let mut header = [0u8; 8];
        file.read_exact(&mut header)
            .map_err(|e| RecordingError::io("read native MP4 box", path, e))?;
        let mut size = u64::from(u32::from_be_bytes(header[..4].try_into().unwrap()));
        let mut head = 8;
        if size == 1 {
            let mut extended = [0; 8];
            file.read_exact(&mut extended)
                .map_err(|e| RecordingError::io("read native MP4 extended box", path, e))?;
            size = u64::from_be_bytes(extended);
            head = 16;
        } else if size == 0 {
            size = length - at;
        }
        if size < head || size > length - at {
            return Err(RecordingError::invalid("native MP4 box extent"));
        }
        if &header[4..] == b"moov" {
            if found.is_some() || at + size != length || head != 8 || size > MAX_MOOV as u64 {
                return Err(RecordingError::invalid(
                    "native recording requires one bounded trailing moov",
                ));
            }
            found = Some((at, size as usize));
        }
        at += size;
        file.seek(SeekFrom::Start(at))
            .map_err(|e| RecordingError::io("seek native MP4 box", path, e))?;
    }
    let (start, size) = found.ok_or_else(|| RecordingError::invalid("native MP4 moov missing"))?;
    file.seek(SeekFrom::Start(start))
        .map_err(|e| RecordingError::io("seek MP4 color metadata", path, e))?;
    let mut original = vec![0; size];
    file.read_exact(&mut original)
        .map_err(|e| RecordingError::io("read bounded MP4 color metadata", path, e))?;
    let mut entries = 0;
    let updated = rewrite(&original, 0, &mut entries)?;
    if entries != 1 {
        return Err(RecordingError::invalid(
            "native MP4 must have exactly one H264 video sample entry",
        ));
    }
    file.seek(SeekFrom::Start(start))
        .map_err(|e| RecordingError::io("seek rewritten MP4 metadata", path, e))?;
    file.write_all(&updated)
        .map_err(|e| RecordingError::io("write MP4 color metadata", path, e))?;
    file.set_len(start + updated.len() as u64)
        .map_err(|e| RecordingError::io("resize MP4 metadata", path, e))?;
    Ok(())
}
fn rewrite(bytes: &[u8], depth: u8, entries: &mut usize) -> Result<Vec<u8>, RecordingError> {
    if depth > 8 || bytes.len() > MAX_MOOV {
        return Err(RecordingError::invalid(
            "native MP4 metadata tree admission",
        ));
    }
    let mut output = Vec::with_capacity(bytes.len() + 19);
    let mut at = 0;
    while at < bytes.len() {
        if bytes.len() - at < 8 {
            return Err(RecordingError::invalid("truncated native MP4 metadata box"));
        }
        let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        if size < 8 || size > bytes.len() - at {
            return Err(RecordingError::invalid("native MP4 metadata box extent"));
        }
        let kind = &bytes[at + 4..at + 8];
        let payload = &bytes[at + 8..at + size];
        let replacement = match kind {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                Some(rewrite(payload, depth + 1, entries)?)
            }
            b"stsd" => {
                if payload.len() < 8 || payload[..4] != [0; 4] {
                    return Err(RecordingError::invalid(
                        "native MP4 sample description header",
                    ));
                }
                let count = u32::from_be_bytes(payload[4..8].try_into().unwrap());
                if count != 1 {
                    return Err(RecordingError::invalid(
                        "native MP4 sample description count",
                    ));
                }
                let mut value = payload[..8].to_vec();
                value.extend(rewrite(&payload[8..], depth + 1, entries)?);
                Some(value)
            }
            b"avc1" => {
                *entries += 1;
                if *entries > 1 || payload.len() < 78 {
                    return Err(RecordingError::invalid(
                        "native MP4 H264 sample entry admission",
                    ));
                }
                let mut value = payload[..78].to_vec();
                let (mut child, mut colors) = (78, 0);
                while child < payload.len() {
                    if payload.len() - child < 8 {
                        return Err(RecordingError::invalid("native H264 child metadata header"));
                    }
                    let size =
                        u32::from_be_bytes(payload[child..child + 4].try_into().unwrap()) as usize;
                    if size < 8 || size > payload.len() - child {
                        return Err(RecordingError::invalid("native H264 child metadata extent"));
                    }
                    if &payload[child + 4..child + 8] == b"colr" {
                        colors += 1;
                        if colors > 1 {
                            return Err(RecordingError::invalid(
                                "duplicate native H264 color descriptions",
                            ));
                        }
                    } else {
                        value.extend_from_slice(&payload[child..child + size]);
                    }
                    child += size;
                }
                value.extend_from_slice(&NCLX);
                Some(value)
            }
            _ => None,
        };
        if let Some(payload) = replacement {
            let size = u32::try_from(payload.len() + 8)
                .map_err(|_| RecordingError::invalid("native MP4 metadata size overflow"))?;
            output.extend_from_slice(&size.to_be_bytes());
            output.extend_from_slice(kind);
            output.extend(payload);
        } else {
            output.extend_from_slice(&bytes[at..at + size]);
        }
        if output.len() > MAX_MOOV {
            return Err(RecordingError::invalid(
                "rewritten native MP4 metadata limit",
            ));
        }
        at += size;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut value = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        value.extend(kind);
        value.extend(payload);
        value
    }
    fn tree(color: Option<&[u8]>) -> Vec<u8> {
        let mut visual = vec![0; 78];
        visual.extend(atom(b"avcC", b"untouched encoded config"));
        if let Some(color) = color {
            visual.extend(atom(b"colr", color));
        }
        let mut entry = vec![0, 0, 0, 0, 0, 0, 0, 1];
        entry.extend(atom(b"avc1", &visual));
        let mut value = atom(b"stsd", &entry);
        for kind in [b"stbl", b"minf", b"mdia", b"trak", b"moov"] {
            value = atom(kind, &value);
        }
        value
    }
    #[test]
    fn explicit_iso_color_numbers_and_unrelated_codec_bytes_are_preserved() {
        let original = tree(None);
        let mut count = 0;
        let result = rewrite(&original, 0, &mut count).unwrap();
        assert_eq!(count, 1);
        assert_eq!(result.len(), original.len() + 19);
        assert!(result.windows(19).any(|value| value == NCLX));
        assert!(
            result
                .windows(24)
                .any(|value| value == b"untouched encoded config")
        );
        assert_eq!(
            u32::from_be_bytes(result[..4].try_into().unwrap()) as usize,
            result.len()
        );
    }
    #[test]
    fn existing_native_color_metadata_is_replaced_idempotently() {
        let mut count = 0;
        let result = rewrite(&tree(Some(b"nclc\0\x06\0\x06\0\x06")), 0, &mut count).unwrap();
        let mut again = 0;
        assert_eq!(rewrite(&result, 0, &mut again).unwrap(), result);
        assert_eq!(
            result.windows(4).filter(|value| *value == b"colr").count(),
            1
        );
    }
    #[test]
    fn every_truncation_and_hostile_size_is_rejected_without_unbounded_allocation() {
        let input = tree(None);
        for length in 1..input.len() {
            assert!(
                rewrite(&input[..length], 0, &mut 0).is_err(),
                "truncation{length}"
            );
        }
        for size in [0, 1, 7, u32::MAX] {
            let mut bad = input.clone();
            bad[..4].copy_from_slice(&size.to_be_bytes());
            assert!(rewrite(&bad, 0, &mut 0).is_err());
        }
    }
    #[test]
    fn file_annotation_preserves_media_offsets_and_refuses_nontrailing_moov_before_mutation() {
        let path = std::env::temp_dir().join(format!(
            "skylive-color-{}-{}.mp4",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let prefix = atom(
            b"mdat",
            b"literal compressed payload whose offset must not change",
        );
        let mut bytes = prefix.clone();
        bytes.extend(tree(None));
        file.write_all(&bytes).unwrap();
        drop(file);
        annotate(&path).unwrap();
        let updated = std::fs::read(&path).unwrap();
        assert_eq!(&updated[..prefix.len()], prefix.as_slice());
        assert_eq!(updated.len(), bytes.len() + 19);
        let mut invalid = tree(None);
        invalid.extend(&prefix);
        std::fs::write(&path, &invalid).unwrap();
        assert!(annotate(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
        std::fs::remove_file(path).unwrap();
    }
}
