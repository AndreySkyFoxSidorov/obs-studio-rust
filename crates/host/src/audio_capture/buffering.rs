//! Pure OBS buffering clock. Contract: doc/migration/adaptive-audio.md.
//! Wall ticks advance independently of the next emitted media cursor. Prefix
//! intervals selected while waiting are discard-only and may precede zero.
//! The arithmetic replaces OBS's bounded deque; it never replays emitted PCM.

use crate::audio_capture::AudioCaptureError;
use skylive_core::AudioBuffering;

const FRAMES: u64 = 1024;
const NANOS: i128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BufferDecision {
    pub start_frame: i128,
    pub waiting: bool,
    pub added_ticks: u32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct BufferSkip {
    pub wall_ticks: u64,
    pub media_frames: u64,
    pub waiting_ticks: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BufferClock {
    rate: u32,
    fixed: bool,
    max_ticks: u32,
    sync_ticks: u32,
    wall_ticks: u64,
    emitted_cursor: u64,
    total_ticks: u32,
    waiting_ticks: u32,
}
impl BufferClock {
    pub(super) fn new(
        initial_cursor: u64,
        rate: u32,
        policy: AudioBuffering,
    ) -> Result<Self, AudioCaptureError> {
        let max_ticks = policy.max_ticks(rate).map_err(|_| {
            AudioCaptureError::Invalid("invalid audio buffering policy or project sample rate")
        })?;
        // Fixed startup preselects all its discard-only waits. The first
        // observation therefore reports added_ticks=0, while still waiting.
        let initial_ticks = if policy.fixed() { max_ticks } else { 0 };
        Ok(Self {
            rate,
            fixed: policy.fixed(),
            max_ticks,
            sync_ticks: 0,
            wall_ticks: 0,
            emitted_cursor: initial_cursor,
            total_ticks: initial_ticks,
            waiting_ticks: initial_ticks,
        })
    }
    pub(super) fn cursor(&self) -> u64 {
        self.emitted_cursor
    }
    pub(super) fn wall_ticks(&self) -> u64 {
        self.wall_ticks
    }
    pub(super) fn total_ticks(&self) -> u32 {
        self.total_ticks
    }
    pub(super) fn waiting_ticks(&self) -> u32 {
        self.waiting_ticks
    }
    pub(super) fn max_ticks(&self) -> u32 {
        self.max_ticks + self.sync_ticks
    }
    pub(super) fn maxed(&self) -> bool {
        self.total_ticks == self.max_ticks + self.sync_ticks
    }
    /// Source negative offsets require future native PCM, independently from
    /// optional user fixed/adaptive buffering. Never rewind already emitted PCM.
    /// Delay only grows within one Program epoch; shrinking waits for restart.
    pub(super) fn ensure_sync_lookahead(&mut self,frames:u64)->Result<(),AudioCaptureError>{
        self.ensure_source_lookahead(frames,0)
    }
    /// Filter latency uses the existing optional buffer allowance first.
    /// A fixed small user buffer receives only the extra source requirement;
    /// adaptive960ms is not blindly added to the same processing delay again.
    pub(super) fn ensure_source_lookahead(&mut self,sync_frames:u64,source_required_frames:u64)->Result<(),AudioCaptureError>{
        let max_sync=(u64::from(self.rate)*950).div_ceil(1000);
        if sync_frames>max_sync{return Err(AudioCaptureError::Limit("audio sync lookahead exceeds950ms"));}
        let sync_ticks=u32::try_from(sync_frames.div_ceil(FRAMES)).map_err(|_|AudioCaptureError::TimeOverflow)?;
        let required_ticks=u32::try_from(source_required_frames.div_ceil(FRAMES)).map_err(|_|AudioCaptureError::TimeOverflow)?;
        let extra_processing=required_ticks.saturating_sub(self.max_ticks);
        let ticks=sync_ticks.max(extra_processing);
        let admitted_sync=ticks.max(self.sync_ticks);
        let cap=u128::from(self.max_ticks)+u128::from(admitted_sync);
        if cap*u128::from(FRAMES)*1_000_000_000>u128::from(self.rate)*u128::from(crate::audio_output::MAX_AUDIO_EMISSION_SKEW_NS){
            return Err(AudioCaptureError::Limit("audio emission skew exceeds the admitted output interleave profile"));
        }
        if ticks>self.sync_ticks{
            let add=ticks-self.sync_ticks;
            self.waiting_ticks=self.waiting_ticks.checked_add(add).ok_or(AudioCaptureError::TimeOverflow)?;
            self.total_ticks=self.total_ticks.checked_add(add).ok_or(AudioCaptureError::TimeOverflow)?;
            self.sync_ticks=ticks;
        }
        Ok(())
    }
    pub(super) fn next_interval(&self) -> Result<i128, AudioCaptureError> {
        selected_interval(self.emitted_cursor, self.waiting_ticks)
    }
    /// The caller excludes pending/unseen sources. Some(0) is a valid ready
    /// head; this relative-time port never uses OBS's absolute zero sentinel.
    pub(super) fn observe_minimum(
        &mut self,
        minimum: Option<i128>,
    ) -> Result<BufferDecision, AudioCaptureError> {
        let selected = self.next_interval()?;
        let mut added_ticks = 0;
        if let Some(minimum) =
            minimum.filter(|minimum| !self.fixed && !self.maxed() && *minimum < selected)
        {
            // Literal OBS order: floor each signed timestamp, subtract,
            // floor nanoseconds to frames, then ceil frames to blocks.
            let selected_ns = frame_ns(selected, self.rate)?;
            let minimum_ns = frame_ns(minimum, self.rate)?;
            let offset = selected_ns
                .checked_sub(minimum_ns)
                .ok_or(AudioCaptureError::TimeOverflow)?;
            let offset = u128::try_from(offset).map_err(|_| AudioCaptureError::TimeOverflow)?;
            let frames = offset
                .checked_mul(u128::from(self.rate))
                .ok_or(AudioCaptureError::TimeOverflow)?
                / NANOS as u128;
            let ticks = frames.div_ceil(u128::from(FRAMES));
            added_ticks = u32::try_from(ticks.min(u128::from(self.max_ticks + self.sync_ticks - self.total_ticks)))
                .map_err(|_| AudioCaptureError::TimeOverflow)?;
        }
        let total_ticks = self
            .total_ticks
            .checked_add(added_ticks)
            .ok_or(AudioCaptureError::TimeOverflow)?;
        let waiting_ticks = self
            .waiting_ticks
            .checked_add(added_ticks)
            .ok_or(AudioCaptureError::TimeOverflow)?;
        let start_frame = selected_interval(self.emitted_cursor, waiting_ticks)?;
        self.total_ticks = total_ticks;
        self.waiting_ticks = waiting_ticks;
        Ok(BufferDecision {
            start_frame,
            waiting: waiting_ticks != 0,
            added_ticks,
        })
    }
    /// Absolute per-source windows cannot destructively retime one shared raw
    /// device FIFO. A real known-input shortfall instead spends one remaining
    /// adaptive user-buffer tick; it never changes a native source timestamp.
    pub(super) fn observe_source_shortfall(&mut self, needed:bool) -> Result<BufferDecision,AudioCaptureError> {
        let minimum=if needed&&!self.fixed&&!self.maxed() {
            Some(self.next_interval()?.checked_sub(i128::from(FRAMES)).ok_or(AudioCaptureError::TimeOverflow)?)
        }else{None};
        self.observe_minimum(minimum)
    }
    pub(super) fn complete_tick(&mut self) -> Result<(), AudioCaptureError> {
        let wall_ticks = self
            .wall_ticks
            .checked_add(1)
            .ok_or(AudioCaptureError::TimeOverflow)?;
        let (waiting_ticks, emitted_cursor) = if self.waiting_ticks != 0 {
            (self.waiting_ticks - 1, self.emitted_cursor)
        } else {
            (
                0,
                self.emitted_cursor
                    .checked_add(FRAMES)
                    .ok_or(AudioCaptureError::TimeOverflow)?,
            )
        };
        self.wall_ticks = wall_ticks;
        self.waiting_ticks = waiting_ticks;
        self.emitted_cursor = emitted_cursor;
        Ok(())
    }
    /// O(1) suspension retirement. Existing waits consume wall ticks first;
    /// only the remainder skips emitted-media time. Total delay never shrinks.
    pub(super) fn skip_wall_to(&mut self, target: u64) -> Result<BufferSkip, AudioCaptureError> {
        let wall_ticks = target
            .checked_sub(self.wall_ticks)
            .ok_or(AudioCaptureError::Invalid(
                "audio buffering wall ticks moved backwards",
            ))?;
        let waiting_ticks = wall_ticks.min(u64::from(self.waiting_ticks));
        let media_ticks = wall_ticks - waiting_ticks;
        let media_frames = media_ticks
            .checked_mul(FRAMES)
            .ok_or(AudioCaptureError::TimeOverflow)?;
        let emitted_cursor = self
            .emitted_cursor
            .checked_add(media_frames)
            .ok_or(AudioCaptureError::TimeOverflow)?;
        let remaining_waits = self.waiting_ticks
            - u32::try_from(waiting_ticks).map_err(|_| AudioCaptureError::TimeOverflow)?;
        self.wall_ticks = target;
        self.waiting_ticks = remaining_waits;
        self.emitted_cursor = emitted_cursor;
        Ok(BufferSkip {
            wall_ticks,
            media_frames,
            waiting_ticks,
        })
    }
}
fn selected_interval(cursor: u64, waits: u32) -> Result<i128, AudioCaptureError> {
    let prefix = i128::from(waits)
        .checked_mul(i128::from(FRAMES))
        .ok_or(AudioCaptureError::TimeOverflow)?;
    i128::from(cursor)
        .checked_sub(prefix)
        .ok_or(AudioCaptureError::TimeOverflow)
}
fn frame_ns(frame: i128, rate: u32) -> Result<i128, AudioCaptureError> {
    frame
        .checked_mul(NANOS)
        .map(|value| value.div_euclid(i128::from(rate)))
        .ok_or(AudioCaptureError::TimeOverflow)
}

#[cfg(test)]
#[path = "buffering_tests.rs"]
mod tests;
