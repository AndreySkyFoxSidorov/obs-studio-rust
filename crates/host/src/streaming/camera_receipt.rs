//! Camera evidence for a specific streaming connection and encoded frame.
//! Capture bounds remain explicit; they are not a presentation timestamp.
use super::{ProgramVideoFrame, Status, StreamKey};
use skylive_core::Id;
use skylive_media::MediaTime;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[derive(Clone, Debug)]
pub struct CameraVideoReceipt {
    pub key: StreamKey,
    pub connection_epoch: u64,
    pub revision: u64,
    pub source: Id,
    pub video_index: u64,
    pub presentation_at: MediaTime,
    pub presentation_end: MediaTime,
    pub capture_qpc_bounds_ns: (u64, u64),
    pub(crate) delay: crate::capture::AsyncDelayReceipt,
    live_output: Arc<AtomicBool>,
}
impl CameraVideoReceipt {
    pub fn current(&self) -> bool {
        self.live_output.load(Ordering::Acquire) && self.delay.current()
    }
    pub(crate) fn connection_current(&self) -> bool { self.live_output.load(Ordering::Acquire) }
    pub(crate) fn same_scope(&self, other: &Self) -> bool {
        self.key == other.key && self.connection_epoch == other.connection_epoch
            && self.revision == other.revision && self.source == other.source
            && self.delay.owner == other.delay.owner && self.delay.stream == other.delay.stream
            && self.delay.binding_generation == other.delay.binding_generation
            && self.delay.clock_epoch == other.delay.clock_epoch
            && self.delay.filters == other.delay.filters
            && self.delay.history_generation == other.delay.history_generation
            && Arc::ptr_eq(&self.delay.history_live, &other.delay.history_live)
    }
}

/// An encoded packet proves only its exact submitted PTS, never delivery to a viewer.
pub(super) fn accept(status: &mut Status,
    pending: &mut std::collections::VecDeque<(i64, Vec<CameraVideoReceipt>)>, pts: i64) {
    while pending.front().is_some_and(|(old, _)| *old < pts) { pending.pop_front(); }
    let mut values = if pending.front().is_some_and(|(old, _)| *old == pts) {
        pending.pop_front().map(|(_, values)| values).unwrap_or_default()
    } else { Vec::new() };
    values.retain(CameraVideoReceipt::current);
    status.first_camera_video_receipts.retain(|old| old.current() && values.iter().any(|v| old.same_scope(v)));
    let mut failed = false;
    for value in &values {
        if !status.first_camera_video_receipts.iter().any(|old| old.same_scope(value)) {
            if status.first_camera_video_receipts.try_reserve(1).is_err() {
                failed = true; break;
            }
            status.first_camera_video_receipts.push(value.clone());
        }
    }
    if failed { status.first_camera_video_receipts.clear(); values.clear(); }
    status.camera_video_receipts = values;
}

pub(super) fn candidates(frame: &ProgramVideoFrame, trace: &[Id], key: StreamKey,
    epoch: u64, index: u64, at: MediaTime, end: MediaTime,
    live_output: &Arc<AtomicBool>) -> Vec<CameraVideoReceipt> {
    if key.session != frame.session || frame.capture.session() != frame.session
        || trace.len() > skylive_core::MAX_SOURCES { return Vec::new(); }
    let mut values = Vec::new();
    if values.try_reserve(trace.len()).is_err() { return values; }
    let providers = crate::video_filters::FilterProviders::from_snapshot(&frame.model);
    for view in frame.output.scenes() {
        let Ok(sources) = view.visible_sources(&frame.model) else { return Vec::new(); };
        for source in sources {
            if !trace.contains(&source.id()) || values.iter().any(|v: &CameraVideoReceipt| v.source == source.id()) { continue; }
            let Some(sample) = frame.capture.frame(source.id()) else { continue; };
            if !crate::video_filters::camera_delay_applied(source, frame.session, sample, &sample.image, providers) { continue; }
            let bounds = sample.device_qpc_ns.map(|qpc| (qpc, qpc)).or_else(||
                sample.directshow_clock?.qpc_interval(sample.native_timestamp_ns?));
            let (Some(capture_qpc_bounds_ns), Some(delay)) = (bounds, sample.async_delay.as_ref()) else { continue; };
            values.push(CameraVideoReceipt { key, connection_epoch: epoch, revision: frame.model.revision(),
                source: source.id(), video_index: index, presentation_at: at, presentation_end: end,
                capture_qpc_bounds_ns, delay: delay.clone(), live_output: live_output.clone() });
        }
    }
    values
}
