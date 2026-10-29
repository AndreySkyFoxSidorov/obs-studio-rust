//! Selected dependency-free FilmCraft audio kernels. See SOURCE.json/NOTICE.
//! Compressor is integrated; Gate/SimpleEq are independent CPU kernels awaiting
//! authored-filter and host-chain integration. They do not own native capture.
mod compressor;
mod eq;
mod gate;
pub use compressor::{
    Compressor, CompressorError, CompressorParameters, Detector, MAX_COMPRESSOR_FRAMES,
};
pub use eq::{EqError, MAX_EQ_FRAMES, SimpleEq, SimpleEqParameters};
pub use gate::{Gate, GateError, GateParameters, MAX_GATE_FRAMES};
