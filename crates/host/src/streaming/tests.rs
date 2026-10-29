//! Deterministic, injected ports only. Fake encoded payloads are structural
//! ownership oracles, not codec acceptance; native composition is separate.
use super::*;
use super::{
    backend::*,
    timing::{Outbox, Packet, Track},
    video::{NativeRenderer, Prepared, Renderer},
};
use crate::{
    audio_capture::ProgramCaptureKey,
    audio_output::{AudioOutput, ProgramOutputRouter},
    capture::CaptureSnapshot,
    playback::PlaybackSnapshot,
    transition::{Controller, FrameComposition, FrameScene},
};
use skylive_core::{Command, Session, StreamingSettings};
use std::{
    rc::Rc,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Default)]
struct Probe {
    calls: Mutex<Vec<&'static str>>,
    luma: Mutex<Vec<u8>>,
    pcm: Mutex<Vec<(u64, Vec<i16>)>>,
    wire: Mutex<Vec<(bool, u64)>>,
    blocked_network: AtomicBool,
    network_rejections: AtomicUsize,
    block_render: AtomicBool,
    render_entered: AtomicBool,
    block_drop: AtomicBool,
    drop_entered: AtomicUsize,
    objects_dropped: AtomicUsize,
    fail_audio: AtomicBool,
    block_constructor: AtomicBool,
    constructor_entered: AtomicBool,
}
struct Local {
    owner: thread::ThreadId,
    probe: Arc<Probe>,
    _local: Rc<()>,
}
impl Local {
    fn new(probe: Arc<Probe>) -> Self {
        Self {
            owner: thread::current().id(),
            probe,
            _local: Rc::new(()),
        }
    }
    fn check(&self) {
        assert_eq!(thread::current().id(), self.owner);
    }
}
impl Drop for Local {
    fn drop(&mut self) {
        self.check();
        self.probe.drop_entered.fetch_add(1, Ordering::AcqRel);
        while self.probe.block_drop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
        }
        self.probe.objects_dropped.fetch_add(1, Ordering::AcqRel);
    }
}
struct FakeFactory(Arc<Probe>);
fn avcc() -> Arc<[u8]> {
    Arc::from([
        1, 66, 0, 30, 255, 225, 0, 4, 0x67, 66, 0, 30, 1, 0, 2, 0x68, 0x80,
    ])
}
impl Factory for FakeFactory {
    fn video(&mut self, config: &Config, key: StreamKey) -> Result<Box<dyn VideoPort>, Error> {
        self.0.calls.lock().unwrap().push("video_create");
        self.0.constructor_entered.store(true, Ordering::Release);
        while self.0.block_constructor.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
        }
        Ok(Box::new(FakeVideo {
            local: Local::new(self.0.clone()),
            config: config.video_config(key)?,
            event: Some(CodecEvent::Ready(avcc())),
            closed: false,
        }))
    }
    fn audio(&mut self, config: &Config, key: StreamKey) -> Result<Box<dyn AudioPort>, Error> {
        self.0.calls.lock().unwrap().push("audio_create");
        if self.0.fail_audio.load(Ordering::Acquire) {
            return Err(Error::Aac("injected constructor failure".into()));
        }
        let channels = config.audio_config(key)?.unwrap().channels();
        Ok(Box::new(FakeAudio {
            local: Local::new(self.0.clone()),
            channels,
            event: Some(CodecEvent::Ready(Arc::from([
                0x11,
                0x80 | (channels as u8) << 3,
            ]))),
            closed: false,
            residual: 0,
            packets: 0,
            input: 0,
        }))
    }
    fn publisher(
        &mut self,
        _: &Config,
        _: StreamKey,
        video: Arc<[u8]>,
        audio: Option<Arc<[u8]>>,
    ) -> Result<Box<dyn PublisherPort>, Error> {
        assert_eq!(video.as_ref(), avcc().as_ref());
        assert!(audio.as_ref().is_none_or(|a| a.len() == 2));
        self.0.calls.lock().unwrap().push("publisher_create");
        Ok(Box::new(FakePublisher {
            local: Local::new(self.0.clone()),
            event: Some(PublishEvent::Publishing),
            finished: false,
            packets: 0,
        }))
    }
    fn renderer(&mut self) -> Box<dyn Renderer> {
        Box::new(FakeRenderer {
            probe: self.0.clone(),
            native: Default::default(),
        })
    }
}
struct FakeRenderer {
    probe: Arc<Probe>,
    native: NativeRenderer,
}
impl Renderer for FakeRenderer {
    fn render(&mut self, c: &Config, f: &ProgramVideoFrame) -> Result<Prepared, Error> {
        self.probe.render_entered.store(true, Ordering::Release);
        while self.probe.block_render.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
        }
        self.native.render(c, f)
    }
}
struct FakeVideo {
    local: Local,
    config: crate::native_h264::Config,
    event: Option<CodecEvent>,
    closed: bool,
}
impl VideoPort for FakeVideo {
    fn busy(&self) -> bool {
        self.event.is_some()
    }
    fn closed(&self) -> bool {
        self.closed
    }
    fn encode(&mut self, index: u64, bytes: Box<[u8]>) -> Result<(), Error> {
        self.local.check();
        assert_eq!(bytes.len(), self.config.nv12_bytes());
        self.local.probe.luma.lock().unwrap().push(bytes[0]);
        self.local.probe.calls.lock().unwrap().push("video_encode");
        let (pts, duration) = self.config.frame_timing(index).unwrap();
        self.event = Some(CodecEvent::Packets {
            header: avcc(),
            packets: vec![Packet {
                track: Track::Video,
                bytes: Arc::from([0, 0, 0, 2, 0x65, 0x80]),
                pts,
                dts: pts,
                duration,
            }],
            drained: false,
            aac: None,
        });
        Ok(())
    }
    fn finish(&mut self) -> Result<(), Error> {
        self.local.check();
        self.local.probe.calls.lock().unwrap().push("video_eos");
        self.event = Some(CodecEvent::Packets {
            header: avcc(),
            packets: vec![],
            drained: true,
            aac: None,
        });
        Ok(())
    }
    fn poll(&mut self) -> Result<Option<CodecEvent>, Error> {
        self.local.check();
        Ok(self.event.take())
    }
    fn close(&mut self) {
        self.local.check();
        self.closed = true;
    }
}
struct FakeAudio {
    local: Local,
    channels: u16,
    event: Option<CodecEvent>,
    closed: bool,
    residual: u64,
    packets: u64,
    input: u64,
}
impl FakeAudio {
    fn batch(&mut self, eos: bool) {
        let mut packets = vec![];
        let emit = self.residual >= 1024 || (eos && self.residual > 0);
        let padding = if eos && self.residual > 0 {
            1024 - self.residual
        } else {
            0
        };
        if emit {
            let pts = (u128::from(self.packets) * 1024 * 10_000_000 / 48000) as i64;
            let end = (u128::from(self.packets + 1) * 1024 * 10_000_000 / 48000) as i64;
            packets.push(Packet {
                track: Track::Audio,
                bytes: Arc::from([0x21, 0x10]),
                pts,
                dts: pts,
                duration: end - pts,
            });
            self.packets += 1;
            self.residual = self.residual.saturating_sub(1024);
        }
        self.event = Some(CodecEvent::Packets {
            header: Arc::from([0x11, 0x80 | (self.channels as u8) << 3]),
            packets,
            drained: eos,
            aac: Some(crate::native_aac::StreamStats {
                input_frames: self.input,
                submitted_frames: self.packets * 1024,
                explicit_padding_frames: padding as u16,
                packets: self.packets,
                first_native_pts: (self.packets > 0).then_some(0),
                last_native_end: (self.packets > 0)
                    .then_some((u128::from(self.packets) * 1024 * 10_000_000 / 48000) as i64),
            }),
        });
    }
}
impl AudioPort for FakeAudio {
    fn busy(&self) -> bool {
        self.event.is_some()
    }
    fn closed(&self) -> bool {
        self.closed
    }
    fn encode(&mut self, first: u64, samples: Box<[i16]>) -> Result<(), Error> {
        self.local.check();
        assert_eq!(first, self.input);
        let frames = (samples.len() / usize::from(self.channels)) as u64;
        self.input += frames;
        self.residual += frames;
        self.local
            .probe
            .pcm
            .lock()
            .unwrap()
            .push((first, samples.into_vec()));
        self.batch(false);
        Ok(())
    }
    fn finish(&mut self) -> Result<(), Error> {
        self.local.check();
        self.local.probe.calls.lock().unwrap().push("audio_eos");
        self.batch(true);
        Ok(())
    }
    fn poll(&mut self) -> Result<Option<CodecEvent>, Error> {
        self.local.check();
        Ok(self.event.take())
    }
    fn close(&mut self) {
        self.local.check();
        self.closed = true;
    }
}
struct FakePublisher {
    local: Local,
    event: Option<PublishEvent>,
    finished: bool,
    packets: u64,
}
impl PublisherPort for FakePublisher {
    fn poll(&mut self) -> Result<Option<PublishEvent>, Error> {
        self.local.check();
        Ok(self.event.take())
    }
    fn packet(&mut self, p: &crate::rtmp::EncodedPacket) -> Result<bool, Error> {
        self.local.check();
        if self.local.probe.blocked_network.load(Ordering::Acquire) {
            self.local
                .probe
                .network_rejections
                .fetch_add(1, Ordering::AcqRel);
            return Ok(false);
        }
        self.packets += 1;
        self.local.probe.wire.lock().unwrap().push((
            matches!(p, crate::rtmp::EncodedPacket::Audio(_)),
            p.dts_ns(),
        ));
        Ok(true)
    }
    fn stop(&mut self) {
        self.local.check();
        self.local.probe.calls.lock().unwrap().push("publisher_eos");
        self.event = Some(PublishEvent::Stopped {
            packets: self.packets,
        });
        self.finished = true;
    }
    fn cancel(&mut self) {
        self.local.check();
        self.finished = true;
    }
    fn finished(&self) -> bool {
        self.finished
    }
    fn join(self: Box<Self>) -> Result<(), Error> {
        self.local.check();
        assert!(self.finished);
        Ok(())
    }
}
fn session() -> Session {
    let mut s = Session::new();
    s.apply(Command::SetVideo {
        width: 64,
        height: 48,
        fps_num: 30,
        fps_den: 1,
    })
    .unwrap();
    s
}
fn config(session: &Session, audio: bool) -> Config {
    let m = session.snapshot();
    Config::new(
        session.identity(),
        m.profiles().selected_id(),
        m.video().clone(),
        m.audio(),
        StreamingSettings::new(
            "rtmp://127.0.0.1:1935/live?literal=1".into(),
            "literal/key?token=abc".into(),
            1000,
            1,
            audio,
            128,
        )
        .unwrap(),
    )
    .unwrap()
}

fn frame(s: &mut Session, ns: u64) -> ProgramVideoFrame {
    let now = MediaTime(ns);
    let output = Controller::new().sync(s, now).unwrap().output;
    ProgramVideoFrame {
        session: s.identity(),
        time: now,
        model: s.snapshot(),
        output,
        playback: PlaybackSnapshot::at_time(s, now),
        capture: Arc::new(CaptureSnapshot::empty(s.identity())),
        resource_generation: 0,
    }
}
fn wait(mut test: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !test() {
        assert!(Instant::now() < end, "staged owner test deadline");
        thread::sleep(Duration::from_millis(1));
    }
}
fn next(o: &mut StreamingOwner) -> Event {
    let mut value = None;
    wait(|| {
        value = o.poll();
        value.is_some()
    });
    value.unwrap()
}
fn ready(o: &mut StreamingOwner) {
    assert!(matches!(next(o), Event::ReadyForInputs { .. }));
}
fn activate(o: &mut StreamingOwner, s: &mut Session, ns: u64) {
    o.activate(o.key(), o.epoch(), frame(s, ns)).unwrap();
    wait(|| o.status().is_some_and(|s| s.origin.is_some()));
}
fn fake(s: &Session, audio: bool, probe: Arc<Probe>, pool: Arc<AtomicUsize>) -> StartedOwner {
    StreamingOwner::with_factory(config(s, audio), Box::new(FakeFactory(probe)), pool).unwrap()
}
fn program(s: &Session) -> ProgramCaptureKey {
    ProgramCaptureKey {
        session: s.identity(),
        clock_epoch: 1,
        generation: 1,
        settings: s.snapshot().audio(),
    }
}
fn block(s: &Session, first: u64, frames: usize) -> Arc<crate::audio_program::ProgramAudioBlock> {
    Arc::new(crate::audio_program::ProgramAudioBlock {
        tracks:Vec::new(),
        instance: 1,
        epoch: 1,
        session: s.identity(),
        data_generation: 1,
        routing_serial: 1,
        settings: s.snapshot().audio(),
        start_frame: first,
        frames,
        samples: vec![0.125; frames * usize::from(s.snapshot().audio().channels())],
        program_meters: vec![],
        source_meters: vec![],
        source_meter_revision: None,
        clipped_samples: vec![],
        gaps: vec![],
    })
}
#[test]
fn publishing_handshake_uses_fresh_frame_origin_and_drains_without_reading_started() {
    let mut s = session();
    let probe = Arc::new(Probe::default());
    let pool = Arc::new(AtomicUsize::new(0));
    let StartedOwner {
        mut owner,
        audio_sink,
    } = fake(&s, true, probe.clone(), pool.clone());
    let early = owner
        .activate(owner.key(), owner.epoch(), frame(&mut s, 1))
        .unwrap_err();
    assert!(matches!(early.error, Error::State(_)));
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(&s), AudioOutput::Streaming(audio_sink.unwrap()))
        .unwrap();
    router.submit(program(&s), block(&s, 0, 1024));
    ready(&mut owner);
    assert_eq!(
        probe.calls.lock().unwrap().as_slice(),
        ["video_create", "audio_create", "publisher_create"]
    );
    activate(&mut owner, &mut s, 1);
    owner.finish_video(owner.key(), owner.epoch()).unwrap();
    for i in 1..4 {
        router.submit(program(&s), block(&s, i * 1024, 1024));
    }
    router.prune();
    wait(|| owner.status().is_some_and(|s| s.audio_input_frames == 4095));
    owner.stop();
    router.prune();
    wait(|| {
        router.prune();
        owner.is_closed()
    });
    assert_eq!(pool.load(Ordering::Acquire), 0);
    assert!(matches!(
        next(&mut owner),
        Event::Started {
            origin: MediaTime(1),
            ..
        }
    ));
    let Event::Stopped(status) = next(&mut owner) else {
        panic!("graceful completion")
    };
    assert_eq!(status.video_frames, 1);
    assert_eq!(status.audio_converted_frames, 4095);
    assert_eq!(status.native_aac.explicit_padding_frames, 1);
    assert_eq!(status.native_aac.packets, 4);
    assert_eq!(probe.wire.lock().unwrap()[0], (false, 0));
    assert!(
        probe.wire.lock().unwrap().contains(&(true, 20_800)),
        "retain sub-sample audio offset"
    );
    let calls = probe.calls.lock().unwrap();
    assert!(
        calls.iter().position(|c| *c == "audio_eos").unwrap()
            < calls.iter().position(|c| *c == "publisher_eos").unwrap()
    );
}
#[test]
fn direct_program_pcm_beyond_eight_gui_blocks_and_route_edit_does_not_restart() {
    use crate::{
        audio_capture::{NativeAudioFormat, NativePacketFlags},
        audio_program::{AudioProgram, ProgramAudioLimits, ProgramInputPacket},
    };
    use skylive_core::{AudioCaptureBinding, AudioFilter, AudioFlow, AudioTarget, SourceKind};
    let mut s = session();
    let scene = s.preview();
    let binding =
        AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, true, false).unwrap();
    s.apply(Command::AddAudioCaptureSource {
        scene,
        name: "Real Program DSP fixture".into(),
        binding: binding.clone(),
    })
    .unwrap();
    let source = s
        .snapshot()
        .sources()
        .iter()
        .find(|s| matches!(s.kind(), SourceKind::AudioCapture { .. }))
        .unwrap()
        .id();
    s.apply(Command::SetSourceAudioFilters {
        source,
        filters: vec![AudioFilter::gate(), {
            let mut eq = AudioFilter::simple_eq();
            if let AudioFilter::SimpleEq { output_db, .. } = &mut eq {
                *output_db = -6.0206;
            }
            eq
        }],
    })
    .unwrap();
    let mut program_audio =
        AudioProgram::new(s.snapshot().audio(), ProgramAudioLimits::default()).unwrap();
    program_audio
        .route(
            &s,
            &FrameComposition::Scene(FrameScene::Live(scene)),
            MediaTime(0),
        )
        .unwrap();
    let probe = Arc::new(Probe::default());
    let StartedOwner {
        mut owner,
        audio_sink,
    } = fake(&s, true, probe.clone(), Arc::new(AtomicUsize::new(0)));
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(&s), AudioOutput::Streaming(audio_sink.unwrap()))
        .unwrap();
    ready(&mut owner);
    activate(&mut owner, &mut s, 0);
    for i in 0..12u64 {
        if i == 6 {
            s.apply(Command::SetAudio {
                source,
                gain: 0.5,
                muted: false,
            })
            .unwrap();
            program_audio
                .route(
                    &s,
                    &FrameComposition::Scene(FrameScene::Live(scene)),
                    MediaTime(i * 1024 * 1_000_000_000 / 48000),
                )
                .unwrap();
            owner
                .video(owner.key(), owner.epoch(), frame(&mut s, 200_000_000))
                .unwrap();
            owner.finish_video(owner.key(), owner.epoch()).unwrap();
        }
        let samples = vec![0.2; 2048];
        let ns = i128::from(i) * 1024 * 1_000_000_000 / 48000;
        program_audio
            .ingest(ProgramInputPacket {
                identity: program_audio.identity().unwrap(),
                binding: &binding,
                stream_generation: 1,
                sequence: i + 1,
                first_media_ns: ns,
                received_media_ns: ns,
                format: NativeAudioFormat {
                    sample_rate: 48000,
                    channels: 2,
                    channel_mask: 3,
                },
                samples: &samples,
                flags: NativePacketFlags::default(),
            })
            .unwrap();
        router.submit(
            program(&s),
            Arc::new(program_audio.render(i * 1024, 1024).unwrap()),
        );
    }
    wait(|| {
        owner
            .status()
            .is_some_and(|s| s.audio_input_frames == 12 * 1024)
    });
    owner.stop();
    router.prune();
    wait(|| {
        router.prune();
        owner.is_closed()
    });
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    let Event::Stopped(status) = next(&mut owner) else {
        panic!("continuous PCM completion")
    };
    assert_eq!(status.audio_converted_frames, 12288);
    assert_eq!(status.audio_gap_frames, 0);
    assert_eq!(status.audio_packets, 12);
    let pcm = probe.pcm.lock().unwrap();
    let flat: Vec<i16> = pcm.iter().flat_map(|(_, p)| p.iter().copied()).collect();
    assert_eq!(flat.len(), 24576);
    assert!(
        flat[4096..6 * 2048].iter().all(|v| (*v - 3277).abs() <= 1),
        "real Gate/EQ output first gain"
    );
    assert!(
        flat[8 * 2048..].iter().all(|v| (*v - 1638).abs() <= 1),
        "canonical fader edit in same transport"
    );
    assert_eq!(
        probe
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| **c == "video_create")
            .count(),
        1
    );
    assert_eq!(
        probe
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| **c == "audio_create")
            .count(),
        1
    );
}
#[test]
fn stop_closes_atomic_video_admission_and_waits_already_accepted_frames() {
    let mut s = session();
    let probe = Arc::new(Probe::default());
    probe.block_render.store(true, Ordering::Release);
    let StartedOwner { mut owner, .. } =
        fake(&s, false, probe.clone(), Arc::new(AtomicUsize::new(0)));
    ready(&mut owner);
    owner
        .activate(owner.key(), owner.epoch(), frame(&mut s, 0))
        .unwrap();
    wait(|| probe.render_entered.load(Ordering::Acquire));
    owner
        .video(owner.key(), owner.epoch(), frame(&mut s, 100_000_000))
        .unwrap();
    assert!(matches!(
        owner
            .video(owner.key(), owner.epoch(), frame(&mut s, 200_000_000))
            .unwrap_err()
            .error,
        Error::Limit(_)
    ));
    owner.stop();
    assert!(matches!(
        owner
            .video(owner.key(), owner.epoch(), frame(&mut s, 300_000_000))
            .unwrap_err()
            .error,
        Error::State(_)
    ));
    probe.block_render.store(false, Ordering::Release);
    wait(|| owner.is_closed());
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    let Event::Stopped(status) = next(&mut owner) else {
        panic!("Stop admitted prefix")
    };
    assert_eq!(status.video_frames, 2);
    assert_eq!(status.video_packets, 2);
    assert_eq!(status.skipped_video_frames, 2);
}
#[test]
fn old_epoch_and_changed_format_are_not_published() {
    let mut s = session();
    let probe = Arc::new(Probe::default());
    let StartedOwner { mut owner, .. } =
        fake(&s, false, probe.clone(), Arc::new(AtomicUsize::new(0)));
    ready(&mut owner);
    let error = owner
        .activate(owner.key(), owner.epoch() + 1, frame(&mut s, 0))
        .unwrap_err();
    assert_eq!(error.error, Error::Scope);
    activate(&mut owner, &mut s, 0);
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    s.apply(Command::SetVideo {
        width: 128,
        height: 48,
        fps_num: 30,
        fps_den: 1,
    })
    .unwrap();
    owner
        .video(owner.key(), owner.epoch(), frame(&mut s, 100_000_000))
        .unwrap();
    wait(|| owner.is_closed());
    let Event::Failed(status) = next(&mut owner) else {
        panic!("format lease failure")
    };
    assert_eq!(status.failure.as_deref(), Some(&Error::Format));
    assert_eq!(status.video_frames, 1);
}
#[test]
fn unread_failure_and_blocked_actual_destructor_hold_global_capacity() {
    let s = session();
    let pool = Arc::new(AtomicUsize::new(0));
    let probe = Arc::new(Probe::default());
    probe.block_drop.store(true, Ordering::Release);
    let StartedOwner { mut owner, .. } = fake(&s, false, probe.clone(), pool.clone());
    ready(&mut owner);
    owner.cancel();
    wait(|| probe.drop_entered.load(Ordering::Acquire) > 0);
    let other = Arc::new(Probe::default());
    let StartedOwner { owner: second, .. } = fake(&s, false, other, pool.clone());
    assert!(matches!(
        StreamingOwner::with_factory(
            config(&s, false),
            Box::new(FakeFactory(Arc::new(Probe::default()))),
            pool.clone()
        )
        .err(),
        Some(Error::Limit(_))
    ));
    assert!(!owner.is_closed());
    assert_eq!(pool.load(Ordering::Acquire), 2);
    second.cancel();
    wait(|| second.is_closed());
    assert_eq!(pool.load(Ordering::Acquire), 1);
    probe.block_drop.store(false, Ordering::Release);
    wait(|| owner.is_closed());
    assert_eq!(pool.load(Ordering::Acquire), 0);
    assert!(matches!(next(&mut owner), Event::Failed(_)));
}
#[test]
fn partial_constructor_failure_retires_prior_owner_before_releasing_service() {
    let s = session();
    let probe = Arc::new(Probe::default());
    probe.fail_audio.store(true, Ordering::Release);
    probe.block_drop.store(true, Ordering::Release);
    let pool = Arc::new(AtomicUsize::new(0));
    let StartedOwner {
        mut owner,
        audio_sink,
    } = fake(&s, true, probe.clone(), pool.clone());
    drop(audio_sink);
    wait(|| probe.drop_entered.load(Ordering::Acquire) > 0);
    assert!(!owner.is_closed());
    assert_eq!(pool.load(Ordering::Acquire), 1);
    probe.block_drop.store(false, Ordering::Release);
    wait(|| owner.is_closed());
    assert_eq!(pool.load(Ordering::Acquire), 0);
    let Event::Failed(status) = next(&mut owner) else {
        panic!("constructor failed")
    };
    assert!(matches!(status.failure.as_deref(), Some(Error::Aac(_))));
}
#[test]
fn native_signed_dts_gets_one_common_shift_and_observed_duration_is_not_rewritten() {
    let key = StreamKey {
        session: 1,
        output: 9,
    };
    let mut out = Outbox::new(key, true);
    out.audio_offset(208).unwrap();
    assert!(
        out.reserve(Track::Video, super::timing::Operation::Encode)
            .unwrap()
    );
    out.admit_batch(
        Track::Video,
        vec![Packet {
            track: Track::Video,
            bytes: Arc::from([0, 0, 0, 2, 0x65, 0x80]),
            pts: 0,
            dts: 0,
            duration: 333333,
        }],
        false,
    )
    .unwrap();
    assert!(out.front().unwrap().is_none());
    assert!(
        out.reserve(Track::Audio, super::timing::Operation::Encode)
            .unwrap()
    );
    out.admit_batch(
        Track::Audio,
        vec![Packet {
            track: Track::Audio,
            bytes: Arc::from([0x21]),
            pts: -213333,
            dts: -213333,
            duration: 213335,
        }],
        false,
    )
    .unwrap();
    assert_eq!(out.front().unwrap().unwrap().dts_ns(), 0);
    out.pop().unwrap();
    assert!(out.front().unwrap().is_none());
    assert!(
        out.reserve(Track::Audio, super::timing::Operation::Encode)
            .unwrap()
    );
    assert!(
        out.admit_batch(
            Track::Audio,
            vec![Packet {
                track: Track::Audio,
                bytes: Arc::from([0x21]),
                pts: -213333,
                dts: -213333,
                duration: 213335
            }],
            false
        )
        .is_err()
    );
    out.admit_batch(Track::Audio, vec![], false).unwrap();
    assert!(
        out.reserve(Track::Audio, super::timing::Operation::Finish)
            .unwrap()
    );
    out.admit_batch(Track::Audio, vec![], true).unwrap();
    assert_eq!(out.front().unwrap().unwrap().dts_ns(), 21_312_500);
    out.pop().unwrap();
    assert!(
        Packet {
            track: Track::Audio,
            bytes: Arc::from([0x21]),
            pts: i64::MAX,
            dts: i64::MAX,
            duration: 1
        }
        .validate()
        .is_err()
    );
}
#[test]
fn canonical_url_settings_and_floor_gop_are_used_without_a_second_parser() {
    let s = session();
    let c = config(&s, false);
    assert_eq!(
        c.settings().server_url(),
        "rtmp://127.0.0.1:1935/live?literal=1"
    );
    assert_eq!(c.settings().stream_key(), "literal/key?token=abc");
    assert_eq!(
        c.video_config(StreamKey {
            session: s.identity(),
            output: 1
        })
        .unwrap()
        .gop(),
        30
    );
    let v = VideoSettings::new(64, 48, 30000, 1001).unwrap();
    let settings = StreamingSettings::new(
        "rtmp://[::1]:1936/live/a?token=%2F".into(),
        "literal/key".into(),
        1000,
        2,
        false,
        128,
    )
    .unwrap();
    let c = Config::new(
        s.identity(),
        s.snapshot().profiles().selected_id(),
        v,
        s.snapshot().audio(),
        settings,
    )
    .unwrap();
    assert_eq!(
        c.video_config(StreamKey {
            session: s.identity(),
            output: 1
        })
        .unwrap()
        .gop(),
        59,
        "OBS integer floor"
    );
}
#[test]
fn stop_while_network_full_waits_then_drains_every_admitted_pcm_block() {
    let mut s = session();
    let probe = Arc::new(Probe::default());
    probe.blocked_network.store(true, Ordering::Release);
    let StartedOwner {
        mut owner,
        audio_sink,
    } = fake(&s, true, probe.clone(), Arc::new(AtomicUsize::new(0)));
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(&s), AudioOutput::Streaming(audio_sink.unwrap()))
        .unwrap();
    ready(&mut owner);
    activate(&mut owner, &mut s, 0);
    owner.finish_video(owner.key(), owner.epoch()).unwrap();
    for i in 0..20 {
        router.submit(program(&s), block(&s, i * 1024, 1024));
    }
    wait(|| probe.network_rejections.load(Ordering::Acquire) != 0);
    assert_eq!(owner.status().unwrap().packets_published, 0);
    owner.stop();
    router.prune();
    thread::sleep(Duration::from_millis(10));
    assert!(!owner.is_closed(), "full output cannot claim drained");
    probe.blocked_network.store(false, Ordering::Release);
    wait(|| {
        router.prune();
        owner.is_closed()
    });
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    let Event::Stopped(status) = next(&mut owner) else {
        panic!("bounded backpressure drain")
    };
    assert_eq!(status.audio_input_frames, 20 * 1024);
    assert_eq!(status.audio_packets, 20);
    assert_eq!(status.packets_admitted, 21);
    assert_eq!(status.packets_published, 21);
    assert_eq!(
        probe
            .pcm
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.len())
            .sum::<usize>(),
        20 * 2048
    );
}
#[test]
fn recoverable_capture_failure_keeps_exact_frame_source_name_and_encodes_other_layers() {
    use skylive_core::{DisplayCaptureBinding, DisplayTarget};
    let mut s = session();
    let scene = s.preview();
    s.apply(Command::AddColorSource {
        scene,
        name: "Surviving red layer".into(),
        rgba: [255, 0, 0, 255],
    })
    .unwrap();
    s.apply(Command::AddDisplayCaptureSource {
        scene,
        name: "Original captured display name".into(),
        binding: DisplayCaptureBinding::new(
            DisplayTarget::Windows {
                device_id: "hostgdi:fixture".into(),
            },
            false,
            64,
            48,
        )
        .unwrap(),
    })
    .unwrap();
    let original = frame(&mut s, 0);
    let display = original
        .model
        .sources()
        .iter()
        .find(|s| matches!(s.kind(), skylive_core::SourceKind::DisplayCapture { .. }))
        .unwrap()
        .id();
    let probe = Arc::new(Probe::default());
    let StartedOwner { mut owner, .. } =
        fake(&s, false, probe.clone(), Arc::new(AtomicUsize::new(0)));
    ready(&mut owner);
    // The newer canonical rename is not the historical frame being encoded.
    s.apply(Command::RenameSource {
        source: display,
        name: "Newer unrelated display name".into(),
    })
    .unwrap();
    owner
        .activate(owner.key(), owner.epoch(), original)
        .unwrap();
    wait(|| owner.status().is_some_and(|s| s.video_frames == 1));
    owner.stop();
    wait(|| owner.is_closed());
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    let Event::Stopped(status) = next(&mut owner) else {
        panic!("recoverable source loss")
    };
    assert_eq!(status.degraded_frames, 1);
    let warnings = status.warnings.unwrap();
    assert_eq!(
        warnings.source_name(display),
        Some("Original captured display name")
    );
    assert!(!warnings.diagnostics.is_empty());
    assert!(warnings.retained_bytes() <= 64 * 1024);
    assert_eq!(status.video_packets, 1);
    assert_eq!(
        probe.luma.lock().unwrap().as_slice(),
        [63],
        "missing display must preserve the real red layer"
    );
}
#[test]
fn stopped_blocked_constructor_remains_an_active_service_until_it_returns() {
    let s = session();
    let pool = Arc::new(AtomicUsize::new(0));
    let probe = Arc::new(Probe::default());
    probe.block_constructor.store(true, Ordering::Release);
    let StartedOwner { owner, .. } = fake(&s, false, probe.clone(), pool.clone());
    wait(|| probe.constructor_entered.load(Ordering::Acquire));
    owner.stop();
    assert_eq!(pool.load(Ordering::Acquire), 1);
    assert!(!owner.is_closed());
    let StartedOwner { owner: second, .. } =
        fake(&s, false, Arc::new(Probe::default()), pool.clone());
    assert!(
        StreamingOwner::with_factory(
            config(&s, false),
            Box::new(FakeFactory(Arc::new(Probe::default()))),
            pool.clone()
        )
        .is_err()
    );
    second.stop();
    wait(|| second.is_closed());
    probe.block_constructor.store(false, Ordering::Release);
    wait(|| owner.is_closed());
    assert_eq!(pool.load(Ordering::Acquire), 0);
}

#[test]
fn started_event_cannot_overtake_the_matching_status_publication() {
    let mut session = session();
    let probe = Arc::new(Probe::default());
    let StartedOwner { mut owner, .. } =
        fake(&session, false, probe, Arc::new(AtomicUsize::new(0)));
    ready(&mut owner);
    owner
        .activate(owner.key(), owner.epoch(), frame(&mut session, 7))
        .unwrap();
    assert!(matches!(
        next(&mut owner),
        Event::Started {
            origin: MediaTime(7),
            ..
        }
    ));
    let status = owner.status().unwrap();
    assert_eq!(status.phase, Phase::Publishing);
    assert_eq!(status.origin, Some(MediaTime(7)));
    assert_eq!(status.video_frames, 1);
    owner.stop();
    wait(|| owner.is_closed());
    assert!(matches!(next(&mut owner), Event::Stopped(_)));
}

mod watermark_tests {
    include!("watermark_owner_tests.rs");
}
