//! Runtime-only definition/binding proof and desired-page reload receipt.
//! No filesystem, CEF, worker creation or inferred source activation here.
use super::*;
use skylive_core::SourceDefinitionEpoch;
pub const MAX_BROWSER_RELOAD_TARGETS:usize=64;
pub const MAX_BROWSER_RELOAD_METADATA:usize=8*1024*1024;
#[derive(Clone,Debug)]
pub struct BrowserReloadTarget{session:u64,source:Id,definition:SourceDefinitionEpoch,browser:BrowserSource}
fn binding_bytes(browser:&BrowserSource)->usize{std::mem::size_of::<BrowserReloadTarget>().saturating_add(browser.css().len()).saturating_add(match browser.location(){skylive_core::BrowserLocation::LocalFile{path}=>path.len(),skylive_core::BrowserLocation::Url{url}=>url.len()})}
impl BrowserReloadTarget{
 /// Borrowed aggregate admission BEFORE a caller makes any deep binding clone.
 pub fn metadata_bytes_for(session:&Session,source:Id)->Result<usize,BrowserError>{
  if session.is_closed(){return Err(BrowserError::Closed);}let model=session.snapshot();
  let Some(SourceKind::Browser{browser})=model.source(source).map(|s|s.kind())else{return Err(BrowserError::Invalid("Browser reload requires an existing Browser Source"));};
  let bytes=binding_bytes(browser);if bytes>MAX_BROWSER_RELOAD_METADATA{return Err(BrowserError::Invalid("Browser reload target exceeds its metadata bound"));}Ok(bytes)
 }
 pub fn new(session:&Session,source:Id)->Result<Self,BrowserError>{
  if session.is_closed(){return Err(BrowserError::Closed);}let model=session.snapshot();
  let Some(SourceKind::Browser{browser})=model.source(source).map(|s|s.kind())else{return Err(BrowserError::Invalid("Browser reload requires an existing Browser Source"));};
  if binding_bytes(browser)>MAX_BROWSER_RELOAD_METADATA{return Err(BrowserError::Invalid("Browser reload target exceeds its metadata bound"));}
  let definition=session.source_definition_epoch(source).ok_or(BrowserError::Stale)?;
  Ok(Self{session:session.identity(),source,definition,browser:browser.clone()})
 }
 pub fn source(&self)->Id{self.source}
 pub fn metadata_bytes(&self)->usize{binding_bytes(&self.browser)}
 pub fn matches(&self,session:&Session)->bool{!session.is_closed()&&session.identity()==self.session&&session.source_definition_epoch(self.source)==Some(self.definition)&&matches!(session.snapshot().source(self.source).map(|s|s.kind()),Some(SourceKind::Browser{browser})if browser==&self.browser)}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,serde::Serialize)]
#[serde(rename_all="snake_case")]
pub enum BrowserReloadDisposition{DesiredReloadAdmitted,DormantInvalidated}
/// This proves desired-state admission/invalidation only. It cannot prove that
/// native close/navigation finished, or that any new paint has arrived.
#[derive(Clone,Copy,Debug,serde::Serialize)]
pub struct BrowserReloadReceipt{pub instance:u64,pub session:u64,pub source:Id,pub epoch:u64,pub binding_lease:Option<u64>,pub disposition:BrowserReloadDisposition,pub paint_expected:bool,#[serde(skip)]definition:SourceDefinitionEpoch}
#[derive(Clone,Debug,serde::Serialize)]
#[serde(tag="phase",rename_all="snake_case")]
pub enum BrowserReloadProgress{Queued,DormantInvalidated,FirstPaint{native_epoch:u64,wire_lease:u64,sequence:u64},Failed{detail:String},Superseded}
impl BrowserOwner{
 pub fn reload(&mut self,session:&Session,target:&BrowserReloadTarget)->Result<BrowserReloadReceipt,BrowserError>{
  if self.closed.load(Ordering::Acquire)||session.is_closed(){return Err(BrowserError::Closed);}if !target.matches(session){return Err(BrowserError::Stale);}
  let mut desired=self.desired.try_lock().map_err(|_|BrowserError::Busy)?;
  // Check against the actual admission scope, not an earlier widget revision.
  if !target.matches(session){return Err(BrowserError::Stale);}
  let row=if desired.session==session.identity(){desired.rows.iter().position(|row|row.source==target.source)}else{None};
  if row.is_some_and(|index|desired.rows[index].browser.as_ref()!=&target.browser){return Err(BrowserError::Stale);}
  let epoch=self.epoch.checked_add(1).ok_or(BrowserError::Invalid("browser epoch exhausted"))?;
  let binding_lease=if row.is_some(){Some(self.next_lease.checked_add(1).ok_or(BrowserError::Invalid("browser lease exhausted"))?)}else{None};
  let mut eligible=false;let mut paint_expected=false;
  if let(Some(index),Some(lease))=(row,binding_lease){let row=&mut desired.rows[index];eligible=row.helper_eligible();paint_expected=row.visible;row.lease=lease;self.next_lease=lease;}
  self.epoch=epoch;if desired.session==session.identity(){desired.epoch=epoch;}
  // Real stale-publication invalidation even for an absent/dormant row. Do not
  // manufacture a row/activation to obtain an artificial native acknowledgement.
  if self.latest.session==session.identity(){let mut latest=(*self.latest).clone();latest.epoch=epoch;latest.frames.remove(&target.source);latest.formats.remove(&target.source);latest.problems.retain(|problem|problem.source!=target.source);latest.native_audio.clear();self.latest=Arc::new(latest);}
  Ok(BrowserReloadReceipt{instance:self.instance,session:session.identity(),source:target.source,epoch,binding_lease,disposition:if eligible{BrowserReloadDisposition::DesiredReloadAdmitted}else{BrowserReloadDisposition::DormantInvalidated},paint_expected,definition:target.definition})
 }
 /// Observe only real subsequent snapshots, never a successful send as Ready.
 pub fn reload_progress(&self,session:&Session,target:&BrowserReloadTarget,receipt:BrowserReloadReceipt)->BrowserReloadProgress{
  if !target.matches(session)||receipt.instance!=self.instance||receipt.session!=target.session||receipt.source!=target.source||receipt.definition!=target.definition{return BrowserReloadProgress::Superseded;}
  let desired=match self.desired.try_lock(){Ok(value)=>value,Err(_)=>return BrowserReloadProgress::Queued};
  let row=desired.rows.iter().find(|row|desired.session==session.identity()&&row.source==target.source);
  if let Some(row)=row{if Some(row.lease)!=receipt.binding_lease||row.browser.as_ref()!=&target.browser{return BrowserReloadProgress::Superseded;}}
  if self.latest.session==session.identity(){
   if let Some(frame)=self.latest.frames.get(&target.source).filter(|frame|Some(frame.binding_lease)==receipt.binding_lease&&frame.browser.as_ref()==&target.browser){return BrowserReloadProgress::FirstPaint{native_epoch:self.latest.native_epoch,wire_lease:frame.lease,sequence:frame.sequence};}
   if let Some(problem)=self.latest.problems.iter().find(|p|p.source==target.source){return BrowserReloadProgress::Failed{detail:problem.detail.clone()};}
  }
  if row.is_none_or(|row|!row.helper_eligible())||matches!(receipt.disposition,BrowserReloadDisposition::DormantInvalidated){BrowserReloadProgress::DormantInvalidated}else{BrowserReloadProgress::Queued}
 }
}
