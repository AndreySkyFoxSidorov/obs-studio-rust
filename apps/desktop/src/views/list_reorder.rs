//! Pointer row reorder presentation. One release -> one versioned intention.
use super::Intent;
use eframe::egui;
use skylive_core::{Command, Id};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum List {
    Scenes,
    Sources(Id),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Scope {
    pub session: u64,
    pub revision: u64,
    pub list: List,
}
#[derive(Clone, Copy)]
pub(super) struct Payload {
    pub(super) scope: Scope,
    pub(super) id: Id,
}

pub(super) fn row(ui: &egui::Ui, response: &egui::Response, scope: Scope, id: Id, locked: bool) {
    let press_id = response.id.with("reorder-press");
    if ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary))
        && response.is_pointer_button_down_on()
    {
        ui.data_mut(|data| data.insert_temp(press_id, Payload { scope, id }));
    }
    if response.drag_started_by(egui::PointerButton::Primary) && !locked && ui.is_enabled() {
        let pressed = ui.data_mut(|data| {
            let pressed = data.get_temp::<Payload>(press_id);
            data.remove::<Payload>(press_id);
            pressed
        });
        if let Some(pressed) = pressed.filter(|pressed| pressed.scope == scope && pressed.id == id)
        {
            egui::DragAndDrop::set_payload(ui.ctx(), pressed);
        } else {
            egui::DragAndDrop::clear_payload(ui.ctx());
        }
    }
    if ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary)) {
        ui.data_mut(|data| {
            data.remove::<Payload>(press_id);
        });
    }
    if !locked {
        response.clone().on_hover_cursor(egui::CursorIcon::Grab);
    }
    #[cfg(test)]
    ui.data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("reorder-row", scope.list, id.get())),
            response.rect,
        )
    });
}

fn destination(rows: &[(Id, egui::Rect)], id: Id, y: f32) -> Option<(usize, usize)> {
    let old = rows.iter().position(|row| row.0 == id)?;
    let slot = rows
        .iter()
        .position(|row| y < row.1.center().y)
        .unwrap_or(rows.len());
    let index = slot.saturating_sub(usize::from(slot > old));
    (old != index).then_some((slot, index))
}

fn grid_destination(rows: &[(Id, egui::Rect)], id: Id, pointer: egui::Pos2) -> Option<(usize, usize)> {
    let old = rows.iter().position(|row| row.0 == id)?;
    let (target, (_, rect)) = rows.iter().enumerate().min_by(|(_, a), (_, b)| {
        a.1.center().distance_sq(pointer).total_cmp(&b.1.center().distance_sq(pointer))
    })?;
    let before = pointer.y < rect.top() || (pointer.y <= rect.bottom() && pointer.x < rect.center().x);
    let slot = target + usize::from(!before);
    let index = slot.saturating_sub(usize::from(slot > old));
    (old != index).then_some((slot, index))
}

/// Call inside the actual ScrollArea after painting all rows. Geometries are
/// logical points; the viewport clip also excludes toolbar/menu targets.
pub(super) fn finish(
    ui: &mut egui::Ui,
    scope: Scope,
    rows: &[(Id, egui::Rect)],
    intents: &mut Vec<Intent>,
) {
    let Some(payload) = egui::DragAndDrop::payload::<Payload>(ui.ctx()) else {
        return;
    };
    if payload.scope.session != scope.session
        || payload.scope.revision != scope.revision
        || !ui.is_enabled()
    {
        egui::DragAndDrop::clear_payload(ui.ctx());
        return;
    }
    if payload.scope.list != scope.list {
        return;
    }
    if rows.is_empty() || !rows.iter().any(|row| row.0 == payload.id) {
        egui::DragAndDrop::clear_payload(ui.ctx());
        return;
    }
    let viewport = ui.clip_rect();
    #[cfg(test)]
    ui.data_mut(|data| data.insert_temp(egui::Id::new(("reorder-viewport", scope.list)), viewport));
    let Some(pointer) = ui.input(|input| input.pointer.interact_pos()) else {
        return;
    };
    if !viewport.contains(pointer) || !ui.rect_contains_pointer(viewport) {
        return;
    }
    let grid = rows.windows(2).any(|pair| (pair[0].1.center().y - pair[1].1.center().y).abs() < 1.0 && (pair[0].1.center().x - pair[1].1.center().x).abs() > 1.0);
    let target = if grid { grid_destination(rows, payload.id, pointer) } else { destination(rows, payload.id, pointer.y) };
    if let Some((slot, index)) = target {
        let y = if slot == 0 {
            rows[0].1.top()
        } else if slot == rows.len() {
            rows[rows.len() - 1].1.bottom()
        } else {
            (rows[slot - 1].1.bottom() + rows[slot].1.top()) * 0.5
        };
        let painter = ui.painter().with_clip_rect(viewport);
        let color = ui.visuals().selection.stroke.color;
        if grid {
            let tile = rows[slot.min(rows.len() - 1)].1;
            let x = if slot == rows.len() { tile.right() } else { tile.left() };
            painter.vline(x, tile.y_range(), egui::Stroke::new(2.0, color));
        } else {
            painter.hline(viewport.x_range(), y, egui::Stroke::new(2.0, color));
            painter.circle_filled(egui::pos2(viewport.left() + 3.0, y), 3.0, color);
        }
        #[cfg(test)]
        ui.data_mut(|data| {
            data.insert_temp(egui::Id::new(("reorder-marker", scope.list)), (slot, index))
        });
        if ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary)) {
            egui::DragAndDrop::clear_payload(ui.ctx());
            let command = match scope.list {
                List::Scenes => Command::MoveScene {
                    scene: payload.id,
                    index,
                },
                List::Sources(scene) => Command::MoveItem {
                    scene,
                    item: payload.id,
                    index: rows.len() - 1 - index,
                },
            };
            intents.push(Intent::Reorder {
                session: scope.session,
                revision: scope.revision,
                command,
            });
            return;
        }
    }
    // Positive scroll delta moves contents downward. Bound dt so a suspended
    // application never turns one resumed frame into a long scrolling jump.
    let edge = 20.0f32.min(viewport.height() * 0.25);
    let direction = if pointer.y < viewport.top() + edge {
        1.0
    } else if pointer.y > viewport.bottom() - edge {
        -1.0
    } else {
        0.0
    };
    if direction != 0.0 && ui.input(|input| input.pointer.primary_down()) {
        let dt = ui.input(|input| input.stable_dt).clamp(0.0, 0.05);
        ui.scroll_with_delta(egui::vec2(0.0, direction * 300.0 * dt));
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::{
        Layout, TransitionViewState,
        docks::{self, DockViewState},
    };
    use skylive_core::Session;
    use skylive_host::audio_capture::{
        AudioCapabilities, AudioCaptureSnapshot, AudioCaptureWorker,
    };
    use std::sync::Arc;
    fn empty_capture() -> AudioCaptureSnapshot {
        let mut worker = AudioCaptureWorker::new(
            Box::new(|| panic!("row tests never open a native device")),
            AudioCapabilities {
                backend: "reorder test",
                input: false,
                output: false,
                device_enumeration: false,
                exclude_own_audio: false,
                note: None,
            },
        )
        .unwrap();
        let snapshot = worker.snapshot();
        worker.join().unwrap();
        snapshot
    }
    struct Harness {
        ctx: egui::Context,
        session: Session,
        capture: AudioCaptureSnapshot,
        intents: Vec<Intent>,
        list: List,
        time: f64,
    }
    impl Harness {
        fn scenes(count: usize, scale: f32) -> Self {
            let mut session = Session::new();
            for n in 1..count {
                session
                    .apply(Command::AddScene {
                        name: format!("Scene {n}"),
                    })
                    .unwrap();
            }
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(scale);
            let mut harness = Self {
                ctx,
                session,
                capture: empty_capture(),
                intents: vec![],
                list: List::Scenes,
                time: 0.0,
            };
            harness.frame(vec![]);
            harness.frame(vec![]);
            harness
        }
        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            self.time += 1.0 / 60.0;
            let layout = Layout {
                arrangement: super::super::dock_layout::DockLayout::bottom_row(),
                columns: Default::default(),
                scenes: self.list == List::Scenes,
                sources: matches!(self.list, List::Sources(_)),
                mixer: false,
                transitions: false,
                controls: false,
            };
            self.ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(420.0, 460.0),
                    )),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        docks::show(
                            ui,
                            &self.session.snapshot(),
                            self.session.preview(),
                            None,
                            &layout,
                            DockViewState {
                                editing_enabled: true,
                                recording: Default::default(),
                                streaming: Default::default(),
                                transition: TransitionViewState {
                                    session: self.session.identity(),
                                    studio: false,
                                    active: false,
                                    manual: None,
                                    preset_focus: None,
                                },
                                audio: &self.capture,
                                active_audio: &[],
                                program_audio: None,
                                program_status: None,
                                media: None,
                            },
                            &mut self.intents,
                        )
                    });
                },
            )
        }
        fn rect(&self, id: Id) -> egui::Rect {
            self.ctx
                .data_mut(|data| data.get_temp(egui::Id::new(("reorder-row", self.list, id.get()))))
                .unwrap()
        }
        fn press(&mut self, p: egui::Pos2) {
            self.frame(vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        fn release(&mut self, p: egui::Pos2) {
            self.frame(vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        fn command(&mut self) -> Command {
            assert_eq!(
                self.intents.len(),
                1,
                "motion and release publish exactly one intention"
            );
            let Intent::Reorder {
                session,
                revision,
                command,
            } = self.intents.pop().unwrap()
            else {
                panic!("typed reorder required")
            };
            assert_eq!(session, self.session.identity());
            assert_eq!(revision, self.session.snapshot().revision());
            command
        }
    }
    #[test]
    fn actual_scene_pointer_drag_at_2x_is_one_undo_and_preserves_selection() {
        let mut h = Harness::scenes(3, 2.0);
        let original = h.session.snapshot();
        let first = original.scenes()[0].id();
        let last = original.scenes()[2].id();
        let selection = (h.session.preview(), h.session.program());
        let start = h.rect(first).center();
        let target = egui::pos2(h.rect(last).center().x, h.rect(last).bottom() - 1.0);
        h.press(start);
        h.frame(vec![egui::Event::PointerMoved(target)]);
        assert!(h.intents.is_empty());
        assert!(Arc::ptr_eq(&original, &h.session.snapshot()));
        assert!(h.ctx.pixels_per_point() > 1.9);
        h.release(target);
        let command = h.command();
        h.session.apply(command).unwrap();
        assert_eq!(h.session.snapshot().scenes()[2].id(), first);
        assert_eq!(h.session.snapshot().revision(), original.revision() + 1);
        assert_eq!((h.session.preview(), h.session.program()), selection);
        h.session.apply(Command::Undo).unwrap();
        assert_eq!(h.session.snapshot().scenes(), original.scenes());
    }
    #[test]
    fn escape_foreign_session_revision_and_outside_release_cancel_real_drags() {
        for mode in 0..4 {
            let mut h = Harness::scenes(3, 1.0);
            let snapshot = h.session.snapshot();
            let first = snapshot.scenes()[0].id();
            let last = snapshot.scenes()[2].id();
            let start = h.rect(first).center();
            let end = egui::pos2(h.rect(last).center().x, h.rect(last).bottom() - 1.0);
            h.press(start);
            h.frame(vec![egui::Event::PointerMoved(end)]);
            match mode {
                0 => {
                    h.frame(vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }]);
                }
                1 => {
                    h.session
                        .apply(Command::RenameScene {
                            scene: first,
                            name: "Changed during drag".into(),
                        })
                        .unwrap();
                }
                2 => {
                    h.session = Session::from_snapshot((*snapshot).clone(), true).unwrap();
                }
                _ => {}
            }
            h.release(if mode == 3 {
                egui::pos2(700.0, 500.0)
            } else {
                end
            });
            assert!(
                h.intents.is_empty(),
                "cancel mode {mode} must not publish a stale reorder"
            );
            assert_eq!(h.session.snapshot().scenes()[0].id(), first);
        }
    }
    #[test]
    fn source_drag_uses_reversed_layer_order_and_locked_row_never_starts_drag() {
        let mut h = Harness::scenes(1, 1.0);
        let scene = h.session.preview();
        for n in 0..3 {
            h.session
                .apply(Command::AddColorSource {
                    scene,
                    name: format!("Layer {n}"),
                    rgba: [255, 0, 0, 255],
                })
                .unwrap();
        }
        h.list = List::Sources(scene);
        h.frame(vec![]);
        h.frame(vec![]);
        let original = h.session.snapshot();
        let bottom = original.scene(scene).unwrap().items()[0].id();
        let top = original.scene(scene).unwrap().items()[2].id();
        let start = h.rect(top).center();
        let end = egui::pos2(h.rect(bottom).center().x, h.rect(bottom).bottom() - 1.0);
        h.press(start);
        h.frame(vec![egui::Event::PointerMoved(end)]);
        h.release(end);
        let command = h.command();
        h.session.apply(command).unwrap();
        assert_eq!(
            h.session.snapshot().scene(scene).unwrap().items()[0].id(),
            top
        );
        h.session.apply(Command::Undo).unwrap();
        assert_eq!(
            h.session.snapshot().scene(scene).unwrap().items(),
            original.scene(scene).unwrap().items()
        );
        h.session
            .apply(Command::SetLocked {
                scene,
                item: top,
                locked: true,
            })
            .unwrap();
        h.frame(vec![]);
        let before = h.session.snapshot();
        let start = h.rect(top).center();
        h.press(start);
        h.frame(vec![egui::Event::PointerMoved(end)]);
        h.release(end);
        assert!(h.intents.is_empty());
        assert!(Arc::ptr_eq(&before, &h.session.snapshot()));
    }
    #[test]
    fn actual_edge_drag_scrolls_long_scene_list_before_one_release() {
        let mut h = Harness::scenes(25, 1.0);
        let first = h.session.snapshot().scenes()[0].id();
        h.press(h.rect(first).center());
        let initial = h.ctx.data_mut(|data| {
            data.get_temp::<egui::Rect>(egui::Id::new(("reorder-viewport", h.list)))
        });
        // The viewport is recorded by finish once a real payload starts.
        h.frame(vec![egui::Event::PointerMoved(
            h.rect(first).center() + egui::vec2(0.0, 12.0),
        )]);
        let viewport = h
            .ctx
            .data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("reorder-viewport", h.list)))
            })
            .or(initial)
            .unwrap();
        let edge = egui::pos2(viewport.center().x, viewport.bottom() - 2.0);
        h.frame(vec![egui::Event::PointerMoved(edge)]);
        for _ in 0..100 {
            h.frame(vec![]);
        }
        assert!(h.intents.is_empty());
        h.release(edge);
        let command = h.command();
        let Command::MoveScene { index, .. } = command else {
            panic!("scene reorder expected")
        };
        assert!(
            index > 6,
            "a stationary edge drag must reach rows outside the initial viewport"
        );
    }
    #[test]
    fn source_visibility_and_lock_remain_independent_pointer_controls() {
        let mut h = Harness::scenes(1, 1.0);
        let scene = h.session.preview();
        h.session
            .apply(Command::AddColorSource {
                scene,
                name: "Independent row controls".into(),
                rgba: [0, 0, 0, 255],
            })
            .unwrap();
        h.list = List::Sources(scene);
        h.frame(vec![]);
        for caption in ["Show", "Lock"] {
            h.frame(vec![]);
            let item = h.session.snapshot().scene(scene).unwrap().items()[0].id();
            let point = h
                .ctx
                .data_mut(|data| {
                    data.get_temp::<egui::Rect>(egui::Id::new((
                        "group-row-control",
                        item.get(),
                        caption,
                    )))
                })
                .expect("actual compact source toggle is painted")
                .center();
            h.press(point);
            h.release(point);
            assert_eq!(h.intents.len(), 1);
            let Intent::Command(command) = h.intents.pop().unwrap() else {
                panic!("checkbox must not submit a reorder");
            };
            assert!(matches!(
                (&command, caption),
                (Command::SetVisible { visible: false, .. }, "Show")
                    | (Command::SetLocked { locked: true, .. }, "Lock")
            ));
            h.session.apply(command).unwrap();
            assert!(!egui::DragAndDrop::has_payload_of_type::<Payload>(&h.ctx));
        }
    }
}
