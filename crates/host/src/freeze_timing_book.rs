//! Bounded control-owner table for Freeze timers. The surrounding owner must
//! charge each allocation through the shared source-filter budget and connect
//! the returned intents to real application/worker gates.
use std::sync::Arc;
use skylive_core::{Id, VideoFilterId};
pub use super::freeze_timing_state::Due;
use super::freeze_timing_state::{TimingError, TimingState};

pub const MAX_TARGETS: usize = 128;
pub(super) const MAX_CONTROL_BYTES: usize = 512 * 1024;
pub(super) const MAX_EXPIRATIONS_PER_UPDATE: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instance {
    Canonical { filter: VideoFilterId },
    Draft { window: u64, row: u32 },
    DraftPrivate { window:u64, owner:u64, filter:VideoFilterId },
    Private { owner: u64, filter: VideoFilterId },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group { Canonical, Draft, Private }
impl Instance {
    fn group(self)->Group { match self { Self::Canonical { .. }=>Group::Canonical, Self::Draft { .. }|Self::DraftPrivate { .. }=>Group::Draft, Self::Private { .. }=>Group::Private } }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target { pub session: u64, pub source: Id, pub instance: Instance }

/// A definition generation is an owner-issued scalar that changes whenever
/// the accepted action/worker configuration changes. `enabled` is the actual
/// current state of this instance, not a request or optimistic checkbox value.
#[derive(Clone, Copy)]
pub struct Definition {
    pub target: Target,
    pub generation: u64,
    pub enabled: bool,
    pub duration_ms: u32,
    pub refresh_ms: u32,
}
#[derive(Clone, Copy)]
pub struct Intent {
    pub target: Target,
    pub definition_generation: u64,
    pub due: Due,
}
pub struct IntentBatch { pub values:Vec<Intent>, _credit:Option<Arc<dyn Send+Sync>> }
#[derive(Clone, Copy)]
pub(super) struct CapturePlan { pub generation: u64, pub run: u64, pub request: Option<u64> }
struct Entry { definition: Definition, state: TimingState, private_expired: bool }

#[derive(Debug)]
pub(super) enum Error { Limit, Duplicate, InvalidIdentity, Time(TimingError) }
impl From<TimingError> for Error { fn from(value: TimingError) -> Self { Self::Time(value) } }

#[derive(Default)]
pub(super) struct Book {
    entries: Vec<Entry>,
    generation: u64,
    next_run: u64,
    failures:u64,
    // The lease must reserve global bytes and the shared local control ceiling.
    // Dropping a replacement releases it only after its old allocation retires.
    lease: Option<Arc<dyn Send + Sync>>,
}
impl Book {
    /// Input definitions are borrowed; no whole model, native frame, or file
    /// path is cloned. `reserve` must atomically admit old+replacement capacity
    /// before allocation and return an owned credit for the new allocation.
    pub(super) fn observe<F>(&mut self, group: Group, definitions: &[Definition], now: u64, mut reserve: F) -> Result<(), Error>
    where F: FnMut(usize) -> Option<Arc<dyn Send + Sync>> {
        if definitions.len() > MAX_TARGETS { return Err(Error::Limit); }
        for (index, def) in definitions.iter().enumerate() {
            if def.target.instance.group()!=group || def.target.session == 0 || def.generation == 0 || match def.target.instance {
                Instance::Canonical { .. } => false,
                Instance::Draft { window, row } => window == 0 || row == 0,
                Instance::DraftPrivate { window, owner, .. } => window == 0 || owner == 0,
                Instance::Private { owner, .. } => owner == 0,
            } { return Err(Error::InvalidIdentity); }
            if definitions[..index].iter().any(|other| other.target == def.target) { return Err(Error::Duplicate); }
        }
        // Check all clock inputs before any mutation, so one stale observation
        // cannot leave a partially updated table.
        let mut new_runs=0u64;
        for def in definitions {
            if let Some(entry) = self.entries.iter().find(|entry| entry.definition.target == def.target) {
                let expired = entry.private_expired && entry.definition.enabled && def.enabled;
                let enabled=def.enabled&&!expired;
                entry.state.can_observe(enabled, now)?;
                if enabled&&entry.state.run().is_none(){new_runs+=1;}
            }else if def.enabled{
                new_runs+=1;
            }
        }
        self.next_run.checked_add(new_runs).ok_or(Error::Time(TimingError::IdentityExhausted))?;
        let new = definitions.iter().filter(|def| !self.entries.iter().any(|entry| entry.definition.target == def.target)).count();
        let retained = self.entries.iter().filter(|entry| entry.definition.target.instance.group()!=group || definitions.iter().any(|def| def.target == entry.definition.target)).count();
        let required = retained.checked_add(new).ok_or(Error::Limit)?;
        if required > MAX_TARGETS { return Err(Error::Limit); }
        // Removed targets are no longer authorized even if admission of new
        // demand fails; retire them before reserving replacement capacity.
        self.entries.retain(|entry| entry.definition.target.instance.group()!=group || definitions.iter().any(|def| def.target == entry.definition.target));
        if required > self.entries.capacity() {
            let capacity = self.entries.capacity().max(2).saturating_mul(2).min(MAX_TARGETS).max(required);
            let admitted=capacity.checked_mul(2).ok_or(Error::Limit)?;
            let bytes = admitted.checked_mul(std::mem::size_of::<Entry>()).filter(|bytes| *bytes <= MAX_CONTROL_BYTES).ok_or(Error::Limit)?;
            let replacement = reserve(bytes).ok_or(Error::Limit)?;
            self.entries.try_reserve_exact(capacity - self.entries.len()).map_err(|_| Error::Limit)?;
            if self.entries.capacity() > admitted { self.entries=Vec::new();self.lease=None;return Err(Error::Limit); }
            self.lease = Some(replacement);
        }
        for definition in definitions {
            if let Some(entry) = self.entries.iter_mut().find(|entry| entry.definition.target == definition.target) {
                if entry.definition.generation != definition.generation { entry.state.definition_changed(); }
                if !entry.definition.enabled || !definition.enabled { entry.private_expired = false; }
                let effective = definition.enabled && !entry.private_expired;
                let run=if effective&&entry.state.run().is_none(){self.next_run+=1;self.next_run}else{0};
                entry.state.observe(effective, now, definition.duration_ms, definition.refresh_ms,run)?;
                entry.definition = *definition;
            } else {
                let mut state = TimingState::default();
                let run=if definition.enabled{self.next_run+=1;self.next_run}else{0};
                state.observe(definition.enabled, now, definition.duration_ms, definition.refresh_ms,run)?;
                self.entries.push(Entry { definition: *definition, state, private_expired: false });
            }
        }
        if self.entries.is_empty() { self.entries = Vec::new(); self.lease = None; }
        Ok(())
    }

    /// Admit the actual batch before issuing intents. Only the first16 expiry
    /// commands are issued per update; later deadlines remain due.
    pub(super) fn due<F>(&mut self, now: u64, mut reserve:F) -> Result<IntentBatch, Error>
    where F:FnMut(usize)->Option<Arc<dyn Send+Sync>> {
        // A project-clock rewind is a privacy discontinuity, not a new
        // Enabled run. Preserve elapsed enabled time and retire capture state.
        for entry in &self.entries{let enabled=entry.definition.enabled&&!entry.private_expired;entry.state.can_observe(enabled,now)?;}
        for entry in &mut self.entries{let enabled=entry.definition.enabled&&!entry.private_expired;entry.state.observe(enabled,now,entry.definition.duration_ms,entry.definition.refresh_ms,0)?;}
        for entry in &self.entries { entry.state.can_evaluate(now)?; }
        let mut needed:usize=0;let mut expiry_count=0;
        for entry in &self.entries{
            if expiry_count==MAX_EXPIRATIONS_PER_UPDATE{break;}
            match entry.state.due_kind(now){0=>{},1=>{needed+=1;expiry_count+=1;},_=>needed+=1}
        }
        if needed==0{return Ok(IntentBatch{values:Vec::new(),_credit:None});}
        let admitted=needed.checked_mul(2).ok_or(Error::Limit)?;
        let bytes=admitted.checked_mul(std::mem::size_of::<Intent>()).and_then(|n|n.checked_add(64)).filter(|n|*n<=MAX_CONTROL_BYTES).ok_or(Error::Limit)?;
        let credit=reserve(bytes).ok_or(Error::Limit)?;let mut output=Vec::new();output.try_reserve_exact(needed).map_err(|_|Error::Limit)?;
        if output.capacity()>admitted{return Err(Error::Limit);}
        let mut expirations = 0;
        for entry in &mut self.entries {
            // Do not issue more expiration intents than the owner can publish.
            // A refresh is also held back at this point, preserving expiry-first
            // order for every enabled run on the next update.
            if expirations == MAX_EXPIRATIONS_PER_UPDATE { break; }
            let due = entry.state.due(now)?;
            if due == Due::None { continue; }
            if matches!(due, Due::Disable { .. }) { expirations += 1; }
            output.push(Intent { target: entry.definition.target, definition_generation: entry.definition.generation, due });
        }
        Ok(IntentBatch{values:output,_credit:Some(credit)})
    }

    pub(super) fn deadline(&self) -> Option<u64> { self.entries.iter().filter_map(|entry| entry.state.deadline()).min() }
    pub(super) fn state_mut(&mut self, target: Target, generation: u64) -> Option<&mut TimingState> {
        self.entries.iter_mut().find(|entry| entry.definition.target == target && entry.definition.generation == generation).map(|entry| &mut entry.state)
    }
    pub(super) fn retire_capture(&mut self, target: Target) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.definition.target == target) {
            if entry.state.capture_retired(){self.generation=self.generation.saturating_add(1);}
        }
    }
    pub(super) fn generation(&self)->u64{self.generation}
    pub(super) fn failures(&self)->u64{self.failures}
    pub(super) fn can_bump(&self)->bool{self.generation!=u64::MAX}
    pub(super) fn bump(&mut self){self.generation+=1;}
    pub(super) fn capture_plan(&self, target: Target) -> Option<CapturePlan> {
        let entry = self.entries.iter().find(|entry| entry.definition.target == target)?;
        let (run, request) = entry.state.capture_request()?;
        Some(CapturePlan { generation: entry.definition.generation, run, request })
    }
    pub(super) fn effective(&self, target: Target) -> Option<bool> {
        self.entries.iter().find(|entry| entry.definition.target == target)
            .map(|entry| entry.definition.enabled && !entry.private_expired)
    }
    pub(super) fn run(&self,target:Target)->Option<u64>{self.entries.iter().find(|entry|entry.definition.target==target)?.state.run()}
    pub(super) fn refresh_enabled(&self,target:Target)->bool{self.entries.iter().find(|entry|entry.definition.target==target).is_some_and(|entry|entry.definition.enabled&&!entry.private_expired&&entry.definition.refresh_ms!=0)}
    pub(super) fn cache_signature(&self,target:Target)->Option<[u64;4]>{let entry=self.entries.iter().find(|entry|entry.definition.target==target)?;let(run,epoch,request)=entry.state.cache_signature()?;Some([entry.definition.generation,run,epoch,request])}
    pub(super) fn allows_sample(&self,target:Target,at:u64)->Option<bool>{self.entries.iter().find(|entry|entry.definition.target==target).map(|entry|entry.state.allows_sample(at))}
    pub(super) fn captured_at(&self,target:Target)->Option<u64>{self.entries.iter().find(|entry|entry.definition.target==target)?.state.captured_at()}
    pub(super) fn capture_failed(&mut self,target:Target,generation:u64,run:u64,request:Option<u64>){
        let Some(entry)=self.entries.iter_mut().find(|entry|entry.definition.target==target&&entry.definition.generation==generation)else{return;};
        let first=!entry.state.failure();let now=entry.state.last_time();
        if entry.state.capture_failed(run,request,now).unwrap_or(false)&&first{self.failures=self.failures.saturating_add(1);}
    }
    pub(super) fn draw_failed(&mut self,target:Target){
        let Some(entry)=self.entries.iter_mut().find(|entry|entry.definition.target==target)else{return;};
        let now=entry.state.last_time();if entry.state.draw_failed(now){self.failures=self.failures.saturating_add(1);}
    }
    pub(super) fn expire_private(&mut self, intent: Intent, now: u64) -> bool {
        let Due::Disable { run } = intent.due else { return false; };
        if !matches!(intent.target.instance, Instance::Private { .. }|Instance::DraftPrivate { .. }) { return false; }
        if !self.can_bump(){return false;}
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.definition.target == intent.target && entry.definition.generation == intent.definition_generation) else { return false; };
        if entry.state.can_observe(false, now).is_err() || !entry.state.accept_disable(run) { return false; }
        if entry.state.observe(false, now, entry.definition.duration_ms, entry.definition.refresh_ms,0).is_err() { return false; }
        entry.private_expired = true;
        self.bump();
        true
    }
    /// Use under the same synchronization protecting `Book`, immediately at
    /// the canonical publication gate or worker receipt linearization point.
    pub(super) fn is_pending(&self, intent: Intent) -> bool {
        self.entries.iter().find(|entry| entry.definition.target == intent.target && entry.definition.generation == intent.definition_generation)
            .is_some_and(|entry| match intent.due {
                Due::None => false,
                Due::Disable { run } => entry.state.disable_pending(run),
                Due::Refresh { run, request } => entry.state.refresh_pending(run, request),
                Due::RetryCapture { .. } => false,
                Due::RetryRender { .. } => false,
            })
    }
    /// Called exactly once at Session's final publication gate while holding
    /// the book lock; rejected or stale intents cannot be accepted twice.
    pub(super) fn accept_disable(&mut self, intent: Intent) -> bool {
        let Due::Disable { run } = intent.due else { return false; };
        self.state_mut(intent.target, intent.definition_generation).is_some_and(|state| state.accept_disable(run))
    }
}
