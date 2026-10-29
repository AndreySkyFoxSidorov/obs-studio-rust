//! Dialog-owned pixels-only preview port. No capture admission or model edit.
use eframe::egui;
use skylive_core::{Session,VideoFilter,VideoFilterPreviewTarget};
use skylive_host::{capture::CaptureSnapshot,playback::PlaybackSnapshot,
    preview::{FilterPreview,PreviewWorker,PreviewError},render_gpu::RenderGpu,
    transition::{CompositionKey,OutputKey}};
use std::sync::Arc;
use std::time::{Duration,Instant};
// A live camera can publish frames much faster than a failed filter recovers.
// Bound retries so a persistent GPU/resource failure does not render on every frame.
const ERROR_RETRY:Duration=Duration::from_millis(500);
// Projection or timing admission errors may persist until another owner
// retires. Keep those retries slower than ordinary per-frame failures.
const PREPARE_RETRY:Duration=Duration::from_secs(2);
fn retryable_prepare(error:&PreviewError)->bool {
    matches!(error,PreviewError::Busy|PreviewError::StaleCapture|PreviewError::StalePlayback
        |PreviewError::Media(skylive_media::MediaError::BudgetExceeded|skylive_media::MediaError::StaleFrame|skylive_media::MediaError::SharedResource)
        |PreviewError::ScrollClock(_)|PreviewError::TemporalClock(_)
        |PreviewError::Transition(skylive_host::transition::TransitionError::Busy|skylive_host::transition::TransitionError::Limit(_)))
}
#[derive(Clone,PartialEq)]
struct Key {
    session:u64,revision:u64,target:VideoFilterPreviewTarget,filters:Vec<VideoFilter>,window:u64,rows:Vec<u32>,
    resource_generation:u64,dimensions:(u32,u32),playback_epoch:u64,capture_epoch:u64,
}
#[derive(Clone,Copy)]
struct Sample {
    epoch:u64,output:OutputKey,composition:CompositionKey,time_ns:u64,
    capture_generation:u64,width:u32,height:u32,worker_generation:u64,
    modal_raw:Option<(skylive_core::Id,(u64,u64,Option<u64>))>,
}
#[derive(Default)]
pub struct Preview {
    scroll:skylive_host::scroll_animation::Clock,
    mask_clock:skylive_host::mask_animation_clock::DraftClock,
    worker:Option<PreviewWorker>,prepared:Option<FilterPreview>,key:Option<Key>,
    submitted:Option<Sample>,epoch:u64,worker_generation:u64,
    resource_generation:Option<u64>,observed:Option<Result<Vec<VideoFilter>,String>>,
    texture:Option<egui::TextureHandle>,texture_dimensions:Option<(u32,u32)>,
    texture_owner:Option<(u64,u64,VideoFilterPreviewTarget)>,source_dimensions:Option<(u32,u32)>,ready:bool,next_at_ns:Option<u64>,
    error:Option<String>,diagnostics:Vec<skylive_host::preview::SourceDiagnostic>,gpu_notice:Option<String>,retry_at:Option<Instant>,
}
impl Preview {
    fn invalidate(&mut self) {
        if let Some(worker)=&mut self.worker {worker.invalidate();}
        self.scroll.reset();self.prepared=None;self.key=None;self.submitted=None;self.ready=false;self.next_at_ns=None;self.retry_at=None;self.gpu_notice=None;
    }
    pub fn observe(&mut self,filters:Result<Vec<VideoFilter>,String>,context:&egui::Context) {
        if self.observed.as_ref()==Some(&filters) {return;}
        self.observed=Some(filters.clone());
        match filters {
            Ok(filters)=>{
                if self.key.as_ref().is_some_and(|key|key.filters!=filters) {
                    self.invalidate();context.request_repaint();
                }
            }
            Err(error)=>{self.invalidate();self.error=Some(error);}
        }
        context.request_repaint();
    }
    #[allow(clippy::too_many_arguments)]
    pub fn update(&mut self,context:&egui::Context,session:&Session,binding:(u64,u64),target:VideoFilterPreviewTarget,window:u64,rows:&[u32],
        filters:Result<Vec<VideoFilter>,String>,playback:&PlaybackSnapshot,capture:&Arc<CaptureSnapshot>,render_gpu:&RenderGpu,resource_generation:u64) {
        // A retained frame is a visual fallback only for the same dialog and
        // source. Never carry it across a different document or source.
        if self.texture_owner.is_some_and(|owner|owner!=(binding.0,window,target)) {
            self.texture=None;self.texture_dimensions=None;self.texture_owner=None;self.source_dimensions=None;
        }
        if session.is_closed() || binding!=(session.identity(),session.snapshot().revision())
            || playback.session()!=session.identity() || playback.revision()!=binding.1 || capture.session()!=binding.0 {
            self.invalidate();self.texture=None;self.texture_dimensions=None;self.texture_owner=None;self.source_dimensions=None;
            self.error=Some("The filter preview belongs to another document or revision.".into());return;
        }
        let filters=match filters {Ok(value)=>value,Err(error)=>{self.invalidate();self.error=Some(error);return;}};
        if window==0||rows.len()!=filters.len()||rows.len()>skylive_core::MAX_VIDEO_FILTERS||rows.iter().enumerate().any(|(index,row)|*row==0||rows[..index].contains(row)){self.invalidate();self.error=Some("Invalid filter preview draft row ownership".into());return;}
        let dimensions=match FilterPreview::draft_source_dimensions(&session.snapshot(),target,capture) {
            Ok(value)=>value,Err(error)=>{
                // Capture can temporarily have no admitted raw frame while a
                // delay queue is filling or the device is reconnecting. This
                // error happens before the worker retry machinery below, so a
                // static dialog might otherwise never ask for another frame.
                let live_source=match target {
                    VideoFilterPreviewTarget::Source(id)=>session.snapshot().source(id).is_some_and(|source|
                        matches!(source.kind(),skylive_core::SourceKind::VideoCapture {..}|skylive_core::SourceKind::NdiSource {..})),
                    VideoFilterPreviewTarget::Scene(_)=>false,
                };
                let retry=retryable_prepare(&error)||(live_source&&matches!(&error,PreviewError::NotFound(_)));
                self.source_dimensions=None;self.invalidate();self.error=Some(error.to_string());
                if retry {context.request_repaint_after(PREPARE_RETRY);}
                return;
            }
        };
        self.source_dimensions=Some(dimensions);
        if self.worker.is_none() {
            if let Some(deadline)=self.retry_at.filter(|deadline|Instant::now()<*deadline) {
                context.request_repaint_after(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1)));
                return;
            }
            match render_gpu.filter_preview_worker() {
                Ok(worker)=>{
                    // Every new PreviewWorker starts its resource generation
                    // at zero. Carrying the previous owner's counter into the
                    // sample key rejects every result after worker recovery.
                    self.worker=Some(worker);
                    self.worker_generation=0;
                },
                Err(error)=>{self.error=Some(error.to_string());self.retry_at=Some(Instant::now()+ERROR_RETRY);context.request_repaint_after(ERROR_RETRY);return;}
            }
        }
        let key=Key {session:binding.0,revision:binding.1,target,filters,window,rows:rows.to_vec(),resource_generation,dimensions,
            playback_epoch:playback.epoch(),capture_epoch:capture.epoch()};
        let prepare_retry=self.prepared.is_none()&&self.key.as_ref()==Some(&key)
            &&self.retry_at.is_some_and(|deadline|Instant::now()>=deadline);
        if self.key.as_ref()!=Some(&key)||prepare_retry {
            self.prepared=None;self.ready=false;self.submitted=None;self.next_at_ns=None;self.retry_at=None;self.diagnostics.clear();
            let retry_resources=self.resource_generation.is_some_and(|old|old!=resource_generation);
            if let Some(worker)=&mut self.worker {
                worker.invalidate();
                if retry_resources {
                    if let Err(error)=worker.invalidate_resources(){
                        // A worker whose resource identity cannot advance can
                        // never accept this reload. Retire it rather than retry
                        // the same failure on every camera repaint.
                        if let Some(mut retired)=self.worker.take(){retired.close();}
                        self.invalidate();
                        self.error=Some(error.to_string());
                        self.retry_at=Some(Instant::now()+ERROR_RETRY);
                        context.request_repaint_after(ERROR_RETRY);
                        return;
                    }
                    let Some(next)=self.worker_generation.checked_add(1) else {self.error=Some("Filter preview generation exhausted.".into());return;};
                    self.worker_generation=next;
                }
            }
            self.resource_generation=Some(resource_generation);
            let Some(epoch)=self.epoch.checked_add(1) else {self.error=Some("Filter preview identity exhausted.".into());return;};
            self.epoch=epoch;self.ready=false;self.submitted=None;self.next_at_ns=None;self.diagnostics.clear();self.error=None;
            match FilterPreview::prepare_with_rows(binding.0,&session.snapshot(),target,key.filters.clone(),capture,key.window,&key.rows) {
                Ok(prepared)=>self.prepared=Some(prepared),
                Err(error)=>{
                    self.error=Some(error.to_string());self.key=Some(key);
                    if retryable_prepare(&error){self.retry_at=Some(Instant::now()+PREPARE_RETRY);context.request_repaint_after(PREPARE_RETRY);}
                    return;
                }
            }
            self.key=Some(key);
            context.request_repaint();
        }
        if self.prepared.is_none(){
            if let Some(deadline)=self.retry_at {context.request_repaint_after(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1)));}
            return;
        }
        // Do not hammer admission/timer state on every camera repaint while an
        // earlier failure is waiting for its retry deadline. Any active worker
        // result remains owned and will be polled after this bounded delay.
        if let Some(deadline)=self.retry_at.filter(|deadline|Instant::now()<*deadline) {
            context.request_repaint_after(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1)));
            return;
        }
        let Some(key)=&self.key else{return;};
        if let Some(prepared)=&self.prepared{
            let Some(domain)=self.worker.as_ref().and_then(PreviewWorker::freeze_domain)else{self.ready=false;self.error=Some("Filter preview timer owner is unavailable".into());return;};
            if let Err(error)=prepared.observe_freeze(render_gpu,key.window,&key.rows,domain,playback.now().0){
                self.ready=false;self.error=Some(error);self.submitted=None;self.next_at_ns=None;self.retry_at=Some(Instant::now()+PREPARE_RETRY);
                context.request_repaint_after(PREPARE_RETRY);return;
            }
        }
        let Some(worker)=&mut self.worker else{return;};
        let was_busy=worker.is_busy();
        match worker.poll_with_capture(session,capture) {
            Ok(Some(result))=>{
                let fresh=self.submitted.is_some_and(|sample|sample.epoch==self.epoch
                    && result.key.session==key.session && result.key.revision==key.revision
                    && result.key.playback_epoch==key.playback_epoch && result.key.capture_epoch==key.capture_epoch
                    && result.key.capture_generation==sample.capture_generation && result.key.output==sample.output
                    && result.key.composition==sample.composition && result.key.time_ns==sample.time_ns
                    && result.key.width==sample.width && result.key.height==sample.height
                    && result.key.resource_generation==sample.worker_generation
                    && sample.modal_raw.is_none_or(|(source,identity)|capture.modal_raw_identity(source)==Some(identity)));
                if fresh {
                    // A failed source/filter produces a transparent image plus
                    // diagnostics. Keep the last good sample visible instead
                    // of replacing it with the checkerboard; the warning and
                    // updating indicator still make the stale frame explicit.
                    let good=result.diagnostics.is_empty();
                    if good || self.texture.is_none(){
                        let image=egui::ColorImage::from_rgba_unmultiplied([result.key.width as usize,result.key.height as usize],result.image.pixels());
                        if let Some(texture)=&mut self.texture {texture.set(image,egui::TextureOptions::LINEAR);}
                        else {self.texture=Some(context.load_texture("video-filter-preview",image,egui::TextureOptions::LINEAR));}
                        self.texture_dimensions=self.prepared.as_ref().map(FilterPreview::dimensions);
                        self.texture_owner=Some((binding.0,window,target));
                    }
                    self.ready=good;self.error=None;self.diagnostics=result.diagnostics;self.gpu_notice=result.gpu_notice;
                    self.next_at_ns=result.next_after_ns.and_then(|after|result.key.time_ns.checked_add(after.max(1)));
                    if good {self.retry_at=None;} else {
                        // Also retry a still image when no capture generation or
                        // animation deadline changes after a transient failure.
                        self.submitted=None;
                        self.retry_at=Some(Instant::now()+ERROR_RETRY);
                    }
                } else {
                    // A completed result that no longer matches the submitted
                    // draft must not suppress a replacement on a still frame.
                    self.submitted=None;
                }
            }
            Ok(None)=>{
                // poll_with_capture can consume and discard a completion after
                // invalidation or a capture-epoch change. No later camera frame
                // is guaranteed to wake this draft, so allow another request.
                if was_busy && !worker.is_busy() {self.submitted=None;}
            },Err(error)=>{
                // A disconnected render thread cannot recover by retrying the
                // same worker. Retire it and prepare a new owner after the
                // bounded retry interval while retaining the last good texture.
                if matches!(error,PreviewError::Disconnected|PreviewError::Closed) {
                    if let Some(mut retired)=self.worker.take(){retired.close();}
                    self.invalidate();
                    self.error=Some(error.to_string());
                    self.retry_at=Some(Instant::now()+ERROR_RETRY);
                    context.request_repaint_after(ERROR_RETRY);
                    return;
                }
                self.ready=false;self.error=Some(error.to_string());self.diagnostics.clear();self.next_at_ns=None;
                self.submitted=None;self.retry_at=Some(Instant::now()+ERROR_RETRY);
            }
        }
        let due=self.next_at_ns.is_some_and(|deadline|playback.now().0>=deadline);
        let capture_changed=self.submitted.is_some_and(|sample|sample.capture_generation!=capture.generation());
        let retry_ready=self.retry_at.is_none_or(|deadline|Instant::now()>=deadline);
        if retry_ready && (self.submitted.is_none() || due || capture_changed) && !worker.is_busy() {
            if let Some(prepared)=&self.prepared {
                let (width,height)=prepared.render_dimensions();
                let modal_raw=match prepared.modal_raw_source() {
                    Some(source)=>match capture.modal_raw_identity(source) {
                        Some(identity)=>Some((source,identity)),
                        None=>{self.ready=false;self.submitted=None;self.error=Some("The raw camera preview is temporarily unavailable.".into());
                            self.retry_at=Some(Instant::now()+PREPARE_RETRY);context.request_repaint_after(PREPARE_RETRY);return;}
                    },
                    None=>None,
                };
                let sample=Sample {epoch:self.epoch,output:prepared.output_key(),composition:prepared.sample_key(),
                    time_ns:playback.now().0,capture_generation:capture.generation(),width,height,worker_generation:self.worker_generation,modal_raw};
                let sample_playback=match prepared.draft_playback(playback,capture,render_gpu,&mut self.scroll,&mut self.mask_clock){Ok(sample)=>sample,Err(error)=>{
                    self.ready=false;self.error=Some(error.to_string());self.submitted=None;
                    self.retry_at=Some(Instant::now()+PREPARE_RETRY);context.request_repaint_after(PREPARE_RETRY);return;
                }};
                match worker.request_filter_preview(session,prepared,sample_playback,capture.clone()) {
                    Ok(())=>{self.submitted=Some(sample);self.retry_at=None;},
                    Err(PreviewError::Busy)=>{self.submitted=None;self.retry_at=None;context.request_repaint_after(Duration::from_millis(50));},
                    Err(error)=>{self.submitted=None;self.ready=false;self.error=Some(error.to_string());self.retry_at=Some(Instant::now()+ERROR_RETRY);}
                }
            }
        }
        if worker.is_busy() {context.request_repaint_after(std::time::Duration::from_millis(16));}
        else if let Some(deadline)=self.retry_at {context.request_repaint_after(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1)));}
        else if let Some(deadline)=self.next_at_ns {context.request_repaint_after(std::time::Duration::from_nanos(deadline.saturating_sub(playback.now().0).max(1_000_000)));}
    }
    pub fn show(&self,ui:&mut egui::Ui,height:f32) {
        let width=ui.available_width().max(1.0);
        let (area,response)=ui.allocate_exact_size(egui::vec2(width,height.max(60.0)),egui::Sense::hover());
        response.on_hover_text(crate::i18n::tr(ui,"Filters run from top to bottom, matching OBS. Changes affect every scene reference to this source. Apply saves the stack; Cancel discards the draft."));
        ui.painter().rect_filled(area,4.0,crate::theme::preview_background(ui));
        if let (Some(texture),Some((w,h)))=(&self.texture,self.texture_dimensions) {
                let scale=(area.width()/w as f32).min(area.height()/h as f32);
                let rect=egui::Rect::from_center_size(area.center(),egui::vec2(w as f32*scale,h as f32*scale));
                ui.put(rect,egui::Image::new(texture).fit_to_exact_size(rect.size()).alt_text(crate::i18n::tr(ui,"Preview")));
        }
        if !self.ready {
            let text=if self.error.is_some()||!self.diagnostics.is_empty(){"Unavailable"}else{"Updating preview…"};
            let (pos,align)=if self.texture.is_some(){(area.left_top()+egui::vec2(6.0,4.0),egui::Align2::LEFT_TOP)}else{(area.center(),egui::Align2::CENTER_CENTER)};
            ui.painter().text(pos,align,crate::i18n::tr(ui,text),egui::FontId::proportional(13.0),ui.visuals().text_color());
        }
        if let Some(error)=&self.error {ui.colored_label(ui.visuals().error_fg_color,crate::i18n::tr(ui,error));}
        if let Some(notice)=&self.gpu_notice {ui.label(format!("GPU: {notice}"));}
        if !self.diagnostics.is_empty() {
            egui::ScrollArea::vertical().id_salt("filter-preview-diagnostics").max_height(52.0).show(ui,|ui|{
                for problem in &self.diagnostics {ui.label(crate::i18n::tf(ui,"{code}: {detail}",&[("{code}",problem.code.to_string()),("{detail}",crate::i18n::tr(ui,&problem.detail))]));}
            });
        }
    }
    pub fn close(&mut self){if let Some(worker)=&mut self.worker {worker.close();}self.invalidate();self.texture=None;self.texture_dimensions=None;self.texture_owner=None;self.source_dimensions=None;self.mask_clock.reset();}
    pub fn source_dimensions(&self)->Option<(u32,u32)>{self.source_dimensions}
    pub fn freeze_source(&self)->Option<skylive_core::Id>{self.prepared.as_ref().and_then(FilterPreview::freeze_owner)}
    pub fn refresh_frame(&mut self){if let Some(worker)=&mut self.worker{worker.invalidate();}self.submitted=None;self.next_at_ns=None;self.retry_at=None;}
}
impl Drop for Preview {fn drop(&mut self){self.close();}}
