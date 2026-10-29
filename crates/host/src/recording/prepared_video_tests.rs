//! Exact CPU oracles, not native encoder/runtime throughput acceptance.
use super::*;
use crate::transition::FrameComposition;
use crate::{capture::CaptureSnapshot, transition::FrameScene};
use skylive_core::{Command, Session};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "skylive-prepared-frame-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn base_session() -> Session {
    let mut value = Session::new();
    value
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    value
        .apply(Command::AddColorSource {
            scene: value.preview(),
            name: "Green".into(),
            rgba: [0, 255, 0, 255],
        })
        .unwrap();
    value
}
fn frame(session: &Session, time: u64) -> RecordingFrame {
    RecordingFrame {
        session: session.identity(),
        time: MediaTime(time),
        model: session.snapshot(),
        output: FrameComposition::Scene(FrameScene::Live(session.preview())),
        playback: PlaybackSnapshot::at_time(session, MediaTime(time)),
        capture: Arc::new(CaptureSnapshot::empty(session.identity())),
        resource_generation: 0,
    }
}
struct Oracle {
    cached: FrameCache,
    raw: FrameCache,
    a: OutputRenderer,
    b: OutputRenderer,
}
impl Default for Oracle {
    fn default() -> Self {
        Self {
            cached: FrameCache::default(),
            raw: FrameCache {
                enabled: false,
                ..Default::default()
            },
            a: OutputRenderer::default(),
            b: OutputRenderer::default(),
        }
    }
}
impl Oracle {
    fn sample(&mut self, frame: &RecordingFrame) -> (bool, Vec<u8>) {
        let actual = self.cached.frame(frame, (16, 16), &mut self.a).unwrap();
        let reference = self.raw.frame(frame, (16, 16), &mut self.b).unwrap();
        assert_eq!(
            actual.video.get().image().pixels(),
            reference.video.get().image().pixels()
        );
        assert_eq!(actual.video.get().nv12(), reference.video.get().nv12());
        assert_eq!(actual.diagnostics, reference.diagnostics);
        (actual.hit, actual.video.get().image().pixels().to_vec())
    }
}
#[test]
fn cached_static_color_text_png_shared_scene_and_group_equal_unoptimized_rgba_nv12() {
    let directory = Directory::new();
    let path = directory.0.join("image.png");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
        .save(&path)
        .unwrap();
    for case in 0..5 {
        let mut session = base_session();
        let root = session.preview();
        match case {
            0 => {}
            1 => {
                session
                    .apply(Command::AddTextSource {
                        scene: root,
                        name: "Text".into(),
                        settings: skylive_core::TextSource::new(
                            "Cache".into(),
                            16,
                            16,
                            12,
                            skylive_core::TextStyle::default(),
                        )
                        .unwrap(),
                    })
                    .unwrap();
            }
            2 => {
                let prepared = crate::assets::prepare_image(&path).unwrap();
                session
                    .apply(Command::AddImageSource {
                        scene: root,
                        name: "PNG".into(),
                        asset: prepared.asset,
                    })
                    .unwrap();
            }
            3 => {
                let target = session
                    .apply(Command::AddScene {
                        name: "Shared".into(),
                    })
                    .unwrap()
                    .created
                    .unwrap();
                session
                    .apply(Command::AddColorSource {
                        scene: target,
                        name: "Nested red".into(),
                        rgba: [255, 0, 0, 255],
                    })
                    .unwrap();
                session
                    .apply(Command::AddSceneSource {
                        scene: root,
                        target,
                    })
                    .unwrap();
            }
            _ => {
                let group = session
                    .apply(Command::AddGroup {
                        scene: root,
                        name: "Private".into(),
                    })
                    .unwrap()
                    .created
                    .unwrap();
                let model = session.snapshot();
                let source = model
                    .source(model.item_container(group).unwrap().1.source())
                    .unwrap();
                let skylive_core::SourceKind::Group { group } = source.kind() else {
                    panic!("group")
                };
                let container = group.scene();
                session
                    .apply(Command::AddColorSource {
                        scene: container,
                        name: "Group blue".into(),
                        rgba: [0, 0, 255, 255],
                    })
                    .unwrap();
            }
        }
        let mut oracle = Oracle::default();
        for (index, time) in [0, 1, 33_333_334, 999_000_000].into_iter().enumerate() {
            assert_eq!(
                oracle.sample(&frame(&session, time)).0,
                index != 0,
                "case{case}/time{time}"
            );
        }
        let entry = oracle.cached.entry.as_ref().unwrap();
        assert!(
            entry.key.retained_bytes() + size_of::<Entry>() + entry.prepared.byte_len()
                <= MAX_PREPARED_VIDEO_BYTES
        );
    }
}
#[test]
fn model_arc_identity_resource_generation_and_backward_time_invalidate_without_pinning_model() {
    let mut session = base_session();
    let mut oracle = Oracle::default();
    let mut first = frame(&session, 100);
    let old = Arc::downgrade(&first.model);
    assert!(!oracle.sample(&first).0);
    assert!(oracle.sample(&frame(&session, 200)).0);
    assert!(!oracle.sample(&frame(&session, 50)).0);
    let mut equal_revision = frame(&session, 60);
    equal_revision.model = Arc::new((*equal_revision.model).clone());
    assert!(!oracle.sample(&equal_revision).0);
    first.resource_generation = 1;
    assert!(!oracle.sample(&first).0);
    assert!(oracle.sample(&first).0);
    drop(equal_revision);
    drop(first);
    session
        .apply(Command::RenameScene {
            scene: session.preview(),
            name: "New scene name".into(),
        })
        .unwrap();
    assert!(
        old.upgrade().is_none(),
        "cache Weak identity must not pin the retired model payload"
    );
    assert!(!oracle.sample(&frame(&session, 300)).0);
    let other = base_session();
    assert!(!oracle.sample(&frame(&other, 300)).0);
}
#[test]
fn envelope_and_admission_failures_clear_prior_entry_before_any_hit() {
    let mut session = base_session();
    let mut cache = FrameCache::default();
    let mut renderer = OutputRenderer::default();
    cache
        .frame(&frame(&session, 0), (16, 16), &mut renderer)
        .unwrap();
    assert!(cache.entry.is_some());
    let mut stale = frame(&session, 1);
    session
        .apply(Command::RenameScene {
            scene: session.preview(),
            name: "Changed".into(),
        })
        .unwrap();
    stale.model = session.snapshot();
    assert!(cache.frame(&stale, (16, 16), &mut renderer).is_err());
    assert!(cache.entry.is_none());
    cache
        .frame(&frame(&session, 2), (16, 16), &mut renderer)
        .unwrap();
    let mut foreign = frame(&session, 3);
    foreign.capture = Arc::new(CaptureSnapshot::empty(u64::MAX));
    assert!(cache.frame(&foreign, (16, 16), &mut renderer).is_err());
    assert!(cache.entry.is_none());
    cache
        .frame(&frame(&session, 4), (16, 16), &mut renderer)
        .unwrap();
    assert!(
        cache
            .frame(&frame(&session, 5), (0, 16), &mut renderer)
            .is_err()
    );
    assert!(cache.entry.is_none());
    cache
        .frame(&frame(&session, 6), (16, 16), &mut renderer)
        .unwrap();
    let mut foreign_owner = base_session();
    let mut controller = crate::transition::Controller::new();
    controller
        .set_studio(&mut foreign_owner, true, MediaTime(0))
        .unwrap();
    let mut wrong_scene = frame(&session, 6);
    wrong_scene.output = controller
        .sync(&mut foreign_owner, MediaTime(0))
        .unwrap()
        .output;
    assert!(cache.frame(&wrong_scene, (16, 16), &mut renderer).is_err());
    assert!(cache.entry.is_none());
    cache
        .frame(&frame(&session, 6), (16, 16), &mut renderer)
        .unwrap();
    cache.limit = 0;
    let mut invalid = frame(&session, 7);
    invalid.resource_generation = 1;
    assert!(cache.frame(&invalid, (16, 16), &mut renderer).is_err());
    assert!(cache.entry.is_none());
    assert!(PreparedVideo::new(RgbaFrame::solid(2, 2, [255; 4]).unwrap(), 21).is_err());
    assert!(PreparedVideo::new(RgbaFrame::solid(2, 2, [255, 0, 0, 128]).unwrap(), 1024).is_err());
}
#[test]
fn sampled_transition_progress_and_overflow_deadline_never_alias() {
    let mut session = base_session();
    let root = session.preview();
    let blue = session
        .apply(Command::AddScene {
            name: "Blue".into(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::AddColorSource {
            scene: blue,
            name: "Blue source".into(),
            rgba: [0, 0, 255, 255],
        })
        .unwrap();
    let mut oracle = Oracle::default();
    for (progress, expected_hit) in [(0, false), (1, false), (1, true), (2, false), (2, true)] {
        let mut value = frame(&session, progress);
        value.output = FrameComposition::Manual {
            from: Some(FrameScene::Live(root)),
            to: Some(FrameScene::Live(blue)),
            kind: skylive_core::TransitionKind::Fade,
            logical_scene: blue,
            progress_num: progress,
            progress_den: 2,
            epoch: 1,
            next_after_ns: None,
        };
        let (hit, _) = oracle.sample(&value);
        assert_eq!(hit, expected_hit);
    }
    let mut value = frame(&session, u64::MAX - 100);
    value.output = FrameComposition::Manual {
        from: Some(FrameScene::Live(root)),
        to: Some(FrameScene::Live(blue)),
        kind: skylive_core::TransitionKind::Fade,
        logical_scene: blue,
        progress_num: 1,
        progress_den: 2,
        epoch: 2,
        next_after_ns: Some(200),
    };
    assert!(!oracle.sample(&value).0);
    assert!(oracle.cached.entry.is_none());
    assert!(!oracle.sample(&value).0);
}

#[test]
fn gif_strict_deadline_finite_end_and_same_epoch_different_clock_offsets_match_raw() {
    let directory = Directory::new();
    let path = directory.0.join("clock.gif");
    let mut encoder = image::codecs::gif::GifEncoder::new(std::fs::File::create(&path).unwrap());
    encoder
        .encode_frames(
            [[0, 255, 0, 255], [0, 0, 255, 255]]
                .into_iter()
                .map(|color| {
                    image::Frame::from_parts(
                        image::RgbaImage::from_pixel(2, 2, image::Rgba(color)),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(100, 1),
                    )
                }),
        )
        .unwrap();
    drop(encoder);
    let mut session = base_session();
    let asset = crate::assets::prepare_image(&path).unwrap().asset;
    session
        .apply(Command::AddImageSource {
            scene: session.preview(),
            name: "Clocked GIF".into(),
            asset,
        })
        .unwrap();
    let mut oracle = Oracle::default();
    for (time, hit, color) in [
        (0, false, [0, 255, 0, 255]),
        (99_000_000, true, [0, 255, 0, 255]),
        (100_000_000, true, [0, 255, 0, 255]),
        (100_000_001, false, [0, 0, 255, 255]),
        (150_000_000, true, [0, 0, 255, 255]),
    ] {
        let (actual, pixels) = oracle.sample(&frame(&session, time));
        assert_eq!(actual, hit, "GIF {time}");
        assert_eq!(&pixels[..4], &color);
    }
    oracle.sample(&frame(&session, 500_000_000));
    assert!(
        oracle.sample(&frame(&session, 600_000_000)).0,
        "finite EOF is stable"
    );
    let mut clock_a = crate::playback::PlaybackClock::new();
    let mut clock_b = crate::playback::PlaybackClock::new();
    let views = [FrameScene::Live(session.preview())];
    clock_a
        .update_outputs(&session, &views, MediaTime(0))
        .unwrap();
    let mut a = frame(&session, 50_000_000);
    a.playback = clock_a.update_outputs(&session, &views, a.time).unwrap();
    assert!(!oracle.sample(&a).0);
    let mut b = frame(&session, 100_000_000);
    b.playback = clock_b.update_outputs(&session, &views, b.time).unwrap();
    assert_eq!(a.playback.epoch(), b.playback.epoch());
    assert!(!b.playback.advances_from(&a.playback));
    assert!(
        !oracle.sample(&b).0,
        "equal local epochs cannot alias source clock origins"
    );
    let mut next = frame(&session, 100_000_001);
    next.playback = clock_b.update_outputs(&session, &views, next.time).unwrap();
    assert!(oracle.sample(&next).0);
}

#[test]
fn capture_generation_recovery_resolution_and_binding_changes_do_not_reuse_stale_pixels() {
    use crate::capture::{
        CaptureBackend, CaptureCapabilities, CaptureError, CaptureLimits, CaptureWorker,
        DisplayInfo,
    };
    use skylive_core::{DisplayCaptureBinding, DisplayTarget};
    struct Backend(Arc<AtomicU64>);
    impl CaptureBackend for Backend {
        fn capabilities(&self) -> CaptureCapabilities {
            CaptureCapabilities {
                available: true,
                backend: "prepared-cache-fake",
                cursor: false,
                hdr: false,
                note: None,
            }
        }
        fn enumerate(&mut self, _: CaptureLimits) -> Result<Vec<DisplayInfo>, CaptureError> {
            Ok(vec![])
        }
        fn capture(
            &mut self,
            _: &DisplayCaptureBinding,
            _: CaptureLimits,
        ) -> Result<RgbaFrame, CaptureError> {
            match self.0.load(Ordering::SeqCst) {
                2 => Err(CaptureError::Backend("injected unavailable".into())),
                3 => Ok(RgbaFrame::solid(32, 16, [0, 255, 0, 255]).unwrap()),
                1 => Ok(RgbaFrame::solid(16, 16, [0, 0, 255, 255]).unwrap()),
                _ => Ok(RgbaFrame::solid(16, 16, [255, 0, 0, 255]).unwrap()),
            }
        }
    }
    fn poll(worker: &mut CaptureWorker, session: &Session, time: u64) -> Arc<CaptureSnapshot> {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        assert!(
            worker.request_frame(session, MediaTime(time)).unwrap(),
            "fixture must use an admitted capture timestamp: requested={time}, last={:?}, next_after={:?}",
            worker.last_requested_at(),
            worker.next_after_ns(MediaTime(time))
        );
        while worker.is_busy() {
            worker.poll(session, MediaTime(time)).unwrap();
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        worker.snapshot()
    }
    let mut session = base_session();
    let binding = DisplayCaptureBinding::new(
        DisplayTarget::Windows {
            device_id: "fake-display".into(),
        },
        false,
        16,
        16,
    )
    .unwrap();
    session
        .apply(Command::AddDisplayCaptureSource {
            scene: session.preview(),
            name: "Display".into(),
            binding: binding.clone(),
        })
        .unwrap();
    let source = session.snapshot().sources().last().unwrap().id();
    let stage = Arc::new(AtomicU64::new(0));
    let mut worker = CaptureWorker::new(Box::new(Backend(stage.clone()))).unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(session.preview())])
        .unwrap();
    let mut oracle = Oracle::default();
    let mut sample = frame(&session, 0);
    sample.capture = poll(&mut worker, &session, 0);
    assert_eq!(oracle.sample(&sample).1[..4], [255, 0, 0, 255]);
    assert!(oracle.sample(&sample).0);
    // Independent owners can legitimately have the same local epoch/generation.
    let mut other = CaptureWorker::new(Box::new(Backend(Arc::new(AtomicU64::new(1))))).unwrap();
    other
        .synchronize(&session, &[FrameScene::Live(session.preview())])
        .unwrap();
    let alternate = poll(&mut other, &session, 0);
    assert_eq!(
        (sample.capture.epoch(), sample.capture.generation()),
        (alternate.epoch(), alternate.generation())
    );
    sample.capture = alternate;
    let (hit, pixels) = oracle.sample(&sample);
    assert!(!hit);
    assert_eq!(
        pixels[..4],
        [0, 0, 255, 255],
        "another owner's coincident counters must not return cached red"
    );
    let mut failed = CaptureWorker::new(Box::new(Backend(Arc::new(AtomicU64::new(2))))).unwrap();
    failed
        .synchronize(&session, &[FrameScene::Live(session.preview())])
        .unwrap();
    let problem = poll(&mut failed, &session, 0);
    assert_eq!(problem.frame_count(), 0);
    assert!(!problem.problems().is_empty());
    assert!(!problem.is_semantically_empty());
    assert_eq!(
        (sample.capture.epoch(), sample.capture.generation()),
        (problem.epoch(), problem.generation())
    );
    sample.capture = problem;
    assert!(!oracle.sample(&sample).0);
    assert!(oracle.cached.entry.is_none());
    other.close();
    failed.close();
    drop(other);
    drop(failed);
    sample.capture = worker.snapshot();
    assert!(!oracle.sample(&sample).0);
    assert!(oracle.sample(&sample).0);
    stage.store(1, Ordering::SeqCst);
    sample.time = MediaTime(40_000_000);
    sample.playback = PlaybackSnapshot::at_time(&session, sample.time);
    sample.capture = poll(&mut worker, &session, sample.time.0);
    let (hit, pixels) = oracle.sample(&sample);
    assert!(!hit);
    assert_eq!(pixels[..4], [0, 0, 255, 255]);
    stage.store(2, Ordering::SeqCst);
    sample.time = MediaTime(80_000_000);
    sample.playback = PlaybackSnapshot::at_time(&session, sample.time);
    sample.capture = poll(&mut worker, &session, sample.time.0);
    assert!(!oracle.sample(&sample).0);
    assert!(oracle.cached.entry.is_none());
    assert!(
        !oracle.sample(&sample).0,
        "degraded output is never prepared-cache eligible"
    );
    stage.store(3, Ordering::SeqCst);
    // A failed capture deliberately backs off for one second. Respect that
    // contract, then retry at the worker's actual deadline without sleeping.
    let early = MediaTime(120_000_000);
    assert!(
        !worker.request_frame(&session, early).unwrap(),
        "failure cooldown must remain enforced"
    );
    let recovery = early
        .0
        .checked_add(worker.next_after_ns(early).unwrap())
        .unwrap();
    sample.time = MediaTime(recovery);
    sample.playback = PlaybackSnapshot::at_time(&session, sample.time);
    sample.capture = poll(&mut worker, &session, recovery);
    let (hit, pixels) = oracle.sample(&sample);
    assert!(!hit);
    assert_eq!(pixels[..4], [0, 255, 0, 255]);
    let replacement = DisplayCaptureBinding::new(
        DisplayTarget::Windows {
            device_id: "new-fake-display".into(),
        },
        false,
        16,
        16,
    )
    .unwrap();
    session
        .apply(Command::RebindDisplayCapture {
            source,
            binding: replacement,
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(session.preview())])
        .unwrap();
    let after_rebind = recovery.checked_add(40_000_000).unwrap();
    sample = frame(&session, after_rebind);
    sample.capture = poll(&mut worker, &session, after_rebind);
    assert!(!oracle.sample(&sample).0);
    let capture = Arc::downgrade(&sample.capture);
    drop(sample);
    worker.close();
    drop(worker);
    assert!(
        capture.upgrade().is_none(),
        "cache must not retain captured frame/snapshot payloads"
    );
}

#[test]
fn fade_singlefade_and_shared_authored_property_edits_match_unoptimized() {
    let mut session = base_session();
    let green = session.preview();
    let blue = session
        .apply(Command::AddScene {
            name: "To blue".into(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::AddColorSource {
            scene: blue,
            name: "Blue background".into(),
            rgba: [0, 0, 255, 255],
        })
        .unwrap();
    let mut oracle = Oracle::default();
    for variant in 0..3 {
        for (time, hit) in [
            (0, false),
            (150_000_000, false),
            (150_000_000, true),
            (300_000_000, false),
        ] {
            let mut value = frame(&session, time);
            value.output = if variant == 0 {
                FrameComposition::Fade {
                    from: FrameScene::Live(green),
                    to: FrameScene::Live(blue),
                    elapsed_ns: time,
                    duration_ns: 300_000_000,
                    epoch: variant,
                }
            } else {
                FrameComposition::SingleFade {
                    scene: FrameScene::Live(green),
                    logical_scene: green,
                    incoming: variant == 1,
                    elapsed_ns: time,
                    duration_ns: 300_000_000,
                    epoch: variant,
                }
            };
            assert_eq!(oracle.sample(&value).0, hit);
        }
    }
    let source = session.snapshot().scenes()[0].items()[0].source();
    session
        .apply(Command::SetColor {
            source,
            rgba: [255, 0, 0, 255],
        })
        .unwrap();
    assert!(!oracle.sample(&frame(&session, 400_000_000)).0);
    assert!(oracle.sample(&frame(&session, 450_000_000)).0);
}
