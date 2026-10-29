//! Live degradation tests use a worker-owned fake writer, never native SDKs.
use super::*;
use crate::recording::{MAX_RECORDING_WARNING_BYTES, MAX_RECORDING_WARNINGS, RecordingWarnings};
use crate::{preview::SourceDiagnostic, resource_issue::ResourceIssueKind};

fn wait_frames(owner: &mut RecordingOwner, expected: u64) -> RecordingStatus {
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        let status = owner.status();
        assert!(status.problem.is_none(), "{:?}", status.problem);
        if status.video_frames == expected {
            return status;
        }
        assert!(Instant::now() < end, "recorded frame deadline: {status:?}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn cold_source_start_continues_then_healthy_frame_clears_warnings_without_extra_events() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let scene = session.preview();
    session
        .apply(Command::AddColorSource {
            scene,
            name: "Healthy".into(),
            rgba: [0, 255, 0, 255],
        })
        .unwrap();
    let missing = session
        .apply(Command::AddImageSource {
            scene,
            name: "Missing".into(),
            asset: skylive_core::ImageAsset::new(
                "missing-linked-resource.png".into(),
                [0; 32],
                1,
                1,
            )
            .unwrap(),
        })
        .unwrap()
        .created
        .unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(calls.clone(), false, None),
        )
        .unwrap();
    // Started stays unread: warnings must never fill the terminal event channel.
    send(&mut owner, frame(&mut session, 0));
    let degraded = wait_frames(&mut owner, 1);
    assert!(degraded.active);
    assert_eq!(degraded.degraded_video_frames, 1);
    let warning = degraded.warnings.unwrap();
    assert_eq!(warning.diagnostics.len(), 1);
    assert_eq!(warning.diagnostics[0].kind, ResourceIssueKind::Unavailable);
    assert_eq!(warning.session, session.identity());
    assert_eq!(warning.revision, session.snapshot().revision());
    assert!(warning.retained_bytes() <= MAX_RECORDING_WARNING_BYTES);
    session
        .apply(Command::RemoveItem {
            scene,
            item: missing,
        })
        .unwrap();
    send(&mut owner, frame(&mut session, 1));
    let healthy = wait_frames(&mut owner, 2);
    assert!(healthy.warnings.is_none());
    assert_eq!(healthy.degraded_video_frames, 1);
    assert!(warning.revision < session.snapshot().revision());
    owner.stop().unwrap();
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    assert!(matches!(
        settle(&mut owner),
        RecordingEvent::Finished {
            video_frames: 2,
            ..
        }
    ));
    owner.join().unwrap();
    assert_eq!(
        &calls.lock().unwrap()[1..],
        &[Call::Video(0), Call::Video(1), Call::Finalize]
    );
}

#[test]
fn warning_payload_bounds_exact_strings_and_capacities_without_historical_sources() {
    let directory = Directory::new();
    let (mut session, _) = setup(directory.output());
    let value = frame(&mut session, 0);
    let mut diagnostics = Vec::with_capacity(4096);
    for index in 0..MAX_RECORDING_WARNINGS + 3 {
        let mut detail = String::with_capacity(128 * 1024);
        detail.push_str(&format!("exact native detail {index}"));
        diagnostics.push(SourceDiagnostic {
            source: skylive_core::Id::new(index as u64),
            kind: ResourceIssueKind::Unavailable,
            code: "fixture",
            detail,
        });
    }
    diagnostics.push(SourceDiagnostic {
        source: skylive_core::Id::new(900),
        kind: ResourceIssueKind::Unavailable,
        code: "oversize",
        detail: "X".repeat(MAX_RECORDING_WARNING_BYTES + 1),
    });
    let warnings = RecordingWarnings::from_frame(&value, diagnostics).unwrap();
    assert_eq!(warnings.diagnostics.len(), MAX_RECORDING_WARNINGS);
    assert_eq!(warnings.omitted, 4);
    assert_eq!(warnings.diagnostics.capacity(), warnings.diagnostics.len());
    for (index, issue) in warnings.diagnostics.iter().enumerate() {
        assert_eq!(issue.detail, format!("exact native detail {index}"));
        assert_eq!(issue.detail.capacity(), issue.detail.len());
    }
    assert!(warnings.retained_bytes() <= MAX_RECORDING_WARNING_BYTES);
    assert!(RecordingWarnings::from_frame(&value, vec![]).is_none());
}

#[test]
fn stale_frame_remains_fatal_with_recovery_instead_of_becoming_a_warning() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let mut stale = frame(&mut session, 0);
    stale.capture = Arc::new(CaptureSnapshot::empty(session.identity() + 1));
    let calls = Arc::new(Mutex::new(vec![]));
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(calls.clone(), false, None),
        )
        .unwrap();
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    send(&mut owner, stale);
    let RecordingEvent::Failed(error) = settle(&mut owner) else {
        panic!("foreign capture must fail");
    };
    assert!(error.recovery_path.is_some());
    assert!(!owner.status().active);
    assert!(owner.status().warnings.is_none());
    owner.join().unwrap();
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| matches!(call, Call::Video(_)))
    );
}

#[test]
fn warning_human_name_is_captured_from_the_submitted_model_not_the_latest_rename() {
    let directory = Directory::new();
    let (mut session, _) = setup(directory.output());
    let scene = session.preview();
    session
        .apply(Command::AddImageSource {
            scene,
            name: "Original source name".into(),
            asset: skylive_core::ImageAsset::new("missing.png".into(), [0; 32], 1, 1).unwrap(),
        })
        .unwrap();
    let source = session.snapshot().sources()[0].id();
    let old = frame(&mut session, 0);
    session
        .apply(Command::RenameSource {
            source,
            name: "Later source name".into(),
        })
        .unwrap();
    let issues = || {
        vec![SourceDiagnostic {
            source,
            kind: ResourceIssueKind::Unavailable,
            code: "image_io",
            detail: "exact diagnostic".into(),
        }]
    };
    let captured = RecordingWarnings::from_frame(&old, issues()).unwrap();
    assert_eq!(captured.source_name(source), Some("Original source name"));
    let newer = RecordingWarnings::from_frame(&frame(&mut session, 1), issues()).unwrap();
    assert_eq!(newer.source_name(source), Some("Later source name"));
    for warnings in [&captured, &newer] {
        assert_eq!(warnings.source_names.len(), 1);
        assert_eq!(warnings.source_names.capacity(), 1);
        assert_eq!(
            warnings.source_names[0].name.capacity(),
            warnings.source_names[0].name.len()
        );
        assert!(warnings.retained_bytes() <= MAX_RECORDING_WARNING_BYTES);
    }
}
