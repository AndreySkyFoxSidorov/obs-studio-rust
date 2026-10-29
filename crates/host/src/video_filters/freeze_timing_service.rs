//! Control metadata uses the same budget and owner as temporal pixels.
//! This module has no device, worker thread, timer, model or frame ownership.
use super::*;
use crate::freeze_timing_book::{self as control, Definition, Intent, Target};
use crate::freeze_timing_state::Due;

struct ControlCredit { local: Arc<AtomicUsize>, _global: Lease, bytes: usize }
impl Drop for ControlCredit {
    fn drop(&mut self) { self.local.fetch_sub(self.bytes, Ordering::AcqRel); }
}
pub(super) fn reserve(local: &Arc<AtomicUsize>, global: &Arc<Budget>, bytes: usize) -> Option<Arc<dyn Send + Sync>> {
    local.fetch_update(Ordering::AcqRel, Ordering::Acquire,
        |used| used.checked_add(bytes).filter(|sum| *sum <= control::MAX_CONTROL_BYTES)).ok()?;
    let Some(lease) = Lease::acquire(global, bytes) else {
        local.fetch_sub(bytes, Ordering::AcqRel);
        return None;
    };
    Some(Arc::new(ControlCredit { local: local.clone(), _global: lease, bytes }))
}
fn issue(error: control::Error) -> FilterError { limited(format!("Freeze timer control: {error:?}")) }

impl Runtime {
    pub(crate) fn freeze_observe(&self, group:control::Group, definitions: &[Definition], now: u64) -> Result<(), FilterError> {
        let shared = &self.life.shared;
        let mut state = shared.state.lock().map_err(|_| unavailable("Freeze timer state poisoned"))?;
        let local = state.freeze_timer_bytes.clone();
        state.freeze_timers.observe(group, definitions, now, |bytes| reserve(&local, &shared.budget, bytes)).map_err(issue)
    }
    pub(crate) fn freeze_due(&self, now: u64) -> Result<control::IntentBatch, FilterError> {
        let mut state = self.life.shared.state.lock().map_err(|_| unavailable("Freeze timer state poisoned"))?;
        let local=state.freeze_timer_bytes.clone();let budget=&self.life.shared.budget;
        state.freeze_timers.due(now,|bytes|reserve(&local,budget,bytes)).map_err(issue)
    }
    pub(crate) fn freeze_deadline(&self) -> Result<Option<u64>, FilterError> {
        let state = self.life.shared.state.lock().map_err(|_| unavailable("Freeze timer state poisoned"))?;
        Ok(state.freeze_timers.deadline())
    }
    pub(crate) fn freeze_generation(&self)->Option<u64>{self.life.shared.state.try_lock().ok().map(|state|state.freeze_timers.generation())}
    pub(crate) fn freeze_failures(&self)->Option<u64>{self.life.shared.state.try_lock().ok().map(|state|state.freeze_timers.failures())}
    pub(crate) fn freeze_credit(&self,bytes:usize)->Result<Arc<dyn Send+Sync>,FilterError>{
        let state=self.life.shared.state.lock().map_err(|_|unavailable("Freeze timer state poisoned"))?;
        reserve(&state.freeze_timer_bytes,&self.life.shared.budget,bytes).ok_or_else(||limited("Freeze control metadata exceeds shared budget"))
    }
    pub(crate) fn freeze_accept_disable(&self, intent: Intent) -> bool {
        self.life.shared.state.lock().ok().is_some_and(|mut state| state.freeze_timers.accept_disable(intent))
    }
    pub(crate) fn freeze_reject_disable(&self, intent: Intent) {
        let Due::Disable { run } = intent.due else { return; };
        if let Ok(mut state) = self.life.shared.state.lock() {
            if let Some(timer) = state.freeze_timers.state_mut(intent.target, intent.definition_generation) { timer.disable_rejected(run); }
        }
    }
    pub(crate) fn freeze_pending(&self, intent: Intent) -> bool {
        self.life.shared.state.lock().ok().is_some_and(|state| state.freeze_timers.is_pending(intent))
    }
    pub(crate) fn freeze_expire_private(&self, intent: Intent, now:u64) -> bool {
        self.life.shared.state.lock().ok().is_some_and(|mut state| state.freeze_timers.expire_private(intent,now))
    }
    pub(crate) fn freeze_effective(&self, target: Target) -> Option<bool> {
        self.life.shared.state.lock().ok()?.freeze_timers.effective(target)
    }
    pub(crate) fn freeze_retire_capture(&self, target: Target, generation: u64) {
        if let Ok(mut state) = self.life.shared.state.lock() {
            if let Some(timer) = state.freeze_timers.state_mut(target, generation) { timer.capture_retired(); }
        }
    }
}

impl Shared {
    pub(super) fn freeze_run(&self,target:Target)->Option<u64>{self.state.lock().ok()?.freeze_timers.run(target)}
    pub(super) fn freeze_effective(&self, target: Target) -> Option<bool> {
        self.state.lock().ok()?.freeze_timers.effective(target)
    }
    pub(super) fn freeze_retire_target(&self, target: Target) {
        if let Ok(mut state) = self.state.lock() { state.freeze_timers.retire_capture(target); }
    }
    pub(super) fn freeze_capture_plan(&self, target: Target) -> Option<control::CapturePlan> {
        self.state.lock().ok()?.freeze_timers.capture_plan(target)
    }
    pub(super) fn freeze_refresh_failed(&self, target: Target, plan: control::CapturePlan) {
        if let Ok(mut state) = self.state.lock() {
            state.freeze_timers.capture_failed(target,plan.generation,plan.run,plan.request);
        }
    }
    pub(super) fn freeze_draw_failed(&self,target:Target){if let Ok(mut state)=self.state.lock(){state.freeze_timers.draw_failed(target);}}
    /// Atomically authorize an already-prepared frame installation and its
    /// scalar receipt. `current` must check actual desired temporal scope,
    /// config and privacy grant under this same lock. `install` may only swap
    /// preallocated worker-owned metadata; it must return replaced raster
    /// ownership to be dropped after the lock is released.
    pub(super) fn freeze_install<R>(&self, target: Target, generation: u64, run: u64,
        request: Option<u64>, at: u64, current: impl FnOnce(&State) -> bool,
        install: impl FnOnce() -> R) -> Option<R> {
        let mut state = self.state.lock().ok()?;
        if !current(&state) { return None; }
        if !state.freeze_timers.can_bump(){return None;}
        if !state.freeze_timers.state_mut(target, generation)?.can_capture(run, request, at) { return None; }
        let retired = install();
        // The checked timer cannot change while this lock is held.
        let recorded = state.freeze_timers.state_mut(target, generation)?.captured(run, request, at);
        debug_assert!(recorded);
        state.freeze_timers.bump();
        Some(retired)
    }
}
