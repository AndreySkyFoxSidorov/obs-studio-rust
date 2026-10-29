//! Pure logical deadlines: no native device, wall-clock sleeps or SDK calls.
use super::*;

#[test]
fn crossing_a_deadline_after_input_poll_does_not_render_before_repoll() {
    let session = Session::new();
    let output = FrameComposition::Scene(FrameScene::Live(session.preview()));
    let route = AudioRouteSnapshot::capture(&session, &output, MediaTime(0)).unwrap();
    let anchor = Instant::now() - Duration::from_millis(250);
    let (send, receive) = mpsc::sync_channel(8);
    let live = LiveProgram::new(route, 1, 1, anchor, send.clone()).unwrap();
    let deadline = live.next_deadline().unwrap();
    assert!(
        deadline < Instant::now(),
        "real clock represents the end of an opaque poll pass"
    );
    let mut owner = NativeOwner {
        camera_activation: Arc::new(Default::default()),
        camera_pending: None,
        camera_last: None,
        monitor: Default::default(),
        factory: Some(Box::new(|| {
            panic!("empty Program must not initialize a native backend")
        })),
        backend: None,
        streams: vec![],
        pool: vec![],
        scope: None,
        sequence: 0,
        retries: vec![],
        live: Some(live),
        retired: vec![],
        sources: vec![],
        latest: HashMap::new(),
        rotate: 0,
        immediate: false,
        worked: false,
        outputs: send,
        limits: AudioCaptureLimits::default(),
    };
    // A native poll starts1ns before the logical deadline; wall time is already
    // later by the time it returns. A fresh tick time here would emit4blocks.
    owner.periodic_at(deadline - Duration::from_nanos(1));
    assert!(matches!(receive.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert_eq!(owner.live.as_ref().unwrap().next_deadline(), Some(deadline));
    assert_eq!(owner.live.as_ref().unwrap().stats().output_blocks, 0);
    assert!(owner.backend.is_none());
    // The next input pass at the deadline may render exactly that block. No
    // deadline or sample cursor is skipped despite the later real wall clock.
    owner.periodic_at(deadline);
    let block = receive.try_recv().unwrap();
    assert_eq!(block.block.start_frame, 0);
    assert_eq!(block.block.frames, 1024);
    assert_eq!(block.stats.output_blocks, 1);
    assert_eq!(block.stats.skipped_blocks, 0);
    assert!(receive.try_recv().is_err());
}

#[test]
fn genuinely_missing_source_still_has_pending_and_gap_diagnostics() {
    let mut session = Session::new();
    let scene = session.preview();
    let item = session
        .apply(skylive_core::Command::AddAudioCaptureSource {
            scene,
            name: "No packets".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap()
        .created
        .unwrap();
    let source = session
        .snapshot()
        .scene(scene)
        .unwrap()
        .items()
        .iter()
        .find(|entry| entry.id() == item)
        .unwrap()
        .source();
    let route = AudioRouteSnapshot::capture(
        &session,
        &FrameComposition::Scene(FrameScene::Live(scene)),
        MediaTime(0),
    )
    .unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    let mut live = LiveProgram::new(route, 1, 1, Instant::now(), send).unwrap();
    live.tick(live.next_deadline().unwrap());
    let output = receive.try_recv().unwrap();
    assert_eq!(output.stats.pending_inputs, 1);
    assert!(
        output
            .block
            .gaps
            .iter()
            .any(|gap| gap.source == source && gap.missing_frames == 1024)
    );
    assert!(output.block.samples.iter().all(|&sample| sample == 0.0));
}
