//! Shared/live nested scenes with bounded last-observed metadata fallbacks.
use super::*;
use skylive_core::{MAX_ITEMS, MAX_SCENE_DEPTH, MAX_SCENES, SourceKind};
use std::collections::HashSet;
type RetainedSceneMetadata = (HashMap<Id, Arc<Source>>, HashMap<Id, Arc<Scene>>);

impl FrameScene {
    pub fn source_video_dimensions(
        &self,
        current: &StudioSnapshot,
        source: &Source,
    ) -> Result<Option<(u32, u32)>, TransitionError> {
        if let SourceKind::Group { group } = source.kind() {
            fn has_video(
                view: &FrameScene,
                current: &StudioSnapshot,
                scene: Id,
                depth: usize,
            ) -> Result<bool, TransitionError> {
                if depth > MAX_SCENE_DEPTH {
                    return Err(TransitionError::Limit("group video depth limit"));
                }
                for item in view.scene_items(current, scene)? {
                    let source = view
                        .source(current, item.source())
                        .ok_or(TransitionError::NotFound(item.source()))?;
                    match source.kind() {
                        SourceKind::Group { group } => {
                            if has_video(view, current, group.scene(), depth + 1)? {
                                return Ok(true);
                            }
                        }
                        SourceKind::Scene { .. } => return Ok(true),
                        leaf if leaf.video_dimensions().is_some() => return Ok(true),
                        _ => {}
                    }
                }
                Ok(false)
            }
            Ok(has_video(self, current, group.scene(), 0)?.then(|| group.basis()))
        } else if let SourceKind::Scene { scene } = source.kind() {
            self.scene_items(current, *scene)?;
            Ok(Some(self.filter_canvas_dimensions(current)))
        } else {
            Ok(source.kind().video_dimensions())
        }
    }
    /// Nested references remain shared under OBS PRIVATE_REFS. A removed scene
    /// uses the last layout observed before its canonical removal.
    pub fn scene_items<'a>(
        &'a self,
        current: &'a StudioSnapshot,
        scene: Id,
    ) -> Result<&'a [SceneItem], TransitionError> {
        if let Self::Frozen(view)=self {
            if view.auto_text_draft.iter().any(|snapshot|snapshot.is_pinned())
                && view.retained.values().any(|source|matches!(source.kind(),SourceKind::Group{group} if group.scene()==scene)) {
                if let Some(layout)=view.nested.get(&scene){return Ok(layout.items());}
            }
            if view.normalized_group_preview {
                if let Some(layout)=view.nested.get(&scene) {return Ok(layout.items());}
            }
        }
        current
            .scene_container(scene)
            .or_else(|| match self {
                Self::Frozen(view) => view.nested.get(&scene).map(Arc::as_ref).or_else(||view.projection.as_ref().and_then(|pool|pool.metadata.model.scene_container(scene))),
                Self::Live(_) => None,
            })
            .map(Scene::items)
            .ok_or(TransitionError::NotFound(scene))
    }
    /// Unique visible source closure. Memoization bounds DAG fan-out; active
    /// path checks also reject cycles formed by a frozen root plus live refs.
    pub fn visible_sources<'a>(
        &'a self,
        current: &'a StudioSnapshot,
    ) -> Result<Vec<&'a Source>, TransitionError> {
        if let Self::Frozen(view) = self {
            if let Some(error) = &view.closure_error {
                return Err(error.clone());
            }
        }
        #[allow(clippy::too_many_arguments)]
        fn visit<'a>(
            view: &'a FrameScene,
            current: &'a StudioSnapshot,
            items: &[SceneItem],
            depth: usize,
            active: &mut HashSet<Id>,
            done: &mut HashMap<Id, usize>,
            seen: &mut HashSet<Id>,
            out: &mut Vec<&'a Source>,
            edges: &mut usize,
        ) -> Result<usize, TransitionError> {
            if depth > MAX_SCENE_DEPTH {
                return Err(TransitionError::Limit("nested scene runtime depth limit"));
            }
            *edges = edges
                .checked_add(items.len())
                .ok_or(TransitionError::Limit("nested scene edge budget"))?;
            if *edges > 2 * MAX_ITEMS {
                return Err(TransitionError::Limit("nested scene edge budget"));
            }
            let mut height = 0;
            for item in items.iter().filter(|item| item.visible()) {
                let source = view
                    .source(current, item.source())
                    .ok_or(TransitionError::NotFound(item.source()))?;
                if seen.insert(source.id()) {
                    if out.len() >= 2 * MAX_SOURCES {
                        return Err(TransitionError::Limit("nested source-reference budget"));
                    }
                    out.push(source);
                }
                if let Some(scene) = source.kind().nested_scene() {
                    if active.contains(&scene) {
                        return Err(TransitionError::Engine(skylive_core::Error::Invalid(
                            "nested runtime scene cycle",
                        )));
                    }
                    let child_height = if let Some(height) = done.get(&scene) {
                        *height
                    } else {
                        active.insert(scene);
                        let height = visit(
                            view,
                            current,
                            view.scene_items(current, scene)?,
                            depth + 1,
                            active,
                            done,
                            seen,
                            out,
                            edges,
                        )?;
                        active.remove(&scene);
                        done.insert(scene, height);
                        height
                    };
                    height = height.max(1 + child_height);
                    if depth + height > MAX_SCENE_DEPTH {
                        return Err(TransitionError::Limit("nested scene runtime depth limit"));
                    }
                }
            }
            Ok(height)
        }
        let mut out = Vec::new();
        // A PRIVATE_REFS root is a distinct OBS source instance. Its numeric
        // canonical Scene ID must not mark that live Scene as already active:
        // old private A→B may legitimately reach a newer live A through B.
        let mut active = match self {
            Self::Live(scene) => HashSet::from([*scene]),
            Self::Frozen(_) => HashSet::new(),
        };
        let mut done = HashMap::new();
        let mut seen = HashSet::new();
        let mut edges = 0;
        visit(
            self,
            current,
            self.items(current)?,
            0,
            &mut active,
            &mut done,
            &mut seen,
            &mut out,
            &mut edges,
        )?;
        Ok(out)
    }
    pub(super) fn refreshed_closure(
        &self,
        current: &StudioSnapshot,
    ) -> Result<RetainedSceneMetadata, TransitionError> {
        let Self::Frozen(old) = self else {
            return Ok((HashMap::new(), HashMap::new()));
        };
        let mut sources = HashMap::new();
        let mut scenes = HashMap::new();
        let mut pending = vec![(old.layout.clone(), true)];
        let mut seen = HashSet::new();
        let mut edges = 0usize;
        while let Some((layout, private_root)) = pending.pop() {
            // The captured private root and a reachable current Scene with the
            // same authored ID have different layouts/lifetimes. Traverse and
            // retain the latter once under its canonical key.
            if !private_root && !seen.insert(layout.id()) {
                continue;
            }
            edges = edges
                .checked_add(layout.items().len())
                .ok_or(TransitionError::Limit("retained scene item budget"))?;
            if edges > 2 * MAX_ITEMS {
                return Err(TransitionError::Limit("retained scene item budget"));
            }
            for item in layout.items() {
                let Some(source) = self.source(current, item.source()) else {
                    continue;
                };
                if let Some(scene) = source.kind().nested_scene() {
                    if !seen.contains(&scene) && !scenes.contains_key(&scene) {
                        if let Some(nested) = current
                            .scene_container(scene)
                            .map(|scene| Arc::new(scene.clone()))
                            .or_else(|| old.nested.get(&scene).cloned())
                        {
                            if scenes.len() >= 2 * (MAX_SCENES + MAX_SOURCES)
                                && !scenes.contains_key(&scene)
                            {
                                return Err(TransitionError::Limit("retained nested scene budget"));
                            }
                            scenes.insert(scene, nested.clone());
                            pending.push((nested, false));
                        }
                    }
                }
                if sources.len() + usize::from(old.root_source.is_some()) >= 2 * MAX_SOURCES
                    && !sources.contains_key(&source.id())
                {
                    return Err(TransitionError::Limit("retained nested source budget"));
                }
                sources
                    .entry(source.id())
                    .or_insert_with(|| Arc::new(source.clone()));
            }
        }
        // Each reachable object comes from either the current bounded model or
        // the previous bounded closure; unreachable fallbacks retire here.
        Ok((sources, scenes))
    }
}

#[cfg(test)]
mod private_identity_tests {
    use super::*;
    use skylive_core::{Command, Session};
    #[test]
    fn distinct_private_root_does_not_disable_real_retained_scene_cycle_guard() {
        let mut session = Session::new();
        let a = session.preview();
        let b = session
            .apply(Command::AddScene { name: "B".into() })
            .unwrap()
            .created
            .unwrap();
        let item = session
            .apply(Command::AddSceneSource {
                scene: a,
                target: b,
            })
            .unwrap()
            .created
            .unwrap();
        let mut view = FrameScene::capture(session.identity(), &session.snapshot(), a,None).unwrap();
        let old_a = session.snapshot().scene(a).unwrap().clone();
        session
            .apply(Command::RemoveItem { scene: a, item })
            .unwrap();
        session
            .apply(Command::AddSceneSource {
                scene: b,
                target: a,
            })
            .unwrap();
        let intermediate = session.snapshot();
        let old_b = intermediate.scene(b).unwrap().clone();
        let a_source = intermediate.scene_source(a).unwrap().clone();
        session
            .apply(Command::AddScene {
                name: "Survivor".into(),
            })
            .unwrap();
        session.apply(Command::RemoveScene { scene: b }).unwrap();
        session.apply(Command::RemoveScene { scene: a }).unwrap();
        // Stitch independently valid historical metadata into a cycle. Even an
        // invalid retained closure must fail before raster recursion/allocation.
        let FrameScene::Frozen(instance) = &mut view else {
            unreachable!()
        };
        let instance = Arc::make_mut(instance);
        instance.nested.insert(a, Arc::new(old_a));
        instance.nested.insert(b, Arc::new(old_b));
        instance.retained.insert(a_source.id(), Arc::new(a_source));
        assert!(matches!(
            view.visible_sources(&session.snapshot()),
            Err(TransitionError::Engine(skylive_core::Error::Invalid(
                "nested runtime scene cycle"
            )))
        ));
    }
}
