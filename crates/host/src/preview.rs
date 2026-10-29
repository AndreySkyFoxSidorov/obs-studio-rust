//! Owned preview work with one admitted job, full freshness identity and explicit close.
use crate::capture::CaptureSnapshot;
use crate::playback::PlaybackSnapshot;
use crate::transition::{CompositionKey, FrameComposition, FrameScene, OutputKey, SceneKey};
use serde::Serialize;
use skylive_core::{Id, Session, SourceKind, StudioSnapshot, TransitionDirection, TransitionKind};
use skylive_media::{
    Affine2, AffineLayer, AnimationClip, CompositeLimits, MediaError, MediaTime, RgbaFrame,
    SourceBlend, TexelRect, UvRect, composite_affine,
};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, mpsc},
    thread,
};

// Owner guards belong inside background worker closures or a headless command
// loop. Reexport through host so frontends do not import the inference adapter.
pub use skylive_person_matte::{
    BackgroundOwner as RenderServiceOwner, retain_background_owner as retain_render_services,
};

#[path="preview/image_slideshow.rs"]
mod image_slideshow;
mod image_cache;
mod filter_draft;
mod profile_output;
mod luma_wipes;
pub use filter_draft::FilterPreview;
pub const MAX_PREVIEW_WIDTH: u32 = 1920;
pub const MAX_PREVIEW_HEIGHT: u32 = 1080;
pub const MAX_RENDER_IMAGE_BYTES: usize = 128 * 1024 * 1024;
/// Independent image clips (128MiB) plus captured live inputs (256MiB).
pub const MAX_RENDER_RESOURCE_BYTES: usize = 384 * 1024 * 1024;
#[cfg(test)]
#[path = "preview/live_output_tests.rs"]
mod live_output_tests;
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceDiagnostic {
    pub kind: crate::resource_issue::ResourceIssueKind,
    pub source: Id,
    pub code: &'static str,
    pub detail: String,
}
#[derive(Debug)]
pub struct RenderedScene {
    pub image: RgbaFrame,
    blend: SourceBlend,
    pub diagnostics: Vec<SourceDiagnostic>,
    /// Non-fatal GPU diagnostics for the filter dialog; pixels remain valid.
    pub gpu_notice: Option<String>,
    pub next_after_ns: Option<u64>,
    auto_text:Vec<crate::text::auto_worker::Snapshot>,
    // Conservative provenance for recording: only camera pixels known to
    // participate in this exact rendered image are included. This is not an
    // inventory of sources merely referenced by the scene.
    pub(crate) camera_trace: Vec<Id>,
    // Pixel centres with proven contribution, used only when a nested scene
    // is sampled by another scene. They are not frame-wide attribution.
    camera_witnesses: Vec<(Id, [f64; 2])>,
    // Pixel centres proven in the final root canvas. Unlike nested witnesses,
    // these may include partially transparent layers; they are only valid
    // while a later output stage selects the exact same pixel.
    output_witnesses: Vec<(Id, [f64; 2])>,
}
impl RenderedScene {
    pub(crate) fn auto_text_current(&self)->bool{self.auto_text.iter().all(|snapshot|snapshot.is_live())}
    pub(crate) fn auto_text_guards(&self)->Vec<crate::text::auto_worker::Snapshot>{self.auto_text.clone()}
    pub(crate) fn into_straight(self) -> RgbaFrame {
        skylive_media::straighten_rgba(self.image, self.blend)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewKey {
    pub session: u64,
    pub revision: u64,
    pub request: u64,
    pub scene: Id,
    pub width: u32,
    pub height: u32,
    pub playback_epoch: u64,
    pub time_ns: u64,
    pub resource_generation: u64,
    pub capture_epoch: u64,
    pub capture_generation: u64,
    pub output: OutputKey,
    pub composition: CompositionKey,
}
#[derive(Debug)]
pub struct PreviewResult {
    pub key: PreviewKey,
    pub image: RgbaFrame,
    pub diagnostics: Vec<SourceDiagnostic>,
    pub gpu_notice: Option<String>,
    pub next_after_ns: Option<u64>,
    auto_text:Vec<crate::text::auto_worker::Snapshot>,
}
/// Geometry generation paired with a displayed preview image. A UI must not
/// hit-test a newer auto-text snapshot over pixels from an older generation.
#[derive(Clone,Debug,Default)]
pub struct AutoTextView {pub(crate) snapshots:Vec<crate::text::auto_worker::Snapshot>}
impl AutoTextView {
    pub fn matches(&self,session:u64,model:&StudioSnapshot)->bool{
        if self.snapshots.iter().any(|snapshot|snapshot.is_pinned()){return false;}
        let mut count=0usize;
        for source in model.sources(){
            let SourceKind::Text{settings}=source.kind() else{continue;};
            if !settings.auto_extents(){continue;}
            count+=1;
            if !self.snapshots.iter().any(|snapshot|snapshot.current(session,source.id(),settings)){return false;}
        }
        count==self.snapshots.len()
    }
    /// Execute an authored edit only while every displayed live generation is
    /// held against worker publication. Contention is nonblocking: the edit is
    /// cancelled rather than applying geometry from a stale image.
    pub fn with_current<R>(&self,session:u64,model:&StudioSnapshot,edit:impl FnOnce()->R)->Option<R>{
        if !self.matches(session,model){return None;}
        let mut locks=Vec::with_capacity(self.snapshots.len());
        for snapshot in &self.snapshots {locks.push(snapshot.try_commit_lock()?);}
        Some(edit())
    }
}
impl PreviewResult {
    /// Recheck immediately before uploading a completed preview image.
    pub fn auto_text_current(&self)->bool{self.auto_text.iter().all(|snapshot|snapshot.is_live())}
    pub fn auto_text_view(&self)->AutoTextView{AutoTextView{snapshots:self.auto_text.clone()}}
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewError {
    Busy,
    Closed,
    Disconnected,
    InvalidSize,
    InvalidCollection(skylive_core::Error),
    NotFound(Id),
    IdentityExhausted,
    StalePlayback,
    StaleCapture,
    AutoTextRetry,
    Transition(crate::transition::TransitionError),
    Media(MediaError),
    LumaWipe{mask:&'static str,detail:String},
    ScrollClock(crate::scroll_animation::Error),
    TemporalClock(String),
    ResourcesUnavailable(Vec<SourceDiagnostic>),
}
impl fmt::Display for PreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PreviewError {}

pub fn render(
    collection: &StudioSnapshot,
    scene: Id,
    width: u32,
    height: u32,
) -> Result<RgbaFrame, PreviewError> {
    render_at(collection, scene, width, height, MediaTime(0))
}
pub fn render_at(
    collection: &StudioSnapshot,
    scene: Id,
    width: u32,
    height: u32,
    time: MediaTime,
) -> Result<RgbaFrame, PreviewError> {
    let result = render_cached(
        collection,
        scene,
        width,
        height,
        &PlaybackSnapshot::for_collection(collection, time),
        &mut ClipCache::default(),
    )?;
    if result.diagnostics.is_empty() {
        Ok(result.image)
    } else {
        Err(PreviewError::ResourcesUnavailable(result.diagnostics))
    }
}

/// Preview may retain other visible layers while reporting missing/stale assets.
/// Export callers use `render` to require complete resource coverage before I/O.
pub fn render_with_diagnostics(
    collection: &StudioSnapshot,
    scene: Id,
    width: u32,
    height: u32,
) -> Result<RenderedScene, PreviewError> {
    render_cached(
        collection,
        scene,
        width,
        height,
        &PlaybackSnapshot::for_collection(collection, MediaTime(0)),
        &mut ClipCache::default(),
    )
}
type ResourceResult = Result<
    Arc<AnimationClip>,
    (
        crate::resource_issue::ResourceIssueKind,
        &'static str,
        String,
    ),
>;
#[derive(Clone, Default)]
pub(crate) struct PointBackend {
    pub(crate) shared:Option<crate::video_filters::SharedSourceFilters>,
    #[cfg(feature = "gpu-filters")]
    pub(crate) gpu: Option<crate::video_filters::GpuPointConfig>,
}
impl PointBackend {
    fn effects(&self)->crate::video_filters::FilterCache {
        #[cfg(feature = "gpu-filters")]
        if let Some(gpu)=&self.gpu {return crate::video_filters::FilterCache::with_gpu(gpu.client.clone(),gpu.policy).with_shared(self.shared.clone());}
        crate::video_filters::FilterCache::default().with_shared(self.shared.clone())
    }
}
#[cfg(feature = "gpu-filters")]
pub use crate::video_filters::GpuFailurePolicy;
#[derive(Default)]
struct ClipCache {
    luma_wipe:luma_wipes::Cache,
    point_backend: PointBackend,
    scope: Option<(u64, u64, Vec<SceneKey>)>,
    generation: u64,
    bytes: usize,
    entries: HashMap<skylive_core::ImageAsset, image_cache::Entry>,
    preload: Option<image_cache::Preload>,
    text: crate::text::TextCache,
}
impl ClipCache {
    fn resource_generation(&mut self, generation: u64) {
        if self.generation != generation {
            if let Some(preload)=&mut self.preload {preload.invalidate();}
            self.luma_wipe.clear_error();
            self.entries.clear();
            self.text.clear();
            self.bytes = 0;
            self.scope = None;
            self.generation = generation;
        }
    }
    fn scope(&mut self, session: u64, revision: u64, scene: Id) {
        self.scopes(session, revision, vec![SceneKey::Live(scene)]);
    }
    fn scopes(&mut self, session: u64, revision: u64, scenes: Vec<SceneKey>) {
        if self.scope.as_ref() != Some(&(session, revision, scenes.clone())) {
            if let Some(preload)=&mut self.preload {preload.invalidate();}
            self.luma_wipe.clear_error();
            if self.scope.as_ref().is_none_or(|(owner,_,_)|*owner!=session){self.entries.clear();self.bytes=0;}
            self.text.clear();
            self.scope = Some((session, revision, scenes));
        }
    }

}
#[cfg(test)]
mod interaction_cache_tests {
    use super::*;
    #[test]
    fn layout_only_drafts_share_resource_scope_but_not_render_identity() {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(skylive_core::Command::AddColorSource {
                scene,
                name: "Red".into(),
                rgba: [255, 0, 0, 255],
            })
            .unwrap();
        let snapshot = session.snapshot();
        let item = snapshot.scene(scene).unwrap().items()[0].id();
        let mut transform = snapshot.scene(scene).unwrap().items()[0]
            .transform()
            .clone();
        transform.x = 10.0;
        let first = FrameScene::preview_transform(
            session.identity(),
            &snapshot,
            scene,
            item,
            transform.clone(),
        )
        .unwrap();
        transform.x = 20.0;
        let second =
            FrameScene::preview_transform(session.identity(), &snapshot, scene, item, transform)
                .unwrap();
        assert_ne!(first.key(), second.key());
        assert_eq!(first.resource_key(), SceneKey::Live(scene));
        let mut cache = ClipCache::default();
        cache.resource_generation(7);
        cache.scope(session.identity(), snapshot.revision(), scene);
        let asset =
            skylive_core::ImageAsset::new("injected-no-io.png".into(), [1; 32], 1, 1).unwrap();
        cache.entries.insert(
            asset.clone(),
            image_cache::Entry{slots:[Some(Err((
                crate::resource_issue::ResourceIssueKind::Unavailable,
                "fixture",
                "retained diagnostic".into(),
            ))),None],visible:1},
        );
        for frame in [&first, &second] {
            cache.scopes(
                session.identity(),
                snapshot.revision(),
                vec![frame.resource_key()],
            );
            assert!(cache.entries.contains_key(&asset));
        }
        cache.scope(session.identity(), snapshot.revision(), scene);
        assert!(
            cache.entries.contains_key(&asset),
            "cancel retains unchanged linked assets"
        );
        cache.resource_generation(8);
        assert!(
            cache.entries.is_empty(),
            "explicit reload still invalidates resources"
        );
    }
}
fn render_cached(
    collection: &StudioSnapshot,
    scene: Id,
    width: u32,
    height: u32,
    playback: &PlaybackSnapshot,
    cache: &mut ClipCache,
) -> Result<RenderedScene, PreviewError> {
    cache.scope(playback.session(), collection.revision(), scene);
    let mut pinned=image_cache::Pins::default();
    pinned.persistent(collection,cache).map_err(PreviewError::Media)?;
    for source in FrameScene::Live(scene).visible_sources(collection).map_err(PreviewError::Transition)? {
        image_slideshow::pin(source,playback,&mut pinned).map_err(PreviewError::Media)?;
    }
    image_slideshow::retain(cache,&pinned);
    cache.preload_hidden(&pinned);
    let capture = CaptureSnapshot::empty(playback.session());
    let mut resources = RenderResources::new(cache, &capture);
    render_frame(
        collection,
        &FrameScene::Live(scene),
        width,
        height,
        playback,
        &mut resources,
    )
}
struct RenderResources<'a> {
    full_resolution: bool,
    recording_ndi: bool,
    cache: &'a mut ClipCache,
    reads: crate::assets::ReadBudget,
    font_reads:crate::font_assets::FontReadBudget,
    text_reads:usize,
    decoded: crate::assets::DecodeBudget,
    capture: &'a CaptureSnapshot,
    // A guarded auto-text frame may be paired with a derived model projection.
    // Ordinary render entry points leave this unset until the lifecycle owner
    // can supply a coherent snapshot to every output and Preview consumer.
    auto_text:Vec<crate::text::auto_worker::Snapshot>,
    used_auto_text:Vec<crate::text::auto_worker::Snapshot>,
    live_bytes: usize,
    live_buffers: std::collections::HashSet<usize>,
    effects: crate::video_filters::FilterCache,
    nested: HashMap<(SceneKey, Id), NestedFrame>,
    pixel_visits: usize,
    base_work_limit:usize,
    sampling_extra_visits:u64,
    scene_diagnostics: HashMap<SceneKey, HashMap<(Id, &'static str), SourceDiagnostic>>,
    diagnostic_bytes: usize,
    // Scalar completeness witness for actual/cached nested render inputs.
    // It is local to this existing render owner/request, not a new runtime.
    incomplete_inputs:usize,
}
#[derive(Clone)]
struct NestedFrame {
    complete:bool,
    image: Arc<RgbaFrame>,
    blend: SourceBlend,
    next_after_ns: Option<u64>,
    camera_witnesses: Arc<Vec<(Id, [f64; 2])>>,
}
impl<'a> RenderResources<'a> {
    fn incomplete(&mut self)->Result<(),PreviewError>{self.incomplete_inputs=self.incomplete_inputs.checked_add(1).ok_or(PreviewError::Media(MediaError::BudgetExceeded))?;Ok(())}
    fn work_remaining(&self)->usize {self.base_work_limit.saturating_sub(self.pixel_visits)}
    fn filter_work_remaining(&self)->usize {self.work_remaining().min(64*1024*1024)}
    fn new(cache: &'a mut ClipCache, capture: &'a CaptureSnapshot) -> Self {
        let effects=cache.point_backend.effects().with_resource_generation(cache.generation);
        let luma_bytes=cache.luma_wipe.bytes();
        Self {
            full_resolution: false,
            recording_ndi: false,
            cache,
            reads: crate::assets::ReadBudget::new(128 * 1024 * 1024),
            font_reads:crate::font_assets::FontReadBudget::new(64*1024*1024),
            text_reads:16*1024*1024,
            decoded: crate::assets::DecodeBudget::new(MAX_RENDER_IMAGE_BYTES),
            capture,
            auto_text:Vec::new(),
            used_auto_text:Vec::new(),
            live_bytes: luma_bytes,
            live_buffers: std::collections::HashSet::new(),
            effects,
            nested: HashMap::new(),
            pixel_visits: 0,
            base_work_limit:64*1024*1024,
            sampling_extra_visits:0,
            scene_diagnostics: HashMap::new(),
            diagnostic_bytes: 0,
            incomplete_inputs:0,
        }
    }
}
/// Strict export of the same captured Program composition used by desktop.
pub fn render_output(
    collection: &StudioSnapshot,
    output: &FrameComposition,
    width: u32,
    height: u32,
    playback: &PlaybackSnapshot,
) -> Result<RgbaFrame, PreviewError> {
    render_output_with_capture(
        collection,
        output,
        width,
        height,
        playback,
        &CaptureSnapshot::empty(playback.session()),
    )
}
pub fn render_output_with_capture(
    collection: &StudioSnapshot,
    output: &FrameComposition,
    width: u32,
    height: u32,
    playback: &PlaybackSnapshot,
    capture: &CaptureSnapshot,
) -> Result<RgbaFrame, PreviewError> {
    let result = render_output_cached_with_capture(
        collection,
        output,
        width,
        height,
        playback,
        &mut ClipCache::default(),
        capture,
    )?;
    if result.diagnostics.is_empty() {
        Ok(result.image)
    } else {
        Err(PreviewError::ResourcesUnavailable(result.diagnostics))
    }
}
/// Worker-owned reusable resource cache for strict Program output. No UI handles.
#[derive(Default)]
pub struct OutputRenderer {
    cache: ClipCache,
}
#[cfg(test)]
#[path = "preview_output_tests.rs"]
mod output_resolution_tests;
impl OutputRenderer {
    pub(crate) fn resource_generation(&mut self,generation:u64){self.cache.resource_generation(generation);}
    pub(crate) fn with_point_backend(point_backend:PointBackend)->Self {
        Self {cache:ClipCache {point_backend,..Default::default()}}
    }
    /// Concrete GPU injection for the existing full-resolution ordered graph.
    /// Keep the service Owner alive outside all renderers until workers settle.
    #[cfg(feature = "gpu-filters")]
    pub fn with_gpu(client:crate::gpu_color_correction::Client,policy:GpuFailurePolicy)->Self {
        let mut renderer=Self::default();
        renderer.cache.point_backend.gpu=Some(crate::video_filters::GpuPointConfig {client:client.into(),policy});
        renderer
    }

    /// Shared lazy service from the desktop/CLI composition root.
    #[cfg(feature = "gpu-filters")]
    pub fn with_gpu_runtime(runtime:crate::gpu_color_correction::Runtime,policy:GpuFailurePolicy)->Self {
        let mut renderer=Self::default();
        renderer.cache.point_backend.gpu=Some(crate::video_filters::GpuPointConfig {client:runtime.into(),policy});
        renderer
    }

    /// Metadata/freshness admission shared by normal rendering and recording's
    /// prepared-sample cache. No resource reads, decoding or pixel allocation.
    pub(crate) fn validate_live_inputs(
        collection: &StudioSnapshot,
        output: &FrameComposition,
        width: u32,
        height: u32,
        playback: &PlaybackSnapshot,
        capture: &CaptureSnapshot,
    ) -> Result<(), PreviewError> {
        validate_render_inputs(collection, output, width, height, playback, capture, true)
    }
    /// Strict bounded preview export with this renderer's injected backend.
    /// Preserves the legacy CLI capture/preview dimension contract.
    pub fn render_preview(
        &mut self,
        collection:&StudioSnapshot,
        output:&FrameComposition,
        width:u32,
        height:u32,
        playback:&PlaybackSnapshot,
        capture:&CaptureSnapshot,
    )->Result<RgbaFrame,PreviewError> {
        let result=render_output_cached_with_capture(collection,output,width,height,playback,&mut self.cache,capture)?;
        if result.diagnostics.is_empty() {Ok(result.image)} else {Err(PreviewError::ResourcesUnavailable(result.diagnostics))}
    }
    /// Live output skips unavailable individual sources while preserving all
    /// frame/model/freshness and resource admission failures.
    pub fn render_live(
        &mut self,
        collection: &StudioSnapshot,
        output: &FrameComposition,
        width: u32,
        height: u32,
        playback: &PlaybackSnapshot,
        capture: &CaptureSnapshot,
    ) -> Result<RenderedScene, PreviewError> {
        let rendered = render_output_policy(
            collection,
            output,
            width,
            height,
            playback,
            &mut self.cache,
            capture,
            true,
        )?;
        if rendered
            .diagnostics
            .iter()
            .any(|issue| !issue.kind.recoverable())
        {
            return Err(PreviewError::ResourcesUnavailable(rendered.diagnostics));
        }
        Ok(rendered)
    }
    pub fn render(
        &mut self,
        collection: &StudioSnapshot,
        output: &FrameComposition,
        width: u32,
        height: u32,
        playback: &PlaybackSnapshot,
        capture: &CaptureSnapshot,
    ) -> Result<RgbaFrame, PreviewError> {
        let rendered = render_output_policy(
            collection,
            output,
            width,
            height,
            playback,
            &mut self.cache,
            capture,
            true,
        )?;
        if rendered.diagnostics.is_empty() {
            Ok(rendered.image)
        } else {
            Err(PreviewError::ResourcesUnavailable(rendered.diagnostics))
        }
    }
}
#[cfg(test)]
fn render_output_cached(
    collection: &StudioSnapshot,
    output: &FrameComposition,
    width: u32,
    height: u32,
    playback: &PlaybackSnapshot,
    cache: &mut ClipCache,
) -> Result<RenderedScene, PreviewError> {
    render_output_cached_with_capture(
        collection,
        output,
        width,
        height,
        playback,
        cache,
        &CaptureSnapshot::empty(playback.session()),
    )
}
fn render_output_cached_with_capture(
    collection: &StudioSnapshot,
    output: &FrameComposition,
    width: u32,
    height: u32,
    playback: &PlaybackSnapshot,
    cache: &mut ClipCache,
    capture: &CaptureSnapshot,
) -> Result<RenderedScene, PreviewError> {
    render_output_policy(
        collection, output, width, height, playback, cache, capture, false,
    )
}
#[allow(clippy::too_many_arguments)]
fn render_output_policy(
    collection: &StudioSnapshot,
    output: &FrameComposition,
    width: u32,
    height: u32,
    playback: &PlaybackSnapshot,
    cache: &mut ClipCache,
    capture: &CaptureSnapshot,
    full_resolution: bool,
) -> Result<RenderedScene, PreviewError> {
    render_output_policy_profile(collection,output,width,height,playback,cache,capture,full_resolution,None,false)
}
#[allow(clippy::too_many_arguments)]
fn render_output_policy_profile(
    collection: &StudioSnapshot,
    output: &FrameComposition,
    width: u32,
    height: u32,
    playback: &PlaybackSnapshot,
    cache: &mut ClipCache,
    capture: &CaptureSnapshot,
    full_resolution: bool,
    profile:Option<&profile_output::Plan>,
    recording_ndi:bool,
) -> Result<RenderedScene, PreviewError> {
    let has_auto=collection.sources().iter().any(|source|matches!(source.kind(),SourceKind::Text{settings} if settings.auto_extents()));
    let mut pinned_fallback=false;
    let mut fallback_detail=None;
    let auto_snapshots=if has_auto {
        // A retained source without a live generation cannot safely use a
        // freshly prepared frame from a different source identity.
        for view in output.scenes(){
            if matches!(&view,FrameScene::Frozen(_)) {
                let visible=view.visible_sources(collection).map_err(PreviewError::Transition)?;
                if visible.iter().any(|source|matches!(source.kind(),SourceKind::Text{settings} if settings.auto_extents()) && collection.source(source.id()).is_none()
                    && !view.auto_text_snapshots().is_some_and(|snapshots|snapshots.iter().any(|snapshot|snapshot.source()==source.id()&&snapshot.is_pinned()&&snapshot.is_live()))){
                    return Err(PreviewError::TemporalClock("A frozen automatic text source no longer has a live generation".into()));
                }
            }
        }
        match crate::text::auto_worker::shared_snapshots(playback.session(),collection){
            Ok(frames)=>frames,
            Err(error)=>{
                let views=output.scenes();
                let covered=!views.is_empty()&&views.iter().all(|view|view.auto_text_snapshots().is_some_and(|frames|
                    !frames.is_empty()&&frames.iter().all(|frame|frame.is_pinned()&&frame.is_live())));
                if !covered{return Err(match error{crate::text::auto_worker::AcquireError::Pending=>PreviewError::AutoTextRetry,
                    crate::text::auto_worker::AcquireError::Unavailable(detail)=>PreviewError::TemporalClock(detail)});}
                pinned_fallback=true;
                if let crate::text::auto_worker::AcquireError::Unavailable(detail)=error{fallback_detail=Some(detail);}
                Vec::new()
            }
        }
    }else{Vec::new()};
    let mut projected=if has_auto&&!pinned_fallback {Some(collection.clone())}else{None};
    if let Some(model)=&mut projected {
        *model=crate::text::auto_worker::project_all(playback.session(),model,&auto_snapshots)
            .map_err(|error|if auto_snapshots.iter().any(|snapshot|!snapshot.is_live()){PreviewError::AutoTextRetry}else{PreviewError::InvalidCollection(error)})?;
    }
    let authored_collection=collection;
    let projected_output=projected.as_ref().map(|model|output.with_auto_projection(collection,model))
        .transpose().map_err(PreviewError::Transition)?;
    let collection=projected.as_ref().unwrap_or(collection);
    let output=projected_output.as_ref().unwrap_or(output);
    validate_render_inputs(
        collection,
        output,
        width,
        height,
        playback,
        capture,
        full_resolution,
    )?;
    cache.scopes(
        playback.session(),
        collection.revision(),
        output
            .scenes()
            .iter()
            .map(FrameScene::resource_key)
            .collect(),
    );
    let mut pinned=image_cache::Pins::default();
    pinned.persistent(collection,cache).map_err(PreviewError::Media)?;
    for view in output.scenes(){
        for source in view.visible_sources(collection).map_err(PreviewError::Transition)? {
            image_slideshow::pin(source,playback,&mut pinned).map_err(PreviewError::Media)?;
        }
    }
    image_slideshow::retain(cache,&pinned);
    cache.preload_hidden(&pinned);
    let mut resources = RenderResources::new(cache, capture);
    resources.auto_text=auto_snapshots.clone();
    resources.full_resolution = full_resolution;
    resources.recording_ndi = recording_ndi;
    if let Some(profile)=profile {profile.reserve(&mut resources,output,(width,height))?;}
    luma_wipes::reserve(&mut resources,output,(width,height),profile.is_some())?;
    let rendered=match output {
        FrameComposition::Scene(scene) | FrameComposition::SceneAlias { scene, .. } => {
            render_frame_for_output(authored_collection,collection, scene, width, height, playback, &mut resources)
        }
        FrameComposition::Black { .. } => blend_endpoints(
            None,
            None,
            (0, 1),
            (width, height),
            None,
            TransitionKind::Fade,
            None,
        ),
        FrameComposition::SingleFade {
            scene,
            incoming,
            elapsed_ns,
            duration_ns,
            ..
        } => {
            let rendered =
                render_frame_for_output(authored_collection,collection, scene, width, height, playback, &mut resources)?;
            let (from, to) = if *incoming {
                (None, Some(rendered))
            } else {
                (Some(rendered), None)
            };
            blend_endpoints(
                from,
                to,
                (*elapsed_ns, *duration_ns),
                (width, height),
                output.next_after_ns(collection),
                TransitionKind::Fade,
                None,
            )
        }
        FrameComposition::Manual {
            from,
            to,
            progress_num,
            progress_den,
            kind,
            ..
        }
        | FrameComposition::Directional {
            from,
            to,
            elapsed_ns: progress_num,
            duration_ns: progress_den,
            kind,
            ..
        }
        | FrameComposition::Configured {
            from,
            to,
            elapsed_ns: progress_num,
            duration_ns: progress_den,
            kind,
            ..
        } => {
            let from = from
                .as_ref()
                .map(|view| render_frame_for_output(authored_collection,collection, view, width, height, playback, &mut resources))
                .transpose()?;
            let to = to
                .as_ref()
                .map(|view| render_frame_for_output(authored_collection,collection, view, width, height, playback, &mut resources))
                .transpose()?;
            blend_endpoints(
                from,
                to,
                (*progress_num, *progress_den),
                (width, height),
                output.next_after_ns(collection),
                *kind,
                if let TransitionKind::LumaWipe{mask,..}=kind{Some(resources.cache.luma_wipe.get(*mask)?)}else{None},
            )
        }
        FrameComposition::Fade {
            from,
            to,
            elapsed_ns,
            duration_ns,
            ..
        } => {
            let a = render_frame_for_output(authored_collection,collection, from, width, height, playback, &mut resources)?;
            let b = render_frame_for_output(authored_collection,collection, to, width, height, playback, &mut resources)?;
            blend_endpoints(
                Some(a),
                Some(b),
                (*elapsed_ns, *duration_ns),
                (width, height),
                output.next_after_ns(collection),
                TransitionKind::Fade,
                None,
            )
        }
    }?;
    let mut rendered=if let Some(profile)=profile {profile.apply(rendered)?}else{rendered};
    if pinned_fallback {rendered.next_after_ns=Some(rendered.next_after_ns.unwrap_or(250_000_000).min(250_000_000));}
    if let Some(detail)=fallback_detail {
        if let Some(source)=collection.sources().iter().find(|source|matches!(source.kind(),SourceKind::Text{settings} if settings.auto_extents())){
            rendered.diagnostics.push(SourceDiagnostic{source:source.id(),kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"text_auto_prepare",detail});
        }
    }
    let mut guards=resources.used_auto_text.clone();
    let views=output.scenes();
    if views.iter().any(|view|matches!(view,FrameScene::Live(_))){
        for snapshot in &auto_snapshots {if !guards.iter().any(|old|old.same(snapshot)){guards.push(snapshot.clone());}}
    }
    for view in &views {if let Some(snapshots)=view.auto_text_snapshots(){for snapshot in snapshots {
        if !guards.iter().any(|old|old.same(snapshot)){guards.push(snapshot.clone());}
    }}}
    if guards.iter().any(|snapshot|!snapshot.is_live()){
        return Err(PreviewError::AutoTextRetry);
    }
    rendered.auto_text=guards;
    Ok(rendered)
}
fn validate_render_inputs(
    collection: &StudioSnapshot,
    output: &FrameComposition,
    width: u32,
    height: u32,
    playback: &PlaybackSnapshot,
    capture: &CaptureSnapshot,
    full_resolution: bool,
) -> Result<(), PreviewError> {
    if playback.revision() != collection.revision() {
        return Err(PreviewError::StalePlayback);
    }
    if capture.session() != playback.session() {
        return Err(PreviewError::StaleCapture);
    }
    validate_output_policy(collection, output, width, height, full_resolution)?;
    for scene in output.scenes() {
        scene
            .validate_session(playback.session())
            .map_err(PreviewError::Transition)?;
        scene
            .visible_sources(collection)
            .map_err(PreviewError::Transition)?;
    }
    Ok(())
}
fn validate_output(
    collection: &StudioSnapshot,
    output: &FrameComposition,
    width: u32,
    height: u32,
) -> Result<(), PreviewError> {
    validate_output_policy(collection, output, width, height, false)
}
fn validate_output_policy(
    collection: &StudioSnapshot,
    output: &FrameComposition,
    width: u32,
    height: u32,
    full_resolution: bool,
) -> Result<(), PreviewError> {
    if !admitted_size(width, height, full_resolution) {
        return Err(PreviewError::InvalidSize);
    }
    collection
        .validate()
        .map_err(PreviewError::InvalidCollection)?;
    if collection.scene(output.primary_scene()).is_none() {
        return Err(PreviewError::NotFound(output.primary_scene()));
    }
    match output {
        FrameComposition::Fade {
            elapsed_ns,
            duration_ns,
            ..
        }
        | FrameComposition::SingleFade {
            elapsed_ns,
            duration_ns,
            ..
        }
        | FrameComposition::Directional {
            elapsed_ns,
            duration_ns,
            ..
        }
        | FrameComposition::Configured {
            elapsed_ns,
            duration_ns,
            ..
        } if *duration_ns == 0 || elapsed_ns > duration_ns => {
            return Err(PreviewError::Media(MediaError::InvalidTransition));
        }
        FrameComposition::Manual {
            progress_num,
            progress_den,
            next_after_ns,
            ..
        } if *progress_den == 0 || progress_num > progress_den || *next_after_ns == Some(0) => {
            return Err(PreviewError::Media(MediaError::InvalidTransition));
        }
        FrameComposition::Directional { kind, .. } if directional_effect(*kind).is_none() => {
            return Err(PreviewError::Media(MediaError::InvalidTransition));
        }
        FrameComposition::Manual {
            kind: TransitionKind::Cut,
            ..
        } => {
            return Err(PreviewError::Media(MediaError::InvalidTransition));
        }
        FrameComposition::Configured { kind, .. }
            if !matches!(kind, TransitionKind::FadeToColor { .. }|TransitionKind::LumaWipe{..}) =>
        {
            return Err(PreviewError::Media(MediaError::InvalidTransition));
        }
        _ => {}
    }
    if let FrameComposition::Configured { kind, .. } | FrameComposition::Manual { kind, .. } =
        output
    {
        skylive_core::TransitionConfig::try_new(*kind, 300)
            .map_err(PreviewError::InvalidCollection)?;
    }
    // A retained visual scene can have disappeared from authored data, but
    // its separate logical Program selection must still be valid.
    Ok(())
}
fn admitted_size(width: u32, height: u32, full_resolution: bool) -> bool {
    width != 0
        && height != 0
        && if full_resolution {
            width <= 4096 && height <= 4096 && u64::from(width) * u64::from(height) <= 8_847_360
        } else {
            width <= MAX_PREVIEW_WIDTH && height <= MAX_PREVIEW_HEIGHT
        }
}
fn blend_endpoints(
    mut from: Option<RenderedScene>,
    mut to: Option<RenderedScene>,
    progress: (u64, u64),
    size: (u32, u32),
    next: Option<u64>,
    kind: TransitionKind,
    luma:Option<&skylive_media::LumaWipeMask>,
) -> Result<RenderedScene, PreviewError> {
    let pixels = (size.0 as usize)
        .checked_mul(size.1 as usize)
        .ok_or(PreviewError::InvalidSize)?;
    let bytes = pixels.checked_mul(4).ok_or(PreviewError::InvalidSize)?;
    let limits = CompositeLimits {
        max_layers: if luma.is_some(){3}else{2},
        max_input_bytes: 2 * bytes+luma.map_or(0,|mask|mask.byte_len()),
        max_output_bytes: bytes,
        max_pixel_visits: (if luma.is_some(){7}else{5}) * pixels as u64,
    };
    let next_after_ns = [
        from.as_ref().and_then(|frame| frame.next_after_ns),
        to.as_ref().and_then(|frame| frame.next_after_ns),
        next,
    ]
    .into_iter()
    .flatten()
    .min();
    let (numerator, denominator) = progress;
    let image = if let TransitionKind::LumaWipe{softness,invert,..}=kind{
        let placeholder=if from.is_none()||to.is_none(){Some(RgbaFrame::solid(size.0,size.1,[0,0,0,0]).map_err(PreviewError::Media)?)}else{None};
        let a=from.as_ref().map(|frame|&frame.image).or(placeholder.as_ref()).ok_or(PreviewError::Media(MediaError::InvalidTransition))?;
        let b=to.as_ref().map(|frame|&frame.image).or(placeholder.as_ref()).ok_or(PreviewError::Media(MediaError::InvalidTransition))?;
        skylive_media::luma_wipe_on_black(a,b,luma.ok_or(PreviewError::Media(MediaError::InvalidTransition))?,numerator,denominator,softness.get(),invert,limits)
    }else if let TransitionKind::FadeToColor { rgb, switch_point } = kind {
        // SDR Fade to Color blends encoded premultiplied values over black.
        // Its primitive returns opaque output; applying linear null-fade
        // flattening here would change a null/white half from 128 to 188.
        let placeholder = if from.is_none() || to.is_none() {
            Some(RgbaFrame::solid(size.0, size.1, [0, 0, 0, 0]).map_err(PreviewError::Media)?)
        } else {
            None
        };
        let a = from
            .as_ref()
            .map(|frame| &frame.image)
            .or(placeholder.as_ref())
            .ok_or(PreviewError::Media(MediaError::InvalidTransition))?;
        let b = to
            .as_ref()
            .map(|frame| &frame.image)
            .or(placeholder.as_ref())
            .ok_or(PreviewError::Media(MediaError::InvalidTransition))?;
        let settings = skylive_media::FadeToColorSettings::new(rgb, switch_point)
            .map_err(PreviewError::Media)?;
        skylive_media::fade_to_color_rgba(a, b, numerator, denominator, settings, limits)
    } else if let Some(effect) = directional_effect(kind) {
        // The OBS transition callback receives a transparent placeholder for
        // a null endpoint, then blends its result over the black output. Keep
        // that distinction: a fabricated opaque black scene is not a null.
        let placeholder = if from.is_none() || to.is_none() {
            Some(RgbaFrame::solid(size.0, size.1, [0, 0, 0, 0]).map_err(PreviewError::Media)?)
        } else {
            None
        };
        let a = from
            .as_ref()
            .map(|frame| &frame.image)
            .or(placeholder.as_ref())
            .ok_or(PreviewError::Media(MediaError::InvalidTransition))?;
        let b = to
            .as_ref()
            .map(|frame| &frame.image)
            .or(placeholder.as_ref())
            .ok_or(PreviewError::Media(MediaError::InvalidTransition))?;
        let sampled = skylive_media::directional_rgba(a, b, numerator, denominator, effect, limits)
            .map_err(PreviewError::Media)?;
        skylive_media::fade_single_on_black(&sampled, 1, 1, limits)
    } else {
        match (&from, &to) {
            (Some(a), Some(b)) => {
                skylive_media::crossfade_rgba(&a.image, &b.image, numerator, denominator, limits)
            }
            (Some(a), None) => skylive_media::fade_single_on_black(
                &a.image,
                denominator - numerator,
                denominator,
                limits,
            ),
            (None, Some(b)) => {
                skylive_media::fade_single_on_black(&b.image, numerator, denominator, limits)
            }
            (None, None) => RgbaFrame::solid(size.0, size.1, [0, 0, 0, 255]),
        }
    }
    .map_err(PreviewError::Media)?;
    // A directional or luma transition can hide either source in ways that
    // require per-pixel attribution. Only propagate proof for plain fades,
    // and only when the endpoint's weight is large enough to survive 8-bit
    // quantisation. The endpoint trace itself is deliberately conservative.
    let mut camera_trace = Vec::new();
    if matches!(kind, TransitionKind::Fade) && denominator != 0 {
        let from_weight = denominator.saturating_sub(numerator);
        if u128::from(from_weight) * 255 >= u128::from(denominator) {
            if let Some(scene) = &from { camera_trace.extend(scene.camera_trace.iter().copied()); }
        }
        if u128::from(numerator) * 255 >= u128::from(denominator) {
            if let Some(scene) = &to {
                for &id in &scene.camera_trace { if !camera_trace.contains(&id) { camera_trace.push(id); } }
            }
        }
    }
    // At the exact endpoints an ordinary fade retains the chosen raster at
    // full weight. Intermediate weights need magnitude-aware contribution
    // proof and intentionally do not propagate pixel witnesses.
    let output_witnesses=if matches!(kind,TransitionKind::Fade)&&denominator!=0 {
        if numerator==0 {from.as_mut().map(|scene|std::mem::take(&mut scene.output_witnesses)).unwrap_or_default()}
        else if numerator==denominator {to.as_mut().map(|scene|std::mem::take(&mut scene.output_witnesses)).unwrap_or_default()}
        else {Vec::new()}
    } else {Vec::new()};
    let mut diagnostics = Vec::<SourceDiagnostic>::new();
    for diagnostic in from
        .into_iter()
        .chain(to)
        .flat_map(|frame| frame.diagnostics)
    {
        if !diagnostics.iter().any(|old| old == &diagnostic) {
            diagnostics.push(diagnostic);
        }
    }
    Ok(RenderedScene {
        image,
        blend: SourceBlend::LinearSrgb,
        diagnostics,
        gpu_notice: None,
        next_after_ns,
        auto_text:Vec::new(),
        camera_trace,
        camera_witnesses: Vec::new(),
        output_witnesses,
    })
}

fn directional_effect(kind: TransitionKind) -> Option<skylive_media::DirectionalTransition> {
    fn direction(value: TransitionDirection) -> skylive_media::Direction {
        match value {
            TransitionDirection::Left => skylive_media::Direction::Left,
            TransitionDirection::Right => skylive_media::Direction::Right,
            TransitionDirection::Up => skylive_media::Direction::Up,
            TransitionDirection::Down => skylive_media::Direction::Down,
        }
    }
    match kind {
        TransitionKind::Swipe {
            direction: value,
            swipe_in,
        } => Some(skylive_media::DirectionalTransition::Swipe {
            direction: direction(value),
            swipe_in,
        }),
        TransitionKind::Slide { direction: value } => {
            Some(skylive_media::DirectionalTransition::Slide {
                direction: direction(value),
            })
        }
        TransitionKind::Cut | TransitionKind::Fade | TransitionKind::FadeToColor { .. }|TransitionKind::LumaWipe{..} => None,
    }
}
/// Frozen transition endpoints use their own pinned auto-text generation;
/// live endpoints use the current shared generation. Both paths retain the
/// pixel lease through compositing and output publication.
fn render_frame_for_output(authored:&StudioSnapshot,live:&StudioSnapshot,view:&FrameScene,
    width:u32,height:u32,playback:&PlaybackSnapshot,resources:&mut RenderResources<'_>)->Result<RenderedScene,PreviewError>{
    let Some(snapshots)=view.auto_text_snapshots() else{return render_frame(live,view,width,height,playback,resources);};
    let mut applicable=Vec::new();
    for snapshot in snapshots {
        // A frozen endpoint can retain the old source even after its live
        // definition was changed or removed. Its SceneInstance supplies the
        // matching retained text/group metadata instead of substituting the
        // new source under the same ID.
        if snapshot.is_pinned()&&snapshot.is_live()&&!snapshot.matches_model(playback.session(),authored){continue;}
        applicable.push(snapshot.clone());
    }
    let pinned=crate::text::auto_worker::project_all(playback.session(),authored,&applicable)
        .map_err(|error|if applicable.iter().any(|snapshot|!snapshot.is_live()){PreviewError::AutoTextRetry}else{PreviewError::InvalidCollection(error)})?;
    let old=std::mem::replace(&mut resources.auto_text,snapshots.to_vec());
    let rendered=render_frame(&pinned,view,width,height,playback,resources);
    resources.auto_text=old;
    rendered
}
fn render_frame(
    collection: &StudioSnapshot,
    view: &FrameScene,
    width: u32,
    height: u32,
    playback: &PlaybackSnapshot,
    resources: &mut RenderResources<'_>,
) -> Result<RenderedScene, PreviewError> {
    render_frame_background(collection,view,width,height,playback,resources,true)
}
/// Internal rendering entry point for a future activated auto-text lifecycle.
/// Its model and leased pixels come from the same guarded worker snapshot. A
/// caller must also invalidate the source's revision on linked-file changes;
/// ordinary Program and Preview entry points do not enable this path yet.
#[allow(dead_code,clippy::too_many_arguments)]
fn render_frame_with_auto_text<'a>(collection:&StudioSnapshot,scene:Id,width:u32,height:u32,
    playback:&PlaybackSnapshot,capture:&'a CaptureSnapshot,cache:&'a mut ClipCache,
    auto:&'a crate::text::auto_worker::Snapshot)->Result<RenderedScene,PreviewError>{
    let projection=auto.project(playback.session(),collection).map_err(PreviewError::InvalidCollection)?;
    let mut resources=RenderResources::new(cache,capture);
    resources.auto_text=vec![auto.clone()];
    let rendered=render_frame(&projection.model,&FrameScene::Live(scene),width,height,playback,&mut resources)?;
    if !auto.is_live(){return Err(PreviewError::AutoTextRetry);}
    Ok(rendered)
}
#[allow(clippy::too_many_arguments)]
fn render_frame_background(
    collection:&StudioSnapshot,view:&FrameScene,width:u32,height:u32,playback:&PlaybackSnapshot,
    resources:&mut RenderResources<'_>,opaque_root:bool,
)->Result<RenderedScene,PreviewError> {
    if !admitted_size(width, height, resources.full_resolution) {
        return Err(PreviewError::InvalidSize);
    }
    collection
        .validate()
        .map_err(PreviewError::InvalidCollection)?;
    view.validate_session(playback.session())
        .map_err(PreviewError::Transition)?;
    view.visible_sources(collection)
        .map_err(PreviewError::Transition)?;
    let root_source = view.root_scene_source(collection);
    let filtered = root_source.is_some_and(|source| {
        source.video_filters().iter().any(|filter| {
            filter.enabled && collection.preferences().plugin_enabled(filter.plugin_id())
        })
    });
    if !filtered {
        return render_scene(
            collection, view, None, width, height, playback, resources, 0, opaque_root,
        );
    }
    // Filters sample intrinsic scene pixels, never downscaled Preview pixels.
    let incomplete_before=resources.incomplete_inputs;
    let raw = render_scene(
        collection,
        view,
        None,
        view.canvas_dimensions(collection).0,
        view.canvas_dimensions(collection).1,
        playback,
        resources,
        0,
        false,
    )?;
    let source = root_source.ok_or(PreviewError::NotFound(view.scene()))?;
    let mut diagnostics = raw.diagnostics;
    resources.effects.temporal_context=playback.temporal.context(source,crate::scroll_animation::root_owner(view)).map(|mut context|{context.input_complete=diagnostics.is_empty()&&resources.incomplete_inputs==incomplete_before;context});
    resources.effects.scroll_context=playback.scroll.context(source,crate::scroll_animation::root_owner(view));
    resources.effects.mask_context=Some((source.id(),playback.mask_animation.clone()));
    let scroll_next=(resources.effects.scroll_context.moving||crate::video_filters::has_render_delay(source.video_filters())||crate::video_filters::has_animated_mask(source.video_filters(),resources.effects.mask_context.as_ref().map(|(_,clock)|clock))).then_some(1_000_000_000u64*u64::from(collection.video().fps_den())/u64::from(collection.video().fps_num()));
    let raw_next=match(raw.next_after_ns,scroll_next){(Some(a),Some(b))=>Some(a.min(b.max(1))),(a,b)=>a.or(b)};
    let mut next_after_ns = if crate::video_filters::has_linked_filter_resources(source.video_filters()) {Some(raw_next.unwrap_or(1_000_000_000).min(1_000_000_000))}else{raw_next};
    let used = resources
        .cache
        .bytes
        .saturating_add(resources.cache.text.bytes)
        .saturating_add(resources.live_bytes);
    let result = resources.effects.apply_with_providers(
        source,
        Arc::new(raw.image),
        raw.blend,
        crate::video_filters::FilterProviders::from_snapshot(collection).with_canvas(view.filter_canvas_dimensions(collection)),
        MAX_RENDER_RESOURCE_BYTES.saturating_sub(used),
        resources.filter_work_remaining(),
    );
    let (input, blend) = match result {
        Ok(filtered) => {
            resources.live_bytes += filtered.new_bytes;
            resources.pixel_visits += filtered.pixel_stages;
            (Some(filtered.image), filtered.blend)
        }
        Err(error) => {
            if matches!(error.code,"mask_revision_pending"|"shared_source_filter_busy") {next_after_ns=Some(next_after_ns.unwrap_or(100_000_000).min(100_000_000));}
            diagnostics.push(SourceDiagnostic {
                source: source.id(),
                kind: error.kind,
                code: error.code,
                detail: error.detail,
            });
            (None, SourceBlend::LinearSrgb)
        }
    };
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or(PreviewError::InvalidSize)?;
    let bytes = pixels.checked_mul(4).ok_or(PreviewError::InvalidSize)?;
    let visits = pixels
        .checked_mul(if input.is_some() { 2 } else { 1 })
        .ok_or(PreviewError::InvalidSize)?;
    if bytes
        > MAX_RENDER_RESOURCE_BYTES.saturating_sub(
            resources
                .cache
                .bytes
                .saturating_add(resources.cache.text.bytes)
                .saturating_add(resources.live_bytes),
        )
        || visits > resources.work_remaining()
    {
        return Err(PreviewError::Media(MediaError::BudgetExceeded));
    }
    resources.live_bytes += bytes;
    resources.pixel_visits += visits;
    let layers = input.as_ref().map(|image| {
        AffineLayer::new(
            image,
            Affine2 {
                xx: if view.is_filter_preview(){f64::from(width)}else{f64::from(width)*f64::from(image.width())/f64::from(view.canvas_dimensions(collection).0)},
                xy: 0.0,
                yx: 0.0,
                yy: if view.is_filter_preview(){f64::from(height)}else{f64::from(height)*f64::from(image.height())/f64::from(view.canvas_dimensions(collection).1)},
                tx: 0.0,
                ty: 0.0,
            },
            UvRect::FULL,
            1.0,
        )
        .with_blend_mode(blend)
    });
    let resize_limits=CompositeLimits {max_layers:1,max_input_bytes:MAX_RENDER_RESOURCE_BYTES,max_output_bytes:bytes,max_pixel_visits:visits as u64};
    let image = if opaque_root {
        composite_affine(width,height,[0,0,0,255],layers.as_slice(),resize_limits)
    }else{
        skylive_media::composite_affine_premultiplied(width,height,layers.as_slice(),resize_limits)
    }
    .map_err(PreviewError::Media)?;
    Ok(RenderedScene {
        image,
        blend: if opaque_root {SourceBlend::LinearSrgb}else{SourceBlend::PremultipliedLinearSrgb},
        diagnostics,
        gpu_notice: None,
        next_after_ns,
        auto_text:Vec::new(),
        // Scene-level filters can hold or replace an earlier frame, or remove
        // its pixels. Their source provenance is not yet represented.
        camera_trace: Vec::new(),
        camera_witnesses: Vec::new(),
        output_witnesses: Vec::new(),
    })
}
#[allow(clippy::too_many_arguments)]
fn render_scene(
    collection: &StudioSnapshot,
    view: &FrameScene,
    nested: Option<Id>,
    width: u32,
    height: u32,
    playback: &PlaybackSnapshot,
    resources: &mut RenderResources<'_>,
    depth: usize,
    opaque_root: bool,
) -> Result<RenderedScene, PreviewError> {
    if depth > skylive_core::MAX_SCENE_DEPTH {
        return Err(PreviewError::Transition(
            crate::transition::TransitionError::Limit("nested raster depth limit"),
        ));
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or(PreviewError::InvalidSize)?;
    let bytes = pixels.checked_mul(4).ok_or(PreviewError::InvalidSize)?;
    let combined = resources
        .cache
        .bytes
        .saturating_add(resources.cache.text.bytes)
        .saturating_add(resources.live_bytes);
    if bytes > MAX_RENDER_RESOURCE_BYTES.saturating_sub(combined)
        || pixels > resources.work_remaining()
    {
        return Err(PreviewError::Transition(
            crate::transition::TransitionError::Limit("nested raster resource/work budget"),
        ));
    }
    resources.live_bytes += bytes;
    resources.pixel_visits += pixels;
    let items = if let Some(scene) = nested {
        view.scene_items(collection, scene)
    } else {
        view.items(collection)
    }
    .map_err(PreviewError::Transition)?;
    let scale_x = if nested.is_none() {
        f64::from(width) / f64::from(view.canvas_dimensions(collection).0)
    } else {
        1.0
    };
    let scale_y = if nested.is_none() {
        f64::from(height) / f64::from(view.canvas_dimensions(collection).1)
    } else {
        1.0
    };
    let mut inputs:Vec<skylive_media::SharedVideoFrame> = Vec::with_capacity(items.len());
    let mut nested_witnesses:HashMap<usize,Vec<(Id,[f64;2])>> = HashMap::new();
    let mut nested_witness_count=0usize;
    let mut properties = Vec::with_capacity(items.len());
    // A bounded set of pixel-center witnesses for camera layers. Later
    // layers are conservatively treated as covering their whole affine AABB.
    let mut camera_candidates:Vec<(Id,usize,[[f64;2];5])> = Vec::new();
    let mut resolved = HashMap::<Id, Option<usize>>::new();
    let mut encodings = HashMap::<Id, SourceBlend>::new();
    let mut diagnostics = Vec::new();
    let mut next_after_ns: Option<u64> = None;
    for item in items.iter().filter(|i| i.visible()) {
        let source = view
            .source(collection, item.source())
            .ok_or(PreviewError::NotFound(item.source()))?;
        // An audio scene item contributes activation/routing, never pixels or
        // synthetic geometry. The renderer does not start device acquisition.
        let Some(authored_dimensions) = view
            .source_video_dimensions(collection, source)
            .map_err(PreviewError::Transition)?
        else {
            continue;
        };
        let (basis_w, basis_h) = if nested.is_none() {
            view.captured_dimensions(source.id())
                .unwrap_or(authored_dimensions)
        } else {
            authored_dimensions
        };
        encodings
            .entry(source.id())
            .or_insert_with(|| source_blend(source));
        let input = if let Some(input) = resolved.get(&source.id()) {
            *input
        } else {
            let incomplete_before=resources.incomplete_inputs;let diagnostic_start=diagnostics.len();
            let index = match source.kind() {
                SourceKind::Scene { .. } | SourceKind::Group { .. } => {
                    let scene = source
                        .kind()
                        .nested_scene()
                        .ok_or(PreviewError::NotFound(source.id()))?;
                    let dimensions = match source.kind() {
                        SourceKind::Group { group } => group.basis(),
                        _ => view.filter_canvas_dimensions(collection),
                    };
                    let key = (view.key(), scene);
                    let frame = if let Some(frame) = resources.nested.get(&key) {
                        frame.clone()
                    } else {
                        if resources.nested.len()
                            >= 3 * (skylive_core::MAX_SCENES + skylive_core::MAX_SOURCES)
                        {
                            return Err(PreviewError::Transition(
                                crate::transition::TransitionError::Limit(
                                    "nested raster cache count",
                                ),
                            ));
                        }
                        let nested_incomplete_before=resources.incomplete_inputs;
                        let rendered = render_scene(
                            collection,
                            view,
                            Some(scene),
                            dimensions.0,
                            dimensions.1,
                            playback,
                            resources,
                            depth + 1,
                            false,
                        )?;
                        let witness_bytes=rendered.camera_witnesses.len()
                            .checked_mul(std::mem::size_of::<(Id,[f64;2])>())
                            .and_then(|bytes|bytes.checked_add(std::mem::size_of::<Vec<(Id,[f64;2])>>() + 2*std::mem::size_of::<usize>()))
                            .ok_or(PreviewError::Media(MediaError::BudgetExceeded))?;
                        let used=resources.cache.bytes.saturating_add(resources.cache.text.bytes).saturating_add(resources.live_bytes);
                        if witness_bytes>MAX_RENDER_RESOURCE_BYTES.saturating_sub(used) {return Err(PreviewError::Media(MediaError::BudgetExceeded));}
                        resources.live_bytes+=witness_bytes;
                        let frame = NestedFrame {
                            complete:rendered.diagnostics.is_empty()&&resources.incomplete_inputs==nested_incomplete_before,
                            image: Arc::new(rendered.image),
                            blend: rendered.blend,
                            next_after_ns: rendered.next_after_ns,
                            camera_witnesses: Arc::new(rendered.camera_witnesses),
                        };
                        resources.nested.insert(key, frame.clone());
                        frame
                    };
                    if !frame.complete{resources.incomplete()?;}
                    if let Some(after) = frame.next_after_ns {
                        next_after_ns = Some(next_after_ns.map_or(after, |old| old.min(after)));
                    }
                    let index = inputs.len();
                    encodings.insert(source.id(), frame.blend);
                    if frame.complete && !frame.camera_witnesses.is_empty()
                        && !source.video_filters().iter().any(|filter| filter.enabled && collection.preferences().plugin_enabled(filter.plugin_id())) {
                        let count=frame.camera_witnesses.len().min((skylive_core::MAX_SOURCES*5).saturating_sub(nested_witness_count));
                        if count!=0 {
                            let mut retained=Vec::new();
                            retained.try_reserve_exact(count).map_err(|_|PreviewError::Media(MediaError::BudgetExceeded))?;
                            retained.extend_from_slice(&frame.camera_witnesses[..count]);
                            nested_witnesses.try_reserve(1).map_err(|_|PreviewError::Media(MediaError::BudgetExceeded))?;
                            nested_witnesses.insert(index,retained);nested_witness_count+=count;
                        }
                    }
                    inputs.push(frame.image.into());
                    Some(index)
                }
                SourceKind::AudioCapture { .. } => None,
                SourceKind::MediaSource { media } if media.video_dimensions().is_none() => None,
                SourceKind::MediaSource { media }
                    if resources.capture.media_is_empty(source.id(), media) =>
                {
                    None
                }
                SourceKind::Color { rgba, .. } => {
                    let index = inputs.len();
                    let dimensions=if color_needs_intrinsic_pixels(source,collection) {authored_dimensions}else{(1,1)};
                    let bytes=dimensions.0 as usize*dimensions.1 as usize*4;
                    let used=resources.cache.bytes.saturating_add(resources.cache.text.bytes).saturating_add(resources.live_bytes);
                    if bytes>MAX_RENDER_RESOURCE_BYTES.saturating_sub(used) || bytes/4>resources.work_remaining() {return Err(PreviewError::Media(MediaError::BudgetExceeded));}
                    resources.live_bytes+=bytes;resources.pixel_visits+=bytes/4;
                    inputs.push(Arc::new(RgbaFrame::solid(dimensions.0,dimensions.1,*rgba).map_err(PreviewError::Media)?).into());
                    Some(index)
                }
                SourceKind::Text { settings } => {
                    if let Some(auto)=resources.auto_text.iter().find(|snapshot|snapshot.source()==source.id()) {
                        if settings.file().is_some(){next_after_ns=Some(next_after_ns.unwrap_or(250_000_000).min(250_000_000));}
                        if !auto.is_live(){return Err(PreviewError::AutoTextRetry);}
                        if !resources.used_auto_text.iter().any(|snapshot|snapshot.same(auto)){
                            if resources.used_auto_text.len()>=2*skylive_core::MAX_SOURCES{return Err(PreviewError::Media(MediaError::BudgetExceeded));}
                            resources.used_auto_text.push(auto.clone());
                        }
                        if let Some(detail)=auto.failure(){diagnostics.push(SourceDiagnostic{
                            source:source.id(),kind:crate::resource_issue::ResourceIssueKind::Unavailable,
                            code:"text_auto_prepare",detail,
                        });}
                        let charge=auto.raster().byte_len();
                        if charge>MAX_RENDER_RESOURCE_BYTES.saturating_sub(resources.cache.bytes.saturating_add(resources.live_bytes)){
                            return Err(PreviewError::Media(MediaError::BudgetExceeded));
                        }
                        resources.live_bytes+=charge;
                        let index=inputs.len();
                        inputs.push(auto.shared_frame());
                        Some(index)
                    } else {
                    if settings.file().is_some() { next_after_ns=Some(next_after_ns.unwrap_or(250_000_000).min(250_000_000)); }
                    let remaining = MAX_RENDER_RESOURCE_BYTES
                        .saturating_sub(resources.cache.bytes.saturating_add(resources.live_bytes));
                    match resources.cache.text.load_with_fonts(settings, remaining,&mut resources.font_reads,&mut resources.text_reads) {
                        Ok(image) => {
                            let index = inputs.len();
                            inputs.push(image.into());
                            Some(index)
                        }
                        Err(error) => {
                            // During a changing-file read retain the last complete
                            // raster for this exact text configuration. The
                            // diagnostic still marks the snapshot incomplete.
                            let previous = if crate::text::retryable_file_read(&error) {
                                resources.cache.text.previous_file_frame(settings)
                            } else { None };
                            diagnostics.push(SourceDiagnostic {
                                source: source.id(),
                                kind: error.kind,
                                code: error.code,
                                detail: error.detail,
                            });
                            previous.map(|image| { let index = inputs.len(); inputs.push(image.into()); index })
                        }
                    }
                    }
                }
                SourceKind::ImageSlideShow {settings} => {
                    match crate::image_slideshow::sample_playback(settings,source.id(),playback){Err(detail)=>{diagnostics.push(SourceDiagnostic{source:source.id(),kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"slideshow_clock_required",detail:detail.into()});None},Ok(position)=>{
                        if let Some(after)=position.next_after_ns{next_after_ns=Some(next_after_ns.map_or(after,|old|old.min(after)));}
                        match image_slideshow::render(settings,position,resources,&mut next_after_ns){Ok(Some((image,blend)))=>{let index=inputs.len();encodings.insert(source.id(),blend);inputs.push(image.into());Some(index)},Ok(None)=>None,Err((kind,code,detail))=>{diagnostics.push(SourceDiagnostic{source:source.id(),kind,code,detail});None}}
                    }}
                }
                SourceKind::Image { asset } => {
                    let result = resources.cache.load_image(
                        asset,
                        source.image_settings().linear_alpha,
                        &mut resources.reads,
                        &mut resources.decoded,
                        resources.live_bytes,
                    );
                    match result {
                        Ok(clip) => {
                            let sample = clip.sample(playback.elapsed(source.id()));
                            if let Some(after) = sample.next_after_ns {
                                next_after_ns =
                                    Some(next_after_ns.map_or(after, |old| old.min(after)));
                            }
                            let index = inputs.len();
                            inputs.push(sample.image.into());
                            Some(index)
                        }
                        Err((kind, code, detail)) => {
                            diagnostics.push(SourceDiagnostic {
                                source: source.id(),
                                kind,
                                code,
                                detail,
                            });
                            None
                        }
                    }
                }
                SourceKind::NdiSource {..} if !collection.preferences().plugin_enabled(skylive_core::NDI_PLUGIN)=>{diagnostics.push(SourceDiagnostic {source:source.id(),kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"ndi_module_disabled",detail:"Enable the DistroAV NDI source module to receive this source".into()});None},
                SourceKind::DisplayCapture { .. }
                | SourceKind::WindowCapture { .. }
                | SourceKind::VideoCapture { .. }
                | SourceKind::MediaSource { .. } | SourceKind::NdiSource {..} | SourceKind::Browser {..} | SourceKind::GameCapture {..} => {
                    let problem = resources.capture.problem(source.id());
                    if let Some(problem) = problem {
                        diagnostics.push(SourceDiagnostic {
                            source: source.id(),
                            kind: problem.kind,
                            code: problem.code,
                            detail: problem.detail.clone(),
                        });
                    }
                    match resources
                        .capture
                        .frame(source.id())
                        .filter(|frame| frame.binding.matches_source(source.kind()))
                    {
                        Some(frame) => {
                            if frame.window.is_some_and(|metadata| metadata.encoding==crate::window_capture::WindowFrameEncoding::PremultipliedLinearSrgb) {encodings.insert(source.id(),SourceBlend::PremultipliedLinearSrgb);}
                            let image = frame.image.clone();
                            let pointer = image.pixels().as_ptr() as usize;
                            let charge = if resources.live_buffers.contains(&pointer) {
                                0
                            } else {
                                image.byte_len()
                            };
                            if resources
                                .cache
                                .bytes
                                .saturating_add(resources.cache.text.bytes)
                                .saturating_add(resources.live_bytes)
                                .saturating_add(charge)
                                > MAX_RENDER_RESOURCE_BYTES
                            {
                                diagnostics.push(SourceDiagnostic {
                                    source: source.id(),
                                    kind: crate::resource_issue::ResourceIssueKind::Limit,
                                    code: "capture_budget_exceeded",
                                    detail: "shared image/display render byte budget".into(),
                                });
                                None
                            } else {
                                resources.live_bytes += charge;
                                resources.live_buffers.insert(pointer);
                                let index = inputs.len();
                                inputs.push(image);
                                Some(index)
                            }
                        }
                        None => {
                            if problem.is_none() {
                                diagnostics.push(SourceDiagnostic {
                                    source: source.id(),
                                    kind: crate::resource_issue::ResourceIssueKind::Unavailable,
                                    code: "capture_frame_unavailable",
                                    detail:
                                        "display capture has no admitted frame for this binding"
                                            .into(),
                                });
                            }
                            None
                        }
                    }
                }
            };
            resources.effects.temporal_context=playback.temporal.context(source,crate::scroll_animation::source_owner(view,source)).map(|mut context|{context.input_complete=diagnostics.len()==diagnostic_start&&resources.incomplete_inputs==incomplete_before;context});
            resources.effects.scroll_context=playback.scroll.context(source,crate::scroll_animation::source_owner(view,source));
            resources.effects.mask_context=Some((source.id(),playback.mask_animation.clone()));
            // Only complete, native camera frames can establish a fallback
            // scope. Synthetic captures and changed streams remain fail-closed.
            resources.effects.camera_scope=if diagnostics.len()==diagnostic_start&&resources.incomplete_inputs==incomplete_before
                &&matches!(source.kind(),SourceKind::VideoCapture{..})&&resources.capture.session()==playback.session(){
                resources.capture.frame(source.id()).filter(|frame|frame.binding.matches_source(source.kind())).and_then(|frame|{
                    match frame.native_origin {
                        Some(origin @ crate::capture::NativeFrameOrigin::Camera{..})=>Some(crate::video_filters::CameraScope {
                            session:playback.session(),source:source.id(),binding:frame.binding.clone(),origin,
                        }),_=>None,
                    }
                })
            }else{None};
            if resources.effects.scroll_context.moving||crate::video_filters::has_render_delay(source.video_filters())||crate::video_filters::has_animated_mask(source.video_filters(),resources.effects.mask_context.as_ref().map(|(_,clock)|clock)){let after=(1_000_000_000u64*u64::from(collection.video().fps_den())/u64::from(collection.video().fps_num())).max(1);next_after_ns=Some(next_after_ns.map_or(after,|old|old.min(after)));}
            if crate::video_filters::has_linked_filter_resources(source.video_filters()) {next_after_ns=Some(next_after_ns.unwrap_or(1_000_000_000).min(1_000_000_000));}
            let index = if let Some(index) = index {
                let used = resources
                    .cache
                    .bytes
                    .saturating_add(resources.cache.text.bytes)
                    .saturating_add(resources.live_bytes);
                let providers=crate::video_filters::FilterProviders::from_snapshot(collection).with_canvas(view.filter_canvas_dimensions(collection));
                let async_delay_applied=resources.capture.session()==playback.session()
                    && resources.capture.frame(source.id()).is_some_and(|frame|
                        crate::video_filters::source_video_delay_applied(source,playback.session(),frame,&inputs[index],providers)
                            || resources.recording_ndi && crate::video_filters::ndi_recording_delay_applied(source,playback.session(),frame,&inputs[index],providers));
                let async_guard=if async_delay_applied {resources.capture.frame(source.id()).and_then(crate::video_filters::AsyncCameraGuard::from_frame)}else{None};
                match resources.effects.apply_with_shared_frame(
                    source,
                    inputs[index].clone(),
                    encodings
                        .get(&source.id())
                        .copied()
                        .unwrap_or_else(|| source_blend(source)),
                    providers,
                    MAX_RENDER_RESOURCE_BYTES.saturating_sub(used),
                    resources.filter_work_remaining(),
                    async_delay_applied,
                    async_guard,
                ) {
                    Ok(filtered) => {
                        if async_delay_applied && !resources.capture.frame(source.id()).is_some_and(|frame|
                            crate::video_filters::source_video_delay_applied(source,playback.session(),frame,&inputs[index],providers)
                                || resources.recording_ndi && crate::video_filters::ndi_recording_delay_applied(source,playback.session(),frame,&inputs[index],providers)) {
                            diagnostics.push(SourceDiagnostic { source: source.id(),
                                kind: crate::resource_issue::ResourceIssueKind::Unavailable,
                                code:"async_camera_frame_stale", detail:"Camera delay scope changed during rendering".into() });
                            None
                        } else {
                            resources.live_bytes += filtered.new_bytes;
                            resources.pixel_visits += filtered.pixel_stages;
                            inputs[index] = filtered.image;
                            encodings.insert(source.id(), filtered.blend);
                            Some(index)
                        }
                    }
                    Err(error) => {
                        if matches!(error.code,"mask_revision_pending"|"shared_source_filter_busy") {next_after_ns=Some(next_after_ns.unwrap_or(100_000_000).min(100_000_000));}
                        let retryable=matches!(error.kind,crate::resource_issue::ResourceIssueKind::Unavailable|crate::resource_issue::ResourceIssueKind::Limit);
                        let fallback=if retryable{
                            inputs[index].direct_arc().and_then(|input|resources.effects.camera_fallback(source,&input,encodings.get(&source.id()).copied().unwrap_or_else(||source_blend(source)),
                                crate::video_filters::FilterProviders::from_snapshot(collection).with_canvas(view.filter_canvas_dimensions(collection)),
                                MAX_RENDER_RESOURCE_BYTES.saturating_sub(used)))
                        }else{None};
                        // Keep a live camera visible when only color correction
                        // failed and no previously filtered frame is available.
                        // Never bypass a key, mask, crop, or other active stage.
                        let raw_fallback=fallback.is_none()&&retryable&&resources.effects.camera_scope.is_some()
                            &&crate::video_filters::camera_color_only(source.video_filters(),crate::video_filters::FilterProviders::from_snapshot(collection));
                        diagnostics.push(SourceDiagnostic {
                            source: source.id(),
                            kind: error.kind,
                            code: error.code,
                            detail: if raw_fallback {format!("{}; showing the unfiltered camera until color correction recovers",error.detail)} else {error.detail},
                        });
                        if let Some(filtered)=fallback {
                            resources.live_bytes+=filtered.new_bytes;
                            inputs[index]=filtered.image.into();
                            encodings.insert(source.id(),filtered.blend);
                            Some(index)
                        }else if raw_fallback {Some(index)}else{None}
                    }
                }
            } else {
                None
            };
            resolved.insert(source.id(), index);
            index
        };
        let Some(input) = input else {
            continue;
        };
        let t = item.transform();
        let (source_w,source_h)=if matches!(source.kind(),SourceKind::Color {..})
            && !color_needs_intrinsic_pixels(source,collection) {
            authored_dimensions
        }else{(inputs[input].width(),inputs[input].height())};
        let geometry = match t.geometry_for_frame(basis_w, basis_h, source_w, source_h) {
            Ok(geometry) => geometry,
            Err(error)
                if matches!(
                    source.kind(),
                    SourceKind::DisplayCapture { .. }
                        | SourceKind::WindowCapture { .. }
                        | SourceKind::VideoCapture { .. }
                        | SourceKind::MediaSource { .. } | SourceKind::NdiSource {..} | SourceKind::Browser {..} | SourceKind::GameCapture {..}
                ) =>
            {
                diagnostics.push(SourceDiagnostic {
                    source: source.id(),
                    kind: crate::resource_issue::ResourceIssueKind::Unavailable,
                    code: "capture_geometry_unavailable",
                    detail: error.to_string(),
                });
                continue;
            }
            Err(error) => return Err(PreviewError::InvalidCollection(error)),
        };
        // Until composition carries per-pixel attribution, use pixel-center
        // witnesses in a normal camera layer. Temporal filters can
        // replace the current sample and are therefore not certified here.
        let xx=geometry.axis_x[0]*scale_x;let xy=geometry.axis_y[0]*scale_x;
        let yx=geometry.axis_x[1]*scale_y;let yy=geometry.axis_y[1]*scale_y;
        let tx=geometry.origin[0]*scale_x;let ty=geometry.origin[1]*scale_y;
        let determinant=xx*yy-xy*yx;
        if (matches!(source.kind(),SourceKind::VideoCapture{..}) || matches!(source.kind(),SourceKind::NdiSource{..}))
            && t.opacity>0.0 && item.compositing()==skylive_core::ItemCompositing::default()
            && determinant.is_finite() && determinant.abs()>1e-12
            && !source.video_filters().iter().any(|filter|filter.enabled && collection.preferences().plugin_enabled(filter.plugin_id())
                && matches!(filter.effect,skylive_core::VideoEffect::Freeze{..}|skylive_core::VideoEffect::RenderDelay{delay_ms:1..=u16::MAX}))
            && resources.capture.frame(source.id()).is_some_and(|frame|
                crate::video_filters::source_video_delay_applied(source,playback.session(),frame,&frame.image,
                    crate::video_filters::FilterProviders::from_snapshot(collection))
                    || resources.recording_ndi && crate::video_filters::ndi_recording_delay_applied(source,playback.session(),frame,&frame.image,
                        crate::video_filters::FilterProviders::from_snapshot(collection)))
            && resources.work_remaining()>=5 {
            // Round candidate local positions to actual destination pixel
            // centers, then invert the affine to exclude points outside a
            // rotated/cropped layer or the viewport.
            let point=|(u,v):(f64,f64)| {
                let x=(tx+xx*u+xy*v).floor()+0.5;let y=(ty+yx*u+yy*v).floor()+0.5;
                let scale=xx.abs().max(xy.abs()).max(yx.abs()).max(yy.abs());
                if !scale.is_finite() || scale==0.0 {return [f64::NAN,f64::NAN];}
                let (a,b,c,d)=(xx/scale,xy/scale,yx/scale,yy/scale);let det=a*d-b*c;
                if !det.is_finite() || det.abs()<1e-12 {return [f64::NAN,f64::NAN];}
                let (dx,dy)=((x-tx)/scale,(y-ty)/scale);
                let local_u=(d*dx-b*dy)/det;let local_v=(a*dy-c*dx)/det;
                if x>=0.0 && x<f64::from(width) && y>=0.0 && y<f64::from(height)
                    && (0.0..1.0).contains(&local_u) && (0.0..1.0).contains(&local_v) {
                    // The default compositor uses nearest-texel sampling in
                    // the cropped window. Check the exact sampled alpha, not
                    // the alpha of an unrelated pixel in the source image.
                    let crop=t.crop;
                    let crop_width=source_w-crop.left-crop.right;
                    let crop_height=source_h-crop.top-crop.bottom;
                    let (fx,fy)=(local_u*f64::from(crop_width),local_v*f64::from(crop_height));
                    if fx.fract()<1e-9 || fy.fract()<1e-9 || fx.fract()>1.0-1e-9 || fy.fract()>1.0-1e-9 {return [f64::NAN,f64::NAN];}
                    let sx=crop.left+((fx.floor() as u32).min(crop_width-1));
                    let sy=crop.top+((fy.floor() as u32).min(crop_height-1));
                    let alpha=inputs[input].pixels()[(sy as usize*source_w as usize+sx as usize)*4+3];
                    // Require more than one alpha quantisation step so a
                    // partially transparent witness is not rounded to zero.
                    if f64::from(alpha)*f64::from(t.opacity)>=2.0 {return [x,y];}
                }
                [f64::NAN,f64::NAN]
            };
            resources.pixel_visits+=5;
            camera_candidates.try_reserve(1).map_err(|_|PreviewError::Media(MediaError::BudgetExceeded))?;
            camera_candidates.push((source.id(),properties.len(),[point((0.5,0.5)),point((0.25,0.25)),point((0.75,0.25)),point((0.25,0.75)),point((0.75,0.75))]));
        }
        // Preserve a nested witness only when the parent's nearest-neighbour
        // sample selects that exact child pixel after crop and transform.
        // Scene-level filters and non-default compositing are excluded: their
        // output cannot be attributed by this pixel-only proof.
        if matches!(source.kind(),SourceKind::Scene{..}|SourceKind::Group{..})
            && t.opacity>0.0 && item.compositing()==skylive_core::ItemCompositing::default()
            && determinant.is_finite() && determinant.abs()>1e-12 {
            if let Some(proofs)=nested_witnesses.get(&input) {
                let crop=t.crop;let cw=source_w-crop.left-crop.right;let ch=source_h-crop.top-crop.bottom;
                for &(id,child) in proofs {
                    if camera_candidates.len()>=skylive_core::MAX_SOURCES*5 || resources.work_remaining()==0 {break;}
                    resources.pixel_visits+=1;
                    if !child[0].is_finite() || !child[1].is_finite() {continue;}
                    let (cx,cy)=(child[0].floor(),child[1].floor());
                    if cx<f64::from(crop.left) || cy<f64::from(crop.top)
                        || cx>=f64::from(crop.left+cw) || cy>=f64::from(crop.top+ch) {continue;}
                    let (u,v)=((cx+0.5-f64::from(crop.left))/f64::from(cw),(cy+0.5-f64::from(crop.top))/f64::from(ch));
                    let (x,y)=((tx+xx*u+xy*v).floor()+0.5,(ty+yx*u+yy*v).floor()+0.5);
                    if x<0.0 || x>=f64::from(width) || y<0.0 || y>=f64::from(height) {continue;}
                    // Mirror the compositor's normalized inverse, and reject
                    // numerical and texel-boundary ambiguity conservatively.
                    let scale=xx.abs().max(xy.abs()).max(yx.abs()).max(yy.abs());
                    if !scale.is_finite() || scale==0.0 {continue;}
                    let (a,b,c,d)=(xx/scale,xy/scale,yx/scale,yy/scale);let det=a*d-b*c;
                    if !det.is_finite() || det.abs()<1e-12 {continue;}
                    let (dx,dy)=((x-tx)/scale,(y-ty)/scale);
                    let (ru,rv)=((d*dx-b*dy)/det,(a*dy-c*dx)/det);
                    if !ru.is_finite() || !rv.is_finite() || !(0.0..1.0).contains(&ru) || !(0.0..1.0).contains(&rv) {continue;}
                    let (sx,sy)=(ru*f64::from(cw),rv*f64::from(ch));
                    if sx.fract()<1e-9 || sy.fract()<1e-9 || sx.fract()>1.0-1e-9 || sy.fract()>1.0-1e-9 {continue;}
                    if crop.left+(sx.floor() as u32).min(cw-1)!=cx as u32 || crop.top+(sy.floor() as u32).min(ch-1)!=cy as u32 {continue;}
                    let offset=((cy as usize)*source_w as usize+(cx as usize))*4+3;
                    if inputs[input].pixels().get(offset).is_none_or(|alpha|f64::from(*alpha)*f64::from(t.opacity)<2.0) {continue;}
                    camera_candidates.try_reserve(1).map_err(|_|PreviewError::Media(MediaError::BudgetExceeded))?;
                    camera_candidates.push((id,properties.len(),[[x,y],[f64::NAN,f64::NAN],[f64::NAN,f64::NAN],[f64::NAN,f64::NAN],[f64::NAN,f64::NAN]]));
                }
            }
        }
        properties.push((
            input,
            Affine2 {
                xx: geometry.axis_x[0] * scale_x,
                xy: geometry.axis_y[0] * scale_x,
                yx: geometry.axis_x[1] * scale_y,
                yy: geometry.axis_y[1] * scale_y,
                tx: geometry.origin[0] * scale_x,
                ty: geometry.origin[1] * scale_y,
            },
            match source.kind() {
                SourceKind::Group { .. }
                | SourceKind::Scene { .. }
                | SourceKind::Text { .. }
                | SourceKind::Image { .. }
                | SourceKind::ImageSlideShow { .. }
                | SourceKind::DisplayCapture { .. }
                | SourceKind::WindowCapture { .. }
                | SourceKind::VideoCapture { .. }
                | SourceKind::MediaSource { .. } | SourceKind::NdiSource {..} | SourceKind::Browser {..} | SourceKind::GameCapture {..} => Some(TexelRect {
                    x: t.crop.left,
                    y: t.crop.top,
                    width: source_w - t.crop.left - t.crop.right,
                    height: source_h - t.crop.top - t.crop.bottom,
                }),
                SourceKind::Color {..} if color_needs_intrinsic_pixels(source,collection)=>Some(TexelRect {x:t.crop.left,y:t.crop.top,width:source_w-t.crop.left-t.crop.right,height:source_h-t.crop.top-t.crop.bottom}),
                SourceKind::Color { .. } | SourceKind::AudioCapture { .. } => None,
            },
            t.opacity,
            encodings
                .get(&source.id())
                .copied()
                .unwrap_or_else(|| source_blend(source)),
            item.compositing(),
        ));
    }
    let trace_work=camera_candidates.len().checked_mul(properties.len()).and_then(|n|n.checked_mul(5));
    let can_trace=trace_work.is_some_and(|work|work<=resources.work_remaining());
    if let Some(work)=trace_work.filter(|_|can_trace) {resources.pixel_visits+=work;}
    let mut camera_trace=Vec::new();
    let mut camera_witnesses=Vec::new();
    let mut output_witnesses=Vec::new();
    if can_trace {camera_trace.try_reserve(camera_candidates.len()).map_err(|_|PreviewError::Media(MediaError::BudgetExceeded))?;}
    if can_trace && opaque_root {
        let slots=camera_candidates.len().saturating_mul(5).min(skylive_core::MAX_SOURCES*5);
        let envelope=slots.checked_mul(2).and_then(|n|n.checked_mul(std::mem::size_of::<(Id,[f64;2])>())).ok_or(PreviewError::Media(MediaError::BudgetExceeded))?;
        let used=resources.cache.bytes.saturating_add(resources.cache.text.bytes).saturating_add(resources.live_bytes);
        if envelope>MAX_RENDER_RESOURCE_BYTES.saturating_sub(used) {return Err(PreviewError::Media(MediaError::BudgetExceeded));}
        output_witnesses.try_reserve_exact(slots).map_err(|_|PreviewError::Media(MediaError::BudgetExceeded))?;
        if output_witnesses.capacity()>slots.saturating_mul(2) {return Err(PreviewError::Media(MediaError::BudgetExceeded));}
        resources.live_bytes+=output_witnesses.capacity()*std::mem::size_of::<(Id,[f64;2])>();
    }
    for (id,index,witnesses) in camera_candidates.into_iter().filter(|_|can_trace) {
        for point in witnesses.iter().filter(|point|point[0].is_finite() && point[1].is_finite() && properties.iter().skip(index+1).all(|(input,affine,window,opacity,_,compositing)| {
            if *opacity==0.0 {return true;}
            let xs=[affine.tx,affine.tx+affine.xx,affine.tx+affine.xy,affine.tx+affine.xx+affine.xy];
            let ys=[affine.ty,affine.ty+affine.yx,affine.ty+affine.yy,affine.ty+affine.yx+affine.yy];
            let left=xs.into_iter().fold(f64::INFINITY,f64::min);let right=xs.into_iter().fold(f64::NEG_INFINITY,f64::max);
            let top=ys.into_iter().fold(f64::INFINITY,f64::min);let bottom=ys.into_iter().fold(f64::NEG_INFINITY,f64::max);
            if point[0]<left || point[0]>=right || point[1]<top || point[1]>=bottom {return true;}
            // Normal source-over cannot change a witness when the sampled
            // texel is all zero. Use the same normalized inverse as the media
            // compositor and admit only points safely inside a texel; a point
            // near a rounding boundary remains conservatively occluded.
            if *compositing!=skylive_core::ItemCompositing::default() {return false;}
            let scale=affine.xx.abs().max(affine.xy.abs()).max(affine.yx.abs()).max(affine.yy.abs());
            if !scale.is_finite() || scale==0.0 {return false;}
            let (a,b,c,d)=(affine.xx/scale,affine.xy/scale,affine.yx/scale,affine.yy/scale);
            let det=a*d-b*c;if !det.is_finite() || det.abs()<1e-12 {return false;}
            let (dx,dy)=((point[0]-affine.tx)/scale,(point[1]-affine.ty)/scale);
            let (u,v)=((d*dx-b*dy)/det,(a*dy-c*dx)/det);
            if !u.is_finite() || !v.is_finite() {return false;}
            // Do not infer invisibility near the affine boundary.
            if u<0.0 || u>=1.0 || v<0.0 || v>=1.0 {return false;}
            let frame=&inputs[*input];
            let (x,y,w,h)=window.map_or((0,0,frame.width(),frame.height()),|rect|(rect.x,rect.y,rect.width,rect.height));
            if w==0 || h==0 {return false;}
            let (fx,fy)=(u*f64::from(w),v*f64::from(h));
            // Neighboring texels may differ; reject unstable boundary cases.
            if fx.fract()<1e-9 || fy.fract()<1e-9 || fx.fract()>1.0-1e-9 || fy.fract()>1.0-1e-9 {return false;}
            let sx=x+((fx.floor() as u32).min(w-1));
            let sy=y+((fy.floor() as u32).min(h-1));
            if sx>=frame.width() || sy>=frame.height() {return false;}
            let offset=(sy as usize*frame.width() as usize+sx as usize)*4;
            frame.pixels().get(offset..offset+4)==Some(&[0,0,0,0][..])
        })) {
            if !camera_trace.contains(&id) {camera_trace.push(id);}
            if opaque_root && output_witnesses.len()<skylive_core::MAX_SOURCES*5 {
                output_witnesses.push((id,*point));
            }
            // A fully opaque source-over sample hides all earlier layers, so
            // it can be transferred even when the camera is not the first
            // layer. Inspect its own sampled alpha: final canvas opacity alone
            // could come from an opaque background behind a translucent camera.
            let opaque_sample = || {
                let (input,affine,window,opacity,_,compositing)=&properties[index];
                if *opacity != 1.0 || *compositing != skylive_core::ItemCompositing::default() { return false; }
                let scale=affine.xx.abs().max(affine.xy.abs()).max(affine.yx.abs()).max(affine.yy.abs());
                if !scale.is_finite() || scale==0.0 { return false; }
                let (a,b,c,d)=(affine.xx/scale,affine.xy/scale,affine.yx/scale,affine.yy/scale);
                let det=a*d-b*c;if !det.is_finite() || det.abs()<1e-12 {return false;}
                let (dx,dy)=((point[0]-affine.tx)/scale,(point[1]-affine.ty)/scale);
                let (u,v)=((d*dx-b*dy)/det,(a*dy-c*dx)/det);
                if !u.is_finite() || !v.is_finite() || !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {return false;}
                let frame=&inputs[*input];
                let (x,y,w,h)=window.map_or((0,0,frame.width(),frame.height()),|rect|(rect.x,rect.y,rect.width,rect.height));
                if w==0 || h==0 {return false;}
                let (fx,fy)=(u*f64::from(w),v*f64::from(h));
                if fx.fract()<1e-9 || fy.fract()<1e-9 || fx.fract()>1.0-1e-9 || fy.fract()>1.0-1e-9 {return false;}
                let sx=x+((fx.floor() as u32).min(w-1));let sy=y+((fy.floor() as u32).min(h-1));
                if sx>=frame.width() || sy>=frame.height() {return false;}
                frame.pixels().get((sy as usize*frame.width() as usize+sx as usize)*4+3)==Some(&255)
            };
            if !opaque_root && camera_witnesses.len()<skylive_core::MAX_SOURCES*5 && opaque_sample() {
                camera_witnesses.try_reserve(1).map_err(|_|PreviewError::Media(MediaError::BudgetExceeded))?;
                camera_witnesses.push((id,*point));
            }
        }
    }
    let layers: Vec<_> = properties
        .into_iter()
        .map(|(input, transform, texels, opacity, blend, settings)| {
            let layer = if let Some(texels) = texels {
                AffineLayer::from_texels(&inputs[input], transform, texels, opacity)
            } else {
                AffineLayer::new(&inputs[input], transform, UvRect::FULL, opacity)
            };
            item_compositing_layer(layer.with_blend_mode(blend),settings)
        })
        .collect();
    let sampling=skylive_media::affine_sample_requirements(width,height,&layers).map_err(PreviewError::Media)?;
    // The clear was charged on entering render_scene. Each covered destination
    // pixel remains in the existing64Mi base/filter budget. Additional texture
    // taps have a separate, cumulative256Mi budget across all nested buffers.
    const MAX_EXTRA_SAMPLE_VISITS:u64=256*1024*1024;
    let base=sampling.base_pixel_visits.saturating_sub(pixels as u64);
    if base>resources.work_remaining() as u64
        || sampling.extra_sample_visits>MAX_EXTRA_SAMPLE_VISITS.saturating_sub(resources.sampling_extra_visits) {
        return Err(PreviewError::Transition(crate::transition::TransitionError::Limit("nested raster base-work/256Mi sampler-tap budget")));
    }
    resources.pixel_visits+=usize::try_from(base).map_err(|_|PreviewError::InvalidSize)?;
    resources.sampling_extra_visits+=sampling.extra_sample_visits;
    // Affine composition counts unique image buffers; repeated scene references
    // still consume clipped pixel work but do not allocate another image.
    let advanced=layers.iter().any(|layer|layer.opacity!=0.0&&layer.requires_item_compositing());
    let premultiplied = advanced || (!opaque_root
        && layers
            .iter()
            .any(|layer| layer.blend_mode() == SourceBlend::PremultipliedLinearSrgb));
    let limits = CompositeLimits {
        max_layers: skylive_core::MAX_ITEMS,
        max_input_bytes: MAX_RENDER_RESOURCE_BYTES + skylive_core::MAX_ITEMS * 4,
        max_output_bytes: bytes,
        max_pixel_visits:sampling.total_sample_visits,
    };
    let mut image = if premultiplied {
        skylive_media::composite_affine_premultiplied_background(width,height,
            if opaque_root {[0,0,0,255]}else{[0;4]},&layers,limits)
    } else {
        composite_affine(
            width,
            height,
            if opaque_root {
                [0, 0, 0, 255]
            } else {
                [0, 0, 0, 0]
            },
            &layers,
            limits,
        )
    }
    .map_err(PreviewError::Media)?;
    if premultiplied && opaque_root {skylive_media::flatten_premultiplied_on_black(&mut image);}
    // The accepted witness samples an opaque camera contribution and has no
    // contributing later layers. Confirm that it survived quantization at this
    // exact pixel before it is transferred to a parent scene.
    camera_witnesses.retain(|(_,point)| {
        let (x,y)=(point[0].floor() as usize,point[1].floor() as usize);
        x<image.width() as usize && y<image.height() as usize
            && image.pixels().get((y*image.width() as usize+x)*4+3)==Some(&255)
    });
    if !diagnostics.is_empty(){resources.incomplete()?;}
    let retained = resources.scene_diagnostics.entry(view.key()).or_default();
    for diagnostic in diagnostics.drain(..) {
        let key = (diagnostic.source, diagnostic.code);
        if !retained.contains_key(&key) {
            let charge = diagnostic
                .detail
                .len()
                .saturating_add(diagnostic.code.len())
                .saturating_add(std::mem::size_of::<SourceDiagnostic>());
            if retained.len() >= 2 * skylive_core::MAX_SOURCES
                || charge > (8 * 1024 * 1024usize).saturating_sub(resources.diagnostic_bytes)
            {
                return Err(PreviewError::Transition(
                    crate::transition::TransitionError::Limit("nested source diagnostic budget"),
                ));
            }
            resources.diagnostic_bytes += charge;
            retained.insert(key, diagnostic);
        }
    }
    if nested.is_none() {
        diagnostics = resources
            .scene_diagnostics
            .remove(&view.key())
            .unwrap_or_default()
            .into_values()
            .collect();
        diagnostics.sort_by_key(|diagnostic| (diagnostic.source.get(), diagnostic.code));
    }
    Ok(RenderedScene {
        image,
        blend: if premultiplied && !opaque_root {
            SourceBlend::PremultipliedLinearSrgb
        } else {
            SourceBlend::LinearSrgb
        },
        diagnostics,
        gpu_notice: None,
        next_after_ns,
        auto_text:Vec::new(),
        camera_trace,
        camera_witnesses,
        output_witnesses,
    })
}
// A single solid texel is equivalent only for spatially constant stages.
// Spatial masks/model outputs and dimension-changing stages need the original
// authored Color Source extent before any actual filtering or item crop.
fn color_needs_intrinsic_pixels(source:&skylive_core::Source,model:&StudioSnapshot)->bool{
    source.video_filters().iter().any(|filter|filter.enabled&&model.preferences().plugin_enabled(filter.plugin_id())&&match filter.effect{
        skylive_core::VideoEffect::ScaleFilter{target:skylive_core::ScaleFilterTarget::None,..}=>false,
        skylive_core::VideoEffect::GaussianBlur{blurriness,..}=>blurriness!=0.0,
        skylive_core::VideoEffect::Vignette{amount,angle_of_view,..}=>amount!=0.0 && angle_of_view!=0.0,
        // Capture the full authored extent even before a mask is enabled.
        // Toggling mask settings must not resize/rearm the held picture.
        skylive_core::VideoEffect::Freeze{..}=>true,
        skylive_core::VideoEffect::CropPad{..}|skylive_core::VideoEffect::Scroll{..}|skylive_core::VideoEffect::ScaleFilter{..}
            |skylive_core::VideoEffect::ImageMask{..}|skylive_core::VideoEffect::PersonBackgroundRemoval{..}=>true,
        _=>false,
    })
}
fn item_compositing_layer<'a>(layer:AffineLayer<'a>,settings:skylive_core::ItemCompositing)->AffineLayer<'a> {
    use skylive_core::{BlendMode as C,BlendingMethod as M,ScaleFiltering as S};
    use skylive_media::{ItemBlendMode as B,ItemBlendingMethod as BM,SamplingFilter as F};
    let mode=match settings.blend_mode {C::Normal=>B::Normal,C::Additive=>B::Additive,C::Subtract=>B::Subtract,C::Screen=>B::Screen,C::Multiply=>B::Multiply,C::Lighten=>B::Lighten,C::Darken=>B::Darken};
    let method=match settings.blending_method {M::Default=>BM::Default,M::SrgbOff=>BM::SrgbOff};
    let filter=match settings.scale_filter {S::Disabled|S::Point=>F::Nearest,S::Bilinear=>F::Bilinear,S::Bicubic=>F::Bicubic,S::Lanczos=>F::Lanczos,S::Area=>F::Area};
    layer.with_item_compositing(mode,method,filter)
}
fn source_blend(source: &skylive_core::Source) -> SourceBlend {
    if matches!(source.kind(),SourceKind::Image{..})&&source.image_settings().linear_alpha{return SourceBlend::PremultipliedLinearSrgb;}
    match source.kind() {
        SourceKind::Text { .. } | SourceKind::Image { .. } | SourceKind::ImageSlideShow { .. } | SourceKind::Browser {..} => SourceBlend::EncodedPremultipliedSrgb,
        _ => SourceBlend::LinearSrgb,
    }
}
struct Work {
    key: PreviewKey,
    snapshot: Arc<StudioSnapshot>,
    playback: PlaybackSnapshot,
    output: FrameComposition,
    capture: Arc<CaptureSnapshot>,
    filter_preview: bool,
}
type Completion = (PreviewKey, Result<RenderedScene, PreviewError>);
pub struct PreviewWorker {
    sender: Option<mpsc::SyncSender<Work>>,
    receiver: Option<mpsc::Receiver<Completion>>,
    desired: Option<PreviewKey>,
    busy: bool,
    serial: u64,
    handle: Option<thread::JoinHandle<()>>,
    resource_generation: u64,
    temporal_observer:Option<crate::video_filters::TemporalObserver>,
}
impl PreviewWorker {
    pub(crate) fn with_point_backend(point_backend:PointBackend,filter_preview:bool)->std::io::Result<Self> {
        let permit=if filter_preview {Some(filter_draft::Permit::acquire()?)}else{None};
        Self::new_backend_with_filter_permit(point_backend,permit)
    }
    pub fn new() -> std::io::Result<Self> { Self::new_backend(PointBackend::default()) }
    #[cfg(feature = "gpu-filters")]
    pub fn new_with_gpu(client:crate::gpu_color_correction::Client,policy:GpuFailurePolicy)->std::io::Result<Self> {
        Self::new_backend(PointBackend {gpu:Some(crate::video_filters::GpuPointConfig {client:client.into(),policy}),..Default::default()})
    }
    #[cfg(feature = "gpu-filters")]
    pub fn new_with_gpu_runtime(runtime:crate::gpu_color_correction::Runtime,policy:GpuFailurePolicy)->std::io::Result<Self> {
        Self::new_backend(PointBackend {gpu:Some(crate::video_filters::GpuPointConfig {client:runtime.into(),policy}),..Default::default()})
    }
    fn new_backend(point_backend:PointBackend) -> std::io::Result<Self> {
        Self::new_backend_with_filter_permit(point_backend,None)
    }
    fn new_backend_with_filter_permit(point_backend:PointBackend,permit:Option<filter_draft::Permit>)->std::io::Result<Self> {
        let temporal_observer=if permit.is_some(){point_backend.shared.as_ref().map(|owner|owner.temporal_observer(true)).transpose().map_err(|e|std::io::Error::other(e.detail))?}else{None};
        let (sender, jobs) = mpsc::sync_channel::<Work>(1);
        let (results, receiver) = mpsc::sync_channel(1);
        let handle = thread::Builder::new()
            .name("skylive-preview".into())
            .spawn(move || {
                let _filter_preview_permit=permit;
                // Retain an acquisition error as well as a successful guard:
                // evaluation reports poison/identity exhaustion without hiding
                // unrelated non-neural sources behind a startup disconnect.
                let _render_services = retain_render_services().inspect_err(|error| {
                    eprintln!("preview render service {}: {}", error.code(), error);
                });
                let mut cache = ClipCache {point_backend,..Default::default()};
                while let Ok(work) = jobs.recv() {
                    cache.resource_generation(work.key.resource_generation);
                    let value=if work.filter_preview {
                        filter_draft::render(&work.snapshot,&work.output,work.key.width,work.key.height,&work.playback,&mut cache,&work.capture)
                    }else{
render_output_cached_with_capture(
                        &work.snapshot,
                        &work.output,
                        work.key.width,
                        work.key.height,
                        &work.playback,
                        &mut cache,
                        &work.capture,
                    )
                    };
                    if results.send((work.key, value)).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            receiver: Some(receiver),
            desired: None,
            busy: false,
            serial: 0,
            handle: Some(handle),
            resource_generation: 0,
            temporal_observer,
        })
    }
    pub fn is_busy(&self) -> bool {
        self.busy
    }
    /// Invalidate publication without claiming that active work has finished.
    pub fn invalidate(&mut self) {
        self.desired = None;
    }
    /// Explicit resource retry also handles a successful no-op Reload: runtime
    /// recovery must not fabricate an authored revision/history entry.
    pub fn invalidate_resources(&mut self) -> Result<(), PreviewError> {
        let generation = self
            .resource_generation
            .checked_add(1)
            .ok_or(PreviewError::IdentityExhausted)?;
        self.invalidate();
        self.resource_generation = generation;
        Ok(())
    }
    /// Carry the owner's resource identity into a replacement worker.
    pub fn sync_resource_generation(&mut self, generation: u64) {
        if generation > self.resource_generation {
            self.invalidate();
            self.resource_generation = generation;
        }
    }
    pub fn request(
        &mut self,
        session: &Session,
        scene: Id,
        width: u32,
        height: u32,
    ) -> Result<(), PreviewError> {
        self.request_playback(
            session,
            scene,
            width,
            height,
            PlaybackSnapshot::at_time(session, MediaTime(0)),
        )
    }
    pub fn request_playback(
        &mut self,
        session: &Session,
        scene: Id,
        width: u32,
        height: u32,
        playback: PlaybackSnapshot,
    ) -> Result<(), PreviewError> {
        self.request_output(
            session,
            width,
            height,
            playback,
            FrameComposition::Scene(FrameScene::Live(scene)),
        )
    }
    pub fn request_output(
        &mut self,
        session: &Session,
        width: u32,
        height: u32,
        playback: PlaybackSnapshot,
        output: FrameComposition,
    ) -> Result<(), PreviewError> {
        self.request_output_with_capture(
            session,
            width,
            height,
            playback,
            output,
            Arc::new(CaptureSnapshot::empty(session.identity())),
        )
    }
    pub fn request_output_with_capture(
        &mut self,
        session: &Session,
        width: u32,
        height: u32,
        playback: PlaybackSnapshot,
        output: FrameComposition,
        capture: Arc<CaptureSnapshot>,
    ) -> Result<(), PreviewError> {
        self.request_snapshot_with_capture(session,session.snapshot(),width,height,playback,output,capture,false)
    }
    #[allow(clippy::too_many_arguments)]
    fn request_snapshot_with_capture(&mut self,session:&Session,current:Arc<StudioSnapshot>,width:u32,height:u32,
        playback:PlaybackSnapshot,output:FrameComposition,capture:Arc<CaptureSnapshot>,filter_preview:bool)->Result<(),PreviewError> {
        if session.is_closed() || self.sender.is_none() {return Err(PreviewError::Closed);}
        if current.revision()!=session.snapshot().revision() {return Err(PreviewError::StalePlayback);}
        if width == 0 || height == 0 || width > MAX_PREVIEW_WIDTH || height > MAX_PREVIEW_HEIGHT {
            return Err(PreviewError::InvalidSize);
        }
        validate_output(&current, &output, width, height)?;
        for view in output.scenes() {
            view.validate_session(session.identity())
                .map_err(PreviewError::Transition)?;
            view.items(&current).map_err(PreviewError::Transition)?;
        }
        if playback.session() != session.identity()
            || playback.revision() != session.snapshot().revision()
        {
            return Err(PreviewError::StalePlayback);
        }
        if capture.session() != session.identity() {
            return Err(PreviewError::StaleCapture);
        }
        if self.busy {
            return Err(PreviewError::Busy);
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(PreviewError::IdentityExhausted)?;
        let key = PreviewKey {
            session: session.identity(),
            revision: session.snapshot().revision(),
            request: serial,
            scene: output.primary_scene(),
            width,
            height,
            playback_epoch: playback.epoch(),
            time_ns: playback.now().0,
            resource_generation: self.resource_generation,
            capture_epoch: capture.epoch(),
            capture_generation: capture.generation(),
            output: output.identity(),
            composition: output.sample_key(),
        };
        let work = Work {
            key: key.clone(),
            snapshot: current,
            playback,
            output,
            capture,
            filter_preview,
        };
        match self
            .sender
            .as_ref()
            .ok_or(PreviewError::Closed)?
            .try_send(work)
        {
            Ok(()) => {
                self.desired = Some(key);
                self.serial = serial;
                self.busy = true;
                Ok(())
            }
            Err(mpsc::TrySendError::Full(_)) => Err(PreviewError::Busy),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(PreviewError::Disconnected),
        }
    }
    pub fn poll(&mut self, session: &Session) -> Result<Option<PreviewResult>, PreviewError> {
        if session.is_closed() {
            self.invalidate();
            return Err(PreviewError::Closed);
        }
        let receiver = self.receiver.as_ref().ok_or(PreviewError::Closed)?;
        let (key, value) = match receiver.try_recv() {
            Ok(value) => value,
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.busy = false;
                self.desired = None;
                return Err(PreviewError::Disconnected);
            }
        };
        self.busy = false;
        let fresh = self.desired.as_ref() == Some(&key)
            && session.identity() == key.session
            && session.snapshot().revision() == key.revision;
        self.desired = None;
        if fresh {
            let value = value?;
            if value.auto_text.iter().any(|snapshot|!snapshot.is_live()){return Ok(None);}
            Ok(Some(PreviewResult {
                key,
                image: value.image,
                diagnostics: value.diagnostics,
                gpu_notice: value.gpu_notice,
                next_after_ns: value.next_after_ns,
                auto_text:value.auto_text,
            }))
        } else {
            Ok(None)
        }
    }
    pub fn poll_with_capture(
        &mut self,
        session: &Session,
        capture: &CaptureSnapshot,
    ) -> Result<Option<PreviewResult>, PreviewError> {
        if capture.session() != session.identity() {
            return Err(PreviewError::StaleCapture);
        }
        Ok(self.poll(session)?.filter(|result| {
            result.key.capture_epoch == capture.epoch()
                && result.key.capture_generation <= capture.generation()
        }))
    }
    pub fn close(&mut self) {
        self.temporal_observer.take();
        self.invalidate();
        self.sender.take();
        self.receiver.take();
    }
    /// Headless owner only. CPU work has resource bounds; OS file I/O can
    /// still stall, so this join has no finite wall-time guarantee. Never join
    /// from a GUI frame; close invalidates publication while active work counts.
    pub fn join(&mut self) -> thread::Result<()> {
        self.close();
        let result = self.handle.take().map_or(Ok(()), thread::JoinHandle::join);
        self.busy = false;
        result
    }
}
impl Drop for PreviewWorker {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod animation_tests;
#[cfg(test)]
mod capture_tests;
#[cfg(test)]
mod color_transition_tests;
#[cfg(test)]
mod directional_tests;
#[cfg(test)]
mod group_tests;
#[cfg(test)]
mod scene_tests;
#[cfg(test)]
mod transition_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    static IMAGE_TEST_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    struct ImageTemp(PathBuf);
    impl ImageTemp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "skylive-preview-image-{}-{}",
                std::process::id(),
                IMAGE_TEST_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn png(&self, name: &str, rgba: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            let image = image::RgbaImage::from_raw(2, 2, rgba.to_vec()).unwrap();
            image
                .save_with_format(&path, image::ImageFormat::Png)
                .unwrap();
            path
        }
    }
    impl Drop for ImageTemp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn actual_color_and_png_alpha_choose_distinct_obs_sdr_policies_before_black_fade() {
        let temp = ImageTemp::new();
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
                name: "Gray background".into(),
                rgba: [128, 128, 128, 255],
            })
            .unwrap();
        let background = session.snapshot().sources()[0].id();
        let color_item = session
            .apply(Command::AddColorSource {
                scene,
                name: "Half Color".into(),
                rgba: [255, 255, 255, 128],
            })
            .unwrap()
            .created
            .unwrap();
        assert_eq!(
            render(&session.snapshot(), scene, 1, 1).unwrap().pixels(),
            &[205, 205, 205, 255]
        );
        session
            .apply(Command::SetColor {
                source: background,
                rgba: [0, 0, 0, 255],
            })
            .unwrap();
        assert_eq!(
            render(&session.snapshot(), scene, 1, 1).unwrap().pixels(),
            &[188, 188, 188, 255]
        );
        let single = FrameComposition::SingleFade {
            scene: FrameScene::Live(scene),
            logical_scene: scene,
            incoming: false,
            elapsed_ns: 1,
            duration_ns: 2,
            epoch: 1,
        };
        assert_eq!(
            render_output(
                &session.snapshot(),
                &single,
                1,
                1,
                &PlaybackSnapshot::at_time(&session, MediaTime(0))
            )
            .unwrap()
            .pixels(),
            &[137, 137, 137, 255]
        );
        session
            .apply(Command::RemoveItem {
                scene,
                item: color_item,
            })
            .unwrap();
        let path = temp.png("white-alpha.png", &[255, 255, 255, 128].repeat(4));
        let asset = crate::assets::prepare_image(&path).unwrap().asset;
        let item = session
            .apply(Command::AddImageSource {
                scene,
                name: "Half Image".into(),
                asset,
            })
            .unwrap()
            .created
            .unwrap();
        let image = session.snapshot().sources().last().unwrap().id();
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: skylive_core::Transform::full(16, 16),
            })
            .unwrap();
        assert_eq!(
            render(&session.snapshot(), scene, 1, 1).unwrap().pixels(),
            &[128, 128, 128, 255]
        );
        session
            .apply(Command::SetColor {
                source: background,
                rgba: [128, 128, 128, 255],
            })
            .unwrap();
        assert_eq!(
            render(&session.snapshot(), scene, 1, 1).unwrap().pixels(),
            &[154, 154, 154, 255]
        );
        let path = temp.png("gray-alpha.png", &[128, 128, 128, 128].repeat(4));
        let asset = crate::assets::prepare_image(&path).unwrap().asset;
        session
            .apply(Command::SetImageSource {
                source: image,
                asset,
            })
            .unwrap();
        assert_eq!(
            render(&session.snapshot(), scene, 1, 1).unwrap().pixels(),
            &[111, 111, 111, 255]
        );
    }
    #[test]
    fn real_png_pixels_keep_alpha_rotation_and_crop_through_scene_evaluation() {
        let temp = ImageTemp::new();
        let path = temp.png(
            "pattern.png",
            &[
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 128, 255, 255, 255, 255,
            ],
        );
        let asset = crate::assets::prepare_image(&path).unwrap().asset;
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
        let item = session
            .apply(Command::AddImageSource {
                scene,
                name: "Pattern".into(),
                asset,
            })
            .unwrap()
            .created
            .unwrap();
        let transform = skylive_core::Transform {
            x: 4.0,
            y: 4.0,
            width: 8.0,
            height: 8.0,
            ..skylive_core::Transform::full(2, 2)
        };
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform,
            })
            .unwrap();
        let normal = render(&session.snapshot(), scene, 16, 16).unwrap();
        session
            .apply(Command::TransformItem {
                scene,
                item,
                action: skylive_core::TransformAction::Rotate90Clockwise,
            })
            .unwrap();
        let rotated = render(&session.snapshot(), scene, 16, 16).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                let inside = (4..12).contains(&x) && (4..12).contains(&y);
                let region = usize::from(x >= 8) + usize::from(y >= 8) * 2;
                let colors = [
                    [255, 0, 0, 255],
                    [0, 255, 0, 255],
                    [0, 0, 128, 255],
                    [255, 255, 255, 255],
                ];
                let expected = if inside {
                    colors[region]
                } else {
                    [0, 0, 0, 255]
                };
                let expected_rotated = if inside {
                    colors[[2, 0, 3, 1][region]]
                } else {
                    [0, 0, 0, 255]
                };
                let index = (y * 16 + x) * 4;
                assert_eq!(&normal.pixels()[index..index + 4], &expected);
                assert_eq!(&rotated.pixels()[index..index + 4], &expected_rotated);
            }
        }
        let crop = skylive_core::Transform {
            x: 4.0,
            y: 4.0,
            width: 8.0,
            height: 8.0,
            crop: skylive_core::Crop {
                left: 1,
                ..Default::default()
            },
            ..skylive_core::Transform::full(2, 2)
        };
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: crop,
            })
            .unwrap();
        let cropped = render(&session.snapshot(), scene, 16, 16).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                let expected = if (4..8).contains(&x) && (4..12).contains(&y) {
                    if y < 8 { [0, 255, 0, 255] } else { [255; 4] }
                } else {
                    [0, 0, 0, 255]
                };
                let index = (y * 16 + x) * 4;
                assert_eq!(&cropped.pixels()[index..index + 4], &expected);
            }
        }
    }
    #[test]
    fn missing_or_changed_images_report_diagnostics_and_strict_render_refuses_partial_output() {
        let temp = ImageTemp::new();
        let path = temp.png("linked.png", &[255; 16]);
        let asset = crate::assets::prepare_image(&path).unwrap().asset;
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
                name: "Background".into(),
                rgba: [10, 20, 30, 255],
            })
            .unwrap();
        session
            .apply(Command::AddImageSource {
                scene,
                name: "Linked".into(),
                asset,
            })
            .unwrap();
        let source = session.snapshot().sources()[1].id();
        std::fs::remove_file(&path).unwrap();
        let partial = render_with_diagnostics(&session.snapshot(), scene, 16, 16).unwrap();
        assert_eq!(partial.diagnostics.len(), 1);
        assert_eq!(partial.diagnostics[0].source, source);
        assert_eq!(partial.diagnostics[0].code, "image_io");
        assert!(
            partial
                .image
                .pixels()
                .chunks_exact(4)
                .all(|p| p == [10, 20, 30, 255])
        );
        assert!(matches!(
            render(&session.snapshot(), scene, 16, 16),
            Err(PreviewError::ResourcesUnavailable(_))
        ));
        temp.png("linked.png", &[0; 16]);
        let partial = render_with_diagnostics(&session.snapshot(), scene, 16, 16).unwrap();
        assert_eq!(partial.diagnostics[0].code, "image_changed");
    }
    #[test]
    fn missing_image_identities_are_deduplicated_without_spending_decode_admission() {
        let temp = ImageTemp::new();
        let mut session = Session::new();
        let scene = session.preview();
        let same = skylive_core::ImageAsset::new(
            temp.0.join("same.png").to_str().unwrap().into(),
            [1; 32],
            4096,
            4096,
        )
        .unwrap();
        for i in 0..3 {
            session
                .apply(Command::AddImageSource {
                    scene,
                    name: format!("Same {i}"),
                    asset: same.clone(),
                })
                .unwrap();
        }
        for i in 0..2 {
            let asset = skylive_core::ImageAsset::new(
                temp.0
                    .join(format!("other{i}.png"))
                    .to_str()
                    .unwrap()
                    .into(),
                [2; 32],
                4096,
                4096,
            )
            .unwrap();
            session
                .apply(Command::AddImageSource {
                    scene,
                    name: format!("Other {i}"),
                    asset,
                })
                .unwrap();
        }
        let mut cache = ClipCache::default();
        let result = render_cached(
            &session.snapshot(),
            scene,
            1,
            1,
            &PlaybackSnapshot::at_time(&session, MediaTime(0)),
            &mut cache,
        )
        .unwrap();
        assert_eq!(cache.entries.len(), 3);
        assert_eq!(cache.bytes, 0);
        let codes: Vec<_> = result.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            ["image_io", "image_io", "image_io", "image_io", "image_io"]
        );
    }
    #[test]
    fn nonuniform_preview_scale_follows_canvas_rotation_and_tiny_items_stay_tiny() {
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
        let item = session
            .apply(Command::AddColorSource {
                scene,
                name: "Red".into(),
                rgba: [255, 0, 0, 255],
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        session
            .apply(Command::SetColorSize {
                source,
                width: 4,
                height: 2,
            })
            .unwrap();
        let mut transform = skylive_core::Transform {
            x: 8.0,
            y: 8.0,
            width: 8.0,
            height: 2.0,
            rotation_degrees: 90.0,
            flip_x: true,
            crop: skylive_core::Crop {
                left: 1,
                right: 1,
                ..Default::default()
            },
            ..skylive_core::Transform::full(4, 2)
        };
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: transform.clone(),
            })
            .unwrap();
        let frame = render(&session.snapshot(), scene, 8, 4).unwrap();
        for (index, pixel) in frame.pixels().chunks_exact(4).enumerate() {
            assert_eq!(
                pixel,
                if index == 11 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 0, 255]
                }
            );
        }
        transform = skylive_core::Transform {
            width: 0.001,
            height: 0.001,
            ..skylive_core::Transform::full(4, 2)
        };
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform,
            })
            .unwrap();
        let frame = render(&session.snapshot(), scene, 16, 16).unwrap();
        assert!(
            frame
                .pixels()
                .chunks_exact(4)
                .all(|pixel| pixel == [0, 0, 0, 255])
        );
    }
    use skylive_core::Command;
    use std::time::{Duration, Instant};
    fn settle(worker: &mut PreviewWorker, session: &Session) -> Option<PreviewResult> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let value = worker.poll(session).unwrap();
            if value.is_some() || !worker.is_busy() {
                return value;
            }
            assert!(Instant::now() < deadline, "worker did not finish");
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn actual_pixels_visibility_and_transform() {
        let mut s = Session::new();
        let scene = s.preview();
        let item = s
            .apply(Command::AddColorSource {
                scene,
                name: "Red".into(),
                rgba: [255, 0, 0, 255],
            })
            .unwrap()
            .created
            .unwrap();
        assert_eq!(
            &render(&s.snapshot(), scene, 2, 2).unwrap().pixels()[..4],
            &[255, 0, 0, 255]
        );
        s.apply(Command::SetTransform {
            scene,
            item,
            transform: skylive_core::Transform {
                x: 960.0,
                y: 0.0,
                width: 960.0,
                height: 1080.0,
                opacity: 1.0,
                ..skylive_core::Transform::full(1920, 1080)
            },
        })
        .unwrap();
        assert_eq!(
            render(&s.snapshot(), scene, 2, 1).unwrap().pixels(),
            &[0, 0, 0, 255, 255, 0, 0, 255]
        );
        s.apply(Command::SetVisible {
            scene,
            item,
            visible: false,
        })
        .unwrap();
        assert_eq!(
            render(&s.snapshot(), scene, 1, 1).unwrap().pixels(),
            &[0, 0, 0, 255]
        );
    }
    #[test]
    fn stale_busy_fresh_and_close() {
        let mut s = Session::new();
        let mut w = PreviewWorker::new().unwrap();
        w.request(&s, s.preview(), 2, 2).unwrap();
        assert_eq!(w.request(&s, s.preview(), 2, 2), Err(PreviewError::Busy));
        s.apply(Command::AddScene { name: "Two".into() }).unwrap();
        assert!(settle(&mut w, &s).is_none());
        w.request(&s, s.preview(), 2, 2).unwrap();
        assert!(settle(&mut w, &s).is_some());
        w.request(&s, s.preview(), 2, 2).unwrap();
        w.invalidate();
        assert!(w.is_busy());
        assert!(settle(&mut w, &s).is_none());
        w.request(&s, s.preview(), 2, 2).unwrap();
        s.close();
        assert_eq!(w.poll(&s).unwrap_err(), PreviewError::Closed);
        w.close();
        assert!(w.is_busy());
        w.join().unwrap();
        assert!(!w.is_busy());
        assert_eq!(w.request(&s, s.preview(), 2, 2), Err(PreviewError::Closed));
    }
}
