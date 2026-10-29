//! Item-scoped menu adapter. The app rechecks the complete selected context.
use eframe::egui;
use skylive_core::{BlendMode,BlendingMethod,ScaleFiltering,ItemCompositing,Id,StudioSnapshot};
#[derive(Clone,Copy,Debug)]
pub struct Focus {pub session:u64,pub revision:u64,pub root:Id,pub selected:Option<Id>}
#[derive(Clone,Copy)]
pub struct Submission {pub focus:Focus,pub container:Id,pub item:Id,pub source:Id,pub before:ItemCompositing,pub settings:ItemCompositing}
const FOCUS:&str="item-compositing-focus";
pub fn install(ctx:&egui::Context,focus:Focus) {ctx.data_mut(|d|d.insert_temp(egui::Id::new(FOCUS),focus));}
pub fn menus(ui:&mut egui::Ui,model:&StudioSnapshot,container:Id,item:Id,session:u64,intents:&mut Vec<super::Intent>) {
    let Some((owner,entry))=model.item_container(item).filter(|(owner,_)|*owner==container) else {return;};
    let Some(source)=model.source(entry.source()) else {return;};
    let visual=model.source_video_dimensions(source.kind()).is_ok_and(|d|d.is_some());
    let focus=ui.ctx().data(|d|d.get_temp::<Focus>(egui::Id::new(FOCUS)));
    let available=visual&&focus.is_some_and(|f|f.session==session&&f.revision==model.revision()
        &&f.selected==Some(item)&&model.root_scene_for_container(owner)==Some(f.root));
    let old=entry.compositing();
    let mut selected=None;
    let reason=if !visual {"Audio-only items have no video compositing settings."}else{"Select this source before changing its item compositing."};
    ui.add_enabled_ui(available,|ui| {
        ui.menu_button(crate::i18n::tr_key_or(ui,"BlendingMode","Blending Mode"),|ui| {
            for mode in BlendMode::ALL {
                let key=match mode {BlendMode::Normal=>"BlendingMode.Normal",BlendMode::Additive=>"BlendingMode.Additive",BlendMode::Subtract=>"BlendingMode.Subtract",BlendMode::Screen=>"BlendingMode.Screen",BlendMode::Multiply=>"BlendingMode.Multiply",BlendMode::Lighten=>"BlendingMode.Lighten",BlendMode::Darken=>"BlendingMode.Darken"};
                if ui.radio(old.blend_mode==mode,crate::i18n::tr_key_or(ui,key,mode.label()))
                    .on_hover_text(crate::i18n::tr(ui,"Blend this scene item with the layers below it. Other references to the source keep their own settings.")).clicked() {
                    selected=Some(ItemCompositing {blend_mode:mode,..old});ui.close_menu();
                }
            }
        }).response.on_hover_text(crate::i18n::tr(ui,"Color and alpha use OBS blend factors; Subtract, Lighten and Darken also affect alpha."));
        ui.menu_button(crate::i18n::tr_key_or(ui,"BlendingMethod","Blending Method"),|ui| {
            for method in [BlendingMethod::Default,BlendingMethod::SrgbOff] {
                let key=match method {BlendingMethod::Default=>"BlendingMethod.Default",BlendingMethod::SrgbOff=>"BlendingMethod.SrgbOff"};
                if ui.radio(old.blending_method==method,crate::i18n::tr_key_or(ui,key,method.label()))
                    .on_hover_text(crate::i18n::tr(ui,if method==BlendingMethod::Default {"Use source-aware linear-light SDR blending."}else{"Blend encoded SDR values instead of linear-light values for this item."})).clicked() {
                    selected=Some(ItemCompositing {blending_method:method,..old});ui.close_menu();
                }
            }
        }).response.on_hover_text(crate::i18n::tr(ui,"Choose the color space used for this item's blend arithmetic."));
        ui.menu_button(crate::i18n::tr_key_or(ui,"ScaleFiltering","Scale Filtering"),|ui| {
            for filter in [ScaleFiltering::Disabled,ScaleFiltering::Point,ScaleFiltering::Bilinear,ScaleFiltering::Bicubic,ScaleFiltering::Lanczos,ScaleFiltering::Area] {
                let key=match filter {ScaleFiltering::Disabled=>"Disable",ScaleFiltering::Point=>"ScaleFiltering.Point",ScaleFiltering::Bilinear=>"ScaleFiltering.Bilinear",ScaleFiltering::Bicubic=>"ScaleFiltering.Bicubic",ScaleFiltering::Lanczos=>"ScaleFiltering.Lanczos",ScaleFiltering::Area=>"ScaleFiltering.Area"};
                if ui.radio(old.scale_filter==filter,crate::i18n::tr_key_or(ui,key,filter.label()))
                    .on_hover_text(crate::i18n::tr(ui,match filter {ScaleFiltering::Disabled=>"Keep the current nearest-neighbor default.",ScaleFiltering::Point=>"Sample the nearest source pixel without interpolation.",ScaleFiltering::Bilinear=>"Interpolate four premultiplied source pixels without sampling outside the crop.",ScaleFiltering::Bicubic=>"Sharpened bicubic scaling. Uses OBS low-resolution sampling below half size.",ScaleFiltering::Lanczos=>"Sharp six-tap Lanczos scaling. Uses OBS low-resolution sampling below half size.",ScaleFiltering::Area=>"Average the source coverage of each output pixel, including rotated and reflected footprints. Strong downscales use OBS low-resolution sampling."})).clicked() {
                    selected=Some(ItemCompositing {scale_filter:filter,..old});ui.close_menu();
                }
            }
        }).response.on_hover_text(crate::i18n::tr(ui,"Choose how this scene item is resampled. Bicubic, Lanczos and Area use bounded CPU processing."));
    }).response.on_disabled_hover_text(crate::i18n::tr(ui,reason));
    if let (Some(focus),Some(settings))=(focus,selected.filter(|s|*s!=old)) {
        intents.push(super::Intent::ItemCompositing(Submission {focus,container,item,source:entry.source(),before:old,settings}));
    }
}
