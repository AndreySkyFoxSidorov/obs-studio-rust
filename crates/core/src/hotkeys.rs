//! Portable shortcut authoring. Native global registration belongs to the host;
//! persisted bindings never imply successful OS registration.
use crate::Error;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyAction {
    Undo,
    Redo,
    Save,
    ToggleStudio,
    Transition,
    QuickCut,
    StartRecording,
    StopRecording,
    PauseRecording,
    ResumeRecording,
    StartStreaming,
    StopStreaming,
    StartReplayBuffer,
    StopReplayBuffer,
    SaveReplayBuffer,
    StartVirtualCamera,
    StopVirtualCamera,
    ScreenshotOutput,
    ScreenshotSelectedSource,
    /// Internal native route; authored only by SourcePlaybackHotkeys.
    SourcePlayback{source:crate::Id,operation:crate::SourcePlaybackAction},
    /// Internal native route; authored only by this exact Source.
    SourceAudio{source:crate::Id,operation:crate::SourceAudioAction},
}
impl HotkeyAction {
    pub const ALL: [Self; 19] = [
        Self::Undo,
        Self::Redo,
        Self::Save,
        Self::ToggleStudio,
        Self::Transition,
        Self::QuickCut,
        Self::StartRecording,
        Self::StopRecording,
        Self::PauseRecording, Self::ResumeRecording,
        Self::StartStreaming, Self::StopStreaming,
        Self::StartReplayBuffer, Self::StopReplayBuffer, Self::SaveReplayBuffer,
        Self::StartVirtualCamera, Self::StopVirtualCamera,
        Self::ScreenshotOutput, Self::ScreenshotSelectedSource,
    ];
    pub const OUTPUT: [Self;11]=[Self::StartRecording,Self::StopRecording,Self::PauseRecording,Self::ResumeRecording,Self::StartStreaming,Self::StopStreaming,Self::StartReplayBuffer,Self::StopReplayBuffer,Self::SaveReplayBuffer,Self::StartVirtualCamera,Self::StopVirtualCamera];
    pub fn is_output_action(self)->bool {Self::OUTPUT.contains(&self)}
    /// Existing application intentions safe to dispatch from an acknowledged
    /// native registration. Document editing shortcuts remain window-local.
    pub const GLOBAL:[Self;16]=[
        Self::StartRecording,Self::StopRecording,Self::PauseRecording,Self::ResumeRecording,
        Self::StartStreaming,Self::StopStreaming,Self::StartReplayBuffer,Self::StopReplayBuffer,
        Self::SaveReplayBuffer,Self::StartVirtualCamera,Self::StopVirtualCamera,
        Self::ToggleStudio,Self::Transition,Self::QuickCut,
        Self::ScreenshotOutput,Self::ScreenshotSelectedSource,
    ];
    pub fn is_global_action(self)->bool{Self::GLOBAL.contains(&self)}
    pub fn requires_extended_format(self)->bool {
        matches!(self,Self::PauseRecording|Self::ResumeRecording|Self::StartStreaming|Self::StopStreaming
            |Self::StartReplayBuffer|Self::StopReplayBuffer|Self::SaveReplayBuffer|Self::StartVirtualCamera|Self::StopVirtualCamera
            |Self::ScreenshotOutput|Self::ScreenshotSelectedSource)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "ChordData")]
pub struct HotkeyChord {
    key: String,
    primary: bool,
    alt: bool,
    shift: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChordData {
    key: String,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    alt: bool,
    #[serde(default)]
    shift: bool,
}
impl TryFrom<ChordData> for HotkeyChord {
    type Error = Error;
    fn try_from(value: ChordData) -> Result<Self, Error> {
        Self::new(&value.key, value.primary, value.alt, value.shift)
    }
}
impl HotkeyChord {
    pub fn new(key: &str, primary: bool, alt: bool, shift: bool) -> Result<Self, Error> {
        let letter_or_digit = key.len() == 1
            && key
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        let function = key
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=24).contains(&n) && key == format!("F{n}"));
        let named = matches!(
            key,
            "ArrowDown"
                | "ArrowLeft"
                | "ArrowRight"
                | "ArrowUp"
                | "Tab"
                | "Backspace"
                | "Enter"
                | "Insert"
                | "Delete"
                | "Home"
                | "End"
                | "PageUp"
                | "PageDown"
                | "Space"
                | "Colon"
                | "Comma"
                | "Minus"
                | "Period"
                | "Plus"
                | "Equals"
                | "Semicolon"
                | "Backslash"
                | "Slash"
                | "Pipe"
                | "Questionmark"
                | "Exclamationmark"
                | "OpenBracket"
                | "CloseBracket"
                | "OpenCurlyBracket"
                | "CloseCurlyBracket"
                | "Backtick"
                | "Quote"
        );
        let keypad = key.strip_prefix("Numpad").is_some_and(|suffix|
            (suffix.len() == 1 && suffix.as_bytes()[0].is_ascii_digit())
                || matches!(suffix, "Add" | "Subtract" | "Multiply" | "Divide" | "Decimal"));
        if !letter_or_digit && !function && !named && !keypad && key != "NumLock" {
            return Err(Error::Invalid(
                "This key is not supported for application shortcuts.",
            ));
        }
        Ok(Self {
            key: key.to_owned().into_boxed_str().into_string(),
            primary,
            alt,
            shift,
        })
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn primary(&self) -> bool {
        self.primary
    }
    pub fn alt(&self) -> bool {
        self.alt
    }
    pub fn shift(&self) -> bool {
        self.shift
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HotkeyBinding {
    pub action: HotkeyAction,
    pub chord: HotkeyChord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<HotkeyBinding>")]
pub struct HotkeyBindings(Vec<HotkeyBinding>);
impl TryFrom<Vec<HotkeyBinding>> for HotkeyBindings {
    type Error = Error;
    fn try_from(value: Vec<HotkeyBinding>) -> Result<Self, Error> {
        Self::new(value)
    }
}
impl Default for HotkeyBindings {
    fn default() -> Self {
        let chord = |key: &str, shift| HotkeyChord {
            key: key.into(),
            primary: true,
            alt: false,
            shift,
        };
        Self(vec![
            HotkeyBinding {
                action: HotkeyAction::Undo,
                chord: chord("Z", false),
            },
            HotkeyBinding {
                action: HotkeyAction::Redo,
                chord: chord("Z", true),
            },
            HotkeyBinding {
                action: HotkeyAction::Save,
                chord: chord("S", false),
            },
        ])
    }
}
impl HotkeyBindings {
    pub fn new(mut bindings: Vec<HotkeyBinding>) -> Result<Self, Error> {
        if bindings.len() > HotkeyAction::ALL.len() * 2 {
            return Err(Error::Limit(
                    "Application shortcuts exceed two bindings per supported action.",
            ));
        }
        let mut chords = HashSet::new();
        for binding in &bindings {
            if matches!(binding.action,HotkeyAction::SourceAudio{..}){return Err(Error::Invalid("Source audio shortcuts belong to their source, not the frontend shortcut list."));}
            if matches!(binding.action,HotkeyAction::SourcePlayback{..}){return Err(Error::Invalid("Source playback shortcuts belong to their source, not the frontend shortcut list."));}
            if !chords.insert(binding.chord.clone()) {
                return Err(Error::Invalid(
                    "A shortcut is assigned more than once. Choose a different key combination.",
                ));
            }
            if bindings
                .iter()
                .filter(|b| b.action == binding.action)
                .count()
                > 2
            {
                return Err(Error::Limit("An action can have at most two shortcuts."));
            }
        }
        bindings.sort_by_key(|binding| binding.action);
        Ok(Self(bindings.into_boxed_slice().into_vec()))
    }
    pub fn bindings(&self) -> &[HotkeyBinding] {
        &self.0
    }
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

pub const MAX_GLOBAL_HOTKEYS:usize=22;
/// A conservative portable set with unambiguous Windows virtual-key identities.
/// OEM punctuation/layout aliases are not silently mapped to another chord.
pub fn global_key_supported(chord:&HotkeyChord)->bool {
    let key=chord.key();
    (key.len()==1&&key.bytes().all(|b|b.is_ascii_uppercase()||b.is_ascii_digit()))
        || key.strip_prefix('F').and_then(|n|n.parse::<u8>().ok()).is_some_and(|n|(1..=24).contains(&n))
        || key == "NumLock" || key.strip_prefix("Numpad").is_some_and(|suffix|
            (suffix.len() == 1 && suffix.as_bytes()[0].is_ascii_digit())
                || matches!(suffix, "Add" | "Subtract" | "Multiply" | "Divide" | "Decimal"))
        || matches!(key,"ArrowDown"|"ArrowLeft"|"ArrowRight"|"ArrowUp"|"Tab"|"Backspace"|"Enter"|"Insert"|"Delete"|"Home"|"End"|"PageUp"|"PageDown"|"Space")
}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="Vec<HotkeyBinding>")]
pub struct GlobalHotkeyBindings(Vec<HotkeyBinding>);
impl TryFrom<Vec<HotkeyBinding>> for GlobalHotkeyBindings {type Error=Error;fn try_from(value:Vec<HotkeyBinding>)->Result<Self,Error>{Self::new(value)}}
impl GlobalHotkeyBindings {
    pub fn new(bindings:Vec<HotkeyBinding>)->Result<Self,Error>{
        if bindings.len()>MAX_GLOBAL_HOTKEYS{return Err(Error::Limit("Global shortcuts admit at most 22 assignments."));}
        if bindings.iter().any(|b|!b.action.is_global_action()){return Err(Error::Invalid("Global shortcuts support output, Studio and screenshot actions. Document editing shortcuts work in SkyLive only."));}
        if bindings.iter().any(|b|!global_key_supported(&b.chord)){return Err(Error::Invalid("Global shortcuts support letters, digits, navigation and function keys; punctuation is unavailable."));}
        let local=HotkeyBindings::new(bindings)?;
        Ok(Self(local.0))
    }
    pub fn bindings(&self)->&[HotkeyBinding]{&self.0}
    pub fn is_empty(&self)->bool{self.0.is_empty()}
    pub fn requires_extended_format(&self)->bool{!self.is_empty()}
}
pub fn validate_hotkey_scopes(local:&HotkeyBindings,global:&GlobalHotkeyBindings)->Result<(),Error>{
    if global.bindings().iter().any(|g|local.bindings().iter().any(|l|l.chord==g.chord)){
        return Err(Error::Invalid("A shortcut cannot be assigned in both local and global scopes."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_keys_conflicts_and_bounded_bindings_are_validated_before_publication() {
        for key in ["A", "0", "F1", "F24", "ArrowLeft", "Plus", "Space"] {
            assert!(HotkeyChord::new(key, true, false, false).is_ok());
        }
        for key in [
            "",
            "a",
            "F0",
            "F01",
            "F25",
            "Escape",
            "А",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(HotkeyChord::new(key, false, false, false).is_err());
        }
        let chord = HotkeyChord::new("F9", false, false, false).unwrap();
        let binding = HotkeyBinding {
            action: HotkeyAction::StartRecording,
            chord,
        };
        assert!(HotkeyBindings::new(vec![binding.clone(), binding]).is_err());
        assert!(
            HotkeyBindings::new(Vec::new())
                .unwrap()
                .bindings()
                .is_empty()
        );
    }
    #[test]
    fn preferences_edit_preserves_scene_undo_and_noop_revision() {
        use crate::{Command, Session};
        let mut session = Session::new();
        session
            .apply(Command::RenameCollection {
                name: "Authored edit".into(),
            })
            .unwrap();
        let document = session.document_revision();
        session
            .apply(Command::SetHotkeys {
                hotkeys: HotkeyBindings::new(Vec::new()).unwrap(),
            })
            .unwrap();
        assert_eq!(session.document_revision(), document);
        assert!(
            session
                .snapshot()
                .preferences()
                .hotkeys()
                .bindings()
                .is_empty()
        );
        let revision = session.snapshot().revision();
        session
            .apply(Command::SetHotkeys {
                hotkeys: HotkeyBindings::new(Vec::new()).unwrap(),
            })
            .unwrap();
        assert_eq!(session.snapshot().revision(), revision);
        session.apply(Command::Undo).unwrap();
        assert!(
            session
                .snapshot()
                .preferences()
                .hotkeys()
                .bindings()
                .is_empty()
        );
    }
}
