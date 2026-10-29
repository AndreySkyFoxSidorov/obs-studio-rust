//! Real typed worker outcomes with a controlled backend; no native calls.
use super::*;
use skylive_core::{Command, DisplayCaptureMethod};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
enum Step {
    Frame(u8),
    Wait,
    Blocked(mpsc::Receiver<()>),
}
#[derive(Default)]
struct State {
    steps: Mutex<VecDeque<Step>>,
    calls: AtomicUsize,
    retained: Mutex<Vec<(u64, u64, usize, thread::ThreadId)>>,
    dropped: Mutex<Option<thread::ThreadId>>,
}
struct Fake(Arc<State>);
impl Drop for Fake {
    fn drop(&mut self) {
        *self.0.dropped.lock().unwrap() = Some(thread::current().id());
    }
}
impl CaptureBackend for Fake {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            available: true,
            backend: "typed display fixture",
            cursor: false,
            hdr: false,
            note: None,
        }
    }
    fn enumerate(&mut self, _: CaptureLimits) -> Result<Vec<DisplayInfo>, CaptureError> {
        panic!("these tests never enumerate displays")
    }
    fn capture(
        &mut self,
        _: &DisplayCaptureBinding,
        _: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError> {
        panic!("typed outcome must be used")
    }
    fn retain_display_bindings(
        &mut self,
        session: u64,
        epoch: u64,
        bindings: &[DisplayCaptureBinding],
    ) {
        self.0.retained.lock().unwrap().push((
            session,
            epoch,
            bindings.len(),
            thread::current().id(),
        ));
    }
    fn capture_display(
        &mut self,
        binding: &DisplayCaptureBinding,
        _: CaptureLimits,
    ) -> Result<DisplayCaptureOutcome, CaptureError> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        let step = self
            .0
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("one controlled step per acquisition");
        let color = match step {
            Step::Wait => return Ok(DisplayCaptureOutcome::NoNewFrame),
            Step::Frame(color) => color,
            Step::Blocked(receiver) => {
                receiver.recv_timeout(Duration::from_secs(3)).unwrap();
                17
            }
        };
        Ok(DisplayCaptureOutcome::Frame {
            image: RgbaFrame::solid(2, 2, [color, 0, 0, 255]).unwrap(),
            metadata: Some(DisplayFrameMetadata {
                backend: if binding.method() == DisplayCaptureMethod::Gdi {
                    DisplayCaptureBackend::Gdi
                } else {
                    DisplayCaptureBackend::Dxgi
                },
                generation: None,
                fallback_reason: None,
            }),
        })
    }
}
fn binding(method: DisplayCaptureMethod) -> DisplayCaptureBinding {
    DisplayCaptureBinding::new(
        DisplayTarget::Windows {
            device_id: "fixture-output".into(),
        },
        false,
        2,
        2,
    )
    .unwrap()
    .with_method(method)
    .unwrap()
}
fn fixture(method: DisplayCaptureMethod) -> (Session, Id, Id, CaptureWorker, Arc<State>) {
    let mut session = Session::new();
    let scene = session.preview();
    let item = session
        .apply(Command::AddDisplayCaptureSource {
            scene,
            name: "Display".into(),
            binding: binding(method),
        })
        .unwrap()
        .created
        .unwrap();
    let source = session.snapshot().scene(scene).unwrap().items()[0].source();
    let state = Arc::new(State::default());
    let mut worker = CaptureWorker::new(Box::new(Fake(state.clone()))).unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(scene)])
        .unwrap();
    (session, item, source, worker, state)
}
fn settle(worker: &mut CaptureWorker, session: &Session, now: u64) -> Option<CaptureEvent> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let event = worker.poll(session, MediaTime(now)).unwrap();
        if !worker.is_busy() {
            return event;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
}
fn run(worker: &mut CaptureWorker, session: &Session, now: u64) -> Option<CaptureEvent> {
    assert!(worker.request_frame(session, MediaTime(now)).unwrap());
    settle(worker, session, now)
}
#[test]
fn no_new_frame_preserves_image_sequence_generation_and_normal_cadence_then_retires_on_owner() {
    let (mut session, item, source, mut worker, state) = fixture(DisplayCaptureMethod::Dxgi);
    state
        .steps
        .lock()
        .unwrap()
        .extend([Step::Frame(44), Step::Wait, Step::Wait]);
    run(&mut worker, &session, 0);
    let first = worker.snapshot();
    let sequence = first.frame(source).unwrap().sequence;
    assert_eq!(
        first
            .frame(source)
            .unwrap()
            .display
            .as_ref()
            .unwrap()
            .backend,
        DisplayCaptureBackend::Dxgi
    );
    assert!(run(&mut worker, &session, 40_000_000).is_none());
    let second = worker.snapshot();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(second.frame(source).unwrap().sequence, sequence);
    assert_eq!(second.generation(), first.generation());
    assert!(worker.next_after_ns(MediaTime(40_000_000)).unwrap() > 1_000_000);
    assert!(run(&mut worker, &session, 80_000_000).is_none());
    session
        .apply(Command::RemoveItem {
            scene: session.preview(),
            item,
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(session.preview())])
        .unwrap();
    run(&mut worker, &session, 120_000_000);
    let retained = state.retained.lock().unwrap();
    assert_eq!(retained.last().unwrap().2, 0);
    assert!(retained.iter().all(|row| row.3 != thread::current().id()));
    drop(retained);
    assert_eq!(state.calls.load(Ordering::SeqCst), 3);
    drop(second);
    drop(first);
    worker.join().unwrap();
    assert_ne!(
        state.dropped.lock().unwrap().unwrap(),
        thread::current().id()
    );
}
#[test]
fn initial_wait_has_no_fake_pixels_or_frame_generation() {
    let (session, _, source, mut worker, state) = fixture(DisplayCaptureMethod::Dxgi);
    state.steps.lock().unwrap().extend([Step::Wait, Step::Wait]);
    let generation = worker.snapshot().generation();
    run(&mut worker, &session, 0);
    let snapshot = worker.snapshot();
    assert!(snapshot.frame(source).is_none());
    assert_eq!(snapshot.generation(), generation);
    assert!(
        snapshot
            .problems()
            .iter()
            .any(|problem| problem.code == "capture_frame_unavailable")
    );
    assert!(run(&mut worker, &session, 40_000_000).is_none());
    drop(snapshot);
    worker.join().unwrap();
}
#[test]
fn method_switch_rejects_old_pending_completion_and_wait_cannot_resurrect_old_binding() {
    let (mut session, _, source, mut worker, state) = fixture(DisplayCaptureMethod::Gdi);
    let (release, wait) = mpsc::sync_channel(1);
    state
        .steps
        .lock()
        .unwrap()
        .extend([Step::Blocked(wait), Step::Wait, Step::Frame(99)]);
    assert!(worker.request_frame(&session, MediaTime(0)).unwrap());
    let deadline = Instant::now() + Duration::from_secs(3);
    while state.calls.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    session
        .apply(Command::RebindDisplayCapture {
            source,
            binding: binding(DisplayCaptureMethod::Dxgi),
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(session.preview())])
        .unwrap();
    let epoch = worker.snapshot().epoch();
    release.send(()).unwrap();
    assert!(settle(&mut worker, &session, 0).is_none());
    assert!(worker.snapshot().frame(source).is_none());
    run(&mut worker, &session, 40_000_000);
    assert!(worker.snapshot().frame(source).is_none());
    run(&mut worker, &session, 80_000_000);
    let snapshot = worker.snapshot();
    let frame = snapshot.frame(source).unwrap();
    assert_eq!(frame.image.pixels(), &[99, 0, 0, 255].repeat(4));
    assert_eq!(
        frame.display.as_ref().unwrap().backend,
        DisplayCaptureBackend::Dxgi
    );
    assert_eq!(state.retained.lock().unwrap().last().unwrap().1, epoch);
    drop(snapshot);
    worker.join().unwrap();
}
#[test]
fn resource_dedup_keeps_method_identity_and_close_while_pending_drops_backend_on_worker() {
    let (mut session, _, _, mut worker, state) = fixture(DisplayCaptureMethod::Gdi);
    let scene = session.preview();
    session
        .apply(Command::AddDisplayCaptureSource {
            scene,
            name: "Same target different method".into(),
            binding: binding(DisplayCaptureMethod::Dxgi),
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(scene)])
        .unwrap();
    state
        .steps
        .lock()
        .unwrap()
        .extend([Step::Frame(22), Step::Frame(77)]);
    run(&mut worker, &session, 0);
    assert_eq!(state.calls.load(Ordering::SeqCst), 2);
    assert_eq!(worker.snapshot().frames().count(), 2);
    let (release, wait) = mpsc::sync_channel(1);
    state
        .steps
        .lock()
        .unwrap()
        .extend([Step::Blocked(wait), Step::Wait]);
    assert!(
        worker
            .request_frame(&session, MediaTime(40_000_000))
            .unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while state.calls.load(Ordering::SeqCst) < 3 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    worker.close();
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_ne!(
        state.dropped.lock().unwrap().unwrap(),
        thread::current().id()
    );
}
