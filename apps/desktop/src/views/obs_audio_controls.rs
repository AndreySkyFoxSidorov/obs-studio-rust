//! Draft-only controls for built-in OBS Gain, Polarity and 3-Band Equalizer.
use eframe::egui;
use skylive_core::AudioFilter;

pub(super) fn editor(ui: &mut egui::Ui, filter: &mut AudioFilter) {
    match filter {
        AudioFilter::ObsNoiseSuppression{enabled}=>{
            ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled"))
                .on_hover_text(crate::i18n::tr(ui,"Enable or bypass RNNoise without removing this filter. Bypass retires its queued processing state."));
            ui.horizontal(|ui|{
                ui.label(crate::i18n::tr(ui,"Method"));
                ui.label(crate::i18n::tr(ui,"RNNoise (original model)"))
                    .on_hover_text(crate::i18n::tr(ui,"RNNoise is the available noise-suppression method. Speex is not implemented in this build."));
            });
            ui.label(crate::i18n::tr(ui,"Suppresses background noise using independent audio-channel processing. RNNoise adds 10 ms per stage; packet conversion and buffering add latency. Output admission keeps the total within the supported two-second profile."));
        }

        AudioFilter::ObsThreeBandEq {low_db,mid_db,high_db,enabled}=>{
            ui.checkbox(enabled,crate::i18n::tr(ui,"Enabled")).on_hover_text(crate::i18n::tr(ui,"Enable or bypass the OBS equalizer without removing its gain settings."));
            ui.label(crate::i18n::tr(ui,"Uses the standard OBS band split at 800 Hz and 5000 Hz. Each audio channel is equalized independently."));
            for (name,value,hint)in[
                ("High gain (dB)",high_db,"Adjust the high band from -20 to +20 dB in 0.1 dB steps."),
                ("Mid gain (dB)",mid_db,"Adjust the mid band from -20 to +20 dB in 0.1 dB steps."),
                ("Low gain (dB)",low_db,"Adjust the low band from -20 to +20 dB in 0.1 dB steps."),
            ]{
                let label=ui.label(crate::i18n::tr(ui,name));
                ui.horizontal(|ui|{
                    ui.add(egui::Slider::new(&mut *value,-20.0..=20.0).step_by(0.1).show_value(false).clamping(egui::SliderClamping::Edits))
                        .labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,hint));
                    ui.add(egui::DragValue::new(value).speed(0.1).suffix(" dB"))
                        .labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,hint));
                });
            }
            if ui.button(crate::i18n::tr(ui,"Reset EQ gains")).on_hover_text(crate::i18n::tr(ui,"Set all three gains to 0 dB while keeping the current enabled state.")).clicked(){
                let active=*enabled;*filter=AudioFilter::ObsThreeBandEq{low_db:0.0,mid_db:0.0,high_db:0.0,enabled:active};
            }
        }

        AudioFilter::ObsGain { db, enabled } => {
            ui.checkbox(enabled, crate::i18n::tr(ui, "Enabled"))
                .on_hover_text(crate::i18n::tr(
                    ui,
                    "Enable or bypass this OBS Gain filter without removing its settings.",
                ));
            ui.label(crate::i18n::tr(ui,"Adjust every audio channel by the same gain. The filter does not clip peaks before the source volume control."));
            let label = ui.label(crate::i18n::tr(ui, "Gain (dB)"));
            ui.add(
                egui::Slider::new(db, -30.0..=30.0)
                    .step_by(0.1)
                    .show_value(false)
                    .clamping(egui::SliderClamping::Edits),
            )
            .labelled_by(label.id)
            .on_hover_text(crate::i18n::tr(
                ui,
                "Set the OBS Gain filter from -30 to +30 dB in 0.1 dB steps.",
            ));
            ui.add(egui::DragValue::new(db).speed(0.1).suffix(" dB"))
                .labelled_by(label.id)
                .on_hover_text(crate::i18n::tr(
                    ui,
                    "Set the OBS Gain filter from -30 to +30 dB in 0.1 dB steps.",
                ));
        }
        AudioFilter::ObsPolarityInvert { enabled } => {
            ui.checkbox(enabled, crate::i18n::tr(ui, "Enabled"))
                .on_hover_text(crate::i18n::tr(
                    ui,
                    "Enable or bypass polarity inversion without removing the filter.",
                ));
            ui.label(crate::i18n::tr(ui,"Invert the polarity of every audio channel. Signal level and playback timing remain unchanged."));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn painting_gain_keeps_invalid_drafts_and_unrounded_valid_values_exact() {
        let context = egui::Context::default();
        for db in [-31.125, 31.125, 7.1234] {
            let mut filter = AudioFilter::ObsGain { db, enabled: true };
            for _ in 0..3 {
                let _ = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(600.0, 400.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| editor(ui, &mut filter));
                    },
                );
                let AudioFilter::ObsGain { db: actual, .. } = filter else {
                    unreachable!()
                };
                assert_eq!(
                    actual.to_bits(),
                    db.to_bits(),
                    "painting must not clamp/round a staged value"
                );
            }
            assert_eq!(filter.validate().is_err(), db.abs() > 30.0);
        }
    }
}
