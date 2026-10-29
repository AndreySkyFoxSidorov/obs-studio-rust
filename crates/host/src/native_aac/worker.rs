use super::{
    AccessUnit, Adapter, CodecConfiguration, Config, Error, Factory, Generation, MAX_BATCH_BYTES,
    MAX_PACKETS, PcmInput,
};
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
};

/// Process-wide native capacity is released only after actual owner teardown.
struct AdmissionCounter {
    active: AtomicUsize,
}
impl AdmissionCounter {
    fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
        }
    }
    fn acquire(self: &Arc<Self>) -> Result<NativePermit, Error> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                if active < 2 {
                    active.checked_add(1)
                } else {
                    None
                }
            })
            .map_err(|_| Error::Limit("two asynchronous AAC workers are active or retiring"))?;
        Ok(NativePermit {
            admission: self.clone(),
        })
    }
}
struct NativePermit {
    admission: Arc<AdmissionCounter>,
}
impl Drop for NativePermit {
    fn drop(&mut self) {
        self.admission.active.fetch_sub(1, Ordering::AcqRel);
    }
}
fn native_admission() -> Arc<AdmissionCounter> {
    static ADMISSION: OnceLock<Arc<AdmissionCounter>> = OnceLock::new();
    ADMISSION
        .get_or_init(|| Arc::new(AdmissionCounter::new()))
        .clone()
}

#[derive(Debug)]
pub struct PacketBatch {
    pub generation: Generation,
    pub configuration: CodecConfiguration,
    pub packets: Vec<AccessUnit>,
    pub drained: bool,
    pub stats: super::StreamStats,
}
impl PacketBatch {
    fn checked(
        config: &Config,
        adapter: &dyn Adapter,
        packets: Vec<AccessUnit>,
        drained: bool,
    ) -> Result<Self, Error> {
        if packets.len() > MAX_PACKETS {
            return Err(Error::Limit("encoded packet count"));
        }
        let configuration = adapter.configuration();
        let mut bytes = configuration.retained_bytes()
            + std::mem::size_of::<Self>()
            + packets.len() * std::mem::size_of::<AccessUnit>();
        for packet in &packets {
            if packet.generation() != config.generation() {
                return Err(Error::Generation);
            }
            bytes = bytes
                .checked_add(packet.bytes().len())
                .ok_or(Error::Limit("packet batch bytes"))?;
        }
        if bytes > MAX_BATCH_BYTES {
            return Err(Error::Limit("packet batch bytes"));
        }
        Ok(Self {
            generation: config.generation(),
            configuration: configuration.clone(),
            packets: packets.into_boxed_slice().into_vec(),
            drained,
            stats: adapter.stats(),
        })
    }
}
#[derive(Debug)]
pub enum Event {
    Ready {
        generation: Generation,
        configuration: CodecConfiguration,
    },
    Packets(PacketBatch),
    Failed {
        generation: Generation,
        error: Error,
    },
    Closed {
        generation: Generation,
    },
}
enum Command {
    Encode(PcmInput),
    Drain,
    Close,
}
#[derive(Debug)]
pub struct RejectedInput {
    pub error: Error,
    pub input: Option<PcmInput>,
}

/// Native AAC state is constructed and destroyed on the worker. One admitted command
/// includes its unread reply; polling never calls COM or joins a live SDK call.
pub struct EncoderWorker {
    config: Config,
    commands: SyncSender<Command>,
    replies: Receiver<Event>,
    stop: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    busy: bool,
    draining: bool,
    reported_closed: bool,
    next_frame: u64,
    _thread: thread::JoinHandle<()>,
}
impl EncoderWorker {
    pub fn native(config: Config) -> Result<Self, Error> {
        Self::with_factory_and_admission(config, super::native_factory(), native_admission())
    }
    #[cfg(test)]
    pub(crate) fn with_factory(config: Config, factory: Factory) -> Result<Self, Error> {
        // Isolated test pool: unrelated parallel Fake tests never consume the
        // process-global native slots. Capacity tests inject one shared pool.
        Self::with_factory_and_admission(config, factory, Arc::new(AdmissionCounter::new()))
    }
    fn with_factory_and_admission(
        config: Config,
        factory: Factory,
        admission: Arc<AdmissionCounter>,
    ) -> Result<Self, Error> {
        // Reserve before even spawning the owner. A blocked constructor counts
        // exactly like an active/retiring adapter. Spawn failure drops the
        // unstarted closure/captured permit without constructing native state.
        let permit = admission.acquire()?;
        let (commands, requests) = mpsc::sync_channel(1);
        let (results, replies) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let worker_config = config.clone();
        let worker_stop = stop.clone();
        let worker_closed = closed.clone();
        let thread = thread::Builder::new()
            .name("native-aac-packets".into())
            .spawn(move || {
                let _closed = ClosedSignal(worker_closed);
                // Rebind AFTER ClosedSignal: reverse local drop order ensures
                // run/native destruction → permit release → Closed publication.
                let _permit = permit;
                run(worker_config, factory, requests, results, worker_stop);
            })
            .map_err(|error| Error::Worker(error.to_string()))?;
        Ok(Self {
            config,
            commands,
            replies,
            stop,
            closed,
            busy: true,
            draining: false,
            reported_closed: false,
            next_frame: 0,
            _thread: thread,
        })
    }
    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn is_busy(&self) -> bool {
        self.busy
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    /// Rejected input is returned to its owner; the port never silently drops it.
    pub fn submit(&mut self, input: PcmInput) -> Result<(), RejectedInput> {
        let check = if self.stop.load(Ordering::Acquire) || self.is_closed() {
            Err(Error::Closed)
        } else if self.busy {
            Err(Error::Busy)
        } else if self.draining {
            Err(Error::Protocol("encoding after EOS"))
        } else {
            input.validate(&self.config)
        };
        if let Err(error) = check {
            return Err(RejectedInput {
                error,
                input: Some(input),
            });
        }
        if input.first_frame() != self.next_frame {
            return Err(RejectedInput {
                error: Error::Invalid("noncontiguous PCM frame index"),
                input: Some(input),
            });
        }
        let next_frame = match input.first_frame().checked_add(input.frames()) {
            Some(value) => value,
            None => {
                return Err(RejectedInput {
                    error: Error::Invalid("PCM position overflow"),
                    input: Some(input),
                });
            }
        };
        match self.commands.try_send(Command::Encode(input)) {
            Ok(()) => {
                self.busy = true;
                self.next_frame = next_frame;
                Ok(())
            }
            Err(TrySendError::Full(Command::Encode(input))) => Err(RejectedInput {
                error: Error::Busy,
                input: Some(input),
            }),
            Err(TrySendError::Disconnected(Command::Encode(input))) => Err(RejectedInput {
                error: Error::Closed,
                input: Some(input),
            }),
            // try_send returns the exact command passed above; this arm is kept
            // total without using a panic even if that invariant changes later.
            Err(_) => Err(RejectedInput {
                error: Error::Protocol("unexpected command rejection"),
                input: None,
            }),
        }
    }
    pub fn finish(&mut self) -> Result<(), Error> {
        if self.stop.load(Ordering::Acquire) || self.is_closed() {
            return Err(Error::Closed);
        }
        if self.busy {
            return Err(Error::Busy);
        }
        if self.draining {
            return Err(Error::Protocol("EOS already requested"));
        }
        self.commands
            .try_send(Command::Drain)
            .map_err(|error| match error {
                TrySendError::Full(_) => Error::Busy,
                TrySendError::Disconnected(_) => Error::Closed,
            })?;
        self.draining = true;
        self.busy = true;
        Ok(())
    }
    pub fn poll(&mut self) -> Option<Event> {
        match self.replies.try_recv() {
            Ok(event) => {
                if matches!(&event, Event::Failed { .. }) {
                    self.stop.store(true, Ordering::Release);
                }
                self.busy = false;
                Some(event)
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                if self.is_closed() && !self.reported_closed {
                    self.reported_closed = true;
                    self.busy = false;
                    Some(Event::Closed {
                        generation: self.config.generation(),
                    })
                } else {
                    None
                }
            }
        }
    }
    /// Does not claim a blocked native call has stopped. Observe is_closed/poll.
    pub fn request_close(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake an idle owner without a periodic timer. If an Encode is queued,
        // it already wakes the receiver and the stop flag wins before the SDK.
        let _ = self.commands.try_send(Command::Close);
    }
}
impl Drop for EncoderWorker {
    fn drop(&mut self) {
        self.request_close();
    }
}
struct ClosedSignal(Arc<AtomicBool>);
impl Drop for ClosedSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn run(
    config: Config,
    factory: Factory,
    requests: Receiver<Command>,
    results: SyncSender<Event>,
    stop: Arc<AtomicBool>,
) {
    let mut adapter = match factory(&config) {
        Ok(adapter) => adapter,
        Err(error) => {
            let _ = results.send(Event::Failed {
                generation: config.generation(),
                error,
            });
            return;
        }
    };
    if results
        .send(Event::Ready {
            generation: config.generation(),
            configuration: adapter.configuration().clone(),
        })
        .is_err()
    {
        return;
    }
    while !stop.load(Ordering::Acquire) {
        let command = match requests.recv() {
            Ok(command) => command,
            Err(_) => break,
        };
        if stop.load(Ordering::Acquire) {
            break;
        }
        let (packets, drained) = match command {
            Command::Encode(input) => (adapter.encode(input), false),
            Command::Drain => (adapter.drain(), true),
            Command::Close => break,
        };
        let batch = packets
            .and_then(|packets| PacketBatch::checked(&config, adapter.as_ref(), packets, drained));
        let event = match batch {
            Ok(batch) => Event::Packets(batch),
            Err(error) => {
                let _ = results.send(Event::Failed {
                    generation: config.generation(),
                    error,
                });
                break;
            }
        };
        if results.send(event).is_err() {
            break;
        }
    }
    // adapter (and its native context) drops here on its constructing thread.
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use std::{
        rc::Rc,
        sync::{Condvar, Mutex},
        time::{Duration, Instant},
    };
    #[derive(Default)]
    struct Gate {
        state: Mutex<(usize, bool)>,
        signal: Condvar,
    }
    impl Gate {
        fn enter(&self) {
            let mut state = self.state.lock().unwrap();
            state.0 += 1;
            self.signal.notify_all();
            while !state.1 {
                state = self.signal.wait(state).unwrap();
            }
        }
        fn wait(&self, count: usize) {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut state = self.state.lock().unwrap();
            while state.0 < count {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "Fake constructor/cleanup must reach gate"
                );
                state = self.signal.wait_timeout(state, remaining).unwrap().0;
            }
        }
        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.1 = true;
            self.signal.notify_all();
        }
    }
    struct Release(Arc<Gate>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release();
        }
    }
    fn config() -> Config {
        Config::new(
            Generation {
                session: 10,
                output: 11,
                encoder: 12,
            },
            2,
            128_000,
        )
        .unwrap()
    }
    fn next(worker: &mut EncoderWorker) -> Event {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(event) = worker.poll() {
                return event;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn closed(worker: &EncoderWorker) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !worker.is_closed() {
            assert!(Instant::now() < deadline, "actual owner retirement");
            thread::sleep(Duration::from_millis(1));
        }
    }
    struct Fake {
        header: CodecConfiguration,
        cleanup: Arc<Gate>,
        fail: bool,
        _not_send: Rc<()>,
    }
    impl Adapter for Fake {
        fn configuration(&self) -> &CodecConfiguration {
            &self.header
        }
        fn stats(&self) -> super::super::StreamStats {
            super::super::StreamStats::default()
        }
        fn encode(&mut self, _: PcmInput) -> Result<Vec<AccessUnit>, Error> {
            if self.fail {
                Err(Error::Protocol("synthetic encoder failure"))
            } else {
                Ok(vec![])
            }
        }
        fn drain(&mut self) -> Result<Vec<AccessUnit>, Error> {
            Ok(vec![])
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.cleanup.enter();
        }
    }
    fn factory(constructor: Option<Arc<Gate>>, cleanup: Arc<Gate>, fail: bool) -> Factory {
        Box::new(move |config| {
            if let Some(gate) = constructor {
                gate.enter();
            }
            Ok(Box::new(Fake {
                header: CodecConfiguration::from_asc(&[0x11, 0x90], config)?,
                cleanup,
                fail,
                _not_send: Rc::new(()),
            }))
        })
    }
    fn rejected(pool: Arc<AdmissionCounter>, cleanup: Arc<Gate>) {
        let result = EncoderWorker::with_factory_and_admission(
            config(),
            factory(None, cleanup, false),
            pool,
        );
        assert!(matches!(
            result,
            Err(Error::Limit(
                "two native AAC encoders are active or retiring"
            ))
        ));
    }
    #[test]
    fn blocked_constructors_and_requested_close_hold_two_slots_through_actual_cleanup() {
        let pool = Arc::new(AdmissionCounter::new());
        let constructor = Arc::new(Gate::default());
        let cleanup = Arc::new(Gate::default());
        let _release_constructor = Release(constructor.clone());
        let _release_cleanup = Release(cleanup.clone());
        let mut first = EncoderWorker::with_factory_and_admission(
            config(),
            factory(Some(constructor.clone()), cleanup.clone(), false),
            pool.clone(),
        )
        .unwrap();
        let mut second = EncoderWorker::with_factory_and_admission(
            config(),
            factory(Some(constructor.clone()), cleanup.clone(), false),
            pool.clone(),
        )
        .unwrap();
        constructor.wait(2);
        assert_eq!(pool.active.load(Ordering::Acquire), 2);
        rejected(pool.clone(), cleanup.clone());
        first.request_close();
        second.request_close();
        assert!(!first.is_closed());
        assert!(!second.is_closed());
        rejected(pool.clone(), cleanup.clone());
        constructor.release();
        cleanup.wait(2);
        assert_eq!(pool.active.load(Ordering::Acquire), 2);
        assert!(!first.is_closed());
        assert!(!second.is_closed());
        rejected(pool.clone(), cleanup.clone());
        cleanup.release();
        closed(&first);
        closed(&second);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
        let mut replacement = EncoderWorker::with_factory_and_admission(
            config(),
            factory(None, cleanup, false),
            pool.clone(),
        )
        .unwrap();
        assert!(matches!(next(&mut replacement), Event::Ready { .. }));
        replacement.request_close();
        closed(&replacement);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
    }
    #[test]
    fn unread_failure_does_not_free_a_native_slot_while_destructor_is_stuck() {
        let pool = Arc::new(AdmissionCounter::new());
        let cleanup = Arc::new(Gate::default());
        let _release = Release(cleanup.clone());
        let mut first = EncoderWorker::with_factory_and_admission(
            config(),
            factory(None, cleanup.clone(), true),
            pool.clone(),
        )
        .unwrap();
        let mut second = EncoderWorker::with_factory_and_admission(
            config(),
            factory(None, cleanup.clone(), true),
            pool.clone(),
        )
        .unwrap();
        for worker in [&mut first, &mut second] {
            assert!(matches!(next(worker), Event::Ready { .. }));
            worker
                .submit(PcmInput::new(&config(), 0, vec![0; 2].into_boxed_slice()).unwrap())
                .unwrap();
        }
        cleanup.wait(2); // Failed replies deliberately remain unread here.
        assert_eq!(pool.active.load(Ordering::Acquire), 2);
        assert!(!first.is_closed());
        assert!(!second.is_closed());
        rejected(pool.clone(), cleanup.clone());
        first.request_close();
        second.request_close();
        assert_eq!(pool.active.load(Ordering::Acquire), 2);
        cleanup.release();
        closed(&first);
        closed(&second);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
        for worker in [&mut first, &mut second] {
            assert!(matches!(
                next(worker),
                Event::Failed {
                    error: Error::Protocol("synthetic encoder failure"),
                    ..
                }
            ));
            assert!(matches!(next(worker), Event::Closed { .. }));
        }
    }
    struct ConstructorCleanup(Arc<Gate>);
    impl Drop for ConstructorCleanup {
        fn drop(&mut self) {
            self.0.enter();
        }
    }
    #[test]
    fn native_constructor_error_releases_its_permit_only_after_local_context_cleanup() {
        let pool = Arc::new(AdmissionCounter::new());
        let failed_cleanup = Arc::new(Gate::default());
        let _release = Release(failed_cleanup.clone());
        let gate = failed_cleanup.clone();
        let mut failed = EncoderWorker::with_factory_and_admission(
            config(),
            Box::new(move |_| {
                let _native_context = ConstructorCleanup(gate);
                Err(Error::Unavailable("synthetic constructor error"))
            }),
            pool.clone(),
        )
        .unwrap();
        failed_cleanup.wait(1);
        let ordinary_cleanup = Arc::new(Gate::default());
        ordinary_cleanup.release();
        let mut ordinary = EncoderWorker::with_factory_and_admission(
            config(),
            factory(None, ordinary_cleanup.clone(), false),
            pool.clone(),
        )
        .unwrap();
        assert!(matches!(next(&mut ordinary), Event::Ready { .. }));
        assert!(failed.poll().is_none());
        rejected(pool.clone(), ordinary_cleanup.clone());
        failed.request_close();
        assert!(!failed.is_closed());
        failed_cleanup.release();
        closed(&failed);
        assert_eq!(pool.active.load(Ordering::Acquire), 1);
        assert!(matches!(
            next(&mut failed),
            Event::Failed {
                error: Error::Unavailable("synthetic constructor error"),
                ..
            }
        ));
        let mut replacement = EncoderWorker::with_factory_and_admission(
            config(),
            factory(None, ordinary_cleanup, false),
            pool.clone(),
        )
        .unwrap();
        assert!(matches!(next(&mut replacement), Event::Ready { .. }));
        ordinary.request_close();
        replacement.request_close();
        closed(&ordinary);
        closed(&replacement);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
    }
    #[test]
    fn abandoned_unstarted_spawn_closure_returns_reserved_capacity_without_factory_execution() {
        let pool = Arc::new(AdmissionCounter::new());
        let permit = pool.acquire().unwrap();
        assert_eq!(pool.active.load(Ordering::Acquire), 1);
        // std::thread::Builder drops its owned closure when spawn fails. This
        // fixed pure fixture verifies our capture owns the permit, not a caller
        // counter that might be released before construction/after a failed spawn.
        let unstarted_job = move || {
            let _permit = permit;
        };
        drop(unstarted_job);
        assert_eq!(pool.active.load(Ordering::Acquire), 0);
    }
}
