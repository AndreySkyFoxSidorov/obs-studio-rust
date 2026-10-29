use super::*;
/// An asynchronous file packet is fenced by the complete Program admission
/// key. Native and file identities remain distinct in the mixer.
pub(crate) struct MediaPcmPacket {
    pub program: ProgramCaptureKey,
    pub source: Id,
    pub media: Arc<MediaSource>,
    pub stream_generation: u64,
    pub sequence: u64,
    pub first_media_ns: i128,
    pub format: NativeAudioFormat,
    pub samples: Box<[f32]>,
    pub end: bool,
    pub sample_offset: usize,
    pub barrier: Option<Arc<super::control::MediaBarrier>>,
}
#[derive(Clone)]
pub struct MediaAudioSink {
    pub(crate) sender: mpsc::SyncSender<Arc<MediaPcmPacket>>,
}
impl MediaAudioSink {
    pub(crate) fn send(
        &self,
        packet: Arc<MediaPcmPacket>,
    ) -> Result<(), mpsc::TrySendError<Arc<MediaPcmPacket>>> {
        self.sender.try_send(packet)
    }
}
