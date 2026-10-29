//! Immutable source-size confirmation; no pixels/IO or editable dimensions.
use super::Intent;
use eframe::egui;
use skylive_core::{Id,StudioSnapshot};
use skylive_host::source_output_resize::Plan;
use std::sync::atomic::{AtomicU64,Ordering};
static NEXT:AtomicU64=AtomicU64::new(1);
pub struct Draft {plan:Plan,token:u64,pub ready:bool,pub reason:Option<String>,pub rejected:Option<String>}
impl Draft {
    pub fn new(plan:Plan)->Result<Self,String>{let token=NEXT.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|value|value.checked_add(1)).map_err(|_|"Resize output confirmation identity exhausted. Restart SkyLive.".to_owned())?;Ok(Self{plan,token,ready:true,reason:None,rejected:None})}
    pub fn plan(&self)->&Plan{&self.plan}
    pub fn token(&self)->u64{self.token}
}
#[derive(Clone)]
struct Availability {session:u64,revision:u64,scene:Id,item:Id,reason:Option<String>}
/// One selected-row metadata receipt, never pixels or a retained model snapshot.
pub fn install(context:&egui::Context,session:u64,revision:u64,target:Option<(Id,Id)>,reason:Option<String>){
    let key=egui::Id::new("source-output-resize-availability");context.data_mut(|data|{data.remove::<Availability>(key);if let Some((scene,item))=target{data.insert_temp(key,Availability{session,revision,scene,item,reason});}});
}
pub fn button(ui:&mut egui::Ui,session:u64,model:&StudioSnapshot,scene:Id,item:Id,intents:&mut Vec<Intent>){
    let enabled=model.scene_container(scene).and_then(|scene|scene.items().iter().find(|row|row.id()==item)).and_then(|row|model.source(row.source())).is_some_and(|source|model.source_video_dimensions(source.kind()).is_ok_and(|value|value.is_some()));
    let runtime=ui.data(|data|data.get_temp::<Availability>(egui::Id::new("source-output-resize-availability"))).filter(|value|value.session==session&&value.revision==model.revision()&&value.scene==scene&&value.item==item);
    let ready=enabled&&runtime.as_ref().is_some_and(|value|value.reason.is_none());
    let reason=runtime.as_ref().and_then(|value|value.reason.as_deref()).unwrap_or("Select this source and wait for current video metadata.");
    if ui.add_enabled(ready,egui::Button::new(crate::i18n::tr(ui,"Resize output (source size)")))
        .on_hover_text(crate::i18n::tr(ui,"Use this source's current intrinsic video size for both canvas and output. Confirm before changing settings."))
        .on_disabled_hover_text(crate::i18n::tr(ui,if enabled{reason}else{"A source with actual video dimensions is required."}))
        .clicked(){intents.push(Intent::OpenSourceOutputResize{session,revision:model.revision(),scene,item});ui.close_menu();}
}
pub fn editor(ui:&mut egui::Ui,model:&StudioSnapshot,draft:&Draft){
    ui.label(crate::i18n::tr(ui,"The base and output resolutions will be resized to the size of the current source."));
    ui.label(crate::i18n::tf(ui,"Source: {name}",&[("{name}",draft.plan.name().to_owned())]));
    let size=draft.plan.size();
    ui.label(crate::i18n::tf(ui,"Source input: {width} × {height}",&[("{width}",size.raw_width.to_string()),("{height}",size.raw_height.to_string())]));
    ui.label(crate::i18n::tf(ui,"After source filters: {width} × {height}",&[("{width}",size.source_width.to_string()),("{height}",size.source_height.to_string())]));
    ui.label(crate::i18n::tf(ui,"New canvas and output: {width} × {height}",&[("{width}",size.width.to_string()),("{height}",size.height.to_string())]));
    ui.label(crate::i18n::tr(ui,"Width is rounded up to a multiple of four; height to a multiple of two. Frame rate and scaling algorithm are preserved."));
    ui.label(crate::i18n::tr(ui,if size.fits_item{"The selected source item will be fitted to the new canvas. Other item transforms are retained."}else{"The selected item is locked, so its transform will be retained."}));
    if model.revision()!=draft.plan.revision()||!draft.ready {if let Some(reason)=&draft.reason{ui.colored_label(ui.visuals().warn_fg_color,crate::i18n::tr(ui,reason));}}
    ui.label(crate::i18n::tr(ui,"Do you want to continue?"));
}
