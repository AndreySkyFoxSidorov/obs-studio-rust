use super::*;
use egui::TextBuffer;
use skylive_core::Session;

fn fixture() -> (Session, Id, Id) {
    let mut owner = Session::new();
    let first = owner.preview();
    owner
        .apply(Command::AddScene {
            name: "Second scene".into(),
        })
        .unwrap();
    let second = owner.snapshot().scenes()[1].id();
    (owner, first, second)
}
fn enable(owner: &mut Session, value: bool) {
    owner
        .apply(Command::SetPluginEnabled {
            plugin: SCENE_NOTES_PLUGIN.into(),
            enabled: value,
        })
        .unwrap();
}
#[test]
fn activation_clean_follow_and_dirty_pin_apply_to_original_owner_only() {
    let (mut owner, first, second) = fixture();
    let mut dock = NotesDock::default();
    assert!(!dock.observe(owner.identity(), &owner.snapshot(), first));
    assert!(dock.draft.is_none());
    enable(&mut owner, true);
    let initial = owner.snapshot();
    assert!(dock.observe(owner.identity(), &initial, first));
    assert!(!dock.observe(owner.identity(), &initial, second));
    assert_eq!(dock.draft.as_ref().unwrap().scene, second);
    dock.draft.as_mut().unwrap().text = "Cue second\nПривіт".into();
    let token = dock.draft.as_ref().unwrap().token;
    dock.observe(owner.identity(), &initial, first);
    assert_eq!(dock.draft.as_ref().unwrap().token, token);
    assert_eq!(dock.draft.as_ref().unwrap().scene, second);
    assert!(std::sync::Arc::ptr_eq(&initial, &owner.snapshot()));
    let submission = dock.submission(owner.identity(), &initial).unwrap();
    let command = dock
        .command_for(owner.identity(), &initial, &submission)
        .unwrap();
    owner
        .apply_at(Some(submission.token.revision), command)
        .unwrap();
    assert_eq!(
        owner.snapshot().scene(second).unwrap().notes(),
        "Cue second\nПривіт"
    );
    assert_eq!(owner.snapshot().scene(first).unwrap().notes(), "");
    dock.accepted(owner.identity(), &owner.snapshot(), first)
        .unwrap();
    assert_eq!(dock.draft.as_ref().unwrap().scene, first);
    assert!(!dock.draft.as_ref().unwrap().dirty());
    assert!(
        dock.command_for(owner.identity(), &owner.snapshot(), &submission)
            .is_err()
    );
}
#[test]
fn foreign_document_and_revision_changes_keep_dirty_text_until_explicit_cancel() {
    let (mut owner, first, second) = fixture();
    enable(&mut owner, true);
    let mut dock = NotesDock::default();
    dock.observe(owner.identity(), &owner.snapshot(), first);
    dock.draft.as_mut().unwrap().text = "Retained original draft".into();
    let submission = dock
        .submission(owner.identity(), &owner.snapshot())
        .unwrap();
    owner
        .apply(Command::RenameScene {
            scene: second,
            name: "Unrelated change".into(),
        })
        .unwrap();
    dock.observe(owner.identity(), &owner.snapshot(), second);
    assert_eq!(dock.draft.as_ref().unwrap().text, submission.text);
    assert_eq!(
        dock.current(owner.identity(), &owner.snapshot()).err(),
        Some(STALE)
    );
    let (mut replacement, replacement_first, _) = fixture();
    enable(&mut replacement, true);
    assert_eq!(
        first, replacement_first,
        "test intentionally collides stable IDs across distinct sessions"
    );
    dock.observe(
        replacement.identity(),
        &replacement.snapshot(),
        replacement_first,
    );
    assert_eq!(
        dock.current(replacement.identity(), &replacement.snapshot())
            .err(),
        Some(FOREIGN)
    );
    assert!(
        dock.command_for(replacement.identity(), &replacement.snapshot(), &submission)
            .is_err()
    );
    assert_eq!(dock.draft.as_ref().unwrap().text, submission.text);
    dock.cancel(
        submission.token,
        replacement.identity(),
        &replacement.snapshot(),
        replacement_first,
    )
    .unwrap();
    assert_eq!(
        dock.draft.as_ref().unwrap().token.session,
        replacement.identity()
    );
    assert_eq!(dock.draft.as_ref().unwrap().text, "");
    assert!(
        dock.cancel(
            submission.token,
            replacement.identity(),
            &replacement.snapshot(),
            replacement_first
        )
        .is_err()
    );
}
#[test]
fn disable_reenable_preserves_authored_notes_and_dirty_draft_without_rebasing() {
    let (mut owner, first, _) = fixture();
    enable(&mut owner, true);
    owner
        .apply(Command::SetSceneNotes {
            scene: first,
            text: "Authored note".into(),
        })
        .unwrap();
    let mut dock = NotesDock::default();
    dock.observe(owner.identity(), &owner.snapshot(), first);
    dock.draft.as_mut().unwrap().text = "Unapplied draft".into();
    let token = dock.draft.as_ref().unwrap().token;
    enable(&mut owner, false);
    assert!(!dock.observe(owner.identity(), &owner.snapshot(), first));
    assert!(
        dock.submission(owner.identity(), &owner.snapshot())
            .is_err()
    );
    assert_eq!(dock.draft.as_ref().unwrap().token, token);
    enable(&mut owner, true);
    assert!(dock.observe(owner.identity(), &owner.snapshot(), first));
    assert_eq!(dock.draft.as_ref().unwrap().text, "Unapplied draft");
    assert_eq!(
        dock.current(owner.identity(), &owner.snapshot()).err(),
        Some(STALE)
    );
    assert_eq!(
        owner.snapshot().scene(first).unwrap().notes(),
        "Authored note"
    );
    dock.cancel(token, owner.identity(), &owner.snapshot(), first)
        .unwrap();
    assert_eq!(dock.draft.as_ref().unwrap().text, "Authored note");
}
#[test]
fn forged_submissions_exhaustion_and_noop_do_not_publish_unrelated_edits() {
    let (mut owner, first, second) = fixture();
    enable(&mut owner, true);
    let mut dock = NotesDock::default();
    dock.observe(owner.identity(), &owner.snapshot(), first);
    let initial = owner.snapshot();
    let valid = dock.submission(owner.identity(), &initial).unwrap();
    let noop = dock
        .command_for(owner.identity(), &initial, &valid)
        .unwrap();
    assert!(!owner.apply(noop).unwrap().changed);
    assert!(std::sync::Arc::ptr_eq(&initial, &owner.snapshot()));
    for bad in [
        NotesSubmission {
            scene: second,
            ..valid.clone()
        },
        NotesSubmission {
            text: "injected".into(),
            ..valid.clone()
        },
        NotesSubmission {
            token: PluginEditorToken {
                generation: valid.token.generation + 1,
                ..valid.token
            },
            ..valid.clone()
        },
    ] {
        assert!(dock.command_for(owner.identity(), &initial, &bad).is_err());
    }
    dock.sequence = u64::MAX;
    assert_eq!(
        dock.submission(owner.identity(), &initial).err(),
        Some(EXHAUSTED)
    );
    assert_eq!(
        dock.cancel(valid.token, owner.identity(), &initial, second)
            .err(),
        Some(EXHAUSTED)
    );
    assert_eq!(dock.draft.as_ref().unwrap().scene, first);
    assert_eq!(dock.draft.as_ref().unwrap().token, valid.token);
    assert!(std::sync::Arc::ptr_eq(&initial, &owner.snapshot()));
}
#[test]
fn notes_text_admission_counts_bytes_preserves_unicode_and_rejects_whole_replacement() {
    let mut text = "A".to_owned();
    let mut buffer = NotesText::new(&mut text);
    assert_eq!(buffer.insert_text("я🙂", 1), 2);
    assert_eq!(buffer.as_str(), "Aя🙂");
    buffer.delete_char_range(1..2);
    assert_eq!(buffer.as_str(), "A🙂");
    let mut text = "я".repeat(MAX_SCENE_NOTES_BYTES / 2);
    let before = text.clone();
    let rejected = {
        let mut buffer = NotesText::new(&mut text);
        buffer.delete_char_range(0..1);
        assert_eq!(buffer.insert_text("🙂", 0), 0);
        assert_eq!(buffer.as_str(), before);
        buffer.delete_char_range(0..1);
        assert_eq!(buffer.insert_text("x", 0), 0);
        assert_eq!(buffer.as_str(), before);
        buffer.rejected
    };
    if rejected {
        text = before.clone();
    }
    assert_eq!(text, before);
    assert_eq!(text.len(), MAX_SCENE_NOTES_BYTES);
    let mut buffer = NotesText::new(&mut text);
    assert_eq!(buffer.insert_text("x", 0), 0);
    assert!(buffer.rejected);
    assert_eq!(buffer.as_str(), before);
}

#[test]
fn changing_editor_identity_or_hiding_retires_old_focus_and_text_undo_state() {
    let ctx = egui::Context::default();
    let mut slot = None;
    let old = egui::Id::new(("notes-editor", 1_u64));
    let new = egui::Id::new(("notes-editor", 2_u64));
    use_editor(&ctx, &mut slot, old);
    ctx.memory_mut(|memory| memory.request_focus(old));
    assert!(egui::text_edit::TextEditState::load(&ctx, old).is_some());
    use_editor(&ctx, &mut slot, new);
    assert!(egui::text_edit::TextEditState::load(&ctx, old).is_none());
    assert!(!ctx.memory(|memory| memory.has_focus(old)));
    assert!(egui::text_edit::TextEditState::load(&ctx, new).is_some());
    retire_editor(&ctx, &mut slot);
    assert!(slot.is_none());
    assert!(egui::text_edit::TextEditState::load(&ctx, new).is_none());
}
