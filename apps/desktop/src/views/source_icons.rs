use eframe::egui;
use skylive_core::SourceKind;

pub fn show(ui: &mut egui::Ui, kind: &SourceKind) {
    if !ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::new("source-type-icons"))).unwrap_or(true) { return; }
    let (rect, response) = ui.allocate_exact_size(egui::vec2(16.0, 18.0), egui::Sense::hover());
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let center = rect.center();
    let stroke = egui::Stroke::new(1.2, ui.visuals().text_color());
    let bounds = egui::Rect::from_center_size(center, egui::vec2(14.0, 11.0));
    let label = match kind {
        SourceKind::Color { rgba, .. } => {
            painter.rect_filled(bounds.shrink(1.0), 1.0, egui::Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]));
            "Color Source"
        }
        SourceKind::Text { .. } => {
            painter.line_segment([center + egui::vec2(-5.0, -5.0), center + egui::vec2(5.0, -5.0)], stroke);
            painter.line_segment([center + egui::vec2(0.0, -5.0), center + egui::vec2(0.0, 5.0)], stroke);
            painter.line_segment([center + egui::vec2(-3.0, 5.0), center + egui::vec2(3.0, 5.0)], stroke);
            "Text"
        }
        SourceKind::Browser {..}=>{painter.rect_stroke(bounds,1.0,stroke,egui::StrokeKind::Inside);painter.line_segment([bounds.left_top()+egui::vec2(0.0,3.0),bounds.right_top()+egui::vec2(0.0,3.0)],stroke);"Browser"},
        SourceKind::VideoCapture { .. } => {
            painter.circle_stroke(center, 3.0, stroke);
            painter.rect_stroke(bounds, 1.0, stroke, egui::StrokeKind::Inside);
            "Video Capture Device"
        }
        SourceKind::DisplayCapture { .. } | SourceKind::WindowCapture { .. } => {
            painter.rect_stroke(bounds.translate(egui::vec2(0.0, -2.0)), 1.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment([center + egui::vec2(0.0, 4.0), center + egui::vec2(0.0, 7.0)], stroke);
            painter.line_segment([center + egui::vec2(-4.0, 7.0), center + egui::vec2(4.0, 7.0)], stroke);
            if matches!(kind, SourceKind::DisplayCapture { .. }) { "Display Capture" } else { "Window Capture" }
        }
        SourceKind::GameCapture {..}=>{painter.rect_stroke(bounds,3.0,stroke,egui::StrokeKind::Inside);painter.line_segment([center+egui::vec2(-4.0,0.0),center+egui::vec2(0.0,0.0)],stroke);painter.circle_filled(center+egui::vec2(4.0,-2.0),1.0,stroke.color);"Game Capture"},
        SourceKind::NdiSource {..}=>{painter.rect_stroke(bounds,1.0,stroke,egui::StrokeKind::Inside);painter.circle_stroke(center,3.0,stroke);"NDI Source"},
        SourceKind::MediaSource { .. } => {
            painter.add(egui::Shape::convex_polygon(vec![center + egui::vec2(-4.0, -6.0), center + egui::vec2(6.0, 0.0), center + egui::vec2(-4.0, 6.0)], stroke.color, egui::Stroke::NONE));
            "Media Source"
        }
        SourceKind::AudioCapture { binding } => {
            painter.rect_stroke(egui::Rect::from_center_size(center + egui::vec2(-2.0, -1.0), egui::vec2(6.0, 10.0)), 3.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment([center + egui::vec2(-2.0, 4.0), center + egui::vec2(-2.0, 7.0)], stroke);
            painter.line_segment([center + egui::vec2(-5.0, 7.0), center + egui::vec2(1.0, 7.0)], stroke);
            if matches!(binding.target(),skylive_core::AudioTarget::WindowsProcess {..}|skylive_core::AudioTarget::WindowsWindow {..}) {"Application Audio Capture"}
            else if binding.flow() == skylive_core::AudioFlow::Input { "Audio Input Capture" } else { "Audio Output Capture" }
        }
        SourceKind::Group { .. } => {
            painter.rect_stroke(bounds, 1.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment([bounds.left_top() + egui::vec2(1.0, -2.0), bounds.left_top() + egui::vec2(7.0, -2.0)], stroke);
            "Group"
        }
        SourceKind::Scene { .. } => {
            painter.rect_stroke(bounds.translate(egui::vec2(2.0, -2.0)).shrink(1.0), 1.0, stroke, egui::StrokeKind::Inside);
            painter.rect_stroke(bounds.translate(egui::vec2(-1.0, 1.0)).shrink(1.0), 1.0, stroke, egui::StrokeKind::Inside);
            "Scene"
        }
        SourceKind::Image { .. } | SourceKind::ImageSlideShow { .. } => {
            painter.rect_stroke(bounds, 1.0, stroke, egui::StrokeKind::Inside);
            painter.add(egui::Shape::line(vec![center + egui::vec2(-5.0, 3.0), center + egui::vec2(-1.0, -2.0), center + egui::vec2(2.0, 1.0), center + egui::vec2(5.0, -2.0)], stroke));
            painter.circle_filled(center + egui::vec2(4.0, -3.0), 1.2, stroke.color);
            if matches!(kind, SourceKind::ImageSlideShow { .. }) { "Image Slide Show" } else { "Image" }
        }
    };
    response.on_hover_text(crate::i18n::tr(ui, label));
}
