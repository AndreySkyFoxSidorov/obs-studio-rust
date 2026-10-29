//! An authored neural filter was introduced at8 independently of prior features.
use super::*;
pub(super) fn check(schema: u32, snapshot: &StudioSnapshot) -> Result<(), StorageError> {
    if schema >= 8 {
        return Ok(());
    }
    let Some(source) = snapshot.sources().iter().find(|source| {
        source.video_filters().iter().any(|filter| {
            matches!(
                filter.effect,
                skylive_core::VideoEffect::PersonBackgroundRemoval { .. }
            )
        })
    }) else {
        return Ok(());
    };
    Err(rejected(format!(
        "AI person background removal requires native schema8 (source {})",
        source.id().get()
    )))
}
