use super::*;
use crate::transition::FrameScene;
use skylive_core::{Command, SourceKind};
use std::time::{Duration, Instant};
#[test]
fn single_assignment_sink_keeps_same_owner_and_stopped_scope_does_not_repeat_notifiers() {
    use std::sync::atomic::AtomicUsize;
    let mut owner = MediaOwner::new().unwrap();
    let identity = owner.instance();
    let thread = owner.handle.as_ref().unwrap().thread().id();
    let (sender, receiver) = mpsc::sync_channel(64);
    owner.connect_audio_sink(MediaAudioSink { sender }).unwrap();
    let (other, other_receiver) = mpsc::sync_channel(1);
    assert!(
        owner
            .connect_audio_sink(MediaAudioSink { sender: other })
            .is_err()
    );
    assert_eq!(owner.instance(), identity);
    assert_eq!(owner.handle.as_ref().unwrap().thread().id(), thread);
    let notifications = Arc::new(AtomicUsize::new(0));
    let observed = notifications.clone();
    owner
        .set_waker(Arc::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
        }))
        .unwrap();
    thread::sleep(Duration::from_millis(15));
    assert_eq!(notifications.load(Ordering::SeqCst), 0);
    let mut session = Session::new();
    let media = prepared(&mut owner, &session);
    let id = source(&mut session, media);
    session
        .apply(Command::AddScene {
            name: "Inactive".into(),
        })
        .unwrap();
    let other_scene = session.snapshot().scenes().last().unwrap().id();
    owner
        .synchronize(
            &session,
            &FrameComposition::Scene(FrameScene::Live(other_scene)),
            MediaTime(0),
            None,
        )
        .unwrap();
    drain_scope(&mut owner, &session);
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        let _ = owner.poll(&session);
        if owner.snapshot().state(id) == Some(MediaPlaybackState::Stopped) {
            break;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    while notifications.load(Ordering::SeqCst) < 3 {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    let before = notifications.load(Ordering::SeqCst);
    thread::sleep(Duration::from_millis(130));
    assert_eq!(notifications.load(Ordering::SeqCst), before);
    drop(receiver);
    drop(other_receiver);
    owner.join().unwrap();
}
#[test]
fn actual_cleared_eof_composites_background_and_stale_empty_marker_cannot_hide_rebind() {
    use crate::{
        capture::{CaptureSnapshot, MediaCaptureUnion},
        playback::PlaybackSnapshot,
        preview,
    };
    let mut session = Session::new();
    session
        .apply(Command::SetVideo {
            width: 64,
            height: 48,
            fps_num: 10,
            fps_den: 1,
        })
        .unwrap();
    session
        .apply(Command::AddColorSource {
            scene: session.preview(),
            name: "Background".into(),
            rgba: [10, 20, 30, 255],
        })
        .unwrap();
    let mut owner = MediaOwner::new().unwrap();
    let media = prepared(&mut owner, &session).with_playback(false, true, true, true);
    let id = source(&mut session, media.clone());
    let output = FrameComposition::Scene(FrameScene::Live(session.preview()));
    owner
        .synchronize(&session, &output, MediaTime(0), None)
        .unwrap();
    drain_scope(&mut owner, &session);
    let initial = await_frame(&mut owner, &session, id);
    let base = Arc::new(CaptureSnapshot::empty(session.identity()));
    let mut union = MediaCaptureUnion::default();
    let old = union.merge(&session, base.clone(), &owner).unwrap();
    assert!(old.frame(id).is_some());
    owner
        .synchronize(&session, &output, MediaTime(700_000_000), None)
        .unwrap();
    drain_scope(&mut owner, &session);
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let _ = owner.poll(&session);
        if owner.snapshot().state(id) == Some(MediaPlaybackState::Ended) {
            break;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    let cleared = union.merge(&session, base.clone(), &owner).unwrap();
    assert!(cleared.frame(id).is_none());
    assert!(cleared.media_is_empty(id, &media));
    assert!(cleared.problems().is_empty());
    let image = preview::render_output_with_capture(
        &session.snapshot(),
        &output,
        2,
        2,
        &PlaybackSnapshot::at_time(&session, MediaTime(700_000_000)),
        &cleared,
    )
    .unwrap();
    assert_eq!(image.pixels(), &[10, 20, 30, 255].repeat(4));
    assert!(old.frame(id).is_some());
    let replacement =
        MediaSource::new(format!("{}.replacement", media.path()), 64, 48, true).unwrap();
    session
        .apply(Command::RebindMediaSource {
            source: id,
            media: replacement.clone(),
        })
        .unwrap();
    assert!(!cleared.media_is_empty(id, &replacement));
    assert!(
        preview::render_output_with_capture(
            &session.snapshot(),
            &output,
            2,
            2,
            &PlaybackSnapshot::at_time(&session, MediaTime(700_000_000)),
            &cleared
        )
        .is_err()
    );
    let mut failed = (*owner.snapshot()).clone();
    failed.generation += 1;
    failed.problems = Arc::new(vec![MediaProblem {
        kind: crate::resource_issue::ResourceIssueKind::Unavailable,
        source: id,
        code: "media_decode",
        detail: "actual decoder failure".into(),
    }]);
    owner.snapshot = Arc::new(failed);
    assert!(union.merge(&session, base, &owner).is_err());
    drop(old);
    drop(cleared);
    drop(initial);
    union.clear();
    owner.join().unwrap();
}
#[test]
fn unrepresentable_playback_clock_is_rejected_before_scope_identity_or_io_admission() {
    let session = Session::new();
    let mut owner = MediaOwner::new().unwrap();
    let before = owner.snapshot();
    let output = FrameComposition::Scene(FrameScene::Live(session.preview()));
    assert!(matches!(
        owner.synchronize(&session, &output, MediaTime(u64::MAX), None),
        Err(MediaSourceError::TimeOverflow)
    ));
    assert_eq!(owner.serial, 0);
    assert_eq!(owner.epoch, 1);
    assert!(Arc::ptr_eq(&before, &owner.snapshot()));
    drop(before);
    owner.join().unwrap();
}
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/no_pns/cavlc.mp4")
}
fn wait(
    owner: &mut MediaOwner,
    session: &Session,
    mut predicate: impl FnMut(&MediaEvent) -> bool,
) -> MediaEvent {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = owner.poll(session).unwrap() {
            if predicate(&event) {
                return event;
            }
        }
        assert!(Instant::now() < deadline, "bounded media owner reply");
        thread::sleep(Duration::from_millis(1));
    }
}
fn prepared(owner: &mut MediaOwner, session: &Session) -> MediaSource {
    let key = owner.prepare(session, fixture()).unwrap();
    match wait(
        owner,
        session,
        |e| matches!(e,MediaEvent::Prepared{key:k,..}if*k==key),
    ) {
        MediaEvent::Prepared {
            result: Ok(result), ..
        } => result.media,
        other => panic!("{other:?}"),
    }
}
fn source(session: &mut Session, media: MediaSource) -> Id {
    session
        .apply(Command::AddMediaSource {
            scene: session.preview(),
            name: "File".into(),
            media,
        })
        .unwrap();
    session
        .snapshot()
        .sources()
        .iter()
        .find(|s| matches!(s.kind(), SourceKind::MediaSource { .. }))
        .unwrap()
        .id()
}
fn drain_scope(owner: &mut MediaOwner, session: &Session) {
    wait(owner, session, |e| matches!(e, MediaEvent::Updated { .. }));
}
fn await_frame(owner: &mut MediaOwner, session: &Session, id: Id) -> Arc<MediaSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = owner.poll(session).unwrap();
        let sample = owner.snapshot();
        if sample.frame(id).is_some() {
            return sample;
        }
        assert!(
            Instant::now() < deadline,
            "bounded real decoder first frame"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn prepare_decodes_real_metadata_and_invalid_file_does_not_publish_a_source() {
    let session = Session::new();
    let mut owner = MediaOwner::new().unwrap();
    let media = prepared(&mut owner, &session);
    assert_eq!(media.video_dimensions(), Some((64, 48)));
    assert!(media.has_audio());
    assert!(owner.snapshot().frames().next().is_none());
    let key = owner
        .prepare(&session, fixture().with_file_name("absent.mp4"))
        .unwrap();
    assert!(matches!(
        wait(
            &mut owner,
            &session,
            |e| matches!(e,MediaEvent::Prepared{key:k,..}if*k==key)
        ),
        MediaEvent::Prepared { result: Err(_), .. }
    ));
    assert_eq!(session.snapshot().sources().len(), 0);
    owner.join().unwrap();
}
#[test]
fn prepared_receipt_is_delivered_after_session_replacement() {
    let session = Session::new();
    let newer = Session::new();
    let mut owner = MediaOwner::new().unwrap();
    let key = owner.prepare(&session, fixture()).unwrap();
    let event = wait(&mut owner, &newer, |e| {
        matches!(e, MediaEvent::Prepared { .. })
    });
    assert!(matches!(event,MediaEvent::Prepared{key:k,result:Ok(_)}if k==key));
    assert!(!owner.is_busy());
    owner.join().unwrap();
}
#[test]
fn video_can_play_without_starting_any_audio_owner() {
    let (mut session, mut owner) = (Session::new(), MediaOwner::new().unwrap());
    let media = prepared(&mut owner, &session).with_playback(false, true, false, true);
    let id = source(&mut session, media);
    let (sender, receiver) = mpsc::sync_channel(64);
    owner
        .connect_audio_sink(MediaAudioSink { sender })
        .unwrap_err(); // binding after admission is forbidden
    // Video-only playback may still decode the file's audio without routing it.
    let output = FrameComposition::Scene(FrameScene::Live(session.preview()));
    owner
        .synchronize(&session, &output, MediaTime(0), None)
        .unwrap();
    drain_scope(&mut owner, &session);
    let frame = await_frame(&mut owner, &session, id);
    assert!(frame.frame(id).is_some());
    drop(frame);
    drop(receiver);
    owner.join().unwrap();
}
#[test]
fn real_video_and_aac_share_one_source_clock_loop_and_restart() {
    let mut session = Session::new();
    let (sender, receiver) = mpsc::sync_channel(64);
    let mut owner = MediaOwner::with_audio_sink(MediaAudioSink { sender }).unwrap();
    let media = prepared(&mut owner, &session).with_playback(true, true, false, false);
    let id = source(&mut session, media.clone());
    let output = FrameComposition::Scene(FrameScene::Live(session.preview()));
    let program = ProgramCaptureKey {
        session: session.identity(),
        clock_epoch: 1,
        generation: 1,
        settings: session.snapshot().audio(),
    };
    owner
        .synchronize(&session, &output, MediaTime(0), Some(program))
        .unwrap();
    drain_scope(&mut owner, &session);
    let first = await_frame(&mut owner, &session, id);
    let frame = first.frame(id).unwrap();
    assert_eq!((frame.image.width(), frame.image.height()), (64, 48));
    assert!(frame.image.pixels().chunks_exact(4).all(|p| p[3] == 255));
    assert!(frame.image.pixels().chunks_exact(4).any(|p| p[0] != p[1]));
    let packet = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(packet.source, id);
    assert_eq!(packet.program, program);
    assert_eq!(packet.first_media_ns, 0);
    assert_eq!(packet.format.channels, 1);
    assert!(packet.samples.iter().all(|v| v.is_finite()));
    let generation = packet.stream_generation;
    drop(packet);
    // A forward whole-loop seek does not reset the stable per-source PCM stream.
    owner
        .synchronize(&session, &output, MediaTime(650_000_000), Some(program))
        .unwrap();
    drain_scope(&mut owner, &session);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut loop_packet = None;
    while Instant::now() < deadline {
        if let Ok(packet) = receiver.recv_timeout(Duration::from_millis(10)) {
            if packet.first_media_ns >= 600_000_000 {
                loop_packet = Some(packet);
                break;
            }
        }
    }
    let packet = loop_packet.expect("second loop real AAC");
    assert_eq!(packet.stream_generation, generation);
    assert!(!packet.end);
    drop(packet);
    // This fixture consumes the Program FIFO itself. Restart now waits for an
    // actual Program truncation receipt; dropping its barrier is cancellation.
    let mut program_audio = crate::audio_program::AudioProgram::new(
        session.snapshot().audio(),
        crate::audio_program::ProgramAudioLimits::default(),
    )
    .unwrap();
    program_audio
        .route(&session, &output, MediaTime(0))
        .unwrap();
    owner.restart(&session, id, MediaTime(700_000_000)).unwrap();
    drain_scope(&mut owner, &session);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut restarted = false;
    while Instant::now() < deadline {
        if let Ok(packet) = receiver.recv_timeout(Duration::from_millis(10)) {
            if let Some(barrier) = &packet.barrier {
                let receipt =
                    program_audio.control_media(program_audio.identity().unwrap(), barrier);
                assert!(receipt.is_ok(), "{receipt:?}");
                barrier.reply.try_send(receipt).unwrap();
                continue;
            }
            if !packet.end
                && packet.first_media_ns >= 700_000_000
                && packet.stream_generation > generation
            {
                restarted = true;
                break;
            }
        }
    }
    assert!(restarted);
    drop(first);
    drop(receiver);
    owner.join().unwrap();
}
#[test]
fn default_program_activation_excludes_preview_and_deactivation_clears_pixels() {
    let mut session = Session::new();
    let (sender, receiver) = mpsc::sync_channel(64);
    let mut owner = MediaOwner::with_audio_sink(MediaAudioSink { sender }).unwrap();
    let media = prepared(&mut owner, &session);
    let id = source(&mut session, media);
    let a = session.preview();
    session
        .apply(Command::AddScene {
            name: "Other".into(),
        })
        .unwrap();
    let b = session.snapshot().scenes().last().unwrap().id();
    assert_ne!(a, b);
    let program = ProgramCaptureKey {
        session: session.identity(),
        clock_epoch: 1,
        generation: 1,
        settings: session.snapshot().audio(),
    };
    owner
        .synchronize(
            &session,
            &FrameComposition::Scene(FrameScene::Live(b)),
            MediaTime(0),
            Some(program),
        )
        .unwrap();
    drain_scope(&mut owner, &session);
    thread::sleep(Duration::from_millis(20));
    let _ = owner.poll(&session);
    assert!(owner.snapshot().frame(id).is_none());
    assert!(receiver.try_recv().is_err());
    owner
        .synchronize(
            &session,
            &FrameComposition::Scene(FrameScene::Live(a)),
            MediaTime(30_000_000),
            Some(program),
        )
        .unwrap();
    drain_scope(&mut owner, &session);
    let frame = await_frame(&mut owner, &session, id);
    drop(frame);
    owner
        .synchronize(
            &session,
            &FrameComposition::Scene(FrameScene::Live(b)),
            MediaTime(50_000_000),
            Some(program),
        )
        .unwrap();
    drain_scope(&mut owner, &session);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let _ = owner.poll(&session);
        if owner.snapshot().frame(id).is_none() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    drop(receiver);
    owner.join().unwrap();
}
#[test]
fn direct_file_pcm_transport_continues_without_frontend_polls_or_native_backend() {
    use crate::audio_capture::{AudioCapabilities, AudioCaptureWorker};
    use std::sync::atomic::AtomicUsize;
    let factories = Arc::new(AtomicUsize::new(0));
    let observed = factories.clone();
    let mut audio = AudioCaptureWorker::new(
        Box::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Err(crate::audio_capture::AudioCaptureError::Unsupported(
                "must not instantiate native SDK for file PCM",
            ))
        }),
        AudioCapabilities {
            backend: "no-native-fixture",
            input: false,
            output: false,
            device_enumeration: false,
            exclude_own_audio: false,
            note: None,
        },
    )
    .unwrap();
    let mut owner = MediaOwner::with_audio_sink(audio.media_sink()).unwrap();
    let mut session = Session::new();
    let media = prepared(&mut owner, &session);
    let id = source(&mut session, media.clone());
    session
        .apply(Command::SetAudio {
            source: id,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    let view = FrameScene::Live(session.preview());
    let output = FrameComposition::Scene(view.clone());
    audio.synchronize(&session, &[view]).unwrap();
    audio
        .request_program(&session, &output, MediaTime(0))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while audio.is_busy() {
        let _ = audio.poll(&session).unwrap();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    owner
        .synchronize(&session, &output, MediaTime(0), audio.program_key())
        .unwrap();
    drain_scope(&mut owner, &session);
    // The presentation/control frontend is deliberately idle while both owners
    // continue decoding, transporting and mixing real AAC packets.
    thread::sleep(Duration::from_millis(190));
    let mut decoder = crate::mediafile::MediaFileReader::open(
        std::path::Path::new(media.path()),
        Default::default(),
    )
    .unwrap();
    let mut reference = Vec::new();
    while reference.len() < 8192 {
        reference.extend(decoder.next_audio().unwrap().unwrap().samples);
    }
    let mut frames = 0;
    for _ in 0..8 {
        let Some(output) = audio.poll_program(&session).unwrap() else {
            break;
        };
        assert_eq!(output.block.start_frame, frames as u64);
        assert!(output.block.gaps.is_empty(), "{:?}", output.block.gaps);
        for (sample, expected) in output
            .block
            .samples
            .chunks_exact(2)
            .zip(&reference[frames..])
        {
            assert!((sample[0] - expected * 0.5).abs() < 0.000001);
            assert_eq!(sample[0], sample[1]);
        }
        frames += output.block.frames;
        assert_eq!(output.stats.input_errors, 0);
    }
    assert!(frames >= 6144);
    assert_eq!(factories.load(Ordering::SeqCst), 0);
    owner.close();
    audio.close();
    audio.join().unwrap();
    owner.join().unwrap();
}
#[test]
fn lifecycle_edit_refreshes_frame_binding_without_restarting_pixels_or_source_generation() {
    let mut session = Session::new();
    let (sender, receiver) = mpsc::sync_channel(64);
    let mut owner = MediaOwner::with_audio_sink(MediaAudioSink { sender }).unwrap();
    let media = prepared(&mut owner, &session);
    let id = source(&mut session, media.clone());
    let output = FrameComposition::Scene(FrameScene::Live(session.preview()));
    let key = ProgramCaptureKey {
        session: session.identity(),
        clock_epoch: 1,
        generation: 1,
        settings: session.snapshot().audio(),
    };
    owner
        .synchronize(&session, &output, MediaTime(0), Some(key))
        .unwrap();
    drain_scope(&mut owner, &session);
    let before = await_frame(&mut owner, &session, id);
    let packet = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    let generation = packet.stream_generation;
    drop(packet);
    let changed = media.with_playback(true, false, false, false);
    session
        .apply(Command::RebindMediaSource {
            source: id,
            media: changed.clone(),
        })
        .unwrap();
    owner
        .synchronize(&session, &output, MediaTime(20_000_000), Some(key))
        .unwrap();
    drain_scope(&mut owner, &session);
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        let _ = owner.poll(&session);
        let after = owner.snapshot();
        if let Some(frame) = after.frame(id) {
            if *frame.media == changed {
                assert!(Arc::ptr_eq(&frame.image, &before.frame(id).unwrap().image));
                break;
            }
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    while let Ok(packet) = receiver.try_recv() {
        assert_eq!(packet.stream_generation, generation);
    }
    drop(before);
    drop(receiver);
    owner.join().unwrap();
}
