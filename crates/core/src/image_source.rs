//! Authored Image Source properties. No paths are opened or pixels evaluated.
use crate::{Collection,Error,Id,ImageAsset,Source,SourceKind,StudioSnapshot};
use serde::{Deserialize,Serialize};

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(default,deny_unknown_fields)]
pub struct ImageSourceSettings {
    pub unload_when_hidden:bool,
    pub linear_alpha:bool,
}
impl ImageSourceSettings {
    pub fn is_default(self)->bool{self==Self::default()}
}

#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum ImageSourceCommand {
    Add{scene:Id,name:String,asset:ImageAsset,#[serde(default)] settings:ImageSourceSettings},
    Configure{source:Id,asset:ImageAsset,#[serde(default)] settings:ImageSourceSettings},
}
impl Source {
    pub fn image_settings(&self)->ImageSourceSettings{self.image_settings.unwrap_or_default()}
    pub fn has_image_source_metadata(&self)->bool{self.image_settings.is_some()}
}
impl Collection {
    pub fn has_image_source_metadata(&self)->bool{self.sources().iter().any(Source::has_image_source_metadata)}
}
impl StudioSnapshot {
    pub(super) fn image_source_command(&mut self,command:ImageSourceCommand)->Result<Option<Id>,Error>{
        match command {
            ImageSourceCommand::Add{scene,name,asset,settings}=>{
                asset.validate()?;
                let item=self.add_source(scene,name,SourceKind::Image{asset})?;
                let source=self.item_container(item).ok_or(Error::NotFound(item))?.1.source();
                self.source_mut(source)?.image_settings=(!settings.is_default()).then_some(settings);
                Ok(Some(item))
            }
            ImageSourceCommand::Configure{source,asset,settings}=>{
                asset.validate()?;
                let previous=self.source(source).ok_or(Error::NotFound(source))?;
                let SourceKind::Image{asset:old}=previous.kind()else{return Err(Error::Invalid("source is not an image"));};
                let (old_w,old_h)=(old.width(),old.height());let(new_w,new_h)=(asset.width(),asset.height());
                let target=self.source_mut(source)?;target.kind=SourceKind::Image{asset};
                target.image_settings=(!settings.is_default()).then_some(settings);
                self.resize_reference_items(source,old_w,old_h,new_w,new_h);
                Ok(None)
            }
        }
    }
}

/// Rebind all authored Image Sources with exactly this path. No filesystem I/O.
fn rebind_path(model:&mut StudioSnapshot,asset:&ImageAsset)->Result<bool,Error>{
    let mut targets=Vec::new();
    targets.try_reserve_exact(model.sources().len()).map_err(|_|Error::Limit("image refresh source list allocation"))?;
    for source in model.sources(){
        if let SourceKind::Image{asset:old}=source.kind(){
            if old.path()==asset.path() && old!=asset {targets.push((source.id(),source.image_settings()));}
        }
    }
    let changed=!targets.is_empty();
    for (source,settings) in targets {model.image_source_command(ImageSourceCommand::Configure{source,asset:asset.clone(),settings})?;}
    Ok(changed)
}
// Explicitly configuring an already watched path establishes a newer identity
// for history as well; otherwise Undo would resurrect the prior watcher result.
pub(super) fn manual_override(overrides:&[ImageAsset],command:&crate::Command)->Result<Option<Vec<ImageAsset>>,Error>{
    let mut replacements:Vec<&ImageAsset>=Vec::new();
    match command {
        crate::Command::SetImageSource{asset,..}|crate::Command::ImageSource{command:ImageSourceCommand::Configure{asset,..}}=>{
            replacements.try_reserve_exact(1).map_err(|_|Error::Limit("image refresh overlay allocation"))?;replacements.push(asset);
        }
        crate::Command::RelinkResources{batch,..}=>{
            batch.validate()?;
            replacements.try_reserve_exact(batch.rows.len()).map_err(|_|Error::Limit("image refresh overlay allocation"))?;
            for row in &batch.rows {
                if matches!(row.slot,crate::ResourceSlot::Image{..}) {
                    if let crate::ResourceValue::Image{asset}=&row.replacement {replacements.push(asset);}
                }
            }
        }
        _=>return Ok(None),
    }
    // Multiple receipts for a watched path must agree; otherwise the same
    // path has no single safe identity to use when history is restored.
    for (index,asset) in replacements.iter().enumerate(){
        if overrides.iter().any(|old|old.path()==asset.path()) && replacements[index+1..].iter().any(|other|other.path()==asset.path() && *other!=*asset){
            return Err(Error::Invalid("relinked images for one watched path have conflicting identities"));
        }
    }
    if !overrides.iter().any(|old|replacements.iter().any(|asset|old.path()==asset.path() && old!=*asset)){return Ok(None);}
    let mut next=Vec::new();next.try_reserve_exact(overrides.len()).map_err(|_|Error::Limit("image refresh overlay allocation"))?;
    next.extend(overrides.iter().map(|old|replacements.iter().find(|asset|old.path()==asset.path()).map_or_else(||old.clone(),|asset|(*asset).clone())));
    Ok(Some(next))
}
pub(super) fn rebind_history_images(model:&mut StudioSnapshot,overrides:&[ImageAsset])->Result<(),Error>{
    for asset in overrides {rebind_path(model,asset)?;}
    Ok(())
}
impl crate::Session {
    /// Apply a prepared filesystem identity as runtime synchronization, not an
    /// authored scene edit. Exact receipt guard prevents replacing newer edits.
    pub fn refresh_image_asset(&mut self,session:u64,source:Id,expected:&ImageAsset,asset:ImageAsset)->Result<bool,Error>{
        if self.closed{return Err(Error::Closed);}
        if self.identity!=session{return Err(Error::Invalid("image refresh belongs to another session"));}
        let current=self.current.source(source).ok_or(Error::NotFound(source))?;
        if !matches!(current.kind(),SourceKind::Image{asset:old} if old==expected){return Err(Error::Invalid("image refresh source changed"));}
        if expected.path()!=asset.path(){return Err(Error::Invalid("image refresh path changed"));}
        asset.validate()?;
        let mut candidate=(*self.current).clone();
        if !rebind_path(&mut candidate,&asset)? {return Ok(false);}
        candidate.prepare_scale_anchors(Some(&self.current))?;
        let canvas_changed=candidate.rebase_scene_canvas()?;
        candidate.normalize_groups(if canvas_changed{None}else{Some(&self.current)})?;
        candidate.revision=self.current.revision.checked_add(1).ok_or(Error::IdentityExhausted)?;
        candidate.validate()?;
        let definitions=self.source_definitions.prepare(Some(self.current.as_ref()),&candidate)?;
        // Prepare the bounded overlay before publication. Old history remains
        // immutable; its matching paths are rebound when history is restored.
        let mut overrides=Vec::new();
        overrides.try_reserve_exact(self.image_refresh_overrides.len().saturating_add(1)).map_err(|_|Error::Limit("image refresh overlay allocation"))?;
        overrides.extend(self.image_refresh_overrides.iter().filter(|old|old.path()!=asset.path()).cloned());
        if overrides.len()>=crate::MAX_SOURCES{return Err(Error::Limit("image refresh path overlay capacity"));}
        overrides.push(asset);
        let was_clean=self.saved_revision==Some(self.current.revision);
        let revision=candidate.revision;
        self.current=std::sync::Arc::new(candidate);
        self.source_definitions=definitions;
        self.image_refresh_overrides=overrides;
        if was_clean {self.saved_revision=Some(revision);}
        Ok(true)
    }
}