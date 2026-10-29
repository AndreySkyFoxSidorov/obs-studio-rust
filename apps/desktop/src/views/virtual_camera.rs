//! Real service state is projected here; this module never starts native devices.
use eframe::egui;
use skylive_core::{Id,VirtualCameraRoute};
use super::{Intent,modal_footer::{self,Action}};

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub enum Profile { #[default] Hd720p30, FullHd1080p30 }
impl Profile {
    pub fn dimensions(self)->(u32,u32,u32) {match self {Self::Hd720p30=>(1280,720,30),Self::FullHd1080p30=>(1920,1080,30)}}
    pub fn label(self)->&'static str {match self {Self::Hd720p30=>"1280 × 720 · 30 fps",Self::FullHd1080p30=>"1920 × 1080 · 30 fps"}}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct Token {pub session:u64,pub revision:u64,pub nonce:u64}
#[derive(Clone)]
pub struct Draft {pub token:Token,pub profile:Profile,pub route:VirtualCameraRoute,pub scenes:Vec<Choice>,pub sources:Vec<Choice>}
#[derive(Clone)]
pub struct Choice {pub id:Id,pub name:String}
#[derive(Clone,Copy)]
pub struct Submission {pub token:Token,pub profile:Profile,pub route:VirtualCameraRoute}
#[derive(Clone,Default)]
pub struct View {
    pub available:bool,pub active:bool,pub starting:bool,pub stopping:bool,
    pub frames:u64,pub source_starts:u64,pub consumer_active:bool,pub waiting_for_frame:bool,pub problem:Option<String>,
}
impl View {pub fn owned(&self)->bool {self.active||self.starting||self.stopping}}
const VIEW:&str="actual-virtual-camera-view";
pub fn install(ctx:&egui::Context,view:View) {ctx.data_mut(|d|d.insert_temp(egui::Id::new(VIEW),view));}
fn current(ctx:&egui::Context)->View {ctx.data(|d|d.get_temp::<View>(egui::Id::new(VIEW))).unwrap_or_default()}

pub fn control(ui:&mut egui::Ui,editing:bool,intents:&mut Vec<Intent>) {
    let state=current(ui.ctx());
    let owned=state.owned();
    let caption=if state.stopping {"Stopping virtual camera…"}else if state.starting {"Cancel Virtual Camera Start"}else if state.active {"Stop Virtual Camera"}else{"Start Virtual Camera"};
    let help=if !state.available {"Virtual camera requires Windows 11 x64 (build 22000 or later)."}
        else if owned {"Stop the virtual camera independently of recording and streaming."}
        else{"Send the selected video output to the installed SkyLive virtual camera. Audio stays separate."};
    ui.horizontal(|ui| {
        let width=(ui.available_width()-ui.spacing().item_spacing.x-24.0).max(1.0);
        let button=ui.add_enabled_ui(state.available&&!state.stopping&&(editing||owned),|ui| {
            ui.add_sized([width,24.0],egui::Button::new(crate::i18n::tr(ui,caption)).truncate())
        }).inner.on_hover_text(crate::i18n::tr(ui,help)).on_disabled_hover_text(crate::i18n::tr(ui,help));
        if let Some(problem)=&state.problem {let _=button.clone().on_hover_text(problem);}
        if button.clicked() {intents.push(if owned {Intent::StopVirtualCamera}else{Intent::StartVirtualCamera});}
        ui.scope(|ui| {
            ui.spacing_mut().interact_size=egui::vec2(24.0,24.0);
            if super::dock_toolbar::button(ui,"Virtual Camera","Virtual Camera Settings",super::dock_toolbar::Icon::Settings,state.available&&editing).clicked() {
                intents.push(Intent::OpenVirtualCameraSettings);
            }
        });
    });
}

pub fn settings(ctx:&egui::Context,draft:&mut Draft,view:&View,busy:bool)->Vec<Intent> {
    let mut intents=Vec::new();
    let response=egui::Modal::new(egui::Id::new("virtual-camera-settings")).show(ctx,|ui| {
        ui.set_width(520.0_f32.min((ctx.screen_rect().width()-48.0).max(120.0)));
        ui.heading(crate::i18n::tr(ui,"Virtual Camera Settings"));
        let actions=[
            Action::new("Apply",view.available&&!view.owned()&&!busy&&valid_route(draft),"Use this output route and camera profile on the next start."),
            Action::new("Close",true,"Close virtual-camera settings without applying the draft."),
        ];
        let height=(ctx.screen_rect().height()-modal_footer::required_height(ui,&actions)-112.0).max(64.0);
        egui::ScrollArea::vertical().id_salt("virtual-camera-settings-body").max_height(height).auto_shrink([false,true]).show(ui,|ui| {
            ui.style_mut().wrap_mode=Some(egui::TextWrapMode::Wrap);
            route_picker(ui,draft,!view.owned()&&!busy);
            ui.label(crate::i18n::tr(ui,"The selected video uses the global Video output settings, then fits the camera format with black borders when needed."));
            ui.label(crate::i18n::tr(ui,"An individual source is fitted into the canvas after its filters. Scene and group sources retain their own filters."));
            if view.owned(){ui.label(crate::i18n::tr(ui,"Stop the virtual camera before changing its output or format."));}
            ui.separator();
            ui.label(crate::i18n::tr(ui,"Camera format"));
            ui.add_enabled_ui(!view.owned()&&!busy,|ui| {
                for profile in [Profile::Hd720p30,Profile::FullHd1080p30] {
                    ui.radio_value(&mut draft.profile,profile,crate::i18n::tr(ui,profile.label()))
                        .on_hover_text(crate::i18n::tr(ui,"Use this landscape resolution at 30 frames per second for the virtual camera. Other frame rates are not supported by this camera component."))
                        .on_disabled_hover_text(crate::i18n::tr(ui,"Stop the virtual camera before changing its format."));
                }
            });
            ui.label(crate::i18n::tr(ui,"This profile is kept for this application window. Apply does not start the camera."));
            ui.separator();
            let state=if view.stopping {"Stopping virtual camera…"}else if view.starting {"Starting virtual camera…"}
                else if view.active&&view.waiting_for_frame {"Waiting for a frame from the selected video output."}
                else if view.active&&view.consumer_active {"Virtual camera is active; a receiving application is connected."}
                else if view.active {"Virtual camera started; waiting for a receiving application."}
                else {"Virtual camera is stopped."};
            ui.label(crate::i18n::tr(ui,state));
            ui.label(crate::i18n::tf(ui,"Frames: {frames} · Source activations: {starts}",
                &[("{frames}",view.frames.to_string()),("{starts}",view.source_starts.to_string())]));
            if view.owned()&&ui.add_enabled(!view.stopping,egui::Button::new(crate::i18n::tr(ui,if view.starting {"Cancel Virtual Camera Start"}else{"Stop Virtual Camera"})).wrap())
                .on_hover_text(crate::i18n::tr(ui,"Stop the virtual camera independently of recording and streaming.")).clicked() {
                intents.push(Intent::StopVirtualCamera);
            }
            if let Some(problem)=&view.problem {ui.colored_label(ui.visuals().error_fg_color,problem);}
            ui.separator();
            ui.label(crate::i18n::tr(ui,"Windows 11 x64 (build 22000 or later) and a protected SkyLive virtual-camera installation are required."));
            if ui.add_enabled(view.available&&!view.owned()&&!busy,egui::Button::new(crate::i18n::tr(ui,"Install Virtual Camera…")))
                .on_hover_text(crate::i18n::tr(ui,"Install the bundled camera component. Windows will ask for administrator approval. This does not start the camera.")).clicked() {
                intents.push(Intent::InstallVirtualCamera);
            }
            ui.label(crate::i18n::tr(ui,"After starting, select SkyLive (Windows Virtual Camera) in the receiving application. Audio uses that application's separate microphone selection."));
        });
        ui.separator();
        match modal_footer::show(ui,&actions) {
            Some(0)=>intents.push(Intent::ApplyVirtualCameraSettings(Submission {token:draft.token,profile:draft.profile,route:draft.route})),
            Some(1)=>intents.push(Intent::CloseVirtualCameraSettings(draft.token)),_=>{},
        }
    });
    if response.should_close() {intents.push(Intent::CloseVirtualCameraSettings(draft.token));}
    intents
}

fn valid_route(draft:&Draft)->bool {
    match draft.route {
        VirtualCameraRoute::Program|VirtualCameraRoute::Preview=>true,
        VirtualCameraRoute::Scene {scene}=>draft.scenes.iter().any(|value|value.id==scene),
        VirtualCameraRoute::Source {source}=>draft.sources.iter().any(|value|value.id==source),
    }
}
fn route_picker(ui:&mut egui::Ui,draft:&mut Draft,enabled:bool) {
    ui.label(crate::i18n::tr(ui,"Video output"));
    ui.add_enabled_ui(enabled,|ui| {
        for (selected,label,help) in [
            (matches!(draft.route,VirtualCameraRoute::Program),"Program","Send the actual Program output, including current scene transitions."),
            (matches!(draft.route,VirtualCameraRoute::Preview),"Preview","Send the current Preview scene without Program transitions or uncommitted transform drafts."),
            (matches!(draft.route,VirtualCameraRoute::Scene {..}),"Scene","Send one selected scene even while Program and Preview show other scenes."),
            (matches!(draft.route,VirtualCameraRoute::Source {..}),"Source","Send one selected video source or group, fitted to the canvas after its filters."),
        ] {
            let available=match label {"Scene"=>!draft.scenes.is_empty(),"Source"=>!draft.sources.is_empty(),_=>true};
            if ui.add_enabled(available,egui::RadioButton::new(selected,crate::i18n::tr(ui,label)))
                .on_hover_text(crate::i18n::tr(ui,help))
                .on_disabled_hover_text(crate::i18n::tr(ui,if !enabled {"Stop the virtual camera before changing its output or format."}else{"No video target is available in this scene collection."})).clicked() {
                draft.route=match label {
                    "Program"=>VirtualCameraRoute::Program,"Preview"=>VirtualCameraRoute::Preview,
                    "Scene"=>if selected {draft.route}else{VirtualCameraRoute::Scene {scene:draft.scenes[0].id}},
                    _=>if selected {draft.route}else{VirtualCameraRoute::Source {source:draft.sources[0].id}},
                };
            }
        }
        let (selected,choices,label)=match draft.route {
            VirtualCameraRoute::Scene {scene}=>(Some(scene),&draft.scenes,"Scene"),
            VirtualCameraRoute::Source {source}=>(Some(source),&draft.sources,"Source"),
            _=>(None,&draft.scenes,"Scene"),
        };
        if let Some(selected)=selected {
            let title=choices.iter().find(|value|value.id==selected).map(|value|value.name.clone())
                .unwrap_or_else(||crate::i18n::tr(ui,"Selected target is unavailable"));
            let mut chosen=selected;
            egui::ComboBox::from_id_salt("virtual-camera-route-target").width(ui.available_width().min(420.0))
                .selected_text(title).show_ui(ui,|ui| {
                    for value in choices {ui.selectable_value(&mut chosen,value.id,&value.name)
                        .on_hover_text(crate::i18n::tr(ui,"Use this existing target for the virtual-camera video output."));}
                }).response.on_hover_text(crate::i18n::tr(ui,"Use this existing target for the virtual-camera video output."));
            draft.route=if label=="Scene"{VirtualCameraRoute::Scene {scene:chosen}}else{VirtualCameraRoute::Source {source:chosen}};
        }
    }).response.on_disabled_hover_text(crate::i18n::tr(ui,"Stop the virtual camera before changing its output or format."));
    if !valid_route(draft){ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,"Selected target is unavailable"));}
}
