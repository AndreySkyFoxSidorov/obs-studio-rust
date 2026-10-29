//! Window-local keyboard adapter. Modal/text entry blocks invocation in app.rs.
use crate::views::{Intent, RecordingViewState};
use eframe::egui;
use skylive_core::{Command, HotkeyAction, HotkeyBindings, HotkeyChord};

pub fn chord(key: egui::Key, modifiers: egui::Modifiers) -> Result<HotkeyChord, String> {
    if modifiers.ctrl && (!modifiers.command || modifiers.mac_cmd)
        || modifiers.mac_cmd && !modifiers.command
    {
        return Err(
            "Use the primary modifier (Ctrl on Windows, Command on macOS), Alt or Shift.".into(),
        );
    }
    let name = match key {
        egui::Key::ArrowDown => "ArrowDown",
        egui::Key::ArrowLeft => "ArrowLeft",
        egui::Key::ArrowRight => "ArrowRight",
        egui::Key::ArrowUp => "ArrowUp",
        _ => key.name(),
    };
    HotkeyChord::new(name, modifiers.command, modifiers.alt, modifiers.shift)
        .map_err(|error| error.to_string())
}
pub fn label(chord: &HotkeyChord) -> String {
    let mut parts = Vec::with_capacity(4);
    if chord.primary() {
        parts.push(if cfg!(target_os = "macos") {
            "Command"
        } else {
            "Ctrl"
        });
    }
    if chord.alt() {
        parts.push("Alt");
    }
    if chord.shift() {
        parts.push("Shift");
    }
    parts.push(chord.key());
    parts.join("+")
}
pub fn keyboard_reserved(context: &egui::Context) -> bool {
    let (focused, popup) = context.memory(|memory| (memory.focused(), memory.any_popup_open()));
    popup || focused.is_some_and(|id| egui::TextEdit::load_state(context, id).is_some())
}
pub fn collect(
    context: &egui::Context,
    bindings: &HotkeyBindings,
    studio: bool,
    transition_active: bool,
    recording: RecordingViewState,
    can_undo: bool,
    can_redo: bool,
) -> Vec<Intent> {
    let mut actions = Vec::with_capacity(HotkeyAction::ALL.len());
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
            let Ok(chord) = chord(*key, *modifiers) else {
                return true;
            };
            let Some(binding) = bindings
                .bindings()
                .iter()
                .find(|binding| binding.chord == chord)
            else {
                return true;
            };
            if *repeat && !matches!(binding.action, HotkeyAction::Undo | HotkeyAction::Redo) {
                return false;
            }
            if !actions.contains(&binding.action) {
                actions.push(binding.action);
            }
            false
        })
    });
    actions.into_iter().filter_map(|action|action_intent(action,studio,transition_active,recording.clone(),can_undo,can_redo)).collect()
}
/// Local and OS-global adapters call the same state-aware output routes.
pub fn action_intent(action:HotkeyAction,studio:bool,transition_active:bool,recording:RecordingViewState,can_undo:bool,can_redo:bool)->Option<Intent>{
    match action {
            HotkeyAction::SourcePlayback{..}|HotkeyAction::SourceAudio{..}=>None,
            HotkeyAction::Undo => can_undo.then_some(Intent::Command(Command::Undo)),
            HotkeyAction::Redo => can_redo.then_some(Intent::Command(Command::Redo)),
            HotkeyAction::Save => Some(Intent::Save),
            HotkeyAction::ToggleStudio => Some(Intent::StudioMode(!studio)),
            HotkeyAction::Transition => {
                (studio && !transition_active).then_some(Intent::Transition)
            }
            HotkeyAction::QuickCut => (studio && !transition_active).then_some(Intent::Cut),
            HotkeyAction::StartRecording => {
                (recording.available && !recording.active && !recording.finishing)
                    .then_some(Intent::StartRecording)
            }
            HotkeyAction::StopRecording => {
                (recording.active && !recording.finishing).then_some(Intent::StopRecording)
            }
            HotkeyAction::PauseRecording|HotkeyAction::ResumeRecording=>{
                let paused=action==HotkeyAction::PauseRecording;
                (recording.active&&!recording.starting&&!recording.finishing&&!recording.pause_pending
                    &&recording.recording!=0&&recording.session!=0&&recording.paused!=paused)
                    .then_some(Intent::SetRecordingPaused {recording:recording.recording,session:recording.session,paused})
            }
            // The same frontend handlers as Controls own availability,
            // configuration and active/pending/stop admission. No output or OS
            // call is started from keyboard collection itself.
            HotkeyAction::StartStreaming=>cfg!(target_os="windows").then_some(Intent::StartStreaming),
            HotkeyAction::StopStreaming=>cfg!(target_os="windows").then_some(Intent::StopStreaming),
            HotkeyAction::StartReplayBuffer=>skylive_host::replay_buffer::available().then_some(Intent::StartReplayBuffer),
            HotkeyAction::StopReplayBuffer=>skylive_host::replay_buffer::available().then_some(Intent::StopReplayBuffer),
            HotkeyAction::SaveReplayBuffer=>skylive_host::replay_buffer::available().then_some(Intent::SaveReplayBuffer),
            HotkeyAction::StartVirtualCamera=>cfg!(all(target_os="windows",target_arch="x86_64")).then_some(Intent::StartVirtualCamera),
            HotkeyAction::StopVirtualCamera=>cfg!(all(target_os="windows",target_arch="x86_64")).then_some(Intent::StopVirtualCamera),
            HotkeyAction::ScreenshotOutput=>cfg!(target_os="windows").then_some(Intent::ScreenshotHotkey {selected:false}),
            HotkeyAction::ScreenshotSelectedSource=>cfg!(target_os="windows").then_some(Intent::ScreenshotHotkey {selected:true}),

    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn button_focus_is_not_text_entry_and_assigned_repeats_cannot_nudge_preview() {
        let context = egui::Context::default();
        let mut text = String::new();
        let _ = context.run(Default::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                ui.button("Focused action").request_focus();
            });
        });
        assert!(context.wants_keyboard_input());
        assert!(!keyboard_reserved(&context));
        let _ = context.run(Default::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut text).id(egui::Id::new("shortcut-text-entry")),
                )
                .request_focus();
            });
        });
        assert!(keyboard_reserved(&context));
        assert!(
            chord(
                egui::Key::K,
                egui::Modifiers {
                    ctrl: true,
                    command: true,
                    mac_cmd: true,
                    ..Default::default()
                }
            )
            .is_err()
        );
        let context = egui::Context::default();
        let bindings = HotkeyBindings::new(vec![skylive_core::HotkeyBinding {
            action: HotkeyAction::StartRecording,
            chord: HotkeyChord::new("ArrowRight", false, false, false).unwrap(),
        }])
        .unwrap();
        for repeat in [false, true] {
            let _ = context.run(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::ArrowRight,
                        physical_key: None,
                        pressed: true,
                        repeat,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |context| {
                    let intents = collect(
                        context,
                        &bindings,
                        false,
                        false,
                        RecordingViewState {
                            available: true,
                            ..Default::default()
                        },
                        false,
                        false,
                    );
                    assert_eq!(intents.len(), usize::from(!repeat));
                    assert!(
                        !context.input(|input| input.key_pressed(egui::Key::ArrowRight)),
                        "assigned repeat must not fall through to Preview nudging"
                    );
                },
            );
        }
    }
    #[test]
    fn quick_cut_requires_idle_studio_state() {
        let bindings = HotkeyBindings::new(vec![skylive_core::HotkeyBinding {
            action: HotkeyAction::QuickCut,
            chord: HotkeyChord::new("F10", false, false, false).unwrap(),
        }])
        .unwrap();
        for (studio, active, expected) in [(false, false, 0), (true, true, 0), (true, false, 1)] {
            let context = egui::Context::default();
            let _ = context.run(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::F10,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |context| {
                    let intents = collect(
                        context,
                        &bindings,
                        studio,
                        active,
                        RecordingViewState::default(),
                        false,
                        false,
                    );
                    assert_eq!(intents.len(), expected);
                },
            );
        }
    }
    #[test]
    fn exact_modifiers_repeat_and_recording_capability_select_real_intents() {
        let context = egui::Context::default();
        let mut result = Vec::new();
        let _ = context.run(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Z,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                }],
                ..Default::default()
            },
            |context| {
                result = collect(
                    context,
                    &HotkeyBindings::default(),
                    false,
                    false,
                    RecordingViewState::default(),
                    true,
                    true,
                );
            },
        );
        assert!(matches!(
            result.as_slice(),
            [Intent::Command(Command::Redo)]
        ));
        let binding = skylive_core::HotkeyBinding {
            action: HotkeyAction::StartRecording,
            chord: HotkeyChord::new("F9", false, false, false).unwrap(),
        };
        let bindings = HotkeyBindings::new(vec![binding]).unwrap();
        for (repeat, available, expected) in [(false, false, 0), (true, true, 0), (false, true, 1)]
        {
            // Egui derives repeat from the held-key state; a fresh press with
            // a forged repeat flag is correctly normalized to nonrepeat.
            let context = egui::Context::default();
            if repeat {
                let _ = context.run(
                    egui::RawInput {
                        events: vec![egui::Event::Key {
                            key: egui::Key::F9,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        }],
                        ..Default::default()
                    },
                    |_| {},
                );
            }
            let _ = context.run(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::F9,
                        physical_key: None,
                        pressed: true,
                        repeat,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |context| {
                    result = collect(
                        context,
                        &bindings,
                        false,
                        false,
                        RecordingViewState {
                            available,
                            ..Default::default()
                        },
                        false,
                        false,
                    );
                },
            );
            assert_eq!(result.len(), expected);
            if expected == 1 {
                assert!(matches!(result[0], Intent::StartRecording));
            }
        }
    }
}
