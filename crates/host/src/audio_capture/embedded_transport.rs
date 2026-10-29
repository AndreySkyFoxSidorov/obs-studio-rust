//! Bounded ownership transfer from the camera graph owner to the audio actor.
use super::{AudioCaptureError, NativePcmBlock, ProgramCaptureKey};
use skylive_core::{Id, VideoCaptureBinding};
use skylive_media::QpcMediaCorrelation;
use std::sync::{Arc, mpsc};

/// A block is owned by the channel after successful submission. QPC in the
/// block is a host receive observation, not a DirectShow device timestamp.
pub struct EmbeddedPcmPacket {
    pub program: ProgramCaptureKey,
    pub source: Id,
    pub binding: Arc<VideoCaptureBinding>,
    pub stream_generation: u64,
    pub sequence: u64,
    pub block: NativePcmBlock,
    pub correlation: QpcMediaCorrelation,
}

#[derive(Clone)]
pub struct EmbeddedPcmSink { pub(super) sender: mpsc::SyncSender<EmbeddedPcmPacket> }
impl EmbeddedPcmSink {
    /// A rejected packet remains a discontinuity in the native stream. The
    /// producer must mark its next accepted block rather than block a callback.
    pub fn submit(&self, packet: EmbeddedPcmPacket) -> Result<(), AudioCaptureError> {
        self.sender.try_send(packet).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => AudioCaptureError::Busy,
            mpsc::TrySendError::Disconnected(_) => AudioCaptureError::Closed,
        })
    }
}
