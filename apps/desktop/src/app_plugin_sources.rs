//! Fresh manager request -> existing typed Add workflow. No direct model edit.
use super::*;
use crate::views::plugin_sources::{PluginSourceRequest,SourceCapabilities};
use skylive_core::{PluginOperation,SourceCreationKind};
impl StudioApp {
    pub(super) fn source_creation_capabilities(&self)->SourceCapabilities {
        let audio=self.audio.capabilities();
        SourceCapabilities {platform:self.platform,preview:Some(self.session.preview()),display:self.capture.capabilities().available,window:self.capture.window_capabilities().available,camera:self.camera.capabilities().available,audio_input:audio.input,audio_output:audio.output,ndi:skylive_host::ndi::NdiOwner::available()}
    }
    pub(super) fn open_plugin_source(&mut self,request:PluginSourceRequest,context:&egui::Context){
        let current=request.manager!=0&&matches!(&self.dialog,Some(Dialog::Plugins {session,token,..}) if *session==request.session && *token==request.manager);
        if !current||self.session.is_closed()||request.session!=self.session.identity()||request.revision!=self.session.snapshot().revision(){self.message(true,"The document or Plugin Manager changed. Choose the source target again.");return;}
        let model=self.session.snapshot();
        let registration=skylive_core::plugin_source_creations(&request.plugin).iter().find(|r|r.kind==request.kind&&self.source_creation_capabilities().matches_platform(r.platform));
        if !model.preferences().plugin_enabled(&request.plugin)
            ||!skylive_core::plugin_entry_points(&request.plugin).iter().any(|entry|entry.operation==PluginOperation::Sources)
            ||registration.is_none_or(|r|!self.source_creation_capabilities().available(*r)) {
            self.message(true,"This module has no available source provider for this operation on this platform.");return;
        }
        if model.scene(request.scene).is_none(){self.message(true,"The selected target scene no longer exists.");return;}
        if model.sources().len()>=skylive_core::MAX_SOURCES {self.message(true,"The source catalog is full.");return;}
        let scene=request.scene;
        let name=unique_name(request.kind.label(),model.sources().iter().map(|s|s.name()));
        let intent=match request.kind {
            SourceCreationKind::Browser=>Intent::OpenDialog(Dialog::BrowserSource(views::browser_source::Draft::add(scene,name))),
            SourceCreationKind::GameCapture=>Intent::OpenDialog(Dialog::GameCapture(views::game_capture::Draft::add(scene,name))),
            SourceCreationKind::NdiSource=>Intent::OpenDialog(Dialog::NdiSource(views::ndi_source::NdiDraft::add(scene,name))),
            SourceCreationKind::Image=>Intent::OpenDialog(Dialog::AddImage {scene,name,path:String::new(),settings:Default::default()}),
            SourceCreationKind::ImageSlideShow=>Intent::OpenDialog(Dialog::ImageSlideShow(views::image_slideshow::SlideShowDraft::add(scene,name))),
            SourceCreationKind::MediaSource=>Intent::OpenDialog(Dialog::MediaSource(views::media_source::MediaDraft::add(scene,name))),
            SourceCreationKind::DisplayCapture=>Intent::OpenDisplayCapture {edit:views::DisplayEdit::Add {scene},name,binding:None},
            SourceCreationKind::WindowCapture=>Intent::OpenWindowCapture {edit:views::WindowEdit::Add {scene},name,binding:None},
            SourceCreationKind::VideoCapture=>Intent::OpenCameraCapture {edit:views::CameraEdit::Add {scene},name,binding:None},
            SourceCreationKind::ApplicationAudioCapture=>Intent::OpenDialog(Dialog::application_audio(views::AudioEdit::Add {scene},name)),
            SourceCreationKind::AudioInputCapture=>Intent::OpenDialog(Dialog::audio(views::AudioEdit::Add {scene},name,skylive_core::AudioFlow::Input)),
            SourceCreationKind::AudioOutputCapture=>Intent::OpenDialog(Dialog::audio(views::AudioEdit::Add {scene},name,skylive_core::AudioFlow::Output)),
        };
        self.intent(intent,context);
    }
}
fn unique_name<'a>(base:&str,names:impl Iterator<Item=&'a str>)->String {
    let names:std::collections::HashSet<_>=names.collect();
    if !names.contains(base){return base.to_owned();}
    for index in 2..=skylive_core::MAX_SOURCES+1 {let candidate=format!("{base} {index}");if !names.contains(candidate.as_str()){return candidate;}}
    base.to_owned()
}
