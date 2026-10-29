//! Desktop composition root. Views adapt input to the shared headless API.
mod app;
mod projectors;
mod browser_interaction;
mod edit_clipboard;
mod edit_clipboard_shortcuts;
mod source_clipboard;
mod hints;
mod i18n;
mod io;
mod image_watch;
mod slideshow_watch;
mod startup;
mod native_browse;
mod platform;
mod preview;
mod filter_preview;
mod preview_interaction;
mod preview_display;
mod shortcuts;
mod status;
mod statistics;
mod theme;
mod typography;
mod views;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "SkyLive",
        options,
        Box::new(|cc| Ok(Box::new(app::StudioApp::new(cc)?))),
    )
}
