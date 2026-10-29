//! Fenced authoring replies. Files and decoder metadata are checked only by the
//! media owner; a delayed reply cannot replace a newer modal or authored edit.
use super::*;
use crate::views::media_source::MediaPrepare;
use skylive_host::media_source::{MediaRequestKey, MediaSourceError, PreparedMedia};

pub(super) struct PendingMedia {
    pub key: MediaRequestKey,
    pub request: MediaPrepare,
}

impl StudioApp {
    pub(super) fn prepare_media(&mut self, request: MediaPrepare, context: &egui::Context) {
        let matches =
            matches!(&self.dialog, Some(Dialog::MediaSource(draft)) if draft.matches(&request));
        if !matches
            || request.session != self.session.identity()
            || request.revision != self.session.snapshot().revision()
        {
            self.media_prepare_error(
                "The document or media properties changed. Reopen this dialog before applying."
                    .into(),
            );
            return;
        }
        if self.pending_media.is_some() || self.media.is_busy() {
            self.media_prepare_wait();
            return;
        }
        match self
            .media
            .prepare(&self.session, PathBuf::from(&request.path))
        {
            Ok(key) => {
                self.pending_media = Some(PendingMedia { key, request });
                if let Some(Dialog::MediaSource(draft)) = &mut self.dialog {
                    draft.loading = true;
                    draft.error = None;
                }
            }
            Err(MediaSourceError::Busy) => self.media_prepare_wait(),
            Err(error) => self.media_worker_error(error),
        }
        context.request_repaint();
    }
    fn media_prepare_error(&mut self, error: String) {
        if let Some(Dialog::MediaSource(draft)) = &mut self.dialog {
            draft.loading = false;
            draft.error = Some(error.clone());
        }
        self.message(true, error);
    }
    fn media_prepare_wait(&mut self) {
        if let Some(Dialog::MediaSource(draft)) = &mut self.dialog {
            draft.worker_busy = true;
            draft.error = None;
        }
        let message = crate::status::Status::from(views::media_source::WAIT_MEDIA);
        self.message(false, message);
    }
    fn media_worker_error(&mut self, error: MediaSourceError) {
        let message = crate::i18n::tf_catalog(
            &self.localization,
            "The media file could not be checked. Verify its path, format and availability. Details: {error}",
            &[("{error}", error.to_string())],
        );
        self.media_prepare_error(message);
        self.message(true, crate::status::Status::formatted(
            "The media file could not be checked. Verify its path, format and availability. Details: {error}",
            &[("{error}", error.to_string())],
        ));
    }
    pub(super) fn accept_prepared_media(
        &mut self,
        key: MediaRequestKey,
        result: Result<PreparedMedia, MediaSourceError>,
    ) {
        let Some(pending) = &self.pending_media else {
            return;
        };
        if pending.key != key {
            return;
        }
        let Some(pending) = self.pending_media.take() else {
            return;
        };
        if !matches!(&self.dialog, Some(Dialog::MediaSource(draft)) if draft.matches(&pending.request))
        {
            return;
        }
        if pending.request.session != self.session.identity()
            || pending.request.revision != self.session.snapshot().revision()
        {
            self.media_prepare_error("The document changed while checking this file. Reopen the media properties and try again.".into());
            return;
        }
        match result {
            Ok(prepared) => {
                if let Some(Dialog::MediaSource(draft)) = &mut self.dialog {
                    draft.loading = false;
                }
                self.apply_at(
                    Some(pending.request.revision),
                    pending.request.command(prepared.media),
                );
            }
            Err(error) => self.media_worker_error(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::media_source::MediaDraft;
    use std::time::Duration;
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
    fn point(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|shape| point(shape, label)),
            _ => None,
        }
    }
    fn click_at(app: &mut StudioApp, context: &egui::Context, pos: egui::Pos2) {
        let pointer = |pressed| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        frame(app, context, pointer(true));
        frame(app, context, pointer(false));
    }
    fn click(app: &mut StudioApp, context: &egui::Context, label: &str) {
        // The complete app can have an outstanding Scope acknowledgement even
        // before a file exists. Four fast paint calls are not an acknowledgement:
        // on a loaded test runner the worker may not have been scheduled yet.
        // Prepare deliberately rejects Busy, so wait for its real admission gate.
        if label == "Apply" {
            settle(app, context, |app| !app.media.is_busy());
        }
        frame(app, context, vec![]);
        let output = frame(app, context, vec![]);
        let caption = output
            .shapes
            .iter()
            .find_map(|shape| {
                point(&shape.shape, label).filter(|pos| shape.clip_rect.contains(*pos))
            })
            .expect("media modal action visible");
        let pos = match label {
            "Apply" | "Cancel" => {
                let index = usize::from(label == "Cancel");
                let rect = context
                    .data(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", index)))
                    })
                    .expect("actual modal footer response recorded");
                assert!(
                    rect.contains(caption),
                    "caption remains inside its real button"
                );
                rect.center()
            }
            _ => caption,
        };
        click_at(app, context, pos);
    }
    fn settle(
        app: &mut StudioApp,
        context: &egui::Context,
        condition: impl Fn(&StudioApp) -> bool,
    ) {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            frame(app, context, vec![]);
            if condition(app) {
                return;
            }
            assert!(
                Instant::now() < until,
                "media reply timed out: {}",
                app.status
            );
            std::thread::yield_now();
        }
    }
    fn fixture() -> String {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/host/tests/fixtures/media/cavlc.mp4")
            .to_string_lossy()
            .into_owned()
    }
    fn open(app: &mut StudioApp, context: &egui::Context, path: String) {
        let mut draft = MediaDraft::add(app.session.preview(), "Movie".into());
        draft.path = path;
        app.intent(Intent::OpenDialog(Dialog::MediaSource(draft)), context);
    }
    #[test]
    fn real_modal_prepares_file_off_frame_preserves_failed_draft_then_adds_one_undoable_media_source()
     {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let before = app.session.snapshot();
        let missing = std::env::temp_dir()
            .join(format!(
                "skylive-missing-media-{}.mp4",
                app.session.identity()
            ))
            .to_string_lossy()
            .into_owned();
        open(&mut app, &context, missing.clone());
        click(&mut app, &context, "Apply");
        assert!(
            app.pending_media.is_some(),
            "Apply was not admitted: {}",
            app.status
        );
        settle(&mut app, &context, |app| app.pending_media.is_none());
        assert!(app.error);
        assert!(
            matches!(&app.dialog,Some(Dialog::MediaSource(draft))if draft.path==missing&&!draft.loading&&draft.error.is_some())
        );
        assert!(Arc::ptr_eq(&before, &app.session.snapshot()));
        frame(&mut app, &context, vec![]);
        let rect = context
            .data_mut(|data| data.get_temp::<egui::Rect>(egui::Id::new("media-source-path-rect")))
            .unwrap();
        click_at(&mut app, &context, rect.center());
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
                egui::Event::Text(fixture()),
            ],
        );
        click(&mut app, &context, "Loop");
        click(&mut app, &context, "Apply");
        assert!(
            app.pending_media.is_some(),
            "Apply was not admitted: {}",
            app.status
        );
        settle(&mut app, &context, |app| app.pending_media.is_none());
        assert!(!app.error, "{}", app.status);
        assert!(app.dialog.is_none());
        let model = app.session.snapshot();
        let media = model
            .sources()
            .iter()
            .find_map(|source| {
                if let skylive_core::SourceKind::MediaSource { media } = source.kind() {
                    Some(media)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(media.video_dimensions(), Some((64, 48)));
        assert!(media.has_audio() && media.looping());
        assert_eq!(model.revision(), before.revision() + 1);
        app.apply(Command::Undo);
        assert!(app.session.snapshot().sources().is_empty());
        app.apply(Command::Redo);
        assert!(app.session.snapshot().sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::MediaSource{media}if media.looping())));
    }
    #[test]
    fn real_prepare_reply_rejects_revision_change_and_replaced_modal_without_publishing_media() {
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        open(&mut app, &context, fixture());
        click(&mut app, &context, "Apply");
        assert!(
            app.pending_media.is_some(),
            "Apply was not admitted: {}",
            app.status
        );
        app.apply(Command::RenameScene {
            scene: app.session.preview(),
            name: "Edited during validation".into(),
        });
        let after_edit = app.session.snapshot();
        settle(&mut app, &context, |app| app.pending_media.is_none());
        assert!(Arc::ptr_eq(&after_edit, &app.session.snapshot()));
        assert!(app.session.snapshot().sources().is_empty());
        // Replacing the modal also invalidates its private token, even when the
        // same path is reopened against the same canonical revision.
        open(&mut app, &context, fixture());
        click(&mut app, &context, "Apply");
        assert!(
            app.pending_media.is_some(),
            "Apply was not admitted: {}",
            app.status
        );
        let old_token = app.pending_media.as_ref().unwrap().request.token;
        open(&mut app, &context, fixture());
        assert!(matches!(&app.dialog,Some(Dialog::MediaSource(draft))if draft.token!=old_token));
        settle(&mut app, &context, |app| app.pending_media.is_none());
        assert!(Arc::ptr_eq(&after_edit, &app.session.snapshot()));
        assert!(app.session.snapshot().sources().is_empty());
        click(&mut app, &context, "Cancel");
        assert!(app.dialog.is_none());
    }
}
