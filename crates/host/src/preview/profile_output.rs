//! Whole Program scaling after full-canvas composition, on existing render workers.
use super::*;
use skylive_media::{FrameResamplePlan,SamplingFilter};
pub(super) struct Plan {stages:[Option<FrameResamplePlan>;2],extra_bytes:usize,base_work:u64,extra_samples:u64}
fn filter(value:skylive_core::ScaleFiltering)->SamplingFilter {use skylive_core::ScaleFiltering as C;match value {C::Disabled|C::Point=>SamplingFilter::Nearest,C::Bilinear=>SamplingFilter::Bilinear,C::Bicubic=>SamplingFilter::Bicubic,C::Lanczos=>SamplingFilter::Lanczos,C::Area=>SamplingFilter::Area}}
impl Plan {
    fn prepare(model:&StudioSnapshot,fitted:Option<(u32,u32)>)->Result<Self,PreviewError>{
        let video=model.output_video().map_err(PreviewError::InvalidCollection)?;
        let canvas=(model.video().width(),model.video().height());let output=(video.width(),video.height());
        let first=if canvas!=output {Some(FrameResamplePlan::stretch(canvas,output,filter(model.output_video_settings().scale_filter())).map_err(PreviewError::Media)?)}else{None};
        let second=match fitted {Some(target) if target!=output=>Some(FrameResamplePlan::fit(output,target,SamplingFilter::Bilinear).map_err(PreviewError::Media)?),_=>None};
        let stages=[first,second];let canvas_bytes=canvas.0 as usize*canvas.1 as usize*4;
        let mut previous=canvas_bytes;let mut peak=canvas_bytes;let mut base_work=0u64;let mut extra_samples=0u64;
        for stage in stages.iter().flatten(){let r=stage.requirements();peak=peak.max(previous.checked_add(r.output_bytes).ok_or(PreviewError::InvalidSize)?);previous=r.output_bytes;
            base_work=base_work.checked_add(r.samples.base_pixel_visits).ok_or(PreviewError::InvalidSize)?;
            extra_samples=extra_samples.checked_add(r.samples.extra_sample_visits).ok_or(PreviewError::InvalidSize)?;
        }
        Ok(Self {stages,extra_bytes:peak-canvas_bytes,base_work,extra_samples})
    }
    pub(super) fn reserve(&self,resources:&mut RenderResources<'_>,output:&FrameComposition,canvas:(u32,u32))->Result<(),PreviewError>{
        // Output-only256Mi work profile covers full4K canvas passes. Preview
        // remains64Mi, each source-filter chain remains independently64Mi.
        resources.base_work_limit=256*1024*1024;
        let pixels=canvas.0 as usize*canvas.1 as usize;
        let (buffers,passes)=match output {
            FrameComposition::Scene(_)|FrameComposition::SceneAlias {..}=>(0,0),
            FrameComposition::Black {..}=>(1,1),
            FrameComposition::Manual {from,to,kind,..}|FrameComposition::Directional {from,to,kind,..}|FrameComposition::Configured {from,to,kind,..}=>{
                let missing=usize::from(from.is_none()||to.is_none());
                if directional_effect(*kind).is_some(){(2+missing,10+missing)}
                else if matches!(kind,TransitionKind::LumaWipe{..}){(1+missing,7)}
                else if matches!(kind,TransitionKind::FadeToColor {..}){(1+missing,5+missing)}else{(1,5)}
            }
            _=>(1,5),
        };
        // Includes each real intermediate/placeholder and final mix; endpoint
        // scene rasters are charged by render_frame before their allocation.
        let bytes=self.extra_bytes.checked_add(pixels.checked_mul(4*buffers).ok_or(PreviewError::InvalidSize)?).ok_or(PreviewError::InvalidSize)?;
        let work=self.base_work.checked_add(pixels as u64*passes as u64).ok_or(PreviewError::InvalidSize)?;
        let used=resources.cache.bytes.saturating_add(resources.cache.text.bytes).saturating_add(resources.live_bytes);
        if bytes>MAX_RENDER_RESOURCE_BYTES.saturating_sub(used) || work>resources.work_remaining() as u64
            || self.extra_samples>(256*1024*1024u64).saturating_sub(resources.sampling_extra_visits) {return Err(PreviewError::Media(MediaError::BudgetExceeded));}
        resources.live_bytes+=bytes;resources.pixel_visits+=work as usize;resources.sampling_extra_visits+=self.extra_samples;Ok(())
    }
    pub(super) fn apply(&self,mut frame:RenderedScene)->Result<RenderedScene,PreviewError>{
        // Preserve proof only when this output centre reconstructs exactly
        // one previously proven texel; mixed kernels remain conservative.
        let scaled=self.stages.iter().any(Option::is_some);
        for stage in self.stages.iter().flatten(){
            frame.output_witnesses.retain_mut(|(_,point)|match stage.exact_witness(*point){Some(next)=>{*point=next;true},None=>false});
            let r=stage.requirements();let image=stage.apply(&frame.image,frame.blend,CompositeLimits {max_layers:1,max_input_bytes:r.input_bytes,max_output_bytes:r.output_bytes,max_pixel_visits:r.samples.total_sample_visits}).map_err(PreviewError::Media)?;
            frame.image=image;frame.blend=SourceBlend::LinearSrgb;
        }
        if scaled {frame.camera_trace.retain(|id|frame.output_witnesses.iter().any(|(source,_)|source==id));}
        Ok(frame)
    }
}
impl OutputRenderer {
    pub(crate) fn validate_profile_output_inputs(model:&StudioSnapshot,output:&FrameComposition,playback:&PlaybackSnapshot,capture:&CaptureSnapshot)->Result<(),PreviewError>{
        validate_render_inputs(model,output,model.video().width(),model.video().height(),playback,capture,true)?;
        Plan::prepare(model,None)?;Ok(())
    }
    pub fn render_profile_output(&mut self,model:&StudioSnapshot,output:&FrameComposition,playback:&PlaybackSnapshot,capture:&CaptureSnapshot)->Result<RenderedScene,PreviewError>{
        self.profile_output(model,output,playback,capture,None,false)
    }
    /// Recording-only renderer. Its opt-in NDI path is valid only while the
    /// recording owns a sink-specific delayed PCM reader.
    pub(crate) fn render_recording_profile_output(&mut self,model:&StudioSnapshot,output:&FrameComposition,playback:&PlaybackSnapshot,capture:&CaptureSnapshot)->Result<RenderedScene,PreviewError>{
        self.profile_output(model,output,playback,capture,None,true)
    }
    /// Streaming's separate AVC-proof/PCM reader owns this opt-in render.
    pub(crate) fn render_streaming_profile_output(&mut self,model:&StudioSnapshot,output:&FrameComposition,playback:&PlaybackSnapshot,capture:&CaptureSnapshot)->Result<RenderedScene,PreviewError>{
        self.profile_output(model,output,playback,capture,None,true)
    }
    /// OBS view routes use the global canvas/output scaler, then the explicitly
    /// independent virtual-camera format receives a centered aspect fit.
    pub fn render_profile_output_fitted(&mut self,model:&StudioSnapshot,output:&FrameComposition,width:u32,height:u32,playback:&PlaybackSnapshot,capture:&CaptureSnapshot)->Result<RenderedScene,PreviewError>{
        if !admitted_size(width,height,true){return Err(PreviewError::InvalidSize);}
        self.profile_output(model,output,playback,capture,Some((width,height)),false)
    }
    fn profile_output(&mut self,model:&StudioSnapshot,output:&FrameComposition,playback:&PlaybackSnapshot,capture:&CaptureSnapshot,fitted:Option<(u32,u32)>,recording_ndi:bool)->Result<RenderedScene,PreviewError>{
        let plan=Plan::prepare(model,fitted)?;
        let result=render_output_policy_profile(model,output,model.video().width(),model.video().height(),playback,&mut self.cache,capture,true,Some(&plan),recording_ndi)?;
        if result.diagnostics.iter().any(|issue|!issue.kind.recoverable()){return Err(PreviewError::ResourcesUnavailable(result.diagnostics));}
        Ok(result)
    }
}
