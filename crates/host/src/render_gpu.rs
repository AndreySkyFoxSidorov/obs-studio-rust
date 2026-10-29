//! One composition-root graphics capability shared by every render owner.
//! Cloning retains the same lazy native Owner; no device work occurs here.
pub use crate::video_filters::SharedFilterStats;
#[derive(Clone, Default)]
pub struct RenderGpu {
    source_filters:Option<crate::video_filters::SharedSourceFilters>,
    #[cfg(feature = "gpu-filters")]
    runtime:Option<crate::gpu_color_correction::Runtime>,
}
impl RenderGpu {
    /// Synchronize actual authored and explicitly owned draft/private timer
    /// targets. Call after accepted edits and before submitting their renders.
    pub fn freeze_observe(&self, group:crate::freeze_timing_book::Group, definitions:&[crate::freeze_timing_book::Definition], now:u64)->Result<(),String>{
        self.source_filters.as_ref().ok_or("source filter owner unavailable")?.freeze_observe(group,definitions,now).map_err(|e|e.detail)
    }
    pub fn freeze_due(&self, now:u64)->Result<crate::freeze_timing_book::IntentBatch,String>{
        self.source_filters.as_ref().ok_or("source filter owner unavailable")?.freeze_due(now).map_err(|e|e.detail)
    }
    pub fn freeze_deadline(&self)->Result<Option<u64>,String>{
        self.source_filters.as_ref().ok_or("source filter owner unavailable")?.freeze_deadline().map_err(|e|e.detail)
    }
    pub fn freeze_generation(&self)->Option<u64>{self.source_filters.as_ref().and_then(|owner|owner.freeze_generation())}
    pub fn freeze_failures(&self)->Option<u64>{self.source_filters.as_ref().and_then(|owner|owner.freeze_failures())}
    pub(crate) fn freeze_credit(&self,bytes:usize)->Result<std::sync::Arc<dyn Send+Sync>,String>{self.source_filters.as_ref().ok_or("source filter owner unavailable")?.freeze_credit(bytes).map_err(|e|e.detail)}
    pub fn freeze_accept_disable(&self,intent:crate::freeze_timing_book::Intent)->bool{self.source_filters.as_ref().is_some_and(|owner|owner.freeze_accept_disable(intent))}
    pub fn freeze_reject_disable(&self,intent:crate::freeze_timing_book::Intent){if let Some(owner)=&self.source_filters{owner.freeze_reject_disable(intent);}}
    pub fn freeze_pending(&self,intent:crate::freeze_timing_book::Intent)->bool{self.source_filters.as_ref().is_some_and(|owner|owner.freeze_pending(intent))}
    pub fn freeze_expire_private(&self,intent:crate::freeze_timing_book::Intent,now:u64)->bool{self.source_filters.as_ref().is_some_and(|owner|owner.freeze_expire_private(intent,now))}
    pub fn freeze_effective(&self,target:crate::freeze_timing_book::Target)->Option<bool>{self.source_filters.as_ref().and_then(|owner|owner.freeze_effective(target))}
    /// One lazy filter service per actual desktop/CLI owner. Construction does
    /// not spawn workers, initialize models or create native graphics objects.
    pub fn new()->Self {
        #[cfg(all(feature="gpu-filters",any(target_os="windows",target_os="macos")))]
        let runtime=if std::env::var_os("SKYLIVE_GPU_FILTERS").as_deref()==Some(std::ffi::OsStr::new("cpu")) {None}
            else {Some(crate::gpu_color_correction::Runtime::new(Default::default()))};
        Self {
            source_filters:Some(crate::video_filters::SharedSourceFilters::new()),
            #[cfg(feature="gpu-filters")]
            runtime:{
                #[cfg(any(target_os="windows",target_os="macos"))] {runtime}
                #[cfg(not(any(target_os="windows",target_os="macos")))] {None}
            },
        }
    }
    /// Explicit deterministic CPU capability for callers which require it.
    pub fn cpu()->Self {Self {source_filters:Some(crate::video_filters::SharedSourceFilters::new()),..Default::default()}}
    /// Bind one metadata clock to this same lazy CPU/GPU source-filter owner.
    pub fn playback_clock(&self)->std::io::Result<crate::playback::PlaybackClock>{let observer=self.source_filters.as_ref().map(|owner|owner.temporal_observer(false)).transpose().map_err(|error|std::io::Error::other(error.detail))?;Ok(crate::playback::PlaybackClock::with_render_filters(observer,self.source_filters.as_ref().map(|owner|owner.mask_versions())))}
    pub(crate) fn draft_mask_files(&self,model:&skylive_core::StudioSnapshot,views:&[crate::transition::FrameScene],domain:u64)->Result<Option<std::sync::Arc<crate::mask_live_versions::Sample>>,&'static str>{
        if domain==0{return Ok(None);}
        let Some(owner)=&self.source_filters else{return Ok(None);};let book=owner.mask_versions();
        let mut versions=book.lock().map_err(|_|"mask revision owner poisoned")?;
        crate::mask_live_versions::sample_scene(&mut versions,domain,model,views)
    }
    #[cfg(feature = "gpu-filters")]
    pub fn report(&self)->Option<crate::gpu_color_correction::Report> {
        self.runtime.as_ref().and_then(crate::gpu_color_correction::Runtime::report)
    }
    #[cfg(feature = "gpu-filters")]
    pub fn completed_gpu_jobs(&self)->u64 {
        self.runtime.as_ref().map_or(0,crate::gpu_color_correction::Runtime::completed_gpu_jobs)
    }
    pub fn filter_cache_stats(&self)->SharedFilterStats {
        self.source_filters.as_ref().map_or_else(Default::default,|cache|cache.stats())
    }
    fn point_backend(&self)->crate::preview::PointBackend {
        crate::preview::PointBackend {
            shared:self.source_filters.clone(),
            #[cfg(feature="gpu-filters")]
            gpu:self.runtime.as_ref().map(|runtime|crate::video_filters::GpuPointConfig {
                client:runtime.clone().into(),policy:crate::preview::GpuFailurePolicy::PreferGpu,
            }),
        }
    }
    pub fn output_renderer(&self)->crate::preview::OutputRenderer {
        crate::preview::OutputRenderer::with_point_backend(self.point_backend())
    }
    pub fn filter_preview_worker(&self)->std::io::Result<crate::preview::PreviewWorker> {
        crate::preview::PreviewWorker::with_point_backend(self.point_backend(),true)
    }
    pub fn preview_worker(&self)->std::io::Result<crate::preview::PreviewWorker> {
        crate::preview::PreviewWorker::with_point_backend(self.point_backend(),false)
    }
}
