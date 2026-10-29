// Include inside app_document::tests; reuses its run-owned Directory/current/blue/pump.
fn file_ui_frame(
    app: &mut StudioApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 560.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| app.frame_at(ctx, MediaTime(0)),
    )
}
fn file_ui_point(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
    match shape {
        egui::epaint::Shape::Text(t) if t.galley.text() == label => {
            Some(t.pos + t.galley.size() * 0.5)
        }
        egui::epaint::Shape::Vec(items) => items.iter().find_map(|s| file_ui_point(s, label)),
        _ => None,
    }
}
fn file_ui_click(app: &mut StudioApp, ctx: &egui::Context, english: &str) {
    let _ = file_ui_frame(app, ctx, vec![]);
    let output = file_ui_frame(app, ctx, vec![]);
    let label = crate::i18n::tr_context(ctx, english);
    let pos = output
        .shapes
        .iter()
        .find_map(|s| file_ui_point(&s.shape, &label).filter(|p| s.clip_rect.contains(*p)))
        .unwrap_or_else(|| panic!("real file control {label}"));
    for pressed in [true, false] {
        let _ = file_ui_frame(
            app,
            ctx,
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
fn file_menu_chooser_cancel_is_pure_and_confirm_uses_guarded_save_then_replace_binding() {
    let dir = Directory::new();
    let a = dir.0.join("a.json");
    let b = dir.0.join("b.json");
    blue(&b);
    let original_b = fs::read(&b).unwrap();
    let ctx = egui::Context::default();
    let mut app = current(&ctx);
    app.intent(
        Intent::SaveAsTo {
            path: a.to_string_lossy().into_owned(),
        },
        &ctx,
    );
    pump(&mut app, &ctx);
    app.apply(Command::AddScene {
        name: "Save this into A".into(),
    });
    let before = app.session.snapshot();
    let owner = app.session.identity();
    let binding = app.active_document_path.clone();
    let open_draft = app.document_path.clone();
    let output = file_ui_frame(&mut app, &ctx, vec![]);
    assert!(
        !output
            .shapes
            .iter()
            .any(|s| file_ui_point(&s.shape, "Open / Recover target").is_some())
    );
    file_ui_click(&mut app, &ctx, "File");
    file_ui_click(&mut app, &ctx, "Open…");
    assert!(!app.io.busy());
    assert!(matches!(app.documents.prompt, Some(Prompt::Choose(..))));
    if let Some(Prompt::Choose(draft)) = &mut app.documents.prompt {
        draft.path = b.to_string_lossy().into_owned();
    }
    file_ui_click(&mut app, &ctx, "Cancel");
    assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    assert_eq!(owner, app.session.identity());
    assert_eq!(app.active_document_path, binding);
    assert_eq!(app.document_path, open_draft);
    assert!(!app.io.busy());
    file_ui_click(&mut app, &ctx, "File");
    file_ui_click(&mut app, &ctx, "Open…");
    if let Some(Prompt::Choose(draft)) = &mut app.documents.prompt {
        draft.path = b.to_string_lossy().into_owned();
    }
    file_ui_click(&mut app, &ctx, "Open");
    pump(&mut app, &ctx);
    assert!(matches!(app.documents.prompt, Some(Prompt::Replace { .. })));
    assert_eq!(app.active_document_path, binding);
    assert_eq!(app.session.identity(), owner);
    file_ui_click(&mut app, &ctx, "Save and continue");
    pump(&mut app, &ctx);
    assert_eq!(
        app.active_document_path,
        Some(fs::canonicalize(&b).unwrap())
    );
    assert_ne!(app.session.identity(), owner);
    assert_eq!(color(&app), [0, 0, 255, 255]);
    assert_eq!(fs::read(&b).unwrap(), original_b);
    let saved = skylive_host::storage::load(&a).unwrap();
    assert!(
        saved
            .snapshot()
            .scenes()
            .iter()
            .any(|s| s.name() == "Save this into A")
    );
}

#[test]
fn stale_file_chooser_confirm_keeps_draft_and_current_document_without_io() {
    let ctx = egui::Context::default();
    let mut app = current(&ctx);
    file_ui_click(&mut app, &ctx, "File");
    file_ui_click(&mut app, &ctx, "Recover…");
    let draft = match &app.documents.prompt {
        Some(Prompt::Choose(draft)) => draft.clone(),
        _ => panic!("recover chooser"),
    };
    app.session
        .apply(Command::AddScene {
            name: "A newer owner revision".into(),
        })
        .unwrap();
    let current = app.session.snapshot();
    file_ui_click(&mut app, &ctx, "Recover");
    assert!(app.error);
    assert!(!app.io.busy());
    assert!(Arc::ptr_eq(&current, &app.session.snapshot()));
    assert!(matches!(&app.documents.prompt,Some(Prompt::Choose(current))if current==&draft));
    file_ui_click(&mut app, &ctx, "Cancel");
    assert!(app.documents.prompt.is_none());
}
