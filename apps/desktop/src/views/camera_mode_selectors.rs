//! Dependent selectors only choose exact members of a completed camera query.
use super::{CameraDraft, VideoCaptureMode, mode_label};
use eframe::egui;
use skylive_core::CameraFpsPolicy;
use skylive_host::camera_capture::{CameraModeRecommendations, best_camera_resolution_mode, highest_camera_frame_rate, camera_frame_rate_matching_project};
use std::collections::BTreeSet;

pub(super) fn select_resolution(draft: &mut CameraDraft, width: u32, height: u32) {
    // Preserve the chosen native format where the new resolution supports it.
    // Otherwise changing resolution silently changes e.g. MJPEG to YUY2,
    // which can make the desired high-resolution frame rate unavailable.
    let preferred = draft.selected_mode.and_then(|current| {
        let candidate = draft.modes.iter().filter_map(|info| info.mode).find(|mode|
            mode.width() == width && mode.height() == height && mode.format() == current.format())?;
        camera_frame_rate_matching_project(&draft.modes, candidate, current.fps_num(), current.fps_den()).ok().flatten()
    });
    if let Some(mode) = preferred.or_else(|| best_camera_resolution_mode(&draft.modes, width, height).ok().flatten()) {
        draft.selected_mode = Some(mode);
    }
}
pub(super) fn select_rate(draft: &mut CameraDraft, num: u32, den: u32) {
    let Some(previous) = draft.selected_mode else {
        return;
    };
    let matches = |mode: VideoCaptureMode| {
        mode.width() == previous.width()
            && mode.height() == previous.height()
            && mode.fps_num() == num
            && mode.fps_den() == den
    };
    let selected = draft
        .modes
        .iter()
        .filter_map(|info| info.mode)
        .find(|mode| matches(*mode) && mode.format() == previous.format())
        .or_else(|| {
            draft
                .modes
                .iter()
                .filter_map(|info| info.mode)
                .find(|mode| matches(*mode))
        });
    if let Some(mode) = selected {
        draft.selected_mode = Some(mode);
        draft.fps_policy = CameraFpsPolicy::Fixed;
    }
}
fn rate_label(ui: &egui::Ui, num: u32, den: u32) -> String {
    // Keep the exact reported rational visible for fractional rates, while
    // showing ordinary integer rates in the form camera users expect.
    let rate=if den==0 {format!("{num}/{den}")} else if num%den==0 {(num/den).to_string()} else {
        let decimal=format!("{:.3}",f64::from(num)/f64::from(den));
        format!("{} ({num}/{den})",decimal.trim_end_matches('0').trim_end_matches('.'))
    };
    crate::i18n::tf(
        ui,
        "{rate} FPS",
        &[("{rate}", rate)],
    )
}
pub(super) fn show(ui: &mut egui::Ui, draft: &mut CameraDraft, project_fps:(u32,u32)) {
    let queried = draft.queried_target.is_some() && draft.queried_target == draft.selected;
    let recommendations = match CameraModeRecommendations::from_modes(&draft.modes) {
        Ok(recommendations) => Some(recommendations),
        Err(error) => {
            ui.colored_label(ui.visuals().error_fg_color, error.to_string());
            None
        }
    };
    let available = !draft.loading() && queried && recommendations.is_some();
    let recommended = recommendations.and_then(|value| value.recommended);
    let full_hd = recommendations.and_then(|value| value.full_hd);
    ui.push_id("camera-mode-shortcuts", |ui| {
        let actions = [
            super::super::modal_footer::Action::new("Use recommended mode", available && recommended.is_some(),
                "Choose a reported Full HD mode near 30 FPS when available; otherwise use the best supported streaming resolution. This changes the draft only."),
            super::super::modal_footer::Action::new("Use Full HD", available && full_hd.is_some(),
                "Select an actual reported 1920 × 1080 mode with its exact frame rate and format. No resolution or frame rate is invented."),
        ];
        match super::super::modal_footer::show(ui, &actions) {
            Some(0) => { draft.selected_mode = recommended; draft.fps_policy = CameraFpsPolicy::Fixed; },
            Some(1) => { draft.selected_mode = full_hd; draft.fps_policy = CameraFpsPolicy::Fixed; },
            _ => {}
        }
    });
    if let Some(mode) = full_hd {
        ui.label(crate::i18n::tf(
            ui,
            "Full HD available: {mode}",
            &[("{mode}", mode_label(ui, mode))],
        ));
    } else if queried {
        ui.label(crate::i18n::tr(ui, "No supported 1920 × 1080 mode was reported. Unsupported camera formats remain visible in the full list."));
    }
    let mut resolutions: Vec<_> = draft
        .modes
        .iter()
        .filter_map(|info| info.mode)
        .map(|mode| (mode.width(), mode.height()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    resolutions.sort_unstable_by_key(|&(width, height)| {
        std::cmp::Reverse((u64::from(width) * u64::from(height), width, height))
    });
    let current = draft
        .selected_mode
        .map(|mode| (mode.width(), mode.height()));
    let name = ui.label(crate::i18n::tr(ui, "Resolution"));
    ui.add_enabled_ui(available && !resolutions.is_empty(), |ui| {
        egui::ComboBox::from_id_salt("camera-resolution")
            .selected_text(current.map_or_else(|| crate::i18n::tr(ui, "Select resolution"), |(w, h)| format!("{w} × {h}")))
            .width(ui.available_width().min(450.0)).truncate().show_ui(ui, |ui| {
                for (width, height) in resolutions {
                    if ui.selectable_label(current == Some((width, height)), format!("{width} × {height}"))
                        .on_hover_text(crate::i18n::tr(ui, "Choose an actual supported native capture resolution."))
                        .clicked() { select_resolution(draft, width, height); }
                }
            }).response.labelled_by(name.id)
            .on_hover_text(crate::i18n::tr(ui, "Choose an actual supported native capture resolution."))
            .on_disabled_hover_text(crate::i18n::tr(ui, "Query camera modes before choosing a capture format."));
    });
    let selected = draft.selected_mode;
    let mut rates: Vec<_> = draft
        .modes
        .iter()
        .filter_map(|info| info.mode)
        .filter(|mode| {
            selected.is_some_and(|chosen| {
                chosen.width() == mode.width() && chosen.height() == mode.height()
            })
        })
        .map(|mode| (mode.fps_num(), mode.fps_den()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    rates.sort_unstable_by(|a, b| {
        (u64::from(b.0) * u64::from(a.1))
            .cmp(&(u64::from(a.0) * u64::from(b.1)))
            .then_with(|| a.cmp(b))
    });
    let project_rate_mode = selected.and_then(|current|
        camera_frame_rate_matching_project(&draft.modes,current,project_fps.0,project_fps.1).ok().flatten());
    let name = ui.label(crate::i18n::tr(ui, "Frame rate"));
    ui.add_enabled_ui(available && !rates.is_empty(), |ui| {
        egui::ComboBox::from_id_salt("camera-frame-rate")
            .selected_text(match draft.fps_policy {
                CameraFpsPolicy::Highest => crate::i18n::tr_key_or(ui,"Module.win-dshow.FPS.Highest","Highest FPS"),
                CameraFpsPolicy::MatchOutput => crate::i18n::tr_key_or(ui,"Module.win-dshow.FPS.Matching","Match Output FPS"),
                CameraFpsPolicy::Fixed => selected.map_or_else(
                    || crate::i18n::tr(ui, "Select frame rate"),
                    |mode| rate_label(ui, mode.fps_num(), mode.fps_den()),
                ),
            })
            .width(ui.available_width().min(450.0))
            .truncate()
            .show_ui(ui, |ui| {
                if ui.selectable_label(draft.fps_policy==CameraFpsPolicy::MatchOutput,
                    crate::i18n::tr_key_or(ui,"Module.win-dshow.FPS.Matching","Match Output FPS"))
                    .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Camera.MatchOutputPolicyHint",
                        "Keep matching the output frame rate when the project or device changes. Uses a supported mode at the selected resolution and format."))
                    .clicked() { draft.fps_policy=CameraFpsPolicy::MatchOutput; }
                if ui.selectable_label(draft.fps_policy==CameraFpsPolicy::Highest,
                    crate::i18n::tr_key_or(ui,"Module.win-dshow.FPS.Highest","Highest FPS"))
                    .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Camera.HighestPolicyHint",
                        "Always choose the highest supported frame rate at the selected resolution and format, including after reconnecting the device."))
                    .clicked() { draft.fps_policy=CameraFpsPolicy::Highest; }
                ui.separator();
                ui.add_enabled_ui(project_rate_mode.is_some(), |ui| {
                    if ui.selectable_label(false, crate::i18n::tr_key_or(ui, "SkyLive.Camera.MatchProjectFpsOnce", "Match project FPS once"))
                        .on_hover_text(crate::i18n::tr_key_or(ui, "SkyLive.Camera.MatchProjectFpsFixedHint", "Choose the reported frame interval closest to the current project FPS, keeping the selected resolution and format. This saves a fixed camera mode."))
                        .clicked() { draft.selected_mode=project_rate_mode; draft.fps_policy=CameraFpsPolicy::Fixed; }
                });
                if ui.selectable_label(false, crate::i18n::tr_key_or(ui, "SkyLive.Camera.SelectHighestFpsOnce", "Select highest FPS once"))
                    .on_hover_text(crate::i18n::tr_key_or(ui, "SkyLive.Camera.HighestFixedHint", "Select the highest reported frame rate for the current resolution and format. This selects a fixed mode."))
                    .clicked() {
                        if let Some(current)=selected {
                            match highest_camera_frame_rate(&draft.modes,current) {
                                Ok(Some(mode)) => { draft.selected_mode=Some(mode); draft.fps_policy=CameraFpsPolicy::Fixed; },
                                Ok(None) => {},
                                Err(error) => draft.error=Some(error.to_string()),
                            }
                        }
                    }
                ui.separator();
                for (num, den) in rates {
                    if ui
                        .selectable_label(
                            draft.fps_policy==CameraFpsPolicy::Fixed && selected
                                .is_some_and(|mode| (mode.fps_num(), mode.fps_den()) == (num, den)),
                            rate_label(ui, num, den),
                        )
                        .on_hover_text(crate::i18n::tr(
                            ui,
                            "Choose an exact reported frame rate for the selected resolution.",
                        ))
                        .clicked()
                    {
                        select_rate(draft, num, den);
                    }
                }
            })
            .response
            .labelled_by(name.id)
            .on_hover_text(match draft.fps_policy {
                CameraFpsPolicy::Highest => crate::i18n::tr_key_or(ui,"SkyLive.Camera.HighestPolicyHint",
                    "Always choose the highest supported frame rate at the selected resolution and format, including after reconnecting the device."),
                CameraFpsPolicy::MatchOutput => crate::i18n::tr_key_or(ui,"SkyLive.Camera.MatchOutputPolicyHint",
                    "Keep matching the output frame rate when the project or device changes. Uses a supported mode at the selected resolution and format."),
                CameraFpsPolicy::Fixed => crate::i18n::tr(ui,"Choose an exact reported frame rate for the selected resolution."),
            })
            .on_disabled_hover_text(crate::i18n::tr(
                ui,
                "Query camera modes before choosing a capture format.",
            ));
    });
    let selected = draft.selected_mode;
    let name = ui.label(crate::i18n::tr(ui, "Video format"));
    ui.add_enabled_ui(available && selected.is_some(), |ui| {
        egui::ComboBox::from_id_salt("camera-pixel-format")
            .selected_text(selected.map_or_else(|| crate::i18n::tr(ui, "Select mode"), |mode| mode_label(ui, mode)))
            .width(ui.available_width().min(450.0)).truncate().show_ui(ui, |ui| {
                if let Some(current) = selected {
                    let mut shown = std::collections::HashSet::new();
                    for info in &draft.modes {
                        if (info.width, info.height) != (current.width(), current.height()) { continue; }
                        let Some(candidate) = info.mode else { continue; };
                        if !shown.insert(candidate.format()) { continue; }
                        // A camera can report MJPEG at 1080p/30 and YUY2 only
                        // at 1080p/5. List both and select an actual mode of
                        // that format closest to the currently selected rate.
                        let Some(mode) = camera_frame_rate_matching_project(&draft.modes, candidate,
                            current.fps_num(), current.fps_den()).ok().flatten() else { continue; };
                        let label = mode_label(ui, mode);
                        if ui.selectable_label(current.format() == mode.format(), &label)
                            .on_hover_text(crate::i18n::tr(ui, "Select an actual native pixel format and its reported color metadata."))
                            .clicked() { draft.selected_mode = Some(mode); }
                    }
                    for info in draft.modes.iter().filter(|info| info.mode.is_none()
                        && (info.width, info.height, info.fps_num, info.fps_den)
                            == (current.width(), current.height(), current.fps_num(), current.fps_den())) {
                        ui.add_enabled(false, egui::Label::new(&info.native_subtype))
                            .on_disabled_hover_text(crate::i18n::tr(ui, info.unsupported_reason.unwrap_or("Unsupported native camera mode")));
                    }
                }
            }).response.labelled_by(name.id)
            .on_hover_text(crate::i18n::tr(ui, "Select an actual native pixel format and its reported color metadata."))
            .on_disabled_hover_text(crate::i18n::tr(ui, "Query camera modes before choosing a capture format."));
    });
}
