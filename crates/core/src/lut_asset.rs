//! Immutable worker-proven LUT identity; no file access or decoded values in Core.
use crate::Error;
use serde::{Serialize,Deserialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum LutFormat {Cube1d,Cube3d,CubeShaper3d,ObsPng64}
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct LutAsset {path:String,sha256:[u8;32],format:LutFormat,size:u32,encoded_bytes:u64}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {path:String,sha256:[u8;32],format:LutFormat,size:u32,encoded_bytes:u64}
impl TryFrom<Data> for LutAsset {type Error=Error;fn try_from(v:Data)->Result<Self,Error>{Self::new(v.path,v.sha256,v.format,v.size,v.encoded_bytes)}}
impl LutAsset {
    pub fn new(path:String,sha256:[u8;32],format:LutFormat,size:u32,encoded_bytes:u64)->Result<Self,Error>{let value=Self {path,sha256,format,size,encoded_bytes};value.validate()?;Ok(value)}
    pub fn path(&self)->&str {&self.path}
    pub fn sha256(&self)->&[u8;32]{&self.sha256}
    pub fn format(&self)->LutFormat{self.format}
    pub fn size(&self)->u32{self.size}
    pub fn encoded_bytes(&self)->u64{self.encoded_bytes}
    // Combined CUBE's 1D length is encoded in the file rather than this
    // historical metadata shape. Reserve its maximum before reading the file.
    pub fn entries(&self)->usize{match self.format {LutFormat::Cube1d=>self.size as usize,LutFormat::CubeShaper3d=>(self.size as usize).pow(3)+65536,_=>(self.size as usize).pow(3)}}
    pub fn table_bytes(&self)->usize{self.entries()*3*std::mem::size_of::<f32>()}
    pub(crate) fn validate(&self)->Result<(),Error>{
        let bytes=self.path.as_bytes();let absolute=self.path.starts_with('/')||self.path.starts_with("\\\\")||(bytes.len()>2&&bytes[0].is_ascii_alphabetic()&&bytes[1]==b':'&&matches!(bytes[2],b'/'|b'\\'));
        if !absolute||self.path.len()>4096||self.path.chars().any(char::is_control){return Err(Error::Invalid("LUT reference requires an absolute path of at most4096 UTF-8 bytes without controls"));}
        if self.encoded_bytes==0||self.encoded_bytes>32*1024*1024{return Err(Error::Invalid("LUT encoded file exceeds32MiB or is empty"));}
        let valid=match self.format {LutFormat::Cube1d=>(2..=65536).contains(&self.size),LutFormat::Cube3d|LutFormat::CubeShaper3d=>(2..=128).contains(&self.size),LutFormat::ObsPng64=>self.size==64};
        if !valid{return Err(Error::Invalid("LUT dimensions exceed the supported1D/3D/PNG profile"));}Ok(())
    }
}
