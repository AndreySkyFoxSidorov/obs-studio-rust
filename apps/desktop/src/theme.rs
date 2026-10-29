//! Semantic palettes adapted from the supplied OBS Yami, Grey and Light themes.
use eframe::egui::{self, Color32, FontId, Stroke, TextStyle};
use skylive_core::{Appearance, InterfaceTheme};

pub const WINDOW_BACKGROUND: Color32 = Color32::from_rgb(0x1d, 0x1f, 0x26);
pub const DOCK_BACKGROUND: Color32 = Color32::from_rgb(0x27, 0x2a, 0x33);
pub const PREVIEW_BACKGROUND: Color32 = Color32::from_rgb(0x13, 0x14, 0x1a);
const PRIMARY: Color32 = Color32::from_rgb(0x28, 0x4c, 0xb8);
const HIGHLIGHT: Color32 = Color32::from_rgb(0x71, 0x8c, 0xdc);
pub const PREVIEW_SELECTION: Color32 = Color32::from_rgb(255, 0, 0);
pub const PREVIEW_CROP: Color32 = Color32::from_rgb(0, 255, 0);
pub const BORDER: Color32 = Color32::from_rgb(0x3c, 0x40, 0x4d);

pub fn install_accessibility(context:&egui::Context,value:skylive_core::Accessibility) {
    context.data_mut(|data|data.insert_temp(egui::Id::new("interface-accessibility"),value));
}
pub fn accessibility(context:&egui::Context)->skylive_core::Accessibility {
    context.data(|data|data.get_temp::<skylive_core::Accessibility>(egui::Id::new("interface-accessibility"))).unwrap_or_default()
}
pub fn interface_color(ui:&egui::Ui,role:skylive_core::InterfaceColor)->Color32 {
    let value=accessibility(ui.ctx());
    if !value.override_colors() {
        match role {skylive_core::InterfaceColor::Selection=>return PREVIEW_SELECTION,skylive_core::InterfaceColor::Crop=>return PREVIEW_CROP,_=>{}}
    }
    let [r,g,b]=value.effective_color(role);Color32::from_rgb(r,g,b)
}

pub fn dock_background(ui: &egui::Ui) -> Color32 {
    ui.visuals().faint_bg_color
}
pub fn border(ui: &egui::Ui) -> Color32 {
    ui.visuals().window_stroke.color
}
pub fn preview_background(ui: &egui::Ui) -> Color32 {
    ui.visuals().code_bg_color
}

/// Per-frontend derived state; no authored preference copy is ever mutated here.
#[derive(Default)]
pub struct Runtime {
    applied: Appearance,
}
impl Runtime {
    pub fn sync(&mut self, context: &egui::Context, value: Appearance) {
        if self.applied.theme() != value.theme() {
            apply_theme(context, value.theme());
        }
        if self.applied.scale_percent() != value.scale_percent() {
            context.set_zoom_factor(f32::from(value.scale_percent()) / 100.0);
        }
        self.applied = value;
    }
}
pub fn install(context: &egui::Context) {
    let id = egui::Id::new("skylive-fonts-installed");
    if !context.data(|data| data.get_temp::<bool>(id).unwrap_or(false)) {
        crate::typography::install(context);
        context.data_mut(|data| data.insert_temp(id, true));
    }
    context.options_mut(|options| options.zoom_with_keyboard = false);
    apply_theme(context, InterfaceTheme::Yami);
}
fn apply_theme(context: &egui::Context, theme: InterfaceTheme) {
    context.set_style_of(
        egui::Theme::Dark,
        variant_style(theme),
    );
    context.set_style_of(egui::Theme::Light, style(true, false));
    let (preference, native) = match theme {
        InterfaceTheme::Yami | InterfaceTheme::Dark | InterfaceTheme::Acri | InterfaceTheme::Classic | InterfaceTheme::Rachni => {
            (egui::ThemePreference::Dark, egui::SystemTheme::Dark)
        }
        InterfaceTheme::Light => (egui::ThemePreference::Light, egui::SystemTheme::Light),
        InterfaceTheme::System => (
            egui::ThemePreference::System,
            egui::SystemTheme::SystemDefault,
        ),
    };
    context.set_theme(preference);
    context.send_viewport_cmd(egui::ViewportCommand::SetTheme(native));
}
fn variant_style(theme: InterfaceTheme) -> egui::Style {
    let mut style = style(false, theme == InterfaceTheme::Dark);
    // Exact semantic tokens from the supplied32.2.2 Yami variant files.
    let rgb = Color32::from_rgb;
    let (window, dock, preview, input, button, hover, pressed, border, primary, accent, selected, radius) = match theme {
        InterfaceTheme::Acri => (rgb(16,16,16),rgb(24,24,25),rgb(9,9,9),rgb(40,40,42),rgb(22,36,88),rgb(42,58,117),rgb(16,16,16),rgb(97,97,97),rgb(19,26,48),HIGHLIGHT,rgb(19,26,48),4),
        InterfaceTheme::Classic => (rgb(58,57,58),rgb(31,30,31),rgb(76,76,76),rgb(76,76,76),rgb(76,76,76),rgb(97,97,97),rgb(31,30,31),rgb(76,76,76),rgb(25,52,76),rgb(33,71,109),rgb(25,52,76),2),
        InterfaceTheme::Rachni => (rgb(49,54,59),rgb(35,38,41),rgb(34,37,40),rgb(35,38,41),rgb(0,188,212),rgb(145,76,103),rgb(89,66,79),rgb(118,121,124),rgb(145,76,103),rgb(240,96,146),rgb(0,188,212),4),
        _ => return style,
    };
    let visuals = &mut style.visuals;
    visuals.panel_fill = window;
    visuals.window_fill = window;
    visuals.faint_bg_color = dock;
    visuals.code_bg_color = preview;
    visuals.extreme_bg_color = input;
    visuals.window_stroke = Stroke::new(1.0, border);
    visuals.window_corner_radius = egui::CornerRadius::same(radius);
    visuals.menu_corner_radius = egui::CornerRadius::same(radius);
    visuals.selection.bg_fill = selected;
    visuals.hyperlink_color = accent;
    visuals.widgets.noninteractive.bg_fill = dock;
    for widget in [&mut visuals.widgets.inactive,&mut visuals.widgets.hovered,&mut visuals.widgets.active,&mut visuals.widgets.open] {
        widget.corner_radius = egui::CornerRadius::same(radius);
        widget.bg_stroke = Stroke::new(1.0, border);
    }
    visuals.widgets.inactive.bg_fill = button;
    visuals.widgets.inactive.weak_bg_fill = button;
    visuals.widgets.hovered.bg_fill = hover;
    visuals.widgets.hovered.weak_bg_fill = hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, accent);
    visuals.widgets.active.bg_fill = pressed;
    visuals.widgets.active.weak_bg_fill = pressed;
    visuals.widgets.open.bg_fill = primary;
    visuals.widgets.open.weak_bg_fill = primary;
    if theme == InterfaceTheme::Classic { style.spacing.interact_size.y = 20.0; }
    style
}
fn style(light: bool, grey: bool) -> egui::Style {
    let mut style = egui::Style::default();
    let (panel, dock, preview, border, hover, text, _muted, primary, accent, error, warn) = if light
    {
        // Yami_Light.ovt: grey7/6/8/5/3, black1/4, primary and blue3.
        (
            Color32::from_gray(211),
            Color32::from_gray(229),
            Color32::from_gray(193),
            Color32::from_gray(193),
            Color32::from_gray(254),
            Color32::from_gray(10),
            Color32::from_gray(100),
            Color32::from_rgb(140, 181, 255),
            PRIMARY,
            Color32::from_rgb(0x7d, 0x12, 0x24),
            Color32::from_rgb(0x49, 0x37, 0x09),
        )
    } else if grey {
        // Yami_Grey.ovt changes the eight greys; all semantic accents inherit Yami.
        (
            Color32::from_gray(33),
            Color32::from_gray(47),
            Color32::from_gray(21),
            Color32::from_gray(67),
            Color32::from_gray(77),
            Color32::WHITE,
            Color32::from_gray(150),
            PRIMARY,
            HIGHLIGHT,
            Color32::from_rgb(255, 144, 162),
            Color32::from_rgb(0xea, 0xbc, 0x48),
        )
    } else {
        (
            WINDOW_BACKGROUND,
            DOCK_BACKGROUND,
            PREVIEW_BACKGROUND,
            BORDER,
            Color32::from_rgb(0x46, 0x4b, 0x59),
            Color32::WHITE,
            Color32::from_gray(150),
            PRIMARY,
            HIGHLIGHT,
            Color32::from_rgb(255, 144, 162),
            Color32::from_rgb(0xea, 0xbc, 0x48),
        )
    };
    let mut visuals = if light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    visuals.panel_fill = panel;
    visuals.window_fill = panel;
    visuals.extreme_bg_color = border;
    visuals.faint_bg_color = dock;
    visuals.code_bg_color = preview;
    visuals.hyperlink_color = accent;
    visuals.selection.bg_fill = primary;
    visuals.selection.stroke = Stroke::new(1.0, text);
    visuals.window_stroke = Stroke::new(1.0, border);
    visuals.window_corner_radius = egui::CornerRadius::same(4);
    visuals.menu_corner_radius = egui::CornerRadius::same(4);
    visuals.error_fg_color = error;
    visuals.warn_fg_color = warn;
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = egui::CornerRadius::same(3);
        widget.fg_stroke = Stroke::new(1.0, text);
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.bg_fill = dock;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    visuals.widgets.inactive.bg_fill = border;
    visuals.widgets.inactive.weak_bg_fill = border;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
    visuals.widgets.hovered.bg_fill = hover;
    visuals.widgets.hovered.weak_bg_fill = hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, accent);
    visuals.widgets.active.bg_fill = panel;
    visuals.widgets.active.weak_bg_fill = panel;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, accent);
    visuals.widgets.open.bg_fill = primary;
    visuals.widgets.open.weak_bg_fill = primary;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, accent);
    style.visuals = visuals;
    for text in [TextStyle::Body, TextStyle::Button] {
        style.text_styles.insert(text, FontId::proportional(13.0));
    }
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(11.0));
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(18.0));
    style.spacing.item_spacing = egui::vec2(6.0, 3.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.interact_size = egui::vec2(40.0, 24.0);
    style.spacing.window_margin = egui::Margin::same(8);
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn yami_dark_profile_survives_native_light_preference() {
        let context = egui::Context::default();
        install(&context);
        let _ = context.run(
            egui::RawInput {
                system_theme: Some(egui::Theme::Light),
                ..Default::default()
            },
            |context| {
                assert_eq!(
                    context.options(|options| options.theme_preference),
                    egui::ThemePreference::Dark
                );
                let style = context.style();
                assert!(style.visuals.dark_mode);
                assert_eq!(style.visuals.panel_fill, WINDOW_BACKGROUND);
                assert_eq!(style.visuals.window_fill, DOCK_BACKGROUND);
                assert_eq!(style.visuals.selection.bg_fill, PRIMARY);
                assert_eq!(style.text_styles[&TextStyle::Button].size, 13.0);
                assert_eq!(style.spacing.interact_size.y, 24.0);
            },
        );
    }
    #[test]
    fn semantic_light_dark_system_and_zoom_are_applied_only_after_preference_changes() {
        let context = egui::Context::default();
        install(&context);
        let mut runtime = Runtime::default();
        let baseline = context.style();
        runtime.sync(&context, Appearance::default());
        assert!(std::sync::Arc::ptr_eq(&baseline, &context.style()));
        runtime.sync(
            &context,
            Appearance::new(InterfaceTheme::Light, 150).unwrap(),
        );
        let _ = context.run(egui::RawInput::default(), |_| {});
        assert!(!context.style().visuals.dark_mode);
        assert_eq!(context.zoom_factor(), 1.5);
        assert_eq!(context.style().visuals.window_fill, Color32::from_gray(229));
        assert_eq!(context.style().visuals.text_color(), Color32::from_gray(10));
        assert_ne!(
            context.style().visuals.error_fg_color,
            egui::Color32::LIGHT_RED
        );
        let light = context.style();
        runtime.sync(
            &context,
            Appearance::new(InterfaceTheme::Light, 150).unwrap(),
        );
        assert!(std::sync::Arc::ptr_eq(&light, &context.style()));
        runtime.sync(&context, Appearance::new(InterfaceTheme::Dark, 75).unwrap());
        let _ = context.run(egui::RawInput::default(), |_| {});
        assert_eq!(context.zoom_factor(), 0.75);
        assert_eq!(context.style().visuals.panel_fill, Color32::from_gray(33));
        runtime.sync(
            &context,
            Appearance::new(InterfaceTheme::System, 100).unwrap(),
        );
        let light = context.style_of(egui::Theme::Light);
        let dark = context.style_of(egui::Theme::Dark);
        for theme in [egui::Theme::Light, egui::Theme::Dark, egui::Theme::Light] {
            let _ = context.run(
                egui::RawInput {
                    system_theme: Some(theme),
                    ..Default::default()
                },
                |ctx| {
                    runtime.sync(ctx, Appearance::new(InterfaceTheme::System, 100).unwrap());
                    assert_eq!(ctx.theme(), theme);
                    assert_eq!(ctx.style().visuals.dark_mode, theme == egui::Theme::Dark);
                },
            );
            assert!(std::sync::Arc::ptr_eq(
                &light,
                &context.style_of(egui::Theme::Light)
            ));
            assert!(std::sync::Arc::ptr_eq(
                &dark,
                &context.style_of(egui::Theme::Dark)
            ));
        }
        assert_eq!(context.zoom_factor(), 1.0);
        assert!(!context.options(|options| options.zoom_with_keyboard));
    }
    #[test]
    fn every_semantic_palette_keeps_readable_text_and_warning_contrast() {
        fn luminance(color: Color32) -> f64 {
            let channel = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        }
        fn contrast(a: Color32, b: Color32) -> f64 {
            let (a, b) = (luminance(a), luminance(b));
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        }
        for (light, grey) in [(false, false), (false, true), (true, false)] {
            let visuals = style(light, grey).visuals;
            for background in [
                visuals.panel_fill,
                visuals.window_fill,
                visuals.extreme_bg_color,
            ] {
                assert!(contrast(visuals.text_color(), background) >= 4.5);
                assert!(
                    contrast(visuals.error_fg_color, background) >= 4.5,
                    "error on {background:?}"
                );
                assert!(
                    contrast(visuals.warn_fg_color, background) >= 4.5,
                    "warning on {background:?}"
                );
            }
        }
    }
}
