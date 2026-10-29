use super::*;
use views::plugin_routes::PluginEditorToken;
use views::scene_notes_dock::NotesSubmission;

impl StudioApp {
    fn notes_dock_interactive(&self) -> bool {
        self.layout.arrangement.scene_notes_visible
            && self.dialog.is_none()
            && !self.close_dialog
            && self.after_io.is_none()
            && !self.io.busy()
            && !self.documents.modal_open()
            && !self.session.is_closed()
    }
    pub(super) fn apply_notes_dock(
        &mut self,
        submission: NotesSubmission,
        context: &egui::Context,
    ) {
        if !self.notes_dock_interactive() {
            self.message(
                true,
                "Reopen the Scene Notes dock and finish the active dialog before editing notes.",
            );
            return;
        }
        let command = match self.notes_dock.command_for(
            self.session.identity(),
            &self.session.snapshot(),
            &submission,
        ) {
            Ok(command) => command,
            Err(error) => {
                self.message(true, error);
                return;
            }
        };
        if self.publish_command_at(Some(submission.token.revision), command, false) {
            // command_for reserves this next presentation identity before Apply.
            // Canonical publication is already complete; a refresh failure must
            // never be reported as rollback or as a document-file Save.
            if let Err(error) = self.notes_dock.accepted(
                self.session.identity(),
                &self.session.snapshot(),
                self.session.preview(),
            ) {
                self.message(
                    true,
                    crate::status::Status::formatted(
                        "Scene notes were applied, but the dock could not refresh: {error}",
                        &[("{error}", error.into())],
                    ),
                );
            }
        }
        context.request_repaint();
    }
    pub(super) fn cancel_notes_dock(&mut self, token: PluginEditorToken, context: &egui::Context) {
        if !self.notes_dock_interactive() {
            self.message(
                true,
                "Reopen the Scene Notes dock and finish the active dialog before editing notes.",
            );
            return;
        }
        if let Err(error) = self.notes_dock.cancel(
            token,
            self.session.identity(),
            &self.session.snapshot(),
            self.session.preview(),
        ) {
            self.message(true, error);
        }
        context.request_repaint();
    }
}
