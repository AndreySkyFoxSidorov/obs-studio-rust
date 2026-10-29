//! Shared scene-closure policy for persistent video devices. Unlike screenshots,
//! an authored hidden-live camera remains active until explicit stop/release.
use super::*;
use crate::transition::FrameScene;
use skylive_core::{Id, Session, SourceKind};
use std::collections::{HashMap, HashSet};
impl CameraOwner {
    pub fn synchronize(
        &mut self,
        session: &Session,
        outputs: &[FrameScene],
    ) -> Result<bool, CameraError> {
        if outputs.len() > crate::projector::MAX_VIDEO_VIEWS {
            return Err(CameraError::BudgetExceeded("camera view union"));
        }
        let current = session.snapshot();
        let mut bindings = HashMap::new();
        for source in current.sources() {
            if let SourceKind::VideoCapture { binding } = source.kind() {
                if !binding.deactivate_when_hidden() {
                    insert(&mut bindings, source.id(), binding)?;
                }
            }
        }
        for view in outputs {
            view.validate_session(session.identity())
                .map_err(|_| CameraError::ForeignSession)?;
            for source in view
                .visible_sources(&current)
                .map_err(|_| CameraError::InvalidFrame("camera visible scene closure"))?
            {
                if let SourceKind::VideoCapture { binding } = source.kind() {
                    insert(&mut bindings, source.id(), binding)?;
                }
            }
            // Retained frozen source objects also own hidden-live devices. Traverse
            // hidden edges with scene memoization; the visible closure above performs
            // the renderer's mixed frozen/live cycle validation before admission.
            let mut scenes = vec![view.scene()];
            let mut seen = HashSet::new();
            let mut edges = 0usize;
            while let Some(scene) = scenes.pop() {
                if !seen.insert(scene) {
                    continue;
                }
                let items = if scene == view.scene() {
                    view.items(&current)
                } else {
                    view.scene_items(&current, scene)
                }
                .map_err(|_| CameraError::InvalidFrame("camera retained scene closure"))?;
                edges = edges
                    .checked_add(items.len())
                    .ok_or(CameraError::BudgetExceeded("camera scene edges"))?;
                if seen.len() > skylive_core::MAX_SCENES + skylive_core::MAX_SOURCES
                    || edges > 2 * skylive_core::MAX_ITEMS
                {
                    return Err(CameraError::BudgetExceeded("camera retained scene closure"));
                }
                for item in items {
                    let source = view
                        .source(&current, item.source())
                        .ok_or(CameraError::InvalidFrame("camera retained source missing"))?;
                    if let Some(scene) = source.kind().nested_scene() {
                        scenes.push(scene);
                    }
                    match source.kind() {
                        SourceKind::VideoCapture { binding }
                            if !binding.deactivate_when_hidden() =>
                        {
                            insert(&mut bindings, source.id(), binding)?
                        }
                        _ => {}
                    }
                }
            }
        }
        self.synchronize_bindings(session, bindings.into_iter().collect())
    }
}
fn insert(
    bindings: &mut HashMap<Id, VideoCaptureBinding>,
    source: Id,
    binding: &VideoCaptureBinding,
) -> Result<(), CameraError> {
    if !bindings.contains_key(&source) && bindings.len() >= crate::capture::MAX_CAPTURE_REFERENCES {
        return Err(CameraError::BudgetExceeded("camera source union"));
    }
    if bindings.get(&source).is_some_and(|old|!old.same_video(binding)) {
        return Err(CameraError::InvalidFrame("conflicting frozen/live camera bindings for one source"));
    }
    bindings.insert(source, binding.video_only());
    Ok(())
}
