//! Fake-only shared-owner acceptance. Never constructs a native adapter.
use super::*;
use skylive_core::{Command, WindowMatchPriority, WindowTarget};
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

struct Fake {
    displays: Arc<AtomicUsize>,
    windows: Arc<AtomicUsize>,
    enumerations: Arc<AtomicUsize>,
}
impl CaptureBackend for Fake {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            available: true,
            backend: "fake",
            cursor: true,
            hdr: false,
            note: None,
        }
    }
    fn enumerate(&mut self, _: CaptureLimits) -> Result<Vec<DisplayInfo>, CaptureError> {
        self.enumerations.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    }
    fn enumerate_windows(&mut self, _: CaptureLimits) -> Result<Vec<WindowInfo>, CaptureError> {
        self.enumerations.fetch_add(1, Ordering::SeqCst);
        Ok(vec![WindowInfo {
            full_window_size: None,
            target: target("Editor"),
            name: "Editor".into(),
            width: 2,
            height: 2,
            minimized: false,
        }])
    }
    fn capture(
        &mut self,
        _: &DisplayCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError> {
        assert!(limits.max_frame_bytes >= 16);
        self.displays.fetch_add(1, Ordering::SeqCst);
        Ok(RgbaFrame::solid(2, 2, [255, 0, 0, 255]).unwrap())
    }
    fn capture_window(
        &mut self,
        _: &WindowCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError> {
        assert!(limits.max_frame_bytes >= 16);
        self.windows.fetch_add(1, Ordering::SeqCst);
        Ok(RgbaFrame::solid(2, 2, [0, 255, 0, 255]).unwrap())
    }
}
fn target(title: &str) -> WindowTarget {
    WindowTarget::Windows {
        title: title.into(),
        class: "EditorClass".into(),
        executable: "editor.exe".into(),
    }
}
fn binding(title: &str, width: u32) -> WindowCaptureBinding {
    WindowCaptureBinding::new(
        target(title),
        WindowMatchPriority::Class,
        true,
        true,
        width,
        2,
    )
    .unwrap()
}
fn add(session: &mut Session, name: &str, binding: WindowCaptureBinding) -> Id {
    let scene = session.preview();
    let item = session
        .apply(Command::AddWindowCaptureSource {
            scene,
            name: name.into(),
            binding,
        })
        .unwrap()
        .created
        .unwrap();
    session
        .snapshot()
        .scene(scene)
        .unwrap()
        .items()
        .iter()
        .find(|i| i.id() == item)
        .unwrap()
        .source()
}
fn settle(worker: &mut CaptureWorker, session: &Session) -> Option<CaptureEvent> {
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        let event = worker.poll(session, MediaTime(0)).unwrap();
        if !worker.is_busy() {
            return event;
        }
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn display_and_window_share_one_admission_budget_and_window_basis_deduplicates() {
    let displays = Arc::new(AtomicUsize::new(0));
    let windows = Arc::new(AtomicUsize::new(0));
    let enumerations = Arc::new(AtomicUsize::new(0));
    let mut worker = CaptureWorker::with_limits(
        Box::new(Fake {
            displays: displays.clone(),
            windows: windows.clone(),
            enumerations: enumerations.clone(),
        }),
        CaptureLimits {
            max_frame_bytes: 16,
            max_total_frame_bytes: 32,
            max_raw_bytes: 64,
            ..CaptureLimits::default()
        },
    )
    .unwrap();
    let mut session = Session::new();
    let a = add(&mut session, "Window A", binding("Editor", 2));
    let b = add(&mut session, "Same window new basis", binding("Editor", 4));
    let scene = session.preview();
    let display_item = session
        .apply(Command::AddDisplayCaptureSource {
            scene,
            name: "Display".into(),
            binding: DisplayCaptureBinding::new(
                DisplayTarget::Windows {
                    device_id: "display".into(),
                },
                false,
                2,
                2,
            )
            .unwrap(),
        })
        .unwrap()
        .created
        .unwrap();
    let display = session
        .snapshot()
        .scene(scene)
        .unwrap()
        .items()
        .iter()
        .find(|i| i.id() == display_item)
        .unwrap()
        .source();
    let last = add(&mut session, "Different window", binding("Other", 2));
    worker
        .synchronize(&session, &[FrameScene::Live(scene)])
        .unwrap();
    assert_eq!(windows.load(Ordering::SeqCst), 0);
    assert_eq!(enumerations.load(Ordering::SeqCst), 0);
    worker.request_frame(&session, MediaTime(0)).unwrap();
    settle(&mut worker, &session);
    let snapshot = worker.snapshot();
    assert_eq!(windows.load(Ordering::SeqCst), 1);
    assert_eq!(displays.load(Ordering::SeqCst), 1);
    assert!(snapshot.frame(a).unwrap().image.shares_pixels_with(&snapshot.frame(b).unwrap().image));
    assert!(snapshot.frame(display).is_some());
    assert!(snapshot.frame(last).is_none());
    assert_eq!(
        snapshot.problem(last).unwrap().code,
        "capture_budget_exceeded"
    );
    assert!(
        snapshot
            .frame(a)
            .unwrap()
            .binding
            .matches_source(session.snapshot().source(a).unwrap().kind())
    );
    drop(snapshot);
    worker.join().unwrap();
}
#[test]
fn window_enumeration_uses_same_unread_work_slot_and_revision_fence() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut worker = CaptureWorker::new(Box::new(Fake {
        displays: count.clone(),
        windows: count.clone(),
        enumerations: count.clone(),
    }))
    .unwrap();
    let mut session = Session::new();
    let key = worker.request_windows(&session).unwrap();
    assert_eq!(worker.request_displays(&session), Err(CaptureError::Busy));
    session
        .apply(Command::AddScene {
            name: "changed".into(),
        })
        .unwrap();
    assert!(settle(&mut worker, &session).is_none());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    worker.request_windows(&session).unwrap();
    match settle(&mut worker, &session).unwrap() {
        CaptureEvent::Windows { key: new, result } => {
            assert_ne!(new.revision, key.revision);
            assert_eq!(result.unwrap()[0].target, target("Editor"));
        }
        _ => panic!("window catalog"),
    };
    worker.join().unwrap();
}
#[test]
fn catalog_rejects_unknown_handles_oversize_metadata_and_invalid_native_bindings() {
    let mut info = WindowInfo {
        full_window_size: None,
        target: target("Editor"),
        name: "Editor".into(),
        width: 2,
        height: 2,
        minimized: false,
    };
    assert!(validate_windows(vec![info.clone()]).is_ok());
    info.width = 0;
    assert!(validate_windows(vec![info.clone()]).is_err());
    info.width = 2;
    info.name = "x".repeat(2077);
    assert!(validate_windows(vec![info]).is_err());
}
