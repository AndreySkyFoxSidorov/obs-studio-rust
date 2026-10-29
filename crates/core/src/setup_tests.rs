use super::*;
use crate::{Command, Session};
use std::sync::Arc;

fn configuration(session: &Session) -> SetupConfiguration {
    let model = session.snapshot();
    SetupConfiguration::new(
        model.profiles().selected_id(),
        VideoSettings::new(1280, 720, 30, 1).unwrap(),
        RecordingSettings::new("D:/Clips/Запис".into(), 4500, 160, true, false).unwrap(),
        StreamingSettings::new(
            "rtmp://127.0.0.1:1935/live".into(),
            "exact-key".into(),
            4500,
            2,
            true,
            160,
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn one_setup_publication_applies_all_selected_profile_settings_and_metadata_outside_scene_undo() {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::AddColorSource {
            scene,
            name: "Scene history".into(),
            rgba: [1, 2, 3, 255],
        })
        .unwrap();
    let original = session.snapshot();
    let configuration = configuration(&session);
    let revision = original.revision();
    session
        .apply_at(
            Some(revision),
            Command::Setup {
                command: SetupCommand::Apply {
                    configuration: configuration.clone(),
                },
            },
        )
        .unwrap();
    let applied = session.snapshot();
    assert_eq!(applied.revision(), revision + 1);
    assert_eq!(applied.video(), configuration.video());
    assert_eq!(
        applied.profiles().selected().recording(),
        configuration.recording()
    );
    assert_eq!(
        applied.profiles().selected().streaming(),
        configuration.streaming()
    );
    assert_eq!(applied.preferences().setup_state(), SetupState::Completed);
    assert_eq!(applied.scenes(), original.scenes());
    assert_eq!(applied.sources(), original.sources());
    assert_eq!(applied.audio(), original.audio());
    session.apply(Command::Undo).unwrap();
    assert!(session.snapshot().sources().is_empty());
    assert_eq!(session.snapshot().profiles(), applied.profiles());
    assert_eq!(session.snapshot().preferences(), applied.preferences());
}

#[test]
fn wrong_profile_stale_and_closed_setup_preserve_snapshot_and_history() {
    let mut session = Session::new();
    let initial = session.snapshot();
    let configuration = configuration(&session);
    assert!(Arc::ptr_eq(&initial, &session.snapshot()));
    assert!(!session.can_undo());
    session
        .apply(Command::Profile {
            command: ProfileCommand::New {
                name: "Different selected profile".into(),
            },
        })
        .unwrap();
    let before = session.snapshot();
    assert!(
        session
            .apply(Command::Setup {
                command: SetupCommand::Apply {
                    configuration: configuration.clone()
                }
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
    assert!(
        session
            .apply_at(
                Some(0),
                Command::Setup {
                    command: SetupCommand::Skip
                }
            )
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
    session.close();
    assert!(
        session
            .apply(Command::Setup {
                command: SetupCommand::Skip
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
}

#[test]
fn explicit_skip_is_global_not_scene_dirty_and_equal_apply_is_noop() {
    let mut session = Session::new();
    session
        .apply(Command::Setup {
            command: SetupCommand::Skip,
        })
        .unwrap();
    let skipped = session.snapshot();
    assert_eq!(skipped.preferences().setup_state(), SetupState::Skipped);
    assert!(!session.is_document_dirty());
    assert!(!session.can_undo());
    session
        .apply(Command::Setup {
            command: SetupCommand::Skip,
        })
        .unwrap();
    assert!(Arc::ptr_eq(&skipped, &session.snapshot()));
    let configuration = configuration(&session);
    session
        .apply(Command::Setup {
            command: SetupCommand::Apply {
                configuration: configuration.clone(),
            },
        })
        .unwrap();
    let completed = session.snapshot();
    session
        .apply(Command::Setup {
            command: SetupCommand::Apply { configuration },
        })
        .unwrap();
    assert!(Arc::ptr_eq(&completed, &session.snapshot()));
}

#[test]
fn new_profile_checkbox_and_profile_publish_together_and_rejected_name_changes_neither() {
    let mut session = Session::new();
    assert!(session.snapshot().preferences().setup_on_new_profile());
    session
        .apply(Command::Setup {
            command: SetupCommand::CreateProfile {
                name: "New profile".into(),
                run_setup: false,
            },
        })
        .unwrap();
    let created = session.snapshot();
    assert_eq!(created.profiles().selected().name(), "New profile");
    assert!(!created.preferences().setup_on_new_profile());
    assert_eq!(created.preferences().setup_state(), SetupState::Unseen);
    assert!(!session.can_undo());
    assert!(
        session
            .apply(Command::Setup {
                command: SetupCommand::CreateProfile {
                    name: String::new(),
                    run_setup: true
                }
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&created, &session.snapshot()));
}
