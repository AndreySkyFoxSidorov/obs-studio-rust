//! Window/session-scoped output choices, not persisted authored source IDs.
use super::*;

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum VirtualCameraRoute {
    #[default] Program,
    Preview,
    Scene {scene:Id},
    Source {source:Id},
}
impl VirtualCameraRoute {
    pub fn validate(self,model:&StudioSnapshot)->Result<(),Error> {
        match self {
            Self::Program|Self::Preview=>Ok(()),
            Self::Scene {scene}=>model.scene(scene).map(|_|()).ok_or(Error::NotFound(scene)),
            Self::Source {source}=>{
                let value=model.source(source).ok_or(Error::NotFound(source))?;
                if model.source_video_dimensions(value.kind())?.is_none(){return Err(Error::Invalid("virtual camera requires a video source"));}
                if matches!(value.kind(),SourceKind::NdiSource {..})&&!model.preferences().plugin_enabled(NDI_PLUGIN){
                    return Err(Error::Unavailable("Enable the DistroAV NDI source module first"));
                }
                Ok(())
            }
        }
    }
}
impl StudioSnapshot {
    /// A private source item fits the filtered image into the full canvas.
    /// Its item ID is local to this derived layout and never enters Session.
    /// Dynamic dimensions are a host-observed raw pixel basis, before filters.
    pub fn virtual_camera_source_layout(&self,source:Id,raw:(u32,u32))->Result<Scene,Error> {
        VirtualCameraRoute::Source {source}.validate(self)?;
        if raw.0==0||raw.1==0||raw.0>8192||raw.1>8192||u64::from(raw.0)*u64::from(raw.1)>32*1024*1024 {
            return Err(Error::Invalid("virtual camera source dimensions are outside source limits"));
        }
        let value=self.source(source).ok_or(Error::NotFound(source))?;
        let filtered=self.effective_filtered_video_dimensions(value,raw,Some((self.video().width(),self.video().height())))?;
        let scale=(f64::from(self.video().width())/f64::from(filtered.0))
            .min(f64::from(self.video().height())/f64::from(filtered.1));
        let mut transform=Transform::full(raw.0,raw.1);
        transform.width=(f64::from(raw.0)*scale) as f32;
        transform.height=(f64::from(raw.1)*scale) as f32;
        transform.x=((f64::from(self.video().width())-f64::from(filtered.0)*scale)*0.5) as f32;
        transform.y=((f64::from(self.video().height())-f64::from(filtered.1)*scale)*0.5) as f32;
        self.source_item_geometry(value,&transform)?;
        let mut layout=self.scenes().first().ok_or(Error::Invalid("virtual camera requires a canvas scene"))?.clone();
        layout.items=vec![SceneItem {scale_anchor:None,color_tag:SourceItemColorTag::Clear,id:source,source,visible:true,locked:false,transform,compositing:ItemCompositing::default()}];
        Ok(layout)
    }
}
