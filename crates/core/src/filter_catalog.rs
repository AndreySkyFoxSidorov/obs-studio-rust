//! Closed, bounded discovery of actual filter prototypes. No evaluation or I/O.
use crate::{AudioFilter, PluginOperation, VideoEffect};
use serde::{Deserialize, Serialize};

// Each registration owns its stable ID, existing constructor and semantic help.
// A new algorithm is advertised only after its real constructor is connected.
macro_rules! factories {
    ($name:ident, $value:ty, $count:literal, [
        $($variant:ident => ($label:literal, $make:path, $hint:literal)),+ $(,)?
    ]) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const ALL: [Self; $count] = [$(Self::$variant),+];
            pub const fn label(self) -> &'static str {
                match self { $(Self::$variant => $label),+ }
            }
            pub const fn hint(self) -> &'static str {
                match self { $(Self::$variant => $hint),+ }
            }
            pub fn prototype(self) -> $value {
                match self { $(Self::$variant => $make()),+ }
            }
            pub fn plugin_id(self) -> &'static str { self.prototype().plugin_id() }
        }
    };
}

factories!(AudioFilterFactory, AudioFilter, 20, [
    Pan => ("Add Audio Pan Filter", AudioFilter::default,
        "Add Audio Pan Filter"),
    CraftCompressor => ("Add Compressor", AudioFilter::compressor,
        "Add Compressor"),
    CraftGate => ("Add Gate", AudioFilter::gate,
        "Add Gate"),
    CraftEq => ("Add Simple EQ", AudioFilter::simple_eq,
        "Add Simple EQ"),
    ObsGain => ("Add Gain", AudioFilter::obs_gain,
        "Add Gain"),
    ObsPolarityInvert => ("Add Invert Polarity", AudioFilter::obs_polarity_invert,
        "Add Invert Polarity"),
    ObsLimiter => ("Add OBS Limiter", AudioFilter::obs_limiter,
        "Add OBS Limiter"),
    ObsCompressor => ("Add OBS Compressor", AudioFilter::obs_compressor,
        "Add OBS Compressor"),
    ObsExpander => ("Add OBS Expander", AudioFilter::obs_expander,
        "Add OBS Expander"),
    ObsNoiseGate => ("Add OBS Noise Gate", AudioFilter::obs_noise_gate,
        "Add the OBS noise gate with separate opening and closing thresholds, attack, hold and release."),
    ObsUpwardCompressor => ("Add OBS Upward Compressor", AudioFilter::obs_upward_compressor,
        "Raise quiet audio with the OBS upward compressor, its noise-floor rule and adjustable knee."),
    ObsThreeBandEq => ("Add OBS 3-Band Equalizer", AudioFilter::obs_three_band_eq,
        "Add the standard OBS equalizer with independent low, mid and high band gains."),
    CraftModDelay => ("Add Craft Mod Delay", AudioFilter::craft_mod_delay,
        "Add SoundCraft's modulated feedback delay with its original time, modulation, feedback and mix controls."),
    CraftFdnReverb => ("Add Craft FDN Reverb", AudioFilter::craft_fdn_reverb,
        "Add FilmCraft's eight-line algorithmic reverb with pre-delay, RT60, damping, size and mix."),
    CraftDither => ("Add Craft Dither", AudioFilter::craft_dither,
        "Add SoundCraft TPDF dither with optional first-order noise shaping."),
    CraftRectifier => ("Add Craft Rectifier", AudioFilter::craft_rectifier,
        "Add SoundCraft half or full-wave rectification with DC removal and wet mix."),
    CraftDcRemoval => ("Add Craft DC Removal", AudioFilter::craft_dc_removal,
        "Remove DC offset using SoundCraft’s adjustable one-pole high-pass filter."),
    CraftLoFi => ("Add Craft Lo-Fi", AudioFilter::craft_lofi,
        "Add SoundCraft bit reduction, sample-rate decimation and optional noise."),
    CraftSaturator => ("Add Craft Saturator", AudioFilter::craft_saturator,
        "Add SoundCraft's saturator with drive, asymmetry, tone, mix and output controls."),
    ObsNoiseSuppression => ("Add OBS Noise Suppression (RNNoise)", AudioFilter::obs_noise_suppression,
        "Suppress background noise with the original RNNoise model. Processing uses the audio worker and adds latency; CPU use grows with channel count.")
]);

factories!(VideoFilterFactory, VideoEffect, 33, [
    ColorCorrection => ("Color Correction", VideoEffect::color_correction,
        "Adjust SDR gamma, contrast, brightness, saturation, hue, opacity and color washes."),
    ColorKey => ("Color Key", VideoEffect::color_key_v2,
        "Remove a background using OBS Color Key v2 RGB distance and SDR color controls."),
    ChromaKey => ("Chroma Key", VideoEffect::chroma_key_v2,
        "Remove a background using OBS Chroma Key v2 spatial sampling and spill reduction."),
    LumaKey => ("Luma Key", VideoEffect::luma_key_v2,
        "Remove pixels outside a luminance band using OBS Luma Key v2."),
    Sharpen => ("Sharpen", VideoEffect::sharpen_v2,
        "Sharpen RGBA edges with OBS's gated 3×3 kernel, including transparency."),
    CropPad => ("Crop/Pad", VideoEffect::crop_pad,
        "Crop source edges or add transparent padding; filters below receive the new image size."),
    ImageMask => ("Image Mask/Blend", VideoEffect::image_mask_v2,
        "Apply a static image as an alpha mask or blend it with the source using the five OBS v2 methods."),
    ApplyLut => ("Apply LUT", VideoEffect::apply_lut,
        "Map source colors through an OBS PNG or CUBE lookup table with the original SDR interpolation and alpha options."),
    Scroll => ("Scroll", VideoEffect::scroll,
        "Move the source continuously with linear sampling, looping or transparent borders, and optional output dimensions."),
    Scale => ("Scaling/Aspect Ratio", VideoEffect::scale_filter,
        "Resize a source or change its aspect ratio before the remaining filters, while retaining transparency."),
    RenderDelay => ("Render Delay", VideoEffect::render_delay,
        "Delay video at this position in the filter chain using shared, bounded CPU frame history. Audio is unchanged."),
    AsyncVideoDelay => ("Video Delay (Async)", VideoEffect::async_video_delay,
        "Delay NDI video and audio. Streaming and recording use independent confirmed readers; Program, Replay and NDI output use estimated capture timing."),
    LinearColorKey => ("Linear Color Key", VideoEffect::color_key,
        "Remove a chosen background color or inspect its matte."),
    BrightnessContrast => ("Brightness & Contrast", VideoEffect::brightness_contrast,
        "Adjust source brightness and contrast."),
    SpillSuppression => ("Spill Suppression", VideoEffect::spill_suppression,
        "Reduce green or blue color spill around foreground edges."),
    Invert => ("Invert Colors", VideoEffect::invert,
        "Invert source RGB colors while preserving its alpha."),
    Grayscale => ("Grayscale", VideoEffect::grayscale,
        "Mix Rec.601 grayscale into the source without removing transparency."),
    Posterize => ("Posterize", VideoEffect::posterize,
        "Quantize RGB into PhotoCraft's eight-bit posterization levels."),
    Exposure => ("Exposure", VideoEffect::exposure,
        "Adjust exposure stops, offset and gamma with EffectCraft's signed2.2 curve."),
    TemperatureTint => ("Temperature & Tint", VideoEffect::temperature_tint,
        "Adjust warm/cool and green/magenta balance using EffectCraft's basic white balance."),
    Vibrance => ("Vibrance", VideoEffect::vibrance,
        "Adjust vibrance and saturation with EffectCraft's HSL color controls."),
    CraftTint => ("Tint (EffectCraft)", VideoEffect::craft_tint,
        "Map dark and bright tones to chosen colors using EffectCraft Tint."),
    Tritone => ("Tritone (EffectCraft)", VideoEffect::tritone,
        "Map shadows, midtones and highlights to colors with EffectCraft Tritone."),
    CraftColorBalance => ("Color Balance (EffectCraft)", VideoEffect::craft_color_balance,
        "Adjust red, green and blue in shadows, midtones and highlights."),
    CraftChannelMixer => ("Channel Mixer (EffectCraft)", VideoEffect::craft_channel_mixer,
        "Mix input RGB channels and offsets into each output channel, or create monochrome video."),
    CraftGammaPedestalGain => ("Gamma/Pedestal/Gain (EffectCraft)", VideoEffect::craft_gamma_pedestal_gain,
        "Adjust gamma, pedestal, and gain per channel with black stretch."),
    Threshold => ("Threshold", VideoEffect::threshold,
        "Convert source luminosity to black and white using PhotoCraft's eight-bit threshold rule."),
    PersonBackgroundRemoval => ("AI Background Removal (Person)", VideoEffect::person_background_removal,
        "Remove the background using the fixed Selfie256 person model on the CPU. A green screen is not required."),
    Vignette => ("Vignette", VideoEffect::vignette,
        "Shade or brighten the source edges with EffectCraft's lens falloff, adjustable center and highlight protection."),
    Sepia => ("Sepia (Photo Filter)", VideoEffect::sepia,
        "Apply EffectCraft’s fixed Sepia photo-filter tint with density and preserve-luminosity controls. CPU/GPU processing; alpha is unchanged."),
    Freeze => ("Freeze", VideoEffect::freeze_filter,
        "Hold the first real frame at this filter position. Disable and apply to release it; enable and apply to capture again. Optional edge masks overlay held regions on live video."),
    PhotoFilter => ("Photo Filter", VideoEffect::photo_filter,
        "Tint the source with EffectCraft’s 20 fixed photographic filters or a Custom color, with density and luminosity controls. CPU/GPU processing; alpha is unchanged."),
    GaussianBlur => ("Gaussian Blur", VideoEffect::gaussian_blur,
        "Blur the source using EffectCraft’s Gaussian algorithm. Large images and radii require more CPU time and memory.")
]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "media", content = "factory", rename_all = "snake_case")]
pub enum FilterFactory {
    Audio(AudioFilterFactory),
    Video(VideoFilterFactory),
}
impl FilterFactory {
    pub const fn operation(self) -> PluginOperation {
        match self {
            Self::Audio(_) => PluginOperation::AudioFilters,
            Self::Video(_) => PluginOperation::VideoFilters,
        }
    }
    pub const fn label(self) -> &'static str {
        match self { Self::Audio(value) => value.label(), Self::Video(value) => value.label() }
    }
    pub const fn hint(self) -> &'static str {
        match self { Self::Audio(value) => value.hint(), Self::Video(value) => value.hint() }
    }
    pub fn plugin_id(self) -> &'static str {
        match self { Self::Audio(value) => value.plugin_id(), Self::Video(value) => value.plugin_id() }
    }
}

pub fn plugin_filter_factories(plugin: &str) -> impl Iterator<Item = FilterFactory> + '_ {
    AudioFilterFactory::ALL.into_iter().map(FilterFactory::Audio)
        .chain(VideoFilterFactory::ALL.into_iter().map(FilterFactory::Video))
        .filter(move |factory| factory.plugin_id() == plugin)
}
