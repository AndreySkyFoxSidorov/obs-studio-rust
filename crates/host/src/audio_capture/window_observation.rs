//! Runtime evidence that a particular capture source produced a window frame.
//! It never changes the durable audio selector, and must not outlive its scope.
use skylive_core::{Id, WindowCaptureBinding};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowAudioObservation {
    session: u64,
    source: Id,
    capture_epoch: u64,
    binding: WindowCaptureBinding,
    backend: crate::window_capture::WindowCaptureBackendKind,
    backend_generation: u64,
    native: crate::window_capture::NativeWindowIdentity,
}

impl WindowAudioObservation {
    /// Produce evidence only from the current session's successful frame and
    /// the exact authored binding. A capture error, pending/no frame, synthetic
    /// backend or stale binding cannot authorize opening an audio process.
    /// A caller must re-evaluate this against each new capture snapshot and
    /// retire the associated audio generation before publishing stale PCM.
    pub fn from_snapshot(session: u64, source: Id, binding: &WindowCaptureBinding,
        capture: &crate::capture::CaptureSnapshot) -> Option<Self> {
        if session == 0 || capture.session() != session || capture.epoch() == 0
            || !binding.capture_audio() || capture.problem(source).is_some() { return None; }
        let frame = capture.frame(source)?;
        if frame.binding != *binding { return None; }
        let metadata = frame.window.as_ref()?;
        let native = metadata.native_window?;
        if native.handle == 0 || native.process_id == 0 || native.process_creation_time_100ns.is_none_or(|value|value==0) || metadata.generation == 0 { return None; }
        Some(Self { session, source, capture_epoch: capture.epoch(), binding: binding.clone(),
            backend: metadata.backend, backend_generation: metadata.generation, native })
    }
    pub fn session(&self) -> u64 { self.session }
    pub fn source(&self) -> Id { self.source }
    pub fn binding(&self) -> &WindowCaptureBinding { &self.binding }
    pub fn native(&self) -> crate::window_capture::NativeWindowIdentity { self.native }
}

/// A single source-scoped runtime generation. The frontend retires the token
/// immediately when its observation changes, so already queued jobs can no
/// longer authorize the old process. A future PCM route must carry this same
/// token all the way to the mixer and check it before consuming buffered data.
#[derive(Clone, Debug)]
pub(crate) struct WindowAudioEvidence {
    observation: WindowAudioObservation,
    live: Arc<AtomicBool>,
}
impl WindowAudioEvidence {
    pub(crate) fn new(observation: WindowAudioObservation) -> Self {
        Self { observation, live: Arc::new(AtomicBool::new(true)) }
    }
    pub(crate) fn observation(&self) -> &WindowAudioObservation { &self.observation }
    pub(crate) fn current(&self) -> bool { self.live.load(Ordering::Acquire) }
    pub(crate) fn same_generation(&self,other:&Self) -> bool { Arc::ptr_eq(&self.live,&other.live) }
    pub(crate) fn retire(&self) { self.live.store(false, Ordering::Release); }
}
