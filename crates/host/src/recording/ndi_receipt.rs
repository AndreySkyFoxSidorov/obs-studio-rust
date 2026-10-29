//! NDI video-only evidence for a specific accepted recording frame.
//! Sender-to-local mapping is arrival-based; it does not bound network delay.
use super::owner::RecordingFrame;
use skylive_core::Id;
use skylive_media::MediaTime;

#[derive(Clone,Debug)]
pub struct NdiVideoReceipt {
    pub recording:u64,pub session:u64,pub revision:u64,pub source:Id,
    pub submitted_at:MediaTime,pub presentation_at:MediaTime,pub presentation_end:MediaTime,
    pub video_index:u64,
    /// This value is an arrival-anchored estimate with unknown network error.
    pub estimated_sender_media_ns:Option<i128>,
    pub(crate) delay:crate::capture::AsyncDelayReceipt,
}
impl NdiVideoReceipt {
    pub fn current(&self)->bool {self.delay.current()}
    pub fn sequence(&self)->u64 {self.delay.sequence}
    pub fn elapsed_ns(&self)->u64 {self.delay.elapsed_ns}
    /// Scope identity of the original delayed stream, independent of sequence.
    /// Used to retain the first writer-accepted anchor without joining two
    /// recordings, epochs, sender clock domains or independently reset queues.
    pub(crate) fn same_scope(&self,other:&Self)->bool {
        self.recording==other.recording && self.session==other.session
            && self.revision==other.revision && self.source==other.source
            && self.delay.binding_generation==other.delay.binding_generation
            && self.delay.owner==other.delay.owner && self.delay.stream==other.delay.stream
            && self.delay.clock_epoch==other.delay.clock_epoch && self.delay.filters==other.delay.filters
            && self.delay.ndi_native_clock==other.delay.ndi_native_clock
            && self.delay.history_generation==other.delay.history_generation
            && std::sync::Arc::ptr_eq(&self.delay.history_live,&other.delay.history_live)
    }
    /// Raw sender timestamp and its clock domain, kept separate from the
    /// arrival-anchored local estimate. It is not a synchronization proof.
    pub fn sender_timestamp(&self)->Option<(crate::ndi::NdiVideoClock,i128)> {
        Some((self.delay.ndi_native_clock?,self.delay.timestamp_ns))
    }
    /// Offset between the sender's audio packet start and this accepted video
    /// frame. This preserves the sender clock domain and stream identity; it
    /// does not establish the mapping to a local output/presentation clock.
    pub(crate) fn audio_sender_offset_ns(&self,packet:&crate::ndi::NdiPcmPacket)->Option<i128> {
        if !self.current() || !packet.scope_current() || self.session!=packet.program.session
            || self.source!=packet.source || self.delay.owner!=packet.owner
            || self.delay.binding_generation!=packet.scope_epoch || self.delay.stream!=packet.stream_generation
            || packet.native_clock_epoch==0 {return None;}
        let (video_clock,video_ns)=self.sender_timestamp()?;
        if packet.native_clock?!=video_clock {return None;}
        packet.native_timestamp_ns?.checked_sub(video_ns)
    }
}

pub(super) fn accepted_samples(frame:&RecordingFrame,trace:&[Id],recording:u64,index:u64,
    presentation_at:MediaTime,presentation_end:MediaTime)->Vec<NdiVideoReceipt> {
    if recording==0 || frame.capture.session()!=frame.session || trace.len()>skylive_core::MAX_SOURCES {return Vec::new();}
    let mut receipts=Vec::new();
    if receipts.try_reserve(trace.len()).is_err(){return receipts;}
    let providers=crate::video_filters::FilterProviders::from_snapshot(&frame.model);
    for view in frame.output.scenes() {
        let Ok(sources)=view.visible_sources(&frame.model) else {return Vec::new();};
        for source in sources {
            if !trace.contains(&source.id()) || receipts.iter().any(|old:&NdiVideoReceipt|old.source==source.id()){continue;}
            let Some(sample)=frame.capture.frame(source.id()) else {continue;};
            if !crate::video_filters::ndi_recording_delay_applied(source,frame.session,sample,&sample.image,providers){continue;}
            let Some(delay)=sample.async_delay.as_ref() else {continue;};
            receipts.push(NdiVideoReceipt {recording,session:frame.session,revision:frame.model.revision(),source:source.id(),
                submitted_at:frame.time,presentation_at,presentation_end,video_index:index,
                estimated_sender_media_ns:delay.ndi_media_time_estimate_ns,delay:delay.clone()});
        }
    }
    receipts
}
