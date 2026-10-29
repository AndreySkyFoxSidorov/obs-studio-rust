//! Public-worker integration with owner-bound fake native streams. No devices.
use super::*;
use crate::transition::{FrameComposition, FrameScene};
use skylive_core::{AudioSettings, Command, Id, Session, SpeakerLayout};
use skylive_media::MediaTime;
use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

const HOST: u64 = 10_000_000_000;
const DEADLINE: Duration = Duration::from_secs(3);
struct Shared {
    start: Instant,
    packets: Mutex<HashMap<AudioCaptureBinding, VecDeque<NativePcmBlock>>>,
    opens: Mutex<Vec<AudioCaptureBinding>>,
    closes: Mutex<Vec<AudioCaptureBinding>>,
    consumed: AtomicUsize,
    factories: AtomicUsize,
    backend_drops: AtomicUsize,
    stream_drops: AtomicUsize,
    owner: Mutex<Option<ThreadId>>,
}
struct State {
    shared: Arc<Shared>,
    owner: ThreadId,
    last_clock: Cell<u64>,
}
impl State {
    fn check(&self) {
        assert_eq!(
            thread::current().id(),
            self.owner,
            "native object changed owner"
        );
    }
    fn now(&self) -> u64 {
        HOST + u64::try_from(self.shared.start.elapsed().as_nanos()).unwrap()
    }
}
// Rc/Cell intentionally make both native objects !Send; only the factory moves.
struct Backend(Rc<State>);
impl NativeAudioBackend for Backend {
    fn capabilities(&self) -> AudioCapabilities {
        self.0.check();
        caps()
    }
    fn host_now_ns(&self) -> Result<u64, AudioCaptureError> {
        self.0.check();
        let now = self.0.now();
        self.0.last_clock.set(now);
        Ok(now)
    }
    fn enumerate(
        &mut self,
        _flow: AudioFlow,
        _limits: AudioCaptureLimits,
    ) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        self.0.check();
        Ok(Vec::new())
    }
    fn open(
        &mut self,
        binding: &AudioCaptureBinding,
        _limits: AudioCaptureLimits,
    ) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
        self.0.check();
        self.0.shared.opens.lock().unwrap().push(binding.clone());
        Ok(Box::new(Stream {
            state: self.0.clone(),
            binding: binding.clone(),
            zero: self.0.last_clock.get(),
            closed: false,
        }))
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        self.0.check();
        self.0.shared.backend_drops.fetch_add(1, Ordering::SeqCst);
    }
}
struct Stream {
    state: Rc<State>,
    binding: AudioCaptureBinding,
    zero: u64,
    closed: bool,
}
impl NativeAudioStream for Stream {
    fn poll(&mut self) -> Result<Option<NativePcmBlock>, AudioCaptureError> {
        self.state.check();
        assert!(!self.closed);
        let block = self
            .state
            .shared
            .packets
            .lock()
            .unwrap()
            .get_mut(&self.binding)
            .and_then(VecDeque::pop_front);
        Ok(block.map(|mut block| {
            block.device_timestamp_ns = Some(self.zero + block.device_timestamp_ns.unwrap());
            block.host_received_ns = self.state.now();
            self.state.shared.consumed.fetch_add(1, Ordering::SeqCst);
            block
        }))
    }
    fn close(&mut self) -> Result<(), AudioCaptureError> {
        self.state.check();
        if !self.closed {
            self.closed = true;
            self.state
                .shared
                .closes
                .lock()
                .unwrap()
                .push(self.binding.clone());
        }
        Ok(())
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.state.check();
        self.state
            .shared
            .stream_drops
            .fetch_add(1, Ordering::SeqCst);
    }
}
struct Fixture {
    shared: Arc<Shared>,
}
impl Fixture {
    fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                start: Instant::now(),
                packets: Mutex::new(HashMap::new()),
                opens: Mutex::new(Vec::new()),
                closes: Mutex::new(Vec::new()),
                consumed: AtomicUsize::new(0),
                factories: AtomicUsize::new(0),
                backend_drops: AtomicUsize::new(0),
                stream_drops: AtomicUsize::new(0),
                owner: Mutex::new(None),
            }),
        }
    }
    fn worker(&self) -> AudioCaptureWorker {
        let shared = self.shared.clone();
        AudioCaptureWorker::new(
            Box::new(move || {
                let owner = thread::current().id();
                *shared.owner.lock().unwrap() = Some(owner);
                shared.factories.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Backend(Rc::new(State {
                    shared,
                    owner,
                    last_clock: Cell::new(0),
                }))))
            }),
            caps(),
        )
        .unwrap()
    }
    fn enqueue(&self, binding: &AudioCaptureBinding, first_frame: u64, count: usize, value: f32) {
        let packets = (0..count).map(|index| {
            let first = first_frame + index as u64 * 256;
            NativePcmBlock {
                format: NativeAudioFormat {
                    sample_rate: 48000,
                    channels: 1,
                    channel_mask: 4,
                },
                frames: 256,
                samples: vec![value; 256],
                device_timestamp_ns: Some(first * 1_000_000_000 / 48000),
                graph_timestamp_ns: None,
                directshow_clock: None,
                host_received_ns: 0,
                device_frame_position: Some(first),
                flags: Default::default(),
            }
        });
        self.shared
            .packets
            .lock()
            .unwrap()
            .entry(binding.clone())
            .or_default()
            .extend(packets);
    }
    fn consumed(&self) -> usize {
        self.shared.consumed.load(Ordering::SeqCst)
    }
    fn opened(&self, binding: &AudioCaptureBinding) -> usize {
        self.shared
            .opens
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| *entry == binding)
            .count()
    }
    fn closed(&self, binding: &AudioCaptureBinding) -> usize {
        self.shared
            .closes
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| *entry == binding)
            .count()
    }
    fn finish(&self, worker: &mut AudioCaptureWorker) {
        worker.join().unwrap();
        let opens = self.shared.opens.lock().unwrap().len();
        assert_eq!(self.shared.closes.lock().unwrap().len(), opens);
        assert_eq!(self.shared.stream_drops.load(Ordering::SeqCst), opens);
        assert_eq!(self.shared.factories.load(Ordering::SeqCst), 1);
        assert_eq!(self.shared.backend_drops.load(Ordering::SeqCst), 1);
        assert_ne!(
            *self.shared.owner.lock().unwrap(),
            Some(thread::current().id())
        );
    }
}
fn caps() -> AudioCapabilities {
    AudioCapabilities {
        backend: "continuous-fixture",
        input: true,
        output: true,
        device_enumeration: false,
        exclude_own_audio: true,
        note: None,
    }
}
fn binding(name: &str) -> AudioCaptureBinding {
    AudioCaptureBinding::new(
        AudioFlow::Input,
        AudioTarget::WindowsEndpoint { id: name.into() },
        true,
        false,
    )
    .unwrap()
}
fn session() -> Session {
    let mut session = Session::new();
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(48000, SpeakerLayout::Mono)
                .unwrap()
                .with_buffering(skylive_core::AudioBuffering::new(80, true).unwrap())
                .unwrap(),
        })
        .unwrap();
    session
}
fn add(session: &mut Session, name: &str, binding: &AudioCaptureBinding) -> Id {
    let scene = session.preview();
    let item = session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: name.into(),
            binding: binding.clone(),
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
        .find(|row| row.id() == item)
        .unwrap()
        .source()
}
fn start(worker: &mut AudioCaptureWorker, session: &Session, now: MediaTime) {
    let scene = session.preview();
    worker
        .synchronize(session, &[FrameScene::Live(scene)])
        .unwrap();
    worker
        .request_program(
            session,
            &FrameComposition::Scene(FrameScene::Live(scene)),
            now,
        )
        .unwrap();
}
fn wait(mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + DEADLINE;
    while !ready() {
        assert!(
            Instant::now() < until,
            "bounded continuous fixture deadline"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
fn pause(duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        thread::sleep(Duration::from_millis(2));
    }
}
fn ack(worker: &mut AudioCaptureWorker, session: &Session) -> ProgramTransportStats {
    let mut result = None;
    wait(|| {
        if let Some(AudioCaptureEvent::Program { result: reply, .. }) =
            worker.poll(session).unwrap()
        {
            result = Some(reply.unwrap());
        }
        result.is_some()
    });
    result.unwrap()
}
fn output(worker: &mut AudioCaptureWorker, session: &Session) -> ProgramCaptureOutput {
    let mut result = None;
    wait(|| {
        result = worker.poll_program(session).unwrap();
        result.is_some()
    });
    result.unwrap()
}

#[test]
fn unread_start_ack_and_absent_meter_requests_do_not_coalesce_program_pcm() {
    let fixture = Fixture::new();
    let input = binding("Shared hardware");
    let mut session = session();
    let a = add(&mut session, "A", &input);
    let b = add(&mut session, "B", &input);
    session
        .apply(Command::SetAudio {
            source: a,
            gain: 1.0,
            muted: false,
        })
        .unwrap();
    session
        .apply(Command::SetAudio {
            source: b,
            gain: 0.5,
            muted: false,
        })
        .unwrap();
    fixture.enqueue(&input, 0, 64, 0.25);
    let mut worker = fixture.worker();
    start(&mut worker, &session, MediaTime(0));
    // Deliberately leave the control acknowledgment unread throughout native
    // draining. No request_packets/GUI tick is needed for this progress.
    wait(|| fixture.consumed() == 64);
    assert!(worker.is_busy());
    let block = output(&mut worker, &session);
    assert_eq!(block.stats.input_packets, 64);
    assert_eq!(block.stats.input_frames, 64 * 256);
    assert_eq!(block.stats.input_errors, 0);
    assert!(
        block.block.samples[512..768]
            .iter()
            .all(|sample| *sample == 0.375)
    );
    let peaks: HashMap<_, _> = block
        .block
        .source_meters
        .iter()
        .map(|meter| (meter.source, meter.channels[0].peak))
        .collect();
    assert_eq!(peaks[&a], 0.25);
    assert_eq!(peaks[&b], 0.125);
    assert_eq!(fixture.opened(&input), 1);
    drop(block);
    ack(&mut worker, &session);
    fixture.finish(&mut worker);
}

#[test]
fn full_output_queue_keeps_polling_and_recovers_with_exact_drop_gap_and_monotone_status() {
    let fixture = Fixture::new();
    let input = binding("Slow sink");
    let mut session = session();
    add(&mut session, "Audio", &input);
    fixture.enqueue(&input, 0, 64, 0.25);
    let mut worker = fixture.worker();
    start(&mut worker, &session, MediaTime(0));
    wait(|| fixture.consumed() == 64);
    pause(Duration::from_millis(450));
    fixture.enqueue(&input, 64 * 256, 16, 0.25);
    wait(|| fixture.consumed() == 80);
    ack(&mut worker, &session);
    let now = worker.program_now().unwrap().unwrap();
    worker
        .request_program(
            &session,
            &FrameComposition::Scene(FrameScene::Live(session.preview())),
            now,
        )
        .unwrap();
    let latest = ack(&mut worker, &session);
    assert_eq!(latest.input_packets, 80);
    assert!(latest.dropped_blocks > 0);
    let mut last = None;
    for _ in 0..8 {
        let item = worker
            .poll_program(&session)
            .unwrap()
            .expect("the bounded eight-entry output queue must be full");
        let status = worker.program_status().unwrap();
        assert!(status.input_packets >= latest.input_packets);
        assert!(status.output_blocks >= latest.output_blocks);
        last = Some((
            item.block.start_frame + item.block.frames as u64,
            item.stats.dropped_frames,
            item.stats.skipped_frames,
        ));
        drop(item);
    }
    let (last_end, last_dropped, last_skipped) = last.unwrap();
    let recovered = output(&mut worker, &session);
    assert_eq!(recovered.stats.input_packets, 80);
    assert_eq!(
        recovered.stats.dropped_frames,
        recovered.stats.dropped_blocks * 1024
    );
    assert_eq!(
        recovered.block.start_frame - last_end,
        (recovered.stats.dropped_frames - last_dropped)
            + (recovered.stats.skipped_frames - last_skipped)
    );
    assert!(recovered.stats.dropped_frames > last_dropped);
    assert_eq!(fixture.opened(&input), 1);
    drop(recovered);
    fixture.finish(&mut worker);
}

#[test]
fn restart_and_format_generations_filter_stale_outputs_while_old_reader_stays_worker_owned() {
    let fixture = Fixture::new();
    let input = binding("Restart");
    let mut session = session();
    add(&mut session, "Audio", &input);
    fixture.enqueue(&input, 0, 64, 0.25);
    let mut worker = fixture.worker();
    start(&mut worker, &session, MediaTime(0));
    ack(&mut worker, &session);
    wait(|| fixture.consumed() == 64);
    let old = output(&mut worker, &session);
    let old_key = old.key;
    let held = old.block.clone();
    drop(old);
    assert!(Arc::strong_count(&held) >= 2);
    pause(Duration::from_millis(100));
    worker.request_stop_program(&session).unwrap();
    ack(&mut worker, &session);
    // Stop has acknowledged and polling removes all queued stale outputs; the
    // additional strong reference is the retired worker pool, not this reader.
    assert!(worker.poll_program(&session).unwrap().is_none());
    assert!(Arc::strong_count(&held) >= 2);
    fixture.enqueue(&input, 0, 64, 0.75);
    start(&mut worker, &session, MediaTime(0));
    ack(&mut worker, &session);
    wait(|| fixture.consumed() == 128);
    let resumed = output(&mut worker, &session);
    assert!(resumed.key.generation > old_key.generation);
    assert_eq!(
        resumed.stats.input_packets, 64,
        "new generation starts its own counters"
    );
    assert!(
        resumed.block.samples[512..768]
            .iter()
            .all(|value| *value == 0.75)
    );
    let restarted_key = resumed.key;
    drop(resumed);
    session
        .apply(Command::SetAudioSettings {
            settings: AudioSettings::new(48000, SpeakerLayout::Stereo)
                .unwrap()
                .with_buffering(skylive_core::AudioBuffering::new(80, true).unwrap())
                .unwrap(),
        })
        .unwrap();
    let now = worker.program_now().unwrap().unwrap();
    start(&mut worker, &session, now);
    ack(&mut worker, &session);
    let changed = output(&mut worker, &session);
    assert!(changed.key.generation > restarted_key.generation);
    assert_eq!(changed.key.settings.channels(), 2);
    assert_eq!(held.settings.channels(), 1);
    assert_eq!(held.samples[512], 0.25);
    assert!(Arc::strong_count(&held) >= 2);
    assert_eq!(
        fixture.opened(&input),
        2,
        "format-only DSP replacement must share the existing native stream"
    );
    drop(changed);
    drop(held);
    fixture.finish(&mut worker);
}

#[test]
fn rebind_keeps_historical_and_current_native_bindings_without_source_id_conflict() {
    let fixture = Fixture::new();
    let old_input = binding("Old endpoint");
    let new_input = binding("New endpoint");
    let mut session = session();
    let source = add(&mut session, "Audio", &old_input);
    fixture.enqueue(&old_input, 0, 64, 0.25);
    fixture.enqueue(&new_input, 0, 64, 0.75);
    let mut worker = fixture.worker();
    start(&mut worker, &session, MediaTime(0));
    ack(&mut worker, &session);
    session
        .apply(Command::RebindAudioCaptureSource {
            source,
            binding: new_input.clone(),
        })
        .unwrap();
    start(&mut worker, &session, MediaTime(200_000_000));
    let route = ack(&mut worker, &session);
    assert!(route.running);
    wait(|| fixture.consumed() == 128);
    assert_eq!(fixture.opened(&old_input), 1);
    assert_eq!(fixture.opened(&new_input), 1);
    let mut saw_old = false;
    let mut saw_new = false;
    wait(|| {
        if let Some(item) = worker.poll_program(&session).unwrap() {
            assert_eq!(item.stats.input_errors, 0);
            let middle = item.block.start_frame + 512;
            if middle < 8000 {
                assert_eq!(item.block.samples[512], 0.25);
                saw_old = true;
            }
            if (11000..14000).contains(&middle) {
                assert_eq!(item.block.samples[512], 0.75);
                saw_new = true;
            }
            drop(item);
        }
        saw_old && saw_new
    });
    wait(|| fixture.closed(&old_input) == 1);
    assert_eq!(fixture.opened(&new_input), 1);
    fixture.finish(&mut worker);
}

#[test]
fn retired_output_pool_count_is_global_and_recovers_after_last_readers_release() {
    let session = session();
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let calls = factory_calls.clone();
    let mut worker = AudioCaptureWorker::new(
        Box::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            panic!("an empty Program route must not construct any native backend")
        }),
        caps(),
    )
    .unwrap();
    let reply = |worker: &mut AudioCaptureWorker| {
        let mut result = None;
        wait(|| {
            if let Some(AudioCaptureEvent::Program {
                result: received, ..
            }) = worker.poll(&session).unwrap()
            {
                result = Some(received);
            }
            result.is_some()
        });
        result.unwrap()
    };
    let mut readers = Vec::new();
    let mut last_generation = 0;
    for _ in 0..5 {
        start(&mut worker, &session, MediaTime(0));
        assert!(reply(&mut worker).unwrap().running);
        let produced = output(&mut worker, &session);
        assert!(produced.key.generation > last_generation);
        last_generation = produced.key.generation;
        assert_eq!(produced.stats.input_packets, 0);
        readers.push(produced.block.clone());
        drop(produced);
        worker.request_stop_program(&session).unwrap();
        assert!(!reply(&mut worker).unwrap().running);
        assert!(worker.program_status().is_none());
        assert_eq!(worker.program_now().unwrap(), None);
        assert!(worker.poll_program(&session).unwrap().is_none());
        assert!(
            readers.iter().all(|reader| Arc::strong_count(reader) >= 2),
            "the owner must retain stopped pools while external readers exist"
        );
    }
    assert_eq!(readers.len(), 5);
    assert!(
        readers
            .iter()
            .map(|reader| reader.samples.len() * 4)
            .sum::<usize>()
            < 1024 * 1024,
        "the rejection below must exercise pool count, not byte exhaustion"
    );
    start(&mut worker, &session, MediaTime(0));
    assert!(matches!(
        reply(&mut worker),
        Err(AudioCaptureError::Backpressure(_))
    ));
    assert_eq!(
        worker.program_now().unwrap(),
        None,
        "a rejected creation must not leave a phantom media clock for the retry"
    );
    assert!(worker.program_status().is_none());
    assert_eq!(factory_calls.load(Ordering::SeqCst), 0);
    drop(readers);
    let deadline = Instant::now() + DEADLINE;
    loop {
        start(&mut worker, &session, MediaTime(0));
        match reply(&mut worker) {
            Ok(stats) => {
                assert!(stats.running);
                break;
            }
            Err(AudioCaptureError::Backpressure(_)) => {
                assert!(
                    Instant::now() < deadline,
                    "released reader pools must eventually retire"
                );
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("unexpected recovery result: {error}"),
        }
    }
    let recovered = output(&mut worker, &session);
    assert!(recovered.key.generation > last_generation);
    assert_eq!(recovered.stats.input_packets, 0);
    drop(recovered);
    worker.join().unwrap();
    assert_eq!(factory_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn buffering_only_change_replaces_program_policy_without_reopening_native_stream() {
    let fixture = Fixture::new();
    let input = binding("Buffering policy");
    let mut session = session();
    let source = add(&mut session, "Audio", &input);
    let original_source = session.snapshot().source(source).unwrap().kind().clone();
    let original_settings = session.snapshot().audio();
    let fixed = skylive_core::AudioBuffering::new(80, true).unwrap();
    assert_eq!(original_settings.buffering(), fixed);
    fixture.enqueue(&input, 0, 64, 0.25);
    let mut worker = fixture.worker();
    start(&mut worker, &session, MediaTime(0));
    ack(&mut worker, &session);
    wait(|| fixture.consumed() == 64);
    let old = output(&mut worker, &session);
    assert_eq!(old.stats.max_buffer_blocks, 4);
    let old_key = old.key;
    let held = old.block.clone();
    drop(old);
    assert!(Arc::strong_count(&held) >= 2);
    // Leave old-policy publications in the FIFO as well as retaining a reader.
    pause(Duration::from_millis(50));
    assert!(worker.program_status_for(&session).is_some());

    let adaptive = skylive_core::AudioBuffering::default();
    let changed_settings = original_settings.with_buffering(adaptive).unwrap();
    session
        .apply(Command::SetAudioSettings {
            settings: changed_settings,
        })
        .unwrap();
    assert_eq!(changed_settings.sample_rate(), 48000);
    assert_eq!(changed_settings.speakers(), SpeakerLayout::Mono);
    assert_eq!(
        session.snapshot().source(source).unwrap().kind(),
        &original_source,
        "the policy edit keeps the same source and native binding"
    );
    assert!(
        worker.program_status_for(&session).is_none(),
        "old-policy status must disappear before the replacement is admitted"
    );
    let now = worker.program_now().unwrap().unwrap();
    start(&mut worker, &session, now);
    let admitted = ack(&mut worker, &session);
    assert_eq!(admitted.max_buffer_blocks, 45);
    assert_eq!(admitted.buffer_blocks, 0);
    assert!(worker.program_status_for(&session).is_some());

    // Keep the existing device's timestamps ahead of the new media cursor. A
    // missing/early source must not accidentally exercise adaptive lag growth.
    let future_frame = u64::try_from(u128::from(now.0) * 48000 / 1_000_000_000).unwrap() + 48000;
    fixture.enqueue(&input, future_frame, 64, 0.75);
    wait(|| fixture.consumed() == 128);
    let mut changed = None;
    wait(|| {
        if let Some(item) = worker.poll_program(&session).unwrap() {
            assert!(item.key.generation > old_key.generation);
            assert_eq!(item.key.settings, changed_settings);
            if item.stats.input_packets == 64 {
                changed = Some(item);
            }
        }
        changed.is_some()
    });
    let changed = changed.unwrap();
    assert_eq!(changed.key.clock_epoch, old_key.clock_epoch);
    assert_eq!(changed.key.settings.buffering(), adaptive);
    assert_eq!(changed.stats.max_buffer_blocks, 45);
    assert_eq!(changed.stats.buffer_blocks, 0);
    assert_eq!(held.settings.buffering(), fixed);
    assert!(Arc::strong_count(&held) >= 2);
    assert!(worker.program_status_for(&session).is_some());
    assert_eq!(fixture.opened(&input), 1);
    assert_eq!(fixture.shared.factories.load(Ordering::SeqCst), 1);
    drop(changed);
    drop(held);
    fixture.finish(&mut worker);
}

#[test]
fn selecting_a_different_profile_replaces_program_format_and_preserves_global_buffering() {
    use skylive_core::ProfileCommand;

    let fixture = Fixture::new();
    let input = binding("Profile switch");
    let mut session = session();
    let source = add(&mut session, "Audio", &input);
    let original_source = session.snapshot().source(source).unwrap().kind().clone();
    let original_profile = session.snapshot().profiles().selected_id();
    let original_settings = session.snapshot().audio();
    let buffering = original_settings.buffering();
    session
        .apply(Command::Profile {
            command: ProfileCommand::Duplicate {
                name: "44.1 kHz Stereo".into(),
            },
        })
        .unwrap();
    let other_profile = session.snapshot().profiles().selected_id();
    let other_settings = AudioSettings::new(44100, SpeakerLayout::Stereo)
        .unwrap()
        .with_buffering(buffering)
        .unwrap();
    session
        .apply(Command::SetAudioSettings {
            settings: other_settings,
        })
        .unwrap();
    session
        .apply(Command::Profile {
            command: ProfileCommand::Select {
                id: original_profile,
            },
        })
        .unwrap();
    assert_eq!(session.snapshot().audio(), original_settings);
    assert_eq!(
        session.snapshot().preferences().audio_buffering(),
        buffering
    );

    fixture.enqueue(&input, 0, 64, 0.25);
    let mut worker = fixture.worker();
    start(&mut worker, &session, MediaTime(0));
    ack(&mut worker, &session);
    wait(|| fixture.consumed() == 64);
    let old = output(&mut worker, &session);
    let old_key = old.key;
    let held = old.block.clone();
    drop(old);
    pause(Duration::from_millis(50));
    assert!(worker.program_status_for(&session).is_some());
    session
        .apply(Command::Profile {
            command: ProfileCommand::Select { id: other_profile },
        })
        .unwrap();
    assert_eq!(session.snapshot().profiles().selected_id(), other_profile);
    assert_eq!(session.snapshot().audio(), other_settings);
    assert_eq!(
        session.snapshot().preferences().audio_buffering(),
        buffering
    );
    assert_eq!(
        session.snapshot().source(source).unwrap().kind(),
        &original_source
    );
    assert!(worker.program_status_for(&session).is_none());
    assert!(
        worker.poll_program(&session).unwrap().is_none(),
        "queued PCM from the previous selected profile must be rejected before new admission"
    );

    let now = worker.program_now().unwrap().unwrap();
    start(&mut worker, &session, now);
    let admitted = ack(&mut worker, &session);
    assert!(admitted.running);
    assert!(worker.program_status_for(&session).is_some());
    // The device stays 48 kHz mono. Only the Program conversion/output grid
    // changes, and future input avoids making this test a lag-recovery oracle.
    let future_frame = u64::try_from(u128::from(now.0) * 48000 / 1_000_000_000).unwrap() + 48000;
    fixture.enqueue(&input, future_frame, 64, 0.75);
    wait(|| fixture.consumed() == 128);
    let mut changed = None;
    wait(|| {
        if let Some(item) = worker.poll_program(&session).unwrap() {
            assert!(item.key.generation > old_key.generation);
            assert_eq!(item.key.settings, other_settings);
            if item.stats.input_packets == 64 {
                changed = Some(item);
            }
        }
        changed.is_some()
    });
    let changed = changed.unwrap();
    assert_eq!(changed.key.clock_epoch, old_key.clock_epoch);
    assert_eq!(changed.key.settings.sample_rate(), 44100);
    assert_eq!(changed.key.settings.channels(), 2);
    assert_eq!(changed.key.settings.buffering(), buffering);
    assert_eq!(changed.block.settings, other_settings);
    assert_eq!(changed.block.samples.len(), 2048);
    assert_eq!(changed.stats.input_errors, 0);
    assert_eq!(held.settings, original_settings);
    assert_eq!(held.samples.len(), 1024);
    assert!(
        Arc::strong_count(&held) >= 2,
        "the retired worker pool owns old PCM until the reader releases it"
    );
    assert_eq!(fixture.opened(&input), 1);
    assert_eq!(fixture.shared.factories.load(Ordering::SeqCst), 1);
    drop(changed);
    drop(held);
    fixture.finish(&mut worker);
}
