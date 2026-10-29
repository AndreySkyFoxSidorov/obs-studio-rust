//! Source-specific shortcut publication and runtime dispatch through real owners.
use super::*;
use skylive_core::{SourceKind,SourcePlaybackAction as Operation,SourcePlaybackHotkeys};
impl StudioApp{
    pub(super) fn apply_source_playback_hotkeys(&mut self,session:u64,revision:u64,source:Id,token:u64,bindings:SourcePlaybackHotkeys,context:&egui::Context){
        let current=self.dialog.as_ref().is_some_and(|dialog|matches!(dialog,Dialog::SourcePlaybackHotkeys(draft)if draft.token==token&&draft.source==source&&draft.fresh(session,&self.session.snapshot())&&draft.prepared(&self.session.snapshot()).is_ok_and(|prepared|prepared==bindings)));
        if !current||session!=self.session.identity()||revision!=self.session.snapshot().revision(){self.message(true,"This source shortcut draft is obsolete. Cancel and reopen it.");return;}
        self.publish_command_at(Some(revision),Command::SetSourcePlaybackHotkeys{source,bindings},true);context.request_repaint();
    }
    pub(super) fn source_playback_hotkey(&mut self,source:Id,operation:Operation,callback:skylive_host::global_hotkeys::Callback,context:&egui::Context){
        let model=self.session.snapshot();
        let Some(value)=model.source(source)else{callback.reject();return;};
        if !operation.supported(value.kind()){callback.reject();return;}
        match value.kind(){
            SourceKind::ImageSlideShow{..}=>{
                use skylive_host::image_slideshow::transport::Target;
                let target=Target{session:self.session.identity(),revision:model.revision(),source};
                match self.playback.control_slideshow_hotkey_with_accept(&self.session,target,operation,self.frame_now,||callback.accept()){
                    Ok(Some(receipt))=>{if receipt.applied{context.request_repaint();}},Ok(None)=>{callback.reject();},Err(error)=>{callback.reject();self.message(true,error.to_string());},
                }
            },
            SourceKind::MediaSource{media}=>{
                let _=media; // Actual playback state is read only inside MediaOwner.
                if self.global_media_control_busy()||self.media.is_busy(){callback.reject();self.message(true,"Wait for the active media operation.");return;}
                let target=match self.media.control_target(&self.session,source){Ok(target)=>target,Err(error)=>{callback.reject();self.message(true,error.to_string());return;}};
                match self.media.source_hotkey_control(&self.session,target,operation,callback.clone(),self.frame_now){
                    Ok(key)=>{self.global_media_control_started(key,source);context.request_repaint();},Err(error)=>{callback.reject();self.message(true,error.to_string());},
                }
            },_=>{callback.reject();},
        }
    }
}
