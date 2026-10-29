//! Atomic local/global shortcut draft; capture requires actual OS pause acknowledgement.
use eframe::egui;
use skylive_core::{HotkeyAction,HotkeyBinding,HotkeyBindings,GlobalHotkeyBindings,UserPreferences,Command,validate_hotkey_scopes};
#[derive(Clone,Copy,PartialEq,Eq)]enum Scope {Local,Global}
pub struct HotkeyDraft {pub source_scope:Option<(u64,u64)>,baseline:HotkeyBindings,baseline_global:GlobalHotkeyBindings,bindings:Vec<HotkeyBinding>,global:Vec<HotkeyBinding>,scope:Scope,listening:Option<(HotkeyAction,usize)>,error:Option<String>,capture_ready:bool,global_available:bool,runtime_status:String,runtime_detail:Option<String>}
impl HotkeyDraft {
 pub fn new(bindings:&HotkeyBindings)->Self {Self {source_scope:None,baseline:bindings.clone(),baseline_global:GlobalHotkeyBindings::default(),bindings:bindings.bindings().to_vec(),global:Vec::new(),scope:Scope::Local,listening:None,error:None,capture_ready:true,global_available:cfg!(target_os="windows"),runtime_status:"Global shortcuts are inactive".into(),runtime_detail:None}}
 pub fn from_preferences(preferences:&UserPreferences)->Self {let mut draft=Self::new(preferences.hotkeys());draft.baseline_global=preferences.global_hotkeys().clone();draft.global=preferences.global_hotkeys().bindings().to_vec();draft.capture_ready=false;draft}
 pub fn runtime(&mut self,available:bool,capture_ready:bool,status:&str,detail:Option<String>){self.global_available=available;self.capture_ready=capture_ready;self.runtime_status=status.to_owned();self.runtime_detail=detail;}
 pub fn is_listening(&self)->bool{self.listening.is_some()}
 fn prepared_pair(&self)->Result<(HotkeyBindings,GlobalHotkeyBindings),String>{if self.is_listening(){return Err("Finish or cancel the shortcut capture before applying.".into());}let local=HotkeyBindings::new(self.bindings.clone()).map_err(|e|e.to_string())?;let global=GlobalHotkeyBindings::new(self.global.clone()).map_err(|e|e.to_string())?;validate_hotkey_scopes(&local,&global).map_err(|e|e.to_string())?;Ok((local,global))}
 pub fn prepared(&self)->Result<HotkeyBindings,String>{self.prepared_pair().map(|(local,_)|local)}
 pub fn unchanged(&self,current:&HotkeyBindings)->bool{self.prepared_pair().is_ok_and(|(local,global)|&local==current&&global==self.baseline_global)}
 pub fn command(&self,current:&HotkeyBindings)->Result<Command,String>{if current!=&self.baseline||self.global.as_slice()!=self.baseline_global.bindings(){return Err("Shortcut assignments changed. Reopen this page before applying.".into());}Ok(Command::SetHotkeys {hotkeys:self.prepared()?})}
 pub fn unchanged_preferences(&self,current:&UserPreferences)->bool{self.prepared_pair().is_ok_and(|(local,global)|&local==current.hotkeys()&&&global==current.global_hotkeys())}
 pub fn command_preferences(&self,current:&UserPreferences)->Result<Command,String>{if current.hotkeys()!=&self.baseline||current.global_hotkeys()!=&self.baseline_global{return Err("Shortcut assignments changed. Reopen this page before applying.".into());}let(local,global)=self.prepared_pair()?;Ok(Command::ConfigureHotkeys {local,global})}
 fn assign(&mut self,action:HotkeyAction,slot:usize,chord:skylive_core::HotkeyChord)->Result<(),String>{
  let mut local=self.bindings.clone();let mut global=self.global.clone();let values=if self.scope==Scope::Local {&mut local}else{&mut global};
  if let Some(index)=values.iter().enumerate().filter(|(_,b)|b.action==action).nth(slot).map(|(i,_)|i){values[index].chord=chord;}else{values.push(HotkeyBinding {action,chord});}
  let local=HotkeyBindings::new(local).map_err(|e|e.to_string())?;let global=GlobalHotkeyBindings::new(global).map_err(|e|e.to_string())?;validate_hotkey_scopes(&local,&global).map_err(|e|e.to_string())?;
  self.bindings=local.bindings().to_vec();self.global=global.bindings().to_vec();Ok(())
 }
}
pub fn action_label(action: HotkeyAction) -> &'static str {
    match action {
        HotkeyAction::Undo => "Undo",
        HotkeyAction::Redo => "Redo",
        HotkeyAction::Save => "Save",
        HotkeyAction::ToggleStudio => "Toggle Studio Mode",
        HotkeyAction::Transition => "Transition",
        HotkeyAction::QuickCut => "Quick Cut",
        HotkeyAction::StartRecording => "Start Recording",
        HotkeyAction::StopRecording => "Stop Recording",
        HotkeyAction::PauseRecording=>"Pause Recording",
        HotkeyAction::ResumeRecording=>"Resume Recording",
        HotkeyAction::StartStreaming=>"Start Streaming",
        HotkeyAction::StopStreaming=>"Stop Streaming",
        HotkeyAction::StartReplayBuffer=>"Start Replay Buffer",
        HotkeyAction::StopReplayBuffer=>"Stop Replay Buffer",
        HotkeyAction::SaveReplayBuffer=>"Save Replay Buffer",
        HotkeyAction::StartVirtualCamera=>"Start Virtual Camera",
        HotkeyAction::StopVirtualCamera=>"Stop Virtual Camera",
        HotkeyAction::ScreenshotOutput=>"Screenshot Output",
        HotkeyAction::ScreenshotSelectedSource=>"Screenshot Selected Source",
        HotkeyAction::SourcePlayback{operation,..}=>operation.label(),
        HotkeyAction::SourceAudio{operation,..}=>operation.label(),
    }
}
pub fn editor(ui:&mut egui::Ui,draft:&mut HotkeyDraft){
 ui.horizontal_wrapped(|ui|{ui.add_enabled_ui(!draft.is_listening(),|ui|{ui.selectable_value(&mut draft.scope,Scope::Local,crate::i18n::tr(ui,"In SkyLive")).on_hover_text(crate::i18n::tr(ui,"Local shortcuts work while SkyLive is focused, outside text entry and dialogs."));ui.selectable_value(&mut draft.scope,Scope::Global,crate::i18n::tr(ui,"Windows global")).on_hover_text(crate::i18n::tr(ui,"Global shortcuts sample only configured keys and modifiers on Windows, including while another app has focus. They do not reserve shortcuts from other apps."));});});
 ui.add(egui::Label::new(crate::i18n::tr(ui,if draft.scope==Scope::Local {"Local shortcuts work while SkyLive is focused, outside text entry and dialogs."}else{"Global shortcuts sample only configured keys and modifiers on Windows, including while another app has focus. They do not reserve shortcuts from other apps."})).wrap());
 if draft.scope==Scope::Global {ui.add(egui::Label::new(crate::i18n::tr_key_or(ui,"SkyLive.Hotkeys.WindowsLimits","Ctrl, Alt and Shift are supported. Windows-key combinations and punctuation are unavailable.")).wrap());}
 if !draft.global_available {ui.add(egui::Label::new(crate::i18n::tr(ui,"Global shortcuts are unavailable on this platform. Saved assignments are retained.")).wrap());}
 ui.label(crate::i18n::tr(ui,&draft.runtime_status));if let Some(detail)=&draft.runtime_detail{ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(ui,detail)).color(ui.visuals().warn_fg_color)).wrap());}
 if !draft.capture_ready {ui.label(crate::i18n::tr(ui,"Waiting for configured shortcut input to suspend before capture."));}
 ui.add(egui::Label::new(crate::i18n::tr(ui,"Click an assignment and press a key combination. Escape cancels capture. Apply saves both scopes; Cancel discards this draft.")).wrap());
 if let Some((action,slot))=draft.listening {
  let event=ui.input_mut(|input|{let index=input.events.iter().position(|event|matches!(event,egui::Event::Key {pressed:true,repeat:false,key,..}if *key==egui::Key::Escape||draft.capture_ready))?;match input.events.remove(index){egui::Event::Key {key,modifiers,..}=>Some((key,modifiers)),_=>None}});
  if let Some((key,modifiers))=event {if key==egui::Key::Escape {draft.listening=None;draft.error=None;}else{match crate::shortcuts::chord(key,modifiers).and_then(|chord|draft.assign(action,slot,chord)){Ok(())=>{draft.listening=None;draft.error=None;},Err(error)=>draft.error=Some(error)}}}
 }
 if draft.scope==Scope::Global && draft.capture_ready {
  if let Some((action,slot))=draft.listening {
   if let Some(chord)=keypad_picker(ui) {match draft.assign(action,slot,chord){Ok(())=>{draft.listening=None;draft.error=None;},Err(error)=>draft.error=Some(error)}}
  }
 }
 if let Some(error)=&draft.error{ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(ui,error)).color(ui.visuals().error_fg_color)).wrap());}
 let capture=draft.capture_ready&&(draft.scope==Scope::Local||draft.global_available);
 egui::ScrollArea::horizontal().id_salt("hotkeys-columns").show(ui,|ui|{
  ui.set_min_width(570.0);let actions:&[HotkeyAction]=if draft.scope==Scope::Local{&HotkeyAction::ALL}else{&HotkeyAction::GLOBAL};
  for &action in actions {ui.push_id((draft.scope==Scope::Global,action),|ui|{ui.horizontal(|ui|{
   ui.add_sized([170.0,28.0],egui::Label::new(crate::i18n::tr(ui,action_label(action))).truncate()).on_hover_text(crate::i18n::tr(ui,action_label(action)));
   for slot in 0..2 {ui.push_id(slot,|ui|{
    let values=if draft.scope==Scope::Local {&draft.bindings}else{&draft.global};let binding=values.iter().filter(|b|b.action==action).nth(slot);let assigned=binding.is_some();
    let caption=if draft.listening==Some((action,slot)){crate::i18n::tr(ui,"Press a shortcut…")}else{binding.map(|b|if draft.scope==Scope::Global{global_label(ui,&b.chord)}else{crate::shortcuts::label(&b.chord)}).unwrap_or_else(||crate::i18n::tr(ui,"Unassigned"))};
    let response=ui.add_enabled_ui(capture,|ui|{
     ui.add_sized([145.0,28.0],egui::Button::new(&caption)
      .selected(draft.listening==Some((action,slot)))
      .truncate())
    }).inner
      .on_hover_text(crate::i18n::tr(ui,"Assign a shortcut to this action. Escape cancels capture."))
      .on_hover_text(&caption)
      .on_disabled_hover_text(crate::i18n::tr(ui,"Wait until configured shortcut input is suspended before capturing a shortcut."));
    #[cfg(test)]if draft.scope==Scope::Local {ui.ctx().data_mut(|data|{data.insert_temp(egui::Id::new(("hotkey-assignment",action,slot)),response.rect);});}
    if response.clicked(){draft.listening=Some((action,slot));draft.error=None;response.request_focus();}
    if ui.add_enabled_ui(assigned,|ui|ui.add_sized([56.0,28.0],egui::Button::new(crate::i18n::tr(ui,"Clear")))).inner.on_hover_text(crate::i18n::tr(ui,"Remove this shortcut assignment.")).on_disabled_hover_text(crate::i18n::tr(ui,"Remove this shortcut assignment.")).clicked(){let values=if draft.scope==Scope::Local{&mut draft.bindings}else{&mut draft.global};if let Some(index)=values.iter().enumerate().filter(|(_,b)|b.action==action).nth(slot).map(|(i,_)|i){values.remove(index);}draft.listening=None;draft.error=None;}
   });}
  });});}
 });
 let label=if draft.scope==Scope::Local {"Restore default shortcuts"}else{"Clear application global shortcuts"};
 if ui.button(crate::i18n::tr(ui,label)).on_hover_text(crate::i18n::tr(ui,"Restore defaults in this scope only; Apply saves the draft.")).clicked(){
  let local=if draft.scope==Scope::Local {HotkeyBindings::default()}else{HotkeyBindings::new(draft.bindings.clone()).expect("validated local draft")};
  let global=if draft.scope==Scope::Global {GlobalHotkeyBindings::default()}else{GlobalHotkeyBindings::new(draft.global.clone()).expect("validated global draft")};
  match validate_hotkey_scopes(&local,&global){Ok(())=>{draft.bindings=local.bindings().to_vec();draft.global=global.bindings().to_vec();draft.listening=None;draft.error=None;},Err(error)=>draft.error=Some(error.to_string())}
 }
}

fn key_label(ui:&egui::Ui,key:&str)->String {
 if let Some(digit)=key.strip_prefix("Numpad").filter(|v|v.len()==1&&v.as_bytes()[0].is_ascii_digit()) {
  return crate::i18n::tr_key_or(ui,"Hotkeys.NumpadNum","Numpad %1").replace("%1",digit);
 }
 let (id,fallback)=match key {
  "NumLock"=>("Hotkeys.NumLock","Num Lock"),"NumpadAdd"=>("Hotkeys.NumpadAdd","Numpad Add"),
  "NumpadSubtract"=>("Hotkeys.NumpadSubtract","Numpad Subtract"),"NumpadMultiply"=>("Hotkeys.NumpadMultiply","Numpad Multiply"),
  "NumpadDivide"=>("Hotkeys.NumpadDivide","Numpad Divide"),"NumpadDecimal"=>("Hotkeys.NumpadDecimal","Numpad Decimal"),
  _=>return key.to_owned(),
 };
 crate::i18n::tr_key_or(ui,id,fallback)
}
pub(super) fn global_label(ui:&egui::Ui,chord:&skylive_core::HotkeyChord)->String {
 let mut parts=Vec::with_capacity(4);if chord.primary(){parts.push("Ctrl".to_owned());}if chord.alt(){parts.push("Alt".to_owned());}if chord.shift(){parts.push("Shift".to_owned());}parts.push(key_label(ui,chord.key()));parts.join("+")
}
/// Egui does not retain a distinct numpad key identity. Choose its physical
/// Windows key explicitly while holding any desired modifiers.
pub(super) fn keypad_picker(ui:&mut egui::Ui)->Option<skylive_core::HotkeyChord> {
 let mut result=None;
 let title=crate::i18n::tr_key_or(ui,"Hotkeys.NumpadNum","Numpad %1").replace(" %1","");
 ui.menu_button(title,|ui| {
  for key in ["NumLock","Numpad0","Numpad1","Numpad2","Numpad3","Numpad4","Numpad5","Numpad6","Numpad7","Numpad8","Numpad9","NumpadAdd","NumpadSubtract","NumpadMultiply","NumpadDivide","NumpadDecimal"] {
   if ui.button(key_label(ui,key)).clicked() {
    let modifiers=ui.input(|input|input.modifiers);
    result=skylive_core::HotkeyChord::new(key,modifiers.ctrl,modifiers.alt,modifiers.shift).ok();
    ui.close_menu();
   }
  }
 });
 result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::{
        Intent,
        dialogs::{self, Dialog},
    };
    use skylive_core::{Command, Session};
    #[test]
    fn replaced_assignments_reject_stale_editor_before_publication() {
        let draft = HotkeyDraft::new(&HotkeyBindings::default());
        assert!(
            draft
                .command(&HotkeyBindings::new(Vec::new()).unwrap())
                .is_err()
        );
        assert!(draft.command(&HotkeyBindings::default()).is_ok());
    }
    fn frame(
        context: &egui::Context,
        slot: &mut Option<Dialog>,
        session: &Session,
        events: Vec<egui::Event>,
    ) -> Vec<Intent> {
        let mut intents = Vec::new();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 650.0),
                )),
                events,
                ..Default::default()
            },
            |context| dialogs::show(context, slot, &mut intents, true, &session.snapshot()),
        );
        intents
    }
    fn click(
        context: &egui::Context,
        slot: &mut Option<Dialog>,
        session: &Session,
        rect: egui::Rect,
    ) -> Vec<Intent> {
        frame(
            context,
            slot,
            session,
            vec![
                egui::Event::PointerMoved(rect.center()),
                egui::Event::PointerButton {
                    pos: rect.center(),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        frame(
            context,
            slot,
            session,
            vec![egui::Event::PointerButton {
                pos: rect.center(),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        )
    }
    fn key(
        context: &egui::Context,
        slot: &mut Option<Dialog>,
        session: &Session,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) -> Vec<Intent> {
        frame(
            context,
            slot,
            session,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
        )
    }
    fn assignment(context: &egui::Context, action: HotkeyAction) -> egui::Rect {
        context.data(|data| {
            data.get_temp::<egui::Rect>(egui::Id::new(("hotkey-assignment", action, 0usize)))
                .unwrap()
        })
    }
    fn footer(context: &egui::Context, index: usize) -> egui::Rect {
        context.data(|data| {
            data.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", index)))
                .unwrap()
        })
    }
    #[test]
    fn actual_capture_conflict_escape_cancel_and_apply_use_one_preference_command() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let mut session = Session::new();
        let before = session.snapshot();
        let mut slot = Some(Dialog::Hotkeys(HotkeyDraft::new(
            before.preferences().hotkeys(),
        )));
        for _ in 0..3 {
            frame(&context, &mut slot, &session, vec![]);
        }
        click(
            &context,
            &mut slot,
            &session,
            assignment(&context, HotkeyAction::StartRecording),
        );
        key(
            &context,
            &mut slot,
            &session,
            egui::Key::Z,
            egui::Modifiers::COMMAND,
        );
        let Some(Dialog::Hotkeys(draft)) = &slot else {
            panic!("hotkey dialog retained");
        };
        assert!(draft.error.as_ref().unwrap().contains("more than once"));
        assert!(draft.prepared().is_err());
        assert!(draft.is_listening());
        let blocked = click(&context, &mut slot, &session, footer(&context, 0));
        assert!(
            blocked.is_empty(),
            "Apply stays disabled during shortcut capture"
        );
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        key(
            &context,
            &mut slot,
            &session,
            egui::Key::Escape,
            egui::Modifiers::NONE,
        );
        assert!(
            matches!(&slot,Some(Dialog::Hotkeys(draft))if draft.unchanged(before.preferences().hotkeys()))
        );
        for _ in 0..3 {
            frame(&context, &mut slot, &session, vec![]);
        }
        click(
            &context,
            &mut slot,
            &session,
            assignment(&context, HotkeyAction::StartRecording),
        );
        key(
            &context,
            &mut slot,
            &session,
            egui::Key::F9,
            egui::Modifiers::NONE,
        );
        // Finishing capture removes its error/status rows and can recenter the
        // modal. Click the actual enabled footer at its settled position.
        for _ in 0..3 {
            frame(&context, &mut slot, &session, vec![]);
        }
        assert!(matches!(&slot,Some(Dialog::Hotkeys(draft))if !draft.is_listening()));
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        let intents = click(&context, &mut slot, &session, footer(&context, 0));
        let command = intents
            .into_iter()
            .find_map(|intent| match intent {
                Intent::Command(command) => Some(command),
                Intent::SubmitSettings { .. } => {
                    super::super::settings::prepare(slot.as_ref().unwrap(), &session).ok()
                }
                _ => None,
            })
            .unwrap();
        assert!(
            matches!(&command,Command::SetHotkeys{hotkeys}if hotkeys.bindings().iter().any(|binding|binding.action==HotkeyAction::StartRecording&&binding.chord.key()=="F9"))
        );
        session.apply(command).unwrap();
        assert_eq!(session.document_revision(), 0);
        assert!(!session.can_undo());
        slot = Some(Dialog::Hotkeys(HotkeyDraft::new(
            session.snapshot().preferences().hotkeys(),
        )));
        for _ in 0..3 {
            frame(&context, &mut slot, &session, vec![]);
        }
        click(
            &context,
            &mut slot,
            &session,
            assignment(&context, HotkeyAction::StopRecording),
        );
        key(
            &context,
            &mut slot,
            &session,
            egui::Key::F10,
            egui::Modifiers::NONE,
        );
        let before_cancel = session.snapshot();
        let intents = click(&context, &mut slot, &session, footer(&context, 1));
        assert!(slot.is_none());
        assert!(intents.is_empty());
        assert!(std::sync::Arc::ptr_eq(&before_cancel, &session.snapshot()));
    }
}
