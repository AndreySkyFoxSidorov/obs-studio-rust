//! Deterministic Program routing and bounded CPU PCM evaluation.
//! No devices, callbacks, presentation state or implicit clocks are involved.
//! Contract: doc/migration/audio-routing.md.

use crate::transition::{CompositionKey, FrameComposition, FrameScene, OutputKey, TransitionError};
use serde::Serialize;
use skylive_core::{
    AudioCaptureBinding, AudioFilter, AudioSettings, Id,
    MAX_AUDIO_FILTERS as MAX_SOURCE_AUDIO_FILTERS, Session, Source, SourceKind, StudioSnapshot,
    TransitionKind,
};
use skylive_media::{
    FadeToColorSettings, MediaError, StereoPan, StereoPanSettings, directional_audio_gains,
    fade_audio_gains, fade_to_color_audio_gains,
};
use std::fmt;

pub(crate) mod sidechain;
/// Source-owned project-rate RNNoise adapter; authored filter wiring is separate.
pub mod noise_suppression;
pub(crate) use sidechain::view_keys as sidechain_view_keys;
mod dsp;
mod craft_ring_mod;
mod craft_maximizer;
mod craft_pitch;
pub(crate) mod delayed_source;
pub(crate) use dsp::{craft_filter_bytes,MAX_CRAFT_RETAINED};
mod mix;
pub(crate) mod true_peak;
pub use dsp::{
    AudioDspState, MAX_DSP_STATE_BYTES, compressor_parameters, gate_parameters,
    obs_compressor_parameters, obs_expander_parameters, obs_limiter_parameters, obs_noise_gate_parameters, obs_upward_parameters,
    simple_eq_parameters,obs_three_band_eq_parameters,
};
mod timeline;
pub(crate) use timeline::MonitorVideoStamp;
pub use timeline::{AudioRouteSnapshot, AudioRouteSpan, AudioRouteTimeline, RequiredAudioBinding};
#[cfg(test)]
mod obs_filter_tests;
#[cfg(test)]
mod plugin_tests;
#[cfg(test)]
mod source_properties_tests;
#[cfg(test)]
mod tests;

pub const MAX_AUDIO_INPUTS: usize = 64;
/// Retained source identities across historical route spans of one block.
/// Each individual plan remains limited to MAX_AUDIO_INPUTS.
pub const MAX_AUDIO_BLOCK_SOURCES: usize = 128;
pub const MAX_AUDIO_CHANNELS: usize = 8;
pub const MAX_AUDIO_FRAMES: usize = 4096;
pub const MAX_AUDIO_MIX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioError {
    Closed,
    InvalidTransition,
    InvalidRequest(&'static str),
    Limit(&'static str),
    TimeOverflow,
    MissingScene(Id),
    MissingSource(Id),
    InconsistentSource(Id),
    MissingInput(Id),
    UnexpectedInput(Id),
    DuplicateInput(Id),
    FormatMismatch(Id),
    InvalidBuffer(Id),
    NonFiniteSample { source: Id, index: usize },
    Transition(TransitionError),
    Media(MediaError),
    NoiseSuppression(noise_suppression::Error),
    Compressor(skylive_media::craft_audio::CompressorError),
    Gate(skylive_media::craft_audio::GateError),
    Eq(skylive_media::craft_audio::EqError),
    ObsThreeBandEq(skylive_media::obs_three_band_eq::ObsThreeBandEqError),
    ObsDynamics(skylive_media::obs_dynamics::ObsDynamicsError),
    CraftTime(skylive_media::craft_time::CraftTimeError),
}
impl AudioError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Closed => "audio_closed",
            Self::InvalidTransition => "audio_invalid_transition",
            Self::InvalidRequest(_) => "audio_invalid_request",
            Self::Limit(_) => "audio_limit",
            Self::TimeOverflow => "audio_time_overflow",
            Self::MissingScene(_) => "audio_scene_not_found",
            Self::MissingSource(_) => "audio_source_not_found",
            Self::InconsistentSource(_) => "audio_source_conflict",
            Self::MissingInput(_) => "audio_missing_pcm",
            Self::UnexpectedInput(_) => "audio_unexpected_pcm",
            Self::DuplicateInput(_) => "audio_duplicate_pcm",
            Self::FormatMismatch(_) => "audio_format_mismatch",
            Self::InvalidBuffer(_) => "audio_invalid_buffer",
            Self::NonFiniteSample { .. } => "audio_nonfinite_sample",
            Self::Transition(error) => error.code(),
            Self::Media(_) => "audio_media",
            Self::NoiseSuppression(_) => "audio_rnnoise",
            Self::Compressor(_) => "audio_compressor",
            Self::Gate(_) => "audio_gate",
            Self::Eq(_) => "audio_eq",
            Self::ObsThreeBandEq(_)=>"audio_obs_three_band_eq",
            Self::CraftTime(_)=>"craft_audio_effect",
            Self::ObsDynamics(_) => "audio_obs_dynamics",
        }
    }
}
impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(detail) | Self::Limit(detail) => {
                write!(f, "{}: {detail}", self.code())
            }
            Self::MissingScene(id)
            | Self::MissingSource(id)
            | Self::InconsistentSource(id)
            | Self::MissingInput(id)
            | Self::UnexpectedInput(id)
            | Self::DuplicateInput(id)
            | Self::FormatMismatch(id)
            | Self::InvalidBuffer(id) => write!(f, "{}: source {}", self.code(), id.get()),
            Self::NonFiniteSample { source, index } => {
                write!(f, "{}: source {} sample {index}", self.code(), source.get())
            }
            Self::Transition(error) => error.fmt(f),
            Self::Media(error) => error.fmt(f),
            Self::NoiseSuppression(error) => error.fmt(f),
            Self::Compressor(error) => error.fmt(f),
            Self::Gate(error) => error.fmt(f),
            Self::Eq(error) => error.fmt(f),
            Self::ObsThreeBandEq(error)=>error.fmt(f),
            Self::ObsDynamics(error) => error.fmt(f),
            Self::CraftTime(error)=>error.fmt(f),
            _ => f.write_str(self.code()),
        }
    }
}
impl std::error::Error for AudioError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct AudioMixRequest {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: usize,
    /// Offset from the composition's captured elapsed time, not wall time.
    pub advance_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioMixLimits {
    pub max_total_bytes: usize,
}
impl Default for AudioMixLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: MAX_AUDIO_MIX_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PcmInput<'a> {
    Samples {
        source: Id,
        sample_rate: u32,
        channels: u16,
        samples: &'a [f32],
    },
    /// PCM already converted/remixed to the requested format. Unknown original
    /// layout explicitly disables native-stereo-only balance.
    Converted {
        source: Id,
        sample_rate: u32,
        channels: u16,
        samples: &'a [f32],
        original: Option<skylive_media::AudioFormat>,
    },
    /// Explicit source silence in the requested mix format.
    Silence { source: Id },
}
impl<'a> PcmInput<'a> {
    pub fn source(&self) -> Id {
        match self {
            Self::Samples { source, .. }
            | Self::Converted { source, .. }
            | Self::Silence { source } => *source,
        }
    }
    fn buffer(&self) -> Option<(u32, u16, &'a [f32])> {
        match self {
            Self::Samples {
                sample_rate,
                channels,
                samples,
                ..
            }
            | Self::Converted {
                sample_rate,
                channels,
                samples,
                ..
            } => Some((*sample_rate, *channels, *samples)),
            Self::Silence { .. } => None,
        }
    }
    fn original(&self) -> Result<Option<skylive_media::AudioFormat>, MediaError> {
        match self {
            Self::Samples {
                sample_rate,
                channels,
                ..
            } => skylive_media::AudioFormat::new(*sample_rate, *channels, 0).map(Some),
            Self::Converted { original, .. } => Ok(*original),
            Self::Silence { .. } => Ok(None),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct ChannelMeter {
    pub peak: f64,
    pub rms: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SourceMeter {
    pub source: Id,
    pub channels: Vec<ChannelMeter>,
    /// Post-filter input sample peaks before the source fader and mute.
    pub input_peaks: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MixedAudio {
    pub session: u64,
    pub revision: u64,
    pub output_key: CompositionKey,
    pub request: AudioMixRequest,
    pub samples: Vec<f32>,
    #[serde(skip_serializing_if="Vec::is_empty")]
    pub monitor_samples:Vec<f32>,
    /// Post source gain/mute, before transition weighting.
    pub source_meters: Vec<SourceMeter>,
    pub program_meters: Vec<ChannelMeter>,
    pub clipped_samples: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AudioRoutes {
    pub global: bool,
    pub from: bool,
    pub to: bool,
    pub duplicated: bool,
}
impl AudioRoutes {
    fn weight(self, factors: (f32, f32)) -> f64 {
        // OBS promotes shared sources to a single unweighted audio root.
        if self.global || self.duplicated || (self.from && self.to) {
            1.0
        } else if self.from {
            f64::from(factors.0)
        } else if self.to {
            f64::from(factors.1)
        } else {
            0.0
        }
    }
}
/// Complete PCM resource identity. Native wire shape remains backward compatible.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum AudioInputKey {
    Native(AudioCaptureBinding),
    /// Window Capture owns this PCM separately from standalone application
    /// capture, even when their durable window selectors are identical.
    Window {source:Id, binding:AudioCaptureBinding},
    EmbeddedCamera {source:Id,binding:skylive_core::VideoCaptureBinding},
    Browser(crate::browser::BrowserAudioKey),
    Ndi {source:Id,ndi:skylive_core::NdiSource},
    Media {
        source: Id,
        media: skylive_core::MediaSource,
    },
}
impl PartialEq for AudioInputKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Native(a), Self::Native(b)) => a == b,
            (Self::Window {source:a,binding:x}, Self::Window {source:b,binding:y}) => a == b && x == y,
            (Self::EmbeddedCamera {source:a,binding:x},Self::EmbeddedCamera {source:b,binding:y})=>a==b&&x.same_video(y),
            (Self::Browser(a),Self::Browser(b))=>a==b,
            (Self::Ndi {source:a,ndi:x},Self::Ndi {source:b,ndi:y})=>a==b&&x==y,
            (
                Self::Media {
                    source: a,
                    media: x,
                },
                Self::Media {
                    source: b,
                    media: y,
                },
            ) => {
                a == b
                    && x.path() == y.path()
                    && x.video_dimensions() == y.video_dimensions()
                    && x.has_audio() == y.has_audio()
                    // Speed changes the decoder's output sample rate and
                    // timeline. A packet produced by the old decoder must
                    // not match the newly authored source binding.
                    && x.speed_percent() == y.speed_percent()
                    // Network property edits recreate the downloader and
                    // decoder even when their URL and format stay the same.
                    // Their old PCM must not match the replacement stream.
                    && (!crate::media_source::network_http::is_url(x.path()) || x == y)
            }
            _ => false,
        }
    }
}
impl Eq for AudioInputKey {}
impl std::hash::Hash for AudioInputKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::Native(binding) => {
                0u8.hash(state);
                binding.hash(state);
            }
            Self::Window {source,binding}=>{5u8.hash(state);source.hash(state);binding.hash(state);},
            Self::EmbeddedCamera {source,binding}=>{4u8.hash(state);source.hash(state);binding.video_only().hash(state);},
            Self::Ndi {source,ndi}=>{2u8.hash(state);source.hash(state);ndi.hash(state);},
            Self::Browser(key)=>{3u8.hash(state);key.hash(state);},
            Self::Media { source, media } => {
                1u8.hash(state);
                source.hash(state);
                media.path().hash(state);
                media.video_dimensions().hash(state);
                media.has_audio().hash(state);
                media.speed_percent().hash(state);
                if crate::media_source::network_http::is_url(media.path()) {
                    media.hash(state);
                }
            }
        }
    }
}
impl AudioInputKey {
    pub fn as_native(&self) -> Option<&AudioCaptureBinding> {
        if let Self::Native(binding) = self {
            Some(binding)
        } else {
            None
        }
    }
}
impl From<AudioCaptureBinding> for AudioInputKey {
    fn from(binding: AudioCaptureBinding) -> Self {
        Self::Native(binding)
    }
}
impl PartialEq<AudioCaptureBinding> for AudioInputKey {
    fn eq(&self, other: &AudioCaptureBinding) -> bool {
        self.as_native() == Some(other)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AudioSourcePlan {
    #[serde(skip_serializing_if="skylive_core::AudioMonitoringMode::is_off")]
    pub monitoring:skylive_core::AudioMonitoringMode,
    pub properties: skylive_core::SourceAudioProperties,
    pub source: Id,
    pub binding: AudioInputKey,
    pub gain: f32,
    pub muted: bool,
    /// Enabled providers only, retained in authored OBS filter-list order.
    /// Runtime evaluation reverses this list, matching filter_async_audio.
    pub filters: Vec<AudioFilter>,
    pub routes: AudioRoutes,
    /// Linear scene-bus gains for each composition side, before the
    /// transition curve. Global and duplicate-source promotion are handled
    /// independently by the route policy.
    pub scene_from_gain: f64,
    pub scene_to_gain: f64,
    pub initial_weight: f64,
}

/// One owned edge in the scene audio graph. Scene references preserve their
/// source identity separately from the referenced scene, so distinct source
/// instances can later carry independent processing without flattening them.
#[derive(Clone, Debug, Serialize)]
#[serde(tag="kind", rename_all="snake_case")]
pub enum AudioSceneInput {
    Source { item: Id, source: Id },
    Scene { item: Id, source: Id, scene: Id, gain: f64, muted: bool },
}

#[derive(Clone, Debug, Serialize)]
pub struct AudioSceneNode {
    pub scene: Id,
    pub inputs: Vec<AudioSceneInput>,
}

/// An owned snapshot of one side of the output composition. Nodes are
/// topologically ordered children first, with the root as the last node.
#[derive(Clone, Debug, Serialize)]
pub struct AudioSceneGraph {
    pub root: Id,
    pub root_gain: f64,
    pub root_muted: bool,
    pub nodes: Vec<AudioSceneNode>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum AudioCurve {
    Scene,
    Timed {
        kind: TransitionKind,
        elapsed_ns: u64,
        duration_ns: u64,
    },
    Manual {
        kind: TransitionKind,
        progress_num: u64,
        progress_den: u64,
    },
}
impl AudioCurve {
    fn factors(self, advance_ns: u64) -> Result<(f32, f32), AudioError> {
        match self {
            Self::Scene => Ok((1.0, 0.0)),
            Self::Timed {
                kind,
                elapsed_ns,
                duration_ns,
            } => {
                if duration_ns == 0 || elapsed_ns > duration_ns {
                    return Err(AudioError::InvalidTransition);
                }
                let elapsed = elapsed_ns
                    .checked_add(advance_ns)
                    .ok_or(AudioError::TimeOverflow)?
                    .min(duration_ns);
                factors(kind, elapsed, duration_ns)
            }
            Self::Manual {
                kind,
                progress_num,
                progress_den,
            } => factors(kind, progress_num, progress_den),
        }
    }
}
fn factors(
    kind: TransitionKind,
    numerator: u64,
    denominator: u64,
) -> Result<(f32, f32), AudioError> {
    if denominator == 0 || numerator > denominator {
        return Err(AudioError::InvalidTransition);
    }
    match kind {
        TransitionKind::Cut => return Err(AudioError::InvalidTransition),
        TransitionKind::Fade|TransitionKind::LumaWipe{..} => fade_audio_gains(numerator, denominator),
        TransitionKind::Swipe { .. } | TransitionKind::Slide { .. } => {
            directional_audio_gains(numerator, denominator)
        }
        TransitionKind::FadeToColor { rgb, switch_point } => {
            let settings =
                FadeToColorSettings::new(rgb, switch_point).map_err(AudioError::Media)?;
            fade_to_color_audio_gains(numerator, denominator, settings)
        }
    }
    .map_err(AudioError::Media)
}

#[derive(Clone, Debug, Serialize)]
pub struct AudioPlan {
    #[serde(skip_serializing_if="skylive_core::AudioMonitoringDevice::is_default")]
    monitor_device:skylive_core::AudioMonitoringDevice,
    session: u64,
    revision: u64,
    output_key: CompositionKey,
    stable_output_key: OutputKey,
    audio_settings: AudioSettings,
    #[serde(skip)]
    peak_mode: skylive_core::MixerPeakMode,
    curve: AudioCurve,
    #[serde(skip_serializing_if="Option::is_none")]
    scene_from: Option<AudioSceneGraph>,
    #[serde(skip_serializing_if="Option::is_none")]
    scene_to: Option<AudioSceneGraph>,
    sources: Vec<AudioSourcePlan>,
    #[serde(skip_serializing_if="Vec::is_empty")]
    auxiliary: Vec<AudioSourcePlan>,
}
/// Transient fader value for one live mixer gesture. This is never serialized
/// or written to the authored session; the caller commits once on release.
#[derive(Clone, Copy, Debug)]
pub struct MixerGainPreview {
    pub session: u64,
    pub revision: u64,
    pub source: Id,
    pub gain: f32,
}
impl AudioPlan {
    /// Retain real session/composition identities while projecting one source's
    /// processing contract. No output-time gain/mute/routing is baked into PCM.
    pub(crate) fn processing_source_plan(&self, id:Id) -> Result<Self,AudioError> {
        let source=self.processing_sources().find(|source|source.source==id).ok_or(AudioError::MissingInput(id))?;
        let mut source=source.clone();
        source.gain=1.0;source.muted=false;source.routes=AudioRoutes::default();source.monitoring=skylive_core::AudioMonitoringMode::Off;source.initial_weight=1.0;
        source.properties=skylive_core::SourceAudioProperties::new(source.properties.force_mono(),source.properties.balance())
            .map_err(|_|AudioError::InvalidRequest("invalid source channel processing config"))?;
        Ok(Self {monitor_device:skylive_core::AudioMonitoringDevice::default(),session:self.session,
            revision:self.revision,output_key:self.output_key,stable_output_key:self.stable_output_key,
            audio_settings:self.audio_settings,peak_mode:self.peak_mode,curve:self.curve,scene_from:None,scene_to:None,sources:vec![source],auxiliary:Vec::new()})
    }
    /// Already processed source PCM must never pass through Mono/Balance or
    /// stateful filters a second time. Output-time gain/mute/routes remain real.
    pub(crate) fn processed_output_plan(&self)->Self {
        let mut result=self.clone();
        for source in &mut result.sources {source.properties=skylive_core::SourceAudioProperties::default().with_audio_tracks(source.properties.audio_tracks());source.filters.clear();}
        result.auxiliary.clear();
        result
    }
    pub(crate) fn processing_sources(&self)->impl Iterator<Item=&AudioSourcePlan>{self.sources.iter().chain(&self.auxiliary)}
    pub(crate) fn detector_sources(&self)->&[AudioSourcePlan]{&self.auxiliary}
    pub(crate) fn craft_work_per_frame(&self)->usize{
        self.processing_sources().flat_map(|source|&source.filters).map(|filter|match filter{
            AudioFilter::CraftTimeShift{enabled:true,..}=>usize::from(self.audio_settings.channels())*12,
            AudioFilter::CraftModDelay{enabled:true,..}=>usize::from(self.audio_settings.channels())*72+8,
            AudioFilter::CraftFdnReverb{enabled:true,..}=>256+usize::from(self.audio_settings.channels())*16,
            AudioFilter::CraftSoundReverb{enabled:true,..}=>512+usize::from(self.audio_settings.channels())*16,
            AudioFilter::CraftPitch{enabled:true,..}=>usize::from(self.audio_settings.channels())*32+48,AudioFilter::CraftMaximizer{enabled:true,..}=>usize::from(self.audio_settings.channels())*8+600,AudioFilter::CraftDeEsser{enabled:true,..}=>usize::from(self.audio_settings.channels())*30+40,AudioFilter::CraftRingMod{enabled:true,..}=>usize::from(self.audio_settings.channels())*32+48,AudioFilter::CraftSaturator{enabled:true,..}=>usize::from(self.audio_settings.channels())*30+12,AudioFilter::CraftLoFi{enabled:true,..}=>usize::from(self.audio_settings.channels())*12+8,AudioFilter::CraftDither{enabled:true,..}=>usize::from(self.audio_settings.channels())*12,AudioFilter::CraftEq1{enabled:true,..}=>usize::from(self.audio_settings.channels())*12,AudioFilter::CraftRectifier{enabled:true,..}=>usize::from(self.audio_settings.channels())*8,AudioFilter::CraftDcRemoval{enabled:true,..}=>usize::from(self.audio_settings.channels())*5,_=>0,
        }).sum()
    }
    pub(crate) fn craft_state_bytes(&self)->Result<usize,AudioError>{
        self.processing_sources().try_fold(0usize,|bytes,source|bytes.checked_add(dsp::craft_filter_bytes(&source.filters,self.audio_settings.sample_rate(),self.audio_settings.channels())?)
            .ok_or(AudioError::Limit("Craft source state byte overflow")))
    }
    pub(crate) fn processing_state_bytes(&self)->Result<usize,AudioError>{dsp::validate_plan_state(self)}
    pub(crate) fn monitor_only_plan(&self) -> Self {
        let mut result = self.clone();
        result.sources.retain(|source| !source.monitoring.is_off());
        result
    }
    pub fn monitoring_device(&self)->&skylive_core::AudioMonitoringDevice {&self.monitor_device}
    pub fn has_monitoring(&self)->bool {self.sources.iter().any(|s|!s.monitoring.is_off())}
    pub(crate) fn peak_mode(&self)->skylive_core::MixerPeakMode {self.peak_mode}
    pub fn session(&self) -> u64 {
        self.session
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn output_key(&self) -> CompositionKey {
        self.output_key
    }
    pub fn sources(&self) -> &[AudioSourcePlan] {
        &self.sources
    }
    pub fn stable_output_key(&self) -> OutputKey {
        self.stable_output_key
    }
    pub fn audio_settings(&self) -> AudioSettings {
        self.audio_settings
    }
    fn same_routing_as(&self, other: &Self) -> bool {
        self.monitor_device==other.monitor_device
            && self.peak_mode==other.peak_mode
            && self.stable_output_key == other.stable_output_key
            && self.audio_settings == other.audio_settings
            && self.auxiliary.len()==other.auxiliary.len()
            && self.auxiliary.iter().zip(&other.auxiliary).all(|(a,b)|a.source==b.source&&a.binding==b.binding&&a.filters==b.filters&&a.properties==b.properties&&a.muted==b.muted)
            && self.sources.len() == other.sources.len()
            && self.sources.iter().zip(&other.sources).all(|(a, b)| {
                a.source == b.source
                    && a.monitoring == b.monitoring
                    && a.binding == b.binding
                    && a.gain == b.gain
                    && a.muted == b.muted
                    && a.filters == b.filters
                    && a.properties == b.properties
                    && a.routes == b.routes
                    // Scene/group bus filters and faders are folded into
                    // these per-side coefficients. They must publish a new
                    // timeline event even when the leaf source is unchanged.
                    && a.scene_from_gain.to_bits() == b.scene_from_gain.to_bits()
                    && a.scene_to_gain.to_bits() == b.scene_to_gain.to_bits()
            })
            && match (self.curve, other.curve) {
                (
                    AudioCurve::Manual {
                        progress_num: a,
                        progress_den: ad,
                        ..
                    },
                    AudioCurve::Manual {
                        progress_num: b,
                        progress_den: bd,
                        ..
                    },
                ) => u128::from(a) * u128::from(bd) == u128::from(b) * u128::from(ad),
                (AudioCurve::Manual { .. }, _) | (_, AudioCurve::Manual { .. }) => false,
                _ => true,
            }
    }
}

impl AudioSourcePlan {
    fn output_weight(&self, factors:(f32,f32))->f64 {
        if self.routes.global || self.routes.duplicated || (self.routes.from && self.routes.to) {
            1.0
        } else if self.routes.from {
            f64::from(factors.0)*self.scene_from_gain
        } else if self.routes.to {
            f64::from(factors.1)*self.scene_to_gain
        } else { 0.0 }
    }
}

/// Derive only Program-visible audio and explicit global roots. The result is
/// an owned offline plan, not a promise that a later Session still has this revision.
pub fn derive_audio_plan(
    session: &Session,
    output: &FrameComposition,
) -> Result<AudioPlan, AudioError> {
    if session.is_closed() {
        return Err(AudioError::Closed);
    }
    let (curve, from, to) = match output {
        FrameComposition::Scene(scene) | FrameComposition::SceneAlias { scene, .. } => {
            (AudioCurve::Scene, Some(scene), None)
        }
        FrameComposition::Black { .. } => (AudioCurve::Scene, None, None),
        FrameComposition::Fade {
            from,
            to,
            elapsed_ns,
            duration_ns,
            ..
        } => (
            AudioCurve::Timed {
                kind: TransitionKind::Fade,
                elapsed_ns: *elapsed_ns,
                duration_ns: *duration_ns,
            },
            Some(from),
            Some(to),
        ),
        FrameComposition::SingleFade {
            scene,
            incoming,
            elapsed_ns,
            duration_ns,
            ..
        } => (
            AudioCurve::Timed {
                kind: TransitionKind::Fade,
                elapsed_ns: *elapsed_ns,
                duration_ns: *duration_ns,
            },
            (!*incoming).then_some(scene),
            (*incoming).then_some(scene),
        ),
        FrameComposition::Configured {
            from,
            to,
            kind,
            elapsed_ns,
            duration_ns,
            ..
        } => {
            if !matches!(kind, TransitionKind::FadeToColor { .. }|TransitionKind::LumaWipe{..}) {
                return Err(AudioError::InvalidTransition);
            }
            (
                AudioCurve::Timed {
                    kind: *kind,
                    elapsed_ns: *elapsed_ns,
                    duration_ns: *duration_ns,
                },
                from.as_ref(),
                to.as_ref(),
            )
        }
        FrameComposition::Directional {
            from,
            to,
            kind,
            elapsed_ns,
            duration_ns,
            ..
        } => {
            if !matches!(
                kind,
                TransitionKind::Swipe { .. } | TransitionKind::Slide { .. }
            ) {
                return Err(AudioError::InvalidTransition);
            }
            (
                AudioCurve::Timed {
                    kind: *kind,
                    elapsed_ns: *elapsed_ns,
                    duration_ns: *duration_ns,
                },
                from.as_ref(),
                to.as_ref(),
            )
        }
        FrameComposition::Manual {
            from,
            to,
            kind,
            progress_num,
            progress_den,
            next_after_ns,
            ..
        } => {
            if *next_after_ns == Some(0) {
                return Err(AudioError::InvalidTransition);
            }
            (
                AudioCurve::Manual {
                    kind: *kind,
                    progress_num: *progress_num,
                    progress_den: *progress_den,
                },
                from.as_ref(),
                to.as_ref(),
            )
        }
    };
    // Reject malformed public composition scalars before gathering graph data.
    let initial_factors = curve.factors(0)?;
    let current = session.snapshot();
    if current.scene(output.primary_scene()).is_none()
        && !matches!(output, FrameComposition::Scene(FrameScene::Frozen(_)))
    {
        return Err(AudioError::MissingScene(output.primary_scene()));
    }
    let mut sources = Vec::new();
    sources
        .try_reserve_exact(MAX_AUDIO_INPUTS)
        .map_err(|_| AudioError::Limit("audio plan allocation"))?;
    let scene_from = from.map(|view|gather_view(session.identity(), &current, view, false, &mut sources)).transpose()?;
    let scene_to = to.map(|view|gather_view(session.identity(), &current, view, true, &mut sources)).transpose()?;
    for root in current.global_audio() {
        let source = current
            .source(root.source())
            .ok_or(AudioError::MissingSource(root.source()))?;
        insert_source(
            source,
            &current,
            AudioRoutes {
                global: true,
                ..Default::default()
            },
            &mut sources,
        )?;
    }
    // Preview-only monitored sources have zero Program routes. They share the
    // existing capture/converter/DSP owner instead of opening a second input.
    for source in FrameScene::Live(session.preview()).visible_sources(&current).map_err(AudioError::Transition)? {
        if source.kind().has_audio()&&!matches!(source.kind(),SourceKind::VideoCapture{..})&&!source.audio_monitoring().is_off()&&!sources.iter().any(|s|s.source==source.id()) {
            insert_source(source,&current,AudioRoutes::default(),&mut sources)?;
        }
    }
    let mut auxiliary=Vec::new();
    // Explicit authored key graph only. These roots have no Program, fader,
    // monitoring or transition route and cannot make themselves audible.
    let mut keys:Vec<Id>=sources.iter().flat_map(|source|source.filters.iter())
        .filter(|filter|matches!(filter,AudioFilter::ObsCompressor{enabled:true,..})).filter_map(|filter|filter.sidechain_source()).collect();
    let mut visited=std::collections::HashSet::new();
    while let Some(key)=keys.pop(){
        if !visited.insert(key)||sources.iter().any(|source|source.source==key){continue;}
        let Some(source)=current.source(key).filter(|source|source.kind().has_audio())else{continue;};
        if sources.len()+auxiliary.len()>=MAX_AUDIO_BLOCK_SOURCES{return Err(AudioError::Limit("audio processing roots exceed128 sources"));}
        insert_source_bounded(source,&current,AudioRoutes::default(),&mut auxiliary,MAX_AUDIO_BLOCK_SOURCES)?;
        if let Some(source)=auxiliary.iter_mut().find(|source|source.source==key){
            source.monitoring=skylive_core::AudioMonitoringMode::Off;
            keys.extend(source.filters.iter().filter(|filter|matches!(filter,AudioFilter::ObsCompressor{enabled:true,..})).filter_map(|filter|filter.sidechain_source()));
        }
    }
    auxiliary.sort_by_key(|source|source.source.get());
    sources.sort_by_key(|source| source.source.get());
    for source in &mut sources {
        // libobs promotes duplicated individual sources to root_nodes and
        // skips their contribution inside scenes and transitions. Global and
        // cross-transition sources use that same independent root route, so
        // scene-bus gain does not apply to them.
        source.initial_weight = source.output_weight(initial_factors);
    }
    Ok(AudioPlan {
        monitor_device:current.preferences().audio_monitoring_device().clone(),
        peak_mode:current.preferences().mixer_display().peak_mode(),
        session: session.identity(),
        revision: current.revision(),
        output_key: output.sample_key(),
        stable_output_key: output.identity(),
        audio_settings: current.audio(),
        curve,
        scene_from,
        scene_to,
        sources,
        auxiliary,
    })
}
fn gather_view(
    session: u64,
    collection: &StudioSnapshot,
    view: &FrameScene,
    to: bool,
    output: &mut Vec<AudioSourcePlan>,
) -> Result<AudioSceneGraph, AudioError> {
    view.validate_session(session)
        .map_err(AudioError::Transition)?;
    let visible = view
        .visible_sources(collection)
        .map_err(AudioError::Transition)?;
    let nested: std::collections::HashMap<_, _> = visible
        .iter()
        .filter_map(|source| {
            source
                .kind()
                .nested_scene()
                .map(|scene| (source.id(), scene))
        })
        .collect();
    let audio_ids: std::collections::HashSet<_> = visible.iter()
        // Disabled NDI sources are excluded from the actual audio plan by
        // insert_source_bounded. Exclude them before enforcing the same limit
        // and counting paths, too, so disabled modules cannot reject an
        // otherwise valid scene merely by being present in it.
        .filter(|source| source.kind().has_audio()
            && (!matches!(source.kind(), SourceKind::NdiSource { .. })
                || collection.preferences().plugin_enabled(skylive_core::NDI_PLUGIN)))
        .map(|source| source.id())
        .collect();
    if audio_ids.len() > MAX_AUDIO_INPUTS {
        return Err(AudioError::Limit("audio plan exceeds64 sources"));
    }
    // Count all audible leaves in one bounded bottom-up traversal. Shared
    // child scenes are evaluated once, but each incoming edge contributes to
    // the count; this preserves the existing OBS duplicate-source promotion.
    let root_source=view.root_scene_source(collection);
    let root_gain = root_source.map(|source|scene_bus_gain(source,collection)).transpose()?.unwrap_or(1.0);
    let mut graph = AudioSceneGraph {root:view.scene(),root_gain,
        root_muted:root_source.is_some_and(|source|source.muted()),nodes:Vec::new()};
    let mut seen = std::collections::HashSet::new();
    build_audio_scene_graph(view,collection,view.scene(),view.items(collection).map_err(AudioError::Transition)?,
        &nested,&audio_ids,&mut seen,&mut graph.nodes)?;
    let counts = scene_audio_counts(&graph)?;
    let gains = scene_audio_gains(&graph)?;
    for source in visible {
        if audio_ids.contains(&source.id()) {
            let duplicated = counts.get(&source.id()).copied().unwrap_or(0) > 1;
            insert_source(
                source,
                collection,
                AudioRoutes {
                    from: !to,
                    to,
                    duplicated,
                    ..Default::default()
                },
                output,
            )?;
            if let Some(entry)=output.iter_mut().find(|entry|entry.source==source.id()) {
                let gain=gains.get(&source.id()).copied().unwrap_or(1.0);
                if to {entry.scene_to_gain=gain;} else {entry.scene_from_gain=gain;}
            }
        }
    }
    Ok(graph)
}

/// Preserve the visible nested-scene edges and item identities; shared scenes
/// are stored once and referenced by every parent edge. The earlier
/// `visible_sources` validation enforces cycle, depth and graph limits.
fn build_audio_scene_graph(
    view: &FrameScene,
    current: &StudioSnapshot,
    scene: Id,
    items: &[skylive_core::SceneItem],
    nested: &std::collections::HashMap<Id, Id>,
    audio_ids: &std::collections::HashSet<Id>,
    seen: &mut std::collections::HashSet<Id>,
    nodes: &mut Vec<AudioSceneNode>,
) -> Result<(), AudioError> {
    if !seen.insert(scene) { return Ok(()); }
    let mut inputs=Vec::new();
    inputs.try_reserve(items.len()).map_err(|_|AudioError::Limit("audio scene graph allocation"))?;
    for item in items.iter().filter(|item| item.visible()) {
        if audio_ids.contains(&item.source()) {
            inputs.push(AudioSceneInput::Source {item:item.id(),source:item.source()});
        }
        if let Some(child) = nested.get(&item.source()) {
            build_audio_scene_graph(view,current,*child,view.scene_items(current,*child).map_err(AudioError::Transition)?,
                nested,audio_ids,seen,nodes)?;
            let submix=view.source(current,item.source()).ok_or(AudioError::MissingSource(item.source()))?;
            inputs.push(AudioSceneInput::Scene {item:item.id(),source:item.source(),scene:*child,gain:scene_bus_gain(submix,current)?,muted:submix.muted()});
        }
    }
    nodes.try_reserve(1).map_err(|_|AudioError::Limit("audio scene graph node allocation"))?;
    nodes.push(AudioSceneNode {scene,inputs});
    Ok(())
}

/// The supported scene filters are linear and stateless, so they commute with
/// mixing and can be applied once to each unshared leaf on that bus. OBS
/// promotes shared leaves separately, which continues to bypass this gain.
fn scene_bus_gain(source:&Source, snapshot:&StudioSnapshot)->Result<f64,AudioError> {
    let mut gain=f64::from(source.gain());
    for filter in source.audio_filters() {
        if !snapshot.preferences().plugin_enabled(filter.plugin_id()) {continue;}
        match *filter {
            AudioFilter::ObsGain {db,enabled:true}=>gain*=10.0_f64.powf(f64::from(db)/20.0),
            AudioFilter::ObsPolarityInvert {enabled:true}=>gain = -gain,
            AudioFilter::ObsGain {enabled:false,..} | AudioFilter::ObsPolarityInvert {enabled:false}=>{},
            _=>return Err(AudioError::InvalidRequest("stateful scene audio filters require submix histories")),
        }
    }
    if !gain.is_finite() {return Err(AudioError::InvalidRequest("scene audio gain overflow"));}
    Ok(gain)
}

/// Per-node leaf multiplicities retain OBS duplicate-source promotion while
/// evaluating each shared child once, directly from the owned scene graph.
fn scene_audio_counts(graph:&AudioSceneGraph)->Result<std::collections::HashMap<Id,u8>,AudioError> {
    let mut memo:std::collections::HashMap<Id,std::collections::HashMap<Id,u8>>=std::collections::HashMap::new();
    for node in &graph.nodes {
        let mut counts=std::collections::HashMap::new();
        for input in &node.inputs {
            match input {
                AudioSceneInput::Source {source,..}=>{
                    let entry=counts.entry(*source).or_insert(0u8);
                    *entry=entry.saturating_add(1).min(2);
                },
                AudioSceneInput::Scene {scene,..}=>{
                    let child=memo.get(scene).ok_or(AudioError::InvalidRequest("audio scene graph is not topologically ordered"))?;
                    for (&source,&multiplicity) in child {
                        let entry=counts.entry(source).or_insert(0u8);
                        *entry=entry.saturating_add(multiplicity).min(2);
                    }
                },
            }
        }
        memo.insert(node.scene,counts);
    }
    memo.remove(&graph.root).ok_or(AudioError::InvalidRequest("audio scene graph root missing"))
}

/// Linear gain is distributive over a sum, so an unshared leaf can be routed
/// through a scene bus without allocating another block. Duplicated individual
/// inputs are promoted by OBS to an independent root and skip all scene buses.
fn scene_audio_gains(graph:&AudioSceneGraph)->Result<std::collections::HashMap<Id,f64>,AudioError> {
    type Gains=std::collections::HashMap<Id,(u8,f64)>;
    let mut memo:std::collections::HashMap<Id,Gains>=std::collections::HashMap::new();
    for node in &graph.nodes {
        let mut gains=Gains::new();
        for input in &node.inputs {
            match *input {
                AudioSceneInput::Source {source,..}=>{
                    let entry=gains.entry(source).or_insert((0,1.0));
                    entry.0=entry.0.saturating_add(1).min(2);
                },
                AudioSceneInput::Scene {scene,gain,muted,..}=>{
                    let child=memo.get(&scene).ok_or(AudioError::InvalidRequest("audio scene graph is not topologically ordered"))?;
                    let bus=if muted {0.0}else{gain};
                    for (&source,&(count,child_gain)) in child {
                        let value=bus*child_gain;
                        if !value.is_finite(){return Err(AudioError::InvalidRequest("scene audio gain overflow"));}
                        let entry=gains.entry(source).or_insert((0,value));
                        entry.0=entry.0.saturating_add(count).min(2);
                    }
                },
            }
        }
        memo.insert(node.scene,gains);
    }
    let gains=memo.remove(&graph.root).ok_or(AudioError::InvalidRequest("audio scene graph root missing"))?;
    let root=if graph.root_muted {0.0}else{graph.root_gain};
    gains.into_iter().map(|(source,(count,gain))|{
        let gain=root*gain;
        if !gain.is_finite(){return Err(AudioError::InvalidRequest("scene audio gain overflow"));}
        Ok((source,if count>1 {1.0} else {gain}))
    }).collect()
}
fn insert_source(source:&Source,snapshot:&StudioSnapshot,routes:AudioRoutes,output:&mut Vec<AudioSourcePlan>)->Result<(),AudioError>{
    insert_source_bounded(source,snapshot,routes,output,MAX_AUDIO_INPUTS)
}
// One source-to-binding projection is shared by authored routes and temporary
// mixer previews so a changed or frozen input cannot receive another gain.
fn source_input_key(source:&Source)->Result<AudioInputKey,AudioError>{
    Ok(match source.kind() {
        SourceKind::AudioCapture { binding } => AudioInputKey::Native(binding.clone()),
        SourceKind::WindowCapture { binding } => AudioInputKey::Window {source:source.id(),binding:binding.audio().ok_or(AudioError::InconsistentSource(source.id()))?.clone()},
        SourceKind::VideoCapture {binding} if binding.embedded_audio() => AudioInputKey::EmbeddedCamera {source:source.id(),binding:binding.video_only()},
        SourceKind::VideoCapture {binding} => AudioInputKey::Native(binding.audio().ok_or(AudioError::InconsistentSource(source.id()))?.clone()),
        SourceKind::Browser {browser} if browser.control_audio()=>AudioInputKey::Browser(crate::browser::BrowserAudioKey {source:source.id(),browser:std::sync::Arc::new(browser.clone())}),
        SourceKind::NdiSource{ndi} if ndi.audio()=>AudioInputKey::Ndi {source:source.id(),ndi:ndi.clone()},
        SourceKind::MediaSource { media } if media.has_audio() => AudioInputKey::Media {source:source.id(),media:media.clone()},
        _ => return Err(AudioError::InconsistentSource(source.id())),
    })
}
fn insert_source_bounded(
    source: &Source,
    snapshot: &StudioSnapshot,
    routes: AudioRoutes,
    output: &mut Vec<AudioSourcePlan>,
    limit:usize,
) -> Result<(), AudioError> {
    if matches!(source.kind(),SourceKind::NdiSource{..})&&!snapshot.preferences().plugin_enabled(skylive_core::NDI_PLUGIN){return Ok(());}
    let binding = source_input_key(source)?;
    if !source.gain().is_finite() || source.gain() < 0.0 {
        return Err(AudioError::InconsistentSource(source.id()));
    }
    if source.audio_filters().len() > MAX_SOURCE_AUDIO_FILTERS {
        return Err(AudioError::Limit("source audio filter count exceeds 8"));
    }
    let filters: Vec<_> = source
        .audio_filters()
        .iter()
        .copied()
        .filter(|filter| snapshot.preferences().plugin_enabled(filter.plugin_id()))
        .collect::<Vec<_>>()
        .into_boxed_slice()
        .into_vec();
    if let Some(existing) = output.iter_mut().find(|entry| entry.source == source.id()) {
        if existing.binding != binding
            || existing.monitoring != source.audio_monitoring()
            || existing.properties != *source.audio_properties()
            || existing.gain != source.gain()
            || existing.muted != source.muted()
            || existing.filters != filters
        {
            return Err(AudioError::InconsistentSource(source.id()));
        }
        existing.routes.duplicated |= routes.duplicated
            || (existing.routes.from && routes.from)
            || (existing.routes.to && routes.to);
        existing.routes.global |= routes.global;
        existing.routes.from |= routes.from;
        existing.routes.to |= routes.to;
    } else {
        if output.len() >= limit {
            return Err(AudioError::Limit("audio source plan root ceiling exceeded"));
        }
        output.push(AudioSourcePlan {
            monitoring:source.audio_monitoring(),
            properties: *source.audio_properties(),
            source: source.id(),
            binding: binding.clone(),
            gain: source.gain(),
            muted: source.muted(),
            filters,
            routes,
            scene_from_gain: 1.0,
            scene_to_gain: 1.0,
            initial_weight: 0.0,
        });
    }
    Ok(())
}

fn pan_settings(filter: AudioFilter) -> Result<StereoPanSettings, AudioError> {
    match filter {
        AudioFilter::Pan {
            pan_left,
            pan_right,
            gain_left_db,
            gain_right_db,
            invert_left,
            invert_right,
        } => StereoPanSettings::new(
            pan_left,
            pan_right,
            gain_left_db,
            gain_right_db,
            invert_left,
            invert_right,
        )
        .map_err(AudioError::Media),
        AudioFilter::CraftSignalGenerator{..}|AudioFilter::CraftTimeShift{..}|AudioFilter::CraftPitch{..}|AudioFilter::CraftMaximizer{..}|AudioFilter::CraftDeEsser{..}|AudioFilter::ObsNoiseSuppression{..}|AudioFilter::CraftRingMod{..}|AudioFilter::CraftDither{..}|AudioFilter::CraftEq1{..}|AudioFilter::CraftRectifier{..}|AudioFilter::CraftDcRemoval{..}|AudioFilter::CraftLoFi{..}|AudioFilter::CraftSaturator{..}|AudioFilter::CraftModDelay{..}|AudioFilter::CraftFdnReverb{..}|AudioFilter::CraftSoundReverb{..}|AudioFilter::ObsThreeBandEq { .. }
        | AudioFilter::ObsLimiter { .. }
        | AudioFilter::ObsNoiseGate { .. }
        | AudioFilter::ObsUpwardCompressor { .. }
        | AudioFilter::ObsCompressor { .. }
        | AudioFilter::ObsExpander { .. }
        | AudioFilter::ObsGain { .. }
        | AudioFilter::ObsPolarityInvert { .. }
        | AudioFilter::Compressor { .. }
        | AudioFilter::Gate { .. }
        | AudioFilter::SimpleEq { .. } => {
            Err(AudioError::InvalidRequest("filter is not a pan filter"))
        }
    }
}

#[cfg(test)]
mod obs_dynamics_tests;

