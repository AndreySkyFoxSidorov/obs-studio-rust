//! Authored next-use replay buffer policy; no encoder, clock or encoded payload.
use crate::Error;
use serde::{Serialize,Deserialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct ReplayBufferSettings {#[serde(default="crate::audio_tracks::track_one",skip_serializing_if="crate::audio_tracks::is_track_one")]audio_track:crate::AudioTrackId,duration_seconds:u32,memory_mib:u32}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {#[serde(default="crate::audio_tracks::track_one")]audio_track:crate::AudioTrackId,duration_seconds:u32,memory_mib:u32}
impl TryFrom<Data> for ReplayBufferSettings {type Error=Error;fn try_from(value:Data)->Result<Self,Error>{Self::new(value.duration_seconds,value.memory_mib).map(|settings|settings.with_audio_track(value.audio_track))}}
impl Default for ReplayBufferSettings {fn default()->Self{Self {audio_track:crate::AudioTrackId::ONE,duration_seconds:20,memory_mib:512}}}
impl ReplayBufferSettings {
    pub fn audio_track(self)->crate::AudioTrackId{self.audio_track}
    pub fn with_audio_track(mut self,track:crate::AudioTrackId)->Self{self.audio_track=track;self}
    pub fn new(duration_seconds:u32,memory_mib:u32)->Result<Self,Error>{let value=Self {audio_track:crate::AudioTrackId::ONE,duration_seconds,memory_mib};value.validate()?;Ok(value)}
    pub fn duration_seconds(self)->u32{self.duration_seconds}
    pub fn memory_mib(self)->u32{self.memory_mib}
    pub fn memory_bytes(self)->u64{u64::from(self.memory_mib)*1024*1024}
    pub fn is_default(&self)->bool{*self==Self::default()}
    pub fn validate(self)->Result<(),Error>{
        if !(1..=600).contains(&self.duration_seconds){return Err(Error::Invalid("Replay buffer duration requires1..600 seconds"));}
        if !(16..=2048).contains(&self.memory_mib){return Err(Error::Invalid("Replay buffer encoded memory requires16..2048 MiB"));}Ok(())
    }
}
