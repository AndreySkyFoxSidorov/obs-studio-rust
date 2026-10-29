//! Explicit per-source/filter mask timeline, sampled from the same media time
//! as the playback snapshot, with separate stable ownership for editor drafts.
use crate::transition::FrameScene;
use skylive_core::{Id,ImageAsset,StudioSnapshot,VideoFilter};
use skylive_media::MediaTime;
use std::{collections::HashMap,sync::Arc};
const MAX_LANES:usize=3*skylive_core::MAX_SOURCES*skylive_core::MAX_VIDEO_FILTERS;
const MAX_METADATA:usize=512*1024;
#[derive(Clone,Debug,Hash,PartialEq,Eq)]
struct Key{source:Id,filter:u64,asset:ImageAsset}
#[derive(Clone,Debug,Default)]
pub(crate) struct Snapshot{session:u64,scope:u64,revision:u64,now:MediaTime,starts:Option<Arc<HashMap<Key,MediaTime>>>,pub(crate) files:Option<Arc<crate::mask_live_versions::Sample>>,pub(crate) files_domain:u64,_permit:Option<Arc<crate::playback::MetadataPermit>>}
impl Snapshot{
 pub(crate) fn files_bytes(&self)->usize{self.files.as_ref().map_or(0,|files|files.bytes())}
 pub(crate) fn files_generation(&self)->u64{self.files.as_ref().map_or(0,|files|files.generation())}
 pub(crate) fn with_files(mut self,files:Option<Arc<crate::mask_live_versions::Sample>>,domain:u64)->Self{self.files=files;self.files_domain=domain;self}
 pub(crate) fn same_files(&self,old:&Self)->bool{match (&self.files,&old.files){(None,None)=>true,(Some(a),Some(b))=>a.same_versions(b),_=>false}}
 pub fn elapsed(&self,source:Id,filter:&VideoFilter)->Option<MediaTime>{
  let asset=filter.mask_image()?;let key=Key{source,filter:filter.id()?.get(),asset:asset.clone()};
  let start=self.starts.as_ref()?.get(&key)?;Some(MediaTime(self.now.0.checked_sub(start.0)?))
 }
 pub fn session(&self)->u64{self.session}
 pub fn scope(&self)->u64{self.scope}
 pub fn revision(&self)->u64{self.revision}
 pub fn now(&self)->MediaTime{self.now}
 pub fn reusable_from(&self,old:&Self)->bool{
  if self.starts.as_ref().is_none_or(|s|s.is_empty())&&old.starts.as_ref().is_none_or(|s|s.is_empty()){return true;}
  if self.session!=old.session||self.scope!=old.scope||self.revision!=old.revision||self.starts!=old.starts{return false;}
  if self.now==old.now{return true;}
  // A candidate PNG/WebP is no longer time-varying once its exact live
  // revision was decoded as a static clip. Unknown revisions stay conservative.
  self.same_files(old)&&self.starts.as_ref().is_some_and(|starts|starts.keys().all(|key|
   self.files.as_ref().and_then(|files|files.version(key.asset.path())).is_some_and(|revision|!revision.may_animate())))
 }
 pub fn cold(session:u64,model:&StudioSnapshot,now:MediaTime)->Self{let mut clock=Clock::default();clock.sample(session,model,&[],MediaTime(0)).unwrap_or_default().at(now)}
 pub(crate) fn at(mut self,now:MediaTime)->Self{self.now=now;self}
}
#[derive(Clone,Default)]
pub(crate) struct Clock{session:u64,revision:u64,last:Option<MediaTime>,starts:Option<Arc<HashMap<Key,MediaTime>>>,_permit:Option<Arc<crate::playback::MetadataPermit>>}
impl Clock{
 pub fn reset(&mut self){self.session=0;self.last=None;self.starts=None;self._permit=None;}
 pub fn sample(&mut self,session:u64,model:&StudioSnapshot,views:&[FrameScene],now:MediaTime)->Result<Snapshot,&'static str>{
  let reset=self.session!=session||self.last.is_some_and(|last|now<last);
  let mut estimate=128usize;
  let mut estimate_source=|source:&skylive_core::Source|->Result<(),&'static str>{for filter in source.video_filters(){if let Some(asset)=filter.mask_image().filter(|asset|crate::assets::mask_animation_candidate(asset.path())){estimate=estimate.checked_add(asset.path().len()+std::mem::size_of::<(Key,MediaTime)>()+128).filter(|n|*n<=MAX_METADATA).ok_or("mask timeline metadata limit")?;}}Ok(())};
  for source in model.sources(){estimate_source(source)?;}
  for view in views{if let Some(source)=view.root_scene_source(model){estimate_source(source)?;}for source in view.visible_sources(model).map_err(|_|"mask timeline scene unavailable")?{estimate_source(source)?;}}
  // No animated assets in the canonical model or retained transition views:
  // retire this clock's ownership, but existing snapshots retain their own
  // Arc and credit. Keep session/time/revision transitions exactly as before.
  if estimate==128{
   if reset||self.starts.as_ref().is_some_and(|s|!s.is_empty()){self.revision=self.revision.wrapping_add(1);}
   self.session=session;self.last=Some(now);self.starts=None;self._permit=None;
   return Ok(Snapshot{session,scope:0,revision:self.revision,now,starts:None,files:None,files_domain:0,_permit:None});
  }
  let permit=Arc::new(crate::playback::MetadataPermit::acquire(estimate).map_err(|_|"mask timeline metadata budget")?);
  let mut starts=HashMap::new();let mut bytes=0usize;
  let mut include=|source:&skylive_core::Source|->Result<(),&'static str>{
   for filter in source.video_filters(){let Some(asset)=filter.mask_image().filter(|asset|crate::assets::mask_animation_candidate(asset.path())) else{continue;};let Some(id)=filter.id()else{continue;};
    let key=Key{source:source.id(),filter:id.get(),asset:asset.clone()};if starts.contains_key(&key){continue;}
    bytes=bytes.checked_add(asset.path().len()+std::mem::size_of::<(Key,MediaTime)>()+128).filter(|n|*n<=MAX_METADATA).ok_or("mask timeline metadata limit")?;
    if starts.len()>=MAX_LANES{return Err("mask timeline lane limit");}
    let start=if reset{now}else{self.starts.as_ref().and_then(|s|s.get(&key)).copied().unwrap_or(now)};starts.insert(key,start);
   }Ok(())
  };
  // Canonical filters tick while hidden; retained transition views may carry
  // a different historical asset under the same source/filter identity.
  for source in model.sources(){include(source)?;}
  for view in views{if let Some(source)=view.root_scene_source(model){include(source)?;}for source in view.visible_sources(model).map_err(|_|"mask timeline scene unavailable")?{include(source)?;}}
  if reset||self.starts.as_deref().map_or(!starts.is_empty(),|old|*old!=starts){self.revision=self.revision.wrapping_add(1);}
  let starts=Arc::new(starts);let snapshot=Snapshot{session,scope:0,revision:self.revision,now,starts:Some(starts.clone()),files:None,files_domain:0,_permit:Some(permit.clone())};self.session=session;self.last=Some(now);self.starts=Some(starts);self._permit=Some(permit);Ok(snapshot)
 }
}

#[derive(Clone,Debug,Hash,PartialEq,Eq)]
struct DraftKey{window:u64,source:Id,row:u32,asset:ImageAsset}
/// Dialog-owned state. It never mutates the canonical Program timeline.
#[derive(Default)]
pub struct DraftClock{files_pin:Option<Arc<crate::mask_live_versions::Sample>>,session:u64,revision:u64,base_revision:u64,projection:Vec<(u32,u64)>,last:Option<MediaTime>,starts:HashMap<DraftKey,MediaTime>,_permit:Option<Arc<crate::playback::MetadataPermit>>}
impl DraftClock{
 pub(crate) fn hold_files(&mut self,files:Option<Arc<crate::mask_live_versions::Sample>>){self.files_pin=files;}
 pub fn reset(&mut self){self.files_pin=None;self.session=0;self.last=None;self.starts=HashMap::new();self.projection=Vec::new();self._permit=None;}
 pub(crate) fn sample(&mut self,base:&Snapshot,source:&skylive_core::Source,draft_owner:Id,window:u64,rows:&[u32],now:MediaTime)->Result<Snapshot,&'static str>{
  if window==0||rows.len()!=source.video_filters().len()||rows.len()>skylive_core::MAX_VIDEO_FILTERS||rows.iter().enumerate().any(|(i,row)|*row==0||rows[..i].contains(row)){return Err("invalid mask draft row identity");}
  let reset=self.session!=base.session||self.last.is_some_and(|old|now<old);
  // Fast path must also retire a formerly populated draft. A later Add then
  // creates a fresh lane at its actual media time rather than reviving it.
  let draft_has_animation_candidate=source.video_filters().iter().any(|filter|filter.mask_image().is_some_and(|asset|crate::assets::mask_animation_candidate(asset.path())));
  if !draft_has_animation_candidate&&base.starts.as_ref().is_none_or(|s|s.is_empty()){
   if reset||!self.starts.is_empty()||!self.projection.is_empty()||self.base_revision!=base.revision{self.revision=self.revision.wrapping_add(1);}
   self.session=base.session;self.base_revision=base.revision;self.last=Some(now);
   self.starts=HashMap::new();self.projection=Vec::new();self._permit=None;
   return Ok(Snapshot{session:base.session,scope:window,revision:self.revision,now,starts:None,files:base.files.clone(),files_domain:base.files_domain,_permit:None});
  }
  let mut charge=128usize.checked_add(rows.len()*2*std::mem::size_of::<(u32,u64)>()).ok_or("mask draft metadata limit")?;
  for key in base.starts.iter().flat_map(|starts|starts.keys()){charge=charge.checked_add(key.asset.path().len()+std::mem::size_of::<(Key,MediaTime)>()+128).ok_or("mask draft metadata limit")?;}
  for filter in source.video_filters(){if let Some(asset)=filter.mask_image().filter(|asset|crate::assets::mask_animation_candidate(asset.path())){charge=charge.checked_add(2*asset.path().len()+std::mem::size_of::<(Key,MediaTime)>()+std::mem::size_of::<(DraftKey,MediaTime)>()+256).ok_or("mask draft metadata limit")?;}}
  if charge>2*MAX_METADATA{return Err("mask draft metadata limit");}
  let permit=Arc::new(crate::playback::MetadataPermit::acquire(charge).map_err(|_|"mask draft metadata budget")?);
  let mut overlay=base.starts.as_deref().cloned().unwrap_or_default();let mut starts=HashMap::new();let mut projection=Vec::new();
  for (filter,row) in source.video_filters().iter().zip(rows){
   let Some(asset)=filter.mask_image().filter(|asset|crate::assets::mask_animation_candidate(asset.path()))else{continue;};
   let id=filter.id().ok_or("mask draft filter identity unavailable")?.get();
   let draft=DraftKey{window,source:draft_owner,row:*row,asset:asset.clone()};
   projection.push((*row,id));
   let start=if reset{now}else{self.starts.get(&draft).copied().unwrap_or(now)};
   overlay.insert(Key{source:source.id(),filter:id,asset:asset.clone()},start);starts.insert(draft,start);
  }
  if overlay.len()>MAX_LANES{return Err("mask draft lane limit");}
  if reset||self.starts!=starts||self.projection!=projection||self.base_revision!=base.revision{self.revision=self.revision.wrapping_add(1);}
  self.session=base.session;self.base_revision=base.revision;self.projection=projection;self.last=Some(now);self.starts=starts;self._permit=Some(permit.clone());
  Ok(Snapshot{session:base.session,scope:window,revision:self.revision,now,starts:Some(Arc::new(overlay)),files:base.files.clone(),files_domain:base.files_domain,_permit:Some(permit)})
 }
}
