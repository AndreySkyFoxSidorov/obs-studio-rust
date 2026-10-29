use super::*;
#[test]
fn keyed_video_eof_is_idempotent_audio_continues_and_real_audio_barrier_is_required() {
    let mut s = session();
    let probe = Arc::new(Probe::default());
    let StartedOwner {
        mut owner,
        audio_sink,
    } = fake(&s, true, probe.clone(), Arc::new(AtomicUsize::new(0)));
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(&s), AudioOutput::Streaming(audio_sink.unwrap()))
        .unwrap();
    router.submit(program(&s), block(&s, 0, 1024));
    ready(&mut owner);
    activate(&mut owner, &mut s, 0);
    let wrong = StreamKey {
        session: owner.key().session,
        output: owner.key().output + 1,
    };
    assert!(matches!(
        owner.finish_video(wrong, owner.epoch()),
        Err(Error::Scope)
    ));
    assert!(matches!(
        owner.finish_video(owner.key(), owner.epoch() + 1),
        Err(Error::Scope)
    ));
    owner
        .video(owner.key(), owner.epoch(), frame(&mut s, 33_333_334))
        .unwrap();
    owner.finish_video(owner.key(), owner.epoch()).unwrap();
    owner.finish_video(owner.key(), owner.epoch()).unwrap();
    assert!(matches!(
        owner
            .video(owner.key(), owner.epoch(), frame(&mut s, 33_333_334))
            .unwrap_err()
            .error,
        Error::State(_)
    ));
    for n in 1..20 {
        router.submit(program(&s), block(&s, n * 1024, 1024));
    }
    wait(|| {
        owner
            .status()
            .is_some_and(|s| s.audio_input_frames == 20 * 1024 && s.interleave.video_drained)
    });
    owner.finish_audio(owner.key(), owner.epoch()).unwrap();
    owner.finish_audio(owner.key(), owner.epoch()).unwrap();
    wait(|| owner.status().is_some_and(|s| s.audio_sealed));
    assert!(
        !owner.status().unwrap().interleave.audio_drained,
        "a UI request is not the unique producer's end acknowledgment"
    );
    assert!(!owner.is_closed());
    wait(|| {
        router.prune();
        owner.status().is_some_and(|s| s.interleave.audio_drained)
    });
    assert!(
        !owner.is_closed(),
        "track EOF does not pretend the whole service stopped"
    );
    owner.stop();
    wait(|| {
        router.prune();
        owner.is_closed()
    });
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    let Event::Stopped(status) = next(&mut owner) else {
        panic!("clean terminal")
    };
    assert_eq!(status.audio_input_frames, 20 * 1024);
    assert_eq!(status.audio_packets, 20);
    assert_eq!(status.video_packets, 2);
    assert!(status.interleave.video_drained && status.interleave.audio_drained);
    let wire = probe.wire.lock().unwrap();
    assert_eq!(wire.len(), 22);
    assert!(wire.windows(2).all(|p| p[0].1 <= p[1].1));
    assert_eq!(&wire[..2], &[(false, 0), (true, 0)]);
}
#[test]
fn audio_eof_allows_later_video_and_never_accepts_more_stream_pcm_after_real_seal() {
    let mut s = session();
    let probe = Arc::new(Probe::default());
    let StartedOwner {
        mut owner,
        audio_sink,
    } = fake(&s, true, probe.clone(), Arc::new(AtomicUsize::new(0)));
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(&s), AudioOutput::Streaming(audio_sink.unwrap()))
        .unwrap();
    router.submit(program(&s), block(&s, 0, 1024));
    ready(&mut owner);
    activate(&mut owner, &mut s, 0);
    assert!(matches!(
        owner.finish_audio(owner.key(), owner.epoch() + 1),
        Err(Error::Scope)
    ));
    router.submit(program(&s), block(&s, 1024, 1024));
    wait(|| owner.status().is_some_and(|s| s.audio_input_frames == 2048));
    owner.finish_audio(owner.key(), owner.epoch()).unwrap();
    wait(|| owner.status().is_some_and(|s| s.audio_sealed));
    wait(|| {
        router.prune();
        owner.status().is_some_and(|s| s.interleave.audio_drained)
    });
    router.submit(program(&s), block(&s, 2048, 1024));
    owner
        .video(owner.key(), owner.epoch(), frame(&mut s, 33_333_334))
        .unwrap();
    wait(|| owner.status().is_some_and(|s| s.video_frames == 2));
    owner.finish_video(owner.key(), owner.epoch()).unwrap();
    owner.stop();
    wait(|| {
        router.prune();
        owner.is_closed()
    });
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    let Event::Stopped(status) = next(&mut owner) else {
        panic!("clean terminal")
    };
    assert_eq!(status.audio_input_frames, 2048);
    assert_eq!(status.audio_packets, 2);
    assert_eq!(status.video_packets, 2);
    assert_eq!(
        *probe.wire.lock().unwrap(),
        [
            (false, 0),
            (true, 0),
            (true, 21_333_300),
            (false, 33_333_300)
        ]
    );
}
struct FaultFactory {
    base: FakeFactory,
    fault: Arc<AtomicBool>,
}
struct FaultAudio {
    inner: Box<dyn AudioPort>,
    fault: Arc<AtomicBool>,
}
impl Factory for FaultFactory {
    fn video(&mut self, c: &Config, k: StreamKey) -> Result<Box<dyn VideoPort>, Error> {
        self.base.video(c, k)
    }
    fn audio(&mut self, c: &Config, k: StreamKey) -> Result<Box<dyn AudioPort>, Error> {
        Ok(Box::new(FaultAudio {
            inner: self.base.audio(c, k)?,
            fault: self.fault.clone(),
        }))
    }
    fn publisher(
        &mut self,
        c: &Config,
        k: StreamKey,
        v: Arc<[u8]>,
        a: Option<Arc<[u8]>>,
    ) -> Result<Box<dyn PublisherPort>, Error> {
        self.base.publisher(c, k, v, a)
    }
    fn renderer(&mut self) -> Box<dyn Renderer> {
        self.base.renderer()
    }
}
impl AudioPort for FaultAudio {
    fn busy(&self) -> bool {
        self.inner.busy()
    }
    fn closed(&self) -> bool {
        self.inner.closed()
    }
    fn encode(&mut self, first: u64, samples: Box<[i16]>) -> Result<(), Error> {
        self.inner.encode(first, samples)
    }
    fn finish(&mut self) -> Result<(), Error> {
        self.inner.finish()
    }
    fn poll(&mut self) -> Result<Option<CodecEvent>, Error> {
        if self.fault.swap(false, Ordering::AcqRel) {
            return Err(Error::Aac(
                "injected AAC failure while awaiting opposing watermark".into(),
            ));
        }
        self.inner.poll()
    }
    fn close(&mut self) {
        self.inner.close()
    }
}
#[test]
fn native_error_is_polled_while_track_has_no_new_batch_room_and_original_cause_survives() {
    let mut s = session();
    let probe = Arc::new(Probe::default());
    let fault = Arc::new(AtomicBool::new(false));
    let StartedOwner {
        mut owner,
        audio_sink,
    } = StreamingOwner::with_factory(
        config(&s, true),
        Box::new(FaultFactory {
            base: FakeFactory(probe.clone()),
            fault: fault.clone(),
        }),
        Arc::new(AtomicUsize::new(0)),
    )
    .unwrap();
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(&s), AudioOutput::Streaming(audio_sink.unwrap()))
        .unwrap();
    ready(&mut owner);
    activate(&mut owner, &mut s, 0);
    for n in 0..5 {
        router.submit(program(&s), block(&s, n * 1024, 1024));
    }
    wait(|| {
        owner.status().is_some_and(|s| {
            s.interleave.audio_packets >= 2
                && s.interleave.waiting == InterleaveWait::VideoWatermark
        })
    });
    let _release = ReleaseDrop(probe.clone());
    probe.block_drop.store(true, Ordering::Release);
    fault.store(true, Ordering::Release);
    wait(|| {
        router.prune();
        owner
            .status()
            .is_some_and(|s| s.phase == Phase::Retiring && s.failure.is_some())
            && probe.drop_entered.load(Ordering::Acquire) > 0
    });
    assert!(!owner.is_closed());
    let rejected = owner.video(owner.key(), owner.epoch(), frame(&mut s, 33_333_334));
    probe.block_drop.store(false, Ordering::Release);
    assert!(
        matches!(rejected.unwrap_err().error, Error::State(_)),
        "retiring service must not accept snapshots discarded by teardown"
    );
    wait(|| {
        router.prune();
        owner.is_closed()
    });
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    let Event::Failed(status) = next(&mut owner) else {
        panic!("original failure terminal")
    };
    assert!(
        matches!(status.failure.as_deref(),Some(Error::Aac(text))if text=="injected AAC failure while awaiting opposing watermark")
    );
}
#[test]
fn video_only_finish_and_cancel_keep_typed_scope_and_actual_retirement() {
    let mut s = session();
    let probe = Arc::new(Probe::default());
    let pool = Arc::new(AtomicUsize::new(0));
    let StartedOwner { mut owner, .. } = fake(&s, false, probe, pool.clone());
    assert!(matches!(
        owner.finish_video(owner.key(), owner.epoch()),
        Err(Error::State(_))
    ));
    ready(&mut owner);
    activate(&mut owner, &mut s, 0);
    assert!(matches!(
        owner.finish_audio(owner.key(), owner.epoch()),
        Err(Error::State(_))
    ));
    owner.finish_video(owner.key(), owner.epoch()).unwrap();
    owner.finish_video(owner.key(), owner.epoch()).unwrap();
    wait(|| owner.status().is_some_and(|s| s.interleave.video_drained));
    owner.cancel();
    wait(|| owner.is_closed());
    assert_eq!(pool.load(Ordering::Acquire), 0);
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    assert!(
        matches!(next(&mut owner),Event::Failed(s)if matches!(s.failure.as_deref(),Some(Error::Cancelled)))
    );
}

struct ReleaseDrop(Arc<Probe>);
impl Drop for ReleaseDrop {
    fn drop(&mut self) {
        self.0.block_drop.store(false, Ordering::Release);
    }
}

#[test]
fn three_audio_aus_keep_real_eos_tail_credit_without_future_video_or_increased_queue() {
    let mut s = session();
    let probe = Arc::new(Probe::default());
    let StartedOwner {
        mut owner,
        audio_sink,
    } = fake(&s, true, probe.clone(), Arc::new(AtomicUsize::new(0)));
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(&s), AudioOutput::Streaming(audio_sink.unwrap()))
        .unwrap();
    router.submit(program(&s), block(&s, 0, 1024));
    ready(&mut owner);
    activate(&mut owner, &mut s, 0);
    for n in 1..3 {
        router.submit(program(&s), block(&s, n * 1024, 1024));
    }
    wait(|| owner.status().is_some_and(|s| s.audio_packets == 3));
    let before = owner.status().unwrap();
    assert_eq!(before.interleave.audio_packets, 3);
    assert_eq!(before.interleave.audio_reserved_packets, 2);
    assert_eq!(before.interleave.video_packets, 0);
    assert!(!before.interleave.audio_drained);
    owner.finish_audio(owner.key(), owner.epoch()).unwrap();
    wait(|| {
        router.prune();
        owner.status().is_some_and(|s| s.interleave.audio_drained)
    });
    assert!(probe.calls.lock().unwrap().contains(&"audio_eos"));
    assert!(!owner.is_closed());
    owner
        .video(owner.key(), owner.epoch(), frame(&mut s, 100_000_000))
        .unwrap();
    owner.finish_video(owner.key(), owner.epoch()).unwrap();
    owner.stop();
    wait(|| {
        router.prune();
        owner.is_closed()
    });
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    let Event::Stopped(status) = next(&mut owner) else {
        panic!("clean terminal")
    };
    assert_eq!(status.audio_packets, 3);
    assert_eq!(status.video_packets, 2);
    assert_eq!(status.audio_input_frames, 3072);
    assert!(
        probe
            .wire
            .lock()
            .unwrap()
            .windows(2)
            .all(|p| p[0].1 <= p[1].1)
    );
}

#[test]
fn three_video_aus_can_reach_real_eos_while_audio_watermark_stays_at_zero() {
    fn observed(owner: &StreamingOwner, description: &str, check: impl Fn(&Status) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = owner.status();
            if status.as_ref().is_some_and(&check) {
                return;
            }
            assert!(Instant::now() < deadline, "{description}: {status:?}");
            thread::sleep(Duration::from_millis(1));
        }
    }
    let mut s = session();
    let probe = Arc::new(Probe::default());
    let StartedOwner {
        mut owner,
        audio_sink,
    } = fake(&s, true, probe.clone(), Arc::new(AtomicUsize::new(0)));
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(&s), AudioOutput::Streaming(audio_sink.unwrap()))
        .unwrap();
    router.submit(program(&s), block(&s, 0, 1024));
    ready(&mut owner);
    activate(&mut owner, &mut s, 0);
    // Activation is acknowledged by the actual producer port. Its owner must
    // flush preroll even when this fixture intentionally submits no new PCM.
    router.prune();
    observed(&owner, "initial real AVC/AAC output", |s| {
        s.video_frames == 1 && s.audio_packets == 1
    });
    for n in 1..3 {
        owner
            .video(owner.key(), owner.epoch(), frame(&mut s, n * 33_333_334))
            .unwrap();
    }
    observed(&owner, "three accepted AVC frames", |s| s.video_packets == 3);
    let before = owner.status().unwrap();
    assert_eq!(before.interleave.video_packets, 3);
    assert_eq!(before.interleave.highest_audio_100ns, Some(0));
    owner.finish_video(owner.key(), owner.epoch()).unwrap();
    observed(&owner, "real AVC EOS", |s| s.interleave.video_drained);
    assert!(probe.calls.lock().unwrap().contains(&"video_eos"));
    assert!(!owner.status().unwrap().interleave.audio_drained);
    router.submit(program(&s), block(&s, 1024, 1024));
    owner.stop();
    wait(|| {
        router.prune();
        owner.is_closed()
    });
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    let Event::Stopped(status) = next(&mut owner) else {
        panic!("clean terminal")
    };
    assert_eq!(status.video_packets, 3);
    assert_eq!(status.audio_packets, 2);
    let wire = probe.wire.lock().unwrap();
    assert_eq!(wire.len(), 5);
    assert!(wire.windows(2).all(|p| p[0].1 <= p[1].1));
}

#[test]
fn program_fault_is_observed_while_a_bounded_pcm_chunk_waits_for_video_progress() {
    let mut s = session();
    let probe = Arc::new(Probe::default());
    let StartedOwner {
        mut owner,
        audio_sink,
    } = fake(&s, true, probe, Arc::new(AtomicUsize::new(0)));
    let mut router = ProgramOutputRouter::default();
    router
        .attach(program(&s), AudioOutput::Streaming(audio_sink.unwrap()))
        .unwrap();
    router.submit(program(&s), block(&s, 0, 1024));
    ready(&mut owner);
    activate(&mut owner, &mut s, 0);
    for n in 1..5 {
        router.submit(program(&s), block(&s, n * 1024, 1024));
    }
    wait(|| {
        owner
            .status()
            .is_some_and(|s| s.audio_packets == 3 && s.audio_converted_frames >= 4096)
    });
    for n in 5..65 {
        router.submit(program(&s), block(&s, n * 1024, 1024));
    }
    wait(|| {
        router.prune();
        owner.is_closed()
    });
    assert!(matches!(next(&mut owner), Event::Started { .. }));
    let Event::Failed(status) = next(&mut owner) else {
        panic!("real Program fault")
    };
    assert!(
        matches!(status.failure.as_deref(),Some(Error::Audio(text)) if text=="Program streaming sink: Backpressure")
    );
}
