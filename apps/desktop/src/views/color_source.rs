//! Authored intrinsic Color properties; no resource/OS work or UI-owned model.
use eframe::egui;
use skylive_core::{Command, Id, Session, SourceKind};

#[derive(Clone, Copy)]
pub enum ColorEdit {
    Add { scene: Id },
    Rebind { source: Id },
}
pub struct ColorDraft {
    pub edit: ColorEdit,
    pub name: String,
    pub rgba: [u8; 4],
    pub width: String,
    pub height: String,
    session: Option<u64>,
    revision: u64,
    original: Option<([u8; 4], u32, u32)>,
}
pub struct ColorSubmission {
    session: u64,
    revision: u64,
    command: Command,
}
impl ColorSubmission {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn into_command(self, session: &Session) -> Result<Command, String> {
        if session.is_closed()
            || self.session != session.identity()
            || self.revision != session.snapshot().revision()
        {
            return Err("Color properties are stale. Reopen Properties before applying.".into());
        }
        Ok(self.command)
    }
}
impl ColorDraft {
    pub fn add(scene: Id, name: String, width: u32, height: u32) -> Self {
        Self {
            edit: ColorEdit::Add { scene },
            name,
            rgba: [60, 100, 200, 255],
            width: width.to_string(),
            height: height.to_string(),
            session: None,
            revision: 0,
            original: None,
        }
    }
    pub fn rebind(source: Id, rgba: [u8; 4], width: u32, height: u32) -> Self {
        Self {
            edit: ColorEdit::Rebind { source },
            name: String::new(),
            rgba,
            width: width.to_string(),
            height: height.to_string(),
            session: None,
            revision: 0,
            original: Some((rgba, width, height)),
        }
    }
    pub fn title(&self) -> &'static str {
        match self.edit {
            ColorEdit::Add { .. } => "Color Source",
            ColorEdit::Rebind { .. } => "Properties: Color",
        }
    }
    pub fn bind(&mut self, session: &Session) -> Result<(), String> {
        if session.is_closed() {
            return Err("Color properties belong to a closed document.".into());
        }
        let model = session.snapshot();
        if self
            .session
            .is_some_and(|owner| owner != session.identity() || self.revision != model.revision())
        {
            return Err("Color properties are stale. Reopen Properties before applying.".into());
        }
        match self.edit {
            ColorEdit::Add { scene } if model.scene(scene).is_none() => {
                return Err("Color target scene is unavailable.".into());
            }
            ColorEdit::Rebind { source } => {
                let Some(SourceKind::Color {
                    rgba,
                    width,
                    height,
                }) = model.source(source).map(|s| s.kind())
                else {
                    return Err("Color source is unavailable.".into());
                };
                if self.original != Some((*rgba, *width, *height)) {
                    return Err("Color source changed before its properties opened.".into());
                }
            }
            _ => {}
        }
        self.session = Some(session.identity());
        self.revision = model.revision();
        Ok(())
    }
    fn command(&self) -> Result<Command, String> {
        let edge = |text: &str| {
            text.parse::<u32>()
                .ok()
                .filter(|v| (1..=8192).contains(v))
                .ok_or_else(|| {
                    "Color width and height require whole pixels from 1 to 8192.".to_owned()
                })
        };
        let width = edge(&self.width)?;
        let height = edge(&self.height)?;
        Ok(match self.edit {
            ColorEdit::Add { scene } => {
                if self.name.trim().is_empty()
                    || self.name.len() > 256
                    || self.name.chars().any(char::is_control)
                {
                    return Err(
                        "Source name requires 1..256 UTF-8 bytes without control characters."
                            .into(),
                    );
                }
                Command::AddColorSourceSized {
                    scene,
                    name: self.name.clone(),
                    rgba: self.rgba,
                    width,
                    height,
                }
            }
            ColorEdit::Rebind { source } => Command::ConfigureColorSource {
                source,
                rgba: self.rgba,
                width,
                height,
            },
        })
    }
    pub fn intent(&self) -> Result<super::Intent, String> {
        let session = self
            .session
            .ok_or("Color properties must be opened in a document before Apply.")?;
        Ok(super::Intent::ApplyColorSource(ColorSubmission {
            session,
            revision: self.revision,
            command: self.command()?,
        }))
    }
}
pub fn editor(ui: &mut egui::Ui, draft: &mut ColorDraft) {
    if matches!(draft.edit, ColorEdit::Add { .. }) {
        let label = ui.label(crate::i18n::tr(ui, "Name"));
        ui.add(
            egui::TextEdit::singleline(&mut draft.name)
                .char_limit(256)
                .id_salt("color-source-name"),
        )
        .labelled_by(label.id)
        .on_hover_text(crate::i18n::tr(ui, "Name this shared Color Source."))
        .on_disabled_hover_text(crate::i18n::tr(ui, "Name this shared Color Source."));
    }
    ui.horizontal_wrapped(|ui| {
        let label = ui.label(crate::i18n::tr(ui, "Color (RGBA)"));
        ui.color_edit_button_srgba_unmultiplied(&mut draft.rgba)
            .labelled_by(label.id)
            .on_hover_text(crate::i18n::tr(
                ui,
                "Choose the shared source color and opacity.",
            ))
            .on_disabled_hover_text(crate::i18n::tr(
                ui,
                "Choose the shared source color and opacity.",
            ));
    });
    ui.horizontal_wrapped(|ui| {
        for (index, value) in draft.rgba.iter_mut().enumerate() {
            ui.label(["R", "G", "B", "A"][index]);
            ui.add(egui::DragValue::new(value).range(0..=255))
                .on_hover_text(crate::i18n::tr(ui, "Numeric value"))
                .on_disabled_hover_text(crate::i18n::tr(ui, "Numeric value"));
        }
    });
    for (key, label, value) in [
        ("color-source-width", "Width", &mut draft.width),
        ("color-source-height", "Height", &mut draft.height),
    ] {
        let label = ui.label(crate::i18n::tr(ui, label));
        let response = ui
            .add(
                egui::TextEdit::singleline(value)
                    .char_limit(10)
                    .desired_width(140.0)
                    .id_salt(key),
            )
            .labelled_by(label.id)
            .on_hover_text(crate::i18n::tr(
                ui,
                "Intrinsic source size in whole pixels, from 1 to 8192.",
            ))
            .on_disabled_hover_text(crate::i18n::tr(
                ui,
                "Intrinsic source size in whole pixels, from 1 to 8192.",
            ));
        #[cfg(test)]
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::new(key), response.rect));
        #[cfg(not(test))]
        let _ = response;
    }
    ui.label(crate::i18n::tr(ui,"Color and size update all references together. Existing item scale and crop are preserved."));
}
#[cfg(test)]
#[path = "color_source_tests.rs"]
mod tests;
