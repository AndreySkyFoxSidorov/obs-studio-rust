//! Application-owned timer service. Rendering only reports real captures;
//! canonical expiry uses the normal guarded command publication path.
use super::*;
use skylive_host::freeze_timing_book::{Due,Group,Instance};

impl StudioApp {
    /// Returns true after a canonical mutation so this frame can be restarted
    /// with a fresh transition/model instead of using its old snapshot.
    pub(super) fn service_freeze_timing(&mut self,ctx:&egui::Context,views:&[FrameScene],now:MediaTime)->bool {
        if let Err(error)=skylive_host::freeze_timing_collect::observe(&self._render_gpu,&self.session,views,now.0){
            self.message(true,error);ctx.request_repaint_after(std::time::Duration::from_millis(100));return true;
        }
        let draft_current=matches!(&self.dialog,Some(Dialog::VideoFilters(draft)) if draft.session==self.session.identity()&&draft.revision==self.session.snapshot().revision()&&draft.preview.freeze_source().is_some());
        if !draft_current{if let Err(error)=self._render_gpu.freeze_observe(Group::Draft,&[],now.0){self.message(true,error);ctx.request_repaint_after(std::time::Duration::from_millis(100));return true;}}
        let intents=match self._render_gpu.freeze_due(now.0){Ok(value)=>value,Err(error)=>{self.message(true,error);ctx.request_repaint_after(std::time::Duration::from_millis(100));return true;}};
        let mut refresh=false;
        for (index,intent) in intents.values.iter().copied().enumerate(){
            if intent.target.session!=self.session.identity(){self._render_gpu.freeze_reject_disable(intent);continue;}
            match (intent.target.instance,intent.due){
                (_,Due::Refresh{..}|Due::RetryCapture{..}|Due::RetryRender{..})=>{refresh=true;},
                (Instance::Canonical{filter},Due::Disable{..})=>{
                    let revision=self.session.snapshot().revision();
                    let valid=revision.checked_add(1)==Some(intent.definition_generation)&&self.session.snapshot().source(intent.target.source).is_some_and(|source|source.video_filters().iter().any(|value|value.id()==Some(filter)&&value.enabled&&matches!(value.effect,skylive_core::VideoEffect::Freeze{..})));
                    if !valid{self._render_gpu.freeze_reject_disable(intent);continue;}
                    let Some(mut filters)=self.session.snapshot().source(intent.target.source).map(|source|source.video_filters().to_vec()) else {self._render_gpu.freeze_reject_disable(intent);continue;};
                    let Some(target)=filters.iter_mut().find(|value|value.id()==Some(filter)&&value.enabled&&matches!(value.effect,skylive_core::VideoEffect::Freeze{..})) else {self._render_gpu.freeze_reject_disable(intent);continue;};
                    target.enabled=false;
                    let owner=self._render_gpu.clone();
                    let committed=self.publish_command_projected_with_gate(Some(revision),Command::SetSourceVideoFilters{source:intent.target.source,filters},false,move||owner.freeze_accept_disable(intent),|session,_|session.snapshot().revision());
                    if committed.is_some(){for remaining in &intents.values[index+1..]{self._render_gpu.freeze_reject_disable(*remaining);}ctx.request_repaint();return true;}
                    self._render_gpu.freeze_reject_disable(intent);
                },
                (Instance::Draft{window,row},Due::Disable{..})=>{
                    let valid=matches!(&self.dialog,Some(Dialog::VideoFilters(draft)) if draft.can_expire_freeze(window,row,intent.target.source));
                    if valid&&self._render_gpu.freeze_accept_disable(intent){if let Some(Dialog::VideoFilters(draft))=&mut self.dialog{if draft.expire_freeze(window,row,intent.target.source){draft.preview.refresh_frame();refresh=true;}}}
                    else{self._render_gpu.freeze_reject_disable(intent);}
                },
                (Instance::Private{..}|Instance::DraftPrivate{..},Due::Disable{..})=>{
                    if self._render_gpu.freeze_expire_private(intent,now.0){refresh=true;}
                    else{self._render_gpu.freeze_reject_disable(intent);}
                },
                (_,Due::None)=>{},
            }
        }
        if let Some(generation)=self._render_gpu.freeze_generation(){if generation!=self.freeze_generation{self.freeze_generation=generation;refresh=true;}}
        if let Some(failures)=self._render_gpu.freeze_failures(){if failures!=self.freeze_failures{self.freeze_failures=failures;self.message(true,"Freeze could not update its image. The last complete image remains visible when available; another attempt is scheduled.");}}
        if refresh{
            self.preview.refresh_frame();self.program.refresh_frame();self.projectors.refresh_frames();
            if let Some(Dialog::VideoFilters(draft))=&mut self.dialog{draft.preview.refresh_frame();}
            ctx.request_repaint();
        }
        if let Ok(Some(deadline))=self._render_gpu.freeze_deadline(){ctx.request_repaint_after(std::time::Duration::from_nanos(deadline.saturating_sub(now.0).max(1_000_000)));}
        false
    }
}
