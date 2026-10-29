//! NDI controls are a window-only draft. Start is separate from preference Apply.
use eframe::egui;
use skylive_core::{NdiOutputSettings,NdiOutputRoute,StudioSnapshot};
use skylive_host::ndi::output::{Key,Status};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct Token {pub session:u64,pub revision:u64,pub generation:u64}
pub struct Draft {pub token:Token,program:String,preview:String,groups:String,audio:bool,audio_track:skylive_core::AudioTrackId}
impl Draft {pub fn new(token:Token,settings:&NdiOutputSettings)->Self{Self {token,program:settings.program_name().into(),preview:settings.preview_name().into(),groups:settings.groups().into(),audio:settings.program_audio(),audio_track:settings.audio_track()}}pub fn settings(&self)->Result<NdiOutputSettings,String>{NdiOutputSettings::new(self.program.clone(),self.preview.clone(),self.groups.clone(),self.audio).map(|v|v.with_audio_track(self.audio_track)).map_err(|e|e.to_string())}}
pub enum Action {Apply {token:Token,settings:NdiOutputSettings},Start {token:Token,route:NdiOutputRoute},Stop(Key),Close(Token)}
pub struct View {pub session:u64,pub enabled:bool,pub editing:bool,pub statuses:[Status;2],pub keys:[Option<Key>;2],pub owned:[bool;2]}
pub fn show(ctx:&egui::Context,draft:&mut Draft,model:&StudioSnapshot,view:View)->Vec<Action>{
    let mut actions=Vec::new();let fresh=draft.token.session==view.session&&draft.token.revision==model.revision();let active=view.owned.iter().any(|v|*v);
    let response=egui::Modal::new(egui::Id::new("ndi-output-settings")).show(ctx,|ui|{
        ui.set_width(640.0_f32.min((ctx.screen_rect().width()-48.0).max(120.0)));ui.heading(crate::i18n::tr(ui,"NDI Output Settings"));
        egui::ScrollArea::vertical().max_height((ctx.screen_rect().height()-190.0).max(120.0)).show(ui,|ui|{
            ui.label(crate::i18n::tr(ui,"Program sends the final output video and optional Program audio. Preview sends only its base-canvas scene, without Program transitions or output downscaling."));
            ui.add_enabled_ui(fresh&&view.editing&&!active,|ui|{
                for (label,field,id) in [("Program NDI Name",&mut draft.program,"ndi-program-name"),("Preview NDI Name",&mut draft.preview,"ndi-preview-name"),("NDI Groups",&mut draft.groups,"ndi-output-groups")]{let label=ui.label(crate::i18n::tr(ui,label));ui.add(egui::TextEdit::singleline(field).char_limit(512).desired_width(ui.available_width()).id_salt(id)).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Names are sent exactly as entered. Groups use the installed NDI Runtime's comma-separated group syntax; empty means the default group."));}
                ui.checkbox(&mut draft.audio,crate::i18n::tr(ui,"Send Program Audio")).on_hover_text(crate::i18n::tr(ui,"Send the selected post-DSP audio track as planar float PCM with Program video. Preview never sends audio."));
                super::output_audio_tracks::selector(ui,"ndi-program-audio-track","NDI Program Audio Track",&mut draft.audio_track,draft.audio);
            }).response.on_disabled_hover_text(crate::i18n::tr(ui,"Stop both NDI outputs before editing sender names, groups or audio settings."));
            let valid=draft.settings();let changed=valid.as_ref().is_ok_and(|v|v!=model.preferences().ndi_output());
            if !fresh{ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,"The document changed. Reopen NDI Output Settings before applying or starting."));}
            if let Err(error)=&valid{ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,error));}
            for route in [NdiOutputRoute::Program,NdiOutputRoute::Preview]{let i=route.index();let s=&view.statuses[i];ui.separator();ui.strong(crate::i18n::tr(ui,if route==NdiOutputRoute::Program{"Program NDI Output"}else{"Preview NDI Output"}));
                let dimensions=if route==NdiOutputRoute::Program{model.output_video().ok()}else{Some(model.video().clone())};if let Some(video)=dimensions{ui.label(crate::i18n::tf(ui,"NDI format: {width}×{height} · {num}/{den} FPS",&[("{width}",video.width().to_string()),("{height}",video.height().to_string()),("{num}",video.fps_num().to_string()),("{den}",video.fps_den().to_string())]));}
                let phase=if s.stopping{"NDI sender stopping…"}else if s.starting{"NDI sender starting…"}else if s.sending{"NDI sender sending"}else if s.backend_ready{"NDI sender ready; waiting for video"}else{"NDI sender stopped"};ui.label(crate::i18n::tr(ui,phase));
                if let Some(name)=&s.advertised_name{ui.label(crate::i18n::tf(ui,"Advertised NDI source: {name}",&[("{name}",name.clone())]));}if let Some(version)=&s.runtime_version{ui.small(version);}if let Some(problem)=&s.problem{ui.colored_label(ui.visuals().error_fg_color,problem);}
                ui.small(crate::i18n::tf(ui,"NDI: {video} video frames · {audio} audio frames · {connections} receivers",&[("{video}",s.video_frames.to_string()),("{audio}",s.audio_frames.to_string()),("{connections}",s.connections.to_string())]));
                let buttons=[super::modal_footer::Action::new("Start NDI Output",view.enabled&&view.editing&&fresh&&!changed&&valid.is_ok()&&!view.owned[i],"Apply current settings first. Start explicitly advertises this sender through the installed NDI6 Runtime."),super::modal_footer::Action::new("Stop NDI Output",view.owned[i]&&!s.stopping,"Stop this sender and release its retained video on its worker. Other outputs keep running.")];
                match super::modal_footer::show(ui,&buttons){Some(0)=>actions.push(Action::Start {token:draft.token,route}),Some(1)=>{if let Some(key)=view.keys[i]{actions.push(Action::Stop(key));}},_=>{}}
            }
            ui.separator();ui.small(crate::i18n::tr(ui,"Opening settings, enabling the module or loading a collection never starts NDI sending. Sender names remain advertised only while that output is owned; Stop can wait for an in-flight SDK call."));
        });ui.separator();
        let valid=draft.settings();let changed=valid.as_ref().is_ok_and(|v|v!=model.preferences().ndi_output());
        match super::modal_footer::show(ui,&[super::modal_footer::Action::new("Apply",view.editing&&fresh&&!active&&changed&&valid.is_ok(),"Save NDI names, groups and Program audio preference. This does not start any sender."),super::modal_footer::Action::new("Close",true,"Close this editor. Running NDI outputs continue until explicitly stopped.")]){Some(0)=>{if let Ok(settings)=valid{actions.push(Action::Apply {token:draft.token,settings});}},Some(1)=>actions.push(Action::Close(draft.token)),_=>{}}
    });if response.should_close(){actions.push(Action::Close(draft.token));}actions
}
