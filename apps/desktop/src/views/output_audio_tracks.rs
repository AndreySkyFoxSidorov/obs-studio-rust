//! Presentation of existing typed logical audio mixes; no DSP, IO or native calls.
use eframe::egui;
use skylive_core::{AudioTrackId,RecordingTrack,RecordingTracks};
const NAME_HINT:&str="Leave the name empty to use Track1 to Track6. Names are written to the recording exactly as entered; at most 256 UTF-8 bytes, without control characters.";
const RATE_HINT:&str="Inherit the recording audio bitrate, or choose an explicit AAC bitrate for this track. Unselected tracks retain their names and bitrates.";
const SELECT_HINT:&str="Select one real post-DSP audio mix from Advanced Audio Properties. This does not add audio from Preview-only sources or start an output.";
pub fn label(ui:&egui::Ui,id:AudioTrackId)->String{crate::i18n::tf(ui,"Audio Track {number}",&[("{number}",id.get().to_string())])}
pub fn selector(ui:&mut egui::Ui,id:&str,label_text:&str,track:&mut AudioTrackId,enabled:bool){
    let disabled_hint=if enabled{SELECT_HINT}else{"The selected audio track is retained while this output's audio is disabled."};
    ui.add_enabled_ui(enabled,|ui|{let caption=ui.label(crate::i18n::tr(ui,label_text));egui::ComboBox::from_id_salt(id).selected_text(label(ui,*track)).show_ui(ui,|ui|{for raw in 1..=6{if let Ok(value)=AudioTrackId::new(raw){ui.selectable_value(track,value,label(ui,value)).on_hover_text(crate::i18n::tr(ui,SELECT_HINT));}}}).response.labelled_by(caption.id).on_hover_text(crate::i18n::tr(ui,SELECT_HINT)).on_disabled_hover_text(crate::i18n::tr(ui,disabled_hint));}).response.on_disabled_hover_text(crate::i18n::tr(ui,disabled_hint));
}
pub fn names(settings:&RecordingTracks)->[String;6]{std::array::from_fn(|index|AudioTrackId::new(index as u8+1).map(|id|settings.entry(id).name().to_owned()).unwrap_or_default())}
pub fn settings(tracks:&RecordingTracks,names:&[String;6])->Result<RecordingTracks,String>{
    if names.iter().any(|name|name.len()>256||name.chars().any(char::is_control)){return Err("Track names must contain at most 256 UTF-8 bytes and no control characters.".into());}
    let mut entries=std::array::from_fn(|_|RecordingTrack::default());for raw in 1..=6{let id=AudioTrackId::new(raw).map_err(|e|e.to_string())?;entries[id.index()]=RecordingTrack::new(names[id.index()].clone(),tracks.entry(id).bitrate_kbps()).map_err(|e|e.to_string())?;}RecordingTracks::new(tracks.mask(),entries).map_err(|e|e.to_string())
}
fn rate_text(ui:&egui::Ui,rate:Option<u32>,inherited:u32)->String{match rate{None=>crate::i18n::tf(ui,"Inherit · {rate} kbit/s",&[("{rate}",inherited.to_string())]),Some(rate)=>crate::i18n::tf(ui,"{rate} kbit/s",&[("{rate}",rate.to_string())])}}
fn rate(ui:&mut egui::Ui,id:AudioTrackId,value:&mut Option<u32>,inherited:u32,label_id:egui::Id,track_id:egui::Id){egui::ComboBox::from_id_salt(("recording-track-rate",id.get())).selected_text(rate_text(ui,*value,inherited)).width(ui.available_width().clamp(24.0,230.0)).truncate().show_ui(ui,|ui|{ui.selectable_value(value,None,rate_text(ui,None,inherited)).on_hover_text(crate::i18n::tr(ui,RATE_HINT));for rate in [96,128,160,192]{ui.selectable_value(value,Some(rate),rate_text(ui,Some(rate),inherited)).on_hover_text(crate::i18n::tr(ui,RATE_HINT));}}).response.labelled_by(label_id).labelled_by(track_id).on_hover_text(crate::i18n::tr(ui,RATE_HINT)).on_disabled_hover_text(crate::i18n::tr(ui,RATE_HINT));}
fn selected(ui:&mut egui::Ui,id:AudioTrackId,mask:&mut u8)->egui::Response{
    let mut checked=*mask&id.bit()!=0;let can_toggle=!checked||*mask!=id.bit();let response=ui.add_enabled(can_toggle,egui::Checkbox::new(&mut checked,label(ui,id))).on_hover_text(crate::i18n::tr(ui,"Include this logical audio mix as a separate AAC track in the recording. At least one track must remain selected.")).on_disabled_hover_text(crate::i18n::tr(ui,if can_toggle{"Include this logical audio mix as a separate AAC track in the recording. At least one track must remain selected."}else{"At least one recording track must remain selected. Select another track before disabling this one."}));if response.changed(){if checked{*mask|=id.bit();}else{*mask&=!id.bit();}}response
}
pub fn recording(ui:&mut egui::Ui,tracks:&mut RecordingTracks,names:&mut[String;6],inherited:u32,audio_enabled:bool){
    let capabilities=skylive_host::recording::capabilities();let capability=capabilities.get("multitrack");let supported=capability.and_then(|v|v.get("native_backend_compiled")).and_then(|v|v.as_bool()).unwrap_or(false)&&capability.and_then(|v|v.get("program_producer_attached")).and_then(|v|v.as_bool()).unwrap_or(false);
    ui.strong(crate::i18n::tr(ui,"Recording Audio Tracks"));ui.small(crate::i18n::tr(ui,"Record up to six independent AAC mixes. Route each source to tracks in Advanced Audio Properties; selecting a track does not duplicate another mix."));
    if !audio_enabled{ui.small(crate::i18n::tr(ui,"Recording audio is off. These track choices are saved for the next recording with audio enabled."));}
    let mut mask=tracks.mask();let mut rates:[Option<u32>;6]=std::array::from_fn(|i|AudioTrackId::new(i as u8+1).ok().and_then(|id|tracks.entry(id).bitrate_kbps()));
    ui.add_enabled_ui(supported,|ui|{
        let wide=ui.available_width()>=580.0;
        if wide{egui::Grid::new("recording-track-fields").num_columns(3).striped(true).spacing(egui::vec2(12.0,6.0)).show(ui,|ui|{
            ui.label(crate::i18n::tr(ui,"Record"));let name_label=ui.label(crate::i18n::tr(ui,"Track name"));let rate_label=ui.label(crate::i18n::tr(ui,"AAC bitrate (kbit/s)"));ui.end_row();
            for raw in 1..=6{if let Ok(id)=AudioTrackId::new(raw){let track_label=selected(ui,id,&mut mask);ui.add(egui::TextEdit::singleline(&mut names[id.index()]).char_limit(256).desired_width((ui.available_width()-250.0).clamp(90.0,240.0)).hint_text(format!("Track{}",id.get())).id_salt(("recording-track-name",id.get()))).labelled_by(name_label.id).labelled_by(track_label.id).on_hover_text(crate::i18n::tr(ui,NAME_HINT)).on_disabled_hover_text(crate::i18n::tr(ui,NAME_HINT));rate(ui,id,&mut rates[id.index()],inherited,rate_label.id,track_label.id);ui.end_row();}}
        });}else{for raw in 1..=6{if let Ok(id)=AudioTrackId::new(raw){ui.push_id(("recording-track-row",id.get()),|ui|{let track_id=ui.horizontal_wrapped(|ui|{let track_label=selected(ui,id,&mut mask);let name_label=ui.label(crate::i18n::tr(ui,"Track name"));ui.add(egui::TextEdit::singleline(&mut names[id.index()]).char_limit(256).desired_width(ui.available_width().max(60.0)).hint_text(format!("Track{}",id.get())).id_salt(("recording-track-name",id.get()))).labelled_by(name_label.id).labelled_by(track_label.id).on_hover_text(crate::i18n::tr(ui,NAME_HINT)).on_disabled_hover_text(crate::i18n::tr(ui,NAME_HINT));track_label.id}).inner;ui.horizontal_wrapped(|ui|{let caption=ui.label(crate::i18n::tr(ui,"AAC bitrate (kbit/s)"));rate(ui,id,&mut rates[id.index()],inherited,caption.id,track_id);});});ui.add_space(6.0);}}}
    }).response.on_disabled_hover_text(crate::i18n::tr(ui,"Multiple audio track recording requires the active Windows multitrack backend. Saved choices are retained."));
    if !supported{ui.small(crate::i18n::tr(ui,"Multiple audio track recording requires the active Windows multitrack backend. Saved choices are retained."));}
    let mut entries=std::array::from_fn(|_|RecordingTrack::default());let mut problem=None;
    for raw in 1..=6{if let Ok(id)=AudioTrackId::new(raw){match RecordingTrack::new(tracks.entry(id).name().to_owned(),rates[id.index()]){Ok(entry)=>entries[id.index()]=entry,Err(error)=>problem=Some(error.to_string())}}}
    if problem.is_none(){match RecordingTracks::new(mask,entries){Ok(value)=>*tracks=value,Err(error)=>problem=Some(error.to_string())}}
    if let Some(error)=problem{ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,error));}
    if let Err(error)=settings(tracks,names){ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,error));}
}
