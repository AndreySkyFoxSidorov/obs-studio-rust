//! Canonical mute control admission; callback acceptance is not device readiness.
use super::*;
use skylive_core::{SourceAudioAction,SourceAudioHotkeys,HotkeyAction};
impl StudioApp {
    pub(super) fn apply_source_audio_hotkeys(&mut self,session:u64,revision:u64,source:Id,token:u64,bindings:SourceAudioHotkeys,push:skylive_core::PushAudioSettings,context:&egui::Context){
        let current=self.dialog.as_ref().is_some_and(|dialog|matches!(dialog,Dialog::SourceAudioHotkeys(draft)if draft.token==token&&draft.source==source&&draft.fresh(session,&self.session.snapshot())&&draft.prepared(&self.session.snapshot()).is_ok_and(|prepared|prepared==bindings)&&draft.prepared_push().is_ok_and(|prepared|prepared==push)));
        if !current||session!=self.session.identity()||revision!=self.session.snapshot().revision(){self.message(true,"This source shortcut draft is obsolete. Cancel and reopen it.");return;}
        self.publish_command_at(Some(revision),Command::ConfigureSourceAudioHotkeys{source,bindings,push},true);context.request_repaint();
    }
    pub(super) fn source_audio_hotkey(&mut self,source:Id,operation:SourceAudioAction,callback:skylive_host::global_hotkeys::Callback,context:&egui::Context){
        let model=self.session.snapshot();
        let (resolved,command)=match model.source_audio_hotkey_command(source,operation){Ok(Some(value))=>value,Ok(None)=>{callback.reject();return;},Err(error)=>{callback.reject();self.message(true,error.to_string());return;}};
        let action=HotkeyAction::SourceAudio{source,operation:resolved};
        // The native owner already checked session/source-definition/config/
        // press identity. Its live atomic receipt is the final Core commit gate.
        if self.publish_command_with_gate(Some(model.revision()),command,false,||callback.accept_action(action)){context.request_repaint();}else{callback.reject();}
    }
}
