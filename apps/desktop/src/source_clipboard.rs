//! Window-local weak source clipboard, shared typed paste commands.
use eframe::egui;
use skylive_core::{Command, HotkeyChord, Id, MAX_ITEMS, Session, SourcePasteItem};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Copy, PasteReference, PasteDuplicate }
impl Action {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "actionCopySource" => Some(Self::Copy),
            "actionPasteRef" => Some(Self::PasteReference),
            "actionPasteDup" => Some(Self::PasteDuplicate),
            _ => None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Request {
    action: Action,
    session: u64,
    revision: u64,
    scene: Id,
    generation: u64,
    selection: Vec<Id>,
}
#[derive(Default)]
pub struct Clipboard {
    session: Option<u64>,
    generation: u64,
    items: Vec<SourcePasteItem>,
}
pub enum Outcome { Copied, Paste { revision: u64, command: Command } }

pub fn selected(context: &egui::Context, session: &Session, focus: Option<Id>) -> Vec<Id> {
    crate::views::group_source::selected_items_context(context, &session.snapshot(), session.preview(), session.identity(), focus)
}
impl Clipboard {
    pub fn install(&self, context: &egui::Context, session: &Session) {
        context.data_mut(|data| data.insert_temp(egui::Id::new("source-clipboard-available"),
            self.session == Some(session.identity()) && !self.items.is_empty()));
    }
    pub fn synchronize(&mut self, session: &Session) {
        let before = self.items.len();
        if self.session != Some(session.identity()) {
            self.items.clear();
            self.session = None;
        } else {
            let model = session.snapshot();
            self.items.retain(|item| model.source(item.source).is_some());
        }
        if self.items.len() != before {
            if let Some(next) = self.generation.checked_add(1) { self.generation = next; }
            else { self.items.clear(); self.session = None; }
        }
    }
    pub fn request(&self, action: Action, session: &Session, selection: Vec<Id>) -> Result<Request, String> {
        if session.is_closed() { return Err("Session is closed".into()); }
        if selection.len() > MAX_ITEMS { return Err("Too many selected source items".into()); }
        let model = session.snapshot();
        if action == Action::Copy {
            if selection.is_empty() { return Err("Select a source to copy".into()); }
            for id in &selection {
                let (owner, _) = model.item_container(*id).ok_or("The selected source was removed")?;
                if model.root_scene_for_container(owner) != Some(session.preview()) {
                    return Err("Select sources from the current scene".into());
                }
            }
        } else {
            if self.session != Some(session.identity()) || self.items.is_empty() {
                return Err("Copy a source before pasting".into());
            }
            model.check_source_paste(session.preview(), &self.items, action == Action::PasteDuplicate).map_err(|error| error.to_string())?;
        }
        Ok(Request { action, session: session.identity(), revision: model.revision(), scene: session.preview(), generation: self.generation, selection })
    }
    pub fn execute(&mut self, request: Request, session: &Session, selection: Vec<Id>) -> Result<Outcome, String> {
        self.synchronize(session);
        if request.session != session.identity() || request.revision != session.snapshot().revision()
            || request.scene != session.preview() || request.generation != self.generation
            || request.selection != selection {
            return Err("The scene, selection or source clipboard changed. Repeat this action.".into());
        }
        self.request(request.action, session, selection.clone())?;
        if request.action == Action::Copy {
            let model = session.snapshot();
            // Canonical bottom-to-top order, independent of Ctrl-click order.
            let mut items = Vec::with_capacity(selection.len());
            for container in model.scenes().iter().chain(model.groups()) {
                for item in container.items() {
                    if selection.contains(&item.id()) {
                        items.push(model.source_copy_value(item.id()).map_err(|error| error.to_string())?);
                    }
                }
            }
            let generation = self.generation.checked_add(1).ok_or("Source clipboard identity exhausted")?;
            self.items = items;
            self.session = Some(session.identity());
            self.generation = generation;
            Ok(Outcome::Copied)
        } else {
            Ok(Outcome::Paste { revision: request.revision, command: Command::PasteSources {
                scene: request.scene, items: self.items.clone(), duplicate: request.action == Action::PasteDuplicate,
            } })
        }
    }
}

pub fn context_menu(ui: &mut egui::Ui, session: u64, revision: u64, item: Id, intents: &mut Vec<crate::views::Intent>) {
    let available = ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::new("source-clipboard-available"))).unwrap_or(false);
    for (action, label) in [(Action::Copy, "Copy"), (Action::PasteReference, "Paste (Reference)"), (Action::PasteDuplicate, "Paste (Duplicate)")] {
        if ui.add_enabled(action == Action::Copy || available, egui::Button::new(crate::i18n::tr(ui, label)))
            .on_hover_text(crate::i18n::tr(ui, match action {
                Action::Copy => "Copy selected sources, including their position, crop and visibility.",
                Action::PasteReference => "Add shared references to the copied sources in the current scene.",
                Action::PasteDuplicate => "Create independent copies with their current properties and filters.",
            })).on_disabled_hover_text(crate::i18n::tr(ui, "Copy a source before pasting")).clicked() {
            intents.push(crate::views::Intent::ContextSourceClipboard { action, session, revision, item });
            ui.close_menu();
        }
    }
}

/// Paste into the current scene when the Sources menu has no selected row.
pub fn empty_context_menu(ui: &mut egui::Ui, session: u64, revision: u64, scene: Id, intents: &mut Vec<crate::views::Intent>) {
    let available = ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::new("source-clipboard-available"))).unwrap_or(false);
    for (action,label,hint) in [
        (Action::PasteReference,"Paste (Reference)","Add shared references to the copied sources in the current scene."),
        (Action::PasteDuplicate,"Paste (Duplicate)","Create independent copies with their current properties and filters."),
    ] {
        if ui.add_enabled(available,egui::Button::new(crate::i18n::tr(ui,label)))
            .on_hover_text(crate::i18n::tr(ui,hint))
            .on_disabled_hover_text(crate::i18n::tr(ui,"Copy a source before pasting")).clicked() {
            intents.push(crate::views::Intent::ContextSourcePaste { action,session,revision,scene });
            ui.close_menu();
        }
    }
}

pub fn shortcut(action: Action, session: &Session) -> String {
    let key = match action { Action::Copy => "C", Action::PasteReference => "V", Action::PasteDuplicate => return String::new() };
    let chord = HotkeyChord::new(key, true, false, false).expect("fixed clipboard chord");
    if session.snapshot().preferences().hotkeys().bindings().iter().any(|binding| binding.chord == chord) {
        String::new()
    } else { crate::shortcuts::label(&chord) }
}
pub fn collect(context: &egui::Context, clipboard: &Clipboard, session: &Session, focus: Option<Id>) -> Vec<Request> {
    let mut requests = Vec::new();
    context.input_mut(|input| {
        input.events.retain(|event| {
            let egui::Event::Key { key, pressed: true, repeat, modifiers, .. } = event else { return true; };
            let Ok(chord) = crate::shortcuts::chord(*key, *modifiers) else { return true; };
            if !chord.primary() || chord.shift() || chord.alt() { return true; }
            let action = match chord.key() { "C" => Action::Copy, "V" => Action::PasteReference, _ => return true };
            if shortcut(action, session).is_empty() { return true; }
            if !*repeat && requests.is_empty() {
                if let Ok(request) = clipboard.request(action, session, selected(context, session, focus)) { requests.push(request); }
            }
            false
        });
    });
    requests
}
