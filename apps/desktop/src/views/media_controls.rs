//! Selected-source transport; immutable worker metadata, no decoder/IO/model writes.
use eframe::egui;
use skylive_core::{Id, SourceKind, StudioSnapshot};
use skylive_host::media_source::{MediaControl, MediaPlaybackState, MediaSnapshot};
#[derive(Clone, Copy)]
pub struct MediaTransportView<'a> {
    pub snapshot: &'a MediaSnapshot,
    pub busy: bool,
}
#[cfg(test)]
pub fn show(
    ui: &mut egui::Ui,
    session: u64,
    model: &StudioSnapshot,
    source: Id,
    view: MediaTransportView<'_>,
    intents: &mut Vec<super::Intent>,
) {
    show_inner(ui, session, model, source, view, intents, false);
}
pub fn show_compact(
    ui: &mut egui::Ui, session: u64, model: &StudioSnapshot, source: Id,
    view: MediaTransportView<'_>, intents: &mut Vec<super::Intent>,
) {
    show_inner(ui, session, model, source, view, intents, true);
}
fn show_inner(
    ui: &mut egui::Ui, session: u64, model: &StudioSnapshot, source: Id,
    view: MediaTransportView<'_>, intents: &mut Vec<super::Intent>, compact: bool,
) {
    let Some(SourceKind::MediaSource { media }) = model.source(source).map(|s| s.kind()) else {
        return;
    };
    let target = view
        .snapshot
        .control_target_for(session, model.revision(), source, media);
    let status = target.as_ref().ok().map(|t| t.status());
    let state = status.map(|s| s.state);
    let state_label = match state {
            Some(MediaPlaybackState::Playing) => "Playing",
            Some(MediaPlaybackState::Paused) => "Paused",
            Some(MediaPlaybackState::Stopped) => "Stopped",
            Some(MediaPlaybackState::Ended) => "Ended",
            Some(MediaPlaybackState::Failed) => "Failed",
            None => "Waiting for media playback state…",
    };
    let first = match state {
        Some(MediaPlaybackState::Playing) => ("Pause", MediaControl::Pause),
        Some(MediaPlaybackState::Paused) => ("Play", MediaControl::Resume),
        _ => ("Play", MediaControl::Restart),
    };
    if !compact { ui.label(crate::i18n::tr(ui, state_label)); }
    ui.horizontal_wrapped(|ui| {
        if compact { ui.label(crate::i18n::tr(ui, state_label)); }
        for (label, operation, hint) in [
            (
                first.0,
                first.1,
                "Play or pause this shared Media Source without changing the scene history.",
            ),
            (
                "Restart",
                MediaControl::Restart,
                "Restart only this Media Source from the beginning.",
            ),
            (
                "Stop",
                MediaControl::Stop,
                "Stop this Media Source, clear its picture and silence its future audio.",
            ),
        ] {
            let enabled = !view.busy
                && target.is_ok()
                && !(operation == MediaControl::Stop && state == Some(MediaPlaybackState::Stopped));
            let response = ui
                .add_enabled(
                    enabled,
                    egui::Button::new(crate::i18n::tr(ui, label)).wrap(),
                )
                .on_hover_text(crate::i18n::tr(ui, hint))
                .on_disabled_hover_text(crate::i18n::tr(
                    ui,
                    if view.busy {
                        "Wait for the active media operation."
                    } else {
                        "Media playback state is not available for this source."
                    },
                ));
            #[cfg(test)]
            ui.ctx().data_mut(|d| {
                d.insert_temp(egui::Id::new(("media-transport", label)), response.rect)
            });
            if response.clicked() {
                if let Ok(target) = &target {
                    intents.push(super::Intent::ControlMedia {
                        target: target.clone(),
                        operation,
                    });
                }
            }
        }
    });
    if !compact { ui.small(crate::i18n::tr(ui,"Transport does not edit source filters."))
        .on_hover_text(crate::i18n::tr(ui,"Pause and Play keep filter settings. Play currently resumes with fresh resampler and audio-filter history.")); }
}
#[cfg(test)]
#[path = "media_controls_tests.rs"]
mod tests;
