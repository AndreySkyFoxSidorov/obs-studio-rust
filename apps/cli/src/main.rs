mod collection_library;
mod file_media;
mod replay_buffer;
mod ndi_output;
mod recording;
#[path="process_priority.rs"]
mod process_priority_cli;
mod streaming;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use skylive_core::{Command, Id, QuickTransitionId, Session};
use skylive_host::{
    assets,
    audio::{self, AudioMixRequest, PcmInput},
    audio_capture::{
        AudioCaptureError, AudioCaptureEvent, AudioCaptureWorker, AudioRequestKey,
        ProgramTransportStats,
    },
    audio_program::{
        AudioProgram, ProgramAudioError, ProgramAudioLimits, ProgramInputIdentity,
        ProgramInputPacket,
    },
    camera_capture::{CameraError, CameraEvent, CameraOwner},
    capture::{self, CaptureError, CaptureEvent, CaptureWorker},
    obs::{self, ObsIdentities},
    playback::{PlaybackClock, PlaybackSnapshot},
    preview,
    storage::{self, SaveJob},
    transition::{Controller, FrameScene, TransitionSnapshot},
};
use skylive_media::MediaTime;
use std::{
    fs,
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    sync::Arc,
};

const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_REQUESTS: usize = 4096;
#[derive(Debug, Serialize)]
struct CliError {
    code: &'static str,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<&'static str>,
}
impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}
impl From<skylive_core::Error> for CliError {
    fn from(e: skylive_core::Error) -> Self {
        Self {
            code: e.code(),
            detail: e.to_string(),
            phase: None,
        }
    }
}
impl From<storage::StorageError> for CliError {
    fn from(e: storage::StorageError) -> Self {
        Self {
            code: "storage_failure",
            detail: e.detail,
            phase: Some(match e.phase {
                storage::Phase::BeforePublication => "before_publication",
                storage::Phase::AfterPublication => "after_publication",
            }),
        }
    }
}
impl From<preview::PreviewError> for CliError {
    fn from(e: preview::PreviewError) -> Self {
        let code = if matches!(&e, preview::PreviewError::ResourcesUnavailable(problems) if problems.iter().any(|problem|problem.code.starts_with("capture_")))
        {
            "capture_resources_unavailable"
        } else if matches!(&e, preview::PreviewError::ResourcesUnavailable(_)) {
            "image_resources_unavailable"
        } else {
            "preview_failure"
        };
        Self {
            code,
            detail: e.to_string(),
            phase: None,
        }
    }
}
impl From<assets::AssetError> for CliError {
    fn from(error: assets::AssetError) -> Self {
        Self {
            code: error.code(),
            detail: error.to_string(),
            phase: None,
        }
    }
}
impl From<skylive_host::transition::TransitionError> for CliError {
    fn from(error: skylive_host::transition::TransitionError) -> Self {
        Self {
            code: error.code(),
            detail: error.to_string(),
            phase: None,
        }
    }
}
impl From<skylive_host::playback::PlaybackError> for CliError {
    fn from(error: skylive_host::playback::PlaybackError) -> Self {
        Self {
            code: "playback_failure",
            detail: error.to_string(),
            phase: None,
        }
    }
}
impl From<CaptureError> for CliError {
    fn from(error: CaptureError) -> Self {
        Self {
            code: error.code(),
            detail: error.to_string(),
            phase: None,
        }
    }
}
impl From<audio::AudioError> for CliError {
    fn from(error: audio::AudioError) -> Self {
        Self {
            code: error.code(),
            detail: error.to_string(),
            phase: None,
        }
    }
}
impl From<AudioCaptureError> for CliError {
    fn from(error: AudioCaptureError) -> Self {
        Self {
            code: error.code(),
            detail: error.to_string(),
            phase: None,
        }
    }
}
impl From<ProgramAudioError> for CliError {
    fn from(error: ProgramAudioError) -> Self {
        Self {
            code: error.code(),
            detail: error.to_string(),
            phase: None,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CliOriginalAudioFormat {
    sample_rate: u32,
    channels: u16,
    channel_mask: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CliProgramIdentity {
    instance: u64,
    epoch: u64,
    session: u64,
}
impl CliProgramIdentity {
    fn value(self) -> ProgramInputIdentity {
        ProgramInputIdentity {
            instance: self.instance,
            epoch: self.epoch,
            session: self.session,
        }
    }
}
struct CliMedia<'a> {
    process_priority:&'a mut process_priority_cli::Runtime,
    render_gpu: &'a skylive_host::render_gpu::RenderGpu,
    files: &'a mut file_media::Runtime,
    replay:&'a mut replay_buffer::Runtime,
    recording: &'a mut recording::Recorder,
    streaming: &'a mut streaming::Runtime,
    controller: &'a mut Controller,
    playback: &'a mut PlaybackClock,
    capture: &'a mut CaptureWorker,
    camera: &'a mut CameraOwner,
    capture_union: &'a mut capture::CaptureUnion,
    audio_capture: &'a mut AudioCaptureWorker,
    program: &'a mut Option<AudioProgram>,
}
impl From<skylive_host::media_source::MediaSourceError> for CliError {
    fn from(error: skylive_host::media_source::MediaSourceError) -> Self {
        Self {
            code: error.code(),
            detail: error.to_string(),
            phase: None,
        }
    }
}
impl From<skylive_host::game_capture::GameError> for CliError {fn from(error:skylive_host::game_capture::GameError)->Self{Self {code:error.code(),phase:Some(if matches!(&error,skylive_host::game_capture::GameError::AfterInjection(_)|skylive_host::game_capture::GameError::Retiring(_)){"after_injection"}else{"before_injection"}),detail:error.to_string()}}}
impl From<skylive_host::ndi::NdiError> for CliError {fn from(error:skylive_host::ndi::NdiError)->Self {Self {code:error.code(),detail:error.to_string(),phase:None}}}
impl From<skylive_host::browser::BrowserError> for CliError {fn from(error:skylive_host::browser::BrowserError)->Self{Self {code:error.code(),detail:error.to_string(),phase:Some("browser_source")}}}
impl From<skylive_host::replay_buffer::ReplayError> for CliError {fn from(error:skylive_host::replay_buffer::ReplayError)->Self {Self {code:error.code(),phase:Some(if matches!(&error,skylive_host::replay_buffer::ReplayError::AfterPublication {..}){"after_publication"}else{"before_publication"}),detail:error.to_string()}}}
impl From<CameraError> for CliError {
    fn from(error: CameraError) -> Self {
        Self {
            code: error.code(),
            detail: error.to_string(),
            phase: None,
        }
    }
}
impl From<obs::ObsError> for CliError {
    fn from(e: obs::ObsError) -> Self {
        Self {
            code: "obs_interchange_failure",
            detail: e.to_string(),
            phase: None,
        }
    }
}
impl From<skylive_host::localization::LocalizationError> for CliError {
    fn from(error: skylive_host::localization::LocalizationError) -> Self {
        Self {
            code: error.code(),
            detail: error.to_string(),
            phase: Some("before_publication"),
        }
    }
}
impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        Self {
            code: "invalid_request",
            detail: e.to_string(),
            phase: None,
        }
    }
}
impl From<io::Error> for CliError {
    fn from(e: io::Error) -> Self {
        Self {
            code: "io_failure",
            detail: e.to_string(),
            phase: None,
        }
    }
}
impl From<String> for CliError {
    fn from(detail: String) -> Self {
        Self {
            code: "request_rejected",
            detail,
            phase: None,
        }
    }
}
impl From<&str> for CliError {
    fn from(detail: &str) -> Self {
        detail.to_string().into()
    }
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CliPcmInput {
    Converted {
        source: Id,
        sample_rate: u32,
        channels: u16,
        samples: Vec<f32>,
        original: Option<CliOriginalAudioFormat>,
    },
    Samples {
        source: Id,
        sample_rate: u32,
        channels: u16,
        samples: Vec<f32>,
    },
    Silence {
        source: Id,
    },
}
impl CliPcmInput {
    fn as_input(&self) -> Result<PcmInput<'_>, CliError> {
        Ok(match self {
            Self::Converted {
                source,
                sample_rate,
                channels,
                samples,
                original,
            } => PcmInput::Converted {
                source: *source,
                sample_rate: *sample_rate,
                channels: *channels,
                samples,
                original: original
                    .as_ref()
                    .map(|value| {
                        skylive_media::AudioFormat::new(
                            value.sample_rate,
                            value.channels,
                            value.channel_mask,
                        )
                    })
                    .transpose()
                    .map_err(|error| CliError {
                        code: "audio_original_format",
                        detail: error.to_string(),
                        phase: None,
                    })?,
            },
            Self::Samples {
                source,
                sample_rate,
                channels,
                samples,
            } => PcmInput::Samples {
                source: *source,
                sample_rate: *sample_rate,
                channels: *channels,
                samples,
            },
            Self::Silence { source } => PcmInput::Silence { source: *source },
        })
    }
}

#[derive(Deserialize)]
#[serde(tag = "request", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    SourceAudioControl {source:Id,operation:skylive_core::SourceAudioAction,expected_revision:Option<u64>},
    ScanMissingFiles {#[serde(default)]start:usize},
    PrepareResourceRelink {choice:skylive_host::missing_files::Choice},
    CollectionLibrary {action:collection_library::Action,#[serde(default)]remember:bool},
    ProjectorRestoreSnapshot,
    ProjectorSnapshot {target:skylive_host::projector::Target,#[serde(default)] options:Option<skylive_host::projector::Options>,width:u32,height:u32},
    RemuxRecordings {jobs:Vec<skylive_host::remux::Job>},
    RemuxStatus {key:skylive_host::remux::Key},
    CancelRemux {key:skylive_host::remux::Key},
    ProcessPriorityStatus,
    StreamServicePresets {query:Option<String>},
    SaveScreenshot {path:PathBuf,target:skylive_host::screenshot::Target},
    StartNdiOutput {route:skylive_core::NdiOutputRoute,time_ns:Option<u64>},
    NdiOutputStatus,
    NdiOutputFrame {key:skylive_host::ndi::output::Key,time_ns:Option<u64>},
    StopNdiOutput {key:skylive_host::ndi::output::Key},
    /// Explicit worker-scoped driver operation; capabilities alone stays pure.
    ProbeNvenc,
    StartBrowserSources {time_ns:Option<u64>},
    StopBrowserSources,
    BrowserStatus,
    RefreshBrowser {source:Id},
    ReloadBrowser {source:Id},
    BrowserInput {source:Id,lease:u64,x:i32,y:i32,button:Option<u8>,up:Option<bool>},
    StartReplayBuffer {time_ns:Option<u64>},
    ReplayBufferStatus,
    DiscoverGames,
    StartGameCapture {source:Id},
    StopGameCapture {source:Id},
    GameCaptureStatus,
    /// Inspect the same real composition factory used by the native desktop
    /// owner. This query never claims to start or install a virtual camera.
    InspectVirtualCameraRoute {route:skylive_core::VirtualCameraRoute,width:u32,height:u32,fps:u32},
    ReplayBufferFrame {key:replay_buffer::Key,time_ns:Option<u64>},
    SaveReplayBuffer {key:replay_buffer::Key,path:PathBuf},
    CancelReplaySave {key:replay_buffer::Key},
    StopReplayBuffer {key:replay_buffer::Key},
    ProcessStatistics,
    DiscoverNdi {include_local:bool},
    PrepareNdi {connection:skylive_core::NdiConnection},
    ImportNdi {scene:Id,name:String,connection:skylive_core::NdiConnection,expected_revision:Option<u64>},
    NdiStatus,
    PrepareMedia {
        path: PathBuf,
    },
    ImportMedia {
        scene: Id,
        name: String,
        path: PathBuf,
        expected_revision: Option<u64>,
    },
    StartMediaPlayback {
        time_ns: Option<u64>,
    },
    SlideshowControl {source:Id,operation:skylive_host::image_slideshow::transport::Control,time_ns:Option<u64>,expected_revision:Option<u64>},
    SlideshowStatus {source:Id,time_ns:Option<u64>},
    PrepareSlideshowEntries {entries:Vec<skylive_core::SlideShowEntry>},
    RefreshSlideshowEntries {source:Id,expected_revision:Option<u64>},
    ReseedSlideshow {source:Id,seed:u64,time_ns:Option<u64>,expected_revision:Option<u64>},
    ReloadMedia {source:Id,time_ns:Option<u64>},
    RestartMedia {
        source: Id,
        time_ns: Option<u64>,
    },
    MediaPlaybackStatus,
    StopMediaPlayback,
    StartProgramAudio {
        time_ns: Option<u64>,
    },
    ReadProgramAudio {
        max_blocks: Option<usize>,
    },
    StartStreaming {
        time_ns: Option<u64>,
    },
    StreamingStatus,
    FinishStreamVideo {
        key: streaming::CliKey,
    },
    FinishStreamAudio {
        key: streaming::CliKey,
    },
    StreamFrame {
        key: streaming::CliKey,
        time_ns: Option<u64>,
    },
    StopStreaming {
        key: streaming::CliKey,
    },
    StartRecording {
        path: PathBuf,
        time_ns: Option<u64>,
        audio: Option<bool>,
    },
    RecordFrame {
        time_ns: Option<u64>,
    },
    RecordingStatus,
    PauseRecording {recording:u64,time_ns:Option<u64>},
    ResumeRecording {recording:u64,time_ns:Option<u64>},
    StopRecording,
    Localization {
        path: Option<PathBuf>,
        language: Option<String>,
        key: Option<String>,
    },
    SetLanguage {
        language: String,
        path: Option<PathBuf>,
    },
    Plugins,
    Snapshot,
    Capabilities,
    AudioPlan,
    AudioProgramReset,
    AudioMonitoringStatus,
    AudioProgramRoute,
    AudioProgramStatus,
    AudioProgramPacket {
        identity: CliProgramIdentity,
        binding: skylive_core::AudioCaptureBinding,
        stream_generation: u64,
        sequence: u64,
        first_media_ns: String,
        received_media_ns: String,
        sample_rate: u32,
        channels: u16,
        channel_mask: u32,
        samples: Vec<f32>,
        #[serde(default)]
        silent: bool,
        #[serde(default)]
        discontinuity: bool,
        #[serde(default)]
        queue_dropped: u64,
    },
    AudioProgramFinish {
        identity: CliProgramIdentity,
        binding: skylive_core::AudioCaptureBinding,
        stream_generation: u64,
        sequence: u64,
    },
    RenderAudioProgram {
        start_frame: u64,
        frames: usize,
    },
    ListAudioApplications,
    ListAudioDevices {
        flow: skylive_core::AudioFlow,
    },
    CaptureAudio {
        time_ns: Option<u64>,
    },
    StartNativeAudioProgram {
        time_ns: Option<u64>,
    },
    ReadNativeAudioProgram {
        max_blocks: Option<usize>,
    },
    StopNativeAudioProgram,
    MixProgramAudio {
        sample_rate: u32,
        channels: u16,
        frames: usize,
        #[serde(default)]
        advance_ns: u64,
        inputs: Vec<CliPcmInput>,
    },
    ListDisplays,
    PrepareFont {path:PathBuf},
    DiscoverSystemFonts,
    PrepareLut {path:PathBuf},
    ListWindows,
    ListCameraDevices,
    PrepareProfileFile { path: PathBuf },
    ExportProfileFile { path: PathBuf },
    ListCameraControls {source:Id},
    SetCameraControl {lease:skylive_host::camera_capture::ControlLease,change:skylive_host::camera_capture::ControlChange},
    ListCameraModes {
        target: skylive_core::VideoCaptureTarget,
    },
    StartCameraSource {source:Id,time_ns:Option<u64>},
    StopCameraSource {source:Id,time_ns:Option<u64>},
    StopCameraCapture,
    CameraActivationStatus,
    CaptureFrame {
        time_ns: Option<u64>,
    },
    GetDisplayTransform {
        scene: Id,
        item: Id,
    },
    SetDisplayTransform {
        scene: Id,
        item: Id,
        transform: skylive_core::Transform,
        expected_revision: Option<u64>,
        expected_frame_dimensions: Option<[u32; 2]>,
    },
    MovePreviewSource {
        scene:Id,item:Id,delta:[f64;2],display_scale:[f64;2],
        #[serde(default)]control:bool,
        expected_revision:Option<u64>,
    },
    ResizePreviewSource {
        scene:Id,item:Id,handle:skylive_host::preview_interaction::Handle,
        delta:[f64;2],display_scale:[f64;2],
        #[serde(default)]control:bool,
        #[serde(default)]free_aspect:bool,
        #[serde(default)]crop:bool,
        expected_revision:Option<u64>,
    },
    New,
    ResizeOutputToSource {scene:Id,item:Id,expected_revision:Option<u64>},
    Command {
        command: Command,
        expected_revision: Option<u64>,
    },
    SelectPreview {
        scene: Id,
        time_ns: Option<u64>,
    },
    Cut {
        time_ns: Option<u64>,
    },
    StudioMode {
        enabled: bool,
        time_ns: Option<u64>,
    },
    Transition {
        time_ns: Option<u64>,
    },
    QuickTransition {
        id: QuickTransitionId,
        time_ns: Option<u64>,
    },
    TbarPosition {
        value: u16,
        time_ns: Option<u64>,
    },
    TbarRelease {
        time_ns: Option<u64>,
    },
    Advance {
        time_ns: u64,
    },
    Save {
        path: PathBuf,
    },
    Checkpoint {
        path: PathBuf,
    },
    Load {
        path: PathBuf,
    },
    Recover {
        path: PathBuf,
    },
    ImportObs {
        path: PathBuf,
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
    },
    PrepareSlideShowImages {paths:Vec<PathBuf>},
    ImportSlideShow {scene:Id,name:String,#[serde(default)]paths:Vec<PathBuf>,#[serde(default)]entries:Vec<skylive_core::SlideShowEntry>,duration_ms:u32,transition:skylive_core::SlideTransition,transition_ms:u32,looping:bool,shuffle:bool,hide_when_done:bool,#[serde(default)]canvas:skylive_core::SlideShowCanvas,#[serde(default)]behavior:skylive_core::SlideShowBehavior,#[serde(default)]mode:skylive_core::SlideShowMode,#[serde(default)]randomize:bool,#[serde(default)]random_compatibility:skylive_core::SlideShowRandomCompatibility,expected_revision:Option<u64>},
    RebindSlideShow {source:Id,#[serde(default)]paths:Vec<PathBuf>,#[serde(default)]entries:Vec<skylive_core::SlideShowEntry>,duration_ms:u32,transition:skylive_core::SlideTransition,transition_ms:u32,looping:bool,shuffle:bool,hide_when_done:bool,#[serde(default)]canvas:skylive_core::SlideShowCanvas,#[serde(default)]behavior:skylive_core::SlideShowBehavior,#[serde(default)]mode:skylive_core::SlideShowMode,#[serde(default)]randomize:bool,#[serde(default)]random_compatibility:skylive_core::SlideShowRandomCompatibility,expected_revision:Option<u64>},
    ImportImage {
        scene: Id,
        name: String,
        path: PathBuf,
        #[serde(default)]
        settings: skylive_core::ImageSourceSettings,
        expected_revision: Option<u64>,
    },
    ReloadImage {
        source: Id,
        path: PathBuf,
        #[serde(default)]
        settings: Option<skylive_core::ImageSourceSettings>,
        expected_revision: Option<u64>,
    },
    ExportObs {
        path: PathBuf,
    },
    Render {
        path: PathBuf,
        scene: Option<Id>,
        width: u32,
        height: u32,
        #[serde(default)]
        time_ns: u64,
    },
    RenderProgram {
        path: PathBuf,
        width: u32,
        height: u32,
        time_ns: Option<u64>,
    },
    Close,
}
fn sync_runtime(
    session: &mut Session,
    controller: &mut Controller,
    playback: &mut PlaybackClock,
    ndi_preview:bool,
) -> Result<(TransitionSnapshot, PlaybackSnapshot), CliError> {
    let now = controller.now();
    let state = controller.sync(session, now)?;
    let mut outputs = state.output.scenes();
    if state.studio {
        outputs.push(FrameScene::Live(session.preview()));
    }
    if ndi_preview&&!state.studio{let extra=FrameScene::Live(state.output.primary_scene());if !outputs.iter().any(|v|v.key()==extra.key()){outputs.push(extra);}}
    let sample = playback.update_outputs(session, &outputs, state.now)?;
    Ok((state, sample))
}
fn sync_capture_runtime(
    session: &mut Session,
    controller: &mut Controller,
    playback: &mut PlaybackClock,
    capture: &mut CaptureWorker,
    camera: &mut CameraOwner,
    ndi_preview:bool,
) -> Result<(TransitionSnapshot, PlaybackSnapshot), CliError> {
    let (state, _) = sync_runtime(session, controller, playback,ndi_preview)?;
    let mut outputs = state.output.scenes();
    if state.studio {
        outputs.push(FrameScene::Live(session.preview()));
    }
    if ndi_preview&&!state.studio{let extra=FrameScene::Live(state.output.primary_scene());if !outputs.iter().any(|v|v.key()==extra.key()){outputs.push(extra);}}
    capture.synchronize(session, &outputs)?;
    camera.synchronize(session, &outputs)?;
    if camera
        .last_requested_at()
        .is_some_and(|previous| state.now < previous)
    {
        camera.reset_timing(session)?;
    }
    if let Some(CameraEvent::Activated {key,result:Err(error)})=camera.poll(session)? {
        if !camera.is_av_activation_key(key){return Err(error.into());}
    }
    if camera.is_active()&&!camera.is_busy()&&!camera.av_admission_reserved() {
        camera.request_frame(session, state.now)?;
    }
    if capture
        .last_requested_at()
        .is_some_and(|previous| state.now < previous)
    {
        capture.reset_timing()?;
    }
    let sample=playback.update_outputs_with_capture(session,&outputs,state.now,&capture.snapshot())?;
    Ok((state, sample))
}
fn capture_summary(worker: &CaptureWorker) -> Value {
    let sample = worker.snapshot();
    let mut frames:Vec<_>=sample.frames().map(|(source,frame)|json!({"source":source,"binding":frame.binding,"display":frame.display,"window":frame.window,"sequence":frame.sequence,"requested_at_ns":frame.requested_at.0,"width":frame.image.width(),"height":frame.image.height()})).collect();
    frames.sort_by_key(|frame| frame["source"].as_u64());
    json!({"epoch":sample.epoch(),"generation":sample.generation(),"active_sources":worker.active_source_count(),"frames":frames,"problems":sample.problems()})
}
fn camera_summary(owner: &CameraOwner) -> Value {
    let sample = owner.snapshot();
    let mut frames:Vec<_>=sample.frames().map(|(source,frame)|json!({"source":source,"binding":frame.binding,"sequence":frame.sequence,"stream_generation":frame.stream_generation,"received_media_ns":frame.requested_at.0,"presentation_ns":frame.presentation_ns.to_string(),"width":frame.image.width(),"height":frame.image.height(),"dropped_frames":frame.dropped_frames})).collect();
    frames.sort_by_key(|f| f["source"].as_u64());
    json!({"instance":sample.instance(),"epoch":sample.epoch(),"generation":sample.generation(),"active":owner.is_active(),"active_sources":owner.active_source_count(),"frames":frames,"problems":sample.problems()})
}
fn settle_camera_controls(session: &Session, camera: &mut CameraOwner) -> Result<(), CliError> {
    while camera.is_busy() {
        if let Some(CameraEvent::Activated { result, .. }) = camera.poll(session)? {
            result?;
        }
        if camera.is_busy() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    Ok(())
}
fn merged_capture(
    session: &Session,
    capture: &CaptureWorker,
    camera: &CameraOwner,
    union: &mut capture::CaptureUnion,
    files: &mut file_media::Runtime,
) -> Result<Arc<capture::CaptureSnapshot>, CliError> {
    union.set_camera_activation(files.camera_activation.snapshot());
    let base = union.merge(session, capture.snapshot(), camera)?;
    let base=files.union.merge(session,base,&files.owner)?;
    let base=files.ndi_union.merge(session,base,&files.ndi)?;
    let base=files.game_union.merge(session,base,&files.game)?;
    Ok(files.browser_union.merge(session,base,&files.browser)?)
}
fn sync_audio_runtime(
    session: &Session,
    state: &TransitionSnapshot,
    worker: &mut AudioCaptureWorker,
) -> Result<(), CliError> {
    let mut outputs = state.output.scenes();
    if state.studio {
        outputs.push(FrameScene::Live(session.preview()));
    }
    worker.synchronize_with_program(session, &outputs, &state.output.scenes())?;
    if worker
        .last_requested_at()
        .is_some_and(|previous| state.now < previous)
    {
        worker.reset_timing()?;
    }
    Ok(())
}
fn audio_capture_summary(worker: &AudioCaptureWorker) -> Value {
    let snapshot = worker.snapshot();
    let mut packets:Vec<_>=snapshot.packets().map(|(source,packet)|json!({"source":source,"sequence":packet.sequence,"stream_generation":packet.stream_generation,"requested_at_ns":packet.requested_at_ns,"first_sample_media_ns":packet.first_sample_media_ns.to_string(),"timing_origin":packet.timing_origin,"coalesced_packets":packet.coalesced_packets,"resolved_device":packet.resolved_device,"format":packet.block.format,"frames":packet.block.frames,"device_timestamp_ns":packet.block.device_timestamp_ns,"host_received_ns":packet.block.host_received_ns,"flags":packet.block.flags,"meters":packet.meters})).collect();
    packets.sort_by_key(|packet| packet["source"].as_u64());
    json!({"session":snapshot.session(),"epoch":snapshot.epoch(),"generation":snapshot.generation(),"active_sources":worker.active_source_count(),"packets":packets,"problems":snapshot.problems(),"program_audio_output":false})
}
fn wait_program_ack(
    session: &Session,
    worker: &mut AudioCaptureWorker,
    expected: Option<AudioRequestKey>,
) -> Result<ProgramTransportStats, CliError> {
    while worker.is_busy() {
        match worker.poll(session)? {
            Some(AudioCaptureEvent::Program { key, result })
                if expected.is_none_or(|expected| expected == key) =>
            {
                return Ok(result?);
            }
            _ => {}
        }
        if worker.is_busy() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    Err(AudioCaptureError::Invalid("Program control acknowledgement is missing or stale").into())
}
fn stop_live_audio_if_running(
    session: &Session,
    worker: &mut AudioCaptureWorker,
) -> Result<bool, CliError> {
    if worker.program_now()?.is_none() {
        return Ok(false);
    }
    let key = worker.request_stop_program(session)?;
    wait_program_ack(session, worker, Some(key))?;
    Ok(true)
}
fn signed_ns(value: &str) -> Result<i128, CliError> {
    if value.is_empty() || value.len() > 40 {
        return Err(ProgramAudioError::Invalid(
            "signed media nanoseconds require a bounded decimal string",
        )
        .into());
    }
    value
        .parse::<i128>()
        .map_err(|_| ProgramAudioError::Invalid("invalid signed media nanoseconds").into())
}
fn program_owner<'a>(
    session: &Session,
    program: &'a mut Option<AudioProgram>,
) -> Result<&'a mut AudioProgram, CliError> {
    let owner = program.as_mut().ok_or(ProgramAudioError::Invalid(
        "reset the Program audio evaluator first",
    ))?;
    let state = owner.status();
    if state.session != Some(session.identity()) {
        return Err(ProgramAudioError::Invalid("Program belongs to another session").into());
    }
    if state.settings != session.snapshot().audio() {
        return Err(ProgramAudioError::FormatChanged.into());
    }
    Ok(owner)
}
fn snapshot(
    session: &mut Session,
    controller: &mut Controller,
    playback: &mut PlaybackClock,
    capture: &mut CaptureWorker,
    audio_capture: &mut AudioCaptureWorker,
    camera: &mut CameraOwner,
    ndi_preview:bool,
) -> Result<Value, CliError> {
    let (state, _) = sync_capture_runtime(session, controller, playback, capture, camera,ndi_preview)?;
    sync_audio_runtime(session, &state, audio_capture)?;
    let current = session.snapshot();
    let mut collection = serde_json::to_value(current.collection())
        .map_err(|error| CliError::from(error.to_string()))?;
    let fields = collection
        .as_object_mut()
        .ok_or("invalid collection read model")?;
    fields.insert("revision".into(), json!(current.revision()));
    fields.insert("video".into(), json!(current.video()));
    fields.insert("output_video_settings".into(),json!(current.output_video_settings()));
    fields.insert("output_video".into(),json!(current.output_video().map_err(|error|CliError::from(error.to_string()))?));
    fields.insert("audio".into(), json!(current.audio()));
    Ok(
        json!({"collection":collection,"profiles":current.profiles(),"preferences":current.preferences(),"preview":session.preview(),"program":session.program(),"dirty":session.is_dirty(),"closed":session.is_closed(),
        "transition":{"studio":state.studio,"active":state.active,"epoch":state.epoch,"time_ns":state.now.0,"composition":state.output.sample_key(),"manual":state.manual,"needs_tick":state.needs_tick},"capture":capture_summary(capture),"camera_capture":camera_summary(camera),"audio_capture":audio_capture_summary(audio_capture)}),
    )
}
fn execute(
    session: &mut Session,
    obs_ids: &mut Option<Arc<ObsIdentities>>,
    media: &mut CliMedia<'_>,
    request: Request,
) -> Result<Value, CliError> {
    // Closing ends the protocol owner. A new/load request cannot revive it.
    if session.is_closed() {
        return Err("session closed".into());
    }
    if matches!(&request,Request::CollectionLibrary{action,..}if action.changes_document()){
        if media.recording.active()||media.streaming.active()||media.replay.owner.active()||media.files.outputs.owner.any_active(){return Err("stop active outputs before changing managed collections".into());}
        media.process_priority.invalidate();
    }
    if matches!(&request,Request::ResizeOutputToSource{..}){guard_source_output_resize(media.recording.active()||media.streaming.active()||media.replay.owner.active()||media.files.outputs.owner.any_active())?;}
    let _=media.process_priority.poll(session,media.camera,media.controller)?;
    if let Request::Command{command:command@Command::ConfigureAdvancedSettings{..},expected_revision}=&request{media.streaming.guard(&request)?;return media.process_priority.apply_command(session,media.camera,media.controller,*expected_revision,command.clone());}
    if matches!(&request,Request::StartStreaming{..})&&media.process_priority.advanced_pending(){return Err("Wait for the Advanced settings operation to finish.".into());}
    if let Request::Command{command:Command::SetProcessPriority{priority},expected_revision}=&request{return media.process_priority.apply(session,media.camera,media.controller,*expected_revision,*priority);}
    if matches!(&request,Request::SourceAudioControl{..}|Request::Command{..}|Request::New|Request::Load{..}|Request::Recover{..}|Request::ImportObs{..}){media.process_priority.invalidate();}
    media.streaming.guard(&request)?;
    media.replay.guard(&request)?;
    media.files.outputs.guard(&request)?;
    media.files.outputs.poll(session,media.controller,media.audio_capture)?;
    media.files.outputs.refresh_clock(session,media.controller,media.audio_capture)?;
    media.replay.poll(session,media.controller,media.audio_capture)?;
    if media.replay.owner.active(){
        let document=matches!(&request,Request::New|Request::Load {..}|Request::Recover {..}|Request::ImportObs {..});
        let format=matches!(&request,Request::Command {command,..}if skylive_host::recording::blocks_format_change(command));
        if document||format{return Err("stop Replay Buffer before replacing the collection or changing the audio/video format/profile".into());}
    }
    if media.recording.active() {
        let changes_document = matches!(
            &request,
            Request::New
                | Request::Load { .. }
                | Request::Recover { .. }
                | Request::ImportObs { .. }
        );
        let changes_format = matches!(&request,Request::Command{command,..}if skylive_host::recording::blocks_format_change(command));
        if changes_document || changes_format {
            return Err("stop recording before replacing the collection or changing audio/video format/profile".into());
        }
    }
    if media.files.running {
        if let Some(now) = media.files.owner.media_now()? {
            media.controller.sync(session, now)?;
        }
    }
    media
        .streaming
        .refresh_clock(session, media.controller, media.audio_capture)?;
    media.replay.refresh_clock(session,media.controller,media.audio_capture)?;
    let _=service_freeze(session,media)?;
    media.files.camera_activation.synchronize(session,media.camera,media.audio_capture)
        .map_err(|error|CliError {code:error.code(),detail:error.to_string(),phase:None})?;
    media.capture_union.set_camera_activation(media.files.camera_activation.snapshot());
    let (state, _) = sync_capture_runtime(
        session,
        media.controller,
        media.playback,
        media.capture,
        media.camera,
        media.files.outputs.owner.active(skylive_core::NdiOutputRoute::Preview),
    )?;
    sync_audio_runtime(session, &state, media.audio_capture)?;
    media
        .files
        .synchronize(session, &state, media.audio_capture)?;
    let _=media.audio_capture.poll(session)?;
    media.files.camera_activation.drive(session,media.camera,media.audio_capture,Some(&state.output),state.now)
        .map_err(|error|CliError {code:error.code(),detail:error.to_string(),phase:None})?;

    let before = (session.identity(), session.snapshot().revision());
    let result = execute_request(session, obs_ids, media, request);
    media.files.outputs.owner.synchronize(session);
    let mut published = (session.identity(), session.snapshot().revision()) != before;
    if session.identity() != before.0 {
        media.capture_union.clear();
        media.files.ndi_running=false;media.files.ndi.stop();media.files.ndi_union.clear();
        media.files.union.clear();
        media.files.running = false;
    }
    match result {
        Ok(value) => {
            if !session.is_closed() {
                published |= service_freeze(session,media).map_err(|mut error|{if published{error.phase=Some("after_publication");}error})?;
                media
                    .streaming
                    .refresh_clock(session, media.controller, media.audio_capture)
                    .map_err(|mut error| {
                        if published {
                            error.phase = Some("after_publication");
                        }
                        error
                    })?;
                sync_capture_runtime(
                    session,
                    media.controller,
                    media.playback,
                    media.capture,
                    media.camera,
                    media.files.outputs.owner.active(skylive_core::NdiOutputRoute::Preview),
                )
                .and_then(|(state, _)| {
                    sync_audio_runtime(session, &state, media.audio_capture)?;
                    media
                        .files
                        .synchronize(session, &state, media.audio_capture)?;
                    media
                        .streaming
                        .synchronize(session, &state, media.audio_capture)
                })
                .map_err(|mut error| {
                    if published {
                        error.phase = Some("after_publication");
                    }
                    error
                })?;
            }
            Ok(value)
        }
        Err(mut error) => {
            if published && error.phase.is_none() {
                error.phase = Some("after_publication");
            }
            Err(error)
        }
    }
}
fn guard_source_output_resize(busy:bool)->Result<(),CliError>{
    if busy{return Err("Stop recording, streaming, Replay Buffer and NDI outputs before resizing output.".into());}Ok(())
}

// CLI services deadlines on its existing request/update clock. Blocking stdin
// itself does not create a new timer or background device.
fn service_freeze(session:&mut Session,media:&mut CliMedia<'_>)->Result<bool,CliError>{
    use skylive_host::freeze_timing_book::{Due,Group,Instance};
    let now=media.controller.now();let state=media.controller.sync(session,now)?;
    let mut views=state.output.scenes();if state.studio{views.push(FrameScene::Live(session.preview()));}
    if media.files.outputs.owner.active(skylive_core::NdiOutputRoute::Preview)&&!state.studio{let extra=FrameScene::Live(state.output.primary_scene());if !views.iter().any(|view|view.key()==extra.key()){views.push(extra);}}
    skylive_host::freeze_timing_collect::observe(media.render_gpu,session,&views,now.0).map_err(CliError::from)?;
    media.render_gpu.freeze_observe(Group::Draft,&[],now.0).map_err(CliError::from)?;
    let intents=media.render_gpu.freeze_due(now.0).map_err(CliError::from)?;
    for (index,intent) in intents.values.iter().copied().enumerate(){match (intent.target.instance,intent.due){
        (Instance::Canonical{filter},Due::Disable{..})=>{
            let revision=session.snapshot().revision();
            let current=intent.target.session==session.identity()&&revision.checked_add(1)==Some(intent.definition_generation)&&session.snapshot().source(intent.target.source).is_some_and(|source|source.video_filters().iter().any(|value|value.id()==Some(filter)&&value.enabled&&matches!(value.effect,skylive_core::VideoEffect::Freeze{..})));
            if !current{media.render_gpu.freeze_reject_disable(intent);continue;}
            let Some(mut filters)=session.snapshot().source(intent.target.source).map(|source|source.video_filters().to_vec()) else {media.render_gpu.freeze_reject_disable(intent);continue;};
            let Some(target)=filters.iter_mut().find(|value|value.id()==Some(filter)&&value.enabled&&matches!(value.effect,skylive_core::VideoEffect::Freeze{..})) else {media.render_gpu.freeze_reject_disable(intent);continue;};
            target.enabled=false;
            if let Err(error)=media.camera.before_session_change(){for pending in &intents.values[index..]{media.render_gpu.freeze_reject_disable(*pending);}return Err(error.into());}
            if let Err(error)=media.controller.before_change(session){for pending in &intents.values[index..]{media.render_gpu.freeze_reject_disable(*pending);}return Err(error.into());}
            let accepted=session.apply_at_with_gate(Some(revision),Command::SetSourceVideoFilters{source:intent.target.source,filters},||media.render_gpu.freeze_accept_disable(intent));
            match accepted{Ok(Some(_))=>{for remaining in &intents.values[index+1..]{media.render_gpu.freeze_reject_disable(*remaining);}return Ok(true);},Ok(None)=>media.render_gpu.freeze_reject_disable(intent),Err(error)=>{media.render_gpu.freeze_reject_disable(intent);for remaining in &intents.values[index+1..]{media.render_gpu.freeze_reject_disable(*remaining);}return Err(error.into());}}
        },
        (Instance::Private{..}|Instance::DraftPrivate{..},Due::Disable{..})=>{if !media.render_gpu.freeze_expire_private(intent,now.0){media.render_gpu.freeze_reject_disable(intent);}},
        (_,Due::Disable{..})=>media.render_gpu.freeze_reject_disable(intent),
        (_,Due::Refresh{..}|Due::RetryCapture{..}|Due::RetryRender{..}|Due::None)=>{},
    }}
    Ok(false)
}

fn execute_request(
    session: &mut Session,
    obs_ids: &mut Option<Arc<ObsIdentities>>,
    media: &mut CliMedia<'_>,
    request: Request,
) -> Result<Value, CliError> {
    let process_priority=&mut *media.process_priority;
    let controller = &mut *media.controller;
    let playback = &mut *media.playback;
    let capture = &mut *media.capture;
    let camera = &mut *media.camera;
    let capture_union = &mut *media.capture_union;
    let audio_capture = &mut *media.audio_capture;
    let program = &mut *media.program;
    let recording = &mut *media.recording;
    let streaming = &mut *media.streaming;
    let files = &mut *media.files;
    let replay=&mut *media.replay;
    let ndi_preview=files.outputs.owner.active(skylive_core::NdiOutputRoute::Preview);
    let start_camera_source=matches!(&request,Request::StartCameraSource {..});
    let pause_recording=matches!(&request,Request::PauseRecording {..});
    match request {
        Request::RemuxRecordings {jobs}=>{let key=files.remux.start(jobs,Arc::new(||{}))?;Ok(json!({"remux_key":key,"status":files.remux.status()}))},
        Request::RemuxStatus {key}=>{let status=files.remux.status();if status.key!=Some(key){return Err("Remux batch identity is obsolete".into());}Ok(json!(status))},
        Request::CancelRemux {key}=>{files.remux.cancel(key)?;Ok(json!({"cancel_requested":true,"status":files.remux.status(),"retirement_claimed":files.remux.is_retired()}))},
        Request::ProcessPriorityStatus=>Ok(process_priority.status()),
        Request::StreamServicePresets {query}=>{
            use skylive_core::stream_service_presets as presets;
            let folded=presets::fold_query(query.as_deref().unwrap_or(""))?;
            let services:Vec<_>=presets::services().iter().filter(|s|s.matches_folded(&folded)).collect();
            Ok(json!({"read_only":true,"historical":true,"obs_version":presets::OBS_VERSION,"source_commit":presets::SOURCE_COMMIT,"data_sha256":presets::DATA_SHA256,"connectivity_checked":false,"services":services}))
        },
        Request::SaveScreenshot {path,target}=>{
            // Sampling an existing immutable capture union must not authorize
            // acquisition. Missing capture/media frames fail in the shared host
            // operation; screenshot does not start devices/helpers/audio roles.
            let (state,sample)=sync_runtime(session,controller,playback,ndi_preview)?;
            files.poll(session)?;
            let frame=skylive_host::recording::RecordingFrame {session:session.identity(),time:state.now,model:session.snapshot(),output:state.output,playback:sample,capture:merged_capture(session,capture,camera,capture_union,files)?,resource_generation:recording.generation()};
            let request=skylive_host::screenshot::Request {path,target,frame,preview:session.preview(),studio:state.studio,gpu:media.render_gpu.clone()};
            let worker=std::thread::Builder::new().name("skylive-cli-screenshot".into()).spawn(move||skylive_host::screenshot::save(request)).map_err(|e|e.to_string())?;
            let receipt=worker.join().map_err(|_|"Screenshot worker failed.")?.map_err(CliError::from)?;
            Ok(json!({"screenshot":receipt,"format":"png_rgba8","overwritten":false}))
        },
        Request::StartBrowserSources {time_ns}=>{let state=controller.sync(session,MediaTime(time_ns.unwrap_or(controller.now().0)))?;let mut views=state.output.scenes();if state.studio{views.push(FrameScene::Live(session.preview()));}files.browser_running=true;files.browser.synchronize(session,&views,&state.output.scenes(),&views,state.now,audio_capture.program_key())?;Ok(json!({"requested":true,"status":files.browser_summary()}))},
        Request::StopBrowserSources=>{files.browser_running=false;files.browser.stop(session)?;files.browser_union.clear();Ok(json!({"requested":true,"native_retirement_async":true}))},
        Request::BrowserStatus=>{files.browser.poll(session)?;Ok(files.browser_summary())},
        Request::RefreshBrowser {source}|Request::ReloadBrowser {source}=>{let target=skylive_host::browser::BrowserReloadTarget::new(session,source)?;let receipt=files.browser.reload(session,&target)?;let progress=files.browser.reload_progress(session,&target,receipt);Ok(json!({"refresh_requested":true,"receipt":receipt,"progress":progress,"page_ready":false,"native_retirement_acknowledged":false}))},
        Request::BrowserInput {source,lease,x,y,button,up}=>{let event=if let Some(button)=button {if button>2{return Err("browser mouse button must be0..2".into());}skylive_host::browser::BrowserInput::Click{x,y,modifiers:0,button:i32::from(button),up:up.unwrap_or(false),count:1}}else{skylive_host::browser::BrowserInput::Move{x,y,modifiers:0,leave:false}};files.browser.input(session,source,lease,event)?;Ok(json!({"input_admitted":true,"source":source,"lease":lease}))},
        Request::StartNdiOutput {route,time_ns}=>{let now=time_ns.map(MediaTime).unwrap_or(audio_capture.program_now()?.unwrap_or_else(||controller.now()));let state=controller.sync(session,now)?;files.outputs.start(session,&state,audio_capture,route)},
        Request::NdiOutputStatus=>Ok(files.outputs.status()),
        Request::StopNdiOutput {key}=>files.outputs.stop(key),
        Request::NdiOutputFrame {key,time_ns}=>{
            let now=time_ns.map(MediaTime).unwrap_or(audio_capture.program_now()?.unwrap_or_else(||controller.now()));controller.sync(session,now)?;
            let (state,sample)=sync_capture_runtime(session,controller,playback,capture,camera,ndi_preview)?;
            files.start(session,&state,audio_capture)?;files.synchronize(session,&state,audio_capture)?;
            if !capture.is_busy(){capture.request_frame(session,state.now)?;}capture.poll(session,state.now)?;camera.poll(session)?;
            let config=files.outputs.owner.config(key.route).ok_or("NDI output is not started")?.clone();let output=config.composition(session,&state.output,state.studio)?;
            let frame=skylive_host::recording::RecordingFrame {session:session.identity(),time:state.now,model:session.snapshot(),output,playback:sample,capture:merged_capture(session,capture,camera,capture_union,files)?,resource_generation:recording.generation()};files.outputs.frame(key,frame)
        },
        Request::StartReplayBuffer {time_ns}=>{let now=time_ns.map(MediaTime).unwrap_or(audio_capture.program_now()?.unwrap_or_else(||controller.now()));let state=controller.sync(session,now)?;replay.start(session,&state,audio_capture)},
        Request::ReplayBufferStatus=>Ok(replay.status()),
        Request::ProbeNvenc=>{
            let worker=std::thread::Builder::new().name("skylive-cli-nvenc-probe".into()).spawn(skylive_host::native_h264::probe_nvenc).map_err(|e|e.to_string())?;
            let catalog=worker.join().map_err(|_|"NVENC probe worker failed".to_owned())?.map_err(|e|e.to_string())?;
            Ok(json!({"nvenc":catalog,"encoding_started":false}))
        },
        Request::DiscoverGames=>Ok(json!({"targets":files.discover_games(session)?,"injected":false})),
        Request::StartGameCapture {source}=>{let result=files.game_control(session,source,true)?;let now=controller.now();let state=controller.sync(session,now)?;files.synchronize(session,&state,audio_capture)?;Ok(result)},
        Request::StopGameCapture {source}=>files.game_control(session,source,false),
        Request::GameCaptureStatus=>{files.game.poll(session)?;Ok(files.game_summary())},
        Request::InspectVirtualCameraRoute {route,width,height,fps}=>{
            let config=skylive_host::virtual_camera::Config::for_route(session,route,width,height,fps)
                .map_err(|error|CliError {code:error.code(),detail:error.to_string(),phase:None})?;
            let now=controller.now();let state=controller.sync(session,now)?;
            let capture=merged_capture(session,capture,camera,capture_union,files)?;
            let output=skylive_host::virtual_camera::RouteFactory::default().composition(session,route,&state.output,&capture)
                .map_err(|error|CliError {code:error.code(),detail:error.to_string(),phase:None})?;
            let model=session.snapshot();let mut sources=Vec::new();
            for view in output.scenes(){for source in view.visible_sources(&model)? {
                if model.source_video_dimensions(source.kind())?.is_some()&&!sources.contains(&source.id()){sources.push(source.id());}
            }}
            Ok(json!({"session":session.identity(),"revision":model.revision(),"route":route,"composition":output.sample_key(),
                "active_video_sources":sources,"camera_profile":{"width":config.width(),"height":config.height(),"fps":config.fps()},
                "pipeline":"canvas_global_output_camera_fit","video_only":true,"producer_started":false,
                "native_control":"desktop_owner","native_verified":false}))
        },
        Request::SaveReplayBuffer {key,path}=>replay.save(key,path),
        Request::CancelReplaySave {key}=>replay.cancel(key),
        Request::StopReplayBuffer {key}=>replay.stop(key),
        Request::ReplayBufferFrame {key,time_ns}=>{
            let now=time_ns.map(MediaTime).unwrap_or(audio_capture.program_now()?.unwrap_or_else(||controller.now()));controller.sync(session,now)?;let (state,playback)=sync_capture_runtime(session,controller,playback,capture,camera,ndi_preview)?;
            files.start(session,&state,audio_capture)?;files.synchronize(session,&state,audio_capture)?;
            if !capture.is_busy(){capture.request_frame(session,state.now)?;}capture.poll(session,state.now)?;camera.poll(session)?;
            let frame=skylive_host::recording::RecordingFrame {session:session.identity(),time:state.now,model:session.snapshot(),output:state.output,playback,capture:merged_capture(session,capture,camera,capture_union,files)?,resource_generation:recording.generation()};replay.frame(key,frame)
        },        Request::DiscoverNdi {include_local}=>Ok(json!({"senders":files.discover_ndi(session,include_local)?,"discovery_requested":true})),
        Request::PrepareNdi {connection}=>Ok(json!({"ndi":files.prepare_ndi(session,connection)?,"actual_video_probed":true})),
        Request::ImportNdi {scene,name,connection,expected_revision}=>{
            if expected_revision.is_some_and(|r|r!=session.snapshot().revision()){return Err("NDI source request revision is stale".into());}
            let revision=session.snapshot().revision();let ndi=files.prepare_ndi(session,connection)?;camera.before_session_change()?;controller.before_change(session)?;
            let change=session.apply_at(Some(revision),Command::AddNdiSource {scene,name,ndi})?;Ok(json!({"change":change,"actual_video_probed":true}))
        },
        Request::NdiStatus=>{files.ndi.poll(session)?;Ok(files.ndi_summary())},
        Request::ProcessStatistics=>Ok(serde_json::to_value(skylive_host::process_statistics::sample()?)?),
        Request::PrepareMedia { path } => {
            let prepared = files.prepare(session, path)?;
            Ok(
                json!({"media":prepared.media,"info":prepared.info,"native_capture":false,"external_decoder_process":false}),
            )
        }
        Request::ImportMedia {
            scene,
            name,
            path,
            expected_revision,
        } => {
            let prepared = files.prepare(session, path)?;
            camera.before_session_change()?;
            controller.before_change(session)?;
            let change = session.apply_at(
                expected_revision,
                Command::AddMediaSource {
                    scene,
                    name,
                    media: prepared.media,
                },
            )?;
            Ok(json!({"change":change,"info":prepared.info}))
        }
        Request::StartMediaPlayback { time_ns } => {
            let now = time_ns.map(MediaTime).unwrap_or_else(|| controller.now());
            let state = controller.sync(session, now)?;
            files.start(session, &state, audio_capture)?;
            files.wait_ready(session, &state)?;
            Ok(files.summary())
        }
        Request::PrepareSlideshowEntries {entries}=>Ok(json!(skylive_host::image_slideshow::directories::prepare_entries(&entries)?)),
        Request::RefreshSlideshowEntries {source,expected_revision}=>{
            let snapshot=session.snapshot();if let Some(expected)=expected_revision{if expected!=snapshot.revision(){return Err(skylive_core::Error::Conflict{expected,actual:snapshot.revision()}.into());}}let Some(skylive_core::SourceKind::ImageSlideShow{settings})=snapshot.source(source).map(|source|source.kind())else{return Err("Source is not an image slideshow".into());};
            let prepared=skylive_host::image_slideshow::directories::prepare_entries(&settings.authored_entries())?;let refreshed=prepared.into_settings(settings)?;camera.before_session_change()?;controller.before_change(session)?;let change=session.apply_at(expected_revision,Command::SetImageSlideShow{source,settings:refreshed})?;playback.restart_source(source,controller.now())?;Ok(json!(change))
        },
        Request::SlideshowControl {source,operation,time_ns,expected_revision}=>{
            let actual=session.snapshot().revision();if let Some(expected)=expected_revision{if expected!=actual{return Err(skylive_core::Error::Conflict{expected,actual}.into());}}
            let now=time_ns.map(MediaTime).unwrap_or_else(||controller.now());let state=controller.sync(session,now)?;let mut outputs=state.output.scenes();if state.studio{outputs.push(FrameScene::Live(session.preview()));}if let Err(error)=playback.update_outputs(session,&outputs,now){if !(matches!(operation,skylive_host::image_slideshow::transport::Control::Restart|skylive_host::image_slideshow::transport::Control::Stop)&&error.is_random_work_limit()){return Err(error.into());}}
            let target=skylive_host::image_slideshow::transport::Target{session:session.identity(),revision:actual,source};Ok(json!(playback.control_slideshow(session,target,operation,now)?))
        },
        Request::ReseedSlideshow {source,seed,time_ns,expected_revision}=>{
            let actual=session.snapshot().revision();if let Some(expected)=expected_revision{if expected!=actual{return Err(skylive_core::Error::Conflict{expected,actual}.into());}}
            let now=time_ns.map(MediaTime).unwrap_or_else(||controller.now());let state=controller.sync(session,now)?;let mut outputs=state.output.scenes();if state.studio{outputs.push(FrameScene::Live(session.preview()));}if let Err(error)=playback.update_outputs(session,&outputs,now){if !error.is_random_work_limit(){return Err(error.into());}}
            let target=skylive_host::image_slideshow::transport::Target{session:session.identity(),revision:actual,source};Ok(json!(playback.reseed_slideshow(session,target,seed,now)?))
        },
        Request::SlideshowStatus {source,time_ns}=>{
            let now=time_ns.map(MediaTime).unwrap_or_else(||controller.now());let state=controller.sync(session,now)?;let mut outputs=state.output.scenes();if state.studio{outputs.push(FrameScene::Live(session.preview()));}playback.update_outputs(session,&outputs,now)?;
            let target=skylive_host::image_slideshow::transport::Target{session:session.identity(),revision:session.snapshot().revision(),source};Ok(json!(playback.slideshow_status(session,target,now)?))
        },
        Request::ReloadMedia{source,time_ns}=>files.reload_media(session,source,time_ns.map(MediaTime).unwrap_or_else(||controller.now())),
        Request::RestartMedia { source, time_ns } => {
            let now = time_ns.map(MediaTime).unwrap_or_else(|| controller.now());
            let state = controller.sync(session, now)?;
            files.start(session, &state, audio_capture)?;
            files.owner.restart(session, source, now)?;
            Ok(files.summary())
        }
        Request::MediaPlaybackStatus => {
            files.poll(session)?;
            Ok(files.summary())
        }
        Request::StopMediaPlayback => {
            files.stop(session)?;
            Ok(files.summary())
        }
        Request::StartStreaming { time_ns } => streaming.start(
            streaming::ProgramContext {
                session,
                controller,
                playback,
                capture,
                camera,
                union: capture_union,
                files,
                audio: audio_capture,
            },
            recording,
            time_ns,
        ),
        Request::StreamingStatus => streaming.status(),
        Request::FinishStreamVideo { key } => streaming.finish_track(key, false),
        Request::FinishStreamAudio { key } => streaming.finish_track(key, true),
        Request::StreamFrame { key, time_ns } => streaming.frame(
            streaming::ProgramContext {
                session,
                controller,
                playback,
                capture,
                camera,
                union: capture_union,
                files,
                audio: audio_capture,
            },
            key,
            time_ns,
        ),
        Request::StopStreaming { key } => streaming.stop(session, audio_capture, Some(key)),
        Request::StartRecording {
            path,
            time_ns,
            audio,
        } => {
            let now = match time_ns {
                Some(time) => MediaTime(time),
                None => audio_capture
                    .program_now()?
                    .unwrap_or_else(|| controller.now()),
            };
            let state = controller.sync(session, now)?;
            let continuous = audio_capture.program_key().is_some();
            recording.start(
                session,
                path,
                now,
                audio,
                continuous.then_some((&mut *audio_capture, &state.output)),
            )
        }
        Request::RecordFrame { time_ns } => {
            if let Some(time) = time_ns {
                controller.sync(session, MediaTime(time))?;
            }
            let (state, sample) =
                sync_capture_runtime(session, controller, playback, capture, camera,ndi_preview)?;
            files.start(session, &state, audio_capture)?;
            files.wait_ready(session, &state)?;
            let value = skylive_host::recording::RecordingFrame {
                session: session.identity(),
                time: state.now,
                model: session.snapshot(),
                output: state.output,
                playback: sample,
                capture: merged_capture(session, capture, camera, capture_union, files)?,
                resource_generation: recording.generation(),
            };
            recording.video(value)
        }
        Request::RecordingStatus => {
            Ok(json!({"status":recording.status(),"admitted":recording.active()}))
        }
        Request::PauseRecording {recording:identity,time_ns} | Request::ResumeRecording {recording:identity,time_ns}=>{
            let now=MediaTime(match time_ns {Some(time)=>time,None=>audio_capture.program_now()?.unwrap_or_else(||controller.now()).0});
            controller.sync(session,now)?;
            recording.set_paused(identity,session.identity(),now,pause_recording)
        }
        Request::StopRecording => recording.stop(),
        Request::Localization {
            path,
            language,
            key,
        } => {
            let current = session.snapshot();
            let language = language
                .as_deref()
                .unwrap_or(current.preferences().language());
            let catalog = match path {
                Some(path) => skylive_host::localization::Localization::load(&path, language)?,
                None => skylive_host::localization::Localization::embedded(language)?,
            };
            Ok(
                json!({"language":catalog.language(),"languages":catalog.languages().iter().map(|language|json!({"code":language.code,"name":language.name})).collect::<Vec<_>>(),"key_count":catalog.key_count(),"missing":catalog.missing(),"retained_bytes":catalog.retained_bytes(),"text":key.as_deref().map(|key|catalog.translate(key)),"native_capture":false}),
            )
        }
        Request::SetLanguage { language, path } => {
            let catalog = match path {
                Some(path) => skylive_host::localization::Localization::load(&path, &language)?,
                None => skylive_host::localization::Localization::embedded(&language)?,
            };
            let change = session.apply(Command::SetLanguage { language })?;
            Ok(
                json!({"change":change,"language":catalog.language(),"missing":catalog.missing(),"native_capture":false}),
            )
        }
        Request::Plugins => {
            let current = session.snapshot();
            Ok(
                json!({"schema":1,"providers":skylive_core::plugin_catalog().iter().map(|provider|json!({"descriptor":provider,"enabled":current.preferences().plugin_enabled(provider.id),"entry_points":skylive_core::plugin_entry_points(provider.id),"filter_factories":skylive_core::plugin_filter_factories(provider.id).map(|factory|json!({"factory":factory,"operation":factory.operation(),"label":factory.label(),"hint":factory.hint()})).collect::<Vec<_>>(),"source_creations":skylive_core::plugin_source_creations(provider.id)})).collect::<Vec<_>>(),"third_party_default_enabled":false,"native_binary_loading":false,"obs_binary_compatible":false}),
            )
        }
        Request::AudioMonitoringStatus=>Ok(json!({"available":audio_capture.monitoring_available(),"selected_device":session.snapshot().preferences().audio_monitoring_device(),"sources":session.snapshot().sources().iter().filter(|s|s.kind().has_audio()).map(|s|json!({"source":s.id(),"mode":s.audio_monitoring()})).collect::<Vec<_>>(),"status":audio_capture.program_status().map(|s|&s.monitor)})),
        Request::AudioProgramReset => {
            let (state, _) = sync_runtime(session, controller, playback,ndi_preview)?;
            let mut candidate =
                AudioProgram::new(session.snapshot().audio(), ProgramAudioLimits::default())?;
            candidate.route(session, &state.output, state.now)?;
            let result = json!({"status":candidate.status(),"identity":candidate.identity()?,"time_ns":state.now.0,"native_capture":false,"audio_output":false});
            *program = Some(candidate);
            Ok(result)
        }
        Request::AudioProgramStatus => Ok(
            json!({"status":program.as_ref().map(AudioProgram::status),"native_capture":false,"audio_output":false}),
        ),
        Request::AudioProgramRoute => {
            let (state, _) = sync_runtime(session, controller, playback,ndi_preview)?;
            let owner = program_owner(session, program)?;
            let changed = owner.route(session, &state.output, state.now)?;
            Ok(json!({"changed":changed,"status":owner.status(),"identity":owner.identity()?}))
        }
        Request::AudioProgramPacket {
            identity,
            binding,
            stream_generation,
            sequence,
            first_media_ns,
            received_media_ns,
            sample_rate,
            channels,
            channel_mask,
            samples,
            silent,
            discontinuity,
            queue_dropped,
        } => {
            let first_media_ns = signed_ns(&first_media_ns)?;
            let received_media_ns = signed_ns(&received_media_ns)?;
            let owner = program_owner(session, program)?;
            let generation = owner.status().data_generation;
            let result = owner.ingest(ProgramInputPacket {
                identity: identity.value(),
                binding: &binding,
                stream_generation,
                sequence,
                first_media_ns,
                received_media_ns,
                format: skylive_host::audio_capture::NativeAudioFormat {
                    sample_rate,
                    channels,
                    channel_mask,
                },
                samples: &samples,
                flags: skylive_host::audio_capture::NativePacketFlags {
                    silent,
                    discontinuity,
                    timestamp_error: false,
                    queue_dropped,
                },
            });
            let report = result.map_err(|error| {
                let mut error = CliError::from(error);
                if owner.status().data_generation != generation {
                    error.phase = Some("after_audio_runtime_reset");
                }
                error
            })?;
            Ok(
                json!({"report":report,"status":owner.status(),"native_capture":false,"audio_output":false}),
            )
        }
        Request::AudioProgramFinish {
            identity,
            binding,
            stream_generation,
            sequence,
        } => {
            let owner = program_owner(session, program)?;
            let generation = owner.status().data_generation;
            let report = owner
                .finish_input(identity.value(), &binding, stream_generation, sequence)
                .map_err(|error| {
                    let mut error = CliError::from(error);
                    if owner.status().data_generation != generation {
                        error.phase = Some("after_audio_runtime_reset");
                    }
                    error
                })?;
            Ok(
                json!({"report":report,"status":owner.status(),"native_capture":false,"audio_output":false}),
            )
        }
        Request::RenderAudioProgram {
            start_frame,
            frames,
        } => {
            let block = Arc::new(program_owner(session, program)?.render(start_frame, frames)?);
            recording.audio(block.clone())?;
            Ok(json!({"block":block,"native_capture":false,"audio_output":false}))
        }
        Request::SourceAudioControl{source,operation,expected_revision}=>{
            let model=session.snapshot();
            if let Some(expected)=expected_revision{if expected!=model.revision(){return Err(skylive_core::Error::Conflict{expected,actual:model.revision()}.into());}}
            let Some((resolved,command))=model.source_audio_hotkey_command(source,operation)? else{return Ok(json!({"accepted":false,"changed":false,"native_keyboard_event":false}));};
            camera.before_session_change()?;controller.before_change(session)?;
            let change=session.apply_at(Some(model.revision()),command)?;
            Ok(json!({"accepted":change.changed,"resolved":resolved,"change":change,"source_muted":session.snapshot().source(source).map(|source|source.muted()),"native_keyboard_event":false,"device_mute_changed":false}))
        }
        Request::Snapshot => snapshot(
            session,
            controller,
            playback,
            capture,
            audio_capture,
            camera,
            ndi_preview,
        ),
        Request::ListAudioApplications => {
            let key=audio_capture.request_processes(session)?;
            loop {
                if let Some(AudioCaptureEvent::Processes {key:actual,result})=audio_capture.poll(session)? {
                    if actual==key {return Ok(json!({"applications":result?,"backend":audio_capture.capabilities(),"capture_started":false,"audio_output":false}));}
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        Request::ListAudioDevices { flow } => {
            let key = audio_capture.request_devices(session, flow)?;
            loop {
                if let Some(AudioCaptureEvent::Devices {
                    key: actual,
                    result,
                    ..
                }) = audio_capture.poll(session)?
                {
                    if key == actual {
                        return Ok(
                            json!({"devices":result?,"backend":audio_capture.capabilities(),"audio_output":false}),
                        );
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        Request::CaptureAudio { time_ns } => {
            let state =
                controller.sync(session, MediaTime(time_ns.unwrap_or(controller.now().0)))?;
            sync_audio_runtime(session, &state, audio_capture)?;
            audio_capture.request_packets(session, state.now)?;
            while audio_capture.is_busy() {
                audio_capture.poll(session)?;
                if audio_capture.is_busy() {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            Ok(audio_capture_summary(audio_capture))
        }
        Request::StartNativeAudioProgram { time_ns } | Request::StartProgramAudio { time_ns } => {
            let now = match time_ns {
                Some(time) => MediaTime(time),
                None => audio_capture
                    .program_now()?
                    .unwrap_or_else(|| controller.now()),
            };
            let state = controller.sync(session, now)?;
            sync_audio_runtime(session, &state, audio_capture)?;
            let requested = if let Some(sink) = recording.live_sink(session.snapshot().audio())? {
                audio_capture.request_program_with_recording(
                    session,
                    &state.output,
                    state.now,
                    sink,
                )?
            } else {
                audio_capture.request_program(session, &state.output, state.now)?
            };
            files.ndi_running=true;
            files.synchronize(session, &state, audio_capture)?;
            let status = if requested {
                wait_program_ack(session, audio_capture, None)?
            } else {
                audio_capture.program_status().cloned().unwrap_or_default()
            };
            Ok(
                json!({"requested":requested,"time_ns":state.now.0,"status":status,"native_capture_requested":true,"native_capture_verified":false,"audio_output":false}),
            )
        }
        Request::ReadNativeAudioProgram { max_blocks }
        | Request::ReadProgramAudio { max_blocks } => {
            let max_blocks = max_blocks.unwrap_or(8);
            if !(1..=8).contains(&max_blocks) {
                return Err(
                    AudioCaptureError::Invalid("Program read max_blocks must be 1..8").into(),
                );
            }
            let Some(now) = audio_capture.program_now()? else {
                return Ok(
                    json!({"blocks":[],"status":null,"native_capture_requested":false,"audio_output":false}),
                );
            };
            let state = controller.sync(session, now)?;
            sync_audio_runtime(session, &state, audio_capture)?;
            if audio_capture.request_program(session, &state.output, state.now)? {
                wait_program_ack(session, audio_capture, None)?;
            }
            files.synchronize(session, &state, audio_capture)?;
            let mut blocks = Vec::with_capacity(max_blocks);
            for _ in 0..max_blocks {
                let Some(block) = audio_capture.poll_program(session)? else {
                    break;
                };
                blocks.push(block);
            }
            Ok(
                json!({"blocks":blocks,"status":audio_capture.program_status(),"time_ns":state.now.0,"audio_output":false}),
            )
        }
        Request::StopNativeAudioProgram => {
            let key = audio_capture.request_stop_program(session)?;
            let status = wait_program_ack(session, audio_capture, Some(key))?;
            Ok(
                json!({"stop_requested":true,"native_stops_acknowledged":false,"status":status,"audio_output":false}),
            )
        }
        Request::AudioPlan => {
            let (state, _) = sync_runtime(session, controller, playback,ndi_preview)?;
            let plan = audio::derive_audio_plan(session, &state.output)?;
            Ok(json!({"time_ns":state.now.0,"plan":plan,"native_capture":false}))
        }
        Request::MixProgramAudio {
            sample_rate,
            channels,
            frames,
            advance_ns,
            inputs,
        } => {
            let (state, _) = sync_runtime(session, controller, playback,ndi_preview)?;
            let plan = audio::derive_audio_plan(session, &state.output)?;
            let borrowed: Vec<_> = inputs
                .iter()
                .map(CliPcmInput::as_input)
                .collect::<Result<_, _>>()?;
            let mixed = plan.mix(
                AudioMixRequest {
                    sample_rate,
                    channels,
                    frames,
                    advance_ns,
                },
                &borrowed,
            )?;
            Ok(
                json!({"time_ns":state.now.0,"mixed":mixed,"native_capture":false,"audio_output":false}),
            )
        }
        Request::PrepareProfileFile {path} => {
            let profile=skylive_host::profile_io::load(&path)?;
            Ok(json!({"profile":profile,"format":"SkyLiveProfile","version":1,"starts_outputs":false}))
        }
        Request::ExportProfileFile {path} => {
            skylive_host::profile_io::save(&path,session.snapshot().profiles().selected().clone())?;
            Ok(json!({"path":path,"format":"SkyLiveProfile","version":1}))
        }
        Request::ListCameraControls {source} => {
            settle_camera_controls(session,camera)?;
            let key=camera.request_controls(session,source)?;
            loop {
                if let Some(CameraEvent::Controls {key:actual,result})=camera.poll(session)? {
                    if actual==key {return Ok(json!({"catalog":result?,"device_values_are_live":true,"hardware_values_persisted":false}));}
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        Request::SetCameraControl {lease,change} => {
            settle_camera_controls(session,camera)?;
            let key=camera.request_control_change(session,lease,change)?;
            loop {
                if let Some(CameraEvent::ControlSet {key:actual,lease,receipt})=camera.poll(session)? {
                    if actual==key {return Ok(json!({"lease":lease,"receipt":receipt,"hardware_values_persisted":false,"undo_available":false}));}
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        Request::ListCameraDevices => {
            settle_camera_controls(session, camera)?;
            let key = camera.request_devices(session)?;
            loop {
                if let Some(CameraEvent::Devices {
                    key: actual,
                    result,
                }) = camera.poll(session)?
                {
                    if actual == key {
                        return Ok(json!({"devices":result?,"backend":camera.capabilities()}));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        Request::ListCameraModes { target } => {
            settle_camera_controls(session, camera)?;
            let key = camera.request_modes(session, target)?;
            loop {
                if let Some(CameraEvent::Modes {
                    key: actual,
                    target,
                    result,
                }) = camera.poll(session)?
                {
                    if actual == key {
                        return Ok(
                            json!({"target":target,"modes":result?,"backend":camera.capabilities(),"mode_query_may_activate_device":true}),
                        );
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        Request::StartCameraSource {source,time_ns}|Request::StopCameraSource {source,time_ns}=>{
            let now=MediaTime(time_ns.unwrap_or(controller.now().0));let state=controller.sync(session,now)?;
            let key=files.camera_activation.request(session,source,start_camera_source,now).map_err(|error|CliError {code:error.code(),detail:error.to_string(),phase:None})?;
            files.camera_activation.synchronize(session,camera,audio_capture).map_err(|error|CliError {code:error.code(),detail:error.to_string(),phase:None})?;
            sync_audio_runtime(session,&state,audio_capture)?;
            let status=files.camera_activation.drive(session,camera,audio_capture,Some(&state.output),now).map_err(|error|CliError {code:error.code(),detail:error.to_string(),phase:None})?;
            Ok(json!({"request":key,"status":status,"acknowledgement_pending":true,"scope":"camera_source_video_and_optional_custom_audio"}))
        },
        Request::StopCameraCapture=>{
            let now=controller.now();let state=controller.sync(session,now)?;
            let key=files.camera_activation.stop_all(session,now).map_err(|error|CliError {code:error.code(),detail:error.to_string(),phase:None})?;
            files.camera_activation.synchronize(session,camera,audio_capture).map_err(|error|CliError {code:error.code(),detail:error.to_string(),phase:None})?;
            sync_audio_runtime(session,&state,audio_capture)?;
            let status=files.camera_activation.drive(session,camera,audio_capture,Some(&state.output),now).map_err(|error|CliError {code:error.code(),detail:error.to_string(),phase:None})?;
            Ok(json!({"request":key,"status":status,"stopped":false,"acknowledgement_pending":true,"scope":"all_camera_sources_only"}))
        },
        Request::CameraActivationStatus=>Ok(json!({"status":files.camera_activation.status(),"camera_video":camera_summary(camera),"audio_capture":audio_capture_summary(audio_capture)})),
        Request::DiscoverSystemFonts=>Ok(json!({"catalog":skylive_host::system_fonts::discover().map_err(|error|CliError{code:error.code,detail:error.to_string(),phase:None})?,"canonical_mutation":false})),
        Request::PrepareFont {path}=>Ok(json!({"catalog":skylive_host::font_assets::prepare_font(&path).map_err(|error|CliError {code:error.code,detail:error.to_string(),phase:None})?,"glyphs_rendered":false,"canonical_mutation":false})),
        Request::PrepareLut {path}=>Ok(json!({"asset":skylive_host::lut_assets::prepare_lut(&path).map_err(|error|CliError {code:error.code(),detail:error.to_string(),phase:None})?,"pixels_rendered":false,"canonical_mutation":false})),
        Request::ListWindows => {
            let key = capture.request_windows(session)?;
            loop {
                if let Some(CaptureEvent::Windows {
                    key: actual,
                    result,
                }) = capture.poll(session, controller.now())?
                {
                    if actual == key {
                        return Ok(
                            json!({"windows":result?,"backend":capture.window_capabilities()}),
                        );
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        Request::ProjectorRestoreSnapshot=>{let prepared=skylive_host::projector::restoration::prepare(session.identity(),&session.snapshot()).map_err(|error|CliError{code:"projector_restore",detail:error.to_string(),phase:None})?;Ok(json!({"snapshot":prepared.snapshot(),"monitor_resolution":"not_requested","windows_opened":false,"pixels_rendered":false}))},
        Request::ProjectorSnapshot {target,options,width,height}=>{let(state,_)=sync_runtime(session,controller,playback,ndi_preview)?;let mut planner=skylive_host::projector::Planner::default();let plan=planner.prepare(session,skylive_host::projector::Spec{id:1,epoch:1,session:session.identity(),target,options:options.unwrap_or_else(||skylive_host::projector::Options::from_preferences(session.snapshot().preferences().projectors())),width,height},&state.output,state.studio,&capture.snapshot()).map_err(|error|CliError{code:"projector_plan",detail:error.to_string(),phase:None})?;Ok(json!({"snapshot":plan.snapshot(),"windows_opened":false,"pixels_rendered":false}))},
        Request::ListDisplays => {
            let key = capture.request_displays(session)?;
            loop {
                if let Some(CaptureEvent::Displays {
                    key: actual,
                    result,
                }) = capture.poll(session, controller.now())?
                {
                    if actual == key {
                        return Ok(json!({"displays":result?,"backend":capture.capabilities()}));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        Request::CaptureFrame { time_ns } => {
            controller.sync(session, MediaTime(time_ns.unwrap_or(controller.now().0)))?;
            sync_capture_runtime(session, controller, playback, capture, camera,ndi_preview)?;
            let started = capture.request_frame(session, controller.now())?;
            while capture.is_busy() {
                capture.poll(session, controller.now())?;
                if capture.is_busy() {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            settle_camera_controls(session, camera)?;
            let camera_started = camera.request_frame(session, controller.now())?;
            while camera.is_busy() {
                if let Some(CameraEvent::Activated { result, .. }) = camera.poll(session)? {
                    result?;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let camera_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while camera.is_active()
                && camera.active_source_count() != 0
                && camera.snapshot().frames().count() < camera.active_source_count()
            {
                if std::time::Instant::now() >= camera_deadline {
                    return Err(CameraError::NoFrame.into());
                }
                camera.poll(session)?;
                let state = camera.snapshot();
                if state
                    .problems()
                    .iter()
                    .any(|p| p.code != "camera_frame_pending")
                {
                    return Err(CliError {
                        code: "camera_resources_unavailable",
                        detail: format!("{:?}", state.problems()),
                        phase: None,
                    });
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let state=controller.sync(session,controller.now())?;files.start(session,&state,audio_capture)?;files.wait_ready(session,&state)?;
            let sample = merged_capture(session, capture, camera, capture_union, files)?;
            if sample
                .problems()
                .iter()
                .any(|problem| problem.code != "capture_frame_unavailable")
            {
                return Err(CliError {
                    code: "capture_resources_unavailable",
                    detail: format!("{:?}", sample.problems()),
                    phase: None,
                });
            }
            Ok(
                json!({"requested":started,"camera_requested":camera_started,"ready":sample.problems().is_empty(),"capture":capture_summary(capture),"camera_capture":camera_summary(camera)}),
            )
        }
        Request::ResizePreviewSource{scene,item,handle,delta,display_scale,control,free_aspect,crop,expected_revision}=>{
            if let Some(expected)=expected_revision{let actual=session.snapshot().revision();if expected!=actual{return Err(skylive_core::Error::Conflict{expected,actual}.into());}}
            let captured=merged_capture(session,capture,camera,capture_union,files)?;
            let issue=|error:skylive_host::preview_interaction::InteractionError|CliError{code:"preview_snapping",detail:error.to_string(),phase:None};
            let target=skylive_host::preview_interaction::Target::read(session,&captured,scene,item).map_err(issue)?;
            let transform=target.resized_snapped(session,&captured,handle,delta,free_aspect,crop,display_scale,control).map_err(issue)?;
            let command=target.command(session,&captured,transform.clone()).map_err(issue)?;
            camera.before_session_change()?;controller.before_change(session)?;
            let change=session.apply_at(Some(target.revision),command)?;
            Ok(json!({"change":change,"transform":transform,"pointer_event":false,"native_capture_started":false}))
        },
        Request::MovePreviewSource{scene,item,delta,display_scale,control,expected_revision}=>{
            if let Some(expected)=expected_revision{let actual=session.snapshot().revision();if expected!=actual{return Err(skylive_core::Error::Conflict{expected,actual}.into());}}
            let captured=merged_capture(session,capture,camera,capture_union,files)?;
            let issue=|error:skylive_host::preview_interaction::InteractionError|CliError{code:"preview_snapping",detail:error.to_string(),phase:None};
            let target=skylive_host::preview_interaction::Target::read(session,&captured,scene,item).map_err(issue)?;
            let transform=target.moved_snapped(session,&captured,delta,display_scale,control).map_err(issue)?;
            let command=target.command(session,&captured,transform.clone()).map_err(issue)?;
            camera.before_session_change()?;controller.before_change(session)?;
            let change=session.apply_at(Some(target.revision),command)?;
            Ok(json!({"change":change,"transform":transform,"pointer_event":false,"native_capture_started":false}))
        },
        Request::GetDisplayTransform { scene, item } => {
            let projection = capture::project_capture_transform(
                session,
                merged_capture(session, capture, camera, capture_union, files)?.as_ref(),
                scene,
                item,
            )?
            .ok_or(CaptureError::InvalidFrame("item is not a display capture"))?;
            Ok(
                json!({"transform":projection.transform,"frame_dimensions":projection.context.observed_frame_size().map(|(width,height)|[width,height]),"revision":session.snapshot().revision()}),
            )
        }
        Request::SetDisplayTransform {
            scene,
            item,
            transform,
            expected_revision,
            expected_frame_dimensions,
        } => {
            if let Some(expected) = expected_revision {
                let actual = session.snapshot().revision();
                if expected != actual {
                    return Err(skylive_core::Error::Conflict { expected, actual }.into());
                }
            }
            let projection = capture::project_capture_transform(
                session,
                merged_capture(session, capture, camera, capture_union, files)?.as_ref(),
                scene,
                item,
            )?
            .ok_or(CaptureError::InvalidFrame("item is not a display capture"))?;
            if expected_frame_dimensions.is_some_and(|[width, height]| {
                projection.context.observed_frame_size() != Some((width, height))
            }) {
                return Err(CaptureError::InvalidFrame(
                    "display frame size changed; read the transform again",
                )
                .into());
            }
            let command = capture::prepare_capture_transform(
                session,
                merged_capture(session, capture, camera, capture_union, files)?.as_ref(),
                scene,
                item,
                transform,
                &projection.context,
            )?;
            if let Command::SetLanguage { language } = &command {
                skylive_host::localization::Localization::embedded(language)?;
            }
            camera.before_session_change()?;
            controller.before_change(session)?;
            session
                .apply_at(expected_revision, command)
                .map(|change| json!(change))
                .map_err(CliError::from)
        }
        Request::Capabilities => Ok(
            json!({"help_links":skylive_host::help_links::catalog(),"help_link_navigation":"desktop_explicit_action","implemented":["preview_move_snapping","preview_resize_snapping","obs_help_menu_links","missing_files_explicit_relink","missing_files_browser_local_html","scene_edit","managed_scene_collections","color_source","static_image_source","image_source_unload_and_linear_alpha","unicode_text_case_transform","text_static_font_files","camera_source_vertical_flip","craft_vignette","local_media_source","pure_rust_mp4_h264_aac","persistent_media_owner","media_program_audio","display_capture_source","window_capture_source","window_capture_workers","video_capture_source","persistent_camera_owner","display_capture_workers","animated_gif_source","image_slideshow_source","image_slideshow_cut_fade","image_slideshow_swipe_slide","image_slideshow_custom_canvas","image_slideshow_visibility","image_slideshow_manual_mode","image_slideshow_runtime_transport","image_slideshow_obs_random_legacy_v1","image_slideshow_obs_random_buffered_v2","image_slideshow_directory_entries","image_slideshow_explicit_folder_refresh","obs_slideshow_collection_v2_subset","obs_slideshow_collection_v1_subset","image_reload","rgba_preview","affine_transform","source_crop","undo_redo","json_save_recovery","immediate_cut","automatic_fade","named_transition_presets","swipe_transition","slide_transition","fade_to_color_transition","luma_wipe_transition","source_scale_aspect_filter","transition_preset_removal","studio_scene_instances","manual_tbar","quick_transitions","fade_to_black","scene_transition_override","obs_color_collection_v1_subset","obs_image_collection_v1_subset"],
            "text_font_files":{"formats":["ttf_glyf","ttc_glyf"],"cff":false,"system_catalog":false,"maximum_file_bytes":skylive_core::MAX_FONT_FILE_BYTES,"maximum_faces":skylive_core::MAX_FONT_FACES,"fallback":"explicit_embedded_choice_only","hinting":false,"runtime_verified":false},"virtual_camera":{"platform_supported":cfg!(all(target_os="windows",target_arch="x86_64")),"minimum_windows_build":22000,"output_routes":["program","preview","scene","source"],"profiles":[[1280,720,30],[1920,1080,30]],"cli":"inspect_factory_only","native_control":"desktop_owner","installation_required":true,"native_verified":false},"game_capture":{"available":skylive_host::game_capture::GameOwner::available(),"renderer":"d3d11_present_present1_vtable","supported_transport":"opaque_sdr_rgba8_bgra8_shared_keyed_texture","explicit_session_target_authorization":true,"process_lifetime_hook_pin":true,"max_pinned_targets":2,"max_actual_edge":4096,"max_actual_pixels":8847360,"native_verified":false},"camera_capture_backend":camera.capabilities(),"camera_capture_native_verified":false,"camera_source_orientation":{"flip_vertically":true,"default":false,"stage":"normalized_ingress_before_source_filters","changes_native_mode":false,"runtime_verified":false},"window_capture_methods":{"gdi":cfg!(target_os="windows"),"wgc":cfg!(target_os="windows"),"screen_capture_kit":cfg!(target_os="macos"),"runtime_verified":false},"window_capture_backend":capture.window_capabilities(),"window_capture_native_verified":false,"global_output_hotkeys":{"available":cfg!(target_os="windows"),"registration":"desktop_owner","default":"empty","maximum_bindings":skylive_core::MAX_GLOBAL_HOTKEYS,"hooks":false,"native_verified":false},"display_capture_methods":{"auto":cfg!(any(target_os="windows",target_os="macos")),"gdi":cfg!(target_os="windows"),"dxgi":cfg!(target_os="windows"),"wgc":cfg!(target_os="windows"),"dxgi_cursor_shapes":["color","monochrome","masked_color"],"wgc_minimum_windows_build":18362,"runtime_verified":false},"display_capture_backend":capture.capabilities(),"replay_buffer":{"available":skylive_host::replay_buffer::available(),"container":"mp4","ring":"encoded_avc_aac","duration_seconds":[1,600],"memory_mib":[16,2048],"default_duration_seconds":20,"default_memory_mib":512,"hardware_encoding":"explicit_nvenc_h264_selection","native_verified":false},"ndi_output":{"compiled":skylive_host::ndi::output::available(),"runtime":"installed_NDI6","start":"explicit_only","program":"global_output_scaled_video_and_optional_program_pcm","preview":"base_canvas_video_only","video_schedule":"caller_driven","native_verified":false},"media_decoders":skylive_host::mediafile::decoder_capabilities(),"video_encoders":skylive_host::native_h264::encoder_capabilities(),"remux":skylive_host::remux::capabilities(),"process_priority":skylive_host::process_priority::capabilities(),"recording":skylive_host::recording::capabilities(),"streaming":streaming::capabilities(),"audio":{"authoring":true,"project_settings":true,"explicit_pcm_mix":true,"timestamped_pcm_program":true,"continuous_program_worker":true,"native_capture":audio_capture.capabilities().input||audio_capture.capabilities().output,"capture_backend":audio_capture.capabilities(),"binding_defaults":audio_capture.binding_defaults(),"native_capture_verified":false,"resampling":true,"continuous_native_transport":audio_capture.capabilities().input||audio_capture.capabilities().output,"adaptive_buffering":true,"monitoring":audio_capture.monitoring_available()},"unavailable":(["game_capture","audio_device_output","recording","streaming","replay_buffer","virtual_camera","browser","obs_websocket","native_plugins","hdr_transitions"].into_iter().filter(|item| match *item {"recording"=>!skylive_host::recording::available(),"replay_buffer"=>!skylive_host::replay_buffer::available(),"game_capture"=>!skylive_host::game_capture::GameOwner::available(),"browser"=>!skylive_host::browser::BrowserOwner::available(),"streaming"=>!cfg!(target_os="windows"),"virtual_camera"=>!cfg!(all(target_os="windows",target_arch="x86_64")),_=>true}).collect::<Vec<_>>())}),
        ),
        Request::ScanMissingFiles{start}=>Ok(json!({"session":session.identity(),"revision":session.snapshot().revision(),"page":skylive_host::missing_files::scan(&session.snapshot(),start).map_err(CliError::from)?,"content_checked":false})),
        Request::PrepareResourceRelink{choice}=>{let prepared=skylive_host::missing_files::prepare(skylive_host::missing_files::Request{model:session.snapshot(),choice}).map_err(CliError::from)?;let reload_request=if prepared.expected==prepared.replacement{match prepared.slot{skylive_core::ResourceSlot::Media{..}=>Some("reload_media"),skylive_core::ResourceSlot::BrowserHtml{..}=>Some("reload_browser"),_=>None}}else{None};let reload_source=prepared.slot.source();Ok(json!({"runtime_reload":reload_request.map(|request|if request=="reload_media"{json!({"request":request,"source":reload_source,"time_ns":null})}else{json!({"request":request,"source":reload_source})}),"session":session.identity(),"revision":session.snapshot().revision(),"command":Command::RelinkResources{session:session.identity(),revision:session.snapshot().revision(),batch:skylive_core::ResourceRelinks{rows:vec![prepared]}},"published":false}))},
        Request::CollectionLibrary{action,remember}=>{
            let prepared=collection_library::prepare(session,&files.documents,action)?;
            let collection_library::Prepared::Replace{opened,remove,created}=prepared else{let collection_library::Prepared::Listed(view)=prepared else{return Err("invalid collection operation phase".into());};return Ok(json!({"library":view,"read_only":true}));};
            let path=opened.path.clone();let candidate=session.adopt_document_preferences(opened.loaded.session).map_err(|source|{let mut error=CliError::from(source);if created{error.phase=Some("after_publication");error.detail.push_str(&format!("; created managed native artifact retained at {}",path.display()));}error})?;
            let lifecycle=(||->Result<(),CliError>{camera.before_session_change()?;controller.before_change(session)?;files.stop(session)?;stop_live_audio_if_running(session,audio_capture)?;Ok(())})();
            if let Err(mut error)=lifecycle{if created{error.detail.push_str(&format!("; created managed native artifact retained at {}",path.display()));}return Err(error);}
            *program=None;*session=candidate;*obs_ids=None;files.documents.active=Some(path.clone());
            let mut view=opened.view;let mut recovery=None;let mut remove_error=None;
            if let Some(plan)=remove{match skylive_host::collection_library::remove(plan,&path){Ok(result)=>{view=result.view;recovery=Some(result.recovery);remove_error=result.cleanup_problem;},Err(error)=>remove_error=Some(error.to_string())}}
            let remember_error=if remember{collection_library::remember(path.clone()).err()}else{None};
            let mut value=match snapshot(session,controller,playback,capture,audio_capture,camera,ndi_preview){Ok(value)=>value,Err(error)=>json!({"snapshot_error":error})};
            if let Some(fields)=value.as_object_mut(){fields.insert("published".into(),json!(true));fields.insert("library".into(),json!(view));fields.insert("native_path".into(),json!(path));fields.insert("created".into(),json!(created));fields.insert("recovery".into(),json!(recovery));fields.insert("remove_error".into(),json!(remove_error));fields.insert("remember_requested".into(),json!(remember));fields.insert("remember_error".into(),json!(remember_error));}
            Ok(value)
        },
        Request::New => {
            if session.is_dirty() {
                return Err("save dirty collection before replacing it".into());
            }
            let candidate = session.new_collection()?;
            camera.before_session_change()?;
            controller.before_change(session)?;
            files.stop(session)?;
            stop_live_audio_if_running(session, audio_capture)?;
            *program = None;
            *session = candidate;
            files.documents.active=None;
            *obs_ids = None;
            snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )
        }
        Request::ResizeOutputToSource{scene,item,expected_revision}=>{
            guard_source_output_resize(recording.active()||streaming.active()||replay.owner.active()||files.outputs.owner.any_active())?;
            if let Some(expected)=expected_revision{let actual=session.snapshot().revision();if expected!=actual{return Err(skylive_core::Error::Conflict{expected,actual}.into());}}
            let capture=merged_capture(session,capture,camera,capture_union,files)?;
            let plan=skylive_host::source_output_resize::Plan::prepare(session,&capture,scene,item).map_err(|error|CliError{code:"source_output_resize",detail:error.to_string(),phase:None})?;
            let command=plan.command(session,&capture).map_err(|error|CliError{code:"source_output_resize_stale",detail:error.to_string(),phase:None})?;
            let size=plan.size();let revision=plan.revision();
            camera.before_session_change()?;controller.before_change(session)?;
            let change=session.apply_at(Some(revision),command)?;
            Ok(json!({"change":change,"size":size,"native_output_started":false}))
        }
        Request::Command {
            command,
            expected_revision,
        } => {
            if let Some(expected) = expected_revision {
                let actual = session.snapshot().revision();
                if expected != actual {
                    return Err(skylive_core::Error::Conflict { expected, actual }.into());
                }
            }
            let command = match command {
                Command::ResizeOutputToSource{scene,item,raw_width,raw_height}=>{
                    guard_source_output_resize(recording.active()||streaming.active()||replay.owner.active()||files.outputs.owner.any_active())?;
                    let capture=merged_capture(session,capture,camera,capture_union,files)?;
                    let plan=skylive_host::source_output_resize::Plan::prepare(session,&capture,scene,item).map_err(|error|CliError{code:"source_output_resize",detail:error.to_string(),phase:None})?;
                    if (raw_width,raw_height)!=(plan.size().raw_width,plan.size().raw_height){return Err("Declared source dimensions do not match the current source frame.".into());}
                    plan.command(session,&capture).map_err(|error|CliError{code:"source_output_resize_stale",detail:error.to_string(),phase:None})?
                }
                Command::TransformItem {
                    scene,
                    item,
                    action,
                } => capture::prepare_transform_command(
                    session,
                    merged_capture(session, capture, camera, capture_union, files)?.as_ref(),
                    scene,
                    item,
                    action,
                )?,
                other => other,
            };
            camera.before_session_change()?;
            controller.before_change(session)?;
            session
                .apply_at(expected_revision, command)
                .map(|v| json!(v))
                .map_err(CliError::from)
        }
        Request::SelectPreview { scene, time_ns } => {
            let now = time_ns.map(MediaTime).unwrap_or_else(|| controller.now());
            controller.select_preview(session, scene, now)?;
            snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )
        }
        Request::Cut { time_ns } => {
            let now = time_ns.map(MediaTime).unwrap_or_else(|| controller.now());
            controller.cut(session, now)?;
            snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )
        }
        Request::StudioMode { enabled, time_ns } => {
            let now = time_ns.map(MediaTime).unwrap_or_else(|| controller.now());
            controller.set_studio(session, enabled, now)?;
            snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )
        }
        Request::Transition { time_ns } => {
            let now = time_ns.map(MediaTime).unwrap_or_else(|| controller.now());
            controller.transition(session, now)?;
            snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )
        }
        Request::QuickTransition { id, time_ns } => {
            let now = time_ns.map(MediaTime).unwrap_or_else(|| controller.now());
            controller.quick(session, id, now)?;
            snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )
        }
        Request::TbarPosition { value, time_ns } => {
            let now = time_ns.map(MediaTime).unwrap_or_else(|| controller.now());
            controller.manual_position(session, value, now)?;
            snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )
        }
        Request::TbarRelease { time_ns } => {
            let now = time_ns.map(MediaTime).unwrap_or_else(|| controller.now());
            controller.manual_release(session, now)?;
            snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )
        }
        Request::Advance { time_ns } => {
            controller.sync(session, MediaTime(time_ns))?;
            snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )
        }
        Request::Save { path } => {
            let root=collection_library::root().ok();let absolute=std::path::absolute(&path).map_err(|error|CliError::from(error.to_string()))?;
            let receipt=skylive_host::collection_library::save_native(root.as_ref(),SaveJob::new(session,path))?;let stamp=receipt.file_stamp();let accepted=receipt.accept(session);
            let mut library_error=None;if accepted{files.documents.active=Some(absolute.clone());if let Some(root)=root{if let Err(error)=skylive_host::collection_library::sync_saved_name(&root,&absolute,session.snapshot().name(),stamp){library_error=Some(error.to_string());}}}
            Ok(json!({"accepted":accepted,"library_name_error":library_error}))
        }
        Request::Checkpoint { path } => {
            SaveJob::new(session, path)
                .checkpoint()
                .map_err(CliError::from)?;
            Ok(json!({"checkpointed":true}))
        }
        Request::Load { path } => {
            if session.is_dirty() {
                return Err("save dirty collection before replacing it".into());
            }
            let loaded = storage::load_with_report(&path).map_err(CliError::from)?;
            let candidate = loaded.session;
            let migration = loaded.migration;
            camera.before_session_change()?;
            controller.before_change(session)?;
            files.stop(session)?;
            stop_live_audio_if_running(session, audio_capture)?;
            *program = None;
            *session = candidate;
            files.documents.active=std::path::absolute(&path).ok();
            *obs_ids = None;
            let mut value = snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )?;
            value
                .as_object_mut()
                .ok_or("snapshot result is not an object")?
                .insert(
                    "migration".into(),
                    serde_json::to_value(migration).map_err(CliError::from)?,
                );
            Ok(value)
        }
        Request::Recover { path } => {
            if session.is_dirty() {
                return Err("save dirty collection before recovery".into());
            }
            let loaded = storage::recover_with_report(&path).map_err(CliError::from)?;
            let candidate = loaded.session;
            let migration = loaded.migration;
            camera.before_session_change()?;
            controller.before_change(session)?;
            files.stop(session)?;
            stop_live_audio_if_running(session, audio_capture)?;
            *program = None;
            *session = candidate;
            files.documents.active=None;
            *obs_ids = None;
            let mut value = snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )?;
            value
                .as_object_mut()
                .ok_or("snapshot result is not an object")?
                .insert(
                    "migration".into(),
                    serde_json::to_value(migration).map_err(CliError::from)?,
                );
            Ok(value)
        }
        Request::ImportObs {
            path,
            width,
            height,
            fps_num,
            fps_den,
        } => {
            if session.is_dirty() {
                return Err("save dirty collection before import".into());
            }
            let candidate = obs::load_collection_with_preferences(
                &path,
                obs::ImportVideo {
                    width,
                    height,
                    fps_num,
                    fps_den,
                },
                session.snapshot().preferences(),
            )?
            .adopt_into(session)?;
            camera.before_session_change()?;
            controller.before_change(session)?;
            files.stop(session)?;
            stop_live_audio_if_running(session, audio_capture)?;
            *program = None;
            session.close();
            *session = candidate.session;
            *obs_ids = Some(Arc::new(candidate.identities));
            snapshot(
                session,
                controller,
                playback,
                capture,
                audio_capture,
                camera,
                ndi_preview,
            )
        }
        Request::ExportObs { path } => {
            let bytes = obs::export_collection(session, obs_ids.as_deref())?;
            storage::publish_bytes(&path, &bytes)?;
            Ok(
                json!({"path":path,"format":"obs_image_collection_v1_subset","native_dirty":session.is_dirty()}),
            )
        }
        Request::PrepareSlideShowImages {paths}=>{
            let images=skylive_host::image_slideshow::prepare(&paths)?;
            Ok(json!({"images":images,"metadata_only":true}))
        }
        Request::ImportSlideShow {scene,name,paths,entries,duration_ms,transition,transition_ms,looping,shuffle,hide_when_done,canvas,behavior,mode,randomize,random_compatibility,expected_revision}=>{
            skylive_core::ImageSlideShow::validate_timing(duration_ms,transition,transition_ms)?;skylive_core::ImageSlideShow::validate_randomization(shuffle,randomize,random_compatibility)?;canvas.validate()?;
            if let Some(expected)=expected_revision {let actual=session.snapshot().revision();if expected!=actual{return Err(skylive_core::Error::Conflict {expected,actual}.into());}}
            let entries=if entries.is_empty(){paths.iter().map(|path|{let text=path.to_str().ok_or("Slideshow path is not exact UTF-8")?;skylive_core::SlideShowEntry::new(skylive_core::SlideShowEntryKind::File,text.to_owned()).map_err(|error|error.to_string())}).collect::<Result<Vec<_>,String>>()?}else{if !paths.is_empty(){return Err("Choose typed slideshow entries or legacy file paths, not both".into());}entries};
            let prepared=skylive_host::image_slideshow::directories::prepare_entries(&entries)?;
            let settings=skylive_core::ImageSlideShow::new_with_options(prepared.images,duration_ms,transition,transition_ms,looping,shuffle,hide_when_done,canvas,behavior)?.with_mode(mode)?.with_randomization(randomize,random_compatibility)?.with_entries(prepared.entries,prepared.counts)?;
            camera.before_session_change()?;controller.before_change(session)?;
            session.apply_at(expected_revision,Command::AddImageSlideShow {scene,name,settings}).map(|change|json!(change)).map_err(CliError::from)
        }
        Request::RebindSlideShow {source,paths,entries,duration_ms,transition,transition_ms,looping,shuffle,hide_when_done,canvas,behavior,mode,randomize,random_compatibility,expected_revision}=>{
            skylive_core::ImageSlideShow::validate_timing(duration_ms,transition,transition_ms)?;skylive_core::ImageSlideShow::validate_randomization(shuffle,randomize,random_compatibility)?;canvas.validate()?;
            if let Some(expected)=expected_revision {let actual=session.snapshot().revision();if expected!=actual{return Err(skylive_core::Error::Conflict {expected,actual}.into());}}
            let entries=if entries.is_empty(){paths.iter().map(|path|{let text=path.to_str().ok_or("Slideshow path is not exact UTF-8")?;skylive_core::SlideShowEntry::new(skylive_core::SlideShowEntryKind::File,text.to_owned()).map_err(|error|error.to_string())}).collect::<Result<Vec<_>,String>>()?}else{if !paths.is_empty(){return Err("Choose typed slideshow entries or legacy file paths, not both".into());}entries};
            let prepared=skylive_host::image_slideshow::directories::prepare_entries(&entries)?;
            let settings=skylive_core::ImageSlideShow::new_with_options(prepared.images,duration_ms,transition,transition_ms,looping,shuffle,hide_when_done,canvas,behavior)?.with_mode(mode)?.with_randomization(randomize,random_compatibility)?.with_entries(prepared.entries,prepared.counts)?;
            camera.before_session_change()?;controller.before_change(session)?;
            let change=session.apply_at(expected_revision,Command::SetImageSlideShow {source,settings})?;
            playback.restart_source(source,controller.now())?;
            Ok(json!(change))
        }
        Request::ImportImage {
            scene,
            name,
            path,
            settings,
            expected_revision,
        } => {
            if let Some(expected)=expected_revision {let actual=session.snapshot().revision();if expected!=actual{return Err(skylive_core::Error::Conflict{expected,actual}.into());}}
            camera.before_session_change()?;
            controller.before_change(session)?;
            let prepared = assets::prepare_image(&path)?;
            drop(prepared.frame);
            session
                .apply_at(
                    expected_revision,
                    Command::ImageSource {command:skylive_core::ImageSourceCommand::Add {
                        scene,
                        name,
                        asset: prepared.asset,
                        settings,
                    }},
                )
                .map(|change| json!(change))
                .map_err(CliError::from)
        }
        Request::ReloadImage {
            source,
            path,
            settings,
            expected_revision,
        } => {
            if let Some(expected)=expected_revision {let actual=session.snapshot().revision();if expected!=actual{return Err(skylive_core::Error::Conflict{expected,actual}.into());}}
            let current=session.snapshot();
            let target=current.source(source).ok_or(skylive_core::Error::NotFound(source))?;
            if !matches!(target.kind(),skylive_core::SourceKind::Image{..}){return Err("The image source no longer exists or changed type.".into());}
            let settings=settings.unwrap_or_else(||target.image_settings());
            camera.before_session_change()?;
            controller.before_change(session)?;
            let prepared = assets::prepare_image(&path)?;
            drop(prepared.frame);
            let change = session.apply_at(
                expected_revision,
                Command::ImageSource {command:skylive_core::ImageSourceCommand::Configure {
                    source,
                    asset: prepared.asset,
                    settings,
                }},
            )?;
            playback.restart_source(source, controller.now())?;
            recording.invalidate_resources()?;
            streaming.invalidate_resources()?;
            Ok(json!(change))
        }
        Request::Render {
            path,
            scene,
            width,
            height,
            time_ns,
        } => {
            let image = media.render_gpu.output_renderer().render_preview(
                &session.snapshot(),
                &skylive_host::transition::FrameComposition::Scene(FrameScene::Live(
                    scene.unwrap_or(session.preview()),
                )),
                width,
                height,
                &PlaybackSnapshot::at_time(session, MediaTime(time_ns)),
                merged_capture(session, capture, camera, capture_union, files)?.as_ref(),
            )
            .map_err(CliError::from)?;
            let mut bytes = format!("P6\n{width} {height}\n255\n").into_bytes();
            for pixel in image.pixels().chunks_exact(4) {
                bytes.extend_from_slice(&pixel[..3]);
            }
            // Export refuses to overwrite existing data. No codec or recording claim.
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .map_err(CliError::from)?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(CliError::from)?;
            Ok(
                json!({"path":path,"width":width,"height":height,"time_ns":time_ns,"format":"ppm_rgb8"}),
            )
        }
        Request::RenderProgram {
            path,
            width,
            height,
            time_ns,
        } => {
            let now = time_ns.map(MediaTime).unwrap_or_else(|| controller.now());
            controller.sync(session, now)?;
            let (state, sample) =
                sync_capture_runtime(session, controller, playback, capture, camera,ndi_preview)?;
            files.start(session, &state, audio_capture)?;
            files.wait_ready(session, &state)?;
            let image = media.render_gpu.output_renderer().render_preview(
                &session.snapshot(),
                &state.output,
                width,
                height,
                &sample,
                merged_capture(session, capture, camera, capture_union, files)?.as_ref(),
            )?;
            let mut bytes = format!("P6\n{width} {height}\n255\n").into_bytes();
            for pixel in image.pixels().chunks_exact(4) {
                bytes.extend_from_slice(&pixel[..3]);
            }
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)?;
            file.write_all(&bytes).and_then(|_| file.sync_all())?;
            Ok(
                json!({"path":path,"width":width,"height":height,"time_ns":state.now.0,"format":"ppm_rgb8","composition":state.output.sample_key()}),
            )
        }
        Request::Close => {
            files.remux.close();if !files.remux.is_retired(){return Err("Remux file worker is retiring; poll RemuxStatus before closing".into());}

            if session.is_dirty() {
                return Err("save dirty collection before closing".into());
            }
            if streaming.active() {
                streaming.stop(session, audio_capture, None)?;
            }
            if recording.active() {
                recording.stop()?;
            }
            files.outputs.owner.close();
            replay.owner.close();
            files.ndi.close();files.ndi_union.clear();files.browser.close();files.browser_union.clear();
            files.owner.close();
            files.union.clear();
            audio_capture.close();
            camera.close();
            capture_union.clear();
            capture.close();
            *program = None;
            let priority_restore=process_priority.close_wait()?;
            session.close();
            Ok(json!({"closed":true,"process_priority_restore":priority_restore}))
        }
    }
}
fn run(reader: &mut dyn BufRead, writer: &mut dyn Write) -> Result<(), CliError> {
    // Headless loop owner, declared before the native workers so their final
    // drops/joins settle first. Repeated synchronous Render requests share the
    // model; on error/EOF the last actual owner performs background cleanup.
    let _render_services = preview::retain_render_services().map_err(|error| CliError {
        code: error.code(),
        detail: error.to_string(),
        phase: None,
    })?;
    #[cfg(test)]
    let render_gpu=skylive_host::render_gpu::RenderGpu::cpu();
    #[cfg(not(test))]
    let render_gpu=skylive_host::render_gpu::RenderGpu::new();
    let mut session = Session::new();
    let mut process_priority=process_priority_cli::Runtime::default();
    let mut checkpoint = None;
    let mut obs_ids = None;
    let mut controller = Controller::new();
    let mut playback = render_gpu.playback_clock()?;
    let mut capture = CaptureWorker::new(capture::native::backend())?;
    let mut camera = CameraOwner::native()?;
    let mut capture_union = capture::CaptureUnion::default();
    let mut audio_capture = AudioCaptureWorker::native()?;
    let mut files = file_media::Runtime::new_with_render_gpu(&audio_capture,render_gpu.clone())?;
    let mut replay=replay_buffer::Runtime::new(render_gpu.clone());
    let mut program = None;
    let mut recording = recording::Recorder::new_with_render_gpu(render_gpu.clone());
    // Declared after AudioCaptureWorker: headless Drop cancels/waits the output
    // while its Program producer owner is still alive to signal the end barrier.
    let mut streaming = streaming::Runtime::new_with_render_gpu(render_gpu.clone());
    for _ in 0..MAX_REQUESTS {
        let mut bytes = Vec::new();
        reader
            .take(MAX_REQUEST_BYTES + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(CliError::from)?;
        if bytes.is_empty() {
            if streaming.active() {
                streaming.stop(&session, &mut audio_capture, None)?;
            }
            if recording.active() {
                recording.stop()?;
            }
            if session.is_dirty()
                && checkpoint != Some((session.identity(), session.snapshot().revision()))
            {
                return Err("input ended with unsaved edits; save or checkpoint before EOF".into());
            }
            return Ok(());
        }
        let result = if bytes.len() as u64 > MAX_REQUEST_BYTES {
            Err("request exceeds 1 MiB".into())
        } else {
            serde_json::from_slice::<Request>(&bytes)
                .map_err(CliError::from)
                .and_then(|r| {
                    let creates_checkpoint =
                        matches!(&r, Request::Checkpoint { .. } | Request::Recover { .. });
                    let result = execute(
                        &mut session,
                        &mut obs_ids,
                        &mut CliMedia {
                            process_priority:&mut process_priority,
                            render_gpu: &render_gpu,
                            files: &mut files,
                            replay:&mut replay,
                            recording: &mut recording,
                            streaming: &mut streaming,
                            controller: &mut controller,
                            playback: &mut playback,
                            capture: &mut capture,
                            camera: &mut camera,
                            capture_union: &mut capture_union,
                            audio_capture: &mut audio_capture,
                            program: &mut program,
                        },
                        r,
                    );
                    if result.is_ok() && creates_checkpoint {
                        checkpoint = Some((session.identity(), session.snapshot().revision()));
                    }
                    result
                })
        };
        let response = match &result {
            Ok(value) => json!({"ok":true,"result":value}),
            Err(error) => json!({"ok":false,"error":error}),
        };
        serde_json::to_writer(&mut *writer, &response).map_err(CliError::from)?;
        writer
            .write_all(b"\n")
            .and_then(|_| writer.flush())
            .map_err(CliError::from)?;
        result?;
    }
    Err("request count exceeds 4096".into())
}
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        [mode] if mode == "--stdio" => run(&mut io::stdin().lock(), &mut io::stdout().lock()),
        [mode, path] if mode == "--script" => fs::File::open(path)
            .map_err(CliError::from)
            .and_then(|f| run(&mut io::BufReader::new(f), &mut io::stdout().lock())),
        [] => {
            println!(
                "SkyLive headless CLI\n  skylive-cli --stdio\n  skylive-cli --script requests.jsonl\nNative schemas 1..={}; shared typed commands, capture, source filters and configured transitions. Query capabilities for the actual platform support for devices, recording and streaming. Native capture requires explicit requests and platform permission. See doc/migration/current-checkpoint.md for supported scopes and verification.",
                skylive_host::storage::CURRENT_SCHEMA
            );
            Ok(())
        }
        _ => Err("usage: skylive-cli [--stdio | --script PATH]".into()),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use skylive_core::{DisplayCaptureBinding, DisplayTarget};
    use skylive_host::capture::{CaptureBackend, CaptureCapabilities, CaptureLimits, DisplayInfo};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Fake {
        enumerations: Arc<AtomicUsize>,
        captures: Arc<AtomicUsize>,
        width: Arc<AtomicUsize>,
    }
    impl CaptureBackend for Fake {
        fn capabilities(&self) -> CaptureCapabilities {
            CaptureCapabilities {
                available: true,
                backend: "fixture",
                cursor: true,
                hdr: false,
                note: None,
            }
        }
        fn window_capabilities(&self) -> CaptureCapabilities {
            self.capabilities()
        }
        fn enumerate_windows(
            &mut self,
            _: CaptureLimits,
        ) -> Result<Vec<skylive_host::window_capture::WindowInfo>, CaptureError> {
            self.enumerations.fetch_add(1, Ordering::SeqCst);
            Ok(vec![skylive_host::window_capture::WindowInfo {
                full_window_size: None,
                target: skylive_core::WindowTarget::Windows {
                    title: "Fixture window".into(),
                    class: "FixtureClass".into(),
                    executable: "fixture.exe".into(),
                },
                name: "Fixture window".into(),
                width: 2,
                height: 2,
                minimized: false,
            }])
        }
        fn capture_window(
            &mut self,
            _: &skylive_core::WindowCaptureBinding,
            limits: CaptureLimits,
        ) -> Result<skylive_media::RgbaFrame, CaptureError> {
            self.capture(
                &DisplayCaptureBinding::new(
                    DisplayTarget::Windows {
                        device_id: "fixture-screen".into(),
                    },
                    false,
                    2,
                    2,
                )
                .unwrap(),
                limits,
            )
        }
        fn enumerate(&mut self, _: CaptureLimits) -> Result<Vec<DisplayInfo>, CaptureError> {
            self.enumerations.fetch_add(1, Ordering::SeqCst);
            Ok(vec![DisplayInfo {
                target: DisplayTarget::Windows {
                    device_id: "fixture-screen".into(),
                },
                name: "Fixture screen".into(),
                width: 2,
                height: 2,
                primary: true,
            }])
        }
        fn capture(
            &mut self,
            _: &DisplayCaptureBinding,
            _: CaptureLimits,
        ) -> Result<skylive_media::RgbaFrame, CaptureError> {
            self.captures.fetch_add(1, Ordering::SeqCst);
            let width = self.width.load(Ordering::SeqCst);
            let mut pixels = Vec::new();
            for y in 0..2 {
                for x in 0..width {
                    pixels.extend_from_slice(match (x % 2 == 0, y == 0) {
                        (true, true) => &[255, 0, 0, 255],
                        (false, true) => &[0, 255, 0, 255],
                        (true, false) => &[0, 0, 255, 255],
                        (false, false) => &[255, 255, 255, 255],
                    });
                }
            }
            skylive_media::RgbaFrame::new(width as u32, 2, pixels)
                .map_err(|_| CaptureError::InvalidFrame("fixture pixels"))
        }
    }
    struct Harness {
        render_gpu: skylive_host::render_gpu::RenderGpu,
        files: file_media::Runtime,
        recording: recording::Recorder,
        streaming: streaming::Runtime,
        session: Session,
        ids: Option<Arc<ObsIdentities>>,
        controller: Controller,
        playback: PlaybackClock,
        capture: CaptureWorker,
        camera: CameraOwner,
        capture_union: capture::CaptureUnion,
        audio_capture: AudioCaptureWorker,
        program: Option<AudioProgram>,
        enumerations: Arc<AtomicUsize>,
        captures: Arc<AtomicUsize>,
        width: Arc<AtomicUsize>,
    }
    impl Harness {
        fn new() -> Self {
            let enumerations = Arc::new(AtomicUsize::new(0));
            let captures = Arc::new(AtomicUsize::new(0));
            let width = Arc::new(AtomicUsize::new(2));
            let capture = CaptureWorker::new(Box::new(Fake {
                enumerations: enumerations.clone(),
                captures: captures.clone(),
                width: width.clone(),
            }))
            .unwrap();
            let audio_capture = AudioCaptureWorker::new(
                Box::new(|| Err(AudioCaptureError::Unsupported("headless fixture"))),
                skylive_host::audio_capture::AudioCapabilities {
                    backend: "fixture-unavailable",
                    input: false,
                    output: false,
                    device_enumeration: false,
                    exclude_own_audio: false,
                    note: None,
                },
            )
            .unwrap();
            let files = file_media::Runtime::new(&audio_capture).unwrap();
            Self {
                render_gpu: skylive_host::render_gpu::RenderGpu::cpu(),
                files,
                recording: Default::default(),
                streaming: Default::default(),
                session: Session::new(),
                ids: None,
                controller: Controller::new(),
                playback: PlaybackClock::new(),
                capture,
                camera: CameraOwner::new(
                    Box::new(|| Err(CameraError::Unavailable("headless camera fixture"))),
                    skylive_host::camera_capture::CameraCapabilities {
                        available: false,
                        backend: "fake-unavailable",
                        video: false,
                        audio: false,
                        note: None,
                    },
                    skylive_host::camera_capture::CameraLimits::default(),
                )
                .unwrap(),
                capture_union: Default::default(),
                audio_capture,
                program: None,
                enumerations,
                captures,
                width,
            }
        }
        fn call(&mut self, data: Value) -> Result<Value, CliError> {
            execute(
                &mut self.session,
                &mut self.ids,
                &mut CliMedia {
                    render_gpu: &self.render_gpu,
                    files: &mut self.files,
                    recording: &mut self.recording,
                    streaming: &mut self.streaming,
                    controller: &mut self.controller,
                    playback: &mut self.playback,
                    capture: &mut self.capture,
                    camera: &mut self.camera,
                    capture_union: &mut self.capture_union,
                    audio_capture: &mut self.audio_capture,
                    program: &mut self.program,
                },
                serde_json::from_value(data).unwrap(),
            )
        }
    }
    #[test]
    fn rendering_without_media_never_acquires_file_wall_clock() {
        let temp = Temp::new();
        let mut host = Harness::new();
        host.call(json!({"request":"render_program","path":temp.0.join("empty.ppm"),"width":2,"height":2,"time_ns":125000000})).unwrap();
        assert!(!host.files.running);
        assert!(host.files.owner.media_now().unwrap().is_none());
        assert_eq!(host.controller.now(), MediaTime(125000000));
        std::thread::sleep(std::time::Duration::from_millis(12));
        host.call(json!({"request":"snapshot"})).unwrap();
        assert_eq!(host.controller.now(), MediaTime(125000000));
        assert_eq!(host.enumerations.load(Ordering::SeqCst), 0);
        assert_eq!(host.captures.load(Ordering::SeqCst), 0);
        host.call(json!({"request":"close"})).unwrap();
    }
    #[test]
    fn typed_source_channel_controls_use_explicit_original_layout_and_native_persistence() {
        let temp = Temp::new();
        let mut host = Harness::new();
        host.call(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"CPU voice","binding":{"flow":"input","target":{"kind":"default"},"use_device_timing":false,"exclude_own_audio":false}}})).unwrap();
        let source = host.session.snapshot().sources()[0].id();
        host.call(json!({"request":"command","command":{"op":"set_source_audio_properties","source":source,"properties":{"force_mono":false,"balance":0.0}}})).unwrap();
        let converted=host.call(json!({"request":"mix_program_audio","sample_rate":48000,"channels":2,"frames":1,"inputs":[{"kind":"converted","source":source,"sample_rate":48000,"channels":2,"samples":[0.25,0.25],"original":{"sample_rate":48000,"channels":1,"channel_mask":4}}]})).unwrap();
        assert_eq!(converted["mixed"]["samples"], json!([0.25, 0.25]));
        let native=host.call(json!({"request":"mix_program_audio","sample_rate":48000,"channels":2,"frames":1,"inputs":[{"kind":"samples","source":source,"sample_rate":48000,"channels":2,"samples":[0.25,0.25]}]})).unwrap();
        assert_eq!(native["mixed"]["samples"], json!([0.25, 0.0]));
        host.call(json!({"request":"command","command":{"op":"set_source_audio_properties","source":source,"properties":{"force_mono":true,"balance":0.5}}})).unwrap();
        let path = temp.0.join("channel-properties.json");
        host.call(json!({"request":"save","path":path})).unwrap();
        host.call(json!({"request":"command","command":{"op":"undo"}}))
            .unwrap();
        assert_eq!(
            host.session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_properties()
                .balance(),
            0.0
        );
        host.call(json!({"request":"command","command":{"op":"redo"}}))
            .unwrap();
        host.call(json!({"request":"save","path":path})).unwrap();
        host.call(json!({"request":"load","path":path})).unwrap();
        assert!(
            host.session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_properties()
                .force_mono()
        );
        assert_eq!(host.enumerations.load(Ordering::SeqCst), 0);
        assert_eq!(host.captures.load(Ordering::SeqCst), 0);
        host.call(json!({"request":"close"})).unwrap();
    }
    #[test]
    fn real_media_cli_pixels_pcm_and_failed_load_preserve_live_owner_without_native_devices() {
        let temp = Temp::new();
        let mut host = Harness::new();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/host/tests/fixtures/media/no_pns/cavlc.mp4")
            .canonicalize()
            .unwrap();
        let scene = host.session.preview();
        host.call(json!({"request":"command","command":{"op":"set_video","width":64,"height":48,"fps_num":10,"fps_den":1}})).unwrap();
        let prepared = host
            .call(json!({"request":"prepare_media","path":path}))
            .unwrap();
        assert_eq!(prepared["media"]["width"], 64);
        assert!(!host.files.running);
        host.call(json!({"request":"import_media","scene":scene,"name":"Movie","path":path}))
            .unwrap();
        assert!(!host.files.running);
        let started = host
            .call(json!({"request":"start_media_playback","time_ns":0}))
            .unwrap();
        assert_eq!(started["frames"][0]["width"], 64);
        let rendered = temp.0.join("movie.ppm");
        host.call(
            json!({"request":"render_program","path":rendered,"width":64,"height":48,"time_ns":0}),
        )
        .unwrap();
        let mut decoder =
            skylive_host::mediafile::MediaFileReader::open(&path, Default::default()).unwrap();
        let picture = decoder.next_video().unwrap().unwrap();
        let frame = decoder.rgba(&picture).unwrap();
        let mut expected = b"P6\n64 48\n255\n".to_vec();
        for pixel in frame.pixels().chunks_exact(4) {
            expected.extend_from_slice(&pixel[..3]);
        }
        assert_eq!(fs::read(&rendered).unwrap(), expected);
        host.call(json!({"request":"start_program_audio","time_ns":0}))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(90));
        let output = host
            .call(json!({"request":"read_program_audio","max_blocks":8}))
            .unwrap();
        assert!(output["status"]["input_packets"].as_u64().unwrap() > 0);
        assert_eq!(output["status"]["input_errors"], 0);
        assert!(
            output["blocks"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|b| b["block"]["samples"].as_array().unwrap())
                .any(|v| v.as_f64().unwrap().abs() > 0.01)
        );
        assert_eq!(host.enumerations.load(Ordering::SeqCst), 0);
        assert_eq!(host.captures.load(Ordering::SeqCst), 0);
        host.call(json!({"request":"save","path":temp.0.join("native.json")}))
            .unwrap();
        let identity = host.session.identity();
        assert!(
            host.call(json!({"request":"load","path":temp.0.join("missing.json")}))
                .is_err()
        );
        assert_eq!(host.session.identity(), identity);
        assert!(host.files.running);
        host.call(json!({"request":"stop_media_playback"})).unwrap();
        host.call(json!({"request":"stop_native_audio_program"}))
            .unwrap();
        host.call(json!({"request":"close"})).unwrap();
        host.audio_capture.join().unwrap();
        host.files.owner.join().unwrap();
    }
    struct Temp(PathBuf);
    fn continuous_harness() -> (Harness, Arc<[AtomicUsize; 4]>) {
        use skylive_core::{AudioCaptureBinding, AudioFlow};
        use skylive_host::audio_capture::{
            AudioCapabilities, AudioCaptureLimits, AudioDeviceInfo, NativeAudioBackend,
            NativeAudioFormat, NativeAudioStream, NativePacketFlags, NativePcmBlock,
        };
        struct Backend {
            counts: Arc<[AtomicUsize; 4]>,
            _owner: std::rc::Rc<()>,
        }
        struct Stream {
            counts: Arc<[AtomicUsize; 4]>,
            packet: usize,
        }
        fn capabilities() -> AudioCapabilities {
            AudioCapabilities {
                backend: "injected-continuous-PCM",
                input: true,
                output: false,
                device_enumeration: true,
                exclude_own_audio: false,
                note: None,
            }
        }
        impl NativeAudioBackend for Backend {
            fn capabilities(&self) -> AudioCapabilities {
                capabilities()
            }
            fn host_now_ns(&self) -> Result<u64, AudioCaptureError> {
                Ok(1_000_000_000)
            }
            fn enumerate(
                &mut self,
                _: AudioFlow,
                _: AudioCaptureLimits,
            ) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
                self.counts[3].fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            }
            fn open(
                &mut self,
                _: &AudioCaptureBinding,
                _: AudioCaptureLimits,
            ) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
                self.counts[1].fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Stream {
                    counts: self.counts.clone(),
                    packet: 0,
                }))
            }
        }
        impl NativeAudioStream for Stream {
            fn poll(&mut self) -> Result<Option<NativePcmBlock>, AudioCaptureError> {
                if self.packet == 8 {
                    return Ok(None);
                }
                let position = self.packet * 4096;
                self.packet += 1;
                Ok(Some(NativePcmBlock {
                    format: NativeAudioFormat {
                        sample_rate: 48000,
                        channels: 1,
                        channel_mask: 4,
                    },
                    frames: 4096,
                    samples: vec![0.25; 4096],
                    device_timestamp_ns: Some(
                        1_000_000_000 + position as u64 * 1_000_000_000 / 48000,
                    ),
                    graph_timestamp_ns: None,
                    directshow_clock: None,
                    host_received_ns: 1_000_000_000
                        + (position + 4096) as u64 * 1_000_000_000 / 48000,
                    device_frame_position: Some(position as u64),
                    flags: NativePacketFlags::default(),
                }))
            }
            fn close(&mut self) -> Result<(), AudioCaptureError> {
                self.counts[2].fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
        let counts = Arc::new(std::array::from_fn(|_| AtomicUsize::new(0)));
        let worker_counts = counts.clone();
        let mut h = Harness::new();
        h.audio_capture = AudioCaptureWorker::new(
            Box::new(move || {
                worker_counts[0].fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Backend {
                    counts: worker_counts,
                    _owner: std::rc::Rc::new(()),
                }))
            }),
            capabilities(),
        )
        .unwrap();
        (h, counts)
    }
    fn add_continuous_source(h: &mut Harness) {
        h.call(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"Injected microphone","binding":{"flow":"input","target":{"kind":"default"},"use_device_timing":true,"exclude_own_audio":false}}})).unwrap();
    }
    fn read_fake_program(h: &mut Harness, max_blocks: usize) -> Value {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let result = h
                .call(json!({"request":"read_native_audio_program","max_blocks":max_blocks}))
                .unwrap();
            if result["blocks"]
                .as_array()
                .unwrap()
                .first()
                .is_some_and(|output| {
                    output["block"]["samples"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|sample| sample.as_f64() == Some(0.25))
                })
            {
                return result;
            }
            assert!(
                std::time::Instant::now() < until,
                "fake Program must produce a bounded output block"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    #[test]
    fn parsed_continuous_start_read_stop_restart_share_one_injected_native_owner() {
        let (mut h, counts) = continuous_harness();
        add_continuous_source(&mut h);
        assert_eq!(
            h.call(json!({"request":"read_native_audio_program"}))
                .unwrap()["blocks"],
            json!([])
        );
        assert_eq!(
            h.call(json!({"request":"stop_native_audio_program"}))
                .unwrap()["stop_requested"],
            true
        );
        h.call(json!({"request":"audio_program_reset"})).unwrap();
        h.call(json!({"request":"audio_plan"})).unwrap();
        assert_eq!(
            counts[0].load(Ordering::SeqCst),
            0,
            "inactive read/stop must not initialize native backend"
        );
        let caps = h.call(json!({"request":"capabilities"})).unwrap();
        assert_eq!(caps["audio"]["continuous_program_worker"], true);
        assert_eq!(caps["audio"]["continuous_native_transport"], true);
        assert_eq!(caps["audio"]["native_capture_verified"], false);
        let started = h
            .call(json!({"request":"start_native_audio_program","time_ns":0}))
            .unwrap();
        assert_eq!(started["status"]["running"], true);
        assert_eq!(started["audio_output"], false);
        let first = read_fake_program(&mut h, 2);
        assert!(first["blocks"].as_array().unwrap().len() <= 2);
        let first_key = first["blocks"][0]["key"].clone();
        assert_eq!(
            first["status"]["input_packets"], 8,
            "every fake native packet must reach Program"
        );
        assert!(
            first["blocks"][0]["block"]["samples"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value.as_f64() == Some(0.25))
        );
        if first["blocks"].as_array().unwrap().len() == 2 {
            assert_eq!(
                first["blocks"][1]["block"]["start_frame"].as_u64().unwrap(),
                first["blocks"][0]["block"]["start_frame"].as_u64().unwrap() + 1024
            );
        }
        let stopped = h
            .call(json!({"request":"stop_native_audio_program"}))
            .unwrap();
        assert_eq!(stopped["status"]["running"], false);
        assert_eq!(stopped["native_stops_acknowledged"], false);
        let inactive = h
            .call(json!({"request":"read_native_audio_program","max_blocks":8}))
            .unwrap();
        assert!(inactive["status"].is_null());
        assert_eq!(inactive["blocks"], json!([]));
        h.call(json!({"request":"start_native_audio_program"}))
            .unwrap();
        assert_eq!(
            counts[0].load(Ordering::SeqCst),
            1,
            "restart must reuse the existing native owner/factory"
        );
        assert_eq!(counts[1].load(Ordering::SeqCst), 2);
        assert_eq!(counts[3].load(Ordering::SeqCst), 0);
        let next = read_fake_program(&mut h, 1);
        assert_ne!(
            next["blocks"][0]["key"]["generation"],
            first_key["generation"]
        );
        h.call(json!({"request":"stop_native_audio_program"}))
            .unwrap();
        h.audio_capture.close();
        h.audio_capture.join().unwrap();
        assert!(counts[2].load(Ordering::SeqCst) >= 2);
    }
    #[test]
    fn parsed_continuous_read_limits_precede_native_admission_and_program_ack_errors_propagate() {
        let (mut h, counts) = continuous_harness();
        for limit in [0, 9] {
            assert_eq!(
                h.call(json!({"request":"read_native_audio_program","max_blocks":limit}))
                    .unwrap_err()
                    .code,
                "audio_capture_invalid"
            );
        }
        assert_eq!(counts[0].load(Ordering::SeqCst), 0);
        let error = h
            .call(json!({"request":"start_native_audio_program","time_ns":u64::MAX-1_000_000}))
            .unwrap_err();
        assert_eq!(error.code, "audio_capture_time_overflow");
        assert!(error.phase.is_none());
        assert_eq!(
            counts[0].load(Ordering::SeqCst),
            0,
            "empty route failure must not initialize a backend"
        );
        h.audio_capture.close();
        h.audio_capture.join().unwrap();
    }
    #[test]
    fn parsed_failed_load_recover_and_import_keep_current_continuous_program_alive() {
        let temp = Temp::new();
        let (mut h, counts) = continuous_harness();
        add_continuous_source(&mut h);
        h.call(json!({"request":"save","path":temp.0.join("current.json")}))
            .unwrap();
        h.call(json!({"request":"start_native_audio_program","time_ns":0}))
            .unwrap();
        let identity = h.session.identity();
        for request in [
            json!({"request":"load","path":temp.0.join("missing.json")}),
            json!({"request":"recover","path":temp.0.join("missing.json")}),
            json!({"request":"import_obs","path":temp.0.join("missing.json"),"width":1920,"height":1080,"fps_num":30,"fps_den":1}),
        ] {
            assert!(h.call(request).is_err());
            assert_eq!(h.session.identity(), identity);
            assert!(h.audio_capture.program_now().unwrap().is_some());
            assert_eq!(counts[2].load(Ordering::SeqCst), 0);
            assert_eq!(
                h.call(json!({"request":"read_native_audio_program","max_blocks":1}))
                    .unwrap()["status"]["running"],
                true
            );
        }
        h.call(json!({"request":"stop_native_audio_program"}))
            .unwrap();
        h.audio_capture.close();
        h.audio_capture.join().unwrap();
    }
    #[test]
    fn parsed_successful_collection_replacement_stops_previous_program_after_candidate_preparation()
    {
        for operation in ["new", "load", "recover", "import_obs"] {
            let temp = Temp::new();
            let native = temp.0.join("candidate.json");
            let (mut h, counts) = continuous_harness();
            add_continuous_source(&mut h);
            h.call(json!({"request":"save","path":native})).unwrap();
            h.call(json!({"request":"checkpoint","path":native}))
                .unwrap();
            h.call(json!({"request":"start_native_audio_program","time_ns":0}))
                .unwrap();
            let identity = h.session.identity();
            let request = match operation {
                "new" => json!({"request":"new"}),
                "load" => json!({"request":"load","path":native}),
                "recover" => json!({"request":"recover","path":native}),
                _ => {
                    json!({"request":"import_obs","path":PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/host/tests/fixtures/obs-color-absolute.json"),"width":1920,"height":1080,"fps_num":30,"fps_den":1})
                }
            };
            h.call(request).unwrap();
            assert_ne!(h.session.identity(), identity);
            assert!(h.audio_capture.program_now().unwrap().is_none());
            assert!(h.audio_capture.program_status().is_none());
            assert_eq!(
                h.call(json!({"request":"read_native_audio_program"}))
                    .unwrap()["blocks"],
                json!([])
            );
            assert_eq!(counts[0].load(Ordering::SeqCst), 1);
            assert_eq!(counts[1].load(Ordering::SeqCst), 1);
            h.audio_capture.close();
            h.audio_capture.join().unwrap();
            assert!(counts[2].load(Ordering::SeqCst) >= 1);
        }
    }
    #[test]
    fn parsed_native_audio_requests_use_only_injected_backend_and_explicit_admission() {
        use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget};
        use skylive_host::audio_capture::{
            AudioCapabilities, AudioCaptureLimits, AudioDeviceInfo, NativeAudioBackend,
            NativeAudioFormat, NativeAudioStream, NativePacketFlags, NativePcmBlock,
        };
        use std::rc::Rc;
        struct AudioFake {
            counts: Arc<[AtomicUsize; 3]>,
            _owner: Rc<()>,
        }
        struct Stream {
            sent: bool,
        }
        fn caps() -> AudioCapabilities {
            AudioCapabilities {
                backend: "injected-PCM",
                input: true,
                output: false,
                device_enumeration: true,
                exclude_own_audio: false,
                note: None,
            }
        }
        impl NativeAudioBackend for AudioFake {
            fn capabilities(&self) -> AudioCapabilities {
                caps()
            }
            fn host_now_ns(&self) -> Result<u64, AudioCaptureError> {
                Ok(1_000_000_000)
            }
            fn enumerate(
                &mut self,
                flow: AudioFlow,
                _: AudioCaptureLimits,
            ) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
                self.counts[1].fetch_add(1, Ordering::SeqCst);
                Ok(vec![AudioDeviceInfo {
                    target: AudioTarget::WindowsEndpoint {
                        id: "fixture-no-system".into(),
                    },
                    name: "Injected microphone".into(),
                    flow,
                    is_default: true,
                }])
            }
            fn open(
                &mut self,
                _: &AudioCaptureBinding,
                _: AudioCaptureLimits,
            ) -> Result<Box<dyn NativeAudioStream>, AudioCaptureError> {
                self.counts[2].fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Stream { sent: false }))
            }
        }
        impl NativeAudioStream for Stream {
            fn poll(&mut self) -> Result<Option<NativePcmBlock>, AudioCaptureError> {
                if self.sent {
                    return Ok(None);
                }
                self.sent = true;
                Ok(Some(NativePcmBlock {
                    format: NativeAudioFormat {
                        sample_rate: 8000,
                        channels: 1,
                        channel_mask: 4,
                    },
                    frames: 2,
                    samples: vec![0.25, -0.5],
                    device_timestamp_ns: Some(999_750_000),
                    graph_timestamp_ns: None,
                    directshow_clock: None,
                    host_received_ns: 1_000_000_000,
                    device_frame_position: Some(8),
                    flags: NativePacketFlags::default(),
                }))
            }
            fn close(&mut self) -> Result<(), AudioCaptureError> {
                Ok(())
            }
        }
        let counts = Arc::new([
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        ]);
        let worker_counts = counts.clone();
        let mut h = Harness::new();
        h.audio_capture = AudioCaptureWorker::new(
            Box::new(move || {
                worker_counts[0].fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(AudioFake {
                    counts: worker_counts,
                    _owner: Rc::new(()),
                }))
            }),
            caps(),
        )
        .unwrap();
        let temp = Temp::new();
        h.call(json!({"request":"command","command":{"op":"add_audio_capture_source","scene":1,"name":"Mic","binding":{"flow":"input","target":{"kind":"default"},"use_device_timing":false,"exclude_own_audio":false}}})).unwrap();
        h.call(json!({"request":"snapshot"})).unwrap();
        h.call(json!({"request":"capabilities"})).unwrap();
        h.call(
            json!({"request":"render","path":temp.0.join("audio-only.ppm"),"width":2,"height":2}),
        )
        .unwrap();
        assert_eq!(
            counts[0].load(Ordering::SeqCst),
            0,
            "authoring, rendering and capabilities cannot touch a backend"
        );
        let listed = h
            .call(json!({"request":"list_audio_devices","flow":"input"}))
            .unwrap();
        assert_eq!(listed["devices"][0]["name"], "Injected microphone");
        assert_eq!(counts[1].load(Ordering::SeqCst), 1);
        let captured = h
            .call(json!({"request":"capture_audio","time_ns":1_000_000_000u64}))
            .unwrap();
        assert_eq!(captured["packets"].as_array().unwrap().len(), 1);
        assert_eq!(captured["packets"][0]["format"]["sample_rate"], 8000);
        assert_eq!(captured["packets"][0]["meters"][0]["peak"], 0.5);
        assert_eq!(captured["packets"][0]["timing_origin"], "receive_estimate");
        assert_eq!(captured["program_audio_output"], false);
        assert!(
            captured["packets"][0].get("samples").is_none(),
            "metadata query must not duplicate PCM payload per SourceID"
        );
        let generation = captured["packets"][0]["stream_generation"]
            .as_u64()
            .unwrap();
        h.call(json!({"request":"command","command":{"op":"rebind_audio_capture_source","source":2,"binding":{"flow":"input","target":{"kind":"windows_endpoint","id":"other-injected-endpoint"},"use_device_timing":true,"exclude_own_audio":false}}})).unwrap();
        assert_eq!(
            counts[2].load(Ordering::SeqCst),
            1,
            "rebind alone cannot open a device"
        );
        let rebound = h
            .call(json!({"request":"capture_audio","time_ns":1_020_000_000u64}))
            .unwrap();
        assert_eq!(counts[2].load(Ordering::SeqCst), 2);
        assert!(rebound["packets"][0]["stream_generation"].as_u64().unwrap() > generation);
        assert_eq!(rebound["packets"][0]["timing_origin"], "device");
        h.call(json!({"request":"save","path":temp.0.join("audio.json")}))
            .unwrap();
        h.call(json!({"request":"close"})).unwrap();
        h.audio_capture.join().unwrap();
    }
    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(1);
            let path = std::env::temp_dir().join(format!(
                "skylive-cli-capture-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn parsed_camera_requests_use_persistent_injected_stream_and_real_pixel_union() {
        use skylive_core::{
            VideoCaptureBinding, VideoCaptureMode, VideoCaptureTarget, VideoPixelFormat,
        };
        use skylive_host::camera_capture::{
            CameraCapabilities, CameraDeviceInfo, CameraLimits, CameraModeInfo,
            NativeCameraBackend, NativeCameraStream, NativeVideoFrame,
        };
        struct Backend {
            counts: Arc<[AtomicUsize; 5]>,
            _owner: std::rc::Rc<()>,
        }
        struct Stream {
            counts: Arc<[AtomicUsize; 5]>,
            emitted: bool,
        }
        fn target() -> VideoCaptureTarget {
            VideoCaptureTarget::Windows {
                symbolic_link: "fixture-camera".into(),
            }
        }
        fn mode() -> VideoCaptureMode {
            VideoCaptureMode::new(2, 2, 30, 1, VideoPixelFormat::Rgb32).unwrap()
        }
        impl NativeCameraBackend for Backend {
            fn enumerate_devices(
                &mut self,
                _: CameraLimits,
            ) -> Result<Vec<CameraDeviceInfo>, CameraError> {
                self.counts[1].fetch_add(1, Ordering::SeqCst);
                Ok(vec![CameraDeviceInfo {
                    target: target(),
                    name: "Fixture camera".into(),
                }])
            }
            fn enumerate_modes(
                &mut self,
                _: &VideoCaptureTarget,
                _: CameraLimits,
            ) -> Result<Vec<CameraModeInfo>, CameraError> {
                self.counts[2].fetch_add(1, Ordering::SeqCst);
                Ok(vec![CameraModeInfo {
                    width: 2,
                    height: 2,
                    fps_num: 30,
                    fps_den: 1,
                    native_subtype: "RGB32".into(),
                    mode: Some(mode()),
                    unsupported_reason: None,
                }])
            }
            fn open(
                &mut self,
                _: &VideoCaptureBinding,
                _: CameraLimits,
            ) -> Result<Box<dyn NativeCameraStream>, CameraError> {
                self.counts[3].fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Stream {
                    counts: self.counts.clone(),
                    emitted: false,
                }))
            }
        }
        impl NativeCameraStream for Stream {
            fn poll(&mut self, _: CameraLimits) -> Result<Option<NativeVideoFrame>, CameraError> {
                if self.emitted {
                    return Ok(None);
                }
                self.emitted = true;
                Ok(Some(NativeVideoFrame {
                    image: skylive_media::RgbaFrame::new(
                        2,
                        2,
                        vec![
                            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
                        ],
                    )
                    .unwrap(),
                    presentation_ns: 1234,
                    device_qpc_ns: None,
                    directshow_clock: None,
                    discontinuity: false,
                    dropped_frames: 0,
                }))
            }
            fn stop(&mut self) -> Result<bool, CameraError> {
                self.counts[4].fetch_add(1, Ordering::SeqCst);
                Ok(true)
            }
        }
        let mut host = Harness::new();
        host.camera.join().unwrap();
        let counts = Arc::new(std::array::from_fn(|_| AtomicUsize::new(0)));
        let created = counts.clone();
        host.camera = CameraOwner::new(
            Box::new(move || {
                created[0].fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Backend {
                    counts: created,
                    _owner: std::rc::Rc::new(()),
                }))
            }),
            CameraCapabilities {
                available: true,
                backend: "injected-camera",
                video: true,
                audio: false,
                note: None,
            },
            CameraLimits::default(),
        )
        .unwrap();
        host.call(json!({"request":"capabilities"})).unwrap();
        host.call(json!({"request":"snapshot"})).unwrap();
        assert_eq!(counts[0].load(Ordering::SeqCst), 0);
        let binding = VideoCaptureBinding::new(target(), mode(), false).unwrap();
        host.call(json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}})).unwrap();
        host.call(json!({"request":"command","command":{"op":"add_video_capture_source","scene":1,"name":"Camera","binding":binding}})).unwrap();
        assert_eq!(counts[0].load(Ordering::SeqCst), 0);
        assert_eq!(
            host.call(json!({"request":"list_camera_devices"})).unwrap()["devices"][0]["name"],
            "Fixture camera"
        );
        assert_eq!(counts[3].load(Ordering::SeqCst), 0);
        assert_eq!(
            host.call(json!({"request":"list_camera_modes","target":target()}))
                .unwrap()["modes"][0]["mode"],
            serde_json::to_value(mode()).unwrap()
        );
        let captured = host
            .call(json!({"request":"capture_frame","time_ns":0}))
            .unwrap();
        assert_eq!(captured["camera_capture"]["frames"][0]["width"], 2);
        assert_eq!(counts[3].load(Ordering::SeqCst), 1);
        host.call(json!({"request":"command","command":{"op":"transform_item","scene":1,"item":3,"action":"stretch_to_canvas"}})).unwrap();
        let temp = Temp::new();
        let image = temp.0.join("camera.ppm");
        let native = temp.0.join("camera.json");
        host.call(
            json!({"request":"render_program","path":image,"width":2,"height":2,"time_ns":0}),
        )
        .unwrap();
        let bytes = fs::read(image).unwrap();
        assert_eq!(
            &bytes[b"P6\n2 2\n255\n".len()..],
            &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]
        );
        host.call(json!({"request":"save","path":native})).unwrap();
        let old = host.session.identity();
        host.call(json!({"request":"load","path":native})).unwrap();
        assert_ne!(host.session.identity(), old);
        assert!(!host.camera.is_active());
        assert_eq!(counts[3].load(Ordering::SeqCst), 1);
        assert!(host.session.snapshot().sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::VideoCapture{binding:b}if b==&binding)));
        host.call(json!({"request":"capture_frame","time_ns":0}))
            .unwrap();
        assert_eq!(counts[3].load(Ordering::SeqCst), 2);
        let stopped = host.call(json!({"request":"stop_camera_capture"})).unwrap();
        assert_eq!(stopped["native_stops_acknowledged"], true);
        assert!(!host.camera.is_active());
        host.capture_union.clear();
        host.camera.join().unwrap();
    }
    #[test]
    fn parsed_pending_display_frame_keeps_the_owner_alive_until_a_real_frame_arrives() {
        use skylive_host::capture::DisplayCaptureOutcome;
        struct PendingFirst {
            inner: Fake,
            pending: bool,
        }
        impl CaptureBackend for PendingFirst {
            fn capabilities(&self) -> CaptureCapabilities {
                self.inner.capabilities()
            }
            fn enumerate(
                &mut self,
                limits: CaptureLimits,
            ) -> Result<Vec<DisplayInfo>, CaptureError> {
                self.inner.enumerate(limits)
            }
            fn capture(
                &mut self,
                binding: &DisplayCaptureBinding,
                limits: CaptureLimits,
            ) -> Result<skylive_media::RgbaFrame, CaptureError> {
                self.inner.capture(binding, limits)
            }
            fn capture_display(
                &mut self,
                binding: &DisplayCaptureBinding,
                limits: CaptureLimits,
            ) -> Result<DisplayCaptureOutcome, CaptureError> {
                if self.pending {
                    self.pending = false;
                    return Ok(DisplayCaptureOutcome::NoNewFrame);
                }
                self.capture(binding, limits)
                    .map(|image| DisplayCaptureOutcome::Frame {
                        image,
                        metadata: None,
                    })
            }
        }
        let mut host = Harness::new();
        host.capture = CaptureWorker::new(Box::new(PendingFirst {
            inner: Fake {
                enumerations: host.enumerations.clone(),
                captures: host.captures.clone(),
                width: host.width.clone(),
            },
            pending: true,
        }))
        .unwrap();
        let binding = DisplayCaptureBinding::new(
            DisplayTarget::Windows {
                device_id: "fixture-screen".into(),
            },
            false,
            2,
            2,
        )
        .unwrap();
        host.call(json!({"request":"command","command":{"op":"add_display_capture_source","scene":1,"name":"Pending GPU frame","binding":binding}})).unwrap();
        let pending = host
            .call(json!({"request":"capture_frame","time_ns":0}))
            .unwrap();
        assert_eq!(pending["ready"], false);
        assert_eq!(
            pending["capture"]["problems"][0]["code"],
            "capture_frame_unavailable"
        );
        assert_eq!(pending["capture"]["frames"], json!([]));
        assert_eq!(host.captures.load(Ordering::SeqCst), 0);
        let captured = host
            .call(json!({"request":"capture_frame","time_ns":40_000_000}))
            .unwrap();
        assert_eq!(captured["ready"], true);
        assert_eq!(captured["capture"]["frames"][0]["width"], 2);
        assert_eq!(host.captures.load(Ordering::SeqCst), 1);
        host.capture_union.clear();
        host.capture.join().unwrap();
    }
    #[test]
    fn parsed_window_requests_are_lazy_and_use_shared_native_owner() {
        let mut host = Harness::new();
        let caps = host.call(json!({"request":"capabilities"})).unwrap();
        assert_eq!(caps["window_capture_backend"]["available"], true);
        assert_eq!(host.enumerations.load(Ordering::SeqCst), 0);
        assert_eq!(host.captures.load(Ordering::SeqCst), 0);
        let windows = host.call(json!({"request":"list_windows"})).unwrap();
        assert_eq!(windows["windows"][0]["name"], "Fixture window");
        assert_eq!(host.enumerations.load(Ordering::SeqCst), 1);
        let binding = skylive_core::WindowCaptureBinding::new(
            skylive_core::WindowTarget::Windows {
                title: "Fixture window".into(),
                class: "FixtureClass".into(),
                executable: "fixture.exe".into(),
            },
            skylive_core::WindowMatchPriority::Class,
            false,
            true,
            2,
            2,
        )
        .unwrap();
        host.call(json!({"request":"command","command":{"op":"add_window_capture_source","scene":1,"name":"Window","binding":binding}})).unwrap();
        assert_eq!(host.captures.load(Ordering::SeqCst), 0);
        host.call(json!({"request":"capture_frame","time_ns":0}))
            .unwrap();
        assert_eq!(host.captures.load(Ordering::SeqCst), 1);
        assert_eq!(host.capture.snapshot().frame_count(), 1);
        let before = host.session.snapshot();
        let invalid = serde_json::from_value::<Request>(
            json!({"request":"command","command":{"op":"set_window_capture_source","source":2,"binding":{"target":{"platform":"windows","title":"Fixture window","class":"FixtureClass","executable":"fixture.exe"},"priority":"class","capture_cursor":false,"client_area":true,"width":0,"height":2}}}),
        );
        assert!(invalid.is_err());
        assert!(Arc::ptr_eq(&before, &host.session.snapshot()));
        host.capture_union.clear();
        host.capture.join().unwrap();
    }
    #[test]
    fn parsed_capture_requests_use_injected_backend_and_shared_render_transform_persistence() {
        let temp = Temp::new();
        let ppm = temp.0.join("capture.ppm");
        let missing = temp.0.join("missing.ppm");
        let native = temp.0.join("capture.json");
        let mut host = Harness::new();
        host.call(json!({"request":"capabilities"})).unwrap();
        host.call(json!({"request":"snapshot"})).unwrap();
        assert_eq!(host.enumerations.load(Ordering::SeqCst), 0);
        assert_eq!(host.captures.load(Ordering::SeqCst), 0);
        let displays = host.call(json!({"request":"list_displays"})).unwrap();
        assert_eq!(displays["displays"][0]["name"], "Fixture screen");
        assert_eq!(host.enumerations.load(Ordering::SeqCst), 1);
        let binding = DisplayCaptureBinding::new(
            DisplayTarget::Windows {
                device_id: "fixture-screen".into(),
            },
            true,
            2,
            2,
        )
        .unwrap();
        host.call(json!({"request":"command","command":{"op":"set_video","width":16,"height":16,"fps_num":30,"fps_den":1}})).unwrap();
        host.call(json!({"request":"command","command":{"op":"add_display_capture_source","scene":1,"name":"Screen","binding":binding}})).unwrap();
        let before = host.session.snapshot();
        let failure=host.call(json!({"request":"command","command":{"op":"transform_item","scene":1,"item":3,"action":"fit_to_canvas"}})).unwrap_err();
        assert_eq!(failure.code, "capture_unavailable");
        assert!(Arc::ptr_eq(&before, &host.session.snapshot()));
        assert_eq!(host.captures.load(Ordering::SeqCst), 0);
        let acquired = host
            .call(json!({"request":"capture_frame","time_ns":0}))
            .unwrap();
        assert_eq!(acquired["requested"], true);
        assert_eq!(host.captures.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&before, &host.session.snapshot()));
        host.call(json!({"request":"command","command":{"op":"transform_item","scene":1,"item":3,"action":"fit_to_canvas"}})).unwrap();
        host.call(json!({"request":"render","path":ppm,"width":4,"height":4}))
            .unwrap();
        let mut expected = b"P6\n4 4\n255\n".to_vec();
        for y in 0..4 {
            for x in 0..4 {
                expected.extend_from_slice(match (x < 2, y < 2) {
                    (true, true) => &[255, 0, 0],
                    (false, true) => &[0, 255, 0],
                    (true, false) => &[0, 0, 255],
                    (false, false) => &[255, 255, 255],
                });
            }
        }
        assert_eq!(fs::read(&ppm).unwrap(), expected);
        assert_eq!(
            host.call(json!({"request":"capture_frame","time_ns":0}))
                .unwrap()["requested"],
            false
        );
        assert_eq!(host.captures.load(Ordering::SeqCst), 1);
        let before = host.session.snapshot();
        let next = host
            .call(json!({"request":"capture_frame","time_ns":33_333_334}))
            .unwrap();
        assert_eq!(next["requested"], true);
        assert_eq!(host.captures.load(Ordering::SeqCst), 2);
        assert!(Arc::ptr_eq(&before, &host.session.snapshot()));
        let previous_epoch = host.capture.snapshot().epoch();
        host.call(json!({"request":"advance","time_ns":0})).unwrap();
        assert!(host.capture.snapshot().epoch() > previous_epoch);
        assert_eq!(host.capture.snapshot().frame_count(), 0);
        assert_eq!(host.captures.load(Ordering::SeqCst), 2);
        host.call(json!({"request":"capture_frame","time_ns":0}))
            .unwrap();
        assert_eq!(host.captures.load(Ordering::SeqCst), 3);
        host.width.store(4, Ordering::SeqCst);
        host.call(json!({"request":"capture_frame","time_ns":33_333_334}))
            .unwrap();
        let projected = host
            .call(json!({"request":"get_display_transform","scene":1,"item":3}))
            .unwrap();
        assert_eq!(projected["transform"]["width"], 32.0);
        assert_eq!(projected["frame_dimensions"], json!([4, 2]));
        let before = host.session.snapshot();
        let mut desired = projected["transform"].clone();
        host.call(json!({"request":"set_display_transform","scene":1,"item":3,"transform":desired,"expected_frame_dimensions":[4,2]})).unwrap();
        assert!(Arc::ptr_eq(&before, &host.session.snapshot()));
        desired["width"] = 16.into();
        assert_eq!(host.call(json!({"request":"set_display_transform","scene":1,"item":3,"transform":desired,"expected_frame_dimensions":[2,2]})).unwrap_err().code,"capture_invalid_frame");
        assert!(Arc::ptr_eq(&before, &host.session.snapshot()));
        host.call(json!({"request":"set_display_transform","scene":1,"item":3,"transform":desired,"expected_frame_dimensions":[4,2]})).unwrap();
        assert_eq!(
            host.session.snapshot().scene(Id::new(1)).unwrap().items()[0]
                .transform()
                .width,
            8.0
        );
        host.call(json!({"request":"command","command":{"op":"undo"}}))
            .unwrap();
        assert_eq!(
            host.session.snapshot().scene(Id::new(1)).unwrap().items()[0]
                .transform()
                .width,
            16.0
        );
        host.call(json!({"request":"save","path":native})).unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&native).unwrap()).unwrap();
        assert!(saved["collection"].get("capture").is_none());
        host.call(json!({"request":"load","path":native})).unwrap();
        assert_eq!(host.capture.snapshot().frame_count(), 0);
        assert_eq!(host.captures.load(Ordering::SeqCst), 4);
        assert_eq!(
            host.call(json!({"request":"render_program","path":missing,"width":4,"height":4}))
                .unwrap_err()
                .code,
            "capture_resources_unavailable"
        );
        assert!(!missing.exists());
        host.capture.join().unwrap();
    }
}
