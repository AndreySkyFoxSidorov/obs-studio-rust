use super::*;
use crate::edit_clipboard::{Action, Request};
use skylive_core::{VideoEffect, VideoFilter};
fn assert_undo_content(
    before: &skylive_core::StudioSnapshot,
    pasted: &skylive_core::StudioSnapshot,
    actual: &skylive_core::StudioSnapshot,
) {
    let mut expected = serde_json::to_value(before.collection()).unwrap();
    expected["next_id"] = serde_json::to_value(pasted.collection()).unwrap()["next_id"].clone();
    assert_eq!(serde_json::to_value(actual.collection()).unwrap(), expected);
}
fn fixture() -> (egui::Context, StudioApp, Id, Id) {
    let context = egui::Context::default();
    let mut app = StudioApp::headless(&context).unwrap();
    let scene = app.session.preview();
    let mut add = |name: &str| {
        app.session
            .apply(Command::AddColorSourceSized {
                scene,
                name: name.into(),
                rgba: [255; 4],
                width: 16,
                height: 16,
            })
            .unwrap()
            .created
            .unwrap()
    };
    let a = add("Clipboard A");
    let b = add("Clipboard B");
    app.selected = Some(a);
    (context, app, a, b)
}
fn request(app: &StudioApp, id: &str) -> Request {
    let Some(Intent::Clipboard(request)) = views::menus::action_intent_with_clipboard(
        id,
        &app.session,
        app.selected,
        app.studio,
        &app.edit_clipboard,
    ) else {
        panic!("available {id}")
    };
    request
}
fn paint(
    app: &mut StudioApp,
    context: &egui::Context,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut intents = vec![];
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280., 800.),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::TopBottomPanel::top("clipboard-real-main-menu").show(ctx, |ui| {
                app.catalog.show_with_clipboard(
                    ui,
                    views::menus::MenuContext {
                        session: &app.session,
                        selected: app.selected,
                        layout: &app.layout,
                        studio: app.studio,
                        clipboard: &app.edit_clipboard,
                        source_clipboard: None,
                    },
                    &mut intents,
                )
            });
        },
    );
    for intent in intents {
        app.intent(intent, context);
    }
    output
}
fn text(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
    match shape {
        egui::epaint::Shape::Text(t) if t.galley.text() == label => {
            Some(egui::Rect::from_min_size(t.pos, t.galley.size()))
        }
        egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|s| text(s, label)),
        _ => None,
    }
}
fn click(app: &mut StudioApp, context: &egui::Context, label: &str) {
    paint(app, context, vec![]);
    let output = paint(app, context, vec![]);
    let pos = output
        .shapes
        .iter()
        .find_map(|s| text(&s.shape, label).filter(|r| s.clip_rect.contains_rect(*r)))
        .unwrap_or_else(|| panic!("visible {label}"))
        .center();
    for pressed in [true, false] {
        paint(
            app,
            context,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}
#[test]
fn real_edit_menu_buttons_copy_current_filters_paste_into_target_and_one_undo_restores() {
    let (context, mut app, a, b) = fixture();
    let from = app.session.snapshot().item_container(a).unwrap().1.source();
    let to = app.session.snapshot().item_container(b).unwrap().1.source();
    app.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![
            VideoFilter::new(VideoEffect::color_correction())
                .unwrap()
                .with_name("Visible copied filter".into())
                .unwrap(),
        ],
    });
    click(&mut app, &context, "Edit");
    click(&mut app, &context, "Copy Filters");
    app.selected = Some(b);
    let before = app.session.snapshot();
    click(&mut app, &context, "Edit");
    click(&mut app, &context, "Paste Filters");
    assert_eq!(
        app.session.snapshot().source(to).unwrap().video_filters()[0].name(),
        "Visible copied filter"
    );
    assert_eq!(app.session.snapshot().revision(), before.revision() + 1);
    let after = app.session.snapshot();
    app.apply(Command::Undo);
    assert_undo_content(&before, &after, &app.session.snapshot());
}
#[test]
fn menu_requests_are_revision_and_clipboard_fenced_and_locked_transform_is_unavailable() {
    let (context, mut app, a, b) = fixture();
    let copy = request(&app, "actionCopyTransform");
    app.intent(Intent::Clipboard(copy), &context);
    app.selected = Some(b);
    let paste = request(&app, "actionPasteTransform");
    app.apply(Command::SetLocked {
        scene: app.session.preview(),
        item: b,
        locked: true,
    });
    let before = app.session.snapshot();
    app.intent(Intent::Clipboard(paste), &context);
    assert!(app.error);
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    assert!(
        app.edit_clipboard
            .request(Action::PasteTransform, &app.session, Some(b))
            .is_err()
    );
    app.selected = Some(a);
    let copy = request(&app, "actionCopyFilters");
    app.intent(Intent::Clipboard(copy), &context);
    assert!(
        app.edit_clipboard
            .request(Action::PasteFilters, &app.session, Some(a))
            .is_ok()
    );
    app.apply(Command::RemoveItem {
        scene: app.session.preview(),
        item: a,
    });
    app.apply(Command::Undo);
    assert!(
        app.edit_clipboard
            .request(Action::PasteFilters, &app.session, Some(a))
            .is_err(),
        "publication invalidation survives Undo"
    );
}
#[test]
fn default_transform_chords_respect_configured_overrides_and_never_repeat_copy() {
    let (context, mut app, a, b) = fixture();
    let scene = app.session.preview();
    let mut t = skylive_core::Transform::full(16, 16);
    t.x = 123.;
    app.apply(Command::SetTransform {
        scene,
        item: a,
        transform: t,
    });
    let key = |key, repeat| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat,
        modifiers: egui::Modifiers {
            ctrl: true,
            command: true,
            shift: true,
            ..Default::default()
        },
    };
    let _ = context.run(
        egui::RawInput {
            events: vec![key(egui::Key::C, false)],
            ..Default::default()
        },
        |ctx| app.frame_at(ctx, MediaTime(0)),
    );
    app.selected = Some(b);
    let _ = context.run(
        egui::RawInput {
            events: vec![key(egui::Key::V, false)],
            ..Default::default()
        },
        |ctx| app.frame_at(ctx, MediaTime(0)),
    );
    assert_eq!(
        app.session
            .snapshot()
            .item_container(b)
            .unwrap()
            .1
            .transform()
            .x,
        123.
    );
    let old = app
        .edit_clipboard
        .request(Action::PasteTransform, &app.session, Some(b))
        .unwrap();
    let _ = context.run(
        egui::RawInput {
            events: vec![key(egui::Key::C, true)],
            ..Default::default()
        },
        |ctx| app.frame_at(ctx, MediaTime(0)),
    );
    assert!(
        app.edit_clipboard
            .execute(old, &app.session, Some(b))
            .is_ok(),
        "repeat must not replace clipboard generation"
    );
    app.apply(Command::SetHotkeys {
        hotkeys: skylive_core::HotkeyBindings::new(vec![skylive_core::HotkeyBinding {
            action: skylive_core::HotkeyAction::Undo,
            chord: skylive_core::HotkeyChord::new("C", true, false, true).unwrap(),
        }])
        .unwrap(),
    });
    assert!(crate::edit_clipboard_shortcuts::label(Action::CopyTransform, &app.session).is_empty());
    let before = app.session.snapshot().revision();
    let _ = context.run(
        egui::RawInput {
            events: vec![key(egui::Key::C, false)],
            ..Default::default()
        },
        |ctx| app.frame_at(ctx, MediaTime(0)),
    );
    assert!(
        app.session.snapshot().revision() > before,
        "configured Undo owns this chord"
    );
}

#[test]
fn source11_real_menu_disables_nn_paste_until_provider_is_reenabled_even_when_filter_is_bypassed() {
    let (context, mut app, a, b) = fixture();
    let from = app.session.snapshot().item_container(a).unwrap().1.source();
    let to = app.session.snapshot().item_container(b).unwrap().1.source();
    app.apply(Command::SetPluginEnabled {
        plugin: skylive_core::PERSON_BACKGROUND_PLUGIN.into(),
        enabled: true,
    });
    let mut neural = VideoFilter::new(VideoEffect::PersonBackgroundRemoval {
        threshold: 0.67,
        softness: 0.29,
    })
    .unwrap();
    neural.enabled = false;
    app.apply(Command::SetSourceVideoFilters {
        source: from,
        filters: vec![neural],
    });
    click(&mut app, &context, "Edit");
    click(&mut app, &context, "Copy Filters");
    app.apply(Command::SetPluginEnabled {
        plugin: skylive_core::PERSON_BACKGROUND_PLUGIN.into(),
        enabled: false,
    });
    app.selected = Some(b);
    assert!(
        views::menus::action_intent_with_clipboard(
            "actionPasteFilters",
            &app.session,
            app.selected,
            app.studio,
            &app.edit_clipboard
        )
        .is_none()
    );
    assert!(matches!(
        app.edit_clipboard
            .request(Action::PasteFilters, &app.session, app.selected),
        Err(crate::edit_clipboard::Error::Core(
            skylive_core::Error::Unavailable(_)
        ))
    ));
    assert!(
        app.session
            .snapshot()
            .source(to)
            .unwrap()
            .video_filters()
            .is_empty()
    );
    app.apply(Command::SetPluginEnabled {
        plugin: skylive_core::PERSON_BACKGROUND_PLUGIN.into(),
        enabled: true,
    });
    let before = app.session.snapshot();
    click(&mut app, &context, "Edit");
    click(&mut app, &context, "Paste Filters");
    let after = app.session.snapshot();
    let copied = &after.source(to).unwrap().video_filters()[0];
    assert!(!copied.enabled);
    assert_eq!(
        copied.effect,
        VideoEffect::PersonBackgroundRemoval {
            threshold: 0.67,
            softness: 0.29
        }
    );
    assert_eq!(after.revision(), before.revision() + 1);
    app.apply(Command::Undo);
    assert_undo_content(&before, &after, &app.session.snapshot());
}
