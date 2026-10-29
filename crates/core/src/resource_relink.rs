//! Pure atomic resource rebinding; no filesystem operations.
use crate::{Error,Id,ImageAsset,LutAsset,MediaSource,SlideShowEntry,SourceKind,StudioSnapshot,VideoFilterId};
use serde::{Deserialize,Serialize};
pub const MAX_RESOURCE_RELINKS:usize=64;
pub const MAX_RELINK_METADATA:usize=8*1024*1024;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum ResourceSlot{TextFile{source:Id},BrowserHtml{source:Id},Image{source:Id},Media{source:Id},SlideShow{source:Id,entry:u16},Mask{source:Id,filter:VideoFilterId},Lut{source:Id,filter:VideoFilterId}}
impl ResourceSlot{
 pub fn source(self)->Id{match self{Self::TextFile{source}|Self::BrowserHtml{source}|Self::Image{source}|Self::Media{source}|Self::SlideShow{source,..}|Self::Mask{source,..}|Self::Lut{source,..}=>source}}
 pub fn optional(self)->bool{matches!(self,Self::Mask{..}|Self::Lut{..})}
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum ResourceValue{TextFile{file:crate::TextFile},BrowserHtml{browser:crate::BrowserSource},Image{asset:ImageAsset},Media{media:MediaSource},SlideShow{entry:SlideShowEntry,images:Vec<ImageAsset>},Mask{asset:Option<ImageAsset>},Lut{asset:Option<LutAsset>}}
impl ResourceValue{
 pub fn metadata_bytes(&self)->usize{std::mem::size_of::<Self>().saturating_add(match self{
  Self::TextFile{file}=>file.path().len(),
  Self::BrowserHtml{browser}=>browser.css().len().saturating_add(match browser.location(){crate::BrowserLocation::LocalFile{path}=>path.len(),crate::BrowserLocation::Url{url}=>url.len()}),
  Self::Image{asset}=>asset.path().len(),Self::Media{media}=>media.path().len(),Self::Mask{asset}=>asset.as_ref().map_or(0,|a|a.path().len()),Self::Lut{asset}=>asset.as_ref().map_or(0,|a|a.path().len()),
  Self::SlideShow{entry,images}=>entry.path().len().saturating_add(images.iter().fold(0usize,|n,a|n.saturating_add(std::mem::size_of::<ImageAsset>()).saturating_add(a.path().len()))),
 })}
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRelink{pub slot:ResourceSlot,pub expected:ResourceValue,pub replacement:ResourceValue}
impl ResourceRelink{pub fn metadata_bytes(&self)->usize{std::mem::size_of::<Self>().saturating_add(self.expected.metadata_bytes()).saturating_add(self.replacement.metadata_bytes())}}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRelinks{pub rows:Vec<ResourceRelink>}
impl ResourceRelinks{
 pub fn validate(&self)->Result<(),Error>{if self.rows.is_empty()||self.rows.len()>MAX_RESOURCE_RELINKS{return Err(Error::Limit("relink requires 1..=64 prepared resources"));}let mut seen=std::collections::HashSet::new();let mut bytes=0usize;
  for row in &self.rows{if !seen.insert(row.slot){return Err(Error::Invalid("duplicate resource relink slot"));}bytes=bytes.checked_add(row.metadata_bytes()).filter(|n|*n<=MAX_RELINK_METADATA).ok_or(Error::Limit("relink prepared metadata exceeds 8 MiB"))?;}Ok(())}
}
fn span(settings:&crate::ImageSlideShow,index:usize)->Result<(SlideShowEntry,std::ops::Range<usize>),Error>{
 if settings.entries().is_empty(){let image=settings.images().get(index).ok_or(Error::Invalid("slideshow entry no longer exists"))?;return Ok((SlideShowEntry::new(crate::SlideShowEntryKind::File,image.path().to_owned())?,index..index+1));}
 let entry=settings.entries().get(index).ok_or(Error::Invalid("slideshow entry no longer exists"))?.clone();let start=settings.entry_image_counts()[..index].iter().map(|n|usize::from(*n)).sum::<usize>();Ok((entry,start..start+usize::from(settings.entry_image_counts()[index])))
}
impl StudioSnapshot{
 pub fn resource_value(&self,slot:ResourceSlot)->Result<ResourceValue,Error>{let source=self.source(slot.source()).ok_or(Error::NotFound(slot.source()))?;
  match(slot,source.kind()){
   (ResourceSlot::TextFile{..},SourceKind::Text{settings})=>settings.file().cloned().map(|file|ResourceValue::TextFile{file}).ok_or(Error::Invalid("text source no longer reads from a file")),
   (ResourceSlot::BrowserHtml{..},SourceKind::Browser{browser})if matches!(browser.location(),crate::BrowserLocation::LocalFile{..})=>Ok(ResourceValue::BrowserHtml{browser:browser.clone()}),
   (ResourceSlot::Image{..},SourceKind::Image{asset})=>Ok(ResourceValue::Image{asset:asset.clone()}),
   (ResourceSlot::Media{..},SourceKind::MediaSource{media})=>Ok(ResourceValue::Media{media:media.clone()}),
   (ResourceSlot::SlideShow{entry,..},SourceKind::ImageSlideShow{settings})=>{let(entry,range)=span(settings,usize::from(entry))?;Ok(ResourceValue::SlideShow{entry,images:settings.images()[range].to_vec()})},
   (ResourceSlot::Mask{filter,..},_)=>source.video_filters().iter().find(|f|f.id()==Some(filter)&&matches!(f.effect,crate::VideoEffect::ImageMask{..})).map(|f|ResourceValue::Mask{asset:f.mask_image().cloned()}).ok_or(Error::Invalid("mask relink target changed")),
   (ResourceSlot::Lut{filter,..},_)=>source.video_filters().iter().find(|f|f.id()==Some(filter)&&matches!(f.effect,crate::VideoEffect::ApplyLut{..})).map(|f|ResourceValue::Lut{asset:f.lut_asset().cloned()}).ok_or(Error::Invalid("LUT relink target changed")),
   _=>Err(Error::Invalid("resource relink target kind changed")),
  }
 }
 pub(crate) fn relink_resources(&mut self,batch:ResourceRelinks)->Result<(),Error>{batch.validate()?;
  for row in &batch.rows{if self.resource_value(row.slot)?!=row.expected{return Err(Error::Invalid("resource changed since relink preparation"));}}
  for row in batch.rows{let id=row.slot.source();let source=self.source_mut(id)?;let mut resize=None;
   match(row.slot,row.expected,row.replacement){
    (ResourceSlot::TextFile{..},ResourceValue::TextFile{file:old},ResourceValue::TextFile{file})=>{
     if old.last_lines()!=file.last_lines()||old.saved_lines()!=file.saved_lines(){return Err(Error::Invalid("text file relink must retain chat-log settings"));}
     let SourceKind::Text{settings}=&source.kind else{return Err(Error::Invalid("text file relink target changed"));};
     source.kind=SourceKind::Text{settings:settings.clone().with_file(Some(file))?};
    },
    (ResourceSlot::BrowserHtml{..},ResourceValue::BrowserHtml{browser:old},ResourceValue::BrowserHtml{browser})=>{
     let crate::BrowserLocation::LocalFile{path}=browser.location()else{return Err(Error::Invalid("browser HTML relink cannot change a local source into a URL"));};
     let retained=old.with_local_file(path.clone())?;
     if retained!=browser{return Err(Error::Invalid("browser HTML relink must retain every setting except its local file path"));}
     source.kind=SourceKind::Browser{browser:retained};
    },
    (ResourceSlot::Image{..},ResourceValue::Image{asset:old},ResourceValue::Image{asset})=>{asset.validate()?;resize=Some(((old.width(),old.height()),(asset.width(),asset.height())));source.kind=SourceKind::Image{asset};},
    (ResourceSlot::Media{..},ResourceValue::Media{media:old},ResourceValue::Media{media})=>{media.validate()?;
     if(old.looping(),old.restart_on_activate(),old.close_when_inactive(),old.clear_on_end())!=(media.looping(),media.restart_on_activate(),media.close_when_inactive(),media.clear_on_end()){return Err(Error::Invalid("relink must retain media playback settings"));}
     if !media.has_audio()&&(!source.audio_filters.is_empty()||!source.audio_properties.is_default()||!source.audio_monitoring.is_off()){return Err(Error::Invalid("replacement media cannot discard authored audio controls"));}
     if media.video_dimensions().is_none()&&!source.video_filters.is_empty(){return Err(Error::Invalid("replacement media cannot discard authored video filters"));}
     resize=Some((old.video_dimensions().unwrap_or((1,1)),media.video_dimensions().unwrap_or((1,1))));source.kind=SourceKind::MediaSource{media};},
    (ResourceSlot::SlideShow{entry:index,..},ResourceValue::SlideShow{entry:old,..},ResourceValue::SlideShow{entry,images})=>{
     if entry.kind()!=old.kind(){return Err(Error::Invalid("slideshow relink cannot change file/folder kind"));}let SourceKind::ImageSlideShow{settings}=&source.kind else{return Err(Error::Invalid("slideshow relink target changed"));};
     let(_,range)=span(settings,usize::from(index))?;if settings.images().len()-range.len()+images.len()>crate::MAX_SLIDES{return Err(Error::Limit("relinked slideshow exceeds 256 images"));}
     let mut all=settings.images().to_vec();all.splice(range,images.iter().cloned());let mut entries=settings.authored_entries();entries[usize::from(index)]=entry;
     let mut counts=if settings.entries().is_empty(){vec![1u16;entries.len()]}else{settings.entry_image_counts().to_vec()};counts[usize::from(index)]=u16::try_from(images.len()).map_err(|_|Error::Limit("relinked slideshow exceeds 256 images"))?;
     let next=settings.clone().with_prepared_resources(all,entries,counts)?;resize=Some((settings.dimensions(),next.dimensions()));source.kind=SourceKind::ImageSlideShow{settings:next};},
    (ResourceSlot::Mask{filter,..},ResourceValue::Mask{..},ResourceValue::Mask{asset})=>{let f=source.video_filters.iter_mut().find(|f|f.id()==Some(filter)).ok_or(Error::Invalid("mask filter removed"))?;*f=f.clone().with_mask_image(asset)?;},
    (ResourceSlot::Lut{filter,..},ResourceValue::Lut{..},ResourceValue::Lut{asset})=>{let f=source.video_filters.iter_mut().find(|f|f.id()==Some(filter)).ok_or(Error::Invalid("LUT filter removed"))?;*f=f.clone().with_lut_asset(asset)?;},
    _=>return Err(Error::Invalid("resource replacement kind mismatch")),
   }
   if let Some(((ow,oh),(nw,nh)))=resize{self.resize_reference_items(id,ow,oh,nw,nh);}
  }Ok(())
 }
}

impl StudioSnapshot{
 /// Pure unpublished candidate projection. Host admits old/provisional metadata
 /// before calling; no IO, Session history, native ownership or saved receipt.
 pub fn prepare_relinked_snapshot(&self,batch:ResourceRelinks)->Result<Self,Error>{
  batch.validate()?;
  let mut candidate=self.clone();candidate.relink_resources(batch)?;
  candidate.prepare_scale_anchors(Some(self))?;
  let changed=candidate.rebase_scene_canvas()?;
  candidate.normalize_groups(if changed{None}else{Some(self)})?;
  candidate.validate()?;
  if candidate!=*self{candidate.revision=self.revision().checked_add(1).ok_or(Error::IdentityExhausted)?;}
  Ok(candidate)
 }
}
