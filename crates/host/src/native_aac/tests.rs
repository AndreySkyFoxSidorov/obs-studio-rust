//! Pure protocol/PCM fixtures, not native-encoder acceptance.
use super::*;
use std::{
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
fn config() -> Config {
    Config::new(
        Generation {
            session: 1,
            output: 2,
            encoder: 3,
        },
        2,
        128_000,
    )
    .unwrap()
}
fn header(config: &Config) -> CodecConfiguration {
    CodecConfiguration::from_asc(&[0x11, 0x80 | ((config.channels() as u8) << 3)], config).unwrap()
}
fn input(config: &Config, first: u64, frames: usize) -> PcmInput {
    PcmInput::new(
        config,
        first,
        vec![100; frames * usize::from(config.channels())].into_boxed_slice(),
    )
    .unwrap()
}
fn next(worker: &mut EncoderWorker) -> Event {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(event) = worker.poll() {
            return event;
        }
        assert!(Instant::now() < deadline, "AAC worker reply deadline");
        thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn exact_grid_and_pcm_conversion_boundaries_reject_before_any_native_call() {
    let config = config();
    assert_eq!(config.sample_timing(0, 1024).unwrap(), (0, 213333));
    assert_eq!(config.sample_timing(2048, 1024).unwrap(), (426666, 213334));
    let mut sum = 0;
    for index in 0..48_000 {
        let (pts, duration) = config.sample_timing(index, 1).unwrap();
        assert_eq!(pts, sum);
        sum += duration;
    }
    assert_eq!(sum, 10_000_000);
    assert!(config.sample_timing(u64::MAX, 1).is_err());
    assert!(config.sample_timing(0, 0).is_err());
    assert!(
        Config::new(
            Generation {
                session: 0,
                ..config.generation()
            },
            2,
            128_000
        )
        .is_err()
    );
    for channels in [0, 3, 8] {
        assert!(Config::new(config.generation(), channels, 128_000).is_err());
    }
    assert!(Config::new(config.generation(), 2, 1).is_err());
    assert!(PcmInput::new(&config, 0, vec![0; 3].into_boxed_slice()).is_err());
    assert!(PcmInput::new(&config, 0, vec![0; 2050].into_boxed_slice()).is_err());
    assert_eq!(
        pcm16_interleaved(&[-2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 1.0 / 32768.0], 2)
            .unwrap()
            .as_ref(),
        &[-32768, -32768, -16384, 0, 16384, 32767, 32767, 1]
    );
    assert!(pcm16_interleaved(&[0.0, f32::NAN], 2).is_err());
    assert!(pcm16_interleaved(&[f32::INFINITY], 1).is_err());
    assert!(pcm16_interleaved(&[0.0; 2050], 2).is_err());
}
#[test]
fn native_header_is_validated_and_preserved_without_adts_or_guessed_asc() {
    let config = config();
    let mut native = vec![0; 12];
    native[2] = 0x29;
    native.extend_from_slice(&[0x11, 0x90]);
    let parsed = CodecConfiguration::from_native(&native, &config).unwrap();
    assert_eq!(parsed.asc(), [0x11, 0x90]);
    assert_eq!(parsed.channels(), 2);
    let mut bad = native.clone();
    bad[0] = 1;
    assert!(CodecConfiguration::from_native(&bad, &config).is_err());
    let mut bad = native.clone();
    bad[4] = 1;
    assert!(CodecConfiguration::from_native(&bad, &config).is_err());
    let mut bad = native.clone();
    bad[12] = 0x12;
    assert!(CodecConfiguration::from_native(&bad, &config).is_err());
    assert!(CodecConfiguration::from_native(&native[..13], &config).is_err());
    assert!(CodecConfiguration::from_native(&[0; 129], &config).is_err());
    assert!(CodecConfiguration::from_asc(&[0x11, 0x88], &config).is_err());
    assert!(CodecConfiguration::from_asc(&[0x11, 0x91], &config).is_err());
    assert!(AccessUnit::new(config.generation(), 0, vec![0xff, 0xf1, 0, 0], 0, 0, 213333).is_err());
    assert!(
        AccessUnit::new(
            config.generation(),
            0,
            vec![1; MAX_AU_BYTES + 1],
            0,
            0,
            213333
        )
        .is_err()
    );
    assert!(AccessUnit::new(config.generation(), 0, vec![1], 0, 1, 213333).is_err());
    let packet = AccessUnit::new(
        config.generation(),
        0,
        vec![0x21, 0x01],
        -213333,
        -213333,
        213333,
    )
    .unwrap();
    assert_eq!(
        packet.pts(),
        -213333,
        "signed SDK priming observations preserved"
    );
}
#[test]
fn residual_partition_identity_eos_padding_and_invalid_position_are_exact() {
    let config = config();
    let payload: Vec<i16> = (0..2500 * 2).map(|n| (n % 30001) as i16).collect();
    let mut assembler = pcm::Assembler::new(config.clone());
    let mut blocks = Vec::new();
    let mut first = 0;
    for count in [1, 1023, 13, 1024, 439] {
        if let Some(block) = assembler
            .push(
                PcmInput::new(
                    &config,
                    first,
                    payload[first as usize * 2..(first as usize + count) * 2]
                        .to_vec()
                        .into_boxed_slice(),
                )
                .unwrap(),
            )
            .unwrap()
        {
            blocks.push(block);
        }
        first += count as u64;
        assert!(assembler.residual_frames() <= 1023);
    }
    assert_eq!(first, 2500);
    assert_eq!(assembler.received, 2500);
    assert_eq!(assembler.submitted, 2048);
    let old = (
        assembler.received,
        assembler.submitted,
        assembler.residual_frames(),
    );
    assert!(assembler.push(input(&config, 0, 1)).is_err());
    assert_eq!(
        (
            assembler.received,
            assembler.submitted,
            assembler.residual_frames()
        ),
        old
    );
    blocks.push(assembler.finish().unwrap().unwrap());
    assert_eq!(assembler.padding, 572);
    assert_eq!(assembler.received, 2500);
    assert_eq!(assembler.submitted, 3072);
    assert!(assembler.finish().is_err());
    let actual: Vec<i16> = blocks
        .iter()
        .flat_map(|block| block.samples.iter().copied())
        .collect();
    assert_eq!(&actual[..payload.len()], payload);
    assert!(actual[payload.len()..].iter().all(|sample| *sample == 0));
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.first_frame)
            .collect::<Vec<_>>(),
        vec![0, 1024, 2048]
    );
    let mut empty = pcm::Assembler::new(config);
    assert!(empty.finish().unwrap().is_none());
    assert_eq!(empty.padding, 0);
}
struct Fake {
    config: Config,
    header: CodecConfiguration,
    assembler: pcm::Assembler,
    packets: u64,
    calls: Arc<AtomicUsize>,
    drop_thread: Arc<Mutex<Option<thread::ThreadId>>>,
    owner: thread::ThreadId,
    _not_send: Rc<()>,
    fail: bool,
}
impl Adapter for Fake {
    fn configuration(&self) -> &CodecConfiguration {
        &self.header
    }
    fn stats(&self) -> StreamStats {
        StreamStats {
            input_frames: self.assembler.received,
            submitted_frames: self.assembler.submitted,
            explicit_padding_frames: self.assembler.padding,
            packets: self.packets,
            first_native_pts: None,
            last_native_end: None,
        }
    }
    fn encode(&mut self, input: PcmInput) -> Result<Vec<AccessUnit>, Error> {
        assert_eq!(self.owner, thread::current().id());
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(Error::Native {
                operation: "synthetic failure",
                hresult: 0x80004005u32 as i32,
                detail: "exact fixture detail".into(),
            });
        }
        let block = self.assembler.push(input)?;
        self.packet(block)
    }
    fn drain(&mut self) -> Result<Vec<AccessUnit>, Error> {
        let block = self.assembler.finish()?;
        self.packet(block)
    }
}
impl Fake {
    fn packet(&mut self, block: Option<pcm::PcmBlock>) -> Result<Vec<AccessUnit>, Error> {
        let Some(block) = block else {
            return Ok(vec![]);
        };
        let (pts, duration) = self.config.sample_timing(block.first_frame, 1024)?;
        let packet = AccessUnit::new(
            self.config.generation(),
            self.packets,
            vec![0x21, 0x01],
            pts,
            pts,
            duration,
        )?;
        self.packets += 1;
        Ok(vec![packet])
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        assert_eq!(self.owner, thread::current().id());
        *self.drop_thread.lock().unwrap() = Some(thread::current().id());
    }
}
fn factory(
    calls: Arc<AtomicUsize>,
    drop_thread: Arc<Mutex<Option<thread::ThreadId>>>,
    fail: bool,
) -> Factory {
    Box::new(move |config| {
        Ok(Box::new(Fake {
            config: config.clone(),
            header: header(config),
            assembler: pcm::Assembler::new(config.clone()),
            packets: 0,
            calls,
            drop_thread,
            owner: thread::current().id(),
            _not_send: Rc::new(()),
            fail,
        }))
    })
}
#[test]
fn one_inflight_unread_reply_stale_generation_and_actual_owner_retirement() {
    let config = config();
    let calls = Arc::new(AtomicUsize::new(0));
    let retired = Arc::new(Mutex::new(None));
    let mut worker = EncoderWorker::with_factory(
        config.clone(),
        factory(calls.clone(), retired.clone(), false),
    )
    .unwrap();
    let rejected = worker.submit(input(&config, 0, 1024)).unwrap_err();
    assert_eq!(rejected.error, Error::Busy);
    assert!(rejected.input.is_some());
    assert!(matches!(next(&mut worker), Event::Ready { .. }));
    let other = Config::new(
        Generation {
            encoder: 9,
            ..config.generation()
        },
        2,
        128_000,
    )
    .unwrap();
    assert_eq!(
        worker.submit(input(&other, 0, 1)).unwrap_err().error,
        Error::Generation
    );
    worker.submit(input(&config, 0, 1024)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        worker.submit(input(&config, 1024, 1)).unwrap_err().error,
        Error::Busy
    );
    assert_eq!(worker.finish(), Err(Error::Busy));
    let Event::Packets(batch) = next(&mut worker) else {
        panic!("packets")
    };
    assert_eq!(batch.stats.input_frames, 1024);
    assert_eq!(batch.packets.len(), 1);
    assert_eq!(batch.packets[0].pts(), 0);
    assert_eq!(
        worker.submit(input(&config, 0, 1)).unwrap_err().error,
        Error::Invalid("noncontiguous PCM frame index")
    );
    worker.submit(input(&config, 1024, 1)).unwrap();
    assert!(matches!(next(&mut worker), Event::Packets(_)));
    worker.finish().unwrap();
    let Event::Packets(batch) = next(&mut worker) else {
        panic!("drain")
    };
    assert!(batch.drained);
    assert_eq!(batch.stats.explicit_padding_frames, 1023);
    assert_eq!(batch.stats.input_frames, 1025);
    assert_eq!(batch.stats.submitted_frames, 2048);
    worker.request_close();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !worker.is_closed() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(next(&mut worker), Event::Closed { .. }));
    assert_ne!(retired.lock().unwrap().unwrap(), thread::current().id());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
#[test]
fn unread_ready_close_and_terminal_native_failure_are_bounded() {
    let config = config();
    let retired = Arc::new(Mutex::new(None));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut worker = EncoderWorker::with_factory(
        config.clone(),
        factory(calls.clone(), retired.clone(), false),
    )
    .unwrap();
    worker.request_close();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !worker.is_closed() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(retired.lock().unwrap().is_some());
    assert_eq!(
        worker.submit(input(&config, 0, 1)).unwrap_err().error,
        Error::Closed
    );
    let _ = worker.poll();
    let mut failed = EncoderWorker::with_factory(
        config.clone(),
        factory(calls, Arc::new(Mutex::new(None)), true),
    )
    .unwrap();
    assert!(matches!(next(&mut failed), Event::Ready { .. }));
    failed.submit(input(&config, 0, 1)).unwrap();
    let Event::Failed { error, .. } = next(&mut failed) else {
        panic!("failure")
    };
    assert!(
        matches!(error,Error::Native{operation:"synthetic failure",hresult:-2147467259,ref detail}if detail=="exact fixture detail")
    );
    assert_eq!(
        failed.submit(input(&config, 1, 1)).unwrap_err().error,
        Error::Closed
    );
}

#[test]
fn excess_packet_count_bytes_and_foreign_result_generation_are_terminal_not_truncated() {
    struct Flood {
        config: Config,
        header: CodecConfiguration,
        mode: u8,
    }
    impl Adapter for Flood {
        fn configuration(&self) -> &CodecConfiguration {
            &self.header
        }
        fn stats(&self) -> StreamStats {
            StreamStats::default()
        }
        fn encode(&mut self, _: PcmInput) -> Result<Vec<AccessUnit>, Error> {
            let count = if self.mode == 0 { 5 } else { 4 };
            (0..count)
                .map(|index| {
                    AccessUnit::new(
                        if self.mode == 2 {
                            Generation {
                                encoder: 999,
                                ..self.config.generation()
                            }
                        } else {
                            self.config.generation()
                        },
                        index,
                        vec![0x21; if self.mode == 1 { MAX_AU_BYTES } else { 2 }],
                        index as i64 * 213333,
                        index as i64 * 213333,
                        213333,
                    )
                })
                .collect()
        }
        fn drain(&mut self) -> Result<Vec<AccessUnit>, Error> {
            Ok(vec![])
        }
    }
    for mode in 0..3 {
        let config = config();
        let mut worker = EncoderWorker::with_factory(
            config.clone(),
            Box::new(move |config| {
                Ok(Box::new(Flood {
                    config: config.clone(),
                    header: header(config),
                    mode,
                }))
            }),
        )
        .unwrap();
        assert!(matches!(next(&mut worker), Event::Ready { .. }));
        worker.submit(input(&config, 0, 1024)).unwrap();
        let Event::Failed { error, .. } = next(&mut worker) else {
            panic!("overflow/foreign output must fail")
        };
        if mode == 2 {
            assert_eq!(error, Error::Generation);
        } else {
            assert!(matches!(error, Error::Limit(_)));
        }
        assert_eq!(
            worker.submit(input(&config, 1024, 1)).unwrap_err().error,
            Error::Closed
        );
    }
}

#[test]
fn native_duration_is_preserved_verbatim_and_order_overflow_and_accepted_counts_remain_strict() {
    let config = config();
    // Actual Microsoft mono output: PTS426666,duration213335 precedes
    // PTS640000. Preserve that SDK observation, do not clamp/epsilon-normalize
    // it to our separately exact1024-frame input sample grid.
    let packet = AccessUnit::new(
        config.generation(),
        2,
        vec![0x21, 0x01],
        426666,
        426666,
        213335,
    )
    .unwrap();
    assert_eq!(packet.duration(), 213335);
    assert_eq!(packet.pts(), 426666);
    assert_eq!(config.sample_timing(2048, 1024).unwrap(), (426666, 213334));
    validate_native_timing(packet.pts(), packet.dts(), packet.duration(), Some(213333)).unwrap();
    validate_native_timing(640000, 640000, 213333, Some(packet.pts())).unwrap();
    for (pts, dts, duration, previous) in [
        (0, 0, 0, None),
        (0, 0, -1, None),
        (i64::MAX, i64::MAX, 1, None),
        (10, 10, 1, Some(10)),
        (9, 9, 1, Some(10)),
        (10, 9, 1, None),
    ] {
        assert!(validate_native_timing(pts, dts, duration, previous).is_err());
    }
    assert_eq!(next_packet_count(4, 5, false).unwrap(), 5);
    assert_eq!(next_packet_count(5, 5, true).unwrap(), 6);
    assert_eq!(next_packet_count(6, 5, true).unwrap(), 7);
    for (packets, accepted, eos) in [
        (0, 0, false),
        (5, 5, false),
        (7, 5, true),
        (u64::MAX, u64::MAX, false),
        (0, u64::MAX, true),
    ] {
        assert!(next_packet_count(packets, accepted, eos).is_err());
    }
    // Foreign-generation output rejection remains covered by the injected
    // packet-batch oracle; no native duration observation changes identity.
}
