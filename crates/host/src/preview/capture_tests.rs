//! Capture evaluation through a real bounded worker and an explicitly injected
//! deterministic backend. No native backend, enumeration or OS capture occurs.
use super::*;
use crate::{
    capture::{
        CaptureBackend, CaptureCapabilities, CaptureError, CaptureLimits, CaptureSnapshot,
        CaptureWorker, DisplayInfo, prepare_transform_command,
    },
    playback::PlaybackClock,
    transition::Controller,
};
use skylive_core::{
    Command, Crop, DisplayCaptureBinding, DisplayTarget, Transform, TransformAction,
};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const R: [u8; 4] = [255, 0, 0, 255];
const G: [u8; 4] = [0, 255, 0, 255];
const B: [u8; 4] = [0, 0, 255, 255];
const W: [u8; 4] = [255; 4];
const K: [u8; 4] = [0, 0, 0, 255];
fn image(width: u32, height: u32, pixels: &[[u8; 4]]) -> RgbaFrame {
    RgbaFrame::new(width, height, pixels.iter().flatten().copied().collect()).unwrap()
}
fn pattern() -> RgbaFrame {
    image(2, 2, &[R, G, B, W])
}
fn wide() -> RgbaFrame {
    image(
        4,
        2,
        &[
            R,
            G,
            B,
            W,
            [255, 255, 0, 255],
            [0, 255, 255, 255],
            [255, 0, 255, 255],
            [128, 128, 128, 255],
        ],
    )
}
fn binding(width: u32, height: u32) -> DisplayCaptureBinding {
    DisplayCaptureBinding::new(
        DisplayTarget::Windows {
            device_id: "mock-monitor-id".into(),
        },
        true,
        width,
        height,
    )
    .unwrap()
}

struct FakeCaptureBackend {
    frames: Arc<Mutex<VecDeque<Result<RgbaFrame, CaptureError>>>>,
    calls: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
}
impl CaptureBackend for FakeCaptureBackend {
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            available: true,
            backend: "queued-test",
            cursor: true,
            hdr: false,
            note: None,
        }
    }
    fn enumerate(&mut self, _: CaptureLimits) -> Result<Vec<DisplayInfo>, CaptureError> {
        Ok(Vec::new())
    }
    fn capture(
        &mut self,
        _: &DisplayCaptureBinding,
        _: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.frames.lock().unwrap().pop_front().unwrap_or_else(|| {
            Err(CaptureError::Backend(
                "test frame schedule exhausted".into(),
            ))
        })
    }
}
impl Drop for FakeCaptureBackend {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
pub(super) fn worker(
    frames: Vec<RgbaFrame>,
    limits: CaptureLimits,
) -> (CaptureWorker, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let backend = FakeCaptureBackend {
        frames: Arc::new(Mutex::new(frames.into_iter().map(Ok).collect())),
        calls: calls.clone(),
        dropped: dropped.clone(),
    };
    (
        CaptureWorker::with_limits(Box::new(backend), limits).unwrap(),
        calls,
        dropped,
    )
}
pub(super) fn fixture(basis: (u32, u32), transform: Transform) -> (Session, Id, Id, Id) {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    let item = session
        .apply(Command::AddDisplayCaptureSource {
            scene,
            name: "Display".into(),
            binding: binding(basis.0, basis.1),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::SetTransform {
            scene,
            item,
            transform,
        })
        .unwrap();
    let source = session.snapshot().scene(scene).unwrap().items()[0].source();
    (session, scene, item, source)
}
pub(super) fn acquire(
    worker: &mut CaptureWorker,
    session: &Session,
    views: &[FrameScene],
    now: u64,
) -> Arc<CaptureSnapshot> {
    worker.synchronize(session, views).unwrap();
    assert!(worker.request_frame(session, MediaTime(now)).unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while worker.is_busy() {
        worker.poll(session, MediaTime(now)).unwrap();
        assert!(Instant::now() < deadline, "capture test worker deadline");
        if worker.is_busy() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    worker.snapshot()
}
fn render_live(
    session: &Session,
    scene: Id,
    capture: &CaptureSnapshot,
    width: u32,
    height: u32,
) -> RgbaFrame {
    render_output_with_capture(
        &session.snapshot(),
        &FrameComposition::Scene(FrameScene::Live(scene)),
        width,
        height,
        &PlaybackSnapshot::at_time(session, MediaTime(0)),
        capture,
    )
    .unwrap()
}
fn assert_pixels(image: &RgbaFrame, width: u32, height: u32, expected: &[[u8; 4]]) {
    assert_eq!((image.width(), image.height()), (width, height));
    assert_eq!(expected.len(), (width * height) as usize);
    assert_eq!(
        image.pixels(),
        expected.iter().flatten().copied().collect::<Vec<_>>()
    );
}
fn state(
    session: &mut Session,
    owner: &mut Controller,
    clock: &mut PlaybackClock,
    now: u64,
) -> (FrameComposition, PlaybackSnapshot, Vec<FrameScene>) {
    let state = owner.sync(session, MediaTime(now)).unwrap();
    let mut views = state.output.scenes();
    if state.studio {
        views.push(FrameScene::Live(session.preview()));
    }
    let sample = clock
        .update_outputs(session, &views, MediaTime(now))
        .unwrap();
    (state.output, sample, views)
}
fn completion(
    worker: &mut PreviewWorker,
    session: &Session,
    capture: &CaptureSnapshot,
) -> Option<PreviewResult> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = worker.poll_with_capture(session, capture).unwrap();
        if result.is_some() || !worker.is_busy() {
            return result;
        }
        assert!(Instant::now() < deadline, "preview test worker deadline");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn camera_filter_failure_retains_processed_frame_and_reports_diagnostic() {
    use crate::capture::{CaptureBinding, CapturedFrame, NativeFrameOrigin};
    use skylive_core::{VideoCaptureBinding, VideoCaptureMode, VideoCaptureTarget, VideoEffect, VideoFilter, VideoPixelFormat};

    let mut session = Session::new();
    let scene = session.preview();
    session.apply(Command::SetVideo { width: 16, height: 16, fps_num: 30, fps_den: 1 }).unwrap();
    let binding = VideoCaptureBinding::new(
        VideoCaptureTarget::Windows { symbolic_link: "test-camera".into() },
        VideoCaptureMode::new(16, 16, 30, 1, VideoPixelFormat::Rgb32).unwrap(), false,
    ).unwrap();
    let item = session.apply(Command::AddVideoCaptureSource {
        scene, name: "Camera".into(), binding: binding.clone(),
    }).unwrap().created.unwrap();
    session.apply(Command::SetTransform { scene, item, transform: Transform::full(16, 16) }).unwrap();
    let source = session.snapshot().scene(scene).unwrap().items()[0].source();
    session.apply(Command::SetSourceVideoFilters {
        source, filters: vec![VideoFilter::new(VideoEffect::color_key_v2()).unwrap()],
    }).unwrap();
    let make_capture = |sequence, stream, color| CaptureSnapshot::test_camera_frame(
        session.identity(), source, CapturedFrame {
            native_origin: Some(NativeFrameOrigin::Camera { owner: 19, stream }),
            binding: CaptureBinding::Video(Arc::new(binding.clone())), sequence,
            requested_at: MediaTime(0), native_timestamp_ns: None, native_clock_epoch: None, device_qpc_ns: None, directshow_clock: None, async_delay: None,draft_raw:None, image: Arc::new(RgbaFrame::solid(16, 16, color).unwrap()).into(),
            display: None, window: None,
        },
    );
    let first_capture = make_capture(1, 27, [220, 10, 10, 255]);
    let later_capture = make_capture(2, 27, [10, 10, 220, 255]);
    let changed_stream = make_capture(3, 28, [10, 10, 220, 255]);
    let snapshot = session.snapshot();
    let playback = PlaybackSnapshot::at_time(&session, MediaTime(0));
    let view = FrameScene::Live(scene);
    let mut cache = ClipCache::default();
    cache.point_backend.shared = Some(crate::video_filters::SharedSourceFilters::new());

    let render = |cache: &mut ClipCache, capture: &CaptureSnapshot, work_limit| {
        let mut resources = RenderResources::new(cache, capture);
        resources.base_work_limit = work_limit;
        render_scene(&snapshot, &view, None, 1, 1, &playback, &mut resources, 0, true).unwrap()
    };
    let complete = render(&mut cache, &first_capture, 1_000_000);
    assert!(complete.diagnostics.is_empty());
    assert_ne!(complete.image.pixels(), &[0, 0, 0, 255]);
    // One output pixel fits, but the 16x16 source filter exceeds this work budget.
    let failed = render(&mut cache, &later_capture, 16);
    assert_eq!(failed.image.pixels(), complete.image.pixels());
    assert_eq!(failed.diagnostics.len(), 1);
    assert_eq!(failed.diagnostics[0].source, source);
    assert_eq!(failed.diagnostics[0].kind, crate::resource_issue::ResourceIssueKind::Limit);

    let other_stream = render(&mut cache, &changed_stream, 16);
    assert_eq!(other_stream.image.pixels(), &[0, 0, 0, 255]);
    assert_eq!(other_stream.diagnostics.len(), 1);
    assert_eq!(other_stream.diagnostics[0].source, source);
}

#[test]
fn actual_capture_pixels_scale_crop_reflect_and_rotate_through_the_shared_renderer() {
    let (mut session, scene, item, _) = fixture((2, 2), Transform::full(16, 16));
    let (mut capture, calls, dropped) = worker(vec![pattern()], CaptureLimits::default());
    let snapshot = acquire(&mut capture, &session, &[FrameScene::Live(scene)], 0);
    assert_pixels(
        &render_live(&session, scene, &snapshot, 4, 2),
        4,
        2,
        &[R, R, G, G, B, B, W, W],
    );
    session
        .apply(Command::SetTransform {
            scene,
            item,
            transform: Transform {
                x: 16.0,
                flip_x: true,
                ..Transform::full(16, 16)
            },
        })
        .unwrap();
    assert_pixels(
        &render_live(&session, scene, &snapshot, 4, 2),
        4,
        2,
        &[G, G, R, R, W, W, B, B],
    );
    session
        .apply(Command::SetTransform {
            scene,
            item,
            transform: Transform {
                x: 16.0,
                rotation_degrees: 90.0,
                ..Transform::full(16, 16)
            },
        })
        .unwrap();
    assert_pixels(
        &render_live(&session, scene, &snapshot, 4, 2),
        4,
        2,
        &[B, B, R, R, W, W, G, G],
    );
    session
        .apply(Command::SetTransform {
            scene,
            item,
            transform: Transform {
                crop: Crop {
                    left: 1,
                    ..Default::default()
                },
                ..Transform::full(16, 16)
            },
        })
        .unwrap();
    assert_pixels(
        &render_live(&session, scene, &snapshot, 4, 2),
        4,
        2,
        &[G, G, K, K, W, W, K, K],
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(snapshot);
    capture.join().unwrap();
    assert!(dropped.load(Ordering::SeqCst));
}

#[test]
fn runtime_resolution_changes_keep_authored_basis_and_native_pixel_scale() {
    let (session, scene, _, source) = fixture((2, 2), Transform::full(8, 8));
    let before = session.snapshot();
    let (mut capture, _, _) = worker(vec![pattern(), wide()], CaptureLimits::default());
    let first = acquire(&mut capture, &session, &[FrameScene::Live(scene)], 0);
    assert_pixels(
        &render_live(&session, scene, &first, 4, 4),
        4,
        4,
        &[R, G, K, K, B, W, K, K, K, K, K, K, K, K, K, K],
    );
    let next = acquire(
        &mut capture,
        &session,
        &[FrameScene::Live(scene)],
        40_000_000,
    );
    assert_eq!(first.epoch(), next.epoch());
    assert!(next.generation() > first.generation());
    assert_pixels(
        &render_live(&session, scene, &next, 4, 4),
        4,
        4,
        &[
            R,
            G,
            B,
            W,
            [255, 255, 0, 255],
            [0, 255, 255, 255],
            [255, 0, 255, 255],
            [128, 128, 128, 255],
            K,
            K,
            K,
            K,
            K,
            K,
            K,
            K,
        ],
    );
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
    assert_eq!(
        session
            .snapshot()
            .source(source)
            .unwrap()
            .kind()
            .dimensions(),
        (2, 2)
    );
    drop((first, next));
    capture.join().unwrap();
}

#[test]
fn frozen_program_uses_its_captured_basis_once_after_rebind_and_last_source_reference_removal() {
    let (mut session, scene, item, source) = fixture((2, 2), Transform::full(8, 8));
    let mut owner = Controller::new();
    let mut clock = PlaybackClock::new();
    owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
    let (_, _, views) = state(&mut session, &mut owner, &mut clock, 0);
    let (mut capture, _, _) = worker(vec![pattern(), wide()], CaptureLimits::default());
    acquire(&mut capture, &session, &views, 0);
    owner.before_change(&session).unwrap();
    session
        .apply(Command::RebindDisplayCapture {
            source,
            binding: binding(4, 2),
        })
        .unwrap();
    let (output, playback, views) = state(&mut session, &mut owner, &mut clock, 40_000_000);
    let snapshot = acquire(&mut capture, &session, &views, 40_000_000);
    assert_eq!(output.scenes()[0].captured_dimensions(source), Some((2, 2)));
    let expected = [
        R,
        G,
        B,
        W,
        [255, 255, 0, 255],
        [0, 255, 255, 255],
        [255, 0, 255, 255],
        [128, 128, 128, 255],
        K,
        K,
        K,
        K,
        K,
        K,
        K,
        K,
    ];
    assert_pixels(
        &render_output_with_capture(&session.snapshot(), &output, 4, 4, &playback, &snapshot)
            .unwrap(),
        4,
        4,
        &expected,
    );
    owner.before_change(&session).unwrap();
    session.apply(Command::RemoveItem { scene, item }).unwrap();
    assert!(session.snapshot().source(source).is_none());
    let (output, playback, views) = state(&mut session, &mut owner, &mut clock, 40_000_000);
    assert!(!capture.synchronize(&session, &views).unwrap());
    assert_eq!(capture.snapshot().epoch(), snapshot.epoch());
    assert_pixels(
        &render_output_with_capture(
            &session.snapshot(),
            &output,
            4,
            4,
            &playback,
            &capture.snapshot(),
        )
        .unwrap(),
        4,
        4,
        &expected,
    );
    assert_pixels(
        &render_live(&session, scene, &capture.snapshot(), 4, 2),
        4,
        2,
        &[K; 8],
    );
    drop(snapshot);
    capture.join().unwrap();
}

#[test]
fn opaque_screen_opacity_uses_linear_srgb_for_gray_and_white() {
    let (session, scene, _, _) = fixture(
        (2, 2),
        Transform {
            opacity: 0.5,
            ..Transform::full(16, 16)
        },
    );
    let (mut capture, _, _) = worker(
        vec![
            RgbaFrame::solid(2, 2, [128, 128, 128, 255]).unwrap(),
            RgbaFrame::solid(2, 2, W).unwrap(),
        ],
        CaptureLimits::default(),
    );
    let first = acquire(&mut capture, &session, &[FrameScene::Live(scene)], 0);
    assert_pixels(
        &render_live(&session, scene, &first, 4, 2),
        4,
        2,
        &[[92, 92, 92, 255]; 8],
    );
    let next = acquire(
        &mut capture,
        &session,
        &[FrameScene::Live(scene)],
        40_000_000,
    );
    assert_pixels(
        &render_live(&session, scene, &next, 4, 2),
        4,
        2,
        &[[188, 188, 188, 255]; 8],
    );
    drop((first, next));
    capture.join().unwrap();
}

#[test]
fn program_and_preview_share_one_capture_and_completed_older_generation_cannot_starve_publication()
{
    let (mut session, a, _, source) = fixture((2, 2), Transform::full(16, 16));
    let b = session
        .apply(Command::AddScene {
            name: "Shared preview".into(),
        })
        .unwrap()
        .created
        .unwrap();
    let item = session
        .apply(Command::AddExistingSource { scene: b, source })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::SetTransform {
            scene: b,
            item,
            transform: Transform::full(16, 16),
        })
        .unwrap();
    let mut owner = Controller::new();
    let mut clock = PlaybackClock::new();
    owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
    owner.select_preview(&mut session, b, MediaTime(0)).unwrap();
    let (output, playback, views) = state(&mut session, &mut owner, &mut clock, 0);
    let (mut capture, calls, _) = worker(
        vec![
            RgbaFrame::solid(2, 2, R).unwrap(),
            RgbaFrame::solid(2, 2, B).unwrap(),
        ],
        CaptureLimits::default(),
    );
    let first = acquire(&mut capture, &session, &views, 0);
    assert_eq!(first.frame_count(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_pixels(
        &render_output_with_capture(&session.snapshot(), &output, 4, 2, &playback, &first).unwrap(),
        4,
        2,
        &[R; 8],
    );
    assert_pixels(&render_live(&session, b, &first, 4, 2), 4, 2, &[R; 8]);
    let mut preview = PreviewWorker::new().unwrap();
    preview
        .request_output_with_capture(
            &session,
            4,
            2,
            playback.clone(),
            output.clone(),
            first.clone(),
        )
        .unwrap();
    let second = acquire(&mut capture, &session, &views, 40_000_000);
    assert_eq!(first.epoch(), second.epoch());
    assert!(second.generation() > first.generation());
    let old = completion(&mut preview, &session, &second)
        .expect("same-epoch completed frame remains publishable");
    assert_eq!(old.key.capture_generation, first.generation());
    assert_pixels(&old.image, 4, 2, &[R; 8]);
    preview
        .request_output_with_capture(&session, 4, 2, playback, output, second.clone())
        .unwrap();
    assert!(capture.synchronize(&session, &[]).unwrap());
    let retired = capture.snapshot();
    assert_ne!(retired.epoch(), second.epoch());
    assert!(
        completion(&mut preview, &session, &retired).is_none(),
        "prior epoch must not reach presentation"
    );
    assert!(!preview.is_busy());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(session.program(), a);
    preview.join().unwrap();
    drop((first, second, retired));
    capture.join().unwrap();
}

#[test]
fn missing_crop_invalid_and_overbudget_capture_are_diagnostics_without_canonical_mutation() {
    let (session, scene, _, _) = fixture((2, 2), Transform::full(16, 16));
    let (mut capture, calls, _) = worker(vec![], CaptureLimits::default());
    capture
        .synchronize(&session, &[FrameScene::Live(scene)])
        .unwrap();
    let empty = capture.snapshot();
    let before = session.snapshot();
    let playback = PlaybackSnapshot::at_time(&session, MediaTime(0));
    let output = FrameComposition::Scene(FrameScene::Live(scene));
    let mut cache = ClipCache::default();
    let result = render_output_cached_with_capture(
        &session.snapshot(),
        &output,
        4,
        2,
        &playback,
        &mut cache,
        &empty,
    )
    .unwrap();
    assert_pixels(&result.image, 4, 2, &[K; 8]);
    assert_eq!(result.diagnostics[0].code, "capture_frame_unavailable");
    assert!(matches!(
        render(&session.snapshot(), scene, 4, 2),
        Err(PreviewError::ResourcesUnavailable(_))
    ));
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(empty);
    capture.join().unwrap();
    let (session, scene, _, _) = fixture(
        (4, 2),
        Transform {
            crop: Crop {
                left: 3,
                ..Default::default()
            },
            ..Transform::full(16, 16)
        },
    );
    let before = session.snapshot();
    let (mut capture, _, _) = worker(vec![pattern()], CaptureLimits::default());
    let snapshot = acquire(&mut capture, &session, &[FrameScene::Live(scene)], 0);
    let result = render_output_cached_with_capture(
        &session.snapshot(),
        &FrameComposition::Scene(FrameScene::Live(scene)),
        4,
        2,
        &PlaybackSnapshot::at_time(&session, MediaTime(0)),
        &mut ClipCache::default(),
        &snapshot,
    )
    .unwrap();
    assert_pixels(&result.image, 4, 2, &[K; 8]);
    assert_eq!(result.diagnostics[0].code, "capture_geometry_unavailable");
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
    assert!(session.snapshot().validate().is_ok());
    drop(snapshot);
    capture.join().unwrap();
    let (session, scene, _, _) = fixture((2, 2), Transform::full(16, 16));
    let (mut capture, _, _) = worker(
        vec![pattern()],
        CaptureLimits {
            max_frame_bytes: 8,
            ..CaptureLimits::default()
        },
    );
    let snapshot = acquire(&mut capture, &session, &[FrameScene::Live(scene)], 0);
    assert_eq!(snapshot.frame_count(), 0);
    assert!(
        matches!(render_output_with_capture(&session.snapshot(),&FrameComposition::Scene(FrameScene::Live(scene)),4,2,&PlaybackSnapshot::at_time(&session,MediaTime(0)),&snapshot),Err(PreviewError::ResourcesUnavailable(problems))if problems[0].code=="capture_budget_exceeded")
    );
    drop(snapshot);
    capture.join().unwrap();
}

#[test]
fn shared_transform_preparation_checks_frame_binding_and_session_then_fits_actual_size() {
    let (mut session, scene, item, source) = fixture((2, 2), Transform::full(16, 16));
    let (mut capture, _, _) = worker(vec![wide()], CaptureLimits::default());
    let snapshot = acquire(&mut capture, &session, &[FrameScene::Live(scene)], 0);
    let before = session.snapshot();
    let command = prepare_transform_command(
        &session,
        &snapshot,
        scene,
        item,
        TransformAction::FitToCanvas,
    )
    .unwrap();
    assert!(matches!(
        command,
        Command::TransformItemForFrame {
            frame_width: 4,
            frame_height: 2,
            ..
        }
    ));
    assert!(Arc::ptr_eq(&before, &session.snapshot()));
    session.apply(command).unwrap();
    let current = session.snapshot();
    let t = current.scene(scene).unwrap().items()[0].transform();
    assert_eq!((t.width, t.height, t.x, t.y), (8.0, 8.0, 0.0, 4.0));
    assert_eq!(current.source(source).unwrap().kind().dimensions(), (2, 2));
    session
        .apply(Command::RebindDisplayCapture {
            source,
            binding: binding(4, 2),
        })
        .unwrap();
    assert!(matches!(
        prepare_transform_command(&session, &snapshot, scene, item, TransformAction::Center),
        Err(CaptureError::Unavailable(_))
    ));
    let other = Session::from_snapshot((*session.snapshot()).clone(), true).unwrap();
    assert_eq!(
        prepare_transform_command(&other, &snapshot, scene, item, TransformAction::Center)
            .unwrap_err(),
        CaptureError::ForeignSession
    );
    drop(snapshot);
    capture.close();
    assert_eq!(capture.snapshot().frame_count(), 0);
    assert_eq!(
        capture.request_frame(&session, MediaTime(40_000_000)),
        Err(CaptureError::Closed)
    );
    capture.join().unwrap();
}
