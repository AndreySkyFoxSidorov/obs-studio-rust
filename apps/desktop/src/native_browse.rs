//! Native chooser button contract. The app captures the exact current draft.
use eframe::egui;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target { PendingFiles,MissingFiles,RemuxFiles,Lut,Mask,TextFont,TextFile,Image, Media, SlideFiles, SlideFolder, DocumentOpen, DocumentSave, Profile, RecordingFolder }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scope { pub session:u64, pub revision:u64, pub epoch:u64, pub available:bool }
#[derive(Clone, Copy, Debug)]
pub struct Request { pub target:Target, pub scope:Scope }
const SCOPE:&str="native-file-dialog-scope";
pub fn install(ctx:&egui::Context,scope:Scope) {ctx.data_mut(|d|d.insert_temp(egui::Id::new(SCOPE),scope));}
pub fn button(ui:&mut egui::Ui,label:&str,target:Target,enabled:bool)->Option<crate::views::Intent> {
    #[cfg(target_os="windows")]
    {
        let scope=ui.ctx().data(|d|d.get_temp::<Scope>(egui::Id::new(SCOPE)));
        let ready=enabled&&scope.is_some_and(|s|s.available);
        let response=ui.add_enabled(ready,egui::Button::new(crate::i18n::tr(ui,label)).wrap())
            .on_hover_text(crate::i18n::tr(ui,"Choose a path using the Windows file dialog. Cancel keeps this draft unchanged."))
            .on_disabled_hover_text(crate::i18n::tr(ui,"Wait for the active operation and keep the application window available before browsing."));
        if response.clicked() {return scope.map(|scope|crate::views::Intent::NativeBrowse(Request {target,scope}));}
        None
    }
    #[cfg(not(target_os="windows"))]
    {let _=(ui,label,target,enabled);None}
}
