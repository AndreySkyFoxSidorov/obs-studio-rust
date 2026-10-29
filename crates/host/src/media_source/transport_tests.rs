//! Deterministic supplied-PCM + actual local-decoder control tests; no devices.
use super::*;
use crate::{
    audio_program::{AudioProgram, MediaProgramInputPacket, ProgramAudioLimits},
    transition::FrameScene,
};
use skylive_core::{AudioSettings, Command, Session, SpeakerLayout};
fn fixture() -> (
    Session,
    Id,
    Arc<MediaSource>,
    Owner,
    mpsc::Receiver<Arc<MediaPcmPacket>>,
    AudioProgram,
) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/media/no_pns/cavlc.mp4")
        .canonicalize()
        .unwrap();
    let mut session = Session::new();
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(48_000, SpeakerLayout::Mono).unwrap(),
        })
        .unwrap();
    let scene = session.preview();
    let media = Arc::new(MediaSource::new(path.to_str().unwrap().into(), 64, 48, true).unwrap());
    session
        .apply(Command::AddMediaSource {
            scene,
            name: "Controlled".into(),
            media: (*media).clone(),
        })
        .unwrap();
    let id = session.snapshot().sources()[0].id();
    let (sender, receiver) = mpsc::sync_channel(8);
    let sink = Arc::new(OnceLock::new());
    assert!(sink.set(MediaAudioSink { sender }).is_ok());
    let program_key = ProgramCaptureKey {
        session: session.identity(),
        clock_epoch: 1,
        generation: 1,
        settings: session.snapshot().audio(),
    };
    let mut owner = Owner {
        instance: 1,
        key: MediaRequestKey {
            instance: 1,
            session: session.identity(),
            revision: session.snapshot().revision(),
            epoch: 1,
            request: 1,
        },
        states: HashMap::new(),
        staged_network: None,
        auto_control: None,
        anchor: None,
        program: Some(program_key),
        generation: 0,
        sequence: 1,
        residents: Vec::new(),
        pcm: Vec::new(),
        sink,
        global_problem: None,
        control: None,
        control_late_frames: 0,
        control_late_frames_saturated: false,
        video_round_robin_after: None,
        prefer_video_round_robin: true,
    };
    let mut source = SourceState::new(media.clone(), true, 0);
    source.reader = Some(
        MediaFileReader::open(std::path::Path::new(media.path()), Default::default()).unwrap(),
    );
    source.frame = {
        let picture = source
            .reader
            .as_mut()
            .unwrap()
            .next_video()
            .unwrap()
            .unwrap();
        Some(MediaFrame {
            stream_generation:source.stream_generation,
            media: media.clone(),
            image: Arc::new(source.reader.as_ref().unwrap().rgba(&picture).unwrap()),
            sequence: 1,
            requested_at: MediaTime(0),
        })
    };
    source.audio_cursor = Some(control::AudioCursor {
        program: program_key,
        generation: 1,
        sequence: 1,
        suspended: false,
    });
    source.sequence = 1;
    owner.states.insert(id, source);
    let mut program =
        AudioProgram::new(session.snapshot().audio(), ProgramAudioLimits::default()).unwrap();
    program
        .route(
            &session,
            &FrameComposition::Scene(FrameScene::Live(scene)),
            MediaTime(0),
        )
        .unwrap();
    (session, id, media, owner, receiver, program)
}
fn target(owner: &Owner, id: Id) -> MediaControlTarget {
    owner
        .snapshot()
        .control_target_for(
            owner.key.session,
            owner.key.revision,
            id,
            &owner.states[&id].media,
        )
        .unwrap()
}
fn begin(
    owner: &mut Owner,
    id: Id,
    operation: MediaControl,
    at: u64,
) -> Result<Option<MediaTransportStatus>, MediaSourceError> {
    let target = target(owner, id);
    let key = MediaRequestKey {
        request: owner.key.request + 1,
        ..owner.key
    };
    owner.begin_control_at(key, target, operation, at)
}
fn acknowledge(receiver: &mpsc::Receiver<Arc<MediaPcmPacket>>, program: &mut AudioProgram) {
    let packet = receiver.try_recv().unwrap();
    let barrier = packet.barrier.as_ref().unwrap();
    let result = program.control_media(program.identity().unwrap(), barrier);
    assert!(result.is_ok(), "{result:?}");
    barrier.reply.try_send(result).unwrap();
}
fn ingest(program: &mut AudioProgram, packet: &MediaPcmPacket) {
    program
        .ingest_media(MediaProgramInputPacket {
            identity: program.identity().unwrap(),
            source: packet.source,
            media: &packet.media,
            stream_generation: packet.stream_generation,
            sequence: packet.sequence,
            first_media_ns: packet.first_media_ns,
            received_media_ns: packet.first_media_ns,
            format: packet.format,
            samples: &packet.samples[packet.sample_offset..],
            flags: Default::default(),
        })
        .unwrap();
}
fn supplied(owner: &mut Owner, id: Id, media: &Arc<MediaSource>) -> *const f32 {
    let samples: Vec<f32> = (0..4096).map(|i| i as f32 / 8192.0).collect();
    let ptr = samples.as_ptr();
    let packet = Arc::new(MediaPcmPacket {
        program: owner.program.unwrap(),
        source: id,
        media: media.clone(),
        stream_generation: 1,
        sequence: 1,
        first_media_ns: 0,
        format: NativeAudioFormat {
            sample_rate: 48_000,
            channels: 1,
            channel_mask: 4,
        },
        samples: samples.into_boxed_slice(),
        sample_offset: 0,
        end: false,
        barrier: None,
    });
    owner.pcm.push(packet);
    ptr
}
#[test]
fn pause_truncates_predecoded_future_and_resume_moves_same_pcm_allocation_without_gop_rewind() {
    let (session, id, media, mut owner, receiver, mut program) = fixture();
    let model = session.snapshot();
    let ptr = supplied(&mut owner, id, &media);
    ingest(&mut program, &owner.pcm[0]);
    let first = program.render(0, 128).unwrap();
    assert_eq!(first.samples[127], 127.0 / 8192.0);
    let image = owner.states[&id].frame.as_ref().unwrap().image.clone();
    let original_start = owner.states[&id].start_ns;
    assert!(
        begin(&mut owner, id, MediaControl::Pause, 5_000_000)
            .unwrap()
            .is_none()
    );
    assert!(
        owner.states[&id].running,
        "queue acceptance is not completion"
    );
    acknowledge(&receiver, &mut program);
    let (_, _, result) = owner.poll_control().unwrap();
    assert_eq!(result.unwrap().state, MediaPlaybackState::Paused);
    let held = program.render(128, 384).unwrap();
    assert_eq!(
        &held.samples[..112],
        &(128..240).map(|i| i as f32 / 8192.0).collect::<Vec<_>>()
    );
    assert!(held.samples[112..].iter().all(|v| *v == 0.0));
    assert!(held.gaps.is_empty());
    assert!(Arc::ptr_eq(
        &image,
        &owner.states[&id].frame.as_ref().unwrap().image
    ));
    assert!(
        begin(&mut owner, id, MediaControl::Resume, 25_000_000)
            .unwrap()
            .is_none()
    );
    acknowledge(&receiver, &mut program);
    assert_eq!(
        owner.poll_control().unwrap().2.unwrap().state,
        MediaPlaybackState::Playing
    );
    let mut source = owner.states.remove(&id).unwrap();
    assert_eq!(source.start_ns, original_start + 20_000_000);
    assert!(Arc::ptr_eq(&image, &source.frame.as_ref().unwrap().image));
    assert!(
        owner
            .replay_pcm(id, &mut source, &mut 4, MediaTime(25_000_000))
            .unwrap()
    );
    let packet = receiver.try_recv().unwrap();
    assert_eq!(
        packet.samples.as_ptr(),
        ptr,
        "resume must move the decoded allocation, not clone it"
    );
    assert_eq!(
        (
            packet.first_media_ns,
            packet.sample_offset,
            packet.stream_generation,
            packet.sequence
        ),
        (25_000_000, 240, 3, 1)
    );
    ingest(&mut program, &packet);
    owner.states.insert(id, source);
    let resumed = program.render(512, 1024).unwrap();
    assert!(resumed.samples[..688].iter().all(|v| *v == 0.0));
    assert_eq!(
        &resumed.samples[688..],
        &(240..576).map(|i| i as f32 / 8192.0).collect::<Vec<_>>()
    );
    assert!(resumed.gaps.is_empty());
    assert!(
        Arc::ptr_eq(&model, &session.snapshot()),
        "transport creates no authored revision or Undo"
    );
}
#[test]
fn barrier_queue_full_stale_binding_foreign_session_and_absent_source_reject_before_mutation() {
    let (_, id, media, mut owner, _receiver, _) = fixture();
    // Fill the actual bounded sink; no second queue or success placeholder.
    let (sender, _blocked) = mpsc::sync_channel(1);
    let sink = Arc::new(OnceLock::new());
    assert!(sink.set(MediaAudioSink { sender }).is_ok());
    owner.sink = sink;
    supplied(&mut owner, id, &media);
    assert!(owner.sink.get().unwrap().send(owner.pcm[0].clone()).is_ok());
    let frame = owner.states[&id].frame.as_ref().unwrap().image.clone();
    let key = owner.key;
    let generation = owner.generation;
    assert!(matches!(
        begin(&mut owner, id, MediaControl::Pause, 5_000_000),
        Err(MediaSourceError::Busy)
    ));
    assert!(owner.control.is_none() && owner.states[&id].running);
    assert_eq!(owner.generation, generation);
    assert_eq!(owner.key, key);
    assert!(Arc::ptr_eq(
        &frame,
        &owner.states[&id].frame.as_ref().unwrap().image
    ));
    let mut stale = target(&owner, id);
    stale.status.stream_generation += 1;
    assert!(
        owner
            .begin_control_at(
                MediaRequestKey { request: 2, ..key },
                stale,
                MediaControl::Stop,
                5_000_000
            )
            .is_err()
    );
    let mut foreign = target(&owner, id);
    foreign.session += 1;
    assert!(
        owner
            .begin_control_at(
                MediaRequestKey { request: 2, ..key },
                foreign,
                MediaControl::Stop,
                5_000_000
            )
            .is_err()
    );
    let mut missing = target(&owner, id);
    missing.source = Id::new(999);
    assert!(
        owner
            .begin_control_at(
                MediaRequestKey { request: 2, ..key },
                missing,
                MediaControl::Stop,
                5_000_000
            )
            .is_err()
    );
    assert_eq!(owner.key, key);
    assert_eq!(owner.generation, generation);
}
#[test]
fn stop_clears_only_target_pixels_and_restart_seeks_only_target_with_new_generation() {
    let (_, id, _, mut owner, _, _) = fixture();
    owner.program = None;
    let other = Id::new(900);
    let media = owner.states[&id].media.clone();
    owner
        .states
        .insert(other, SourceState::new(media, true, 123));
    let other_generation = owner.states[&other].stream_generation;
    begin(&mut owner, id, MediaControl::Stop, 10_000_000).unwrap();
    owner.poll_control().unwrap().2.unwrap();
    assert_eq!(
        owner.states[&id].transport_status().state,
        MediaPlaybackState::Stopped
    );
    assert!(owner.states[&id].frame.is_none());
    let snap = owner.snapshot();
    assert!(snap.empty_sources().any(|(source, _)| source == id));
    assert!(snap.frame(id).is_none());
    assert_eq!(owner.states[&other].start_ns, 123);
    assert_eq!(owner.states[&other].stream_generation, other_generation);
    let generation = owner.states[&id].stream_generation;
    assert!(
        begin(&mut owner, id, MediaControl::Stop, 11_000_000)
            .unwrap()
            .is_some()
    );
    assert_eq!(owner.states[&id].stream_generation, generation);
    begin(&mut owner, id, MediaControl::Restart, 20_000_000).unwrap();
    owner.poll_control().unwrap().2.unwrap();
    assert_eq!(owner.states[&id].start_ns, 20_000_000);
    assert!(owner.states[&id].running);
    assert_eq!(owner.states[&id].stream_generation, generation + 1);
    assert_eq!(owner.states[&other].start_ns, 123);
    assert_eq!(owner.states[&other].stream_generation, other_generation);
}

#[test]
fn late_pause_never_replays_the_interval_that_program_already_emitted() {
    let (_, id, media, mut owner, receiver, mut program) = fixture();
    supplied(&mut owner, id, &media);
    ingest(&mut program, &owner.pcm[0]);
    program.render(0, 1024).unwrap();
    begin(&mut owner, id, MediaControl::Pause, 5_000_000).unwrap();
    acknowledge(&receiver, &mut program);
    let status = owner.poll_control().unwrap().2.unwrap();
    let applied = status.last_control.unwrap();
    assert_eq!(
        (
            applied.audio_frame,
            applied.effective_ns,
            applied.late_frames
        ),
        (Some(1024), 21_333_333, 784)
    );
    program.render(1024, 128).unwrap();
    begin(&mut owner, id, MediaControl::Resume, 50_000_000).unwrap();
    acknowledge(&receiver, &mut program);
    owner.poll_control().unwrap().2.unwrap();
    let mut source = owner.states.remove(&id).unwrap();
    assert!(
        owner
            .replay_pcm(id, &mut source, &mut 4, MediaTime(50_000_000))
            .unwrap()
    );
    let packet = receiver.try_recv().unwrap();
    assert_eq!(
        (packet.sample_offset, packet.first_media_ns),
        (1024, 50_000_000)
    );
    ingest(&mut program, &packet);
    owner.states.insert(id, source);
    assert!(
        program
            .render(1152, 1024)
            .unwrap()
            .samples
            .iter()
            .all(|v| *v == 0.0)
    );
    let resumed = program.render(2176, 512).unwrap();
    assert!(resumed.samples[..224].iter().all(|v| *v == 0.0));
    assert_eq!(
        &resumed.samples[224..],
        &(1024..1312).map(|i| i as f32 / 8192.0).collect::<Vec<_>>()
    );
    assert!(resumed.gaps.is_empty());
}
#[test]
fn pending_control_reserves_visual_identity_and_late_stat_overflow_is_explicit_not_failed_commit() {
    let (_, id, media, mut owner, receiver, mut program) = fixture();
    supplied(&mut owner, id, &media);
    ingest(&mut program, &owner.pcm[0]);
    program.render(0, 1024).unwrap();
    owner.generation = u64::MAX - 3;
    owner.control_late_frames = u64::MAX;
    owner.states.insert(
        Id::new(900),
        SourceState::new(
            Arc::new(
                media
                    .as_ref()
                    .clone()
                    .with_playback(false, false, false, true),
            ),
            false,
            0,
        ),
    );
    begin(&mut owner, id, MediaControl::Pause, 5_000_000).unwrap();
    owner.step().unwrap();
    assert_eq!(owner.generation, u64::MAX - 2);
    owner.step().unwrap();
    assert_eq!(
        owner.generation,
        u64::MAX - 2,
        "other source work cannot spend the reserved commit identity"
    );
    acknowledge(&receiver, &mut program);
    assert_eq!(
        owner.poll_control().unwrap().2.unwrap().state,
        MediaPlaybackState::Paused
    );
    assert_eq!(owner.generation, u64::MAX - 1);
    assert_eq!(owner.control_late_frames, u64::MAX);
    assert!(owner.control_late_frames_saturated);
    owner.step().unwrap();
    assert_eq!(owner.generation, u64::MAX);
    assert!(owner.states.is_empty());
    assert_eq!(
        owner.global_problem.as_ref().unwrap().code,
        "media_source_identity_exhausted"
    );
}
#[test]
fn exhausted_control_admission_never_sends_a_barrier_and_defensive_post_barrier_fault_retires_media()
 {
    let (_, id, media, mut owner, receiver, mut program) = fixture();
    supplied(&mut owner, id, &media);
    ingest(&mut program, &owner.pcm[0]);
    owner.generation = u64::MAX - 1;
    let before = program.status().data_generation;
    assert!(matches!(
        begin(&mut owner, id, MediaControl::Pause, 5_000_000),
        Err(MediaSourceError::IdentityExhausted)
    ));
    assert!(receiver.try_recv().is_err());
    assert_eq!(program.status().data_generation, before);
    assert!(owner.states[&id].running);
    owner.generation = 0;
    begin(&mut owner, id, MediaControl::Pause, 5_000_000).unwrap();
    acknowledge(&receiver, &mut program);
    // Corrupt a private invariant deliberately: public scheduling reserves credit.
    owner.generation = u64::MAX;
    assert!(matches!(
        owner.poll_control().unwrap().2,
        Err(MediaSourceError::ControlRetired(_))
    ));
    assert!(owner.states.is_empty());
    assert_eq!(
        owner.global_problem.as_ref().unwrap().code,
        "media_source_control_retired"
    );
}
#[test]
fn close_retires_pending_control_even_when_an_external_peer_keeps_the_ack_sender_alive() {
    let (mut session, id, media, old, old_receiver, program) = fixture();
    drop(old);
    drop(old_receiver);
    drop(program);
    session
        .apply(Command::RebindMediaSource {
            source: id,
            media: media
                .as_ref()
                .clone()
                .with_playback(true, true, false, true),
        })
        .unwrap();
    let (sender, receiver) = mpsc::sync_channel(64);
    let mut frontend = MediaOwner::with_audio_sink(MediaAudioSink { sender }).unwrap();
    let key = ProgramCaptureKey {
        session: session.identity(),
        clock_epoch: 1,
        generation: 1,
        settings: session.snapshot().audio(),
    };
    frontend
        .synchronize(
            &session,
            &FrameComposition::Scene(FrameScene::Live(session.preview())),
            MediaTime(0),
            Some(key),
        )
        .unwrap();
    let until = Instant::now() + std::time::Duration::from_secs(3);
    while frontend.is_busy() || frontend.snapshot().frame(id).is_none() {
        frontend.poll(&session).unwrap();
        assert!(Instant::now() < until);
        thread::sleep(std::time::Duration::from_millis(1));
    }
    let target = frontend.control_target(&session, id).unwrap();
    frontend
        .control(&session, target, MediaControl::Pause, MediaTime(0))
        .unwrap();
    let pending = loop {
        let packet = receiver
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        if packet.barrier.is_some() {
            break packet;
        }
    };
    frontend.close();
    drop(receiver);
    let (done, finished) = mpsc::sync_channel(1);
    let join = thread::spawn(move || {
        frontend.join().unwrap();
        assert!(done.try_send(()).is_ok());
    });
    // Holding only the barrier metadata/ack must not pin old frame/PCM residents.
    finished
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    drop(pending);
    join.join().unwrap();
}

#[test]
fn paused_source_parks_a_replacement_program_and_activation_uses_the_same_acknowledged_generation_path()
 {
    let (session, id, media, mut owner, receiver, mut first) = fixture();
    supplied(&mut owner, id, &media);
    ingest(&mut first, &owner.pcm[0]);
    begin(&mut owner, id, MediaControl::Pause, 5_000_000).unwrap();
    acknowledge(&receiver, &mut first);
    owner.poll_control().unwrap().2.unwrap();
    let mut source = owner.states.remove(&id).unwrap();
    owner.program = Some(ProgramCaptureKey {
        generation: 2,
        ..owner.program.unwrap()
    });
    source.audio_cursor = None;
    source.audio_sync = None;
    source.stream_generation += 1;
    source.sequence = 0;
    source.audio_sync_at = 20_000_000;
    let mut second =
        AudioProgram::new(session.snapshot().audio(), ProgramAudioLimits::default()).unwrap();
    second
        .route(
            &session,
            &FrameComposition::Scene(FrameScene::Live(session.preview())),
            MediaTime(20_000_000),
        )
        .unwrap();
    owner
        .sync_source_audio(id, &mut source, MediaTime(20_000_000))
        .unwrap();
    assert!(source.audio_blocked);
    acknowledge(&receiver, &mut second);
    owner
        .sync_source_audio(id, &mut source, MediaTime(20_000_000))
        .unwrap();
    assert!(!source.audio_blocked);
    assert_eq!(source.transport_status().state, MediaPlaybackState::Paused);
    source.restart(30_000_000).unwrap();
    owner
        .sync_source_audio(id, &mut source, MediaTime(30_000_000))
        .unwrap();
    assert!(source.audio_blocked);
    acknowledge(&receiver, &mut second);
    owner
        .sync_source_audio(id, &mut source, MediaTime(30_000_000))
        .unwrap();
    assert!(!source.audio_blocked);
    let data = vec![0.25; 1024];
    second
        .ingest_media(MediaProgramInputPacket {
            identity: second.identity().unwrap(),
            source: id,
            media: &media,
            stream_generation: source.stream_generation,
            sequence: 1,
            first_media_ns: 30_000_000,
            received_media_ns: 30_000_000,
            format: NativeAudioFormat {
                sample_rate: 48_000,
                channels: 1,
                channel_mask: 4,
            },
            samples: &data,
            flags: Default::default(),
        })
        .unwrap();
    let output = second.render(960, 1024).unwrap();
    assert!(output.samples[..480].iter().all(|v| *v == 0.0));
    assert!(output.samples[480..].iter().all(|v| *v == 0.25));
    assert!(output.gaps.is_empty());
}
