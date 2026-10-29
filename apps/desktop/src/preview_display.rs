//! Window-owned preview scale and pan; never changes capture/output formats.
use eframe::egui;

#[derive(Clone, Copy, Debug)]
pub enum Action { FitWindow, Canvas, Output, ZoomIn, ZoomOut, ResetZoom }
#[derive(Clone,Copy)]
enum NativeScale {Canvas,Output}
#[derive(Clone, Copy)]
struct Display { native: Option<NativeScale>, zoom: f32, pan: egui::Vec2, fitted_pixels: egui::Vec2 }
impl Default for Display {
    fn default() -> Self { Self { native: None, zoom: 1.0, pan: egui::Vec2::ZERO, fitted_pixels: egui::vec2(640.0,360.0) } }
}
fn identity(name: &str) -> egui::Id { egui::Id::new(("preview-display", name)) }
pub fn apply(context: &egui::Context, action: Action) {
    context.data_mut(|data| {
        for name in ["preview", "program"] {
            let state = data.get_temp_mut_or_default::<Display>(identity(name));
            match action {
                Action::FitWindow => { state.native = None; state.zoom = 1.0; state.pan = egui::Vec2::ZERO; }
                Action::Canvas=>{state.native=Some(NativeScale::Canvas);state.zoom=1.0;state.pan=egui::Vec2::ZERO;}
                Action::Output=>{state.native=Some(NativeScale::Output);state.zoom=1.0;state.pan=egui::Vec2::ZERO;}
                Action::ZoomIn => state.zoom = (state.zoom * 1.25).min(8.0),
                Action::ZoomOut => state.zoom = (state.zoom / 1.25).max(0.125),
                Action::ResetZoom => { state.zoom = 1.0; state.pan = egui::Vec2::ZERO; }
            }
        }
    });
    context.request_repaint();
}
#[allow(dead_code)] // Compatibility facade for callers without output metadata.
pub fn render_dimensions(context: &egui::Context, name: &str, width: u32, height: u32) -> (u32, u32) {
    render_dimensions_profile(context,name,[width,height],[width,height])
}
pub fn render_dimensions_profile(context:&egui::Context,name:&str,canvas:[u32;2],output:[u32;2])->(u32,u32){
    let state = context.data(|data| data.get_temp::<Display>(identity(name))).unwrap_or_default();
    let reference=if matches!(state.native,Some(NativeScale::Output)){output}else{canvas};
    let native_width=f64::from(reference[0].max(1));
    let native_height=f64::from(reference[1].max(1));
    // Match the actual physical display area, including Windows DPI. Coarse
    // upward buckets avoid creating a new render key for every resize pixel.
    let bucket=|value:f32| ((f64::from(value.max(1.0))/32.0).ceil()*32.0).max(1.0);
    let fitted_scale=(bucket(state.fitted_pixels.x)/native_width)
        .min(bucket(state.fitted_pixels.y)/native_height).min(1.0);
    let desired=if state.native.is_some() {1.0}else{fitted_scale}*f64::from(state.zoom.max(1.0));
    let scale=desired.min(1920.0/native_width).min(1080.0/native_height);
    ((native_width*scale).round().clamp(1.0,1920.0) as u32,
        (native_height*scale).round().clamp(1.0,1080.0) as u32)
}
#[allow(dead_code)] // Compatibility facade for callers without output metadata.
pub fn rectangle(ui: &egui::Ui, name: &str, stage: egui::Rect, fitted: egui::Rect, canvas: [u32; 2]) -> egui::Rect {
    rectangle_profile(ui,name,stage,fitted,canvas,canvas)
}
pub fn rectangle_profile(ui:&egui::Ui,name:&str,stage:egui::Rect,fitted:egui::Rect,canvas:[u32;2],output:[u32;2])->egui::Rect{
    let mut state = ui.ctx().data(|data| data.get_temp::<Display>(identity(name))).unwrap_or_default();
    let pixels=fitted.size()*ui.ctx().pixels_per_point();
    let bucket=|value:egui::Vec2| ((value.x/32.0).ceil() as u32,(value.y/32.0).ceil() as u32);
    if bucket(pixels)!=bucket(state.fitted_pixels) {ui.ctx().request_repaint();}
    state.fitted_pixels=pixels;
    let previous_zoom=state.zoom;
    if ui.ctx().pointer_hover_pos().is_some_and(|position| stage.contains(position)) {
        let (scroll, command, middle, delta) = ui.input(|input| (
            input.raw_scroll_delta, input.modifiers.command || input.modifiers.ctrl,
            input.pointer.button_down(egui::PointerButton::Middle), input.pointer.delta(),
        ));
        if command && scroll.y != 0.0 {
            state.zoom = (state.zoom * (scroll.y * 0.004).exp()).clamp(0.125, 8.0);
        } else { state.pan += scroll; }
        if middle { state.pan += delta; }
    }
    if state.zoom!=previous_zoom {ui.ctx().request_repaint();}
    let reference=if matches!(state.native,Some(NativeScale::Output)){output}else{canvas};
    let base = if state.native.is_some() {
        egui::vec2(reference[0] as f32,reference[1] as f32) / ui.ctx().pixels_per_point()
    } else { fitted.size() };
    let size = base * state.zoom;
    let overflow = (size - stage.size()).max(egui::Vec2::ZERO) * 0.5;
    state.pan.x = state.pan.x.clamp(-overflow.x, overflow.x);
    state.pan.y = state.pan.y.clamp(-overflow.y, overflow.y);
    ui.ctx().data_mut(|data| data.insert_temp(identity(name), state));
    egui::Rect::from_center_size(stage.center() + state.pan, size)
}
