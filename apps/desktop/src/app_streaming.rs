//! UI coordination only; shared host owners perform all native/network work.
use super::*;
use skylive_host::streaming::{
    Config, Error as StreamingError, Event, Phase, ProgramVideoFrame, RejectedFrame, Status,
    StreamKey, StreamingOwner,
};

/// App-owned capability boundary. No codec/socket object is exposed to a view.
pub(super) trait Service {
    fn key(&self) -> StreamKey;
    fn epoch(&self) -> u64;
    fn status(&self) -> Option<Status>;
    fn poll(&mut self) -> Option<Event>;
    fn activate(
        &mut self,
        key: StreamKey,
        epoch: u64,
        frame: ProgramVideoFrame,
    ) -> Result<(), RejectedFrame>;
    fn video(
        &mut self,
        key: StreamKey,
        epoch: u64,
        frame: ProgramVideoFrame,
    ) -> Result<(), RejectedFrame>;
    fn stop(&self);
    fn cancel(&self);
    fn is_closed(&self) -> bool;
}
struct NativeService(StreamingOwner);
impl Service for NativeService {
    fn key(&self) -> StreamKey {
        self.0.key()
    }
    fn epoch(&self) -> u64 {
        self.0.epoch()
    }
    fn status(&self) -> Option<Status> {
        self.0.status()
    }
    fn poll(&mut self) -> Option<Event> {
        self.0.poll()
    }
    fn activate(
        &mut self,
        key: StreamKey,
        epoch: u64,
        frame: ProgramVideoFrame,
    ) -> Result<(), RejectedFrame> {
        self.0.activate(key, epoch, frame)
    }
    fn video(
        &mut self,
        key: StreamKey,
        epoch: u64,
        frame: ProgramVideoFrame,
    ) -> Result<(), RejectedFrame> {
        self.0.video(key, epoch, frame)
    }
    fn stop(&self) {
        self.0.stop();
    }
    fn cancel(&self) {
        self.0.cancel();
    }
    fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}
pub(super) struct StartedService {
    pub(super) owner: Box<dyn Service>,
    pub(super) audio_sink: Option<skylive_host::audio_output::StreamingAudioSink>,
}
pub(super) type ServiceFactory = Box<dyn FnMut(Config) -> Result<StartedService, StreamingError>>;
fn native_service(config:Config,render_gpu:skylive_host::render_gpu::RenderGpu)->Result<StartedService,StreamingError> {
    let started = StreamingOwner::start_with_render_gpu(config,render_gpu)?;
    Ok(StartedService {
        owner: Box::new(NativeService(started.owner)),
        audio_sink: started.audio_sink,
    })
}

pub(super) struct Runtime {
    owner: Option<Box<dyn Service>>,
    factory: ServiceFactory,
    config: Option<Config>,
    ready: bool,
    origin: Option<MediaTime>,
    last_tick: Option<u64>,
    stopping: bool,
    cancelled_start: bool,
    failure_reported: bool,
    close_after: bool,
    resource_generation: u64,
    pub view: views::StreamingViewState,
}
impl Runtime {
    pub(super) fn hotkey_intent(&self,action:skylive_core::HotkeyAction)->Option<Intent>{
        use skylive_core::HotkeyAction as Action;match action{
            Action::StartStreaming=>(!self.active()).then_some(Intent::StartStreaming),
            Action::StopStreaming=>(!self.stopping&&self.owner.as_ref().is_some_and(|owner|!owner.is_closed()&&owner.status().is_some_and(|status|status.ever_published))).then_some(Intent::StopStreaming),_=>None,
        }
    }

    #[cfg(test)]
    pub fn new() -> Self {
        Self::new_with_render_gpu(skylive_host::render_gpu::RenderGpu::cpu())
    }
    pub fn new_with_render_gpu(render_gpu:skylive_host::render_gpu::RenderGpu)->Self {
        Self {
            owner: None,
            factory: Box::new(move|config|native_service(config,render_gpu.clone())),
            config: None,
            ready: false,
            origin: None,
            last_tick: None,
            stopping: false,
            cancelled_start: false,
            failure_reported: false,
            close_after: false,
            resource_generation: 0,
            view: views::StreamingViewState {
                available: cfg!(target_os = "windows"),
                ..Default::default()
            },
        }
    }
    #[cfg(test)]
    pub(super) fn with_factory(factory: ServiceFactory) -> Self {
        let mut runtime = Self::new();
        runtime.factory = factory;
        runtime.view.available = true;
        runtime
    }
    pub fn active(&self) -> bool {
        self.owner.as_ref().is_some_and(|owner| !owner.is_closed())
    }
    pub fn blocks_command(&self,command:&Command)->bool{self.config.as_ref().is_none_or(|config|config.blocks_command(command))}
    pub fn statistics(&self) -> Option<Status> { self.owner.as_ref().and_then(|owner| owner.status()) }
    pub fn needs_audio(&self) -> bool {
        self.active()
            && !self.stopping
            && self
                .config
                .as_ref()
                .is_some_and(|config| config.audio().is_some())
    }
    pub fn close(&mut self) {
        if let Some(owner) = &self.owner {
            owner.cancel();
        }
    }
    pub fn invalidate_resources(&mut self) -> Result<(), String> {
        self.resource_generation = self
            .resource_generation
            .checked_add(1)
            .ok_or("Streaming resource generation exhausted")?;
        Ok(())
    }
}
impl StudioApp {
    pub(super) fn start_streaming(&mut self) {
        if self.streaming.active() {return;}
        if self.process_priority.advanced_pending(){self.message(true,"Wait for the Advanced settings operation to finish.");return;}
        if self.io.busy() || self.documents.modal_open() {
            self.message(
                true,
                "Finish the document operation before starting streaming",
            );
            return;
        }
        if !self.streaming.view.available {
            self.message(true, "Streaming is unavailable on this platform");
            return;
        }
        let config = match Config::from_session(&self.session) {
            Ok(value) => value,
            Err(error) => {
                self.message(true, error.to_string());
                return;
            }
        };
        if config.audio().is_some() && self.audio.is_busy() {
            self.message(true, "Audio worker is busy; try streaming again");
            return;
        }
        let started = match (self.streaming.factory)(config.clone()) {
            Ok(value) => value,
            Err(error) => {
                self.message(true, error.to_string());
                return;
            }
        };
        self.streaming.owner = Some(started.owner);
        self.streaming.config = Some(config);
        self.streaming.ready = false;
        self.streaming.origin = None;
        self.streaming.last_tick = None;
        self.streaming.stopping = false;
        self.streaming.cancelled_start = false;
        self.streaming.failure_reported = false;
        self.streaming.view.warnings = None;
        self.streaming.view.degraded_video_frames = 0;
        self.streaming.view.active = true;
        self.streaming.view.finishing = false;
        self.streaming.view.phase = Some(Phase::Initializing);
        if let Some(sink) = started.audio_sink {
            let Some(output) = self.sync_transition() else {
                self.stop_streaming();
                self.message(true, "Program output is unavailable for streaming");
                return;
            };
            if let Err(error) = self.audio.request_program_with_output(
                &self.session,
                &output.output,
                self.frame_now,
                skylive_host::audio_output::AudioOutput::Streaming(sink),
            ) {
                if let Some(owner) = &self.streaming.owner {
                    owner.cancel();
                }
                self.streaming.stopping = true;
                self.streaming.view.finishing = true;
                self.message(true, error.to_string());
                return;
            }
        }
        self.message(false, "Starting streaming…");
    }
    pub(super) fn stop_streaming(&mut self) {
        if let Some(owner) = &self.streaming.owner {
            if !owner.is_closed() {
                if self.streaming.origin.is_none()&&!owner.status().is_some_and(|s|s.ever_published) {
                    owner.cancel();
                    self.streaming.cancelled_start = true;
                } else {
                    owner.stop();
                }
                self.streaming.stopping = true;
                self.streaming.view.finishing = true;
            }
        }
    }
    pub(super) fn poll_streaming(&mut self, context: &egui::Context) {
        let mut message = None;
        if let Some(owner) = &mut self.streaming.owner {
            while let Some(event) = owner.poll() {
                match event {
                    Event::ReadyForInputs { key, epoch }
                        if key == owner.key()
                            && epoch == owner.epoch()
                            && key.session == self.session.identity()
                            && !self.streaming.stopping =>
                    {
                        self.streaming.view.connection_epoch=epoch;self.streaming.origin=None;self.streaming.last_tick=None;self.streaming.ready = true
                    }
                    Event::Started { key, epoch, .. }
                        if key == owner.key() && epoch == owner.epoch() =>
                    {
                        message = Some((false, crate::status::Status::from("Streaming started")))
                    }
                    Event::Stopped(status) if status.key == owner.key() => {
                        message = Some((false, crate::status::Status::from("Streaming stopped")))
                    }
                    Event::Failed(status) if status.key == owner.key() => {
                        if self.streaming.cancelled_start
                            && matches!(
                                status.failure.as_deref(),
                                Some(skylive_host::streaming::Error::Cancelled)
                            )
                        {
                            message = Some((
                                false,
                                crate::status::Status::from("Streaming start cancelled"),
                            ));
                            continue;
                        }
                        message = Some((
                            true,
                            crate::status::Status::formatted(
                                "Streaming failed: {error}",
                                &[(
                                    "{error}",
                                    status.failure.map_or_else(
                                        || "Unknown streaming failure".into(),
                                        |error| error.to_string(),
                                    ),
                                )],
                            ),
                        ));
                    }
                    _ => {}
                }
            }
            if let Some(status) = owner.status() {
                if let Err(error)=self.audio.observe_streaming_ndi_receipts(&self.session,&status) {
                    message=Some((true,crate::status::Status::formatted("Streaming NDI alignment: {error}", &[("{error}",error.to_string())])));
                }
                if status.connection_epoch!=self.streaming.view.connection_epoch{self.streaming.origin=None;self.streaming.last_tick=None;self.streaming.ready=false;}
                self.streaming.view.connection_epoch=status.connection_epoch;self.streaming.view.reconnect_attempt=status.reconnect_attempt;self.streaming.view.reconnect_limit=self.streaming.config.as_ref().map_or(0,|c|c.settings().reconnect().max_attempts());self.streaming.view.reconnect_wait_ms=status.reconnect_wait_ms;self.streaming.view.reconnect_error=status.reconnect_last_error.clone();
                if matches!(status.phase, Phase::Finishing | Phase::Retiring) {
                    self.streaming.stopping = true;
                    self.streaming.view.finishing = true;
                }
                if !self.streaming.failure_reported
                    && status.phase == Phase::Retiring
                    && !self.streaming.cancelled_start
                {
                    if let Some(error) = &status.failure {
                        message = Some((
                            true,
                            crate::status::Status::formatted(
                                "Streaming failed; closing its output resources: {error}",
                                &[("{error}", error.to_string())],
                            ),
                        ));
                        self.streaming.failure_reported = true;
                    }
                }
                self.streaming.view.phase = Some(status.phase);
                self.streaming.view.warnings = status.warnings;
                self.streaming.view.degraded_video_frames = status.degraded_frames;
            }
        }
        if let Some((error, text)) = message {
            self.message(error, text);
        }
        self.streaming.view.active = self.streaming.active();
        if self.streaming.active() {
            context.request_repaint_after(std::time::Duration::from_millis(10));
        } else {
            self.streaming.view.finishing = false;
            self.streaming.config = None;
            self.streaming.ready = false;
            if self.streaming.close_after {
                self.streaming.close_after = false;
                self.shutdown(context);
            }
        }
    }
    pub(super) fn wait_for_streaming_before_close(&mut self, context: &egui::Context) -> bool {
        if !self.streaming.active() {
            return false;
        }
        self.streaming.close_after = true;
        self.stop_streaming();
        context.request_repaint_after(std::time::Duration::from_millis(10));
        true
    }
    pub(super) fn stream_program_frame(
        &mut self,
        context: &egui::Context,
        sample: &skylive_host::playback::PlaybackSnapshot,
        output: &skylive_host::transition::FrameComposition,
        capture: &Arc<CaptureSnapshot>,
    ) {
        if !self.streaming.active() || self.streaming.stopping {
            return;
        }
        let Some(config) = &self.streaming.config else {
            return;
        };
        let frame_ns = (1_000_000_000u64 * u64::from(config.video().fps_den())
            / u64::from(config.video().fps_num()))
        .max(1);
        let Some(owner) = &mut self.streaming.owner else {
            return;
        };
        if owner.epoch()!=self.streaming.view.connection_epoch{return;}
        if !self.streaming.ready && self.streaming.origin.is_none() {
            return;
        }
        if sample.session() != self.session.identity()
            || sample.revision() != self.session.snapshot().revision()
            || sample.now() != self.frame_now
        {
            context.request_repaint();
            return;
        }
        let initial = self.streaming.origin.is_none();
        let origin = self.streaming.origin.unwrap_or(self.frame_now);
        let Some(elapsed) = self.frame_now.0.checked_sub(origin.0) else {
            self.stop_streaming();
            self.message(true, "Streaming clock moved backwards");
            return;
        };
        let index = (u128::from(elapsed) * u128::from(config.video().fps_num())
            / (1_000_000_000 * u128::from(config.video().fps_den()))) as u64;
        if self
            .streaming
            .last_tick
            .is_some_and(|previous| index <= previous)
        {
            return;
        }
        let frame = ProgramVideoFrame {
            session: self.session.identity(),
            time: self.frame_now,
            model: self.session.snapshot(),
            output: output.clone(),
            playback: sample.clone(),
            capture: capture.clone(),
            resource_generation: self.streaming.resource_generation,
        };
        let result = if initial {
            owner.activate(owner.key(), owner.epoch(), frame)
        } else {
            owner.video(owner.key(), owner.epoch(), frame)
        };
        match result {
            Ok(()) => {
                self.streaming.last_tick = Some(index);
                if initial {
                    self.streaming.origin = Some(origin);
                    self.streaming.ready = false;
                }
            }
            Err(rejected) if matches!(rejected.error, skylive_host::streaming::Error::Limit(_)|skylive_host::streaming::Error::Reconnecting) => {
            }
            Err(rejected) => {
                self.stop_streaming();
                self.message(true, rejected.error.to_string());
            }
        }
        context.request_repaint_after(std::time::Duration::from_nanos(frame_ns));
    }
}

#[cfg(test)]
#[path = "app_streaming_test_support.rs"]
pub(super) mod test_support;
#[cfg(test)]
#[path = "app_streaming_tests.rs"]
mod tests;
