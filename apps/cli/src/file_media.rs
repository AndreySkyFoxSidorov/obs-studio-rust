//! File playback controls; ordinary author/save/capabilities never open media.
use super::*;
use skylive_host::media_source::{MediaEvent, MediaOwner, MediaSourceError, PreparedMedia};
use std::time::{Duration, Instant};
struct ReloadReceipt{key:skylive_host::media_source::MediaRequestKey,target:skylive_host::media_source::MediaReloadTarget,completed:bool,status:Option<skylive_host::media_source::MediaTransportStatus>,problem:Option<String>}
pub(super) struct Runtime {
    reload:Option<ReloadReceipt>,
    pub documents:crate::collection_library::State,
    pub remux:skylive_host::remux::Owner,
    pub camera_activation:skylive_host::camera_activation::Owner,
    pub outputs:crate::ndi_output::Runtime,
    pub browser_running:bool,
    pub browser:skylive_host::browser::BrowserOwner,
    pub browser_union:capture::BrowserCaptureUnion,
    pub game:skylive_host::game_capture::GameOwner,
    pub game_union:capture::GameCaptureUnion,
    pub ndi_running:bool,
    pub ndi:skylive_host::ndi::NdiOwner,
    pub ndi_union:capture::NdiCaptureUnion,
    pub owner: MediaOwner,
    pub union: capture::MediaCaptureUnion,
    pub running: bool,
}
impl Runtime {
    pub fn new(audio:&AudioCaptureWorker)->Result<Self,MediaSourceError>{Self::new_with_render_gpu(audio,skylive_host::render_gpu::RenderGpu::cpu())}
    pub fn new_with_render_gpu(audio: &AudioCaptureWorker,gpu:skylive_host::render_gpu::RenderGpu) -> Result<Self, MediaSourceError> {
        Ok(Self {
            reload:None,
            documents:Default::default(),
            remux:Default::default(),
            camera_activation:Default::default(),
            outputs:crate::ndi_output::Runtime::new(gpu),
            browser_running:false,browser:skylive_host::browser::BrowserOwner::new(audio.browser_sink()).map_err(MediaSourceError::Io)?,browser_union:Default::default(),
            game:skylive_host::game_capture::GameOwner::new().map_err(MediaSourceError::Io)?,game_union:Default::default(),
            ndi_running:false,
            ndi:skylive_host::ndi::NdiOwner::new(audio.ndi_sink()).map_err(MediaSourceError::Io)?,
            ndi_union:Default::default(),
            owner: MediaOwner::with_audio_sink(audio.media_sink())?,
            union: Default::default(),
            running: false,
        })
    }
    pub fn poll(&mut self, session: &Session) -> Result<(), CliError> {
        self.browser.poll(session)?;
        while self.game.poll(session)?.is_some(){}
        while self.ndi.poll(session)?.is_some(){}
        for _ in 0..4 {
            match self.owner.poll(session)? {
                Some(MediaEvent::Controlled { key,source,result }) => {
                    if let Some(receipt)=self.reload.as_mut().filter(|r|r.key==key&&r.target.source()==source){
                        receipt.completed=true;
                        if !receipt.target.matches(session){receipt.problem=Some("Media reload receipt belongs to a changed source definition".into());return Err("Media reload receipt belongs to a changed source definition".into());}
                        match &result{Ok(status)=>receipt.status=Some(*status),Err(error)=>receipt.problem=Some(error.to_string())}
                    }
                    result?;
                }
                None => break,
                _ => {}
            }
        }
        Ok(())
    }
    pub fn synchronize(
        &mut self,
        session: &Session,
        state: &TransitionSnapshot,
        audio: &AudioCaptureWorker,
    ) -> Result<(), CliError> {
        self.poll(session)?;
        let policy=self.camera_activation.snapshot();
        self.browser.set_camera_activation_policy(policy.clone());
        self.ndi.set_camera_activation_policy(policy.clone());
        self.owner.set_camera_activation_policy(policy);
        let mut views=state.output.scenes();if state.studio {views.push(FrameScene::Live(session.preview()));}let audio_views=views.clone();if self.outputs.owner.active(skylive_core::NdiOutputRoute::Preview)&&!state.studio{let extra=FrameScene::Live(state.output.primary_scene());if !views.iter().any(|v|v.key()==extra.key()){views.push(extra);}}
        if self.browser_running {self.browser.synchronize(session,&views,&state.output.scenes(),&audio_views,state.now,audio.program_key())?;}
        self.game.synchronize(session,&views,state.now)?;
        self.ndi.synchronize_video(session,if self.ndi_running {&views}else{&[]},&audio_views,state.now,audio.program_key())?;
        if self.running {
            // Publish changed scope to a deferred control before waiting for
            // its receipt; otherwise a busy owner could hide a scene change.
            if self.owner.is_busy() {
                self.owner.synchronize_video(session, &state.output, &views, state.now, audio.program_key())?;
            }
            self.settle(session)?;
            self.owner
                .synchronize_video(session, &state.output, &views, state.now, audio.program_key())?;
            self.settle(session)?;
        }
        Ok(())
    }
    fn settle(&mut self, session: &Session) -> Result<(), CliError> {
        let until = Instant::now() + Duration::from_secs(10);
        while self.owner.is_busy() {
            self.poll(session)?;
            if Instant::now() >= until {
                return Err("media worker control timed out".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
    pub fn prepare(&mut self, session: &Session, path: PathBuf) -> Result<PreparedMedia, CliError> {
        self.settle(session)?;
        let expected = self.owner.prepare(session, path)?;
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match self.owner.poll(session)? {
                Some(MediaEvent::Prepared { key, result }) if key == expected => {
                    return Ok(result?);
                }
                _ => {}
            }
            if Instant::now() >= until {
                return Err("media preparation timed out".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub fn start(
        &mut self,
        session: &Session,
        state: &TransitionSnapshot,
        audio: &AudioCaptureWorker,
    ) -> Result<(), CliError> {
        self.browser_running=true;
        self.ndi_running=true;
        let policy=self.camera_activation.snapshot();
        self.browser.set_camera_activation_policy(policy.clone());
        self.ndi.set_camera_activation_policy(policy.clone());
        self.owner.set_camera_activation_policy(policy);
        let mut views=state.output.scenes();if state.studio {views.push(FrameScene::Live(session.preview()));}let audio_views=views.clone();if self.outputs.owner.active(skylive_core::NdiOutputRoute::Preview)&&!state.studio{let extra=FrameScene::Live(state.output.primary_scene());if !views.iter().any(|v|v.key()==extra.key()){views.push(extra);}}
        self.browser.synchronize(session,&views,&state.output.scenes(),&audio_views,state.now,audio.program_key())?;
        self.ndi.synchronize_video(session,&views,&audio_views,state.now,audio.program_key())?;
        let current = session.snapshot();
        let mut has_media = current
            .sources()
            .iter()
            .any(|source| matches!(source.kind(), skylive_core::SourceKind::MediaSource { .. }));
        if !has_media {
            for view in state.output.scenes() {
                if view.visible_sources(&current)?.iter().any(|source| {
                    matches!(source.kind(), skylive_core::SourceKind::MediaSource { .. })
                }) {
                    has_media = true;
                    break;
                }
            }
        }
        // A color/image/GIF-only deterministic render must not acquire a file
        // wall clock or change the caller's supplied transition timeline.
        if !has_media {
            return Ok(());
        }
        self.settle(session)?;
        self.running = true;
        self.synchronize(session, state, audio)?;
        self.settle(session)
    }
    pub fn wait_ready(
        &mut self,
        session: &Session,
        state: &TransitionSnapshot,
    ) -> Result<(), CliError> {
        self.wait_ndi_ready(session,state)?;
        self.wait_browser_ready(session,state)?;
        let current = session.snapshot();
        let mut expected = Vec::new();
        for view in state.output.scenes() {
            for source in view.visible_sources(&current)? {
                if matches!(source.kind(),skylive_core::SourceKind::MediaSource{media}if media.video_dimensions().is_some())
                    && !expected.contains(&source.id())
                {
                    expected.push(source.id());
                }
            }
        }
        if expected.len() > 2 {
            return Err(
                "media H.264 decoder profile admits at most two active video sources".into(),
            );
        }
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            self.poll(session)?;
            let sample = self.owner.snapshot();
            if let Some(problem) = sample.problems().first() {
                return Err(CliError {
                    code: problem.code,
                    detail: problem.detail.clone(),
                    phase: None,
                });
            }
            if expected.iter().all(|id| sample.frame(*id).is_some()) {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err("media decoder first frame timed out".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub fn stop(&mut self, session: &Session) -> Result<(), CliError> {
        self.browser_running=false;self.browser.stop(session)?;self.browser_union.clear();
        self.ndi_running=false;self.ndi.stop();self.ndi_union.clear();
        self.settle(session)?;
        self.owner.stop(session)?;
        self.settle(session)?;
        self.running = false;
        self.union.clear();
        Ok(())
    }
    fn wait_ndi_ready(&mut self,session:&Session,state:&TransitionSnapshot)->Result<(),CliError>{
        let current=session.snapshot();if !current.preferences().plugin_enabled(skylive_core::NDI_PLUGIN){return Ok(());}
        let mut expected=Vec::new();for view in state.output.scenes(){for source in view.visible_sources(&current)?{if matches!(source.kind(),skylive_core::SourceKind::NdiSource {..})&&!expected.contains(&source.id()){expected.push(source.id());}}}
        let until=Instant::now()+Duration::from_secs(5);loop {self.ndi.poll(session)?;let snapshot=self.ndi.snapshot();if let Some(problem)=snapshot.problems().iter().find(|p|expected.contains(&p.source)&&p.code!="ndi_unavailable"){return Err(CliError {code:problem.code,detail:problem.detail.clone(),phase:None});}
            if expected.iter().all(|id|snapshot.frame(*id).is_some()){return Ok(());}if Instant::now()>=until{return Err("NDI actual first frame unavailable after5 seconds; receive owner remains asynchronous".into());}std::thread::sleep(Duration::from_millis(1));}
    }
    pub fn discover_ndi(&mut self,session:&Session,local:bool)->Result<Vec<skylive_core::NdiSender>,CliError>{
        let key=self.ndi.discover(session,local)?;loop {if let Some(skylive_host::ndi::NdiEvent::Discovered {key:actual,result})=self.ndi.poll(session)?{if actual==key{return Ok(result?);}}std::thread::sleep(Duration::from_millis(1));}
    }
    pub fn prepare_ndi(&mut self,session:&Session,connection:skylive_core::NdiConnection)->Result<skylive_core::NdiSource,CliError>{
        let key=self.ndi.prepare(session,connection)?;loop {if let Some(skylive_host::ndi::NdiEvent::Prepared {key:actual,result})=self.ndi.poll(session)?{if actual==key{return Ok(result?);}}std::thread::sleep(Duration::from_millis(1));}
    }
    fn wait_browser_ready(&mut self,session:&Session,state:&TransitionSnapshot)->Result<(),CliError>{
        let model=session.snapshot();let mut expected=Vec::new();for view in state.output.scenes(){for source in view.visible_sources(&model)?{if matches!(source.kind(),skylive_core::SourceKind::Browser {..})&&!expected.contains(&source.id()){expected.push(source.id());}}}
        let until=Instant::now()+Duration::from_secs(30);loop{self.browser.poll(session)?;let snapshot=self.browser.snapshot();if let Some(problem)=snapshot.problems().iter().find(|p|expected.contains(&p.source)&&p.code!="browser_frame_pending"){return Err(CliError {code:problem.code,detail:problem.detail.clone(),phase:None});}if expected.iter().all(|source|snapshot.frame(*source).is_some()){return Ok(());}if Instant::now()>=until{return Err("Browser Source actual first paint unavailable after30 seconds".into());}std::thread::sleep(Duration::from_millis(1));}
    }
    pub fn browser_summary(&self)->Value{let sample=self.browser.snapshot();json!({"running":self.browser_running,"available":skylive_host::browser::BrowserOwner::available(),"instance":sample.instance(),"session":sample.session(),"epoch":sample.epoch(),"native_epoch":sample.native_epoch(),"generation":sample.generation(),"frames":sample.frames().map(|(source,f)|json!({"source":source,"lease":f.lease,"binding_lease":f.binding_lease,"sequence":f.sequence,"time_ns":f.requested_at.0,"width":f.image.width(),"height":f.image.height()})).collect::<Vec<_>>(),"problems":sample.problems().iter().map(|p|json!({"source":p.source,"code":p.code,"detail":p.detail})).collect::<Vec<_>>(),"audio":"source_controlled_cef_pcm_or_eligible_native_playback","native_playback_acknowledgements":sample.native_playback().map(|(source,receipt)|json!({"source":source,"receipt":receipt})).collect::<Vec<_>>(),"audio_formats":sample.formats().map(|(source,_,format)|json!({"source":source,"sample_rate":format.sample_rate,"channels":format.channels,"channel_mask":format.channel_mask})).collect::<Vec<_>>(),"native_obs_plugin_loaded":false})}
    pub fn ndi_summary(&self)->Value {let sample=self.ndi.snapshot();json!({"port_available":skylive_host::ndi::NdiOwner::available(),"runtime_version":sample.runtime_version,"session":sample.session(),"epoch":sample.epoch(),"generation":sample.generation(),"frames":sample.frames().map(|(source,f)|json!({"source":source,"sequence":f.sequence,"video":f.metadata})).collect::<Vec<_>>(),"problems":sample.problems().iter().map(|p|json!({"source":p.source,"code":p.code,"detail":p.detail})).collect::<Vec<_>>(),"native_obs_plugin_loaded":false})}
    pub fn discover_games(&mut self,session:&Session)->Result<Vec<skylive_host::game_capture::GameTargetInfo>,CliError>{
        let key=self.game.targets(session)?;loop {match self.game.poll(session)? {Some(skylive_host::game_capture::GameEvent::Targets {key:actual,result})if actual==key=>return Ok(result?),_=>{}}
            std::thread::sleep(Duration::from_millis(1));}
    }
    pub fn game_control(&mut self,session:&Session,source:Id,start:bool)->Result<Value,CliError>{
        let model=session.snapshot();let skylive_core::SourceKind::GameCapture {binding}=model.source(source).ok_or("Game source is unavailable")?.kind() else{return Err("Source is not Game Capture".into());};
        if start {self.game.authorize(session,binding.target())?;}else{self.game.revoke(session,source)?;}
        Ok(json!({"source":source,"capture_authorized":start,"start_requested":start,"stop_requested":!start,"native_frame_verified":false,"status":self.game_summary(),"video_only":true}))
    }
    pub fn game_summary(&self)->Value {
        let sample=self.game.snapshot();json!({"available":skylive_host::game_capture::GameOwner::available(),"session":sample.session(),"epoch":sample.epoch(),"generation":sample.generation(),
            "native_streams":sample.streams(),"frames":sample.frames().map(|(source,frame)|json!({"source":source,"sequence":frame.sequence,"generation":frame.generation,"qpc":frame.qpc,"time_ns":frame.requested_at.0,"width":frame.image.width(),"height":frame.image.height(),"dropped":frame.dropped,"throttled":frame.throttled})).collect::<Vec<_>>(),
            "problems":sample.problems().iter().map(|problem|json!({"source":problem.source,"code":problem.error.code(),"detail":problem.error.to_string()})).collect::<Vec<_>>(),"renderer":"d3d11_present_vtable","dll_unloaded_on_stop":false,"native_verified":false})
    }
    pub fn reload_media(&mut self,session:&Session,source:Id,now:MediaTime)->Result<Value,CliError>{
        if !self.running{return Err("Media playback is not running; its next normal start opens the current file.".into());}
        self.poll(session)?;
        if self.reload.as_ref().is_some_and(|r|!r.completed){return Err("Wait for the pending media reload receipt.".into());}
        let target=skylive_host::media_source::MediaReloadTarget::new(session,source)?;
        let key=self.owner.reload(session,&target,now)?;
        self.reload=Some(ReloadReceipt{key,target,completed:false,status:None,problem:None});
        Ok(self.summary())
    }
    pub fn summary(&self) -> Value {
        let snapshot = self.owner.snapshot();
        json!({"reload":self.reload.as_ref().map(|r|json!({"request":r.key,"source":r.target.source(),"completed":r.completed,"reader_retirement_acknowledged":r.status.is_some(),"decoder_ready_claimed":false,"transport_status":r.status,"problem":r.problem})),"running":self.running,"instance":snapshot.instance(),"epoch":snapshot.epoch(),"generation":snapshot.generation(),"playing":snapshot.playing,"sources":snapshot.source_count(),"frames":snapshot.frames().map(|(source,frame)|json!({"source":source,"sequence":frame.sequence,"time_ns":frame.requested_at.0,"width":frame.image.width(),"height":frame.image.height()})).collect::<Vec<_>>(),"problems":snapshot.problems(),"native_capture":false,"external_decoder_process":false})
    }
}
