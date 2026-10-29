//! Monitoring edits and endpoint discovery use the same canonical/actor APIs
//! as CLI. Enumeration never starts a playback device.
use super::*;
impl StudioApp {
    pub(super) fn install_monitoring_view(&self,context:&egui::Context){
        let same=self.monitor_devices_session==self.session.identity();
        views::audio_monitoring::MonitoringView {
            session:self.session.identity(),available:self.platform==crate::platform::CapturePlatform::Windows&&self.audio.monitoring_available(),
            loading:self.pending_monitor_devices.is_some_and(|key|key.session==self.session.identity()),
            devices:if same {self.monitor_devices.clone()}else{vec![]},error:if same {self.monitor_device_error.clone()}else{None},
            status:self.audio.program_status_for(&self.session).map(|s|s.monitor.clone()).unwrap_or_default(),
        }.install(context);
    }
    pub(super) fn enumerate_monitor_devices(&mut self,session:u64,revision:u64){
        if session!=self.session.identity()||revision!=self.session.snapshot().revision(){self.message(true,"Audio monitoring device request is stale");return;}
        if self.platform!=crate::platform::CapturePlatform::Windows||!self.audio.monitoring_available(){self.message(true,"Windows audio monitoring backend is unavailable");return;}
        self.monitor_devices_session=session;
        match self.audio.request_devices(&self.session,skylive_core::AudioFlow::Output){
            Ok(key)=>{self.pending_monitor_devices=Some(key);self.monitor_device_error=None;},
            Err(error)=>{self.monitor_device_error=Some(error.to_string());self.message(true,error.to_string());},
        }
    }
    pub(super) fn set_monitoring_device(&mut self,session:u64,revision:u64,device:skylive_core::AudioMonitoringDevice,context:&egui::Context){
        if session!=self.session.identity()||revision!=self.session.snapshot().revision(){self.message(true,"Audio monitoring settings changed; choose the device again");return;}
        if self.platform!=crate::platform::CapturePlatform::Windows||!self.audio.monitoring_available(){self.message(true,"Windows audio monitoring backend is unavailable");return;}
        let audio_settings=matches!(&self.dialog,Some(Dialog::AudioSettings {..}));
        if audio_settings&&!self.dialog.as_ref().is_some_and(|d|views::settings::unchanged(d,&self.session.snapshot())) {self.message(true,"Apply or Cancel the audio format draft before changing the monitoring device");return;}
        let mut dialog=self.dialog.take();
        let published=self.publish_command_at(Some(revision),Command::SetAudioMonitoringDevice {device},false);
        if published&&audio_settings {
            // Retag this clean page after its own live preference edit. External
            // changes still use the existing stale Settings policy.
            if let Some(current)=&dialog {if let Err(error)=self.settings_flow.opened(&self.session,current){self.message(true,error);}}
        }
        if let Some(Dialog::AdvancedAudio(d))=&mut dialog {d.error=self.error.then(||self.status.clone());}
        self.dialog=dialog;context.request_repaint();
    }
    pub(super) fn accept_monitor_devices(&mut self,key:AudioRequestKey,flow:skylive_core::AudioFlow,result:Result<Vec<skylive_host::audio_capture::AudioDeviceInfo>,AudioCaptureError>){
        self.pending_monitor_devices=None;
        if key.session!=self.session.identity()||key.revision!=self.session.snapshot().revision()||flow!=skylive_core::AudioFlow::Output {self.monitor_device_error=Some("Audio device list became stale; refresh playback devices".into());return;}
        self.monitor_devices_session=key.session;
        match result {
            Ok(devices)=>{self.monitor_devices=devices.into_iter().filter(|d|d.flow==skylive_core::AudioFlow::Output&&matches!(d.target,skylive_core::AudioTarget::WindowsEndpoint {..})).collect();self.monitor_device_error=None;},
            Err(error)=>self.monitor_device_error=Some(error.to_string()),
        }
    }
}
