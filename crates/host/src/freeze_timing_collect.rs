//! Borrowed canonical/private Freeze target collection for the application
//! owner. Draft rows are supplied separately by their actual editor owner.
use crate::{freeze_timing_book::{Definition,Group,Instance,Target,MAX_TARGETS},render_gpu::RenderGpu,transition::{FrameScene,SceneKey}};
use skylive_core::{Session,Source,VideoEffect};
use std::sync::Arc;

pub(crate) struct Definitions<'a>{gpu:&'a RenderGpu,values:Vec<Definition>,credit:Option<Arc<dyn Send+Sync>>}
impl<'a> Definitions<'a>{
    pub(crate) fn new(gpu:&'a RenderGpu)->Self{Self{gpu,values:Vec::new(),credit:None}}
    pub(crate) fn push(&mut self,value:Definition)->Result<(),String>{
        if self.values.iter().any(|entry|entry.target==value.target){return Ok(());}
        if self.values.len()==MAX_TARGETS{return Err("Freeze source history count128".into());}
        if self.values.len()==self.values.capacity(){
            let capacity=self.values.capacity().max(2).saturating_mul(2).min(MAX_TARGETS);
            let admitted=capacity.checked_mul(2).ok_or("Freeze definition metadata overflow")?;
            let bytes=admitted.checked_mul(std::mem::size_of::<Definition>()).and_then(|v|v.checked_add(64)).ok_or("Freeze definition metadata overflow")?;
            let replacement=self.gpu.freeze_credit(bytes)?;
            self.values.try_reserve_exact(capacity-self.values.len()).map_err(|_|"Freeze definition metadata allocation")?;
            if self.values.capacity()>admitted{return Err("Freeze definition metadata capacity".into());}
            self.credit=Some(replacement);
        }
        self.values.push(value);Ok(())
    }
    pub(crate) fn values(&self)->&[Definition]{&self.values}
}
fn append(definitions:&mut Definitions<'_>,session:u64,generation:u64,source:&Source,owner:Option<u64>,module_enabled:bool)->Result<(),String>{
    for filter in source.video_filters(){
        let VideoEffect::Freeze{settings}=filter.effect else{continue};
        let Some(filter_id)=filter.id() else{return Err("Freeze target has no stable filter identity".into());};
        let instance=owner.map_or(Instance::Canonical{filter:filter_id},|owner|Instance::Private{owner,filter:filter_id});
        let target=Target{session,source:source.id(),instance};
        definitions.push(Definition{target,generation,enabled:module_enabled&&filter.enabled,duration_ms:settings.duration_ms(),refresh_ms:settings.refresh_interval_ms()})?;
    }
    Ok(())
}

/// Publish current canonical sources and actual frozen-view private instances.
/// No capture or native device is opened. The generation is a conservative
/// revision fence; enabled-run origins are retained across definition edits.
pub fn observe(gpu:&RenderGpu,session:&Session,views:&[FrameScene],now:u64)->Result<(),String>{
    if session.is_closed(){return Err("Freeze session is closed".into());}
    let model=session.snapshot();let id=session.identity();
    let generation=model.revision().checked_add(1).ok_or("Freeze definition generation exhausted")?;
    let enabled=model.preferences().plugin_enabled(skylive_core::EXELDRO_FREEZE_PLUGIN);
    let mut private=Definitions::new(gpu);
    for view in views{
        view.validate_session(id).map_err(|error|error.to_string())?;
        let SceneKey::Frozen{instance,..}=view.key() else{continue;};
        if view.is_filter_preview(){continue;}
        for source in view.visible_sources(&model).map_err(|error|error.to_string())?{
            if model.source(source.id()).is_none(){continue;}
            let owned=source.kind().nested_scene().is_some()||source.requires_video_canvas()&&view.filter_canvas_dimensions(&model)!=(model.video().width(),model.video().height());
            if owned{append(&mut private,id,generation,source,Some(instance),enabled)?;}
        }
        if let Some(source)=view.root_scene_source(&model).filter(|source|model.source(source.id()).is_some()){
            append(&mut private,id,generation,source,Some(instance),enabled)?;
        }
    }
    observe_canonical(gpu,session,now)?;
    gpu.freeze_observe(Group::Private,private.values(),now)
}

/// Call immediately after each accepted canonical publication, including a
/// same-frame disable/re-enable sequence; it does not touch draft/private runs.
pub fn observe_canonical(gpu:&RenderGpu,session:&Session,now:u64)->Result<(),String>{
    if session.is_closed(){return Err("Freeze session is closed".into());}
    let model=session.snapshot();let id=session.identity();let generation=model.revision().checked_add(1).ok_or("Freeze definition generation exhausted")?;
    let enabled=model.preferences().plugin_enabled(skylive_core::EXELDRO_FREEZE_PLUGIN);
    let mut canonical=Definitions::new(gpu);
    for source in model.sources(){append(&mut canonical,id,generation,source,None,enabled)?;}
    gpu.freeze_observe(Group::Canonical,canonical.values(),now)
}
