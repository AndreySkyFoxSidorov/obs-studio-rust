//! One bounded chord namespace for all actual per-source shortcut domains.
use crate::{Error,HotkeyChord,Id,Source,StudioSnapshot,MAX_SOURCES};
use std::collections::HashSet;
pub const MAX_SOURCE_HOTKEYS:usize=106;
impl Source {
    /// Shared read-only projection; filter controls extend this same namespace.
    pub fn shortcut_chords(&self)->impl Iterator<Item=&HotkeyChord>{self.playback_hotkeys().chords().chain(self.audio_hotkeys().chords())}
    /// Only authored binding definitions, never gain/mute, playback cursor,
    /// filter enabled/settings or a whole-model runtime hash.
    pub(crate) fn same_shortcut_definition(&self,other:&Source)->bool{self.playback_hotkeys()==other.playback_hotkeys()&&self.audio_hotkeys()==other.audio_hotkeys()}
}
struct Budget<'a>{chords:HashSet<&'a HotkeyChord>,source_count:usize}
impl<'a> Budget<'a>{
    fn new(model:&'a StudioSnapshot)->Result<Self,Error>{
        if model.sources().len()>MAX_SOURCES{return Err(Error::Limit("collection entity budget"));}
        let mut chords=HashSet::new();chords.try_reserve(MAX_SOURCE_HOTKEYS+crate::HotkeyAction::ALL.len()*2+crate::MAX_GLOBAL_HOTKEYS).map_err(|_|Error::Limit("Cannot allocate bounded source shortcut metadata."))?;
        for binding in model.preferences().hotkeys().bindings().iter().chain(model.preferences().global_hotkeys().bindings()){
            if !chords.insert(&binding.chord){return Err(Error::Invalid("A shortcut is assigned in more than one scope."));}
        }Ok(Self{chords,source_count:0})
    }
    fn source(&mut self,chord:&'a HotkeyChord)->Result<(),Error>{
        self.source_count=self.source_count.checked_add(1).ok_or(Error::Limit("Source shortcut count overflow."))?;
        if self.source_count>MAX_SOURCE_HOTKEYS{return Err(Error::Limit("The collection admits at most 106 source shortcuts across all controls."));}
        if !self.chords.insert(chord){return Err(Error::Invalid("This shortcut is already assigned to another source or application action."));}Ok(())
    }
}
impl StudioSnapshot {
    pub(crate) fn validate_source_shortcuts(&self)->Result<(),Error>{
        let mut budget=Budget::new(self)?;
        for source in self.sources(){
            source.playback_hotkeys().validate_kind(source.kind())?;source.audio_hotkeys().validate_kind(source.kind())?;
            for chord in source.shortcut_chords(){budget.source(chord)?;}
        }Ok(())
    }
    /// Crate-owned domain editors supply the current domain's actual chords.
    /// Value exclusion is unambiguous because current full-scope uniqueness is
    /// validated first. All other domains/IDs remain in the same106-slot pool.
    pub(crate) fn check_source_shortcut_replacement<'a,I,J>(&'a self,source:Id,previous:I,replacement:J)->Result<(),Error>
        where I:IntoIterator<Item=&'a HotkeyChord>,J:IntoIterator<Item=&'a HotkeyChord>{
        self.validate_source_shortcuts()?;let target=self.source(source).ok_or(Error::NotFound(source))?;
        let mut old=HashSet::new();old.try_reserve(MAX_SOURCE_HOTKEYS).map_err(|_|Error::Limit("Cannot allocate bounded source shortcut metadata."))?;
        for chord in previous {
            if old.len()>=MAX_SOURCE_HOTKEYS||!target.shortcut_chords().any(|actual|actual==chord)||!old.insert(chord){return Err(Error::Invalid("Source shortcut replacement does not match current bindings."));}
        }
        let mut budget=Budget::new(self)?;
        for value in self.sources(){for chord in value.shortcut_chords(){if value.id()==source&&old.contains(chord){continue;}budget.source(chord)?;}}
        for chord in replacement{budget.source(chord)?;}Ok(())
    }
}
