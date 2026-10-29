//! Exact OBS32 has six executable mixes but saves a uint32 source mask.
//! Normalize inactive high bits at interchange; Core remains strictly six-bit.
use super::*;
use skylive_core::SourceAudioTrackMask;

pub(super) fn parse(map:&Map<String,Value>,path:&str)->Result<SourceAudioTrackMask,ObsError>{
    let bits=match map.get("mixers") {
        None=>u64::from(SourceAudioTrackMask::ALL.bits()),
        Some(value)=>value.as_u64().filter(|bits|*bits<=u64::from(u32::MAX))
            .ok_or_else(||issue(&format!("{path}.mixers"),"invalid","OBS source mixers must be an unsigned 32-bit mask."))?,
    };
    SourceAudioTrackMask::new((bits&u64::from(SourceAudioTrackMask::ALL.bits()))as u8)
        .map_err(|error|issue(&format!("{path}.mixers"),"invalid",error.to_string()))
}

pub(super) fn neutral(map:&Map<String,Value>,path:&str)->Result<(),ObsError>{
    if !parse(map,path)?.is_all(){
        return Err(issue(&format!("{path}.mixers"),"unsupported_filter","Filter audio routing must enable all six OBS mixes."));
    }
    Ok(())
}
