use eframe::egui;
use skylive_host::camera_activation::{Phase,Status};
fn phase(ui:&egui::Ui,value:Phase)->String{crate::i18n::tr(ui,match value {Phase::Idle=>"Idle",Phase::Queued=>"Queued",Phase::Working=>"Working",Phase::Retiring=>"Retiring",Phase::Complete=>"Acknowledged",Phase::Failed=>"Failed"})}
pub fn show(ui:&mut egui::Ui,status:Option<&Status>,source:skylive_core::Id){
    let Some(status)=status.filter(|status|status.key.is_some())else{ui.small(crate::i18n::tr(ui,"Camera source AV is eligible by default. Headless Program audio does not require camera video to be armed."));return;};
    if !status.sources.contains(&source){if status.pending {ui.small(crate::i18n::tr(ui,"Another camera AV operation is pending. Deactivate All Camera Sources can stop a pending start."));}return;}
    if status.pending {ui.horizontal(|ui|{ui.spinner();ui.label(crate::i18n::tr(ui,if status.queued_stop.is_some(){"Stop queued; waiting for noncancelable native work"}else{"Waiting for camera video and audio acknowledgements"}));});}
    ui.label(crate::i18n::tf(ui,"Video: {video} · Audio: {audio}",&[("{video}",phase(ui,status.video.phase)),("{audio}",phase(ui,status.audio.phase))]));
    for problem in [status.problem.as_ref(),status.video.problem.as_ref(),status.audio.problem.as_ref()].into_iter().flatten(){ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tf(ui,"{code}: {detail}",&[("{code}",problem.code.into()),("{detail}",crate::i18n::tr(ui,&problem.detail))]));}
    if let Some(cut)=status.audio.cut {ui.small(crate::i18n::tf(ui,"Audio cut: frame {frame} at {rate} Hz. Already committed mixed blocks cannot be recalled.",&[("{frame}",cut.frame.to_string()),("{rate}",cut.sample_rate.to_string())]));}
    if let (Some(blocks),Some(maximum),Some(padding))=(status.audio_buffer_blocks,status.audio_max_buffer_blocks,status.monitor_padding_ns){ui.small(crate::i18n::tf(ui,"Program buffer: {blocks}/{maximum} blocks · monitor padding: {padding} ns",&[("{blocks}",blocks.to_string()),("{maximum}",maximum.to_string()),("{padding}",padding.to_string())]));}
    ui.small(crate::i18n::tr(ui,"Acknowledgement confirms source eligibility or retirement. An unused source does not open hidden devices; actual frame availability remains separate."));
}
