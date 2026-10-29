//! Source-scoped OBS-style true-peak estimation for already processed PCM.
//!
//! The caller supplies post-DSP, post-gain interleaved PCM on a contiguous
//! output sample grid. These values are presentation diagnostics only: this
//! owner does not alter audio, provide limiting, or claim sink presentation.
//! A route owner must retire/reconcile identities when sources disappear.

use super::{AudioError, Id, MAX_AUDIO_BLOCK_SOURCES, MAX_AUDIO_CHANNELS, MAX_AUDIO_FRAMES};
use skylive_media::ObsTruePeakMeter;

#[derive(Clone)]
struct Lane {
    source: Id,
    generation: (u64,u64),
    rate: u32,
    channels: usize,
    end_frame: u64,
    meter: ObsTruePeakMeter,
}
/// One retained owner and one transactional clone, including descriptors for
/// the largest admitted Program source union.
pub(crate) const MAX_METER_STATE_BYTES:usize =
    MAX_AUDIO_BLOCK_SOURCES * (2 * std::mem::size_of::<Lane>() + 128)
    + 2 * std::mem::size_of::<MasterLane>();

/// One bounded meter owner per output/mix timeline. Cloning permits a caller
/// to publish a candidate only after the whole mix succeeds.
#[derive(Clone, Default)]
pub(crate) struct SourceTruePeaks { lanes: Vec<Lane> }

impl SourceTruePeaks {
    /// Discard source histories outside the current document/route. This must
    /// not be shared across independently paced recording/streaming readers.
    pub(crate) fn retain(&mut self, sources: &[Id]) {
        self.lanes.retain(|lane| sources.contains(&lane.source));
    }

    pub(crate) fn clear(&mut self) { self.lanes.clear(); }

    /// Observe already mixed source samples once per committed sample range.
    /// A gap, backwards timestamp, changed stream generation or format resets
    /// the interpolation history; missing samples are never interpolated over.
    pub(crate) fn observe(&mut self, source: Id, generation: (u64,u64), rate: u32,
        channels: usize, start_frame: u64, samples: &[f64]) -> Result<[f64; MAX_AUDIO_CHANNELS], AudioError> {
        if !(8000..=192000).contains(&rate)
            || !(1..=MAX_AUDIO_CHANNELS).contains(&channels) || samples.is_empty()
            || samples.len() % channels != 0 || samples.len() / channels > MAX_AUDIO_FRAMES {
            return Err(AudioError::InvalidRequest("true-peak source block scope or shape"));
        }
        if let Some(index) = samples.iter().position(|sample| !sample.is_finite()) {
            return Err(AudioError::NonFiniteSample { source, index });
        }
        let end = start_frame.checked_add((samples.len() / channels) as u64).ok_or(AudioError::TimeOverflow)?;
        let index = self.lanes.iter().position(|lane| lane.source == source);
        // Work on a private candidate: malformed samples/allocation failure do
        // not advance the state of a previously published audio block.
        let mut lane = if let Some(index) = index {
            let old = &self.lanes[index];
            if old.generation == generation && old.rate == rate && old.channels == channels && old.end_frame == start_frame {
                old.clone()
            } else {
                Lane { source, generation, rate, channels, end_frame: start_frame,
                    meter: ObsTruePeakMeter::new(channels).map_err(AudioError::Media)? }
            }
        } else {
            if self.lanes.len() >= MAX_AUDIO_BLOCK_SOURCES {
                return Err(AudioError::Limit("true-peak source identities"));
            }
            self.lanes.try_reserve(1).map_err(|_| AudioError::Limit("true-peak source allocation"))?;
            Lane { source, generation, rate, channels, end_frame: start_frame,
                meter: ObsTruePeakMeter::new(channels).map_err(AudioError::Media)? }
        };
        let peaks = lane.meter.process(samples).map_err(AudioError::Media)?;
        lane.end_frame = end;
        if let Some(index) = index { self.lanes[index] = lane; } else { self.lanes.push(lane); }
        Ok(peaks)
    }
}

/// Independent history for the final Program PCM timeline.
#[derive(Clone, Default)]
pub(crate) struct MasterTruePeak { lane: Option<MasterLane> }

#[derive(Clone)]
struct MasterLane {
    generation: (u64, u64),
    rate: u32,
    channels: usize,
    end_frame: u64,
    meter: ObsTruePeakMeter,
}

impl MasterTruePeak {
    pub(crate) fn clear(&mut self) { self.lane = None; }

    pub(crate) fn observe(&mut self, generation: (u64, u64), rate: u32,
        channels: usize, start_frame: u64, samples: &[f32]) -> Result<[f64; MAX_AUDIO_CHANNELS], AudioError> {
        if !(8000..=192000).contains(&rate) || !(1..=MAX_AUDIO_CHANNELS).contains(&channels)
            || samples.is_empty() || samples.len() % channels != 0
            || samples.len() / channels > MAX_AUDIO_FRAMES || samples.iter().any(|x| !x.is_finite()) {
            return Err(AudioError::InvalidRequest("true-peak Program block scope or shape"));
        }
        let end = start_frame.checked_add((samples.len() / channels) as u64).ok_or(AudioError::TimeOverflow)?;
        let mut lane = match &self.lane {
            Some(old) if old.generation == generation && old.rate == rate
                && old.channels == channels && old.end_frame == start_frame => old.clone(),
            _ => MasterLane { generation, rate, channels, end_frame: start_frame,
                meter: ObsTruePeakMeter::new(channels).map_err(AudioError::Media)? },
        };
        let mut peaks = [0.0f64; MAX_AUDIO_CHANNELS];
        let mut frame = [0.0f64; MAX_AUDIO_CHANNELS];
        for input in samples.chunks_exact(channels) {
            for (destination, value) in frame.iter_mut().zip(input) { *destination = f64::from(*value); }
            let current = lane.meter.process(&frame[..channels]).map_err(AudioError::Media)?;
            for channel in 0..channels { peaks[channel] = peaks[channel].max(current[channel]); }
        }
        lane.end_frame = end;
        self.lane = Some(lane);
        Ok(peaks)
    }
}
