//! Browser source orchestration. Source configuration is a canonical command;
//! frame acquisition, CEF and helper lifecycle stay in the host owner.
use super::*;
impl StudioApp {
 pub(super) fn commit_browser(&mut self,request:views::browser_source::Submission,ctx:&egui::Context){
  if request.session!=self.session.identity()||request.revision!=self.session.snapshot().revision()||!matches!(&self.dialog,Some(Dialog::BrowserSource(d))if d.request().is_ok_and(|value|value==request)){self.message(true,"The Browser Source draft changed. Reopen its properties.");return;}
  self.apply_at(Some(request.revision),request.command());ctx.request_repaint();
 }
 pub(super) fn refresh_browser(&mut self,token:u64,session:u64,revision:u64,source:Id,ctx:&egui::Context){
  if session!=self.session.identity()||revision!=self.session.snapshot().revision()||!matches!(&self.dialog,Some(Dialog::BrowserSource(d))if d.token==token&&d.session==session&&d.revision==revision&&matches!(d.edit,views::browser_source::Edit::Rebind {source:id}if id==source)&&d.unchanged()){self.message(true,"The Browser Source draft changed. Reopen its properties.");return;}
  let result=skylive_host::browser::BrowserReloadTarget::new(&self.session,source).and_then(|target|self.browser.reload(&self.session,&target));
  match result{Ok(receipt)=>self.message(false,if matches!(receipt.disposition,skylive_host::browser::BrowserReloadDisposition::DormantInvalidated){"Dormant Browser Source invalidated; its page was not started."}else{"Browser reload admitted; native navigation and first paint remain pending."}),Err(error)=>self.message(true,error.to_string())}ctx.request_repaint();
 }
 pub(super) fn update_browser(&mut self,ctx:&egui::Context,video:&[FrameScene],program:&[FrameScene],audio_views:&[FrameScene],now:MediaTime,base:Arc<CaptureSnapshot>)->Arc<CaptureSnapshot>{
  self.drive_missing_browser_reloads(ctx);
  // Interaction is a video-resource scope only: never add it to Program or
  // audio activation. A hidden shutdown-when-hidden source can paint while open.
  let mut interaction_video=video.to_vec();
  if let Some(scope)=self.browser_interaction.scope(&self.session,&mut self.browser){interaction_video.push(scope);}
  // Keep native CEF speaker permission on independently selected ORIGINAL
  // audio membership; detector expansion happens inside the controlled PCM path.
  self.browser.set_camera_activation_policy(self.camera_activation.snapshot());
  if let Err(error)=self.browser.synchronize_scoped_audio(&self.session,&interaction_video,program,audio_views,audio_views,now,self.audio.program_key()){if !matches!(error,skylive_host::browser::BrowserError::Busy){self.message(true,error.to_string());}}
  if let Err(error)=self.browser.poll(&self.session){self.message(true,error.to_string());}
  if let Some(Dialog::BrowserSource(d))=&mut self.dialog {d.runtime_problem=if let views::browser_source::Edit::Rebind {source}=d.edit {self.browser.snapshot().problems().iter().find(|p|p.source==source).map(|p|p.detail.clone())}else{None};}
  if self.browser.has_sources(){ctx.request_repaint_after(std::time::Duration::from_millis(16));}
  match self.browser_union.merge(&self.session,base.clone(),&self.browser){Ok(snapshot)=>snapshot,Err(error)=>{self.message(true,error.to_string());base}}
 }
}
