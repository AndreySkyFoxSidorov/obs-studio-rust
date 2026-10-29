//! Runtime-only selected Media controls; no scene Undo or persisted transport.
use super::*;
use skylive_host::media_source::{
    MediaControl, MediaControlTarget, MediaRequestKey, MediaSourceError, MediaTransportStatus,
};
pub(super) struct PendingMediaControl {
    key: MediaRequestKey,
    target: MediaControlTarget,
}
impl StudioApp {
    pub(super) fn control_media(
        &mut self,
        target: MediaControlTarget,
        operation: MediaControl,
        context: &egui::Context,
    ) {
        if self.pending_media_control.is_some() || self.media.is_busy() {
            self.message(true, "Wait for the active media operation.");
            return;
        }
        match self
            .media
            .control(&self.session, target.clone(), operation, self.frame_now)
        {
            Ok(key) => self.pending_media_control = Some(PendingMediaControl { key, target }),
            Err(error) => self.message(true, error.to_string()),
        }
        context.request_repaint();
    }
    pub(super) fn accept_media_control(
        &mut self,
        key: MediaRequestKey,
        source: Id,
        result: Result<MediaTransportStatus, MediaSourceError>,
        context: &egui::Context,
    ) {
        if self
            .pending_media_control
            .as_ref()
            .is_none_or(|p| p.key != key || p.target.source() != source)
        {
            return;
        }
        let pending = self.pending_media_control.take().unwrap();
        if pending.target.matches(&self.session) && self.media.target_is_current(&pending.target) {
            match result {
                Ok(_) => self.message(false, "Media control applied"),
                Err(error) => self.message(true, error.to_string()),
            }
        }
        context.request_repaint();
    }
}
