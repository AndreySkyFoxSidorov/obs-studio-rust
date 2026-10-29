pub mod missing_files;
pub mod collection_library;
pub mod source_output_resize;
pub mod output_audio_tracks;
pub mod stream_service_presets;
pub mod advanced_audio;
pub mod screenshot;
pub mod remux;
pub mod accessibility;
pub mod advanced_process;
pub mod stream_reconnect;
pub mod audio_monitoring;
pub mod appearance;
pub mod audio_device;
pub mod camera_capture;
pub mod camera_audio;
pub mod camera_av_status;
pub mod camera_controls;
pub mod color_source;
pub mod craft_audio_controls;
pub mod dialogs;
pub mod dock_columns;
pub mod dock_layout;
pub mod dock_toolbar;
pub mod docks;
pub mod document;
pub mod document_picker;
pub mod global_audio_devices;
pub mod group_source;
pub mod hotkeys;
pub mod list_reorder;
pub mod item_compositing;
pub mod source_color_tags;
pub mod media_controls;
pub mod media_source;
pub mod game_capture;
pub mod ndi_source;
pub mod ndi_output;
pub mod browser_source;
pub mod image_slideshow;
pub mod slideshow_options;
pub mod slideshow_controls;
pub mod source_icons;
pub mod source_toolbar;
pub mod virtual_camera;
pub mod menus;
pub mod mixer;
pub mod modal_footer;
pub mod plugin_manager;
pub mod plugin_routes;
pub mod plugin_sources;
pub mod recording_settings;
pub mod video_encoder;
pub mod replay_buffer;
pub mod scene_notes_dock;
pub mod scene_source;
pub mod settings;
pub mod projector_settings;
pub mod preview_snapping;
pub mod video_settings;
pub mod setup;
pub mod source_audio_filters;
pub mod image_source;
mod filter_add_menu;
mod filter_chain_reorder;
pub mod streaming_settings;
pub mod text_source;
pub mod video_filters;
mod scale_filter;
pub mod window_capture;

use skylive_core::{
    Command, DisplayCaptureBinding, Id, QuickTransitionId, StudioSnapshot, TransitionPresetId,
    TransitionRef,
};

pub fn transition_kind_label(ui: &eframe::egui::Ui, kind: skylive_core::TransitionKind) -> String {
    use skylive_core::TransitionKind;
    let key = match kind {
        TransitionKind::Cut => "Module.obs-transitions.CutTransition",
        TransitionKind::Fade => "Module.obs-transitions.FadeTransition",
        TransitionKind::Swipe { .. } => "Module.obs-transitions.SwipeTransition",
        TransitionKind::Slide { .. } => "Module.obs-transitions.SlideTransition",
        TransitionKind::FadeToColor { .. } => "Module.obs-transitions.FadeToColorTransition",
        TransitionKind::LumaWipe{..}=>"Module.obs-transitions.LumaWipeTransition",
    };
    let value = crate::i18n::tr_key(ui, key);
    if value == key {
        kind.label().into()
    } else {
        value
    }
}
fn direction_label(ui: &eframe::egui::Ui, value: skylive_core::TransitionDirection) -> String {
    use skylive_core::TransitionDirection;
    let (key, english) = match value {
        TransitionDirection::Left => ("Module.obs-transitions.Direction.Left", "Left"),
        TransitionDirection::Right => ("Module.obs-transitions.Direction.Right", "Right"),
        TransitionDirection::Up => ("Module.obs-transitions.Direction.Up", "Up"),
        TransitionDirection::Down => ("Module.obs-transitions.Direction.Down", "Down"),
    };
    let value = crate::i18n::tr_key(ui, key);
    if value == key { english.into() } else { value }
}
pub fn transition_reference_label(
    ui: &eframe::egui::Ui,
    model: &StudioSnapshot,
    reference: TransitionRef,
) -> String {
    use skylive_core::TransitionKind;
    match reference {
        TransitionRef::Cut => transition_kind_label(ui, TransitionKind::Cut),
        TransitionRef::Fade => transition_kind_label(ui, TransitionKind::Fade),
        TransitionRef::Preset(id) => model.transition_preset(id).map_or_else(
            || crate::i18n::tf(ui, "Unavailable preset {}", &[("{}", id.get().to_string())]),
            |preset| {
                let kind = preset.kind();
                let label = transition_kind_label(ui, kind);
                let detail = match kind {
                    TransitionKind::Swipe {
                        direction,
                        swipe_in,
                    } => format!(
                        "{label} · {} · {}: {}",
                        direction_label(ui, direction),
                        crate::i18n::tr(ui, "Swipe In"),
                        crate::i18n::tr(ui, if swipe_in { "Enabled" } else { "Disabled" })
                    ),
                    TransitionKind::Slide { direction } => {
                        format!("{label} · {}", direction_label(ui, direction))
                    }
                    TransitionKind::LumaWipe {mask,softness,invert}=>format!(
                        "{label} · {} · {:.2} · {}: {}",
                        crate::i18n::tr_key_or(ui,mask.locale_key(),mask.label()),softness.get(),
                        crate::i18n::tr_key_or(ui,"Module.obs-transitions.LumaWipe.Invert","Invert"),
                        crate::i18n::tr(ui,if invert{"Enabled"}else{"Disabled"})
                    ),
                    TransitionKind::FadeToColor { rgb, switch_point } => format!(
                        "{label} · #{:02X}{:02X}{:02X} · {switch_point}%",
                        rgb[0], rgb[1], rgb[2]
                    ),
                    _ => label,
                };
                format!("{} [{detail}]", preset.name())
            },
        ),
    }
}

/// Presentation adapter for a staged transition value. Its caller decides
/// whether Apply edits a shared preset or another validated command boundary.
/// Direction and reveal flags are metadata; views never evaluate transition math.
pub fn transition_kind_editor(
    ui: &mut eframe::egui::Ui,
    scope: impl std::hash::Hash,
    kind: &mut skylive_core::TransitionKind,
    spatial_only: bool,
) {
    use skylive_core::{TransitionDirection, TransitionKind};
    ui.push_id(scope, |ui| {
        let direction = match *kind {
            TransitionKind::Swipe { direction, .. } | TransitionKind::Slide { direction } => {
                direction
            }
            _ => TransitionDirection::Left,
        };
        let swipe_in = match *kind {
            TransitionKind::Swipe { swipe_in, .. } => swipe_in,
            _ => false,
        };
        let fade_color=if matches!(*kind,TransitionKind::FadeToColor{..}){*kind}else{TransitionKind::fade_to_color()};
        let luma=if matches!(*kind,TransitionKind::LumaWipe{..}){*kind}else{TransitionKind::luma_wipe()};
        ui.horizontal_wrapped(|ui| {
            for (index, provider) in [
                TransitionKind::Cut,
                TransitionKind::Fade,
                TransitionKind::Swipe {
                    direction,
                    swipe_in,
                },
                TransitionKind::Slide { direction },
                fade_color,
                luma,
            ]
            .into_iter()
            .enumerate()
            {
                ui.push_id(("provider", index), |ui| {
                    ui.add_enabled_ui(
                        !spatial_only
                            || !matches!(provider, TransitionKind::Cut | TransitionKind::Fade),
                        |ui| {
                            ui.selectable_value(kind, provider, transition_kind_label(ui, provider))
                                .on_hover_text(crate::hints::text(ui, provider.label()))
                                .on_disabled_hover_text(crate::hints::text(ui, provider.label()))
                        },
                    )
                });
            }
        });
        let directional = match *kind {
            TransitionKind::Swipe { direction, .. } | TransitionKind::Slide { direction } => {
                Some(direction)
            }
            _ => None,
        };
        if let Some(mut direction) = directional {
            ui.label(crate::i18n::tr(ui, "Direction"));
            ui.horizontal_wrapped(|ui| {
                for (value, label) in [
                    (TransitionDirection::Left, "Left"),
                    (TransitionDirection::Right, "Right"),
                    (TransitionDirection::Up, "Up"),
                    (TransitionDirection::Down, "Down"),
                ] {
                    ui.push_id(("direction", value), |ui| {
                        ui.selectable_value(&mut direction, value, direction_label(ui, value))
                            .on_hover_text(crate::hints::text(ui, label))
                            .on_disabled_hover_text(crate::hints::text(ui, label))
                    });
                }
            });
            match kind {
                TransitionKind::Swipe {
                    direction: current,
                    swipe_in,
                } => {
                    *current = direction;
                    ui.push_id("swipe-in", |ui| {
                        ui.checkbox(swipe_in, crate::i18n::tr(ui, "Swipe In"))
                            .on_hover_text(crate::hints::text(ui, "Swipe In"))
                            .on_disabled_hover_text(crate::hints::text(ui, "Swipe In"))
                    });
                }
                TransitionKind::Slide { direction: current } => *current = direction,
                _ => {}
            }
        }
        if let TransitionKind::LumaWipe{mask,softness,invert}=kind{
            use skylive_core::LumaWipePattern;
            let pattern_text=|ui:&eframe::egui::Ui,value:LumaWipePattern|crate::i18n::tr_key_or(ui,value.locale_key(),value.label());
            let label=ui.label(crate::i18n::tr_key_or(ui,"Module.obs-transitions.LumaWipe.Image","Image"));
            eframe::egui::ComboBox::from_id_salt("luma-wipe-pattern").width(ui.available_width()).selected_text(pattern_text(ui,*mask)).show_ui(ui,|ui|{
                let query_id=ui.id().with("luma-pattern-search");let mut query=ui.ctx().data_mut(|d|d.get_temp::<String>(query_id)).unwrap_or_default();
                ui.add(eframe::egui::TextEdit::singleline(&mut query).char_limit(64).hint_text(crate::i18n::tr(ui,"Search patterns…")))
                    .on_hover_text(crate::i18n::tr(ui,"Search standard OBS wipe patterns by translated name or filename."));
                ui.ctx().data_mut(|d|d.insert_temp(query_id,query.clone()));let query=query.to_lowercase();
                eframe::egui::ScrollArea::vertical().max_height(240.0).show(ui,|ui|{
                    for value in LumaWipePattern::ALL{
                        let label=pattern_text(ui,value);
                        if query.is_empty()||label.to_lowercase().contains(&query)||value.label().to_lowercase().contains(&query)||value.filename().contains(&query){
                            ui.selectable_value(mask,value,label).on_hover_text(crate::i18n::tr(ui,"Reveal the incoming scene with this standard OBS grayscale mask."));
                        }
                    }
                });
            }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Choose the grayscale pattern used to reveal the incoming scene."));
            let mut amount=softness.get();
            ui.horizontal(|ui|{
                let label=ui.label(crate::i18n::tr_key_or(ui,"Module.obs-transitions.LumaWipe.Softness","Softness"));
                if ui.add(eframe::egui::DragValue::new(&mut amount).speed(0.05).range(0.0..=1.0)).labelled_by(label.id)
                    .on_hover_text(crate::i18n::tr(ui,"Widen the transition edge. Zero follows a hard boundary along the mask.")).changed(){
                    if let Ok(value)=skylive_core::LumaWipeSoftness::new(amount){*softness=value;}
                }
            });
            ui.checkbox(invert,crate::i18n::tr_key_or(ui,"Module.obs-transitions.LumaWipe.Invert","Invert"))
                .on_hover_text(crate::i18n::tr(ui,"Reverse the dark and light portions of the wipe mask."));
        }
        let mut reset_fade_color=false;
        if let TransitionKind::FadeToColor { rgb, switch_point } = kind {
            ui.horizontal(|ui| {
                let label=ui.label(crate::i18n::tr_key_or(ui,"Module.obs-transitions.Color","Color"));
                ui.color_edit_button_srgb(rgb).labelled_by(label.id)
                    .on_hover_text(crate::i18n::tr(ui,"Fade through this opaque color. Alpha is always 255, matching OBS."));
            });
            ui.horizontal_wrapped(|ui| {
                for (index,value) in rgb.iter_mut().enumerate() {
                    let channel=["Red","Green","Blue"][index];
                    let label=ui.label(crate::i18n::tr(ui,channel));
                    ui.push_id(("fade-color-byte",index),|ui| {
                        ui.add(eframe::egui::DragValue::new(value).range(0..=255)).labelled_by(label.id)
                            .on_hover_text(crate::i18n::tr(ui,"Encoded RGB channel from 0 to 255; the fade color remains opaque."));
                    });
                }
            });
            ui.horizontal(|ui| {
                let label=ui.label(crate::i18n::tr_key_or(ui,"Module.obs-transitions.SwitchPoint","Peak Color Point"));
                ui.push_id("fade-color-switch-point",|ui| {
                    ui.add(eframe::egui::Slider::new(switch_point,0..=100).suffix("%")).labelled_by(label.id)
                        .on_hover_text(crate::i18n::tr(ui,"Position of the color peak between outgoing and incoming scenes. At 0% or 100%, one fade half has zero duration."));
                });
            });
            ui.label(crate::i18n::tr(ui,"Outgoing and incoming scene audio use separate smoothstep fades around the color peak. Shared sources and global audio keep their existing routing."));
            reset_fade_color=ui.add_enabled(*rgb!=[0;3]||*switch_point!=50,
                eframe::egui::Button::new(crate::i18n::tr(ui,"Restore Fade to Color defaults")))
                .on_hover_text(crate::i18n::tr(ui,"Reset this draft to opaque black and a 50% peak. Preset name, duration and references stay unchanged until Apply."))
                .on_disabled_hover_text(crate::i18n::tr(ui,"Fade to Color is already at its default settings.")).clicked();
        }
        if reset_fade_color{*kind=TransitionKind::fade_to_color();}
    });
}

/// Full descriptive label, never an identity or serialized key.
pub fn transition_kind_description(kind: skylive_core::TransitionKind) -> String {
    use skylive_core::{TransitionDirection, TransitionKind};
    let direction = |direction| match direction {
        TransitionDirection::Left => "Left",
        TransitionDirection::Right => "Right",
        TransitionDirection::Up => "Up",
        TransitionDirection::Down => "Down",
    };
    match kind {
        TransitionKind::Cut | TransitionKind::Fade => kind.label().to_owned(),
        TransitionKind::Swipe {
            direction: value,
            swipe_in,
        } => format!(
            "{} · {} · {}",
            kind.label(),
            direction(value),
            if swipe_in { "In" } else { "Out" }
        ),
        TransitionKind::Slide { direction: value } => {
            format!("{} · {}", kind.label(), direction(value))
        }
        TransitionKind::LumaWipe {mask,softness,invert}=>format!("{} · {} · {:.2}{}",kind.label(),mask.label(),softness.get(),if invert{" · Invert"}else{""}),
        TransitionKind::FadeToColor { rgb, switch_point } => format!(
            "{} · #{:02X}{:02X}{:02X} · {}%",
            kind.label(),
            rgb[0],
            rgb[1],
            rgb[2],
            switch_point
        ),
    }
}

pub fn transition_reference_editor(
    ui: &mut eframe::egui::Ui,
    scope: impl std::hash::Hash,
    model: &StudioSnapshot,
    reference: &mut TransitionRef,
) {
    use eframe::egui;
    ui.push_id(scope, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.push_id("builtin-cut", |ui| {
                ui.selectable_value(
                    reference,
                    TransitionRef::Cut,
                    transition_kind_label(ui, skylive_core::TransitionKind::Cut),
                )
                .on_hover_text(crate::hints::text(ui, "Cut"))
                .on_disabled_hover_text(crate::hints::text(ui, "Cut"))
            });
            ui.push_id("builtin-fade", |ui| {
                ui.selectable_value(
                    reference,
                    TransitionRef::Fade,
                    transition_kind_label(ui, skylive_core::TransitionKind::Fade),
                )
                .on_hover_text(crate::hints::text(ui, "Fade"))
                .on_disabled_hover_text(crate::hints::text(ui, "Fade"))
            });
        });
        let selected = match *reference {
            TransitionRef::Preset(id) => model.transition_preset(id).map_or_else(
                || crate::i18n::tr(ui, "Unavailable preset"),
                |preset| preset.name().to_owned(),
            ),
            _ => crate::i18n::tr(ui, "Presets"),
        };
        ui.add_enabled_ui(!model.transition_presets().is_empty(), |ui| {
            egui::ComboBox::from_id_salt("preset-reference")
                .width(ui.available_width())
                .selected_text(&selected)
                .truncate()
                .show_ui(ui, |ui| {
                    ui.set_width(ui.available_width().min(450.0));
                    ui.style_mut().wrap_mode = Some(eframe::egui::TextWrapMode::Wrap);
                    for preset in model.transition_presets() {
                        ui.push_id(preset.id().get(), |ui| {
                            ui.selectable_value(
                                reference,
                                TransitionRef::Preset(preset.id()),
                                preset.name(),
                            )
                            .on_hover_text(crate::hints::text(ui, "Transition preset"))
                            .on_disabled_hover_text(crate::hints::text(ui, "Transition preset"))
                        });
                    }
                })
                .response
                .on_hover_text(selected)
                .on_hover_text(crate::hints::text(ui, "Menu"))
                .on_disabled_hover_text(crate::hints::text(ui, "Add Transition Preset"));
        });
        if matches!(reference, TransitionRef::Preset(_)) {
            ui.label(transition_reference_label(ui, model, *reference));
        }
    });
}

pub fn transition_reference_description(
    model: &StudioSnapshot,
    reference: TransitionRef,
) -> String {
    match reference {
        TransitionRef::Cut => "Cut".into(),
        TransitionRef::Fade => "Fade".into(),
        TransitionRef::Preset(id) => model.transition_preset(id).map_or_else(
            || format!("Unavailable preset {}", id.get()),
            |preset| {
                format!(
                    "{} [{}]",
                    preset.name(),
                    transition_kind_description(preset.kind())
                )
            },
        ),
    }
}

pub enum Intent {
    ContextMixerHidden { session: u64, revision: u64, scene: Id, item: Id, source: Id, hidden: bool },
    ContextOrder { session: u64, revision: u64, scene: Id, item: Id, index: usize },
    ContextTransform { session: u64, revision: u64, scene: Id, item: Id, action: skylive_core::TransformAction },
    OpenHelpLink(skylive_host::help_links::Target),
    OpenSourceOutputResize {session:u64,revision:u64,scene:Id,item:Id},
    ApplySourceOutputResize {token:u64},
    Projector(crate::projectors::Action),
    RepairFiles(u64,missing_files::Action),
    CancelPendingOpen,
    RetryPendingOpen,
    OpenMissingFiles,
    MissingFiles(u64,missing_files::Action),
    OpenRemux,
    Remux(remux::Action),
    ScreenshotHotkey {selected:bool},
    Screenshot {session:u64,revision:u64,target:skylive_host::screenshot::Target},
    GlobalHotkey(skylive_host::global_hotkeys::Fired),
    OpenNdiOutputs,
    NdiOutput(ndi_output::Action),
    SourceColorTag(source_color_tags::Action),
    ItemCompositing(item_compositing::Submission),
    OpenStatistics,
    StartVirtualCamera,
    StopVirtualCamera,
    OpenVirtualCameraSettings,
    ApplyVirtualCameraSettings(virtual_camera::Submission),
    CloseVirtualCameraSettings(virtual_camera::Token),
    NativeBrowse(crate::native_browse::Request),
    Clipboard(crate::edit_clipboard::Request),
    ContextTransformClipboard(crate::edit_clipboard::Request),
    MixerFilterClipboard(crate::edit_clipboard::FilterRequest),
    SourceClipboard(crate::source_clipboard::Request),
    ContextSourceClipboard { action: crate::source_clipboard::Action, session: u64, revision: u64, item: Id },
    ContextSourcePaste { action: crate::source_clipboard::Action, session: u64, revision: u64, scene: Id },
    ToggleFullscreen,
    ToggleAlwaysOnTop,
    ToggleStatusBar,
    TogglePreviewLock,
    PreviewDisplay(crate::preview_display::Action),
    SceneGrid(bool),
    ToggleSourceIcons,
    ToggleSourceToolbar,
    ToggleListboxToolbars,
    ShowRecordingsFolder,
    ShowSettingsFolder,
    InstallVirtualCamera,
    ImportProfile(String),
    ExportProfile(String),
    OpenAdvancedAudio,
    MixerEdit(mixer::MixerSubmission),
    OpenGlobalAudioDevices,
    LiveGlobalAudio(global_audio_devices::LiveGlobalAudioEdit),
    LiveAudio(advanced_audio::LiveAudioEdit),
    LiveAudioMonitoring {session:u64,revision:u64,source:Id,mode:skylive_core::AudioMonitoringMode},
    EnumerateMonitorDevices {session:u64,revision:u64},
    SetMonitoringDevice {session:u64,revision:u64,device:skylive_core::AudioMonitoringDevice},
    LiveAudioProperties(advanced_audio::LiveAudioPropertyEdit),
    PrepareMedia(media_source::MediaPrepare),
    DiscoverTextSystemFonts(text_source::FontKey),
    PrepareTextFont(text_source::FontKey),
    ApplyTextSource(text_source::FontKey),
    PrepareLut(video_filters::LutKey),
    PrepareMask(video_filters::MaskKey),
    DiscoverGames {token:u64,session:u64,revision:u64},
    ApplyGameCapture(game_capture::Apply),
    StartGameCapture {session:u64,revision:u64,source:Id},
    StopGameCapture {session:u64,revision:u64,source:Id},
    PrepareNdi(ndi_source::NdiPrepare),
    InteractBrowser {session:u64,revision:u64,source:Id},
    CommitBrowser(browser_source::Submission),
    RefreshBrowser {token:u64,session:u64,revision:u64,source:Id},
    DiscoverNdi {token:u64,session:u64,revision:u64,local:bool},
    ApplySourceAudioHotkeys {session:u64,revision:u64,source:Id,token:u64,bindings:skylive_core::SourceAudioHotkeys,push:skylive_core::PushAudioSettings},
    ApplySourcePlaybackHotkeys {session:u64,revision:u64,source:Id,token:u64,bindings:skylive_core::SourcePlaybackHotkeys},
    ControlSlideshow {target:skylive_host::image_slideshow::transport::Target,operation:Option<skylive_host::image_slideshow::transport::Control>},
    ControlMedia {
        target: skylive_host::media_source::MediaControlTarget,
        operation: skylive_host::media_source::MediaControl,
    },
    StartStreaming,
    StopStreaming,
    StartReplayBuffer,StopReplayBuffer,SaveReplayBuffer,CancelReplaySave,
    StartRecording,
    StopRecording,
    SetRecordingPaused {recording:u64,session:u64,paused:bool},
    ApplySceneVideoFilters {
        token: u64,
        session: u64,
        revision: u64,
        scene: Id,
        filters: Vec<skylive_core::VideoFilter>,
    },
    ApplyVideoFilters {
        token: u64,
        session: u64,
        revision: u64,
        source: Id,
        filters: Vec<skylive_core::VideoFilter>,
    },
    Reorder {
        session: u64,
        revision: u64,
        command: Command,
    },
    SubmitSettings {
        page: settings::SettingsPage,
        action: settings::SettingsAction,
        token: Option<settings::SettingsToken>,
    },
    ApplyLocalization {
        language: String,
    },
    Command(Command),
    SelectScene(Id),
    SelectItem(Option<Id>),
    Cut,
    Transition,
    Quick(QuickTransitionId),
    ManualPosition(u16),
    ManualRelease,
    DialogError(String),
    OpenDialog(dialogs::Dialog),
    OpenDisplayCapture {
        edit: DisplayEdit,
        name: String,
        binding: Option<DisplayCaptureBinding>,
    },
    RefreshDisplays,
    OpenWindowCapture {
        edit: WindowEdit,
        name: String,
        binding: Option<skylive_core::WindowCaptureBinding>,
    },
    RefreshWindows,
    OpenCameraCapture {
        edit: CameraEdit,
        name: String,
        binding: Option<skylive_core::VideoCaptureBinding>,
    },
    RefreshCameras,
    RefreshCameraAudioDevices {token:u64},
    PrepareSlideShow(image_slideshow::Submission),
    QueryCameraModes,
    QueryCameraControls { token:u64, source:Id },
    ApplyCameraControl {token:u64, lease:skylive_host::camera_capture::ControlLease, change:skylive_host::camera_capture::ControlChange},
    SetCameraSourceEnabled {token:u64,session:u64,revision:u64,source:Id,binding:skylive_core::VideoCaptureBinding,enabled:bool},
    StopCameras,
    CommitCamera {
        token:u64,
        session: u64,
        revision: u64,
        edit: CameraEdit,
        name: String,
        binding: skylive_core::VideoCaptureBinding,
    },
    CommitWindow {
        session: u64,
        revision: u64,
        edit: WindowEdit,
        name: String,
        binding: skylive_core::WindowCaptureBinding,
    },
    EnumerateAudioDevices,
    ApplyColorSource(color_source::ColorSubmission),
    ApplyApplicationAudioBinding { token:u64, session:u64, revision:u64, command:Command },
    ApplyAudioBinding {
        session: u64,
        revision: u64,
        command: Command,
    },
    CommitDisplay {
        session: u64,
        revision: u64,
        edit: DisplayEdit,
        name: String,
        binding: DisplayCaptureBinding,
    },
    ApplyCaptureTransform {
        scene: Id,
        item: Id,
        transform: skylive_core::Transform,
        context: skylive_host::capture::CaptureTransformContext,
    },
    ProbeNvenc {token:Option<settings::SettingsToken>},
    ApplyStreamingSettings(streaming_settings::StreamingApply),
    Save,
    SaveAs,
    SaveAsTo {
        path: String,
    },
    ResolveDocument(document::Choice),
    CancelDocumentSave,
    OpenDocumentPicker(document_picker::DocumentAction),
    ConfirmDocumentPath(document_picker::PathDraft),
    CancelDocumentPath,
    Load,
    Recover,
    Exit,
    NewCollection,
    CollectionLibrary(collection_library::Action),
    OpenObsImport,
    OpenObsExport,
    ImportObs {
        path: String,
    },
    ExportObs {
        path: String,
    },
    PrepareImage {
        path: String,
        edit: ImageEdit,
    },
    StudioMode(bool),
    OpenPluginEditor(plugin_routes::PluginEditorRequest),
    OpenPluginSource(plugin_sources::PluginSourceRequest),
    ApplyPluginEditor {
        token: Option<plugin_routes::PluginEditorToken>,
        command: Command,
    },
    SetPluginEnabled {
        plugin: String,
        enabled: bool,
    },
    ApplyNotesDock(scene_notes_dock::NotesSubmission),
    CancelNotesDock(plugin_routes::PluginEditorToken),
    ToggleDock(Dock),
    ResetLayout,
    OpenSetup,
    SetupAction(setup::SetupToken, setup::SetupAction),
    SetDockPreset(dock_layout::DockPreset),
    SetDocksLocked(bool),
    SetFullHeightDocks(bool),
    SetDockSize(dock_layout::DockSize, u16),
    About,
}
#[derive(Clone)]
pub enum DisplayEdit {
    Add { scene: Id },
    Rebind { source: Id },
}
#[derive(Clone, Default)]
pub struct RecordingViewState {
    pub recording:u64,
    pub session:u64,
    pub starting:bool,
    pub paused:bool,
    pub pause_pending:bool,
    pub degraded_video_frames: u64,
    pub warnings: Option<std::sync::Arc<skylive_host::recording::RecordingWarnings>>,
    pub available: bool,
    pub active: bool,
    pub finishing: bool,
}
#[derive(Clone, Default)]
pub struct StreamingViewState {
    pub connection_epoch:u64,
    pub reconnect_attempt:u32,
    pub reconnect_limit:u32,
    pub reconnect_wait_ms:u32,
    pub reconnect_error:Option<std::sync::Arc<skylive_host::streaming::Error>>,
    pub available: bool,
    pub active: bool,
    pub finishing: bool,
    pub phase: Option<skylive_host::streaming::Phase>,
    pub degraded_video_frames: u64,
    pub warnings: Option<std::sync::Arc<skylive_host::recording::RecordingWarnings>>,
}
#[derive(Clone)]
pub enum WindowEdit {
    Add { scene: Id },
    Rebind { source: Id },
}
#[derive(Clone, PartialEq, Eq)]
pub enum CameraEdit {
    Add { scene: Id },
    Rebind { source: Id },
}
#[derive(Clone, Copy)]
pub enum AudioEdit {
    Add { scene: Id },
    Rebind { source: Id },
    AddGlobal { slot: skylive_core::AudioSlot },
}
#[derive(Clone, Copy)]
pub struct TransitionViewState {
    pub session: u64,
    pub studio: bool,
    pub active: bool,
    pub manual: Option<ManualView>,
    pub preset_focus: Option<TransitionPresetId>,
}
#[derive(Clone, Copy)]
pub struct ManualView {
    pub value: u16,
    pub current: f32,
    pub target: f32,
    pub finishing: bool,
}
/// Small presentation projection. Never retain Program PCM in a view.
pub struct ProgramAudioView {
    pub start_frame: u64,
    pub source_meters: Vec<skylive_host::audio::SourceMeter>,
    pub source_meter_revision: Option<u64>,
    pub key: skylive_host::audio_capture::ProgramCaptureKey,
    pub meters: Vec<skylive_host::audio::ChannelMeter>,
    pub missing_source_frames: usize,
    pub clipped_samples: usize,
}
#[derive(Clone)]
pub enum ImageEdit {
    Add { scene: Id, name: String },
    Replace { source: Id },
}
#[derive(Clone, Copy)]
pub enum Dock {
    Scenes,
    Sources,
    Mixer,
    Transitions,
    Controls,
    SceneNotes,
}
#[derive(Clone)]
pub struct Layout {
    pub arrangement: dock_layout::DockLayout,
    pub columns: dock_columns::DockColumns,
    pub scenes: bool,
    pub sources: bool,
    pub mixer: bool,
    pub transitions: bool,
    pub controls: bool,
}
impl Default for Layout {
    fn default() -> Self {
        Self {
            arrangement: Default::default(),
            columns: Default::default(),
            scenes: true,
            sources: true,
            mixer: true,
            transitions: true,
            controls: true,
        }
    }
}
impl Layout {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn toggle(&mut self, dock: Dock) {
        self.columns.cancel();
        let value = match dock {
            Dock::Scenes => &mut self.scenes,
            Dock::Sources => &mut self.sources,
            Dock::Mixer => &mut self.mixer,
            Dock::Transitions => &mut self.transitions,
            Dock::Controls => &mut self.controls,
            Dock::SceneNotes => &mut self.arrangement.scene_notes_visible,
        };
        *value = !*value;
    }
}

#[cfg(test)]
mod transition_editor_tests {
    use super::*;
    use eframe::egui;
    use skylive_core::{TransitionDirection, TransitionKind};
    fn render(
        context: &egui::Context,
        kind: &mut TransitionKind,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 360.0),
                )),
                events,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    transition_kind_editor(ui, "staged-provider", kind, false)
                });
            },
        )
    }
    fn position(output: &egui::FullOutput, label: &str) -> Option<egui::Pos2> {
        fn find(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().find_map(|shape| find(shape, label))
                }
                _ => None,
            }
        }
        output
            .shapes
            .iter()
            .find_map(|shape| find(&shape.shape, label))
    }
    fn click(context: &egui::Context, kind: &mut TransitionKind, label: &str) {
        let output = render(context, kind, vec![]);
        let point =
            position(&output, label).expect("semantic transition property control is painted");
        let _ = render(
            context,
            kind,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let _ = render(
            context,
            kind,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
    }
    #[test]
    fn shared_staged_provider_controls_keep_direction_and_expose_swipe_only_in_flag() {
        let context = egui::Context::default();
        let mut kind = TransitionKind::Fade;
        let _ = render(&context, &mut kind, vec![]);
        click(&context, &mut kind, "Swipe");
        assert_eq!(
            kind,
            TransitionKind::Swipe {
                direction: TransitionDirection::Left,
                swipe_in: false
            }
        );
        click(&context, &mut kind, "Right");
        click(&context, &mut kind, "Swipe In");
        assert_eq!(
            kind,
            TransitionKind::Swipe {
                direction: TransitionDirection::Right,
                swipe_in: true
            }
        );
        let description = transition_kind_description(kind);
        assert!(description.contains("Right") && description.contains("In"));
        click(&context, &mut kind, "Slide");
        assert_eq!(
            kind,
            TransitionKind::Slide {
                direction: TransitionDirection::Right
            }
        );
        let output = render(&context, &mut kind, vec![]);
        assert!(position(&output, "Swipe In").is_none());
        click(&context, &mut kind, "Up");
        assert_eq!(
            kind,
            TransitionKind::Slide {
                direction: TransitionDirection::Up
            }
        );
        let before = kind;
        let _ = render(&context, &mut kind, vec![]);
        assert_eq!(kind, before);
    }
}

mod recording_warning;

mod obs_audio_controls;

mod native_migration;

pub mod obs_dynamics_controls;

mod craft_time_controls;

pub(crate) mod source_playback_hotkeys;
pub(crate) mod source_audio_hotkeys;
