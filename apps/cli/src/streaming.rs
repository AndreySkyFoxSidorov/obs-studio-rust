//! Caller-driven CLI video control; codecs, network and Program PCM stay on
//! the shared host owners. This module never polls/forwards PCM for streaming.
use super::*;
use skylive_host::{
    audio_output::AudioOutput,
    streaming::{
        Config, Error as StreamError, Event, Phase, ProgramVideoFrame, StartedOwner, Status,
        StreamingOwner,
    },
};
use std::time::{Duration, Instant};
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(try_from = "KeyData")]
pub(super) struct CliKey {
    pub session: u64,
    pub output: u64,
    pub epoch: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyData {
    session: u64,
    output: u64,
    epoch: u64,
}
impl TryFrom<KeyData> for CliKey {
    type Error = &'static str;
    fn try_from(k: KeyData) -> Result<Self, Self::Error> {
        if k.session == 0 || k.output == 0 || k.epoch == 0 {
            return Err("streaming key fields must be nonzero");
        }
        Ok(Self {
            session: k.session,
            output: k.output,
            epoch: k.epoch,
        })
    }
}
pub(super) struct ProgramContext<'a> {
    pub session: &'a mut Session,
    pub controller: &'a mut Controller,
    pub playback: &'a mut PlaybackClock,
    pub capture: &'a mut CaptureWorker,
    pub camera: &'a mut CameraOwner,
    pub union: &'a mut capture::CaptureUnion,
    pub files: &'a mut file_media::Runtime,
    pub audio: &'a mut AudioCaptureWorker,
}
#[derive(Default)]
pub(super) struct Runtime {
    render_gpu: skylive_host::render_gpu::RenderGpu,
    owner: Option<StreamingOwner>,
    config: Option<Config>,
    resource_generation: u64,
    started: bool,
    ready:bool,
    connection_epoch:u64,
    terminal: bool,
}
impl From<StreamError> for CliError {
    fn from(error: StreamError) -> Self {
        Self {
            code: error.code(),
            detail: error.to_string(),
            phase: Some("streaming_output"),
        }
    }
}
fn scoped_error(detail: &str) -> CliError {
    CliError {
        code: "streaming_state",
        detail: detail.into(),
        phase: Some("streaming_output"),
    }
}
impl Runtime {
    pub fn new_with_render_gpu(render_gpu:skylive_host::render_gpu::RenderGpu)->Self {
        let mut owner=Self::default();owner.render_gpu=render_gpu;owner
    }
    pub fn active(&self) -> bool {
        self.owner.as_ref().is_some_and(|o| !o.is_closed())
    }
    pub fn audio_enabled(&self) -> bool {
        self.active() && self.config.as_ref().is_some_and(|c| c.audio().is_some())
    }
    fn check_key(&self, key: CliKey) -> Result<(), CliError> {
        let owner = self
            .owner
            .as_ref()
            .ok_or_else(|| scoped_error("streaming owner is not initialized"))?;
        if key.session != owner.key().session
            || key.output != owner.key().output
            || key.epoch != owner.epoch()
        {
            return Err(StreamError::Scope.into());
        }
        Ok(())
    }
    pub fn invalidate_resources(&mut self) -> Result<(), CliError> {
        self.resource_generation = self
            .resource_generation
            .checked_add(1)
            .ok_or_else(|| scoped_error("streaming resource generation exhausted"))?;
        Ok(())
    }
    pub fn guard(&self, request: &Request) -> Result<(), CliError> {
        if !self.active() {
            return Ok(());
        }
        let replacement = matches!(
            request,
            Request::New
                | Request::Load { .. }
                | Request::Recover { .. }
                | Request::ImportObs { .. }
        );
        let format=matches!(request,Request::Command{command,..}if self.config.as_ref().is_some_and(|config|config.blocks_command(command)));
        if replacement || format {
            return Err(scoped_error(
                "stop streaming before replacing the document or changing its profile/audio/video format",
            ));
        }
        if self.audio_enabled() {
            let rewrites_clock = matches!(
                request,
                Request::Advance { .. }
                    | Request::SelectPreview {
                        time_ns: Some(_),
                        ..
                    }
                    | Request::Cut { time_ns: Some(_) }
                    | Request::StudioMode {
                        time_ns: Some(_),
                        ..
                    }
                    | Request::Transition { time_ns: Some(_) }
                    | Request::QuickTransition {
                        time_ns: Some(_),
                        ..
                    }
                    | Request::TbarPosition {
                        time_ns: Some(_),
                        ..
                    }
                    | Request::TbarRelease { time_ns: Some(_) }
                    | Request::RecordFrame { time_ns: Some(_) }
                    | Request::CaptureFrame { time_ns: Some(_) }
                    | Request::CaptureAudio { time_ns: Some(_) }
                    | Request::MixProgramAudio {
                        advance_ns: 1..,
                        ..
                    }
                    | Request::RenderProgram {
                        time_ns: Some(_),
                        ..
                    }
                    | Request::StartMediaPlayback { time_ns: Some(_) }
                    | Request::RestartMedia {
                        time_ns: Some(_),
                        ..
                    }
                    | Request::StartNativeAudioProgram { time_ns: Some(_) }
                    | Request::StartProgramAudio { time_ns: Some(_) }
                    | Request::StartRecording {
                        time_ns: Some(_),
                        ..
                    }
                    | Request::StartNdiOutput {time_ns:Some(_),..}
                    | Request::NdiOutputFrame {time_ns:Some(_),..}
                    | Request::StopNativeAudioProgram
            );
            if rewrites_clock {
                return Err(scoped_error(
                    "streaming owns a live Program clock; omit explicit time_ns and stop streaming before stopping/restarting that clock",
                ));
            }
        }
        Ok(())
    }
    /// Advance the ONE existing Program clock, never create a streaming clock.
    pub fn refresh_clock(
        &self,
        session: &mut Session,
        controller: &mut Controller,
        audio: &AudioCaptureWorker,
    ) -> Result<(), CliError> {
        if self.active() {
            if let Some(now) = audio.program_now()? {
                controller.sync(session, now)?;
            } else if self.audio_enabled() {
                return Err(scoped_error("streaming Program clock retired"));
            }
        }
        Ok(())
    }
    pub fn synchronize(
        &mut self,
        session: &Session,
        state: &TransitionSnapshot,
        audio: &mut AudioCaptureWorker,
    ) -> Result<(), CliError> {
        if !self.audio_enabled() {
            return Ok(());
        }
        settle_audio(session, audio)?;
        if audio.request_program(session, &state.output, state.now)? {
            wait_audio(session, audio)?;
        }
        self.poll()?;
        Ok(())
    }
    pub fn start(
        &mut self,
        context: ProgramContext<'_>,
        recording: &mut recording::Recorder,
        time_ns: Option<u64>,
    ) -> Result<Value, CliError> {
        let ProgramContext {
            session,
            controller,
            playback,
            capture,
            camera,
            union,
            files,
            audio,
        } = context;
        self.poll()?;
        if self.owner.is_some() && (!self.terminal || self.active()) {
            return Err(scoped_error(
                "consume completion and retire the previous streaming output before another Start",
            ));
        }
        let config = Config::from_session(session)?;
        settle_audio(session, audio)?;
        let now = match (audio.program_now()?, time_ns) {
            (Some(_), Some(_)) if config.audio().is_some() => {
                return Err(scoped_error(
                    "omit time_ns when Program audio is already running",
                ));
            }
            (Some(now), _) => now,
            (None, Some(ns)) => MediaTime(ns),
            (None, None) => controller.now(),
        };
        let state = controller.sync(session, now)?;
        sync_audio_runtime(session, &state, audio)?;
        // Admit/settle the shared Program owner BEFORE an output is created.
        // Existing recording is attached to that same DSP render, not copied.
        if config.audio().is_some() {
            if let Some(sink) = recording.live_sink(session.snapshot().audio())? {
                audio.request_program_with_recording(session, &state.output, state.now, sink)?;
                wait_audio(session, audio)?;
            } else if audio.request_program(session, &state.output, state.now)? {
                wait_audio(session, audio)?;
            }
        }
        files.start(session, &state, audio)?;
        let StartedOwner { owner, audio_sink } = StreamingOwner::start_with_render_gpu(config.clone(),self.render_gpu.clone())?;
        self.connection_epoch=owner.epoch();self.ready=false;
        self.owner = Some(owner);
        self.config = Some(config);
        self.started = false;
        self.terminal = false;
        if let Some(sink) = audio_sink {
            if let Err(error) = audio.request_program_with_output(
                session,
                &state.output,
                state.now,
                AudioOutput::Streaming(sink),
            ) {
                self.cancel_and_retire(session, audio)?;
                return Err(error.into());
            }
            if let Err(error) = wait_audio(session, audio) {
                self.cancel_and_retire(session, audio)?;
                return Err(error);
            }
        }
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let event = self.owner.as_mut().and_then(StreamingOwner::poll);
            match event {
                Some(Event::ReadyForInputs { key, epoch }) => {
                    let frame = self.fresh_frame(
                        ProgramContext {
                            session: &mut *session,
                            controller: &mut *controller,
                            playback: &mut *playback,
                            capture: &mut *capture,
                            camera: &mut *camera,
                            union: &mut *union,
                            files: &mut *files,
                            audio: &mut *audio,
                        },
                        None,
                    )?;
                    let owner = self
                        .owner
                        .as_mut()
                        .ok_or_else(|| scoped_error("streaming owner disappeared"))?;
                    if let Err(rejected) = owner.activate(key, epoch, frame) {
                        owner.cancel();
                        self.retire(session, audio)?;
                        return Err(rejected.error.into());
                    }
                }
                Some(Event::Started { key, epoch, .. }) => {
                    let owner = self
                        .owner
                        .as_ref()
                        .ok_or_else(|| scoped_error("streaming owner disappeared"))?;
                    if key != owner.key() || epoch != owner.epoch() {
                        return Err(scoped_error("foreign streaming Started completion"));
                    }
                    self.started = true;
                    return Ok(
                        json!({"started":true,"status":self.status_value(),"video_schedule":"caller_driven","program_audio":"direct_owner_fanout"}),
                    );
                }
                Some(Event::Failed(status)) => {
                    self.terminal = true;
                    self.retire(session, audio)?;
                    return Err(status_error(&status));
                }
                Some(Event::Stopped(_)) => {
                    self.terminal = true;
                    return Err(scoped_error("streaming stopped before first Program frame"));
                }
                None => {}
            }
            poll_audio(session, audio)?;
            if Instant::now() >= until {
                self.owner.as_ref().unwrap().cancel();
                return Err(scoped_error(
                    "streaming initialization deadline expired; output remains owned/retiring, use Stop/Status",
                ));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn fresh_frame(
        &mut self,
        context: ProgramContext<'_>,
        time_ns: Option<u64>,
    ) -> Result<ProgramVideoFrame, CliError> {
        let ProgramContext {
            session,
            controller,
            playback,
            capture,
            camera,
            union,
            files,
            audio,
        } = context;
        if self.audio_enabled() && time_ns.is_some() {
            return Err(scoped_error(
                "omit time_ns for a live-audio streaming frame",
            ));
        }
        let first = frame_now(self.audio_enabled(), controller, files, audio, time_ns)?;
        controller.sync(session, first)?;
        let (prepared, _) = sync_capture_runtime(session, controller, playback, capture, camera, files.outputs.owner.active(skylive_core::NdiOutputRoute::Preview))?;
        sync_audio_runtime(session, &prepared, audio)?;
        // Any bounded synchronous media controls/IO finish BEFORE the actual
        // origin clock is sampled. Two preroll blocks cannot cover slow IO.
        files.start(session, &prepared, audio)?;
        files.synchronize(session, &prepared, audio)?;
        let now = frame_now(self.audio_enabled(), controller, files, audio, time_ns)?;
        controller.sync(session, now)?;
        let (state, sample) = sync_capture_runtime(session, controller, playback, capture, camera, files.outputs.owner.active(skylive_core::NdiOutputRoute::Preview))?;
        sync_audio_runtime(session, &state, audio)?;
        files.poll(session)?; // nonblocking snapshot/control receive only
        // Start/Frame request real capture, but a pending/lost source remains a
        // typed live warning. No whole-output wait/abort for one missing layer.
        if !capture.is_busy() {
            capture.request_frame(session, state.now)?;
        }
        capture.poll(session, state.now)?;
        camera.poll(session)?;
        Ok(ProgramVideoFrame {
            session: session.identity(),
            time: state.now,
            model: session.snapshot(),
            output: state.output,
            playback: sample,
            capture: merged_capture(session, capture, camera, union, files)?,
            resource_generation: self.resource_generation,
        })
    }
    pub fn frame(
        &mut self,
        context: ProgramContext<'_>,
        key: CliKey,
        time_ns: Option<u64>,
    ) -> Result<Value, CliError> {
        let ProgramContext {
            session,
            controller,
            playback,
            capture,
            camera,
            union,
            files,
            audio,
        } = context;
        self.check_key(key)?;
        self.poll()?;
        if self.active()&&!self.started&&!self.ready{return Err(StreamError::Reconnecting.into());}
        if !self.active() {
            return Err(scoped_error(
                "start streaming before submitting Program video",
            ));
        }
        let activating=self.ready;
        let mut frame = self.fresh_frame(
            ProgramContext {
                session: &mut *session,
                controller: &mut *controller,
                playback: &mut *playback,
                capture: &mut *capture,
                camera: &mut *camera,
                union: &mut *union,
                files: &mut *files,
                audio: &mut *audio,
            },
            time_ns,
        )?;
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let owner = self.owner.as_mut().unwrap();
            let result=if activating{owner.activate(owner.key(),owner.epoch(),frame)}else{owner.video(owner.key(),owner.epoch(),frame)};
            match result {
                Ok(()) => {
                    if activating{self.ready=false;}
                    return Ok(
                        json!({"accepted":true,"status":self.status_value(),"video_schedule":"caller_driven"}),
                    );
                }
                Err(rejected) if matches!(rejected.error, StreamError::Limit(_)) => {
                    frame = *rejected.frame
                }
                Err(rejected) => return Err(rejected.error.into()),
            }
            if Instant::now() >= until {
                return Err(scoped_error(
                    "streaming video admission deadline expired; snapshot was not admitted",
                ));
            }
            poll_audio(session, audio)?;
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn poll(&mut self) -> Result<(), CliError> {
        if let Some(owner) = self.owner.as_mut() {
            let epoch=owner.epoch();if self.connection_epoch!=epoch{self.connection_epoch=epoch;self.started=false;self.ready=false;}
            while let Some(event) = owner.poll() {
                match event {
                    Event::Failed(status) => {
                        self.terminal = true;
                        return Err(status_error(&status));
                    }
                    Event::Stopped(_) => self.terminal = true,
                    Event::Started { .. } => self.started = true,
                    Event::ReadyForInputs {epoch,..} => {self.connection_epoch=epoch;self.ready=true;self.started=false;}
                }
            }
        }
        Ok(())
    }
    pub fn status(&mut self) -> Result<Value, CliError> {
        let failure = self.poll().err();
        let value = json!({"active":self.active(),"needs_fresh_program_frame":self.ready,"status":self.status_value(),"failure":failure,"video_schedule":"caller_driven","program_audio":"direct_owner_fanout"});
        Ok(value)
    }
    fn status_value(&self) -> Value {
        self.owner
            .as_ref()
            .and_then(StreamingOwner::status)
            .map_or(Value::Null, |s| status_dto(&s))
    }
    /// Request only this producer's EOF; acceptance is NOT native drain proof.
    pub fn finish_track(&mut self, key: CliKey, audio: bool) -> Result<Value, CliError> {
        self.check_key(key)?;
        self.poll()?;
        let owner = self
            .owner
            .as_mut()
            .ok_or_else(|| scoped_error("streaming owner is not initialized"))?;
        if audio {
            owner.finish_audio(owner.key(), owner.epoch())?;
        } else {
            owner.finish_video(owner.key(), owner.epoch())?;
        }
        Ok(
            json!({"seal_requested":true,"track":if audio{"audio"}else{"video"},"status":self.status_value()}),
        )
    }
    pub fn stop(
        &mut self,
        session: &Session,
        audio: &mut AudioCaptureWorker,
        key: Option<CliKey>,
    ) -> Result<Value, CliError> {
        if let Some(key)=key{let owner=self.owner.as_ref().ok_or_else(||scoped_error("streaming owner is not initialized"))?;if key.session!=owner.key().session||key.output!=owner.key().output{return Err(StreamError::Scope.into());}}
        // Stop names the stable output operation, so an old connection epoch
        // can still cancel its own currently reconnecting output.
        if let Some(owner) = &self.owner {
            owner.stop();
        }
        self.retire(session, audio)?;
        let failure = self.poll().err();
        if let Some(error) = failure {
            return Err(error);
        }
        if let Some(status) = self.owner.as_ref().and_then(StreamingOwner::status) {
            if status.failure.is_some() {
                return Err(status_error(&status));
            }
        }
        Ok(
            json!({"stopped":true,"status":self.status_value(),"actual_owners_closed":self.owner.as_ref().is_none_or(StreamingOwner::is_closed)}),
        )
    }
    fn cancel_and_retire(
        &mut self,
        session: &Session,
        audio: &mut AudioCaptureWorker,
    ) -> Result<(), CliError> {
        if let Some(owner) = &self.owner {
            owner.cancel();
        }
        self.retire(session, audio)
    }
    fn retire(
        &mut self,
        session: &Session,
        audio: &mut AudioCaptureWorker,
    ) -> Result<(), CliError> {
        let until = Instant::now() + Duration::from_secs(30);
        while self.owner.as_ref().is_some_and(|o| !o.is_closed()) {
            // Program owner continues periodic sink pruning/DSP without GUI PCM.
            // Poll only bounded control acknowledgement, never Program blocks.
            poll_audio(session, audio)?;
            if Instant::now() >= until {
                return Err(scoped_error(
                    "streaming retirement deadline expired; actual owners are still retained",
                ));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(owner) = &self.owner {
            owner.cancel();
            while !owner.is_closed() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}
fn status_error(status: &Status) -> CliError {
    status.failure.as_ref().map_or_else(||scoped_error("streaming failed without a diagnostic"),|e|CliError{code:e.code(),detail:format!("{}; output={} session={} video_frames={} audio_frames={} packets_admitted={} last_publisher_progress_packets={}",e,status.key.output,status.key.session,status.video_frames,status.audio_converted_frames,status.packets_admitted,status.packets_published),phase:Some("streaming_output")})
}
fn status_dto(s: &Status) -> Value {
    json!({"key":{"session":s.key.session,"output":s.key.output},"epoch":s.connection_epoch,"reconnect":{"attempt":s.reconnect_attempt,"total_scheduled":s.reconnect_total,"wait_ms":s.reconnect_wait_ms,"last_error":s.reconnect_last_error.as_ref().map(|e|json!({"code":e.code(),"detail":e.to_string()})),"discarded_queued_video_frames":s.reconnect_video_discarded,"discarded_audio_input_frames":s.reconnect_audio_discarded_input},"phase":phase_name(s.phase),"origin_ns":s.origin.map(|t|t.0),"audio_offset_100ns":s.audio_offset_100ns,"video_frames":s.video_frames,"skipped_video_frames":s.skipped_video_frames,"audio_input_frames":s.audio_input_frames,"audio_converted_frames":s.audio_converted_frames,"audio_gap_frames":s.audio_gap_frames,"video_packets":s.video_packets,"audio_packets":s.audio_packets,"packets_admitted":s.packets_admitted,"packets_published":s.packets_published,"encoded_bytes":s.encoded_bytes,"video_sealed":s.video_sealed,"audio_sealed":s.audio_sealed,"interleave":{"video_packets":s.interleave.video_packets,"audio_packets":s.interleave.audio_packets,"video_bytes":s.interleave.video_bytes,"audio_bytes":s.interleave.audio_bytes,"video_reserved_packets":s.interleave.video_reserved_packets,"audio_reserved_packets":s.interleave.audio_reserved_packets,"video_reserved_bytes":s.interleave.video_reserved_bytes,"audio_reserved_bytes":s.interleave.audio_reserved_bytes,"highest_video_100ns":s.interleave.highest_video_100ns.map(|v|v.to_string()),"highest_audio_100ns":s.interleave.highest_audio_100ns.map(|v|v.to_string()),"video_drained":s.interleave.video_drained,"audio_drained":s.interleave.audio_drained,"waiting":interleave_wait(s.interleave.waiting)},"degraded_frames":s.degraded_frames,"warnings":s.warnings,"failure":s.failure.as_ref().map(|e|json!({"code":e.code(),"detail":e.to_string()})),"native_aac":{"input_frames":s.native_aac.input_frames,"submitted_frames":s.native_aac.submitted_frames,"explicit_padding_frames":s.native_aac.explicit_padding_frames,"packets":s.native_aac.packets,"first_native_pts":s.native_aac.first_native_pts,"last_native_end":s.native_aac.last_native_end}})
}
fn poll_audio(session: &Session, audio: &mut AudioCaptureWorker) -> Result<(), CliError> {
    if let Some(AudioCaptureEvent::Program { result, .. }) = audio.poll(session)? {
        result?;
    }
    Ok(())
}
fn settle_audio(session: &Session, audio: &mut AudioCaptureWorker) -> Result<(), CliError> {
    let end = Instant::now() + Duration::from_secs(10);
    while audio.is_busy() {
        poll_audio(session, audio)?;
        if Instant::now() >= end {
            return Err(AudioCaptureError::Busy.into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}
fn wait_audio(session: &Session, audio: &mut AudioCaptureWorker) -> Result<(), CliError> {
    settle_audio(session, audio)
}

fn interleave_wait(value: skylive_host::streaming::InterleaveWait) -> &'static str {
    use skylive_host::streaming::InterleaveWait;
    match value {
        InterleaveWait::None => "none",
        InterleaveWait::FirstVideo => "first_video",
        InterleaveWait::FirstAudio => "first_audio",
        InterleaveWait::VideoWatermark => "video_watermark",
        InterleaveWait::AudioWatermark => "audio_watermark",
        InterleaveWait::Network => "network",
    }
}
fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Initializing => "initializing",
        Phase::Connecting => "connecting",
        Phase::ReconnectRetiring=>"reconnect_retiring",
        Phase::ReconnectWaiting=>"reconnect_waiting",
        Phase::ReadyAwaitingInput => "ready_awaiting_input",
        Phase::Publishing => "publishing",
        Phase::Finishing => "finishing",
        Phase::Retiring => "retiring",
        Phase::Closed => "closed",
    }
}

pub(super) fn capabilities() -> Value {
    json!({"available":cfg!(target_os="windows"),"transports":["rtmp","rtmps"],"rtmps":cfg!(target_os="windows"),"rtmps_tls":"schannel_tls12_system_trust_hostname_and_revocation_validation","rtmps_default_port":443,"rtmp_handshake":"simple_v3_or_hmac_sha256_scheme_0_1","video":"h264_baseline_nv12_sdr_bt709_limited","encoders":skylive_host::native_h264::encoder_capabilities(),"audio":"aac_lc_48000_mono_or_stereo","video_schedule":"caller_driven","audio_delivery":"direct_program_owner_fanout","automatic_reconnect":{"enabled_by_default":true,"default_delay_seconds":2,"default_attempts":25,"delay_seconds":[1,30],"max_attempts":[1,10000],"backoff_ms_max":900000,"initial_connect_failure_retried":false,"fresh_program_frame_required":true,"offline_media_replayed":false},"native_verified":false,"service_compatibility_verified":false})
}

fn frame_now(
    live_audio: bool,
    controller: &Controller,
    files: &file_media::Runtime,
    audio: &AudioCaptureWorker,
    explicit: Option<u64>,
) -> Result<MediaTime, CliError> {
    if let Some(ns) = explicit {
        if live_audio {
            return Err(scoped_error(
                "a live Program frame cannot use an independent caller timestamp",
            ));
        }
        return Ok(MediaTime(ns));
    }
    if let Some(now) = audio.program_now()? {
        return Ok(now);
    }
    if live_audio {
        return Err(scoped_error("streaming Program clock is unavailable"));
    }
    if files.running {
        if let Some(now) = files.owner.media_now()? {
            return Ok(now);
        }
    }
    Ok(controller.now())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_status_and_key_validation_are_inert() {
        let mut runtime = Runtime::default();
        let status = runtime.status().unwrap();
        assert_eq!(status["active"], false);
        assert!(status["status"].is_null());
        assert_eq!(status["video_schedule"], "caller_driven");
        for key in [
            json!({"session":0,"output":1,"epoch":1}),
            json!({"session":1,"output":0,"epoch":1}),
            json!({"session":1,"output":1,"epoch":0}),
            json!({"session":1,"output":1,"epoch":1,"unknown":true}),
        ] {
            assert!(serde_json::from_value::<CliKey>(key).is_err());
        }
        let before = runtime.resource_generation;
        assert!(
            runtime
                .check_key(CliKey {
                    session: 1,
                    output: 1,
                    epoch: 1
                })
                .is_err()
        );
        assert_eq!(runtime.resource_generation, before);
    }
    #[test]
    fn bounded_dto_keeps_failure_text_and_prefix_counters_exact() {
        let s = Status {
            key: skylive_host::streaming::StreamKey {
                session: 7,
                output: 8,
            },
            phase: Phase::Closed,
            origin: Some(MediaTime(9)),
            video_frames: 2,
            skipped_video_frames: 3,
            audio_input_frames: 1024,
            audio_converted_frames: 1024,
            audio_gap_frames: 0,
            video_packets: 1,
            audio_packets: 1,
            packets_admitted: 2,
            packets_published: 1,
            encoded_bytes: 64,
            degraded_frames: 0,
            warnings: None,
            failure: Some(Arc::new(StreamError::Transport(
                "Denied exact stream/key?token=abc Password exact".into(),
            ))),
            native_aac: Default::default(),
            video_sealed: true,
            audio_sealed: true,
            interleave: skylive_host::streaming::InterleaveState {
                highest_video_100ns: Some(i128::from(i64::MIN) - 1),
                ..Default::default()
            },
        };
        let dto = status_dto(&s);
        assert_eq!(dto["phase"], "closed");
        assert_eq!(
            dto["interleave"]["highest_video_100ns"],
            (i128::from(i64::MIN) - 1).to_string()
        );
        assert_eq!(dto["interleave"]["waiting"], "none");
        assert_eq!(dto["key"]["session"], 7);
        assert_eq!(dto["epoch"], 8);
        assert_eq!(dto["packets_admitted"], 2);
        assert_eq!(dto["packets_published"], 1);
        assert!(
            dto["failure"]["detail"]
                .as_str()
                .unwrap()
                .contains("stream/key?token=abc Password exact")
        );
        let error = status_error(&s);
        assert!(error.detail.contains("packets_admitted=2"));
        assert!(error.detail.contains("last_publisher_progress_packets=1"));
        assert_eq!(error.phase, Some("streaming_output"));
    }
}

#[cfg(test)]
mod watermark_tests {
    use super::*;
    #[test]
    fn inactive_keyed_track_finish_never_constructs_a_native_owner() {
        let mut runtime = Runtime::default();
        let key = CliKey {
            session: 1,
            output: 1,
            epoch: 1,
        };
        for audio in [false, true] {
            assert!(runtime.finish_track(key, audio).is_err());
            assert!(runtime.owner.is_none());
            assert!(!runtime.active());
            assert_eq!(runtime.resource_generation, 0);
        }
    }
}
