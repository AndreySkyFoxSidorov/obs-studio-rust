//! Authored Quick Transition rows. Runtime actions never live in this list.
use crate::{Error, TransitionKind, TransitionSelection};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;

pub const MAX_QUICK_TRANSITIONS: usize = 64;
pub(crate) const QUICK_ID_SENTINEL: u32 = i32::MAX as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct QuickTransitionId(u32);
impl QuickTransitionId {
    pub fn new(value: u32) -> Result<Self, Error> {
        if value == 0 || value >= QUICK_ID_SENTINEL {
            return Err(Error::Invalid(
                "quick transition ID must be in 1..=2147483646",
            ));
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u32 {
        self.0
    }
}
impl<'de> Deserialize<'de> for QuickTransitionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(u32::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "QuickData")]
pub struct QuickTransition {
    id: QuickTransitionId,
    config: TransitionSelection,
    fade_to_black: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuickData {
    id: QuickTransitionId,
    config: TransitionSelection,
    fade_to_black: bool,
}
impl TryFrom<QuickData> for QuickTransition {
    type Error = Error;
    fn try_from(value: QuickData) -> Result<Self, Error> {
        Self::from_selection(value.id, value.config, value.fade_to_black)
    }
}
impl QuickTransition {
    pub fn try_new(
        id: QuickTransitionId,
        kind: TransitionKind,
        duration_ms: u32,
        fade_to_black: bool,
    ) -> Result<Self, Error> {
        Ok(Self {
            id,
            config: TransitionSelection::legacy(kind, duration_ms)?,
            fade_to_black,
        })
    }
    pub fn from_selection(
        id: QuickTransitionId,
        config: TransitionSelection,
        fade_to_black: bool,
    ) -> Result<Self, Error> {
        config.validate()?;
        Ok(Self {
            id,
            config,
            fade_to_black,
        })
    }
    pub fn id(&self) -> QuickTransitionId {
        self.id
    }
    pub fn config(&self) -> &TransitionSelection {
        &self.config
    }
    pub fn fade_to_black(&self) -> bool {
        self.fade_to_black
    }
}

pub(crate) fn defaults() -> Vec<QuickTransition> {
    vec![
        QuickTransition {
            id: QuickTransitionId(1),
            config: TransitionSelection::legacy(TransitionKind::Cut, 300)
                .expect("constant duration"),
            fade_to_black: false,
        },
        QuickTransition {
            id: QuickTransitionId(2),
            config: TransitionSelection::default(),
            fade_to_black: false,
        },
        QuickTransition {
            id: QuickTransitionId(3),
            config: TransitionSelection::default(),
            fade_to_black: true,
        },
    ]
}
pub(crate) fn next_default() -> u32 {
    4
}
pub(crate) fn validate(rows: &[QuickTransition], next: u32) -> Result<(), Error> {
    if rows.len() > MAX_QUICK_TRANSITIONS {
        return Err(Error::Limit("quick transition row limit exceeded"));
    }
    if next == 0 || next > QUICK_ID_SENTINEL {
        return Err(Error::Invalid(
            "quick transition counter must be positive and at most its exhausted sentinel",
        ));
    }
    let mut seen = HashSet::new();
    for row in rows {
        row.config.validate()?;
        if row.id.get() >= next || !seen.insert(row.id) {
            return Err(Error::Invalid(
                "quick transition IDs must be unique and below the next issued counter",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, Session};
    use std::sync::Arc;
    fn add(session: &mut Session) -> QuickTransitionId {
        session
            .apply(Command::AddQuickTransition {
                kind: TransitionKind::Fade,
                duration_ms: 500,
                fade_to_black: false,
            })
            .unwrap();
        session.snapshot().quick_transitions().last().unwrap().id()
    }
    #[test]
    fn empty_imported_rows_cannot_hide_a_zero_next_counter() {
        // These are the exact authored fields of a decoded native document
        // with explicit quick_transitions:[] and next_quick_id:0. Empty rows
        // must not skip the aggregate's automatic-ID validity requirement.
        let mut malformed = crate::StudioSnapshot::default();
        malformed.quick_transitions.clear();
        malformed.next_quick_id = 0;
        assert!(matches!(malformed.validate(), Err(Error::Invalid(_))));
        assert!(matches!(
            Session::from_snapshot(malformed, true),
            Err(Error::Invalid(_))
        ));
        let mut valid = crate::StudioSnapshot::default();
        valid.quick_transitions.clear();
        let mut session = Session::from_snapshot(valid, true).unwrap();
        assert_eq!(add(&mut session).get(), 4);
    }
    #[test]
    fn replacement_rows_strip_spare_capacity_before_snapshots_and_history() {
        let mut session = Session::new();
        let original = session.snapshot().quick_transitions()[0].clone();
        let mut rows = Vec::with_capacity(256);
        rows.push(original.clone());
        assert!(rows.capacity() > rows.len());
        session
            .apply(Command::ReplaceQuickTransitions { rows })
            .unwrap();
        let retained = session.snapshot();
        assert_eq!(retained.quick_transitions(), &[original]);
        assert_eq!(retained.quick_transitions.capacity(), 1);
        session
            .apply(Command::RenameCollection {
                name: "Next history entry".into(),
            })
            .unwrap();
        assert_eq!(retained.quick_transitions.capacity(), 1);
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().quick_transitions.capacity(), 1);
        session.apply(Command::Redo).unwrap();
        assert_eq!(session.snapshot().quick_transitions.capacity(), 1);
    }
    #[test]
    fn defaults_and_typed_ids_are_valid_without_aliasing_scene_identity() {
        assert!(QuickTransitionId::new(0).is_err());
        assert_eq!(QuickTransitionId::new(1).unwrap().get(), 1);
        assert!(QuickTransitionId::new(QUICK_ID_SENTINEL - 1).is_ok());
        assert!(QuickTransitionId::new(QUICK_ID_SENTINEL).is_err());
        let session = Session::new();
        let rows = session.snapshot();
        assert_eq!(rows.quick_transitions().len(), 3);
        assert_eq!(
            rows.quick_transitions()
                .iter()
                .map(|row| (
                    row.id().get(),
                    rows.resolve_transition(row.config()).unwrap().kind(),
                    row.config().duration_ms(),
                    row.fade_to_black()
                ))
                .collect::<Vec<_>>(),
            [
                (1, TransitionKind::Cut, 300, false),
                (2, TransitionKind::Fade, 300, false),
                (3, TransitionKind::Fade, 300, true)
            ]
        );
    }
    #[test]
    fn invalid_duplicate_missing_and_no_op_rows_preserve_owner_and_history() {
        let mut session = Session::new();
        let before = session.snapshot();
        let one = QuickTransitionId::new(1).unwrap();
        assert!(
            !session
                .apply(Command::UpdateQuickTransition {
                    id: one,
                    kind: TransitionKind::Cut,
                    duration_ms: 300,
                    fade_to_black: false
                })
                .unwrap()
                .changed
        );
        assert!(
            !session
                .apply(Command::MoveQuickTransition { id: one, index: 0 })
                .unwrap()
                .changed
        );
        let duplicate = before.quick_transitions()[0].clone();
        for command in [
            Command::AddQuickTransition {
                kind: TransitionKind::Fade,
                duration_ms: 0,
                fade_to_black: false,
            },
            Command::ReplaceQuickTransitions {
                rows: vec![duplicate.clone(), duplicate],
            },
            Command::MoveQuickTransition { id: one, index: 3 },
            Command::RemoveQuickTransition {
                id: QuickTransitionId::new(99).unwrap(),
            },
        ] {
            assert!(session.apply(command).is_err());
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
            assert!(!session.can_undo());
        }
    }
    #[test]
    fn automatic_quick_ids_survive_undo_redo_delete_and_explicit_replacement() {
        let mut session = Session::new();
        let first = add(&mut session);
        assert_eq!(first.get(), 4);
        session.apply(Command::Undo).unwrap();
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session.snapshot().quick_transitions().last().unwrap().id(),
            first
        );
        session.apply(Command::Undo).unwrap();
        let second = add(&mut session);
        assert!(second.get() > first.get());
        session
            .apply(Command::RemoveQuickTransition { id: second })
            .unwrap();
        session
            .apply(Command::ReplaceQuickTransitions {
                rows: vec![
                    QuickTransition::try_new(
                        QuickTransitionId::new(80).unwrap(),
                        TransitionKind::Cut,
                        300,
                        true,
                    )
                    .unwrap(),
                ],
            })
            .unwrap();
        assert_eq!(add(&mut session).get(), 81);
        session
            .apply(Command::ReplaceQuickTransitions { rows: vec![] })
            .unwrap();
        assert!(session.snapshot().quick_transitions().is_empty());
        assert_eq!(add(&mut session).get(), 82);
    }
    #[test]
    fn quick_move_update_and_exhaustion_are_atomic() {
        let mut session = Session::new();
        let three = QuickTransitionId::new(3).unwrap();
        session
            .apply(Command::MoveQuickTransition {
                id: three,
                index: 0,
            })
            .unwrap();
        assert_eq!(session.snapshot().quick_transitions()[0].id(), three);
        session
            .apply(Command::UpdateQuickTransition {
                id: three,
                kind: TransitionKind::Cut,
                duration_ms: 20000,
                fade_to_black: true,
            })
            .unwrap();
        assert_eq!(
            session.snapshot().quick_transitions()[0]
                .config()
                .duration_ms(),
            20000
        );
        let last = QuickTransition::try_new(
            QuickTransitionId::new(QUICK_ID_SENTINEL - 1).unwrap(),
            TransitionKind::Fade,
            300,
            false,
        )
        .unwrap();
        session
            .apply(Command::ReplaceQuickTransitions { rows: vec![last] })
            .unwrap();
        let before = session.snapshot();
        assert_eq!(
            session
                .apply(Command::AddQuickTransition {
                    kind: TransitionKind::Cut,
                    duration_ms: 300,
                    fade_to_black: false
                })
                .unwrap_err(),
            Error::IdentityExhausted
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        let mut fresh = Session::new();
        let row = fresh.snapshot().quick_transitions()[0].clone();
        let rows = (0..MAX_QUICK_TRANSITIONS + 1)
            .map(|_| row.clone())
            .collect();
        assert!(matches!(
            fresh.apply(Command::ReplaceQuickTransitions { rows }),
            Err(Error::Limit(_))
        ));
    }
    #[test]
    fn exact_row_limit_rejects_add_without_issuing_or_publishing_an_id() {
        let mut session = Session::new();
        let rows = (1..=MAX_QUICK_TRANSITIONS as u32)
            .map(|id| {
                QuickTransition::try_new(
                    QuickTransitionId::new(id).unwrap(),
                    TransitionKind::Fade,
                    300,
                    false,
                )
                .unwrap()
            })
            .collect();
        session
            .apply(Command::ReplaceQuickTransitions { rows })
            .unwrap();
        let before = session.snapshot();
        assert!(matches!(
            session.apply(Command::AddQuickTransition {
                kind: TransitionKind::Cut,
                duration_ms: 300,
                fade_to_black: false
            }),
            Err(Error::Limit(_))
        ));
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        session
            .apply(Command::RemoveQuickTransition {
                id: QuickTransitionId::new(1).unwrap(),
            })
            .unwrap();
        assert_eq!(add(&mut session).get(), 65);
    }
}
