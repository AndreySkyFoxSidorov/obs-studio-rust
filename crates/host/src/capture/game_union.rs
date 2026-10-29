//! Metadata-only union; GameOwner keeps final pixel drops on its worker.
use super::*;
#[derive(Default)]
pub struct GameCaptureUnion {identity:Option<(u64,u64,u64,u64,u64,u64)>,epoch:u64,generation:u64,latest:Option<Arc<CaptureSnapshot>>}
impl GameCaptureUnion {
    pub fn snapshot(&self,session:u64)->Option<Arc<CaptureSnapshot>>{self.latest.as_ref().filter(|v|v.session()==session).cloned()}
    pub fn clear(&mut self){self.identity=None;self.latest=None;}
    pub fn merge(&mut self,session:&Session,base:Arc<CaptureSnapshot>,owner:&crate::game_capture::GameOwner)->Result<Arc<CaptureSnapshot>,CaptureError>{
        if session.is_closed(){return Err(CaptureError::Closed);}if base.session()!=session.identity(){return Err(CaptureError::ForeignSession);}
        let input=owner.snapshot();if input.session()!=session.identity(){return Ok(base);}
        let identity=(session.identity(),base.epoch(),base.generation(),input.instance(),input.epoch(),input.generation());
        if self.identity==Some(identity){if let Some(latest)=&self.latest{return Ok(latest.clone());}}
        let changed=self.identity.is_none_or(|old|(old.0,old.1,old.3,old.4)!=(identity.0,identity.1,identity.3,identity.4));
        let epoch=if changed{self.epoch.checked_add(1).ok_or(CaptureError::IdentityExhausted)?}else{self.epoch};
        let generation=self.generation.checked_add(1).ok_or(CaptureError::IdentityExhausted)?;
        if input.frames().count()>16||input.problems().len()>16{return Err(CaptureError::BudgetExceeded("game union references16"));}
        let mut frames=base.frames.clone();let mut problems=base.problems.clone();let media_empty=base.media_empty.clone();
        if frames.len()+media_empty.len()+input.frames().count()>MAX_CAPTURE_REFERENCES{return Err(CaptureError::BudgetExceeded("combined game capture references"));}
        let mut seen=std::collections::HashSet::new();let mut bytes=0usize;let mut metadata=0usize;
        for frame in frames.values(){if seen.insert(frame.image.pixels().as_ptr() as usize){bytes=bytes.checked_add(frame.image.byte_len()).ok_or(CaptureError::BudgetExceeded("game union bytes"))?;}if let Some(raw)=&frame.draft_raw{if seen.insert(raw.image.pixels().as_ptr() as usize){bytes=bytes.checked_add(raw.image.byte_len()).ok_or(CaptureError::BudgetExceeded("draft camera frame bytes"))?;}}}
        for (source,frame) in input.frames(){
            if frames.contains_key(&source)||media_empty.contains_key(&source){return Err(CaptureError::InvalidFrame("game source has multiple pixel owners"));}
            metadata=metadata.checked_add(frame.binding.target().executable.len()+frame.binding.target().title.len()+frame.binding.target().class.len()+128).ok_or(CaptureError::BudgetExceeded("game union metadata"))?;
            if seen.insert(frame.image.pixels().as_ptr() as usize){bytes=bytes.checked_add(frame.image.byte_len()).ok_or(CaptureError::BudgetExceeded("game union pixel bytes"))?;}
            if bytes>CaptureLimits::default().max_total_frame_bytes{return Err(CaptureError::BudgetExceeded("combined native/media/game pixels"));}
            frames.insert(source,CapturedFrame {native_origin:Some(NativeFrameOrigin::Game{owner:input.instance(),generation:frame.generation}),binding:CaptureBinding::Game(frame.binding.clone()),sequence:frame.sequence,requested_at:frame.requested_at,native_timestamp_ns:None,native_clock_epoch:None,device_qpc_ns:None,directshow_clock:None,async_delay:None,draft_raw:None,image:frame.image.clone().into(),display:None,window:None});
        }
        for issue in input.problems(){let detail=issue.error.to_string();metadata=metadata.checked_add(detail.len()+64).ok_or(CaptureError::BudgetExceeded("game union diagnostics"))?;
            problems.push(CaptureProblem {source:issue.source,kind:if matches!(issue.error,crate::game_capture::GameError::Limit(_)){crate::resource_issue::ResourceIssueKind::Limit}else{crate::resource_issue::ResourceIssueKind::Unavailable},code:issue.error.code(),detail});
        }
        if metadata>256*1024||problems.len()>MAX_CAPTURE_REFERENCES{return Err(CaptureError::BudgetExceeded("game union metadata256KiB"));}
        let draft_frames=admit_draft_frames(&base.draft_frames,&frames,&mut seen,&mut bytes);
        let latest=Arc::new(CaptureSnapshot {session:session.identity(),epoch,generation,frames,draft_frames,media_empty,problems});
        self.identity=Some(identity);self.epoch=epoch;self.generation=generation;self.latest=Some(latest.clone());Ok(latest)
    }
}
