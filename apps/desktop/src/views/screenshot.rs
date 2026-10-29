//! Screenshot entries carry only the selected canonical identity.
use eframe::egui;
use super::Intent;
use skylive_host::screenshot::Target;
pub fn button(ui:&mut egui::Ui,session:u64,revision:u64,target:Target,enabled:bool,intents:&mut Vec<Intent>){
    if ui.add_enabled(enabled,egui::Button::new(crate::i18n::tr(ui,target.label())))
        .on_hover_text(crate::i18n::tr(ui,"Choose a PNG destination, then save a full-size rendered frame with its current filters. Existing files are preserved."))
        .on_disabled_hover_text(crate::i18n::tr(ui,"A screenshot requires an available video source and the application window."))
        .clicked(){intents.push(Intent::Screenshot {session,revision,target});ui.close_menu();}
}
