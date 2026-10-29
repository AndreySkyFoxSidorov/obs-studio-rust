//! Pure owned composition selection shared by GUI, CLI and host output owner.
//! It never starts capture, installs a device, mutates Session or fabricates pixels.
use super::*;
use crate::{capture::CaptureSnapshot,playback::PlaybackSnapshot,transition::{FrameComposition,FrameScene}};
use skylive_core::{Session,VirtualCameraRoute};

#[derive(Default)]
pub struct RouteFactory {cached:Option<((u64,u64,VirtualCameraRoute,(u32,u32)),FrameComposition)>}
impl RouteFactory {
    pub fn composition(&mut self,session:&Session,route:VirtualCameraRoute,program:&FrameComposition,
        capture:&CaptureSnapshot)->Result<FrameComposition,Error> {
        if session.is_closed()||session.identity()!=capture.session(){return Err(Error::Invalid("foreign virtual-camera route session"));}
        let model=session.snapshot();route.validate(&model).map_err(|error|Error::RouteUnavailable(error.to_string()))?;
        match route {
            VirtualCameraRoute::Program=>{for view in program.scenes(){view.validate_session(session.identity()).map_err(|e|Error::RouteUnavailable(e.to_string()))?;}Ok(program.clone())},
            VirtualCameraRoute::Preview=>Ok(FrameComposition::Scene(FrameScene::Live(session.preview()))),
            VirtualCameraRoute::Scene {scene}=>Ok(FrameComposition::Scene(FrameScene::Live(scene))),
            VirtualCameraRoute::Source {source}=>{
                let value=model.source(source).ok_or(Error::Invalid("virtual-camera source was removed"))?;
                let authored=model.source_video_dimensions(value.kind()).map_err(|e|Error::RouteUnavailable(e.to_string()))?
                    .ok_or(Error::Invalid("virtual-camera target is audio-only"))?;
                let raw=capture.frame(source).filter(|frame|frame.binding.matches_source(value.kind()))
                    .map(|frame|(frame.image.width(),frame.image.height())).unwrap_or(authored);
                let key=(session.identity(),model.revision(),route,raw);
                if let Some((old,composition))=&self.cached {if *old==key{return Ok(composition.clone());}}
                let composition=FrameComposition::Scene(FrameScene::output_video_source(session.identity(),&model,source,raw)
                    .map_err(|e|Error::RouteUnavailable(e.to_string()))?);
                self.cached=Some((key,composition.clone()));Ok(composition)
            }
        }
    }
    pub fn frame(&mut self,session:&Session,route:VirtualCameraRoute,program:&FrameComposition,
        playback:&PlaybackSnapshot,capture:Arc<CaptureSnapshot>,resource_generation:u64)->Result<RoutedVideoFrame,Error> {
        if playback.session()!=session.identity()||playback.revision()!=session.snapshot().revision(){return Err(Error::Invalid("stale virtual-camera playback"));}
        let output=self.composition(session,route,program,&capture)?;
        Ok(RoutedVideoFrame {route,frame:ProgramVideoFrame {session:session.identity(),time:playback.now(),
            model:session.snapshot(),output,playback:playback.clone(),capture,resource_generation}})
    }
}
pub struct RoutedVideoFrame {pub(super) route:VirtualCameraRoute,pub(super) frame:ProgramVideoFrame}
impl RoutedVideoFrame {pub fn route(&self)->VirtualCameraRoute{self.route}}
