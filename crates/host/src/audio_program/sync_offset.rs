//! Source-specific presentation windows over shared, unmodified native PCM.
//! Epochs are authored at capture time. Increasing delay must not replay old
//! captured samples; decreasing it replaces the overlapping future placement.
use super::*;
const MAX_LANES: usize = MAX_AUDIO_BLOCK_SOURCES;
const MAX_EPOCHS: usize = 4096;
const MAX_KEY_BYTES: usize = 1024 * 1024;
/// Includes both epoch states during atomic preparation and bounded key copies.
pub(super) const WORKSPACE_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone)]
struct Epoch {
    raw_start: i128,
    output_start:i128,
    history_epoch:u64,
    raw_end: Option<i128>,
    output_end: Option<i128>,
    offset: i128,
}

#[cfg(test)]
mod scoped_history_tests {
    use super::*;

    fn binding(name: &str) -> AudioInputKey {
        AudioInputKey::Native(skylive_core::AudioCaptureBinding::new(
            skylive_core::AudioFlow::Input,
            skylive_core::AudioTarget::WindowsEndpoint { id: name.into() },
            true, false,
        ).unwrap())
    }
    fn epoch(scope: u64, offset: i128) -> Epoch {
        Epoch { raw_start: 0, output_start: offset.max(0), history_epoch: scope,
            raw_end: None, output_end: None, offset }
    }
    fn lane(source: u64, binding: AudioInputKey, epochs: Vec<Epoch>) -> Lane {
        Lane { source: Id::new(source), binding, active: true, absolute: true,
            program: true, monitor: false, epochs }
    }
    #[test]
    fn positive_delay_and_retention_are_source_binding_and_epoch_scoped() {
        let shared = binding("shared");
        let other = binding("other");
        let mut sync = SourceSync { lanes: vec![
            lane(1, shared.clone(), vec![epoch(0, 100), epoch(0, 200), epoch(1, 400)]),
            lane(2, shared.clone(), vec![epoch(0, 900)]),
            lane(1, other.clone(), vec![epoch(0, 800)]),
        ] };
        let source = Id::new(1);
        assert_eq!(sync.positive_source_frames(source, &shared, 0), 200);
        assert_eq!(sync.positive_source_frames(source, &shared, 1), 400);
        assert_eq!(sync.positive_source_frames(source, &shared, 2), 0);
        assert_eq!(sync.positive_source_frames(Id::new(3), &shared, 0), 0);
        assert_eq!(sync.source_retention_floor_epoch(source, &shared, 0, 1000), 800);
        assert_eq!(sync.source_retention_floor_epoch(source, &shared, 1, 1000), 600);
        assert_eq!(sync.source_retention_floor_epoch(source, &shared, 2, 1000), 1000);
        assert_eq!(sync.source_retention_floor_epoch(source, &other, 0, 1000), 200);
        // A clipped pre-stop prefix cannot retain an already-consumed tail.
        for epoch in &mut sync.lanes[0].epochs { epoch.output_end = Some(950); }
        assert_eq!(sync.source_retention_floor_epoch(source, &shared, 0, 1000), 1000);
    }
    #[test]
    fn causal_monitor_floor_applies_only_to_current_active_history_scope() {
        let key = binding("monitor");
        let mut sync = SourceSync { lanes: vec![
            lane(1, key.clone(), vec![epoch(0, -200), epoch(1, -100)]),
        ] };
        sync.lanes[0].monitor = true;
        let source = Id::new(1);
        assert_eq!(sync.positive_source_frames(source, &key, 1), 0);
        assert_eq!(sync.source_retention_floor_epoch(source, &key, 0, 500), 700);
        assert_eq!(sync.source_retention_floor_epoch(source, &key, 1, 500), 500);
        sync.lanes[0].active = false;
        assert_eq!(sync.source_retention_floor_epoch(source, &key, 1, 500), 600);
    }
}
impl Epoch {
    fn end(&self) -> i128 {
        self.output_end.unwrap_or(i128::MAX).min(
            self.raw_end.and_then(|v| v.checked_add(self.offset)).unwrap_or(i128::MAX))
    }
}
#[derive(Clone)]
struct Lane {
    source: Id,
    binding: AudioInputKey,
    active: bool,
    absolute: bool,
    program: bool,
    monitor: bool,
    epochs: Vec<Epoch>,
}
#[derive(Clone, Default)]
pub(super) struct SourceSync {
    lanes: Vec<Lane>,
}
struct KeyCounter(usize);
impl std::io::Write for KeyCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.checked_add(bytes.len()).filter(|v| *v <= MAX_KEY_BYTES)
            .ok_or_else(|| std::io::Error::other("source sync key metadata limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}
impl SourceSync {
    pub(super) fn clear(&mut self) { self.lanes.clear(); }
    pub(super) fn prepare(&self, snapshot: &AudioRouteSnapshot, rate: u32, cursor: u64,gates:&camera_gate::Gates)
        -> Result<Self, ProgramAudioError>
    {
        // Serialize into a counter before cloning potentially path-bearing keys.
        // JSON escaping bounds the UTF8 allocation required by each cloned key.
        let mut keys = KeyCounter(0);
        for lane in &self.lanes {
            serde_json::to_writer(&mut keys, &lane.binding)
                .map_err(|_| ProgramAudioError::Limit("source sync key metadata"))?;
        }
        for source in snapshot.source_controls() {
            serde_json::to_writer(&mut keys, &source.binding)
                .map_err(|_| ProgramAudioError::Limit("source sync key metadata"))?;
        }
        let at = i128::try_from((u128::from(snapshot.at().0) * u128::from(rate))
            .div_ceil(1_000_000_000)).map_err(|_| ProgramAudioError::TimeOverflow)?;
        let mut candidate = self.clone();
        candidate.retire(cursor,snapshot.camera_delay_history());
        let additions = snapshot.source_controls().iter().filter(|source| {
            candidate.lanes.iter().all(|lane| lane.source != source.source || lane.binding != source.binding)
        }).count();
        if candidate.lanes.len().checked_add(additions).is_none_or(|n| n > MAX_LANES) {
            return Err(ProgramAudioError::Limit("source sync lanes"));
        }
        // Closing a route never changes another source sharing its binding.
        for lane in &mut candidate.lanes {
            let next = snapshot.source_controls().iter().find(|source|
                source.source == lane.source && source.binding == lane.binding);
            if next.is_none() && lane.active {
                if let Some(epoch) = lane.epochs.last_mut() { epoch.raw_end = Some(at); }
                // An inactive source's positive-delay tail cannot reappear if
                // it is enabled again while another source retains this device.
                for epoch in &mut lane.epochs {
                    epoch.output_end=Some(epoch.output_end.map_or(at,|end|end.min(at)));
                }
                lane.active = false;
            }
        }
        for source in snapshot.source_controls() {
            // Signed floor on the project sample grid, matching timestamp
            // placement. Capture conversion/resampling timestamps stay intact.
            let scope=gates.scope(source.source);
            let (raw_floor,output_floor)=gates.floor(source.source);
            let raw_at=at.max(i128::from(raw_floor));
            let offset = (i128::from(source.properties.sync_offset_ms()) * i128::from(rate)).div_euclid(1000);
            let position = candidate.lanes.iter().position(|lane|
                lane.source == source.source && lane.binding == source.binding);
            if let Some(position) = position {
                let lane = &mut candidate.lanes[position];
                lane.monitor = !source.monitoring.is_off();
                lane.program = (source.routes.global || source.routes.from || source.routes.to || source.routes.duplicated)
                    && source.monitoring != skylive_core::AudioMonitoringMode::MonitorOnly;
                let unchanged = lane.active && lane.epochs.last().is_some_and(|e| e.offset == offset&&e.history_epoch==scope);
                if unchanged { continue; }
                let start = raw_at.checked_add(offset).ok_or(ProgramAudioError::TimeOverflow)?.max(i128::from(output_floor));
                if let Some(last) = lane.epochs.last_mut() { last.raw_end = Some(last.raw_end.map_or(raw_at,|old|old.min(raw_at))); }
                for old in &mut lane.epochs { old.output_end = Some(old.output_end.map_or(start, |end| end.min(start))); }
                lane.epochs.push(Epoch { raw_start: raw_at, output_start:start,history_epoch:scope,raw_end: None, output_end: None, offset });
                lane.active = true;
                lane.absolute |= offset != 0;
            } else {
                candidate.lanes.push(Lane { source: source.source, binding: source.binding.clone(), active: true,
                    absolute: offset != 0,
                    monitor: !source.monitoring.is_off(),
                    program: (source.routes.global || source.routes.from || source.routes.to || source.routes.duplicated)
                        && source.monitoring != skylive_core::AudioMonitoringMode::MonitorOnly,
                    epochs: vec![Epoch { raw_start: raw_at, output_start:raw_at.saturating_add(offset).max(i128::from(output_floor)),history_epoch:scope,raw_end: None, output_end: None, offset }] });
            }
        }
        if candidate.lanes.iter().map(|lane| lane.epochs.len()).sum::<usize>() > MAX_EPOCHS {
            return Err(ProgramAudioError::Limit("source sync edit epochs"));
        }
        Ok(candidate)
    }
    pub(super) fn absolute(&self, binding: &AudioInputKey) -> bool {
        self.lanes.iter().any(|lane| &lane.binding == binding && (lane.absolute || lane.monitor))
    }
    pub(super) fn pending_sources_routed(&self, cursor:u64, frames:usize,
        histories:&post_dsp::Histories,groups:&HashMap<AudioInputKey,InputGroup>, fences:&HashMap<AudioInputKey,InputFence>,routed:&dyn Fn(Id,&AudioInputKey)->bool)->u32
    {
        let at=i128::from(cursor);
        self.lanes.iter().filter(|lane|lane.active&&lane.program&&routed(lane.source,&lane.binding)).filter(|lane| {
            let Some(epoch)=lane.epochs.iter().rev().find(|e|at>=e.output_start&&at<e.end())else{return false;};
            if fences.get(&lane.binding).is_some_and(|fence|fence.sealed)&&!histories.unprocessed(lane.source,&lane.binding,epoch.history_epoch,groups){return false;}
            let raw=at.saturating_sub(epoch.offset);
            histories.ring_epoch(lane.source,&lane.binding,epoch.history_epoch).is_none_or(|ring|
                !ring.covers(raw)||ring.end()<raw.saturating_add(frames as i128))
        }).count()as u32
    }
    pub(super) fn future_shortfall_routed(&self, cursor: u64, frames: usize,
        histories:&post_dsp::Histories,groups:&HashMap<AudioInputKey,InputGroup>, fences:&HashMap<AudioInputKey,InputFence>,routed:&dyn Fn(Id,&AudioInputKey)->bool) -> bool
    {
        let at=i128::from(cursor);
        self.lanes.iter().filter(|lane|lane.active&&lane.program&&routed(lane.source,&lane.binding)).any(|lane| {
            let Some(epoch)=lane.epochs.iter().rev().find(|epoch|
                at>=epoch.output_start&&at<epoch.end())else{return false;};
            let Some(group)=groups.get(&lane.binding)else{return false;};
            if fences.get(&lane.binding).is_some_and(|fence|fence.sealed)&&!histories.unprocessed(lane.source,&lane.binding,epoch.history_epoch,groups){return false;}
            let raw=at.saturating_sub(epoch.offset);
            let ring=histories.ring_epoch(lane.source,&lane.binding,epoch.history_epoch);
            let head=ring.and_then(SampleRing::queue_head).unwrap_or(group.output_anchor);
            let end=ring.map_or(group.output_anchor,SampleRing::end);
            // A known late native input may have been processed only for DSP
            // warm-up after its output prefix retired. An empty produced ring
            // still needs adaptive waiting, otherwise lateness repeats forever.
            head<=raw&&end<raw.saturating_add(frames as i128)
        })
    }
    pub(super) fn processing_pending(&self,source:Id,binding:&AudioInputKey,start:i128,frames:usize,
        histories:&post_dsp::Histories,groups:&HashMap<AudioInputKey,InputGroup>)->bool{
        let Some(lane)=self.lanes.iter().find(|lane|lane.source==source&&&lane.binding==binding)else{return false;};
        lane.epochs.iter().any(|epoch|{
            let first=start.max(epoch.output_start);
            let end=(start+frames as i128).min(epoch.end());
            first<end&&histories.pending_before(source,binding,epoch.history_epoch,end.saturating_sub(epoch.offset),groups)
        })
    }
    pub(super) fn negative_frames_routed(&self,routed:&dyn Fn(Id,&AudioInputKey)->bool) -> u64 {
        self.lanes.iter().filter(|lane| lane.active && lane.program && routed(lane.source,&lane.binding)).filter_map(|lane| lane.epochs.last())
            .map(|e| u64::try_from((-e.offset).max(0)).unwrap_or(u64::MAX)).max().unwrap_or(0)
    }
    /// Maximum actual source requirement, rather than adding unrelated
    /// sources' negative offset and processing delay together.
    pub(super) fn source_lookahead_routed(&self,routed:&dyn Fn(Id,&AudioInputKey)->bool,
        processed:&dyn Fn(Id,&AudioInputKey,u64)->u64)->u64{
        self.lanes.iter().filter(|lane|lane.active&&lane.program&&routed(lane.source,&lane.binding))
            .filter_map(|lane|lane.epochs.last().map(|epoch|{
                let negative=u64::try_from((-epoch.offset).max(0)).unwrap_or(u64::MAX);
                negative.saturating_add(processed(lane.source,&lane.binding,epoch.history_epoch))
            })).max().unwrap_or(0)
    }
    pub(super) fn positive_source_frames(&self, source: Id, binding: &AudioInputKey, history_epoch: u64) -> usize {
        self.lanes.iter().filter(|lane| lane.source == source && &lane.binding == binding)
            .flat_map(|lane| &lane.epochs).filter(|epoch| epoch.history_epoch == history_epoch)
            .map(|epoch| usize::try_from(epoch.offset.max(0)).unwrap_or(usize::MAX))
            .max().unwrap_or(0)
    }
    pub(super) fn source_retention_floor_epoch(&self, source: Id, binding: &AudioInputKey,
        history_epoch: u64, cursor: i128) -> i128
    {
        let Some(lane) = self.lanes.iter().find(|lane| lane.source == source && &lane.binding == binding) else {
            return cursor;
        };
        let floor = lane.epochs.iter().filter(|epoch| epoch.history_epoch == history_epoch)
            .filter_map(|epoch| {
                let first = cursor.max(epoch.output_start);
                (first < epoch.end()).then(|| first.saturating_sub(epoch.offset))
            }).min().unwrap_or(cursor);
        // The causal monitor reads only the current active source scope. An old
        // camera epoch must not retain PCM for a resumed scope's monitor reader.
        if lane.active && lane.monitor && lane.epochs.last().is_some_and(|epoch| epoch.history_epoch == history_epoch) {
            floor.min(cursor)
        } else { floor }
    }
    pub(super) fn retention_floor(&self, binding: &AudioInputKey, cursor: i128) -> i128 {
        let floor=self.lanes.iter().filter(|lane| &lane.binding == binding)
            .flat_map(|lane| &lane.epochs).filter_map(|epoch| {
                let first = cursor.max(epoch.output_start);
                (first < epoch.end()).then(|| first.saturating_sub(epoch.offset))
            }).min().unwrap_or(cursor);
        // Physical monitoring is causal, particularly MonitorOnly with a
        // negative authored offset. It still needs unshifted current input.
        if self.lanes.iter().any(|lane|&lane.binding==binding&&lane.monitor) {floor.min(cursor)}else{floor}
    }
    pub(super) fn camera_stop(&mut self,source:Id,cut:u64,raw:u64){
        for lane in self.lanes.iter_mut().filter(|lane|lane.source==source){
            lane.active=false;
            for epoch in &mut lane.epochs{
                epoch.raw_end=Some(epoch.raw_end.map_or(i128::from(raw),|old|old.min(i128::from(raw))));
                epoch.output_end=Some(epoch.output_end.map_or(i128::from(cut),|old|old.min(i128::from(cut))));
            }
        }
    }
    pub(super) fn camera_prefix_epoch(&self,source:Id,scope:u64,cursor:u64,cut:u64)->bool{
        self.lanes.iter().filter(|lane|lane.source==source).flat_map(|lane|&lane.epochs).filter(|epoch|epoch.history_epoch==scope)
            .any(|epoch|epoch.output_start<i128::from(cut)&&epoch.end()>i128::from(cursor)&&epoch.output_start<epoch.end())
    }
    pub(super) fn camera_prefix_routed(&self,source:Id,scope:Option<u64>,cursor:u64,cut:u64,
        routed:&dyn Fn(Id,&AudioInputKey,u64,u64)->bool)->bool{
        self.lanes.iter().filter(|lane|lane.source==source).any(|lane|lane.epochs.iter().filter(|epoch|scope.is_none_or(|scope|scope==epoch.history_epoch)).any(|epoch|{
            let start=epoch.output_start.max(i128::from(cursor)).max(0);
            let end=epoch.end().min(i128::from(cut));
            start<end&&routed(source,&lane.binding,u64::try_from(start).unwrap_or(u64::MAX),u64::try_from(end).unwrap_or(u64::MAX))
        }))
    }
    pub(super) fn raw_offset(&self, source: Id, binding: &AudioInputKey, raw: i128) -> Option<i128> {
        self.lanes.iter().find(|lane| lane.source == source && &lane.binding == binding)?
            .epochs.iter().rev().find(|e| raw >= e.raw_start && e.raw_end.is_none_or(|end| raw < end)).map(|e| e.offset)
    }
    pub(super) fn activation(&self, source: Id, binding: &AudioInputKey) -> Option<i128> {
        self.lanes.iter().find(|lane| lane.source == source && &lane.binding == binding)?.epochs.last().map(|e| e.raw_start)
    }
    pub(super) fn retire(&mut self, cursor: u64, camera_history:&[(Id,usize)]) {
        let cursor = i128::from(cursor);
        for lane in &mut self.lanes {
            let history=camera_history.iter().find_map(|(source,frames)|(*source==lane.source).then_some(*frames)).unwrap_or(0);
            let raw_floor=cursor.saturating_sub(history as i128);
            lane.epochs.retain(|e| e.end() > cursor || (history!=0 && e.raw_end.is_none_or(|end|end>raw_floor)));
        }
        self.lanes.retain(|lane| lane.active || !lane.epochs.is_empty());
    }
    /// Distinguishes authored delay silence from absent captured samples.
    /// Only real retained data is returned; no timestamp or generation retag.
    pub(super) fn read(&self, source: Id, binding: &AudioInputKey, at: i128, frames: usize,
        channels: usize, histories:&post_dsp::Histories, controls: &media_controls::MediaControls, out: &mut [f32])
        -> Result<usize, ProgramAudioError>
    {
        let Some(lane) = self.lanes.iter().find(|lane| lane.source == source && &lane.binding == binding) else {
            return Ok(frames);
        };
        let mut missing = 0;
        let mut offset = 0;
        while offset < frames {
            let t = at.checked_add(offset as i128).ok_or(ProgramAudioError::TimeOverflow)?;
            let selected = lane.epochs.iter().rev().find(|e|
                t >= e.output_start && t < e.end());
            let Some(epoch) = selected else { offset += 1; continue; };
            let end = (at + frames as i128).min(epoch.end());
            // A newer epoch starts before this span ends only if it replaced
            // old future placement; output_end already fences that boundary.
            let count = usize::try_from(end - t).map_err(|_| ProgramAudioError::TimeOverflow)?;
            let raw = t.checked_sub(epoch.offset).ok_or(ProgramAudioError::TimeOverflow)?;
            let slice = &mut out[offset * channels..(offset + count) * channels];
            let ring=histories.ring_epoch(source,binding,epoch.history_epoch);
            let gap = if let Some(ring) = ring { ring.read(raw, count, slice)? } else { count };
            missing += controls.apply(binding, ring, raw, count, channels, slice, gap);
            offset += count;
        }
        Ok(missing)
    }
}
