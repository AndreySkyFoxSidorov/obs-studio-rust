//! Compact dock chrome with real egui button/menu semantics and named controls.
use eframe::egui;
#[derive(Clone, Copy)]
pub enum Icon {
    Add,
    Remove,
    Properties,
    Settings,
    Filters,
    More,
}
#[cfg(test)]
#[derive(Clone)]
pub struct Probe {
    pub scope: &'static str,
    pub action: String,
    pub rect: egui::Rect,
}
#[cfg(test)]
pub fn begin(ui: &egui::Ui) {
    begin_context(ui.ctx());
}
pub fn begin_context(ctx: &egui::Context) {
    #[cfg(test)]
    ctx.data_mut(|data| {
        data.insert_temp(egui::Id::new("dock-toolbar-probes"), Vec::<Probe>::new())
    });
    #[cfg(not(test))]
    let _ = ctx;
}
pub fn size(ui: &mut egui::Ui, count: usize) {
    let width = ((ui.available_width() - 3.0 * count.saturating_sub(1) as f32)
        / count.max(1) as f32)
        .clamp(18.0, 24.0);
    ui.spacing_mut().interact_size = egui::vec2(width, 24.0);
    ui.spacing_mut().button_padding = egui::vec2(0.0, 3.0);
    ui.spacing_mut().item_spacing.x = 3.0;
}
fn decorate(
    ui: &egui::Ui,
    response: egui::Response,
    scope: &'static str,
    action: &str,
    icon: Icon,
) -> egui::Response {
    let name = crate::i18n::tr(ui, action);
    let info = egui::WidgetInfo::labeled(egui::WidgetType::Button, response.enabled(), name);
    response.widget_info(|| info.clone());
    let rect = response.rect;
    let center = rect.center();
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let stroke = egui::Stroke::new(1.5, ui.style().interact(&response).fg_stroke.color);
    let radius = 5.0;
    match icon {
        Icon::Add => {
            painter.line_segment(
                [
                    center - egui::vec2(radius, 0.0),
                    center + egui::vec2(radius, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center - egui::vec2(0.0, radius),
                    center + egui::vec2(0.0, radius),
                ],
                stroke,
            );
        }
        Icon::Remove => {
            painter.line_segment(
                [
                    center - egui::vec2(radius, 0.0),
                    center + egui::vec2(radius, 0.0),
                ],
                stroke,
            );
        }
        Icon::Properties => {
            for y in [-4.0, 4.0] {
                painter.line_segment(
                    [
                        center + egui::vec2(-radius, y),
                        center + egui::vec2(radius, y),
                    ],
                    stroke,
                );
                painter.circle_filled(
                    center + egui::vec2(if y < 0.0 { -2.0 } else { 2.0 }, y),
                    2.0,
                    stroke.color,
                );
            }
        }
        Icon::Filters => {
            painter.add(egui::Shape::closed_line(
                vec![
                    center + egui::vec2(-6.0, -5.0),
                    center + egui::vec2(6.0, -5.0),
                    center + egui::vec2(1.5, 0.0),
                    center + egui::vec2(1.5, 5.0),
                    center + egui::vec2(-1.5, 5.0),
                    center + egui::vec2(-1.5, 0.0),
                ],
                stroke,
            ));
        }
        Icon::Settings => {
            painter.circle_stroke(center, 4.0, stroke);
            painter.circle_stroke(center, 1.0, stroke);
            for index in 0..8 {
                let angle=index as f32*std::f32::consts::TAU/8.0;
                let direction=egui::vec2(angle.cos(),angle.sin());
                painter.line_segment([center+direction*4.0,center+direction*7.0],stroke);
            }
        }
        Icon::More => {
            for x in [-5.0, 0.0, 5.0] {
                painter.circle_filled(center + egui::vec2(x, 0.0), 1.5, stroke.color);
            }
        }
    }
    #[cfg(test)]
    if ui.clip_rect().contains_rect(rect) {
        ui.ctx().data_mut(|data| {
            let id = egui::Id::new("dock-toolbar-probes");
            let mut probes = data.get_temp::<Vec<Probe>>(id).unwrap_or_default();
            probes.push(Probe {
                scope,
                action: action.into(),
                rect,
            });
            data.insert_temp(id, probes);
        });
    }
    #[cfg(not(test))]
    let _ = scope;
    response
}
pub fn button(
    ui: &mut egui::Ui,
    scope: &'static str,
    action: &str,
    icon: Icon,
    enabled: bool,
) -> egui::Response {
    let response = ui
        .add_enabled(
            enabled,
            egui::Button::new("").min_size(ui.spacing().interact_size),
        )
        .on_hover_text(crate::hints::text(ui, action))
        .on_disabled_hover_text(crate::hints::text(ui, action));
    decorate(ui, response, scope, action, icon)
}
pub fn menu(
    ui: &mut egui::Ui,
    scope: &'static str,
    action: &str,
    icon: Icon,
    enabled: bool,
    contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let response = ui
        .push_id((scope, action), |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                egui::menu::menu_custom_button(
                    ui,
                    egui::Button::new("").min_size(ui.spacing().interact_size),
                    contents,
                )
                .response
                .on_hover_text(crate::hints::text(ui, action))
                .on_disabled_hover_text(crate::hints::text(ui, action))
            })
            .inner
        })
        .inner;
    decorate(ui, response, scope, action, icon)
}

#[derive(Clone, Copy)]
pub enum RowIcon {
    Visibility,
    Lock,
    Disclosure,
}
/// Compact, independently operable source-row controls. Geometry is painted
/// explicitly, so eye/lock/disclosure symbols do not depend on font coverage.
pub fn row_toggle(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut bool,
    icon: RowIcon,
) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let name = crate::i18n::tr(ui, label);
    let info = if matches!(icon, RowIcon::Disclosure) {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), name)
    } else {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *value, name)
    };
    response.widget_info(|| info.clone());
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let center = rect.center();
    let visuals = ui.style().interact(&response);
    let color = if *value || matches!(icon, RowIcon::Disclosure) {
        visuals.fg_stroke.color
    } else {
        ui.visuals().weak_text_color()
    };
    let stroke = egui::Stroke::new(1.5, color);
    if response.hovered() {
        painter.rect_filled(rect, 2.0, visuals.weak_bg_fill);
    }
    match icon {
        RowIcon::Visibility => {
            painter.add(egui::Shape::closed_line(
                [
                    [-7.0, 0.0],
                    [-4.0, -4.0],
                    [0.0, -5.0],
                    [4.0, -4.0],
                    [7.0, 0.0],
                    [4.0, 4.0],
                    [0.0, 5.0],
                    [-4.0, 4.0],
                ]
                .into_iter()
                .map(|[x, y]| center + egui::vec2(x, y))
                .collect(),
                stroke,
            ));
            painter.circle_filled(center, 2.0, color);
            if !*value {
                painter.line_segment(
                    [
                        center + egui::vec2(-7.0, 6.0),
                        center + egui::vec2(7.0, -6.0),
                    ],
                    stroke,
                );
            }
        }
        RowIcon::Lock => {
            painter.rect_stroke(
                egui::Rect::from_center_size(center + egui::vec2(0.0, 3.0), egui::vec2(10.0, 8.0)),
                1.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            let shift = if *value { 0.0 } else { 3.0 };
            painter.add(egui::Shape::line(
                [
                    [-4.0, -1.0],
                    [-4.0, -5.0],
                    [-2.0, -7.0],
                    [2.0, -7.0],
                    [4.0, -5.0],
                    [4.0, -1.0],
                ]
                .into_iter()
                .map(|[x, y]| center + egui::vec2(x + shift, y))
                .collect(),
                stroke,
            ));
        }
        RowIcon::Disclosure => {
            let points = if *value {
                [[-5.0, -3.0], [5.0, -3.0], [0.0, 4.0]]
            } else {
                [[-3.0, -5.0], [4.0, 0.0], [-3.0, 5.0]]
            };
            painter.add(egui::Shape::convex_polygon(
                points
                    .into_iter()
                    .map(|[x, y]| center + egui::vec2(x, y))
                    .collect(),
                color,
                egui::Stroke::NONE,
            ));
        }
    }
    response
        .on_hover_text(crate::hints::text(ui, label))
        .on_disabled_hover_text(crate::hints::text(ui, label))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_row_symbols_are_visible_vector_controls_with_named_toggle_semantics() {
        let context = egui::Context::default();
        context.enable_accesskit();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless.csv"),
        );
        let mut values = [true, false, true];
        let mut rects = vec![];
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(200.0, 80.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        for (index, (label, icon)) in [
                            ("Show", RowIcon::Visibility),
                            ("Lock", RowIcon::Lock),
                            ("Collapse Group", RowIcon::Disclosure),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            rects.push(row_toggle(ui, label, &mut values[index], icon).rect);
                        }
                    });
                });
            },
        );
        assert_eq!(values, [true, false, true]);
        let nodes = output.platform_output.accesskit_update.as_ref().unwrap();
        for label in ["Show", "Lock", "Collapse Group"] {
            assert!(
                nodes
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some(label))
            );
        }
        for rect in rects {
            assert_eq!(rect.size(), egui::vec2(22.0, 22.0));
            assert!(
                output.shapes.iter().any(|shape| match &shape.shape {
                    egui::epaint::Shape::Path(path) =>
                        !path.points.is_empty()
                            && path
                                .points
                                .iter()
                                .all(|point| rect.contains(*point)
                                    && shape.clip_rect.contains(*point)),
                    _ => false,
                }),
                "eye, lock and disclosure must contain real visible vector strokes, independent of font glyphs"
            );
        }
    }
    #[test]
    fn compact_icon_actions_have_equal_bounded_buttons_and_accessible_names() {
        for width in [88.0, 110.0, 160.0] {
            let context = egui::Context::default();
            context.enable_accesskit();
            crate::theme::install(&context);
            crate::i18n::install(
                &context,
                crate::i18n::english().unwrap(),
                std::path::Path::new("headless-translations.csv"),
            );
            let mut bounds = egui::Rect::NOTHING;
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width + 16.0, 100.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        begin(ui);
                        bounds = ui.available_rect_before_wrap();
                        ui.horizontal(|ui| {
                            size(ui, 4);
                            button(ui, "Sources", "Add", Icon::Add, true);
                            button(ui, "Sources", "Remove", Icon::Remove, true);
                            button(ui, "Sources", "Properties", Icon::Properties, true);
                            menu(ui, "Sources", "Source Actions", Icon::More, true, |ui| {
                                ui.label("fixture");
                            });
                        });
                    });
                },
            );
            let probes = context
                .data_mut(|data| data.get_temp::<Vec<Probe>>(egui::Id::new("dock-toolbar-probes")))
                .unwrap();
            assert_eq!(probes.len(), 4);
            for probe in &probes {
                assert!(
                    probe.rect.width() >= 18.0 && probe.rect.height() >= 22.0,
                    "empty-caption buttons require a real icon-sized hit area"
                );
                assert!(
                    bounds.contains_rect(probe.rect),
                    "{} at width {width}: {:?} exceeds {bounds:?}",
                    probe.action,
                    probe.rect
                );
                assert!((probe.rect.width() - probes[0].rect.width()).abs() < 0.01);
                fn ink(shape: &egui::epaint::Shape, clip: egui::Rect, rect: egui::Rect) -> usize {
                    match shape {
                        egui::epaint::Shape::LineSegment { points, stroke }
                            if stroke.width >= 1.0
                                && stroke.color.a() > 0
                                && points.iter().all(|point| {
                                    rect.contains(*point) && clip.contains(*point)
                                }) =>
                        {
                            1
                        }
                        egui::epaint::Shape::Circle(circle)
                            if circle.radius >= 1.0
                                && circle.fill.a() > 0
                                && rect.contains(circle.center)
                                && clip.contains(circle.center) =>
                        {
                            1
                        }
                        egui::epaint::Shape::Vec(shapes) => {
                            shapes.iter().map(|shape| ink(shape, clip, rect)).sum()
                        }
                        _ => 0,
                    }
                }
                let visible_ink: usize = output
                    .shapes
                    .iter()
                    .map(|shape| ink(&shape.shape, shape.clip_rect, probe.rect))
                    .sum();
                let expected = match probe.action.as_str() {
                    "Add" => 2,
                    "Remove" => 1,
                    "Properties" => 4,
                    _ => 3,
                };
                assert!(
                    visible_ink >= expected,
                    "{} requires visible icon strokes inside its actual clip, not only an accessibility node",
                    probe.action
                );
            }
            let nodes = output.platform_output.accesskit_update.unwrap().nodes;
            for label in ["Add", "Remove", "Properties", "Source Actions"] {
                assert!(
                    nodes.iter().any(|(_, node)| node.label() == Some(label)),
                    "icon control {label} must have an accessible name"
                );
            }
        }
    }
}
