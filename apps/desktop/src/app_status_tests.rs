use super::*;

fn frame(app: &mut StudioApp, context: &egui::Context) -> egui::FullOutput {
    context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        },
        |ctx| app.frame_at(ctx, MediaTime(0)),
    )
}
fn painted(shape: &egui::epaint::Shape, label: &str) -> bool {
    match shape {
        egui::epaint::Shape::Text(text) => text.galley.text() == label,
        egui::epaint::Shape::Vec(shapes) => shapes.iter().any(|shape| painted(shape, label)),
        _ => false,
    }
}
#[test]
fn real_bottom_status_repaints_revision_using_new_catalog_without_mutating_raw_receipt() {
    let context = egui::Context::default();
    let mut app = StudioApp::headless(&context).unwrap();
    app.apply(Command::SetVideo {
        width: 64,
        height: 48,
        fps_num: 30,
        fps_den: 1,
    });
    let raw = format!("Revision {}", app.session.snapshot().revision());
    assert_eq!(app.status, raw);
    for language in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
        app.localization =
            Arc::new(skylive_host::localization::Localization::embedded(language).unwrap());
        crate::i18n::install(&context, app.localization.clone(), &app.localization_path);
        let expected = crate::i18n::tf_context(
            &context,
            "Revision {}",
            &[("{}", app.session.snapshot().revision().to_string())],
        );
        if language == "ru-RU" {
            assert_ne!(expected, raw);
        }
        let _ = frame(&mut app, &context);
        let output = frame(&mut app, &context);
        assert!(
            output
                .shapes
                .iter()
                .any(|shape| painted(&shape.shape, &expected)),
            "actual bottom status {language}: {expected}"
        );
        assert_eq!(app.status, raw);
    }
}

#[test]
fn real_status_uses_translated_prefix_and_exact_opaque_error_even_when_it_contains_chrome() {
    let context = egui::Context::default();
    let mut app = StudioApp::headless(&context).unwrap();
    app.localization =
        Arc::new(skylive_host::localization::Localization::embedded("ru-RU").unwrap());
    crate::i18n::install(&context, app.localization.clone(), &app.localization_path);
    let detail = "Settings {error} / Юнікод / C:\\Secrets\\name.mp4";
    app.message(
        true,
        crate::status::Status::formatted(
            "Streaming failed: {error}",
            &[("{error}", detail.into())],
        ),
    );
    let expected = crate::i18n::tf_context(
        &context,
        "Streaming failed: {error}",
        &[("{error}", detail.into())],
    );
    let _ = frame(&mut app, &context);
    let output = frame(&mut app, &context);
    assert!(
        output
            .shapes
            .iter()
            .any(|shape| painted(&shape.shape, &expected))
    );
    assert!(app.error);
    assert_eq!(app.status, format!("Streaming failed: {detail}"));
    assert!(expected.contains(detail));
    app.message(false, "Settings".to_owned());
    let output = frame(&mut app, &context);
    assert!(
        output
            .shapes
            .iter()
            .any(|shape| painted(&shape.shape, "Settings"))
    );
}
