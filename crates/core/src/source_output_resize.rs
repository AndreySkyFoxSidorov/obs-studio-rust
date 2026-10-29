//! Atomic profile resolution plus the selected item's existing fit operation.
use crate::{Error,Id,MAX_DISPLAY_PIXELS,SourceKind,StudioSnapshot,TransformAction,VideoSettings,OutputResolution,OutputVideoSettings,Transform};
use serde::Serialize;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize)]
pub struct SourceOutputSize {pub raw_width:u32,pub raw_height:u32,pub source_width:u32,pub source_height:u32,pub width:u32,pub height:u32,pub fits_item:bool}
impl StudioSnapshot {
    /// Observed raw dimensions are supplied by the Host's current source receipt.
    /// Pure callers must supply the actual runtime size, not a proposed mode.
    pub fn source_output_size(&self,scene:Id,item:Id,raw_width:u32,raw_height:u32)->Result<SourceOutputSize,Error>{
        let row=self.scene_container(scene).and_then(|scene|scene.items().iter().find(|row|row.id()==item)).ok_or(Error::NotFound(item))?;
        let source=self.source(row.source()).ok_or(Error::NotFound(row.source()))?;
        let basis=self.source_video_dimensions(source.kind())?.ok_or(Error::Invalid("Selected source has no video dimensions."))?;
        if raw_width==0||raw_height==0||raw_width>8192||raw_height>8192||u64::from(raw_width)*u64::from(raw_height)>MAX_DISPLAY_PIXELS{return Err(Error::Invalid("Source dimensions exceed the admitted video source limits."));}
        let live=matches!(source.kind(),SourceKind::DisplayCapture{..}|SourceKind::WindowCapture{..}|SourceKind::VideoCapture{..}|SourceKind::MediaSource{..}|SourceKind::NdiSource{..}|SourceKind::GameCapture{..}|SourceKind::Browser{..});
        if !live&&(raw_width,raw_height)!=basis{return Err(Error::Invalid("Source size metadata no longer matches its intrinsic definition."));}
        let(source_width,source_height)=self.effective_filtered_video_dimensions(source,(raw_width,raw_height),Some((self.video().width(),self.video().height())))?;
        if source_width<32||source_height<32{return Err(Error::Invalid("Resize output requires a source of at least 32 by 32 pixels."));}
        let width=source_width.checked_add(3).ok_or(Error::Invalid("Source width rounding overflow."))?/4*4;
        let height=source_height.checked_add(1).ok_or(Error::Invalid("Source height rounding overflow."))?/2*2;
        VideoSettings::new(width,height,self.video().fps_num(),self.video().fps_den())?;
        let size=SourceOutputSize{raw_width,raw_height,source_width,source_height,width,height,fits_item:!row.locked()&&!self.container_is_locked(scene)};
        // Final fit uses the prospective normalized Group/Scene basis at the
        // atomic command boundary, rather than the inspector's current basis.
        Ok(size)
    }
    fn source_output_fit(&self,scene:Id,item:Id,size:SourceOutputSize)->Result<Option<Transform>,Error>{
        if !size.fits_item{return Ok(None);}
        let row=self.scene_container(scene).and_then(|scene|scene.items().iter().find(|row|row.id()==item)).ok_or(Error::NotFound(item))?;
        let source=self.source(row.source()).ok_or(Error::NotFound(row.source()))?;
        // Called after common context/group normalization: a selected Group
        // may have changed its raw basis when its Scene children were rebased.
        let basis=self.source_video_dimensions(source.kind())?.ok_or(Error::Invalid("Selected source has no video dimensions."))?;
        let raw=if matches!(source.kind(),SourceKind::Scene{..}|SourceKind::Group{..}){basis}else{(size.raw_width,size.raw_height)};
        let frame=self.effective_filtered_video_dimensions(source,raw,Some((size.width,size.height)))?;
        let mut transform=row.transform().clone();transform.apply_action_for_frame(TransformAction::FitToCanvas,basis,frame,(size.width,size.height))?;Ok(Some(transform))
    }
    pub(super) fn resize_output_to_source(&mut self,scene:Id,item:Id,raw_width:u32,raw_height:u32)->Result<bool,Error>{
        let size=self.source_output_size(scene,item,raw_width,raw_height)?;
        let video=VideoSettings::new(size.width,size.height,self.video().fps_num(),self.video().fps_den())?;
        // Original writes explicit OutputCX/CY alongside BaseCX/CY. Preserve the
        // existing downscale algorithm and FPS, but do not silently make output
        // follow later canvas changes via MatchCanvas authoring semantics.
        let output=OutputVideoSettings::new(OutputResolution::Custom{width:size.width,height:size.height},self.output_video_settings().scale_filter())?;
        self.profiles.configure_video(video,output)?;
        if self.rebase_scene_canvas()?{self.normalize_groups(None)?;}
        let transform=self.source_output_fit(scene,item,size)?;
        let Some(transform)=transform else{return Ok(false)};
        let original=self.scene_container(scene).and_then(|scene|scene.items().iter().find(|row|row.id()==item)).ok_or(Error::NotFound(item))?;
        let changed=original.transform()!=&transform;
        let anchor=if original.transform().width!=transform.width||original.transform().height!=transform.height {
            self.scale_anchor_for(self.source(original.source()).ok_or(Error::NotFound(original.source()))?,&transform)?
        }else{original.scale_anchor};
        let row=self.item_mut(scene,item)?;row.transform=transform;row.scale_anchor=anchor;
        Ok(changed)
    }
}
