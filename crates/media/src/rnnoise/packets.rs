//! Bounded native-48-kHz OBS noise-filter packet adapter.
//!
//! A push may produce no packet. A pop returns an original packet's exact
//! frame count, original opaque provenance token and timestamp minus 10 ms.
//! Callers drain ready packets before admitting more input. This replaces
//! OBS's unbounded deques with an explicit nonblocking admission contract.
//! No application/source clocks or metadata are fabricated here.

use super::{ALGORITHM_DELAY_NS, Denoiser, Error as FrameError, FRAME_SIZE, Limits as FrameLimits,
    MAX_INPUT_MAGNITUDE, requirements as frame_requirements};
use std::{error::Error as StdError, fmt, mem::size_of};

pub const MAX_CHANNELS: usize = 8;
pub const MAX_PACKET_FRAMES: usize = 1024;
// With all ready packets drained: output < oldest packet (<=1024), and
// unprocessed input <480. The next <=1024-frame input needs <=1503 raw
// and <=2463 output frames. Fixed capacities round these upward explicitly.
const INPUT_CAPACITY: usize = 1504;
const OUTPUT_CAPACITY: usize = 2560;
const INFO_CAPACITY: usize = 1504;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputStamp {
    /// Signed project-independent timestamp; negative output is representable.
    pub timestamp_ns: i128,
    /// Opaque owner provenance. It is retained, never replaced with the latest revision.
    pub token: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputStamp {
    pub input: InputStamp,
    pub timestamp_ns: i128,
    pub frames: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirements {
    pub retained_bytes: usize,
    pub scratch_bytes: usize,
    /// Maximum work for one <=1024-frame push at this channel count.
    pub work_units: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_retained_bytes: usize,
    pub max_scratch_bytes: usize,
    pub max_work_units: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_retained_bytes: 1024 * 1024, max_scratch_bytes: 144 * 1024,
            max_work_units: 24 * 1_000_000 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Format,
    Shape,
    Timestamp,
    Input,
    Limit,
    Allocation,
    /// Drain pop() before retrying; no input was accepted or state mutated.
    OutputPending,
    Finished,
    Poisoned,
    Frame(FrameError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(f, "RNNoise frame: {error}"),
            _ => f.write_str(match self {
                Self::Format => "RNNoise packet adapter supports 48 kHz and 1..8 channels",
                Self::Shape => "RNNoise packet must contain 1..1024 complete interleaved frames",
                Self::Timestamp => "RNNoise shifted packet timestamp is not representable",
                Self::Input => "RNNoise packet has a nonfinite or out-of-range input value",
                Self::Limit => "RNNoise packet state, scratch, queue or work limit exceeded",
                Self::Allocation => "RNNoise packet allocation failed",
                Self::OutputPending => "Drain ready RNNoise output before admitting more input",
                Self::Finished => "RNNoise packet input has ended",
                Self::Poisoned => "RNNoise packet adapter requires reset after a processing failure",
                Self::Frame(_) => "RNNoise frame failed",
            }),
        }
    }
}
impl StdError for Error {}

#[derive(Clone, Copy, Default)]
struct Info { stamp: InputStamp, frames: usize }

struct Ring {
    samples: Vec<f32>,
    capacity: usize,
    channels: usize,
    head: usize,
    frames: usize,
}

impl Ring {
    fn new(capacity: usize, channels: usize) -> Result<Self, Error> {
        let count = capacity.checked_mul(channels).ok_or(Error::Limit)?;
        let mut samples = Vec::new();
        samples.try_reserve_exact(count).map_err(|_| Error::Allocation)?;
        samples.resize(count, 0.);
        Ok(Self { samples, capacity, channels, head: 0, frames: 0 })
    }
    fn at(&self, frame: usize, channel: usize) -> f32 {
        self.samples[((self.head + frame) % self.capacity) * self.channels + channel]
    }
    fn append_value(&mut self, frame: usize, channel: usize, value: f32) {
        let index = ((self.head + self.frames + frame) % self.capacity) * self.channels + channel;
        self.samples[index] = value;
    }
    fn consume(&mut self, count: usize) {
        self.head = (self.head + count) % self.capacity;
        self.frames -= count;
    }
    fn clear(&mut self) { self.head = 0; self.frames = 0; }
    fn try_clone(&self) -> Result<Self, Error> {
        let mut copy = Self::new(self.capacity, self.channels)?;
        copy.samples.copy_from_slice(&self.samples);
        copy.head = self.head; copy.frames = self.frames;
        Ok(copy)
    }
}

/// Each channel has genuine independent recurrent/analysis/pitch state. The
/// caller owns source/session/binding/native/processing epochs and must reset or
/// retire this adapter on those discontinuities. No device or thread is owned.
pub struct PacketFramer {
    channels: usize,
    processors: Vec<Denoiser>,
    input: Ring,
    output: Ring,
    info: Vec<Info>,
    info_head: usize,
    info_count: usize,
    finished: bool,
    poisoned: bool,
}

pub fn requirements(sample_rate: u32, channels: usize) -> Result<Requirements, Error> {
    if sample_rate != super::SAMPLE_RATE || !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(Error::Format);
    }
    let primitive = frame_requirements();
    let audio = (INPUT_CAPACITY + OUTPUT_CAPACITY).checked_mul(channels)
        .and_then(|n| n.checked_mul(size_of::<f32>())).ok_or(Error::Limit)?;
    let retained_bytes = primitive.retained_bytes.checked_mul(channels)
        .and_then(|n| n.checked_add(audio))
        .and_then(|n| n.checked_add(INFO_CAPACITY * size_of::<Info>()))
        .and_then(|n| n.checked_add(size_of::<PacketFramer>() + 256)).ok_or(Error::Limit)?;
    let work_units = primitive.work_units.checked_mul(channels).and_then(|n| n.checked_mul(3))
        .ok_or(Error::Limit)?;
    Ok(Requirements { retained_bytes, scratch_bytes: 144 * 1024, work_units })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PushResult {
    pub processed_segments: usize,
    pub ready_packet: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinishResult {
    /// Actual unprocessed PCM shorter than one 480-frame segment.
    pub discarded_input_frames: usize,
    /// Processed output that cannot complete its original packet at true EOF.
    pub discarded_output_frames: usize,
    pub discarded_packets: usize,
}

impl PacketFramer {
    pub fn new(sample_rate: u32, channels: usize, limits: Limits) -> Result<Self, Error> {
        admit(channels, sample_rate, limits)?;
        let mut processors = Vec::new();
        processors.try_reserve_exact(channels).map_err(|_| Error::Allocation)?;
        let primitive_limits = primitive_limits();
        for _ in 0..channels {
            processors.push(Denoiser::new(primitive_limits).map_err(Error::Frame)?);
        }
        let input = Ring::new(INPUT_CAPACITY, channels)?;
        let output = Ring::new(OUTPUT_CAPACITY, channels)?;
        let mut info = Vec::new();
        info.try_reserve_exact(INFO_CAPACITY).map_err(|_| Error::Allocation)?;
        info.resize(INFO_CAPACITY, Info::default());
        Ok(Self { channels, processors, input, output, info, info_head: 0,
            info_count: 0, finished: false, poisoned: false })
    }

    /// A complete admitted snapshot. The owner must reserve old+new peak bytes
    /// in its aggregate before calling; no hidden infallible Clone exists.
    pub fn try_clone(&self, limits: Limits) -> Result<Self, Error> {
        admit(self.channels, super::SAMPLE_RATE, limits)?;
        let mut processors = Vec::new();
        processors.try_reserve_exact(self.channels).map_err(|_| Error::Allocation)?;
        for processor in &self.processors {
            processors.push(processor.try_clone(primitive_limits()).map_err(Error::Frame)?);
        }
        let mut info = Vec::new();
        info.try_reserve_exact(INFO_CAPACITY).map_err(|_| Error::Allocation)?;
        info.extend_from_slice(&self.info);
        Ok(Self { channels: self.channels, processors, input: self.input.try_clone()?,
            output: self.output.try_clone()?, info, info_head: self.info_head, info_count: self.info_count,
            finished: self.finished, poisoned: self.poisoned })
    }

    pub fn channels(&self) -> usize { self.channels }

    /// Size of the next real original packet, when ready. No fake empty PCM.
    pub fn ready_frames(&self) -> Option<usize> {
        if self.poisoned || self.info_count == 0 { return None; }
        let frames = self.info[self.info_head].frames;
        if self.output.frames >= frames { Some(frames) } else { None }
    }

    pub fn push(&mut self, stamp: InputStamp, samples: &[f32], limits: Limits) -> Result<PushResult, Error> {
        admit(self.channels, super::SAMPLE_RATE, limits)?;
        if self.poisoned { return Err(Error::Poisoned); }
        if self.finished { return Err(Error::Finished); }
        if samples.is_empty() || samples.len() % self.channels != 0 { return Err(Error::Shape); }
        let frames = samples.len() / self.channels;
        if frames > MAX_PACKET_FRAMES { return Err(Error::Shape); }
        stamp.timestamp_ns.checked_sub(i128::from(ALGORITHM_DELAY_NS)).ok_or(Error::Timestamp)?;
        if samples.iter().any(|v| !v.is_finite() || v.abs() > MAX_INPUT_MAGNITUDE) { return Err(Error::Input); }
        if self.ready_frames().is_some() { return Err(Error::OutputPending); }
        let accepted = self.input.frames.checked_add(frames).ok_or(Error::Limit)?;
        let segments = accepted / FRAME_SIZE;
        let produced = segments.checked_mul(FRAME_SIZE).ok_or(Error::Limit)?;
        let work = segments.checked_mul(self.channels)
            .and_then(|n| n.checked_mul(frame_requirements().work_units)).ok_or(Error::Limit)?;
        if work > limits.max_work_units || accepted > self.input.capacity
            || self.output.frames.checked_add(produced).ok_or(Error::Limit)? > self.output.capacity
            || self.info_count >= INFO_CAPACITY { return Err(Error::Limit); }
        // All admission above precedes every queue write and recurrent mutation.
        for frame in 0..frames {
            for channel in 0..self.channels {
                self.input.append_value(frame, channel, samples[frame * self.channels + channel]);
            }
        }
        self.input.frames = accepted;
        let index = (self.info_head + self.info_count) % INFO_CAPACITY;
        self.info[index] = Info { stamp, frames };
        self.info_count += 1;
        let mut input = [0.; FRAME_SIZE];
        let mut output = [0.; FRAME_SIZE];
        for _ in 0..segments {
            for channel in 0..self.channels {
                for (frame, value) in input.iter_mut().enumerate() { *value = self.input.at(frame, channel); }
                if let Err(error) = self.processors[channel].process(&input, &mut output, primitive_limits()) {
                    self.poisoned = true;
                    return Err(Error::Frame(error));
                }
                for (frame, &value) in output.iter().enumerate() { self.output.append_value(frame, channel, value); }
            }
            self.input.consume(FRAME_SIZE);
            self.output.frames += FRAME_SIZE;
        }
        Ok(PushResult { processed_segments: segments, ready_packet: self.ready_frames().is_some() })
    }

    /// Copy one real packet into caller-owned exact-size storage. Calls do not
    /// allocate or run DSP. Its original token stays attached to its own PCM.
    pub fn pop(&mut self, samples: &mut [f32]) -> Result<Option<OutputStamp>, Error> {
        if self.poisoned { return Err(Error::Poisoned); }
        let Some(frames) = self.ready_frames() else { return Ok(None); };
        if samples.len() != frames * self.channels { return Err(Error::Shape); }
        let info = self.info[self.info_head];
        let timestamp_ns = info.stamp.timestamp_ns.checked_sub(i128::from(ALGORITHM_DELAY_NS))
            .ok_or(Error::Timestamp)?;
        for frame in 0..frames {
            for channel in 0..self.channels { samples[frame * self.channels + channel] = self.output.at(frame, channel); }
        }
        self.output.consume(frames);
        self.info_head = (self.info_head + 1) % INFO_CAPACITY;
        self.info_count -= 1;
        Ok(Some(OutputStamp { input: info.stamp, timestamp_ns, frames }))
    }

    /// True EOF only, after draining ready packets. The original filter has no
    /// zero-padding flush: a partial 10 ms input and an unreturnable packet
    /// prefix are explicitly discarded. No final 480-frame synthetic packet.
    pub fn finish(&mut self) -> Result<FinishResult, Error> {
        if self.poisoned { return Err(Error::Poisoned); }
        if self.finished { return Err(Error::Finished); }
        if self.ready_frames().is_some() { return Err(Error::OutputPending); }
        let result = FinishResult { discarded_input_frames: self.input.frames,
            discarded_output_frames: self.output.frames, discarded_packets: self.info_count };
        self.finished = true;
        self.input.clear(); self.output.clear(); self.info_count = 0; self.info_head = 0;
        Ok(result)
    }

    pub fn reset(&mut self) -> Result<(), Error> {
        for processor in &mut self.processors {
            if let Err(error) = processor.reset() { self.poisoned = true; return Err(Error::Frame(error)); }
        }
        self.input.clear(); self.output.clear(); self.info_count = 0; self.info_head = 0;
        self.finished = false; self.poisoned = false;
        Ok(())
    }
}

fn primitive_limits() -> FrameLimits {
    let needed = frame_requirements();
    FrameLimits { max_retained_bytes: needed.retained_bytes,
        max_scratch_bytes: needed.scratch_bytes, max_work_units: needed.work_units }
}

fn admit(channels: usize, sample_rate: u32, limits: Limits) -> Result<(), Error> {
    let needed = requirements(sample_rate, channels)?;
    if needed.retained_bytes > limits.max_retained_bytes || needed.scratch_bytes > limits.max_scratch_bytes
        || needed.work_units > limits.max_work_units { return Err(Error::Limit); }
    Ok(())
}
