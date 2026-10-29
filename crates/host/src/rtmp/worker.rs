//! One socket/transport owner. No network work occurs before explicit start.
use super::{ConnectOptions, EncodedPacket, Error, io_error, transport::Client};
use std::{
    net::{Shutdown, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub packets: u64,
    pub encoded_bytes: u64,
    pub wire_sent: u64,
    pub wire_received: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishKey {
    pub session: u64,
    pub output: u64,
    pub connection: u64,
}
#[derive(Clone, Debug)]
pub enum Event {
    Publishing { key: PublishKey },
    Progress { key: PublishKey, stats: Stats },
    Stopped { key: PublishKey, stats: Stats },
    Failed { key: PublishKey, error: Error },
}
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct OwnerPermit;
impl OwnerPermit {
    fn acquire() -> Result<Self, Error> {
        ACTIVE
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 2).then_some(n + 1)
            })
            .map_err(|_| Error::Backpressure)?;
        Ok(Self)
    }
}
impl Drop for OwnerPermit {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Permit {
    bytes: usize,
    used: Arc<AtomicUsize>,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
struct Job {
    packet: EncodedPacket,
    _permit: Permit,
}
pub struct Publisher {
    options: ConnectOptions,
    sender: Option<mpsc::SyncSender<Job>>,
    events: mpsc::Receiver<Event>,
    used: Arc<AtomicUsize>,
    stopping: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    progress: Arc<AtomicBool>,
    publishing: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Publisher {
    /// Explicit network admission. Caller resolves the endpoint off the GUI.
    /// The socket is created on the worker, including connect/handshake failure.
    pub fn start(options: ConnectOptions) -> Result<Self, Error> {
        options.validate()?;
        let permit = OwnerPermit::acquire()?;
        let (sender, receiver) = mpsc::sync_channel(options.limits.queue_messages);
        let (events_out, events) = mpsc::sync_channel(8);
        let used = Arc::new(AtomicUsize::new(0));
        let stopping = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let publishing = Arc::new(AtomicBool::new(false));
        let is_publishing = publishing.clone();
        let progress = Arc::new(AtomicBool::new(false));
        let requested = progress.clone();
        let worker_options = options.clone();
        let stop = stopping.clone();
        let cancelled = cancel.clone();
        let done = finished.clone();
        let handle = thread::Builder::new()
            .name("skylive-rtmp".into())
            .spawn(move || {
                // Progress is requested only after the frontend drains the event
                // channel, so at most one progress event plus Publishing can be
                // pending. The terminal event always has reserved capacity.
                let _permit = permit;
                let key = PublishKey {
                    session: worker_options.video_generation.session,
                    output: worker_options.video_generation.output,
                    connection: worker_options.connection,
                };
                let result = run(
                    worker_options,
                    &receiver,
                    &events_out,
                    key,
                    WorkerControl {
                        stop: &stop,
                        cancel: cancelled,
                        progress: &requested,
                        publishing: &is_publishing,
                    },
                );
                let event = match result {
                    Ok(stats) => Event::Stopped { key, stats },
                    Err(error) => Event::Failed { key, error },
                };
                let _ = events_out.try_send(event);
                done.store(true, Ordering::Release);
                // Keep admission receiver alive until the original terminal
                // cause is queued. Closed/disconnected cannot precede it.
                // Also retire queued packet Arcs on this owner before its lease.
                drop(receiver);
            })
            .map_err(|e| io_error("spawn RTMP owner", e))?;
        Ok(Self {
            options,
            sender: Some(sender),
            events,
            used,
            stopping,
            cancel,
            finished,
            progress,
            publishing,
            handle: Some(handle),
        })
    }
    pub fn try_packet(&mut self, packet: &EncodedPacket) -> Result<(), Error> {
        if self.stopping.load(Ordering::Acquire) || self.finished.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        if !self.options.accepts(packet) {
            return Err(Error::StaleGeneration);
        }
        let bytes = packet.byte_len();
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|sum| *sum <= self.options.limits.queue_bytes)
            })
            .map_err(|_| Error::Backpressure)?;
        let job = Job {
            packet: packet.clone(),
            _permit: Permit {
                bytes,
                used: self.used.clone(),
            },
        };
        match self.sender.as_ref().ok_or(Error::Closed)?.try_send(job) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => Err(Error::Backpressure),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(Error::Closed),
        }
    }
    /// Stop drains accepted packets then sends EOS/delete. It never blocks the caller.
    pub fn stop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if !self.publishing.load(Ordering::Acquire) {
            self.cancel.store(true, Ordering::Release);
        }
        self.sender.take();
    }
    /// Cancel interrupts bounded socket polls. DNS never runs here; connect may
    /// remain pending only until the configured connect_timeout (at most30s).
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.stop();
    }
    pub fn poll(&mut self) -> Option<Event> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => {
                self.progress.store(true, Ordering::Release);
                None
            }
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }
    pub fn key(&self) -> PublishKey {
        PublishKey {
            session: self.options.video_generation.session,
            output: self.options.video_generation.output,
            connection: self.options.connection,
        }
    }
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    pub fn queued_bytes(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
    /// Tests/headless only. UI Drop detaches instead of waiting.
    pub fn join(mut self) -> Result<(), Error> {
        self.stop();
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| Error::Protocol("RTMP worker panicked"))?;
        }
        Ok(())
    }
}
impl Drop for Publisher {
    fn drop(&mut self) {
        self.cancel();
        self.handle.take();
    }
}
struct WorkerControl<'a> {
    stop: &'a AtomicBool,
    cancel: Arc<AtomicBool>,
    progress: &'a AtomicBool,
    publishing: &'a AtomicBool,
}
fn run(
    options: ConnectOptions,
    receiver: &mpsc::Receiver<Job>,
    events: &mpsc::SyncSender<Event>,
    key: PublishKey,
    control: WorkerControl<'_>,
) -> Result<Stats, Error> {
    let WorkerControl {
        stop,
        cancel,
        progress,
        publishing,
    } = control;
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    // Share the configured deadline across DNS candidates: a broken IPv6
    // route must not prevent a usable IPv4 endpoint from being attempted.
    let deadline = std::time::Instant::now().checked_add(options.timeouts.connect)
        .ok_or(Error::Limit("RTMP connect deadline overflow"))?;
    let addresses = options.target.addresses();
    let mut socket = None;
    let mut last_error = None;
    for (index, address) in addresses.iter().enumerate() {
        if cancel.load(Ordering::Acquire) { return Err(Error::Cancelled); }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() { break; }
        let attempts_left = (addresses.len() - index) as u32;
        // Reserve time for fallbacks without reducing a normal first attempt
        // to a few milliseconds on hosts with many DNS answers.
        let timeout = (remaining / attempts_left.min(4)).max(Duration::from_millis(1)).min(remaining);
        match TcpStream::connect_timeout(address, timeout) {
            Ok(connected) => { socket = Some(connected); break; }
            Err(error) => last_error = Some(error),
        }
    }
    let socket = socket.ok_or_else(|| match last_error {
        Some(error) => io_error("connect RTMP", error),
        None => Error::Deadline("RTMP connect deadline expired"),
    })?;
    socket
        .set_nodelay(true)
        .map_err(|e| io_error("set RTMP nodelay", e))?;
    let shutdown = socket
        .try_clone()
        .map_err(|e| io_error("clone shutdown socket", e))?;
    struct Close(TcpStream);
    impl Drop for Close {
        fn drop(&mut self) {
            let _ = self.0.shutdown(Shutdown::Both);
        }
    }
    let _close = Close(shutdown);
    let tls_deadline = std::time::Instant::now().checked_add(options.timeouts.handshake)
        .ok_or(Error::Limit("TLS handshake deadline overflow"))?;
    let wire = super::network::NetworkWire::open(socket, &options.target, tls_deadline, cancel.clone())?;
    let mut client = Client::new(wire, options, cancel.clone())?;
    client.connect()?;
    publishing.store(true, Ordering::Release);
    let _ = events.try_send(Event::Publishing { key });
    let mut stats = Stats::default();
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(job) => {
                client.packet(&job.packet)?;
                stats.packets = stats
                    .packets
                    .checked_add(1)
                    .ok_or(Error::Limit("packet counter"))?;
                stats.encoded_bytes = stats
                    .encoded_bytes
                    .checked_add(job.packet.byte_len() as u64)
                    .ok_or(Error::Limit("encoded byte counter"))?;
                let (sent, received) = client.wire_counts();
                stats.wire_sent = sent;
                stats.wire_received = received;
                // Progress is intentionally latest-on-poll, not one event per
                // packet. A separate terminal reservation avoids losing EOS.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                client.stop()?;
                let (sent, received) = client.wire_counts();
                stats.wire_sent = sent;
                stats.wire_received = received;
                return Ok(stats);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Acquire) {
                    client.stop()?;
                    let (sent, received) = client.wire_counts();
                    stats.wire_sent = sent;
                    stats.wire_received = received;
                    return Ok(stats);
                }
            }
        }
        client.poll_control()?;
        if progress.swap(false, Ordering::AcqRel) {
            let (sent, received) = client.wire_counts();
            stats.wire_sent = sent;
            stats.wire_received = received;
            let _ = events.try_send(Event::Progress { key, stats });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncancellable_connect_slots_remain_bounded_until_actual_owner_drop() {
        let a = OwnerPermit::acquire().unwrap();
        let b = OwnerPermit::acquire().unwrap();
        assert!(matches!(OwnerPermit::acquire(), Err(Error::Backpressure)));
        drop(a);
        let c = OwnerPermit::acquire().unwrap();
        drop(b);
        drop(c);
        assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
    }
}
