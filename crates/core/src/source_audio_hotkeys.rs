//! Per-source audio controls, distinct from media playback and held push gates.
use crate::{Command,Error,HotkeyChord,Id,SourceKind,StudioSnapshot};
use serde::{Deserialize,Serialize};
use std::collections::HashSet;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,PartialOrd,Ord,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum SourceAudioAction {ToggleMute,Mute,Unmute,PushToMute,PushToTalk}
impl SourceAudioAction {
    pub const ALL:[Self;5]=[Self::ToggleMute,Self::Mute,Self::Unmute,Self::PushToMute,Self::PushToTalk];
    pub fn label(self)->&'static str{match self{Self::ToggleMute=>"Toggle Mute",Self::Mute=>"Mute",Self::Unmute=>"Unmute",Self::PushToMute=>"Push-to-mute",Self::PushToTalk=>"Push-to-talk"}}
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAudioBinding {pub operation:SourceAudioAction,pub chord:HotkeyChord}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="Vec<SourceAudioBinding>")]
pub struct SourceAudioHotkeys(Vec<SourceAudioBinding>);
impl TryFrom<Vec<SourceAudioBinding>> for SourceAudioHotkeys {type Error=Error;fn try_from(value:Vec<SourceAudioBinding>)->Result<Self,Error>{Self::new(value)}}
impl SourceAudioHotkeys {
    pub fn new(mut bindings:Vec<SourceAudioBinding>)->Result<Self,Error>{
        if bindings.len()>10{return Err(Error::Limit("Source audio controls admit at most two shortcuts per control."));}
        let mut chords=HashSet::new();
        for binding in &bindings {
            if !crate::global_key_supported(&binding.chord){return Err(Error::Invalid("Source mute shortcuts use the supported Windows global keys; punctuation is unavailable."));}
            if !chords.insert(&binding.chord){return Err(Error::Invalid("A source mute shortcut is assigned more than once. Use Toggle Mute for the common pair."));}
            if bindings.iter().filter(|value|value.operation==binding.operation).count()>2{return Err(Error::Limit("An action can have at most two shortcuts."));}
        }
        bindings.sort_by_key(|binding|binding.operation);Ok(Self(bindings.into_boxed_slice().into_vec()))
    }
    pub fn bindings(&self)->&[SourceAudioBinding]{&self.0}
    pub fn chords(&self)->impl Iterator<Item=&HotkeyChord>{self.0.iter().map(|binding|&binding.chord)}
    pub fn is_empty(&self)->bool{self.0.is_empty()}
    pub fn validate_kind(&self,kind:&SourceKind)->Result<(),Error>{
        if !self.is_empty()&&!kind.supports_audio_settings(){return Err(Error::Invalid("Mute shortcuts require an audio-capable input source."));}Ok(())
    }
}
impl StudioSnapshot {
    pub fn check_source_audio_hotkeys(&self,source:Id,bindings:&SourceAudioHotkeys)->Result<(),Error>{
        let value=self.source(source).ok_or(Error::NotFound(source))?;bindings.validate_kind(value.kind())?;
        self.check_source_shortcut_replacement(source,value.audio_hotkeys().chords(),bindings.chords())
    }
    /// Exact pressed-event callback predicate. No source-showing gate, no
    /// playback state, no physical device mute. Current source gain is retained.
    pub fn source_audio_hotkey_command(&self,source:Id,operation:SourceAudioAction)->Result<Option<(SourceAudioAction,Command)>,Error>{
        let value=self.source(source).ok_or(Error::NotFound(source))?;
        if !value.kind().supports_audio_settings(){return Err(Error::Unavailable("This input source has no supported mute control."));}
        let mute=match operation{SourceAudioAction::ToggleMute=>!value.muted(),SourceAudioAction::Mute if !value.muted()=>true,SourceAudioAction::Unmute if value.muted()=>false,_=>return Ok(None)};
        let resolved=if mute{SourceAudioAction::Mute}else{SourceAudioAction::Unmute};
        Ok(Some((resolved,Command::SetAudio{source,gain:value.gain(),muted:mute})))
    }
}
