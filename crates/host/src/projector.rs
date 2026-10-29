//! Runtime-only projector plans shared by desktop and headless adapters.
//! OBS32 Projectors/Multiview GPL-2.0+; no UI/native calls in planning.
use crate::{capture::CaptureSnapshot,transition::{FrameScene,FrameComposition},playback::PlaybackSnapshot};
use skylive_core::{Session,StudioSnapshot,Id,VideoFilterPreviewTarget};
use serde::{Serialize,Deserialize};use std::{collections::HashMap,sync::Arc};
mod worker;pub use worker::{Worker,Request,Reply,Frame,TileStatus,Lease};
pub mod restoration;
mod monitors;pub use monitors::{Monitor,enumerate_monitors};
pub const MAX_WINDOWS:usize=skylive_core::MAX_SAVED_PROJECTORS;pub const MAX_VIDEO_VIEWS:usize=40;pub const MAX_ATLAS_BYTES:usize=32*1024*1024;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]pub enum Target {Preview,Program,Source {source:Id},Scene {scene:Id},Multiview}
pub use skylive_core::MultiviewLayout as Layout;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]#[serde(deny_unknown_fields)]pub struct Options {pub layout:Layout,pub labels:bool,pub safe_areas:bool,pub mouse_switching:bool,pub double_click_transition:bool}
impl Default for Options{fn default()->Self{Self{layout:Layout::Top8,labels:true,safe_areas:true,mouse_switching:true,double_click_transition:false}}}
impl Options{pub fn from_preferences(value:skylive_core::ProjectorPreferences)->Self{Self{layout:value.layout(),labels:value.labels(),safe_areas:value.safe_areas(),mouse_switching:value.mouse_switching(),double_click_transition:value.double_click_transition()}}}
impl Target{pub fn saved(self)->skylive_core::SavedProjectorTarget{use skylive_core::SavedProjectorTarget as S;match self{Self::Preview=>S::Preview,Self::Program=>S::Program,Self::Source{source}=>S::Source{source},Self::Scene{scene}=>S::Scene{scene},Self::Multiview=>S::Multiview}}pub fn from_saved(value:skylive_core::SavedProjectorTarget)->Self{use skylive_core::SavedProjectorTarget as S;match value{S::Preview=>Self::Preview,S::Program=>Self::Program,S::Source{source}=>Self::Source{source},S::Scene{scene}=>Self::Scene{scene},S::Multiview=>Self::Multiview}}}
impl Options{pub fn saved(self)->skylive_core::SavedProjectorOptions{skylive_core::SavedProjectorOptions{layout:self.layout,labels:self.labels,safe_areas:self.safe_areas,mouse_switching:self.mouse_switching,double_click_transition:self.double_click_transition}}pub fn from_saved(value:skylive_core::SavedProjectorOptions)->Self{Self{layout:value.layout,labels:value.labels,safe_areas:value.safe_areas,mouse_switching:value.mouse_switching,double_click_transition:value.double_click_transition}}}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]#[serde(deny_unknown_fields)]pub struct Spec {pub id:u64,pub epoch:u64,pub session:u64,pub target:Target,pub options:Options,pub width:u32,pub height:u32}
#[derive(Clone,Copy,Debug,Serialize,PartialEq,Eq)]#[serde(rename_all="snake_case")]pub enum Role {Preview,Program,Scene,Source}
#[derive(Clone,Copy,Debug,Serialize)]pub struct Rect {pub x:f32,pub y:f32,pub width:f32,pub height:f32}
#[derive(Clone)]pub(crate) struct Tile {pub rect:Rect,pub role:Role,pub scene:Option<Id>,pub label:String,pub output:FrameComposition,pub basis:(u32,u32)}
#[derive(Clone)]pub struct Plan {pub spec:Spec,pub revision:u64,pub(crate) tiles:Vec<Tile>,_metadata:Arc<crate::transition::ProjectionMetadata>}
#[derive(Clone,Debug,Serialize)]pub struct Snapshot {pub spec:Spec,pub revision:u64,pub tiles:Vec<TileSnapshot>}
#[derive(Clone,Debug,Serialize)]pub struct TileSnapshot {pub rect:Rect,pub role:Role,pub scene:Option<Id>,pub label:String,pub width:u32,pub height:u32}
impl Plan {pub fn tiles_key(&self)->Vec<crate::transition::OutputKey>{self.tiles.iter().map(|tile|tile.output.identity()).collect()}pub fn snapshot(&self)->Snapshot{Snapshot{spec:self.spec,revision:self.revision,tiles:self.tiles.iter().map(|t|TileSnapshot{rect:t.rect,role:t.role,scene:t.scene,label:t.label.clone(),width:t.basis.0,height:t.basis.1}).collect()}}pub fn views(&self)->Vec<FrameScene>{let mut result=Vec::new();for t in &self.tiles{for view in t.output.scenes(){if !result.iter().any(|old:&FrameScene|old.key()==view.key()){result.push(view);}}}result}}
#[derive(Debug)]pub enum Error {Invalid(&'static str),Busy,Closed,Backend(String)}
impl std::fmt::Display for Error{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{write!(f,"Projector: {self:?}")}}impl std::error::Error for Error{}
#[derive(Default)]pub struct Planner {session:u64,revision:u64,sources:HashMap<(bool,Id,u32,u32),FrameScene>,metadata:Option<Arc<crate::transition::ProjectionMetadata>>}
impl Planner {
 pub fn prepare(&mut self,session:&Session,spec:Spec,program:&FrameComposition,studio:bool,capture:&CaptureSnapshot)->Result<Plan,Error>{let model=session.snapshot();self.prepare_inner(session.identity(),&model,Some(model.clone()),session.preview(),spec,program,studio,capture)}
 pub fn prepare_snapshot(&mut self,session:u64,model:&StudioSnapshot,preview_scene:Id,spec:Spec,program:&FrameComposition,studio:bool,capture:&CaptureSnapshot)->Result<Plan,Error>{
  self.prepare_inner(session,model,None,preview_scene,spec,program,studio,capture)
 }
 fn prepare_inner(&mut self,session:u64,model:&StudioSnapshot,shared:Option<Arc<StudioSnapshot>>,preview_scene:Id,spec:Spec,program:&FrameComposition,studio:bool,capture:&CaptureSnapshot)->Result<Plan,Error>{
  if session==0||spec.id==0||spec.epoch==0||spec.session!=session||capture.session()!=session||spec.width==0||spec.height==0||spec.width>1920||spec.height>1080{return Err(Error::Invalid("projector spec/session/dimensions"));}
  if self.session!=session||self.revision!=model.revision(){self.sources.clear();self.metadata=None;self.session=session;self.revision=model.revision();}
  if self.metadata.is_none(){self.metadata=Some(match shared{Some(model)=>crate::transition::ProjectionMetadata::shared(session,model),None=>crate::transition::ProjectionMetadata::borrowed(session,model)}.map_err(|error|Error::Backend(error.to_string()))?);}
  let metadata=self.metadata.as_ref().ok_or(Error::Invalid("projector metadata pool"))?.clone();
  let canvas=(model.video().width(),model.video().height());let preview=if studio{FrameComposition::Scene(FrameScene::Live(preview_scene))}else{program.clone()};
  let scene_basis=|scene:Id|model.scene_source(scene).map_or(Ok(canvas),|source|model.effective_filtered_video_dimensions(source,canvas,Some(canvas))).map_err(|_|Error::Invalid("projector filtered scene dimensions"));
  let mut tile=|target:Target,rect:Rect|->Result<Tile,Error>{let(output,role,scene,label,basis)=match target{
   Target::Preview=>(preview.clone(),Role::Preview,Some(preview.primary_scene()),"Preview".to_owned(),canvas),Target::Program=>(program.clone(),Role::Program,Some(program.primary_scene()),"Program".to_owned(),canvas),
   Target::Scene{scene}=>{let name=model.scene(scene).ok_or(Error::Invalid("projector scene removed"))?.name().to_owned();let basis=scene_basis(scene)?;let key=(true,scene,basis.0,basis.1);let scope=if let Some(view)=self.sources.get(&key){view.clone()}else{if self.sources.len()>=29{self.sources.clear();}let view=metadata.preview(VideoFilterPreviewTarget::Scene(scene),basis).map_err(|error|Error::Backend(error.to_string()))?;self.sources.insert(key,view.clone());view};(FrameComposition::Scene(scope),Role::Scene,Some(scene),name,basis)},
   Target::Source{source}=>{let value=model.source(source).ok_or(Error::Invalid("projector source removed"))?;let raw=capture.frame(source).filter(|f|f.binding.matches_source(value.kind())).map(|f|(f.image.width(),f.image.height())).or(model.source_video_dimensions(value.kind()).map_err(|_|Error::Invalid("projector source dimensions"))?).ok_or(Error::Invalid("audio-only source has no video projector"))?;let basis=model.effective_filtered_video_dimensions(value,raw,Some(canvas)).map_err(|_|Error::Invalid("projector source filtered dimensions"))?;
    let key=(false,source,basis.0,basis.1);let scope=if let Some(v)=self.sources.get(&key){v.clone()}else{if self.sources.len()>=29{self.sources.clear();}let v=metadata.preview(VideoFilterPreviewTarget::Source(source),basis).map_err(|e|Error::Backend(e.to_string()))?;self.sources.insert(key,v.clone());v};
    (FrameComposition::Scene(scope),Role::Source,None,value.name().to_owned(),basis)},Target::Multiview=>return Err(Error::Invalid("nested Multiview"))};Ok(Tile{rect,role,scene,label,output,basis})};
  let mut tiles=Vec::new();let full=Rect{x:0.0,y:0.0,width:1.0,height:1.0};
  if spec.target!=Target::Multiview{tiles.push(tile(spec.target,full)?);}else{
   let layout=spec.options.layout;if !layout.scenes_only(){let(pvw,prg)=match layout{Layout::Bottom8=>(Rect{x:0.0,y:0.5,width:0.5,height:0.5},Rect{x:0.5,y:0.5,width:0.5,height:0.5}),Layout::Left8=>(Rect{x:0.0,y:0.5,width:0.5,height:0.5},Rect{x:0.0,y:0.0,width:0.5,height:0.5}),Layout::Right8=>(Rect{x:0.5,y:0.5,width:0.5,height:0.5},Rect{x:0.5,y:0.0,width:0.5,height:0.5}),Layout::Top24=>(Rect{x:1.0/6.0,y:0.0,width:1.0/3.0,height:1.0/3.0},Rect{x:0.5,y:0.0,width:1.0/3.0,height:1.0/3.0}),_=>(Rect{x:0.0,y:0.0,width:0.5,height:0.5},Rect{x:0.5,y:0.0,width:0.5,height:0.5})};tiles.push(tile(Target::Preview,pvw)?);tiles.push(tile(Target::Program,prg)?);}
   for(index,scene)in model.scenes().iter().filter(|scene|scene.show_in_multiview()).take(layout.count()).enumerate(){let r=match layout{
    Layout::Top18=>Rect{x:(index%6)as f32/6.0,y:0.5+(index/6)as f32/6.0,width:1.0/6.0,height:1.0/6.0},Layout::Top24=>Rect{x:(index%6)as f32/6.0,y:1.0/3.0+(index/6)as f32/6.0,width:1.0/6.0,height:1.0/6.0},Layout::Left8|Layout::Right8=>Rect{x:(if layout==Layout::Left8{0.5}else{0.0})+(index%2)as f32*0.25,y:(index/2)as f32*0.25,width:0.25,height:0.25},Layout::Bottom8=>Rect{x:(index%4)as f32*0.25,y:(index/4)as f32*0.25,width:0.25,height:0.25},Layout::Top8=>Rect{x:(index%4)as f32*0.25,y:0.5+(index/4)as f32*0.25,width:0.25,height:0.25},_=>{let n=match layout{Layout::Scenes4=>2,Layout::Scenes9=>3,Layout::Scenes16=>4,_=>5};Rect{x:(index%n)as f32/n as f32,y:(index/n)as f32/n as f32,width:1.0/n as f32,height:1.0/n as f32}}};tiles.push(tile(Target::Scene{scene:scene.id()},r)?);}
  }if spec.target==Target::Multiview{let scale=(spec.width as f32/canvas.0 as f32).min(spec.height as f32/canvas.1 as f32);let gx=canvas.0 as f32*scale/spec.width as f32;let gy=canvas.1 as f32*scale/spec.height as f32;for tile in &mut tiles{tile.rect.x=(1.0-gx)*0.5+tile.rect.x*gx;tile.rect.y=(1.0-gy)*0.5+tile.rect.y*gy;tile.rect.width*=gx;tile.rect.height*=gy;}}Ok(Plan{spec,revision:model.revision(),tiles,_metadata:metadata})
 }
}
