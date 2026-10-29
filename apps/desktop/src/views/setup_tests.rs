use super::*;
use std::sync::Arc;

fn caps() -> SetupCapabilities {
    SetupCapabilities {
        recording: true,
        streaming: true,
    }
}
#[test]
fn staged_navigation_back_and_cancel_do_not_publish_and_review_applies_one_command() {
    let session = Session::new();
    let original = session.snapshot();
    let mut flow = SetupFlow::default();
    flow.open(&session, SetupOrigin::Tools, caps()).unwrap();
    let token = flow.token().unwrap();
    assert!(
        flow.action(token, SetupAction::Apply, &session, caps())
            .is_err()
    );
    flow.action(token, SetupAction::Next, &session, caps())
        .unwrap();
    flow.wizard.as_mut().unwrap().preset = VideoPreset::P720;
    flow.action(token, SetupAction::Back, &session, caps())
        .unwrap();
    assert_eq!(flow.wizard.as_ref().unwrap().preset, VideoPreset::P720);
    for _ in 0..3 {
        flow.action(token, SetupAction::Next, &session, caps())
            .unwrap();
    }
    let command = flow
        .action(token, SetupAction::Apply, &session, caps())
        .unwrap()
        .unwrap();
    assert!(matches!(
        command,
        Command::Setup {
            command: SetupCommand::Apply { .. }
        }
    ));
    assert!(Arc::ptr_eq(&original, &session.snapshot()));
    flow.action(token, SetupAction::Cancel, &session, caps())
        .unwrap();
    assert!(!flow.is_open());
    assert!(Arc::ptr_eq(&original, &session.snapshot()));
}

#[test]
fn reopened_foreign_and_changed_revision_actions_reject_without_closing_a_new_wizard() {
    let mut session = Session::new();
    let mut flow = SetupFlow::default();
    flow.open(&session, SetupOrigin::FirstRun, caps()).unwrap();
    let old = flow.token().unwrap();
    flow.action(old, SetupAction::Cancel, &session, caps())
        .unwrap();
    flow.open(&session, SetupOrigin::Tools, caps()).unwrap();
    let current = flow.token().unwrap();
    assert_ne!(old.generation, current.generation);
    assert!(
        flow.action(old, SetupAction::Cancel, &session, caps())
            .is_err()
    );
    assert!(
        flow.action(old, SetupAction::Skip, &session, caps())
            .is_err()
    );
    assert!(flow.is_open());
    session
        .apply(Command::RenameCollection {
            name: "Changed".into(),
        })
        .unwrap();
    assert!(
        flow.action(current, SetupAction::Next, &session, caps())
            .is_err()
    );
    assert!(
        flow.action(current, SetupAction::Next, &Session::new(), caps())
            .is_err()
    );
    flow.action(current, SetupAction::Cancel, &session, caps())
        .unwrap();
    assert!(!flow.is_open());
}

#[test]
fn unavailable_output_bad_codec_format_and_unconfigured_rtmp_never_produce_success() {
    let session = Session::new();
    let mut flow = SetupFlow::default();
    flow.open(
        &session,
        SetupOrigin::FirstRun,
        SetupCapabilities::default(),
    )
    .unwrap();
    let token = flow.token().unwrap();
    assert!(
        flow.action(
            token,
            SetupAction::Next,
            &session,
            SetupCapabilities::default()
        )
        .is_err()
    );
    assert!(matches!(
        flow.action(
            token,
            SetupAction::Skip,
            &session,
            SetupCapabilities::default()
        )
        .unwrap(),
        Some(Command::Setup {
            command: SetupCommand::Skip
        })
    ));
    flow.close();
    flow.open(&session, SetupOrigin::Tools, caps()).unwrap();
    let token = flow.token().unwrap();
    let wizard = flow.wizard.as_mut().unwrap();
    wizard.usage = Usage::Streaming;
    wizard.step = Step::Output;
    assert!(
        flow.action(token, SetupAction::Next, &session, caps())
            .is_err()
    );
    let wizard = flow.wizard.as_mut().unwrap();
    wizard.usage = Usage::Recording;
    wizard.original_video = VideoSettings::new(1279, 719, 30, 1).unwrap();
    assert!(
        flow.action(token, SetupAction::Next, &session, caps())
            .is_err(),
        "native output rejects odd dimensions without allocating or starting codecs"
    );
}

#[test]
fn switching_usage_keeps_local_drafts_but_applies_only_the_reviewed_output() {
    let mut session = Session::new();
    let id = session.snapshot().profiles().selected_id();
    session
        .apply(Command::Profile {
            command: skylive_core::ProfileCommand::SetStreaming {
                id,
                settings: skylive_core::StreamingSettings::new(
                    "rtmp://127.0.0.1:1935/live".into(),
                    "key".into(),
                    4500,
                    2,
                    true,
                    160,
                )
                .unwrap(),
            },
        })
        .unwrap();
    let mut flow = SetupFlow::default();
    flow.open(&session, SetupOrigin::Tools, caps()).unwrap();
    let token = flow.token().unwrap();
    let wizard = flow.wizard.as_mut().unwrap();
    wizard.recording.video_bitrate = "12000".into();
    wizard.usage = Usage::Streaming;
    wizard.step = Step::Review;
    let command = flow
        .action(token, SetupAction::Apply, &session, caps())
        .unwrap()
        .unwrap();
    let Command::Setup {
        command: SetupCommand::Apply { configuration },
    } = command
    else {
        panic!()
    };
    assert_eq!(
        configuration.recording(),
        session.snapshot().profiles().selected().recording()
    );
    assert_eq!(
        flow.wizard.as_ref().unwrap().recording.video_bitrate,
        "12000",
        "Back retains an unapplied draft without leaking it into another mode"
    );
}

#[test]
fn current_4k_video_and_fractional_rate_survive_manual_review_exactly() {
    let mut session = Session::new();
    session
        .apply(Command::SetVideo {
            width: 3840,
            height: 2160,
            fps_num: 30000,
            fps_den: 1001,
        })
        .unwrap();
    let before = session.snapshot();
    let mut flow = SetupFlow::default();
    flow.open(&session, SetupOrigin::Tools, caps()).unwrap();
    let token = flow.token().unwrap();
    for _ in 0..3 {
        flow.action(token, SetupAction::Next, &session, caps())
            .unwrap();
    }
    let Some(Command::Setup {
        command: SetupCommand::Apply { configuration },
    }) = flow
        .action(token, SetupAction::Apply, &session, caps())
        .unwrap()
    else {
        panic!("review must return one typed setup command")
    };
    assert_eq!(configuration.video(), before.video());
    assert_eq!(
        (
            configuration.video().fps_num(),
            configuration.video().fps_den()
        ),
        (30000, 1001)
    );
    assert!(
        Arc::ptr_eq(&before, &session.snapshot()),
        "review does not publish or claim native encoder acceptance"
    );
}

#[test]
fn review_rechecks_actual_output_capabilities_and_native_format_limits_before_commit() {
    let mut session = Session::new();
    let mut flow = SetupFlow::default();
    flow.open(&session, SetupOrigin::Tools, caps()).unwrap();
    let token = flow.token().unwrap();
    for _ in 0..3 {
        flow.action(token, SetupAction::Next, &session, caps())
            .unwrap();
    }
    let before = session.snapshot();
    assert!(
        flow.action(
            token,
            SetupAction::Apply,
            &session,
            SetupCapabilities::default()
        )
        .is_err()
    );
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
    flow.action(token, SetupAction::Cancel, &session, caps())
        .unwrap();
    session
        .apply(Command::SetVideo {
            width: 3839,
            height: 2160,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    let before = session.snapshot();
    flow.open(&session, SetupOrigin::Tools, caps()).unwrap();
    let token = flow.token().unwrap();
    flow.wizard.as_mut().unwrap().step = Step::Review;
    assert!(
        flow.action(token, SetupAction::Apply, &session, caps())
            .is_err(),
        "a valid core canvas must still satisfy native output constraints"
    );
    assert_eq!(
        flow.wizard.as_ref().unwrap().video().unwrap(),
        *before.video()
    );
    assert!(
        Arc::ptr_eq(&before, &session.snapshot()),
        "unsupported current video is never silently lowered or published"
    );
}
