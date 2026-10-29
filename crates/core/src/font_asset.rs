//! Exact linked static font identity. Font parsing and glyphs belong to Host.
use crate::Error;
use serde::{Deserialize,Serialize};
pub const MAX_FONT_FILE_BYTES:u64=16*1024*1024;
pub const MAX_FONT_FACES:u32=16;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum FontFaceStyle {Normal,Italic,Oblique}
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(try_from="FaceData")]
pub struct FontFace {index:u32,family:String,post_script_name:String,weight:u16,stretch:u16,style:FontFaceStyle,glyphs:u16,units_per_em:u16}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FaceData {index:u32,family:String,post_script_name:String,weight:u16,stretch:u16,style:FontFaceStyle,glyphs:u16,units_per_em:u16}
impl TryFrom<FaceData> for FontFace {type Error=Error;fn try_from(v:FaceData)->Result<Self,Error>{Self::new(v.index,v.family,v.post_script_name,v.weight,v.stretch,v.style,v.glyphs,v.units_per_em)}}
impl FontFace {
    #[allow(clippy::too_many_arguments)]
    pub fn new(index:u32,family:String,post_script_name:String,weight:u16,stretch:u16,style:FontFaceStyle,glyphs:u16,units_per_em:u16)->Result<Self,Error>{
        let result=Self{index,family,post_script_name,weight,stretch,style,glyphs,units_per_em};result.validate()?;Ok(result)
    }
    pub(crate) fn validate(&self)->Result<(),Error>{
        let name=|value:&str|!value.trim().is_empty()&&value.len()<=256&&!value.chars().any(char::is_control);
        if self.index>=MAX_FONT_FACES||!name(&self.family)||!name(&self.post_script_name)
            ||!(1..=1000).contains(&self.weight)||!(1..=9).contains(&self.stretch)
            ||self.glyphs==0||self.glyphs>32768||!(16..=16384).contains(&self.units_per_em){
            return Err(Error::Invalid("Font face metadata exceeds the supported static outline profile."));
        }Ok(())
    }
    pub fn index(&self)->u32{self.index}
    pub fn family(&self)->&str{&self.family}
    pub fn post_script_name(&self)->&str{&self.post_script_name}
    pub fn weight(&self)->u16{self.weight}
    pub fn stretch(&self)->u16{self.stretch}
    pub fn style(&self)->FontFaceStyle{self.style}
    pub fn glyphs(&self)->u16{self.glyphs}
    pub fn units_per_em(&self)->u16{self.units_per_em}
}
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct FontAsset {path:String,sha256:[u8;32],encoded_bytes:u64,face_count:u32,face:FontFace}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {path:String,sha256:[u8;32],encoded_bytes:u64,face_count:u32,face:FontFace}
impl TryFrom<Data> for FontAsset {type Error=Error;fn try_from(v:Data)->Result<Self,Error>{Self::new(v.path,v.sha256,v.encoded_bytes,v.face_count,v.face)}}
impl FontAsset {
    pub fn new(path:String,sha256:[u8;32],encoded_bytes:u64,face_count:u32,face:FontFace)->Result<Self,Error>{
        let value=Self{path,sha256,encoded_bytes,face_count,face};value.validate()?;Ok(value)
    }
    pub(crate) fn validate(&self)->Result<(),Error>{
        let bytes=self.path.as_bytes();let absolute=self.path.starts_with('/')||self.path.starts_with("\\\\")
            ||(bytes.len()>2&&bytes[0].is_ascii_alphabetic()&&bytes[1]==b':'&&matches!(bytes[2],b'/'|b'\\'));
        if !absolute||self.path.len()>4096||self.path.chars().any(char::is_control){return Err(Error::Invalid("Font file requires an absolute path of at most4096 UTF-8 bytes without controls."));}
        if self.encoded_bytes==0||self.encoded_bytes>MAX_FONT_FILE_BYTES||self.face_count==0||self.face_count>MAX_FONT_FACES||self.face.index>=self.face_count{
            return Err(Error::Invalid("Font file exceeds16MiB or16 faces, or the selected face is invalid."));
        }self.face.validate()
    }
    pub fn path(&self)->&str{&self.path}
    pub fn sha256(&self)->&[u8;32]{&self.sha256}
    pub fn encoded_bytes(&self)->u64{self.encoded_bytes}
    pub fn face_count(&self)->u32{self.face_count}
    pub fn face(&self)->&FontFace{&self.face}
}
