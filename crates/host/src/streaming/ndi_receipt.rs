//! Stream-specific NDI video evidence. A candidate becomes visible only when
//! the H.264 encoder returns an access unit with the exact submitted PTS.
use super::{ProgramVideoFrame,StreamKey,Status};
use skylive_core::Id;
use skylive_media::MediaTime;

#[derive(Clone,Debug)]
pub struct NdiVideoReceipt {
    pub key:StreamKey,pub connection_epoch:u64,pub revision:u64,pub source:Id,
    pub video_index:u64,pub presentation_at:MediaTime,pub presentation_end:MediaTime,
    pub estimated_sender_media_ns:Option<i128>,
    pub(crate) delay:crate::capture::AsyncDelayReceipt,
    live_output:std::sync::Arc<std::sync::atomic::AtomicBool>,
}
/// Retires cloned receipts when this exact streaming connection owner exits.
pub(super) struct Lifetime(pub std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Lifetime {pub fn new()->Self{Self(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)))}}
impl Drop for Lifetime {fn drop(&mut self){self.0.store(false,std::sync::atomic::Ordering::Release);}}
/// Retire submission candidates in PTS order after the encoder returned an
/// access unit. A missing match clears proof, never fabricates attribution.
pub(super) fn accept(status:&mut Status,pending:&mut std::collections::VecDeque<(i64,Vec<NdiVideoReceipt>)>,pts:i64){
    while pending.front().is_some_and(|(old,_)|*old<pts){pending.pop_front();}
    let mut receipts=if pending.front().is_some_and(|(old,_)|*old==pts){pending.pop_front().map(|(_,v)|v).unwrap_or_default()}else{Vec::new()};
    receipts.retain(NdiVideoReceipt::current);
    status.first_ndi_video_receipts.retain(|old|old.current()&&receipts.iter().any(|value|old.same_scope(value)));
    let mut failed=false;
    for value in &receipts {
        if !status.first_ndi_video_receipts.iter().any(|old|old.same_scope(value)) {
            if status.first_ndi_video_receipts.try_reserve(1).is_err(){failed=true;break;}
            status.first_ndi_video_receipts.push(value.clone());
        }
    }
    if failed {status.first_ndi_video_receipts.clear();receipts.clear();}
    status.ndi_video_receipts=receipts;
}
impl NdiVideoReceipt {
    pub fn current(&self)->bool {self.live_output.load(std::sync::atomic::Ordering::Acquire)&&self.delay.current()}
    pub(crate) fn connection_current(&self)->bool {self.live_output.load(std::sync::atomic::Ordering::Acquire)}
    pub fn sequence(&self)->u64 {self.delay.sequence}
    pub fn sender_timestamp(&self)->Option<(crate::ndi::NdiVideoClock,i128)> {
        Some((self.delay.ndi_native_clock?,self.delay.timestamp_ns))
    }
    pub(crate) fn same_scope(&self,other:&Self)->bool {
        self.key==other.key && self.connection_epoch==other.connection_epoch && self.revision==other.revision
            && self.source==other.source && self.delay.owner==other.delay.owner && self.delay.stream==other.delay.stream
            && self.delay.binding_generation==other.delay.binding_generation && self.delay.clock_epoch==other.delay.clock_epoch
            && self.delay.ndi_native_clock==other.delay.ndi_native_clock && self.delay.filters==other.delay.filters
            && self.delay.history_generation==other.delay.history_generation
            && std::sync::Arc::ptr_eq(&self.delay.history_live,&other.delay.history_live)
    }
}
pub(super) fn candidates(frame:&ProgramVideoFrame,trace:&[Id],key:StreamKey,epoch:u64,index:u64,
    at:MediaTime,end:MediaTime,live_output:&std::sync::Arc<std::sync::atomic::AtomicBool>)->Vec<NdiVideoReceipt>{
    if key.session!=frame.session || frame.capture.session()!=frame.session || trace.len()>skylive_core::MAX_SOURCES{return Vec::new();}
    let mut result=Vec::new();if result.try_reserve(trace.len()).is_err(){return result;}
    let providers=crate::video_filters::FilterProviders::from_snapshot(&frame.model);
    for view in frame.output.scenes(){
        let Ok(sources)=view.visible_sources(&frame.model) else{return Vec::new();};
        for source in sources {
            if !trace.contains(&source.id()) || result.iter().any(|old:&NdiVideoReceipt|old.source==source.id()){continue;}
            let Some(sample)=frame.capture.frame(source.id()) else{continue;};
            if !crate::video_filters::ndi_recording_delay_applied(source,frame.session,sample,&sample.image,providers)
                || !matches!(source.kind(),skylive_core::SourceKind::NdiSource {..}) {continue;}
            let Some(delay)=sample.async_delay.as_ref().filter(|delay|delay.current()) else{continue;};
            result.push(NdiVideoReceipt {key,connection_epoch:epoch,revision:frame.model.revision(),source:source.id(),
                video_index:index,presentation_at:at,presentation_end:end,estimated_sender_media_ns:delay.ndi_media_time_estimate_ns,delay:delay.clone(),live_output:live_output.clone()});
        }
    }
    result
}
