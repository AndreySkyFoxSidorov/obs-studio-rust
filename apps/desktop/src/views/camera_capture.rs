//! Device discovery and explicit native-mode query are separate staged actions.
use super::{CameraEdit, Intent};
use eframe::egui;
use skylive_core::{CameraFpsPolicy, CameraModePolicy, VideoCaptureBinding, VideoCaptureMode, VideoCaptureTarget};
use skylive_host::camera_capture::{
    CameraCapabilities, CameraDeviceInfo, CameraError, CameraModeInfo, CameraRequestKey,
};
#[path = "camera_mode_selectors.rs"]
mod mode_selectors;
pub struct CameraDraft {
    pub platform: crate::platform::CapturePlatform,
    pub token: u64,
    pub session: u64,
    pub revision: u64,
    pub edit: CameraEdit,
    pub name: String,
    pub devices: Vec<CameraDeviceInfo>,
    pub selected: Option<VideoCaptureTarget>,
    pub modes: Vec<CameraModeInfo>,
    pub selected_mode: Option<VideoCaptureMode>,
    pub(super) fps_policy: CameraFpsPolicy,
    pub(super) mode_policy: CameraModePolicy,
    pub deactivate_when_hidden: bool,
    pub flip_vertically: bool,
    pub capabilities: CameraCapabilities,
    pub audio: super::camera_audio::CameraAudioDraft,
    pub loading_devices: bool,
    pub loading_modes: bool,
    pub query_modes: bool,
    pub loading_preferred: bool,
    pub query_preferred: bool,
    pub error: Option<String>,
    pub(super) original: Option<VideoCaptureBinding>,
    pub driver: super::camera_controls::DriverDraft,
    pub source_enabled:bool,
    pub av_enabled:bool,
    pub av_status:Option<skylive_host::camera_activation::Status>,
    pub observed_mode: Option<(VideoCaptureBinding, VideoCaptureMode, bool)>,
    queried_target: Option<VideoCaptureTarget>,
}
impl CameraDraft {
    pub fn on_platform(mut self, platform: crate::platform::CapturePlatform) -> Self {
        self.platform = platform;
        // Resolve the name of an existing custom audio binding in the
        // background; keep its exact saved target even if enumeration fails.
        if matches!(platform,crate::platform::CapturePlatform::Windows) && self.audio.enabled {
            self.audio.loading = true;
        }
        self
    }
    pub fn new(
        token: u64,
        session: u64,
        revision: u64,
        edit: CameraEdit,
        name: String,
        binding: Option<VideoCaptureBinding>,
        capabilities: CameraCapabilities,
    ) -> Self {
        Self {
            platform: crate::platform::CapturePlatform::Unavailable,
            token,
            session,
            revision,
            edit,
            name,
            devices: vec![],
            selected: binding.as_ref().map(|b| b.target().clone()),
            modes: vec![],
            selected_mode: binding.as_ref().map(|b| b.mode()),
            fps_policy: binding.as_ref().map_or(CameraFpsPolicy::Fixed, |b| b.fps_policy()),
            mode_policy: binding.as_ref().map_or(CameraModePolicy::Fixed, |b| b.mode_policy()),
            deactivate_when_hidden: binding.as_ref().is_some_and(|b| b.deactivate_when_hidden()),
            flip_vertically: binding.as_ref().is_some_and(|b| b.flip_vertically()),
            audio: super::camera_audio::CameraAudioDraft::new(binding.as_ref().and_then(|b|b.audio()),binding.as_ref().is_some_and(|b|b.embedded_audio())),
            capabilities,
            loading_devices: true,
            loading_modes: false,
            query_modes: false,
            loading_preferred: false,
            query_preferred: false,
            error: None,
            original: binding,
            driver: Default::default(),
            source_enabled:false,
            av_enabled:true,av_status:None,
            observed_mode: None,
            queried_target: None,
        }
    }
    pub fn loading(&self) -> bool {
        self.loading_devices || self.loading_modes || self.query_modes || self.loading_preferred || self.query_preferred
    }
    fn accepts(
        &mut self,
        pending: Option<(u64, CameraRequestKey)>,
        key: CameraRequestKey,
        session: u64,
        revision: u64,
    ) -> bool {
        if pending != Some((self.token, key)) {
            return false;
        }
        if key.session != self.session
            || key.revision != self.revision
            || key.session != session
            || key.revision != revision
        {
            self.loading_devices = false;
            self.loading_modes = false;
            self.query_modes = false;
            self.loading_preferred = false;
            self.query_preferred = false;
            self.error = Some("The document changed. Reopen this dialog to bind a camera.".into());
            return false;
        }
        true
    }
    pub fn devices(
        &mut self,
        pending: Option<(u64, CameraRequestKey)>,
        key: CameraRequestKey,
        session: u64,
        revision: u64,
        result: Result<Vec<CameraDeviceInfo>, CameraError>,
    ) {
        if !self.accepts(pending, key, session, revision) {
            return;
        }
        self.loading_devices = false;
        match result {
            Ok(found) => {
                let found: Vec<_> = found
                    .into_iter()
                    .filter(|device| self.platform.camera(&device.target))
                    .collect();
                if self.selected.is_none() {
                    self.selected = found.first().map(|d| d.target.clone());
                }
                self.devices = found;
                self.error = None;
                // New sources need an actual mode before they can be applied.
                // Start the existing worker-owned query for the initially
                // selected device so the recommended Full HD mode is offered
                // without an extra, easily missed button press. Existing saved
                // bindings remain unchanged until the user asks to query them.
                if self.original.is_none() && self.selected.is_some() && self.queried_target != self.selected {
                    self.query_modes = true;
                }
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    pub fn modes(
        &mut self,
        pending: Option<(u64, CameraRequestKey)>,
        key: CameraRequestKey,
        session: u64,
        revision: u64,
        target: VideoCaptureTarget,
        result: Result<Vec<CameraModeInfo>, CameraError>,
    ) {
        if !self.accepts(pending, key, session, revision) || self.selected.as_ref() != Some(&target)
        {
            return;
        }
        self.loading_modes = false;
        self.query_modes = false;
        match result {
            Ok(mut found) => {
                if let Err(error) = skylive_host::camera_capture::arrange_camera_modes(&mut found) {
                    self.error = Some(error.to_string());
                    return;
                }
                if self.selected_mode.is_none() {
                    match skylive_host::camera_capture::CameraModeRecommendations::from_modes(
                        &found,
                    ) {
                        Ok(recommendations) => self.selected_mode = recommendations.recommended,
                        Err(error) => {
                            self.error = Some(error.to_string());
                            return;
                        }
                    }
                }
                self.queried_target = Some(target);
                self.modes = found;
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    pub fn intent(&self) -> Result<Intent, String> {
        if self.loading() {
            return Err("Wait for camera enumeration before applying".into());
        }
        if !self.capabilities.available {
            return Err("Camera capture is unavailable on this platform".into());
        }
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let target = self.selected.as_ref().ok_or("Select a camera")?;
        if !self.platform.camera(target) {
            return Err(
                "This saved camera belongs to another platform. Choose an available camera.".into(),
            );
        }
        if !self.devices.iter().any(|d| &d.target == target) {
            return Err("The selected camera is unavailable".into());
        }
        let mode = self
            .selected_mode
            .ok_or("Query and select a supported native camera mode")?;
        let verified = self.queried_target.as_ref() == Some(target)
            && self.modes.iter().any(|m| m.mode == Some(mode));
        let saved = self
            .original
            .as_ref()
            .is_some_and(|b| b.target() == target && b.mode() == mode);
        if !verified && (!saved || self.queried_target.as_ref() == Some(target)) {
            return Err("Select an actual supported camera mode".into());
        }
        let binding = VideoCaptureBinding::new(target.clone(), mode, self.deactivate_when_hidden)
            .map(|binding| binding.with_mode_policy(self.mode_policy).with_fps_policy(self.fps_policy).with_flip_vertically(self.flip_vertically))
            .and_then(|binding|self.audio.binding().map_err(skylive_core::Error::Invalid).and_then(|audio|binding.with_audio(audio)))
            .and_then(|binding|binding.with_embedded_audio(self.audio.embedded))
            .map_err(|e| e.to_string())?;
        Ok(Intent::CommitCamera {
            token:self.token,
            session: self.session,
            revision: self.revision,
            edit: self.edit.clone(),
            name: self.name.clone(),
            binding,
        })
    }

    /// Accept a DirectShow Preferred result only for the same dialog, document,
    /// target and previously enumerated MF capabilities. The native request is
    /// owned by CameraOwner and may finish after the dialog has been closed.
    pub fn preferred(
        &mut self, pending: Option<(u64, CameraRequestKey)>, key: CameraRequestKey,
        session: u64, revision: u64, target: VideoCaptureTarget,
        result: Result<CameraModeInfo, CameraError>,
    ) {
        if !self.accepts(pending, key, session, revision) || self.selected.as_ref() != Some(&target) {
            return;
        }
        self.loading_preferred = false;
        self.query_preferred = false;
        let preferred = match result {
            Ok(info) => info,
            Err(CameraError::DeviceBusy) => {
                self.error = Some("Stop capture of this camera before querying Device Default.".into());
                return;
            }
            Err(error) => { self.error = Some(error.to_string()); return; }
        };
        let actual = skylive_host::obs::CameraImportDevice::from_owner_modes(target.clone(), &self.modes)
            .and_then(|entry| entry.with_preferred(&preferred))
            .ok().and_then(|entry| entry.preferred);
        match actual {
            Some(mode) if self.queried_target.as_ref() == Some(&target) => {
                self.selected_mode = Some(mode);
                self.fps_policy = CameraFpsPolicy::Fixed;
                self.mode_policy = CameraModePolicy::DirectShowPreferred;
                self.error = None;
            }
            _ => self.error = Some("The camera's DirectShow default has no matching supported capture mode.".into()),
        }
    }
}
fn mode_label(ui: &egui::Ui, mode: VideoCaptureMode) -> String {
    let base=crate::i18n::tf(ui,"{width}×{height} · {num}/{den} fps · {format}",
        &[("{width}",mode.width().to_string()),("{height}",mode.height().to_string()),
          ("{num}",mode.fps_num().to_string()),("{den}",mode.fps_den().to_string()),("{format}",mode.format().label().into())]);
    match mode.format() {
        skylive_core::VideoPixelFormat::Yuy2 {
            matrix,
            range,
            chroma_left,
        }
        | skylive_core::VideoPixelFormat::Uyvy { matrix, range, chroma_left }
        | skylive_core::VideoPixelFormat::I420 { matrix, range, chroma_left }
        | skylive_core::VideoPixelFormat::Yv12 { matrix, range, chroma_left }
        | skylive_core::VideoPixelFormat::Nv12 {
            matrix,
            range,
            chroma_left,
        } => {
            let matrix = match matrix {
                skylive_core::VideoColorMatrix::Bt601 => "BT.601",
                skylive_core::VideoColorMatrix::Bt709 => "BT.709",
            };
            let range = crate::i18n::tr(
                ui,
                match range {
                    skylive_core::VideoColorRange::Limited => "Limited range",
                    skylive_core::VideoColorRange::Full => "Full range",
                },
            );
            let chroma = crate::i18n::tr(
                ui,
                if chroma_left {
                    "Chroma left"
                } else {
                    "Chroma centered"
                },
            );
            format!("{base} · {matrix} · {range} · {chroma}")
        }
        _ => base,
    }
}

pub fn editor(ui: &mut egui::Ui, draft: &mut CameraDraft, intents: &mut Vec<Intent>, project_fps: (u32,u32)) {
    if matches!(draft.edit, CameraEdit::Add { .. }) {
        let label = ui.label(crate::i18n::tr(ui, "Name"));
        ui.add(
            egui::TextEdit::singleline(&mut draft.name)
                .char_limit(256)
                .id_salt("camera-source-name"),
        )
        .labelled_by(label.id)
        .on_hover_text(crate::i18n::tr(ui, "Name of this video capture source"));
    }
    ui.label(crate::i18n::tf(
        ui,
        "Backend: {} · SDR",
        &[("{}", draft.capabilities.backend.to_string())],
    ));
    if draft
        .selected
        .as_ref()
        .is_some_and(|target| !draft.platform.camera(target))
    {
        ui.colored_label(ui.visuals().warn_fg_color,crate::i18n::tr(ui,"This saved capture device belongs to another platform. Choose an available device to repair the binding."));
    }
    if let Some(note) = draft.capabilities.note {
        ui.label(crate::i18n::tr(ui, note));
    }
    if draft.loading() {
        ui.horizontal(|ui| {
            ui.spinner();
            let text=if draft.loading_preferred || draft.query_preferred {
                crate::i18n::tr_key_or(ui,"SkyLive.Camera.PreferredLoading","Finding the camera's device default…")
            } else { crate::i18n::tr(ui,"Querying camera capabilities…") };
            ui.label(text);
        });
    }
    if let Some(error) = &draft.error {
        let message=match error.as_str() {
            "Stop capture of this camera before querying Device Default."=>crate::i18n::tr_key_or(ui,"SkyLive.Camera.PreferredBusy",error),
            "The camera's DirectShow default has no matching supported capture mode."=>crate::i18n::tr_key_or(ui,"SkyLive.Camera.PreferredNoMatch",error),
            _=>crate::i18n::tr(ui,error),
        };
        ui.colored_label(ui.visuals().error_fg_color, message);
    }
    let old = draft.selected.clone();
    let selected = draft
        .devices
        .iter()
        .find(|d| draft.platform.camera(&d.target) && Some(&d.target) == draft.selected.as_ref())
        .map_or_else(|| crate::i18n::tr(ui, "Select camera"), |d| d.name.clone());
    let loading = draft.loading();
    ui.add_enabled_ui(!loading, |ui| {
        egui::ComboBox::from_id_salt("camera-device")
            .selected_text(selected.clone())
            .width(ui.available_width().min(450.0))
            .truncate()
            .show_ui(ui, |ui| {
                ui.set_width(ui.available_width().min(450.0));
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                for device in draft
                    .devices
                    .iter()
                    .filter(|device| draft.platform.camera(&device.target))
                {
                    ui.selectable_value(
                        &mut draft.selected,
                        Some(device.target.clone()),
                        &device.name,
                    )
                    .on_hover_ui(|ui| {
                        ui.label(&device.name);
                        ui.label(crate::i18n::tr(ui, "Choose the video capture device"));
                    });
                }
            })
            .response
            .on_hover_ui(|ui| {
                ui.label(selected);
                ui.label(crate::i18n::tr(ui, "Device"));
            });
    });
    if old != draft.selected {
        // A different camera needs its own DirectShow/MF match before Preferred
        // may be saved; a previous device's result cannot authorize it.
        draft.mode_policy=CameraModePolicy::Fixed;
        draft.modes.clear();
        draft.selected_mode = None;
        draft.queried_target = None;
        draft.error = None;
        // Selecting another device is an explicit request to configure it;
        // query that exact target through the camera worker before Apply.
        draft.query_modes = draft.selected.is_some();
    }
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !loading,
                egui::Button::new(crate::i18n::tr(ui, "Refresh Cameras")),
            )
            .on_hover_text(crate::i18n::tr(
                ui,
                "Refresh device names without starting video capture",
            ))
            .clicked()
        {
            intents.push(Intent::RefreshCameras);
        }
        if ui
            .add_enabled(
                !loading
                    && draft
                        .selected
                        .as_ref()
                        .is_some_and(|target| draft.platform.camera(target)),
                egui::Button::new(crate::i18n::tr(ui, "Query Modes")),
            )
            .on_hover_text(crate::i18n::tr(
                ui,
                "Querying modes may briefly activate the selected camera driver",
            ))
            .clicked()
        {
            intents.push(Intent::QueryCameraModes);
        }
    });
    let mode_field_label=ui.label(crate::i18n::tr_key_or(ui,"Module.win-dshow.ResFPSType","Resolution/FPS Type"));
    ui.add_enabled_ui(!loading, |ui| { egui::ComboBox::from_id_salt("camera-mode-policy")
        .selected_text(match draft.mode_policy {
            CameraModePolicy::Fixed=>crate::i18n::tr_key_or(ui,"Module.win-dshow.ResFPSType.Custom","Custom"),
            CameraModePolicy::DirectShowPreferred=>crate::i18n::tr_key_or(ui,"Module.win-dshow.ResFPSType.DevPreferred","Device Default"),
        })
        .show_ui(ui,|ui|{
            if ui.selectable_label(draft.mode_policy==CameraModePolicy::Fixed,crate::i18n::tr_key_or(ui,"Module.win-dshow.ResFPSType.Custom","Custom")).clicked() {
                draft.mode_policy=CameraModePolicy::Fixed;
                draft.error=None;
            }
            let can_probe=!loading && matches!(draft.selected.as_ref(),Some(VideoCaptureTarget::Windows{..}))
                && draft.queried_target==draft.selected && !draft.modes.is_empty();
            ui.add_enabled_ui(can_probe || draft.mode_policy==CameraModePolicy::DirectShowPreferred,|ui|{
                if ui.selectable_label(draft.mode_policy==CameraModePolicy::DirectShowPreferred,crate::i18n::tr_key_or(ui,"Module.win-dshow.ResFPSType.DevPreferred","Device Default"))
                    .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Camera.PreferredHint","Query the camera's DirectShow default and match it to a supported capture mode. Stop this camera first if it is active."))
                    .on_disabled_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Camera.PreferredQueryFirst","Query camera modes before selecting Device Default."))
                    .clicked() && can_probe && draft.mode_policy!=CameraModePolicy::DirectShowPreferred {
                        draft.query_preferred=true;
                        draft.error=None;
                    }
            });
        }).response.labelled_by(mode_field_label.id); });
    ui.add_enabled_ui(draft.mode_policy==CameraModePolicy::Fixed,|ui|mode_selectors::show(ui, draft, project_fps));
    if let Some((binding, mode, carried)) = &draft.observed_mode {
        // Do not present a live stream as the result of unapplied draft edits.
        if draft.selected.as_ref()==Some(binding.target())
            && draft.selected_mode==Some(binding.mode())
            && draft.mode_policy==binding.mode_policy()
            && draft.fps_policy==binding.fps_policy()
            && draft.flip_vertically==binding.flip_vertically()
            && draft.deactivate_when_hidden==binding.deactivate_when_hidden() {
            let label=if *carried {
                crate::i18n::tr_key_or(ui,"SkyLive.Camera.PreviousStreamMode","Previous stream mode (reconnecting)")
            } else {
                crate::i18n::tr_key_or(ui,"SkyLive.Camera.ActiveStreamMode","Active stream mode")
            };
            let description=mode_label(ui,*mode);
            ui.label(format!("{label}: {description}"));
        }
    }
    ui.add_enabled_ui(!draft.loading(), |ui| { egui::CollapsingHeader::new(crate::i18n::tr(ui, "All reported native modes"))
        .id_salt("camera-all-native")
        .show(ui, |ui| {
            ui.label(crate::i18n::tr(ui, "Native camera modes"));
            let selected = draft.selected_mode.map_or_else(
                || crate::i18n::tr(ui, "Select mode"),
                |mode| mode_label(ui, mode),
            );
            egui::ComboBox::from_id_salt("camera-mode")
                .selected_text(selected.clone())
                .width(ui.available_width().min(450.0))
                .truncate()
                .show_ui(ui, |ui| {
                    ui.set_width(ui.available_width().min(450.0));
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                    for info in &draft.modes {
                        let label = info.mode.map_or_else(
                            || {
                                let resolution=if info.width==0||info.height==0 {
                                    format!("{}: {}",crate::i18n::tr_key(ui,"Module.win-dshow.Resolution"),crate::i18n::tr_key(ui,"EncoderMissing.Unknown"))
                                }else{format!("{}×{}",info.width,info.height)};
                                let rate=if info.fps_num==0||info.fps_den==0 {
                                    format!("{}: {}",crate::i18n::tr_key(ui,"Module.mac-avcapture.FrameRate"),crate::i18n::tr_key(ui,"EncoderMissing.Unknown"))
                                }else{format!("{}/{} fps",info.fps_num,info.fps_den)};
                                format!("{resolution} · {rate} · {}",info.native_subtype)
                            },
                            |mode| mode_label(ui, mode),
                        );
                        ui.add_enabled_ui(info.mode.is_some(), |ui| {
                            if ui.selectable_value(&mut draft.selected_mode, info.mode, label.clone())
                                .on_hover_ui(|ui| {
                                    ui.label(&label);
                                    ui.label(crate::i18n::tr(ui, info.unsupported_reason
                                        .unwrap_or("Select this exact native capture mode")));
                                })
                                .on_disabled_hover_text(crate::i18n::tr(
                                    ui,
                                    info.unsupported_reason
                                        .unwrap_or("Unsupported native camera mode"),
                                )).clicked() { draft.fps_policy = CameraFpsPolicy::Fixed; draft.mode_policy=CameraModePolicy::Fixed; }
                        });
                    }
                })
                .response
                .on_hover_ui(|ui| {
                    ui.label(selected);
                    ui.label(crate::i18n::tr(ui, "Choose exact native resolution, frame rate and pixel format"));
                });
        }); });
    if draft.selected_mode.is_some()
        && draft.queried_target == draft.selected
        && !draft
            .modes
            .iter()
            .any(|info| info.mode == draft.selected_mode)
    {
        ui.colored_label(ui.visuals().warn_fg_color, crate::i18n::tr(ui, "The selected camera mode is not reported by this device. Choose an available mode; the saved selection has not been downgraded."));
    }
    if draft.modes.is_empty() && draft.selected_mode.is_some() {
        ui.label(crate::i18n::tr(
            ui,
            "Saved mode; the device will verify it when capture starts.",
        ));
    }
    if !loading && !draft.modes.is_empty() && draft.modes.iter().all(|m| m.mode.is_none()) {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            crate::i18n::tr(
                ui,
                "This camera has no modes supported by the current raw/MJPEG capture profile.",
            ),
        );
    }
    ui.checkbox(
        &mut draft.deactivate_when_hidden,
        crate::i18n::tr(ui, "Deactivate when not showing"),
    )
    .on_hover_text(crate::i18n::tr(
        ui,
        "Release the camera while no active scene or Studio Preview shows this source",
    ));
    if matches!(draft.selected, Some(VideoCaptureTarget::Windows {..})) || draft.flip_vertically {
        ui.checkbox(&mut draft.flip_vertically, crate::i18n::tr(ui,"Flip Vertically"))
            .on_hover_text(crate::i18n::tr(ui,"Flip this camera source before filters, without changing scene item transforms or restarting the device."));
    }
    if let CameraEdit::Rebind {source}=draft.edit {
        let original=draft.original.as_ref();
        let unchanged=original.is_some_and(|binding|draft.selected.as_ref()==Some(binding.target())&&draft.selected_mode==Some(binding.mode())&&draft.mode_policy==binding.mode_policy()&&draft.fps_policy==binding.fps_policy()&&draft.deactivate_when_hidden==binding.deactivate_when_hidden()&&draft.flip_vertically==binding.flip_vertically()&&draft.audio.embedded==binding.embedded_audio()&&draft.audio.binding().is_ok_and(|audio|audio.as_ref()==binding.audio()));
        let pending=draft.av_status.as_ref().is_some_and(|status|status.pending);
        let stop_queued=draft.av_status.as_ref().is_some_and(|status|status.queued_stop.is_some());
        ui.horizontal_wrapped(|ui| {
            for (enabled,label) in [(true,"Activate Camera (Video + Audio)"),(false,"Deactivate Camera (Video + Audio)")] {
                let ready=if enabled {unchanged&&!draft.loading()&&!draft.driver.loading&&!pending&&(!draft.av_enabled||!draft.source_enabled)}else{original.is_some()&&!stop_queued&&(draft.av_enabled||pending)};
                if ui.add_enabled(ready,egui::Button::new(crate::i18n::tr(ui,label)))
                    .on_hover_text(crate::i18n::tr(ui,"Controls this source's camera video and optional custom audio together. Shared devices used by other sources remain active."))
                    .on_disabled_hover_text(crate::i18n::tr(ui,"Apply binding changes before activation. Deactivation waits actual native retirement and remains available during a pending start."))
                    .clicked(){if let Some(binding)=original {intents.push(Intent::SetCameraSourceEnabled {token:draft.token,session:draft.session,revision:draft.revision,source,binding:binding.clone(),enabled});}}
            }
        });
        super::camera_av_status::show(ui,draft.av_status.as_ref(),source);
    }
    egui::CollapsingHeader::new(crate::i18n::tr(ui,"Camera Capture Resources")).show(ui,|ui| {
        ui.label(crate::i18n::tr(ui,"Up to four cameras share one capture owner. Closing devices keep their admission until shutdown is acknowledged. Conflicting modes on the same device are reported without stopping other cameras."));
        if ui.add_enabled(!draft.av_status.as_ref().is_some_and(|status|status.queued_stop.is_some()),egui::Button::new(crate::i18n::tr(ui,"Deactivate All Camera Sources")))
            .on_hover_text(crate::i18n::tr(ui,"Deactivate camera video and custom audio for every camera source. Independent audio input sources keep working, including shared endpoints."))
            .clicked(){intents.push(Intent::StopCameras);}
    });
    super::camera_audio::editor(ui,draft,intents);
    super::camera_controls::editor(ui,draft,intents);
    ui.label(crate::i18n::tr(ui,"Apply binds camera video and optional custom input audio. Audio follows Program and uses the source mixer. Embedded camera audio, H.264 and vendor configuration dialogs are not connected. Cancel changes no source."));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsupported_native_modes_are_visible_but_never_become_a_binding() {
        let session = skylive_core::Session::new();
        let caps = CameraCapabilities {
            available: true,
            backend: "fake",
            video: true,
            audio: false,
            note: None,
        };
        let mut draft = CameraDraft::new(
            1,
            session.identity(),
            0,
            CameraEdit::Add {
                scene: session.preview(),
            },
            "Camera".into(),
            None,
            caps,
        )
        .on_platform(crate::platform::CapturePlatform::Windows);
        draft.loading_devices = false;
        draft.selected = Some(VideoCaptureTarget::Windows {
            symbolic_link: "camera".into(),
        });
        draft.devices = vec![CameraDeviceInfo {
            target: draft.selected.clone().unwrap(),
            name: "Camera".into(),
        }];
        draft.modes = vec![CameraModeInfo {
            width: 1920,
            height: 1080,
            fps_num: 30,
            fps_den: 1,
            native_subtype: "NV12".into(),
            mode: None,
            unsupported_reason: Some("unsupported"),
        }];
        assert!(draft.intent().is_err());
    }
    #[test]
    fn otherwise_identical_yuv_modes_have_distinct_color_metadata_labels() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless-translations.csv"),
        );
        let mut labels = std::collections::BTreeSet::new();
        let _=context.run(egui::RawInput::default(),|ctx|{egui::CentralPanel::default().show(ctx,|ui|{
            for matrix in [skylive_core::VideoColorMatrix::Bt601,skylive_core::VideoColorMatrix::Bt709]{for range in [skylive_core::VideoColorRange::Limited,skylive_core::VideoColorRange::Full]{for chroma_left in [false,true]{
                let mode=VideoCaptureMode::new(640,480,30,1,skylive_core::VideoPixelFormat::Nv12{matrix,range,chroma_left}).unwrap();let label=mode_label(ui,mode);assert!(label.contains("NV12")&&label.contains("BT.")&&label.contains("range")&&label.contains("Chroma"));assert!(labels.insert(label),"distinct admitted metadata must remain distinguishable in the device mode picker");
            }}}
        });});
        assert_eq!(labels.len(), 8);
    }
    #[test]
    fn portable_foreign_camera_is_retained_but_never_offered_as_a_current_device() {
        let session = skylive_core::Session::new();
        let foreign = VideoCaptureTarget::Macos {
            unique_id: "saved-mac-camera".into(),
        };
        let mode =
            VideoCaptureMode::new(640, 480, 30, 1, skylive_core::VideoPixelFormat::Rgb32).unwrap();
        let binding = VideoCaptureBinding::new(foreign.clone(), mode, false).unwrap();
        let mut draft = CameraDraft::new(
            1,
            session.identity(),
            0,
            CameraEdit::Rebind {
                source: skylive_core::Id::new(1),
            },
            "Portable".into(),
            Some(binding),
            CameraCapabilities {
                available: true,
                backend: "fake",
                video: true,
                audio: false,
                note: None,
            },
        )
        .on_platform(crate::platform::CapturePlatform::Windows);
        let key = CameraRequestKey {
            instance: 1,
            session: session.identity(),
            revision: 0,
            epoch: 0,
            serial: 1,
        };
        let current = VideoCaptureTarget::Windows {
            symbolic_link: "current-win-camera".into(),
        };
        draft.devices(
            Some((1, key)),
            key,
            session.identity(),
            0,
            Ok(vec![
                CameraDeviceInfo {
                    target: foreign.clone(),
                    name: "Foreign".into(),
                },
                CameraDeviceInfo {
                    target: current,
                    name: "Current".into(),
                },
            ]),
        );
        assert_eq!(draft.selected, Some(foreign));
        assert_eq!(draft.devices.len(), 1);
        assert!(draft.intent().is_err());
    }
}

#[cfg(test)]
#[path = "camera_selection_tests.rs"]
mod selection_tests;
