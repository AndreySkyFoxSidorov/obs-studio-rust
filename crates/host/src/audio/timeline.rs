use super::{AudioError, AudioInputKey, AudioPlan, MAX_AUDIO_FRAMES, derive_audio_plan};
use crate::transition::FrameComposition;
use serde::Serialize;
use skylive_core::{AudioCaptureBinding, AudioSettings, Id, Session};
use skylive_media::MediaTime;
use std::io::{self, Write};

// Two seconds of emission lookahead at up to60 control snapshots/second,
// plus the retained preceding route. The existing8MiB byte cap still applies.
const MAX_EVENTS: usize = 128;
const MAX_REQUIRED_BINDINGS: usize = super::MAX_AUDIO_BLOCK_SOURCES;
const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RequiredAudioBinding {
    pub source: Id,
    pub binding: AudioInputKey,
}
/// Validated owned routing metadata. Capture on the session owner; consuming
/// it on an audio worker never borrows a mutable Session or native resource.
#[derive(Clone, Debug)]
pub struct AudioRouteSnapshot {
    plan: AudioPlan,
    at: MediaTime,
    source_epochs:Vec<(Id,Option<skylive_core::SourceDefinitionEpoch>)>,
    // Retain processed PCM while an async camera filter is filling, before
    // the first delayed frame (and hence its timestamp proof) can arrive.
    camera_delay_history: Vec<(Id, usize)>,
    recording_camera_alignments: Vec<crate::audio_capture::RecordingCameraAlignment>,
    streaming_alignments: Vec<crate::audio_capture::RecordingCameraAlignment>,
    monitor_video: Vec<MonitorVideoStamp>,
    camera_activation:std::sync::Arc<crate::camera_activation::Snapshot>,
}
#[derive(Clone, Debug)]
pub(crate) struct MonitorVideoStamp {
    pub source: Id,
    pub binding: AudioInputKey,
    pub at: MediaTime,
    /// Identity of the camera sample whose observation time is stored in `at`.
    /// Other video providers still supply their existing presentation stamp.
    pub camera_sample: Option<(crate::capture::NativeFrameOrigin, u64, u64)>,
    /// Present only for a validated camera delay receipt and an unambiguous
    /// capture-clock calibration or bounded estimate. Identifies one authored delay generation.
    pub camera_delay_scope: Option<(u64, u64, u64, u64, u64)>,
    pub camera_delay_receipt: Option<crate::capture::AsyncDelayReceipt>,
    /// Present when the camera capture time is a bounded estimate.
    pub camera_estimate_uncertainty_ns: Option<u64>,
}
impl AudioRouteSnapshot {
    /// Apply one validated live mixer gesture to the output-time routing plan.
    /// The same plan feeds Program and monitoring, while source processing and
    /// canonical document history remain unchanged.
    pub(crate) fn with_mixer_gain_preview(mut self,session:&Session,preview:super::MixerGainPreview)->Result<Self,AudioError>{
        let model=session.snapshot();
        if session.is_closed()||preview.session!=session.identity()||preview.revision!=model.revision()
            ||self.session()!=preview.session||self.revision()!=preview.revision
            ||!preview.gain.is_finite()||!(0.0..=20.0).contains(&preview.gain) {
            return Err(AudioError::InvalidRequest("stale or invalid mixer gain preview"));
        }
        let source=model.source(preview.source).filter(|source|source.kind().has_audio()&&!source.mixer_state().volume_locked())
            .ok_or(AudioError::InvalidRequest("mixer preview source is unavailable or locked"))?;
        // A frozen transition can retain an older binding for the same source.
        // Never apply a current gesture to that replaced input accidentally.
        let binding=super::source_input_key(source).ok();
        if self.source_epochs.iter().any(|(id,epoch)|*id==preview.source&&epoch.is_some()&&*epoch==session.source_definition_epoch(preview.source)) {
            for source in &mut self.plan.sources {if source.source==preview.source&&binding.as_ref()==Some(&source.binding) {source.gain=preview.gain;}}
        }
        Ok(self)
    }
    pub fn capture(
        session: &Session,
        output: &FrameComposition,
        at: MediaTime,
    ) -> Result<Self, AudioError> {
        if session.is_closed() {
            return Err(AudioError::Closed);
        }
        let plan = derive_audio_plan(session, output)?;
        let frame = event_frame(at.0, plan.audio_settings().sample_rate())?;
        sample_ns(frame, plan.audio_settings().sample_rate())?;
        let mut source_epochs=Vec::new();
        for source in plan.processing_sources(){
            if source_epochs.len()>=super::MAX_AUDIO_BLOCK_SOURCES{return Err(AudioError::Limit("audio source epoch proof count"));}
            // A frozen Program scene can retain a removed source. It has no
            // live shortcut proof and must not inherit a later source's hold.
            let epoch=session.source_definition_epoch(source.source);
            source_epochs.try_reserve_exact(1).map_err(|_|AudioError::Limit("audio source epoch proof allocation"))?;
            source_epochs.push((source.source,epoch));
        }
        let model=session.snapshot();
        let rate=plan.audio_settings().sample_rate();
        let mut camera_delay_history=Vec::new();
        for control in plan.processing_sources() {
            let Some(source)=model.source(control.source) else {continue;};
            let delayed_audio_source=match source.kind() {
                skylive_core::SourceKind::VideoCapture{binding}=>binding.audio().is_some() || binding.embedded_audio(),
                skylive_core::SourceKind::NdiSource{ndi}=>ndi.audio(),
                _=>false,
            };
            if !delayed_audio_source || super::source_input_key(source).ok().as_ref()!=Some(&control.binding) {continue;}
            let delay_ms=source.video_filters().iter().filter_map(|filter| {
                if !filter.enabled || !model.preferences().plugin_enabled(filter.plugin_id()) {return None;}
                match filter.effect {skylive_core::VideoEffect::AsyncVideoDelay{delay_ms}=>Some(u64::from(delay_ms)),_=>None}
            }).try_fold(0u64,|total,value|total.checked_add(value)).ok_or(AudioError::Limit("camera delay history overflow"))?;
            // The monitor and current source-history profile admit at most 21s.
            // An unsupported longer chain is not silently represented as 21s.
            if delay_ms==0 {continue;}
            if delay_ms>21_000 {return Err(AudioError::Limit("camera async delay exceeds the 21 second source audio history"));}
            // NDI's sender-to-local clock is arrival anchored. Reserve one
            // additional second for bounded render/packet scheduling jitter;
            // network accuracy itself remains unknown and is never asserted.
            let retained_ms=if matches!(source.kind(),skylive_core::SourceKind::NdiSource{..}) {delay_ms.saturating_add(1000).min(21_000)} else {delay_ms};
            let frames=usize::try_from((u128::from(retained_ms)*u128::from(rate)).div_ceil(1000))
                .map_err(|_|AudioError::Limit("camera delay history frames"))?;
            camera_delay_history.try_reserve_exact(1).map_err(|_|AudioError::Limit("camera delay history metadata"))?;
            camera_delay_history.push((control.source,frames));
        }
        Ok(Self { plan, at,source_epochs,camera_delay_history,recording_camera_alignments:Vec::new(),streaming_alignments:Vec::new(), monitor_video: Vec::new(),camera_activation:std::sync::Arc::new(Default::default()) })
    }
    /// Carry recording-specific proofs to the audio owner without changing the
    /// Program mix. Each proof retains its live camera-generation guard.
    pub(crate) fn with_recording_camera_alignments(mut self, values:&[crate::audio_capture::RecordingCameraAlignment])->Result<Self,AudioError>{
        if values.len()>super::MAX_AUDIO_INPUTS {return Err(AudioError::Limit("recording camera alignment count"));}
        let mut copy=Vec::new();
        copy.try_reserve_exact(values.len()).map_err(|_|AudioError::Limit("recording camera alignment allocation"))?;
        for value in values {
            if value.session!=self.session()||value.revision!=self.revision()
                || value.sample_rate!=self.settings().sample_rate()||!value.current()
                || copy.iter().any(|old:&crate::audio_capture::RecordingCameraAlignment|old.source==value.source) {
                return Err(AudioError::InvalidRequest("stale or duplicate recording camera alignment"));
            }
            copy.push(value.clone());
        }
        self.recording_camera_alignments=copy;
        Ok(self)
    }
    pub(crate) fn recording_camera_alignments(&self)->&[crate::audio_capture::RecordingCameraAlignment]{&self.recording_camera_alignments}
    pub(crate) fn with_streaming_alignments(mut self,values:&[crate::audio_capture::RecordingCameraAlignment])->Result<Self,AudioError>{
        if values.len()>super::MAX_AUDIO_INPUTS{return Err(AudioError::Limit("streaming alignment count"));}
        let mut copy=Vec::new();copy.try_reserve_exact(values.len()).map_err(|_|AudioError::Limit("streaming alignment allocation"))?;
        for value in values {
            if value.session!=self.session()||value.revision!=self.revision()||value.sample_rate!=self.settings().sample_rate()
                ||!value.current()||copy.iter().any(|old:&crate::audio_capture::RecordingCameraAlignment|old.source==value.source) {
                return Err(AudioError::InvalidRequest("stale or duplicate streaming alignment"));
            }
            copy.push(value.clone());
        }
        self.streaming_alignments=copy;Ok(self)
    }
    pub(crate) fn streaming_alignments(&self)->&[crate::audio_capture::RecordingCameraAlignment]{&self.streaming_alignments}
    pub(crate) fn with_camera_activation(mut self,policy:std::sync::Arc<crate::camera_activation::Snapshot>)->Result<Self,AudioError>{
        if policy.session()!=0&&policy.session()!=self.session(){return Err(AudioError::InvalidRequest("foreign camera activation route"));}
        self.plan.retain_camera_eligibility(&policy);self.camera_activation=policy;Ok(self)
    }
    pub(crate) fn at_least_cut(mut self,cut:crate::camera_activation::CameraAudioCut)->Result<Self,AudioError>{
        if cut.sample_rate!=self.settings().sample_rate(){return Err(AudioError::InvalidRequest("camera activation audio grid changed"));}
        self.at=MediaTime(self.at.0.max(sample_ns(cut.frame,cut.sample_rate)?));Ok(self)
    }
    pub(crate) fn camera_activation(&self)->&[crate::camera_activation::SourceEpoch]{self.camera_activation.sources()}
    pub(crate) fn with_monitor_video(mut self, stamps: &[MonitorVideoStamp]) -> Self {
        self.monitor_video = stamps.iter().filter(|stamp| stamp.camera_delay_receipt.as_ref().is_none_or(|receipt|receipt.current()) && stamp.at.0 <= self.at.0 && self.plan.sources().iter()
            .any(|source| source.source == stamp.source && source.binding == stamp.binding)).cloned().collect();
        self
    }
    pub(crate) fn monitor_video(&self) -> &[MonitorVideoStamp] { &self.monitor_video }
    pub(crate) fn camera_delay_history(&self)->&[(Id,usize)] {&self.camera_delay_history}
    pub fn monitor_config(&self)->Option<crate::audio_monitor::MonitorConfig>{
        self.plan.has_monitoring().then(||crate::audio_monitor::MonitorConfig {session:self.session(),settings:self.settings(),device:self.plan.monitoring_device().clone()})
    }
    pub fn session(&self) -> u64 {
        self.plan.session()
    }
    pub fn revision(&self) -> u64 {
        self.plan.revision()
    }
    pub fn settings(&self) -> AudioSettings {
        self.plan.audio_settings()
    }
    pub fn at(&self) -> MediaTime {
        self.at
    }
    pub(crate) fn plan(&self)->&super::AudioPlan {&self.plan}
    pub(crate) fn source_epochs(&self)->&[(Id,Option<skylive_core::SourceDefinitionEpoch>)]{&self.source_epochs}
    pub(crate) fn processing_controls(&self)->impl Iterator<Item=&super::AudioSourcePlan>{self.plan.processing_sources()}
    pub(crate) fn source_controls(&self) -> &[super::AudioSourcePlan] { self.plan.sources() }
    pub fn required_inputs(&self) -> impl Iterator<Item = (Id, &AudioInputKey)> + '_ {
        self.plan
            .processing_sources()
            .map(|source| (source.source, &source.binding))
    }
    pub fn required_bindings(&self) -> impl Iterator<Item = (Id, &AudioCaptureBinding)> + '_ {
        self.plan.processing_sources().filter_map(|source| {
            source
                .binding
                .as_native()
                .map(|binding| (source.source, binding))
        })
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct AudioRouteSpan {
    pub offset: usize,
    pub frames: usize,
    pub plan: AudioPlan,
    pub advance_ns: u64,
    pub routing_serial: u64,
}
#[derive(Clone, Debug)]
struct Event {
    at_ns: u64,
    frame: u64,
    serial: u64,
    bytes: usize,
    plan: AudioPlan,
}

/// Metadata-only Program history on one output sample grid. Retained plans can
/// intentionally evaluate an old document revision after the live owner moves on.
#[derive(Debug)]
pub struct AudioRouteTimeline {
    sample_rate: u32,
    session: Option<u64>,
    revision: Option<u64>,
    observed_at: Option<u64>,
    serial: u64,
    retired_before: u64,
    metadata_bytes: usize,
    events: Vec<Event>,
    required: Vec<RequiredAudioBinding>,
}
impl AudioRouteTimeline {
    /// Output-time track routing, independent from source processing/sync epochs.
    pub(crate) fn source_routes_between(&self,source:Id,binding:&AudioInputKey,start:u64,end:u64,tracks:skylive_core::SourceAudioTrackMask)->bool{
        if start>=end||tracks.is_empty(){return false;}
        self.events.iter().enumerate().any(|(index,event)|{
            let until=self.events.get(index+1).map_or(u64::MAX,|next|next.frame);
            event.frame<end&&until>start&&event.plan.sources().iter().any(|input|input.source==source&&&input.binding==binding
                &&input.monitoring!=skylive_core::AudioMonitoringMode::MonitorOnly
                &&(input.routes.global||input.routes.from||input.routes.to||input.routes.duplicated)
                &&input.properties.audio_tracks().intersects(tracks))
        })
    }

    pub(crate) fn first_frame(&self)->Option<u64>{self.events.first().map(|event|event.frame)}
    pub fn new(sample_rate: u32) -> Result<Self, AudioError> {
        validate_rate(sample_rate)?;
        let mut events = Vec::new();
        events
            .try_reserve_exact(MAX_EVENTS)
            .map_err(|_| AudioError::Limit("audio route event allocation"))?;
        Ok(Self {
            sample_rate,
            session: None,
            revision: None,
            observed_at: None,
            serial: 0,
            retired_before: 0,
            metadata_bytes: 0,
            events,
            required: Vec::new(),
        })
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn session(&self) -> Option<u64> {
        self.session
    }
    pub fn routing_serial(&self) -> u64 {
        self.serial
    }
    pub fn event_count(&self) -> usize {
        self.events.len()
    }
    pub fn required_bindings(&self) -> &[RequiredAudioBinding] {
        &self.required
    }

    pub fn push(
        &mut self,
        session: &Session,
        output: &FrameComposition,
        at: MediaTime,
    ) -> Result<bool, AudioError> {
        self.push_snapshot(AudioRouteSnapshot::capture(session, output, at)?)
    }
    pub fn push_snapshot(&mut self, snapshot: AudioRouteSnapshot) -> Result<bool, AudioError> {
        let AudioRouteSnapshot { plan, at, .. } = snapshot;
        if self.session.is_some_and(|owner| owner != plan.session()) {
            return Err(AudioError::InvalidRequest(
                "audio routes require explicit reset for a new session",
            ));
        }
        if plan.audio_settings().sample_rate() != self.sample_rate {
            return Err(AudioError::InvalidRequest(
                "audio route grid does not match project sample rate",
            ));
        }
        if self
            .revision
            .is_some_and(|revision| plan.revision() < revision)
        {
            return Err(AudioError::InvalidRequest(
                "audio route revision moved backwards",
            ));
        }
        if self.observed_at.is_some_and(|previous| at.0 < previous) {
            return Err(AudioError::InvalidRequest(
                "audio route event time moved backwards",
            ));
        }
        let frame = event_frame(at.0, self.sample_rate)?;
        // A final nanosecond can round up to a sample whose timestamp cannot
        // be represented. Reject before publishing unusable route history.
        sample_ns(frame, self.sample_rate)?;
        if frame < self.retired_before {
            return Err(AudioError::InvalidRequest(
                "audio route event is before the retired sample cursor",
            ));
        }
        if self
            .events
            .last()
            .is_some_and(|last| last.plan.same_routing_as(&plan))
        {
            // Configuration-equivalent edits still advance the meter's
            // authored-revision receipt. Keep the original curve/time anchor;
            // adding a route here would reset no signal but exhaust history on
            // ordinary canvas edits. Published plans/blocks are owned clones.
            let last = self.events.last().ok_or(AudioError::InvalidRequest(
                "same route has no retained event",
            ))?;
            if last.plan.revision() != plan.revision() {
                // revision is the only changed serialized field (ordinary JSON
                // u64). Charge its exact decimal width without cloning source
                // bindings/filter vectors on the audio owner.
                let digits = |revision: u64| {
                    if revision == 0 {
                        1
                    } else {
                        revision.ilog10() as usize + 1
                    }
                };
                let bytes = last
                    .bytes
                    .checked_sub(digits(last.plan.revision()))
                    .and_then(|bytes| bytes.checked_add(digits(plan.revision())))
                    .ok_or(AudioError::Limit("audio meter revision bytes"))?;
                let metadata_bytes = self
                    .metadata_bytes
                    .checked_sub(last.bytes)
                    .and_then(|total| total.checked_add(bytes))
                    .filter(|total| *total <= MAX_METADATA_BYTES)
                    .ok_or(AudioError::Limit(
                        "retained audio route metadata exceeds8MiB",
                    ))?;
                let last = self.events.last_mut().ok_or(AudioError::InvalidRequest(
                    "same route has no retained event",
                ))?;
                last.plan.revision = plan.revision();
                last.bytes = bytes;
                self.metadata_bytes = metadata_bytes;
            }
            self.observed_at = Some(at.0);
            self.revision = Some(plan.revision());
            return Ok(false);
        }
        let replace = self.events.last().is_some_and(|last| last.at_ns == at.0);
        let keep = self.events.len() - usize::from(replace);
        if keep >= MAX_EVENTS {
            return Err(AudioError::Limit("retained audio routes exceed 128 events"));
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(AudioError::Limit("audio route serial exhausted"))?;
        let bytes = encoded_size(&plan)?;
        let previous = if replace {
            self.events.last().map_or(0, |event| event.bytes)
        } else {
            0
        };
        let metadata_bytes = self
            .metadata_bytes
            .checked_sub(previous)
            .and_then(|value| value.checked_add(bytes))
            .filter(|bytes| *bytes <= MAX_METADATA_BYTES)
            .ok_or(AudioError::Limit(
                "retained audio route metadata exceeds 8 MiB",
            ))?;
        let required = gather_required(
            self.events
                .iter()
                .take(keep)
                .map(|event| &event.plan)
                .chain(std::iter::once(&plan)),
        )?;
        let session = plan.session();
        let revision = plan.revision();
        let event = Event {
            at_ns: at.0,
            frame,
            serial,
            bytes,
            plan,
        };
        if replace {
            self.events.pop();
        }
        self.events.push(event);
        self.required = required;
        self.metadata_bytes = metadata_bytes;
        self.serial = serial;
        self.session = Some(session);
        self.revision = Some(revision);
        self.observed_at = Some(at.0);
        Ok(true)
    }

    pub fn segments(
        &self,
        start_frame: u64,
        frames: usize,
    ) -> Result<Vec<AudioRouteSpan>, AudioError> {
        if frames == 0 || frames > MAX_AUDIO_FRAMES {
            return Err(AudioError::InvalidRequest(
                "audio route block frames must be 1..4096",
            ));
        }
        if start_frame < self.retired_before {
            return Err(AudioError::InvalidRequest(
                "audio route block precedes retired history",
            ));
        }
        let end = start_frame
            .checked_add(frames as u64)
            .ok_or(AudioError::TimeOverflow)?;
        // Admit the complete requested block's temporal range before cloning a plan.
        sample_ns(end - 1, self.sample_rate)?;
        let mut selected = self
            .events
            .iter()
            .rposition(|event| event.frame <= start_frame)
            .ok_or(AudioError::InvalidRequest(
                "audio route history does not cover block start",
            ))?;
        let mut cursor = start_frame;
        let mut spans = Vec::new();
        spans
            .try_reserve_exact(self.events.len())
            .map_err(|_| AudioError::Limit("audio route span allocation"))?;
        while cursor < end {
            // Multiple meaningful events can quantize to one sample. Only the
            // last one controls that sample; their metadata remains retained.
            while selected + 1 < self.events.len() && self.events[selected + 1].frame <= cursor {
                selected += 1;
            }
            let event = &self.events[selected];
            let next = self
                .events
                .get(selected + 1)
                .map_or(end, |next| next.frame.min(end));
            let count = usize::try_from(next - cursor).map_err(|_| AudioError::TimeOverflow)?;
            if count == 0 {
                return Err(AudioError::InvalidRequest("empty audio route interval"));
            }
            let advance_ns = sample_ns(cursor, self.sample_rate)?
                .checked_sub(event.at_ns)
                .ok_or(AudioError::TimeOverflow)?;
            // A late block cannot defer an elapsed overflow until a partially
            // evaluated program has already been returned to the caller.
            let final_advance = sample_ns(next - 1, self.sample_rate)?
                .checked_sub(event.at_ns)
                .ok_or(AudioError::TimeOverflow)?;
            event.plan.curve.factors(final_advance)?;
            spans.push(AudioRouteSpan {
                offset: (cursor - start_frame) as usize,
                frames: count,
                plan: event.plan.clone(),
                advance_ns,
                routing_serial: event.serial,
            });
            cursor = next;
        }
        Ok(spans)
    }

    pub fn retire_before(&mut self, frame: u64) -> Result<bool, AudioError> {
        if frame < self.retired_before {
            return Err(AudioError::InvalidRequest(
                "audio route retirement moved backwards",
            ));
        }
        let baseline = self
            .events
            .iter()
            .rposition(|event| event.frame <= frame)
            .ok_or(AudioError::InvalidRequest(
                "cannot retire before the first audio route event",
            ))?;
        if frame == self.retired_before && baseline == 0 {
            return Ok(false);
        }
        let required = gather_required(self.events.iter().skip(baseline).map(|event| &event.plan))?;
        let bytes = self
            .events
            .iter()
            .skip(baseline)
            .map(|event| event.bytes)
            .sum();
        self.events.drain(..baseline);
        self.required = required;
        self.metadata_bytes = bytes;
        self.retired_before = frame;
        Ok(true)
    }
    pub fn reset(&mut self) -> Result<(), AudioError> {
        self.rebase(self.sample_rate)
    }
    pub fn rebase(&mut self, sample_rate: u32) -> Result<(), AudioError> {
        validate_rate(sample_rate)?;
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(AudioError::Limit("audio route serial exhausted"))?;
        self.events.clear();
        self.required.clear();
        self.session = None;
        self.revision = None;
        self.observed_at = None;
        self.retired_before = 0;
        self.metadata_bytes = 0;
        self.serial = serial;
        self.sample_rate = sample_rate;
        Ok(())
    }
}
fn validate_rate(sample_rate: u32) -> Result<(), AudioError> {
    if !(8000..=192000).contains(&sample_rate) {
        return Err(AudioError::InvalidRequest(
            "audio route rate must be 8000..192000 Hz",
        ));
    }
    Ok(())
}
fn event_frame(ns: u64, rate: u32) -> Result<u64, AudioError> {
    u64::try_from((u128::from(ns) * u128::from(rate)).div_ceil(1_000_000_000))
        .map_err(|_| AudioError::TimeOverflow)
}
fn sample_ns(frame: u64, rate: u32) -> Result<u64, AudioError> {
    u64::try_from(u128::from(frame) * 1_000_000_000 / u128::from(rate))
        .map_err(|_| AudioError::TimeOverflow)
}
fn gather_required<'a>(
    plans: impl Iterator<Item = &'a AudioPlan>,
) -> Result<Vec<RequiredAudioBinding>, AudioError> {
    let mut required = Vec::new();
    required
        .try_reserve_exact(MAX_REQUIRED_BINDINGS)
        .map_err(|_| AudioError::Limit("audio binding union allocation"))?;
    for plan in plans {
        for source in plan.processing_sources() {
            if required.iter().any(|old: &RequiredAudioBinding| {
                old.source == source.source && old.binding == source.binding
            }) {
                continue;
            }
            if required.len() >= MAX_REQUIRED_BINDINGS {
                return Err(AudioError::Limit(
                    "retained audio routes exceed 128 source bindings",
                ));
            }
            required.push(RequiredAudioBinding {
                source: source.source,
                binding: source.binding.clone(),
            });
        }
    }
    Ok(required)
}
struct Counter {
    bytes: usize,
}
impl Write for Counter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(data.len())
            .filter(|bytes| *bytes <= MAX_METADATA_BYTES)
            .ok_or_else(|| io::Error::other("audio route metadata limit"))?;
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encoded_size(plan: &AudioPlan) -> Result<usize, AudioError> {
    let mut counter = Counter { bytes: 0 };
    serde_json::to_writer(&mut counter, plan)
        .map_err(|_| AudioError::Limit("audio route encoded metadata limit"))?;
    Ok(counter.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioMixRequest, PcmInput};
    use crate::transition::FrameScene;
    use skylive_core::{
        AudioFlow, AudioSettings, AudioTarget, Command, SpeakerLayout, TransitionKind,
    };
    use skylive_media::fade_audio_gains;
    #[test]
    fn equivalent_revision_retags_meter_receipt_with_exact_bytes_and_keeps_published_plan_immutable()
     {
        let mut session = Session::new();
        let scene = session.preview();
        add(&mut session, scene, "Audio");
        while session.snapshot().revision() < 9 {
            session
                .apply(Command::RenameScene {
                    scene,
                    name: format!("Scene{}", session.snapshot().revision() + 1),
                })
                .unwrap();
        }
        let initial = AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(0)).unwrap();
        let mut timeline = AudioRouteTimeline::new(48000).unwrap();
        timeline.push_snapshot(initial.clone()).unwrap();
        let old = std::sync::Arc::new(timeline.segments(0, 8).unwrap().remove(0).plan);
        let old_json = serde_json::to_value(&old).unwrap();
        let bytes = timeline.metadata_bytes;
        let serial = timeline.routing_serial();
        session
            .apply(Command::RenameScene {
                scene,
                name: "Scene10".into(),
            })
            .unwrap();
        assert_eq!(session.snapshot().revision(), 10);
        assert!(
            !timeline
                .push(&session, &view(scene), MediaTime(1_000_000))
                .unwrap()
        );
        assert_eq!(timeline.routing_serial(), serial);
        assert_eq!(timeline.event_count(), 1);
        assert_eq!(timeline.metadata_bytes, bytes + 1);
        assert_eq!(
            timeline.events[0].bytes,
            encoded_size(&timeline.events[0].plan).unwrap()
        );
        assert_eq!(timeline.events[0].plan.revision(), 10);
        assert_eq!(old.revision(), 9);
        assert_eq!(serde_json::to_value(&old).unwrap(), old_json);
        let mut full = AudioRouteTimeline::new(48000).unwrap();
        full.push_snapshot(initial).unwrap();
        full.metadata_bytes = MAX_METADATA_BYTES;
        let before = (
            full.routing_serial(),
            full.observed_at,
            full.revision,
            full.events[0].plan.revision(),
            full.events[0].bytes,
        );
        assert!(matches!(
            full.push(&session, &view(scene), MediaTime(1_000_000)),
            Err(AudioError::Limit(_))
        ));
        assert_eq!(
            (
                full.routing_serial(),
                full.observed_at,
                full.revision,
                full.events[0].plan.revision(),
                full.events[0].bytes
            ),
            before
        );
        assert_eq!(full.metadata_bytes, MAX_METADATA_BYTES);
    }

    fn binding(id: &str) -> AudioCaptureBinding {
        AudioCaptureBinding::new(
            AudioFlow::Input,
            AudioTarget::WindowsEndpoint { id: id.into() },
            false,
            false,
        )
        .unwrap()
    }
    fn add(session: &mut Session, scene: Id, name: &str) -> Id {
        let item = session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: name.into(),
                binding: binding(name),
            })
            .unwrap()
            .created
            .unwrap();
        session
            .snapshot()
            .scene(scene)
            .unwrap()
            .items()
            .iter()
            .find(|row| row.id() == item)
            .unwrap()
            .source()
    }
    fn new_scene(session: &mut Session, name: &str) -> Id {
        session
            .apply(Command::AddScene { name: name.into() })
            .unwrap()
            .created
            .unwrap()
    }
    fn view(scene: Id) -> FrameComposition {
        FrameComposition::Scene(FrameScene::Live(scene))
    }
    fn fade(a: Id, b: Id, elapsed_ns: u64) -> FrameComposition {
        FrameComposition::Fade {
            from: FrameScene::Live(a),
            to: FrameScene::Live(b),
            elapsed_ns,
            duration_ns: 100_000,
            epoch: 10,
        }
    }
    fn mix(span: &AudioRouteSpan, start: u64, rate: u32) -> Vec<f32> {
        let buffers: Vec<_> = span
            .plan
            .sources()
            .iter()
            .map(|source| (source.source, vec![1.0; span.frames]))
            .collect();
        let inputs: Vec<_> = buffers
            .iter()
            .map(|(source, samples)| PcmInput::Samples {
                source: *source,
                sample_rate: rate,
                channels: 1,
                samples,
            })
            .collect();
        span.plan
            .mix_on_grid(
                AudioMixRequest {
                    sample_rate: rate,
                    channels: 1,
                    frames: span.frames,
                    advance_ns: span.advance_ns,
                },
                &inputs,
                start + span.offset as u64,
            )
            .unwrap()
            .samples
    }

    #[test]
    fn cut_is_quantized_never_early_and_future_event_does_not_replace_old_block() {
        let mut session = Session::new();
        let a = session.preview();
        let b = new_scene(&mut session, "B");
        let source_a = add(&mut session, a, "A audio");
        let source_b = add(&mut session, b, "B audio");
        let mut timeline = AudioRouteTimeline::new(48000).unwrap();
        timeline.push(&session, &view(a), MediaTime(0)).unwrap();
        timeline
            .push(&session, &view(b), MediaTime(20_834))
            .unwrap(); // strictly after sample 1
        let spans = timeline.segments(0, 4).unwrap();
        assert_eq!(
            spans
                .iter()
                .map(|s| (s.offset, s.frames))
                .collect::<Vec<_>>(),
            [(0, 2), (2, 2)]
        );
        assert_eq!(spans[0].plan.sources()[0].source, source_a);
        assert_eq!(spans[1].plan.sources()[0].source, source_b);
        assert_eq!(spans[1].advance_ns, 41_666 - 20_834);
        assert_eq!(
            timeline.segments(0, 1).unwrap()[0].plan.sources()[0].source,
            source_a
        );
    }

    #[test]
    fn fade_continuations_do_not_fill_history_and_nonzero_grid_phase_is_exact() {
        let mut session = Session::new();
        let a = session.preview();
        let b = new_scene(&mut session, "B");
        session
            .apply(Command::SetAudioSettings {
                settings: AudioSettings::new(44100, SpeakerLayout::Mono).unwrap(),
            })
            .unwrap();
        let source = add(&mut session, a, "A audio");
        let mut timeline = AudioRouteTimeline::new(44100).unwrap();
        assert!(
            timeline
                .push(&session, &fade(a, b, 0), MediaTime(0))
                .unwrap()
        );
        assert!(
            !timeline
                .push(&session, &fade(a, b, 20_000), MediaTime(20_000))
                .unwrap()
        );
        assert_eq!(timeline.event_count(), 1);
        let span = &timeline.segments(1, 2).unwrap()[0];
        let samples = mix(span, 1, 44100);
        assert_eq!(
            samples,
            [
                fade_audio_gains(22_675, 100_000).unwrap().0,
                fade_audio_gains(45_351, 100_000).unwrap().0
            ]
        );
        let local = span
            .plan
            .mix(
                AudioMixRequest {
                    sample_rate: 44100,
                    channels: 1,
                    frames: 2,
                    advance_ns: span.advance_ns,
                },
                &[PcmInput::Samples {
                    source,
                    sample_rate: 44100,
                    channels: 1,
                    samples: &[1.0; 2],
                }],
            )
            .unwrap();
        assert_ne!(
            local.samples[1], samples[1],
            "independent local floors lose one nanosecond of phase"
        );
    }

    #[test]
    fn gain_mute_and_binding_changes_are_retained_and_same_time_last_writer_wins() {
        let mut session = Session::new();
        let a = session.preview();
        let source = add(&mut session, a, "A audio");
        let mut timeline = AudioRouteTimeline::new(48000).unwrap();
        timeline.push(&session, &view(a), MediaTime(0)).unwrap();
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.5,
                muted: false,
            })
            .unwrap();
        timeline
            .push(&session, &view(a), MediaTime(20_000))
            .unwrap();
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.25,
                muted: false,
            })
            .unwrap();
        timeline
            .push(&session, &view(a), MediaTime(20_000))
            .unwrap();
        assert_eq!(timeline.event_count(), 2);
        let spans = timeline.segments(0, 3).unwrap();
        assert_eq!(mix(&spans[0], 0, 48000), [1.0]);
        assert_eq!(mix(&spans[1], 0, 48000), [0.25, 0.25]);
        session
            .apply(Command::RebindAudioCaptureSource {
                source,
                binding: binding("Replacement"),
            })
            .unwrap();
        timeline
            .push(&session, &view(a), MediaTime(40_000))
            .unwrap();
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.25,
                muted: true,
            })
            .unwrap();
        timeline
            .push(&session, &view(a), MediaTime(60_000))
            .unwrap();
        assert_eq!(timeline.required_bindings().len(), 2);
        assert_eq!(mix(&timeline.segments(3, 1).unwrap()[0], 3, 48000), [0.0]);
        timeline.retire_before(2).unwrap();
        assert_eq!(timeline.required_bindings().len(), 1);
        assert!(timeline.segments(1, 1).is_err());
    }

    #[test]
    fn manual_changes_hold_until_next_event_and_equivalent_rationals_are_noops() {
        let mut session = Session::new();
        let a = session.preview();
        let b = new_scene(&mut session, "B");
        add(&mut session, a, "A audio");
        let manual = |n, d| FrameComposition::Manual {
            from: Some(FrameScene::Live(a)),
            to: Some(FrameScene::Live(b)),
            kind: TransitionKind::Fade,
            logical_scene: b,
            progress_num: n,
            progress_den: d,
            epoch: 7,
            next_after_ns: None,
        };
        let mut timeline = AudioRouteTimeline::new(48000).unwrap();
        timeline
            .push(&session, &manual(1, 4), MediaTime(0))
            .unwrap();
        assert!(
            !timeline
                .push(&session, &manual(2, 8), MediaTime(1))
                .unwrap()
        );
        timeline
            .push(&session, &manual(3, 4), MediaTime(20_834))
            .unwrap();
        let spans = timeline.segments(0, 4).unwrap();
        assert_eq!(mix(&spans[0], 0, 48000), [0.75, 0.75]);
        assert_eq!(mix(&spans[1], 0, 48000), [0.25, 0.25]);
        assert!(
            timeline
                .push(&session, &manual(1, 4), MediaTime(0))
                .is_err(),
            "even no-op observations advance monotonic time"
        );
    }

    #[test]
    fn fade_to_black_and_retained_deleted_source_are_evaluated_offline() {
        let mut session = Session::new();
        let a = session.preview();
        let b = new_scene(&mut session, "B");
        let source = add(&mut session, a, "A audio");
        let mut timeline = AudioRouteTimeline::new(48000).unwrap();
        let black = FrameComposition::SingleFade {
            scene: FrameScene::Live(a),
            logical_scene: b,
            incoming: false,
            elapsed_ns: 0,
            duration_ns: 41_666,
            epoch: 9,
        };
        timeline.push(&session, &black, MediaTime(0)).unwrap();
        timeline
            .push(
                &session,
                &FrameComposition::Black {
                    logical_scene: b,
                    epoch: 9,
                },
                MediaTime(41_666),
            )
            .unwrap();
        session.apply(Command::RemoveScene { scene: a }).unwrap();
        assert!(session.snapshot().source(source).is_none());
        let spans = timeline.segments(0, 4).unwrap();
        let old = mix(&spans[0], 0, 48000);
        assert_eq!(old, [1.0, 0.5]);
        assert!(spans[1].plan.sources().is_empty());
        assert_eq!(mix(&spans[1], 0, 48000), [0.0, 0.0]);
        assert_eq!(timeline.required_bindings()[0].source, source);
        timeline.retire_before(2).unwrap();
        assert!(timeline.required_bindings().is_empty());
    }

    #[test]
    fn distinct_subsample_events_remain_retained_but_latest_controls_sample() {
        let mut session = Session::new();
        let a = session.preview();
        let source = add(&mut session, a, "A audio");
        let mut timeline = AudioRouteTimeline::new(48000).unwrap();
        timeline.push(&session, &view(a), MediaTime(0)).unwrap();
        for (time, gain) in [(1, 0.75), (2, 0.5), (3, 0.25)] {
            session
                .apply(Command::SetAudio {
                    source,
                    gain,
                    muted: false,
                })
                .unwrap();
            timeline.push(&session, &view(a), MediaTime(time)).unwrap();
        }
        assert_eq!(timeline.event_count(), 4);
        let spans = timeline.segments(0, 3).unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!(mix(&spans[1], 0, 48000), [0.25, 0.25]);
    }

    #[test]
    fn event_limit_retirement_and_serial_overflow_are_atomic() {
        let mut session = Session::new();
        let a = session.preview();
        let source = add(&mut session, a, "A audio");
        let mut timeline = AudioRouteTimeline::new(48000).unwrap();
        for time in 0..64 {
            session
                .apply(Command::SetAudio {
                    source,
                    gain: if time % 2 == 0 { 1.0 } else { 0.5 },
                    muted: false,
                })
                .unwrap();
            timeline
                .push(&session, &view(a), MediaTime(time * 100_000))
                .unwrap();
        }
        let serial = timeline.routing_serial();
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.75,
                muted: false,
            })
            .unwrap();
        assert!(matches!(
            timeline.push(&session, &view(a), MediaTime(6_400_000)),
            Err(AudioError::Limit(_))
        ));
        assert_eq!(timeline.event_count(), 64);
        assert_eq!(timeline.routing_serial(), serial);
        timeline
            .retire_before(event_frame(6_300_000, 48000).unwrap())
            .unwrap();
        assert!(
            timeline
                .push(&session, &view(a), MediaTime(6_400_000))
                .unwrap()
        );
        assert_eq!(timeline.event_count(), 2);
        timeline.serial = u64::MAX;
        let count = timeline.event_count();
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.25,
                muted: false,
            })
            .unwrap();
        assert!(
            timeline
                .push(&session, &view(a), MediaTime(6_500_000))
                .is_err()
        );
        assert_eq!(timeline.event_count(), count);
        assert!(timeline.reset().is_err());
        assert_eq!(timeline.event_count(), count);
    }

    #[test]
    fn bounded_binding_union_and_metadata_writer_refuse_excess_without_large_buffers() {
        let mut session = Session::new();
        let a = session.preview();
        add(&mut session, a, "A audio");
        let base = derive_audio_plan(&session, &view(a)).unwrap();
        let plans: Vec<_> = (0..129)
            .map(|index| {
                let mut plan = base.clone();
                plan.sources[0].binding = binding(&format!("Endpoint{index}")).into();
                plan
            })
            .collect();
        assert_eq!(gather_required(plans[..128].iter()).unwrap().len(), 128);
        assert!(matches!(
            gather_required(plans.iter()),
            Err(AudioError::Limit(_))
        ));
        let mut counter = Counter {
            bytes: MAX_METADATA_BYTES - 2,
        };
        assert_eq!(counter.write(b"xx").unwrap(), 2);
        assert!(counter.write(b"x").is_err());
        let mut timeline = AudioRouteTimeline::new(48000).unwrap();
        timeline.metadata_bytes = MAX_METADATA_BYTES;
        assert!(timeline.push(&session, &view(a), MediaTime(0)).is_err());
        assert_eq!(timeline.event_count(), 0);
        assert_eq!(timeline.session(), None);
    }

    #[test]
    fn lifecycle_missing_history_rate_and_temporal_errors_preserve_history() {
        let mut session = Session::new();
        let a = session.preview();
        let mut timeline = AudioRouteTimeline::new(48000).unwrap();
        assert!(timeline.segments(0, 1).is_err());
        timeline
            .push(&session, &view(a), MediaTime(30_000))
            .unwrap();
        assert!(timeline.segments(0, 1).is_err());
        assert!(timeline.segments(2, 0).is_err());
        assert!(timeline.segments(2, 4097).is_err());
        assert!(matches!(
            timeline.segments(u64::MAX, 1),
            Err(AudioError::TimeOverflow)
        ));
        assert!(
            timeline
                .push(&session, &view(a), MediaTime(29_999))
                .is_err()
        );
        assert!(matches!(
            timeline.push(&session, &view(a), MediaTime(u64::MAX)),
            Err(AudioError::TimeOverflow)
        ));
        let foreign = Session::new();
        assert!(
            timeline
                .push(&foreign, &view(foreign.preview()), MediaTime(40_000))
                .is_err()
        );
        session
            .apply(Command::SetAudioSettings {
                settings: AudioSettings::new(44100, SpeakerLayout::Stereo).unwrap(),
            })
            .unwrap();
        assert!(
            timeline
                .push(&session, &view(a), MediaTime(40_000))
                .is_err()
        );
        assert_eq!(timeline.event_count(), 1);
        timeline.rebase(44100).unwrap();
        assert_eq!(timeline.event_count(), 0);
        assert_eq!(timeline.session(), None);
        timeline.push(&session, &view(a), MediaTime(0)).unwrap();
        assert!(timeline.rebase(1).is_err());
        assert_eq!(timeline.event_count(), 1);
        session.close();
        assert_eq!(
            timeline.push(&session, &view(a), MediaTime(1)),
            Err(AudioError::Closed)
        );
        assert_eq!(event_frame(1, 48000).unwrap(), 1);
        assert_eq!(sample_ns(1, 48000).unwrap(), 20_833);
    }

    #[test]
    fn owned_route_snapshot_freezes_settings_gain_and_bindings_without_a_session_borrow() {
        fn assert_send<T: Send + 'static>() {}
        assert_send::<AudioRouteSnapshot>();
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::SetAudioSettings {
                settings: AudioSettings::new(48000, SpeakerLayout::Mono).unwrap(),
            })
            .unwrap();
        let source = add(&mut session, scene, "Owned input");
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.5,
                muted: false,
            })
            .unwrap();
        let old = AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(0)).unwrap();
        let owner = session.identity();
        let revision = session.snapshot().revision();
        assert_eq!(old.session(), owner);
        assert_eq!(old.revision(), revision);
        assert_eq!(
            old.settings(),
            AudioSettings::new(48000, SpeakerLayout::Mono).unwrap()
        );
        assert_eq!(old.at(), MediaTime(0));
        assert_eq!(
            old.required_bindings()
                .map(|(id, binding)| (id, binding.clone()))
                .collect::<Vec<_>>(),
            [(source, binding("Owned input"))]
        );
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.25,
                muted: false,
            })
            .unwrap();
        let updated =
            AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(20_834)).unwrap();
        session.close();
        assert!(matches!(
            AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(0)),
            Err(AudioError::Closed)
        ));
        drop(session);
        let mut timeline = AudioRouteTimeline::new(48000).unwrap();
        timeline.push_snapshot(old).unwrap();
        timeline.push_snapshot(updated).unwrap();
        let spans = timeline.segments(0, 4).unwrap();
        assert_eq!(mix(&spans[0], 0, 48000), [0.5, 0.5]);
        assert_eq!(mix(&spans[1], 0, 48000), [0.25, 0.25]);
    }

    #[test]
    fn snapshot_rejection_and_unchanged_continuations_preserve_timeline_publication_rules() {
        let mut session = Session::new();
        let scene = session.preview();
        let source = add(&mut session, scene, "Ordered input");
        let first = AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(0)).unwrap();
        let stale_revision =
            AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(40_000)).unwrap();
        let mut timeline = AudioRouteTimeline::new(48000).unwrap();
        timeline.push_snapshot(first).unwrap();
        session
            .apply(Command::SetAudio {
                source,
                gain: 0.5,
                muted: false,
            })
            .unwrap();
        timeline
            .push_snapshot(
                AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(20_000)).unwrap(),
            )
            .unwrap();
        session
            .apply(Command::RenameScene {
                scene,
                name: "Metadata rename".into(),
            })
            .unwrap();
        let backwards_time =
            AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(29_999)).unwrap();
        assert!(
            !timeline
                .push_snapshot(
                    AudioRouteSnapshot::capture(&session, &view(scene), MediaTime(30_000)).unwrap()
                )
                .unwrap()
        );
        assert_eq!(timeline.revision, Some(session.snapshot().revision()));
        assert_eq!(timeline.observed_at, Some(30_000));
        let before = (
            timeline.serial,
            timeline.metadata_bytes,
            timeline.events.len(),
            timeline.revision,
            timeline.observed_at,
        );
        assert!(timeline.push_snapshot(stale_revision).is_err());
        assert!(timeline.push_snapshot(backwards_time).is_err());
        let foreign = Session::new();
        assert!(
            timeline
                .push_snapshot(
                    AudioRouteSnapshot::capture(
                        &foreign,
                        &view(foreign.preview()),
                        MediaTime(50_000)
                    )
                    .unwrap()
                )
                .is_err()
        );
        assert_eq!(
            (
                timeline.serial,
                timeline.metadata_bytes,
                timeline.events.len(),
                timeline.revision,
                timeline.observed_at
            ),
            before
        );
    }
}
