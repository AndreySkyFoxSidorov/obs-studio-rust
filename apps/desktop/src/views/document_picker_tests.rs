use super::*;
use crate::views::document::{self, Prompt};
use std::sync::Arc;

fn catalog(locale: &str) -> Arc<skylive_host::localization::Localization> {
    let seeds: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../localization/obs-file-ui-seeds.json"
    ))
    .unwrap();
    fn cell(text: &str) -> String {
        format!("\"{}\"", text.replace('"', "\"\""))
    }
    let mut csv = "key,en-US,es-ES,ru-RU,uk-UA,de-DE\n".to_owned();
    for (index, (english, values)) in seeds.as_object().unwrap().iter().enumerate() {
        csv.push_str(&format!("FileUiTest.{index},{}", cell(english)));
        for lang in ["es-ES", "ru-RU", "uk-UA", "de-DE"] {
            csv.push(',');
            csv.push_str(&cell(values[lang].as_str().unwrap()));
        }
        csv.push('\n');
    }
    Arc::new(skylive_host::localization::Localization::from_csv(csv.as_bytes(), locale).unwrap())
}
fn text<'a>(shape: &'a egui::epaint::Shape, label: &str) -> Option<&'a egui::epaint::TextShape> {
    match shape {
        egui::epaint::Shape::Text(t) if t.galley.text() == label => Some(t),
        egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| text(s, label)),
        _ => None,
    }
}
fn render(
    ctx: &egui::Context,
    prompt: &mut Prompt,
    size: egui::Vec2,
    events: Vec<egui::Event>,
    intents: &mut Vec<Intent>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |_| {});
            if let Some(intent) = document::show(ctx, prompt, false) {
                intents.push(intent);
            }
        },
    )
}
fn click(
    ctx: &egui::Context,
    prompt: &mut Prompt,
    size: egui::Vec2,
    caption: &str,
    intents: &mut Vec<Intent>,
) {
    let _ = render(ctx, prompt, size, vec![], intents);
    let output = render(ctx, prompt, size, vec![], intents);
    let label = crate::i18n::tr_context(ctx, caption);
    let p = output
        .shapes
        .iter()
        .find_map(|s| {
            text(&s.shape, &label)
                .map(|t| t.pos + t.galley.size() * 0.5)
                .filter(|p| s.clip_rect.contains(*p))
        })
        .expect("real visible chooser control");
    for pressed in [true, false] {
        let _ = render(
            ctx,
            prompt,
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
            intents,
        );
    }
}

#[test]
fn chooser_controls_fit_and_cancel_confirms_no_path_in_five_locales_at_scale_boundaries() {
    for locale in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
        for scale in [1.0_f32, 1.5, 2.0] {
            for action in [
                DocumentAction::Open,
                DocumentAction::Recover,
                DocumentAction::ImportObs,
            ] {
                let ctx = egui::Context::default();
                crate::theme::install(&ctx);
                crate::i18n::install(
                    &ctx,
                    catalog(locale),
                    Path::new("unused-test-translations.csv"),
                );
                let size = egui::vec2(800.0 / scale, 560.0 / scale);
                let mut prompt = Prompt::Choose(PathDraft {
                    action,
                    path: "collection.json".into(),
                    session: 7,
                    revision: 11,
                });
                let mut intents = Vec::new();
                let _ = render(&ctx, &mut prompt, size, vec![], &mut intents);
                let output = render(&ctx, &mut prompt, size, vec![], &mut intents);
                for caption in [action.title(), action.action(), "Cancel", "File path"] {
                    let label = crate::i18n::tr_context(&ctx, caption);
                    let (paint, clip) = output
                        .shapes
                        .iter()
                        .find_map(|s| text(&s.shape, &label).map(|t| (t, s.clip_rect)))
                        .unwrap_or_else(|| panic!("{caption} {locale} {scale}"));
                    let bounds = egui::Rect::from_min_size(paint.pos, paint.galley.size());
                    assert!(
                        clip.contains_rect(bounds)
                            && egui::Rect::from_min_size(egui::Pos2::ZERO, size)
                                .contains_rect(bounds),
                        "{caption} {locale} {scale} bounds={bounds:?} clip={clip:?}"
                    );
                }
                click(&ctx, &mut prompt, size, "Cancel", &mut intents);
                assert!(matches!(intents.as_slice(), [Intent::CancelDocumentPath]));
                intents.clear();
                click(&ctx, &mut prompt, size, action.action(), &mut intents);
                assert!(
                    matches!(intents.as_slice(), [Intent::ConfirmDocumentPath(draft)]
                    if draft.action==action && draft.path=="collection.json" && draft.session==7 && draft.revision==11)
                );
            }
        }
    }
}

#[test]
fn exact_typed_path_is_staged_until_actual_confirm_and_empty_path_cannot_confirm() {
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    crate::i18n::install(&ctx, catalog("en-US"), Path::new("unused.csv"));
    let size = egui::vec2(800.0, 560.0);
    let mut prompt = Prompt::Choose(PathDraft {
        action: DocumentAction::Open,
        path: String::new(),
        session: 7,
        revision: 11,
    });
    let mut intents = Vec::new();
    click(&ctx, &mut prompt, size, "Open", &mut intents);
    assert!(intents.is_empty());
    if let Prompt::Choose(draft) = &mut prompt {
        draft.path = "original.json".into();
    }
    let output = render(&ctx, &mut prompt, size, vec![], &mut intents);
    let p = output
        .shapes
        .iter()
        .find_map(|s| text(&s.shape, "original.json").map(|t| t.pos + t.galley.size() * 0.5))
        .unwrap();
    for pressed in [true, false] {
        let _ = render(
            &ctx,
            &mut prompt,
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
            &mut intents,
        );
    }
    let modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    let exact = r"C:\Authored folder\collection.json";
    let _ = render(
        &ctx,
        &mut prompt,
        size,
        vec![
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: Some(egui::Key::A),
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Text(exact.into()),
        ],
        &mut intents,
    );
    assert!(intents.is_empty());
    assert!(matches!(&prompt,Prompt::Choose(d)if d.path==exact));
    click(&ctx, &mut prompt, size, "Open", &mut intents);
    assert!(
        matches!(intents.as_slice(),[Intent::ConfirmDocumentPath(d)]if d.path==exact&&d.session==7&&d.revision==11)
    );
}

#[test]
fn pretty_windows_paths_never_mutate_actual_bindings() {
    for (input, pretty) in [
        (r"\\?\C:\Folder\file.json", r"C:\Folder\file.json"),
        (
            r"\\?\UNC\server\share\file.json",
            r"\\server\share\file.json",
        ),
        (r"\\?\Volume{123}\file.json", r"\\?\Volume{123}\file.json"),
    ] {
        let path = std::path::PathBuf::from(input);
        let before = path.clone();
        assert_eq!(display_path(&path), pretty);
        assert_eq!(path, before);
    }
}
