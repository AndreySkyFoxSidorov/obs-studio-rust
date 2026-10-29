use super::*;
use crate::views::{Intent, dialogs};
use eframe::egui;
use std::{collections::BTreeMap, path::Path, sync::Arc};

fn catalog(locale: &str) -> Arc<skylive_host::localization::Localization> {
    let mut rows: BTreeMap<String, BTreeMap<String, String>> = serde_json::from_str(include_str!(
        "../../../../localization/settings-workflow-seeds.json"
    ))
    .unwrap();
    for caption in [
        "General",
        "Appearance",
        "Stream",
        "Output",
        "Audio",
        "Video",
        "Hotkeys",
        "Accessibility",
        "Advanced",
        "Settings — General",
        "Settings — Appearance",
        "Settings — Stream",
        "Settings — Output",
        "Settings — Audio",
        "Settings — Video",
        "Settings — Hotkeys",
    ] {
        let values = ["es-ES", "ru-RU", "uk-UA", "de-DE"].map(|lang| {
            let embedded = skylive_host::localization::Localization::embedded(lang).unwrap();
            (lang.to_owned(), embedded.text(caption).into_owned())
        });
        rows.entry(caption.into())
            .or_insert_with(|| values.into_iter().collect());
    }
    fn cell(value: &str) -> String {
        format!("\"{}\"", value.replace('"', "\"\""))
    }
    let mut csv = "key,en-US,es-ES,ru-RU,uk-UA,de-DE\n".to_owned();
    for (i, (english, values)) in rows.iter().enumerate() {
        csv.push_str(&format!("SettingsFlowTest.{i},{}", cell(english)));
        for lang in ["es-ES", "ru-RU", "uk-UA", "de-DE"] {
            csv.push(',');
            csv.push_str(&cell(&values[lang]));
        }
        csv.push('\n');
    }
    Arc::new(skylive_host::localization::Localization::from_csv(csv.as_bytes(), locale).unwrap())
}
fn frame(
    ctx: &egui::Context,
    session: &Session,
    slot: &mut Option<Dialog>,
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
            dialogs::show(ctx, slot, intents, true, &session.snapshot());
        },
    )
}
fn text<'a>(shape: &'a egui::epaint::Shape, label: &str) -> Option<&'a egui::epaint::TextShape> {
    match shape {
        egui::epaint::Shape::Text(t) if t.galley.text() == label => Some(t),
        egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| text(s, label)),
        _ => None,
    }
}

#[test]
fn seven_settings_pages_have_equal_reachable_ok_cancel_apply_in_five_locales_at_scale_boundaries() {
    let session = Session::new();
    let before = session.snapshot();
    for locale in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
        let catalog = catalog(locale);
        for scale in [1.0_f32, 1.5, 2.0] {
            let ctx = egui::Context::default();
            crate::theme::install(&ctx);
            ctx.set_zoom_factor(scale);
            crate::i18n::install(
                &ctx,
                catalog.clone(),
                Path::new("unused-settings-layout.csv"),
            );
            let size = egui::vec2(800.0 / scale, 560.0 / scale);
            for page in [
                SettingsPage::General,
                SettingsPage::Appearance,
                SettingsPage::Stream,
                SettingsPage::Output,
                SettingsPage::Audio,
                SettingsPage::Video,
                SettingsPage::Hotkeys,
            ] {
                let mut dialog = fresh(page, &before);
                if let Dialog::Streaming(draft) = &mut dialog {
                    draft.bind(&session).unwrap();
                }
                let mut slot = Some(dialog);
                let mut intents = Vec::new();
                for _ in 0..3 {
                    let _ = frame(&ctx, &session, &mut slot, size, vec![], &mut intents);
                }
                let output = frame(&ctx, &session, &mut slot, size, vec![], &mut intents);
                let mut rects = Vec::new();
                for (index, label) in ["OK", "Cancel", "Apply"].into_iter().enumerate() {
                    let rect = ctx.data(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", index)))
                            .unwrap()
                    });
                    assert!(
                        egui::Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(rect),
                        "{page:?} {locale} {scale} {label} {rect:?}"
                    );
                    let label = crate::i18n::tr_context(&ctx, label);
                    let (paint, clip) = output
                        .shapes
                        .iter()
                        .find_map(|s| text(&s.shape, &label).map(|t| (t, s.clip_rect)))
                        .unwrap();
                    assert!(
                        clip.contains_rect(egui::Rect::from_min_size(
                            paint.pos,
                            paint.galley.size()
                        )),
                        "{page:?} {locale} {scale} {label} fully painted"
                    );
                    rects.push(rect);
                }
                assert!(
                    (rects[0].width() - rects[1].width()).abs() < 0.1
                        && (rects[1].width() - rects[2].width()).abs() < 0.1
                );
                assert!(intents.is_empty());
                for pressed in [true, false] {
                    let p = rects[1].center();
                    let _ = frame(
                        &ctx,
                        &session,
                        &mut slot,
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
                assert!(slot.is_none());
                assert!(intents.is_empty());
                assert!(Arc::ptr_eq(&before, &session.snapshot()));
            }
        }
    }
}

#[test]
fn footer_apply_is_disabled_until_changed_and_hotkey_capture_disables_both_publication_actions() {
    let session = Session::new();
    let model = session.snapshot();
    let mut dialog = fresh(SettingsPage::Video, &model);
    let actions = actions(&dialog, &model).unwrap();
    assert!(actions[0].enabled);
    assert!(!actions[2].enabled);
    if let Dialog::Video { width, .. } = &mut dialog {
        *width = 128;
    }
    let actions = super::actions(&dialog, &model).unwrap();
    assert!(actions[0].enabled && actions[2].enabled);
    assert_eq!(actions[0].label, "OK");
    assert_eq!(actions[2].label, "Apply");
    let ctx = egui::Context::default();
    crate::theme::install(&ctx);
    let size = egui::vec2(1280.0, 800.0);
    let mut slot = Some(fresh(SettingsPage::Hotkeys, &model));
    let mut intents = Vec::new();
    for _ in 0..3 {
        let _ = frame(&ctx, &session, &mut slot, size, vec![], &mut intents);
    }
    let p = ctx
        .data(|data| {
            data.get_temp::<egui::Rect>(egui::Id::new((
                "hotkey-assignment",
                skylive_core::HotkeyAction::Undo,
                0_usize,
            )))
            .unwrap()
        })
        .center();
    for pressed in [true, false] {
        let _ = frame(
            &ctx,
            &session,
            &mut slot,
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
    assert!(matches!(&slot,Some(Dialog::Hotkeys(draft))if draft.is_listening()));
    let blocked = super::actions(slot.as_ref().unwrap(), &model).unwrap();
    assert!(!blocked[0].enabled && !blocked[2].enabled);
    for _ in 0..3 {
        let _ = frame(&ctx, &session, &mut slot, size, vec![], &mut intents);
    }
    for index in [0, 2] {
        let p = ctx
            .data(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", index)))
                    .unwrap()
            })
            .center();
        for pressed in [true, false] {
            let _ = frame(
                &ctx,
                &session,
                &mut slot,
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
    }
    assert!(intents.is_empty());
    assert!(Arc::ptr_eq(&model, &session.snapshot()));
}
