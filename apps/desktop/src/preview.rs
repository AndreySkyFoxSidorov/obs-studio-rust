//! Presentation-only textures. Rendering belongs to the bounded host worker.
use eframe::egui;
use skylive_core::{Id, Session};
use skylive_host::capture::CaptureSnapshot;
use skylive_host::playback::PlaybackSnapshot;
use skylive_host::preview::{PreviewError, PreviewWorker, SourceDiagnostic};
use skylive_host::transition::{CompositionKey, FrameComposition, FrameScene, OutputKey};
use std::sync::Arc;
use std::time::{Duration, Instant};

// A failed worker request may recover without any camera or scene change.
// Back off to avoid repeatedly submitting a costly failing filter every frame.
const ERROR_RETRY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ViewKey {
    session: u64,
    revision: u64,
    scene: Id,
    width: u32,
    height: u32,
    playback_epoch: u64,
    resource_generation: u64,
    capture_epoch: u64,
    output: OutputKey,
    draft_epoch: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SampleKey {
    view: ViewKey,
    time_ns: u64,
    composition: CompositionKey,
    capture_generation: u64,
}

#[derive(Clone, Copy, Default)]
pub struct Metrics {
    pub submitted: u64,
    pub published: u64,
    pub failures: u64,
    pub last_latency_ms: Option<f64>,
}
pub struct PreviewPane {
    metrics: Metrics,
    submitted_at: Option<std::time::Instant>,
    worker: PreviewWorker,
    render_gpu: skylive_host::render_gpu::RenderGpu,
    worker_disconnected: bool,
    texture: Option<egui::TextureHandle>,
    submitted: Option<SampleKey>,
    desired: Option<ViewKey>,
    displayed: Option<SampleKey>,
    displayed_auto:Option<skylive_host::preview::AutoTextView>,
    next_at_ns: Option<u64>,
    retry_at: Option<Instant>,
    diagnostic_retry_at: Option<Instant>,
    resource_generation: u64,
    #[cfg(test)]
    first_pixel: Option<[u8; 4]>,
    pub error: Option<String>,
    diagnostics: Vec<SourceDiagnostic>,
    name: &'static str,
}
impl PreviewPane {
    #[cfg(test)]
    pub fn new(name: &'static str) -> std::io::Result<Self> {
        Self::new_with_render_gpu(name,&skylive_host::render_gpu::RenderGpu::cpu())
    }
    pub fn new_with_render_gpu(name:&'static str,render_gpu:&skylive_host::render_gpu::RenderGpu)->std::io::Result<Self> {
        Ok(Self {
            metrics: Metrics::default(),
            submitted_at: None,
            worker: render_gpu.preview_worker()?,
            render_gpu: render_gpu.clone(),
            worker_disconnected: false,
            texture: None,
            submitted: None,
            desired: None,
            displayed: None,
            displayed_auto:None,
            next_at_ns: None,
            retry_at: None,
            diagnostic_retry_at: None,
            resource_generation: 0,
            #[cfg(test)]
            first_pixel: None,
            error: None,
            diagnostics: Vec::new(),
            name,
        })
    }
    pub fn metrics(&self) -> Metrics { self.metrics }
    /// Geometry witness for exactly the pixels currently shown in this pane.
    #[allow(dead_code)]
    pub fn displayed_auto_text(&self)->Option<&skylive_host::preview::AutoTextView>{self.displayed_auto.as_ref()}
    #[cfg(test)]
    pub fn update_playback(
        &mut self,
        context: &egui::Context,
        session: &Session,
        scene: Id,
        playback: &PlaybackSnapshot,
    ) {
        self.update_playback_with_capture(
            context,
            session,
            scene,
            playback,
            &Arc::new(CaptureSnapshot::empty(session.identity())),
        );
    }
    pub fn update_playback_with_capture(
        &mut self,
        context: &egui::Context,
        session: &Session,
        scene: Id,
        playback: &PlaybackSnapshot,
        capture: &Arc<CaptureSnapshot>,
    ) {
        self.update_output_with_capture(
            context,
            session,
            playback,
            &FrameComposition::Scene(FrameScene::Live(scene)),
            capture,
        );
    }
    #[cfg(test)]
    pub fn update_output(
        &mut self,
        context: &egui::Context,
        session: &Session,
        playback: &PlaybackSnapshot,
        output: &FrameComposition,
    ) {
        self.update_output_with_capture(
            context,
            session,
            playback,
            output,
            &Arc::new(CaptureSnapshot::empty(session.identity())),
        );
    }
    pub fn update_output_with_capture(
        &mut self,
        context: &egui::Context,
        session: &Session,
        playback: &PlaybackSnapshot,
        output: &FrameComposition,
        capture: &Arc<CaptureSnapshot>,
    ) {
        self.update_sample(context, session, playback, output, capture, 0);
    }
    /// A gesture has stable view identity; completed intermediate samples are
    /// accepted while later pointer positions coalesce behind the one worker.
    pub fn update_draft_with_capture(
        &mut self,
        context: &egui::Context,
        session: &Session,
        playback: &PlaybackSnapshot,
        output: &FrameComposition,
        capture: &Arc<CaptureSnapshot>,
        epoch: u64,
    ) {
        self.update_sample(context, session, playback, output, capture, epoch);
    }
    fn update_sample(
        &mut self,
        context: &egui::Context,
        session: &Session,
        playback: &PlaybackSnapshot,
        output: &FrameComposition,
        capture: &Arc<CaptureSnapshot>,
        draft_epoch: u64,
    ) {
        let scene = output.primary_scene();
        let video = session.snapshot();
        let output_video=match video.output_video(){Ok(value)=>value,Err(error)=>{self.error=Some(error.to_string());self.desired=None;return;}};
        let (width,height)=crate::preview_display::render_dimensions_profile(context,self.name,
            [video.video().width(),video.video().height()],[output_video.width(),output_video.height()]);
        let key = ViewKey {
            session: session.identity(),
            revision: video.revision(),
            scene,
            width,
            height,
            playback_epoch: playback.epoch(),
            resource_generation: self.resource_generation,
            capture_epoch: capture.epoch(),
            output: if draft_epoch == 0 {
                output.identity()
            } else {
                FrameComposition::Scene(FrameScene::Live(scene)).identity()
            },
            draft_epoch,
        };
        let now = playback.now().0;
        if playback.session() != session.identity()
            || playback.revision() != video.revision()
            || capture.session() != session.identity()
        {
            self.error = Some("Playback snapshot belongs to another session or revision".into());
            return;
        }
        if self.worker_disconnected {
            if self.retry_at.is_some_and(|deadline| Instant::now() < deadline) {
                context.request_repaint_after(self.retry_at.unwrap().saturating_duration_since(Instant::now()).max(Duration::from_millis(1)));
                return;
            }
            match self.render_gpu.preview_worker() {
                Ok(mut worker) => {
                    worker.sync_resource_generation(self.resource_generation);
                    self.worker = worker;
                    self.worker_disconnected = false;
                    self.desired = None;
                    self.submitted = None;
                    self.submitted_at = None;
                    self.retry_at = None;
                }
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.retry_at = Some(Instant::now() + ERROR_RETRY);
                    context.request_repaint_after(ERROR_RETRY);
                    return;
                }
            }
        }
        if self.desired != Some(key) {
            self.worker.invalidate();
            self.desired = Some(key);
            self.submitted = None;
            self.submitted_at = None;
            self.next_at_ns = None;
            self.retry_at = None;
            self.diagnostic_retry_at = None;
            self.diagnostics.clear();
            self.error = None;
        }
        // Keep an error visible while waiting and do not keep polling a
        // disconnected worker on every camera repaint.
        let retry_ready = self.retry_at.is_none_or(|deadline| Instant::now() >= deadline);
        let was_busy = self.worker.is_busy();
        if retry_ready || self.worker.is_busy() { match self.worker.poll_with_capture(session, capture) {
            Ok(Some(result)) => {
                let returned = SampleKey {
                    view: ViewKey {
                        session: result.key.session,
                        revision: result.key.revision,
                        scene: result.key.scene,
                        width: result.key.width,
                        height: result.key.height,
                        playback_epoch: result.key.playback_epoch,
                        resource_generation: result.key.resource_generation,
                        capture_epoch: result.key.capture_epoch,
                        output: if draft_epoch == 0 {
                            result.key.output
                        } else {
                            key.output
                        },
                        draft_epoch,
                    },
                    time_ns: result.key.time_ns,
                    composition: result.key.composition,
                    capture_generation: result.key.capture_generation,
                };
                // The worker checks its exact request serial; the pane checks
                // the captured sample as well. Advancing wall time alone does
                // not invalidate a completed frame or starve publication.
                if returned.view == key && self.submitted == Some(returned) && result.auto_text_current() {
                    self.metrics.published = self.metrics.published.saturating_add(1);
                    self.metrics.last_latency_ms = self.submitted_at.take().map(|at| at.elapsed().as_secs_f64()*1000.0);
                    #[cfg(test)]
                    {
                        self.first_pixel = result
                            .image
                            .pixels()
                            .get(..4)
                            .and_then(|pixel| pixel.try_into().ok());
                    }
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        result.image.pixels(),
                    );
                    if let Some(texture) = &mut self.texture {
                        texture.set(image, egui::TextureOptions::LINEAR);
                    } else {
                        self.texture = Some(context.load_texture(
                            self.name,
                            image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                    self.displayed = Some(returned);
                    self.displayed_auto=Some(result.auto_text_view());
                    self.next_at_ns = result
                        .next_after_ns
                        .and_then(|after| returned.time_ns.checked_add(after.max(1)));
                    self.diagnostics = result.diagnostics;
                    // A source may temporarily fail without publishing a new
                    // camera frame or changing the scene. Retry that render,
                    // but keep normal live frames and animation unthrottled.
                    self.diagnostic_retry_at = (!self.diagnostics.is_empty())
                        .then(|| Instant::now() + ERROR_RETRY);
                    self.retry_at = None;
                    self.error = if result.next_after_ns.is_some() && self.next_at_ns.is_none() {
                        Some("Animation deadline exceeds supported nanoseconds".into())
                    } else {
                        None
                    };
                } else {
                    // A result that no longer belongs to our sample must not
                    // occupy the submission slot after the worker retired it.
                    self.submitted = None;
                    self.submitted_at = None;
                }
            }
            Ok(None) => {
                if was_busy && !self.worker.is_busy() {
                    // The worker retired an obsolete result without publishing
                    // it (for example after the capture epoch changed).
                    self.submitted = None;
                    self.submitted_at = None;
                }
            }
            Err(error) => {
                if matches!(error, PreviewError::Disconnected) {
                    // close only detaches the channels; it never joins a render
                    // thread on the UI thread.
                    self.worker.close();
                    self.worker_disconnected = true;
                }
                self.metrics.failures = self.metrics.failures.saturating_add(1);
                self.error = Some(error.to_string());
                self.next_at_ns = None;
                self.submitted = None;
                self.submitted_at = None;
                self.retry_at = Some(Instant::now() + ERROR_RETRY);
                self.diagnostic_retry_at = None;
            }
        }}
        let due = self.next_at_ns.is_some_and(|deadline| now >= deadline);
        // A settled held T-bar has no timer. A later slider move can change its
        // target while keeping the same pair key; accept pending samples, then
        // coalesce the newest evaluated progress when the worker is free.
        let manual_changed = if let (
            FrameComposition::Manual {
                progress_num,
                progress_den,
                ..
            },
            Some(sample),
        ) = (output, self.displayed)
        {
            sample.view == key
                && self.error.is_none()
                && match sample.composition {
                    CompositionKey::Manual {
                        progress_num: old_num,
                        progress_den: old_den,
                        ..
                    } => old_num != *progress_num || old_den != *progress_den,
                    _ => true,
                }
        } else {
            false
        };
        let capture_changed = self.submitted.is_some_and(|sample| {
            sample.view == key && sample.capture_generation != capture.generation()
        });
        let draft_changed = draft_epoch != 0
            && self
                .displayed
                .is_none_or(|sample| sample.composition != output.sample_key());
        let diagnostic_due = self.diagnostic_retry_at.is_some_and(|deadline| Instant::now() >= deadline);
        let retry_ready = self.retry_at.is_none_or(|deadline| Instant::now() >= deadline);
        if !self.worker_disconnected && retry_ready && (self.submitted.is_none() || due || manual_changed || capture_changed || draft_changed || diagnostic_due)
            && !self.worker.is_busy()
        {
            let sample = SampleKey {
                view: key,
                time_ns: now,
                composition: output.sample_key(),
                capture_generation: capture.generation(),
            };
            match self.worker.request_output_with_capture(
                session,
                width,
                height,
                playback.clone(),
                output.clone(),
                capture.clone(),
            ) {
                Ok(()) => {
                    self.submitted = Some(sample);
                    self.retry_at = None;
                    self.diagnostic_retry_at = None;
                    self.submitted_at = Some(std::time::Instant::now());
                    self.metrics.submitted = self.metrics.submitted.saturating_add(1);
                },
                Err(PreviewError::Busy) => {
                    self.submitted = None;
                    self.retry_at = Some(Instant::now() + Duration::from_millis(50));
                    self.diagnostic_retry_at = None;
                }
                Err(error) => {
                    if matches!(error, PreviewError::Disconnected) {
                        self.worker.close();
                        self.worker_disconnected = true;
                    }
                    self.error = Some(error.to_string());
                    self.next_at_ns = None;
                    self.submitted = None;
                    self.retry_at = Some(Instant::now() + ERROR_RETRY);
                    self.diagnostic_retry_at = None;
                }
            }
        }
        if self.worker.is_busy() {
            context.request_repaint_after(std::time::Duration::from_millis(16));
        } else if let Some(deadline) = self.retry_at {
            context.request_repaint_after(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1)));
        } else {
            if let Some(deadline) = self.diagnostic_retry_at {
                context.request_repaint_after(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1)));
            }
            if let Some(deadline) = self.next_at_ns {
                // OBS uses an inclusive GIF frame edge. Its 1 ns deadline needs
                // an actual positive OS timer; avoid a zero-delay repaint spin.
                context.request_repaint_after(Duration::from_nanos(deadline.saturating_sub(now).max(1_000_000)));
            } else if output.is_manual() && self.error.is_none() {
                if let Some(after) = output.next_after_ns(&video) {
                    context.request_repaint_after(Duration::from_nanos(after.max(1_000_000)));
                }
            }
        }
    }
    #[cfg(test)]
    pub fn show(&self, ui: &mut egui::Ui, title: &str, session: &Session, scene: Id) -> egui::Rect {
        self.show_with_actions(ui,title,session,scene,&mut Vec::new())
    }
    pub fn show_with_actions(&self, ui:&mut egui::Ui,title:&str,session:&Session,scene:Id,intents:&mut Vec<crate::views::Intent>)->egui::Rect {
        let target=if self.name=="program"{skylive_host::screenshot::Target::Program}else{skylive_host::screenshot::Target::Preview};
        ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(ui,title)).strong()).sense(egui::Sense::click())).on_hover_text(crate::i18n::tr(ui,"Right-click to save a full-size PNG screenshot of this view."))
            .context_menu(|ui|{crate::projectors::menu(ui,session.identity(),session.snapshot().revision(),if self.name=="program"{skylive_host::projector::Target::Program}else{skylive_host::projector::Target::Preview},intents);ui.separator();crate::views::screenshot::button(ui,session.identity(),session.snapshot().revision(),target,!session.is_closed(),intents);});
        let collection = session.snapshot();
        let fresh = self.displayed.is_some_and(|value| {
            self.desired == Some(value.view)
                && value.view.session == session.identity()
                && value.view.revision == collection.revision()
                && value.view.scene == scene
        });
        // Keep the last image for this scene while a filter edit renders the
        // next revision. Never carry an image across sessions or scenes.
        let drawable = self.displayed.is_some_and(|value| {
            value.view.session == session.identity() && value.view.scene == scene
        });
        let available = ui.available_size();
        let aspect = collection.video().width() as f32 / collection.video().height() as f32;
        let diagnostics_height = if fresh && !self.diagnostics.is_empty() {
            88.0 + ui.spacing().item_spacing.y
        } else {
            0.0
        };
        let error_height = if self.error.is_some() {
            36.0 + ui.spacing().item_spacing.y
        } else {
            0.0
        };
        let size = egui::vec2(
            available.x.max(1.0),
            (available.y - diagnostics_height - error_height).max(1.0),
        );
        let (stage, stage_response) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.ctx().data_mut(|data|data.insert_temp(egui::Id::new(("preview-pane-viewport",self.name)),stage.intersect(ui.clip_rect())));
        stage_response.on_hover_text(crate::i18n::tr(ui,if title=="Program" {
            "Program shows the composition used for streaming and recording. Use Controls to start or stop an output."
        } else {
            "Preview shows the selected scene. In Studio Mode, use Transition to send it to Program."
        }));
        ui.painter()
            .rect_filled(stage, 0.0, crate::theme::preview_background(ui));
        let padding = stage.width().min(stage.height()).min(64.0) * 0.5;
        let fitted = fit_preview_rect(stage.shrink(padding), aspect);
        let output_video=match collection.output_video(){Ok(value)=>value,Err(error)=>{ui.label(error.to_string());return egui::Rect::NOTHING;}};
        let rect=crate::preview_display::rectangle_profile(ui,self.name,stage.shrink(padding),fitted,
            [collection.video().width(),collection.video().height()],[output_video.width(),output_video.height()]);
        if drawable {
            if let Some(texture) = &self.texture {
                let mut canvas = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt((self.name, "preview-image"))
                        .max_rect(rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                canvas.set_clip_rect(stage.intersect(ui.clip_rect()));
                canvas.add(
                    egui::Image::new(texture)
                        .fit_to_exact_size(rect.size())
                        .alt_text(crate::i18n::tr(ui, title)),
                );
            }
        } else {
            ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                crate::i18n::tr(ui, "Updating preview…"),
                egui::FontId::proportional(14.0),
                egui::Color32::LIGHT_GRAY,
            );
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if fresh && !self.diagnostics.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt((self.name, "source-diagnostics"))
                .max_height(88.0)
                .show(ui, |ui| {
                    for diagnostic in &self.diagnostics {
                        let source = collection
                            .source(diagnostic.source)
                            .map(|source| source.name())
                            .unwrap_or("Missing source");
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            crate::i18n::tf(
                                ui,
                                "{source} [{}]: {}",
                                &[
                                    ("{source}", (source).to_string()),
                                    ("{}", (diagnostic.code).to_string()),
                                    ("{}", crate::i18n::tr(ui, &diagnostic.detail)),
                                ],
                            ),
                        );
                    }
                });
        }
        rect
    }
    pub fn diagnostics(&self) -> &[SourceDiagnostic] {
        &self.diagnostics
    }
    #[cfg(test)]
    pub fn displayed_pixel(&self) -> Option<([u8; 4], OutputKey)> {
        let sample = self
            .displayed
            .filter(|sample| self.desired == Some(sample.view))?;
        Some((self.first_pixel?, sample.view.output))
    }
    pub fn resource_generation(&self)->u64 {self.resource_generation}
    pub fn refresh_frame(&mut self){self.worker.invalidate();self.submitted=None;self.next_at_ns=None;self.retry_at=None;self.diagnostic_retry_at=None;}
    pub fn close(&mut self) {
        self.worker.close();
        self.worker_disconnected = false;
        self.texture = None;
        self.diagnostics.clear();
        self.submitted = None;
        self.desired = None;
        self.displayed = None;
        self.next_at_ns = None;
        self.retry_at = None;
        self.diagnostic_retry_at = None;
    }
    pub fn retry_resources(&mut self) -> Result<(), skylive_host::preview::PreviewError> {
        let generation = self
            .resource_generation
            .checked_add(1)
            .ok_or(skylive_host::preview::PreviewError::IdentityExhausted)?;
        self.worker.invalidate_resources()?;
        self.resource_generation = generation;
        self.desired = None;
        self.submitted = None;
        self.next_at_ns = None;
        self.retry_at = None;
        self.diagnostic_retry_at = None;
        Ok(())
    }
}

fn fit_preview_rect(stage: egui::Rect, aspect: f32) -> egui::Rect {
    let available = stage.size().max(egui::Vec2::ZERO);
    let size = if available.y == 0.0 || !aspect.is_finite() || aspect <= 0.0 {
        egui::Vec2::ZERO
    } else if available.x > available.y * aspect {
        egui::vec2(available.y * aspect, available.y)
    } else {
        egui::vec2(available.x, available.x / aspect)
    };
    egui::Rect::from_center_size(stage.center(), size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylive_core::{Command, Transform};
    use skylive_host::{assets, playback::PlaybackClock};
    use skylive_media::MediaTime;
    #[test]
    fn displayed_preview_centers_and_scales_in_ordinary_and_studio_panes() {
        fn gather(
            shape: &egui::epaint::Shape,
            stages: &mut Vec<egui::Rect>,
            images: &mut Vec<(egui::TextureId, egui::Rect)>,
        ) {
            match shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.fill == crate::theme::PREVIEW_BACKGROUND =>
                {
                    stages.push(rect.rect)
                }
                egui::epaint::Shape::Rect(rect) => {
                    if let Some(brush) = &rect.brush {
                        images.push((brush.fill_texture_id, rect.rect));
                    }
                }
                egui::epaint::Shape::Mesh(mesh) => {
                    images.push((mesh.texture_id, mesh.calc_bounds()))
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        gather(shape, stages, images);
                    }
                }
                _ => {}
            }
        }
        for studio in [false, true] {
            let context = egui::Context::default();
            crate::theme::install(&context);
            let session = Session::new();
            let scene = session.preview();
            let output = FrameComposition::Scene(FrameScene::Live(scene));
            let mut panes = Vec::new();
            let mut texture_ids = Vec::new();
            for name in if studio {
                vec!["layout-preview", "layout-program"]
            } else {
                vec!["layout-preview"]
            } {
                let mut pane = PreviewPane::new(name).unwrap();
                let view = ViewKey {
                    session: session.identity(),
                    revision: session.snapshot().revision(),
                    scene,
                    width: 640,
                    height: 360,
                    playback_epoch: 0,
                    resource_generation: 0,
                    capture_epoch: 0,
                    output: output.identity(),
                    draft_epoch: 0,
                };
                pane.desired = Some(view);
                pane.displayed = Some(SampleKey {
                    view,
                    time_ns: 0,
                    composition: output.sample_key(),
                    capture_generation: 0,
                });
                let texture = context.load_texture(
                    name,
                    egui::ColorImage::new([640, 360], egui::Color32::RED),
                    egui::TextureOptions::NEAREST,
                );
                texture_ids.push(texture.id());
                pane.texture = Some(texture);
                panes.push(pane);
            }
            let painted = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        if studio {
                            ui.columns(2, |columns| {
                                panes[0].show(&mut columns[0], "Preview", &session, scene);
                                panes[1].show(&mut columns[1], "Program", &session, scene);
                            });
                        } else {
                            panes[0].show(ui, "Preview", &session, scene);
                        }
                    });
                },
            );
            let mut stages = Vec::new();
            let mut images = Vec::new();
            for shape in &painted.shapes {
                gather(&shape.shape, &mut stages, &mut images);
            }
            stages.sort_by(|a, b| a.left().total_cmp(&b.left()));
            assert_eq!(stages.len(), panes.len());
            for (index, texture) in texture_ids.iter().enumerate() {
                let image = images.iter().find(|(id, _)| id == texture).unwrap().1;
                assert!(stages[index].contains_rect(image));
                assert!(
                    image.center().distance(stages[index].center()) <= 1.0,
                    "image must center on both axes"
                );
                assert!((image.width() / image.height() - 16.0 / 9.0).abs() < 0.01);
                if !studio {
                    assert!(
                        image.width() > 640.0,
                        "display size is independent of the640px render budget"
                    );
                }
                assert_eq!(
                    (
                        panes[index].desired.unwrap().width,
                        panes[index].desired.unwrap().height
                    ),
                    (640, 360)
                );
            }
            for pane in &mut panes {
                pane.close();
            }
        }
    }
    #[test]
    fn settled_manual_frame_resumes_on_progress_change_and_keeps_pending_samples_publishable() {
        let mut session = Session::new();
        let first = session.preview();
        session
            .apply(Command::AddColorSource {
                scene: first,
                name: "Red".into(),
                rgba: [255, 0, 0, 255],
            })
            .unwrap();
        let second = session
            .apply(Command::AddScene {
                name: "Second".into(),
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::AddColorSource {
                scene: second,
                name: "Blue".into(),
                rgba: [0, 0, 255, 255],
            })
            .unwrap();
        let context = egui::Context::default();
        let mut pane = PreviewPane::new("held-manual").unwrap();
        let output = |num, den, next| FrameComposition::Manual {
            kind: skylive_core::TransitionKind::Fade,
            from: Some(FrameScene::Live(first)),
            to: Some(FrameScene::Live(second)),
            logical_scene: second,
            progress_num: num,
            progress_den: den,
            epoch: 1,
            next_after_ns: next,
        };
        let tick = |pane: &mut PreviewPane, time, frame: &FrameComposition| {
            let playback = PlaybackSnapshot::at_time(&session, MediaTime(time));
            let _ = context.run(egui::RawInput::default(), |context| {
                pane.update_output(context, &session, &playback, frame)
            });
        };
        let held = output(1, 2, None);
        let deadline = Instant::now() + Duration::from_secs(5);
        tick(&mut pane, 0, &held);
        while pane.displayed.is_none() {
            tick(&mut pane, 0, &held);
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(pane.first_pixel, Some([128, 0, 128, 255]));
        assert_eq!(pane.next_at_ns, None);
        let submitted = pane.submitted;
        tick(&mut pane, 1, &held);
        assert_eq!(pane.submitted, submitted);
        assert!(!pane.worker.is_busy());
        // A target change can request a future tick while pixels still match.
        // A changing deadline is not itself a reason to recompose this frame.
        tick(&mut pane, 2, &output(1, 2, Some(1_000_000)));
        assert!(!pane.worker.is_busy());
        assert_eq!(pane.submitted, submitted);
        let moving = output(3, 4, Some(1_000_000));
        tick(&mut pane, 1_000_002, &moving);
        assert!(
            pane.worker.is_busy(),
            "a held frame must resume even without an old deadline"
        );
        assert_eq!(pane.first_pixel, Some([128, 0, 128, 255]));
        let final_hold = output(9, 10, None);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut time = 1_000_003;
        while pane
            .displayed
            .is_none_or(|sample| sample.composition != final_hold.sample_key())
            || pane.worker.is_busy()
        {
            tick(&mut pane, time, &final_hold);
            time += 1;
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(pane.first_pixel, Some([26, 0, 230, 255]));
        assert_eq!(pane.next_at_ns, None);
        let submitted = pane.submitted;
        tick(&mut pane, u64::MAX, &final_hold);
        assert_eq!(pane.submitted, submitted);
        assert!(!pane.worker.is_busy());
        pane.close();
    }
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "skylive-playback-ui-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn gif_bytes() -> Vec<u8> {
        // Two 1x1 full-canvas frames: red then green, 100 ms each, infinite loop.
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0, 255, 0, 0, 0, 255, 0]);
        bytes.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\0\0\0");
        for data in [0x44, 0x4c] {
            bytes.extend_from_slice(&[0x21, 0xf9, 4, 0, 10, 0, 0, 0]);
            bytes.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, data, 1, 0]);
        }
        bytes.push(0x3b);
        bytes
    }
    fn tick(
        pane: &mut PreviewPane,
        context: &egui::Context,
        session: &Session,
        sample: &PlaybackSnapshot,
    ) -> egui::FullOutput {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 480.0),
                )),
                ..Default::default()
            },
            |context| {
                pane.update_playback(context, session, session.preview(), sample);
                egui::CentralPanel::default().show(context, |ui| {
                    pane.show(ui, "Preview", session, session.preview())
                });
            },
        )
    }
    fn has_text(output: &egui::FullOutput, label: &str) -> bool {
        fn has(shape: &egui::epaint::Shape, label: &str) -> bool {
            match shape {
                egui::epaint::Shape::Text(text) => text.galley.text() == label,
                egui::epaint::Shape::Vec(shapes) => shapes.iter().any(|shape| has(shape, label)),
                _ => false,
            }
        }
        output.shapes.iter().any(|shape| has(&shape.shape, label))
    }
    #[test]
    fn gif_completed_samples_publish_while_now_advances_without_flashing_between_frames() {
        let temp = Temp::new();
        let path = temp.0.join("two-frames.gif");
        std::fs::write(&path, gif_bytes()).unwrap();
        let prepared = assets::prepare_image(&path).unwrap();
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
                name: "Animated GIF".into(),
                asset: prepared.asset,
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform: Transform::full(16, 16),
            })
            .unwrap();
        let mut clock = PlaybackClock::new();
        let mut pane = PreviewPane::new("gif-test").unwrap();
        let context = egui::Context::default();
        let initial = clock.update(&session, &[scene], MediaTime(0)).unwrap();
        let _ = tick(&mut pane, &context, &session, &initial);
        assert!(pane.worker.is_busy());
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut attempt = 0u64;
        while pane.displayed.is_none() {
            // Explicit media time advances while the captured time-zero job is
            // running. Its result must still be published, not invalidated.
            let current = clock
                .update(&session, &[scene], MediaTime(50_000_000 + attempt))
                .unwrap();
            let _ = tick(&mut pane, &context, &session, &current);
            assert!(pane.error.is_none(), "{:?}", pane.error);
            assert!(
                Instant::now() < deadline,
                "initial GIF preview did not settle"
            );
            attempt += 1;
            std::thread::yield_now();
        }
        assert_eq!(pane.displayed.unwrap().time_ns, 0);
        assert_eq!(pane.first_pixel, Some([255, 0, 0, 255]));
        assert!(pane.next_at_ns.is_some());
        let texture_id = pane.texture.as_ref().unwrap().id();
        let second = clock
            .update(&session, &[scene], MediaTime(150_000_000))
            .unwrap();
        let output = tick(&mut pane, &context, &session, &second);
        assert!(pane.worker.is_busy());
        assert_eq!(pane.first_pixel, Some([255, 0, 0, 255]));
        assert_eq!(pane.texture.as_ref().unwrap().id(), texture_id);
        assert!(
            !has_text(&output, "Updating preview…"),
            "the last complete animation frame stays visible while the next is pending"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while pane
            .displayed
            .is_none_or(|sample| sample.time_ns != 150_000_000)
        {
            let _ = tick(&mut pane, &context, &session, &second);
            assert!(pane.error.is_none(), "{:?}", pane.error);
            assert!(
                Instant::now() < deadline,
                "second GIF preview did not settle"
            );
            std::thread::yield_now();
        }
        assert_eq!(pane.first_pixel, Some([0, 255, 0, 255]));
        assert_eq!(pane.texture.as_ref().unwrap().id(), texture_id);
        pane.close();
    }
    #[test]
    fn static_preview_has_no_animation_deadline_or_repeated_admission() {
        let mut session = Session::new();
        let scene = session.preview();
        session
            .apply(Command::AddColorSource {
                scene,
                name: "Static".into(),
                rgba: [10, 20, 30, 255],
            })
            .unwrap();
        let context = egui::Context::default();
        let mut pane = PreviewPane::new("static-test").unwrap();
        let mut clock = PlaybackClock::new();
        let initial = clock.update(&session, &[scene], MediaTime(0)).unwrap();
        let _ = tick(&mut pane, &context, &session, &initial);
        let deadline = Instant::now() + Duration::from_secs(5);
        while pane.displayed.is_none() {
            let _ = tick(&mut pane, &context, &session, &initial);
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(pane.next_at_ns, None);
        assert!(!pane.worker.is_busy());
        let later = clock
            .update(&session, &[scene], MediaTime(1_000_000_000))
            .unwrap();
        let _ = tick(&mut pane, &context, &session, &later);
        assert_eq!(pane.submitted.unwrap().time_ns, 0);
        assert!(!pane.worker.is_busy());
        assert_eq!(pane.next_at_ns, None);
        pane.close();
    }
}
