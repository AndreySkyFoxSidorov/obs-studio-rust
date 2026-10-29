//! UI-independent CPU media primitives. See the module card in ../README.md.

use std::collections::{HashSet, VecDeque};
use std::error::Error;
use std::fmt;

mod obs_scroll;
mod async_video_delay;
mod qpc_media_clock;
pub use qpc_media_clock::{MediaTimeBounds, QpcMediaCorrelation, SampleTimeEstimate};
pub use async_video_delay::{DirectShowClockSnapshot, AsyncDelayBudget, AsyncDelayError, AsyncTransformError, AsyncWorkspacePermit, AsyncVideoDelay, DelayedFrameContext, DelayedVideoFrame, LeasedVideoPixels, SharedVideoFrame, WeakSharedVideoFrame, MAX_ASYNC_VIDEO_DELAY_MS};
pub use obs_scroll::{ScrollSettings,ScrollPhase,ScrollRequirements,ScrollLimits,scroll_requirements,scroll_rgba};
mod color_lut;
pub use color_lut::{ColorLut,LutKind,ColorLutSettings,ColorLutRequirements,ColorLutLimits,color_lut_requirements,apply_color_lut,cube_lut_linear_value,png_lut_linear_value,png_lut_linear_value_16};
mod animation;
mod image_alpha;
mod audio_convert;
mod obs_true_peak;
pub use obs_true_peak::ObsTruePeakMeter;
pub use animation::{
    AnimationClip, AnimationFrame, AnimationLimits, AnimationLoop, AnimationSample,
};
pub use audio_convert::{AudioConversionStats, AudioConversionRequirements, audio_conversion_requirements, AudioConvertLimits, AudioConverter, AudioFormat};
pub mod craft_audio;
/// Pure-Rust original-model RNNoise primitive; application integration is separate.
pub mod rnnoise;
mod plugin_audio;
mod source_channels;
pub use plugin_audio::{PluginAudioLimits, StereoPan, StereoPanSettings, stereo_pan_interleaved};
pub use source_channels::{SourceChannelSettings, SourceChannelTransform};
mod fade;
pub use fade::{crossfade_rgba, fade_audio_gains};
mod obs_scale_filter;
pub use obs_scale_filter::{ScaleFilterPlan,ScaleFilterRequirements};
mod luma_wipe;
pub use luma_wipe::{LumaWipeMask,luma_wipe_on_black};
mod fade_to_color;
pub use fade_to_color::{FadeToColorSettings, fade_to_color_audio_gains, fade_to_color_rgba};
mod manual;
pub use manual::{manual_torque_step, progress_rational};
mod fade_single;
pub use fade_single::fade_single_on_black;
mod item_compositing;
mod source_scale_filtering;
mod frame_resample;
pub use frame_resample::{FrameResamplePlan,FrameResampleRequirements};
pub use item_compositing::{ItemBlendMode,ItemBlendingMethod,SamplingFilter};
mod source_color;
pub use source_color::{SourceBlend, straighten_rgba};
mod color_correction;
mod obs_keyer;
mod obs_luma_sharpen;
mod obs_crop_pad;
pub use obs_crop_pad::CropPadSettings;
pub use obs_luma_sharpen::{LumaKeySettings,SharpenSettings};
mod source_effects;
pub use color_correction::{
    ColorCorrectionCoefficients, color_correction_decode_byte, color_correction_linear_alpha_byte,
    ColorCorrectionInput, ColorCorrectionLimits, ColorCorrectionSettings, PremultipliedLinearFrame,
    color_correct_v2,
};
pub use obs_keyer::{ChromaKeySettings, ChromaKeyType, ColorKeySettings, ColorKeyType};
pub use source_effects::{
    SourceEffectFrame, SourceEffectRequirements, SourceEffectsLimits, SourceVideoEffect, MAX_SOURCE_VIDEO_EFFECTS,
    source_effect_requirements,source_effect_requirements_for_dimensions, source_video_effects,
};
mod person_matte;
pub use person_matte::{
    PERSON_MASK_LEN, PERSON_MATTE_EDGE, PERSON_RGB_LEN, PersonMatteLimits, apply_person_matte,
    prepare_person_rgb,
};
mod source_transition;
pub use source_transition::crossfade_rgba_with_blend;
mod directional;
pub use directional::{
    Direction, DirectionalTransition, directional_audio_gains, directional_rgba,directional_rgba_with_blend,
};
mod camera_color;
mod planar_frame;
pub use planar_frame::{Chroma420Halo, Chroma420Vertical, Planar420Layout, ingest_planar_420, ingest_planar_420_sited, ingest_planar_420_with_halo};
mod recording_color;
pub use recording_color::recording_nv12;
mod packed_frame;
pub use camera_color::{
    CameraFrameLayout, CameraMatrix, CameraPixelFormat, CameraRange, ingest_camera_frame,
};
pub use packed_frame::{
    AlphaMode, PackedFrameLayout, PackedFrameLimits, PackedPixelFormat, RowOrder,
    ingest_packed_frame,
};

const MAX_EDGE: u32 = 8192;
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    InvalidDimensions,
    InvalidBuffer,
    InvalidOpacity,
    InvalidFrameRate,
    Overflow,
    BudgetExceeded,
    StaleFrame,
    Closed,
    InvalidAudio,
    InvalidTransform,
    InvalidUv,
    InvalidAnimation,
    SharedResource,
    InvalidTransition,
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Error for MediaError {}

/// Position in a stream's time domain, in integer nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct MediaTime(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRate {
    numerator: u32,
    denominator: u32,
}

impl FrameRate {
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, MediaError> {
        if numerator == 0 || denominator == 0 {
            return Err(MediaError::InvalidFrameRate);
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    pub fn numerator(self) -> u32 {
        self.numerator
    }
    pub fn denominator(self) -> u32 {
        self.denominator
    }

    /// Round down only after calculating the complete rational position.
    pub fn timestamp(self, frame: u64) -> Result<MediaTime, MediaError> {
        let ticks = u128::from(frame)
            .checked_mul(u128::from(self.denominator))
            .and_then(|value| value.checked_mul(1_000_000_000))
            .ok_or(MediaError::Overflow)?
            / u128::from(self.numerator);
        Ok(MediaTime(
            u64::try_from(ticks).map_err(|_| MediaError::Overflow)?,
        ))
    }
}

fn image_bytes(width: u32, height: u32) -> Result<usize, MediaError> {
    if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE {
        return Err(MediaError::InvalidDimensions);
    }
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(MediaError::Overflow)?;
    if bytes > MAX_FRAME_BYTES {
        return Err(MediaError::BudgetExceeded);
    }
    Ok(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaFrame {
    width: u32,
    height: u32,
    pixels: Box<[u8]>,
}


impl RgbaFrame {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, MediaError> {
        if pixels.len() != image_bytes(width, height)? {
            return Err(MediaError::InvalidBuffer);
        }
        // A length-valid Vec can retain an arbitrarily large allocation. Strip
        // spare capacity at this ownership boundary so queue accounting is exact.
        Ok(Self {
            width,
            height,
            pixels: pixels.into_boxed_slice(),
        })
    }

    pub fn solid(width: u32, height: u32, color: [u8; 4]) -> Result<Self, MediaError> {
        let bytes = image_bytes(width, height)?;
        let mut pixels = vec![0; bytes];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
        Ok(Self {
            width,
            height,
            pixels: pixels.into_boxed_slice(),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn byte_len(&self) -> usize {
        self.pixels.len()
    }
    /// Reverse complete RGBA rows in an exclusively owned raster. This moves
    /// bytes only: RGB transfer and alpha representation remain unchanged.
    pub fn flip_vertically(&mut self) {
        let stride = self.width as usize * 4;
        for row in 0..self.height as usize / 2 {
            let top = row * stride;
            let bottom = (self.height as usize - 1 - row) * stride;
            let (before, after) = self.pixels.split_at_mut(bottom);
            before[top..top + stride].swap_with_slice(&mut after[..stride]);
        }
    }
    /// One fallible oriented copy for callers that have already admitted both
    /// old and new image leases. Never uses Arc::make_mut or a hidden clone.
    pub fn try_flipped_vertically(&self) -> Result<Self, MediaError> {
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(self.byte_len()).map_err(|_| MediaError::BudgetExceeded)?;
        for row in self.pixels.chunks_exact(self.width as usize * 4).rev() {
            pixels.extend_from_slice(row);
        }
        Self::new(self.width, self.height, pixels)
    }

}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Layer<'a> {
    pub image: &'a RgbaFrame,
    pub destination: Rect,
    pub opacity: f32,
}

impl<'a> Layer<'a> {
    pub fn new(image: &'a RgbaFrame, destination: Rect, opacity: f32) -> Self {
        Self {
            image,
            destination,
            opacity,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CompositeLimits {
    pub max_layers: usize,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_pixel_visits: u64,
}

impl Default for CompositeLimits {
    fn default() -> Self {
        Self {
            max_layers: 256,
            max_input_bytes: 512 * 1024 * 1024,
            max_output_bytes: 64 * 1024 * 1024,
            max_pixel_visits: 64 * 1024 * 1024,
        }
    }
}

fn clipped_bounds(rect: Rect, width: u32, height: u32) -> (u32, u32, u32, u32) {
    let x0 = i64::from(rect.x).clamp(0, i64::from(width));
    let y0 = i64::from(rect.y).clamp(0, i64::from(height));
    let x1 = (i64::from(rect.x) + i64::from(rect.width)).clamp(x0, i64::from(width));
    let y1 = (i64::from(rect.y) + i64::from(rect.height)).clamp(y0, i64::from(height));
    (x0 as u32, y0 as u32, x1 as u32, y1 as u32)
}

/// Composite bottom-to-top with straight alpha and nearest-neighbor scaling.
/// Validates all inputs and budgets before allocating or writing output.
pub fn composite(
    width: u32,
    height: u32,
    background: [u8; 4],
    layers: &[Layer<'_>],
    limits: CompositeLimits,
) -> Result<RgbaFrame, MediaError> {
    if layers.len() > limits.max_layers || image_bytes(width, height)? > limits.max_output_bytes {
        return Err(MediaError::BudgetExceeded);
    }
    let mut input_bytes = 0usize;
    let mut visits = u64::from(width) * u64::from(height);
    for layer in layers {
        if !layer.opacity.is_finite() || !(0.0..=1.0).contains(&layer.opacity) {
            return Err(MediaError::InvalidOpacity);
        }
        if layer.destination.width == 0 || layer.destination.height == 0 {
            return Err(MediaError::InvalidDimensions);
        }
        input_bytes = input_bytes
            .checked_add(layer.image.byte_len())
            .ok_or(MediaError::Overflow)?;
        let (x0, y0, x1, y1) = clipped_bounds(layer.destination, width, height);
        visits = visits
            .checked_add(u64::from(x1 - x0) * u64::from(y1 - y0))
            .ok_or(MediaError::Overflow)?;
    }
    if input_bytes > limits.max_input_bytes || visits > limits.max_pixel_visits {
        return Err(MediaError::BudgetExceeded);
    }
    let mut output = RgbaFrame::solid(width, height, background)?;
    for layer in layers {
        if layer.opacity == 0.0 {
            continue;
        }
        let rect = layer.destination;
        let (x0, y0, x1, y1) = clipped_bounds(rect, width, height);
        for y in y0..y1 {
            let sy = ((i64::from(y) - i64::from(rect.y)) as u64 * u64::from(layer.image.height)
                / u64::from(rect.height)) as usize;
            for x in x0..x1 {
                let sx = ((i64::from(x) - i64::from(rect.x)) as u64 * u64::from(layer.image.width)
                    / u64::from(rect.width)) as usize;
                let source_index = (sy * layer.image.width as usize + sx) * 4;
                let destination_index = (y as usize * width as usize + x as usize) * 4;
                let source = &layer.image.pixels[source_index..source_index + 4];
                let destination = &mut output.pixels[destination_index..destination_index + 4];
                let sa = f64::from(source[3]) / 255.0 * f64::from(layer.opacity);
                let da = f64::from(destination[3]) / 255.0;
                let alpha = sa + da * (1.0 - sa);
                for channel in 0..3 {
                    let premultiplied = f64::from(source[channel]) * sa
                        + f64::from(destination[channel]) * da * (1.0 - sa);
                    destination[channel] = if alpha == 0.0 {
                        0
                    } else {
                        (premultiplied / alpha).round().clamp(0.0, 255.0) as u8
                    };
                }
                destination[3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok(output)
}

/// Maps local unit coordinates to destination pixels:
/// `x = xx*u + xy*v + tx`, `y = yx*u + yy*v + ty`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2 {
    pub xx: f64,
    pub xy: f64,
    pub yx: f64,
    pub yy: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Affine2 {
    pub const IDENTITY: Self = Self {
        xx: 1.0,
        xy: 0.0,
        yx: 0.0,
        yy: 1.0,
        tx: 0.0,
        ty: 0.0,
    };
}

/// Positive-area normalized source bounds, inclusive left/top and exclusive
/// right/bottom. Local coordinates always retain their original orientation;
/// reflections belong in `Affine2`, not in reversed UV bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UvRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl UvRect {
    pub const FULL: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };
}

/// Exact native source-pixel window. Reflections remain in `Affine2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TexelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy)]
enum SourceWindow {
    Uv(UvRect),
    Texels(TexelRect),
}

#[derive(Debug, Clone, Copy)]
pub struct AffineLayer<'a> {
    pub image: &'a RgbaFrame,
    pub transform: Affine2,
    pub opacity: f32,
    window: SourceWindow,
    blend_mode: SourceBlend,
    item_blend_mode: ItemBlendMode,
    item_blending_method: ItemBlendingMethod,
    sampling_filter: SamplingFilter,
    sampling_scope:source_scale_filtering::Scope,
}

impl<'a> AffineLayer<'a> {
    pub fn new(image: &'a RgbaFrame, transform: Affine2, uv: UvRect, opacity: f32) -> Self {
        Self {
            image,
            transform,
            opacity,
            window: SourceWindow::Uv(uv),
            blend_mode: SourceBlend::Encoded,
            item_blend_mode: ItemBlendMode::Normal,
            item_blending_method: ItemBlendingMethod::Default,
            sampling_filter: SamplingFilter::Nearest,
            sampling_scope:source_scale_filtering::Scope::SceneItem,
        }
    }

    pub fn from_texels(
        image: &'a RgbaFrame,
        transform: Affine2,
        window: TexelRect,
        opacity: f32,
    ) -> Self {
        Self {
            image,
            transform,
            opacity,
            window: SourceWindow::Texels(window),
            blend_mode: SourceBlend::Encoded,
            item_blend_mode: ItemBlendMode::Normal,
            item_blending_method: ItemBlendingMethod::Default,
            sampling_filter: SamplingFilter::Nearest,
            sampling_scope:source_scale_filtering::Scope::SceneItem,
        }
    }

    pub(crate) fn with_global_output_sampling(mut self,width:u32,height:u32)->Self {
        self.sampling_scope=source_scale_filtering::Scope::GlobalOutput {width,height};self
    }
    pub fn with_item_compositing(mut self,mode:ItemBlendMode,method:ItemBlendingMethod,sampling:SamplingFilter)->Self {
        self.item_blend_mode=mode;self.item_blending_method=method;self.sampling_filter=sampling;self
    }
    pub fn requires_item_compositing(&self)->bool {
        self.item_blend_mode!=ItemBlendMode::Normal || self.item_blending_method!=ItemBlendingMethod::Default || self.sampling_filter!=SamplingFilter::Nearest
    }
    pub fn with_blend_mode(mut self, mode: SourceBlend) -> Self {
        self.blend_mode = mode;
        self
    }
    pub fn blend_mode(&self) -> SourceBlend {
        self.blend_mode
    }

    fn validation_uv(&self) -> UvRect {
        match self.window {
            SourceWindow::Uv(uv) => uv,
            SourceWindow::Texels(_) => UvRect::FULL,
        }
    }

    fn sampling_window(&self) -> Result<SamplingWindow, MediaError> {
        match self.window {
            SourceWindow::Texels(rect) => {
                if rect.width == 0
                    || rect.height == 0
                    || u64::from(rect.x) + u64::from(rect.width) > u64::from(self.image.width)
                    || u64::from(rect.y) + u64::from(rect.height) > u64::from(self.image.height)
                {
                    return Err(MediaError::InvalidUv);
                }
                Ok(SamplingWindow::Texels(rect))
            }
            SourceWindow::Uv(uv) => {
                validate_uv(uv)?;
                Ok(SamplingWindow::Uv {
                    uv,
                    last_x: ((uv.right * f64::from(self.image.width)).ceil() as usize - 1)
                        .min(self.image.width as usize - 1),
                    last_y: ((uv.bottom * f64::from(self.image.height)).ceil() as usize - 1)
                        .min(self.image.height as usize - 1),
                })
            }
        }
    }
}

enum SamplingWindow {
    Texels(TexelRect),
    Uv {
        uv: UvRect,
        last_x: usize,
        last_y: usize,
    },
}
impl SamplingWindow {
    fn sample(&self, u: f64, v: f64, image: &RgbaFrame) -> (usize, usize) {
        match self {
            Self::Texels(rect) => {
                let x = ((u * f64::from(rect.width)).floor() as u32).min(rect.width - 1);
                let y = ((v * f64::from(rect.height)).floor() as u32).min(rect.height - 1);
                ((rect.x + x) as usize, (rect.y + y) as usize)
            }
            Self::Uv { uv, last_x, last_y } => {
                let source_u = uv.left + u * (uv.right - uv.left);
                let source_v = uv.top + v * (uv.bottom - uv.top);
                (
                    ((source_u * f64::from(image.width)).floor() as usize).min(*last_x),
                    ((source_v * f64::from(image.height)).floor() as usize).min(*last_y),
                )
            }
        }
    }
}

struct NormalizedSolve {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    scale: f64,
    determinant: f64,
}

struct AffinePlan {
    inverse_xx: f64,
    inverse_xy: f64,
    inverse_yx: f64,
    inverse_yy: f64,
    normalized: Option<NormalizedSolve>,
    bounds: (u32, u32, u32, u32),
}

impl AffinePlan {
    fn local(&self, dx: f64, dy: f64) -> (f64, f64) {
        if let Some(solve) = &self.normalized {
            // Divide the displacement BEFORE solving rather than multiplying
            // a rounded reciprocal: 49 / 49 stays exactly one at an excluded
            // edge, whereas 49 * (1 / 49) can fall below one and admit it.
            let x = dx / solve.scale;
            let y = dy / solve.scale;
            (
                (solve.d * x - solve.b * y) / solve.determinant,
                (solve.a * y - solve.c * x) / solve.determinant,
            )
        } else {
            // Fallback for representable inverses of extreme mixed axes whose
            // common normalization underflows (e.g. 1e308 by 1e-308).
            (
                self.inverse_xx * dx + self.inverse_xy * dy,
                self.inverse_yx * dx + self.inverse_yy * dy,
            )
        }
    }
}

fn validate_uv(uv: UvRect) -> Result<(), MediaError> {
    if ![uv.left, uv.top, uv.right, uv.bottom]
        .iter()
        .all(|value| value.is_finite())
        || uv.left < 0.0
        || uv.top < 0.0
        || uv.right > 1.0
        || uv.bottom > 1.0
        || uv.left >= uv.right
        || uv.top >= uv.bottom
    {
        return Err(MediaError::InvalidUv);
    }
    Ok(())
}

fn prepare_affine(
    transform: Affine2,
    uv: UvRect,
    width: u32,
    height: u32,
) -> Result<AffinePlan, MediaError> {
    validate_uv(uv)?;
    if ![
        transform.xx,
        transform.xy,
        transform.yx,
        transform.yy,
        transform.tx,
        transform.ty,
    ]
    .iter()
    .all(|value| value.is_finite())
    {
        return Err(MediaError::InvalidTransform);
    }
    // Normalizing first avoids overflow/underflow in the usual scale/rotation
    // determinant. There is deliberately no epsilon that erases tiny geometry.
    let scale = transform
        .xx
        .abs()
        .max(transform.xy.abs())
        .max(transform.yx.abs())
        .max(transform.yy.abs());
    if scale == 0.0 {
        return Err(MediaError::InvalidTransform);
    }
    let (a, b, c, d) = (
        transform.xx / scale,
        transform.xy / scale,
        transform.yx / scale,
        transform.yy / scale,
    );
    let determinant = a * d - b * c;
    let mut inverse = [
        d / determinant / scale,
        -b / determinant / scale,
        -c / determinant / scale,
        a / determinant / scale,
    ];
    let mut normalized = Some(NormalizedSolve {
        a,
        b,
        c,
        d,
        scale,
        determinant,
    });
    if determinant == 0.0 || !inverse.iter().all(|value| value.is_finite()) {
        normalized = None;
        // Extremely different axis magnitudes can underflow during uniform
        // normalization even though the ordinary determinant/inverse fits f64.
        // Fall back without introducing an arbitrary minimum accepted scale.
        let determinant = transform.xx * transform.yy - transform.xy * transform.yx;
        inverse = [
            transform.yy / determinant,
            -transform.xy / determinant,
            -transform.yx / determinant,
            transform.xx / determinant,
        ];
        if determinant == 0.0
            || !determinant.is_finite()
            || !inverse.iter().all(|value| value.is_finite())
        {
            return Err(MediaError::InvalidTransform);
        }
    }
    let corners = [
        (transform.tx, transform.ty),
        (transform.xx + transform.tx, transform.yx + transform.ty),
        (transform.xy + transform.tx, transform.yy + transform.ty),
        (
            transform.xx + transform.xy + transform.tx,
            transform.yx + transform.yy + transform.ty,
        ),
    ];
    if !corners.iter().all(|(x, y)| x.is_finite() && y.is_finite()) {
        return Err(MediaError::InvalidTransform);
    }
    let (mut min_x, mut min_y) = corners[0];
    let (mut max_x, mut max_y) = corners[0];
    for (x, y) in corners.into_iter().skip(1) {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    // Clip before conversion. AABB loops are bounded by actual canvas pixels,
    // regardless of offscreen positions or authored scale.
    let bounds = (
        min_x.floor().clamp(0.0, f64::from(width)) as u32,
        min_y.floor().clamp(0.0, f64::from(height)) as u32,
        max_x.ceil().clamp(0.0, f64::from(width)) as u32,
        max_y.ceil().clamp(0.0, f64::from(height)) as u32,
    );
    Ok(AffinePlan {
        inverse_xx: inverse[0],
        inverse_xy: inverse[1],
        inverse_yx: inverse[2],
        inverse_yy: inverse[3],
        normalized,
        bounds,
    })
}

fn blend_affine_pixel(destination: &mut [u8], source: &[u8], opacity: f32, mode: SourceBlend) {
    if mode != SourceBlend::Encoded {
        source_color::blend(destination, source, opacity, mode);
        return;
    }
    let source_alpha = f64::from(source[3]) / 255.0 * f64::from(opacity);
    let destination_alpha = f64::from(destination[3]) / 255.0;
    let alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
    for channel in 0..3 {
        let premultiplied = f64::from(source[channel]) * source_alpha
            + f64::from(destination[channel]) * destination_alpha * (1.0 - source_alpha);
        destination[channel] = if alpha == 0.0 {
            0
        } else {
            (premultiplied / alpha).round().clamp(0.0, 255.0) as u8
        };
    }
    destination[3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// Conservative admission for the exact sampler selected by each affine layer.
/// Base visits retain clear/covered-pixel work; extra visits are texture taps.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct AffineSampleRequirements {
    pub base_pixel_visits:u64,
    pub extra_sample_visits:u64,
    pub total_sample_visits:u64,
}
pub fn affine_sample_requirements(width:u32,height:u32,layers:&[AffineLayer<'_>])->Result<AffineSampleRequirements,MediaError>{
    image_bytes(width,height)?;
    let mut base=u64::from(width)*u64::from(height);let mut extra=0u64;
    for layer in layers {
        if !layer.opacity.is_finite() || !(0.0..=1.0).contains(&layer.opacity) {return Err(MediaError::InvalidOpacity);}
        let plan=prepare_affine(layer.transform,layer.validation_uv(),width,height)?;
        let window=layer.sampling_window()?;
        let sampler=source_scale_filtering::Prepared::new(layer,&window,&plan)?;
        let cost=sampler.map_or(if layer.sampling_filter==SamplingFilter::Bilinear {4}else{1},|s|s.cost);
        let (x0,y0,x1,y1)=plan.bounds;
        let covered=u64::from(x1-x0)*u64::from(y1-y0);
        base=base.checked_add(covered).ok_or(MediaError::Overflow)?;
        extra=extra.checked_add(covered.checked_mul(cost-1).ok_or(MediaError::Overflow)?).ok_or(MediaError::Overflow)?;
    }
    Ok(AffineSampleRequirements {base_pixel_visits:base,extra_sample_visits:extra,total_sample_visits:base.checked_add(extra).ok_or(MediaError::Overflow)?})
}

/// Inverse-map destination pixel centers through an affine transform and crop.
/// Default nearest-neighbor sampling and encoded-space straight-alpha blending
/// match `composite`. Layers may request bilinear, bicubic, Lanczos or bounded
/// area sampling, and separate
/// OBS item color/alpha blend operations in linear or encoded SDR. Layers can
/// also select source-aware SDR sRGB policies; unlike
/// the integer rectangle API, subpixel geometry is retained.
/// All validation and cumulative budgets are checked before output allocation.
/// Input bytes count unique RGBA allocations; repeated references still count
/// their clipped pixel visits and layers, but do not retain another pixel buffer.
pub fn composite_affine(
    width: u32,
    height: u32,
    background: [u8; 4],
    layers: &[AffineLayer<'_>],
    limits: CompositeLimits,
) -> Result<RgbaFrame, MediaError> {
    composite_affine_target(width, height, background, layers, limits, false)
}

/// A transparent SDR offscreen target whose RGB bytes encode premultiplied
/// linear light. Feed the result through SourceBlend::PremultipliedLinearSrgb.
pub fn composite_affine_premultiplied(
    width: u32,
    height: u32,
    layers: &[AffineLayer<'_>],
    limits: CompositeLimits,
) -> Result<RgbaFrame, MediaError> {
    composite_affine_target(width, height, [0; 4], layers, limits, true)
}

/// Encoded premultiplied-linear SDR target with an explicitly typed initial
/// framebuffer. Used by item blend operations before opaque Program flattening.
pub fn composite_affine_premultiplied_background(
    width:u32,height:u32,background:[u8;4],layers:&[AffineLayer<'_>],limits:CompositeLimits,
)->Result<RgbaFrame,MediaError> {
    composite_affine_target(width,height,background,layers,limits,true)
}

/// Flatten a premultiplied-linear scene over black in place, including hidden
/// energy whose UNORM alpha rounded to zero. No additional pixel allocation.
pub fn flatten_premultiplied_on_black(frame:&mut RgbaFrame) {
    for pixel in frame.pixels.chunks_exact_mut(4) {pixel[3]=255;}
}

fn composite_affine_target(
    width: u32,
    height: u32,
    background: [u8; 4],
    layers: &[AffineLayer<'_>],
    limits: CompositeLimits,
    premultiplied: bool,
) -> Result<RgbaFrame, MediaError> {
    composite_affine_target_policy(
        width,
        height,
        background,
        layers,
        limits,
        premultiplied,
        true,
    )
}

// Keep the validated generic pixel loop as the exact test oracle. The public
// path may only select a faster mapping after the same admission pass succeeds.
fn composite_affine_target_policy(
    width: u32,
    height: u32,
    background: [u8; 4],
    layers: &[AffineLayer<'_>],
    limits: CompositeLimits,
    premultiplied: bool,
    rows: bool,
) -> Result<RgbaFrame, MediaError> {
    if layers.len() > limits.max_layers || image_bytes(width, height)? > limits.max_output_bytes {
        return Err(MediaError::BudgetExceeded);
    }
    let required=affine_sample_requirements(width,height,layers)?;
    let mut input_bytes=0usize;let mut allocations=HashSet::new();
    for layer in layers {
        if allocations.insert(layer.image.pixels.as_ptr()) {
            input_bytes=input_bytes.checked_add(layer.image.byte_len()).ok_or(MediaError::Overflow)?;
        }
    }
    if input_bytes>limits.max_input_bytes || required.total_sample_visits>limits.max_pixel_visits {return Err(MediaError::BudgetExceeded);}
    let mut output = RgbaFrame::solid(width, height, background)?;
    // At most MAX_EDGE u16 indices (16 KiB); reused between eligible layers.
    // It is bounded algorithm metadata, separate from admitted input/output RGBA.
    let mut row_indices = Vec::new();
    for layer in layers {
        if layer.opacity == 0.0 {
            continue;
        }
        // Repeat constant-size preparation instead of retaining a per-layer
        // allocation; inputs are immutably borrowed for both validation passes.
        let plan = prepare_affine(layer.transform, layer.validation_uv(), width, height)?;
        let (x0, y0, x1, y1) = plan.bounds;
        let sampling = layer.sampling_window()?;
        let scale_sampling=source_scale_filtering::Prepared::new(layer,&sampling,&plan)?;
        if rows
            && composite_axis_rows(
                &mut output,
                layer,
                &plan,
                &sampling,
                premultiplied,
                &mut row_indices,
            )?
        {
            continue;
        }
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = f64::from(x) + 0.5 - layer.transform.tx;
                let dy = f64::from(y) + 0.5 - layer.transform.ty;
                let (u, v) = plan.local(dx, dy);
                if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                    continue;
                }
                let destination_index = (y as usize * width as usize + x as usize) * 4;
                if layer.requires_item_compositing() {
                    let sample=if let Some(sampler)=&scale_sampling {sampler.sample(layer,u,v)}else{item_compositing::sample(layer,&sampling,u,v)};
                    item_compositing::blend(&mut output.pixels[destination_index..destination_index+4],sample,layer,premultiplied);
                    continue;
                }
                let (sx, sy) = sampling.sample(u, v, layer.image);
                let source_index = (sy * layer.image.width as usize + sx) * 4;
                if premultiplied {
                    source_color::blend_premultiplied(
                        &mut output.pixels[destination_index..destination_index + 4],
                        &layer.image.pixels[source_index..source_index + 4],
                        layer.opacity,
                        layer.blend_mode,
                    );
                } else {
                    blend_affine_pixel(
                        &mut output.pixels[destination_index..destination_index + 4],
                        &layer.image.pixels[source_index..source_index + 4],
                        layer.opacity,
                        layer.blend_mode,
                    );
                }
            }
        }
    }
    Ok(output)
}

const NO_ROW_SAMPLE: u16 = u16::MAX;
const MAX_AXIS_ROW_INDICES: usize = 8192;

fn full_affine_window(layer: &AffineLayer<'_>) -> bool {
    match layer.window {
        SourceWindow::Uv(uv) => {
            uv.left == 0.0 && uv.top == 0.0 && uv.right == 1.0 && uv.bottom == 1.0
        }
        SourceWindow::Texels(rect) => {
            rect.x == 0
                && rect.y == 0
                && rect.width == layer.image.width
                && rect.height == layer.image.height
        }
    }
}

fn composite_axis_rows(
    output: &mut RgbaFrame,
    layer: &AffineLayer<'_>,
    plan: &AffinePlan,
    sampling: &SamplingWindow,
    premultiplied: bool,
    indices: &mut Vec<u16>,
) -> Result<bool, MediaError> {
    if layer.requires_item_compositing()
        || layer.transform.xy != 0.0
        || layer.transform.yx != 0.0
        || !full_affine_window(layer)
        || layer.image.width >= u32::from(NO_ROW_SAMPLE)
    {
        return Ok(false);
    }
    let (x0, y0, x1, y1) = plan.bounds;
    if x0 == x1 || y0 == y1 {
        return Ok(true);
    }
    let count = (x1 - x0) as usize;
    if count > MAX_AXIS_ROW_INDICES {
        return Ok(false);
    }
    if indices.capacity() < count {
        indices
            .try_reserve_exact(count - indices.len())
            .map_err(|_| MediaError::BudgetExceeded)?;
    }
    indices.resize(count, NO_ROW_SAMPLE);
    let reference_dx = f64::from(x0) + 0.5 - layer.transform.tx;
    let reference_dy = f64::from(y0) + 0.5 - layer.transform.ty;
    // Use precisely the generic solve and sampling expressions, not rounded
    // reciprocals or incremental u/v arithmetic. Axis cross terms are zero.
    for (offset, source_x) in indices.iter_mut().enumerate() {
        let dx = f64::from(x0) + offset as f64 + 0.5 - layer.transform.tx;
        let u = plan.local(dx, reference_dy).0;
        *source_x = if (0.0..1.0).contains(&u) {
            sampling.sample(u, 0.5, layer.image).0 as u16
        } else {
            NO_ROW_SAMPLE
        };
    }
    let solid = !premultiplied
        && layer.opacity == 1.0
        && layer.image.width == 1
        && layer.image.height == 1
        && layer.image.pixels[3] == 255
        && indices.iter().all(|source| *source == 0);
    let contiguous = !premultiplied
        && layer.opacity == 1.0
        && indices[0] != NO_ROW_SAMPLE
        && indices
            .iter()
            .enumerate()
            .all(|(offset, source)| usize::from(*source) == usize::from(indices[0]) + offset);
    #[cfg(test)]
    AXIS_ROW_STATS.with(|stats| {
        let mut value = stats.get();
        value[0] += 1;
        stats.set(value);
    });
    for y in y0..y1 {
        let dy = f64::from(y) + 0.5 - layer.transform.ty;
        let v = plan.local(reference_dx, dy).1;
        if !(0.0..1.0).contains(&v) {
            continue;
        }
        let source_y = sampling.sample(0.5, v, layer.image).1;
        let destination = ((y as usize * output.width as usize) + x0 as usize) * 4;
        let destination_row = &mut output.pixels[destination..destination + count * 4];
        if solid {
            for pixel in destination_row.chunks_exact_mut(4) {
                pixel.copy_from_slice(&layer.image.pixels[..4]);
            }
            #[cfg(test)]
            AXIS_ROW_STATS.with(|stats| {
                let mut value = stats.get();
                value[1] += 1;
                stats.set(value);
            });
            continue;
        }
        if contiguous {
            let source = (source_y * layer.image.width as usize + usize::from(indices[0])) * 4;
            let source_row = &layer.image.pixels[source..source + count * 4];
            if source_row.chunks_exact(4).all(|pixel| pixel[3] == 255) {
                destination_row.copy_from_slice(source_row);
                #[cfg(test)]
                AXIS_ROW_STATS.with(|stats| {
                    let mut value = stats.get();
                    value[2] += 1;
                    stats.set(value);
                });
                continue;
            }
        }
        for (pixel, source_x) in destination_row
            .chunks_exact_mut(4)
            .zip(indices.iter().copied())
        {
            if source_x == NO_ROW_SAMPLE {
                continue;
            }
            let source = (source_y * layer.image.width as usize + usize::from(source_x)) * 4;
            if premultiplied {
                source_color::blend_premultiplied(
                    pixel,
                    &layer.image.pixels[source..source + 4],
                    layer.opacity,
                    layer.blend_mode,
                );
            } else {
                blend_affine_pixel(
                    pixel,
                    &layer.image.pixels[source..source + 4],
                    layer.opacity,
                    layer.blend_mode,
                );
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
std::thread_local! {static AXIS_ROW_STATS:std::cell::Cell<[usize;3]>=const{std::cell::Cell::new([0;3])};}
#[cfg(test)]
#[path = "affine_rows_tests.rs"]
mod affine_rows_tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamIdentity {
    pub source: u64,
    pub epoch: u64,
    pub settings: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameIdentity {
    pub source: u64,
    pub epoch: u64,
    pub settings: u64,
    pub pts: MediaTime,
}

impl FrameIdentity {
    pub fn stream(self) -> StreamIdentity {
        StreamIdentity {
            source: self.source,
            epoch: self.epoch,
            settings: self.settings,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub identity: FrameIdentity,
    pub image: RgbaFrame,
}

/// Rejected frames remain owned by the caller; no queued frame is discarded.
#[derive(Debug)]
pub struct RejectedFrame {
    pub error: MediaError,
    pub frame: VideoFrame,
}

#[derive(Debug)]
pub struct FrameQueue {
    queue: VecDeque<VideoFrame>,
    max_items: usize,
    max_bytes: usize,
    bytes: usize,
    expected: StreamIdentity,
    closed: bool,
}

impl FrameQueue {
    pub fn new(
        max_items: usize,
        max_bytes: usize,
        expected: StreamIdentity,
    ) -> Result<Self, MediaError> {
        if max_items == 0 || max_bytes == 0 {
            return Err(MediaError::BudgetExceeded);
        }
        Ok(Self {
            queue: VecDeque::new(),
            max_items,
            max_bytes,
            bytes: 0,
            expected,
            closed: false,
        })
    }

    pub fn push(&mut self, frame: VideoFrame) -> Result<(), RejectedFrame> {
        let error = if self.closed {
            Some(MediaError::Closed)
        } else if frame.identity.stream() != self.expected {
            Some(MediaError::StaleFrame)
        } else if self.queue.len() >= self.max_items
            || frame.image.byte_len() > self.max_bytes.saturating_sub(self.bytes)
        {
            Some(MediaError::BudgetExceeded)
        } else {
            None
        };
        if let Some(error) = error {
            return Err(RejectedFrame { error, frame });
        }
        self.bytes += frame.image.byte_len();
        self.queue.push_back(frame);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<VideoFrame> {
        let frame = self.queue.pop_front()?;
        self.bytes -= frame.image.byte_len();
        Some(frame)
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    pub fn queued_bytes(&self) -> usize {
        self.bytes
    }
    pub fn expected(&self) -> StreamIdentity {
        self.expected
    }
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Drain buffers before changing the expected stream, typically after seek.
    /// A closed owner cannot be reopened by resetting a stream.
    pub fn reset(&mut self, expected: StreamIdentity) -> Result<(), MediaError> {
        if self.closed {
            return Err(MediaError::Closed);
        }
        self.queue.clear();
        self.bytes = 0;
        self.expected = expected;
        Ok(())
    }

    pub fn close(&mut self) {
        self.closed = true;
        self.queue.clear();
        self.bytes = 0;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AudioInput<'a> {
    pub samples: &'a [f32],
    pub gain: f32,
    pub muted: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct AudioLimits {
    pub max_inputs: usize,
    pub max_channels: usize,
    pub max_frames: usize,
}

impl Default for AudioLimits {
    fn default() -> Self {
        Self {
            max_inputs: 64,
            max_channels: 8,
            max_frames: 4096,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meter {
    pub peak: f32,
    pub clipped_samples: usize,
}

/// Sum linear gain in f64, hard-clip once. No allocation, I/O or locks.
/// Any validation error leaves `output` unchanged.
pub fn mix_audio(
    inputs: &[AudioInput<'_>],
    output: &mut [f32],
    channels: usize,
    limits: AudioLimits,
) -> Result<Meter, MediaError> {
    if channels == 0
        || channels > limits.max_channels
        || inputs.len() > limits.max_inputs
        || !output.len().is_multiple_of(channels)
        || output.len() / channels > limits.max_frames
    {
        return Err(MediaError::InvalidAudio);
    }
    for input in inputs {
        if input.samples.len() != output.len() || !input.gain.is_finite() || input.gain < 0.0 {
            return Err(MediaError::InvalidAudio);
        }
        if !input.muted && input.samples.iter().any(|sample| !sample.is_finite()) {
            return Err(MediaError::InvalidAudio);
        }
    }
    let mut meter = Meter {
        peak: 0.0,
        clipped_samples: 0,
    };
    for (index, destination) in output.iter_mut().enumerate() {
        let mixed: f64 = inputs
            .iter()
            .filter(|input| !input.muted)
            .map(|input| f64::from(input.samples[index]) * f64::from(input.gain))
            .sum();
        if !(-1.0..=1.0).contains(&mixed) {
            meter.clipped_samples += 1;
        }
        *destination = mixed.clamp(-1.0, 1.0) as f32;
        meter.peak = meter.peak.max(destination.abs());
    }
    Ok(meter)
}

#[cfg(test)]
mod tests {
    #[test]
    fn affine_shared_layers_charge_one_buffer_but_keep_per_layer_work() {
        let image = super::RgbaFrame::solid(1, 1, [255; 4]).unwrap();
        let layer =
            super::AffineLayer::new(&image, super::Affine2::IDENTITY, super::UvRect::FULL, 1.0);
        let limits = super::CompositeLimits {
            max_input_bytes: 4,
            max_pixel_visits: 4,
            ..Default::default()
        };
        assert!(super::composite_affine(1, 1, [0; 4], &[layer, layer, layer], limits).is_ok());
        let copy = image.clone();
        let other =
            super::AffineLayer::new(&copy, super::Affine2::IDENTITY, super::UvRect::FULL, 1.0);
        assert_eq!(
            super::composite_affine(1, 1, [0; 4], &[layer, other], limits),
            Err(super::MediaError::BudgetExceeded)
        );
        let limits = super::CompositeLimits {
            max_pixel_visits: 3,
            ..limits
        };
        assert_eq!(
            super::composite_affine(1, 1, [0; 4], &[layer, layer, layer], limits),
            Err(super::MediaError::BudgetExceeded)
        );
    }
    use super::*;

    fn pattern(width: u32, height: u32) -> RgbaFrame {
        let pixels = (1..=width * height)
            .flat_map(|value| [value as u8, 0, 0, 255])
            .collect();
        RgbaFrame::new(width, height, pixels).unwrap()
    }

    fn pixel_labels(image: &RgbaFrame) -> Vec<u8> {
        image
            .pixels()
            .chunks_exact(4)
            .map(|pixel| pixel[0])
            .collect()
    }

    #[test]
    fn native_texel_crop_never_samples_either_removed_edge_or_excluded_destination_edge() {
        let mut pixels = vec![0; 87 * 4];
        for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(if (11..60).contains(&index) {
                &[0, 255, 0, 255]
            } else {
                &[255, 0, 0, 255]
            });
        }
        let image = RgbaFrame::new(87, 1, pixels).unwrap();
        let crop = TexelRect {
            x: 11,
            y: 0,
            width: 49,
            height: 1,
        };
        let transform = Affine2 {
            xx: 49.0,
            tx: -0.5,
            ..Affine2::IDENTITY
        };
        let layer = AffineLayer::from_texels(&image, transform, crop, 1.0);
        let output =
            composite_affine(64, 1, [0, 0, 0, 255], &[layer], CompositeLimits::default()).unwrap();
        for (x, pixel) in output.pixels().chunks_exact(4).enumerate() {
            assert_eq!(
                pixel,
                if x < 48 {
                    &[0, 255, 0, 255]
                } else {
                    &[0, 0, 0, 255]
                }
            );
        }
        assert_eq!(&output.pixels()[47 * 4..48 * 4], &[0, 255, 0, 255]);
        assert_eq!(&output.pixels()[48 * 4..49 * 4], &[0, 0, 0, 255]);
        let plan = prepare_affine(transform, UvRect::FULL, 64, 1).unwrap();
        assert_eq!(plan.local(49.0, 0.5).0, 1.0);
        // At the included left edge the exact native index is 11, never the
        // cropped-away texel 10 even if a normalized 11/87 representation drifts.
        let transform = Affine2 {
            xx: 49.0,
            tx: 0.5,
            ..Affine2::IDENTITY
        };
        let layer = AffineLayer::from_texels(&image, transform, crop, 1.0);
        let output =
            composite_affine(64, 1, [0, 0, 0, 255], &[layer], CompositeLimits::default()).unwrap();
        assert_eq!(&output.pixels()[..4], &[0, 255, 0, 255]);
        assert_eq!(&output.pixels()[49 * 4..50 * 4], &[0, 0, 0, 255]);
        let sampling = layer.sampling_window().unwrap();
        assert_eq!(sampling.sample(0.0, 0.5, &image), (11, 0));
        assert_eq!(
            sampling.sample(f64::from_bits(1.0f64.to_bits() - 1), 0.5, &image),
            (59, 0)
        );
    }

    #[test]
    fn native_texel_window_reflections_and_invalid_windows_have_pixel_oracles() {
        let image = pattern(4, 2);
        let transform = Affine2 {
            xx: -2.0,
            yy: 2.0,
            tx: 2.0,
            ..Affine2::IDENTITY
        };
        let crop = TexelRect {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        };
        let layer = AffineLayer::from_texels(&image, transform, crop, 1.0);
        let output = composite_affine(2, 2, [0; 4], &[layer], CompositeLimits::default()).unwrap();
        assert_eq!(pixel_labels(&output), [3, 2, 7, 6]);
        for crop in [
            TexelRect { width: 0, ..crop },
            TexelRect { height: 0, ..crop },
            TexelRect {
                x: u32::MAX,
                width: 2,
                ..crop
            },
            TexelRect {
                y: u32::MAX,
                height: 2,
                ..crop
            },
            TexelRect {
                x: 3,
                width: 2,
                ..crop
            },
            TexelRect {
                y: 1,
                height: 2,
                ..crop
            },
        ] {
            let layer = AffineLayer::from_texels(&image, Affine2::IDENTITY, crop, 1.0);
            assert_eq!(
                composite_affine(1, 1, [0; 4], &[layer], CompositeLimits::default()),
                Err(MediaError::InvalidUv)
            );
        }
    }

    #[test]
    fn affine_rotation_and_reflections_match_patterned_texture_oracles() {
        // A B C      D A
        // D E F  ->  E B  (clockwise ninety degrees)
        //           F C
        let image = pattern(3, 2);
        let rotation = Affine2 {
            xx: 0.0,
            xy: -2.0,
            yx: 3.0,
            yy: 0.0,
            tx: 2.0,
            ty: 0.0,
        };
        let layer = AffineLayer::new(&image, rotation, UvRect::FULL, 1.0);
        let output = composite_affine(2, 3, [0; 4], &[layer], CompositeLimits::default()).unwrap();
        assert_eq!(pixel_labels(&output), [4, 1, 5, 2, 6, 3]);
        let horizontal = Affine2 {
            xx: -3.0,
            xy: 0.0,
            yx: 0.0,
            yy: 2.0,
            tx: 3.0,
            ty: 0.0,
        };
        let layer = AffineLayer::new(&image, horizontal, UvRect::FULL, 1.0);
        let output = composite_affine(3, 2, [0; 4], &[layer], CompositeLimits::default()).unwrap();
        assert_eq!(pixel_labels(&output), [3, 2, 1, 6, 5, 4]);
        let vertical = Affine2 {
            xx: 3.0,
            xy: 0.0,
            yx: 0.0,
            yy: -2.0,
            tx: 0.0,
            ty: 2.0,
        };
        let layer = AffineLayer::new(&image, vertical, UvRect::FULL, 1.0);
        let output = composite_affine(3, 2, [0; 4], &[layer], CompositeLimits::default()).unwrap();
        assert_eq!(pixel_labels(&output), [4, 5, 6, 1, 2, 3]);
    }

    #[test]
    fn affine_crop_maps_selected_texels_and_does_not_bleed_exclusive_edge() {
        let image = pattern(4, 2);
        let crop = UvRect {
            left: 0.25,
            top: 0.0,
            right: 0.75,
            bottom: 1.0,
        };
        let transform = Affine2 {
            xx: 2.0,
            yy: 2.0,
            ..Affine2::IDENTITY
        };
        let layer = AffineLayer::new(&image, transform, crop, 1.0);
        let output = composite_affine(2, 2, [0; 4], &[layer], CompositeLimits::default()).unwrap();
        assert_eq!(pixel_labels(&output), [2, 3, 6, 7]);
        // Here inverse u is the representable value just below 1. Addition in
        // UV space rounds to the exclusive right edge: it must remain cropped.
        let crop = UvRect {
            left: 0.25,
            top: 0.0,
            right: 0.5,
            bottom: 1.0,
        };
        let transform = Affine2 {
            tx: -0.4999999999999999,
            ..Affine2::IDENTITY
        };
        let layer = AffineLayer::new(&image, transform, crop, 1.0);
        let output = composite_affine(1, 1, [0; 4], &[layer], CompositeLimits::default()).unwrap();
        assert_eq!(pixel_labels(&output), [6]);
        let near_one = f64::from_bits(1.0f64.to_bits() - 1);
        let crop = UvRect {
            left: near_one,
            top: 0.0,
            right: 1.0,
            bottom: 1.0,
        };
        let layer = AffineLayer::new(&image, Affine2::IDENTITY, crop, 1.0);
        let output = composite_affine(1, 1, [0; 4], &[layer], CompositeLimits::default()).unwrap();
        assert_eq!(pixel_labels(&output), [8]);
    }

    #[test]
    fn affine_diamond_coverage_and_inverse_corners_match_independent_oracle() {
        let image = pattern(2, 2);
        let transform = Affine2 {
            xx: 2.0,
            xy: -2.0,
            yx: 2.0,
            yy: 2.0,
            tx: 2.0,
            ty: 0.0,
        };
        let layer = AffineLayer::new(&image, transform, UvRect::FULL, 1.0);
        let output = composite_affine(4, 4, [0; 4], &[layer], CompositeLimits::default()).unwrap();
        // In this diamond u=(x-2+y)/4, v=(2-x+y)/4. Half-open u/v
        // includes two top/left boundary centers and excludes bottom/right.
        assert_eq!(
            pixel_labels(&output),
            [0, 1, 1, 0, 3, 3, 2, 2, 0, 4, 4, 0, 0, 0, 0, 0]
        );
        let plan = prepare_affine(transform, UvRect::FULL, 4, 4).unwrap();
        for ((x, y), (u, v)) in [
            ((2.0, 0.0), (0.0, 0.0)),
            ((4.0, 2.0), (1.0, 0.0)),
            ((0.0, 2.0), (0.0, 1.0)),
            ((2.0, 4.0), (1.0, 1.0)),
        ] {
            let dx = x - transform.tx;
            let dy = y - transform.ty;
            assert_eq!(plan.local(dx, dy), (u, v));
        }
    }

    #[test]
    fn affine_composition_preserves_order_opacity_and_straight_alpha() {
        let red = RgbaFrame::solid(1, 1, [255, 0, 0, 128]).unwrap();
        let blue = RgbaFrame::solid(1, 1, [0, 0, 255, 255]).unwrap();
        let layers = [
            AffineLayer::new(&blue, Affine2::IDENTITY, UvRect::FULL, 1.0),
            AffineLayer::new(&red, Affine2::IDENTITY, UvRect::FULL, 1.0),
        ];
        let output = composite_affine(1, 1, [0; 4], &layers, CompositeLimits::default()).unwrap();
        assert_eq!(output.pixels(), &[128, 0, 127, 255]);
        let layer = AffineLayer::new(&blue, Affine2::IDENTITY, UvRect::FULL, 0.5);
        let output = composite_affine(1, 1, [0; 4], &[layer], CompositeLimits::default()).unwrap();
        assert_eq!(output.pixels(), &[0, 0, 255, 128]);
    }

    #[test]
    fn affine_budgets_count_background_and_clipped_aabb_before_composition() {
        let image = pattern(2, 2);
        let transform = Affine2 {
            xx: 2.0,
            xy: -2.0,
            yx: 2.0,
            yy: 2.0,
            tx: 2.0,
            ty: 0.0,
        };
        let layer = AffineLayer::new(&image, transform, UvRect::FULL, 0.0);
        let limits = CompositeLimits {
            max_pixel_visits: 31,
            ..CompositeLimits::default()
        };
        assert_eq!(
            composite_affine(4, 4, [0; 4], &[layer], limits),
            Err(MediaError::BudgetExceeded)
        );
        let limits = CompositeLimits {
            max_pixel_visits: 32,
            ..limits
        };
        assert!(composite_affine(4, 4, [0; 4], &[layer], limits).is_ok());
        let limits = CompositeLimits {
            max_input_bytes: 15,
            ..CompositeLimits::default()
        };
        assert_eq!(
            composite_affine(4, 4, [0; 4], &[layer], limits),
            Err(MediaError::BudgetExceeded)
        );
        let limits = CompositeLimits {
            max_output_bytes: 63,
            ..CompositeLimits::default()
        };
        assert_eq!(
            composite_affine(4, 4, [0; 4], &[layer], limits),
            Err(MediaError::BudgetExceeded)
        );
        let limits = CompositeLimits {
            max_layers: 0,
            ..CompositeLimits::default()
        };
        assert_eq!(
            composite_affine(4, 4, [0; 4], &[layer], limits),
            Err(MediaError::BudgetExceeded)
        );
        let limits = CompositeLimits {
            max_pixel_visits: 0,
            ..CompositeLimits::default()
        };
        assert_eq!(
            composite_affine(1, 1, [0; 4], &[], limits),
            Err(MediaError::BudgetExceeded)
        );
    }

    #[test]
    fn affine_far_offscreen_and_tiny_geometry_do_not_expand_work_or_pixels() {
        let image = RgbaFrame::solid(1, 1, [255; 4]).unwrap();
        let transforms = [
            Affine2 {
                xx: 1e150,
                yy: 1e150,
                tx: 1e151,
                ty: 1e151,
                ..Affine2::IDENTITY
            },
            Affine2 {
                xx: 1e-150,
                yy: 1e-150,
                ..Affine2::IDENTITY
            },
            Affine2 {
                xx: 1e-200,
                yy: 1e-200,
                ..Affine2::IDENTITY
            },
            Affine2 {
                xx: 1e308,
                yy: 1e-308,
                ..Affine2::IDENTITY
            },
        ];
        for transform in transforms {
            let layer = AffineLayer::new(&image, transform, UvRect::FULL, 1.0);
            let output =
                composite_affine(1, 1, [7, 8, 9, 255], &[layer], CompositeLimits::default())
                    .unwrap();
            assert_eq!(output.pixels(), &[7, 8, 9, 255]);
        }
        let layer = AffineLayer::new(&image, transforms[0], UvRect::FULL, 1.0);
        let limits = CompositeLimits {
            max_pixel_visits: 1,
            ..CompositeLimits::default()
        };
        assert!(composite_affine(1, 1, [0; 4], &[layer], limits).is_ok());
        let covering = Affine2 {
            xx: 1e150,
            yy: 1e150,
            tx: -5e149,
            ty: -5e149,
            ..Affine2::IDENTITY
        };
        let image = pattern(2, 2);
        let layer = AffineLayer::new(&image, covering, UvRect::FULL, 1.0);
        assert_eq!(
            composite_affine(1, 1, [0; 4], &[layer], limits),
            Err(MediaError::BudgetExceeded)
        );
        let limits = CompositeLimits {
            max_pixel_visits: 2,
            ..limits
        };
        let output = composite_affine(1, 1, [0; 4], &[layer], limits).unwrap();
        assert_eq!(pixel_labels(&output), [4]);
    }

    #[test]
    fn affine_rejects_invalid_uv_opacity_singular_and_nonfinite_geometry() {
        let image = pattern(1, 1);
        let transforms = [
            Affine2 {
                xx: 0.0,
                yy: 0.0,
                ..Affine2::IDENTITY
            },
            Affine2 {
                xx: 1.0,
                xy: 1.0,
                yx: 1.0,
                yy: 1.0,
                ..Affine2::IDENTITY
            },
            Affine2 {
                xx: f64::NAN,
                ..Affine2::IDENTITY
            },
            Affine2 {
                ty: f64::INFINITY,
                ..Affine2::IDENTITY
            },
            Affine2 {
                xx: 1e308,
                xy: 1e308,
                yy: 1e308,
                ..Affine2::IDENTITY
            },
        ];
        for transform in transforms {
            let layer = AffineLayer::new(&image, transform, UvRect::FULL, 1.0);
            assert_eq!(
                composite_affine(1, 1, [0; 4], &[layer], CompositeLimits::default()),
                Err(MediaError::InvalidTransform)
            );
        }
        for uv in [
            UvRect {
                left: -0.1,
                ..UvRect::FULL
            },
            UvRect {
                right: 1.1,
                ..UvRect::FULL
            },
            UvRect {
                left: 1.0,
                ..UvRect::FULL
            },
            UvRect {
                bottom: 0.0,
                ..UvRect::FULL
            },
            UvRect {
                top: f64::NAN,
                ..UvRect::FULL
            },
        ] {
            let layer = AffineLayer::new(&image, Affine2::IDENTITY, uv, 1.0);
            assert_eq!(
                composite_affine(1, 1, [0; 4], &[layer], CompositeLimits::default()),
                Err(MediaError::InvalidUv)
            );
        }
        let layer = AffineLayer::new(&image, Affine2::IDENTITY, UvRect::FULL, f32::NAN);
        assert_eq!(
            composite_affine(1, 1, [0; 4], &[layer], CompositeLimits::default()),
            Err(MediaError::InvalidOpacity)
        );
    }

    #[test]
    fn fractional_rate_does_not_accumulate_rounding_error() {
        let ntsc = FrameRate::new(30000, 1001).unwrap();
        assert_eq!(ntsc.timestamp(30000), Ok(MediaTime(1_001_000_000_000)));
        assert_eq!(ntsc.timestamp(1), Ok(MediaTime(33_366_666)));
        assert_eq!(FrameRate::new(0, 1), Err(MediaError::InvalidFrameRate));
        assert_eq!(FrameRate::new(1, 0), Err(MediaError::InvalidFrameRate));
        assert_eq!(
            FrameRate::new(1, u32::MAX).unwrap().timestamp(u64::MAX),
            Err(MediaError::Overflow)
        );
    }

    #[test]
    fn composition_blends_and_preserves_transparency() {
        let red = RgbaFrame::solid(1, 1, [255, 0, 0, 128]).unwrap();
        let rect = Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        let output = composite(
            1,
            1,
            [0, 0, 255, 255],
            &[Layer::new(&red, rect, 1.0)],
            CompositeLimits::default(),
        )
        .unwrap();
        assert_eq!(output.pixels(), &[128, 0, 127, 255]);
        let transparent = composite(
            1,
            1,
            [0, 0, 0, 0],
            &[Layer::new(&red, rect, 1.0)],
            CompositeLimits::default(),
        )
        .unwrap();
        assert_eq!(transparent.pixels(), &[255, 0, 0, 128]);
    }

    #[test]
    fn scaling_clipping_and_z_order_match_pixel_oracle() {
        let source = RgbaFrame::new(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 255]).unwrap();
        let blue = RgbaFrame::solid(1, 1, [0, 0, 255, 255]).unwrap();
        let layers = [
            Layer::new(
                &source,
                Rect {
                    x: -1,
                    y: 0,
                    width: 4,
                    height: 1,
                },
                1.0,
            ),
            Layer::new(
                &blue,
                Rect {
                    x: 1,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                1.0,
            ),
        ];
        let result = composite(4, 1, [0, 0, 0, 255], &layers, CompositeLimits::default()).unwrap();
        assert_eq!(
            result.pixels(),
            &[255, 0, 0, 255, 0, 0, 255, 255, 0, 255, 0, 255, 0, 0, 0, 255]
        );
    }

    #[test]
    fn hostile_composition_is_rejected_before_rendering() {
        assert_eq!(
            RgbaFrame::solid(0, 1, [0; 4]),
            Err(MediaError::InvalidDimensions)
        );
        assert_eq!(
            RgbaFrame::new(1, 1, vec![0; 3]),
            Err(MediaError::InvalidBuffer)
        );
        let frame = RgbaFrame::solid(1, 1, [0; 4]).unwrap();
        let rect = Rect {
            x: i32::MAX,
            y: i32::MIN,
            width: u32::MAX,
            height: u32::MAX,
        };
        let layer = Layer::new(&frame, rect, f32::NAN);
        assert_eq!(
            composite(1, 1, [0; 4], &[layer], CompositeLimits::default()),
            Err(MediaError::InvalidOpacity)
        );
        let layer = Layer::new(
            &frame,
            Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            1.0,
        );
        let limits = CompositeLimits {
            max_pixel_visits: 1,
            ..CompositeLimits::default()
        };
        assert_eq!(
            composite(1, 1, [0; 4], &[layer], limits),
            Err(MediaError::BudgetExceeded)
        );
        let limits = CompositeLimits {
            max_input_bytes: 3,
            ..CompositeLimits::default()
        };
        assert_eq!(
            composite(1, 1, [0; 4], &[layer], limits),
            Err(MediaError::BudgetExceeded)
        );
    }

    fn stream() -> StreamIdentity {
        StreamIdentity {
            source: 10,
            epoch: 2,
            settings: 3,
        }
    }

    fn video(identity: StreamIdentity) -> VideoFrame {
        VideoFrame {
            identity: FrameIdentity {
                source: identity.source,
                epoch: identity.epoch,
                settings: identity.settings,
                pts: MediaTime(5),
            },
            image: RgbaFrame::solid(1, 1, [1, 2, 3, 255]).unwrap(),
        }
    }

    #[test]
    fn queue_bounds_bytes_and_count_without_losing_queued_frames() {
        let mut queue = FrameQueue::new(2, 7, stream()).unwrap();
        queue.push(video(stream())).unwrap();
        let rejection = queue.push(video(stream())).unwrap_err();
        assert_eq!(rejection.error, MediaError::BudgetExceeded);
        assert_eq!(rejection.frame.image.pixels(), &[1, 2, 3, 255]);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.queued_bytes(), 4);
        assert_eq!(queue.pop().unwrap().identity.pts, MediaTime(5));
        assert_eq!(queue.queued_bytes(), 0);
        let mut queue = FrameQueue::new(1, 100, stream()).unwrap();
        queue.push(video(stream())).unwrap();
        assert_eq!(
            queue.push(video(stream())).unwrap_err().error,
            MediaError::BudgetExceeded
        );
    }

    #[test]
    fn frame_ingestion_discards_spare_capacity_before_byte_admission() {
        let mut pixels = Vec::with_capacity(1024 * 1024);
        pixels.extend_from_slice(&[1, 2, 3, 255]);
        assert!(pixels.capacity() > pixels.len());
        let image = RgbaFrame::new(1, 1, pixels).unwrap();
        // Box<[u8]> carries only the initialized slice; converting it back to
        // Vec cannot expose spare capacity inherited from the caller.
        let normalized_pixels = image.pixels.into_vec();
        assert_eq!(normalized_pixels.capacity(), 4);
        let image = RgbaFrame::new(1, 1, normalized_pixels).unwrap();
        let mut frame = video(stream());
        frame.image = image;
        let mut queue = FrameQueue::new(1, 4, stream()).unwrap();
        queue.push(frame).unwrap();
        assert_eq!(queue.queued_bytes(), 4);
        assert_eq!(queue.pop().unwrap().image.byte_len(), 4);
    }

    #[test]
    fn queue_rejects_stale_source_epoch_settings_and_close() {
        let mut queue = FrameQueue::new(2, 8, stream()).unwrap();
        for identity in [
            StreamIdentity {
                source: 11,
                ..stream()
            },
            StreamIdentity {
                epoch: 3,
                ..stream()
            },
            StreamIdentity {
                settings: 4,
                ..stream()
            },
        ] {
            assert_eq!(
                queue.push(video(identity)).unwrap_err().error,
                MediaError::StaleFrame
            );
        }
        queue.push(video(stream())).unwrap();
        let next = StreamIdentity {
            epoch: 3,
            ..stream()
        };
        queue.reset(next).unwrap();
        assert!(queue.is_empty());
        assert_eq!(queue.queued_bytes(), 0);
        assert_eq!(
            queue.push(video(stream())).unwrap_err().error,
            MediaError::StaleFrame
        );
        queue.push(video(next)).unwrap();
        queue.close();
        assert!(queue.is_closed());
        assert!(queue.is_empty());
        assert_eq!(
            queue.push(video(next)).unwrap_err().error,
            MediaError::Closed
        );
        assert_eq!(queue.reset(stream()), Err(MediaError::Closed));
    }

    #[test]
    fn audio_gain_mute_and_final_clip_match_oracle() {
        let first = [0.5, -0.5, 0.75, -0.75];
        let second = [0.5, -0.5, 0.75, -0.75];
        let ignored = [f32::NAN; 4];
        let inputs = [
            AudioInput {
                samples: &first,
                gain: 0.5,
                muted: false,
            },
            AudioInput {
                samples: &second,
                gain: 1.0,
                muted: false,
            },
            AudioInput {
                samples: &ignored,
                gain: 1.0,
                muted: true,
            },
        ];
        let mut output = [99.0; 4];
        let meter = mix_audio(&inputs, &mut output, 2, AudioLimits::default()).unwrap();
        assert_eq!(output, [0.75, -0.75, 1.0, -1.0]);
        assert_eq!(
            meter,
            Meter {
                peak: 1.0,
                clipped_samples: 2
            }
        );
    }

    #[test]
    fn invalid_audio_preserves_output_and_silence_is_real_zero() {
        let bad = [0.0, f32::INFINITY];
        let mut output = [0.25, 0.5];
        let input = AudioInput {
            samples: &bad,
            gain: 1.0,
            muted: false,
        };
        assert_eq!(
            mix_audio(&[input], &mut output, 2, AudioLimits::default()),
            Err(MediaError::InvalidAudio)
        );
        assert_eq!(output, [0.25, 0.5]);
        assert_eq!(
            mix_audio(&[], &mut output, 0, AudioLimits::default()),
            Err(MediaError::InvalidAudio)
        );
        let limits = AudioLimits {
            max_frames: 0,
            ..AudioLimits::default()
        };
        assert_eq!(
            mix_audio(&[], &mut output, 2, limits),
            Err(MediaError::InvalidAudio)
        );
        assert_eq!(output, [0.25, 0.5]);
        assert_eq!(
            mix_audio(&[], &mut output, 2, AudioLimits::default())
                .unwrap()
                .peak,
            0.0
        );
        assert_eq!(output, [0.0, 0.0]);
    }
}

mod craft_video;
mod craft_vignette;
mod craft_gaussian;
pub use craft_gaussian::{GaussianBlurDimensions,GaussianBlurSettings};
pub use craft_vignette::VignetteSettings;
pub use craft_video::{
    ColorKeyMatch, ColorKeyView, CraftVideoEffect, CraftVideoLimits, craft_video_effects,
};

/// Source-faithful stateless built-in OBS audio filters.
pub mod obs_audio;

pub mod obs_three_band_eq;

pub mod obs_dynamics;

mod freeze_mask;
pub use freeze_mask::{FreezeMaskSettings,FreezeMaskRequirements,FreezeMaskLimits,freeze_mask_requirements,freeze_mask_rgba};
mod image_mask;
pub use image_mask::{ImageMaskMethod,ImageMaskSettings,ImageMaskRequirements,ImageMaskLimits,image_mask_requirements,image_mask_v2};

pub mod craft_time;
