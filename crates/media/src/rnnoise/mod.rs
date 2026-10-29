//! Genuine original-model RNNoise, implemented entirely in Rust.
//!
//! This primitive processes exactly one mono 480-sample frame at 48 kHz.
//! Input/output use normalized float units. It includes RNNoise's real overlap
//! delay; it does NOT hide that delay with synthesized startup capture packets.
//! The OBS packet/timestamp adapter belongs to the host and is not supplied by
//! this module. No application capability is implied by exporting a primitive.
//!
//! Based on the BSD-licensed RNNoise bundled with OBS Studio 32.2.2. See
//! assets/licenses/RNNOISE.txt and doc/modules/obs-rnnoise.md.

mod fft;
mod network;
pub mod packets;
mod pitch;
mod signal;
mod weights;

use signal::{State, Tables};
use std::{error::Error as StdError, fmt, mem::size_of};

pub const FRAME_SIZE: usize = 480;
pub const SAMPLE_RATE: u32 = 48_000;
pub const ALGORITHM_DELAY_FRAMES: usize = 480;
pub const ALGORITHM_DELAY_NS: u64 = 10_000_000;
pub const MODEL_ID: &str = "obs-32.2.2/rnnoise_model_orig";

// A numeric admission ceiling, not a signal limiter: 60 dB above normalized
// full scale. Internal original-math overflow still fails explicitly rather
// than clamping the audio, concealing a bad state or publishing nonfinite PCM.
pub const MAX_INPUT_MAGNITUDE: f32 = 1024.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirements {
    /// Source-owned tables/state, Vec owners and allocation-header allowance.
    pub retained_bytes: usize,
    /// Conservative maximum nested scalar processing/construction stack.
    pub scratch_bytes: usize,
    /// Conservative scalar operations/visits for one mono 10 ms frame.
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
        Self { max_retained_bytes: 64 * 1024, max_scratch_bytes: 128 * 1024, max_work_units: 1_000_000 }
    }
}

pub fn requirements() -> Requirements {
    // No dynamically shaped model or caller-controlled FFT is accepted. All
    // arrays below have fixed dimensions; this sum cannot overflow usize.
    Requirements { retained_bytes: size_of::<Denoiser>() + size_of::<Tables>() + size_of::<State>() + 128,
        scratch_bytes: 128 * 1024, work_units: 1_000_000 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    FrameShape,
    NonFiniteInput,
    InputMagnitude,
    Limit,
    Allocation,
    Poisoned,
    NonFiniteOutput,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::FrameShape => "RNNoise requires exactly 480 mono samples at 48 kHz",
            Self::NonFiniteInput => "RNNoise input contains a nonfinite value",
            Self::InputMagnitude => "RNNoise input exceeds the finite original-math admission range",
            Self::Limit => "RNNoise state, scratch or work limit exceeded",
            Self::Allocation => "RNNoise state allocation failed",
            Self::Poisoned => "RNNoise instance requires reset after a processing failure",
            Self::NonFiniteOutput => "RNNoise produced a nonfinite internal state or output",
        })
    }
}
impl StdError for Error {}

#[derive(Clone, Copy, Debug)]
pub struct FrameResult {
    pub speech_probability: f32,
    pub silent_analysis: bool,
}

/// One channel, one source epoch, no global state, queue, callback or device.
/// Use `try_clone` for an admitted transactional snapshot; it is deliberately
/// not an infallible Clone that conceals heap allocation in a host DSP clone.
pub struct Denoiser {
    tables: Vec<Tables>,
    state: Vec<State>,
    poisoned: bool,
}

impl Denoiser {
    pub fn new(limits: Limits) -> Result<Self, Error> {
        admit(limits)?;
        let mut tables = Vec::new();
        let mut state = Vec::new();
        tables.try_reserve_exact(1).map_err(|_| Error::Allocation)?;
        state.try_reserve_exact(1).map_err(|_| Error::Allocation)?;
        admit_capacity(tables.capacity(), state.capacity(), limits)?;
        tables.push(Tables::new());
        state.push(State::new());
        Ok(Self { tables, state, poisoned: false })
    }

    pub fn try_clone(&self, limits: Limits) -> Result<Self, Error> {
        admit(limits)?;
        let tables = self.tables.first().ok_or(Error::Allocation)?;
        let state = self.state.first().ok_or(Error::Allocation)?;
        let mut next_tables = Vec::new();
        let mut next_state = Vec::new();
        next_tables.try_reserve_exact(1).map_err(|_| Error::Allocation)?;
        next_state.try_reserve_exact(1).map_err(|_| Error::Allocation)?;
        admit_capacity(next_tables.capacity(), next_state.capacity(), limits)?;
        next_tables.push(tables.clone());
        next_state.push(state.clone());
        Ok(Self { tables: next_tables, state: next_state, poisoned: self.poisoned })
    }

    /// All shape/input/work checks precede state mutation. On an unexpected
    /// arithmetic failure, output remains unchanged and this instance is
    /// poisoned; the host discards its transactional candidate or resets it.
    pub fn process(&mut self, input: &[f32], output: &mut [f32], limits: Limits) -> Result<FrameResult, Error> {
        admit(limits)?;
        if input.len() != FRAME_SIZE || output.len() != FRAME_SIZE { return Err(Error::FrameShape); }
        if input.iter().any(|x| !x.is_finite()) { return Err(Error::NonFiniteInput); }
        if input.iter().any(|x| x.abs() > MAX_INPUT_MAGNITUDE) { return Err(Error::InputMagnitude); }
        if self.poisoned { return Err(Error::Poisoned); }
        let tables = self.tables.first().ok_or(Error::Allocation)?;
        let state = self.state.first_mut().ok_or(Error::Allocation)?;
        let mut staged = [0.; FRAME_SIZE];
        let (speech_probability, silent_analysis) = state.process(tables, input, &mut staged);
        if !speech_probability.is_finite() || !state.finite() || staged.iter().any(|x| !x.is_finite()) {
            self.poisoned = true;
            return Err(Error::NonFiniteOutput);
        }
        output.copy_from_slice(&staged);
        Ok(FrameResult { speech_probability, silent_analysis })
    }

    pub fn reset(&mut self) -> Result<(), Error> {
        let state = self.state.first_mut().ok_or(Error::Allocation)?;
        *state = State::new();
        self.poisoned = false;
        Ok(())
    }
}

fn admit(limits: Limits) -> Result<(), Error> {
    let needed = requirements();
    if needed.retained_bytes > limits.max_retained_bytes || needed.scratch_bytes > limits.max_scratch_bytes
        || needed.work_units > limits.max_work_units { return Err(Error::Limit); }
    Ok(())
}

fn admit_capacity(tables: usize, state: usize, limits: Limits) -> Result<(), Error> {
    let held = tables.checked_mul(size_of::<Tables>())
        .and_then(|v| state.checked_mul(size_of::<State>()).and_then(|s| v.checked_add(s)))
        .and_then(|v| v.checked_add(size_of::<Denoiser>() + 128)).ok_or(Error::Limit)?;
    if held > limits.max_retained_bytes { return Err(Error::Limit); }
    Ok(())
}
