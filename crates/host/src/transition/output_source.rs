//! Immutable isolated-source output view, distinct from filter-preview/drafts.
use super::*;
impl FrameScene {
    pub fn output_video_source(session:u64,current:&StudioSnapshot,source:Id,raw:(u32,u32))->Result<Self,TransitionError> {
        let layout=current.virtual_camera_source_layout(source,raw)?;
        let mut retained=HashMap::new();let mut nested=HashMap::new();
        let mut dimensions=HashMap::new();let mut pending=vec![source];
        let mut edges=0usize;
        while let Some(id)=pending.pop() {
            if retained.contains_key(&id){continue;}
            if retained.len()>=MAX_SOURCES{return Err(TransitionError::Limit("isolated source closure limit"));}
            let value=current.source(id).ok_or(TransitionError::NotFound(id))?;
            if let Some(basis)=current.source_video_dimensions(value.kind())? {dimensions.insert(id,if id==source {raw}else{basis});}
            if let Some(scene)=value.kind().nested_scene() {
                if !nested.contains_key(&scene) {
                    let child=current.scene_container(scene).ok_or(TransitionError::NotFound(scene))?;
                    edges=edges.checked_add(child.items().len()).ok_or(TransitionError::Limit("isolated source edge overflow"))?;
                    if edges>skylive_core::MAX_ITEMS{return Err(TransitionError::Limit("isolated source edge limit"));}
                    pending.extend(child.items().iter().map(SceneItem::source));
                    nested.insert(scene,Arc::new(child.clone()));
                }
            }
            retained.insert(id,Arc::new(value.clone()));
        }
        let view=Self::Frozen(Arc::new(SceneInstance {session,instance:issue_instance(&NEXT_INSTANCE)?,layout:Arc::new(layout),
            root_source:None,retained,nested,projection:None,closure_error:None,capture_dimensions:dimensions,
            refreshed_revision:current.revision(),preview_layout:false,normalized_group_preview:false,filter_preview:false,auto_text_draft:Vec::new(),auto_authored_root:None,preview_canvas:None,filter_canvas:(current.video().width(),current.video().height())}));
        view.visible_sources(current)?;
        Ok(view)
    }
}
