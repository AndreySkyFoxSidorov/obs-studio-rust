//! Reviewed compiled Rust providers; downloaded repositories are references only.
use crate::Error;
use serde::{Deserialize, Serialize};

mod plugin_catalog {
    include!("plugin_catalog.rs");
}
pub const MAX_PLUGINS: usize = 256;
pub const MAX_AUDIO_FILTERS: usize = 8;
pub const MAX_SCENE_NOTES_BYTES: usize = 16_384;
pub const CRAFT_AUDIO_PLUGIN: &str = "skylive/craft-audio-effects";
pub const AUDIO_PAN_PLUGIN: &str = "norihiro/obs-audio-pan-filter";
pub const SCENE_NOTES_PLUGIN: &str = "exeldro/obs-scene-notes-dock";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginOrigin {
    BuiltIn,
    Bundled,
    ThirdParty,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct PluginDescriptor {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub description_key: &'static str,
    pub source_url: &'static str,
    pub origin: PluginOrigin,
    pub implemented: bool,
    pub port_scope: &'static str,
}
pub fn plugin_catalog() -> &'static [PluginDescriptor] {
    plugin_catalog::CATALOG
}
pub fn plugin_descriptor(id: &str) -> Option<&'static PluginDescriptor> {
    plugin_catalog().iter().find(|provider| provider.id == id)
}
pub(crate) fn validate_enabled_plugins(enabled: &[String]) -> Result<(), Error> {
    if enabled.len() > MAX_PLUGINS {
        return Err(Error::Limit("enabled plugin limit exceeded"));
    }
    let mut seen = std::collections::HashSet::new();
    for id in enabled {
        let provider = plugin_descriptor(id).ok_or(Error::Invalid("unknown plugin ID"))?;
        if !provider.implemented || provider.origin != PluginOrigin::ThirdParty {
            return Err(Error::Invalid("plugin has no activatable Rust provider"));
        }
        if !seen.insert(id) {
            return Err(Error::Invalid("duplicate enabled plugin ID"));
        }
    }
    Ok(())
}
/// Opt-outs are preferences, not execution claims: retain a known bundled ID
/// even when its provider is unavailable in the current build.
pub(crate) fn validate_disabled_bundled_plugins(disabled: &[String]) -> Result<(), Error> {
    if disabled.len() > MAX_PLUGINS {
        return Err(Error::Limit("disabled bundled plugin limit exceeded"));
    }
    let mut seen = std::collections::HashSet::new();
    for id in disabled {
        let provider = plugin_descriptor(id).ok_or(Error::Invalid("unknown bundled plugin ID"))?;
        if provider.origin != PluginOrigin::Bundled {
            return Err(Error::Invalid(
                "disabled bundled list requires bundled provider IDs",
            ));
        }
        if !seen.insert(id) {
            return Err(Error::Invalid("duplicate disabled bundled plugin ID"));
        }
    }
    Ok(())
}
pub(crate) fn provider_enabled(
    provider: &PluginDescriptor,
    enabled: &[String],
    disabled: &[String],
) -> bool {
    provider.implemented
        && match provider.origin {
            PluginOrigin::BuiltIn => true,
            PluginOrigin::Bundled => !disabled.iter().any(|id| id == provider.id),
            PluginOrigin::ThirdParty => enabled.iter().any(|id| id == provider.id),
        }
}

/// Detector for the bundled FilmCraft-derived sampled-peak/RMS compressor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressorDetector {
    Peak,
    Rms,
}
/// OBS Expander detector; distinct implementation from the Craft detector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObsExpanderDetector {
    Rms,
    Peak,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObsExpanderPreset {
    #[default]
    Expander,
    Gate,
}
/// Authored UI/execution-order chain settings (top to bottom). Runtime DSP state belongs to the worker.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "AudioFilterData", tag = "kind", rename_all = "snake_case")]
pub enum AudioFilter {
    ObsNoiseSuppression { enabled: bool },
    CraftDither{bits:u8,noise_shaping:bool,enabled:bool},
    CraftRectifier{full_wave:bool,mix_pct:f32,output_db:f32,enabled:bool},
    CraftDcRemoval{cutoff_hz:f32,enabled:bool},
    CraftLoFi{bits:f32,sample_rate:f32,noise_pct:f32,mix_pct:f32,enabled:bool},
    CraftSaturator{drive_db:f32,bias_pct:f32,tone_hz:f32,mix_pct:f32,output_db:f32,enabled:bool},
    CraftModDelay{time_ms:f32,feedback_pct:f32,mix_pct:f32,mod_rate_hz:f32,mod_depth_pct:f32,lowpass_hz:f32,enabled:bool},
    CraftFdnReverb{predelay_ms:f32,decay_seconds:f32,damping_pct:f32,size_pct:f32,mix_pct:f32,enabled:bool},
    ObsThreeBandEq { low_db:f32,mid_db:f32,high_db:f32,enabled:bool },
    ObsUpwardCompressor {
        threshold_db:f32,ratio:f32,attack_ms:u32,release_ms:u32,output_db:f32,knee_db:u32,
        detector:ObsExpanderDetector,enabled:bool,
    },
    ObsNoiseGate {
        open_threshold_db:f32, close_threshold_db:f32,
        attack_ms:u32, hold_ms:u32, release_ms:u32, enabled:bool,
    },
    ObsLimiter {
        threshold_db: f32,
        release_ms: u32,
        enabled: bool,
    },
    ObsCompressor {
        threshold_db: f32,
        ratio: f32,
        attack_ms: u32,
        release_ms: u32,
        output_db: f32,
        #[serde(skip_serializing_if = "Option::is_none")]
        sidechain: Option<crate::Id>,
        enabled: bool,
    },
    ObsExpander {
        threshold_db: f32,
        ratio: f32,
        attack_ms: u32,
        release_ms: u32,
        output_db: f32,
        detector: ObsExpanderDetector,
        preset: ObsExpanderPreset,
        enabled: bool,
    },

    ObsGain {
        db: f32,
        enabled: bool,
    },
    ObsPolarityInvert {
        enabled: bool,
    },
    Gate {
        threshold_db: f32,
        ratio: f32,
        range_db: f32,
        attack_ms: f32,
        hold_ms: f32,
        release_ms: f32,
        enabled: bool,
    },
    SimpleEq {
        low_db: f32,
        low_freq_hz: f32,
        mid_db: f32,
        mid_freq_hz: f32,
        mid_q: f32,
        high_db: f32,
        high_freq_hz: f32,
        output_db: f32,
        enabled: bool,
    },
    Pan {
        pan_left: f32,
        pan_right: f32,
        gain_left_db: f32,
        gain_right_db: f32,
        invert_left: bool,
        invert_right: bool,
    },
    Compressor {
        threshold_db: f32,
        ratio: f32,
        knee_db: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
        detector: CompressorDetector,
        enabled: bool,
    },
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AudioFilterData {
    ObsNoiseSuppression { #[serde(default="enabled_filter")] enabled: bool },
    CraftDither{bits:u8,noise_shaping:bool,#[serde(default="enabled_filter")]enabled:bool},
    CraftRectifier{full_wave:bool,mix_pct:f32,output_db:f32,#[serde(default="enabled_filter")]enabled:bool},
    CraftDcRemoval{cutoff_hz:f32,#[serde(default="enabled_filter")]enabled:bool},
    CraftLoFi{bits:f32,sample_rate:f32,noise_pct:f32,mix_pct:f32,#[serde(default="enabled_filter")]enabled:bool},
    CraftSaturator{drive_db:f32,bias_pct:f32,tone_hz:f32,mix_pct:f32,output_db:f32,#[serde(default="enabled_filter")]enabled:bool},
    CraftModDelay{time_ms:f32,feedback_pct:f32,mix_pct:f32,mod_rate_hz:f32,mod_depth_pct:f32,lowpass_hz:f32,#[serde(default="enabled_filter")]enabled:bool},
    CraftFdnReverb{predelay_ms:f32,decay_seconds:f32,damping_pct:f32,size_pct:f32,mix_pct:f32,#[serde(default="enabled_filter")]enabled:bool},
    ObsThreeBandEq {
        #[serde(default)] low_db:f32,
        #[serde(default)] mid_db:f32,
        #[serde(default)] high_db:f32,
        #[serde(default = "enabled_filter")] enabled:bool,
    },
    ObsUpwardCompressor {
        threshold_db:f32,ratio:f32,attack_ms:u32,release_ms:u32,output_db:f32,knee_db:u32,
        detector:ObsExpanderDetector,
        #[serde(default = "enabled_filter")]
        enabled:bool,
    },
    ObsNoiseGate {
        open_threshold_db:f32, close_threshold_db:f32,
        attack_ms:u32, hold_ms:u32, release_ms:u32,
        #[serde(default = "enabled_filter")]
        enabled:bool,
    },
    ObsLimiter {
        threshold_db: f32,
        release_ms: u32,
        #[serde(default = "enabled_filter")]
        enabled: bool,
    },
    ObsCompressor {
        threshold_db: f32,
        ratio: f32,
        attack_ms: u32,
        release_ms: u32,
        output_db: f32,
        #[serde(default)]
        sidechain: Option<crate::Id>,
        #[serde(default = "enabled_filter")]
        enabled: bool,
    },
    ObsExpander {
        threshold_db: f32,
        ratio: f32,
        attack_ms: u32,
        release_ms: u32,
        output_db: f32,
        detector: ObsExpanderDetector,
        #[serde(default)]
        preset: ObsExpanderPreset,
        #[serde(default = "enabled_filter")]
        enabled: bool,
    },

    ObsGain {
        db: f32,
        #[serde(default = "enabled_filter")]
        enabled: bool,
    },
    ObsPolarityInvert {
        #[serde(default = "enabled_filter")]
        enabled: bool,
    },
    Gate {
        threshold_db: f32,
        ratio: f32,
        range_db: f32,
        attack_ms: f32,
        hold_ms: f32,
        release_ms: f32,
        #[serde(default = "enabled_filter")]
        enabled: bool,
    },
    SimpleEq {
        low_db: f32,
        low_freq_hz: f32,
        mid_db: f32,
        mid_freq_hz: f32,
        mid_q: f32,
        high_db: f32,
        high_freq_hz: f32,
        output_db: f32,
        #[serde(default = "enabled_filter")]
        enabled: bool,
    },
    Pan {
        pan_left: f32,
        pan_right: f32,
        gain_left_db: f32,
        gain_right_db: f32,
        invert_left: bool,
        invert_right: bool,
    },
    Compressor {
        threshold_db: f32,
        ratio: f32,
        knee_db: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
        detector: CompressorDetector,
        #[serde(default = "enabled_filter")]
        enabled: bool,
    },
}
fn enabled_filter() -> bool {
    true
}
impl TryFrom<AudioFilterData> for AudioFilter {
    type Error = Error;
    fn try_from(value: AudioFilterData) -> Result<Self, Error> {
        let filter = match value {
            AudioFilterData::ObsNoiseSuppression {enabled} => Self::ObsNoiseSuppression {enabled},
            AudioFilterData::CraftModDelay{time_ms,feedback_pct,mix_pct,mod_rate_hz,mod_depth_pct,lowpass_hz,enabled}
                =>Self::CraftModDelay{time_ms,feedback_pct,mix_pct,mod_rate_hz,mod_depth_pct,lowpass_hz,enabled},
            AudioFilterData::CraftDither{bits,noise_shaping,enabled}=>Self::CraftDither{bits,noise_shaping,enabled},
            AudioFilterData::CraftRectifier{full_wave,mix_pct,output_db,enabled}=>Self::CraftRectifier{full_wave,mix_pct,output_db,enabled},
            AudioFilterData::CraftDcRemoval{cutoff_hz,enabled}=>Self::CraftDcRemoval{cutoff_hz,enabled},
            AudioFilterData::CraftLoFi{bits,sample_rate,noise_pct,mix_pct,enabled}=>Self::CraftLoFi{bits,sample_rate,noise_pct,mix_pct,enabled},
            AudioFilterData::CraftSaturator{drive_db,bias_pct,tone_hz,mix_pct,output_db,enabled}
                =>Self::CraftSaturator{drive_db,bias_pct,tone_hz,mix_pct,output_db,enabled},
            AudioFilterData::CraftFdnReverb{predelay_ms,decay_seconds,damping_pct,size_pct,mix_pct,enabled}
                =>Self::CraftFdnReverb{predelay_ms,decay_seconds,damping_pct,size_pct,mix_pct,enabled},

            AudioFilterData::ObsThreeBandEq {low_db,mid_db,high_db,enabled}=>Self::ObsThreeBandEq {low_db,mid_db,high_db,enabled},
            AudioFilterData::ObsUpwardCompressor {threshold_db,ratio,attack_ms,release_ms,output_db,knee_db,detector,enabled}
                =>Self::ObsUpwardCompressor {threshold_db,ratio,attack_ms,release_ms,output_db,knee_db,detector,enabled},
            AudioFilterData::ObsNoiseGate {open_threshold_db,close_threshold_db,attack_ms,hold_ms,release_ms,enabled}
                =>Self::ObsNoiseGate {open_threshold_db,close_threshold_db,attack_ms,hold_ms,release_ms,enabled},
            AudioFilterData::ObsLimiter {
                threshold_db,
                release_ms,
                enabled,
            } => Self::ObsLimiter {
                threshold_db,
                release_ms,
                enabled,
            },
            AudioFilterData::ObsCompressor {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                output_db,
                sidechain,
                enabled,
            } => Self::ObsCompressor {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                output_db,
                sidechain,
                enabled,
            },
            AudioFilterData::ObsExpander {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                output_db,
                detector,
                preset,
                enabled,
            } => Self::ObsExpander {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                output_db,
                detector,
                preset,
                enabled,
            },

            AudioFilterData::ObsGain { db, enabled } => Self::ObsGain { db, enabled },
            AudioFilterData::ObsPolarityInvert { enabled } => Self::ObsPolarityInvert { enabled },
            AudioFilterData::Gate {
                threshold_db,
                ratio,
                range_db,
                attack_ms,
                hold_ms,
                release_ms,
                enabled,
            } => Self::Gate {
                threshold_db,
                ratio,
                range_db,
                attack_ms,
                hold_ms,
                release_ms,
                enabled,
            },
            AudioFilterData::SimpleEq {
                low_db,
                low_freq_hz,
                mid_db,
                mid_freq_hz,
                mid_q,
                high_db,
                high_freq_hz,
                output_db,
                enabled,
            } => Self::SimpleEq {
                low_db,
                low_freq_hz,
                mid_db,
                mid_freq_hz,
                mid_q,
                high_db,
                high_freq_hz,
                output_db,
                enabled,
            },
            AudioFilterData::Pan {
                pan_left,
                pan_right,
                gain_left_db,
                gain_right_db,
                invert_left,
                invert_right,
            } => Self::Pan {
                pan_left,
                pan_right,
                gain_left_db,
                gain_right_db,
                invert_left,
                invert_right,
            },
            AudioFilterData::Compressor {
                threshold_db,
                ratio,
                knee_db,
                attack_ms,
                release_ms,
                makeup_db,
                detector,
                enabled,
            } => Self::Compressor {
                threshold_db,
                ratio,
                knee_db,
                attack_ms,
                release_ms,
                makeup_db,
                detector,
                enabled,
            },
        };
        filter.validate()?;
        Ok(filter)
    }
}
impl Default for AudioFilter {
    fn default() -> Self {
        Self::Pan {
            pan_left: -100.0,
            pan_right: 100.0,
            gain_left_db: 0.0,
            gain_right_db: 0.0,
            invert_left: false,
            invert_right: false,
        }
    }
}
impl AudioFilter {
    /// Reset an owned settings draft, retaining the filter's bypass state.
    pub fn reset_settings(&mut self){
        let enabled=self.enabled_mut().map(|value|*value);
        let mut defaults=match *self{
            Self::ObsNoiseSuppression{..}=>Self::obs_noise_suppression(),
            Self::CraftDither{..}=>Self::craft_dither(),
            Self::CraftRectifier{..}=>Self::craft_rectifier(),
            Self::CraftDcRemoval{..}=>Self::craft_dc_removal(),
            Self::CraftLoFi{..}=>Self::craft_lofi(),
            Self::CraftSaturator{..}=>Self::craft_saturator(),
            Self::CraftModDelay{..}=>Self::craft_mod_delay(),Self::CraftFdnReverb{..}=>Self::craft_fdn_reverb(),
            Self::ObsThreeBandEq{..}=>Self::obs_three_band_eq(),
            Self::ObsUpwardCompressor{..}=>Self::obs_upward_compressor(),
            Self::ObsNoiseGate{..}=>Self::obs_noise_gate(),Self::ObsLimiter{..}=>Self::obs_limiter(),
            Self::ObsCompressor{..}=>Self::obs_compressor(),Self::ObsExpander{..}=>Self::obs_expander(),
            Self::ObsGain{..}=>Self::obs_gain(),Self::ObsPolarityInvert{..}=>Self::obs_polarity_invert(),
            Self::Gate{..}=>Self::gate(),Self::SimpleEq{..}=>Self::simple_eq(),
            Self::Compressor{..}=>Self::compressor(),Self::Pan{..}=>Self::default(),
        };
        if let(Some(value),Some(target))=(enabled,defaults.enabled_mut()){*target=value;}
        *self=defaults;
    }
    fn enabled_mut(&mut self)->Option<&mut bool>{match self{
        Self::ObsNoiseSuppression{enabled}=>Some(enabled),
        Self::CraftModDelay{enabled,..}|Self::CraftFdnReverb{enabled,..}|Self::CraftSaturator{enabled,..}|Self::CraftLoFi{enabled,..}|Self::CraftDcRemoval{enabled,..}|Self::CraftRectifier{enabled,..}|Self::CraftDither{enabled,..}=>Some(enabled),
        Self::ObsThreeBandEq{enabled,..}|Self::ObsUpwardCompressor{enabled,..}|
        Self::ObsNoiseGate{enabled,..}|Self::ObsLimiter{enabled,..}|Self::ObsCompressor{enabled,..}|
        Self::ObsExpander{enabled,..}|Self::ObsGain{enabled,..}|Self::ObsPolarityInvert{enabled,..}|
        Self::Gate{enabled,..}|Self::SimpleEq{enabled,..}|Self::Compressor{enabled,..}=>Some(enabled),
        Self::Pan{..}=>None,
    }}
    pub fn obs_noise_suppression()->Self{Self::ObsNoiseSuppression{enabled:true}}
    pub fn craft_dither()->Self{Self::CraftDither{bits:16,noise_shaping:false,enabled:true}}
    pub fn craft_rectifier()->Self{Self::CraftRectifier{full_wave:true,mix_pct:100.0,output_db:0.0,enabled:true}}
    pub fn craft_dc_removal()->Self{Self::CraftDcRemoval{cutoff_hz:5.0,enabled:true}}
    pub fn craft_lofi()->Self{Self::CraftLoFi{bits:8.,sample_rate:11025.,noise_pct:0.,mix_pct:100.,enabled:true}}
    pub fn craft_saturator()->Self{Self::CraftSaturator{drive_db:6.0,bias_pct:0.0,tone_hz:20000.0,mix_pct:100.0,output_db:0.0,enabled:true}}
    pub fn craft_mod_delay()->Self{Self::CraftModDelay{time_ms:250.,feedback_pct:30.,mix_pct:30.,mod_rate_hz:0.5,mod_depth_pct:0.,lowpass_hz:12000.,enabled:true}}
    pub fn craft_fdn_reverb()->Self{Self::CraftFdnReverb{predelay_ms:20.,decay_seconds:2.,damping_pct:40.,size_pct:50.,mix_pct:25.,enabled:true}}
    pub fn obs_three_band_eq()->Self{Self::ObsThreeBandEq {low_db:0.0,mid_db:0.0,high_db:0.0,enabled:true}}
    pub fn obs_upward_compressor()->Self{
        Self::ObsUpwardCompressor {threshold_db:-20.,ratio:0.5,attack_ms:10,release_ms:50,output_db:0.,knee_db:10,detector:ObsExpanderDetector::Rms,enabled:true}
    }
    pub fn obs_noise_gate()->Self{
        Self::ObsNoiseGate {open_threshold_db:-26.,close_threshold_db:-32.,attack_ms:25,hold_ms:200,release_ms:150,enabled:true}
    }
    pub fn obs_limiter() -> Self {
        Self::ObsLimiter {
            threshold_db: -6.,
            release_ms: 60,
            enabled: true,
        }
    }
    pub fn obs_compressor() -> Self {
        Self::ObsCompressor {
            threshold_db: -18.,
            ratio: 10.,
            attack_ms: 6,
            release_ms: 60,
            output_db: 0.,
            sidechain: None,
            enabled: true,
        }
    }
    pub fn obs_expander() -> Self {
        Self::obs_expander_preset(ObsExpanderPreset::Expander)
    }
    pub fn obs_expander_preset(preset: ObsExpanderPreset) -> Self {
        Self::ObsExpander {
            threshold_db: -40.,
            ratio: if preset == ObsExpanderPreset::Gate {
                10.
            } else {
                2.
            },
            attack_ms: 10,
            release_ms: if preset == ObsExpanderPreset::Gate {
                125
            } else {
                50
            },
            output_db: 0.,
            detector: ObsExpanderDetector::Rms,
            preset,
            enabled: true,
        }
    }

    pub fn obs_gain() -> Self {
        Self::ObsGain {
            db: 0.0,
            enabled: true,
        }
    }
    pub fn obs_polarity_invert() -> Self {
        Self::ObsPolarityInvert { enabled: true }
    }
    pub fn gate() -> Self {
        Self::Gate {
            threshold_db: -40.0,
            ratio: 10.0,
            range_db: 60.0,
            attack_ms: 1.0,
            hold_ms: 50.0,
            release_ms: 150.0,
            enabled: true,
        }
    }
    pub fn simple_eq() -> Self {
        Self::SimpleEq {
            low_db: 0.0,
            low_freq_hz: 200.0,
            mid_db: 0.0,
            mid_freq_hz: 1000.0,
            mid_q: 0.7,
            high_db: 0.0,
            high_freq_hz: 5000.0,
            output_db: 0.0,
            enabled: true,
        }
    }
    pub fn compressor() -> Self {
        Self::Compressor {
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 6.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 0.0,
            detector: CompressorDetector::Peak,
            enabled: true,
        }
    }
    pub fn plugin_id(&self) -> &'static str {
        match self {
            Self::Pan { .. } => AUDIO_PAN_PLUGIN,
            Self::ObsNoiseSuppression { .. }
            | Self::ObsThreeBandEq { .. }
            | Self::ObsGain { .. }
            | Self::ObsPolarityInvert { .. }
            | Self::ObsLimiter { .. }
            | Self::ObsCompressor { .. }
            | Self::ObsNoiseGate { .. }
            | Self::ObsUpwardCompressor { .. }
            | Self::ObsExpander { .. } => crate::OBS_VIDEO_FILTERS_PLUGIN,
            Self::CraftDither{..}|Self::CraftRectifier{..}|Self::CraftDcRemoval{..}|Self::CraftLoFi{..}|Self::CraftModDelay{..}|Self::CraftFdnReverb{..}|Self::CraftSaturator{..}|Self::Compressor { .. } | Self::Gate { .. } | Self::SimpleEq { .. } => {
                CRAFT_AUDIO_PLUGIN
            }
        }
    }
    pub fn sidechain_source(self)->Option<crate::Id>{
        if let Self::ObsCompressor {sidechain,..}=self{sidechain}else{None}
    }
    pub fn validate(&self) -> Result<(), Error> {
        match *self {
            Self::ObsNoiseSuppression {..} => {},
            Self::CraftDither{bits,..}=>{if !matches!(bits,16|20|24){return Err(Error::Invalid("Craft Dither bit depth must be 16, 20 or 24"));}},
            Self::CraftRectifier{mix_pct,output_db,..}=>{if !mix_pct.is_finite() || !(0.0..=100.0).contains(&mix_pct) || !output_db.is_finite() || !(-24.0..=12.0).contains(&output_db){return Err(Error::Invalid("Craft Rectifier parameters outside SoundCraft ranges"));}},
            Self::CraftDcRemoval{cutoff_hz,..}=>{if !cutoff_hz.is_finite() || !(1.0..=40.0).contains(&cutoff_hz){return Err(Error::Invalid("Craft DC Removal cutoff must be 1..40 Hz"));}},
            Self::CraftLoFi{bits,sample_rate,noise_pct,mix_pct,..}=>{
                for (value,min,max) in [(bits,1.,24.),(sample_rate,500.,48000.),(noise_pct,0.,100.),(mix_pct,0.,100.)] {if !value.is_finite() || !(min..=max).contains(&value) {return Err(Error::Invalid("Craft Lo-Fi parameters are outside SoundCraft ranges"));}}
            },
            Self::CraftSaturator{drive_db,bias_pct,tone_hz,mix_pct,output_db,..}=>{
                for (value,min,max) in [(drive_db,0.,36.),(bias_pct,0.,100.),(tone_hz,1000.,20000.),(mix_pct,0.,100.),(output_db,-24.,12.)] {
                    if !value.is_finite() || !(min..=max).contains(&value) {return Err(Error::Invalid("Craft Saturator parameters are outside SoundCraft ranges"));}
                }
            },
            Self::CraftModDelay{time_ms,feedback_pct,mix_pct,mod_rate_hz,mod_depth_pct,lowpass_hz,..}=>{
                for(value,min,max)in[(time_ms,1.,2000.),(feedback_pct,-99.,99.),(mix_pct,0.,100.),(mod_rate_hz,0.01,10.),(mod_depth_pct,0.,100.),(lowpass_hz,500.,20000.)]{
                    if !value.is_finite()||!(min..=max).contains(&value){return Err(Error::Invalid("Craft Mod Delay parameters are outside their SoundCraft ranges"));}
                }
            },
            Self::CraftFdnReverb{predelay_ms,decay_seconds,damping_pct,size_pct,mix_pct,..}=>{
                for(value,min,max)in[(predelay_ms,0.,200.),(decay_seconds,0.1,20.),(damping_pct,0.,100.),(size_pct,0.,100.),(mix_pct,0.,100.)]{
                    if !value.is_finite()||!(min..=max).contains(&value){return Err(Error::Invalid("Craft FDN Reverb parameters are outside their FilmCraft ranges"));}
                }
            },
            Self::ObsThreeBandEq {low_db,mid_db,high_db,..}=>{if ![low_db,mid_db,high_db].iter().all(|value|value.is_finite()&&(-20.0..=20.0).contains(value)){return Err(Error::Invalid("OBS 3-Band Equalizer requires finite gains in -20..20 dB"));}}
            Self::ObsUpwardCompressor {threshold_db,ratio,attack_ms,release_ms,output_db,knee_db,..}=>{
                if !threshold_db.is_finite()||!(-60.0..=0.0).contains(&threshold_db)
                    ||!ratio.is_finite()||!(0.0..=1.0).contains(&ratio)
                    ||!output_db.is_finite()||!(-32.0..=32.0).contains(&output_db)
                    ||!(1..=100).contains(&attack_ms)||!(1..=1000).contains(&release_ms)||knee_db>20 {
                    return Err(Error::Invalid("OBS Upward Compressor parameter outside finite source range"));
                }
            }
            Self::ObsNoiseGate {open_threshold_db,close_threshold_db,attack_ms,hold_ms,release_ms,..}=>{
                if ![open_threshold_db,close_threshold_db].iter().all(|v|v.is_finite()&&(-96.0..=0.0).contains(v))
                    ||attack_ms>10000||hold_ms>10000||release_ms>10000 {
                    return Err(Error::Invalid("OBS Noise Gate requires thresholds -96..0 dB and integer times 0..10000 ms"));
                }
            }
            Self::ObsLimiter {
                threshold_db,
                release_ms,
                ..
            } => {
                if !threshold_db.is_finite()
                    || !(-60.0..=0.0).contains(&threshold_db)
                    || !(1..=1000).contains(&release_ms)
                {
                    return Err(Error::Invalid(
                        "OBS Limiter requires threshold -60..0 dB and integer release 1..1000 ms",
                    ));
                }
            }
            Self::ObsCompressor {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                output_db,
                ..
            } => {
                for (value, min, max) in [
                    (threshold_db, -60., 0.),
                    (ratio, 1., 32.),
                    (output_db, -32., 32.),
                ] {
                    if !value.is_finite() || !(min..=max).contains(&value) {
                        return Err(Error::Invalid(
                            "OBS Compressor parameter outside finite source range",
                        ));
                    }
                }
                if !(1..=500).contains(&attack_ms) || !(1..=1000).contains(&release_ms) {
                    return Err(Error::Invalid(
                        "OBS Compressor integer attack/release outside source range",
                    ));
                }
            }
            Self::ObsExpander {
                threshold_db,
                ratio,
                attack_ms,
                release_ms,
                output_db,
                ..
            } => {
                for (value, min, max) in [
                    (threshold_db, -60., 0.),
                    (ratio, 1., 20.),
                    (output_db, -32., 32.),
                ] {
                    if !value.is_finite() || !(min..=max).contains(&value) {
                        return Err(Error::Invalid(
                            "OBS Expander parameter outside finite source range",
                        ));
                    }
                }
                if !(1..=100).contains(&attack_ms) || !(1..=1000).contains(&release_ms) {
                    return Err(Error::Invalid(
                        "OBS Expander integer attack/release outside source range",
                    ));
                }
            }

            Self::ObsGain { db, .. } => {
                if !db.is_finite() || !(-30.0..=30.0).contains(&db) {
                    return Err(Error::Invalid("OBS Gain requires finite -30..=30 dB"));
                }
            }
            Self::ObsPolarityInvert { .. } => {}
            Self::Gate {
                threshold_db,
                ratio,
                range_db,
                attack_ms,
                hold_ms,
                release_ms,
                ..
            } => {
                for (value, min, max) in [
                    (threshold_db, -90.0, 0.0),
                    (ratio, 1.0, 100.0),
                    (range_db, 0.0, 90.0),
                    (attack_ms, 0.01, 100.0),
                    (hold_ms, 0.0, 2000.0),
                    (release_ms, 1.0, 3000.0),
                ] {
                    if !value.is_finite() || !(min..=max).contains(&value) {
                        return Err(Error::Invalid(
                            "gate requires finite parameters inside the supported dB, ratio and time ranges",
                        ));
                    }
                }
            }
            Self::SimpleEq {
                low_db,
                low_freq_hz,
                mid_db,
                mid_freq_hz,
                mid_q,
                high_db,
                high_freq_hz,
                output_db,
                ..
            } => {
                for (value, min, max) in [
                    (low_db, -24.0, 24.0),
                    (low_freq_hz, 20.0, 1000.0),
                    (mid_db, -24.0, 24.0),
                    (mid_freq_hz, 100.0, 10000.0),
                    (mid_q, 0.1, 10.0),
                    (high_db, -24.0, 24.0),
                    (high_freq_hz, 1000.0, 20000.0),
                    (output_db, -24.0, 24.0),
                ] {
                    if !value.is_finite() || !(min..=max).contains(&value) {
                        return Err(Error::Invalid(
                            "simple EQ requires finite parameters inside the supported gain, frequency and Q ranges",
                        ));
                    }
                }
            }
            Self::Pan {
                pan_left,
                pan_right,
                gain_left_db,
                gain_right_db,
                ..
            } => {
                if ![pan_left, pan_right, gain_left_db, gain_right_db]
                    .iter()
                    .all(|value| value.is_finite())
                    || !(-100.0..=100.0).contains(&pan_left)
                    || !(-100.0..=100.0).contains(&pan_right)
                    || !(-80.0..=30.0).contains(&gain_left_db)
                    || !(-80.0..=30.0).contains(&gain_right_db)
                {
                    return Err(Error::Invalid(
                        "audio pan requires -100..100 percent and -80..30 dB finite values",
                    ));
                }
            }
            Self::Compressor {
                threshold_db,
                ratio,
                knee_db,
                attack_ms,
                release_ms,
                makeup_db,
                ..
            } => {
                for (value, min, max) in [
                    (threshold_db, -60.0, 0.0),
                    (ratio, 1.0, 30.0),
                    (knee_db, 0.0, 24.0),
                    (attack_ms, 0.01, 300.0),
                    (release_ms, 1.0, 3000.0),
                    (makeup_db, -40.0, 40.0),
                ] {
                    if !value.is_finite() || !(min..=max).contains(&value) {
                        return Err(Error::Invalid(
                            "compressor requires finite parameters inside the supported dB, ratio and time ranges",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod obs_audio_filter_tests {
    use super::*;
    #[test]
    fn builtin_audio_defaults_bounds_and_atomic_history() {
        use crate::{AudioCaptureBinding, AudioFlow, AudioTarget, Command, Session};
        for filter in [AudioFilter::obs_gain(), AudioFilter::obs_polarity_invert()] {
            filter.validate().unwrap();
            assert_eq!(filter.plugin_id(), crate::OBS_VIDEO_FILTERS_PLUGIN);
        }
        let mut session = Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: "Input".into(),
                binding: AudioCaptureBinding::new(
                    AudioFlow::Input,
                    AudioTarget::Default,
                    false,
                    false,
                )
                .unwrap(),
            })
            .unwrap()
            .created
            .unwrap();
        let source = session
            .snapshot()
            .scene(scene)
            .unwrap()
            .items()
            .iter()
            .find(|entry| entry.id() == item)
            .unwrap()
            .source();
        let initial = session.snapshot();
        for db in [f32::NAN, f32::INFINITY, -30.01, 30.01] {
            assert!(
                session
                    .apply(Command::SetSourceAudioFilters {
                        source,
                        filters: vec![AudioFilter::ObsGain { db, enabled: true }]
                    })
                    .is_err()
            );
            assert_eq!(session.snapshot().revision(), initial.revision());
            assert!(
                session
                    .snapshot()
                    .source(source)
                    .unwrap()
                    .audio_filters()
                    .is_empty()
            );
        }
        let chain = vec![
            AudioFilter::ObsGain {
                db: 20.0,
                enabled: true,
            },
            AudioFilter::obs_polarity_invert(),
        ];
        session
            .apply(Command::SetSourceAudioFilters {
                source,
                filters: chain.clone(),
            })
            .unwrap();
        let revision = session.snapshot().revision();
        session
            .apply(Command::SetSourceAudioFilters {
                source,
                filters: chain.clone(),
            })
            .unwrap();
        assert_eq!(session.snapshot().revision(), revision);
        session.apply(Command::Undo).unwrap();
        assert!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .audio_filters()
                .is_empty()
        );
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session.snapshot().source(source).unwrap().audio_filters(),
            chain
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gate_and_eq_use_bundled_provider_and_reject_each_invalid_scalar() {
        for filter in [AudioFilter::gate(), AudioFilter::simple_eq()] {
            assert!(filter.validate().is_ok());
            assert_eq!(filter.plugin_id(), CRAFT_AUDIO_PLUGIN);
        }
        for (index, min, max) in [
            (0, -90.0, 0.0),
            (1, 1.0, 100.0),
            (2, 0.0, 90.0),
            (3, 0.01, 100.0),
            (4, 0.0, 2000.0),
            (5, 1.0, 3000.0),
        ] {
            for value in [f32::NAN, f32::INFINITY, min - 0.01, max + 0.01] {
                let mut filter = AudioFilter::gate();
                if let AudioFilter::Gate {
                    threshold_db,
                    ratio,
                    range_db,
                    attack_ms,
                    hold_ms,
                    release_ms,
                    ..
                } = &mut filter
                {
                    *[
                        threshold_db,
                        ratio,
                        range_db,
                        attack_ms,
                        hold_ms,
                        release_ms,
                    ][index] = value;
                }
                assert!(filter.validate().is_err());
            }
        }
        for (index, min, max) in [
            (0, -24.0, 24.0),
            (1, 20.0, 1000.0),
            (2, -24.0, 24.0),
            (3, 100.0, 10000.0),
            (4, 0.1, 10.0),
            (5, -24.0, 24.0),
            (6, 1000.0, 20000.0),
            (7, -24.0, 24.0),
        ] {
            for value in [f32::NAN, f32::NEG_INFINITY, min - 0.01, max + 0.01] {
                let mut filter = AudioFilter::simple_eq();
                if let AudioFilter::SimpleEq {
                    low_db,
                    low_freq_hz,
                    mid_db,
                    mid_freq_hz,
                    mid_q,
                    high_db,
                    high_freq_hz,
                    output_db,
                    ..
                } = &mut filter
                {
                    *[
                        low_db,
                        low_freq_hz,
                        mid_db,
                        mid_freq_hz,
                        mid_q,
                        high_db,
                        high_freq_hz,
                        output_db,
                    ][index] = value;
                }
                assert!(filter.validate().is_err());
            }
        }
    }
    #[test]
    fn bundled_policy_is_default_on_only_when_available_and_never_uses_third_party_allowlist() {
        let mut bundled = *plugin_descriptor(CRAFT_AUDIO_PLUGIN).unwrap();
        assert_eq!(bundled.origin, PluginOrigin::Bundled);
        bundled.implemented = true;
        assert!(provider_enabled(&bundled, &[], &[]));
        assert!(!provider_enabled(&bundled, &[], &[bundled.id.into()]));
        assert!(provider_enabled(&bundled, &[bundled.id.into()], &[]));
        bundled.implemented = false;
        assert!(!provider_enabled(&bundled, &[], &[]));
        let third = plugin_descriptor(AUDIO_PAN_PLUGIN).unwrap();
        assert!(!provider_enabled(third, &[], &[]));
        assert!(provider_enabled(third, &[AUDIO_PAN_PLUGIN.into()], &[]));
        assert!(validate_enabled_plugins(&[CRAFT_AUDIO_PLUGIN.into()]).is_err());
        assert!(validate_disabled_bundled_plugins(&[CRAFT_AUDIO_PLUGIN.into()]).is_ok());
        assert!(validate_disabled_bundled_plugins(&[AUDIO_PAN_PLUGIN.into()]).is_err());
        assert!(
            validate_disabled_bundled_plugins(&[
                CRAFT_AUDIO_PLUGIN.into(),
                CRAFT_AUDIO_PLUGIN.into()
            ])
            .is_err()
        );
        assert!(
            validate_disabled_bundled_plugins(&vec![CRAFT_AUDIO_PLUGIN.into(); MAX_PLUGINS + 1])
                .is_err()
        );
    }
    #[test]
    fn compressor_settings_are_typed_finite_and_do_not_change_pan_defaults() {
        assert!(AudioFilter::compressor().validate().is_ok());
        assert_eq!(AudioFilter::compressor().plugin_id(), CRAFT_AUDIO_PLUGIN);
        assert_eq!(AudioFilter::default().plugin_id(), AUDIO_PAN_PLUGIN);
        for value in [f32::NAN, f32::INFINITY, -60.01, 0.01] {
            let mut filter = AudioFilter::compressor();
            if let AudioFilter::Compressor { threshold_db, .. } = &mut filter {
                *threshold_db = value;
            }
            assert!(filter.validate().is_err());
        }
        for value in [0.0, 30.1, f32::NEG_INFINITY] {
            let mut filter = AudioFilter::compressor();
            if let AudioFilter::Compressor { ratio, .. } = &mut filter {
                *ratio = value;
            }
            assert!(filter.validate().is_err());
        }
    }
    #[test]
    fn catalog_has_exact_unique_backlog_and_never_activates_unported_providers() {
        assert_eq!(plugin_catalog().len(), 145);
        assert!(plugin_catalog().len() <= MAX_PLUGINS);
        let ids = plugin_catalog()
            .iter()
            .map(|p| p.id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 145);
        assert_eq!(
            plugin_catalog()
                .iter()
                .filter(|provider| provider.origin != PluginOrigin::Bundled)
                .count(),
            143
        );
        assert_eq!(
            plugin_catalog()
                .iter()
                .filter(|provider| provider.origin == PluginOrigin::Bundled)
                .count(),
            2
        );
        assert_eq!(
            plugin_catalog()
                .iter()
                .filter(|p| p.origin == PluginOrigin::ThirdParty)
                .count(),
            127
        );
        assert!(validate_enabled_plugins(&[]).is_ok());
        for p in plugin_catalog() {
            assert!(!p.port_scope.is_empty());
            assert_eq!(
                validate_enabled_plugins(&[p.id.into()]).is_ok(),
                p.implemented && p.origin == PluginOrigin::ThirdParty
            );
        }
        assert!(
            validate_enabled_plugins(&[AUDIO_PAN_PLUGIN.into(), AUDIO_PAN_PLUGIN.into()]).is_err()
        );
        assert!(validate_enabled_plugins(&["missing/provider".into()]).is_err());
    }
    #[test]
    fn pan_validates_source_units_and_nonfinite_values() {
        assert!(AudioFilter::default().validate().is_ok());
        for value in [f32::NAN, f32::INFINITY, -100.1, 100.1] {
            let filter = AudioFilter::Pan {
                pan_left: value,
                pan_right: 100.0,
                gain_left_db: 0.0,
                gain_right_db: 0.0,
                invert_left: false,
                invert_right: false,
            };
            assert!(filter.validate().is_err());
        }
    }
}

#[cfg(test)]
mod obs_dynamics_tests {
    include!("obs_dynamics_core_tests.rs");
}
