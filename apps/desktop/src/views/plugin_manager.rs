//! Compiled module browser: compact virtualized rows and one selected detail.
//! No network, plugin binaries or automatic external navigation.
use super::Intent;
use eframe::egui;
use skylive_core::{PluginDescriptor, PluginOrigin, StudioSnapshot};

fn description(ui: &egui::Ui, plugin: &PluginDescriptor) -> String {
    crate::i18n::tr_key_or(ui, plugin.description_key, plugin.description)
}
fn scope(ui: &egui::Ui, plugin: &PluginDescriptor) -> String {
    if plugin.id == "skylive/craft-video-effects" {
        crate::i18n::tr_key_or(ui,"SkyLive.Craft.PluginScopeGpu",plugin.port_scope)
    } else {
        crate::i18n::tr(ui,plugin.port_scope)
    }
}
fn state(ui: &egui::Ui, plugin: &PluginDescriptor) -> String {
    if plugin.implemented && !super::plugin_sources::module_on_platform(ui,plugin.id) {
        return crate::i18n::tr(ui,if plugin.origin==PluginOrigin::BuiltIn {
            "Built-in · unavailable on this platform"
        }else {"Unavailable on this platform"});
    }
    crate::i18n::tr(
        ui,
        if plugin.origin == PluginOrigin::BuiltIn {
            if plugin.implemented {
                "Built-in · available"
            } else {
                "Built-in reference · not yet implemented"
            }
        } else if plugin.implemented {
            "Rust port available"
        } else {
            "Reference only · not ported"
        },
    )
}
fn details(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    plugin: &PluginDescriptor,
    identity: (u64, u64),
    intents: &mut Vec<Intent>,
) {
    ui.strong(plugin.name);
    ui.label(plugin.id);
    ui.label(state(ui, plugin));
    ui.separator();
    let entries = skylive_core::plugin_entry_points(plugin.id);
    if entries.is_empty() {
        ui.label(crate::i18n::tr(ui,"This reference has no compiled Rust operations. It cannot add sources, filters or docks."));
    } else {
        for entry in entries {
            super::plugin_routes::show(ui, model, plugin.id, entry, identity, intents);
            ui.separator();
        }
    }
    ui.label(description(ui, plugin));
    ui.label(scope(ui, plugin));
    ui.add_space(8.0);
    ui.label(crate::i18n::tr(ui, "Source"));
    ui.add(egui::Label::new(plugin.source_url).wrap().selectable(true))
        .on_hover_text(crate::hints::text(ui, "Source URL"));
}
pub fn show(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    query: &mut String,
    identity: (u64, u64),
    intents: &mut Vec<Intent>,
) {
    let label = ui.label(crate::i18n::tr(ui, "Search modules"));
    ui.add(
        egui::TextEdit::singleline(query)
            .char_limit(256)
            .id_salt("plugin-search")
            .desired_width(ui.available_width()),
    )
    .labelled_by(label.id)
    .on_hover_text(crate::hints::text(ui, "plugin-search"))
    .on_disabled_hover_text(crate::hints::text(ui, "plugin-search"));
    let catalog = skylive_core::plugin_catalog();
    let built_in = catalog
        .iter()
        .filter(|plugin| plugin.origin == PluginOrigin::BuiltIn)
        .count();
    let third_party = catalog
        .iter()
        .filter(|plugin| plugin.origin == PluginOrigin::ThirdParty)
        .count();
    ui.label(crate::i18n::tf(
        ui,
        "Modules: {built_in} built-in · {} third-party",
        &[
            ("{built_in}", built_in.to_string()),
            ("{}", third_party.to_string()),
        ],
    ));
    let available = catalog.iter().filter(|plugin| plugin.implemented).count();
    ui.label(crate::i18n::tf(
        ui,
        "Available Rust modules: {}",
        &[("{}", available.to_string())],
    ));
    if catalog.len() > built_in + third_party {
        ui.label(crate::i18n::tf(
            ui,
            "Bundled modules: {}",
            &[("{}", (catalog.len() - built_in - third_party).to_string())],
        ));
    }
    ui.label(crate::i18n::tr(ui,"Third-party modules are off by default. Only available Rust ports can be enabled; OBS plugin binaries cannot be loaded here."));
    let needle = query.to_lowercase();
    let mut filtered: Vec<_> = catalog
        .iter()
        .filter(|plugin| {
            needle.is_empty()
                || plugin.id.to_lowercase().contains(&needle)
                || plugin.name.to_lowercase().contains(&needle)
                || plugin.description.to_lowercase().contains(&needle)
                || plugin.port_scope.to_lowercase().contains(&needle)
                || description(ui, plugin).to_lowercase().contains(&needle)
                || scope(ui,plugin).to_lowercase().contains(&needle)
        })
        .collect();
    // The first visible rows should be usable modules, while every reference
    // remains searchable in the same catalog. Sorting is presentation only.
    filtered.sort_by_key(|plugin| (!plugin.implemented, plugin.origin != PluginOrigin::Bundled));
    if filtered.is_empty() {
        ui.label(crate::i18n::tr(ui, "No modules match this search."));
        return;
    }
    let selection_id = egui::Id::new("plugin-manager-selected");
    let mut selected = ui
        .data(|data| data.get_temp::<String>(selection_id))
        .filter(|id| filtered.iter().any(|plugin| plugin.id == id))
        .unwrap_or_else(|| filtered[0].id.to_owned());
    let wide = ui.available_width() >= 620.0;
    let list_height = if wide { 260.0 } else { 176.0 };
    let list_width = if wide {
        ui.available_width() * 0.53
    } else {
        ui.available_width()
    };

    if wide {
        let (area, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), list_height),
            egui::Sense::hover(),
        );
        let left = egui::Rect::from_min_max(
            area.min,
            egui::pos2(area.left() + list_width, area.bottom()),
        );
        let right = egui::Rect::from_min_max(egui::pos2(left.right() + 16.0, area.top()), area.max);
        ui.painter().line_segment(
            [
                egui::pos2(left.right() + 8.0, area.top()),
                egui::pos2(left.right() + 8.0, area.bottom()),
            ],
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
        let mut rows = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("plugin-list-pane")
                .max_rect(left)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        rows.set_clip_rect(left.intersect(ui.clip_rect()));
        rows.set_width(left.width());
        list(
            &mut rows,
            model,
            &filtered,
            &mut selected,
            intents,
            list_height,
        );
        let mut detail = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("plugin-detail-pane")
                .max_rect(right)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        detail.set_clip_rect(right.intersect(ui.clip_rect()));
        detail.set_width(right.width());
        egui::ScrollArea::vertical()
            .id_salt("plugin-details")
            .max_height(list_height)
            .show(&mut detail, |ui| {
                if let Some(plugin) = filtered.iter().find(|plugin| plugin.id == selected) {
                    details(ui, model, plugin, identity, intents);
                }
            });
    } else {
        list(ui, model, &filtered, &mut selected, intents, list_height);
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("plugin-details")
            .max_height(160.0)
            .show(ui, |ui| {
                if let Some(plugin) = filtered.iter().find(|plugin| plugin.id == selected) {
                    details(ui, model, plugin, identity, intents);
                }
            });
    }
    ui.data_mut(|data| data.insert_temp(selection_id, selected));
}

fn list(
    ui: &mut egui::Ui,
    model: &StudioSnapshot,
    filtered: &[&PluginDescriptor],
    selected: &mut String,
    intents: &mut Vec<Intent>,
    list_height: f32,
) {
    let row_height = ui.spacing().interact_size.y.max(28.0);
    egui::ScrollArea::vertical().id_salt("plugin-catalog").max_height(list_height).min_scrolled_height(112.0).show_rows(ui,row_height,filtered.len(),|ui,range|{
            for index in range {let plugin=filtered[index];ui.push_id(("plugin",plugin.id),|ui|{
                ui.horizontal(|ui|{
                    if !plugin.implemented {
                        ui.label(crate::i18n::tr(ui,"Read-only"))
                            .on_hover_text(crate::i18n::tr(ui,"This module has not been ported to Rust."));
                    } else {
                        let mut enabled=model.preferences().plugin_enabled(plugin.id);
                        let toggle=plugin.origin!=PluginOrigin::BuiltIn;
                        if ui.add_enabled(toggle,egui::Checkbox::new(&mut enabled,crate::i18n::tr(ui,"Enabled")))
                            .on_hover_text(crate::hints::text(ui,"Enabled"))
                            .on_disabled_hover_text(crate::i18n::tr(ui,"Built-in modules are managed by the application.")).changed(){intents.push(Intent::SetPluginEnabled{plugin:plugin.id.into(),enabled});}
                    }
                    let selected_now=selected.as_str()==plugin.id;
                    let remaining=ui.available_width();
                    if ui.add_sized([remaining,row_height],egui::Button::new(plugin.name).selected(selected_now).truncate())
                        .on_hover_text(crate::i18n::tr(ui,"Select this module to read its description and supported features.")).clicked(){*selected=plugin.id.to_owned();}
                });
            });}
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(
        context: &egui::Context,
        model: &StudioSnapshot,
        width: f32,
        query: &mut String,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Vec<Intent>) {
        crate::i18n::install(
            context,
            crate::i18n::english().unwrap(),
            std::path::Path::new("headless-translations.csv"),
        );
        let mut intents = vec![];
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 640.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .show(ctx, |ui| show(ui, model, query, (1, 1), &mut intents));
            },
        );
        (output, intents)
    }
    fn text(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().find_map(|shape| self::text(shape, label))
            }
            _ => None,
        }
    }
    #[test]
    fn searchable_list_exposes_only_selected_details_and_real_available_toggle() {
        for width in [340.0, 580.0, 900.0] {
            let context = egui::Context::default();
            let session = skylive_core::Session::new();
            let model = session.snapshot();
            let mut query = skylive_core::AUDIO_PAN_PLUGIN.to_owned();
            frame(&context, &model, width, &mut query, vec![]);
            let (output, _) = frame(&context, &model, width, &mut query, vec![]);
            let toggle = output
                .shapes
                .iter()
                .find_map(|shape| {
                    text(&shape.shape, "Enabled")
                        .filter(|rect| shape.clip_rect.contains_rect(*rect))
                })
                .expect("module toggle stays visible");
            assert!(
                output
                    .shapes
                    .iter()
                    .any(|shape| text(&shape.shape, "Rust port available").is_some())
            );
            let pos = toggle.center();
            frame(
                &context,
                &model,
                width,
                &mut query,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            let (_, intents) = frame(
                &context,
                &model,
                width,
                &mut query,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert!(intents.iter().any(|intent|matches!(intent,Intent::SetPluginEnabled{plugin,enabled:true} if plugin==skylive_core::AUDIO_PAN_PLUGIN)));
            query = "norihiro/obs-mute-filter".into();
            frame(&context, &model, width, &mut query, vec![]);
            let (output, _) = frame(&context, &model, width, &mut query, vec![]);
            assert!(
                output
                    .shapes
                    .iter()
                    .any(|shape| text(&shape.shape, "Reference only · not ported").is_some())
            );
            assert!(
                output
                    .shapes
                    .iter()
                    .all(|shape| text(&shape.shape, "Enabled").is_none())
            );
            assert!(
                output
                    .shapes
                    .iter()
                    .any(|shape| text(&shape.shape, "Read-only").is_some())
            );
            let (_, intents) = frame(&context, &model, width, &mut query, vec![]);
            assert!(intents.is_empty());
        }
    }
    #[test]
    fn full_catalog_is_virtualized_and_does_not_repeat_every_description() {
        let context = egui::Context::default();
        let model = skylive_core::Session::new().snapshot();
        let mut query = String::new();
        frame(&context, &model, 900.0, &mut query, vec![]);
        let (output, _) = frame(&context, &model, 900.0, &mut query, vec![]);
        let catalog = crate::i18n::english().unwrap();
        let descriptions: Vec<_> = skylive_core::plugin_catalog()
            .iter()
            .filter(|plugin| {
                let english = catalog.label(plugin.description_key);
                output
                    .shapes
                    .iter()
                    .any(|shape| text(&shape.shape, &english).is_some())
            })
            .map(|plugin| plugin.id.to_owned())
            .collect();
        let selected = context.data(|data| {
            data.get_temp::<String>(egui::Id::new("plugin-manager-selected"))
                .unwrap()
        });
        assert_eq!(selected, skylive_core::CRAFT_AUDIO_PLUGIN);
        assert_eq!(descriptions, vec![selected]);
        let names = skylive_core::plugin_catalog()
            .iter()
            .filter(|plugin| {
                output
                    .shapes
                    .iter()
                    .any(|shape| text(&shape.shape, plugin.name).is_some())
            })
            .count();
        assert!(
            names < 20,
            "only visible list rows and selected details are painted"
        );
    }
}
