//! Frontend-owned coordination; all rendering/COM/device work stays on host owners.
use super::*;
use skylive_host::virtual_camera::{Config,Event,Owner,RouteFactory};
use skylive_core::VirtualCameraRoute;
use views::virtual_camera::{self,Profile,Token,Draft,Submission,View};

pub(super) struct Runtime {
    owner:Owner,
    profile:Profile,
    route:VirtualCameraRoute,
    route_session:u64,
    route_factory:RouteFactory,
    config:Option<Config>,
    admitted:bool,
    stop_requested:bool,
    origin:MediaTime,
    last_tick:Option<u64>,
    resource_generation:u64,
    close_after:bool,
    sequence:u64,
    pub settings:Option<Draft>,
    problem:Option<String>,
}
impl Runtime {
    pub(super) fn hotkey_intent(&self,action:skylive_core::HotkeyAction)->Option<Intent>{
        use skylive_core::HotkeyAction as Action;let status=self.owner.status();match action{
            Action::StartVirtualCamera=>(!self.owned()).then_some(Intent::StartVirtualCamera),
            Action::StopVirtualCamera=>(status.active&&!status.starting&&!status.stopping&&!self.stop_requested).then_some(Intent::StopVirtualCamera),_=>None,
        }
    }

    pub fn new(gpu:skylive_host::render_gpu::RenderGpu)->std::io::Result<Self> {
        Ok(Self {owner:Owner::new(gpu)?,profile:Profile::default(),route:VirtualCameraRoute::Program,route_session:0,route_factory:RouteFactory::default(),config:None,admitted:false,stop_requested:false,
            origin:MediaTime(0),last_tick:None,resource_generation:0,close_after:false,
            sequence:0,settings:None,problem:None})
    }
    pub fn owned(&self)->bool {
        let status=self.owner.status();self.admitted||status.active||status.starting||status.stopping
    }
    pub fn settings_open(&self)->bool {self.settings.is_some()}
    pub fn statistics(&self)->(u64,bool,u64) {
        let state=self.owner.status();(state.frames,state.active,self.origin.0)
    }
    pub fn close(&mut self) {self.settings=None;self.owner.close();}
    pub fn invalidate_resources(&mut self)->Result<(),String> {
        self.resource_generation=self.resource_generation.checked_add(1).ok_or("Virtual-camera resource generation exhausted")?;Ok(())
    }
    fn view(&self)->View {
        let status=self.owner.status();
        View {available:cfg!(all(target_os="windows",target_arch="x86_64")),active:status.active,
            starting:!self.stop_requested&&(status.starting||(self.admitted&&!status.active&&!status.stopping)),stopping:status.stopping||self.stop_requested,
            frames:status.frames,source_starts:status.source_starts,consumer_active:status.consumer_active,waiting_for_frame:status.waiting_for_frame,
            problem:self.problem.clone().or_else(||status.problem.map(|p|p.to_string()))}
    }
}
impl StudioApp {
    pub(super) fn start_virtual_camera(&mut self,ctx:&egui::Context) {
        if self.virtual_camera.owned() {return;}
        if !cfg!(all(target_os="windows",target_arch="x86_64")) {self.message(true,"Virtual camera requires Windows 11 x64 (build 22000 or later).");return;}
        let (width,height,fps)=self.virtual_camera.profile.dimensions();
        if matches!(self.virtual_camera.route,VirtualCameraRoute::Scene {..}|VirtualCameraRoute::Source {..}) && self.virtual_camera.route_session!=self.session.identity() {
            self.message(true,"Choose a virtual-camera output for the current scene collection.");return;
        }
        let config=match Config::for_route(&self.session,self.virtual_camera.route,width,height,fps) {Ok(c)=>c,Err(error)=>{self.message(true,error.to_string());return;}};
        match self.virtual_camera.owner.start(config.clone()) {
            Ok(())=>{
                self.virtual_camera.config=Some(config);self.virtual_camera.admitted=true;self.virtual_camera.stop_requested=false;
                self.virtual_camera.origin=self.frame_now;self.virtual_camera.last_tick=None;
                self.virtual_camera.problem=None;self.message(false,"Starting virtual camera…");
                ctx.request_repaint();
            },
            Err(error)=>{self.virtual_camera.problem=Some(error.to_string());self.message(true,error.to_string());},
        }
    }
    pub(super) fn stop_virtual_camera(&mut self) {
        if self.virtual_camera.owned() {
            self.virtual_camera.stop_requested=true;self.virtual_camera.owner.stop();self.message(false,"Stopping virtual camera…");
        }
    }
    pub(super) fn poll_virtual_camera(&mut self,ctx:&egui::Context) {
        for _ in 0..4 {
            match self.virtual_camera.owner.poll() {
                Some(Event::Started)=>if !self.virtual_camera.stop_requested {self.message(false,"Virtual camera started.");},
                Some(Event::Stopped)=>{
                    self.virtual_camera.admitted=false;self.virtual_camera.stop_requested=false;self.virtual_camera.config=None;self.virtual_camera.last_tick=None;
                    if let Some(problem)=self.virtual_camera.problem.clone() {self.message(true,problem);}
                    else {self.message(false,"Virtual camera is stopped.");}
                },
                Some(Event::Failed(error))=>{
                    self.virtual_camera.admitted=false;self.virtual_camera.stop_requested=false;self.virtual_camera.config=None;self.virtual_camera.last_tick=None;
                    self.virtual_camera.problem=Some(error.to_string());self.message(true,error.to_string());
                },
                None=>break,
            }
        }
        virtual_camera::install(ctx,self.virtual_camera.view());
        if self.virtual_camera.owned() {ctx.request_repaint_after(std::time::Duration::from_millis(10));}
        else if self.virtual_camera.close_after {self.virtual_camera.close_after=false;self.shutdown(ctx);}
    }
    pub(super) fn wait_for_virtual_camera_before_close(&mut self,ctx:&egui::Context)->bool {
        if !self.virtual_camera.owned() {return false;}
        self.virtual_camera.close_after=true;self.stop_virtual_camera();
        ctx.request_repaint_after(std::time::Duration::from_millis(10));true
    }
    pub(super) fn virtual_camera_frame(&mut self,ctx:&egui::Context,sample:&skylive_host::playback::PlaybackSnapshot,
        output:&skylive_host::transition::FrameComposition,capture:&Arc<CaptureSnapshot>) {
        let status=self.virtual_camera.owner.status();
        if !status.active||status.starting||status.stopping||self.virtual_camera.stop_requested {return;}
        let Some(config)=self.virtual_camera.config.as_ref() else {return;};
        if config.session()!=self.session.identity() {self.stop_virtual_camera();self.message(true,"Virtual-camera document changed; output was stopped.");return;}
        if sample.session()!=self.session.identity()||sample.revision()!=self.session.snapshot().revision()||sample.now()!=self.frame_now {
            ctx.request_repaint();return;
        }
        let Some(elapsed)=self.frame_now.0.checked_sub(self.virtual_camera.origin.0) else {
            self.stop_virtual_camera();self.message(true,"Virtual-camera clock moved backwards");return;
        };
        let fps=config.fps();
        let tick=(u128::from(elapsed)*u128::from(fps)/1_000_000_000) as u64;
        if self.virtual_camera.last_tick.is_some_and(|last|tick<=last) {return;}
        let frame=match self.virtual_camera.route_factory.frame(&self.session,config.route(),output,sample,capture.clone(),self.virtual_camera.resource_generation) {
            Ok(frame)=>frame,Err(error)=>{self.virtual_camera.problem=Some(error.to_string());self.stop_virtual_camera();self.message(true,error.to_string());return;}
        };
        match self.virtual_camera.owner.routed_video(frame) {
            Ok(())=>self.virtual_camera.last_tick=Some(tick),
            Err(error) if error.code()=="virtual_camera_backpressure"=>{},
            Err(error)=>{
                self.virtual_camera.problem=Some(error.to_string());self.stop_virtual_camera();self.message(true,error.to_string());
            },
        }
        ctx.request_repaint_after(std::time::Duration::from_nanos(1_000_000_000/u64::from(fps)));
    }
    /// Metadata activation view is built before camera/media/NDI sync, including
    /// native startup. It never adds this route to the Program audio union.
    pub(super) fn virtual_camera_output(&mut self,output:Option<&skylive_host::transition::FrameComposition>,
        capture:&CaptureSnapshot)->Option<skylive_host::transition::FrameComposition> {
        if !self.virtual_camera.owned()||self.virtual_camera.stop_requested{return None;}
        let Some(output)=output else {
            let message="Virtual-camera output is unavailable; output was stopped.";
            self.virtual_camera.problem=Some(message.into());self.stop_virtual_camera();self.message(true,message);return None;
        };
        let route=self.virtual_camera.config.as_ref()?.route();
        match self.virtual_camera.route_factory.composition(&self.session,route,output,capture) {
            Ok(output)=>Some(output),
            Err(error)=>{self.virtual_camera.problem=Some(error.to_string());self.stop_virtual_camera();self.message(true,error.to_string());None}
        }
    }
    pub(super) fn open_virtual_camera_settings(&mut self) {
        if self.dialog.is_some()||self.documents.modal_open()||self.setup.is_open()||self.close_dialog {return;}
        let Some(nonce)=self.virtual_camera.sequence.checked_add(1) else {self.message(true,"Virtual-camera settings identity exhausted");return;};
        self.virtual_camera.sequence=nonce;
        let model=self.session.snapshot();
        let scenes=model.scenes().iter().map(|scene|virtual_camera::Choice {id:scene.id(),name:scene.name().to_owned()}).collect();
        let sources=model.sources().iter().filter(|source|model.source_video_dimensions(source.kind()).is_ok_and(|size|size.is_some()))
            .map(|source|virtual_camera::Choice {id:source.id(),name:source.name().to_owned()}).collect();
        let route=if self.virtual_camera.route_session!=self.session.identity()&&matches!(self.virtual_camera.route,VirtualCameraRoute::Scene {..}|VirtualCameraRoute::Source {..}) {
            VirtualCameraRoute::Program
        }else{self.virtual_camera.route};
        self.virtual_camera.settings=Some(Draft {token:Token {session:self.session.identity(),revision:model.revision(),nonce},profile:self.virtual_camera.profile,route,scenes,sources});
    }
    pub(super) fn apply_virtual_camera_settings(&mut self,submission:Submission) {
        let current=self.virtual_camera.settings.as_ref().is_some_and(|d|d.token==submission.token&&d.profile==submission.profile&&d.route==submission.route);
        if !current||submission.token.session!=self.session.identity()||submission.token.revision!=self.session.snapshot().revision()||self.session.is_closed() {
            self.message(true,"Virtual-camera settings are stale; reopen Settings.");return;
        }
        if self.virtual_camera.owned() {self.message(true,"Stop the virtual camera before changing its output or format.");return;}
        let (width,height,fps)=submission.profile.dimensions();
        if let Err(error)=Config::for_route(&self.session,submission.route,width,height,fps) {self.message(true,error.to_string());return;}
        self.virtual_camera.profile=submission.profile;self.virtual_camera.route=submission.route;
        self.virtual_camera.route_session=self.session.identity();self.virtual_camera.route_factory=RouteFactory::default();self.virtual_camera.settings=None;
        self.message(false,"Virtual-camera output and profile updated for the next start.");
    }
    pub(super) fn close_virtual_camera_settings(&mut self,token:Token) {
        if self.virtual_camera.settings.as_ref().is_some_and(|d|d.token==token) {self.virtual_camera.settings=None;}
    }
    pub(super) fn show_virtual_camera_settings(&mut self,ctx:&egui::Context) {
        let view=self.virtual_camera.view();
        let Some(draft)=self.virtual_camera.settings.as_mut() else {return;};
        let intents=virtual_camera::settings(ctx,draft,&view,self.io.busy());
        for intent in intents {self.intent(intent,ctx);}
    }
}
