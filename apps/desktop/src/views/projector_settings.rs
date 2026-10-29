//! Projector defaults draft; opens no external windows or native services.
use eframe::egui;use skylive_core::{ProjectorPreferences,MultiviewLayout,Command};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct Draft{baseline:ProjectorPreferences,value:ProjectorPreferences}
impl Draft{
 pub fn new(value:ProjectorPreferences)->Self{Self{baseline:value,value}}
 pub fn prepared(&self)->ProjectorPreferences{self.value}
 pub fn unchanged(&self,current:ProjectorPreferences)->bool{self.value==current}
 pub fn command(&self,current:ProjectorPreferences)->Result<Command,String>{if self.baseline!=current{return Err("Projector defaults changed. Reopen this page before applying.".into());}Ok(Command::SetProjectorPreferences{preferences:self.value})}
}
pub fn editor(ui:&mut egui::Ui,draft:&mut Draft){
 ui.label(crate::i18n::tr(ui,"Projector defaults"));
 let mut value=draft.value.hide_cursor();if ui.checkbox(&mut value,crate::i18n::tr(ui,"Hide cursor over fullscreen projectors")).on_hover_text(crate::i18n::tr(ui,"Hide the pointer in fullscreen single-view projectors; Multiview and context menus keep it visible.")).changed(){draft.value=draft.value.with_hide_cursor(value);}
 let mut value=draft.value.always_on_top();if ui.checkbox(&mut value,crate::i18n::tr(ui,"Make projectors always on top")).on_hover_text(crate::i18n::tr(ui,"Apply this window level to open projectors and use it for newly opened projectors.")).changed(){draft.value=draft.value.with_always_on_top(value);}
 let mut value=draft.value.save_windows();if ui.checkbox(&mut value,crate::i18n::tr_key_or(ui,"Basic.Settings.General.SaveProjectors","Save projectors on exit")).on_hover_text(crate::i18n::tr(ui,"Save projector routes with this native collection and restore them when that native collection is opened. Enabling this setting opens no windows.")).changed(){draft.value=draft.value.with_save_windows(value);}
 let mut value=draft.value.limit_fullscreen_per_monitor();if ui.checkbox(&mut value,crate::i18n::tr(ui,"Limit one fullscreen projector per monitor")).on_hover_text(crate::i18n::tr(ui,"When a fullscreen projector is explicitly opened or moved to a monitor, close other projectors using that monitor. Enabling this setting alone closes nothing.")).changed(){draft.value=draft.value.with_limit_fullscreen_per_monitor(value);}
 ui.separator();ui.label(crate::i18n::tr(ui,"Multiview defaults"));
 let label=ui.label(crate::i18n::tr(ui,"Multiview layout"));let mut layout=draft.value.layout();egui::ComboBox::from_id_salt("projector-default-layout").selected_text(crate::i18n::tr(ui,layout.label())).show_ui(ui,|ui|{for option in MultiviewLayout::ALL{ui.selectable_value(&mut layout,option,crate::i18n::tr(ui,option.label()));}}).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose the scene grid used by open and future Multiview windows."));draft.value=draft.value.with_layout(layout);
 let mut value=draft.value.labels();if ui.checkbox(&mut value,crate::i18n::tr(ui,"Show Scene Names")).on_hover_text(crate::i18n::tr(ui,"Draw authored scene names and Preview/Program labels over the corresponding tiles.")).changed(){draft.value=draft.value.with_labels(value);}
 let mut value=draft.value.safe_areas();if ui.add_enabled(!draft.value.layout().scenes_only(),egui::Checkbox::new(&mut value,crate::i18n::tr(ui,"Show Safe Areas"))).on_disabled_hover_text(crate::i18n::tr(ui,"This layout has no Preview tile.")).on_hover_text(crate::i18n::tr(ui,"Draw the original OBS safe-area guides on Preview tiles when the layout includes Preview.")).changed(){draft.value=draft.value.with_safe_areas(value);}
 let mut value=draft.value.mouse_switching();if ui.checkbox(&mut value,crate::i18n::tr(ui,"Switch Scenes on Click")).on_hover_text(crate::i18n::tr(ui,"Click a scene tile to select it in Preview, or switch it normally outside Studio Mode.")).changed(){draft.value=draft.value.with_mouse_switching(value);}
 let mut value=draft.value.double_click_transition();if ui.add_enabled(draft.value.mouse_switching(),egui::Checkbox::new(&mut value,crate::i18n::tr(ui,"Transition on Double-click"))).on_disabled_hover_text(crate::i18n::tr(ui,"Enable scene switching on click to use double-click transitions.")).on_hover_text(crate::i18n::tr(ui,"In Studio Mode, double-click a scene tile to send it to Program with the selected transition.")).changed(){draft.value=draft.value.with_double_click_transition(value);}
 ui.label(crate::i18n::tr(ui,"Apply updates the changed defaults on open projectors. Local window choices survive changes to unrelated defaults. No external window opens from this page."));
 ui.label(crate::i18n::tr(ui,"Scene visibility is configured with Show in Multiview in each scene context menu."));
 if ui.button(crate::i18n::tr(ui,"Restore OBS projector defaults")).on_hover_text(crate::i18n::tr(ui,"Restore the OBS 32.2.2 projector and Multiview defaults in this draft.")).clicked(){draft.value=ProjectorPreferences::default();}
}
