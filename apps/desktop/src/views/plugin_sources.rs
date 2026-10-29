//! Source creation navigation; no source is inserted merely by enabling or
//! selecting a module. Existing Add dialogs perform real preparation/commit.
use super::Intent;
use crate::platform::CapturePlatform;
use eframe::egui;
use skylive_core::{Id,SourceCreation,SourceCreationKind,SourceCreationPlatform,StudioSnapshot};
#[derive(Clone,Debug)]
pub struct PluginSourceRequest {
    pub session:u64,pub revision:u64,pub manager:u64,pub plugin:String,
    pub scene:Id,pub kind:SourceCreationKind,
}
#[derive(Clone,Copy)]
pub struct SourceCapabilities {
    pub platform:CapturePlatform,pub preview:Option<Id>,pub display:bool,pub window:bool,
    pub camera:bool,pub audio_input:bool,pub audio_output:bool,pub ndi:bool,
}
impl Default for SourceCapabilities {
    fn default()->Self {Self {platform:CapturePlatform::Unavailable,preview:None,display:false,window:false,camera:false,audio_input:false,audio_output:false,ndi:false}}
}
impl SourceCapabilities {
    pub fn matches_platform(self,platform:SourceCreationPlatform)->bool {match platform {
        SourceCreationPlatform::Any=>true,
        SourceCreationPlatform::Windows=>self.platform==CapturePlatform::Windows,
        SourceCreationPlatform::Macos=>self.platform==CapturePlatform::Macos,
    }}
    pub fn available(self,registration:SourceCreation)->bool {
        if !self.matches_platform(registration.platform){return false;}
        match registration.kind {
            SourceCreationKind::Browser=>self.platform==CapturePlatform::Windows,
            SourceCreationKind::GameCapture=>cfg!(all(target_os="windows",target_arch="x86_64")),
            SourceCreationKind::NdiSource=>self.ndi,
            SourceCreationKind::Image|SourceCreationKind::ImageSlideShow|SourceCreationKind::MediaSource=>true,
            SourceCreationKind::DisplayCapture=>self.display,SourceCreationKind::WindowCapture=>self.window,
            SourceCreationKind::ApplicationAudioCapture=>self.platform==CapturePlatform::Windows && self.audio_output,
            SourceCreationKind::VideoCapture=>self.camera,SourceCreationKind::AudioInputCapture=>self.audio_input,SourceCreationKind::AudioOutputCapture=>self.audio_output,
        }
    }
    pub fn install(self,context:&egui::Context){context.data_mut(|d|d.insert_temp(egui::Id::new("plugin-source-capabilities"),self));}
    pub fn from_ui(ui:&egui::Ui)->Self {ui.ctx().data(|d|d.get_temp(egui::Id::new("plugin-source-capabilities"))).unwrap_or_default()}
}
pub fn module_on_platform(ui:&egui::Ui,plugin:&str)->bool {
    let registrations=skylive_core::plugin_source_creations(plugin);
    registrations.is_empty()||registrations.iter().any(|r|SourceCapabilities::from_ui(ui).matches_platform(r.platform))
}
pub fn show(ui:&mut egui::Ui,model:&StudioSnapshot,plugin:&str,identity:(u64,u64),intents:&mut Vec<Intent>){
    ui.strong(crate::i18n::tr(ui,"Sources"));
    let capabilities=SourceCapabilities::from_ui(ui);
    let registrations:Vec<_>=skylive_core::plugin_source_creations(plugin).iter().filter(|r|capabilities.matches_platform(r.platform)).copied().collect();
    if registrations.is_empty(){ui.label(crate::i18n::tr(ui,"This module's sources are available on another operating system."));return;}
    let key=ui.id().with(("plugin-source-scene",plugin,identity.0,identity.1));
    let selected=ui.data(|d|d.get_temp::<Id>(key)).filter(|id|model.scene(*id).is_some())
        .or(capabilities.preview.filter(|id|model.scene(*id).is_some())).or_else(||model.scenes().first().map(|s|s.id()));
    let Some(mut scene)=selected else {ui.label(crate::i18n::tr(ui,"Create a scene before adding a source."));return;};
    let label=ui.label(crate::i18n::tr(ui,"Target scene"));
    let name=model.scene(scene).map_or("",|s|s.name());
    egui::ComboBox::from_id_salt(("plugin-source-scene",plugin)).width(ui.available_width()).truncate().selected_text(name).show_ui(ui,|ui| {
        for target in model.scenes(){ui.selectable_value(&mut scene,target.id(),target.name()).on_hover_text(target.name());}
    }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose the scene that will receive this source"));
    ui.data_mut(|d|d.insert_temp(key,scene));
    let active=model.preferences().plugin_enabled(plugin);
    let capacity=model.sources().len()<skylive_core::MAX_SOURCES;
    for registration in registrations {
        let enabled=active&&capacity&&capabilities.available(registration);
        let label=crate::i18n::tf(ui,"Add {source}",&[("{source}",crate::i18n::tr(ui,registration.kind.label()))]);
        if ui.add_enabled(enabled,egui::Button::new(label))
            .on_hover_text(crate::i18n::tr(ui,"Open source properties for the chosen scene; Apply creates the source"))
            .on_disabled_hover_text(crate::i18n::tr(ui,if !capacity {"The source catalog is full."}else if !active {"Enable this module before adding its sources."}else{"This native source backend is unavailable in this build."}))
            .clicked(){intents.push(Intent::OpenPluginSource(PluginSourceRequest {session:identity.0,revision:model.revision(),manager:identity.1,plugin:plugin.into(),scene,kind:registration.kind}));}
    }
    ui.small(crate::i18n::tr(ui,"Built-in source modules are always active. Adding a source opens its existing properties; it does not install a native OBS plugin."));
}
