use super::{
    AccessUnit, Adapter, CodecConfiguration, Config, Error, Factory, Generation, MAX_BATCH_BYTES,
    MAX_PACKETS, Nv12Input,
};
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::Duration,
};

#[derive(Debug)]
pub struct PacketBatch {
    pub generation: Generation,
    pub configuration: Option<CodecConfiguration>,
    pub packets: Vec<AccessUnit>,
    pub drained: bool,
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
        if !packets.is_empty() && configuration.is_none() {
            return Err(Error::Protocol("encoded packets lack codec configuration"));
        }
        let mut bytes = configuration.map_or(0, CodecConfiguration::retained_bytes)
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
            configuration: configuration.cloned(),
            packets: packets.into_boxed_slice().into_vec(),
            drained,
        })
    }
}
#[derive(Debug)]
pub enum Event {
    Ready {
        generation: Generation,
        configuration: Option<CodecConfiguration>,
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
    Encode(Nv12Input),
    Drain,
}
#[derive(Debug)]
pub struct RejectedInput {
    pub error: Error,
    pub input: Option<Nv12Input>,
}

const NATIVE_OWNER_LIMIT: usize = 2;
static NATIVE_ADMISSION: OnceLock<Arc<AdmissionCounter>> = OnceLock::new();
/// Each codec's isolated staging module owns its own admission pool. Integration
/// must make any stricter combined H264/AAC process-wide policy explicit.
#[derive(Default)]
pub(crate) struct AdmissionCounter {
    active: AtomicUsize,
}
impl AdmissionCounter {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    #[cfg(test)]
    pub(crate) fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
    fn acquire(self: &Arc<Self>) -> Result<NativePermit, Error> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                if count < NATIVE_OWNER_LIMIT {
                    Some(count + 1)
                } else {
                    None
                }
            })
            .map_err(|_| Error::Limit("native H264 active/retiring owner limit"))?;
        Ok(NativePermit(self.clone()))
    }
}
struct NativePermit(Arc<AdmissionCounter>);
impl Drop for NativePermit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Native state is constructed and destroyed on the worker. One admitted command
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
    last_end: Option<i64>,
    _thread: thread::JoinHandle<()>,
}
impl EncoderWorker {
    pub fn native(config: Config) -> Result<Self, Error> {
        let admission = NATIVE_ADMISSION
            .get_or_init(|| Arc::new(AdmissionCounter::new()))
            .clone();
        Self::with_factory_and_admission(config, super::native_factory(), admission)
    }
    #[cfg(test)]
    pub(crate) fn with_factory(config: Config, factory: Factory) -> Result<Self, Error> {
        Self::with_factory_and_admission(config, factory, Arc::new(AdmissionCounter::new()))
    }
    pub(crate) fn with_factory_and_admission(
        config: Config,
        factory: Factory,
        admission: Arc<AdmissionCounter>,
    ) -> Result<Self, Error> {
        // Reserve BEFORE spawn, including factory initialization and retirement.
        let permit = admission.acquire()?;
        let (commands, requests) = mpsc::sync_channel(1);
        let (results, replies) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let worker_config = config.clone();
        let worker_stop = stop.clone();
        let worker_closed = closed.clone();
        let thread = thread::Builder::new()
            .name("native-h264-packets".into())
            .spawn(move || {
                let _closed = ClosedSignal(worker_closed);
                // Reverse local-drop order: SDK/run first, permit second,
                // ClosedSignal last. A failed/stuck destructor keeps its lease.
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
            last_end: None,
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
    pub fn submit(&mut self, input: Nv12Input) -> Result<(), RejectedInput> {
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
        if self.last_end.is_some_and(|end| input.pts() < end) {
            return Err(RejectedInput {
                error: Error::Invalid("overlapping/backward input timestamps"),
                input: Some(input),
            });
        }
        let end = input.pts() + input.duration();
        match self.commands.try_send(Command::Encode(input)) {
            Ok(()) => {
                self.busy = true;
                self.last_end = Some(end);
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
                self.busy = false;
                if matches!(&event, Event::Failed { .. }) {
                    self.stop.store(true, Ordering::Release);
                }
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
    }
}
impl Drop for EncoderWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
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
            configuration: adapter.configuration().cloned(),
        })
        .is_err()
    {
        return;
    }
    while !stop.load(Ordering::Acquire) {
        let command = match requests.recv_timeout(Duration::from_millis(20)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if stop.load(Ordering::Acquire) {
            break;
        }
        let (packets, drained) = match command {
            Command::Encode(input) => (adapter.encode(input), false),
            Command::Drain => (adapter.drain(), true),
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
