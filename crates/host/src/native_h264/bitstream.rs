#[cfg(any(target_os = "windows", test))]
use super::{Error, MAX_AU_BYTES, MAX_HEADER_BYTES};
#[cfg(any(target_os = "windows", test))]
const MAX_NALS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecConfiguration {
    sps: Vec<Box<[u8]>>,
    pps: Vec<Box<[u8]>>,
    avcc: Box<[u8]>,
    input_avcc: Box<[u8]>,
    color_metadata_rewritten: bool,
}
impl CodecConfiguration {
    pub fn sps(&self) -> &[Box<[u8]>] {
        &self.sps
    }
    pub fn pps(&self) -> &[Box<[u8]>] {
        &self.pps
    }
    pub fn avcc(&self) -> &[u8] {
        &self.avcc
    }
    /// Actual input SPS/PPS normalized to avcC BEFORE SkyLive color signaling.
    pub fn input_avcc(&self) -> &[u8] {
        &self.input_avcc
    }
    /// Sticky for the encoder epoch, including in-band SPS normalization.
    pub fn color_metadata_rewritten(&self) -> bool {
        self.color_metadata_rewritten
    }
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.sps.capacity() * std::mem::size_of::<Box<[u8]>>()
            + self.pps.capacity() * std::mem::size_of::<Box<[u8]>>()
            + self.avcc.len()
            + self.input_avcc.len()
            + self.sps.iter().map(|n| n.len()).sum::<usize>()
            + self.pps.iter().map(|n| n.len()).sum::<usize>()
    }
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, Error> {Self::parse_profile(bytes,super::H264Profile::Baseline)}
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn parse_profile(bytes:&[u8],profile:super::H264Profile)->Result<Self,Error>{
        if bytes.is_empty() || bytes.len() > MAX_HEADER_BYTES {
            return Err(Error::Limit("H264 sequence header"));
        }
        let mut sps = Vec::new();
        let mut pps = Vec::new();
        if bytes[0] == 1 {
            if bytes.len() < 7 || bytes[4] & 3 != 3 {
                return Err(Error::Protocol("unsupported avcC NAL length width"));
            }
            let mut offset = 6;
            let count = (bytes[5] & 31) as usize;
            for _ in 0..count {
                sps.push(take_parameter(bytes, &mut offset, 7)?);
            }
            let count = *bytes
                .get(offset)
                .ok_or(Error::Protocol("truncated avcC PPS count"))?
                as usize;
            offset += 1;
            for _ in 0..count {
                pps.push(take_parameter(bytes, &mut offset, 8)?);
            }
            if profile==super::H264Profile::High && bytes.get(offset..)==Some(&[0xfd,0xf8,0xf8,0][..]) {offset+=4;}
            if offset != bytes.len() {
                return Err(Error::Protocol("unsupported avcC trailing profile data"));
            }
        } else {
            for nal in annex_b(bytes)? {
                match nal[0] & 31 {
                    7 => sps.push(nal.into()),
                    8 => pps.push(nal.into()),
                    _ => return Err(Error::Protocol("sequence header has non-parameter NAL")),
                }
            }
        }
        if sps.is_empty()
            || pps.is_empty()
            || sps.len() > 31
            || pps.len() > 255
            || sps.len() + pps.len() > MAX_NALS
            || sps[0].len() < 4
        {
            return Err(Error::Protocol("missing or excessive SPS/PPS"));
        }
        if sps.iter().any(|nal| nal.len() < 4 || u32::from(nal[1]) != profile.idc()) {
            return Err(Error::Protocol("encoder did not produce the requested SPS profile"));
        }
        let mut input_avcc = vec![
            1,
            sps[0][1],
            sps[0][2],
            sps[0][3],
            0xff,
            0xe0 | sps.len() as u8,
        ];
        for nal in &sps {
            append_parameter(&mut input_avcc, nal)?;
        }
        input_avcc.push(pps.len() as u8);
        for nal in &pps {
            append_parameter(&mut input_avcc, nal)?;
        }
        if input_avcc.len() > MAX_HEADER_BYTES {
            return Err(Error::Limit("normalized input avcC"));
        }
        // Canonical High8bit420 extension; the SPS parser independently
        // validates chroma and both bit depths rather than trusting this tag.
        if profile==super::H264Profile::High {input_avcc.extend_from_slice(&[0xfd,0xf8,0xf8,0]);}
        let rewritten = super::color::rewrite_sdr_avcc(&input_avcc)?;
        let mut output_sps = Vec::with_capacity(sps.len());
        let mut offset = 6;
        for _ in 0..sps.len() {
            output_sps.push(take_parameter(&rewritten.bytes, &mut offset, 7)?);
        }
        let candidate = Self {
            sps: output_sps,
            pps,
            avcc: rewritten.bytes.into_boxed_slice(),
            input_avcc: input_avcc.into_boxed_slice(),
            color_metadata_rewritten: matches!(
                rewritten.origin,
                super::color::MetadataOrigin::RewrittenFromInputContract
            ),
        };
        if candidate.retained_bytes() > MAX_HEADER_BYTES {
            return Err(Error::Limit("retained H264 input/output headers"));
        }
        Ok(candidate)
    }
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn inherit_color_provenance(&mut self, previous: &Self) {
        self.color_metadata_rewritten |= previous.color_metadata_rewritten;
    }
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn rewrite_sample(&mut self, bytes: &[u8]) -> Result<Vec<u8>, Error> {
        let rewritten = super::color::rewrite_length_prefixed_sample(bytes)?;
        self.color_metadata_rewritten |= rewritten.changed_sps > 0;
        Ok(rewritten.bytes)
    }
}
#[cfg(any(target_os = "windows", test))]
fn take_parameter(bytes: &[u8], offset: &mut usize, kind: u8) -> Result<Box<[u8]>, Error> {
    let end = offset
        .checked_add(2)
        .ok_or(Error::Protocol("parameter length overflow"))?;
    let len = bytes
        .get(*offset..end)
        .ok_or(Error::Protocol("truncated parameter length"))?;
    let len = u16::from_be_bytes([len[0], len[1]]) as usize;
    *offset = end;
    let end = offset
        .checked_add(len)
        .ok_or(Error::Protocol("parameter size overflow"))?;
    let nal = bytes
        .get(*offset..end)
        .ok_or(Error::Protocol("truncated parameter"))?;
    if nal.is_empty() || nal[0] & 0x80 != 0 || nal[0] & 31 != kind {
        return Err(Error::Protocol("invalid SPS/PPS NAL"));
    }
    *offset = end;
    Ok(nal.into())
}
#[cfg(any(target_os = "windows", test))]
fn append_parameter(out: &mut Vec<u8>, nal: &[u8]) -> Result<(), Error> {
    let len = u16::try_from(nal.len()).map_err(|_| Error::Limit("parameter set size"))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(nal);
    Ok(())
}
#[cfg(any(target_os = "windows", test))]
fn start_code(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at..at + 3) == Some(&[0, 0, 1]) {
        Some(3)
    } else if bytes.get(at..at + 4) == Some(&[0, 0, 0, 1]) {
        Some(4)
    } else {
        None
    }
}
#[cfg(any(target_os = "windows", test))]
fn annex_b(bytes: &[u8]) -> Result<Vec<&[u8]>, Error> {
    let mut position = 0;
    while bytes.get(position) == Some(&0) && start_code(bytes, position).is_none() {
        position += 1;
    }
    let mut nals = Vec::new();
    while position < bytes.len() {
        let prefix =
            start_code(bytes, position).ok_or(Error::Protocol("invalid Annex B prefix"))?;
        let start = position + prefix;
        position = start;
        while position < bytes.len() && start_code(bytes, position).is_none() {
            position += 1;
        }
        let mut end = position;
        while end > start && bytes[end - 1] == 0 {
            end -= 1;
        }
        let nal = &bytes[start..end];
        if nal.is_empty() || nal[0] & 0x80 != 0 {
            return Err(Error::Protocol("empty/forbidden NAL"));
        }
        nals.push(nal);
        if nals.len() > MAX_NALS {
            return Err(Error::Limit("NAL count"));
        }
    }
    if nals.is_empty() {
        return Err(Error::Protocol("no Annex B NAL units"));
    }
    Ok(nals)
}
#[cfg(any(target_os = "windows", test))]
pub(crate) fn normalize_access_unit(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    if bytes.is_empty() || bytes.len() > MAX_AU_BYTES {
        return Err(Error::Limit("native access unit"));
    }
    // MFVideoFormat_H264 output is Annex B. Do not guess framing from an
    // ambiguous length field: the native adapter uses this explicit contract.
    let nals = annex_b(bytes)?;
    let size = nals
        .iter()
        .try_fold(0usize, |n, nal| n.checked_add(4 + nal.len()))
        .ok_or(Error::Limit("normalized access unit size"))?;
    if size > MAX_AU_BYTES {
        return Err(Error::Limit("normalized access unit"));
    }
    let mut out = Vec::with_capacity(size);
    for nal in nals {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    validate_access_unit(&out)?;
    Ok(out)
}
#[cfg(any(target_os = "windows", test))]
pub(crate) fn validate_access_unit(bytes: &[u8]) -> Result<bool, Error> {
    if bytes.is_empty() || bytes.len() > MAX_AU_BYTES {
        return Err(Error::Limit("access unit"));
    }
    let mut position = 0;
    let mut count = 0;
    let mut vcl = false;
    let mut idr = false;
    while position < bytes.len() {
        let len = bytes
            .get(position..position + 4)
            .ok_or(Error::Protocol("truncated NAL length"))?;
        let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
        position += 4;
        let end = position
            .checked_add(len)
            .ok_or(Error::Protocol("NAL size overflow"))?;
        let nal = bytes
            .get(position..end)
            .ok_or(Error::Protocol("truncated NAL payload"))?;
        if nal.is_empty() || nal[0] & 0x80 != 0 {
            return Err(Error::Protocol("invalid NAL header"));
        }
        if matches!(nal[0]&31,1|5){validate_no_b_slice(nal)?;}
        match nal[0] & 31 {
            1 => vcl = true,
            5 => {
                vcl = true;
                idr = true;
            }
            6..=9 | 12 => {}
            _ => return Err(Error::Protocol("NAL type outside progressive AVC profile")),
        }
        position = end;
        count += 1;
        if count > MAX_NALS {
            return Err(Error::Limit("NAL count"));
        }
    }
    if !vcl {
        return Err(Error::Protocol("access unit has no picture"));
    }
    Ok(idr)
}

#[cfg(any(target_os = "windows", test))]
fn validate_no_b_slice(nal:&[u8])->Result<(),Error>{
    // first_mb_in_slice and slice_type occur before PPS-dependent syntax.
    // A64-byte bounded prefix is ample for their32-bit Exp-Golomb bounds;
    // never allocate another full encoded AU to inspect these two fields.
    let mut prefix=[0u8;64];let mut n=0;let mut zeroes=0;
    for &byte in nal.iter().skip(1).take(96){
        if zeroes>=2&&byte==3{zeroes=0;continue;}
        prefix[n]=byte;n+=1;zeroes=if byte==0{zeroes+1}else{0};if n==prefix.len(){break;}
    }
    let mut bits=skylive_codec_bits::BitReader::new(&prefix[..n]);
    let first=bits.read_ue().map_err(|_|Error::Protocol("truncated AVC slice position"))?;
    let kind=bits.read_ue().map_err(|_|Error::Protocol("truncated AVC slice type"))?;
    if first>65535||kind>9{return Err(Error::Protocol("AVC slice fields exceed progressive profile"));}
    if kind%5==1{return Err(Error::Reordered);}
    Ok(())
}
