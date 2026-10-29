//! Staged Gate/EQ controls and scalar response views. No audio buffers, native
//! calls or persistent DSP state belong to this view.
use eframe::egui;
use skylive_core::AudioFilter;

pub fn editor(ui: &mut egui::Ui, filter: &mut AudioFilter, sample_rate: u32) {
    match filter {
        AudioFilter::Gate {
            threshold_db,
            ratio,
            range_db,
            attack_ms,
            hold_ms,
            release_ms,
            enabled,
        } => {
            ui.checkbox(enabled, crate::i18n::tr(ui, "Enabled"))
                .on_hover_text(crate::i18n::tr(
                    ui,
                    "Bypass or enable this gate without removing its settings.",
                ))
                .on_disabled_hover_text(crate::i18n::tr(
                    ui,
                    "Bypass or enable this gate without removing its settings.",
                ));
            ui.label(crate::i18n::tr(ui,"Attenuate quiet sound below the threshold. This gate changes the signal level, not its playback timing."));
            egui::Grid::new("gate-properties")
                .num_columns(2)
                .show(ui, |ui| {
                    for (label, value, min, max, unit, speed, help) in [
                        (
                            "Threshold",
                            threshold_db,
                            -90.0,
                            0.0,
                            " dB",
                            0.1,
                            "Set the level below which the gate attenuates quiet sound.",
                        ),
                        (
                            "Ratio",
                            ratio,
                            1.0,
                            100.0,
                            ":1",
                            0.1,
                            "Set how strongly sound below the threshold is attenuated.",
                        ),
                        (
                            "Attenuation Range",
                            range_db,
                            0.0,
                            90.0,
                            " dB",
                            0.1,
                            "Limit how much the gate can reduce the signal level.",
                        ),
                        (
                            "Attack",
                            attack_ms,
                            0.01,
                            100.0,
                            " ms",
                            0.1,
                            "Set how quickly the gate opens when sound becomes louder.",
                        ),
                        (
                            "Hold",
                            hold_ms,
                            0.0,
                            2000.0,
                            " ms",
                            1.0,
                            "Keep the gate open for this long before it begins closing.",
                        ),
                        (
                            "Release",
                            release_ms,
                            1.0,
                            3000.0,
                            " ms",
                            1.0,
                            "Set how quickly the gate closes after the hold time.",
                        ),
                    ] {
                        field(
                            ui,
                            value,
                            Field {
                                label,
                                min,
                                max,
                                unit,
                                speed,
                                help,
                            },
                        );
                    }
                });
        }
        AudioFilter::SimpleEq {
            low_db,
            low_freq_hz,
            mid_db,
            mid_freq_hz,
            mid_q,
            high_db,
            high_freq_hz,
            output_db,
            enabled,
        } => {
            ui.checkbox(enabled, crate::i18n::tr(ui, "Enabled"))
                .on_hover_text(crate::i18n::tr(
                    ui,
                    "Bypass or enable this equalizer without removing its settings.",
                ))
                .on_disabled_hover_text(crate::i18n::tr(
                    ui,
                    "Bypass or enable this equalizer without removing its settings.",
                ));
            ui.label(crate::i18n::tr(ui,"Three bands: low shelf, mid peak and high shelf. Q controls the width of the mid band."));
            egui::Grid::new("simple-eq-properties")
                .num_columns(2)
                .show(ui, |ui| {
                    for (label, value, min, max, unit, speed, help) in [
                        (
                            "Low Gain",
                            low_db,
                            -24.0,
                            24.0,
                            " dB",
                            0.1,
                            "Boost or cut the low-frequency shelf.",
                        ),
                        (
                            "Low Frequency",
                            low_freq_hz,
                            20.0,
                            1000.0,
                            " Hz",
                            1.0,
                            "Set the low-shelf frequency from 20 to 1000 Hz.",
                        ),
                        (
                            "Mid Gain",
                            mid_db,
                            -24.0,
                            24.0,
                            " dB",
                            0.1,
                            "Boost or cut the mid-frequency band.",
                        ),
                        (
                            "Mid Frequency",
                            mid_freq_hz,
                            100.0,
                            10000.0,
                            " Hz",
                            1.0,
                            "Set the mid-band center frequency from 100 to 10000 Hz.",
                        ),
                        (
                            "Mid Q",
                            mid_q,
                            0.1,
                            10.0,
                            "",
                            0.01,
                            "Set the mid-band width. A higher Q makes the band narrower.",
                        ),
                        (
                            "High Gain",
                            high_db,
                            -24.0,
                            24.0,
                            " dB",
                            0.1,
                            "Boost or cut the high-frequency shelf.",
                        ),
                        (
                            "High Frequency",
                            high_freq_hz,
                            1000.0,
                            20000.0,
                            " Hz",
                            1.0,
                            "Set the high-shelf frequency from 1000 to 20000 Hz.",
                        ),
                        (
                            "Output Gain",
                            output_db,
                            -24.0,
                            24.0,
                            " dB",
                            0.1,
                            "Adjust the equalizer's overall output level.",
                        ),
                    ] {
                        field(
                            ui,
                            value,
                            Field {
                                label,
                                min,
                                max,
                                unit,
                                speed,
                                help,
                            },
                        );
                    }
                });
        }
        _ => return,
    }
    response(ui, *filter, sample_rate);
}
struct Field {
    label: &'static str,
    min: f32,
    max: f32,
    unit: &'static str,
    speed: f64,
    help: &'static str,
}
fn field(ui: &mut egui::Ui, value: &mut f32, spec: Field) {
    let label = ui.label(crate::i18n::tr(ui, spec.label));
    let response = ui
        .push_id(spec.label, |ui| {
            ui.add(
                egui::DragValue::new(value)
                    .range(spec.min..=spec.max)
                    .clamp_existing_to_range(false)
                    .speed(spec.speed)
                    .suffix(spec.unit)
                    .max_decimals(6),
            )
            .labelled_by(label.id)
            .on_hover_text(crate::i18n::tr(ui, spec.help))
            .on_disabled_hover_text(crate::i18n::tr(ui, spec.help))
        })
        .inner;
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("craft-audio-field", spec.label)),
            response.rect,
        )
    });
    let _ = response;
    ui.end_row();
}
fn response(ui: &mut egui::Ui, filter: AudioFilter, sample_rate: u32) {
    let (points, caption, help) = match filter {
        AudioFilter::Gate { .. } => {
            let Ok(parameters) = skylive_host::audio::gate_parameters(filter) else {
                invalid_curve(ui);
                return;
            };
            let points = (0..=64)
                .filter_map(|index| {
                    let x = index as f32 / 64.0;
                    parameters
                        .transfer_db(-90.0 + 90.0 * x)
                        .ok()
                        .map(|db| egui::pos2(x, (db + 180.0) / 180.0))
                })
                .collect::<Vec<_>>();
            (
                points,
                "Static gate response · input −90…0 dB · output −180…0 dB",
                "The curve shows the gate's target level. Attack, hold and release control how quickly the sound follows it.",
            )
        }
        AudioFilter::SimpleEq { .. } => {
            let Ok(parameters) = skylive_host::audio::simple_eq_parameters(filter) else {
                invalid_curve(ui);
                return;
            };
            let Ok(eq) = skylive_media::craft_audio::SimpleEq::new(sample_rate, 1, parameters)
            else {
                invalid_curve(ui);
                return;
            };
            let points = (0..=64)
                .filter_map(|index| {
                    let x = index as f32 / 64.0;
                    eq.response_db(20.0 * 1000.0f64.powf(f64::from(x)))
                        .ok()
                        .map(|db| egui::pos2(x, (db as f32 + 36.0) / 72.0))
                })
                .collect::<Vec<_>>();
            (
                points,
                "EQ response · 20…20000 Hz · view −36…36 dB",
                "The curve shows the target frequency response at the project's sample rate. Responses beyond the displayed gain range are clipped to the plot.",
            )
        }
        _ => return,
    };
    ui.label(crate::i18n::tr(ui, caption));
    let (rect, hover) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(440.0), 112.0),
        egui::Sense::hover(),
    );
    hover.on_hover_text(crate::i18n::tr(ui, help));
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.rect_filled(rect, 2.0, crate::theme::preview_background(ui));
    for index in 1..6 {
        let x = rect.left() + rect.width() * index as f32 / 6.0;
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0, ui.visuals().text_color().gamma_multiply(0.25)),
        );
    }
    painter.add(egui::Shape::line(
        points
            .into_iter()
            .map(|point| {
                egui::pos2(
                    rect.left() + point.x * rect.width(),
                    rect.bottom() - point.y * rect.height(),
                )
            })
            .collect(),
        egui::Stroke::new(
            2.0,
            if ui.visuals().dark_mode {
                egui::Color32::from_rgb(75, 175, 255)
            } else {
                ui.visuals().hyperlink_color
            },
        ),
    ));
}
fn invalid_curve(ui: &mut egui::Ui) {
    ui.colored_label(
        ui.visuals().error_fg_color,
        crate::i18n::tr(ui, "Correct the filter parameters to display its response."),
    );
}
