//! OBS logical audio mixes1..6, independent of dense container track indices.
use serde::{Serialize,Deserialize};use crate::Error;
#[derive(Clone,Copy,Debug,PartialEq,Eq,PartialOrd,Ord,Serialize,Deserialize)]#[serde(try_from="u8",into="u8")]
pub struct AudioTrackId(u8);
impl AudioTrackId{pub const ONE:Self=Self(1);pub fn new(id:u8)->Result<Self,Error>{if (1..=6).contains(&id){Ok(Self(id))}else{Err(Error::Invalid("audio track ID must be1..6"))}}pub fn get(self)->u8{self.0}pub fn index(self)->usize{usize::from(self.0-1)}pub fn bit(self)->u8{1<<(self.0-1)}}
impl TryFrom<u8> for AudioTrackId{type Error=Error;fn try_from(id:u8)->Result<Self,Error>{Self::new(id)}}impl From<AudioTrackId> for u8{fn from(id:AudioTrackId)->u8{id.0}}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]#[serde(try_from="TrackData")]
pub struct RecordingTrack{name:String,bitrate_kbps:Option<u32>}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct TrackData{#[serde(default)]name:String,#[serde(default)]bitrate_kbps:Option<u32>}
impl TryFrom<TrackData> for RecordingTrack{type Error=Error;fn try_from(v:TrackData)->Result<Self,Error>{Self::new(v.name,v.bitrate_kbps)}}
impl RecordingTrack{pub fn new(name:String,bitrate_kbps:Option<u32>)->Result<Self,Error>{let v=Self{name:name.into_boxed_str().into_string(),bitrate_kbps};v.validate()?;Ok(v)}pub fn name(&self)->&str{&self.name}pub fn bitrate_kbps(&self)->Option<u32>{self.bitrate_kbps}pub fn resolved_bitrate(&self,legacy:u32)->u32{self.bitrate_kbps.unwrap_or(legacy)}pub fn validate(&self)->Result<(),Error>{if self.name.len()>256||self.name.chars().any(char::is_control){return Err(Error::Invalid("recording track name exceeds256UTF8bytes or contains controls"));}if self.bitrate_kbps.is_some_and(|v|![96,128,160,192].contains(&v)){return Err(Error::Invalid("recording track AAC bitrate must be96,128,160 or192kbit/s"));}Ok(())}}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]#[serde(try_from="TracksData")]
pub struct RecordingTracks{mask:u8,entries:[RecordingTrack;6]}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct TracksData{mask:u8,entries:[RecordingTrack;6]}
impl TryFrom<TracksData> for RecordingTracks{type Error=Error;fn try_from(v:TracksData)->Result<Self,Error>{Self::new(v.mask,v.entries)}}
impl Default for RecordingTracks{fn default()->Self{Self{mask:1,entries:std::array::from_fn(|_|RecordingTrack::default())}}}
impl RecordingTracks{pub fn new(mask:u8,entries:[RecordingTrack;6])->Result<Self,Error>{let value=Self{mask,entries};value.validate()?;Ok(value)}pub fn mask(&self)->u8{self.mask}pub fn entry(&self,id:AudioTrackId)->&RecordingTrack{&self.entries[id.index()]}pub fn selected(&self)->impl Iterator<Item=AudioTrackId>+'_ {(1u8..=6).filter_map(|id|{let track=AudioTrackId(id);(self.mask&track.bit()!=0).then_some(track)})}pub fn is_default(&self)->bool{*self==Self::default()}pub fn validate(&self)->Result<(),Error>{if self.mask==0||self.mask&!0x3f!=0{return Err(Error::Invalid("recording track selection must be a nonempty six-bit mask"));}for entry in &self.entries{entry.validate()?;}Ok(())}}
