//! Atomic OBS-style filter prepend. Frontend clipboards retain IDs, not model owners.
use crate::{Error, Id, MAX_AUDIO_FILTERS, MAX_VIDEO_FILTERS, StudioSnapshot};

impl StudioSnapshot {
    /// Pure capability query; the command repeats it against its candidate.
    pub fn check_filter_paste(&self, from: Id, to: Id) -> Result<(), Error> {
        let source = self.source(from).ok_or(Error::NotFound(from))?;
        let target = self.source(to).ok_or(Error::NotFound(to))?;
        if from == to {
            return Ok(());
        }
        if !source.audio_filters().is_empty() && !target.kind().supports_audio_settings() {
            return Err(Error::Invalid(
                "Filter paste requires an audio-capable destination for every copied audio filter",
            ));
        }
        if !source.video_filters().is_empty()
            && self.source_video_dimensions(target.kind())?.is_none()
        {
            return Err(Error::Invalid(
                "Filter paste requires a video-capable destination for every copied video filter",
            ));
        }
        if source
            .audio_filters()
            .len()
            .checked_add(target.audio_filters().len())
            .is_none_or(|n| n > MAX_AUDIO_FILTERS)
            || source
                .video_filters()
                .len()
                .checked_add(target.video_filters().len())
                .is_none_or(|n| n > MAX_VIDEO_FILTERS)
        {
            return Err(Error::Limit(
                "Pasted filters exceed the destination filter-chain limit",
            ));
        }
        let mut copied=source.audio_filters().to_vec();copied.extend_from_slice(target.audio_filters());
        self.collection.validate_sidechain_assignment(to,&copied,&copied)?;
        for provider in source
            .audio_filters()
            .iter()
            .map(|f| f.plugin_id())
            .chain(source.video_filters().iter().map(|f| f.plugin_id()))
        {
            if !self.preferences().plugin_enabled(provider) {
                return Err(Error::Unavailable(
                    "Enable every copied filter module before pasting",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn paste_source_filters(&mut self, from: Id, to: Id) -> Result<(), Error> {
        self.check_filter_paste(from, to)?;
        if from == to {
            return Ok(());
        }
        let source = self.source(from).ok_or(Error::NotFound(from))?;
        let target = self.source(to).ok_or(Error::NotFound(to))?;
        let mut audio = source.audio_filters().to_vec();
        audio.extend_from_slice(target.audio_filters());
        let mut video = target.video_filters().to_vec();
        // OBS32 duplicate_filters traverses the source backwards and inserts
        // each copy at the front; name allocation follows that same traversal.
        for filter in source.video_filters().iter().rev() {
            let base = filter.name();
            let mut chosen = None;
            for suffix in 1..=MAX_VIDEO_FILTERS + 1 {
                let name = if suffix == 1 {
                    base.to_owned()
                } else {
                    // Keep the numbered copy within the canonical UTF-8 byte
                    // limit, without splitting a non-ASCII character.
                    let suffix = format!(" {suffix}");
                    let mut end = base.len().min(256 - suffix.len());
                    while !base.is_char_boundary(end) { end -= 1; }
                    format!("{}{suffix}", &base[..end])
                };
                if video.iter().all(|old| old.name() != name) {
                    chosen = Some(name);
                    break;
                }
            }
            let copy = filter.as_new().with_name(
                chosen.ok_or(Error::Limit("Pasted filter name allocation exhausted"))?,
            )?;
            video.insert(0, copy);
        }
        crate::video_filters::validate_activation(
            &self.preferences,
            target.video_filters(),
            &video,
        )?;
        self.collection.validate_sidechain_assignment(to,&audio,&audio)?;
        self.collection.set_video_filters(to, video)?;
        self.collection.source_mut(to)?.audio_filters = audio.into_boxed_slice().into_vec();
        Ok(())
    }
}

#[cfg(test)]
#[path = "source_filter_paste_tests.rs"]
mod tests;
