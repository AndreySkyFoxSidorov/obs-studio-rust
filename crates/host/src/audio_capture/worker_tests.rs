//! Controlled owner-boundary tests. No native factory, device or audio output.
use super::*;
use crate::transition::{Controller, FrameScene};
use skylive_core::{AudioSlot, Command, Id, Session};
use skylive_media::MediaTime;
use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
};

const HOST: u64 = 10_000_000_000;
#[test]
fn conflicting_retired_source_bindings_reject_before_owner_publication_or_native_access() {
    let mut session = Session::new();
    let scene = session.preview();
    let (source, item) = add(&mut session, scene, "Retained mic", binding("old", false));
    let mut controller = Controller::new();
    controller
        .set_studio(&mut session, true, MediaTime(0))
        .unwrap();
    let old = controller
        .sync(&mut session, MediaTime(0))
        .unwrap()
        .output
        .scenes()
        .remove(0);
    controller.before_change(&session).unwrap();
    session
        .apply(Command::RebindAudioCaptureSource {
            source,
            binding: binding("new", false),
        })
        .unwrap();
    let new = controller
        .sync(&mut session, MediaTime(0))
        .unwrap()
        .output
        .scenes()
        .remove(0);
    controller.before_change(&session).unwrap();
    session.apply(Command::RemoveItem { scene, item }).unwrap();
    assert!(session.snapshot().source(source).is_none());
    let mut worker = AudioCaptureWorker::new(
        Box::new(|| panic!("metadata validation must not instantiate a native backend")),
        caps(),
    )
    .unwrap();
    worker
        .synchronize(&session, std::slice::from_ref(&old))
        .unwrap();
    let before = worker.snapshot();
    assert!(matches!(
        worker.synchronize(&session, &[old, new]),
        Err(AudioCaptureError::Invalid(_))
    ));
    assert_eq!(worker.snapshot().epoch(), before.epoch());
    assert_eq!(worker.active_source_count(), 1);
    drop(before);
    worker.join().unwrap();
}
enum Call {
    Factory(ThreadId),
    Clock(SyncSender<Result<u64, AudioCaptureError>>),
    Devices {
        flow: AudioFlow,
        limits: AudioCaptureLimits,
        reply: SyncSender<Result<Vec<AudioDeviceInfo>, AudioCaptureError>>,
    },
    Open {
        id: u64,
        binding: AudioCaptureBinding,
        limits: AudioCaptureLimits,
        reply: SyncSender<Result<(), AudioCaptureError>>,
    },
    Poll {
        id: u64,
        reply: SyncSender<Result<Option<NativePcmBlock>, AudioCaptureError>>,
    },
    Close {
        id: u64,
        reply: SyncSender<Result<(), AudioCaptureError>>,
    },
    StreamDrop {
        id: u64,
        owner: ThreadId,
    },
    BackendDrop(ThreadId),
}
struct State {
    calls: SyncSender<Call>,
    owner: ThreadId,
    next_stream: Cell<u64>,
}
impl State {
    fn check(&self) {
        assert_eq!(
            self.owner,
            thread::current().id(),
            "native object crossed owners"
        );
    }
    fn request<T>(
        &self,
        make: impl FnOnce(SyncSender<Result<T, AudioCaptureError>>) -> Call,
    ) -> Result<T, AudioCaptureError> {
        self.check();
        let (reply, response) = mpsc::sync_channel(1);
        self.calls
            .send(make(reply))
            .map_err(|_| AudioCaptureError::Disconnected)?;
        response
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| {
                Err(AudioCaptureError::Native {
                    operation: "fake",
                    detail: "response deadline".into(),
                })
            })
    }
}
// Rc deliberately makes both backend and stream !Send. Only the factory is
// Send; it constructs these objects after entering the production worker.
struct Backend(Rc<State>);
impl Drop for Backend {
    fn drop(&mut self) {
        self.0.check();
        let _ = self.0.calls.send(Call::BackendDrop(thread::current().id()));
    }
}
impl NativeAudioBackend for Backend {
    fn capabilities(&self) -> AudioCapabilities {
        caps()
    }
    fn host_now_ns(&self) -> Result<u64, AudioCaptureError> {
        self.0.request(Call::Clock)
    }
    fn enumerate(
        &mut self,
        flow: AudioFlow,
        limits: AudioCaptureLimits,
    ) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        self.0.request(|reply| Call::Devices {
            flow,
            limits,
            reply,
        })
    }
    fn open(
        &mut self,
        binding: &AudioCaptureBinding,
        limits: AudioCaptureLimits,
    ) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
        let id = self.0.next_stream.get() + 1;
        self.0.next_stream.set(id);
        self.0.request(|reply| Call::Open {
            id,
            binding: binding.clone(),
            limits,
            reply,
        })?;
        Ok(Box::new(Stream {
            state: self.0.clone(),
            id,
            closed: false,
        }))
    }
}
struct Stream {
    state: Rc<State>,
    id: u64,
    closed: bool,
}
impl NativeAudioStream for Stream {
    fn poll(&mut self) -> Result<Option<NativePcmBlock>, AudioCaptureError> {
        assert!(!self.closed, "closed stream was polled");
        self.state
            .request(|reply| Call::Poll { id: self.id, reply })
    }
    fn close(&mut self) -> Result<(), AudioCaptureError> {
        self.state.check();
        if self.closed {
            return Ok(());
        }
        let (reply, response) = mpsc::sync_channel(1);
        if self
            .state
            .calls
            .send(Call::Close { id: self.id, reply })
            .is_err()
        {
            self.closed = true;
            return Ok(());
        }
        // Test failures must not orphan a worker permanently. Successful tests
        // acknowledge every close explicitly, including failed-stop retries.
        match response.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => {
                if result.is_ok() {
                    self.closed = true;
                }
                result
            }
            Err(_) => {
                self.closed = true;
                Ok(())
            }
        }
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.state.check();
        let _ = self.state.calls.send(Call::StreamDrop {
            id: self.id,
            owner: thread::current().id(),
        });
    }
}
fn caps() -> AudioCapabilities {
    AudioCapabilities {
        backend: "controlled-audio",
        input: true,
        output: true,
        device_enumeration: true,
        exclude_own_audio: true,
        note: None,
    }
}
struct Harness {
    calls: Receiver<Call>,
    factories: Arc<AtomicUsize>,
    caller: ThreadId,
}
impl Harness {
    fn new(limits: AudioCaptureLimits) -> (AudioCaptureWorker, Self) {
        let (sender, calls) = mpsc::sync_channel(64);
        let factories = Arc::new(AtomicUsize::new(0));
        let count = factories.clone();
        let factory: AudioBackendFactory = Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            let owner = thread::current().id();
            sender
                .send(Call::Factory(owner))
                .map_err(|_| AudioCaptureError::Disconnected)?;
            Ok(Box::new(Backend(Rc::new(State {
                calls: sender,
                owner,
                next_stream: Cell::new(0),
            }))))
        });
        (
            AudioCaptureWorker::with_limits(factory, caps(), limits).unwrap(),
            Self {
                calls,
                factories,
                caller: thread::current().id(),
            },
        )
    }
    fn next(&self) -> Call {
        self.calls
            .recv_timeout(Duration::from_secs(2))
            .expect("audio fake call deadline")
    }
    fn action(&self) -> Call {
        loop {
            match self.next() {
                Call::Factory(owner) => assert_ne!(owner, self.caller),
                Call::StreamDrop { id, owner } => {
                    assert!(id > 0);
                    assert_ne!(owner, self.caller);
                }
                call => return call,
            }
        }
    }
    fn idle(&self) {
        assert!(matches!(
            self.calls.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
    fn open(&self, expected: &AudioCaptureBinding) -> u64 {
        match self.action() {
            Call::Clock(reply) => reply.send(Ok(HOST)).unwrap(),
            _ => panic!("expected host clock"),
        }
        match self.action() {
            Call::Open {
                id,
                binding,
                limits,
                reply,
            } => {
                assert_eq!(&binding, expected);
                assert!(limits.max_frames <= 4096);
                reply.send(Ok(())).unwrap();
                id
            }
            _ => panic!("expected native open"),
        }
    }
    fn poll(&self, stream: u64, result: Result<Option<NativePcmBlock>, AudioCaptureError>) {
        match self.action() {
            Call::Poll { id, reply } => {
                assert_eq!(id, stream);
                reply.send(result).unwrap();
            }
            _ => panic!("expected native poll"),
        }
    }
    fn packet(&self, stream: u64, block: NativePcmBlock) {
        self.poll(stream, Ok(Some(block)));
        self.poll(stream, Ok(None));
    }
    fn close(&self, stream: u64, result: Result<(), AudioCaptureError>) {
        match self.action() {
            Call::Close { id, reply } => {
                assert_eq!(id, stream);
                reply.send(result).unwrap();
            }
            _ => panic!("expected stop acknowledgment"),
        }
    }
    fn devices(&self) -> SyncSender<Result<Vec<AudioDeviceInfo>, AudioCaptureError>> {
        match self.action() {
            Call::Devices {
                flow,
                limits,
                reply,
            } => {
                assert_eq!(flow, AudioFlow::Input);
                assert!(limits.max_devices <= 64);
                reply
            }
            _ => panic!("expected enumeration"),
        }
    }
    fn finish(self, worker: &mut AudioCaptureWorker) {
        worker.close();
        if self.factories.load(Ordering::SeqCst) != 0 {
            loop {
                match self.next() {
                    Call::Factory(owner) => assert_ne!(owner, self.caller),
                    Call::BackendDrop(owner) => {
                        assert_ne!(owner, self.caller);
                        break;
                    }
                    Call::StreamDrop { owner, .. } => assert_ne!(owner, self.caller),
                    Call::Close { reply, .. } => {
                        let _ = reply.send(Ok(()));
                    }
                    Call::Clock(reply) => {
                        let _ = reply.send(Err(AudioCaptureError::Closed));
                    }
                    Call::Devices { reply, .. } => {
                        let _ = reply.send(Err(AudioCaptureError::Closed));
                    }
                    Call::Open { reply, .. } => {
                        let _ = reply.send(Err(AudioCaptureError::Closed));
                    }
                    Call::Poll { reply, .. } => {
                        let _ = reply.send(Err(AudioCaptureError::Closed));
                    }
                }
            }
        }
        worker.join().unwrap();
    }
}
fn settle(worker: &mut AudioCaptureWorker, session: &Session) -> Option<AudioCaptureEvent> {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        let event = worker.poll(session).unwrap();
        if !worker.is_busy() {
            return event;
        }
        assert!(Instant::now() < until, "audio completion deadline");
        thread::sleep(Duration::from_millis(1));
    }
}
fn binding(id: &str, timing: bool) -> AudioCaptureBinding {
    AudioCaptureBinding::new(
        AudioFlow::Input,
        AudioTarget::WindowsEndpoint { id: id.into() },
        timing,
        false,
    )
    .unwrap()
}
fn add(session: &mut Session, scene: Id, name: &str, binding: AudioCaptureBinding) -> (Id, Id) {
    let item = session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: name.into(),
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
        .find(|i| i.id() == item)
        .unwrap()
        .source();
    (source, item)
}
fn scene(session: &mut Session, name: &str) -> Id {
    session
        .apply(Command::AddScene { name: name.into() })
        .unwrap()
        .created
        .unwrap()
}
fn packet(value: f32) -> NativePcmBlock {
    NativePcmBlock {
        format: NativeAudioFormat {
            sample_rate: 8000,
            channels: 1,
            channel_mask: 4,
        },
        frames: 2,
        samples: vec![value; 2],
        device_timestamp_ns: Some(HOST),
        graph_timestamp_ns: None,
        directshow_clock: None,
        host_received_ns: HOST,
        device_frame_position: Some(0),
        flags: NativePacketFlags::default(),
    }
}
fn device(id: &str) -> AudioDeviceInfo {
    AudioDeviceInfo {
        target: AudioTarget::WindowsEndpoint { id: id.into() },
        name: format!("Mic {id}"),
        flow: AudioFlow::Input,
        is_default: false,
    }
}

#[test]
fn factory_is_lazy_owner_local_and_unread_completion_keeps_the_job_busy() {
    let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let bind = binding("A", true);
    add(&mut session, live, "A", bind.clone());
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    assert_eq!(worker.capabilities().backend, "controlled-audio");
    assert_eq!(harness.factories.load(Ordering::SeqCst), 0);
    harness.idle();
    worker.request_packets(&session, MediaTime(0)).unwrap();
    let stream = harness.open(&bind);
    harness.packet(stream, packet(0.25));
    assert!(worker.is_busy());
    assert_eq!(
        worker.request_packets(&session, MediaTime(1)),
        Err(AudioCaptureError::Busy)
    );
    assert_eq!(
        worker.request_devices(&session, AudioFlow::Input),
        Err(AudioCaptureError::Busy)
    );
    assert!(matches!(
        settle(&mut worker, &session),
        Some(AudioCaptureEvent::Packets { received: 1, .. })
    ));
    assert_eq!(harness.factories.load(Ordering::SeqCst), 1);
    harness.finish(&mut worker);
}

#[test]
fn shared_binding_acquires_once_but_distinct_source_ids_and_gain_state_remain_independent() {
    let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let bind = binding("A", true);
    let (a, _) = add(&mut session, live, "A", bind.clone());
    let (b, _) = add(&mut session, live, "Other source", bind.clone());
    session
        .apply(Command::AddExistingSource {
            scene: live,
            source: a,
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    assert_eq!(worker.active_source_count(), 2);
    worker.request_packets(&session, MediaTime(0)).unwrap();
    let stream = harness.open(&bind);
    harness.packet(stream, packet(0.5));
    settle(&mut worker, &session);
    let before = worker.snapshot();
    assert_eq!(before.packet_count(), 2);
    assert!(std::ptr::eq(
        before.packet(a).unwrap(),
        before.packet(b).unwrap()
    ));
    assert_eq!(before.packet(a).unwrap().meters[0].peak, 0.5);
    session
        .apply(Command::SetAudio {
            source: a,
            gain: 0.25,
            muted: true,
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    let after = worker.snapshot();
    assert_eq!(before.epoch(), after.epoch());
    assert!(std::ptr::eq(
        before.packet(a).unwrap(),
        after.packet(a).unwrap()
    ));
    assert_eq!(session.snapshot().source(b).unwrap().gain(), 1.0);
    assert!(!session.snapshot().source(b).unwrap().muted());
    drop(before);
    drop(after);
    harness.idle();
    harness.finish(&mut worker);
}

#[test]
fn preview_and_global_acquisition_union_is_explicit_and_empty_request_stops_inactive_streams() {
    let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
    let mut session = Session::new();
    let a = session.preview();
    let b = scene(&mut session, "Preview");
    let binding_a = binding("A", true);
    let binding_b = binding("B", true);
    let binding_g = binding("G", true);
    add(&mut session, a, "A", binding_a.clone());
    add(&mut session, b, "B", binding_b.clone());
    session
        .apply(Command::AddGlobalAudioCaptureSource {
            slot: AudioSlot::Mic1,
            name: "Global".into(),
            binding: binding_g.clone(),
        })
        .unwrap();
    session.select_preview(b).unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(a)])
        .unwrap();
    assert_eq!(worker.active_source_count(), 2);
    worker
        .synchronize(&session, &[FrameScene::Live(a), FrameScene::Live(b)])
        .unwrap();
    assert_eq!(worker.active_source_count(), 3);
    worker.request_packets(&session, MediaTime(0)).unwrap();
    let mut streams = Vec::new();
    for bind in [&binding_a, &binding_b, &binding_g] {
        let id = harness.open(bind);
        harness.packet(id, packet(0.1));
        streams.push(id);
    }
    settle(&mut worker, &session);
    assert_eq!(worker.snapshot().packet_count(), 3);
    session
        .apply(Command::SetGlobalAudio {
            slot: AudioSlot::Mic1,
            source: None,
        })
        .unwrap();
    worker.synchronize(&session, &[]).unwrap();
    assert_eq!(worker.active_source_count(), 0);
    worker.request_packets(&session, MediaTime(1)).unwrap();
    for id in streams {
        harness.close(id, Ok(()));
    }
    settle(&mut worker, &session);
    assert_eq!(worker.snapshot().packet_count(), 0);
    harness.finish(&mut worker);
}

#[test]
fn rebind_rejects_old_epoch_reply_and_keeps_unrelated_shared_packet_visible() {
    let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let bind_a = binding("A", true);
    let bind_b = binding("B", true);
    let bind_c = binding("C", true);
    let (a, _) = add(&mut session, live, "A", bind_a.clone());
    let (b, _) = add(&mut session, live, "B", bind_b.clone());
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_packets(&session, MediaTime(0)).unwrap();
    let stream_a = harness.open(&bind_a);
    harness.packet(stream_a, packet(0.1));
    let stream_b = harness.open(&bind_b);
    harness.packet(stream_b, packet(0.2));
    settle(&mut worker, &session);
    let before = worker.snapshot();
    worker.request_packets(&session, MediaTime(1)).unwrap();
    let held = match harness.action() {
        Call::Poll { id, reply } => {
            assert_eq!(id, stream_a);
            reply
        }
        _ => panic!(),
    };
    session
        .apply(Command::RebindAudioCaptureSource {
            source: a,
            binding: bind_c.clone(),
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    let changed = worker.snapshot();
    assert!(changed.packet(a).is_none());
    assert!(std::ptr::eq(
        before.packet(b).unwrap(),
        changed.packet(b).unwrap()
    ));
    assert_ne!(before.epoch(), changed.epoch());
    assert_eq!(
        worker.request_packets(&session, MediaTime(2)),
        Err(AudioCaptureError::Busy)
    );
    held.send(Ok(Some(packet(0.9)))).unwrap();
    harness.poll(stream_a, Ok(None));
    harness.packet(stream_b, packet(0.8));
    assert!(settle(&mut worker, &session).is_none());
    assert!(std::ptr::eq(
        changed.packet(b).unwrap(),
        worker.snapshot().packet(b).unwrap()
    ));
    worker.request_packets(&session, MediaTime(2)).unwrap();
    harness.close(stream_a, Ok(()));
    let stream_c = harness.open(&bind_c);
    harness.packet(stream_c, packet(0.3));
    harness.packet(stream_b, packet(0.4));
    settle(&mut worker, &session);
    assert_eq!(worker.snapshot().packet(a).unwrap().binding, bind_c);
    drop(before);
    drop(changed);
    harness.finish(&mut worker);
}

#[test]
fn device_and_receive_time_mapping_keep_signed_preroll_and_recorded_origin() {
    for timing in [true, false] {
        let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
        let mut session = Session::new();
        let live = session.preview();
        let bind = binding("A", timing);
        let (source, _) = add(&mut session, live, "A", bind.clone());
        worker
            .synchronize(&session, &[FrameScene::Live(live)])
            .unwrap();
        let began = Instant::now();
        worker.request_packets(&session, MediaTime(0)).unwrap();
        let stream = harness.open(&bind);
        let mut block = packet(0.5);
        block.device_timestamp_ns = Some(1_000_000_000);
        block.host_received_ns = 1_000_000_000;
        harness.packet(stream, block);
        settle(&mut worker, &session);
        let snap = worker.snapshot();
        let captured = snap.packet(source).unwrap();
        let expected = if timing {
            -9_000_000_000i128
        } else {
            -9_000_250_000i128
        };
        assert!(captured.first_sample_media_ns >= expected);
        assert!(captured.first_sample_media_ns <= expected + began.elapsed().as_nanos() as i128);
        assert!(captured.first_sample_media_ns < 0);
        assert_eq!(
            captured.timing_origin,
            if timing {
                AudioTimingOrigin::Device
            } else {
                AudioTimingOrigin::ReceiveEstimate
            }
        );
        assert!(captured.block.flags.discontinuity);
        let json = serde_json::to_value(captured).unwrap();
        assert!(
            json["first_sample_media_ns"]
                .as_str()
                .unwrap()
                .starts_with('-')
        );
        drop(snap);
        harness.finish(&mut worker);
    }
}

#[test]
fn latest_packet_coalescing_and_bad_device_timestamp_fallback_are_explicit() {
    let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let bind = binding("A", true);
    let (source, _) = add(&mut session, live, "A", bind.clone());
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_packets(&session, MediaTime(0)).unwrap();
    let stream = harness.open(&bind);
    harness.poll(stream, Ok(Some(packet(0.1))));
    let mut latest = packet(0.75);
    latest.flags.timestamp_error = true;
    latest.flags.queue_dropped = 7;
    harness.packet(stream, latest);
    settle(&mut worker, &session);
    let snap = worker.snapshot();
    let captured = snap.packet(source).unwrap();
    assert_eq!(captured.coalesced_packets, 1);
    assert_eq!(captured.block.flags.queue_dropped, 7);
    assert_eq!(captured.block.samples, [0.75; 2]);
    assert_eq!(captured.timing_origin, AudioTimingOrigin::ReceiveEstimate);
    assert_eq!(captured.meters[0].rms, 0.75);
    drop(snap);
    harness.finish(&mut worker);
}

#[test]
fn source_and_sixteen_stream_limits_reject_before_factory_admission() {
    let (mut worker, harness) = Harness::new(AudioCaptureLimits {
        max_sources: 1,
        ..AudioCaptureLimits::default()
    });
    let mut session = Session::new();
    let live = session.preview();
    let bind = binding("A", true);
    let (a, _) = add(&mut session, live, "A", bind.clone());
    session
        .apply(Command::AddExistingSource {
            scene: live,
            source: a,
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    assert_eq!(worker.active_source_count(), 1);
    add(&mut session, live, "Other SourceID", bind);
    assert!(matches!(
        worker.synchronize(&session, &[FrameScene::Live(live)]),
        Err(AudioCaptureError::Limit(_))
    ));
    assert_eq!(worker.active_source_count(), 1);
    harness.idle();
    harness.finish(&mut worker);
    let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    for n in 0..17 {
        add(
            &mut session,
            live,
            &format!("Mic {n}"),
            binding(&format!("Endpoint {n}"), true),
        );
    }
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    assert!(matches!(
        worker.request_packets(&session, MediaTime(0)),
        Err(AudioCaptureError::Limit(_))
    ));
    assert_eq!(worker.active_source_count(), 17);
    assert_eq!(harness.factories.load(Ordering::SeqCst), 0);
    harness.idle();
    harness.finish(&mut worker);
}

#[test]
fn stale_enumeration_revision_and_wrong_metadata_cannot_publish_devices() {
    let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
    let mut session = Session::new();
    worker.synchronize(&session, &[]).unwrap();
    worker.request_devices(&session, AudioFlow::Input).unwrap();
    let held = harness.devices();
    session
        .apply(Command::RenameCollection {
            name: "New revision".into(),
        })
        .unwrap();
    held.send(Ok(vec![device("A")])).unwrap();
    assert!(settle(&mut worker, &session).is_none());
    let mut bad = device("A");
    bad.name = "bad\nname".into();
    for devices in [vec![device("A"), device("A")], vec![bad]] {
        worker.request_devices(&session, AudioFlow::Input).unwrap();
        harness.devices().send(Ok(devices)).unwrap();
        match settle(&mut worker, &session).unwrap() {
            AudioCaptureEvent::Devices { result, .. } => {
                assert!(matches!(result, Err(AudioCaptureError::Invalid(_))))
            }
            _ => panic!(),
        }
    }
    harness.finish(&mut worker);
}

#[test]
fn invalid_native_pcm_finite_shape_and_capacity_are_rejected_without_partial_packet() {
    let mut invalid = Vec::new();
    let mut nan = packet(0.0);
    nan.samples[0] = f32::NAN;
    invalid.push(nan);
    let mut shape = packet(0.0);
    shape.frames = 3;
    invalid.push(shape);
    let mut too_many_frames = packet(0.0);
    too_many_frames.frames = 4097;
    invalid.push(too_many_frames);
    let mut false_silence = packet(0.5);
    false_silence.flags.silent = true;
    invalid.push(false_silence);
    let mut overcapacity = packet(0.0);
    overcapacity.samples = Vec::with_capacity(33_000);
    overcapacity.samples.extend_from_slice(&[0.0; 2]);
    invalid.push(overcapacity);
    for block in invalid {
        let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
        let mut session = Session::new();
        let live = session.preview();
        let bind = binding("A", true);
        let (source, _) = add(&mut session, live, "A", bind.clone());
        worker
            .synchronize(&session, &[FrameScene::Live(live)])
            .unwrap();
        worker.request_packets(&session, MediaTime(0)).unwrap();
        let stream = harness.open(&bind);
        harness.poll(stream, Ok(Some(block)));
        harness.close(stream, Ok(()));
        settle(&mut worker, &session);
        assert!(worker.snapshot().packet(source).is_none());
        assert_eq!(worker.snapshot().problems().len(), 1);
        harness.finish(&mut worker);
    }
}

#[test]
fn poll_failure_stops_once_backs_off_then_reopens_with_new_stream_generation() {
    let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let bind = binding("A", true);
    let (source, _) = add(&mut session, live, "A", bind.clone());
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_packets(&session, MediaTime(0)).unwrap();
    let stream = harness.open(&bind);
    harness.packet(stream, packet(0.25));
    settle(&mut worker, &session);
    let before = worker.snapshot();
    let generation = before.packet(source).unwrap().stream_generation;
    worker.request_packets(&session, MediaTime(1)).unwrap();
    harness.poll(
        stream,
        Err(AudioCaptureError::DeviceUnavailable("changed".into())),
    );
    harness.close(stream, Ok(()));
    settle(&mut worker, &session);
    assert!(std::ptr::eq(
        before.packet(source).unwrap(),
        worker.snapshot().packet(source).unwrap()
    ));
    worker.request_packets(&session, MediaTime(2)).unwrap();
    settle(&mut worker, &session);
    // action() consumes only the completed StreamDrop; no new clock/open/poll.
    match harness.next() {
        Call::StreamDrop { id, owner } => {
            assert_eq!(id, stream);
            assert_ne!(owner, harness.caller);
        }
        _ => panic!(),
    }
    harness.idle();
    thread::sleep(Duration::from_millis(1100));
    worker.request_packets(&session, MediaTime(3)).unwrap();
    let reopened = harness.open(&bind);
    assert_ne!(reopened, stream);
    harness.packet(reopened, packet(0.5));
    settle(&mut worker, &session);
    let after = worker.snapshot();
    assert_ne!(after.packet(source).unwrap().stream_generation, generation);
    assert!(after.packet(source).unwrap().block.flags.discontinuity);
    drop(before);
    drop(after);
    harness.finish(&mut worker);
}

#[test]
fn failed_stop_remains_admitted_until_ack_and_cannot_be_replaced_by_a_new_stream() {
    let limits = AudioCaptureLimits {
        max_streams: 1,
        ..AudioCaptureLimits::default()
    };
    let (mut worker, harness) = Harness::new(limits);
    let mut session = Session::new();
    let live = session.preview();
    let a = binding("A", true);
    let b = binding("B", true);
    let (source, _) = add(&mut session, live, "A", a.clone());
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_packets(&session, MediaTime(0)).unwrap();
    let old = harness.open(&a);
    harness.packet(old, packet(0.1));
    settle(&mut worker, &session);
    session
        .apply(Command::RebindAudioCaptureSource {
            source,
            binding: b.clone(),
        })
        .unwrap();
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_packets(&session, MediaTime(1)).unwrap();
    harness.close(
        old,
        Err(AudioCaptureError::Native {
            operation: "stop",
            detail: "pending".into(),
        }),
    );
    // A closing stream consumes the only native slot. The owner may retry its
    // close once more while processing the resulting capacity failure.
    harness.close(old, Ok(()));
    settle(&mut worker, &session);
    let snap = worker.snapshot();
    assert!(snap.packet(source).is_none());
    drop(snap);
    worker.reset_timing().unwrap();
    worker.request_packets(&session, MediaTime(0)).unwrap();
    let new = harness.open(&b);
    harness.packet(new, packet(0.2));
    settle(&mut worker, &session);
    harness.finish(&mut worker);
}

#[test]
fn closed_session_and_reopen_discard_old_busy_result_and_explicit_close_does_not_join() {
    let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
    let mut old = Session::new();
    let old_scene = old.preview();
    let bind = binding("A", true);
    add(&mut old, old_scene, "Old", bind.clone());
    worker
        .synchronize(&old, &[FrameScene::Live(old_scene)])
        .unwrap();
    worker.request_packets(&old, MediaTime(0)).unwrap();
    let stream = harness.open(&bind);
    let held = match harness.action() {
        Call::Poll { reply, .. } => reply,
        _ => panic!(),
    };
    old.close();
    assert!(matches!(worker.poll(&old), Err(AudioCaptureError::Closed)));
    let mut reopened = Session::new();
    let new_scene = reopened.preview();
    add(&mut reopened, new_scene, "New", bind.clone());
    worker
        .synchronize(&reopened, &[FrameScene::Live(new_scene)])
        .unwrap();
    assert_eq!(
        worker.request_packets(&reopened, MediaTime(0)),
        Err(AudioCaptureError::Busy)
    );
    held.send(Ok(Some(packet(0.9)))).unwrap();
    harness.poll(stream, Ok(None));
    assert!(settle(&mut worker, &reopened).is_none());
    assert_eq!(worker.snapshot().packet_count(), 0);
    worker.request_packets(&reopened, MediaTime(0)).unwrap();
    harness.close(stream, Ok(()));
    let new = harness.open(&bind);
    let held = match harness.action() {
        Call::Poll { id, reply } => {
            assert_eq!(id, new);
            reply
        }
        _ => panic!(),
    };
    worker.close();
    assert!(worker.is_busy());
    assert_eq!(
        worker.request_packets(&reopened, MediaTime(1)),
        Err(AudioCaptureError::Closed)
    );
    held.send(Ok(None)).unwrap();
    harness.finish(&mut worker);
}

#[test]
fn timing_reset_and_frozen_retired_audio_have_explicit_epoch_and_source_ownership() {
    let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let bind = binding("A", true);
    let (source, item) = add(&mut session, live, "A", bind.clone());
    let mut controller = Controller::new();
    controller
        .set_studio(&mut session, true, MediaTime(0))
        .unwrap();
    controller.before_change(&session).unwrap();
    session
        .apply(Command::RemoveItem { scene: live, item })
        .unwrap();
    assert!(session.snapshot().source(source).is_none());
    let views = controller
        .sync(&mut session, MediaTime(0))
        .unwrap()
        .output
        .scenes();
    worker.synchronize(&session, &views).unwrap();
    assert_eq!(worker.active_source_count(), 1);
    worker.request_packets(&session, MediaTime(100)).unwrap();
    let old = harness.open(&bind);
    let held = match harness.action() {
        Call::Poll { reply, .. } => reply,
        _ => panic!(),
    };
    let epoch = worker.snapshot().epoch();
    assert!(matches!(
        worker.request_packets(&session, MediaTime(99)),
        Err(AudioCaptureError::Invalid(_))
    ));
    worker.reset_timing().unwrap();
    assert_ne!(worker.snapshot().epoch(), epoch);
    assert!(worker.is_busy());
    assert_eq!(worker.last_requested_at(), None);
    held.send(Ok(Some(packet(0.9)))).unwrap();
    harness.poll(old, Ok(None));
    assert!(settle(&mut worker, &session).is_none());
    worker.request_packets(&session, MediaTime(0)).unwrap();
    harness.close(old, Ok(()));
    let new = harness.open(&bind);
    harness.packet(new, packet(0.4));
    settle(&mut worker, &session);
    assert!(worker.snapshot().packet(source).is_some());
    harness.finish(&mut worker);
}

#[test]
fn packet_drain_is_bounded_to_sixteen_and_no_data_preserves_the_shared_packet() {
    let (mut worker, harness) = Harness::new(AudioCaptureLimits::default());
    let mut session = Session::new();
    let live = session.preview();
    let bind = binding("A", true);
    let (source, _) = add(&mut session, live, "A", bind.clone());
    worker
        .synchronize(&session, &[FrameScene::Live(live)])
        .unwrap();
    worker.request_packets(&session, MediaTime(0)).unwrap();
    let stream = harness.open(&bind);
    for index in 0..16 {
        harness.poll(stream, Ok(Some(packet(index as f32 / 16.0))));
    }
    settle(&mut worker, &session);
    harness.idle();
    let before = worker.snapshot();
    assert_eq!(before.packet(source).unwrap().coalesced_packets, 15);
    assert_eq!(
        before.packet(source).unwrap().block.samples,
        [15.0 / 16.0; 2]
    );
    worker.request_packets(&session, MediaTime(1)).unwrap();
    harness.poll(stream, Ok(None));
    settle(&mut worker, &session);
    let after = worker.snapshot();
    assert_eq!(before.generation(), after.generation());
    assert!(std::ptr::eq(
        before.packet(source).unwrap(),
        after.packet(source).unwrap()
    ));
    drop(before);
    drop(after);
    harness.finish(&mut worker);
}

#[test]
fn enumeration_count_flow_and_retained_string_capacity_are_checked_on_the_worker() {
    let limits = AudioCaptureLimits {
        max_devices: 1,
        ..AudioCaptureLimits::default()
    };
    let (mut worker, harness) = Harness::new(limits);
    let session = Session::new();
    worker.synchronize(&session, &[]).unwrap();
    worker.request_devices(&session, AudioFlow::Input).unwrap();
    harness
        .devices()
        .send(Ok(vec![device("A"), device("B")]))
        .unwrap();
    match settle(&mut worker, &session).unwrap() {
        AudioCaptureEvent::Devices { result, .. } => {
            assert!(matches!(result, Err(AudioCaptureError::Limit(_))))
        }
        _ => panic!(),
    }
    let mut bad = device("A");
    bad.flow = AudioFlow::Output;
    worker.request_devices(&session, AudioFlow::Input).unwrap();
    harness.devices().send(Ok(vec![bad])).unwrap();
    match settle(&mut worker, &session).unwrap() {
        AudioCaptureEvent::Devices { result, .. } => {
            assert!(matches!(result, Err(AudioCaptureError::Invalid(_))))
        }
        _ => panic!(),
    }
    let mut name = String::with_capacity(4096);
    name.push_str("Mic A");
    let mut id = String::with_capacity(4096);
    id.push('A');
    let device = AudioDeviceInfo {
        target: AudioTarget::WindowsEndpoint { id },
        name,
        flow: AudioFlow::Input,
        is_default: true,
    };
    worker.request_devices(&session, AudioFlow::Input).unwrap();
    harness.devices().send(Ok(vec![device])).unwrap();
    match settle(&mut worker, &session).unwrap() {
        AudioCaptureEvent::Devices { result, .. } => {
            let devices = result.unwrap();
            assert_eq!(devices[0].name.capacity(), devices[0].name.len());
            let AudioTarget::WindowsEndpoint { id } = &devices[0].target else {
                panic!()
            };
            assert_eq!(id.capacity(), id.len());
        }
        _ => panic!(),
    }
    harness.finish(&mut worker);
}
