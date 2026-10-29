//! Image options are an ordinary owned dialog draft; preparation stays on IO.
use super::{dialogs::Dialog,ImageEdit};
use eframe::egui;
use skylive_core::ImageSourceSettings;

pub fn options(ui:&mut egui::Ui,settings:&mut ImageSourceSettings){
    ui.checkbox(&mut settings.unload_when_hidden,crate::i18n::tr_key_or(ui,"Module.image-source.UnloadWhenNotShowing","Unload image when not showing"))
        .on_hover_text(crate::i18n::tr(ui,"Free memory used by this image when it is not shown. Images still used by another output remain loaded."));
    ui.checkbox(&mut settings.linear_alpha,crate::i18n::tr_key_or(ui,"Module.image-source.LinearAlpha","Apply alpha in linear space"))
        .on_hover_text(crate::i18n::tr(ui,"Apply transparency in linear color space for smoother blending around semi-transparent edges."));
}
impl Dialog {
    pub fn image_settings(&self)->Option<ImageSourceSettings>{match self{
        Self::AddImage{settings,..}|Self::ImageFile{settings,..}=>Some(*settings),_=>None,
    }}
    pub fn matches_image(&self,edit:&ImageEdit,path:&str,settings:ImageSourceSettings)->bool{
        match(self,edit){
            (Self::AddImage{scene,name,path:current,settings:actual},ImageEdit::Add{scene:target,name:submitted})=>scene==target&&name==submitted&&current==path&&*actual==settings,
            (Self::ImageFile{source,path:current,settings:actual},ImageEdit::Replace{source:target})=>source==target&&current==path&&*actual==settings,
            _=>false,
        }
    }
}
