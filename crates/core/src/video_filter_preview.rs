//! Pure same-revision draft projection. No Session, history or host resources.
use super::*;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum VideoFilterPreviewTarget {Source(Id),Scene(Id)}
impl StudioSnapshot {
    pub(super) fn set_source_video_filters(&mut self,source:Id,filters:Vec<VideoFilter>)->Result<(),Error> {
        if filters.len()>MAX_VIDEO_FILTERS {return Err(Error::Limit("video filter stack exceeds 32 effects"));}
        let original=self.source(source).ok_or(Error::NotFound(source))?;
        if self.source_video_dimensions(original.kind())?.is_none() {
            return Err(Error::Invalid("audio-only sources cannot contain video effects"));
        }
        video_filters::validate_activation(&self.preferences,original.video_filters(),&filters)?;
        self.set_video_filters(source,filters)
    }
    pub(super) fn set_scene_video_filters(&mut self,scene:Id,filters:Vec<VideoFilter>)->Result<(),Error> {
        self.scene(scene).ok_or(Error::NotFound(scene))?;
        video_filters::validate_activation(&self.preferences,self.scene_source(scene).map_or(&[],|source|source.video_filters()),&filters)?;
        if !filters.is_empty() || self.scene_source(scene).is_some() {
            let source=self.ensure_scene_source(scene)?;
            self.set_video_filters(source,filters)?;
        }
        Ok(())
    }
    pub fn preview_video_filters(&self,target:VideoFilterPreviewTarget,filters:Vec<VideoFilter>)->Result<Self,Error> {
        let mut candidate=self.clone();
        match target {
            VideoFilterPreviewTarget::Source(source)=>candidate.set_source_video_filters(source,filters)?,
            VideoFilterPreviewTarget::Scene(scene)=>candidate.set_scene_video_filters(scene,filters)?,
        }
        candidate.prepare_scale_anchors(Some(self))?;
        let canvas_changed=candidate.rebase_scene_canvas()?;
        candidate.normalize_groups(if canvas_changed{None}else{Some(self)})?;
        candidate.validate()?;
        Ok(candidate)
    }
    /// Reuse the actual containing scene/item identities, including a source
    /// nested in a group. This layout never replaces canonical scene contents.
    pub fn isolated_video_source_layout(&self,source:Id)->Result<Scene,Error> {
        fn find(model:&StudioSnapshot,scene:Id,source:Id,depth:usize,visited:&mut std::collections::HashSet<Id>)->Result<Option<SceneItem>,Error> {
            if depth>MAX_SCENE_DEPTH {return Err(Error::Limit("source preview scene depth"));}
            if !visited.insert(scene) {return Ok(None);}
            let layout=model.scene_container(scene).ok_or(Error::NotFound(scene))?;
            for item in layout.items() {
                if item.source()==source {return Ok(Some(item.clone()));}
                if let Some(nested)=model.source(item.source()).and_then(|value|value.kind().nested_scene()) {
                    if let Some(item)=find(model,nested,source,depth+1,visited)? {return Ok(Some(item));}
                }
            }
            Ok(None)
        }
        let value=self.source(source).ok_or(Error::NotFound(source))?;
        let (width,height)=self.source_video_dimensions(value.kind())?.ok_or(Error::Invalid("audio-only source preview"))?;
        let mut visited=std::collections::HashSet::new();
        for scene in self.scenes() {
            if let Some(mut item)=find(self,scene.id(),source,0,&mut visited)? {
                let mut layout=scene.clone();
                item.visible=true; item.locked=false; item.transform=Transform::full(width,height);
                // Isolate the source's own output. Per-item blending/sampling
                // belongs to its containing scene, not to a source screenshot
                // or the source's filter editor. Nested child items keep theirs.
                item.compositing=ItemCompositing::default();
                layout.items=vec![item];
                return Ok(layout);
            }
        }
        Err(Error::Unavailable("source has no scene reference to preview"))
    }
}
