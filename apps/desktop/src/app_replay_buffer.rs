//! Program replay uses the same immutable composition/capture/playback inputs
//! and RenderGpu service as recording. Controls are explicit owner commands.
use super::*;
use skylive_host::{replay_buffer::{ReplayConfig,ReplayOwner,ReplayEvent,ReplayError},audio_output::{OutputKey,AudioOutput}};
pub(super) struct Runtime {pub(super) owner:ReplayOwner,close_after:bool,ready:Option<OutputKey>,origin:Option<MediaTime>,last_tick:Option<u64>}
impl Runtime {
    pub(super) fn hotkey_intent(&self,action:skylive_core::HotkeyAction)->Option<Intent>{
        use skylive_core::HotkeyAction as Action;let state=self.owner.status();match action{
            Action::StartReplayBuffer=>(!self.active()).then_some(Intent::StartReplayBuffer),
            Action::StopReplayBuffer=>(state.active&&!state.starting&&!state.stopping).then_some(Intent::StopReplayBuffer),_=>None,
        }
    }

    pub(super) fn new(gpu:skylive_host::render_gpu::RenderGpu)->Self{Self {owner:ReplayOwner::new(gpu),close_after:false,ready:None,origin:None,last_tick:None}}
    pub(super) fn active(&self)->bool{self.owner.active()}
    pub(super) fn needs_audio(&self)->bool{self.active()&&!self.owner.status().stopping&&self.owner.config().is_some_and(|c|c.audio_enabled)}
    pub(super) fn close(&mut self){self.owner.close();}
}
impl StudioApp {
    pub(super) fn start_replay(&mut self){
        if self.replay.active(){return;}if self.io.busy()||self.documents.modal_open(){self.message(true,"Finish the document operation before starting Replay Buffer");return;}
        let config=match ReplayConfig::from_session(&self.session){Ok(c)=>c,Err(e)=>{self.replay_error(e);return;}};
        if config.audio_enabled&&self.audio.is_busy(){self.message(true,"Audio worker is busy; try Replay Buffer again");return;}
        match self.replay.owner.start(&self.session,config){
            Ok(sink)=>{self.replay.ready=None;self.replay.origin=None;self.replay.last_tick=None;
                if let Some(sink)=sink {let Some(state)=self.sync_transition()else{self.replay.owner.close();return;};if let Err(e)=self.audio.request_program_with_output(&self.session,&state.output,self.frame_now,AudioOutput::ReplayBuffer(sink)){self.replay.owner.close();self.message(true,e.to_string());}}
            },Err(e)=>self.replay_error(e),
        }
    }
    pub(super) fn stop_replay(&mut self){self.replay.owner.stop();self.replay.ready=None;}
    pub(super) fn save_replay(&mut self){
        let Some(key)=self.replay.owner.key()else{return;};let model=self.session.snapshot();let directory=model.profiles().selected().recording().directory();let directory=if directory.is_empty(){skylive_host::recording::default_directory()}else{PathBuf::from(directory)};
        let stamp=match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH){Ok(v)=>v,Err(e)=>{self.message(true,e.to_string());return;}};
        let path=directory.join(format!("SkyLive-Replay-{}-{:09}.mp4",stamp.as_secs(),stamp.subsec_nanos()));
        if let Err(e)=self.replay.owner.save(key,path){self.replay_error(e);}
    }
    fn replay_error(&mut self,error:ReplayError){self.message(true,crate::status::Status::formatted("Replay Buffer: {error}",&[("{error}",error.to_string())]));}
    pub(super) fn poll_replay(&mut self,context:&egui::Context){
        if self.replay.owner.key().is_some_and(|key|key.session!=self.session.identity()){self.replay.owner.close();self.replay.ready=None;}
        for _ in 0..8 {let Some(event)=self.replay.owner.poll()else{break;};match event {
            ReplayEvent::ReadyForInputs {key} if Some(key)==self.replay.owner.key()&&key.session==self.session.identity()=>self.replay.ready=Some(key),
            ReplayEvent::Started {..}=>self.message(false,"Replay Buffer started"),
            ReplayEvent::Saved {result:Ok(receipt),..}=>self.message(false,crate::status::Status::formatted("Replay saved: {path}",&[("{path}",receipt.path.display().to_string())])),
            ReplayEvent::Saved {result:Err(error),..}|ReplayEvent::Failed {error,..}=>self.replay_error(error),
            ReplayEvent::Stopped {..}=>{self.replay.ready=None;self.replay.origin=None;self.message(false,"Replay Buffer stopped");},_=>{},
        }}
        views::replay_buffer::ReplayView {available:skylive_host::replay_buffer::available(),status:self.replay.owner.status()}.install(context);
        if self.replay.active(){context.request_repaint_after(std::time::Duration::from_millis(10));}else if self.replay.close_after{self.replay.close_after=false;self.shutdown(context);}
    }
    pub(super) fn replay_program_frame(&mut self,context:&egui::Context,playback:&skylive_host::playback::PlaybackSnapshot,output:&skylive_host::transition::FrameComposition,capture:&Arc<CaptureSnapshot>){
        if !self.replay.active()||self.replay.owner.status().stopping{return;}
        let model=self.session.snapshot();if playback.session()!=self.session.identity()||playback.revision()!=model.revision()||playback.now()!=self.frame_now{return;}
        if let Some(key)=self.replay.ready.take(){match self.replay.owner.activate(key,self.frame_now){Ok(())=>self.replay.origin=Some(self.frame_now),Err(e)=>{self.replay.owner.close();self.replay_error(e);return;}}}
        let (Some(origin),Some(config),Some(key))=(self.replay.origin,self.replay.owner.config(),self.replay.owner.key())else{return;};
        let Some(elapsed)=self.frame_now.0.checked_sub(origin.0)else{self.replay.owner.close();return;};let tick=(u128::from(elapsed)*u128::from(config.video.fps_num())/(1_000_000_000u128*u128::from(config.video.fps_den()))) as u64;
        if self.replay.last_tick.is_some_and(|t|tick<=t){return;}
        let frame=skylive_host::recording::RecordingFrame {session:self.session.identity(),time:self.frame_now,model,output:output.clone(),playback:playback.clone(),capture:capture.clone(),resource_generation:self.preview.resource_generation()};
        match self.replay.owner.submit(key,frame){Ok(())=>self.replay.last_tick=Some(tick),Err((ReplayError::Busy,_))=>{},Err((error,_))=>{self.replay.owner.close();self.replay_error(error);}}
        context.request_repaint_after(std::time::Duration::from_millis(5));
    }
    pub(super) fn wait_replay_close(&mut self,context:&egui::Context)->bool{if !self.replay.active(){return false;}self.replay.close_after=true;self.replay.owner.close();context.request_repaint_after(std::time::Duration::from_millis(10));true}
}
