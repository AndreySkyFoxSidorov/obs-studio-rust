//! Read-only load receipt; no authored scene settings or hidden edits.
use eframe::egui;
pub(super) fn show(ui: &mut egui::Ui, report: &skylive_host::storage::MigrationReport) {
    ui.label(crate::i18n::tr(ui,"Scene filters now apply both to nested references and when a scene is shown directly. Older versions skipped filters on the direct scene output."));
    ui.label(crate::i18n::tr(ui,"Filter identities, names, settings and order were preserved. Review these scenes and save explicitly to keep the new format."));
    ui.separator();
    for scene in &report.scene_source_filters {
        ui.add(
            egui::Label::new(crate::i18n::tf(
                ui,
                "{name} · scene {scene} · source {source} · filters {filters}",
                &[
                    ("{name}", scene.name.clone()),
                    ("{scene}", scene.scene.get().to_string()),
                    ("{source}", scene.source.get().to_string()),
                    ("{filters}", scene.filter_count.to_string()),
                ],
            ))
            .wrap(),
        );
    }
}
