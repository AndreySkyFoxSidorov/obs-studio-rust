//! Authored NDI receive settings, not a network handle or fabricated sender mode.
use crate::{Error,MAX_DISPLAY_PIXELS};
use serde::{Deserialize,Serialize};
pub const NDI_PLUGIN:&str="distroav/distroav";
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NdiSender {pub name:String,pub url:Option<String>}
impl NdiSender {
    pub fn validate(&self)->Result<(),Error>{
        for value in std::iter::once(&self.name).chain(self.url.iter()) {
            if value.trim().is_empty()||value.len()>4096||value.chars().any(char::is_control){return Err(Error::Invalid("NDI sender name/opaque URL requires1..4096 UTF-8 bytes without controls"));}
        }Ok(())
    }
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum NdiBandwidth {Highest,Lowest}
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NdiConnection {pub sender:NdiSender,pub bandwidth:NdiBandwidth,pub audio:bool}
impl NdiConnection {pub fn validate(&self)->Result<(),Error>{self.sender.validate()}}
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct NdiSource {connection:NdiConnection,width:u32,height:u32}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {connection:NdiConnection,width:u32,height:u32}
impl TryFrom<Data> for NdiSource {type Error=Error;fn try_from(v:Data)->Result<Self,Error>{Self::new(v.connection,v.width,v.height)}}
impl NdiSource {
    /// Width/height are the actually probed initial video basis. Subsequent
    /// incoming frames retain their real dynamic dimensions in the host.
    pub fn new(mut connection:NdiConnection,width:u32,height:u32)->Result<Self,Error>{connection.sender.name=connection.sender.name.into_boxed_str().into_string();connection.sender.url=connection.sender.url.map(|s|s.into_boxed_str().into_string());let v=Self {connection,width,height};v.validate()?;Ok(v)}
    pub fn connection(&self)->&NdiConnection{&self.connection}
    pub fn width(&self)->u32{self.width}
    pub fn height(&self)->u32{self.height}
    pub fn audio(&self)->bool{self.connection.audio}
    pub fn validate(&self)->Result<(),Error>{
        self.connection.validate()?;
        if self.width==0||self.height==0||self.width>8192||self.height>8192||u64::from(self.width)*u64::from(self.height)>MAX_DISPLAY_PIXELS{return Err(Error::Invalid("NDI video basis exceeds8192-edge/32M-pixel profile"));}Ok(())
    }
}
