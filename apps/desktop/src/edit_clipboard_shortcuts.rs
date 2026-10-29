//! Default window-local OBS transform chords; configured hotkeys take precedence.
use crate::edit_clipboard::{Action, Clipboard, Request};
use eframe::egui;
use skylive_core::{HotkeyChord, Id, Session};
pub fn label(action: Action, session: &Session) -> String {
    let key = match action {
        Action::CopyTransform => "C",
        Action::PasteTransform => "V",
        _ => return String::new(),
    };
    let Ok(chord) = HotkeyChord::new(key, true, false, true) else {
        return String::new();
    };
    if session
        .snapshot()
        .preferences()
        .hotkeys()
        .bindings()
        .iter()
        .any(|b| b.chord == chord)
    {
        return String::new();
    }
    crate::shortcuts::label(&chord)
}
/// Caller applies ordinary modal/text-entry/splitter guards and runs its
/// configurable shortcut collector first. No input is replayed or synthesized.
pub fn collect(
    context: &egui::Context,
    clipboard: &Clipboard,
    session: &Session,
    selected: Option<Id>,
) -> Vec<Request> {
    let mut requests = Vec::with_capacity(2);
    context.input_mut(|input| {
        input.events.retain(|event| {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } = event
            else {
                return true;
            };
            let Ok(chord) = crate::shortcuts::chord(*key, *modifiers) else {
                return true;
            };
            if !chord.primary() || !chord.shift() || chord.alt() {
                return true;
            }
            let action = match chord.key() {
                "C" => Action::CopyTransform,
                "V" => Action::PasteTransform,
                _ => return true,
            };
            if session
                .snapshot()
                .preferences()
                .hotkeys()
                .bindings()
                .iter()
                .any(|b| b.chord == chord)
            {
                return true;
            }
            if *repeat {
                return false;
            }
            if !requests.iter().any(|r: &Request| r.action == action) {
                if let Ok(request) = clipboard.request(action, session, selected) {
                    requests.push(request);
                }
            }
            false
        })
    });
    requests
}
