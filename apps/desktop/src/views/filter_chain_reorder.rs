//! Fixed-size, draft-only pointer reorder with an exact press-time list proof.
use eframe::egui;
const MAX: usize = skylive_core::MAX_VIDEO_FILTERS;

#[derive(Clone, Copy, PartialEq)]
pub(super) struct Scope<P = ()> {
    owner: egui::Id,
    session: u64,
    revision: u64,
    token: u64,
    order: [u32; MAX],
    count: usize,
    proof: P,
}
#[derive(Clone, Copy)]
struct Payload<P> { scope: Scope<P>, row: u32 }

impl Scope {
    pub(super) fn new(owner: egui::Id, session: u64, revision: u64, token: u64,
        rows: impl Iterator<Item = u32>) -> Option<Self> {
        let mut order = [0; MAX];
        let mut count = 0;
        for row in rows {
            if count == MAX { return None; }
            order[count] = row;
            count += 1;
        }
        Some(Self { owner, session, revision, token, order, count, proof:() })
    }
}

pub(super) type AudioProof = [Option<skylive_core::AudioFilter>; MAX];
impl Scope<AudioProof> {
    pub(super) fn audio(owner:egui::Id,session:u64,revision:u64,token:u64,
        filters:&[skylive_core::AudioFilter])->Option<Self> {
        let base=Scope::new(owner,session,revision,token,
            filters.iter().enumerate().map(|(index,_)|index as u32+1))?;
        let mut proof=[None;MAX];
        for (index,filter) in filters.iter().enumerate() { proof[index]=Some(*filter); }
        Some(Self {owner,session,revision,token,order:base.order,count:base.count,proof})
    }
}

pub(super) fn row<P:Copy+PartialEq+Send+Sync+'static>(ui: &egui::Ui, response: &egui::Response, scope: Scope<P>,
    row: u32, enabled: bool) {
    let press_id = response.id.with("filter-reorder-press");
    if ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary))
        && response.is_pointer_button_down_on() && enabled && ui.is_enabled() {
        ui.data_mut(|data| data.insert_temp(press_id, Payload { scope, row }));
    }
    if response.drag_started_by(egui::PointerButton::Primary) && enabled && ui.is_enabled() {
        let pressed = ui.data_mut(|data| {
            let value = data.get_temp::<Payload<P>>(press_id);
            data.remove::<Payload<P>>(press_id);
            value
        });
        if let Some(payload) = pressed.filter(|value| value.scope == scope && value.row == row) {
            egui::DragAndDrop::set_payload(ui.ctx(), payload);
        }
    }
    if ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary)) {
        ui.data_mut(|data| data.remove::<Payload<P>>(press_id));
    }
    if enabled { response.clone().on_hover_cursor(egui::CursorIcon::Grab); }
}

pub(super) fn finish<P:Copy+PartialEq+Send+Sync+'static>(ui: &mut egui::Ui, scope: Scope<P>, rows: &[(u32, egui::Rect)],
    enabled: impl Fn(u32) -> bool) -> Option<(usize, usize)> {
    let payload = egui::DragAndDrop::payload::<Payload<P>>(ui.ctx())?;
    if payload.scope.owner != scope.owner { return None; }
    if payload.scope != scope || rows.len() != scope.count || !ui.is_enabled()
        || !enabled(payload.row) || ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        egui::DragAndDrop::clear_payload(ui.ctx());
        return None;
    }
    let old = rows.iter().position(|(id, _)| *id == payload.row)?;
    let released = ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary));
    if !released && !ui.input(|input| input.pointer.primary_down()) {
        egui::DragAndDrop::clear_payload(ui.ctx());
        return None;
    }
    let pointer = ui.input(|input| input.pointer.interact_pos())?;
    let viewport = ui.clip_rect();
    if !viewport.contains(pointer) || !ui.rect_contains_pointer(viewport) {
        if released { egui::DragAndDrop::clear_payload(ui.ctx()); }
        return None;
    }
    let slot = rows.iter().position(|(_, rect)| pointer.y < rect.center().y).unwrap_or(rows.len());
    let index = slot.saturating_sub(usize::from(slot > old));
    if old != index {
        let y = if slot == 0 { rows[0].1.top() }
            else if slot == rows.len() { rows[rows.len() - 1].1.bottom() }
            else { (rows[slot - 1].1.bottom() + rows[slot].1.top()) * 0.5 };
        let painter = ui.painter().with_clip_rect(viewport);
        let color = ui.visuals().selection.stroke.color;
        painter.hline(viewport.x_range(), y, egui::Stroke::new(2.0, color));
        if released {
            egui::DragAndDrop::clear_payload(ui.ctx());
            return Some((old, index));
        }
    } else if released {
        egui::DragAndDrop::clear_payload(ui.ctx());
    }
    let edge = 20.0f32.min(viewport.height() * 0.25);
    let direction = if pointer.y < viewport.top() + edge { 1.0 }
        else if pointer.y > viewport.bottom() - edge { -1.0 } else { 0.0 };
    if direction != 0.0 && !released {
        let dt = ui.input(|input| input.stable_dt).clamp(0.0, 0.05);
        ui.scroll_with_delta(egui::vec2(0.0, direction * 300.0 * dt));
        ui.ctx().request_repaint();
    }
    None
}
