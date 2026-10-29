//! Replacement repair coordinator. Candidate model stays on the IO actor.
use super::*;
use skylive_host::document_repair::{Key,Lease,Stage,State,Event,Receipt};
use skylive_host::missing_files::Choice as ResourceChoice;
use views::missing_files::{Panel,Action};
pub(super) struct Ready{pub key:Key,pub lease:Lease,pub path:PathBuf}
enum Operation{Advance,Page,Prepare(ResourceChoice),Apply,Accept,Discard,ReturnProjection,Commit}
struct Pending{request:u64,session:u64,operation:Operation,token:u64,generation:u64}
struct ReturnProjection{receipt:Receipt,discard:bool}
struct Deferred{intent:Intent,path:Option<String>}
#[derive(Default)]pub(super) struct Flow{
 pub panel:Option<Panel>,pub key:Option<Key>,path:Option<PathBuf>,lease:Option<Lease>,scope:Option<(u64,u64)>,
 pending:Option<Pending>,next:Option<usize>,retire:Option<ReturnProjection>,commit:Option<(Key,Lease)>,
 cancelled:bool,waiting_initial:bool,await_dirty:bool,
 startup:bool,published:bool,close_after_retire:bool,token:u64,deferred:Option<Deferred>,problem:Option<String>,
}
impl Flow{pub fn owns(&self)->bool{self.waiting_initial||self.key.is_some()||self.pending.is_some()||self.retire.is_some()||self.commit.is_some()}}
impl StudioApp{
 pub(super) fn begin_document_repair(&mut self,event:Event,startup:bool,fresh:bool,ctx:&egui::Context){
  self.document_repair.waiting_initial=false;self.document_repair.startup=startup;self.document_repair.problem=None;self.document_repair.published=false;
  self.document_repair.scope=Some((self.session.identity(),self.session.snapshot().revision()));
  self.document_repair.cancelled|=!fresh;
  self.document_repair.token=match self.document_repair.token.checked_add(1){Some(n)=>n,None=>{self.document_repair.cancelled=true;self.document_repair.token}};
  self.receive_repair_event(event,ctx);self.invalidate_shortcut_callbacks();ctx.request_repaint();
 }
 pub(super) fn repair_scope_fresh(&self)->bool{self.document_repair.scope==Some((self.session.identity(),self.session.snapshot().revision()))&&!self.session.is_closed()}
 fn receive_repair_event(&mut self,event:Event,ctx:&egui::Context){match event{
  Event::Stage(Stage{key,path,state,lease})=>{
   if self.document_repair.key.is_some_and(|old|!old.same_candidate(key)){self.document_repair.cancelled=true;}
   self.document_repair.key=Some(key);self.document_repair.path=Some(path);self.document_repair.lease=Some(lease);
   if self.document_repair.cancelled||!self.repair_scope_fresh(){self.document_repair.cancelled=true;return;}
   match state{
    State::Inspecting{next}=>{self.document_repair.panel=None;self.document_repair.next=Some(next);},
    State::Missing{page,problem,show_present}=>{
     self.document_repair.next=None;
     let mut panel=Panel::new(self.document_repair.token,self.session.identity(),self.session.snapshot().revision());panel.page=Some(page);panel.error=problem;panel.missing_only=!show_present;self.document_repair.panel=Some(panel);
    },
    State::Ready{path,origin:_,authorized,problem}=>{
     self.document_repair.panel=None;self.document_repair.next=None;self.document_repair.await_dirty=!authorized;self.document_repair.problem=problem;
     let ready=Ready{key,lease:self.document_repair.lease.as_ref().unwrap().clone(),path};
     if self.document_repair.startup&&self.session.is_document_dirty(){self.cancel_pending_open(ctx);return;}
     self.stage_repaired_document(ready,authorized,ctx);
    }
   }
  },
  Event::Prepared(receipt)=>{
   if self.document_repair.key.is_some_and(|key|!key.same_candidate(receipt.key)){self.document_repair.cancelled=true;}
   self.document_repair.key=Some(receipt.key);
   if self.document_repair.cancelled||!self.repair_scope_fresh()||receipt.preferences!=*self.session.snapshot().preferences(){self.document_repair.cancelled=true;self.document_repair.retire=Some(ReturnProjection{receipt,discard:true});return;}
   self.document_repair.problem=None;self.document_repair.await_dirty=false;self.document_repair.panel=None;self.document_repair.next=None;
   self.publish_repaired_document(receipt,self.document_repair.startup,ctx);
  }
 }}
 fn submit_repair_job(&mut self,job:Job,operation:Operation){let(token,generation)=(self.document_repair.token,self.document_repair.panel.as_ref().map_or(0,|p|p.generation));
  match self.io.submit_retained(self.session.identity(),job){
   Ok(())=>{if let Operation::Prepare(choice)=&operation{if let Some(panel)=&mut self.document_repair.panel{panel.ready.retain(|row|row.choice.slot!=choice.slot);}}if let Some(request)=self.io.request(){self.document_repair.pending=Some(Pending{request,session:self.session.identity(),operation,token,generation});}},
   Err((error,job))=>{
    // Retain the actual provisional/lease after failed admission. A disconnected
    // worker is an explicit fault, never a false retirement acknowledgement.
    match job{
     Job::ReturnRepairProjection{receipt,discard}=>self.document_repair.retire=Some(ReturnProjection{receipt,discard}),
     Job::CommitRepairCandidate{key,lease}=>self.document_repair.commit=Some((key,lease)),
     Job::RepairAdvance{start,..}=>self.document_repair.next=Some(start),
     _=>{}
    }self.repair_problem(error);
   }
  }
 }
 fn repair_problem(&mut self,detail:impl ToString){let detail=detail.to_string();self.document_repair.problem=Some(detail.clone());if let Some(panel)=&mut self.document_repair.panel{panel.error=Some(detail.clone());}self.message(true,detail);}
 pub(super) fn start_repair_accept(&mut self,ready:Ready,saved:Option<(PathBuf,skylive_host::storage::SavedFileStamp)>,ctx:&egui::Context){
  if self.document_repair.key!=Some(ready.key)||self.document_repair.cancelled||self.session.is_closed(){self.cancel_pending_open(ctx);return;}
  self.document_repair.scope=Some((self.session.identity(),self.session.snapshot().revision()));
  self.document_repair.lease=Some(ready.lease);self.document_repair.await_dirty=false;self.document_repair.next=None;
  self.submit_repair_job(Job::AcceptCandidate{key:ready.key,preferences:self.session.snapshot().preferences().clone(),saved},Operation::Accept);ctx.request_repaint();
 }
 pub(super) fn candidate_repair_action(&mut self,token:u64,action:Action,ctx:&egui::Context){
  if token!=self.document_repair.token{return;}if matches!(action,Action::Cancel){self.cancel_pending_open(ctx);return;}
  if self.io.busy()||self.document_repair.cancelled||!self.repair_scope_fresh(){self.repair_problem("The current document or pending collection changed. Cancel and reopen it.");return;}
  let Some(key)=self.document_repair.key else{return;};
  let Some(panel)=self.document_repair.panel.as_mut()else{return;};
  match action{
   Action::Select(slot)=>panel.select(slot),Action::Unstage=>if let Err(error)=panel.changed(){self.repair_problem(error);},
   Action::Scan(start)=>self.submit_repair_job(Job::RepairPage{key,start},Operation::Page),
   Action::Prepare|Action::Clear=>{let Some(mut choice)=panel.choice()else{return;};if matches!(action,Action::Clear){if !choice.slot.optional(){return;}choice.path=None;}self.submit_repair_job(Job::PrepareCandidateRelink{key,choice:choice.clone()},Operation::Prepare(choice));},
   Action::Done=>{let batch=(!panel.ready.is_empty()).then(||panel.batch());self.submit_repair_job(Job::ApplyCandidateRelinks{key,batch},Operation::Apply);},
   Action::Cancel=>{}
  }ctx.request_repaint();
 }
 pub(super) fn repair_completion(&mut self,completion:crate::io::Completion,ctx:&egui::Context)->Option<crate::io::Completion>{
  if self.document_repair.pending.as_ref().is_none_or(|p|p.request!=completion.request){return Some(completion);}let pending=self.document_repair.pending.take()?;
  if completion.session!=pending.session{self.document_repair.cancelled=true;self.repair_problem("Pending collection receipt belongs to another session.");}
  if matches!(pending.operation,Operation::Discard|Operation::ReturnProjection|Operation::Commit){
   match completion.result{
    Ok(Reply::RepairProjectionReturned(Some(stage)))if completion.session==pending.session=>self.receive_repair_event(Event::Stage(stage),ctx),
    Ok(Reply::CandidateDiscarded|Reply::RepairProjectionReturned(None)|Reply::RepairCommitted)if completion.session==pending.session=>{self.document_repair.key=None;self.document_repair.lease=None;self.document_repair.scope=None;self.document_repair.cancelled=false;self.document_repair.await_dirty=false;self.document_repair.waiting_initial=false;self.document_repair.published=false;},
    Err(error)=>self.repair_problem(error),_=>self.repair_problem("Pending collection retirement returned an unexpected receipt.")
   }ctx.request_repaint();return None;
  }
  if !self.repair_scope_fresh(){self.document_repair.cancelled=true;}
  match(pending.operation,completion.result){
   (Operation::Advance|Operation::Apply|Operation::Accept,Ok(Reply::DocumentRepair(event)))=>self.receive_repair_event(event,ctx),
   (Operation::Page,Ok(Reply::RepairPage(page)))=>if !self.document_repair.cancelled{if let Some(panel)=&mut self.document_repair.panel{if panel.token==pending.token&&panel.generation==pending.generation{panel.page=Some(page);panel.selected=None;panel.path.clear();}}},
   (Operation::Prepare(choice),Ok(Reply::CandidateRelink(prepared)))=>if !self.document_repair.cancelled{if let Some(panel)=&mut self.document_repair.panel{if panel.token==pending.token&&panel.generation==pending.generation&&panel.selected==Some(choice.slot)&&prepared.slot==choice.slot&&choice.path.as_ref().is_none_or(|p|p==&panel.path){if let Err(error)=panel.retain(choice,prepared){self.repair_problem(error);}}}},
   (_,Err(error))=>self.repair_problem(error),_=>self.repair_problem("Pending collection repair returned an unexpected receipt.")
  }ctx.request_repaint();None
 }
 pub(super) fn cancel_pending_open(&mut self,ctx:&egui::Context){
  self.documents.cancel();self.document_repair.cancelled=true;self.document_repair.panel=None;self.document_repair.next=None;self.document_repair.await_dirty=false;
  if self.native_replacement_pending(){self.cancel_native_replacement();self.document_repair.waiting_initial=true;}
  self.invalidate_shortcut_callbacks();ctx.request_repaint();
 }
 pub(super) fn defer_repair_intent(&mut self,intent:Intent,ctx:&egui::Context)->Option<Intent>{
  let newer=matches!(&intent,Intent::OpenDocumentPicker(_)|Intent::NewCollection|Intent::Load|Intent::Recover|Intent::ImportObs{..}|Intent::CollectionLibrary(views::collection_library::Action::Create{..}|views::collection_library::Action::Remove|views::collection_library::Action::Switch{..}));
  if !newer||(!self.document_repair.owns()&&!self.native_replacement_pending()){return Some(intent);}
  let path=matches!(&intent,Intent::Load|Intent::Recover).then(||self.document_path.clone());
  if path.as_ref().is_some_and(|p|p.len()>16_384)||matches!(&intent,Intent::ImportObs{path}if path.len()>16_384){self.repair_problem("The newer document choice exceeds the bounded path contract.");return None;}
  self.cancel_startup();self.cancel_pending_open(ctx);self.document_repair.deferred=Some(Deferred{intent,path});None
 }
 pub(super) fn update_pending_open(&mut self,ctx:&egui::Context){
  if self.document_repair.await_dirty&&!self.has_repair_document_candidate(){self.cancel_pending_open(ctx);}
  if self.io.busy()||self.document_repair.pending.is_some(){return;}
  if let Some(value)=self.document_repair.retire.take(){self.submit_repair_job(Job::ReturnRepairProjection{receipt:value.receipt,discard:value.discard},Operation::ReturnProjection);return;}
  if let Some((key,lease))=self.document_repair.commit.take(){self.document_repair.lease=None;self.submit_repair_job(Job::CommitRepairCandidate{key,lease},Operation::Commit);return;}
  if self.document_repair.cancelled{if let Some(key)=self.document_repair.key{self.document_repair.lease=None;self.submit_repair_job(Job::DiscardCandidate(key),Operation::Discard);return;}if !self.document_repair.waiting_initial{self.document_repair.cancelled=false;self.document_repair.scope=None;}}
  if let Some(start)=self.document_repair.next.take(){if let Some(key)=self.document_repair.key{self.submit_repair_job(Job::RepairAdvance{key,start},Operation::Advance);}return;}
  if !self.document_repair.owns()&&self.document_repair.close_after_retire{self.document_repair.close_after_retire=false;self.shutdown(ctx);return;}
  if !self.document_repair.owns(){if let Some(deferred)=self.document_repair.deferred.take(){if let Some(path)=deferred.path{self.document_path=path;}self.intent(deferred.intent,ctx);}}
 }
 pub(super) fn show_pending_open(&mut self,ctx:&egui::Context){
  let fresh=self.repair_scope_fresh();let mut intents=Vec::new();if let Some(panel)=&mut self.document_repair.panel{panel.show_pending(ctx,&mut intents,self.io.busy(),fresh,self.document_repair.path.as_deref().and_then(std::path::Path::to_str).unwrap_or(""));}for intent in intents{self.intent(intent,ctx);}
 }
 pub(super) fn show_pending_open_progress(&mut self,ctx:&egui::Context){
  if !self.document_repair.owns()||self.document_repair.panel.is_some()||(self.document_repair.await_dirty&&self.documents.modal_open()){return;}
  let can_retry=self.has_repair_document_candidate()&&!self.documents.modal_open()&&!self.io.busy();let mut intents=Vec::new();
  egui::TopBottomPanel::top("pending-native-repair-progress").show(ctx,|ui|{ui.horizontal_wrapped(|ui|{ui.spinner();ui.label(crate::i18n::tr(ui,if self.document_repair.published{"Collection opened; finishing cleanup…"}else if self.document_repair.cancelled{"Canceling collection opening…"}else{"Preparing the collection; the current Program remains active…"}));if !self.document_repair.published&&ui.button(crate::i18n::tr(ui,"Cancel opening")).clicked(){intents.push(Intent::CancelPendingOpen);}if can_retry&&ui.button(crate::i18n::tr(ui,"Retry opening")).clicked(){intents.push(Intent::RetryPendingOpen);}});if let Some(error)=&self.document_repair.problem{ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,error));}});
  for intent in intents{self.intent(intent,ctx);}
 }

}

impl StudioApp{
 pub(super) fn wait_pending_open_before_close(&mut self,ctx:&egui::Context)->bool{
  if !self.document_repair.owns()&&!self.native_replacement_pending(){return false;}
  self.document_repair.close_after_retire=true;self.document_repair.deferred=None;
  self.cancel_pending_open(ctx);self.update_pending_open(ctx);
  ctx.request_repaint_after(std::time::Duration::from_millis(32));true
 }
 pub(super) fn cancel_pending_open_close(&mut self){self.document_repair.close_after_retire=false;}
 pub(super) fn initial_repair_failed(&mut self){self.document_repair.waiting_initial=false;}
 pub(super) fn reject_repaired_publication(&mut self,receipt:Receipt,problem:impl ToString,ctx:&egui::Context){self.repair_problem(problem);self.document_repair.retire=Some(ReturnProjection{receipt,discard:self.document_repair.cancelled||!self.repair_scope_fresh()});self.document_repair.await_dirty=false;ctx.request_repaint();}
 pub(super) fn commit_repaired_publication(&mut self,key:Key,lease:Lease,ctx:&egui::Context){self.document_repair.published=true;self.document_repair.commit=Some((key,lease));self.document_repair.await_dirty=false;ctx.request_repaint();}

}
