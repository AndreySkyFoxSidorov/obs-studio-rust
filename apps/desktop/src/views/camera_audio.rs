//! Draft-only explicit camera audio input; enumeration belongs to the audio worker.
use eframe::egui;
use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget};
use skylive_host::audio_capture::AudioDeviceInfo;
use super::{Intent, camera_capture::CameraDraft};
pub struct CameraAudioDraft {
    pub enabled: bool,
    pub embedded: bool,
    pub selected: Option<AudioTarget>,
    pub use_device_timing: bool,
    pub devices: Vec<AudioDeviceInfo>,
    pub loading: bool,
    pub error: Option<String>,
}
impl CameraAudioDraft {
    pub fn new(binding: Option<&AudioCaptureBinding>, embedded:bool) -> Self {
        Self { enabled:binding.is_some(), embedded, selected:binding.map(|b|b.target().clone()),
            use_device_timing:binding.is_some_and(|b|b.use_device_timing()),
            devices:vec![],loading:false,error:None }
    }
    pub fn binding(&self) -> Result<Option<AudioCaptureBinding>, &'static str> {
        if !self.enabled {return Ok(None);}
        if self.loading {return Err("Wait for camera audio device enumeration before applying");}
        let target=self.selected.clone().ok_or("Select a custom camera audio input device")?;
        AudioCaptureBinding::new(AudioFlow::Input,target,self.use_device_timing,false)
            .map(Some).map_err(|_|"camera custom audio requires a Windows input endpoint")
    }
}
pub fn editor(ui:&mut egui::Ui,draft:&mut CameraDraft,intents:&mut Vec<Intent>) {
    let windows=matches!(draft.platform,crate::platform::CapturePlatform::Windows);
    if !windows && !draft.audio.enabled && !draft.audio.embedded {return;}
    egui::CollapsingHeader::new(crate::i18n::tr(ui,"Camera Audio"))
        .id_salt("camera-custom-audio").default_open(true).show(ui,|ui| {
        if ui.add_enabled(windows || draft.audio.embedded,egui::Checkbox::new(&mut draft.audio.embedded,crate::i18n::tr(ui,"Capture camera's embedded audio")))
            .on_hover_text(crate::i18n::tr(ui,"Use the camera's own DirectShow audio pin in the same capture graph as video."))
            .changed() && draft.audio.embedded {draft.audio.enabled=false;}
        let can_toggle=(windows||draft.audio.enabled) && !draft.audio.embedded;
        let enabled_changed=ui.add_enabled(can_toggle,egui::Checkbox::new(&mut draft.audio.enabled,crate::i18n::tr(ui,"Use custom audio device")))
            .on_hover_text(crate::i18n::tr(ui,"Capture a separately selected Windows input into this camera source's mixer channel. No device is guessed or opened before Apply."))
            .changed();
        if windows && enabled_changed && draft.audio.enabled && !draft.audio.loading {
            intents.push(Intent::RefreshCameraAudioDevices {token:draft.token});
        }
        if !windows {
            ui.label(crate::i18n::tr(ui,"Camera audio capture requires Windows. Disable it explicitly before choosing a camera on this platform."));
            return;
        }
        if draft.audio.embedded {
            ui.small(crate::i18n::tr(ui,"Embedded audio uses receive-time estimates and requires a camera with compatible DirectShow audio and video pins."));
        }
        if !draft.audio.embedded {
            ui.small(crate::i18n::tr(ui,"Camera audio is captured only while this source is in Program. Preview and video-only outputs do not start it."));
        }
        ui.add_enabled_ui(draft.audio.enabled,|ui| {
            let audio=&mut draft.audio;
            let label=ui.label(crate::i18n::tr(ui,"Audio input device"));
            let selected=match audio.selected.as_ref() {
                Some(AudioTarget::Default)=>audio.devices.iter().find(|d|d.is_default)
                    .map(|d|crate::i18n::tf(ui,"Default input: {name}",&[("{name}",d.name.clone())]))
                    .unwrap_or_else(||crate::i18n::tr(ui,"Default input device")),
                Some(target)=>audio.devices.iter().find(|d|&d.target==target).map(|d|d.name.clone())
                    .unwrap_or_else(||crate::i18n::tr(ui,"Saved input; refresh to resolve its name")),
                None=>crate::i18n::tr(ui,"Select audio input"),
            };
            ui.add_enabled_ui(!audio.loading,|ui| {
                egui::ComboBox::from_id_salt("camera-audio-input").selected_text(selected.clone())
                    .width(ui.available_width().min(450.0)).truncate().show_ui(ui,|ui| {
                    ui.selectable_value(&mut audio.selected,Some(AudioTarget::Default),crate::i18n::tr(ui,"Default input device"))
                        .on_hover_text(crate::i18n::tr(ui,"Follow the Windows default recording endpoint; this does not choose a camera-associated device."));
                    for device in audio.devices.iter().filter(|d|d.flow==AudioFlow::Input&&matches!(d.target,AudioTarget::WindowsEndpoint{..})) {
                        ui.selectable_value(&mut audio.selected,Some(device.target.clone()),&device.name).on_hover_text(&device.name);
                    }
                }).response.labelled_by(label.id).on_hover_text(selected);
            });
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!audio.loading,egui::Button::new(crate::i18n::tr(ui,"Refresh audio inputs")))
                    .on_hover_text(crate::i18n::tr(ui,"Enumerate actual Windows recording endpoint names without starting capture."))
                    .clicked() {intents.push(Intent::RefreshCameraAudioDevices {token:draft.token});}
                if audio.loading {ui.spinner();}
            });
            ui.checkbox(&mut audio.use_device_timing,crate::i18n::tr(ui,"Use device timestamps"))
                .on_hover_text(crate::i18n::tr(ui,"Use the selected input's native timestamps instead of receive-time estimates."));
            if let Some(error)=&audio.error {ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,error));}
        });
        ui.small(crate::i18n::tr(ui,"Activate/Deactivate controls both camera video and custom audio. This checkbox configures whether the source has a custom audio input."));
        ui.small(crate::i18n::tr(ui,"DirectSound and WaveOut playback are not supported."));
        ui.small(crate::i18n::tr(ui,"Disabling camera audio stops its route and preserves filters, monitoring, gain, balance and sync offset for re-enable."));
    });
}
