//! Per-source OBS mix routing. Zero is valid and does not disable DSP/monitoring.
use crate::{AudioTrackId,Error};
use serde::{Serialize,Deserialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="u8",into="u8")]
pub struct SourceAudioTrackMask(u8);
impl Default for SourceAudioTrackMask{fn default()->Self{Self::ALL}}
impl SourceAudioTrackMask{
 pub const ALL:Self=Self(63);pub const NONE:Self=Self(0);
 pub fn new(bits:u8)->Result<Self,Error>{if bits&!63==0{Ok(Self(bits))}else{Err(Error::Invalid("source audio track mask must contain only six bits"))}}
 pub fn bits(self)->u8{self.0}pub fn contains(self,track:AudioTrackId)->bool{self.0&track.bit()!=0}
 pub fn is_all(&self)->bool{self.0==63}pub fn is_empty(self)->bool{self.0==0}
 pub fn intersects(self,other:Self)->bool{self.0&other.0!=0}
 pub fn with(self,track:AudioTrackId,enabled:bool)->Self{Self(if enabled{self.0|track.bit()}else{self.0&!track.bit()})}
 pub fn union(self,other:Self)->Self{Self(self.0|other.0)}
}
impl TryFrom<u8> for SourceAudioTrackMask{type Error=Error;fn try_from(bits:u8)->Result<Self,Error>{Self::new(bits)}}
impl From<SourceAudioTrackMask> for u8{fn from(mask:SourceAudioTrackMask)->u8{mask.0}}
pub(crate) fn track_one()->AudioTrackId{AudioTrackId::ONE}
pub(crate) fn is_track_one(track:&AudioTrackId)->bool{*track==AudioTrackId::ONE}
