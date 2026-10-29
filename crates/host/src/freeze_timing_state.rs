//! Scalar state for one Freeze enabled run. This is an internal building block:
//! it does not itself publish model changes or retain images.
//!
//! The owner must bind this value to the actual session, source, filter and
//! canonical/draft/private instance. It must fence capture receipts with the
//! corresponding source definition and privacy grant before calling captured.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    None,
    Disable { run: u64 },
    Refresh { run: u64, request: u64 },
    RetryCapture { run: u64 },
    RetryRender { run: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TimingError {
    TimeReversed,
    IdentityExhausted,
}

#[derive(Default)]
pub(super) struct TimingState {
    now: u64,
    run: u64,
    enabled_since: Option<u64>,
    elapsed_before_segment:u64,
    captured_at: Option<u64>,
    pending_refresh: Option<u64>,
    retry_at: Option<u64>,
    next_request: u64,
    pending_disable: bool,
    disable_accepted: bool,
    disable_retry_at: Option<u64>,
    duration_ms: u32,
    refresh_ms: u32,
    failure:bool,
    cache_epoch:u64,
}

impl TimingState {
    pub(super) fn last_time(&self) -> u64 { self.now }
    pub(super) fn can_observe(&self, enabled: bool, now: u64) -> Result<(), TimingError> {
        if now<self.now&&self.enabled_since.is_some(){
            if self.cache_epoch==u64::MAX{return Err(TimingError::IdentityExhausted);}
            self.elapsed_before_segment.checked_add(self.now.saturating_sub(self.enabled_since.unwrap_or(self.now))).ok_or(TimingError::IdentityExhausted)?;
        }
        let _=enabled;
        Ok(())
    }
    pub(super) fn can_evaluate(&self, now: u64) -> Result<(), TimingError> {
        if now < self.now { return Err(TimingError::TimeReversed); }
        let Some(start) = self.enabled_since else { return Ok(()); };
        let duration = u64::from(self.duration_ms) * 1_000_000;
        let expired = duration != 0 && self.elapsed_before_segment.saturating_add(now.saturating_sub(start)) > duration;
        let refresh_due = !expired && !self.pending_disable && self.pending_refresh.is_none()
            && self.retry_at.is_none_or(|at| now >= at) && self.refresh_ms != 0
            && self.captured_at.is_some_and(|at| now.saturating_sub(at) >= u64::from(self.refresh_ms) * 1_000_000);
        if refresh_due && self.next_request == u64::MAX { return Err(TimingError::IdentityExhausted); }
        if self.retry_at.is_some_and(|at|now>=at)&&self.cache_epoch==u64::MAX{return Err(TimingError::IdentityExhausted);}
        Ok(())
    }
    /// Zero = no action, one = expiration, two = refresh. No mutation.
    pub(super) fn due_kind(&self, now:u64)->u8 {
        let Some(start)=self.enabled_since else{return 0;};
        let duration=u64::from(self.duration_ms)*1_000_000;
        if duration!=0&&self.elapsed_before_segment.saturating_add(now.saturating_sub(start))>duration{
            return u8::from(!self.pending_disable&&self.disable_retry_at.is_none_or(|retry|now>=retry));
        }
        if !self.pending_disable&&self.pending_refresh.is_none()&&self.captured_at.is_none()&&self.retry_at.is_some_and(|at|now>=at){return 3;}
        if !self.pending_disable&&self.pending_refresh.is_none()&&self.captured_at.is_some()&&self.retry_at.is_some_and(|at|now>=at){return 4;}
        if !self.pending_disable&&self.pending_refresh.is_none()&&self.retry_at.is_none_or(|at|now>=at)&&self.refresh_ms!=0&&self.captured_at.is_some_and(|at|now.saturating_sub(at)>=u64::from(self.refresh_ms)*1_000_000){2}else{0}
    }
    fn advance_clock(&mut self, now: u64) -> Result<(), TimingError> {
        if now < self.now { return Err(TimingError::TimeReversed); }
        self.now = now;
        Ok(())
    }

    /// Call from the owning control path on every observed authored enable
    /// state, including when capture is not yet available. Definition or
    /// threshold edits alone must not call this as a new enabled transition.
    pub(super) fn observe(&mut self, enabled: bool, now: u64, duration_ms: u32, refresh_ms: u32, new_run:u64) -> Result<(), TimingError> {
        self.can_observe(enabled,now)?;
        if now<self.now{
            if let Some(start)=self.enabled_since{
                self.elapsed_before_segment=self.elapsed_before_segment.checked_add(self.now.saturating_sub(start)).ok_or(TimingError::IdentityExhausted)?;
                self.enabled_since=Some(now);self.captured_at=None;self.pending_refresh=None;self.retry_at=None;self.pending_disable=false;self.disable_accepted=false;self.disable_retry_at=None;self.cache_epoch+=1;
            }
            self.now=now;
        }
        self.advance_clock(now)?;
        if enabled && self.enabled_since.is_none() {
            if new_run==0{return Err(TimingError::IdentityExhausted);}
            self.run = new_run;
            self.enabled_since = Some(now);
            self.elapsed_before_segment=0;
            self.captured_at = None;
            self.pending_refresh = None;
            self.retry_at = None;
            self.pending_disable = false;
            self.disable_accepted = false;
            self.disable_retry_at = None;
            self.failure=false;
            self.cache_epoch=0;
        } else if !enabled && self.enabled_since.is_some() {
            self.enabled_since = None;
            self.elapsed_before_segment=0;
            self.captured_at = None;
            self.pending_refresh = None;
            self.retry_at = None;
            self.pending_disable = false;
            self.disable_accepted = false;
            self.disable_retry_at = None;
            self.failure=false;
        }
        self.duration_ms = duration_ms;
        self.refresh_ms = refresh_ms;
        Ok(())
    }

    /// A privacy/binding retirement releases capture authority without
    /// changing the canonical Enabled flag or its enabled-run origin.
    pub(super) fn capture_retired(&mut self) -> bool {
        let changed=self.captured_at.is_some()||self.pending_refresh.is_some();
        self.captured_at = None;
        self.pending_refresh = None;
        self.retry_at = None;
        if changed{self.cache_epoch=self.cache_epoch.saturating_add(1);}
        changed
    }

    /// The owner must also invalidate the external atomic acceptance gate.
    /// A metadata edit cannot restart the run or erase the installed picture.
    pub(super) fn definition_changed(&mut self) {
        self.pending_disable = false;
        self.disable_accepted = false;
        self.disable_retry_at = None;
        self.pending_refresh = None;
        self.retry_at = None;
    }

    /// Only an actually installed, current capture may establish this anchor.
    /// `request=None` is reserved for a first capture; replacement must carry
    /// the exact request returned by `due`. Stale receipts leave state intact.
    pub(super) fn can_capture(&self, run: u64, request: Option<u64>, at: u64) -> bool {
        let Some(start) = self.enabled_since else { return false; };
        if self.run != run || self.pending_disable || at < start || at > self.now || self.cache_epoch==u64::MAX { return false; }
        match request {
            None => self.captured_at.is_none() && self.pending_refresh.is_none(),
            Some(id) => self.pending_refresh == Some(id) && self.captured_at.is_some_and(|old| at >= old),
        }
    }
    pub(super) fn captured(&mut self, run: u64, request: Option<u64>, at: u64) -> bool {
        if !self.can_capture(run, request, at) { return false; }
        self.captured_at = Some(at);
        self.pending_refresh = None;
        self.retry_at = None;
        self.failure=false;
        self.cache_epoch+=1;
        true
    }

    /// A failed refresh preserves the old capture and sets a bounded retry.
    /// Failure receipts carry project time, not native capture timestamps.
    pub(super) fn capture_failed(&mut self, run: u64, request: Option<u64>, now: u64) -> Result<bool, TimingError> {
        self.advance_clock(now)?;
        if self.run != run || self.enabled_since.is_none() || self.pending_disable || match request{Some(id)=>self.pending_refresh!=Some(id),None=>self.pending_refresh.is_some()||self.captured_at.is_some()}{return Ok(false);}
        self.pending_refresh = None;
        self.retry_at = Some(now.saturating_add(100_000_000));
        self.failure=true;
        Ok(true)
    }

    /// Pending means an intent was issued, not that the enabled flag changed.
    /// The command owner acknowledges the real result and then observes the
    /// authoritative enabled state. A rejected intent can be retried by owner.
    pub(super) fn disable_rejected(&mut self, run: u64) {
        if self.run == run && !self.disable_accepted { self.pending_disable = false; self.disable_retry_at=Some(self.now.saturating_add(100_000_000)); }
    }

    pub(super) fn run(&self) -> Option<u64> { self.enabled_since.map(|_| self.run) }
    pub(super) fn cache_signature(&self)->Option<(u64,u64,u64)>{self.run().map(|run|(run,self.cache_epoch,self.pending_refresh.unwrap_or(0)))}
    pub(super) fn allows_sample(&self,at:u64)->bool{self.enabled_since.is_some_and(|start|at>=start&&at<=self.now)}
    pub(super) fn captured_at(&self)->Option<u64>{self.captured_at}
    pub(super) fn failure(&self)->bool{self.failure}
    pub(super) fn draw_failed(&mut self,now:u64)->bool{
        if self.enabled_since.is_none()||self.pending_disable{return false;}
        let first=!self.failure;self.failure=true;self.retry_at=Some(now.saturating_add(100_000_000));first
    }
    pub(super) fn disable_pending(&self, run: u64) -> bool {
        self.enabled_since.is_some() && self.run == run && self.pending_disable && !self.disable_accepted
    }
    pub(super) fn accept_disable(&mut self, run: u64) -> bool {
        if !self.disable_pending(run) { return false; }
        self.disable_accepted = true;
        true
    }
    pub(super) fn refresh_pending(&self, run: u64, request: u64) -> bool {
        self.enabled_since.is_some() && self.run == run && !self.pending_disable && self.pending_refresh == Some(request)
    }
    pub(super) fn capture_request(&self) -> Option<(u64, Option<u64>)> {
        if self.enabled_since.is_none() || self.pending_disable { return None; }
        if let Some(request) = self.pending_refresh { return Some((self.run, Some(request))); }
        (self.captured_at.is_none()&&self.retry_at.is_none_or(|at|self.now>=at)).then_some((self.run, None))
    }

    pub(super) fn due(&mut self, now: u64) -> Result<Due, TimingError> {
        self.can_evaluate(now)?;
        self.advance_clock(now)?;
        let Some(start) = self.enabled_since else { return Ok(Due::None); };
        let duration = u64::from(self.duration_ms) * 1_000_000;
        if duration != 0 && self.elapsed_before_segment.saturating_add(now.saturating_sub(start)) > duration {
            if !self.pending_disable && self.disable_retry_at.is_none_or(|retry| now>=retry) {
                self.pending_disable = true;
                self.disable_retry_at = None;
                self.pending_refresh = None;
                self.retry_at = None;
                return Ok(Due::Disable { run: self.run });
            }
            return Ok(Due::None);
        }
        if self.captured_at.is_none()&&self.retry_at.is_some_and(|at|now>=at)&&!self.pending_disable{
            self.retry_at=None;self.cache_epoch+=1;return Ok(Due::RetryCapture{run:self.run});
        }
        if self.captured_at.is_some()&&self.pending_refresh.is_none()&&self.retry_at.is_some_and(|at|now>=at)&&!self.pending_disable{
            self.retry_at=None;self.cache_epoch+=1;return Ok(Due::RetryRender{run:self.run});
        }
        if !self.pending_disable && self.pending_refresh.is_none() && self.retry_at.is_none_or(|at| now >= at) && self.refresh_ms != 0
            && self.captured_at.is_some_and(|at| now.saturating_sub(at) >= u64::from(self.refresh_ms) * 1_000_000)
        {
            self.next_request = self.next_request.checked_add(1).ok_or(TimingError::IdentityExhausted)?;
            self.pending_refresh = Some(self.next_request);
            self.retry_at = None;
            return Ok(Due::Refresh { run: self.run, request: self.next_request });
        }
        Ok(Due::None)
    }

    pub(super) fn deadline(&self) -> Option<u64> {
        let start = self.enabled_since?;
        if self.pending_disable { return None; }
        let duration = if self.duration_ms==0{None}else{let limit=u64::from(self.duration_ms)*1_000_000;if self.elapsed_before_segment>limit{Some(self.now)}else{start.checked_add(limit-self.elapsed_before_segment).and_then(|at|at.checked_add(1))}}.map(|at|self.disable_retry_at.map_or(at,|retry|at.max(retry)));
        let refresh = if self.pending_refresh.is_some(){None}else if self.retry_at.is_some(){self.retry_at}
            else if self.refresh_ms!=0{self.captured_at.and_then(|at|at.checked_add(u64::from(self.refresh_ms)*1_000_000))}else{None};
        match (duration, refresh) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) }
    }
}
