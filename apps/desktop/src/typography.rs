//! Fixed, licensed fallback data; no system-font IO on the UI thread.
//! Glyph availability is not a guarantee of complex-script shaping or bidi.
use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

pub fn install(context: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    // Preserve the established UI face. These only supply absent glyphs.
    for (name, bytes) in [
        (
            "skylive-noto-sans",
            include_bytes!("../../../assets/fonts/NotoSans-Regular.ttf").as_slice(),
        ),
        (
            "skylive-noto-symbols",
            include_bytes!("../../../assets/fonts/NotoSansSymbols-Regular.ttf").as_slice(),
        ),
        (
            "skylive-noto-arabic",
            include_bytes!("../../../assets/fonts/NotoSansArabic-Regular.ttf").as_slice(),
        ),
        (
            "skylive-noto-hebrew",
            include_bytes!("../../../assets/fonts/NotoSansHebrew-Regular.ttf").as_slice(),
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), FontData::from_static(bytes).into());
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.into());
        }
    }
    context.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_ui_font_cache_contains_localized_letters_accents_and_arrow_hints() {
        let context = egui::Context::default();
        install(&context);
        let _ = context.run(egui::RawInput::default(), |context| {
            context.fonts(|fonts| {
                for family in [FontFamily::Proportional, FontFamily::Monospace] {
                    let id = egui::FontId::new(13.0, family);
                    for ch in
                        "English Español Русский Українська Deutsch e\u{301} ← → العربية עברית"
                            .chars()
                            .filter(|ch| !ch.is_whitespace())
                    {
                        assert!(fonts.has_glyph(&id, ch), "missing UI glyph {ch:?}");
                    }
                    let galley = fonts.layout_no_wrap(
                        "Українська → Español".into(),
                        id,
                        egui::Color32::WHITE,
                    );
                    assert!(galley.size().x > 50.0);
                    assert!(!galley.rows[0].visuals.mesh.vertices.is_empty());
                }
            });
        });
    }
}
