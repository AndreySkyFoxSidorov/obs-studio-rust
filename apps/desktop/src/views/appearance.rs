//! Staged global presentation values. No style mutation until core publication.
use eframe::egui;
use skylive_core::{Appearance, Command, InterfaceTheme};
pub struct AppearanceDraft {
    baseline: Appearance,
    pub theme: InterfaceTheme,
    pub scale_percent: u16,
}
impl AppearanceDraft {
    pub fn new(value: Appearance) -> Self {
        Self {
            baseline: value,
            theme: value.theme(),
            scale_percent: value.scale_percent(),
        }
    }
    pub fn prepared(&self) -> Result<Appearance, String> {
        Appearance::new(self.theme, self.scale_percent).map_err(|error| error.to_string())
    }
    pub fn unchanged(&self, current: Appearance) -> bool {
        self.prepared().is_ok_and(|value| value == current)
    }
    pub fn command(&self, current: Appearance) -> Result<Command, String> {
        if current != self.baseline {
            return Err("Appearance settings changed. Reopen this page before applying.".into());
        }
        Ok(Command::SetAppearance {
            appearance: self.prepared()?,
        })
    }
}
fn label(theme: InterfaceTheme) -> &'static str {
    match theme {
        InterfaceTheme::Yami => "Yami",
        InterfaceTheme::Dark => "Grey",
        InterfaceTheme::Acri => "Acri",
        InterfaceTheme::Classic => "Classic",
        InterfaceTheme::Rachni => "Rachni",
        InterfaceTheme::Light => "Light",
        InterfaceTheme::System => "System",
    }
}
pub fn editor(ui: &mut egui::Ui, draft: &mut AppearanceDraft) {
    egui::Grid::new("appearance-fields").num_columns(2).spacing([14.0,10.0]).show(ui,|ui|{
        let name=ui.label(crate::i18n::tr(ui,"Theme"));
        let menu=egui::ComboBox::from_id_salt("appearance-theme").selected_text(crate::i18n::tr(ui,label(draft.theme))).width(200.0).truncate().show_ui(ui,|ui|{
            for theme in InterfaceTheme::ALL{
                let response=ui.selectable_value(&mut draft.theme,theme,crate::i18n::tr(ui,label(theme))).on_hover_text(crate::i18n::tr(ui,"Choose the colors used by the interface."));
                #[cfg(test)] ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new(("appearance-theme-option",theme)),response.rect));
                let _=response;
            }
        }).response.labelled_by(name.id).on_hover_text(crate::i18n::tr(ui,"Choose the colors used by the interface."));
        #[cfg(test)] ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new("appearance-theme-menu"),menu.rect));
        let _=menu;ui.end_row();
        let name=ui.label(crate::i18n::tr(ui,"Interface scale"));
        let scale=ui.add(egui::DragValue::new(&mut draft.scale_percent).range(75..=200).clamp_existing_to_range(false).speed(1).suffix("%")).labelled_by(name.id).on_hover_text(crate::i18n::tr(ui,"Scale interface text and controls from 75% to 200%. Video resolution stays unchanged."));
        #[cfg(test)] ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new("appearance-scale"),scale.rect));
        let _=scale;ui.end_row();
    });
    if draft.theme == InterfaceTheme::System {
        ui.label(crate::i18n::tr(
            ui,
            "System follows the operating system's light or dark appearance.",
        ));
    }
    ui.label(crate::i18n::tr(ui,"Apply changes the interface theme and scale. Cancel keeps your current appearance. These preferences are separate from scene Undo."));
    if ui
        .button(crate::i18n::tr(ui, "Restore default appearance"))
        .on_hover_text(crate::i18n::tr(
            ui,
            "Choose Yami and 100% scale in this draft.",
        ))
        .clicked()
    {
        let value = Appearance::default();
        draft.theme = value.theme();
        draft.scale_percent = value.scale_percent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::{
        Intent,
        dialogs::{self, Dialog},
    };
    use skylive_core::Session;
    fn frame(
        context: &egui::Context,
        slot: &mut Option<Dialog>,
        session: &Session,
        events: Vec<egui::Event>,
    ) -> Vec<Intent> {
        let mut intents = vec![];
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 560.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| dialogs::show(ctx, slot, &mut intents, true, &session.snapshot()),
        );
        intents
    }
    fn settle(context: &egui::Context, slot: &mut Option<Dialog>, session: &Session) {
        for _ in 0..3 {
            frame(context, slot, session, vec![]);
        }
    }
    fn point(context: &egui::Context, id: egui::Id) -> egui::Pos2 {
        context
            .data(|data| data.get_temp::<egui::Rect>(id).unwrap())
            .center()
    }
    fn click(
        context: &egui::Context,
        slot: &mut Option<Dialog>,
        session: &Session,
        position: egui::Pos2,
    ) -> Vec<Intent> {
        let mut out = vec![];
        for pressed in [true, false] {
            out.extend(frame(
                context,
                slot,
                session,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            ));
        }
        out
    }
    #[test]
    fn real_theme_and_scale_controls_stage_cancel_then_publish_one_global_preference() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let mut runtime = crate::theme::Runtime::default();
        let mut session = Session::new();
        let before = session.snapshot();
        let mut slot = Some(Dialog::Appearance(AppearanceDraft::new(
            before.preferences().appearance(),
        )));
        settle(&context, &mut slot, &session);
        click(
            &context,
            &mut slot,
            &session,
            point(&context, egui::Id::new("appearance-theme-menu")),
        );
        click(
            &context,
            &mut slot,
            &session,
            point(
                &context,
                egui::Id::new(("appearance-theme-option", InterfaceTheme::Light)),
            ),
        );
        assert!(
            context.style().visuals.dark_mode,
            "draft cannot preview/publish a canonical theme"
        );
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        click(
            &context,
            &mut slot,
            &session,
            point(&context, egui::Id::new(("modal-action-rect", 1))),
        );
        assert!(slot.is_none());
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        slot = Some(Dialog::Appearance(AppearanceDraft::new(
            before.preferences().appearance(),
        )));
        settle(&context, &mut slot, &session);
        click(
            &context,
            &mut slot,
            &session,
            point(&context, egui::Id::new("appearance-theme-menu")),
        );
        click(
            &context,
            &mut slot,
            &session,
            point(
                &context,
                egui::Id::new(("appearance-theme-option", InterfaceTheme::Light)),
            ),
        );
        // Choosing a different theme adds the unsaved-settings navigation note,
        // so obtain the field from its newly laid-out frame before clicking.
        settle(&context, &mut slot, &session);
        click(
            &context,
            &mut slot,
            &session,
            point(&context, egui::Id::new("appearance-scale")),
        );
        assert!(
            context
                .memory(|memory| memory.focused())
                .is_some_and(|id| egui::TextEdit::load_state(&context, id).is_some()),
            "the actual scale field must have text-entry focus before typing"
        );
        frame(
            &context,
            &mut slot,
            &session,
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                },
                egui::Event::Text("150".into()),
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        settle(&context, &mut slot, &session);
        let intents = click(
            &context,
            &mut slot,
            &session,
            point(&context, egui::Id::new(("modal-action-rect", 0))),
        );
        let mut commands = intents.into_iter().filter_map(|intent| match intent {
            Intent::Command(command) => Some(command),
            Intent::SubmitSettings { .. } => {
                super::super::settings::prepare(slot.as_ref().unwrap(), &session).ok()
            }
            _ => None,
        });
        let command = commands
            .next()
            .expect("actual Apply emits the shared operation");
        assert!(commands.next().is_none());
        session.apply(command).unwrap();
        assert_eq!(
            session.snapshot().preferences().appearance(),
            Appearance::new(InterfaceTheme::Light, 150).unwrap()
        );
        assert_eq!(session.snapshot().revision(), before.revision() + 1);
        assert_eq!(session.document_revision(), 0);
        assert!(!session.is_document_dirty());
        assert!(!session.can_undo());
        assert_eq!(session.snapshot().profiles(), before.profiles());
        assert_eq!(session.snapshot().sources(), before.sources());
        runtime.sync(&context, session.snapshot().preferences().appearance());
        let _ = context.run(egui::RawInput::default(), |_| {});
        assert!(!context.style().visuals.dark_mode);
        assert_eq!(context.zoom_factor(), 1.5);
    }
    #[test]
    fn invalid_and_concurrently_replaced_appearance_preserve_drafts_and_canonical_state() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let mut session = Session::new();
        let mut draft = AppearanceDraft::new(Appearance::default());
        draft.scale_percent = 201;
        let mut slot = Some(Dialog::Appearance(draft));
        settle(&context, &mut slot, &session);
        let before = session.snapshot();
        let intents = click(
            &context,
            &mut slot,
            &session,
            point(&context, egui::Id::new(("modal-action-rect", 0))),
        );
        assert!(
            intents
                .iter()
                .any(|intent| matches!(intent, Intent::DialogError(_)))
        );
        assert!(matches!(&slot,Some(Dialog::Appearance(draft))if draft.scale_percent==201));
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        slot = Some(Dialog::Appearance(AppearanceDraft::new(
            Appearance::default(),
        )));
        session
            .apply(Command::SetAppearance {
                appearance: Appearance::new(InterfaceTheme::Dark, 125).unwrap(),
            })
            .unwrap();
        let current = session.snapshot();
        settle(&context, &mut slot, &session);
        let intents = click(
            &context,
            &mut slot,
            &session,
            point(&context, egui::Id::new(("modal-action-rect", 0))),
        );
        assert!(
            intents
                .iter()
                .any(|intent| matches!(intent, Intent::DialogError(_)))
        );
        assert!(std::sync::Arc::ptr_eq(&current, &session.snapshot()));
        assert!(slot.is_some());
        assert!(matches!(
            AppearanceDraft::new(current.preferences().appearance())
                .command(current.preferences().appearance()),
            Ok(Command::SetAppearance { .. })
        ));
    }
}
