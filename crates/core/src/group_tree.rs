//! Atomic private-group tree construction; no intermediate child publication.
use crate::*;
use serde::{Deserialize,Serialize};

#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupTreeItem {
    pub transform:Transform,
    pub visible:bool,
    pub locked:bool,
    #[serde(default)] pub compositing:ItemCompositing,
    #[serde(default)] pub color_tag:SourceItemColorTag,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum GroupTreeEntry {
    Existing {source:Id,item:GroupTreeItem},
    Scene {scene:Id,item:GroupTreeItem},
    Group {group:GroupTree},
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupTree {
    pub name:String,
    /// Actual declared raw raster, before source filters. Zero/zero is an
    /// empty, nonrendering Group; its canonical scale-only basis is1x1.
    pub width:u32,
    pub height:u32,
    #[serde(default)] pub filters:Vec<VideoFilter>,
    pub item:GroupTreeItem,
    pub children:Vec<GroupTreeEntry>,
}
struct Admission {groups:usize,items:usize,names:std::collections::HashSet<String>,scene_owners:std::collections::HashSet<Id>}
impl GroupTree {
    fn preflight(&self,model:&StudioSnapshot,depth:usize,state:&mut Admission)->Result<(),Error>{
        if depth>=MAX_SCENE_DEPTH{return Err(Error::Limit("group ancestor depth"));}
        state.groups=state.groups.checked_add(1).ok_or(Error::Limit("collection entity budget"))?;
        state.items=state.items.checked_add(1).ok_or(Error::Limit("collection entity budget"))?;
        if state.groups>MAX_SOURCES||state.items>MAX_ITEMS||self.children.len()>MAX_ITEMS||self.filters.len()>MAX_VIDEO_FILTERS{
            return Err(Error::Limit("collection entity budget"));
        }
        validate_name(&self.name)?;
        state.names.try_reserve(1).map_err(|_|Error::Limit("Cannot allocate bounded Group import metadata."))?;
        if model.sources().iter().any(|source|source.name()==self.name)||model.scenes().iter().chain(model.groups()).any(|scene|scene.name()==self.name)
            ||!state.names.insert(self.name.clone()) {return Err(Error::Invalid("duplicate source name"));}
        if self.width==0||self.height==0 {
            if self.width!=0||self.height!=0{return Err(Error::Invalid("Empty Group dimensions must both be zero."));}
        }else{GroupSource::new(Id(1),self.width,self.height)?;}
        self.item.transform.geometry(8192,8192)?;
        for child in &self.children {match child{
            GroupTreeEntry::Group{group}=>group.preflight(model,depth+1,state)?,
            GroupTreeEntry::Existing{source,item}=>{
                let source=model.source(*source).ok_or(Error::NotFound(*source))?;
                if matches!(source.kind(),SourceKind::Group{..}){return Err(Error::Invalid("Private Group sources cannot be reused as aliases."));}
                item.transform.geometry(8192,8192)?;
                state.items=state.items.checked_add(1).ok_or(Error::Limit("collection entity budget"))?;
            }
            GroupTreeEntry::Scene{scene,item}=>{model.scene(*scene).ok_or(Error::NotFound(*scene))?;if model.scene_source(*scene).is_none(){state.scene_owners.try_reserve(1).map_err(|_|Error::Limit("Cannot allocate bounded Group import metadata."))?;state.scene_owners.insert(*scene);}item.transform.geometry(8192,8192)?;state.items=state.items.checked_add(1).ok_or(Error::Limit("collection entity budget"))?;}
        }if state.items>MAX_ITEMS{return Err(Error::Limit("collection entity budget"));}}
        Ok(())
    }
}
impl StudioSnapshot {
    pub(super) fn add_group_tree(&mut self,scene:Id,tree:GroupTree)->Result<Id,Error>{
        self.ensure_container_unlocked(scene)?;self.scene_container(scene).ok_or(Error::NotFound(scene))?;
        let mut depth=0usize;let mut ancestor=scene;
        while self.scene(ancestor).is_none(){if depth>=MAX_SCENE_DEPTH{return Err(Error::Limit("group ancestor depth"));}ancestor=self.group_parent(ancestor).ok_or(Error::NotFound(ancestor))?.0;depth+=1;}
        let mut admission=Admission{groups:0,items:0,names:std::collections::HashSet::new(),scene_owners:std::collections::HashSet::new()};tree.preflight(self,depth,&mut admission)?;
        let existing=self.scenes().iter().chain(self.groups()).map(|scene|scene.items().len()).sum::<usize>();
        if existing.checked_add(admission.items).is_none_or(|count|count>MAX_ITEMS)
            ||self.sources().len().checked_add(admission.groups).and_then(|count|count.checked_add(admission.scene_owners.len())).is_none_or(|count|count>MAX_SOURCES)
            ||self.groups().len().checked_add(admission.groups).is_none_or(|count|count>MAX_SOURCES){return Err(Error::Limit("collection entity budget"));}
        let mut declared=Vec::new();declared.try_reserve_exact(admission.groups).map_err(|_|Error::Limit("Cannot allocate bounded Group import metadata."))?;
        let created=self.install_group_tree(scene,tree,&mut declared)?;
        // Check the complete tree BEFORE normalization. Intermediate partial
        // groups are never exposed, and arbitrary custom rasters are not hidden
        // by the ordinary group-normalization repair of a candidate.
        for (container,expected) in declared {
            if self.group_native_dimensions(container)?!=expected{return Err(Error::Invalid("Declared Group dimensions disagree with its derived raster."));}
        }
        Ok(created)
    }
    fn install_group_tree(&mut self,parent:Id,tree:GroupTree,declared:&mut Vec<(Id,Option<(u32,u32)>)>)->Result<Id,Error>{
        let item=self.create_group(parent,tree.name,Vec::new())?;
        let source=self.item_container(item).ok_or(Error::NotFound(item))?.1.source();
        let container=match self.source(source).ok_or(Error::NotFound(source))?.kind(){SourceKind::Group{group}=>group.scene(),_=>return Err(Error::Invalid("private group has no source owner"))};
        let expected=if tree.width==0{None}else{Some((tree.width,tree.height))};
        let basis=expected.unwrap_or((1,1));self.source_mut(source)?.kind=SourceKind::Group{group:GroupSource::new(container,basis.0,basis.1)?};
        self.set_source_video_filters(source,tree.filters)?;
        for child in tree.children {match child {
            GroupTreeEntry::Group{group}=>{self.install_group_tree(container,group,declared)?;}
            GroupTreeEntry::Existing{source,item}=>{self.install_group_tree_item(container,source,item)?;}
            GroupTreeEntry::Scene{scene,item}=>{let source=self.ensure_scene_source(scene)?;self.install_group_tree_item(container,source,item)?;}
        }}
        let row=self.item_mut(parent,item)?;row.transform=tree.item.transform;row.visible=tree.item.visible;row.locked=tree.item.locked;row.compositing=tree.item.compositing;row.color_tag=tree.item.color_tag;
        declared.push((container,expected));Ok(item)
    }
    fn install_group_tree_item(&mut self,container:Id,source:Id,settings:GroupTreeItem)->Result<(),Error>{
        let id=self.issue()?;self.scene_mut(container)?.items.push(SceneItem{scale_anchor:None,color_tag:settings.color_tag,compositing:settings.compositing,id,source,visible:settings.visible,locked:settings.locked,transform:settings.transform});Ok(())
    }
}
