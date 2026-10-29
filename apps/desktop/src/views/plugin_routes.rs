//! Window-local navigation to existing canonical filter/notes editors.
use super::Intent;
use eframe::egui;
use skylive_core::{Id, PluginEntryPoint, PluginOperation, StudioSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginTarget {
    Source(Id),
    Scene(Id),
}
#[derive(Clone, Debug)]
pub struct PluginEditorRequest {
    pub session: u64,
    pub revision: u64,
    pub manager: u64,
    pub plugin: String,
    pub operation: PluginOperation,
    pub target: PluginTarget,
    pub filter: Option<skylive_core::FilterFactory>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginEditorToken {
    pub session: u64,
    pub revision: u64,
    pub generation: u64,
}

pub(crate) fn filter_capacity(model:&StudioSnapshot,operation:PluginOperation,target:PluginTarget)->bool {
    match (operation,target) {
        (PluginOperation::AudioFilters,PluginTarget::Source(id))=>model.source(id)
            .is_some_and(|source|source.audio_filters().len()<skylive_core::MAX_AUDIO_FILTERS),
        (PluginOperation::VideoFilters,PluginTarget::Source(id))=>model.source(id)
            .is_some_and(|source|source.video_filters().len()<skylive_core::MAX_VIDEO_FILTERS),
        (PluginOperation::VideoFilters,PluginTarget::Scene(id))=>model.scene(id).is_some()
            &&model.scene_source(id).is_none_or(|source|source.video_filters().len()<skylive_core::MAX_VIDEO_FILTERS),
        _=>false,
    }
}

pub fn compatible(
    model: &StudioSnapshot,
    operation: PluginOperation,
    target: PluginTarget,
) -> bool {
    match (operation, target) {
        (PluginOperation::AudioFilters, PluginTarget::Source(id)) => model
            .source(id)
            .is_some_and(|source| source.kind().has_audio()),
        (PluginOperation::VideoFilters, PluginTarget::Source(id)) => {
            model.source(id).is_some_and(|source| {
                model
                    .source_video_dimensions(source.kind())
                    .is_ok_and(|size| size.is_some())
            })
        }
        (PluginOperation::VideoFilters | PluginOperation::SceneNotes | PluginOperation::Sources, PluginTarget::Scene(id)) => {
            model.scene(id).is_some()
        }
        _ => false,
    }
}

fn choices(model: &StudioSnapshot, operation: PluginOperation) -> Vec<PluginTarget> {
    let mut result = Vec::new();
    if operation != PluginOperation::SceneNotes && operation != PluginOperation::Sources {
        for source in model.sources() {
            let matches = match operation {
                PluginOperation::AudioFilters => source.kind().has_audio(),
                PluginOperation::VideoFilters => model
                    .source_video_dimensions(source.kind())
                    .is_ok_and(|size| size.is_some()),
                PluginOperation::SceneNotes | PluginOperation::Sources | PluginOperation::Encoders | PluginOperation::Outputs => false,
            };
            if matches {
                result.push(PluginTarget::Source(source.id()));
            }
        }
    }
    if operation != PluginOperation::AudioFilters {
        result.extend(
            model
                .scenes()
                .iter()
                .map(|scene| PluginTarget::Scene(scene.id())),
        );
    }
    result
}

fn target_label(ui: &egui::Ui, model: &StudioSnapshot, target: PluginTarget) -> String {
    match target {
        PluginTarget::Source(id) => crate::i18n::tf(
            ui,
            "Source: {name}",
            &[(
                "{name}",
                model
                    .source(id)
                    .map_or_else(String::new, |source| source.name().into()),
            )],
        ),
        PluginTarget::Scene(id) => crate::i18n::tf(
            ui,
            "Scene: {name}",
            &[(
                "{name}",
                model
                    .scene(id)
                    .map_or_else(String::new, |scene| scene.name().into()),
            )],
        ),
    }
}

pub fn show(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    plugin: &str,
    entry: &PluginEntryPoint,
    identity: (u64, u64),
    intents: &mut Vec<Intent>,
) {
    if entry.operation==PluginOperation::Outputs {
        ui.strong(crate::i18n::tr(ui,"NDI Outputs"));ui.label(crate::i18n::tr(ui,"Program sends video and optional Program audio. Preview sends base-canvas video only. Enabling the module never starts a sender."));
        if ui.add_enabled(model.preferences().plugin_enabled(plugin),egui::Button::new(crate::i18n::tr(ui,"NDI Output Settings"))).on_hover_text(crate::i18n::tr(ui,"Configure and explicitly start or stop Program and Preview NDI senders.")).on_disabled_hover_text(crate::i18n::tr(ui,"Enable the DistroAV module before starting NDI output")).clicked(){intents.push(Intent::OpenNdiOutputs);}return;
    }
    if entry.operation==PluginOperation::Encoders {
        ui.strong(crate::i18n::tr(ui,"NVIDIA NVENC H.264"));
        ui.label(crate::i18n::tr(ui,"Choose NVIDIA explicitly in Output or Stream settings. Probe reads the installed driver; opening this module never loads it."));
        if ui.button(crate::i18n::tr(ui,"Open Output Encoder Settings")).on_hover_text(crate::i18n::tr(ui,"Configure the next recording and Replay Buffer video encoder.")).clicked(){intents.push(Intent::OpenDialog(super::dialogs::Dialog::recording(model)));}
        if ui.button(crate::i18n::tr(ui,"Open Stream Encoder Settings")).on_hover_text(crate::i18n::tr(ui,"Configure the next streaming video encoder.")).clicked(){intents.push(Intent::OpenDialog(super::dialogs::Dialog::streaming(model)));}
        return;
    }
    if entry.operation==PluginOperation::Sources {
        super::plugin_sources::show(ui,model,plugin,identity,intents);
        return;
    }
    let title = match entry.operation {
        PluginOperation::AudioFilters => "Audio Filters",
        PluginOperation::VideoFilters => "Video Filters",
        PluginOperation::SceneNotes => "Scene Notes",
        PluginOperation::Sources => "Sources",
        PluginOperation::Encoders=>"Video Encoder",
        PluginOperation::Outputs=>"NDI Outputs",
    };
    ui.strong(crate::i18n::tr(ui, title));
    let targets = choices(model, entry.operation);
    let selection_id = ui
        .id()
        .with(("plugin-operation-target", plugin, entry.operation));
    let selected = ui
        .data(|data| data.get_temp::<PluginTarget>(selection_id))
        .filter(|target| targets.contains(target))
        .or_else(|| targets.first().copied());
    if let Some(mut selected) = selected {
        let label = ui.label(crate::i18n::tr(ui, "Target"));
        let selected_label = target_label(ui, model, selected);
        egui::ComboBox::from_id_salt(("plugin-target", plugin, entry.operation))
            .width(ui.available_width())
            .height(180.0)
            .truncate()
            .selected_text(selected_label.clone())
            .show_ui(ui, |ui| {
                for target in targets {
                    let name = target_label(ui, model, target);
                    ui.selectable_value(&mut selected, target, name.clone())
                        .on_hover_text(name);
                }
            })
            .response
            .labelled_by(label.id)
            .on_hover_text(selected_label)
            .on_disabled_hover_text(crate::i18n::tr(
                ui,
                "Choose the existing source or scene to configure.",
            ));
        ui.data_mut(|data| data.insert_temp(selection_id, selected));
        let active = model.preferences().plugin_enabled(plugin);
        let button = match entry.operation {
            PluginOperation::AudioFilters => "Open Audio Filters",
            PluginOperation::VideoFilters => "Open Video Filters",
            PluginOperation::SceneNotes => "Open Scene Notes",
            PluginOperation::Sources => "Sources",
            PluginOperation::Encoders=>"Video Encoder",
        PluginOperation::Outputs=>"NDI Outputs",
        };
        if !active {
            ui.label(crate::i18n::tr(
                ui,
                "Enable this module before opening its editor.",
            ));
        }
        if ui
            .add_enabled(active, egui::Button::new(crate::i18n::tr(ui, button)))
            .on_hover_text(crate::i18n::tr(
                ui,
                "Choose the existing source or scene to configure.",
            ))
            .on_disabled_hover_text(crate::i18n::tr(
                ui,
                "Enable this module before opening its editor.",
            ))
            .clicked()
        {
            intents.push(Intent::OpenPluginEditor(PluginEditorRequest {
                session: identity.0,
                revision: model.revision(),
                manager: identity.1,
                plugin: plugin.into(),
                operation: entry.operation,
                target: selected,
                filter: None,
            }));
        }
        if matches!(entry.operation,PluginOperation::AudioFilters|PluginOperation::VideoFilters) {
            ui.separator();
            for factory in skylive_core::plugin_filter_factories(plugin)
                .filter(|factory|factory.operation()==entry.operation) {
                let capacity=filter_capacity(model,entry.operation,selected);
                if ui.add_enabled(active&&capacity,egui::Button::new(crate::i18n::tr(ui,factory.label())).wrap())
                    .on_hover_text(crate::i18n::tr(ui,factory.hint()))
                    .on_disabled_hover_text(crate::i18n::tr(ui,if !active{"Enable this module before opening its editor."}
                        else if entry.operation==PluginOperation::AudioFilters{"A source supports at most eight audio filters. Remove a filter before adding another."}
                        else{"A source supports at most 32 video filters."}))
                    .clicked() {
                    intents.push(Intent::OpenPluginEditor(PluginEditorRequest {
                        session:identity.0,revision:model.revision(),manager:identity.1,
                        plugin:plugin.into(),operation:entry.operation,target:selected,filter:Some(factory),
                    }));
                }
            }
        }
    } else {
        ui.label(crate::i18n::tr(
            ui,
            "No compatible target. Add an audio or video source, or create a scene first.",
        ));
    }
    if !matches!(entry.operation,PluginOperation::AudioFilters|PluginOperation::VideoFilters) {
        for feature in entry.features { ui.label(crate::i18n::tr(ui, feature)); }
    }
    if entry.operation != PluginOperation::SceneNotes {
        ui.label(crate::i18n::tr(ui, "Choose an effect to add it to a draft, or open the existing chain. Apply saves changes; Cancel discards the draft. Enabling a module does not add filters automatically."));
    }
}

#[cfg(test)]
#[path = "plugin_routes_tests.rs"]
mod tests;
