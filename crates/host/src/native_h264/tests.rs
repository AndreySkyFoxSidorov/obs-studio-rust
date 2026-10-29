use super::*;
use std::{
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
        64,
        48,
        30,
        1,
        800_000,
        30,
    )
    .unwrap()
}
fn header() -> CodecConfiguration {
    CodecConfiguration::parse(&[
        1, 66, 192, 20, 255, 225, 0, 24, 103, 66, 192, 20, 149, 161, 30, 192, 68, 0, 0, 3, 0, 4, 0,
        0, 3, 0, 240, 54, 130, 33, 26, 128, 1, 0, 4, 104, 206, 56, 128,
    ])
    .unwrap()
}
fn input(config: &Config, index: u64) -> Nv12Input {
    let (pts, duration) = config.frame_timing(index).unwrap();
    Nv12Input::new(
        config,
        pts,
        duration,
        vec![128; config.nv12_bytes()].into_boxed_slice(),
    )
    .unwrap()
}
fn packet(config: &Config, input: &Nv12Input) -> AccessUnit {
    AccessUnit::new(
        config.generation(),
        vec![0, 0, 0, 2, 0x65, 0x80],
        input.pts(),
        input.pts(),
        input.duration(),
    )
    .unwrap()
}
fn next(worker: &mut EncoderWorker) -> Event {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(event) = worker.poll() {
            return event;
        }
        assert!(Instant::now() < deadline, "bounded worker reply");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn validated_nv12_config_and_rational_native_timestamps() {
    let cfg = config();
    assert_eq!(cfg.nv12_bytes(), 4608);
    for (w, h) in [(0, 48), (63, 48), (64, 47), (4096, 4096)] {
        assert!(Config::new(cfg.generation(), w, h, 30, 1, 800_000, 30).is_err());
    }
    assert!(Config::new(cfg.generation(), 64, 48, 121, 1, 800_000, 30).is_err());
    assert!(Config::new(cfg.generation(), 64, 48, 30, 0, 800_000, 30).is_err());
    assert!(Config::new(cfg.generation(), 64, 48, 30, 1, 1, 30).is_err());
    assert!(
        Config::new(
            Generation {
                session: 0,
                ..cfg.generation()
            },
            64,
            48,
            30,
            1,
            800_000,
            30
        )
        .is_err()
    );
    assert!(Nv12Input::new(&cfg, 0, 1, vec![0; 4607].into_boxed_slice()).is_err());
    assert!(Nv12Input::new(&cfg, -1, 1, vec![0; 4608].into_boxed_slice()).is_err());
    assert!(Nv12Input::new(&cfg, i64::MAX, 1, vec![0; 4608].into_boxed_slice()).is_err());
    let fractional = Config::new(cfg.generation(), 64, 48, 30_000, 1001, 800_000, 30).unwrap();
    let mut sum = 0;
    for index in 0..300 {
        let (pts, duration) = fractional.frame_timing(index).unwrap();
        assert_eq!(pts, sum);
        sum += duration;
    }
    assert_eq!(sum, 100_100_000);
    assert!(fractional.frame_timing(u64::MAX).is_err());
}
#[test]
fn bounded_sps_pps_and_annexb_to_avcc_preserve_payloads() {
    let cfg = header();
    let roundtrip = CodecConfiguration::parse(cfg.avcc()).unwrap();
    assert_eq!(cfg.avcc(), roundtrip.avcc());
    assert_eq!(cfg.sps(), roundtrip.sps());
    assert_eq!(cfg.pps(), roundtrip.pps());
    assert!(cfg.color_metadata_rewritten());
    assert!(!roundtrip.color_metadata_rewritten());
    assert_eq!(roundtrip.input_avcc(), cfg.avcc());
    assert_ne!(cfg.input_avcc(), roundtrip.input_avcc());
    assert!(cfg.retained_bytes() <= MAX_HEADER_BYTES);
    assert_eq!(cfg.avcc()[4] & 3, 3);
    let raw = [0, 0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x65, 0x88, 0x84];
    let normalized = bitstream::normalize_access_unit(&raw).unwrap();
    assert_eq!(
        normalized,
        [0, 0, 0, 2, 0x09, 0xf0, 0, 0, 0, 3, 0x65, 0x88, 0x84]
    );
    assert!(bitstream::validate_access_unit(&normalized).unwrap());
    assert!(!bitstream::validate_access_unit(&[0, 0, 0, 2, 0x41, 0x80]).unwrap());
    for invalid in [
        &[][..],
        &[0, 0, 1][..],
        &[0, 0, 0, 1, 0x80][..],
        &[0, 0, 0, 3, 0x65][..],
        &[0, 0, 0, 1, 9][..],
    ] {
        assert!(bitstream::normalize_access_unit(invalid).is_err());
    }
    assert!(CodecConfiguration::parse(&cfg.avcc()[..cfg.avcc().len() - 1]).is_err());
    let mut wrong = cfg.avcc().to_vec();
    wrong[4] = 0xfc;
    assert!(CodecConfiguration::parse(&wrong).is_err());
    let mut oversized = vec![0; MAX_HEADER_BYTES + 1];
    oversized[0] = 1;
    assert!(matches!(
        CodecConfiguration::parse(&oversized),
        Err(Error::Limit(_))
    ));
    let mut many = Vec::new();
    for _ in 0..129 {
        many.extend_from_slice(&[0, 0, 1, 0x41, 0x80]);
    }
    assert!(matches!(
        bitstream::normalize_access_unit(&many),
        Err(Error::Limit(_))
    ));
    assert!(AccessUnit::new(config().generation(), vec![0, 0, 0, 2, 0x65, 0x80], 1, 0, 1).is_err());
}

#[test]
fn header_provenance_and_in_band_sps_are_sticky_without_changing_coded_slices() {
    let original = header();
    let mut configuration = CodecConfiguration::parse(original.avcc()).unwrap();
    assert!(!configuration.color_metadata_rewritten());
    let input = original.input_avcc();
    let size = u16::from_be_bytes([input[6], input[7]]) as usize;
    let source_sps = &input[8..8 + size];
    let slice = [0x65, 0x88, 0x84];
    let mut sample = Vec::new();
    for nal in [source_sps, &slice[..]] {
        sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        sample.extend_from_slice(nal);
    }
    let output = configuration.rewrite_sample(&sample).unwrap();
    assert!(configuration.color_metadata_rewritten());
    let length = u32::from_be_bytes(output[..4].try_into().unwrap()) as usize;
    assert_eq!(&output[4..4 + length], original.sps()[0].as_ref());
    assert_eq!(
        &output[4 + length..],
        &sample[4 + source_sps.len()..],
        "coded slice framing and bytes unchanged"
    );
    let mut next = CodecConfiguration::parse(original.avcc()).unwrap();
    next.inherit_color_provenance(&configuration);
    assert!(next.color_metadata_rewritten());
    let clean_sample = [0, 0, 0, 3, 0x65, 0x88, 0x84];
    assert_eq!(next.rewrite_sample(&clean_sample).unwrap(), clean_sample);
    assert!(next.color_metadata_rewritten());
    assert!(matches!(
        CodecConfiguration::parse(&[0, 0, 0, 1, 0x67, 66, 0, 30, 0x80, 0, 0, 1, 0x68, 0xce, 0x80]),
        Err(Error::Color(_))
    ));
}

#[test]
fn retained_header_budget_counts_the_original_normalized_avcc_too() {
    let configuration = header();
    let input = configuration.input_avcc();
    let sps_size = u16::from_be_bytes([input[6], input[7]]) as usize;
    let pps_count_at = 8 + sps_size;
    let mut large = input[..pps_count_at + 1].to_vec();
    let mut pps = vec![0; 15_000];
    pps[..4].copy_from_slice(&input[pps_count_at + 3..pps_count_at + 7]);
    large.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    large.extend_from_slice(&pps);
    assert!(large.len() < MAX_HEADER_BYTES);
    assert!(matches!(
        CodecConfiguration::parse(&large),
        Err(Error::Limit("retained H264 input/output headers"))
    ));
}
struct Fake {
    config: Config,
    header: CodecConfiguration,
    count: Arc<AtomicUsize>,
    drop_thread: Arc<Mutex<Option<thread::ThreadId>>>,
    oversized: bool,
    error: bool,
}
impl Adapter for Fake {
    fn configuration(&self) -> Option<&CodecConfiguration> {
        Some(&self.header)
    }
    fn encode(&mut self, input: Nv12Input) -> Result<Vec<AccessUnit>, Error> {
        self.count.fetch_add(1, Ordering::Release);
        if self.error {
            return Err(Error::Native {
                operation: "synthetic codec failure",
                hresult: -123,
                detail: "exact original native detail".into(),
            });
        }
        let count = if self.oversized { 5 } else { 1 };
        Ok((0..count).map(|_| packet(&self.config, &input)).collect())
    }
    fn drain(&mut self) -> Result<Vec<AccessUnit>, Error> {
        Ok(Vec::new())
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        *self.drop_thread.lock().unwrap() = Some(thread::current().id());
    }
}
fn worker(
    oversized: bool,
    error: bool,
) -> (
    EncoderWorker,
    Arc<AtomicUsize>,
    Arc<Mutex<Option<thread::ThreadId>>>,
) {
    let count = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(Mutex::new(None));
    let calls = count.clone();
    let destructor = drops.clone();
    let factory: Factory = Box::new(move |config| {
        Ok(Box::new(Fake {
            config: config.clone(),
            header: header(),
            count: calls,
            drop_thread: destructor,
            oversized,
            error,
        }))
    });
    (
        EncoderWorker::with_factory(config(), factory).unwrap(),
        count,
        drops,
    )
}
#[test]
fn one_admitted_command_includes_unread_reply_and_shutdown_drops_on_owner() {
    let (mut worker, calls, drops) = worker(false, false);
    assert!(worker.is_busy());
    assert!(matches!(next(&mut worker), Event::Ready { .. }));
    worker.submit(input(worker.config(), 0)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::Acquire) == 0 {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let rejected = worker.submit(input(worker.config(), 1)).unwrap_err();
    assert_eq!(rejected.error, Error::Busy);
    assert_eq!(rejected.input.unwrap().bytes().len(), 4608);
    match next(&mut worker) {
        Event::Packets(batch) => {
            assert_eq!(batch.packets.len(), 1);
            assert!(!batch.drained);
        }
        other => panic!("{other:?}"),
    }
    let backwards = worker.submit(input(worker.config(), 0)).unwrap_err();
    assert!(matches!(backwards.error, Error::Invalid(_)));
    let other = Config::new(
        Generation {
            encoder: 999,
            ..worker.config().generation()
        },
        64,
        48,
        30,
        1,
        800_000,
        30,
    )
    .unwrap();
    assert_eq!(
        worker.submit(input(&other, 1)).unwrap_err().error,
        Error::Generation
    );
    worker.finish().unwrap();
    assert_eq!(worker.finish().unwrap_err(), Error::Busy);
    assert!(matches!(
        next(&mut worker),
        Event::Packets(PacketBatch { drained: true, .. })
    ));
    assert!(matches!(
        worker.submit(input(worker.config(), 1)).unwrap_err().error,
        Error::Protocol(_)
    ));
    worker.request_close();
    assert!(matches!(next(&mut worker), Event::Closed { .. }));
    assert!(worker.is_closed());
    assert_ne!(drops.lock().unwrap().unwrap(), thread::current().id());
}
#[test]
fn packet_overflow_and_native_failure_are_terminal_without_truncation() {
    for overflow in [true, false] {
        let (mut worker, _, drops) = worker(overflow, !overflow);
        next(&mut worker);
        worker.submit(input(worker.config(), 0)).unwrap();
        match next(&mut worker) {
            Event::Failed {
                error: Error::Limit("encoded packet count"),
                ..
            } if overflow => {}
            Event::Failed {
                error:
                    Error::Native {
                        hresult: -123,
                        detail,
                        ..
                    },
                ..
            } if !overflow => assert_eq!(detail, "exact original native detail"),
            event => panic!("{event:?}"),
        }
        assert!(matches!(next(&mut worker), Event::Closed { .. }));
        assert!(drops.lock().unwrap().is_some());
    }
}

struct DeferredHeader {
    config: Config,
    header: Option<CodecConfiguration>,
    supply: bool,
}
impl Adapter for DeferredHeader {
    fn configuration(&self) -> Option<&CodecConfiguration> {
        self.header.as_ref()
    }
    fn encode(&mut self, input: Nv12Input) -> Result<Vec<AccessUnit>, Error> {
        if self.supply {
            self.header = Some(header());
        }
        Ok(vec![packet(&self.config, &input)])
    }
    fn drain(&mut self) -> Result<Vec<AccessUnit>, Error> {
        Ok(Vec::new())
    }
}
#[test]
fn ready_may_defer_headers_but_actual_packets_must_carry_their_configuration() {
    for supply in [false, true] {
        let factory: Factory = Box::new(move |config| {
            Ok(Box::new(DeferredHeader {
                config: config.clone(),
                header: None,
                supply,
            }))
        });
        let mut worker = EncoderWorker::with_factory(config(), factory).unwrap();
        assert!(matches!(
            next(&mut worker),
            Event::Ready {
                configuration: None,
                ..
            }
        ));
        worker.submit(input(worker.config(), 0)).unwrap();
        match next(&mut worker) {
            Event::Packets(batch) if supply => {
                assert!(batch.configuration.is_some());
                assert_eq!(batch.packets.len(), 1);
            }
            Event::Failed {
                error: Error::Protocol("encoded packets lack codec configuration"),
                ..
            } if !supply => {}
            event => panic!("{event:?}"),
        }
        worker.request_close();
        assert!(matches!(next(&mut worker), Event::Closed { .. }));
    }
}

#[test]
fn terminal_reply_refuses_new_input_while_native_retirement_is_still_in_progress() {
    use std::sync::atomic::AtomicBool;
    struct Retiring {
        header: CodecConfiguration,
        release: Arc<AtomicBool>,
    }
    impl Adapter for Retiring {
        fn configuration(&self) -> Option<&CodecConfiguration> {
            Some(&self.header)
        }
        fn encode(&mut self, _input: Nv12Input) -> Result<Vec<AccessUnit>, Error> {
            Err(Error::Protocol("codec failed before delayed cleanup"))
        }
        fn drain(&mut self) -> Result<Vec<AccessUnit>, Error> {
            Ok(Vec::new())
        }
    }
    impl Drop for Retiring {
        fn drop(&mut self) {
            while !self.release.load(Ordering::Acquire) {
                thread::yield_now();
            }
        }
    }
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let release = Arc::new(AtomicBool::new(false));
    let _release_on_failure = Release(release.clone());
    let worker_release = release.clone();
    let factory: Factory = Box::new(move |_| {
        Ok(Box::new(Retiring {
            header: header(),
            release: worker_release,
        }))
    });
    let mut worker = EncoderWorker::with_factory(config(), factory).unwrap();
    next(&mut worker);
    worker.submit(input(worker.config(), 0)).unwrap();
    assert!(matches!(next(&mut worker), Event::Failed { .. }));
    assert!(
        !worker.is_closed(),
        "cleanup has not acknowledged retirement"
    );
    assert_eq!(
        worker.submit(input(worker.config(), 1)).unwrap_err().error,
        Error::Closed
    );
    release.store(true, Ordering::Release);
    assert!(matches!(next(&mut worker), Event::Closed { .. }));
}

#[test]
fn global_style_two_owner_admission_counts_blocked_factories_and_actual_retirement() {
    use super::worker::AdmissionCounter;
    use std::sync::atomic::AtomicBool;
    struct Gate {
        opened: Arc<AtomicBool>,
    }
    impl Drop for Gate {
        fn drop(&mut self) {
            self.opened.store(true, Ordering::Release);
        }
    }
    struct BlockedCleanup {
        header: CodecConfiguration,
        release: Arc<AtomicBool>,
    }
    impl Adapter for BlockedCleanup {
        fn configuration(&self) -> Option<&CodecConfiguration> {
            Some(&self.header)
        }
        fn encode(&mut self, _input: Nv12Input) -> Result<Vec<AccessUnit>, Error> {
            Err(Error::Protocol("failure before blocked native cleanup"))
        }
        fn drain(&mut self) -> Result<Vec<AccessUnit>, Error> {
            Ok(Vec::new())
        }
    }
    impl Drop for BlockedCleanup {
        fn drop(&mut self) {
            while !self.release.load(Ordering::Acquire) {
                thread::yield_now();
            }
        }
    }
    let admission = Arc::new(AdmissionCounter::new());
    let init_open = Arc::new(AtomicBool::new(false));
    let cleanup_open = Arc::new(AtomicBool::new(false));
    let _release_init = Gate {
        opened: init_open.clone(),
    };
    let _release_cleanup = Gate {
        opened: cleanup_open.clone(),
    };
    let started = Arc::new(AtomicUsize::new(0));
    let factory = || -> Factory {
        let init_open = init_open.clone();
        let cleanup_open = cleanup_open.clone();
        let started = started.clone();
        Box::new(move |_| {
            started.fetch_add(1, Ordering::AcqRel);
            while !init_open.load(Ordering::Acquire) {
                thread::yield_now();
            }
            Ok(Box::new(BlockedCleanup {
                header: header(),
                release: cleanup_open,
            }))
        })
    };
    let mut first =
        EncoderWorker::with_factory_and_admission(config(), factory(), admission.clone()).unwrap();
    let mut second =
        EncoderWorker::with_factory_and_admission(config(), factory(), admission.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while started.load(Ordering::Acquire) != 2 {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert_eq!(admission.active(), 2);
    let rejected =
        EncoderWorker::with_factory_and_admission(config(), factory(), admission.clone())
            .err()
            .expect("third factory must not spawn");
    assert_eq!(
        rejected,
        Error::Limit("native H264 active/retiring owner limit")
    );
    assert_eq!(
        started.load(Ordering::Acquire),
        2,
        "rejected factory was not invoked"
    );
    init_open.store(true, Ordering::Release);
    assert!(matches!(next(&mut first), Event::Ready { .. }));
    assert!(matches!(next(&mut second), Event::Ready { .. }));
    first.submit(input(first.config(), 0)).unwrap();
    second.submit(input(second.config(), 0)).unwrap();
    assert!(matches!(next(&mut first), Event::Failed { .. }));
    assert!(matches!(next(&mut second), Event::Failed { .. }));
    first.request_close();
    second.request_close();
    assert!(!first.is_closed() && !second.is_closed());
    assert_eq!(
        admission.active(),
        2,
        "Failed/close request must not retire a native permit"
    );
    assert!(
        EncoderWorker::with_factory_and_admission(config(), factory(), admission.clone()).is_err()
    );
    cleanup_open.store(true, Ordering::Release);
    assert!(matches!(next(&mut first), Event::Closed { .. }));
    assert!(matches!(next(&mut second), Event::Closed { .. }));
    assert_eq!(
        admission.active(),
        0,
        "Closed acknowledgement follows lease release"
    );
    let mut replacement =
        EncoderWorker::with_factory_and_admission(config(), factory(), admission.clone()).unwrap();
    assert!(matches!(next(&mut replacement), Event::Ready { .. }));
    replacement.request_close();
    assert!(matches!(next(&mut replacement), Event::Closed { .. }));
    assert_eq!(admission.active(), 0);
}

#[test]
fn failed_factory_keeps_admission_until_its_own_cleanup_actually_finishes() {
    use super::worker::AdmissionCounter;
    use std::sync::atomic::AtomicBool;
    let admission = Arc::new(AdmissionCounter::new());
    let release = Arc::new(AtomicBool::new(false));
    struct Cleanup(Arc<AtomicBool>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            while !self.0.load(Ordering::Acquire) {
                thread::yield_now();
            }
        }
    }
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let _release = Release(release.clone());
    let started = Arc::new(AtomicUsize::new(0));
    let factory = || -> Factory {
        let release = release.clone();
        let started = started.clone();
        Box::new(move |_| {
            let _cleanup = Cleanup(release);
            started.fetch_add(1, Ordering::AcqRel);
            Err(Error::Unavailable("synthetic constructor failure"))
        })
    };
    let mut first =
        EncoderWorker::with_factory_and_admission(config(), factory(), admission.clone()).unwrap();
    let mut second =
        EncoderWorker::with_factory_and_admission(config(), factory(), admission.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while started.load(Ordering::Acquire) != 2 {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert_eq!(admission.active(), 2);
    first.request_close();
    second.request_close();
    assert!(
        EncoderWorker::with_factory_and_admission(config(), factory(), admission.clone()).is_err()
    );
    release.store(true, Ordering::Release);
    assert!(matches!(next(&mut first), Event::Failed { .. }));
    assert!(matches!(next(&mut second), Event::Failed { .. }));
    assert!(matches!(next(&mut first), Event::Closed { .. }));
    assert!(matches!(next(&mut second), Event::Closed { .. }));
    assert_eq!(admission.active(), 0);
}
