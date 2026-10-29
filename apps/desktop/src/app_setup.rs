use super::*;
use views::setup::{SetupAction, SetupCapabilities, SetupOrigin, SetupToken};

impl StudioApp {
    /// Owned-file helper retained for existing tests; native startup uses IoWorker.
    /// Existing or invalid
    /// files never count as first-run, including legacy preferences without metadata.
    #[cfg(test)]
    pub(super) fn load_setup_preferences(
        &mut self,
        path: &std::path::Path,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        match skylive_host::user_preferences::load(path) {
            Ok(Some(preferences)) => {
                self.session
                    .apply(Command::SetUserPreferences { preferences })?;
                self.arm_startup_process_priority();
                Ok(false)
            }
            Ok(None) => Ok(self
                .session
                .snapshot()
                .preferences()
                .setup_state()
                .is_unseen()),
            Err(error) => {
                self.message(true, error.to_string());
                Ok(false)
            }
        }
    }
    fn setup_capabilities(&self) -> SetupCapabilities {
        SetupCapabilities {
            recording: self.recording.view.available,
            streaming: self.streaming.view.available,
        }
    }
    pub(super) fn open_setup(&mut self, origin: SetupOrigin) {
        if self.recording.active()
            || self.streaming.active()
            || self.io.busy()
            || self.after_io.is_some()
            || self.close_dialog
            || self.dialog.is_some()
            || self.documents.modal_open()
        {
            self.message(
                true,
                "Stop active outputs and finish the current dialog before opening setup.",
            );
            return;
        }
        let capabilities = self.setup_capabilities();
        if let Err(error) = self.setup.open(&self.session, origin, capabilities) {
            self.message(true, error);
        }
    }
    pub(super) fn setup_action(&mut self, token: SetupToken, action: SetupAction) {
        if matches!(action, SetupAction::Apply | SetupAction::Skip)
            && (self.io.busy()
                || self.after_io.is_some()
                || self.close_dialog
                || self.dialog.is_some()
                || self.documents.modal_open())
        {
            let error = "Wait for the active document operation before editing";
            self.setup.error(error.into());
            self.message(true, error);
            return;
        }
        if (self.recording.active() || self.streaming.active())
            && !matches!(action, SetupAction::Cancel | SetupAction::Back)
        {
            self.message(true, "Stop active outputs before applying setup.");
            return;
        }
        let capabilities = self.setup_capabilities();
        match self
            .setup
            .action(token, action, &self.session, capabilities)
        {
            Ok(Some(command)) => {
                if self.publish_command_at(Some(token.revision), command, false) {
                    self.setup.close();
                } else {
                    self.setup.error(self.status.clone());
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.setup.error(error.clone());
                self.message(true, error);
            }
        }
    }
}
