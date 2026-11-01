//! Deterministic association of a camera and active audio inputs using a
//! Windows device container identity obtained by a platform owner.
//!
//! Friendly names, USB vendor/product IDs and interface paths are not proof
//! that two devices are the same physical device. Unknown and ambiguous
//! identities therefore never select an endpoint.
use skylive_core::AudioTarget;

#[cfg(target_os = "windows")]
pub mod windows;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerId(pub [u8; 16]);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Association {
    UnknownCamera,
    NoMatch,
    Ambiguous,
    Unique(AudioTarget),
}

/// Runtime choice for an automatic camera route. The authored route must stay
/// independent of the endpoint, so reconnecting a device cannot silently
/// convert automatic selection into a persisted manual selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutoRoute {
    generation: u64,
    choice: Association,
}

impl Default for AutoRoute {
    fn default() -> Self {
        Self { generation: 0, choice: Association::UnknownCamera }
    }
}

impl AutoRoute {
    pub fn generation(&self) -> u64 { self.generation }
    pub fn choice(&self) -> &Association { &self.choice }

    /// Publish a changed endpoint or loss of identity as a new generation.
    /// Callers must stop the prior capture and fence its packets before using
    /// a new endpoint. Unchanged enumeration results do not restart capture.
    pub fn update(&mut self, choice: Association) -> Result<bool, &'static str> {
        if self.choice == choice { return Ok(false); }
        let next = self.generation.checked_add(1).ok_or("camera audio route generation exhausted")?;
        self.choice = choice;
        self.generation = next;
        Ok(true)
    }
}

/// All candidate identities must come from active recording endpoints. The
/// caller is responsible for obtaining identities and applying the choice on
/// an owning worker; this function performs no device I/O.
pub fn associate<'a>(camera: Option<ContainerId>, candidates: impl IntoIterator<Item = (Option<ContainerId>, &'a AudioTarget)>) -> Association {
    let Some(camera) = camera.filter(|id| id.0 != [0; 16]) else { return Association::UnknownCamera; };
    let mut selected: Option<&AudioTarget> = None;
    for (container, target) in candidates {
        if container != Some(camera) { continue; }
        match selected {
            None => selected = Some(target),
            Some(previous) if previous == target => {},
            Some(_) => return Association::Ambiguous,
        }
    }
    selected.cloned().map_or(Association::NoMatch, Association::Unique)
}
