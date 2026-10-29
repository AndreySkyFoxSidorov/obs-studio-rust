//! Mixer UI publication. Ordinary CLI/Advanced Audio gain is not volume-locked.
use super::*;

impl StudioApp {
    pub(super) fn apply_mixer_edit(
        &mut self,
        edit: views::mixer::MixerSubmission,
        context: &egui::Context,
    ) {
        if self.session.is_closed()
            || edit.session != self.session.identity()
            || edit.revision != self.session.snapshot().revision()
            || !self.layout.mixer
            || self.close_dialog
            || self.dialog.is_some()
            || self.setup.is_open()
            || self.after_io.is_some()
            || self.io.busy()
            || self.documents.modal_open()
        {
            self.message(
                true,
                "Mixer controls changed. Review the current channel and try again.",
            );
            return;
        }
        let model = self.session.snapshot();
        let allowed = match &edit.command {
            Command::SetMixerAudio { source, gain, .. } | Command::SetAudio { source, gain, .. } => {
                model.source(*source).is_some_and(|current| {
                    current.kind().has_audio()
                        && (!current.mixer_state().volume_locked() || *gain == current.gain())
                })
            }
            Command::SetSourceAudioMonitoring {source,mode}=>model.source(*source).is_some_and(|current|current.kind().has_audio())
                && (mode.is_off() || (self.platform==crate::platform::CapturePlatform::Windows && self.audio.monitoring_available())),
            Command::SetSourceMixerState { source, .. } => model
                .source(*source)
                .is_some_and(|current| current.kind().has_audio()),
            Command::SetMixerDisplay { .. } | Command::UnhideAllMixerSources => true,
            _ => false,
        };
        if !allowed {
            self.message(
                true,
                "This mixer channel is unavailable or its volume control is locked.",
            );
            return;
        }
        self.publish_command_at(Some(edit.revision), edit.command, false);
        context.request_repaint();
    }
}
