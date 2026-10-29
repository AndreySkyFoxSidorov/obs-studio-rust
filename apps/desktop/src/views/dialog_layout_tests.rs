//! Actual modal painting, without a native viewport, device discovery or I/O.
//! This is an app child module so the close-document prompt is exercised too.
use super::StudioApp;
use crate::views::dialogs::{AudioDevicePicker, Dialog, ProfileEdit};
use crate::views::{self, AudioEdit, CameraEdit, DisplayEdit, WindowEdit};
use eframe::egui;
use skylive_core::*;
use skylive_host::localization::Localization;
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, OnceLock},
};

const LOCALES: [&str; 5] = ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"];
const SIZES: [(f32, f32); 3] = [(1100.0, 760.0), (800.0, 600.0), (800.0, 560.0)];

fn catalogs() -> &'static [Arc<Localization>; 5] {
    static VALUES: OnceLock<[Arc<Localization>; 5]> = OnceLock::new();
    VALUES
        .get_or_init(|| LOCALES.map(|language| Arc::new(Localization::embedded(language).unwrap())))
}

// Deliberately exhaustive: a new production Dialog variant must get audited.
fn title(dialog: &Dialog) -> &'static str {
    match dialog {
        Dialog::SourceColorTag(_)=>"Custom Color",
        Dialog::SourcePlaybackHotkeys(_)=>"Source Playback Shortcuts",
        Dialog::General { .. } => "Settings — General",
        Dialog::Appearance(_) => "Settings — Appearance",
        Dialog::AddScene { .. } => "Add Scene",
        Dialog::RenameScene { .. } => "Rename Scene",
        Dialog::DuplicateScene { .. } => "Duplicate Scene",
        Dialog::ColorSource(draft) => draft.title(),
        Dialog::AddImage { .. } => "Add Image Source",
        Dialog::ImageFile { .. } => "Properties: Image",
        Dialog::Text(draft) => draft.title(),
        Dialog::MediaSource(draft) => draft.title(),
        Dialog::SceneSource(_) => "Scene Source",
        Dialog::Group(_) => "Group",
        Dialog::Recording(_) => "Settings — Output",
        Dialog::Streaming(_) => "Settings — Stream",
        Dialog::Hotkeys(_) => "Settings — Hotkeys",
        Dialog::WindowCapture(draft) => {
            if matches!(draft.edit, WindowEdit::Add { .. }) {
                "Add Window Capture"
            } else {
                "Properties: Window Capture"
            }
        }
        Dialog::CameraCapture(draft) => {
            if matches!(draft.edit, CameraEdit::Add { .. }) {
                "Add Video Capture Device"
            } else {
                "Properties: Video Capture Device"
            }
        }
        Dialog::DisplayCapture {
            edit: DisplayEdit::Add { .. },
            ..
        } => "Add Display Capture",
        Dialog::DisplayCapture { .. } => "Properties: Display Capture",
        Dialog::AudioCapture {
            edit: AudioEdit::Rebind { .. },
            ..
        } => "Properties: Audio Capture",
        Dialog::AudioCapture {
            flow: AudioFlow::Input,
            ..
        } => "Add Audio Input Capture",
        Dialog::AudioCapture { .. } => "Add Audio Output Capture",
        Dialog::RenameSource { .. } => "Rename Source",
        Dialog::Transform { .. } => "Edit Transform",
        Dialog::SourceOutputResize(_) => "Resize output (source size)",
        Dialog::Video { .. } => "Settings — Video",
        Dialog::AudioSettings { .. } => "Settings — Audio",
        Dialog::Collection { .. } => "Rename Scene Collection",
        Dialog::ProfileName {
            edit: ProfileEdit::New,
            ..
        } => "New Profile",
        Dialog::ProfileName {
            edit: ProfileEdit::Duplicate,
            ..
        } => "Duplicate Profile",
        Dialog::ProfileName { .. } => "Rename Profile",
        Dialog::RemoveProfile { .. } => "Remove Profile",
        Dialog::Plugins { .. } => "Plugins",
        Dialog::AdvancedAudio(_) => "Advanced Audio Properties",
        Dialog::GlobalAudioDevices(_) => "Global Audio Devices",
        Dialog::AudioFilters { .. } => "Audio Filters",
        Dialog::VideoFilters(_) => "Video Filters",
        Dialog::SceneNotes { .. } => "Scene Notes",
        Dialog::QuickRow { id: None, .. } => "Add Quick Transition",
        Dialog::QuickRow { .. } => "Edit Quick Transition",
        Dialog::SceneTransition { .. } => "Scene Transition Override",
        Dialog::TransitionPreset { id: None, .. } => "Add Transition Preset",
        Dialog::TransitionPreset { .. } => "Transition Preset Properties",
        Dialog::RemoveTransitionPreset { .. } => "Remove Transition Preset",
        Dialog::NativeMigration(_) => "Scene filter compatibility",
        Dialog::About => "About SkyLive",
        Dialog::ObsPath { import: true, .. } => "Import OBS Scene Collection",
        Dialog::ObsPath { .. } => "Export OBS Scene Collection",
    }
}

fn fixture() -> Session {
    let mut session = Session::new();
    let scene = session.preview();
    session
        .apply(Command::AddColorSource {
            scene,
            name: "Geometry fixture".into(),
            rgba: [32, 64, 128, 255],
        })
        .unwrap();
    session
        .apply(Command::AddScene {
            name: "Nested scene choice ".repeat(12),
        })
        .unwrap();
    session
        .apply(Command::AddTransitionPreset {
            name: "Directional preset ".repeat(12),
            kind: TransitionKind::Swipe {
                direction: TransitionDirection::Left,
                swipe_in: true,
            },
        })
        .unwrap();
    let color = session.snapshot().sources()[0].id();
    session
        .apply(Command::SetSourceVideoFilters {
            source: color,
            filters: vec![VideoFilter::new(VideoEffect::color_key()).unwrap(); 8],
        })
        .unwrap();
    session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Audio properties fixture".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap();
    let audio = session
        .snapshot()
        .sources()
        .iter()
        .find(|source| matches!(source.kind(), SourceKind::AudioCapture { .. }))
        .unwrap()
        .id();
    session
        .apply(Command::SetSourceAudioFilters {
            source: audio,
            filters: vec![AudioFilter::compressor(); 8],
        })
        .unwrap();
    session
}

fn dialogs(session: &Session) -> Vec<Dialog> {
    let model = session.snapshot();
    let scene = session.preview();
    let item = model.scene(scene).unwrap().items()[0].id();
    let source = model.sources()[0].id();
    let profile = model.profiles().selected_id();
    let preset = model.transition_presets()[0].id();
    let mut result = vec![
        Dialog::general(&model),
        Dialog::AddScene {
            name: "New scene".into(),
        },
        Dialog::RenameScene {
            scene,
            name: "Scene".into(),
        },
        Dialog::DuplicateScene {
            scene,
            name: "Scene copy".into(),
        },
        Dialog::ColorSource(views::color_source::ColorDraft::add(
            scene,
            "Color".into(),
            1920,
            1080,
        )),
        Dialog::AddImage {
            scene,
            name: "Image".into(),
            path: "C:/assets/image.png".into(),
            settings: Default::default(),
        },
        Dialog::ImageFile {
            source,
            path: "C:/assets/image.png".into(),
            settings: Default::default(),
        },
        Dialog::Text(views::text_source::TextDraft::add(scene, "Text".into())),
        Dialog::Text(views::text_source::TextDraft::rebind(
            source,
            &TextSource::default(),
        )),
        Dialog::MediaSource(views::media_source::MediaDraft::add(scene, "Movie".into())),
        Dialog::MediaSource(views::media_source::MediaDraft::rebind(
            source,
            &MediaSource::new("movie.mp4".into(), 64, 48, true).unwrap(),
        )),
        Dialog::SceneSource(views::scene_source::SceneDraft::add(scene)),
        Dialog::SceneSource(views::scene_source::SceneDraft::rebind(
            source,
            model.scenes()[1].id(),
        )),
        Dialog::Group(views::group_source::GroupDraft::empty(&model, scene).unwrap()),
        Dialog::Group(
            views::group_source::GroupDraft::selected(&model, scene, vec![item]).unwrap(),
        ),
        Dialog::RenameSource {
            source,
            name: "Source".into(),
        },
        Dialog::Transform {
            scene,
            item,
            draft: model.scene(scene).unwrap().items()[0].transform().clone(),
            capture_context: None,
        },
        Dialog::ColorSource(views::color_source::ColorDraft::rebind(
            source, [255; 4], 1920, 1080,
        )),
        Dialog::video(model.video()),
        Dialog::audio_settings(&model.audio()),
        Dialog::recording(&model),
        Dialog::Collection {
            name: "Collection".into(),
        },
        Dialog::ProfileName {
            edit: ProfileEdit::New,
            run_setup: true,
            name: "Profile".into(),
        },
        Dialog::ProfileName {
            edit: ProfileEdit::Duplicate,
            run_setup: false,
            name: "Profile copy".into(),
        },
        Dialog::ProfileName {
            edit: ProfileEdit::Rename(profile),
            run_setup: false,
            name: "Renamed profile".into(),
        },
        Dialog::RemoveProfile {
            id: profile,
            name: "Profile".into(),
        },
        Dialog::Plugins {
            query: String::new(),
            session: 0,
            token: 0,
        },
        Dialog::AdvancedAudio(views::advanced_audio::AdvancedAudioDraft::new(
            session.identity(),
        )),
        Dialog::GlobalAudioDevices(views::global_audio_devices::GlobalAudioDraft::new(
            session.identity(),
            crate::platform::CapturePlatform::Windows,
        )),
        Dialog::Appearance(views::appearance::AppearanceDraft::new(
            model.preferences().appearance(),
        )),
        Dialog::streaming(&model),
        Dialog::Hotkeys(views::hotkeys::HotkeyDraft::new(
            model.preferences().hotkeys(),
        )),
        Dialog::audio_filters(
            model
                .sources()
                .iter()
                .find(|source| matches!(source.kind(), SourceKind::AudioCapture { .. }))
                .unwrap(),
        ),
        Dialog::VideoFilters(
            views::video_filters::VideoFilterDraft::capture(session.identity(), &model, source)
                .unwrap(),
        ),
        Dialog::SceneNotes {
            binding: None,
            scene,
            name: "Scene".into(),
            text: "Retained notes\n".repeat(200),
        },
        Dialog::QuickRow {
            id: None,
            reference: TransitionRef::Fade,
            duration_ms: 300,
            fade_to_black: false,
        },
        Dialog::QuickRow {
            id: Some(model.quick_transitions()[0].id()),
            reference: TransitionRef::Fade,
            duration_ms: 300,
            fade_to_black: true,
        },
        Dialog::SceneTransition {
            scene,
            enabled: true,
            reference: TransitionRef::Preset(preset),
            duration_ms: 300,
        },
        Dialog::TransitionPreset {
            id: None,
            name: "New preset".into(),
            kind: TransitionKind::FadeToColor {
                rgb: [255, 255, 255],
                switch_point: 50,
            },
        },
        Dialog::TransitionPreset {
            id: Some(preset),
            name: "Directional preset".into(),
            kind: TransitionKind::Swipe {
                direction: TransitionDirection::Down,
                swipe_in: false,
            },
        },
        Dialog::remove_transition_preset(&model, preset).unwrap(),
        Dialog::NativeMigration(Arc::new(skylive_host::storage::MigrationReport {
            from_schema: 3,
            to_schema: 4,
            scene_source_filters: vec![skylive_host::storage::SceneSourceMigration {
                scene: model.scenes()[0].id(),
                source: Id::new(999),
                name: "Affected scene ".repeat(16),
                filter_count: 8,
            }],
        })),
        Dialog::About,
        Dialog::ObsPath {
            import: true,
            path: "C:/collections/scene.json".into(),
            canvas: "1920x1080".into(),
        },
        Dialog::ObsPath {
            import: false,
            path: "C:/collections/scene.json".into(),
            canvas: "1920x1080".into(),
        },
    ];
    let caps = skylive_host::capture::CaptureCapabilities {
        available: true,
        backend: "metadata-only test",
        cursor: true,
        hdr: false,
        note: None,
    };
    // These are owned fixture metadata, never OS enumeration or captured data.
    let long_name = "Studio metadata-only device ".repeat(12);
    let diagnostic = "The selected device is unavailable. Reopen its properties after checking platform permissions. ".repeat(8);
    for edit in [WindowEdit::Add { scene }, WindowEdit::Rebind { source }] {
        let rebind = matches!(edit, WindowEdit::Rebind { .. });
        let mut draft = views::window_capture::WindowDraft::new(
            1,
            session.identity(),
            model.revision(),
            edit,
            "Window".into(),
            None,
            caps,
        )
        .on_platform(crate::platform::CapturePlatform::Windows);
        draft.loading = false;
        let target = WindowTarget::Windows {
            title: "Metadata-only fixture window".into(),
            class: "FixtureWindowClass".into(),
            executable: "fixture.exe".into(),
        };
        draft.selected = Some(target.clone());
        draft.windows = vec![skylive_host::window_capture::WindowInfo {
            full_window_size: None,
            target,
            name: long_name.clone(),
            width: 1920,
            height: 1080,
            minimized: false,
        }];
        if rebind {
            draft.error = Some(diagnostic.clone());
        }
        result.push(Dialog::WindowCapture(draft));
    }
    for edit in [CameraEdit::Add { scene }, CameraEdit::Rebind { source }] {
        let rebind = matches!(edit, CameraEdit::Rebind { .. });
        let mut draft = views::camera_capture::CameraDraft::new(
            1,
            session.identity(),
            model.revision(),
            edit,
            "Camera".into(),
            None,
            skylive_host::camera_capture::CameraCapabilities {
                available: true,
                backend: "metadata-only test",
                video: true,
                audio: false,
                note: None,
            },
        )
        .on_platform(crate::platform::CapturePlatform::Windows);
        draft.loading_devices = false;
        let target = VideoCaptureTarget::Windows {
            symbolic_link: "metadata-only-camera".into(),
        };
        let mode = VideoCaptureMode::new(1920, 1080, 30000, 1001, VideoPixelFormat::Bgra8).unwrap();
        draft.devices = vec![skylive_host::camera_capture::CameraDeviceInfo {
            target: target.clone(),
            name: long_name.clone(),
        }];
        draft.selected = Some(target.clone());
        let key = skylive_host::camera_capture::CameraRequestKey {
            instance: 1,
            session: session.identity(),
            revision: model.revision(),
            epoch: 0,
            serial: 1,
        };
        draft.modes(
            Some((draft.token, key)),
            key,
            session.identity(),
            model.revision(),
            target,
            Ok(vec![skylive_host::camera_capture::CameraModeInfo {
                width: 1920,
                height: 1080,
                fps_num: 30000,
                fps_den: 1001,
                native_subtype: "BGRA".into(),
                mode: Some(mode),
                unsupported_reason: None,
            }]),
        );
        if rebind {
            draft.error = Some(diagnostic.clone());
        }
        result.push(Dialog::CameraCapture(draft));
    }
    for edit in [DisplayEdit::Add { scene }, DisplayEdit::Rebind { source }] {
        let rebind = matches!(edit, DisplayEdit::Rebind { .. });
        let target = DisplayTarget::Windows {
            device_id: "metadata-display".into(),
        };
        result.push(Dialog::DisplayCapture {
            platform: crate::platform::CapturePlatform::Windows,
            token: 1,
            session: session.identity(),
            revision: model.revision(),
            edit,
            name: "Display".into(),
            selected: Some(target.clone()),
            capture_cursor: false,
            method: DisplayCaptureMethod::Auto,
            displays: vec![skylive_host::capture::DisplayInfo {
                target,
                name: long_name.clone(),
                width: 1920,
                height: 1080,
                primary: true,
            }],
            capabilities: caps,
            loading: false,
            error: rebind.then(|| diagnostic.clone()),
        });
    }
    result.push(Dialog::audio(
        AudioEdit::Add { scene },
        "Microphone".into(),
        AudioFlow::Input,
    ));
    result.push(Dialog::audio(
        AudioEdit::Add { scene },
        "Desktop".into(),
        AudioFlow::Output,
    ));
    let binding =
        AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false).unwrap();
    result.push(Dialog::audio_properties(
        source,
        "Microphone".into(),
        &binding,
    ));
    result.push(Dialog::audio(
        AudioEdit::AddGlobal {
            slot: AudioSlot::Desktop1,
        },
        "Global desktop audio".into(),
        AudioFlow::Output,
    ));
    for dialog in &mut result {
        if let Dialog::AudioCapture {
            flow,
            device_picker,
            edit,
            ..
        } = dialog
        {
            *device_picker = Some(AudioDevicePicker {
                platform: crate::platform::CapturePlatform::Windows,
                token: 1,
                session: session.identity(),
                revision: model.revision(),
                capabilities: skylive_host::audio_capture::AudioCapabilities {
                    backend: "metadata-only test",
                    input: true,
                    output: true,
                    device_enumeration: true,
                    exclude_own_audio: true,
                    note: None,
                },
                devices: vec![skylive_host::audio_capture::AudioDeviceInfo {
                    target: AudioTarget::WindowsEndpoint {
                        id: "metadata-only-endpoint".into(),
                    },
                    name: long_name.clone(),
                    flow: *flow,
                    is_default: true,
                }],
                loading: false,
                error: matches!(edit, AudioEdit::Rebind { .. }).then(|| diagnostic.clone()),
            });
        }
    }
    result
}

fn raw(size: (f32, f32), events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size.0, size.1),
        )),
        events,
        ..Default::default()
    }
}

fn text_rects(
    shape: &egui::epaint::Shape,
    clip: egui::Rect,
    needle: &str,
    output: &mut Vec<(egui::Rect, egui::Rect)>,
) {
    match shape {
        egui::epaint::Shape::Text(text) if text.galley.text() == needle => output.push((
            egui::Rect::from_min_size(text.pos, text.galley.size()),
            clip,
        )),
        egui::epaint::Shape::Vec(shapes) => {
            for shape in shapes {
                text_rects(shape, clip, needle, output);
            }
        }
        _ => {}
    }
}

fn assert_caption(output: &egui::FullOutput, label: &str, bounds: egui::Rect, case: &str) {
    let mut found = vec![];
    for shape in &output.shapes {
        text_rects(&shape.shape, shape.clip_rect, label, &mut found);
    }
    assert!(
        !found.is_empty(),
        "{case}: missing painted caption {label:?}"
    );
    assert!(
        found
            .iter()
            .any(|(rect, clip)| bounds.expand(0.75).contains_rect(*rect)
                && clip.expand(0.75).contains_rect(*rect)),
        "{case}: caption {label:?} clipped: {found:?}, expected inside {bounds:?}"
    );
}

fn assert_actions(
    ctx: &egui::Context,
    output: &egui::FullOutput,
    size: (f32, f32),
    labels: &[&str],
    catalog: &Localization,
    case: &str,
) -> Vec<egui::Rect> {
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size.0, size.1));
    let mut rects = vec![];
    for (index, label) in labels.iter().enumerate() {
        let rect = ctx
            .data(|data| data.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", index))))
            .expect("actual footer must publish action geometry");
        assert!(
            viewport.expand(0.75).contains_rect(rect),
            "{case}: action {label} outside {viewport:?}: {rect:?}"
        );
        assert_caption(output, &catalog.text(label), rect, case);
        if let Some(first) = rects.first() {
            let first: &egui::Rect = first;
            assert!(
                (rect.width() - first.width()).abs() < 0.1
                    && (rect.height() - first.height()).abs() < 0.1,
                "{case}: unequal footer actions"
            );
        }
        assert!(
            rects
                .iter()
                .all(|previous: &egui::Rect| !previous.intersects(rect)),
            "{case}: overlapping footer actions"
        );
        rects.push(rect);
    }
    rects
}

#[test]
fn every_dialog_has_visible_heading_and_clickable_equal_footer_at_supported_sizes_in_five_locales()
{
    let session = fixture();
    let before = session.snapshot();
    let mut covered = BTreeSet::new();
    for catalog in catalogs() {
        for size in SIZES {
            for dialog in dialogs(&session) {
                let heading = title(&dialog);
                covered.insert(heading);
                let labels: &[&str] = match &dialog {
                    dialog if views::settings::page(dialog).is_some() => &["OK", "Cancel", "Apply"],
                    Dialog::NativeMigration(_)
                    | Dialog::About
                    | Dialog::Plugins { .. }
                    | Dialog::AdvancedAudio(_)
                    | Dialog::GlobalAudioDevices(_) => &["Close"],
                    Dialog::ObsPath { import: true, .. } => &["Import", "Cancel"],
                    Dialog::ObsPath { .. } => &["Export", "Cancel"],
                    Dialog::RemoveProfile { .. } | Dialog::RemoveTransitionPreset { .. } => {
                        &["Delete", "Cancel"]
                    }
                    _ => &["Apply", "Cancel"],
                };
                let case = format!("{} {heading} {size:?}", catalog.language());
                let context = egui::Context::default();
                crate::theme::install(&context);
                crate::i18n::install(&context, catalog.clone(), Path::new("embedded:test"));
                let mut slot = Some(dialog);
                let mut intents = vec![];
                let mut output = egui::FullOutput::default();
                for _ in 0..3 {
                    output = context.run(raw(size, vec![]), |ctx| {
                        views::dialogs::show(ctx, &mut slot, &mut intents, true, &before)
                    });
                }
                let viewport =
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size.0, size.1));
                assert_caption(&output, &catalog.text(heading), viewport, &case);
                let rects = assert_actions(&context, &output, size, labels, catalog, &case);
                let dismiss = labels
                    .iter()
                    .position(|label| matches!(*label, "Cancel" | "Close"))
                    .unwrap();
                let pos = rects[dismiss].center();
                for pressed in [true, false] {
                    let _ = context.run(
                        raw(
                            size,
                            vec![
                                egui::Event::PointerMoved(pos),
                                egui::Event::PointerButton {
                                    pos,
                                    button: egui::PointerButton::Primary,
                                    pressed,
                                    modifiers: egui::Modifiers::NONE,
                                },
                            ],
                        ),
                        |ctx| views::dialogs::show(ctx, &mut slot, &mut intents, true, &before),
                    );
                }
                assert!(
                    slot.is_none(),
                    "{case}: footer Cancel/Close could not be clicked"
                );
                assert!(
                    intents.is_empty(),
                    "{case}: painting or cancellation submitted a mutation"
                );
                assert!(Arc::ptr_eq(&before, &session.snapshot()));
            }
        }
    }
    assert_eq!(
        covered.len(),
        52,
        "branch coverage changed; audit new titles explicitly"
    );
}

#[test]
fn actual_close_prompt_localized_four_actions_are_visible_and_cancel_preserves_document() {
    for catalog in catalogs() {
        for size in SIZES {
            let context = egui::Context::default();
            let mut app = StudioApp::headless(&context).unwrap();
            crate::i18n::install(&context, catalog.clone(), Path::new("embedded:test"));
            app.close_dialog = true;
            let before = app.session.snapshot();
            let mut output = egui::FullOutput::default();
            for _ in 0..3 {
                output = context.run(raw(size, vec![]), |ctx| app.close_prompt(ctx));
            }
            let case = format!("{} close prompt {size:?}", catalog.language());
            let rects = assert_actions(
                &context,
                &output,
                size,
                &[
                    "Save and close",
                    "Keep recovery and close",
                    "Discard and close",
                    "Cancel",
                ],
                catalog,
                &case,
            );
            assert_caption(
                &output,
                &catalog.text("Close SkyLive"),
                context.screen_rect(),
                &case,
            );
            let pos = rects[3].center();
            for pressed in [true, false] {
                let _ = context.run(
                    raw(
                        size,
                        vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                    ),
                    |ctx| app.close_prompt(ctx),
                );
            }
            assert!(!app.close_dialog, "{case}: actual Cancel was not reachable");
            assert!(!app.session.is_closed());
            assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        }
    }
}

#[test]
fn maximum_interface_scale_keeps_real_modal_footer_navigation_and_exit_cancel_reachable() {
    // 800x560 native pixels at 200% zoom gives 400x280 logical UI points.
    // Feed the host's logical rectangle after a settling pass, as eframe does.
    let size = (400.0, 280.0);
    for catalog in catalogs()
        .iter()
        .filter(|catalog| matches!(catalog.language(), "de-DE" | "ru-RU"))
    {
        for kind in 0..3 {
            let context = egui::Context::default();
            crate::theme::install(&context);
            crate::i18n::install(
                &context,
                catalog.clone(),
                Path::new("embedded:scale-boundary"),
            );
            let mut session = Session::new();
            session
                .apply(Command::SetAppearance {
                    appearance: Appearance::new(InterfaceTheme::Yami, 200).unwrap(),
                })
                .unwrap();
            let mut runtime = crate::theme::Runtime::default();
            runtime.sync(&context, session.snapshot().preferences().appearance());
            let _ = context.run(raw(size, vec![]), |_| {});
            let mut slot = Some(match kind {
                0 => Dialog::Appearance(views::appearance::AppearanceDraft::new(
                    session.snapshot().preferences().appearance(),
                )),
                1 => {
                    Dialog::GlobalAudioDevices(views::global_audio_devices::GlobalAudioDraft::new(
                        session.identity(),
                        crate::platform::CapturePlatform::Windows,
                    ))
                }
                _ => Dialog::Hotkeys(views::hotkeys::HotkeyDraft::new(
                    session.snapshot().preferences().hotkeys(),
                )),
            });
            let before = session.snapshot();
            let heading = title(slot.as_ref().unwrap());
            let mut output = egui::FullOutput::default();
            let mut intents = vec![];
            for _ in 0..4 {
                output = context.run(raw(size, vec![]), |ctx| {
                    views::dialogs::show(ctx, &mut slot, &mut intents, true, &session.snapshot())
                });
            }
            assert_eq!(context.zoom_factor(), 2.0);
            assert_eq!(context.screen_rect().size(), egui::vec2(400.0, 280.0));
            let labels = if kind == 1 {
                vec!["Close"]
            } else {
                vec!["OK", "Cancel", "Apply"]
            };
            let case = format!("{} 200% {heading}", catalog.language());
            let rects = assert_actions(&context, &output, size, &labels, catalog, &case);
            assert_caption(
                &output,
                &catalog.text(heading),
                context.screen_rect(),
                &case,
            );
            if kind != 1 {
                assert_caption(
                    &output,
                    &catalog.text("General"),
                    context.screen_rect(),
                    &case,
                );
            }
            let dismiss = labels
                .iter()
                .position(|label| matches!(*label, "Cancel" | "Close"))
                .unwrap();
            let cancel = rects[dismiss].center();
            for pressed in [true, false] {
                let _ = context.run(
                    raw(
                        size,
                        vec![
                            egui::Event::PointerMoved(cancel),
                            egui::Event::PointerButton {
                                pos: cancel,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                    ),
                    |ctx| {
                        views::dialogs::show(
                            ctx,
                            &mut slot,
                            &mut intents,
                            true,
                            &session.snapshot(),
                        )
                    },
                );
            }
            assert!(slot.is_none(), "{case}: actual footer click must close");
            assert!(intents.is_empty());
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        crate::i18n::install(
            &context,
            catalog.clone(),
            Path::new("embedded:scale-boundary"),
        );
        app.session
            .apply(Command::SetAppearance {
                appearance: Appearance::new(InterfaceTheme::Yami, 200).unwrap(),
            })
            .unwrap();
        app.appearance_runtime
            .sync(&context, app.session.snapshot().preferences().appearance());
        let _ = context.run(raw(size, vec![]), |_| {});
        app.close_dialog = true;
        let before = app.session.snapshot();
        let mut output = egui::FullOutput::default();
        for _ in 0..4 {
            output = context.run(raw(size, vec![]), |ctx| app.close_prompt(ctx));
        }
        let case = format!("{} 200% close prompt", catalog.language());
        let rects = assert_actions(
            &context,
            &output,
            size,
            &[
                "Save and close",
                "Keep recovery and close",
                "Discard and close",
                "Cancel",
            ],
            catalog,
            &case,
        );
        assert_caption(
            &output,
            &catalog.text("Close SkyLive"),
            context.screen_rect(),
            &case,
        );
        let cancel = rects[3].center();
        for pressed in [true, false] {
            let _ = context.run(
                raw(
                    size,
                    vec![
                        egui::Event::PointerMoved(cancel),
                        egui::Event::PointerButton {
                            pos: cancel,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                ),
                |ctx| app.close_prompt(ctx),
            );
        }
        assert!(!app.close_dialog);
        assert!(!app.session.is_closed());
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    }
}

#[test]
fn audio_settings_help_paragraphs_paint_inside_body_at_light_150_percent() {
    let logical = (1280.0 / 1.5, 800.0 / 1.5);
    let paragraphs = [
        "Enable to reduce the buffer limit; this can be less reliable on busy devices. Turning this option off restores automatic buffering.",
        "Devices can use a different input format; audio is converted to these project settings. Speaker playback and monitoring are not connected.",
        "Apply updates the selected profile's audio format and user buffering preference together. These settings are separate from scene Undo. Source bindings, gain and mute stay unchanged.",
    ];
    for catalog in catalogs()
        .iter()
        .filter(|catalog| matches!(catalog.language(), "en-US" | "de-DE" | "ru-RU"))
    {
        let context = egui::Context::default();
        crate::theme::install(&context);
        crate::i18n::install(&context, catalog.clone(), Path::new("embedded:audio-wrap"));
        let mut session = Session::new();
        session
            .apply(Command::SetAppearance {
                appearance: Appearance::new(InterfaceTheme::Light, 150).unwrap(),
            })
            .unwrap();
        let mut runtime = crate::theme::Runtime::default();
        runtime.sync(&context, session.snapshot().preferences().appearance());
        let _ = context.run(raw(logical, vec![]), |_| {});
        let before = session.snapshot();
        let mut slot = Some(Dialog::audio_settings(&before.audio()));
        let mut intents = vec![];
        let mut output = egui::FullOutput::default();
        for _ in 0..4 {
            output = context.run(raw(logical, vec![]), |ctx| {
                views::dialogs::show(ctx, &mut slot, &mut intents, true, &session.snapshot())
            });
        }
        assert_eq!(context.zoom_factor(), 1.5);
        assert!(!context.style().visuals.dark_mode);
        let case = format!("{} Audio body Light150", catalog.language());
        assert_actions(
            &context,
            &output,
            logical,
            &["OK", "Cancel", "Apply"],
            catalog,
            &case,
        );
        let labels = paragraphs.map(|paragraph| catalog.text(paragraph));
        let mut seen = [false; 3];
        for _ in 0..10 {
            let mut scroll_point = context.screen_rect().center();
            for (index, label) in labels.iter().enumerate() {
                let mut found = vec![];
                for shape in &output.shapes {
                    text_rects(&shape.shape, shape.clip_rect, label, &mut found);
                }
                for (rect, clip) in found {
                    assert!(
                        rect.left() >= clip.left() - 0.75 && rect.right() <= clip.right() + 0.75,
                        "{case}: paragraph extends horizontally beyond body: {rect:?} vs {clip:?}: {label}"
                    );
                    scroll_point = clip.center();
                    if clip.expand(0.75).contains_rect(rect) {
                        seen[index] = true;
                    }
                }
            }
            if seen.iter().all(|value| *value) {
                break;
            }
            // Long translations may need ordinary vertical scrolling; no hidden
            // text is treated as visible merely because it has a layout rect.
            output = context.run(
                raw(
                    logical,
                    vec![
                        egui::Event::PointerMoved(scroll_point),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, -70.0),
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                ),
                |ctx| views::dialogs::show(ctx, &mut slot, &mut intents, true, &session.snapshot()),
            );
            for _ in 0..3 {
                output = context.run(raw(logical, vec![]), |ctx| {
                    views::dialogs::show(ctx, &mut slot, &mut intents, true, &session.snapshot())
                });
            }
        }
        assert!(
            seen.iter().all(|value| *value),
            "{case}: all real help paragraphs must become fully visible by scrolling: {seen:?}"
        );
        let footer = assert_actions(
            &context,
            &output,
            logical,
            &["OK", "Cancel", "Apply"],
            catalog,
            &case,
        );
        // A real pointer still reaches Cancel and no format/preference edit is published.
        let point = footer[1].center();
        for pressed in [true, false] {
            let _ = context.run(
                raw(
                    logical,
                    vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                ),
                |ctx| views::dialogs::show(ctx, &mut slot, &mut intents, true, &session.snapshot()),
            );
        }
        assert!(slot.is_none());
        assert!(intents.is_empty());
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
    }
}

#[test]
fn actual_main_status_text_stays_inside_its_panel_at_enlarged_scales() {
    let startup = "Development build — local scene composition, capture and MP4 recording. Streaming and virtual-camera output are pending.";
    let path_status = "Recording saved: C:/Example/SkyLive/recordings/owned-test-output-with-a-long-file-name-and-explicit-destination/recording-123456789.mp4";
    for catalog in catalogs()
        .iter()
        .filter(|catalog| matches!(catalog.language(), "en-US" | "de-DE" | "ru-RU"))
    {
        for scale in [150, 200] {
            let context = egui::Context::default();
            let mut app = StudioApp::headless(&context).unwrap();
            app.session
                .apply(Command::SetLanguage {
                    language: catalog.language().into(),
                })
                .unwrap();
            app.session
                .apply(Command::SetAppearance {
                    appearance: Appearance::new(InterfaceTheme::Light, scale).unwrap(),
                })
                .unwrap();
            app.localization = catalog.clone();
            crate::i18n::install(&context, catalog.clone(), Path::new("embedded:main-status"));
            let logical = (1280.0 * 100.0 / scale as f32, 800.0 * 100.0 / scale as f32);
            for _ in 0..3 {
                let _ = context.run(raw(logical, vec![]), |ctx| {
                    app.frame_at(ctx, skylive_media::MediaTime(0))
                });
            }
            for (message, error) in [(startup, false), (path_status, false), (path_status, true)] {
                // Chrome is translated at paint time; an owned path receipt is
                // opaque. Exercise the actual message boundary, not direct field
                // assignment with stale presentation metadata.
                let expected = if message == startup {
                    app.message(error, startup);
                    catalog.text(startup).into_owned()
                } else {
                    app.message(error, message.to_owned());
                    message.to_owned()
                };
                let mut output = egui::FullOutput::default();
                for _ in 0..3 {
                    output = context.run(raw(logical, vec![]), |ctx| {
                        app.frame_at(ctx, skylive_media::MediaTime(0))
                    });
                }
                let mut found = vec![];
                for shape in &output.shapes {
                    text_rects(&shape.shape, shape.clip_rect, &expected, &mut found);
                }
                assert!(
                    !found.is_empty(),
                    "actual main status must paint its complete string"
                );
                for (rect, clip) in found {
                    assert!(
                        clip.expand(0.75).contains_rect(rect),
                        "{} Light{scale}: status paint {rect:?} outside clip {clip:?}",
                        catalog.language()
                    );
                    assert!(context.screen_rect().expand(0.75).contains_rect(rect));
                }
            }
        }
    }
}

#[test]
fn audio_format_and_custom_buffering_summaries_use_complete_localized_templates() {
    let size = (1100.0, 760.0);
    for catalog in catalogs() {
        for (maximum, fixed) in [(120, false), (120, true), (0, true)] {
            let context = egui::Context::default();
            crate::theme::install(&context);
            crate::i18n::install(
                &context,
                catalog.clone(),
                Path::new("embedded:audio-summaries"),
            );
            let mut session = Session::new();
            let settings = AudioSettings::new(48000, SpeakerLayout::Mono)
                .unwrap()
                .with_buffering(AudioBuffering::new(maximum, fixed).unwrap())
                .unwrap();
            session
                .apply(Command::SetAudioSettings { settings })
                .unwrap();
            let before = session.snapshot();
            let mut slot = Some(Dialog::audio_settings(&before.audio()));
            let mut intents = Vec::new();
            let mut output = egui::FullOutput::default();
            for _ in 0..4 {
                output = context.run(raw(size, vec![]), |ctx| {
                    views::dialogs::show(ctx, &mut slot, &mut intents, true, &before)
                });
            }
            let summary = catalog
                .text("Chosen Program format: {} · {} · {} channels")
                .replacen("{}", "48 kHz", 1)
                .replacen("{}", &catalog.text("Mono"), 1)
                .replacen("{}", "1", 1);
            let template = if maximum == 0 {
                "Custom buffering uses the default maximum and is preserved."
            } else if fixed {
                "Custom fixed buffering, maximum {max_ms} ms, is preserved."
            } else {
                "Custom automatic buffering, maximum {max_ms} ms, is preserved."
            };
            let buffering = catalog
                .text(template)
                .replace("{max_ms}", &maximum.to_string());
            let viewport = context.screen_rect();
            assert_caption(&output, &summary, viewport, catalog.language());
            assert_caption(&output, &buffering, viewport, catalog.language());
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
            assert!(intents.is_empty());
        }
    }
}

#[test]
fn scene_migration_receipt_close_and_heading_fit_at_150_and_200_percent_in_five_locales() {
    for catalog in catalogs() {
        for scale in [150, 200] {
            let context = egui::Context::default();
            crate::theme::install(&context);
            crate::i18n::install(
                &context,
                catalog.clone(),
                Path::new("embedded:scene-migration-scale"),
            );
            let mut session = Session::new();
            session
                .apply(Command::SetAppearance {
                    appearance: Appearance::new(InterfaceTheme::Yami, scale).unwrap(),
                })
                .unwrap();
            let mut runtime = crate::theme::Runtime::default();
            runtime.sync(&context, session.snapshot().preferences().appearance());
            let size = (800.0 * 100.0 / scale as f32, 560.0 * 100.0 / scale as f32);
            let _ = context.run(raw(size, vec![]), |_| {});
            let report = skylive_host::storage::MigrationReport {
                from_schema: 3,
                to_schema: 4,
                scene_source_filters: (0..16)
                    .map(|index| skylive_host::storage::SceneSourceMigration {
                        scene: Id::new(index + 1),
                        source: Id::new(index + 100),
                        name: format!("Affected scene {index} {}", "long-name ".repeat(20)),
                        filter_count: 8,
                    })
                    .collect(),
            };
            let before = session.snapshot();
            let mut slot = Some(Dialog::NativeMigration(Arc::new(report)));
            let mut intents = vec![];
            let mut output = egui::FullOutput::default();
            for _ in 0..4 {
                output = context.run(raw(size, vec![]), |ctx| {
                    views::dialogs::show(ctx, &mut slot, &mut intents, true, &before)
                });
            }
            let case = format!("{} migration {scale}%", catalog.language());
            let actions = assert_actions(&context, &output, size, &["Close"], catalog, &case);
            assert_caption(
                &output,
                &catalog.text("Scene filter compatibility"),
                context.screen_rect(),
                &case,
            );
            for pressed in [true, false] {
                let _ = context.run(
                    raw(
                        size,
                        vec![
                            egui::Event::PointerMoved(actions[0].center()),
                            egui::Event::PointerButton {
                                pos: actions[0].center(),
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                    ),
                    |ctx| views::dialogs::show(ctx, &mut slot, &mut intents, true, &before),
                );
            }
            assert!(slot.is_none());
            assert!(intents.is_empty());
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
    }
}
