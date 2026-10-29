use super::*;
fn media() -> MediaSource {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/media/no_pns/cavlc.mp4")
        .canonicalize()
        .unwrap();
    MediaSource::new(path.to_str().unwrap().into(), 64, 48, true).unwrap()
}
fn owner() -> (Owner, mpsc::Receiver<Arc<MediaPcmPacket>>) {
    let (sender, receiver) = mpsc::sync_channel(64);
    let sink = Arc::new(OnceLock::new());
    assert!(sink.set(MediaAudioSink { sender }).is_ok());
    (
        Owner {
            instance: 1,
            key: MediaRequestKey {
                instance: 1,
                session: 1,
                revision: 0,
                epoch: 1,
                request: 1,
            },
            states: HashMap::new(),
            staged_network: None,
            auto_control: None,
            anchor: Some((MediaTime(0), Instant::now())),
            program: Some(ProgramCaptureKey {
                session: 1,
                clock_epoch: 1,
                generation: 1,
                settings: skylive_core::AudioSettings::default(),
            }),
            generation: 0,
            sequence: 0,
            residents: Vec::new(),
            pcm: Vec::new(),
            sink,
            global_problem: None,
            control: None,
            control_late_frames: 0,
            control_late_frames_saturated: false,
            video_round_robin_after: None,
            prefer_video_round_robin: true,
        },
        receiver,
    )
}
#[test]
fn inactive_nonrestarting_close_policy_still_starts_and_activation_keeps_overlapping_aac() {
    let (mut owner, receiver) = owner();
    let media = Arc::new(media().with_playback(false, false, true, true));
    let mut state = SourceState::new(media, false, 0);
    let id = Id::new(2);
    owner
        .step_source(id, &mut state, MediaTime(0), &mut 32usize, &mut 4usize)
        .unwrap();
    assert!(state.reader.is_some());
    assert!(state.running);
    assert!(state.frame.is_some());
    assert!(receiver.try_recv().is_err());
    owner
        .step_source(
            id,
            &mut state,
            MediaTime(50_000_000),
            &mut 32usize,
            &mut 4usize,
        )
        .unwrap();
    assert_eq!(
        state.next_audio.as_ref().unwrap().presentation_ns,
        42_666_666
    );
    assert!(receiver.try_recv().is_err());
    state.active = true;
    let generation = state.stream_generation;
    owner
        .step_source(
            id,
            &mut state,
            MediaTime(50_000_000),
            &mut 32usize,
            &mut 4usize,
        )
        .unwrap();
    let packet = receiver.try_recv().unwrap();
    assert_eq!(packet.first_media_ns, 42_666_666);
    assert_eq!(packet.stream_generation, generation);
    assert_eq!(packet.sequence, 1);
    assert!(packet.first_media_ns + 21_333_333 > 50_000_000);
    let mut reference = MediaFileReader::open(
        std::path::Path::new(packet.media.path()),
        Default::default(),
    )
    .unwrap();
    reference.next_audio().unwrap();
    reference.next_audio().unwrap();
    assert_eq!(
        &*packet.samples,
        &reference.next_audio().unwrap().unwrap().samples
    );
}
#[test]
fn ended_close_policy_and_initial_closed_inactive_policy_do_not_reopen_files() {
    let (mut owner, _receiver) = owner();
    let path = std::env::temp_dir().join("skylive-must-not-open-ended-media-file.mp4");
    let binding = Arc::new(
        MediaSource::new(path.to_str().unwrap().into(), 64, 48, true)
            .unwrap()
            .with_playback(false, true, true, true),
    );
    let mut ended = SourceState::new(binding.clone(), true, 0);
    ended.running = false;
    ended.ended = true;
    owner
        .step_source(
            Id::new(2),
            &mut ended,
            MediaTime(700_000_000),
            &mut 32usize,
            &mut 4usize,
        )
        .unwrap();
    assert!(ended.reader.is_none());
    let mut stopped = SourceState::new(binding, false, 0);
    owner
        .step_source(
            Id::new(3),
            &mut stopped,
            MediaTime(0),
            &mut 32usize,
            &mut 4usize,
        )
        .unwrap();
    assert!(stopped.reader.is_none());
    assert!(!stopped.running);
}
#[test]
fn only_validated_future_picture_or_normal_stop_can_mark_intentional_empty() {
    let (mut owner, _receiver) = owner();
    let binding = Arc::new(media());
    let mut reader =
        MediaFileReader::open(std::path::Path::new(binding.path()), Default::default()).unwrap();
    let picture = reader.next_video().unwrap().unwrap();
    let image = Arc::new(reader.rgba(&picture).unwrap());
    let mut state = SourceState::new(binding, true, 0);
    state.reader = Some(reader);
    state.next_video = Some(PreparedVideo {
        pts: 100_000_000,
        image,
    });
    let id = Id::new(2);
    owner.states.insert(id, state);
    assert!(
        owner
            .snapshot()
            .empty_sources()
            .any(|(source, _)| source == id)
    );
    owner
        .states
        .get_mut(&id)
        .unwrap()
        .next_video
        .as_mut()
        .unwrap()
        .pts = -1;
    assert!(owner.snapshot().empty_sources().next().is_none());
    owner.states.get_mut(&id).unwrap().problem = Some(MediaProblem {
        kind: crate::resource_issue::ResourceIssueKind::Unavailable,
        source: id,
        code: "media_decode",
        detail: "actual decode failure".into(),
    });
    assert!(owner.snapshot().empty_sources().next().is_none());
}
#[test]
fn binding_admission_does_not_insert_ninth_long_path() {
    let mut bindings = HashMap::new();
    let media = MediaSource::new("a".repeat(16_384), 0, 0, true).unwrap();
    for id in 1..=8 {
        insert_binding(&mut bindings, Id::new(id), &media).unwrap();
    }
    let previous = bindings.get(&Id::new(1)).unwrap().clone();
    assert!(matches!(
        insert_binding(&mut bindings, Id::new(9), &media),
        Err(MediaSourceError::Limit(_))
    ));
    assert_eq!(bindings.len(), 8);
    assert!(Arc::ptr_eq(&previous, bindings.get(&Id::new(1)).unwrap()));
}
