use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn strip(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    source: &Source,
    session: u64,
    editable: bool,
    capture: &AudioCaptureSnapshot,
    levels: Option<&Levels<'_>>,
    draft: &mut VolumeDraft,
    meter: &mut MeterState,
    now: f64,
    intents: &mut Vec<Intent>,
) {
    let locked = source.mixer_state().volume_locked();
    let mut db = gain_db(draft.value);
    ui.horizontal(|ui| {
        let name_width = (ui.available_width() - 25.0).max(12.0);
        let name = egui::menu::menu_custom_button(
            ui,
            egui::Button::new(egui::RichText::new(source.name()).strong())
                .frame(false).truncate().min_size(egui::vec2(name_width, 22.0)),
            |ui| source_options(ui, model, source, session, capture, levels, editable, intents),
        ).response;
        name.clone()
            .on_hover_ui(|ui| source_details(ui, source, capture, levels, editable));
        name.context_menu(|ui| {
            source_options(
                ui, model, source, session, capture, levels, editable, intents,
            )
        });
        let properties_name = crate::i18n::tr(ui, "Source Properties");
        ui.menu_button("...", |ui| {
            source_options(
                ui, model, source, session, capture, levels, editable, intents,
            )
        })
        .response
        .on_hover_text(crate::hints::text(ui, "Source Properties"))
        .widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                editable,
                properties_name.clone(),
            )
        });
    });
    let numeric = ui
        .add_enabled_ui(editable && !locked, |ui| {
            ui.push_id(
                (
                    session,
                    model.revision(),
                    source.gain().to_bits(),
                    source.muted(),
                ),
                |ui| {
                    ui.add(
                        egui::DragValue::new(&mut db)
                            .range(SILENCE_DB..=gain_db(20.0))
                            .clamp_existing_to_range(false)
                            .speed(0.1)
                            .max_decimals(6)
                            .suffix(" dB")
                            .custom_formatter(|value, _| format_db(value))
                            .custom_parser(parse_db),
                    )
                    .on_hover_text(crate::hints::text(ui, "Volume"))
                },
            )
            .inner
        })
        .inner;
    let mut numeric_info = egui::WidgetInfo::drag_value(numeric.enabled(), db);
    numeric_info.label = Some(format!("{} — {}", crate::i18n::tr(ui, "Volume"), source.name()));
    numeric.widget_info(|| numeric_info.clone());
    if numeric.changed() {
        draft.value = db_gain(db);
        draft.dirty = true;
    }
    let available = update_meter(meter, levels, now);
    let height = (ui.available_height() - 56.0).clamp(32.0, 200.0);
    let mut slider = None;
    ui.horizontal(|ui| {
        paint(ui, source, meter, available, height);
        let mut deflection = gain_deflection(draft.value);
        let response = ui
            .add_enabled_ui(editable && !locked, |ui| {
                ui.add_sized(
                    [22.0, height],
                    egui::Slider::new(&mut deflection, 0.0..=1.0)
                        .vertical()
                        .show_value(false),
                )
            })
            .inner
            .on_hover_text(crate::hints::text(ui, "Volume"))
            .on_disabled_hover_text(crate::i18n::tr_key(ui, "LockVolume"));
        let volume_name = format!("{} · {}", source.name(), crate::i18n::tr(ui, "Volume"));
        response.widget_info(|| {
            egui::WidgetInfo::slider(
                editable && !locked,
                gain_db(draft.value),
                volume_name.clone(),
            )
        });
        if response.changed() {
            draft.value = deflection_gain(deflection);
            draft.dirty = true;
        }
        slider = Some(response);
    });
    let slider = slider.expect("vertical mixer slider");
    let mut muted = source.muted();
    let mut mute = None;
    ui.horizontal(|ui| {
        mute = Some(ui.add_enabled_ui(editable, |ui| mute_button(ui, &mut muted)).inner);
        monitor_button(ui,model,source,session,editable,intents);
    });
    let mute = mute.expect("vertical mixer mute button");
    finish_edit(
        ui,
        model,
        source,
        session,
        editable,
        draft,
        &slider,
        &numeric,
        muted,
        mute.changed(),
        intents,
    );
    #[cfg(test)]
    ui.data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("audio-fader", source.id().get())),
            slider.rect,
        );
        data.insert_temp(
            egui::Id::new(("audio-fader-widget", source.id().get())),
            slider.id,
        );
        data.insert_temp(
            egui::Id::new(("audio-gain", source.id().get())),
            numeric.rect,
        );
    });
    if !available {
        ui.small(crate::i18n::tr(ui, "Waiting for audio levels"));
    }
    if !editable {
        ui.small(crate::i18n::tr(
            ui,
            "Retired Program source · restore its authored reference to edit",
        ));
    }
}
fn paint(ui: &mut egui::Ui, source: &Source, state: &MeterState, available: bool, height: f32) {
    let channels = state.channels.clamp(1, 8);
    let width = (channels as f32 * 7.0 + 22.0).min(78.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    for channel in 0..channels {
        let bar = egui::Rect::from_min_max(
            rect.min + egui::vec2(channel as f32 * 7.0, 0.0),
            egui::pos2(rect.left() + channel as f32 * 7.0 + 5.0, rect.bottom()),
        );
        let y = |db: f32| bar.bottom() - bar.height() * meter_position(db);
        for (low, high, color, inactive) in meter_segments(ui) {
            let segment = egui::Rect::from_min_max(
                egui::pos2(bar.left(), y(high)),
                egui::pos2(bar.right(), y(low)),
            );
            ui.painter()
                .rect_filled(segment, 0.0, inactive);
            let peak = state.display[channel].min(high);
            if peak > low {
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(bar.left(), y(peak)),
                        egui::pos2(bar.right(), y(low)),
                    ),
                    0.0,
                    if source.muted() || !available {
                        egui::Color32::from_gray(95)
                    } else {
                        color
                    },
                );
            }
        }
        ui.painter().hline(
            bar.x_range(),
            y(state.held[channel]),
            egui::Stroke::new(1.0, ui.visuals().text_color()),
        );
        ui.painter().hline(
            bar.x_range(),
            y(state.rms[channel]),
            egui::Stroke::new(1.0, egui::Color32::WHITE),
        );
        if state.clip_until > ui.input(|input| input.time) {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(bar.min, egui::vec2(5.0, 3.0)),
                0.0,
                crate::theme::interface_color(ui,skylive_core::InterfaceColor::MeterRedActive),
            );
        }
    }
    for db in [-60.0, -40.0, -20.0, -9.0, 0.0] {
        ui.painter().text(
            egui::pos2(
                rect.left() + channels as f32 * 7.0 + 2.0,
                rect.bottom() - rect.height() * meter_position(db),
            ),
            egui::Align2::LEFT_CENTER,
            format!("{db:.0}"),
            egui::FontId::proportional(9.0),
            ui.visuals().weak_text_color(),
        );
    }
    let meter_name = format!("{} · {}", source.name(), crate::i18n::tr(ui, "Audio Meter"));
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            available,
            meter_name.clone(),
        )
    });
    response.on_hover_text(crate::i18n::tr(
        ui,
        "Peak, RMS and held peak in decibels. Meter visibility does not change audio processing.",
    ));
}
