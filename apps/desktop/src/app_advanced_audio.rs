//! Live scalar edits retain their inspector while canonical validation remains
//! in Session. A stale control never writes against a replacement document.
use super::*;
use crate::views::advanced_audio::{AdvancedAudioDraft, LiveAudioEdit, LiveAudioPropertyEdit};
impl StudioApp {
    pub(super) fn open_advanced_audio(&mut self, context: &egui::Context) {
        self.dialog = Some(Dialog::AdvancedAudio(AdvancedAudioDraft::new(
            self.session.identity(),
        )));
        context.request_repaint();
    }
    pub(super) fn live_audio_monitoring(&mut self,session:u64,revision:u64,source:Id,mode:skylive_core::AudioMonitoringMode,context:&egui::Context){
        if !self.advanced_audio_target(session,revision,source){return;}
        if !mode.is_off()&&(self.platform!=crate::platform::CapturePlatform::Windows||!self.audio.monitoring_available()){self.advanced_audio_error("Windows audio monitoring backend is unavailable");return;}
        self.publish_advanced_audio(revision,Command::SetSourceAudioMonitoring {source,mode},context);
    }
    pub(super) fn live_audio_edit(&mut self, edit: LiveAudioEdit, context: &egui::Context) {
        if !self.advanced_audio_target(edit.session, edit.revision, edit.source) {
            return;
        }
        self.publish_advanced_audio(
            edit.revision,
            Command::SetAudio {
                source: edit.source,
                gain: edit.gain,
                muted: edit.muted,
            },
            context,
        );
    }
    pub(super) fn live_audio_properties(
        &mut self,
        edit: LiveAudioPropertyEdit,
        context: &egui::Context,
    ) {
        if !self.advanced_audio_target(edit.session, edit.revision, edit.source) {
            return;
        }
        let Some(push)=self.session.snapshot().source(edit.source).map(|source|source.audio_properties().push()) else {return;};
        match skylive_core::SourceAudioProperties::new(edit.force_mono, edit.balance).and_then(|properties|properties.with_sync_offset_ms(edit.sync_offset_ms)).map(|properties|properties.with_audio_tracks(edit.audio_tracks).with_push(push)) {
            Ok(properties) => self.publish_advanced_audio(
                edit.revision,
                Command::SetSourceAudioProperties {
                    source: edit.source,
                    properties,
                },
                context,
            ),
            Err(error) => self.advanced_audio_error(error.to_string()),
        }
    }
    fn advanced_audio_target(&mut self, session: u64, revision: u64, source: Id) -> bool {
        if session != self.session.identity()
            || !matches!(&self.dialog,Some(Dialog::AdvancedAudio(draft))if draft.session==session)
        {
            self.advanced_audio_error(
                "This audio dialog belongs to another document. Close and reopen it.",
            );
            return false;
        }
        let model = self.session.snapshot();
        if model.revision() != revision {
            self.advanced_audio_error(
                "The audio settings changed. Review their current values and try again.",
            );
            return false;
        }
        if !model
            .source(source)
            .is_some_and(|source| source.kind().has_audio())
        {
            self.advanced_audio_error("This audio source is no longer available.");
            return false;
        }
        true
    }
    fn publish_advanced_audio(&mut self, revision: u64, command: Command, context: &egui::Context) {
        let mut dialog = self.dialog.take();
        self.apply_at(Some(revision), command);
        if let Some(Dialog::AdvancedAudio(draft)) = &mut dialog {
            draft.error = self.error.then(|| self.status.clone());
        }
        self.dialog = dialog;
        context.request_repaint();
    }
    fn advanced_audio_error(&mut self, error: impl Into<crate::status::Status>) {
        let error = error.into();
        if let Some(Dialog::AdvancedAudio(draft)) = &mut self.dialog {
            draft.error = Some(error.clone().into_parts().0);
        }
        self.message(true, error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn text(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(value) if value.galley.text() == label => {
                Some(value.pos + value.galley.size() * 0.5)
            }
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|shape| text(shape, label)),
            _ => None,
        }
    }
    fn click_at(app: &mut StudioApp, context: &egui::Context, pos: egui::Pos2) {
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
    fn click(app: &mut StudioApp, context: &egui::Context, label: &str) {
        frame(app, context, vec![]);
        let output = frame(app, context, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| {
                text(&shape.shape, label).filter(|pos| shape.clip_rect.contains(*pos))
            })
            .expect("Advanced Audio control must be visibly reachable");
        click_at(app, context, pos);
    }
    fn point(context: &egui::Context, kind: &str, source: Id) -> egui::Pos2 {
        context
            .data_mut(|data| data.get_temp::<egui::Rect>(egui::Id::new((kind, source.get()))))
            .expect("actual live audio widget rect")
            .center()
    }
    fn type_volume(app: &mut StudioApp, context: &egui::Context, source: Id, value: &str) {
        frame(app, context, vec![]);
        let pos = point(context, "advanced-audio-volume", source);
        click_at(app, context, pos);
        frame(
            app,
            context,
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        command: true,
                        ..Default::default()
                    },
                },
                egui::Event::Text(value.into()),
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    fn fixture(context: &egui::Context) -> (StudioApp, Id, Id) {
        let mut app = StudioApp::headless(context).unwrap();
        let scene = app.session.preview();
        app.apply(Command::AddAudioCaptureSource {
            scene,
            name: "Voice".into(),
            binding: skylive_core::AudioCaptureBinding::new(
                skylive_core::AudioFlow::Input,
                skylive_core::AudioTarget::Default,
                false,
                false,
            )
            .unwrap(),
        });
        let audio = app.session.snapshot().sources()[0].id();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/host/tests/fixtures/media/cavlc.mp4");
        app.apply(Command::AddMediaSource {
            scene,
            name: "Movie AAC".into(),
            media: skylive_core::MediaSource::new(
                path.to_string_lossy().into_owned(),
                64,
                48,
                true,
            )
            .unwrap(),
        });
        let media = app.session.snapshot().sources()[1].id();
        (app, audio, media)
    }
    #[test]
    fn real_advanced_audio_widgets_mute_and_type_db_percent_keep_inspector_and_one_undo() {
        let context = egui::Context::default();
        let (mut app, audio, media) = fixture(&context);
        let before = app.session.snapshot();
        // The catalog and the mixer use the same supported frontend action.
        assert!(matches!(
            views::menus::action_intent("actionAdvAudioProperties", &app.session, None, false),
            Some(Intent::OpenAdvancedAudio)
        ));
        app.intent(Intent::OpenAdvancedAudio, &context);
        frame(&mut app, &context, vec![]);
        frame(&mut app, &context, vec![]);
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        let pos = point(&context, "advanced-audio-mute", audio);
        click_at(&mut app, &context, pos);
        assert!(matches!(&app.dialog, Some(Dialog::AdvancedAudio(_))));
        assert!(app.session.snapshot().source(audio).unwrap().muted());
        assert!(!app.session.snapshot().source(media).unwrap().muted());
        assert_eq!(app.session.snapshot().revision(), before.revision() + 1);
        click(&mut app, &context, "Close");
        assert!(app.dialog.is_none());
        frame(
            &mut app,
            &context,
            vec![egui::Event::Key {
                key: egui::Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            }],
        );
        assert!(!app.session.snapshot().source(audio).unwrap().muted());
        assert_eq!(
            app.session.snapshot().sources().len(),
            2,
            "one Undo restores the mute rather than removing a source"
        );
        app.intent(Intent::OpenAdvancedAudio, &context);
        type_volume(&mut app, &context, media, "-6.0206");
        assert!(matches!(&app.dialog, Some(Dialog::AdvancedAudio(_))));
        assert!((app.session.snapshot().source(media).unwrap().gain() - 0.5).abs() < 0.00001);
        assert_eq!(app.session.snapshot().source(audio).unwrap().gain(), 1.0);
        let db = app.session.snapshot();
        click(&mut app, &context, "%");
        assert!(
            Arc::ptr_eq(&db, &app.session.snapshot()),
            "display units are presentation state"
        );
        type_volume(&mut app, &context, media, "25");
        assert!((app.session.snapshot().source(media).unwrap().gain() - 0.25).abs() < 0.00001);
        assert!(matches!(&app.dialog, Some(Dialog::AdvancedAudio(_))));
        let confirmed = app.session.snapshot();
        frame(&mut app, &context, vec![]);
        let pos = point(&context, "advanced-audio-volume", media);
        click_at(&mut app, &context, pos);
        frame(
            &mut app,
            &context,
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        command: true,
                        ..Default::default()
                    },
                },
                egui::Event::Text("90".into()),
            ],
        );
        click(&mut app, &context, "Close");
        assert!(app.dialog.is_none());
        assert!(
            Arc::ptr_eq(&confirmed, &app.session.snapshot()),
            "closing must not publish an unconfirmed typed value"
        );
    }
    #[test]
    fn real_mono_checkbox_keeps_live_dialog_and_one_undo_preserves_source_binding() {
        let context = egui::Context::default();
        let (mut app, audio, _) = fixture(&context);
        let before = app.session.snapshot();
        let binding = before.source(audio).unwrap().kind().clone();
        app.intent(Intent::OpenAdvancedAudio, &context);
        frame(&mut app, &context, vec![]);
        frame(&mut app, &context, vec![]);
        let pos = point(&context, "advanced-audio-mono", audio);
        click_at(&mut app, &context, pos);
        assert!(matches!(&app.dialog, Some(Dialog::AdvancedAudio(_))));
        assert!(
            app.session
                .snapshot()
                .source(audio)
                .unwrap()
                .audio_properties()
                .force_mono()
        );
        assert_eq!(app.session.snapshot().revision(), before.revision() + 1);
        assert_eq!(
            app.session.snapshot().source(audio).unwrap().kind(),
            &binding
        );
        click(&mut app, &context, "Close");
        frame(
            &mut app,
            &context,
            vec![egui::Event::Key {
                key: egui::Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..Default::default()
                },
            }],
        );
        assert!(
            !app.session
                .snapshot()
                .source(audio)
                .unwrap()
                .audio_properties()
                .force_mono()
        );
        assert_eq!(
            app.session.snapshot().source(audio).unwrap().kind(),
            &binding
        );
    }
    #[test]
    fn advanced_audio_rejects_stale_removed_and_replacement_document_controls() {
        let context = egui::Context::default();
        let (mut app, audio, _) = fixture(&context);
        app.intent(Intent::OpenAdvancedAudio, &context);
        let revision = app.session.snapshot().revision();
        let owner = app.session.identity();
        app.session
            .apply(Command::RenameScene {
                scene: app.session.preview(),
                name: "Concurrent change".into(),
            })
            .unwrap();
        let changed = app.session.snapshot();
        app.intent(
            Intent::LiveAudio(LiveAudioEdit {
                session: owner,
                revision,
                source: audio,
                gain: 0.5,
                muted: false,
            }),
            &context,
        );
        assert!(Arc::ptr_eq(&changed, &app.session.snapshot()));
        assert!(matches!(&app.dialog,Some(Dialog::AdvancedAudio(draft))if draft.error.is_some()));
        let scene = app.session.preview();
        let item = app
            .session
            .snapshot()
            .scene(scene)
            .unwrap()
            .items()
            .iter()
            .find(|item| item.source() == audio)
            .unwrap()
            .id();
        app.session
            .apply(Command::RemoveItem { scene, item })
            .unwrap();
        let removed = app.session.snapshot();
        app.intent(
            Intent::LiveAudio(LiveAudioEdit {
                session: owner,
                revision: removed.revision(),
                source: audio,
                gain: 0.5,
                muted: false,
            }),
            &context,
        );
        assert!(Arc::ptr_eq(&removed, &app.session.snapshot()));
        frame(&mut app, &context, vec![]);
        assert!(matches!(&app.dialog, Some(Dialog::AdvancedAudio(_))));
        app.session = Session::new();
        let replacement = app.session.snapshot();
        app.intent(
            Intent::LiveAudio(LiveAudioEdit {
                session: owner,
                revision: replacement.revision(),
                source: audio,
                gain: 0.5,
                muted: false,
            }),
            &context,
        );
        assert!(Arc::ptr_eq(&replacement, &app.session.snapshot()));
        assert!(app.error);
    }
}
