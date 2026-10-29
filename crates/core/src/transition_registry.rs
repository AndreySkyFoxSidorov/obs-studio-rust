//! Authored named transition presets and shared references. Resolved configs
//! are owned views; references never duplicate a preset's editable kind.
use crate::{Error, TransitionConfig, TransitionKind, validate_name};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;

pub const MAX_TRANSITION_PRESETS: usize = 64;
pub(crate) const PRESET_ID_SENTINEL: u32 = i32::MAX as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TransitionPresetId(u32);
impl TransitionPresetId {
    pub fn new(value: u32) -> Result<Self, Error> {
        if value == 0 || value >= PRESET_ID_SENTINEL {
            return Err(Error::Invalid(
                "transition preset ID must be in 1..=2147483646",
            ));
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u32 {
        self.0
    }
}
impl<'de> Deserialize<'de> for TransitionPresetId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(u32::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionRef {
    Cut,
    Fade,
    Preset(TransitionPresetId),
}
impl TransitionRef {
    pub fn builtin(kind: TransitionKind) -> Result<Self, Error> {
        match kind {
            TransitionKind::Cut => Ok(Self::Cut),
            TransitionKind::Fade => Ok(Self::Fade),
            _ => Err(Error::Invalid(
                "configured transitions require a named preset reference",
            )),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SelectionData")]
pub struct TransitionSelection {
    reference: TransitionRef,
    duration_ms: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionFields {
    reference: TransitionRef,
    duration_ms: u32,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum SelectionData {
    Reference(SelectionFields),
    Legacy(TransitionConfig),
}
impl TryFrom<SelectionData> for TransitionSelection {
    type Error = Error;
    fn try_from(data: SelectionData) -> Result<Self, Error> {
        match data {
            SelectionData::Reference(data) => Self::new(data.reference, data.duration_ms),
            SelectionData::Legacy(config) => {
                Self::new(TransitionRef::builtin(config.kind())?, config.duration_ms())
            }
        }
    }
}
impl Default for TransitionSelection {
    fn default() -> Self {
        Self {
            reference: TransitionRef::Fade,
            duration_ms: 300,
        }
    }
}
impl TransitionSelection {
    pub fn new(reference: TransitionRef, duration_ms: u32) -> Result<Self, Error> {
        TransitionConfig::try_new(TransitionKind::Fade, duration_ms)?;
        Ok(Self {
            reference,
            duration_ms,
        })
    }
    pub fn reference(&self) -> TransitionRef {
        self.reference
    }
    pub fn duration_ms(&self) -> u32 {
        self.duration_ms
    }
    pub fn duration_ns(&self) -> u64 {
        u64::from(self.duration_ms) * 1_000_000
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        Self::new(self.reference, self.duration_ms).map(|_| ())
    }
    pub(crate) fn legacy(kind: TransitionKind, duration_ms: u32) -> Result<Self, Error> {
        Self::new(TransitionRef::builtin(kind)?, duration_ms)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PresetData")]
pub struct TransitionPreset {
    id: TransitionPresetId,
    name: String,
    kind: TransitionKind,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetData {
    id: TransitionPresetId,
    name: String,
    kind: TransitionKind,
}
impl TryFrom<PresetData> for TransitionPreset {
    type Error = Error;
    fn try_from(data: PresetData) -> Result<Self, Error> {
        Self::new(data.id, data.name, data.kind)
    }
}
impl TransitionPreset {
    pub fn new(id: TransitionPresetId, name: String, kind: TransitionKind) -> Result<Self, Error> {
        kind.validate()?;
        validate_name(&name)?;
        if ["Cut", "Fade"].contains(&name.as_str()) {
            return Err(Error::Invalid(
                "transition preset name is reserved for a built-in transition",
            ));
        }
        if !matches!(
            kind,
            TransitionKind::Swipe { .. }
                | TransitionKind::Slide { .. }
                | TransitionKind::FadeToColor { .. }
                | TransitionKind::LumaWipe { .. }
        ) {
            return Err(Error::Invalid(
                "only supported configurable transition kinds can be named presets",
            ));
        }
        // A short name can arrive in a String backed by an arbitrarily large
        // allocation. Strip spare capacity before authored snapshots retain it.
        let name = name.into_boxed_str().into_string();
        Ok(Self { id, name, kind })
    }
    pub fn id(&self) -> TransitionPresetId {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn kind(&self) -> TransitionKind {
        self.kind
    }
}
pub(crate) fn next_default() -> u32 {
    1
}
pub(crate) fn validate(presets: &[TransitionPreset], next: u32) -> Result<(), Error> {
    if presets.len() > MAX_TRANSITION_PRESETS {
        return Err(Error::Limit("transition preset catalog limit exceeded"));
    }
    if next == 0 || next > PRESET_ID_SENTINEL {
        return Err(Error::Invalid(
            "invalid transition preset next identity counter",
        ));
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for preset in presets {
        TransitionPreset::new(preset.id, preset.name.clone(), preset.kind)?;
        if preset.id.get() >= next || !ids.insert(preset.id) || !names.insert(preset.name.as_str())
        {
            return Err(Error::Invalid(
                "transition preset IDs/names must be unique and IDs below the next counter",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, QuickTransitionId, Session, TransitionDirection};
    use std::sync::Arc;
    fn swipe() -> TransitionKind {
        TransitionKind::Swipe {
            direction: TransitionDirection::Left,
            swipe_in: false,
        }
    }
    fn slide() -> TransitionKind {
        TransitionKind::Slide {
            direction: TransitionDirection::Down,
        }
    }
    fn add(session: &mut Session, name: &str) -> TransitionPresetId {
        session
            .apply(Command::AddTransitionPreset {
                name: name.into(),
                kind: swipe(),
            })
            .unwrap()
            .created_transition_preset
            .unwrap()
    }
    #[test]
    fn preset_names_strip_spare_capacity_before_current_and_history_retention() {
        let mut name = String::with_capacity(4096);
        name.push_str("Original");
        assert!(name.capacity() > name.len());
        let mut session = Session::new();
        let id = session
            .apply(Command::AddTransitionPreset {
                name,
                kind: swipe(),
            })
            .unwrap()
            .created_transition_preset
            .unwrap();
        let before = session.snapshot();
        let preset = before.transition_preset(id).unwrap();
        assert_eq!(preset.name(), "Original");
        assert_eq!(preset.name.capacity(), preset.name.len());
        let mut name = String::with_capacity(8192);
        name.push_str("Renamed");
        assert!(name.capacity() > name.len());
        session
            .apply(Command::ConfigureTransitionPreset {
                id,
                name,
                kind: slide(),
            })
            .unwrap();
        let after = session.snapshot();
        let preset = after.transition_preset(id).unwrap();
        assert_eq!(preset.name(), "Renamed");
        assert_eq!(preset.name.capacity(), preset.name.len());
        assert_eq!(
            before.transition_preset(id).unwrap().name.capacity(),
            "Original".len()
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().transition_preset(id).unwrap().name(),
            "Original"
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session.snapshot().transition_preset(id).unwrap().name(),
            "Renamed"
        );
    }
    #[test]
    fn shared_kind_edit_and_atomic_properties_apply_update_all_refs_with_one_revision() {
        let mut session = Session::new();
        let scene = session.preview();
        let id = add(&mut session, "Shared");
        let selection = TransitionSelection::new(TransitionRef::Preset(id), 750).unwrap();
        session
            .apply(Command::SelectTransition { selection })
            .unwrap();
        session
            .apply(Command::AddQuickTransitionSelection {
                selection,
                fade_to_black: false,
            })
            .unwrap();
        session
            .apply(Command::ConfigureSceneTransitionSelection {
                scene,
                reference: Some(TransitionRef::Preset(id)),
                duration_ms: 1250,
            })
            .unwrap();
        let before = session.snapshot();
        let change = session
            .apply(Command::ConfigureTransitionPreset {
                id,
                name: "Renamed".into(),
                kind: slide(),
            })
            .unwrap();
        assert_eq!(change.revision, before.revision() + 1);
        let current = session.snapshot();
        assert_eq!(current.transition_preset(id).unwrap().name(), "Renamed");
        for selection in [
            current.transition(),
            current.quick_transitions().last().unwrap().config(),
            current.scene(scene).unwrap().transition_override().unwrap(),
        ] {
            assert_eq!(
                current.resolve_transition(selection).unwrap().kind(),
                slide()
            );
            assert_eq!(selection.reference(), TransitionRef::Preset(id));
        }
        assert_eq!(current.scene(scene).unwrap().transition_duration_ms(), 1250);
        assert_eq!(
            current
                .resolve_transition(current.transition())
                .unwrap()
                .duration_ms(),
            750
        );
        assert_eq!(
            before
                .resolve_transition(before.transition())
                .unwrap()
                .kind(),
            swipe()
        );
        assert!(
            !session
                .apply(Command::ConfigureTransitionPreset {
                    id,
                    name: "Renamed".into(),
                    kind: slide()
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&current, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().transition_preset(id).unwrap().name(),
            "Shared"
        );
        assert_eq!(
            session
                .snapshot()
                .resolve_transition(session.snapshot().transition())
                .unwrap()
                .kind(),
            swipe()
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session.snapshot().transition_preset(id).unwrap().kind(),
            slide()
        );
    }
    #[test]
    fn removal_rejects_each_reference_owner_and_keeps_inactive_duration_without_dangling_ref() {
        let mut session = Session::new();
        let scene = session.preview();
        let id = add(&mut session, "P");
        let reference = TransitionRef::Preset(id);
        let selection = TransitionSelection::new(reference, 600).unwrap();
        session
            .apply(Command::SelectTransition { selection })
            .unwrap();
        let before = session.snapshot();
        assert_eq!(
            session.apply(Command::RemoveTransitionPreset { id }),
            Err(Error::TransitionPresetReferenced(id))
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        session
            .apply(Command::SetTransition {
                kind: TransitionKind::Fade,
                duration_ms: 300,
            })
            .unwrap();
        session
            .apply(Command::AddQuickTransitionSelection {
                selection,
                fade_to_black: true,
            })
            .unwrap();
        let row = session.snapshot().quick_transitions().last().unwrap().id();
        assert_eq!(
            session.apply(Command::RemoveTransitionPreset { id }),
            Err(Error::TransitionPresetReferenced(id))
        );
        session
            .apply(Command::RemoveQuickTransition { id: row })
            .unwrap();
        session
            .apply(Command::ConfigureSceneTransitionSelection {
                scene,
                reference: Some(reference),
                duration_ms: 750,
            })
            .unwrap();
        assert_eq!(
            session.apply(Command::RemoveTransitionPreset { id }),
            Err(Error::TransitionPresetReferenced(id))
        );
        session
            .apply(Command::ConfigureSceneTransitionSelection {
                scene,
                reference: None,
                duration_ms: 750,
            })
            .unwrap();
        session
            .apply(Command::RemoveTransitionPreset { id })
            .unwrap();
        assert_eq!(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_duration_ms(),
            750
        );
        assert!(session.snapshot().transition_preset(id).is_none());
    }
    #[test]
    fn rejected_catalog_edits_and_dangling_selections_preserve_arc_and_history() {
        let mut session = Session::new();
        let id = add(&mut session, "P");
        let other = add(&mut session, "Q");
        let before = session.snapshot();
        let undo = session.can_undo();
        let missing = TransitionPresetId::new(999).unwrap();
        let selection = TransitionSelection::new(TransitionRef::Preset(missing), 300).unwrap();
        for command in [
            Command::AddTransitionPreset {
                name: "Cut".into(),
                kind: swipe(),
            },
            Command::AddTransitionPreset {
                name: "P".into(),
                kind: slide(),
            },
            Command::AddTransitionPreset {
                name: "X".into(),
                kind: TransitionKind::Fade,
            },
            Command::ConfigureTransitionPreset {
                id: other,
                name: "P".into(),
                kind: slide(),
            },
            Command::RenameTransitionPreset {
                id,
                name: "Fade".into(),
            },
            Command::SetTransitionPresetKind {
                id,
                kind: TransitionKind::Cut,
            },
            Command::SelectTransition { selection },
            Command::AddQuickTransitionSelection {
                selection,
                fade_to_black: false,
            },
            Command::ConfigureSceneTransitionSelection {
                scene: session.preview(),
                reference: Some(selection.reference()),
                duration_ms: 500,
            },
            Command::RemoveTransitionPreset { id: missing },
        ] {
            assert!(session.apply(command).is_err());
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
            assert_eq!(session.can_undo(), undo);
        }
        for command in [
            Command::SetTransition {
                kind: swipe(),
                duration_ms: 300,
            },
            Command::AddQuickTransition {
                kind: slide(),
                duration_ms: 300,
                fade_to_black: false,
            },
            Command::UpdateQuickTransition {
                id: QuickTransitionId::new(1).unwrap(),
                kind: swipe(),
                duration_ms: 300,
                fade_to_black: false,
            },
            Command::ConfigureSceneTransition {
                scene: session.preview(),
                kind: Some(slide()),
                duration_ms: 300,
            },
            Command::SetSceneTransition {
                scene: session.preview(),
                config: Some(TransitionConfig::new(swipe(), 300).unwrap()),
            },
        ] {
            assert!(session.apply(command).is_err());
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
    }
    #[test]
    fn preset_ids_survive_undo_delete_and_exhaustion_without_reuse() {
        let mut session = Session::new();
        let first = add(&mut session, "First");
        session.apply(Command::Undo).unwrap();
        let second = add(&mut session, "Second");
        assert!(second.get() > first.get());
        session
            .apply(Command::RemoveTransitionPreset { id: second })
            .unwrap();
        let third = add(&mut session, "Third");
        assert!(third.get() > second.get());
        let mut collection = (*session.snapshot()).clone();
        collection.next_transition_preset_id = PRESET_ID_SENTINEL;
        let mut exhausted = Session::from_snapshot(collection, true).unwrap();
        let before = exhausted.snapshot();
        assert_eq!(
            exhausted.apply(Command::AddTransitionPreset {
                name: "X".into(),
                kind: swipe()
            }),
            Err(Error::IdentityExhausted)
        );
        assert!(Arc::ptr_eq(&before, &exhausted.snapshot()));
        assert!(TransitionPresetId::new(0).is_err());
        assert!(TransitionPresetId::new(PRESET_ID_SENTINEL).is_err());
    }
    #[test]
    fn catalog_admission_is_bounded_and_missing_or_duplicate_presets_do_not_validate() {
        let mut session = Session::new();
        for index in 0..MAX_TRANSITION_PRESETS {
            add(&mut session, &format!("Preset {index}"));
        }
        let before = session.snapshot();
        assert!(matches!(
            session.apply(Command::AddTransitionPreset {
                name: "Overflow".into(),
                kind: slide()
            }),
            Err(Error::Limit(_))
        ));
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        let mut invalid = (*before).clone();
        let duplicate = invalid.transition_presets[0].clone();
        invalid.transition_presets.push(duplicate);
        assert!(invalid.validate().is_err());
        let mut invalid = (*before).clone();
        invalid.transition = TransitionSelection::new(
            TransitionRef::Preset(TransitionPresetId::new(999).unwrap()),
            300,
        )
        .unwrap();
        assert!(invalid.validate().is_err());
        for duration in [0, 49, 20001, u32::MAX] {
            assert!(TransitionSelection::new(TransitionRef::Fade, duration).is_err());
        }
    }
    #[test]
    fn compound_remove_cleans_exact_refs_preserves_durations_and_is_one_undo_step() {
        let mut session = Session::new();
        let first = session.preview();
        let second = session
            .apply(Command::AddScene {
                name: "Second".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let third_scene = session
            .apply(Command::AddScene {
                name: "Third".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let removed = add(&mut session, "Remove");
        let other = add(&mut session, "Other");
        let last = add(&mut session, "Last");
        session
            .apply(Command::SelectTransition {
                selection: TransitionSelection::new(TransitionRef::Preset(removed), 750).unwrap(),
            })
            .unwrap();
        session
            .apply(Command::ConfigureSceneTransitionSelection {
                scene: first,
                reference: Some(TransitionRef::Preset(removed)),
                duration_ms: 1250,
            })
            .unwrap();
        session
            .apply(Command::ConfigureSceneTransitionSelection {
                scene: second,
                reference: Some(TransitionRef::Preset(other)),
                duration_ms: 650,
            })
            .unwrap();
        session
            .apply(Command::ConfigureSceneTransitionSelection {
                scene: third_scene,
                reference: None,
                duration_ms: 900,
            })
            .unwrap();
        for fade_to_black in [false, true] {
            session
                .apply(Command::AddQuickTransitionSelection {
                    selection: TransitionSelection::new(TransitionRef::Preset(removed), 500)
                        .unwrap(),
                    fade_to_black,
                })
                .unwrap();
        }
        session
            .apply(Command::AddQuickTransitionSelection {
                selection: TransitionSelection::new(TransitionRef::Preset(other), 800).unwrap(),
                fade_to_black: false,
            })
            .unwrap();
        let baseline = session.snapshot();
        let expected_rows: Vec<_> = baseline
            .quick_transitions()
            .iter()
            .filter(|row| row.config().reference() != TransitionRef::Preset(removed))
            .cloned()
            .collect();
        assert_eq!(
            baseline.transition_after_removing_preset(removed).unwrap(),
            TransitionRef::Preset(last)
        );
        assert_eq!(
            session
                .apply(Command::RemoveTransitionPreset { id: removed })
                .unwrap_err(),
            Error::TransitionPresetReferenced(removed)
        );
        assert!(Arc::ptr_eq(&baseline, &session.snapshot()));
        let change = session
            .apply(Command::RemoveTransitionPresetAndReferences { id: removed })
            .unwrap();
        assert_eq!(change.revision, baseline.revision() + 1);
        let cleaned = session.snapshot();
        assert!(cleaned.transition_preset(removed).is_none());
        assert_eq!(
            cleaned.transition().reference(),
            TransitionRef::Preset(last)
        );
        assert_eq!(cleaned.transition().duration_ms(), 750);
        assert_eq!(cleaned.quick_transitions(), expected_rows);
        assert_eq!(cleaned.scene(first).unwrap().transition_override(), None);
        assert_eq!(cleaned.scene(first).unwrap().transition_duration_ms(), 1250);
        assert_eq!(cleaned.scene(second), baseline.scene(second));
        assert_eq!(cleaned.scene(third_scene), baseline.scene(third_scene));
        assert_eq!(cleaned.next_quick_id, baseline.next_quick_id);
        assert_eq!(
            cleaned.next_transition_preset_id,
            baseline.next_transition_preset_id
        );
        session.apply(Command::Undo).unwrap();
        let mut expected = (*baseline).clone();
        expected.revision = session.snapshot().revision();
        assert_eq!(*session.snapshot(), expected);
        session.apply(Command::Redo).unwrap();
        let mut expected = (*cleaned).clone();
        expected.revision = session.snapshot().revision();
        assert_eq!(*session.snapshot(), expected);
    }
    #[test]
    fn compound_remove_always_selects_last_remaining_then_builtin_fade_without_reusing_ids() {
        let mut session = Session::new();
        let first = add(&mut session, "First");
        let second = add(&mut session, "Second");
        let last = add(&mut session, "Last");
        session
            .apply(Command::SetTransition {
                kind: TransitionKind::Cut,
                duration_ms: 600,
            })
            .unwrap();
        session
            .apply(Command::RemoveTransitionPresetAndReferences { id: first })
            .unwrap();
        assert_eq!(
            session.snapshot().transition().reference(),
            TransitionRef::Preset(last)
        );
        session
            .apply(Command::RemoveTransitionPresetAndReferences { id: last })
            .unwrap();
        assert_eq!(
            session.snapshot().transition().reference(),
            TransitionRef::Preset(second)
        );
        session
            .apply(Command::RemoveTransitionPresetAndReferences { id: second })
            .unwrap();
        assert_eq!(
            session.snapshot().transition().reference(),
            TransitionRef::Fade
        );
        assert_eq!(session.snapshot().transition().duration_ms(), 600);
        assert!(add(&mut session, "After").get() > last.get());
        let baseline = session.snapshot();
        let missing = TransitionPresetId::new(999).unwrap();
        assert_eq!(
            baseline
                .transition_after_removing_preset(missing)
                .unwrap_err(),
            Error::TransitionPresetNotFound(missing)
        );
        assert_eq!(
            session
                .apply(Command::RemoveTransitionPresetAndReferences { id: missing })
                .unwrap_err(),
            Error::TransitionPresetNotFound(missing)
        );
        assert!(Arc::ptr_eq(&baseline, &session.snapshot()));
    }
    #[test]
    fn fade_to_color_kind_validates_switch_point_before_adding_or_reconfiguring_preset() {
        let mut session = Session::new();
        let id = add(&mut session, "P");
        let before = session.snapshot();
        for switch_point in [101, 255] {
            let kind = TransitionKind::FadeToColor {
                rgb: [255, 128, 0],
                switch_point,
            };
            assert!(
                session
                    .apply(Command::AddTransitionPreset {
                        name: "Invalid".into(),
                        kind
                    })
                    .is_err()
            );
            assert!(
                session
                    .apply(Command::ConfigureTransitionPreset {
                        id,
                        name: "Changed".into(),
                        kind
                    })
                    .is_err()
            );
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
        for switch_point in [0, 50, 100] {
            let kind = TransitionKind::FadeToColor {
                rgb: [255, 128, 0],
                switch_point,
            };
            session
                .apply(Command::ConfigureTransitionPreset {
                    id,
                    name: "P".into(),
                    kind,
                })
                .unwrap();
            assert_eq!(
                session.snapshot().transition_preset(id).unwrap().kind(),
                kind
            );
        }
    }
}
