//! Source-owner RNNoise project-grid adapter. Not an in-place source filter.
//! Real delayed output and original immutable receipts leave through pop().
use skylive_core::{AudioSettings, Id};
use skylive_media::{AudioConversionRequirements, AudioConvertLimits, AudioConverter, AudioFormat,
    MediaError, audio_conversion_requirements, rnnoise};
use std::{error::Error as StdError, fmt, mem::size_of};

const MAX_PACKET: usize = 1024;
const RAW_CAPACITY: usize = 1504;
const OUTPUT_CAPACITY: usize = 2560;
// Minimum1-frame packets may occupy every declared pending source frame.
const MAX_PROJECT_PENDING: usize = 48000 / 4;


/// Exact existing owner identities, not native-generation substitutes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scope {
    pub session: u64,
    pub source: Id,
    pub binding_scope: u64,
    pub source_epoch: u64,
    pub native_epoch: u64,
    pub generation: u64,
    pub processing_epoch: u64,
    pub original: AudioFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputStamp {
    pub scope: Scope,
    pub start: i128,
    pub config: u64,
    pub revision: u64,
    pub packet: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputStamp {
    pub input: InputStamp,
    pub frames: usize,
    /// Original project-frame position minus exactly441/480 frames, once.
    pub start: i128,
}

#[derive(Clone, Copy, Debug)]
pub struct Requirements {
    pub retained_bytes: usize,
    pub scratch_bytes: usize,
    pub neural_work_per_grain: usize,
    pub converter_work_per_pull: usize,
    pub service_work_bound: usize,
    pub pending_project_frames: usize,
    pub metadata_capacity: usize,
    pub processing_lookahead_frames: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub retained_bytes: usize,
    pub scratch_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Format, Scope, Shape, Clock, Input, Limit, Allocation, Backpressure, Ended, Poisoned,
    Media(MediaError), Frame(rnnoise::Error),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Media(error) => write!(f, "RNNoise conversion: {error}"),
            Self::Frame(error) => write!(f, "RNNoise inference: {error}"),
            other => write!(f, "RNNoise source adapter: {other:?}"),
        }
    }
}
impl StdError for Error {}

#[derive(Clone, Copy)]
struct Info { stamp: InputStamp, frames: usize }

struct Ring {
    values: Vec<f32>, channels: usize, capacity: usize, head: usize, frames: usize,
}
impl Ring {
    fn new(capacity: usize, channels: usize) -> Result<Self, Error> {
        Ok(Self { values: zeros(capacity.checked_mul(channels).ok_or(Error::Limit)?)?,
            channels, capacity, head: 0, frames: 0 })
    }
    fn value(&self, frame: usize, channel: usize) -> f32 {
        self.values[((self.head + frame) % self.capacity) * self.channels + channel]
    }
    fn append_value(&mut self, frame: usize, channel: usize, value: f32) {
        let index = ((self.head + self.frames + frame) % self.capacity) * self.channels + channel;
        self.values[index] = value;
    }
    fn append(&mut self, values: &[f32]) {
        for (frame, channels) in values.chunks_exact(self.channels).enumerate() {
            for (channel, &value) in channels.iter().enumerate() { self.append_value(frame, channel, value); }
        }
        self.frames += values.len() / self.channels;
    }
    fn consume(&mut self, frames: usize) { self.head = (self.head + frames) % self.capacity; self.frames -= frames; }
    fn clear(&mut self) { self.head = 0; self.frames = 0; }
}

struct Conversion {
    to_model: AudioConverter,
    from_model: AudioConverter,
    model_sealed: bool,
    project_sealed: bool,
    input_drained: bool,
    output_drained: bool,
}

/// Single existing audio worker owns all state. No Clone/Arc/Mutex snapshot can
/// advance an accepted resampler through a rejected transactional candidate.
pub struct ProjectRnnoise {
    settings: AudioSettings,
    scope: Scope,
    needed: Requirements,
    processors: Vec<rnnoise::Denoiser>,
    conversion: Option<Conversion>,
    raw: Ring,
    output: Ring,
    converter_scratch: Vec<f32>,
    neural_scratch: Vec<f32>,
    info: Vec<Option<Info>>,
    info_head: usize,
    info_count: usize,
    pending_frames: usize,
    next_input: Option<i128>,
    ended: bool,
    poisoned: bool,
    discarded_raw: usize,
    last_service: ServiceCost,
}

fn conversion_limits() -> AudioConvertLimits {
    AudioConvertLimits { max_input_frames: MAX_PACKET, max_output_frames: MAX_PACKET,
        max_buffered_input_frames: 4096, max_buffered_output_frames: 2048, max_bytes: usize::MAX }
}

fn conversion_plans(settings: AudioSettings) -> Result<Option<(AudioConversionRequirements, AudioConversionRequirements)>, Error> {
    if !matches!(settings.sample_rate(), 44100 | 48000) { return Err(Error::Format); }
    if settings.sample_rate() == 48000 { return Ok(None); }
    let project = AudioFormat::new(settings.sample_rate(), settings.channels(), settings.channel_mask()).map_err(Error::Media)?;
    let model = AudioFormat::new(48000, settings.channels(), settings.channel_mask()).map_err(Error::Media)?;
    Ok(Some((audio_conversion_requirements(project, model, conversion_limits()).map_err(Error::Media)?,
        audio_conversion_requirements(model, project, conversion_limits()).map_err(Error::Media)?)))
}

pub fn requirements(settings: AudioSettings) -> Result<Requirements, Error> {
    let plans = conversion_plans(settings)?;
    let channels = usize::from(settings.channels());
    let converter_bytes = plans.map_or(0, |(a, b)| a.accounted_bytes + b.accounted_bytes);
    let samples = (RAW_CAPACITY + OUTPUT_CAPACITY + MAX_PACKET + rnnoise::FRAME_SIZE)
        .checked_mul(channels).and_then(|v| v.checked_mul(size_of::<f32>())).ok_or(Error::Limit)?;
    let retained_bytes = rnnoise::requirements().retained_bytes.checked_mul(channels)
        .and_then(|v| v.checked_add(converter_bytes)).and_then(|v| v.checked_add(samples))
        .and_then(|v| v.checked_add((settings.sample_rate() as usize / 4 + 1) * size_of::<Option<Info>>() + size_of::<ProjectRnnoise>() + 512))
        .ok_or(Error::Limit)?;
    let neural_work_per_grain = rnnoise::requirements().work_units * channels;
    // At most two fixed1024 output blocks cover a pull plus trimmed leading
    // delay; charge every four-bank cubic sinc and129-tap phase visit conservatively. This is
    // work admission, not a measured runtime/performance estimate.
    let converter_work_per_pull = if plans.is_some() { 2 * 1024 * (4 * 256 + 129 + 128) * channels } else { 0 };
    Ok(Requirements { retained_bytes, scratch_bytes: 144 * 1024,
        neural_work_per_grain, converter_work_per_pull,
        service_work_bound: neural_work_per_grain + 2 * converter_work_per_pull,
        // Fixed-output1024 converters + finite sinc history, one480 neural
        // segment and original packet framing fit this explicit250ms profile.
        // The source history owner must include this in its2s emission ceiling.
        pending_project_frames: settings.sample_rate() as usize / 4,
        metadata_capacity: settings.sample_rate() as usize / 4 + 1,
        // Original packets are <=1024 frames. Identity needs two packet
        // boundaries plus480 overlap. For44.1k, two back-converter chunks
        // (leading delay<1024) need <=four to-model chunks, plus two original
        // packet boundaries. The pinned constructor's max-input is the bound.
        processing_lookahead_frames: plans.map_or((2*MAX_PACKET+rnnoise::FRAME_SIZE)as u64,
            |(to,_)|(4*to.max_processor_input_frames+2*MAX_PACKET)as u64) })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    Progress,
    NeedInput,
    OutputReady,
    WorkLimit,
    Drained,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Discarded {
    pub model_input_frames: usize,
    pub processed_project_frames: usize,
    pub original_packet_frames: usize,
    pub original_packets: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServiceCost {
    pub neural_work_units: usize,
    pub converter_pulls: usize,
    pub work_units: usize,
}

impl ProjectRnnoise {
    pub fn new(settings: AudioSettings, scope: Scope, limits: Limits) -> Result<Self, Error> {
        let needed = requirements(settings)?;
        if needed.pending_project_frames > MAX_PROJECT_PENDING { return Err(Error::Limit); }
        if needed.retained_bytes > limits.retained_bytes || needed.scratch_bytes > limits.scratch_bytes { return Err(Error::Limit); }
        let channels = usize::from(settings.channels());
        let mut processors = Vec::new();
        processors.try_reserve_exact(channels).map_err(|_| Error::Allocation)?;
        for _ in 0..channels { processors.push(rnnoise::Denoiser::new(frame_limits()).map_err(Error::Frame)?); }
        let conversion = if let Some((to, from)) = conversion_plans(settings)? {
            let project = AudioFormat::new(settings.sample_rate(), settings.channels(), settings.channel_mask()).map_err(Error::Media)?;
            let model = AudioFormat::new(48000, settings.channels(), settings.channel_mask()).map_err(Error::Media)?;
            let to_model = AudioConverter::new(project, model, AudioConvertLimits { max_bytes: to.accounted_bytes, ..conversion_limits() }).map_err(Error::Media)?;
            let from_model = AudioConverter::new(model, project, AudioConvertLimits { max_bytes: from.accounted_bytes, ..conversion_limits() }).map_err(Error::Media)?;
            if to_model.delay_output_frames()>=MAX_PACKET||from_model.delay_output_frames()>=MAX_PACKET{return Err(Error::Limit);}
            Some(Conversion { to_model, from_model, model_sealed: false, project_sealed: false, input_drained: false, output_drained: false })
        } else { None };
        let mut info = Vec::new();
        info.try_reserve_exact(needed.metadata_capacity).map_err(|_| Error::Allocation)?;
        info.resize(needed.metadata_capacity, None);
        Ok(Self { settings, scope, needed, processors, conversion, raw: Ring::new(RAW_CAPACITY, channels)?,
            output: Ring::new(OUTPUT_CAPACITY, channels)?, converter_scratch: zeros(MAX_PACKET * channels)?,
            neural_scratch: zeros(rnnoise::FRAME_SIZE * channels)?, info, info_head: 0, info_count: 0,
            pending_frames: 0, next_input: None, ended: false, poisoned: false, discarded_raw: 0,
            last_service: ServiceCost::default() })
    }

    pub fn ready_frames(&self) -> Option<usize> {
        if self.poisoned || self.info_count == 0 { return None; }
        self.info[self.info_head].filter(|info| self.output.frames >= info.frames).map(|info| info.frames)
    }

    pub fn admit_packet(&self, stamp: InputStamp, frames: usize) -> Result<(), Error> {
        if self.poisoned { return Err(Error::Poisoned); }
        if self.ended { return Err(Error::Ended); }
        if stamp.scope != self.scope { return Err(Error::Scope); }
        if frames == 0 || frames > MAX_PACKET { return Err(Error::Shape); }
        stamp.start.checked_add(frames as i128).ok_or(Error::Clock)?;
        stamp.start.checked_sub(i128::from(self.settings.sample_rate() / 100)).ok_or(Error::Clock)?;
        if self.next_input.is_some_and(|next| next != stamp.start) { return Err(Error::Clock); }
        let pending = self.pending_frames.checked_add(frames).ok_or(Error::Limit)?;
        if self.ready_frames().is_some() || self.info_count == self.info.len() || pending > self.needed.pending_project_frames { return Err(Error::Backpressure); }
        if let Some(conversion) = &self.conversion {
            if conversion.to_model.input_capacity_frames() < frames { return Err(Error::Backpressure); }
        } else if self.raw.frames + frames > self.raw.capacity { return Err(Error::Backpressure); }
        Ok(())
    }
    pub fn peek(&self) -> Option<OutputStamp> {
        let frames = self.ready_frames()?;
        let info = self.info[self.info_head]?;
        Some(OutputStamp { input: info.stamp, frames,
            start: info.stamp.start - i128::from(self.settings.sample_rate() / 100) })
    }
    pub fn pending_packets(&self) -> impl Iterator<Item=u64> + '_ {
        (0..self.info_count).filter_map(|offset| self.info[(self.info_head + offset) % self.info.len()].map(|info|info.stamp.packet))
    }
    pub fn push(&mut self, stamp: InputStamp, samples: &[f32]) -> Result<(), Error> {
        if self.poisoned { return Err(Error::Poisoned); }
        if self.ended { return Err(Error::Ended); }
        if stamp.scope != self.scope { return Err(Error::Scope); }
        let channels = usize::from(self.settings.channels());
        if samples.is_empty() || samples.len() % channels != 0 || samples.len() / channels > MAX_PACKET { return Err(Error::Shape); }
        let frames = samples.len() / channels;
        self.admit_packet(stamp, frames)?;
        let end = stamp.start.checked_add(frames as i128).ok_or(Error::Clock)?;
        let pending = self.pending_frames.checked_add(frames).ok_or(Error::Limit)?;
        if samples.iter().any(|v| !v.is_finite() || v.abs() > rnnoise::MAX_INPUT_MAGNITUDE) { return Err(Error::Input); }
        if let Some(conversion) = &mut self.conversion {
            if conversion.to_model.input_capacity_frames() < frames { return Err(Error::Backpressure); }
            if let Err(error) = conversion.to_model.push(samples) { self.poisoned = true; return Err(Error::Media(error)); }
        } else {
            if self.raw.frames + frames > self.raw.capacity { return Err(Error::Backpressure); }
            self.raw.append(samples);
        }
        let info_index=(self.info_head+self.info_count)%self.info.len();
        self.info[info_index] = Some(Info { stamp, frames });
        self.info_count += 1; self.pending_frames = pending; self.next_input = Some(end);
        Ok(())
    }

    /// At most two converter pulls, or one pull plus one480/channel inference
    /// grain. Reserve service_work_bound before mutation, then charge the actual
    /// conservative service_cost().work_units against the owner's common cap.
    pub fn service(&mut self, work_credit: usize) -> Result<Service, Error> {
        self.last_service = ServiceCost::default();
        if self.poisoned { return Err(Error::Poisoned); }
        if self.ready_frames().is_some() { return Ok(Service::OutputReady); }
        if work_credit < self.needed.service_work_bound { return Ok(Service::WorkLimit); }
        let channels = usize::from(self.settings.channels());
        if let Some(conversion) = &mut self.conversion {
            if self.output.capacity - self.output.frames < MAX_PACKET { return Err(Error::Backpressure); }
            self.last_service.converter_pulls += 1;
            let before=conversion.from_model.stats().processed_blocks;
            let result=conversion.from_model.pull(&mut self.converter_scratch);
            let blocks=conversion.from_model.stats().processed_blocks.checked_sub(before).ok_or(Error::Limit)?;
            if blocks>2{self.poisoned=true;return Err(Error::Limit);}
            self.last_service.work_units+=blocks as usize*(self.needed.converter_work_per_pull/2);
            let frames = match result {
                Ok(frames) => frames,
                Err(error) => { self.poisoned = true; return Err(Error::Media(error)); }
            };
            if frames != 0 {
                self.output.append(&self.converter_scratch[..frames * channels]);
                return Ok(if self.ready_frames().is_some() { Service::OutputReady } else { Service::Progress });
            }
            conversion.output_drained = conversion.project_sealed && conversion.from_model.stats().drained;
        }
        if self.raw.frames >= rnnoise::FRAME_SIZE {
            if self.conversion.as_ref().is_some_and(|c| c.from_model.input_capacity_frames() < rnnoise::FRAME_SIZE) {
                return Err(Error::Backpressure);
            }
            if self.conversion.is_none() && self.output.frames + rnnoise::FRAME_SIZE > self.output.capacity { return Err(Error::Backpressure); }
            let mut input = [0.; rnnoise::FRAME_SIZE]; let mut output = [0.; rnnoise::FRAME_SIZE];
            self.last_service.neural_work_units = self.needed.neural_work_per_grain;
            self.last_service.work_units += self.needed.neural_work_per_grain;
            for channel in 0..channels {
                for (frame, value) in input.iter_mut().enumerate() { *value = self.raw.value(frame, channel); }
                if let Err(error) = self.processors[channel].process(&input, &mut output, frame_limits()) {
                    self.poisoned = true; return Err(Error::Frame(error));
                }
                for (frame, &value) in output.iter().enumerate() { self.neural_scratch[frame * channels + channel] = value; }
            }
            if let Some(conversion) = &mut self.conversion {
                if let Err(error) = conversion.from_model.push(&self.neural_scratch) { self.poisoned = true; return Err(Error::Media(error)); }
            } else { self.output.append(&self.neural_scratch); }
            self.raw.consume(rnnoise::FRAME_SIZE);
            return Ok(if self.ready_frames().is_some() { Service::OutputReady } else { Service::Progress });
        }
        if let Some(conversion) = &mut self.conversion {
            if !conversion.input_drained {
                if self.raw.capacity - self.raw.frames < MAX_PACKET { return Err(Error::Backpressure); }
                self.last_service.converter_pulls += 1;
                let before=conversion.to_model.stats().processed_blocks;
                let result=conversion.to_model.pull(&mut self.converter_scratch);
                let blocks=conversion.to_model.stats().processed_blocks.checked_sub(before).ok_or(Error::Limit)?;
                if blocks>2{self.poisoned=true;return Err(Error::Limit);}
                self.last_service.work_units+=blocks as usize*(self.needed.converter_work_per_pull/2);
                let frames = match result {
                    Ok(frames) => frames,
                    Err(error) => { self.poisoned = true; return Err(Error::Media(error)); }
                };
                if frames != 0 { self.raw.append(&self.converter_scratch[..frames * channels]); return Ok(Service::Progress); }
                conversion.input_drained = conversion.model_sealed && conversion.to_model.stats().drained;
            }
            if conversion.input_drained && !conversion.project_sealed {
                self.discarded_raw += self.raw.frames; self.raw.clear();
                if let Err(error) = conversion.from_model.finish() {
                    self.poisoned = true; return Err(Error::Media(error));
                }
                conversion.project_sealed = true; return Ok(Service::Progress);
            }
            if conversion.output_drained { return Ok(Service::Drained); }
        } else if self.ended {
            self.discarded_raw += self.raw.frames; self.raw.clear(); return Ok(Service::Drained);
        }
        Ok(Service::NeedInput)
    }

    pub fn pop(&mut self, samples: &mut [f32]) -> Result<Option<OutputStamp>, Error> {
        if self.poisoned { return Err(Error::Poisoned); }
        let Some(frames) = self.ready_frames() else { return Ok(None); };
        let channels = usize::from(self.settings.channels());
        if samples.len() != frames * channels { return Err(Error::Shape); }
        let info = self.info[self.info_head].ok_or(Error::Scope)?;
        let start = info.stamp.start - i128::from(self.settings.sample_rate() / 100);
        for frame in 0..frames { for channel in 0..channels { samples[frame * channels + channel] = self.output.value(frame, channel); } }
        self.output.consume(frames); self.info[self.info_head] = None;
        self.info_head = (self.info_head + 1) % self.info.len(); self.info_count -= 1; self.pending_frames -= frames;
        Ok(Some(OutputStamp { input: info.stamp, frames, start }))
    }

    /// Actual source EOF only; pending output is serviced and popped normally.
    /// Sinc padding is private valid-sample interpolation, not neural input padding.
    pub fn end_input(&mut self) -> Result<(), Error> {
        if self.poisoned { return Err(Error::Poisoned); }
        if self.ended { return Err(Error::Ended); }
        if let Some(conversion) = &mut self.conversion {
            if let Err(error) = conversion.to_model.finish() {
                self.poisoned = true; return Err(Error::Media(error));
            }
            conversion.model_sealed = true;
        }
        self.ended = true; Ok(())
    }

    pub fn finish_discard(&mut self) -> Result<Discarded, Error> {
        if self.poisoned { return Err(Error::Poisoned); }
        if !self.ended || self.ready_frames().is_some() || self.raw.frames != 0
            || self.conversion.as_ref().is_some_and(|c| !c.output_drained) { return Err(Error::Backpressure); }
        let discarded = Discarded { model_input_frames: self.discarded_raw,
            processed_project_frames: self.output.frames, original_packet_frames: self.pending_frames,
            original_packets: self.info_count };
        self.output.clear(); self.info.fill(None); self.info_head = 0; self.info_count = 0; self.pending_frames = 0;
        Ok(discarded)
    }

    pub fn pending_configs(&self) -> impl Iterator<Item = u64> + '_ {
        (0..self.info_count).filter_map(|offset| self.info[(self.info_head + offset) % self.info.len()].map(|info| info.stamp.config))
    }
    pub fn service_cost(&self) -> ServiceCost { self.last_service }
}

fn frame_limits() -> rnnoise::Limits {
    let required = rnnoise::requirements();
    rnnoise::Limits { max_retained_bytes: required.retained_bytes,
        max_scratch_bytes: required.scratch_bytes, max_work_units: required.work_units }
}
fn zeros(length: usize) -> Result<Vec<f32>, Error> {
    let mut result = Vec::new(); result.try_reserve_exact(length).map_err(|_| Error::Allocation)?;
    result.resize(length, 0.); Ok(result)
}
