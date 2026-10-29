//! Compiled source navigation is typed discovery, not an authored source or
//! native-plugin ABI. Frontends still prepare resources and use Session commands.
use serde::Serialize;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize)]
#[serde(rename_all="snake_case")]
pub enum SourceCreationKind {Browser,GameCapture,NdiSource,Image,ImageSlideShow,MediaSource,DisplayCapture,WindowCapture,VideoCapture,AudioInputCapture,AudioOutputCapture,ApplicationAudioCapture}
impl SourceCreationKind {
    pub fn label(self)->&'static str {match self {
        Self::Browser=>"Browser",Self::GameCapture=>"Game Capture",Self::NdiSource=>"NDI Source",Self::Image=>"Image",Self::ImageSlideShow=>"Image Slide Show",Self::MediaSource=>"Media Source",
        Self::DisplayCapture=>"Display Capture",Self::WindowCapture=>"Window Capture",Self::VideoCapture=>"Video Capture Device",
        Self::ApplicationAudioCapture=>"Application Audio Capture",Self::AudioInputCapture=>"Audio Input Capture",Self::AudioOutputCapture=>"Audio Output Capture",
    }}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize)]
#[serde(rename_all="snake_case")]
pub enum SourceCreationPlatform {Any,Windows,Macos}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize)]
pub struct SourceCreation {pub kind:SourceCreationKind,pub platform:SourceCreationPlatform}
pub fn plugin_source_creations(id:&str)->&'static [SourceCreation] {
    use SourceCreationKind::*;use SourceCreationPlatform::*;
    match id {
        crate::BROWSER_PLUGIN=>&[SourceCreation {kind:Browser,platform:Windows}],
        crate::NDI_PLUGIN=>&[SourceCreation {kind:NdiSource,platform:Windows}],
        "obsproject/obs-studio:image-source"=>&[SourceCreation {kind:Image,platform:Any},SourceCreation {kind:ImageSlideShow,platform:Any}],
        "obsproject/obs-studio:obs-ffmpeg"=>&[SourceCreation {kind:MediaSource,platform:Any}],
        "obsproject/obs-studio:win-capture"=>&[SourceCreation {kind:GameCapture,platform:Windows},SourceCreation {kind:DisplayCapture,platform:Windows},SourceCreation {kind:WindowCapture,platform:Windows}],
        "obsproject/obs-studio:win-dshow"=>&[SourceCreation {kind:VideoCapture,platform:Windows}],
        "obsproject/obs-studio:win-wasapi"=>&[SourceCreation {kind:AudioInputCapture,platform:Windows},SourceCreation {kind:AudioOutputCapture,platform:Windows},SourceCreation {kind:ApplicationAudioCapture,platform:Windows}],
        "obsproject/obs-studio:mac-avcapture"=>&[SourceCreation {kind:VideoCapture,platform:Macos}],
        _=>&[],
    }
}
