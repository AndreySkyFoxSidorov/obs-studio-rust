//! One prepared visual sample, owned/destroyed only by the recording worker.
use super::{RecordingError, owner::RecordingFrame};
use crate::{
    playback::PlaybackSnapshot,
    preview::{OutputRenderer, SourceDiagnostic},
    transition::{CompositionKey, OutputKey},
};
use skylive_core::StudioSnapshot;
use skylive_media::{MediaTime, RgbaFrame, recording_nv12};
use std::{
    mem::size_of,
    sync::{Arc, Weak},
};

pub const MAX_PREPARED_VIDEO_BYTES: usize = 50 * 1024 * 1024;

/// Checked shared SDR representation. Native samples still own distinct buffers.
pub struct PreparedVideo {
    image: RgbaFrame,
    nv12: Box<[u8]>,
    // Proven camera contributors to this exact rendered sample. Cached
    // prepared frames retain the same trace as their pixel data.
    camera_trace: Vec<skylive_core::Id>,
    auto_text:Vec<crate::text::auto_worker::Snapshot>,
}
impl PreparedVideo {
    fn new(image: RgbaFrame, limit: usize) -> Result<Self, RecordingError> {
        let pixels = (image.width() as usize)
            .checked_mul(image.height() as usize)
            .ok_or_else(|| RecordingError::invalid("prepared video byte size overflow"))?;
        let converted = pixels
            .checked_add(pixels / 2)
            .ok_or_else(|| RecordingError::invalid("prepared NV12 byte size overflow"))?;
        if image
            .byte_len()
            .checked_add(converted)
            .is_none_or(|bytes| bytes > limit.min(MAX_PREPARED_VIDEO_BYTES))
        {
            return Err(RecordingError::invalid(
                "prepared recording frame exceeds50MiB admission",
            ));
        }
        let nv12 = recording_nv12(&image, 32 * 1024 * 1024, 18_000_000)
            .map_err(|error| RecordingError::native("Program NV12 preparation", error))?;
        Ok(Self {
            image,
            nv12: nv12.into_boxed_slice(),
            camera_trace: Vec::new(),
            auto_text:Vec::new(),
        })
    }
    pub fn image(&self) -> &RgbaFrame {
        &self.image
    }
    pub fn nv12(&self) -> &[u8] {
        &self.nv12
    }
    fn byte_len(&self) -> usize {
        self.image.byte_len() + self.nv12.len() + self.camera_trace.capacity()*size_of::<skylive_core::Id>()
            + self.auto_text.capacity()*size_of::<crate::text::auto_worker::Snapshot>()
    }
    pub(super) fn camera_trace(&self) -> &[skylive_core::Id] { &self.camera_trace }
    pub(super) fn auto_text_current(&self)->bool{self.auto_text.iter().all(|snapshot|snapshot.is_live())}
}

struct Key {
    model: Weak<StudioSnapshot>,
    capture: Option<Weak<crate::capture::CaptureSnapshot>>,
    session: u64,
    revision: u64,
    output: OutputKey,
    composition: CompositionKey,
    dimensions: (u32, u32),
    playback: PlaybackSnapshot,
    capture_epoch: u64,
    capture_generation: u64,
    resource_generation: u64,
}
impl Key {
    fn new(frame: &RecordingFrame, dimensions: (u32, u32)) -> Self {
        Self {
            model: Arc::downgrade(&frame.model),
            capture: (!frame.capture.is_semantically_empty())
                .then(|| Arc::downgrade(&frame.capture)),
            session: frame.session,
            revision: frame.model.revision(),
            output: frame.output.identity(),
            composition: frame.output.sample_key(),
            dimensions,
            playback: frame.playback.clone(),
            capture_epoch: frame.capture.epoch(),
            capture_generation: frame.capture.generation(),
            resource_generation: frame.resource_generation,
        }
    }
    fn matches(&self, frame: &RecordingFrame, dimensions: (u32, u32)) -> bool {
        self.model.as_ptr() == Arc::as_ptr(&frame.model)
            && match &self.capture {
                Some(identity) => {
                    !frame.capture.is_semantically_empty()
                        && identity.as_ptr() == Arc::as_ptr(&frame.capture)
                }
                None => frame.capture.is_semantically_empty(),
            }
            && self.session == frame.session
            && self.revision == frame.model.revision()
            && self.output == frame.output.identity()
            && self.composition == frame.output.sample_key()
            && self.dimensions == dimensions
            && self.capture_epoch == frame.capture.epoch()
            && self.capture_generation == frame.capture.generation()
            && self.resource_generation == frame.resource_generation
            && frame.playback.advances_from(&self.playback)
    }
    fn retained_bytes(&self) -> usize {
        // Weak reserves this fixed allocation, not its dropped Vec/String graph.
        size_of::<Self>()
            + size_of::<StudioSnapshot>()
            + 2 * size_of::<usize>()
            + self.playback.retained_clock_bytes()
            + if self.capture.is_some() {
                size_of::<crate::capture::CaptureSnapshot>() + 2 * size_of::<usize>()
            } else {
                0
            }
    }
}
struct Entry {
    key: Key,
    prepared: PreparedVideo,
    deadline: Option<MediaTime>,
}
pub(super) enum Video<'a> {
    Cached(&'a PreparedVideo),
    Uncached(PreparedVideo),
}
impl Video<'_> {
    pub(super) fn get(&self) -> &PreparedVideo {
        match self {
            Self::Cached(value) => value,
            Self::Uncached(value) => value,
        }
    }
}
pub(super) struct ResultFrame<'a> {
    pub video: Video<'a>,
    pub diagnostics: Vec<SourceDiagnostic>,
    pub hit: bool,
}

pub(super) struct FrameCache {
    entry: Option<Entry>,
    limit: usize,
    #[cfg(test)]
    enabled: bool,
}
impl Default for FrameCache {
    fn default() -> Self {
        Self {
            entry: None,
            limit: MAX_PREPARED_VIDEO_BYTES,
            #[cfg(test)]
            enabled: true,
        }
    }
}
impl FrameCache {
    pub(super) fn clear(&mut self) {
        self.entry = None;
    }
    pub(super) fn frame<'a>(
        &'a mut self,
        frame: &RecordingFrame,
        dimensions: (u32, u32),
        renderer: &mut OutputRenderer,
    ) -> Result<ResultFrame<'a>, RecordingError> {
        if frame.playback.session() != frame.session {
            self.clear();
            return Err(RecordingError::invalid(
                "prepared frame playback belongs to another session",
            ));
        }
        if let Err(error) = OutputRenderer::validate_profile_output_inputs(
            &frame.model,
            &frame.output,
            &frame.playback,
            &frame.capture,
        ) {
            self.clear();
            return Err(RecordingError::native("recording Program render", error));
        }
        let resolved=frame.model.output_video().map_err(|error|RecordingError::native("profile output format",error))?;
        if dimensions!=(resolved.width(),resolved.height()){self.clear();return Err(RecordingError::invalid("recording output dimensions no longer match the profile"));}
        #[cfg(test)]
        let enabled = self.enabled;
        #[cfg(not(test))]
        let enabled = true;
        let hit = enabled
            && self.entry.as_ref().is_some_and(|entry| {
                entry.prepared.auto_text.iter().all(|snapshot|snapshot.is_live()) && entry.key.matches(frame, dimensions)
                    && entry
                        .deadline
                        .is_none_or(|until| frame.playback.now() < until)
            });
        if hit {
            if let Some(entry) = self.entry.as_ref() {
                return Ok(ResultFrame {
                    video: Video::Cached(&entry.prepared),
                    diagnostics: vec![],
                    hit: true,
                });
            }
            return Err(RecordingError::invalid(
                "prepared frame cache entry disappeared",
            ));
        }
        // No old RGBA/NV12 overlaps the next candidate render/conversion.
        self.clear();
        let key = Key::new(frame, dimensions);
        let metadata = key
            .retained_bytes()
            .checked_add(size_of::<Entry>())
            .and_then(|bytes| bytes.checked_add(size_of::<PreparedVideo>()))
            .ok_or_else(|| RecordingError::invalid("prepared video metadata overflow"))?;
        let pixels=dimensions.0 as usize*dimensions.1 as usize;
        let rgba_and_nv12=pixels.checked_mul(4).and_then(|v|v.checked_add(pixels+pixels/2)).ok_or_else(||RecordingError::invalid("prepared video allocation overflow"))?;
        if metadata>self.limit || rgba_and_nv12>self.limit.saturating_sub(metadata) {
            return Err(RecordingError::invalid(
                "prepared recording RGBA/NV12/metadata exceeds50MiB admission",
            ));
        }
        let rendered = renderer
            .render_recording_profile_output(
                &frame.model,
                &frame.output,
                &frame.playback,
                &frame.capture,
            )
            .map_err(|error|if matches!(&error,crate::preview::PreviewError::AutoTextRetry){RecordingError::auto_text_retry()}else{RecordingError::native("recording Program render", error)})?;
        if !rendered.auto_text_current(){return Err(RecordingError::auto_text_retry());}
        let guards=rendered.auto_text_guards();
        let deadline = rendered
            .next_after_ns
            .map(|after| frame.playback.now().0.checked_add(after).map(MediaTime));
        let eligible = enabled && rendered.diagnostics.is_empty() && deadline != Some(None);
        let mut prepared = PreparedVideo::new(rendered.image, self.limit - metadata)?;
        prepared.auto_text=guards;
        if prepared.auto_text.iter().any(|snapshot|!snapshot.is_live()){return Err(RecordingError::auto_text_retry());}
        prepared.camera_trace = rendered.camera_trace;
        if metadata.checked_add(prepared.byte_len()).is_none_or(|bytes| bytes>self.limit) {
            return Err(RecordingError::invalid("prepared recording frame trace exceeds byte admission"));
        }
        debug_assert!(metadata + prepared.byte_len() <= self.limit);
        if eligible {
            self.entry = Some(Entry {
                key,
                prepared,
                deadline: deadline.flatten(),
            });
            if let Some(entry) = self.entry.as_ref() {
                Ok(ResultFrame {
                    video: Video::Cached(&entry.prepared),
                    diagnostics: vec![],
                    hit: false,
                })
            } else {
                Err(RecordingError::invalid(
                    "prepared frame cache publication failed",
                ))
            }
        } else {
            Ok(ResultFrame {
                video: Video::Uncached(prepared),
                diagnostics: rendered.diagnostics,
                hit: false,
            })
        }
    }
}

#[cfg(test)]
#[path = "prepared_video_tests.rs"]
mod tests;
