//! Dock views consume immutable data and return intentions; no model writes.
#[cfg(test)]
use super::Layout;
use super::list_reorder::{self, List, Scope};
use super::mixer::MixerViewState;
use super::{
    AudioEdit, DisplayEdit, Intent, ProgramAudioView, TransitionViewState, dialogs::Dialog,
    transition_reference_editor, transition_reference_label,
};
use eframe::egui;
use skylive_core::{
    AudioFlow, Command, Id, MAX_QUICK_TRANSITIONS, MAX_TRANSITION_DURATION_MS,
    MAX_TRANSITION_PRESETS, MIN_TRANSITION_DURATION_MS, SourceKind, StudioSnapshot,
    TransitionDirection, TransitionKind, TransitionRef, TransitionSelection,
};
use skylive_host::{
    audio_capture::{AudioCaptureSnapshot, ProgramTransportStats},
    transition::FrameScene,
};

#[derive(Clone)]
pub struct DockViewState<'a> {
    pub editing_enabled: bool,
    pub recording: super::RecordingViewState,
    pub streaming: super::StreamingViewState,
    pub transition: TransitionViewState,
    pub audio: &'a AudioCaptureSnapshot,
    /// Actual Program composition views; Studio Preview is supplied separately.
    pub active_audio: &'a [FrameScene],
    pub program_audio: Option<&'a ProgramAudioView>,
    pub program_status: Option<&'a ProgramTransportStats>,
    pub media: Option<super::media_controls::MediaTransportView<'a>>,
}

#[cfg(test)]
pub fn show(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    scene: Id,
    selected: Option<Id>,
    layout: &Layout,
    state: DockViewState<'_>,
    intents: &mut Vec<Intent>,
) {
    let mut local = layout.clone();
    show_resizable(ui, model, scene, selected, &mut local, state, intents);
}
#[cfg(test)]
pub fn show_resizable(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    scene: Id,
    selected: Option<Id>,
    layout: &mut Layout,
    state: DockViewState<'_>,
    intents: &mut Vec<Intent>,
) {
    super::dock_toolbar::begin(ui);
    let enabled = [
        layout.scenes,
        layout.sources,
        layout.mixer,
        layout.transitions,
        layout.controls,
    ];
    let count = enabled.iter().filter(|value| **value).count();
    if count == 0 {
        ui.label(crate::i18n::tr(ui, "Enable docks from the Docks menu."));
        return;
    }
    let (area, _) = ui.allocate_exact_size(
        ui.available_size().max(egui::vec2(1.0, 1.0)),
        egui::Sense::hover(),
    );
    let plan = layout
        .columns
        .show_locked(ui, area, enabled, layout.arrangement.locked);
    show_plan(ui, model, scene, selected, state, intents, plan);
}

pub(super) fn show_plan(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    scene: Id,
    selected: Option<Id>,
    state: DockViewState<'_>,
    intents: &mut Vec<Intent>,
    plan: super::dock_columns::Plan,
) {
    let DockViewState {
        editing_enabled,
        recording,
        streaming,
        transition: state,
        audio,
        active_audio,
        program_audio,
        program_status,
        media: _,
    } = state;
    for (index, rect) in plan.rects.into_iter().enumerate() {
        let Some(rect) = rect else {
            continue;
        };
        #[cfg(test)]
        ui.ctx()
            .data_mut(|data| data.insert_temp(egui::Id::new(("actual-main-dock", index)), rect));
        ui.painter().rect(
            rect,
            2.0,
            crate::theme::dock_background(ui),
            egui::Stroke::new(1.0, crate::theme::border(ui)),
            egui::StrokeKind::Inside,
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("main-dock", index))
                .max_rect(rect.shrink(5.0))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        child.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        if index != 4 && !editing_enabled {
            child.disable();
        }
        match index {
            0 => {
                scenes(&mut child, model, scene, state.session, intents);
            }
            1 => {
                sources(
                    &mut child,
                    model,
                    scene,
                    selected,
                    state.session,
                    intents,
                );
            }
            2 => {
                super::mixer::show(
                    &mut child,
                    model,
                    scene,
                    MixerViewState {
                        session: state.session,
                        audio,
                        program_views: active_audio,
                        preview: state.studio.then_some(scene),
                        program_audio,
                        program_status,
                    },
                    intents,
                );
            }
            3 => {
                egui::ScrollArea::vertical()
                    .id_salt("transition-dock-scroll")
                    .max_height(child.available_height())
                    .show(&mut child, |ui| transitions(ui, model, state, intents));
            }
            _ => controls_with_outputs(
                &mut child,
                model,
                state.studio,
                recording.clone(),
                streaming.clone(),
                editing_enabled,
                intents,
            ),
        }
    }
}

#[cfg(test)]
fn dock_rects(area: egui::Rect, enabled: [bool; 5]) -> [Option<egui::Rect>; 5] {
    super::dock_columns::DockColumns::default().rects(area, enabled)
}

fn scenes(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    selected: Id,
    session: u64,
    intents: &mut Vec<Intent>,
) {
    // Keep this confirmation scoped to the document that owned the focused
    // scene. A later selection or project change must not delete another row.
    let remove_key = egui::Id::new("scene-keyboard-remove-confirmation");
    let toolbars = super::source_toolbar::listbox_bars_visible(ui.ctx());
    ui.strong(crate::i18n::tr(ui, "Scenes"))
        .on_hover_text(crate::i18n::tr(ui, "Right-click the dock heading for Add and other scene actions."))
        .context_menu(|ui| scene_context(ui, model, selected, session, intents));
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("scene-list")
        .max_height((ui.available_height() - if toolbars { 56.0 } else { 28.0 }).max(24.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let scope = Scope {
                session,
                revision: model.revision(),
                list: List::Scenes,
            };
            let mut rows = Vec::with_capacity(model.scenes().len());
            let mut keyboard_rows = Vec::with_capacity(model.scenes().len());
            let grid = ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::new("scene-grid"))).unwrap_or(false);
            let columns = if grid { (ui.available_width() / 110.0).floor().max(1.0) as usize } else { 1 };
            let width = ((ui.available_width() - ui.spacing().item_spacing.x * columns.saturating_sub(1) as f32) / columns as f32).max(1.0);
            egui::Grid::new("scene-list-presentation").num_columns(columns).show(ui, |ui| {
            for (index, scene) in model.scenes().iter().enumerate() {
                ui.push_id(scene.id().get(), |ui| {
                    let response = ui
                        .add_sized(
                            [width, if grid { 64.0 } else { 22.0 }],
                            egui::Button::new(scene.name())
                                .selected(scene.id() == selected)
                                .frame(grid)
                                .truncate()
                                .sense(egui::Sense::click_and_drag()),
                        )
                        .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.Scenes.KeyboardNavigationHelp","Select a scene. Use arrow keys, Home or End to navigate the focused scene list."))
                        .on_disabled_hover_text(crate::hints::text(ui, "Scene"));
                    if response.clicked() {
                        intents.push(Intent::SelectScene(scene.id()));
                    }
                    list_reorder::row(ui, &response, scope, scene.id(), false);
                    rows.push((scene.id(), response.rect));
                    keyboard_rows.push((scene.id(), response.id, response.rect, response.has_focus()));
                    response.context_menu(|ui| {
                        scene_context(ui, model, scene.id(), scope.session, intents);
                    });
                });
                if (index + 1) % columns == 0 { ui.end_row(); }
            }
            });
            // Match the native scene list: navigation applies only when a scene
            // button owns focus, so preview nudges and text fields keep their keys.
            if ui.is_enabled() {
                if let Some(index) = keyboard_rows.iter().position(|row| row.3) {
                    // OBS scopes scene rename to the scene dock. Act on the
                    // focused row, and leave text editors and other docks alone.
                    let rename_key = if cfg!(target_os = "macos") { egui::Key::Enter } else { egui::Key::F2 };
                    if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, rename_key)) {
                        if let Some(scene) = model.scene(keyboard_rows[index].0) {
                            intents.push(Intent::OpenDialog(Dialog::RenameScene {
                                scene: scene.id(),
                                name: scene.name().to_owned(),
                            }));
                        }
                    }
                    let remove_requested = ui.input_mut(|input| {
                        input.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                            || input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
                    });
                    if remove_requested && model.scenes().len() > 1 {
                        confirm_scene_removal(ui, model, keyboard_rows[index].0, session);
                    }
                    let last = keyboard_rows.len().saturating_sub(1);
                    let target = ui.input_mut(|input| {
                        if input.consume_key(egui::Modifiers::NONE, egui::Key::Home) { Some(0) }
                        else if input.consume_key(egui::Modifiers::NONE, egui::Key::End) { Some(last) }
                        else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) { Some(index.saturating_sub(columns)) }
                        else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) { Some(index.saturating_add(columns).min(last)) }
                        else if grid && input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) { Some(index.saturating_sub(1)) }
                        else if grid && input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) { Some(index.saturating_add(1).min(last)) }
                        else { None }
                    });
                    if let Some((scene, widget, rect, _)) = target.and_then(|target| keyboard_rows.get(target)).copied() {
                        ui.ctx().memory_mut(|memory| memory.request_focus(widget));
                        ui.scroll_to_rect(rect, Some(egui::Align::Center));
                        if scene != selected { intents.push(Intent::SelectScene(scene)); }
                    }
                }
            }
            list_reorder::finish(ui, scope, &rows, intents);
        });
    if let Some(scene) = model.scene(selected) {
        let effective = scene.transition_override().unwrap_or(model.transition());
        let summary = crate::i18n::tf(
            ui,
            "Transition: {}, {} ms ({})",
            &[
                (
                    "{}",
                    transition_reference_label(ui, model, effective.reference()),
                ),
                ("{}", (effective.duration_ms()).to_string()),
                (
                    "{}",
                    crate::i18n::tr(
                        ui,
                        if scene.transition_override().is_some() {
                            "scene override"
                        } else {
                            "Global"
                        },
                    ),
                ),
            ],
        );
        ui.add(egui::Label::new(&summary).truncate())
            .on_hover_text(&summary);
    }
    if let Some((owner, revision, scene, name)) = ui.ctx().data(|data|
        data.get_temp::<(u64, u64, Id, String)>(remove_key)) {
        let valid = ui.is_enabled() && owner == session && revision == model.revision()
            && model.scenes().len() > 1 && model.scene(scene).is_some();
        if !valid {
            ui.ctx().data_mut(|data| { data.remove::<(u64, u64, Id, String)>(remove_key); });
        } else {
            let mut close = false;
            let response = egui::Modal::new(egui::Id::new("scene-keyboard-remove-modal"))
                .show(ui.ctx(), |ui| {
                    ui.set_min_width(300.0);
                    ui.heading(crate::i18n::tr_key(ui, "ConfirmRemove.Title"));
                    ui.label(crate::i18n::tr_key(ui, "ConfirmRemove.Text").replace("%1", &name));
                    ui.separator();
                    let actions = [
                        super::modal_footer::Action::new("Remove", true, "Remove"),
                        super::modal_footer::Action::new("Cancel", true, "Cancel"),
                    ];
                    match super::modal_footer::show(ui, &actions) {
                        Some(0) => { intents.push(Intent::Command(Command::RemoveScene { scene })); close = true; }
                        Some(1) => close = true,
                        _ => {}
                    }
                });
            if close || response.should_close() {
                ui.ctx().data_mut(|data| { data.remove::<(u64, u64, Id, String)>(remove_key); });
            }
        }
    }
    if !toolbars { return; }
    ui.separator();
    ui.horizontal(|ui| {
        super::dock_toolbar::size(ui, 3);
        if super::dock_toolbar::button(ui, "Scenes", "Add", super::dock_toolbar::Icon::Add, true)
            .clicked()
        {
            intents.push(Intent::OpenDialog(Dialog::AddScene {
                name: unique_name("Scene", model.scenes().iter().map(|scene| scene.name())),
            }));
        }
        if super::dock_toolbar::button(
            ui,
            "Scenes",
            "Remove",
            super::dock_toolbar::Icon::Remove,
            model.scenes().len() > 1,
        )
        .clicked()
        {
            confirm_scene_removal(ui, model, selected, session);
        }
        super::dock_toolbar::menu(
            ui,
            "Scenes",
            "Scene Actions",
            super::dock_toolbar::Icon::More,
            true,
            |ui| {
                scene_actions(ui, model, selected, session, intents);
            },
        );
    });
}
fn confirm_scene_removal(ui: &egui::Ui, model: &StudioSnapshot, scene: Id, session: u64) {
    if let Some(scene) = model.scene(scene) {
        ui.ctx().data_mut(|data| data.insert_temp(
            egui::Id::new("scene-keyboard-remove-confirmation"),
            (session, model.revision(), scene.id(), scene.name().to_owned()),
        ));
    }
}
fn add_scene_action(ui: &mut egui::Ui, model: &StudioSnapshot, intents: &mut Vec<Intent>) {
    if ui.button(crate::i18n::tr(ui, "Add"))
        .on_hover_text(crate::hints::text(ui, "Add Scene")).clicked() {
        intents.push(Intent::OpenDialog(Dialog::AddScene {
            name: unique_name("Scene", model.scenes().iter().map(|scene| scene.name())),
        }));
        ui.close_menu();
    }
}
fn scene_context(ui: &mut egui::Ui, model: &StudioSnapshot, selected: Id, session: u64, intents: &mut Vec<Intent>) {
    add_scene_action(ui, model, intents);
    super::screenshot::button(ui,session,model.revision(),skylive_host::screenshot::Target::Scene {scene:selected},model.scene(selected).is_some(),intents);
    if ui.add_enabled(model.scenes().len() > 1, egui::Button::new(crate::i18n::tr(ui, "Remove"))
        .shortcut_text("Delete / Backspace"))
        .on_hover_text(crate::hints::text(ui, "Remove")).clicked() {
        confirm_scene_removal(ui, model, selected, session);
        ui.close_menu();
    }
    ui.separator();
    scene_actions(ui, model, selected, session, intents);
}
fn scene_actions(ui: &mut egui::Ui, model: &StudioSnapshot, selected: Id, session: u64, intents: &mut Vec<Intent>) {
    if let Some(scene)=model.scene(selected){let mut show=scene.show_in_multiview();if ui.checkbox(&mut show,crate::i18n::tr(ui,"Show in Multiview")).on_hover_text(crate::i18n::tr(ui,"Include this scene in Multiview scene tiles without changing Program, Preview or its sources.")).changed(){intents.push(Intent::Projector(crate::projectors::Action::SceneVisibility{session,revision:model.revision(),scene:selected,show}));ui.close_menu();}}
    ui.menu_button(crate::i18n::tr(ui,"Scene Projector"),|ui|crate::projectors::menu(ui,session,model.revision(),skylive_host::projector::Target::Scene{scene:selected},intents));
    let selected_index = model
        .scenes()
        .iter()
        .position(|entry| entry.id() == selected);
    ui.menu_button(crate::i18n::tr_key(ui, "Basic.MainMenu.Edit.Order"), |ui| {
        for (key, hint, index) in [
            ("MoveUp", "actionMoveUp", selected_index.and_then(|index| index.checked_sub(1))),
            ("MoveDown", "actionMoveDown", selected_index.and_then(|index| index.checked_add(1)).filter(|index| *index < model.scenes().len())),
            ("MoveToTop", "actionMoveToTop", selected_index.map(|_| 0)),
            ("MoveToBottom", "actionMoveToBottom", model.scenes().len().checked_sub(1)),
        ] {
            let enabled = selected_index.is_some() && index.is_some() && index != selected_index;
            let label = format!("Basic.MainMenu.Edit.Order.{key}");
            if ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr_key(ui, &label)))
                .on_hover_text(crate::hints::text(ui, hint)).clicked() {
                if let Some(index) = index {
                    intents.push(Intent::Reorder { session, revision: model.revision(), command: Command::MoveScene { scene: selected, index } });
                }
                ui.close_menu();
            }
        }
    });
    if let Some(scene) = model.scene(selected) {
        if ui
            .add(egui::Button::new(crate::i18n::tr(ui, "Rename"))
                .shortcut_text(if cfg!(target_os = "macos") { "Enter" } else { "F2" }))
            .on_hover_text(crate::hints::text(ui, "Rename"))
            .on_disabled_hover_text(crate::hints::text(ui, "Rename"))
            .clicked()
        {
            intents.push(Intent::OpenDialog(Dialog::RenameScene {
                scene: selected,
                name: scene.name().into(),
            }));
        }
        if ui
            .button(crate::i18n::tr(ui, "Duplicate"))
            .on_hover_text(crate::hints::text(ui, "Duplicate"))
            .on_disabled_hover_text(crate::hints::text(ui, "Duplicate"))
            .clicked()
        {
            intents.push(Intent::OpenDialog(Dialog::DuplicateScene {
                scene: selected,
                name: unique_name(
                    &format!("{} Copy", scene.name()),
                    model.scenes().iter().map(|scene| scene.name()),
                ),
            }));
        }
        scene_filters_action(ui, model, selected, session, intents);
        if ui
            .button(crate::i18n::tr(ui, "Transition Override"))
            .on_hover_text(crate::hints::text(ui, "Transition Override"))
            .on_disabled_hover_text(crate::hints::text(ui, "Transition Override"))
            .clicked()
        {
            intents.push(Intent::OpenDialog(scene_transition_dialog(model, selected)));
        }
        if ui
            .add_enabled(
                model
                    .preferences()
                    .plugin_enabled(skylive_core::SCENE_NOTES_PLUGIN),
                egui::Button::new(crate::i18n::tr(ui, "Scene Notes")),
            )
            .on_hover_text(crate::hints::text(ui, "Scene Notes"))
            .on_disabled_hover_text(crate::i18n::tr(
                ui,
                "Enable Scene Notes Dock in Tools > Plugins.",
            ))
            .clicked()
        {
            intents.push(Intent::OpenDialog(Dialog::scene_notes(scene)));
        }
    }
}
fn scene_filters_action(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    scene: Id,
    session: u64,
    intents: &mut Vec<Intent>,
) {
    if ui
        .button(crate::i18n::tr(ui, "Filters"))
        .on_hover_text(crate::hints::text(ui, "Filters"))
        .on_disabled_hover_text(crate::hints::text(ui, "Filters"))
        .clicked()
    {
        match super::video_filters::VideoFilterDraft::capture_scene(session, model, scene) {
            Ok(draft) => intents.push(Intent::OpenDialog(Dialog::VideoFilters(draft))),
            Err(error) => intents.push(Intent::DialogError(error)),
        }
        ui.close_menu();
    }
}
fn scene_transition_dialog(model: &StudioSnapshot, scene: Id) -> Dialog {
    let config = model
        .scene(scene)
        .and_then(|scene| scene.transition_override())
        .unwrap_or(model.transition());
    Dialog::SceneTransition {
        scene,
        enabled: model
            .scene(scene)
            .is_some_and(|scene| scene.transition_override().is_some()),
        reference: config.reference(),
        duration_ms: model
            .scene(scene)
            .map_or(300, |scene| scene.transition_duration_ms()),
    }
}
fn sources(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    scene: Id,
    selected: Option<Id>,
    session: u64,
    intents: &mut Vec<Intent>,
) {
    let toolbars = super::source_toolbar::listbox_bars_visible(ui.ctx());
    ui.strong(crate::i18n::tr(ui, "Sources"))
        .interact(egui::Sense::click())
        .on_hover_text(crate::i18n::tr(ui, "Right-click the dock heading for Add and other source actions."))
        .context_menu(|ui| {
            let entry = selected.and_then(|id| model.item_container(id))
                .filter(|(owner, _)| model.root_scene_for_container(*owner) == Some(scene));
            source_list_actions(ui, model, entry.map_or(scene, |(owner, _)| owner),
                entry.map(|(_, item)| item.id()), session, intents);
        });
    ui.separator();
    let Some(scene_model) = model.scene(scene) else {
        return;
    };
    let list_height=(ui.available_height() - if toolbars { 36.0 } else { 0.0 }).max(24.0);
    egui::ScrollArea::vertical()
        .id_salt("source-list")
        .max_height(list_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let list_top=ui.cursor().top();
            super::group_source::tree(ui, model, scene, scene, selected, session, intents, 0);
            if scene_model.items().is_empty() {
                ui.label(crate::i18n::tr(
                    ui,
                    "No sources. Add a Color Source, Image or Display Capture to compose a scene.",
                ));
            }
            // The empty area has its own source menu, including Paste even
            // when there is no selected row in the destination scene.
            let empty_height=(list_height-(ui.cursor().top()-list_top)).max(24.0);
            let empty=ui.allocate_response(egui::vec2(ui.available_width(),empty_height),egui::Sense::click());
            empty.context_menu(|ui|source_list_actions(ui,model,scene,None,session,intents));
        });
    if !toolbars { return; }
    ui.separator();
    let selected_entry = selected
        .and_then(|id| model.item_container(id))
        .filter(|(owner, _)| model.root_scene_for_container(*owner) == Some(scene));
    let owner = selected_entry.map_or(scene, |(owner, _)| owner);
    let item = selected_entry.map(|(_, item)| item);
    ui.horizontal(|ui| {
        super::dock_toolbar::size(ui, 5);
        super::dock_toolbar::menu(
            ui,
            "Sources",
            "Add",
            super::dock_toolbar::Icon::Add,
            true,
            |ui| {
                add_source_actions(ui, model, scene, intents);
            },
        );
        if super::dock_toolbar::button(
            ui,
            "Sources",
            "Remove",
            super::dock_toolbar::Icon::Remove,
            item.is_some_and(|item| !item.locked() && !model.container_is_locked(owner)),
        )
        .clicked()
        {
            if let Some(item) = item {
                super::group_source::request_remove_selection(ui,model,scene,session,item.id());
            }
        }
        let property_source = item.and_then(|item| model.source(item.source()));
        let has_properties = property_source
            .is_some_and(|source| !matches!(source.kind(), SourceKind::Group { .. }));
        if super::dock_toolbar::button(
            ui,
            "Sources",
            "Properties",
            super::dock_toolbar::Icon::Properties,
            has_properties,
        )
        .clicked()
        {
            if let Some(intent) = property_source.and_then(source_properties_intent) {
                intents.push(intent);
            }
        }
        let visual = property_source.is_some_and(|source| {
            model
                .source_video_dimensions(source.kind())
                .is_ok_and(|size| size.is_some())
        });
        let audio = property_source.is_some_and(|source| source.kind().has_audio());
        let filter_label = if audio && !visual {
            "Audio Filters"
        } else {
            "Filters"
        };
        if super::dock_toolbar::button(
            ui,
            "Sources",
            filter_label,
            super::dock_toolbar::Icon::Filters,
            visual || audio,
        )
        .clicked()
        {
            if let Some(source) = property_source {
                if visual {
                    match super::video_filters::VideoFilterDraft::capture(
                        session,
                        model,
                        source.id(),
                    ) {
                        Ok(draft) => intents.push(Intent::OpenDialog(Dialog::VideoFilters(draft))),
                        Err(error) => intents.push(Intent::DialogError(error)),
                    }
                } else if audio {
                    intents.push(Intent::OpenDialog(Dialog::audio_filters(source)));
                }
            }
        }
        super::dock_toolbar::menu(
            ui,
            "Sources",
            "Source Actions",
            super::dock_toolbar::Icon::More,
            item.is_some(),
            |ui| {
                source_list_actions(ui, model, owner, selected, session, intents);
            },
        );
    });

}
fn add_source_actions(ui: &mut egui::Ui, model: &StudioSnapshot, scene: Id, intents: &mut Vec<Intent>) {
    if ui
        .button(crate::i18n::tr(ui, "Color Source"))
        .on_hover_text(crate::hints::text(ui, "Color Source"))
        .on_disabled_hover_text(crate::hints::text(ui, "Color Source"))
        .clicked()
    {
        intents.push(Intent::OpenDialog(Dialog::ColorSource(
            super::color_source::ColorDraft::add(
                scene,
                unique_name(
                    "Color Source",
                    model.sources().iter().map(|source| source.name()),
                ),
                model.video().width(),
                model.video().height(),
            ),
        )));
        ui.close_menu();
    }
    if ui
        .button(crate::i18n::tr(ui, "Group"))
        .on_hover_text(crate::i18n::tr(
            ui,
            "Add an empty group, then move source items into it.",
        ))
        .clicked()
    {
        match super::group_source::GroupDraft::for_ui(
            ui,
            model,
            super::group_source::GroupEdit::Empty { scene },
        ) {
            Ok(draft) => intents.push(Intent::OpenDialog(Dialog::Group(draft))),
            Err(error) => intents.push(Intent::DialogError(error)),
        }
        ui.close_menu();
    }
    if ui
        .button(crate::i18n::tr(ui, "Scene"))
        .on_hover_text(crate::i18n::tr(
            ui,
            "Use an existing scene as a shared source.",
        ))
        .clicked()
    {
        intents.push(Intent::OpenDialog(Dialog::SceneSource(
            super::scene_source::SceneDraft::add(scene),
        )));
        ui.close_menu();
    }
    if ui
        .button(crate::i18n::tr(ui, "Text"))
        .on_hover_text(crate::hints::text(ui, "Text"))
        .on_disabled_hover_text(crate::hints::text(ui, "Text"))
        .clicked()
    {
        intents.push(Intent::OpenDialog(Dialog::Text(
            super::text_source::TextDraft::add(
                scene,
                unique_name("Text", model.sources().iter().map(|source| source.name())),
            ),
        )));
        ui.close_menu();
    }
    if model.preferences().plugin_enabled(skylive_core::NDI_PLUGIN)&&super::plugin_sources::SourceCapabilities::from_ui(ui).ndi {
        if ui.button(crate::i18n::tr(ui,"NDI Source")).on_hover_text(crate::i18n::tr(ui,"Receive video and audio from an NDI sender using an installed NDI6 Runtime.")).clicked(){intents.push(Intent::OpenDialog(Dialog::NdiSource(super::ndi_source::NdiDraft::add(scene,unique_name("NDI Source",model.sources().iter().map(|s|s.name()))))));ui.close_menu();}
    }
    if super::plugin_sources::SourceCapabilities::from_ui(ui).platform==crate::platform::CapturePlatform::Windows && ui.button(crate::i18n::tr(ui,"Browser")).on_hover_text(crate::i18n::tr(ui,"Render a URL or local HTML with CEF offscreen browser pixels.")).clicked(){intents.push(Intent::OpenDialog(Dialog::BrowserSource(super::browser_source::Draft::add(scene,unique_name("Browser",model.sources().iter().map(|s|s.name()))))));ui.close_menu();}
    if ui.button(crate::i18n::tr(ui,"Image Slide Show"))
        .on_hover_text(crate::i18n::tr(ui,"Display an ordered image playlist with timed cuts or fades"))
        .clicked() {
        intents.push(Intent::OpenDialog(Dialog::ImageSlideShow(super::image_slideshow::SlideShowDraft::add(scene,unique_name("Image Slide Show",model.sources().iter().map(|s|s.name()))))));
        ui.close_menu();
    }
    if ui
        .button(crate::i18n::tr(ui, "Image"))
        .on_hover_text(crate::hints::text(ui, "Image"))
        .on_disabled_hover_text(crate::hints::text(ui, "Image"))
        .clicked()
    {
        intents.push(Intent::OpenDialog(Dialog::AddImage {
            settings:Default::default(),
            scene,
            name: unique_name(
                "Image",
                model.sources().iter().map(|source| source.name()),
            ),
            path: String::new(),
        }));
        ui.close_menu();
    }
    if ui
        .button(crate::i18n::tr(ui, "Media Source"))
        .on_hover_text(crate::i18n::tr(
            ui,
            "Play a local MP4 or MOV file as a video or audio source.",
        ))
        .on_disabled_hover_text(crate::i18n::tr(
            ui,
            "Play a local MP4 or MOV file as a video or audio source.",
        ))
        .clicked()
    {
        intents.push(Intent::OpenDialog(Dialog::MediaSource(
            super::media_source::MediaDraft::add(
                scene,
                unique_name(
                    "Media Source",
                    model.sources().iter().map(|source| source.name()),
                ),
            ),
        )));
        ui.close_menu();
    }
    if ui
        .button(crate::i18n::tr(ui, "Display Capture"))
        .on_hover_text(crate::hints::text(ui, "Display Capture"))
        .on_disabled_hover_text(crate::hints::text(ui, "Display Capture"))
        .clicked()
    {
        intents.push(Intent::OpenDisplayCapture {
            edit: DisplayEdit::Add { scene },
            name: unique_name(
                "Display Capture",
                model.sources().iter().map(|source| source.name()),
            ),
            binding: None,
        });
        ui.close_menu();
    }
    if ui
        .button(crate::i18n::tr(ui, "Window Capture"))
        .on_hover_text(crate::hints::text(ui, "Window Capture"))
        .clicked()
    {
        intents.push(Intent::OpenWindowCapture {
            edit: super::WindowEdit::Add { scene },
            name: unique_name(
                "Window Capture",
                model.sources().iter().map(|source| source.name()),
            ),
            binding: None,
        });
        ui.close_menu();
    }
    if ui
        .button(crate::i18n::tr(ui, "Video Capture Device"))
        .on_hover_text(crate::i18n::tr(
            ui,
            "Choose a camera and query its actual supported native modes",
        ))
        .clicked()
    {
        intents.push(Intent::OpenCameraCapture {
            edit: super::CameraEdit::Add { scene },
            name: unique_name(
                "Video Capture Device",
                model.sources().iter().map(|s| s.name()),
            ),
            binding: None,
        });
        ui.close_menu();
    }
    if super::plugin_sources::SourceCapabilities::from_ui(ui).platform==crate::platform::CapturePlatform::Windows {
        if ui.button(crate::i18n::tr(ui,"Application Audio Capture")).on_hover_text(crate::i18n::tr(ui,"Capture the selected application's process tree, including its child processes.")).clicked(){
            intents.push(Intent::OpenDialog(Dialog::application_audio(AudioEdit::Add {scene},unique_name("Application Audio Capture",model.sources().iter().map(|source|source.name())))));ui.close_menu();
        }
    }
    for (flow, label) in [
        (AudioFlow::Input, "Audio Input Capture"),
        (AudioFlow::Output, "Audio Output Capture"),
    ] {
        if ui
            .button(crate::i18n::tr(ui, label))
            .on_hover_text(crate::hints::text(ui, "Item"))
            .on_disabled_hover_text(crate::hints::text(ui, "Item"))
            .clicked()
        {
            intents.push(Intent::OpenDialog(Dialog::audio(
                AudioEdit::Add { scene },
                unique_name(label, model.sources().iter().map(|source| source.name())),
                flow,
            )));
            ui.close_menu();
        }
    }
    if model
        .sources()
        .iter()
        .any(|source| !matches!(source.kind(), SourceKind::Group { .. }))
    {
        ui.menu_button(crate::i18n::tr(ui, "Add Existing"), |ui| {
            for source in model
                .sources()
                .iter()
                .filter(|source| !matches!(source.kind(), SourceKind::Group { .. }))
            {
                if ui
                    .button(source.name())
                    .on_hover_text(crate::hints::text(ui, "Source"))
                    .on_disabled_hover_text(crate::hints::text(ui, "Source"))
                    .clicked()
                {
                    intents.push(Intent::Command(Command::AddExistingSource {
                        scene,
                        source: source.id(),
                    }));
                    ui.close_menu();
                }
            }
        })
        .response
        .on_hover_text(crate::hints::text(ui, "Add Existing"))
        .on_disabled_hover_text(crate::hints::text(ui, "Add Existing"));
    }
    if cfg!(all(target_os="windows",target_arch="x86_64"))&&ui.button(crate::i18n::tr(ui,"Game Capture"))
        .on_hover_text(crate::i18n::tr(ui,"Add an actual D3D11 game backbuffer source for a selected process.")).clicked(){
        intents.push(Intent::OpenDialog(Dialog::GameCapture(super::game_capture::Draft::add(scene,unique_name("Game Capture",model.sources().iter().map(|s|s.name()))))));ui.close_menu();
    }
}
pub(crate) fn source_list_actions(ui:&mut egui::Ui,model:&StudioSnapshot,scene:Id,selected:Option<Id>,session:u64,intents:&mut Vec<Intent>){
    source_actions(ui,model,scene,selected,session,intents);
    if let Some(item)=selected{ui.separator();super::source_color_tags::menu(ui,model,scene,item,session,intents);}
}
pub(super) fn source_actions(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    scene: Id,
    selected: Option<Id>,
    session: u64,
    intents: &mut Vec<Intent>,
) {
    ui.menu_button(crate::i18n::tr(ui, "Add"), |ui| {
        let root = model.root_scene_for_container(scene).unwrap_or(scene);
        add_source_actions(ui, model, root, intents);
    }).response.on_hover_text(crate::hints::text(ui, "Add Source"));
    ui.separator();
    let Some(scene_model) = model.scene_container(scene) else {
        return;
    };
    let item = selected.and_then(|id| scene_model.items().iter().find(|item| item.id() == id));

    if let Some(item) = item {
        let editable = !item.locked() && !model.container_is_locked(scene);
        super::item_compositing::menus(ui,model,scene,item.id(),session,intents);
        if ui.add_enabled(editable, egui::Button::new(crate::i18n::tr(ui, "Remove")))
            .on_hover_text(crate::hints::text(ui, "Remove")).clicked() {
            let root=model.root_scene_for_container(scene).unwrap_or(scene);
            super::group_source::request_remove_selection(ui,model,root,session,item.id());
            ui.close_menu();
        }
        super::group_source::actions(ui, model, scene, item.id(), session, intents);
        if let Some(source) = model.source(item.source()) {
            if source.kind().has_audio() {
                let flags = source.mixer_state();
                let global = model.global_audio().iter().any(|slot| slot.source() == source.id());
                let mut hidden = flags.hidden();
                if ui.add_enabled_ui(!global && !flags.pinned(), |ui| {
                    ui.checkbox(&mut hidden, crate::i18n::tr_key(ui, "HideMixer"))
                }).inner
                    .on_hover_text(crate::i18n::tr(ui, "Hide only the mixer control. The source keeps playing and its audio is unchanged."))
                    .on_disabled_hover_text(crate::i18n::tr(ui, "Unpin this channel before hiding it. Global audio channels cannot be hidden."))
                    .changed() {
                    intents.push(Intent::ContextMixerHidden { session, revision: model.revision(), scene, item: item.id(), source: source.id(), hidden });
                    ui.close_menu();
                }
            }
            if model.source_video_dimensions(source.kind()).is_ok_and(|v|v.is_some()){ui.menu_button(crate::i18n::tr(ui,"Source Projector"),|ui|crate::projectors::menu(ui,session,model.revision(),skylive_host::projector::Target::Source{source:source.id()},intents));}
            super::screenshot::button(ui,session,model.revision(),skylive_host::screenshot::Target::Source {source:source.id()},model.source_video_dimensions(source.kind()).is_ok_and(|v|v.is_some()),intents);
            let properties = source_properties_intent(source);
            if ui.add_enabled(properties.is_some(), egui::Button::new(crate::i18n::tr(ui, "Properties")))
                .on_hover_text(crate::hints::text(ui, "Properties"))
                .on_disabled_hover_text(crate::hints::text(ui, "Properties"))
                .clicked() {
                if let Some(intent) = properties {
                    intents.push(intent);
                    ui.close_menu();
                }
            }
            if let SourceKind::WindowCapture {binding}=source.kind() {
                if super::plugin_sources::SourceCapabilities::from_ui(ui).platform==crate::platform::CapturePlatform::Windows {
                    let name=unique_name("Application Audio Capture",model.sources().iter().map(|source|source.name()));
                    let draft=Dialog::application_audio_for_window(AudioEdit::Add {scene},name,binding);
                    if ui.add_enabled(draft.is_some(),egui::Button::new(crate::i18n::tr_key_or(ui,
                        "SkyLive.WindowCapture.AddApplicationAudio","Add application audio capture…")))
                        .on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.WindowCapture.AddApplicationAudioHint",
                            "Create a separate audio source preselected for this window. Later changes to this window source do not update the audio source."))
                        .clicked() {
                        if let Some(draft)=draft {intents.push(Intent::OpenDialog(draft));}
                        ui.close_menu();
                    }
                }
            }
            if matches!(source.kind(),SourceKind::Browser {..}) {
                if ui.add_enabled(skylive_host::browser::BrowserOwner::available(),egui::Button::new(crate::i18n::tr(ui,"Interact")))
                    .on_hover_text(crate::i18n::tr(ui,"Open the real browser page in a separate interaction window."))
                    .on_disabled_hover_text(crate::i18n::tr(ui,"Browser Interaction requires Windows x64 and the matching external CEF runtime."))
                    .clicked(){intents.push(Intent::InteractBrowser {session,revision:model.revision(),source:source.id()});ui.close_menu();}
            }
            if matches!(source.kind(),SourceKind::ImageSlideShow{..}){super::slideshow_controls::menu(ui,session,model,source.id(),intents);}
            super::source_playback_hotkeys::open_button(ui,session,model,source.id(),intents);
            super::source_audio_hotkeys::open_button(ui,session,model,source.id(),intents);
            super::source_output_resize::button(ui,session,model,scene,item.id(),intents);
            crate::source_clipboard::context_menu(ui, session, model.revision(), item.id(), intents);
            if source.kind().has_audio() || model.source_video_dimensions(source.kind()).is_ok_and(|dimensions|dimensions.is_some()) {
                for (action,key,hint) in [
                    (crate::edit_clipboard::Action::CopyFilters,"Copy.Filters","actionCopyFilters"),
                    (crate::edit_clipboard::Action::PasteFilters,"Paste.Filters","actionPasteFilters"),
                ] {
                    let request=crate::edit_clipboard::mixer_request(ui.ctx(),model,session,source.id(),action);
                    if ui.add_enabled(request.is_some(),egui::Button::new(crate::i18n::tr_key(ui,key)))
                        .on_hover_text(crate::hints::text(ui,hint)).clicked() {
                        if let Some(request)=request {intents.push(Intent::MixerFilterClipboard(request));}
                        ui.close_menu();
                    }
                }
            }
            ui.separator();
            if ui
                .add(egui::Button::new(crate::i18n::tr(ui, "Rename"))
                    .shortcut_text(if cfg!(target_os = "macos") { "Enter" } else { "F2" }))
                .on_hover_text(crate::hints::text(ui, "Rename"))
                .on_disabled_hover_text(crate::hints::text(ui, "Rename"))
                .clicked()
            {
                intents.push(Intent::OpenDialog(Dialog::RenameSource {
                    source: source.id(),
                    name: source.name().into(),
                }));
            }
            if source.kind().has_audio()
                && ui
                    .add_enabled(
                        model
                            .preferences()
                            .plugin_enabled(skylive_core::OBS_VIDEO_FILTERS_PLUGIN)
                            || model
                                .preferences()
                                .plugin_enabled(skylive_core::AUDIO_PAN_PLUGIN)
                            || model
                                .preferences()
                                .plugin_enabled(skylive_core::CRAFT_AUDIO_PLUGIN)
                            || !source.audio_filters().is_empty(),
                        egui::Button::new(crate::i18n::tr(ui, "Audio Filters")),
                    )
                    .on_hover_text(crate::hints::text(ui, "Audio Filters"))
                    .on_disabled_hover_text(crate::i18n::tr(
                        ui,
                        "Enable Audio Pan Filter in Tools > Plugins.",
                    ))
                    .clicked()
            {
                intents.push(Intent::OpenDialog(Dialog::audio_filters(source)));
            }
            if model
                .source_video_dimensions(source.kind())
                .is_ok_and(|dimensions| dimensions.is_some())
                && ui
                    .add_enabled(
                        model
                            .preferences()
                            .plugin_enabled(skylive_core::OBS_VIDEO_FILTERS_PLUGIN)
                            || model
                                .preferences()
                                .plugin_enabled(skylive_core::CRAFT_VIDEO_PLUGIN)
                            || model
                                .preferences()
                                .plugin_enabled(skylive_core::PERSON_BACKGROUND_PLUGIN)
                            || !source.video_filters().is_empty(),
                        egui::Button::new(crate::i18n::tr(ui, "Filters")),
                    )
                    .on_hover_text(crate::i18n::tr(
                        ui,
                        "Edit this source's color key, brightness, contrast and spill suppression.",
                    ))
                    .on_disabled_hover_text(crate::i18n::tr(
                        ui,
                        "Enable bundled video effects in Tools > Plugins.",
                    ))
                    .clicked()
            {
                match super::video_filters::VideoFilterDraft::capture(session, model, source.id()) {
                    Ok(draft) => intents.push(Intent::OpenDialog(Dialog::VideoFilters(draft))),
                    Err(error) => intents.push(Intent::DialogError(error)),
                }
            }
        }
        let has_video = model.source(item.source()).is_some_and(|source| {
            model.source_video_dimensions(source.kind()).is_ok_and(|value| value.is_some())
        });
        ui.add_enabled_ui(has_video, |ui| {
            ui.menu_button(crate::i18n::tr_key(ui, "Basic.MainMenu.Edit.Transform"), |ui| {
                if ui.add_enabled(editable, egui::Button::new(crate::i18n::tr_key(ui, "Basic.MainMenu.Edit.Transform.EditTransform")))
                    .on_hover_text(crate::hints::text(ui, "actionEditTransform")).clicked() {
                    intents.push(Intent::OpenDialog(Dialog::Transform {
                        scene, item: item.id(), draft: item.transform().clone(), capture_context: None,
                    }));
                    ui.close_menu();
                }
                ui.separator();
                for (key,hint,action) in [
                    ("CopyTransform","actionCopyTransform",crate::edit_clipboard::Action::CopyTransform),
                    ("PasteTransform","actionPasteTransform",crate::edit_clipboard::Action::PasteTransform),
                ] {
                    let request=crate::edit_clipboard::context_transform_request(ui.ctx(),model,session,scene,item.id(),action);
                    let label=format!("Basic.MainMenu.Edit.Transform.{key}");
                    if ui.add_enabled(request.is_some(),egui::Button::new(crate::i18n::tr_key(ui,&label)))
                        .on_hover_text(crate::hints::text(ui,hint)).clicked() {
                        if let Some(request)=request {intents.push(Intent::ContextTransformClipboard(request));}
                        ui.close_menu();
                    }
                }
                ui.separator();
                for (key, hint, action) in [
                    ("ResetTransform", "actionResetTransform", skylive_core::TransformAction::Reset),
                    ("Rotate90CW", "actionRotate90CW", skylive_core::TransformAction::Rotate90Clockwise),
                    ("Rotate90CCW", "actionRotate90CCW", skylive_core::TransformAction::Rotate90CounterClockwise),
                    ("Rotate180", "actionRotate180", skylive_core::TransformAction::Rotate180),
                    ("FlipHorizontal", "actionFlipHorizontal", skylive_core::TransformAction::FlipHorizontal),
                    ("FlipVertical", "actionFlipVertical", skylive_core::TransformAction::FlipVertical),
                    ("FitToScreen", "actionFitToScreen", skylive_core::TransformAction::FitToCanvas),
                    ("StretchToScreen", "actionStretchToScreen", skylive_core::TransformAction::StretchToCanvas),
                    ("CenterToScreen", "actionCenterToScreen", skylive_core::TransformAction::Center),
                    ("VerticalCenter", "actionVerticalCenter", skylive_core::TransformAction::CenterVertical),
                    ("HorizontalCenter", "actionHorizontalCenter", skylive_core::TransformAction::CenterHorizontal),
                ] {
                    let label = format!("Basic.MainMenu.Edit.Transform.{key}");
                    if ui.add_enabled(editable, egui::Button::new(crate::i18n::tr_key(ui, &label)))
                        .on_hover_text(crate::hints::text(ui, hint)).clicked() {
                        intents.push(Intent::ContextTransform { session, revision: model.revision(), scene, item: item.id(), action });
                        ui.close_menu();
                    }
                }
            }).response.on_hover_text(crate::hints::text(ui, "Transform"));
        }).response.on_disabled_hover_text(crate::i18n::tr(ui,
            "Audio-only sources have no video transform; locked items cannot be transformed."));
        let current = scene_model.items().iter().position(|candidate| candidate.id() == item.id());
        let top = scene_model.items().len().checked_sub(1);
        ui.menu_button(crate::i18n::tr_key(ui, "Basic.MainMenu.Edit.Order"), |ui| {
            for (key,hint,target) in [
                ("MoveUp", "actionMoveUp", current.and_then(|index| index.checked_add(1)).filter(|index| *index < scene_model.items().len())),
                ("MoveDown", "actionMoveDown", current.and_then(|index| index.checked_sub(1))),
                ("MoveToTop", "actionMoveToTop", top),
                ("MoveToBottom", "actionMoveToBottom", current.map(|_| 0)),
            ] {
                let enabled = editable && target.is_some() && target != current;
                let label = format!("Basic.MainMenu.Edit.Order.{key}");
                if ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr_key(ui, &label)))
                    .on_hover_text(crate::hints::text(ui, hint)).clicked() {
                    if let Some(index) = target {
                        intents.push(Intent::ContextOrder { session, revision: model.revision(), scene, item: item.id(), index });
                    }
                    ui.close_menu();
                }
            }
        });
    } else {
        let root=model.root_scene_for_container(scene).unwrap_or(scene);
        crate::source_clipboard::empty_context_menu(ui,session,model.revision(),root,intents);
    }
}

pub(super) fn source_properties_intent(source: &skylive_core::Source) -> Option<Intent> {
    match source.kind() {
        SourceKind::Group { .. } => None,
        SourceKind::Browser {browser}=>Some(Intent::OpenDialog(Dialog::BrowserSource(super::browser_source::Draft::rebind(source.id(),browser)))),
        SourceKind::GameCapture {binding}=>Some(Intent::OpenDialog(Dialog::GameCapture(super::game_capture::Draft::rebind(source.id(),binding)))),
        SourceKind::NdiSource {ndi}=>Some(Intent::OpenDialog(Dialog::NdiSource(super::ndi_source::NdiDraft::rebind(source.id(),ndi)))),
        SourceKind::MediaSource { media } => Some(Intent::OpenDialog(Dialog::MediaSource(
            super::media_source::MediaDraft::rebind(source.id(), media),
        ))),
        SourceKind::Scene { scene: target } => Some(Intent::OpenDialog(Dialog::SceneSource(
            super::scene_source::SceneDraft::rebind(source.id(), *target),
        ))),
        SourceKind::Color {
            rgba,
            width,
            height,
        } => Some(Intent::OpenDialog(Dialog::ColorSource(
            super::color_source::ColorDraft::rebind(source.id(), *rgba, *width, *height),
        ))),
        SourceKind::ImageSlideShow {settings}=>Some(Intent::OpenDialog(Dialog::ImageSlideShow(super::image_slideshow::SlideShowDraft::rebind(source.id(),settings)))),
        SourceKind::Image { asset } => Some(Intent::OpenDialog(Dialog::ImageFile {
            settings:source.image_settings(),
            source: source.id(),
            path: asset.path().to_owned(),
        })),
        SourceKind::Text { settings } => Some(Intent::OpenDialog(Dialog::Text(
            super::text_source::TextDraft::rebind(source.id(), settings),
        ))),
        SourceKind::VideoCapture { binding } => Some(Intent::OpenCameraCapture {
            edit: super::CameraEdit::Rebind {
                source: source.id(),
            },
            name: source.name().to_owned(),
            binding: Some(binding.clone()),
        }),
        SourceKind::WindowCapture { binding } => Some(Intent::OpenWindowCapture {
            edit: super::WindowEdit::Rebind {
                source: source.id(),
            },
            name: source.name().to_owned(),
            binding: Some(binding.clone()),
        }),
        SourceKind::DisplayCapture { binding } => Some(Intent::OpenDisplayCapture {
            edit: DisplayEdit::Rebind {
                source: source.id(),
            },
            name: source.name().to_owned(),
            binding: Some(binding.clone()),
        }),
        SourceKind::AudioCapture { binding } => Some(Intent::OpenDialog(Dialog::audio_properties(
            source.id(),
            source.name().to_owned(),
            binding,
        ))),
    }
}

#[derive(Clone, Copy)]
struct DurationDraft {
    session: u64,
    canonical: u32,
    reference: TransitionRef,
    value: u32,
    dirty: bool,
}
fn transitions(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    state: TransitionViewState,
    intents: &mut Vec<Intent>,
) {
    ui.strong(crate::i18n::tr(ui, "Scene Transitions"));
    ui.separator();
    if state.studio {
        transition_actions(ui, state, intents);
        manual_tbar(ui, state, intents);
        ui.separator();
    }
    let config = model.transition();
    let mut reference = config.reference();
    transition_reference_editor(ui, "global-transition-reference", model, &mut reference);
    ui.menu_button(crate::i18n::tr(ui,"Preset Settings"), |ui| {
        if ui
            .add_enabled(
                model.transition_presets().len() < MAX_TRANSITION_PRESETS,
                egui::Button::new(crate::i18n::tr(ui,"Add Transition Preset")),
            ).on_hover_text(crate::hints::text(ui,"Add Transition Preset")).on_disabled_hover_text(crate::hints::text(ui,"Add Transition Preset"))
            .clicked()
        {
            intents.push(Intent::OpenDialog(Dialog::TransitionPreset {
                id: None,
                name: unique_name(
                    "Swipe",
                    model
                        .transition_presets()
                        .iter()
                        .map(|preset| preset.name()),
                ),
                kind: TransitionKind::Swipe {
                    direction: TransitionDirection::Left,
                    swipe_in: false,
                },
            }));
            ui.close_menu();
        }
        let focus = state
            .preset_focus
            .or(match reference {
                TransitionRef::Preset(id) => Some(id),
                _ => None,
            })
            .and_then(|id| model.transition_preset(id));
        if ui
            .add_enabled(focus.is_some(), egui::Button::new(crate::i18n::tr(ui,"Preset Properties"))).on_hover_text(crate::hints::text(ui,"Preset Properties")).on_disabled_hover_text(crate::hints::text(ui,"Preset Properties"))
            .clicked()
        {
            if let Some(preset) = focus {
                intents.push(Intent::OpenDialog(Dialog::TransitionPreset {
                    id: Some(preset.id()),
                    name: preset.name().into(),
                    kind: preset.kind(),
                }));
                ui.close_menu();
            }
        }
        if ui.add_enabled(focus.is_some(), egui::Button::new(crate::i18n::tr(ui,"Remove Preset"))).on_hover_text(crate::hints::text(ui,"Remove Preset")).on_disabled_hover_text(crate::hints::text(ui,"Remove Preset")).clicked() {
            if let Some(preset) = focus {
                match Dialog::remove_transition_preset(model, preset.id()) {
                    Ok(dialog) => intents.push(Intent::OpenDialog(dialog)),
                    Err(error) => intents.push(Intent::DialogError(error)),
                }
                ui.close_menu();
            }
        }
        ui.menu_button(crate::i18n::tr(ui,"Preset Library"), |ui| {
            for preset in model.transition_presets() {
                ui.push_id(preset.id().get(), |ui| {
                    ui.menu_button(preset.name(), |ui| {
                        if ui.button(crate::i18n::tr(ui,"Properties")).on_hover_text(crate::hints::text(ui,"Properties")).on_disabled_hover_text(crate::hints::text(ui,"Properties")).clicked() {
                            intents.push(Intent::OpenDialog(Dialog::TransitionPreset {
                                id: Some(preset.id()),
                                name: preset.name().into(),
                                kind: preset.kind(),
                            }));
                            ui.close_menu();
                        }
                        if ui
                            .button(crate::i18n::tr(ui,"Remove Preset"))
                            .on_hover_text(crate::i18n::tr(ui,"Review affected quick rows, scene overrides and global fallback before deletion"))
                            .clicked()
                        {
                            match Dialog::remove_transition_preset(model,preset.id()) {Ok(dialog)=>intents.push(Intent::OpenDialog(dialog)),Err(error)=>intents.push(Intent::DialogError(error))}
                            ui.close_menu();
                        }
                    }).response.on_hover_text(crate::hints::text(ui,"Transition preset")).on_disabled_hover_text(crate::hints::text(ui,"Transition preset"))
                });
            }
            if model.transition_presets().is_empty() {
                ui.label(crate::i18n::tr(ui,"No shared presets"));
            }
        }).response.on_hover_text(crate::hints::text(ui,"Preset Library")).on_disabled_hover_text(crate::hints::text(ui,"Preset Library"));
    }).response.on_hover_text(crate::hints::text(ui,"Preset Settings")).on_disabled_hover_text(crate::hints::text(ui,"Preset Settings"));
    let draft_id = ui.id().with("transition-duration-draft");
    let mut draft = ui
        .data_mut(|data| data.get_temp::<DurationDraft>(draft_id))
        .unwrap_or(DurationDraft {
            session: state.session,
            canonical: config.duration_ms(),
            reference: config.reference(),
            value: config.duration_ms(),
            dirty: false,
        });
    if draft.session != state.session
        || draft.canonical != config.duration_ms()
        || draft.reference != config.reference()
        || reference != config.reference()
    {
        draft = DurationDraft {
            session: state.session,
            canonical: config.duration_ms(),
            reference: config.reference(),
            value: config.duration_ms(),
            dirty: false,
        };
    }
    if reference != config.reference() {
        match TransitionSelection::new(reference, config.duration_ms()) {
            Ok(selection) => intents.push(Intent::Command(Command::SelectTransition { selection })),
            Err(error) => intents.push(Intent::DialogError(error.to_string())),
        }
    }
    ui.label(crate::i18n::tr(ui, "Duration (ms)"));
    let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let response = ui
        .add_enabled(
            reference != TransitionRef::Cut,
            egui::DragValue::new(&mut draft.value)
                .range(MIN_TRANSITION_DURATION_MS..=MAX_TRANSITION_DURATION_MS)
                .speed(1)
                .suffix(" ms"),
        )
        .on_hover_text(crate::hints::text(ui, "Duration (ms)"))
        .on_disabled_hover_text(crate::hints::text(ui, "Duration (ms)"));
    draft.dirty |= response.changed();
    let cancel = escape && (response.has_focus() || response.lost_focus());
    if cancel {
        draft.value = config.duration_ms();
        draft.dirty = false;
    }
    let commit = !cancel
        && (response.drag_stopped()
            || response.lost_focus()
            || (response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))));
    let apply_duration = ui
        .add_enabled(
            reference != TransitionRef::Cut && draft.dirty,
            egui::Button::new(crate::i18n::tr(ui, "Apply Duration")),
        )
        .on_hover_text(crate::hints::text(ui, "Apply Duration"))
        .on_disabled_hover_text(crate::hints::text(ui, "Apply Duration"))
        .clicked();
    if (commit && draft.dirty) || apply_duration {
        match TransitionSelection::new(reference, draft.value) {
            Ok(selection) => intents.push(Intent::Command(Command::SelectTransition { selection })),
            Err(error) => intents.push(Intent::DialogError(error.to_string())),
        }
        draft.dirty = false;
    }
    ui.data_mut(|data| data.insert_temp(draft_id, draft));
    if !state.studio {
        transition_actions(ui, state, intents);
    }
    ui.add_enabled(
        false,
        egui::Button::new(crate::i18n::tr(ui, "Stinger / other providers")),
    )
    .on_hover_text(crate::hints::text(ui, "Stinger / other providers"))
    .on_disabled_hover_text(crate::i18n::tr(
        ui,
        "Additional transition providers are pending implementation.",
    ));
    ui.separator();
    ui.separator();
    ui.strong(crate::i18n::tr(ui, "Quick Transitions"));
    // Scope the drag to the session and revision so an intervening edit or
    // document switch cannot apply an obsolete index to another transition.
    // The existing Move Up/Down menu remains the non-drag alternative.
    let quick_drag = ui.make_persistent_id("quick-transition-reorder");
    let mut quick_rows = Vec::with_capacity(model.quick_transitions().len());
    for (index, row) in model.quick_transitions().iter().enumerate() {
        let rendered = ui.push_id(("quick-transition", row.id().get()), |ui| {
            ui.horizontal_wrapped(|ui| {
                let handle = ui.add(egui::Label::new("⠿").sense(egui::Sense::drag()))
                    .on_hover_text(crate::i18n::tr_key_or(ui, "SkyLive.QuickTransition.DragReorder",
                        "Drag to reorder this quick transition. Use Options to move it without dragging."));
                if handle.drag_started() {
                    ui.ctx().data_mut(|data| data.insert_temp(quick_drag, (state.session, model.revision(), row.id())));
                }
                let description = transition_reference_label(ui, model, row.config().reference());
                let label = if row.fade_to_black() {
                    crate::i18n::tf(
                        ui,
                        "{description} To Black ({} ms)",
                        &[
                            ("{description}", description.clone()),
                            ("{}", row.config().duration_ms().to_string()),
                        ],
                    )
                } else if row.config().reference() == TransitionRef::Cut {
                    description
                } else {
                    crate::i18n::tf(
                        ui,
                        "{description} ({} ms)",
                        &[
                            ("{description}", description),
                            ("{}", row.config().duration_ms().to_string()),
                        ],
                    )
                };
                let response = ui
                    .add_enabled(state.studio && !state.active, egui::Button::new(label))
                    .on_hover_text(crate::hints::text(ui, "Item"))
                    .on_disabled_hover_text(crate::hints::text(ui, "Item"));
                if response.clicked() {
                    intents.push(Intent::Quick(row.id()));
                }
                let mut actions = |ui: &mut egui::Ui| {
                    if ui
                        .button(crate::i18n::tr(ui, "Edit Quick Transition"))
                        .on_hover_text(crate::hints::text(ui, "Edit Quick Transition"))
                        .on_disabled_hover_text(crate::hints::text(ui, "Edit Quick Transition"))
                        .clicked()
                    {
                        intents.push(Intent::OpenDialog(Dialog::QuickRow {
                            id: Some(row.id()),
                            reference: row.config().reference(),
                            duration_ms: row.config().duration_ms(),
                            fade_to_black: row.fade_to_black(),
                        }));
                        ui.close_menu();
                    }
                    if ui
                        .button(crate::i18n::tr(ui, "Remove Quick Transition"))
                        .on_hover_text(crate::hints::text(ui, "Remove Quick Transition"))
                        .on_disabled_hover_text(crate::hints::text(ui, "Remove Quick Transition"))
                        .clicked()
                    {
                        intents.push(Intent::Command(Command::RemoveQuickTransition {
                            id: row.id(),
                        }));
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(index > 0, egui::Button::new(crate::i18n::tr(ui, "Move Up")))
                        .on_hover_text(crate::hints::text(ui, "Move Up"))
                        .on_disabled_hover_text(crate::hints::text(ui, "Move Up"))
                        .clicked()
                    {
                        intents.push(Intent::Command(Command::MoveQuickTransition {
                            id: row.id(),
                            index: index - 1,
                        }));
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            index + 1 < model.quick_transitions().len(),
                            egui::Button::new(crate::i18n::tr(ui, "Move Down")),
                        )
                        .on_hover_text(crate::hints::text(ui, "Move Down"))
                        .on_disabled_hover_text(crate::hints::text(ui, "Move Down"))
                        .clicked()
                    {
                        intents.push(Intent::Command(Command::MoveQuickTransition {
                            id: row.id(),
                            index: index + 1,
                        }));
                        ui.close_menu();
                    }
                };
                response.context_menu(&mut actions);
                ui.menu_button(crate::i18n::tr(ui, "Options"), actions)
                    .response
                    .on_hover_text(crate::hints::text(ui, "Options"))
                    .on_disabled_hover_text(crate::hints::text(ui, "Options"));
            }).response
        });
        quick_rows.push((row.id(), rendered.inner.rect));
    }
    if ui.input(|input| input.pointer.primary_released()) {
        let dragged = ui.ctx().data_mut(|data| {
            let dragged = data.get_temp::<(u64, u64, skylive_core::QuickTransitionId)>(quick_drag);
            data.remove::<(u64, u64, skylive_core::QuickTransitionId)>(quick_drag);
            dragged
        });
        if let (Some((session, revision, id)), Some(position)) = (dragged, ui.input(|input| input.pointer.interact_pos())) {
            if session == state.session && revision == model.revision() {
                if let (Some(from), Some(to)) = (quick_rows.iter().position(|(row, _)| *row == id),
                    quick_rows.iter().position(|(_, rect)| rect.contains(position))) {
                    if from != to {
                        intents.push(Intent::Command(Command::MoveQuickTransition { id, index: to }));
                    }
                }
            }
        }
    }
    if ui
        .add_enabled(
            model.quick_transitions().len() < MAX_QUICK_TRANSITIONS,
            egui::Button::new(crate::i18n::tr(ui, "Add Quick Transition")),
        )
        .on_hover_text(crate::hints::text(ui, "Add Quick Transition"))
        .on_disabled_hover_text(crate::hints::text(ui, "Add Quick Transition"))
        .clicked()
    {
        intents.push(Intent::OpenDialog(Dialog::QuickRow {
            id: None,
            reference: TransitionRef::Fade,
            duration_ms: 300,
            fade_to_black: false,
        }));
    }
}
fn transition_actions(ui: &mut egui::Ui, state: TransitionViewState, intents: &mut Vec<Intent>) {
    if state.active {
        ui.label(crate::i18n::tr(ui, "Transition in progress"));
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                !state.active,
                egui::Button::new(crate::i18n::tr(ui, "Transition"))
                    .wrap_mode(egui::TextWrapMode::Extend),
            )
            .on_hover_text(crate::i18n::tr(
                ui,
                "Send Preview to Program using the selected transition",
            ))
            .clicked()
        {
            intents.push(Intent::Transition);
        }
        if ui
            .add_enabled(
                !state.active,
                egui::Button::new(crate::i18n::tr(ui, "Quick Cut"))
                    .wrap_mode(egui::TextWrapMode::Extend),
            )
            .on_hover_text(crate::i18n::tr(ui, "Send Preview to Program immediately"))
            .clicked()
        {
            intents.push(Intent::Cut);
        }
    });
}
fn manual_tbar(ui: &mut egui::Ui, state: TransitionViewState, intents: &mut Vec<Intent>) {
    let manual_enabled =
        state.studio && (!state.active || state.manual.is_some_and(|manual| !manual.finishing));
    let mut value = state.manual.map_or(0, |manual| manual.value);
    let label = ui.label(crate::i18n::tr(ui, "T-bar (0–1023)"));
    let slider = ui
        .push_id("manual-tbar", |ui| {
            ui.add_enabled(
                manual_enabled,
                egui::Slider::new(&mut value, 0..=1023)
                    .show_value(false)
                    .smart_aim(false),
            )
            .on_hover_text(crate::hints::text(ui, "T-bar"))
            .on_disabled_hover_text(crate::hints::text(ui, "T-bar"))
        })
        .inner
        .labelled_by(label.id);
    #[cfg(test)]
    ui.data_mut(|data| data.insert_temp(egui::Id::new("skylive-tbar-control"), slider.rect));
    if slider.changed() {
        intents.push(Intent::ManualPosition(value));
    }
    if slider.drag_stopped() && (state.manual.is_some() || slider.changed()) {
        intents.push(Intent::ManualRelease);
    }
    ui.label(crate::i18n::tf(
        ui,
        "T-bar value: {value}",
        &[("{value}", (value).to_string())],
    ));
    if let Some(manual) = state.manual {
        ui.label(crate::i18n::tf(
            ui,
            "Actual {:.1}% · Target {:.1}%{}",
            &[
                ("{:.1}", format!("{:.1}", manual.current * 100.0)),
                ("{:.1}", format!("{:.1}", manual.target * 100.0)),
                (
                    "{}",
                    (if manual.finishing {
                        " · finishing"
                    } else {
                        ""
                    })
                    .to_string(),
                ),
            ],
        ));
    }
    if ui
        .add_enabled(
            state.studio && state.manual.is_some_and(|manual| !manual.finishing),
            egui::Button::new(crate::i18n::tr(ui, "Release T-bar")),
        )
        .on_hover_text(crate::hints::text(ui, "Release T-bar"))
        .on_disabled_hover_text(crate::hints::text(ui, "Release T-bar"))
        .clicked()
    {
        intents.push(Intent::ManualRelease);
    }
}
#[cfg(test)]
fn controls(ui: &mut egui::Ui, model: &StudioSnapshot, studio: bool, intents: &mut Vec<Intent>) {
    controls_with_outputs(
        ui,
        model,
        studio,
        Default::default(),
        Default::default(),
        true,
        intents,
    );
}
fn streaming_control(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    streaming: &super::StreamingViewState,
    editing_enabled: bool,
    intents: &mut Vec<Intent>,
) {
    use skylive_host::streaming::Phase;
    if streaming.active&&streaming.reconnect_attempt>0&&matches!(streaming.phase,Some(Phase::ReconnectRetiring|Phase::ReconnectWaiting|Phase::Initializing|Phase::Connecting|Phase::ReadyAwaitingInput)){
        let caption=if streaming.phase==Some(Phase::ReconnectRetiring){crate::i18n::tf(ui,"Reconnecting: closing the previous connection (attempt {attempt}/{limit})",&[("{attempt}",streaming.reconnect_attempt.to_string()),("{limit}",streaming.reconnect_limit.to_string())])}else if streaming.phase==Some(Phase::ReconnectWaiting){crate::i18n::tf(ui,"Reconnecting: attempt {attempt}/{limit}, waiting {seconds} s",&[("{attempt}",streaming.reconnect_attempt.to_string()),("{limit}",streaming.reconnect_limit.to_string()),("{seconds}",streaming.reconnect_wait_ms.div_ceil(1000).to_string())])}else{crate::i18n::tf(ui,"Reconnecting: establishing attempt {attempt}/{limit}",&[("{attempt}",streaming.reconnect_attempt.to_string()),("{limit}",streaming.reconnect_limit.to_string())])};
        ui.label(caption).on_hover_text(crate::i18n::tr(ui,"The old connection is retired before a new codec and publisher epoch starts. No offline media backlog is replayed."));
        if let Some(error)=&streaming.reconnect_error{ui.small(error.to_string());}
    }
    let configured = model.profiles().selected().streaming().is_configured();
    let caption = if streaming.finishing {
        "Stopping streaming…"
    } else if streaming.active&&streaming.reconnect_attempt==0
        && matches!(
            streaming.phase,
            Some(Phase::Initializing | Phase::Connecting | Phase::ReadyAwaitingInput)
        )
    {
        "Cancel Streaming Start"
    } else if streaming.active {
        "Stop Streaming"
    } else {
        "Start Streaming"
    };
    let help = if !streaming.available {
        "Streaming is unavailable on this platform"
    } else if !streaming.active && !configured {
        "Configure the server and stream key in Settings > Stream first."
    } else if streaming.active&&streaming.reconnect_attempt>0 {
        "Cancel automatic reconnect and wait for the current connection and codecs to retire."
    } else if streaming.active {
        "Stop this stream and drain its audio, video and connection. Recording continues independently."
    } else {
        "Publish the current Program as H.264 and optional AAC to the configured RTMP server."
    };
    if ui
        .add_enabled(
            streaming.available
                && (configured || streaming.active)
                && !streaming.finishing
                && (editing_enabled || streaming.active),
            egui::Button::new(crate::i18n::tr(ui, caption))
                .wrap_mode(egui::TextWrapMode::Extend)
                .min_size(egui::vec2(ui.available_width(), 24.0)),
        )
        .on_hover_text(crate::i18n::tr(ui, help))
        .on_disabled_hover_text(crate::i18n::tr(ui, help))
        .clicked()
    {
        intents.push(if streaming.active {
            Intent::StopStreaming
        } else {
            Intent::StartStreaming
        });
    }
}
fn controls_with_outputs(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    studio: bool,
    recording: super::RecordingViewState,
    streaming: super::StreamingViewState,
    editing_enabled: bool,
    intents: &mut Vec<Intent>,
) {
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.strong(crate::i18n::tr(ui, "Controls"));
    ui.add(egui::Separator::default().spacing(4.0));
    streaming_control(ui, model, &streaming, editing_enabled, intents);
    for (name, reason) in [
        (
            "Start Recording",
            "Recording is unavailable on this platform.",
        ),
        (
            "Start Virtual Camera",
            "Send Program to the installed SkyLive virtual camera.",
        ),
        (
            "Start Replay Buffer",
            "Keep recent encoded Program output in memory and save an MP4 replay on demand.",
        ),
    ] {
        if name=="Start Replay Buffer" {super::replay_buffer::control(ui,editing_enabled,intents);continue;}
        if name=="Start Virtual Camera" {
            super::virtual_camera::control(ui,editing_enabled,intents);
            continue;
        }
        let is_recording = name == "Start Recording";
        let caption = if is_recording && recording.finishing {
            "Finishing recording…"
        } else if is_recording && recording.active {
            "Stop Recording"
        } else {
            name
        };
        let help = if is_recording && recording.available {
            if recording.active {
                "Stop recording and finish the current file."
            } else {
                "Record the Program output using the selected recording format."
            }
        } else {
            reason
        };
        if is_recording {
            ui.horizontal(|ui| {
                let pause_width=ui.available_width().min(78.0).min(ui.available_width()*0.38);
                let stop_width=(ui.available_width()-pause_width-ui.spacing().item_spacing.x).max(30.0);
                if ui.add_enabled(recording.available && !recording.finishing && (editing_enabled||recording.active),egui::Button::new(crate::i18n::tr(ui,caption)).wrap_mode(egui::TextWrapMode::Wrap).min_size(egui::vec2(stop_width,24.0)))
                    .on_hover_text(crate::i18n::tr(ui,help)).on_disabled_hover_text(crate::i18n::tr(ui,help)).clicked(){intents.push(if recording.active {Intent::StopRecording}else{Intent::StartRecording});}
                let caption=if recording.pause_pending {"Wait…"} else if recording.paused {"Resume"} else {"Pause"};
                if ui.add_enabled(recording.available && recording.recording!=0 && recording.active && !recording.starting && !recording.finishing && !recording.pause_pending,egui::Button::new(crate::i18n::tr(ui,caption)).selected(recording.paused).wrap_mode(egui::TextWrapMode::Wrap).min_size(egui::vec2(pause_width,24.0)))
                    .on_hover_text(crate::i18n::tr(ui,"Pause recording while streaming, replay, virtual camera and Preview continue."))
                    .on_disabled_hover_text(crate::i18n::tr(ui,"Wait for recording and pause acknowledgements. Stop remains available while paused."))
                    .clicked(){intents.push(Intent::SetRecordingPaused {recording:recording.recording,session:recording.session,paused:!recording.paused});}
            });
            continue;
        }
        if ui
            .add_enabled(
                is_recording
                    && recording.available
                    && !recording.finishing
                    && (editing_enabled || recording.active),
                egui::Button::new(crate::i18n::tr(ui, caption))
                    .wrap_mode(egui::TextWrapMode::Extend)
                    .min_size(egui::vec2(ui.available_width(), 24.0)),
            )
            .on_hover_text(crate::i18n::tr(ui, help))
            .on_disabled_hover_text(crate::i18n::tr(ui, help))
            .clicked()
        {
            intents.push(if recording.active {
                Intent::StopRecording
            } else {
                Intent::StartRecording
            });
        }
    }
    super::recording_warning::show(ui, &recording);
    super::recording_warning::show_streaming(ui, &streaming);
    if ui
        .add_enabled(
            editing_enabled,
            egui::Button::new(crate::i18n::tr(ui, "Studio Mode"))
                .selected(studio)
                .wrap_mode(egui::TextWrapMode::Extend)
                .min_size(egui::vec2(ui.available_width(), 24.0)),
        )
        .on_hover_text(crate::hints::text(ui, "Studio Mode"))
        .on_disabled_hover_text(crate::hints::text(ui, "Studio Mode"))
        .clicked()
    {
        intents.push(Intent::StudioMode(!studio));
    }
    if ui
        .add_enabled(
            editing_enabled,
            egui::Button::new(crate::i18n::tr(ui, "Settings"))
                .min_size(egui::vec2(ui.available_width(), 24.0)),
        )
        .on_hover_text(crate::hints::text(ui, "Settings"))
        .on_disabled_hover_text(crate::hints::text(ui, "Settings"))
        .clicked()
    {
        intents.push(Intent::OpenDialog(Dialog::general(model)));
    }
    if ui
        .add(
            egui::Button::new(crate::i18n::tr(ui, "Exit"))
                .min_size(egui::vec2(ui.available_width(), 24.0)),
        )
        .on_hover_text(crate::hints::text(ui, "Exit"))
        .on_disabled_hover_text(crate::hints::text(ui, "Exit"))
        .clicked()
    {
        intents.push(Intent::Exit);
    }
}
fn unique_name<'a>(base: &str, names: impl Iterator<Item = &'a str>) -> String {
    let names: Vec<_> = names.collect();
    if !names.contains(&base) {
        return base.into();
    }
    for number in 2..=4097 {
        let candidate = format!("{base} {number}");
        if !names.contains(&candidate.as_str()) {
            return candidate;
        }
    }
    format!("{base} copy")
}

#[cfg(test)]
mod visual_layout_tests {
    use super::*;
    #[test]
    fn active_and_disabled_control_tooltips_appear_after_real_pointer_hover() {
        fn text(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::epaint::Shape::Text(value) if value.galley.text() == label => {
                    Some(value.pos + value.galley.size() * 0.5)
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().find_map(|shape| text(shape, label))
                }
                _ => None,
            }
        }
        for locale in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
            for (caption, expected) in [
                (
                    "Start Streaming",
                    "Streaming is unavailable on this platform",
                ),
                (
                    "Settings",
                    "Open application settings. Only implemented settings pages are available.",
                ),
            ] {
                let context = egui::Context::default();
                crate::theme::install(&context);
                let catalog = std::sync::Arc::new(
                    skylive_host::localization::Localization::embedded(locale).unwrap(),
                );
                let caption = catalog.text(caption).into_owned();
                let expected = catalog.text(expected).into_owned();
                crate::i18n::install(&context, catalog, std::path::Path::new("translations.csv"));
                let session = skylive_core::Session::new();
                let model = session.snapshot();
                let mut intents = Vec::new();
                let mut render = |time, events| {
                    context.run(
                        egui::RawInput {
                            time: Some(time),
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(800.0, 560.0),
                            )),
                            events,
                            ..Default::default()
                        },
                        |context| {
                            egui::CentralPanel::default().show(context, |ui| {
                                let mut child = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(egui::Rect::from_min_size(
                                            egui::pos2(20.0, 20.0),
                                            egui::vec2(230.0, 300.0),
                                        ))
                                        .layout(egui::Layout::top_down(egui::Align::Min)),
                                );
                                controls(&mut child, &model, false, &mut intents);
                            });
                        },
                    )
                };
                let _ = render(0.0, vec![]);
                let painted = render(0.01, vec![]);
                let position = painted
                    .shapes
                    .iter()
                    .find_map(|shape| text(&shape.shape, &caption))
                    .unwrap();
                let _ = render(0.02, vec![egui::Event::PointerMoved(position)]);
                let _ = render(0.8, vec![]);
                let painted = render(1.0, vec![]);
                assert!(
                    painted
                        .shapes
                        .iter()
                        .any(|shape| text(&shape.shape, &expected).is_some()),
                    "{caption} must show its tooltip after hovering, including disabled controls"
                );
                assert!(intents.is_empty());
            }
        }
    }
    #[test]
    fn studio_tbar_and_mixer_status_remain_reachable_in_all_default_docks() {
        use skylive_core::{AudioCaptureBinding, AudioTarget};
        use skylive_host::audio_capture::{
            AudioBackendFactory, AudioCapabilities, AudioCaptureError, AudioCaptureWorker,
        };
        for viewport in [egui::vec2(800.0, 560.0), egui::vec2(1280.0, 800.0)] {
            let context = egui::Context::default();
            crate::theme::install(&context);
            let mut session = skylive_core::Session::new();
            let scene = session.preview();
            session
                .apply(Command::AddAudioCaptureSource {
                    scene,
                    name: "Mixer layout source".into(),
                    binding: AudioCaptureBinding::new(
                        AudioFlow::Input,
                        AudioTarget::Default,
                        false,
                        false,
                    )
                    .unwrap(),
                })
                .unwrap();
            let model = session.snapshot();
            let factory: AudioBackendFactory = Box::new(|| {
                Err(AudioCaptureError::Unsupported(
                    "layout test never opens audio",
                ))
            });
            let audio = AudioCaptureWorker::new(
                factory,
                AudioCapabilities {
                    backend: "unsupported layout fixture",
                    input: false,
                    output: false,
                    device_enumeration: false,
                    exclude_own_audio: false,
                    note: None,
                },
            )
            .unwrap();
            let snapshot = audio.snapshot();
            let mut intents = Vec::new();
            let area = egui::Rect::from_min_max(
                egui::pos2(8.0, viewport.y - 228.0),
                egui::pos2(viewport.x - 8.0, viewport.y - 8.0),
            );
            let columns = dock_rects(area, [true; 5]);
            let render = |events: Vec<egui::Event>, intents: &mut Vec<Intent>| {
                context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport)),
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            let mut child = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(area)
                                    .layout(egui::Layout::top_down(egui::Align::Min)),
                            );
                            child.set_clip_rect(area);
                            show(
                                &mut child,
                                &model,
                                scene,
                                None,
                                &Layout::default(),
                                DockViewState {
                                    editing_enabled: true,
                                    recording: Default::default(),
                                    streaming: Default::default(),
                                    transition: TransitionViewState {
                                        session: session.identity(),
                                        studio: true,
                                        active: false,
                                        manual: None,
                                        preset_focus: None,
                                    },
                                    audio: &snapshot,
                                    active_audio: &[],
                                    program_audio: None,
                                    program_status: None,
                                    media: None,
                                },
                                intents,
                            );
                        });
                    },
                )
            };
            let _ = render(vec![], &mut intents);
            let painted = render(vec![], &mut intents);
            fn status_visible(shape: &egui::epaint::Shape, clip: egui::Rect) -> bool {
                match shape {
                    egui::epaint::Shape::Text(text)
                        if text.galley.text() == "Signal unavailable · monitoring off" =>
                    {
                        let bounds = egui::Rect::from_min_size(text.pos, text.galley.size());
                        if !clip.contains_rect(bounds) {
                            eprintln!("status bounds={bounds:?}, clip={clip:?}");
                        }
                        clip.contains_rect(bounds)
                    }
                    egui::epaint::Shape::Vec(shapes) => {
                        shapes.iter().any(|shape| status_visible(shape, clip))
                    }
                    _ => false,
                }
            }
            assert!(
                painted
                    .shapes
                    .iter()
                    .any(|shape| status_visible(&shape.shape, shape.clip_rect)),
                "mixer signal status must be fully visible at {viewport:?}"
            );
            let slider = context
                .data_mut(|data| data.get_temp::<egui::Rect>(egui::Id::new("skylive-tbar-control")))
                .unwrap();
            assert!(
                columns[3].unwrap().contains_rect(slider),
                "T-bar must fit the default Studio transition dock"
            );
            let start = egui::pos2(slider.left() + 1.0, slider.center().y);
            let target = egui::pos2(
                slider.left() + slider.width() * (512.0 / 1023.0),
                slider.center().y,
            );
            let _ = render(
                vec![
                    egui::Event::PointerMoved(start),
                    egui::Event::PointerButton {
                        pos: start,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                &mut intents,
            );
            let _ = render(vec![egui::Event::PointerMoved(target)], &mut intents);
            assert!(
                intents
                    .iter()
                    .any(|intent| matches!(intent, Intent::ManualPosition(512))),
                "the visible T-bar must receive a real pointer drag at {viewport:?}"
            );
        }
    }
    #[test]
    fn controls_fit_default_and_minimum_windows_without_enabling_unavailable_outputs() {
        fn text_shapes<'a>(
            shape: &'a egui::epaint::Shape,
            texts: &mut Vec<(&'a egui::epaint::TextShape, egui::Rect)>,
            clip: egui::Rect,
        ) {
            match shape {
                egui::epaint::Shape::Text(text) => texts.push((text, clip)),
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text_shapes(shape, texts, clip);
                    }
                }
                _ => {}
            }
        }
        fn render(
            context: &egui::Context,
            model: &StudioSnapshot,
            viewport: egui::Vec2,
            rect: egui::Rect,
            events: Vec<egui::Event>,
            intents: &mut Vec<Intent>,
        ) -> egui::FullOutput {
            context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport)),
                    events,
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        let mut child = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(rect.shrink(5.0))
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                        );
                        child.set_clip_rect(rect);
                        controls(&mut child, model, false, intents);
                    });
                },
            )
        }
        for locale in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
            let catalog = std::sync::Arc::new(
                skylive_host::localization::Localization::embedded(locale).unwrap(),
            );
            for viewport in [egui::vec2(800.0, 560.0), egui::vec2(1280.0, 800.0)] {
                let context = egui::Context::default();
                crate::theme::install(&context);
                crate::i18n::install(
                    &context,
                    catalog.clone(),
                    std::path::Path::new("embedded.csv"),
                );
                let session = skylive_core::Session::new();
                let model = session.snapshot();
                let mut intents = Vec::new();
                let area = egui::Rect::from_min_max(
                    egui::pos2(8.0, viewport.y - 228.0),
                    egui::pos2(viewport.x - 8.0, viewport.y - 8.0),
                );
                let columns = dock_rects(area, [true; 5]);
                for index in 0..4 {
                    assert!(columns[index].unwrap().right() < columns[index + 1].unwrap().left());
                }
                assert!(columns[2].unwrap().width() > columns[0].unwrap().width());
                assert!(columns[4].unwrap().width() >= 170.0);
                let controls_rect = columns[4].unwrap();
                let painted = render(
                    &context,
                    &model,
                    viewport,
                    controls_rect,
                    vec![],
                    &mut intents,
                );
                let mut texts = Vec::new();
                for shape in &painted.shapes {
                    text_shapes(&shape.shape, &mut texts, shape.clip_rect);
                }
                for label in [
                    "Controls",
                    "Start Streaming",
                    "Start Recording",
                    "Start Virtual Camera",
                    "Start Replay Buffer",
                    "Studio Mode",
                    "Settings",
                    "Exit",
                ] {
                    let (text, clip) = texts
                        .iter()
                        .find(|(text, _)| {
                            text.galley.text() == crate::i18n::tr_context(&context, label)
                        })
                        .unwrap();
                    let bounds = egui::Rect::from_min_size(text.pos, text.galley.size());
                    assert_eq!(text.galley.rows.len(), 1);
                    assert!(
                        controls_rect.contains_rect(bounds) && clip.contains_rect(bounds),
                        "{label} must be fully visible at {viewport:?}"
                    );
                }
                assert!(intents.is_empty());
                for (label, available) in [("Start Streaming", false), ("Settings", true)] {
                    let (text, _) = texts
                        .iter()
                        .find(|(text, _)| {
                            text.galley.text() == crate::i18n::tr_context(&context, label)
                        })
                        .unwrap();
                    let pos = text.pos + text.galley.size() * 0.5;
                    for pressed in [true, false] {
                        let _ = render(
                            &context,
                            &model,
                            viewport,
                            controls_rect,
                            vec![
                                egui::Event::PointerMoved(pos),
                                egui::Event::PointerButton {
                                    pos,
                                    button: egui::PointerButton::Primary,
                                    pressed,
                                    modifiers: egui::Modifiers::NONE,
                                },
                            ],
                            &mut intents,
                        );
                    }
                    if available {
                        assert!(matches!(
                            intents.as_slice(),
                            [Intent::OpenDialog(Dialog::General { .. })]
                        ));
                    } else {
                        assert!(
                            intents.is_empty(),
                            "unavailable output must never create a fake command"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "source_actions_filters_tests.rs"]
mod source_actions_filters_tests;

#[cfg(test)]
#[path = "streaming_controls_tests.rs"]
mod streaming_controls_tests;

#[cfg(test)]
#[path = "camera_craft_filter_tests.rs"]
mod camera_craft_filter_tests;
