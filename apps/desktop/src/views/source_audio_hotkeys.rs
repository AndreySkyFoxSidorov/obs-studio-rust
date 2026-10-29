//! Source-bound mute shortcut draft; no OS input or audio processing.
use eframe::egui;
use skylive_core::{Id,SourceKind,StudioSnapshot,SourceAudioAction as Operation,SourceAudioBinding,SourceAudioHotkeys};
use std::sync::atomic::{AtomicU64,Ordering};
static NEXT:AtomicU64=AtomicU64::new(1);
pub struct Draft{
    pub session:u64,pub revision:u64,pub source:Id,pub token:u64,pub name:String,
    baseline:SourceAudioHotkeys,bindings:Vec<SourceAudioBinding>,
    baseline_push:skylive_core::PushAudioSettings,mute_enabled:bool,mute_delay_ms:u32,talk_enabled:bool,talk_delay_ms:u32,
    listening:Option<(Operation,usize)>,pub capture_ready:bool,pub available:bool,
    pub current:bool,pub status:String,pub detail:Option<String>,error:Option<String>,
}
impl Draft{
    pub fn capture(session:u64,model:&StudioSnapshot,source:Id)->Result<Self,String>{
        let value=model.source(source).filter(|value|eligible(value.kind())).ok_or("Choose an audio-capable source for mute shortcuts.")?;
        let token=NEXT.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|value|value.checked_add(1)).map_err(|_|"Source shortcut editor identity exhausted.")?;
        let push=value.audio_properties().push();
        Ok(Self{session,revision:model.revision(),source,token,name:value.name().to_owned(),baseline:value.audio_hotkeys().clone(),bindings:value.audio_hotkeys().bindings().to_vec(),baseline_push:push,mute_enabled:push.mute_enabled(),mute_delay_ms:push.mute_delay_ms(),talk_enabled:push.talk_enabled(),talk_delay_ms:push.talk_delay_ms(),listening:None,capture_ready:false,available:false,current:true,status:"Waiting for global shortcut suspension…".into(),detail:None,error:None})
    }
    pub fn fresh(&self,session:u64,model:&StudioSnapshot)->bool{
        self.session==session&&self.revision==model.revision()&&self.token!=0
            &&model.source(self.source).is_some_and(|source|source.audio_hotkeys()==&self.baseline&&source.audio_properties().push()==self.baseline_push&&eligible(source.kind()))
    }
    pub fn is_listening(&self)->bool{self.listening.is_some()}
    pub fn prepared(&self,model:&StudioSnapshot)->Result<SourceAudioHotkeys,String>{
        if !self.current||!self.fresh(self.session,model){return Err("This source shortcut draft is obsolete. Cancel and reopen it.".into());}
        if self.is_listening(){return Err("Finish or cancel the shortcut capture before applying.".into());}
        let bindings=SourceAudioHotkeys::new(self.bindings.clone()).map_err(|error|error.to_string())?;
        model.check_source_audio_hotkeys(self.source,&bindings).map_err(|error|error.to_string())?;Ok(bindings)
    }
    pub fn prepared_push(&self)->Result<skylive_core::PushAudioSettings,String>{skylive_core::PushAudioSettings::new(self.mute_enabled,self.mute_delay_ms,self.talk_enabled,self.talk_delay_ms).map_err(|error|error.to_string())}
    pub fn intent(&self,model:&StudioSnapshot)->Result<super::Intent,String>{Ok(super::Intent::ApplySourceAudioHotkeys{session:self.session,revision:self.revision,source:self.source,token:self.token,bindings:self.prepared(model)?,push:self.prepared_push()?})}
    fn assign(&mut self,operation:Operation,slot:usize,chord:skylive_core::HotkeyChord)->Result<(),String>{
        let mut candidate=self.bindings.clone();
        if let Some(index)=candidate.iter().enumerate().filter(|(_,binding)|binding.operation==operation).nth(slot).map(|(index,_)|index){candidate.remove(index);}
        candidate.push(SourceAudioBinding{operation,chord});
        let candidate=SourceAudioHotkeys::new(candidate).map_err(|error|error.to_string())?;self.bindings=candidate.bindings().to_vec();Ok(())
    }
}
pub fn eligible(kind:&SourceKind)->bool{kind.supports_audio_settings()}
pub fn open_button(ui:&mut egui::Ui,session:u64,model:&StudioSnapshot,source:Id,intents:&mut Vec<super::Intent>){
    if !model.source(source).is_some_and(|source|eligible(source.kind())){return;}
    if ui.button(crate::i18n::tr(ui,"Audio Hotkeys…")).on_hover_text(crate::i18n::tr(ui,"Configure mute and push-to-talk shortcuts for this audio source.")).clicked(){
        match Draft::capture(session,model,source){Ok(draft)=>intents.push(super::Intent::OpenDialog(super::dialogs::Dialog::SourceAudioHotkeys(draft))),Err(error)=>intents.push(super::Intent::DialogError(error))};ui.close_menu();
    }
}
pub fn settings_sources(ui:&mut egui::Ui,model:&StudioSnapshot,scope:Option<(u64,u64)>,enabled:bool,intents:&mut Vec<super::Intent>){
    egui::CollapsingHeader::new(crate::i18n::tr(ui,"Source Audio Shortcuts")).show(ui,|ui|{
        ui.label(crate::i18n::tr(ui,"Apply or cancel changes on this settings page before opening a source editor."));
        ui.add_enabled_ui(enabled&&scope.is_some_and(|(_,revision)|revision==model.revision()),|ui|{
            egui::ScrollArea::vertical().id_salt("source-audio-shortcut-targets").max_height(180.0).show(ui,|ui|{
                let mut found=false;
                for source in model.sources().iter().filter(|source|eligible(source.kind())){found=true;ui.push_id(source.id(),|ui|{ui.horizontal_wrapped(|ui|{ui.label(source.name()).on_hover_text(source.name());open_button(ui,scope.map_or(0,|scope|scope.0),model,source.id(),intents);});});}
                if !found{ui.label(crate::i18n::tr(ui,"Add an audio-capable source to configure mute shortcuts."));}
            });
        });
    }).header_response.on_hover_text(crate::i18n::tr(ui,"Configure mute and push-to-talk shortcuts for this audio source."));
}
pub fn editor(ui:&mut egui::Ui,model:&StudioSnapshot,draft:&mut Draft){
    ui.label(crate::i18n::tf(ui,"Source: {name}",&[("{name}",draft.name.clone())]));
    ui.label(crate::i18n::tr(ui,"These Windows global shortcuts always address this source. They also work while another app has focus."));
    ui.label(crate::i18n::tr(ui,&draft.status));if let Some(detail)=&draft.detail{ui.add(egui::Label::new(crate::i18n::tr(ui,detail)).wrap());}
    if !draft.available{ui.label(crate::i18n::tr(ui,"Global shortcuts are unavailable on this platform. Saved assignments are retained."));}
    let fresh=draft.current&&draft.fresh(draft.session,model);
    if !fresh{draft.listening=None;}
    if !fresh{ui.colored_label(ui.visuals().warn_fg_color,crate::i18n::tr(ui,"This source shortcut draft is obsolete. Cancel and reopen it."));}
    if let Some((operation,slot))=draft.listening{
        let pressed=ui.input_mut(|input|{let index=input.events.iter().position(|event|matches!(event,egui::Event::Key{pressed:true,repeat:false,key,..}if *key==egui::Key::Escape||draft.capture_ready))?;match input.events.remove(index){egui::Event::Key{key,modifiers,..}=>Some((key,modifiers)),_=>None}});
        if let Some((key,modifiers))=pressed{if key==egui::Key::Escape{draft.listening=None;draft.error=None;}else{match crate::shortcuts::chord(key,modifiers).and_then(|chord|draft.assign(operation,slot,chord)){Ok(())=>{draft.listening=None;draft.error=None;},Err(error)=>draft.error=Some(error)}}}
    }
    if fresh && draft.available && draft.capture_ready {
        if let Some((operation,slot))=draft.listening {
            if let Some(chord)=super::hotkeys::keypad_picker(ui) {
                match draft.assign(operation,slot,chord){Ok(())=>{draft.listening=None;draft.error=None;},Err(error)=>draft.error=Some(error)}
            }
        }
    }
    if let Some(error)=&draft.error{ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,error));}
    if !draft.is_listening(){if let Err(error)=draft.prepared(model){ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(ui,error)).color(ui.visuals().warn_fg_color)).wrap());}}
    let operations:&[Operation]=&Operation::ALL;
    for &operation in operations{ui.push_id(operation,|ui|{
        if ui.available_width()>=580.0{
            ui.horizontal(|ui|{
                let label=ui.add_sized([150.0,28.0],egui::Label::new(crate::i18n::tr(ui,operation.label())).truncate()).on_hover_text(crate::i18n::tr(ui,operation.label()));
                for slot in 0..2{binding_slot(ui,draft,operation,slot,fresh,label.id);}
            });
        }else{
            ui.group(|ui|{
                let label=ui.label(crate::i18n::tr(ui,operation.label())).on_hover_text(crate::i18n::tr(ui,operation.label()));
                for slot in 0..2{ui.horizontal(|ui|binding_slot(ui,draft,operation,slot,fresh,label.id));}
            });
        }
    });}
    ui.checkbox(&mut draft.mute_enabled,crate::i18n::tr_key_or(ui,"Basic.Settings.Audio.EnablePushToMute","Enable Push-to-mute"))
        .on_hover_text(crate::i18n::tr(ui,"Silence this source while a Push-to-mute shortcut is held."));
    ui.horizontal(|ui|{let label=ui.label(crate::i18n::tr_key_or(ui,"Basic.Settings.Audio.PushToMuteDelay","Push-to-mute delay"));ui.add(egui::DragValue::new(&mut draft.mute_delay_ms).range(0..=i32::MAX as u32).suffix(" ms")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Keep the source muted for this long after the last Push-to-mute shortcut is released."));});
    ui.checkbox(&mut draft.talk_enabled,crate::i18n::tr_key_or(ui,"Basic.Settings.Audio.EnablePushToTalk","Enable Push-to-talk"))
        .on_hover_text(crate::i18n::tr(ui,"Silence this source unless a Push-to-talk shortcut is held. Without an assigned shortcut it stays silent."));
    ui.horizontal(|ui|{let label=ui.label(crate::i18n::tr_key_or(ui,"Basic.Settings.Audio.PushToTalkDelay","Push-to-talk delay"));ui.add(egui::DragValue::new(&mut draft.talk_delay_ms).range(0..=i32::MAX as u32).suffix(" ms")).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Keep the source open for this long after the last Push-to-talk shortcut is released."));});
    ui.label(crate::i18n::tr(ui,"Toggle Mute assigns the same shortcut to both original Mute and Unmute callbacks."));
    ui.label(crate::i18n::tr_key_or(ui,"SkyLive.Hotkeys.WindowsLimits","Ctrl, Alt and Shift are supported. Windows-key combinations and punctuation are unavailable."));
    ui.label(crate::i18n::tr(ui,"Apply saves these source mute bindings; Cancel leaves them unchanged. These controls do not mute the Windows device."));
}

fn binding_slot(ui:&mut egui::Ui,draft:&mut Draft,operation:Operation,slot:usize,fresh:bool,label:egui::Id){
    ui.push_id(slot,|ui|{
        let binding=draft.bindings.iter().filter(|binding|binding.operation==operation).nth(slot);let assigned=binding.is_some();
        let caption=if draft.listening==Some((operation,slot)){crate::i18n::tr(ui,"Press a shortcut…")}else{binding.map(|binding|super::hotkeys::global_label(ui,&binding.chord)).unwrap_or_else(||crate::i18n::tr(ui,"Unassigned"))};
        let response=ui.add_enabled_ui(fresh&&draft.available&&draft.capture_ready,|ui|ui.add_sized([140.0,28.0],egui::Button::new(&caption).truncate().selected(draft.listening==Some((operation,slot))))).inner.labelled_by(label)
            .on_hover_text(crate::i18n::tr(ui,"Assign a shortcut to this action. Escape cancels capture.")).on_hover_text(&caption)
            .on_disabled_hover_text(crate::i18n::tr(ui,"Wait until configured shortcut input is suspended before capturing a shortcut."));
        if response.clicked(){draft.listening=Some((operation,slot));draft.error=None;response.request_focus();}
        if ui.add_enabled_ui(fresh&&assigned,|ui|ui.add_sized([56.0,28.0],egui::Button::new(crate::i18n::tr(ui,"Clear")).truncate())).inner.labelled_by(label)
            .on_hover_text(crate::i18n::tr(ui,"Remove this shortcut assignment.")).on_disabled_hover_text(crate::i18n::tr(ui,"Remove this shortcut assignment.")).clicked(){
            if let Some(index)=draft.bindings.iter().enumerate().filter(|(_,binding)|binding.operation==operation).nth(slot).map(|(index,_)|index){draft.bindings.remove(index);}
            draft.listening=None;draft.error=None;
        }
    });
}
