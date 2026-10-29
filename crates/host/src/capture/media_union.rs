//! Metadata-only union. The file owner retains every published pixel payload.
use super::*;
#[derive(Default)]
pub struct MediaCaptureUnion {
    identity: Option<(u64, u64, u64, u64, u64, u64)>,
    epoch: u64,
    generation: u64,
    latest: Option<Arc<CaptureSnapshot>>,
}
impl MediaCaptureUnion {
    pub fn clear(&mut self) {
        self.identity = None;
        self.latest = None;
    }
    pub fn merge(
        &mut self,
        session: &Session,
        base: Arc<CaptureSnapshot>,
        media: &crate::media_source::MediaOwner,
    ) -> Result<Arc<CaptureSnapshot>, CaptureError> {
        let input = media.snapshot();
        if session.is_closed() {
            return Err(CaptureError::Closed);
        }
        if base.session() != session.identity() {
            return Err(CaptureError::ForeignSession);
        }
        // An owner that has not admitted a scope contributes no frame to this session.
        if input.session() != session.identity() {
            return Ok(base);
        }
        if input.source_count() == 0 && input.problems().is_empty() && self.identity.is_none() {
            return Ok(base);
        }
        let identity = (
            session.identity(),
            base.epoch(),
            base.generation(),
            input.instance(),
            input.epoch(),
            input.generation(),
        );
        if self.identity == Some(identity) {
            if let Some(latest) = &self.latest {
                return Ok(latest.clone());
            }
        }
        let scope_changed = self.identity.is_none_or(|old| {
            (old.0, old.1, old.3, old.4) != (identity.0, identity.1, identity.3, identity.4)
        });
        let epoch = if scope_changed {
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
        let mut frames = base.frames.clone();
        let mut media_empty = base.media_empty.clone();
        let mut problems = base.problems.clone();
        if input
            .frames()
            .count()
            .checked_add(input.empty_sources().count())
            .is_none_or(|n| n > 8)
        {
            return Err(CaptureError::BudgetExceeded(
                "media ready/empty reference profile",
            ));
        }
        let mut metadata_bytes = 0usize;
        if input.problems().len() > 9 {
            return Err(CaptureError::BudgetExceeded("media diagnostic count"));
        }
        for problem in input.problems() {
            if problem.detail.len() > 8192 {
                return Err(CaptureError::BudgetExceeded("media diagnostic text"));
            }
            metadata_bytes = metadata_bytes
                .checked_add(
                    problem.detail.len() + std::mem::size_of::<crate::media_source::MediaProblem>(),
                )
                .ok_or(CaptureError::BudgetExceeded("media diagnostic bytes"))?;
        }
        for (_, frame) in input.frames() {
            metadata_bytes = metadata_bytes
                .checked_add(
                    frame.media.path().len()
                        + std::mem::size_of::<skylive_core::MediaSource>()
                        + 64,
                )
                .ok_or(CaptureError::BudgetExceeded("media metadata bytes"))?;
        }
        for (source, media) in input.empty_sources() {
            if frames.contains_key(&source)
                || media_empty.contains_key(&source)
                || input
                    .problems()
                    .iter()
                    .any(|problem| problem.source == source)
            {
                return Err(CaptureError::InvalidFrame(
                    "media source cannot be ready/empty/failed simultaneously",
                ));
            }
            metadata_bytes = metadata_bytes
                .checked_add(
                    media.path().len() + std::mem::size_of::<skylive_core::MediaSource>() + 64,
                )
                .ok_or(CaptureError::BudgetExceeded("media metadata bytes"))?;
            media_empty.insert(source, media.clone());
        }
        if metadata_bytes > 256 * 1024 {
            return Err(CaptureError::BudgetExceeded("media metadata256KiB"));
        }
        if frames
            .len()
            .checked_add(input.frames().count())
            .and_then(|n| n.checked_add(media_empty.len()))
            .is_none_or(|n| n > MAX_CAPTURE_REFERENCES)
        {
            return Err(CaptureError::BudgetExceeded(
                "combined media source references",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        let mut bytes = 0usize;
        for frame in frames.values() {
            if seen.insert(frame.image.pixels().as_ptr() as usize) {
                bytes = bytes
                    .checked_add(frame.image.byte_len())
                    .ok_or(CaptureError::BudgetExceeded("combined media frame bytes"))?;
            }
            if let Some(raw)=&frame.draft_raw {
                if seen.insert(raw.image.pixels().as_ptr() as usize) {
                    bytes=bytes.checked_add(raw.image.byte_len()).ok_or(CaptureError::BudgetExceeded("draft camera frame bytes"))?;
                }
            }
        }
        for (source, frame) in input.frames() {
            if frames.contains_key(&source) || media_empty.contains_key(&source) {
                return Err(CaptureError::InvalidFrame(
                    "source has multiple acquisition owners",
                ));
            }
            if seen.insert(frame.image.pixels().as_ptr() as usize) {
                bytes = bytes
                    .checked_add(frame.image.byte_len())
                    .ok_or(CaptureError::BudgetExceeded("combined media frame bytes"))?;
            }
            if bytes > CaptureLimits::default().max_total_frame_bytes {
                return Err(CaptureError::BudgetExceeded(
                    "combined native and media frame bytes",
                ));
            }
            frames.insert(
                source,
                CapturedFrame {native_origin:Some(NativeFrameOrigin::Media{owner:input.instance(),stream:frame.stream_generation}),
                    binding: CaptureBinding::Media(frame.media.clone()),
                    sequence: frame.sequence,
                    requested_at: frame.requested_at,
                    native_timestamp_ns: None,
                    native_clock_epoch: None,
                    device_qpc_ns: None, directshow_clock: None,
                    async_delay: None,draft_raw:None,
                    image: frame.image.clone().into(),
                    display: None,
                    window: None,
                },
            );
        }
        for problem in input.problems() {
            problems.push(CaptureProblem {
                source: problem.source,
                kind: problem.kind,
                code: problem.code,
                detail: problem.detail.clone(),
            });
        }
        if problems.len() > MAX_CAPTURE_REFERENCES {
            return Err(CaptureError::BudgetExceeded("combined media diagnostics"));
        }
        let draft_frames=admit_draft_frames(&base.draft_frames,&frames,&mut seen,&mut bytes);
        let latest = Arc::new(CaptureSnapshot {
            media_empty,
            session: session.identity(),
            epoch,
            generation,
            frames,
            draft_frames,
            problems,
        });
        self.identity = Some(identity);
        self.epoch = epoch;
        self.generation = generation;
        self.latest = Some(latest.clone());
        Ok(latest)
    }
}
