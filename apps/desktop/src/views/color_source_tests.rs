use super::*;
use crate::views::{
    Intent,
    dialogs::{self, Dialog},
};
use skylive_core::Session;
use skylive_host::localization::Localization;
use std::{path::Path, sync::Arc};
fn bound_add(session: &Session) -> ColorDraft {
    let model = session.snapshot();
    let mut draft = ColorDraft::add(
        session.preview(),
        "Sized color".into(),
        model.video().width(),
        model.video().height(),
    );
    draft.bind(session).unwrap();
    draft
}
fn apply_intent(session: &mut Session, intent: Intent) {
    let Intent::ApplyColorSource(submission) = intent else {
        panic!("expected Color Apply")
    };
    let revision = submission.revision();
    let command = submission.into_command(session).unwrap();
    session.apply_at(Some(revision), command).unwrap();
}
fn frame(
    ctx: &egui::Context,
    session: &Session,
    slot: &mut Option<Dialog>,
    events: Vec<egui::Event>,
    size: egui::Vec2,
) -> (egui::FullOutput, Vec<Intent>) {
    let mut intents = Vec::new();
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        },
        |ctx| dialogs::show(ctx, slot, &mut intents, true, &session.snapshot()),
    );
    (output, intents)
}
fn click(
    ctx: &egui::Context,
    session: &Session,
    slot: &mut Option<Dialog>,
    pos: egui::Pos2,
    size: egui::Vec2,
) -> Vec<Intent> {
    let mut emitted = Vec::new();
    for pressed in [true, false] {
        emitted.extend(
            frame(
                ctx,
                session,
                slot,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                size,
            )
            .1,
        );
    }
    emitted
}
fn replace_text(
    ctx: &egui::Context,
    session: &Session,
    slot: &mut Option<Dialog>,
    key: &str,
    value: &str,
    size: egui::Vec2,
) {
    let rect = ctx
        .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(key)))
        .unwrap();
    assert!(click(ctx, session, slot, rect.center(), size).is_empty());
    let select = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    let events = vec![
        egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: select,
        },
        egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: select,
        },
        egui::Event::Text(value.into()),
    ];
    assert!(frame(ctx, session, slot, events, size).1.is_empty());
}
#[test]
fn actual_five_locale_color_fields_and_footer_publish_one_sized_source_or_cancel_nothing() {
    for language in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
        for size in [egui::vec2(360.0, 360.0), egui::vec2(800.0, 560.0)] {
            let ctx = egui::Context::default();
            crate::theme::install(&ctx);
            crate::i18n::install(
                &ctx,
                Arc::new(Localization::embedded(language).unwrap()),
                Path::new("headless-color.csv"),
            );
            let mut session = Session::new();
            let baseline = session.snapshot();
            let mut slot = Some(Dialog::ColorSource(bound_add(&session)));
            frame(&ctx, &session, &mut slot, vec![], size);
            frame(&ctx, &session, &mut slot, vec![], size);
            if size.x > 400.0 {
                replace_text(&ctx, &session, &mut slot, "color-source-width", "317", size);
                replace_text(
                    &ctx,
                    &session,
                    &mut slot,
                    "color-source-height",
                    "229",
                    size,
                );
                assert!(
                    matches!(&slot,Some(Dialog::ColorSource(d)) if d.width=="317"&&d.height=="229")
                );
            }
            assert!(Arc::ptr_eq(&baseline, &session.snapshot()));
            let apply = ctx
                .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", 0))))
                .unwrap();
            let cancel = ctx
                .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", 1))))
                .unwrap();
            let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            assert!(viewport.contains_rect(apply) && viewport.contains_rect(cancel));
            assert!(
                (apply.width() - cancel.width()).abs() < 0.1
                    && (apply.height() - cancel.height()).abs() < 0.1
            );
            let mut intents = click(&ctx, &session, &mut slot, apply.center(), size);
            assert_eq!(intents.len(), 1);
            apply_intent(&mut session, intents.pop().unwrap());
            let expected = if size.x > 400.0 {
                (317, 229)
            } else {
                (baseline.video().width(), baseline.video().height())
            };
            assert_eq!(
                session.snapshot().sources()[0].kind().dimensions(),
                expected
            );
            assert_eq!(session.snapshot().revision(), baseline.revision() + 1);
            session.apply(Command::Undo).unwrap();
            assert!(session.snapshot().sources().is_empty());
            let mut draft = bound_add(&session);
            draft.width = "999".into();
            slot = Some(Dialog::ColorSource(draft));
            frame(&ctx, &session, &mut slot, vec![], size);
            frame(&ctx, &session, &mut slot, vec![], size);
            let before = session.snapshot();
            let cancel = ctx
                .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", 1))))
                .unwrap();
            assert!(click(&ctx, &session, &mut slot, cancel.center(), size).is_empty());
            assert!(slot.is_none());
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
    }
}
#[test]
fn invalid_text_and_stale_or_replaced_documents_never_publish_color_or_size() {
    let mut s = Session::new();
    let mut draft = bound_add(&s);
    let before = s.snapshot();
    for invalid in ["", "0", "8193", "1.5", "-1", "4294967296"] {
        draft.width = invalid.into();
        assert!(draft.intent().is_err());
        assert!(Arc::ptr_eq(&before, &s.snapshot()));
    }
    draft.width = "16".into();
    draft.height = "8".into();
    let Intent::ApplyColorSource(stale) = draft.intent().unwrap() else {
        panic!()
    };
    s.apply(Command::RenameCollection {
        name: "Changed".into(),
    })
    .unwrap();
    let changed = s.snapshot();
    assert!(stale.into_command(&s).is_err());
    assert!(Arc::ptr_eq(&changed, &s.snapshot()));
    let draft = bound_add(&s);
    let Intent::ApplyColorSource(foreign) = draft.intent().unwrap() else {
        panic!()
    };
    assert!(foreign.into_command(&Session::new()).is_err());
    let draft = bound_add(&s);
    let Intent::ApplyColorSource(closed) = draft.intent().unwrap() else {
        panic!()
    };
    s.close();
    assert!(closed.into_command(&s).is_err());
}
#[test]
fn rebind_captures_real_size_rejects_changed_source_before_open_and_combines_one_undo() {
    let mut s = Session::new();
    let mut draft = bound_add(&s);
    draft.width = "100".into();
    draft.height = "80".into();
    apply_intent(&mut s, draft.intent().unwrap());
    let source = s.snapshot().sources()[0].id();
    let mut stale = ColorDraft::rebind(source, [60, 100, 200, 255], 100, 80);
    s.apply(Command::SetColorSize {
        source,
        width: 120,
        height: 90,
    })
    .unwrap();
    assert!(stale.bind(&s).is_err());
    let before = s.snapshot();
    let mut draft = ColorDraft::rebind(source, [60, 100, 200, 255], 120, 90);
    draft.bind(&s).unwrap();
    draft.width = "240".into();
    draft.height = "45".into();
    draft.rgba = [9, 8, 7, 6];
    apply_intent(&mut s, draft.intent().unwrap());
    assert_eq!(
        s.snapshot().source(source).unwrap().kind(),
        &SourceKind::Color {
            rgba: [9, 8, 7, 6],
            width: 240,
            height: 45
        }
    );
    s.apply(Command::Undo).unwrap();
    assert_eq!(s.snapshot().sources(), before.sources());
    assert_eq!(s.snapshot().scenes(), before.scenes());
}
