//! Draft-only explicit camera audio input; enumeration belongs to the audio worker.
use eframe::egui;
use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget};
use skylive_host::audio_capture::AudioDeviceInfo;
use super::{Intent, camera_capture::CameraDraft};
#[cfg(target_os="windows")]
use std::sync::{atomic::{AtomicBool, Ordering}, mpsc};
#[cfg(target_os="windows")]
static MATCH_BUSY: AtomicBool = AtomicBool::new(false);
#[cfg(target_os="windows")]
struct MatchPermit;
#[cfg(target_os="windows")]
impl Drop for MatchPermit { fn drop(&mut self) { MATCH_BUSY.store(false, Ordering::Release); } }
#[cfg(target_os="windows")]
struct PendingMatch {
    camera: skylive_core::VideoCaptureTarget,
    original: Option<AudioTarget>,
    result: mpsc::Receiver<skylive_host::camera_capture::audio_association::Association>,
}
pub struct CameraAudioDraft {
    pub enabled: bool,
    pub embedded: bool,
    pub selected: Option<AudioTarget>,
    pub use_device_timing: bool,
    pub devices: Vec<AudioDeviceInfo>,
    pub loading: bool,
    pub error: Option<String>,
    #[cfg(target_os="windows")]
    pending_match: Option<PendingMatch>,
    #[cfg(target_os="windows")]
    auto_attempted: Option<skylive_core::VideoCaptureTarget>,
    // This is draft-only provenance: a saved or manually chosen endpoint is
    // always explicit, but an inferred match belongs to the camera it matched.
    #[cfg(target_os="windows")]
    matched_camera: Option<(skylive_core::VideoCaptureTarget, AudioTarget)>,
    // An explicit Find request may have to enumerate endpoints first. Keep
    // its identity so a late list cannot change a different camera/choice.
    #[cfg(target_os="windows")]
    match_after_refresh: Option<(skylive_core::VideoCaptureTarget, Option<AudioTarget>)>,
    #[cfg(target_os="windows")]
    next_auto_refresh: std::time::Instant,
}
impl CameraAudioDraft {
    pub(crate) fn devices_refreshed(&mut self) {
        #[cfg(target_os="windows")]
        { self.auto_attempted=None; self.pending_match=None; }
    }
    pub(crate) fn device_refresh_failed(&mut self) {
        #[cfg(target_os="windows")]
        { self.match_after_refresh=None; }
    }
    pub fn new(binding: Option<&AudioCaptureBinding>, embedded:bool) -> Self {
        Self { enabled:binding.is_some(), embedded, selected:binding.map(|b|b.target().clone()),
            use_device_timing:binding.is_some_and(|b|b.use_device_timing()),
            devices:vec![],loading:false,error:None,
            #[cfg(target_os="windows")]
            pending_match:None,
            #[cfg(target_os="windows")]
            auto_attempted:None,
            #[cfg(target_os="windows")]
            matched_camera:None,
            #[cfg(target_os="windows")]
            match_after_refresh:None,
            #[cfg(target_os="windows")]
            next_auto_refresh:std::time::Instant::now()+std::time::Duration::from_secs(3) }
    }
    pub fn binding(&self) -> Result<Option<AudioCaptureBinding>, &'static str> {
        if !self.enabled {return Ok(None);}
        if self.loading {return Err("Wait for camera audio device enumeration before applying");}
        let target=self.selected.clone().ok_or("Select a custom camera audio input device")?;
        AudioCaptureBinding::new(AudioFlow::Input,target,self.use_device_timing,false)
            .map(Some).map_err(|_|"camera custom audio requires a Windows input endpoint")
    }
}
#[cfg(target_os="windows")]
fn begin_match(audio:&mut CameraAudioDraft,camera:skylive_core::VideoCaptureTarget,ctx:&egui::Context) {
    if MATCH_BUSY.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).is_err() {
        audio.error=Some("Camera audio matching is already running.".into());
        return;
    }
    let candidates=audio.devices.iter().filter(|device|device.flow==AudioFlow::Input).map(|device|device.target.clone()).collect::<Vec<_>>();
    let (sender,result)=mpsc::channel();
    let worker_camera=camera.clone();
    let started=std::thread::Builder::new().name("camera-audio-match".into()).spawn(move || {
        let _permit=MatchPermit;
        let value=skylive_host::camera_capture::audio_association::windows::find_camera_input(&worker_camera,&candidates);
        let _=sender.send(value);
    });
    if started.is_ok() {
        audio.pending_match=Some(PendingMatch {camera,original:audio.selected.clone(),result});
        audio.error=None;
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    } else {
        MATCH_BUSY.store(false,Ordering::Release);
        audio.error=Some("Could not start camera audio matching.".into());
    }
}
pub fn editor(ui:&mut egui::Ui,draft:&mut CameraDraft,intents:&mut Vec<Intent>) {
    // An automatic route follows the camera selected in this draft. Store its
    // stable camera identity, never the currently discovered audio endpoint.
    if matches!(draft.audio.selected.as_ref(), Some(AudioTarget::WindowsCamera { .. })) {
        if let Some(skylive_core::VideoCaptureTarget::Windows { symbolic_link }) = &draft.selected {
            let desired = AudioTarget::WindowsCamera { symbolic_link: symbolic_link.clone() };
            if draft.audio.selected.as_ref() != Some(&desired) { draft.audio.selected = Some(desired); }
        }
    }
    // A match belongs to one physical camera. If the draft switches cameras,
    // discard only the inferred selection; never erase a manual or saved one.
    #[cfg(target_os="windows")]
    if let Some((camera, target)) = draft.audio.matched_camera.as_ref() {
        if draft.audio.selected.as_ref() != Some(target) {
            draft.audio.matched_camera = None;
        } else if draft.selected.as_ref() != Some(camera) {
            draft.audio.selected = None;
            draft.audio.matched_camera = None;
            draft.audio.auto_attempted = None;
        }
    }
    // While the properties window is open, retry discovery for an unresolved
    // custom input. Never replace an existing explicit or saved selection.
    #[cfg(target_os="windows")]
    if draft.audio.enabled && !draft.audio.embedded && draft.audio.selected.is_none()
        && !draft.audio.loading && draft.audio.pending_match.is_none() {
        let now=std::time::Instant::now();
        if now >= draft.audio.next_auto_refresh {
            draft.audio.next_auto_refresh=now+std::time::Duration::from_secs(3);
            intents.push(Intent::RefreshCameraAudioDevices {token:draft.token});
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(3));
    }
    #[cfg(target_os="windows")]
    if let Some(pending) = draft.audio.pending_match.take() {
        match pending.result.try_recv() {
            Ok(result) if draft.selected.as_ref()==Some(&pending.camera) && draft.audio.selected==pending.original && draft.audio.enabled && !draft.audio.embedded => {
                use skylive_host::camera_capture::audio_association::Association;
                match result {
                    Association::Unique(target) => { draft.audio.matched_camera=Some((pending.camera,target.clone())); draft.audio.selected=Some(target); draft.audio.error=None; },
                    Association::UnknownCamera => draft.audio.error=Some("Unable to determine the camera device container.".into()),
                    Association::NoMatch => draft.audio.error=Some("No associated active audio input was found.".into()),
                    Association::Ambiguous => draft.audio.error=Some("Several audio inputs match this camera; select one manually.".into()),
                }
            },
            Ok(_) => {},
            Err(mpsc::TryRecvError::Empty) => { draft.audio.pending_match=Some(pending); ui.ctx().request_repaint_after(std::time::Duration::from_millis(250)); },
            Err(mpsc::TryRecvError::Disconnected) => draft.audio.error=Some("Camera audio matching could not complete.".into()),
        }
    }
    #[cfg(target_os="windows")]
    if !draft.audio.loading {
        if let Some((camera, original))=draft.audio.match_after_refresh.take() {
            if draft.selected.as_ref()==Some(&camera) && draft.audio.selected==original
                && draft.audio.enabled && !draft.audio.embedded {
                draft.audio.auto_attempted=Some(camera.clone());
                begin_match(&mut draft.audio,camera,ui.ctx());
            }
        }
    }
    #[cfg(target_os="windows")]
    if draft.audio.enabled && !draft.audio.embedded && !draft.audio.loading && draft.audio.selected.is_none()
        && !draft.audio.devices.is_empty() && draft.audio.pending_match.is_none() {
        if let Some(camera)=draft.selected.clone().filter(|camera|draft.audio.auto_attempted.as_ref()!=Some(camera)) {
            draft.audio.auto_attempted=Some(camera.clone());
            begin_match(&mut draft.audio,camera,ui.ctx());
        }
    }
    let windows=matches!(draft.platform,crate::platform::CapturePlatform::Windows);
    if !windows && !draft.audio.enabled && !draft.audio.embedded {return;}
    egui::CollapsingHeader::new(crate::i18n::tr(ui,"Camera Audio"))
        .id_salt("camera-custom-audio").default_open(true).show(ui,|ui| {
        let embedded_changed=ui.add_enabled(windows || draft.audio.embedded,egui::Checkbox::new(&mut draft.audio.embedded,crate::i18n::tr(ui,"Capture camera's embedded audio")))
            .on_hover_text(crate::i18n::tr(ui,"Use the camera's own DirectShow audio pin in the same capture graph as video."))
            .changed();
        if embedded_changed {
            #[cfg(target_os="windows")]
            {draft.audio.pending_match=None;draft.audio.auto_attempted=None;draft.audio.match_after_refresh=None;}
            if draft.audio.embedded {draft.audio.enabled=false;}
        }
        let can_toggle=(windows||draft.audio.enabled) && !draft.audio.embedded;
        let enabled_changed=ui.add_enabled(can_toggle,egui::Checkbox::new(&mut draft.audio.enabled,crate::i18n::tr(ui,"Use custom audio device")))
            .on_hover_text(crate::i18n::tr(ui,"Capture a separately selected Windows input into this camera source's mixer channel. No device is guessed or opened before Apply."))
            .changed();
        #[cfg(target_os="windows")]
        if enabled_changed {draft.audio.pending_match=None;draft.audio.auto_attempted=None;draft.audio.match_after_refresh=None;}
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
                Some(AudioTarget::WindowsCamera { .. })=>crate::i18n::tr_key_or(ui,"SkyLive.CameraAudio.Automatic","Automatic camera audio"),
                Some(target)=>audio.devices.iter().find(|d|&d.target==target).map(|d|d.name.clone())
                    .unwrap_or_else(||crate::i18n::tr(ui,"Saved input; refresh to resolve its name")),
                None=>crate::i18n::tr(ui,"Select audio input"),
            };
            ui.add_enabled_ui(!audio.loading,|ui| {
                egui::ComboBox::from_id_salt("camera-audio-input").selected_text(selected.clone())
                    .width(ui.available_width().min(450.0)).truncate().show_ui(ui,|ui| {
                    if let Some(skylive_core::VideoCaptureTarget::Windows { symbolic_link }) = &draft.selected {
                        let automatic=AudioTarget::WindowsCamera { symbolic_link: symbolic_link.clone() };
                        let response=ui.selectable_value(&mut audio.selected,Some(automatic),crate::i18n::tr_key_or(ui,"SkyLive.CameraAudio.Automatic","Automatic camera audio"))
                            .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.CameraAudio.AutomaticHelp","Follow the uniquely associated camera input as it connects or changes. Ambiguous inputs require manual selection."));
                        #[cfg(target_os="windows")]
                        if response.clicked() {audio.matched_camera=None;audio.pending_match=None;audio.match_after_refresh=None;}
                    }
                    let default_choice=ui.selectable_value(&mut audio.selected,Some(AudioTarget::Default),crate::i18n::tr(ui,"Default input device"))
                        .on_hover_text(crate::i18n::tr(ui,"Follow the Windows default recording endpoint; this does not choose a camera-associated device."));
                    #[cfg(target_os="windows")]
                    if default_choice.clicked() { audio.matched_camera=None; audio.pending_match=None; audio.match_after_refresh=None; }
                    for device in audio.devices.iter().filter(|d|d.flow==AudioFlow::Input&&matches!(d.target,AudioTarget::WindowsEndpoint{..})) {
                        let choice=ui.selectable_value(&mut audio.selected,Some(device.target.clone()),&device.name).on_hover_text(&device.name);
                        #[cfg(target_os="windows")]
                        if choice.clicked() { audio.matched_camera=None; audio.pending_match=None; audio.match_after_refresh=None; }
                    }
                }).response.labelled_by(label.id).on_hover_text(selected);
            });
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!audio.loading,egui::Button::new(crate::i18n::tr(ui,"Refresh audio inputs")))
                    .on_hover_text(crate::i18n::tr(ui,"Enumerate actual Windows recording endpoint names without starting capture."))
                    .clicked() {intents.push(Intent::RefreshCameraAudioDevices {token:draft.token});}
                if audio.loading {ui.spinner();}
                #[cfg(target_os="windows")]
                if ui.add_enabled(!audio.loading && audio.pending_match.is_none() && draft.selected.is_some(), egui::Button::new(crate::i18n::tr_key_or(ui,"SkyLive.CameraAudio.Match","Find camera audio input")))
                    .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.CameraAudio.MatchHelp","Match active inputs by Windows device container. Ambiguous matches require manual selection."))
                    .clicked() {
                    if let Some(camera)=draft.selected.clone() {
                        // A nonempty list can still predate a device hotplug.
                        // Resolve against a fresh enumeration for every explicit
                        // Find, retaining the initiating camera and choice so
                        // late results cannot replace a newer manual decision.
                        audio.match_after_refresh=Some((camera,audio.selected.clone()));
                        intents.push(Intent::RefreshCameraAudioDevices {token:draft.token});
                    }
                }
            });
            ui.checkbox(&mut audio.use_device_timing,crate::i18n::tr(ui,"Use device timestamps"))
                .on_hover_text(crate::i18n::tr(ui,"Use the selected input's native timestamps instead of receive-time estimates."));
            if let Some(error)=&audio.error {
                let key=match error.as_str() {
                    "Unable to determine the camera device container."=>Some("SkyLive.CameraAudio.ContainerUnknown"),
                    "No associated active audio input was found."=>Some("SkyLive.CameraAudio.NoMatch"),
                    "Several audio inputs match this camera; select one manually."=>Some("SkyLive.CameraAudio.Ambiguous"),
                    "Camera audio matching could not complete."=>Some("SkyLive.CameraAudio.Failed"),
                    "Camera audio matching is already running."=>Some("SkyLive.CameraAudio.Busy"),
                    "Could not start camera audio matching."=>Some("SkyLive.CameraAudio.StartFailed"),
                    _=>None,
                };
                let text=key.map_or_else(||crate::i18n::tr(ui,error),|key|crate::i18n::tr_key_or(ui,key,error));
                ui.colored_label(ui.visuals().error_fg_color,text);
            }
        });
        ui.small(crate::i18n::tr(ui,"Activate/Deactivate controls both camera video and custom audio. This checkbox configures whether the source has a custom audio input."));
        ui.small(crate::i18n::tr(ui,"DirectSound and WaveOut playback are not supported."));
        ui.small(crate::i18n::tr(ui,"Disabling camera audio stops its route and preserves filters, monitoring, gain, balance and sync offset for re-enable."));
    });
}
