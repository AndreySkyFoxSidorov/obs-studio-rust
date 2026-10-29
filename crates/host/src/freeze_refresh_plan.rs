//! Pure start-point choice for ordered Freeze stages in a source chain.
//! The temporal worker owns actual history and must validate every request and
//! capture against its current grant before and after running the prefix.

#[derive(Clone, Copy)]
pub(super) struct FreezeStage {
    pub index: usize,
    pub held: bool,
    pub unmasked: bool,
    pub refresh_requested: bool,
}

/// A downstream held frame can hide earlier work for ordinary rendering, but
/// it must not suppress a requested earlier refresh. The latest valid full
/// hold strictly before the earliest refresh remains an admissible prefix.
/// If there is no requested refresh the existing last-full-hold rule applies.
/// The input order is the actual filter-chain order, bounded by the source filter limit.
pub(super) fn start(stages: &[FreezeStage]) -> usize {
    let earliest_refresh = stages.iter().find(|stage| stage.refresh_requested).map(|stage| stage.index);
    stages.iter().rev()
        .find(|stage| stage.held && stage.unmasked && earliest_refresh.is_none_or(|index| stage.index < index))
        .map_or(0, |stage| stage.index)
}
