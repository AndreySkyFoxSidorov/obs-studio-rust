//! UI adapter for owned NDI receive work; completed preparation is fenced before
//! the canonical Session command. Cancel cannot create a source.
use super::*;
use crate::views::ndi_source::NdiPrepare;
use skylive_host::ndi::{NdiEvent,NdiRequestKey};
pub(super) struct PendingNdi {pub request:NdiPrepare,pub key:NdiRequestKey}
impl StudioApp {
    pub(super) fn source_capture_snapshot(&self)->Arc<CaptureSnapshot>{self.browser_union.snapshot(self.session.identity()).or_else(||self.game_union.snapshot(self.session.identity())).or_else(||self.ndi_union.snapshot(self.session.identity())).unwrap_or_else(||self.capture_union.snapshot(self.session.identity()))}
    pub(super) fn discover_ndi(&mut self,token:u64,session:u64,revision:u64,local:bool,context:&egui::Context){
        let current=matches!(&self.dialog,Some(Dialog::NdiSource(d))if d.token==token&&d.session==session&&d.revision==revision&&!d.loading);
        if !current||session!=self.session.identity()||revision!=self.session.snapshot().revision(){self.message(true,"The NDI source draft changed. Reopen its properties.");return;}
        match self.ndi.discover(&self.session,local){Ok(key)=>{self.pending_ndi_discovery=Some((token,key));if let Some(Dialog::NdiSource(d))=&mut self.dialog{d.loading=true;d.error=None;}},Err(e)=>self.ndi_dialog_error(e.to_string())}context.request_repaint();
    }
    pub(super) fn prepare_ndi(&mut self,request:NdiPrepare,context:&egui::Context){
        if request.session!=self.session.identity()||request.revision!=self.session.snapshot().revision()||!matches!(&self.dialog,Some(Dialog::NdiSource(d))if d.matches(&request)&&!d.loading){self.message(true,"The NDI source draft changed. Reopen its properties.");return;}
        // An existing source already owns a real, validated video basis. If its
        // sender has not changed, bandwidth/audio edits can be authored while
        // that sender is offline; runtime reconnection remains asynchronous.
        if let views::ndi_source::NdiEdit::Rebind {source}=request.edit {
            let snapshot=self.session.snapshot();
            let Some(source)=snapshot.source(source) else {self.message(true,"The NDI source draft changed. Reopen its properties.");return;};
            let skylive_core::SourceKind::NdiSource {ndi:current}=source.kind() else {self.message(true,"The NDI source draft changed. Reopen its properties.");return;};
            if current.connection().sender==request.connection.sender {
                let updated=skylive_core::NdiSource::new(request.connection.clone(),current.width(),current.height());
                match updated {
                    Ok(ndi)=>self.apply_at(Some(request.revision),request.command(ndi)),
                    Err(error)=>self.ndi_dialog_error(error.to_string()),
                }
                context.request_repaint();return;
            }
        }
        match self.ndi.prepare(&self.session,request.connection.clone()){Ok(key)=>{self.pending_ndi=Some(PendingNdi {request,key});if let Some(Dialog::NdiSource(d))=&mut self.dialog{d.loading=true;d.error=None;}},Err(e)=>self.ndi_dialog_error(e.to_string())}context.request_repaint();
    }
    fn ndi_dialog_error(&mut self,error:String){if let Some(Dialog::NdiSource(d))=&mut self.dialog{d.loading=false;d.error=Some(error);}else{self.message(true,crate::status::Status::formatted("NDI receive error: {error}",&[("{error}",error)]));}}
    pub(super) fn update_ndi(&mut self,context:&egui::Context,active:&[FrameScene],video_active:&[FrameScene],now:MediaTime,base:Arc<CaptureSnapshot>)->Arc<CaptureSnapshot>{
        self.ndi.set_camera_activation_policy(self.camera_activation.snapshot());
        if let Err(e)=self.ndi.synchronize_video(&self.session,video_active,active,now,self.audio.program_key()){self.message(true,crate::status::Status::formatted("NDI receive error: {error}",&[("{error}",e.to_string())]));}
        for _ in 0..4 {match self.ndi.poll(&self.session){
            Ok(Some(NdiEvent::Discovered {key,result}))=>{
                let pending=self.pending_ndi_discovery.take();
                if let Some((token,expected))=pending {if key==expected {if let Some(Dialog::NdiSource(d))=&mut self.dialog {if d.token==token&&d.session==key.session&&d.revision==key.revision {d.loading=false;match result{Ok(senders)=>d.senders=senders,Err(e)=>d.error=Some(e.to_string())}}}}}
            },
            Ok(Some(NdiEvent::Prepared {key,result}))=>{
                let Some(pending)=self.pending_ndi.take() else{continue;};
                if key!=pending.key||!matches!(&self.dialog,Some(Dialog::NdiSource(d))if d.matches(&pending.request)){continue;}
                match result {Ok(ndi)=>{if let Some(Dialog::NdiSource(d))=&mut self.dialog{d.loading=false;}self.apply_at(Some(key.revision),pending.request.command(ndi));context.request_repaint();},Err(e)=>self.ndi_dialog_error(e.to_string())}
            },
            Ok(None)=>break,
            Err(e)=>{self.pending_ndi=None;self.pending_ndi_discovery=None;self.ndi_dialog_error(e.to_string());break;}
        }}
        if let Some(Dialog::NdiSource(draft))=&mut self.dialog {draft.observed=if let views::ndi_source::NdiEdit::Rebind {source}=draft.edit {self.ndi.snapshot().frame(source).map(|frame|frame.metadata.clone())}else{None};}
        if self.ndi.is_busy()||self.ndi.has_active_sources(){context.request_repaint_after(std::time::Duration::from_millis(16));}
        match self.ndi_union.merge(&self.session,base.clone(),&self.ndi){Ok(frame)=>frame,Err(e)=>{self.message(true,crate::status::Status::formatted("NDI receive error: {error}",&[("{error}",e.to_string())]));base}}
    }
}
