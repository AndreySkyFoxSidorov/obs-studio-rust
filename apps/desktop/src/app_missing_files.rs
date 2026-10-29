//! One modal draft, one existing IO request, one atomic canonical publication.
use super::*;
use skylive_host::missing_files::{self,Choice};
use views::missing_files::{Action,Panel};
use skylive_host::media_source::{MediaReloadTarget,MediaRequestKey,MediaSourceError,MediaTransportStatus};
use skylive_host::browser::{BrowserReloadTarget,BrowserReloadReceipt,BrowserReloadDisposition,BrowserReloadProgress,BrowserError};
use std::collections::VecDeque;
use std::time::{Instant,Duration};
enum Operation{Scan,Prepare(Choice)}
struct Pending{request:u64,session:u64,revision:u64,token:u64,generation:u64,operation:Operation}
#[derive(Default)]pub(super) struct Runtime{pub panel:Option<Panel>,pending:Option<Pending>,next:u64,reloads:VecDeque<MediaReloadTarget>,reload:Option<(MediaRequestKey,MediaReloadTarget)>,deadline:Option<Instant>,browser_reloads:VecDeque<BrowserReloadTarget>,browser_reload:Option<(BrowserReloadTarget,BrowserReloadReceipt)>,browser_deadline:Option<Instant>}
impl StudioApp{
 pub(super) fn open_missing_files(&mut self){
  if !self.missing_files.browser_reloads.is_empty()||self.missing_files.browser_reload.is_some()||!self.missing_files.reloads.is_empty()||self.missing_files.reload.is_some()||self.io.busy()||self.dialog.is_some()||self.documents.modal_open()||self.collections.modal_open()||self.remux.view.open||self.close_dialog||self.after_io.is_some()||self.virtual_camera.settings_open()||self.ndi_output.settings.is_some()||self.session.is_closed(){self.message(true,"Close the active dialog or file operation before checking missing files.");return;}
  let Some(token)=self.missing_files.next.checked_add(1)else{self.message(true,"Missing Files dialog identity exhausted");return;};self.missing_files.next=token;
  self.missing_files.panel=Some(Panel::new(token,self.session.identity(),self.session.snapshot().revision()));self.invalidate_shortcut_callbacks();self.missing_files_action(token,Action::Scan(0));
 }
 pub(super) fn missing_files_action(&mut self,token:u64,action:Action){
  let Some(panel)=self.missing_files.panel.as_ref().filter(|p|p.token==token)else{return;};
  if matches!(action,Action::Cancel){self.missing_files.panel=None;self.invalidate_shortcut_callbacks();return;}
  if self.io.busy()||panel.session!=self.session.identity()||panel.revision!=self.session.snapshot().revision()||self.session.is_closed(){self.message(true,"The project changed or a file operation is active. Cancel and reopen Missing Files.");return;}
  match action{
   Action::Select(slot)=>{if let Some(p)=&mut self.missing_files.panel{p.select(slot);}},
   Action::Unstage=>{if let Some(p)=&mut self.missing_files.panel{if let Err(e)=p.changed(){p.error=Some(e);}}},
   Action::Done=>{let batch=panel.batch();if let Err(e)=batch.validate(){self.message(true,e.to_string());return;}let revision=panel.revision;
    let images:Vec<Id>=batch.rows.iter().filter_map(|row|matches!(row.slot,skylive_core::ResourceSlot::Image{..}|skylive_core::ResourceSlot::SlideShow{..}).then_some(row.slot.source())).collect();
    let media:Vec<Id>=batch.rows.iter().filter_map(|row|(matches!(row.slot,skylive_core::ResourceSlot::Media{..})&&row.expected==row.replacement).then_some(row.slot.source())).collect();
    let browsers:Vec<Id>=batch.rows.iter().filter_map(|row|(matches!(row.slot,skylive_core::ResourceSlot::BrowserHtml{..})&&row.expected==row.replacement).then_some(row.slot.source())).collect();
    // Capture obligations against the actual accepted canonical/no-op Session,
    // before backend hooks can report a postpublication failure or mutate scope.
    let committed=self.publish_command_projected_with_gate(Some(revision),Command::RelinkResources{session:self.session.identity(),revision,batch},false,||true,|session,_|prepare_relink_reloads(session,&media,&browsers));
    match committed{
     Some(reloads)=>{self.missing_files.panel=None;match reloads{
      Ok(reloads)=>self.refresh_relinked_resources(&images,reloads),
      Err(error)=>{self.refresh_relinked_resources(&images,RelinkReloads::default());self.message(true,crate::status::Status::formatted("Linked files were applied, but runtime reload preparation failed: {error}",&[("{error}",error)]));}
     }self.invalidate_shortcut_callbacks();},
     None=>if let Some(p)=&mut self.missing_files.panel{p.error=Some("Replacements were not applied. Review the status message, then correct or reopen the draft.".into());}
    }
   },
   Action::Scan(start)=>self.submit_missing_files(Job::ScanMissingFiles{model:self.session.snapshot(),start},Operation::Scan),
   Action::Prepare|Action::Clear=>{let Some(mut choice)=panel.choice()else{return;};if matches!(action,Action::Clear){if !choice.slot.optional(){return;}choice.path=None;}
    let request=missing_files::Request{model:self.session.snapshot(),choice:choice.clone()};self.submit_missing_files(Job::PrepareRelink(request),Operation::Prepare(choice));
   },
   Action::Cancel=>{},
  }
 }
 fn submit_missing_files(&mut self,job:Job,operation:Operation){let Some(panel)=&self.missing_files.panel else{return;};let(session,revision,token,generation)=(panel.session,panel.revision,panel.token,panel.generation);
  match self.io.submit(session,job){Ok(())=>{if let Operation::Prepare(choice)=&operation{if let Some(p)=&mut self.missing_files.panel{p.ready.retain(|r|r.choice.slot!=choice.slot);}}if let Some(request)=self.io.request(){self.missing_files.pending=Some(Pending{request,session,revision,token,generation,operation});}},Err(e)=>self.message(true,e)}
 }
 pub(super) fn missing_files_completion(&mut self,completion:crate::io::Completion)->Option<crate::io::Completion>{
  if self.missing_files.pending.as_ref().is_none_or(|p|p.request!=completion.request){return Some(completion);}let Some(pending)=self.missing_files.pending.take()else{return Some(completion);};
  let current=self.missing_files.panel.as_ref().is_some_and(|p|p.session==pending.session&&p.revision==pending.revision&&p.token==pending.token&&p.generation==pending.generation);
  if !current||completion.session!=pending.session||self.session.identity()!=pending.session||self.session.snapshot().revision()!=pending.revision||self.session.is_closed(){return None;}
  let Some(panel)=&mut self.missing_files.panel else{return None;};
  match(pending.operation,completion.result){
   (Operation::Scan,Ok(Reply::MissingFilesScanned(page)))=>{panel.page=Some(page);panel.selected=None;panel.path.clear();panel.error=None;},
   (Operation::Prepare(choice),Ok(Reply::RelinkPrepared(prepared)))=>{
    if prepared.slot!=choice.slot||panel.selected!=Some(choice.slot)||choice.path.as_ref().is_some_and(|path|path!=&panel.path){panel.error=Some("The replacement draft changed; prepare it again.".into());return None;}
    if let Err(e)=panel.retain(choice,prepared){panel.error=Some(e);}
   },
   (_,Err(error))=>panel.error=Some(error),
   _=>panel.error=Some("Missing Files returned an unexpected receipt.".into()),
  }None
 }
 pub(super) fn show_missing_files(&mut self,ctx:&egui::Context){let fresh=self.missing_files.panel.as_ref().is_some_and(|p|p.session==self.session.identity()&&p.revision==self.session.snapshot().revision()&&!self.session.is_closed());let mut intents=Vec::new();if let Some(panel)=&mut self.missing_files.panel{panel.show(ctx,&mut intents,self.io.busy(),fresh);}for intent in intents{self.intent(intent,ctx);}}
}

impl StudioApp{
 fn refresh_relinked_resources(&mut self,images:&[Id],reloads:RelinkReloads){
  let mut restarted=std::collections::HashSet::new();for source in images{if restarted.insert(*source){if let Err(error)=self.playback.restart_source(*source,self.frame_now){self.message(true,crate::status::Status::formatted("Linked files were applied, but resource refresh failed: {error}",&[("{error}",error.to_string())]));}}}
  if let Err(error)=self.recording.invalidate_resources(){self.stop_recording();self.message(true,crate::status::Status::formatted("Linked files were applied, but resource refresh failed: {error}",&[("{error}",error)]));}
  if let Err(error)=self.streaming.invalidate_resources(){self.stop_streaming();self.message(true,crate::status::Status::formatted("Linked files were applied, but resource refresh failed: {error}",&[("{error}",error)]));}
  if let Err(error)=self.virtual_camera.invalidate_resources(){self.stop_virtual_camera();self.message(true,crate::status::Status::formatted("Linked files were applied, but resource refresh failed: {error}",&[("{error}",error)]));}
  if let Err(error)=self.preview.retry_resources().and_then(|()|self.program.retry_resources()){self.message(true,crate::status::Status::formatted("Linked files were applied, but resource refresh failed: {error}",&[("{error}",error.to_string())]));}
  self.missing_files.reloads=reloads.media;self.missing_files.deadline=Instant::now().checked_add(Duration::from_secs(10));
  self.missing_files.browser_reloads=reloads.browsers;self.missing_files.browser_reload=None;self.missing_files.browser_deadline=Instant::now().checked_add(Duration::from_secs(10));
 }
 pub(super) fn drive_missing_media_reloads(&mut self,context:&egui::Context){
  if self.allow_close||self.session.is_closed(){self.missing_files.reloads.clear();return;}
  // The dirty-close prompt is still a reversible choice. Keep committed
  // reload obligations when it is canceled, just as for Browser reloads.
  if self.close_dialog{self.missing_files.deadline=Instant::now().checked_add(Duration::from_secs(10));return;}
  if self.missing_files.reload.is_some(){context.request_repaint_after(Duration::from_millis(10));return;}
  if self.media.is_busy(){return;}
  let Some(target)=self.missing_files.reloads.front()else{return;};
  if !target.matches(&self.session){self.missing_files.reloads.pop_front();self.message(true,"Linked files were applied; a changed source invalidated its pending media reload.");self.missing_files.deadline=Instant::now().checked_add(Duration::from_secs(10));return;}
  if self.missing_files.deadline.is_some_and(|deadline|Instant::now()>=deadline){self.missing_files.reloads.pop_front();self.message(true,"Linked files were applied, but the media runtime did not admit its reload before the deadline.");self.missing_files.deadline=Instant::now().checked_add(Duration::from_secs(10));return;}
  match self.media.reload(&self.session,target,self.frame_now){
   Ok(key)=>{if let Some(target)=self.missing_files.reloads.pop_front(){self.missing_files.reload=Some((key,target));}},
   Err(MediaSourceError::Busy)=>{},
   Err(error)=>{self.missing_files.reloads.pop_front();self.message(true,crate::status::Status::formatted("Linked files were applied, but media reload failed: {error}",&[("{error}",error.to_string())]));self.missing_files.deadline=Instant::now().checked_add(Duration::from_secs(10));}
  }
 }
 pub(super) fn accept_missing_media_reload(&mut self,key:MediaRequestKey,source:Id,result:&Result<MediaTransportStatus,MediaSourceError>)->bool{
  if self.missing_files.reload.as_ref().is_none_or(|(expected,target)|*expected!=key||target.source()!=source){return false;}
  let Some((_,target))=self.missing_files.reload.take()else{return false;};
  if target.matches(&self.session){match result{Ok(_)=>self.message(false,"Linked files were applied; the old current-source media reader was retired or absent. New decoding remains subject to eligibility and file errors."),Err(error)=>self.message(true,crate::status::Status::formatted("Linked files were applied, but media reload failed: {error}",&[("{error}",error.to_string())]))}}
  else{self.message(true,"Linked files were applied; a changed source invalidated its media reload receipt.");}
  self.missing_files.deadline=Instant::now().checked_add(Duration::from_secs(10));true
 }
}

impl StudioApp{
 fn browser_relink_problem(&mut self,error:impl ToString){self.message(true,crate::status::Status::formatted("Linked files were applied, but Browser reload failed: {error}",&[("{error}",error.to_string())]));}
 pub(super) fn drive_missing_browser_reloads(&mut self,context:&egui::Context){
  if self.allow_close||self.session.is_closed(){self.missing_files.browser_reloads.clear();self.missing_files.browser_reload=None;return;}
  // A canceled dirty-close prompt must not silently lose a committed relink’s
  // still-unadmitted reload. Pause admission while that modal owns user choice.
  if self.close_dialog{self.missing_files.browser_deadline=Instant::now().checked_add(Duration::from_secs(if self.missing_files.browser_reload.is_some(){30}else{10}));return;}
  if let Some((target,receipt))=self.missing_files.browser_reload.take(){
   match self.browser.reload_progress(&self.session,&target,receipt){
    BrowserReloadProgress::FirstPaint{..}=>self.message(false,"Linked files were applied; the reloaded Browser Source produced its first paint."),
    BrowserReloadProgress::Failed{detail}=>self.browser_relink_problem(detail),
    BrowserReloadProgress::Superseded=>self.message(true,"Linked files were applied; a changed source invalidated its pending Browser reload."),
    BrowserReloadProgress::DormantInvalidated=>self.message(false,"Linked files were applied; the dormant Browser Source was invalidated without starting its page."),
    BrowserReloadProgress::Queued=>{
     if self.missing_files.browser_deadline.is_some_and(|deadline|Instant::now()>=deadline){self.message(true,"Linked files were applied, but the reloaded Browser Source has not produced its first paint before the deadline.");}
     else{self.missing_files.browser_reload=Some((target,receipt));context.request_repaint_after(Duration::from_millis(16));return;}
    }
   }self.missing_files.browser_deadline=Instant::now().checked_add(Duration::from_secs(10));
  }
  let Some(target)=self.missing_files.browser_reloads.front()else{return;};
  if !target.matches(&self.session){self.missing_files.browser_reloads.pop_front();self.message(true,"Linked files were applied; a changed source invalidated its pending Browser reload.");self.missing_files.browser_deadline=Instant::now().checked_add(Duration::from_secs(10));return;}
  if self.missing_files.browser_deadline.is_some_and(|deadline|Instant::now()>=deadline){self.missing_files.browser_reloads.pop_front();self.message(true,"Linked files were applied, but the Browser runtime did not admit its reload before the deadline.");self.missing_files.browser_deadline=Instant::now().checked_add(Duration::from_secs(10));return;}
  match self.browser.reload(&self.session,target){
   Ok(receipt)=>{if let Some(target)=self.missing_files.browser_reloads.pop_front(){
    if matches!(receipt.disposition,BrowserReloadDisposition::DormantInvalidated){self.message(false,"Linked files were applied; the dormant Browser Source was invalidated without starting its page.");}
    else{self.message(false,"Linked files were applied; Browser reload was admitted. Page readiness is reported separately.");if receipt.paint_expected{self.missing_files.browser_reload=Some((target,receipt));self.missing_files.browser_deadline=Instant::now().checked_add(Duration::from_secs(30));return;}}
   }self.missing_files.browser_deadline=Instant::now().checked_add(Duration::from_secs(10));},
   Err(BrowserError::Busy)=>context.request_repaint_after(Duration::from_millis(16)),
   Err(error)=>{self.missing_files.browser_reloads.pop_front();self.browser_relink_problem(error);self.missing_files.browser_deadline=Instant::now().checked_add(Duration::from_secs(10));}
  }
 }
}

/// Small per-commit runtime obligations; never a retained snapshot/model table.
#[derive(Default)]struct RelinkReloads{media:VecDeque<MediaReloadTarget>,browsers:VecDeque<BrowserReloadTarget>}
fn prepare_relink_reloads(session:&Session,media:&[Id],browsers:&[Id])->Result<RelinkReloads,String>{
 if media.len().checked_add(browsers.len()).is_none_or(|count|count>skylive_core::MAX_RESOURCE_RELINKS){return Err("Runtime reload targets exceed the shared 64-source repair bound".into());}
 let mut media_ids=Vec::with_capacity(media.len());let mut browser_ids=Vec::with_capacity(browsers.len());
 for source in media{if !media_ids.contains(source){media_ids.push(*source);}}
 for source in browsers{if !browser_ids.contains(source){browser_ids.push(*source);}}
 if media_ids.len().checked_add(browser_ids.len()).is_none_or(|count|count>skylive_core::MAX_RESOURCE_RELINKS){return Err("Runtime reload targets exceed the shared 64-source repair bound".into());}
 let model=session.snapshot();let mut bytes=0usize;
 // Borrow metadata from the canonical committed model. Do not make even the
 // first deep binding clone until the complete aggregate has been admitted.
 for source in &media_ids{
  let Some(skylive_core::SourceKind::MediaSource{media})=model.source(*source).map(|s|s.kind())else{return Err("The committed media reload source changed".into());};
  bytes=bytes.checked_add(std::mem::size_of::<MediaReloadTarget>()).and_then(|n|n.checked_add(media.path().len())).filter(|n|*n<=skylive_host::media_source::MAX_MEDIA_RELOAD_METADATA).ok_or("Media reload target metadata exceeds 2 MiB")?;
 }
 for source in &browser_ids{let size=BrowserReloadTarget::metadata_bytes_for(session,*source).map_err(|e|e.to_string())?;bytes=bytes.checked_add(size).filter(|n|*n<=skylive_host::browser::MAX_BROWSER_RELOAD_METADATA).ok_or("Combined media and Browser reload target metadata exceeds 8 MiB")?;}
 drop(model);
 let mut reloads=RelinkReloads::default();
 for source in media_ids{reloads.media.push_back(MediaReloadTarget::new(session,source).map_err(|e|e.to_string())?);}
 for source in browser_ids{reloads.browsers.push_back(BrowserReloadTarget::new(session,source).map_err(|e|e.to_string())?);}
 Ok(reloads)
}
