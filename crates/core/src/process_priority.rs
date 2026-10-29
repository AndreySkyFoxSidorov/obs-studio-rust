//! Authored process scheduling preference. Native classes/handles stay in host.
use serde::{Serialize,Deserialize};
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ProcessPriority {High,AboveNormal,#[default] Normal,BelowNormal,Idle}
impl ProcessPriority {pub fn is_default(&self)->bool{*self==Self::Normal}}
