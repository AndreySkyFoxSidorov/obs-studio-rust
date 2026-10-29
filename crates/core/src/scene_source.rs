//! Validated shared scene graph, independent of pixels and runtime activation.
use crate::{Collection, Error, Id, MAX_SCENE_DEPTH, SourceKind, StudioSnapshot};
use std::collections::{HashMap, HashSet};

impl StudioSnapshot {
    /// Contextual native dimensions. Missing nested targets are errors; audio
    /// alone returns None. Scene dimensions are derived from the chosen profile.
    pub fn source_video_dimensions(&self, kind: &SourceKind) -> Result<Option<(u32, u32)>, Error> {
        if let SourceKind::Scene { scene } = kind {
            self.scene(*scene).ok_or(Error::NotFound(*scene))?;
            Ok(Some((self.video().width(), self.video().height())))
        } else {
            Ok(kind.video_dimensions())
        }
    }
}
impl Collection {
    /// One lazily materialized source owner per selectable root scene.
    /// Absence means default source properties; reads never issue identities.
    pub fn scene_source(&self, scene: Id) -> Option<&crate::Source> {
        self.sources
            .iter()
            .find(|source| matches!(source.kind,SourceKind::Scene{scene:target} if target==scene))
    }
    pub(super) fn ensure_scene_source(&mut self, scene: Id) -> Result<Id, Error> {
        let name = self
            .scene(scene)
            .ok_or(Error::NotFound(scene))?
            .name
            .clone();
        if let Some(source) = self.scene_source(scene) {
            return Ok(source.id);
        }
        self.add_source_owner(name, SourceKind::Scene { scene })
    }
    pub(super) fn validate_scene_sources(&self) -> Result<(), Error> {
        let scenes: HashMap<_, _> = self
            .scenes
            .iter()
            .chain(&self.groups)
            .map(|scene| (scene.id, scene))
            .collect();
        let mut targets = HashSet::new();
        let mut edges: HashMap<Id, Vec<Id>> = HashMap::new();
        for source in &self.sources {
            if let SourceKind::Group { group } = &source.kind {
                if source.gain != 1.0 || source.muted {
                    return Err(Error::Unavailable(
                        "group bus gain/mute is unavailable; edit child source audio instead",
                    ));
                }
                let target = self
                    .groups
                    .iter()
                    .find(|scene| scene.id == group.scene())
                    .ok_or(Error::NotFound(group.scene()))?;
                if !targets.insert(group.scene()) || source.name != target.name {
                    return Err(Error::Invalid("duplicate or incorrectly named group owner"));
                }
                let owners = self
                    .scenes
                    .iter()
                    .chain(&self.groups)
                    .flat_map(|scene| &scene.items)
                    .filter(|item| item.source == source.id)
                    .count();
                if owners != 1 {
                    return Err(Error::Invalid(
                        "group source requires exactly one owning item",
                    ));
                }
            }
            if let SourceKind::Scene { scene } = source.kind {
                if source.gain != 1.0 || source.muted {
                    return Err(Error::Unavailable(
                        "nested scene bus gain/mute is unavailable; edit child source audio instead",
                    ));
                }
                let target = self.scene(scene).ok_or(Error::NotFound(scene))?;
                if !targets.insert(scene) {
                    return Err(Error::Invalid("duplicate shared scene source"));
                }
                if source.name != target.name {
                    return Err(Error::Invalid("scene source name must match its target"));
                }
            }
        }
        for group in &self.groups {
            if !targets.contains(&group.id) {
                return Err(Error::Invalid("private group has no source owner"));
            }
        }
        let sources: HashMap<_, _> = self
            .sources
            .iter()
            .map(|source| (source.id, &source.kind))
            .collect();
        for scene in self.scenes.iter().chain(&self.groups) {
            for item in &scene.items {
                if let Some(kind) = sources.get(&item.source) {
                    if let Some(target) = Self::group_source_scene(kind) {
                        edges.entry(scene.id).or_default().push(target);
                    }
                }
            }
        }
        fn depth(
            id: Id,
            edges: &HashMap<Id, Vec<Id>>,
            active: &mut HashSet<Id>,
            memo: &mut HashMap<Id, usize>,
        ) -> Result<usize, Error> {
            if let Some(value) = memo.get(&id) {
                return Ok(*value);
            }
            if !active.insert(id) {
                return Err(Error::Invalid("nested scene cycle"));
            }
            let mut value = 0;
            if let Some(children) = edges.get(&id) {
                for child in children {
                    value = value.max(1 + depth(*child, edges, active, memo)?);
                    if value > MAX_SCENE_DEPTH {
                        return Err(Error::Limit("nested scene depth limit"));
                    }
                }
            }
            active.remove(&id);
            memo.insert(id, value);
            Ok(value)
        }
        let mut memo = HashMap::new();
        let mut active = HashSet::new();
        for scene in scenes.values() {
            depth(scene.id, &edges, &mut active, &mut memo)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, Session, Transform};
    use std::sync::Arc;
    fn opacity_filter() -> crate::VideoFilter {
        let mut effect = crate::VideoEffect::color_correction();
        if let crate::VideoEffect::ColorCorrection { opacity, .. } = &mut effect {
            *opacity = 0.5;
        }
        crate::VideoFilter::new(effect)
            .unwrap()
            .with_name("Root opacity".into())
            .unwrap()
    }
    #[test]
    fn lazy_root_owner_survives_last_reference_duplicate_issues_fresh_filter_ids_and_undo() {
        let mut session = Session::new();
        let root = session.preview();
        let before = session.snapshot();
        session
            .apply(Command::SetSceneVideoFilters {
                scene: root,
                filters: vec![],
            })
            .unwrap();
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        assert!(session.snapshot().scene_source(root).is_none());
        session
            .apply(Command::SetSceneVideoFilters {
                scene: root,
                filters: vec![opacity_filter()],
            })
            .unwrap();
        let model = session.snapshot();
        let owner = model.scene_source(root).unwrap();
        let owner_id = owner.id();
        let original = owner.video_filters().to_vec();
        let second = session
            .apply(Command::AddScene {
                name: "Second".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let item = session
            .apply(Command::AddSceneSource {
                scene: second,
                target: root,
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::RemoveItem {
                scene: second,
                item,
            })
            .unwrap();
        assert_eq!(
            session.snapshot().scene_source(root).unwrap().id(),
            owner_id
        );
        assert_eq!(
            session
                .snapshot()
                .scene_source(root)
                .unwrap()
                .video_filters(),
            original
        );
        let duplicate = session
            .apply(Command::DuplicateScene {
                scene: root,
                name: "Copy".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let model = session.snapshot();
        let copy = model.scene_source(duplicate).unwrap();
        assert_ne!(copy.id(), owner_id);
        assert_eq!(copy.video_filters()[0].effect, original[0].effect);
        assert_eq!(copy.video_filters()[0].name(), original[0].name());
        assert_ne!(copy.video_filters()[0].id(), original[0].id());
        session.apply(Command::Undo).unwrap();
        assert!(session.snapshot().scene(duplicate).is_none());
        assert_eq!(
            session
                .snapshot()
                .scene_source(root)
                .unwrap()
                .video_filters(),
            original
        );
        session.apply(Command::RemoveScene { scene: root }).unwrap();
        assert!(session.snapshot().source(owner_id).is_none());
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().scene_source(root).unwrap().id(),
            owner_id
        );
        let before = session.snapshot();
        assert!(
            session
                .apply(Command::SetSceneVideoFilters {
                    scene: Id::new(99999),
                    filters: vec![opacity_filter()]
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
    }
    #[test]
    fn rebind_switches_references_to_existing_target_owner_without_moving_either_stack() {
        let mut session = Session::new();
        let a = session.preview();
        let b = session
            .apply(Command::AddScene { name: "B".into() })
            .unwrap()
            .created
            .unwrap();
        let host = session
            .apply(Command::AddScene {
                name: "Container".into(),
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::SetSceneVideoFilters {
                scene: a,
                filters: vec![opacity_filter()],
            })
            .unwrap();
        let item = session
            .apply(Command::AddSceneSource {
                scene: host,
                target: a,
            })
            .unwrap()
            .created
            .unwrap();
        let old = session.snapshot().scene_source(a).unwrap().id();
        let before = session.snapshot();
        session
            .apply(Command::RebindSceneSource {
                source: old,
                target: b,
            })
            .unwrap();
        let model = session.snapshot();
        let new = model.scene_source(b).unwrap();
        assert!(new.video_filters().is_empty());
        assert_eq!(
            model.scene_source(a).unwrap().video_filters(),
            before.scene_source(a).unwrap().video_filters()
        );
        assert_eq!(model.scene(host).unwrap().items()[0].id(), item);
        assert_eq!(model.scene(host).unwrap().items()[0].source(), new.id());
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().scene(host).unwrap().items()[0].source(),
            old
        );
    }
    #[test]
    fn shared_nested_reference_rename_rebind_remove_and_undo_are_atomic() {
        let mut session = Session::new();
        let outer = session.preview();
        let target = session
            .apply(Command::AddScene {
                name: "Nested".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let item = session
            .apply(Command::AddSceneSource {
                scene: outer,
                target,
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().scene(outer).unwrap().items()[0].source();
        session
            .apply(Command::AddSceneSource {
                scene: outer,
                target,
            })
            .unwrap();
        assert_eq!(session.snapshot().sources().len(), 1);
        assert_eq!(
            session.snapshot().scene(outer).unwrap().items()[1].source(),
            source
        );
        session
            .apply(Command::RenameScene {
                scene: target,
                name: "Renamed".into(),
            })
            .unwrap();
        assert_eq!(session.snapshot().source(source).unwrap().name(), "Renamed");
        let before = session.snapshot();
        assert!(
            session
                .apply(Command::AddSceneSource {
                    scene: target,
                    target: outer
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        let other = session
            .apply(Command::AddScene {
                name: "Other".into(),
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::RebindSceneSource {
                source,
                target: other,
            })
            .unwrap();
        assert_eq!(
            session.snapshot().scene(outer).unwrap().items()[0].id(),
            item
        );
        let replacement = session.snapshot().scene_source(other).unwrap().id();
        assert_eq!(
            session.snapshot().scene(outer).unwrap().items()[0].source(),
            replacement
        );
        assert_eq!(
            session.snapshot().source(source).unwrap().kind(),
            &SourceKind::Scene { scene: target }
        );
        let before_remove = session.snapshot();
        session
            .apply(Command::RemoveScene { scene: other })
            .unwrap();
        assert!(session.snapshot().scene(outer).unwrap().items().is_empty());
        assert!(session.snapshot().source(replacement).is_none());
        assert!(
            session.snapshot().source(source).is_some(),
            "old owner belongs to its surviving root scene"
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().collection(), before_remove.collection());
        let duplicate = session
            .apply(Command::DuplicateScene {
                scene: outer,
                name: "Copy".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let model = session.snapshot();
        assert_eq!(
            model.scene(duplicate).unwrap().items()[0].source(),
            replacement
        );
        assert_ne!(model.scene(duplicate).unwrap().items()[0].id(), item);
    }
    #[test]
    fn missing_hidden_cycles_and_depth_limit_reject_without_publication() {
        let mut session = Session::new();
        let first = session.preview();
        let mut current = first;
        let before = session.snapshot();
        assert!(
            session
                .apply(Command::AddSceneSource {
                    scene: first,
                    target: Id::new(999)
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        for n in 0..=MAX_SCENE_DEPTH {
            let next = session
                .apply(Command::AddScene {
                    name: format!("Level {n}"),
                })
                .unwrap()
                .created
                .unwrap();
            let before = session.snapshot();
            let result = session.apply(Command::AddSceneSource {
                scene: current,
                target: next,
            });
            if n == MAX_SCENE_DEPTH {
                assert!(result.is_err());
                assert!(Arc::ptr_eq(&before, &session.snapshot()));
            } else {
                result.unwrap();
                current = next;
            }
        }
        let snapshot = session.snapshot();
        let kind = snapshot.sources()[0].kind();
        assert_eq!(kind.video_dimensions(), None);
        assert_eq!(
            snapshot.source_video_dimensions(kind).unwrap(),
            Some((1920, 1080))
        );
        let mut malformed = (*snapshot).clone();
        let source = malformed
            .collection
            .sources
            .iter_mut()
            .find(|source| matches!(source.kind, SourceKind::Scene { .. }))
            .unwrap();
        source.kind = SourceKind::Scene {
            scene: Id::new(999),
        };
        assert!(malformed.validate().is_err());
    }
    #[test]
    fn scene_item_transform_is_real_canvas_geometry_and_invalid_crop_rolls_back() {
        let mut session = Session::new();
        let outer = session.preview();
        let target = session
            .apply(Command::AddScene {
                name: "Child".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let item = session
            .apply(Command::AddSceneSource {
                scene: outer,
                target,
            })
            .unwrap()
            .created
            .unwrap();
        let mut transform = Transform::full(1920, 1080);
        transform.width = 960.0;
        transform.crop.left = 20;
        session
            .apply(Command::SetTransform {
                scene: outer,
                item,
                transform: transform.clone(),
            })
            .unwrap();
        let before = session.snapshot();
        transform.crop.right = 1900;
        assert!(
            session
                .apply(Command::SetTransform {
                    scene: outer,
                    item,
                    transform
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        session.apply(Command::Undo).unwrap();
        assert_eq!(
            session.snapshot().scene(outer).unwrap().items()[0].transform(),
            &Transform::full(1920, 1080)
        );
    }
    #[test]
    fn scene_bus_audio_is_rejected_explicitly_without_silent_ignored_settings() {
        let mut session = Session::new();
        let outer = session.preview();
        let target = session
            .apply(Command::AddScene {
                name: "Audio child".into(),
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::AddSceneSource {
                scene: outer,
                target,
            })
            .unwrap();
        let before = session.snapshot();
        let source = before.sources()[0].id();
        for (gain, muted) in [(0.5, false), (1.0, true)] {
            assert!(matches!(
                session.apply(Command::SetAudio {
                    source,
                    gain,
                    muted
                }),
                Err(Error::Unavailable(_))
            ));
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
        let mut malformed = (*before).clone();
        malformed.collection.sources[0].muted = true;
        assert!(malformed.validate().is_err());
    }
}
