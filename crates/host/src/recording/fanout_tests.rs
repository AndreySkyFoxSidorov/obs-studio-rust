//! Owner-to-owner transport oracles; all native interfaces are injected fakes.
use super::*;
use crate::audio_capture::{
    AudioCapabilities, AudioCaptureError, AudioCaptureLimits, AudioCaptureWorker, AudioDeviceInfo,
    NativeAudioBackend, NativeAudioFormat, NativeAudioStream, NativePacketFlags, NativePcmBlock,
    ProgramCaptureKey,
};
use skylive_core::{AudioBuffering, AudioCaptureBinding, AudioFilter, AudioFlow, AudioTarget};
use std::{collections::VecDeque, sync::atomic::AtomicUsize};

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[path = "fanout_native_acceptance.rs"]
mod native_acceptance;

fn transport(session: &Session) -> ProgramCaptureKey {
    ProgramCaptureKey {
        session: session.identity(),
        clock_epoch: 1,
        generation: 1,
        settings: session.snapshot().audio(),
    }
}

#[test]
fn historical_cut_can_record_sixty_six_source_metrics_within_one_output_block() {
    use crate::{
        audio_program::{AudioProgram, ProgramAudioLimits, ProgramInputPacket},
        transition::{FrameComposition, FrameScene},
    };
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let a = session.preview();
    let b = session
        .apply(Command::AddScene {
            name: "Second audio route".into(),
        })
        .unwrap()
        .created
        .unwrap();
    let binding =
        AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, true, false).unwrap();
    for (scene, label) in [(a, "A"), (b, "B")] {
        for index in 0..33 {
            session
                .apply(Command::AddAudioCaptureSource {
                    scene,
                    name: format!("{label}{index}"),
                    binding: binding.clone(),
                })
                .unwrap();
        }
    }
    let mut program =
        AudioProgram::new(session.snapshot().audio(), ProgramAudioLimits::default()).unwrap();
    program
        .route(
            &session,
            &FrameComposition::Scene(FrameScene::Live(a)),
            MediaTime(0),
        )
        .unwrap();
    program
        .route(
            &session,
            &FrameComposition::Scene(FrameScene::Live(b)),
            MediaTime(10_000_000),
        )
        .unwrap();
    let samples = vec![0.01; 2048];
    program
        .ingest(ProgramInputPacket {
            identity: program.identity().unwrap(),
            binding: &binding,
            stream_generation: 1,
            sequence: 1,
            first_media_ns: 0,
            received_media_ns: 0,
            format: NativeAudioFormat {
                sample_rate: 48000,
                channels: 2,
                channel_mask: 3,
            },
            samples: &samples,
            flags: NativePacketFlags::default(),
        })
        .unwrap();
    let block = program.render(0, 1024).unwrap();
    assert_eq!(
        block.source_meters.len(),
        66,
        "metric union spans both33-source routes"
    );
    assert!(crate::recording::audio_sink::block_charge(&block).is_ok());
    let calls = Arc::new(Mutex::new(vec![]));
    let mut recorder = RecordingOwner::new().unwrap();
    recorder
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(calls.clone(), false, None),
        )
        .unwrap();
    recorder.video(frame(&mut session, 0)).unwrap();
    let mut sink = recorder.audio_sink(session.snapshot().audio()).unwrap();
    assert!(sink.submit(transport(&session), Arc::new(block)));
    recorder.stop().unwrap();
    drop(sink);
    wait_for(|| finished(&calls));
    assert!(matches!(settle(&mut recorder), RecordingEvent::Started));
    assert!(matches!(
        settle(&mut recorder),
        RecordingEvent::Finished {
            audio_frames: 1024,
            ..
        }
    ));
    recorder.join().unwrap();
}
fn block(session: &Session, start: u64, frames: usize) -> Arc<ProgramAudioBlock> {
    let mut block = Arc::try_unwrap(pcm(session, start)).unwrap();
    block.frames = frames;
    block.samples = vec![0.01; frames * 2];
    Arc::new(block)
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < until, "owner fanout deadline");
        thread::sleep(Duration::from_millis(2));
    }
}
fn finished(calls: &Arc<Mutex<Vec<Call>>>) -> bool {
    calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| matches!(call, Call::Finalize))
}

#[test]
fn end_barrier_waits_for_the_unique_producer_then_finishes_without_reading_started() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let calls = Arc::new(Mutex::new(vec![]));
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(calls.clone(), false, None),
        )
        .unwrap();
    owner.video(frame(&mut session, 0)).unwrap();
    let mut sink = owner.audio_sink(session.snapshot().audio()).unwrap();
    let key = transport(&session);
    for index in 0..16 {
        assert!(sink.submit(key, block(&session, index * 960, 960)));
    }
    owner.stop().unwrap();
    wait_for(|| {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| matches!(call, Call::Audio(..)))
            .count()
            == 16
    });
    assert!(
        !finished(&calls),
        "Stop cannot finalize ahead of the source end barrier"
    );
    drop(sink);
    wait_for(|| finished(&calls));
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    assert!(matches!(
        settle(&mut owner),
        RecordingEvent::Finished {
            audio_frames: 15360,
            ..
        }
    ));
    assert_eq!(owner.status().audio_gap_frames, 0);
    assert!(directory.output().exists());
    owner.join().unwrap();
}

#[test]
fn full_audio_queue_fault_drains_exact_prefix_and_does_not_need_gui_stop_retry() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let calls = Arc::new(Mutex::new(vec![]));
    let (release, gate) = mpsc::sync_channel(0);
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(calls.clone(), false, Some(gate)),
        )
        .unwrap();
    wait_for(|| !calls.lock().unwrap().is_empty());
    owner.video(frame(&mut session, 0)).unwrap();
    let mut sink = owner.audio_sink(session.snapshot().audio()).unwrap();
    let key = transport(&session);
    assert!(
        owner.audio(pcm(&session, 0)).is_err(),
        "manual/live ingress must not mix"
    );
    for index in 0..40 {
        assert!(sink.submit(key, block(&session, index * 960, 960)));
    }
    assert!(!sink.submit(key, block(&session, 40 * 960, 960)));
    owner.stop().unwrap();
    drop(sink);
    release.send(()).unwrap();
    wait_for(|| finished(&calls));
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    let RecordingEvent::Failed(error) = settle(&mut owner) else {
        panic!("overload must not report silent success");
    };
    assert_eq!(error.code, "recording_audio_backpressure");
    assert_eq!(error.recovery_path.as_ref(), Some(&directory.output()));
    assert_eq!(owner.status().audio_frames, 40 * 960);
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| matches!(call, Call::Audio(..)))
            .count(),
        40
    );
    assert!(directory.output().exists());
    owner.join().unwrap();
}

#[test]
fn route_changes_keep_the_stream_policy_migration_is_explicit_and_old_generation_is_rejected() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let calls = Arc::new(Mutex::new(vec![]));
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(calls.clone(), false, None),
        )
        .unwrap();
    owner.video(frame(&mut session, 0)).unwrap();
    let mut sink = owner.audio_sink(session.snapshot().audio()).unwrap();
    let key = transport(&session);
    assert!(sink.submit(key, block(&session, 0, 960)));
    let mut next = Arc::try_unwrap(block(&session, 960, 960)).unwrap();
    next.routing_serial = 99;
    next.data_generation = 100;
    assert!(
        sink.submit(key, Arc::new(next)),
        "ordinary authored routes are not stream identity"
    );
    let mut policy = key;
    policy.generation += 1;
    policy.settings = key
        .settings
        .with_buffering(AudioBuffering::new(137, true).unwrap())
        .unwrap();
    sink.bind(policy).unwrap();
    let mut next = Arc::try_unwrap(block(&session, 2880, 960)).unwrap();
    next.settings = policy.settings;
    next.instance = 2;
    assert!(sink.submit(policy, Arc::new(next)));
    assert!(
        !sink.submit(key, block(&session, 3840, 960)),
        "stale transport generation cannot rebind backwards"
    );
    drop(sink);
    wait_for(|| finished(&calls));
    assert!(matches!(settle(&mut owner), RecordingEvent::Started));
    let RecordingEvent::Failed(error) = settle(&mut owner) else {
        panic!("stale timeline must stop explicitly");
    };
    assert_eq!(error.code, "recording_audio_scope");
    assert_eq!(owner.status().audio_gap_frames, 960);
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|call| if let Call::Audio(first, ..) = call {
                Some(*first)
            } else {
                None
            })
            .collect::<Vec<_>>(),
        vec![0, 960, 2880]
    );
    owner.join().unwrap();
}

#[test]
fn weak_producer_cannot_keep_closed_owner_alive_with_unread_started_or_overcapacity_metadata() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let calls = Arc::new(Mutex::new(vec![]));
    let mut owner = RecordingOwner::new().unwrap();
    owner
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(calls.clone(), false, None),
        )
        .unwrap();
    owner.video(frame(&mut session, 0)).unwrap();
    let sink = owner.audio_sink(session.snapshot().audio()).unwrap();
    // join closes ingress while a Weak producer remains alive. It must drain
    // accepted work and release the fake writer without any event consumer.
    owner.join().unwrap();
    assert!(finished(&calls));
    drop(sink);
    let mut bad = Arc::try_unwrap(pcm(&session, 0)).unwrap();
    bad.gaps = Vec::with_capacity(129);
    assert!(crate::recording::audio_sink::block_charge(&bad).is_err());
    bad.gaps = vec![];
    bad.source_meters = Vec::with_capacity(129);
    assert!(crate::recording::audio_sink::block_charge(&bad).is_err());
}

const CAPS: AudioCapabilities = AudioCapabilities {
    backend: "recording-test",
    input: true,
    output: false,
    device_enumeration: false,
    exclude_own_audio: false,
    note: None,
};
struct Backend {
    packets: Arc<Mutex<VecDeque<NativePcmBlock>>>,
    opens: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    started: Instant,
    owner: thread::ThreadId,
    _local: Rc<()>,
}
struct Stream {
    packets: Arc<Mutex<VecDeque<NativePcmBlock>>>,
    closes: Arc<AtomicUsize>,
    owner: thread::ThreadId,
    closed: bool,
    zero: u64,
    started: Instant,
    _local: Rc<()>,
}
impl NativeAudioBackend for Backend {
    fn capabilities(&self) -> AudioCapabilities {
        CAPS
    }
    fn host_now_ns(&self) -> Result<u64, AudioCaptureError> {
        assert_eq!(self.owner, thread::current().id());
        Ok(1_000_000_000 + self.started.elapsed().as_nanos() as u64)
    }
    fn enumerate(
        &mut self,
        _: AudioFlow,
        _: AudioCaptureLimits,
    ) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        Ok(vec![])
    }
    fn open(
        &mut self,
        _: &AudioCaptureBinding,
        _: AudioCaptureLimits,
    ) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
        assert_eq!(self.owner, thread::current().id());
        self.opens.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(Stream {
            packets: self.packets.clone(),
            closes: self.closes.clone(),
            owner: self.owner,
            closed: false,
            zero: self.host_now_ns()?,
            started: self.started,
            _local: Rc::new(()),
        }))
    }
}
impl NativeAudioStream for Stream {
    fn poll(&mut self) -> Result<Option<NativePcmBlock>, AudioCaptureError> {
        assert_eq!(self.owner, thread::current().id());
        let Some(mut block) = self.packets.lock().unwrap().pop_front() else {
            return Ok(None);
        };
        // Device timestamps use the backend's HOST clock domain. The fixture
        // queue stores relative positions, just like continuous_tests; passing
        // them as absolute timestamps would place every packet one second late.
        block.device_timestamp_ns = block
            .device_timestamp_ns
            .map(|time| {
                self.zero
                    .checked_add(time)
                    .ok_or(AudioCaptureError::TimeOverflow)
            })
            .transpose()?;
        block.host_received_ns = 1_000_000_000 + self.started.elapsed().as_nanos() as u64;
        Ok(Some(block))
    }
    fn close(&mut self) -> Result<(), AudioCaptureError> {
        assert_eq!(self.owner, thread::current().id());
        if !self.closed {
            self.closes.fetch_add(1, Ordering::Relaxed);
            self.closed = true;
        }
        Ok(())
    }
}

#[test]
fn real_program_owner_records_gate_eq_pcm_past_eight_unread_gui_blocks_without_duplicate_capture() {
    let directory = Directory::new();
    let (mut session, config) = setup(directory.output());
    let scene = session.preview();
    let binding =
        AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, true, false).unwrap();
    let item = session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Synthetic voice".into(),
            binding,
        })
        .unwrap()
        .created
        .unwrap();
    let source = session
        .snapshot()
        .scene(scene)
        .unwrap()
        .items()
        .iter()
        .find(|entry| entry.id() == item)
        .unwrap()
        .source();
    let gate = AudioFilter::Gate {
        threshold_db: -20.0,
        ratio: 2.0,
        range_db: 60.0,
        attack_ms: 1.0,
        hold_ms: 0.0,
        release_ms: 1.0,
        enabled: true,
    };
    let mut eq = AudioFilter::simple_eq();
    if let AudioFilter::SimpleEq { mid_db, .. } = &mut eq {
        *mid_db = 6.0;
    }
    session
        .apply(Command::SetSourceAudioFilters {
            source,
            filters: vec![eq, gate],
        })
        .unwrap();
    session
        .apply(Command::AddExistingSource { scene, source })
        .unwrap();
    session
        .apply(Command::SetGlobalAudio {
            slot: skylive_core::AudioSlot::Mic1,
            source: Some(source),
        })
        .unwrap();
    let started = Instant::now();
    let packets = Arc::new(Mutex::new(VecDeque::new()));
    for index in 0..64 {
        packets.lock().unwrap().push_back(NativePcmBlock {
            format: NativeAudioFormat {
                sample_rate: 48000,
                channels: 2,
                channel_mask: 3,
            },
            frames: 256,
            samples: vec![0.01; 512],
            device_timestamp_ns: Some(index * 256 * 1_000_000_000 / 48000),
            graph_timestamp_ns: None,
            directshow_clock: None,
            host_received_ns: 1_000_000_000,
            device_frame_position: Some(index * 256),
            flags: NativePacketFlags::default(),
        });
    }
    let opens = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let (p, o, c) = (packets.clone(), opens.clone(), closes.clone());
    let mut audio = AudioCaptureWorker::with_limits(
        Box::new(move || {
            Ok(Box::new(Backend {
                packets: p,
                opens: o,
                closes: c,
                started,
                owner: thread::current().id(),
                _local: Rc::new(()),
            }))
        }),
        CAPS,
        AudioCaptureLimits::default(),
    )
    .unwrap();
    audio
        .synchronize(&session, &[crate::transition::FrameScene::Live(scene)])
        .unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let mut recorder = RecordingOwner::new().unwrap();
    recorder
        .start_with(
            session.identity(),
            config,
            MediaTime(0),
            factory(calls.clone(), false, None),
        )
        .unwrap();
    recorder.video(frame(&mut session, 0)).unwrap();
    let sink = recorder.audio_sink(session.snapshot().audio()).unwrap();
    audio
        .request_program_with_recording(
            &session,
            &crate::transition::FrameComposition::Scene(crate::transition::FrameScene::Live(scene)),
            MediaTime(0),
            sink,
        )
        .unwrap();
    // Intentionally never poll audio control, Program output, or Started. The
    // presentation queue fills at8, but the recording transport must continue.
    wait_for(|| {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| matches!(call, Call::Audio(..)))
            .count()
            >= 12
    });
    recorder.stop().unwrap();
    wait_for(|| finished(&calls));
    assert!(matches!(settle(&mut recorder), RecordingEvent::Started));
    assert!(matches!(
        settle(&mut recorder),
        RecordingEvent::Finished { .. }
    ));
    let calls = calls.lock().unwrap();
    let blocks: Vec<_> = calls
        .iter()
        .filter_map(|call| {
            if let Call::Audio(first, pcm) = call {
                Some((*first, pcm))
            } else {
                None
            }
        })
        .collect();
    assert!(blocks.len() >= 12);
    for pair in blocks.windows(2) {
        assert_eq!(pair[1].0, pair[0].0 + pair[0].1.len() as u64 / 2);
    }
    assert!(
        blocks
            .iter()
            .skip(4)
            .take(4)
            .flat_map(|(_, pcm)| pcm.iter())
            .any(|&sample| sample == 33),
        "literal -40dB input with2:1 gate below -20dB settles to -60dB, rounded i16=33"
    );
    assert_eq!(recorder.status().audio_gap_frames, 0);
    drop(calls);
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    recorder.join().unwrap();
    audio.join().unwrap();
    assert_eq!(closes.load(Ordering::Relaxed), 1);
}
