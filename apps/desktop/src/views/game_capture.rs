//! Real D3D11 process picker. Apply is the explicit Session-only injection grant.
use eframe::egui;
use skylive_core::{Command,GameCaptureBinding,Id};
use skylive_host::game_capture::{GameTargetInfo,GameFrame};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub enum Edit {Add {scene:Id},Rebind {source:Id}}
#[derive(Clone)]pub struct Apply {pub token:u64,pub session:u64,pub revision:u64,pub edit:Edit,pub name:String,pub binding:GameCaptureBinding}
pub struct Draft {pub token:u64,pub session:u64,pub revision:u64,pub edit:Edit,pub name:String,pub targets:Vec<GameTargetInfo>,
    pub selected:Option<GameTargetInfo>,pub max_fps:u32,pub search:String,pub loading:bool,pub error:Option<String>,pub observed:Option<GameFrame>,original:Option<GameCaptureBinding>}
impl Draft {
    pub fn add(scene:Id,name:String)->Self{Self {token:0,session:0,revision:0,edit:Edit::Add {scene},name,targets:Vec::new(),selected:None,max_fps:60,search:String::new(),loading:false,error:None,observed:None,original:None}}
    pub fn rebind(source:Id,binding:&GameCaptureBinding)->Self{let mut draft=Self::add(source,String::new());draft.edit=Edit::Rebind {source};draft.max_fps=binding.max_fps();
        draft.selected=Some(GameTargetInfo {target:binding.target().clone(),width:binding.width(),height:binding.height()});draft.original=Some(binding.clone());draft}
    pub fn title(&self)->&'static str{if matches!(self.edit,Edit::Add {..}){"Add Game Capture"}else{"Properties: Game Capture"}}
    fn binding(&self)->Result<GameCaptureBinding,&'static str>{let selected=self.selected.as_ref().ok_or("Choose an actual game window first.")?;
        GameCaptureBinding::new(selected.target.clone(),selected.width,selected.height,self.max_fps).map_err(|_|"Selected game target or capture rate is invalid.")}
    pub fn request(&self)->Result<Apply,&'static str>{
        if self.loading{return Err("Wait for game-window discovery to finish.");}
        if matches!(self.edit,Edit::Add {..})&&(self.name.trim().is_empty()||self.name.len()>256||self.name.chars().any(char::is_control)){return Err("Source name requires 1..256 UTF-8 bytes without controls.");}
        Ok(Apply {token:self.token,session:self.session,revision:self.revision,edit:self.edit,name:self.name.clone(),binding:self.binding()?})
    }
    pub fn matches(&self,request:&Apply)->bool{self.token==request.token&&self.session==request.session&&self.revision==request.revision&&self.edit==request.edit&&self.name==request.name&&self.binding().is_ok_and(|binding|binding==request.binding)}
}
impl Apply {pub fn command(&self)->Command{match self.edit {Edit::Add {scene}=>Command::AddGameCapture {scene,name:self.name.clone(),binding:self.binding.clone()},Edit::Rebind {source}=>Command::SetGameCapture {source,binding:self.binding.clone()}}}}
pub fn editor(ui:&mut egui::Ui,draft:&mut Draft,intents:&mut Vec<super::Intent>){
    ui.label(crate::i18n::tr(ui,"Captures a selected Windows x64 D3D11 game through the bundled Rust hook. Apply or Start enables this process for this session."));
    ui.small(crate::i18n::tr(ui,"D3D11 Present/Present1, opaque SDR RGBA/BGRA8 only. DX12, Vulkan, OpenGL, anti-cheat bypass and cursor capture are unavailable."));
    ui.small(crate::i18n::tr(ui,"Single-threaded D3D11 devices and custom swap-chain implementations are unsupported."));
    ui.add_enabled_ui(!draft.loading,|ui|{
        if matches!(draft.edit,Edit::Add {..}){let label=ui.label(crate::i18n::tr(ui,"Name"));ui.add(egui::TextEdit::singleline(&mut draft.name).char_limit(256).desired_width(ui.available_width())).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Name this game source in the scene."));}
        if ui.button(crate::i18n::tr(ui,"Refresh game windows")).on_hover_text(crate::i18n::tr(ui,"List real visible game windows and process identities. Discovery does not inject a hook.")).clicked(){intents.push(super::Intent::DiscoverGames {token:draft.token,session:draft.session,revision:draft.revision});}
        let label=ui.label(crate::i18n::tr(ui,"Search game windows"));ui.add(egui::TextEdit::singleline(&mut draft.search).desired_width(ui.available_width())).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Filter discovered windows by title, executable or PID."));
        let label=ui.label(crate::i18n::tr(ui,"Game window and process"));
        let caption=draft.selected.as_ref().map(label_target).unwrap_or_else(||crate::i18n::tr(ui,"Choose an actual game window first."));
        let search=draft.search.to_lowercase();
        egui::ComboBox::from_id_salt("game-target-picker").width(ui.available_width()).height(240.0).truncate().selected_text(caption).show_ui(ui,|ui|{
            for target in &draft.targets {let label=label_target(target);if !search.is_empty()&&!label.to_lowercase().contains(&search){continue;}
                ui.selectable_value(&mut draft.selected,Some(target.clone()),label)
                    .on_hover_text(format!("{}\nPID {} · creation {} · HWND {}\n{}",target.target.executable,target.target.pid,target.target.creation_time,target.target.hwnd,target.target.class));
            }
        }).response.labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Select the exact running process and window. PID reuse or a different executable cannot rebind this source."));
        ui.label(crate::i18n::tr(ui,"Capture frame-rate limit"));ui.horizontal_wrapped(|ui|{
            for fps in [30,60,120]{ui.radio_value(&mut draft.max_fps,fps,crate::i18n::tf(ui,"{fps} fps",&[("{fps}",fps.to_string())])).on_hover_text(crate::i18n::tr(ui,"Limit captured frames without changing the game's own frame rate."));}
        });
        if let Edit::Rebind {source}=draft.edit {
            let unchanged=draft.binding().is_ok_and(|binding|draft.original.as_ref()==Some(&binding));
            ui.horizontal_wrapped(|ui|{
                if ui.add_enabled(unchanged,egui::Button::new(crate::i18n::tr(ui,"Start Game Capture"))).on_hover_text(crate::i18n::tr(ui,"Enable the saved exact target for this session. Apply a different target before starting it.")).on_disabled_hover_text(crate::i18n::tr(ui,"Enable the saved exact target for this session. Apply a different target before starting it.")).clicked(){intents.push(super::Intent::StartGameCapture {session:draft.session,revision:draft.revision,source});}
                if ui.button(crate::i18n::tr(ui,"Stop Game Capture")).on_hover_text(crate::i18n::tr(ui,"Revoke this process's capture permission for the current session. Other output and audio owners remain independent.")).clicked(){intents.push(super::Intent::StopGameCapture {session:draft.session,revision:draft.revision,source});}
            });
        }
    });
    if draft.observed.is_none()&&matches!(draft.edit,Edit::Rebind {..}){ui.label(crate::i18n::tr(ui,"No captured game frame is available yet."));}
    if let Some(frame)=&draft.observed {ui.label(crate::i18n::tf(ui,"Captured game frame: {width} × {height} · dropped {dropped}",&[("{width}",frame.image.width().to_string()),("{height}",frame.image.height().to_string()),("{dropped}",frame.dropped.to_string())]));}
    ui.small(crate::i18n::tr(ui,"Opening a collection does not inject. The hook code remains loaded until the selected game exits; at most two target processes are admitted."));
    ui.small(crate::i18n::tr(ui,"Initial window size is only the transform basis. Captured backbuffers retain their actual dimensions, up to4096-edge/8.8M pixels; they are never replaced by Window Capture."));
    if draft.loading{ui.horizontal_wrapped(|ui|{ui.spinner();ui.label(crate::i18n::tr(ui,"Discovering game windows…"));});}
    if let Some(error)=&draft.error{ui.colored_label(ui.visuals().error_fg_color,error);}
}
fn label_target(value:&GameTargetInfo)->String{format!("{} · {} · PID {}",value.target.title,value.target.executable,value.target.pid)}
