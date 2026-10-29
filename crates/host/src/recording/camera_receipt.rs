//! Metadata admitted only after one recording owner accepted a rendered frame.
//! This is not a shared Program-output or external presentation acknowledgement.
use super::owner::RecordingFrame;
use crate::capture::AsyncDelayReceipt;
use skylive_core::Id;
use skylive_media::MediaTime;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug)]
pub struct CameraReceipt {
    pub recording: u64,
    pub session: u64,
    pub revision: u64,
    pub source: Id,
    pub submitted_at: MediaTime,
    /// Recording's original media timeline for the accepted video sample.
    /// Pause removal is accounted for; this is not wall-clock encoder latency.
    pub presentation_at: MediaTime,
    /// Exclusive end of this accepted video frame on the original media grid.
    pub presentation_end: MediaTime,
    pub video_index: u64,
    /// Inclusive bounds in the QPC epoch. Media Foundation may provide a
    /// single device timestamp; DirectShow supplies a conservative interval
    /// from the graph clock. Consumers must not collapse an interval unless
    /// both endpoints map to the same audio sample, unless they explicitly
    /// retain a bounded estimate and its uncertainty.
    pub capture_qpc_bounds_ns: (u64, u64),
    pub(crate) delay: AsyncDelayReceipt,
}

impl CameraReceipt {
    /// A result remains useful only while the camera binding generation is
    /// still active. Consumers must also verify their own session and sink.
    pub fn current(&self) -> bool {
        self.delay.history_live.load(Ordering::Acquire) && self.delay.live_generation.as_ref().is_some_and(|live|
            live.load(Ordering::Acquire) == self.delay.binding_generation)
    }
    pub fn elapsed_ns(&self) -> u64 { self.delay.elapsed_ns }
    pub fn sequence(&self) -> u64 { self.delay.sequence }
    /// Compare the authored camera history without comparing individual
    /// samples. This lets the recording retain its first accepted frame for
    /// startup alignment while the current stream continues to publish.
    pub(crate) fn same_scope(&self, other:&Self)->bool {
        self.recording==other.recording && self.session==other.session
            && self.revision==other.revision && self.source==other.source
            && self.delay.binding_generation==other.delay.binding_generation
            && self.delay.owner==other.delay.owner && self.delay.stream==other.delay.stream
            && self.delay.clock_epoch==other.delay.clock_epoch && self.delay.filters==other.delay.filters
            && self.delay.history_generation==other.delay.history_generation
            && std::sync::Arc::ptr_eq(&self.delay.history_live,&other.delay.history_live)
    }
}

/// Called after the writer accepted a diagnostic-free rendered frame. The
/// trace comes from that exact composition, including cache hits; a source
/// merely present in an endpoint is not proof that it contributed pixels.
pub(super) fn accepted_samples(frame: &RecordingFrame, trace:&[Id], recording: u64, video_index: u64, presentation_at: MediaTime, presentation_end:MediaTime) -> Vec<CameraReceipt> {
    if recording == 0 || frame.capture.session() != frame.session { return Vec::new(); }
    let mut receipts = Vec::new();
    if trace.len()>skylive_core::MAX_SOURCES || receipts.try_reserve(trace.len()).is_err() { return Vec::new(); }
    let providers = crate::video_filters::FilterProviders::from_snapshot(&frame.model);
    for view in frame.output.scenes() {
        let Ok(sources) = view.visible_sources(&frame.model) else { return Vec::new(); };
        for source in sources {
            if !trace.contains(&source.id()) || receipts.iter().any(|receipt:&CameraReceipt|receipt.source==source.id()) { continue; }
            let Some(sample) = frame.capture.frame(source.id()) else { continue; };
            let bounds = sample.device_qpc_ns.map(|qpc|(qpc,qpc)).or_else(|| {
                sample.directshow_clock?.qpc_interval(sample.native_timestamp_ns?)
            });
            let Some(bounds) = bounds else { continue; };
            let Some(delay) = sample.async_delay.as_ref() else { continue; };
            if !crate::video_filters::camera_delay_applied(source, frame.session, sample, &sample.image, providers) { continue; }
            receipts.push(CameraReceipt { recording, session:frame.session, revision:frame.model.revision(),
                source:source.id(), submitted_at:frame.time, presentation_at, presentation_end, video_index, capture_qpc_bounds_ns:bounds, delay:delay.clone() });
        }
    }
    receipts
}
