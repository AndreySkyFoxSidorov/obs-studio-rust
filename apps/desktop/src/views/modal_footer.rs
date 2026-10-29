//! Shared modal action geometry: equal buttons, centered rows, no clipped footer.
use eframe::egui;

pub struct Action<'a> {
    pub label: &'a str,
    pub enabled: bool,
    pub hint: &'a str,
}
impl<'a> Action<'a> {
    pub fn new(label: &'a str, enabled: bool, hint: &'a str) -> Self {
        Self {
            label,
            enabled,
            hint,
        }
    }
}
struct Layout {
    width: f32,
    height: f32,
    columns: usize,
    button: egui::Vec2,
    gap: egui::Vec2,
}
fn layout(ui: &egui::Ui, actions: &[Action<'_>]) -> Layout {
    let available = ui.available_width().max(1.0);
    let gap = egui::vec2(8.0, 8.0);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let padding = ui.spacing().button_padding;
    let captions: Vec<_> = actions
        .iter()
        .map(|action| crate::i18n::tr(ui, action.label))
        .collect();
    let width = ui
        .fonts(|fonts| {
            captions
                .iter()
                .map(|label| {
                    fonts
                        .layout_no_wrap(label.clone(), font.clone(), egui::Color32::WHITE)
                        .size()
                        .x
                        + padding.x * 2.0
                        + 8.0
                })
                .fold(96.0, f32::max)
        })
        .min(available);
    let height = ui.fonts(|fonts| {
        captions
            .iter()
            .map(|label| {
                fonts
                    .layout(
                        label.clone(),
                        font.clone(),
                        egui::Color32::WHITE,
                        (width - padding.x * 2.0).max(1.0),
                    )
                    .size()
                    .y
                    + padding.y * 2.0
            })
            .fold(28.0, f32::max)
    });
    let columns = (((available + gap.x) / (width + gap.x)).floor() as usize)
        .max(1)
        .min(actions.len().max(1));
    let rows = actions.len().div_ceil(columns);
    Layout {
        width: available,
        height: rows as f32 * height + rows.saturating_sub(1) as f32 * gap.y,
        columns,
        button: egui::vec2(width, height),
        gap,
    }
}
pub fn required_height(ui: &egui::Ui, actions: &[Action<'_>]) -> f32 {
    layout(ui, actions).height
}
pub fn show(ui: &mut egui::Ui, actions: &[Action<'_>]) -> Option<usize> {
    if actions.is_empty() {
        return None;
    }
    let plan = layout(ui, actions);
    let (area, _) =
        ui.allocate_exact_size(egui::vec2(plan.width, plan.height), egui::Sense::hover());
    let mut clicked = None;
    for (row, chunk) in actions.chunks(plan.columns).enumerate() {
        let row_width =
            chunk.len() as f32 * plan.button.x + chunk.len().saturating_sub(1) as f32 * plan.gap.x;
        let left = area.center().x - row_width * 0.5;
        for (column, action) in chunk.iter().enumerate() {
            let index = row * plan.columns + column;
            let rect = egui::Rect::from_min_size(
                egui::pos2(
                    left + column as f32 * (plan.button.x + plan.gap.x),
                    area.top() + row as f32 * (plan.button.y + plan.gap.y),
                ),
                plan.button,
            );
            let response = ui
                .push_id(("modal-action", index), |ui| {
                    ui.add_enabled_ui(action.enabled, |ui| {
                        ui.put(
                            rect,
                            egui::Button::new(crate::i18n::tr(ui, action.label))
                                .wrap()
                                .min_size(plan.button),
                        )
                    })
                    .inner
                })
                .inner
                .on_hover_text(crate::i18n::tr(ui, action.hint))
                .on_disabled_hover_text(crate::i18n::tr(ui, action.hint));
            #[cfg(test)]
            {
                let pointer = ui.input(|input| input.pointer.hover_pos());
                let actual = ui
                    .ctx()
                    .rect_contains_pointer(response.layer_id, response.rect);
                let at = pointer.and_then(|point| ui.ctx().layer_id_at(point));
                let timing = ui.input(|input| {
                    (
                        input.time,
                        input.pointer.time_since_last_click(),
                        input.pointer.time_since_last_movement(),
                        input.time_since_last_scroll(),
                        input.pointer.is_still(),
                        input.pointer.any_down(),
                        input.pointer.interact_pos(),
                    )
                });
                let popup = ui.ctx().memory(|memory| memory.any_popup_open());
                let diagnostics = format!(
                    "id={:?} hint={:?} rect={:?} enabled={} hovered={} contains_pointer={} layer={:?} layer_at={at:?} rect_contains={actual} pointer={pointer:?} timing(time,click,move,scroll,still,down,interact)={timing:?} popup={popup} tooltip_open={} delay={}",
                    response.id,
                    action.hint,
                    response.rect,
                    response.enabled(),
                    response.hovered(),
                    response.contains_pointer(),
                    response.layer_id,
                    response.is_tooltip_open(),
                    ui.ctx().style().interaction.tooltip_delay
                );
                ui.data_mut(|data| {
                    data.insert_temp(egui::Id::new(("modal-action-hover", index)), diagnostics)
                });
            }
            #[cfg(test)]
            ui.data_mut(|data| {
                data.insert_temp(egui::Id::new(("modal-action-rect", index)), response.rect)
            });
            if response.clicked() {
                clicked = Some(index);
            }
        }
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn equal_actions_center_wrap_and_remain_inside_narrow_and_default_viewports() {
        for width in [220.0, 360.0, 520.0, 760.0] {
            let context = egui::Context::default();
            let mut output = egui::FullOutput::default();
            let mut frame = egui::Rect::NOTHING;
            for _ in 0..2 {
                output = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width + 40.0, 500.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            let rect = egui::Rect::from_min_size(
                                egui::pos2(20.0, 20.0),
                                egui::vec2(width, 450.0),
                            );
                            let mut child = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(rect)
                                    .layout(egui::Layout::top_down(egui::Align::Min)),
                            );
                            frame = rect;
                            show(
                                &mut child,
                                &[
                                    Action::new("Save and close", true, "Save and close"),
                                    Action::new(
                                        "Keep recovery and close",
                                        true,
                                        "Keep recovery and close",
                                    ),
                                    Action::new("Discard and close", false, "Discard and close"),
                                    Action::new("Cancel", true, "Cancel"),
                                ],
                            );
                        });
                    },
                );
            }
            let rects: Vec<_> = (0..4)
                .map(|index| {
                    context.data(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new(("modal-action-rect", index)))
                            .unwrap()
                    })
                })
                .collect();
            for rect in &rects {
                assert!(frame.contains_rect(*rect));
                assert!((rect.width() - rects[0].width()).abs() < 0.1);
                assert!((rect.height() - rects[0].height()).abs() < 0.1);
            }
            for row in rects.chunk_by(|a, b| (a.top() - b.top()).abs() < 0.1) {
                assert!(
                    ((row[0].left() + row.last().unwrap().right()) * 0.5 - frame.center().x).abs()
                        < 0.1
                );
            }
            fn visible(shape: &egui::epaint::Shape, clip: egui::Rect) {
                match shape {
                    egui::epaint::Shape::Text(text)
                        if [
                            "Save and close",
                            "Keep recovery and close",
                            "Discard and close",
                            "Cancel",
                        ]
                        .contains(&text.galley.text()) =>
                    {
                        assert!(
                            clip.contains_rect(egui::Rect::from_min_size(
                                text.pos,
                                text.galley.size()
                            ))
                        )
                    }
                    egui::epaint::Shape::Vec(shapes) => {
                        for shape in shapes {
                            visible(shape, clip)
                        }
                    }
                    _ => {}
                }
            }
            for shape in &output.shapes {
                visible(&shape.shape, shape.clip_rect);
            }
        }
    }
}
