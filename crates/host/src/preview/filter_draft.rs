//! Typed immutable filter-preview projection and bounded modal-worker credit.
use super::*;
use skylive_core::{VideoFilter,VideoFilterPreviewTarget};
use std::sync::atomic::{AtomicBool,Ordering};
static ACTIVE:AtomicBool=AtomicBool::new(false);
pub(super) struct Permit;
impl Permit {
    pub(super) fn acquire()->std::io::Result<Self> {
        ACTIVE.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire)
            .map_err(|_|std::io::Error::new(std::io::ErrorKind::WouldBlock,"previous filter preview is still retiring"))?;
        Ok(Self)
    }
}
impl Drop for Permit {fn drop(&mut self){ACTIVE.store(false,Ordering::Release);}}
pub struct FilterPreview {
    session:u64,
    snapshot:Arc<StudioSnapshot>,
    output:FrameComposition,
    dimensions:(u32,u32),
    draft_rows:Option<crate::video_filters::DraftRows>,
    freeze_owner:Option<skylive_core::Id>,
    mask_rows:Option<(u64,Vec<u32>)>,
    mask_owner:skylive_core::Id,
    camera_draft:Option<skylive_core::Id>,
}
impl FilterPreview {
    pub fn prepare(session:u64,current:&StudioSnapshot,target:VideoFilterPreviewTarget,filters:Vec<VideoFilter>,
        capture:&CaptureSnapshot)->Result<Self,PreviewError> {
        if filters.iter().any(|filter|filter.id().is_none()&&matches!(filter.effect,skylive_core::VideoEffect::Freeze{..})){return Err(PreviewError::InvalidCollection(skylive_core::Error::Unavailable("Freeze filter preview requires stable draft row identities")));}
        if crate::video_filters::has_animated_mask(&filters,None){return Err(PreviewError::InvalidCollection(skylive_core::Error::Unavailable("Animated mask preview requires stable draft row identities")));}
        Self::prepare_bound(session,current,target,filters,capture,None)
    }
    /// Bind the actual editor's stable rows before deriving temporary Core IDs.
    pub fn prepare_with_rows(session:u64,current:&StudioSnapshot,target:VideoFilterPreviewTarget,filters:Vec<VideoFilter>,
        capture:&CaptureSnapshot,window:u64,rows:&[u32])->Result<Self,PreviewError>{
        if window==0||rows.len()!=filters.len()||rows.len()>skylive_core::MAX_VIDEO_FILTERS||rows.iter().enumerate().any(|(index,row)|*row==0||rows[..index].contains(row)){return Err(PreviewError::InvalidCollection(skylive_core::Error::Invalid("Invalid filter preview draft row ownership")));}
        Self::prepare_bound(session,current,target,filters,capture,Some((window,rows)))
    }
    fn prepare_bound(session:u64,current:&StudioSnapshot,target:VideoFilterPreviewTarget,mut filters:Vec<VideoFilter>,
        capture:&CaptureSnapshot,rows:Option<(u64,&[u32])>)->Result<Self,PreviewError> {
        if capture.session()!=session {return Err(PreviewError::StaleCapture);}
        // Draft changes cannot reconfigure the native NDI sample queue. Show
        // the most recently delivered sample with the remaining draft effects;
        // the editor explicitly labels this approximation until Apply commits
        // the new delay to the source owner. Never rewrite canonical settings.
        if matches!(target,VideoFilterPreviewTarget::Source(id)
            if current.source(id).is_some_and(|source|matches!(source.kind(),SourceKind::NdiSource {..}))) {
            for filter in &mut filters {
                if let skylive_core::VideoEffect::AsyncVideoDelay {delay_ms}=&mut filter.effect {*delay_ms=0;}
            }
        }
        // A modal draft cannot reconfigure the Program delay history. Use a
        // separate raw camera sample and bypass queue time in this preview;
        // Program, its resource snapshots and its receipt are untouched.
        let mut camera_draft=None;
        if let VideoFilterPreviewTarget::Source(id)=target {
            let camera=current.source(id).is_some_and(|source|matches!(source.kind(),SourceKind::VideoCapture{..}));
            let ndi=current.source(id).is_some_and(|source|matches!(source.kind(),SourceKind::NdiSource {..}));
            let draft_last=filters.iter().rposition(|filter|filter.enabled&&matches!(filter.effect,skylive_core::VideoEffect::AsyncVideoDelay{delay_ms:1..=u16::MAX}));
            let camera_delayed=camera && capture.frame(id).is_some_and(|frame|frame.async_delay.is_some());
            // During delay warmup Program has no frame yet, but the separate
            // modal-only camera sample may already be available. This also
            // supports disabling a delay in the draft before its first output.
            let camera_warming=camera && capture.frame(id).is_none() && capture.with_camera_draft(id).is_some();
            // NDI drafts bypass the native delay queue as well. Use the
            // separately admitted raw sample so already-staged effects are
            // never applied twice to the published delayed image.
            let ndi_delayed=ndi && capture.frame(id).is_some_and(|frame|frame.async_delay.is_some());
            let ndi_warming=ndi && capture.frame(id).is_none() && capture.with_camera_draft(id).is_some();
            if ndi_delayed || ndi_warming {camera_draft=Some(id);}
            // Even an otherwise point-only draft must bypass the live delay:
            // the draft cannot own or reconfigure Program's delay receipt.
            // This also covers removing/disabling an already applied delay,
            // where showing the old queued frame would misrepresent the draft.
            if camera && (draft_last.is_some()||camera_delayed||camera_warming) || ndi && (draft_last.is_some()||ndi_delayed||ndi_warming) {
                if camera || ndi_delayed || ndi_warming {camera_draft=Some(id);}
                for filter in &mut filters {
                    if let skylive_core::VideoEffect::AsyncVideoDelay {delay_ms}=&mut filter.effect {*delay_ms=0;}
                }
            }
        }
        // Geometry must describe the same raw sample the modal renderer will
        // consume. The published delayed frame can already have passed Scale,
        // so using its dimensions here scales the draft a second time.
        let draft_capture=camera_draft.map(|source|capture.with_camera_draft(source).ok_or(PreviewError::StaleCapture)).transpose()?;
        let capture=draft_capture.as_ref().unwrap_or(capture);
        let raw=Self::source_dimensions(current,target,capture)?;
        let snapshot=Arc::new(current.preview_video_filters(target,filters).map_err(PreviewError::InvalidCollection)?);
        let mask_owner=match target {VideoFilterPreviewTarget::Source(id)|VideoFilterPreviewTarget::Scene(id)=>id};
        let owner=match target {VideoFilterPreviewTarget::Source(id)=>snapshot.source(id),VideoFilterPreviewTarget::Scene(id)=>snapshot.scene_source(id)};
        let draft_rows=match (owner,rows){(Some(source),Some((window,rows)))=>Some(crate::video_filters::DraftRows::bind(source,window,rows).map_err(|error|PreviewError::TemporalClock(error.detail))?),(None,Some((_,rows))) if !rows.is_empty()=>return Err(PreviewError::InvalidCollection(skylive_core::Error::Invalid("Invalid filter preview draft row ownership"))),_=>None};
        let freeze_owner=owner.map(|source|source.id());
        let canvas=(snapshot.video().width(),snapshot.video().height());
        let dimensions=owner.map_or(Ok(raw),|s|snapshot.effective_filtered_video_dimensions(s,raw,Some(canvas))).map_err(PreviewError::InvalidCollection)?;
        let view=FrameScene::preview_video_filters(session,&snapshot,target,dimensions).map_err(PreviewError::Transition)?;
        Ok(Self {session,snapshot,output:FrameComposition::Scene(view),dimensions,draft_rows,freeze_owner,mask_owner,mask_rows:rows.map(|(window,rows)|(window,rows.to_vec())),camera_draft})
    }
    pub fn source_dimensions(current:&StudioSnapshot,target:VideoFilterPreviewTarget,capture:&CaptureSnapshot)->Result<(u32,u32),PreviewError> {
        match target {
            VideoFilterPreviewTarget::Scene(scene)=>{
                current.scene(scene).ok_or(PreviewError::NotFound(scene))?;
                Ok((current.video().width(),current.video().height()))
            }
            VideoFilterPreviewTarget::Source(source)=>{
                let value=current.source(source).ok_or(PreviewError::NotFound(source))?;
                if let Some(frame)=capture.frame(source).filter(|frame|frame.binding.matches_source(value.kind())) {
                    Ok((frame.image.width(),frame.image.height()))
                }else{
                    current.source_video_dimensions(value.kind()).map_err(PreviewError::InvalidCollection)?
                        .ok_or(PreviewError::NotFound(source))
                }
            }
        }
    }
    /// Dimensions used to identify a modal draft. A delayed camera or NDI
    /// image may have already been scaled before entering its delay queue;
    /// the draft instead renders the accompanying raw sample. Track that
    /// sample's geometry so a device mode change invalidates the projection
    /// even when the delayed output happens to retain the same dimensions.
    pub fn draft_source_dimensions(current:&StudioSnapshot,target:VideoFilterPreviewTarget,capture:&CaptureSnapshot)->Result<(u32,u32),PreviewError> {
        if let VideoFilterPreviewTarget::Source(id)=target {
            let delayed_native=current.source(id).is_some_and(|source|matches!(source.kind(),SourceKind::VideoCapture{..}|SourceKind::NdiSource{..}))
                && capture.frame(id).is_some_and(|frame|frame.async_delay.is_some());
            let warming_camera=current.source(id).is_some_and(|source|matches!(source.kind(),SourceKind::VideoCapture{..}|SourceKind::NdiSource{..}))
                && capture.frame(id).is_none() && capture.with_camera_draft(id).is_some();
            if delayed_native || warming_camera {
                let raw=capture.with_camera_draft(id).ok_or(PreviewError::StaleCapture)?;
                return Self::source_dimensions(current,target,&raw);
            }
        }
        Self::source_dimensions(current,target,capture)
    }
    pub fn scroll_playback(&self,playback:&PlaybackSnapshot,capture:&CaptureSnapshot,clock:&mut crate::scroll_animation::Clock)->Result<PlaybackSnapshot,PreviewError>{
        if self.mask_rows.is_some()&&self.freeze_owner.and_then(|id|self.snapshot.source(id)).is_some_and(|source|crate::video_filters::has_animated_mask(source.video_filters(),None)){return Err(PreviewError::TemporalClock("Animated draft mask requires draft_playback".into()));}
        if playback.session()!=self.session||playback.revision()!=self.snapshot.revision(){return Err(PreviewError::StalePlayback);}
        let draft_capture=self.camera_draft.map(|source|capture.with_camera_draft(source).ok_or(PreviewError::StaleCapture)).transpose()?;
        let capture=draft_capture.as_ref().unwrap_or(capture);
        let sample=clock.sample(self.session,&self.snapshot,&self.output.scenes(),playback.now(),Some(capture)).map_err(|error|PreviewError::ScrollClock(error))?;
        Ok(playback.clone().with_scroll_sample(sample))
    }
    pub fn draft_playback(&self,playback:&PlaybackSnapshot,capture:&CaptureSnapshot,render_gpu:&crate::render_gpu::RenderGpu,clock:&mut crate::scroll_animation::Clock,mask_clock:&mut crate::mask_animation_clock::DraftClock)->Result<PlaybackSnapshot,PreviewError>{
        if playback.session()!=self.session||playback.revision()!=self.snapshot.revision(){return Err(PreviewError::StalePlayback);}
        let draft_capture=self.camera_draft.map(|source|capture.with_camera_draft(source).ok_or(PreviewError::StaleCapture)).transpose()?;
        let capture=draft_capture.as_ref().unwrap_or(capture);
        let sample=clock.sample(self.session,&self.snapshot,&self.output.scenes(),playback.now(),Some(capture)).map_err(|error|PreviewError::ScrollClock(error))?;
        let mut projected=playback.clone().with_scroll_sample(sample);
        if let (Some(source),Some((window,rows)))=(self.freeze_owner.and_then(|id|self.snapshot.source(id)),self.mask_rows.as_ref()){
            let mask=mask_clock.sample(&projected.mask_animation,source,self.mask_owner,*window,rows,playback.now()).map_err(|error|PreviewError::TemporalClock(error.into()))?;
            let files=render_gpu.draft_mask_files(&self.snapshot,&self.output.scenes(),mask.files_domain).map_err(|error|PreviewError::TemporalClock(error.into()))?;
            let domain=mask.files_domain;
            mask_clock.hold_files(files.clone());
            projected=projected.with_mask_sample(mask.with_files(files,domain));
        }else{mask_clock.hold_files(None);}
        Ok(projected)
    }
    pub fn dimensions(&self)->(u32,u32){self.dimensions}
    pub fn modal_raw_source(&self)->Option<skylive_core::Id>{self.camera_draft}
    pub fn freeze_owner(&self)->Option<skylive_core::Id>{self.freeze_owner}
    pub fn observe_freeze(&self,gpu:&crate::render_gpu::RenderGpu,window:u64,rows:&[u32],domain:u64,now:u64)->Result<(),String>{
        use crate::freeze_timing_book::{Definition,Group,Instance,Target};
        let owner=self.freeze_owner.and_then(|id|self.snapshot.source(id));
        let Some(source)=owner else{return gpu.freeze_observe(Group::Draft,&[],now);};
        if rows.len()!=source.video_filters().len(){return Err("Invalid filter preview draft row ownership".into());}
        let view=self.output.scenes().into_iter().next().ok_or("Filter preview has no view")?;
        let crate::transition::SceneKey::Frozen{instance:generation,..}=view.key()else{return Err("Filter preview has no instance identity".into());};
        let mut definitions=crate::freeze_timing_collect::Definitions::new(gpu);
        for(filter,row)in source.video_filters().iter().zip(rows){if let skylive_core::VideoEffect::Freeze{settings}=filter.effect{
            definitions.push(Definition{target:Target{session:self.session,source:source.id(),instance:Instance::Draft{window,row:*row}},generation,enabled:self.snapshot.preferences().plugin_enabled(skylive_core::EXELDRO_FREEZE_PLUGIN)&&filter.enabled,duration_ms:settings.duration_ms(),refresh_ms:settings.refresh_interval_ms()})?;
        }}
        let enabled=self.snapshot.preferences().plugin_enabled(skylive_core::EXELDRO_FREEZE_PLUGIN);
        let mut append=|nested:&skylive_core::Source|->Result<(),String>{
            if nested.id()==source.id(){return Ok(());}
            for filter in nested.video_filters(){if let skylive_core::VideoEffect::Freeze{settings}=filter.effect{
                let filter=filter.id().ok_or("Freeze target has no stable filter identity")?;
                let target=Target{session:self.session,source:nested.id(),instance:Instance::DraftPrivate{window,owner:domain,filter}};
                let enabled_filter=nested.video_filters().iter().find(|value|value.id()==Some(filter)).is_some_and(|value|value.enabled);
                definitions.push(Definition{target,generation,enabled:enabled&&enabled_filter,duration_ms:settings.duration_ms(),refresh_ms:settings.refresh_interval_ms()})?;
            }}Ok(())
        };
        for nested in view.visible_sources(&self.snapshot).map_err(|error|error.to_string())?{if self.snapshot.source(nested.id()).is_some(){append(nested)?;}}
        if let Some(root)=view.root_scene_source(&self.snapshot).filter(|root|self.snapshot.source(root.id()).is_some()){append(root)?;}
        gpu.freeze_observe(Group::Draft,definitions.values(),now)
    }
    pub fn output_key(&self)->OutputKey {self.output.identity()}
    pub fn sample_key(&self)->CompositionKey {self.output.sample_key()}
    pub fn render_dimensions(&self)->(u32,u32){
        let (w,h)=self.dimensions;
        // Modal thumbnails retain their independent work profile even when the
        // main preview grows to its actual display size.
        let scale=(640.0/f64::from(w)).min(360.0/f64::from(h)).min(1.0);
        ((f64::from(w)*scale).round().max(1.0) as u32,(f64::from(h)*scale).round().max(1.0) as u32)
    }
    /// Full-size source/scene export, without the modal checker or GUI draft.
    pub fn render_image(&self,renderer:&mut OutputRenderer,playback:&PlaybackSnapshot,capture:&CaptureSnapshot)->Result<RgbaFrame,PreviewError>{
        let draft_capture=self.camera_draft.map(|source|capture.with_camera_draft(source).ok_or(PreviewError::StaleCapture)).transpose()?;
        let capture=draft_capture.as_ref().unwrap_or(capture);
        let (width,height)=self.dimensions;
        validate_render_inputs(&self.snapshot,&self.output,width,height,playback,capture,true)?;
        renderer.cache.scopes(self.session,self.snapshot.revision(),self.output.scenes().iter().map(FrameScene::resource_key).collect());
        let mut pinned=image_cache::Pins::default();
        pinned.persistent(&self.snapshot,&renderer.cache).map_err(PreviewError::Media)?;
        for view in self.output.scenes(){for source in view.visible_sources(&self.snapshot).map_err(PreviewError::Transition)?{image_slideshow::pin(source,playback,&mut pinned).map_err(PreviewError::Media)?;}}
        image_slideshow::retain(&mut renderer.cache,&pinned);
        renderer.cache.preload_hidden(&pinned);
        let mut resources=RenderResources::new(&mut renderer.cache,capture);resources.full_resolution=true;
        let view=self.output.scenes().into_iter().next().ok_or(PreviewError::InvalidSize)?;
        let result=render_frame_background(&self.snapshot,&view,width,height,playback,&mut resources,false)?;
        if !result.diagnostics.is_empty(){return Err(PreviewError::ResourcesUnavailable(result.diagnostics));}
        Ok(skylive_media::straighten_rgba(result.image,result.blend))
    }
}
impl PreviewWorker {
    pub fn freeze_domain(&self)->Option<u64>{self.temporal_observer.as_ref().map(|observer|observer.domain())}
    pub fn new_filter_preview()->std::io::Result<Self> {
        Self::new_backend_with_filter_permit(PointBackend::default(),Some(Permit::acquire()?))
    }
    #[cfg(feature="gpu-filters")]
    pub fn new_filter_preview_with_gpu(runtime:crate::gpu_color_correction::Runtime)->std::io::Result<Self> {
        Self::new_backend_with_filter_permit(PointBackend {gpu:Some(crate::video_filters::GpuPointConfig {client:runtime.into(),policy:GpuFailurePolicy::PreferGpu}),..Default::default()},Some(Permit::acquire()?))
    }
    pub fn request_filter_preview(&mut self,session:&Session,preview:&FilterPreview,playback:PlaybackSnapshot,capture:Arc<CaptureSnapshot>)->Result<(),PreviewError> {
        if session.identity()!=preview.session || session.snapshot().revision()!=preview.snapshot.revision() {return Err(PreviewError::StalePlayback);}
        let capture=if let Some(source)=preview.camera_draft {
            Arc::new(capture.with_camera_draft(source).ok_or(PreviewError::StaleCapture)?)
        }else{capture};
        let mut playback=playback;
        if let Some(observer)=&self.temporal_observer{playback.temporal=observer.sample_draft(session.identity(),&preview.snapshot,&preview.output.scenes(),playback.now(),Some(&capture),preview.draft_rows.as_ref()).map_err(|error|{observer.reset();PreviewError::TemporalClock(error.detail)})?;}
        let (width,height)=preview.render_dimensions();
        self.request_snapshot_with_capture(session,preview.snapshot.clone(),width,height,playback,preview.output.clone(),capture,true)
    }
}
pub(super) fn render(collection:&StudioSnapshot,output:&FrameComposition,width:u32,height:u32,playback:&PlaybackSnapshot,
    cache:&mut ClipCache,capture:&CaptureSnapshot)->Result<RenderedScene,PreviewError> {
    validate_render_inputs(collection,output,width,height,playback,capture,false)?;
    cache.scopes(playback.session(),collection.revision(),output.scenes().iter().map(FrameScene::resource_key).collect());
    let mut pinned=image_cache::Pins::default();
    pinned.persistent(collection,cache).map_err(PreviewError::Media)?;
    for view in output.scenes(){for source in view.visible_sources(collection).map_err(PreviewError::Transition)? {image_slideshow::pin(source,playback,&mut pinned).map_err(PreviewError::Media)?;}}
    image_slideshow::retain(cache,&pinned);
    cache.preload_hidden(&pinned);
    let mut resources=RenderResources::new(cache,capture);
    let view=output.scenes().into_iter().next().ok_or(PreviewError::InvalidSize)?;
    let mut raw=render_frame_background(collection,&view,width,height,playback,&mut resources,false)?;
    // Keep a successful CPU result usable. GPU preflight/dispatch failures are
    // informational here, not source diagnostics that invalidate pixels.
    #[cfg(feature="gpu-filters")]
    { raw.gpu_notice=resources.effects.gpu_last_error.as_ref().map(ToString::to_string); }
    let bytes=(width as usize).checked_mul(height as usize).and_then(|v|v.checked_mul(4)).ok_or(PreviewError::InvalidSize)?;
    let extra=bytes.checked_mul(2).ok_or(PreviewError::InvalidSize)?;
    // One pass builds the checker; composition admits its clear plus two layers.
    let composite_visits=u64::from(width)*u64::from(height)*3;
    let visits=u64::from(width)*u64::from(height)*4;
    let used=resources.cache.bytes.saturating_add(resources.cache.text.bytes).saturating_add(resources.live_bytes);
    if extra>MAX_RENDER_RESOURCE_BYTES.saturating_sub(used) || visits>(64*1024*1024usize).saturating_sub(resources.pixel_visits) as u64 {
        return Err(PreviewError::Media(MediaError::BudgetExceeded));
    }
    let mut pixels=Vec::new();pixels.try_reserve_exact(bytes).map_err(|_|PreviewError::Media(MediaError::BudgetExceeded))?;
    for y in 0..height {for x in 0..width {let value=if (x/12+y/12)%2==0 {72}else{104};pixels.extend_from_slice(&[value,value,value,255]);}}
    let checker=RgbaFrame::new(width,height,pixels).map_err(PreviewError::Media)?;
    let transform=Affine2 {xx:f64::from(width),xy:0.0,yx:0.0,yy:f64::from(height),tx:0.0,ty:0.0};
    let layers=[AffineLayer::new(&checker,transform,UvRect::FULL,1.0).with_blend_mode(SourceBlend::LinearSrgb),
        AffineLayer::new(&raw.image,transform,UvRect::FULL,1.0).with_blend_mode(raw.blend)];
    raw.image=composite_affine(width,height,[0,0,0,255],&layers,CompositeLimits {max_layers:2,max_input_bytes:bytes.saturating_mul(2),max_output_bytes:bytes,max_pixel_visits:composite_visits})
        .map_err(PreviewError::Media)?;
    raw.blend=SourceBlend::LinearSrgb;
    Ok(raw)
}
