use super::ProgramAudioError;

/// Fixed-size sample ring on an absolute project-frame grid. Missing frames
/// carry an explicit validity bit independently of their numerical zero value.
pub(super) struct SampleRing {
    samples: Vec<f32>,
    valid: Vec<bool>,
    channels: usize,
    capacity: usize,
    head: usize,
    start: i128,
    len: usize,
    timestamped: bool,
}
impl SampleRing {
    pub(super) fn new(frames: usize, channels: usize) -> Result<Self, ProgramAudioError> {
        if frames == 0 || channels == 0 || channels > 8 {
            return Err(ProgramAudioError::Invalid("sample ring dimensions"));
        }
        let count = frames
            .checked_mul(channels)
            .ok_or(ProgramAudioError::Limit("sample ring length"))?;
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(count)
            .map_err(|_| ProgramAudioError::Limit("sample ring allocation"))?;
        samples.resize(count, 0.0);
        let mut valid = Vec::new();
        valid
            .try_reserve_exact(frames)
            .map_err(|_| ProgramAudioError::Limit("sample validity allocation"))?;
        valid.resize(frames, false);
        Ok(Self {
            samples,
            valid,
            channels,
            capacity: frames,
            head: 0,
            start: 0,
            len: 0,
            timestamped: false,
        })
    }
    pub(super) fn required_bytes(
        frames: usize,
        channels: usize,
    ) -> Result<usize, ProgramAudioError> {
        frames
            .checked_mul(channels)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| n.checked_add(frames))
            .and_then(|n| n.checked_add(std::mem::size_of::<Self>()))
            .ok_or(ProgramAudioError::Limit("sample ring bytes"))
    }
    pub(super) fn bytes(&self) -> usize {
        self.samples.capacity() * 4 + self.valid.capacity() + std::mem::size_of::<Self>()
    }
    pub(super) fn capacity(&self) -> usize { self.capacity }
    /// Allocate only after the owner charged both the existing and new rings.
    /// Copy validity and wrapped samples directly: no second full-size scratch.
    pub(super) fn grown(&self, frames: usize) -> Result<Self, ProgramAudioError> {
        if frames < self.capacity { return Err(ProgramAudioError::Invalid("source ring cannot shrink during growth")); }
        let mut next = Self::new(frames, self.channels)?;
        next.start = self.start;
        next.len = self.len;
        next.timestamped = self.timestamped;
        for frame in 0..self.len {
            let old = (self.head + frame) % self.capacity;
            next.valid[frame] = self.valid[old];
            next.samples[frame*self.channels..(frame+1)*self.channels]
                .copy_from_slice(&self.samples[old*self.channels..(old+1)*self.channels]);
        }
        Ok(next)
    }
    pub(super) fn end(&self) -> i128 {
        self.start + self.len as i128
    }
    pub(super) fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.timestamped = false;
    }
    pub(super) fn queue_head(&self) -> Option<i128> {
        self.timestamped.then_some(self.start)
    }
    pub(super) fn queue_frames(&self) -> usize {
        self.len
    }
    pub(super) fn queue_append(
        &mut self,
        actual: i128,
        samples: &[f32],
    ) -> Result<Placement, ProgramAudioError> {
        let start = if self.timestamped { self.end() } else { actual };
        self.place(start, samples, false)
    }
    pub(super) fn queue_pop(&mut self, frames: usize) -> Result<(), ProgramAudioError> {
        if frames > self.len {
            return Err(ProgramAudioError::Invalid(
                "source FIFO pop exceeds queued frames",
            ));
        }
        let start = self
            .start
            .checked_add(frames as i128)
            .ok_or(ProgramAudioError::TimeOverflow)?;
        self.head = (self.head + frames) % self.capacity;
        self.len -= frames;
        self.start = start;
        Ok(())
    }
    pub(super) fn queue_retime(&mut self, start: i128) -> Result<(), ProgramAudioError> {
        start
            .checked_add(self.len as i128)
            .ok_or(ProgramAudioError::TimeOverflow)?;
        self.start = start;
        self.timestamped = true;
        Ok(())
    }
    pub(super) fn discard_before(&mut self, frame: i128) {
        if self.len == 0 || frame <= self.start {
            return;
        }
        let count = (frame - self.start).min(self.len as i128) as usize;
        self.head = (self.head + count) % self.capacity;
        self.start += count as i128;
        self.len -= count;
    }
    /// Replace a discontinuous tail as OBS deque_place + pop_back does. A
    /// continuous conversion chunk appends without discarding queued history.
    pub(super) fn place(
        &mut self,
        start: i128,
        samples: &[f32],
        replace: bool,
    ) -> Result<Placement, ProgramAudioError> {
        if !samples.len().is_multiple_of(self.channels)
            || samples.iter().any(|sample| !sample.is_finite())
        {
            return Err(ProgramAudioError::Invalid("converted ring samples"));
        }
        let frames = samples.len() / self.channels;
        if frames == 0 {
            return Ok(Placement::default());
        }
        let end = start
            .checked_add(frames as i128)
            .ok_or(ProgramAudioError::TimeOverflow)?;
        let new_start = if self.len == 0 || start < self.start {
            start
        } else {
            self.start
        };
        let new_end = if replace || self.len == 0 || start < self.start {
            end
        } else {
            self.end().max(end)
        };
        if new_end - new_start > self.capacity as i128 {
            return Err(ProgramAudioError::Backpressure(
                "converted source timeline is full",
            ));
        }
        let old_end = self.end();
        let gap = if self.len != 0 && start > old_end {
            usize::try_from(start - old_end).map_err(|_| ProgramAudioError::TimeOverflow)?
        } else {
            0
        };
        let overlap = if self.len != 0 && start < old_end {
            usize::try_from((old_end - start).min(frames as i128).max(0))
                .map_err(|_| ProgramAudioError::TimeOverflow)?
        } else {
            0
        };
        self.timestamped = true;
        if self.len == 0 || start < self.start {
            self.head = 0;
            self.start = start;
            self.len = 0;
        }
        let offset =
            usize::try_from(start - self.start).map_err(|_| ProgramAudioError::TimeOverflow)?;
        for frame in self.len..offset {
            let target = (self.head + frame) % self.capacity;
            self.valid[target] = false;
            self.samples[target * self.channels..(target + 1) * self.channels].fill(0.0);
        }
        for (frame, input) in samples.chunks_exact(self.channels).enumerate() {
            let target = (self.head + offset + frame) % self.capacity;
            self.samples[target * self.channels..(target + 1) * self.channels]
                .copy_from_slice(input);
            self.valid[target] = true;
        }
        self.len = if replace {
            offset + frames
        } else {
            self.len.max(offset + frames)
        };
        Ok(Placement { gap, overlap })
    }
    pub(super) fn truncate_after(&mut self, frame: i128) {
        if !self.timestamped || frame >= self.end() {
            return;
        }
        if frame <= self.start {
            self.clear();
            return;
        }
        self.len = usize::try_from(frame - self.start).unwrap();
    }
    pub(super) fn covers(&self, frame: i128) -> bool {
        self.timestamped
            && frame >= self.start
            && frame < self.end()
            && self.valid[(self.head + (frame - self.start) as usize) % self.capacity]
    }
    pub(super) fn can_place(&self,start:i128,frames:usize)->bool {
        let Some(end)=start.checked_add(frames as i128)else{return false;};
        let first=if self.len==0||start<self.start{start}else{self.start};
        let last=if self.len==0||start<self.start{end}else{self.end().max(end)};
        last-first<=self.capacity as i128
    }
    /// Locate one actual contiguous valid capture run without inventing PCM
    /// for holes. Caller bounds the search to one1024-frame processing quantum.
    pub(super) fn valid_run(&self,start:i128,frames:usize)->Option<(usize,usize)>{
        let mut first=0;
        while first<frames&&!self.covers(start+first as i128){first+=1;}
        if first==frames{return None;}
        let mut end=first+1;
        while end<frames&&self.covers(start+end as i128){end+=1;}
        Some((first,end-first))
    }
    pub(super) fn read(
        &self,
        start: i128,
        frames: usize,
        output: &mut [f32],
    ) -> Result<usize, ProgramAudioError> {
        if output.len()
            != frames
                .checked_mul(self.channels)
                .ok_or(ProgramAudioError::Limit("sample read dimensions"))?
        {
            return Err(ProgramAudioError::Invalid("sample read buffer"));
        }
        output.fill(0.0);
        let mut missing = 0;
        for frame in 0..frames {
            let position = start
                .checked_add(frame as i128)
                .ok_or(ProgramAudioError::TimeOverflow)?;
            if position < self.start || position >= self.end() {
                missing += 1;
                continue;
            }
            let source = (self.head + (position - self.start) as usize) % self.capacity;
            if !self.valid[source] {
                missing += 1;
                continue;
            }
            output[frame * self.channels..(frame + 1) * self.channels].copy_from_slice(
                &self.samples[source * self.channels..(source + 1) * self.channels],
            );
        }
        Ok(missing)
    }
}
#[derive(Default)]
pub(super) struct Placement {
    pub gap: usize,
    pub overlap: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ring_preserves_absolute_positions_wraps_and_distinguishes_missing_from_silence() {
        let mut ring = SampleRing::new(6, 1).unwrap();
        ring.place(-2, &[1.0, 2.0, 3.0], false).unwrap();
        let mut output = [0.0; 5];
        assert_eq!(ring.read(-3, 5, &mut output).unwrap(), 2);
        assert_eq!(output, [0.0, 1.0, 2.0, 3.0, 0.0]);
        ring.discard_before(0);
        ring.place(1, &[0.0, 4.0, 5.0, 6.0], false).unwrap();
        let mut output = [0.0; 5];
        assert_eq!(ring.read(0, 5, &mut output).unwrap(), 0);
        assert_eq!(output, [3.0, 0.0, 4.0, 5.0, 6.0]);
    }
    #[test]
    fn discontinuous_placement_overwrites_and_truncates_like_obs_deque() {
        let mut ring = SampleRing::new(8, 1).unwrap();
        ring.place(0, &[1.0; 6], false).unwrap();
        let placement = ring.place(2, &[2.0; 2], true).unwrap();
        assert_eq!(placement.overlap, 2);
        assert_eq!(ring.end(), 4);
        ring.place(6, &[3.0], true).unwrap();
        let mut output = [0.0; 7];
        assert_eq!(ring.read(0, 7, &mut output).unwrap(), 2);
        assert_eq!(output, [1.0, 1.0, 2.0, 2.0, 0.0, 0.0, 3.0]);
        let end = ring.end();
        assert!(ring.place(20, &[4.0], false).is_err());
        assert_eq!(ring.end(), end);
        ring.place(-1, &[5.0], true).unwrap();
        assert_eq!(ring.end(), 0);
    }
}
