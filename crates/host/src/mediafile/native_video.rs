//! Windows system software decoder boundary. Construction is explicit worker work.
use super::*;
#[cfg(target_os="windows")]
#[path="native_video/config.rs"]
mod config;
#[cfg(target_os="windows")]
#[path="native_video/windows.rs"]
mod windows;
#[cfg(target_os="windows")]
pub(super) use windows::Decoder;

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize)]
#[serde(rename_all="snake_case")]
pub enum VideoDecoderBackend {Rust,WindowsMfSoftware}
impl VideoDecoderBackend {
    pub fn label(self)->&'static str {match self {Self::Rust=>"Rust H.264 decoder",Self::WindowsMfSoftware=>"Windows Media Foundation (software)"}}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize)]
pub struct VideoDecoderInfo {pub backend:VideoDecoderBackend,pub profile:u8,pub level:u8,pub reordered_timestamps:bool}
pub enum FileVideoFrame {
    Rust(RustPicture),
    #[cfg(target_os="windows")]
    Native(NativePicture),
}
impl FileVideoFrame {
    pub fn width(&self)->u32 {match self {Self::Rust(p)=>p.width,#[cfg(target_os="windows")] Self::Native(p)=>p.width}}
    pub fn height(&self)->u32 {match self {Self::Rust(p)=>p.height,#[cfg(target_os="windows")] Self::Native(p)=>p.height}}
    pub fn presentation_ns(&self)->i64 {match self {Self::Rust(p)=>p.pts,#[cfg(target_os="windows")] Self::Native(p)=>p.pts}}
}
#[cfg(target_os="windows")]
pub struct NativePicture {pub(super) width:u32,pub(super) height:u32,pub(super) pts:i64,pub(super) y:Vec<u8>,pub(super) u:Vec<u8>,pub(super) v:Vec<u8>,pub(super) chroma_left:bool,_credit:PlaneCredit}
#[cfg(target_os="windows")]
impl NativePicture {
    pub(super) fn new(width:u32,height:u32,pts:i64,chroma_left:bool)->Result<Self,MediaFileError>{
        let area=width as usize*height as usize;let credit=PlaneCredit::new(area+area/2)?;
        Ok(Self {width,height,pts,y:vec![0;area],u:vec![0;area/4],v:vec![0;area/4],chroma_left,_credit:credit})
    }
    pub(super) fn byte_len(&self)->usize{self.y.len()+self.u.len()+self.v.len()}
}
#[cfg(target_os="windows")]
static PLANES:std::sync::atomic::AtomicUsize=std::sync::atomic::AtomicUsize::new(0);
#[cfg(target_os="windows")]
struct PlaneCredit(usize);
#[cfg(target_os="windows")]
impl PlaneCredit {fn new(bytes:usize)->Result<Self,MediaFileError>{use std::sync::atomic::Ordering;PLANES.fetch_update(Ordering::AcqRel,Ordering::Acquire,|v|v.checked_add(bytes).filter(|n|*n<=128*1024*1024)).map_err(|_|MediaFileError::Limit("native decoded plane pool128MiB includes pending/retiring frames"))?;Ok(Self(bytes))}}
#[cfg(target_os="windows")]
impl Drop for PlaneCredit {fn drop(&mut self){PLANES.fetch_sub(self.0,std::sync::atomic::Ordering::AcqRel);}}


/// Static compiled scope, never a probe or a native activation.
pub fn decoder_capabilities()->serde_json::Value {serde_json::json!({
    "container":"self_contained_mp4_mov","rust_h264_preserved":true,
    "windows_mf_software_alternative":cfg!(target_os="windows"),"windows_mf_profiles":["baseline","main","high"],
    "windows_mf_output":"system_memory_nv12_8bit420_progressive_sdr_bt709_limited",
    "windows_mf_multislice_access_units":cfg!(target_os="windows"),"native_activation":"explicit_media_prepare_or_source_playback_on_owned_worker",
    "hardware_decoding":false,"native_verified":false,"external_process":false,
    "max_native_owners_including_retiring":10,"max_native_pending_inputs":32,"max_native_batch_frames":4,"native_plane_pool_bytes":128*1024*1024
})}
