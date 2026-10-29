//! Desktop composition of OS global shortcuts; events retain their original
//! owner/Session/config generation until dispatch, including across UI intents.
use super::*;
use skylive_host::global_hotkeys::{Owner,Scope,Phase,Status,Fired};
pub(super) struct Runtime {pub owner:Owner,last_issue:Option<(Option<Scope>,Phase,Option<String>)>,media_pending:Option<(skylive_host::media_source::MediaRequestKey,Id)>}
impl Runtime {pub fn new(context:&egui::Context,push:skylive_host::audio_capture::PushAudioSink)->std::io::Result<Self>{let context=context.clone();Ok(Self {owner:Owner::with_push_audio(Arc::new(move||context.request_repaint()),Some(push))?,last_issue:None,media_pending:None})}}
fn label(status:&Status,desired:Option<Scope>)->&'static str {
 if status.phase==Phase::Closed{return "Global shortcut owner is closed";}
 if status.scope!=desired{return "Updating configured shortcut input…";}
 match status.phase{Phase::Idle=>"Global shortcuts are inactive",Phase::Paused=>"Global shortcuts are suspended while editing or confirming a document action",Phase::WaitingForRelease=>"Release the configured keys before global shortcuts are activated",Phase::Active=>"Configured shortcuts are active with press/release arbitration",Phase::AwaitingCallback=>"Waiting for shortcut callback acceptance",Phase::Retiring=>"Retiring shortcut press state and pending callbacks",Phase::Failed=>"Configured shortcut input stopped",Phase::Unavailable=>"Global shortcuts are unavailable on this platform",Phase::Closed=>"Global shortcut owner is closed"}
}
impl StudioApp {
 fn globals_paused(&self)->bool {self.close_dialog||self.document_repair.owns()||self.missing_files.panel.is_some()||self.documents.modal_open()||self.collections.modal_open()||self.collections.removal_pending()||self.after_io.is_some()||self.session.is_closed()||matches!(self.dialog,Some(Dialog::Hotkeys(_)|Dialog::SourcePlaybackHotkeys(_)|Dialog::SourceAudioHotkeys(_)|Dialog::Collection{..}))}
 pub(super) fn global_hotkey_events(&mut self,context:&egui::Context)->Vec<Intent>{
  let globals=self.session.snapshot().preferences().global_hotkeys().clone();let paused=self.globals_paused();
  if let Err(error)=self.global_hotkeys.owner.synchronize(&self.session,&globals,paused){if !matches!(error,skylive_host::global_hotkeys::Error::Busy){self.message(true,error.to_string());}}
  let events=self.global_hotkeys.owner.poll();let status=self.global_hotkeys.owner.status().clone();let desired=self.global_hotkeys.owner.desired_scope();
  let issue=(status.scope,status.phase,status.detail.clone());
  if self.global_hotkeys.last_issue.as_ref()!=Some(&issue){
   if status.scope==desired&&matches!(status.phase,Phase::Failed|Phase::Unavailable|Phase::Retiring){self.message(true,status.detail.clone().unwrap_or_else(||label(&status,desired).to_owned()));}
   self.global_hotkeys.last_issue=Some(issue);
  }
  let dropped=self.global_hotkeys.owner.take_dropped();if dropped>0 {eprintln!("global hotkeys: dropped events={dropped}");self.message(true,crate::i18n::tf_context(context,"Global shortcut queue overflow: {} events were not delivered.",&[("{}",dropped.to_string())]));}
  if let Some(Dialog::Hotkeys(draft))=&mut self.dialog {draft.source_scope=Some((self.session.identity(),self.session.snapshot().revision()));draft.runtime(Owner::available(),self.global_hotkeys.owner.suspended()||!Owner::available(),label(&status,desired),status.detail.clone());}
  if let Some(Dialog::SourcePlaybackHotkeys(draft))=&mut self.dialog{
   draft.current=draft.fresh(self.session.identity(),&self.session.snapshot());draft.available=Owner::available();draft.capture_ready=self.global_hotkeys.owner.suspended()||!Owner::available();draft.status=label(&status,desired).to_owned();draft.detail=status.detail.clone();
  }
  if let Some(Dialog::SourceAudioHotkeys(draft))=&mut self.dialog{
   draft.current=draft.fresh(self.session.identity(),&self.session.snapshot());draft.available=Owner::available();draft.capture_ready=self.global_hotkeys.owner.suspended()||!Owner::available();draft.status=label(&status,desired).to_owned();draft.detail=status.detail.clone();
  }
  events.into_iter().map(Intent::GlobalHotkey).collect()
 }
 pub(super) fn global_hotkey(&mut self,event:Fired,context:&egui::Context){
  let callback=event.callback();
  if !self.global_hotkeys.owner.accepts(&event,&self.session,self.globals_paused()){callback.reject();return;}
  if let skylive_core::HotkeyAction::SourceAudio{source,operation}=event.action{self.source_audio_hotkey(source,operation,callback,context);return;}
  if let skylive_core::HotkeyAction::SourcePlayback{source,operation}=event.action{self.source_playback_hotkey(source,operation,callback,context);return;}
  use skylive_core::HotkeyAction as Action;
  // Original MAKE_CALLBACK accepts its actual state-gated invocation; native
  // output startup/completion is separate and may still report a later error.
  let intent=match event.action{
   Action::StartRecording|Action::StopRecording|Action::PauseRecording|Action::ResumeRecording=>self.recording.hotkey_intent(event.action),
   Action::StartStreaming|Action::StopStreaming=>self.streaming.hotkey_intent(event.action),
   Action::StartReplayBuffer|Action::StopReplayBuffer=>self.replay.hotkey_intent(event.action),
   Action::StartVirtualCamera|Action::StopVirtualCamera=>self.virtual_camera.hotkey_intent(event.action),
   action=>crate::shortcuts::action_intent(action,self.studio,self.transition_active,self.recording.view.clone(),self.session.can_undo(),self.session.can_redo()),
  };
  if let Some(intent)=intent{if callback.accept(){self.intent(intent,context);}}else{callback.reject();}
 }
 pub(super) fn invalidate_shortcut_callbacks(&mut self){let paused=self.globals_paused();self.global_hotkeys.owner.invalidate_if_stale(&self.session,paused);}
 pub(super) fn global_media_control_busy(&self)->bool{self.global_hotkeys.media_pending.is_some()}
 pub(super) fn global_media_control_started(&mut self,key:skylive_host::media_source::MediaRequestKey,source:Id){self.global_hotkeys.media_pending=Some((key,source));}
 pub(super) fn complete_global_media_control(&mut self,key:skylive_host::media_source::MediaRequestKey,source:Id,result:&Result<skylive_host::media_source::MediaTransportStatus,skylive_host::media_source::MediaSourceError>)->bool{
  if self.global_hotkeys.media_pending!=Some((key,source)){return false;}self.global_hotkeys.media_pending=None;
  if key.session==self.session.identity(){if let Err(error)=result{self.message(true,error.to_string());}}true
 }

}
