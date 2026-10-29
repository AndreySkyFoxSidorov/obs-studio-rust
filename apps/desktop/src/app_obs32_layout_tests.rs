use super::*;
use views::dock_layout::{DockPreset, DockSize};

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
        let _ = frame(app, ctx, size, vec![]);
    }
}
fn rect(ctx: &egui::Context, index: usize) -> egui::Rect {
    ctx.data(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new(("actual-main-dock", index)))
            .unwrap()
    })
}
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
fn point(shape: &egui::epaint::Shape, text: &str) -> Option<egui::Pos2> {
    match shape {
        egui::epaint::Shape::Text(t) if t.galley.text() == text => {
            Some(t.pos + t.galley.size() * 0.5)
        }
        egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| point(s, text)),
        _ => None,
    }
}
fn click(app: &mut StudioApp, ctx: &egui::Context, size: egui::Vec2, text: &str) {
    settle(app, ctx, size);
    let output = frame(app, ctx, size, vec![]);
    let target = output
        .shapes
        .iter()
        .find_map(|s| point(&s.shape, text).filter(|p| s.clip_rect.contains(*p)))
        .unwrap_or_else(|| panic!("painted reachable menu: {text}"));
    for pressed in [true, false] {
        let _ = frame(
            app,
            ctx,
            size,
            vec![
                egui::Event::PointerMoved(target),
                egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}

#[test]
fn source32_default_has_left_vertical_scenes_sources_and_three_bottom_docks_in_five_locales() {
    for locale in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
        let ctx = egui::Context::default();
        let mut app = fixture(&ctx);
        app.localization =
            Arc::new(skylive_host::localization::Localization::embedded(locale).unwrap());
        crate::i18n::install(&ctx, app.localization.clone(), &app.localization_path);
        assert_eq!(app.layout.arrangement.preset, DockPreset::Obs);
        settle(&mut app, &ctx, egui::vec2(1280.0, 800.0));
        let scenes = rect(&ctx, 0);
        let sources = rect(&ctx, 1);
        let mixer = rect(&ctx, 2);
        let transitions = rect(&ctx, 3);
        let controls = rect(&ctx, 4);
        assert!(scenes.bottom() <= sources.top());
        assert!((scenes.left() - sources.left()).abs() < 1.0);
        assert!(sources.right() < mixer.left());
        assert!(sources.bottom() >= mixer.bottom() - 12.0);
        assert!(mixer.right() < transitions.left() && transitions.right() < controls.left());
        for rectangle in [scenes, sources, mixer, transitions, controls] {
            assert!(
                rectangle.is_finite() && rectangle.width() >= 80.0 && rectangle.height() >= 100.0
            );
        }
    }
}

#[test]
fn actual_layout_menu_switch_and_full_height_are_presentation_only_and_reset_restores_default() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    let size = egui::vec2(1280.0, 800.0);
    let before = app.session.snapshot();
    let revision = app.session.document_revision();
    let undo = app.session.can_undo();
    click(&mut app, &ctx, size, "Docks");
    click(&mut app, &ctx, size, "Layout");
    click(&mut app, &ctx, size, "Bottom row");
    settle(&mut app, &ctx, size);
    assert_eq!(app.layout.arrangement.preset, DockPreset::BottomRow);
    for index in 0..4 {
        assert!(rect(&ctx, index).right() < rect(&ctx, index + 1).left());
    }
    app.intent(Intent::SetDockPreset(DockPreset::Obs), &ctx);
    app.intent(Intent::SetFullHeightDocks(false), &ctx);
    settle(&mut app, &ctx, size);
    assert!(rect(&ctx, 1).bottom() < rect(&ctx, 2).top());
    app.intent(Intent::SetDockSize(DockSize::SideWidth, 240), &ctx);
    app.intent(Intent::ToggleDock(views::Dock::Scenes), &ctx);
    app.intent(Intent::SetDocksLocked(true), &ctx);
    app.intent(Intent::ResetLayout, &ctx);
    settle(&mut app, &ctx, size);
    assert_eq!(app.layout.arrangement.preset, DockPreset::Obs);
    assert!(
        app.layout.arrangement.full_height && !app.layout.arrangement.locked && app.layout.scenes
    );
    assert!(rect(&ctx, 1).bottom() >= rect(&ctx, 2).bottom() - 12.0);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    assert_eq!(app.session.document_revision(), revision);
    assert_eq!(app.session.can_undo(), undo);
}

#[test]
fn lock_rejects_column_resize_and_size_intent_without_disabling_real_docks() {
    let ctx = egui::Context::default();
    let mut app = fixture(&ctx);
    let size = egui::vec2(1280.0, 800.0);
    app.intent(Intent::SetDockPreset(DockPreset::BottomRow), &ctx);
    settle(&mut app, &ctx, size);
    app.intent(Intent::SetDocksLocked(true), &ctx);
    settle(&mut app, &ctx, size);
    let before = rect(&ctx, 4);
    let weights = app.layout.columns.weights();
    let split = ctx.data(|data| {
        data.get_temp::<egui::Rect>(egui::Id::new(("dock-splitter-rect", 1_usize, 2_usize)))
            .unwrap()
            .center()
    });
    for (position, pressed) in [
        (split, true),
        (split + egui::vec2(50.0, 0.0), true),
        (split + egui::vec2(50.0, 0.0), false),
    ] {
        let _ = frame(
            &mut app,
            &ctx,
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
    app.intent(Intent::SetDockSize(DockSize::BottomHeight, 500), &ctx);
    settle(&mut app, &ctx, size);
    assert_eq!(app.layout.columns.weights(), weights);
    assert_eq!(rect(&ctx, 4), before);
    let output = frame(&mut app, &ctx, size, vec![]);
    assert!(
        output
            .shapes
            .iter()
            .any(|s| point(&s.shape, "Settings").is_some()),
        "Lock affects geometry, not Controls"
    );
}
