//! Small recording status projection; exact diagnostics remain in a bounded popup.
use super::RecordingViewState;
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, recording: &RecordingViewState) {
    show_output(
        ui,
        recording.active,
        recording.degraded_video_frames,
        &recording.warnings,
        "Recording source warnings",
        "Recording continues with unavailable sources",
    );
}
pub(super) fn show_streaming(ui: &mut egui::Ui, streaming: &super::StreamingViewState) {
    show_output(
        ui,
        streaming.active,
        streaming.degraded_video_frames,
        &streaming.warnings,
        "Streaming source warnings",
        "Streaming continues with unavailable sources",
    );
}
fn show_output(
    ui: &mut egui::Ui,
    active: bool,
    degraded_video_frames: u64,
    warnings: &Option<std::sync::Arc<skylive_host::recording::RecordingWarnings>>,
    title: &str,
    continued: &str,
) {
    if degraded_video_frames == 0 && warnings.is_none() {
        return;
    }
    let count = warnings.as_ref().map_or(0, |warnings| {
        (warnings.diagnostics.len() as u64).saturating_add(warnings.omitted)
    });
    let caption = format!("{} ({count})", crate::i18n::tr(ui, title));
    ui.menu_button(caption, |ui| {
        ui.set_max_width(480.0);
        ui.set_min_width(220.0_f32.min(ui.available_width()));
        if active && warnings.is_some() {
            ui.label(crate::i18n::tr(ui, continued));
        }
        ui.small(crate::i18n::tf(
            ui,
            "Frames with unavailable sources: {}",
            &[("{}", degraded_video_frames.to_string())],
        ));
        let Some(warnings) = warnings else {
            return;
        };
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| {
                for issue in &warnings.diagnostics {
                    if let Some(name) = warnings.source_name(issue.source) {
                        ui.strong(format!(
                            "{} · {} · {}",
                            name,
                            issue.source.get(),
                            issue.code
                        ));
                    } else {
                        ui.strong(format!("{} · {}", issue.source.get(), issue.code));
                    }
                    ui.add(egui::Label::new(&issue.detail).wrap());
                    ui.separator();
                }
                if warnings.omitted != 0 {
                    ui.label(crate::i18n::tf(
                        ui,
                        "{} additional source warnings",
                        &[("{}", warnings.omitted.to_string())],
                    ));
                }
            });
    })
    .response
    .on_hover_text(crate::i18n::tr(
        ui,
        "Show source warnings and the number of affected video frames.",
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn painted(shape: &egui::epaint::Shape, text: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(value) if value.galley.text() == text => {
                Some(value.pos + value.galley.size() * 0.5)
            }
            egui::epaint::Shape::Vec(items) => items.iter().find_map(|item| painted(item, text)),
            _ => None,
        }
    }
    #[test]
    fn warning_popup_keeps_recording_active_and_renders_exact_details_without_payload_copy() {
        use skylive_host::{
            preview::SourceDiagnostic,
            recording::RecordingWarnings,
            resource_issue::ResourceIssueKind,
            transition::{FrameComposition, FrameScene},
        };
        let session = skylive_core::Session::new();
        let output = FrameComposition::Scene(FrameScene::Live(session.preview()));
        let warnings = Arc::new(RecordingWarnings {
            session: session.identity(),
            revision: session.snapshot().revision(),
            time_ns: 0,
            resource_generation: 0,
            capture_epoch: 0,
            capture_generation: 0,
            output: output.identity(),
            composition: output.sample_key(),
            source_names: vec![],
            diagnostics: vec![SourceDiagnostic {
                source: skylive_core::Id::new(72),
                kind: ResourceIssueKind::Unavailable,
                code: "capture_frame_unavailable",
                detail: "Exact device ID: DISPLAY\\Fixture; HRESULT 0x80070005".into(),
            }],
            omitted: 0,
        });
        let view = RecordingViewState {
            available: true,
            active: true,
            degraded_video_frames: 7,
            warnings: Some(warnings.clone()),
            ..Default::default()
        };
        let context = egui::Context::default();
        let render = |events| {
            context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| show(ui, &view));
                },
            )
        };
        let output = render(vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| painted(&shape.shape, "Recording source warnings (1)"))
            .unwrap();
        for pressed in [true, false] {
            let _ = render(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        let output = render(vec![]);
        for text in [
            "Recording continues with unavailable sources",
            "Frames with unavailable sources: 7",
            "Exact device ID: DISPLAY\\Fixture; HRESULT 0x80070005",
        ] {
            assert!(
                output
                    .shapes
                    .iter()
                    .any(|shape| painted(&shape.shape, text).is_some()),
                "missing exact warning text {text}"
            );
        }
        assert!(view.active);
        assert_eq!(
            Arc::strong_count(&warnings),
            2,
            "painting borrows the bounded payload"
        );
    }
}
