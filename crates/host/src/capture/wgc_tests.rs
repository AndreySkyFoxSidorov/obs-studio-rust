//! Fake-only WGC boundary/pixel tests; no OS factory or capture API is used.
use super::*;
use crate::window_capture::{WindowCaptureBackendKind, WindowCaptureOutcome, WindowFrameMetadata};
use skylive_core::{
    Command, Transform, VideoEffect, VideoFilter, WindowCaptureMethod, WindowMatchPriority,
    WindowTarget,
};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
struct Fake {
    queue: Arc<Mutex<VecDeque<Result<WindowCaptureOutcome, CaptureError>>>>,
    calls: Arc<AtomicUsize>,
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
        panic!("no enumeration requested")
    }
    fn capture(
        &mut self,
        _: &DisplayCaptureBinding,
        _: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError> {
        panic!("WGC is not a display/GDI alias")
    }
    fn capture_window_outcome(
        &mut self,
        _: &WindowCaptureBinding,
        _: CaptureLimits,
    ) -> Result<WindowCaptureOutcome, CaptureError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.queue
            .lock()
            .unwrap()
            .pop_front()
            .expect("bounded fixture response")
    }
}
fn binding(width: u32) -> WindowCaptureBinding {
    WindowCaptureBinding::new(
        WindowTarget::Windows {
            title: "Owned fake".into(),
            class: "FakeClass".into(),
            executable: "fake.exe".into(),
        },
        WindowMatchPriority::Class,
        false,
        false,
        width,
        2,
    )
    .unwrap()
    .with_method(WindowCaptureMethod::Wgc)
    .unwrap()
}
fn add(s: &mut Session, name: &str, b: WindowCaptureBinding) -> Id {
    let scene = s.preview();
    let item = s
        .apply(Command::AddWindowCaptureSource {
            scene,
            name: name.into(),
            binding: b,
        })
        .unwrap()
        .created
        .unwrap();
    s.snapshot()
        .scene(scene)
        .unwrap()
        .items()
        .iter()
        .find(|v| v.id() == item)
        .unwrap()
        .source()
}
fn worker(
    responses: Vec<Result<WindowCaptureOutcome, CaptureError>>,
) -> (CaptureWorker, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let queue = Arc::new(Mutex::new(responses.into()));
    (
        CaptureWorker::new(Box::new(Fake {
            queue,
            calls: calls.clone(),
        }))
        .unwrap(),
        calls,
    )
}
fn frame(generation: u64) -> Result<WindowCaptureOutcome, CaptureError> {
    Ok(WindowCaptureOutcome::Frame {
        image: RgbaFrame::solid(2, 2, [128, 128, 128, 128]).unwrap(),
        metadata: WindowFrameMetadata {
            backend: WindowCaptureBackendKind::Wgc,
            generation,
            encoding: crate::window_capture::WindowFrameEncoding::PremultipliedLinearSrgb,
            native_window: None,
        },
    })
}
fn capture(w: &mut CaptureWorker, s: &Session, at: u64) -> Arc<CaptureSnapshot> {
    w.synchronize(s, &[FrameScene::Live(s.preview())]).unwrap();
    assert!(w.request_frame(s, MediaTime(at)).unwrap());
    let deadline = Instant::now() + Duration::from_secs(3);
    while w.is_busy() {
        w.poll(s, MediaTime(at)).unwrap();
        assert!(Instant::now() < deadline);
        if w.is_busy() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    w.snapshot()
}
#[test]
fn pending_preserves_only_the_same_native_generation_and_error_clears_pixels() {
    let (mut w, calls) = worker(vec![
        frame(1),
        Ok(WindowCaptureOutcome::Pending { generation: 1 }),
        Ok(WindowCaptureOutcome::Pending { generation: 2 }),
        frame(2),
        Err(WindowCaptureError::TargetNotFound.into()),
    ]);
    let mut s = Session::new();
    let id = add(&mut s, "WGC", binding(2));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let first = capture(&mut w, &s, 0);
    let same = capture(&mut w, &s, 1_000_000_000);
    assert!(first.frame(id).unwrap().image.shares_pixels_with(&same.frame(id).unwrap().image));
    let replacement = capture(&mut w, &s, 2_000_000_000);
    assert!(replacement.frame(id).is_none());
    assert_eq!(
        replacement.problem(id).unwrap().code,
        "window_capture_frame_pending"
    );
    let current = capture(&mut w, &s, 3_000_000_000);
    assert_eq!(current.frame(id).unwrap().window.unwrap().generation, 2);
    let closed = capture(&mut w, &s, 4_000_000_000);
    assert!(closed.frame(id).is_none());
    assert_eq!(
        closed.problem(id).unwrap().code,
        "window_capture_target_not_found"
    );
    assert_eq!(
        first.frame(id).unwrap().window.unwrap().generation,
        1,
        "published Arc remains immutable"
    );
    drop((first, same, replacement, current, closed));
    w.join().unwrap();
}
#[test]
fn same_resource_different_basis_shares_one_native_admission_and_one_arc() {
    let (mut w, calls) = worker(vec![frame(7)]);
    let mut s = Session::new();
    let a = add(&mut s, "A", binding(2));
    let b = add(&mut s, "B", binding(4));
    let snapshot = capture(&mut w, &s, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(snapshot.frame(a).unwrap().image.shares_pixels_with(&snapshot.frame(b).unwrap().image));
    assert!(
        snapshot
            .frame(a)
            .unwrap()
            .binding
            .matches_source(s.snapshot().source(a).unwrap().kind())
    );
    assert!(
        snapshot
            .frame(b)
            .unwrap()
            .binding
            .matches_source(s.snapshot().source(b).unwrap().kind())
    );
    assert_ne!(
        snapshot.frame(a).unwrap().binding,
        snapshot.frame(b).unwrap().binding
    );
    drop(snapshot);
    w.join().unwrap();
}
#[test]
fn method_participates_in_resource_identity_but_dimensions_do_not() {
    assert_eq!(
        CaptureBinding::Window(binding(2)).resource_key(),
        CaptureBinding::Window(binding(4)).resource_key()
    );
    assert_ne!(
        CaptureBinding::Window(binding(2)).resource_key(),
        CaptureBinding::Window(binding(2).with_method(WindowCaptureMethod::Gdi).unwrap())
            .resource_key()
    );
}
#[test]
fn raw_premultiplied_bytes_survive_real_composition_and_color_correction() {
    let (mut w, _) = worker(vec![frame(9)]);
    let mut s = Session::new();
    let scene = s.preview();
    s.apply(Command::SetVideo {
        width: 16,
        height: 16,
        fps_num: 30,
        fps_den: 1,
    })
    .unwrap();
    let source = add(&mut s, "WGC alpha", binding(2));
    let item = s.snapshot().scene(scene).unwrap().items()[0].id();
    s.apply(Command::SetTransform {
        scene,
        item,
        transform: Transform::full(16, 16),
    })
    .unwrap();
    let snapshot = capture(&mut w, &s, 0);
    let render = |s: &Session| {
        crate::preview::render_output_with_capture(
            &s.snapshot(),
            &crate::transition::FrameComposition::Scene(FrameScene::Live(scene)),
            16,
            16,
            &crate::playback::PlaybackSnapshot::at_time(s, MediaTime(0)),
            &snapshot,
        )
        .unwrap()
    };
    let bare = render(&s);
    // OBS source equation: decode(128) + black*(1-128/255), then sRGB encode.
    // Straight-alpha would produce about92; double encoded premultiply about64.
    assert!(
        bare.pixels()
            .chunks_exact(4)
            .all(|p| p == [128, 128, 128, 255])
    );
    s.apply(Command::SetSourceVideoFilters {
        source,
        filters: vec![VideoFilter::new(VideoEffect::color_correction()).unwrap()],
    })
    .unwrap();
    assert_eq!(render(&s).pixels(), bare.pixels());
    drop(snapshot);
    w.join().unwrap();
}
#[test]
fn stale_owner_epoch_completion_is_discarded_before_publication() {
    // Unread completion counts as busy; resetting freshness cannot release it.
    let (mut w, _) = worker(vec![frame(1), frame(2)]);
    let mut s = Session::new();
    let id = add(&mut s, "WGC", binding(2));
    w.synchronize(&s, &[FrameScene::Live(s.preview())]).unwrap();
    w.request_frame(&s, MediaTime(0)).unwrap();
    w.reset_timing().unwrap();
    assert!(w.is_busy());
    let deadline = Instant::now() + Duration::from_secs(3);
    while w.is_busy() {
        w.poll(&s, MediaTime(0)).unwrap();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(w.snapshot().frame(id).is_none());
    let current = capture(&mut w, &s, 0);
    assert_eq!(current.frame(id).unwrap().window.unwrap().generation, 2);
    drop(current);
    w.join().unwrap();
}

#[test]
fn auto_backend_switch_never_reuses_gdi_generation_one_as_wgc_pending() {
    let gdi = Ok(WindowCaptureOutcome::Frame {
        image: RgbaFrame::solid(2, 2, [33, 66, 99, 255]).unwrap(),
        metadata: WindowFrameMetadata {
            backend: WindowCaptureBackendKind::Gdi,
            generation: 1,
            encoding: crate::window_capture::WindowFrameEncoding::OpaqueSrgb,
            native_window: None,
        },
    });
    let (mut w, calls) = worker(vec![
        gdi,
        Ok(WindowCaptureOutcome::Pending { generation: 1 }),
        frame(1),
    ]);
    let mut s = Session::new();
    let id = add(
        &mut s,
        "Automatic backend replacement",
        binding(2).with_method(WindowCaptureMethod::Auto).unwrap(),
    );
    let old = capture(&mut w, &s, 0);
    assert_eq!(
        old.frame(id).unwrap().window.unwrap().backend,
        WindowCaptureBackendKind::Gdi
    );
    let pending = capture(&mut w, &s, 1_000_000_000);
    assert!(
        pending.frame(id).is_none(),
        "GDI and WGC generation1 are different native identities"
    );
    assert_eq!(
        pending.problem(id).unwrap().code,
        "window_capture_frame_pending"
    );
    let current = capture(&mut w, &s, 2_000_000_000);
    assert_eq!(
        current.frame(id).unwrap().window.unwrap().backend,
        WindowCaptureBackendKind::Wgc
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        old.frame(id).unwrap().image.pixels()[0],
        33,
        "previous immutable snapshot remains valid for existing readers"
    );
    drop((old, pending, current));
    w.join().unwrap();
}
