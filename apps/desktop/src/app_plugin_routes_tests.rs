//! Real manager widgets and coordinator; no native viewport/device enumeration.
use super::*;
use crate::views::plugin_routes::{PluginEditorRequest, PluginTarget};
use skylive_core::{
    AUDIO_PAN_PLUGIN, AudioCaptureBinding, AudioFilter, AudioFlow, AudioTarget, PluginOperation,
};

fn fixture(context: &egui::Context) -> (StudioApp, Id, Id) {
    let mut app = StudioApp::headless(context).unwrap();
    let scene = app.session.preview();
    app.session
        .apply(Command::AddAudioCaptureSource {
            scene,
            name: "Target audio".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        })
        .unwrap();
    let audio = app.session.snapshot().sources()[0].id();
    app.session
        .apply(Command::AddColorSource {
            scene,
            name: "Target color".into(),
            rgba: [255; 4],
        })
        .unwrap();
    let video = app.session.snapshot().sources()[1].id();
    (app, audio, video)
}
fn open_manager(app: &mut StudioApp, context: &egui::Context, plugin: &str) {
    app.intent(
        Intent::OpenDialog(Dialog::Plugins {
            query: plugin.into(),
            session: 0,
            token: 0,
        }),
        context,
    );
}
fn request(
    app: &StudioApp,
    plugin: &str,
    operation: PluginOperation,
    target: PluginTarget,
) -> PluginEditorRequest {
    let Some(Dialog::Plugins { token, .. }) = &app.dialog else {
        panic!("manager required")
    };
    PluginEditorRequest {
        session: app.session.identity(),
        revision: app.session.snapshot().revision(),
        manager: *token,
        plugin: plugin.into(),
        operation,
        target,
        filter: None,
    }
}
fn frame(
    app: &mut StudioApp,
    context: &egui::Context,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut intents = vec![];
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 900.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            dialogs::show(
                ctx,
                &mut app.dialog,
                &mut intents,
                true,
                &app.session.snapshot(),
            );
        },
    );
    for intent in intents {
        app.intent(intent, context);
    }
    output
}
fn text(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
    match shape {
        egui::epaint::Shape::Text(text) if text.galley.text() == label => {
            Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
        }
        egui::epaint::Shape::Vec(shapes) => {
            shapes.iter().find_map(|shape| self::text(shape, label))
        }
        _ => None,
    }
}
fn click(app: &mut StudioApp, context: &egui::Context, label: &str) {
    frame(app, context, vec![]);
    let output = frame(app, context, vec![]);
    let pos = output
        .shapes
        .iter()
        .find_map(|shape| {
            text(&shape.shape, label).filter(|rect| shape.clip_rect.contains_rect(*rect))
        })
        .unwrap_or_else(|| panic!("visible action {label}"))
        .center();
    frame(
        app,
        context,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    frame(
        app,
        context,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
}
#[test]
fn manager_enable_route_add_apply_undo_and_disabled_retention_use_real_owners() {
    let context = egui::Context::default();
    let (mut app, audio, _) = fixture(&context);
    open_manager(&mut app, &context, AUDIO_PAN_PLUGIN);
    click(&mut app, &context, "Open Audio Filters");
    assert!(matches!(app.dialog, Some(Dialog::Plugins { .. })));
    let original = app.session.snapshot();
    click(&mut app, &context, "Enabled");
    assert!(
        app.session
            .snapshot()
            .preferences()
            .plugin_enabled(AUDIO_PAN_PLUGIN)
    );
    assert_eq!(
        app.session
            .snapshot()
            .source(audio)
            .unwrap()
            .audio_filters(),
        original.source(audio).unwrap().audio_filters()
    );
    let enabled = app.session.snapshot();
    click(&mut app, &context, "Open Audio Filters");
    assert!(
        matches!(&app.dialog,Some(Dialog::AudioFilters{source,filters,..})if *source==audio&&filters.is_empty())
    );
    assert!(Arc::ptr_eq(&enabled, &app.session.snapshot()));
    click(&mut app, &context, "Add Audio Pan Filter");
    let Some(Dialog::AudioFilters { filters, .. }) = &mut app.dialog else {
        panic!("filter editor")
    };
    let AudioFilter::Pan { pan_left, .. } = &mut filters[0] else {
        panic!("Pan")
    };
    *pan_left = 0.0;
    assert!(Arc::ptr_eq(&enabled, &app.session.snapshot()));
    click(&mut app, &context, "Apply");
    assert!(!app.error, "{}", app.status);
    let authored = app
        .session
        .snapshot()
        .source(audio)
        .unwrap()
        .audio_filters()[0];
    assert!(matches!(authored,AudioFilter::Pan{pan_left,..}if pan_left==0.0));
    app.apply(Command::Undo);
    assert!(
        app.session
            .snapshot()
            .source(audio)
            .unwrap()
            .audio_filters()
            .is_empty()
    );
    app.apply(Command::Redo);
    open_manager(&mut app, &context, AUDIO_PAN_PLUGIN);
    click(&mut app, &context, "Enabled");
    assert!(
        !app.session
            .snapshot()
            .preferences()
            .plugin_enabled(AUDIO_PAN_PLUGIN)
    );
    assert_eq!(
        app.session
            .snapshot()
            .source(audio)
            .unwrap()
            .audio_filters(),
        &[authored]
    );
    let bytes = skylive_host::storage::encode(&app.session.snapshot()).unwrap();
    let reopened = skylive_host::storage::decode(&bytes, true).unwrap();
    assert_eq!(
        reopened.snapshot().source(audio).unwrap().audio_filters(),
        &[authored]
    );
    assert!(
        !reopened
            .snapshot()
            .preferences()
            .plugin_enabled(AUDIO_PAN_PLUGIN)
    );
}
#[test]
fn routes_reject_foreign_stale_disabled_unregistered_and_incompatible_targets() {
    let context = egui::Context::default();
    let (mut app, audio, video) = fixture(&context);
    open_manager(&mut app, &context, AUDIO_PAN_PLUGIN);
    let baseline = app.session.snapshot();
    let disabled = request(
        &app,
        AUDIO_PAN_PLUGIN,
        PluginOperation::AudioFilters,
        PluginTarget::Source(audio),
    );
    app.intent(Intent::OpenPluginEditor(disabled), &context);
    assert!(app.error);
    assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
    app.intent(
        Intent::SetPluginEnabled {
            plugin: AUDIO_PAN_PLUGIN.into(),
            enabled: true,
        },
        &context,
    );
    let valid = request(
        &app,
        AUDIO_PAN_PLUGIN,
        PluginOperation::AudioFilters,
        PluginTarget::Source(audio),
    );
    let baseline = app.session.snapshot();
    let mut invalid = vec![];
    let mut value = valid.clone();
    value.session += 1;
    invalid.push(value);
    let mut value = valid.clone();
    value.revision = value.revision.saturating_sub(1);
    invalid.push(value);
    let mut value = valid.clone();
    value.manager += 1;
    invalid.push(value);
    let mut value = valid.clone();
    value.plugin = "norihiro/obs-mute-filter".into();
    invalid.push(value);
    let mut value = valid.clone();
    value.operation = PluginOperation::VideoFilters;
    invalid.push(value);
    let mut value = valid.clone();
    value.target = PluginTarget::Source(video);
    invalid.push(value);
    let mut value = valid.clone();
    value.target = PluginTarget::Source(Id::new(u64::MAX));
    invalid.push(value);
    for value in invalid {
        app.intent(Intent::OpenPluginEditor(value), &context);
        assert!(app.error);
        assert!(matches!(app.dialog, Some(Dialog::Plugins { .. })));
        assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
    }
    app.dialog = None;
    app.intent(Intent::OpenPluginEditor(valid.clone()), &context);
    assert!(app.error);
    assert!(app.dialog.is_none());
    open_manager(&mut app, &context, AUDIO_PAN_PLUGIN);
    app.intent(Intent::OpenPluginEditor(valid), &context);
    assert!(app.error);
    assert!(matches!(app.dialog, Some(Dialog::Plugins { .. })));
    assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
}
#[test]
fn video_and_notes_navigation_does_not_create_sources_filters_or_history() {
    let context = egui::Context::default();
    let (mut app, _, video) = fixture(&context);
    let scene = app.session.preview();
    for (plugin, operation, target) in [
        (
            skylive_core::CRAFT_VIDEO_PLUGIN,
            PluginOperation::VideoFilters,
            PluginTarget::Source(video),
        ),
        (
            skylive_core::OBS_VIDEO_FILTERS_PLUGIN,
            PluginOperation::VideoFilters,
            PluginTarget::Scene(scene),
        ),
    ] {
        open_manager(&mut app, &context, plugin);
        let baseline = app.session.snapshot();
        let route = request(&app, plugin, operation, target);
        app.intent(Intent::OpenPluginEditor(route), &context);
        assert!(!app.error, "{}", app.status);
        assert!(matches!(app.dialog, Some(Dialog::VideoFilters(_))));
        assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
        click(&mut app, &context, "Cancel");
        assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
    }
    app.intent(
        Intent::SetPluginEnabled {
            plugin: skylive_core::SCENE_NOTES_PLUGIN.into(),
            enabled: true,
        },
        &context,
    );
    open_manager(&mut app, &context, skylive_core::SCENE_NOTES_PLUGIN);
    let baseline = app.session.snapshot();
    click(&mut app, &context, "Open Scene Notes");
    assert!(matches!(&app.dialog,Some(Dialog::SceneNotes{scene:id,..})if *id==scene));
    assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
    click(&mut app, &context, "Cancel");
    assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
}

fn editor_apply(
    app: &StudioApp,
) -> (
    Option<crate::views::plugin_routes::PluginEditorToken>,
    Command,
) {
    let dialog = app.dialog.as_ref().unwrap();
    let token = match dialog {
        Dialog::AudioFilters { binding, .. } | Dialog::SceneNotes { binding, .. } => *binding,
        _ => panic!("bound plugin draft"),
    };
    (token, dialog.command().unwrap().unwrap())
}
fn audio_draft(app: &mut StudioApp, context: &egui::Context, audio: Id) {
    let source = app.session.snapshot().source(audio).unwrap().clone();
    app.intent(Intent::OpenDialog(Dialog::audio_filters(&source)), context);
    let Some(Dialog::AudioFilters { filters, .. }) = &mut app.dialog else {
        panic!("audio draft")
    };
    filters.push(AudioFilter::obs_gain());
}
#[test]
fn audio_parameter_apply_rejects_stale_reopened_foreign_and_changed_draft_without_publication() {
    let context = egui::Context::default();
    let (mut app, audio, _) = fixture(&context);
    audio_draft(&mut app, &context, audio);
    let (token, command) = editor_apply(&app);
    let before = app.session.snapshot();
    let mut foreign = token.unwrap();
    foreign.session += 1;
    app.intent(
        Intent::ApplyPluginEditor {
            token: Some(foreign),
            command: command.clone(),
        },
        &context,
    );
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    app.intent(
        Intent::ApplyPluginEditor {
            token,
            command: Command::AddScene {
                name: "Injected command".into(),
            },
        },
        &context,
    );
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    if let Some(Dialog::AudioFilters { filters, .. }) = &mut app.dialog {
        filters[0] = AudioFilter::ObsGain {
            db: 3.0,
            enabled: true,
        };
    }
    app.intent(
        Intent::ApplyPluginEditor {
            token,
            command: command.clone(),
        },
        &context,
    );
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    assert!(matches!(app.dialog, Some(Dialog::AudioFilters { .. })));
    app.dialog = None;
    audio_draft(&mut app, &context, audio);
    let (fresh, _) = editor_apply(&app);
    assert_ne!(fresh, token);
    app.intent(
        Intent::ApplyPluginEditor {
            token,
            command: command.clone(),
        },
        &context,
    );
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    let (_, current_command) = editor_apply(&app);
    app.session
        .apply(Command::RenameSource {
            source: audio,
            name: "Renamed while editing".into(),
        })
        .unwrap();
    let changed = app.session.snapshot();
    app.intent(
        Intent::ApplyPluginEditor {
            token: fresh,
            command: current_command,
        },
        &context,
    );
    assert!(app.error);
    assert!(Arc::ptr_eq(&changed, &app.session.snapshot()));
    assert!(
        matches!(&app.dialog,Some(Dialog::AudioFilters{filters,..})if filters==&vec![AudioFilter::obs_gain()])
    );
}
#[test]
fn notes_parameter_apply_is_fenced_and_valid_apply_uses_one_canonical_edit() {
    let context = egui::Context::default();
    let (mut app, _, _) = fixture(&context);
    let scene = app.session.preview();
    app.intent(
        Intent::SetPluginEnabled {
            plugin: skylive_core::SCENE_NOTES_PLUGIN.into(),
            enabled: true,
        },
        &context,
    );
    let open = |app: &mut StudioApp| {
        let canonical = app.session.snapshot().scene(scene).unwrap().clone();
        app.intent(
            Intent::OpenDialog(Dialog::scene_notes(&canonical)),
            &context,
        );
        if let Some(Dialog::SceneNotes { text, .. }) = &mut app.dialog {
            *text = "Cue one\nПочаток".into();
        }
    };
    open(&mut app);
    let (old, command) = editor_apply(&app);
    let before = app.session.snapshot();
    app.dialog = None;
    open(&mut app);
    let (current, current_command) = editor_apply(&app);
    assert_ne!(old, current);
    app.intent(
        Intent::ApplyPluginEditor {
            token: old,
            command,
        },
        &context,
    );
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    app.intent(
        Intent::ApplyPluginEditor {
            token: None,
            command: current_command.clone(),
        },
        &context,
    );
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    app.intent(
        Intent::ApplyPluginEditor {
            token: current,
            command: current_command.clone(),
        },
        &context,
    );
    assert!(!app.error, "{}", app.status);
    assert!(app.dialog.is_none());
    assert_eq!(app.session.snapshot().revision(), before.revision() + 1);
    assert_eq!(
        app.session.snapshot().scene(scene).unwrap().notes(),
        "Cue one\nПочаток"
    );
    let committed = app.session.snapshot();
    app.intent(
        Intent::ApplyPluginEditor {
            token: current,
            command: current_command,
        },
        &context,
    );
    assert!(app.error);
    assert!(Arc::ptr_eq(&committed, &app.session.snapshot()));
    app.apply(Command::Undo);
    assert_eq!(app.session.snapshot().scene(scene).unwrap().notes(), "");
}
#[test]
fn obsolete_initial_filter_draft_and_exhausted_identity_keep_current_window_and_model() {
    let context = egui::Context::default();
    let (mut app, audio, _) = fixture(&context);
    let stale = Dialog::audio_filters(app.session.snapshot().source(audio).unwrap());
    app.session
        .apply(Command::SetSourceAudioFilters {
            source: audio,
            filters: vec![AudioFilter::obs_gain()],
        })
        .unwrap();
    open_manager(&mut app, &context, AUDIO_PAN_PLUGIN);
    let before = app.session.snapshot();
    app.intent(Intent::OpenDialog(stale), &context);
    assert!(app.error);
    assert!(matches!(app.dialog, Some(Dialog::Plugins { .. })));
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    app.plugin_dialog_sequence = u64::MAX;
    let fresh = Dialog::audio_filters(before.source(audio).unwrap());
    app.intent(Intent::OpenDialog(fresh), &context);
    assert!(app.error);
    assert!(matches!(app.dialog, Some(Dialog::Plugins { .. })));
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
}
