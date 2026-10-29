//! Independent live monitor clock. Program lookahead never delays audio-only
//! monitoring. Video-associated sources use OBS32's75ms/5s/stale-video rules.
use super::*;
pub(super) struct MonitorLane {
    pub(super) source: Id,
    source_epoch:u64,
    binding: AudioInputKey,
    raw: i128,
    last_video: Option<MediaTime>,
    video_changed_at: u64,
    camera_delay_scope: Option<(u64,u64,u64,u64,u64)>,
}
impl MonitorLane {
    pub(super) fn reader_floor(&self,source:Id,binding:&AudioInputKey,epoch:u64)->Option<i128>{
        (self.source==source&&&self.binding==binding&&self.source_epoch==epoch).then_some(self.raw)
    }
}
impl AudioProgram {
    pub(crate) fn monitor_reader_active(&mut self,active:bool){
        if !active{self.monitor_lanes.clear();}
    }

    pub(crate) fn render_live_monitor(&mut self, wall_frame: u64, padding_ns: u64)
        -> Result<Option<crate::audio_monitor::MonitorFrame>, ProgramAudioError>
    {
        self.purge_retired_ndi_inputs()?;
        let spans = self.timeline.segments(wall_frame, 1024)?;
        if !spans.iter().any(|span| span.plan.has_monitoring()) {
            self.monitor_lanes.clear(); self.monitor_dsp.reset(); return Ok(None);
        }
        let channels = usize::from(self.settings.channels());
        let rate = self.settings.sample_rate();
        let mut output = vec![0.0; 1024 * channels];
        for span in spans {
            let plan = span.plan.monitor_only_plan();
            let ids: Vec<_> = plan.sources().iter().map(|s| s.source).collect();
            self.monitor_lanes.retain(|lane| ids.contains(&lane.source));
            if plan.sources().is_empty(){continue;}
            let now_frame = wall_frame.checked_add(span.offset as u64).ok_or(ProgramAudioError::TimeOverflow)?;
            let mut buffers = Vec::with_capacity(plan.sources().len());
            for source in plan.sources() {
                let mut data = vec![0.0; span.frames * channels];
                if !self.camera_gates.enabled(source.source){buffers.push(data);continue;}
                let Some(ring)=self.histories.ring_epoch(source.source,&source.binding,self.camera_gates.scope(source.source))else{buffers.push(data);continue;};
                let activation = self.sync.activation(source.source, &source.binding).unwrap_or(i128::from(now_frame));
                let video_stamp = self.monitor_video.iter().find(|stamp|
                    stamp.source == source.source && stamp.binding == source.binding
                        && stamp.camera_delay_receipt.as_ref().is_none_or(|receipt|receipt.current()));
                // Never fall back to undelayed live PCM for an active NDI
                // delay whose sender timestamp or history is unavailable.
                if matches!(&source.binding,AudioInputKey::Ndi {..})
                    && self.camera_delay_history.iter().any(|(id,_)|*id==source.source)
                    && video_stamp.is_none_or(|stamp|stamp.camera_delay_receipt.is_none()) {
                    buffers.push(data);continue;
                }
                let video = video_stamp.map(|stamp| stamp.at);
                let delay_scope=video_stamp.and_then(|stamp|stamp.camera_delay_scope);
                let delayed_raw=if delay_scope.is_some() {
                    video.and_then(|time| i128::try_from((u128::from(time.0)*u128::from(rate))
                        .div_ceil(1_000_000_000)).ok())
                } else {None};
                let position = self.monitor_lanes.iter().position(|lane|
                    lane.source == source.source && lane.binding == source.binding && lane.source_epoch==self.camera_gates.scope(source.source));
                let index = if let Some(index) = position { index } else {
                    self.monitor_lanes.retain(|lane| lane.source != source.source);
                    if self.monitor_lanes.len() == crate::audio::MAX_AUDIO_INPUTS {
                        return Err(ProgramAudioError::Limit("monitor source cursors"));
                    }
                    self.monitor_lanes.push(MonitorLane { source: source.source, source_epoch:self.camera_gates.scope(source.source), binding: source.binding.clone(),
                        raw: activation.max(delayed_raw.unwrap_or(i128::from(now_frame))).max(ring.queue_head().unwrap_or(activation)),
                        last_video: None, video_changed_at: now_frame, camera_delay_scope:delay_scope });
                    self.monitor_lanes.len()-1
                };
                let lane = &mut self.monitor_lanes[index];
                // A new authenticated delayed stream/chain is an independent
                // history. Start at the captured video time if retained PCM
                // permits it; never seek before the activation or ring head.
                if delay_scope.is_some() && lane.camera_delay_scope != delay_scope {
                    if let Some(raw)=delayed_raw {
                        lane.raw=raw.max(activation).max(ring.queue_head().unwrap_or(activation));
                    }
                } else if delay_scope.is_none() && lane.camera_delay_scope.is_some() {
                    // The proof disappeared or the delay was disabled. Resume
                    // ordinary monitoring instead of silently retaining a
                    // previous delayed cursor without a valid clock mapping.
                    lane.raw=i128::from(now_frame).max(activation).max(ring.queue_head().unwrap_or(activation));
                }
                lane.camera_delay_scope=delay_scope;
                // A raw discontinuity retires only this monitor source, never
                // all sources sharing its hardware capture or another device.
                if ring.queue_head().is_some_and(|head| lane.raw < head) {
                    lane.raw = ring.queue_head().unwrap();
                }
                if video != lane.last_video { lane.last_video = video; lane.video_changed_at = now_frame; }
                let stale_after_ns=if delay_scope.is_some() && matches!(&source.binding,AudioInputKey::Ndi {..}) {
                    2_000_000_000u64
                } else {100_000_000u64};
                let stale_video = now_frame.saturating_sub(lane.video_changed_at) as u128 * 1_000_000_000
                    > u128::from(rate) * u128::from(stale_after_ns);
                if stale_video && delay_scope.is_some() && matches!(&source.binding,AudioInputKey::Ndi {..}) {
                    // Do not let a stopped sender's monitor cursor catch up
                    // to live audio while its last delayed image is frozen.
                    buffers.push(data);continue;
                }
                let has_video = match &source.binding {
                    AudioInputKey::Native(_) | AudioInputKey::Window {..} | AudioInputKey::EmbeddedCamera {..} => video.is_some(),
                    AudioInputKey::Media { media, .. } => media.video_dimensions().is_some(),
                    AudioInputKey::Ndi { .. } | AudioInputKey::Browser(_) => true,
                };
                let mut held = false;
                // At most22s of admitted ring history. This bounds the catch-up
                // scan without a packet metadata queue or duplicated PCM.
                let max_scan = ring.capacity().div_ceil(span.frames.max(1));
                for _ in 0..=max_scan {
                    let available = ring.end().saturating_sub(lane.raw);
                    if available <= 0 || (available < span.frames as i128 &&
                        self.histories.has_delayed_source(source.source,&source.binding,self.camera_gates.scope(source.source))) {
                        held = true; break;
                    }
                    let raw_ns = lane.raw.checked_mul(1_000_000_000).ok_or(ProgramAudioError::TimeOverflow)?.div_euclid(i128::from(rate));
                    let offset = self.sync.raw_offset(source.source, &source.binding, lane.raw).unwrap_or(0);
                    let shifted_ns = raw_ns.checked_add(offset * 1_000_000_000 / i128::from(rate))
                        .and_then(|value| value.checked_sub(i128::from(padding_ns))).ok_or(ProgramAudioError::TimeOverflow)?;
                    let difference = video.map(|video| shifted_ns - i128::from(video.0));
                    let bad = !has_video || video.is_none() || stale_video
                        // Unknown timestamps keep OBS's five-second safety
                        // limit. A validated delayed camera can legitimately
                        // be farther behind; its retention and identity were
                        // bounded when its monitor stamp was admitted.
                        || (delay_scope.is_none() && difference.is_some_and(|diff| diff.abs() > 5_000_000_000));
                    if !bad && difference.is_some_and(|diff| diff > 75_000_000) { held = true; break; }
                    if !bad && difference.is_some_and(|diff| diff < -75_000_000)
                        && available >= (span.frames * 2) as i128 {
                        lane.raw = lane.raw.checked_add(span.frames as i128).ok_or(ProgramAudioError::TimeOverflow)?;
                        continue;
                    }
                    break;
                }
                if !held {
                    let gap = ring.read(lane.raw, span.frames, &mut data)?;
                    self.media_controls.apply(&source.binding, Some(ring), lane.raw,
                        span.frames, channels, &mut data, gap);
                    lane.raw = lane.raw.checked_add(span.frames as i128).ok_or(ProgramAudioError::TimeOverflow)?;
                }
                buffers.push(data);
            }
            let inputs: Vec<_> = plan.sources().iter().zip(&buffers).map(|(source, data)| PcmInput::Converted {
                source: source.source, sample_rate: rate, channels: self.settings.channels(), samples: data,
                original: self.groups.get(&source.binding).map(|group| group.converter.input_format()),
            }).collect();
            let mixed = plan.mix_processed_on_grid(AudioMixRequest { sample_rate: rate, channels: self.settings.channels(),
                frames: span.frames, advance_ns: span.advance_ns }, &inputs, now_frame, &|source,frame|{let blocked=self.push.silent(source,frame);self.histories.gain_at(source,frame).map(|(gain,muted)|(gain,muted||blocked)).or(blocked.then_some((0.0,true)))})?;
            output[span.offset*channels..(span.offset+span.frames)*channels].copy_from_slice(&mixed.monitor_samples);
        }
        if self.has_retired_ndi_input(){return Err(ProgramAudioError::Invalid("NDI audio history changed during monitor rendering"));}
        Ok(Some(crate::audio_monitor::MonitorFrame { settings: self.settings, start_frame: wall_frame, samples: output }))
    }
}
