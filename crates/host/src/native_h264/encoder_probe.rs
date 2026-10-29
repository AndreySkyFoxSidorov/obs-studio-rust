//! Explicit host operation. Merely displaying capabilities never loads NVENC.
use super::Error;
#[derive(Clone,Debug,serde::Serialize)]
pub struct NvencAdapter {
    pub luid:u64,pub name:String,pub available:bool,pub reason:Option<String>,
    pub min_width:u32,pub min_height:u32,pub max_width:u32,pub max_height:u32,
    pub rate_control_mask:u32,pub baseline:bool,pub nv12:bool,
}
#[derive(Clone,Debug,serde::Serialize)]
pub struct NvencCatalog {pub adapters:Vec<NvencAdapter>,pub required_api_major:u32,pub required_api_minor:u32}
pub fn probe_nvenc()->Result<NvencCatalog,Error>{
    #[cfg(all(target_os="windows",target_arch="x86_64"))] {super::nvenc::probe()}
    #[cfg(not(all(target_os="windows",target_arch="x86_64")))] {Err(Error::Unavailable("NVENC H264 requires Windows x64"))}
}

/// Pure compiled scope; does not enumerate adapters or load the driver.
pub fn encoder_capabilities()->serde_json::Value{serde_json::json!({"default":"software","nvenc_h264":{"compiled":cfg!(all(target_os="windows",target_arch="x86_64")),"availability":"explicit_probe_or_start","api":"12.1","input":"nv12_8bit_sdr_bt709_limited","profile":"baseline","rate_control":["cbr"],"preset":"p4","b_frames":0,"lookahead":false,"adapter_identity":"exact_luid","verified":false}})}
