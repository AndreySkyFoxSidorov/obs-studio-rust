//! Canvas is authored space; output dimensions are a derived encoder format.
use crate::{Error,ScaleFiltering,StudioSnapshot,VideoSettings};
use serde::{Serialize,Deserialize};
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="mode",rename_all="snake_case",deny_unknown_fields)]
pub enum OutputResolution {#[default] MatchCanvas,Custom {width:u32,height:u32}}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="OutputData")]
pub struct OutputVideoSettings {resolution:OutputResolution,scale_filter:ScaleFiltering}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputData {resolution:OutputResolution,scale_filter:ScaleFiltering}
impl TryFrom<OutputData> for OutputVideoSettings {type Error=Error;fn try_from(data:OutputData)->Result<Self,Error>{Self::new(data.resolution,data.scale_filter)}}
impl Default for OutputVideoSettings {fn default()->Self{Self {resolution:OutputResolution::MatchCanvas,scale_filter:ScaleFiltering::Bicubic}}}
impl OutputVideoSettings {
    pub fn new(resolution:OutputResolution,scale_filter:ScaleFiltering)->Result<Self,Error>{
        if let OutputResolution::Custom {width,height}=resolution {VideoSettings::new(width,height,30,1)?;}
        Ok(Self {resolution,scale_filter})
    }
    pub fn resolution(self)->OutputResolution{self.resolution}
    pub fn scale_filter(self)->ScaleFiltering{self.scale_filter}
    pub fn is_default(&self)->bool{*self==Self::default()}
    pub fn resolve(self,canvas:&VideoSettings)->Result<VideoSettings,Error>{
        let (width,height)=match self.resolution {OutputResolution::MatchCanvas=>(canvas.width(),canvas.height()),OutputResolution::Custom {width,height}=>(width,height)};
        VideoSettings::new(width,height,canvas.fps_num(),canvas.fps_den())
    }
}
impl StudioSnapshot {
    pub fn output_video_settings(&self)->OutputVideoSettings{self.profiles().selected().output_video_settings()}
    pub fn output_video(&self)->Result<VideoSettings,Error>{self.output_video_settings().resolve(self.video())}
}
