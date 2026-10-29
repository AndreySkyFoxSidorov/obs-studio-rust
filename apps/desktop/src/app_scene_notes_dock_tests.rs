use super::*;
use skylive_core::SCENE_NOTES_PLUGIN;

fn fixture(ctx: &egui::Context) -> StudioApp {
    let mut app = StudioApp::headless(ctx).unwrap();
    app.apply(Command::SetVideo {
        width: 64,
        height: 48,
        fps_num: 30,
        fps_den: 1,
    });
    app
}
fn frame(
    app: &mut StudioApp,
    ctx: &egui::Context,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        },
        |ctx| app.frame_at(ctx, MediaTime(0)),
    )
}
fn settle(app: &mut StudioApp, ctx: &egui::Context, size: egui::Vec2) {
    for _ in 0..3 {
        frame(app, ctx, size, vec![]);
    }
}
fn probe(ctx: &egui::Context, key: &str) -> Option<egui::Rect> {
    ctx.data(|data| data.get_temp::<egui::Rect>(egui::Id::new(key)))
}
fn click_at(app: &mut StudioApp, ctx: &egui::Context, size: egui::Vec2, position: egui::Pos2) {
    for pressed in [true, false] {
        frame(
            app,
            ctx,
            size,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}
fn point(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
    match shape {
        egui::epaint::Shape::Text(text) if text.galley.text() == label => {
            Some(text.pos + text.galley.size() * 0.5)
        }
        egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|shape| point(shape, label)),
        _ => None,
    }
}
fn click_text(app: &mut StudioApp, ctx: &egui::Context, size: egui::Vec2, label: &str) {
    settle(app, ctx, size);
    let output = frame(app, ctx, size, vec![]);
    let position = output
        .shapes
        .iter()
        .rev()
        .find_map(|shape| point(&shape.shape, label).filter(|p| shape.clip_rect.contains(*p)))
        .unwrap_or_else(|| panic!("visible {label}"));
    click_at(app, ctx, size, position);
}
fn enable(app: &mut StudioApp, ctx: &egui::Context, enabled: bool) {
    app.intent(
        Intent::SetPluginEnabled {
            plugin: SCENE_NOTES_PLUGIN.into(),
            enabled,
        },
        ctx,
    );
}
#[test]
fn actual_dock_text_apply_roundtrip_undo_and_dynamic_menu_hide_are_connected() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    let size = egui::vec2(1280.0, 800.0);
    let scene = app.session.preview();
    settle(&mut app, &ctx, size);
    assert!(probe(&ctx, "actual-notes-dock").is_none());
    enable(&mut app, &ctx, true);
    settle(&mut app, &ctx, size);
    assert!(probe(&ctx, "actual-notes-dock").is_some());
    let before = app.session.snapshot();
    let editor = probe(&ctx, "scene-notes-editor-rect").unwrap();
    click_at(&mut app, &ctx, size, editor.center());
    frame(
        &mut app,
        &ctx,
        size,
        vec![egui::Event::Text("Cue one\nПочаток".into())],
    );
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    let submission = app
        .notes_dock
        .submission(app.session.identity(), &app.session.snapshot())
        .unwrap();
    assert_eq!(submission.text, "Cue one\nПочаток");
    app.intent(Intent::ToggleDock(views::Dock::SceneNotes), &ctx);
    app.intent(Intent::ApplyNotesDock(submission.clone()), &ctx);
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    app.intent(Intent::ToggleDock(views::Dock::SceneNotes), &ctx);
    app.dialog = Some(Dialog::About);
    app.intent(Intent::ApplyNotesDock(submission.clone()), &ctx);
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    app.dialog = None;
    app.after_io = Some(AfterIo::ReplaceDocument);
    app.intent(Intent::ApplyNotesDock(submission.clone()), &ctx);
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    app.after_io = None;
    settle(&mut app, &ctx, size);
    let apply = probe(&ctx, "scene-notes-apply-rect").unwrap();
    click_at(&mut app, &ctx, size, apply.center());
    assert!(!app.error, "{}", app.status);
    assert_eq!(app.session.snapshot().revision(), before.revision() + 1);
    assert_eq!(
        app.session.snapshot().scene(scene).unwrap().notes(),
        submission.text
    );
    let bytes = skylive_host::storage::encode(&app.session.snapshot()).unwrap();
    let reopened = skylive_host::storage::decode(&bytes, true).unwrap();
    assert_eq!(
        reopened.snapshot().scene(scene).unwrap().notes(),
        submission.text
    );
    let committed = app.session.snapshot();
    app.intent(Intent::ApplyNotesDock(submission), &ctx);
    assert!(app.error);
    assert!(Arc::ptr_eq(&committed, &app.session.snapshot()));
    app.apply(Command::Undo);
    settle(&mut app, &ctx, size);
    assert_eq!(app.session.snapshot().scene(scene).unwrap().notes(), "");
    assert!(
        app.session
            .snapshot()
            .preferences()
            .plugin_enabled(SCENE_NOTES_PLUGIN)
    );
    let unchanged = app.session.snapshot();
    click_text(&mut app, &ctx, size, "Docks");
    click_text(&mut app, &ctx, size, "Scene Notes Dock");
    settle(&mut app, &ctx, size);
    assert!(!app.layout.arrangement.scene_notes_visible);
    assert!(probe(&ctx, "actual-notes-dock").is_none());
    assert!(Arc::ptr_eq(&unchanged, &app.session.snapshot()));
    // Close the menu before changing capability, then verify no stale registration.
    frame(
        &mut app,
        &ctx,
        size,
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    enable(&mut app, &ctx, false);
    settle(&mut app, &ctx, size);
    click_text(&mut app, &ctx, size, "Docks");
    let output = frame(&mut app, &ctx, size, vec![]);
    assert!(
        output
            .shapes
            .iter()
            .all(|shape| point(&shape.shape, "Scene Notes Dock").is_none())
    );
}
#[test]
fn notes_dock_does_not_move_controls_and_first_stop_stays_reachable_in_five_locales() {
    use std::sync::atomic::Ordering;
    for locale in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
        for (physical, scale) in [
            (egui::vec2(1280.0, 800.0), 100_u16),
            (egui::vec2(800.0, 560.0), 150),
            (egui::vec2(800.0, 560.0), 200),
        ] {
            let ctx = egui::Context::default();
            let mut app = fixture(&ctx);
            app.localization =
                Arc::new(skylive_host::localization::Localization::embedded(locale).unwrap());
            crate::i18n::install(&ctx, app.localization.clone(), &app.localization_path);
            app.apply(Command::SetAppearance {
                appearance: skylive_core::Appearance::new(
                    skylive_core::InterfaceTheme::Yami,
                    scale,
                )
                .unwrap(),
            });
            let size = physical / (f32::from(scale) / 100.0);
            settle(&mut app, &ctx, size);
            let controls = ctx.data(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("actual-main-dock", 4_usize)))
                    .unwrap()
            });
            enable(&mut app, &ctx, true);
            settle(&mut app, &ctx, size);
            let enabled_controls = ctx.data(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("actual-main-dock", 4_usize)))
                    .unwrap()
            });
            assert_eq!(
                controls, enabled_controls,
                "Notes must not steal Controls space {locale}/{scale}"
            );
            assert!(
                probe(&ctx, "actual-notes-dock").is_some_and(|r| r.is_finite() && r.is_positive())
            );
            let state = Arc::new(streaming::test_support::State::default());
            app.streaming =
                streaming::Runtime::with_factory(streaming::test_support::factory(state.clone()));
            streaming::test_support::configure(&mut app);
            app.intent(Intent::StartStreaming, &ctx);
            state.ready();
            settle(&mut app, &ctx, size);
            app.after_io = Some(AfterIo::ReplaceDocument);
            let output = frame(&mut app, &ctx, size, vec![]);
            let caption = crate::i18n::tr_context(&ctx, "Stop Streaming");
            let stop = output
                .shapes
                .iter()
                .find_map(|shape| {
                    point(&shape.shape, &caption).filter(|p| shape.clip_rect.contains(*p))
                })
                .unwrap_or_else(|| panic!("reachable Stop {locale}/{scale}"));
            click_at(&mut app, &ctx, size, stop);
            assert!(state.stops.load(Ordering::SeqCst) > 0);
            state.closed.store(true, Ordering::SeqCst);
            app.poll_streaming(&ctx);
            app.after_io = None;
        }
    }
}

#[test]
fn scene_notes_dock_preserves_source11_five_button_filters_route_and_neural_default_policy() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    let scene = app.session.preview();
    app.apply(Command::AddColorSource {
        scene,
        name: "Source11 video fixture".into(),
        rgba: [30, 40, 50, 255],
    });
    let snapshot = app.session.snapshot();
    let item = snapshot.scene(scene).unwrap().items()[0].id();
    let source = snapshot.scene(scene).unwrap().items()[0].source();
    app.selected = Some(item);
    let size = egui::vec2(1280.0, 800.0);
    settle(&mut app, &ctx, size);
    let probes = |ctx: &egui::Context| {
        ctx.data(|data| {
            data.get_temp::<Vec<views::dock_toolbar::Probe>>(egui::Id::new("dock-toolbar-probes"))
        })
        .unwrap()
        .into_iter()
        .filter(|probe| probe.scope == "Sources")
        .collect::<Vec<_>>()
    };
    let before = probes(&ctx);
    assert_eq!(
        before
            .iter()
            .map(|probe| probe.action.as_str())
            .collect::<Vec<_>>(),
        ["Add", "Remove", "Properties", "Filters", "Source Actions"]
    );
    enable(&mut app, &ctx, true);
    settle(&mut app, &ctx, size);
    let after = probes(&ctx);
    assert_eq!(before.len(), after.len());
    for (before, after) in before.iter().zip(&after) {
        assert_eq!(before.action, after.action);
        assert_eq!(
            before.rect, after.rect,
            "Notes must not move source controls"
        );
    }
    let providers = skylive_core::plugin_catalog();
    assert_eq!(providers.len(), 145);
    assert_eq!(
        providers
            .iter()
            .filter(|provider| provider.implemented)
            .count(),
        6
    );
    assert_eq!(
        providers
            .iter()
            .filter(|provider| !provider.implemented)
            .count(),
        139
    );
    let snapshot = app.session.snapshot();
    assert!(
        snapshot
            .preferences()
            .plugin_enabled(skylive_core::CRAFT_VIDEO_PLUGIN)
    );
    assert!(
        !snapshot
            .preferences()
            .plugin_enabled(skylive_core::PERSON_BACKGROUND_PLUGIN)
    );
    assert!(snapshot.preferences().plugin_enabled(SCENE_NOTES_PLUGIN));
    assert!(snapshot.source(source).unwrap().video_filters().is_empty());
    let filters = after
        .iter()
        .find(|probe| probe.action == "Filters")
        .unwrap();
    click_at(&mut app, &ctx, size, filters.rect.center());
    assert!(matches!(&app.dialog, Some(Dialog::VideoFilters(_))));
    assert!(
        Arc::ptr_eq(&snapshot, &app.session.snapshot()),
        "opening Filters cannot manufacture filters or mutate activation"
    );
}
