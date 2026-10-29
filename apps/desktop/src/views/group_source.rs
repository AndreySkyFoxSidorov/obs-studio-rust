//! Private-group authoring draft. Selection/collapse state belongs to the view.
use super::{
    Intent,
    list_reorder::{self, List, Scope},
};
use eframe::egui;
use skylive_core::{Command, Id, MAX_ITEMS, StudioSnapshot};
use std::collections::HashSet;

#[derive(Clone)]
struct Selection {
    session: u64,
    revision: u64,
    owner: Id,
    anchor: Id,
    items: Vec<Id>,
}
/// Visual-order range within one container; group children belong to their
/// own container and cannot accidentally join a parent selection.
fn sibling_range(model: &StudioSnapshot, owner: Id, anchor: Id, target: Id) -> Option<Vec<Id>> {
    let ids: Vec<_> = model.scene_container(owner)?.items().iter().rev().map(|item| item.id()).collect();
    let a = ids.iter().position(|id| *id == anchor)?;
    let b = ids.iter().position(|id| *id == target)?;
    let mut selected = ids[a.min(b)..=a.max(b)].to_vec();
    selected.retain(|id| *id != target);
    selected.push(target); // Existing selection contract uses the last item as focus.
    Some(selected)
}
fn selection_id(_root: Id) -> egui::Id {
    egui::Id::new("group-source-selection")
}
#[derive(Clone, Default)]
struct TreeState {
    session: u64,
    collapsed: HashSet<Id>,
}
#[derive(Default)]
struct TreeFrame {
    containers: Vec<ContainerRows>,
    groups: Vec<GroupTarget>,
    keyboard_rows: Vec<KeyboardRow>,
}
struct KeyboardRow { owner:Id, item:Id, widget:egui::Id, rect:egui::Rect, focused:bool }
#[derive(Clone)]
struct RemoveSelection {
    session:u64,
    revision:u64,
    root:Id,
    owner:Id,
    items:Vec<Id>,
    name:String,
}
fn remove_selection_id() -> egui::Id { egui::Id::new("group-source-remove-selection") }
/// Ask for confirmation using the same selection and revision guard for the
/// toolbar, context menu and keyboard. A context click on an unselected row
/// naturally falls back to that row through `selected_items`.
pub(super) fn request_remove_selection(ui:&egui::Ui,model:&StudioSnapshot,root:Id,session:u64,focus:Id) {
    let Some((owner,item))=model.item_container(focus) else{return;};
    if model.root_scene_for_container(owner)!=Some(root) {return;}
    let items=selected_items(ui,model,root,session,Some(focus));
    if model.container_is_locked(owner)||items.is_empty()||items.iter().any(|id|
        model.item_container(*id).is_none_or(|(container,item)|container!=owner||item.locked())) {return;}
    let name=model.source(item.source()).map(|source|source.name().to_owned()).unwrap_or_default();
    ui.ctx().data_mut(|data|data.insert_temp(remove_selection_id(),RemoveSelection {
        session,revision:model.revision(),root,owner,items,name,
    }));
}
struct ContainerRows {
    scope: Scope,
    rows: Vec<(Id, egui::Rect)>,
    depth: usize,
}
struct GroupTarget {
    container: Id,
    item: Id,
    rect: egui::Rect,
    locked: bool,
}
fn group_is_open(ui: &egui::Ui, model: &StudioSnapshot, session: u64, item: Id) -> bool {
    ui.data_mut(|data| {
        let state = data.get_temp_mut_or_default::<TreeState>(egui::Id::new("group-source-tree"));
        if state.session != session {
            state.session = session;
            state.collapsed.clear();
        }
        if state.collapsed.len() >= skylive_core::MAX_SOURCES {
            state
                .collapsed
                .retain(|id| model.item_container(*id).is_some());
        }
        !state.collapsed.contains(&item)
    })
}
pub(super) fn selected_items(
    ui: &egui::Ui,
    model: &StudioSnapshot,
    root: Id,
    session: u64,
    focus: Option<Id>,
) -> Vec<Id> {
    selected_items_context(ui.ctx(), model, root, session, focus)
}
pub(crate) fn selected_items_context(
    context: &egui::Context,
    model: &StudioSnapshot,
    root: Id,
    session: u64,
    focus: Option<Id>,
) -> Vec<Id> {
    if let Some(state) = context.data(|data| data.get_temp::<Selection>(selection_id(root))) {
        if state.session == session
            && state.revision == model.revision()
            && model.root_scene_for_container(state.owner) == Some(root)
            && state.items.last().copied() == focus
            && focus.is_none_or(|id| {
                model
                    .item_container(id)
                    .is_some_and(|(owner, _)| owner == state.owner)
            })
        {
            return state.items;
        }
    }
    focus
        .filter(|item| {
            model
                .item_container(*item)
                .is_some_and(|(owner, _)| model.root_scene_for_container(owner) == Some(root))
        })
        .into_iter()
        .collect()
}

/// Toggle a preview hit in the same source container as the existing selection.
/// The focused item is kept last, matching the source-tree selection contract.
pub(crate) fn toggle_preview_selection(
    context: &egui::Context,
    model: &StudioSnapshot,
    root: Id,
    session: u64,
    target: Id,
    mut items: Vec<Id>,
) -> Option<Id> {
    let (owner, _) = model.item_container(target)?;
    if model.root_scene_for_container(owner) != Some(root) { return None; }
    if items.iter().any(|id| model.item_container(*id).is_none_or(|(container, _)| container != owner)) {
        items.clear();
    }
    if let Some(index) = items.iter().position(|id| *id == target) {
        items.remove(index);
    } else {
        items.push(target);
    }
    let focus = items.last().copied();
    context.data_mut(|data| data.insert_temp(selection_id(root), Selection {
        session, revision: model.revision(), owner, anchor: focus.unwrap_or(target), items,
    }));
    focus
}

/// Keep a multi-selection when the user starts dragging another member.
pub(crate) fn focus_preview_selection(context:&egui::Context,model:&StudioSnapshot,root:Id,session:u64,target:Id,mut items:Vec<Id>)->Option<Id> {
    let (owner,_)=model.item_container(target)?;
    if model.root_scene_for_container(owner)!=Some(root) || !items.contains(&target)
        || items.iter().any(|id|model.item_container(*id).is_none_or(|(container,_)|container!=owner)) {return None;}
    items.retain(|id|*id!=target);
    items.push(target);
    context.data_mut(|data|data.insert_temp(selection_id(root),Selection {session,revision:model.revision(),owner,anchor:target,items}));
    Some(target)
}
/// Carry the same selection across a successful atomic preview move only.
pub(crate) fn retain_preview_selection_after_move(context:&egui::Context,model:&StudioSnapshot,root:Id,session:u64,old_revision:u64,focus:Option<Id>) {
    context.data_mut(|data| {
        let id=selection_id(root);
        let Some(mut state)=data.get_temp::<Selection>(id) else{return;};
        if state.session!=session || state.revision!=old_revision || state.items.last().copied()!=focus
            || model.root_scene_for_container(state.owner)!=Some(root)
            || state.items.iter().any(|item|model.item_container(*item).is_none_or(|(owner,_)|owner!=state.owner)) {return;}
        state.revision=model.revision();
        data.insert_temp(id,state);
    });
}
#[allow(clippy::too_many_arguments)]
pub(super) fn tree(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    root: Id,
    owner: Id,
    focus: Option<Id>,
    session: u64,
    intents: &mut Vec<Intent>,
    depth: usize,
) {
    let mut frame = TreeFrame::default();
    tree_visit(
        ui, model, root, owner, focus, session, intents, depth, &mut frame,
    );
    keyboard_navigation(ui, model, root, session, &frame, intents);
    resolve_drop(ui, model, session, &frame, intents);
}

pub(crate) fn confirm_remove_selection(ui:&mut egui::Ui, model:&StudioSnapshot, root:Id, session:u64, intents:&mut Vec<Intent>) {
    let key=remove_selection_id();
    let Some(pending)=ui.ctx().data(|data|data.get_temp::<RemoveSelection>(key)) else{return;};
    let valid=ui.is_enabled() && pending.session==session && pending.revision==model.revision()
        && pending.root==root && model.root_scene_for_container(pending.owner)==Some(root)
        && !model.container_is_locked(pending.owner) && !pending.items.is_empty()
        && pending.items.iter().all(|id|model.item_container(*id)
            .is_some_and(|(owner,item)|owner==pending.owner&&!item.locked()));
    if !valid {ui.ctx().data_mut(|data|{data.remove::<RemoveSelection>(key);});return;}
    let mut close=false;
    let response=egui::Modal::new(egui::Id::new("source-keyboard-remove-modal")).show(ui.ctx(),|ui|{
        ui.set_min_width(300.0);
        ui.heading(crate::i18n::tr_key(ui,"ConfirmRemove.Title"));
        let (text, replacement)=if pending.items.len()==1 {
            ("ConfirmRemove.Text",pending.name.clone())
        } else {("ConfirmRemove.TextMultiple",pending.items.len().to_string())};
        ui.label(crate::i18n::tr_key(ui,text).replace("%1",&replacement));
        ui.separator();
        let actions=[super::modal_footer::Action::new("Remove",true,"Remove"),super::modal_footer::Action::new("Cancel",true,"Cancel")];
        match super::modal_footer::show(ui,&actions) {
            Some(0)=>{intents.push(Intent::Command(Command::RemoveItems {scene:pending.owner,items:pending.items.clone()}));close=true;}
            Some(1)=>close=true,
            _=>{}
        }
    });
    if response.should_close() {close=true;}
    if close {ui.ctx().data_mut(|data|{data.remove::<RemoveSelection>(key);});}
}

/// Operate only while an actual source-name button owns focus. Text editors,
/// preview controls and other docks must keep their own arrow and select-all keys.
fn keyboard_navigation(ui:&mut egui::Ui, model:&StudioSnapshot, root:Id, session:u64, frame:&TreeFrame, intents:&mut Vec<Intent>) {
    if !ui.is_enabled() { return; }
    let Some(index)=frame.keyboard_rows.iter().position(|row|row.focused) else{return;};
    let current=&frame.keyboard_rows[index];
    if ui.input_mut(|input|input.consume_key(egui::Modifiers::NONE,egui::Key::Delete)
        ||input.consume_key(egui::Modifiers::NONE,egui::Key::Backspace)) {
        request_remove_selection(ui,model,root,session,current.item);
        return;
    }
    if ui.input_mut(|input|input.consume_key(egui::Modifiers::COMMAND,egui::Key::A)) {
        // Multi-selection operations require one common container, including
        // when a group is open and its children are interleaved on screen.
        let mut items:Vec<Id>=frame.keyboard_rows.iter().filter(|row|row.owner==current.owner)
            .map(|row|row.item).filter(|item|*item!=current.item).collect();
        items.push(current.item);
        ui.data_mut(|data|data.insert_temp(selection_id(root),Selection {
            session,revision:model.revision(),owner:current.owner,anchor:current.item,items,
        }));
        intents.push(Intent::SelectItem(Some(current.item)));
        return;
    }
    // Standard tree navigation operates only on the focused source row.
    // Opening a group leaves focus on it; a second Right enters its first
    // visible child. Left collapses an open group or selects its parent.
    let horizontal=ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE,egui::Key::ArrowLeft) {Some(false)}
        else if input.consume_key(egui::Modifiers::NONE,egui::Key::ArrowRight) {Some(true)}
        else {None}
    });
    if let Some(right)=horizontal {
        let group=model.item_container(current.item)
            .and_then(|(_,item)|model.source(item.source()))
            .and_then(|source|match source.kind(){
                skylive_core::SourceKind::Group {group}=>Some(group.scene()),
                _=>None,
            });
        if let Some(group)=group {
            let open=group_is_open(ui,model,session,current.item);
            if right && !open || !right && open {
                ui.data_mut(|data| {
                    let state=data.get_temp_mut_or_default::<TreeState>(egui::Id::new("group-source-tree"));
                    if right {state.collapsed.remove(&current.item);}
                    else if state.collapsed.len()<skylive_core::MAX_SOURCES {state.collapsed.insert(current.item);}
                });
                return;
            }
            if right {
                if let Some(next)=frame.keyboard_rows.iter().find(|row|row.owner==group) {
                    ui.data_mut(|data|data.insert_temp(selection_id(root),Selection {
                        session,revision:model.revision(),owner:next.owner,anchor:next.item,items:vec![next.item],
                    }));
                    ui.ctx().memory_mut(|memory|memory.request_focus(next.widget));
                    ui.scroll_to_rect(next.rect,Some(egui::Align::Center));
                    intents.push(Intent::SelectItem(Some(next.item)));
                }
                return;
            }
        }
        if !right {
            if let Some((_,parent))=model.group_parent(current.owner) {
                if let Some(next)=frame.keyboard_rows.iter().find(|row|row.item==parent) {
                    ui.data_mut(|data|data.insert_temp(selection_id(root),Selection {
                        session,revision:model.revision(),owner:next.owner,anchor:next.item,items:vec![next.item],
                    }));
                    ui.ctx().memory_mut(|memory|memory.request_focus(next.widget));
                    ui.scroll_to_rect(next.rect,Some(egui::Align::Center));
                    intents.push(Intent::SelectItem(Some(next.item)));
                }
            }
        }
        return;
    }
    let extend=ui.input(|input|input.modifiers.shift && !input.modifiers.command && !input.modifiers.alt);
    if extend {
        let step=ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::SHIFT,egui::Key::ArrowUp) {Some(false)}
            else if input.consume_key(egui::Modifiers::SHIFT,egui::Key::ArrowDown) {Some(true)}
            else {None}
        });
        if let Some(down)=step {
            let next=if down {frame.keyboard_rows[index+1..].iter().find(|row|row.owner==current.owner)}
                else {frame.keyboard_rows[..index].iter().rev().find(|row|row.owner==current.owner)};
            if let Some(next)=next {
                let anchor=ui.ctx().data(|data|data.get_temp::<Selection>(selection_id(root)))
                    .filter(|state|state.session==session && state.revision==model.revision()
                        && state.owner==current.owner && state.items.last()==Some(&current.item))
                    .map_or(current.item,|state|state.anchor);
                if let Some(items)=sibling_range(model,current.owner,anchor,next.item) {
                    ui.data_mut(|data|data.insert_temp(selection_id(root),Selection {
                        session,revision:model.revision(),owner:current.owner,anchor,items,
                    }));
                    ui.ctx().memory_mut(|memory|memory.request_focus(next.widget));
                    ui.scroll_to_rect(next.rect,Some(egui::Align::Center));
                    intents.push(Intent::SelectItem(Some(next.item)));
                }
            }
        }
        return;
    }
    let target=ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE,egui::Key::ArrowUp) {index.checked_sub(1)}
        else if input.consume_key(egui::Modifiers::NONE,egui::Key::ArrowDown) {(index+1<frame.keyboard_rows.len()).then_some(index+1)}
        else if input.consume_key(egui::Modifiers::NONE,egui::Key::Home) {Some(0)}
        else if input.consume_key(egui::Modifiers::NONE,egui::Key::End) {frame.keyboard_rows.len().checked_sub(1)}
        else {None}
    });
    let Some(next)=target.and_then(|target|frame.keyboard_rows.get(target)) else{return;};
    ui.data_mut(|data|data.insert_temp(selection_id(root),Selection {
        session,revision:model.revision(),owner:next.owner,anchor:next.item,items:vec![next.item],
    }));
    ui.ctx().memory_mut(|memory|memory.request_focus(next.widget));
    ui.scroll_to_rect(next.rect,Some(egui::Align::Center));
    intents.push(Intent::SelectItem(Some(next.item)));
}
#[allow(clippy::too_many_arguments)]
fn tree_visit(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    root: Id,
    owner: Id,
    focus: Option<Id>,
    session: u64,
    intents: &mut Vec<Intent>,
    depth: usize,
    frame: &mut TreeFrame,
) {
    if depth > skylive_core::MAX_SCENE_DEPTH {
        return;
    }
    let Some(scene) = model.scene_container(owner) else {
        return;
    };
    let scope = Scope {
        session,
        revision: model.revision(),
        list: List::Sources(owner),
    };
    let selected = selected_items(ui, model, root, session, focus);
    let mut rows = Vec::with_capacity(scene.items().len());
    for item in scene.items().iter().rev() {
        let Some(source) = model.source(item.source()) else {
            continue;
        };
        let group = if let skylive_core::SourceKind::Group { group } = source.kind() {
            Some(group.scene())
        } else {
            None
        };
        let mut open = group_is_open(ui, model, session, item.id());
        let tag=super::source_color_tags::effective(ui.ctx(),model,session,owner,item.id(),item.color_tag());
        let tint=tag.rgba().filter(|rgba|rgba[3]!=0);let fill=tint.map(|rgba|super::source_color_tags::row_fill(ui,rgba));let placeholder=ui.painter().add(egui::Shape::Noop);let mut foreground=None;
        let row=ui.scope(|ui|{if let Some(rgba)=tint{foreground=Some(super::source_color_tags::row_style(ui,rgba));}
        ui.push_id(item.id().get(), |ui| {
            ui.horizontal(|ui| {
                let mut visible = item.visible();
                let mut locked = item.locked();
                let visual = model
                    .source_video_dimensions(source.kind())
                    .is_ok_and(|value| value.is_some());
                if group.is_some() {
                    let label = if open {
                        "Collapse Group"
                    } else {
                        "Expand Group"
                    };
                    let response = super::dock_toolbar::row_toggle(
                        ui,
                        label,
                        &mut open,
                        super::dock_toolbar::RowIcon::Disclosure,
                    );
                    #[cfg(test)]
                    ui.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new(("group-toggle", item.id().get())),
                            response.rect,
                        )
                    });
                    if response.clicked() {
                        ui.data_mut(|data| {
                            let state = data.get_temp_mut_or_default::<TreeState>(egui::Id::new(
                                "group-source-tree",
                            ));
                            if open {
                                state.collapsed.remove(&item.id());
                            } else if state.collapsed.len() < skylive_core::MAX_SOURCES {
                                state.collapsed.insert(item.id());
                            }
                        });
                    }
                }
                super::source_icons::show(ui, source.kind());
                let response = ui
                    .add_sized(
                        [
                            (ui.available_width() - 44.0 - 2.0 * ui.spacing().item_spacing.x)
                                .max(1.0),
                            22.0,
                        ],
                        egui::Button::new(source.name())
                            .selected(selected.contains(&item.id()))
                            .frame(false)
                            .truncate()
                            .sense(egui::Sense::click_and_drag()),
                    )
                    .on_hover_text(super::source_color_tags::tooltip(ui,source.name(),tag));
                // OBS uses Return on macOS and F2 on other platforms. Scope
                // the shortcut to the focused source row, not a text editor.
                let rename_key = if cfg!(target_os = "macos") { egui::Key::Enter } else { egui::Key::F2 };
                if ui.is_enabled() && response.has_focus() && selected.contains(&item.id())
                    && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, rename_key))
                {
                    intents.push(Intent::OpenDialog(super::dialogs::Dialog::RenameSource {
                        source: source.id(), name: source.name().to_owned(),
                    }));
                }
                for (label, value, icon) in [
                    (
                        if visual { "Show" } else { "Active" },
                        &mut visible,
                        super::dock_toolbar::RowIcon::Visibility,
                    ),
                    ("Lock", &mut locked, super::dock_toolbar::RowIcon::Lock),
                ] {
                    let control = super::dock_toolbar::row_toggle(ui, label, value, icon);
                    #[cfg(test)]
                    ui.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new(("group-row-control", item.id().get(), label)),
                            control.rect,
                        )
                    });
                    if control.changed() {
                        intents.push(Intent::Command(if label == "Lock" {
                            Command::SetLocked {
                                scene: owner,
                                item: item.id(),
                                locked: *value,
                            }
                        } else {
                            Command::SetVisible {
                                scene: owner,
                                item: item.id(),
                                visible: *value,
                            }
                        }));
                    }
                }
                list_reorder::row(
                    ui,
                    &response,
                    scope,
                    item.id(),
                    item.locked() || model.container_is_locked(owner),
                );
                rows.push((item.id(), response.rect));
                frame.keyboard_rows.push(KeyboardRow {owner,item:item.id(),widget:response.id,rect:response.rect,focused:response.has_focus()});
                if let Some(container) = group {
                    frame.groups.push(GroupTarget {
                        container,
                        item: item.id(),
                        rect: response.rect,
                        locked: item.locked() || model.container_is_locked(owner),
                    });
                }
                if response.clicked() {
                    let modifiers = ui.input(|input| input.modifiers);
                    if modifiers.shift {
                        let anchor=ui.ctx().data(|data|data.get_temp::<Selection>(selection_id(root)))
                            .filter(|state|state.session==session && state.revision==model.revision()
                                && state.owner==owner && state.items.last().copied()==focus)
                            .map(|state|state.anchor).or(focus).unwrap_or(item.id());
                        let items=sibling_range(model,owner,anchor,item.id()).unwrap_or_else(||vec![item.id()]);
                        let anchor=if items.contains(&anchor){anchor}else{item.id()};
                        ui.data_mut(|data|data.insert_temp(selection_id(root),Selection {
                            session,revision:model.revision(),owner,anchor,items,
                        }));
                        intents.push(Intent::SelectItem(Some(item.id())));
                    } else {
                    let mut items = if modifiers.command || modifiers.ctrl {
                        selected.clone()
                    } else {
                        vec![]
                    };
                    if items.iter().any(|id| {
                        model
                            .item_container(*id)
                            .is_none_or(|(container, _)| container != owner)
                    }) {
                        items.clear();
                    }
                    if let Some(index) = items.iter().position(|id| *id == item.id()) {
                        items.remove(index);
                    } else {
                        items.push(item.id());
                    }
                    let focus = items.last().copied();
                    ui.data_mut(|data| {
                        data.insert_temp(
                            selection_id(root),
                            Selection {
                                session,
                                revision: model.revision(),
                                owner,
                                anchor: focus.unwrap_or(item.id()),
                                items,
                            },
                        )
                    });
                    intents.push(Intent::SelectItem(focus));
                    }
                }
                // OBS expands/collapses groups and opens configurable source
                // properties on double-click. Resolve the row itself, not the
                // previous selection.
                if response.double_clicked() {
                    if group.is_some() {
                        ui.data_mut(|data| {
                            let state = data.get_temp_mut_or_default::<TreeState>(egui::Id::new("group-source-tree"));
                            if !state.collapsed.remove(&item.id()) && state.collapsed.len() < skylive_core::MAX_SOURCES {
                                state.collapsed.insert(item.id());
                            }
                        });
                    } else if let Some(intent) = super::docks::source_properties_intent(source) {
                        intents.push(intent);
                    }
                }
                if response.secondary_clicked() && !selected.contains(&item.id()) {
                    ui.data_mut(|data| {
                        data.insert_temp(selection_id(root), Selection {
                            session,
                            revision: model.revision(),
                            owner,
                            anchor: item.id(),
                            items: vec![item.id()],
                        });
                    });
                    intents.push(Intent::SelectItem(Some(item.id())));
                }
                response.context_menu(|ui| {
                    super::docks::source_list_actions(
                        ui,
                        model,
                        owner,
                        Some(item.id()),
                        session,
                        intents,
                    )
                });
            });
        });
        });
        if let Some(fill)=fill{ui.painter().set(placeholder,egui::Shape::rect_filled(row.response.rect,2.0,fill));if selected.contains(&item.id()){ui.painter().rect_stroke(row.response.rect.shrink(0.75),2.0,egui::Stroke::new(1.5,foreground.unwrap_or(egui::Color32::WHITE)),egui::StrokeKind::Inside);}}
        if let Some(group) = group.filter(|_| open) {
            ui.indent(("group-children", item.id().get()), |ui| {
                tree_visit(
                    ui,
                    model,
                    root,
                    group,
                    focus,
                    session,
                    intents,
                    depth + 1,
                    frame,
                )
            });
        }
    }
    frame.containers.push(ContainerRows { scope, rows, depth });
}

fn resolve_drop(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    session: u64,
    frame: &TreeFrame,
    intents: &mut Vec<Intent>,
) {
    let Some(payload) = egui::DragAndDrop::payload::<list_reorder::Payload>(ui.ctx()) else {
        return;
    };
    if payload.scope.session != session
        || payload.scope.revision != model.revision()
        || !ui.is_enabled()
        || ui.input(|input| input.key_pressed(egui::Key::Escape))
    {
        egui::DragAndDrop::clear_payload(ui.ctx());
        return;
    }
    let List::Sources(from) = payload.scope.list else {
        return;
    };
    if !model
        .item_container(payload.id)
        .is_some_and(|(owner, item)| owner == from && !item.locked())
    {
        egui::DragAndDrop::clear_payload(ui.ctx());
        return;
    }
    let Some(pointer) = ui.input(|input| input.pointer.interact_pos()) else {
        return;
    };
    let viewport = ui.clip_rect();
    if !viewport.contains(pointer) || !ui.rect_contains_pointer(viewport) {
        return;
    }
    let released = ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary));
    if let Some(target) = frame.groups.iter().find(|target| {
        target.item != payload.id
            && egui::Rect::from_min_max(
                egui::pos2(
                    target.rect.left(),
                    target.rect.center().y - target.rect.height() * 0.25,
                ),
                egui::pos2(
                    target.rect.right(),
                    target.rect.center().y + target.rect.height() * 0.25,
                ),
            )
            .contains(pointer)
    }) {
        if !target.locked {
            ui.painter().with_clip_rect(viewport).rect_stroke(
                target.rect,
                2.0,
                egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
                egui::StrokeKind::Inside,
            );
            if released {
                let len = model
                    .scene_container(target.container)
                    .map_or(0, |scene| scene.items().len());
                let index = if from == target.container {
                    len.saturating_sub(1)
                } else {
                    len
                };
                intents.push(Intent::Reorder {
                    session,
                    revision: payload.scope.revision,
                    command: Command::MoveGroupItem {
                        from,
                        into: target.container,
                        item: payload.id,
                        index,
                    },
                });
                egui::DragAndDrop::clear_payload(ui.ctx());
            }
        } else {
            ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
            if released {
                egui::DragAndDrop::clear_payload(ui.ctx());
            }
        }
        return;
    }
    let bounds = |container: &ContainerRows| {
        container
            .rows
            .iter()
            .map(|row| row.1)
            .reduce(|a, b| a.union(b))
    };
    let target = frame
        .containers
        .iter()
        .filter(|container| bounds(container).is_some_and(|bounds| bounds.contains(pointer)))
        .max_by_key(|container| container.depth)
        .or_else(|| {
            frame
                .containers
                .iter()
                .find(|container| container.depth == 0)
        });
    let Some(target) = target else {
        return;
    };
    let List::Sources(into) = target.scope.list else {
        return;
    };
    if into == from {
        list_reorder::finish(ui, target.scope, &target.rows, intents);
        return;
    }
    if model.container_is_locked(into) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
        if released {
            egui::DragAndDrop::clear_payload(ui.ctx());
        }
        return;
    }
    let slot = target
        .rows
        .iter()
        .position(|row| pointer.y < row.1.center().y)
        .unwrap_or(target.rows.len());
    let y = if slot == 0 {
        target.rows.first().map_or(pointer.y, |row| row.1.top())
    } else {
        target.rows[slot - 1].1.bottom()
    };
    ui.painter().with_clip_rect(viewport).hline(
        viewport.x_range(),
        y,
        egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
    );
    if released {
        intents.push(Intent::Reorder {
            session,
            revision: payload.scope.revision,
            command: Command::MoveGroupItem {
                from,
                into,
                item: payload.id,
                index: target.rows.len() - slot,
            },
        });
        egui::DragAndDrop::clear_payload(ui.ctx());
        return;
    }
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

pub(super) fn actions(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    owner: Id,
    selected: Id,
    session: u64,
    intents: &mut Vec<Intent>,
) {
    let Some((_, item)) = model.item_container(selected) else {
        return;
    };
    let editable = !item.locked() && !model.container_is_locked(owner);
    let is_group = model
        .source(item.source())
        .is_some_and(|source| matches!(source.kind(), skylive_core::SourceKind::Group { .. }));
    if is_group {
        if ui
            .add_enabled(editable, egui::Button::new(crate::i18n::tr(ui, "Ungroup")))
            .on_hover_text(crate::i18n::tr(
                ui,
                "Move the group's children into its parent and remove the group.",
            ))
            .clicked()
        {
            intents.push(Intent::Command(Command::Ungroup {
                scene: owner,
                item: selected,
            }));
            ui.close_menu();
        }
    } else {
        let root = model.root_scene_for_container(owner).unwrap_or(owner);
        let items = selected_items(ui, model, root, session, Some(selected));
        let eligible = editable
            && !items.is_empty()
            && items.iter().all(|id| {
                model.item_container(*id).is_some_and(|(container, item)| {
                    container == owner
                        && !item.locked()
                        && model.source(item.source()).is_some_and(|source| {
                            !matches!(source.kind(), skylive_core::SourceKind::Group { .. })
                        })
                })
            });
        if ui.add_enabled(eligible, egui::Button::new(crate::i18n::tr(ui, "Group Selected Items"))).on_hover_text(crate::i18n::tr(ui, "Group selected source rows. Ctrl-click or Command-click rows to select more than one.")).clicked() {match GroupDraft::for_ui(ui,model,GroupEdit::Selected{scene:owner,items}){Ok(draft)=>intents.push(Intent::OpenDialog(super::dialogs::Dialog::Group(draft))),Err(error)=>intents.push(Intent::DialogError(error))}ui.close_menu(); }
    }
    ui.add_enabled_ui(editable, |ui| {
        ui.menu_button(crate::i18n::tr(ui, "Move into Group"), |ui| {
            if let Some(container) = model.scene_container(owner) {
                for sibling in container.items() {
                    if sibling.id() == selected {
                        continue;
                    }
                    if let Some(source) = model.source(sibling.source()).filter(|source| {
                        matches!(source.kind(), skylive_core::SourceKind::Group { .. })
                    }) {
                        if ui
                            .add_enabled(!sibling.locked(), egui::Button::new(source.name()))
                            .on_hover_text(source.name())
                            .clicked()
                        {
                            intents.push(Intent::Command(Command::MoveIntoGroup {
                                scene: owner,
                                item: selected,
                                group: sibling.id(),
                            }));
                            ui.close_menu();
                        }
                    }
                }
            }
        })
        .response
        .on_hover_text(crate::i18n::tr(
            ui,
            "Move this item into an existing sibling group.",
        ));
        if let Some((parent, group)) = model.group_parent(owner) {
            if ui
                .button(crate::i18n::tr(ui, "Move out of Group"))
                .on_hover_text(crate::i18n::tr(
                    ui,
                    "Move this item to the top of its parent group's scene.",
                ))
                .clicked()
            {
                let index = model
                    .scene_container(parent)
                    .map_or(0, |scene| scene.items().len());
                intents.push(Intent::Command(Command::MoveOutOfGroup {
                    scene: parent,
                    group,
                    item: selected,
                    index,
                }));
                ui.close_menu();
            }
        }
    });
}

pub enum GroupEdit {
    Empty { scene: Id },
    Selected { scene: Id, items: Vec<Id> },
}
pub struct GroupDraft {
    pub edit: GroupEdit,
    pub name: String,
}
impl GroupDraft {
    #[cfg(test)]
    pub fn empty(model: &StudioSnapshot, scene: Id) -> Result<Self, String> {
        Ok(Self {
            edit: GroupEdit::Empty { scene },
            name: unique_name(model, "Group")?,
        })
    }
    #[cfg(test)]
    pub fn selected(model: &StudioSnapshot, scene: Id, items: Vec<Id>) -> Result<Self, String> {
        Ok(Self {
            edit: GroupEdit::Selected { scene, items },
            name: unique_name(model, "Group")?,
        })
    }
    pub fn for_ui(ui: &egui::Ui, model: &StudioSnapshot, edit: GroupEdit) -> Result<Self, String> {
        Ok(Self {
            edit,
            name: unique_name(model, &crate::i18n::tr(ui, "Group"))?,
        })
    }
    pub fn command(&self) -> Result<Command, String> {
        match &self.edit {
            GroupEdit::Empty { scene } => Ok(Command::AddGroup {
                scene: *scene,
                name: self.name.clone(),
            }),
            GroupEdit::Selected { scene, items } => {
                if items.is_empty() || items.len() > MAX_ITEMS {
                    return Err("Select between one and 4096 source items to group.".into());
                }
                Ok(Command::GroupItems {
                    scene: *scene,
                    name: self.name.clone(),
                    items: items.clone(),
                })
            }
        }
    }
}
fn unique_name(model: &StudioSnapshot, label: &str) -> Result<String, String> {
    let exists = |name: &str| {
        model.sources().iter().any(|source| source.name() == name)
            || model.scenes().iter().any(|scene| scene.name() == name)
    };
    unique_name_in(
        label,
        model.sources().len() + model.scenes().len() + 2,
        exists,
    )
}
fn unique_name_in(
    label: &str,
    limit: usize,
    exists: impl Fn(&str) -> bool,
) -> Result<String, String> {
    if !exists(label) {
        return Ok(label.into());
    }
    // Names are unique and the total source count is bounded, so this always
    // finds a candidate after at most sources+root-scenes+one comparisons.
    for index in 2..=limit {
        let name = format!("{label} {index}");
        if !exists(&name) {
            return Ok(name);
        }
    }
    Err(
        "Unable to choose a unique group name. Rename an existing scene or source and try again."
            .into(),
    )
}
pub fn editor(ui: &mut egui::Ui, draft: &mut GroupDraft) {
    let label = ui.label(crate::i18n::tr(ui, "Name"));
    ui.add(
        egui::TextEdit::singleline(&mut draft.name)
            .char_limit(256)
            .id_salt("group-source-name"),
    )
    .labelled_by(label.id)
    .on_hover_text(crate::i18n::tr(
        ui,
        "Name this group so you can find it in Sources.",
    ));
    ui.label(crate::i18n::tr(ui, "A group keeps its own item order and local transforms. Its child sources remain shared with their other scene references."));
    if let GroupEdit::Selected { items, .. } = &draft.edit {
        ui.label(crate::i18n::tf(
            ui,
            "Selected source items: {}",
            &[("{}", items.len().to_string())],
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn narrow_source_rows_reserve_name_space_and_place_independent_icon_controls_on_right() {
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddColorSource {
                scene,
                name: "Rust MP4/AAC".into(),
                rgba: [0, 0, 0, 255],
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::AddGroup {
                scene,
                name: "Group".into(),
            })
            .unwrap();
        let context = egui::Context::default();
        crate::theme::install(&context);
        crate::i18n::install(
            &context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless.csv"),
        );
        let model = session.snapshot();
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(228.0, 240.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    tree(
                        ui,
                        &model,
                        scene,
                        scene,
                        None,
                        session.identity(),
                        &mut vec![],
                        0,
                    )
                });
            },
        );
        let row = context
            .data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new((
                    "reorder-row",
                    List::Sources(scene),
                    item.get(),
                )))
            })
            .unwrap();
        let show = context
            .data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new((
                    "group-row-control",
                    item.get(),
                    "Show",
                )))
            })
            .unwrap();
        let lock = context
            .data_mut(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new((
                    "group-row-control",
                    item.get(),
                    "Lock",
                )))
            })
            .unwrap();
        assert!(
            row.width() >= 140.0,
            "source names must not lose half the column to translated Show/Lock captions"
        );
        assert!(row.right() < show.left() && show.right() < lock.left());
        assert_eq!(show.size(), lock.size());
        assert!((show.center().y - row.center().y).abs() <= 1.0);
        assert!(lock.right() <= 220.0);
    }
    #[test]
    fn bounded_group_name_failure_is_an_error_and_localized_defaults_become_authored_names() {
        let result = unique_name_in("Group", 3, |_| true);
        assert!(result.is_err());
        // A normal namespace finds its first available suffix without touching
        // the canonical graph. The error path above cannot unwind the UI.
        let expected =
            unique_name_in("Group", 3, |name| name == "Group" || name == "Group 2").unwrap();
        assert_eq!(expected, "Group 3");
        for locale in ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            crate::i18n::install(
                &ctx,
                std::sync::Arc::new(
                    skylive_host::localization::Localization::embedded(locale).unwrap(),
                ),
                std::path::Path::new("headless.csv"),
            );
            let mut session = skylive_core::Session::new();
            let baseline = session.snapshot();
            let mut draft = None;
            let mut label = String::new();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    label = crate::i18n::tr(ui, "Group");
                    draft = Some(
                        GroupDraft::for_ui(
                            ui,
                            &baseline,
                            GroupEdit::Empty {
                                scene: session.preview(),
                            },
                        )
                        .unwrap(),
                    );
                });
            });
            let draft = draft.unwrap();
            assert_eq!(draft.name, label);
            assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
            session.apply(draft.command().unwrap()).unwrap();
            let model = session.snapshot();
            assert_eq!(model.sources()[0].name(), label);
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let next = GroupDraft::for_ui(
                        ui,
                        &model,
                        GroupEdit::Empty {
                            scene: session.preview(),
                        },
                    )
                    .unwrap();
                    assert_eq!(next.name, format!("{label} 2"));
                    tree(
                        ui,
                        &model,
                        session.preview(),
                        session.preview(),
                        None,
                        session.identity(),
                        &mut vec![],
                        0,
                    );
                });
            });
            crate::i18n::install(
                &ctx,
                crate::i18n::english().unwrap(),
                std::path::Path::new("headless.csv"),
            );
            assert_eq!(
                session.snapshot().sources()[0].name(),
                label,
                "changing language must not rename an authored group"
            );
        }
    }
    #[test]
    fn draft_uses_shared_command_boundary_without_mutating_the_model() {
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        let baseline = session.snapshot();
        let draft = GroupDraft::empty(&baseline, scene).unwrap();
        assert_eq!(draft.name, "Group");
        assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
        let item = session
            .apply(draft.command().unwrap())
            .unwrap()
            .created
            .unwrap();
        let next = GroupDraft::empty(&session.snapshot(), scene).unwrap();
        assert_eq!(next.name, "Group 2");
        assert!(
            GroupDraft::selected(&session.snapshot(), scene, vec![])
                .unwrap()
                .command()
                .is_err()
        );
        assert!(
            matches!(GroupDraft::selected(&session.snapshot(), scene, vec![item]).unwrap().command().unwrap(), Command::GroupItems { items, .. } if items==vec![item])
        );
    }
    fn caption(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().find_map(|shape| caption(shape, label))
            }
            _ => None,
        }
    }
    fn tree_frame(
        ctx: &egui::Context,
        session: &skylive_core::Session,
        selected: &mut Option<Id>,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
    ) -> (egui::FullOutput, Vec<Intent>) {
        let mut intents = vec![];
        let model = session.snapshot();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                modifiers,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    tree(
                        ui,
                        &model,
                        session.preview(),
                        session.preview(),
                        *selected,
                        session.identity(),
                        &mut intents,
                        0,
                    )
                });
            },
        );
        for intent in &intents {
            if let Intent::SelectItem(value) = intent {
                *selected = *value;
            }
        }
        (output, intents)
    }
    fn pointer(pos: egui::Pos2, pressed: bool, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            },
        ]
    }
    #[test]
    fn real_tree_multiselect_and_collapse_are_presentation_only_and_keep_child_owner() {
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        let mut ids = vec![];
        for name in ["First leaf", "Second leaf"] {
            ids.push(
                session
                    .apply(Command::AddColorSource {
                        scene,
                        name: name.into(),
                        rgba: [255; 4],
                    })
                    .unwrap()
                    .created
                    .unwrap(),
            );
        }
        let group_item = session
            .apply(Command::GroupItems {
                scene,
                name: "Tree group".into(),
                items: ids.clone(),
            })
            .unwrap()
            .created
            .unwrap();
        let baseline = session.snapshot();
        let context = egui::Context::default();
        crate::theme::install(&context);
        let mut selected = None;
        let (output, _) = tree_frame(
            &context,
            &session,
            &mut selected,
            vec![],
            egui::Modifiers::NONE,
        );
        for (label, modifiers) in [
            ("First leaf", egui::Modifiers::NONE),
            ("Second leaf", egui::Modifiers::CTRL),
        ] {
            let pos = output
                .shapes
                .iter()
                .find_map(|shape| caption(&shape.shape, label))
                .unwrap();
            tree_frame(
                &context,
                &session,
                &mut selected,
                pointer(pos, true, modifiers),
                modifiers,
            );
            tree_frame(
                &context,
                &session,
                &mut selected,
                pointer(pos, false, modifiers),
                modifiers,
            );
        }
        assert_eq!(selected, Some(ids[1]));
        // Reading the recorded selection must not replace the actual tree with
        // a blank frame: egui hit-testing correctly uses the previous pass's
        // registered widgets when the next pointer-down is delivered.
        let chosen = context
            .data(|data| data.get_temp::<Selection>(selection_id(scene)))
            .unwrap()
            .items;
        assert_eq!(chosen, ids);
        assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
        let rect = context
            .data(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new(("group-toggle", group_item.get())))
            })
            .unwrap();
        tree_frame(
            &context,
            &session,
            &mut selected,
            pointer(rect.center(), true, egui::Modifiers::NONE),
            egui::Modifiers::NONE,
        );
        let (output, _) = tree_frame(
            &context,
            &session,
            &mut selected,
            pointer(rect.center(), false, egui::Modifiers::NONE),
            egui::Modifiers::NONE,
        );
        let (output, _) = if output
            .shapes
            .iter()
            .any(|shape| caption(&shape.shape, "First leaf").is_some())
        {
            tree_frame(
                &context,
                &session,
                &mut selected,
                vec![],
                egui::Modifiers::NONE,
            )
        } else {
            (output, vec![])
        };
        assert!(
            !output
                .shapes
                .iter()
                .any(|shape| caption(&shape.shape, "First leaf").is_some())
        );
        assert!(
            output
                .shapes
                .iter()
                .any(|shape| caption(&shape.shape, "Tree group").is_some())
        );
        assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
    }
    #[test]
    fn actual_group_modal_cancel_and_apply_create_one_history_edit() {
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddColorSource {
                scene,
                name: "Leaf".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        let baseline = session.snapshot();
        let context = egui::Context::default();
        crate::theme::install(&context);
        for apply in [false, true] {
            let mut slot = Some(super::super::dialogs::Dialog::Group(
                GroupDraft::selected(&baseline, scene, vec![item]).unwrap(),
            ));
            let mut intents = vec![];
            let mut output = egui::FullOutput::default();
            for _ in 0..3 {
                output = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 560.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        super::super::dialogs::show(ctx, &mut slot, &mut intents, true, &baseline)
                    },
                );
            }
            let label = if apply { "Apply" } else { "Cancel" };
            let pos = output
                .shapes
                .iter()
                .find_map(|shape| caption(&shape.shape, label))
                .unwrap();
            for pressed in [true, false] {
                let _ = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 560.0),
                        )),
                        events: pointer(pos, pressed, egui::Modifiers::NONE),
                        ..Default::default()
                    },
                    |ctx| {
                        super::super::dialogs::show(ctx, &mut slot, &mut intents, true, &baseline)
                    },
                );
            }
            if apply {
                assert_eq!(intents.len(), 1);
                let Intent::Command(command) = intents.pop().unwrap() else {
                    panic!("shared command");
                };
                session.apply(command).unwrap();
                assert_eq!(session.snapshot().revision(), baseline.revision() + 1);
                session.apply(Command::Undo).unwrap();
                assert_eq!(session.snapshot().scenes(), baseline.scenes());
                assert_eq!(session.snapshot().sources(), baseline.sources());
            } else {
                assert!(slot.is_none());
                assert!(intents.is_empty());
                assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
            }
        }
    }
    fn drag_fixture() -> (skylive_core::Session, Id, Id, Id, Id, Id) {
        let mut session = skylive_core::Session::new();
        let scene = session.preview();
        let leaf = session
            .apply(Command::AddColorSource {
                scene,
                name: "Dragged leaf".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().item_container(leaf).unwrap().1.source();
        session
            .apply(Command::SetColorSize {
                source,
                width: 4,
                height: 2,
            })
            .unwrap();
        let mut transform = skylive_core::Transform::full(4, 2);
        transform.x = 10.0;
        transform.y = 20.0;
        session
            .apply(Command::SetTransform {
                scene,
                item: leaf,
                transform,
            })
            .unwrap();
        let a = session
            .apply(Command::GroupItems {
                scene,
                name: "Origin group".into(),
                items: vec![leaf],
            })
            .unwrap()
            .created
            .unwrap();
        let b = session
            .apply(Command::AddGroup {
                scene,
                name: "Destination group".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let model = session.snapshot();
        let private = |item| {
            let source = model.item_container(item).unwrap().1.source();
            let skylive_core::SourceKind::Group { group } = model.source(source).unwrap().kind()
            else {
                panic!("group");
            };
            group.scene()
        };
        let from = private(a);
        let into = private(b);
        (session, a, b, leaf, from, into)
    }
    fn begin_drag(
        context: &egui::Context,
        session: &skylive_core::Session,
        selected: &mut Option<Id>,
    ) -> egui::Pos2 {
        tree_frame(context, session, selected, vec![], egui::Modifiers::NONE);
        let (output, _) = tree_frame(context, session, selected, vec![], egui::Modifiers::NONE);
        let source = output
            .shapes
            .iter()
            .find_map(|shape| caption(&shape.shape, "Dragged leaf"))
            .unwrap();
        let target = output
            .shapes
            .iter()
            .find_map(|shape| caption(&shape.shape, "Destination group"))
            .unwrap();
        tree_frame(
            context,
            session,
            selected,
            pointer(source, true, egui::Modifiers::NONE),
            egui::Modifiers::NONE,
        );
        tree_frame(
            context,
            session,
            selected,
            vec![egui::Event::PointerMoved(source + egui::vec2(12.0, 0.0))],
            egui::Modifiers::NONE,
        );
        let (_, intents) = tree_frame(
            context,
            session,
            selected,
            vec![egui::Event::PointerMoved(target)],
            egui::Modifiers::NONE,
        );
        assert!(
            !intents
                .iter()
                .any(|intent| matches!(intent, Intent::Reorder { .. })),
            "hover cannot commit a membership edit"
        );
        target
    }
    #[test]
    fn actual_pointer_moves_between_groups_at_dpi_one_and_two_with_one_versioned_undo() {
        for dpi in [1.0, 2.0] {
            let (mut session, _, _, leaf, from, into) = drag_fixture();
            let context = egui::Context::default();
            crate::theme::install(&context);
            context.set_pixels_per_point(dpi);
            let mut selected = None;
            let before = session.snapshot();
            let target = begin_drag(&context, &session, &mut selected);
            let (_, intents) = tree_frame(
                &context,
                &session,
                &mut selected,
                pointer(target, false, egui::Modifiers::NONE),
                egui::Modifiers::NONE,
            );
            let mut moves = intents
                .into_iter()
                .filter_map(|intent| {
                    if let Intent::Reorder {
                        session,
                        revision,
                        command,
                    } = intent
                    {
                        Some((session, revision, command))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(moves.len(), 1);
            let (identity, revision, command) = moves.pop().unwrap();
            assert_eq!(identity, session.identity());
            assert_eq!(revision, before.revision());
            assert!(
                matches!(command,Command::MoveGroupItem{from:a,into:b,item,index:0}if a==from&&b==into&&item==leaf)
            );
            session.apply_at(Some(revision), command).unwrap();
            assert_eq!(session.snapshot().revision(), revision + 1);
            assert_eq!(session.snapshot().item_container(leaf).unwrap().0, into);
            session.apply(Command::Undo).unwrap();
            assert_eq!(session.snapshot().collection(), before.collection());
        }
    }
    #[test]
    fn pointer_membership_rejects_stale_scope_locked_target_and_escape_without_edit() {
        for mode in 0..3 {
            let (mut session, _, destination, leaf, from, _) = drag_fixture();
            let scene = session.preview();
            if mode == 1 {
                session
                    .apply(Command::SetLocked {
                        scene,
                        item: destination,
                        locked: true,
                    })
                    .unwrap();
            }
            let context = egui::Context::default();
            crate::theme::install(&context);
            let mut selected = None;
            let target = begin_drag(&context, &session, &mut selected);
            if mode == 0 {
                session
                    .apply(Command::AddScene {
                        name: "New revision".into(),
                    })
                    .unwrap();
            }
            if mode == 2 {
                let (_, intents) = tree_frame(
                    &context,
                    &session,
                    &mut selected,
                    vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    egui::Modifiers::NONE,
                );
                assert!(
                    !intents
                        .iter()
                        .any(|intent| matches!(intent, Intent::Reorder { .. }))
                );
            }
            let baseline = session.snapshot();
            let (_, intents) = tree_frame(
                &context,
                &session,
                &mut selected,
                pointer(target, false, egui::Modifiers::NONE),
                egui::Modifiers::NONE,
            );
            assert!(
                !intents
                    .iter()
                    .any(|intent| matches!(intent, Intent::Reorder { .. })),
                "mode{mode} must not publish a stale/locked/cancelled drag"
            );
            assert_eq!(session.snapshot().item_container(leaf).unwrap().0, from);
            assert!(std::sync::Arc::ptr_eq(&baseline, &session.snapshot()));
        }
    }
}
