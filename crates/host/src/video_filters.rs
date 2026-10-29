//! Worker-owned Craft point-effect evaluation. Source settings stay canonical;
//! this per-render cache retains its input Arc so pointer identities cannot recycle.
use skylive_core::{KeyMatch, MatteView, Source, SpillChannel, VideoEffect};
use skylive_media::{
    ColorCorrectionSettings, ColorKeyMatch, ColorKeyView, CraftVideoEffect, RgbaFrame, SourceBlend,
    SourceEffectsLimits, SourceVideoEffect,
};
use std::{collections::HashMap, sync::Arc};
// The model and the media engine are independent crates, but every authored
// source chain must be admissible by the media planner before work is budgeted.
const _: () = assert!(skylive_core::MAX_VIDEO_FILTERS == skylive_media::MAX_SOURCE_VIDEO_EFFECTS);
// Scroll and GPU planning use a u64 slot mask; reject incompatible future
// model limits at compilation rather than silently truncating filter identity.
const _: () = assert!(skylive_core::MAX_VIDEO_FILTERS <= u64::BITS as usize);
mod person_background;
mod image_mask;
pub(crate) use image_mask::{MaskStore as StagedMaskStore, StagedMask, StagedMaskSet};
mod lut_chain;
pub(crate) use lut_chain::{LutStore as StagedLutStore,StagedLutSet};
mod scale;
pub(crate) use scale::plan as staged_scale_plan;
pub(crate) fn has_animated_mask(filters:&[skylive_core::VideoFilter],clock:Option<&crate::mask_animation_clock::Snapshot>)->bool{image_mask::has_animated_mask(filters,clock)}
pub(crate) fn has_linked_filter_resources(filters:&[skylive_core::VideoFilter])->bool{image_mask::has_live_mask(filters)||lut_chain::has_live_lut(filters)}
mod shared;
pub(crate) use shared::Runtime as SharedSourceFilters;
pub(crate) use shared::{TemporalObserver,TemporalSnapshot,TemporalContext,DraftRows};
pub(crate) use shared::CameraScope;
pub(crate) fn has_render_delay(filters:&[skylive_core::VideoFilter])->bool{filters.iter().any(|filter|filter.enabled&&matches!(filter.effect,VideoEffect::RenderDelay{delay_ms:1..=500}))}
pub use shared::Stats as SharedFilterStats;
#[cfg(feature = "gpu-filters")]
mod gpu_point;
#[cfg(feature = "gpu-filters")]
pub use gpu_point::GpuFailurePolicy;
#[cfg(feature = "gpu-filters")]
pub(crate) use gpu_point::GpuPointConfig;

#[derive(Clone, Copy)]
pub(crate) struct FilterProviders {
    pub(super) canvas:Option<(u32,u32)>,
    craft: bool,
    person: bool,
    freeze: bool,
}
impl FilterProviders {
    pub(crate) fn with_canvas(mut self,dimensions:(u32,u32))->Self{self.canvas=Some(dimensions);self}
    pub fn from_snapshot(model: &skylive_core::StudioSnapshot) -> Self {
        Self {
            canvas:Some((model.video().width(),model.video().height())),
            freeze:model.preferences().plugin_enabled(skylive_core::EXELDRO_FREEZE_PLUGIN),
            craft: model
                .preferences()
                .plugin_enabled(skylive_core::CRAFT_VIDEO_PLUGIN),
            person: model
                .preferences()
                .plugin_enabled(skylive_core::PERSON_BACKGROUND_PLUGIN),
        }
    }
    fn enabled(self, id: &str) -> bool {
        match id {
            skylive_core::EXELDRO_FREEZE_PLUGIN=>self.freeze,
            skylive_core::OBS_VIDEO_FILTERS_PLUGIN => true,
            skylive_core::CRAFT_VIDEO_PLUGIN => self.craft,
            skylive_core::PERSON_BACKGROUND_PLUGIN => self.person,
            _ => false,
        }
    }
}
/// Accept a camera delay only for the same delivered sample and exact active
/// authored chain. This also rejects draft edits made after capture.
pub(crate) fn camera_delay_applied(source:&Source, session:u64,
    frame:&crate::capture::CapturedFrame, input:&skylive_media::SharedVideoFrame,
    providers:FilterProviders)->bool {
    let Some(receipt)=frame.async_delay.as_ref() else {return false;};
    if receipt.session!=session || receipt.source!=source.id()
        || !receipt.history_live.load(std::sync::atomic::Ordering::Acquire)
        || receipt.live_generation.as_ref().is_none_or(|live| live.load(std::sync::atomic::Ordering::Acquire)!=receipt.binding_generation)
        || !frame.binding.matches_source(source.kind())
        || frame.native_origin!=Some(crate::capture::NativeFrameOrigin::Camera{owner:receipt.owner,stream:receipt.stream})
        || frame.native_clock_epoch!=Some(receipt.clock_epoch)
        || frame.sequence!=receipt.sequence || frame.native_timestamp_ns!=Some(receipt.timestamp_ns)
        || !frame.image.shares_pixels_with(input) {return false;}
    let mut expected=source.video_filters().iter().filter_map(|filter| {
        if !filter.enabled || !providers.enabled(filter.plugin_id()) {return None;}
        match filter.effect {
            VideoEffect::AsyncVideoDelay{delay_ms:delay_ms @ 1..=u16::MAX} => Some((filter.id(),delay_ms)),
            _=>None,
        }
    });
    for &(id,delay_ms) in receipt.filters.iter() {
        if expected.next()!=Some((Some(id),delay_ms)) {return false;}
    }
    if receipt.filters.is_empty() || expected.next().is_some() {return false;}
    match &receipt.staged_camera {
        None=>true,
        Some(proof)=>camera_staged_proof_matches(source,providers,proof),
    }
}
fn camera_staged_proof_matches(source:&Source,providers:FilterProviders,proof:&crate::capture::StagedCameraProof)->bool {
    if proof.live_generation.load(std::sync::atomic::Ordering::Acquire)!=proof.generation
        || proof.mask_live.as_ref().is_some_and(|live|!live.load(std::sync::atomic::Ordering::Acquire))
        || proof.scroll_live.as_ref().is_some_and(|live|!live.load(std::sync::atomic::Ordering::Acquire))
        || proof.mask_resources.as_ref().is_some_and(|resources|!resources.current()){return false;}
    let active:Vec<_>=source.video_filters().iter().filter(|filter|filter.enabled&&providers.enabled(filter.plugin_id())).collect();
    let Some(last)=active.iter().rposition(|filter|matches!(filter.effect,VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX})) else {return false;};
    if active[..last].len()!=proof.upstream.len() || !active[..last].iter().zip(proof.upstream.iter()).all(|(a,b)|**a==*b) {return false;}
    if active.iter().any(|filter|matches!(filter.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{..})) {return false;}
    let mut actual=proof.processed.ids().iter().copied();
    let mut expected_blend=SourceBlend::LinearSrgb;
    for filter in &active[..last] {
        match filter.effect {
            VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX}=>{},
            VideoEffect::ImageMask{..} if filter.mask_image().is_none()=>{},
            VideoEffect::ImageMask{..} if filter.id().is_some_and(|id|actual.next()==Some(id))=>{
                expected_blend=SourceBlend::PremultipliedLinearSrgb;
            },
            VideoEffect::ApplyLut{..} if filter.lut_asset().is_none()=>{},
            VideoEffect::ApplyLut{..} if filter.id().is_some_and(|id|actual.next()==Some(id))=>{
                expected_blend=SourceBlend::PremultipliedLinearSrgb;
            },
            VideoEffect::ScaleFilter{target:skylive_core::ScaleFilterTarget::Resolution{..},..}
                if filter.id().is_some_and(|id|actual.next()==Some(id))=>{
                    expected_blend=SourceBlend::PremultipliedLinearSrgb;
                },
            VideoEffect::Scroll{..} if proof.scroll_live.is_some() && filter.id().is_some_and(|id|actual.next()==Some(id))=>{
                expected_blend=SourceBlend::PremultipliedLinearSrgb;
            },
            VideoEffect::PersonBackgroundRemoval{..} if filter.id().is_some_and(|id|actual.next()==Some(id))=>{},
            _=>return false,
        }
    }
    actual.next().is_none() && proof.blend==expected_blend
}
fn ndi_staged_proof_matches(source:&Source,providers:FilterProviders,proof:&crate::capture::StagedNdiProof)->bool {
    // Reuse the ordered resource checks, while keeping NDI sender-clock and
    // camera capture proofs as distinct types and validating origin above.
    let stages=crate::capture::StagedCameraProof {upstream:proof.upstream.clone(),processed:proof.processed.clone(),blend:proof.blend,
        generation:proof.generation,live_generation:proof.live_generation.clone(),mask_live:proof.mask_live.clone(),mask_resources:proof.mask_resources.clone(),scroll_live:proof.scroll_live.clone()};
    camera_staged_proof_matches(source,providers,&stages)
}
/// NDI video without associated audio can use its verified delay immediately.
/// Audio-enabled NDI remains unavailable until the output-specific PCM route
/// can be aligned; the sender-to-local mapping is only an estimate.
pub(crate) fn ndi_video_delay_applied(source:&Source, session:u64,
    frame:&crate::capture::CapturedFrame, input:&skylive_media::SharedVideoFrame,
    providers:FilterProviders)->bool {
    if !matches!(source.kind(),skylive_core::SourceKind::NdiSource {ndi} if !ndi.audio()) {return false;}
    ndi_recording_delay_applied(source,session,frame,input,providers)
}
/// Proof for a recording-owned render. Audio-enabled NDI must only use this
/// path together with the independent recording audio reader.
pub(crate) fn ndi_recording_delay_applied(source:&Source, session:u64,
    frame:&crate::capture::CapturedFrame, input:&skylive_media::SharedVideoFrame,
    providers:FilterProviders)->bool {
    if !matches!(source.kind(),skylive_core::SourceKind::NdiSource {..}) {return false;}
    let Some(receipt)=frame.async_delay.as_ref() else {return false;};
    if receipt.session!=session || receipt.source!=source.id() || !receipt.current()
        || !frame.binding.matches_source(source.kind())
        || frame.native_origin!=Some(crate::capture::NativeFrameOrigin::Ndi{owner:receipt.owner,stream:receipt.stream})
        || frame.native_clock_epoch!=Some(receipt.clock_epoch) || frame.sequence!=receipt.sequence
        || frame.native_timestamp_ns!=Some(receipt.timestamp_ns) || !frame.image.shares_pixels_with(input) {return false;}
    let mut expected=source.video_filters().iter().filter_map(|filter| {
        if !filter.enabled || !providers.enabled(filter.plugin_id()) {return None;}
        match filter.effect {VideoEffect::AsyncVideoDelay{delay_ms:delay_ms @ 1..=u16::MAX}=>Some((filter.id(),delay_ms)),_=>None}
    });
    for &(id,delay_ms) in receipt.filters.iter() {if expected.next()!=Some((Some(id),delay_ms)){return false;}}
    !receipt.filters.is_empty() && expected.next().is_none()
        && receipt.staged_camera.is_none()
        && receipt.staged_ndi.as_ref().is_none_or(|proof|ndi_staged_proof_matches(source,providers,proof))
}
/// The independent monitor reader may use a delayed NDI capture proof. This
/// does not by itself authorize Program or any other output to use the frame.
pub(crate) fn ndi_monitor_delay_applied(source:&Source,session:u64,
    frame:&crate::capture::CapturedFrame,input:&skylive_media::SharedVideoFrame,
    providers:FilterProviders)->bool {
    matches!(source.kind(),skylive_core::SourceKind::NdiSource {ndi} if ndi.audio())
        && !source.audio_monitoring().is_off()
        && ndi_recording_delay_applied(source,session,frame,input,providers)
}
pub(crate) fn source_video_delay_applied(source:&Source,session:u64,frame:&crate::capture::CapturedFrame,
    input:&skylive_media::SharedVideoFrame,providers:FilterProviders)->bool {
    camera_delay_applied(source,session,frame,input,providers)
        || ndi_recording_delay_applied(source,session,frame,input,providers)
}
/// A delayed camera job can outlive its initiating render. Its generation
/// remains observable until the background worker and any GPU request retire.
#[derive(Clone)]
pub(crate) struct AsyncCameraGuard {
    live: Arc<std::sync::atomic::AtomicU64>,
    generation: u64,
    history_live: Arc<std::sync::atomic::AtomicBool>,
    staged:Option<crate::capture::StagedCameraProof>,
    staged_ndi:Option<crate::capture::StagedNdiProof>,
}
impl AsyncCameraGuard {
    pub(crate) fn from_frame(frame:&crate::capture::CapturedFrame)->Option<Self> {
        let receipt=frame.async_delay.as_ref()?;
        Some(Self {live:receipt.live_generation.as_ref()?.clone(), generation:receipt.binding_generation, history_live:receipt.history_live.clone(),staged:receipt.staged_camera.clone(),staged_ndi:receipt.staged_ndi.clone()})
    }
    fn current(&self)->bool {self.live.load(std::sync::atomic::Ordering::Acquire)==self.generation && self.history_live.load(std::sync::atomic::Ordering::Acquire)}
}
/// Only color correction can safely fall back to an unprocessed camera frame.
/// An active key, mask, crop or any other effect must never be silently skipped.
pub(crate) fn camera_color_only(filters:&[skylive_core::VideoFilter],providers:FilterProviders)->bool {
    let mut active=filters.iter().filter(|filter|filter.enabled&&providers.enabled(filter.plugin_id()));
    matches!(active.next(),Some(filter) if matches!(filter.effect,VideoEffect::ColorCorrection{..}))
        && active.all(|filter|matches!(filter.effect,VideoEffect::ColorCorrection{..}))
}
#[derive(Clone, Copy)]
enum FilterStage {
    Point(SourceVideoEffect),
    Person { threshold: f32, softness: f32, id:u64, interval_ms:u16 },
}

const MAX_FILTERED_BYTES: usize = 128 * 1024 * 1024;
const MAX_TEXEL_VISITS: u64 = 128 * 1024 * 1024;
#[derive(Clone, Debug)]
pub(crate) struct FilterError {
    pub kind: crate::resource_issue::ResourceIssueKind,
    pub code: &'static str,
    pub detail: String,
}
#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    input: usize,
    blend: SourceBlend,
    settings: Vec<[u32; 14]>,
    mask_identity:Vec<u8>,mask_epoch:u64,resource_generation:u64,
}
struct Entry {
    _input: Arc<RgbaFrame>,
    output: Arc<RgbaFrame>,
    blend: SourceBlend,
    _shared_lease:Option<Arc<shared::Lease>>,
}
#[derive(Default)]
pub(crate) struct FilterCache {
    pub(crate) scroll_context:crate::scroll_animation::Context,
    pub(crate) mask_context:Option<(skylive_core::Id,crate::mask_animation_clock::Snapshot)>,
    pub(crate) temporal_context:Option<TemporalContext>,
    // Set only on the serial worker after the capture receipt was validated.
    temporal_async_guard:Option<AsyncCameraGuard>,
    pub(crate) camera_scope:Option<CameraScope>,
    camera_fallback_leases:Vec<Arc<shared::Lease>>,
    person_store:Option<Arc<std::sync::Mutex<shared::person_cache::Store>>>,
    person_inference_gate:Option<Arc<std::sync::Mutex<()>>>,
    shared:Option<SharedSourceFilters>,
    resource_generation:u64,mask_epoch:u64,mask_store:Option<Arc<std::sync::Mutex<image_mask::MaskStore>>>,lut_store:Option<Arc<std::sync::Mutex<lut_chain::LutStore>>>,
    entries: HashMap<Key, Entry>,
    bytes: usize,
    texel_visits: u64,
    #[cfg(feature = "gpu-filters")]
    gpu: Option<GpuPointConfig>,
    #[cfg(feature = "gpu-filters")]
    pub(crate) gpu_calls: usize,
    #[cfg(feature = "gpu-filters")]
    pub(crate) gpu_report: Option<crate::gpu_color_correction::Report>,
    #[cfg(feature = "gpu-filters")]
    pub(crate) gpu_last_error: Option<crate::gpu_color_correction::Error>,
    #[cfg(test)]
    neural_calls: usize,
    #[cfg(test)]
    neural_error: Option<skylive_person_matte::Error>,
}
pub(crate) struct FilterResult {
    pub image: Arc<RgbaFrame>,
    pub blend: SourceBlend,
    pub new_bytes: usize,
    pub pixel_stages: usize,
}
/// A render result that keeps an asynchronous input's admission credit alive
/// when the chain is a passthrough. Freshly allocated filter output has its own
/// ordinary render-budget charge.
pub(crate) struct SharedFilterResult {
    pub image: skylive_media::SharedVideoFrame,
    pub blend: SourceBlend,
    pub new_bytes: usize,
    pub pixel_stages: usize,
}
fn fail(detail: impl Into<String>) -> FilterError {
    FilterError {
        kind: crate::resource_issue::ResourceIssueKind::Invalid,
        code: "video_filter_unavailable",
        detail: detail.into(),
    }
}
fn limited(detail: impl Into<String>) -> FilterError {
    let mut error = fail(detail);
    error.kind = crate::resource_issue::ResourceIssueKind::Limit;
    error
}
fn media_fail(error:skylive_media::MediaError)->FilterError {
    if error==skylive_media::MediaError::BudgetExceeded {limited(error.to_string())}else{fail(error.to_string())}
}
impl FilterCache {
    pub(crate) fn with_shared(mut self,shared:Option<SharedSourceFilters>)->Self {if let Some(runtime)=&shared {self.mask_store=Some(runtime.mask_store());self.lut_store=Some(runtime.lut_store());}self.shared=shared;self}
    pub(crate) fn with_resource_generation(mut self,generation:u64)->Self {self.resource_generation=generation;self}
    #[cfg(test)]
    pub fn apply(
        &mut self,
        source: &Source,
        input: Arc<RgbaFrame>,
        provider_enabled: bool,
        free_bytes: usize,
        free_pixel_stages: usize,
    ) -> Result<FilterResult, FilterError> {
        self.apply_encoded(
            source,
            input,
            SourceBlend::LinearSrgb,
            provider_enabled,
            free_bytes,
            free_pixel_stages,
        )
    }
    #[cfg(test)]
    pub fn apply_encoded(
        &mut self,
        source: &Source,
        input: Arc<RgbaFrame>,
        blend: SourceBlend,
        provider_enabled: bool,
        free_bytes: usize,
        free_pixel_stages: usize,
    ) -> Result<FilterResult, FilterError> {
        self.apply_with_providers(
            source,
            input,
            blend,
            FilterProviders {
                canvas:None,
                craft: provider_enabled,
                person: false,
                freeze:false,
            },
            free_bytes,
            free_pixel_stages,
        )
    }
    pub fn apply_with_providers(
        &mut self,
        source: &Source,
        input: Arc<RgbaFrame>,
        blend: SourceBlend,
        providers: FilterProviders,
        free_bytes: usize,
        free_pixel_stages: usize,
    ) -> Result<FilterResult, FilterError> {
        self.apply_filters(source.video_filters(),input,blend,providers,free_bytes,free_pixel_stages)
    }
    pub(crate) fn apply_with_shared_frame(
        &mut self, source:&Source, input:skylive_media::SharedVideoFrame, mut blend:SourceBlend,
        providers:FilterProviders, free_bytes:usize, free_pixel_stages:usize, async_delay_applied:bool,
        async_guard:Option<AsyncCameraGuard>,
    )->Result<SharedFilterResult,FilterError> {
        // Preserve the existing caching and behavior of all ordinary frames.
        if let Some(ordinary)=input.direct_arc() {
            let output=self.apply_with_providers(source,ordinary,blend,providers,free_bytes,free_pixel_stages)?;
            return Ok(SharedFilterResult{image:output.image.into(),blend:output.blend,new_bytes:output.new_bytes,pixel_stages:output.pixel_stages});
        }
        let filters=source.video_filters();
        if filters.len()>skylive_core::MAX_VIDEO_FILTERS{return Err(limited("source filter count"));}
        let mut active:Vec<_>=filters.iter().filter(|f|f.enabled&&providers.enabled(f.plugin_id())).collect();
        let staged=async_guard.as_ref().and_then(|guard|guard.staged.as_ref().map(|proof|(guard,proof)));
        if let Some((guard,proof))=staged {
            if !async_delay_applied || !guard.current() || !camera_staged_proof_matches(source,providers,proof) {
                return Err(FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"async_camera_frame_stale",detail:"Camera staged filter proof no longer matches".into()});
            }
            blend=proof.blend;
            active.retain(|filter|!filter.id().is_some_and(|id|proof.processed.ids().contains(&id)));
        }
        if let Some((guard,proof))=async_guard.as_ref().and_then(|guard|guard.staged_ndi.as_ref().map(|proof|(guard,proof))) {
            if !async_delay_applied || !guard.current() || !ndi_staged_proof_matches(source,providers,proof) {
                return Err(FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"async_ndi_frame_stale",detail:"NDI staged filter proof no longer matches".into()});
            }
            blend=proof.blend;
            active.retain(|filter|!filter.id().is_some_and(|id|proof.processed.ids().contains(&id)));
        }
        // The capture owner currently applies all asynchronous queues before
        // the render owner evaluates resource-backed effects. Reject authored
        // chains that would otherwise silently run a mask or neural matte at
        // a different timestamp than its requested position. The desktop
        // editor enforces the same restriction, but import/CLI can bypass it.
        if let Some(last_delay)=active.iter().rposition(|filter|matches!(filter.effect,VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX})) {
            if active[..last_delay].iter().any(|filter|matches!(filter.effect,VideoEffect::Scroll{..})) {
                return Err(FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"async_video_delay_order_unsupported",detail:"Scroll before or between asynchronous delays requires source-timestamp processing".into()});
            }
            if active[..last_delay].iter().any(|filter|matches!(filter.effect,VideoEffect::PersonBackgroundRemoval{..})
                || matches!(filter.effect,VideoEffect::ImageMask{..}) && filter.mask_image().is_some()) {
                return Err(FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"async_video_delay_order_unsupported",detail:"Image Mask and AI Background Removal before or between asynchronous delays require source-timestamp processing".into()});
            }
        }
        // NDI's stateful temporal chain remains unavailable independently of
        // the camera path. Do not silently bypass that provider limitation.
        if matches!(source.kind(),skylive_core::SourceKind::NdiSource {..}) {
            if active.iter().any(|filter|matches!(filter.effect,VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX})) {
                if active.iter().any(|filter|matches!(filter.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{..})) {
                    return Err(FilterError {kind:crate::resource_issue::ResourceIssueKind::Unavailable,
                        code:"ndi_delay_filter_order_unsupported",detail:"NDI Async Video Delay cannot be combined with Freeze or Render Delay".into()});
                }
            }
        }
        if active.iter().any(|filter|matches!(filter.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{delay_ms:1..=500})) {
            let guard=async_guard.filter(|guard|async_delay_applied&&guard.current())
                .ok_or_else(||FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"async_camera_frame_stale",detail:"The delayed camera generation is no longer current".into()})?;
            return self.apply_delay_shared(filters,input,blend,providers,free_bytes,free_pixel_stages,guard);
        }
        // The same budgeted, scope-aware mask owner serves the ordinary
        // worker and delayed renderers. It stores probabilities only, never
        // the delayed raster or its lease.
        if active.iter().any(|filter|matches!(filter.effect,VideoEffect::PersonBackgroundRemoval{..})&&filter.person_interval_ms()!=0)
            && self.person_store.is_none() {
            self.person_store=self.shared.as_ref().map(|runtime|runtime.person_store());
            self.person_inference_gate=self.shared.as_ref().map(|runtime|runtime.person_inference_gate());
        }
        let mut visual=active.iter().copied().filter(|filter|!matches!(filter.effect,VideoEffect::AsyncVideoDelay{..}));
        if let (Some(lut),None)=(visual.next(),visual.next()) {
            if matches!(lut.effect,VideoEffect::ApplyLut{..}) {
                for filter in &active {
                    filter.validate().map_err(|error|fail(error.to_string()))?;
                    if matches!(filter.effect,VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX}) && !async_delay_applied {
                        return Err(FilterError {kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"async_video_delay_unavailable",detail:"Camera delay evidence does not match this source and filter chain".into()});
                    }
                }
                return self.apply_delayed_single_lut(lut,input,blend,free_bytes,free_pixel_stages,false);
            }
        }
        // A mixed delayed chain cannot lend an uncredited intermediate Arc to
        // an asynchronous GPU worker. Run these ordered stages synchronously;
        // the original wrapper and each intermediate remain owned until the
        // next result has been produced and admitted.
        if active.iter().any(|filter|filter.lut_asset().is_some()||filter.mask_image().is_some()||matches!(filter.effect,VideoEffect::ScaleFilter{..}|VideoEffect::Scroll{..}|VideoEffect::PersonBackgroundRemoval{..})) {
            return self.apply_delayed_resource_chain(&active,input,blend,providers,free_bytes,free_pixel_stages,async_delay_applied);
        }
        let mut effects=Vec::new();
        for filter in active.iter().copied() {
            filter.validate().map_err(|error|fail(error.to_string()))?;
            // An unconfigured image mask or LUT is a neutral stage, just as
            // on the ordinary capture path. Do not hide a delayed camera while
            // the user is choosing a file or after clearing its path.
            if (matches!(filter.effect,VideoEffect::ImageMask{..}) && filter.mask_image().is_none())
                || (matches!(filter.effect,VideoEffect::ApplyLut{..}) && filter.lut_asset().is_none()) {
                continue;
            }
            // Delay must be fulfilled before this boundary. Stateful and
            // resource-backed stages still require a lease-aware history.
            if (matches!(filter.effect,VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX}) && !async_delay_applied)
                || matches!(filter.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{..}|VideoEffect::ScaleFilter{..}|VideoEffect::Scroll{..}|VideoEffect::ApplyLut{..}|VideoEffect::ImageMask{..}|VideoEffect::PersonBackgroundRemoval{..}) {
                return Err(FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"delayed_filter_chain_unavailable",detail:"This delayed frame requires an asynchronous or stateful filter path that is not connected yet".into()});
            }
            if let Some(FilterStage::Point(effect))=prepare_filter(filter).1 {effects.push(effect);}
        }
        if effects.is_empty(){return Ok(SharedFilterResult{image:input,blend,new_bytes:0,pixel_stages:0});}
        let required=skylive_media::source_effect_requirements(&input,&effects).map_err(media_fail)?;
        let limits=SourceEffectsLimits{max_output_bytes:free_bytes.saturating_sub(required.workspace_bytes),max_workspace_bytes:required.workspace_bytes,max_pixel_stages:free_pixel_stages,max_texel_visits:MAX_TEXEL_VISITS.saturating_sub(self.texel_visits)};
        let cpu_peak=required.output_bytes.checked_add(required.workspace_bytes).ok_or_else(||limited("video filter allocation overflow"))?;
        let peak=cpu_peak.max(self.gpu_points_peak((input.width(),input.height()),&effects,limits,free_bytes)?);
        let retained=required.final_width as usize*required.final_height as usize*4;
        if peak>free_bytes||retained>MAX_FILTERED_BYTES.saturating_sub(self.bytes)||required.pixel_stages>free_pixel_stages||required.texel_visits>MAX_TEXEL_VISITS.saturating_sub(self.texel_visits){return Err(limited("delayed source filter resource budget"));}
        self.texel_visits+=required.texel_visits;
        let output=self.apply_points(input,blend,&effects,limits,free_bytes)?;
        let image=Arc::new(output.image);let new_bytes=image.byte_len();
        Ok(SharedFilterResult{image:image.into(),blend:output.blend,new_bytes,pixel_stages:required.pixel_stages})
    }
    fn apply_delayed_resource_chain(&mut self,filters:&[&skylive_core::VideoFilter],input:skylive_media::SharedVideoFrame,
        ingress:SourceBlend,providers:FilterProviders,free_bytes:usize,free_work:usize,async_delay_applied:bool)->Result<SharedFilterResult,FilterError>{
        #[cfg(feature="gpu-filters")]
        if self.gpu.as_ref().is_some_and(|gpu|matches!(gpu.policy,GpuFailurePolicy::RequireGpu)) {
            return Err(FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"delayed_resource_gpu_unsupported",detail:"Ordered delayed spatial, LUT and mask chains currently require CPU processing".into()});
        }
        for filter in filters {
            filter.validate().map_err(|error|fail(error.to_string()))?;
            if (matches!(filter.effect,VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX})&&!async_delay_applied)
                || matches!(filter.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{..}) {
                return Err(FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"delayed_filter_chain_unavailable",detail:"This delayed chain contains an unsupported asynchronous or stateful stage".into()});
            }
        }
        let original=input.clone();let mut current=input;let mut blend=ingress;let mut work=0usize;let mut new_bytes=0usize;let mut points=Vec::new();
        for filter in filters.iter().copied().map(Some).chain(std::iter::once(None)) {
            let resource=filter.is_some_and(|f|f.lut_asset().is_some()||f.mask_image().is_some()||matches!(f.effect,VideoEffect::ScaleFilter{..}|VideoEffect::Scroll{..}|VideoEffect::PersonBackgroundRemoval{..}));
            if filter.is_some()&&!resource {
                if let Some(FilterStage::Point(effect))=prepare_filter(filter.unwrap()).1 {points.push(effect);}
                continue;
            }
            if !points.is_empty(){
                let held=if current.shares_pixels_with(&original){0}else{current.byte_len()};
                let available=free_bytes.saturating_sub(held);let remaining=free_work.saturating_sub(work);
                let required=skylive_media::source_effect_requirements(&current,&points).map_err(media_fail)?;
                let peak=required.output_bytes.checked_add(required.workspace_bytes).ok_or_else(||limited("delayed point budget overflow"))?;
                if peak>available||required.output_bytes>MAX_FILTERED_BYTES.saturating_sub(self.bytes)||required.pixel_stages>remaining||required.texel_visits>MAX_TEXEL_VISITS.saturating_sub(self.texel_visits){return Err(limited("delayed point segment exceeds shared bytes/work budget"));}
                let limits=SourceEffectsLimits{max_output_bytes:required.output_bytes,max_workspace_bytes:required.workspace_bytes,max_pixel_stages:required.pixel_stages,max_texel_visits:required.texel_visits};
                let output=skylive_media::source_video_effects(&current,blend,&points,limits).map_err(media_fail)?;
                self.texel_visits+=required.texel_visits;work=work.checked_add(required.pixel_stages).ok_or_else(||limited("delayed filter work overflow"))?;
                let image=Arc::new(output.image);new_bytes=image.byte_len();current=image.into();blend=output.blend;points.clear();
            }
            if let Some(filter)=filter {
                let held=if current.shares_pixels_with(&original){0}else{current.byte_len()};
                let available=free_bytes.saturating_sub(held);let remaining=free_work.saturating_sub(work);
                let output=if filter.lut_asset().is_some(){self.apply_delayed_single_lut(filter,current,blend,available,remaining,true)?}
                    else if filter.mask_image().is_some(){self.apply_delayed_single_mask(filter,current,blend,available,remaining)?}
                    else if matches!(filter.effect,VideoEffect::PersonBackgroundRemoval{..}){self.apply_delayed_person(filter,current,blend,available,remaining)?}
                    else {self.apply_delayed_spatial(filter,current,blend,providers,available,remaining)?};
                work=work.checked_add(output.pixel_stages).ok_or_else(||limited("delayed filter work overflow"))?;
                // A skipped spatial target returns its input unchanged. Keep
                // the charge of an earlier allocated intermediate in that case.
                if output.new_bytes!=0 {new_bytes=output.new_bytes;}
                current=output.image;blend=output.blend;
            }
        }
        Ok(SharedFilterResult{image:current,blend,new_bytes,pixel_stages:work})
    }
    /// Spatial filters are synchronous: the input wrapper and its delayed-frame
    /// credit live through each operation. Intermediate output is budgeted by
    /// the caller while a subsequent stage is evaluated.
    fn apply_delayed_spatial(&mut self,filter:&skylive_core::VideoFilter,input:skylive_media::SharedVideoFrame,
        blend:SourceBlend,providers:FilterProviders,free_bytes:usize,free_work:usize)->Result<SharedFilterResult,FilterError>{
        let (image,work,texels)=match filter.effect {
            VideoEffect::ScaleFilter{..}=>{
                let Some(plan)=scale::plan(filter.effect,(input.width(),input.height()),providers.canvas)? else {
                    return Ok(SharedFilterResult{image:input,blend,new_bytes:0,pixel_stages:0});
                };
                let required=plan.requirements();
                let work=usize::try_from(required.pixel_visits).map_err(|_|limited("delayed scale work overflow"))?;
                if required.output_bytes>free_bytes||required.output_bytes>MAX_FILTERED_BYTES.saturating_sub(self.bytes)||work>free_work||required.texel_visits>MAX_TEXEL_VISITS.saturating_sub(self.texel_visits){return Err(limited("delayed scale exceeds shared bytes/work budget"));}
                let max_pixel_visits=required.pixel_visits.checked_add(required.texel_visits).ok_or_else(||limited("delayed scale visits overflow"))?;
                let image=plan.apply(&input,blend,skylive_media::CompositeLimits{max_layers:1,max_input_bytes:required.input_bytes,max_output_bytes:required.output_bytes,max_pixel_visits}).map_err(media_fail)?;
                (image,work,required.texel_visits)
            },
            VideoEffect::Scroll{..}=>{
                let settings=lut_chain::scroll_settings(filter.effect);
                let phase=self.scroll_context.phase(filter);
                let required=skylive_media::scroll_requirements((input.width(),input.height()),settings,phase).map_err(media_fail)?;
                let work=required.output_bytes/4;
                if required.output_bytes>free_bytes||required.output_bytes>MAX_FILTERED_BYTES.saturating_sub(self.bytes)||work>free_work||required.texel_visits>MAX_TEXEL_VISITS.saturating_sub(self.texel_visits){return Err(limited("delayed scroll exceeds shared bytes/work budget"));}
                let image=skylive_media::scroll_rgba(&input,blend,settings,phase,skylive_media::ScrollLimits{max_output_bytes:required.output_bytes,max_texel_visits:required.texel_visits}).map_err(media_fail)?;
                (image,work,required.texel_visits)
            },
            _=>return Err(fail("not a delayed spatial filter")),
        };
        self.texel_visits+=texels;
        let image=Arc::new(image);let new_bytes=image.byte_len();
        Ok(SharedFilterResult{image:image.into(),blend:SourceBlend::PremultipliedLinearSrgb,new_bytes,pixel_stages:work})
    }
    pub(super) fn apply_filters(&mut self,filters:&[skylive_core::VideoFilter],input:Arc<RgbaFrame>,blend:SourceBlend,
        providers:FilterProviders,free_bytes:usize,free_pixel_stages:usize)->Result<FilterResult,FilterError> {
        if filters.len()>skylive_core::MAX_VIDEO_FILTERS{return Err(limited("source filter count"));}
        // Async delay belongs to the native sample path and source A/V clock,
        // before latest-frame coalescing. Until that path is connected, a
        // nonzero delay must not masquerade as an applied render filter.
        if self.temporal_async_guard.as_ref().is_none_or(|guard|!guard.current()) && filters.iter().any(|filter|filter.enabled&&providers.enabled(filter.plugin_id())
            &&matches!(filter.effect,VideoEffect::AsyncVideoDelay {delay_ms:1..=u16::MAX})) {
            return Err(FilterError {kind:crate::resource_issue::ResourceIssueKind::Unavailable,
                code:"async_video_delay_unavailable",detail:"Async Video Delay requires the source sample and A/V synchronization pipeline".into()});
        }
        // A default Scroll filter is a no-op. For an opaque input at the exact
        // origin, its linear-premultiplied render is equivalent to the original
        // frame. Avoid a full-resolution CPU resample for every camera frame.
        // Partial alpha still takes the regular OBS-compatible conversion path.
        let mut active=filters.iter().filter(|filter|filter.enabled&&providers.enabled(filter.plugin_id()));
        if let (Some(filter),None)=(active.next(),active.next()) {
            if matches!(filter.effect,VideoEffect::Scroll {limit_width:false,limit_height:false,..}) {
                let phase=self.scroll_context.phase(filter);
                let pixels=input.byte_len()/4;
                if matches!(blend,SourceBlend::LinearSrgb|SourceBlend::PremultipliedLinearSrgb)
                    &&phase.x==0.0&&phase.y==0.0&&pixels<=free_pixel_stages
                    &&input.pixels().chunks_exact(4).all(|pixel|pixel[3]==255) {
                    filter.validate().map_err(|error|fail(error.to_string()))?;
                    return Ok(FilterResult {image:input,blend,new_bytes:0,pixel_stages:pixels});
                }
            }
        }
        if shared::has_delay(filters)&&filters.iter().any(|f|f.enabled&&providers.enabled(f.plugin_id())&&matches!(f.effect,VideoEffect::Freeze{..}|VideoEffect::RenderDelay{delay_ms:1..=500})){return self.apply_delay(filters,input,blend,providers,free_bytes,free_pixel_stages);}
        // OBS's None target skips the provider entirely, including on a GPU-
        // required chain. Keep it authored, but do not submit a fake CPU stage.
        if filters.iter().any(|filter|filter.enabled&&matches!(filter.effect,VideoEffect::ScaleFilter{target:skylive_core::ScaleFilterTarget::None,..})){
            let active:Vec<_>=filters.iter().filter(|filter|!filter.enabled||!matches!(filter.effect,VideoEffect::ScaleFilter{target:skylive_core::ScaleFilterTarget::None,..})).cloned().collect();
            return self.apply_filters(&active,input,blend,providers,free_bytes,free_pixel_stages);
        }
        if lut_chain::has_lut(filters,providers)||filters.iter().any(|filter|filter.enabled&&providers.enabled(filter.plugin_id())&&matches!(filter.effect,VideoEffect::Scroll{..}|VideoEffect::ScaleFilter{..})){return self.apply_lut_chain(filters,input,blend,providers,free_bytes,free_pixel_stages);}
        if image_mask::has_mask(filters,providers) {return self.apply_mask_chain(filters,input,blend,providers,free_bytes,free_pixel_stages);}
        let mut effects = Vec::new();
        let mut settings = Vec::new();
        for filter in filters
            .iter()
            .filter(|filter| filter.enabled && providers.enabled(filter.plugin_id()))
        {
            filter.validate().map_err(|e| fail(e.to_string()))?;
            let (signature, effect) = prepare_filter(filter);
            if let Some(effect) = effect {
                settings.push(signature);
                effects.push(effect);
            }
        }
        if effects.is_empty() {
            return Ok(FilterResult {
                image: input,
                blend,
                new_bytes: 0,
                pixel_stages: 0,
            });
        }
        let key = Key {
            input: input.pixels().as_ptr() as usize,
            blend,
            settings,mask_identity:Vec::new(),mask_epoch:0,resource_generation:0,
        };
        if let Some(value) = self.entries.get(&key) {
            return Ok(FilterResult {
                image: value.output.clone(),
                blend: value.blend,
                new_bytes: 0,
                pixel_stages: 0,
            });
        }
        if self.entries.len() >= 3 * skylive_core::MAX_SOURCES

        {
            return Err(limited(
                "filtered video exceeds the shared/128 MiB effect resource budget",
            ));
        }
        if effects
            .iter()
            .any(|stage| matches!(stage, FilterStage::Person { .. }))
        {
            if self.shared.is_some() {
                let plan=self.person_plan(&input,&effects,free_bytes,free_pixel_stages)?;
                if let Some(result)=self.apply_shared(filters,key.clone(),&input,blend,providers,
                    shared::Admission {peak:plan.peak,work:plan.work,texels:plan.texels,width:plan.final_dimensions.0,height:plan.final_dimensions.1},free_bytes,free_pixel_stages)? {return Ok(result);}
            }
            return self.apply_person_chain(
                key,
                input,
                blend,
                &effects,
                free_bytes,
                free_pixel_stages,
            );
        }
        let effects: Vec<_> = effects
            .into_iter()
            .filter_map(|stage| match stage {
                FilterStage::Point(effect) => Some(effect),
                FilterStage::Person { .. } => None,
            })
            .collect();
        let required = skylive_media::source_effect_requirements(&input, &effects)
            .map_err(media_fail)?;
        let cpu_peak=required.output_bytes.checked_add(required.workspace_bytes).ok_or_else(||limited("video filter allocation overflow"))?;
        let gpu_peak=self.gpu_points_peak((input.width(),input.height()),&effects,
            SourceEffectsLimits {max_output_bytes:free_bytes.saturating_sub(required.workspace_bytes),max_workspace_bytes:required.workspace_bytes,
                max_pixel_stages:free_pixel_stages,max_texel_visits:MAX_TEXEL_VISITS.saturating_sub(self.texel_visits)},free_bytes)?;
        let peak=cpu_peak.max(gpu_peak);
        let retained_bytes=required.final_width as usize*required.final_height as usize*4;
        if peak > free_bytes || retained_bytes>MAX_FILTERED_BYTES.saturating_sub(self.bytes)
            || required.pixel_stages > free_pixel_stages
            || required.texel_visits > MAX_TEXEL_VISITS.saturating_sub(self.texel_visits)
        {
            return Err(limited(
                "video keyer output/scratch or cumulative 128 Mi texel-visit budget exceeded",
            ));
        }
        if let Some(result)=self.apply_shared(filters,key.clone(),&input,blend,providers,
            shared::Admission {peak,work:required.pixel_stages,texels:required.texel_visits,width:required.final_width,height:required.final_height},free_bytes,free_pixel_stages)? {return Ok(result);}
        let visits = required.pixel_stages;
        self.texel_visits += required.texel_visits;
        let filtered = self.apply_points(input.clone(), blend, &effects,
            SourceEffectsLimits { max_output_bytes: free_bytes - required.workspace_bytes,
                max_workspace_bytes: required.workspace_bytes, max_pixel_stages: free_pixel_stages,
                max_texel_visits: required.texel_visits }, free_bytes)?;
        let blend = filtered.blend;
        let image = Arc::new(filtered.image);
        let new_bytes = image.byte_len();
        self.bytes += new_bytes;
        self.entries.insert(
            key,
            Entry {
                _input: input,
                output: image.clone(),
                blend,
                _shared_lease:None,
            },
        );
        Ok(FilterResult {
            image,
            blend,
            new_bytes,
            pixel_stages: visits,
        })
    }
}
fn prepare_filter(filter:&skylive_core::VideoFilter)->([u32;14],Option<FilterStage>){
    let (mut signature,mut stage)=prepare(filter.effect);
    if let Some(FilterStage::Person{id,interval_ms,..})=&mut stage{*id=filter.id().map_or(0,|value|value.get());*interval_ms=filter.person_interval_ms();signature[3]=u32::from(*interval_ms);}
    (signature,stage)
}
fn prepare(effect: VideoEffect) -> ([u32; 14], Option<FilterStage>) {
    let mut signature = [0; 14];
    let effect = match effect {
        // A zero delay is a true no-op. Nonzero values are rejected at the
        // chain boundary above until the asynchronous sample path is wired.
        VideoEffect::AsyncVideoDelay {delay_ms}=>{signature[0]=28;signature[1]=u32::from(delay_ms);return(signature,None);},
        VideoEffect::Freeze {..}=>{signature[0]=27;return(signature,None);},
        VideoEffect::RenderDelay {..}=>{signature[0]=26;return(signature,None);},
        VideoEffect::ScaleFilter {..}=>{signature[0]=20;return(signature,None);},
        VideoEffect::Scroll {..}=>{signature[0]=19;return(signature,None);},
        VideoEffect::ApplyLut {amount,passthrough_alpha}=>{signature[0]=18;signature[1]=amount.to_bits();signature[2]=u32::from(passthrough_alpha);return(signature,None);},
        VideoEffect::ImageMask {..}=>return(signature,None),
        VideoEffect::CropPad {relative,left,top,right,bottom,width,height}=>{
            signature[0]=15;signature[1]=u32::from(relative);signature[2]=left as u32;signature[3]=top as u32;signature[4]=right as u32;signature[5]=bottom as u32;signature[6]=width;signature[7]=height;
            return(signature,Some(FilterStage::Point(SourceVideoEffect::CropPad(skylive_media::CropPadSettings {relative,left,top,right,bottom,width,height}))));
        }
        VideoEffect::LumaKey {luma_max,luma_min,luma_max_smooth,luma_min_smooth,legacy}=>{
            signature[0]=13;signature[1]=luma_max.to_bits();signature[2]=luma_min.to_bits();signature[3]=luma_max_smooth.to_bits();signature[4]=luma_min_smooth.to_bits();signature[5]=u32::from(legacy);
            return (signature,Some(FilterStage::Point(SourceVideoEffect::LumaKey(skylive_media::LumaKeySettings {luma_max,luma_min,luma_max_smooth,luma_min_smooth,legacy}))));
        }
        VideoEffect::Sharpen {sharpness}=>{
            signature[0]=14;signature[1]=sharpness.to_bits();return(signature,Some(FilterStage::Point(SourceVideoEffect::Sharpen(skylive_media::SharpenSettings {sharpness}))));
        }
        VideoEffect::Invert{amount}=>{if amount==0.0{return(signature,None);}signature[0]=7;signature[1]=amount.to_bits();CraftVideoEffect::Invert{amount}}
        VideoEffect::Grayscale{amount}=>{if amount==0.0{return(signature,None);}signature[0]=8;signature[1]=amount.to_bits();CraftVideoEffect::Grayscale{amount}}
        VideoEffect::Posterize{levels}=>{signature[0]=9;signature[1]=u32::from(levels);CraftVideoEffect::Posterize{levels}}
        VideoEffect::Exposure{exposure,offset,gamma,bypass_linear}=>{if exposure==0.0&&offset==0.0&&gamma==1.0{return(signature,None);}signature[0]=10;signature[1]=exposure.to_bits();signature[2]=offset.to_bits();signature[3]=gamma.to_bits();signature[4]=u32::from(bypass_linear);CraftVideoEffect::Exposure{gain:2.0f32.powf(exposure),offset,inverse_gamma:1.0/gamma,bypass_linear}}
        VideoEffect::TemperatureTint{temperature,tint}=>{if temperature==0.0&&tint==0.0{return(signature,None);}signature[0]=11;signature[1]=temperature.to_bits();signature[2]=tint.to_bits();CraftVideoEffect::TemperatureTint{temperature,tint}}
        VideoEffect::Vibrance{vibrance,saturation}=>{if vibrance==0.0&&saturation==0.0{return(signature,None);}signature[0]=32;signature[1]=vibrance.to_bits();signature[2]=saturation.to_bits();CraftVideoEffect::Vibrance{vibrance,saturation}}
        VideoEffect::Tritone{shadows,midtones,highlights,blend}=>{if blend==100.0{return(signature,None);}signature[0]=34;signature[1]=u32::from_le_bytes([shadows[0],shadows[1],shadows[2],0]);signature[2]=u32::from_le_bytes([midtones[0],midtones[1],midtones[2],0]);signature[3]=u32::from_le_bytes([highlights[0],highlights[1],highlights[2],0]);signature[4]=blend.to_bits();CraftVideoEffect::Tritone{shadows,midtones,highlights,blend}}
        VideoEffect::CraftColorBalance{shadows,midtones,highlights,preserve_luminosity}=>{signature[0]=35;for(i,v)in shadows.into_iter().chain(midtones).chain(highlights).enumerate(){signature[1+i]=v.to_bits();}signature[10]=u32::from(preserve_luminosity);CraftVideoEffect::ColorBalance{shadows,midtones,highlights,preserve_luminosity}}
        VideoEffect::CraftChannelMixer{matrix,monochrome}=>{if !monochrome&&matrix==[[100.0,0.0,0.0,0.0],[0.0,100.0,0.0,0.0],[0.0,0.0,100.0,0.0]]{return(signature,None);}signature[0]=36;for(i,v)in matrix.into_iter().flatten().enumerate(){signature[1+i]=v.to_bits();}signature[13]=u32::from(monochrome);CraftVideoEffect::ChannelMixer{matrix,monochrome}}
        VideoEffect::CraftGammaPedestalGain{gamma,pedestal,gain,black_stretch}=>{signature[0]=37;for(i,v)in gamma.into_iter().chain(pedestal).chain(gain).enumerate(){signature[1+i]=v.to_bits();}signature[10]=black_stretch.to_bits();CraftVideoEffect::GammaPedestalGain{gamma,pedestal,gain,black_stretch}}
        VideoEffect::CraftTint{black,white,amount}=>{if amount==0.0{return(signature,None);}signature[0]=33;signature[1]=u32::from_le_bytes([black[0],black[1],black[2],0]);signature[2]=u32::from_le_bytes([white[0],white[1],white[2],0]);signature[3]=amount.to_bits();CraftVideoEffect::Tint{black,white,amount}}
        VideoEffect::Threshold{level}=>{signature[0]=12;signature[1]=u32::from(level);CraftVideoEffect::Threshold{level}}
        VideoEffect::GaussianBlur{blurriness,dimensions,repeat_edge}=>{
            if blurriness==0.0{return(signature,None);}
            signature[0]=31;signature[1]=blurriness.to_bits();signature[2]=dimensions as u32;signature[3]=u32::from(repeat_edge);
            let dimensions=match dimensions{skylive_core::GaussianBlurDimensions::Both=>skylive_media::GaussianBlurDimensions::Both,skylive_core::GaussianBlurDimensions::Horizontal=>skylive_media::GaussianBlurDimensions::Horizontal,skylive_core::GaussianBlurDimensions::Vertical=>skylive_media::GaussianBlurDimensions::Vertical};
            return(signature,Some(FilterStage::Point(SourceVideoEffect::GaussianBlur(skylive_media::GaussianBlurSettings{blurriness,dimensions,repeat_edge}))));
        }
        VideoEffect::Vignette {amount,angle_of_view,center,pin_highlights}=>{
            if amount==0.0 || angle_of_view==0.0 {return(signature,None);}
            //27 is reserved for the independent Freeze stage.
            signature[0]=28;signature[1]=amount.to_bits();signature[2]=angle_of_view.to_bits();
            signature[3]=center[0].to_bits();signature[4]=center[1].to_bits();signature[5]=pin_highlights.to_bits();
            return(signature,Some(FilterStage::Point(SourceVideoEffect::Vignette(skylive_media::VignetteSettings {amount,angle_of_view,center,pin_highlights}))));
        }
        VideoEffect::PhotoFilter{preset,custom_color,density,preserve_luminosity}=>{
            // Neutral density is omitted BEFORE CPU/GPU segment planning.
            if density==0.0{return(signature,None);}
            signature[0]=30;signature[1]=preset as u32;
            for(index,value)in custom_color.into_iter().enumerate(){signature[2+index]=value.to_bits();}
            signature[5]=density.to_bits();signature[6]=u32::from(preserve_luminosity);
            CraftVideoEffect::PhotoFilter{color:preset.resolve_color(custom_color),density,preserve_luminosity}
        }
        VideoEffect::Sepia{density,preserve_luminosity}=>{if density==0.0{return(signature,None);}signature[0]=29;signature[1]=density.to_bits();signature[2]=u32::from(preserve_luminosity);CraftVideoEffect::Sepia{density,preserve_luminosity}}

        VideoEffect::PersonBackgroundRemoval {
            threshold,
            softness,
        } => {
            signature[0] = 6;
            signature[1] = threshold.to_bits();
            signature[2] = softness.to_bits();
            return (
                signature,
                Some(FilterStage::Person {
                    threshold,
                    softness,
                    id:0,interval_ms:0,
                }),
            );
        }
        VideoEffect::ColorKey {
            key_color_type,
            key_color,
            similarity,
            smoothness,
            opacity,
            contrast,
            brightness,
            gamma,
        } => {
            signature = key_signature(
                4,
                key_color_type as u32,
                key_color,
                similarity,
                smoothness,
                0,
                opacity,
                contrast,
                brightness,
                gamma,
            );
            let key_color_type = match key_color_type {
                skylive_core::ColorKeyType::Green => skylive_media::ColorKeyType::Green,
                skylive_core::ColorKeyType::Blue => skylive_media::ColorKeyType::Blue,
                skylive_core::ColorKeyType::Red => skylive_media::ColorKeyType::Red,
                skylive_core::ColorKeyType::Magenta => skylive_media::ColorKeyType::Magenta,
                skylive_core::ColorKeyType::Custom => skylive_media::ColorKeyType::Custom,
            };
            return (
                signature,
                Some(FilterStage::Point(SourceVideoEffect::ColorKey(
                    skylive_media::ColorKeySettings {
                        key_color_type,
                        key_color,
                        similarity,
                        smoothness,
                        opacity,
                        contrast,
                        brightness,
                        gamma,
                    },
                ))),
            );
        }
        VideoEffect::ChromaKey {
            key_color_type,
            key_color,
            similarity,
            smoothness,
            spill,
            opacity,
            contrast,
            brightness,
            gamma,
        } => {
            signature = key_signature(
                5,
                key_color_type as u32,
                key_color,
                similarity,
                smoothness,
                spill,
                opacity,
                contrast,
                brightness,
                gamma,
            );
            let key_color_type = match key_color_type {
                skylive_core::ChromaKeyType::Green => skylive_media::ChromaKeyType::Green,
                skylive_core::ChromaKeyType::Blue => skylive_media::ChromaKeyType::Blue,
                skylive_core::ChromaKeyType::Magenta => skylive_media::ChromaKeyType::Magenta,
                skylive_core::ChromaKeyType::Custom => skylive_media::ChromaKeyType::Custom,
            };
            return (
                signature,
                Some(FilterStage::Point(SourceVideoEffect::ChromaKey(
                    skylive_media::ChromaKeySettings {
                        key_color_type,
                        key_color,
                        similarity,
                        smoothness,
                        spill,
                        opacity,
                        contrast,
                        brightness,
                        gamma,
                    },
                ))),
            );
        }
        VideoEffect::ColorCorrection {
            gamma,
            contrast,
            brightness,
            saturation,
            hue_shift,
            opacity,
            color_multiply,
            color_add,
        } => {
            signature = [
                3,
                gamma.to_bits() as u32,
                (gamma.to_bits() >> 32) as u32,
                contrast.to_bits(),
                brightness.to_bits(),
                saturation.to_bits(),
                hue_shift.to_bits(),
                opacity.to_bits(),
                u32::from_le_bytes([color_multiply[0], color_multiply[1], color_multiply[2], 0]),
                u32::from_le_bytes([color_add[0], color_add[1], color_add[2], 0]),
                0,
                0,
                0,
                0,
            ];
            return (
                signature,
                Some(FilterStage::Point(SourceVideoEffect::ColorCorrection(
                    ColorCorrectionSettings {
                        gamma,
                        contrast,
                        brightness,
                        saturation,
                        hue_shift,
                        opacity,
                        color_multiply,
                        color_add,
                    },
                ))),
            );
        }
        VideoEffect::LinearColorKey {
            color,
            matching,
            tolerance,
            softness,
            keep,
            view,
        } => {
            if view == MatteView::Source {
                return (signature, None);
            }
            signature = [
                0,
                u32::from_le_bytes([color[0], color[1], color[2], 0]),
                matching as u32,
                tolerance.to_bits(),
                softness.to_bits(),
                u32::from(keep),
                view as u32,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ];
            CraftVideoEffect::LinearColorKey {
                color,
                matching: match matching {
                    KeyMatch::Rgb => ColorKeyMatch::Rgb,
                    KeyMatch::Hue => ColorKeyMatch::Hue,
                    KeyMatch::Chroma => ColorKeyMatch::Chroma,
                },
                tolerance,
                softness,
                keep,
                view: match view {
                    MatteView::Final => ColorKeyView::Final,
                    MatteView::Source => ColorKeyView::Source,
                    MatteView::Matte => ColorKeyView::Matte,
                },
            }
        }
        VideoEffect::BrightnessContrast {
            brightness,
            contrast,
            legacy,
        } => {
            if brightness == 0.0 && contrast == 0.0 {
                return (signature, None);
            }
            signature[0] = 1;
            signature[1] = brightness.to_bits();
            signature[2] = contrast.to_bits();
            signature[3] = u32::from(legacy);
            CraftVideoEffect::BrightnessContrast {
                brightness,
                contrast,
                legacy,
            }
        }
        VideoEffect::SpillSuppression {
            channel,
            balance,
            amount,
        } => {
            if amount == 0.0 {
                return (signature, None);
            }
            signature[0] = 2;
            signature[1] = channel as u32;
            signature[2] = balance.to_bits();
            signature[3] = amount.to_bits();
            CraftVideoEffect::SpillSuppression {
                blue: channel == SpillChannel::Blue,
                balance,
                amount,
            }
        }
    };
    (
        signature,
        Some(FilterStage::Point(SourceVideoEffect::Craft(effect))),
    )
}

#[allow(clippy::too_many_arguments)] // Authored fields are all part of the exact cache identity.
fn key_signature(
    kind: u32,
    ty: u32,
    color: [u8; 3],
    similarity: u16,
    smoothness: u16,
    spill: u16,
    opacity: f32,
    contrast: f64,
    brightness: f32,
    gamma: f64,
) -> [u32; 14] {
    [
        kind,
        ty,
        u32::from_le_bytes([color[0], color[1], color[2], 0]),
        u32::from(similarity),
        u32::from(smoothness),
        u32::from(spill),
        opacity.to_bits(),
        contrast.to_bits() as u32,
        (contrast.to_bits() >> 32) as u32,
        brightness.to_bits(),
        gamma.to_bits() as u32,
        (gamma.to_bits() >> 32) as u32,
        0,
        0,
    ]
}

#[cfg(test)]
#[path = "video_filters/color_correction_tests.rs"]
mod color_correction_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use skylive_core::{Command, Session, VideoFilter};
    #[test]
    fn actual_source_effect_cache_deduplicates_exact_input_and_settings_but_not_new_frames() {
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
            .apply(Command::SetSourceVideoFilters {
                source,
                filters: vec![VideoFilter::new(VideoEffect::color_key()).unwrap()],
            })
            .unwrap();
        let model = session.snapshot();
        let source = model.source(source).unwrap();
        let input = Arc::new(RgbaFrame::solid(16, 16, [0, 255, 0, 255]).unwrap());
        let mut cache = FilterCache::default();
        let first = cache.apply(source, input.clone(), true, 1024, 256).unwrap();
        assert_eq!(&first.image.pixels()[..4], &[0; 4]);
        let second = cache.apply(source, input.clone(), true, 0, 0).unwrap();
        assert!(Arc::ptr_eq(&first.image, &second.image));
        assert_eq!((second.new_bytes, second.pixel_stages), (0, 0));
        let bypass = cache.apply(source, input.clone(), false, 0, 0).unwrap();
        assert!(Arc::ptr_eq(&bypass.image, &input));
        let changed = Arc::new(RgbaFrame::solid(16, 16, [255, 0, 0, 255]).unwrap());
        assert!(
            cache
                .apply(source, changed.clone(), true, 1023, 256)
                .is_err()
        );
        let red = cache.apply(source, changed, true, 1024, 256).unwrap();
        assert_eq!(&red.image.pixels()[..4], &[255, 0, 0, 255]);
        assert!(!Arc::ptr_eq(&first.image, &red.image));
    }
    #[test]
    fn actual_preview_default_provider_bypass_order_undo_and_native_reopen_match_pixels() {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::SetVideo {
                width: 16,
                height: 16,
                fps_num: 30,
                fps_den: 1,
            })
            .unwrap();
        session
            .apply(Command::AddColorSource {
                scene,
                name: "Screen".into(),
                rgba: [0, 255, 0, 255],
            })
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        assert!(
            session
                .snapshot()
                .preferences()
                .plugin_enabled(skylive_core::CRAFT_VIDEO_PLUGIN)
        );
        assert!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .video_filters()
                .is_empty(),
            "default availability must not insert effects"
        );
        let key = VideoFilter::new(VideoEffect::color_key()).unwrap();
        let bright = VideoFilter::new(VideoEffect::BrightnessContrast {
            brightness: 50.0,
            contrast: 0.0,
            legacy: true,
        })
        .unwrap();
        let frame =
            |session: &Session| crate::preview::render(&session.snapshot(), scene, 16, 16).unwrap();
        session
            .apply(Command::SetSourceVideoFilters {
                source,
                filters: vec![key.clone(), bright.clone()],
            })
            .unwrap();
        assert_eq!(
            &frame(&session).pixels()[..4],
            &[0, 0, 0, 255],
            "top key runs before bottom brightness, preserving transparency"
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(&frame(&session).pixels()[..4], &[0, 255, 0, 255]);
        session.apply(Command::Redo).unwrap();
        assert_eq!(&frame(&session).pixels()[..4], &[0, 0, 0, 255]);
        let bytes = crate::storage::encode(&session.snapshot()).unwrap();
        let restored = crate::storage::decode(&bytes, true).unwrap();
        assert_eq!(
            restored.snapshot().source(source).unwrap().video_filters(),
            session.snapshot().source(source).unwrap().video_filters()
        );
        assert_eq!(frame(&restored).pixels(), frame(&session).pixels());
        session
            .apply(Command::SetPluginEnabled {
                plugin: skylive_core::CRAFT_VIDEO_PLUGIN.into(),
                enabled: false,
            })
            .unwrap();
        assert_eq!(&frame(&session).pixels()[..4], &[0, 255, 0, 255]);
        assert_eq!(
            session
                .snapshot()
                .source(source)
                .unwrap()
                .video_filters()
                .len(),
            2,
            "global bypass retains authored filters"
        );
        session
            .apply(Command::SetPluginEnabled {
                plugin: skylive_core::CRAFT_VIDEO_PLUGIN.into(),
                enabled: true,
            })
            .unwrap();
        session
            .apply(Command::SetSourceVideoFilters {
                source,
                filters: vec![bright, key],
            })
            .unwrap();
        assert_eq!(
            &frame(&session).pixels()[..4],
            &[128, 255, 128, 255],
            "top brightness changes color before bottom key"
        );
    }
}

#[cfg(test)]
#[path = "video_filters/keyer_tests.rs"]
mod keyer_tests;

#[cfg(test)]
#[path = "video_filters/person_background_tests.rs"]
mod person_background_tests;

#[cfg(test)]
#[path = "video_filters/person_performance_tests.rs"]
mod person_performance_tests;

#[cfg(not(feature = "gpu-filters"))]
impl FilterCache {
    fn gpu_person_peak(&self,_dimensions:(u32,u32),_threshold:f32,_softness:f32,
        _limits:skylive_media::PersonMatteLimits,_free_cpu_bytes:usize)->Result<usize,FilterError> {Ok(0)}
    fn apply_person_output(&mut self,input:impl Into<skylive_media::SharedVideoFrame>,blend:SourceBlend,mask:&[f32],threshold:f32,softness:f32,
        limits:skylive_media::PersonMatteLimits,_free_cpu_bytes:usize)->Result<RgbaFrame,FilterError> {
        let input=input.into();
        skylive_media::apply_person_matte(&input,blend,mask,threshold,softness,limits).map_err(|error|fail(error.to_string()))
    }
    fn gpu_points_peak(&self,_dimensions:(u32,u32),_effects:&[SourceVideoEffect],_limits:SourceEffectsLimits,_free_cpu_bytes:usize)->Result<usize,FilterError>{Ok(0)}
    fn apply_points(&mut self,input:impl Into<skylive_media::SharedVideoFrame>,blend:SourceBlend,effects:&[SourceVideoEffect],limits:SourceEffectsLimits,_free_cpu_bytes:usize)->Result<skylive_media::SourceEffectFrame,FilterError> {
        let input=input.into();
        skylive_media::source_video_effects(&input,blend,effects,limits).map_err(|error|if error==skylive_media::MediaError::BudgetExceeded {limited(error.to_string())}else{fail(error.to_string())})
    }
}
