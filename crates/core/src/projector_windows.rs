//! Bounded native-collection restore intents. They never open an OS window.
use crate::{Error,Id,MultiviewLayout};use serde::{Serialize,Deserialize};
pub const MAX_SAVED_PROJECTORS:usize=4;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum SavedProjectorTarget{Preview,Program,Source{source:Id},Scene{scene:Id},Multiview}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedProjectorOptions{pub layout:MultiviewLayout,pub labels:bool,pub safe_areas:bool,pub mouse_switching:bool,pub double_click_transition:bool}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="GeometryData")]
pub struct ProjectorWindowGeometry{x:i32,y:i32,width:u32,height:u32}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct GeometryData{x:i32,y:i32,width:u32,height:u32}
impl TryFrom<GeometryData> for ProjectorWindowGeometry{type Error=Error;fn try_from(value:GeometryData)->Result<Self,Error>{Self::new(value.x,value.y,value.width,value.height)}}
impl ProjectorWindowGeometry{
 pub fn new(x:i32,y:i32,width:u32,height:u32)->Result<Self,Error>{if x.unsigned_abs()>1_000_000||y.unsigned_abs()>1_000_000||!(64..=16384).contains(&width)||!(64..=16384).contains(&height){return Err(Error::Invalid("saved projector physical geometry exceeds supported bounds"));}Ok(Self{x,y,width,height})}
 pub fn x(self)->i32{self.x}pub fn y(self)->i32{self.y}pub fn width(self)->u32{self.width}pub fn height(self)->u32{self.height}
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="WindowData")]
pub struct SavedProjectorWindow{target:SavedProjectorTarget,monitor:Option<String>,geometry:Option<ProjectorWindowGeometry>,always_on_top:Option<bool>,hide_cursor:Option<bool>,options:Option<SavedProjectorOptions>}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct WindowData{target:SavedProjectorTarget,#[serde(default)]monitor:Option<String>,#[serde(default)]geometry:Option<ProjectorWindowGeometry>,#[serde(default)]always_on_top:Option<bool>,#[serde(default)]hide_cursor:Option<bool>,#[serde(default)]options:Option<SavedProjectorOptions>}
impl TryFrom<WindowData> for SavedProjectorWindow{type Error=Error;fn try_from(value:WindowData)->Result<Self,Error>{Self::new(value.target,value.monitor,value.geometry,value.always_on_top,value.hide_cursor,value.options)}}
impl SavedProjectorWindow{
 pub fn new(target:SavedProjectorTarget,monitor:Option<String>,geometry:Option<ProjectorWindowGeometry>,always_on_top:Option<bool>,hide_cursor:Option<bool>,options:Option<SavedProjectorOptions>)->Result<Self,Error>{
  if matches!(target,SavedProjectorTarget::Source{source}|SavedProjectorTarget::Scene{scene:source}if source.get()==0){return Err(Error::Invalid("saved projector target requires nonzero collection-local identity"));}
  if monitor.as_ref().is_some_and(|id|id.is_empty()||id.len()>4096||id.chars().any(char::is_control)){return Err(Error::Invalid("saved projector monitor identity requires1..4096 UTF-8 bytes without controls"));}
  Ok(Self{target,monitor:monitor.map(|value|value.into_boxed_str().into_string()),geometry,always_on_top,hide_cursor,options})
 }
 pub fn target(&self)->SavedProjectorTarget{self.target}pub fn monitor(&self)->Option<&str>{self.monitor.as_deref()}pub fn geometry(&self)->Option<ProjectorWindowGeometry>{self.geometry}pub fn always_on_top(&self)->Option<bool>{self.always_on_top}pub fn hide_cursor(&self)->Option<bool>{self.hide_cursor}pub fn options(&self)->Option<SavedProjectorOptions>{self.options}
}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize)]#[serde(transparent)]pub struct SavedProjectorWindows(Vec<SavedProjectorWindow>);
impl SavedProjectorWindows{
 pub fn new(values:Vec<SavedProjectorWindow>)->Result<Self,Error>{if values.len()>MAX_SAVED_PROJECTORS||values.iter().filter(|value|value.target==SavedProjectorTarget::Multiview).count()>1{return Err(Error::Limit("saved projectors support four windows including one Multiview"));}Ok(Self(values.into_boxed_slice().into_vec()))}
 pub fn entries(&self)->&[SavedProjectorWindow]{&self.0}pub fn is_empty(&self)->bool{self.0.is_empty()}
}
impl<'de> Deserialize<'de> for SavedProjectorWindows{fn deserialize<D:serde::Deserializer<'de>>(deserializer:D)->Result<Self,D::Error>{struct Rows;impl<'de> serde::de::Visitor<'de> for Rows{type Value=SavedProjectorWindows;fn expecting(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str("at most four saved projector windows") }fn visit_seq<A:serde::de::SeqAccess<'de>>(self,mut seq:A)->Result<Self::Value,A::Error>{if seq.size_hint().is_some_and(|count|count>MAX_SAVED_PROJECTORS){return Err(serde::de::Error::custom("saved projector count exceeds4"));}let mut rows=Vec::with_capacity(MAX_SAVED_PROJECTORS);while let Some(row)=seq.next_element()?{if rows.len()==MAX_SAVED_PROJECTORS{return Err(serde::de::Error::custom("saved projector count exceeds4"));}rows.push(row);}SavedProjectorWindows::new(rows).map_err(serde::de::Error::custom)}}deserializer.deserialize_seq(Rows)}}
