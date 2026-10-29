//! OBS Designer menu order/labels are data; action IDs map to typed intentions.
use super::{
    Dock, Intent, Layout,
    dialogs::{Dialog, ProfileEdit},
};
use eframe::egui;
use serde::Deserialize;
use skylive_core::{
    Command, HotkeyAction, Id, MAX_PROFILES, ProfileCommand, Session, StudioSnapshot,
    TransformAction,
};

#[derive(Deserialize)]
pub struct Catalog {
    menu_order: Vec<String>,
    menus: Vec<Menu>,
    actions: Vec<Action>,
}
#[derive(Deserialize)]
struct Menu {
    id: String,
    label: String,
    items: Vec<Item>,
}
#[derive(Deserialize)]
struct Item {
    kind: String,
    id: String,
}
#[derive(Deserialize)]
struct Action {
    id: String,
    label: String,
}
#[derive(Clone, Copy)]
pub struct MenuContext<'a> {
    pub session: &'a Session,
    pub selected: Option<Id>,
    pub layout: &'a Layout,
    pub studio: bool,
    pub clipboard: &'a crate::edit_clipboard::Clipboard,
    pub source_clipboard: Option<&'a crate::source_clipboard::Clipboard>,
}
impl Catalog {
    pub fn load() -> Result<Self, serde_json::Error> {
        serde_json::from_str(include_str!("../../../../assets/obs-menu-catalog.json"))
    }
    pub fn show_with_clipboard(
        &self,
        ui: &mut egui::Ui,
        context: MenuContext<'_>,
        intents: &mut Vec<Intent>,
    ) {
        egui::menu::bar(ui, |ui| {
            for id in &self.menu_order {
                if let Some(menu) = self.menus.iter().find(|menu| &menu.id == id) {
                    ui.menu_button(crate::i18n::tr(ui, label(&menu.label)), |ui| {
                        self.show_menu(ui, menu, &context, intents)
                    })
                    .response
                    .on_hover_text(crate::hints::text(ui, "Menu"))
                    .on_disabled_hover_text(crate::hints::text(ui, "Menu"));
                }
            }
        });
    }
    fn show_menu(
        &self,
        ui: &mut egui::Ui,
        menu: &Menu,
        context: &MenuContext<'_>,
        intents: &mut Vec<Intent>,
    ) {
        let MenuContext {
            session,
            selected,
            layout,
            studio,
            clipboard,
            source_clipboard,
        } = *context;
        if menu.id=="multiviewProjectorMenu"{crate::projectors::menu(ui,session.identity(),session.snapshot().revision(),skylive_host::projector::Target::Multiview,intents);return;}
        if menu.id=="viewMenu"{ui.menu_button(crate::i18n::tr(ui,"Preview Projector"),|ui|crate::projectors::menu(ui,session.identity(),session.snapshot().revision(),skylive_host::projector::Target::Preview,intents));ui.menu_button(crate::i18n::tr(ui,"Program Projector"),|ui|crate::projectors::menu(ui,session.identity(),session.snapshot().revision(),skylive_host::projector::Target::Program,intents));if ui.button(crate::i18n::tr(ui,"Close All Projectors")).clicked(){intents.push(Intent::Projector(crate::projectors::Action::CloseAll{session:session.identity()}));ui.close_menu();}ui.separator();}
        if menu.id=="sceneCollectionMenu"{super::collection_library::menu(ui,session,intents);ui.separator();}
        if menu.id == "menu_File" {
            native_document_actions(ui, session, intents);
            ui.separator();
        }
        for item in &menu.items {
            match item.kind.as_str() {
                "separator" => {
                    ui.separator();
                }
                "menu" => {
                    if let Some(submenu) = self.menus.iter().find(|entry| entry.id == item.id) {
                        ui.menu_button(crate::i18n::tr(ui, label(&submenu.label)), |ui| {
                            self.show_menu(ui, submenu, context, intents)
                        })
                        .response
                        .on_hover_text(crate::hints::text(ui, "Menu"))
                        .on_disabled_hover_text(crate::hints::text(ui, "Menu"));
                    }
                }
                "action" => {
                    if menu.id == "menuDocks"
                        && matches!(item.id.as_str(), "lockDocks" | "sideDocks" | "resetDocks")
                    {
                        continue;
                    }
                    if let Some(action) = self.actions.iter().find(|entry| entry.id == item.id) {
                        let source_action = crate::source_clipboard::Action::from_id(&action.id);
                        let source_request = source_action.and_then(|action| source_clipboard.map(|clipboard| {
                            clipboard.request(action, session, crate::source_clipboard::selected(ui.ctx(), session, selected))
                        }));
                        let intent = source_request.as_ref().and_then(|request| request.as_ref().ok()).cloned()
                            .map(Intent::SourceClipboard).or_else(|| action_intent_with_clipboard(
                                &action.id, session, selected, studio, clipboard,
                            ));
                        let enabled = intent.is_some()&&(action.id!="actionRemoveSceneCollection"||super::collection_library::can_remove(ui.ctx(),session))&&(!matches!(action.id.as_str(),"actionNewSceneCollection"|"actionDupSceneCollection")||cfg!(target_os="windows"));
                        let mut text = crate::i18n::tr(ui, label(&action.label));
                        if matches!(action.id.as_str(), "actionScaleCanvas" | "actionScaleOutput") {
                            let model = session.snapshot();
                            let dimensions=if action.id=="actionScaleOutput" {model.output_video().ok().map(|v|(v.width(),v.height()))}else{Some((model.video().width(),model.video().height()))};
                            if let Some((width,height))=dimensions {text=text.replace("%1",&width.to_string()).replace("%2",&height.to_string());}
                        }
                        let checked = match action.id.as_str() {
                            "actionFullscreenInterface" => Some(ui.ctx().input(|input| input.viewport().fullscreen.unwrap_or(false))),
                            "actionAlwaysOnTop" => Some(ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::new("window-always-on-top"))).unwrap_or(false)),
                            "toggleStatusBar" => Some(ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::new("window-status-visible"))).unwrap_or(true)),
                            "actionLockPreview" => Some(ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::new("window-preview-locked"))).unwrap_or(false)),
                            "actionSceneGridMode" => Some(ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::new("scene-grid"))).unwrap_or(false)),
                            "actionSceneListMode" => Some(!ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::new("scene-grid"))).unwrap_or(false)),
                            "toggleContextBar" | "toggleContextToolbars" => Some(super::source_toolbar::source_bar_visible(ui.ctx())),
                            "toggleListboxToolbars" => Some(super::source_toolbar::listbox_bars_visible(ui.ctx())),
                            "toggleSourceIcons" => Some(ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::new("source-type-icons"))).unwrap_or(true)),
                            _ => None,
                        };
                        let response = if let Some(mut checked) = checked {
                            ui.add_enabled(enabled, egui::Checkbox::new(&mut checked, text))
                        } else {
                            ui.add_enabled(enabled, egui::Button::new(text).shortcut_text(action_shortcut(&action.id, session)))
                        }.on_hover_text(crate::hints::text(ui, &action.id));
                        if enabled {
                            if response.clicked() {
                                if let Some(intent) = intent {
                                    intents.push(intent);
                                    ui.close_menu();
                                }
                            }
                        } else if matches!(action.id.as_str(),"actionNewSceneCollection"|"actionDupSceneCollection"){response.on_disabled_hover_text(crate::i18n::tr(ui,"Managed collection writes are available only on Windows."));
                        } else if action.id=="actionRemoveSceneCollection"{
                            response.on_disabled_hover_text(crate::i18n::tr(ui,"Refresh the library and open an indexed managed collection. Removal requires another managed collection and Windows write support."));
                        } else if let Some(Err(reason)) = source_request {
                            response.on_disabled_hover_text(crate::i18n::tr(ui, &reason));
                        } else if let Some(action) =
                            crate::edit_clipboard::Action::from_id(&action.id)
                        {
                            if let Err(reason) = clipboard.request(action, session, selected) {
                                response.on_disabled_hover_text(reason.chrome().map_or_else(
                                    || reason.to_string(),
                                    |key| crate::i18n::tr(ui, key),
                                ));
                            }
                        } else {
                            response.on_disabled_hover_text(crate::i18n::tr(ui,match action.id.as_str() {
                                "actionRemoveProfile"=>"At least one profile is required.",
                                "actionNewProfile"|"actionDupProfile"=>"The profile catalog is full.",
                                _=>"This OBS action has not been implemented in Rust yet, or requires a selected editable item.",
                            }));
                        }
                    }
                }
                _ => {}
            }
        }
        if menu.id=="menuTools"&&session.snapshot().preferences().plugin_enabled(skylive_core::NDI_PLUGIN){
            ui.separator();if ui.button(crate::i18n::tr(ui,"NDI Output Settings")).on_hover_text(crate::i18n::tr(ui,"Configure and explicitly start or stop Program and Preview NDI senders.")).clicked(){intents.push(Intent::OpenNdiOutputs);ui.close_menu();}
        }
        if menu.id == "profileMenu" {
            ui.separator();
            let snapshot = session.snapshot();
            for profile in snapshot.profiles().entries() {
                ui.push_id(("profile-selection", profile.id()), |ui| {
                    if ui
                        .radio(
                            snapshot.profiles().selected_id() == profile.id(),
                            profile.name(),
                        )
                        .on_hover_text(crate::hints::text(ui, "Profile"))
                        .on_disabled_hover_text(crate::hints::text(ui, "Profile"))
                        .clicked()
                    {
                        intents.push(Intent::Command(Command::Profile {
                            command: ProfileCommand::Select { id: profile.id() },
                        }));
                        ui.close_menu();
                    }
                });
            }
        }
        if menu.id == "menuDocks" {
            super::dock_layout::menu(ui, layout, intents);
            if ui
                .button(crate::i18n::tr(ui, "Reset Layout"))
                .on_hover_text(crate::i18n::tr(
                    ui,
                    "Restore default dock widths, visibility and compact selection.",
                ))
                .clicked()
            {
                intents.push(Intent::ResetLayout);
                ui.close_menu();
            }
            ui.separator();
            for (name, visible, dock) in [
                ("Scenes", layout.scenes, Dock::Scenes),
                ("Sources", layout.sources, Dock::Sources),
                ("Audio Mixer", layout.mixer, Dock::Mixer),
                ("Scene Transitions", layout.transitions, Dock::Transitions),
                ("Controls", layout.controls, Dock::Controls),
            ] {
                let mut checked = visible;
                if ui
                    .checkbox(&mut checked, crate::i18n::tr(ui, name))
                    .on_hover_text(crate::hints::text(ui, name))
                    .on_disabled_hover_text(crate::hints::text(ui, name))
                    .changed()
                {
                    intents.push(Intent::ToggleDock(dock));
                }
            }
        }
        if menu.id == "menuDocks"
            && session
                .snapshot()
                .preferences()
                .plugin_enabled(skylive_core::SCENE_NOTES_PLUGIN)
        {
            ui.separator();
            let mut visible = layout.arrangement.scene_notes_visible;
            if ui.checkbox(&mut visible,crate::i18n::tr(ui,"Scene Notes Dock"))
                .on_hover_text(crate::i18n::tr(ui,"Show or hide the plain-text Scene Notes dock. Hiding retains its draft and authored notes."))
                .on_disabled_hover_text(crate::i18n::tr(ui,"Show or hide the plain-text Scene Notes dock. Hiding retains its draft and authored notes.")).changed() {
                intents.push(Intent::ToggleDock(Dock::SceneNotes));
            }
        }
        if menu.items.is_empty() && menu.id != "menuDocks" {
            ui.add_enabled(
                false,
                egui::Button::new(crate::i18n::tr(ui, "No providers available")),
            )
            .on_hover_text(crate::hints::text(ui, "No providers available"))
            .on_disabled_hover_text(crate::i18n::tr(
                ui,
                "Dynamic OBS provider entries are pending implementation.",
            ));
        }
    }
}
fn native_document_actions(ui: &mut egui::Ui, session: &Session, intents: &mut Vec<Intent>) {
    use super::document_picker::DocumentAction;
    for (id, label, hint, intent) in [
        (
            "native-file-new",
            "New collection",
            "Create a new collection after resolving unsaved changes.",
            Intent::NewCollection,
        ),
        (
            "native-file-open",
            "Open…",
            "Choose a SkyLive collection to open.",
            Intent::OpenDocumentPicker(DocumentAction::Open),
        ),
        (
            "native-file-recover",
            "Recover…",
            "Choose a SkyLive collection to recover from its checkpoint.",
            Intent::OpenDocumentPicker(DocumentAction::Recover),
        ),
        (
            "native-file-save",
            "Save",
            "Save changes to the active collection file.",
            Intent::Save,
        ),
        (
            "native-file-save-as",
            "Save As…",
            "Choose and explicitly confirm a different native save destination.",
            Intent::SaveAs,
        ),
        (
            "native-file-import",
            "Import OBS…",
            "Choose an OBS scene collection to import.",
            Intent::OpenObsImport,
        ),
        (
            "native-file-export",
            "Export OBS…",
            "Choose an OBS scene collection export destination.",
            Intent::OpenObsExport,
        ),
    ] {
        let shortcut = if id == "native-file-save" {
            session
                .snapshot()
                .preferences()
                .hotkeys()
                .bindings()
                .iter()
                .filter(|binding| binding.action == HotkeyAction::Save)
                .map(|binding| crate::shortcuts::label(&binding.chord))
                .collect::<Vec<_>>()
                .join(" / ")
        } else {
            String::new()
        };
        let clicked = ui
            .push_id(id, |ui| {
                ui.add(egui::Button::new(crate::i18n::tr(ui, label)).shortcut_text(shortcut))
                    .on_hover_text(crate::i18n::tr(ui, hint))
                    .clicked()
            })
            .inner;
        if clicked {
            intents.push(intent);
            ui.close_menu();
        }
    }
}
fn label(value: &str) -> String {
    value.replace('&', "")
}

// Only advertise shortcuts routed by the actual application adapter. OBS's
// Designer catalog also contains chords for actions that are not wired here.
fn action_shortcut(id: &str, session: &Session) -> String {
    if let Some(action) = crate::source_clipboard::Action::from_id(id) {
        return crate::source_clipboard::shortcut(action, session);
    }
    if let Some(action) = crate::edit_clipboard::Action::from_id(id) {
        return crate::edit_clipboard_shortcuts::label(action, session);
    }
    let action = match id {
        "actionMainUndo" => HotkeyAction::Undo,
        "actionMainRedo" => HotkeyAction::Redo,
        _ => return String::new(),
    };
    session
        .snapshot()
        .preferences()
        .hotkeys()
        .bindings()
        .iter()
        .filter(|binding| binding.action == action)
        .map(|binding| crate::shortcuts::label(&binding.chord))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Clipboard actions require this window's typed presentation state.
pub fn action_intent_with_clipboard(
    id: &str,
    session: &Session,
    selected: Option<Id>,
    studio: bool,
    clipboard: &crate::edit_clipboard::Clipboard,
) -> Option<Intent> {
    if let Some(action) = crate::edit_clipboard::Action::from_id(id) {
        return clipboard
            .request(action, session, selected)
            .ok()
            .map(Intent::Clipboard);
    }
    action_intent(id, session, selected, studio)
}

pub fn action_intent(
    id: &str,
    session: &Session,
    selected: Option<Id>,
    studio: bool,
) -> Option<Intent> {
    if let Some(target) = skylive_host::help_links::Target::from_action_id(id) {
        return Some(Intent::OpenHelpLink(target));
    }
    let snapshot = session.snapshot();
    let scene = session.preview();
    match id {
        "actionInteract" => {
            if !skylive_host::browser::BrowserOwner::available(){return None;}
            let (owner,item)=snapshot.item_container(selected?)?;
            if snapshot.root_scene_for_container(owner)!=Some(scene){return None;}
            let source=snapshot.source(item.source())?;
            matches!(source.kind(),skylive_core::SourceKind::Browser {..}).then_some(Intent::InteractBrowser {session:session.identity(),revision:snapshot.revision(),source:source.id()})
        }
        "actionShowMissingFiles"=>Some(Intent::OpenMissingFiles),
        "actionRemux"=>Some(Intent::OpenRemux),
        "stats" => Some(Intent::OpenStatistics),
        "actionFullscreenInterface" => Some(Intent::ToggleFullscreen),
        "actionAlwaysOnTop" => Some(Intent::ToggleAlwaysOnTop),
        "toggleStatusBar" => Some(Intent::ToggleStatusBar),
        "actionLockPreview" => Some(Intent::TogglePreviewLock),
        "resetUI" => Some(Intent::ResetLayout),
        "actionSceneGridMode" => Some(Intent::SceneGrid(true)),
        "actionSceneListMode" => Some(Intent::SceneGrid(false)),
        "toggleSourceIcons" => Some(Intent::ToggleSourceIcons),
        "toggleContextBar" | "toggleContextToolbars" => Some(Intent::ToggleSourceToolbar),
        "toggleListboxToolbars" => Some(Intent::ToggleListboxToolbars),
        "actionScaleWindow" => Some(Intent::PreviewDisplay(crate::preview_display::Action::FitWindow)),
        "actionScaleCanvas" => Some(Intent::PreviewDisplay(crate::preview_display::Action::Canvas)),
        "actionScaleOutput" => Some(Intent::PreviewDisplay(crate::preview_display::Action::Output)),
        "actionPreviewZoomIn" => Some(Intent::PreviewDisplay(crate::preview_display::Action::ZoomIn)),
        "actionPreviewZoomOut" => Some(Intent::PreviewDisplay(crate::preview_display::Action::ZoomOut)),
        "actionPreviewResetZoom" => Some(Intent::PreviewDisplay(crate::preview_display::Action::ResetZoom)),
        "actionShow_Recordings" => Some(Intent::ShowRecordingsFolder),
        "actionShowSettingsFolder" => Some(Intent::ShowSettingsFolder),
        "actionImportProfile" if snapshot.profiles().entries().len() < MAX_PROFILES => Some(Intent::OpenDialog(Dialog::ProfilePath { import: true, path: String::new() })),
        "actionExportProfile" => Some(Intent::OpenDialog(Dialog::ProfilePath { import: false, path: "profile.skyliveprofile".into() })),
        "actionAdvAudioProperties" | "actionMixerToolbarAdvAudio" => {
            Some(Intent::OpenAdvancedAudio)
        }
        "autoConfigure" => Some(Intent::OpenSetup),
        "actionOpenPluginManager" => Some(Intent::OpenDialog(Dialog::Plugins {
            query: String::new(),
            session: 0,
            token: 0,
        })),
        "actionNewProfile" if snapshot.profiles().entries().len() < MAX_PROFILES => {
            Some(Intent::OpenDialog(Dialog::ProfileName {
                edit: ProfileEdit::New,
                run_setup: snapshot.preferences().setup_on_new_profile(),
                name: unique_profile_name(&snapshot, "Untitled"),
            }))
        }
        "actionDupProfile" if snapshot.profiles().entries().len() < MAX_PROFILES => {
            Some(Intent::OpenDialog(Dialog::ProfileName {
                edit: ProfileEdit::Duplicate,
                run_setup: false,
                name: unique_profile_name(
                    &snapshot,
                    &format!("{} Copy", snapshot.profiles().selected().name()),
                ),
            }))
        }
        "actionRenameProfile" => Some(Intent::OpenDialog(Dialog::ProfileName {
            edit: ProfileEdit::Rename(snapshot.profiles().selected_id()),
            run_setup: false,
            name: snapshot.profiles().selected().name().into(),
        })),
        "actionRemoveProfile" if snapshot.profiles().entries().len() > 1 => {
            Some(Intent::OpenDialog(Dialog::RemoveProfile {
                id: snapshot.profiles().selected_id(),
                name: snapshot.profiles().selected().name().into(),
            }))
        }
        "actionMainUndo" if session.can_undo() => Some(Intent::Command(Command::Undo)),
        "actionMainRedo" if session.can_redo() => Some(Intent::Command(Command::Redo)),
        "action_Settings" => Some(Intent::OpenDialog(Dialog::general(&snapshot))),
        "actionE_xit" => Some(Intent::Exit),
        "actionShowAbout" => Some(Intent::About),
        "actionNewSceneCollection" => Some(Intent::CollectionLibrary(super::collection_library::Action::Create{duplicate:false})),
        "actionDupSceneCollection"=>Some(Intent::CollectionLibrary(super::collection_library::Action::Create{duplicate:true})),
        "actionRemoveSceneCollection"=>Some(Intent::CollectionLibrary(super::collection_library::Action::Remove)),
        "actionImportSceneCollection" => Some(Intent::OpenObsImport),
        "actionExportSceneCollection" => Some(Intent::OpenObsExport),
        "actionRenameSceneCollection" => Some(Intent::OpenDialog(Dialog::Collection {
            name: snapshot.name().into(),
        })),
        "actionStudioMode" => Some(Intent::StudioMode(!studio)),
        "actionEditTransform" => {
            let (owner, item) = selected.and_then(|id| snapshot.item_container(id))?;
            if snapshot.root_scene_for_container(owner) != Some(scene) {
                return None;
            }
            (!item.locked()
                && !snapshot.container_is_locked(owner)
                && snapshot
                    .source_video_dimensions(snapshot.source(item.source())?.kind())
                    .ok()
                    .flatten()
                    .is_some())
            .then(|| {
                Intent::OpenDialog(Dialog::Transform {
                    scene: owner,
                    item: item.id(),
                    draft: item.transform().clone(),
                    capture_context: None,
                })
            })
        }
        "actionResetTransform"
        | "actionFitToScreen"
        | "actionStretchToScreen"
        | "actionCenterToScreen"
        | "actionHorizontalCenter"
        | "actionVerticalCenter"
        | "actionRotate90CW"
        | "actionRotate90CCW"
        | "actionRotate180"
        | "actionFlipHorizontal"
        | "actionFlipVertical" => {
            let (owner, item) = selected.and_then(|id| snapshot.item_container(id))?;
            if snapshot.root_scene_for_container(owner) != Some(scene) {
                return None;
            }
            let action = match id {
                "actionResetTransform" => TransformAction::Reset,
                "actionFitToScreen" => TransformAction::FitToCanvas,
                "actionStretchToScreen" => TransformAction::StretchToCanvas,
                "actionHorizontalCenter" => TransformAction::CenterHorizontal,
                "actionVerticalCenter" => TransformAction::CenterVertical,
                "actionRotate90CW" => TransformAction::Rotate90Clockwise,
                "actionRotate90CCW" => TransformAction::Rotate90CounterClockwise,
                "actionRotate180" => TransformAction::Rotate180,
                "actionFlipHorizontal" => TransformAction::FlipHorizontal,
                "actionFlipVertical" => TransformAction::FlipVertical,
                _ => TransformAction::Center,
            };
            (!item.locked()
                && !snapshot.container_is_locked(owner)
                && snapshot
                    .source_video_dimensions(snapshot.source(item.source())?.kind())
                    .ok()
                    .flatten()
                    .is_some())
            .then(|| {
                Intent::Command(Command::TransformItem {
                    scene: owner,
                    item: item.id(),
                    action,
                })
            })
        }
        "actionMoveUp" | "actionMoveDown" | "actionMoveToTop" | "actionMoveToBottom" => {
            let (owner, selected_item) = selected.and_then(|id| snapshot.item_container(id))?;
            if snapshot.root_scene_for_container(owner) != Some(scene) {
                return None;
            }
            let items = snapshot.scene_container(owner)?.items();
            let index = items
                .iter()
                .position(|item| item.id() == selected_item.id())?;
            if items[index].locked() || snapshot.container_is_locked(owner) {
                return None;
            }
            let target = match id {
                "actionMoveUp" => index.checked_add(1).filter(|value| *value < items.len())?,
                "actionMoveDown" => index.checked_sub(1)?,
                "actionMoveToTop" => items.len().checked_sub(1)?,
                _ => 0,
            };
            (target != index).then(|| {
                Intent::Command(Command::MoveItem {
                    scene: owner,
                    item: items[index].id(),
                    index: target,
                })
            })
        }
        _ => None,
    }
}

fn unique_profile_name(model: &StudioSnapshot, base: &str) -> String {
    if !model
        .profiles()
        .entries()
        .iter()
        .any(|profile| profile.name() == base)
    {
        return base.into();
    }
    for number in 2..=MAX_PROFILES + 1 {
        let candidate = format!("{base} {number}");
        if !model
            .profiles()
            .entries()
            .iter()
            .any(|profile| profile.name() == candidate)
        {
            return candidate;
        }
    }
    base.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_preserves_obs_top_menu_order() {
        let catalog = Catalog::load().unwrap();
        assert_eq!(
            catalog.menu_order,
            [
                "menu_File",
                "menuBasic_MainMenu_Edit",
                "viewMenu",
                "menuDocks",
                "profileMenu",
                "sceneCollectionMenu",
                "menuTools",
                "menuBasic_MainMenu_Help"
            ]
        );
        assert_eq!(catalog.actions.len(), 102);
    }
    #[test]
    fn profile_actions_capture_typed_identity_and_guard_last_remove_and_full_catalog() {
        let mut session = Session::new();
        let id = session.snapshot().profiles().selected_id();
        assert!(action_intent("actionRemoveProfile", &session, None, false).is_none());
        assert!(matches!(
            action_intent("actionNewProfile", &session, None, false),
            Some(Intent::OpenDialog(Dialog::ProfileName {
                edit: ProfileEdit::New,
                ..
            }))
        ));
        assert!(
            matches!(action_intent("actionRenameProfile",&session,None,false),Some(Intent::OpenDialog(Dialog::ProfileName {edit:ProfileEdit::Rename(actual),..})) if actual==id)
        );
        for index in 1..MAX_PROFILES {
            session
                .apply(Command::Profile {
                    command: ProfileCommand::New {
                        name: format!("Profile {index}"),
                    },
                })
                .unwrap();
        }
        assert!(action_intent("actionNewProfile", &session, None, false).is_none());
        assert!(action_intent("actionDupProfile", &session, None, false).is_none());
        let selected = session.snapshot().profiles().selected_id();
        assert!(
            matches!(action_intent("actionRemoveProfile",&session,None,false),Some(Intent::OpenDialog(Dialog::RemoveProfile {id,..})) if id==selected)
        );
        assert!(action_intent("actionImportProfile", &session, None, false).is_none());
        assert!(action_intent("actionExportProfile", &session, None, false).is_none());
    }
    #[test]
    fn unavailable_media_actions_do_not_produce_fake_commands() {
        let session = Session::new();
        for id in [
            "actionStartStreaming",
            "actionStartRecording",
            "actionMainUndo",
            "actionFitToScreen",
        ] {
            assert!(action_intent(id, &session, None, false).is_none());
        }
        assert!(matches!(
            action_intent("action_Settings", &session, None, false),
            Some(Intent::OpenDialog(_))
        ));
    }
    #[test]
    fn obs_interchange_menu_uses_guarded_frontend_intents() {
        let mut session = Session::new();
        assert!(matches!(
            action_intent("actionImportSceneCollection", &session, None, false),
            Some(Intent::OpenObsImport)
        ));
        assert!(matches!(
            action_intent("actionExportSceneCollection", &session, None, false),
            Some(Intent::OpenObsExport)
        ));
        session
            .apply(Command::AddScene {
                name: "Unsaved".into(),
            })
            .unwrap();
        assert!(matches!(
            action_intent("actionImportSceneCollection", &session, None, false),
            Some(Intent::OpenObsImport)
        ));
        assert!(matches!(
            action_intent("actionNewSceneCollection", &session, None, false),
            Some(Intent::NewCollection)
        ));
        assert!(matches!(
            action_intent("actionExportSceneCollection", &session, None, false),
            Some(Intent::OpenObsExport)
        ));
    }
    #[test]
    fn rotate_and_flip_menu_actions_require_an_unlocked_selected_item() {
        let mut session = Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddColorSource {
                scene,
                name: "Transform target".into(),
                rgba: [0, 0, 0, 255],
            })
            .unwrap()
            .created
            .unwrap();
        for id in [
            "actionRotate90CW",
            "actionRotate90CCW",
            "actionRotate180",
            "actionFlipHorizontal",
            "actionFlipVertical",
        ] {
            assert!(action_intent(id, &session, None, false).is_none());
            let Some(Intent::Command(Command::TransformItem {
                scene: actual_scene,
                item: actual_item,
                action,
            })) = action_intent(id, &session, Some(item), false)
            else {
                panic!("{id} must adapt to the shared transform command");
            };
            assert_eq!(actual_scene, scene);
            assert_eq!(actual_item, item);
            assert!(matches!(
                (id, action),
                ("actionRotate90CW", TransformAction::Rotate90Clockwise)
                    | (
                        "actionRotate90CCW",
                        TransformAction::Rotate90CounterClockwise
                    )
                    | ("actionRotate180", TransformAction::Rotate180)
                    | ("actionFlipHorizontal", TransformAction::FlipHorizontal)
                    | ("actionFlipVertical", TransformAction::FlipVertical)
            ));
        }
        session
            .apply(Command::SetLocked {
                scene,
                item,
                locked: true,
            })
            .unwrap();
        for id in [
            "actionEditTransform",
            "actionRotate90CW",
            "actionRotate90CCW",
            "actionRotate180",
            "actionFlipHorizontal",
            "actionFlipVertical",
        ] {
            assert!(
                action_intent(id, &session, Some(item), false).is_none(),
                "{id} must respect the locked state"
            );
        }
    }
    #[test]
    fn actual_menu_paints_only_current_application_shortcuts() {
        use skylive_core::{HotkeyBinding, HotkeyBindings, HotkeyChord};
        let mut session = Session::new();
        session
            .apply(Command::SetHotkeys {
                hotkeys: HotkeyBindings::new(vec![
                    HotkeyBinding {
                        action: HotkeyAction::Undo,
                        chord: HotkeyChord::new("F9", false, false, false).unwrap(),
                    },
                    HotkeyBinding {
                        action: HotkeyAction::Undo,
                        chord: HotkeyChord::new("F10", false, false, false).unwrap(),
                    },
                    HotkeyBinding {
                        action: HotkeyAction::Save,
                        chord: HotkeyChord::new("S", true, false, false).unwrap(),
                    },
                ])
                .unwrap(),
            })
            .unwrap();
        assert_eq!(action_shortcut("actionMainUndo", &session), "F9 / F10");
        assert!(action_shortcut("actionMainRedo", &session).is_empty());
        for id in [
            "actionStretchToScreen",
            "actionMoveUp",
            "actionFullscreenInterface",
        ] {
            assert!(
                action_shortcut(id, &session).is_empty(),
                "unwired {id} cannot advertise a chord"
            );
        }
        let context = egui::Context::default();
        crate::theme::install(&context);
        let catalog = Catalog::load().unwrap();
        let layout = Layout::default();
        let mut intents = vec![];
        let output = context.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let menu = catalog
                    .menus
                    .iter()
                    .find(|menu| menu.id == "menuBasic_MainMenu_Edit")
                    .unwrap();
                catalog.show_menu(
                    ui,
                    menu,
                    &MenuContext {
                        session: &session,
                        selected: None,
                        layout: &layout,
                        studio: false,
                        clipboard: &crate::edit_clipboard::Clipboard::default(),
                        source_clipboard: None,
                    },
                    &mut intents,
                );
            });
        });
        fn text(shape: &egui::epaint::Shape, value: &str) -> bool {
            match shape {
                egui::epaint::Shape::Text(t) => t.galley.text() == value,
                egui::epaint::Shape::Vec(v) => v.iter().any(|s| text(s, value)),
                _ => false,
            }
        }
        assert!(output.shapes.iter().any(|s| text(&s.shape, "F9 / F10")));
        assert!(!output.shapes.iter().any(|s| text(&s.shape, "Ctrl+C")));
    }
    #[test]
    fn top_menu_transform_and_order_use_group_child_owner_and_ancestor_locks() {
        let mut session = Session::new();
        let root = session.preview();
        let group_item = session
            .apply(Command::AddGroup {
                scene: root,
                name: "Group".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let model = session.snapshot();
        let group_source = model
            .source(model.item_container(group_item).unwrap().1.source())
            .unwrap();
        let skylive_core::SourceKind::Group { group } = group_source.kind() else {
            panic!("real group")
        };
        let owner = group.scene();
        let child = session
            .apply(Command::AddColorSource {
                scene: owner,
                name: "Child".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::AddColorSource {
                scene: owner,
                name: "Sibling".into(),
                rgba: [0, 0, 0, 255],
            })
            .unwrap();
        assert!(
            matches!(action_intent("actionEditTransform",&session,Some(child),false),Some(Intent::OpenDialog(Dialog::Transform{scene,..}))if scene==owner)
        );
        let Some(Intent::Command(command @ Command::TransformItem { scene, .. })) =
            action_intent("actionRotate90CW", &session, Some(child), false)
        else {
            panic!("child transform command")
        };
        assert_eq!(scene, owner);
        let before = session.snapshot();
        session.apply(command).unwrap();
        assert_eq!(
            session
                .snapshot()
                .item_container(child)
                .unwrap()
                .1
                .transform()
                .rotation_degrees,
            90.0
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().scene_container(owner),
            before.scene_container(owner)
        );
        let Some(Intent::Command(command @ Command::MoveItem { scene, .. })) =
            action_intent("actionMoveUp", &session, Some(child), false)
        else {
            panic!("child order command")
        };
        assert_eq!(scene, owner);
        session.apply(command).unwrap();
        assert_eq!(
            session.snapshot().scene_container(owner).unwrap().items()[1].id(),
            child
        );
        session
            .apply(Command::SetLocked {
                scene: root,
                item: group_item,
                locked: true,
            })
            .unwrap();
        for id in ["actionEditTransform", "actionRotate90CW", "actionMoveDown"] {
            assert!(action_intent(id, &session, Some(child), false).is_none());
        }
        session
            .apply(Command::SetLocked {
                scene: root,
                item: group_item,
                locked: false,
            })
            .unwrap();
        let another = session
            .apply(Command::AddScene {
                name: "Another root".into(),
            })
            .unwrap()
            .created
            .unwrap();
        session.select_preview(another).unwrap();
        assert!(action_intent("actionEditTransform", &session, Some(child), false).is_none());
    }
}
