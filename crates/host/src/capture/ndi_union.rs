//! Metadata-only union: NDI worker owns and retires all published pixels.
use super::*;
#[derive(Default)]
pub struct NdiCaptureUnion {identity:Option<(u64,u64,u64,u64,u64,u64)>,epoch:u64,generation:u64,latest:Option<Arc<CaptureSnapshot>>}
impl NdiCaptureUnion {
    pub fn snapshot(&self,session:u64)->Option<Arc<CaptureSnapshot>>{self.latest.as_ref().filter(|s|s.session()==session).cloned()}
    pub fn clear(&mut self){self.identity=None;self.latest=None;}
    pub fn merge(&mut self,session:&Session,base:Arc<CaptureSnapshot>,owner:&crate::ndi::NdiOwner)->Result<Arc<CaptureSnapshot>,CaptureError>{
        if session.is_closed(){return Err(CaptureError::Closed);}if base.session()!=session.identity(){return Err(CaptureError::ForeignSession);}
        let input=owner.snapshot();if input.session()!=session.identity(){return Ok(base);}
        let identity=(session.identity(),base.epoch(),base.generation(),input.instance(),input.epoch(),input.generation());
        if self.identity==Some(identity){if let Some(latest)=&self.latest{return Ok(latest.clone());}}
        let changed=self.identity.is_none_or(|v|(v.0,v.1,v.3,v.4)!=(identity.0,identity.1,identity.3,identity.4));
        let epoch=if changed{self.epoch.checked_add(1).ok_or(CaptureError::IdentityExhausted)?}else{self.epoch};
        let generation=self.generation.checked_add(1).ok_or(CaptureError::IdentityExhausted)?;
        if input.frames().count()>4||input.problems().len()>4{return Err(CaptureError::BudgetExceeded("NDI union references"));}
        let mut frames=base.frames.clone();let mut problems=base.problems.clone();let media_empty=base.media_empty.clone();
        if frames.len()+media_empty.len()+input.frames().count()>MAX_CAPTURE_REFERENCES{return Err(CaptureError::BudgetExceeded("combined NDI frame references"));}
        let mut seen=std::collections::HashSet::new();let mut bytes=0usize;
        for frame in frames.values(){if seen.insert(frame.image.pixels().as_ptr() as usize){bytes=bytes.checked_add(frame.image.byte_len()).ok_or(CaptureError::BudgetExceeded("combined NDI bytes"))?;}if let Some(raw)=&frame.draft_raw{if seen.insert(raw.image.pixels().as_ptr() as usize){bytes=bytes.checked_add(raw.image.byte_len()).ok_or(CaptureError::BudgetExceeded("draft camera frame bytes"))?;}}}
        let mut metadata=0usize;
        for (source,frame) in input.frames(){
            if frames.contains_key(&source)||media_empty.contains_key(&source){return Err(CaptureError::InvalidFrame("NDI source has multiple pixel owners"));}
            metadata=metadata.checked_add(frame.ndi.connection().sender.name.len()+frame.ndi.connection().sender.url.as_ref().map_or(0,String::len)+256).ok_or(CaptureError::BudgetExceeded("NDI union metadata"))?;
            if seen.insert(frame.image.pixels().as_ptr() as usize){bytes=bytes.checked_add(frame.image.byte_len()).ok_or(CaptureError::BudgetExceeded("combined NDI bytes"))?;}
            if bytes>CaptureLimits::default().max_total_frame_bytes{return Err(CaptureError::BudgetExceeded("combined native/media/NDI pixels"));}
            let draft_raw=frame.draft_raw.as_ref().and_then(|raw|{
                let pointer=raw.image.pixels().as_ptr() as usize;
                let extra=if seen.contains(&pointer){0}else{raw.image.byte_len()};
                let next=bytes.checked_add(extra)?;
                if next>CaptureLimits::default().max_total_frame_bytes{return None;}
                bytes=next;seen.insert(pointer);
                Some(Arc::new(CapturedFrame {native_origin:Some(NativeFrameOrigin::Ndi{owner:input.instance(),stream:raw.stream_generation}),binding:CaptureBinding::Ndi(raw.ndi.clone()),sequence:raw.sequence,requested_at:raw.requested_at,native_timestamp_ns:raw.native_timestamp_ns,native_clock_epoch:raw.native_timestamp_ns.map(|_|raw.native_clock_epoch),device_qpc_ns:None,directshow_clock:None,async_delay:None,draft_raw:None,image:raw.image.clone(),display:None,window:None}))
            });
            frames.insert(source,CapturedFrame {native_origin:Some(NativeFrameOrigin::Ndi{owner:input.instance(),stream:frame.stream_generation}),binding:CaptureBinding::Ndi(frame.ndi.clone()),sequence:frame.sequence,requested_at:frame.requested_at,native_timestamp_ns:frame.native_timestamp_ns,native_clock_epoch:frame.native_timestamp_ns.map(|_|frame.native_clock_epoch),device_qpc_ns:None,directshow_clock:None,async_delay:frame.async_delay.clone(),draft_raw,image:frame.image.clone(),display:None,window:None});
        }
        for issue in input.problems(){metadata=metadata.checked_add(issue.detail.len()+64).ok_or(CaptureError::BudgetExceeded("NDI union diagnostics"))?;problems.push(CaptureProblem {source:issue.source,kind:if issue.code=="ndi_limit" {crate::resource_issue::ResourceIssueKind::Limit}else{crate::resource_issue::ResourceIssueKind::Unavailable},code:issue.code,detail:issue.detail.clone()});}
        if metadata>256*1024||problems.len()>MAX_CAPTURE_REFERENCES{return Err(CaptureError::BudgetExceeded("NDI union metadata256KiB"));}
        let mut candidates=base.draft_frames.clone();
        for (source,raw,live_stream) in input.draft_frames() {
            if media_empty.contains_key(&source) {continue;}
            if let Some(current)=frames.get(&source) {
                // A stale delayed receipt may coexist with the current raw
                // sample. Only the same NDI owner and authored binding may
                // contribute the modal alternative; Program is untouched.
                if !matches!(current.native_origin,Some(NativeFrameOrigin::Ndi{owner,..}) if owner==input.instance())
                    || current.binding!=CaptureBinding::Ndi(raw.ndi.clone()) {continue;}
            }
            candidates.insert(source,DraftCapture {
                epoch:input.epoch(),live:owner.delay_epoch_token(),stream:Some((raw.stream_generation,live_stream.clone())),
                frame:CapturedFrame {native_origin:Some(NativeFrameOrigin::Ndi{owner:input.instance(),stream:raw.stream_generation}),binding:CaptureBinding::Ndi(raw.ndi.clone()),sequence:raw.sequence,requested_at:raw.requested_at,native_timestamp_ns:raw.native_timestamp_ns,native_clock_epoch:raw.native_timestamp_ns.map(|_|raw.native_clock_epoch),device_qpc_ns:None,directshow_clock:None,async_delay:None,draft_raw:None,image:raw.image.clone(),display:None,window:None}
            });
        }
        let draft_frames=admit_draft_frames(&candidates,&frames,&mut seen,&mut bytes);
        let latest=Arc::new(CaptureSnapshot {session:session.identity(),epoch,generation,frames,draft_frames,media_empty,problems});
        self.identity=Some(identity);self.epoch=epoch;self.generation=generation;self.latest=Some(latest.clone());Ok(latest)
    }
}
