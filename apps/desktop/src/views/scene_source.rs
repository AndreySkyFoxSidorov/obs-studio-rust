//! Existing-scene chooser. A draft owns no scene contents, pixels or OS state.
use eframe::egui;
use skylive_core::{Command, Id, StudioSnapshot};
#[derive(Clone, Copy)]
pub enum SceneEdit {
    Add { scene: Id },
    Rebind { source: Id },
}
pub struct SceneDraft {
    pub edit: SceneEdit,
    pub target: Option<Id>,
}
impl SceneDraft {
    pub fn add(scene: Id) -> Self {
        Self {
            edit: SceneEdit::Add { scene },
            target: None,
        }
    }
    pub fn rebind(source: Id, target: Id) -> Self {
        Self {
            edit: SceneEdit::Rebind { source },
            target: Some(target),
        }
    }
    pub fn command(&self) -> Result<Command, String> {
        let target = self
            .target
            .ok_or_else(|| "Select an existing scene.".to_owned())?;
        match self.edit {
            SceneEdit::Add { scene } if scene == target => {
                Err("A scene cannot contain itself.".into())
            }
            SceneEdit::Add { scene } => Ok(Command::AddSceneSource { scene, target }),
            SceneEdit::Rebind { source } => Ok(Command::RebindSceneSource { source, target }),
        }
    }
}
pub fn editor(ui: &mut egui::Ui, model: &StudioSnapshot, draft: &mut SceneDraft) {
    ui.label(crate::i18n::tr(ui, "Existing scene"));
    egui::ScrollArea::vertical()
        .id_salt("existing-scene-choices")
        .max_height(240.0)
        .show(ui, |ui| {
            for scene in model.scenes() {
                let enabled =
                    !matches!(draft.edit,SceneEdit::Add {scene:owner} if owner==scene.id());
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(scene.name())
                            .wrap()
                            .selected(draft.target == Some(scene.id())),
                    )
                    .on_hover_text(scene.name())
                    .on_hover_text(crate::i18n::tr(ui, "Use this shared scene as a source."))
                    .on_disabled_hover_text(crate::i18n::tr(ui, "A scene cannot contain itself."))
                    .clicked()
                {
                    draft.target = Some(scene.id());
                }
            }
        });
    ui.label(crate::i18n::tr(
        ui,
        "Changes to the nested scene are shared. Cycles and excessive nesting are rejected.",
    ));
}
#[cfg(test)]
mod tests {
    use super::*;
    use skylive_core::Session;
    use std::sync::Arc;
    #[test]
    fn chooser_is_a_draft_and_apply_creates_one_shared_reference_with_one_undo() {
        let mut session = Session::new();
        let outer = session.preview();
        let target = session
            .apply(Command::AddScene {
                name: "Existing child".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let context = egui::Context::default();
        let before = session.snapshot();
        let mut draft = SceneDraft::add(outer);
        assert!(draft.command().is_err());
        let _ = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| editor(ui, &before, &mut draft));
        });
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        assert_eq!(draft.target, None, "painting must not auto-author a target");
        draft.target = Some(outer);
        assert!(draft.command().is_err());
        draft.target = Some(target);
        session.apply(draft.command().unwrap()).unwrap();
        assert_eq!(session.snapshot().scene(outer).unwrap().items().len(), 1);
        assert_eq!(session.snapshot().revision(), before.revision() + 1);
        let issued = session.snapshot().scene(outer).unwrap().items()[0]
            .id()
            .get();
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().scenes(), before.scenes());
        assert_eq!(session.snapshot().sources(), before.sources());
        let next = session
            .apply(draft.command().unwrap())
            .unwrap()
            .created
            .unwrap();
        assert!(
            next.get() > issued,
            "Undo never reuses an issued source/item identity"
        );
    }
}
