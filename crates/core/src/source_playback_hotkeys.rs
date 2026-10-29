//! Authored source identity and chords; no registration or playback clocks.
use crate::{Error,HotkeyChord,SourceKind,StudioSnapshot};
use serde::{Deserialize,Serialize};
use std::collections::HashSet;
pub const MAX_SOURCE_PLAYBACK_HOTKEYS:usize=crate::MAX_SOURCE_HOTKEYS;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,PartialOrd,Ord,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum SourcePlaybackAction{PlayPause,Play,Pause,Restart,Stop,Previous,Next}
impl SourcePlaybackAction{
    pub const ALL:[Self;7]=[Self::PlayPause,Self::Play,Self::Pause,Self::Restart,Self::Stop,Self::Previous,Self::Next];
    pub const SLIDESHOW:[Self;5]=[Self::PlayPause,Self::Restart,Self::Stop,Self::Previous,Self::Next];
    pub const MEDIA:[Self;5]=[Self::PlayPause,Self::Play,Self::Pause,Self::Restart,Self::Stop];
    pub fn label(self)->&'static str{match self{Self::PlayPause=>"Play/Pause",Self::Play=>"Play",Self::Pause=>"Pause",Self::Restart=>"Restart",Self::Stop=>"Stop",Self::Previous=>"Previous in Playlist",Self::Next=>"Next in Playlist"}}
    pub fn supported(self,kind:&SourceKind)->bool{matches!(kind,SourceKind::ImageSlideShow{..})&&Self::SLIDESHOW.contains(&self)||matches!(kind,SourceKind::MediaSource{..})&&Self::MEDIA.contains(&self)}
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePlaybackBinding{pub operation:SourcePlaybackAction,pub chord:HotkeyChord}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="Vec<SourcePlaybackBinding>")]
pub struct SourcePlaybackHotkeys(Vec<SourcePlaybackBinding>);
impl TryFrom<Vec<SourcePlaybackBinding>> for SourcePlaybackHotkeys{type Error=Error;fn try_from(value:Vec<SourcePlaybackBinding>)->Result<Self,Error>{Self::new(value)}}
impl SourcePlaybackHotkeys{
    pub fn new(mut bindings:Vec<SourcePlaybackBinding>)->Result<Self,Error>{
        if bindings.len()>10{return Err(Error::Limit("Source playback admits at most two shortcuts per control."));}
        let mut chords=HashSet::new();
        for binding in &bindings{
            if !crate::global_key_supported(&binding.chord){return Err(Error::Invalid("Source playback shortcuts use the supported Windows global keys; punctuation is unavailable."));}
            if !chords.insert(&binding.chord){return Err(Error::Invalid("A source playback shortcut is assigned more than once."));}
            if bindings.iter().filter(|value|value.operation==binding.operation).count()>2{return Err(Error::Limit("An action can have at most two shortcuts."));}
        }
        bindings.sort_by_key(|binding|binding.operation);
        Ok(Self(bindings.into_boxed_slice().into_vec()))
    }
    pub fn bindings(&self)->&[SourcePlaybackBinding]{&self.0}
    pub fn is_empty(&self)->bool{self.0.is_empty()}
    pub fn chords(&self)->impl Iterator<Item=&HotkeyChord>{self.0.iter().map(|binding|&binding.chord)}
    pub fn validate_kind(&self,kind:&SourceKind)->Result<(),Error>{
        if self.0.iter().any(|binding|!binding.operation.supported(kind)){return Err(Error::Invalid("Playback shortcuts require a matching Media Source or Slideshow control."));}Ok(())
    }
}
impl StudioSnapshot{
    pub(crate) fn validate_source_playback_hotkeys(&self)->Result<(),Error>{self.validate_source_shortcuts()}
    pub fn check_source_playback_hotkeys(&self,source:crate::Id,bindings:&SourcePlaybackHotkeys)->Result<(),Error>{
        let value=self.source(source).ok_or(Error::NotFound(source))?;bindings.validate_kind(value.kind())?;
        self.check_source_shortcut_replacement(source,value.playback_hotkeys().chords(),bindings.chords())
    }
}
