//! UI receives actual output endpoint metadata and native status; no OS work.
use super::Intent;
use eframe::egui;
use skylive_core::{AudioMonitoringDevice,AudioMonitoringMode,Id,StudioSnapshot};
use skylive_host::{audio_capture::AudioDeviceInfo,audio_monitor::MonitorStatus};
#[derive(Clone,Default)]
pub struct MonitoringView {pub session:u64,pub available:bool,pub loading:bool,pub devices:Vec<AudioDeviceInfo>,pub error:Option<String>,pub status:MonitorStatus}
impl MonitoringView {
    pub fn install(self,context:&egui::Context){context.data_mut(|data|data.insert_temp(egui::Id::new("audio-monitoring-view"),self));}
    fn get(ui:&egui::Ui)->Self {ui.ctx().data(|d|d.get_temp(egui::Id::new("audio-monitoring-view"))).unwrap_or_default()}
}
pub fn session(ui:&egui::Ui)->u64 {MonitoringView::get(ui).session}
pub(super) fn available(ui:&egui::Ui,session:u64)->bool {let view=MonitoringView::get(ui);view.available&&view.session==session}
pub fn mode(ui:&mut egui::Ui,session:u64,revision:u64,source:Id,current:AudioMonitoringMode,intents:&mut Vec<Intent>){
    let view=MonitoringView::get(ui);let available=view.available&&view.session==session;
    let mut selected=current;
    egui::ComboBox::from_id_salt(("audio-monitor-mode",source)).width(192.0).truncate().selected_text(crate::i18n::tr(ui,current.label())).show_ui(ui,|ui| {
        for mode in [AudioMonitoringMode::Off,AudioMonitoringMode::MonitorOnly,AudioMonitoringMode::MonitorAndOutput] {
            ui.add_enabled_ui(mode.is_off()||available,|ui|ui.selectable_value(&mut selected,mode,crate::i18n::tr(ui,mode.label()))
                .on_hover_text(crate::i18n::tr(ui,match mode {AudioMonitoringMode::Off=>"Send this source to Program only; physical monitoring stays off",AudioMonitoringMode::MonitorOnly=>"Listen on the monitoring device and remove this source from Program",AudioMonitoringMode::MonitorAndOutput=>"Listen on the monitoring device and also send this source to Program"}))
                .on_disabled_hover_text(crate::i18n::tr(ui,"Physical audio monitoring requires the Windows WASAPI output backend")));
        }
    }).response.on_hover_text(crate::i18n::tr(ui,"Choose whether this source feeds Program, the selected monitor device, or both"));
    if selected!=current {intents.push(Intent::LiveAudioMonitoring {session,revision,source,mode:selected});}
}
pub fn device(ui:&mut egui::Ui,model:&StudioSnapshot,session:u64,enabled:bool,intents:&mut Vec<Intent>){
    let view=MonitoringView::get(ui);let available=view.available&&view.session==session;
    ui.strong(crate::i18n::tr(ui,"Audio Monitoring Device"));
    if !available {ui.label(crate::i18n::tr(ui,"Physical audio monitoring requires the Windows WASAPI output backend"));return;}
    let mut selected=model.preferences().audio_monitoring_device().clone();
    let name=match &selected {
        AudioMonitoringDevice::Default=>view.devices.iter().find(|d|d.is_default).map_or_else(||crate::i18n::tr(ui,"Default"),|d|crate::i18n::tf(ui,"{label} ({device})",&[("{label}",crate::i18n::tr(ui,"Default")),("{device}",d.name.clone())])),
        AudioMonitoringDevice::WindowsEndpoint {id}=>view.devices.iter().find(|d|matches!(&d.target,skylive_core::AudioTarget::WindowsEndpoint {id:other} if other==id)).map_or_else(||crate::i18n::tf(ui,"{label} ({device})",&[("{label}",crate::i18n::tr(ui,"Unavailable endpoint")),("{device}",id.clone())]),|d|d.name.clone()),
    };
    ui.add_enabled_ui(enabled,|ui| {
        egui::ComboBox::from_id_salt("audio-monitor-output").width(ui.available_width().min(480.0)).truncate().selected_text(name.clone()).show_ui(ui,|ui| {
            ui.selectable_value(&mut selected,AudioMonitoringDevice::Default,crate::i18n::tr(ui,"Default")).on_hover_text(crate::i18n::tr(ui,"Use the default playback endpoint when monitoring starts"));
            for endpoint in &view.devices {if let skylive_core::AudioTarget::WindowsEndpoint {id}=&endpoint.target {
                ui.selectable_value(&mut selected,AudioMonitoringDevice::WindowsEndpoint {id:id.clone()},endpoint.name.clone()).on_hover_text(format!("{} · {}",endpoint.name,id));
            }}
        }).response.on_hover_text(name).on_disabled_hover_text(crate::i18n::tr(ui,"Apply or Cancel the audio format draft before changing the monitoring device"));
        if ui.add_enabled(!view.loading,egui::Button::new(crate::i18n::tr(ui,"Refresh playback devices")))
            .on_hover_text(crate::i18n::tr(ui,"Enumerate actual active Windows playback endpoints; this does not start monitoring"))
            .on_disabled_hover_text(crate::i18n::tr(ui,"Wait for device enumeration"))
            .clicked(){intents.push(Intent::EnumerateMonitorDevices {session,revision:model.revision()});}
    });
    if selected!=*model.preferences().audio_monitoring_device(){intents.push(Intent::SetMonitoringDevice {session,revision:model.revision(),device:selected});}
    if view.loading {ui.spinner();}
    if let Some(error)=&view.error {ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,error));}
    let status=&view.status;
    if status.stopping {ui.label(crate::i18n::tr(ui,"Stopping the physical monitoring output…"));}
    else if status.active {ui.label(crate::i18n::tf(ui,"Monitoring active: {device} · {frames} frames",&[("{device}",status.device_name.clone().unwrap_or_default()),("{frames}",status.frames_written.to_string())]));}
    else if status.requested&&status.problem.is_none() {ui.label(crate::i18n::tr(ui,"Opening the selected monitoring device…"));}
    else {ui.label(crate::i18n::tr(ui,"Monitor Off"));}
    if let Some(error)=&status.problem {ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,error));ui.small(crate::i18n::tr(ui,"Select Monitor Off and then a monitor mode to retry a failed output."));}
    ui.small(crate::i18n::tr(ui,"Selecting a device does not start playback. Enable a source monitoring mode in Advanced Audio. Capturing and monitoring the same playback endpoint can feed audio back; choose a separate output when needed."));
}
