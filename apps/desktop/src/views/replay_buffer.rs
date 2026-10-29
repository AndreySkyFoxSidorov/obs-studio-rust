//! Replay Controls reflect real worker state; no presentation-owned toggle.
use super::Intent;
use eframe::egui;
#[derive(Clone,Default)]
pub struct ReplayView {pub available:bool,pub status:skylive_host::replay_buffer::ReplayStatus}
impl ReplayView {pub fn install(self,context:&egui::Context){context.data_mut(|d|d.insert_temp(egui::Id::new("replay-buffer-state"),self));}}
pub fn control(ui:&mut egui::Ui,enabled:bool,intents:&mut Vec<Intent>){
    let view=ui.ctx().data(|d|d.get_temp::<ReplayView>(egui::Id::new("replay-buffer-state"))).unwrap_or_default();let state=&view.status;
    let running=state.active||state.starting||state.stopping;
    let caption=if state.stopping {"Stopping Replay Buffer…"}else if state.starting {"Starting Replay Buffer…"}else if state.active {"Stop Replay Buffer"}else{"Start Replay Buffer"};
    if ui.add_enabled(view.available&&!state.stopping&&(enabled||running),egui::Button::new(crate::i18n::tr(ui,caption)).wrap().min_size(egui::vec2(ui.available_width(),24.0)))
        .on_hover_text(crate::i18n::tr(ui,"Keep recent encoded Program video and audio in memory without continuous disk recording."))
        .on_disabled_hover_text(crate::i18n::tr(ui,if !view.available {"Replay Buffer packet encoders are available on Windows."}else{"Wait for the replay owner to stop."})).clicked(){intents.push(if running {Intent::StopReplayBuffer}else{Intent::StartReplayBuffer});}
    if running||state.saving {
        if ui.add_enabled(state.active&&!state.stopping&&state.ready&&!state.saving,egui::Button::new(crate::i18n::tr(ui,if state.saving {"Saving Replay…"}else{"Save Replay"})).wrap().min_size(egui::vec2(ui.available_width(),24.0)))
            .on_hover_text(crate::i18n::tr(ui,"Save a complete keyframe-aligned MP4 to the recording folder without overwriting an existing file."))
            .on_disabled_hover_text(crate::i18n::tr(ui,"Wait for a complete video keyframe and encoded audio, or for the current save to finish.")).clicked(){intents.push(Intent::SaveReplayBuffer);}
        if state.saving&&ui.add(egui::Button::new(crate::i18n::tr(ui,"Cancel Replay Save")).wrap().min_size(egui::vec2(ui.available_width(),24.0))).on_hover_text(crate::i18n::tr(ui,"Cancel the pending save while keeping the replay buffer running. An already published file is retained.")).clicked(){intents.push(Intent::CancelReplaySave);}
        ui.small(crate::i18n::tf(ui,"Replay: {seconds} s · {memory} MiB encoded",&[("{seconds}",format!("{:.1}",state.duration_100ns as f64/10_000_000.0)),("{memory}",format!("{:.1}",state.stored_bytes as f64/(1024.0*1024.0)))]));
    }
    if state.memory_limited {ui.small(crate::i18n::tr(ui,"Replay duration was shortened by the encoded memory limit."));}
    if let Some(error)=&state.problem {ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tf(ui,"Replay Buffer: {error}",&[("{error}",error.clone())]));}
}
