//! Camera input discovery shares the existing bounded WASAPI actor, never a GUI native call.
use super::*;
use skylive_host::audio_capture;
impl StudioApp {
    pub(super) fn refresh_camera_audio_devices(&mut self,token:u64,context:&egui::Context) {
        if let Some(Dialog::CameraCapture(draft))=&mut self.dialog {
            if draft.token!=token {return;}
            if draft.session!=self.session.identity() || draft.revision!=self.session.snapshot().revision() {
                draft.audio.loading=false;draft.audio.error=Some("The document changed. Reopen camera audio properties.".into());
            } else if matches!(draft.platform,crate::platform::CapturePlatform::Windows) {
                draft.audio.loading=true;draft.audio.error=None;
            }
        }
        context.request_repaint();
    }
    pub(super) fn accept_camera_audio_devices(&mut self,key:AudioRequestKey,flow:skylive_core::AudioFlow,result:Result<Vec<audio_capture::AudioDeviceInfo>,AudioCaptureError>) {
        let pending=self.pending_camera_audio_devices.take();
        let Some(Dialog::CameraCapture(draft))=&mut self.dialog else{return;};
        if pending!=Some((draft.token,key)) {return;}
        draft.audio.loading=false;
        if flow!=skylive_core::AudioFlow::Input || key.session!=draft.session || key.revision!=draft.revision
            || key.session!=self.session.identity() || key.revision!=self.session.snapshot().revision() {
            draft.audio.error=Some("The document changed. Reopen camera audio properties.".into());return;
        }
        match result {
            Ok(devices)=>{draft.audio.devices=devices.into_iter().filter(|device|device.flow==skylive_core::AudioFlow::Input&&matches!(device.target,skylive_core::AudioTarget::WindowsEndpoint{..}|skylive_core::AudioTarget::Default)).collect();draft.audio.error=None;}
            Err(error)=>draft.audio.error=Some(error.to_string()),
        }
    }
    pub(super) fn admit_camera_audio_devices(&mut self) {
        if self.audio.is_busy() || self.allow_close {return;}
        if let Some((token,_))=self.pending_camera_audio_devices.take() {
            if let Some(Dialog::CameraCapture(draft))=&mut self.dialog {
                if draft.token==token && draft.audio.loading {draft.audio.loading=false;draft.audio.error=Some("Camera audio enumeration became obsolete. Refresh audio inputs again.".into());}
            }
        }
        let request=match &self.dialog {Some(Dialog::CameraCapture(draft)) if draft.audio.loading=>Some((draft.token,draft.session,draft.revision)),_=>None};
        let Some((token,session,revision))=request else{return;};
        if session!=self.session.identity() || revision!=self.session.snapshot().revision() {
            if let Some(Dialog::CameraCapture(draft))=&mut self.dialog {draft.audio.loading=false;draft.audio.error=Some("The document changed. Reopen camera audio properties.".into());}return;
        }
        match self.audio.request_devices(&self.session,skylive_core::AudioFlow::Input) {
            Ok(key)=>self.pending_camera_audio_devices=Some((token,key)),
            Err(error)=>if let Some(Dialog::CameraCapture(draft))=&mut self.dialog {draft.audio.loading=false;draft.audio.error=Some(error.to_string());},
        }
    }
}
