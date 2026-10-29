//! Bounded staged manual setup. Publication belongs to the app/Session owner.
use super::{
    Intent,
    modal_footer::{self, Action},
    recording_settings::RecordingDraft,
    streaming_settings::StreamingDraft,
};
use eframe::egui;
use skylive_core::{AudioSlot, Command, ProfileId, Session, SetupCommand, SetupConfiguration, SetupDefaultAudio, VideoSettings};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupOrigin {
    FirstRun,
    Tools,
    NewProfile,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupToken {
    pub session: u64,
    pub revision: u64,
    pub profile: ProfileId,
    pub generation: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupAction {
    Back,
    Next,
    Apply,
    Skip,
    Cancel,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct SetupCapabilities {
    pub recording: bool,
    pub streaming: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Usage,
    Video,
    Output,
    Review,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Usage {
    Recording,
    Streaming,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VideoPreset {
    Current,
    P720,
    P1080,
    P1080Fast,
}
struct Wizard {
    token: SetupToken,
    origin: SetupOrigin,
    capabilities: SetupCapabilities,
    step: Step,
    usage: Usage,
    profile_name: String,
    original_video: VideoSettings,
    original_output:skylive_core::OutputVideoSettings,
    preset: VideoPreset,
    recording: RecordingDraft,
    streaming: StreamingDraft,
    default_audio:SetupDefaultAudio,
    desktop_available:bool,
    microphone_available:bool,
    error: Option<String>,
}
#[derive(Default)]
pub struct SetupFlow {
    serial: u64,
    wizard: Option<Wizard>,
}
impl SetupFlow {
    pub fn is_open(&self) -> bool {
        self.wizard.is_some()
    }
    pub fn open(
        &mut self,
        session: &Session,
        origin: SetupOrigin,
        capabilities: SetupCapabilities,
    ) -> Result<(), String> {
        if session.is_closed() {
            return Err("Setup requires an open document.".into());
        }
        if self.is_open() {
            return Err("Finish or cancel the current setup first.".into());
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or("Setup generation is exhausted.")?;
        let snapshot = session.snapshot();
        let profile = snapshot.profiles().selected();
        let desktop_available=cfg!(target_os="windows")&&!snapshot.global_audio().iter().any(|root|root.slot()==AudioSlot::Desktop1);
        let microphone_available=cfg!(target_os="windows")&&!snapshot.global_audio().iter().any(|root|root.slot()==AudioSlot::Mic1);
        let first_run=origin==SetupOrigin::FirstRun;
        let mut streaming = StreamingDraft::from_model(&snapshot);
        streaming.bind(session)?;
        self.wizard = Some(Wizard {
            token: SetupToken {
                session: session.identity(),
                revision: snapshot.revision(),
                profile: profile.id(),
                generation: serial,
            },
            origin,
            capabilities,
            step: Step::Usage,
            usage: if capabilities.recording {
                Usage::Recording
            } else {
                Usage::Streaming
            },
            profile_name: profile.name().into(),
            original_video: profile.video().clone(),
            original_output:profile.output_video_settings(),
            preset: VideoPreset::Current,
            recording: RecordingDraft::from_profile(profile),
            streaming,
            default_audio:SetupDefaultAudio{desktop:first_run&&desktop_available,microphone:first_run&&microphone_available},
            desktop_available,microphone_available,
            error: None,
        });
        self.serial = serial;
        Ok(())
    }
    pub fn token(&self) -> Option<SetupToken> {
        self.wizard.as_ref().map(|wizard| wizard.token)
    }
    pub fn close(&mut self) {
        self.wizard = None;
    }
    pub fn error(&mut self, error: String) {
        if let Some(wizard) = &mut self.wizard {
            wizard.error = Some(error);
        }
    }
    fn checked(&mut self, token: SetupToken, session: &Session) -> Result<&mut Wizard, String> {
        let wizard = self.wizard.as_mut().ok_or("Setup is no longer open.")?;
        if wizard.token != token
            || session.is_closed()
            || session.identity() != token.session
            || session.snapshot().revision() != token.revision
            || session.snapshot().profiles().selected_id() != token.profile
        {
            return Err(
                "The setup target changed. Cancel and reopen setup before applying.".into(),
            );
        }
        Ok(wizard)
    }
    /// Local navigation returns no command. Cancel/Back never publish settings.
    pub fn action(
        &mut self,
        token: SetupToken,
        action: SetupAction,
        session: &Session,
        capabilities: SetupCapabilities,
    ) -> Result<Option<Command>, String> {
        // Cancel is still valid for a stale underlying document, but never for an
        // older/reopened wizard generation.
        if action == SetupAction::Cancel {
            if self.token() != Some(token) {
                return Err("Setup is no longer open.".into());
            }
            self.close();
            return Ok(None);
        }
        let wizard = self.checked(token, session)?;
        wizard.error = None;
        match action {
            SetupAction::Back => {
                wizard.step = match wizard.step {
                    Step::Usage => return Err("Setup is already on its first page.".into()),
                    Step::Video => Step::Usage,
                    Step::Output => Step::Video,
                    Step::Review => Step::Output,
                };
                Ok(None)
            }
            SetupAction::Next => {
                wizard.validate_step(session, capabilities)?;
                wizard.step = match wizard.step {
                    Step::Usage => Step::Video,
                    Step::Video => Step::Output,
                    Step::Output => Step::Review,
                    Step::Review => return Err("Review the settings before applying.".into()),
                };
                Ok(None)
            }
            SetupAction::Apply => {
                if wizard.step != Step::Review {
                    return Err("Review the settings before applying.".into());
                }
                let configuration = wizard.configuration(session, capabilities)?;
                Ok(Some(Command::Setup {
                    command: SetupCommand::Apply { configuration },
                }))
            }
            SetupAction::Skip => {
                if wizard.origin != SetupOrigin::FirstRun {
                    return Err("Skip is only available during first-run setup.".into());
                }
                Ok(Some(Command::Setup {
                    command: SetupCommand::Skip,
                }))
            }
            SetupAction::Cancel => unreachable!(),
        }
    }
    pub fn show(
        &mut self,
        context: &egui::Context,
        session: &Session,
        enabled: bool,
        intents: &mut Vec<Intent>,
    ) {
        let Some(wizard) = &mut self.wizard else {
            return;
        };
        let token = wizard.token;
        let response = egui::Modal::new(egui::Id::new("setup-wizard")).show(context, |ui| {
            ui.set_width(620.0_f32.min((context.screen_rect().width() - 48.0).max(120.0)));
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.heading(crate::i18n::tr(ui, "Setup wizard"));
            let step_name = match wizard.step { Step::Usage => "Usage", Step::Video => "Video", Step::Output => "Output", Step::Review => "Review" };
            ui.strong(crate::i18n::tr(ui, step_name));
            let fresh = !session.is_closed() && session.identity() == token.session
                && session.snapshot().revision() == token.revision && session.snapshot().profiles().selected_id() == token.profile;
            let validation = if fresh { wizard.validate_step(session, wizard.capabilities) }
                else { Err("The setup target changed. Cancel and reopen setup before applying.".into()) };
            let mut footer = vec![
                Action::new("Back", enabled && fresh && wizard.step != Step::Usage, "Return to the previous setup page without applying changes."),
                Action::new(if wizard.step == Step::Review { "Apply" } else { "Next" }, enabled && validation.is_ok(),
                    if wizard.step == Step::Review { "Apply all reviewed profile settings together. Recording and streaming stay stopped." } else { "Validate this page and continue without applying changes." }),
            ];
            if wizard.origin == SetupOrigin::FirstRun {
                footer.push(Action::new("Skip setup", enabled && fresh, "Keep current settings and remember that first-run setup was skipped."));
            }
            footer.push(Action::new("Cancel", true, "Close setup without changing settings or completion state."));
            let body_height = (context.screen_rect().height() - modal_footer::required_height(ui, &footer) - 112.0).max(48.0);
            egui::ScrollArea::vertical().id_salt("setup-body").max_height(body_height).auto_shrink([false, true]).show(ui, |ui| {
                ui.label(crate::i18n::tr(ui, "Choose settings for the selected profile. These are manual presets; no bandwidth or optimization tests have run."));
                ui.label(crate::i18n::tf(ui, "Profile: {name}", &[("{name}", wizard.profile_name.clone())]));
                ui.add_enabled_ui(enabled, |ui| wizard.body(ui));
                if let Some(error) = wizard.error.as_ref().or(validation.as_ref().err()) {
                    ui.colored_label(ui.visuals().error_fg_color, crate::i18n::tr(ui, error));
                }
            });
            ui.separator();
            if let Some(index) = modal_footer::show(ui, &footer) {
                let action = if index == 0 { SetupAction::Back } else if index == 1 {
                    if wizard.step == Step::Review { SetupAction::Apply } else { SetupAction::Next }
                } else if wizard.origin == SetupOrigin::FirstRun && index == 2 { SetupAction::Skip } else { SetupAction::Cancel };
                intents.push(Intent::SetupAction(token, action));
            }
        });
        if response.should_close() {
            intents.push(Intent::SetupAction(token, SetupAction::Cancel));
        }
    }
}
impl Wizard {
    fn output_video(&self)->skylive_core::OutputVideoSettings{if self.preset==VideoPreset::Current{self.original_output}else{Default::default()}}
    fn video(&self) -> Result<VideoSettings, String> {
        match self.preset {
            VideoPreset::Current => Ok(self.original_video.clone()),
            VideoPreset::P720 => VideoSettings::new(1280, 720, 30, 1).map_err(|e| e.to_string()),
            VideoPreset::P1080 => VideoSettings::new(1920, 1080, 30, 1).map_err(|e| e.to_string()),
            VideoPreset::P1080Fast => {
                VideoSettings::new(1920, 1080, 60, 1).map_err(|e| e.to_string())
            }
        }
    }
    fn allowed(&self, capabilities: SetupCapabilities) -> Result<(), String> {
        if match self.usage {
            Usage::Recording => capabilities.recording,
            Usage::Streaming => capabilities.streaming,
        } {
            Ok(())
        } else {
            Err("The selected output is unavailable on this platform.".into())
        }
    }
    fn validate_step(
        &self,
        session: &Session,
        capabilities: SetupCapabilities,
    ) -> Result<(), String> {
        self.allowed(capabilities)?;
        match self.step {
            Step::Usage => Ok(()),
            Step::Video => self.video().map(|_| ()),
            Step::Output | Step::Review => self.configuration(session, capabilities).map(|_| ()),
        }
    }
    fn configuration(
        &self,
        session: &Session,
        capabilities: SetupCapabilities,
    ) -> Result<SetupConfiguration, String> {
        self.allowed(capabilities)?;
        let video = self.video()?;
        let output_video=self.output_video().resolve(&video).map_err(|e|e.to_string())?;
        let model = session.snapshot();
        let profile = model.profiles().selected();
        // Back preserves both drafts, but only the reviewed output draft is
        // applied. Switching usage must not publish hidden edits to the other.
        let recording = if self.usage == Usage::Recording {
            self.recording.settings()?
        } else {
            profile.recording().clone()
        };
        let streaming = if self.usage == Usage::Streaming {
            self.streaming.settings()?
        } else {
            profile.streaming().clone()
        };
        let audio = model.audio();
        match self.usage {
            Usage::Recording => {
                let directory = if recording.directory().is_empty() {
                    skylive_host::recording::default_directory()
                } else {
                    std::path::PathBuf::from(recording.directory())
                };
                skylive_host::recording::RecordingConfig {
                    path: directory.join(format!("SkyLive-setup-validation.{}",recording.container().extension())),
                    encoder:recording.encoder(),
                    keyframe_interval_seconds:recording.keyframe_interval_seconds(),
                    video: output_video.clone(),
                    video_bitrate_kbps: recording.video_bitrate_kbps(),
                    audio: recording.audio_enabled().then_some(
                        skylive_host::recording::RecordingAudio {
                            sample_rate: audio.sample_rate(),
                            channels: if audio.speakers().channels() == 1 {
                                1
                            } else {
                                2
                            },
                            bitrate_kbps: recording.audio_bitrate_kbps(),
                        },
                    ),
                    prefer_hardware: recording.prefer_hardware(),
                }
                .validate()
                .map_err(|error| error.to_string())?;
            }
            Usage::Streaming => {
                skylive_host::streaming::Config::new(
                    session.identity(),
                    self.token.profile,
                    output_video.clone(),
                    audio,
                    streaming.clone(),
                )
                .map_err(|error| error.to_string())?;
            }
        }
        if (self.default_audio.desktop&&!self.desktop_available)||(self.default_audio.microphone&&!self.microphone_available){
            return Err("Setup default audio is unavailable or its slot is already occupied.".into());
        }
        SetupConfiguration::new(self.token.profile, video, recording, streaming)
            .and_then(|configuration|configuration.with_output_video(self.output_video()))
            .map(|configuration|configuration.with_default_audio(self.default_audio))
            .map_err(|e| e.to_string())
    }
    fn body(&mut self, ui: &mut egui::Ui) {
        match self.step {
            Step::Usage => {
                ui.add_enabled_ui(self.capabilities.recording, |ui| {
                    ui.radio_value(
                        &mut self.usage,
                        Usage::Recording,
                        crate::i18n::tr(ui, "Recording · MP4/Matroska · H.264/AAC"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        ui,
                        "Configure the real native recording profile without starting a recording.",
                    ))
                    .on_disabled_hover_text(crate::i18n::tr(
                        ui,
                        "The selected output is unavailable on this platform.",
                    ));
                });
                ui.add_enabled_ui(self.capabilities.streaming, |ui| {
                    ui.radio_value(
                        &mut self.usage,
                        Usage::Streaming,
                        crate::i18n::tr(ui, "Streaming · RTMP/RTMPS"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        ui,
                        "Configure streaming with a service preset or custom RTMP/RTMPS address without making a connection.",
                    ))
                    .on_disabled_hover_text(crate::i18n::tr(
                        ui,
                        "The selected output is unavailable on this platform.",
                    ));
                });
            }
            Step::Video => {
                for (preset, name) in [
                    (VideoPreset::Current, "Use current video settings"),
                    (VideoPreset::P720, "1280 × 720 · 30 FPS"),
                    (VideoPreset::P1080, "1920 × 1080 · 30 FPS"),
                    (VideoPreset::P1080Fast, "1920 × 1080 · 60 FPS"),
                ] {
                    ui.radio_value(&mut self.preset, preset, crate::i18n::tr(ui, name))
                        .on_hover_text(crate::i18n::tr(ui, "Choose a manual canvas and frame rate. This does not measure encoding performance."))
                        .on_disabled_hover_text(crate::i18n::tr(ui, "Wait for the active document operation before editing"));
                }
                ui.label(crate::i18n::tr(ui,"Current video settings also retain the existing output size and scaling filter. Other presets use their canvas size for output."));
                ui.label(crate::i18n::tr(ui,"Existing audio format, device bindings, scenes and filters are preserved."));
                if cfg!(target_os="windows"){
                    ui.separator();ui.strong(crate::i18n::tr(ui,"Default audio devices"));
                    ui.label(crate::i18n::tr(ui,"Choose the default Windows devices for empty global audio slots. Capture starts only after Apply."));
                    ui.add_enabled(self.desktop_available,egui::Checkbox::new(&mut self.default_audio.desktop,crate::i18n::tr(ui,"Add default Desktop Audio")))
                        .on_hover_text(crate::i18n::tr(ui,"Add real WASAPI loopback from the default Windows playback device to Desktop Audio 1."))
                        .on_disabled_hover_text(crate::i18n::tr(ui,"This global audio slot already has a source. Its binding is preserved."));
                    ui.add_enabled(self.microphone_available,egui::Checkbox::new(&mut self.default_audio.microphone,crate::i18n::tr(ui,"Add default Mic/Aux")))
                        .on_hover_text(crate::i18n::tr(ui,"Add the default Windows recording device to Mic/Aux 1."))
                        .on_disabled_hover_text(crate::i18n::tr(ui,"This global audio slot already has a source. Its binding is preserved."));
                }
            }
            Step::Output => match self.usage {
                Usage::Recording => super::recording_settings::editor_recording_only(ui, &mut self.recording),
                Usage::Streaming => super::streaming_settings::editor_setup(ui, &mut self.streaming),
            },
            Step::Review => {
                if let Ok(video) = self.video() {
                    ui.label(crate::i18n::tf(
                        ui,
                        "Video: {width} × {height}, {num}/{den} FPS",
                        &[
                            ("{width}", video.width().to_string()),
                            ("{height}", video.height().to_string()),
                            ("{num}", video.fps_num().to_string()),
                            ("{den}", video.fps_den().to_string()),
                        ],
                    ));
                }
                ui.label(crate::i18n::tr(
                    ui,
                    match self.usage {
                        Usage::Recording => "Recording · MP4/Matroska · H.264/AAC",
                        Usage::Streaming => "Streaming · RTMP/RTMPS",
                    },
                ));
                if let Ok(output)=self.video().and_then(|canvas|self.output_video().resolve(&canvas).map_err(|e|e.to_string())){
                    ui.label(crate::i18n::tf(ui,"Output video: {width} × {height}",&[("{width}",output.width().to_string()),("{height}",output.height().to_string())]));
                }
                if self.default_audio.desktop{ui.label(crate::i18n::tr(ui,"Desktop Audio 1: default Windows playback device (loopback)"));}
                if self.default_audio.microphone{ui.label(crate::i18n::tr(ui,"Mic/Aux 1: default Windows recording device"));}
                if self.usage==Usage::Recording{
                    if let Ok(recording)=self.recording.settings(){ui.label(crate::i18n::tf(ui,"Recording Format: {format}",&[("{format}",match recording.container(){skylive_core::RecordingContainer::Mp4=>"MP4",skylive_core::RecordingContainer::Matroska=>"Matroska"}.into())]));}
                }
                let settings = match self.usage {
                    Usage::Recording => self.recording.settings().map(|settings| {
                        (
                            settings.video_bitrate_kbps(),
                            settings.audio_enabled(),
                            settings.audio_bitrate_kbps(),
                        )
                    }),
                    Usage::Streaming => self.streaming.settings().map(|settings| {
                        (
                            settings.video_bitrate_kbps(),
                            settings.audio_enabled(),
                            settings.audio_bitrate_kbps(),
                        )
                    }),
                };
                if let Ok((video, audio, bitrate)) = settings {
                    ui.label(crate::i18n::tf(
                        ui,
                        "Video bitrate: {rate} kbit/s",
                        &[("{rate}", video.to_string())],
                    ));
                    ui.label(crate::i18n::tf(
                        ui,
                        "Program audio: {state}",
                        &[(
                            "{state}",
                            crate::i18n::tr(ui, if audio { "Enabled" } else { "Disabled" }),
                        )],
                    ));
                    if audio {
                        ui.label(crate::i18n::tf(
                            ui,
                            "Audio bitrate: {rate} kbit/s",
                            &[("{rate}", bitrate.to_string())],
                        ));
                    }
                }
                match self.usage {
                    Usage::Recording => {
                        let path = if self.recording.directory.is_empty() {
                            skylive_host::recording::default_directory()
                                .display()
                                .to_string()
                        } else {
                            self.recording.directory.clone()
                        };
                        ui.label(crate::i18n::tf(
                            ui,
                            "Output directory: {path}",
                            &[("{path}", path)],
                        ));
                        ui.label(crate::i18n::tf(
                            ui,
                            "Video Encoder: {state}",
                            &[(
                                "{state}",
                                crate::i18n::tr(
                                    ui,
                                    if self.recording.encoder.selection.is_default() {
                                        "Software H.264"
                                    } else {
                                        "NVIDIA NVENC H.264"
                                    },
                                ),
                            )],
                        ));
                    }
                    Usage::Streaming => {
                        if let Ok(settings) = self.streaming.settings() {
                            ui.label(crate::i18n::tf(
                                ui,
                                "Server: {url}",
                                &[("{url}", settings.server_url().to_owned())],
                            ));
                        }
                    }
                }
                ui.label(crate::i18n::tr(ui, "Apply updates this profile and setup completion together. Installed preferences save separately; save the collection to keep its profile changes."));
                ui.label(crate::i18n::tr(
                    ui,
                    "No recording, stream, hardware test or network test starts when you apply.",
                ));
            }
        }
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
