use super::*;
use skylive_host::media_source::{MediaEvent, MediaSourceError};
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Scope {
    session: u64,
    revision: u64,
    output: skylive_host::transition::OutputKey,
    video:[Option<skylive_host::transition::SceneKey>;40],
    program: Option<skylive_host::audio_capture::ProgramCaptureKey>,
}
impl StudioApp {
    pub(super) fn update_media(
        &mut self,
        context: &egui::Context,
        output: Option<&skylive_host::transition::FrameComposition>,
        video_active:&[FrameScene],
        now: MediaTime,
        base: Arc<CaptureSnapshot>,
    ) -> Arc<CaptureSnapshot> {
        if self.media.set_camera_activation_policy(self.camera_activation.snapshot()){self.media_scope=None;}
        let rewound = self
            .media_frame_at
            .replace(now)
            .is_some_and(|previous| now < previous);
        if rewound {
            if let Err(error) = self.media.reset_timing(&self.session) {
                self.message(true, error.to_string());
            }
            self.media_scope = None;
        }
        for _ in 0..4 {
            match self.media.poll(&self.session) {
                Ok(Some(MediaEvent::Prepared { key, result })) => {
                    self.accept_prepared_media(key, result)
                }
                Ok(Some(MediaEvent::Controlled {
                    key,
                    source,
                    result,
                })) => {if !self.accept_missing_media_reload(key,source,&result)&&!self.complete_global_media_control(key,source,&result){self.accept_media_control(key,source,result,context);}},
                Ok(Some(MediaEvent::Updated { .. })) => {}
                Ok(None) => break,
                Err(error) => {
                    self.message(true, error.to_string());
                    break;
                }
            }
        }
        self.drive_missing_media_reloads(context);
        self.media_error = self
            .media
            .snapshot()
            .problems()
            .first()
            .map(|problem| format!("{}: {}", problem.code, problem.detail));
        {
            if let Some(output) = output {
                let key = Scope {
                    session: self.session.identity(),
                    revision: self.session.snapshot().revision(),
                    output: output.identity(),
                    video:std::array::from_fn(|index|video_active.get(index).map(FrameScene::key)),
                    program: self.audio.program_key(),
                };
                let drift = self.media.snapshot().source_count() != 0
                    && self
                        .media
                        .media_now()
                        .ok()
                        .flatten()
                        .is_some_and(|expected| expected.0.abs_diff(now.0) > 50_000_000);
                if self.media_scope != Some(key) || drift {
                    match self.media.synchronize_video(&self.session, output, video_active, now, self.audio.program_key()) {
                        Ok(true)=>self.media_scope=Some(key),
                        Ok(false)=>{},
                        Err(MediaSourceError::Busy)=>{},
                        Err(error)=>self.message(true,error.to_string()),
                    }
                }
            } else if self.media.is_busy() {
                self.media.invalidate_pending_control();
            }
        }
        if self.media.is_busy() || self.pending_media.is_some() {
            context.request_repaint_after(std::time::Duration::from_millis(10));
        }
        match self
            .media_union
            .merge(&self.session, base.clone(), &self.media)
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.message(true, error.to_string());
                base
            }
        }
    }
}
