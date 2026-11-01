//! Authored Craft effect stack. Pixels and processor caches belong to media/host.
use crate::{Collection, Error, Id};
use serde::{Deserialize, Serialize};
fn is_false(value:&bool)->bool{!*value}
fn levels_channels_default()->[[f32;5];4]{[[0.0,1.0,1.0,0.0,1.0];4]}
fn levels_channels_is_default(value:&[[f32;5];4])->bool{*value==levels_channels_default()}
fn channel_alpha_default()->u8{5}
fn channel_alpha_is_default(value:&u8)->bool{*value==5}
fn no_gradient_stops(value:&[Option<(u8,[u8;3])>;4])->bool{value.iter().all(Option::is_none)}
fn valid_gradient_stops(value:[Option<(u8,[u8;3])>;4])->bool{
    let mut previous=0u8;let mut empty=false;
    for stop in value {match stop {Some((position,_)) if !empty&&position>previous&&position<255=>previous=position,None=>empty=true,_=>return false}}
    true
}

pub const PERSON_BACKGROUND_PLUGIN: &str = "royshil/obs-backgroundremoval";
/// New Person filters reuse a camera mask for up to 100 ms by default.
/// Saved filters without an interval retain their original every-frame policy.
pub const PERSON_DEFAULT_INTERVAL_MS: u16 = 100;
pub const CRAFT_VIDEO_PLUGIN: &str = "skylive/craft-video-effects";
pub const OBS_VIDEO_FILTERS_PLUGIN: &str = "obsproject/obs-studio:obs-filters";
// Keep in sync with skylive_media::MAX_SOURCE_VIDEO_EFFECTS at the host boundary.
pub const MAX_VIDEO_FILTERS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorKeyType {
    Green,
    Blue,
    Red,
    Magenta,
    Custom,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChromaKeyType {
    Green,
    Blue,
    Magenta,
    Custom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyMatch {
    #[default]
    Rgb,
    Hue,
    Chroma,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatteView {
    #[default]
    Final,
    Source,
    Matte,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpillChannel {
    #[default]
    Green,
    Blue,
}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ImageMaskMethod {#[default] MaskColor,MaskAlpha,BlendMultiply,BlendAddition,BlendSubtraction}
impl ImageMaskMethod {
    pub const ALL:[Self;5]=[Self::MaskColor,Self::MaskAlpha,Self::BlendMultiply,Self::BlendAddition,Self::BlendSubtraction];
    pub fn label(self)->&'static str{match self{Self::MaskColor=>"Alpha Mask (Color Channel)",Self::MaskAlpha=>"Alpha Mask (Alpha Channel)",Self::BlendMultiply=>"Blend (Multiply)",Self::BlendAddition=>"Blend (Addition)",Self::BlendSubtraction=>"Blend (Subtraction)"}}
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum GaussianBlurDimensions { #[default] Both, Horizontal, Vertical }
impl GaussianBlurDimensions {
    pub const ALL:[Self;3]=[Self::Both,Self::Horizontal,Self::Vertical];
    pub fn label(self)->&'static str{match self{Self::Both=>"Horizontal and Vertical",Self::Horizontal=>"Horizontal",Self::Vertical=>"Vertical"}}
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VideoEffect {
    Freeze {settings:crate::FreezeSettings},
    /// OBS async_delay_filter: holds captured frames according to their source
    /// timestamps, before ordinary rendering. This is not Render Delay.
    AsyncVideoDelay {delay_ms:u16},
    RenderDelay {delay_ms:u16},
    ScaleFilter {target:crate::ScaleFilterTarget,sampling:crate::ScaleFilterSampling,undistort:bool},
    Scroll {speed_x:f32,speed_y:f32,limit_width:bool,limit_height:bool,width:u32,height:u32,looping:bool},
    ApplyLut {amount:f32,passthrough_alpha:bool},
    ImageMask {method:ImageMaskMethod,color:[u8;3],opacity:f32,stretch:bool},
    CropPad {relative:bool,left:i32,top:i32,right:i32,bottom:i32,width:u32,height:u32},
    LumaKey {luma_max:f32,luma_min:f32,luma_max_smooth:f32,luma_min_smooth:f32,#[serde(default,skip_serializing_if="is_false")] legacy:bool},
    Sharpen {sharpness:f32},
    Invert { amount:f32 },
    Grayscale { amount:f32 },
    Posterize { levels:u16 },
    Exposure { exposure:f32, offset:f32, gamma:f32, bypass_linear:bool },
    TemperatureTint { temperature:f32, tint:f32 },
    /// EffectCraft HSL vibrance and saturation, expressed as percentages.
    Vibrance { vibrance:f32, saturation:f32 },
    /// EffectCraft Color Balance (HLS), degrees and percentages.
    HlsBalance { hue:f32, lightness:f32, saturation:f32 },
    /// EffectCraft CC Color Offset: phase in degrees and overflow mode.
    ColorOffset { phase:[f32;3], overflow:u8 },
    /// PhotoCraft selective CMYK adjustments by nine color ranges, in percent.
    SelectiveColor {relative:bool, adjustments:[[i8;4];9]},
    /// EffectCraft Tint; colors are encoded sRGB, amount is percent.
    CraftTint {black:[u8;3],white:[u8;3],amount:f32},
    /// EffectCraft Tritone; encoded sRGB colors, blend with original in percent.
    Tritone {shadows:[u8;3],midtones:[u8;3],highlights:[u8;3],blend:f32},
    /// EffectCraft Color Balance adjustments in percent, RGB by tonal range.
    CraftColorBalance {shadows:[f32;3],midtones:[f32;3],highlights:[f32;3],preserve_luminosity:bool},
    /// EffectCraft Channel Mixer coefficients in percent, one row per output channel.
    CraftChannelMixer {matrix:[[f32;4];3],monochrome:bool},
    /// PhotoCraft six-color monochrome adjustment with optional color tint.
    CraftBlackWhite {weights:[f32;6],tint:Option<[u8;3]>},
    /// EffectCraft Gamma/Pedestal/Gain, values in native units.
    CraftGammaPedestalGain {gamma:[f32;3],pedestal:[f32;3],gain:[f32;3],black_stretch:f32},
    Threshold { level:u16 },
    /// EffectCraft CC Vignette; center is percent of this ordered input raster.
    Vignette {amount:f32,angle_of_view:f32,center:[f32;2],pin_highlights:f32},
    /// EffectCraft Photo Filter, fixed Sepia preset; density is a percentage.
    Sepia { density:f32, preserve_luminosity:bool },
    GaussianBlur {blurriness:f32,dimensions:GaussianBlurDimensions,repeat_edge:bool},
    /// PhotoCraft Mosaic: average premultiplied pixels in aligned cells.
    Mosaic {cell_size:u16},
    PhotoFilter {preset:crate::PhotoFilterPreset,custom_color:[f32;3],density:f32,preserve_luminosity:bool},
    /// PhotoCraft Solarize, with adjustable threshold and blend for live video.
    Solarize {threshold:f32,amount:f32},
    /// EffectCraft Levels master RGB controls; values are normalized.
    CraftLevels {input_black:f32,input_white:f32,gamma:f32,output_black:f32,output_white:f32,#[serde(default="levels_channels_default",skip_serializing_if="levels_channels_is_default")] channels:[[f32;5];4]},
    /// EffectCraft Leave Color: desaturate colors outside a softened match.
    LeaveColor {color:[u8;3],amount:f32,tolerance:f32,softness:f32,by_hue:bool},
    /// EffectCraft Set Channels for this source's RGBA and constants.
    ChannelRemap {channels:[u8;3],#[serde(default="channel_alpha_default",skip_serializing_if="channel_alpha_is_default")] alpha:u8},
    /// PhotoCraft color unmixing with transparent and opaque thresholds.
    ColorToAlpha {color:[u8;3],transparency:f32,opacity:f32},
    /// Endpoints plus up to four ordered intermediate stops (positions 1..254).
    GradientMap { shadows:[u8;3], highlights:[u8;3], #[serde(default,skip_serializing_if="no_gradient_stops")] stops:[Option<(u8,[u8;3])>;4], reverse:bool, amount:f32, #[serde(default,skip_serializing_if="is_false")] dither:bool },
    PersonBackgroundRemoval {
        threshold: f32,
        softness: f32,
    },
    ColorKey {
        key_color_type: ColorKeyType,
        key_color: [u8; 3],
        similarity: u16,
        smoothness: u16,
        opacity: f32,
        contrast: f64,
        brightness: f32,
        gamma: f64,
    },
    ChromaKey {
        key_color_type: ChromaKeyType,
        key_color: [u8; 3],
        similarity: u16,
        smoothness: u16,
        spill: u16,
        opacity: f32,
        contrast: f64,
        brightness: f32,
        gamma: f64,
    },
    ColorCorrection {
        gamma: f64,
        contrast: f32,
        brightness: f32,
        saturation: f32,
        hue_shift: f32,
        opacity: f32,
        color_multiply: [u8; 3],
        color_add: [u8; 3],
    },
    LinearColorKey {
        color: [u8; 3],
        matching: KeyMatch,
        tolerance: f32,
        softness: f32,
        keep: bool,
        view: MatteView,
    },
    BrightnessContrast {
        brightness: f32,
        contrast: f32,
        legacy: bool,
    },
    SpillSuppression {
        channel: SpillChannel,
        balance: f32,
        amount: f32,
    },
}
impl VideoEffect {
    /// Factory settings for this exact provider/kind. No identity is changed.
    pub fn defaults_for_kind(self)->Self{match self{
        Self::Freeze{..}=>Self::freeze_filter(),Self::AsyncVideoDelay{..}=>Self::async_video_delay(),Self::RenderDelay{..}=>Self::render_delay(),Self::ScaleFilter{..}=>Self::scale_filter(),Self::Scroll{..}=>Self::scroll(),Self::ApplyLut{..}=>Self::apply_lut(),Self::ImageMask{..}=>Self::image_mask_v2(),Self::CropPad{..}=>Self::crop_pad(),
        // Defaults reset parameters, not the OBS filter version. Legacy luma
        // key has different alpha and luma coefficients from v2, so replacing
        // it with v2 silently changes the imported filter's behavior.
        Self::LumaKey{legacy,..}=>Self::LumaKey{luma_max:1.0,luma_min:0.0,luma_max_smooth:0.0,luma_min_smooth:0.0,legacy},Self::Sharpen{..}=>Self::sharpen_v2(),
        Self::Invert{..}=>Self::invert(),Self::Grayscale{..}=>Self::grayscale(),
        Self::Posterize{..}=>Self::posterize(),Self::Exposure{..}=>Self::exposure(),
        Self::TemperatureTint{..}=>Self::temperature_tint(),Self::Vibrance{..}=>Self::vibrance(),Self::HlsBalance{..}=>Self::hls_balance(),Self::ColorOffset{..}=>Self::color_offset(),Self::SelectiveColor{..}=>Self::selective_color(),Self::CraftTint{..}=>Self::craft_tint(),Self::Tritone{..}=>Self::tritone(),Self::CraftColorBalance{..}=>Self::craft_color_balance(),Self::CraftChannelMixer{..}=>Self::craft_channel_mixer(),Self::CraftBlackWhite{..}=>Self::craft_black_white(),Self::CraftGammaPedestalGain{..}=>Self::craft_gamma_pedestal_gain(),Self::Threshold{..}=>Self::threshold(),Self::Sepia{..}=>Self::sepia(),Self::PhotoFilter{..}=>Self::photo_filter(),
        Self::Vignette{..}=>Self::vignette(),Self::GaussianBlur{..}=>Self::gaussian_blur(),Self::Mosaic{..}=>Self::mosaic(),Self::Solarize{..}=>Self::solarize(),Self::CraftLevels{..}=>Self::craft_levels(),Self::GradientMap{..}=>Self::gradient_map(),Self::LeaveColor{..}=>Self::leave_color(),Self::ChannelRemap{..}=>Self::channel_remap(),Self::ColorToAlpha{..}=>Self::color_to_alpha(),
        Self::PersonBackgroundRemoval{..}=>Self::person_background_removal(),
        Self::ColorKey{..}=>Self::color_key_v2(),Self::ChromaKey{..}=>Self::chroma_key_v2(),
        Self::ColorCorrection{..}=>Self::color_correction(),Self::LinearColorKey{..}=>Self::color_key(),
        Self::BrightnessContrast{..}=>Self::brightness_contrast(),Self::SpillSuppression{..}=>Self::spill_suppression(),
    }}
    pub fn name(&self) -> &'static str {
        match self {
            Self::Freeze{..}=>"Freeze",
            Self::AsyncVideoDelay{..}=>"Video Delay (Async)",
            Self::RenderDelay{..}=>"Render Delay",
            Self::ScaleFilter{..}=>"Scaling/Aspect Ratio",
            Self::Scroll{..}=>"Scroll",
            Self::ApplyLut{..}=>"Apply LUT",
            Self::ImageMask{..}=>"Image Mask/Blend",
            Self::CropPad{..}=>"Crop/Pad",
            Self::LumaKey{..}=>"Luma Key",
            Self::Sharpen{..}=>"Sharpen",
            Self::Invert{..}=>"Invert Colors",
            Self::Grayscale{..}=>"Grayscale",
            Self::Posterize{..}=>"Posterize",
            Self::Exposure{..}=>"Exposure",
            Self::TemperatureTint{..}=>"Temperature & Tint",
            Self::Vibrance{..}=>"Vibrance",
            Self::HlsBalance{..}=>"Color Balance (HLS)",
            Self::ColorOffset{..}=>"CC Color Offset (EffectCraft)",
            Self::SelectiveColor{..}=>"Selective Color (PhotoCraft)",
            Self::CraftTint{..}=>"Tint (EffectCraft)",
            Self::Tritone{..}=>"Tritone (EffectCraft)",
            Self::CraftColorBalance{..}=>"Color Balance (EffectCraft)",
            Self::CraftChannelMixer{..}=>"Channel Mixer (EffectCraft)",
            Self::CraftBlackWhite{..}=>"Black & White (PhotoCraft)",
            Self::CraftGammaPedestalGain{..}=>"Gamma/Pedestal/Gain (EffectCraft)",
            Self::Threshold{..}=>"Threshold",
            Self::Solarize{..}=>"Solarize (PhotoCraft)",
            Self::CraftLevels{..}=>"Levels (EffectCraft)",
            Self::LeaveColor{..}=>"Leave Color (EffectCraft)",
            Self::ChannelRemap{..}=>"Set RGB Channels (EffectCraft)",
            Self::ColorToAlpha{..}=>"Color to Alpha (PhotoCraft)",
            Self::GradientMap{..}=>"Gradient Map (PhotoCraft)",
            Self::Vignette{..}=>"Vignette",
            Self::Sepia{..}=>"Sepia (Photo Filter)",
            Self::PhotoFilter{..}=>"Photo Filter",
            Self::GaussianBlur{..}=>"Gaussian Blur",
            Self::Mosaic{..}=>"Mosaic (PhotoCraft)",
            Self::PersonBackgroundRemoval { .. } => "AI Background Removal (Person)",
            Self::ColorKey { .. } => "Color Key",
            Self::ChromaKey { .. } => "Chroma Key",
            Self::ColorCorrection { .. } => "Color Correction",
            Self::LinearColorKey { .. } => "Linear Color Key",
            Self::BrightnessContrast { .. } => "Brightness & Contrast",
            Self::SpillSuppression { .. } => "Spill Suppression",
        }
    }
    pub fn freeze_filter()->Self{Self::Freeze{settings:crate::FreezeSettings::default()}}
    pub fn async_video_delay()->Self{Self::AsyncVideoDelay{delay_ms:0}}
    pub fn render_delay()->Self{Self::RenderDelay{delay_ms:0}}
    pub fn scale_filter()->Self{Self::ScaleFilter {target:Default::default(),sampling:Default::default(),undistort:false}}
    pub fn scroll()->Self{Self::Scroll {speed_x:0.0,speed_y:0.0,limit_width:false,limit_height:false,width:100,height:100,looping:true}}
    pub fn apply_lut()->Self{Self::ApplyLut{amount:1.0,passthrough_alpha:false}}
    pub fn image_mask_v2()->Self{Self::ImageMask{method:ImageMaskMethod::MaskColor,color:[255;3],opacity:1.0,stretch:false}}
    pub fn crop_pad()->Self{Self::CropPad {relative:true,left:0,top:0,right:0,bottom:0,width:0,height:0}}
    pub fn luma_key_v2()->Self{Self::LumaKey {luma_max:1.0,luma_min:0.0,luma_max_smooth:0.0,luma_min_smooth:0.0,legacy:false}}
    pub fn sharpen_v2()->Self{Self::Sharpen {sharpness:0.08}}
    pub fn invert()->Self{Self::Invert{amount:100.0}}
    pub fn grayscale()->Self{Self::Grayscale{amount:100.0}}
    pub fn posterize()->Self{Self::Posterize{levels:4}}
    pub fn exposure()->Self{Self::Exposure{exposure:0.0,offset:0.0,gamma:1.0,bypass_linear:false}}
    pub fn temperature_tint()->Self{Self::TemperatureTint{temperature:0.0,tint:0.0}}
    pub fn vibrance()->Self{Self::Vibrance{vibrance:0.0,saturation:0.0}}
    pub fn hls_balance()->Self{Self::HlsBalance{hue:0.0,lightness:0.0,saturation:0.0}}
    pub fn color_offset()->Self{Self::ColorOffset{phase:[0.0;3],overflow:0}}
    pub fn selective_color()->Self{Self::SelectiveColor{relative:true,adjustments:[[0;4];9]}}
    pub fn craft_tint()->Self{Self::CraftTint{black:[0,0,0],white:[255,255,255],amount:100.0}}
    pub fn tritone()->Self{Self::Tritone{shadows:[0,0,0],midtones:[128,102,77],highlights:[255,255,255],blend:0.0}}
    pub fn craft_color_balance()->Self{Self::CraftColorBalance{shadows:[0.0;3],midtones:[0.0;3],highlights:[0.0;3],preserve_luminosity:false}}
    pub fn craft_channel_mixer()->Self{Self::CraftChannelMixer{matrix:[[100.0,0.0,0.0,0.0],[0.0,100.0,0.0,0.0],[0.0,0.0,100.0,0.0]],monochrome:false}}
    pub fn craft_black_white()->Self{Self::CraftBlackWhite{weights:[40.0,60.0,40.0,60.0,20.0,80.0],tint:None}}
    pub fn craft_gamma_pedestal_gain()->Self{Self::CraftGammaPedestalGain{gamma:[1.0;3],pedestal:[0.0;3],gain:[1.0;3],black_stretch:0.0}}
    pub fn threshold()->Self{Self::Threshold{level:128}}
    pub fn solarize()->Self{Self::Solarize{threshold:50.0,amount:100.0}}
    pub fn craft_levels()->Self{Self::CraftLevels{input_black:0.0,input_white:1.0,gamma:1.0,output_black:0.0,output_white:1.0,channels:levels_channels_default()}}
    pub fn leave_color()->Self{Self::LeaveColor{color:[255,0,0],amount:0.0,tolerance:15.0,softness:0.0,by_hue:false}}
    pub fn channel_remap()->Self{Self::ChannelRemap{channels:[0,1,2],alpha:5}}
    pub fn color_to_alpha()->Self{Self::ColorToAlpha{color:[255;3],transparency:0.0,opacity:1.0}}
    pub fn gradient_map()->Self{Self::GradientMap{shadows:[0,0,0],highlights:[255,255,255],stops:[None;4],reverse:false,amount:100.0,dither:false}}
    pub fn gaussian_blur()->Self{Self::GaussianBlur{blurriness:0.0,dimensions:GaussianBlurDimensions::Both,repeat_edge:false}}
    pub fn mosaic()->Self{Self::Mosaic{cell_size:16}}
    pub fn vignette()->Self{Self::Vignette {amount:50.0,angle_of_view:60.0,center:[50.0,50.0],pin_highlights:0.0}}
    pub fn sepia()->Self{Self::Sepia{density:25.0,preserve_luminosity:true}}
    pub fn photo_filter()->Self{Self::PhotoFilter{preset:crate::PhotoFilterPreset::Warming85,custom_color:[0.93,0.54,0.09],density:25.0,preserve_luminosity:true}}
    pub fn person_background_removal() -> Self {
        Self::PersonBackgroundRemoval {
            threshold: 0.5,
            softness: 1.0,
        }
    }
    pub fn color_correction() -> Self {
        Self::ColorCorrection {
            gamma: 0.0,
            contrast: 0.0,
            brightness: 0.0,
            saturation: 0.0,
            hue_shift: 0.0,
            opacity: 1.0,
            color_multiply: [255; 3],
            color_add: [0; 3],
        }
    }
    pub fn color_key_v2() -> Self {
        Self::ColorKey {
            key_color_type: ColorKeyType::Green,
            key_color: [0, 255, 0],
            similarity: 80,
            smoothness: 50,
            opacity: 1.0,
            contrast: 0.0,
            brightness: 0.0,
            gamma: 0.0,
        }
    }
    pub fn chroma_key_v2() -> Self {
        Self::ChromaKey {
            key_color_type: ChromaKeyType::Green,
            key_color: [0, 255, 0],
            similarity: 400,
            smoothness: 80,
            spill: 100,
            opacity: 1.0,
            contrast: 0.0,
            brightness: 0.0,
            gamma: 0.0,
        }
    }
    pub fn plugin_id(&self) -> &'static str {
        match self {
            Self::Freeze{..}=>crate::EXELDRO_FREEZE_PLUGIN,
            Self::PersonBackgroundRemoval { .. } => PERSON_BACKGROUND_PLUGIN,
            Self::AsyncVideoDelay{..} | Self::RenderDelay{..} | Self::ScaleFilter{..} | Self::Scroll{..} | Self::ApplyLut{..} | Self::ImageMask {..} | Self::CropPad {..} | Self::LumaKey {..} | Self::Sharpen {..} | Self::ColorCorrection { .. } | Self::ColorKey { .. } | Self::ChromaKey { .. } => {
                OBS_VIDEO_FILTERS_PLUGIN
            }
            _ => CRAFT_VIDEO_PLUGIN,
        }
    }
    pub fn color_key() -> Self {
        Self::LinearColorKey {
            color: [0, 255, 0],
            matching: KeyMatch::Rgb,
            tolerance: 10.0,
            softness: 10.0,
            keep: false,
            view: MatteView::Final,
        }
    }
    pub fn brightness_contrast() -> Self {
        Self::BrightnessContrast {
            brightness: 0.0,
            contrast: 0.0,
            legacy: false,
        }
    }
    pub fn spill_suppression() -> Self {
        Self::SpillSuppression {
            channel: SpillChannel::Green,
            balance: 50.0,
            amount: 100.0,
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        let percent = |v: f32| v.is_finite() && (0.0..=100.0).contains(&v);
        if let Self::Freeze{settings}=*self{return settings.validate_manual();}
        let valid = match *self {
            Self::Freeze{..}=>true, // handled by the exact diagnostic above
            Self::AsyncVideoDelay{delay_ms}=>delay_ms<=20_000,
            Self::RenderDelay{delay_ms}=>delay_ms<=500,
            Self::ScaleFilter{target,..}=>target.validate().is_ok(),
            Self::Scroll {speed_x,speed_y,width,height,..}=>[speed_x,speed_y].into_iter().all(|v|v.is_finite()&&(-500.0..=500.0).contains(&v))&&(1..=8192).contains(&width)&&(1..=8192).contains(&height),
            Self::ApplyLut{amount,..}=>amount.is_finite()&&(0.0..=1.0).contains(&amount),
            Self::ImageMask{opacity,..}=>opacity.is_finite()&&(0.0..=1.0).contains(&opacity),
            Self::CropPad {left,top,right,bottom,width,height,..}=>[left,top,right,bottom].into_iter().all(|v|(-8192..=8192).contains(&v)) && width<=8192 && height<=8192,
            Self::LumaKey {luma_max,luma_min,luma_max_smooth,luma_min_smooth,..}=>[luma_max,luma_min,luma_max_smooth,luma_min_smooth].iter().all(|v|v.is_finite() && (0.0..=1.0).contains(v)),
            Self::Sharpen {sharpness}=>sharpness.is_finite() && (0.0..=1.0).contains(&sharpness),
            Self::Invert{amount}|Self::Grayscale{amount}=>percent(amount),
            Self::Solarize{threshold,amount}=>percent(threshold)&&percent(amount),
            Self::CraftLevels{input_black,input_white,gamma,output_black,output_white,channels}=>{let valid=|p:[f32;5]| [p[0],p[1],p[3],p[4]].into_iter().all(|v|v.is_finite()&&(0.0..=1.0).contains(&v))&&p[0]<p[1]&&p[2].is_finite()&&(0.01..=10.0).contains(&p[2]);valid([input_black,input_white,gamma,output_black,output_white])&&channels.into_iter().all(valid)},
            Self::LeaveColor{amount,tolerance,softness,..}=>percent(amount)&&percent(tolerance)&&percent(softness),
            Self::ChannelRemap{channels,alpha}=>channels.into_iter().all(|channel|channel<=5)&&alpha<=5,
            Self::ColorToAlpha{transparency,opacity,..}=>[transparency,opacity].into_iter().all(|v|v.is_finite()&&(0.0..=1.0).contains(&v)),
            Self::GradientMap{amount,stops,..}=>percent(amount)&&valid_gradient_stops(stops),
            Self::GaussianBlur{blurriness,..}=>blurriness.is_finite()&&(0.0..=3000.0).contains(&blurriness),
            Self::Mosaic{cell_size} => (1..=512).contains(&cell_size),
            Self::Sepia{density,..}=>percent(density),
            Self::PhotoFilter{custom_color,density,..}=>percent(density)&&custom_color.iter().all(|value|value.is_finite()&&(0.0..=1.0).contains(value)),
            Self::Posterize{levels}=>(2..=255).contains(&levels),
            Self::Threshold{level}=>(1..=255).contains(&level),
            Self::Vignette {amount,angle_of_view,center,pin_highlights} =>
                amount.is_finite() && (-100.0..=100.0).contains(&amount)
                && angle_of_view.is_finite() && (0.0..=179.0).contains(&angle_of_view)
                && center.iter().all(|value|value.is_finite() && (-800.0..=800.0).contains(value))
                && percent(pin_highlights),
            Self::Exposure{exposure,offset,gamma,..}=>exposure.is_finite()&&(-20.0..=20.0).contains(&exposure)&&offset.is_finite()&&(-0.5..=0.5).contains(&offset)&&gamma.is_finite()&&(0.01..=9.99).contains(&gamma),
            Self::TemperatureTint{temperature,tint}=>[temperature,tint].iter().all(|value|value.is_finite()&&(-100.0..=100.0).contains(value)),
            Self::Vibrance{vibrance,saturation}=>[vibrance,saturation].iter().all(|value|value.is_finite()&&(-100.0..=100.0).contains(value)),
            Self::HlsBalance{hue,lightness,saturation}=>hue.is_finite()&&(-360.0..=360.0).contains(&hue)&&[lightness,saturation].iter().all(|v|v.is_finite()&&(-100.0..=100.0).contains(v)),
            Self::ColorOffset{phase,overflow}=>overflow<=2&&phase.into_iter().all(|v|v.is_finite()&&(-360.0..=360.0).contains(&v)),
            Self::SelectiveColor{adjustments,..}=>adjustments.into_iter().flatten().all(|value|(-100..=100).contains(&value)),
            Self::CraftTint{amount,..}=>percent(amount),
            Self::Tritone{blend,..}=>percent(blend),
            Self::CraftColorBalance{shadows,midtones,highlights,..}=>shadows.into_iter().chain(midtones).chain(highlights).all(|v|v.is_finite()&&(-100.0..=100.0).contains(&v)),
            Self::CraftChannelMixer{matrix,..}=>matrix.into_iter().flatten().all(|v|v.is_finite()&&(-200.0..=200.0).contains(&v)),
            Self::CraftBlackWhite{weights,..}=>weights.into_iter().all(|v|v.is_finite()&&(-200.0..=300.0).contains(&v)),
            Self::CraftGammaPedestalGain{gamma,pedestal,gain,black_stretch}=>gamma.into_iter().all(|v|v.is_finite()&&(0.1..=10.0).contains(&v))&&pedestal.into_iter().all(|v|v.is_finite()&&(-2.0..=2.0).contains(&v))&&gain.into_iter().all(|v|v.is_finite()&&(0.0..=4.0).contains(&v))&&black_stretch.is_finite()&&(0.0..=4.0).contains(&black_stretch),
            Self::PersonBackgroundRemoval {
                threshold,
                softness,
            } => {
                threshold.is_finite()
                    && (0.0..=1.0).contains(&threshold)
                    && softness.is_finite()
                    && (0.001..=1.0).contains(&softness)
            }
            Self::ColorKey {
                similarity,
                smoothness,
                opacity,
                contrast,
                brightness,
                gamma,
                ..
            } => {
                key_curve_valid(opacity, contrast, brightness, gamma)
                    && (1..=1000).contains(&similarity)
                    && (1..=1000).contains(&smoothness)
            }
            Self::ChromaKey {
                similarity,
                smoothness,
                spill,
                opacity,
                contrast,
                brightness,
                gamma,
                ..
            } => {
                key_curve_valid(opacity, contrast, brightness, gamma)
                    && (1..=1000).contains(&similarity)
                    && (1..=1000).contains(&smoothness)
                    && (1..=1000).contains(&spill)
            }
            Self::ColorCorrection {
                gamma,
                contrast,
                brightness,
                saturation,
                hue_shift,
                opacity,
                ..
            } => {
                gamma.is_finite()
                    && (-3.0..=3.0).contains(&gamma)
                    && [
                        (contrast, -4.0, 4.0),
                        (brightness, -1.0, 1.0),
                        (saturation, -1.0, 5.0),
                        (hue_shift, -180.0, 180.0),
                        (opacity, 0.0, 1.0),
                    ]
                    .iter()
                    .all(|&(value, min, max)| value.is_finite() && (min..=max).contains(&value))
            }
            Self::LinearColorKey {
                tolerance,
                softness,
                ..
            } => percent(tolerance) && percent(softness),
            Self::BrightnessContrast {
                brightness,
                contrast,
                ..
            } => {
                brightness.is_finite()
                    && contrast.is_finite()
                    && (-150.0..=150.0).contains(&brightness)
                    && (-100.0..=100.0).contains(&contrast)
            }
            Self::SpillSuppression {
                balance, amount, ..
            } => percent(balance) && percent(amount),
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Invalid(
                "video effect contains a non-finite or out-of-range parameter",
            ))
        }
    }
}
/// Optional provider state is canonical. Unchanged disabled configurations may
/// remain or be removed; adding/changing them requires explicit activation.
pub(crate) fn validate_activation(
    preferences: &crate::UserPreferences,
    old: &[VideoFilter],
    new: &[VideoFilter],
) -> Result<(), Error> {
    if new.len() > MAX_VIDEO_FILTERS {
        return Err(Error::Limit("video filter stack exceeds 32 effects"));
    }
    let mut retained = old.iter().collect::<Vec<_>>();
    for filter in new {
        filter.validate()?;
        if !preferences.plugin_enabled(filter.plugin_id()) {
            // An omitted name with an existing ID means retain that name in
            // the existing collection command, not rename the disabled filter.
            let mut normalized = filter.clone();
            if normalized.name.is_none() {
                normalized.name = normalized.id.and_then(|id| {
                    old.iter()
                        .find(|existing| existing.id == Some(id))
                        .and_then(|existing| existing.name.clone())
                });
            }
            if let Some(index) = retained
                .iter()
                .position(|existing| **existing == normalized)
            {
                retained.remove(index);
            } else {
                return Err(Error::Unavailable(
                    "enable the video filter module before adding or changing its settings",
                ));
            }
        }
    }
    Ok(())
}

fn key_curve_valid(opacity: f32, contrast: f64, brightness: f32, gamma: f64) -> bool {
    opacity.is_finite()
        && (0.0..=1.0).contains(&opacity)
        && contrast.is_finite()
        && (-4.0..=4.0).contains(&contrast)
        && brightness.is_finite()
        && (-1.0..=1.0).contains(&brightness)
        && gamma.is_finite()
        && (-1.0..=1.0).contains(&gamma)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct VideoFilterId(Id);
impl VideoFilterId {
    pub fn new(value: u64) -> Result<Self, Error> {
        if value == 0 {
            Err(Error::Invalid("video filter ID must be nonzero"))
        } else {
            Ok(Self(Id::new(value)))
        }
    }
    pub fn get(self) -> u64 {
        self.0.get()
    }
    pub(super) fn entity(self) -> Id {
        self.0
    }
}
impl TryFrom<u64> for VideoFilterId {
    type Error = Error;
    fn try_from(v: u64) -> Result<Self, Error> {
        Self::new(v)
    }
}
impl From<VideoFilterId> for u64 {
    fn from(v: VideoFilterId) -> u64 {
        v.get()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "FilterData")]
pub struct VideoFilter {
    #[serde(skip_serializing_if="Option::is_none")]
    person_interval_ms:Option<u16>,
    #[serde(skip_serializing_if="Option::is_none")]
    lut_asset:Option<crate::LutAsset>,
    #[serde(skip_serializing_if="Option::is_none")]
    mask_image:Option<crate::ImageAsset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<VideoFilterId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    pub enabled: bool,
    pub effect: VideoEffect,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilterData {
    #[serde(default)]
    person_interval_ms:Option<u16>,
    #[serde(default)]
    lut_asset:Option<crate::LutAsset>,
    #[serde(default)]
    mask_image:Option<crate::ImageAsset>,
    #[serde(default, deserialize_with = "present_id")]
    id: Option<VideoFilterId>,
    #[serde(default, deserialize_with = "present_name")]
    name: Option<String>,
    enabled: bool,
    effect: VideoEffect,
}
fn present_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<VideoFilterId>, D::Error> {
    VideoFilterId::deserialize(d).map(Some)
}
fn present_name<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}
impl TryFrom<FilterData> for VideoFilter {
    type Error = Error;
    fn try_from(value: FilterData) -> Result<Self, Error> {
        let filter = Self {
            person_interval_ms:value.person_interval_ms,
            lut_asset:value.lut_asset,
            mask_image:value.mask_image,
            id: value.id,
            name: value.name,
            enabled: value.enabled,
            effect: value.effect,
        };
        filter.validate()?;
        Ok(filter)
    }
}
impl VideoFilter {
    /// Reset an owned draft; Session publication remains a separate operation.
    pub fn reset_settings(&mut self){self.person_interval_ms=matches!(self.effect,VideoEffect::PersonBackgroundRemoval{..}).then_some(PERSON_DEFAULT_INTERVAL_MS);self.effect=self.effect.defaults_for_kind();self.mask_image=None;self.lut_asset=None;}
    pub fn new(effect: VideoEffect) -> Result<Self, Error> {
        effect.validate()?;
        Ok(Self {
            person_interval_ms:matches!(effect,VideoEffect::PersonBackgroundRemoval{..}).then_some(PERSON_DEFAULT_INTERVAL_MS),
            lut_asset:None,
            mask_image:None,
            id: None,
            name: None,
            enabled: true,
            effect,
        })
    }
    pub fn person_interval_ms(&self)->u16{self.person_interval_ms.unwrap_or(0)}
    pub fn set_person_interval_ms(&mut self,value:u16)->Result<(),Error>{if value>5000||!matches!(self.effect,VideoEffect::PersonBackgroundRemoval{..}){return Err(Error::Invalid("Person inference interval requires the AI filter and 0..5000 ms."));}self.person_interval_ms=(value!=0).then_some(value);Ok(())}
    pub fn lut_asset(&self)->Option<&crate::LutAsset>{self.lut_asset.as_ref()}
    pub fn with_lut_asset(mut self,asset:Option<crate::LutAsset>)->Result<Self,Error>{self.lut_asset=asset;self.validate()?;Ok(self)}
    pub fn mask_image(&self)->Option<&crate::ImageAsset>{self.mask_image.as_ref()}
    pub fn with_mask_image(mut self,image:Option<crate::ImageAsset>)->Result<Self,Error>{self.mask_image=image;self.validate()?;Ok(self)}
    pub fn id(&self) -> Option<VideoFilterId> {
        self.id
    }
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or_else(|| self.effect.name())
    }
    pub fn with_name(mut self, name: String) -> Result<Self, Error> {
        crate::validate_name(&name)?;
        self.name = Some(name.into_boxed_str().into_string());
        Ok(self)
    }
    pub fn as_new(&self) -> Self {
        let mut result = self.clone();
        result.id = None;
        result
    }
    pub fn plugin_id(&self) -> &'static str {
        self.effect.plugin_id()
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.person_interval_ms.is_some_and(|ms|ms==0||ms>5000)||self.person_interval_ms.is_some()&&!matches!(self.effect,VideoEffect::PersonBackgroundRemoval{..}){return Err(Error::Invalid("Person inference interval requires the AI filter and 1..5000 ms."));}
        if let Some(asset)=&self.lut_asset {if !matches!(self.effect,VideoEffect::ApplyLut{..}){return Err(Error::Invalid("LUT asset belongs only to Apply LUT"));}asset.validate()?;}
        if let Some(image)=&self.mask_image {if !matches!(self.effect,VideoEffect::ImageMask{..}){return Err(Error::Invalid("mask image belongs only to Image Mask/Blend"));}image.validate()?;}
        if let Some(name) = &self.name {
            crate::validate_name(name)?;
        }
        self.effect.validate()
    }
}

impl Collection {
    /// Codec-only owned migration for native schemas1/2. Their source stacks
    /// were displayed top-to-bottom but evaluated bottom-to-top.
    pub fn upgrade_legacy_filter_order(mut self) -> Result<Self, Error> {
        self.validate_with_legacy_filters(true)?;
        for source in &mut self.sources {
            source.video_filters.reverse();
            for filter in &mut source.video_filters {
                if let VideoEffect::ColorCorrection { gamma, .. } = &mut filter.effect {
                    *gamma = f64::from(*gamma as f32);
                }
            }
            source.audio_filters.reverse();
        }
        Ok(self)
    }
    pub(super) fn validate_filter_metadata(
        &self,
        source: &crate::Source,
        legacy: bool,
        check: &mut impl FnMut(Id) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut names = std::collections::HashSet::new();
        for filter in &source.video_filters {
            filter.validate()?;
            match filter.id {
                Some(id) => check(id.entity())?,
                None if legacy => {}
                None => {
                    return Err(Error::Invalid(
                        "canonical video filter has no issued identity",
                    ));
                }
            }
            match &filter.name {
                Some(name) => {
                    if !names.insert(name.as_str()) {
                        return Err(Error::Invalid("duplicate video filter name"));
                    }
                }
                None if legacy => {}
                None => return Err(Error::Invalid("canonical video filter has no name")),
            }
        }
        Ok(())
    }
    pub(super) fn set_video_filters(
        &mut self,
        source: Id,
        mut filters: Vec<VideoFilter>,
    ) -> Result<(), Error> {
        if filters.len() > MAX_VIDEO_FILTERS {
            return Err(Error::Limit("video filter stack exceeds 32 effects"));
        }
        let original = self.source(source).ok_or(Error::NotFound(source))?;
        let mut ids = std::collections::HashSet::new();
        for filter in &mut filters {
            filter.validate()?;
            if let Some(id) = filter.id {
                if !ids.insert(id) || !original.video_filters.iter().any(|old| old.id == Some(id)) {
                    return Err(Error::Invalid(
                        "video filter identity is duplicate or does not belong to this source",
                    ));
                }
                if filter.name.is_none() {
                    filter.name = original
                        .video_filters
                        .iter()
                        .find(|old| old.id == Some(id))
                        .and_then(|old| old.name.clone());
                }
            }
        }
        assign_names(&mut filters)?;
        for filter in &mut filters {
            if filter.id.is_none() {
                filter.id = Some(VideoFilterId(self.issue()?));
            }
        }
        self.source_mut(source)?.video_filters = filters.into_boxed_slice().into_vec();
        Ok(())
    }
    pub(super) fn migrate_video_filters(&mut self) -> Result<(), Error> {
        self.validate_with_legacy_filters(true)?;
        for source_index in 0..self.sources.len() {
            assign_names(&mut self.sources[source_index].video_filters)?;
            for index in 0..self.sources[source_index].video_filters.len() {
                if self.sources[source_index].video_filters[index].id.is_none() {
                    let id = VideoFilterId(self.issue()?);
                    self.sources[source_index].video_filters[index].id = Some(id);
                }
            }
        }
        Ok(())
    }
}
fn assign_names(filters: &mut [VideoFilter]) -> Result<(), Error> {
    let mut reserved = std::collections::HashSet::new();
    for filter in filters.iter() {
        filter.validate()?;
        if let Some(name) = &filter.name {
            if !reserved.insert(name.clone()) {
                return Err(Error::Invalid("duplicate video filter name"));
            }
        }
    }
    for filter in filters.iter_mut().filter(|f| f.name.is_none()) {
        let base = filter.effect.name();
        let mut chosen = None;
        for index in 1..=MAX_VIDEO_FILTERS + 1 {
            let name = if index == 1 {
                base.to_owned()
            } else {
                format!("{base} {index}")
            };
            if reserved.insert(name.clone()) {
                chosen = Some(name);
                break;
            }
        }
        filter.name = Some(chosen.ok_or(Error::Limit("video filter name generation exhausted"))?);
    }
    Ok(())
}

#[cfg(test)]
#[path = "video_filters/identity_tests.rs"]
mod identity_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_percent_ranges_and_unknown_fields_are_enforced_on_disabled_filters_too() {
        for effect in [
            VideoEffect::color_key(),
            VideoEffect::brightness_contrast(),
            VideoEffect::spill_suppression(),
        ] {
            assert!(VideoFilter::new(effect).is_ok());
        }
        for value in [f32::NAN, f32::INFINITY, -0.01, 100.01] {
            let mut effect = VideoEffect::color_key();
            if let VideoEffect::LinearColorKey { tolerance, .. } = &mut effect {
                *tolerance = value;
            }
            assert!(
                VideoFilter {
                    person_interval_ms:None,
                    lut_asset:None,
                    mask_image:None,
                    id: None,
                    name: None,
                    enabled: false,
                    effect
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn stack_is_shared_undoable_validated_and_never_mutates_audio_only_sources() {
        use crate::{AudioCaptureBinding, AudioFlow, AudioTarget, Command, Session};
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddColorSource {
                scene,
                name: "Screen".into(),
                rgba: [0, 255, 0, 255],
            })
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        session
            .apply(Command::AddExistingSource { scene, source })
            .unwrap();
        let before = session.snapshot();
        let filters = vec![VideoFilter::new(VideoEffect::color_key()).unwrap()];
        session
            .apply(Command::SetSourceVideoFilters {
                source,
                filters: filters.clone(),
            })
            .unwrap();
        let published = session
            .snapshot()
            .source(source)
            .unwrap()
            .video_filters()
            .to_vec();
        assert_eq!(published[0].effect, filters[0].effect);
        assert!(published[0].id().is_some());
        assert_eq!(published[0].name(), filters[0].effect.name());
        assert_eq!(session.snapshot().scene(scene).unwrap().items().len(), 2);
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().sources(), before.sources());
        session.apply(Command::Redo).unwrap();
        assert_eq!(
            session.snapshot().source(source).unwrap().video_filters(),
            published
        );
        let current = session.snapshot();
        assert!(
            session
                .apply(Command::SetSourceVideoFilters {
                    source,
                    filters: vec![filters[0].clone(); MAX_VIDEO_FILTERS + 1]
                })
                .is_err()
        );
        assert!(std::sync::Arc::ptr_eq(&current, &session.snapshot()));
        session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: "Audio".into(),
                binding: AudioCaptureBinding::new(
                    AudioFlow::Input,
                    AudioTarget::Default,
                    false,
                    false,
                )
                .unwrap(),
            })
            .unwrap();
        let audio = session
            .snapshot()
            .sources()
            .iter()
            .find(|source| source.name() == "Audio")
            .unwrap()
            .id();
        assert!(
            session
                .apply(Command::SetSourceVideoFilters {
                    source: audio,
                    filters
                })
                .is_err()
        );
    }
}

#[cfg(test)]
mod keyer_tests {
    use super::*;
    #[test]
    fn keyer_defaults_are_complete_and_invalid_authored_values_reject_atomically() {
        use crate::{Command, Session};
        let mut session = Session::new();
        session
            .apply(Command::AddColorSource {
                scene: session.preview(),
                name: "Green".into(),
                rgba: [0, 255, 0, 255],
            })
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        for effect in [VideoEffect::color_key_v2(), VideoEffect::chroma_key_v2()] {
            assert_eq!(effect.plugin_id(), OBS_VIDEO_FILTERS_PLUGIN);
            session
                .apply(Command::SetSourceVideoFilters {
                    source,
                    filters: vec![VideoFilter::new(effect).unwrap()],
                })
                .unwrap();
        }
        let before = session.snapshot();
        let mut bad = before.source(source).unwrap().video_filters()[0].clone();
        if let VideoEffect::ChromaKey { spill, .. } = &mut bad.effect {
            *spill = 1001;
        }
        assert!(
            session
                .apply(Command::SetSourceVideoFilters {
                    source,
                    filters: vec![bad]
                })
                .is_err()
        );
        assert!(std::sync::Arc::ptr_eq(&before, &session.snapshot()));
        for value in [f64::NAN, f64::INFINITY, -1.00001, 1.00001] {
            let mut effect = VideoEffect::color_key_v2();
            if let VideoEffect::ColorKey { gamma, .. } = &mut effect {
                *gamma = value;
            }
            assert!(VideoFilter::new(effect).is_err());
        }
    }
}

#[cfg(test)]
#[path = "video_filters/person_background_tests.rs"]
mod person_background_tests;
