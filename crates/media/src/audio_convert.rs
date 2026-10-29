//! Worker-owned bounded PCM rematrixing and bandlimited rate conversion.
use crate::MediaError;
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, WindowFunction,
    calculate_cutoff,
};

mod matrix;
mod phase;
#[cfg(test)]
mod tests;
const SINC_LEN: usize = 256;
const OVERSAMPLING: usize = 128;
const CHUNK: usize = 1024;
const MAX_RATIO_ADJUSTMENT: f64 = 1.01;
const RATIO_SCALE: u64 = 1u64 << 53;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    sample_rate: u32,
    channels: u16,
    channel_mask: u32,
}
impl AudioFormat {
    pub fn new(sample_rate: u32, channels: u16, channel_mask: u32) -> Result<Self, MediaError> {
        Self::validated(sample_rate,channels,channel_mask,8000,192000)
    }
    /// An effective rate for already-decoded local media after applying the
    /// OBS playback-speed multiplier. Native device formats remain restricted
    /// to the normal range via `new`.
    pub fn new_media_effective(sample_rate:u32,channels:u16,channel_mask:u32)->Result<Self,MediaError>{
        Self::validated(sample_rate,channels,channel_mask,80,384000)
    }
    fn validated(sample_rate:u32,channels:u16,channel_mask:u32,min:u32,max:u32)->Result<Self,MediaError>{
        if !(min..=max).contains(&sample_rate)
            || !(1..=8).contains(&channels)
            || (channel_mask != 0
                && (channel_mask & !0x3ffff != 0
                    || channel_mask.count_ones() != u32::from(channels)))
        {
            return Err(MediaError::InvalidAudio);
        }
        Ok(Self {
            sample_rate,
            channels,
            channel_mask,
        })
    }
    pub fn sample_rate(self) -> u32 {
        self.sample_rate
    }
    pub fn channels(self) -> u16 {
        self.channels
    }
    pub fn channel_mask(self) -> u32 {
        self.channel_mask
    }
}
#[derive(Clone, Copy, Debug)]
pub struct AudioConvertLimits {
    pub max_input_frames: usize,
    pub max_output_frames: usize,
    pub max_buffered_input_frames: usize,
    pub max_buffered_output_frames: usize,
    pub max_bytes: usize,
}
impl Default for AudioConvertLimits {
    fn default() -> Self {
        Self {
            max_input_frames: 4096,
            max_output_frames: 1024,
            max_buffered_input_frames: 12288,
            max_buffered_output_frames: 2048,
            max_bytes: 16 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioConversionStats {
    /// Actual attempted fixed-output sinc blocks; identity conversion stays0.
    pub processed_blocks: u64,
    pub accepted_input_frames: u64,
    pub consumed_input_frames: u64,
    pub produced_output_frames: u64,
    pub delivered_output_frames: u64,
    pub padded_input_frames: u64,
    pub delay_output_frames: usize,
    pub queued_input_frames: usize,
    pub queued_output_frames: usize,
    pub accounted_bytes: usize,
    pub finished: bool,
    pub drained: bool,
}
struct Fifo {
    data: Box<[f64]>,
    read: usize,
    frames: usize,
    channels: usize,
}
impl Fifo {
    fn new(frames: usize, channels: usize) -> Result<Self, MediaError> {
        Ok(Self {
            data: zeros(frames.checked_mul(channels).ok_or(MediaError::Overflow)?)?,
            read: 0,
            frames: 0,
            channels,
        })
    }
    fn capacity(&self) -> usize {
        self.data.len() / self.channels
    }
    fn free(&self) -> usize {
        self.capacity() - self.frames
    }
    fn value(&self, frame: usize, channel: usize) -> f64 {
        self.data[((self.read + frame) % self.capacity()) * self.channels + channel]
    }
    fn push_frame(&mut self, frame: &[f64]) {
        let start = ((self.read + self.frames) % self.capacity()) * self.channels;
        self.data[start..start + self.channels].copy_from_slice(frame);
        self.frames += 1;
    }
    fn consume(&mut self, frames: usize) {
        self.read = (self.read + frames) % self.capacity();
        self.frames -= frames;
    }
    fn reset(&mut self) {
        self.read = 0;
        self.frames = 0;
    }
}
fn zeros(count: usize) -> Result<Box<[f64]>, MediaError> {
    let mut data = Vec::new();
    data.try_reserve_exact(count)
        .map_err(|_| MediaError::BudgetExceeded)?;
    if data.capacity() != count {
        return Err(MediaError::BudgetExceeded);
    }
    data.resize(count, 0.0);
    Ok(data.into_boxed_slice())
}
fn planar(channels: usize, frames: usize) -> Result<Vec<Vec<f64>>, MediaError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(channels)
        .map_err(|_| MediaError::BudgetExceeded)?;
    if result.capacity() != channels {
        return Err(MediaError::BudgetExceeded);
    }
    for _ in 0..channels {
        result.push(zeros(frames)?.into_vec());
    }
    Ok(result)
}

pub struct AudioConverter {
    input: AudioFormat,
    output: AudioFormat,
    limits: AudioConvertLimits,
    matrix: [[f64; 8]; 8],
    matrix_identity: bool,
    resampler: Option<Async<f64>>,
    phase: Option<phase::PhaseAlignment>,
    input_fifo: Fifo,
    output_fifo: Fifo,
    packet: Box<[f64]>,
    input_scratch: Vec<Vec<f64>>,
    output_scratch: Vec<Vec<f64>>,
    processed_blocks: u64,
    accepted: u64,
    consumed: u64,
    produced: u64,
    delivered: u64,
    padded: u64,
    delay: usize,
    leading: usize,
    accounted: usize,
    sealed: bool,
    poisoned: bool,
    relative_scaled: u64,
    ideal_numerator: u128,
}
/// Exact pinned constructor admission; evaluating this performs no DSP or allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioConversionRequirements {
    pub accounted_bytes: usize,
    pub max_processor_input_frames: usize,
    pub max_processor_output_frames: usize,
}
struct ConversionPlan {
    matrix: [[f64; 8]; 8],
    matrix_identity: bool,
    different_rate: bool,
    ratio: f64,
    max_input: usize,
    accounted: usize,
}
fn conversion_plan(input: AudioFormat, output: AudioFormat, limits: AudioConvertLimits) -> Result<ConversionPlan, MediaError> {
    if !(1..=4096).contains(&limits.max_input_frames)
        || !(1..=CHUNK).contains(&limits.max_output_frames)
        || limits.max_buffered_input_frames < limits.max_input_frames
        || limits.max_buffered_input_frames > 65536
        || limits.max_buffered_output_frames < CHUNK
        || limits.max_buffered_output_frames > 65536
    {
        return Err(MediaError::BudgetExceeded);
    }
    let channels = usize::from(output.channels);
    let matrix = matrix::build(input, output)?;
    let matrix_identity = input.channels == output.channels
        && matrix
            .iter()
            .enumerate()
            .take(channels)
            .all(|(row, values)| {
                values
                    .iter()
                    .enumerate()
                    .take(channels)
                    .all(|(column, value)| *value == if row == column { 1.0 } else { 0.0 })
            });
    let different_rate = input.sample_rate != output.sample_rate;
    let ratio = f64::from(output.sample_rate) / f64::from(input.sample_rate);
    // Integer ceiling bounds Rubato's fixed-output max-input formula,
    // including the complete +/-1% ratio envelope, history and rounding.
    let max_input = if different_rate {
        let numerator = (CHUNK as u128)
            .checked_mul(u128::from(input.sample_rate))
            .and_then(|n| n.checked_mul(101))
            .ok_or(MediaError::Overflow)?;
        let denominator = u128::from(output.sample_rate) * 100;
        usize::try_from(numerator.div_ceil(denominator)).map_err(|_| MediaError::Overflow)?
            + SINC_LEN / 2
            + 4
    } else {
        0
    };
    let required_input_fifo = if different_rate {
        max_input
            .checked_add(limits.max_input_frames - 1)
            .ok_or(MediaError::Overflow)?
    } else {
        limits.max_input_frames
    };
    if required_input_fifo > limits.max_buffered_input_frames {
        return Err(MediaError::BudgetExceeded);
    }
    let payload_samples = limits
        .max_buffered_input_frames
        .checked_add(limits.max_buffered_output_frames)
        .and_then(|n| n.checked_add(limits.max_input_frames))
        .and_then(|n| n.checked_add(max_input))
        .and_then(|n| {
            n.checked_add(if different_rate {
                CHUNK + max_input + 2 * SINC_LEN
            } else {
                0
            })
        })
        .and_then(|n| n.checked_mul(channels))
        .ok_or(MediaError::Overflow)?;
    // Four complete coefficient banks cover pinned make_sincs construction
    // temporaries + aligned table copies, including per-row metadata. This
    // is an admission bound for payloads, not allocator/OS RSS accounting.
    let coefficients = if different_rate {
        4 * SINC_LEN * OVERSAMPLING * 8 + 4 * OVERSAMPLING * 64 + 2 * (SINC_LEN + 1) * 8
    } else {
        0
    };
    let phase_bytes = if different_rate {
        (channels + 1) * phase::TAPS * 8
    } else {
        0
    };
    let accounted = payload_samples
        .checked_mul(8)
        .and_then(|n| n.checked_add(coefficients))
        .and_then(|n| n.checked_add(phase_bytes))
        .and_then(|n| n.checked_add(64 * 1024 + std::mem::size_of::<AudioConverter>()))
        .ok_or(MediaError::Overflow)?;
    if accounted > limits.max_bytes {
        return Err(MediaError::BudgetExceeded);
    }
    Ok(ConversionPlan { matrix, matrix_identity, different_rate, ratio, max_input, accounted })
}
pub fn audio_conversion_requirements(input: AudioFormat, output: AudioFormat, limits: AudioConvertLimits) -> Result<AudioConversionRequirements, MediaError> {
    let plan = conversion_plan(input, output, limits)?;
    Ok(AudioConversionRequirements { accounted_bytes: plan.accounted,
        max_processor_input_frames: plan.max_input,
        max_processor_output_frames: if plan.different_rate { CHUNK } else { limits.max_input_frames } })
}

impl AudioConverter {
    pub fn new(
        input: AudioFormat,
        output: AudioFormat,
        limits: AudioConvertLimits,
    ) -> Result<Self, MediaError> {
        let ConversionPlan { matrix, matrix_identity, different_rate, ratio, max_input, accounted } =
            conversion_plan(input, output, limits)?;
        let channels = usize::from(output.channels);
        let resampler = if different_rate {
            // The usual 256-sample sinc requires 1.6 s of input at an
            // effective media rate of 80 Hz. Use a shorter bounded kernel for
            // those rates so a realtime producer can prime it inside its
            // lookahead; accounting above deliberately retains the larger
            // 256-tap memory bound.
            let sinc_len=if input.sample_rate<8000 {16} else {SINC_LEN};
            let params =
                SincInterpolationParameters::new(sinc_len, WindowFunction::BlackmanHarris2)
                    .f_cutoff(
                        calculate_cutoff::<f32>(sinc_len, WindowFunction::BlackmanHarris2)
                            / MAX_RATIO_ADJUSTMENT as f32,
                    );
            let resampler = Async::<f64>::new_sinc(
                ratio,
                MAX_RATIO_ADJUSTMENT,
                &params,
                CHUNK,
                channels,
                FixedAsync::Output,
            )
            .map_err(|_| MediaError::InvalidAudio)?;
            if resampler.input_frames_max() > max_input || resampler.output_frames_max() > CHUNK {
                return Err(MediaError::BudgetExceeded);
            }
            Some(resampler)
        } else {
            None
        };
        let raw_delay = resampler.as_ref().map_or(0, Resampler::output_delay);
        let phase = if different_rate {
            Some(phase::PhaseAlignment::new(
                input.sample_rate,
                output.sample_rate,
                raw_delay,
                channels,
            )?)
        } else {
            None
        };
        let delay = raw_delay
            .checked_add(if different_rate { phase::CENTER } else { 0 })
            .ok_or(MediaError::Overflow)?;
        Ok(Self {
            input,
            output,
            limits,
            matrix,
            matrix_identity,
            resampler,
            phase,
            input_fifo: Fifo::new(limits.max_buffered_input_frames, channels)?,
            output_fifo: Fifo::new(limits.max_buffered_output_frames, channels)?,
            packet: zeros(limits.max_input_frames * channels)?,
            input_scratch: planar(channels, max_input)?,
            output_scratch: planar(channels, if different_rate { CHUNK } else { 0 })?,
            processed_blocks: 0,
            accepted: 0,
            consumed: 0,
            produced: 0,
            delivered: 0,
            padded: 0,
            delay,
            leading: delay,
            accounted,
            sealed: false,
            poisoned: false,
            relative_scaled: RATIO_SCALE,
            ideal_numerator: 0,
        })
    }
    pub fn input_format(&self) -> AudioFormat {
        self.input
    }
    pub fn output_format(&self) -> AudioFormat {
        self.output
    }
    /// Remaining admission capacity, in complete input frames.
    pub fn input_capacity_frames(&self) -> usize {
        self.input_fifo.free()
    }
    /// Removed leading filter delay; returned PCM already starts at input time0.
    pub fn delay_output_frames(&self) -> usize {
        self.delay
    }
    fn target_frames(&self) -> Result<u64, MediaError> {
        let denominator = u128::from(self.input.sample_rate) * u128::from(RATIO_SCALE);
        u64::try_from(self.ideal_numerator.div_ceil(denominator)).map_err(|_| MediaError::Overflow)
    }
    pub fn stats(&self) -> AudioConversionStats {
        AudioConversionStats {
            processed_blocks: self.processed_blocks,
            accepted_input_frames: self.accepted,
            consumed_input_frames: self.consumed,
            produced_output_frames: self.produced,
            delivered_output_frames: self.delivered,
            padded_input_frames: self.padded,
            delay_output_frames: self.delay,
            queued_input_frames: self.input_fifo.frames,
            queued_output_frames: self.output_fifo.frames,
            accounted_bytes: self.accounted,
            finished: self.sealed,
            drained: self.sealed
                && self.output_fifo.frames == 0
                && self
                    .target_frames()
                    .is_ok_and(|target| self.delivered >= target),
        }
    }
    pub fn push(&mut self, samples: &[f32]) -> Result<(), MediaError> {
        if self.sealed {
            return Err(MediaError::Closed);
        }
        if self.poisoned {
            return Err(MediaError::InvalidAudio);
        }
        let input_channels = usize::from(self.input.channels);
        if samples.len() % input_channels != 0 || samples.is_empty() {
            return Err(MediaError::InvalidBuffer);
        }
        let frames = samples.len() / input_channels;
        if frames > self.limits.max_input_frames || frames > self.input_fifo.free() {
            return Err(MediaError::BudgetExceeded);
        }
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(MediaError::InvalidAudio);
        }
        let accepted = self
            .accepted
            .checked_add(frames as u64)
            .ok_or(MediaError::Overflow)?;
        let increment = (frames as u128)
            .checked_mul(u128::from(self.output.sample_rate))
            .and_then(|n| n.checked_mul(u128::from(self.relative_scaled)))
            .ok_or(MediaError::Overflow)?;
        let ideal_numerator = self
            .ideal_numerator
            .checked_add(increment)
            .ok_or(MediaError::Overflow)?;
        let denominator = u128::from(self.input.sample_rate) * u128::from(RATIO_SCALE);
        u64::try_from(ideal_numerator.div_ceil(denominator)).map_err(|_| MediaError::Overflow)?;
        let output_channels = usize::from(self.output.channels);
        // Stage the entire packet before FIFO/counter publication, including
        // representability checks on large but finite input values.
        for frame in 0..frames {
            for channel in 0..output_channels {
                let mut sum = if self.matrix_identity {
                    f64::from(samples[frame * input_channels + channel])
                } else {
                    0.0
                };
                if !self.matrix_identity {
                    for input in 0..input_channels {
                        sum += f64::from(samples[frame * input_channels + input])
                            * self.matrix[channel][input];
                    }
                }
                if !sum.is_finite() || sum.abs() > f64::from(f32::MAX) {
                    return Err(MediaError::InvalidAudio);
                }
                self.packet[frame * output_channels + channel] = sum;
            }
        }
        for frame in self.packet[..frames * output_channels].chunks_exact(output_channels) {
            self.input_fifo.push_frame(frame);
        }
        self.accepted = accepted;
        self.ideal_numerator = ideal_numerator;
        Ok(())
    }
    /// Seal finite input. Pull until stats().drained; zero filter padding is
    /// never counted as accepted/consumed source data or returned tail audio.
    pub fn finish(&mut self) -> Result<(), MediaError> {
        if self.poisoned {
            return Err(MediaError::InvalidAudio);
        }
        self.target_frames()?;
        self.sealed = true;
        Ok(())
    }
    /// Relative ratio in [1/1.01,1.01]. Applies a smooth next-block ramp.
    /// A rate-identity bypass explicitly rejects adjustments requiring a filter.
    pub fn set_ratio_adjustment(&mut self, relative: f64) -> Result<(), MediaError> {
        if self.sealed {
            return Err(MediaError::Closed);
        }
        if self.poisoned
            || !relative.is_finite()
            || !(1.0 / MAX_RATIO_ADJUSTMENT..=MAX_RATIO_ADJUSTMENT).contains(&relative)
        {
            return Err(MediaError::InvalidAudio);
        }
        if let Some(resampler) = self.resampler.as_mut() {
            resampler
                .set_resample_ratio_relative(relative, true)
                .map_err(|_| MediaError::InvalidAudio)?;
        } else if relative != 1.0 {
            return Err(MediaError::InvalidAudio);
        }
        self.relative_scaled = (relative * RATIO_SCALE as f64) as u64;
        Ok(())
    }
    pub fn reset(&mut self) {
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
        if let Some(phase) = self.phase.as_mut() {
            phase.reset();
        }
        self.input_fifo.reset();
        self.output_fifo.reset();
        self.processed_blocks = 0;
        self.accepted = 0;
        self.consumed = 0;
        self.produced = 0;
        self.delivered = 0;
        self.padded = 0;
        self.leading = self.delay;
        self.sealed = false;
        self.poisoned = false;
        self.relative_scaled = RATIO_SCALE;
        self.ideal_numerator = 0;
    }
    pub fn pull(&mut self, samples: &mut [f32]) -> Result<usize, MediaError> {
        if self.poisoned {
            return Err(MediaError::InvalidAudio);
        }
        let channels = usize::from(self.output.channels);
        if samples.len() % channels != 0 {
            return Err(MediaError::InvalidBuffer);
        }
        let requested = samples.len() / channels;
        if requested > self.limits.max_output_frames {
            return Err(MediaError::BudgetExceeded);
        }
        if requested == 0 {
            return Ok(0);
        }
        if self.resampler.is_none() {
            let frames = requested.min(self.input_fifo.frames);
            let next_consumed = self
                .consumed
                .checked_add(frames as u64)
                .ok_or(MediaError::Overflow)?;
            let next_produced = self
                .produced
                .checked_add(frames as u64)
                .ok_or(MediaError::Overflow)?;
            let next_delivered = self
                .delivered
                .checked_add(frames as u64)
                .ok_or(MediaError::Overflow)?;
            for frame in 0..frames {
                for channel in 0..channels {
                    samples[frame * channels + channel] =
                        self.input_fifo.value(frame, channel) as f32;
                }
            }
            self.input_fifo.consume(frames);
            self.consumed = next_consumed;
            self.produced = next_produced;
            self.delivered = next_delivered;
            return Ok(frames);
        }
        // Largest supported ratio needs at most three discarded startup blocks
        // plus two output blocks; a finite bound also covers tail progress.
        for _ in 0..16 {
            if self.output_fifo.frames >= requested || !self.process_one()? {
                break;
            }
        }
        let frames = requested.min(self.output_fifo.frames);
        let delivered = self
            .delivered
            .checked_add(frames as u64)
            .ok_or(MediaError::Overflow)?;
        for frame in 0..frames {
            for channel in 0..channels {
                samples[frame * channels + channel] = self.output_fifo.value(frame, channel) as f32;
            }
        }
        self.output_fifo.consume(frames);
        self.delivered = delivered;
        Ok(frames)
    }
    fn process_one(&mut self) -> Result<bool, MediaError> {
        let target = self.target_frames()?;
        if self.sealed && self.produced >= target {
            return Ok(false);
        }
        let resampler = self.resampler.as_mut().unwrap();
        let needed = resampler.input_frames_next();
        if (!self.sealed && self.input_fifo.frames < needed) || self.output_fifo.free() < CHUNK {
            return Ok(false);
        }
        let real = needed.min(self.input_fifo.frames);
        let consumed = self
            .consumed
            .checked_add(real as u64)
            .ok_or(MediaError::Overflow)?;
        let padded = self
            .padded
            .checked_add((needed - real) as u64)
            .ok_or(MediaError::Overflow)?;
        let upper_produced = self
            .produced
            .checked_add(CHUNK as u64)
            .ok_or(MediaError::Overflow)?;
        let _ = upper_produced;
        for (channel, scratch) in self.input_scratch.iter_mut().enumerate() {
            for (frame, value) in scratch[..needed].iter_mut().enumerate() {
                *value = if frame < real {
                    self.input_fifo.value(frame, channel)
                } else {
                    0.0
                };
            }
        }
        let channels = usize::from(self.output.channels);
        let input = SequentialSliceOfVecs::new(&self.input_scratch, channels, needed)
            .map_err(|_| MediaError::InvalidAudio)?;
        let mut output = SequentialSliceOfVecs::new_mut(&mut self.output_scratch, channels, CHUNK)
            .map_err(|_| MediaError::InvalidAudio)?;
        // Charge an actual attempt, including a backend failure, rather than
        // treating a no-input pull as1024 frames of expensive sinc work.
        self.processed_blocks=self.processed_blocks.checked_add(1).ok_or(MediaError::Overflow)?;
        let result = resampler.process_into_buffer(&input, &mut output, None);
        let Ok((used, produced)) = result else {
            self.poisoned = true;
            return Err(MediaError::InvalidAudio);
        };
        if used != needed || produced != CHUNK {
            self.poisoned = true;
            return Err(MediaError::InvalidAudio);
        }
        // Include the complete raw prefix in the fractional-delay history;
        // trimming before this stage would invent zero history at input time0.
        self.phase
            .as_mut()
            .unwrap()
            .process(&mut self.output_scratch);
        self.input_fifo.consume(real);
        self.consumed = consumed;
        self.padded = padded;
        let skip = self.leading.min(produced);
        self.leading -= skip;
        let count = if self.sealed {
            (produced - skip).min(usize::try_from(target - self.produced).unwrap_or(usize::MAX))
        } else {
            produced - skip
        };
        for channel in &self.output_scratch {
            for value in &channel[skip..skip + count] {
                if !value.is_finite() || value.abs() > f64::from(f32::MAX) {
                    self.poisoned = true;
                    return Err(MediaError::InvalidAudio);
                }
            }
        }
        let mut frame = [0.0; 8];
        for index in skip..skip + count {
            for (channel, value) in frame[..channels].iter_mut().enumerate() {
                *value = self.output_scratch[channel][index];
            }
            self.output_fifo.push_frame(&frame[..channels]);
        }
        self.produced += count as u64;
        Ok(true)
    }
}
