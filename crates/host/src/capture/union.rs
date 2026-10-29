//! Frontend-owned identity for the bounded immutable Display/Window/Camera
//! union. It owns metadata only; each acquisition worker retains pixel payloads.
use super::*;
#[derive(Default)]
pub struct CaptureUnion {
    session: u64,
    scope: Option<(u64, u64, u64,u64)>,
    activation:Arc<crate::camera_activation::Snapshot>,
    samples: Option<(u64, u64)>,
    epoch: u64,
    generation: u64,
    latest: Option<Arc<CaptureSnapshot>>,
}
impl CaptureUnion {
    pub fn set_camera_activation(&mut self,policy:Arc<crate::camera_activation::Snapshot>){self.activation=policy;}
    pub fn clear(&mut self) {
        self.latest = None;
        self.scope = None;
        self.samples = None;
        self.session = 0;
    }
    pub fn snapshot(&self, session: u64) -> Arc<CaptureSnapshot> {
        self.latest
            .as_ref()
            .filter(|s| s.session() == session)
            .cloned()
            .unwrap_or_else(|| Arc::new(CaptureSnapshot::empty(session)))
    }
    pub fn merge(
        &mut self,
        session: &Session,
        base: Arc<CaptureSnapshot>,
        camera: &crate::camera_capture::CameraOwner,
    ) -> Result<Arc<CaptureSnapshot>, CaptureError> {
        if session.is_closed() {
            return Err(CaptureError::Closed);
        }
        let camera_snapshot = camera.snapshot();
        if base.session() != session.identity()
            || camera_snapshot.session() != session.identity()
            || camera_snapshot.instance() != camera.instance()
        {
            return Err(CaptureError::ForeignSession);
        }
        let scope = (
            base.epoch(),
            camera_snapshot.instance(),
            camera_snapshot.epoch(),
            if self.activation.session()==session.identity(){self.activation.serial()}else{0},
        );
        let samples = (base.generation(), camera_snapshot.generation());
        if self.session == session.identity()
            && self.scope == Some(scope)
            && self.samples == Some(samples)
        {
            if let Some(latest) = &self.latest {
                return Ok(latest.clone());
            }
        }
        let epoch = if self.session != session.identity() || self.scope != Some(scope) {
            self.epoch
                .checked_add(1)
                .ok_or(CaptureError::IdentityExhausted)?
        } else {
            self.epoch
        };
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(CaptureError::IdentityExhausted)?;
        if base
            .frame_count()
            .checked_add(camera_snapshot.frames().count())
            .is_none_or(|n| n > MAX_CAPTURE_REFERENCES)
        {
            return Err(CaptureError::BudgetExceeded(
                "combined live capture source references",
            ));
        }
        let mut frames = base.frames.clone();
        let mut problems = base.problems.clone();
        let mut seen = std::collections::HashSet::new();
        let mut bytes = 0usize;
        for frame in frames.values() {
            if seen.insert(frame.image.pixels().as_ptr() as usize) {
                bytes = bytes
                    .checked_add(frame.image.byte_len())
                    .ok_or(CaptureError::BudgetExceeded("combined capture bytes"))?;
            }
            if let Some(raw)=&frame.draft_raw {
                if seen.insert(raw.image.pixels().as_ptr() as usize) {
                    bytes=bytes.checked_add(raw.image.byte_len()).ok_or(CaptureError::BudgetExceeded("draft camera frame bytes"))?;
                }
            }
        }
        for (source, frame) in camera_snapshot.frames() {
            if self.activation.session()==session.identity()&&!self.activation.publish_video(source){continue;}
            if frames.contains_key(&source) || base.media_empty.contains_key(&source) {
                return Err(CaptureError::InvalidFrame(
                    "source has multiple acquisition owners",
                ));
            }
            if seen.insert(frame.image.pixels().as_ptr() as usize) {
                bytes = bytes
                    .checked_add(frame.image.byte_len())
                    .ok_or(CaptureError::BudgetExceeded("combined capture bytes"))?;
            }
            if bytes > CaptureLimits::default().max_total_frame_bytes {
                return Err(CaptureError::BudgetExceeded(
                    "combined Display/Window/Camera frame bytes",
                ));
            }
            // The modal preview may borrow a separate unprocessed sample.
            // Admit it under the same union budget; omit it on pressure rather
            // than refusing the live Program frame.
            let draft_raw=frame.draft_raw.as_ref().and_then(|raw| {
                let pointer=raw.image.pixels().as_ptr() as usize;
                let extra=if seen.contains(&pointer){0}else{raw.image.byte_len()};
                let next=bytes.checked_add(extra)?;
                if next>CaptureLimits::default().max_total_frame_bytes{return None;}
                bytes=next;seen.insert(pointer);
                Some(Arc::new(CapturedFrame {native_origin:Some(NativeFrameOrigin::Camera{owner:camera_snapshot.instance(),stream:raw.stream_generation}),
                    binding:CaptureBinding::Video(raw.binding.clone()),sequence:raw.sequence,requested_at:raw.requested_at,
                    native_timestamp_ns:Some(raw.presentation_ns),native_clock_epoch:Some(raw.clock_epoch),device_qpc_ns:raw.device_qpc_ns,
                    directshow_clock:raw.directshow_clock,async_delay:None,draft_raw:None,image:raw.image.clone(),display:None,window:None}))
            });
            frames.insert(
                source,
                CapturedFrame {native_origin:Some(NativeFrameOrigin::Camera{owner:camera_snapshot.instance(),stream:frame.stream_generation}),
                    binding: CaptureBinding::Video(frame.binding.clone()),
                    sequence: frame.sequence,
                    requested_at: frame.requested_at,
                    native_timestamp_ns: Some(frame.presentation_ns),
                    native_clock_epoch: Some(frame.clock_epoch),
                    device_qpc_ns: frame.device_qpc_ns,
                    directshow_clock: frame.directshow_clock,
                    async_delay: frame.async_delay.clone().map(|mut receipt| {
                        receipt.live_generation = Some(camera.delay_epoch_token());
                        receipt
                    }),
                    draft_raw,
                    image: frame.image.clone(),
                    display: None,
                    window: None,
                },
            );
        }
        for problem in camera_snapshot.problems() {
            if self.activation.session()==session.identity()&&!self.activation.enabled(problem.source){continue;}
            problems.push(CaptureProblem {
                source: problem.source,
                kind: problem.kind,
                code: problem.code,
                detail: problem.detail.to_string(),
            });
        }
        if problems.len() > MAX_CAPTURE_REFERENCES {
            return Err(CaptureError::BudgetExceeded("combined capture diagnostics"));
        }
        let mut candidates=base.draft_frames.clone();
        for (source,raw) in camera_snapshot.draft_frames() {
            if self.activation.session()==session.identity()&&!self.activation.publish_video(source){continue;}
            candidates.insert(source,DraftCapture {epoch:camera_snapshot.epoch(),live:camera.delay_epoch_token(),stream:None,
                frame:CapturedFrame {native_origin:Some(NativeFrameOrigin::Camera{owner:camera_snapshot.instance(),stream:raw.stream_generation}),
                    binding:CaptureBinding::Video(raw.binding.clone()),sequence:raw.sequence,requested_at:raw.requested_at,
                    native_timestamp_ns:Some(raw.presentation_ns),native_clock_epoch:Some(raw.clock_epoch),device_qpc_ns:raw.device_qpc_ns,
                    directshow_clock:raw.directshow_clock,async_delay:None,draft_raw:None,image:raw.image.clone(),display:None,window:None}});
        }
        let draft_frames=admit_draft_frames(&candidates,&frames,&mut seen,&mut bytes);
        let latest = Arc::new(CaptureSnapshot {
            media_empty: base.media_empty.clone(),
            session: session.identity(),
            epoch,
            generation,
            frames,
            draft_frames,
            problems,
        });
        self.session = session.identity();
        self.scope = Some(scope);
        self.samples = Some(samples);
        self.epoch = epoch;
        self.generation = generation;
        self.latest = Some(latest.clone());
        Ok(latest)
    }
}
