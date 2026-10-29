//! Frontend coordination only: owned host worker renders, encodes and publishes.
use super::*;
use crate::views::RecordingViewState as View;
use skylive_host::{
    playback::PlaybackSnapshot,
    recording::{RecordingConfig, RecordingEvent, RecordingFrame, RecordingOwner},
};
pub(super) struct Runtime {
    factory: Box<dyn FnMut() -> skylive_host::recording::RecordingFactory>,
    owner: RecordingOwner,
    config: Option<RecordingConfig>,
    origin: MediaTime,
    last_tick: Option<u64>,
    stopping: bool,
    close_after: bool,
    resource_generation: u64,
    admission_error: Option<String>,
    pending_start: Option<PendingStart>,
    pub view: View,
}
struct PendingStart {
    session: u64,
    revision: u64,
    profile: skylive_core::ProfileId,
    recording: skylive_core::RecordingSettings,
    audio: skylive_core::AudioSettings,
    video: skylive_core::VideoSettings,
    output: skylive_core::OutputVideoSettings,
    started: std::time::Instant,
}
impl Runtime {
    #[cfg(test)]
    pub fn new() -> std::io::Result<Self> {
        Self::new_with_render_gpu(skylive_host::render_gpu::RenderGpu::cpu())
    }
    pub fn new_with_render_gpu(render_gpu:skylive_host::render_gpu::RenderGpu)->std::io::Result<Self> {
        Ok(Self {
            factory: Box::new(skylive_host::recording::native_factory),
            owner: RecordingOwner::new_with_render_gpu(render_gpu)?,
            config: None,
            origin: MediaTime(0),
            last_tick: None,
            stopping: false,
            close_after: false,
            resource_generation: 0,
            admission_error: None,
            pending_start: None,
            view: View {
                available: skylive_host::recording::available(),
                ..Default::default()
            },
        })
    }
    pub(super) fn hotkey_intent(&mut self,action:skylive_core::HotkeyAction)->Option<Intent>{
        use skylive_core::HotkeyAction as Action;let state=self.owner.status();let active=self.owner.is_active()||self.pending_start.is_some();
        match action{
            Action::StartRecording=>(!active).then_some(Intent::StartRecording),
            Action::StopRecording=>(self.pending_start.is_some()||(active&&state.active&&!state.starting&&!state.finishing)).then_some(Intent::StopRecording),
            Action::PauseRecording=>(active&&state.active&&state.recording!=0&&state.session!=0&&!state.starting&&!state.finishing&&!state.pause_pending&&!state.paused).then_some(Intent::SetRecordingPaused{recording:state.recording,session:state.session,paused:true}),
            Action::ResumeRecording=>(active&&state.active&&state.recording!=0&&state.session!=0&&!state.starting&&!state.finishing&&!state.pause_pending&&state.paused).then_some(Intent::SetRecordingPaused{recording:state.recording,session:state.session,paused:false}),_=>None,
        }
    }
    pub fn active(&self) -> bool {
        self.owner.is_active()
    }
    pub fn statistics(&mut self) -> skylive_host::recording::RecordingStatus { self.owner.status() }
    pub fn needs_audio(&self) -> bool {
        self.active()
            && !self.stopping
            && self
                .config
                .as_ref()
                .is_some_and(|config| config.audio.is_some())
    }
    pub fn stop(&mut self) -> Result<(), String> {
        if self.active() {
            self.stopping = true;
            self.view.finishing = true;
            self.owner.stop().map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    pub fn invalidate_resources(&mut self) -> Result<(), String> {
        self.resource_generation = self
            .resource_generation
            .checked_add(1)
            .ok_or("Recording resource generation exhausted")?;
        Ok(())
    }
    pub fn close(&mut self) {
        self.pending_start = None;
        self.owner.close();
    }
}
pub(super) fn blocks_command(command: &Command) -> bool {
    skylive_host::recording::blocks_format_change(command)
}
impl StudioApp {
    pub(super) fn start_recording(&mut self) {
        if !self.recording.view.available {
            self.message(true, "Recording is unavailable on this platform");
            return;
        }
        if self.recording.active() || self.recording.pending_start.is_some() {
            return;
        }
        let model = self.session.snapshot();
        // Program audio is a bounded actor, and it may be servicing an
        // ordinary periodic request at the instant Start is clicked. Keep a
        // single scoped request instead of making the user race its queue.
        if model.profiles().selected().recording().audio_enabled() && self.audio.is_busy() {
            self.recording.pending_start = Some(PendingStart {
                session:self.session.identity(), revision:model.revision(),
                profile:model.profiles().selected().id(), recording:model.profiles().selected().recording().clone(),
                audio:model.audio(), video:model.video().clone(),
                output:model.profiles().selected().output_video_settings(), started:std::time::Instant::now(),
            });
            self.recording.view.active=true;
            self.recording.view.starting=true;
            self.recording.view.finishing=false;
            self.message(false,"Waiting for audio to start recording…");
            return;
        }
        let directory = model.profiles().selected().recording().directory();
        let directory = if directory.is_empty() {
            skylive_host::recording::default_directory()
        } else {
            PathBuf::from(directory)
        };
        let stamp = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(value) => value,
            Err(error) => {
                self.message(true, error.to_string());
                return;
            }
        };
        let path = directory.join(format!(
            "SkyLive-{}-{:09}.{}",
            stamp.as_secs(),
            stamp.subsec_nanos(),
            model.profiles().selected().recording().container().extension()
        ));
        let plan = match skylive_host::recording::MultitrackRecordingConfig::from_session(&self.session, path) {
            Ok(value) => value,
            Err(error) => {
                self.message(true, error.to_string());
                return;
            }
        };
        let config=plan.base().clone();
        let mask=plan.tracks().iter().fold(0,|mask,track|mask|track.id.bit());
        let factory=if config.audio.is_none()||model.profiles().selected().recording().audio_tracks().is_default(){
            (self.recording.factory)()
        }else{skylive_host::recording::native_multitrack_factory(plan)};
        if config.audio.is_some() && self.audio.is_busy() {
            self.message(true, "Audio worker is busy; try recording again");
            return;
        }
        match self.recording.owner.start_tracks(
            self.session.identity(),
            config.clone(),
            self.frame_now,
            factory,
            mask,
        ) {
            Ok(()) => {
                self.recording.config = Some(config);
                self.recording.origin = self.frame_now;
                self.recording.last_tick = None;
                self.recording.stopping = false;
                self.recording.admission_error = None;
                self.recording.view.active = true;
                self.recording.view.starting=true;self.recording.view.paused=false;self.recording.view.pause_pending=false;
                self.recording.view.finishing = false;
                self.audio_next_request = None;
                if self.recording.needs_audio() {
                    let sink = match self.recording.owner.audio_sink(model.audio()) {
                        Ok(sink) => sink,
                        Err(error) => {
                            self.recording.admission_error = Some(error.to_string());
                            self.stop_recording();
                            self.message(true, error.to_string());
                            return;
                        }
                    };
                    let Some(output) = self.sync_transition() else {
                        drop(sink);
                        self.stop_recording();
                        return;
                    };
                    if let Err(error) = self.audio.request_program_with_recording(
                        &self.session,
                        &output.output,
                        self.frame_now,
                        sink,
                    ) {
                        self.recording.admission_error = Some(error.to_string());
                        self.stop_recording();
                        self.message(true, error.to_string());
                        return;
                    }
                    self.audio_program_revision =
                        Some((self.session.identity(), self.session.snapshot().revision()));
                }
                self.message(false, "Starting recording…");
            }
            Err(error) => self.message(true, error.to_string()),
        }
    }
    pub(super) fn set_recording_paused(&mut self,recording:u64,session:u64,paused:bool) {
        if session!=self.session.identity() {self.message(true,"Recording pause belongs to another session");return;}
        match self.recording.owner.set_paused(recording,session,self.frame_now,paused) {
            Ok(Some(_))=>{self.recording.view.pause_pending=true;self.message(false,if paused {"Pausing recording…"} else {"Resuming recording…"});}
            Ok(None)=>{},Err(error)=>self.message(true,error.to_string()),
        }
    }
    pub(super) fn stop_recording(&mut self) {
        if self.cancel_pending_recording_start() {
            self.message(false, "Recording start cancelled.");
            return;
        }
        if let Err(error) = self.recording.stop() {
            self.message(true, error);
        } else if self.recording.active() {
            self.message(false, "Finishing recording…");
        }
    }
    pub(super) fn cancel_pending_recording_start(&mut self)->bool {
        if self.recording.pending_start.take().is_none(){return false;}
        self.recording.view.active=false;
        self.recording.view.starting=false;
        self.recording.view.finishing=false;
        true
    }
    /// Called after the audio owner is polled and UI intents are handled, but
    /// before another periodic Program request can claim its single slot.
    pub(super) fn service_pending_recording_start(&mut self,context:&egui::Context) {
        let Some(pending)=self.recording.pending_start.as_ref() else{return;};
        let model=self.session.snapshot();
        let valid=!self.session.is_closed() && !self.allow_close && !self.close_dialog
            && pending.session==self.session.identity() && pending.revision==model.revision()
            && pending.profile==model.profiles().selected().id()
            && pending.recording==*model.profiles().selected().recording()
            && pending.audio==model.audio() && pending.video==*model.video()
            && pending.output==model.profiles().selected().output_video_settings();
        if !valid {
            self.cancel_pending_recording_start();
            self.message(true,"Recording start cancelled because the session or settings changed.");
            return;
        }
        if pending.started.elapsed()>=std::time::Duration::from_secs(5) {
            self.cancel_pending_recording_start();
            self.message(true,"Audio did not become available in time to start recording.");
            return;
        }
        if self.audio.is_busy() {
            context.request_repaint_after(std::time::Duration::from_millis(10));
            return;
        }
        self.cancel_pending_recording_start();
        self.start_recording();
        context.request_repaint();
    }
    pub(super) fn poll_recording(&mut self, context: &egui::Context) {
        for _ in 0..3 {
            match self.recording.owner.poll() {
                Ok(Some(RecordingEvent::Started)) => {self.recording.view.starting=false;self.message(false,"Recording");},
                Ok(Some(RecordingEvent::PauseChanged {result,..}))=>{
                    self.recording.view.pause_pending=false;
                    match result {
                        Ok(receipt)=>{self.recording.view.paused=receipt.paused;if !self.recording.stopping {self.message(false,if receipt.paused {"Recording paused"} else {"Recording resumed"});}}
                        Err(error)=>self.message(true,error.to_string()),
                    }
                }
                Ok(Some(RecordingEvent::Finished { path, .. })) => {
                    self.recording.view.active = false;
                    self.recording.view.starting=false;self.recording.view.paused=false;self.recording.view.pause_pending=false;
                    self.recording.view.finishing = false;
                    self.recording.config = None;
                    let problem = self.recording.admission_error.take();
                    let message = if let Some(error) = &problem {
                        crate::status::Status::formatted(
                            "Recording stopped after an input error. Saved prefix: {path}. {error}",
                            &[
                                ("{path}", path.display().to_string()),
                                ("{error}", error.clone()),
                            ],
                        )
                    } else {
                        crate::status::Status::formatted(
                            "Recording saved: {path}",
                            &[("{path}", path.display().to_string())],
                        )
                    };
                    self.message(problem.is_some(), message);
                }
                Ok(Some(RecordingEvent::Failed(error))) => {
                    self.recording.view.active = false;
                    self.recording.view.starting=false;self.recording.view.paused=false;self.recording.view.pause_pending=false;
                    self.recording.view.finishing = false;
                    self.recording.config = None;
                    let detail = if let Some(path) = error.recovery_path {
                        crate::status::Status::formatted(
                            "Recording failed: {error}. Recovery file: {path}",
                            &[
                                ("{error}", error.detail),
                                ("{path}", path.display().to_string()),
                            ],
                        )
                    } else {
                        crate::status::Status::formatted(
                            "Recording failed: {error}",
                            &[("{error}", error.detail)],
                        )
                    };
                    self.message(true, detail);
                }
                Ok(None) => break,
                Err(error) => {
                    self.message(true, error.to_string());
                    break;
                }
            }
        }
        let recording_status = self.recording.owner.status();
        if let Err(error)=self.audio.observe_recording_camera_receipts(&self.session,&recording_status) {
            self.message(true,format!("Recording camera alignment: {error}"));
        }
        self.recording.view.recording=recording_status.recording;
        self.recording.view.session=recording_status.session;
        self.recording.view.starting=recording_status.starting||self.recording.pending_start.is_some();
        self.recording.view.paused=recording_status.paused;
        self.recording.view.pause_pending=recording_status.pause_pending;
        self.recording.view.degraded_video_frames = recording_status.degraded_video_frames;
        self.recording.view.warnings = recording_status.warnings;
        if self.recording.active() {
            context.request_repaint_after(std::time::Duration::from_millis(10));
        } else if self.recording.close_after {
            self.recording.close_after = false;
            self.shutdown(context);
        }
    }
    pub(super) fn wait_for_recording_before_close(&mut self, context: &egui::Context) -> bool {
        if !self.recording.active() {
            return false;
        }
        self.recording.close_after = true;
        self.stop_recording();
        context.request_repaint_after(std::time::Duration::from_millis(10));
        true
    }
    pub(super) fn record_initial_program_frame(
        &mut self,
        context: &egui::Context,
        sample: &PlaybackSnapshot,
        output: &skylive_host::transition::FrameComposition,
        capture: &Arc<CaptureSnapshot>,
    ) {
        let model = self.session.snapshot();
        if sample.session() != self.session.identity()
            || sample.revision() != model.revision()
            || sample.now() != self.frame_now
        {
            context.request_repaint();
            return;
        }
        // Cut, manual/quick transitions and Studio changes have runtime
        // identities independent of the immutable authored revision. A fresh
        // checked output must match the complete sampled composition, including
        // manual progress, null endpoints and captured scene instance keys.
        let Some(current) = self.sync_transition() else {
            context.request_repaint();
            return;
        };
        if current.session == self.session.identity()
            && current.revision == model.revision()
            && current.output.sample_key() == output.sample_key()
        {
            self.record_program_frame(context, sample, output, capture);
        }
        context.request_repaint();
    }
    pub(super) fn record_program_frame(
        &mut self,
        context: &egui::Context,
        sample: &PlaybackSnapshot,
        output: &skylive_host::transition::FrameComposition,
        capture: &Arc<CaptureSnapshot>,
    ) {
        if !self.recording.active() || self.recording.stopping || self.recording.view.paused {
            return;
        }
        let Some(config) = &self.recording.config else {
            return;
        };
        let Some(elapsed) = self.frame_now.0.checked_sub(self.recording.origin.0) else {
            self.stop_recording();
            self.message(true, "Recording clock moved backwards");
            return;
        };
        let index = (u128::from(elapsed) * u128::from(config.video.fps_num())
            / (1_000_000_000 * u128::from(config.video.fps_den()))) as u64;
        let frame_ns = (1_000_000_000u64 * u64::from(config.video.fps_den())
            / u64::from(config.video.fps_num()))
        .max(1);
        if self
            .recording
            .last_tick
            .is_some_and(|previous| index <= previous)
        {
            return;
        }
        let value = RecordingFrame {
            session: self.session.identity(),
            time: self.frame_now,
            model: self.session.snapshot(),
            output: output.clone(),
            playback: sample.clone(),
            capture: capture.clone(),
            resource_generation: self.recording.resource_generation,
        };
        match self.recording.owner.video(value) {
            Ok(()) => self.recording.last_tick = Some(index),
            Err(error) if matches!(error.code,"recording_backpressure"|"recording_paused") => {}
            Err(error) => {
                self.recording.admission_error = Some(error.to_string());
                self.stop_recording();
                self.message(true, error.to_string());
            }
        }
        context.request_repaint_after(std::time::Duration::from_nanos(frame_ns));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylive_host::recording::{RecordingError, RecordingFactory, RecordingWriter};
    use std::{
        fs,
        io::Write,
        rc::Rc,
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        thread,
        time::Duration,
    };
    static NEXT: AtomicU64 = AtomicU64::new(1);
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "skylive-desktop-recording-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            if let Ok(entries) = fs::read_dir(&self.0) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        if let Ok(files) = fs::read_dir(&path) {
                            for file in files.flatten() {
                                let _ = fs::remove_file(file.path());
                            }
                        }
                        let _ = fs::remove_dir(path);
                    } else {
                        let _ = fs::remove_file(path);
                    }
                }
            }
            let _ = fs::remove_dir(&self.0);
        }
    }
    #[derive(Default)]
    struct State {
        calls: Mutex<Vec<Call>>,
        hold_finalize: AtomicBool,
        audio_calls: AtomicU64,
    }
    #[derive(Debug)]
    enum Call {
        Create(thread::ThreadId),
        Video(u64, [u8; 4]),
        Finalize(thread::ThreadId),
    }
    struct Release(Arc<State>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.hold_finalize.store(false, Ordering::SeqCst);
        }
    }
    struct Writer {
        path: PathBuf,
        state: Arc<State>,
        audio_channels: Option<usize>,
        _local: Rc<()>,
    }
    impl RecordingWriter for Writer {
        fn video(
            &mut self,
            image: &skylive_media::RgbaFrame,
            index: u64,
        ) -> Result<(), RecordingError> {
            self.state
                .calls
                .lock()
                .unwrap()
                .push(Call::Video(index, image.pixels()[..4].try_into().unwrap()));
            Ok(())
        }
        fn audio(&mut self, samples: &[i16], _: u64) -> Result<(), RecordingError> {
            let channels = self
                .audio_channels
                .expect("video-only fixture received audio");
            assert!(!samples.is_empty() && samples.len() % channels == 0);
            assert!(samples.len() / channels <= 4096);
            self.state.audio_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn finalize(&mut self) -> Result<(), RecordingError> {
            self.state
                .calls
                .lock()
                .unwrap()
                .push(Call::Finalize(thread::current().id()));
            while self.state.hold_finalize.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(1));
            }
            let mut file = fs::File::create(&self.path).unwrap();
            for kind in [b"ftyp", b"moov", b"mdat"] {
                file.write_all(&8u32.to_be_bytes()).unwrap();
                file.write_all(kind).unwrap();
            }
            Ok(())
        }
    }
    fn factory(state: Arc<State>) -> RecordingFactory {
        Box::new(move |config, path| {
            state
                .calls
                .lock()
                .unwrap()
                .push(Call::Create(thread::current().id()));
            fs::File::create(path).unwrap();
            Ok(Box::new(Writer {
                path: path.into(),
                state,
                audio_channels: config.audio.map(|audio| usize::from(audio.channels)),
                _local: Rc::new(()),
            }))
        })
    }
    fn frame(
        app: &mut StudioApp,
        context: &egui::Context,
        now: u64,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| app.frame_at(ctx, MediaTime(now)),
        )
    }
    fn point(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|shape| point(shape, label)),
            _ => None,
        }
    }
    fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }
    fn click(app: &mut StudioApp, context: &egui::Context, label: &str) {
        let now = app.frame_now.0;
        frame(app, context, now, vec![]);
        let output = frame(app, context, now, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| {
                point(&shape.shape, label).filter(|pos| shape.clip_rect.contains(*pos))
            })
            .unwrap_or_else(|| panic!("recording control {label} is visible"));
        frame(app, context, now, pointer(pos, true));
        frame(app, context, now, pointer(pos, false));
    }
    fn settle(
        app: &mut StudioApp,
        context: &egui::Context,
        condition: impl Fn(&StudioApp) -> bool,
    ) {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let now = app.frame_now.0;
            frame(app, context, now, vec![]);
            if condition(app) {
                return;
            }
            assert!(
                Instant::now() < until,
                "recording frontend acknowledgment timed out: {}",
                app.status
            );
            thread::yield_now();
        }
    }
    fn fixture(context: &egui::Context, directory: &Directory, state: Arc<State>) -> StudioApp {
        let mut app = StudioApp::headless(context).unwrap();
        app.recording.factory = Box::new(move || factory(state.clone()));
        app.recording.view.available = true;
        app.apply(Command::SetVideo {
            width: 64,
            height: 48,
            fps_num: 30,
            fps_den: 1,
        });
        let profile = app.session.snapshot().profiles().selected_id();
        app.apply(Command::Profile {
            command: skylive_core::ProfileCommand::SetRecording {
                id: profile,
                settings: skylive_core::RecordingSettings::new(
                    directory.0.to_string_lossy().into_owned(),
                    6000,
                    160,
                    false,
                    false,
                )
                .unwrap(),
            },
        });
        let program = app.session.preview();
        app.apply(Command::AddColorSource {
            scene: program,
            name: "Program red".into(),
            rgba: [255, 0, 0, 255],
        });
        app.intent(Intent::StudioMode(true), context);
        app.apply(Command::AddScene {
            name: "Unsent blue preview".into(),
        });
        app.apply(Command::AddColorSource {
            scene: app.session.preview(),
            name: "Preview blue".into(),
            rgba: [0, 0, 255, 255],
        });
        assert_ne!(app.session.preview(), app.session.program());
        app
    }
    #[test]
    fn due_program_control_cannot_starve_start_intent_in_the_same_frame() {
        let directory = Directory::new();
        let state = Arc::new(State::default());
        let _release = Release(state.clone());
        let context = egui::Context::default();
        let mut app = StudioApp::headless(&context).unwrap();
        let writer_state = state.clone();
        app.recording.factory = Box::new(move || factory(writer_state.clone()));
        app.recording.view.available = true;
        app.apply(Command::SetVideo {
            width: 64,
            height: 48,
            fps_num: 30,
            fps_den: 1,
        });
        let profile = app.session.snapshot().profiles().selected_id();
        app.apply(Command::Profile {
            command: skylive_core::ProfileCommand::SetRecording {
                id: profile,
                settings: skylive_core::RecordingSettings::new(
                    directory.0.to_string_lossy().into_owned(),
                    6000,
                    160,
                    true,
                    false,
                )
                .unwrap(),
            },
        });
        // An audio-only file route exercises the real CPU actor; no native
        // backend/device is opened, and no physical sound is generated.
        app.apply(Command::AddMediaSource {
            scene: app.session.program(),
            name: "Pending file audio".into(),
            media: skylive_core::MediaSource::new("injected-missing-audio.mp4".into(), 0, 0, true)
                .unwrap(),
        });
        let output = app.sync_transition().unwrap().output;
        let active = output.scenes();
        app.audio.synchronize(&app.session, &active).unwrap();
        app.audio
            .request_program(&app.session, &output, MediaTime(0))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.audio.is_busy() {
            app.audio.poll(&app.session).unwrap();
            assert!(
                Instant::now() < deadline,
                "real AudioActor initial route acknowledgment"
            );
            thread::sleep(Duration::from_millis(1));
        }
        app.audio_next_request = Some(0);
        let _ = context.run(egui::RawInput::default(), |ctx| {
            // This is the production frame order: poll, user intents, then
            // ordinary control admission. The old update_audio takes the slot.
            app.update_audio(ctx, &active, Some(&output), MediaTime(0));
            assert!(
                !app.audio.is_busy(),
                "periodic Program admission must wait for intents"
            );
            app.intent(Intent::StartRecording, ctx);
            assert!(app.recording.active(), "{}", app.status);
            app.intent(Intent::StartRecording, ctx);
            app.admit_authored_audio(ctx, MediaTime(0));
        });
        settle(&mut app, &context, |_| {
            state
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| matches!(call, Call::Create(_)))
        });
        assert_eq!(
            state
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|call| matches!(call, Call::Create(_)))
                .count(),
            1
        );
        assert!(app.recording.active());
        settle(&mut app, &context, |_| {
            // Audio and video workers acknowledge independently. Stopping on
            // the first audio block could cancel the first video frame and
            // correctly produce an empty-recording error under test load.
            state.audio_calls.load(Ordering::SeqCst) != 0
                && state
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|call| matches!(call, Call::Video(_, _)))
        });
        app.stop_recording();
        settle(&mut app, &context, |app| !app.recording.active());
    }
    fn preview_rect(shape: &egui::epaint::Shape) -> Option<egui::Rect> {
        let rect = match shape {
            // egui Image paints a textured Rect before tessellation; accepting
            // only Mesh incorrectly misses the actual visible preview image.
            egui::epaint::Shape::Rect(rect) if rect.brush.is_some() => Some(rect.rect),
            egui::epaint::Shape::Mesh(mesh) if mesh.vertices.len() >= 4 => Some(mesh.calc_bounds()),
            egui::epaint::Shape::Vec(shapes) => return shapes.iter().find_map(preview_rect),
            _ => None,
        }?;
        if rect.width() > 32.0 && rect.height() > 32.0 && rect.center().x < 640.0 {
            Some(rect)
        } else {
            None
        }
    }
    fn painted_preview(app: &mut StudioApp, context: &egui::Context) -> egui::Rect {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let now = app.frame_now.0;
            let output = frame(app, context, now, vec![]);
            if let Some(rect) = output.shapes.iter().find_map(|shape| {
                preview_rect(&shape.shape).filter(|rect| shape.clip_rect.contains_rect(*rect))
            }) {
                return rect;
            }
            assert!(
                Instant::now() < until,
                "real Preview image must be painted before pointer interaction"
            );
            thread::yield_now();
        }
    }
    #[test]
    fn real_record_buttons_capture_program_while_preview_drag_is_uncommitted_and_guards_keep_model()
    {
        let directory = Directory::new();
        let state = Arc::new(State::default());
        let _release = Release(state.clone());
        let context = egui::Context::default();
        let mut app = fixture(&context, &directory, state.clone());
        settle(&mut app, &context, |app| {
            app.preview
                .displayed_pixel()
                .is_some_and(|pixel| pixel.0 == [0, 0, 255, 255])
        });
        click(&mut app, &context, "Start Recording");
        settle(&mut app, &context, |_| {
            state
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| matches!(call, Call::Video(..)))
        });
        assert!(app.recording.active());
        let baseline = app.session.snapshot();
        let canvas = painted_preview(&mut app, &context);
        let start = canvas.center();
        let moved = start + egui::vec2(canvas.width() * 0.25, 0.0);
        frame(&mut app, &context, 0, pointer(start, true));
        frame(
            &mut app,
            &context,
            33_333_334,
            vec![egui::Event::PointerMoved(moved)],
        );
        frame(&mut app, &context, 66_666_668, vec![]);
        let capture = app.capture_union.snapshot(app.session.identity());
        assert!(
            app.preview_interaction
                .draft_output(&app.session, &capture, app.session.preview(), app.selected)
                .is_some()
        );
        assert!(
            Arc::ptr_eq(&baseline, &app.session.snapshot()),
            "a held pointer drag must not publish a canonical transform"
        );
        frame(
            &mut app,
            &context,
            100_000_002,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        frame(&mut app, &context, 100_000_002, pointer(moved, false));
        assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
        app.intent(
            Intent::Command(Command::SetVideo {
                width: 4,
                height: 4,
                fps_num: 30,
                fps_den: 1,
            }),
            &context,
        );
        assert!(app.error);
        assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
        app.intent(
            Intent::Command(Command::Profile {
                command: skylive_core::ProfileCommand::New {
                    name: "Blocked profile".into(),
                },
            }),
            &context,
        );
        assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
        app.intent(Intent::NewCollection, &context);
        assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
        assert!(!app.io.busy());
        click(&mut app, &context, "Exit");
        assert!(app.close_dialog);
        click(&mut app, &context, "Cancel");
        assert!(!app.close_dialog && !app.allow_close && app.recording.active());
        assert!(Arc::ptr_eq(&baseline, &app.session.snapshot()));
        let path = app.recording.config.as_ref().unwrap().path.clone();
        click(&mut app, &context, "Stop Recording");
        settle(&mut app, &context, |app| !app.recording.active());
        assert!(path.is_file());
        assert!(
            app.status.contains(&path.display().to_string()),
            "{}",
            app.status
        );
        assert!(!app.status.contains("{path}"));
        let calls = state.calls.lock().unwrap();
        let ui_thread = thread::current().id();
        assert!(matches!(calls.first(),Some(Call::Create(id)) if *id!=ui_thread));
        assert!(
            calls
                .iter()
                .any(|call| matches!(call,Call::Finalize(id) if *id!=ui_thread))
        );
        let videos: Vec<_> = calls
            .iter()
            .filter_map(|call| {
                if let Call::Video(index, color) = call {
                    Some((*index, *color))
                } else {
                    None
                }
            })
            .collect();
        assert!(videos.len() >= 2);
        assert!(videos.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(
            videos.iter().all(|(_, color)| *color == [255, 0, 0, 255]),
            "recorded frames must be Program red, never unsent/draft Preview blue"
        );
    }
    #[test]
    fn actual_discard_exit_waits_for_fake_worker_finalization_before_closing_session() {
        let directory = Directory::new();
        let state = Arc::new(State::default());
        let _release = Release(state.clone());
        state.hold_finalize.store(true, Ordering::SeqCst);
        let context = egui::Context::default();
        let mut app = fixture(&context, &directory, state.clone());
        click(&mut app, &context, "Start Recording");
        settle(&mut app, &context, |_| {
            state
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| matches!(call, Call::Video(..)))
        });
        let path = app.recording.config.as_ref().unwrap().path.clone();
        click(&mut app, &context, "Exit");
        assert!(app.close_dialog);
        click(&mut app, &context, "Discard and close");
        assert!(app.recording.close_after && app.recording.view.finishing);
        assert!(!app.allow_close && !app.session.is_closed());
        settle(&mut app, &context, |_| {
            state
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| matches!(call, Call::Finalize(..)))
        });
        assert!(
            !app.allow_close && !path.exists(),
            "publication and window close must wait for finalization"
        );
        state.hold_finalize.store(false, Ordering::SeqCst);
        settle(&mut app, &context, |app| app.allow_close);
        assert!(app.session.is_closed());
        assert!(path.is_file());
        app.recording.owner.join().unwrap();
    }
    #[test]
    fn start_button_admits_timestamp_zero_without_another_ui_frame() {
        let directory = Directory::new();
        let state = Arc::new(State::default());
        let _release = Release(state.clone());
        let context = egui::Context::default();
        let mut app = fixture(&context, &directory, state.clone());
        click(&mut app, &context, "Start Recording");
        assert!(
            app.recording.active(),
            "actual Start button admits recording"
        );
        // Do not redraw/advance the UI here: that hid the original missing
        // timestamp-zero frame. Observe the real worker-owned writer instead.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if state
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| matches!(call, Call::Video(0, [255, 0, 0, 255])))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Start must admit Program frame zero in its own UI frame"
            );
            thread::yield_now();
        }
        frame(&mut app, &context, 33_333_334, vec![]);
        click(&mut app, &context, "Stop Recording");
        settle(&mut app, &context, |app| !app.recording.active());
        let calls = state.calls.lock().unwrap();
        let indices: Vec<_> = calls
            .iter()
            .filter_map(|call| match call {
                Call::Video(index, _) => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(indices, [0, 1]);
        assert_eq!(app.recording.owner.status().skipped_video_frames, 0);
    }

    fn checked_program(
        app: &mut StudioApp,
    ) -> (
        PlaybackSnapshot,
        skylive_host::transition::FrameComposition,
        Arc<CaptureSnapshot>,
    ) {
        let output = app.sync_transition().unwrap().output;
        let scenes = output.scenes();
        app.capture.synchronize(&app.session, &scenes).unwrap();
        let sample = app
            .playback
            .update_outputs(&app.session, &scenes, app.frame_now)
            .unwrap();
        (sample, output, app.capture.snapshot())
    }
    #[test]
    fn initial_recording_rejects_runtime_cut_without_model_revision_and_accepts_fresh_blue_zero() {
        let directory = Directory::new();
        let state = Arc::new(State::default());
        let _release = Release(state.clone());
        let context = egui::Context::default();
        let mut app = fixture(&context, &directory, state.clone());
        let (sample, red, capture) = checked_program(&mut app);
        let revision = app.session.snapshot().revision();
        app.start_recording();
        assert!(app.recording.active());
        app.intent(Intent::Cut, &context);
        assert_eq!(app.session.snapshot().revision(), revision);
        assert_ne!(
            red.sample_key(),
            app.sync_transition().unwrap().output.sample_key()
        );
        app.record_initial_program_frame(&context, &sample, &red, &capture);
        assert_eq!(
            app.recording.last_tick, None,
            "old red snapshot must never be admitted as frame zero"
        );
        let (sample, blue, capture) = checked_program(&mut app);
        app.record_initial_program_frame(&context, &sample, &blue, &capture);
        assert_eq!(app.recording.last_tick, Some(0));
        app.stop_recording();
        settle(&mut app, &context, |app| !app.recording.active());
        let calls = state.calls.lock().unwrap();
        let video: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                Call::Video(i, p) => Some((*i, *p)),
                _ => None,
            })
            .collect();
        assert_eq!(video, [(0, [0, 0, 255, 255])]);
    }
    #[test]
    fn same_scene_manual_or_black_transition_changes_full_sample_identity_before_initial_admission()
    {
        for manual in [false, true] {
            let directory = Directory::new();
            let state = Arc::new(State::default());
            let _release = Release(state.clone());
            let context = egui::Context::default();
            let mut app = fixture(&context, &directory, state);
            let scene = app.session.program();
            app.intent(Intent::SelectScene(scene), &context);
            let (sample, old, capture) = checked_program(&mut app);
            let revision = app.session.snapshot().revision();
            app.start_recording();
            if manual {
                app.transition
                    .manual_position(&mut app.session, 512, app.frame_now)
                    .unwrap();
            } else {
                app.transition
                    .quick(
                        &mut app.session,
                        skylive_core::QuickTransitionId::new(3).unwrap(),
                        app.frame_now,
                    )
                    .unwrap();
            }
            let current = app.sync_transition().unwrap().output;
            assert_eq!(current.primary_scene(), old.primary_scene());
            assert_eq!(app.session.snapshot().revision(), revision);
            assert_ne!(current.sample_key(), old.sample_key());
            app.record_initial_program_frame(&context, &sample, &old, &capture);
            assert_eq!(app.recording.last_tick, None);
            app.stop_recording();
            settle(&mut app, &context, |app| !app.recording.active());
        }
    }
    include!("app_recording_streaming_close_tests.rs");
}
