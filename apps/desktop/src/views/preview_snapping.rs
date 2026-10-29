//! General-page pointer-alignment draft; no canonical mutation or OS work.
use eframe::egui;
use skylive_core::PreviewSnapping;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct Draft {baseline:PreviewSnapping,enabled:bool,screen_edges:bool,source_edges:bool,canvas_center:bool,distance_tenths:u16}
impl Draft {
    pub fn new(value:PreviewSnapping)->Self{Self{baseline:value,enabled:value.enabled(),screen_edges:value.screen_edges(),source_edges:value.source_edges(),canvas_center:value.canvas_center(),distance_tenths:value.distance_tenths()}}
    pub fn prepared(&self)->Result<PreviewSnapping,String>{PreviewSnapping::new(self.enabled,self.screen_edges,self.source_edges,self.canvas_center,self.distance_tenths).map_err(|error|error.to_string())}
    pub fn unchanged(&self,current:PreviewSnapping)->bool{self.prepared().is_ok_and(|value|value==current)}
    pub fn prepare_for(&self,current:PreviewSnapping)->Result<PreviewSnapping,String>{if self.baseline!=current{return Err("Preview snapping settings changed. Reopen General settings before applying.".into());}self.prepared()}
}
pub fn editor(ui:&mut egui::Ui,draft:&mut Draft){
    ui.label(crate::i18n::tr_key_or(ui,"Basic.Settings.General.Snapping","Source Alignment Snapping"));
    ui.push_id("preview-snapping-settings",|ui|{
        ui.checkbox(&mut draft.enabled,crate::i18n::tr(ui,"Enable"))
            .on_hover_text(crate::i18n::tr(ui,"Align dragged sources with canvas edges, adjacent source edges or the canvas center. Hold Ctrl to move freely."));
        ui.add_enabled_ui(draft.enabled,|ui|{
            ui.horizontal_wrapped(|ui|{
                let label=ui.label(crate::i18n::tr_key_or(ui,"Basic.Settings.General.SnapDistance","Snap Sensitivity"));
                ui.add(egui::DragValue::new(&mut draft.distance_tenths).range(0..=1000).speed(5.0)
                    .custom_formatter(|value,_|format!("{:.1}",value/10.0))
                    .custom_parser(|text|text.trim().parse::<f64>().ok().filter(|value|value.is_finite()&&(0.0..=100.0).contains(value)).map(|value|(value*10.0).round())))
                    .labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Maximum alignment distance in display pixels; it follows Preview zoom. Zero disables snapping."));
            });
            for(value,key,fallback,hint)in[
                (&mut draft.screen_edges,"Basic.Settings.General.ScreenSnapping","Snap Sources to edge of screen","Align the visible source bounds with the four canvas edges."),
                (&mut draft.source_edges,"Basic.Settings.General.SourceSnapping","Snap Sources to other sources","Align facing source edges when their perpendicular spans overlap."),
                (&mut draft.canvas_center,"Basic.Settings.General.CenterSnapping","Snap Sources to horizontal and vertical center","Align the dragged source center with the horizontal or vertical canvas center."),
            ]{ui.checkbox(value,crate::i18n::tr_key_or(ui,key,fallback)).on_hover_text(crate::i18n::tr(ui,hint)).on_disabled_hover_text(crate::i18n::tr(ui,"Enable source alignment snapping to edit these options."));}
        });
        if ui.button(crate::i18n::tr(ui,"Restore OBS snapping defaults")).on_hover_text(crate::i18n::tr(ui,"Restore enabled edge snapping at 10 pixels, with center snapping off, in this draft.")).clicked(){let baseline=draft.baseline;*draft=Draft::new(PreviewSnapping::default());draft.baseline=baseline;}
    });
}
