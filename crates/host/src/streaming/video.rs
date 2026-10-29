use super::*;
pub(crate) trait Renderer {
    fn render(&mut self, config: &Config, frame: &ProgramVideoFrame) -> Result<Prepared, Error>;
    /// Proven pixel contributors to the last render. Empty for uninstrumented
    /// renderers and for renders with diagnostics.
    fn take_delay_trace(&mut self)->Vec<skylive_core::Id> {Vec::new()}
}
pub(crate) struct Prepared {
    pub nv12: Box<[u8]>,
    pub warnings: Option<Arc<crate::recording::RecordingWarnings>>,
    auto_text:Vec<crate::text::auto_worker::Snapshot>,
}
impl Prepared {pub(super) fn auto_text_current(&self)->bool{self.auto_text.iter().all(|snapshot|snapshot.is_live())}}
#[derive(Default)]
pub(crate) struct NativeRenderer {
    renderer: crate::preview::OutputRenderer,
    render_gpu: crate::render_gpu::RenderGpu,
    resource_generation: Option<u64>,
    delay_trace:Vec<skylive_core::Id>,
}
impl NativeRenderer {
    pub(crate) fn new(render_gpu:crate::render_gpu::RenderGpu)->Self {
        Self {renderer:render_gpu.output_renderer(),render_gpu,resource_generation:None,delay_trace:Vec::new()}
    }
}
impl Renderer for NativeRenderer {
    fn render(&mut self, config: &Config, frame: &ProgramVideoFrame) -> Result<Prepared, Error> {
        self.delay_trace.clear();
        config.validate_frame(frame, None)?;
        if self.resource_generation != Some(frame.resource_generation) {
            self.renderer = self.render_gpu.output_renderer();
            self.resource_generation = Some(frame.resource_generation);
        }
        let output = self
            .renderer
            .render_streaming_profile_output(
                &frame.model,
                &frame.output,
                &frame.playback,
                &frame.capture,
            )
            .map_err(|e|if matches!(&e,crate::preview::PreviewError::AutoTextRetry){Error::AutoTextRetry}else{Error::Render(e.to_string())})?;
        if !output.auto_text_current(){return Err(Error::AutoTextRetry);}
        let auto_text=output.auto_text_guards();
        let bytes = output
            .image
            .pixels()
            .len()
            .checked_add(
                (u64::from(config.video.width()) * u64::from(config.video.height()) * 3 / 2)
                    as usize,
            )
            .ok_or(Error::Limit("prepared bytes"))?;
        if bytes > 50 * 1024 * 1024 {
            return Err(Error::Limit("prepared50MiB"));
        }
        // Reuse the already bounded32-entry/64KiB immutable warning projection.
        // Source names are captured from this frame or its exact retained scenes.
        if output.diagnostics.is_empty(){self.delay_trace=output.camera_trace.clone();}
        let warnings = crate::recording::RecordingWarnings::from_frame(frame, output.diagnostics);
        let nv12 = skylive_media::recording_nv12(&output.image, 32 * 1024 * 1024, 18_000_000)
            .map_err(|e| Error::Render(e.to_string()))?
            .into_boxed_slice();
        if auto_text.iter().any(|snapshot|!snapshot.is_live()){return Err(Error::AutoTextRetry);}
        Ok(Prepared { nv12, warnings, auto_text })
    }
    fn take_delay_trace(&mut self)->Vec<skylive_core::Id>{std::mem::take(&mut self.delay_trace)}
}
