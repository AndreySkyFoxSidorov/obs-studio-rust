//! Explicit caller-driven replay frames; native startup/save completion stays
//! observable through actual events. Accepted requests are not saved receipts.
use super::*;
use skylive_host::{audio_output::{AudioOutput,OutputKey,OutputKind},replay_buffer::{ReplayOwner,ReplayEvent,ReplayConfig,ReplayError}};
#[derive(Clone,Copy,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Key {session:u64,id:u64,epoch:u64}
impl Key {fn host(self)->OutputKey{OutputKey {kind:OutputKind::ReplayBuffer,session:self.session,id:self.id,epoch:self.epoch}}}
pub(super) struct Runtime {pub owner:ReplayOwner,pub origin:Option<MediaTime>,events:Vec<Value>,events_dropped:u64}
impl Runtime {
    pub fn new(gpu:skylive_host::render_gpu::RenderGpu)->Self{Self {owner:ReplayOwner::new(gpu),origin:None,events:Vec::new(),events_dropped:0}}
    pub fn key(&self)->Option<Key>{self.owner.key().map(|k|Key {session:k.session,id:k.id,epoch:k.epoch})}
    fn require(&self,key:Key)->Result<OutputKey,CliError>{let key=key.host();if self.owner.key()!=Some(key){return Err("Replay Buffer key does not identify this run".into());}Ok(key)}
    pub fn guard(&self,request:&Request)->Result<(),CliError>{
        if !self.owner.active()||!self.owner.config().is_some_and(|c|c.audio_enabled){return Ok(());}
        let rewrites=matches!(request,Request::Advance {..}|Request::SelectPreview {time_ns:Some(_),..}|Request::Cut {time_ns:Some(_)}|Request::StudioMode {time_ns:Some(_),..}|Request::Transition {time_ns:Some(_)}|Request::QuickTransition {time_ns:Some(_),..}|Request::TbarPosition {time_ns:Some(_),..}|Request::TbarRelease {time_ns:Some(_)}|Request::RecordFrame {time_ns:Some(_)}|Request::CaptureFrame {time_ns:Some(_)}|Request::CaptureAudio {time_ns:Some(_)}|Request::MixProgramAudio {advance_ns:1..,..}|Request::RenderProgram {time_ns:Some(_),..}|Request::StartMediaPlayback {time_ns:Some(_)}|Request::RestartMedia {time_ns:Some(_),..}|Request::StartNativeAudioProgram {time_ns:Some(_)}|Request::StartProgramAudio {time_ns:Some(_)}|Request::StartRecording {time_ns:Some(_),..}|Request::StopNativeAudioProgram|Request::ReplayBufferFrame {time_ns:Some(_),..}|Request::StartNdiOutput {time_ns:Some(_),..}|Request::NdiOutputFrame {time_ns:Some(_),..});
        if rewrites{return Err("Replay Buffer uses the live Program audio clock; omit explicit time_ns and stop replay before restarting that clock".into());}Ok(())
    }
    pub fn refresh_clock(&self,session:&mut Session,controller:&mut Controller,audio:&AudioCaptureWorker)->Result<(),CliError>{if self.owner.active()&&self.origin.is_some()&&self.owner.config().is_some_and(|c|c.audio_enabled){let now=audio.program_now()?.ok_or("Replay Buffer Program audio clock is no longer running")?;controller.sync(session,now)?;}Ok(())}
    pub fn start(&mut self,session:&Session,state:&TransitionSnapshot,audio:&mut AudioCaptureWorker)->Result<Value,CliError>{
        if self.owner.active(){return Err(ReplayError::Busy.into());}let config=ReplayConfig::from_session(session)?;if config.audio_enabled&&audio.is_busy(){return Err(AudioCaptureError::Busy.into());}
        let sink=self.owner.start(session,config)?;self.origin=None;
        if let Some(sink)=sink {if let Err(error)=audio.request_program_with_output(session,&state.output,state.now,AudioOutput::ReplayBuffer(sink)){self.owner.close();return Err(error.into());}}
        Ok(json!({"key":self.key(),"admitted":true,"starting":true,"status":self.owner.status(),"video_schedule":"caller_driven","file_published":false}))
    }
    pub fn poll(&mut self,session:&mut Session,controller:&mut Controller,audio:&AudioCaptureWorker)->Result<(),CliError>{
        if self.owner.key().is_some_and(|k|k.session!=session.identity()){self.owner.close();}
        for _ in 0..8 {let Some(event)=self.owner.poll()else{break;};let value=match event {
            ReplayEvent::ReadyForInputs {key}=>{if key.session!=session.identity(){self.owner.close();continue;}let origin=audio.program_now()?.unwrap_or_else(||controller.now());controller.sync(session,origin)?;self.owner.activate(key,origin)?;self.origin=Some(origin);json!({"event":"native_ready","key":self.key(),"origin_ns":origin.0})},
            ReplayEvent::Started {..}=>json!({"event":"started","key":self.key()}),
            ReplayEvent::Stopped {..}=>{self.origin=None;json!({"event":"stopped","key":self.key()})},
            ReplayEvent::Failed {error,..}=>json!({"event":"failed","key":self.key(),"code":error.code(),"detail":error.to_string()}),
            ReplayEvent::Saved {result:Ok(receipt),..}=>json!({"event":"saved","receipt":receipt,"file_published":true}),
            ReplayEvent::Saved {result:Err(error),..}=>json!({"event":"save_failed","code":error.code(),"detail":error.to_string(),"phase":if matches!(error,ReplayError::AfterPublication {..}){"after_publication"}else{"before_publication"}}),
        };if self.events.len()==16{self.events.remove(0);self.events_dropped=self.events_dropped.saturating_add(1);}self.events.push(value);}
        Ok(())
    }
    pub fn status(&mut self)->Value{json!({"key":self.key(),"available":skylive_host::replay_buffer::available(),"status":self.owner.status(),"events":std::mem::take(&mut self.events),"events_dropped":self.events_dropped,"native_verified":false})}
    pub fn frame(&mut self,key:Key,frame:skylive_host::recording::RecordingFrame)->Result<Value,CliError>{let key=self.require(key)?;if self.origin.is_none(){return Err("Replay Buffer encoders are still starting; query replay_buffer_status before submitting video".into());}self.owner.submit(key,frame).map_err(|(e,_)|CliError::from(e))?;Ok(json!({"admitted":true,"key":self.key(),"file_published":false}))}
    pub fn save(&mut self,key:Key,path:PathBuf)->Result<Value,CliError>{let key=self.require(key)?;self.owner.save(key,path)?;Ok(json!({"key":self.key(),"save_requested":true,"file_published":false,"status":self.owner.status()}))}
    pub fn stop(&mut self,key:Key)->Result<Value,CliError>{self.require(key)?;self.owner.stop();Ok(json!({"stop_requested":true,"status":self.owner.status()}))}
    pub fn cancel(&mut self,key:Key)->Result<Value,CliError>{self.require(key)?;self.owner.cancel_save();Ok(json!({"cancel_requested":true,"published_files_retained":true}))}
}
impl Drop for Runtime {fn drop(&mut self){self.owner.join();}}
