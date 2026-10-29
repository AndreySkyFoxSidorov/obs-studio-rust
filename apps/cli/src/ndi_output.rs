//! Explicit CLI sender controls. Video scheduling is caller-driven; native SDK
//! readiness and submission are separate receipts, never remote-delivery claims.
use super::*;
use skylive_core::NdiOutputRoute;
use skylive_host::{ndi::output::{OutputOwner,Key,Event},audio_output::AudioOutput};
pub(super) struct Runtime {pub owner:OutputOwner,origins:[Option<MediaTime>;2],events:Vec<Value>,dropped:u64}
impl Runtime {
    pub fn new(gpu:skylive_host::render_gpu::RenderGpu)->Self{Self {owner:OutputOwner::new(gpu),origins:[None,None],events:Vec::new(),dropped:0}}
    pub fn audio_live(&self)->bool{self.owner.active(NdiOutputRoute::Program)&&self.owner.config(NdiOutputRoute::Program).is_some_and(|c|c.audio.is_some())}
    pub fn guard(&self,request:&Request)->Result<(),CliError>{
        if self.owner.any_active(){let document=matches!(request,Request::New|Request::Load {..}|Request::Recover {..}|Request::ImportObs {..});let format=matches!(request,Request::Command {command,..}if skylive_host::ndi::output::blocks_command(command));if document||format{return Err("Stop NDI outputs before replacing the collection or changing formats/settings/profile".into());}}
        if self.audio_live(){let rewrites=matches!(request,Request::Advance {..}|Request::StartStreaming {time_ns:Some(_)}|Request::StartReplayBuffer {time_ns:Some(_)}|Request::StartBrowserSources {time_ns:Some(_)}|Request::SelectPreview {time_ns:Some(_),..}|Request::Cut {time_ns:Some(_)}|Request::StudioMode {time_ns:Some(_),..}|Request::Transition {time_ns:Some(_)}|Request::QuickTransition {time_ns:Some(_),..}|Request::TbarPosition {time_ns:Some(_),..}|Request::TbarRelease {time_ns:Some(_)}|Request::RecordFrame {time_ns:Some(_)}|Request::CaptureFrame {time_ns:Some(_)}|Request::CaptureAudio {time_ns:Some(_)}|Request::MixProgramAudio {advance_ns:1..,..}|Request::RenderProgram {time_ns:Some(_),..}|Request::StartMediaPlayback {time_ns:Some(_)}|Request::RestartMedia {time_ns:Some(_),..}|Request::StartNativeAudioProgram {time_ns:Some(_)}|Request::StartProgramAudio {time_ns:Some(_)}|Request::StartRecording {time_ns:Some(_),..}|Request::StopNativeAudioProgram|Request::ReplayBufferFrame {time_ns:Some(_),..}|Request::NdiOutputFrame {time_ns:Some(_),..}|Request::StartNdiOutput {time_ns:Some(_),..});if rewrites{return Err("NDI Program audio uses the live Program clock; omit explicit times and stop NDI before restarting that clock".into());}}Ok(())
    }
    pub fn refresh_clock(&self,session:&mut Session,controller:&mut Controller,audio:&AudioCaptureWorker)->Result<(),CliError>{if self.audio_live()&&self.origins[0].is_some(){controller.sync(session,audio.program_now()?.ok_or("NDI Program audio clock is no longer running")?)?;}Ok(())}
    pub fn start(&mut self,session:&Session,state:&TransitionSnapshot,audio:&mut AudioCaptureWorker,route:NdiOutputRoute)->Result<Value,CliError>{
        if route==NdiOutputRoute::Program&&session.snapshot().preferences().ndi_output().program_audio()&&audio.is_busy(){return Err(AudioCaptureError::Busy.into());}
        let sink=self.owner.start(session,route)?;self.origins[route.index()]=None;if let Some(sink)=sink{if let Err(error)=audio.request_program_with_output(session,&state.output,state.now,AudioOutput::NdiProgram(sink)){self.owner.stop(route);return Err(error.into());}}
        Ok(json!({"key":self.owner.key(route),"admitted":true,"starting":true,"backend_ready":false,"video_schedule":"caller_driven","status":self.owner.status(route)}))
    }
    pub fn poll(&mut self,session:&mut Session,controller:&mut Controller,audio:&AudioCaptureWorker)->Result<(),CliError>{
        self.owner.synchronize(session);for _ in 0..8{let Some(event)=self.owner.poll()else{break;};let value=match event {
            Event::Ready {key}=>{if key.session!=session.identity()||self.owner.key(key.route)!=Some(key)||self.owner.status(key.route).stopping{continue;}let origin=audio.program_now()?.unwrap_or_else(||controller.now());controller.sync(session,origin)?;self.owner.activate(key,origin)?;self.origins[key.route.index()]=Some(origin);json!({"event":"backend_ready","key":key,"origin_ns":origin.0,"status":self.owner.status(key.route)})},
            Event::Sending {key}=>json!({"event":"video_submitted_to_sdk","key":key,"remote_delivery_verified":false}),
            Event::Failed {key,error}=>json!({"event":"failed","key":key,"code":error.code(),"detail":error.to_string()}),
            Event::Stopped {key}=>{self.origins[key.route.index()]=None;json!({"event":"stopped","key":key})},
        };if self.events.len()==16{self.events.remove(0);self.dropped=self.dropped.saturating_add(1);}self.events.push(value);}Ok(())
    }
    pub fn status(&mut self)->Value{json!({"available":skylive_host::ndi::output::available(),"program":{"key":self.owner.key(NdiOutputRoute::Program),"status":self.owner.status(NdiOutputRoute::Program)},"preview":{"key":self.owner.key(NdiOutputRoute::Preview),"status":self.owner.status(NdiOutputRoute::Preview)},"events":std::mem::take(&mut self.events),"events_dropped":self.dropped,"video_schedule":"caller_driven","native_verified":false})}
    pub fn frame(&mut self,key:Key,frame:skylive_host::recording::RecordingFrame)->Result<Value,CliError>{if self.owner.key(key.route)!=Some(key){return Err("NDI output key belongs to another run".into());}if self.origins[key.route.index()].is_none(){return Err("NDI sender is still initializing; poll ndi_output_status before submitting video".into());}self.owner.submit(key,frame).map_err(|(e,_)|CliError::from(e))?;Ok(json!({"key":key,"frame_admitted":true,"remote_delivery_verified":false}))}
    pub fn stop(&mut self,key:Key)->Result<Value,CliError>{if self.owner.key(key.route)!=Some(key){return Err("NDI output key belongs to another run".into());}self.owner.stop(key.route);Ok(json!({"key":key,"stop_requested":true,"status":self.owner.status(key.route)}))}
}
impl Drop for Runtime {fn drop(&mut self){self.owner.join();}}
