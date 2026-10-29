//! Sender identity/settings only; loading preferences never starts network IO.
use crate::Error;
use serde::{Deserialize,Serialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum NdiOutputRoute {Program,Preview}
impl NdiOutputRoute {pub fn index(self)->usize{match self {Self::Program=>0,Self::Preview=>1}}}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct NdiOutputSettings {#[serde(default="crate::audio_tracks::track_one",skip_serializing_if="crate::audio_tracks::is_track_one")]audio_track:crate::AudioTrackId,program_name:String,preview_name:String,groups:String,program_audio:bool}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {#[serde(default="crate::audio_tracks::track_one")]audio_track:crate::AudioTrackId,program_name:String,preview_name:String,groups:String,program_audio:bool}
impl TryFrom<Data> for NdiOutputSettings {type Error=Error;fn try_from(v:Data)->Result<Self,Error>{Self::new(v.program_name,v.preview_name,v.groups,v.program_audio).map(|settings|settings.with_audio_track(v.audio_track))}}
impl Default for NdiOutputSettings {fn default()->Self{Self {audio_track:crate::AudioTrackId::ONE,program_name:"SkyLive Program".into(),preview_name:"SkyLive Preview".into(),groups:String::new(),program_audio:true}}}
impl NdiOutputSettings {
    pub fn audio_track(&self)->crate::AudioTrackId{self.audio_track}
    pub fn with_audio_track(mut self,track:crate::AudioTrackId)->Self{self.audio_track=track;self}
    pub fn new(program_name:String,preview_name:String,groups:String,program_audio:bool)->Result<Self,Error>{let value=Self {audio_track:crate::AudioTrackId::ONE,program_name:program_name.into_boxed_str().into_string(),preview_name:preview_name.into_boxed_str().into_string(),groups:groups.into_boxed_str().into_string(),program_audio};value.validate()?;Ok(value)}
    pub fn validate(&self)->Result<(),Error>{
        for name in [&self.program_name,&self.preview_name]{if name.trim().is_empty()||name.len()>256||name.chars().any(char::is_control){return Err(Error::Invalid("NDI sender names must contain 1 to 256 UTF-8 bytes and no control characters."));}}
        if self.program_name==self.preview_name{return Err(Error::Invalid("NDI Program and Preview names must be different."));}
        if self.groups.len()>512||self.groups.chars().any(char::is_control){return Err(Error::Invalid("NDI groups must contain at most 512 UTF-8 bytes and no control characters."));}Ok(())
    }
    pub fn program_name(&self)->&str{&self.program_name}pub fn preview_name(&self)->&str{&self.preview_name}
    pub fn groups(&self)->&str{&self.groups}pub fn program_audio(&self)->bool{self.program_audio}
    pub fn is_default(&self)->bool{self==&Self::default()}
    pub fn name(&self,route:NdiOutputRoute)->&str{match route {NdiOutputRoute::Program=>&self.program_name,NdiOutputRoute::Preview=>&self.preview_name}}
}
