use super::*;
use skylive_host::localization::Localization;
use std::{path::Path, sync::Arc};

fn painted(shape: &egui::epaint::Shape, expected: &str) -> bool {
    match shape {
        egui::epaint::Shape::Text(text) => text.galley.text() == expected,
        egui::epaint::Shape::Vec(shapes) => shapes.iter().any(|shape| painted(shape, expected)),
        _ => false,
    }
}
fn paint(context: &egui::Context, presentation: &Presentation, raw: &str, expected: &str) {
    let output = context.run(Default::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(presentation.text(ctx, raw));
        });
    });
    assert!(
        output
            .shapes
            .iter()
            .any(|shape| painted(&shape.shape, expected)),
        "expected painted {expected:?}"
    );
}

#[test]
fn published_revision_repaints_after_catalog_switch_in_five_languages() {
    let context = egui::Context::default();
    let (raw, presentation) = Status::formatted("Revision {}", &[("{}", "1".into())]).into_parts();
    assert_eq!(raw, "Revision 1");
    for (language, word) in [
        ("en-US", "Revision"),
        ("es-ES", "Revisión"),
        ("ru-RU", "Ревизия"),
        ("uk-UA", "Ревізія"),
        ("de-DE", "Revision"),
    ] {
        // A deterministic public CSV fixture proves deferred selection independently
        // of translators changing the repository's current wording.
        let csv = format!("key,en-US,{language}\nApp.Revision,Revision {{}},{word} {{}}\n");
        let csv = if language == "en-US" {
            "key,en-US\nApp.Revision,Revision {}\n".to_owned()
        } else {
            csv
        };
        crate::i18n::install(
            &context,
            Arc::new(Localization::from_csv(csv.as_bytes(), language).unwrap()),
            Path::new("fixture.csv"),
        );
        paint(&context, &presentation, &raw, &format!("{word} 1"));
        assert_eq!(raw, "Revision 1");
    }
}

#[test]
fn opaque_paths_errors_names_and_braces_are_never_translated_or_reinterpreted() {
    let context = egui::Context::default();
    let csv = "key,en-US,ru-RU\nApp.Saved,Recording saved: {path},Запись сохранена: {path}\nSettings,Settings,Настройки\n";
    crate::i18n::install(
        &context,
        Arc::new(Localization::from_csv(csv.as_bytes(), "ru-RU").unwrap()),
        Path::new("fixture.csv"),
    );
    let path = r"\\?\C:\Settings\Юнікод {path} {{}}.mp4";
    let (raw, presentation) =
        Status::formatted("Recording saved: {path}", &[("{path}", path.into())]).into_parts();
    assert_eq!(raw, format!("Recording saved: {path}"));
    paint(
        &context,
        &presentation,
        &raw,
        &format!("Запись сохранена: {path}"),
    );
    let (raw, presentation) = Status::from("Settings".to_owned()).into_parts();
    paint(&context, &presentation, &raw, "Settings");
    let (raw, presentation) = Status::from("Settings").into_parts();
    paint(&context, &presentation, &raw, "Настройки");
    // A direct external replacement cannot reuse unrelated old chrome metadata.
    paint(
        &context,
        &presentation,
        "Settings {author}",
        "Settings {author}",
    );
}

#[test]
fn over_budget_metadata_keeps_complete_raw_values() {
    let argument = "Ж{error}".repeat(5000);
    let (raw, presentation) = Status::formatted(
        "Streaming failed: {error}",
        &[("{error}", argument.clone())],
    )
    .into_parts();
    assert_eq!(raw, format!("Streaming failed: {argument}"));
    assert!(matches!(presentation, Presentation::Raw));
    assert_eq!(
        render_raw("{{{value}}}", &[("{value}", "{kept}".into())]),
        "{{kept}}"
    );
}

#[test]
fn supplied_obs_transition_cut_uses_contextual_key_not_edit_alias() {
    let context = egui::Context::default();
    crate::i18n::install(
        &context,
        Arc::new(Localization::embedded("ru-RU").unwrap()),
        Path::new("embedded.csv"),
    );
    let output = context.run(Default::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(crate::views::transition_kind_label(
                ui,
                skylive_core::TransitionKind::Cut,
            ));
        });
    });
    assert!(
        output
            .shapes
            .iter()
            .any(|shape| painted(&shape.shape, "Обрезка"))
    );
    let (raw, presentation) = Status::from("Cut".to_owned()).into_parts();
    paint(&context, &presentation, &raw, "Cut");
}
