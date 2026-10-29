//! Timestamp-based queue for asynchronous source frames.
//!
//! This is a media primitive, not a registered source filter. Its caller must
//! feed each native source sample once, retire it on a stream change, and map
//! the returned source timestamp into the audio/video synchronization clock.
//! In particular, project render ticks are not a substitute for source samples.
//!
//! Ownership: one sequential queue per filter/stream; budgets may be shared.
//! Time: signed nanoseconds in a single native source-clock domain.
//! Limits: delay 0..=20s; queue slots and all retained frame bytes are admitted.
//! Failure: admission returns the original input; it never fabricates a frame.
//! Lifecycle: configuration, seek, source retirement and clock changes reset
//! the queue; outputs held by consumers keep their admission credits.
//! Verification: deferred; this primitive is not yet wired to capture or audio.
use crate::RgbaFrame;
use std::{collections::VecDeque, sync::{Arc, Mutex}};

pub const MAX_ASYNC_VIDEO_DELAY_MS: u32 = 20_000;
const NS_PER_MS: i128 = 1_000_000;
const MAX_TIMESTAMP_GAP_NS: i128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsyncDelayError {
    InvalidLimits,
    InvalidDelay,
    BudgetExceeded,
    Allocation,
    BudgetPoisoned,
}
impl std::fmt::Display for AsyncDelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "asynchronous video delay: {self:?}")
    }
}
impl std::error::Error for AsyncDelayError {}

/// An ordered transform is admitted before it runs. Its own error is kept
/// distinct from a delay-budget or timestamp failure.
#[derive(Debug)]
pub enum AsyncTransformError<E> { Delay(AsyncDelayError), Transform(E) }

#[derive(Default)]
struct Usage { bytes: usize, frames: usize }
struct SharedBudget { max_bytes: usize, max_frames: usize, usage: Mutex<Usage> }

/// Shared admission across queues and frames still held by consumers.
#[derive(Clone)]
pub struct AsyncDelayBudget(Arc<SharedBudget>);
impl AsyncDelayBudget {
    pub fn new(max_bytes: usize, max_frames: usize) -> Result<Self, AsyncDelayError> {
        if max_bytes == 0 || max_frames == 0 { return Err(AsyncDelayError::InvalidLimits); }
        Ok(Self(Arc::new(SharedBudget { max_bytes, max_frames, usage: Mutex::new(Usage::default()) })))
    }
    fn reserve(&self, bytes: usize, frames: usize) -> Result<Lease, AsyncDelayError> {
        let mut usage = self.0.usage.lock().map_err(|_| AsyncDelayError::BudgetPoisoned)?;
        let new_bytes = usage.bytes.checked_add(bytes).filter(|n| *n <= self.0.max_bytes)
            .ok_or(AsyncDelayError::BudgetExceeded)?;
        let new_frames = usage.frames.checked_add(frames).filter(|n| *n <= self.0.max_frames)
            .ok_or(AsyncDelayError::BudgetExceeded)?;
        usage.bytes = new_bytes;
        usage.frames = new_frames;
        Ok(Lease { budget: self.0.clone(), bytes, frames })
    }
}
struct Lease { budget: Arc<SharedBudget>, bytes: usize, frames: usize }
/// Exclusive scratch credit for a background transform. The credit follows the
/// permit and is returned only after the transform has completed or failed.
pub struct AsyncWorkspacePermit { _lease: Lease }
impl AsyncDelayBudget {
    pub fn reserve_workspace(&self, bytes:usize)->Result<AsyncWorkspacePermit,AsyncDelayError> {
        Ok(AsyncWorkspacePermit {_lease:self.reserve(bytes,0)?})
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        // A poisoned budget must not strand credits when a consumer retires.
        let mut usage = self.budget.usage.lock().unwrap_or_else(|e| e.into_inner());
        usage.bytes -= self.bytes;
        usage.frames -= self.frames;
    }
}
/// One bounded observation of the DirectShow graph's reference clock. QPC
/// bracket and reference time are simultaneous only within the stated range;
/// stream times use `run_start_100ns`, not the QPC epoch. This is evidence,
/// not an assertion that the two clocks have identical long-term rates.
#[derive(Clone, Copy, Debug)]
pub struct DirectShowClockSnapshot {
    /// Unique for this graph owner within the current process.
    pub graph_id: u64,
    pub audio_clock_generation: u64,
    pub video_clock_generation: u64,
    pub run_start_100ns: i64,
    pub reference_100ns: i64,
    pub qpc_before_ns: u64,
    pub qpc_after_ns: u64,
    /// Previous reference observation in the same clock/run generation.
    pub previous: Option<(i64,u64,u64)>,
}
impl DirectShowClockSnapshot {
    /// Assuming a monotonic graph clock, give conservative bounds without
    /// assuming equal clock rates or hardware capture accuracy.
    /// No extrapolation: a sample outside the observed interval is unavailable.
    pub fn qpc_interval(&self,stream_ns:i128)->Option<(u64,u64)> {
        let (previous_ref,before,after)=self.previous?;
        // Reject malformed or stale evidence even when supplied by a caller
        // other than the graph owner. Bounds are inclusive and preserve the
        // uncertainty in both reference-clock observations.
        if before>after || after>self.qpc_before_ns || self.qpc_before_ns>self.qpc_after_ns
            || self.qpc_after_ns.checked_sub(before)?>100_000_000
            || previous_ref>=self.reference_100ns {return None;}
        let target=i128::from(self.run_start_100ns).checked_mul(100)?.checked_add(stream_ns)?;
        // Equal readings can span a clock plateau (including 100 ns
        // quantization). Only strict ordering proves that the sample occurred
        // after the earlier observation and before the later observation.
        if target<=i128::from(previous_ref)*100 || target>=i128::from(self.reference_100ns)*100 {return None;}
        Some((before,self.qpc_after_ns))
    }
}

/// Immutable source-owned metadata retained with delayed pixels. The owner must
/// include its own resource-budget permit inside the value before supplying it.
pub type DelayedFrameContext = Arc<dyn std::any::Any + Send + Sync>;
struct TrackedFrame { timestamp_ns: i128, sequence: Option<u64>, device_qpc_ns: Option<u64>, directshow_clock: Option<DirectShowClockSnapshot>, context: Option<DelayedFrameContext>, image: SharedVideoFrame, _lease: Lease }

/// The retained credit follows all clones, including after removal from the queue.
#[derive(Clone)]
pub struct DelayedVideoFrame(Arc<TrackedFrame>);
impl DelayedVideoFrame {
    pub fn timestamp_ns(&self) -> i128 { self.0.timestamp_ns }
    /// Original capture sequence, if supplied by the source owner. It must
    /// travel with the delayed pixels, not be taken from the arriving frame.
    pub fn sequence(&self) -> Option<u64> { self.0.sequence }
    /// Original Windows device timestamp in QPC's epoch, when supplied by
    /// the capture device. This travels with the pixels through every stage.
    pub fn device_qpc_ns(&self) -> Option<u64> { self.0.device_qpc_ns }
    pub fn directshow_clock(&self) -> Option<DirectShowClockSnapshot> { self.0.directshow_clock }
    pub fn context(&self) -> Option<&DelayedFrameContext> { self.0.context.as_ref() }
    pub fn image(&self) -> &RgbaFrame { self.0.image.image() }
}

/// An immutable frame that can travel through capture and render owners while
/// preserving the credit of a delayed image.  No method exposes the inner
/// `Arc<RgbaFrame>`: cloning that Arc separately would allow the pixels to
/// outlive the credit. Cloning this wrapper retains both together.
#[derive(Clone)]
pub struct SharedVideoFrame(FrameStorage);

/// Pixel owner with an external lifetime/credit. Implementations must retain
/// their admission lease for as long as `image` can be accessed.
pub trait LeasedVideoPixels: Send + Sync { fn image(&self)->&RgbaFrame; }

#[derive(Clone)]
enum FrameStorage {
    Direct(Arc<RgbaFrame>),
    Delayed(DelayedVideoFrame),
    Leased(Arc<dyn LeasedVideoPixels>),
}

/// Identity without retaining either delayed pixels or their credit.
#[derive(Clone)]
pub struct WeakSharedVideoFrame(WeakFrameStorage);

#[derive(Clone)]
enum WeakFrameStorage {
    Direct(std::sync::Weak<RgbaFrame>),
    Delayed(std::sync::Weak<TrackedFrame>),
    Leased(std::sync::Weak<dyn LeasedVideoPixels>),
}

impl WeakSharedVideoFrame {
    pub fn ptr_eq(&self, frame: &SharedVideoFrame) -> bool {
        match (&self.0, &frame.0) {
            (WeakFrameStorage::Direct(weak), FrameStorage::Direct(image)) =>
                std::sync::Weak::ptr_eq(weak, &Arc::downgrade(image)),
            (WeakFrameStorage::Delayed(weak), FrameStorage::Delayed(image)) =>
                std::sync::Weak::ptr_eq(weak, &Arc::downgrade(&image.0)),
            (WeakFrameStorage::Leased(weak), FrameStorage::Leased(image)) =>
                std::sync::Weak::ptr_eq(weak, &Arc::downgrade(image)),
            _ => false,
        }
    }
}

impl SharedVideoFrame {
    /// Ordinary capture frame whose lifetime is managed by its capture owner.
    pub fn direct(image: Arc<RgbaFrame>) -> Self { Self(FrameStorage::Direct(image)) }

    /// Clone an ordinary frame for legacy Arc-only consumers. Delayed frames
    /// must remain wrapped: exposing their inner Arc would orphan the credit.
    pub fn direct_arc(&self) -> Option<Arc<RgbaFrame>> {
        match &self.0 {
            FrameStorage::Direct(image) => Some(image.clone()),
            FrameStorage::Delayed(_) | FrameStorage::Leased(_) => None,
        }
    }

    /// Retain the original delayed frame, including its admission credit.
    pub fn delayed(frame: DelayedVideoFrame) -> Self { Self(FrameStorage::Delayed(frame)) }

    /// The supplied owner retains both pixels and its resource credit.
    pub fn leased(owner:Arc<dyn LeasedVideoPixels>)->Self {Self(FrameStorage::Leased(owner))}

    pub fn image(&self) -> &RgbaFrame {
        match &self.0 {
            FrameStorage::Direct(image) => image,
            FrameStorage::Delayed(frame) => frame.image(),
            FrameStorage::Leased(frame) => frame.image(),
        }
    }

    /// Weak identity keeps the allocation identity reserved without retaining
    /// the frame or the admission lease after the last strong owner retires.
    pub fn downgrade(&self) -> WeakSharedVideoFrame {
        WeakSharedVideoFrame(match &self.0 {
            FrameStorage::Direct(image) => WeakFrameStorage::Direct(Arc::downgrade(image)),
            FrameStorage::Delayed(image) => WeakFrameStorage::Delayed(Arc::downgrade(&image.0)),
            FrameStorage::Leased(image) => WeakFrameStorage::Leased(Arc::downgrade(image)),
        })
    }

    /// True when two wrappers refer to the same immutable pixel allocation.
    /// This is an identity check, not a content hash or freshness comparison.
    pub fn shares_pixels_with(&self, other: &Self) -> bool {
        std::ptr::eq(self.image().pixels().as_ptr(), other.image().pixels().as_ptr())
    }
}

impl From<Arc<RgbaFrame>> for SharedVideoFrame {
    fn from(image: Arc<RgbaFrame>) -> Self { Self::direct(image) }
}
impl From<DelayedVideoFrame> for SharedVideoFrame {
    fn from(frame: DelayedVideoFrame) -> Self { Self::delayed(frame) }
}

impl std::ops::Deref for SharedVideoFrame {
    type Target = RgbaFrame;
    fn deref(&self) -> &Self::Target { self.image() }
}
impl std::fmt::Debug for SharedVideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedVideoFrame")
            .field("width", &self.width()).field("height", &self.height())
            .field("delayed", &matches!(&self.0, FrameStorage::Delayed(_))).finish()
    }
}

pub struct AsyncVideoDelay {
    interval_ns: i128,
    last_timestamp_ns: Option<i128>,
    reached: bool,
    pending: VecDeque<DelayedVideoFrame>,
    max_pending: usize,
    budget: AsyncDelayBudget,
    _queue_lease: Lease,
}
impl AsyncVideoDelay {
    /// `max_pending` must cover the desired frame rate and delay. The caller
    /// chooses it from its declared workload; capacity and pixels are bounded.
    pub fn new(delay_ms: u32, max_pending: usize, budget: AsyncDelayBudget) -> Result<Self, AsyncDelayError> {
        if delay_ms > MAX_ASYNC_VIDEO_DELAY_MS { return Err(AsyncDelayError::InvalidDelay); }
        if max_pending == 0 || max_pending > budget.0.max_frames { return Err(AsyncDelayError::InvalidLimits); }
        // VecDeque's allocation can round up; admit a twofold slot envelope.
        let bytes = max_pending.checked_mul(2)
            .and_then(|n| n.checked_mul(std::mem::size_of::<DelayedVideoFrame>()))
            .ok_or(AsyncDelayError::InvalidLimits)?;
        let queue_lease = budget.reserve(bytes, 0)?;
        let mut pending = VecDeque::new();
        pending.try_reserve_exact(max_pending).map_err(|_| AsyncDelayError::Allocation)?;
        if pending.capacity() > max_pending.saturating_mul(2) { return Err(AsyncDelayError::Allocation); }
        Ok(Self { interval_ns: i128::from(delay_ms) * NS_PER_MS, last_timestamp_ns: None,
            reached: false, pending, max_pending, budget, _queue_lease: queue_lease })
    }
    /// OBS resets its history when settings change, even when the delay grows.
    pub fn reconfigure(&mut self, delay_ms: u32) -> Result<(), AsyncDelayError> {
        if delay_ms > MAX_ASYNC_VIDEO_DELAY_MS { return Err(AsyncDelayError::InvalidDelay); }
        self.interval_ns = i128::from(delay_ms) * NS_PER_MS;
        self.reset();
        Ok(())
    }
    /// Also call on a seek, device rebind, source retirement or clock-domain change.
    pub fn reset(&mut self) { self.pending.clear(); self.last_timestamp_ns = None; self.reached = false; }

    /// Accept one source frame in its native monotonic timestamp domain.
    /// Returns `None` during warmup and one original-timestamped frame once the
    /// interval has been reached. Never fabricates frames for missing samples.
    /// On failure the submitted image is returned to the owner unchanged.
    pub fn push(&mut self, timestamp_ns: i128, image: Arc<RgbaFrame>)
        -> Result<Option<DelayedVideoFrame>, (AsyncDelayError, Arc<RgbaFrame>)> {
        self.push_direct(timestamp_ns, None, image)
    }

    /// Source owners can preserve their sample identity through warmup and
    /// buffering. Stream/clock changes must still call `reset` explicitly.
    pub fn push_with_sequence(&mut self, timestamp_ns: i128, sequence: u64, image: Arc<RgbaFrame>)
        -> Result<Option<DelayedVideoFrame>, (AsyncDelayError, Arc<RgbaFrame>)> {
        self.push_direct(timestamp_ns, Some(sequence), image)
    }

    fn push_direct(&mut self, timestamp_ns: i128, sequence: Option<u64>, image: Arc<RgbaFrame>)
        -> Result<Option<DelayedVideoFrame>, (AsyncDelayError, Arc<RgbaFrame>)> {
        self.push_inner(timestamp_ns, sequence, SharedVideoFrame::direct(image)).map_err(|(error, image)| {
            match image.0 {
                FrameStorage::Direct(image) => (error, image),
                FrameStorage::Delayed(_) | FrameStorage::Leased(_) => unreachable!("direct input remains direct on rejection"),
            }
        })
    }

    /// Accept an upstream delayed output without separating its pixels from
    /// their credit. Each stage conservatively reserves its own full credit.
    pub fn push_shared_with_sequence(&mut self, timestamp_ns: i128, sequence: u64, image: SharedVideoFrame)
        -> Result<Option<DelayedVideoFrame>, (AsyncDelayError, SharedVideoFrame)> {
        self.push_inner(timestamp_ns, Some(sequence), image)
    }

    /// Detach the first delayed stage from a capture owner's ordinary frame
    /// ledger. Reserve the whole delayed-frame charge before allocating pixels,
    /// so a long history cannot keep every capture-reader slot alive. Later
    /// delay stages can use `push_shared_with_sequence` without another copy.
    pub fn push_copy_with_sequence(&mut self, timestamp_ns: i128, sequence: u64, image: &RgbaFrame)
        -> Result<Option<DelayedVideoFrame>, AsyncDelayError> {
        self.push_copy_with_sequence_and_qpc(timestamp_ns, sequence, None, image)
    }

    pub fn push_copy_with_sequence_and_qpc(&mut self, timestamp_ns: i128, sequence: u64, device_qpc_ns: Option<u64>, image: &RgbaFrame)
        -> Result<Option<DelayedVideoFrame>, AsyncDelayError> {
        self.push_copy_with_clock(timestamp_ns,sequence,device_qpc_ns,None,image)
    }

    pub fn push_copy_with_clock(&mut self,timestamp_ns:i128,sequence:u64,device_qpc_ns:Option<u64>,directshow_clock:Option<DirectShowClockSnapshot>,image:&RgbaFrame) -> Result<Option<DelayedVideoFrame>,AsyncDelayError> {
        self.push_transformed_with_clock(timestamp_ns,sequence,device_qpc_ns,directshow_clock,
            (image.width(),image.height()), || {
                let mut pixels=Vec::new();
                pixels.try_reserve_exact(image.byte_len()).map_err(|_| AsyncDelayError::Allocation)?;
                pixels.extend_from_slice(image.pixels());
                RgbaFrame::new(image.width(),image.height(),pixels).map_err(|_|AsyncDelayError::Allocation)
            }).map_err(|error|match error {AsyncTransformError::Delay(error)|AsyncTransformError::Transform(error)=>error})
    }

    /// Produce one frame for this queue only after its final pixel storage has
    /// been admitted. The caller must independently admit transformation
    /// scratch and keep any upstream frame/lease alive until `transform`
    /// returns. Timestamps and clock evidence must belong to that exact input.
    /// The output dimensions are part of admission and may not be changed by
    /// the callback. Failed transforms retain no delayed output or credit;
    /// an observed timestamp discontinuity still resets the prior history.
    pub fn push_transformed_with_clock<E>(&mut self,timestamp_ns:i128,sequence:u64,
        device_qpc_ns:Option<u64>,directshow_clock:Option<DirectShowClockSnapshot>,
        dimensions:(u32,u32),transform:impl FnOnce()->Result<RgbaFrame,E>)
        -> Result<Option<DelayedVideoFrame>,AsyncTransformError<E>> {
        self.push_transformed_with_context(timestamp_ns,sequence,device_qpc_ns,directshow_clock,
            None,dimensions,transform)
    }

    /// Like `push_transformed_with_clock`, but retains a source-owned immutable
    /// context for this exact sample across all subsequent delay stages.
    pub fn push_transformed_with_context<E>(&mut self,timestamp_ns:i128,sequence:u64,
        device_qpc_ns:Option<u64>,directshow_clock:Option<DirectShowClockSnapshot>,
        context:Option<DelayedFrameContext>,dimensions:(u32,u32),transform:impl FnOnce()->Result<RgbaFrame,E>)
        -> Result<Option<DelayedVideoFrame>,AsyncTransformError<E>> {
        let bytes=(dimensions.0 as usize).checked_mul(dimensions.1 as usize)
            .and_then(|pixels|pixels.checked_mul(4)).filter(|_|dimensions.0>0&&dimensions.1>0)
            .ok_or(AsyncTransformError::Delay(AsyncDelayError::InvalidLimits))?;
        let (full, lease) = self.admit(timestamp_ns,bytes).map_err(AsyncTransformError::Delay)?;
        let copied=transform().map_err(AsyncTransformError::Transform)?;
        if (copied.width(),copied.height())!=dimensions || copied.byte_len()!=bytes {
            return Err(AsyncTransformError::Delay(AsyncDelayError::InvalidLimits));
        }
        let frame = DelayedVideoFrame(Arc::new(TrackedFrame {
            timestamp_ns, sequence: Some(sequence), device_qpc_ns, directshow_clock, context, image: SharedVideoFrame::direct(Arc::new(copied)), _lease: lease,
        }));
        Ok(self.commit(timestamp_ns, full, frame))
    }

    /// Transform an output of an earlier asynchronous stage before admitting it
    /// to this one. The earlier frame (and therefore its credit) remains alive
    /// for the complete callback; its sequence and both clock observations are
    /// copied from the same frame rather than supplied by a newer capture.
    /// Transform scratch is admitted separately by the invoking owner.
    pub fn push_transformed_from_frame<E>(&mut self, input:&DelayedVideoFrame,
        dimensions:(u32,u32), transform:impl FnOnce(&RgbaFrame)->Result<RgbaFrame,E>)
        ->Result<Option<DelayedVideoFrame>,AsyncTransformError<E>> {
        let sequence=input.sequence().ok_or(AsyncTransformError::Delay(AsyncDelayError::InvalidLimits))?;
        self.push_transformed_with_context(input.timestamp_ns(),sequence,input.device_qpc_ns(),
            input.directshow_clock(),input.context().cloned(),dimensions,||transform(input.image()))
    }

    /// A source stage can replace its immutable metadata alongside newly
    /// transformed pixels while retaining the original sample clock.
    pub fn push_transformed_from_frame_with_context<E>(&mut self, input:&DelayedVideoFrame,
        context:Option<DelayedFrameContext>, dimensions:(u32,u32),
        transform:impl FnOnce(&RgbaFrame)->Result<RgbaFrame,E>)
        ->Result<Option<DelayedVideoFrame>,AsyncTransformError<E>> {
        let sequence=input.sequence().ok_or(AsyncTransformError::Delay(AsyncDelayError::InvalidLimits))?;
        self.push_transformed_with_context(input.timestamp_ns(),sequence,input.device_qpc_ns(),
            input.directshow_clock(),context,dimensions,||transform(input.image()))
    }

    fn push_inner(&mut self, timestamp_ns: i128, sequence: Option<u64>, image: SharedVideoFrame)
        -> Result<Option<DelayedVideoFrame>, (AsyncDelayError, SharedVideoFrame)> {
        let (full, lease) = match self.admit(timestamp_ns, image.byte_len()) {
            Ok(value) => value,
            Err(error) => return Err((error, image)),
        };
        let device_qpc_ns = match &image.0 { FrameStorage::Delayed(frame) => frame.device_qpc_ns(), FrameStorage::Direct(_) | FrameStorage::Leased(_) => None };
        let directshow_clock = match &image.0 { FrameStorage::Delayed(frame) => frame.directshow_clock(), FrameStorage::Direct(_) | FrameStorage::Leased(_) => None };
        let context = match &image.0 { FrameStorage::Delayed(frame) => frame.context().cloned(), FrameStorage::Direct(_) | FrameStorage::Leased(_) => None };
        let frame = DelayedVideoFrame(Arc::new(TrackedFrame { timestamp_ns, sequence, device_qpc_ns, directshow_clock, context, image, _lease: lease }));
        Ok(self.commit(timestamp_ns, full, frame))
    }

    fn admit(&mut self, timestamp_ns: i128, pixel_bytes: usize) -> Result<(bool, Lease), AsyncDelayError> {
        if self.last_timestamp_ns.is_some_and(|last| timestamp_ns < last
            || timestamp_ns.checked_sub(last).is_none_or(|gap| gap > MAX_TIMESTAMP_GAP_NS)) {
            self.reset();
        }
        // Once the oldest frame is due, a full queue can exchange it for the
        // incoming frame without allocating an extra VecDeque slot. Keep the
        // output lease alive, and still reserve the new frame before mutation.
        let full = self.pending.len() >= self.max_pending;
        let due = self.reached || self.pending.front().is_some_and(|oldest| {
            timestamp_ns.checked_sub(oldest.timestamp_ns())
                .is_none_or(|age| age >= self.interval_ns)
        });
        if full && !due { return Err(AsyncDelayError::BudgetExceeded); }
        let bytes = match pixel_bytes.checked_add(
            std::mem::size_of::<TrackedFrame>() + std::mem::size_of::<RgbaFrame>() + 4 * std::mem::size_of::<usize>()
        ) {
            Some(value) => value, None => return Err(AsyncDelayError::BudgetExceeded),
        };
        let lease = self.budget.reserve(bytes, 1)?;
        Ok((full, lease))
    }

    fn commit(&mut self, timestamp_ns: i128, full: bool, frame: DelayedVideoFrame) -> Option<DelayedVideoFrame> {
        self.last_timestamp_ns = Some(timestamp_ns);
        if full {
            let output = self.pending.pop_front().expect("full delay queue has a frame");
            self.pending.push_back(frame);
            self.reached = true;
            return Some(output);
        }
        self.pending.push_back(frame);
        let oldest = self.pending.front().expect("frame was just admitted").timestamp_ns();
        // On a valid monotonic stream the subtraction is nonnegative. If the
        // representable span is exceeded, it is necessarily past any 20s delay.
        if !self.reached && timestamp_ns.checked_sub(oldest).is_some_and(|age| age < self.interval_ns) { return None; }
        self.reached = true;
        self.pending.pop_front()
    }
}
