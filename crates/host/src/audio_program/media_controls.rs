//! Private AudioProgram control history. Copy to audio_program/media_controls.rs.
use super::*;
use crate::media_source::control::{MediaBarrier, MediaBarrierReceipt};
const MAX_CONTROL_KEYS: usize = 64;
const MAX_CONTROL_INTERVALS: usize = 64;
struct Silence {
    start: u64,
    end: Option<u64>,
}
struct Entry {
    key: AudioInputKey,
    silence: Vec<Silence>,
    reset: Option<(u64, u64)>, // new source generation, first admissible project frame
    last_frame: u64,
}
pub(super) struct MediaControls {
    entries: Vec<Entry>,
}
impl MediaControls {
    pub(super) fn new() -> Result<Self, ProgramAudioError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(MAX_CONTROL_KEYS)
            .map_err(|_| ProgramAudioError::Limit("media control entries"))?;
        Ok(Self { entries })
    }
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
    pub(super) fn bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<Entry>()
            + self
                .entries
                .iter()
                .map(|e| {
                    e.silence.capacity() * std::mem::size_of::<Silence>()
                        + match &e.key {
                            AudioInputKey::Media { media, .. } => media.path().len(),
                            AudioInputKey::Ndi {ndi,..}=>ndi.connection().sender.name.len()+ndi.connection().sender.url.as_ref().map_or(0,String::len),
                            AudioInputKey::Browser(key)=>key.browser.css().len()+match key.browser.location(){skylive_core::BrowserLocation::Url{url}=>url.len(),skylive_core::BrowserLocation::LocalFile{path}=>path.len()},
                            AudioInputKey::EmbeddedCamera {binding,..}=>match binding.target(){skylive_core::VideoCaptureTarget::Windows{symbolic_link}=>symbolic_link.len(),skylive_core::VideoCaptureTarget::Macos{unique_id}=>unique_id.len()},
                            _ => 0,
                        }
                })
                .sum::<usize>()
    }
    fn entry(&self, key: &AudioInputKey) -> Option<&Entry> {
        self.entries.iter().find(|e| &e.key == key)
    }
    pub(super) fn has(&self, key: &AudioInputKey) -> bool {
        self.entry(key).is_some()
    }
    pub(super) fn reset_frame(&self, key: &AudioInputKey, generation: u64) -> Option<u64> {
        self.entry(key)?
            .reset
            .filter(|(g, _)| *g == generation)
            .map(|(_, f)| f)
    }
    pub(super) fn accepted(&mut self, key: &AudioInputKey, generation: u64) {
        if let Some(e) = self.entries.iter_mut().find(|e| &e.key == key) {
            if e.reset.is_some_and(|(g, _)| g == generation) {
                e.reset = None;
            }
        }
    }
    pub(super) fn suspended(&self, key: &AudioInputKey) -> bool {
        self.entry(key)
            .is_some_and(|e| e.silence.last().is_some_and(|s| s.end.is_none()))
    }
    fn silent(e: &Entry, frame: i128) -> bool {
        frame >= 0
            && e.silence.iter().any(|s| {
                frame >= i128::from(s.start) && s.end.is_none_or(|end| frame < i128::from(end))
            })
    }
    pub(super) fn covers(
        &self,
        key: &AudioInputKey,
        ring: Option<&SampleRing>,
        start: i128,
        frames: usize,
    ) -> bool {
        let Some(e) = self.entry(key) else {
            return false;
        };
        (0..frames).all(|offset| {
            let frame = start + offset as i128;
            Self::silent(e, frame) || ring.is_some_and(|r| r.covers(frame))
        })
    }
    /// Override known intentional silence and remove only its missing-frame count.
    #[allow(clippy::too_many_arguments)] // Explicit ring and interleaved span metadata from the validated mixer.
    pub(super) fn apply(
        &self,
        key: &AudioInputKey,
        ring: Option<&SampleRing>,
        start: i128,
        frames: usize,
        channels: usize,
        data: &mut [f32],
        mut missing: usize,
    ) -> usize {
        let Some(e) = self.entry(key) else {
            return missing;
        };
        let mut index = e
            .silence
            .partition_point(|s| s.end.is_some_and(|end| i128::from(end) <= start));
        for offset in 0..frames {
            let frame = start + offset as i128;
            while index < e.silence.len()
                && e.silence[index]
                    .end
                    .is_some_and(|end| i128::from(end) <= frame)
            {
                index += 1;
            }
            if let Some(s) = e
                .silence
                .get(index)
                .filter(|s| frame >= i128::from(s.start))
            {
                debug_assert!(s.end.is_none_or(|end| frame < i128::from(end)));
                data[offset * channels..(offset + 1) * channels].fill(0.0);
                if !ring.is_some_and(|r| r.covers(frame)) {
                    missing -= 1;
                }
            }
        }
        missing
    }
    pub(super) fn retire_with_sync(&mut self, frame: u64, required: &HashSet<&AudioInputKey>, sync: &sync_offset::SourceSync) {
        for e in &mut self.entries {
            let retained = if sync.absolute(&e.key) {
                u64::try_from(sync.retention_floor(&e.key, i128::from(frame)).max(0)).unwrap_or(u64::MAX)
            } else { frame };
            e.silence.retain(|s| s.end.is_none_or(|end| end > retained));
        }
        self.entries
            .retain(|e| required.contains(&e.key) && (!e.silence.is_empty() || e.reset.is_some()));
    }
}
impl AudioProgram {
    /// Validate all identities/capacity before publishing the source-only barrier.
    pub(crate) fn control_media(
        &mut self,
        identity: ProgramInputIdentity,
        b: &MediaBarrier,
    ) -> Result<MediaBarrierReceipt, ProgramAudioError> {
        if identity != self.identity()?
            || b.next_generation == 0
            || b.next_generation <= b.generation
        {
            return Err(ProgramAudioError::Invalid("stale media control identity"));
        }
        let key = AudioInputKey::Media {
            source: b.source,
            media: (*b.media).clone(),
        };
        if !self.required_bindings().iter().any(|r| r.binding == key) {
            return Err(ProgramAudioError::Invalid(
                "media control input is not retained by Program",
            ));
        }
        match self.fences.get(&key) {
            Some(f)
                if f.cursor.generation > b.generation
                    || f.cursor.generation == b.generation
                        && f.cursor.sequence > b.last_sequence =>
            {
                return Err(ProgramAudioError::Invalid(
                    "stale media control packet cursor",
                ));
            }
            _ => {}
        }
        if !self.fences.contains_key(&key) && self.fences.len() >= 128 {
            return Err(ProgramAudioError::Limit(
                "retained input identity fences; reset the Program epoch",
            ));
        }
        let requested = u64::try_from(
            (u128::from(b.effective_ns) * u128::from(self.settings.sample_rate()))
                .div_ceil(1_000_000_000),
        )
        .map_err(|_| ProgramAudioError::TimeOverflow)?;
        let frame = requested.max(self.cursor.unwrap_or(0)).max(self.histories.binding_control_floor(&key));
        let effective_ns = u64::try_from(
            u128::from(frame) * 1_000_000_000 / u128::from(self.settings.sample_rate()),
        )
        .map_err(|_| ProgramAudioError::TimeOverflow)?;
        let issued = next(self.data_generation)?;
        let index = self
            .media_controls
            .entries
            .iter()
            .position(|e| e.key == key);
        let total = self
            .media_controls
            .entries
            .iter()
            .map(|e| e.silence.len())
            .sum::<usize>();
        let opens = b.suspend
            && index.is_none_or(|i| {
                self.media_controls.entries[i]
                    .silence
                    .last()
                    .is_none_or(|s| s.end.is_some())
            });
        if opens && total >= MAX_CONTROL_INTERVALS {
            return Err(ProgramAudioError::Backpressure(
                "unemitted media silence intervals",
            ));
        }
        if index.is_some_and(|i| self.media_controls.entries[i].last_frame > frame) {
            return Err(ProgramAudioError::ClockRewind);
        }
        let prepared = if index.is_none() {
            if self.media_controls.entries.len() >= MAX_CONTROL_KEYS {
                return Err(ProgramAudioError::Limit(
                    "retained media control identities",
                ));
            }
            let extra =
                b.media.path().len() + MAX_CONTROL_INTERVALS * std::mem::size_of::<Silence>();
            if self
                .bytes()
                .checked_add(extra)
                .is_none_or(|v| v > self.limits.max_bytes)
            {
                return Err(ProgramAudioError::Backpressure("media control metadata"));
            }
            let mut silence = Vec::new();
            silence
                .try_reserve_exact(MAX_CONTROL_INTERVALS)
                .map_err(|_| ProgramAudioError::Limit("media control intervals"))?;
            Some(Entry {
                key: key.clone(),
                silence,
                reset: None,
                last_frame: frame,
            })
        } else {
            None
        };
        // HashMap insertion capacity is prepared before ring/fence mutation.
        self.fences
            .try_reserve(usize::from(!self.fences.contains_key(&key)))
            .map_err(|_| ProgramAudioError::Limit("media control ordering fence"))?;
        self.histories.validate_binding_control(&key)?;
        let index = match prepared {
            Some(entry) => {
                self.media_controls.entries.push(entry);
                self.media_controls.entries.len() - 1
            }
            None => index.unwrap(),
        };
        let e = &mut self.media_controls.entries[index];
        if b.suspend {
            if opens {
                e.silence.push(Silence {
                    start: frame,
                    end: None,
                });
            }
        } else if let Some(s) = e.silence.last_mut().filter(|s| s.end.is_none()) {
            s.end = Some(frame);
        }
        e.last_frame = frame;
        e.reset = Some((b.next_generation, frame));
        self.histories.control_binding(&key,frame);
        if let Some(group) = self.groups.get_mut(&key) {
            group.ring.truncate_after(i128::from(frame));
        }
        self.fences.insert(
            key,
            InputFence {
                cursor: ProgramInputCursor {
                    generation: b.next_generation,
                    sequence: 0,
                },
                sealed: false,
            },
        );
        self.data_generation = issued;
        Ok(MediaBarrierReceipt {
            effective_frame: frame,
            effective_ns,
            sample_rate: self.settings.sample_rate(),
            late_frames: frame - requested,
        })
    }
}
