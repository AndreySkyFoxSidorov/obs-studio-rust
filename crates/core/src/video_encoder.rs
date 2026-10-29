//! Authored codec choice only. Native probing/driver ownership belongs to host.
use serde::{Deserialize,Serialize};
use crate::Error;
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="provider",rename_all="snake_case",deny_unknown_fields)]
pub enum VideoEncoder {
    #[default] Software,
    /// An exact Windows adapter identity, never an index or fallback hint.
    NvencH264 {adapter_luid:u64},
}
impl VideoEncoder {
    pub fn validate(self)->Result<(),Error>{if matches!(self,Self::NvencH264 {adapter_luid:0}){return Err(Error::Invalid("NVENC adapter LUID is zero"));}Ok(())}
    pub fn is_default(self)->bool{self==Self::Software}
}
