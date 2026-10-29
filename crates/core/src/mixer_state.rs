//! Saved mixer view metadata. These values never configure capture or DSP.
use crate::{Collection, Error, Id, StudioSnapshot};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MixerLayout {
    #[default]
    Vertical,
    Horizontal,
}

/// Meter selection affects presentation measurements, never the audio output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MixerPeakMode {
    #[default]
    Sample,
    TruePeakEstimate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MixerDisplayPreferences {
    #[serde(default)]
    layout: MixerLayout,
    #[serde(default)]
    show_inactive: bool,
    #[serde(default)]
    show_hidden: bool,
    #[serde(default)]
    keep_inactive_last: bool,
    #[serde(default)]
    keep_hidden_last: bool,
    #[serde(default)]
    peak_mode: MixerPeakMode,
}
impl MixerDisplayPreferences {
    pub fn new(
        layout: MixerLayout,
        show_inactive: bool,
        show_hidden: bool,
        keep_inactive_last: bool,
        keep_hidden_last: bool,
    ) -> Self {
        Self {
            layout,
            show_inactive,
            show_hidden,
            keep_inactive_last,
            keep_hidden_last,
            peak_mode: MixerPeakMode::Sample,
        }
    }
    pub fn with_peak_mode(mut self, mode: MixerPeakMode) -> Self { self.peak_mode = mode; self }
    pub fn peak_mode(self) -> MixerPeakMode { self.peak_mode }
    pub fn layout(self) -> MixerLayout {
        self.layout
    }
    pub fn show_inactive(self) -> bool {
        self.show_inactive
    }
    pub fn show_hidden(self) -> bool {
        self.show_hidden
    }
    pub fn keep_inactive_last(self) -> bool {
        self.keep_inactive_last
    }
    pub fn keep_hidden_last(self) -> bool {
        self.keep_hidden_last
    }
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MixerSourceState {
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    volume_locked: bool,
}
impl MixerSourceState {
    pub fn hidden(self) -> bool {
        self.hidden
    }
    pub fn pinned(self) -> bool {
        self.pinned
    }
    pub fn volume_locked(self) -> bool {
        self.volume_locked
    }
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub(crate) fn validate(self) -> Result<(), Error> {
        if self.pinned && self.hidden {
            return Err(Error::Invalid("pinned mixer sources cannot be hidden"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MixerSourceAction {
    SetHidden(bool),
    SetPinned(bool),
    SetVolumeLocked(bool),
}

impl Collection {
    pub(crate) fn set_source_mixer_state(
        &mut self,
        source: Id,
        action: MixerSourceAction,
    ) -> Result<(), Error> {
        let current = self.source(source).ok_or(Error::NotFound(source))?;
        if !current.kind().has_audio() {
            return Err(Error::Invalid(
                "mixer view metadata requires an audio source",
            ));
        }
        let global = self.global_audio.iter().any(|slot| slot.source() == source);
        let mut state = current.mixer;
        match action {
            MixerSourceAction::SetHidden(hidden) => {
                if global || state.pinned {
                    return Err(Error::Unavailable(
                        "global or pinned mixer sources cannot be hidden",
                    ));
                }
                state.hidden = hidden;
            }
            MixerSourceAction::SetPinned(pinned) => {
                if global {
                    return Err(Error::Unavailable("global mixer sources cannot be pinned"));
                }
                state.pinned = pinned;
                if pinned {
                    state.hidden = false;
                }
            }
            MixerSourceAction::SetVolumeLocked(locked) => state.volume_locked = locked,
        }
        state.validate()?;
        self.sources
            .iter_mut()
            .find(|item| item.id == source)
            .ok_or(Error::NotFound(source))?
            .mixer = state;
        Ok(())
    }
    pub(crate) fn unhide_all_mixer_sources(&mut self) {
        for source in &mut self.sources {
            source.mixer.hidden = false;
        }
    }
    /// An authored Undo must not undo view metadata for a still-live identity.
    /// A resurrected deleted source instead retains its historical source state.
    pub(crate) fn preserve_live_mixer_state(&mut self, live: &Collection) {
        for source in &mut self.sources {
            if let Some(current) = live.source(source.id) {
                source.mixer = current.mixer;
            }
        }
    }
}
impl StudioSnapshot {
    pub fn has_mixer_metadata(&self) -> bool {
        !self.preferences().mixer_display().is_default()
            || self
                .sources()
                .iter()
                .any(|source| !source.mixer_state().is_default())
    }
}
