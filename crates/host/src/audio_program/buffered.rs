//! Native-live FIFO rules. Offline absolute-grid evaluation does not use these.
use super::*;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct BufferedReadiness {
    pub minimum_frame: Option<i128>,
    pub pending_inputs: u32,
}
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct BufferedDiscardReport {
    pub discarded_frames: u64,
    pub reset_inputs: u32,
    pub stopped_inputs: u32,
}
pub(super) struct InputState {
    pub pending: bool,
    last_size: usize,
    pending_stop: bool,
}
struct LateAction {
    binding: AudioInputKey,
    frames: usize,
    recovered: bool,
}
impl Default for InputState {
    fn default() -> Self {
        Self {
            pending: true,
            last_size: 0,
            pending_stop: false,
        }
    }
}
impl InputState {
    fn stopped(&mut self, ring: &mut SampleRing) -> usize {
        let size = ring.queue_frames();
        if size == 0 {
            return 0;
        }
        if self.last_size == size {
            if !self.pending_stop {
                self.pending_stop = true;
                return 0;
            }
            ring.clear();
            self.pending_stop = false;
            self.last_size = 0;
            self.pending = true;
            size
        } else {
            self.last_size = size;
            0
        }
    }
}
impl AudioProgram {
    pub(crate) fn new_buffered(
        settings: AudioSettings,
        limits: ProgramAudioLimits,
    ) -> Result<Self, ProgramAudioError> {
        let mut program = Self::new(settings, limits)?;
        program.buffered = true;
        Ok(program)
    }
    pub(crate) fn capture_bindings(&self) -> &[AudioCaptureBinding] {
        &self.capture_bindings
    }
    pub(crate) fn set_capture_bindings(
        &mut self,
        bindings: Vec<AudioCaptureBinding>,
    ) -> Result<(), ProgramAudioError> {
        self.ensure_buffered()?;
        if bindings.len() > 3 * skylive_core::MAX_SOURCES {
            return Err(ProgramAudioError::Limit(
                "native-live binding reference metadata",
            ));
        }
        let mut unique = Vec::new();
        for binding in bindings {
            if !unique.contains(&binding) {
                if unique.len() == 16 {
                    return Err(ProgramAudioError::Limit("native-live capture bindings"));
                }
                unique.push(binding);
            }
        }
        if unique.len() == self.capture_bindings.len()
            && unique
                .iter()
                .all(|binding| self.capture_bindings.contains(binding))
        {
            return Ok(());
        }
        let generation = next(self.data_generation)?;
        self.capture_bindings = unique.into_boxed_slice().into_vec();
        let needed = self.buffered_bindings();
        self.groups.retain(|binding, _| needed.contains(binding));
        self.data_generation = generation;
        Ok(())
    }
    fn ensure_buffered(&self) -> Result<(), ProgramAudioError> {
        if self.buffered {
            Ok(())
        } else {
            Err(ProgramAudioError::Invalid(
                "operation requires native-live FIFO mode",
            ))
        }
    }
    fn buffered_bindings(&self) -> Vec<AudioInputKey> {
        let mut result: Vec<_> = self
            .capture_bindings
            .iter()
            .cloned()
            .map(AudioInputKey::Native)
            .collect();
        for required in self.timeline.required_bindings() {
            if !result.contains(&required.binding) {
                result.push(required.binding.clone());
            }
        }
        result
    }
    /// Peek exactly one source block. Pending flags are derived runtime state;
    /// this does not consume PCM, pull filters, change routes or mutate Session.
    pub(crate) fn buffered_prepare(
        &mut self,
        at: i128,
    ) -> Result<BufferedReadiness, ProgramAudioError> {
        self.ensure_buffered()?;
        at.checked_add(1024)
            .ok_or(ProgramAudioError::TimeOverflow)?;
        let needed:Vec<_> = self.buffered_bindings().into_iter().filter(|binding| !self.histories.has_binding(binding)).collect();
        let mut rows = Vec::with_capacity(needed.len());
        for binding in needed {
            let (head, size) = self.groups.get(&binding).map_or((None, 0), |group| {
                (group.ring.queue_head(), group.ring.queue_frames())
            });
            if self.media_controls.covers(
                &binding,
                self.groups.get(&binding).map(|g| &g.ring),
                at,
                1024,
            ) {
                rows.push((binding, Some(at), 1024, false));
            } else {
                rows.push((binding, head, size, head.is_none() || size < 1024));
            }
        }
        let minimum = rows
            .iter()
            .filter(|row| !row.3)
            .filter_map(|row| row.1)
            .min()
            .map_or(at, |head| head.min(at));
        // Literal aligned-insufficient second pass after finding the global
        // eligible minimum. A future-only head contributes no samples yet.
        for (_, head, size, pending) in &mut rows {
            if *pending {
                continue;
            }
            let offset = head
                .unwrap()
                .checked_sub(minimum)
                .ok_or(ProgramAudioError::TimeOverflow)?;
            let required = 1024usize.saturating_sub(usize::try_from(offset).unwrap_or(usize::MAX));
            if *size < required {
                *pending = true;
            }
        }
        let result = BufferedReadiness {
            minimum_frame: rows
                .iter()
                .filter(|row| !row.3)
                .filter_map(|row| row.1)
                .min(),
            pending_inputs: rows.iter().filter(|row| row.3).count() as u32,
        };
        for (binding, _, _, pending) in rows {
            if let Some(group) = self.groups.get_mut(&binding) {
                group.buffered.pending = pending;
            }
        }
        Ok(result)
    }
    /// Used only after the buffering controller reaches its maximum. Drop a
    /// complete late prefix, then repeat the full peek; an unrecovered source
    /// loses its CPU timing/filter state while its ordering fence survives.
    pub(crate) fn buffered_drop_late(
        &mut self,
        at: i128,
    ) -> Result<BufferedDiscardReport, ProgramAudioError> {
        self.trim_late(at, false)
    }
    /// Normalize only the eligible heads selected by the immediately preceding
    /// prepare. Pending sources must retain their ordinary stop observations;
    /// a clock's zero-floor adjustment is not maximum-buffer recovery.
    pub(crate) fn buffered_normalize_ready(
        &mut self,
        at: i128,
    ) -> Result<BufferedDiscardReport, ProgramAudioError> {
        self.trim_late(at, true)
    }
    fn trim_late(
        &mut self,
        at: i128,
        ready_only: bool,
    ) -> Result<BufferedDiscardReport, ProgramAudioError> {
        self.ensure_buffered()?;
        at.checked_add(1024)
            .ok_or(ProgramAudioError::TimeOverflow)?;
        let actions = self.prepare_late_actions(at, ready_only)?;
        if actions.is_empty() {
            return Ok(BufferedDiscardReport::default());
        }
        let generation = next(self.data_generation)?;
        let report = self.apply_late_actions(at, actions);
        self.data_generation = generation;
        Ok(report)
    }
    /// Suspension recovery must account source loss before retiring source
    /// storage. Public offline skip_to deliberately retains its old contract.
    pub(crate) fn buffered_skip_to(
        &mut self,
        frame: u64,
    ) -> Result<BufferedDiscardReport, ProgramAudioError> {
        self.ensure_buffered()?;
        if self.cursor.is_some_and(|cursor| frame < cursor) {
            return Err(ProgramAudioError::ClockRewind);
        }
        let at = i128::from(frame);
        let actions = self.prepare_late_actions(at, false)?;
        // A suspension can consume only waiting ticks, leaving media time
        // unchanged. Its late source queues still need one accounted recovery.
        if self.cursor == Some(frame) && actions.is_empty() {
            return Ok(BufferedDiscardReport::default());
        }
        let generation = next(self.data_generation)?;
        drop(self.timeline.segments(frame, 1)?);
        // This is the final fallible preparation step. It does not touch CPU
        // source queues; staged queue actions below have no fallible arithmetic
        // or allocation. One exclusive owner publishes the compound result.
        self.timeline.retire_before(frame)?;
        let report = self.apply_late_actions(at, actions);
        self.retire_sources_to(frame, false, frame);
        self.data_generation = generation;
        Ok(report)
    }
    fn prepare_late_actions(
        &self,
        at: i128,
        ready_only: bool,
    ) -> Result<Vec<LateAction>, ProgramAudioError> {
        let mut actions = Vec::new();
        for binding in self.buffered_bindings() {
            if self.histories.has_binding(&binding) {continue;}
            let Some(group) = self.groups.get(&binding) else {
                continue;
            };
            if ready_only && group.buffered.pending {
                continue;
            }
            let Some(head) = group.ring.queue_head().filter(|head| *head < at) else {
                continue;
            };
            let difference = at
                .checked_sub(head)
                .ok_or(ProgramAudioError::TimeOverflow)?;
            let drop = usize::try_from(difference.min(group.ring.queue_frames() as i128))
                .map_err(|_| ProgramAudioError::TimeOverflow)?;
            let recovered = head
                .checked_add(drop as i128)
                .ok_or(ProgramAudioError::TimeOverflow)?
                >= at;
            actions.push(LateAction {
                binding,
                frames: drop,
                recovered,
            });
        }
        Ok(actions)
    }
    fn apply_late_actions(&mut self, at: i128, actions: Vec<LateAction>) -> BufferedDiscardReport {
        let mut report = BufferedDiscardReport::default();
        for action in actions {
            report.discarded_frames += action.frames as u64;
            if action.recovered {
                let group = self.groups.get_mut(&action.binding).unwrap();
                group.ring.discard_before(at);
                // ignore_audio resets the size observation; pending_stop is
                // cleared by a successful ordinary discard, not this trim.
                group.buffered.last_size = 0;
                group.buffered.pending = group.ring.queue_frames() < 1024;
            } else {
                self.groups.remove(&action.binding);
                report.reset_inputs += 1;
            }
        }
        report
    }
    /// OBS discard runs on emitted and discard-only waiting intervals alike.
    /// Underfull input is retimed without popping. Three equal nonzero-size
    /// observations clear a stopped queue; a successful pop resets that probe.
    pub(crate) fn buffered_discard(
        &mut self,
        at: i128,
    ) -> Result<BufferedDiscardReport, ProgramAudioError> {
        self.ensure_buffered()?;
        let end = at
            .checked_add(1024)
            .ok_or(ProgramAudioError::TimeOverflow)?;
        let bindings:Vec<_> = self.buffered_bindings().into_iter().filter(|binding| !self.histories.has_binding(binding)).collect();
        for binding in &bindings {
            if let Some(head) = self
                .groups
                .get(binding)
                .and_then(|group| group.ring.queue_head())
            {
                head.checked_sub(at)
                    .ok_or(ProgramAudioError::TimeOverflow)?;
                at.checked_sub(head)
                    .ok_or(ProgramAudioError::TimeOverflow)?;
                end.checked_add(self.groups[binding].ring.queue_frames() as i128)
                    .ok_or(ProgramAudioError::TimeOverflow)?;
            }
        }
        if bindings.is_empty() {
            return Ok(BufferedDiscardReport::default());
        }
        let generation = next(self.data_generation)?;
        let mut report = BufferedDiscardReport::default();
        for binding in bindings {
            if self.media_controls.has(&binding) {
                if let Some(group) = self.groups.get_mut(&binding) {
                    let before = group.ring.queue_frames();
                    group.ring.discard_before(end);
                    report.discarded_frames += (before - group.ring.queue_frames()) as u64;
                    group.buffered.last_size = 0;
                    group.buffered.pending_stop = false;
                }
                continue;
            }
            let Some(group) = self.groups.get_mut(&binding) else {
                continue;
            };
            let Some(head) = group.ring.queue_head() else {
                continue;
            };
            if end <= head {
                continue;
            }
            if head < at {
                if group.buffered.pending && group.ring.queue_frames() < 1024 {
                    let stopped = group.buffered.stopped(&mut group.ring);
                    if stopped != 0 {
                        report.discarded_frames += stopped as u64;
                        report.stopped_inputs += 1;
                    }
                }
                continue;
            }
            let required =
                1024 - usize::try_from(head - at).map_err(|_| ProgramAudioError::TimeOverflow)?;
            if group.ring.queue_frames() < required {
                let stopped = group.buffered.stopped(&mut group.ring);
                if stopped != 0 {
                    report.discarded_frames += stopped as u64;
                    report.stopped_inputs += 1;
                } else {
                    group.ring.queue_retime(end)?;
                }
            } else {
                group.ring.queue_pop(required)?;
                report.discarded_frames += required as u64;
                group.ring.queue_retime(end)?;
                group.buffered.last_size = 0;
                group.buffered.pending_stop = false;
            }
        }
        self.data_generation = generation;
        Ok(report)
    }
}
