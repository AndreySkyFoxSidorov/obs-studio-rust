//! Actual compiled operations. This registry advertises no downloaded binary ABI.
use crate::{
    AUDIO_PAN_PLUGIN, CRAFT_AUDIO_PLUGIN, CRAFT_VIDEO_PLUGIN, OBS_VIDEO_FILTERS_PLUGIN,
    PERSON_BACKGROUND_PLUGIN, SCENE_NOTES_PLUGIN,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all="snake_case")]
pub enum PluginOperation {
    AudioFilters,
    VideoFilters,
    SceneNotes,
    Sources,
    Encoders,
    Outputs,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct PluginEntryPoint {
    pub operation: PluginOperation,
    /// Exact existing Add choices (or the notes operation), for discovery only.
    pub features: &'static [&'static str],
}

// Resolve factory labels in a constant context so the entry-point arrays can
// be promoted to static storage while remaining tied to the shared catalog.
const FREEZE: &str = crate::VideoFilterFactory::Freeze.label();
const NOISE_SUPPRESSION: &str = crate::AudioFilterFactory::ObsNoiseSuppression.label();
const RENDER_DELAY: &str = crate::VideoFilterFactory::RenderDelay.label();
const MOD_DELAY: &str = crate::AudioFilterFactory::CraftModDelay.label();
const FDN_REVERB: &str = crate::AudioFilterFactory::CraftFdnReverb.label();
const SATURATOR: &str = crate::AudioFilterFactory::CraftSaturator.label();
const LO_FI: &str = crate::AudioFilterFactory::CraftLoFi.label();
const DC_REMOVAL: &str = crate::AudioFilterFactory::CraftDcRemoval.label();
const RECTIFIER: &str = crate::AudioFilterFactory::CraftRectifier.label();
const DITHER: &str = crate::AudioFilterFactory::CraftDither.label();
const SEPIA: &str = crate::VideoFilterFactory::Sepia.label();
const PHOTO_FILTER: &str = crate::VideoFilterFactory::PhotoFilter.label();
const GAUSSIAN_BLUR: &str = crate::VideoFilterFactory::GaussianBlur.label();

/// Static operations, not automatic source/filter creation or native registration.
pub fn plugin_entry_points(id: &str) -> &'static [PluginEntryPoint] {
    match id {
        crate::EXELDRO_FREEZE_PLUGIN=>&[PluginEntryPoint{operation:PluginOperation::VideoFilters,features:&[FREEZE]}],
        "obsproject/obs-studio:obs-nvenc"=>&[PluginEntryPoint {operation:PluginOperation::Encoders,features:&["NVIDIA NVENC H.264 (Windows x64 · NV12 · CBR · Baseline)"]}],
        crate::BROWSER_PLUGIN=>&[PluginEntryPoint {operation:PluginOperation::Sources,features:&["Browser Source (CEF127 software OSR)","Browser audio control (CEF48kHz stereo PCM)"]}],
        crate::NDI_PLUGIN=>&[PluginEntryPoint {operation:PluginOperation::Sources,features:&["NDI Source (installed NDI6 Runtime)"]},PluginEntryPoint {operation:PluginOperation::Outputs,features:&["Program NDI video and audio","Preview NDI video (base canvas)"]}],
        "obsproject/obs-studio:image-source"=>&[PluginEntryPoint {operation:PluginOperation::Sources,features:&["Image (including animated GIF, APNG and WebP)","Image Slide Show"]}],
        "obsproject/obs-studio:obs-ffmpeg"=>&[PluginEntryPoint {operation:PluginOperation::Sources,features:&["Media Source"]}],
        "obsproject/obs-studio:win-capture"=>&[PluginEntryPoint {operation:PluginOperation::Sources,features:&["Display Capture","Window Capture","Game Capture"]}],
        "obsproject/obs-studio:win-dshow"|"obsproject/obs-studio:mac-avcapture"=>&[PluginEntryPoint {operation:PluginOperation::Sources,features:&["Video Capture Device"]}],
        "obsproject/obs-studio:win-wasapi"=>&[PluginEntryPoint {operation:PluginOperation::Sources,features:&["Audio Input Capture","Audio Output Capture","Application Audio Capture"]}],
        OBS_VIDEO_FILTERS_PLUGIN => &[
            PluginEntryPoint {
                operation: PluginOperation::AudioFilters,
                features: &[
                    "Add Gain",
                    "Add Invert Polarity",
                    "Add OBS Limiter",
                    "Add OBS Compressor",
                    "Add OBS Expander",
                    "Add OBS Noise Gate",
                    "Add OBS 3-Band Equalizer",
                    "Add OBS Upward Compressor",
                    NOISE_SUPPRESSION,
                ],
            },
            PluginEntryPoint {
                operation: PluginOperation::VideoFilters,
                features: &["Color Correction", "Color Key", "Chroma Key", "Luma Key", "Sharpen", "Crop/Pad", "Image Mask/Blend", "Apply LUT", "Scroll", "Scaling/Aspect Ratio",RENDER_DELAY],
            },
        ],
        CRAFT_AUDIO_PLUGIN => &[PluginEntryPoint {
            operation: PluginOperation::AudioFilters,
            features: &["Add Compressor", "Add Gate", "Add Simple EQ",MOD_DELAY,FDN_REVERB,SATURATOR,LO_FI,DC_REMOVAL,RECTIFIER,DITHER],
        }],
        CRAFT_VIDEO_PLUGIN => &[PluginEntryPoint {
            operation: PluginOperation::VideoFilters,
            features: &[
                "Linear Color Key",
                "Brightness & Contrast",
                "Spill Suppression",
                "Invert Colors", "Grayscale", "Posterize", "Exposure", "Temperature & Tint", "Vibrance", "Tint (EffectCraft)", "Tritone (EffectCraft)", "Color Balance (EffectCraft)", "Channel Mixer (EffectCraft)", "Threshold", "Vignette",
                SEPIA,PHOTO_FILTER,GAUSSIAN_BLUR,
            ],
        }],
        AUDIO_PAN_PLUGIN => &[PluginEntryPoint {
            operation: PluginOperation::AudioFilters,
            features: &["Add Audio Pan Filter"],
        }],
        PERSON_BACKGROUND_PLUGIN => &[PluginEntryPoint {
            operation: PluginOperation::VideoFilters,
            features: &["AI Background Removal (Person)"],
        }],
        SCENE_NOTES_PLUGIN => &[PluginEntryPoint {
            operation: PluginOperation::SceneNotes,
            features: &["Scene Notes"],
        }],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_actual_compiled_providers_have_entry_points() {
        for provider in crate::plugin_catalog() {
            let entries = plugin_entry_points(provider.id);
            assert_eq!(!entries.is_empty(), provider.implemented, "{}", provider.id);
            let mut seen = std::collections::HashSet::new();
            for entry in entries {
                assert!(seen.insert(entry.operation));
                assert!(!entry.features.is_empty());
                assert!(entry.features.iter().all(|name| !name.is_empty()));
            }
        }
        assert!(plugin_entry_points("untrusted/not-installed").is_empty());
        assert!(plugin_entry_points("norihiro/obs-mute-filter").is_empty());
    }
    #[test]
    fn entry_points_match_existing_filter_provider_ownership_and_defaults() {
        use crate::{AudioFilter, VideoEffect};
        for (id, filters) in [
            (
                OBS_VIDEO_FILTERS_PLUGIN,
                vec![
                    AudioFilter::obs_gain(),
                    AudioFilter::obs_polarity_invert(),
                    AudioFilter::obs_limiter(),
                    AudioFilter::obs_compressor(),
                    AudioFilter::obs_expander(),
                ],
            ),
            (
                CRAFT_AUDIO_PLUGIN,
                vec![
                    AudioFilter::compressor(),
                    AudioFilter::gate(),
                    AudioFilter::simple_eq(),
                ],
            ),
            (AUDIO_PAN_PLUGIN, vec![AudioFilter::default()]),
        ] {
            let entry = plugin_entry_points(id)
                .iter()
                .find(|entry| entry.operation == PluginOperation::AudioFilters)
                .unwrap();
            assert_eq!(entry.features.len(), filters.len());
            assert!(
                filters
                    .iter()
                    .all(|filter| filter.plugin_id() == id && filter.validate().is_ok())
            );
        }
        for (id, effects) in [
            (
                PERSON_BACKGROUND_PLUGIN,
                vec![VideoEffect::person_background_removal()],
            ),
            (
                OBS_VIDEO_FILTERS_PLUGIN,
                vec![
                    VideoEffect::color_correction(),
                    VideoEffect::color_key_v2(),
                    VideoEffect::chroma_key_v2(),
                ],
            ),
            (
                CRAFT_VIDEO_PLUGIN,
                vec![
                    VideoEffect::color_key(),
                    VideoEffect::brightness_contrast(),
                    VideoEffect::spill_suppression(),
                ],
            ),
        ] {
            let entry = plugin_entry_points(id)
                .iter()
                .find(|entry| entry.operation == PluginOperation::VideoFilters)
                .unwrap();
            assert_eq!(
                entry.features,
                effects.iter().map(VideoEffect::name).collect::<Vec<_>>()
            );
            assert!(
                effects
                    .iter()
                    .all(|effect| effect.plugin_id() == id && effect.validate().is_ok())
            );
        }
        let prefs = crate::Session::new().snapshot().preferences().clone();
        assert!(prefs.plugin_enabled(CRAFT_AUDIO_PLUGIN));
        assert!(prefs.plugin_enabled(CRAFT_VIDEO_PLUGIN));
        assert!(prefs.plugin_enabled(OBS_VIDEO_FILTERS_PLUGIN));
        assert!(!prefs.plugin_enabled(AUDIO_PAN_PLUGIN));
        assert!(!prefs.plugin_enabled(SCENE_NOTES_PLUGIN));
        assert!(!prefs.plugin_enabled(PERSON_BACKGROUND_PLUGIN));
    }
}
