//! Authored file/folder provenance; all enumeration and image IO belongs to Host.
use crate::{Error,Id,ImageAsset,ImageSlideShow,SourceKind,StudioSnapshot,MAX_SLIDES};
use serde::{Deserialize,Serialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum SlideShowEntryKind{File,Directory}
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct SlideShowEntry{kind:SlideShowEntryKind,path:String}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct Data{kind:SlideShowEntryKind,path:String}
impl TryFrom<Data>for SlideShowEntry{type Error=Error;fn try_from(data:Data)->Result<Self,Error>{Self::new(data.kind,data.path)}}
impl SlideShowEntry{
    pub fn new(kind:SlideShowEntryKind,path:String)->Result<Self,Error>{if path.trim().is_empty()||path.len()>4096||path.chars().any(char::is_control){return Err(Error::Invalid("slideshow entry requires 1..=4096 UTF-8 path bytes without controls"));}Ok(Self{kind,path})}
    pub fn kind(&self)->SlideShowEntryKind{self.kind}pub fn path(&self)->&str{&self.path}
}
pub(crate) fn validate(entries:&[SlideShowEntry],counts:&[u16],images:&[ImageAsset])->Result<(),Error>{
    if entries.is_empty(){return if counts.is_empty(){Ok(())}else{Err(Error::Invalid("slideshow entry counts require authored entries"))};}
    if entries.len()>MAX_SLIDES||entries.len()!=counts.len(){return Err(Error::Invalid("slideshow requires at most 256 entries with matching resolved counts"));}
    let mut cursor=0usize;for(entry,count)in entries.iter().zip(counts){let count=usize::from(*count);let end=cursor.checked_add(count).filter(|end|*end<=images.len()).ok_or(Error::Invalid("slideshow entry image counts exceed the prepared playlist"))?;
        match entry.kind{SlideShowEntryKind::File=>{if count!=1||images[cursor].path()!=entry.path(){return Err(Error::Invalid("slideshow file entry does not match its prepared image"));}},SlideShowEntryKind::Directory=>{
            let root=entry.path.trim_end_matches(|ch:char|ch=='/'||ch=='\u{5c}');for image in &images[cursor..end]{let tail=image.path().strip_prefix(root).and_then(|value|value.strip_prefix('/').or_else(||value.strip_prefix('\u{5c}'))).ok_or(Error::Invalid("slideshow folder image is outside its immediate directory"))?;if tail.is_empty()||tail.contains('/')||tail.contains('\u{5c}'){return Err(Error::Invalid("slideshow folder expansion must be nonrecursive"));}}
        }}cursor=end;
    }if cursor!=images.len(){return Err(Error::Invalid("slideshow prepared images are not fully owned by authored entries"));}Ok(())
}
impl StudioSnapshot{
    /// Pure load candidate: one clone and final graph/dimension validation;
    /// no IO, live Session mutation, history entry or runtime cursor.
    pub fn with_refreshed_slideshows(&self,updates:Vec<(Id,ImageSlideShow)>)->Result<Self,Error>{
        if updates.len()>64{return Err(Error::Limit("slideshow load refresh exceeds 64 source updates"));}let mut seen=std::collections::HashSet::new();let mut candidate=self.clone();
        for(source,settings)in updates{if !seen.insert(source){return Err(Error::Invalid("slideshow refresh contains duplicate source identities"));}settings.validate()?;let SourceKind::ImageSlideShow{settings:old}=&candidate.source_mut(source)?.kind else{return Err(Error::Invalid("source is not an image slideshow"));};let(ow,oh)=old.dimensions();let(nw,nh)=settings.dimensions();candidate.source_mut(source)?.kind=SourceKind::ImageSlideShow{settings};candidate.resize_reference_items(source,ow,oh,nw,nh);}
        candidate.prepare_scale_anchors(Some(self))?;
        let canvas_changed=candidate.rebase_scene_canvas()?;
        candidate.normalize_groups(if canvas_changed{None}else{Some(self)})?;
        candidate.validate()?;if candidate!=*self{candidate.revision=candidate.revision.checked_add(1).ok_or(Error::IdentityExhausted)?;}Ok(candidate)
    }
}

/// Rebind only derived resources for the same source and authored entries.
/// Playback options in the historical scene remain exactly as authored.
pub(super) fn rebind_history_slideshows(model:&mut StudioSnapshot,overrides:&[(Id,ImageSlideShow)])->Result<(),Error>{
    let mut updates=Vec::new();
    for (id,prepared) in overrides{
        let Some(source)=model.source(*id) else{continue;};
        let SourceKind::ImageSlideShow{settings}=source.kind() else{continue;};
        if settings.entries()!=prepared.entries(){continue;}
        let candidate=settings.clone().with_prepared_resources(prepared.images().to_vec(),prepared.entries().to_vec(),prepared.entry_image_counts().to_vec())?;
        if &candidate!=settings{updates.push((*id,candidate));}
    }
    if !updates.is_empty(){let revision=model.revision;let mut rebound=model.with_refreshed_slideshows(updates)?;rebound.revision=revision;*model=rebound;}
    Ok(())
}
impl crate::Session{
    /// Apply a prepared filesystem observation without adding an authored edit.
    /// The exact previous source settings guard prevents stale background work.
    pub fn refresh_slideshow_resources(&mut self,session:u64,source:Id,expected:&ImageSlideShow,prepared:ImageSlideShow)->Result<bool,Error>{
        if self.closed{return Err(Error::Closed);}
        if self.identity!=session{return Err(Error::Invalid("slideshow refresh belongs to another session"));}
        let current=self.current.source(source).ok_or(Error::NotFound(source))?;
        if !matches!(current.kind(),SourceKind::ImageSlideShow{settings} if settings==expected){return Err(Error::Invalid("slideshow refresh source changed"));}
        if expected.entries()!=prepared.entries(){return Err(Error::Invalid("slideshow refresh authored entries changed"));}
        let reconstructed=expected.clone().with_prepared_resources(prepared.images().to_vec(),prepared.entries().to_vec(),prepared.entry_image_counts().to_vec())?;
        if reconstructed!=prepared{return Err(Error::Invalid("slideshow refresh authored options changed"));}
        if expected==&prepared{return Ok(false);}
        let candidate=self.current.with_refreshed_slideshows(vec![(source,prepared.clone())])?;
        let definitions=self.source_definitions.prepare(Some(self.current.as_ref()),&candidate)?;
        let mut overrides=Vec::new();overrides.try_reserve_exact(self.slideshow_refresh_overrides.len().saturating_add(1)).map_err(|_|Error::Limit("slideshow refresh overlay allocation"))?;
        overrides.extend(self.slideshow_refresh_overrides.iter().filter(|(id,_)|*id!=source).cloned());
        // Evicted history will be observed again when its source is restored.
        if overrides.len()>=64{overrides.remove(0);}overrides.push((source,prepared));
        let was_clean=self.saved_revision==Some(self.current.revision);let revision=candidate.revision;
        self.current=std::sync::Arc::new(candidate);self.source_definitions=definitions;self.slideshow_refresh_overrides=overrides;
        if was_clean{self.saved_revision=Some(revision);}Ok(true)
    }
}
