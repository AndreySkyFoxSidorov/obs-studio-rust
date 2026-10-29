//! Driver values are external live state; this editor never submits a scene
//! command, records Undo, saves hardware values, or promises Cancel rollback.
use super::{CameraEdit, Intent, camera_capture::CameraDraft};
use eframe::egui;
use skylive_host::camera_capture::{
    ControlCatalog, ControlChange, ControlMode, ControlRange, ControlReceipt, ControlSetting, ControlPhase, CameraControlId,
};

/// Move to the next value on the driver's grid, which is anchored at its
/// minimum (not necessarily at zero). Use i64 to handle the full i32 range.
fn step_driver_value(range:ControlRange, value:i32, increase:bool)->Option<i32> {
    if !range.valid() || !range.supports(ControlMode::Manual) {return None;}
    let minimum=i64::from(range.minimum);
    let step=i64::from(range.step);
    let offset=i64::from(value.clamp(range.minimum,range.maximum))-minimum;
    let quotient=offset/step;
    let index=if increase {quotient.checked_add(1)?}
        else if offset%step==0 {quotient.checked_sub(1)?} else {quotient};
    let target=minimum.checked_add(index.checked_mul(step)?)?;
    if !(minimum..=i64::from(range.maximum)).contains(&target) {return None;}
    i32::try_from(target).ok()
}

#[derive(Default)]
pub struct DriverDraft {
    pub catalog: Option<ControlCatalog>,
    pub loading: bool,
    pub message: Option<String>,
    receipt: Option<ControlReceipt>,
    edits: Vec<(skylive_host::camera_capture::CameraControlId, i32, ControlMode)>,
}
impl DriverDraft {
    pub fn loaded(&mut self, catalog: ControlCatalog) {
        self.edits = catalog.query.controls.iter().map(|p| (p.id,p.observed.value,p.observed.mode)).collect();
        self.catalog=Some(catalog);
        self.receipt=None;
        self.loading=false;
        self.message=None;
    }
    pub fn receipt(&mut self, receipt: &ControlReceipt) {
        self.loading=false;
        self.message=None;
        self.receipt=Some(receipt.clone());
        if let Some(value)=receipt.observed {
            if let Some(catalog)=&mut self.catalog {
                if let Some(row)=catalog.query.controls.iter_mut().find(|row| row.id==receipt.requested.id) {row.observed=value;}
            }
            if let Some(edit)=self.edits.iter_mut().find(|edit| edit.0==receipt.requested.id) {edit.1=value.value;edit.2=value.mode;}
        }
    }
}
fn control_label(context:&egui::Context,id:CameraControlId)->String {
    let key=format!("SkyLive.CameraControl.{}",id.label().replace(' ',""));
    // Context-specific keys keep camera Pan distinct from the audio panorama.
    crate::i18n::catalog(context).map(|catalog|catalog.translate(&key).to_owned())
        .filter(|value|value!=&key).unwrap_or_else(||id.label().to_owned())
}
fn mode_label(context:&egui::Context,mode:ControlMode)->String {
    crate::i18n::tr_context(context,match mode {ControlMode::Auto=>"Auto",ControlMode::Manual=>"Manual"})
}
fn diagnostic(context:&egui::Context,d:&skylive_host::camera_capture::ControlDiagnostic)->String {
    crate::i18n::tf_context(context,"{property} · {operation}: {detail} · HRESULT {hresult} · native values {values}",
        &[("{property}",d.property.map_or_else(||crate::i18n::tr_context(context,"Interface"),|id|control_label(context,id))),
          ("{operation}",d.operation.into()),("{detail}",crate::i18n::tr_context(context,d.detail)),
          ("{hresult}",format!("{:?}",d.hresult)),("{values}",format!("{:?}",d.native_values))])
}
pub fn summary(context:&egui::Context,receipt:&ControlReceipt)->String {
    let state=crate::i18n::tr_context(context,if receipt.write_succeeded {"Driver Set succeeded"}
        else if receipt.write_started {"Driver Set failed; hardware state may have changed"}else{"Driver was not written"});
    let phase=crate::i18n::tr_context(context,match receipt.phase {ControlPhase::BeforeWrite=>"Before write",ControlPhase::AfterWrite=>"After write",ControlPhase::Readback=>"Readback"});
    let mut text=crate::i18n::tf_context(context,"{control} · {phase}: {state}",
        &[("{control}",control_label(context,receipt.requested.id)),("{phase}",phase),("{state}",state)]);
    if let Some(value)=receipt.observed {
        text.push_str(&crate::i18n::tf_context(context," · readback {value} ({mode})",
            &[("{value}",value.value.to_string()),("{mode}",mode_label(context,value.mode))]));
    }
    if let Some(detail)=&receipt.diagnostic {text.push_str(" · ");text.push_str(&diagnostic(context,detail));}
    text
}
pub fn editor(ui:&mut egui::Ui,draft:&mut CameraDraft,intents:&mut Vec<Intent>) {
    if !matches!(draft.platform,crate::platform::CapturePlatform::Windows) {return;}
    let source=match draft.edit {CameraEdit::Rebind {source}=>Some(source),CameraEdit::Add {..}=>None};
    let selected_matches=draft.original.as_ref().is_some_and(|b| draft.selected.as_ref()==Some(b.target()) && draft.selected_mode==Some(b.mode()));
    if !selected_matches {
        draft.driver.catalog=None;
        draft.driver.receipt=None;
        draft.driver.edits.clear();
    }
    egui::CollapsingHeader::new(crate::i18n::tr(ui,"Camera driver controls"))
        .id_salt("camera-driver-controls").default_open(true).show(ui,|ui| {
        ui.small(crate::i18n::tr(ui,"Live hardware settings. Apply each control directly; closing or cancelling this dialog does not restore camera values."));
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(source.is_some()&&selected_matches&&!draft.driver.loading,egui::Button::new(crate::i18n::tr(ui,"Read driver controls")))
                .on_hover_text(crate::i18n::tr(ui,"Read actual ranges and current values from this running camera; unsupported properties are not shown"))
                .on_disabled_hover_text(crate::i18n::tr(ui,"Bind and start this exact camera mode first, then reopen its properties"))
                .clicked() {
                if let Some(source)=source {intents.push(Intent::QueryCameraControls {token:draft.token,source});}
            }
            if draft.driver.loading {ui.spinner();}
        });
        if let Some(message)=&draft.driver.message {ui.label(crate::i18n::tr(ui,message));}
        else if !draft.driver.loading {if let Some(receipt)=&draft.driver.receipt {ui.label(summary(ui.ctx(),receipt));}}
        let Some(catalog)=&draft.driver.catalog else {return;};
        if catalog.query.controls.is_empty() {ui.label(crate::i18n::tr(ui,"The driver did not expose standard camera controls for this source."));}
        egui::ScrollArea::vertical().id_salt("camera-driver-rows").max_height(250.0).show(ui,|ui| {
            for (index,row) in catalog.query.controls.iter().enumerate() {
                let Some(edit)=draft.driver.edits.get_mut(index) else {continue;};
                ui.push_id(index,|ui| {ui.group(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(control_label(ui.ctx(),row.id));
                        ui.label(crate::i18n::tf(ui,"{value} · {mode}",&[("{value}",row.observed.value.to_string()),("{mode}",mode_label(ui.ctx(),row.observed.mode))])).on_hover_text(crate::i18n::tr(ui,"Last driver readback; raw native units"));
                    });
                    ui.add_enabled_ui(!draft.driver.loading,|ui| {ui.horizontal_wrapped(|ui| {
                        if row.range.supports(ControlMode::Auto) {ui.selectable_value(&mut edit.2,ControlMode::Auto,crate::i18n::tr(ui,"Auto")).on_hover_text(crate::i18n::tr(ui,"Let the camera driver control this property"));}
                        if row.range.supports(ControlMode::Manual) {ui.selectable_value(&mut edit.2,ControlMode::Manual,crate::i18n::tr(ui,"Manual")).on_hover_text(crate::i18n::tr(ui,"Use an explicit native driver value"));}
                        let previous=step_driver_value(row.range,edit.1,false);
                        if ui.add_enabled(edit.2==ControlMode::Manual && previous.is_some(),egui::Button::new(crate::i18n::tr_key_or(ui,"SkyLive.CameraControl.StepDown","Step −")))
                            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.CameraControl.StepDownHint","Choose the preceding value allowed by the camera driver."))
                            .clicked() {if let Some(value)=previous {edit.1=value;}}
                        ui.add_enabled(edit.2==ControlMode::Manual,egui::DragValue::new(&mut edit.1).range(row.range.minimum..=row.range.maximum).speed(f64::from(row.range.step)))
                            .on_hover_text(crate::i18n::tf(ui,"{minimum}…{maximum}; step {step}; driver default {default}",
                                &[("{minimum}",row.range.minimum.to_string()),("{maximum}",row.range.maximum.to_string()),("{step}",row.range.step.to_string()),("{default}",row.range.default.to_string())]));
                        let next=step_driver_value(row.range,edit.1,true);
                        if ui.add_enabled(edit.2==ControlMode::Manual && next.is_some(),egui::Button::new(crate::i18n::tr_key_or(ui,"SkyLive.CameraControl.StepUp","Step +")))
                            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.CameraControl.StepUpHint","Choose the next value allowed by the camera driver."))
                            .clicked() {if let Some(value)=next {edit.1=value;}}
                        let setting=if edit.2==ControlMode::Auto {ControlSetting::Auto} else {ControlSetting::Manual(edit.1)};
                        let valid=edit.2==ControlMode::Auto || row.range.manual_value(edit.1);
                        if ui.add_enabled(valid,egui::Button::new(crate::i18n::tr(ui,"Apply")))
                            .on_hover_text(crate::i18n::tr(ui,"Write this control to the running camera and read back its actual value"))
                            .on_disabled_hover_text(crate::i18n::tr(ui,"Value must match the reported driver range and step"))
                            .clicked() {intents.push(Intent::ApplyCameraControl {token:draft.token,lease:catalog.lease.clone(),change:ControlChange {id:row.id,setting}});}
                        if row.range.manual_value(row.range.default) && ui.button(crate::i18n::tr(ui,"Default"))
                            .on_hover_text(crate::i18n::tr(ui,"Write the driver's reported default value in manual mode"))
                            .clicked() {intents.push(Intent::ApplyCameraControl {token:draft.token,lease:catalog.lease.clone(),change:ControlChange {id:row.id,setting:ControlSetting::ManualDefault}});}
                    });});
                });});
            }
        });
        if !catalog.query.diagnostics.is_empty() {
            egui::CollapsingHeader::new(crate::i18n::tr(ui,"Unavailable driver properties")).show(ui,|ui| {
                for d in &catalog.query.diagnostics {ui.label(diagnostic(ui.ctx(),d));}
            });
        }
    });
}
