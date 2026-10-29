//! OBS dynamics draft controls. No PCM, devices, persistent DSP or publication.
use eframe::egui;
use skylive_core::{AudioFilter, ObsExpanderDetector, ObsExpanderPreset};
fn number(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    min: f32,
    max: f32,
    step: f64,
    suffix: &str,
) {
    let caption = ui.label(crate::i18n::tr(ui, label));
    ui.add(
        egui::Slider::new(value, min..=max)
            .show_value(false)
            .step_by(step)
            .clamping(egui::SliderClamping::Edits),
    )
    .labelled_by(caption.id)
    .on_hover_text(crate::i18n::tr(
        ui,
        "Adjust this OBS filter parameter; Apply validates its displayed range.",
    ));
    ui.add(egui::DragValue::new(value).speed(step).suffix(suffix))
        .labelled_by(caption.id)
        .on_hover_text(crate::i18n::tr(
            ui,
            "Type an exact value. Invalid drafts remain editable until Apply.",
        ));
}
fn milliseconds(ui: &mut egui::Ui, label: &str, value: &mut u32, max: u32) {
    let caption = ui.label(crate::i18n::tf(ui, label, &[("{}", max.to_string())]));
    ui.add(egui::DragValue::new(value).speed(1.0).suffix(" ms"))
        .labelled_by(caption.id)
        .on_hover_text(crate::i18n::tr(
            ui,
            "Set a whole number of milliseconds within the displayed range.",
        ));
}
fn choose_preset(filter: &mut AudioFilter, preset: ObsExpanderPreset) {
    let enabled = match *filter {
        AudioFilter::ObsExpander { enabled, .. } => enabled,
        _ => return,
    };
    *filter = AudioFilter::obs_expander_preset(preset);
    if let AudioFilter::ObsExpander { enabled: next, .. } = filter {
        *next = enabled;
    }
}
pub(super) fn editor(ui: &mut egui::Ui, filter: &mut AudioFilter) {
    if let AudioFilter::ObsExpander { preset, .. } = *filter {
        let mut next = preset;
        egui::ComboBox::from_id_salt("obs-expander-preset").selected_text(crate::i18n::tr(ui,if preset==ObsExpanderPreset::Gate{"Expander preset: Gate"}else{"Expander preset: Expander"})).truncate().show_ui(ui,|ui|{
   ui.selectable_value(&mut next,ObsExpanderPreset::Expander,crate::i18n::tr(ui,"Expander preset: Expander" )).on_hover_text(crate::hints::text(ui,"Expander preset: Expander")).on_disabled_hover_text(crate::hints::text(ui,"Expander preset: Expander"));
   ui.selectable_value(&mut next,ObsExpanderPreset::Gate,crate::i18n::tr(ui,"Expander preset: Gate" )).on_hover_text(crate::hints::text(ui,"Expander preset: Gate")).on_disabled_hover_text(crate::hints::text(ui,"Expander preset: Gate"));
  }).response.on_hover_text(crate::i18n::tr(ui,"Choosing a preset replaces this draft's expander controls. Apply commits them together."));
        if next != preset {
            choose_preset(filter, next);
        }
    }
    match filter {
        AudioFilter::ObsUpwardCompressor {threshold_db,ratio,attack_ms,release_ms,output_db,knee_db,detector,enabled}=>{
            ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled")).on_hover_text(crate::i18n::tr(ui,"Bypass this OBS dynamics filter without removing its settings or detector history."));
            ui.label(crate::i18n::tr(ui,"OBS Upward Compressor raises quiet audio near the threshold, with a soft knee and the original -60 dB noise-floor rule."));
            number(ui,"Ratio (0:1 to 1:1)",ratio,0.,1.,0.1,":1");
            number(ui,"Threshold (-60 to 0 dB)",threshold_db,-60.,0.,0.1," dB");
            milliseconds(ui,"Attack (1 to {} ms)",attack_ms,100);
            milliseconds(ui,"Release (1 to {} ms)",release_ms,1000);
            number(ui,"Output gain (-32 to 32 dB)",output_db,-32.,32.,0.1," dB");
            let label=ui.label(crate::i18n::tr(ui,"Knee (0 to 20 dB)"));
            ui.add(egui::Slider::new(knee_db,0..=20).suffix(" dB")).labelled_by(label.id)
                .on_hover_text(crate::i18n::tr(ui,"Set the integer soft-knee width. Zero selects a hard threshold."));
            egui::ComboBox::from_id_salt("obs-upward-detector").selected_text(crate::i18n::tr(ui,if *detector==ObsExpanderDetector::Rms{"RMS detector"}else{"Peak detector"})).show_ui(ui,|ui|{
                for (value,caption) in [(ObsExpanderDetector::Rms,"RMS detector"),(ObsExpanderDetector::Peak,"Peak detector")]{
                    ui.selectable_value(detector,value,crate::i18n::tr(ui,caption)).on_hover_text(crate::i18n::tr(ui,"RMS follows the smoothed signal power; Peak follows its absolute sample level."));
                }
            }).response.on_hover_text(crate::i18n::tr(ui,"RMS follows the smoothed signal power; Peak follows its absolute sample level."));
        }
        AudioFilter::ObsNoiseGate {open_threshold_db,close_threshold_db,attack_ms,hold_ms,release_ms,enabled}=>{
            ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"))
                .on_hover_text(crate::i18n::tr(ui,"Bypass this noise gate while retaining its envelope. Changing parameters resets its history."));
            ui.label(crate::i18n::tr(ui,"OBS Noise Gate links the channels with separate open and close thresholds. Zero attack or release changes the gain immediately."));
            number(ui,"Close threshold (-96 to 0 dB)",close_threshold_db,-96.,0.,1.," dB");
            number(ui,"Open threshold (-96 to 0 dB)",open_threshold_db,-96.,0.,1.," dB");
            milliseconds(ui,"Attack (0 to {} ms)",attack_ms,10000);
            milliseconds(ui,"Hold (0 to {} ms)",hold_ms,10000);
            milliseconds(ui,"Release (0 to {} ms)",release_ms,10000);
            return;
        }
        AudioFilter::ObsLimiter {
            threshold_db,
            release_ms,
            enabled,
        } => {
            ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled")).on_hover_text(crate::i18n::tr(ui,"Bypass this OBS dynamics filter without removing its settings or detector history."));
            ui.label(crate::i18n::tr(ui,"OBS Limiter uses a linked peak detector with a fixed 0.001 ms attack. It does not provide lookahead or true-peak limiting."));
            number(
                ui,
                "Threshold (-60 to 0 dB)",
                threshold_db,
                -60.,
                0.,
                0.1,
                " dB",
            );
            milliseconds(ui, "Release (1 to {} ms)", release_ms, 1000);
        }
        AudioFilter::ObsCompressor {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            output_db,
            sidechain: _,
            enabled,
        } => {
            ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled")).on_hover_text(crate::i18n::tr(ui,"Bypass this OBS dynamics filter without removing its settings or detector history."));
            ui.label(crate::i18n::tr(ui,"OBS Compressor links all channels through a peak detector. An external sidechain ducks this source using another source after its filters and before its volume fader; muting the key silences its detector."));
            number(ui, "Ratio (1:1 to 32:1)", ratio, 1., 32., 0.5, ":1");
            number(
                ui,
                "Threshold (-60 to 0 dB)",
                threshold_db,
                -60.,
                0.,
                0.1,
                " dB",
            );
            milliseconds(ui, "Attack (1 to {} ms)", attack_ms, 500);
            milliseconds(ui, "Release (1 to {} ms)", release_ms, 1000);
            number(
                ui,
                "Output gain (-32 to 32 dB)",
                output_db,
                -32.,
                32.,
                0.1,
                " dB",
            );
        }
        AudioFilter::ObsExpander {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            output_db,
            detector,
            enabled,
            ..
        } => {
            ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled")).on_hover_text(crate::i18n::tr(ui,"Bypass this OBS dynamics filter without removing its settings or detector history."));
            ui.label(crate::i18n::tr(ui,"OBS Expander detects each channel independently and attenuates quiet audio by up to 60 dB. The Gate preset uses the same expander algorithm."));
            number(ui, "Ratio (1:1 to 20:1)", ratio, 1., 20., 0.1, ":1");
            number(
                ui,
                "Threshold (-60 to 0 dB)",
                threshold_db,
                -60.,
                0.,
                0.1,
                " dB",
            );
            milliseconds(ui, "Attack (1 to {} ms)", attack_ms, 100);
            milliseconds(ui, "Release (1 to {} ms)", release_ms, 1000);
            number(
                ui,
                "Output gain (-32 to 32 dB)",
                output_db,
                -32.,
                32.,
                0.1,
                " dB",
            );
            egui::ComboBox::from_id_salt("obs-expander-detector").selected_text(crate::i18n::tr(ui,if *detector==ObsExpanderDetector::Rms{"RMS detector"}else{"Peak detector"})).truncate().show_ui(ui,|ui|{ui.selectable_value(detector,ObsExpanderDetector::Rms,crate::i18n::tr(ui,"RMS detector" )).on_hover_text(crate::hints::text(ui,"RMS detector")).on_disabled_hover_text(crate::hints::text(ui,"RMS detector"));ui.selectable_value(detector,ObsExpanderDetector::Peak,crate::i18n::tr(ui,"Peak detector" )).on_hover_text(crate::hints::text(ui,"Peak detector")).on_disabled_hover_text(crate::hints::text(ui,"Peak detector"));}).response.on_hover_text(crate::i18n::tr(ui,"RMS follows the smoothed signal power; Peak follows its absolute sample level."));
        }
        _ => return,
    }
    curve(ui, *filter);
}
fn curve(ui: &mut egui::Ui, filter: AudioFilter) {
    let transfer = |input| match filter {
        AudioFilter::ObsUpwardCompressor {..}=>skylive_host::audio::obs_upward_parameters(filter).ok().and_then(|p|p.transfer_db(input).ok()),
        AudioFilter::ObsLimiter { .. } => skylive_host::audio::obs_limiter_parameters(filter)
            .ok()
            .and_then(|p| p.transfer_db(input).ok()),
        AudioFilter::ObsCompressor { .. } => skylive_host::audio::obs_compressor_parameters(filter)
            .ok()
            .and_then(|p| p.transfer_db(input).ok()),
        AudioFilter::ObsExpander { .. } => skylive_host::audio::obs_expander_parameters(filter)
            .ok()
            .and_then(|p| p.transfer_db(input).ok()),
        _ => None,
    };
    if transfer(0.).is_none() {
        return;
    }
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(440.), 112.),
        egui::Sense::hover(),
    );
    response.on_hover_text(crate::i18n::tr(ui,"Static input/output curve. Attack and release determine the actual signal's response over time."));
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.rect_filled(rect, 2., crate::theme::preview_background(ui));
    let points = (0..=64)
        .filter_map(|i| {
            let x = -60. + 60. * i as f32 / 64.;
            transfer(x).map(|y| {
                egui::pos2(
                    rect.left() + rect.width() * i as f32 / 64.,
                    rect.bottom() - rect.height() * (y + 120.) / 160.,
                )
            })
        })
        .collect();
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(2., ui.visuals().hyperlink_color),
    ));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_drafts_survive_paint_and_presets_are_explicit_draft_changes() {
        let ctx = egui::Context::default();
        for mut filter in [
            AudioFilter::ObsLimiter {
                threshold_db: 1.25,
                release_ms: 0,
                enabled: true,
            },
            AudioFilter::ObsCompressor {
                threshold_db: -18.,
                ratio: 33.25,
                attack_ms: 0,
                release_ms: 60,
                output_db: 0.,
                sidechain: None,
            enabled: true,
            },
            AudioFilter::ObsExpander {
                threshold_db: -40.,
                ratio: 2.,
                attack_ms: 101,
                release_ms: 0,
                output_db: 0.,
                detector: ObsExpanderDetector::Peak,
                preset: ObsExpanderPreset::Expander,
                enabled: false,
            },
        ] {
            let original = filter;
            for _ in 0..3 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800., 600.),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| editor(ui, &mut filter));
                    },
                );
                assert_eq!(filter, original);
                assert!(filter.validate().is_err());
            }
        }
        let mut filter = AudioFilter::obs_expander();
        if let AudioFilter::ObsExpander { enabled, .. } = &mut filter {
            *enabled = false;
        }
        choose_preset(&mut filter, ObsExpanderPreset::Gate);
        assert!(matches!(
            filter,
            AudioFilter::ObsExpander {
                ratio: 10.,
                release_ms: 125,
                enabled: false,
                ..
            }
        ));
        assert!(filter.validate().is_ok());
    }
}
