//! Desktop scheduler supplies immutable current Program/Preview inputs; workers
//! alone render, send, flush and release native resources. GUI never joins them.
use super::*;
use skylive_core::NdiOutputRoute;
use skylive_host::{ndi::output::{OutputOwner,Event,Key},audio_output::AudioOutput,transition::FrameComposition};
use views::ndi_output::{self,Action,Token,Draft,View};
pub(super) struct Runtime {pub owner:OutputOwner,pub settings:Option<Draft>,sequence:u64,ready:[Option<Key>;2],origins:[Option<MediaTime>;2],last_ticks:[Option<u64>;2],close_after:bool}
impl Runtime {
    pub fn new(gpu:skylive_host::render_gpu::RenderGpu)->Self{Self {owner:OutputOwner::new(gpu),settings:None,sequence:0,ready:[None,None],origins:[None,None],last_ticks:[None,None],close_after:false}}
    pub fn needs_audio(&self)->bool{self.owner.active(NdiOutputRoute::Program)&&self.owner.config(NdiOutputRoute::Program).is_some_and(|c|c.audio.is_some())&&!self.owner.status(NdiOutputRoute::Program).stopping}
    pub fn close(&mut self){self.settings=None;self.owner.close();}
}
impl StudioApp {
    pub(super) fn open_ndi_outputs(&mut self){
        let model=self.session.snapshot();if !model.preferences().plugin_enabled(skylive_core::NDI_PLUGIN){self.message(true,"Enable the DistroAV module before starting NDI output");return;}
        let Some(generation)=self.ndi_output.sequence.checked_add(1)else{self.message(true,"NDI settings identity exhausted");return;};self.ndi_output.sequence=generation;
        let token=Token {session:self.session.identity(),revision:model.revision(),generation};self.ndi_output.settings=Some(Draft::new(token,model.preferences().ndi_output()));self.dialog=None;
    }
    fn ndi_token(&self,token:Token)->bool{self.ndi_output.settings.as_ref().is_some_and(|d|d.token==token)&&token.session==self.session.identity()&&token.revision==self.session.snapshot().revision()}
    pub(super) fn ndi_output_action(&mut self,action:Action){match action {
        Action::Close(token)=>{if self.ndi_output.settings.as_ref().is_some_and(|d|d.token==token){self.ndi_output.settings=None;}},
        Action::Stop(key)=>{if self.ndi_output.owner.key(key.route)==Some(key){self.ndi_output.owner.stop(key.route);self.ndi_output.ready[key.route.index()]=None;}},
        Action::Apply {token,settings}=>{
            if !self.ndi_token(token)||self.ndi_output.owner.any_active(){self.message(true,"Stop NDI outputs and reopen current settings before applying");return;}
            if self.publish_command_at(Some(token.revision),Command::SetNdiOutputSettings {settings},false){let token=Token {revision:self.session.snapshot().revision(),..token};self.ndi_output.settings=Some(Draft::new(token,self.session.snapshot().preferences().ndi_output()));}
        },
        Action::Start {token,route}=>{
            if !self.ndi_token(token){self.message(true,"The document changed. Reopen NDI Output Settings before applying or starting.");return;}
            if self.ndi_output.settings.as_ref().and_then(|d|d.settings().ok()).as_ref()!=Some(self.session.snapshot().preferences().ndi_output()){self.message(true,"Apply NDI output settings before starting");return;}
            if self.ndi_output.owner.active(route){return;}if self.io.busy()||self.documents.modal_open(){self.message(true,"Finish the document operation before starting NDI output");return;}
            let audio=route==NdiOutputRoute::Program&&self.session.snapshot().preferences().ndi_output().program_audio();if audio&&self.audio.is_busy(){self.message(true,"Audio worker is busy; try NDI output again");return;}
            match self.ndi_output.owner.start(&self.session,route){Ok(sink)=>{
                let i=route.index();self.ndi_output.ready[i]=None;self.ndi_output.origins[i]=None;self.ndi_output.last_ticks[i]=None;
                if let Some(sink)=sink {let Some(state)=self.sync_transition()else{self.ndi_output.owner.stop(route);return;};if let Err(error)=self.audio.request_program_with_output(&self.session,&state.output,self.frame_now,AudioOutput::NdiProgram(sink)){self.ndi_output.owner.stop(route);self.message(true,error.to_string());}}
            },Err(error)=>self.message(true,crate::status::Status::formatted("NDI output: {error}",&[("{error}",error.to_string())]))}
        },
    }}
    pub(super) fn poll_ndi_outputs(&mut self,ctx:&egui::Context){
        self.ndi_output.owner.synchronize(&self.session);
        for _ in 0..8 {let Some(event)=self.ndi_output.owner.poll()else{break;};match event {
            Event::Ready {key} if key.session==self.session.identity()&&Some(key)==self.ndi_output.owner.key(key.route)=>self.ndi_output.ready[key.route.index()]=Some(key),
            Event::Sending {..}=>self.message(false,"NDI sender submitted its first video frame"),
            Event::Failed {error,..}=>self.message(true,crate::status::Status::formatted("NDI output: {error}",&[("{error}",error.to_string())])),
            Event::Stopped {key}=>{self.ndi_output.ready[key.route.index()]=None;self.ndi_output.origins[key.route.index()]=None;self.ndi_output.last_ticks[key.route.index()]=None;},_=>{},
        }}
        if self.ndi_output.owner.any_active(){ctx.request_repaint_after(std::time::Duration::from_millis(10));}else if self.ndi_output.close_after{self.ndi_output.close_after=false;self.shutdown(ctx);}
    }
    pub(super) fn ndi_preview_output(&mut self,program:Option<&FrameComposition>)->Option<FrameComposition>{
        if !self.ndi_output.owner.active(NdiOutputRoute::Preview){return None;}let config=self.ndi_output.owner.config(NdiOutputRoute::Preview)?;let program=program?;
        match config.composition(&self.session,program,self.studio){Ok(output)=>Some(output),Err(error)=>{self.ndi_output.owner.stop(NdiOutputRoute::Preview);self.message(true,error.to_string());None}}
    }
    pub(super) fn ndi_output_frames(&mut self,ctx:&egui::Context,playback:&skylive_host::playback::PlaybackSnapshot,program:&FrameComposition,preview:Option<&FrameComposition>,capture:&Arc<CaptureSnapshot>){
        let model=self.session.snapshot();if playback.session()!=self.session.identity()||playback.revision()!=model.revision()||playback.now()!=self.frame_now{return;}
        for route in [NdiOutputRoute::Program,NdiOutputRoute::Preview]{let i=route.index();if !self.ndi_output.owner.active(route)||self.ndi_output.owner.status(route).stopping{continue;}
            if let Some(key)=self.ndi_output.ready[i].take(){match self.ndi_output.owner.activate(key,self.frame_now){Ok(())=>self.ndi_output.origins[i]=Some(self.frame_now),Err(error)=>{self.ndi_output.owner.stop(route);self.message(true,error.to_string());continue;}}}
            let (Some(origin),Some(config),Some(key))=(self.ndi_output.origins[i],self.ndi_output.owner.config(route),self.ndi_output.owner.key(route))else{continue;};
            let Some(elapsed)=self.frame_now.0.checked_sub(origin.0)else{self.ndi_output.owner.stop(route);continue;};let tick=(u128::from(elapsed)*u128::from(config.video.fps_num())/(1_000_000_000u128*u128::from(config.video.fps_den()))) as u64;if self.ndi_output.last_ticks[i].is_some_and(|t|tick<=t){continue;}
            let output=if route==NdiOutputRoute::Program{program}else{let Some(output)=preview else{continue;};output};
            let frame=skylive_host::recording::RecordingFrame {session:self.session.identity(),time:self.frame_now,model:model.clone(),output:output.clone(),playback:playback.clone(),capture:capture.clone(),resource_generation:self.preview.resource_generation()};
            match self.ndi_output.owner.submit(key,frame){Ok(())=>self.ndi_output.last_ticks[i]=Some(tick),Err((skylive_host::ndi::NdiError::Busy,_))=>{},Err((error,_))=>{self.ndi_output.owner.stop(route);self.message(true,error.to_string());}}
        }if self.ndi_output.owner.any_active(){ctx.request_repaint_after(std::time::Duration::from_millis(5));}
    }
    pub(super) fn show_ndi_output_settings(&mut self,ctx:&egui::Context){
        let Some(draft)=&mut self.ndi_output.settings else{return;};let model=self.session.snapshot();let owner=&self.ndi_output.owner;
        let actions=ndi_output::show(ctx,draft,&model,View {session:self.session.identity(),enabled:skylive_host::ndi::output::available()&&model.preferences().plugin_enabled(skylive_core::NDI_PLUGIN),editing:!self.io.busy()&&!self.documents.modal_open(),statuses:[owner.status(NdiOutputRoute::Program),owner.status(NdiOutputRoute::Preview)],keys:[owner.key(NdiOutputRoute::Program),owner.key(NdiOutputRoute::Preview)],owned:[owner.active(NdiOutputRoute::Program),owner.active(NdiOutputRoute::Preview)]});
        for action in actions{self.ndi_output_action(action);}
    }
    pub(super) fn wait_ndi_output_close(&mut self,ctx:&egui::Context)->bool{if !self.ndi_output.owner.any_active(){return false;}self.ndi_output.close_after=true;self.ndi_output.close();ctx.request_repaint_after(std::time::Duration::from_millis(10));true}
}
