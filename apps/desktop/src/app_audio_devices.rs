//! Live edits are collection commands; the profile settings draft is separate.
use super::*;
use crate::views::global_audio_devices::{GlobalAudioDraft, LiveGlobalAudioEdit};
impl StudioApp {
    pub(super) fn open_global_audio_devices(&mut self, context: &egui::Context) {
        self.dialog = Some(Dialog::GlobalAudioDevices(GlobalAudioDraft::new(
            self.session.identity(),
            self.platform,
        )));
        context.request_repaint();
    }
    pub(super) fn live_global_audio(&mut self, edit: LiveGlobalAudioEdit, context: &egui::Context) {
        let error=if edit.session!=self.session.identity()||!matches!(&self.dialog,Some(Dialog::GlobalAudioDevices(draft))if draft.session==edit.session) {
            Some("This audio dialog belongs to another document. Close and reopen it.")
        }else if edit.revision!=self.session.snapshot().revision() {
            Some("The audio settings changed. Review their current values and try again.")
        }else if edit.source.is_some_and(|id|!self.session.snapshot().source(id).is_some_and(|source|matches!(source.kind(),skylive_core::SourceKind::AudioCapture{binding}if binding.flow()==edit.slot.flow()&&self.platform.audio(edit.slot.flow(),binding.target())))) {
            Some("This audio source is no longer available for this global slot.")
        }else {None};
        if let Some(error) = error {
            if let Some(Dialog::GlobalAudioDevices(draft)) = &mut self.dialog {
                draft.error = Some(error.into());
            }
            self.message(true, error);
            return;
        }
        let mut dialog = self.dialog.take();
        self.apply_at(
            Some(edit.revision),
            Command::SetGlobalAudio {
                slot: edit.slot,
                source: edit.source,
            },
        );
        if let Some(Dialog::GlobalAudioDevices(draft)) = &mut dialog {
            draft.error = self.error.then(|| self.status.clone());
        }
        self.dialog = dialog;
        context.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylive_core::{AudioCaptureBinding, AudioFlow, AudioSlot, AudioTarget};
    fn frame(
        app: &mut StudioApp,
        context: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 760.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| app.frame_at(ctx, MediaTime(0)),
        )
    }
    fn click(app: &mut StudioApp, context: &egui::Context, pos: egui::Pos2) {
        for pressed in [true, false] {
            frame(
                app,
                context,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }
    fn rect(context: &egui::Context, id: egui::Id) -> egui::Rect {
        context.data(|data| data.get_temp::<egui::Rect>(id).unwrap())
    }
    fn settle(app: &mut StudioApp, context: &egui::Context) {
        for _ in 0..3 {
            frame(app, context, vec![]);
        }
    }
    #[test]
    fn actual_global_audio_slot_picker_publishes_once_retains_close_only_inspector_and_undo() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        app.apply(Command::AddAudioCaptureSource {
            scene: app.session.preview(),
            name: "Global microphone".into(),
            binding: AudioCaptureBinding::new(AudioFlow::Input, AudioTarget::Default, false, false)
                .unwrap(),
        });
        let source = app.session.snapshot().sources()[0].id();
        let before = app.session.snapshot();
        let format = before.audio();
        let profile = before.profiles().clone();
        app.intent(Intent::OpenGlobalAudioDevices, &context);
        settle(&mut app, &context);
        let pos = rect(
            &context,
            egui::Id::new(("global-audio-choice", AudioSlot::Mic1)),
        )
        .center();
        click(&mut app, &context, pos);
        settle(&mut app, &context);
        let pos = rect(
            &context,
            egui::Id::new(("global-audio-option", AudioSlot::Mic1, Some(source))),
        )
        .center();
        click(&mut app, &context, pos);
        assert_eq!(
            app.session.snapshot().global_audio().len(),
            1,
            "actual choice must publish a slot; status={}, error={}, dialog_open={}",
            app.status,
            app.error,
            app.dialog.is_some()
        );
        assert_eq!(app.session.snapshot().global_audio()[0].source(), source);
        assert_eq!(app.session.snapshot().revision(), before.revision() + 1);
        assert!(matches!(app.dialog, Some(Dialog::GlobalAudioDevices(_))));
        assert_eq!(app.session.snapshot().audio(), format);
        assert_eq!(app.session.snapshot().profiles(), &profile);
        settle(&mut app, &context);
        let pos = rect(&context, egui::Id::new(("modal-action-rect", 0))).center();
        click(&mut app, &context, pos);
        assert!(app.dialog.is_none());
        assert_eq!(
            app.session.snapshot().global_audio().len(),
            1,
            "Close keeps confirmed live edits"
        );
        app.intent(Intent::Command(Command::Undo), &context);
        assert!(app.session.snapshot().global_audio().is_empty());
        assert_eq!(app.session.snapshot().sources(), before.sources());
        app.intent(Intent::Command(Command::Redo), &context);
        assert_eq!(app.session.snapshot().global_audio()[0].source(), source);
        app.intent(Intent::OpenGlobalAudioDevices, &context);
        settle(&mut app, &context);
        let pos = rect(
            &context,
            egui::Id::new(("global-audio-choice", AudioSlot::Mic1)),
        )
        .center();
        click(&mut app, &context, pos);
        settle(&mut app, &context);
        let pos = rect(
            &context,
            egui::Id::new(("global-audio-option", AudioSlot::Mic1, None::<Id>)),
        )
        .center();
        click(&mut app, &context, pos);
        assert!(app.session.snapshot().global_audio().is_empty());
        assert!(matches!(app.dialog, Some(Dialog::GlobalAudioDevices(_))));
    }
    #[test]
    fn global_audio_live_commands_reject_stale_foreign_and_wrong_flow_without_mutation() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        for (name, target) in [
            ("Native", AudioTarget::Default),
            (
                "Foreign",
                AudioTarget::MacDevice {
                    uid: "portable-device".into(),
                },
            ),
        ] {
            app.apply(Command::AddAudioCaptureSource {
                scene: app.session.preview(),
                name: name.into(),
                binding: AudioCaptureBinding::new(AudioFlow::Input, target, false, false).unwrap(),
            });
        }
        app.intent(Intent::OpenGlobalAudioDevices, &context);
        let before = app.session.snapshot();
        for (revision, slot, source) in [
            (
                before.revision() - 1,
                AudioSlot::Mic1,
                before.sources()[0].id(),
            ),
            (
                before.revision(),
                AudioSlot::Desktop1,
                before.sources()[0].id(),
            ),
            (before.revision(), AudioSlot::Mic1, before.sources()[1].id()),
        ] {
            app.intent(
                Intent::LiveGlobalAudio(LiveGlobalAudioEdit {
                    session: app.session.identity(),
                    revision,
                    slot,
                    source: Some(source),
                }),
                &context,
            );
            assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
            assert!(
                matches!(&app.dialog,Some(Dialog::GlobalAudioDevices(draft))if draft.error.is_some())
            );
        }
    }
    #[test]
    fn audio_settings_device_navigation_requires_confirmed_format_and_cancel_has_no_live_side_effects()
     {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let before = app.session.snapshot();
        let mut dialog = Dialog::audio_settings(&before.audio());
        if let Dialog::AudioSettings { sample_rate, .. } = &mut dialog {
            *sample_rate = 44100;
        }
        app.intent(Intent::OpenDialog(dialog), &context);
        settle(&mut app, &context);
        let (button, enabled) = context.data(|data| {
            data.get_temp::<(egui::Rect, bool)>(egui::Id::new("settings-audio-devices"))
                .unwrap()
        });
        assert!(!enabled);
        click(&mut app, &context, button.center());
        assert!(matches!(
            app.dialog,
            Some(Dialog::AudioSettings {
                sample_rate: 44100,
                ..
            })
        ));
        let cancel = rect(&context, egui::Id::new(("modal-action-rect", 1))).center();
        click(&mut app, &context, cancel);
        assert!(app.dialog.is_none());
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        app.intent(
            Intent::OpenDialog(Dialog::audio_settings(&before.audio())),
            &context,
        );
        settle(&mut app, &context);
        let (button, enabled) = context.data(|data| {
            data.get_temp::<(egui::Rect, bool)>(egui::Id::new("settings-audio-devices"))
                .unwrap()
        });
        assert!(enabled);
        click(&mut app, &context, button.center());
        assert!(matches!(app.dialog, Some(Dialog::GlobalAudioDevices(_))));
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
    }
}
