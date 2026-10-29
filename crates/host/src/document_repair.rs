//! One raw unpublished native document and at most one publication projection.
//! Owned by the existing IO actor; no capture/render/audio owner or implicit save.
use crate::{storage,missing_files,collection_library};
use skylive_core::{Session,StudioSnapshot,ResourceRelinks,UserPreferences};
use std::{path::{Path,PathBuf},sync::{Arc,atomic::{AtomicU64,Ordering}}};
#[path="document_repair/budget.rs"]mod budget;
pub use budget::{Lease,METADATA_BYTES};
#[derive(Debug)]pub struct Error{pub code:&'static str,pub detail:String}
impl std::fmt::Display for Error{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{write!(f,"{}: {}",self.code,self.detail)}}
impl std::error::Error for Error{}
fn diagnostic(detail:impl ToString)->String{let detail=detail.to_string();if detail.len()>64*1024{eprintln!("Pending collection full diagnostic: {detail}");"Pending collection diagnostic exceeds 64 KiB; see standard error output.".into()}else{detail}}
fn limit(detail:impl ToString)->Error{Error{code:"repair_metadata_limit",detail:diagnostic(detail)}}
fn stale(detail:impl ToString)->Error{Error{code:"repair_stale",detail:diagnostic(detail)}}
fn failure(detail:impl ToString)->Error{Error{code:"repair_failed",detail:diagnostic(detail)}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub struct Key{owner:u64,serial:u64,generation:u64}
impl Key{pub fn same_candidate(self,other:Self)->bool{self.owner==other.owner&&self.serial==other.serial}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub enum OriginKind{Native,Recovery,Managed,ManagedRemoval}
pub enum Begin{Native{path:PathBuf,recover:bool},Managed{view:collection_library::View,id:collection_library::EntryId},Remove{view:collection_library::View,current:PathBuf}}
pub enum State{Inspecting{next:usize},Missing{page:missing_files::Page,problem:Option<String>,show_present:bool},Ready{path:PathBuf,origin:OriginKind,authorized:bool,problem:Option<String>}}
pub struct Stage{pub key:Key,pub path:PathBuf,pub state:State,pub lease:Lease}
pub enum Event{Stage(Stage),Prepared(Receipt)}
pub enum Accepted{Native{loaded:Box<storage::LoadedDocument>,path:PathBuf,recover:bool},Managed{opened:collection_library::Opened,removal:Option<collection_library::RemovePlan>}}
/// A pre-adopted provisional, never proof that the GUI published it. Return it
/// to reject_projection, or acknowledge commit only after real publication.
pub struct Receipt{pub key:Key,pub value:Accepted,pub preferences:UserPreferences,pub repaired:bool,pub lease:Lease}
enum Origin{Native{path:PathBuf,read:PathBuf,stamp:storage::SavedFileStamp,recover:bool},Managed(collection_library::RepairContext)}
impl Origin{
 fn path(&self)->&Path{match self{Self::Native{path,..}=>path,Self::Managed(context)=>&context.path}}
 fn kind(&self)->OriginKind{match self{Self::Native{recover:true,..}=>OriginKind::Recovery,Self::Native{..}=>OriginKind::Native,Self::Managed(context)=>if context.has_removal(){OriginKind::ManagedRemoval}else{OriginKind::Managed}}}
 fn recheck(&mut self)->Result<(),Error>{match self{Self::Native{read,stamp,..}=>{let bytes=storage::read_bytes(read).map_err(failure)?;if !stamp.matches(&bytes){return Err(stale("The native collection changed while its resources were being repaired. Cancel and reopen it."));}Ok(())},Self::Managed(context)=>context.recheck().map_err(failure)}}
}
struct Candidate{key:Key,raw:storage::LoadedDocument,ready:bool,accepting:bool,outstanding:bool,inspect_next:Option<usize>,preferences:Option<UserPreferences>,origin:Origin,lease:Lease}
impl Candidate{
 fn replace(&mut self,next:StudioSnapshot)->Result<(),Error>{
  let old=self.raw.session.snapshot();if next==*old{return Ok(());}let preview=self.raw.session.preview();let program=self.raw.session.program();
  // No apply/Undo history. Prior raw document remains owned until every fallible
  // facade/selection validation succeeds; the one new snapshot moves, not clones.
  let mut session=Session::from_snapshot(next,false).map_err(failure)?;session.select_preview(preview).map_err(failure)?;session.select_program(program).map_err(failure)?;self.raw.session=session;Ok(())
 }
}
static NEXT_OWNER:AtomicU64=AtomicU64::new(1);
pub struct Owner{identity:u64,serial:u64,candidate:Option<Candidate>}
impl Default for Owner{fn default()->Self{Self{identity:NEXT_OWNER.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|n.checked_add(1)).unwrap_or(0),serial:0,candidate:None}}}
impl Owner{
 pub fn begin(&mut self,begin:Begin)->Result<Event,Error>{
  if self.identity==0{return Err(stale("Pending-open owner identities exhausted"));}if self.candidate.is_some(){return Err(stale("A pending native candidate still owns the repair actor"));}
  let serial=self.serial.checked_add(1).ok_or_else(||stale("Pending-open request identities exhausted"))?;let lease=Lease::reserve()?;
  let(raw,origin)=match begin{
   Begin::Native{path,recover}=>{let(path,read)=if recover{let read=std::fs::canonicalize(storage::recovery_path(&path)).map_err(failure)?;let path=std::path::absolute(path).map_err(failure)?;(path,read)}else{let path=std::fs::canonicalize(path).map_err(failure)?;(path.clone(),path)};
    for p in [&path,&read]{let text=p.to_str().ok_or_else(||failure("Pending native path is not exact UTF-8"))?;if text.len()>16_384||text.chars().any(char::is_control){return Err(limit("Pending native path exceeds 16 KiB or contains controls"));}}
    let bytes=storage::read_bytes(&read).map_err(failure)?;let raw=storage::decode_with_report(&bytes,!recover).map_err(failure)?;let stamp=storage::SavedFileStamp::from_bytes(raw.session.snapshot().revision(),&bytes);(raw,Origin::Native{path,read,stamp,recover})},
   Begin::Managed{view,id}=>{let(raw,context)=collection_library::read_for_repair(&view,id).map_err(failure)?;(raw,Origin::Managed(context))},
   Begin::Remove{view,current}=>{let(raw,context)=collection_library::read_removal_for_repair(&view,&current).map_err(failure)?;(raw,Origin::Managed(context))},
  };
  budget::preflight(&raw.session.snapshot())?;let key=Key{owner:self.identity,serial,generation:0};self.serial=serial;
  self.candidate=Some(Candidate{key,raw,ready:false,accepting:false,outstanding:false,inspect_next:Some(0),preferences:None,origin,lease});self.stage(State::Inspecting{next:0})
 }
 fn current(&self,key:Key)->Result<&Candidate,Error>{let c=self.candidate.as_ref().filter(|c|c.key==key).ok_or_else(||stale("The pending collection or its resource generation changed"))?;if c.outstanding{return Err(stale("A publication projection is still owned or retiring"));}Ok(c)}
 fn current_mut(&mut self,key:Key)->Result<&mut Candidate,Error>{let c=self.candidate.as_mut().filter(|c|c.key==key).ok_or_else(||stale("The pending collection or its resource generation changed"))?;if c.outstanding{return Err(stale("A publication projection is still owned or retiring"));}Ok(c)}
 fn stage(&self,state:State)->Result<Event,Error>{let c=self.candidate.as_ref().ok_or_else(||stale("The pending collection has retired"))?;Ok(Event::Stage(Stage{key:c.key,path:c.origin.path().to_owned(),state,lease:c.lease.clone()}))}
 fn ready_stage(&self,problem:Option<String>)->Result<Event,Error>{let c=self.candidate.as_ref().ok_or_else(||stale("The pending collection has retired"))?;self.stage(State::Ready{path:c.origin.path().to_owned(),origin:c.origin.kind(),authorized:c.accepting,problem})}
 pub fn page(&self,key:Key,start:usize)->Result<missing_files::Page,Error>{missing_files::scan(&self.current(key)?.raw.session.snapshot(),start).map_err(failure)}
 pub fn prepare(&self,key:Key,choice:missing_files::Choice)->Result<skylive_core::ResourceRelink,Error>{let model=self.current(key)?.raw.session.snapshot();missing_files::prepare(missing_files::Request{model,choice}).map_err(failure)}
 pub fn apply(&mut self,key:Key,batch:Option<ResourceRelinks>)->Result<Event,Error>{
  let c=self.current_mut(key)?;let generation=c.key.generation.checked_add(1).ok_or_else(||stale("Pending resource generations exhausted"))?;
  if let Some(batch)=batch{let model=c.raw.session.snapshot();budget::relink(&model,&batch)?;let next=model.prepare_relinked_snapshot(batch).map_err(failure)?;c.replace(next)?;}
  c.key.generation=generation;c.ready=false;c.inspect_next=Some(0);self.stage(State::Inspecting{next:0})
 }
 /// Cursor continuity is a host invariant, not trusted to a widget/headless caller.
 pub fn advance(&mut self,key:Key,start:usize)->Result<Event,Error>{
  if self.current(key)?.inspect_next!=Some(start){return Err(stale("Pending collection inspection cursor changed"));}let page=self.page(key,start)?;
  if page.rows.iter().any(|r|r.state!=missing_files::State::Present){let c=self.current_mut(key)?;c.ready=false;c.inspect_next=None;return self.stage(State::Missing{page,problem:None,show_present:false});}
  if let Some(next)=page.next{self.current_mut(key)?.inspect_next=Some(next);return self.stage(State::Inspecting{next});}self.current_mut(key)?.inspect_next=None;self.materialize(key)
 }
 fn materialize(&mut self,key:Key)->Result<Event,Error>{
  let c=self.current_mut(key)?;
  {let model=c.raw.session.snapshot();budget::folders(&model)?;match crate::image_slideshow::directories::prepare_refreshed_snapshot(&model){
   Ok(Some(next))=>{let generation=c.key.generation.checked_add(1).ok_or_else(||stale("Pending resource generations exhausted"))?;c.replace(next)?;c.key.generation=generation;},Ok(None)=>{},
   Err(error)=>{let problem=diagnostic(error);let page=missing_files::scan(&model,0).map_err(failure)?;c.ready=false;return self.stage(State::Missing{page,problem:Some(problem),show_present:true});}
  }}
  c.ready=true;let key=c.key;if c.accepting{return self.project(key);}self.ready_stage(None)
 }
 /// Dirty choice is resolved by the real receiver workflow. No receiver Session
 /// is sent here; only its validated bounded preferences and current save proof.
 pub fn accept(&mut self,key:Key,preferences:UserPreferences,saved:Option<(PathBuf,storage::SavedFileStamp)>)->Result<Event,Error>{
  let c=self.current_mut(key)?;if !c.ready{return Err(stale("Pending collection is not fully prepared for acceptance"));}
  if let(Origin::Managed(context),Some((path,stamp)))=(&mut c.origin,saved){context.accepted_save(&path,stamp);}c.origin.recheck()?;
  c.preferences=Some(preferences);c.accepting=true;c.ready=false;c.inspect_next=Some(0);self.stage(State::Inspecting{next:0})
 }
 fn project(&mut self,key:Key)->Result<Event,Error>{
  let c=self.current_mut(key)?;c.origin.recheck()?;let generation=c.key.generation.checked_add(1).ok_or_else(||stale("Pending publication generations exhausted"))?;
  let preferences=c.preferences.as_ref().ok_or_else(||stale("Pending receiver preferences are unavailable"))?;
  let projected=(||{budget::adoption(&c.raw.session.snapshot(),preferences)?;Session::prepare_document_with_preferences(&c.raw.session,preferences).map_err(failure)})();
  let session=match projected{Ok(session)=>session,Err(error)=>return self.ready_stage(Some(error.to_string()))};
  let loaded=storage::LoadedDocument{session,migration:c.raw.migration.clone()};let value=match &c.origin{Origin::Native{path,recover,..}=>Accepted::Native{loaded:Box::new(loaded),path:path.clone(),recover:*recover},Origin::Managed(context)=>{let(opened,removal)=context.project(loaded);Accepted::Managed{opened,removal}}};
  c.key.generation=generation;c.outstanding=true;
  Ok(Event::Prepared(Receipt{key:c.key,value,preferences:preferences.clone(),repaired:c.raw.session.is_document_dirty(),lease:c.lease.clone()}))
 }
 /// Caller must return the actual provisional. Drop it on this actor BEFORE
 /// allowing any new preparation. A stale rejection cannot release a newer one.
 pub fn reject_projection(&mut self,receipt:Receipt,discard:bool)->Result<Option<Stage>,Error>{
  let key=receipt.key;let c=self.candidate.as_mut().filter(|c|c.key==key&&c.outstanding).ok_or_else(||stale("Publication projection retirement scope changed"))?;
  drop(receipt);c.outstanding=false;
  if discard{self.candidate=None;return Ok(None);}
  let Event::Stage(stage)=self.ready_stage(None)?else{return Err(stale("Pending readiness receipt is unavailable"));};Ok(Some(stage))
 }
 /// Acknowledged by GUI ONLY after the provisional became the published Session.
 pub fn commit(&mut self,key:Key)->Result<(),Error>{if self.candidate.as_ref().is_none_or(|c|c.key!=key||!c.outstanding){return Err(stale("Published projection retirement scope changed"));}self.candidate=None;Ok(())}
 /// Cancel may retire the generation just produced by its already-admitted job,
 /// but must never bypass an outstanding provisional's actual IO retirement.
 pub fn discard(&mut self,key:Key)->Result<(),Error>{if let Some(c)=&self.candidate{if !c.key.same_candidate(key){return Err(stale("Another pending collection still owns the repair actor"));}if c.outstanding{return Err(stale("Return the outstanding publication projection before discarding its raw candidate"));}}self.candidate=None;Ok(())}
}
