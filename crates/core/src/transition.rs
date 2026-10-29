//! Authored transition choice/duration. Runtime progress and output ownership
//! belong to the host; configuration changes use the normal command history.
use crate::Error;
use serde::{Deserialize, Serialize};

pub const MIN_TRANSITION_DURATION_MS: u32 = 50;
pub const MAX_TRANSITION_DURATION_MS: u32 = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TransitionKind {
    Cut,
    Fade,
    Swipe {
        direction: TransitionDirection,
        swipe_in: bool,
    },
    Slide {
        direction: TransitionDirection,
    },
    FadeToColor {
        rgb: [u8; 3],
        switch_point: u8,
    },
    LumaWipe {
        mask:crate::LumaWipePattern,
        softness:crate::LumaWipeSoftness,
        invert:bool,
    },
}
impl TransitionKind {
    /// Original OBS Fade to Color defaults: forced opaque black, peak50%.
    pub fn fade_to_color()->Self{Self::FadeToColor{rgb:[0;3],switch_point:50}}
    pub fn luma_wipe()->Self{Self::LumaWipe{mask:Default::default(),softness:Default::default(),invert:false}}
    pub fn label(&self) -> &'static str {
        match self {
            Self::Cut => "Cut",
            Self::Fade => "Fade",
            Self::Swipe { .. } => "Swipe",
            Self::Slide { .. } => "Slide",
            Self::FadeToColor { .. } => "Fade to Color",
            Self::LumaWipe { .. } => "Luma Wipe",
        }
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if matches!(self,Self::FadeToColor{switch_point,..} if *switch_point>100) {
            return Err(Error::Invalid(
                "fade-to-color switch point must be in 0..=100 percent",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "TransitionData")]
pub struct TransitionConfig {
    kind: TransitionKind,
    duration_ms: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionData {
    kind: TransitionKind,
    duration_ms: u32,
}
impl TryFrom<TransitionData> for TransitionConfig {
    type Error = Error;
    fn try_from(data: TransitionData) -> Result<Self, Error> {
        Self::try_new(data.kind, data.duration_ms)
    }
}
impl Default for TransitionConfig {
    fn default() -> Self {
        Self {
            kind: TransitionKind::Fade,
            duration_ms: 300,
        }
    }
}
impl TransitionConfig {
    pub fn try_new(kind: TransitionKind, duration_ms: u32) -> Result<Self, Error> {
        let config = Self { kind, duration_ms };
        config.validate()?;
        Ok(config)
    }
    pub fn new(kind: TransitionKind, duration_ms: u32) -> Result<Self, Error> {
        Self::try_new(kind, duration_ms)
    }
    pub fn kind(&self) -> TransitionKind {
        self.kind
    }
    pub fn duration_ms(&self) -> u32 {
        self.duration_ms
    }
    pub fn duration_ns(&self) -> u64 {
        u64::from(self.duration_ms) * 1_000_000
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        self.kind.validate()?;
        if !(MIN_TRANSITION_DURATION_MS..=MAX_TRANSITION_DURATION_MS).contains(&self.duration_ms) {
            return Err(Error::Invalid(
                "transition duration must be in 50..=20000 milliseconds",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, Id, Session, TransitionRef, TransitionSelection};
    use std::sync::Arc;

    #[test]
    fn fade_to_color_kind_validates_percentage_before_resolved_config_publication() {
        for switch_point in [0, 1, 50, 99, 100] {
            let kind = TransitionKind::FadeToColor {
                rgb: [17, 89, 203],
                switch_point,
            };
            assert_eq!(kind.label(), "Fade to Color");
            assert!(kind.validate().is_ok());
            let config = TransitionConfig::new(kind, 750).unwrap();
            assert_eq!(config.kind(), kind);
        }
        for switch_point in [101, 255] {
            let kind = TransitionKind::FadeToColor {
                rgb: [0; 3],
                switch_point,
            };
            assert!(kind.validate().is_err());
            assert!(TransitionConfig::new(kind, 750).is_err());
        }
    }

    #[test]
    fn validated_config_has_explicit_units_default_and_inclusive_limits() {
        let default = TransitionConfig::default();
        assert_eq!(
            (default.kind(), default.duration_ms(), default.duration_ns()),
            (TransitionKind::Fade, 300, 300_000_000)
        );
        for kind in [TransitionKind::Cut, TransitionKind::Fade] {
            for duration_ms in [50, 20_000] {
                assert_eq!(
                    TransitionConfig::try_new(kind, duration_ms)
                        .unwrap()
                        .duration_ms(),
                    duration_ms
                );
            }
            for duration_ms in [0, 49, 20_001, u32::MAX] {
                assert!(TransitionConfig::try_new(kind, duration_ms).is_err());
            }
        }
    }

    #[test]
    fn transition_commands_preserve_invalid_and_no_op_snapshots_then_undo_redo() {
        let mut session = Session::new();
        let baseline = session.snapshot();
        assert!(
            !session
                .apply(Command::SetTransition {
                    kind: TransitionKind::Fade,
                    duration_ms: 300
                })
                .unwrap()
                .changed
        );
        for duration_ms in [0, 49, 20_001, u32::MAX] {
            assert!(
                session
                    .apply(Command::SetTransition {
                        kind: TransitionKind::Cut,
                        duration_ms
                    })
                    .is_err()
            );
            assert!(Arc::ptr_eq(&baseline, &session.snapshot()));
            assert!(!session.can_undo());
        }
        let change = session
            .apply(Command::SetTransition {
                kind: TransitionKind::Cut,
                duration_ms: 750,
            })
            .unwrap();
        assert!(change.changed);
        assert_eq!(
            session.snapshot().transition().reference(),
            TransitionRef::Cut
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            *session.snapshot().transition(),
            TransitionSelection::default()
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(session.snapshot().transition().duration_ms(), 750);
        assert_eq!(session.snapshot().revision(), 3);
    }

    #[test]
    fn scene_override_is_one_validated_edit_with_no_op_undo_and_duplicate_semantics() {
        let mut session = Session::new();
        let scene = session.preview();
        let initial = session.snapshot();
        assert_eq!(initial.scene(scene).unwrap().transition_override(), None);
        assert!(
            !session
                .apply(Command::SetSceneTransition {
                    scene,
                    config: None
                })
                .unwrap()
                .changed
        );
        let invalid = TransitionConfig {
            kind: TransitionKind::Fade,
            duration_ms: 49,
        };
        assert!(
            session
                .apply(Command::SetSceneTransition {
                    scene,
                    config: Some(invalid)
                })
                .is_err()
        );
        assert!(
            session
                .apply(Command::SetSceneTransition {
                    scene: Id::new(u64::MAX),
                    config: None
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&initial, &session.snapshot()));
        assert!(!session.can_undo());
        let config = TransitionConfig::try_new(TransitionKind::Cut, 500).unwrap();
        session
            .apply(Command::SetSceneTransition {
                scene,
                config: Some(config),
            })
            .unwrap();
        let configured = session.snapshot();
        assert_eq!(
            configured.scene(scene).unwrap().transition_override(),
            Some(&TransitionSelection::legacy(config.kind(), config.duration_ms()).unwrap())
        );
        assert_eq!(*configured.transition(), TransitionSelection::default());
        assert!(
            !session
                .apply(Command::SetSceneTransition {
                    scene,
                    config: Some(config)
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&configured, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override(),
            None
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override(),
            Some(&TransitionSelection::legacy(config.kind(), config.duration_ms()).unwrap())
        );
        let duplicate = session
            .apply(Command::DuplicateScene {
                scene,
                name: "Copy".into(),
            })
            .unwrap()
            .created
            .unwrap();
        assert_eq!(
            session
                .snapshot()
                .scene(duplicate)
                .unwrap()
                .transition_override(),
            Some(&TransitionSelection::legacy(config.kind(), config.duration_ms()).unwrap())
        );
        session
            .apply(Command::SetSceneTransition {
                scene: duplicate,
                config: None,
            })
            .unwrap();
        assert_eq!(
            session
                .snapshot()
                .scene(duplicate)
                .unwrap()
                .transition_override(),
            None
        );
        assert_eq!(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override(),
            Some(&TransitionSelection::legacy(config.kind(), config.duration_ms()).unwrap())
        );
    }

    #[test]
    fn disabling_and_reenabling_an_override_remembers_its_single_duration_owner() {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::SetSceneTransition {
                scene,
                config: Some(TransitionConfig::try_new(TransitionKind::Fade, 1250).unwrap()),
            })
            .unwrap();
        session
            .apply(Command::SetSceneTransition {
                scene,
                config: None,
            })
            .unwrap();
        let disabled = session.snapshot();
        let target = disabled.scene(scene).unwrap();
        assert_eq!(target.transition_override(), None);
        assert_eq!(target.transition_duration_ms(), 1250);
        assert_eq!(target.inactive_transition_duration_ms, Some(1250));
        assert!(
            !session
                .apply(Command::SetSceneTransition {
                    scene,
                    config: None
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&disabled, &session.snapshot()));
        let copy = session
            .apply(Command::DuplicateScene {
                scene,
                name: "Disabled copy".into(),
            })
            .unwrap()
            .created
            .unwrap();
        assert_eq!(
            session
                .snapshot()
                .scene(copy)
                .unwrap()
                .transition_duration_ms(),
            1250
        );
        assert_eq!(
            session
                .snapshot()
                .scene(copy)
                .unwrap()
                .transition_override(),
            None
        );
        session
            .apply(Command::ConfigureSceneTransition {
                scene,
                kind: Some(TransitionKind::Cut),
                duration_ms: 1250,
            })
            .unwrap();
        assert_eq!(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override()
                .unwrap()
                .reference(),
            TransitionRef::Cut
        );
        assert_eq!(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .inactive_transition_duration_ms,
            None
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override(),
            None
        );
        assert_eq!(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_duration_ms(),
            1250
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_duration_ms(),
            1250
        );
    }

    #[test]
    fn inactive_configuration_is_atomic_undoable_and_semantically_no_op() {
        let mut session = Session::new();
        let scene = session.preview();
        let initial = session.snapshot();
        for duration_ms in [0, 49, 20_001, u32::MAX] {
            assert!(
                session
                    .apply(Command::ConfigureSceneTransition {
                        scene,
                        kind: None,
                        duration_ms
                    })
                    .is_err()
            );
            assert!(Arc::ptr_eq(&initial, &session.snapshot()));
            assert!(!session.can_undo());
        }
        session
            .apply(Command::ConfigureSceneTransition {
                scene,
                kind: None,
                duration_ms: 450,
            })
            .unwrap();
        let configured = session.snapshot();
        assert_eq!(configured.revision(), 1);
        assert_eq!(configured.scene(scene).unwrap().transition_override(), None);
        assert_eq!(
            configured.scene(scene).unwrap().transition_duration_ms(),
            450
        );
        assert!(
            !session
                .apply(Command::ConfigureSceneTransition {
                    scene,
                    kind: None,
                    duration_ms: 450
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&configured, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_duration_ms(),
            300
        );
        assert!(!session.can_undo());
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_duration_ms(),
            450
        );
        let mut native = crate::StudioSnapshot::default();
        native.scenes[0].inactive_transition_duration_ms = Some(300);
        let mut normalized = Session::from_snapshot(native, true).unwrap();
        let before = normalized.snapshot();
        assert!(
            !normalized
                .apply(Command::ConfigureSceneTransition {
                    scene: normalized.preview(),
                    kind: None,
                    duration_ms: 300
                })
                .unwrap()
                .changed
        );
        assert!(
            !normalized
                .apply(Command::SetSceneTransition {
                    scene: normalized.preview(),
                    config: None
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&before, &normalized.snapshot()));
        assert!(!normalized.can_undo());
    }

    #[test]
    fn inactive_duration_validation_rejects_dual_owners_and_invalid_ranges() {
        let mut collection = crate::StudioSnapshot::default();
        collection.scenes[0].transition_override =
            Some(TransitionSelection::legacy(TransitionKind::Fade, 1250).unwrap());
        assert!(
            collection.validate().is_ok(),
            "older native active configs need no new metadata"
        );
        collection.scenes[0].inactive_transition_duration_ms = Some(1250);
        assert!(collection.validate().is_err());
        collection.scenes[0].transition_override = None;
        for duration_ms in [0, 49, 20_001, u32::MAX] {
            collection.scenes[0].inactive_transition_duration_ms = Some(duration_ms);
            assert!(collection.validate().is_err());
        }
        for duration_ms in [50, 300, 20_000] {
            collection.scenes[0].inactive_transition_duration_ms = Some(duration_ms);
            assert!(collection.validate().is_ok());
        }
    }

    #[test]
    fn program_selection_is_validated_runtime_state_without_history_or_preview_changes() {
        let mut session = Session::new();
        let first = session.preview();
        let second = session
            .apply(Command::AddScene {
                name: "Second".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let baseline = session.snapshot();
        let dirty = session.is_dirty();
        session.select_program(second).unwrap();
        assert_eq!(session.program(), second);
        assert_eq!(session.preview(), first);
        assert!(Arc::ptr_eq(&baseline, &session.snapshot()));
        assert_eq!(session.is_dirty(), dirty);
        assert_eq!(
            session.select_program(Id::new(u64::MAX)),
            Err(Error::NotFound(Id::new(u64::MAX)))
        );
        assert_eq!(session.program(), second);
        session.close();
        assert_eq!(session.select_program(first), Err(Error::Closed));
        assert_eq!(session.program(), second);
        assert!(Arc::ptr_eq(&baseline, &session.snapshot()));
        let mut history = Session::new();
        let initial = history.preview();
        let created = history
            .apply(Command::AddScene {
                name: "Output".into(),
            })
            .unwrap()
            .created
            .unwrap();
        history.select_program(created).unwrap();
        history.apply(Command::Undo).unwrap();
        assert_eq!(history.snapshot().scenes().len(), 1);
        assert_eq!(history.program(), initial);
        assert_eq!(
            history.snapshot().revision(),
            2,
            "runtime output selection must not insert an undo step"
        );
    }
}
