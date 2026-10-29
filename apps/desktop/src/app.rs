#[path="app_collection_library.rs"]
mod collection_library_flow;
#[path="app_document_repair.rs"]
mod document_repair_flow;
#[path="app_startup.rs"]
mod startup_flow;
#[path="app_source_output_resize.rs"]
mod source_output_resize_flow;
#[path="app_source_playback_hotkeys.rs"]
mod source_playback_hotkeys_flow;
#[path="app_source_audio_hotkeys.rs"]
mod source_audio_hotkeys_flow;
#[path="app_source_color_tags.rs"]
mod source_color_tags_flow;
#[path="app_projectors.rs"]
mod projector_flow;
// Single frontend coordinator; canonical mutations stay in Session.
#[path="app_screenshot.rs"]
mod screenshot_flow;
#[path="app_image_mask.rs"]
mod image_mask_flow;
#[path="app_lut.rs"]
mod lut_flow;
#[path="app_text_font.rs"] mod text_font;
#[path="app_freeze_timing.rs"] mod freeze_timing_flow;
#[path="app_audio_monitoring.rs"]
mod audio_monitoring_flow;
#[path="app_plugin_sources.rs"]
mod plugin_source_flow;
#[path="app_image_slideshow.rs"]
mod image_slideshow_flow;
#[path = "app_advanced_audio.rs"]
mod advanced_audio;
#[path = "app_audio_devices.rs"]
mod audio_devices;
#[path="app_camera_audio.rs"]
mod camera_audio_flow;
#[path="app_camera_activation.rs"]
mod camera_activation_flow;
#[path = "app_native_browse.rs"]
mod native_browse_flow;
#[path = "app_item_compositing.rs"]
mod item_compositing_flow;
#[path = "app_document.rs"]
mod document;
#[path = "app_edit_clipboard.rs"]
mod edit_clipboard_flow;
#[path = "app_media.rs"]
mod media_authoring;
#[path = "app_media_runtime.rs"]
mod media_runtime;
#[path="app_ndi.rs"]
mod ndi_flow;
#[path="app_browser.rs"]
mod browser_flow;
#[path="app_global_hotkeys.rs"]
mod global_hotkeys_flow;
#[path="app_process_priority.rs"]
mod process_priority_flow;
#[path="app_missing_files.rs"]
mod missing_files_flow;
#[path="app_remux.rs"]
mod remux_flow;
#[path = "app_media_transport.rs"]
mod media_transport;
#[path = "app_mixer.rs"]
mod mixer_workflow;
#[path = "app_plugin_routes.rs"]
mod plugin_routes;
#[cfg(test)]
#[path = "app_plugin_routes_tests.rs"]
mod plugin_routes_tests;
#[path = "app_recording.rs"]
pub(crate) mod recording;
#[path="app_replay_buffer.rs"]
mod replay_buffer_flow;
#[path="app_ndi_output.rs"]
mod ndi_output_flow;
#[path = "app_scene_notes_dock.rs"]
mod scene_notes_dock;
#[cfg(test)]
#[path = "app_scene_notes_dock_tests.rs"]
mod scene_notes_dock_tests;
#[path = "app_settings.rs"]
mod settings_workflow;
#[path = "app_setup.rs"]
mod setup_workflow;
#[path = "app_game_capture.rs"]
mod game_flow;
#[path = "app_virtual_camera.rs"]
mod virtual_camera_flow;
#[path = "app_streaming.rs"]
mod streaming;
use crate::{
    io::{IoWorker, Job, Reply},
    preview::PreviewPane,
    views::{
        self, DisplayEdit, ImageEdit, Intent, Layout, ManualView, ProgramAudioView,
        TransitionViewState,
        dialogs::{self, Dialog},
        menus::Catalog,
    },
};
use eframe::egui;
use skylive_core::{Command, Id, Session, TransitionPresetId, TransitionRef};
#[cfg(test)]
use skylive_host::audio_capture::{self, AudioBackendFactory, AudioCapabilities};
use skylive_host::audio_capture::{
    AudioCaptureError, AudioCaptureEvent, AudioCaptureSnapshot, AudioCaptureWorker, AudioRequestKey,
};
use skylive_host::camera_capture::{CameraError, CameraEvent, CameraOwner, CameraRequestKey};
use skylive_host::capture::{
    self, CaptureBackend, CaptureEvent, CaptureRequestKey, CaptureSnapshot, CaptureWorker,
};
use skylive_host::obs::{ExportJob, ImportVideo, ObsIdentities};
use skylive_host::playback::PlaybackClock;
use skylive_host::storage::SaveJob;
use skylive_host::transition::{Controller, FrameScene, TransitionSnapshot};
use skylive_media::MediaTime;
use std::{path::PathBuf, sync::Arc, time::Instant};

#[derive(Clone, Copy)]
enum AfterIo {
    SaveAndClose,
    SaveAndReplace,
    CheckpointAndClose,
    ReplaceDocument,
}
struct PendingImage {
    path:String,
    settings:skylive_core::ImageSourceSettings,
    dialog_epoch:u64,
    session: u64,
    revision: u64,
    edit: ImageEdit,
}
struct PendingCameraStart {
    session: u64,
    source: Id,
    binding: skylive_core::VideoCaptureBinding,
    restart: bool,
    enabled:bool,
    dialog:Option<(u64,u64)>,
}
pub struct StudioApp {
    collections:collection_library_flow::Flow,
    document_repair:document_repair_flow::Flow,
    startup:startup_flow::Flow,
    missing_files:missing_files_flow::Runtime,
    remux:remux_flow::Runtime,
    screenshot:screenshot_flow::Flow,
    global_hotkeys:global_hotkeys_flow::Runtime,
    process_priority:process_priority_flow::Runtime,
    replay:replay_buffer_flow::Runtime,
    ndi_output:ndi_output_flow::Runtime,
    statistics: crate::statistics::State,
    game:skylive_host::game_capture::GameOwner,
    game_union:capture::GameCaptureUnion,
    game_dialog_sequence:u64,
    pending_game:Option<(u64,skylive_host::game_capture::GameRequestKey)>,
    ndi:skylive_host::ndi::NdiOwner,
    browser:skylive_host::browser::BrowserOwner,
    browser_union:capture::BrowserCaptureUnion,
    browser_dialog_sequence:u64,
    browser_interaction:crate::browser_interaction::Runtime,
    ndi_union:capture::NdiCaptureUnion,
    ndi_dialog_sequence:u64,
    pending_ndi:Option<ndi_flow::PendingNdi>,
    pending_ndi_discovery:Option<(u64,skylive_host::ndi::NdiRequestKey)>,
    // The same lazy Owner is retained by the two preview workers and outputs.
    _render_gpu: skylive_host::render_gpu::RenderGpu,
    freeze_generation:u64,
    freeze_failures:u64,
    media: skylive_host::media_source::MediaOwner,
    media_union: capture::MediaCaptureUnion,
    media_error: Option<String>,
    media_scope: Option<media_runtime::Scope>,
    media_frame_at: Option<MediaTime>,
    pending_media: Option<media_authoring::PendingMedia>,
    pending_media_control: Option<media_transport::PendingMediaControl>,
    media_dialog_sequence: u64,
    recording: recording::Runtime,
    streaming: streaming::Runtime,
    virtual_camera: virtual_camera_flow::Runtime,
    platform: crate::platform::CapturePlatform,
    projectors:crate::projectors::Runtime,
    session: Session,
    io: IoWorker,
    image_watch: crate::image_watch::Watch,
    image_watch_error: Option<(Id, String)>,
    slideshow_watch: crate::slideshow_watch::Watch,
    slideshow_watch_error: Option<(Id, String)>,
    mask_flow:image_mask_flow::Flow,
    lut_flow:lut_flow::Flow,
    text_font:text_font::Flow,
    video_filter_dialog_sequence:u64,
    native_browse: native_browse_flow::Flow,
    camera: CameraOwner,
    capture_union: capture::CaptureUnion,
    camera_error: Option<String>,
    camera_dialog_sequence: u64,
    pending_camera: Option<(u64, CameraRequestKey)>,
    pending_camera_controls: Option<(u64, CameraRequestKey)>,
    camera_activation:skylive_host::camera_activation::Owner,
    camera_start: Option<PendingCameraStart>,
    camera_stop: bool,
    explicit_camera_stop: Option<CameraRequestKey>,
    capture: CaptureWorker,
    capture_error: Option<String>,
    window_dialog_sequence: u64,
    pending_windows: Option<(u64, CaptureRequestKey)>,
    display_dialog_sequence: u64,
    pending_displays: Option<(u64, CaptureRequestKey)>,
    audio: AudioCaptureWorker,
    audio_error: Option<String>,
    audio_next_request: Option<u64>,
    audio_stop_pending: bool,
    audio_dialog_sequence: u64,
    pending_audio_devices: Option<(u64, AudioRequestKey)>,
    pending_camera_audio_devices: Option<(u64, AudioRequestKey)>,
    pending_monitor_devices:Option<AudioRequestKey>,
    monitor_devices:Vec<skylive_host::audio_capture::AudioDeviceInfo>,
    monitor_devices_session:u64,
    monitor_device_error:Option<String>,
    program_audio: Option<ProgramAudioView>,
    audio_program_revision: Option<(u64, u64)>,
    preview: PreviewPane,
    preview_interaction: crate::preview_interaction::PreviewInteraction,
    program: PreviewPane,
    catalog: Catalog,
    edit_clipboard: crate::edit_clipboard::Clipboard,
    source_clipboard: crate::source_clipboard::Clipboard,
    always_on_top: bool,
    status_visible: bool,
    preview_locked: bool,
    layout: Layout,
    selected: Option<Id>,
    studio: bool,
    transition: Controller,
    transition_active: bool,
    transition_manual: Option<ManualView>,
    transition_needs_tick: bool,
    transition_preset_focus: Option<TransitionPresetId>,
    frame_now: MediaTime,
    document_path: String,
    active_document_path: Option<PathBuf>,
    native_save_draft: String,
    documents: document::Flow,
    obs_path: String,
    obs_identities: Option<Arc<ObsIdentities>>,
    dialog: Option<Dialog>,
    plugin_dialog_sequence: u64,
    notes_dock: views::scene_notes_dock::NotesDock,
    settings_flow: settings_workflow::Flow,
    setup: views::setup::SetupFlow,
    status: String,
    status_presentation: crate::status::Presentation,
    error: bool,
    close_dialog: bool,
    after_io: Option<AfterIo>,
    pending_image: Option<PendingImage>,
    pending_slideshow: Option<(u64,views::image_slideshow::Submission)>,
    slideshow_dialog_sequence:u64,
    playback: PlaybackClock,
    playback_origin: Instant,
    playback_error: Option<String>,
    restart_image: Option<Id>,
    allow_close: bool,
    localization: Arc<skylive_host::localization::Localization>,
    localization_path: PathBuf,
    appearance_runtime: crate::theme::Runtime,
    preferences_path: Option<PathBuf>,
    saved_preferences: skylive_core::UserPreferences,
    failed_preferences: Option<skylive_core::UserPreferences>,
    pending_preferences: Option<skylive_core::UserPreferences>,
    close_after_preferences: bool,
}
impl StudioApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut app = Self::with_workers(
            &cc.egui_ctx,
            capture::native::backend(),
            AudioCaptureWorker::native()?,
            crate::platform::CapturePlatform::current(),
        )?;
        #[cfg(target_os="windows")]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            app.native_browse.owner=cc.window_handle().ok().and_then(|handle|match handle.as_raw() {
                RawWindowHandle::Win32(win)=>Some(win.hwnd.get()),_=>None,
            });
        }
        app.begin_startup(&cc.egui_ctx);
        Ok(app)
    }

    #[cfg(test)]
    fn headless(context: &egui::Context) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::with_capture_backend(context, Box::new(capture::UnsupportedBackend))
    }
    #[cfg(test)]
    fn with_capture_backend(
        context: &egui::Context,
        backend: Box<dyn CaptureBackend>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let caps = AudioCapabilities {
            backend: "unsupported test backend",
            input: false,
            output: false,
            device_enumeration: false,
            exclude_own_audio: false,
            note: Some("Headless tests never access native audio"),
        };
        let factory: AudioBackendFactory =
            Box::new(|| Err(AudioCaptureError::Unsupported("headless audio backend")));
        Self::with_workers(
            context,
            backend,
            AudioCaptureWorker::new(factory, caps)?,
            crate::platform::CapturePlatform::Windows,
        )
    }
    fn camera_owner() -> Result<CameraOwner, CameraError> {
        #[cfg(not(test))]
        {
            CameraOwner::native()
        }
        #[cfg(test)]
        {
            CameraOwner::new(
                Box::new(|| Err(CameraError::Unavailable("headless camera fixture"))),
                skylive_host::camera_capture::CameraCapabilities {
                    available: false,
                    backend: "headless-unavailable",
                    video: false,
                    audio: false,
                    note: None,
                },
                skylive_host::camera_capture::CameraLimits::default(),
            )
        }
    }
    fn with_workers(
        context: &egui::Context,
        backend: Box<dyn CaptureBackend>,
        audio: AudioCaptureWorker,
        platform: crate::platform::CapturePlatform,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        crate::theme::install(context);
        let localization = crate::i18n::english()?;
        let localization_path = crate::i18n::workspace_path();
        crate::i18n::install(context, localization.clone(), &localization_path);
        let mut media =
            skylive_host::media_source::MediaOwner::with_audio_sink(audio.media_sink())?;
        let media_context = context.clone();
        media.set_waker(Arc::new(move || media_context.request_repaint()))?;
        #[cfg(test)]
        let render_gpu=skylive_host::render_gpu::RenderGpu::cpu();
        #[cfg(not(test))]
        let render_gpu=skylive_host::render_gpu::RenderGpu::new();
        Ok(Self {
            collections:Default::default(),
            document_repair:Default::default(),
            startup:Default::default(),
            projectors:Default::default(),
            missing_files:Default::default(),
            remux:Default::default(),
            statistics: Default::default(),
            screenshot:Default::default(),
            global_hotkeys:global_hotkeys_flow::Runtime::new(context,audio.push_audio_sink())?,
            process_priority:Default::default(),
            game:skylive_host::game_capture::GameOwner::new()?,game_union:Default::default(),game_dialog_sequence:0,pending_game:None,
            ndi:skylive_host::ndi::NdiOwner::new(audio.ndi_sink())?,
            browser:skylive_host::browser::BrowserOwner::new(audio.browser_sink())?,browser_union:Default::default(),browser_dialog_sequence:0,
            browser_interaction:Default::default(),
            ndi_union:Default::default(),ndi_dialog_sequence:0,pending_ndi:None,pending_ndi_discovery:None,
            replay:replay_buffer_flow::Runtime::new(render_gpu.clone()),
            ndi_output:ndi_output_flow::Runtime::new(render_gpu.clone()),
            _render_gpu: render_gpu.clone(),
            freeze_generation:0,
            freeze_failures:0,
            media,
            media_scope: None,
            media_frame_at: None,
            media_error: None,
            media_union: Default::default(),
            pending_media: None,
            pending_media_control: None,
            media_dialog_sequence: 0,
            recording: recording::Runtime::new_with_render_gpu(render_gpu.clone())?,
            streaming: streaming::Runtime::new_with_render_gpu(render_gpu.clone()),
            virtual_camera: virtual_camera_flow::Runtime::new(render_gpu.clone())?,
            platform,
            explicit_camera_stop: None,
            appearance_runtime: Default::default(),
            preferences_path: None,
            saved_preferences: skylive_core::UserPreferences::default(),
            failed_preferences: None,
            pending_preferences: None,
            close_after_preferences: false,
            localization,
            localization_path,
            session: Session::new(),
            io: IoWorker::new(context.clone())?,
            image_watch:crate::image_watch::Watch::new(context.clone())?,
            image_watch_error:None,
            slideshow_watch:crate::slideshow_watch::Watch::new(context.clone())?,
            slideshow_watch_error:None,
            mask_flow:Default::default(),lut_flow:Default::default(),text_font:Default::default(),video_filter_dialog_sequence:0,
            native_browse: Default::default(),
            camera: Self::camera_owner()?,
            capture_union: Default::default(),
            camera_error: None,
            camera_dialog_sequence: 0,
            pending_camera: None,
            pending_camera_controls: None,
            camera_activation:Default::default(),
            camera_start: None,
            camera_stop: false,
            capture: CaptureWorker::new(backend)?,
            capture_error: None,
            window_dialog_sequence: 0,
            pending_windows: None,
            display_dialog_sequence: 0,
            pending_displays: None,
            audio,
            audio_error: None,
            audio_next_request: None,
            audio_stop_pending: false,
            audio_dialog_sequence: 0,
            pending_audio_devices: None,
            pending_camera_audio_devices: None,
            pending_monitor_devices:None,monitor_devices:vec![],monitor_devices_session:0,monitor_device_error:None,
            program_audio: None,
            audio_program_revision: None,
            preview: PreviewPane::new_with_render_gpu("preview",&render_gpu)?,
            preview_interaction: Default::default(),
            program: PreviewPane::new_with_render_gpu("program",&render_gpu)?,
            catalog: Catalog::load()?,
            edit_clipboard: Default::default(),
            source_clipboard: Default::default(),
            always_on_top: false,
            status_visible: true,
            preview_locked: false,
            layout: Layout::default(),
            selected: None,
            studio: false,
            transition: Controller::new(),
            transition_active: false,
            transition_manual: None,
            transition_needs_tick: false,
            transition_preset_focus: None,
            frame_now: MediaTime(0),
            document_path: "untitled.skylive.json".into(),
            active_document_path: None,
            native_save_draft: "untitled.skylive.json".into(),
            documents: Default::default(),
            dialog: None,
            plugin_dialog_sequence: 0,
            notes_dock: Default::default(),
            settings_flow: Default::default(),
            setup: Default::default(),
            obs_path: "collection.obs.json".into(),
            obs_identities: None,
            status: "Ready".into(),
            status_presentation: crate::status::Presentation::Chrome("Ready"),
            error: false,
            close_dialog: false,
            after_io: None,
            pending_image: None,
            pending_slideshow:None,
            slideshow_dialog_sequence:0,
            allow_close: false,
            playback: render_gpu.playback_clock()?,
            playback_origin: Instant::now(),
            playback_error: None,
            restart_image: None,
        })
    }
    fn request_localization(&mut self, context: &egui::Context, language: String, commit: bool) {
        let result = self.io.submit(
            self.session.identity(),
            Job::LoadLocalization {
                path: self.localization_path.clone(),
                language,
                revision: self.session.snapshot().revision(),
                commit,
            },
        );
        match result {
            Ok(()) => self.message(false, "Preparing localization…"),
            Err(error) => self.message(true, error),
        }
        context.request_repaint();
    }
    fn message(&mut self, error: bool, message: impl Into<crate::status::Status>) {
        self.error = error;
        let (raw, presentation) = message.into().into_parts();
        self.status = raw;
        self.status_presentation = presentation;
    }
    fn apply(&mut self, command: Command) {
        self.apply_at(None, command);
    }
    fn apply_at(&mut self, expected: Option<u64>, command: Command) {
        self.publish_command_at(expected, command, true);
    }
    fn publish_command_at(
        &mut self,
        expected: Option<u64>,
        command: Command,
        close_dialog: bool,
    ) -> bool {
        self.publish_command_with_gate(expected,command,close_dialog,||true)
    }
    fn publish_command_with_gate<F:FnOnce()->bool>(&mut self,expected:Option<u64>,command:Command,close_dialog:bool,gate:F)->bool{
        self.publish_command_projected_with_gate(expected,command,close_dialog,gate,|_,_|()).is_some()
    }
    /// Project bounded committed receipt scalars before postpublication hooks.
    /// A later backend/hook error cannot turn committed Some into fake rollback.
    fn publish_command_projected_with_gate<R,F:FnOnce()->bool,P:FnOnce(&Session,&skylive_core::Change)->R>(&mut self,expected:Option<u64>,command:Command,close_dialog:bool,gate:F,project:P)->Option<R>{
        if self.ndi_output.owner.any_active()&&skylive_host::ndi::output::blocks_command(&command){self.message(true,"Stop NDI outputs before changing output settings, formats or the active profile");return None;}
        if self.virtual_camera.owned()&&skylive_host::virtual_camera::blocks_format_change(&command) {
            self.message(true,"Stop the virtual camera before changing the video format or active profile.");return None;
        }
        if self.replay.active()&&recording::blocks_command(&command){self.message(true,"Stop Replay Buffer before changing the audio/video format or active profile");return None;}
        if self.streaming.active()&&self.streaming.blocks_command(&command)
        {
            self.message(true, "Stop streaming before changing the output settings, audio/video format or active profile");
            return None;
        }
        if self.recording.active() && recording::blocks_command(&command) {
            self.message(
                true,
                "Stop recording before changing the audio/video format or active profile",
            );
            return None;
        }
        let open_new_profile_setup = matches!(
            &command,
            Command::Setup {
                command: skylive_core::SetupCommand::CreateProfile {
                    run_setup: true,
                    ..
                }
            }
        );
        let command=if let Command::ResizeOutputToSource{scene,item,raw_width,raw_height}=command {
            let capture=self.source_capture_snapshot();
            let plan=match skylive_host::source_output_resize::Plan::prepare(&self.session,&capture,scene,item){Ok(plan)=>plan,Err(error)=>{self.message(true,error.to_string());return None;}};
            if (raw_width,raw_height)!=(plan.size().raw_width,plan.size().raw_height){self.message(true,"Declared source dimensions do not match the current source frame.");return None;}
            match plan.command(&self.session,&capture){Ok(command)=>command,Err(error)=>{self.message(true,error.to_string());return None;}}
        }else{command};
        let command = if let Command::TransformItem {
            scene,
            item,
            action,
        } = command
        {
            if self.session.is_closed() {
                self.message(true, skylive_core::Error::Closed.to_string());
                return None;
            }
            let actual = self.session.snapshot().revision();
            if let Some(expected) = expected.filter(|expected| *expected != actual) {
                self.message(
                    true,
                    skylive_core::Error::Conflict { expected, actual }.to_string(),
                );
                return None;
            }
            match capture::prepare_transform_command(
                &self.session,
                &self.source_capture_snapshot(),
                scene,
                item,
                action,
            ) {
                Ok(command) => command,
                Err(error) => {
                    self.message(true, error.to_string());
                    return None;
                }
            }
        } else {
            command
        };
        if let Err(error) = self.transition.before_change(&self.session) {
            self.message(true, error.to_string());
            return None;
        }
        if let Err(error) = {self.process_priority.invalidate();self.camera.before_session_change()} {
            self.message(true,error.to_string());
            return None;
        }
        match self.session.apply_at_with_gate(expected, command,gate) {
            Ok(None)=>None,
            Ok(Some(change)) => {
                let receipt=project(&self.session,&change);
                if let Err(error)=skylive_host::freeze_timing_collect::observe_canonical(&self._render_gpu,&self.session,self.frame_now.0){self.message(true,error);}
                self.invalidate_shortcut_callbacks();
                self.ndi_output.owner.synchronize(&self.session);
                self.source_clipboard.synchronize(&self.session);
                if let Err(error) = self.edit_clipboard.synchronize(&self.session) {
                    self.message(true, error);
                }
                self.discard_stale_program_audio();
                // The command owner decides whether a draft committed. Keep a
                // rejected draft open so the user can correct exact values.
                if close_dialog {
                    self.dialog = None;
                }
                if let Some(id) = change.created_transition_preset {
                    self.transition_preset_focus = Some(id);
                }
                self.message(
                    false,
                    if change.changed {
                        crate::status::Status::formatted(
                            "Revision {}",
                            &[("{}", change.revision.to_string())],
                        )
                    } else {
                        crate::status::Status::from("No change")
                    },
                );
                if let Some(created) = change.created {
                    if self.session.snapshot().scene(created).is_some() {
                        self.selected = None;
                        if let Err(error) = self.transition.select_preview(
                            &mut self.session,
                            created,
                            self.frame_now,
                        ) {
                            self.message(true, error.to_string());
                        }
                    } else {
                        self.selected = Some(created);
                    }
                }
                let _ = self.sync_transition();
                let model = self.session.snapshot();
                if change.created_transition_preset.is_none() {
                    if let TransitionRef::Preset(id) = model.transition().reference() {
                        self.transition_preset_focus = Some(id);
                    }
                }
                if self
                    .transition_preset_focus
                    .is_some_and(|id| model.transition_preset(id).is_none())
                {
                    self.transition_preset_focus = None;
                }
                if !self
                    .selected
                    .and_then(|item| model.item_container(item))
                    .is_some_and(|(container, _)| {
                        model.root_scene_for_container(container) == Some(self.session.preview())
                    })
                {
                    self.selected = None;
                }
                if open_new_profile_setup {
                    self.open_setup(views::setup::SetupOrigin::NewProfile);
                }
                Some(receipt)
            }
            Err(error) => {
                self.message(true, error.to_string());
                None
            }
        }
    }
    fn sync_transition(&mut self) -> Option<TransitionSnapshot> {
        match self.transition.sync(&mut self.session, self.frame_now) {
            Ok(snapshot) => {
                self.studio = self.transition.studio_mode();
                self.transition_active = snapshot.active;
                self.transition_manual = snapshot.manual.as_ref().map(|manual| ManualView {
                    value: manual.value,
                    current: manual.current,
                    target: manual.target,
                    finishing: manual.finishing,
                });
                self.transition_needs_tick = snapshot.needs_tick;
                Some(snapshot)
            }
            Err(skylive_host::transition::TransitionError::AutoTextRetry)=>None,
            Err(error) => {
                self.message(true, error.to_string());
                None
            }
        }
    }
    fn start_io(&mut self, job: Job, after: Option<AfterIo>) {
        match self.io.submit(self.session.identity(), job) {
            Ok(()) => {
                self.after_io = after;
                self.message(false, "Document operation in progress…");
            }
            Err(error) => {
                self.after_io = None;
                self.message(true, error);
            }
        }
    }
    fn path(&self) -> Result<PathBuf, String> {
        if self.document_path.trim().is_empty() {
            Err("Enter a document path".into())
        } else {
            Ok(PathBuf::from(&self.document_path))
        }
    }
    fn save(&mut self, after: Option<AfterIo>) {
        if let Some(path) = self.active_document_path.clone() {
            self.save_to(path, after);
        } else {
            self.open_save_as(after);
        }
    }
    fn request_exit(&mut self, context: &egui::Context) {
        self.cancel_pending_recording_start();
        self.missing_files.panel=None;
        self.remux.owner.close();self.remux.view.open=false;
        self.virtual_camera.settings=None;
        self.cancel_startup();
        if self.document_repair.owns()||self.native_replacement_pending(){self.cancel_pending_open(context);}
        self.cancel_library_draft();
        self.documents.cancel();
        if self.save_projectors_before_clean_exit(context){return;}
        if self.session.is_document_dirty()
            || (self.io.busy() && self.pending_preferences.is_none())
        {
            self.close_dialog = true;
        } else {
            self.shutdown(context);
        }
    }
    fn preferences_unsaved(&self) -> bool {
        self.preferences_path.is_some()
            && self.session.snapshot().preferences() != &self.saved_preferences
    }
    fn persist_preferences(&mut self) {
        if self.io.busy() || self.documents.modal_open() || !self.preferences_unsaved() {
            return;
        }
        let preferences = self.session.snapshot().preferences().clone();
        if self.failed_preferences.as_ref() == Some(&preferences) {
            return;
        }
        let Some(path) = self.preferences_path.clone() else {
            return;
        };
        match self.io.submit(
            self.session.identity(),
            Job::SavePreferences {
                path,
                preferences: preferences.clone(),
            },
        ) {
            Ok(()) => self.pending_preferences = Some(preferences),
            Err(error) => self.message(true, error),
        }
    }
    fn shutdown(&mut self, context: &egui::Context) {
        let streaming_wait = self.wait_for_streaming_before_close(context);
        let replay_wait=self.wait_replay_close(context);
        let ndi_wait=self.wait_ndi_output_close(context);
        let recording_wait = self.wait_for_recording_before_close(context);
        let virtual_camera_wait = self.wait_for_virtual_camera_before_close(context);
        if streaming_wait || recording_wait || virtual_camera_wait || replay_wait || ndi_wait {
            return;
        }
        if self.save_projectors_before_clean_exit(context){return;}
        if self.wait_pending_open_before_close(context){return;}
        if self.wait_library_close(context){return;}
        if self.wait_last_native_document_before_close(context){return;}
        if self.preferences_unsaved() || self.pending_preferences.is_some() {
            self.close_after_preferences = true;
            self.persist_preferences();
            if !self.io.busy() {
                self.close_dialog = true;
            }
            return;
        }
        if let Err(error)={self.process_priority.invalidate();self.camera.before_session_change()} {self.message(true,error.to_string());return;}
        if self.wait_remux_close(context){return;}
        if self.wait_process_priority_close(context){return;}
        self.projectors.close(context);
        self.image_watch.close();
        self.slideshow_watch.close();
        self.session.close();
        self.preview.close();
        self.program.close();
        self.capture_union.clear();
        self.ndi_union.clear();
        self.media_union.clear();
        self.pending_media_control = None;
        self.ndi.close();
        self.global_hotkeys.owner.close();
        self.browser_interaction.close();
        self.browser.close();self.browser_union.clear();
        self.media.close();
        self.capture.close();
        self.camera.close();
        self.audio.close();
        self.replay.close();
        self.ndi_output.close();
        self.recording.close();
        self.streaming.close();
        self.game_union.clear();self.game.close();
        self.virtual_camera.close();
        self.image_watch.close();
        self.io.close();
        self.allow_close = true;
        context.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    fn poll_io(&mut self, context: &egui::Context) {
        let Some(completion) = self.io.poll() else {
            return;
        };
        let Some(completion)=self.startup_completion(completion,context) else{return;};
        let Some(completion)=self.repair_completion(completion,context) else{return;};
        let Some(completion)=self.library_completion(completion,context) else{return;};
        let Some(completion)=self.screenshot_completion(completion,context) else{return;};
        let Some(completion)=self.native_browse_completion(completion,context) else {return;};
        let Some(completion)=self.missing_files_completion(completion) else{return;};
        let Some(completion)=self.projector_completion(completion,context) else{return;};
        let Some(completion)=self.text_font_completion(completion) else{return;};
        let Some(completion)=self.lut_completion(completion) else{return;};
        let Some(completion)=self.mask_completion(completion) else{return;};
        let Some(completion)=self.slideshow_completion(completion) else {return;};
        let Some(completion) = self.document_completion(completion, context) else {
            return;
        };
        if self.settings_flow.pending_request().is_some() {
            self.complete_settings_io(completion, context);
            return;
        }
        let completion_request = completion.request;
        let pending_preferences = self.pending_preferences.take();
        let after = self.after_io.take();
        let pending_image = self.pending_image.take();
        if completion.session != self.session.identity() && completion.session != 0 {
            self.document_save_failed(after);
            self.documents.cancel();
            return;
        }
        match completion.result {
            Ok(Reply::NvencProbed {token,catalog})=>{
                if Some(token)!=self.settings_flow.token()||token.session!=self.session.identity()||token.revision!=self.session.snapshot().revision(){return;}
                match &mut self.dialog {Some(Dialog::Recording(d))=>{d.encoder.probing=false;d.encoder.catalog=Some(catalog);},Some(Dialog::Streaming(d))=>{d.encoder.probing=false;d.encoder.catalog=Some(catalog);},_=>{}}
            },
            Ok(Reply::VirtualCameraInstalled(installed)) => self.message(false, if installed {"Virtual camera installed. You can start it from Controls."} else {"Virtual-camera installation was cancelled."}),
            Ok(Reply::ProfileLoaded { profile, revision }) => {
                self.publish_command_at(Some(revision), Command::Profile { command: skylive_core::ProfileCommand::Import { profile } }, true);
            }
            Ok(Reply::ProfileExported(path)) => {
                if matches!(&self.dialog, Some(Dialog::ProfilePath {import:false,path:field}) if std::path::Path::new(field)==path.as_path()) { self.dialog=None; }
                self.message(false, crate::status::Status::formatted("Profile exported: {path}", &[("{path}", path.display().to_string())]));
            }
            Ok(Reply::LibraryListed(_)|Reply::LibraryCreated(_)|Reply::LibraryOpened(_)|Reply::LibraryRemovalPrepared(_)|Reply::LibraryRemoved(_)|Reply::LibraryNameSynced(_))=>self.message(true,"Collection library returned an unexpected receipt."),
            Ok(Reply::StartupRepairLoaded{..}|Reply::DocumentRepair(_)|Reply::RepairPage(_)|Reply::CandidateRelink(_)|Reply::CandidateDiscarded|Reply::RepairProjectionReturned(_)|Reply::RepairCommitted)=>self.message(true,"Pending collection repair has no matching request."),
            Ok(Reply::StartupLoaded(_) | Reply::StartupRetired | Reply::NativeRemembered(_))=>self.message(true,"Startup preparation returned an unexpected receipt."),
            Ok(Reply::ProjectorsResolved(_) | Reply::ProjectorMonitors(_))=>self.message(true,"Projector restoration returned an unexpected receipt."),
            Ok(Reply::MissingFilesScanned(_)|Reply::RelinkPrepared(_))=>self.message(true,"Missing Files has no matching request."),
            Ok(Reply::ScreenshotSaved(_))=>self.message(true,"Screenshot receipt has no matching request."),
            Ok(Reply::PathsChosen(_)) => self.message(true,"Native file selection has no active matching request."),
            Ok(Reply::DirectoryRequested(path)) => {
                self.message(false, crate::status::Status::formatted("Folder requested: {path}", &[("{path}", path.display().to_string())]));
            }
            Ok(Reply::PreferencesSaved(preferences)) => {
                self.saved_preferences = preferences;
                self.failed_preferences = None;
                if self.close_after_preferences {
                    self.shutdown(context);
                }
            }
            Ok(Reply::Localized {
                localization,
                revision,
                commit,
            }) => {
                if self.session.is_closed() || self.session.snapshot().revision() != revision {
                    self.message(
                        true,
                        "Localization result is obsolete; previous translations were retained",
                    );
                    return;
                }
                if commit {
                    if let Err(error)={self.process_priority.invalidate();self.camera.before_session_change()} {self.message(true,error.to_string());return;}
                    if let Err(error) = self.session.apply_at(
                        Some(revision),
                        Command::SetLanguage {
                            language: localization.language().to_owned(),
                        },
                    ) {
                        self.message(true, error.to_string());
                        return;
                    }
                    self.dialog = None;
                } else if self.session.snapshot().preferences().language()
                    != localization.language()
                {
                    return;
                }
                self.localization = localization;
                crate::i18n::install(context, self.localization.clone(), &self.localization_path);
                self.message(
                    false,
                    crate::status::Status::formatted(
                        "Localization loaded: {} · {} missing translations use English",
                        &[
                            ("{}", self.localization.language().to_owned()),
                            ("{}", self.localization.missing().to_string()),
                        ],
                    ),
                );
            }
            Ok(Reply::Saved { receipt, path }) => {
                self.saved_document(completion_request, path, receipt, after, context)
            }
            Ok(Reply::Checkpointed) => {
                self.message(false, "Recovery checkpoint saved");
                if matches!(after, Some(AfterIo::CheckpointAndClose)) {
                    self.shutdown(context);
                }
            }
            Ok(Reply::Loaded { .. } | Reply::Imported(_)) => self.message(
                true,
                "Replacement result has no active request; the current document was retained",
            ),
            Ok(Reply::ObsCameraPrepared(_)) => self.message(true,"OBS camera discovery returned without an active import request"),
            Ok(Reply::ObsExported) => self.message(
                false,
                "OBS subset exported. Native unsaved state is unchanged",
            ),
            Ok(Reply::SlideShowPrepared {..})=>self.message(true,"Slideshow receipt has no active matching preparation"),
            Ok(Reply::MaskPrepared(_))=>self.message(true,"Mask image returned an unexpected receipt."),
            Ok(Reply::SystemFontsDiscovered(_))=>self.message(true,"System font discovery has no active matching request."),
            Ok(Reply::FontPrepared(_))=>self.message(true,"Font preparation has no active matching request."),
            Ok(Reply::LutPrepared(_))=>self.message(true,"LUT preparation has no active matching request."),
            Ok(Reply::ImagePrepared(asset)) => {
                if let Some(pending) =
                    pending_image.filter(|pending| pending.session == self.session.identity()
                        &&pending.dialog_epoch==self.native_browse.epoch()
                        &&self.dialog.as_ref().is_some_and(|dialog|dialog.matches_image(&pending.edit,&pending.path,pending.settings)))
                {
                    let restart = match &pending.edit {
                        ImageEdit::Replace { source } => Some(*source),
                        ImageEdit::Add { .. } => None,
                    };
                    let command = match pending.edit {
                        ImageEdit::Add { scene, name } => {
                            Command::ImageSource {command:skylive_core::ImageSourceCommand::Add{scene,name,asset,settings:pending.settings}}
                        }
                        ImageEdit::Replace { source } => Command::ImageSource {command:skylive_core::ImageSourceCommand::Configure{source,asset,settings:pending.settings}},
                    };
                    self.apply_at(Some(pending.revision), command);
                    if !self.error {
                        self.restart_image = restart;
                        if let Err(error) = self.recording.invalidate_resources() {
                            self.stop_recording();
                            self.message(true, error);
                        }
                        if let Err(error) = self.streaming.invalidate_resources() {
                            self.stop_streaming();
                            self.message(true, error);
                        }
                        if let Err(error) = self.virtual_camera.invalidate_resources() {
                            self.stop_virtual_camera();self.message(true,error);
                        }
                        // A restored identical file produces a canonical no-op.
                        // Retry the derived resource without inventing history.
                        if let Err(error) = self
                            .preview
                            .retry_resources()
                            .and_then(|()| self.program.retry_resources())
                        {
                            self.message(true, error.to_string());
                        }
                    }
                } else {
                    self.message(
                        true,
                        "The image properties changed or closed before preparation finished. The prepared image was not applied.",
                    );
                }
            }
            Err(error) => {
                self.document_save_failed(after);
                if let Some(preferences) = pending_preferences {
                    self.failed_preferences = Some(preferences);
                    self.close_dialog |= self.close_after_preferences;
                }
                self.close_dialog |= matches!(
                    after,
                    Some(AfterIo::SaveAndClose | AfterIo::CheckpointAndClose)
                );
                let error=if matches!(&self.dialog,Some(Dialog::ProfilePath {..})) {crate::i18n::tr_context(context,&error)}else{error};
                self.message(true, error);
            }
        }
    }
    fn intent(&mut self, intent: Intent, context: &egui::Context) {
        let Some(intent)=self.defer_startup_intent(intent,context)else{return;};
        if let Intent::GlobalHotkey(event)=&intent{self.global_hotkey(event.clone(),context);return;}
        let Some(intent)=self.defer_repair_intent(intent,context)else{return;};
        if self.document_repair.owns()&&matches!(&intent,Intent::ResolveDocument(views::document::Choice::Cancel)|Intent::CancelDocumentSave|Intent::CancelDocumentPath){self.cancel_pending_open(context);return;}
        if self.document_repair.owns()&&!matches!(&intent,Intent::RepairFiles(..)|Intent::CancelPendingOpen|Intent::RetryPendingOpen|Intent::NativeBrowse(_)|Intent::ResolveDocument(_)|Intent::SaveAsTo{..}|Intent::StopRecording|Intent::SetRecordingPaused{..}|Intent::StopStreaming|Intent::StopReplayBuffer|Intent::StopVirtualCamera|Intent::NdiOutput(views::ndi_output::Action::Stop(_))|Intent::Exit){self.message(true,"Finish or cancel the pending collection before editing the current project.");return;}
        if (self.collections.modal_open()||self.collections.removal_pending())&&!matches!(&intent,
            Intent::CollectionLibrary(views::collection_library::Action::Confirm{..}|views::collection_library::Action::Cancel(_))
            |Intent::StopRecording|Intent::SetRecordingPaused{..}|Intent::StopStreaming|Intent::StopReplayBuffer
            |Intent::StopVirtualCamera|Intent::NdiOutput(views::ndi_output::Action::Stop(_))|Intent::Exit){
            self.message(true,"Finish or cancel the managed collection operation first.");return;
        }
        if self.missing_files.panel.is_some()&&!matches!(&intent,Intent::MissingFiles(..)|Intent::NativeBrowse(_)|Intent::StopRecording|Intent::SetRecordingPaused{..}|Intent::StopStreaming|Intent::StopReplayBuffer|Intent::StopVirtualCamera|Intent::NdiOutput(views::ndi_output::Action::Stop(_))|Intent::Exit){self.message(true,"Finish or cancel Missing Files first.");return;}
        // Transition controller paths publish outside publish_command_at.
        if matches!(&intent,Intent::Quick(_)|Intent::ManualPosition(_)|Intent::ManualRelease|Intent::SelectScene(_)|Intent::Cut|Intent::Transition|Intent::StudioMode(_)) {
            if let Err(error)={self.process_priority.invalidate();self.camera.before_session_change()} {self.message(true,error.to_string());return;}
        }
        if self.streaming.active()
            && matches!(
                &intent,
                Intent::Load | Intent::Recover | Intent::NewCollection | Intent::ImportObs { .. }
            )
        {
            self.message(true, "Stop streaming before replacing the scene collection");
            return;
        }
        if self.virtual_camera.owned()
            && matches!(&intent,Intent::Load|Intent::Recover|Intent::NewCollection|Intent::ImportObs {..}) {
            self.message(true,"Stop the virtual camera before replacing the scene collection");return;
        }
        if self.virtual_camera.settings_open()
            && !matches!(&intent,Intent::ApplyVirtualCameraSettings(_)|Intent::CloseVirtualCameraSettings(_)
                |Intent::StopVirtualCamera|Intent::StopStreaming|Intent::StopRecording|Intent::SetRecordingPaused {..}|Intent::StopReplayBuffer|Intent::NdiOutput(views::ndi_output::Action::Stop(_))|Intent::Exit) {
            self.message(true,"Finish or close virtual-camera settings first");return;
        }
        if self.recording.active()
            && matches!(
                &intent,
                Intent::Load | Intent::Recover | Intent::NewCollection | Intent::ImportObs { .. }
            )
        {
            self.message(true, "Stop recording before replacing the scene collection");
            return;
        }
        if self.documents.modal_open()
            && !matches!(
                &intent,
                Intent::NativeBrowse(_)
                    | Intent::ResolveDocument(_)
                    | Intent::ConfirmDocumentPath(_)
                    | Intent::CancelDocumentPath
                    | Intent::SaveAsTo { .. }
                    | Intent::CancelDocumentSave
                    | Intent::CancelPendingOpen
                    | Intent::RepairFiles(_,views::missing_files::Action::Cancel)
                    | Intent::MissingFiles(_,views::missing_files::Action::Cancel)
                    | Intent::Remux(views::remux::Action::Cancel(_)|views::remux::Action::Close)
                    | Intent::StopStreaming
                    | Intent::StopRecording
                    | Intent::SetRecordingPaused {..}
                    | Intent::NdiOutput(views::ndi_output::Action::Stop(_))
                    | Intent::StopReplayBuffer
                    | Intent::StopVirtualCamera
                    | Intent::CloseVirtualCameraSettings(_)
                    | Intent::Exit
            )
        {
            self.message(
                true,
                "Finish or cancel the current document operation first",
            );
            return;
        }
        if self.io.busy()
            && !matches!(
                intent,
                Intent::Exit
                    | Intent::CancelPendingOpen
                    | Intent::RepairFiles(_,views::missing_files::Action::Cancel)
                    | Intent::MissingFiles(_,views::missing_files::Action::Cancel)
                    | Intent::Remux(views::remux::Action::Cancel(_)|views::remux::Action::Close)
                    | Intent::StopStreaming
                    | Intent::StopRecording
                    | Intent::SetRecordingPaused {..}
                    | Intent::StopReplayBuffer
                    | Intent::NdiOutput(views::ndi_output::Action::Stop(_))
                    | Intent::StopVirtualCamera
                    | Intent::CloseVirtualCameraSettings(_)
                    | Intent::SetupAction(_, views::setup::SetupAction::Cancel)
            )
        {
            self.message(
                true,
                "Wait for the active document operation before editing",
            );
            return;
        }
        match intent {
            Intent::OpenHelpLink(target) => {
                context.open_url(egui::OpenUrl::new_tab(target.url()));
            }
            Intent::ScreenshotHotkey {selected}=>self.screenshot_hotkey(selected,context),
            Intent::Screenshot {session,revision,target}=>self.open_screenshot(session,revision,target,context),
            Intent::OpenNdiOutputs=>self.open_ndi_outputs(),
            Intent::NdiOutput(action)=>self.ndi_output_action(action),
            Intent::ProbeNvenc {token}=>{
                let Some(token)=token.filter(|token|Some(*token)==self.settings_flow.token()&&token.session==self.session.identity()&&token.revision==self.session.snapshot().revision())else{self.message(true,"Settings changed. Reopen the encoder page before probing.");return;};
                let valid=matches!(&self.dialog,Some(Dialog::Recording(_)|Dialog::Streaming(_)));
                if !valid{self.message(true,"Open Output or Stream settings before probing NVIDIA encoders.");return;}
                match self.io.submit(self.session.identity(),Job::ProbeNvenc {token}) {
                    Ok(())=>{match &mut self.dialog {Some(Dialog::Recording(d))=>d.encoder.probing=true,Some(Dialog::Streaming(d))=>d.encoder.probing=true,_=>{}}},
                    Err(error)=>self.message(true,error),
                }
            },
            Intent::SourceColorTag(action)=>self.source_color_action(action,context),
            Intent::ItemCompositing(request) => self.apply_item_compositing(request),
            Intent::Projector(action)=>self.projector_action(action,context),
            Intent::RepairFiles(token,action)=>self.candidate_repair_action(token,action,context),
            Intent::CancelPendingOpen=>self.cancel_pending_open(context),
            Intent::RetryPendingOpen=>self.retry_pending_document(context),
            Intent::OpenMissingFiles=>self.open_missing_files(),
            Intent::MissingFiles(token,action)=>self.missing_files_action(token,action),
            Intent::OpenRemux=>self.open_remux(),
            Intent::Remux(action)=>self.remux_action(action,context),
            Intent::OpenStatistics => self.statistics.open(),
            Intent::NativeBrowse(request) => self.begin_native_browse(request, context),
            Intent::Clipboard(request) => self.clipboard_action(request),
            Intent::ContextMixerHidden { session, revision, scene, item, source, hidden } => {
                let snapshot = self.session.snapshot();
                if self.session.is_closed() || session != self.session.identity() || revision != snapshot.revision()
                    || snapshot.root_scene_for_container(scene) != Some(self.session.preview())
                    || !snapshot.scene_container(scene).is_some_and(|container| container.items().iter().any(|entry| entry.id() == item && entry.source() == source))
                    || !snapshot.source(source).is_some_and(|entry| entry.kind().has_audio()) {
                    self.message(true, "The selected source changed. Open the menu again.");
                    return;
                }
                self.publish_command_at(Some(revision), Command::SetSourceMixerState {
                    source, action: skylive_core::MixerSourceAction::SetHidden(hidden),
                }, false);
                context.request_repaint();
            },
            Intent::ContextOrder { session, revision, scene, item, index } => {
                let snapshot = self.session.snapshot();
                if session != self.session.identity() || revision != snapshot.revision()
                    || snapshot.root_scene_for_container(scene) != Some(self.session.preview())
                    || !snapshot.scene_container(scene).is_some_and(|container| container.items().iter().any(|entry| entry.id() == item)) {
                    self.message(true, "The selected source changed. Open the menu again.");
                    return;
                }
                self.apply_at(Some(revision), Command::MoveItem { scene, item, index });
            },
            Intent::ContextTransformClipboard(request) => self.context_transform_clipboard_action(request),
            Intent::ContextTransform { session, revision, scene, item, action } => {
                let snapshot = self.session.snapshot();
                if session != self.session.identity() || revision != snapshot.revision()
                    || snapshot.root_scene_for_container(scene) != Some(self.session.preview())
                    || !snapshot.scene_container(scene).is_some_and(|container| container.items().iter().any(|entry| entry.id() == item)) {
                    self.message(true, "The selected source changed. Open the menu again.");
                    return;
                }
                self.apply_at(Some(revision), Command::TransformItem { scene, item, action });
            },
            Intent::MixerFilterClipboard(request) => self.mixer_filter_clipboard_action(request),
            Intent::SourceClipboard(request) => self.source_clipboard_action(context, request),
            Intent::ContextSourceClipboard { action, session, revision, item } => {
                if session != self.session.identity() || revision != self.session.snapshot().revision()
                    || self.session.snapshot().item_container(item).is_none() {
                    self.message(true, "The selected source changed. Open the menu again.");
                    return;
                }
                let mut selection = crate::source_clipboard::selected(context, &self.session, self.selected);
                if !selection.contains(&item) { selection = vec![item]; }
                let result = self.source_clipboard.request(action, &self.session, selection.clone())
                    .and_then(|request| self.source_clipboard.execute(request, &self.session, selection));
                match result {
                    Ok(crate::source_clipboard::Outcome::Copied) => self.message(false, "Sources copied"),
                    Ok(crate::source_clipboard::Outcome::Paste { revision, command }) => { self.publish_command_at(Some(revision), command, false); }
                    Err(error) => self.message(true, error),
                }
            }
            Intent::ContextSourcePaste { action, session, revision, scene } => {
                if session != self.session.identity() || revision != self.session.snapshot().revision()
                    || scene != self.session.preview() || action == crate::source_clipboard::Action::Copy {
                    self.message(true, "The scene or source clipboard changed. Open the menu again.");
                    return;
                }
                let result = self.source_clipboard.request(action, &self.session, Vec::new())
                    .and_then(|request| self.source_clipboard.execute(request, &self.session, Vec::new()));
                match result {
                    Ok(crate::source_clipboard::Outcome::Paste { revision, command }) => { self.publish_command_at(Some(revision), command, false); }
                    Ok(crate::source_clipboard::Outcome::Copied) => {}
                    Err(error) => self.message(true, error),
                }
            }
            Intent::ToggleSourceToolbar => views::source_toolbar::toggle_source_bar(context),
            Intent::ToggleListboxToolbars => views::source_toolbar::toggle_listbox_bars(context),
            Intent::ToggleStatusBar => { self.status_visible = !self.status_visible; }
            Intent::TogglePreviewLock => {
                self.preview_locked = !self.preview_locked;
                self.preview_interaction.cancel();
            }
            Intent::PreviewDisplay(action) => crate::preview_display::apply(context, action),
            Intent::SceneGrid(grid) => context.data_mut(|data| data.insert_temp(egui::Id::new("scene-grid"), grid)),
            Intent::ToggleSourceIcons => context.data_mut(|data| {
                let visible = data.get_temp::<bool>(egui::Id::new("source-type-icons")).unwrap_or(true);
                data.insert_temp(egui::Id::new("source-type-icons"), !visible);
            }),
            Intent::ToggleFullscreen => {
                let fullscreen = context.input(|input| input.viewport().fullscreen.unwrap_or(false));
                context.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }
            Intent::ToggleAlwaysOnTop => {
                self.always_on_top = !self.always_on_top;
                context.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if self.always_on_top {
                    egui::WindowLevel::AlwaysOnTop
                } else { egui::WindowLevel::Normal }));
            }
            Intent::ShowRecordingsFolder => {
                let model = self.session.snapshot();
                let directory = model.profiles().selected().recording().directory();
                let path = if directory.is_empty() { skylive_host::recording::default_directory() } else { PathBuf::from(directory) };
                if let Err(error) = self.io.submit(self.session.identity(), Job::OpenDirectory(path)) { self.message(true, error); }
            }
            Intent::ShowSettingsFolder => {
                match skylive_host::user_preferences::default_path() {
                    Ok(path) => {
                        if let Some(folder) = path.parent() {
                            if let Err(error) = self.io.submit(self.session.identity(), Job::OpenDirectory(folder.to_owned())) { self.message(true, error); }
                        }
                    }
                    Err(error) => self.message(true, error.to_string()),
                }
            }
            Intent::InstallVirtualCamera => {
                if self.virtual_camera.owned() {
                    self.message(true, "Stop the virtual camera before installing its component.");
                    return;
                }
                let owner=self.native_browse.owner.unwrap_or_default();
                if let Err(error)=self.io.submit(self.session.identity(),Job::InstallVirtualCamera {owner}) {self.message(true,error);}
                else {self.message(false,"Installing virtual camera…");}
            }
            Intent::ImportProfile(path) => {
                if self.recording.active() || self.streaming.active() || self.virtual_camera.owned() {
                    self.message(true, "Stop active outputs before importing a profile");
                    return;
                }
                let revision = self.session.snapshot().revision();
                if let Err(error) = self.io.submit(self.session.identity(), Job::LoadProfile { path: PathBuf::from(path), revision }) { self.message(true, error); }
            }
            Intent::ExportProfile(path) => {
                let profile = self.session.snapshot().profiles().selected().clone();
                if let Err(error) = self.io.submit(self.session.identity(), Job::ExportProfile { path: PathBuf::from(path), profile }) { self.message(true, error); }
            }
            Intent::OpenAdvancedAudio => self.open_advanced_audio(context),
            Intent::MixerEdit(edit) => self.apply_mixer_edit(edit, context),
            Intent::OpenGlobalAudioDevices => self.open_global_audio_devices(context),
            Intent::LiveGlobalAudio(edit) => self.live_global_audio(edit, context),
            Intent::LiveAudioMonitoring {session,revision,source,mode}=>self.live_audio_monitoring(session,revision,source,mode,context),
            Intent::EnumerateMonitorDevices {session,revision}=>self.enumerate_monitor_devices(session,revision),
            Intent::SetMonitoringDevice {session,revision,device}=>self.set_monitoring_device(session,revision,device,context),
            Intent::LiveAudio(edit) => self.live_audio_edit(edit, context),
            Intent::LiveAudioProperties(edit) => self.live_audio_properties(edit, context),
            Intent::GlobalHotkey(_)=>{},
            Intent::OpenSourceOutputResize{session,revision,scene,item}=>self.open_source_output_resize(session,revision,scene,item,context),
            Intent::ApplySourceOutputResize{token}=>self.apply_source_output_resize(token,context),
            Intent::InteractBrowser {session,revision,source}=>{
                if session!=self.session.identity()||revision!=self.session.snapshot().revision(){self.message(true,"The Browser Source changed. Select it again.");}
                else if let Err(error)=self.browser_interaction.open(&self.session,source,context){self.message(true,error);}
                context.request_repaint();
            },
            Intent::CommitBrowser(request)=>self.commit_browser(request,context),
            Intent::RefreshBrowser {token,session,revision,source}=>self.refresh_browser(token,session,revision,source,context),
            Intent::DiscoverGames {token,session,revision}=>self.discover_games(token,session,revision,context),
            Intent::ApplyGameCapture(request)=>self.apply_game(request),
            Intent::StartGameCapture {session,revision,source}=>self.game_control(session,revision,source,true),
            Intent::StopGameCapture {session,revision,source}=>self.game_control(session,revision,source,false),
            Intent::PrepareNdi(request)=>self.prepare_ndi(request,context),
            Intent::DiscoverNdi {token,session,revision,local}=>self.discover_ndi(token,session,revision,local,context),
            Intent::PrepareMedia(request) => self.prepare_media(request, context),
            Intent::ApplySourceAudioHotkeys{session,revision,source,token,bindings,push}=>self.apply_source_audio_hotkeys(session,revision,source,token,bindings,push,context),
            Intent::ApplySourcePlaybackHotkeys{session,revision,source,token,bindings}=>self.apply_source_playback_hotkeys(session,revision,source,token,bindings,context),
            Intent::ControlSlideshow {target,operation}=>{
                let result=if let Some(operation)=operation{self.playback.control_slideshow(&self.session,target,operation,self.frame_now).map(|receipt|(receipt.status,receipt.transition_busy,receipt.discarded_elapsed))}else{self.playback.slideshow_status(&self.session,target,self.frame_now).map(|status|(status,false,false))};
                match result{Ok((status,busy,discarded))=>{let text=crate::i18n::tf_context(context,"Slide {index} of {total}: {state}; manual: {manual}",&[("{index}",(status.current_index+1).to_string()),("{total}",status.total.to_string()),("{state}",crate::i18n::tr_context(context,status.state.label())),("{manual}",crate::i18n::tr_context(context,if status.manual{"Yes"}else{"No"}))]);let text=if let Some(seed)=status.random_seed{crate::i18n::tf_context(context,"Random run {seed}, generation {generation}, draws {draws}. {status}",&[("{seed}",seed.to_string()),("{generation}",status.random_generation.unwrap_or(0).to_string()),("{draws}",status.random_draws.unwrap_or(0).to_string()),("{status}",text)])}else{text};self.message(false,if discarded{crate::i18n::tf_context(context,"Excessive random catch-up was discarded by the explicit transport reset. {status}",&[("{status}",text)])}else if busy{crate::i18n::tf_context(context,"Transition still running; slide change ignored. {status}",&[("{status}",text)])}else{text});},Err(error)=>self.message(true,crate::i18n::tr_context(context,error.to_string()))}context.request_repaint();
            },
            Intent::ControlMedia { target, operation } => {
                self.control_media(target, operation, context)
            }
            Intent::DiscoverTextSystemFonts(key)=>self.discover_text_system_fonts(key),
            Intent::PrepareTextFont(key)=>self.prepare_text_font(key),
            Intent::ApplyTextSource(key)=>self.apply_text_source(key),
            Intent::PrepareLut(key)=>self.prepare_lut(key),
            Intent::PrepareMask(key)=>self.prepare_mask(key),
            Intent::ApplySceneVideoFilters {
                token,session,
                revision,
                scene,
                filters,
            } => {
                if !self.video_filter_current(token,session,revision,views::video_filters::VideoFilterTarget::Scene(scene),&filters) {
                    self.message(true, "Video filter draft changed; reopen the editor.");
                    return;
                }
                self.apply_at(
                    Some(revision),
                    Command::SetSceneVideoFilters { scene, filters },
                );
                context.request_repaint();
            }
            Intent::ApplyVideoFilters {
                token,session,
                revision,
                source,
                filters,
            } => {
                if !self.video_filter_current(token,session,revision,views::video_filters::VideoFilterTarget::Source(source),&filters) {
                    self.message(true, "Video filter draft changed; reopen the editor.");
                    return;
                }
                self.apply_at(
                    Some(revision),
                    Command::SetSourceVideoFilters { source, filters },
                );
                context.request_repaint();
            }
            Intent::Reorder {
                session,
                revision,
                command,
            } => {
                if session != self.session.identity() {
                    self.message(true, "Row drag belongs to another document");
                    return;
                }
                self.apply_at(Some(revision), command);
                context.request_repaint();
            }
            Intent::ApplyColorSource(submission) => {
                let revision = submission.revision();
                match submission.into_command(&self.session) {
                    Ok(command) => self.apply_at(Some(revision), command),
                    Err(error) => self.message(true, error),
                }
                context.request_repaint();
            }
            Intent::ApplyApplicationAudioBinding {token,session,revision,command} => {
                if session!=self.session.identity() || !matches!(&self.dialog,Some(Dialog::AudioCapture {target:dialogs::AudioTargetChoice::WindowsProcess,device_picker:Some(picker),..}) if picker.token==token && picker.session==session && picker.revision==revision) {
                    self.message(true,"Application audio draft is obsolete. Reopen source properties.");return;
                }
                self.apply_at(Some(revision),command);context.request_repaint();
            }
            Intent::ApplyAudioBinding {
                session,
                revision,
                command,
            } => {
                if session != self.session.identity() {
                    self.message(true, "Audio binding draft belongs to another document");
                    return;
                }
                self.apply_at(Some(revision), command);
                context.request_repaint();
            }
            Intent::RefreshCameraAudioDevices {token} => self.refresh_camera_audio_devices(token,context),
            Intent::EnumerateAudioDevices => {
                if let Some(Dialog::AudioCapture {
                    device_picker: Some(picker),
                    ..
                }) = &mut self.dialog
                {
                    if picker.session != self.session.identity()
                        || picker.revision != self.session.snapshot().revision()
                    {
                        picker.loading = false;
                        picker.error=Some("The document changed. Reopen this audio dialog before enumerating devices.".into());
                    } else {
                        picker.loading = true;
                        picker.error = None;
                        context.request_repaint();
                    }
                }
            }
            Intent::ApplyCaptureTransform {
                scene,
                item,
                transform,
                context: transform_context,
            } => {
                match capture::prepare_capture_transform(
                    &self.session,
                    &self.source_capture_snapshot(),
                    scene,
                    item,
                    transform,
                    &transform_context,
                ) {
                    Ok(command) => self.apply(command),
                    Err(error) => self.message(true, error.to_string()),
                }
                context.request_repaint();
            }
            Intent::OpenCameraCapture {
                edit,
                name,
                binding,
            } => {
                let Some(token) = self.camera_dialog_sequence.checked_add(1) else {
                    self.message(true, "Camera picker identity exhausted");
                    return;
                };
                self.camera_dialog_sequence = token;
                self.dialog = Some(Dialog::CameraCapture(
                    views::camera_capture::CameraDraft::new(
                        token,
                        self.session.identity(),
                        self.session.snapshot().revision(),
                        edit,
                        name,
                        binding,
                        self.camera.capabilities(),
                    )
                    .on_platform(self.platform),
                ));
                context.request_repaint();
            }
            Intent::RefreshCameras => {
                if let Some(Dialog::CameraCapture(draft)) = &mut self.dialog {
                    if draft.session == self.session.identity()
                        && draft.revision == self.session.snapshot().revision()
                    {
                        draft.loading_devices = true;
                        draft.error = None;
                    } else {
                        draft.error = Some(
                            "The document changed. Reopen this dialog to bind a camera.".into(),
                        );
                    }
                }
                context.request_repaint();
            }
            Intent::QueryCameraControls {token,source} => {
                let valid=matches!(&self.dialog,Some(Dialog::CameraCapture(d)) if d.token==token && d.session==self.session.identity() && d.revision==self.session.snapshot().revision() && matches!(d.edit,views::CameraEdit::Rebind {source:s} if s==source));
                if !valid {self.message(true,"Camera properties belong to an obsolete document or dialog");return;}
                match self.camera.request_controls(&self.session,source) {
                    Ok(key)=>{
                        self.pending_camera_controls=Some((token,key));
                        if let Some(Dialog::CameraCapture(d))=&mut self.dialog {d.driver.loading=true;d.driver.message=None;}
                    },
                    Err(error)=>{if let Some(Dialog::CameraCapture(d))=&mut self.dialog {d.driver.loading=false;d.driver.message=Some(error.to_string());}},
                }
                context.request_repaint();
            }
            Intent::ApplyCameraControl {token,lease,change} => {
                if self.transition_active {self.message(true,"Wait for the scene transition to finish before writing camera controls");return;}
                let valid=matches!(&self.dialog,Some(Dialog::CameraCapture(d)) if d.token==token && d.session==self.session.identity() && d.revision==self.session.snapshot().revision() && d.selected.as_ref()==Some(lease.binding.target()) && d.selected_mode==Some(lease.binding.mode()) && matches!(d.edit,views::CameraEdit::Rebind {source} if source==lease.source));
                if !valid {self.message(true,"Camera properties changed; driver was not written");return;}
                match self.camera.request_control_change(&self.session,lease,change) {
                    Ok(key)=>{
                        self.pending_camera_controls=Some((token,key));
                        if let Some(Dialog::CameraCapture(d))=&mut self.dialog {d.driver.loading=true;d.driver.message=None;}
                    },
                    Err(error)=>{if let Some(Dialog::CameraCapture(d))=&mut self.dialog {d.driver.loading=false;d.driver.message=Some(error.to_string());}},
                }
                context.request_repaint();
            }
            Intent::PrepareSlideShow(submission)=>self.prepare_slideshow(submission),
            Intent::QueryCameraModes => {
                if let Some(Dialog::CameraCapture(draft)) = &mut self.dialog {
                    if draft.session == self.session.identity()
                        && draft.revision == self.session.snapshot().revision()
                    {
                        draft.query_modes = true;
                        draft.error = None;
                    } else {
                        draft.error = Some(
                            "The document changed. Reopen this dialog to bind a camera.".into(),
                        );
                    }
                }
                context.request_repaint();
            }
            Intent::SetCameraSourceEnabled {token,session,revision,source,binding,enabled} => {
                let valid=session==self.session.identity() && revision==self.session.snapshot().revision()
                    && matches!(&self.dialog,Some(Dialog::CameraCapture(draft)) if draft.token==token && draft.session==session && draft.revision==revision
                        && matches!(draft.edit,views::CameraEdit::Rebind {source:current} if current==source)
                        && (!enabled || (draft.selected.as_ref()==Some(binding.target()) && draft.selected_mode==Some(binding.mode()) && draft.deactivate_when_hidden==binding.deactivate_when_hidden() && draft.flip_vertically==binding.flip_vertically() && draft.audio.embedded==binding.embedded_audio() && draft.audio.binding().is_ok_and(|audio|audio.as_ref()==binding.audio()))))
                    && self.session.snapshot().source(source).is_some_and(|item|matches!(item.kind(),skylive_core::SourceKind::VideoCapture {binding:current} if current==&binding));
                if !valid {self.message(true,"Camera source action is obsolete. Reopen properties.");return;}
                self.request_camera_av(Some(source),enabled,context);
            }
            Intent::StopCameras => self.request_camera_av(None,false,context),
            Intent::CommitCamera {
                token,
                session,
                revision,
                edit,
                name,
                binding,
            } => {
                if session != self.session.identity() || !matches!(&self.dialog,Some(Dialog::CameraCapture(draft)) if draft.token==token && draft.session==session && draft.revision==revision
                    && matches!(draft.intent(),Ok(Intent::CommitCamera {binding:current,name:current_name,edit:current_edit,..}) if current==binding && current_name==name && current_edit==edit)) {
                    self.message(true, "Camera draft belongs to another document or dialog");
                    return;
                }
                let rebind = match &edit {
                    views::CameraEdit::Rebind { source } => Some(*source),
                    _ => None,
                };
                let source_name = name.clone();
                let expected_binding = binding.clone();
                let video_changed = rebind.is_none_or(|source| !self.session.snapshot().source(source)
                    .is_some_and(|source|matches!(source.kind(),skylive_core::SourceKind::VideoCapture {binding:old} if old.same_device_mode(&expected_binding))));
                let command = match edit {
                    views::CameraEdit::Add { scene } => Command::AddVideoCaptureSource {
                        scene,
                        name,
                        binding,
                    },
                    views::CameraEdit::Rebind { source } => {
                        Command::RebindVideoCapture { source, binding }
                    }
                };
                self.apply_at(Some(revision), command);
                if !self.error {
                    let current = self.session.snapshot();
                    let source=rebind.or_else(||current.sources().iter().find(|s|s.name()==source_name&&matches!(s.kind(),skylive_core::SourceKind::VideoCapture{binding}if binding==&expected_binding)).map(|s|s.id()));
                    if let Some(source) = source.filter(|_|video_changed) {
                        self.camera_start = Some(PendingCameraStart {
                            session,
                            source,
                            binding: expected_binding,
                            restart: false,
                            enabled:true,
                            dialog:None,
                        });
                    }
                }
                context.request_repaint();
            }
            Intent::OpenWindowCapture {
                edit,
                name,
                binding,
            } => {
                let Some(token) = self.window_dialog_sequence.checked_add(1) else {
                    self.message(true, "Window picker identity exhausted");
                    return;
                };
                self.window_dialog_sequence = token;
                self.dialog = Some(Dialog::WindowCapture(
                    views::window_capture::WindowDraft::new(
                        token,
                        self.session.identity(),
                        self.session.snapshot().revision(),
                        edit,
                        name,
                        binding,
                        self.capture.window_capabilities(),
                    )
                    .on_platform(self.platform),
                ));
                context.request_repaint();
            }
            Intent::RefreshWindows => {
                if let Some(Dialog::WindowCapture(draft)) = &mut self.dialog {
                    if draft.session != self.session.identity()
                        || draft.revision != self.session.snapshot().revision()
                    {
                        draft.loading = false;
                        draft.error = Some(
                            "The document changed. Reopen this dialog to bind a window.".into(),
                        );
                    } else {
                        draft.loading = true;
                        draft.error = None;
                        context.request_repaint();
                    }
                }
            }
            Intent::CommitWindow {
                session,
                revision,
                edit,
                name,
                binding,
            } => {
                if session != self.session.identity() {
                    self.message(true, "Window draft belongs to another document");
                    return;
                }
                let command = match edit {
                    views::WindowEdit::Add { scene } => Command::AddWindowCaptureSource {
                        scene,
                        name,
                        binding,
                    },
                    views::WindowEdit::Rebind { source } => {
                        Command::SetWindowCaptureSource { source, binding }
                    }
                };
                self.apply_at(Some(revision), command);
                context.request_repaint();
            }
            Intent::OpenDisplayCapture {
                edit,
                name,
                binding,
            } => {
                let Some(token) = self.display_dialog_sequence.checked_add(1) else {
                    self.message(true, "Display picker identity exhausted");
                    return;
                };
                self.display_dialog_sequence = token;
                let capabilities = self.capture.capabilities();
                self.dialog = Some(Dialog::DisplayCapture {
                    platform: self.platform,
                    method: binding
                        .as_ref()
                        .map_or(skylive_core::DisplayCaptureMethod::Auto, |binding| {
                            binding.method()
                        }),
                    token,
                    session: self.session.identity(),
                    revision: self.session.snapshot().revision(),
                    edit,
                    name,
                    selected: binding.as_ref().map(|binding| binding.target().clone()),
                    capture_cursor: binding
                        .as_ref()
                        .map_or(capabilities.cursor, |binding| binding.capture_cursor()),
                    displays: Vec::new(),
                    capabilities,
                    loading: true,
                    error: None,
                });
                context.request_repaint();
            }
            Intent::RefreshDisplays => {
                if let Some(Dialog::DisplayCapture {
                    session,
                    revision,
                    loading,
                    error,
                    ..
                }) = &mut self.dialog
                {
                    if *session != self.session.identity()
                        || *revision != self.session.snapshot().revision()
                    {
                        *loading = false;
                        *error = Some(
                            "The document changed. Reopen this dialog to bind a display.".into(),
                        );
                    } else {
                        *loading = true;
                        *error = None;
                        context.request_repaint();
                    }
                }
            }
            Intent::CommitDisplay {
                session,
                revision,
                edit,
                name,
                binding,
            } => {
                if session != self.session.identity() {
                    self.message(true, "Display draft belongs to another document");
                    return;
                }
                let command = match edit {
                    DisplayEdit::Add { scene } => Command::AddDisplayCaptureSource {
                        scene,
                        name,
                        binding,
                    },
                    DisplayEdit::Rebind { source } => {
                        Command::RebindDisplayCapture { source, binding }
                    }
                };
                self.apply_at(Some(revision), command);
                context.request_repaint();
            }
            Intent::StartVirtualCamera => self.start_virtual_camera(context),
            Intent::StopVirtualCamera => self.stop_virtual_camera(),
            Intent::OpenVirtualCameraSettings => self.open_virtual_camera_settings(),
            Intent::ApplyVirtualCameraSettings(submission) => self.apply_virtual_camera_settings(submission),
            Intent::CloseVirtualCameraSettings(token) => self.close_virtual_camera_settings(token),
            Intent::StartStreaming => self.start_streaming(),
            Intent::StopStreaming => self.stop_streaming(),
            Intent::StartReplayBuffer=>self.start_replay(),
            Intent::StopReplayBuffer=>self.stop_replay(),
            Intent::SaveReplayBuffer=>self.save_replay(),
            Intent::CancelReplaySave=>self.replay.owner.cancel_save(),
            Intent::StartRecording => self.start_recording(),
            Intent::StopRecording => self.stop_recording(),
            Intent::SetRecordingPaused {recording,session,paused}=>self.set_recording_paused(recording,session,paused),
            Intent::Command(command@Command::ConfigureAdvancedSettings{..})=>self.request_advanced_command(command,None,context),
            Intent::Command(Command::SetProcessPriority{priority})=>self.request_process_priority(priority,None,false,context),
            Intent::Command(command) => self.apply(command),
            Intent::OpenPluginEditor(request) => self.open_plugin_editor(request, context),
            Intent::OpenPluginSource(request)=>self.open_plugin_source(request,context),
            Intent::ApplyPluginEditor { token, command } => {
                self.apply_plugin_editor(token, command, context)
            }
            Intent::SetPluginEnabled { plugin, enabled } => {
                let dialog = self.dialog.take();
                self.apply(Command::SetPluginEnabled { plugin, enabled });
                self.dialog = dialog;
                context.request_repaint();
            }
            Intent::DialogError(error) => self.message(true, error),
            Intent::Quick(id) => match self.transition.quick(&mut self.session, id, self.frame_now)
            {
                Ok(()) => {
                    self.message(false, "Quick transition applied");
                    let _ = self.sync_transition();
                    context.request_repaint();
                }
                Err(error) => self.message(true, error.to_string()),
            },
            Intent::ManualPosition(value) => {
                match self
                    .transition
                    .manual_position(&mut self.session, value, self.frame_now)
                {
                    Ok(()) => {
                        self.message(false, "Manual transition position updated");
                        let _ = self.sync_transition();
                        context.request_repaint();
                    }
                    Err(error) => self.message(true, error.to_string()),
                }
            }
            Intent::ManualRelease => match self
                .transition
                .manual_release(&mut self.session, self.frame_now)
            {
                Ok(()) => {
                    self.message(false, "T-bar released");
                    let _ = self.sync_transition();
                    context.request_repaint();
                }
                Err(error) => self.message(true, error.to_string()),
            },
            Intent::SelectScene(scene) => {
                match self
                    .transition
                    .select_preview(&mut self.session, scene, self.frame_now)
                {
                    Ok(_) => {
                        self.selected = None;
                        self.message(false, "Preview selected");
                        let _ = self.sync_transition();
                        context.request_repaint();
                    }
                    Err(error) => self.message(true, error.to_string()),
                }
            }
            Intent::SelectItem(item) => self.selected = item,
            Intent::Cut => match self.transition.cut(&mut self.session, self.frame_now) {
                Ok(_) => {
                    self.message(false, "Cut applied");
                    let _ = self.sync_transition();
                    context.request_repaint();
                }
                Err(error) => self.message(true, error.to_string()),
            },
            Intent::Transition => match self
                .transition
                .transition(&mut self.session, self.frame_now)
            {
                Ok(_) => {
                    self.message(false, "Transition applied");
                    let _ = self.sync_transition();
                    context.request_repaint();
                }
                Err(error) => self.message(true, error.to_string()),
            },
            Intent::SubmitSettings {
                page,
                action,
                token,
            } => self.submit_settings(page, action, token, context),
            Intent::ApplyLocalization { language } => {
                self.request_localization(context, language, true)
            }
            Intent::OpenDialog(mut dialog) => {
                if let Dialog::SourceAudioHotkeys(draft)=&dialog{
                    let model=self.session.snapshot();
                    if !draft.fresh(self.session.identity(),&model){self.message(true,"This source shortcut draft is obsolete. Cancel and reopen it.");return;}
                    if self.dialog.as_ref().is_some_and(|dialog|!matches!(dialog,Dialog::Hotkeys(draft)if !draft.is_listening()&&draft.unchanged_preferences(model.preferences()))){self.message(true,"Apply or Cancel the current draft before opening source mute shortcuts.");return;}
                }
                if let Dialog::SourcePlaybackHotkeys(draft)=&dialog{
                    let model=self.session.snapshot();
                    if !draft.fresh(self.session.identity(),&model){self.message(true,"This source shortcut draft is obsolete. Cancel and reopen it.");return;}
                    if self.dialog.as_ref().is_some_and(|dialog|!matches!(dialog,Dialog::Hotkeys(draft)if !draft.is_listening()&&draft.unchanged_preferences(model.preferences()))){self.message(true,"Apply or Cancel the current draft before opening source playback shortcuts.");return;}
                }
                if !self.next_browse_epoch() { return; }
                if let Err(error) = self.bind_plugin_editor(&mut dialog) {
                    self.message(true, error);
                    return;
                }
                if let Dialog::Text(draft)=&mut dialog{if let Err(error)=self.text_font.open(draft,&self.session){self.message(true,error);return;}}
                if let Dialog::VideoFilters(draft)=&mut dialog {
                    let Some(token)=self.video_filter_dialog_sequence.checked_add(1)else{self.message(true,"Video filter editor identity exhausted");return;};
                    self.video_filter_dialog_sequence=token;draft.token=token;
                }
                if let Dialog::Plugins { token, session, .. } = &mut dialog {
                    let Some(next) = self.plugin_dialog_sequence.checked_add(1) else {
                        self.message(true, "Plugin manager identity exhausted");
                        return;
                    };
                    self.plugin_dialog_sequence = next;
                    *token = next;
                    *session = self.session.identity();
                }
                if let Dialog::ImageSlideShow(draft)=&mut dialog {
                    let Some(token)=self.slideshow_dialog_sequence.checked_add(1) else {self.message(true,"Slideshow dialog identity exhausted");return;};
                    if let Err(error)=draft.bind(&self.session,token){self.message(true,error);return;}
                    self.slideshow_dialog_sequence=token;
                }
                if let Dialog::ColorSource(draft) = &mut dialog {
                    if let Err(error) = draft.bind(&self.session) {
                        self.message(true, error);
                        return;
                    }
                }
                if let Dialog::Streaming(draft) = &mut dialog {
                    if let Err(error) = draft.bind(&self.session) {
                        self.message(true, error);
                        return;
                    }
                }
                if let Dialog::GameCapture(draft)=&mut dialog {
                    if !skylive_host::game_capture::GameOwner::available(){self.message(true,"Game Capture requires Windows x64 D3D11.");return;}
                    let Some(token)=self.game_dialog_sequence.checked_add(1)else{self.message(true,"Game dialog identity exhausted");return;};
                    self.game_dialog_sequence=token;draft.token=token;draft.session=self.session.identity();draft.revision=self.session.snapshot().revision();
                    match self.game.targets(&self.session){Ok(key)=>{self.pending_game=Some((token,key));draft.loading=true;},Err(error)=>draft.error=Some(error.to_string())}
                }
                if let Dialog::NdiSource(draft)=&mut dialog {
                    if !skylive_host::ndi::NdiOwner::available()||!self.session.snapshot().preferences().plugin_enabled(skylive_core::NDI_PLUGIN){self.message(true,"Enable the NDI receive module on Windows x64 first.");return;}
                    let Some(token)=self.ndi_dialog_sequence.checked_add(1)else{self.message(true,"NDI dialog identity exhausted");return;};self.ndi_dialog_sequence=token;draft.token=token;draft.session=self.session.identity();draft.revision=self.session.snapshot().revision();
                }
                if let Dialog::BrowserSource(draft)=&mut dialog {let Some(token)=self.browser_dialog_sequence.checked_add(1)else{self.message(true,"Browser dialog identity exhausted");return;};self.browser_dialog_sequence=token;draft.token=token;draft.session=self.session.identity();draft.revision=self.session.snapshot().revision();}
                if let Dialog::MediaSource(draft) = &mut dialog {
                    let Some(token) = self.media_dialog_sequence.checked_add(1) else {
                        self.message(true, "Media dialog identity exhausted");
                        return;
                    };
                    self.media_dialog_sequence = token;
                    draft.token = token;
                    draft.session = self.session.identity();
                    draft.revision = self.session.snapshot().revision();
                }
                if let Dialog::AudioCapture {
                    edit,
                    target,
                    flow,
                    use_device_timing,
                    exclude_own_audio,
                    device_picker,
                    ..
                } = &mut dialog
                {
                    if !matches!(edit, views::AudioEdit::Rebind { .. }) {
                        let defaults = self.audio.binding_defaults();
                        *use_device_timing = defaults.device_timing(*flow);
                        *exclude_own_audio = defaults.exclude_own_audio(*flow);
                    }
                    if *target==dialogs::AudioTargetChoice::WindowsProcess {*exclude_own_audio=false;}
                    let Some(token) = self.audio_dialog_sequence.checked_add(1) else {
                        self.message(true, "Audio picker identity exhausted");
                        return;
                    };
                    self.audio_dialog_sequence = token;
                    *device_picker = Some(dialogs::AudioDevicePicker {
                        platform: self.platform,
                        token,
                        session: self.session.identity(),
                        revision: self.session.snapshot().revision(),
                        capabilities: self.audio.capabilities(),
                        devices: Vec::new(),
                        loading: self.audio.capabilities().device_enumeration,
                        error: None,
                    });
                }
                if let Dialog::Transform {
                    scene,
                    item,
                    draft,
                    capture_context,
                } = &mut dialog
                {
                    let model = self.session.snapshot();
                    if model
                        .scene_container(*scene)
                        .and_then(|scene| {
                            scene
                                .items()
                                .iter()
                                .find(|candidate| candidate.id() == *item)
                        })
                        .and_then(|item| model.source(item.source()))
                        .is_some_and(|source| {
                            model
                                .source_video_dimensions(source.kind())
                                .is_ok_and(|value| value.is_none())
                        })
                    {
                        self.message(true, "Audio-only sources have no video transform");
                        return;
                    }
                    match capture::project_capture_transform(
                        &self.session,
                        &self.source_capture_snapshot(),
                        *scene,
                        *item,
                    ) {
                        Ok(Some(projected)) => {
                            *draft = projected.transform;
                            *capture_context = Some(projected.context);
                        }
                        Ok(None) => *capture_context = None,
                        Err(error) => {
                            self.message(true, error.to_string());
                            return;
                        }
                    }
                }
                if let Err(error) = self.settings_flow.opened(&self.session, &dialog) {
                    self.message(true, error);
                    return;
                }
                self.dialog = Some(dialog);
                self.invalidate_shortcut_callbacks();
            }
            Intent::ApplyStreamingSettings(request) => {
                let revision = request.revision;
                match request.command_for(&self.session) {
                    Ok(command) => self.apply_at(Some(revision), command),
                    Err(error) => self.message(true, error),
                }
            }
            Intent::Save => self.save(None),
            Intent::SaveAs => self.open_save_as(None),
            Intent::SaveAsTo { path } => self.confirmed_save_as(path),
            Intent::ResolveDocument(choice) => self.resolve_document(choice, context),
            Intent::CancelDocumentSave => self.cancel_document_save(),
            Intent::PrepareImage { path, edit } => {
                if path.trim().is_empty() {
                    self.message(true, "Enter an image file path");
                    return;
                }
                let Some(settings)=self.dialog.as_ref().and_then(|dialog|dialog.image_settings()
                    .filter(|settings|dialog.matches_image(&edit,&path,*settings)))else{
                    self.message(true,"The image draft changed. Reopen its properties before preparing.");return;
                };
                let pending = PendingImage {
                    path:path.clone(),settings,dialog_epoch:self.native_browse.epoch(),
                    session: self.session.identity(),
                    revision: self.session.snapshot().revision(),
                    edit,
                };
                match self.io.submit(
                    pending.session,
                    Job::PrepareImage {
                        path: PathBuf::from(path),
                    },
                ) {
                    Ok(()) => {
                        self.pending_image = Some(pending);
                        self.message(false, "Reading and validating image…");
                    }
                    Err(error) => self.message(true, error),
                }
            }
            Intent::CollectionLibrary(action)=>self.library_action(action,context),
            Intent::NewCollection => self.request_replacement(document::Replacement::New, context),
            Intent::OpenDocumentPicker(action) => self.open_document_picker(action),
            Intent::ConfirmDocumentPath(draft) => self.confirm_document_path(draft, context),
            Intent::CancelDocumentPath => self.cancel_document_picker(),
            Intent::OpenObsImport => {
                self.open_document_picker(views::document_picker::DocumentAction::ImportObs)
            }
            Intent::OpenObsExport => {
                let import = false;
                let collection = self.session.snapshot();
                let video = collection.video();
                self.dialog = Some(Dialog::ObsPath {
                    import,
                    path: self.obs_path.clone(),
                    canvas: format!(
                        "{} × {}, {}/{} FPS",
                        video.width(),
                        video.height(),
                        video.fps_num(),
                        video.fps_den()
                    ),
                });
            }
            Intent::ImportObs { path } => {
                if path.trim().is_empty() {
                    self.message(true, "Enter an OBS collection path");
                    return;
                }
                let collection = self.session.snapshot();
                let video = collection.video();
                let profile = ImportVideo {
                    width: video.width(),
                    height: video.height(),
                    fps_num: video.fps_num(),
                    fps_den: video.fps_den(),
                };
                self.obs_path = path.clone();
                self.request_replacement(
                    document::Replacement::Import {
                        path: PathBuf::from(path),
                        video: profile,
                    },
                    context,
                );
            }
            Intent::ExportObs { path } => {
                if path.trim().is_empty() {
                    self.message(true, "Enter an OBS collection path");
                    return;
                }
                self.obs_path = path.clone();
                let job = ExportJob::new(&self.session, self.obs_identities.clone());
                self.start_io(
                    Job::ExportObs {
                        path: PathBuf::from(path),
                        job,
                    },
                    None,
                );
            }
            Intent::Load | Intent::Recover => match self.path() {
                Ok(path) => self.request_replacement(
                    document::Replacement::Load {
                        path,
                        recover: matches!(intent, Intent::Recover),
                    },
                    context,
                ),
                Err(error) => self.message(true, error),
            },
            Intent::Exit => {self.virtual_camera.settings=None;self.request_exit(context);}
            Intent::StudioMode(value) => {
                match self
                    .transition
                    .set_studio(&mut self.session, value, self.frame_now)
                {
                    Ok(_) => {
                        self.message(
                            false,
                            if value {
                                "Studio Mode enabled"
                            } else {
                                "Studio Mode disabled"
                            },
                        );
                        let _ = self.sync_transition();
                        context.request_repaint();
                    }
                    Err(error) => self.message(true, error.to_string()),
                }
            }
            Intent::ApplyNotesDock(submission) => self.apply_notes_dock(submission, context),
            Intent::CancelNotesDock(token) => self.cancel_notes_dock(token, context),
            Intent::ToggleDock(dock) => self.layout.toggle(dock),
            Intent::OpenSetup => self.open_setup(views::setup::SetupOrigin::Tools),
            Intent::SetupAction(token, action) => self.setup_action(token, action),
            Intent::ResetLayout => {
                self.layout.reset();
                views::dock_layout::forget_sizes(context);
            }
            Intent::SetDockPreset(preset) => {
                self.layout.columns.cancel();
                self.layout.arrangement.preset = preset;
            }
            Intent::SetDocksLocked(locked) => {
                self.layout.columns.cancel();
                self.layout.arrangement.locked = locked;
            }
            Intent::SetFullHeightDocks(full_height) => {
                self.layout.columns.cancel();
                self.layout.arrangement.full_height = full_height;
            }
            Intent::SetDockSize(dimension, value) => {
                if !self.layout.arrangement.locked {
                    self.layout.columns.cancel();
                    self.layout.arrangement.set_size(context, dimension, value);
                }
            }
            Intent::About => self.dialog = Some(Dialog::About),
        }
    }
    fn close_prompt(&mut self, context: &egui::Context) {
        if !self.close_dialog {
            return;
        }
        let mut choice = 0;
        let response = egui::Modal::new(egui::Id::new("close-document")).show(context, |ui| {
            ui.set_width(720.0_f32.min((context.screen_rect().width()-48.0).max(120.0)));
            ui.heading(crate::i18n::tr(ui,"Close SkyLive"));
            let actions=[
                views::modal_footer::Action::new("Save and close",!self.io.busy(),"Save your changes, then close SkyLive."),
                views::modal_footer::Action::new("Keep recovery and close",!self.io.busy(),"Keep a recovery copy of your changes, then close."),
                views::modal_footer::Action::new("Discard and close",!self.io.busy(),"Close without saving your changes."),
                views::modal_footer::Action::new("Cancel",true,"Return to the application without closing."),
            ];
            let body_height=(context.screen_rect().height()-views::modal_footer::required_height(ui,&actions)-100.0).max(40.0);
            egui::ScrollArea::vertical().id_salt("close-document-body").max_height(body_height).show(ui,|ui| {
                ui.label(crate::i18n::tr(ui,if self.io.busy(){"Waiting for the active document operation. It cannot be safely cancelled."}else{"Save your edits, keep a recovery checkpoint, or discard them."}));
            });
            ui.separator();
            if let Some(index)=views::modal_footer::show(ui,&actions){choice=index+1;}
        });
        if response.should_close() {
            choice = 4;
        }
        match choice {
            1 => {
                self.close_dialog = false;
                self.failed_preferences = None;
                self.save(Some(AfterIo::SaveAndClose));
            }
            2 => {
                self.close_dialog = false;
                match self
                    .active_document_path
                    .clone()
                    .or_else(|| Some(PathBuf::from(&self.native_save_draft)))
                    .ok_or("Enter a native save destination".to_owned())
                {
                    Ok(path) => self.start_io(
                        Job::Checkpoint(SaveJob::new(&self.session, path)),
                        Some(AfterIo::CheckpointAndClose),
                    ),
                    Err(error) => self.message(true, error),
                }
            }
            3 => {
                self.close_dialog = false;
                self.saved_preferences = self.session.snapshot().preferences().clone();
                self.shutdown(context);
            }
            4 => {
                self.close_dialog = false;
                self.close_after_preferences = false;
                self.cancel_startup_close();
                self.cancel_pending_open_close();
                self.cancel_library_close();
            }
            _ => {}
        }
    }
    fn frame(&mut self, context: &egui::Context) {
        self.install_monitoring_view(context);
        self.source_creation_capabilities().install(context);
        let now = match u64::try_from(self.playback_origin.elapsed().as_nanos()) {
            Ok(now) => MediaTime(now),
            Err(_) => {
                self.message(
                    true,
                    "Monotonic playback time exceeds supported nanoseconds",
                );
                MediaTime(u64::MAX)
            }
        };
        self.frame_at(context, now);
        // The playback origin is the owner of MediaTime. Sample QPC between
        // two readings of this same clock; this correlation does not imply
        // that any particular captured frame has reached Program output.
        self.audio.observe_media_clock(&self.session, self.playback_origin);
    }
    fn update_capture(
        &mut self,
        context: &egui::Context,
        active: &[FrameScene],
        now: MediaTime,
    ) -> Arc<CaptureSnapshot> {
        if let Err(error) = self.capture.synchronize(&self.session, active) {
            self.capture_error = Some(error.to_string());
            return Arc::new(CaptureSnapshot::empty(self.session.identity()));
        }
        match self.capture.poll(&self.session, now) {
            Ok(Some(CaptureEvent::Windows { key, result })) => {
                let pending = self.pending_windows.take();
                if let Some(Dialog::WindowCapture(draft)) = &mut self.dialog {
                    draft.accept(
                        pending,
                        key,
                        self.session.identity(),
                        self.session.snapshot().revision(),
                        result,
                    );
                }
            }
            Ok(Some(CaptureEvent::Displays { key, result })) => {
                let pending = self.pending_displays.take();
                if let Some(Dialog::DisplayCapture {
                    token,
                    session,
                    revision,
                    displays,
                    selected,
                    loading,
                    error,
                    ..
                }) = &mut self.dialog
                {
                    if pending == Some((*token, key)) {
                        *loading = false;
                        if key.session != *session
                            || key.revision != *revision
                            || key.session != self.session.identity()
                            || key.revision != self.session.snapshot().revision()
                        {
                            *error=Some("The document changed while enumerating displays. Reopen this dialog to bind a display.".into());
                        } else {
                            match result {
                                Ok(found) => {
                                    let found: Vec<_> = found
                                        .into_iter()
                                        .filter(|display| self.platform.display(&display.target))
                                        .collect();
                                    if selected.is_none() {
                                        *selected = found
                                            .iter()
                                            .find(|display| display.primary)
                                            .or_else(|| found.first())
                                            .map(|display| display.target.clone());
                                    }
                                    *displays = found;
                                    *error = None;
                                }
                                Err(failure) => *error = Some(failure.to_string()),
                            }
                        }
                    }
                }
            }
            Ok(Some(CaptureEvent::Frames { .. })) => self.capture_error = None,
            Ok(None) => {}
            Err(error) => self.capture_error = Some(error.to_string()),
        }
        if let Some(Dialog::WindowCapture(draft)) = &mut self.dialog {
            if draft.session != self.session.identity()
                || draft.revision != self.session.snapshot().revision()
            {
                draft.loading = false;
                draft.error =
                    Some("The document changed. Reopen this dialog to bind a window.".into());
            }
        }
        let waiting_window = match &self.dialog {
            Some(Dialog::WindowCapture(draft)) if draft.loading => Some(draft.token),
            _ => None,
        };
        let waiting = match &self.dialog {
            Some(Dialog::DisplayCapture {
                token,
                loading: true,
                ..
            }) => Some(*token),
            _ => None,
        };
        if !self.capture.is_busy() {
            if let Some(token) = waiting {
                match self.capture.request_displays(&self.session) {
                    Ok(key) => self.pending_displays = Some((token, key)),
                    Err(failure) => {
                        if let Some(Dialog::DisplayCapture { loading, error, .. }) =
                            &mut self.dialog
                        {
                            *loading = false;
                            *error = Some(failure.to_string());
                        }
                    }
                }
            } else if let Some(token) = waiting_window {
                match self.capture.request_windows(&self.session) {
                    Ok(key) => self.pending_windows = Some((token, key)),
                    Err(failure) => {
                        if let Some(Dialog::WindowCapture(draft)) = &mut self.dialog {
                            draft.loading = false;
                            draft.error = Some(failure.to_string());
                        }
                    }
                }
            } else if let Err(error) = self.capture.request_frame(&self.session, now) {
                self.capture_error = Some(error.to_string());
            }
        }
        if self.capture.is_busy() {
            context.request_repaint_after(std::time::Duration::from_millis(10));
        } else if let Some(after) = self.capture.next_after_ns(now) {
            context.request_repaint_after(std::time::Duration::from_nanos(after.max(1_000_000)));
        }
        self.update_camera(context, active, now, self.capture.snapshot())
    }
    fn accept_camera_stop(&mut self, key: CameraRequestKey) {
        self.camera_error = None;
        if key.session == self.session.identity() && self.explicit_camera_stop == Some(key) {
            self.explicit_camera_stop = None;
            self.message(false, "Camera streams stopped");
        }
    }
    fn update_camera(
        &mut self,
        context: &egui::Context,
        active: &[FrameScene],
        now: MediaTime,
        base: Arc<CaptureSnapshot>,
    ) -> Arc<CaptureSnapshot> {
        if let Err(error)=self.camera.set_embedded_audio_route(
            self.audio.embedded_sink(),self.audio.program_key(),self.audio.embedded_clock(&self.session)) {
            self.camera_error=Some(error.to_string());
        }
        if let Err(error) = self.camera.synchronize(&self.session, active) {
            self.camera_error = Some(error.to_string());
            self.fail_obs_camera_import(&error.to_string());
            // Reject a conflicting source binding without discarding frames
            // belonging to unrelated, still valid native camera resources.
            return self.capture_union.merge(&self.session,base.clone(),&self.camera).unwrap_or(base);
        }
        if self
            .camera
            .last_requested_at()
            .is_some_and(|previous| now < previous)
        {
            if let Err(error) = self.camera.reset_timing(&self.session) {
                self.camera_error = Some(error.to_string());
            }
        }
        match self.camera.poll(&self.session) {
            Ok(Some(CameraEvent::Controls {key,result}))=>{
                let pending=self.pending_camera_controls.take();
                if let Some(Dialog::CameraCapture(d))=&mut self.dialog {
                    if pending==Some((d.token,key)) {
                        d.driver.loading=false;
                        if d.session==self.session.identity() && d.revision==self.session.snapshot().revision() {
                            match result {
                                Ok(catalog) if d.selected.as_ref()==Some(catalog.lease.binding.target()) && d.selected_mode==Some(catalog.lease.binding.mode())=>d.driver.loaded(catalog),
                                Ok(_)=>d.driver.message=Some("Camera selection changed; read controls again".into()),
                                Err(error)=>d.driver.message=Some(error.to_string()),
                            }
                        }
                    }
                }
            }
            Ok(Some(CameraEvent::ControlSet {key,lease,receipt}))=>{
                let pending=self.pending_camera_controls.take();
                let mut shown=false;
                if let Some(Dialog::CameraCapture(d))=&mut self.dialog {
                    if pending==Some((d.token,key)) {
                        d.driver.loading=false;
                        if d.session==self.session.identity() && d.revision==self.session.snapshot().revision()
                            && d.driver.catalog.as_ref().is_some_and(|c| c.lease==lease) {
                            d.driver.receipt(&receipt);shown=true;
                        }
                    }
                }
                // An external effect receipt remains observable if the user
                // closed the properties dialog while Set/readback was running.
                if !shown {self.message(receipt.diagnostic.is_some(),views::camera_controls::summary(context,&receipt));}
            }
            Ok(Some(CameraEvent::Devices { key, result })) => {
                let pending = self.pending_camera.take();
                if let Some(Dialog::CameraCapture(draft)) = &mut self.dialog {
                    draft.devices(
                        pending,
                        key,
                        self.session.identity(),
                        self.session.snapshot().revision(),
                        result,
                    );
                }
            }
            Ok(Some(CameraEvent::Modes {
                key,
                target,
                result,
            })) => {
                if self.owns_obs_camera_request(key) {
                    self.obs_camera_modes(key,target,result,context);
                } else {
                    let pending = self.pending_camera.take();
                    if let Some(Dialog::CameraCapture(draft)) = &mut self.dialog {
                        draft.modes(
                            pending,
                            key,
                            self.session.identity(),
                            self.session.snapshot().revision(),
                            target,
                            result,
                        );
                    }
                }
            }
            Ok(Some(CameraEvent::DirectShowPreferred {key,target,result})) => {
                if self.owns_obs_camera_request(key) {
                    self.obs_camera_preferred(key,target,result,context);
                } else if self.pending_camera.is_some_and(|(_, pending)| pending==key) {
                    let pending=self.pending_camera.take();
                    if let Some(Dialog::CameraCapture(draft))=&mut self.dialog {
                        draft.preferred(pending,key,self.session.identity(),self.session.snapshot().revision(),target,result);
                    }
                }
            }
            Ok(Some(CameraEvent::DirectShowVideoOnly {key,target,result})) => {
                if self.owns_obs_camera_request(key) {self.obs_camera_video_only(key,target,result,context);}
            }
            Ok(Some(CameraEvent::Activated {
                result: Err(error), ..
            })) => self.camera_error = Some(error.to_string()),
            Ok(Some(CameraEvent::Stopped { key })) => {
                self.accept_camera_stop(key);
            }
            Ok(Some(CameraEvent::Frames { .. })) => self.camera_error = None,
            Ok(_) => {}
            Err(error) => {self.fail_obs_camera_import(&error.to_string());self.camera_error = Some(error.to_string());},
        }
        let camera_snapshot=self.camera.snapshot();
        if let Some(Dialog::CameraCapture(draft)) = &mut self.dialog {
            draft.source_enabled=match draft.edit {views::CameraEdit::Rebind {source}=>self.camera.source_enabled(source),_=>false};
            draft.av_enabled=match draft.edit {views::CameraEdit::Rebind {source}=>self.camera_activation.enabled(&self.session,source),_=>true};
            draft.av_status=Some(self.camera_activation.status());
            draft.observed_mode=match draft.edit {
                views::CameraEdit::Rebind {source} if draft.source_enabled
                    && draft.session==self.session.identity()
                    && draft.revision==self.session.snapshot().revision()
                    && camera_snapshot.session()==draft.session => {
                    let current=self.session.snapshot();
                    camera_snapshot.frame(source).and_then(|frame| {
                        let binding=match current.source(source)?.kind() {
                            skylive_core::SourceKind::VideoCapture {binding}=>binding,
                            _=>return None,
                        };
                        frame.binding.same_video(binding).then(||(binding.clone(),frame.actual_mode,frame.carried))
                    })
                }
                _=>None,
            };
            if draft.session != self.session.identity()
                || draft.revision != self.session.snapshot().revision()
            {
                draft.loading_devices = false;
                draft.loading_modes = false;
                draft.query_modes = false;
                draft.loading_preferred = false;
                draft.query_preferred = false;
                draft.driver.loading=false;
                draft.driver.catalog=None;
                draft.error =
                    Some("The document changed. Reopen this dialog to bind a camera.".into());
            }
        }
        self.progress_obs_camera_import(context);
        if !self.camera.is_busy() {
            let query = match &self.dialog {
                Some(Dialog::CameraCapture(draft)) if draft.loading_devices => {
                    Some((draft.token, None, false))
                }
                Some(Dialog::CameraCapture(draft)) if draft.query_modes => draft
                    .selected
                    .clone()
                    .map(|target| (draft.token, Some(target), false)),
                Some(Dialog::CameraCapture(draft)) if draft.query_preferred => draft
                    .selected
                    .clone()
                    .map(|target| (draft.token, Some(target), true)),
                _ => None,
            };
            if self.camera_stop {
                match self.camera.request_stop(&self.session) {
                    Ok(key) => {
                        self.camera_stop = false;
                        self.explicit_camera_stop = Some(key);
                    }
                    Err(error) => {
                        self.camera_error = Some(error.to_string());
                        self.camera_stop = false;
                    }
                }
            } else if let Some((token, target, preferred)) = query {
                let modes = target.is_some();
                let result = match target {
                    Some(target) if preferred => self.camera.request_directshow_preferred(&self.session, target),
                    Some(target) => self.camera.request_modes(&self.session, target),
                    None => self.camera.request_devices(&self.session),
                };
                match result {
                    Ok(key) => {
                        self.pending_camera = Some((token, key));
                        if let Some(Dialog::CameraCapture(draft)) = &mut self.dialog {
                            if preferred {
                                draft.loading_preferred = true;
                                draft.query_preferred = false;
                            } else if modes {
                                draft.loading_modes = true;
                                draft.query_modes = false;
                            }
                        }
                    }
                    Err(error) => {
                        if let Some(Dialog::CameraCapture(draft)) = &mut self.dialog {
                            draft.loading_devices = false;
                            draft.loading_modes = false;
                            draft.query_modes = false;
                            draft.loading_preferred = false;
                            draft.query_preferred = false;
                            draft.error = Some(error.to_string());
                        }
                    }
                }
            } else if let Some(start) = self.camera_start.take() {
                let matches=start.session==self.session.identity()&&self.session.snapshot().source(start.source).is_some_and(|source|matches!(source.kind(),skylive_core::SourceKind::VideoCapture{binding}if binding==&start.binding));
                let current_dialog=start.dialog.is_none_or(|(token,revision)|self.session.snapshot().revision()==revision && matches!(&self.dialog,Some(Dialog::CameraCapture(draft)) if draft.token==token && draft.session==start.session && draft.revision==revision && draft.selected.as_ref()==Some(start.binding.target()) && draft.selected_mode==Some(start.binding.mode()) && draft.deactivate_when_hidden==start.binding.deactivate_when_hidden() && draft.flip_vertically==start.binding.flip_vertically()));
                if matches && current_dialog {
                    let result = if !start.enabled {
                        self.camera.request_pause_source(&self.session,start.source,now)
                    } else if start.restart {
                        self.camera
                            .request_restart(&self.session, start.source, now)
                    } else {
                        self.camera.request_start_source(&self.session,start.source,now)
                    };
                    match result {
                        Ok(_) => {}
                        Err(CameraError::Busy) => self.camera_start = Some(start),
                        Err(error) => self.camera_error = Some(error.to_string()),
                    }
                }
            } else if self.camera.is_active()&&!self.camera.av_admission_reserved() {
                if let Err(error) = self.camera.request_frame(&self.session, now) {
                    if error != CameraError::Busy {
                        self.camera_error = Some(error.to_string());
                    }
                }
            }
        }
        if self.camera.is_busy()
            || (self.camera.is_active() && self.camera.active_source_count() != 0)
            || self.camera_start.is_some()
            || self.camera_stop
        {
            context.request_repaint_after(std::time::Duration::from_millis(5));
        }
        match self
            .capture_union
            .merge(&self.session, base.clone(), &self.camera)
        {
            Ok(merged) => merged,
            Err(error) => {
                self.camera_error = Some(error.to_string());
                base
            }
        }
    }
    fn discard_stale_program_audio(&mut self) {
        let settings = self.session.snapshot().audio();
        if self.program_audio.as_ref().is_some_and(|view| {
            view.key.session != self.session.identity() || view.key.settings != settings
        }) {
            self.program_audio = None;
        }
    }
    fn update_audio(
        &mut self,
        context: &egui::Context,
        active: &[FrameScene],
        output: Option<&skylive_host::transition::FrameComposition>,
        now: MediaTime,
    ) -> AudioCaptureSnapshot {
        self.discard_stale_program_audio();
        let previous_count = self.audio.active_source_count();
        if let Some(previous) = self.audio.last_requested_at() {
            if now < previous {
                if let Err(error) = self.audio.reset_timing() {
                    self.audio_error = Some(error.to_string());
                    return self.audio.snapshot();
                }
                self.audio_next_request = None;
                self.program_audio = None;
                self.audio_program_revision = None;
            }
        }
        if let Err(error) = self.audio.synchronize_with_program(&self.session, active, &output.map(|v|v.scenes()).unwrap_or_default()) {
            self.audio_error = Some(error.to_string());
            return self.audio.snapshot();
        }
        if let Err(error) = self.audio.observe_window_capture(&self.session, &self.capture.snapshot()) {
            self.audio_error = Some(error.to_string());
            return self.audio.snapshot();
        }
        let current_count = self.audio.active_source_count();
        let file_audio = output
            .and_then(|output| skylive_host::audio::derive_audio_plan(&self.session, output).ok())
            .is_some_and(|plan| {
                plan.sources().iter().any(|source| {
                    matches!(
                        source.binding,
                        skylive_host::audio::AudioInputKey::Media { .. } | skylive_host::audio::AudioInputKey::Ndi {..} | skylive_host::audio::AudioInputKey::Browser(_) | skylive_host::audio::AudioInputKey::EmbeddedCamera {..}
                    )
                })
            });
        let wants_program = current_count != 0
            || file_audio
            || self.replay.needs_audio()
            || self.ndi_output.needs_audio()
            || self.recording.needs_audio()
            || self.streaming.needs_audio();
        if !wants_program
            && (previous_count != 0
                || self
                    .audio
                    .program_status()
                    .is_some_and(|stats| stats.running))
        {
            self.audio_stop_pending = true;
        }
        if wants_program {
            self.audio_stop_pending = false;
        }
        match self.audio.poll(&self.session) {
            Ok(Some(AudioCaptureEvent::CameraActivation {..}))=>{},
            Ok(Some(AudioCaptureEvent::Processes {key,result})) => self.accept_application_processes(key,result),
            Ok(Some(AudioCaptureEvent::Devices {key,flow,result})) if self.pending_camera_audio_devices.is_some_and(|(_,pending)|pending==key) => self.accept_camera_audio_devices(key,flow,result),
            Ok(Some(AudioCaptureEvent::Devices { key, flow, result })) if self.pending_monitor_devices==Some(key)=>self.accept_monitor_devices(key,flow,result),
            Ok(Some(AudioCaptureEvent::Devices { key, flow, result })) => {
                let pending = self.pending_audio_devices.take();
                if let Some(Dialog::AudioCapture {
                    flow: current_flow,
                    device_picker: Some(picker),
                    ..
                }) = &mut self.dialog
                {
                    if pending == Some((picker.token, key)) {
                        picker.loading = false;
                        if picker.session != key.session
                            || picker.revision != key.revision
                            || *current_flow != flow
                        {
                            picker.error=Some("The audio device request belongs to an obsolete draft. Reopen this dialog.".into());
                        } else {
                            match result {
                                Ok(devices) => {
                                    picker.devices = devices;
                                    picker.error = None;
                                }
                                Err(error) => picker.error = Some(error.to_string()),
                            }
                        }
                    }
                }
            }
            Ok(Some(AudioCaptureEvent::Packets { .. })) => self.audio_error = None,
            Ok(Some(AudioCaptureEvent::Program { result, .. })) => match result {
                Ok(stats) => {
                    self.audio_error = None;
                    if !stats.running {
                        self.program_audio = None;
                    }
                }
                Err(error) => {
                    self.audio_error = Some(error.to_string());
                    self.program_audio = None;
                }
            },
            Ok(None) => {}
            Err(error) => {
                self.audio_error = Some(error.to_string());
                if matches!(
                    error,
                    AudioCaptureError::Closed | AudioCaptureError::Disconnected
                ) {
                    self.audio.close();
                    return self.audio.snapshot();
                }
            }
        }
        self.drive_camera_av(context,output,now);
        // The worker rejects stale revision/epoch device replies before exposing
        // them. End the matching modal request as an explicit obsolete draft.
        if !self.audio.is_busy() {
            if let Some((token, key)) = self.pending_audio_devices.take() {
                if let Some(Dialog::AudioCapture {
                    device_picker: Some(picker),
                    ..
                }) = &mut self.dialog
                {
                    if picker.token == token && picker.loading {
                        picker.loading = false;
                        picker.error=Some(if key.session!=self.session.identity()||key.revision!=self.session.snapshot().revision(){"The document changed while enumerating audio devices. Reopen this dialog."}else{"The audio capture scope changed while enumerating devices. Retry this request."}.into());
                    }
                }
            }
        }
        let waiting = match &self.dialog {
            Some(Dialog::AudioCapture {
                flow,
                target,
                device_picker: Some(picker),
                ..
            }) if picker.loading => Some((picker.token, *flow, picker.session, picker.revision, *target==dialogs::AudioTargetChoice::WindowsProcess)),
            _ => None,
        };
        if !self.audio.is_busy() {
            if let Some((token, flow, session, revision, process)) = waiting {
                if session != self.session.identity()
                    || revision != self.session.snapshot().revision()
                {
                    if let Some(Dialog::AudioCapture {
                        device_picker: Some(picker),
                        ..
                    }) = &mut self.dialog
                    {
                        picker.loading = false;
                        picker.error =
                            Some("The document changed. Reopen this audio dialog.".into());
                    }
                } else {
                    match if process {self.audio.request_processes(&self.session)} else {self.audio.request_devices(&self.session, flow)} {
                        Ok(key) => self.pending_audio_devices = Some((token, key)),
                        Err(error) => {
                            if let Some(Dialog::AudioCapture {
                                device_picker: Some(picker),
                                ..
                            }) = &mut self.dialog
                            {
                                picker.loading = false;
                                picker.error = Some(error.to_string());
                            }
                        }
                    }
                }
            }
        }
        self.admit_camera_audio_devices();
        // Consume only bounded metadata projections. The native owner retains
        // PCM pool references and performs final large-buffer destruction.
        for _ in 0..8 {
            match self.audio.poll_program(&self.session) {
                Ok(Some(output)) => {
                    self.program_audio = Some(ProgramAudioView {
                        start_frame: output.block.start_frame,
                        // Missing audio is scoped to individual sources. Do
                        // not discard valid meters just because another
                        // source in the same Program block had a gap.
                        source_meters: output.block.source_meters.iter()
                            .filter(|meter| !output.block.gaps.iter().any(|gap| gap.source == meter.source && gap.missing_frames != 0))
                            .cloned().collect(),
                        source_meter_revision: output.block.source_meter_revision,
                        key: output.key,
                        meters: output.block.program_meters.clone(),
                        missing_source_frames: output
                            .block
                            .gaps
                            .iter()
                            .map(|gap| gap.missing_frames)
                            .sum(),
                        clipped_samples: output.block.clipped_samples.iter().sum(),
                    });
                }
                Ok(None) => break,
                Err(error) => {
                    self.audio_error = Some(error.to_string());
                    break;
                }
            }
        }
        if self.audio.is_busy() {
            context.request_repaint_after(std::time::Duration::from_millis(10));
        } else if wants_program || self.audio_stop_pending {
            context.request_repaint_after(std::time::Duration::from_nanos(
                self.audio_next_request.map_or(1_000_000, |deadline| {
                    deadline.saturating_sub(now.0).max(1_000_000)
                }),
            ));
        }
        self.audio.snapshot()
    }
    // Ordinary Program controls are admitted after UI intents. Start Recording
    // gets first use of the bounded actor slot that update_audio just polled.
    fn admit_authored_audio(&mut self, context: &egui::Context, now: MediaTime) {
        if self.audio.is_busy() || self.allow_close {
            return;
        }
        let Some(snapshot) = self.sync_transition() else {
            return;
        };
        let mut active = Vec::new();
        if self.studio {
            active.push(FrameScene::Live(self.session.preview()));
        }
        active.extend(snapshot.output.scenes());
        let previous = self.audio.active_source_count();
        if let Err(error) = self.audio.synchronize_with_program(&self.session, &active, &snapshot.output.scenes()) {
            self.audio_error = Some(error.to_string());
            return;
        }
        if let Err(error) = self.audio.observe_window_capture(&self.session, &self.capture.snapshot()) {
            self.audio_error = Some(error.to_string());
            return;
        }
        let count = self.audio.active_source_count();
        let file_audio = skylive_host::audio::derive_audio_plan(&self.session, &snapshot.output)
            .is_ok_and(|plan| {
                plan.sources().iter().any(|source| {
                    matches!(
                        source.binding,
                        skylive_host::audio::AudioInputKey::Media { .. } | skylive_host::audio::AudioInputKey::Ndi {..} | skylive_host::audio::AudioInputKey::Browser(_) | skylive_host::audio::AudioInputKey::EmbeddedCamera {..}
                    )
                })
            });
        let wants_program = count != 0
            || file_audio
            || self.replay.needs_audio()
            || self.ndi_output.needs_audio()
            || self.recording.needs_audio()
            || self.streaming.needs_audio();
        if !wants_program
            && (previous != 0
                || self
                    .audio
                    .program_status()
                    .is_some_and(|stats| stats.running))
        {
            self.audio_stop_pending = true;
        }
        if wants_program {
            self.audio_stop_pending = false;
        }
        let revision = (self.session.identity(), self.session.snapshot().revision());
        if !(wants_program || self.audio_stop_pending)
            || !(self
                .audio_next_request
                .is_none_or(|deadline| now.0 >= deadline)
                || self.audio.last_requested_at().is_none()
                || self.audio_program_revision != Some(revision))
        {
            return;
        }
        let monitor_capture=self.source_capture_snapshot();
        let monitor_media=self.media.snapshot();
        if let Err(error)=self.audio.set_monitor_video_context(&self.session,&monitor_capture,&monitor_media,now) {
            self.audio_error=Some(error.to_string());return;
        }
        let request = if wants_program {
            let gain_preview=if self.layout.mixer&&!self.close_dialog&&self.dialog.is_none()
                &&!self.setup.is_open()&&self.after_io.is_none()&&!self.io.busy()&&!self.documents.modal_open(){
                views::mixer::gain_preview(context).filter(|preview|preview.session==self.session.identity()
                    &&preview.revision==self.session.snapshot().revision())
            }else{None};
            if let Some(preview)=gain_preview {
                self.audio.request_program_with_gain_preview(&self.session,&snapshot.output,now,preview)
            }else{
                self.audio.request_program(&self.session,&snapshot.output,now)
            }
        } else {
            self.audio.request_stop_program(&self.session).map(|_| true)
        };
        match request {
            Ok(_) => {
                self.audio_stop_pending = false;
                self.audio_program_revision = if wants_program {
                    Some(revision)
                } else {
                    self.program_audio = None;
                    None
                };
                if let Some(next) = now.0.checked_add(20_000_000) {
                    self.audio_next_request = Some(next);
                } else {
                    self.audio_error = Some(AudioCaptureError::TimeOverflow.to_string());
                    self.audio.close();
                    self.program_audio = None;
                    return;
                }
                context.request_repaint_after(std::time::Duration::from_millis(10));
            }
            Err(error) => self.audio_error = Some(error.to_string()),
        }
    }
    fn poll_image_watch(&mut self,context:&egui::Context,now:MediaTime){
        // Preserve exact save-receipt revision. The bounded watcher can hold
        // one completion until the native Save owner finishes.
        if self.documents.saving(){context.request_repaint_after(std::time::Duration::from_millis(20));return;}
        if let Some(item)=self.image_watch.poll(){
            let current=self.session.snapshot();
            let matches=item.session==self.session.identity()&&matches!(current.source(item.source).map(|source|source.kind()),Some(skylive_core::SourceKind::Image{asset}) if asset==&item.expected);
            if matches {match item.result {
                Err(error)=>{if self.image_watch_error.as_ref()!=Some(&(item.source,error.clone())){self.image_watch_error=Some((item.source,error.clone()));self.message(true,error);}}
                Ok(Some((asset,recovered)))=>{
                    let path=asset.path().to_owned();
                    // A successful observation of unchanged bytes only retries
                    // if a current pane has actually published a linked-file
                    // read/hash error for a source using this exact path.
                    let visible_error=self.preview.diagnostics().iter().chain(self.program.diagnostics()).any(|diagnostic|{
                        matches!(diagnostic.code,"image_io"|"image_changed") && diagnostic.detail.contains(path.as_str()) && current.source(diagnostic.source).is_some_and(|source|matches!(source.kind(),skylive_core::SourceKind::Image{asset} if asset.path()==path))
                    });
                    if asset==item.expected && !recovered && !visible_error {
                        self.image_watch.schedule(&self.session,context);
                        return;
                    }
                    match self.session.refresh_image_asset(item.session,item.source,&item.expected,asset){
                        Ok(changed)=>{
                            self.image_watch_error=None;
                            if changed||recovered {
                                let model=self.session.snapshot();
                                for source in model.sources(){if matches!(source.kind(),skylive_core::SourceKind::Image{asset} if asset.path()==path){
                                    if let Err(error)=self.playback.restart_source(source.id(),now){self.playback_error=Some(error.to_string());}
                                }}
                            }
                            if let Err(error)=self.recording.invalidate_resources(){self.stop_recording();self.message(true,error);}
                            if let Err(error)=self.streaming.invalidate_resources(){self.stop_streaming();self.message(true,error);}
                            if let Err(error)=self.virtual_camera.invalidate_resources(){self.stop_virtual_camera();self.message(true,error);}
                            if let Err(error)=self.preview.retry_resources().and_then(|()|self.program.retry_resources()){self.message(true,error.to_string());}
                        }
                        Err(error)=>{let error=error.to_string();if self.image_watch_error.as_ref()!=Some(&(item.source,error.clone())){self.image_watch_error=Some((item.source,error.clone()));self.message(true,error);}}
                    }
                }
                Ok(None)=>{}
            }}
        }
        self.image_watch.schedule(&self.session,context);
    }
    fn poll_slideshow_watch(&mut self,context:&egui::Context){
        // Do not mutate the collection underneath a draft or an outstanding save.
        if self.documents.saving()||self.dialog.is_some(){context.request_repaint_after(std::time::Duration::from_millis(100));return;}
        if let Some(item)=self.slideshow_watch.poll(){
            let current=self.session.snapshot();
            let matches=item.session==self.session.identity()&&matches!(current.source(item.source).map(|source|source.kind()),Some(skylive_core::SourceKind::ImageSlideShow{settings}) if settings==&item.expected);
            if matches{match item.result{
                Err(error)=>{if self.slideshow_watch_error.as_ref()!=Some(&(item.source,error.clone())){self.slideshow_watch_error=Some((item.source,error.clone()));self.message(true,error);}}
                Ok(Some(settings))=>{
                    match self.session.refresh_slideshow_resources(item.session,item.source,&item.expected,settings){
                      Ok(true)=>{
                        self.slideshow_watch_error=None;self.restart_image=Some(item.source);
                        if let Err(error)=self.recording.invalidate_resources(){self.stop_recording();self.message(true,error);}
                        if let Err(error)=self.streaming.invalidate_resources(){self.stop_streaming();self.message(true,error);}
                        if let Err(error)=self.virtual_camera.invalidate_resources(){self.stop_virtual_camera();self.message(true,error);}
                        if let Err(error)=self.preview.retry_resources().and_then(|()|self.program.retry_resources()){self.message(true,error.to_string());}
                      }
                      Ok(false)=>{self.slideshow_watch_error=None;}
                      Err(error)=>{let error=error.to_string();if self.slideshow_watch_error.as_ref()!=Some(&(item.source,error.clone())){self.slideshow_watch_error=Some((item.source,error.clone()));self.message(true,error);}}
                    }
                }
                Ok(None)=>{self.slideshow_watch_error=None;}
            }}
        }
        self.slideshow_watch.schedule(&self.session,context);
    }
    fn frame_at(&mut self, context: &egui::Context, now: MediaTime) {
        if self.allow_close {
            return;
        }
        views::mixer::clear_gain_preview(context);
        self.frame_now = now;
        if let Err(error)=self.camera_activation.synchronize(&self.session,&mut self.camera,&mut self.audio){self.camera_error=Some(error.to_string());}
        self.capture_union.set_camera_activation(self.camera_activation.snapshot());
        let mut transition = self.sync_transition();
        self.poll_replay(context);
        self.poll_ndi_outputs(context);
        self.poll_recording(context);
        self.poll_streaming(context);
        self.poll_virtual_camera(context);
        self.poll_io(context);
        self.poll_image_watch(context,now);
        self.poll_slideshow_watch(context);
        self.flush_library(context);
        self.update_pending_open(context);
        self.update_startup(context);
        self.update_projector_restoration(context);
        self.observe_settings_draft();
        self.poll_process_priority(context);
        self.poll_remux(context);
        self.install_browse_scope(context);
        self.appearance_runtime
            .sync(context, self.session.snapshot().preferences().appearance());
        crate::theme::install_accessibility(context,self.session.snapshot().preferences().accessibility());
        if self.allow_close {
            return;
        }
        if context.input(|input| input.viewport().close_requested()) {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_exit(context);
            if self.allow_close {
                return;
            }
        }
        if transition.as_ref().is_none_or(|snapshot| {
            snapshot.session != self.session.identity()
                || snapshot.revision != self.session.snapshot().revision()
        }) {
            transition = self.sync_transition();
        }
        // Captured Program layouts remain output owners even when authored
        // scene items disappear. One activation union feeds both panes.
        let mut active = Vec::new();
        if self.studio {
            active.push(FrameScene::Live(self.session.preview()));
        }
        if let Some(snapshot) = &transition {
            active.extend(snapshot.output.scenes());
        }
        // Virtual camera is video-only. Capture/playback see its extra view;
        // update_audio keeps the original Program/Preview activation scope.
        let latest_capture=self.source_capture_snapshot();
        let vc_output=self.virtual_camera_output(transition.as_ref().map(|state|&state.output),&latest_capture);
        let ndi_preview_output=self.ndi_preview_output(transition.as_ref().map(|state|&state.output));
        let mut video_active=active.clone();
        if let Some(output)=&ndi_preview_output{for view in output.scenes(){if !video_active.iter().any(|old|old.key()==view.key()){video_active.push(view);}}}
        if let Some(output)=&vc_output {
            for view in output.scenes(){if !video_active.iter().any(|old|old.key()==view.key()){video_active.push(view);}}
        }
        if let Some(state)=&transition{for view in self.projectors.scopes(&self.session,&state.output,self.studio,&latest_capture){if !video_active.iter().any(|old|old.key()==view.key())&&video_active.len()<skylive_host::projector::MAX_VIDEO_VIEWS-1{video_active.push(view);}}}
        if self.service_freeze_timing(context,&video_active,now){return;}
        let capture = self.update_capture(context, &video_active, now);
        let audio = self.update_audio(
            context,
            &active,
            transition.as_ref().map(|snapshot| &snapshot.output),
            now,
        );
        let capture = self.update_media(
            context,
            transition.as_ref().map(|s| &s.output),
            &video_active,
            now,
            capture,
        );
        let capture=self.update_ndi(context,&active,&video_active,now,capture);
        let capture=self.update_game(context,&video_active,now,capture);
        let browser_program=transition.as_ref().map(|state|state.output.scenes()).unwrap_or_default();
        let capture=self.update_browser(context,&video_active,&browser_program,&active,now,capture);
        let playback = self
            .restart_image
            .take()
            .map_or(Ok(()), |source| self.playback.restart_source(source, now))
            .and_then(|()| self.playback.update_outputs_with_capture(&self.session, &video_active, now,&capture));
        let preview_menu_enabled = !self.io.busy()
            && self.after_io.is_none()
            && !self.close_dialog
            && self.dialog.is_none()
            && !self.virtual_camera.settings_open() && self.ndi_output.settings.is_none()
            && !self.setup.is_open()
            && !self.document_repair.owns()
            && !self.documents.modal_open()
            && (self.studio || !self.transition_active);
        let preview_editable = preview_menu_enabled && !self.preview_locked;
        if !preview_editable {
            self.preview_interaction.cancel();
        }
        let preview_draft = self.preview_interaction.draft_output(
            &self.session,
            &capture,
            self.session.preview(),
            self.selected,
        );
        let was_recording = self.recording.active();
        match &playback {
            Ok(sample) => {
                // Publish the same immutable mask timeline and revision owner
                // used by playback; capture only consumes scope/time matched
                // snapshots and never waits for this lock.
                if let Some(mask)=self.playback.camera_mask_snapshot_calibrated(sample,self.playback_origin) {
                    let _=self.ndi.set_mask_snapshot(&self.session,mask.clone());
                    let _=self.camera.set_mask_snapshot(&self.session,mask);
                }
                self.playback_error = None;
                if let Some(snapshot) = &transition {
                    self.projectors.update(context,&self.session,&snapshot.output,self.studio,sample,capture.clone(),self.preview.resource_generation());
                    self.ndi_output_frames(context,sample,&snapshot.output,ndi_preview_output.as_ref(),&capture);
                    self.replay_program_frame(context,sample,&snapshot.output,&capture);
                    self.record_program_frame(context, sample, &snapshot.output, &capture);
                    self.stream_program_frame(context, sample, &snapshot.output, &capture);
                    self.virtual_camera_frame(context, sample, &snapshot.output, &capture);
                }
                if self.studio {
                    if let Some((epoch, output)) = &preview_draft {
                        self.preview.update_draft_with_capture(
                            context,
                            &self.session,
                            sample,
                            output,
                            &capture,
                            *epoch,
                        );
                    } else {
                        self.preview.update_playback_with_capture(
                            context,
                            &self.session,
                            self.session.preview(),
                            sample,
                            &capture,
                        );
                    }
                    if let Some(snapshot) = &transition {
                        self.program.update_output_with_capture(
                            context,
                            &self.session,
                            sample,
                            &snapshot.output,
                            &capture,
                        );
                    }
                } else if let Some(snapshot) = &transition {
                    if let Some((epoch, output)) = &preview_draft {
                        self.preview.update_draft_with_capture(
                            context,
                            &self.session,
                            sample,
                            output,
                            &capture,
                            *epoch,
                        );
                    } else {
                        self.preview.update_output_with_capture(
                            context,
                            &self.session,
                            sample,
                            &snapshot.output,
                            &capture,
                        );
                    }
                }
            }
            Err(error) => self.playback_error = Some(error.to_string()),
        }
        self.source_clipboard.synchronize(&self.session);
        if let Err(error) = self.edit_clipboard.synchronize(&self.session) {
            self.message(true, error);
        }
        // Freeze edits during loads as well as saves: a delayed load must never
        // replace edits made after its admission.
        let enabled = !self.io.busy()
            && self.after_io.is_none()
            && !self.close_dialog
            && self.dialog.is_none()
            && !self.virtual_camera.settings_open() && self.ndi_output.settings.is_none()
            && !self.setup.is_open()
            && !self.documents.modal_open()
            && !self.collections.modal_open() && !self.collections.removal_pending();
        if self.notes_dock.observe(
            self.session.identity(),
            &self.session.snapshot(),
            self.session.preview(),
        ) {
            self.layout.arrangement.scene_notes_visible = true;
        }
        self.browser_interaction.show(context,&self.session,&mut self.browser);
        self.projectors.install(context,self.io.busy());
        let projector_intents=self.projectors.show(context,&self.session);
        let mut intents = self.global_hotkey_events(context);
        intents.extend(projector_intents);
        if enabled
            && context.input(|input|input.focused)
            && !self.browser_interaction.reserves_keyboard()
            && !crate::shortcuts::keyboard_reserved(context)
            && !self.layout.columns.reserves_keyboard(context)
            && !self.layout.arrangement.reserves_keyboard(context)
        {
            intents.extend(crate::shortcuts::collect(
                context,
                self.session.snapshot().preferences().hotkeys(),
                self.studio,
                self.transition_active,
                self.recording.view.clone(),
                self.session.can_undo(),
                self.session.can_redo(),
            ));
            intents.extend(
                crate::edit_clipboard_shortcuts::collect(
                    context,
                    &self.edit_clipboard,
                    &self.session,
                    self.selected,
                )
                .into_iter()
                .map(Intent::Clipboard),
            );
            intents.extend(crate::source_clipboard::collect(
                context, &self.source_clipboard, &self.session, self.selected,
            ).into_iter().map(Intent::SourceClipboard));
        }
        self.source_clipboard.install(context, &self.session);
        self.edit_clipboard.install_mixer(context, &self.session);
        self.install_source_output_resize(context);
        views::source_color_tags::install(context,&self.session,self.session.preview(),self.selected,self.dialog.as_ref());
        views::item_compositing::install(context,views::item_compositing::Focus {
            session:self.session.identity(),revision:self.session.snapshot().revision(),
            root:self.session.preview(),selected:self.selected,
        });
        context.data_mut(|data| {
            data.insert_temp(egui::Id::new("window-always-on-top"), self.always_on_top);
            data.insert_temp(egui::Id::new("window-status-visible"), self.status_visible);
            data.insert_temp(egui::Id::new("window-preview-locked"), self.preview_locked);
        });
        self.install_library(context);
        egui::TopBottomPanel::top("main-menu").show(context, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                self.catalog.show_with_clipboard(
                    ui,
                    views::menus::MenuContext {
                        session: &self.session,
                        selected: self.selected,
                        layout: &self.layout,
                        studio: self.studio,
                        clipboard: &self.edit_clipboard,
                        source_clipboard: Some(&self.source_clipboard),
                    },
                    &mut intents,
                )
            });
        });
        self.show_startup_information(context);
        self.show_pending_open_progress(context);
        views::document_picker::sync_collection_title(context,self.session.snapshot().name(),self.session.is_document_dirty());
        if self.status_visible { egui::TopBottomPanel::bottom("status").show(context, |ui| {
            // Keep the compact document identity on its own row. Long status
            // and device errors then receive the full panel width, including
            // at enlarged interface scales; horizontal_wrapped can otherwise
            // measure a long label beyond the remaining row width.
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.vertical(|ui| {
                views::document_picker::current_collection(ui,self.session.snapshot().name(),self.active_document_path.as_deref(),self.session.is_document_dirty());
                if self.io.busy() {
                    ui.spinner();
                }
                if self.error {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        self.status_presentation.text(ui.ctx(), &self.status),
                    );
                } else {
                    ui.label(self.status_presentation.text(ui.ctx(), &self.status));
                }
                if let Some(error) = &self.playback_error {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        crate::i18n::tf(
                            ui,
                            "Playback: {error}",
                            &[("{error}", (error).to_string())],
                        ),
                    );
                }
                if let Some(error) = &self.capture_error {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        crate::i18n::tf(
                            ui,
                            "Display Capture: {error}",
                            &[("{error}", (error).to_string())],
                        ),
                    );
                }
                if let Some(error) = &self.camera_error {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        crate::i18n::tf(ui, "Camera: {error}", &[("{error}", error.to_string())]),
                    );
                }
                if let Some(error) = &self.media_error {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("{}: {error}", crate::i18n::tr(ui, "Media Source")),
                    );
                }
                if let Some(error) = &self.audio_error {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        crate::i18n::tf(
                            ui,
                            "Audio Capture: {error}",
                            &[("{error}", (error).to_string())],
                        ),
                    );
                }
            });
        });
        }
        let mixer_program_views = transition
            .as_ref()
            .map(|snapshot| snapshot.output.scenes())
            .unwrap_or_default();
        views::dock_layout::show(
            context,
            &self.session.snapshot(),
            self.session.preview(),
            self.selected,
            &mut self.layout,
            views::docks::DockViewState {
                editing_enabled: enabled,
                recording: self.recording.view.clone(),
                streaming: self.streaming.view.clone(),
                transition: TransitionViewState {
                    session: self.session.identity(),
                    studio: self.studio,
                    active: self.transition_active,
                    manual: self.transition_manual,
                    preset_focus: self.transition_preset_focus,
                },
                audio: &audio,
                active_audio: &mixer_program_views,
                program_audio: self.program_audio.as_ref(),
                program_status: self.audio.program_status_for(&self.session),
                media: Some(views::media_controls::MediaTransportView {
                    snapshot: &self.media.snapshot(),
                    busy: self.media.is_busy() || self.pending_media_control.is_some(),
                }),
            },
            &mut intents,
        );
        views::scene_notes_dock::show(
            context,
            &mut self.notes_dock,
            views::scene_notes_dock::NotesView {
                session: self.session.identity(),
                model: &self.session.snapshot(),
                selected: self.session.preview(),
                visible: self.layout.arrangement.scene_notes_visible,
                editing: enabled,
                locked: self.layout.arrangement.locked,
            },
            &mut intents,
        );
        if let (Ok(sample),Some(state))=(&playback,&transition){self.screenshot_frame(context,sample,&state.output,&capture);}else{self.screenshot_no_frame();}
        let mut preview_actions = crate::preview_interaction::Actions::default();
        egui::CentralPanel::default().show(context, |ui| {
            if self.studio {
                ui.columns(2, |columns| {
                    let rect = self.preview.show_with_actions(
                        &mut columns[0],
                        "Preview",
                        &self.session,
                        self.session.preview(),
                        &mut intents,
                    );
                    preview_actions = self.preview_interaction.show(
                        &mut columns[0],
                        crate::preview_interaction::View {
                            session: &self.session,
                            capture: &capture,
                            auto_text:self.preview.displayed_auto_text(),
                            scene: self.session.preview(),
                            selected: self.selected,
                            canvas: rect,
                            enabled: preview_editable,
                            menu_enabled: preview_menu_enabled,
                        },
                    );
                    self.program.show_with_actions(
                        &mut columns[1],
                        "Program",
                        &self.session,
                        transition
                            .as_ref()
                            .map_or(self.session.program(), |snapshot| {
                                snapshot.output.primary_scene()
                            }),
                        &mut intents,
                    );
                });
            } else {
                let rect = self.preview.show_with_actions(
                    ui,
                    "Preview",
                    &self.session,
                    transition
                        .as_ref()
                        .map_or(self.session.program(), |snapshot| {
                            snapshot.output.primary_scene()
                        }),
                    &mut intents,
                );
                preview_actions = self.preview_interaction.show(
                    ui,
                    crate::preview_interaction::View {
                        session: &self.session,
                        capture: &capture,
                        auto_text:self.preview.displayed_auto_text(),
                        scene: self.session.preview(),
                        selected: self.selected,
                        canvas: rect,
                        enabled: preview_editable,
                        menu_enabled: preview_menu_enabled,
                    },
                );
            }
            ui.add_enabled_ui(enabled, |ui| views::group_source::confirm_remove_selection(
                ui,&self.session.snapshot(),self.session.preview(),self.session.identity(),&mut intents));
        });
        if !self.close_dialog && self.after_io.is_none() && !self.virtual_camera.settings_open() && self.ndi_output.settings.is_none() && !self.remux.view.open && self.missing_files.panel.is_none() && !self.document_repair.owns() {
            if let Some(Dialog::MediaSource(draft)) = &mut self.dialog {
                draft.worker_busy = self.media.is_busy();
                draft.decoder_info=match draft.edit {views::media_source::MediaEdit::Rebind {source}=>match self.session.snapshot().source(source).map(|s|s.kind()) {Some(skylive_core::SourceKind::MediaSource {media}) if draft.path==media.path()=>self.media.snapshot().video_decoder_for(self.session.identity(),source,media),_=>None},_=>None};
            }
            if let Some(Dialog::AdvancedAudio(draft)) = &mut self.dialog {
                draft.refresh_activity(
                    &self.session,
                    transition.as_ref().map(|snapshot| &snapshot.output),
                    self.studio,
                );
                draft.refresh_formats(
                    &self.session,
                    &self.audio.snapshot(),
                    &self.media.snapshot(),
                );
                draft.refresh_ndi_formats(&self.session,&self.ndi.snapshot());
                draft.refresh_browser_formats(&self.session,&self.browser.snapshot());
            }
            if let Some(Dialog::VideoFilters(draft))=&mut self.dialog {
                if let Ok(sample)=&playback {
                    let filters=draft.preview_filters();
                    let rows=draft.preview_row_ids();
                    draft.preview.update(context,&self.session,(draft.session,draft.revision),draft.target,draft.token,&rows,filters,sample,&capture,&self._render_gpu,self.preview.resource_generation());
                }
            }
            self.observe_settings_draft();
            let settings_token = self.settings_flow.token();
            let model = self.session.snapshot();
            self.refresh_source_output_resize();
            let dialog_enabled=!self.io.busy()||matches!(&self.dialog,Some(Dialog::SourceOutputResize(_)));
            dialogs::show_with_settings(
                context,
                &mut self.dialog,
                &mut intents,
                dialog_enabled,
                &model,
                settings_token,
            );
        }
        if !self.close_dialog && self.after_io.is_none() && !self.virtual_camera.settings_open() && self.ndi_output.settings.is_none() && !self.remux.view.open && self.missing_files.panel.is_none() && !self.document_repair.owns() {
            self.setup
                .show(context, &self.session, !self.io.busy(), &mut intents);
        }
        intents.append(&mut preview_actions.intents);
        for intent in intents {
            self.intent(intent, context);
        }
        self.show_pending_open(context);
        self.show_missing_files(context);
        self.show_library(context);
        self.show_document_prompt(context);
        self.show_virtual_camera_settings(context);
        self.show_ndi_output_settings(context);
        let mut remux_intents=Vec::new();self.remux.view.show(context,&mut remux_intents,!self.io.busy());for intent in remux_intents{self.intent(intent,context);}
        if self.statistics.is_open() {
            let model=self.session.snapshot();
            let video=model.video();
            let (virtual_camera_frames,virtual_camera_active,virtual_camera_run)=self.virtual_camera.statistics();
            self.statistics.show(context,crate::statistics::Snapshot {
                session:self.session.identity(),canvas:(video.width(),video.height(),video.fps_num(),video.fps_den()),
                preview:self.preview.metrics(),program:self.program.metrics(),
                recording:self.recording.statistics(),streaming:self.streaming.statistics(),
                filters:self._render_gpu.filter_cache_stats(),
                replay:self.replay.owner.status(),
                virtual_camera_frames,virtual_camera_active,virtual_camera_run,
            });
        }
        self.service_pending_recording_start(context);
        if !was_recording && self.recording.active() {
            // Reuse only an unchanged, fully checked Program sample. Runtime
            // transitions can change without a canonical model revision.
            if let (Ok(sample), Some(snapshot)) = (&playback, &transition) {
                self.record_initial_program_frame(context, sample, &snapshot.output, &capture);
            }
            context.request_repaint();
        }
        if let Some(selection) = preview_actions.selection {
            self.selected = selection;
        }
        if let Some(commit) = preview_actions.commit {
            if commit.session == self.session.identity() {
                let group_scene=match &commit.command {Command::SetTransforms{scene,..}=>Some(*scene),_=>None};
                let model=self.session.snapshot();
                let applied=commit.auto_text.with_current(commit.session,&model,||self.publish_command_at(Some(commit.revision),commit.command,true));
                if applied==Some(true) {
                    if let Some(scene)=group_scene.and_then(|container|self.session.snapshot().root_scene_for_container(container)) {
                    crate::views::group_source::retain_preview_selection_after_move(
                        context,&self.session.snapshot(),scene,self.session.identity(),commit.revision,self.selected,
                    );
                    }
                }
                if applied.is_none(){self.message(true,"Preview edit was cancelled because automatic text changed.");}
            } else {
                self.message(
                    true,
                    "Preview edit was cancelled because the scene collection changed.",
                );
            }
        }
        self.admit_authored_audio(context, now);
        self.persist_last_native_document();
        self.persist_preferences();
        self.close_prompt(context);
        if self.io.busy() {
            context.request_repaint_after(std::time::Duration::from_millis(32));
        }
        if self.transition_needs_tick {
            context.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
}
impl eframe::App for StudioApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if self.session.snapshot().sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::Text{settings} if settings.auto_extents())){
            context.request_repaint_after(std::time::Duration::from_millis(250));
        }
        self.frame(context);
    }
}
impl Drop for StudioApp {
    fn drop(&mut self) {
        self.remux.owner.close();
        self.process_priority.close();
        self.replay.close();
        if let Some(Dialog::VideoFilters(draft))=&mut self.dialog {draft.preview.close();}
        self.camera.close(); // Revoke queued driver writes before destroying the Session.
        self.session.close();
        self.preview.close();
        self.program.close();
        self.capture_union.clear();
        self.ndi_union.clear();
        self.media_union.clear();
        self.pending_media_control = None;
        self.ndi.close();
        self.global_hotkeys.owner.close();
        self.browser_interaction.close();
        self.browser.close();self.browser_union.clear();
        self.media.close();
        self.capture.close();
        self.camera.close();
        self.audio.close();
        self.image_watch.close();
        self.slideshow_watch.close();
        self.io.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    #[derive(Clone, Default)]
    struct FakeCaptureState {
        enumerations: Arc<AtomicUsize>,
        frames: Arc<AtomicUsize>,
        hold_enumeration: Arc<AtomicBool>,
        yellow: Arc<AtomicBool>,
        wide: Arc<AtomicBool>,
    }
    struct FakeCapture(FakeCaptureState);
    #[derive(Clone, Default)]
    struct FakeAudioState {
        factories: Arc<AtomicUsize>,
        enumerations: Arc<AtomicUsize>,
        opens: Arc<AtomicUsize>,
        closes: Arc<AtomicUsize>,
        hold_enumeration: Arc<AtomicBool>,
        program_packets: Arc<AtomicUsize>,
    }
    fn fake_audio_capabilities() -> AudioCapabilities {
        AudioCapabilities {
            backend: "in-memory audio",
            input: true,
            output: true,
            device_enumeration: true,
            exclude_own_audio: true,
            note: Some("Synthetic PCM; no native device calls"),
        }
    }
    struct FakeAudioBackend(FakeAudioState);
    struct FakeAudioStream {
        state: FakeAudioState,
        sent: bool,
        closed: bool,
        remaining: usize,
        next_frame: u64,
    }
    impl audio_capture::NativeAudioBackend for FakeAudioBackend {
        fn capabilities(&self) -> AudioCapabilities {
            fake_audio_capabilities()
        }
        fn host_now_ns(&self) -> Result<u64, AudioCaptureError> {
            Ok(0)
        }
        fn enumerate(
            &mut self,
            flow: skylive_core::AudioFlow,
            _: audio_capture::AudioCaptureLimits,
        ) -> Result<Vec<skylive_host::audio_capture::AudioDeviceInfo>, AudioCaptureError> {
            self.0.enumerations.fetch_add(1, Ordering::SeqCst);
            while self.0.hold_enumeration.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            Ok(vec![audio_capture::AudioDeviceInfo {
                target: skylive_core::AudioTarget::WindowsEndpoint {
                    id: "fake-audio-endpoint".into(),
                },
                name: "Fake Audio Device".into(),
                flow,
                is_default: true,
            }])
        }
        fn open(
            &mut self,
            _: &skylive_core::AudioCaptureBinding,
            _: audio_capture::AudioCaptureLimits,
        ) -> Result<Box<dyn audio_capture::NativeAudioStream>, AudioCaptureError> {
            self.0.opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeAudioStream {
                state: self.0.clone(),
                sent: false,
                closed: false,
                remaining: self.0.program_packets.load(Ordering::SeqCst),
                next_frame: 0,
            }))
        }
    }
    impl audio_capture::NativeAudioStream for FakeAudioStream {
        fn description(&self) -> Option<Arc<str>> {
            Some(Arc::from("Fake Audio Device"))
        }
        fn poll(&mut self) -> Result<Option<audio_capture::NativePcmBlock>, AudioCaptureError> {
            if self.remaining != 0 {
                self.remaining -= 1;
                self.sent = true;
                let frames = 4096;
                let first = self.next_frame * 1_000_000_000 / 48000;
                self.next_frame += frames as u64;
                return Ok(Some(audio_capture::NativePcmBlock {
                    format: audio_capture::NativeAudioFormat {
                        sample_rate: 48000,
                        channels: 2,
                        channel_mask: 3,
                    },
                    frames,
                    samples: [0.5, 0.25].repeat(frames),
                    device_timestamp_ns: Some(first),
                    graph_timestamp_ns: None,
                    directshow_clock: None,
                    host_received_ns: self.next_frame * 1_000_000_000 / 48000,
                    device_frame_position: Some(self.next_frame - frames as u64),
                    flags: Default::default(),
                }));
            }
            if self.sent {
                return Ok(None);
            }
            self.sent = true;
            Ok(Some(audio_capture::NativePcmBlock {
                format: audio_capture::NativeAudioFormat {
                    sample_rate: 48000,
                    channels: 2,
                    channel_mask: 3,
                },
                frames: 4,
                samples: vec![0.5, -0.25, -0.5, 0.25, 0.5, -0.25, -0.5, 0.25],
                device_timestamp_ns: Some(1_000_000),
                graph_timestamp_ns: None,
                directshow_clock: None,
                host_received_ns: 20_000_000,
                device_frame_position: Some(0),
                flags: Default::default(),
            }))
        }
        fn close(&mut self) -> Result<(), AudioCaptureError> {
            if !self.closed {
                self.closed = true;
                self.state.closes.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
    }
    fn fake_audio_app(context: &egui::Context, state: FakeAudioState) -> StudioApp {
        let factory: AudioBackendFactory = Box::new(move || {
            state.factories.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeAudioBackend(state)))
        });
        StudioApp::with_workers(
            context,
            Box::new(capture::UnsupportedBackend),
            AudioCaptureWorker::new(factory, fake_audio_capabilities()).unwrap(),
            crate::platform::CapturePlatform::Windows,
        )
        .unwrap()
    }
    fn wait_audio_picker(app: &mut StudioApp, context: &egui::Context) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let _ = at_frame(app, context, 0, vec![]);
            if matches!(&app.dialog,Some(Dialog::AudioCapture {device_picker:Some(picker),..}) if !picker.loading)
            {
                break;
            }
            assert!(Instant::now() < deadline, "audio picker did not settle");
            std::thread::yield_now();
        }
    }
    fn scroll_audio_text(
        app: &mut StudioApp,
        context: &egui::Context,
        label: &str,
        direction: f32,
    ) -> egui::FullOutput {
        for _ in 0..32 {
            let output = at_frame(app, context, 0, vec![]);
            if !visible_text_positions(&output, label).is_empty() {
                return output;
            }
            let rect = context
                .data_mut(|data| data.get_temp::<egui::Rect>(egui::Id::new("audio-mixer-viewport")))
                .expect("mixer exposes a real scroll viewport");
            let point = egui::pos2(rect.right() - 12.0, rect.center().y);
            let _ = at_frame(
                app,
                context,
                0,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, direction * 60.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        panic!("mixer text {label} was not reachable through its scroll viewport");
    }
    impl CaptureBackend for FakeCapture {
        fn capabilities(&self) -> capture::CaptureCapabilities {
            capture::CaptureCapabilities {
                available: true,
                backend: "in-memory fake",
                cursor: true,
                hdr: false,
                note: Some("Test pixels; no OS display access"),
            }
        }
        fn window_capabilities(&self) -> capture::CaptureCapabilities {
            self.capabilities()
        }
        fn enumerate_windows(
            &mut self,
            _: capture::CaptureLimits,
        ) -> Result<Vec<skylive_host::window_capture::WindowInfo>, capture::CaptureError> {
            self.0.enumerations.fetch_add(1, Ordering::SeqCst);
            while self.0.hold_enumeration.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            Ok(vec![skylive_host::window_capture::WindowInfo {
                full_window_size: None,
                target: skylive_core::WindowTarget::Windows {
                    title: "Fake Editor".into(),
                    class: "FakeEditorClass".into(),
                    executable: "fake-editor.exe".into(),
                },
                name: "Fake Editor".into(),
                width: 2,
                height: 2,
                minimized: false,
            }])
        }
        fn capture_window(
            &mut self,
            binding: &skylive_core::WindowCaptureBinding,
            limits: capture::CaptureLimits,
        ) -> Result<skylive_media::RgbaFrame, capture::CaptureError> {
            self.capture(
                &skylive_core::DisplayCaptureBinding::new(
                    skylive_core::DisplayTarget::Windows {
                        device_id: "fake-a".into(),
                    },
                    binding.capture_cursor(),
                    binding.width(),
                    binding.height(),
                )
                .unwrap(),
                limits,
            )
        }
        fn enumerate(
            &mut self,
            _: capture::CaptureLimits,
        ) -> Result<Vec<capture::DisplayInfo>, capture::CaptureError> {
            self.0.enumerations.fetch_add(1, Ordering::SeqCst);
            while self.0.hold_enumeration.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            Ok([
                ("fake-a", "Fake Display A", true),
                ("fake-b", "Fake Display B", false),
            ]
            .into_iter()
            .map(|(id, name, primary)| capture::DisplayInfo {
                target: skylive_core::DisplayTarget::Windows {
                    device_id: id.into(),
                },
                name: name.into(),
                width: 2,
                height: 2,
                primary,
            })
            .collect())
        }
        fn capture(
            &mut self,
            binding: &skylive_core::DisplayCaptureBinding,
            _: capture::CaptureLimits,
        ) -> Result<skylive_media::RgbaFrame, capture::CaptureError> {
            self.0.frames.fetch_add(1, Ordering::SeqCst);
            let color = if self.0.yellow.load(Ordering::SeqCst) {
                [255, 255, 0, 255]
            } else if binding.target()
                == &(skylive_core::DisplayTarget::Windows {
                    device_id: "fake-b".into(),
                })
            {
                [0, 0, 255, 255]
            } else if binding.capture_cursor() {
                [255, 0, 0, 255]
            } else {
                [0, 255, 0, 255]
            };
            let width = if self.0.wide.load(Ordering::SeqCst) {
                4
            } else {
                2
            };
            Ok(
                skylive_media::RgbaFrame::new(width, 2, color.repeat((width * 2) as usize))
                    .unwrap(),
            )
        }
    }
    fn capture_click(app: &mut StudioApp, context: &egui::Context, now: u64, label: &str) {
        let _ = at_frame(app, context, now, vec![]);
        let output = at_frame(app, context, now, vec![]);
        if visible_text_positions(&output, label).is_empty() {
            let probes = context
                .data_mut(|data| {
                    data.get_temp::<Vec<views::dock_toolbar::Probe>>(egui::Id::new(
                        "dock-toolbar-probes",
                    ))
                })
                .unwrap_or_default();
            if let Some(probe) = probes.iter().rev().find(|probe| probe.action == label) {
                let _ = click_at(app, context, now, probe.rect.center());
                return;
            }
            for scope in ["Sources", "Scenes"] {
                if probes
                    .iter()
                    .any(|probe| probe.scope == scope && probe.action.ends_with(" Actions"))
                {
                    let _ = at_frame(
                        app,
                        context,
                        now,
                        vec![egui::Event::Key {
                            key: egui::Key::Escape,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        }],
                    );
                    // Escape and asynchronous worker status can relayout the
                    // bottom panel. Read the current toolbar geometry rather
                    // than clicking a coordinate captured before that frame.
                    let _ = at_frame(app, context, now, vec![]);
                    let point = context
                        .data_mut(|data| {
                            data.get_temp::<Vec<views::dock_toolbar::Probe>>(egui::Id::new(
                                "dock-toolbar-probes",
                            ))
                        })
                        .unwrap_or_default()
                        .iter()
                        .find(|probe| probe.scope == scope && probe.action.ends_with(" Actions"))
                        .map(|probe| probe.rect.center());
                    let Some(point) = point else { continue };
                    let _ = click_at(app, context, now, point);
                    let _ = at_frame(app, context, now, vec![]);
                    let menu = at_frame(app, context, now, vec![]);
                    if let Some(point) = visible_text_positions(&menu, label).last() {
                        let _ = click_at(app, context, now, *point);
                        return;
                    }
                }
            }
        }
        let point = visible_text_positions(&output, label)
            .last()
            .copied()
            .unwrap_or_else(|| {
                fn texts(shape: &egui::epaint::Shape, clip: egui::Rect, values: &mut Vec<(String, bool)>) {
                    match shape {
                        egui::epaint::Shape::Text(text) => values.push((text.galley.text().to_owned(), clip.contains(text.pos + text.galley.size() * 0.5))),
                        egui::epaint::Shape::Vec(shapes) => for shape in shapes { texts(shape, clip, values); },
                        _ => {}
                    }
                }
                let mut values=Vec::new();
                for shape in &output.shapes { texts(&shape.shape,shape.clip_rect,&mut values); }
                panic!("display widget {label} is visible; painted texts and center visibility: {values:?}")
            });
        let _ = click_at(app, context, now, point);
    }
    fn toolbar_point(context: &egui::Context, scope: &str, action: &str) -> egui::Pos2 {
        context
            .data_mut(|data| {
                data.get_temp::<Vec<views::dock_toolbar::Probe>>(egui::Id::new(
                    "dock-toolbar-probes",
                ))
            })
            .unwrap_or_default()
            .iter()
            .find(|probe| probe.scope == scope && probe.action == action)
            .map(|probe| probe.rect.center())
            .unwrap_or_else(|| panic!("the actual {scope} toolbar control {action} is present"))
    }
    fn wait_display_picker(app: &mut StudioApp, context: &egui::Context) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let _ = at_frame(app, context, 0, vec![]);
            if matches!(
                &app.dialog,
                Some(Dialog::DisplayCapture { loading: false, .. })
            ) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "display enumeration completion was not consumed"
            );
            std::thread::yield_now();
        }
    }
    fn wait_capture_texture(
        app: &mut StudioApp,
        context: &egui::Context,
        now: u64,
        color: [u8; 4],
        program: bool,
    ) {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let mut uploaded = false;
        loop {
            let output = at_frame(app, context, now, vec![]);
            uploaded |= output
                .textures_delta
                .set
                .iter()
                .any(|(_, delta)| match &delta.image {
                    egui::ImageData::Color(image) => {
                        image.size == [360, 360]
                            && image
                                .pixels
                                .first()
                                .is_some_and(|pixel| pixel.to_array() == color)
                            && image
                                .pixels
                                .last()
                                .is_some_and(|pixel| pixel.to_array() == color)
                    }
                    _ => false,
                });
            let pane = if program { &app.program } else { &app.preview };
            if uploaded
                && pane
                    .displayed_pixel()
                    .is_some_and(|(pixel, _)| pixel == color)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "expected fake capture texture {color:?}; status {}, capture {:?}",
                app.status,
                app.capture_error
            );
            std::thread::yield_now();
        }
    }
    #[test]
    fn capture_transform_dialog_uses_live_pixel_extents_with_no_op_cancel_undo_and_resize_rejection()
     {
        let context = egui::Context::default();
        let state = FakeCaptureState::default();
        state.wide.store(true, Ordering::SeqCst);
        let mut app =
            StudioApp::with_capture_backend(&context, Box::new(FakeCapture(state.clone())))
                .unwrap();
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: true,
            mixer: false,
            transitions: false,
            controls: false,
        };
        app.intent(
            Intent::Command(Command::SetVideo {
                width: 32,
                height: 16,
                fps_num: 30,
                fps_den: 1,
            }),
            &context,
        );
        let scene = app.session.preview();
        let binding = skylive_core::DisplayCaptureBinding::new(
            skylive_core::DisplayTarget::Windows {
                device_id: "fake-a".into(),
            },
            true,
            2,
            2,
        )
        .unwrap();
        app.intent(
            Intent::Command(Command::AddDisplayCaptureSource {
                scene,
                name: "Live transform".into(),
                binding,
            }),
            &context,
        );
        let item = app.selected.unwrap();
        let source = app.session.snapshot().sources()[0].id();
        app.intent(
            Intent::Command(Command::SetTransform {
                scene,
                item,
                transform: skylive_core::Transform::full(16, 16),
            }),
            &context,
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while app.capture.snapshot().frame(source).is_none() {
            let _ = at_frame(&mut app, &context, 0, vec![]);
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(
            app.capture.snapshot().frame(source).unwrap().image.width(),
            4
        );
        let original = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Transform");
        assert!(
            matches!(&app.dialog,Some(Dialog::Transform {draft,capture_context:Some(_),..}) if draft.width==32.0&&draft.height==16.0)
        );
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        assert!(
            Arc::ptr_eq(&original, &app.session.snapshot()),
            "unchanged projected fields must preserve canonical f32 values and history"
        );
        capture_click(&mut app, &context, 0, "Transform");
        if let Some(Dialog::Transform { draft, .. }) = &mut app.dialog {
            draft.width = 40.0;
        }
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "Transform");
        if let Some(Dialog::Transform { draft, .. }) = &mut app.dialog {
            draft.width = 16.0;
        }
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        let edited = app.session.snapshot();
        assert_eq!(edited.revision(), original.revision() + 1);
        let transform = edited.scene(scene).unwrap().items()[0].transform();
        assert_eq!((transform.width, transform.height), (8.0, 16.0));
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let mut uploaded = false;
        while !uploaded {
            let output = at_frame(&mut app, &context, 0, vec![]);
            uploaded = output
                .textures_delta
                .set
                .iter()
                .any(|(_, delta)| match &delta.image {
                    egui::ImageData::Color(image) => {
                        image.size == [640, 320]
                            && image
                                .pixels
                                .first()
                                .is_some_and(|pixel| pixel.to_array() == [255, 0, 0, 255])
                            && image
                                .pixels
                                .last()
                                .is_some_and(|pixel| pixel.to_array() == [0, 0, 0, 255])
                    }
                    _ => false,
                });
            assert!(
                Instant::now() < deadline,
                "world-width edit was not presented as a half-width live capture texture"
            );
            std::thread::yield_now();
        }
        app.intent(Intent::Command(Command::Undo), &context);
        assert_eq!(
            app.session.snapshot().scene(scene).unwrap().items()[0]
                .transform()
                .width,
            16.0
        );
        capture_click(&mut app, &context, 0, "Transform");
        let before_resize = app.session.snapshot();
        assert!(matches!(&app.dialog,Some(Dialog::Transform {draft,..}) if draft.width==32.0));
        state.wide.store(false, Ordering::SeqCst);
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while app.capture.snapshot().frame(source).unwrap().image.width() != 2 {
            let _ = at_frame(&mut app, &context, 100_000_000, vec![]);
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        capture_click(&mut app, &context, 100_000_000, "Apply");
        assert!(
            app.error,
            "changed capture dimensions require reopening the draft"
        );
        assert!(
            matches!(&app.dialog,Some(Dialog::Transform {draft,capture_context:Some(_),..}) if draft.width==32.0)
        );
        assert!(Arc::ptr_eq(&before_resize, &app.session.snapshot()));
        assert_eq!(
            state.enumerations.load(Ordering::SeqCst),
            0,
            "a bound fake source does not enumerate or access any native display"
        );
    }
    #[test]
    fn display_widgets_use_only_injected_backend_rebind_undo_and_keep_frozen_program_live() {
        let context = egui::Context::default();
        let state = FakeCaptureState::default();
        let mut app =
            StudioApp::with_capture_backend(&context, Box::new(FakeCapture(state.clone())))
                .unwrap();
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: true,
            mixer: false,
            transitions: false,
            controls: false,
        };
        app.intent(
            Intent::Command(Command::SetVideo {
                width: 16,
                height: 16,
                fps_num: 30,
                fps_den: 1,
            }),
            &context,
        );
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let _ = at_frame(&mut app, &context, 0, vec![]);
        assert_eq!(state.enumerations.load(Ordering::SeqCst), 0);
        assert_eq!(state.frames.load(Ordering::SeqCst), 0);
        let before = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Add");
        capture_click(&mut app, &context, 0, "Display Capture");
        wait_display_picker(&mut app, &context);
        assert!(
            matches!(&app.dialog,Some(Dialog::DisplayCapture {displays,selected:Some(_),error:None,..}) if displays.len()==2)
        );
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        assert_eq!(state.frames.load(Ordering::SeqCst), 0);
        capture_click(&mut app, &context, 0, "Add");
        capture_click(&mut app, &context, 0, "Display Capture");
        wait_display_picker(&mut app, &context);
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        assert!(app.dialog.is_none());
        let added = app.session.snapshot();
        let source = added.sources()[0].id();
        let scene = app.session.preview();
        let item = app.selected.unwrap();
        assert_eq!(added.revision(), before.revision() + 1);
        let action = Command::TransformItem {
            scene,
            item,
            action: skylive_core::TransformAction::FitToCanvas,
        };
        app.apply_at(Some(added.revision() - 1), action);
        assert_eq!(
            app.status,
            skylive_core::Error::Conflict {
                expected: added.revision() - 1,
                actual: added.revision()
            }
            .to_string()
        );
        assert!(Arc::ptr_eq(&added, &app.session.snapshot()));
        app.apply(Command::TransformItem {
            scene,
            item,
            action: skylive_core::TransformAction::FitToCanvas,
        });
        assert!(
            app.error,
            "live transform action must wait for its matching frame"
        );
        assert!(Arc::ptr_eq(&added, &app.session.snapshot()));
        // The fake backend is deliberately 2×2; scale its authored item to the
        // smallest valid 16×16 canvas so both uploaded texture corners sample it.
        app.intent(
            Intent::Command(Command::SetTransform {
                scene,
                item,
                transform: skylive_core::Transform::full(16, 16),
            }),
            &context,
        );
        let added = app.session.snapshot();
        wait_capture_texture(&mut app, &context, 0, [255, 0, 0, 255], false);
        let first_generation = app.capture.snapshot().generation();
        let frame_count = state.frames.load(Ordering::SeqCst);
        let _ = at_frame(&mut app, &context, 0, vec![]);
        assert_eq!(
            state.frames.load(Ordering::SeqCst),
            frame_count,
            "a frozen explicit timestamp does not continuously grab frames"
        );
        capture_click(&mut app, &context, 0, "Properties");
        wait_display_picker(&mut app, &context);
        capture_click(&mut app, &context, 0, "Capture cursor");
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&added, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "Properties");
        wait_display_picker(&mut app, &context);
        capture_click(&mut app, &context, 0, "Capture cursor");
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        let rebound = app.session.snapshot();
        assert_eq!(rebound.revision(), added.revision() + 1);
        assert!(
            matches!(rebound.source(source).unwrap().kind(),skylive_core::SourceKind::DisplayCapture {binding} if !binding.capture_cursor())
        );
        wait_capture_texture(&mut app, &context, 0, [0, 255, 0, 255], false);
        assert!(app.capture.snapshot().generation() > first_generation);
        app.intent(Intent::Command(Command::Undo), &context);
        wait_capture_texture(&mut app, &context, 0, [255, 0, 0, 255], false);
        assert!(
            matches!(app.session.snapshot().source(source).unwrap().kind(),skylive_core::SourceKind::DisplayCapture {binding} if binding.capture_cursor())
        );
        app.intent(Intent::Command(Command::Redo), &context);
        wait_capture_texture(&mut app, &context, 0, [0, 255, 0, 255], false);
        app.intent(Intent::StudioMode(true), &context);
        app.intent(
            Intent::Command(Command::RemoveItem { scene, item }),
            &context,
        );
        assert!(app.session.snapshot().source(source).is_none());
        state.yellow.store(true, Ordering::SeqCst);
        wait_capture_texture(&mut app, &context, 100_000_000, [255, 255, 0, 255], true);
        assert!(
            app.capture.snapshot().frame(source).is_some(),
            "the private Program layout remains an active capture owner after its last authored reference is removed"
        );
        assert!(!app.transition_active);
    }
    #[test]
    fn display_picker_rejects_obsolete_async_revision_without_creating_a_source() {
        let context = egui::Context::default();
        let state = FakeCaptureState::default();
        state.hold_enumeration.store(true, Ordering::SeqCst);
        let mut app =
            StudioApp::with_capture_backend(&context, Box::new(FakeCapture(state.clone())))
                .unwrap();
        let scene = app.session.preview();
        app.intent(
            Intent::OpenDisplayCapture {
                edit: DisplayEdit::Add { scene },
                name: "Pending Display".into(),
                binding: None,
            },
            &context,
        );
        let _ = at_frame(&mut app, &context, 0, vec![]);
        assert!(app.capture.is_busy());
        // Simulate a second canonical gateway publishing while enumeration is
        // in flight; this intentionally bypasses the modal's input guard.
        app.session
            .apply(Command::RenameCollection {
                name: "New revision".into(),
            })
            .unwrap();
        let changed = app.session.snapshot();
        state.hold_enumeration.store(false, Ordering::SeqCst);
        wait_display_picker(&mut app, &context);
        assert!(
            matches!(&app.dialog,Some(Dialog::DisplayCapture {error:Some(error),displays,..}) if error.contains("document changed")&&displays.is_empty())
        );
        capture_click(&mut app, &context, 0, "Apply");
        assert!(app.error);
        assert!(app.dialog.is_some());
        assert!(Arc::ptr_eq(&changed, &app.session.snapshot()));
        assert!(app.session.snapshot().sources().is_empty());
        assert_eq!(state.frames.load(Ordering::SeqCst), 0);
    }
    // AccessKit invokes widget_info under egui's Context lock. Run this exact
    // path in a child so a future lock regression fails with a bounded timeout.
    #[test]
    fn accesskit_metadata_child() {
        if std::env::var_os("SKYLIVE_ACCESSKIT_TEST_CHILD").is_none() {
            return;
        }
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut app = StudioApp::headless(&context).unwrap();
        let scene = app.session.preview();
        app.session
            .apply(Command::AddColorSource {
                scene,
                name: "Accessibility fixture".into(),
                rgba: [255; 4],
            })
            .unwrap();
        app.session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: "Accessibility audio fixture".into(),
                binding: skylive_core::AudioCaptureBinding::new(
                    skylive_core::AudioFlow::Input,
                    skylive_core::AudioTarget::Default,
                    false,
                    false,
                )
                .unwrap(),
            })
            .unwrap();
        let output = at_frame(&mut app, &context, 0, vec![]);
        assert!(
            output.platform_output.accesskit_update.is_some(),
            "the fixture must execute AccessKit builders, not just ordinary paint"
        );
        assert!(text_position(&output, "Audio Mixer").is_some());
    }
    #[test]
    fn localized_preview_and_mixer_accessibility_complete_without_context_reentrancy() {
        use std::process::{Command as ChildCommand, Stdio};
        let mut command = ChildCommand::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "app::tests::accesskit_metadata_child",
                "--nocapture",
            ])
            .env("SKYLIVE_ACCESSKIT_TEST_CHILD", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "AccessKit frame exceeded 10s; Context reentrancy may have returned: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn injected_audio_device_picker_captures_real_test_pcm_and_shared_postgain_meters() {
        use skylive_core::{AudioSlot, AudioTarget};
        let context = egui::Context::default();
        let state = FakeAudioState::default();
        let mut app = fake_audio_app(&context, state.clone());
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: true,
            mixer: false,
            transitions: false,
            controls: false,
        };
        let _ = at_frame(&mut app, &context, 0, vec![]);
        assert_eq!(state.factories.load(Ordering::SeqCst), 0);
        capture_click(&mut app, &context, 0, "Add");
        capture_click(&mut app, &context, 0, "Audio Input Capture");
        assert_eq!(
            state.opens.load(Ordering::SeqCst),
            0,
            "opening the picker must never start audio capture"
        );
        wait_audio_picker(&mut app, &context);
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
        assert_eq!(state.enumerations.load(Ordering::SeqCst), 1);
        assert_eq!(state.opens.load(Ordering::SeqCst), 0);
        let listed = at_frame(&mut app, &context, 0, vec![]);
        assert!(
            text_position(&listed, "Default device · Fake Audio Device").is_some(),
            "the closed device picker exposes the resolved default name"
        );
        capture_click(&mut app, &context, 0, "Default device · Fake Audio Device");
        capture_click(&mut app, &context, 0, "Fake Audio Device");
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        let source = app.session.snapshot().sources()[0].id();
        let scene = app.session.preview();
        assert!(
            matches!(app.session.snapshot().source(source).unwrap().kind(),skylive_core::SourceKind::AudioCapture {binding} if binding.target()==&AudioTarget::WindowsEndpoint {id:"fake-audio-endpoint".into()})
        );
        app.intent(
            Intent::Command(Command::AddExistingSource { scene, source }),
            &context,
        );
        app.intent(
            Intent::Command(Command::SetGlobalAudio {
                slot: AudioSlot::Mic1,
                source: Some(source),
            }),
            &context,
        );
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: false,
            mixer: true,
            transitions: false,
            controls: false,
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let _ = at_frame(&mut app, &context, 0, vec![]);
            if app.audio.snapshot().packet(source).is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "fake PCM did not reach audio snapshot"
            );
            std::thread::yield_now();
        }
        assert_eq!(
            app.audio
                .snapshot()
                .packet(source)
                .unwrap()
                .resolved_device
                .as_deref(),
            Some("Fake Audio Device")
        );
        // Compact dB meters keep technical channel values in hover details;
        // assert the actual rendered meter inputs beside reachable controls.
        let output = scroll_audio_text(&mut app, &context, "Mute", 1.0);
        assert!(text_position(&output, "Mute").is_some());
        assert!(text_position(&output, "Ch 1 peak 0.500 · RMS 0.500").is_none());
        assert_eq!(
            context.data_mut(|data| data.get_temp::<(f64, f64)>(egui::Id::new((
                "audio-meter",
                source.get(),
                0usize
            )))),
            Some((0.5, 0.5))
        );
        assert_eq!(app.audio.active_source_count(), 1);
        assert_eq!(state.opens.load(Ordering::SeqCst), 1);
        app.intent(
            Intent::Command(Command::SetAudio {
                source,
                gain: 0.5,
                muted: false,
            }),
            &context,
        );
        let _ = at_frame(&mut app, &context, 0, vec![]);
        assert_eq!(
            context.data_mut(|data| data.get_temp::<(f64, f64)>(egui::Id::new((
                "audio-meter",
                source.get(),
                0usize
            )))),
            Some((0.25, 0.25))
        );
        let _ = scroll_audio_text(&mut app, &context, "Mute", 1.0);
        capture_click(&mut app, &context, 0, "Mute");
        let _ = at_frame(&mut app, &context, 0, vec![]);
        assert_eq!(
            context.data_mut(|data| data.get_temp::<(f64, f64)>(egui::Id::new((
                "audio-meter",
                source.get(),
                0usize
            )))),
            Some((0.0, 0.0))
        );
        app.intent(Intent::Command(Command::Undo), &context);
        let _ = at_frame(&mut app, &context, 0, vec![]);
        assert_eq!(
            context.data_mut(|data| data.get_temp::<(f64, f64)>(egui::Id::new((
                "audio-meter",
                source.get(),
                0usize
            )))),
            Some((0.25, 0.25))
        );
        let items = app
            .session
            .snapshot()
            .scene(scene)
            .unwrap()
            .items()
            .iter()
            .map(|item| item.id())
            .collect::<Vec<_>>();
        for item in items {
            app.intent(
                Intent::Command(Command::SetVisible {
                    scene,
                    item,
                    visible: false,
                }),
                &context,
            );
        }
        app.intent(
            Intent::Command(Command::SetGlobalAudio {
                slot: AudioSlot::Mic1,
                source: None,
            }),
            &context,
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while state.closes.load(Ordering::SeqCst) == 0 {
            let _ = at_frame(&mut app, &context, 20_000_000, vec![]);
            assert!(
                Instant::now() < deadline,
                "empty audio scope did not stop the fake stream"
            );
            std::thread::yield_now();
        }
        assert_eq!(app.audio.active_source_count(), 0);
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn audio_device_picker_rejects_obsolete_revision_and_keeps_the_draft() {
        let context = egui::Context::default();
        let state = FakeAudioState::default();
        state.hold_enumeration.store(true, Ordering::SeqCst);
        let mut app = fake_audio_app(&context, state.clone());
        let scene = app.session.preview();
        app.intent(
            Intent::OpenDialog(Dialog::audio(
                views::AudioEdit::Add { scene },
                "Pending Microphone".into(),
                skylive_core::AudioFlow::Input,
            )),
            &context,
        );
        capture_click(&mut app, &context, 0, "List Native Devices");
        let _ = at_frame(&mut app, &context, 0, vec![]);
        assert!(app.audio.is_busy());
        app.session
            .apply(Command::RenameCollection {
                name: "Other authored revision".into(),
            })
            .unwrap();
        let changed = app.session.snapshot();
        state.hold_enumeration.store(false, Ordering::SeqCst);
        wait_audio_picker(&mut app, &context);
        assert!(
            matches!(&app.dialog,Some(Dialog::AudioCapture {device_picker:Some(picker),..}) if picker.devices.is_empty()&&picker.error.as_ref().is_some_and(|error|error.contains("document changed")))
        );
        assert!(Arc::ptr_eq(&changed, &app.session.snapshot()));
        assert!(app.session.snapshot().sources().is_empty());
        assert_eq!(state.opens.load(Ordering::SeqCst), 0);
        capture_click(&mut app, &context, 0, "Apply");
        assert!(app.error);
        assert!(app.dialog.is_some());
        assert!(Arc::ptr_eq(&changed, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "List Native Devices");
        let _ = at_frame(&mut app, &context, 0, vec![]);
        assert!(
            !app.audio.is_busy(),
            "an obsolete modal must not re-enumerate indefinitely"
        );
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(app.dialog.is_none());
    }
    #[test]
    fn cancelled_audio_picker_completion_does_not_replace_a_new_modal() {
        let context = egui::Context::default();
        let state = FakeAudioState::default();
        state.hold_enumeration.store(true, Ordering::SeqCst);
        let mut app = fake_audio_app(&context, state.clone());
        let scene = app.session.preview();
        app.intent(
            Intent::OpenDialog(Dialog::audio(
                views::AudioEdit::Add { scene },
                "First Input".into(),
                skylive_core::AudioFlow::Input,
            )),
            &context,
        );
        capture_click(&mut app, &context, 0, "List Native Devices");
        let _ = at_frame(&mut app, &context, 0, vec![]);
        capture_click(&mut app, &context, 0, "Cancel");
        app.intent(
            Intent::OpenDialog(Dialog::audio(
                views::AudioEdit::Add { scene },
                "Second Output".into(),
                skylive_core::AudioFlow::Output,
            )),
            &context,
        );
        state.hold_enumeration.store(false, Ordering::SeqCst);
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while app.audio.is_busy()
            || matches!(&app.dialog,Some(Dialog::AudioCapture{device_picker:Some(picker),..}) if picker.loading)
        {
            let _ = at_frame(&mut app, &context, 0, vec![]);
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(
            matches!(&app.dialog,Some(Dialog::AudioCapture {name,flow:skylive_core::AudioFlow::Output,device_picker:Some(picker),..}) if name=="Second Output"&&picker.devices.len()==1&&picker.devices[0].flow==skylive_core::AudioFlow::Output&&!picker.loading&&picker.error.is_none())
        );
        assert!(app.session.snapshot().sources().is_empty());
        assert_eq!(state.opens.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn audio_binding_widgets_stage_validate_rebind_and_disable_video_geometry() {
        use skylive_core::{AudioFlow, AudioTarget, SourceKind};
        let context = egui::Context::default();
        let state = FakeAudioState::default();
        let mut app = fake_audio_app(&context, state.clone());
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: true,
            mixer: false,
            transitions: false,
            controls: false,
        };
        let original = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Add");
        capture_click(&mut app, &context, 0, "Audio Input Capture");
        wait_audio_picker(&mut app, &context);
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        assert_eq!(state.opens.load(Ordering::SeqCst), 0);
        capture_click(&mut app, &context, 0, "Add");
        capture_click(&mut app, &context, 0, "Audio Input Capture");
        wait_audio_picker(&mut app, &context);
        capture_click(&mut app, &context, 0, "Default device · Fake Audio Device");
        let output = at_frame(&mut app, &context, 0, vec![]);
        assert!(visible_text_positions(&output, "macOS system audio").is_empty());
        assert!(visible_text_positions(&output, "macOS device").is_empty());
        capture_click(&mut app, &context, 0, "Fake Audio Device");
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        let added = app.session.snapshot();
        let source = added.sources()[0].id();
        let item = app.selected.unwrap();
        assert!(
            matches!(added.sources()[0].kind(),SourceKind::AudioCapture{binding} if binding.flow()==AudioFlow::Input&&binding.target()==&AudioTarget::WindowsEndpoint{id:"fake-audio-endpoint".into()})
        );
        assert_eq!(added.sources()[0].kind().video_dimensions(), None);
        for action in [
            "actionEditTransform",
            "actionResetTransform",
            "actionFitToScreen",
            "actionStretchToScreen",
            "actionCenterToScreen",
            "actionRotate90CW",
            "actionFlipVertical",
        ] {
            assert!(views::menus::action_intent(action, &app.session, Some(item), false).is_none());
        }
        capture_click(&mut app, &context, 0, "Properties");
        wait_audio_picker(&mut app, &context);
        if let Some(Dialog::AudioCapture { identity, .. }) = &mut app.dialog {
            *identity = "endpoint://cancelled".into();
        }
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&added, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "Properties");
        wait_audio_picker(&mut app, &context);
        if let Some(Dialog::AudioCapture { identity, .. }) = &mut app.dialog {
            *identity = "endpoint://rebound".into();
        }
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        assert_eq!(app.session.snapshot().revision(), added.revision() + 1);
        app.intent(Intent::Command(Command::Undo), &context);
        assert_eq!(
            app.session.snapshot().source(source).unwrap().kind(),
            added.source(source).unwrap().kind()
        );
        capture_click(&mut app, &context, 0, "Add");
        capture_click(&mut app, &context, 0, "Audio Output Capture");
        wait_audio_picker(&mut app, &context);
        capture_click(&mut app, &context, 0, "Default device · Fake Audio Device");
        capture_click(&mut app, &context, 0, "Fake Audio Device");
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        assert!(app.session.snapshot().sources().iter().any(|source|matches!(source.kind(),SourceKind::AudioCapture{binding} if binding.flow()==AudioFlow::Output&&binding.target()==&AudioTarget::WindowsEndpoint{id:"fake-audio-endpoint".into()}&&binding.use_device_timing())));
    }
    #[test]
    fn only_matching_explicit_camera_stop_can_replace_a_document_status() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let key = CameraRequestKey {
            instance: 1,
            session: app.session.identity(),
            revision: 0,
            epoch: 1,
            serial: 1,
        };
        app.message(false, "Loaded scene collection");
        app.accept_camera_stop(key);
        assert_eq!(app.status, "Loaded scene collection");
        app.explicit_camera_stop = Some(key);
        app.accept_camera_stop(CameraRequestKey { serial: 2, ..key });
        assert_eq!(app.status, "Loaded scene collection");
        app.accept_camera_stop(key);
        assert_eq!(app.status, "Camera streams stopped");
        assert!(app.explicit_camera_stop.is_none());
        app.message(false, "Loaded another collection");
        app.accept_camera_stop(key);
        assert_eq!(app.status, "Loaded another collection");
    }

    #[test]
    fn audio_mixer_commits_one_fader_edit_shares_mute_and_keeps_numeric_cancel_drafts() {
        use skylive_core::{AudioCaptureBinding, AudioFlow, AudioSlot, AudioTarget};
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let scene = app.session.preview();
        let binding =
            AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false).unwrap();
        app.intent(
            Intent::Command(Command::AddAudioCaptureSource {
                scene,
                name: "Shared Microphone".into(),
                binding,
            }),
            &context,
        );
        let source = app.session.snapshot().sources()[0].id();
        app.intent(
            Intent::Command(Command::AddExistingSource { scene, source }),
            &context,
        );
        app.intent(
            Intent::Command(Command::SetGlobalAudio {
                slot: AudioSlot::Mic1,
                source: Some(source),
            }),
            &context,
        );
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: false,
            mixer: true,
            transitions: false,
            controls: false,
        };
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        assert_eq!(
            visible_text_positions(&output, "Shared Microphone").len(),
            1,
            "scene and global references share one mixer row"
        );
        assert!(text_position(&output, "Signal unavailable · monitoring off").is_some());
        let original = app.session.snapshot();
        let rect = context
            .data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("audio-fader", source.get())))
            })
            .unwrap();
        let start = rect.center();
        let target = egui::pos2(rect.left() + rect.width() * 0.75, rect.center().y);
        let _ = at_frame(
            &mut app,
            &context,
            0,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let _ = at_frame(
            &mut app,
            &context,
            0,
            vec![egui::Event::PointerMoved(target)],
        );
        assert!(
            Arc::ptr_eq(&original, &app.session.snapshot()),
            "an in-progress fader gesture is only presentation state"
        );
        let _ = at_frame(
            &mut app,
            &context,
            0,
            vec![egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let edited = app.session.snapshot();
        assert_eq!(edited.revision(), original.revision() + 1);
        assert!((0.0..1.0).contains(&edited.source(source).unwrap().gain()));
        assert!(!edited.source(source).unwrap().muted());
        app.intent(Intent::Command(Command::Undo), &context);
        assert_eq!(app.session.snapshot().source(source).unwrap().gain(), 1.0);
        capture_click(&mut app, &context, 0, "Mute");
        assert!(app.session.snapshot().source(source).unwrap().muted());
        assert_eq!(app.session.snapshot().source(source).unwrap().gain(), 1.0);
        app.intent(Intent::Command(Command::Undo), &context);
        assert!(!app.session.snapshot().source(source).unwrap().muted());
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let rect = context
            .data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("audio-gain", source.get())))
            })
            .unwrap();
        let _ = click_at(&mut app, &context, 0, rect.center());
        let before_numeric = app.session.snapshot();
        let _ = at_frame(
            &mut app,
            &context,
            0,
            vec![egui::Event::Text("-6.0206".into())],
        );
        assert!(Arc::ptr_eq(&before_numeric, &app.session.snapshot()));
        let _ = at_frame(
            &mut app,
            &context,
            0,
            vec![
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let half = app.session.snapshot();
        assert!(
            (half.source(source).unwrap().gain() - 0.5).abs() < 0.000_001,
            "numeric gain {} revision {} before {} focus {:?}",
            half.source(source).unwrap().gain(),
            half.revision(),
            before_numeric.revision(),
            context.memory(|m| m.focused())
        );
        assert_eq!(half.revision(), before_numeric.revision() + 1);
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let rect = context
            .data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("audio-gain", source.get())))
            })
            .unwrap();
        let _ = click_at(&mut app, &context, 0, rect.center());
        let _ = at_frame(&mut app, &context, 0, vec![egui::Event::Text("3".into())]);
        let _ = at_frame(
            &mut app,
            &context,
            0,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(Arc::ptr_eq(&half, &app.session.snapshot()));
    }
    #[test]
    fn global_audio_slot_menu_creates_one_staged_root_without_a_video_item() {
        use skylive_core::{AudioSlot, SourceKind};
        let context = egui::Context::default();
        let mut app = fake_audio_app(&context, FakeAudioState::default());
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: false,
            mixer: true,
            transitions: false,
            controls: false,
        };
        let original = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Global Audio Slots");
        capture_click(&mut app, &context, 0, "Mic/Aux 1");
        capture_click(&mut app, &context, 0, "Add New Audio Source");
        assert!(matches!(
            &app.dialog,
            Some(Dialog::AudioCapture {
                edit: views::AudioEdit::AddGlobal {
                    slot: AudioSlot::Mic1
                },
                ..
            })
        ));
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "Global Audio Slots");
        capture_click(&mut app, &context, 0, "Mic/Aux 1");
        capture_click(&mut app, &context, 0, "Add New Audio Source");
        wait_audio_picker(&mut app, &context);
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        let added = app.session.snapshot();
        assert_eq!(added.revision(), original.revision() + 1);
        assert_eq!(added.global_audio().len(), 1);
        let source = added.global_audio()[0].source();
        assert!(matches!(
            added.source(source).unwrap().kind(),
            SourceKind::AudioCapture { .. }
        ));
        assert!(
            added
                .scene(app.session.preview())
                .unwrap()
                .items()
                .is_empty()
        );
        app.intent(Intent::Command(Command::Undo), &context);
        assert!(app.session.snapshot().global_audio().is_empty());
        assert!(app.session.snapshot().source(source).is_none());
    }
    #[test]
    fn audio_settings_pages_stage_validate_cancel_and_publish_one_format_edit_without_rebinding_sources()
     {
        use skylive_core::{AudioCaptureBinding, AudioFlow, AudioSlot, AudioTarget, SpeakerLayout};
        fn open(app: &mut StudioApp, context: &egui::Context) {
            let intent =
                views::menus::action_intent("action_Settings", &app.session, None, false).unwrap();
            app.intent(intent, context);
            capture_click(app, context, 0, "Audio");
            assert!(matches!(app.dialog, Some(Dialog::AudioSettings { .. })));
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let scene = app.session.preview();
        let binding = AudioCaptureBinding::new(
            AudioFlow::Input,
            AudioTarget::WindowsEndpoint {
                id: "settings-must-not-rebind".into(),
            },
            true,
            false,
        )
        .unwrap();
        app.intent(
            Intent::Command(Command::AddAudioCaptureSource {
                scene,
                name: "Stable Binding".into(),
                binding: binding.clone(),
            }),
            &context,
        );
        let source = app.session.snapshot().sources()[0].id();
        app.intent(
            Intent::Command(Command::SetAudio {
                source,
                gain: 0.75,
                muted: true,
            }),
            &context,
        );
        app.intent(
            Intent::Command(Command::SetGlobalAudio {
                slot: AudioSlot::Mic1,
                source: Some(source),
            }),
            &context,
        );
        let original = app.session.snapshot();
        open(&mut app, &context);
        assert!(matches!(
            &app.dialog,
            Some(Dialog::AudioSettings {
                sample_rate: 48000,
                speakers: SpeakerLayout::Stereo,
                ..
            })
        ));
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        for label in [
            "General",
            "Appearance",
            "Stream",
            "Output",
            "Audio",
            "Video",
            "Hotkeys",
            "Accessibility",
            "Advanced",
        ] {
            assert!(
                text_position(&output, label).is_some(),
                "Settings keeps page {label}"
            );
        }
        capture_click(&mut app, &context, 0, "Apply");
        assert!(
            Arc::ptr_eq(&original, &app.session.snapshot()),
            "unchanged audio settings Apply preserves history and snapshot"
        );
        open(&mut app, &context);
        capture_click(&mut app, &context, 0, "48 kHz");
        capture_click(&mut app, &context, 0, "44.1 kHz");
        capture_click(&mut app, &context, 0, "Stereo");
        capture_click(&mut app, &context, 0, "5.1");
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        assert!(matches!(
            &app.dialog,
            Some(Dialog::AudioSettings {
                sample_rate: 44100,
                speakers: SpeakerLayout::FivePointOne,
                ..
            })
        ));
        capture_click(&mut app, &context, 0, "Video");
        assert!(
            matches!(
                &app.dialog,
                Some(Dialog::AudioSettings {
                    sample_rate: 44100,
                    speakers: SpeakerLayout::FivePointOne,
                    ..
                })
            ),
            "page navigation cannot discard an unapplied draft"
        );
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        open(&mut app, &context);
        if let Some(Dialog::AudioSettings { sample_rate, .. }) = &mut app.dialog {
            *sample_rate = 32000;
        }
        capture_click(&mut app, &context, 0, "Apply");
        assert!(app.error);
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        assert!(matches!(
            app.dialog,
            Some(Dialog::AudioSettings {
                sample_rate: 32000,
                ..
            })
        ));
        capture_click(&mut app, &context, 0, "Unsupported rate: 32000 Hz");
        capture_click(&mut app, &context, 0, "44.1 kHz");
        capture_click(&mut app, &context, 0, "Stereo");
        capture_click(&mut app, &context, 0, "7.1");
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        let changed = app.session.snapshot();
        assert_eq!(changed.revision(), original.revision() + 1);
        assert_eq!(changed.audio().sample_rate(), 44100);
        assert_eq!(changed.audio().speakers(), SpeakerLayout::SevenPointOne);
        assert_eq!(changed.sources(), original.sources());
        assert_eq!(changed.global_audio(), original.global_audio());
        assert_eq!(changed.source(source).unwrap().gain(), 0.75);
        assert!(changed.source(source).unwrap().muted());
        open(&mut app, &context);
        capture_click(&mut app, &context, 0, "Apply");
        assert!(Arc::ptr_eq(&changed, &app.session.snapshot()));
        app.intent(
            Intent::Command(Command::SetAudioSettings {
                settings: original.audio(),
            }),
            &context,
        );
        let undone = app.session.snapshot();
        assert_eq!(undone.audio(), original.audio());
        assert_eq!(undone.sources(), original.sources());
        assert_eq!(undone.global_audio(), original.global_audio());
    }
    #[test]
    fn typed_audio_defaults_initialize_global_output_without_overwriting_rebind_flags() {
        use skylive_core::{AudioCaptureBinding, AudioFlow, AudioSlot, AudioTarget};
        let context = egui::Context::default();
        let mut app = fake_audio_app(&context, FakeAudioState::default());
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: false,
            mixer: true,
            transitions: false,
            controls: false,
        };
        let original = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Global Audio Slots");
        capture_click(&mut app, &context, 0, "Desktop Audio 1");
        capture_click(&mut app, &context, 0, "Add New Audio Source");
        assert!(matches!(
            &app.dialog,
            Some(Dialog::AudioCapture {
                edit: views::AudioEdit::AddGlobal {
                    slot: AudioSlot::Desktop1
                },
                flow: AudioFlow::Output,
                use_device_timing: true,
                exclude_own_audio: false,
                ..
            })
        ));
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        let scene = app.session.preview();
        let binding =
            AudioCaptureBinding::new(AudioFlow::Output, AudioTarget::Default, false, true).unwrap();
        app.intent(
            Intent::Command(Command::AddAudioCaptureSource {
                scene,
                name: "Authored Timing".into(),
                binding: binding.clone(),
            }),
            &context,
        );
        let source = app.session.snapshot().sources()[0].id();
        app.intent(
            Intent::OpenDialog(Dialog::audio_properties(
                source,
                "Authored Timing".into(),
                &binding,
            )),
            &context,
        );
        assert!(
            matches!(&app.dialog,Some(Dialog::AudioCapture {edit:views::AudioEdit::Rebind {source:actual},use_device_timing:false,exclude_own_audio:true,..}) if *actual==source)
        );
        let before = app.session.snapshot();
        wait_audio_picker(&mut app, &context);
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        assert!(
            Arc::ptr_eq(&before, &app.session.snapshot()),
            "Properties must retain authored flags as a no-op even when host new-source defaults differ"
        );
    }
    #[test]
    fn continuous_program_popup_uses_injected_pcm_excludes_preview_and_stops_the_same_owner() {
        use skylive_core::{AudioCaptureBinding, AudioFlow, AudioSlot, AudioTarget};
        let context = egui::Context::default();
        let state = FakeAudioState::default();
        state.program_packets.store(16, Ordering::SeqCst);
        let mut app = fake_audio_app(&context, state.clone());
        let first = app.session.preview();
        app.intent(
            Intent::Command(Command::SetTransition {
                kind: skylive_core::TransitionKind::Cut,
                duration_ms: 300,
            }),
            &context,
        );
        let binding =
            AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, true, false).unwrap();
        app.intent(
            Intent::Command(Command::AddAudioCaptureSource {
                scene: first,
                name: "Program source".into(),
                binding: binding.clone(),
            }),
            &context,
        );
        let program_source = app.session.snapshot().sources()[0].id();
        app.intent(
            Intent::Command(Command::SetGlobalAudio {
                slot: AudioSlot::Mic1,
                source: Some(program_source),
            }),
            &context,
        );
        app.intent(
            Intent::Command(Command::AddScene {
                name: "Preview only".into(),
            }),
            &context,
        );
        let second = app.session.preview();
        app.intent(
            Intent::Command(Command::AddAudioCaptureSource {
                scene: second,
                name: "Preview source".into(),
                binding,
            }),
            &context,
        );
        app.intent(Intent::SelectScene(first), &context);
        app.intent(Intent::StudioMode(true), &context);
        app.intent(Intent::SelectScene(second), &context);
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: false,
            mixer: true,
            transitions: false,
            controls: false,
        };
        assert_eq!(state.factories.load(Ordering::SeqCst), 0);
        let origin = Instant::now();
        let deadline = origin + std::time::Duration::from_secs(5);
        let mut observed_now;
        loop {
            observed_now = u64::try_from(origin.elapsed().as_nanos()).unwrap();
            let _ = at_frame(&mut app, &context, observed_now, vec![]);
            if app.program_audio.as_ref().is_some_and(|view| {
                view.meters.len() == 2 && view.meters[0].peak == 0.5 && view.meters[1].peak == 0.25
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "routed fake PCM did not produce Program meters: {:?}",
                app.audio_error
            );
            std::thread::yield_now();
        }
        let stats = app.audio.program_status().unwrap();
        assert!(stats.running);
        assert_eq!(stats.input_packets, 16);
        assert_eq!(stats.input_frames, 16 * 4096);
        assert!(stats.output_blocks > 0);
        assert_eq!(app.audio.active_source_count(), 2);
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.opens.load(Ordering::SeqCst),
            1,
            "shared bindings use the same native owner/stream"
        );
        assert_eq!(
            app.program_audio.as_ref().unwrap().key.session,
            app.session.identity()
        );
        // Program + global references promote the same SourceID once. Preview's
        // distinct SourceID must not double its 0.5 / 0.25 signal.
        capture_click(&mut app, &context, observed_now, "Program Audio");
        let _ = at_frame(&mut app, &context, observed_now, vec![]);
        let output = at_frame(&mut app, &context, observed_now, vec![]);
        assert!(text_position(&output, "Audio processing active").is_some());
        let stats = app.audio.program_status().unwrap();
        let rate = f64::from(app.session.snapshot().audio().sample_rate());
        assert!(
            text_position(
                &output,
                &format!(
                    "Adaptive buffer: {:.1} / {:.1} ms",
                    f64::from(stats.buffer_blocks) * 1024.0 * 1000.0 / rate,
                    f64::from(stats.max_buffer_blocks) * 1024.0 * 1000.0 / rate,
                )
            )
            .is_some()
        );
        assert!(
            text_position(
                &output,
                &format!(
                    "Buffering waits remaining: {} · pending sources: {}",
                    stats.waiting_blocks, stats.pending_inputs,
                )
            )
            .is_some()
        );
        assert!(
            text_position(
                &output,
                &format!(
                    "Buffering pauses: {} · buffer increases: {}",
                    stats.suppressed_blocks, stats.buffering_growths,
                )
            )
            .is_some()
        );
        let view = app.program_audio.as_ref().unwrap();
        let label = format!(
            "Program ch 1 peak {:.3} · RMS {:.3}",
            view.meters[0].peak, view.meters[0].rms
        );
        assert!(
            text_position(&output, &label).is_some(),
            "the popup paints actual accepted CPU meter metadata"
        );
        // Change canonical policy without admitting a new native control yet.
        // The legacy unkeyed owner status remains available internally, but the
        // frontend must not relabel it as the replacement policy.
        let original_audio = app.session.snapshot().audio();
        let settings = original_audio
            .with_buffering(skylive_core::AudioBuffering::low_latency())
            .unwrap();
        assert!(app.audio.program_status_for(&app.session).is_some());
        app.apply(Command::SetAudioSettings { settings });
        assert!(app.audio.program_status().is_some());
        assert!(app.audio.program_status_for(&app.session).is_none());
        assert!(app.program_audio.is_none());
        app.apply(Command::SetAudioSettings {
            settings: original_audio,
        });
        assert!(app.audio.program_status_for(&app.session).is_some());
        let authored = app.session.snapshot();
        let items = authored
            .scenes()
            .iter()
            .flat_map(|scene| scene.items().iter().map(|item| (scene.id(), item.id())))
            .collect::<Vec<_>>();
        for (scene, item) in items {
            app.intent(
                Intent::Command(Command::SetVisible {
                    scene,
                    item,
                    visible: false,
                }),
                &context,
            );
        }
        app.intent(
            Intent::Command(Command::SetGlobalAudio {
                slot: AudioSlot::Mic1,
                source: None,
            }),
            &context,
        );
        app.intent(Intent::StudioMode(false), &context);
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let now = u64::try_from(origin.elapsed().as_nanos())
                .unwrap()
                .max(observed_now);
            let _ = at_frame(&mut app, &context, now, vec![]);
            if app.audio.program_now().unwrap().is_none()
                && !app.audio.is_busy()
                && app.audio.active_source_count() == 0
                && state.closes.load(Ordering::SeqCst) == 1
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "empty scope did not acknowledge Program stop"
            );
            std::thread::yield_now();
        }
        assert!(app.program_audio.is_none());
        assert!(app.audio.program_status().is_none());
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
        assert_eq!(state.enumerations.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn program_meter_cache_invalidates_on_policy_apply_restoration_and_update_entry() {
        use skylive_core::AudioBuffering;
        use skylive_host::{audio::ChannelMeter, audio_capture::ProgramCaptureKey};
        fn seed(app: &mut StudioApp) {
            app.program_audio = Some(ProgramAudioView {
                start_frame: 0,
                source_meters: Vec::new(),
                source_meter_revision: None,
                key: ProgramCaptureKey {
                    session: app.session.identity(),
                    clock_epoch: 1,
                    generation: 1,
                    settings: app.session.snapshot().audio(),
                },
                meters: vec![ChannelMeter {
                    peak: 0.75,
                    rms: 0.5,
                }],
                missing_source_frames: 0,
                clipped_samples: 0,
            });
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        seed(&mut app);
        let unchanged = app.session.snapshot().audio();
        app.apply(Command::SetAudioSettings {
            settings: unchanged,
        });
        assert!(
            app.program_audio.is_some(),
            "an equal setting keeps accepted meter metadata"
        );
        let settings = unchanged
            .with_buffering(AudioBuffering::low_latency())
            .unwrap();
        app.apply(Command::SetAudioSettings { settings });
        assert!(
            app.program_audio.is_none(),
            "Apply clears old-policy meters before another GUI frame or PCM result"
        );
        seed(&mut app);
        app.apply(Command::SetAudioSettings {
            settings: unchanged,
        });
        assert!(
            app.program_audio.is_none(),
            "restoring settings also replaces the complete configuration identity"
        );
        seed(&mut app);
        // Simulate a replacement performed outside the frontend command path.
        app.session
            .apply(Command::SetAudioSettings { settings })
            .unwrap();
        let _ = app.update_audio(&context, &[], None, MediaTime(0));
        assert!(
            app.program_audio.is_none(),
            "the next update rejects cached settings independently of command admission"
        );
        seed(&mut app);
        app.session = Session::new();
        let _ = app.update_audio(&context, &[], None, MediaTime(0));
        assert!(
            app.program_audio.is_none(),
            "a replacement document cannot inherit accepted meters"
        );
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: false,
            mixer: true,
            transitions: false,
            controls: false,
        };
        capture_click(&mut app, &context, 0, "Program Audio");
        let output = at_frame(&mut app, &context, 0, vec![]);
        assert!(text_position(&output, "Waiting for audio processing").is_some());
        assert!(text_position(&output, "Waiting for audio levels").is_some());
        assert!(text_position(&output, "Program ch 1 peak 0.750 · RMS 0.500").is_none());
    }
    #[test]
    fn audio_buffering_checkbox_stages_one_edit_and_preserves_custom_policy_across_format_changes()
    {
        use skylive_core::{
            AudioBuffering, AudioCaptureBinding, AudioFlow, AudioSlot, AudioTarget,
        };
        fn open(app: &mut StudioApp, context: &egui::Context) {
            let intent =
                views::menus::action_intent("action_Settings", &app.session, None, false).unwrap();
            app.intent(intent, context);
            capture_click(app, context, 0, "Audio");
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let scene = app.session.preview();
        let binding = AudioCaptureBinding::new(
            AudioFlow::Input,
            AudioTarget::WindowsEndpoint {
                id: "buffering-stable-binding".into(),
            },
            true,
            false,
        )
        .unwrap();
        app.intent(
            Intent::Command(Command::AddAudioCaptureSource {
                scene,
                name: "Buffering Source".into(),
                binding,
            }),
            &context,
        );
        let source = app.session.snapshot().sources()[0].id();
        app.intent(
            Intent::Command(Command::SetGlobalAudio {
                slot: AudioSlot::Mic1,
                source: Some(source),
            }),
            &context,
        );
        let custom = AudioBuffering::new(137, true).unwrap();
        let settings = app
            .session
            .snapshot()
            .audio()
            .with_buffering(custom)
            .unwrap();
        app.intent(
            Intent::Command(Command::SetAudioSettings { settings }),
            &context,
        );
        let original = app.session.snapshot();
        open(&mut app, &context);
        assert!(
            matches!(&app.dialog,Some(Dialog::AudioSettings {buffering,..}) if *buffering==custom)
        );
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let _ = at_frame(&mut app, &context, 0, vec![]);
        assert!(
            matches!(&app.dialog,Some(Dialog::AudioSettings {buffering,..}) if *buffering==custom),
            "painting an unchecked checkbox must not rewrite custom fixed settings"
        );
        capture_click(&mut app, &context, 0, "Low Latency Audio Buffering");
        assert!(
            matches!(&app.dialog,Some(Dialog::AudioSettings {buffering,..}) if buffering.is_low_latency())
        );
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        open(&mut app, &context);
        capture_click(&mut app, &context, 0, "48 kHz");
        capture_click(&mut app, &context, 0, "44.1 kHz");
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        let format = app.session.snapshot();
        assert_eq!(format.audio().sample_rate(), 44100);
        assert_eq!(format.audio().buffering(), custom);
        assert_eq!(format.revision(), original.revision() + 1);
        assert_eq!(format.sources(), original.sources());
        assert_eq!(format.global_audio(), original.global_audio());
        app.intent(
            Intent::Command(Command::SetAudioSettings {
                settings: original.audio(),
            }),
            &context,
        );
        assert_eq!(app.session.snapshot().audio(), original.audio());
        open(&mut app, &context);
        let before_low = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Low Latency Audio Buffering");
        capture_click(&mut app, &context, 0, "Apply");
        let low = app.session.snapshot();
        assert_eq!(low.audio().buffering(), AudioBuffering::low_latency());
        assert_eq!(low.revision(), before_low.revision() + 1);
        assert_eq!(low.sources(), original.sources());
        assert_eq!(low.global_audio(), original.global_audio());
        app.intent(
            Intent::Command(Command::SetAudioSettings {
                settings: original.audio(),
            }),
            &context,
        );
        assert_eq!(app.session.snapshot().audio().buffering(), custom);
        app.intent(
            Intent::Command(Command::SetAudioSettings {
                settings: low.audio(),
            }),
            &context,
        );
        open(&mut app, &context);
        let low = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Low Latency Audio Buffering");
        assert!(
            matches!(&app.dialog,Some(Dialog::AudioSettings {buffering,..}) if *buffering==AudioBuffering::default())
        );
        assert!(Arc::ptr_eq(&low, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&low, &app.session.snapshot()));
        open(&mut app, &context);
        capture_click(&mut app, &context, 0, "Low Latency Audio Buffering");
        capture_click(&mut app, &context, 0, "Apply");
        assert_eq!(
            app.session.snapshot().audio().buffering(),
            AudioBuffering::default()
        );
        let settings = app
            .session
            .snapshot()
            .audio()
            .with_buffering(AudioBuffering::low_latency())
            .unwrap();
        app.intent(
            Intent::Command(Command::SetAudioSettings { settings }),
            &context,
        );
        assert_eq!(
            app.session.snapshot().audio().buffering(),
            AudioBuffering::low_latency()
        );
        open(&mut app, &context);
        let unchanged = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Apply");
        assert!(Arc::ptr_eq(&unchanged, &app.session.snapshot()));
    }
    #[test]
    fn profile_menu_drafts_selection_and_removal_preserve_scene_history_and_global_preferences() {
        use skylive_core::{
            AudioBuffering, AudioSettings, ProfileCommand, SpeakerLayout, VideoSettings,
        };
        use views::dialogs::ProfileEdit;
        fn menu(app: &mut StudioApp, context: &egui::Context, action: &str) {
            capture_click(app, context, 0, "Profile");
            capture_click(app, context, 0, action);
        }
        fn name(app: &mut StudioApp, value: &str) {
            let Some(Dialog::ProfileName { name, .. }) = &mut app.dialog else {
                panic!("profile action opens a name draft")
            };
            *name = value.into();
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let scene = app.session.preview();
        app.apply(Command::AddColorSource {
            scene,
            name: "Preserved source".into(),
            rgba: [255, 0, 0, 255],
        });
        app.apply(Command::SetVideo {
            width: 160,
            height: 90,
            fps_num: 25,
            fps_den: 1,
        });
        let custom = AudioSettings::new(44100, SpeakerLayout::Mono)
            .unwrap()
            .with_buffering(AudioBuffering::new(137, false).unwrap())
            .unwrap();
        app.apply(Command::SetAudioSettings { settings: custom });
        let original = app.session.snapshot();
        let original_id = original.profiles().selected_id();
        assert!(
            views::menus::action_intent("actionRemoveProfile", &app.session, None, false).is_none()
        );
        menu(&mut app, &context, "New...");
        assert!(matches!(
            app.dialog,
            Some(Dialog::ProfileName {
                edit: ProfileEdit::New,
                ..
            })
        ));
        name(&mut app, "Streaming");
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        menu(&mut app, &context, "New...");
        name(&mut app, "Streaming");
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        let created = app.session.snapshot();
        let streaming = created.profiles().selected_id();
        assert_eq!(created.profiles().entries().len(), 2);
        assert_eq!(created.video(), &VideoSettings::default());
        assert_eq!(
            (created.audio().sample_rate(), created.audio().speakers()),
            (48000, SpeakerLayout::Stereo)
        );
        assert_eq!(created.audio().buffering(), custom.buffering());
        assert_eq!(created.scenes(), original.scenes());
        assert_eq!(created.sources(), original.sources());
        assert!(
            app.setup.is_open(),
            "new-profile checkbox opens guided setup"
        );
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(
            Arc::ptr_eq(&created, &app.session.snapshot()),
            "cancelling setup preserves the created profile"
        );
        menu(&mut app, &context, "Untitled");
        assert_eq!(app.session.snapshot().profiles().selected_id(), original_id);
        assert_eq!(app.session.snapshot().audio(), custom);
        menu(&mut app, &context, "Duplicate...");
        name(&mut app, "Backup");
        capture_click(&mut app, &context, 0, "Apply");
        let duplicated = app.session.snapshot();
        let backup = duplicated.profiles().selected_id();
        assert_eq!(duplicated.profiles().entries().len(), 3);
        assert_eq!(duplicated.video(), original.video());
        assert_eq!(duplicated.audio(), custom);
        menu(&mut app, &context, "Rename...");
        name(&mut app, "");
        let before = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Apply");
        assert!(app.error);
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        assert!(matches!(&app.dialog,Some(Dialog::ProfileName {name,..}) if name.is_empty()));
        name(&mut app, "Backup renamed");
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        assert_eq!(
            app.session.snapshot().profiles().selected().name(),
            "Backup renamed"
        );
        let scoped = app.session.snapshot();
        app.apply(Command::Undo);
        let undone = app.session.snapshot();
        assert!(
            undone.sources().is_empty(),
            "scene Undo skips profile and preference edits"
        );
        assert_eq!(undone.profiles(), scoped.profiles());
        assert_eq!(undone.preferences(), scoped.preferences());
        assert_eq!(undone.audio(), custom);
        app.apply(Command::Redo);
        assert_eq!(app.session.snapshot().sources(), original.sources());
        assert_eq!(app.session.snapshot().profiles(), scoped.profiles());
        menu(&mut app, &context, "Remove");
        assert!(matches!(app.dialog,Some(Dialog::RemoveProfile {id,..}) if id==backup));
        let before = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        menu(&mut app, &context, "Remove");
        capture_click(&mut app, &context, 0, "Delete");
        assert_eq!(app.session.snapshot().profiles().entries().len(), 2);
        assert_eq!(app.session.snapshot().profiles().selected_id(), streaming);
        menu(&mut app, &context, "Remove");
        capture_click(&mut app, &context, 0, "Delete");
        assert_eq!(app.session.snapshot().profiles().selected_id(), original_id);
        assert_eq!(app.session.snapshot().profiles().entries().len(), 1);
        let last = app.session.snapshot();
        menu(&mut app, &context, "Remove");
        assert!(app.dialog.is_none());
        assert!(Arc::ptr_eq(&last, &app.session.snapshot()));
        // Closing the still-open menu exposes the next independent action.
        let _ = at_frame(
            &mut app,
            &context,
            0,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        menu(&mut app, &context, "Rename...");
        capture_click(&mut app, &context, 0, "Apply");
        assert!(
            Arc::ptr_eq(&last, &app.session.snapshot()),
            "equal profile Rename is a no-op"
        );
        assert_eq!(app.session.snapshot().scenes(), original.scenes());
        assert_eq!(app.session.snapshot().sources(), original.sources());
        assert_eq!(app.session.snapshot().audio(), custom);
        assert!(matches!(
            views::menus::action_intent("actionDupProfile", &app.session, None, false),
            Some(Intent::OpenDialog(Dialog::ProfileName {
                edit: ProfileEdit::Duplicate,
                ..
            }))
        ));
        // Selecting an existing ID is the same typed boundary used by radio rows.
        app.apply(Command::Profile {
            command: ProfileCommand::Select { id: original_id },
        });
        assert!(Arc::ptr_eq(&last, &app.session.snapshot()));
        let owner = app.session.snapshot();
        let identity = app.session.identity();
        assert!(app.session.mark_saved(identity, owner.revision()));
        app.intent(Intent::NewCollection, &context);
        assert!(!app.error, "{}", app.status);
        assert_ne!(app.session.identity(), identity);
        let empty = app.session.snapshot();
        assert!(empty.sources().is_empty());
        assert_eq!(empty.scenes().len(), 1);
        assert_eq!(empty.profiles(), owner.profiles());
        assert_eq!(empty.preferences(), owner.preferences());
        assert_eq!(empty.audio(), custom);
    }
    #[test]
    fn plugins_catalog_search_and_global_toggles_keep_unported_modules_disabled() {
        use skylive_core::{AUDIO_PAN_PLUGIN, PluginOrigin};
        fn search(app: &mut StudioApp, text: &str) {
            let Some(Dialog::Plugins { query, .. }) = &mut app.dialog else {
                panic!("Plugins remains open")
            };
            *query = text.into();
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        assert!(
            skylive_core::plugin_catalog()
                .iter()
                .filter(|plugin| plugin.origin == PluginOrigin::ThirdParty)
                .all(|plugin| !app
                    .session
                    .snapshot()
                    .preferences()
                    .plugin_enabled(plugin.id))
        );
        capture_click(&mut app, &context, 0, "Tools");
        capture_click(&mut app, &context, 0, "Plugin Manager");
        assert!(matches!(app.dialog, Some(Dialog::Plugins { .. })));
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        assert!(
            !visible_text_positions(&output, "Modules: 16 built-in · 127 third-party").is_empty()
        );
        search(&mut app, AUDIO_PAN_PLUGIN);
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let provider = skylive_core::plugin_descriptor(AUDIO_PAN_PLUGIN).unwrap();
        let details = text_position(&output, provider.port_scope).unwrap();
        // Real operation controls extend the scrollable details. The source
        // link remains reachable by scrolling; it need not fit above the fold.
        let _ = at_frame(
            &mut app,
            &context,
            0,
            vec![
                egui::Event::PointerMoved(details),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -600.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let mut output = output;
        for _ in 0..6 {
            output = at_frame(&mut app, &context, 0, vec![]);
        }
        assert!(text_position(&output, provider.source_url).is_some());
        capture_click(&mut app, &context, 0, "Enabled");
        assert!(
            app.session
                .snapshot()
                .preferences()
                .plugin_enabled(AUDIO_PAN_PLUGIN)
        );
        assert!(!app.session.can_undo());
        assert!(matches!(&app.dialog,Some(Dialog::Plugins {query,..}) if query==AUDIO_PAN_PLUGIN));
        let mute = "norihiro/obs-mute-filter";
        search(&mut app, mute);
        let before = app.session.snapshot();
        let output = at_frame(&mut app, &context, 0, vec![]);
        assert!(visible_text_positions(&output, "Enabled").is_empty());
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        assert!(text_position(&output, "Reference only · not ported").is_some());
        app.intent(
            Intent::SetPluginEnabled {
                plugin: mute.into(),
                enabled: true,
            },
            &context,
        );
        assert!(app.error);
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        assert!(matches!(&app.dialog,Some(Dialog::Plugins {query,..}) if query==mute));
        search(&mut app, AUDIO_PAN_PLUGIN);
        capture_click(&mut app, &context, 0, "Enabled");
        assert!(!app.error, "{}", app.status);
        assert!(
            !app.session
                .snapshot()
                .preferences()
                .plugin_enabled(AUDIO_PAN_PLUGIN)
        );
        search(&mut app, "no-provider-has-this-id");
        let output = at_frame(&mut app, &context, 0, vec![]);
        assert!(text_position(&output, "No modules match this search.").is_some());
        capture_click(&mut app, &context, 0, "Close");
        assert!(app.dialog.is_none());
    }
    #[test]
    fn audio_filter_widgets_stage_source_order_validation_and_disabled_retention() {
        use skylive_core::{
            AUDIO_PAN_PLUGIN, AudioCaptureBinding, AudioFilter, AudioFlow, AudioTarget,
        };
        fn left(app: &mut StudioApp, value: f32) {
            let Some(Dialog::AudioFilters { filters, .. }) = &mut app.dialog else {
                panic!("filter draft")
            };
            let AudioFilter::Pan { pan_left, .. } = &mut filters[0] else {
                panic!("pan filter draft")
            };
            *pan_left = value;
        }
        fn first_button(app: &mut StudioApp, context: &egui::Context, label: &str) {
            let _ = at_frame(app, context, 0, vec![]);
            let output = at_frame(app, context, 0, vec![]);
            let point = *visible_text_positions(&output, label)
                .first()
                .expect("first filter action is visible");
            let _ = click_at(app, context, 0, point);
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: true,
            mixer: false,
            transitions: false,
            controls: false,
        };
        let scene = app.session.preview();
        app.apply(Command::AddAudioCaptureSource {
            scene,
            name: "Filtered source".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        });
        let source = app.session.snapshot().sources()[0].id();
        let before = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Audio Filters");
        assert!(
            matches!(&app.dialog,Some(Dialog::AudioFilters{filters,..}) if filters.is_empty()),
            "bundled audio effects make the shared editor available"
        );
        capture_click(&mut app, &context, 0, "Add Audio Pan Filter");
        assert!(
            matches!(&app.dialog,Some(Dialog::AudioFilters{filters,..}) if filters.is_empty()),
            "third-party Pan remains off by default"
        );
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        app.intent(
            Intent::SetPluginEnabled {
                plugin: AUDIO_PAN_PLUGIN.into(),
                enabled: true,
            },
            &context,
        );
        let before = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Audio Filters");
        capture_click(&mut app, &context, 0, "Add Audio Pan Filter");
        left(&mut app, 0.0);
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "Audio Filters");
        capture_click(&mut app, &context, 0, "Add Audio Pan Filter");
        left(&mut app, 0.0);
        capture_click(&mut app, &context, 0, "Add Audio Pan Filter");
        capture_click(&mut app, &context, 0, "Audio Pan Filter 1");
        first_button(&mut app, &context, "Move Filter Down");
        let default = AudioFilter::default();
        let mut custom = default;
        let AudioFilter::Pan { pan_left, .. } = &mut custom else {
            panic!("pan filter fixture")
        };
        *pan_left = 0.0;
        assert!(
            matches!(&app.dialog,Some(Dialog::AudioFilters {filters,..}) if filters==&vec![default,custom])
        );
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        let configured = app.session.snapshot();
        assert_eq!(
            configured.source(source).unwrap().audio_filters(),
            &[default, custom]
        );
        assert_eq!(configured.revision(), before.revision() + 1);
        app.apply(Command::Undo);
        assert!(
            app.session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_filters()
                .is_empty()
        );
        assert!(
            app.session
                .snapshot()
                .preferences()
                .plugin_enabled(AUDIO_PAN_PLUGIN)
        );
        app.apply(Command::Redo);
        capture_click(&mut app, &context, 0, "Audio Filters");
        left(&mut app, 101.0);
        let valid = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Apply");
        assert!(app.error);
        assert!(Arc::ptr_eq(&valid, &app.session.snapshot()));
        assert!(
            matches!(&app.dialog,Some(Dialog::AudioFilters {filters,..}) if matches!(filters[0],AudioFilter::Pan {pan_left,..} if pan_left==101.0))
        );
        capture_click(&mut app, &context, 0, "Cancel");
        app.intent(
            Intent::SetPluginEnabled {
                plugin: AUDIO_PAN_PLUGIN.into(),
                enabled: false,
            },
            &context,
        );
        assert_eq!(
            app.session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_filters(),
            &[default, custom]
        );
        capture_click(&mut app, &context, 0, "Audio Filters");
        capture_click(&mut app, &context, 0, "Audio Pan Filter 1");
        first_button(&mut app, &context, "Remove Filter");
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        assert_eq!(
            app.session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_filters(),
            &[custom]
        );
        app.apply(Command::Undo);
        assert_eq!(
            app.session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_filters(),
            &[default, custom]
        );
        assert!(
            !app.session
                .snapshot()
                .preferences()
                .plugin_enabled(AUDIO_PAN_PLUGIN)
        );
    }
    #[test]
    fn scene_notes_widgets_stage_utf8_bounds_and_keep_notes_when_module_is_off() {
        use skylive_core::{MAX_SCENE_NOTES_BYTES, SCENE_NOTES_PLUGIN};
        fn text(app: &mut StudioApp, value: String) {
            let Some(Dialog::SceneNotes { text, .. }) = &mut app.dialog else {
                panic!("notes draft")
            };
            *text = value;
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let scene = app.session.preview();
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: true,
            sources: false,
            mixer: false,
            transitions: false,
            controls: false,
        };
        let initial = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Scene Notes");
        assert!(app.dialog.is_none());
        assert!(Arc::ptr_eq(&initial, &app.session.snapshot()));
        app.intent(
            Intent::SetPluginEnabled {
                plugin: SCENE_NOTES_PLUGIN.into(),
                enabled: true,
            },
            &context,
        );
        let before = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Scene Notes");
        text(&mut app, "Discard me".into());
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "Scene Notes");
        let oversized = "я".repeat(MAX_SCENE_NOTES_BYTES / 2 + 1);
        text(&mut app, oversized.clone());
        capture_click(&mut app, &context, 0, "Apply");
        assert!(app.error);
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        assert!(matches!(&app.dialog,Some(Dialog::SceneNotes {text,..}) if text==&oversized));
        let note = "Cue 1\nПочаток ефіру";
        text(&mut app, note.into());
        capture_click(&mut app, &context, 0, "Apply");
        assert!(!app.error, "{}", app.status);
        assert_eq!(app.session.snapshot().scene(scene).unwrap().notes(), note);
        assert_eq!(app.session.snapshot().revision(), before.revision() + 1);
        app.apply(Command::Undo);
        assert_eq!(app.session.snapshot().scene(scene).unwrap().notes(), "");
        assert!(
            app.session
                .snapshot()
                .preferences()
                .plugin_enabled(SCENE_NOTES_PLUGIN)
        );
        app.apply(Command::Redo);
        app.intent(
            Intent::SetPluginEnabled {
                plugin: SCENE_NOTES_PLUGIN.into(),
                enabled: false,
            },
            &context,
        );
        let disabled = app.session.snapshot();
        capture_click(&mut app, &context, 0, "Scene Notes");
        assert!(app.dialog.is_none());
        assert!(Arc::ptr_eq(&disabled, &app.session.snapshot()));
        assert_eq!(disabled.scene(scene).unwrap().notes(), note);
        app.intent(
            Intent::SetPluginEnabled {
                plugin: SCENE_NOTES_PLUGIN.into(),
                enabled: true,
            },
            &context,
        );
        capture_click(&mut app, &context, 0, "Scene Notes");
        assert!(matches!(&app.dialog,Some(Dialog::SceneNotes {text,..}) if text==note));
        capture_click(&mut app, &context, 0, "Apply");
        assert_eq!(app.session.snapshot().scene(scene).unwrap().notes(), note);
    }
    fn global_transition_kind(app: &StudioApp) -> skylive_core::TransitionKind {
        let model = app.session.snapshot();
        model.resolve_transition(model.transition()).unwrap().kind()
    }
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "skylive-desktop-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn settle_io(app: &mut StudioApp, context: &egui::Context) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.io.busy() {
            app.poll_io(context);
            assert!(
                std::time::Instant::now() < deadline,
                "document I/O did not settle"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    #[test]
    fn localized_chrome_never_translates_authored_scene_and_source_names() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let scene = app.session.preview();
        app.session
            .apply(Command::RenameScene {
                scene,
                name: "Settings".into(),
            })
            .unwrap();
        app.session
            .apply(Command::AddColorSource {
                scene,
                name: "Save".into(),
                rgba: [255, 0, 0, 255],
            })
            .unwrap();
        app.session
            .apply(Command::SetLanguage {
                language: "ru-RU".into(),
            })
            .unwrap();
        app.localization =
            Arc::new(skylive_host::localization::Localization::embedded("ru-RU").unwrap());
        crate::i18n::install(&context, app.localization.clone(), &app.localization_path);
        let _ = frame(&mut app, &context, vec![]);
        let output = frame(&mut app, &context, vec![]);
        assert!(
            text_position(&output, "Settings").is_some(),
            "authored scene name remains exact"
        );
        assert!(
            text_position(&output, "Save").is_some(),
            "authored source name remains exact"
        );
        assert!(
            text_position(&output, "Источники").is_some(),
            "Sources chrome is localized"
        );
    }

    #[test]
    fn language_reload_rejects_bad_csv_and_persists_installed_preferences_without_scene_undo() {
        let temp = Temp::new();
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        app.localization_path = temp.0.join("translations.csv");
        app.preferences_path = Some(temp.0.join("user-preferences.json"));
        let csv = "key,en-US,ru-RU\nSettings,Settings,Настройки теста\nSkyLive.LanguageName,English,Русский\n";
        std::fs::write(&app.localization_path, csv).unwrap();
        app.intent(
            Intent::ApplyLocalization {
                language: "ru-RU".into(),
            },
            &context,
        );
        settle_io(&mut app, &context);
        assert_eq!(app.localization.language(), "ru-RU");
        assert_eq!(
            crate::i18n::tr_context(&context, "Settings"),
            "Настройки теста"
        );
        assert_eq!(app.session.snapshot().preferences().language(), "ru-RU");
        assert!(!app.session.can_undo());
        assert!(!app.session.is_document_dirty());
        app.persist_preferences();
        settle_io(&mut app, &context);
        let loaded = skylive_host::user_preferences::load(app.preferences_path.as_ref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(loaded.language(), "ru-RU");
        assert!(!app.preferences_unsaved());
        let revision = app.session.snapshot().revision();
        let changed = csv.replace("Настройки теста", "Настройки изменены");
        std::fs::write(&app.localization_path, changed).unwrap();
        app.intent(
            Intent::ApplyLocalization {
                language: "ru-RU".into(),
            },
            &context,
        );
        settle_io(&mut app, &context);
        assert_eq!(app.session.snapshot().revision(), revision);
        assert_eq!(
            crate::i18n::tr_context(&context, "Settings"),
            "Настройки изменены"
        );
        std::fs::write(&app.localization_path, "key,en-US,ru-RU\nA,a,b\nA,c,d").unwrap();
        app.intent(
            Intent::ApplyLocalization {
                language: "ru-RU".into(),
            },
            &context,
        );
        settle_io(&mut app, &context);
        assert!(app.error);
        assert_eq!(app.localization.language(), "ru-RU");
        assert_eq!(
            crate::i18n::tr_context(&context, "Settings"),
            "Настройки изменены"
        );
        assert_eq!(app.session.snapshot().revision(), revision);
        app.shutdown(&context);
        assert!(app.allow_close);
    }

    #[test]
    fn preference_save_failure_retains_prior_file_and_close_requires_explicit_discard() {
        let temp = Temp::new();
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let obstacle = temp.0.join("not-directory");
        std::fs::write(&obstacle, b"user-owned").unwrap();
        app.preferences_path = Some(obstacle.join("preferences.json"));
        app.session
            .apply(Command::SetLanguage {
                language: "de-DE".into(),
            })
            .unwrap();
        app.shutdown(&context);
        assert!(!app.allow_close);
        settle_io(&mut app, &context);
        assert!(app.error);
        assert!(app.close_dialog);
        assert!(!app.io.busy());
        app.persist_preferences();
        assert!(
            !app.io.busy(),
            "no repeated background retries for a failed exact snapshot"
        );
        assert_eq!(std::fs::read(&obstacle).unwrap(), b"user-owned");
        app.saved_preferences = app.session.snapshot().preferences().clone();
        app.shutdown(&context);
        assert!(app.allow_close);
    }

    #[test]
    fn headless_frame_and_view_command_share_the_engine() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        app.intent(
            Intent::Command(Command::AddColorSource {
                scene: app.session.preview(),
                name: "Red".into(),
                rgba: [255, 0, 0, 255],
            }),
            &context,
        );
        assert_eq!(app.session.snapshot().sources().len(), 1);
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                ..Default::default()
            },
            |ctx| app.frame(ctx),
        );
        app.intent(Intent::Command(Command::Undo), &context);
        assert!(app.session.snapshot().sources().is_empty());
    }
    #[test]
    fn pending_document_replacement_rejects_new_edit_intents() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        app.intent(Intent::Load, &context);
        assert!(app.io.busy());
        assert!(matches!(app.after_io, Some(AfterIo::ReplaceDocument)));
        app.intent(
            Intent::Command(Command::AddScene {
                name: "Must not publish".into(),
            }),
            &context,
        );
        assert_eq!(app.session.snapshot().revision(), 0);
        assert_eq!(app.session.snapshot().scenes().len(), 1);
    }
    fn frame(
        app: &mut StudioApp,
        context: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| app.frame(ctx),
        )
    }
    fn at_frame(
        app: &mut StudioApp,
        context: &egui::Context,
        now: u64,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |context| app.frame_at(context, MediaTime(now)),
        )
    }
    fn click_at(
        app: &mut StudioApp,
        context: &egui::Context,
        now: u64,
        position: egui::Pos2,
    ) -> egui::FullOutput {
        let _ = at_frame(
            app,
            context,
            now,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        at_frame(
            app,
            context,
            now,
            vec![egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        )
    }
    fn visible_text_positions(output: &egui::FullOutput, label: &str) -> Vec<egui::Pos2> {
        fn collect(
            shape: &egui::epaint::Shape,
            clip: egui::Rect,
            label: &str,
            result: &mut Vec<egui::Pos2>,
        ) {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    let pos = text.pos + text.galley.size() * 0.5;
                    if clip.contains(pos) {
                        result.push(pos);
                    }
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, clip, label, result);
                    }
                }
                _ => {}
            }
        }
        let mut result = Vec::new();
        for shape in &output.shapes {
            collect(&shape.shape, shape.clip_rect, label, &mut result);
        }
        result
    }
    fn scroll_to_transition_text(
        app: &mut StudioApp,
        context: &egui::Context,
        now: u64,
        label: &str,
    ) -> egui::Pos2 {
        let output = at_frame(app, context, now, vec![]);
        let x = visible_text_positions(&output, "Scene Transitions")
            .first()
            .map_or(900.0, |pos| pos.x);
        for _ in 0..24 {
            let output = at_frame(app, context, now, vec![]);
            if let Some(position) = visible_text_positions(&output, label).first() {
                return *position;
            }
            let _ = at_frame(
                app,
                context,
                now,
                vec![
                    egui::Event::PointerMoved(egui::pos2(x, 700.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -90.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        panic!("transition control {label} was not reachable by scrolling");
    }
    fn show_transition_dock_only(app: &mut StudioApp) {
        app.layout = Layout {
            arrangement: views::dock_layout::DockLayout::bottom_row(),
            columns: Default::default(),
            scenes: false,
            sources: false,
            mixer: false,
            transitions: true,
            controls: false,
        };
    }
    fn quick_row_menu_action(
        app: &mut StudioApp,
        context: &egui::Context,
        label: &str,
        action: &str,
    ) {
        let row = scroll_to_transition_text(app, context, 0, label);
        let output = at_frame(app, context, 0, vec![]);
        let options = visible_text_positions(&output, "Options")
            .into_iter()
            .min_by(|a, b| (a.y - row.y).abs().total_cmp(&(b.y - row.y).abs()))
            .expect("quick row has reachable Options");
        let _ = click_at(app, context, 0, options);
        let output = at_frame(app, context, 0, vec![]);
        let item = visible_text_positions(&output, action)
            .first()
            .copied()
            .expect("quick row menu action is visible");
        let _ = click_at(app, context, 0, item);
    }
    fn drag_tbar(
        app: &mut StudioApp,
        context: &egui::Context,
        now: u64,
        fraction: f32,
        release: bool,
    ) {
        let _ = at_frame(app, context, now, vec![]);
        let rect = context
            .data_mut(|data| data.get_temp::<egui::Rect>(egui::Id::new("skylive-tbar-control")))
            .expect("semantic T-bar slider is present");
        let start = egui::pos2(rect.left() + 1.0, rect.center().y);
        let target = egui::pos2(rect.left() + rect.width() * fraction, rect.center().y);
        let _ = at_frame(
            app,
            context,
            now,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let _ = at_frame(app, context, now, vec![egui::Event::PointerMoved(target)]);
        if release {
            let _ = at_frame(
                app,
                context,
                now,
                vec![egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
    }
    fn wait_preview_pixels(
        app: &mut StudioApp,
        context: &egui::Context,
        now: u64,
        oracles: &[([u8; 4], [u8; 4])],
    ) {
        let mut seen = vec![false; oracles.len()];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while seen.iter().any(|seen| !*seen) {
            let output = at_frame(app, context, now, vec![]);
            for (_, delta) in &output.textures_delta.set {
                if let egui::ImageData::Color(image) = &delta.image {
                    if image.size != [640, 360] {
                        continue;
                    }
                    let corners = (
                        image.pixels.first().unwrap().to_array(),
                        image.pixels.last().unwrap().to_array(),
                    );
                    for (index, expected) in oracles.iter().enumerate() {
                        seen[index] |= corners == *expected;
                    }
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected preview corner pixels were not presented: {oracles:?}, observed flags {seen:?}, status {}",
                app.status
            );
            std::thread::yield_now();
        }
    }
    fn transition_scenes(context: &egui::Context) -> (StudioApp, Id, Id, Id, Id) {
        let mut app = StudioApp::headless(context).unwrap();
        let first = app.session.preview();
        // Existing setup actions are intentionally immediate; the test starts
        // the actual Fade only through its tested widget below.
        app.intent(
            Intent::Command(Command::SetTransition {
                kind: skylive_core::TransitionKind::Cut,
                duration_ms: 300,
            }),
            context,
        );
        app.intent(
            Intent::Command(Command::AddColorSource {
                scene: first,
                name: "Red source".into(),
                rgba: [255, 0, 0, 255],
            }),
            context,
        );
        let red_item = app.selected.unwrap();
        let red_source = app.session.snapshot().sources()[0].id();
        app.intent(
            Intent::Command(Command::AddScene {
                name: "Second scene".into(),
            }),
            context,
        );
        let second = app.session.preview();
        app.intent(
            Intent::Command(Command::AddColorSource {
                scene: second,
                name: "Blue source".into(),
                rgba: [0, 0, 255, 255],
            }),
            context,
        );
        app.intent(Intent::SelectScene(first), context);
        assert!(!app.error, "{}", app.status);
        (app, first, second, red_item, red_source)
    }
    fn text_position(output: &egui::FullOutput, label: &str) -> Option<egui::Pos2> {
        text_positions(output, label).into_iter().next()
    }
    fn text_positions(output: &egui::FullOutput, label: &str) -> Vec<egui::Pos2> {
        fn find(shape: &egui::epaint::Shape, label: &str, positions: &mut Vec<egui::Pos2>) {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    positions.push(text.pos + text.galley.size() * 0.5);
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        find(shape, label, positions);
                    }
                }
                _ => {}
            }
        }
        let mut positions = Vec::new();
        for shape in &output.shapes {
            find(&shape.shape, label, &mut positions);
        }
        positions
    }
    fn write_bmp(path: &std::path::Path, width: u32, height: u32, rgb: [u8; 3]) {
        let stride = (width * 3 + 3) & !3;
        let pixel_bytes = stride * height;
        let mut bytes = vec![0u8; (54 + pixel_bytes) as usize];
        bytes[0..2].copy_from_slice(b"BM");
        bytes[2..6].copy_from_slice(&(54 + pixel_bytes).to_le_bytes());
        bytes[10..14].copy_from_slice(&54u32.to_le_bytes());
        bytes[14..18].copy_from_slice(&40u32.to_le_bytes());
        bytes[18..22].copy_from_slice(&(width as i32).to_le_bytes());
        bytes[22..26].copy_from_slice(&(height as i32).to_le_bytes());
        bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
        bytes[28..30].copy_from_slice(&24u16.to_le_bytes());
        bytes[34..38].copy_from_slice(&pixel_bytes.to_le_bytes());
        for row in 0..height {
            for column in 0..width {
                let offset = (54 + row * stride + column * 3) as usize;
                bytes[offset..offset + 3].copy_from_slice(&[rgb[2], rgb[1], rgb[0]]);
            }
        }
        std::fs::write(path, bytes).unwrap();
    }
    fn click(
        app: &mut StudioApp,
        context: &egui::Context,
        position: egui::Pos2,
    ) -> egui::FullOutput {
        let _ = frame(
            app,
            context,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        frame(
            app,
            context,
            vec![egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        )
    }
    #[test]
    fn actual_headless_widgets_add_scene_through_shared_command() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let _ = frame(&mut app, &context, vec![]);
        let _ = frame(&mut app, &context, vec![]);
        let add = toolbar_point(&context, "Scenes", "Add");
        let _ = click(&mut app, &context, add);
        assert!(matches!(app.dialog, Some(Dialog::AddScene { .. })));
        let _ = frame(&mut app, &context, vec![]);
        let output = frame(&mut app, &context, vec![]);
        let apply = text_position(&output, "Apply").expect("Modal Apply button is painted");
        let _ = click(&mut app, &context, apply);
        assert!(app.dialog.is_none());
        assert_eq!(app.session.snapshot().scenes().len(), 2);
        assert_eq!(app.session.snapshot().revision(), 1);
        assert!(app.session.can_undo());
    }
    #[test]
    fn transition_kind_and_duration_widgets_publish_one_validated_edit_each() {
        use skylive_core::TransitionKind;
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        assert_eq!(
            (
                global_transition_kind(&app),
                app.session.snapshot().transition().duration_ms()
            ),
            (TransitionKind::Fade, 300)
        );
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let cut = text_position(&output, "Cut").expect("Cut configuration widget exists");
        let _ = click_at(&mut app, &context, 0, cut);
        assert_eq!(global_transition_kind(&app), TransitionKind::Cut);
        assert_eq!(app.session.snapshot().revision(), 1);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let fade = text_position(&output, "Fade").expect("Fade configuration widget exists");
        let _ = click_at(&mut app, &context, 0, fade);
        assert_eq!(global_transition_kind(&app), TransitionKind::Fade);
        assert_eq!(app.session.snapshot().revision(), 2);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let duration =
            text_position(&output, "300 ms").expect("duration DragValue exposes milliseconds");
        let _ = click_at(&mut app, &context, 0, duration);
        let _ = at_frame(&mut app, &context, 0, vec![egui::Event::Text("450".into())]);
        assert_eq!(
            app.session.snapshot().transition().duration_ms(),
            300,
            "an in-progress numeric draft is presentation state"
        );
        let _ = at_frame(
            &mut app,
            &context,
            0,
            vec![
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(app.session.snapshot().transition().duration_ms(), 450);
        assert_eq!(app.session.snapshot().revision(), 3);
        app.intent(Intent::Command(Command::Undo), &context);
        assert_eq!(app.session.snapshot().transition().duration_ms(), 300);
    }
    #[test]
    fn real_tbar_drag_release_cancels_holds_and_finishes_without_authored_edits() {
        let context = egui::Context::default();
        let (mut app, first, second, _, _) = transition_scenes(&context);
        show_transition_dock_only(&mut app);
        app.intent(Intent::StudioMode(true), &context);
        app.intent(Intent::SelectScene(second), &context);
        let authored = app.session.snapshot();
        drag_tbar(&mut app, &context, 0, 0.05, true);
        assert!(app.transition_manual.is_none());
        assert!(!app.transition_active);
        assert_eq!(
            (app.session.preview(), app.session.program()),
            (second, first)
        );
        drag_tbar(&mut app, &context, 0, 512.0 / 1023.0, true);
        assert_eq!(
            app.transition_manual.map(|manual| manual.value),
            Some(512),
            "the pointer reaches manual half-transition value 512"
        );
        assert!(
            app.transition_manual
                .is_some_and(|manual| (103..=920).contains(&manual.value) && !manual.finishing)
        );
        assert!(app.transition_active);
        wait_preview_pixels(
            &mut app,
            &context,
            10_000_000_000,
            &[([128, 0, 128, 255], [128, 0, 128, 255])],
        );
        assert!(app.transition_active);
        assert!(
            !app.transition_needs_tick,
            "a settled middle hold is active without an app repaint loop"
        );
        let release =
            scroll_to_transition_text(&mut app, &context, 10_000_000_000, "Release T-bar");
        let _ = click_at(&mut app, &context, 10_000_000_000, release);
        assert!(
            app.transition_manual
                .is_some_and(|manual| !manual.finishing)
        );
        drag_tbar(&mut app, &context, 10_000_000_000, 0.98, true);
        assert!(
            app.transition_manual.is_some_and(|manual| manual.value == 0
                && manual.finishing
                && manual.target == 1.0)
        );
        assert!(app.transition_needs_tick);
        wait_preview_pixels(
            &mut app,
            &context,
            20_000_000_000,
            &[([0, 0, 255, 255], [0, 0, 255, 255])],
        );
        assert!(!app.transition_active);
        assert!(app.transition_manual.is_none());
        assert_eq!(
            (app.session.preview(), app.session.program()),
            (first, second)
        );
        assert!(
            Arc::ptr_eq(&authored, &app.session.snapshot()),
            "manual runtime must not add canonical history"
        );
    }
    #[test]
    fn quick_fade_widget_bypasses_override_without_changing_global_settings() {
        use skylive_core::{TransitionConfig, TransitionKind};
        let context = egui::Context::default();
        let (mut app, first, second, _, _) = transition_scenes(&context);
        show_transition_dock_only(&mut app);
        app.intent(
            Intent::Command(Command::SetSceneTransition {
                scene: second,
                config: Some(TransitionConfig::try_new(TransitionKind::Cut, 700).unwrap()),
            }),
            &context,
        );
        app.intent(Intent::StudioMode(true), &context);
        app.intent(Intent::SelectScene(second), &context);
        let authored = app.session.snapshot();
        let quick = scroll_to_transition_text(&mut app, &context, 0, "Fade (300 ms)");
        let _ = click_at(&mut app, &context, 0, quick);
        assert!(app.transition_active);
        assert_eq!(global_transition_kind(&app), TransitionKind::Cut);
        assert_eq!(
            app.session
                .snapshot()
                .scene(second)
                .unwrap()
                .transition_override()
                .unwrap()
                .duration_ms(),
            700
        );
        wait_preview_pixels(
            &mut app,
            &context,
            150_000_000,
            &[([128, 0, 128, 255], [128, 0, 128, 255])],
        );
        wait_preview_pixels(
            &mut app,
            &context,
            300_000_000,
            &[([0, 0, 255, 255], [0, 0, 255, 255])],
        );
        assert_eq!(
            (app.session.preview(), app.session.program()),
            (first, second)
        );
        assert!(Arc::ptr_eq(&authored, &app.session.snapshot()));
    }
    #[test]
    fn quick_fade_to_black_widget_uses_linear_half_and_returns_saved_preview_b() {
        let context = egui::Context::default();
        let (mut app, first, second, _, source) = transition_scenes(&context);
        show_transition_dock_only(&mut app);
        app.intent(
            Intent::Command(Command::SetColor {
                source,
                rgba: [255; 4],
            }),
            &context,
        );
        app.intent(Intent::StudioMode(true), &context);
        app.intent(Intent::SelectScene(second), &context);
        let authored = app.session.snapshot();
        let ftb = scroll_to_transition_text(&mut app, &context, 0, "Fade To Black (300 ms)");
        let _ = click_at(&mut app, &context, 0, ftb);
        wait_preview_pixels(
            &mut app,
            &context,
            150_000_000,
            &[([188, 188, 188, 255], [188, 188, 188, 255])],
        );
        wait_preview_pixels(
            &mut app,
            &context,
            300_000_000,
            &[([0, 0, 0, 255], [0, 0, 0, 255])],
        );
        app.intent(Intent::SelectScene(first), &context);
        let ftb =
            scroll_to_transition_text(&mut app, &context, 300_000_000, "Fade To Black (300 ms)");
        let _ = click_at(&mut app, &context, 300_000_000, ftb);
        wait_preview_pixels(
            &mut app,
            &context,
            450_000_000,
            &[([0, 0, 188, 255], [0, 0, 188, 255])],
        );
        wait_preview_pixels(
            &mut app,
            &context,
            600_000_000,
            &[([0, 0, 255, 255], [0, 0, 255, 255])],
        );
        assert_eq!(
            app.session.program(),
            first,
            "logical target may be A while the saved prepared B is the visible output"
        );
        assert!(Arc::ptr_eq(&authored, &app.session.snapshot()));
    }
    #[test]
    fn fade_to_color_draft_widgets_validate_and_shared_held_program_texture_refreshes() {
        use skylive_core::{TransitionKind, TransitionSelection};
        fn click(app: &mut StudioApp, context: &egui::Context, label: &str) {
            let _ = at_frame(app, context, 0, vec![]);
            let output = at_frame(app, context, 0, vec![]);
            let point = visible_text_positions(&output, label)
                .last()
                .copied()
                .unwrap_or_else(|| panic!("{label} is visible"));
            let _ = click_at(app, context, 0, point);
        }
        let context = egui::Context::default();
        let (mut app, first, second, _, _) = transition_scenes(&context);
        show_transition_dock_only(&mut app);
        let original = app.session.snapshot();
        click(&mut app, &context, "Preset Settings");
        click(&mut app, &context, "Add Transition Preset");
        click(&mut app, &context, "Fade to Color");
        let output = at_frame(&mut app, &context, 0, vec![]);
        assert!(text_position(&output, "Opaque RGB color").is_some());
        assert!(text_position(&output, "Switch point (%)").is_some());
        if let Some(Dialog::TransitionPreset { name, kind, .. }) = &mut app.dialog {
            *name = "Tint".into();
            *kind = TransitionKind::FadeToColor {
                rgb: [12, 200, 34],
                switch_point: 50,
            };
        }
        click(&mut app, &context, "50");
        let _ = at_frame(&mut app, &context, 0, vec![egui::Event::Text("101".into())]);
        let _ = at_frame(
            &mut app,
            &context,
            0,
            vec![
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        click(&mut app, &context, "Apply");
        assert!(app.error);
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        assert!(matches!(
            app.dialog,
            Some(Dialog::TransitionPreset {
                kind: TransitionKind::FadeToColor {
                    rgb: [12, 200, 34],
                    switch_point: 101
                },
                ..
            })
        ));
        if let Some(Dialog::TransitionPreset { kind, .. }) = &mut app.dialog {
            *kind = TransitionKind::FadeToColor {
                rgb: [12, 200, 34],
                switch_point: 50,
            };
        }
        click(&mut app, &context, "Cancel");
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        app.intent(
            Intent::OpenDialog(Dialog::TransitionPreset {
                id: None,
                name: "Tint".into(),
                kind: TransitionKind::FadeToColor {
                    rgb: [12, 200, 34],
                    switch_point: 50,
                },
            }),
            &context,
        );
        click(&mut app, &context, "Apply");
        assert!(!app.error, "{}", app.status);
        let id = app.transition_preset_focus.unwrap();
        assert_eq!(app.session.snapshot().revision(), original.revision() + 1);
        app.intent(
            Intent::Command(Command::SelectTransition {
                selection: TransitionSelection::new(TransitionRef::Preset(id), 300).unwrap(),
            }),
            &context,
        );
        app.intent(Intent::StudioMode(true), &context);
        app.intent(Intent::SelectScene(second), &context);
        app.intent(Intent::Transition, &context);
        let color = [12, 200, 34, 255];
        wait_preview_pixels(&mut app, &context, 150_000_000, &[(color, color)]);
        assert_eq!(app.program.displayed_pixel().unwrap().0, color);
        wait_preview_pixels(
            &mut app,
            &context,
            300_000_000,
            &[([0, 0, 255, 255], [0, 0, 255, 255])],
        );
        assert_eq!(
            (app.session.preview(), app.session.program()),
            (first, second)
        );
        app.intent(Intent::ManualPosition(512), &context);
        wait_preview_pixels(&mut app, &context, 10_000_000_000, &[(color, color)]);
        assert_eq!(app.transition_manual.unwrap().current, 0.5);
        let old_output = app.program.displayed_pixel().unwrap().1;
        let before = app.session.snapshot();
        app.intent(
            Intent::Command(Command::ConfigureTransitionPreset {
                id,
                name: "Tint".into(),
                kind: TransitionKind::FadeToColor {
                    rgb: [99, 17, 220],
                    switch_point: 50,
                },
            }),
            &context,
        );
        let changed = [99, 17, 220, 255];
        wait_preview_pixels(&mut app, &context, 10_000_000_000, &[(changed, changed)]);
        let (pixel, key) = app.program.displayed_pixel().unwrap();
        assert_eq!(pixel, changed);
        assert_ne!(key, old_output);
        assert_eq!(app.session.snapshot().revision(), before.revision() + 1);
        assert!(!app.transition_needs_tick);
        assert_eq!(app.transition_manual.unwrap().current, 0.5);
        app.intent(Intent::Command(Command::Undo), &context);
        wait_preview_pixels(&mut app, &context, 10_000_000_000, &[(color, color)]);
        assert_eq!(app.program.displayed_pixel().unwrap().0, color);
    }
    #[test]
    fn remove_named_preset_confirmation_projects_references_and_commits_one_undo() {
        use skylive_core::{TransitionDirection, TransitionKind, TransitionSelection};
        fn click(app: &mut StudioApp, context: &egui::Context, label: &str) {
            let _ = at_frame(app, context, 0, vec![]);
            let output = at_frame(app, context, 0, vec![]);
            let point = visible_text_positions(&output, label)
                .last()
                .copied()
                .unwrap_or_else(|| panic!("{label} is visible"));
            let _ = click_at(app, context, 0, point);
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        show_transition_dock_only(&mut app);
        app.intent(
            Intent::Command(Command::AddTransitionPreset {
                name: "Fallback".into(),
                kind: TransitionKind::Slide {
                    direction: TransitionDirection::Up,
                },
            }),
            &context,
        );
        let fallback = app.transition_preset_focus.unwrap();
        app.intent(
            Intent::Command(Command::AddTransitionPreset {
                name: "Remove Me".into(),
                kind: TransitionKind::FadeToColor {
                    rgb: [1, 2, 3],
                    switch_point: 40,
                },
            }),
            &context,
        );
        let id = app.transition_preset_focus.unwrap();
        let reference = TransitionRef::Preset(id);
        let scene = app.session.preview();
        let selection = TransitionSelection::new(reference, 750).unwrap();
        app.intent(
            Intent::Command(Command::SelectTransition { selection }),
            &context,
        );
        app.intent(
            Intent::Command(Command::AddQuickTransitionSelection {
                selection,
                fade_to_black: false,
            }),
            &context,
        );
        app.intent(
            Intent::Command(Command::ConfigureSceneTransitionSelection {
                scene,
                reference: Some(reference),
                duration_ms: 1250,
            }),
            &context,
        );
        let before = app.session.snapshot();
        click(&mut app, &context, "Preset Settings");
        click(&mut app, &context, "Remove Preset");
        assert!(
            matches!(&app.dialog,Some(Dialog::RemoveTransitionPreset {id:target,name,quick_count:1,scene_count:1,fallback,..}) if *target==id&&name=="Remove Me"&&fallback.contains("Fallback"))
        );
        // egui uses a sizing pass for a modal on its first frame. Inspect the
        // following painted frame, as the subsequent synthetic click helper does.
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        assert!(text_position(&output, "Quick transition rows removed: 1").is_some());
        click(&mut app, &context, "Cancel");
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        click(&mut app, &context, "Preset Settings");
        click(&mut app, &context, "Remove Preset");
        click(&mut app, &context, "Delete");
        assert!(!app.error, "{}", app.status);
        assert!(app.dialog.is_none());
        let removed = app.session.snapshot();
        assert_eq!(removed.revision(), before.revision() + 1);
        assert!(removed.transition_preset(id).is_none());
        assert_eq!(
            removed.quick_transitions().len() + 1,
            before.quick_transitions().len()
        );
        assert_eq!(removed.scene(scene).unwrap().transition_override(), None);
        assert_eq!(removed.scene(scene).unwrap().transition_duration_ms(), 1250);
        assert_eq!(
            removed.transition().reference(),
            TransitionRef::Preset(fallback)
        );
        assert_eq!(removed.transition().duration_ms(), 750);
        app.intent(Intent::Command(Command::Undo), &context);
        let restored = app.session.snapshot();
        assert!(restored.transition_preset(id).is_some());
        assert_eq!(restored.quick_transitions(), before.quick_transitions());
        assert_eq!(
            restored.scene(scene).unwrap().transition_override(),
            before.scene(scene).unwrap().transition_override()
        );
        assert_eq!(restored.transition(), before.transition());
    }
    #[test]
    fn named_directional_program_textures_refresh_during_automatic_and_held_manual_outputs() {
        use skylive_core::{TransitionDirection, TransitionKind, TransitionSelection};
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        for swipe in [true, false] {
            let context = egui::Context::default();
            let (mut app, first, second, _, _) = transition_scenes(&context);
            let kind = |direction| {
                if swipe {
                    TransitionKind::Swipe {
                        direction,
                        swipe_in: false,
                    }
                } else {
                    TransitionKind::Slide { direction }
                }
            };
            app.intent(
                Intent::Command(Command::AddTransitionPreset {
                    name: "Live Direction".into(),
                    kind: kind(TransitionDirection::Left),
                }),
                &context,
            );
            let id = app.transition_preset_focus.unwrap();
            app.intent(
                Intent::Command(Command::SelectTransition {
                    selection: TransitionSelection::new(TransitionRef::Preset(id), 300).unwrap(),
                }),
                &context,
            );
            app.intent(Intent::StudioMode(true), &context);
            app.intent(Intent::SelectScene(second), &context);
            app.intent(Intent::Transition, &context);
            wait_preview_pixels(&mut app, &context, 150_000_000, &[(red, blue)]);
            assert_eq!(
                app.program.displayed_pixel().unwrap().0,
                red,
                "the directional texture is the Program pane, not the solid blue Preview"
            );
            wait_preview_pixels(&mut app, &context, 300_000_000, &[(blue, blue)]);
            assert_eq!(
                (app.session.preview(), app.session.program()),
                (first, second)
            );
            app.intent(Intent::ManualPosition(512), &context);
            wait_preview_pixels(&mut app, &context, 10_000_000_000, &[(blue, red)]);
            let held = app.transition_manual.unwrap();
            assert_eq!((held.current, held.target), (0.5, 0.5));
            assert!(
                !app.transition_needs_tick,
                "a settled held T-bar does not run a timer"
            );
            let old_output = app.program.displayed_pixel().unwrap().1;
            let before = app.session.snapshot();
            app.intent(
                Intent::Command(Command::ConfigureTransitionPreset {
                    id,
                    name: "Live Direction".into(),
                    kind: kind(TransitionDirection::Right),
                }),
                &context,
            );
            assert!(!app.error, "{}", app.status);
            assert_eq!(app.session.snapshot().revision(), before.revision() + 1);
            wait_preview_pixels(&mut app, &context, 10_000_000_000, &[(red, blue)]);
            let (pixel, new_output) = app.program.displayed_pixel().unwrap();
            assert_eq!(pixel, red);
            assert_ne!(
                new_output, old_output,
                "shared provider properties invalidate displayed composition identity"
            );
            assert!(app.transition_active);
            assert_eq!(app.transition_manual.unwrap().current, 0.5);
            assert!(!app.transition_needs_tick);
        }
    }
    #[test]
    fn named_preset_widgets_add_select_share_properties_and_preserve_staged_history() {
        use skylive_core::{TransitionDirection, TransitionKind};
        fn click(app: &mut StudioApp, context: &egui::Context, label: &str) {
            let _ = at_frame(app, context, 0, vec![]);
            let output = at_frame(app, context, 0, vec![]);
            let point = visible_text_positions(&output, label)
                .last()
                .copied()
                .unwrap_or_else(|| panic!("semantic control {label} is visible"));
            let _ = click_at(app, context, 0, point);
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        show_transition_dock_only(&mut app);
        let original = app.session.snapshot();
        click(&mut app, &context, "Preset Settings");
        click(&mut app, &context, "Add Transition Preset");
        assert!(matches!(
            app.dialog,
            Some(Dialog::TransitionPreset { id: None, .. })
        ));
        if let Some(Dialog::TransitionPreset { name, .. }) = &mut app.dialog {
            *name = "Shared Swipe".into();
        }
        click(&mut app, &context, "Right");
        click(&mut app, &context, "Swipe In");
        click(&mut app, &context, "Apply");
        assert!(!app.error, "{}", app.status);
        let added = app.session.snapshot();
        let preset = &added.transition_presets()[0];
        let id = preset.id();
        let reference = TransitionRef::Preset(id);
        assert_eq!(
            preset.kind(),
            TransitionKind::Swipe {
                direction: TransitionDirection::Right,
                swipe_in: true
            }
        );
        assert_eq!(added.revision(), original.revision() + 1);
        assert_eq!(
            added.transition(),
            original.transition(),
            "Add creates only a library entry"
        );
        assert_eq!(app.transition_preset_focus, Some(id));
        click(&mut app, &context, "Presets");
        click(&mut app, &context, "Shared Swipe");
        assert_eq!(app.session.snapshot().transition().reference(), reference);
        assert_eq!(app.session.snapshot().revision(), added.revision() + 1);
        app.intent(
            Intent::OpenDialog(Dialog::QuickRow {
                id: None,
                reference: TransitionRef::Fade,
                duration_ms: 750,
                fade_to_black: false,
            }),
            &context,
        );
        click(&mut app, &context, "Presets");
        click(&mut app, &context, "Shared Swipe");
        click(&mut app, &context, "Apply");
        assert_eq!(
            app.session
                .snapshot()
                .quick_transitions()
                .last()
                .unwrap()
                .config()
                .reference(),
            reference
        );
        let scene = app.session.preview();
        app.intent(
            Intent::OpenDialog(Dialog::SceneTransition {
                scene,
                enabled: true,
                reference: TransitionRef::Fade,
                duration_ms: 1250,
            }),
            &context,
        );
        click(&mut app, &context, "Presets");
        click(&mut app, &context, "Shared Swipe");
        click(&mut app, &context, "Apply");
        let shared = app.session.snapshot();
        assert_eq!(
            shared
                .scene(scene)
                .unwrap()
                .transition_override()
                .unwrap()
                .reference(),
            reference
        );
        click(&mut app, &context, "Preset Settings");
        click(&mut app, &context, "Preset Properties");
        click(&mut app, &context, "Slide");
        click(&mut app, &context, "Up");
        click(&mut app, &context, "Cancel");
        assert!(Arc::ptr_eq(&shared, &app.session.snapshot()));
        click(&mut app, &context, "Preset Settings");
        click(&mut app, &context, "Preset Properties");
        if let Some(Dialog::TransitionPreset { name, .. }) = &mut app.dialog {
            *name = "Fade".into();
        }
        click(&mut app, &context, "Apply");
        assert!(app.error);
        assert!(
            matches!(&app.dialog, Some(Dialog::TransitionPreset { name, .. }) if name == "Fade")
        );
        assert!(Arc::ptr_eq(&shared, &app.session.snapshot()));
        if let Some(Dialog::TransitionPreset { name, .. }) = &mut app.dialog {
            *name = "Shared Slide".into();
        }
        click(&mut app, &context, "Slide");
        click(&mut app, &context, "Down");
        click(&mut app, &context, "Apply");
        assert!(!app.error, "{}", app.status);
        let changed = app.session.snapshot();
        assert_eq!(changed.revision(), shared.revision() + 1);
        assert_eq!(
            changed.transition_preset(id).unwrap().name(),
            "Shared Slide"
        );
        for selection in [
            changed.transition(),
            changed.quick_transitions().last().unwrap().config(),
            changed.scene(scene).unwrap().transition_override().unwrap(),
        ] {
            assert_eq!(selection.reference(), reference);
            assert_eq!(
                changed.resolve_transition(selection).unwrap().kind(),
                TransitionKind::Slide {
                    direction: TransitionDirection::Down
                }
            );
        }
        assert_eq!(
            changed
                .quick_transitions()
                .last()
                .unwrap()
                .config()
                .duration_ms(),
            750
        );
        assert_eq!(changed.scene(scene).unwrap().transition_duration_ms(), 1250);
        click(&mut app, &context, "Preset Settings");
        click(&mut app, &context, "Preset Properties");
        click(&mut app, &context, "Apply");
        assert!(
            Arc::ptr_eq(&changed, &app.session.snapshot()),
            "unchanged properties Apply is a no-op"
        );
        app.intent(Intent::Command(Command::Undo), &context);
        let undone = app.session.snapshot();
        assert_eq!(undone.transition_preset(id).unwrap().name(), "Shared Swipe");
        assert_eq!(
            undone.transition_preset(id).unwrap().kind(),
            TransitionKind::Swipe {
                direction: TransitionDirection::Right,
                swipe_in: true
            }
        );
    }
    #[test]
    fn quick_row_and_scene_override_dialogs_cancel_validate_and_commit_one_undo() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        show_transition_dock_only(&mut app);
        let original = app.session.snapshot();
        let add = scroll_to_transition_text(&mut app, &context, 0, "Add Quick Transition");
        let _ = click_at(&mut app, &context, 0, add);
        assert!(matches!(
            app.dialog,
            Some(Dialog::QuickRow { id: None, .. })
        ));
        if let Some(Dialog::QuickRow { duration_ms, .. }) = &mut app.dialog {
            *duration_ms = 750;
        }
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let cancel = text_position(&output, "Cancel").unwrap();
        let _ = click_at(&mut app, &context, 0, cancel);
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        app.intent(
            Intent::OpenDialog(Dialog::QuickRow {
                id: None,
                reference: TransitionRef::Fade,
                duration_ms: 49,
                fade_to_black: false,
            }),
            &context,
        );
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let apply = text_position(&output, "Apply").unwrap();
        let _ = click_at(&mut app, &context, 0, apply);
        assert!(app.error);
        assert!(app.dialog.is_some());
        assert!(Arc::ptr_eq(&original, &app.session.snapshot()));
        if let Some(Dialog::QuickRow { duration_ms, .. }) = &mut app.dialog {
            *duration_ms = 750;
        }
        let output = at_frame(&mut app, &context, 0, vec![]);
        let apply = text_position(&output, "Apply").unwrap();
        let _ = click_at(&mut app, &context, 0, apply);
        assert!(!app.error);
        assert_eq!(app.session.snapshot().quick_transitions().len(), 4);
        let id = app
            .session
            .snapshot()
            .quick_transitions()
            .last()
            .unwrap()
            .id();
        app.intent(Intent::Command(Command::Undo), &context);
        assert_eq!(app.session.snapshot().quick_transitions().len(), 3);
        app.intent(Intent::Command(Command::Redo), &context);
        assert_eq!(
            app.session
                .snapshot()
                .quick_transitions()
                .last()
                .unwrap()
                .id(),
            id
        );
        app.intent(
            Intent::OpenDialog(Dialog::QuickRow {
                id: Some(id),
                reference: TransitionRef::Fade,
                duration_ms: 900,
                fade_to_black: true,
            }),
            &context,
        );
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let apply = text_position(&output, "Apply").unwrap();
        let _ = click_at(&mut app, &context, 0, apply);
        assert!(
            app.session
                .snapshot()
                .quick_transitions()
                .last()
                .unwrap()
                .fade_to_black()
        );
        app.intent(Intent::Command(Command::Undo), &context);
        assert_eq!(
            app.session
                .snapshot()
                .quick_transitions()
                .last()
                .unwrap()
                .config()
                .duration_ms(),
            750
        );
        quick_row_menu_action(&mut app, &context, "Fade (750 ms)", "Move Up");
        assert_eq!(app.session.snapshot().quick_transitions()[2].id(), id);
        quick_row_menu_action(
            &mut app,
            &context,
            "Fade (750 ms)",
            "Remove Quick Transition",
        );
        assert_eq!(app.session.snapshot().quick_transitions().len(), 3);
        app.intent(Intent::Command(Command::Undo), &context);
        assert_eq!(app.session.snapshot().quick_transitions()[2].id(), id);
        let scene = app.session.preview();
        let before = app.session.snapshot();
        app.intent(
            Intent::OpenDialog(Dialog::SceneTransition {
                scene,
                enabled: true,
                reference: TransitionRef::Fade,
                duration_ms: 1000,
            }),
            &context,
        );
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let cancel = text_position(&output, "Cancel").unwrap();
        let _ = click_at(&mut app, &context, 0, cancel);
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        app.intent(
            Intent::OpenDialog(Dialog::SceneTransition {
                scene,
                enabled: true,
                reference: TransitionRef::Fade,
                duration_ms: 49,
            }),
            &context,
        );
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let apply = text_position(&output, "Apply").unwrap();
        let _ = click_at(&mut app, &context, 0, apply);
        assert!(app.error);
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        if let Some(Dialog::SceneTransition { duration_ms, .. }) = &mut app.dialog {
            *duration_ms = 1100;
        }
        let output = at_frame(&mut app, &context, 0, vec![]);
        let apply = text_position(&output, "Apply").unwrap();
        let _ = click_at(&mut app, &context, 0, apply);
        assert!(app.dialog.is_none());
        assert_eq!(
            app.session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override()
                .unwrap()
                .duration_ms(),
            1100
        );
        assert_eq!(app.session.snapshot().revision(), before.revision() + 1);
        app.intent(Intent::Command(Command::Undo), &context);
        assert_eq!(
            app.session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override(),
            None
        );
    }
    #[test]
    fn disabled_scene_override_reopens_remembered_duration_and_no_op_apply_preserves_history() {
        fn open(app: &mut StudioApp, context: &egui::Context) {
            capture_click(app, context, 0, "Transition Override");
            assert!(matches!(app.dialog, Some(Dialog::SceneTransition { .. })));
        }
        fn apply(app: &mut StudioApp, context: &egui::Context) {
            let _ = at_frame(app, context, 0, vec![]);
            let output = at_frame(app, context, 0, vec![]);
            let button = text_position(&output, "Apply").unwrap();
            let _ = click_at(app, context, 0, button);
            assert!(!app.error, "{}", app.status);
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let scene = app.session.preview();
        open(&mut app, &context);
        if let Some(Dialog::SceneTransition {
            enabled,
            duration_ms,
            ..
        }) = &mut app.dialog
        {
            *enabled = true;
            *duration_ms = 750;
        }
        apply(&mut app, &context);
        assert_eq!(
            app.session
                .snapshot()
                .scene(scene)
                .unwrap()
                .transition_override()
                .unwrap()
                .duration_ms(),
            750
        );
        let revision = app.session.snapshot().revision();
        open(&mut app, &context);
        if let Some(Dialog::SceneTransition { enabled, .. }) = &mut app.dialog {
            *enabled = false;
        }
        apply(&mut app, &context);
        let disabled = app.session.snapshot();
        assert_eq!(disabled.revision(), revision + 1);
        assert_eq!(disabled.scene(scene).unwrap().transition_override(), None);
        assert_eq!(disabled.scene(scene).unwrap().transition_duration_ms(), 750);
        open(&mut app, &context);
        assert!(matches!(
            &app.dialog,
            Some(Dialog::SceneTransition {
                enabled: false,
                duration_ms: 750,
                ..
            })
        ));
        apply(&mut app, &context);
        assert!(
            Arc::ptr_eq(&disabled, &app.session.snapshot()),
            "reopened unchanged disabled preference must be a no-op"
        );
        open(&mut app, &context);
        if let Some(Dialog::SceneTransition { duration_ms, .. }) = &mut app.dialog {
            *duration_ms = 900;
        }
        apply(&mut app, &context);
        let changed = app.session.snapshot();
        assert_eq!(changed.revision(), disabled.revision() + 1);
        assert_eq!(changed.scene(scene).unwrap().transition_override(), None);
        assert_eq!(changed.scene(scene).unwrap().transition_duration_ms(), 900);
        app.intent(Intent::Command(Command::Undo), &context);
        let undone = app.session.snapshot();
        assert_eq!(undone.scene(scene).unwrap().transition_override(), None);
        assert_eq!(
            undone.scene(scene).unwrap().transition_duration_ms(),
            750,
            "one Undo restores the inactive preference without re-enabling its override"
        );
    }
    #[test]
    fn real_studio_fade_widgets_present_midpoint_complete_swap_and_quick_cut_before_exit() {
        let context = egui::Context::default();
        let (mut app, first, second, _, _) = transition_scenes(&context);
        let _ = at_frame(&mut app, &context, 0, vec![]);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let fade = text_position(&output, "Fade").unwrap();
        let _ = click_at(&mut app, &context, 0, fade);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let studio = text_position(&output, "Studio Mode").unwrap();
        let _ = click_at(&mut app, &context, 0, studio);
        assert!(app.studio);
        let output = at_frame(&mut app, &context, 0, vec![]);
        let target = text_position(&output, "Second scene").unwrap();
        let _ = click_at(&mut app, &context, 0, target);
        assert_eq!(
            (app.session.preview(), app.session.program()),
            (second, first)
        );
        let output = at_frame(&mut app, &context, 0, vec![]);
        let start = text_position(&output, "Transition").unwrap();
        let _ = click_at(&mut app, &context, 0, start);
        assert!(app.transition_active);
        let epoch = app
            .transition
            .sync(&mut app.session, MediaTime(0))
            .unwrap()
            .epoch;
        let output = at_frame(&mut app, &context, 0, vec![]);
        assert!(text_position(&output, "Transition in progress").is_some());
        let disabled = text_position(&output, "Transition").unwrap();
        let _ = click_at(&mut app, &context, 0, disabled);
        assert_eq!(
            app.transition
                .sync(&mut app.session, MediaTime(0))
                .unwrap()
                .epoch,
            epoch,
            "busy Transition widget must not start another operation"
        );
        wait_preview_pixels(
            &mut app,
            &context,
            150_000_000,
            &[([128, 0, 128, 255], [128, 0, 128, 255])],
        );
        assert!(app.transition_active);
        wait_preview_pixels(
            &mut app,
            &context,
            300_000_000,
            &[([0, 0, 255, 255], [0, 0, 255, 255])],
        );
        assert!(!app.transition_active);
        assert_eq!(
            (app.session.preview(), app.session.program()),
            (first, second)
        );
        let output = at_frame(&mut app, &context, 300_000_000, vec![]);
        let quick = text_position(&output, "Quick Cut").unwrap();
        let _ = click_at(&mut app, &context, 300_000_000, quick);
        assert_eq!(
            (app.session.preview(), app.session.program()),
            (second, first)
        );
        assert!(!app.transition_active);
        let output = at_frame(&mut app, &context, 300_000_000, vec![]);
        let studio = text_position(&output, "Studio Mode").unwrap();
        let _ = click_at(&mut app, &context, 300_000_000, studio);
        assert!(!app.studio);
        assert_eq!(
            (app.session.preview(), app.session.program()),
            (first, first),
            "Studio exit must keep Program A, not unsent Preview B"
        );
        wait_preview_pixels(
            &mut app,
            &context,
            300_000_000,
            &[([255, 0, 0, 255], [255, 0, 0, 255])],
        );
    }
    #[test]
    fn studio_layout_edits_stay_private_shared_properties_live_and_last_source_is_retained() {
        let context = egui::Context::default();
        let (mut app, first, _, item, source) = transition_scenes(&context);
        app.intent(Intent::StudioMode(true), &context);
        assert!(app.studio);
        let mut half = skylive_core::Transform::full(1920, 1080);
        half.width = 960.0;
        app.intent(
            Intent::Command(Command::SetTransform {
                scene: first,
                item,
                transform: half,
            }),
            &context,
        );
        app.intent(
            Intent::Command(Command::SetColor {
                source,
                rgba: [0, 255, 0, 255],
            }),
            &context,
        );
        wait_preview_pixels(
            &mut app,
            &context,
            0,
            &[
                ([0, 255, 0, 255], [0, 0, 0, 255]),
                ([0, 255, 0, 255], [0, 255, 0, 255]),
            ],
        );
        app.intent(
            Intent::Command(Command::RemoveItem { scene: first, item }),
            &context,
        );
        assert!(app.session.snapshot().source(source).is_none());
        wait_preview_pixels(
            &mut app,
            &context,
            0,
            &[
                ([0, 0, 0, 255], [0, 0, 0, 255]),
                ([0, 255, 0, 255], [0, 255, 0, 255]),
            ],
        );
        app.intent(Intent::StudioMode(false), &context);
        assert_eq!(app.session.program(), first);
        // OBS exits to the LIVE authored logical Program scene. Its private
        // Studio layout is deliberately retired, so the authored empty layout
        // now becomes visible while the Program scene identity stays unchanged.
        let _ = at_frame(&mut app, &context, 0, vec![]);
        assert_eq!(
            app.preview.displayed_pixel(),
            Some((
                [0, 0, 0, 255],
                skylive_host::transition::OutputKey::Scene(
                    skylive_host::transition::SceneKey::Live(first)
                )
            )),
            "the already presented black live Preview texture is reused on Studio exit"
        );
    }
    #[test]
    fn image_widgets_prepare_in_background_retain_invalid_draft_reload_and_undo() {
        use skylive_core::SourceKind;
        let temp = Temp::new();
        let image_path = temp.0.join("image.bmp");
        write_bmp(&image_path, 2, 3, [30, 60, 90]);
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let before = app.session.snapshot();
        let _ = frame(&mut app, &context, vec![]);
        let _ = frame(&mut app, &context, vec![]);
        let add = toolbar_point(&context, "Sources", "Add");
        let _ = click(&mut app, &context, add);
        let output = frame(&mut app, &context, vec![]);
        let image =
            text_position(&output, "Image").expect("Sources Add menu offers a real Image provider");
        let _ = click(&mut app, &context, image);
        assert!(matches!(app.dialog, Some(Dialog::AddImage { .. })));
        let bad_path = temp.0.join("missing.bmp").to_string_lossy().into_owned();
        if let Some(Dialog::AddImage { path, .. }) = &mut app.dialog {
            *path = bad_path.clone();
        }
        let _ = frame(&mut app, &context, vec![]);
        let output = frame(&mut app, &context, vec![]);
        let apply = text_position(&output, "Apply").unwrap();
        let _ = click(&mut app, &context, apply);
        assert!(app.io.busy());
        settle_io(&mut app, &context);
        assert!(app.error);
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        assert!(matches!(&app.dialog,Some(Dialog::AddImage {path,..}) if path==&bad_path));
        if let Some(Dialog::AddImage { path, .. }) = &mut app.dialog {
            *path = image_path.to_string_lossy().into_owned();
        }
        let output = frame(&mut app, &context, vec![]);
        let apply = text_position(&output, "Apply").unwrap();
        let _ = click(&mut app, &context, apply);
        assert!(app.io.busy());
        app.intent(
            Intent::Command(Command::AddScene {
                name: "Must not publish during image preparation".into(),
            }),
            &context,
        );
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        settle_io(&mut app, &context);
        assert!(!app.error, "{}", app.status);
        assert!(app.dialog.is_none());
        let added = app.session.snapshot();
        assert_eq!(added.revision(), before.revision() + 1);
        let source = added.sources()[0].id();
        let SourceKind::Image {
            asset: original_asset,
        } = added.sources()[0].kind()
        else {
            panic!("Image command must publish an image resource");
        };
        assert_eq!((original_asset.width(), original_asset.height()), (2, 3));
        let original_hash = original_asset.sha256().to_owned();
        app.intent(Intent::Command(Command::Undo), &context);
        assert!(
            app.session.snapshot().sources().is_empty(),
            "prepared Add Image is exactly one undoable edit"
        );
        app.intent(Intent::Command(Command::Redo), &context);
        app.selected = Some(app.session.snapshot().scenes()[0].items()[0].id());
        let _ = frame(&mut app, &context, vec![]);
        let properties = toolbar_point(&context, "Sources", "Properties");
        let _ = click(&mut app, &context, properties);
        assert!(
            matches!(&app.dialog,Some(Dialog::ImageFile {source:actual,path,..}) if *actual==source&&path==original_asset.path())
        );
        write_bmp(&image_path, 3, 4, [90, 60, 30]);
        let _ = frame(&mut app, &context, vec![]);
        let output = frame(&mut app, &context, vec![]);
        let apply = text_position(&output, "Apply").unwrap();
        let _ = click(&mut app, &context, apply);
        settle_io(&mut app, &context);
        assert!(!app.error, "{}", app.status);
        assert!(app.dialog.is_none());
        let replaced = app.session.snapshot();
        assert_eq!(replaced.sources()[0].id(), source);
        let SourceKind::Image { asset: replacement } = replaced.sources()[0].kind() else {
            panic!("replacement must retain source type");
        };
        assert_eq!((replacement.width(), replacement.height()), (3, 4));
        assert_ne!(replacement.sha256(), &original_hash);
        app.intent(Intent::Command(Command::Undo), &context);
        let undone = app.session.snapshot();
        assert_eq!(undone.sources()[0].id(), source);
        let SourceKind::Image { asset: restored } = undone.sources()[0].kind() else {
            panic!("Undo must restore the image resource");
        };
        assert_eq!((restored.width(), restored.height()), (2, 3));
        assert_eq!(restored.sha256(), &original_hash);
        app.intent(Intent::Command(Command::Redo), &context);

        std::fs::remove_file(&image_path).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let output = frame(&mut app, &context, vec![]);
            if let Some(diagnostic) = app
                .preview
                .diagnostics()
                .iter()
                .find(|diagnostic| diagnostic.source == source)
            {
                let message = format!("Image [{}]: {}", diagnostic.code, diagnostic.detail);
                assert!(
                    text_position(&output, &message).is_some(),
                    "source resource failure is visibly projected beside preview"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "missing image did not produce a preview diagnostic"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    #[test]
    fn prepared_image_cannot_publish_after_its_owner_revision_changes() {
        let temp = Temp::new();
        let image_path = temp.0.join("stale.bmp");
        write_bmp(&image_path, 2, 2, [20, 30, 40]);
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let scene = app.session.preview();
        let path = image_path.to_string_lossy().into_owned();
        app.dialog = Some(Dialog::AddImage {
            settings:Default::default(),
            scene,
            name: "Stale image".into(),
            path: path.clone(),
        });
        app.intent(
            Intent::PrepareImage {
                path,
                edit: ImageEdit::Add {
                    scene,
                    name: "Stale image".into(),
                },
            },
            &context,
        );
        assert!(app.io.busy());
        // Simulate another application adapter advancing the owner while the
        // GUI is disabled; freshness must be enforced at publication as well.
        app.session
            .apply(Command::AddScene {
                name: "Concurrent edit".into(),
            })
            .unwrap();
        let latest = app.session.snapshot();
        settle_io(&mut app, &context);
        assert!(app.error);
        assert!(Arc::ptr_eq(&latest, &app.session.snapshot()));
        assert!(app.session.snapshot().sources().is_empty());
        assert!(matches!(&app.dialog,Some(Dialog::AddImage {name,..}) if name=="Stale image"));
        assert!(app.pending_image.is_none());
    }
    #[test]
    fn identical_png_reload_recovers_cached_failure_without_authored_edit() {
        // A real 1x1 RGBA PNG containing opaque red. The test's independent
        // output oracle inspects the presented egui texture upload below.
        const PNG: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ];
        fn has_red_preview_upload(output: &egui::FullOutput) -> bool {
            output
                .textures_delta
                .set
                .iter()
                .any(|(_, delta)| match &delta.image {
                    egui::ImageData::Color(image) => {
                        image.size == [640, 360]
                            && image.pixels.first() == Some(&egui::Color32::from_rgb(255, 0, 0))
                    }
                    _ => false,
                })
        }
        let temp = Temp::new();
        let image_path = temp.0.join("restored.png");
        std::fs::write(&image_path, PNG).unwrap();
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let scene = app.session.preview();
        let path = image_path.to_string_lossy().into_owned();
        app.dialog = Some(Dialog::AddImage {
            settings:Default::default(),
            scene,
            name: "Restored PNG".into(),
            path: path.clone(),
        });
        app.intent(
            Intent::PrepareImage {
                path,
                edit: ImageEdit::Add {
                    scene,
                    name: "Restored PNG".into(),
                },
            },
            &context,
        );
        settle_io(&mut app, &context);
        assert!(!app.error, "{}", app.status);
        let source = app.session.snapshot().sources()[0].id();
        let item = app.selected.unwrap();
        app.intent(
            Intent::Command(Command::SetTransform {
                scene,
                item,
                transform: skylive_core::Transform::full(1920, 1080),
            }),
            &context,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let output = frame(&mut app, &context, vec![]);
            if has_red_preview_upload(&output) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "initial PNG did not produce a real red preview texture"
            );
            std::thread::yield_now();
        }
        std::fs::remove_file(&image_path).unwrap();
        // Authored revision change makes this a fresh resolution rather than
        // continuing the previously accepted decoded cache entry.
        app.intent(
            Intent::Command(Command::RenameCollection {
                name: "Cache invalidation".into(),
            }),
            &context,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let failure_output = loop {
            let output = frame(&mut app, &context, vec![]);
            if app
                .preview
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.source == source)
            {
                break output;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "missing PNG did not produce a fresh cached diagnostic"
            );
            std::thread::yield_now();
        };
        let before = app.session.snapshot();
        let before_revision = before.revision();
        std::fs::write(&image_path, PNG).unwrap();
        let _ = failure_output;
        let properties = toolbar_point(&context, "Sources", "Properties");
        let _ = click(&mut app, &context, properties);
        assert!(
            matches!(&app.dialog,Some(Dialog::ImageFile {source:actual,..}) if *actual==source)
        );
        let _ = frame(&mut app, &context, vec![]);
        let output = frame(&mut app, &context, vec![]);
        let apply = text_position(&output, "Apply").expect("Image Properties Apply is painted");
        let _ = click(&mut app, &context, apply);
        assert!(app.io.busy());
        settle_io(&mut app, &context);
        assert!(!app.error, "{}", app.status);
        assert!(app.dialog.is_none());
        assert!(
            Arc::ptr_eq(&before, &app.session.snapshot()),
            "identical descriptor Reload must be a canonical no-op"
        );
        assert_eq!(app.session.snapshot().revision(), before_revision);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let output = frame(&mut app, &context, vec![]);
            if app.preview.diagnostics().is_empty() && has_red_preview_upload(&output) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "same-descriptor Reload did not retry the cached failure and restore real red pixels"
            );
            std::thread::yield_now();
        }
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        app.intent(Intent::Command(Command::Undo), &context);
        assert_eq!(
            app.session.snapshot().name(),
            "Untitled",
            "no-op Reload must not create an undo entry after the collection rename"
        );
        assert_eq!(app.session.snapshot().sources()[0].id(), source);
    }
    #[test]
    fn transform_dialog_cancel_and_apply_publish_rotation_crop_and_flips_as_one_edit() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        app.intent(
            Intent::Command(Command::AddColorSource {
                scene: app.session.preview(),
                name: "Transform target".into(),
                rgba: [20, 40, 60, 255],
            }),
            &context,
        );
        let scene = app.session.preview();
        let item = app.selected.expect("new color item is selected");
        let before = app.session.snapshot();
        let original = before.scene(scene).unwrap().items()[0].transform().clone();
        let mut expected = original.clone();
        expected.x = 30.0;
        expected.y = 40.0;
        expected.width = 640.0;
        expected.height = 360.0;
        expected.rotation_degrees = 37.5;
        expected.crop.left = 5;
        expected.crop.top = 7;
        expected.crop.right = 9;
        expected.crop.bottom = 11;
        let edit =
            views::menus::action_intent("actionEditTransform", &app.session, Some(item), false)
                .unwrap();
        app.intent(edit, &context);
        if let Some(Dialog::Transform { draft, .. }) = &mut app.dialog {
            *draft = expected.clone();
        } else {
            panic!("transform action must open a draft dialog");
        }
        let _ = frame(&mut app, &context, vec![]);
        let output = frame(&mut app, &context, vec![]);
        let horizontal = text_position(&output, "Flip Horizontally")
            .expect("horizontal flip has a semantic label");
        let output = click(&mut app, &context, horizontal);
        let vertical =
            text_position(&output, "Flip Vertically").expect("vertical flip has a semantic label");
        let output = click(&mut app, &context, vertical);
        assert!(
            matches!(&app.dialog, Some(Dialog::Transform { draft, .. }) if draft.flip_x && draft.flip_y)
        );
        assert!(
            Arc::ptr_eq(&before, &app.session.snapshot()),
            "draft widgets must not publish model state"
        );
        let cancel =
            text_position(&output, "Cancel").expect("transform modal has a Cancel control");
        let _ = click(&mut app, &context, cancel);
        assert!(app.dialog.is_none());
        assert!(
            Arc::ptr_eq(&before, &app.session.snapshot()),
            "Cancel must preserve state, revision and history"
        );

        expected.flip_x = true;
        expected.flip_y = true;
        let edit =
            views::menus::action_intent("actionEditTransform", &app.session, Some(item), false)
                .unwrap();
        app.intent(edit, &context);
        if let Some(Dialog::Transform { draft, .. }) = &mut app.dialog {
            *draft = expected.clone();
        } else {
            panic!("transform action must open a draft dialog");
        }
        let _ = frame(&mut app, &context, vec![]);
        let output = frame(&mut app, &context, vec![]);
        let apply = text_position(&output, "Apply").expect("transform modal has an Apply control");
        let _ = click(&mut app, &context, apply);
        assert!(app.dialog.is_none());
        assert!(!app.error, "{}", app.status);
        let after = app.session.snapshot();
        assert_eq!(after.revision(), before.revision() + 1);
        assert_eq!(
            after.scene(scene).unwrap().items()[0].transform(),
            &expected
        );
        app.intent(Intent::Command(Command::Undo), &context);
        let undone = app.session.snapshot();
        assert_eq!(
            undone.sources().len(),
            1,
            "one Undo must restore the transform, not remove the source"
        );
        assert_eq!(
            undone.scene(scene).unwrap().items()[0].transform(),
            &original
        );
        app.intent(Intent::Command(Command::Redo), &context);
        assert_eq!(
            app.session.snapshot().scene(scene).unwrap().items()[0].transform(),
            &expected
        );
    }
    #[test]
    fn invalid_transform_apply_keeps_exact_draft_and_can_be_repaired() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        app.intent(
            Intent::Command(Command::AddColorSource {
                scene: app.session.preview(),
                name: "Crop validation target".into(),
                rgba: [20, 40, 60, 255],
            }),
            &context,
        );
        let scene = app.session.preview();
        let item = app.selected.unwrap();
        let before = app.session.snapshot();
        let original = before.scene(scene).unwrap().items()[0].transform().clone();
        let mut invalid = original.clone();
        invalid.rotation_degrees = 47.5;
        invalid.flip_x = true;
        invalid.crop.left = u32::MAX;
        invalid.crop.top = 7;
        let edit =
            views::menus::action_intent("actionEditTransform", &app.session, Some(item), false)
                .unwrap();
        app.intent(edit, &context);
        if let Some(Dialog::Transform { draft, .. }) = &mut app.dialog {
            *draft = invalid.clone();
        } else {
            panic!("edit transform must open a draft dialog");
        }
        let _ = frame(&mut app, &context, vec![]);
        let output = frame(&mut app, &context, vec![]);
        let apply = text_position(&output, "Apply").expect("transform Apply control is painted");
        let _ = click(&mut app, &context, apply);
        assert!(
            app.error,
            "the engine must reject crop beyond source dimensions"
        );
        assert!(
            Arc::ptr_eq(&before, &app.session.snapshot()),
            "rejection must retain the exact snapshot and revision"
        );
        assert!(
            matches!(&app.dialog, Some(Dialog::Transform { draft, .. }) if draft == &invalid),
            "the complete invalid draft must remain available for correction"
        );
        let output = frame(&mut app, &context, vec![]);
        assert!(
            text_position(&output, &app.status).is_some(),
            "the rejection message must be painted"
        );

        let mut repaired = invalid;
        repaired.crop.left = 9;
        if let Some(Dialog::Transform { draft, .. }) = &mut app.dialog {
            *draft = repaired.clone();
        } else {
            panic!("rejected draft must remain open");
        }
        let output = frame(&mut app, &context, vec![]);
        let apply =
            text_position(&output, "Apply").expect("Apply remains available after rejection");
        let _ = click(&mut app, &context, apply);
        assert!(
            app.dialog.is_none(),
            "successful publication closes the draft"
        );
        assert!(!app.error, "{}", app.status);
        let after = app.session.snapshot();
        assert_eq!(after.revision(), before.revision() + 1);
        assert_eq!(
            after.scene(scene).unwrap().items()[0].transform(),
            &repaired
        );
        app.intent(Intent::Command(Command::Undo), &context);
        let undone = app.session.snapshot();
        assert_eq!(undone.sources().len(), 1);
        assert_eq!(
            undone.scene(scene).unwrap().items()[0].transform(),
            &original,
            "failed Apply must not add a history step"
        );
    }
    #[test]
    fn actual_obs_import_widget_uses_worker_preserves_ids_and_separate_native_path() {
        let temp = Temp::new();
        let source = temp.0.join("original.obs.json");
        let native = temp.0.join("primary.skylive.json");
        let exported = temp.0.join("exported.obs.json");
        std::fs::write(
            &source,
            include_bytes!("../../../crates/host/tests/fixtures/obs-color-absolute.json"),
        )
        .unwrap();
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        app.apply(Command::Profile {
            command: skylive_core::ProfileCommand::Duplicate {
                name: "Preserved OBS profile".into(),
            },
        });
        let settings = skylive_core::AudioSettings::new(44100, skylive_core::SpeakerLayout::Mono)
            .unwrap()
            .with_buffering(skylive_core::AudioBuffering::low_latency())
            .unwrap();
        app.apply(Command::SetAudioSettings { settings });
        let preserved = app.session.snapshot();
        let identity = app.session.identity();
        assert!(app.session.mark_saved(identity, preserved.revision()));
        app.obs_path = source.to_string_lossy().into_owned();
        app.document_path = native.to_string_lossy().into_owned();
        let _ = frame(&mut app, &context, vec![]);
        let output = frame(&mut app, &context, vec![]);
        let file = text_position(&output, "File").expect("File menu is painted");
        let _ = click(&mut app, &context, file);
        let _ = frame(&mut app, &context, vec![]);
        let output = frame(&mut app, &context, vec![]);
        let button =
            text_position(&output, "Import OBS…").expect("OBS import File menu control is painted");
        let _ = click(&mut app, &context, button);
        assert!(matches!(
            app.documents.prompt,
            Some(views::document::Prompt::Choose(ref draft))
                if draft.action == views::document_picker::DocumentAction::ImportObs
        ));
        let _ = frame(&mut app, &context, vec![]);
        let output = frame(&mut app, &context, vec![]);
        let confirm = text_position(&output, "Import").expect("OBS import confirmation is painted");
        let _ = click(&mut app, &context, confirm);
        assert!(app.io.busy());
        app.intent(
            Intent::Command(Command::AddScene {
                name: "Must not publish during import".into(),
            }),
            &context,
        );
        assert_eq!(app.session.snapshot().revision(), preserved.revision());
        settle_io(&mut app, &context);
        assert!(!app.error, "{}", app.status);
        assert_eq!(app.session.snapshot().scenes().len(), 2);
        assert_eq!(app.session.snapshot().sources().len(), 1);
        assert!(!app.session.can_undo());
        assert!(app.session.is_dirty());
        assert!(app.obs_identities.is_some());
        assert_eq!(app.session.snapshot().profiles(), preserved.profiles());
        assert_eq!(
            app.session.snapshot().preferences(),
            preserved.preferences()
        );
        assert_eq!(app.document_path, native.to_string_lossy());
        app.intent(
            Intent::ExportObs {
                path: exported.to_string_lossy().into_owned(),
            },
            &context,
        );
        settle_io(&mut app, &context);
        assert!(!app.error, "{}", app.status);
        assert!(
            app.session.is_dirty(),
            "OBS export must not mark native changes saved"
        );
        let bytes = std::fs::read(&exported).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let color = json["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["name"] == "Shared Color")
            .unwrap();
        assert_eq!(color["uuid"], "1457ed57-09de-4c8e-aad8-3708dd393b9b");
        app.intent(
            Intent::SaveAsTo {
                path: native.to_string_lossy().into_owned(),
            },
            &context,
        );
        settle_io(&mut app, &context);
        assert!(!app.error, "{}", app.status);
        assert!(!app.session.is_dirty());
        assert!(app.obs_identities.is_some());
        app.intent(Intent::Load, &context);
        settle_io(&mut app, &context);
        assert!(!app.error, "{}", app.status);
        assert!(app.obs_identities.is_none());
        assert_eq!(app.session.snapshot().profiles(), preserved.profiles());
        assert_eq!(
            app.session.snapshot().preferences(),
            preserved.preferences()
        );
        let before = app.session.snapshot();
        std::fs::write(
            &source,
            b"{\"name\":\"Unsupported\",\"version\":2,\"sources\":[]}",
        )
        .unwrap();
        app.intent(
            Intent::ImportObs {
                path: source.to_string_lossy().into_owned(),
            },
            &context,
        );
        settle_io(&mut app, &context);
        assert!(app.error);
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        assert_eq!(app.document_path, native.to_string_lossy());
    }
    #[test]
    fn window_picker_apply_cancel_and_stale_receipts_share_capture_owner_without_native_calls() {
        let context = egui::Context::default();
        let state = FakeCaptureState::default();
        let mut app =
            StudioApp::with_capture_backend(&context, Box::new(FakeCapture(state.clone())))
                .unwrap();
        let scene = app.session.preview();
        let before = app.session.snapshot();
        app.intent(
            Intent::OpenWindowCapture {
                edit: views::WindowEdit::Add { scene },
                name: "Window".into(),
                binding: None,
            },
            &context,
        );
        assert_eq!(state.enumerations.load(Ordering::SeqCst), 0);
        assert_eq!(state.frames.load(Ordering::SeqCst), 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let _ = at_frame(&mut app, &context, 0, vec![]);
            if matches!(&app.dialog,Some(Dialog::WindowCapture(draft))if !draft.loading) {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        let intent = match &app.dialog {
            Some(Dialog::WindowCapture(draft)) => draft.intent().unwrap(),
            _ => panic!("window picker"),
        };
        app.intent(intent, &context);
        assert!(!app.error, "{}", app.status);
        let model = app.session.snapshot();
        let source = model
            .sources()
            .iter()
            .find(|s| matches!(s.kind(), skylive_core::SourceKind::WindowCapture { .. }))
            .unwrap()
            .id();
        assert_eq!(state.enumerations.load(Ordering::SeqCst), 1);
        app.intent(Intent::Command(Command::Undo), &context);
        assert!(app.session.snapshot().source(source).is_none());
        app.intent(Intent::Command(Command::Redo), &context);
        assert!(app.session.snapshot().source(source).is_some());
        let unchanged = app.session.snapshot();
        app.intent(
            Intent::OpenWindowCapture {
                edit: views::WindowEdit::Rebind { source },
                name: "Window".into(),
                binding: None,
            },
            &context,
        );
        app.dialog = None;
        assert!(Arc::ptr_eq(&unchanged, &app.session.snapshot()));
        app.intent(
            Intent::CommitWindow {
                session: app.session.identity(),
                revision: before.revision(),
                edit: views::WindowEdit::Rebind { source },
                name: "Window".into(),
                binding: skylive_core::WindowCaptureBinding::new(
                    skylive_core::WindowTarget::Windows {
                        title: "Fake Editor".into(),
                        class: "FakeEditorClass".into(),
                        executable: "fake-editor.exe".into(),
                    },
                    skylive_core::WindowMatchPriority::Title,
                    false,
                    true,
                    2,
                    2,
                )
                .unwrap(),
            },
            &context,
        );
        assert!(app.error);
        assert!(Arc::ptr_eq(&unchanged, &app.session.snapshot()));
        app.capture.join().unwrap();
    }
    #[test]
    fn camera_picker_apply_noop_restart_and_cancel_use_only_injected_backend() {
        use skylive_core::{
            VideoCaptureBinding, VideoCaptureMode, VideoCaptureTarget, VideoPixelFormat,
        };
        use skylive_host::camera_capture::{
            CameraCapabilities, CameraDeviceInfo, CameraLimits, CameraModeInfo,
            NativeCameraBackend, NativeCameraStream, NativeVideoFrame,
        };
        struct Backend {
            counts: Arc<[AtomicUsize; 3]>,
            _local: std::rc::Rc<()>,
        }
        struct Stream {
            emitted: bool,
        }
        fn target() -> VideoCaptureTarget {
            VideoCaptureTarget::Windows {
                symbolic_link: "ui-camera".into(),
            }
        }
        fn mode() -> VideoCaptureMode {
            VideoCaptureMode::new(2, 2, 30, 1, VideoPixelFormat::Rgb32).unwrap()
        }
        impl NativeCameraBackend for Backend {
            fn enumerate_devices(
                &mut self,
                _: CameraLimits,
            ) -> Result<Vec<CameraDeviceInfo>, CameraError> {
                self.counts[1].fetch_add(1, Ordering::SeqCst);
                Ok(vec![CameraDeviceInfo {
                    target: target(),
                    name: "Fake camera".into(),
                }])
            }
            fn enumerate_modes(
                &mut self,
                _: &VideoCaptureTarget,
                _: CameraLimits,
            ) -> Result<Vec<CameraModeInfo>, CameraError> {
                Ok(vec![CameraModeInfo {
                    width: 2,
                    height: 2,
                    fps_num: 30,
                    fps_den: 1,
                    native_subtype: "RGB32".into(),
                    mode: Some(mode()),
                    unsupported_reason: None,
                }])
            }
            fn open(
                &mut self,
                _: &VideoCaptureBinding,
                _: CameraLimits,
            ) -> Result<Box<dyn NativeCameraStream>, CameraError> {
                self.counts[2].fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Stream { emitted: false }))
            }
        }
        impl NativeCameraStream for Stream {
            fn poll(&mut self, _: CameraLimits) -> Result<Option<NativeVideoFrame>, CameraError> {
                if self.emitted {
                    return Ok(None);
                }
                self.emitted = true;
                Ok(Some(NativeVideoFrame {
                    image: skylive_media::RgbaFrame::solid(2, 2, [42, 80, 120, 255]).unwrap(),
                    presentation_ns: 0,
                    device_qpc_ns: None,
                    directshow_clock: None,
                    discontinuity: false,
                    dropped_frames: 0,
                }))
            }
            fn stop(&mut self) -> Result<bool, CameraError> {
                Ok(true)
            }
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        app.camera.join().unwrap();
        let counts = Arc::new(std::array::from_fn(|_| AtomicUsize::new(0)));
        let worker = counts.clone();
        app.camera = CameraOwner::new(
            Box::new(move || {
                worker[0].fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Backend {
                    counts: worker,
                    _local: std::rc::Rc::new(()),
                }))
            }),
            CameraCapabilities {
                available: true,
                backend: "fake",
                video: true,
                audio: false,
                note: None,
            },
            CameraLimits::default(),
        )
        .unwrap();
        let scene = app.session.preview();
        let before = app.session.snapshot();
        app.intent(
            Intent::OpenCameraCapture {
                edit: views::CameraEdit::Add { scene },
                name: "Camera".into(),
                binding: None,
            },
            &context,
        );
        assert_eq!(counts[0].load(Ordering::SeqCst), 0);
        let end = Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let _ = at_frame(&mut app, &context, 0, vec![]);
            if matches!(&app.dialog,Some(Dialog::CameraCapture(d))if !d.loading()) {
                break;
            }
            assert!(Instant::now() < end);
            std::thread::yield_now();
        }
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        assert_eq!(counts[2].load(Ordering::SeqCst), 0);
        app.intent(Intent::QueryCameraModes, &context);
        let end = Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let _ = at_frame(&mut app, &context, 0, vec![]);
            if matches!(&app.dialog,Some(Dialog::CameraCapture(d))if !d.loading()&&d.selected_mode.is_some())
            {
                break;
            }
            assert!(Instant::now() < end);
            std::thread::yield_now();
        }
        let intent = match &app.dialog {
            Some(Dialog::CameraCapture(d)) => d.intent().unwrap(),
            _ => panic!("camera picker"),
        };
        app.intent(intent, &context);
        assert!(!app.error, "{}", app.status);
        let source = app
            .session
            .snapshot()
            .sources()
            .iter()
            .find(|s| matches!(s.kind(), skylive_core::SourceKind::VideoCapture { .. }))
            .unwrap()
            .id();
        let end = Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let _ = at_frame(&mut app, &context, 0, vec![]);
            if app.camera.snapshot().frame(source).is_some() {
                break;
            }
            assert!(Instant::now() < end);
            std::thread::yield_now();
        }
        assert_eq!(counts[2].load(Ordering::SeqCst), 1);
        let before = app.session.snapshot();
        let binding = VideoCaptureBinding::new(target(), mode(), false).unwrap();
        app.intent(
            Intent::CommitCamera {
                session: app.session.identity(),
                revision: before.revision(),
                edit: views::CameraEdit::Rebind { source },
                name: "Camera".into(),
                binding: binding.clone(),
            },
            &context,
        );
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        let end = Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let _ = at_frame(&mut app, &context, 0, vec![]);
            if counts[2].load(Ordering::SeqCst) == 2
                && app.camera.snapshot().frame(source).is_some()
            {
                break;
            }
            assert!(Instant::now() < end);
            std::thread::yield_now();
        }
        app.intent(
            Intent::OpenCameraCapture {
                edit: views::CameraEdit::Rebind { source },
                name: "Camera".into(),
                binding: Some(binding),
            },
            &context,
        );
        app.dialog = None;
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        app.capture_union.clear();
        app.preview.close();
        app.program.close();
        app.camera.close();
    }
    #[test]
    fn scene_actions_toolbar_opens_scene_filter_draft_without_creating_an_owner() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let scene = app.session.preview();
        let before = app.session.snapshot();
        assert!(before.scene_source(scene).is_none());
        capture_click(&mut app, &context, 0, "Scene Actions");
        capture_click(&mut app, &context, 0, "Filters");
        let Some(Dialog::VideoFilters(draft)) = &app.dialog else {
            panic!("Scene toolbar must reach the real filter editor")
        };
        assert!(
            matches!(draft.intent().unwrap(), Intent::ApplySceneVideoFilters { scene: id, .. } if id == scene)
        );
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        capture_click(&mut app, &context, 0, "Cancel");
        assert!(app.dialog.is_none());
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        assert!(app.session.snapshot().scene_source(scene).is_none());
    }
}

#[cfg(test)]
#[path = "views/dialog_layout_tests.rs"]
mod dialog_layout_tests;

#[cfg(test)]
#[path = "app_scene_migration_tests.rs"]
mod scene_migration_tests;

#[cfg(test)]
#[path = "app_status_tests.rs"]
mod status_tests;

#[cfg(test)]
#[path = "app_dock_columns_tests.rs"]
mod dock_columns_tests;

#[cfg(test)]
#[path = "app_obs32_layout_tests.rs"]
mod obs32_layout_tests;

#[cfg(test)]
#[path = "app_setup_tests.rs"]
mod setup_tests;

#[cfg(test)]
#[path = "app_edit_clipboard_tests.rs"]
mod edit_clipboard_tests;

impl StudioApp {
    fn accept_application_processes(&mut self,key:AudioRequestKey,result:Result<Vec<skylive_host::audio_capture::AudioDeviceInfo>,AudioCaptureError>) {
        let pending=self.pending_audio_devices.take();
        if let Some(Dialog::AudioCapture {target:dialogs::AudioTargetChoice::WindowsProcess,device_picker:Some(picker),..})=&mut self.dialog {
            if pending!=Some((picker.token,key)){return;}
            picker.loading=false;
            if picker.session!=key.session || picker.revision!=key.revision || self.session.identity()!=key.session || self.session.snapshot().revision()!=key.revision {
                picker.error=Some("The audio device request belongs to an obsolete draft. Reopen this dialog.".into());return;
            }
            match result {Ok(devices)=>{picker.devices=devices;picker.error=None;},Err(error)=>picker.error=Some(error.to_string())}
        }
    }
}
