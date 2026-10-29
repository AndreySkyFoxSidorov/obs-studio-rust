//! Pure bounded SPS VUI signaling for the native encoder's authored NV12 contract.
#[cfg(test)]
mod tests;
use skylive_codec_bits::{BitError, BitReader};
use std::fmt;
pub const MAX_SPS_BYTES: usize = 8192;
pub const MAX_CONFIG_BYTES: usize = 32 * 1024;
pub const MAX_SAMPLE_BYTES: usize = 2 * 1024 * 1024;
const MAX_NALS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoSignal {
    pub format: u8,
    pub full_range: bool,
    pub color: Option<[u8; 3]>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataOrigin {
    InputAlreadyMatchesContract,
    RewrittenFromInputContract,
    NoSpsInSample,
}
#[derive(Debug)]
pub struct Rewrite {
    pub bytes: Vec<u8>,
    pub changed_sps: usize,
    pub origin: MetadataOrigin,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Unsupported(&'static str),
    Limit(&'static str),
    Bits(BitError),
}
impl From<BitError> for Error {
    fn from(error: BitError) -> Self {
        Self::Bits(error)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(s) | Self::Unsupported(s) | Self::Limit(s) => f.write_str(s),
            Self::Bits(error) => fmt::Display::fmt(error, f),
        }
    }
}
impl std::error::Error for Error {}
struct Layout {
    replace_start: usize,
    replace_end: usize,
    syntax_end: usize,
    vui_present: bool,
    signal: Option<VideoSignal>,
}
struct Reader<'a>(BitReader<'a>);
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self(BitReader::new(bytes))
    }
    fn position(&self) -> usize {
        self.0.position()
    }
    fn bit(&mut self) -> Result<bool, Error> {
        Ok(self.0.read_bit()?)
    }
    fn bits(&mut self, count: u32) -> Result<u32, Error> {
        Ok(self.0.read_bits(count)?)
    }
    fn ue(&mut self, max: u32) -> Result<u32, Error> {
        let value = self.0.read_ue()?;
        if value > max {
            return Err(Error::Invalid("SPS Exp-Golomb field exceeds its bound"));
        }
        Ok(value)
    }
    fn se(&mut self) -> Result<i32, Error> {
        Ok(self.0.read_se()?)
    }
}
fn parse(rbsp: &[u8]) -> Result<Layout, Error> {
    let mut r = Reader::new(rbsp);
    let profile = r.bits(8)?;
    if !matches!(profile, 66 | 77 | 88 | 100) {
        return Err(Error::Unsupported(
            "SPS profile outside baseline/main/extended/high8bit420",
        ));
    }
    let constraints = r.bits(8)?;
    if constraints & 3 != 0 {
        return Err(Error::Invalid("SPS reserved constraint bits"));
    }
    r.bits(8)?;
    r.ue(31)?;
    if profile == 100 {
        if r.ue(3)? != 1 || r.ue(6)? != 0 || r.ue(6)? != 0 {
            return Err(Error::Unsupported("SPS is not8bit420"));
        }
        r.bit()?;
        if r.bit()? {
            for index in 0..8 {
                if r.bit()? {
                    scaling_list(&mut r, if index < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    r.ue(12)?;
    match r.ue(2)? {
        0 => {
            r.ue(12)?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            let count = r.ue(255)?;
            for _ in 0..count {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue(16)?;
    r.bit()?;
    let width = r.ue(255)? + 1;
    let height = r.ue(255)? + 1;
    let frame_only = r.bit()?;
    if !frame_only {
        r.bit()?;
    }
    r.bit()?;
    if r.bit()? {
        let left = r.ue(4096)?;
        let right = r.ue(4096)?;
        let top = r.ue(4096)?;
        let bottom = r.ue(4096)?;
        let horizontal = left
            .checked_add(right)
            .and_then(|n| n.checked_mul(2))
            .ok_or(Error::Invalid("SPS crop overflow"))?;
        let vertical = top
            .checked_add(bottom)
            .and_then(|n| n.checked_mul(if frame_only { 2 } else { 4 }))
            .ok_or(Error::Invalid("SPS crop overflow"))?;
        if horizontal >= width * 16 || vertical >= height * 16 * if frame_only { 1 } else { 2 } {
            return Err(Error::Invalid("SPS crop exceeds coded picture"));
        }
    }
    let vui_flag = r.position();
    let vui_present = r.bit()?;
    let (replace_start, replace_end, signal) = if vui_present {
        parse_vui(&mut r)?
    } else {
        (vui_flag, r.position(), None)
    };
    let syntax_end = r.position();
    if !r.bit()? {
        return Err(Error::Invalid("missing rbsp_stop_one_bit"));
    }
    while r.0.bits_left() > 0 {
        if r.0.bits_left() >= 8 || r.bit()? {
            return Err(Error::Invalid(
                "invalid SPS trailing bits or extra trailing bytes",
            ));
        }
    }
    Ok(Layout {
        replace_start,
        replace_end,
        syntax_end,
        vui_present,
        signal,
    })
}
fn scaling_list(r: &mut Reader<'_>, count: usize) -> Result<(), Error> {
    let mut next = 8i32;
    let mut last = 8i32;
    for _ in 0..count {
        if next != 0 {
            let delta = r.se()?;
            if !(-128..=127).contains(&delta) {
                return Err(Error::Invalid("scaling-list delta"));
            }
            next = (last + delta + 256) % 256;
        }
        if next != 0 {
            last = next;
        }
    }
    Ok(())
}
fn hrd(r: &mut Reader<'_>) -> Result<(), Error> {
    let count = r.ue(31)? + 1;
    r.bits(4)?;
    r.bits(4)?;
    for _ in 0..count {
        r.ue(u32::MAX)?;
        r.ue(u32::MAX)?;
        r.bit()?;
    }
    for _ in 0..4 {
        r.bits(5)?;
    }
    Ok(())
}
fn parse_vui(r: &mut Reader<'_>) -> Result<(usize, usize, Option<VideoSignal>), Error> {
    if r.bit()? && r.bits(8)? == 255 && (r.bits(16)? == 0 || r.bits(16)? == 0) {
        return Err(Error::Invalid("zero extended SAR"));
    }
    if r.bit()? {
        r.bit()?;
    }
    let start = r.position();
    let signal = if r.bit()? {
        let format = r.bits(3)? as u8;
        if format > 5 {
            return Err(Error::Unsupported("reserved video_format"));
        }
        let full_range = r.bit()?;
        let color = if r.bit()? {
            Some([r.bits(8)? as u8, r.bits(8)? as u8, r.bits(8)? as u8])
        } else {
            None
        };
        Some(VideoSignal {
            format,
            full_range,
            color,
        })
    } else {
        None
    };
    let end = r.position();
    if r.bit()? {
        r.ue(5)?;
        r.ue(5)?;
    }
    if r.bit()? {
        if r.bits(32)? == 0 || r.bits(32)? == 0 {
            return Err(Error::Invalid("zero VUI timing scale"));
        }
        r.bit()?;
    }
    let nal_hrd = r.bit()?;
    if nal_hrd {
        hrd(r)?;
    }
    let vcl_hrd = r.bit()?;
    if vcl_hrd {
        hrd(r)?;
    }
    if nal_hrd || vcl_hrd {
        r.bit()?;
    }
    r.bit()?;
    if r.bit()? {
        r.bit()?;
        r.ue(16)?;
        r.ue(16)?;
        r.ue(16)?;
        r.ue(16)?;
        let reorder = r.ue(16)?;
        let buffer = r.ue(16)?;
        if reorder > buffer {
            return Err(Error::Invalid("VUI reorder count exceeds buffering"));
        }
    }
    Ok((start, end, signal))
}
fn unescape(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut zeros = 0;
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if zeros >= 2 {
            if byte == 3 {
                let next = *bytes.get(at + 1).ok_or(Error::Invalid("terminal EPB"))?;
                if next > 3 {
                    return Err(Error::Invalid("invalid EPB following byte"));
                }
                zeros = 0;
                at += 1;
                continue;
            }
            if byte < 3 {
                return Err(Error::Invalid("unescaped start-code emulation"));
            }
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        at += 1;
    }
    Ok(out)
}
fn escape_size(bytes: &[u8]) -> Result<usize, Error> {
    let mut length = 0usize;
    let mut zeros = 0;
    for byte in bytes {
        if zeros >= 2 && *byte <= 3 {
            length = length.checked_add(1).ok_or(Error::Limit("EBSP size"))?;
            zeros = 0;
        }
        length = length.checked_add(1).ok_or(Error::Limit("EBSP size"))?;
        zeros = if *byte == 0 { zeros + 1 } else { 0 };
    }
    Ok(length)
}
fn escape(bytes: &[u8], length: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(length);
    let mut zeros = 0;
    for byte in bytes {
        if zeros >= 2 && *byte <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(*byte);
        zeros = if *byte == 0 { zeros + 1 } else { 0 };
    }
    out
}
struct Builder {
    bytes: Vec<u8>,
    bits: usize,
    max: usize,
}
impl Builder {
    fn new(max: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(max),
            bits: 0,
            max,
        }
    }
    fn bit(&mut self, value: bool) -> Result<(), Error> {
        if self.bits >= self.max * 8 {
            return Err(Error::Limit("rewritten SPS bits"));
        }
        if self.bits % 8 == 0 {
            self.bytes.push(0);
        }
        if value {
            let last = self.bytes.len() - 1;
            self.bytes[last] |= 1 << (7 - self.bits % 8);
        }
        self.bits += 1;
        Ok(())
    }
    fn value(&mut self, value: u32, count: u32) -> Result<(), Error> {
        for bit in (0..count).rev() {
            self.bit(value & (1 << bit) != 0)?;
        }
        Ok(())
    }
    fn copy(&mut self, bytes: &[u8], start: usize, end: usize) -> Result<(), Error> {
        if start > end || end > bytes.len() * 8 {
            return Err(Error::Invalid("SPS bit-copy span"));
        }
        for bit in start..end {
            self.bit(bytes[bit / 8] & (1 << (7 - bit % 8)) != 0)?;
        }
        Ok(())
    }
    fn signal(&mut self, format: u8) -> Result<(), Error> {
        self.bit(true)?;
        self.value(u32::from(format), 3)?;
        self.bit(false)?;
        self.bit(true)?;
        for _ in 0..3 {
            self.value(1, 8)?;
        }
        Ok(())
    }
}

pub fn ensure_bt709_limited_sps(nal: &[u8]) -> Result<Rewrite, Error> {
    if !(2..=MAX_SPS_BYTES).contains(&nal.len()) {
        return Err(Error::Limit("SPS bytes"));
    }
    if nal[0] & 0x80 != 0 || nal[0] & 31 != 7 || nal[0] & 0x60 == 0 {
        return Err(Error::Invalid("SPS NAL header"));
    }
    let rbsp = unescape(&nal[1..])?;
    let layout = parse(&rbsp)?;
    if layout
        .signal
        .is_some_and(|s| !s.full_range && s.color == Some([1, 1, 1]))
    {
        return Ok(Rewrite {
            bytes: nal.to_vec(),
            changed_sps: 0,
            origin: MetadataOrigin::InputAlreadyMatchesContract,
        });
    }
    let new_bits = if layout.vui_present { 30 } else { 39 };
    let total = layout
        .syntax_end
        .checked_sub(layout.replace_end - layout.replace_start)
        .and_then(|n| n.checked_add(new_bits + 1))
        .ok_or(Error::Limit("SPS bit expansion"))?;
    let size = total.div_ceil(8);
    if size + 1 > MAX_SPS_BYTES {
        return Err(Error::Limit("SPS rewrite expansion"));
    }
    let mut out = Builder::new(size);
    out.copy(&rbsp, 0, layout.replace_start)?;
    if !layout.vui_present {
        out.bit(true)?;
        out.bit(false)?;
        out.bit(false)?;
    }
    out.signal(layout.signal.map_or(5, |s| s.format))?;
    if layout.vui_present {
        out.copy(&rbsp, layout.replace_end, layout.syntax_end)?;
    } else {
        for _ in 0..6 {
            out.bit(false)?;
        }
    }
    out.bit(true)?;
    while out.bits % 8 != 0 {
        out.bit(false)?;
    }
    let escaped = escape_size(&out.bytes)?;
    if escaped + 1 > MAX_SPS_BYTES {
        return Err(Error::Limit("SPS EBSP expansion"));
    }
    let mut result = Vec::with_capacity(escaped + 1);
    result.push(nal[0]);
    result.extend_from_slice(&escape(&out.bytes, escaped));
    // Parse the candidate before publishing; original input stays untouched.
    let candidate = unescape(&result[1..])?;
    let checked = parse(&candidate)?;
    if checked
        .signal
        .is_none_or(|s| s.full_range || s.color != Some([1, 1, 1]))
    {
        return Err(Error::Invalid("rewritten VUI validation"));
    }
    Ok(Rewrite {
        bytes: result,
        changed_sps: 1,
        origin: MetadataOrigin::RewrittenFromInputContract,
    })
}

fn append(out: &mut Vec<u8>, bytes: &[u8], limit: usize) -> Result<(), Error> {
    if out.len().checked_add(bytes.len()).is_none_or(|n| n > limit) {
        return Err(Error::Limit("aggregate rewritten output"));
    }
    out.extend_from_slice(bytes);
    Ok(())
}
fn nal_at<'a>(bytes: &'a [u8], offset: &mut usize, length_width: usize) -> Result<&'a [u8], Error> {
    let end = offset
        .checked_add(length_width)
        .ok_or(Error::Invalid("NAL length offset overflow"))?;
    let field = bytes
        .get(*offset..end)
        .ok_or(Error::Invalid("truncated NAL length"))?;
    let mut length = 0usize;
    for byte in field {
        length = (length << 8) | usize::from(*byte);
    }
    let finish = end
        .checked_add(length)
        .ok_or(Error::Invalid("NAL payload offset overflow"))?;
    let nal = bytes
        .get(end..finish)
        .ok_or(Error::Invalid("truncated NAL payload"))?;
    if nal.is_empty() || nal[0] & 0x80 != 0 || !(1..=23).contains(&(nal[0] & 31)) {
        return Err(Error::Invalid("AVC NAL header"));
    }
    *offset = finish;
    Ok(nal)
}
fn assemble(
    limit: usize,
    mut walk: impl FnMut(&mut dyn FnMut(&[u8]) -> Result<(), Error>) -> Result<(usize, usize), Error>,
) -> Result<Rewrite, Error> {
    let mut size = 0usize;
    let (changed, sps_count) = {
        let mut count = |part: &[u8]| {
            size = size
                .checked_add(part.len())
                .filter(|size| *size <= limit)
                .ok_or(Error::Limit("aggregate rewritten output"))?;
            Ok(())
        };
        walk(&mut count)?
    };
    // Admission and all framing validation precede this exact allocation.
    // A second bounded walk avoids retaining every rewritten SPS or geometric
    // Vec growth when a large sample contains many small NAL units.
    let mut out = Vec::with_capacity(size);
    let repeated = {
        let mut emit = |part: &[u8]| append(&mut out, part, size);
        walk(&mut emit)?
    };
    if repeated != (changed, sps_count) || out.len() != size {
        return Err(Error::Invalid("nondeterministic SPS rewrite"));
    }
    Ok(Rewrite {
        bytes: out,
        changed_sps: changed,
        origin: if changed > 0 {
            MetadataOrigin::RewrittenFromInputContract
        } else if sps_count == 0 {
            MetadataOrigin::NoSpsInSample
        } else {
            MetadataOrigin::InputAlreadyMatchesContract
        },
    })
}
pub fn rewrite_baseline_avcc(bytes: &[u8]) -> Result<Rewrite, Error> {
    if bytes.get(1)!=Some(&66){return Err(Error::Unsupported("Baseline avcC required"));}
    rewrite_sdr_avcc(bytes)
}
pub fn rewrite_sdr_avcc(bytes: &[u8]) -> Result<Rewrite, Error> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(Error::Limit("avcC bytes"));
    }
    if bytes.len() < 7
        || bytes[0] != 1
        || !matches!(bytes[1],66|100)
        || bytes[4] != 0xff
        || bytes[5] & 0xe0 != 0xe0
    {
        return Err(Error::Unsupported(
            "Baseline/High8bit420 avcC with four-byte sample lengths required",
        ));
    }
    assemble(MAX_CONFIG_BYTES, |emit| {
        let sps_count = bytes[5] & 31;
        if sps_count == 0 {
            return Err(Error::Invalid("missing avcC SPS"));
        }
        emit(&bytes[..6])?;
        let mut at = 6;
        let mut changed = 0;
        for _ in 0..sps_count {
            let nal = nal_at(bytes, &mut at, 2)?;
            if nal[0] & 31 != 7 {
                return Err(Error::Invalid("wrong avcC SPS type"));
            }
            if nal.len() < 4 || nal[1..4] != bytes[1..4] {
                return Err(Error::Unsupported(
                    "avcC profile/constraints/level differ from SPS",
                ));
            }
            let rewrite = ensure_bt709_limited_sps(nal)?;
            changed += rewrite.changed_sps;
            emit(&(rewrite.bytes.len() as u16).to_be_bytes())?;
            emit(&rewrite.bytes)?;
        }
        let pps_count = *bytes
            .get(at)
            .ok_or(Error::Invalid("missing avcC PPS count"))?;
        at += 1;
        if pps_count == 0 || usize::from(pps_count) + usize::from(sps_count) > MAX_NALS {
            return Err(Error::Limit("avcC parameter count"));
        }
        emit(&[pps_count])?;
        for _ in 0..pps_count {
            let nal = nal_at(bytes, &mut at, 2)?;
            if nal[0] & 31 != 8 {
                return Err(Error::Invalid("wrong avcC PPS type"));
            }
            emit(&(nal.len() as u16).to_be_bytes())?;
            emit(nal)?;
        }
        if bytes[1]==100 {
            if bytes.get(at..)!=Some(&[0xfd,0xf8,0xf8,0][..]){return Err(Error::Unsupported("High avcC must be8bit420 without SPS extensions"));}
            emit(&bytes[at..])?;at=bytes.len();
        }
        if at != bytes.len() {
            return Err(Error::Unsupported("avcC extension/trailing data"));
        }
        Ok((changed, usize::from(sps_count)))
    })
}
pub fn rewrite_length_prefixed_sample(bytes: &[u8]) -> Result<Rewrite, Error> {
    if bytes.is_empty() || bytes.len() > MAX_SAMPLE_BYTES {
        return Err(Error::Limit("AVC sample bytes"));
    }
    assemble(MAX_SAMPLE_BYTES, |emit| {
        let mut at = 0;
        let mut count = 0;
        let mut changed = 0;
        let mut sps_count = 0;
        while at < bytes.len() {
            count += 1;
            if count > MAX_NALS {
                return Err(Error::Limit("AVC sample NAL count"));
            }
            let nal = nal_at(bytes, &mut at, 4)?;
            if nal[0] & 31 == 7 {
                sps_count += 1;
                let rewrite = ensure_bt709_limited_sps(nal)?;
                changed += rewrite.changed_sps;
                emit(&(rewrite.bytes.len() as u32).to_be_bytes())?;
                emit(&rewrite.bytes)?;
            } else {
                emit(&(nal.len() as u32).to_be_bytes())?;
                emit(nal)?;
            }
        }
        Ok((changed, sps_count))
    })
}
