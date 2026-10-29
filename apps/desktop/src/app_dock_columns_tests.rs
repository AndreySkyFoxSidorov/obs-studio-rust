use super::*;
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
fn point(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
    match shape {
        egui::epaint::Shape::Text(t) if t.galley.text() == label => {
            Some(t.pos + t.galley.size() * 0.5)
        }
        egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| point(s, label)),
        _ => None,
    }
}
fn click_at(app: &mut StudioApp, ctx: &egui::Context, size: egui::Vec2, p: egui::Pos2) {
    for pressed in [true, false] {
        let _ = frame(
            app,
            ctx,
            size,
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}
fn click(app: &mut StudioApp, ctx: &egui::Context, size: egui::Vec2, english: &str) {
    let _ = frame(app, ctx, size, vec![]);
    let output = frame(app, ctx, size, vec![]);
    let label = crate::i18n::tr_context(ctx, english);
    let p = output
        .shapes
        .iter()
        .find_map(|s| point(&s.shape, &label).filter(|p| s.clip_rect.contains(*p)))
        .unwrap_or_else(|| panic!("visible actual dock control {label}"));
    click_at(app, ctx, size, p);
}
fn fixture(ctx: &egui::Context) -> StudioApp {
    let mut app = StudioApp::headless(ctx).unwrap();
    app.layout.arrangement = views::dock_layout::DockLayout::bottom_row();
    app.apply(Command::SetVideo {
        width: 64,
        height: 48,
        fps_num: 30,
        fps_den: 1,
    });
    app.apply(Command::AddColorSource {
        scene: app.session.preview(),
        name: "Readable source selection".into(),
        rgba: [60, 100, 200, 255],
    });
    app.selected = None;
    app
}

#[test]
fn actual_main_window_resize_changes_only_presentation_and_reset_menu_restores_all_visibility() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    let size = egui::vec2(1280.0, 800.0);
    let _ = frame(&mut app, &ctx, size, vec![]);
    let _ = frame(&mut app, &ctx, size, vec![]);
    let before = app.session.snapshot();
    let document = app.session.document_revision();
    let can_undo = app.session.can_undo();
    let rect = ctx.data(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new(("dock-splitter-rect", 1_usize, 2_usize)))
            .unwrap()
    });
    let p = rect.center();
    let q = p + egui::vec2(50.0, 0.0);
    let _ = frame(
        &mut app,
        &ctx,
        size,
        vec![
            egui::Event::PointerMoved(p),
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerMoved(q),
            egui::Event::PointerButton {
                pos: q,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    assert_ne!(
        app.layout.columns.weights(),
        views::dock_columns::DockColumns::default().weights()
    );
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    assert_eq!(app.session.document_revision(), document);
    assert_eq!(app.session.can_undo(), can_undo);
    app.intent(Intent::ToggleDock(views::Dock::Scenes), &ctx);
    assert!(!app.layout.scenes);
    click(&mut app, &ctx, size, "Docks");
    click(&mut app, &ctx, size, "Reset Layout");
    assert!(
        app.layout.scenes
            && app.layout.sources
            && app.layout.mixer
            && app.layout.transitions
            && app.layout.controls
    );
    assert_eq!(
        app.layout.columns.weights(),
        views::dock_columns::DockColumns::default().weights()
    );
    assert_eq!(app.layout.columns.selected(), 1);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
}

#[test]
fn compact_sources_mixer_keyboard_and_first_stop_remain_real_at_minimum_scales_in_five_locales() {
    use std::sync::atomic::Ordering;
    for locale in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        app.localization =
            Arc::new(skylive_host::localization::Localization::embedded(locale).unwrap());
        crate::i18n::install(&ctx, app.localization.clone(), &app.localization_path);
        // A configured global arrow binding would undo the source if focused
        // dock navigation leaked into the app's shortcut collector.
        app.apply(Command::SetHotkeys {
            hotkeys: skylive_core::HotkeyBindings::new(vec![skylive_core::HotkeyBinding {
                action: skylive_core::HotkeyAction::Undo,
                chord: skylive_core::HotkeyChord::new("ArrowRight", false, false, false).unwrap(),
            }])
            .unwrap(),
        });
        for (physical, scale) in [
            (egui::vec2(800.0, 560.0), 150_u16),
            (egui::vec2(800.0, 560.0), 200),
            (egui::vec2(1280.0, 800.0), 200),
        ] {
            app.layout.reset();
            app.apply(Command::SetAppearance {
                appearance: skylive_core::Appearance::new(
                    skylive_core::InterfaceTheme::Yami,
                    scale,
                )
                .unwrap(),
            });
            let size = physical / (f32::from(scale) / 100.0);
            let _ = frame(&mut app, &ctx, size, vec![]);
            let _ = frame(&mut app, &ctx, size, vec![]);
            let _ = frame(&mut app, &ctx, size, vec![]);
            let before = app.session.snapshot();
            let selected = before.scene(app.session.preview()).unwrap().items()[0].id();
            let output = frame(&mut app, &ctx, size, vec![]);
            assert!(
                output
                    .shapes
                    .iter()
                    .any(|s| point(&s.shape, "Readable source selection")
                        .is_some_and(|p| s.clip_rect.contains(p))),
                "source selection must paint in compact mode: {locale} {scale}"
            );
            let source_point = output
                .shapes
                .iter()
                .find_map(|shape| {
                    point(&shape.shape, "Readable source selection")
                        .filter(|point| shape.clip_rect.contains(*point))
                })
                .unwrap();
            assert!(ctx.screen_rect().contains(source_point));
            click_at(&mut app, &ctx, size, source_point);
            assert_eq!(app.selected, Some(selected));
            let tab = ctx.data(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("compact-dock-tab", 1_usize)))
                    .unwrap()
            });
            assert!(ctx.screen_rect().contains(tab.center()));
            click_at(&mut app, &ctx, size, tab.center());
            let _ = frame(
                &mut app,
                &ctx,
                size,
                vec![egui::Event::Key {
                    key: egui::Key::ArrowRight,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert_eq!(
                app.layout.columns.selected(),
                2,
                "keyboard switches to the real mixer"
            );
            let output = frame(
                &mut app,
                &ctx,
                size,
                vec![egui::Event::Key {
                    key: egui::Key::ArrowRight,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            let label = crate::i18n::tr_context(&ctx, "Program Audio");
            assert!(
                output
                    .shapes
                    .iter()
                    .any(|s| point(&s.shape, &label).is_some_and(|p| s.clip_rect.contains(p))),
                "actual mixer menu visible {locale} {scale}"
            );
            assert!(
                Arc::ptr_eq(&before, &app.session.snapshot()),
                "focused tab key must not execute scene Undo"
            );

            let state = Arc::new(streaming::test_support::State::default());
            app.streaming =
                streaming::Runtime::with_factory(streaming::test_support::factory(state.clone()));
            streaming::test_support::configure(&mut app);
            app.intent(Intent::StartStreaming, &ctx);
            state.ready();
            let _ = frame(&mut app, &ctx, size, vec![]);
            let _ = frame(&mut app, &ctx, size, vec![]);
            app.after_io = Some(AfterIo::ReplaceDocument); // presentation authoring fence; no fake I/O job is claimed
            let output = frame(&mut app, &ctx, size, vec![]);
            let label = crate::i18n::tr_context(&ctx, "Stop Streaming");
            let p = output
                .shapes
                .iter()
                .find_map(|s| point(&s.shape, &label).filter(|p| s.clip_rect.contains(*p)))
                .unwrap_or_else(|| {
                    panic!("first Stop remains visible with editing disabled: {locale} {scale}")
                });
            click_at(&mut app, &ctx, size, p);
            assert!(state.stops.load(Ordering::SeqCst) > 0);
            state.closed.store(true, Ordering::SeqCst);
            app.poll_streaming(&ctx);
            app.after_io = None;
        }
    }
}
