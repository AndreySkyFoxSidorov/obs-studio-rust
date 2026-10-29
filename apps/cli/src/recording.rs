//! CLI output control uses the same host writer and final Program blocks as UI.
use super::*;
use skylive_host::transition::FrameComposition;
use skylive_host::{
    audio_program::ProgramAudioBlock,
    recording::{
        RecordingError, RecordingEvent, RecordingFrame, RecordingOwner,
        RecordingStatus,
    },
};
use std::time::{Duration, Instant};
#[derive(Default)]
pub(super) struct Recorder {
    render_gpu: skylive_host::render_gpu::RenderGpu,
    owner: Option<RecordingOwner>,
    resource_generation: u64,
}
impl From<RecordingError> for CliError {
    fn from(error: RecordingError) -> Self {
        let detail = match error.recovery_path {
            Some(path) => format!("{}; recovery file: {}", error.detail, path.display()),
            None => error.detail,
        };
        Self {
            code: error.code,
            detail,
            phase: Some("recording_output"),
        }
    }
}
impl Recorder {
    pub fn new_with_render_gpu(render_gpu:skylive_host::render_gpu::RenderGpu)->Self {
        let mut owner=Self::default();owner.render_gpu=render_gpu;owner
    }
    pub fn active(&self) -> bool {
        self.owner.as_ref().is_some_and(RecordingOwner::is_active)
    }
    pub fn start(
        &mut self,
        session: &Session,
        path: PathBuf,
        origin: MediaTime,
        audio: Option<bool>,
        live: Option<(&mut AudioCaptureWorker, &FrameComposition)>,
    ) -> Result<Value, CliError> {
        if self.active() {
            return Err("stop recording before starting another output".into());
        }
        let mut plan=skylive_host::recording::MultitrackRecordingConfig::from_session(session,path)?;
        if audio==Some(false){plan=plan.without_audio();}
        let mut config=plan.base().clone();
        if audio == Some(false) {
            config.audio = None;
        }
        if audio == Some(true) && config.audio.is_none() {
            return Err("enable recording audio in the selected profile first".into());
        }
        if config.audio.is_some() && live.as_ref().is_some_and(|(worker, _)| worker.is_busy()) {
            return Err(AudioCaptureError::Busy.into());
        }
        if self.owner.is_none() {
            self.owner = Some(RecordingOwner::new_with_render_gpu(self.render_gpu.clone())?);
        }
        let owner = self.owner.as_mut().unwrap();
        let mask=plan.tracks().iter().fold(0,|mask,track|mask|track.id.bit());
        owner.start_tracks(session.identity(),config,origin,skylive_host::recording::native_multitrack_factory(plan),mask)?;
        if let Some((worker, output)) = live {
            if owner.audio_enabled() {
                let sink = owner.audio_sink(session.snapshot().audio())?;
                if let Err(error) =
                    worker.request_program_with_recording(session, output, origin, sink)
                {
                    let _ = owner.stop();
                    return Err(error.into());
                }
                wait_program_ack(session, worker, None)?;
            }
        }
        let event = self.wait(false)?;
        Ok(json!({"event":event,"status":self.status(),"recording_output":true}))
    }
    fn wait(&mut self, finish: bool) -> Result<RecordingEvent, CliError> {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(event) = self
                .owner
                .as_mut()
                .ok_or("recording owner is not initialized")?
                .poll()?
            {
                match event {
                    RecordingEvent::Failed(error) => return Err(error.into()),
                    RecordingEvent::Started if !finish => return Ok(event),
                    RecordingEvent::Finished { .. } if finish => return Ok(event),
                    _ => {}
                }
            }
            if Instant::now() >= until {
                return Err("recording acknowledgement deadline expired; output remains owned until stop/close".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub fn status(&mut self) -> RecordingStatus {
        self.owner
            .as_mut()
            .map_or_else(Default::default, RecordingOwner::status)
    }
    pub fn video(&mut self, value: RecordingFrame) -> Result<Value, CliError> {
        if !self.active() {
            return Err("start recording before submitting Program video".into());
        }
        let owner = self.owner.as_mut().unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match owner.video(value.clone()) {
                Ok(()) => {
                    return Ok(
                        json!({"accepted":true,"status":owner.status(),"recording_output":true}),
                    );
                }
                Err(error) if error.code=="recording_paused"=>return Ok(json!({"accepted":false,"paused":true,"status":owner.status(),"recording_output":true})),
                Err(error) if error.code == "recording_backpressure" => {}
                Err(error) => return Err(error.into()),
            }
            if Instant::now() >= until {
                return Err("recording video queue deadline expired".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub fn audio(&mut self, block: Arc<ProgramAudioBlock>) -> Result<(), CliError> {
        if !self.active() {
            return Ok(());
        }
        let owner = self.owner.as_mut().unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match owner.audio(block.clone()) {
                Ok(()) => return Ok(()),
                Err(error) if error.code == "recording_backpressure" => {}
                Err(error) => return Err(error.into()),
            }
            if Instant::now() >= until {
                return Err("recording audio queue deadline expired".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub fn live_sink(
        &mut self,
        settings: skylive_core::AudioSettings,
    ) -> Result<Option<skylive_host::recording::RecordingAudioSink>, CliError> {
        let Some(owner) = self.owner.as_mut() else {
            return Ok(None);
        };
        if !owner.audio_enabled() || owner.live_audio_attached() {
            return Ok(None);
        }
        Ok(Some(owner.audio_sink(settings)?))
    }
    pub fn set_paused(&mut self,recording:u64,session:u64,at:MediaTime,paused:bool)->Result<Value,CliError> {
        let owner=self.owner.as_mut().ok_or("start recording before pausing")?;
        let Some(key)=owner.set_paused(recording,session,at,paused)? else {return Ok(json!({"changed":false,"status":owner.status(),"recording_output":true}));};
        let until=Instant::now()+Duration::from_secs(30);
        loop {
            if let Some(event)=owner.poll()? {match event {
                RecordingEvent::PauseChanged {key:actual,result} if actual==key=>return Ok(json!({"receipt":result?,"status":owner.status(),"recording_output":true})),
                RecordingEvent::Failed(error)=>return Err(error.into()),
                RecordingEvent::Finished {..}=>return Err("recording finished before its pause acknowledgement".into()),
                _=>{},
            }}
            if Instant::now()>=until {return Err("recording pause acknowledgement deadline expired; output remains owned".into());}
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub fn stop(&mut self) -> Result<Value, CliError> {
        if !self.active() {
            return Ok(json!({"active":false,"recording_output":false}));
        }
        self.owner.as_mut().unwrap().stop()?;
        let event = self.wait(true)?;
        Ok(json!({"event":event,"status":self.status(),"recording_output":true}))
    }
    pub fn generation(&self) -> u64 {
        self.resource_generation
    }
    pub fn invalidate_resources(&mut self) -> Result<(), CliError> {
        self.resource_generation = self
            .resource_generation
            .checked_add(1)
            .ok_or("recording resource generation exhausted")?;
        Ok(())
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.as_mut() {
            let _ = owner.join();
        }
    }
}
