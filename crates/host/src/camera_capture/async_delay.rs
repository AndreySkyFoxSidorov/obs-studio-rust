//! Source-owned camera delay adapter used before camera snapshot publication.
//!
//! One adapter is required per authored source/filter, even when several
//! sources share a physical camera. The capture owner explicitly binds a new
//! scope before admitting frames; stale work cannot silently switch streams.
//! The returned wrapper owns the pixel credit for its entire lifetime. A
//! consumer must retain that wrapper alongside any borrowed pixels.
use skylive_core::{Id, VideoFilterId, MAX_SOURCES, MAX_VIDEO_FILTERS};
use skylive_media::{AsyncDelayBudget, AsyncDelayError, AsyncVideoDelay, DelayedVideoFrame, RgbaFrame, SharedVideoFrame};
use std::{collections::{HashMap, HashSet}, sync::Arc};

/// Binding generation is supplied by the source owner and must advance when
/// authored source settings change even if the physical device is shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Scope {
    pub session: u64,
    pub source: Id,
    pub binding_generation: u64,
    pub camera_owner: u64,
    pub stream: u64,
    pub clock_epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    StaleScope,
    StaleSequence,
    MissingFilter,
    InvalidPlan,
    Allocation,
    Delay(AsyncDelayError),
}

/// Distinguish filter failure from a stale source or admission failure so a
/// caller can discard the whole source history on either kind of error.
pub(crate) enum StageTransformError<E> { Source(Error), Transform(E) }

/// Retains both the source identity and the lease-bearing delayed frame.
#[derive(Clone)]
pub(crate) struct Frame {
    scope: Scope,
    frame: DelayedVideoFrame,
}
impl Frame {
    pub fn scope(&self) -> Scope { self.scope }
    pub fn sequence(&self) -> Option<u64> { self.frame.sequence() }
    pub fn timestamp_ns(&self) -> i128 { self.frame.timestamp_ns() }
    pub fn device_qpc_ns(&self) -> Option<u64> { self.frame.device_qpc_ns() }
    pub fn directshow_clock(&self) -> Option<skylive_media::DirectShowClockSnapshot> { self.frame.directshow_clock() }
    pub fn context(&self) -> Option<&skylive_media::DelayedFrameContext> { self.frame.context() }
    pub fn image(&self) -> &RgbaFrame { self.frame.image() }
    /// Transfer a clone into a downstream owner without separating the pixels
    /// from their delay-budget credit.
    pub fn shared_image(&self) -> SharedVideoFrame { SharedVideoFrame::delayed(self.frame.clone()) }
}

pub(crate) struct SourceDelay {
    scope: Scope,
    last_sequence: Option<u64>,
    delay: AsyncVideoDelay,
}
impl SourceDelay {
    pub fn new(scope: Scope, delay_ms: u32, max_pending: usize, budget: AsyncDelayBudget)
        -> Result<Self, Error> {
        let delay = AsyncVideoDelay::new(delay_ms, max_pending, budget).map_err(Error::Delay)?;
        Ok(Self { scope, last_sequence: None, delay })
    }

    /// The serialized source owner decides when a new scope is authoritative.
    /// Retiring queued frames never invalidates output wrappers already held.
    pub fn bind(&mut self, scope: Scope) {
        if self.scope != scope {
            self.delay.reset();
            self.last_sequence = None;
            self.scope = scope;
        }
    }

    /// Discard a partially advanced history after any chain admission error.
    fn reset(&mut self) {
        self.delay.reset();
        self.last_sequence = None;
    }

    /// A settings publication starts a fresh OBS delay history.
    pub fn reconfigure(&mut self, delay_ms: u32) -> Result<(), Error> {
        self.delay.reconfigure(delay_ms).map_err(Error::Delay)?;
        self.last_sequence = None;
        Ok(())
    }

    /// Called once per accepted native sample, before latest-frame coalescing.
    /// On rejection the original image is returned with its ownership intact.
    pub fn push(&mut self, scope: Scope, sequence: u64, timestamp_ns: i128, image: Arc<RgbaFrame>)
        -> Result<Option<Frame>, (Error, Arc<RgbaFrame>)> {
        if scope != self.scope { return Err((Error::StaleScope, image)); }
        if self.last_sequence.is_some_and(|last| sequence <= last) {
            return Err((Error::StaleSequence, image));
        }
        let output = self.delay.push_with_sequence(timestamp_ns, sequence, image)
            .map_err(|(error, image)| (Error::Delay(error), image))?;
        self.last_sequence = Some(sequence);
        Ok(output.map(|frame| Frame { scope, frame }))
    }

    /// The returned image retains credits from every upstream delay stage.
    pub fn push_shared(&mut self, scope: Scope, sequence: u64, timestamp_ns: i128, image: SharedVideoFrame)
        -> Result<Option<Frame>, (Error, SharedVideoFrame)> {
        if scope != self.scope { return Err((Error::StaleScope, image)); }
        if self.last_sequence.is_some_and(|last| sequence <= last) {
            return Err((Error::StaleSequence, image));
        }
        let output = self.delay.push_shared_with_sequence(timestamp_ns, sequence, image)
            .map_err(|(error, image)| (Error::Delay(error), image))?;
        self.last_sequence = Some(sequence);
        Ok(output.map(|frame| Frame { scope, frame }))
    }

    /// Evaluate a source-owned stage between delay queues without replacing
    /// the released frame's timestamp, sequence or device-clock evidence with
    /// those of the newer frame that happened to release it.
    pub fn push_transformed<E>(&mut self, input:&Frame, dimensions:(u32,u32),
        transform:impl FnOnce(&RgbaFrame)->Result<RgbaFrame,E>)
        ->Result<Option<Frame>,StageTransformError<E>> {
        if input.scope!=self.scope {return Err(StageTransformError::Source(Error::StaleScope));}
        let sequence=input.sequence().ok_or(StageTransformError::Source(Error::InvalidPlan))?;
        if self.last_sequence.is_some_and(|last|sequence<=last) {
            return Err(StageTransformError::Source(Error::StaleSequence));
        }
        let output=self.delay.push_transformed_from_frame(&input.frame,dimensions,transform)
            .map_err(|error|match error {
                skylive_media::AsyncTransformError::Delay(error)=>StageTransformError::Source(Error::Delay(error)),
                skylive_media::AsyncTransformError::Transform(error)=>StageTransformError::Transform(error),
            })?;
        self.last_sequence=Some(sequence);
        Ok(output.map(|frame|Frame {scope:input.scope,frame}))
    }

    /// Replace source-owned per-frame metadata when the stage produces new pixels.
    pub fn push_transformed_with_context<E>(&mut self,input:&Frame,context:Option<skylive_media::DelayedFrameContext>,
        dimensions:(u32,u32),transform:impl FnOnce(&RgbaFrame)->Result<RgbaFrame,E>)
        ->Result<Option<Frame>,StageTransformError<E>> {
        if input.scope!=self.scope {return Err(StageTransformError::Source(Error::StaleScope));}
        let sequence=input.sequence().ok_or(StageTransformError::Source(Error::InvalidPlan))?;
        if self.last_sequence.is_some_and(|last|sequence<=last) {return Err(StageTransformError::Source(Error::StaleSequence));}
        let output=self.delay.push_transformed_from_frame_with_context(&input.frame,context,dimensions,transform)
            .map_err(|error|match error {
                skylive_media::AsyncTransformError::Delay(error)=>StageTransformError::Source(Error::Delay(error)),
                skylive_media::AsyncTransformError::Transform(error)=>StageTransformError::Transform(error),
            })?;
        self.last_sequence=Some(sequence);
        Ok(output.map(|frame|Frame {scope:input.scope,frame}))
    }

    /// First stage copies only after the delay budget admits the allocation;
    /// the capture owner's resident reader slots remain independent of history.
    pub fn push_copy(&mut self, scope: Scope, sequence: u64, timestamp_ns: i128, device_qpc_ns: Option<u64>, directshow_clock:Option<skylive_media::DirectShowClockSnapshot>, image: &RgbaFrame)
        -> Result<Option<Frame>, Error> {
        // The current first segment is an identity copy. Route it through the
        // same admission and metadata contract future source-owned transforms
        // must use; no resource or neural work runs on the camera thread here.
        self.push_transformed_capture(scope,sequence,timestamp_ns,device_qpc_ns,directshow_clock,
            image,(image.width(),image.height()),|image| {
                let mut bytes=Vec::new();
                bytes.try_reserve_exact(image.byte_len()).map_err(|_|AsyncDelayError::Allocation)?;
                bytes.extend_from_slice(image.pixels());
                RgbaFrame::new(image.width(),image.height(),bytes).map_err(|_|AsyncDelayError::Allocation)
            }).map_err(|error|match error {
                StageTransformError::Source(error)=>error,
                StageTransformError::Transform(error)=>Error::Delay(error),
            })
    }

    /// First-stage counterpart to `push_transformed`: the native input stays
    /// borrowed until the output has been allocated under the delay budget.
    pub fn push_transformed_capture<E>(&mut self, scope:Scope, sequence:u64, timestamp_ns:i128,
        device_qpc_ns:Option<u64>, directshow_clock:Option<skylive_media::DirectShowClockSnapshot>,
        input:&RgbaFrame, dimensions:(u32,u32), transform:impl FnOnce(&RgbaFrame)->Result<RgbaFrame,E>)
        ->Result<Option<Frame>,StageTransformError<E>> {
        self.push_transformed_capture_with_context(scope,sequence,timestamp_ns,device_qpc_ns,directshow_clock,
            None,input,dimensions,transform)
    }

    pub fn push_transformed_capture_with_context<E>(&mut self, scope:Scope, sequence:u64, timestamp_ns:i128,
        device_qpc_ns:Option<u64>, directshow_clock:Option<skylive_media::DirectShowClockSnapshot>,
        context:Option<skylive_media::DelayedFrameContext>, input:&RgbaFrame, dimensions:(u32,u32),
        transform:impl FnOnce(&RgbaFrame)->Result<RgbaFrame,E>) ->Result<Option<Frame>,StageTransformError<E>> {
        if scope!=self.scope {return Err(StageTransformError::Source(Error::StaleScope));}
        if self.last_sequence.is_some_and(|last|sequence<=last) {return Err(StageTransformError::Source(Error::StaleSequence));}
        let output=self.delay.push_transformed_with_context(timestamp_ns,sequence,device_qpc_ns,directshow_clock,
            context,dimensions,||transform(input)).map_err(|error|match error {
                skylive_media::AsyncTransformError::Delay(error)=>StageTransformError::Source(Error::Delay(error)),
                skylive_media::AsyncTransformError::Transform(error)=>StageTransformError::Transform(error),
            })?;
        self.last_sequence=Some(sequence);
        Ok(output.map(|frame|Frame {scope,frame}))
    }
}

/// An authored filter instance and its current source binding. The owner must
/// advance `chain_revision` whenever any upstream filter changes or the stack
/// is reordered, so buffered pixels from an old chain cannot leak into a new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub scope: Scope,
    pub filter: VideoFilterId,
    pub chain_revision: u64,
    pub delay_ms: u32,
    pub max_pending: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key { source: Id, filter: VideoFilterId }
impl Plan {
    fn key(self) -> Key { Key { source: self.scope.source, filter: self.filter } }
}
struct Lane { plan: Plan, delay: SourceDelay }

/// Configuration is reconciled by the serialized capture owner. This layer is
/// not a registered filter or an audio/video synchronization implementation.
pub(crate) struct SourceDelays {
    budget: AsyncDelayBudget,
    lanes: HashMap<Key, Lane>,
}
impl SourceDelays {
    pub fn new(budget: AsyncDelayBudget) -> Self { Self { budget, lanes: HashMap::new() } }

    /// A chain is advanced sequentially, so an error can occur after an
    /// upstream stage consumed the sample. Restart every stage together;
    /// otherwise old pixels can escape after a gap and acquire a misleading
    /// audio alignment receipt. Outputs already held keep their own leases.
    pub fn reset_source(&mut self, source: Id) {
        for (key, lane) in &mut self.lanes {
            if key.source == source { lane.delay.reset(); }
        }
    }
    pub fn reset_all(&mut self) {
        for lane in self.lanes.values_mut() { lane.delay.reset(); }
    }

    /// Commit all changes only after every new queue has been admitted. Old
    /// queues stay usable if allocation or budget admission fails. Outstanding
    /// output frames retain their credits after a lane is retired.
    pub fn reconcile(&mut self, plans: &[Plan]) -> Result<(), Error> {
        if plans.len() > MAX_SOURCES * MAX_VIDEO_FILTERS { return Err(Error::InvalidPlan); }
        let mut seen = HashSet::new();
        seen.try_reserve(plans.len()).map_err(|_| Error::Allocation)?;
        let mut sources: HashMap<Id, (Scope, u64, usize)> = HashMap::new();
        sources.try_reserve(plans.len().min(MAX_SOURCES)).map_err(|_| Error::Allocation)?;
        for plan in plans {
            if plan.scope.source.get() == 0 || plan.chain_revision == 0 || !seen.insert(plan.key()) {
                return Err(Error::InvalidPlan);
            }
            let entry = sources.entry(plan.scope.source).or_insert((plan.scope, plan.chain_revision, 0));
            if entry.0 != plan.scope || entry.1 != plan.chain_revision { return Err(Error::InvalidPlan); }
            entry.2 += 1;
            if entry.2 > MAX_VIDEO_FILTERS || sources.len() > MAX_SOURCES { return Err(Error::InvalidPlan); }
        }
        let mut next = HashMap::new();
        next.try_reserve(plans.len()).map_err(|_| Error::Allocation)?;
        for &plan in plans {
            let key = plan.key();
            if self.lanes.get(&key).is_some_and(|lane| lane.plan == plan) { continue; }
            let delay = SourceDelay::new(plan.scope, plan.delay_ms, plan.max_pending, self.budget.clone())?;
            next.insert(key, Lane { plan, delay });
        }
        // This phase is infallible: capacity was reserved and every unchanged
        // lane was observed before mutating the original map.
        for &plan in plans {
            let key = plan.key();
            if !next.contains_key(&key) {
                let lane = self.lanes.remove(&key).expect("unchanged delay lane exists");
                next.insert(key, lane);
            }
        }
        self.lanes = next;
        Ok(())
    }

    /// Submit once per native sample to each applicable stage, in authored
    /// order. A caller chaining stages must use the returned frame's sequence
    /// and timestamp, not those of the newer sample that released it. On an
    /// error, earlier stages may already have accepted the sample.
    pub fn push(&mut self, filter: VideoFilterId, scope: Scope, sequence: u64,
        timestamp_ns: i128, image: SharedVideoFrame)
        -> Result<Option<Frame>, (Error, SharedVideoFrame)> {
        match self.lanes.get_mut(&Key { source: scope.source, filter }) {
            Some(lane) => lane.delay.push_shared(scope, sequence, timestamp_ns, image),
            None => Err((Error::MissingFilter, image)),
        }
    }

    pub fn push_copy(&mut self, filter: VideoFilterId, scope: Scope, sequence: u64,
        timestamp_ns: i128, device_qpc_ns: Option<u64>, directshow_clock:Option<skylive_media::DirectShowClockSnapshot>, image: &RgbaFrame) -> Result<Option<Frame>, Error> {
        match self.lanes.get_mut(&Key { source: scope.source, filter }) {
            Some(lane) => lane.delay.push_copy(scope, sequence, timestamp_ns, device_qpc_ns, directshow_clock, image),
            None => Err(Error::MissingFilter),
        }
    }

    /// The sequential source worker can admit the result of a real filter
    /// between queues using the released frame's own clock and memory credit.
    /// It must reset the complete source history after any returned error.
    pub fn push_transformed<E>(&mut self, filter:VideoFilterId, input:&Frame, dimensions:(u32,u32),
        transform:impl FnOnce(&RgbaFrame)->Result<RgbaFrame,E>)
        ->Result<Option<Frame>,StageTransformError<E>> {
        match self.lanes.get_mut(&Key {source:input.scope.source,filter}) {
            Some(lane)=>lane.delay.push_transformed(input,dimensions,transform),
            None=>Err(StageTransformError::Source(Error::MissingFilter)),
        }
    }

    /// Apply source-timestamped stages before the first delay without retaining
    /// an uncharged capture frame in that delay's history.
    pub fn push_transformed_capture<E>(&mut self, filter:VideoFilterId, scope:Scope, sequence:u64,
        timestamp_ns:i128, device_qpc_ns:Option<u64>, directshow_clock:Option<skylive_media::DirectShowClockSnapshot>,
        input:&RgbaFrame, dimensions:(u32,u32), transform:impl FnOnce(&RgbaFrame)->Result<RgbaFrame,E>)
        ->Result<Option<Frame>,StageTransformError<E>> {
        match self.lanes.get_mut(&Key {source:scope.source,filter}) {
            Some(lane)=>lane.delay.push_transformed_capture(scope,sequence,timestamp_ns,device_qpc_ns,directshow_clock,input,dimensions,transform),
            None=>Err(StageTransformError::Source(Error::MissingFilter)),
        }
    }

    pub fn push_transformed_with_context<E>(&mut self,filter:VideoFilterId,input:&Frame,
        context:Option<skylive_media::DelayedFrameContext>,dimensions:(u32,u32),
        transform:impl FnOnce(&RgbaFrame)->Result<RgbaFrame,E>)->Result<Option<Frame>,StageTransformError<E>> {
        match self.lanes.get_mut(&Key {source:input.scope.source,filter}) {
            Some(lane)=>lane.delay.push_transformed_with_context(input,context,dimensions,transform),
            None=>Err(StageTransformError::Source(Error::MissingFilter)),
        }
    }

    /// First-stage entry point for a source-owned, budgeted sample snapshot.
    pub fn push_transformed_capture_with_context<E>(&mut self, filter:VideoFilterId, scope:Scope,
        sequence:u64,timestamp_ns:i128,device_qpc_ns:Option<u64>,directshow_clock:Option<skylive_media::DirectShowClockSnapshot>,
        context:Option<skylive_media::DelayedFrameContext>,input:&RgbaFrame,dimensions:(u32,u32),
        transform:impl FnOnce(&RgbaFrame)->Result<RgbaFrame,E>) ->Result<Option<Frame>,StageTransformError<E>> {
        match self.lanes.get_mut(&Key { source:scope.source,filter }) {
            Some(lane)=>lane.delay.push_transformed_capture_with_context(scope,sequence,timestamp_ns,device_qpc_ns,directshow_clock,context,input,dimensions,transform),
            None=>Err(StageTransformError::Source(Error::MissingFilter)),
        }
    }

    /// Advance a source's asynchronous filters once for this native sample.
    /// OBS applies asynchronous filters before the render filters. The first
    /// stage detaches pixels from capture residency; subsequent stages retain
    /// the earlier leases and use the released sample's own identity and PTS.
    /// On failure an earlier stage may already have consumed the sample.
    pub fn push_chain(&mut self, filters: &[VideoFilterId], scope: Scope, sequence: u64,
        timestamp_ns: i128, device_qpc_ns: Option<u64>, directshow_clock:Option<skylive_media::DirectShowClockSnapshot>, image: &RgbaFrame) -> Result<Option<Frame>, Error> {
        let Some((&first, rest)) = filters.split_first() else { return Err(Error::InvalidPlan); };
        let Some(mut frame) = self.push_copy(first, scope, sequence, timestamp_ns, device_qpc_ns, directshow_clock, image)? else { return Ok(None); };
        for &filter in rest {
            let sequence = frame.sequence().ok_or(Error::InvalidPlan)?;
            let timestamp_ns = frame.timestamp_ns();
            let image = frame.shared_image();
            match self.push(filter, scope, sequence, timestamp_ns, image) {
                Ok(Some(next)) => frame = next,
                Ok(None) => return Ok(None),
                Err((error, _)) => return Err(error),
            }
        }
        Ok(Some(frame))
    }
}
