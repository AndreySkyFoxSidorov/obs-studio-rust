//! Deterministic fake-backend tests. No native backend is constructed or called.
use super::*;
use crate::transition::Controller;
use skylive_core::Command;
use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};

enum Call {
    Displays {
        limits: CaptureLimits,
        reply: SyncSender<Result<Vec<DisplayInfo>, CaptureError>>,
    },
    Frame {
        binding: DisplayCaptureBinding,
        limits: CaptureLimits,
        reply: SyncSender<Result<RgbaFrame, CaptureError>>,
    },
}
struct FakeBackend {
    calls: SyncSender<Call>,
    dropped: SyncSender<thread::ThreadId>,
}
impl Drop for FakeBackend {
    fn drop(&mut self) {
        let _ = self.dropped.send(thread::current().id());
    }
}
impl CaptureBackend for FakeBackend {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            available: true,
            backend: "controlled-test",
            cursor: true,
            hdr: false,
            note: None,
        }
    }
    fn enumerate(&mut self, limits: CaptureLimits) -> Result<Vec<DisplayInfo>, CaptureError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.calls
            .send(Call::Displays { limits, reply })
            .map_err(|_| CaptureError::Disconnected)?;
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| Err(CaptureError::Backend("fake response deadline".into())))
    }
    fn capture(
        &mut self,
        binding: &DisplayCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.calls
            .send(Call::Frame {
                binding: binding.clone(),
                limits,
                reply,
            })
            .map_err(|_| CaptureError::Disconnected)?;
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| Err(CaptureError::Backend("fake response deadline".into())))
    }
}
struct Harness {
    calls: Receiver<Call>,
    dropped: Receiver<thread::ThreadId>,
}
impl Harness {
    fn new(limits: CaptureLimits) -> (CaptureWorker, Self) {
        let (calls, incoming) = mpsc::sync_channel(1);
        let (dropped, outgoing) = mpsc::sync_channel(1);
        (
            CaptureWorker::with_limits(Box::new(FakeBackend { calls, dropped }), limits).unwrap(),
            Self {
                calls: incoming,
                dropped: outgoing,
            },
        )
    }
    fn next(&self) -> Call {
        self.calls
            .recv_timeout(Duration::from_secs(2))
            .expect("fake worker call")
    }
    fn idle(&self) {
        assert!(matches!(
            self.calls.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
    fn frame(
        &self,
    ) -> (
        DisplayCaptureBinding,
        CaptureLimits,
        SyncSender<Result<RgbaFrame, CaptureError>>,
    ) {
        match self.next() {
            Call::Frame {
                binding,
                limits,
                reply,
            } => (binding, limits, reply),
            _ => panic!("expected capture"),
        }
    }
    fn displays(
        &self,
    ) -> (
        CaptureLimits,
        SyncSender<Result<Vec<DisplayInfo>, CaptureError>>,
    ) {
        match self.next() {
            Call::Displays { limits, reply } => (limits, reply),
            _ => panic!("expected enumeration"),
        }
    }
    fn join(self, worker: &mut CaptureWorker) {
        worker.join().unwrap();
        assert_ne!(
            self.dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
            thread::current().id()
        );
    }
}
fn settle(worker: &mut CaptureWorker, session: &Session, now: u64) -> Option<CaptureEvent> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let event = worker.poll(session, MediaTime(now)).unwrap();
        if !worker.is_busy() {
            return event;
        }
        assert!(Instant::now() < deadline, "capture completion deadline");
        thread::sleep(Duration::from_millis(1));
    }
}
fn binding(id: &str, cursor: bool, width: u32, height: u32) -> DisplayCaptureBinding {
    DisplayCaptureBinding::new(
        DisplayTarget::Windows {
            device_id: id.into(),
        },
        cursor,
        width,
        height,
    )
    .unwrap()
}
fn add(session: &mut Session, scene: Id, name: &str, binding: DisplayCaptureBinding) -> (Id, Id) {
    let item = session
        .apply(Command::AddDisplayCaptureSource {
            scene,
            name: name.into(),
            binding,
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
        .find(|i| i.id() == item)
        .unwrap()
        .source();
    (source, item)
}
fn scene(session: &mut Session, name: &str) -> Id {
    session
        .apply(Command::AddScene { name: name.into() })
        .unwrap()
        .created
        .unwrap()
}
fn frame(color: u8) -> RgbaFrame {
    RgbaFrame::solid(2, 2, [color, 0, 0, 255]).unwrap()
}
fn display(id: &str) -> DisplayInfo {
    DisplayInfo {
        target: DisplayTarget::Windows {
            device_id: id.into(),
        },
        name: format!("Display {id}"),
        width: 2,
        height: 2,
        primary: false,
    }
}
fn period(session: &Session) -> u64 {
    let snapshot = session.snapshot();
    (1_000_000_000u64 * u64::from(snapshot.video().fps_den()))
        .div_ceil(u64::from(snapshot.video().fps_num()))
}

#[test]
fn owner_is_lazy_enumeration_is_explicit_and_only_one_job_is_admitted() {
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    add(&mut session, live, "Capture", binding("A", true, 2, 2));
    assert_eq!(worker.capabilities().backend, "controlled-test");
    assert!(
        worker
            .synchronize(&session, &[FrameScene::Live(live)])
            .unwrap()
    );
    harness.idle();
    assert_eq!(worker.snapshot().frame_count(), 0);
    let key = worker.request_displays(&session).unwrap();
    let (limits, reply) = harness.displays();
    assert_eq!(limits.max_displays, 64);
    assert!(worker.is_busy());
    assert_eq!(worker.request_displays(&session), Err(CaptureError::Busy));
    assert_eq!(
        worker.request_frame(&session, MediaTime(0)),
        Err(CaptureError::Busy)
    );
    reply.send(Ok(vec![display("A")])).unwrap();
    match settle(&mut worker, &session, 0).unwrap() {
        CaptureEvent::Displays {
            key: actual,
            result,
        } => {
            assert_eq!(actual, key);
            assert_eq!(result.unwrap(), vec![display("A")]);
        }
        _ => panic!("expected display completion"),
    }
    harness.idle();
    assert_eq!(worker.snapshot().frame_count(), 0);
    assert!(worker.request_frame(&session, MediaTime(0)).unwrap());
    harness.frame().2.send(Ok(frame(7))).unwrap();
    assert!(matches!(
        settle(&mut worker, &session, 0),
        Some(CaptureEvent::Frames { .. })
    ));
    harness.join(&mut worker);
}

#[test]
fn matching_target_and_cursor_share_one_arc_despite_different_authored_bases() {
    let limits = CaptureLimits {
        max_frame_bytes: 16,
        max_total_frame_bytes: 16,
        ..CaptureLimits::default()
    };
    let (mut worker, harness) = Harness::new(limits);
    let mut session = Session::new();
    let live = session.preview();
    let (first, _) = add(&mut session, live, "First", binding("A", true, 2, 2));
    let (second, _) = add(&mut session, live, "Other basis", binding("A", true, 4, 4));
    let (different_cursor, _) = add(
        &mut session,
        live,
        "Other cursor",
        binding("A", false, 2, 2),
    );
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_frame(&session, MediaTime(11)).unwrap();
    let (actual, admission, reply) = harness.frame();
    assert_eq!(actual.target(), binding("A", true, 1, 1).target());
    assert!(actual.capture_cursor());
    assert_eq!(admission.max_frame_bytes, 16);
    reply.send(Ok(frame(19))).unwrap();
    settle(&mut worker, &session, 11);
    let output = worker.snapshot();
    let first = output.frame(first).unwrap();
    let second = output.frame(second).unwrap();
    assert!(first.image.shares_pixels_with(&second.image));
    assert_eq!((first.binding.width(), second.binding.width()), (2, 4));
    assert_eq!(first.image.width(), 2);
    assert_eq!(output.frame_count(), 2);
    assert_eq!(
        output.problem(different_cursor).unwrap().code,
        "capture_budget_exceeded"
    );
    drop(output);
    harness.idle();
    harness.join(&mut worker);
}

#[test]
fn unique_frames_receive_only_remaining_budget_and_oversized_results_are_rejected() {
    let limits = CaptureLimits {
        max_frame_bytes: 16,
        max_total_frame_bytes: 24,
        ..CaptureLimits::default()
    };
    let (mut worker, harness) = Harness::new(limits);
    let mut session = Session::new();
    let live = session.preview();
    let (first, _) = add(&mut session, live, "First", binding("A", false, 2, 2));
    let (second, _) = add(&mut session, live, "Second", binding("B", false, 2, 2));
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_frame(&session, MediaTime(0)).unwrap();
    harness.frame().2.send(Ok(frame(1))).unwrap();
    let (_, admission, reply) = harness.frame();
    assert_eq!(admission.max_frame_bytes, 8);
    // Deliberately noncooperating backend: owner must still reject its 16 bytes.
    reply.send(Ok(frame(2))).unwrap();
    settle(&mut worker, &session, 0);
    assert_eq!(worker.snapshot().frame(first).unwrap().image.pixels()[0], 1);
    assert!(worker.snapshot().frame(second).is_none());
    assert_eq!(
        worker.snapshot().problem(second).unwrap().code,
        "capture_budget_exceeded"
    );
    harness.idle();
    harness.join(&mut worker);
}

#[test]
fn captured_studio_visibility_and_retired_sources_remain_in_the_union() {
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    let mut session = Session::new();
    let first_scene = session.preview();
    let (source, first_item) = add(
        &mut session,
        first_scene,
        "Shared display",
        binding("A", true, 2, 2),
    );
    let second_scene = scene(&mut session, "Preview");
    let second_item = session
        .apply(Command::AddExistingSource {
            scene: second_scene,
            source,
        })
        .unwrap()
        .created
        .unwrap();
    let mut controller = Controller::new();
    controller
        .set_studio(&mut session, true, MediaTime(0))
        .unwrap();
    controller
        .select_preview(&mut session, second_scene, MediaTime(0))
        .unwrap();
    let frozen = controller
        .sync(&mut session, MediaTime(0))
        .unwrap()
        .output
        .scenes()
        .remove(0);
    assert!(matches!(frozen, FrameScene::Frozen(_)));
    for (owner, item) in [(first_scene, first_item), (second_scene, second_item)] {
        controller.before_change(&session).unwrap();
        session
            .apply(Command::SetVisible {
                scene: owner,
                item,
                visible: false,
            })
            .unwrap();
    }
    worker
        .synchronize(&session, &[frozen.clone(), FrameScene::Live(second_scene)])
        .unwrap();
    assert_eq!(worker.bindings.len(), 1);
    // The source disappears from canonical state but remains owned metadata in
    // the private Program layout; it must still be eligible for native capture.
    for (owner, item) in [(first_scene, first_item), (second_scene, second_item)] {
        controller.before_change(&session).unwrap();
        session
            .apply(Command::RemoveItem { scene: owner, item })
            .unwrap();
    }
    assert!(session.snapshot().source(source).is_none());
    let frozen = controller
        .sync(&mut session, MediaTime(0))
        .unwrap()
        .output
        .scenes()
        .remove(0);
    worker
        .synchronize(&session, &[frozen, FrameScene::Live(second_scene)])
        .unwrap();
    assert_eq!(worker.bindings.len(), 1);
    worker.request_frame(&session, MediaTime(0)).unwrap();
    harness.frame().2.send(Ok(frame(3))).unwrap();
    settle(&mut worker, &session, 0);
    assert!(worker.snapshot().frame(source).is_some());
    worker
        .synchronize(&session, &[FrameScene::Live(second_scene)])
        .unwrap();
    assert_eq!(worker.snapshot().frame_count(), 0);
    // The final binding still needs one owner-thread retirement control job.
    // It performs no hardware acquisition and publishes no synthetic frame.
    let cleared = worker.snapshot();
    assert_eq!(worker.next_after_ns(MediaTime(0)), Some(1));
    assert!(worker.request_frame(&session, MediaTime(0)).unwrap());
    assert!(settle(&mut worker, &session, 0).is_none());
    assert!(Arc::ptr_eq(&cleared, &worker.snapshot()));
    assert_eq!(worker.next_after_ns(MediaTime(0)), None);
    assert!(!worker.request_frame(&session, MediaTime(0)).unwrap());
    harness.idle();
    harness.join(&mut worker);
}

#[test]
fn hiding_or_rebinding_while_busy_rejects_late_frames_without_extra_jobs() {
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let (source, item) = add(&mut session, live, "Display", binding("A", true, 2, 2));
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_frame(&session, MediaTime(0)).unwrap();
    let old = harness.frame().2;
    session
        .apply(Command::SetVisible {
            scene: live,
            item,
            visible: false,
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    let cleared = worker.snapshot();
    old.send(Ok(frame(1))).unwrap();
    assert!(settle(&mut worker, &session, 0).is_none());
    assert!(Arc::ptr_eq(&cleared, &worker.snapshot()));
    session
        .apply(Command::SetVisible {
            scene: live,
            item,
            visible: true,
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_frame(&session, MediaTime(1)).unwrap();
    let old = harness.frame().2;
    session
        .apply(Command::RebindDisplayCapture {
            source,
            binding: binding("B", false, 4, 4),
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    assert_eq!(
        worker.request_frame(&session, MediaTime(2)),
        Err(CaptureError::Busy)
    );
    harness.idle();
    old.send(Ok(frame(2))).unwrap();
    assert!(settle(&mut worker, &session, 2).is_none());
    assert_eq!(worker.snapshot().frame_count(), 0);
    worker.request_frame(&session, MediaTime(2)).unwrap();
    let (actual, _, reply) = harness.frame();
    assert_eq!(actual, binding("B", false, 4, 4));
    reply.send(Ok(frame(9))).unwrap();
    settle(&mut worker, &session, 2);
    assert_eq!(
        worker.snapshot().frame(source).unwrap().image.pixels()[0],
        9
    );
    harness.join(&mut worker);
}

#[test]
fn closed_session_reopen_keeps_one_active_job_and_discards_foreign_completion() {
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    let mut old_session = Session::new();
    let old_scene = old_session.preview();
    add(
        &mut old_session,
        old_scene,
        "Old",
        binding("A", false, 2, 2),
    );
    worker
        .synchronize(&old_session, &[FrameScene::Live(old_scene)])
        .unwrap();
    worker.request_frame(&old_session, MediaTime(0)).unwrap();
    let blocked = harness.frame().2;
    old_session.close();
    assert_eq!(
        worker.request_frame(&old_session, MediaTime(1)),
        Err(CaptureError::Closed)
    );
    let mut reopened = Session::new();
    let new_scene = reopened.preview();
    let (source, _) = add(&mut reopened, new_scene, "New", binding("B", false, 2, 2));
    worker
        .synchronize(&reopened, &[FrameScene::Live(new_scene)])
        .unwrap();
    assert_eq!(worker.request_displays(&reopened), Err(CaptureError::Busy));
    assert_eq!(
        worker.request_frame(&reopened, MediaTime(1)),
        Err(CaptureError::Busy)
    );
    harness.idle();
    blocked.send(Ok(frame(1))).unwrap();
    assert!(settle(&mut worker, &reopened, 1).is_none());
    assert_eq!(worker.snapshot().session(), reopened.identity());
    assert_eq!(worker.snapshot().frame_count(), 0);
    worker.request_frame(&reopened, MediaTime(1)).unwrap();
    harness.frame().2.send(Ok(frame(5))).unwrap();
    settle(&mut worker, &reopened, 1);
    assert_eq!(
        worker.snapshot().frame(source).unwrap().image.pixels()[0],
        5
    );
    harness.join(&mut worker);
}

#[test]
fn worker_close_invalidates_publication_then_join_waits_only_after_fake_release() {
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    add(&mut session, live, "Display", binding("A", false, 2, 2));
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_frame(&session, MediaTime(0)).unwrap();
    let blocked = harness.frame().2;
    worker.close();
    assert_eq!(worker.snapshot().frame_count(), 0);
    assert_eq!(worker.next_after_ns(MediaTime(0)), None);
    assert_eq!(
        worker.request_frame(&session, MediaTime(0)),
        Err(CaptureError::Closed)
    );
    assert!(matches!(
        worker.poll(&session, MediaTime(0)),
        Err(CaptureError::Closed)
    ));
    assert!(matches!(
        harness.dropped.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    blocked.send(Ok(frame(8))).unwrap();
    harness.join(&mut worker);
}

#[test]
fn transient_failure_retains_exact_frame_and_repeated_errors_back_off_without_republishing() {
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let (source, _) = add(&mut session, live, "Display", binding("A", false, 2, 2));
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_frame(&session, MediaTime(0)).unwrap();
    harness.frame().2.send(Ok(frame(6))).unwrap();
    settle(&mut worker, &session, 0);
    let first = worker.snapshot();
    let tick = period(&session);
    assert!(!worker.request_frame(&session, MediaTime(tick - 1)).unwrap());
    worker.request_frame(&session, MediaTime(tick)).unwrap();
    harness
        .frame()
        .2
        .send(Err(CaptureError::PermissionDenied))
        .unwrap();
    settle(&mut worker, &session, tick);
    let failed = worker.snapshot();
    assert!(first.frame(source).unwrap().image.shares_pixels_with(&failed.frame(source).unwrap().image));
    assert_eq!(
        failed.problem(source).unwrap().code,
        "capture_permission_denied"
    );
    assert_eq!(worker.next_after_ns(MediaTime(tick)), Some(1_000_000_000));
    assert!(
        !worker
            .request_frame(&session, MediaTime(tick + 999_999_999))
            .unwrap()
    );
    harness.idle();
    worker
        .request_frame(&session, MediaTime(tick + 1_000_000_000))
        .unwrap();
    harness
        .frame()
        .2
        .send(Err(CaptureError::PermissionDenied))
        .unwrap();
    assert!(settle(&mut worker, &session, tick + 1_000_000_000).is_none());
    assert!(Arc::ptr_eq(&failed, &worker.snapshot()));
    drop(first);
    drop(failed);
    harness.join(&mut worker);
}

#[test]
fn enumeration_rejects_oversized_duplicate_or_malformed_metadata_without_model_edits() {
    let (mut worker, harness) = Harness::new(CaptureLimits {
        max_displays: 1,
        ..CaptureLimits::default()
    });
    let session = Session::new();
    let original = session.snapshot();
    let mut invalid = display("A");
    invalid.width = 0;
    let mut bad_name = display("A");
    bad_name.name = "bad\nname".into();
    for (data, code) in [
        (vec![display("A"), display("B")], "capture_budget_exceeded"),
        (vec![invalid], "capture_invalid_frame"),
        (vec![bad_name], "capture_invalid_frame"),
    ] {
        worker.request_displays(&session).unwrap();
        harness.displays().1.send(Ok(data)).unwrap();
        match settle(&mut worker, &session, 0).unwrap() {
            CaptureEvent::Displays { result, .. } => assert_eq!(result.unwrap_err().code(), code),
            _ => panic!(),
        }
        assert!(Arc::ptr_eq(&original, &session.snapshot()));
    }
    harness.join(&mut worker);
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    worker.request_displays(&session).unwrap();
    harness
        .displays()
        .1
        .send(Ok(vec![display("A"), display("A")]))
        .unwrap();
    match settle(&mut worker, &session, 0).unwrap() {
        CaptureEvent::Displays { result, .. } => {
            assert!(matches!(result, Err(CaptureError::InvalidFrame(_))))
        }
        _ => panic!(),
    }
    harness.join(&mut worker);
}

#[test]
fn owner_limits_and_identity_overflow_reject_before_jobs_or_state_publication() {
    assert!(matches!(
        CaptureWorker::with_limits(
            Box::new(UnsupportedBackend),
            CaptureLimits {
                max_displays: 0,
                ..CaptureLimits::default()
            }
        ),
        Err(CaptureError::BudgetExceeded(_))
    ));
    let (mut worker, harness) = Harness::new(CaptureLimits {
        max_sources: 1,
        ..CaptureLimits::default()
    });
    let mut session = Session::new();
    let live = session.preview();
    add(&mut session, live, "A", binding("A", false, 2, 2));
    add(&mut session, live, "B", binding("B", false, 2, 2));
    let initial = worker.snapshot();
    assert!(matches!(
        worker.synchronize(&session, &[FrameScene::Live(live)]),
        Err(CaptureError::BudgetExceeded(_))
    ));
    assert!(Arc::ptr_eq(&initial, &worker.snapshot()));
    assert_eq!(worker.session, 0);
    assert!(matches!(
        worker.synchronize(&session, &vec![FrameScene::Live(live); 4]),
        Err(CaptureError::BudgetExceeded(_))
    ));
    worker.serial = u64::MAX;
    assert_eq!(
        worker.request_displays(&session),
        Err(CaptureError::IdentityExhausted)
    );
    assert!(!worker.is_busy());
    harness.idle();
    worker.epoch = u64::MAX;
    assert_eq!(
        worker.synchronize(&session, &[]),
        Err(CaptureError::IdentityExhausted)
    );
    assert!(Arc::ptr_eq(&initial, &worker.snapshot()));
    harness.join(&mut worker);
}

#[test]
fn frozen_scene_from_another_session_is_rejected_before_source_or_backend_access() {
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    let mut foreign = Session::new();
    let live = foreign.preview();
    add(&mut foreign, live, "Foreign", binding("A", false, 2, 2));
    let mut controller = Controller::new();
    controller
        .set_studio(&mut foreign, true, MediaTime(0))
        .unwrap();
    let frozen = controller
        .sync(&mut foreign, MediaTime(0))
        .unwrap()
        .output
        .scenes()
        .remove(0);
    let session = Session::new();
    let before = worker.snapshot();
    assert_eq!(
        worker.synchronize(&session, &[frozen]),
        Err(CaptureError::ForeignSession)
    );
    assert!(Arc::ptr_eq(&before, &worker.snapshot()));
    harness.idle();
    harness.join(&mut worker);
}

#[test]
fn unrepresentable_capture_deadline_rejects_before_queue_serial_or_snapshot_changes() {
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    add(&mut session, live, "Display", binding("A", true, 2, 2));
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    let before = worker.snapshot();
    let serial = worker.serial;
    let epoch = worker.epoch;
    assert_eq!(
        worker.request_frame(&session, MediaTime(u64::MAX)),
        Err(CaptureError::TimeOverflow)
    );
    assert!(!worker.is_busy());
    assert_eq!(worker.serial, serial);
    assert_eq!(worker.epoch, epoch);
    assert!(Arc::ptr_eq(&before, &worker.snapshot()));
    harness.idle();
    worker.request_frame(&session, MediaTime(0)).unwrap();
    harness.frame().2.send(Ok(frame(1))).unwrap();
    settle(&mut worker, &session, 0);
    harness.join(&mut worker);
}

#[test]
fn visibility_scope_change_preserves_matching_common_frame_without_a_black_gap() {
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let (a, a_item) = add(&mut session, live, "A", binding("A", false, 2, 2));
    let (b, _) = add(&mut session, live, "B", binding("B", false, 2, 2));
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_frame(&session, MediaTime(0)).unwrap();
    harness.frame().2.send(Ok(frame(1))).unwrap();
    harness.frame().2.send(Ok(frame(2))).unwrap();
    settle(&mut worker, &session, 0);
    let before = worker.snapshot();
    session
        .apply(Command::SetVisible {
            scene: live,
            item: a_item,
            visible: false,
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    let after = worker.snapshot();
    assert!(after.frame(a).is_none());
    assert!(before.frame(b).unwrap().image.shares_pixels_with(&after.frame(b).unwrap().image));
    assert_ne!(before.epoch(), after.epoch());
    drop(before);
    drop(after);
    harness.idle();
    harness.join(&mut worker);
}

#[test]
fn pinned_snapshots_backpressure_capture_before_os_work_then_retirement_recovers() {
    let limits = CaptureLimits {
        max_sources: 1,
        max_frame_bytes: 16,
        max_total_frame_bytes: 16,
        ..CaptureLimits::default()
    };
    let (mut worker, harness) = Harness::new(limits);
    let mut session = Session::new();
    let live = session.preview();
    let (source, _) = add(&mut session, live, "Display", binding("A", false, 2, 2));
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    let tick = period(&session);
    let mut pinned = Vec::new();
    for index in 0..4u64 {
        assert!(
            worker
                .request_frame(&session, MediaTime(index * tick))
                .unwrap()
        );
        let (_, admission, reply) = harness.frame();
        assert_eq!(admission.max_frame_bytes, 16);
        reply.send(Ok(frame(index as u8 + 1))).unwrap();
        assert!(matches!(
            settle(&mut worker, &session, index * tick),
            Some(CaptureEvent::Frames { .. })
        ));
        pinned.push(worker.snapshot());
    }
    let fifth_time = 4 * tick;
    assert!(
        worker
            .request_frame(&session, MediaTime(fifth_time))
            .unwrap()
    );
    assert!(matches!(
        settle(&mut worker, &session, fifth_time),
        Some(CaptureEvent::Frames { .. })
    ));
    harness.idle();
    let blocked = worker.snapshot();
    assert_eq!(
        blocked.problem(source).unwrap().code,
        "capture_budget_exceeded"
    );
    assert!(blocked.frame(source).unwrap().image.shares_pixels_with(&pinned[3].frame(source).unwrap().image));
    assert_eq!(blocked.frame(source).unwrap().image.pixels()[0], 4);
    assert_eq!(
        worker.next_after_ns(MediaTime(fifth_time)),
        Some(1_000_000_000)
    );

    // Free one distinct old buffer, not the retained latest image. Admission
    // must reclaim on the worker before issuing the next native request.
    drop(pinned.remove(0));
    let retry_time = fifth_time + 1_000_000_000;
    assert!(
        worker
            .request_frame(&session, MediaTime(retry_time))
            .unwrap()
    );
    let (_, admission, reply) = harness.frame();
    assert_eq!(admission.max_frame_bytes, 16);
    reply.send(Ok(frame(9))).unwrap();
    settle(&mut worker, &session, retry_time);
    assert_eq!(
        worker.snapshot().frame(source).unwrap().image.pixels()[0],
        9
    );
    assert!(worker.snapshot().problem(source).is_none());
    drop(blocked);
    drop(pinned);
    harness.join(&mut worker);
}

#[test]
fn backward_capture_time_rejects_atomically_then_explicit_reset_rebases_to_zero() {
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let (source, _) = add(&mut session, live, "Display", binding("A", false, 2, 2));
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_frame(&session, MediaTime(100)).unwrap();
    harness.frame().2.send(Ok(frame(1))).unwrap();
    settle(&mut worker, &session, 100);
    let before = worker.snapshot();
    let serial = worker.serial;
    let epoch = worker.epoch;
    assert_eq!(worker.last_requested_at(), Some(MediaTime(100)));
    assert_eq!(
        worker.request_frame(&session, MediaTime(99)),
        Err(CaptureError::TimeDiscontinuity)
    );
    assert!(matches!(
        worker.poll(&session, MediaTime(99)),
        Err(CaptureError::TimeDiscontinuity)
    ));
    assert!(Arc::ptr_eq(&before, &worker.snapshot()));
    assert_eq!(worker.serial, serial);
    assert_eq!(worker.epoch, epoch);
    assert_eq!(worker.last_requested_at(), Some(MediaTime(100)));
    assert!(!worker.is_busy());
    harness.idle();

    worker.reset_timing().unwrap();
    assert_eq!(worker.epoch, epoch + 1);
    assert_eq!(worker.snapshot().epoch(), epoch + 1);
    assert_eq!(worker.snapshot().frame_count(), 0);
    assert_eq!(worker.last_requested_at(), None);
    assert_eq!(worker.serial, serial);
    harness.idle();
    assert!(worker.request_frame(&session, MediaTime(0)).unwrap());
    harness.frame().2.send(Ok(frame(9))).unwrap();
    settle(&mut worker, &session, 0);
    assert_eq!(
        worker.snapshot().frame(source).unwrap().image.pixels()[0],
        9
    );
    assert_eq!(worker.last_requested_at(), Some(MediaTime(0)));
    drop(before);
    harness.join(&mut worker);
}

#[test]
fn timing_reset_while_blocked_preserves_active_admission_and_discards_old_epoch() {
    let (mut worker, harness) = Harness::new(CaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let (source, _) = add(&mut session, live, "Display", binding("A", true, 2, 2));
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_frame(&session, MediaTime(100)).unwrap();
    let old_reply = harness.frame().2;
    let old_epoch = worker.epoch;
    let old_serial = worker.serial;
    worker.reset_timing().unwrap();
    let reset = worker.snapshot();
    assert_eq!(reset.epoch(), old_epoch + 1);
    assert_eq!(worker.last_requested_at(), None);
    assert_eq!(worker.serial, old_serial);
    assert!(worker.is_busy());
    assert_eq!(
        worker.request_frame(&session, MediaTime(0)),
        Err(CaptureError::Busy)
    );
    assert_eq!(worker.last_requested_at(), None);
    harness.idle();
    old_reply.send(Ok(frame(1))).unwrap();
    assert!(settle(&mut worker, &session, 0).is_none());
    assert!(!worker.is_busy());
    assert!(Arc::ptr_eq(&reset, &worker.snapshot()));
    assert_eq!(worker.snapshot().frame_count(), 0);
    assert!(worker.request_frame(&session, MediaTime(0)).unwrap());
    harness.frame().2.send(Ok(frame(7))).unwrap();
    settle(&mut worker, &session, 0);
    assert_eq!(
        worker.snapshot().frame(source).unwrap().image.pixels()[0],
        7
    );
    assert_eq!(
        worker.snapshot().frame(source).unwrap().requested_at,
        MediaTime(0)
    );
    assert_eq!(worker.last_requested_at(), Some(MediaTime(0)));
    drop(reset);
    harness.join(&mut worker);
}
