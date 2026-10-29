//! Closed OBS reference destinations shared by native menus and capabilities.
//! Looking up a destination never starts navigation.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    HelpPortal,
    Website,
    Discord,
    ReleaseNotes,
}

impl Target {
    pub const ALL: [Self; 4] = [Self::HelpPortal, Self::Website, Self::Discord, Self::ReleaseNotes];

    pub const fn action_id(self) -> &'static str {
        match self {
            Self::HelpPortal => "actionHelpPortal",
            Self::Website => "actionWebsite",
            Self::Discord => "actionDiscord",
            Self::ReleaseNotes => "actionReleaseNotes",
        }
    }

    pub const fn url(self) -> &'static str {
        match self {
            Self::HelpPortal => "https://obsproject.com/help",
            Self::Website => "https://obsproject.com",
            Self::Discord => "https://obsproject.com/discord",
            Self::ReleaseNotes => "https://github.com/obsproject/obs-studio/releases/32.2.2",
        }
    }

    pub fn from_action_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|target| target.action_id() == id)
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Link {
    pub target: Target,
    pub action_id: &'static str,
    pub url: &'static str,
}

/// Fixed metadata only. The desktop owns the explicit navigation gesture.
pub fn catalog() -> [Link; 4] {
    Target::ALL.map(|target| Link {
        target,
        action_id: target.action_id(),
        url: target.url(),
    })
}
