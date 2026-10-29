//! Headless host integration tests: actual GIF bytes, clocks, cache and workers.
use super::*;
use crate::{assets, playback::PlaybackClock};
use skylive_core::{Command, ImageAsset, Transform};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static SERIAL: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "skylive-host-animation-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
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

fn gif_bytes(repeats: Option<u16>) -> Vec<u8> {
    // Independent 1x1 red/green global palette and two valid LZW streams.
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0, 255, 0, 0, 0, 255, 0]);
    if let Some(repeats) = repeats {
        bytes.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01");
        bytes.extend_from_slice(&repeats.to_le_bytes());
        bytes.push(0);
    }
    for data in [0x44, 0x4c] {
        bytes.extend_from_slice(&[0x21, 0xf9, 4, 0, 10, 0, 0, 0]); // 100ms, keep canvas
        bytes.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, data, 1, 0]);
    }
    bytes.push(0x3b);
    bytes
}

struct Fixture {
    _directory: Temp,
    path: PathBuf,
    asset: ImageAsset,
    session: Session,
    scene: Id,
    item: Id,
    source: Id,
}
fn fixture(repeats: Option<u16>) -> Fixture {
    let directory = Temp::new();
    let path = directory.0.join("two-frame.gif");
    fs::write(&path, gif_bytes(repeats)).unwrap();
    let asset = assets::prepare_image(&path).unwrap().asset;
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
            name: "GIF A".into(),
            asset: asset.clone(),
        })
        .unwrap()
        .created
        .unwrap();
    session
        .apply(Command::SetTransform {
            scene,
            item,
            transform: Transform::full(16, 16),
        })
        .unwrap();
    let source = session.snapshot().scene(scene).unwrap().items()[0].source();
    Fixture {
        _directory: directory,
        path,
        asset,
        session,
        scene,
        item,
        source,
    }
}
fn cached(clip_cache: &ClipCache, asset: &ImageAsset) -> Arc<AnimationClip> {
    clip_cache
        .entries
        .get(asset)
        .unwrap()
        .slots[0]
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone()
}
fn solid(frame: &RgbaFrame, color: [u8; 4]) {
    assert!(
        frame.pixels().chunks_exact(4).all(|pixel| pixel == color),
        "expected uniform {color:?}, got {:?}",
        frame.pixels()
    );
}
fn at(fixture: &Fixture, cache: &mut ClipCache, time: u64) -> RenderedScene {
    render_cached(
        &fixture.session.snapshot(),
        fixture.scene,
        4,
        4,
        &PlaybackSnapshot::at_time(&fixture.session, MediaTime(time)),
        cache,
    )
    .unwrap()
}
fn settle(worker: &mut PreviewWorker, session: &Session) -> Option<PreviewResult> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = worker.poll(session).unwrap();
        if value.is_some() || !worker.is_busy() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "headless animation worker did not settle"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn actual_gif_timeline_has_inclusive_edges_deadlines_and_one_cached_decode() {
    let f = fixture(Some(0));
    let mut cache = ClipCache::default();
    let first = at(&f, &mut cache, 0);
    solid(&first.image, [255, 0, 0, 255]);
    assert!(first.diagnostics.is_empty());
    assert_eq!(first.next_after_ns, Some(100_000_001));
    let retained = cached(&cache, &f.asset);
    assert_eq!(
        (cache.entries.len(), cache.bytes, retained.frame_count()),
        (1, 8, 2)
    );
    // Once admitted, frames come from the immutable clip even if the linked
    // file disappears. A later cache-scope change must revalidate the resource.
    fs::remove_file(&f.path).unwrap();
    for (time, color, deadline) in [
        (100_000_000, [255, 0, 0, 255], 1),
        (100_000_001, [0, 255, 0, 255], 100_000_000),
        (200_000_000, [0, 255, 0, 255], 1),
        (200_000_001, [255, 0, 0, 255], 100_000_000),
    ] {
        let output = at(&f, &mut cache, time);
        solid(&output.image, color);
        assert!(output.diagnostics.is_empty());
        assert_eq!(output.next_after_ns, Some(deadline));
        assert!(Arc::ptr_eq(&retained, &cached(&cache, &f.asset)));
        assert_eq!((cache.entries.len(), cache.bytes), (1, 8));
    }
}

#[test]
fn finite_gif_stops_deadlines_after_final_inclusive_frame_end() {
    let f = fixture(None);
    let mut cache = ClipCache::default();
    let last = at(&f, &mut cache, 200_000_000);
    solid(&last.image, [0, 255, 0, 255]);
    assert_eq!(last.next_after_ns, Some(1));
    for time in [200_000_001, u64::MAX] {
        let output = at(&f, &mut cache, time);
        solid(&output.image, [0, 255, 0, 255]);
        assert_eq!(output.next_after_ns, None);
        assert!(output.diagnostics.is_empty());
    }
}

#[test]
fn cache_scope_tracks_scene_revision_and_session_while_negative_results_are_stable() {
    let mut f = fixture(Some(0));
    let second = f
        .session
        .apply(Command::DuplicateScene {
            scene: f.scene,
            name: "Other scene".into(),
        })
        .unwrap()
        .created
        .unwrap();
    let mut cache = ClipCache::default();
    let first = at(&f, &mut cache, 0);
    solid(&first.image, [255, 0, 0, 255]);
    let retained = cached(&cache, &f.asset);
    fs::remove_file(&f.path).unwrap();
    let sample = PlaybackSnapshot::at_time(&f.session, MediaTime(100_000_001));
    let resized = render_cached(&f.session.snapshot(), f.scene, 2, 2, &sample, &mut cache).unwrap();
    solid(&resized.image, [0, 255, 0, 255]);
    assert!(Arc::ptr_eq(&retained, &cached(&cache, &f.asset)));
    let other = render_cached(&f.session.snapshot(), second, 2, 2, &sample, &mut cache).unwrap();
    assert_eq!(other.diagnostics.len(), 1);
    assert_eq!(other.diagnostics[0].code, "image_io");
    assert_eq!(cache.bytes, 0);
    fs::write(&f.path, gif_bytes(Some(0))).unwrap();
    let still_failed =
        render_cached(&f.session.snapshot(), second, 2, 2, &sample, &mut cache).unwrap();
    assert_eq!(
        still_failed.diagnostics[0].code, "image_io",
        "negative cache must not repeat I/O each animation sample"
    );
    let back = at(&f, &mut cache, 0);
    solid(&back.image, [255, 0, 0, 255]);
    assert!(back.diagnostics.is_empty());
    fs::remove_file(&f.path).unwrap();
    f.session
        .apply(Command::RenameCollection {
            name: "New revision".into(),
        })
        .unwrap();
    let changed = at(&f, &mut cache, 0);
    assert_eq!(changed.diagnostics[0].code, "image_io");
    fs::write(&f.path, gif_bytes(Some(0))).unwrap();
    f.session = Session::from_snapshot((*f.session.snapshot()).clone(), true).unwrap();
    let reopened = at(&f, &mut cache, 0);
    solid(&reopened.image, [255, 0, 0, 255]);
    assert!(reopened.diagnostics.is_empty());
    assert_eq!((cache.entries.len(), cache.bytes), (1, 8));
}

#[test]
fn shared_references_and_distinct_sources_deduplicate_failed_and_successful_resources() {
    let mut f = fixture(Some(0));
    f.session
        .apply(Command::AddImageSource {
            scene: f.scene,
            name: "GIF B".into(),
            asset: f.asset.clone(),
        })
        .unwrap();
    f.session
        .apply(Command::AddExistingSource {
            scene: f.scene,
            source: f.source,
        })
        .unwrap();
    fs::remove_file(&f.path).unwrap();
    let mut cache = ClipCache::default();
    let missing = at(&f, &mut cache, 0);
    assert_eq!(
        missing.diagnostics.len(),
        2,
        "one issue per source, not per repeated scene item"
    );
    assert!(
        missing
            .diagnostics
            .iter()
            .all(|issue| issue.code == "image_io")
    );
    assert_ne!(missing.diagnostics[0].source, missing.diagnostics[1].source);
    assert_eq!((cache.entries.len(), cache.bytes), (1, 0));
    fs::write(&f.path, gif_bytes(Some(0))).unwrap();
    let cached_failure = at(&f, &mut cache, 150_000_000);
    assert_eq!(cached_failure.diagnostics.len(), 2);
    let mut fresh_cache = ClipCache::default();
    let present = at(&f, &mut fresh_cache, 0);
    assert!(present.diagnostics.is_empty());
    assert_eq!((fresh_cache.entries.len(), fresh_cache.bytes), (1, 8));
}

#[test]
fn same_clip_identity_keeps_independent_source_activation_times() {
    let mut f = fixture(Some(0));
    f.session
        .apply(Command::SetTransform {
            scene: f.scene,
            item: f.item,
            transform: Transform {
                width: 8.0,
                height: 16.0,
                ..Transform::full(1, 1)
            },
        })
        .unwrap();
    let second_item = f
        .session
        .apply(Command::AddImageSource {
            scene: f.scene,
            name: "GIF B".into(),
            asset: f.asset.clone(),
        })
        .unwrap()
        .created
        .unwrap();
    let second_source = f
        .session
        .snapshot()
        .scene(f.scene)
        .unwrap()
        .items()
        .last()
        .unwrap()
        .source();
    f.session
        .apply(Command::SetTransform {
            scene: f.scene,
            item: second_item,
            transform: Transform {
                x: 8.0,
                width: 8.0,
                height: 16.0,
                ..Transform::full(1, 1)
            },
        })
        .unwrap();
    f.session
        .apply(Command::SetVisible {
            scene: f.scene,
            item: second_item,
            visible: false,
        })
        .unwrap();
    let mut clock = PlaybackClock::new();
    clock.update(&f.session, &[f.scene], MediaTime(0)).unwrap();
    f.session
        .apply(Command::SetVisible {
            scene: f.scene,
            item: second_item,
            visible: true,
        })
        .unwrap();
    let activated = clock
        .update(&f.session, &[f.scene], MediaTime(50_000_000))
        .unwrap();
    assert_eq!(activated.elapsed(f.source), MediaTime(50_000_000));
    assert_eq!(activated.elapsed(second_source), MediaTime(0));
    let current = clock
        .update(&f.session, &[f.scene], MediaTime(100_000_001))
        .unwrap();
    let mut cache = ClipCache::default();
    let output =
        render_cached(&f.session.snapshot(), f.scene, 16, 16, &current, &mut cache).unwrap();
    for (index, pixel) in output.image.pixels().chunks_exact(4).enumerate() {
        assert_eq!(
            pixel,
            if index % 16 < 8 {
                &[0, 255, 0, 255]
            } else {
                &[255, 0, 0, 255]
            }
        );
    }
    assert!(output.diagnostics.is_empty());
    assert_eq!(output.next_after_ns, Some(50_000_000));
    assert_eq!(
        (cache.entries.len(), cache.bytes),
        (1, 8),
        "decoded clip is shared while source elapsed values stay independent"
    );
    let before = f.session.snapshot();
    let reload = f
        .session
        .apply(Command::SetImageSource {
            source: f.source,
            asset: f.asset.clone(),
        })
        .unwrap();
    assert!(!reload.changed);
    assert!(Arc::ptr_eq(&before, &f.session.snapshot()));
    clock
        .restart_source(f.source, MediaTime(150_000_001))
        .unwrap();
    let restarted = clock
        .update(&f.session, &[f.scene], MediaTime(150_000_001))
        .unwrap();
    assert!(restarted.epoch() > current.epoch());
    assert_eq!(restarted.elapsed(f.source), MediaTime(0));
    assert_eq!(restarted.elapsed(second_source), MediaTime(100_000_001));
    let output = render_cached(
        &f.session.snapshot(),
        f.scene,
        16,
        16,
        &restarted,
        &mut cache,
    )
    .unwrap();
    for (index, pixel) in output.image.pixels().chunks_exact(4).enumerate() {
        assert_eq!(
            pixel,
            if index % 16 < 8 {
                &[255, 0, 0, 255]
            } else {
                &[0, 255, 0, 255]
            }
        );
    }
}

#[test]
fn stale_playback_admission_and_reset_completions_cannot_publish_old_frames() {
    let mut f = fixture(Some(0));
    let mut clock = PlaybackClock::new();
    clock.update(&f.session, &[f.scene], MediaTime(0)).unwrap();
    let old = clock
        .update(&f.session, &[f.scene], MediaTime(150_000_000))
        .unwrap();
    let mut worker = PreviewWorker::new().unwrap();
    let foreign = PlaybackSnapshot::at_time(&Session::new(), MediaTime(0));
    assert_eq!(
        worker.request_playback(&f.session, f.scene, 2, 2, foreign),
        Err(PreviewError::StalePlayback)
    );
    assert!(!worker.is_busy());
    worker
        .request_playback(&f.session, f.scene, 2, 2, old.clone())
        .unwrap();
    clock.reset(MediaTime(150_000_000)).unwrap();
    let reset = clock
        .update(&f.session, &[f.scene], MediaTime(150_000_000))
        .unwrap();
    assert!(reset.epoch() > old.epoch());
    assert_eq!(reset.elapsed(f.source), MediaTime(0));
    worker.invalidate(); // frontend's explicit playback-epoch discontinuity
    assert!(worker.is_busy());
    assert!(settle(&mut worker, &f.session).is_none());
    worker
        .request_playback(&f.session, f.scene, 2, 2, reset.clone())
        .unwrap();
    let returned = settle(&mut worker, &f.session).unwrap();
    solid(&returned.image, [255, 0, 0, 255]);
    assert_eq!(
        (returned.key.playback_epoch, returned.key.time_ns),
        (reset.epoch(), 150_000_000)
    );
    f.session
        .apply(Command::RenameCollection {
            name: "Later revision".into(),
        })
        .unwrap();
    assert_eq!(
        worker.request_playback(&f.session, f.scene, 2, 2, reset),
        Err(PreviewError::StalePlayback)
    );
    assert!(!worker.is_busy());
    let latest = PlaybackSnapshot::at_time(&f.session, MediaTime(0));
    worker
        .request_playback(&f.session, f.scene, 2, 2, latest)
        .unwrap();
    f.session
        .apply(Command::RenameCollection {
            name: "Changed during work".into(),
        })
        .unwrap();
    assert!(settle(&mut worker, &f.session).is_none());
    worker.join().unwrap();
}

#[test]
fn explicit_resource_retry_recovers_negative_cache_after_no_op_reload_without_publishing_old_work()
{
    let mut f = fixture(Some(0));
    fs::remove_file(&f.path).unwrap();
    let mut worker = PreviewWorker::new().unwrap();
    worker
        .request_playback(
            &f.session,
            f.scene,
            2,
            2,
            PlaybackSnapshot::at_time(&f.session, MediaTime(0)),
        )
        .unwrap();
    let missing = settle(&mut worker, &f.session).unwrap();
    assert_eq!(missing.diagnostics.len(), 1);
    assert_eq!(missing.diagnostics[0].code, "image_io");
    let generation = missing.key.resource_generation;
    fs::write(&f.path, gif_bytes(Some(0))).unwrap();
    let before = f.session.snapshot();
    let reload = f
        .session
        .apply(Command::SetImageSource {
            source: f.source,
            asset: f.asset.clone(),
        })
        .unwrap();
    assert!(!reload.changed);
    assert_eq!(reload.revision, before.revision());
    assert!(
        Arc::ptr_eq(&before, &f.session.snapshot()),
        "resource retry must not manufacture canonical history"
    );
    // Queue the old-generation negative result, then retry while it is pending.
    worker
        .request_playback(
            &f.session,
            f.scene,
            2,
            2,
            PlaybackSnapshot::at_time(&f.session, MediaTime(0)),
        )
        .unwrap();
    worker.invalidate_resources().unwrap();
    assert!(worker.is_busy());
    assert!(
        settle(&mut worker, &f.session).is_none(),
        "a pending failure from before resource retry must not publish"
    );
    worker
        .request_playback(
            &f.session,
            f.scene,
            2,
            2,
            PlaybackSnapshot::at_time(&f.session, MediaTime(0)),
        )
        .unwrap();
    let recovered = settle(&mut worker, &f.session).unwrap();
    solid(&recovered.image, [255, 0, 0, 255]);
    assert!(recovered.diagnostics.is_empty());
    assert_eq!(recovered.key.revision, before.revision());
    assert_eq!(recovered.key.resource_generation, generation + 1);
    worker.join().unwrap();
}
