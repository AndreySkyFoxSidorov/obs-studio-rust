//! Window presentation only. Exact OBS32 default placement plus a SkyLive preset.
use super::{Intent, Layout, dock_columns::Plan, docks::DockViewState};
use eframe::egui;
use skylive_core::{Id, StudioSnapshot};

const SIDE: &str = "obs32-side-docks";
const SCENES: &str = "obs32-side-scenes";
const BOTTOM: &str = "main-docks";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DockPreset {
    #[default]
    Obs,
    BottomRow,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockSize {
    SideWidth,
    ScenesHeight,
    BottomHeight,
}
#[derive(Clone)]
pub struct DockLayout {
    pub preset: DockPreset,
    pub locked: bool,
    pub full_height: bool,
    pub scene_notes_visible: bool,
    side_width: Option<u16>,
    scenes_height: Option<u16>,
    bottom_height: Option<u16>,
    side_selected: usize,
    side_tab_ids: [Option<egui::Id>; 2],
}
impl Default for DockLayout {
    fn default() -> Self {
        Self {
            preset: DockPreset::Obs,
            locked: false,
            full_height: true,
            scene_notes_visible: true,
            side_width: None,
            scenes_height: None,
            bottom_height: None,
            side_selected: 1,
            side_tab_ids: [None; 2],
        }
    }
}
impl DockLayout {
    pub fn reserves_keyboard(&self, ctx: &egui::Context) -> bool {
        ctx.memory(|memory| {
            memory
                .focused()
                .is_some_and(|focused| self.side_tab_ids.contains(&Some(focused)))
        })
    }
    #[cfg(test)]
    pub fn bottom_row() -> Self {
        Self {
            preset: DockPreset::BottomRow,
            ..Self::default()
        }
    }
    pub fn set_size(&mut self, ctx: &egui::Context, size: DockSize, value: u16) {
        let (field, id, range) = match size {
            DockSize::SideWidth => (&mut self.side_width, SIDE, 180..=480),
            DockSize::ScenesHeight => (&mut self.scenes_height, SCENES, 100..=600),
            DockSize::BottomHeight => (&mut self.bottom_height, BOTTOM, 140..=600),
        };
        if range.contains(&value) {
            *field = Some(value);
            ctx.data_mut(|data| {
                data.remove::<egui::containers::panel::PanelState>(egui::Id::new(id))
            });
        }
    }
    fn effective(&self, width: f32) -> DockPreset {
        // At small logical widths preserve the existing accessible compact tabs
        // and pinned Controls instead of squeezing a useless lateral pane.
        if width < 800.0 {
            DockPreset::BottomRow
        } else {
            self.preset
        }
    }
    fn size(&self, ctx: &egui::Context, size: DockSize) -> u16 {
        let (id, selected, default, horizontal) = match size {
            DockSize::SideWidth => (SIDE, self.side_width, 320, true),
            DockSize::ScenesHeight => (SCENES, self.scenes_height, 220, false),
            DockSize::BottomHeight => (BOTTOM, self.bottom_height, 260, false),
        };
        egui::containers::panel::PanelState::load(ctx, egui::Id::new(id)).map_or(
            selected.unwrap_or(default),
            |state| {
                if horizontal {
                    state.rect.width()
                } else {
                    state.rect.height()
                }
                .round()
                .clamp(0.0, 600.0) as u16
            },
        )
    }
}
pub fn forget_sizes(ctx: &egui::Context) {
    super::scene_notes_dock::forget_sizes(ctx);
    ctx.data_mut(|data| {
        for id in [SIDE, SCENES, BOTTOM] {
            data.remove::<egui::containers::panel::PanelState>(egui::Id::new(id));
        }
    });
}

pub fn menu(ui: &mut egui::Ui, layout: &Layout, intents: &mut Vec<Intent>) {
    ui.menu_button(crate::i18n::tr(ui, "Layout"), |ui| {
        for (preset, label, hint) in [
            (
                DockPreset::Obs,
                "OBS 32 default",
                "Scenes and Sources at the left; Mixer, Transitions and Controls below Preview.",
            ),
            (
                DockPreset::BottomRow,
                "Bottom row",
                "Place all five docks below Preview. This is a SkyLive convenience layout.",
            ),
        ] {
            if ui
                .radio(
                    layout.arrangement.preset == preset,
                    crate::i18n::tr(ui, label),
                )
                .on_hover_text(crate::i18n::tr(ui, hint))
                .clicked()
            {
                intents.push(Intent::SetDockPreset(preset));
                ui.close_menu();
            }
        }
    })
    .response
    .on_hover_text(crate::i18n::tr(
        ui,
        "Choose a dock arrangement for this window.",
    ));
    let mut locked = layout.arrangement.locked;
    if ui
        .checkbox(
            &mut locked,
            crate::i18n::tr_key(ui, "Basic.MainMenu.Docks.LockDocks").replace('&', ""),
        )
        .on_hover_text(crate::i18n::tr(
            ui,
            "Prevent dock resizing while keeping source, mixer and output controls usable.",
        ))
        .changed()
    {
        intents.push(Intent::SetDocksLocked(locked));
    }
    let side = layout.arrangement.preset == DockPreset::Obs;
    let mut full_height = layout.arrangement.full_height;
    if ui
        .add_enabled(
            side,
            egui::Checkbox::new(
                &mut full_height,
                crate::i18n::tr_key(ui, "Basic.MainMenu.Docks.SideDocks").replace('&', ""),
            ),
        )
        .on_hover_text(crate::i18n::tr(
            ui,
            "Let the left docks extend beside the bottom docks.",
        ))
        .on_disabled_hover_text(crate::i18n::tr(
            ui,
            "This option applies to the OBS 32 default layout.",
        ))
        .changed()
    {
        intents.push(Intent::SetFullHeightDocks(full_height));
    }
    ui.menu_button(crate::i18n::tr(ui, "Dock sizes"), |ui| {
        ui.add_enabled_ui(!locked, |ui| {
            for (kind, label, min, max) in [
                (DockSize::SideWidth, "Side dock width", 180, 480),
                (DockSize::ScenesHeight, "Scenes dock height", 100, 600),
                (DockSize::BottomHeight, "Bottom dock height", 140, 600),
            ] {
                let mut value = layout.arrangement.size(ui.ctx(), kind);
                ui.horizontal(|ui| {
                    let name = ui.label(crate::i18n::tr(ui, label));
                    if ui.add_enabled(side || kind == DockSize::BottomHeight,
                        egui::DragValue::new(&mut value).range(min..=max).clamp_existing_to_range(false).speed(1))
                        .labelled_by(name.id)
                        .on_hover_text(crate::i18n::tr(ui, "Set the dock size in interface points. The available window space limits the result."))
                        .on_disabled_hover_text(crate::i18n::tr(ui, "Unlock docks and choose a layout with this dock to change its size.")).changed() {
                        intents.push(Intent::SetDockSize(kind, value));
                    }
                });
            }
        });
    }).response.on_hover_text(crate::i18n::tr(ui, "Adjust dock dimensions using the keyboard or pointer."));
    ui.separator();
}

pub fn show(
    ctx: &egui::Context,
    model: &StudioSnapshot,
    scene: Id,
    selected: Option<Id>,
    layout: &mut Layout,
    state: DockViewState<'_>,
    intents: &mut Vec<Intent>,
) {
    super::dock_toolbar::begin_context(ctx);
    #[cfg(test)]
    ctx.data_mut(|data| {
        for index in 0_usize..5 {
            data.remove::<egui::Rect>(egui::Id::new(("actual-main-dock", index)));
        }
    });
    let side = layout.arrangement.effective(ctx.available_rect().width()) == DockPreset::Obs
        && (layout.scenes || layout.sources);
    if !side {
        surrender_side_tabs(ctx, &mut layout.arrangement);
    }
    if side && layout.arrangement.full_height {
        side_panel(ctx, model, scene, selected, layout, state.clone(), intents);
    }
    let visible = [
        !side && layout.scenes,
        !side && layout.sources,
        layout.mixer,
        layout.transitions,
        layout.controls,
    ];
    if visible.into_iter().any(|value| value) {
        let default = layout.arrangement.bottom_height.map_or(260.0, f32::from);
        let max = (ctx.available_rect().height() * 0.65).max(140.0);
        egui::TopBottomPanel::bottom(BOTTOM)
            .resizable(!layout.arrangement.locked)
            .default_height(default)
            .min_height(140.0)
            .max_height(max)
            .show(ctx, |ui| {
                let (area, _) = ui.allocate_exact_size(
                    ui.available_size().max(egui::vec2(1.0, 1.0)),
                    egui::Sense::hover(),
                );
                let plan = layout
                    .columns
                    .show_locked(ui, area, visible, layout.arrangement.locked);
                super::docks::show_plan(ui, model, scene, selected, state.clone(), intents, plan);
            });
    }
    if super::source_toolbar::source_bar_visible(ctx) {
        egui::TopBottomPanel::bottom("selected-source-toolbar")
            .resizable(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("selected-source-toolbar-scroll")
                    .max_height(96.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.add_enabled_ui(state.editing_enabled, |ui| {
                            super::source_toolbar::show(
                                ui, model, scene, selected, state.transition.session,
                                state.media, intents,
                            );
                        });
                    });
            });
    }
    if side && !layout.arrangement.full_height {
        side_panel(ctx, model, scene, selected, layout, state, intents);
    }
}

fn side_panel(
    ctx: &egui::Context,
    model: &StudioSnapshot,
    scene: Id,
    selected: Option<Id>,
    layout: &mut Layout,
    state: DockViewState<'_>,
    intents: &mut Vec<Intent>,
) {
    let available = ctx.available_rect();
    let default = layout
        .arrangement
        .side_width
        .map_or((available.width() * 0.3).min(320.0), f32::from);
    let max_width = (available.width() - 520.0).clamp(180.0, 480.0);
    egui::SidePanel::left(SIDE)
        .resizable(!layout.arrangement.locked)
        .default_width(default)
        .min_width(180.0)
        .max_width(max_width)
        .show(ctx, |ui| {
            let both = layout.scenes && layout.sources;
            if both && ui.available_height() < 340.0 {
                let mut focus_next = None;
                ui.horizontal_wrapped(|ui| {
                    for (index, label) in [(0, "Scenes"), (1, "Sources")] {
                        let response = ui
                            .selectable_value(
                                &mut layout.arrangement.side_selected,
                                index,
                                crate::i18n::tr(ui, label),
                            )
                            .on_hover_text(crate::i18n::tr(
                                ui,
                                "Show this dock in the compact side panel.",
                            ));
                        layout.arrangement.side_tab_ids[index] = Some(response.id);
                        if response.has_focus() {
                            let changed = ui.input_mut(|input| {
                                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)
                                    || input
                                        .consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)
                            });
                            if changed {
                                layout.arrangement.side_selected = 1 - index;
                                focus_next = Some(1 - index);
                            }
                        }
                    }
                });
                if let Some(next) =
                    focus_next.and_then(|index| layout.arrangement.side_tab_ids[index])
                {
                    ui.ctx().memory_mut(|memory| memory.request_focus(next));
                }
                single(
                    ui,
                    model,
                    scene,
                    selected,
                    state,
                    intents,
                    layout.arrangement.side_selected,
                );
            } else if both {
                surrender_side_tabs(ui.ctx(), &mut layout.arrangement);
                let available = ui.available_height();
                let default = layout
                    .arrangement
                    .scenes_height
                    .map_or(available * 0.4, f32::from);
                egui::TopBottomPanel::top(SCENES)
                    .resizable(!layout.arrangement.locked)
                    .default_height(default)
                    .min_height(100.0)
                    .max_height((available - 160.0).max(100.0))
                    .frame(egui::Frame::NONE)
                    .show_inside(ui, |ui| {
                        single(ui, model, scene, selected, state.clone(), intents, 0);
                    });
                single(ui, model, scene, selected, state, intents, 1);
            } else {
                surrender_side_tabs(ui.ctx(), &mut layout.arrangement);
                single(
                    ui,
                    model,
                    scene,
                    selected,
                    state,
                    intents,
                    usize::from(!layout.scenes),
                );
            }
        });
}
fn surrender_side_tabs(ctx: &egui::Context, layout: &mut DockLayout) {
    for id in layout.side_tab_ids.iter_mut().filter_map(Option::take) {
        ctx.memory_mut(|memory| memory.surrender_focus(id));
    }
}
fn single(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    scene: Id,
    selected: Option<Id>,
    state: DockViewState<'_>,
    intents: &mut Vec<Intent>,
    index: usize,
) {
    let (area, _) = ui.allocate_exact_size(
        ui.available_size().max(egui::vec2(1.0, 1.0)),
        egui::Sense::hover(),
    );
    let mut rects = [None; 5];
    rects[index] = Some(area);
    super::docks::show_plan(ui, model, scene, selected, state, intents, Plan { rects });
}

#[cfg(test)]
#[path = "dock_layout_tests.rs"]
mod tests;
