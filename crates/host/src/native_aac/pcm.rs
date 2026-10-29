//! Bounded worker-side PCM conversion and exact residual/EOS accounting.
#[cfg(any(test, target_os = "windows"))]
use super::FRAME_LEN;
use super::{Config, Error, Generation, MAX_INPUT_FRAMES};
#[derive(Debug)]
pub struct PcmInput {
    generation: Generation,
    channels: u16,
    sample_rate:u32,
    first_frame: u64,
    samples: Box<[i16]>,
}
impl PcmInput {
    pub fn new(config: &Config, first_frame: u64, samples: Box<[i16]>) -> Result<Self, Error> {
        if samples.is_empty()
            || samples.len() % usize::from(config.channels()) != 0
            || samples.len() / usize::from(config.channels()) > MAX_INPUT_FRAMES
        {
            return Err(Error::Invalid("PCM16 packet dimensions"));
        }
        config.sample_timing(
            first_frame,
            (samples.len() / usize::from(config.channels())) as u64,
        )?;
        Ok(Self {
            generation: config.generation(),
            channels: config.channels(),
            sample_rate:config.sample_rate(),
            first_frame,
            samples,
        })
    }
    pub fn first_frame(&self) -> u64 {
        self.first_frame
    }
    pub fn frames(&self) -> u64 {
        (self.samples.len() / usize::from(self.channels)) as u64
    }
    pub fn samples(&self) -> &[i16] {
        &self.samples
    }
    pub(crate) fn validate(&self, config: &Config) -> Result<(), Error> {
        if self.generation != config.generation() {
            return Err(Error::Generation);
        }
        if self.channels != config.channels() || self.sample_rate!=config.sample_rate(){
            return Err(Error::Invalid("PCM channel mismatch"));
        }
        Ok(())
    }
}
/// Host-worker only. Finite float PCM is clipped to[-1,1], multiplied by32768,
/// rounded to nearest and saturated to signed16. This is not a resampler.
pub fn pcm16_interleaved(samples: &[f32], channels: u16) -> Result<Box<[i16]>, Error> {
    if !(1..=2).contains(&channels)
        || samples.is_empty()
        || samples.len() % usize::from(channels) != 0
        || samples.len() / usize::from(channels) > MAX_INPUT_FRAMES
    {
        return Err(Error::Invalid("float PCM packet dimensions"));
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(Error::Invalid("nonfinite PCM"));
    }
    Ok(samples
        .iter()
        .map(|sample| {
            (sample.clamp(-1.0, 1.0) * 32768.0)
                .round()
                .clamp(-32768.0, 32767.0) as i16
        })
        .collect::<Vec<_>>()
        .into_boxed_slice())
}
#[cfg(any(test, target_os = "windows"))]
pub(crate) struct PcmBlock {
    pub first_frame: u64,
    pub samples: Box<[i16]>,
}
#[cfg(any(test, target_os = "windows"))]
pub(crate) struct Assembler {
    config: Config,
    residual: Vec<i16>,
    pub received: u64,
    pub submitted: u64,
    pub padding: u16,
    sealed: bool,
}
#[cfg(any(test, target_os = "windows"))]
impl Assembler {
    pub fn new(config: Config) -> Self {
        Self {
            residual: Vec::with_capacity(FRAME_LEN * usize::from(config.channels())),
            config,
            received: 0,
            submitted: 0,
            padding: 0,
            sealed: false,
        }
    }
    pub fn push(&mut self, input: PcmInput) -> Result<Option<PcmBlock>, Error> {
        input.validate(&self.config)?;
        if self.sealed {
            return Err(Error::Protocol("PCM after EOS"));
        }
        if input.first_frame() != self.received {
            return Err(Error::Invalid("noncontiguous PCM frame index"));
        }
        let next_received = self
            .received
            .checked_add(input.frames())
            .ok_or(Error::Invalid("PCM counter overflow"))?;
        let size = FRAME_LEN * usize::from(self.config.channels());
        let needed = size - self.residual.len();
        if input.samples().len() < needed {
            self.residual.extend_from_slice(input.samples());
            self.received = next_received;
            return Ok(None);
        }
        let next_submitted = self
            .submitted
            .checked_add(FRAME_LEN as u64)
            .ok_or(Error::Invalid("PCM counter overflow"))?;
        self.config
            .sample_timing(self.submitted, FRAME_LEN as u64)?;
        let first_frame = self.submitted;
        self.residual.extend_from_slice(&input.samples()[..needed]);
        let completed =
            std::mem::replace(&mut self.residual, Vec::with_capacity(size)).into_boxed_slice();
        self.residual.extend_from_slice(&input.samples()[needed..]);
        self.received = next_received;
        self.submitted = next_submitted;
        Ok(Some(PcmBlock {
            first_frame,
            samples: completed,
        }))
    }
    pub fn finish(&mut self) -> Result<Option<PcmBlock>, Error> {
        if self.sealed {
            return Err(Error::Protocol("PCM EOS already sent"));
        }
        if self.residual.is_empty() {
            self.sealed = true;
            return Ok(None);
        }
        let next = self
            .submitted
            .checked_add(FRAME_LEN as u64)
            .ok_or(Error::Invalid("PCM EOS counter overflow"))?;
        self.config
            .sample_timing(self.submitted, FRAME_LEN as u64)?;
        let padding = FRAME_LEN - self.residual.len() / usize::from(self.config.channels());
        self.residual
            .resize(FRAME_LEN * usize::from(self.config.channels()), 0);
        let samples = std::mem::take(&mut self.residual).into_boxed_slice();
        let first_frame = self.submitted;
        self.submitted = next;
        self.padding = padding as u16;
        self.sealed = true;
        Ok(Some(PcmBlock {
            first_frame,
            samples,
        }))
    }
    #[cfg(test)]
    pub fn residual_frames(&self) -> usize {
        self.residual.len() / usize::from(self.config.channels())
    }
}
