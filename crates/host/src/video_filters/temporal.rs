//! OBS32 gpu_delay history. All raster ownership stays on the existing shared
//! filter worker; observers publish only bounded metadata, never captured Arcs.
use super::*;
use crate::{capture::CaptureSnapshot,transition::FrameScene};
use skylive_core::{Id,Source,StudioSnapshot,VideoEffect,MAX_SOURCES,MAX_VIDEO_FILTERS};
use skylive_media::MediaTime;
use sha2::{Digest,Sha256};
use std::collections::VecDeque;
const MAX_SCOPES:usize=3*MAX_SOURCES;
// Preserve the former scope capacity when proofs expand from eight to 32 slots.
// This is a sublimit of the unchanged shared 128 MiB reservation.
const MAX_SCOPE_BYTES:usize=2*1024*1024;
const FRAME_METADATA:usize=MAX_KEY+MAX_VIDEO_FILTERS*std::mem::size_of::<VideoFilter>()+std::mem::size_of::<Frame>()+128;
static NEXT:AtomicU64=AtomicU64::new(1);
fn identity()->Result<u64,FilterError>{NEXT.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|n.checked_add(1)).map_err(|_|limited("temporal identity exhausted"))}
pub(crate) fn active(filters:&[VideoFilter])->bool{filters.iter().any(|f|f.enabled&&matches!(f.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{delay_ms:1..=500}))}
fn active_for(filters:&[VideoFilter],freeze_enabled:bool)->bool{filters.iter().any(|filter|filter.enabled&&match filter.effect{VideoEffect::Freeze{..}=>freeze_enabled,VideoEffect::RenderDelay{delay_ms}=>delay_ms!=0,VideoEffect::PersonBackgroundRemoval{..}=>filter.person_interval_ms()!=0,_=>false})}

pub(super) fn stale(detail:&str)->FilterError{FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"render_delay_stale",detail:detail.into()}}
struct HashWriter{hash:Sha256,bytes:usize}
impl std::io::Write for HashWriter{fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{self.bytes=self.bytes.checked_add(bytes.len()).filter(|n|*n<=MAX_KEY).ok_or_else(||std::io::Error::other("Render Delay metadata exceeds shared key limit"))?;self.hash.update(bytes);Ok(bytes.len())}fn flush(&mut self)->std::io::Result<()>{Ok(())}}
fn digest(value:&(impl serde::Serialize+?Sized))->Result<[u8;32],FilterError>{let mut writer=HashWriter{hash:Sha256::new(),bytes:0};serde_json::to_writer(&mut writer,value).map_err(|e|limited(e.to_string()))?;Ok(writer.hash.finalize().into())}
fn captured_video(kind:&skylive_core::SourceKind)->bool{match kind{skylive_core::SourceKind::DisplayCapture{..}|skylive_core::SourceKind::WindowCapture{..}|skylive_core::SourceKind::VideoCapture{..}|skylive_core::SourceKind::GameCapture{..}|skylive_core::SourceKind::NdiSource{..}|skylive_core::SourceKind::Browser{..}=>true,skylive_core::SourceKind::MediaSource{media}=>media.video_dimensions().is_some(),_=>false}}
fn binding_digest(source:&Source,reset:u64)->Result<[u8;32],FilterError>{
 use skylive_core::SourceKind as Kind;
 // Borrow descriptors directly. A source proof must not clone URL/CSS/media
 // bindings just to hash them, and camera audio-only edits keep video identity.
 Ok(match source.kind(){
  Kind::Image{asset}=>digest(&(asset,source.image_settings(),reset))?,
  Kind::ImageSlideShow{settings}=>{let mut hash=Sha256::new();for asset in settings.images(){hash.update(asset.sha256());hash.update(asset.path().as_bytes());}hash.update(reset.to_le_bytes());hash.finalize().into()},
  Kind::DisplayCapture{binding}=>digest(&("display",binding,reset))?,
  Kind::WindowCapture{binding}=>digest(&("window",binding,reset))?,
  Kind::VideoCapture{binding}=>digest(&("video",binding.target(),binding.mode(),binding.mode_policy(),binding.fps_policy(),binding.deactivate_when_hidden(),binding.flip_vertically(),reset))?,
  Kind::MediaSource{media} if media.video_dimensions().is_some()=>digest(&("media",media,reset))?,
  Kind::GameCapture{binding}=>digest(&("game",binding,reset))?,
  Kind::NdiSource{ndi}=>digest(&("ndi",ndi,reset))?,
  Kind::Browser{browser}=>digest(&("browser",browser,reset))?,
  _=>digest(&(None::<u8>,reset))?,
 })
}
fn source_bound(view:&FrameScene,model:&StudioSnapshot)->usize{let retained=match view{FrameScene::Live(_)=>0,FrameScene::Frozen(instance)=>instance.source_count()};model.sources().len().saturating_add(retained).min(2*MAX_SOURCES)}
fn workspace_bound(model:&StudioSnapshot,views:&[FrameScene])->Result<usize,FilterError>{
 // One visible graph is owned at a time. Bounds cover its growing out Vec,
 // seen/active/done hash tables, nested ID vector and bounded recursive stack.
 // This temporary reservation is based on this model/view, not MAX_SCOPES,
 // and never becomes part of the retained ScopeSet lease.
 let sources=views.iter().map(|view|source_bound(view,model)).max().unwrap_or(0);
 let scenes=model.scenes().len().checked_add(sources).map(|n|n.min(2*(skylive_core::MAX_SCENES+MAX_SOURCES))).ok_or_else(||limited("Render Delay scope metadata capacity"))?;
 sources.checked_add(1).and_then(|n|n.checked_mul(80)).and_then(|n|scenes.checked_add(1).and_then(|m|m.checked_mul(112)).and_then(|m|n.checked_add(m))).and_then(|n|n.checked_add(8192)).filter(|n|*n<=MAX_SCOPE_BYTES).ok_or_else(||limited("Render Delay scope metadata capacity"))
}
fn reserve_scope(scopes:&mut Vec<Scope>,lease:&mut Lease,workspace:usize,budget:&Arc<Budget>)->Result<(),FilterError>{
 let needed=scopes.len().checked_add(1).ok_or_else(||limited("Render Delay scope metadata capacity"))?;
 if needed>MAX_SCOPES{return Err(limited("Render Delay source scope capacity"));}
 if needed<=scopes.capacity(){return Ok(());}
 let fixed=std::mem::size_of::<ScopeSet>()+2*std::mem::size_of::<usize>();let size=std::mem::size_of::<Scope>();
 let old=scopes.capacity().checked_mul(size).ok_or_else(||limited("Render Delay scope metadata capacity"))?;
 // Account old and replacement allocations simultaneously, plus traversal
 // workspace, before requesting capacity. Duplicate (source,owner) proofs do
 // not call this function and therefore never expand the metadata table.
 let maximum=MAX_SCOPE_BYTES.checked_sub(workspace).and_then(|n|n.checked_sub(fixed)).and_then(|n|n.checked_sub(old)).map(|n|n/size).map(|n|n.min(MAX_SCOPES)).ok_or_else(||limited("Render Delay scope metadata capacity"))?;
 if needed>maximum{return Err(limited("Render Delay scope metadata capacity"));}
 let capacity=scopes.capacity().max(4).saturating_mul(2).min(maximum).max(needed);
 let bytes=capacity.checked_mul(size).ok_or_else(||limited("Render Delay scope metadata capacity"))?;
 let mut replacement=Lease::acquire(budget,bytes).ok_or_else(||limited("Render Delay scope byte admission"))?;
 scopes.try_reserve_exact(capacity-scopes.len()).map_err(|_|limited("Render Delay scope allocation"))?;
 let actual=scopes.capacity().checked_mul(size).filter(|n|*n<=bytes).ok_or_else(||limited("Render Delay scope metadata capacity"))?;
 replacement.shrink(actual);lease.shrink(fixed);lease.merge_metadata(replacement)
}
fn native_origin(frame:&crate::capture::CapturedFrame)->Result<[u8;32],FilterError>{
 // A camera can change clock domain without reopening its physical stream.
 // An async-delay reconfiguration likewise keeps the stream but replaces its
 // buffered history. Neither old Freeze captures nor Render Delay rings may
 // be inherited across those boundaries, even if settings are later restored.
 let delay_generation=frame.async_delay.as_ref().map(|receipt|receipt.binding_generation);
 digest(&(frame.native_origin,frame.native_clock_epoch,delay_generation,frame.window.as_ref().map(|w|w.generation),frame.display.as_ref().and_then(|d|d.generation),frame.image.width(),frame.image.height()))
}
fn nested_binding(view:&FrameScene,model:&StudioSnapshot,scene:Id,now:MediaTime,capture:Option<&CaptureSnapshot>,reset:&impl Fn(Id)->u64,total:&mut usize)->Result<Option<[u8;32]>,FilterError>{
 fn visit(view:&FrameScene,model:&StudioSnapshot,scene:Id,depth:usize,ids:&mut Vec<Id>,total:&mut usize)->Result<(),FilterError>{if depth>skylive_core::MAX_SCENE_DEPTH{return Err(limited("Render Delay dependency depth"));}let items=view.scene_items(model,scene).map_err(|e|stale(&e.to_string()))?;*total=total.checked_add(items.len()).filter(|n|*n<=6*skylive_core::MAX_ITEMS).ok_or_else(||limited("Render Delay privacy dependency work"))?;for item in items.iter().filter(|item|item.visible()){if ids.contains(&item.source()){continue;}if ids.len()>=2*MAX_SOURCES{return Err(limited("Render Delay privacy dependency count"));}ids.push(item.source());let source=view.source(model,item.source()).ok_or_else(||stale("Render Delay missing nested source"))?;if let Some(child)=source.kind().nested_scene(){visit(view,model,child,depth+1,ids,total)?;}}Ok(())}
 let mut ids=Vec::new();ids.try_reserve_exact(source_bound(view,model)).map_err(|_|limited("Render Delay privacy dependency allocation"))?;visit(view,model,scene,0,&mut ids,total)?;ids.sort_unstable();let mut hash=Sha256::new();for id in ids{if model.source(id).is_none(){return Ok(None);}let source=view.source(model,id).ok_or_else(||stale("Render Delay missing privacy dependency"))?;let captured=capture.and_then(|c|c.frame(id)).filter(|f|f.requested_at<=now&&f.binding.matches_source(source.kind()));hash.update(id.get().to_le_bytes());hash.update(binding_digest(source,reset(id))?);hash.update(captured.map(native_origin).transpose()?.unwrap_or([0;32]));hash.update([u8::from(!captured_video(source.kind())||captured.is_some())]);}Ok(Some(hash.finalize().into()))
}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash)]
pub(super) struct DelayProof{pub id:u64,pub slot:u8,pub ms:u16,pub layout:[u8;32]}
// A model-input proof is separate from the strict final-frame configuration.
// Its stable identity excludes Person's threshold/softness, which are applied
// after inference. Only static, non-resource prefix stages are eligible.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash)]
pub(super) struct PersonInputProof {pub window:u64,pub row:u32,pub canonical:u64,pub prefix:[u8;32]}
#[derive(Clone,Copy,Debug,Default)]
struct PersonProofSlot {projection_id:u64,proof:PersonInputProof}
// Window+real row identity is distinct from the temporary Core projection ID.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,serde::Serialize)]
pub(crate) struct RowScope{window:u64,rows:[u32;MAX_VIDEO_FILTERS]}
#[derive(Clone,Copy)]
pub(crate) struct DraftRows{window:u64,source:Id,rows:[u32;MAX_VIDEO_FILTERS],filters:[u64;MAX_VIDEO_FILTERS],len:usize,definition:[u8;32]}
impl DraftRows{
 pub(crate) fn bind(source:&Source,window:u64,rows:&[u32])->Result<Self,FilterError>{
  let filters=source.video_filters();
  if window==0||rows.len()!=filters.len()||rows.len()>MAX_VIDEO_FILTERS||rows.iter().enumerate().any(|(index,row)|*row==0||rows[..index].contains(row)){return Err(fail("Invalid filter preview draft row ownership"));}
  let mut value=Self{window,source:source.id(),rows:[0;MAX_VIDEO_FILTERS],filters:[0;MAX_VIDEO_FILTERS],len:rows.len(),definition:digest(filters)?};
  for(index,(row,filter))in rows.iter().zip(filters).enumerate(){value.rows[index]=*row;value.filters[index]=filter.id().ok_or_else(||fail("Filter preview projection has no issued filter identity"))?.get();}
  Ok(value)
 }
 fn scope(&self,source:&Source)->Result<RowScope,FilterError>{
  if source.id()!=self.source{return Ok(RowScope{window:self.window,rows:[0;MAX_VIDEO_FILTERS]});}
  let filters=source.video_filters();
  if filters.len()!=self.len||filters.iter().enumerate().any(|(index,filter)|filter.id().map_or(0,|id|id.get())!=self.filters[index])||digest(filters)?!=self.definition{return Err(stale("Filter preview draft row binding changed"));}
  Ok(RowScope{window:self.window,rows:self.rows})
 }
}
fn freeze_proof(index:usize,filter:&VideoFilter,rows:RowScope)->DelayProof{
 if rows.rows[index]!=0{
  let mut layout=[0;32];layout[..8].copy_from_slice(&rows.window.to_le_bytes());layout[8..16].copy_from_slice(b"draftrow");
  return DelayProof{id:u64::from(rows.rows[index]),slot:0,ms:u16::MAX,layout};
 }
 DelayProof{id:filter.id().map_or(0,|id|id.get()),slot:if filter.id().is_some(){0}else{(index+1)as u8},ms:u16::MAX,layout:[0;32]}
}
pub(super) fn freeze_target(context:Context,index:usize,filter:&VideoFilter)->Option<crate::freeze_timing_book::Target>{
 use crate::freeze_timing_book::{Instance,Target};
 let instance=if context.rows.window!=0 && context.rows.rows[index]!=0{
  Instance::Draft{window:context.rows.window,row:context.rows.rows[index]}
 }else if context.rows.window!=0{
  Instance::DraftPrivate{window:context.rows.window,owner:context.owner,filter:filter.id()?}
 }else if context.owner!=0{
  Instance::Private{owner:context.owner,filter:filter.id()?}
 }else{
  Instance::Canonical{filter:filter.id()?}
 };
 Some(Target{session:context.session,source:context.source,instance})
}
fn delays(filters:&[VideoFilter],freeze_enabled:bool,rows:RowScope)->Result<[DelayProof;MAX_VIDEO_FILTERS],FilterError>{
 if filters.len()>MAX_VIDEO_FILTERS{return Err(limited("temporal filter metadata count"));}
 let mut result=[DelayProof::default();MAX_VIDEO_FILTERS];let mut prefix=[0u64;MAX_VIDEO_FILTERS];
 for(index,filter)in filters.iter().enumerate(){
  prefix[index]=filter.id().map_or((index+1)as u64,|id|id.get());
  if filter.enabled{match filter.effect{
   VideoEffect::Freeze{..} if freeze_enabled=>result[index]=freeze_proof(index,filter,rows),
   VideoEffect::RenderDelay{delay_ms}=>result[index]=DelayProof{id:filter.id().map_or(0,|id|id.get()),slot:index as u8,ms:delay_ms,layout:digest(&prefix[..=index])?},
   _=>{}
  }}
 }
 Ok(result)
}
fn person_proofs(source:&Source,model:&StudioSnapshot,rows:RowScope)->Result<[PersonProofSlot;MAX_VIDEO_FILTERS],FilterError>{
 let mut result=[PersonProofSlot::default();MAX_VIDEO_FILTERS];
 if !matches!(source.kind(),skylive_core::SourceKind::VideoCapture{..}){return Ok(result);}
 let mut prefix=Vec::new();let mut eligible=true;
 for (index,filter) in source.video_filters().iter().enumerate(){
  if !filter.enabled||!model.preferences().plugin_enabled(filter.plugin_id()){continue;}
  if matches!(filter.effect,VideoEffect::PersonBackgroundRemoval{..}) {
   if eligible&&filter.person_interval_ms()!=0 {
    if let Some(id)=filter.id(){let row=rows.rows[index];result[index]=PersonProofSlot{projection_id:id.get(),proof:PersonInputProof{
     window:if row!=0{rows.window}else{0},row,canonical:if row!=0{0}else{id.get()},prefix:digest(&prefix)?}};}
   }
   // A preceding Person's output depends on an independently evolving mask.
   eligible=false;continue;
  }
  if matches!(filter.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{..}|VideoEffect::Scroll{..}|VideoEffect::ScaleFilter{..}|VideoEffect::ApplyLut{..}|VideoEffect::ImageMask{..}){eligible=false;}
  if eligible{prefix.push(filter.effect);}
 }
 Ok(result)
}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,serde::Serialize)]
pub(crate) struct Stamp{pub capture_epoch:u64,pub sequence:u64,pub at:u64,pub native:[u8;32]}
#[derive(Clone,Copy,Debug)]
struct Scope{source:Id,owner:u64,grant:u64,binding:[u8;32],config:[u8;32],stamp:Stamp,delays:[DelayProof;MAX_VIDEO_FILTERS],persons:[PersonProofSlot;MAX_VIDEO_FILTERS],draft:bool,camera:bool,rows:RowScope}
pub(super) struct ScopeSet{domain:u64,session:u64,at:u64,interval:u64,rate:[u32;2],lifecycle:[u8;32],scopes:Vec<Scope>,_lease:Lease}
#[derive(Clone,Default)]
pub(crate) struct Snapshot{set:Option<Arc<ScopeSet>>}
impl std::fmt::Debug for Snapshot{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.debug_struct("RenderDelaySnapshot").field("sources",&self.set.as_ref().map_or(0,|s|s.scopes.len())).finish()}}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,serde::Serialize)]
pub(crate) struct Context{pub domain:u64,pub session:u64,pub source:Id,pub owner:u64,pub grant:u64,pub at:u64,pub tick:u64,pub interval:u64,pub rate:[u32;2],pub config:[u8;32],pub binding:[u8;32],pub stamp:Stamp,pub draft:bool,pub camera:bool,pub input_complete:bool,pub rows:RowScope}
impl Snapshot{
 pub(crate) fn reusable_from(&self,old:&Self)->bool{self.set.as_ref().is_none_or(|set|set.scopes.is_empty())&&old.set.as_ref().is_none_or(|set|set.scopes.is_empty())}
 pub(crate) fn context(&self,source:&Source,owner:u64)->Option<Context>{let set=self.set.as_ref()?;let proof=set.scopes.iter().find(|p|p.source==source.id()&&(p.owner==owner||p.draft)).or_else(||source.kind().nested_scene().is_none().then(||set.scopes.iter().find(|p|p.source==source.id()&&p.owner==0)).flatten())?;Some(Context{domain:set.domain,session:set.session,source:proof.source,owner:proof.owner,grant:proof.grant,at:set.at,tick:((u128::from(set.at)*u128::from(set.rate[0]))/(1_000_000_000u128*u128::from(set.rate[1])))as u64,interval:set.interval,rate:set.rate,config:proof.config,binding:proof.binding,stamp:proof.stamp,draft:proof.draft,camera:proof.camera,input_complete:false,rows:proof.rows})}
}
struct ObserverLife{runtime:Weak<Life>,slot:usize,domain:u64,resets:Mutex<[(u64,u64);MAX_SOURCES]>,_lease:Lease}
impl Drop for ObserverLife{fn drop(&mut self){if let Some(runtime)=self.runtime.upgrade(){let mut state=runtime.shared.state.lock().unwrap_or_else(|e|e.into_inner());if state.observers[self.slot]==self.domain{state.observers[self.slot]=0;state.desired[self.slot]=None;}}}}
#[derive(Clone)]pub(crate) struct Observer{life:Arc<ObserverLife>}
impl Runtime{
 pub(crate) fn temporal_observer(&self,draft:bool)->Result<Observer,FilterError>{let domain=identity()?;let slot=if draft{1}else{0};let lease=Lease::acquire(&self.life.shared.budget,std::mem::size_of::<ObserverLife>()+128).ok_or_else(||limited("Render Delay observer byte admission"))?;let mut state=self.life.shared.state.lock().map_err(|_|unavailable("Render Delay observer poisoned"))?;if state.observers[slot]!=0{return Err(unavailable("Render Delay root/draft observer already owned"));}state.observers[slot]=domain;Ok(Observer{life:Arc::new(ObserverLife{runtime:Arc::downgrade(&self.life),slot,domain,resets:Mutex::new([(0,0);MAX_SOURCES]),_lease:lease})})}
}
impl Observer{
 pub(crate) fn domain(&self)->u64{self.life.domain}
 pub(crate) fn reset(&self){if let Some(runtime)=self.life.runtime.upgrade(){let mut state=runtime.shared.state.lock().unwrap_or_else(|e|e.into_inner());if state.observers[self.life.slot]==self.life.domain{state.desired[self.life.slot]=None;}}}
 pub(crate) fn reset_source(&self,source:Id)->Result<(),FilterError>{let mut resets=self.life.resets.lock().map_err(|_|unavailable("Render Delay reset poisoned"))?;let index=resets.iter().position(|(id,_)|*id==source.get()).or_else(||resets.iter().position(|(id,_)|*id==0)).ok_or_else(||limited("Render Delay source reset capacity"))?;resets[index]=(source.get(),identity()?);Ok(())}
 pub(crate) fn sample(&self,session:u64,model:&StudioSnapshot,views:&[FrameScene],now:MediaTime,capture:Option<&CaptureSnapshot>)->Result<Snapshot,FilterError>{self.sample_rows(session,model,views,now,capture,None)}
 pub(crate) fn sample_draft(&self,session:u64,model:&StudioSnapshot,views:&[FrameScene],now:MediaTime,capture:Option<&CaptureSnapshot>,rows:Option<&DraftRows>)->Result<Snapshot,FilterError>{self.sample_rows(session,model,views,now,capture,rows)}
 fn sample_rows(&self,session:u64,model:&StudioSnapshot,views:&[FrameScene],now:MediaTime,capture:Option<&CaptureSnapshot>,draft_rows:Option<&DraftRows>)->Result<Snapshot,FilterError>{
  let freeze_enabled=model.preferences().plugin_enabled(skylive_core::EXELDRO_FREEZE_PLUGIN);
  let runtime=self.life.runtime.upgrade().ok_or_else(||unavailable("Render Delay owner retired"))?;if capture.is_some_and(|c|c.session()!=session){return Err(stale("Render Delay foreign capture"));}if views.len()>skylive_core::MAX_SCENES{return Err(limited("Render Delay video scope count"));}
  if !model.sources().iter().any(|source|active_for(source.video_filters(),freeze_enabled))&&!views.iter().any(|view|view.root_scene_source(model).is_some_and(|source|active_for(source.video_filters(),freeze_enabled))){self.reset();return Ok(Snapshot::default());}
  let workspace=workspace_bound(model,views)?;let _workspace=Lease::acquire(&runtime.shared.budget,workspace).ok_or_else(||limited("Render Delay scope byte admission"))?;
  let mut demanded=false;for view in views{view.validate_session(session).map_err(|e|stale(&e.to_string()))?;demanded|=view.root_scene_source(model).is_some_and(|source|active_for(source.video_filters(),freeze_enabled));for source in view.visible_sources(model).map_err(|e|stale(&e.to_string()))?{demanded|=active_for(source.video_filters(),freeze_enabled);}}
  if !demanded{self.reset();return Ok(Snapshot::default());}
  let previous={let state=runtime.shared.state.lock().map_err(|_|unavailable("Render Delay metadata poisoned"))?;if state.observers[self.life.slot]!=self.life.domain{return Err(stale("Render Delay observer replaced"));}state.desired[self.life.slot].clone()};
  let fixed=std::mem::size_of::<ScopeSet>()+2*std::mem::size_of::<usize>();
  let mut lease=Lease::acquire(&runtime.shared.budget,fixed).ok_or_else(||limited("Render Delay scope byte admission"))?;let mut scopes=Vec::new();
  let mut resets=self.life.resets.lock().map_err(|_|unavailable("Render Delay reset poisoned"))?;for entry in resets.iter_mut(){if entry.0!=0&&model.sources().iter().all(|s|s.id().get()!=entry.0){*entry=(0,0);}}
  let reset_of=|id:Id|resets.iter().find(|entry|entry.0==id.get()).map_or(0,|entry|entry.1);
  let mut add=|source:&Source,owner:u64,draft:bool,binding:[u8;32],stamp:Stamp,input_definition:Option<u64>,input_dimensions:Option<(u32,u32)>|->Result<(),FilterError>{
   if model.source(source.id()).is_none()||!active_for(source.video_filters(),freeze_enabled){return Ok(());}
   let freezing=freeze_enabled&&source.video_filters().iter().any(|filter|filter.enabled&&matches!(filter.effect,VideoEffect::Freeze{..}));
   let mut config=digest(&(source.video_filters(),model.preferences().plugin_enabled(skylive_core::CRAFT_VIDEO_PLUGIN),model.preferences().plugin_enabled(skylive_core::PERSON_BACKGROUND_PLUGIN),model.preferences().plugin_enabled(skylive_core::EXELDRO_FREEZE_PLUGIN)))?;
   // Revision fences a NEW first capture against canonical Color/Text/layout
   // edits. Drafts keep that revision, so their immutable prepared instance is
   // the extra input-definition fence. Neither value is a held-picture grant.
   // Do not include per-frame sequence/generation/time/Scroll in this proof:
   // normal camera evolution must not starve a slow admitted first capture.
   let binding=if freezing{
    config=digest(&(config,model.revision(),input_definition))?;
    digest(&(binding,"freeze-input-extent-v1",input_dimensions))?
   }else{binding};
   let rows=if draft{draft_rows.map(|rows|rows.scope(source)).transpose()?.unwrap_or_default()}else{RowScope::default()};
   if draft&&source.video_filters().iter().any(|filter|filter.enabled&&matches!(filter.effect,VideoEffect::Freeze{..})&&filter.id().is_none())&&rows.window==0{return Err(fail("Freeze filter preview requires stable draft row identities"));}
   let proof=delays(source.video_filters(),model.preferences().plugin_enabled(skylive_core::EXELDRO_FREEZE_PLUGIN),rows)?;
   let persons=person_proofs(source,model,rows)?;
   if let Some(old)=scopes.iter().find(|old:&&Scope|old.source==source.id()&&old.owner==owner){if old.binding!=binding||old.config!=config||old.stamp!=stamp{return Err(stale("Render Delay conflicting source scope"));}return Ok(());}
   if scopes.len()>=MAX_SCOPES{return Err(limited("Render Delay source scope capacity"));}
   let old=previous.as_ref().filter(|old|old.session==session&&now.0>=old.at).and_then(|old|old.scopes.iter().find(|p|p.source==source.id()&&p.owner==owner&&p.binding==binding&&p.stamp.native==stamp.native&&p.stamp.sequence<=stamp.sequence&&p.stamp.at<=stamp.at));
   let grant=old.map_or_else(identity,|old|Ok(old.grant))?;reserve_scope(&mut scopes,&mut lease,workspace,&runtime.shared.budget)?;scopes.push(Scope{source:source.id(),owner,grant,binding,config,stamp,delays:proof,persons,draft,camera:matches!(source.kind(),skylive_core::SourceKind::VideoCapture{..}),rows});Ok(())
  };
  let mut dependency_work=0usize;
  for view in views{view.validate_session(session).map_err(|e|stale(&e.to_string()))?;let draft=view.is_filter_preview();let input_definition=if draft{match view{FrameScene::Frozen(instance)=>Some(instance.instance()),FrameScene::Live(_)=>None}}else{None};let owner=if draft{self.life.domain}else{crate::scroll_animation::root_owner(view)};let mut visible=view.visible_sources(model).map_err(|e|stale(&e.to_string()))?;visible.sort_unstable_by_key(|source|source.id());let root_safe=visible.iter().all(|source|model.source(source.id()).is_some());let mut root_hash=Sha256::new();let mut root_ready=true;
   for source in visible{let raw_binding=captured_video(source.kind());let captured=capture.and_then(|c|c.frame(source.id())).filter(|f|f.requested_at<=now&&f.binding.matches_source(source.kind()));
    root_ready&=!raw_binding||captured.is_some();
    let mut binding=binding_digest(source,reset_of(source.id()))?;
    if active_for(source.video_filters(),freeze_enabled){if let Some(scene)=source.kind().nested_scene(){let Some(nested)=nested_binding(view,model,scene,now,capture,&reset_of,&mut dependency_work)? else{continue;};binding=digest(&(binding,nested))?;}}
;root_hash.update(source.id().get().to_le_bytes());root_hash.update(binding);
    let stamp=if let Some(frame)=captured{Stamp{capture_epoch:capture.unwrap().epoch(),sequence:frame.sequence,at:frame.requested_at.0,native:native_origin(frame)?}}else{Stamp::default()};
    root_hash.update(stamp.native);root_hash.update([u8::from(!raw_binding||captured.is_some())]);
    if !raw_binding||captured.is_some(){let private=source.kind().nested_scene().is_some()||source.requires_video_canvas()&&view.filter_canvas_dimensions(model)!=(model.video().width(),model.video().height());let source_owner=if draft||private{owner}else{0};let input_dimensions=match source.kind(){skylive_core::SourceKind::Scene{..}=>Some(view.filter_canvas_dimensions(model)),_=>source.kind().video_dimensions()};add(source,source_owner,draft,binding,stamp,input_definition,input_dimensions)?;}
   }
   if let Some(source)=view.root_scene_source(model).filter(|source|root_safe&&(root_ready||!model.preferences().plugin_enabled(skylive_core::EXELDRO_FREEZE_PLUGIN)||!source.video_filters().iter().any(|filter|filter.enabled&&matches!(filter.effect,VideoEffect::Freeze{..})))){root_hash.update(reset_of(source.id()).to_le_bytes());add(source,owner,draft,root_hash.finalize().into(),Stamp::default(),input_definition,Some(view.canvas_dimensions(model)))?;}
  }
  drop(resets);if scopes.is_empty(){self.reset();return Ok(Snapshot::default());}let interval=(1_000_000_000u64*u64::from(model.video().fps_den())/u64::from(model.video().fps_num())).max(1);let mut lifecycle=Sha256::new();lifecycle.update(self.life.domain.to_le_bytes());lifecycle.update(session.to_le_bytes());lifecycle.update(model.video().fps_num().to_le_bytes());lifecycle.update(model.video().fps_den().to_le_bytes());for scope in &scopes{lifecycle.update(scope.source.get().to_le_bytes());lifecycle.update(scope.owner.to_le_bytes());lifecycle.update(scope.grant.to_le_bytes());lifecycle.update(scope.config);for proof in &scope.delays{lifecycle.update(proof.id.to_le_bytes());lifecycle.update([proof.slot]);lifecycle.update(proof.ms.to_le_bytes());lifecycle.update(proof.layout);}}let set=Arc::new(ScopeSet{domain:self.life.domain,session,at:now.0,interval,rate:[model.video().fps_num(),model.video().fps_den()],lifecycle:lifecycle.finalize().into(),scopes,_lease:lease});
  {let mut state=runtime.shared.state.lock().map_err(|_|unavailable("Render Delay metadata poisoned"))?;if state.observers[self.life.slot]!=self.life.domain{return Err(stale("Render Delay observer retired during sample"));}state.desired[self.life.slot]=Some(set.clone());}
  Ok(Snapshot{set:Some(set)})
 }
}
#[derive(Clone,PartialEq,Eq,Hash)]struct LaneKey{domain:u64,session:u64,source:Id,owner:u64,grant:u64,proof:DelayProof,width:u32,height:u32,interval:u64,rate:[u32;2]}
#[derive(Clone)]pub(super) struct Frame{image:Arc<RgbaFrame>,blend:SourceBlend,_receipt:Arc<Receipt>,_lease:Arc<Lease>}
// Provenance remains owned with the actual pixels, never rewritten on a later draw.
#[allow(dead_code)]
#[derive(Clone)]enum Receipt{TransparentWarmup{scope:Context,_credit:Arc<Lease>},Captured{context:Context,upstream:Vec<VideoFilter>,parents:Vec<Arc<Receipt>>,_credit:Arc<Lease>,providers:FilterProviders,scroll:crate::scroll_animation::ScrollKey,resource_generation:u64,mask_epoch:u64,mask_stamp:Option<(u64,u64,u64,u64,u64,u64)>,backend:[u64;3]}}
struct Lane{key:LaneKey,queue:VecDeque<Option<Frame>>,transparent:Option<Frame>,last_tick:Option<u64>,reserve:Lease,binding:[u8;32]}
#[derive(Default)]pub(super) struct History{freeze:FreezeHistory,lanes:HashMap<LaneKey,Lane>,table_credit:Option<Lease>,lifecycle:Option<[Option<[u8;32]>;2]>}
impl History{
 pub(super) fn reconcile(&mut self,shared:&Shared){
  let desired={let state=shared.state.lock().unwrap_or_else(|e|e.into_inner());state.desired.clone()};
  self.freeze.reconcile(shared,&desired);
  // Release each stale fallback outside the shared mutex: its final raster
  // lease may be large and the application metadata owner must not wait on it.
  loop{
   let retired={let mut state=shared.state.lock().unwrap_or_else(|e|e.into_inner());
    let index=state.freeze_fallbacks.iter().position(|entry|{
     let Some(context)=entry.key.temporal else{return true;};
     let scope=desired.iter().flatten().any(|set|set.domain==context.domain&&set.session==context.session&&set.scopes.iter().any(|scope|scope.source==context.source&&scope.owner==context.owner&&scope.grant==context.grant));
     !scope||entry.targets.iter().enumerate().any(|(index,target)|target.is_some_and(|target|state.freeze_timers.effective(target)!=Some(true)||state.freeze_timers.run(target)!=Some(entry.runs[index])))
    });
    if index.is_none()&&state.freeze_fallbacks.is_empty(){state.freeze_fallbacks=Vec::new();state.freeze_fallback_table=None;}
    index.map(|index|state.freeze_fallbacks.swap_remove(index))
   };
   let Some(retired)=retired else{break;};drop(retired);
  }
  let fingerprint=[desired[0].as_ref().map(|set|set.lifecycle),desired[1].as_ref().map(|set|set.lifecycle)];if self.lifecycle==Some(fingerprint){return;}self.lifecycle=Some(fingerprint);
  let private:Vec<_>=if desired.iter().flatten().any(|set|set.scopes.iter().any(|p|p.owner!=0&&!p.draft)){self.lanes.keys().filter(|key|key.owner!=0).cloned().collect()}else{Vec::new()};
  self.lanes.retain(|key,lane|desired.iter().flatten().any(|set|set.domain==key.domain&&set.session==key.session&&set.interval==key.interval&&set.rate==key.rate&&set.scopes.iter().any(|p|p.source==key.source&&p.delays.contains(&key.proof)&&((p.owner==key.owner&&p.grant==key.grant)||(key.owner==0&&p.owner!=0&&!p.draft&&p.binding==lane.binding&&!private.iter().any(|owned|owned.domain==set.domain&&owned.source==p.source&&owned.owner==p.owner&&owned.grant==p.grant&&owned.proof==key.proof))))));
  if self.lanes.is_empty(){self.lanes=HashMap::new();self.table_credit.take();}
  let valid=|context:Context|desired.iter().flatten().any(|set|set.domain==context.domain&&set.session==context.session&&set.interval==context.interval&&set.rate==context.rate&&set.scopes.iter().any(|p|p.source==context.source&&p.owner==context.owner&&p.grant==context.grant));
  // Move the bounded LRU out before retiring raster Arcs. GUI metadata
  // publishers never wait for final pixel destruction under this mutex.
  let valid_key=|key:&SharedKey|key.temporal.is_none_or(|context|valid(context)&&(!key.freeze||desired.iter().flatten().any(|set|set.domain==context.domain&&set.session==context.session&&set.scopes.iter().any(|scope|scope.source==context.source&&scope.owner==context.owner&&scope.grant==context.grant&&scope.config==context.config))))&&key.person_scope.is_none_or(|context|desired.iter().flatten().any(|set|set.domain==context.domain&&set.session==context.session&&set.scopes.iter().any(|scope|scope.source==context.source&&scope.owner==context.owner&&scope.grant==context.grant&&scope.camera&&scope.binding==context.binding&&scope.config==context.config&&scope.rows==context.rows&&scope.stamp.native==context.stamp.native)));
  let retired={let mut state=shared.state.lock().unwrap_or_else(|e|e.into_inner());if state.entries.keys().any(|key|!valid_key(key)){Some(std::mem::take(&mut state.entries))}else{None}};
  if let Some(entries)=retired{for(key,value)in entries{if valid_key(&key){let mut state=shared.state.lock().unwrap_or_else(|e|e.into_inner());if state.entries.len()<MAX_ENTRIES&&!state.entries.contains_key(&key){state.entries.insert(key,value);continue;}}drop(value);}}
 }
 pub(super) fn current_person(shared:&Shared,context:Context)->bool{
  let state=shared.state.lock().unwrap_or_else(|e|e.into_inner());
  Self::current_person_in(&state,context)
 }
 pub(super) fn current_person_in(state:&State,context:Context)->bool{
  state.desired.iter().flatten().any(|set|set.domain==context.domain&&set.session==context.session&&set.scopes.iter().any(|p|p.source==context.source&&p.owner==context.owner&&p.grant==context.grant&&p.camera&&p.binding==context.binding&&p.config==context.config&&p.rows==context.rows&&p.stamp.native==context.stamp.native&&p.stamp.sequence>=context.stamp.sequence&&p.stamp.at>=context.stamp.at))
 }
 // The currently requested configuration must still be exact before looking
 // up a reusable input proof. Publication of the final image remains strict.
 pub(super) fn person_input_proof(shared:&Shared,context:Context,id:u64)->Option<PersonInputProof>{
  let state=shared.state.lock().ok()?;if !Self::current_person_in(&state,context){return None;}
  state.desired.iter().flatten().filter(|set|set.domain==context.domain&&set.session==context.session)
   .flat_map(|set|set.scopes.iter()).filter(|p|p.source==context.source&&p.owner==context.owner&&p.grant==context.grant&&p.binding==context.binding&&p.config==context.config&&p.rows==context.rows&&p.stamp.native==context.stamp.native)
   .flat_map(|p|p.persons.iter()).find(|p|p.projection_id==id).map(|p|p.proof)
 }
 // Retain probabilities only while the currently desired model input has the
 // same stable Person row and ordered static prefix, camera stream and grant.
 pub(super) fn current_person_input(shared:&Shared,context:Context,proof:PersonInputProof)->bool{
  let Ok(state)=shared.state.lock() else{return false;};
  state.desired.iter().flatten().any(|set|set.domain==context.domain&&set.session==context.session&&set.scopes.iter().any(|p|
   p.source==context.source&&p.owner==context.owner&&p.grant==context.grant&&p.camera&&p.binding==context.binding
   &&p.stamp.native==context.stamp.native&&p.stamp.sequence>=context.stamp.sequence&&p.stamp.at>=context.stamp.at
   &&p.persons.iter().any(|slot|slot.projection_id!=0&&slot.proof==proof)))
 }
 pub(super) fn current(shared:&Shared,context:Context)->bool{let state=shared.state.lock().unwrap_or_else(|e|e.into_inner());Self::current_in(&state,context)}
 pub(super) fn exact_in(state:&State,context:Context)->bool{state.desired.iter().flatten().any(|set|set.domain==context.domain&&set.session==context.session&&set.scopes.iter().any(|scope|scope.source==context.source&&scope.owner==context.owner&&scope.grant==context.grant&&scope.config==context.config))}
 pub(super) fn current_filters(shared:&Shared,context:Context,filters:&[VideoFilter],freeze_enabled:bool)->bool{let state=shared.state.lock().unwrap_or_else(|error|error.into_inner());Self::current_filters_in(&state,context,filters,freeze_enabled)}
 pub(super) fn current_filters_in(state:&State,context:Context,filters:&[VideoFilter],freeze_enabled:bool)->bool{
  if !Self::current_in(state,context){return false;}
  if !freeze_enabled{return true;}
  if filters.iter().enumerate().filter(|(_,filter)|filter.enabled&&matches!(filter.effect,VideoEffect::Freeze{..})).any(|(index,filter)|freeze_target(context,index,filter).and_then(|target|state.freeze_timers.allows_sample(target,context.at))==Some(false)){return false;}
  filters.iter().enumerate().filter(|(_,filter)|filter.enabled&&matches!(filter.effect,VideoEffect::Freeze{..})).all(|(index,filter)|state.desired.iter().flatten().any(|set|set.domain==context.domain&&set.session==context.session&&set.scopes.iter().any(|scope|scope.source==context.source&&scope.owner==context.owner&&scope.grant==context.grant&&scope.delays.contains(&freeze_proof(index,filter,context.rows)))))
 }
 pub(super) fn current_in(state:&State,context:Context)->bool{state.desired.iter().flatten().any(|set|set.domain==context.domain&&set.session==context.session&&set.interval==context.interval&&set.rate==context.rate&&set.scopes.iter().any(|p|p.source==context.source&&p.owner==context.owner&&p.grant==context.grant&&(p.config==context.config||context.at<set.at)))}
}

#[derive(Clone)]pub(super) struct DelaySpec{index:usize,key:LaneKey,frames:usize,bytes:usize}
pub(super) struct Plan{pub admission:Admission,specs:Vec<DelaySpec>,freezes:Vec<FreezeSpec>}
fn raster(dimensions:(u32,u32))->Result<usize,FilterError>{(dimensions.0 as usize).checked_mul(dimensions.1 as usize).and_then(|n|n.checked_mul(4)).ok_or_else(||limited("Render Delay frame overflow"))}
impl FilterCache{
 /// Run delayed camera input through the same serial history owner. The
 /// detached copy is admitted before it is allocated on that owner; the
 /// original wrapper and generation guard accompany the Job until retirement.
 pub(in crate::video_filters) fn apply_delay_shared(&mut self,filters:&[VideoFilter],input:skylive_media::SharedVideoFrame,blend:SourceBlend,providers:FilterProviders,free_bytes:usize,free_work:usize,guard:AsyncCameraGuard)->Result<SharedFilterResult,FilterError>{
  #[cfg(feature="gpu-filters")]
  if self.gpu.as_ref().is_some_and(|gpu|matches!(gpu.policy,GpuFailurePolicy::RequireGpu)){return Err(FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"temporal_filter_gpu_unsupported",detail:"Render Delay and Freeze currently use CPU SDR frame history".into()});}
  if !guard.current(){return Err(stale("delayed camera generation changed before temporal admission"));}
  let context=self.temporal_context.ok_or_else(||unavailable("Render Delay requires an explicit root-owned sequential video clock"))?;
  let runtime=self.shared.clone().ok_or_else(||unavailable("Render Delay source history owner is unavailable"))?;
  let copy_bytes=input.byte_len();let copy_work=copy_bytes/4;
  let remaining_bytes=free_bytes.checked_sub(copy_bytes).ok_or_else(||limited("delayed temporal input copy exceeds byte budget"))?;
  let remaining_work=free_work.checked_sub(copy_work).ok_or_else(||limited("delayed temporal input copy exceeds work budget"))?;
  let plan=self.delay_plan(filters,&input,providers,context,remaining_bytes,remaining_work)?;
  let admission=Admission{peak:plan.admission.peak.checked_add(copy_bytes).ok_or_else(||limited("delayed temporal peak overflow"))?,
   work:plan.admission.work.checked_add(copy_work).ok_or_else(||limited("delayed temporal work overflow"))?,..plan.admission};
  let (value,_)=runtime.apply_frame(filters,&input,None,Some(guard.clone()),blend,providers,admission,remaining_bytes,remaining_work,
   Backend::from_cache(self),self.mask_epoch,self.resource_generation,self.scroll_context,self.mask_context.clone(),Some(context),None,None)?
   .ok_or_else(||limited("delayed temporal shared owner admission"))?;
  if !guard.current(){return Err(stale("delayed camera generation changed after temporal processing"));}
  let image=value.output.clone();let new_bytes=image.byte_len();
  self.bytes=self.bytes.checked_add(new_bytes).ok_or_else(||limited("delayed temporal render cache overflow"))?;
  self.texel_visits=self.texel_visits.checked_add(admission.texels).ok_or_else(||limited("delayed temporal texel overflow"))?;
  #[cfg(feature="gpu-filters")] {self.gpu_report=value.report.clone();if value.gpu_error.is_some(){self.gpu_last_error=value.gpu_error.clone();}self.gpu_calls+=value.gpu_calls;}
  // Keep the shared output/history credit for the whole composition lifetime.
  self.camera_fallback_leases.push(value.lease);
  Ok(SharedFilterResult{image:image.into(),blend:value.blend,new_bytes,pixel_stages:admission.work})
 }
 fn delay_plan(&self,filters:&[VideoFilter],input:&RgbaFrame,providers:FilterProviders,context:Context,free_bytes:usize,free_work:usize)->Result<Plan,FilterError>{
  let active:Vec<_>=filters.iter().filter(|f|f.enabled&&providers.enabled(f.plugin_id())).collect();for filter in &active{filter.validate().map_err(|e|fail(e.to_string()))?;}
  let mut dimensions=(input.width(),input.height());let mut peak=0;let mut largest=input.byte_len();let mut work=0usize;let mut texels=0u64;let mut points=Vec::new();let mut specs=Vec::new();let mut freezes=Vec::new();let proofs=delays(filters,providers.freeze,context.rows)?;
  // Both resource stores retain one decoded asset per distinct identity. Count
  // it once even when several filters use the same LUT or mask; each actual
  // filter stage is still charged independently for its pixel work below.
  let mut lut_assets=Vec::new();let mut mask_assets=Vec::new();
  for filter in &active {
   if let Some(asset)=filter.lut_asset(){if !lut_assets.contains(&asset){lut_assets.push(asset);}}
   if let Some(asset)=filter.mask_image(){if !mask_assets.contains(&asset){mask_assets.push(asset);}}
  }
  let lut_bytes=lut_assets.iter().try_fold(0usize,|sum,a|sum.checked_add(a.table_bytes())).ok_or_else(||limited("Render Delay LUT budget overflow"))?;
  let masks=mask_assets.iter().try_fold(0usize,|sum,a|sum.checked_add(a.width() as usize*a.height() as usize*4)).ok_or_else(||limited("Render Delay mask budget overflow"))?;
  let workspace=lut_assets.iter().map(|a|a.encoded_bytes() as usize+1+if a.format()==skylive_core::LutFormat::ObsPng64{8*1024*1024}else{0}).max().unwrap_or(0);
  for filter in active.iter().copied().map(Some).chain(std::iter::once(None)){
   if let Some(f)=filter{if !matches!(f.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{..}|VideoEffect::ScaleFilter{..}|VideoEffect::Scroll{..})&&f.mask_image().is_none()&&f.lut_asset().is_none(){if let Some(stage)=prepare_filter(f).1{points.push(stage);}continue;}}
   if !points.is_empty(){let p=self.person_plan_for_dimensions(dimensions,&points,free_bytes,free_work)?;work=work.checked_add(p.work).ok_or_else(||limited("Render Delay point work overflow"))?;texels=texels.checked_add(p.texels).ok_or_else(||limited("Render Delay point texels overflow"))?;peak=peak.max(p.peak);dimensions=p.final_dimensions;largest=largest.max(raster(dimensions)?);points.clear();}
   let Some(filter)=filter else{break;};
   match filter.effect{
    VideoEffect::Freeze{settings}=>{
     settings.validate_manual().map_err(|error|fail(error.to_string()))?;
     let index=filters.iter().position(|f|std::ptr::eq(f,filter)).ok_or_else(||fail("Freeze filter identity absent"))?;
     let bytes=raster(dimensions)?;
     freezes.push(FreezeSpec{index,key:FreezeKey{domain:context.domain,session:context.session,source:context.source,owner:context.owner,grant:context.grant,proof:proofs[index],width:dimensions.0,height:dimensions.1,target:freeze_target(context,index,filter),run:0}});
     let visits=if settings.mask(){skylive_media::freeze_mask_requirements(dimensions,skylive_media::FreezeMaskSettings{percent:settings.mask_percent()}).map_err(media_fail)?.pixel_visits}else{(bytes/4)as u64};
     work=work.checked_add(visits as usize).ok_or_else(||limited("Freeze work overflow"))?;
     texels=texels.checked_add(visits).ok_or_else(||limited("Freeze texel overflow"))?;
     peak=peak.max(bytes.checked_mul(if settings.mask(){2}else{1}).ok_or_else(||limited("Freeze frame peak overflow"))?);
    },
    VideoEffect::RenderDelay{delay_ms}=>{let frames=(u64::from(delay_ms)*1_000_000/context.interval)as usize;if frames==0{continue;}if frames>120{return Err(limited("Render Delay exceeds120 project slots"));}let index=filters.iter().position(|f|std::ptr::eq(f,filter)).ok_or_else(||fail("Render Delay filter identity absent"))?;
     let proof=proofs[index];let bytes=raster(dimensions)?;let capacity=bytes.checked_add(FRAME_METADATA).and_then(|n|n.checked_mul(frames+if frames>1{1}else{0})).and_then(|n|n.checked_add(1024)).ok_or_else(||limited("Render Delay history overflow"))?;
     specs.push(DelaySpec{index,key:LaneKey{domain:context.domain,session:context.session,source:context.source,owner:context.owner,grant:context.grant,proof,width:dimensions.0,height:dimensions.1,interval:context.interval,rate:context.rate},frames,bytes:capacity});
     // Actual ingress blend is set by the real preceding segment on the owner.
     work=work.checked_add(bytes/2).ok_or_else(||limited("Render Delay copy work overflow"))?;texels=texels.checked_add((bytes/2)as u64).ok_or_else(||limited("Render Delay copy texels overflow"))?;
    },
    VideoEffect::ScaleFilter{..}=>{if let Some(p)=scale::plan(filter.effect,dimensions,providers.canvas)?{let r=p.requirements();work=work.checked_add(r.pixel_visits as usize).ok_or_else(||limited("Render Delay Scale work overflow"))?;texels=texels.checked_add(r.texel_visits).ok_or_else(||limited("Render Delay Scale texels overflow"))?;peak=peak.max(r.input_bytes.checked_add(r.output_bytes).ok_or_else(||limited("Render Delay Scale peak overflow"))?);dimensions=(r.width,r.height);largest=largest.max(r.output_bytes);}},
    VideoEffect::Scroll{limit_width,limit_height,width,height,looping,..}=>{let r=skylive_media::scroll_requirements(dimensions,skylive_media::ScrollSettings{width:limit_width.then_some(width),height:limit_height.then_some(height),looping},self.scroll_context.phase(filter)).map_err(media_fail)?;work=work.checked_add(r.output_bytes/4).ok_or_else(||limited("Render Delay Scroll work overflow"))?;texels=texels.checked_add(r.texel_visits).ok_or_else(||limited("Render Delay Scroll texels overflow"))?;peak=peak.max(raster(dimensions)?.checked_add(r.output_bytes).ok_or_else(||limited("Render Delay Scroll peak overflow"))?);dimensions=(r.width,r.height);largest=largest.max(r.output_bytes);},
    _=>{},
   }
   if let Some(asset)=filter.lut_asset(){let VideoEffect::ApplyLut{amount,passthrough_alpha}=filter.effect else{return Err(fail("Render Delay invalid LUT attachment"));};let settings=skylive_media::ColorLutSettings{amount,passthrough_alpha};let r=skylive_media::color_lut_requirements(dimensions,asset.table_bytes(),settings).map_err(media_fail)?;work=work.checked_add(r.texel_visits.div_ceil(5)as usize).ok_or_else(||limited("Render Delay LUT work overflow"))?;texels=texels.checked_add(r.texel_visits).ok_or_else(||limited("Render Delay LUT texels overflow"))?;peak=peak.max(r.output_bytes.checked_mul(2).ok_or_else(||limited("Render Delay LUT peak overflow"))?);
    #[cfg(feature="gpu-filters")] {peak=peak.max(self.gpu_lut_peak(dimensions,asset.table_bytes(),settings,skylive_media::ColorLutLimits{max_input_bytes:r.input_bytes,max_output_bytes:r.output_bytes,max_texel_visits:r.texel_visits},free_bytes)?);}
   }
   if let Some(mask)=filter.mask_image(){let VideoEffect::ImageMask{method,color,opacity,stretch}=filter.effect else{return Err(fail("Render Delay invalid mask attachment"));};let r=skylive_media::image_mask_requirements(dimensions,(mask.width(),mask.height()),skylive_media::ImageMaskSettings{method:match method{skylive_core::ImageMaskMethod::MaskColor=>skylive_media::ImageMaskMethod::MaskColor,skylive_core::ImageMaskMethod::MaskAlpha=>skylive_media::ImageMaskMethod::MaskAlpha,skylive_core::ImageMaskMethod::BlendMultiply=>skylive_media::ImageMaskMethod::BlendMultiply,skylive_core::ImageMaskMethod::BlendAddition=>skylive_media::ImageMaskMethod::BlendAddition,skylive_core::ImageMaskMethod::BlendSubtraction=>skylive_media::ImageMaskMethod::BlendSubtraction},color,opacity,stretch}).map_err(media_fail)?;work=work.checked_add((r.texel_visits/5)as usize).ok_or_else(||limited("Render Delay mask work overflow"))?;texels=texels.checked_add(r.texel_visits).ok_or_else(||limited("Render Delay mask texels overflow"))?;peak=peak.max(r.output_bytes.checked_mul(2).ok_or_else(||limited("Render Delay mask peak overflow"))?);}
  }
  peak=peak.checked_add(largest).and_then(|n|n.checked_add(lut_bytes)).and_then(|n|n.checked_add(masks)).and_then(|n|n.checked_add(workspace)).ok_or_else(||limited("Render Delay chain peak overflow"))?;
  if peak>free_bytes||work>free_work||texels>MAX_TEXEL_VISITS.saturating_sub(self.texel_visits){return Err(limited("Render Delay full-chain bytes/work admission"));}
  Ok(Plan{admission:Admission{peak,work,texels,width:dimensions.0,height:dimensions.1},specs,freezes})
 }
 pub(in crate::video_filters) fn apply_delay(&mut self,filters:&[VideoFilter],input:Arc<RgbaFrame>,blend:SourceBlend,providers:FilterProviders,free_bytes:usize,free_work:usize)->Result<FilterResult,FilterError>{
  #[cfg(feature="gpu-filters")]
  if self.gpu.as_ref().is_some_and(|gpu|matches!(gpu.policy,GpuFailurePolicy::RequireGpu)){return Err(FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"temporal_filter_gpu_unsupported",detail:"Render Delay and Freeze currently use CPU SDR frame history".into()});}
  let context=self.temporal_context.ok_or_else(||unavailable("Render Delay requires an explicit root-owned sequential video clock"))?;
  let runtime=self.shared.clone().ok_or_else(||unavailable("Render Delay source history owner is unavailable"))?;
  if !filters.iter().any(|f|f.enabled&&providers.enabled(f.plugin_id())&&match f.effect{VideoEffect::Freeze{..}=>true,VideoEffect::RenderDelay{delay_ms}=>u64::from(delay_ms)*1_000_000/context.interval!=0,_=>false}){let bypassed:Vec<_>=filters.iter().filter(|f|!matches!(f.effect,VideoEffect::RenderDelay{..})).cloned().collect();return self.apply_filters(&bypassed,input,blend,providers,free_bytes,free_work);}
  let active=filters;
  let plan=match self.delay_plan(&active,&input,providers,context,free_bytes,free_work){
   Ok(plan)=>plan,
   Err(error)=>return self.freeze_prior_output(&runtime,active,input,blend,providers,context).ok_or(error),
  };
  let key=Key{input:input.pixels().as_ptr()as usize,blend,settings:Vec::new(),mask_identity:serde_json::to_vec(&(context, &active)).map_err(|e|fail(e.to_string()))?,mask_epoch:self.mask_epoch,resource_generation:self.resource_generation};
  let value=match runtime.apply(&active,&input,blend,providers,plan.admission,free_bytes,free_work,Backend::from_cache(self),self.mask_epoch,self.resource_generation,self.scroll_context,self.mask_context.clone(),Some(context),None,None){
   Ok(Some((value,_)))=>value,
   Ok(None)=>return self.freeze_prior_output(&runtime,active,input,blend,providers,context).ok_or_else(||limited("Render Delay shared owner admission")),
   Err(error)=>return self.freeze_prior_output(&runtime,active,input,blend,providers,context).ok_or(error),
  };
  self.bytes=self.bytes.checked_add(value.output.byte_len()).ok_or_else(||limited("Render Delay render cache byte overflow"))?;self.texel_visits+=plan.admission.texels;
  #[cfg(feature="gpu-filters")] {self.gpu_report=value.report.clone();if value.gpu_error.is_some(){self.gpu_last_error=value.gpu_error.clone();}self.gpu_calls+=value.gpu_calls;}
  let image=value.output.clone();let blend=value.blend;self.entries.insert(key,Entry{_input:input,output:image.clone(),blend,_shared_lease:Some(value.lease)});Ok(FilterResult{new_bytes:image.byte_len(),image,blend,pixel_stages:plan.admission.work})
 }
 fn freeze_prior_output(&mut self,runtime:&Runtime,filters:&[VideoFilter],input:Arc<RgbaFrame>,blend:SourceBlend,providers:FilterProviders,context:Context)->Option<FilterResult>{
  let value=runtime.freeze_fallback(filters,&input,blend,providers,context,&Backend::from_cache(self),self.mask_epoch,if image_mask::has_live_mask(filters){self.mask_context.as_ref().map(|(source,clock)|(source.get(),clock.session(),clock.scope(),clock.revision(),clock.files_generation(),if image_mask::has_animated_mask(filters,Some(clock)){clock.now().0}else{0}))}else{None},self.resource_generation)?;
  let key=Key{input:input.pixels().as_ptr()as usize,blend,settings:Vec::new(),mask_identity:serde_json::to_vec(&(context,filters)).ok()?,mask_epoch:self.mask_epoch,resource_generation:self.resource_generation};
  let image=value.output.clone();let output_blend=value.blend;self.entries.insert(key,Entry{_input:input,output:image.clone(),blend:output_blend,_shared_lease:Some(value.lease)});
  Some(FilterResult{new_bytes:image.byte_len(),image,blend:output_blend,pixel_stages:0})
 }
}

pub(super) fn copy_frame(input:&RgbaFrame)->Result<RgbaFrame,FilterError>{let mut bytes=Vec::new();bytes.try_reserve_exact(input.byte_len()).map_err(|_|limited("Render Delay detached frame allocation"))?;bytes.extend_from_slice(input.pixels());RgbaFrame::new(input.width(),input.height(),bytes).map_err(media_fail)}
fn blank(width:u32,height:u32)->Result<RgbaFrame,FilterError>{let size=raster((width,height))?;let mut bytes=Vec::new();bytes.try_reserve_exact(size).map_err(|_|limited("Render Delay warmup allocation"))?;bytes.resize(size,0);RgbaFrame::new(width,height,bytes).map_err(media_fail)}
impl History{
 pub(super) fn apply(&mut self,shared:&Shared,cache:&mut FilterCache,filters:&[VideoFilter],input:Arc<RgbaFrame>,blend:SourceBlend,providers:FilterProviders,context:Context,free_bytes:usize,free_work:usize)->Result<(FilterResult,Vec<Frame>),FilterError>{
  if cache.temporal_async_guard.as_ref().is_some_and(|guard|!guard.current()){return Err(stale("delayed camera generation changed before temporal history"));}
  if !Self::current(shared,context){return Err(stale("Render Delay scope changed before processing"));}
  {let state=shared.state.lock().unwrap_or_else(|e|e.into_inner());if !state.desired.iter().flatten().any(|set|set.domain==context.domain&&set.session==context.session&&set.scopes.iter().any(|scope|scope.source==context.source&&scope.owner==context.owner&&scope.grant==context.grant&&scope.config==context.config)){return Err(stale("Render Delay older configuration has no owned cached result"));}}
  self.reconcile(shared);
  if self.freeze.lanes.iter().any(|lane|lane.key.domain==context.domain&&lane.key.session==context.session&&lane.key.source==context.source&&lane.key.owner==context.owner&&lane.key.grant==context.grant&&lane.captured_tick>context.tick){return Err(stale("Freeze older request predates the owned capture"));}
  if self.lanes.values().any(|lane|lane.key.domain==context.domain&&lane.key.session==context.session&&lane.key.source==context.source&&lane.key.owner==context.owner&&lane.key.grant==context.grant&&lane.last_tick.is_some_and(|tick|tick>context.tick)){return Err(stale("Render Delay older request has no owned cached result"));}
  let mut plan=cache.delay_plan(filters,&input,providers,context,free_bytes,free_work)?;
  for spec in &mut plan.freezes{spec.key.run=spec.key.target.and_then(|target|shared.freeze_run(target)).unwrap_or(0);}
  let mut stages=[crate::freeze_refresh_plan::FreezeStage{index:0,held:false,unmasked:false,refresh_requested:false};MAX_VIDEO_FILTERS];
  let mut stage_count=0usize;
  for spec in &plan.freezes{
   let filter=&filters[spec.index];
   let requested=freeze_target(context,spec.index,filter).and_then(|target|shared.freeze_capture_plan(target)).is_some()&&self.freeze.find(&spec.key).is_some();
   stages[stage_count]=crate::freeze_refresh_plan::FreezeStage{index:spec.index,held:self.freeze.find(&spec.key).is_some(),unmasked:matches!(filter.effect,VideoEffect::Freeze{settings}if !settings.mask()),refresh_requested:context.input_complete&&requested};
   stage_count+=1;
  }
  let start=crate::freeze_refresh_plan::start(&stages[..stage_count]);
  if !context.input_complete&&plan.freezes.iter().filter(|spec|spec.index>=start).any(|spec|self.freeze.find(&spec.key).is_none()){return Err(freeze_incomplete());}
  // A held full-frame Freeze never renders its prefix. In particular do not
  // allocate, warm or advance newly authored upstream Render Delay rings.
  plan.specs.retain(|spec|spec.index>=start);
  self.lanes.retain(|key,_|!plan.specs.iter().any(|spec|key.domain==spec.key.domain&&key.session==spec.key.session&&key.source==spec.key.source&&key.owner==spec.key.owner&&key.grant==spec.key.grant&&key.proof==spec.key.proof&&(key.width!=spec.key.width||key.height!=spec.key.height)));
  let additional=plan.specs.iter().filter(|spec|!self.lanes.contains_key(&spec.key)).count();
  let needed=self.lanes.len().checked_add(additional).filter(|n|*n<=MAX_SCOPES*MAX_VIDEO_FILTERS).ok_or_else(||limited("Render Delay history count"))?;
  if needed>self.lanes.capacity(){
   // Reserve for the replacement hash table and retirement workspace while
   // the previous table remains charged. A fourfold slot envelope covers the
   // table's power-of-two growth; verify the actual capacity before release.
   let slots=needed.checked_add(1).and_then(|n|n.checked_mul(4)).ok_or_else(||limited("Render Delay table peak overflow"))?;
   let per=std::mem::size_of::<(LaneKey,Lane)>()+1;
   let bytes=slots.checked_mul(3).and_then(|n|n.checked_mul(per)).ok_or_else(||limited("Render Delay table peak overflow"))?;
   let mut replacement=Lease::acquire(&shared.budget,bytes).ok_or_else(||limited("Render Delay table/retirement metadata admission"))?;
   let mut next=HashMap::new();
   next.try_reserve(needed).map_err(|_|limited("Render Delay lane metadata allocation"))?;
   let actual=next.capacity().checked_mul(3).and_then(|n|n.checked_mul(per)).filter(|n|*n<=bytes).ok_or_else(||limited("Render Delay lane metadata capacity"))?;
   next.extend(self.lanes.drain());self.lanes=next;
   replacement.shrink(actual);self.table_credit=Some(replacement);
  }
  let mut prepared=Vec::new();prepared.try_reserve_exact(plan.specs.len()).map_err(|_|limited("Render Delay replacement metadata allocation"))?;
  // Acquire every complete new ring before copying pixels or changing a lane.
  // Reserved slot credit is split into individual frame leases. Replaced frames
  // still held by a renderer keep their own credits, never alias new-slot bytes.
  let mut reservations=Vec::new();reservations.try_reserve_exact(plan.specs.len()).map_err(|_|limited("Render Delay reservation metadata allocation"))?;for spec in &plan.specs{if self.lanes.contains_key(&spec.key){continue;}let lease=Lease::acquire(&shared.budget,spec.bytes).ok_or_else(||limited(format!("Render Delay {}x{} / {}ms / {} slots exceeds shared128MiB including active, retiring and escaped frames",spec.key.width,spec.key.height,spec.key.proof.ms,spec.frames)))?;reservations.push((spec.clone(),lease));}
  for(spec,mut reserve)in reservations{
   let inherited=if context.owner!=0&&!context.draft{Some(self.lanes.values().find(|lane|lane.key.session==context.session&&lane.key.source==context.source&&lane.key.owner==0&&lane.key.proof==spec.key.proof&&lane.key.width==spec.key.width&&lane.key.height==spec.key.height&&lane.key.interval==spec.key.interval&&lane.key.rate==spec.key.rate&&lane.binding==context.binding&&lane.last_tick.is_some_and(|tick|tick<=context.tick)).ok_or_else(||stale("Render Delay frozen view has no matching owned live history"))?)}else{None};
   let mut queue=VecDeque::new();queue.try_reserve_exact(spec.frames).map_err(|_|limited("Render Delay ring allocation"))?;
   let transparent=if spec.frames>1{let mut transparent_credit=reserve.split(raster((spec.key.width,spec.key.height))?+FRAME_METADATA)?;let recipe_credit=Arc::new(transparent_credit.split(FRAME_METADATA)?);let transparent_lease=Arc::new(transparent_credit);Some(Frame{image:Arc::new(blank(spec.key.width,spec.key.height)?),blend:SourceBlend::PremultipliedLinearSrgb,_receipt:Arc::new(Receipt::TransparentWarmup{scope:context,_credit:recipe_credit}),_lease:transparent_lease})}else{None};
   let last_tick=if let Some(old)=inherited{queue.extend(old.queue.iter().cloned());old.last_tick}else{queue.resize(spec.frames,None);None};
   prepared.push(Lane{key:spec.key,queue,transparent,last_tick,reserve,binding:context.binding});
  }
  if !prepared.is_empty(){self.lifecycle=None;}
  if cache.temporal_async_guard.as_ref().is_some_and(|guard|!guard.current()){return Err(stale("delayed camera generation changed before lane installation"));}
  for lane in prepared{self.lanes.insert(lane.key.clone(),lane);}
  let mut current=input;let mut encoding=blend;let mut segment=Vec::new();let mut retained=Vec::new();let mut spec_index=0usize;let mut work=0usize;
  for(index,filter)in filters.iter().enumerate().skip(start){
   if cache.temporal_async_guard.as_ref().is_some_and(|guard|!guard.current()){return Err(stale("delayed camera generation changed during temporal chain"));}
   if filter.enabled&&providers.freeze{if let VideoEffect::Freeze{settings}=filter.effect{
    let spec=plan.freezes.iter().find(|spec|spec.index==index).ok_or_else(||fail("Freeze admitted filter missing"))?;
    let held=self.freeze.find(&spec.key).cloned();
    let target=freeze_target(context,index,filter);
    let capture_plan=target.and_then(|target|shared.freeze_capture_plan(target));
    let refreshing=held.is_some()&&context.input_complete&&capture_plan.is_some();
    let mut prepared_mask=None;
    if held.is_none() && capture_plan.is_none() && (settings.duration_ms()!=0||settings.refresh_interval_ms()!=0){return Err(stale("Freeze timer target is not registered"));}
    if held.is_none()||settings.mask()||refreshing{
     if !segment.is_empty(){let result=cache.apply_filters(&segment,current,encoding,providers,free_bytes,free_work.saturating_sub(work))?;current=result.image;encoding=result.blend;work=work.checked_add(result.pixel_stages).ok_or_else(||limited("Freeze upstream work overflow"))?;cache.entries.clear();cache.bytes=0;}
     if (current.width(),current.height())!=(spec.key.width,spec.key.height){return Err(fail("Freeze upstream dimensions differ from admitted plan"));}
    }
    segment.clear();
    if !FreezeHistory::current(shared,&spec.key,None){self.reconcile(shared);return Err(stale("Freeze privacy scope changed during upstream processing"));}
    let selected=if refreshing{
     let target=target.ok_or_else(||fail("Freeze refresh target absent"))?;
     let capture_plan=capture_plan.ok_or_else(||fail("Freeze refresh request absent"))?;
     let mask=settings.mask().then_some((settings.mask_percent(),free_bytes,free_work.saturating_sub(work)));
     match self.freeze.capture(shared,&spec.key,&current,encoding,context,filters,index,&retained,providers,cache,Some((target,capture_plan)),mask){
      Ok((captured,mask))=>{self.lifecycle=None;prepared_mask=mask;captured},
      Err(error)=>return Err(error),
     }
    }else if let Some(held)=held{held}else{
     let planned=target.zip(capture_plan);
     let (captured,_)=self.freeze.capture(shared,&spec.key,&current,encoding,context,filters,index,&retained,providers,cache,planned,None)?;
     self.lifecycle=None;captured
    };
    if let Some(mask)=prepared_mask{
     work=work.checked_add(mask.visits as usize).ok_or_else(||limited("Freeze mask work overflow"))?;
     current=mask.image;encoding=mask.blend;
    }else if settings.mask(){
     let required=skylive_media::freeze_mask_requirements((spec.key.width,spec.key.height),skylive_media::FreezeMaskSettings{percent:settings.mask_percent()}).map_err(media_fail)?;
     if required.output_bytes>free_bytes||required.pixel_visits>free_work.saturating_sub(work)as u64{return Err(limited("Freeze live mask bytes/work admission"));}
     current=Arc::new(skylive_media::freeze_mask_rgba(&current,encoding,&selected.image,selected.blend,skylive_media::FreezeMaskSettings{percent:settings.mask_percent()},skylive_media::FreezeMaskLimits{max_input_bytes:required.input_bytes,max_output_bytes:required.output_bytes,max_pixel_visits:required.pixel_visits}).map_err(media_fail)?);
     encoding=SourceBlend::PremultipliedLinearSrgb;
     work=work.checked_add(required.pixel_visits as usize).ok_or_else(||limited("Freeze mask work overflow"))?;
    }else if !settings.mask(){current=selected.image.clone();encoding=selected.blend;}
    retained.push(selected);continue;
   }}

   let delayed=filter.enabled&&matches!(filter.effect,VideoEffect::RenderDelay{delay_ms}if u64::from(delay_ms)*1_000_000/context.interval!=0);
   if !delayed{if !matches!(filter.effect,VideoEffect::RenderDelay{..}){segment.push(filter.clone());}continue;}
   let capture_needed=self.lanes.get(&plan.specs[spec_index].key).is_none_or(|lane|lane.last_tick!=Some(context.tick));
   if !segment.is_empty()&&capture_needed{let result=cache.apply_filters(&segment,current,encoding,providers,free_bytes,free_work.saturating_sub(work))?;current=result.image;encoding=result.blend;work=work.checked_add(result.pixel_stages).ok_or_else(||limited("Render Delay executed work overflow"))?;segment.clear();cache.entries.clear();cache.bytes=0;}
   if !Self::current(shared,context){return Err(stale("Render Delay scope changed during upstream processing"));}
   segment.clear();
   let spec=&plan.specs[spec_index];spec_index+=1;let lane=self.lanes.get_mut(&spec.key).ok_or_else(||fail("Render Delay admitted lane missing"))?;
   if capture_needed&&(current.width()!=spec.key.width||current.height()!=spec.key.height){return Err(fail("Render Delay upstream dimensions differ from admitted plan"));}
   if lane.last_tick.is_some_and(|tick|context.tick<tick){return Err(stale("Render Delay cannot rewind source history"));}
   if lane.last_tick!=Some(context.tick){
    let frame_bytes=current.byte_len().checked_add(FRAME_METADATA).ok_or_else(||limited("Render Delay frame credit overflow"))?;
    // Don't release an old slot first: admission failure preserves its actual
    // pixels and time. The extra simultaneous capture is part of active peak.
    let mut credit=if lane.reserve.bytes>=frame_bytes+1024{lane.reserve.split(frame_bytes)?}else{Lease::acquire(&shared.budget,frame_bytes).ok_or_else(||limited("Render Delay new/escaped frame admission"))?};
    let recipe_credit=Arc::new(credit.split(FRAME_METADATA)?);
    let upstream=filters[..index].to_vec();let parents=retained.iter().map(|frame:&Frame|frame._receipt.clone()).collect();let detached=Arc::new(copy_frame(&current)?);
    let frame=Frame{image:detached,blend:encoding,_receipt:Arc::new(Receipt::Captured{context,upstream,parents,_credit:recipe_credit,providers,scroll:cache.scroll_context.key,resource_generation:cache.resource_generation,mask_epoch:cache.mask_epoch,mask_stamp:cache.mask_context.as_ref().map(|(source,clock)|(source.get(),clock.session(),clock.scope(),clock.revision(),clock.files_generation(),if image_mask::has_animated_mask(filters,Some(clock)){clock.now().0}else{0})),backend:Backend::from_cache(cache).signature()}),_lease:Arc::new(credit)};
    if cache.temporal_async_guard.as_ref().is_some_and(|guard|!guard.current()){return Err(stale("delayed camera generation changed before history commit"));}
    lane.queue.pop_front();lane.queue.push_back(Some(frame));lane.last_tick=Some(context.tick);
   }
   let ready=lane.queue.front().and_then(|frame|frame.clone());if ready.is_some(){lane.transparent.take();}let selected=ready.or_else(||lane.transparent.clone()).ok_or_else(||fail("Render Delay admitted output absent"))?;current=selected.image.clone();encoding=selected.blend;retained.push(selected);
  }
  if !segment.is_empty(){let result=cache.apply_filters(&segment,current,encoding,providers,free_bytes,free_work.saturating_sub(work))?;current=result.image;encoding=result.blend;let _=work.checked_add(result.pixel_stages).ok_or_else(||limited("Render Delay suffix work overflow"))?;}
  if cache.temporal_async_guard.as_ref().is_some_and(|guard|!guard.current())||!Self::current(shared,context)||plan.freezes.iter().filter(|spec|spec.index>=start).any(|spec|!FreezeHistory::current(shared,&spec.key,None)){self.reconcile(shared);return Err(stale("Render Delay or Freeze scope changed before publication"));}
  Ok((FilterResult{new_bytes:current.byte_len(),image:current,blend:encoding,pixel_stages:plan.admission.work},retained))
 }
}


// Manual Freeze shares this same temporal owner. These are detached frames,
// never native capture Arcs. The exact capture recipe is immutable with pixels.
const MAX_FREEZE_LANES:usize=128;
#[derive(Clone,PartialEq,Eq)]
struct FreezeKey{domain:u64,session:u64,source:Id,owner:u64,grant:u64,proof:DelayProof,width:u32,height:u32,target:Option<crate::freeze_timing_book::Target>,run:u64}
struct FreezeSpec{index:usize,key:FreezeKey}
#[derive(Clone)]
struct FreezeMaskFrame{image:Arc<RgbaFrame>,blend:SourceBlend,visits:u64,_lease:Arc<Lease>}
struct FreezeLane{key:FreezeKey,captured_tick:u64,frame:Frame,target:Option<crate::freeze_timing_book::Target>}
#[derive(Default)]
struct FreezeHistory{lanes:Vec<FreezeLane>,table_credit:Option<Lease>}
impl FreezeHistory{
 fn current(shared:&Shared,key:&FreezeKey,config:Option<[u8;32]>)->bool{
  let state=shared.state.lock().unwrap_or_else(|error|error.into_inner());
  Self::current_in(&state,key,config)
 }
 fn current_in(state:&State,key:&FreezeKey,config:Option<[u8;32]>)->bool{(key.run==0||key.target.is_some_and(|target|state.freeze_timers.run(target)==Some(key.run)))&&state.desired.iter().flatten().any(|set|set.domain==key.domain&&set.session==key.session&&set.scopes.iter().any(|scope|scope.source==key.source&&scope.owner==key.owner&&scope.grant==key.grant&&scope.delays.contains(&key.proof)&&config.is_none_or(|config|scope.config==config)))}
 fn find(&self,key:&FreezeKey)->Option<&Frame>{self.lanes.iter().find(|lane|lane.key==*key).map(|lane|&lane.frame)}
 fn reconcile(&mut self,shared:&Shared,desired:&[Option<Arc<ScopeSet>>;2]){
  let mut index=0;
  while index<self.lanes.len(){
   let lane=&self.lanes[index];
   let live=desired.iter().flatten().any(|set|set.domain==lane.key.domain&&set.session==lane.key.session&&set.scopes.iter().any(|scope|scope.source==lane.key.source&&scope.owner==lane.key.owner&&scope.grant==lane.key.grant&&scope.delays.contains(&lane.key.proof)))&&lane.target.is_none_or(|target|shared.freeze_effective(target)!=Some(false))&&(lane.key.run==0||lane.key.target.is_some_and(|target|shared.freeze_run(target)==Some(lane.key.run)));
   if live{index+=1;continue;}
   let retired=self.lanes.remove(index);let target=retired.target;drop(retired);
   if let Some(target)=target{if !self.lanes.iter().any(|lane|lane.target==Some(target)){shared.freeze_retire_target(target);}}
  }
  if self.lanes.is_empty(){self.lanes=Vec::new();self.table_credit=None;}
 }
 #[allow(clippy::too_many_arguments)]
 fn capture(&mut self,shared:&Shared,key:&FreezeKey,input:&RgbaFrame,blend:SourceBlend,context:Context,filters:&[VideoFilter],index:usize,retained:&[Frame],providers:FilterProviders,cache:&FilterCache,planned:Option<(crate::freeze_timing_book::Target,crate::freeze_timing_book::CapturePlan)>,refresh_mask:Option<([f32;4],usize,usize)>)->Result<(Frame,Option<FreezeMaskFrame>),FilterError>{
  if !context.input_complete{return Err(freeze_incomplete());}
  if !Self::current(shared,key,Some(context.config)){return Err(stale("Freeze source changed before first capture"));}
  if self.lanes.len()>=MAX_FREEZE_LANES&&!self.lanes.iter().any(|lane|lane.key.domain==key.domain&&lane.key.source==key.source&&lane.key.owner==key.owner&&lane.key.proof==key.proof){return Err(limited("Freeze source history count128"));}
  // Reserve old+new frame/recipe and replacement table before any allocation
  // or mutation. An escaped old picture keeps its own lease after replacement.
  let bytes=input.byte_len().checked_add(FRAME_METADATA).ok_or_else(||limited("Freeze frame credit overflow"))?;
  let mut credit=Lease::acquire(&shared.budget,bytes).ok_or_else(||limited("Freeze held/replacing/escaped frames exceed shared128MiB"))?;
  let replacement=self.lanes.iter().position(|lane|lane.key.domain==key.domain&&lane.key.session==key.session&&lane.key.source==key.source&&lane.key.owner==key.owner&&lane.key.grant==key.grant&&lane.key.proof==key.proof);
  if replacement.is_none()&&self.lanes.len()==self.lanes.capacity(){
   let capacity=self.lanes.capacity().max(2).saturating_mul(2).min(MAX_FREEZE_LANES);
   let table_bytes=capacity.checked_mul(std::mem::size_of::<FreezeLane>()).ok_or_else(||limited("Freeze metadata capacity"))?;
   let table=Lease::acquire(&shared.budget,table_bytes).ok_or_else(||limited("Freeze old/new metadata admission"))?;
   self.lanes.try_reserve_exact(capacity-self.lanes.len()).map_err(|_|limited("Freeze metadata allocation"))?;
   if self.lanes.capacity()>capacity{return Err(limited("Freeze metadata capacity"));}
   self.table_credit=Some(table);
  }
  let recipe_credit=Arc::new(credit.split(FRAME_METADATA)?);
  let upstream=filters[..index].to_vec();let parents=retained.iter().map(|frame|frame._receipt.clone()).collect();
  let frame=Frame{image:Arc::new(copy_frame(input)?),blend,_receipt:Arc::new(Receipt::Captured{context,upstream,parents,_credit:recipe_credit,providers,scroll:cache.scroll_context.key,resource_generation:cache.resource_generation,mask_epoch:cache.mask_epoch,mask_stamp:cache.mask_context.as_ref().map(|(source,clock)|(source.get(),clock.session(),clock.scope(),clock.revision(),clock.files_generation(),if image_mask::has_animated_mask(filters,Some(clock)){clock.now().0}else{0})),backend:Backend::from_cache(cache).signature()}),_lease:Arc::new(credit)};
  // Prepare the entire replacement mask before installing its held frame.
  // Failure leaves both the prior picture and its displayed mask intact.
  let candidate_mask=if let Some((percent,free_bytes,free_work))=refresh_mask{
   let settings=skylive_media::FreezeMaskSettings{percent};
   let required=skylive_media::freeze_mask_requirements((key.width,key.height),settings).map_err(media_fail)?;
   if required.output_bytes>free_bytes||required.pixel_visits>free_work as u64{return Err(limited("Freeze live mask bytes/work admission"));}
   let bytes=required.output_bytes.checked_add(std::mem::size_of::<FreezeMaskFrame>()+64).ok_or_else(||limited("Freeze mask retention overflow"))?;
   let lease=Arc::new(Lease::acquire(&shared.budget,bytes).ok_or_else(||limited("Freeze mask retained/replacing frames exceed shared128MiB"))?);
   let image=Arc::new(skylive_media::freeze_mask_rgba(input,blend,&frame.image,frame.blend,settings,skylive_media::FreezeMaskLimits{max_input_bytes:required.input_bytes,max_output_bytes:required.output_bytes,max_pixel_visits:required.pixel_visits}).map_err(media_fail)?);
   Some(FreezeMaskFrame{image,blend:SourceBlend::PremultipliedLinearSrgb,visits:required.pixel_visits,_lease:lease})
  }else{None};
  if cache.temporal_async_guard.as_ref().is_some_and(|guard|!guard.current()){return Err(stale("delayed camera generation changed before Freeze commit"));}
  let mut install=||{let lane=FreezeLane{key:key.clone(),captured_tick:context.tick,frame:frame.clone(),target:planned.map(|(target,_)|target)};if let Some(index)=replacement{Some(std::mem::replace(&mut self.lanes[index],lane))}else{self.lanes.push(lane);None}};
  let retired=if let Some((target,plan))=planned{
   shared.freeze_install(target,plan.generation,plan.run,plan.request,context.at,|state|cache.temporal_async_guard.as_ref().is_none_or(|guard|guard.current())&&Self::current_in(state,key,Some(context.config)),install).ok_or_else(||stale("Freeze source or refresh request changed while capturing"))?
  }else{
   if cache.temporal_async_guard.as_ref().is_some_and(|guard|!guard.current())||!Self::current(shared,key,Some(context.config)){return Err(stale("Freeze source changed while capturing"));}
   install()
  };
  drop(retired);Ok((frame,candidate_mask))
 }
}

fn freeze_incomplete()->FilterError{FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"freeze_input_incomplete",detail:"Freeze is waiting for complete source pixels; the current input has resource diagnostics".into()}}
