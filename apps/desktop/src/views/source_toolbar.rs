//! Selected-source presentation. Dialog owners and media workers keep authority.
use eframe::egui;
use skylive_core::{Id, SourceKind, StudioSnapshot};
use super::{Intent, dialogs::Dialog, media_controls::MediaTransportView};

const SOURCE_BAR: &str = "source-context-bar-visible";
const LISTBOX_BARS: &str = "listbox-toolbars-visible";
fn visible(ctx: &egui::Context, id: &str) -> bool {
    ctx.data(|data| data.get_temp::<bool>(egui::Id::new(id))).unwrap_or(true)
}
fn toggle(ctx: &egui::Context, id: &str) {
    let next = !visible(ctx, id);
    ctx.data_mut(|data| data.insert_temp(egui::Id::new(id), next));
}
pub fn source_bar_visible(ctx: &egui::Context) -> bool { visible(ctx, SOURCE_BAR) }
pub fn listbox_bars_visible(ctx: &egui::Context) -> bool { visible(ctx, LISTBOX_BARS) }
pub fn toggle_source_bar(ctx: &egui::Context) { toggle(ctx, SOURCE_BAR); }
pub fn toggle_listbox_bars(ctx: &egui::Context) { toggle(ctx, LISTBOX_BARS); }

pub fn show(
    ui: &mut egui::Ui, model: &StudioSnapshot, scene: Id, selected: Option<Id>,
    session: u64, media: Option<MediaTransportView<'_>>, intents: &mut Vec<Intent>,
) {
    let entry = selected.and_then(|id| model.item_container(id))
        .filter(|(owner, _)| model.root_scene_for_container(*owner) == Some(scene));
    let source = entry.and_then(|(_, item)| model.source(item.source()));
    let Some(source) = source else {
        ui.label(crate::i18n::tr(ui, "Select a source to show its properties, filters and playback controls."));
        return;
    };
    let visual = model.source_video_dimensions(source.kind())
        .is_ok_and(|size| size.is_some());
    let audio = source.kind().has_audio();
    ui.horizontal_wrapped(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width().min(220.0), 24.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                super::source_icons::show(ui, source.kind());
                ui.add(egui::Label::new(source.name()).truncate())
                    .on_hover_text(source.name())
                    .context_menu(|ui| {
                        if let Some((owner, item)) = entry {
                            super::docks::source_actions(ui, model, owner, Some(item.id()), session, intents);
                        }
                    });
            },
        );
        if action(ui, "Properties", !matches!(source.kind(), SourceKind::Group { .. })) {
            if let Some(intent) = super::docks::source_properties_intent(source) {
                intents.push(intent);
            }
        }
        if visual && action(ui, "Filters", true) {
            match super::video_filters::VideoFilterDraft::capture(session, model, source.id()) {
                Ok(draft) => intents.push(Intent::OpenDialog(Dialog::VideoFilters(draft))),
                Err(error) => intents.push(Intent::DialogError(error)),
            }
        }
        if matches!(source.kind(),SourceKind::Browser {..}) && ui.add_enabled(skylive_host::browser::BrowserOwner::available(),egui::Button::new(crate::i18n::tr(ui,"Interact")))
            .on_hover_text(crate::i18n::tr(ui,"Open the real browser page in a separate interaction window."))
            .on_disabled_hover_text(crate::i18n::tr(ui,"Browser Interaction requires Windows x64 and the matching external CEF runtime."))
            .clicked(){intents.push(Intent::InteractBrowser {session,revision:model.revision(),source:source.id()});}
        super::source_playback_hotkeys::open_button(ui,session,model,source.id(),intents);
        if audio && action(ui, "Audio Filters", true) {
            intents.push(Intent::OpenDialog(Dialog::audio_filters(source)));
        }
    });
    super::slideshow_controls::show(ui,session,model,source.id(),intents);
    if matches!(source.kind(), SourceKind::MediaSource { .. }) {
        if let Some(view) = media {
            super::media_controls::show_compact(ui, session, model, source.id(), view, intents);
        }
    }
}
fn action(ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
    ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr(ui, label)).wrap())
        .on_hover_text(crate::hints::text(ui, label))
        .on_disabled_hover_text(crate::hints::text(ui, label))
        .clicked()
}
