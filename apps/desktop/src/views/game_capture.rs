//! Real D3D11 process picker. Apply is the explicit Session-only injection grant.
use eframe::egui;
use skylive_core::{Command,GameCaptureBinding,Id};
use skylive_host::game_capture::{GameTargetInfo,GameFrame};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub enum Edit {Add {scene:Id},Rebind {source:Id}}
#[derive(Clone)]pub struct Apply {pub token:u64,pub session:u64,pub revision:u64,pub edit:Edit,pub name:String,pub binding:GameCaptureBinding}
pub struct Draft {pub token:u64,pub session:u64,pub revision:u64,pub edit:Edit,pub name:String,pub targets:Vec<GameTargetInfo>,
    pub selected:Option<GameTargetInfo>,pub any_fullscreen:bool,pub hotkey_mode:bool,pub max_fps:u32,pub capture_cursor:bool,pub allow_transparency:bool,pub search:String,pub loading:bool,pub error:Option<String>,pub observed:Option<GameFrame>,original:Option<GameCaptureBinding>}
impl Draft {
    pub fn add(scene:Id,name:String)->Self{Self {token:0,session:0,revision:0,edit:Edit::Add {scene},name,targets:Vec::new(),selected:None,any_fullscreen:true,hotkey_mode:false,max_fps:60,capture_cursor:true,allow_transparency:false,search:String::new(),loading:false,error:None,observed:None,original:None}}
    pub fn rebind(source:Id,binding:&GameCaptureBinding)->Self{let mut draft=Self::add(source,String::new());draft.edit=Edit::Rebind {source};draft.max_fps=binding.max_fps();
        draft.any_fullscreen=binding.obs_selector().is_some_and(|s|s.any_fullscreen);draft.hotkey_mode=binding.obs_selector().is_some_and(|s|s.hotkey_mode);draft.capture_cursor=binding.capture_cursor();draft.allow_transparency=binding.allow_transparency();draft.selected=binding.target().map(|target|GameTargetInfo {target:target.clone(),width:binding.width(),height:binding.height()});draft.original=Some(binding.clone());draft}
    pub fn title(&self)->&'static str{if matches!(self.edit,Edit::Add {..}){"Add Game Capture"}else{"Properties: Game Capture"}}
    fn binding(&self)->Result<GameCaptureBinding,&'static str>{
        if self.any_fullscreen||self.hotkey_mode {
            let (width,height)=self.original.as_ref().map(|b|(b.width(),b.height())).unwrap_or((1920,1080));
            let window=self.original.as_ref().and_then(GameCaptureBinding::obs_selector).map(|s|s.window.clone()).unwrap_or_default();
            let priority=self.original.as_ref().and_then(GameCaptureBinding::obs_selector).map_or(2,|s|s.priority);
            let selector=skylive_core::ObsGameSelector {window,priority,any_fullscreen:self.any_fullscreen,hotkey_mode:self.hotkey_mode};
            return GameCaptureBinding::unresolved_obs(selector,width,height,self.max_fps).map(|value|value.with_capture_cursor(self.capture_cursor).with_allow_transparency(self.allow_transparency)).map_err(|_|"Selected game target or capture rate is invalid.");
        }
        if self.selected.is_none(){if let Some(original)=self.original.as_ref().filter(|binding|binding.target().is_none()&&binding.obs_selector().is_some_and(|s|!s.any_fullscreen&&!s.hotkey_mode)){
            return original.clone().with_max_fps(self.max_fps).map(|value|value.with_capture_cursor(self.capture_cursor).with_allow_transparency(self.allow_transparency)).map_err(|_|"Selected game target or capture rate is invalid.");
        }}
        let selected=self.selected.as_ref().ok_or("Choose an actual game window first.")?;
        let value=GameCaptureBinding::new(selected.target.clone(),selected.width,selected.height,self.max_fps).map_err(|_|"Selected game target or capture rate is invalid.")?.with_capture_cursor(self.capture_cursor).with_allow_transparency(self.allow_transparency);value.validate().map_err(|_|"Selected game target or capture rate is invalid.")?;Ok(value)}
    pub fn request(&self)->Result<Apply,&'static str>{
        if self.loading{return Err("Wait for game-window discovery to finish.");}
        if matches!(self.edit,Edit::Add {..})&&(self.name.trim().is_empty()||self.name.len()>256||self.name.chars().any(char::is_control)){return Err("Source name requires 1..256 UTF-8 bytes without controls.");}
        Ok(Apply {token:self.token,session:self.session,revision:self.revision,edit:self.edit,name:self.name.clone(),binding:self.binding()?})
    }
    pub fn matches(&self,request:&Apply)->bool{self.token==request.token&&self.session==request.session&&self.revision==request.revision&&self.edit==request.edit&&self.name==request.name&&self.binding().is_ok_and(|binding|binding==request.binding)}
}
impl Apply {pub fn command(&self)->Command{match self.edit {Edit::Add {scene}=>Command::AddGameCapture {scene,name:self.name.clone(),binding:self.binding.clone()},Edit::Rebind {source}=>Command::SetGameCapture {source,binding:self.binding.clone()}}}}
pub fn editor(ui:&mut egui::Ui,draft:&mut Draft,intents:&mut Vec<super::Intent>){
    ui.label(crate::i18n::tr_key_or(ui,"SkyLive.GameCapture.FullscreenHelp","Capture the foreground fullscreen application or select a Windows x64 D3D11 game window."));
    ui.small(crate::i18n::tr_key_or(ui,"SkyLive.GameCapture.D3d11Limits","D3D11 Present/Present1, SDR RGBA/BGRA8 only. DX12, Vulkan, OpenGL and anti-cheat bypass are unavailable."));
    ui.small(crate::i18n::tr(ui,"Single-threaded D3D11 devices and custom swap-chain implementations are unsupported."));
    if let Some(selector)=draft.original.as_ref().and_then(GameCaptureBinding::obs_selector) {
        if !selector.any_fullscreen&&!selector.hotkey_mode {ui.label(format!("OBS window selector: {}",selector.window));}
        if draft.selected.is_none()&&!selector.any_fullscreen&&!selector.hotkey_mode{ui.small(crate::i18n::tr_key_or(ui,"SkyLive.GameCapture.SelectorAuto","The imported OBS window selector is searched automatically while this source is visible."));}
    }
    ui.add_enabled_ui(!draft.loading,|ui|{
        if matches!(draft.edit,Edit::Add {..}){let label=ui.label(crate::i18n::tr(ui,"Name"));ui.add(egui::TextEdit::singleline(&mut draft.name).char_limit(256).desired_width(ui.available_width())).labelled_by(label.id).on_hover_text(crate::i18n::tr(ui,"Name this game source in the scene."));}
        let mut mode=if draft.hotkey_mode {2} else if draft.any_fullscreen {0} else {1};
        ui.vertical(|ui|{
            ui.radio_value(&mut mode,0,crate::i18n::tr_key_or(ui,"Module.win-capture.GameCapture.AnyFullscreen","Capture any fullscreen application"));
            ui.radio_value(&mut mode,1,crate::i18n::tr_key_or(ui,"Module.win-capture.GameCapture.CaptureWindow","Capture specific window"));
            ui.radio_value(&mut mode,2,crate::i18n::tr_key_or(ui,"Module.win-capture.GameCapture.UseHotkey","Capture foreground window with hotkey"));
        });
        draft.any_fullscreen=mode==0;draft.hotkey_mode=mode==2;
        if draft.hotkey_mode {ui.small(crate::i18n::tr(ui,"Configure Capture Start and Capture Stop in this source’s Playback Hotkeys."));}
        ui.add_enabled_ui(!draft.any_fullscreen&&!draft.hotkey_mode,|ui|{
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
        });
        ui.label(crate::i18n::tr(ui,"Capture frame-rate limit"));ui.horizontal_wrapped(|ui|{
            for fps in [30,60,120]{ui.radio_value(&mut draft.max_fps,fps,crate::i18n::tf(ui,"{fps} fps",&[("{fps}",fps.to_string())])).on_hover_text(crate::i18n::tr(ui,"Limit captured frames without changing the game's own frame rate."));}
        });
        ui.checkbox(&mut draft.capture_cursor,crate::i18n::tr(ui,"Capture Cursor")).on_hover_text(crate::i18n::tr_key_or(ui,"SkyLive.GameCapture.CursorHelp","Include the system cursor when the selected game is in the foreground."));
        ui.checkbox(&mut draft.allow_transparency,crate::i18n::tr_key_or(ui,"Module.win-capture.AllowTransparency","Allow Transparency"));
        if let Edit::Rebind {source}=draft.edit {
            let unchanged=!draft.hotkey_mode&&draft.binding().is_ok_and(|binding|draft.original.as_ref()==Some(&binding));
            ui.horizontal_wrapped(|ui|{
                if ui.add_enabled(unchanged,egui::Button::new(crate::i18n::tr(ui,"Start Game Capture"))).on_hover_text(crate::i18n::tr(ui,"Enable the saved exact target for this session. Apply a different target before starting it.")).on_disabled_hover_text(crate::i18n::tr(ui,"Enable the saved exact target for this session. Apply a different target before starting it.")).clicked(){intents.push(super::Intent::StartGameCapture {session:draft.session,revision:draft.revision,source});}
                if ui.button(crate::i18n::tr(ui,"Stop Game Capture")).on_hover_text(crate::i18n::tr(ui,"Revoke this process's capture permission for the current session. Other output and audio owners remain independent.")).clicked(){intents.push(super::Intent::StopGameCapture {session:draft.session,revision:draft.revision,source});}
            });
        }
    });
    if draft.observed.is_none()&&matches!(draft.edit,Edit::Rebind {..}){ui.label(crate::i18n::tr(ui,"No captured game frame is available yet."));}
    if let Some(frame)=&draft.observed {ui.label(crate::i18n::tf(ui,"Captured game frame: {width} × {height} · dropped {dropped}",&[("{width}",frame.image.width().to_string()),("{height}",frame.image.height().to_string()),("{dropped}",frame.dropped.to_string())]));}
    ui.small(crate::i18n::tr_key_or(ui,"SkyLive.GameCapture.SelectorLifecycle","Automatic selectors find matching visible games. The hook remains loaded until the game exits; at most two processes are admitted."));
    ui.small(crate::i18n::tr(ui,"Initial window size is only the transform basis. Captured backbuffers retain their actual dimensions, up to4096-edge/8.8M pixels; they are never replaced by Window Capture."));
    if draft.loading{ui.horizontal_wrapped(|ui|{ui.spinner();ui.label(crate::i18n::tr(ui,"Discovering game windows…"));});}
    if let Some(error)=&draft.error{ui.colored_label(ui.visuals().error_fg_color,error);}
}
fn label_target(value:&GameTargetInfo)->String{format!("{} · {} · PID {}",value.target.title,value.target.executable,value.target.pid)}
