//! Draft-only colors. Apply uses the common Settings transaction.
use eframe::egui;
use skylive_core::{Accessibility,ColorPreset,InterfaceColor,Command};
pub struct Draft {pub value:Accessibility}
impl Draft {
    pub fn new(value:Accessibility)->Self {Self {value}}
    pub fn unchanged(&self,current:Accessibility)->bool {self.value==current}
    pub fn command(&self)->Result<Command,String> {
        self.value.validate().map_err(|error|error.to_string())?;
        Ok(Command::SetAccessibility {settings:self.value})
    }
}
pub fn show(ui:&mut egui::Ui,draft:&mut Draft) {
    let mut enabled=draft.value.override_colors();
    if ui.checkbox(&mut enabled,crate::i18n::tr_key(ui,"Basic.Settings.Accessibility.ColorOverrides"))
        .on_hover_text(crate::i18n::tr(ui,"Override selection, crop, hover and audio meter colors. Apply publishes this palette to the interface.")).changed() {
        draft.value=draft.value.with_override(enabled);
    }
    ui.add_enabled_ui(enabled,|ui| {
        let mut preset=draft.value.preset();
        let key=|preset| match preset {
            ColorPreset::Default=>"Basic.Settings.Accessibility.ColorOverrides.Preset.Default",
            ColorPreset::ColorBlind1=>"Basic.Settings.Accessibility.ColorOverrides.Preset.ColorBlind1",
            ColorPreset::Custom=>"Basic.Settings.Accessibility.ColorOverrides.Preset.Custom",
        };
        let previous=preset;
        let label=ui.label(crate::i18n::tr_key(ui,"Basic.Settings.Accessibility.ColorOverrides.Preset"));
        egui::ComboBox::from_id_salt("accessibility-color-preset").selected_text(crate::i18n::tr_key(ui,key(preset))).show_ui(ui,|ui| {
            for value in [ColorPreset::Default,ColorPreset::ColorBlind1,ColorPreset::Custom] {
                ui.selectable_value(&mut preset,value,crate::i18n::tr_key(ui,key(value)))
                    .on_hover_text(crate::i18n::tr(ui,"Use the original OBS palette, its color-blind preset, or your own colors."));
            }
        }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choosing a predefined palette replaces the current draft colors."));
        if preset!=previous {draft.value=draft.value.with_preset(preset);}
        ui.separator();
        if ui.available_width()<360.0 {
            for role in InterfaceColor::ALL {
                ui.group(|ui| {
                    let label=ui.label(crate::i18n::tr_key(ui,role.locale_key()));
                    ui.horizontal_wrapped(|ui| {let hex=color_picker(ui,draft,role,label.id);ui.monospace(hex);});
                });
            }
            return;
        }
        egui::Grid::new("accessibility-colors").striped(true).min_col_width(210.0).show(ui,|ui| {
            for role in InterfaceColor::ALL {
                let label=ui.label(crate::i18n::tr_key(ui,role.locale_key()));
                let hex=color_picker(ui,draft,role,label.id);ui.monospace(hex);ui.end_row();
            }
        });
    });
    ui.label(crate::i18n::tr(ui,"These colors affect interface overlays and meters. They do not change video pixels, audio samples or output color encoding."));
}
fn color_picker(ui:&mut egui::Ui,draft:&mut Draft,role:InterfaceColor,label:egui::Id)->String {
    let mut color=draft.value.color(role);
    if ui.color_edit_button_srgb(&mut color).labelled_by(label)
        .on_hover_text(crate::i18n::tr(ui,"Choose this interface color. Editing a color selects the Custom preset.")).changed() {
        draft.value=draft.value.with_color(role,color);
    }
    format!("#{:02X}{:02X}{:02X}",color[0],color[1],color[2])
}
