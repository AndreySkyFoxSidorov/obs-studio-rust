//! Projector-only shared immutable metadata. No native/pixel work.
use super::*;use std::{io::Write,sync::atomic::{AtomicUsize,Ordering}};
const BYTES:usize=64*1024*1024;const MAX_VIEWS:usize=96;
static USED:AtomicUsize=AtomicUsize::new(0);static VIEWS:AtomicUsize=AtomicUsize::new(0);
struct Credit{bytes:usize,view:bool}
impl Credit{fn reserve(bytes:usize,view:bool)->Result<Self,TransitionError>{USED.fetch_update(Ordering::AcqRel,Ordering::Acquire,|old|old.checked_add(bytes).filter(|sum|*sum<=BYTES)).map_err(|_|TransitionError::Limit("projector combined retained metadata64MiB"))?;if view&&VIEWS.fetch_update(Ordering::AcqRel,Ordering::Acquire,|old|(old<MAX_VIEWS).then_some(old+1)).is_err(){USED.fetch_sub(bytes,Ordering::AcqRel);return Err(TransitionError::Limit("projector current/retiring view metadata96 instances"));}Ok(Self{bytes,view})}}
impl Drop for Credit{fn drop(&mut self){if self.view{VIEWS.fetch_sub(1,Ordering::AcqRel);}USED.fetch_sub(self.bytes,Ordering::AcqRel);}}
struct Count(usize);impl Write for Count{fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{self.0=self.0.checked_add(bytes.len()).filter(|total|*total<=12*1024*1024).ok_or_else(||std::io::Error::other("projector serialized metadata limit12MiB"))?;Ok(bytes.len())}fn flush(&mut self)->std::io::Result<()>{Ok(())}}
fn admit(model:&StudioSnapshot)->Result<Credit,TransitionError>{
 let mut count=Count(0);serde_json::to_writer(&mut count,model).map_err(|_|TransitionError::Limit("projector metadata preflight12MiB"))?;
 // Count BEFORE copying. Four wire bytes per encoded byte cover owned String/
 // Vec slack; explicit fixed structural charge covers map buckets, Scene/Source
 // values, item vectors, IDs/Arcs and one isolated-layout construction scratch.
 let structural=8*skylive_core::MAX_SOURCES*std::mem::size_of::<Source>()
  +8*(skylive_core::MAX_SCENES+skylive_core::MAX_SOURCES)*std::mem::size_of::<Scene>()
  +4*skylive_core::MAX_ITEMS*std::mem::size_of::<SceneItem>()
  +1024*(skylive_core::MAX_SCENES+skylive_core::MAX_SOURCES)+64*1024;
 let bytes=count.0.checked_mul(4).and_then(|n|n.checked_add(structural)).ok_or(TransitionError::Limit("projector metadata charge overflow"))?;Credit::reserve(bytes,false)
}
#[derive(Debug)]pub(crate) struct ProjectionMetadata{pub(super) session:u64,pub(super) model:Arc<StudioSnapshot>,pub(super) layouts:HashMap<Id,Arc<Scene>>,pub(super) dimensions:HashMap<Id,(u32,u32)>,_credit:Credit}
impl std::fmt::Debug for Credit{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.debug_struct("ProjectorMetadataCredit").field("bytes",&self.bytes).finish()}}
#[derive(Debug)]pub(super) struct View{pub(super) metadata:Arc<ProjectionMetadata>,_credit:Credit}
impl ProjectionMetadata {
 pub(crate) fn shared(session:u64,model:Arc<StudioSnapshot>)->Result<Arc<Self>,TransitionError>{let credit=admit(&model)?;Self::build(session,model,credit)}
 pub(crate) fn borrowed(session:u64,model:&StudioSnapshot)->Result<Arc<Self>,TransitionError>{let credit=admit(model)?;Self::build(session,Arc::new(model.clone()),credit)}
 fn build(session:u64,model:Arc<StudioSnapshot>,credit:Credit)->Result<Arc<Self>,TransitionError>{
  let layouts=model.scenes().iter().chain(model.groups()).map(|scene|(scene.id(),Arc::new(scene.clone()))).collect();let mut dimensions=HashMap::new();
  for source in model.sources(){if let Some(size)=model.source_video_dimensions(source.kind())?{dimensions.insert(source.id(),size);}}
  Ok(Arc::new(Self{session,model,layouts,dimensions,_credit:credit}))
 }
 pub(crate) fn preview(self:&Arc<Self>,target:skylive_core::VideoFilterPreviewTarget,dimensions:(u32,u32))->Result<FrameScene,TransitionError>{
  // The lease covers this immutable instance, bounded layout plus IDs/maps; it
  // follows every Arc clone in cache/request/retiring media resource owners.
  let credit=Credit::reserve(64*1024,true)?;let model=&self.model;
  let(layout,source)=match target{skylive_core::VideoFilterPreviewTarget::Scene(scene)=>(self.layouts.get(&scene).ok_or(TransitionError::NotFound(scene))?.clone(),false),skylive_core::VideoFilterPreviewTarget::Source(source)=>(Arc::new(model.isolated_video_source_layout(source)?),true)};
  let projection=Arc::new(View{metadata:self.clone(),_credit:credit});
  Ok(FrameScene::Frozen(Arc::new(SceneInstance{session:self.session,instance:issue_instance(&NEXT_INSTANCE)?,layout,root_source:None,retained:HashMap::new(),nested:HashMap::new(),projection:Some(projection),closure_error:None,capture_dimensions:HashMap::new(),refreshed_revision:model.revision(),preview_layout:true,normalized_group_preview:false,filter_preview:true,auto_text_draft:Vec::new(),auto_authored_root:None,preview_canvas:source.then_some(dimensions),filter_canvas:(model.video().width(),model.video().height())})))
 }
}
