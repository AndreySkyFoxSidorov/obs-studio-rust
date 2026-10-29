//! Staged GUI tests; no native viewport, service, DNS or socket execution.
use super::*;
use crate::views::modal_footer::{self, Action};
#[test]
fn drafts_do_not_publish_until_apply_and_reset_cancel_leave_owner_unchanged() {
    let mut session = Session::new();
    let before = session.snapshot();
    let mut draft = StreamingDraft::capture(&session).unwrap();
    draft.server_url = "rtmp://127.0.0.1/live".into();
    draft.stream_key = " exact/public/key?x=a%2Bb ".into();
    draft.video_bitrate = "8000".into();
    assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
    let request = draft.request().unwrap();
    let command = request.command_for(&session).unwrap();
    session.apply(command).unwrap();
    assert_eq!(
        session
            .snapshot()
            .profiles()
            .selected()
            .streaming()
            .stream_key(),
        " exact/public/key?x=a%2Bb "
    );
    let after = session.snapshot();
    let mut cancelled = StreamingDraft::capture(&session).unwrap();
    cancelled.reset();
    assert!(!cancelled.unchanged(&after));
    drop(cancelled);
    assert!(std::sync::Arc::ptr_eq(&after, &session.snapshot()));
    let unchanged = StreamingDraft::capture(&session)
        .unwrap()
        .request()
        .unwrap()
        .command_for(&session)
        .unwrap();
    session.apply(unchanged).unwrap();
    assert!(std::sync::Arc::ptr_eq(&after, &session.snapshot()));
}
#[test]
fn profile_switch_model_change_closed_and_unbound_drafts_are_rejected() {
    let mut session = Session::new();
    let draft = StreamingDraft::capture(&session).unwrap();
    session
        .apply(Command::Profile {
            command: ProfileCommand::Duplicate {
                name: "Different profile".into(),
            },
        })
        .unwrap();
    let current = session.snapshot();
    assert!(draft.request().unwrap().command_for(&session).is_err());
    assert!(std::sync::Arc::ptr_eq(&current, &session.snapshot()));
    let unbound = StreamingDraft::from_model(&current);
    assert!(unbound.request().is_err());
    let draft = StreamingDraft::capture(&session).unwrap();
    session.close();
    assert!(draft.request().unwrap().command_for(&session).is_err());
}
fn modal(
    context: &egui::Context,
    draft: &mut StreamingDraft,
    events: Vec<egui::Event>,
) -> Option<usize> {
    let mut clicked = None;
    let _ = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 560.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::Modal::new(egui::Id::new("staged-stream-editor")).show(ctx, |ui| {
                ui.set_width(720.0);
                ui.heading(crate::i18n::tr(ui, "Settings — Stream"));
                let actions = [
                    Action::new("Apply", true, "Apply these settings and close the dialog."),
                    Action::new(
                        "Cancel",
                        true,
                        "Discard this draft and keep the current settings.",
                    ),
                ];
                let height = (ctx.screen_rect().height()
                    - modal_footer::required_height(ui, &actions)
                    - 100.0)
                    .max(40.0);
                egui::ScrollArea::vertical()
                    .max_height(height)
                    .show(ui, |ui| editor(ui, draft));
                ui.separator();
                clicked = modal_footer::show(ui, &actions);
            });
        },
    );
    clicked
}
#[test]
fn five_language_minimum_footer_has_reachable_real_apply_and_cancel_without_connection() {
    for language in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let localization = std::sync::Arc::new(
            skylive_host::localization::Localization::embedded(language).unwrap(),
        );
        crate::i18n::install(&context, localization, &crate::i18n::workspace_path());
        let mut session = Session::new();
        let before = session.snapshot();
        let mut draft = StreamingDraft::capture(&session).unwrap();
        draft.server_url = "rtmp://127.0.0.1/live".into();
        draft.stream_key = "public-test-only".into();
        for _ in 0..3 {
            modal(&context, &mut draft, vec![]);
        }
        for index in 0..2 {
            let rect = context
                .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", index))))
                .unwrap();
            assert!(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 560.0))
                    .expand(1.0)
                    .contains_rect(rect),
                "{language}: {rect:?}"
            );
        }
        let apply = context
            .data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", 0))))
            .unwrap();
        let mut result = None;
        for pressed in [true, false] {
            result = modal(
                &context,
                &mut draft,
                vec![
                    egui::Event::PointerMoved(apply.center()),
                    egui::Event::PointerButton {
                        pos: apply.center(),
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
        assert_eq!(result, Some(0));
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        session
            .apply(draft.request().unwrap().command_for(&session).unwrap())
            .unwrap();
        assert!(
            session
                .snapshot()
                .profiles()
                .selected()
                .streaming()
                .is_configured()
        );
    }
}
