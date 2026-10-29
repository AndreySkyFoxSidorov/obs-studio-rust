//! Typed resource failure policy, preserved across owner snapshots.
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceIssueKind {
    /// An external source cannot currently contribute pixels. Other layers remain valid.
    Unavailable,
    /// A declared memory, decode, or raster-work admission limit was exceeded.
    Limit,
    /// Invalid authored state, arithmetic, or owner/freshness identity.
    Invalid,
}

impl ResourceIssueKind {
    pub fn recoverable(self) -> bool {
        self == Self::Unavailable
    }
}
