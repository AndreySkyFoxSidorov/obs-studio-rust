//! Real scene/resource evaluation across the transition owner and bounded worker.
use super::*;
use crate::{playback::PlaybackClock, transition::Controller};
use skylive_core::{Command, ImageAsset, Transform};
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

fn fixture() -> (Session, Id, Id, Id) {
    let mut session = Session::new();
    let first = session.preview();
    session
        .apply(Command::SetVideo {
            width: 16,
            height: 16,
            fps_num: 30,
            fps_den: 1,
        })
        .unwrap();
    let item = session
        .apply(Command::AddColorSource {
            scene: first,
            name: "Red".into(),
            rgba: [255, 0, 0, 255],
        })
        .unwrap()
        .created
        .unwrap();
    let second = session
        .apply(Command::AddScene {
            name: "Blue scene".into(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::AddColorSource {
            scene: second,
            name: "Blue".into(),
            rgba: [0, 0, 255, 255],
        })
        .unwrap();
    (session, first, second, item)
}
fn solid(image: &RgbaFrame, pixel: [u8; 4]) {
    assert!(
        image.pixels().chunks_exact(4).all(|actual| actual == pixel),
        "expected {pixel:?}, got {:?}",
        image.pixels()
    );
}
#[test]
fn invalid_null_or_manual_samples_do_not_admit_work_or_mask_invalid_identity() {
    let (session, first, second, _) = fixture();
    let mut worker = PreviewWorker::new().unwrap();
    let playback = PlaybackSnapshot::at_time(&session, MediaTime(0));
    let invalid = FrameComposition::Black {
        logical_scene: Id::new(u64::MAX),
        epoch: 1,
    };
    assert_eq!(
        worker.request_output(&session, 1, 1, playback.clone(), invalid.clone()),
        Err(PreviewError::NotFound(Id::new(u64::MAX)))
    );
    assert!(matches!(
        render_output(&session.snapshot(), &invalid, 1, 1, &playback),
        Err(PreviewError::NotFound(_))
    ));
    let invalid = FrameComposition::Manual {
        from: Some(FrameScene::Live(first)),
        to: Some(FrameScene::Live(second)),
        kind: TransitionKind::Fade,
        logical_scene: second,
        progress_num: 2,
        progress_den: 1,
        epoch: 1,
        next_after_ns: None,
    };
    assert_eq!(
        worker.request_output(&session, 1, 1, playback, invalid),
        Err(PreviewError::Media(MediaError::InvalidTransition))
    );
    assert!(!worker.is_busy());
    worker.join().unwrap();
}
#[test]
fn synchronous_program_port_rejects_stale_clock_before_resources_for_live_and_black() {
    let (mut session, first, _, _) = fixture();
    let old = PlaybackSnapshot::at_time(&session, MediaTime(150_000_000));
    session
        .apply(Command::RenameCollection {
            name: "New input generation".into(),
        })
        .unwrap();
    for output in [
        FrameComposition::Scene(FrameScene::Live(first)),
        FrameComposition::Black {
            logical_scene: first,
            epoch: 1,
        },
    ] {
        assert!(matches!(
            render_output(&session.snapshot(), &output, 1, 1, &old),
            Err(PreviewError::StalePlayback)
        ));
    }
}
fn frame(
    session: &mut Session,
    owner: &mut Controller,
    clock: &mut PlaybackClock,
    time: u64,
) -> (FrameComposition, PlaybackSnapshot) {
    let state = owner.sync(session, MediaTime(time)).unwrap();
    let mut views = state.output.scenes();
    if state.studio {
        views.push(FrameScene::Live(session.preview()));
    }
    let playback = clock
        .update_outputs(session, &views, MediaTime(time))
        .unwrap();
    (state.output, playback)
}
#[test]
fn actual_fade_pixels_deadlines_completion_and_studio_swap_use_one_runtime_owner() {
    let (mut session, first, second, _) = fixture();
    let mut owner = Controller::new();
    let mut clock = PlaybackClock::new();
    owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
    owner
        .select_preview(&mut session, second, MediaTime(0))
        .unwrap();
    owner.transition(&mut session, MediaTime(0)).unwrap();
    assert_eq!(session.program(), second);
    assert_eq!(session.preview(), second);
    let mut cache = ClipCache::default();
    for (time, pixel) in [
        (0, [255, 0, 0, 255]),
        (150_000_000, [128, 0, 128, 255]),
        (300_000_000, [0, 0, 255, 255]),
    ] {
        let (output, playback) = frame(&mut session, &mut owner, &mut clock, time);
        let result =
            render_output_cached(&session.snapshot(), &output, 4, 4, &playback, &mut cache)
                .unwrap();
        solid(&result.image, pixel);
        assert!(result.diagnostics.is_empty());
        if time < 300_000_000 {
            assert!(result.next_after_ns.is_some_and(|next| next > 0));
        } else {
            assert_eq!(result.next_after_ns, None);
        }
    }
    assert_eq!(session.preview(), first);
    assert_eq!(session.program(), second);
}
#[test]
fn program_layout_freezes_but_source_properties_and_latest_retired_source_remain_live() {
    let (mut session, first, _, item) = fixture();
    let source = session.snapshot().scene(first).unwrap().items()[0].source();
    let mut owner = Controller::new();
    owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
    owner.before_change(&session).unwrap();
    session
        .apply(Command::SetTransform {
            scene: first,
            item,
            transform: Transform {
                x: 8.0,
                width: 8.0,
                height: 16.0,
                ..Transform::full(16, 16)
            },
        })
        .unwrap();
    owner.before_change(&session).unwrap();
    session
        .apply(Command::SetColor {
            source,
            rgba: [0, 255, 0, 255],
        })
        .unwrap();
    let mut clock = PlaybackClock::new();
    let (output, playback) = frame(&mut session, &mut owner, &mut clock, 1);
    solid(
        &render_output(&session.snapshot(), &output, 4, 4, &playback).unwrap(),
        [0, 255, 0, 255],
    );
    let preview = render(&session.snapshot(), first, 4, 4).unwrap();
    for (index, pixel) in preview.pixels().chunks_exact(4).enumerate() {
        assert_eq!(
            pixel,
            if index % 4 < 2 {
                &[0, 0, 0, 255]
            } else {
                &[0, 255, 0, 255]
            }
        );
    }
    owner.before_change(&session).unwrap();
    session
        .apply(Command::RemoveItem { scene: first, item })
        .unwrap();
    assert!(session.snapshot().source(source).is_none());
    let (output, playback) = frame(&mut session, &mut owner, &mut clock, 2);
    solid(
        &render_output(&session.snapshot(), &output, 4, 4, &playback).unwrap(),
        [0, 255, 0, 255],
    );
    owner.set_studio(&mut session, false, MediaTime(3)).unwrap();
    let (output, playback) = frame(&mut session, &mut owner, &mut clock, 3);
    assert_eq!(session.preview(), first);
    assert_eq!(session.program(), first);
    assert!(matches!(output,FrameComposition::Scene(FrameScene::Live(id)) if id==first));
    solid(
        &render_output(&session.snapshot(), &output, 4, 4, &playback).unwrap(),
        [0, 0, 0, 255],
    );
}
#[test]
fn frozen_source_resize_preserves_captured_item_scale_and_position() {
    let (mut session, first, _, item) = fixture();
    let source = session.snapshot().scene(first).unwrap().items()[0].source();
    session
        .apply(Command::SetColorSize {
            source,
            width: 4,
            height: 4,
        })
        .unwrap();
    session
        .apply(Command::SetTransform {
            scene: first,
            item,
            transform: Transform {
                x: 4.0,
                y: 4.0,
                ..Transform::full(4, 4)
            },
        })
        .unwrap();
    let mut owner = Controller::new();
    owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
    owner.before_change(&session).unwrap();
    session
        .apply(Command::SetColorSize {
            source,
            width: 8,
            height: 4,
        })
        .unwrap();
    let mut clock = PlaybackClock::new();
    let (output, playback) = frame(&mut session, &mut owner, &mut clock, 1);
    let image = render_output(&session.snapshot(), &output, 16, 16, &playback).unwrap();
    for (index, pixel) in image.pixels().chunks_exact(4).enumerate() {
        let (x, y) = (index % 16, index / 16);
        assert_eq!(
            pixel,
            if (4..12).contains(&x) && (4..8).contains(&y) {
                &[255, 0, 0, 255]
            } else {
                &[0, 0, 0, 255]
            }
        );
    }
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "skylive-fade-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn gif() -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0, 255, 0, 0, 0, 255, 0]);
    bytes.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\0\0\0");
    for data in [0x44, 0x4c] {
        bytes.extend_from_slice(&[
            0x21, 0xf9, 4, 0, 10, 0, 0, 0, 0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, data, 1, 0,
        ]);
    }
    bytes.push(0x3b);
    bytes
}
#[test]
fn retained_program_gif_stays_active_after_preview_last_reference_disappears() {
    let temp = Temp::new();
    let path = temp.0.join("active.gif");
    fs::write(&path, gif()).unwrap();
    let asset = crate::assets::prepare_image(&path).unwrap().asset;
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
        .apply(Command::AddImageSource {
            scene,
            name: "GIF".into(),
            asset,
        })
        .unwrap()
        .created
        .unwrap();
    let source = session.snapshot().scene(scene).unwrap().items()[0].source();
    session
        .apply(Command::SetTransform {
            scene,
            item,
            transform: Transform::full(16, 16),
        })
        .unwrap();
    let mut owner = Controller::new();
    owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
    let mut clock = PlaybackClock::new();
    let (output, pb) = frame(&mut session, &mut owner, &mut clock, 0);
    solid(
        &render_output(&session.snapshot(), &output, 2, 2, &pb).unwrap(),
        [255, 0, 0, 255],
    );
    owner.before_change(&session).unwrap();
    session.apply(Command::RemoveItem { scene, item }).unwrap();
    let (output, pb) = frame(&mut session, &mut owner, &mut clock, 150_000_000);
    assert_eq!(pb.elapsed(source), MediaTime(150_000_000));
    solid(
        &render_output(&session.snapshot(), &output, 2, 2, &pb).unwrap(),
        [0, 255, 0, 255],
    );
}
#[test]
fn malformed_full_clip_charge_is_shared_across_both_fade_endpoints() {
    use sha2::Digest;
    let temp = Temp::new();
    let mut bad = gif();
    bad[6..10].copy_from_slice(&[0, 32, 0, 8]);
    let descriptor = [0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2];
    let position = bad
        .windows(descriptor.len())
        .position(|part| part == descriptor)
        .unwrap();
    bad[position + 10] = 13;
    let path = temp.0.join("late-error.gif");
    fs::write(&path, &bad).unwrap();
    let bad_asset = ImageAsset::new(
        path.to_str().unwrap().into(),
        sha2::Sha256::digest(&bad).into(),
        8192,
        2048,
    )
    .unwrap();
    let good_path = temp.0.join("good.gif");
    fs::write(&good_path, gif()).unwrap();
    let good = crate::assets::prepare_image(&good_path).unwrap().asset;
    let mut session = Session::new();
    let first = session.preview();
    session
        .apply(Command::AddImageSource {
            scene: first,
            name: "Bad".into(),
            asset: bad_asset,
        })
        .unwrap();
    let second = session
        .apply(Command::AddScene {
            name: "Other".into(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::AddImageSource {
            scene: second,
            name: "Good".into(),
            asset: good,
        })
        .unwrap();
    let output = FrameComposition::Fade {
        from: FrameScene::Live(first),
        to: FrameScene::Live(second),
        elapsed_ns: 150_000_000,
        duration_ns: 300_000_000,
        epoch: 1,
    };
    let mut cache = ClipCache::default();
    let rendered = render_output_cached(
        &session.snapshot(),
        &output,
        1,
        1,
        &PlaybackSnapshot::at_time(&session, MediaTime(0)),
        &mut cache,
    )
    .unwrap();
    assert_eq!(
        rendered
            .diagnostics
            .iter()
            .map(|issue| issue.code)
            .collect::<Vec<_>>(),
        ["image_decode", "image_decode_budget"]
    );
    assert_eq!(cache.bytes, 0);
    assert_eq!(cache.entries.len(), 2);
}
#[test]
fn worker_sample_keys_and_invalidation_prevent_old_fade_after_completion() {
    let (mut session, _, second, _) = fixture();
    let mut owner = Controller::new();
    let mut clock = PlaybackClock::new();
    owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
    owner
        .select_preview(&mut session, second, MediaTime(0))
        .unwrap();
    owner.transition(&mut session, MediaTime(0)).unwrap();
    let (old, pb) = frame(&mut session, &mut owner, &mut clock, 150_000_000);
    let mut worker = PreviewWorker::new().unwrap();
    worker
        .request_output(&session, 2, 2, pb, old.clone())
        .unwrap();
    let (finished, pb) = frame(&mut session, &mut owner, &mut clock, 300_000_000);
    assert_ne!(old.identity(), finished.identity());
    worker.invalidate();
    let deadline = Instant::now() + Duration::from_secs(5);
    while worker.is_busy() {
        assert!(worker.poll(&session).unwrap().is_none());
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    worker
        .request_output(&session, 2, 2, pb, finished.clone())
        .unwrap();
    let result = loop {
        if let Some(result) = worker.poll(&session).unwrap() {
            break result;
        }
        assert!(Instant::now() < deadline);
        thread::yield_now();
    };
    assert_eq!(result.key.output, finished.identity());
    assert_eq!(result.key.composition, finished.sample_key());
    solid(&result.image, [0, 0, 255, 255]);
    worker.join().unwrap();
}
