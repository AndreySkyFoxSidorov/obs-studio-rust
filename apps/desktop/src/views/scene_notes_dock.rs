//! One bounded, explicitly committed plain-text dock draft. No canonical writes.
use super::{Intent, plugin_routes::PluginEditorToken};
use eframe::egui;
use skylive_core::{Command, Id, MAX_SCENE_NOTES_BYTES, SCENE_NOTES_PLUGIN, StudioSnapshot};

const SIDE_PANEL: &str = "scene-notes-right-panel";
const BOTTOM_PANEL: &str = "scene-notes-bottom-panel";
const STALE: &str =
    "Scene notes changed. Copy the retained draft or cancel edits to reload the current scene.";
const FOREIGN: &str = "Another collection is open. This retained notes draft cannot be applied. Cancel edits to load the current scene.";
const EXHAUSTED: &str = "Scene Notes dock identity exhausted. Reopen the application.";
const INPUT_LIMIT: &str =
    "The insertion exceeds the 16 KiB notes limit. The draft was kept unchanged.";

struct Draft {
    token: PluginEditorToken,
    scene: Id,
    name: String,
    baseline: String,
    text: String,
}
impl Draft {
    fn dirty(&self) -> bool {
        self.text != self.baseline
    }
}
#[derive(Clone, Debug)]
pub struct NotesSubmission {
    pub token: PluginEditorToken,
    pub scene: Id,
    pub text: String,
}
#[derive(Default)]
pub struct NotesDock {
    draft: Option<Draft>,
    sequence: u64,
    was_enabled: bool,
    error: Option<&'static str>,
    editor_id: Option<egui::Id>,
}
impl NotesDock {
    /// Return a rising capability edge so the host can reveal this dock.
    pub fn observe(&mut self, session: u64, model: &StudioSnapshot, selected: Id) -> bool {
        let enabled = model.preferences().plugin_enabled(SCENE_NOTES_PLUGIN);
        let revealed = enabled && !self.was_enabled;
        self.was_enabled = enabled;
        if !enabled {
            return revealed;
        }
        let refresh = self.draft.as_ref().is_none_or(|draft| {
            !draft.dirty()
                && (draft.token.session != session
                    || draft.token.revision != model.revision()
                    || draft.scene != selected)
        });
        if refresh {
            match self.capture(session, model, selected) {
                Ok(draft) => {
                    self.draft = Some(draft);
                    self.error = None;
                }
                Err(error) => self.error = Some(error),
            }
        }
        revealed
    }
    fn capture(
        &mut self,
        session: u64,
        model: &StudioSnapshot,
        scene: Id,
    ) -> Result<Draft, &'static str> {
        let source = model
            .scene(scene)
            .ok_or("The selected scene no longer exists.")?;
        if source.notes().len() > MAX_SCENE_NOTES_BYTES {
            return Err("Scene notes exceed the supported limit.");
        }
        let generation = self.sequence.checked_add(1).ok_or(EXHAUSTED)?;
        let mut text = String::with_capacity(MAX_SCENE_NOTES_BYTES);
        text.push_str(source.notes());
        let draft = Draft {
            token: PluginEditorToken {
                session,
                revision: model.revision(),
                generation,
            },
            scene,
            name: source.name().into(),
            baseline: source.notes().into(),
            text,
        };
        self.sequence = generation;
        Ok(draft)
    }
    fn current(&self, session: u64, model: &StudioSnapshot) -> Result<&Draft, &'static str> {
        if !model.preferences().plugin_enabled(SCENE_NOTES_PLUGIN) {
            return Err("Scene Notes module is disabled.");
        }
        let draft = self
            .draft
            .as_ref()
            .ok_or("There is no Scene Notes dock draft.")?;
        if draft.token.session != session {
            return Err(FOREIGN);
        }
        if draft.token.revision != model.revision() || model.scene(draft.scene).is_none() {
            return Err(STALE);
        }
        if draft.text.len() > MAX_SCENE_NOTES_BYTES {
            return Err("Scene notes exceed the supported limit.");
        }
        // Reserve the next presentation identity before canonical publication.
        self.sequence.checked_add(1).ok_or(EXHAUSTED)?;
        Ok(draft)
    }
    pub fn submission(
        &self,
        session: u64,
        model: &StudioSnapshot,
    ) -> Result<NotesSubmission, &'static str> {
        let draft = self.current(session, model)?;
        Ok(NotesSubmission {
            token: draft.token,
            scene: draft.scene,
            text: draft.text.clone(),
        })
    }
    pub fn command_for(
        &self,
        session: u64,
        model: &StudioSnapshot,
        submission: &NotesSubmission,
    ) -> Result<Command, &'static str> {
        let draft = self.current(session, model)?;
        if draft.token != submission.token
            || draft.scene != submission.scene
            || draft.text != submission.text
        {
            return Err(STALE);
        }
        Ok(Command::SetSceneNotes {
            scene: submission.scene,
            text: submission.text.clone(),
        })
    }
    pub fn accepted(
        &mut self,
        session: u64,
        model: &StudioSnapshot,
        selected: Id,
    ) -> Result<(), &'static str> {
        let fresh = self.capture(session, model, selected)?;
        self.draft = Some(fresh);
        self.error = None;
        Ok(())
    }
    pub fn cancel(
        &mut self,
        token: PluginEditorToken,
        session: u64,
        model: &StudioSnapshot,
        selected: Id,
    ) -> Result<(), &'static str> {
        if !model.preferences().plugin_enabled(SCENE_NOTES_PLUGIN) {
            return Err("Scene Notes module is disabled.");
        }
        if self.draft.as_ref().is_none_or(|draft| draft.token != token) {
            return Err(STALE);
        }
        let fresh = self.capture(session, model, selected)?;
        self.draft = Some(fresh);
        self.error = None;
        Ok(())
    }
}

/// Egui deletes selected text before inserting replacement text. The view keeps
/// one cap-bounded pre-edit copy and restores it if this rejects an insertion.
struct NotesText<'a> {
    text: &'a mut String,
    before: String,
    rejected: bool,
}
impl<'a> NotesText<'a> {
    fn new(text: &'a mut String) -> Self {
        let mut before = String::with_capacity(MAX_SCENE_NOTES_BYTES);
        before.push_str(text);
        Self {
            text,
            before,
            rejected: false,
        }
    }
}
impl egui::TextBuffer for NotesText<'_> {
    fn is_mutable(&self) -> bool {
        true
    }
    fn as_str(&self) -> &str {
        self.text.as_str()
    }
    fn insert_text(&mut self, text: &str, char_index: usize) -> usize {
        if self.rejected {
            return 0;
        }
        if text.len() > MAX_SCENE_NOTES_BYTES.saturating_sub(self.text.len()) {
            self.rejected = true;
            *self.text = std::mem::take(&mut self.before);
            return 0;
        }
        let byte = self
            .text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(index, _)| index);
        self.text.insert_str(byte, text);
        text.chars().count()
    }
    fn delete_char_range(&mut self, range: std::ops::Range<usize>) {
        if self.rejected || range.start > range.end {
            return;
        }
        let start = self
            .text
            .char_indices()
            .nth(range.start)
            .map_or(self.text.len(), |(index, _)| index);
        let end = self
            .text
            .char_indices()
            .nth(range.end)
            .map_or(self.text.len(), |(index, _)| index);
        self.text.replace_range(start..end, "");
    }
}

fn retire_editor(ctx: &egui::Context, slot: &mut Option<egui::Id>) {
    if let Some(id) = slot.take() {
        ctx.memory_mut(|memory| memory.surrender_focus(id));
        ctx.data_mut(|data| data.remove::<egui::text_edit::TextEditState>(id));
    }
}
fn use_editor(ctx: &egui::Context, slot: &mut Option<egui::Id>, id: egui::Id) {
    if *slot != Some(id) {
        retire_editor(ctx, slot);
        let mut state = egui::text_edit::TextEditState::default();
        state.set_undoer(egui::util::undoer::Undoer::with_settings(
            egui::util::undoer::Settings {
                max_undos: 8,
                ..Default::default()
            },
        ));
        state.store(ctx, id);
        *slot = Some(id);
    }
}

pub fn forget_sizes(ctx: &egui::Context) {
    ctx.data_mut(|data| {
        for id in [SIDE_PANEL, BOTTOM_PANEL] {
            data.remove::<egui::containers::panel::PanelState>(egui::Id::new(id));
        }
    });
}
pub struct NotesView<'a> {
    pub session: u64,
    pub model: &'a StudioSnapshot,
    pub selected: Id,
    pub visible: bool,
    pub editing: bool,
    pub locked: bool,
}
pub fn show(
    ctx: &egui::Context,
    state: &mut NotesDock,
    view: NotesView<'_>,
    intents: &mut Vec<Intent>,
) {
    let NotesView {
        session,
        model,
        selected,
        visible,
        editing,
        locked,
    } = view;
    #[cfg(test)]
    ctx.data_mut(|data| {
        for id in [
            "actual-notes-dock",
            "scene-notes-editor-rect",
            "scene-notes-apply-rect",
            "scene-notes-cancel-rect",
        ] {
            data.remove::<egui::Rect>(egui::Id::new(id));
        }
    });
    if !visible || !model.preferences().plugin_enabled(SCENE_NOTES_PLUGIN) {
        retire_editor(ctx, &mut state.editor_id);
        return;
    }
    let area = ctx.available_rect();
    let draw = |ui: &mut egui::Ui| {
        #[cfg(test)]
        ui.ctx()
            .data_mut(|data| data.insert_temp(egui::Id::new("actual-notes-dock"), ui.max_rect()));
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        ui.add_enabled_ui(editing, |ui| {
            body(ui, state, session, model, selected, intents)
        });
    };
    if area.width() >= 720.0 && area.height() >= 220.0 {
        egui::SidePanel::right(SIDE_PANEL)
            .resizable(!locked && editing)
            .default_width(280.0)
            .min_width(220.0)
            .max_width((area.width() - 300.0).clamp(220.0, 360.0))
            .show(ctx, draw);
    } else {
        let maximum = (area.height() - 24.0).clamp(48.0, 240.0);
        egui::TopBottomPanel::bottom(BOTTOM_PANEL)
            .resizable(!locked && editing)
            .default_height(160.0)
            .min_height(48.0)
            .max_height(maximum)
            .show(ctx, draw);
    }
}
fn body(
    ui: &mut egui::Ui,
    state: &mut NotesDock,
    session: u64,
    model: &StudioSnapshot,
    selected: Id,
    intents: &mut Vec<Intent>,
) {
    let labels = [
        super::modal_footer::Action::new(
            "Apply notes",
            false,
            "Apply notes to the original scene. This does not save the document file.",
        ),
        super::modal_footer::Action::new(
            "Cancel edits",
            false,
            "Discard this draft and load notes for the currently selected scene.",
        ),
    ];
    let footer = super::modal_footer::required_height(ui, &labels);
    let area = ui.available_rect_before_wrap();
    if area.height() < footer + 80.0 {
        egui::ScrollArea::vertical()
            .id_salt("notes-tiny-dock-scroll")
            .auto_shrink([false, false])
            .max_height(area.height())
            .show(ui, |ui| {
                contents(ui, state, session, model, selected, intents);
                actions(ui, state, session, model, intents);
            });
        return;
    }
    let body_height = (area.height() - footer - 6.0).max(1.0);
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("notes-dock-content")
            .max_rect(egui::Rect::from_min_size(
                area.min,
                egui::vec2(area.width(), body_height),
            )),
    );
    content.set_clip_rect(content.max_rect().intersect(ui.clip_rect()));
    egui::ScrollArea::vertical()
        .id_salt("notes-dock-scroll")
        .auto_shrink([false, false])
        .max_height(body_height)
        .show(&mut content, |ui| {
            contents(ui, state, session, model, selected, intents)
        });
    let footer_rect = egui::Rect::from_min_max(
        egui::pos2(area.left(), (area.bottom() - footer).max(area.top())),
        area.max,
    );
    let mut buttons = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("notes-dock-footer")
            .max_rect(footer_rect),
    );
    buttons.set_clip_rect(footer_rect.intersect(ui.clip_rect()));
    actions(&mut buttons, state, session, model, intents);
    ui.allocate_rect(area, egui::Sense::hover());
}
fn contents(
    ui: &mut egui::Ui,
    state: &mut NotesDock,
    session: u64,
    model: &StudioSnapshot,
    selected: Id,
    intents: &mut Vec<Intent>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.strong(crate::i18n::tr(ui, "Scene Notes Dock"));
        if ui
            .small_button(crate::i18n::tr(ui, "Hide"))
            .on_hover_text(crate::i18n::tr(
                ui,
                "Hide this dock without applying or discarding its draft.",
            ))
            .on_disabled_hover_text(crate::i18n::tr(
                ui,
                "Hide this dock without applying or discarding its draft.",
            ))
            .clicked()
        {
            intents.push(Intent::ToggleDock(super::Dock::SceneNotes));
        }
    });
    ui.small(crate::i18n::tr(ui,"Plain text only"))
            .on_hover_text(crate::i18n::tr(ui,"This port supports plain text. HTML, external files and automatic scrolling are not implemented."));
    let diagnostic = state.current(session, model).err();
    if let Some(draft) = &mut state.draft {
        let label = crate::i18n::tf(ui, "Scene: {name}", &[("{name}", draft.name.clone())]);
        let owner_label = ui
            .add(egui::Label::new(label.clone()).truncate())
            .on_hover_text(label);
        if draft.dirty() && (draft.scene != selected || draft.token.session != session) {
            ui.label(crate::i18n::tr(
                ui,
                "Selection changed. This draft still belongs to the scene named above.",
            ));
        }
        if let Some(error) = diagnostic {
            ui.colored_label(ui.visuals().warn_fg_color, crate::i18n::tr(ui, error));
        }
        let editor_id = egui::Id::new((
            "scene-notes-dock-editor",
            draft.token.session,
            draft.token.generation,
            draft.scene.get(),
        ));
        use_editor(ui.ctx(), &mut state.editor_id, editor_id);
        let mut buffer = NotesText::new(&mut draft.text);
        let response=ui.add(egui::TextEdit::multiline(&mut buffer).id(editor_id)
                .desired_width(f32::INFINITY).desired_rows(4).hint_text(crate::i18n::tr(ui,"Notes for this scene")))
                .labelled_by(owner_label.id)
                .on_hover_text(crate::i18n::tr(ui,"Edit plain-text cues for this scene. Apply notes commits them; Cancel edits discards the draft."))
                .on_disabled_hover_text(crate::i18n::tr(ui,"Finish the active operation before editing scene notes."));
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(egui::Id::new("scene-notes-editor-rect"), response.rect)
        });
        let changed = response.changed();
        if buffer.rejected {
            state.error = Some(INPUT_LIMIT);
        } else if changed && state.error == Some(INPUT_LIMIT) {
            state.error = None;
        }
    }
    if let Some(error) = state.error {
        ui.colored_label(ui.visuals().error_fg_color, crate::i18n::tr(ui, error));
    }
}
fn actions(
    ui: &mut egui::Ui,
    state: &mut NotesDock,
    session: u64,
    model: &StudioSnapshot,
    intents: &mut Vec<Intent>,
) {
    let validation = state.current(session, model);
    let dirty = state.draft.as_ref().is_some_and(Draft::dirty);
    let can_apply = validation.is_ok() && dirty;
    let apply_hint = validation.err().unwrap_or(if dirty {
        "Apply notes to the original scene. This does not save the document file."
    } else {
        "No change"
    });
    let actions = [
        super::modal_footer::Action::new("Apply notes", can_apply, apply_hint),
        super::modal_footer::Action::new(
            "Cancel edits",
            state.draft.is_some(),
            "Discard this draft and load notes for the currently selected scene.",
        ),
    ];
    let action = super::modal_footer::show(ui, &actions);
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        for (index, id) in [
            (0_usize, "scene-notes-apply-rect"),
            (1_usize, "scene-notes-cancel-rect"),
        ] {
            if let Some(rect) =
                data.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", index)))
            {
                data.insert_temp(egui::Id::new(id), rect);
            }
        }
    });
    if let Some(action) = action {
        if action == 0 {
            match state.submission(session, model) {
                Ok(submission) => intents.push(Intent::ApplyNotesDock(submission)),
                Err(error) => intents.push(Intent::DialogError(error.into())),
            }
        } else if let Some(draft) = &state.draft {
            intents.push(Intent::CancelNotesDock(draft.token));
        }
    }
}

#[cfg(test)]
#[path = "scene_notes_dock_tests.rs"]
mod tests;
