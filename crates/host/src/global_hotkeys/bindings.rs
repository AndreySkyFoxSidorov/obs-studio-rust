//! Bounded immutable native routes and exact source metadata proofs.
use super::*;
use skylive_core::{HotkeyBinding,Id,MAX_GLOBAL_HOTKEYS,MAX_SOURCE_HOTKEYS};
pub(super) const MAX_REGISTRATIONS:usize=MAX_GLOBAL_HOTKEYS+MAX_SOURCE_HOTKEYS;
#[derive(Clone,PartialEq,Eq)]
pub(super) struct Bindings(Arc<Compiled>);
#[derive(PartialEq,Eq)]
struct Proof {source:Id,epoch:skylive_core::SourceDefinitionEpoch,media:bool,push:skylive_core::PushAudioSettings}
#[derive(PartialEq,Eq)]
struct Compiled{frontend:GlobalHotkeyBindings,sources:Vec<Proof>,routes:Vec<HotkeyBinding>}
impl Bindings{
    pub(super) fn prepare(session:&Session,frontend:&GlobalHotkeyBindings)->Result<Self,Error>{
        let snapshot=session.snapshot();
        if frontend!=snapshot.preferences().global_hotkeys(){return Err(Error::InvalidBindings("Global shortcut preparation no longer matches the session settings.".into()));}
        let source_count=snapshot.sources().iter().try_fold(0usize,|sum,source|sum.checked_add(source.shortcut_chords().count())).ok_or_else(||Error::InvalidBindings("Source shortcut count overflow.".into()))?;
        let count=frontend.bindings().len().checked_add(source_count).filter(|count|*count<=MAX_REGISTRATIONS).ok_or_else(||Error::InvalidBindings("Combined configured shortcut limit exceeded.".into()))?;
        if source_count>MAX_SOURCE_HOTKEYS{return Err(Error::InvalidBindings("Source shortcut limit exceeded.".into()));}
        let mut routes=Vec::new();routes.try_reserve_exact(count).map_err(|_|Error::InvalidBindings("Cannot allocate global shortcut routes.".into()))?;
        routes.extend_from_slice(frontend.bindings());
        let mut sources=Vec::new();sources.try_reserve_exact(source_count).map_err(|_|Error::InvalidBindings("Cannot allocate source shortcut proofs.".into()))?;
        for source in snapshot.sources().iter().filter(|source|source.shortcut_chords().next().is_some()){
            source.audio_hotkeys().validate_kind(source.kind()).map_err(|error|Error::InvalidBindings(error.to_string()))?;
            source.playback_hotkeys().validate_kind(source.kind()).map_err(|error|Error::InvalidBindings(error.to_string()))?;
            for binding in source.playback_hotkeys().bindings(){routes.push(HotkeyBinding{action:HotkeyAction::SourcePlayback{source:source.id(),operation:binding.operation},chord:binding.chord.clone()});}
            for binding in source.audio_hotkeys().bindings(){routes.push(HotkeyBinding{action:HotkeyAction::SourceAudio{source:source.id(),operation:binding.operation},chord:binding.chord.clone()});}
            sources.push(Proof {source:source.id(),epoch:session.source_definition_epoch(source.id()).ok_or_else(||Error::InvalidBindings("Source definition epoch is absent.".into()))?,media:matches!(source.kind(),skylive_core::SourceKind::MediaSource{..}),push:source.audio_properties().push()});
        }
        if routes.len()!=count{return Err(Error::InvalidBindings("Some source shortcut routes are unavailable in this build.".into()));}
        Ok(Self(Arc::new(Compiled{frontend:frontend.clone(),sources,routes})))
    }
    pub(super) fn bindings(&self)->&[HotkeyBinding]{&self.0.routes}
    pub(super) fn media_source(&self,id:Id)->bool {self.0.sources.iter().any(|proof|proof.source==id&&proof.media)}
    pub(super) fn source_epoch(&self,id:Id)->Option<skylive_core::SourceDefinitionEpoch>{self.0.sources.iter().find(|proof|proof.source==id).map(|proof|proof.epoch)}
    pub(super) fn push_settings(&self,id:Id)->Option<skylive_core::PushAudioSettings>{self.0.sources.iter().find(|proof|proof.source==id).map(|proof|proof.push)}
    pub(super) fn accepts(&self,index:usize,action:HotkeyAction)->bool {
        self.bindings().get(index).is_some_and(|binding|binding.action==action||matches!((binding.action,action),(HotkeyAction::SourcePlayback{source,operation:skylive_core::SourcePlaybackAction::PlayPause},HotkeyAction::SourcePlayback{source:other,operation:skylive_core::SourcePlaybackAction::Play|skylive_core::SourcePlaybackAction::Pause})if source==other&&self.media_source(source))||matches!((binding.action,action),(HotkeyAction::SourceAudio{source,operation:skylive_core::SourceAudioAction::ToggleMute},HotkeyAction::SourceAudio{source:other,operation:skylive_core::SourceAudioAction::Mute|skylive_core::SourceAudioAction::Unmute})if source==other))
    }
    pub(super) fn is_empty(&self)->bool{self.0.routes.is_empty()}
    pub(super) fn matches(&self,session:&Session)->bool{
        let snapshot=session.snapshot();
        self.0.frontend==*snapshot.preferences().global_hotkeys()
            &&snapshot.sources().iter().filter(|source|source.shortcut_chords().next().is_some()).count()==self.0.sources.len()
            &&self.0.sources.iter().all(|proof|session.source_definition_epoch(proof.source)==Some(proof.epoch)&&snapshot.source(proof.source).is_some_and(|source|source.shortcut_chords().next().is_some()&&source.audio_properties().push()==proof.push))
    }
}
