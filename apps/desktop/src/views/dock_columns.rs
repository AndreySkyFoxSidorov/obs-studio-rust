//! Fixed-size window presentation. Never publishes a Session command.
use eframe::egui;
const GAP: f32 = 4.0;
const MIN: [f32; 5] = [80.0, 120.0, 150.0, 130.0, 250.0];
const DEFAULT: [u16; 5] = [1600, 2400, 3000, 2200, 800];
const NAMES: [&str; 5] = [
    "Scenes",
    "Sources",
    "Audio Mixer",
    "Scene Transitions",
    "Controls",
];

#[derive(Clone, Copy, Debug, PartialEq)]
struct Scope {
    area: egui::Rect,
    visible: [bool; 5],
    pixels_per_point: f32,
}
#[derive(Clone, Debug)]
struct Gesture {
    scope: Scope,
    left: usize,
    right: usize,
    start: f32,
    widths: [f32; 5],
    baseline: [u16; 5],
    draft: [u16; 5],
}
#[derive(Clone, Debug)]
pub struct DockColumns {
    weights: [u16; 5],
    compact: usize,
    gesture: Option<Gesture>,
    tab_ids: [Option<egui::Id>; 4],
    compact_scope: Option<Scope>,
    reveal: bool,
    keyboard_latch: bool,
}
impl Default for DockColumns {
    fn default() -> Self {
        Self {
            weights: DEFAULT,
            compact: 1,
            gesture: None,
            tab_ids: [None; 4],
            compact_scope: None,
            reveal: true,
            keyboard_latch: false,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub rects: [Option<egui::Rect>; 5],
}

impl DockColumns {
    #[cfg(test)]
    pub fn weights(&self) -> [u16; 5] {
        self.weights
    }
    #[cfg(test)]
    pub fn selected(&self) -> usize {
        self.compact
    }
    pub fn cancel(&mut self) {
        self.gesture = None;
    }
    fn effective(&self) -> [u16; 5] {
        self.gesture.as_ref().map_or(self.weights, |g| g.draft)
    }
    pub fn reserves_keyboard(&self, ctx: &egui::Context) -> bool {
        if self.keyboard_latch
            && ctx.input(|input| {
                input.key_down(egui::Key::ArrowLeft)
                    || input.key_down(egui::Key::ArrowRight)
                    || input.key_down(egui::Key::Home)
            })
        {
            return true;
        }
        let Some(focused) = ctx.memory(|m| m.focused()) else {
            return false;
        };
        self.tab_ids.contains(&Some(focused))
            || (0..5).any(|l| ((l + 1)..5).any(|r| focused == split_id(l, r)))
    }
    fn wide(area: egui::Rect, visible: [bool; 5], weights: [u16; 5]) -> Plan {
        let count = visible.into_iter().filter(|v| *v).count();
        let available = (area.width() - GAP * count.saturating_sub(1) as f32).max(0.0);
        let minimum: f32 = (0..5).filter(|i| visible[*i]).map(|i| MIN[i]).sum();
        let total: u32 = (0..5)
            .filter(|i| visible[*i])
            .map(|i| u32::from(weights[i]))
            .sum();
        let mut x = area.left();
        let rects = std::array::from_fn(|i| {
            if !visible[i] {
                return None;
            }
            let width = if available < minimum {
                available * MIN[i] / minimum.max(1.0)
            } else {
                MIN[i] + (available - minimum) * f32::from(weights[i]) / total.max(1) as f32
            };
            let rect = egui::Rect::from_min_size(
                egui::pos2(x, area.top()),
                egui::vec2(width, area.height().max(0.0)),
            );
            x += width + GAP;
            Some(rect)
        });
        Plan { rects }
    }
    fn narrow(area: egui::Rect, visible: [bool; 5]) -> bool {
        let count = visible.into_iter().filter(|v| *v).count();
        let minimum: f32 = (0..5).filter(|i| visible[*i]).map(|i| MIN[i]).sum();
        area.width() < minimum + GAP * count.saturating_sub(1) as f32
    }
    /// Pure legacy-test geometry. Production uses show(), including compact tabs.
    #[cfg(test)]
    pub fn rects(&self, area: egui::Rect, visible: [bool; 5]) -> [Option<egui::Rect>; 5] {
        Self::wide(area, visible, self.effective()).rects
    }
    #[cfg(test)]
    pub fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect, visible: [bool; 5]) -> Plan {
        self.show_locked(ui, area, visible, false)
    }
    pub fn show_locked(
        &mut self,
        ui: &mut egui::Ui,
        area: egui::Rect,
        visible: [bool; 5],
        locked: bool,
    ) -> Plan {
        if !area.is_finite() || area.width() <= 0.0 || area.height() <= 0.0 {
            self.cancel();
            return Plan { rects: [None; 5] };
        }
        if !ui.input(|input| {
            input.key_down(egui::Key::ArrowLeft)
                || input.key_down(egui::Key::ArrowRight)
                || input.key_down(egui::Key::Home)
        }) {
            self.keyboard_latch = false;
        }
        let scope = Scope {
            area,
            visible,
            pixels_per_point: ui.ctx().pixels_per_point(),
        };
        let (press, position, released, down, escape, focused, gone) = ui.input(|input| {
            let press = input.events.iter().find_map(|event| match event {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    ..
                } => Some(*pos),
                _ => None,
            });
            let release = input.events.iter().rev().find_map(|event| match event {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    ..
                } => Some(*pos),
                _ => None,
            });
            (
                press,
                release.or(input.pointer.interact_pos()),
                input.pointer.primary_released(),
                input.pointer.primary_down(),
                input.key_pressed(egui::Key::Escape),
                input.focused,
                input
                    .events
                    .iter()
                    .any(|e| matches!(e, egui::Event::PointerGone)),
            )
        });
        let allowed = ui.is_enabled()
            && focused
            && !escape
            && (!gone || released)
            && ui
                .ctx()
                .memory(|m| m.allows_interaction(ui.layer_id()) && !m.any_popup_open());
        let resize_allowed = allowed && !locked;
        if locked {
            for left in 0..5 {
                for right in left + 1..5 {
                    ui.ctx()
                        .memory_mut(|memory| memory.surrender_focus(split_id(left, right)));
                }
            }
        }
        if locked
            || self.gesture.as_ref().is_some_and(|g| g.scope != scope)
            || escape
            || !focused
            || !allowed
            || (gone && !released)
        {
            self.cancel();
        }
        if Self::narrow(area, visible) {
            self.cancel();
            if self.compact_scope != Some(scope) {
                self.compact_scope = Some(scope);
                self.reveal = true;
            }
            return self.compact_plan(ui, area, visible, allowed);
        }
        self.compact_scope = None;
        self.reveal = true;
        for id in self.tab_ids.iter().flatten() {
            ui.ctx().memory_mut(|memory| memory.surrender_focus(*id));
        }
        self.tab_ids = [None; 4];
        let plan = Self::wide(area, visible, self.effective());
        let active: Vec<_> = (0..5).filter(|i| visible[*i]).collect();
        // At most five entries: this Vec is small bounded UI metadata, not a model scan.
        for pair in active.windows(2) {
            let (left, right) = (pair[0], pair[1]);
            let a = plan.rects[left].unwrap();
            let b = plan.rects[right].unwrap();
            let center = (a.right() + b.left()) * 0.5;
            let hit = egui::Rect::from_min_max(
                egui::pos2(center - 4.0, area.top()),
                egui::pos2(center + 4.0, area.bottom()),
            );
            let response = ui.interact(
                hit,
                split_id(left, right),
                if locked {
                    egui::Sense::hover()
                } else {
                    egui::Sense::click_and_drag()
                },
            );
            let label = crate::i18n::tf(
                ui,
                "Resize {left} / {right}",
                &[
                    ("{left}", crate::i18n::tr(ui, NAMES[left])),
                    ("{right}", crate::i18n::tr(ui, NAMES[right])),
                ],
            );
            let hint = crate::i18n::tr(
                ui,
                if locked {
                    "Unlock docks to resize them."
                } else {
                    "Drag to resize dock columns. Left/Right adjusts width; Shift uses larger steps. Escape cancels a drag. Double-click or Home restores default widths."
                },
            );
            let info = egui::WidgetInfo::slider(resize_allowed, f64::from(a.width()), label);
            response.widget_info(|| info.clone());
            response.clone().on_hover_text(hint);
            if !locked
                && (response.hovered()
                    || self
                        .gesture
                        .as_ref()
                        .is_some_and(|g| g.left == left && g.right == right))
            {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if resize_allowed
                && let Some(p) = press.filter(|p| {
                    hit.contains(*p)
                        && ui.clip_rect().contains(*p)
                        && ui.ctx().layer_id_at(*p) == Some(response.layer_id)
                })
            {
                response.request_focus();
                self.gesture = Some(Gesture {
                    scope,
                    left,
                    right,
                    start: p.x,
                    widths: std::array::from_fn(|i| plan.rects[i].map_or(0.0, |r| r.width())),
                    baseline: self.weights,
                    draft: self.weights,
                });
            }
            if resize_allowed && response.double_clicked() {
                self.weights = DEFAULT;
                self.cancel();
            }
            if resize_allowed && response.has_focus() {
                let home =
                    ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Home));
                let mut delta = 0.0;
                for (key, sign) in [(egui::Key::ArrowLeft, -1.0), (egui::Key::ArrowRight, 1.0)] {
                    if ui.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, key)) {
                        delta += sign * 10.0;
                    } else if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key)) {
                        delta += sign;
                    }
                }
                if home {
                    self.keyboard_latch = true;
                    self.weights = DEFAULT;
                    self.cancel();
                } else if delta != 0.0 {
                    self.keyboard_latch = true;
                    self.cancel();
                    let widths = std::array::from_fn(|i| plan.rects[i].map_or(0.0, |r| r.width()));
                    self.weights = adjust(self.weights, widths, left, right, delta);
                }
            }
        }
        if let (Some(g), Some(p)) = (&mut self.gesture, position) {
            g.draft = adjust(g.baseline, g.widths, g.left, g.right, p.x - g.start);
        }
        if released {
            if let Some(g) = self.gesture.take() {
                self.weights = g.draft;
            }
        } else if !down {
            self.cancel();
        }
        let plan = Self::wide(area, visible, self.effective());
        for pair in active.windows(2) {
            let a = plan.rects[pair[0]].unwrap();
            let b = plan.rects[pair[1]].unwrap();
            let x = (a.right() + b.left()) * 0.5;
            let emphasized = self
                .gesture
                .as_ref()
                .is_some_and(|g| g.left == pair[0] && g.right == pair[1])
                || ui.ctx().memory(|m| m.has_focus(split_id(pair[0], pair[1])));
            ui.painter().vline(
                x,
                area.y_range(),
                egui::Stroke::new(
                    if emphasized { 2.0 } else { 1.0 },
                    if emphasized {
                        ui.visuals().selection.stroke.color
                    } else {
                        crate::theme::border(ui)
                    },
                ),
            );
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    egui::Id::new(("dock-splitter-rect", pair[0], pair[1])),
                    egui::Rect::from_center_size(
                        egui::pos2(x, area.center().y),
                        egui::vec2(8.0, area.height()),
                    ),
                )
            });
        }
        plan
    }
    fn compact_plan(
        &mut self,
        ui: &mut egui::Ui,
        area: egui::Rect,
        visible: [bool; 5],
        allowed: bool,
    ) -> Plan {
        let mut rects = [None; 5];
        let has_content = visible[..4].iter().any(|v| *v);
        let controls = if visible[4] {
            area.width().min(MIN[4])
        } else {
            0.0
        };
        if visible[4] {
            rects[4] = Some(egui::Rect::from_min_size(
                egui::pos2(area.right() - controls, area.top()),
                egui::vec2(controls, area.height()),
            ));
        }
        let width =
            (area.width() - controls - if visible[4] && has_content { GAP } else { 0.0 }).max(0.0);
        if !has_content || width <= 0.0 {
            return Plan { rects };
        }
        if self.compact >= 4 || !visible[self.compact] {
            self.compact = (0..4).find(|i| visible[*i]).unwrap_or(0);
            self.reveal = true;
        }
        let left = egui::Rect::from_min_size(area.min, egui::vec2(width, area.height()));
        let tab_area =
            egui::Rect::from_min_size(left.min, egui::vec2(width, 40.0_f32.min(left.height())));
        let mut tabs = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("compact-dock-header")
                .max_rect(tab_area)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        tabs.set_clip_rect(tab_area.intersect(ui.clip_rect()));
        if !allowed {
            tabs.disable();
        }
        let mut focus_next = None;
        for (i, is_visible) in visible.iter().copied().take(4).enumerate() {
            if !is_visible {
                if let Some(id) = self.tab_ids[i].take() {
                    ui.ctx().memory_mut(|memory| memory.surrender_focus(id));
                }
            }
        }
        egui::ScrollArea::horizontal()
            .id_salt("compact-dock-tabs")
            .max_height(tab_area.height())
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(&mut tabs, |ui| {
                ui.horizontal(|ui| {
                    for (i, is_visible) in visible.iter().copied().take(4).enumerate() {
                        if !is_visible {
                            continue;
                        }
                        let hint = crate::i18n::tf(
                            ui,
                            "Show the {dock} dock.",
                            &[("{dock}", crate::i18n::tr(ui, NAMES[i]))],
                        );
                        let response = ui
                            .push_id(tab_id(i), |ui| {
                                ui.selectable_label(
                                    self.compact == i,
                                    crate::i18n::tr(ui, NAMES[i]),
                                )
                                .on_hover_text(hint.clone())
                                .on_disabled_hover_text(hint)
                            })
                            .inner;
                        self.tab_ids[i] = Some(response.id);
                        if response.clicked() {
                            self.compact = i;
                            self.reveal = true;
                            response.request_focus();
                        }
                        if allowed && response.has_focus() {
                            let step = ui.input_mut(|input| {
                                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                                    1
                                } else if input
                                    .consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)
                                {
                                    -1
                                } else {
                                    0
                                }
                            });
                            if step != 0 {
                                self.keyboard_latch = true;
                                let mut next = i;
                                for _ in 0..4 {
                                    next = ((next as i32 + step).rem_euclid(4)) as usize;
                                    if visible[next] {
                                        self.compact = next;
                                        self.reveal = true;
                                        focus_next = Some(next);
                                        break;
                                    }
                                }
                            }
                        }
                        if self.compact == i && self.reveal {
                            response.scroll_to_me(Some(egui::Align::Center));
                            self.reveal = false;
                        } else if response.gained_focus() {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        #[cfg(test)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(egui::Id::new(("compact-dock-tab", i)), response.rect)
                        });
                    }
                });
            });
        if let Some(index) = focus_next
            && let Some(id) = self.tab_ids[index]
        {
            ui.ctx().memory_mut(|m| m.request_focus(id));
        }
        rects[self.compact] = Some(egui::Rect::from_min_max(
            egui::pos2(left.left(), tab_area.bottom() + 2.0),
            left.max,
        ));
        Plan { rects }
    }
}
fn split_id(left: usize, right: usize) -> egui::Id {
    egui::Id::new(("dock-column-split", left, right))
}
fn tab_id(index: usize) -> egui::Id {
    egui::Id::new(("compact-dock", index))
}
fn adjust(weights: [u16; 5], widths: [f32; 5], left: usize, right: usize, delta: f32) -> [u16; 5] {
    let mut next = weights;
    let room = (widths[left] - MIN[left]) + (widths[right] - MIN[right]);
    if !delta.is_finite() || !room.is_finite() || room <= 0.0 {
        return next;
    }
    let sum = u32::from(weights[left]) + u32::from(weights[right]);
    let fraction = ((widths[left] - MIN[left] + delta) / room).clamp(0.0, 1.0);
    let first = ((sum as f32 * fraction).round() as u32).clamp(1, sum - 1);
    next[left] = first as u16;
    next[right] = (sum - first) as u16;
    next
}

#[cfg(test)]
#[path = "dock_columns_tests.rs"]
mod tests;
