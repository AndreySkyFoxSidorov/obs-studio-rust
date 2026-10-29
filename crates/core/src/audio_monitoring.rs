//! Source monitoring is authored routing. A device preference chooses a sink;
//! neither setting claims a native endpoint opened successfully.
use crate::Error;
use serde::{Deserialize,Serialize};
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum AudioMonitoringMode {#[default] Off,MonitorOnly,MonitorAndOutput}
impl AudioMonitoringMode {
    pub fn is_off(&self)->bool{*self==Self::Off}
    pub fn label(self)->&'static str {match self {Self::Off=>"Monitor Off",Self::MonitorOnly=>"Monitor Only (mute output)",Self::MonitorAndOutput=>"Monitor and Output"}}
}
#[derive(Clone,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum AudioMonitoringDevice {#[default] Default,WindowsEndpoint {id:String}}
impl AudioMonitoringDevice {
    pub fn is_default(&self)->bool{matches!(self,Self::Default)}
    pub fn validate(&self)->Result<(),Error>{
        if let Self::WindowsEndpoint {id}=self {if id.is_empty()||id.len()>1024||id.chars().any(char::is_control){return Err(Error::Invalid("monitor endpoint ID requires 1..=1024 UTF-8 bytes without controls"));}}
        Ok(())
    }
}
