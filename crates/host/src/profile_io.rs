//! Bounded portable profile files. Native devices and scene content are excluded.
use serde::{Deserialize, Serialize};
use skylive_core::Profile;
use std::{io::Read, path::Path};
const MAX_BYTES: u64 = 256 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document { format: String, version: u32, profile: Profile }
pub fn load(path: &Path) -> Result<Profile, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path).map_err(|error| error.to_string())?.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES { return Err("Profile file exceeds 256 KiB".into()); }
    let document: Document = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if document.format != "SkyLiveProfile" || !(1..=2).contains(&document.version) { return Err("Unsupported profile file format or version".into()); }
    if document.version<2&&u64::from(document.profile.video().width())*u64::from(document.profile.video().height())>8_388_608{return Err("Expanded4K canvas dimensions require profile file version2".into());}
    if document.version<2&&document.profile.streaming().audio_track()!=skylive_core::AudioTrackId::ONE{return Err("Selected streaming audio track requires profile file version2".into());}
    if document.version<2&&!document.profile.recording().audio_tracks().is_default(){return Err("Recording track settings require profile file version2".into());}
    if document.version<2&&!document.profile.streaming().reconnect().is_default(){return Err("Stream reconnect settings require profile file version2".into());}
    if document.version<2&&!document.profile.output_video_settings().is_default(){return Err("Output video settings require profile file version2".into());}
    if document.version<2&&!document.profile.recording().container().is_default(){return Err("Matroska recording requires profile file version2".into());}
    if document.version<2&&!document.profile.replay_buffer().is_default(){return Err("Replay buffer settings require profile file version2".into());}
    if document.version<2&&document.profile.streaming().endpoint().map_err(|error|error.to_string())?.is_some_and(|endpoint|endpoint.is_secure()){return Err("RTMPS streaming requires profile file version2".into());}
    if document.version<2&&(!document.profile.recording().encoder().is_default()||document.profile.recording().keyframe_interval_seconds()!=2||!document.profile.streaming().encoder().is_default()){return Err("Explicit video encoder settings require profile file version2".into());}
    Ok(document.profile)
}
pub fn save(path: &Path, profile: Profile) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&Document { format: "SkyLiveProfile".into(), version: 2, profile }).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES { return Err("Profile file exceeds 256 KiB".into()); }
    crate::storage::publish_bytes(path, &bytes).map_err(|error| error.to_string())
}
