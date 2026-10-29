use super::*;
use std::{
    collections::VecDeque,
    sync::{
        OnceLock, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
};
pub const MAX_STREAM_BLOCKS: usize = 40;
pub const MAX_STREAM_BYTES: usize = 5 * 1024 * 1024;
const MAX_INGRESSES: usize = 3;
static INGRESS_LEASES: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
struct Lease(Arc<AtomicUsize>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Ingress {
    sender: SyncSender<AudioDelivery>,
}
struct State {
    track:skylive_core::AudioTrackId,
    key: OutputKey,
    settings: AudioSettings,
    activation: AtomicU8,
    origin_ns: AtomicU64,
    finish: AtomicBool,
    // MAX is open. A requested cutoff is not producer quiescence: the router
    // retains this sink until actual delayed Program cursor reaches this grid.
    cutoff_frame:AtomicU64,
    observed_end:AtomicU64,
    reconnect_before:AtomicU64,
    reconnect_discarded:AtomicU64,
    ended: AtomicBool,
    fault: AtomicU8,
    count: AtomicUsize,
    bytes: AtomicUsize,
    _lease: Lease,
}
impl State {
    fn anchor(&self) -> Result<Option<OutputAnchor>, OutputError> {
        if self.activation.load(Ordering::Acquire) != 2 {
            return Ok(None);
        }
        OutputAnchor::new(
            MediaTime(self.origin_ns.load(Ordering::Relaxed)),
            self.settings,
        )
        .map(Some)
    }
    fn fail(&self, fault: OutputFault) {
        let code = match fault {
            OutputFault::Scope => 1,
            OutputFault::Order => 2,
            OutputFault::Backpressure => 3,
            OutputFault::Invalid => 4,
            OutputFault::Disconnected => 5,
            OutputFault::Gap => 6,
        };
        let _ = self
            .fault
            .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
        self.finish.store(true, Ordering::Release);
    }
    fn fault(&self) -> Option<OutputFault> {
        match self.fault.load(Ordering::Acquire) {
            0 => None,
            1 => Some(OutputFault::Scope),
            2 => Some(OutputFault::Order),
            3 => Some(OutputFault::Backpressure),
            4 => Some(OutputFault::Invalid),
            5 => Some(OutputFault::Disconnected),
            _ => Some(OutputFault::Gap),
        }
    }
    fn permit(self: &Arc<Self>, bytes: usize) -> Result<Permit, OutputError> {
        self.count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                if n < MAX_STREAM_BLOCKS {
                    Some(n + 1)
                } else {
                    None
                }
            })
            .map_err(|_| OutputError::Fault(OutputFault::Backpressure))?;
        if self
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(bytes).filter(|n| *n <= MAX_STREAM_BYTES)
            })
            .is_err()
        {
            self.count.fetch_sub(1, Ordering::AcqRel);
            return Err(OutputError::Fault(OutputFault::Backpressure));
        }
        Ok(Permit {
            state: self.clone(),
            bytes,
        })
    }
}
struct Permit {
    state: Arc<State>,
    bytes: usize,
}
impl Permit {
    fn add_bytes(&mut self, bytes: usize) -> Result<(), OutputError> {
        self.state
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|total| *total <= MAX_STREAM_BYTES)
            })
            .map_err(|_| OutputError::Fault(OutputFault::Backpressure))?;
        self.bytes += bytes;
        Ok(())
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.state.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        self.state.count.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Clone, Copy, Debug)]
pub struct StreamingAudioStats {
    pub key: OutputKey,
    pub finishing: bool,
    pub producer_ended: bool,
    pub queued_blocks: usize,
    pub charged_bytes: usize,
    pub fault: Option<OutputFault>,
}
#[derive(Clone)]
pub struct StreamingAudioControl {
    state: Arc<State>,
}
impl StreamingAudioControl {
    pub(crate) fn begin_reconnect(&self){self.state.reconnect_before.store(u64::MAX,Ordering::Release);}
    pub(crate) fn reconnecting(&self)->bool{self.state.reconnect_before.load(Ordering::Acquire)==u64::MAX}
    pub(crate) fn resume_after(&self,frame:u64){self.state.reconnect_before.store(frame,Ordering::Release);}
    pub(crate) fn observed_end_frame(&self)->u64{self.state.observed_end.load(Ordering::Acquire)}
    pub(crate) fn reconnect_discarded_frames(&self)->u64{self.state.reconnect_discarded.load(Ordering::Acquire)}
    pub fn key(&self) -> OutputKey {
        self.state.key
    }
    pub fn request_stop_at(&self,cutoff:MediaTime)->Result<(),OutputError>{
        let anchor=self.state.anchor()?.ok_or(OutputError::Invalid("output cutoff requires activation"))?;
        // A block already delivered to the consumer cannot be retracted.
        // Preserve that accepted prefix; all subsequent delivery is trimmed at
        // this original Program grid. Native AAC priming/tail stays separate.
        let frame=anchor.frame_at_or_after(cutoff)?.max(anchor.first_sample_frame()).max(self.state.observed_end.load(Ordering::Acquire));
        self.state.cutoff_frame.fetch_min(frame,Ordering::AcqRel);
        if self.state.observed_end.load(Ordering::Acquire)>=frame{self.state.finish.store(true,Ordering::Release);}
        Ok(())
    }
    /// Abort/retirement only. A graceful finite output uses request_stop_at.
    pub fn request_stop(&self) {
        self.state.finish.store(true, Ordering::Release);
    }
    /// Only a current ReadyForInputs completion may select the shared fresh
    /// first-video-frame origin. A stale key never faults or changes this output.
    pub fn activate(&self, key: OutputKey, origin: MediaTime) -> Result<OutputAnchor, OutputError> {
        if key != self.state.key {
            return Err(OutputError::Fault(OutputFault::Scope));
        }
        if self.state.finish.load(Ordering::Acquire) {
            return Err(OutputError::Fault(OutputFault::Disconnected));
        }
        let anchor = OutputAnchor::new(origin, self.state.settings)?;
        match self
            .state
            .activation
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                self.state.origin_ns.store(origin.0, Ordering::Relaxed);
                self.state.activation.store(2, Ordering::Release);
                Ok(anchor)
            }
            Err(2) if self.state.origin_ns.load(Ordering::Relaxed) == origin.0 => Ok(anchor),
            _ => Err(OutputError::Invalid(
                "output audio anchor already selected or being selected",
            )),
        }
    }
    pub fn stats(&self) -> StreamingAudioStats {
        StreamingAudioStats {
            key: self.state.key,
            finishing: self.state.finish.load(Ordering::Acquire),
            producer_ended: self.state.ended.load(Ordering::Acquire),
            queued_blocks: self.state.count.load(Ordering::Acquire),
            charged_bytes: self.state.bytes.load(Ordering::Acquire),
            fault: self.state.fault(),
        }
    }
}
/// Owned delivery stays on the Streaming worker. Accessors expose borrowed PCM,
/// never an unbudgeted cloned Arc or a GUI publication queue.
pub struct AudioDelivery {
    track:skylive_core::AudioTrackId,
    output: OutputKey,
    program: ProgramCaptureKey,
    anchor: OutputAnchor,
    block: Arc<ProgramAudioBlock>,
    offset: usize,
    frames: usize,
    gap: u64,
    _permit: Permit,
}
impl AudioDelivery {
    pub fn track(&self)->skylive_core::AudioTrackId{self.track}
    pub fn output_key(&self) -> OutputKey {
        self.output
    }
    pub fn program_key(&self) -> ProgramCaptureKey {
        self.program
    }
    pub fn anchor(&self) -> OutputAnchor {
        self.anchor
    }
    pub fn absolute_first_frame(&self) -> u64 {
        self.block.start_frame + self.offset as u64
    }
    pub fn offset_frames(&self) -> usize {
        self.offset
    }
    pub fn frames(&self) -> usize {
        self.frames
    }
    pub fn gap_before_frames(&self) -> u64 {
        self.gap
    }
    pub fn samples(&self) -> &[f32] {
        let channels = usize::from(self.block.settings.speakers().channels());
        &self.block.track_samples(self.track).expect("admitted selected audio track")[self.offset * channels..(self.offset + self.frames) * channels]
    }
    pub fn first_pts_100ns(&self) -> Result<i64, OutputError> {
        self.anchor.sample_pts_100ns(self.absolute_first_frame())
    }
    #[cfg(test)]
    pub(super) fn allocation(&self) -> *const ProgramAudioBlock {
        Arc::as_ptr(&self.block)
    }
}
pub struct StreamingAudioReceiver {
    receiver: Receiver<AudioDelivery>,
    _ingress: Arc<Ingress>,
    state: Arc<State>,
}
impl StreamingAudioReceiver {
    pub fn key(&self) -> OutputKey {
        self.state.key
    }
    pub fn poll(&mut self) -> Option<AudioDelivery> {
        self.receiver.try_recv().ok()
    }
    pub fn producer_ended(&self) -> bool {
        self.state.ended.load(Ordering::Acquire)
    }
    /// Distinct from native encoder/socket retirement, owned by StreamingOwner.
    pub fn drained(&self) -> bool {
        self.producer_ended() && self.state.count.load(Ordering::Acquire) == 0
    }
}
impl Drop for StreamingAudioReceiver {
    fn drop(&mut self) {
        self.state.finish.store(true, Ordering::Release);
    }
}
pub struct StreamingAudioSink {
    ingress: Weak<Ingress>,
    state: Arc<State>,
    key: Option<ProgramCaptureKey>,
    identity: Option<(u64, u64)>,
    seen_end: Option<u64>,
    end: Option<u64>,
    preroll: VecDeque<Pending>,
}
struct Pending {
    key: ProgramCaptureKey,
    block: Arc<ProgramAudioBlock>,
    permit: Permit,
}
pub fn streaming_channel(
    key: OutputKey,
    settings: AudioSettings,
    origin: Option<MediaTime>,
) -> Result<
    (
        StreamingAudioSink,
        StreamingAudioReceiver,
        StreamingAudioControl,
    ),
    OutputError,
> {
    let pool = INGRESS_LEASES
        .get_or_init(|| Arc::new(AtomicUsize::new(0)))
        .clone();
    channel_with_pool(key, settings, origin, pool)
}
pub fn streaming_channel_track(key:OutputKey,settings:AudioSettings,origin:Option<MediaTime>,track:skylive_core::AudioTrackId)
    ->Result<(StreamingAudioSink,StreamingAudioReceiver,StreamingAudioControl),OutputError>{
    let pool=INGRESS_LEASES.get_or_init(||Arc::new(AtomicUsize::new(0))).clone();
    channel_with_pool_track(key,settings,origin,pool,track)
}
pub(crate) fn channel_with_pool(key:OutputKey,settings:AudioSettings,origin:Option<MediaTime>,pool:Arc<AtomicUsize>)
 ->Result<(StreamingAudioSink,StreamingAudioReceiver,StreamingAudioControl),OutputError>{
 channel_with_pool_track(key,settings,origin,pool,skylive_core::AudioTrackId::ONE)
}
pub(crate) fn channel_with_pool_track(
    key: OutputKey,
    settings: AudioSettings,
    origin: Option<MediaTime>,
    pool: Arc<AtomicUsize>,
    track:skylive_core::AudioTrackId,
) -> Result<
    (
        StreamingAudioSink,
        StreamingAudioReceiver,
        StreamingAudioControl,
    ),
    OutputError,
> {
    key.validate()?;
    if !matches!(key.kind,OutputKind::Streaming|OutputKind::ReplayBuffer|OutputKind::NdiProgram) {
        return Err(OutputError::Invalid(
            "Program PCM channel requires Streaming, ReplayBuffer or NdiProgram output kind",
        ));
    }
    if let Some(origin) = origin {
        OutputAnchor::new(origin, settings)?;
    }
    pool.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
        if n < MAX_INGRESSES { Some(n + 1) } else { None }
    })
    .map_err(|_| OutputError::Limit("active/retiring Streaming audio ingress limit"))?;
    let lease = Lease(pool);
    let (sender, receiver) = mpsc::sync_channel(MAX_STREAM_BLOCKS);
    let ingress = Arc::new(Ingress { sender });
    let state = Arc::new(State {
        track,
        key,
        settings,
        activation: AtomicU8::new(if origin.is_some() { 2 } else { 0 }),
        origin_ns: AtomicU64::new(origin.map_or(0, |value| value.0)),
        finish: AtomicBool::new(false),
        cutoff_frame:AtomicU64::new(u64::MAX),observed_end:AtomicU64::new(0),
        reconnect_before:AtomicU64::new(0),reconnect_discarded:AtomicU64::new(0),
        ended: AtomicBool::new(false),
        fault: AtomicU8::new(0),
        count: AtomicUsize::new(0),
        bytes: AtomicUsize::new(0),
        _lease: lease,
    });
    Ok((
        StreamingAudioSink {
            ingress: Arc::downgrade(&ingress),
            state: state.clone(),
            key: None,
            identity: None,
            seen_end: None,
            end: None,
            preroll: VecDeque::with_capacity(2),
        },
        StreamingAudioReceiver {
            receiver,
            _ingress: ingress,
            state: state.clone(),
        },
        StreamingAudioControl { state },
    ))
}
impl StreamingAudioSink {
    pub(crate) fn selected_track(&self)->skylive_core::AudioTrackId{self.state.track}
    pub fn output_key(&self) -> OutputKey {
        self.state.key
    }
    pub(crate) fn finishing(&self) -> bool {
        self.state.finish.load(Ordering::Acquire)
    }
    pub(crate) fn bind(&mut self, key: ProgramCaptureKey) -> Result<(), OutputError> {
        if !accepts(self.key, key, self.state.key.session, self.state.settings) {
            self.state.fail(OutputFault::Scope);
            return Err(OutputError::Fault(OutputFault::Scope));
        }
        if self.key != Some(key) {
            self.identity = None;
        }
        self.key = Some(key);
        Ok(())
    }
    pub(crate) fn fail_scope(&self) {
        if !self.finishing() {
            self.state.fail(OutputFault::Scope);
        }
    }
    pub(crate) fn fail_backpressure(&self) {
        self.state.fail(OutputFault::Backpressure);
    }
    pub(crate) fn poll_activation(&mut self) -> bool {
        if self.finishing() {
            return false;
        }
        let anchor = match self.state.anchor() {
            Ok(Some(anchor)) => anchor,
            Ok(None) => return true,
            Err(_) => {
                self.state.fail(OutputFault::Invalid);
                return false;
            }
        };
        while let Some(pending) = self.preroll.pop_front() {
            if !self.publish(pending, anchor) {
                return false;
            }
        }
        true
    }
    pub(crate) fn submit(&mut self, key: ProgramCaptureKey, block: Arc<ProgramAudioBlock>) -> bool {
        if self.finishing() || self.bind(key).is_err() {
            return false;
        }
        if block.track_samples(self.state.track).is_none(){self.state.fail(OutputFault::Invalid);return false;}
        let identity = shared_identity(&block);
        if block.session != self.state.key.session
            || block.settings != key.settings
            || self.identity.is_some_and(|old| old != identity)
        {
            self.state.fail(OutputFault::Scope);
            return false;
        }
        let Some(end) = block.start_frame.checked_add(block.frames as u64) else {
            self.state.fail(OutputFault::Invalid);
            return false;
        };
        if self.seen_end.is_some_and(|old| block.start_frame < old) {
            self.state.fail(OutputFault::Order);
            return false;
        }
        let charge = match block_charge(&block) {
            Ok(charge) => charge,
            Err(_) => {
                self.state.fail(OutputFault::Invalid);
                return false;
            }
        };
        let reconnect_before=self.state.reconnect_before.load(Ordering::Acquire);
        if reconnect_before==u64::MAX||end<=reconnect_before&&reconnect_before!=0{
            // Keep validating original Program identity/order, but do not take
            // another queue/byte credit while DNS/codec/socket owner can block.
            // This is deliberate output-only discard, not a second DSP/render.
            let mut discarded=block.frames as u64;for old in self.preroll.drain(..){discarded=match discarded.checked_add(old.block.frames as u64){Some(n)=>n,None=>{self.state.fail(OutputFault::Invalid);return false;}};}
            if self.state.reconnect_discarded.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|n.checked_add(discarded)).is_err(){self.state.fail(OutputFault::Invalid);return false;}
            self.identity=Some(identity);self.seen_end=Some(end);self.end=Some(end);self.state.observed_end.store(end,Ordering::Release);return true;
        }
        let anchor = match self.state.anchor() {
            Ok(anchor) => anchor,
            Err(_) => {
                self.state.fail(OutputFault::Invalid);
                return false;
            }
        };
        if anchor.is_none() && self.preroll.len() == 2 {
            self.preroll.pop_front();
        }
        let bytes = match charge.checked_add(std::mem::size_of::<AudioDelivery>()) {
            Some(bytes) => bytes,
            None => {
                self.state.fail(OutputFault::Invalid);
                return false;
            }
        };
        let permit = match self.state.permit(bytes) {
            Ok(permit) => permit,
            Err(_) => {
                self.state.fail(OutputFault::Backpressure);
                return false;
            }
        };
        self.identity = Some(identity);
        self.seen_end = Some(end);
        let pending = Pending { key, block, permit };
        let Some(anchor) = anchor else {
            self.preroll.push_back(pending);
            return true;
        };
        if !self.poll_activation() {
            return false;
        }
        self.publish(pending, anchor)
    }
    fn publish(&mut self, pending: Pending, anchor: OutputAnchor) -> bool {
        let Pending {
            key,
            block,
            mut permit,
        } = pending;
        let actual_end=block.start_frame+block.frames as u64;
        let cutoff=self.state.cutoff_frame.load(Ordering::Acquire);
        let end=actual_end.min(cutoff);
        if end<=block.start_frame {
            self.state.observed_end.store(actual_end,Ordering::Release);
            if actual_end>=cutoff {self.state.finish.store(true,Ordering::Release);return false;}
            return true;
        }
        // Old prefix carries no output sample, but still advances the unique
        // producer's observed timeline; it is neither silence nor a replay.
        let resume=self.state.reconnect_before.load(Ordering::Acquire);
        if resume==u64::MAX{if self.state.reconnect_discarded.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|n.checked_add(block.frames as u64)).is_err(){self.state.fail(OutputFault::Invalid);return false;}self.end=Some(actual_end);self.state.observed_end.store(actual_end,Ordering::Release);return true;}
        let floor=anchor.first_sample_frame().max(resume);
        if end <= floor {
            return true;
        }
        let first = block.start_frame.max(floor);
        if resume!=0{let discarded=resume.saturating_sub(block.start_frame).min(block.frames as u64);if self.state.reconnect_discarded.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|n.checked_add(discarded)).is_err(){self.state.fail(OutputFault::Invalid);return false;}}
        let expected = self
            .end
            .unwrap_or(anchor.first_sample_frame())
            .max(floor);
        let gap = first.saturating_sub(expected);
        let max_gap = u64::from(self.state.settings.sample_rate() / 10);
        if gap > max_gap {
            self.state.fail(OutputFault::Gap);
            return false;
        }
        let channels = usize::from(self.state.settings.speakers().channels());
        let bytes = match (gap as usize)
            .checked_mul(channels)
            .and_then(|n| n.checked_mul(4))
        {
            Some(bytes) => bytes,
            None => {
                self.state.fail(OutputFault::Invalid);
                return false;
            }
        };
        if permit.add_bytes(bytes).is_err() {
            self.state.fail(OutputFault::Backpressure);
            return false;
        }
        let Some(ingress) = self.ingress.upgrade() else {
            self.state.fail(OutputFault::Disconnected);
            return false;
        };
        let offset = (first - block.start_frame) as usize;
        let frames = (end - first) as usize;
        let delivery = AudioDelivery {
            track:self.state.track,
            output: self.state.key,
            program: key,
            anchor,
            block,
            offset,
            frames,
            gap,
            _permit: permit,
        };
        if ingress.sender.try_send(delivery).is_err() {
            self.state.fail(OutputFault::Backpressure);
            return false;
        }
        self.end = Some(end);
        self.state.observed_end.store(actual_end,Ordering::Release);
        if actual_end>=cutoff {self.state.finish.store(true,Ordering::Release);return false;}
        true
    }
}
impl Drop for StreamingAudioSink {
    fn drop(&mut self) {
        if !self.finishing() {
            self.state.fail(OutputFault::Disconnected);
        }
        self.state.ended.store(true, Ordering::Release);
    }
}
