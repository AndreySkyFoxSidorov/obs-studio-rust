//! Metadata-only confirmation and atomic publication with actual owner gates.
use super::*;
use crate::views::source_output_resize::Draft;
impl StudioApp {
    fn source_resize_idle(&self)->bool{!self.recording.active()&&!self.streaming.active()&&!self.replay.active()&&!self.virtual_camera.owned()&&!self.ndi_output.owner.any_active()}
    pub(super) fn open_source_output_resize(&mut self,session:u64,revision:u64,scene:Id,item:Id,context:&egui::Context){
        if self.dialog.is_some()||self.io.busy()||self.documents.modal_open(){self.message(true,"Finish the current dialog or file operation before resizing output.");return;}
        if !self.source_resize_idle(){self.message(true,"Stop recording, streaming, Replay Buffer, virtual camera and NDI outputs before resizing output.");return;}
        if self.session.identity()!=session||self.session.snapshot().revision()!=revision||self.selected!=Some(item)||self.session.snapshot().root_scene_for_container(scene)!=Some(self.session.preview()){self.message(true,"The source selection changed. Reopen Resize output.");return;}
        let capture=self.source_capture_snapshot();
        match skylive_host::source_output_resize::Plan::prepare(&self.session,&capture,scene,item).map_err(|error|error.to_string()).and_then(Draft::new){
            Ok(draft)=>{self.dialog=Some(Dialog::SourceOutputResize(draft));context.request_repaint();},Err(error)=>self.message(true,error),
        }
    }
    pub(super) fn install_source_output_resize(&self,context:&egui::Context){
        let model=self.session.snapshot();let target=self.selected.and_then(|item|model.item_container(item).map(|(scene,_)|(scene,item)));
        let reason=if !self.source_resize_idle(){Some("Stop recording, streaming, Replay Buffer, virtual camera and NDI outputs before resizing output.".to_owned())}
        else if self.io.busy()||self.documents.modal_open(){Some("Finish the current dialog or file operation before resizing output.".to_owned())}
        else{target.and_then(|(scene,item)|skylive_host::source_output_resize::Plan::prepare(&self.session,&self.source_capture_snapshot(),scene,item).err().map(|error|error.to_string()))};
        views::source_output_resize::install(context,self.session.identity(),model.revision(),target,reason);
    }
    pub(super) fn refresh_source_output_resize(&mut self){
        let Some(Dialog::SourceOutputResize(draft))=&self.dialog else{return;};
        let reason=if let Some(reason)=&draft.rejected{Some(reason.clone())}
        else if !self.source_resize_idle(){Some("Stop recording, streaming, Replay Buffer, virtual camera and NDI outputs before resizing output.".to_owned())}
        else if self.io.busy()||self.documents.modal_open(){Some("Finish the current dialog or file operation before resizing output.".to_owned())}
        else if self.selected!=Some(draft.plan().item())||self.session.snapshot().root_scene_for_container(draft.plan().scene())!=Some(self.session.preview()){Some("The source selection changed. Reopen Resize output.".to_owned())}
        else{draft.plan().validate(&self.session,&self.source_capture_snapshot()).err().map(|error|error.to_string())};
        if let Some(Dialog::SourceOutputResize(draft))=&mut self.dialog{draft.ready=reason.is_none();draft.reason=reason;}
    }
    pub(super) fn apply_source_output_resize(&mut self,token:u64,context:&egui::Context){
        self.refresh_source_output_resize();
        let Some(Dialog::SourceOutputResize(draft))=&self.dialog else{return;};
        if draft.token()!=token||!draft.ready{self.message(true,"Resize output confirmation is obsolete. Cancel and reopen it.");return;}
        let expected=draft.plan().revision();let command=draft.plan().command(&self.session,&self.source_capture_snapshot());
        match command{Ok(command)=>{if self.publish_command_at(Some(expected),command,true){self.message(false,"Canvas and output were resized to the selected source.");}else{let reason=self.status.clone();if let Some(Dialog::SourceOutputResize(draft))=&mut self.dialog{draft.ready=false;draft.reason=Some(reason.clone());draft.rejected=Some(reason);}}},Err(error)=>self.message(true,error.to_string())}
        context.request_repaint();
    }
}
