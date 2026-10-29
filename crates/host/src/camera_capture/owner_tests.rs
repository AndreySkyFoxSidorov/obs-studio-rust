//! Controlled CPU/native-port fixtures. No native backend factory is used.
use super::*;
use std::{
    rc::Rc,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
#[derive(Clone)]
struct State {
    factories: Arc<AtomicUsize>,
    opens: Arc<AtomicUsize>,
    stops: Arc<AtomicUsize>,
    frames: Arc<AtomicUsize>,
    stop_ready: Arc<AtomicBool>,
    dropped: mpsc::SyncSender<thread::ThreadId>,
}
struct Backend {
    state: State,
    _local: Rc<()>,
}
impl Drop for Backend {
    fn drop(&mut self) {
        let _ = self.state.dropped.send(thread::current().id());
    }
}
struct Stream {
    state: State,
    _local: Rc<()>,
}
impl NativeCameraBackend for Backend {
    fn enumerate_devices(&mut self, _: CameraLimits) -> Result<Vec<CameraDeviceInfo>, CameraError> {
        Ok(vec![CameraDeviceInfo {
            target: target("camera"),
            name: "Fixture camera".into(),
        }])
    }
    fn enumerate_modes(
        &mut self,
        _: &VideoCaptureTarget,
        _: CameraLimits,
    ) -> Result<Vec<CameraModeInfo>, CameraError> {
        Ok(vec![CameraModeInfo {
            width: 2,
            height: 2,
            fps_num: 30,
            fps_den: 1,
            native_subtype: "RGB32".into(),
            mode: Some(mode(2)),
            unsupported_reason: None,
        }])
    }
    fn open(
        &mut self,
        _: &VideoCaptureBinding,
        _: CameraLimits,
    ) -> Result<Box<dyn NativeCameraStream>, CameraError> {
        self.state.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Stream {
            state: self.state.clone(),
            _local: Rc::new(()),
        }))
    }
}
impl NativeCameraStream for Stream {
    fn poll(&mut self, _: CameraLimits) -> Result<Option<NativeVideoFrame>, CameraError> {
        if self
            .state
            .frames
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_err()
        {
            return Ok(None);
        }
        Ok(Some(NativeVideoFrame {
            image: RgbaFrame::solid(2, 2, [5, 20, 200, 255]).unwrap(),
            presentation_ns: 123400,
            device_qpc_ns: None,
            directshow_clock: None,
            discontinuity: false,
            dropped_frames: 0,
        }))
    }
    fn stop(&mut self) -> Result<bool, CameraError> {
        self.state.stops.fetch_add(1, Ordering::SeqCst);
        Ok(self.state.stop_ready.load(Ordering::SeqCst))
    }
}
fn target(name: &str) -> VideoCaptureTarget {
    VideoCaptureTarget::Windows {
        symbolic_link: name.into(),
    }
}
fn mode(width: u32) -> VideoCaptureMode {
    VideoCaptureMode::new(width, 2, 30, 1, skylive_core::VideoPixelFormat::Rgb32).unwrap()
}
fn binding(name: &str) -> VideoCaptureBinding {
    VideoCaptureBinding::new(target(name), mode(2), false).unwrap()
}
fn fixture(limits: CameraLimits) -> (CameraOwner, State, mpsc::Receiver<thread::ThreadId>) {
    let (dropped, receiver) = mpsc::sync_channel(1);
    let state = State {
        factories: Arc::new(AtomicUsize::new(0)),
        opens: Arc::new(AtomicUsize::new(0)),
        stops: Arc::new(AtomicUsize::new(0)),
        frames: Arc::new(AtomicUsize::new(1)),
        stop_ready: Arc::new(AtomicBool::new(true)),
        dropped,
    };
    let native = state.clone();
    let owner = CameraOwner::new(
        Box::new(move || {
            native.factories.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Backend {
                state: native,
                _local: Rc::new(()),
            }))
        }),
        CameraCapabilities {
            available: true,
            backend: "fake-only",
            video: true,
            audio: false,
            note: None,
        },
        limits,
    )
    .unwrap();
    (owner, state, receiver)
}
fn wait(owner: &mut CameraOwner, session: &Session, done: impl Fn(&CameraOwner) -> bool) {
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        let _ = owner.poll(session).unwrap();
        if done(owner) {
            return;
        }
        assert!(Instant::now() < end, "camera fake deadline");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn authoring_and_synchronization_are_inert_shared_bindings_open_once_and_frames_share_arc() {
    let (mut owner, state, dropped) = fixture(CameraLimits::default());
    let session = Session::new();
    owner
        .synchronize_bindings(
            &session,
            vec![
                (Id::new(2), binding("camera")),
                (Id::new(3), binding("camera")),
            ],
        )
        .unwrap();
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    assert_eq!(state.opens.load(Ordering::SeqCst), 0);
    owner.request_devices(&session).unwrap();
    wait(&mut owner, &session, |o| !o.is_busy());
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    assert_eq!(state.opens.load(Ordering::SeqCst), 0);
    owner.request_frame(&session, MediaTime(0)).unwrap();
    wait(&mut owner, &session, |o| {
        o.snapshot().frame(Id::new(2)).is_some()
    });
    let snapshot = owner.snapshot();
    assert_eq!(snapshot.instance(), owner.instance());
    assert!(snapshot.frame(Id::new(2)).unwrap().image.shares_pixels_with(
        &snapshot.frame(Id::new(3)).unwrap().image
    ));
    assert_eq!(snapshot.frame(Id::new(2)).unwrap().presentation_ns, 123400);
    assert_eq!(state.opens.load(Ordering::SeqCst), 1);
    assert!(!owner.request_frame(&session, MediaTime(1)).unwrap());
    drop(snapshot);
    owner.join().unwrap();
    assert_ne!(
        dropped.recv_timeout(Duration::from_secs(1)).unwrap(),
        thread::current().id()
    );
}

#[test]
fn hardware_count_conflicts_and_time_overflow_reject_before_native_factory() {
    let (mut owner, state, _) = fixture(CameraLimits {
        max_streams: 1,
        ..CameraLimits::default()
    });
    let session = Session::new();
    owner
        .synchronize_bindings(
            &session,
            vec![(Id::new(2), binding("a")), (Id::new(3), binding("b"))],
        )
        .unwrap();
    let before = owner.snapshot();
    assert!(matches!(
        owner.request_frame(&session, MediaTime(0)),
        Err(CameraError::BudgetExceeded(_))
    ));
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    assert_eq!(state.factories.load(Ordering::SeqCst), 0);
    owner
        .synchronize_bindings(&session, vec![(Id::new(2), binding("a"))])
        .unwrap();
    let before = owner.snapshot();
    assert_eq!(
        owner.request_frame(&session, MediaTime(u64::MAX)),
        Err(CameraError::TimeOverflow)
    );
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    assert_eq!(state.opens.load(Ordering::SeqCst), 0);
    let mut different = binding("a");
    different = VideoCaptureBinding::new(different.target().clone(), mode(4), false).unwrap();
    owner
        .synchronize_bindings(
            &session,
            vec![(Id::new(2), binding("a")), (Id::new(3), different)],
        )
        .unwrap();
    assert_eq!(
        owner.request_frame(&session, MediaTime(0)),
        Err(CameraError::ConflictingModes)
    );
    owner.join().unwrap();
}

#[test]
fn stop_retains_capacity_until_actual_ack_and_final_native_drop_stays_off_frontend() {
    let (mut owner, state, dropped) = fixture(CameraLimits::default());
    let session = Session::new();
    owner
        .synchronize_bindings(&session, vec![(Id::new(2), binding("a"))])
        .unwrap();
    owner.request_frame(&session, MediaTime(0)).unwrap();
    wait(&mut owner, &session, |o| {
        o.snapshot().frame(Id::new(2)).is_some()
    });
    let pinned = owner.snapshot();
    state.stop_ready.store(false, Ordering::SeqCst);
    owner.request_stop(&session).unwrap();
    let end = Instant::now() + Duration::from_secs(1);
    while state.stops.load(Ordering::SeqCst) == 0 {
        let _ = owner.poll(&session).unwrap();
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(owner.is_busy());
    assert_eq!(
        owner.request_frame(&session, MediaTime(1)),
        Err(CameraError::Busy)
    );
    assert_eq!(state.opens.load(Ordering::SeqCst), 1);
    state.stop_ready.store(true, Ordering::SeqCst);
    wait(&mut owner, &session, |o| !o.is_busy());
    owner.close();
    assert!(matches!(dropped.try_recv(), Err(mpsc::TryRecvError::Empty)));
    drop(pinned);
    assert_ne!(
        dropped.recv_timeout(Duration::from_secs(2)).unwrap(),
        thread::current().id()
    );
    owner.join().unwrap();
}

#[test]
fn session_replacement_discards_old_outputs_and_disarms_before_new_capture() {
    let (mut owner, state, _) = fixture(CameraLimits::default());
    let first = Session::new();
    owner
        .synchronize_bindings(&first, vec![(Id::new(2), binding("a"))])
        .unwrap();
    owner.request_frame(&first, MediaTime(0)).unwrap();
    wait(&mut owner, &first, |o| {
        o.snapshot().frame(Id::new(2)).is_some()
    });
    let second = Session::new();
    owner
        .synchronize_bindings(&second, vec![(Id::new(2), binding("b"))])
        .unwrap();
    assert!(!owner.is_active());
    assert!(owner.snapshot().frame(Id::new(2)).is_none());
    assert_eq!(
        owner.request_frame(&second, MediaTime(0)),
        Err(CameraError::Busy)
    );
    wait(&mut owner, &second, |o| !o.is_busy() && !o.stop_pending);
    assert_eq!(state.opens.load(Ordering::SeqCst), 1);
    owner.join().unwrap();
}

#[test]
fn pinned_output_budget_blocks_conversion_before_poll_then_recovers_without_reopening() {
    let (mut owner, state, _) = fixture(CameraLimits {
        max_frame_bytes: 16,
        max_total_frame_bytes: 16,
        max_raw_bytes: 1536,
        ..CameraLimits::default()
    });
    let session = Session::new();
    owner
        .synchronize_bindings(&session, vec![(Id::new(2), binding("a"))])
        .unwrap();
    owner.request_frame(&session, MediaTime(0)).unwrap();
    let mut pinned = Vec::new();
    for sequence in 1..=4 {
        if sequence != 1 {
            state.frames.fetch_add(1, Ordering::SeqCst);
        }
        wait(&mut owner, &session, |o| {
            o.snapshot()
                .frame(Id::new(2))
                .is_some_and(|f| f.sequence == sequence)
        });
        pinned.push(owner.snapshot());
    }
    state.frames.fetch_add(1, Ordering::SeqCst);
    wait(&mut owner, &session, |o| {
        o.snapshot()
            .problems()
            .iter()
            .any(|p| p.code == "camera_budget_exceeded")
    });
    assert_eq!(
        state.frames.load(Ordering::SeqCst),
        1,
        "native conversion must not run without resident admission"
    );
    assert_eq!(state.opens.load(Ordering::SeqCst), 1);
    drop(pinned.remove(0));
    wait(&mut owner, &session, |o| {
        o.snapshot()
            .frame(Id::new(2))
            .is_some_and(|f| f.sequence == 5)
            && o.snapshot().problems().is_empty()
    });
    assert_eq!(state.frames.load(Ordering::SeqCst), 0);
    drop(pinned);
    owner.join().unwrap();
}
