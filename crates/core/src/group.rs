//! Private group containers and their authored scale basis. No pixels or OS state.
use crate::{
    Collection, Error, Id, ItemBounds, MAX_DISPLAY_PIXELS, MAX_ITEMS, MAX_SCENE_DEPTH, Scene,
    SceneItem, SourceKind, StudioSnapshot, Transform,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "GroupData")]
pub struct GroupSource {
    scene: Id,
    width: u32,
    height: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupData {
    scene: Id,
    width: u32,
    height: u32,
}
impl TryFrom<GroupData> for GroupSource {
    type Error = Error;
    fn try_from(value: GroupData) -> Result<Self, Error> {
        Self::new(value.scene, value.width, value.height)
    }
}
impl GroupSource {
    pub(crate) fn new(scene: Id, width: u32, height: u32) -> Result<Self, Error> {
        if scene.get() == 0
            || width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || u64::from(width) * u64::from(height) > MAX_DISPLAY_PIXELS
        {
            return Err(Error::Invalid("invalid group identity or scale basis"));
        }
        Ok(Self {
            scene,
            width,
            height,
        })
    }
    pub fn scene(&self) -> Id {
        self.scene
    }
    /// Last nonempty native bounds, retained through an empty group to preserve scale.
    /// An empty group's 1x1 initial basis is metadata, never a rendered pixel.
    pub fn basis(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub(super) fn validate(&self) -> Result<(), Error> {
        Self::new(self.scene, self.width, self.height).map(|_| ())
    }
}

impl Collection {
    pub fn groups(&self) -> &[Scene] {
        &self.groups
    }
    pub fn scene_container(&self, id: Id) -> Option<&Scene> {
        self.scene(id)
            .or_else(|| self.groups.iter().find(|scene| scene.id == id))
    }
    pub fn item_container(&self, item: Id) -> Option<(Id, &SceneItem)> {
        self.scenes.iter().chain(&self.groups).find_map(|scene| {
            scene
                .items
                .iter()
                .find(|entry| entry.id == item)
                .map(|entry| (scene.id, entry))
        })
    }
    pub fn root_scene_for_container(&self, mut container: Id) -> Option<Id> {
        for _ in 0..=MAX_SCENE_DEPTH {
            if self.scene(container).is_some() {
                return Some(container);
            }
            container = self.group_parent(container)?.0;
        }
        None
    }
    pub fn container_is_locked(&self, mut container: Id) -> bool {
        for _ in 0..=MAX_SCENE_DEPTH {
            let Some((parent, item)) = self.group_parent(container) else {
                return false;
            };
            if self
                .item_container(item)
                .is_some_and(|(_, item)| item.locked)
            {
                return true;
            }
            container = parent;
        }
        true
    }
    pub(super) fn ensure_container_unlocked(&self, mut container: Id) -> Result<(), Error> {
        for _ in 0..=MAX_SCENE_DEPTH {
            let Some((parent, item)) = self.group_parent(container) else {
                return Ok(());
            };
            if self
                .item_container(item)
                .is_some_and(|(_, item)| item.locked)
            {
                return Err(Error::Locked(item));
            }
            container = parent;
        }
        Err(Error::Limit("group ancestor depth"))
    }
    fn item_has_video(&self, item: &SceneItem) -> Result<bool, Error> {
        let source = self
            .source(item.source)
            .ok_or(Error::NotFound(item.source))?;
        Ok(source.kind.video_dimensions().is_some() || source.kind.nested_scene().is_some())
    }
    pub fn group_parent(&self, group: Id) -> Option<(Id, Id)> {
        let source = self.sources.iter().find(|source| matches!(&source.kind, SourceKind::Group { group: value } if value.scene == group))?.id;
        self.scenes.iter().chain(&self.groups).find_map(|scene| {
            scene
                .items
                .iter()
                .find(|item| item.source == source)
                .map(|item| (scene.id, item.id))
        })
    }
    pub(super) fn group_source_scene(kind: &SourceKind) -> Option<Id> {
        match kind {
            SourceKind::Scene { scene } => Some(*scene),
            SourceKind::Group { group } => Some(group.scene),
            _ => None,
        }
    }
    pub(super) fn prune_group_sources(&mut self) {
        let mut needed: HashSet<Id> = self
            .scenes
            .iter()
            .flat_map(|scene| scene.items.iter().map(|item| item.source))
            .chain(self.global_audio.iter().map(|root| root.source()))
            .chain(self.sources.iter().filter_map(|source| match source.kind {
                SourceKind::Scene { scene } if self.scene(scene).is_some() => Some(source.id),
                _ => None,
            }))
            .collect();
        let mut pending: Vec<Id> = needed.iter().copied().collect();
        let mut groups = HashSet::new();
        while let Some(id) = pending.pop() {
            let Some(SourceKind::Group { group }) = self.source(id).map(|source| &source.kind)
            else {
                continue;
            };
            if !groups.insert(group.scene) {
                continue;
            }
            if let Some(scene) = self.groups.iter().find(|scene| scene.id == group.scene) {
                for item in &scene.items {
                    if needed.insert(item.source) {
                        pending.push(item.source);
                    }
                }
            }
        }
        self.sources.retain(|source| needed.contains(&source.id));
        self.groups.retain(|scene| groups.contains(&scene.id));
    }
    pub(super) fn create_group(
        &mut self,
        parent: Id,
        name: String,
        selected: Vec<Id>,
    ) -> Result<Id, Error> {
        self.ensure_container_unlocked(parent)?;
        if selected.len() > MAX_ITEMS {
            return Err(Error::Limit("group selection exceeds item budget"));
        }
        let mut seen = HashSet::new();
        for id in &selected {
            if !seen.insert(*id) {
                return Err(Error::Invalid("duplicate group selection"));
            }
        }
        let parent_scene = self
            .scene_container(parent)
            .ok_or(Error::NotFound(parent))?;
        let mut children = Vec::with_capacity(selected.len());
        let mut insertion = parent_scene.items.len();
        for (index, item) in parent_scene.items.iter().enumerate() {
            if seen.remove(&item.id) {
                if item.locked {
                    return Err(Error::Locked(item.id));
                }
                if matches!(
                    self.source(item.source).map(|source| &source.kind),
                    Some(SourceKind::Group { .. })
                ) {
                    return Err(Error::Invalid(
                        "group selection cannot contain an existing group",
                    ));
                }
                insertion = index;
                children.push(item.clone());
            }
        }
        if let Some(id) = seen.into_iter().next() {
            return Err(Error::NotFound(id));
        }
        let selected_ids: HashSet<_> = children.iter().map(|item| item.id).collect();
        let remaining_before = parent_scene
            .items
            .iter()
            .take(insertion)
            .filter(|item| !selected_ids.contains(&item.id))
            .count();
        let scene = self.issue()?;
        let source = self.add_source_owner(
            name.clone(),
            SourceKind::Group {
                group: GroupSource::new(scene, 1, 1)?,
            },
        )?;
        let item = self.issue()?;
        self.groups.push(Scene {
            show_in_multiview:true,
            id: scene,
            name: name.into_boxed_str().into_string(),
            items: children,
            notes: String::new(),
            transition_override: None,
            inactive_transition_duration_ms: None,
        });
        let parent_scene = self.scene_mut(parent)?;
        parent_scene
            .items
            .retain(|item| !selected_ids.contains(&item.id));
        parent_scene.items.insert(
            remaining_before,
            SceneItem {
                scale_anchor:None,
                color_tag:crate::SourceItemColorTag::Clear,
                compositing: crate::ItemCompositing::default(),
                id: item,
                source,
                visible: true,
                locked: false,
                transform: Transform::full(1, 1),
            },
        );
        Ok(item)
    }
    pub(super) fn ungroup(&mut self,parent:Id,item:Id)->Result<(),Error>{self.ungroup_with_canvas(parent,item,None,None)}
    fn ungroup_with_canvas(&mut self,parent:Id,item:Id,canvas:Option<(u32,u32)>,preferences:Option<&crate::UserPreferences>)->Result<(),Error>{
        self.ensure_container_unlocked(parent)?;
        let wrapper = self
            .scene_container(parent)
            .ok_or(Error::NotFound(parent))?
            .items
            .iter()
            .find(|entry| entry.id == item)
            .ok_or(Error::NotFound(item))?
            .clone();
        if wrapper.locked {
            return Err(Error::Locked(item));
        }
        if !wrapper.compositing.is_default() {
            return Err(Error::Unavailable("Reset the group's blending and scale filtering before ungrouping; its isolated appearance cannot be distributed to child items"));
        }
        let SourceKind::Group { group } = &self
            .source(wrapper.source)
            .ok_or(Error::NotFound(wrapper.source))?
            .kind
        else {
            return Err(Error::Invalid("selected item is not a group"));
        };
        let basis = group.basis();
        self.ensure_group_flattenable(wrapper.source,basis,canvas,preferences)?;
        let mut children = self
            .scene_container(group.scene)
            .ok_or(Error::NotFound(group.scene))?
            .items
            .clone();
        for child in &mut children {
            if child.locked {
                return Err(Error::Locked(child.id));
            }
            child.id = self.issue()?;
            if self.item_has_video(child)? {
                let projection=self.group_filter_transform(wrapper.source,&wrapper.transform,basis,canvas,preferences)?;
                projection.from_group(&mut child.transform,basis)?;
            }
        }
        let scene = self.scene_mut(parent)?;
        let index = scene
            .items
            .iter()
            .position(|entry| entry.id == item)
            .ok_or(Error::NotFound(item))?;
        scene.items.splice(index..=index, children);
        self.prune_group_sources();
        Ok(())
    }
    pub(super) fn move_into_group(&mut self,parent:Id,item:Id,group_item:Id)->Result<(),Error>{self.move_into_group_with_canvas(parent,item,group_item,None,None)}
    fn move_into_group_with_canvas(&mut self,parent:Id,item:Id,group_item:Id,canvas:Option<(u32,u32)>,preferences:Option<&crate::UserPreferences>)->Result<(),Error>{
        self.ensure_container_unlocked(parent)?;
        if item == group_item {
            return Err(Error::Invalid("group cannot contain itself"));
        }
        let scene = self
            .scene_container(parent)
            .ok_or(Error::NotFound(parent))?;
        let wrapper = scene
            .items
            .iter()
            .find(|entry| entry.id == group_item)
            .ok_or(Error::NotFound(group_item))?
            .clone();
        let mut child = scene
            .items
            .iter()
            .find(|entry| entry.id == item)
            .ok_or(Error::NotFound(item))?
            .clone();
        if wrapper.locked {
            return Err(Error::Locked(group_item));
        }
        if child.locked {
            return Err(Error::Locked(item));
        }
        let SourceKind::Group { group } = &self
            .source(wrapper.source)
            .ok_or(Error::NotFound(wrapper.source))?
            .kind
        else {
            return Err(Error::Invalid("target item is not a group"));
        };
        let target = group.scene;
        if self.item_has_video(&child)? {
            let projection=self.group_filter_transform(wrapper.source,&wrapper.transform,group.basis(),canvas,preferences)?;
            projection.into_group(&mut child.transform,group.basis())?;
        }
        self.scene_mut(parent)?
            .items
            .retain(|entry| entry.id != item);
        self.scene_mut(target)?.items.push(child);
        Ok(())
    }
    pub(super) fn move_out_of_group(&mut self,parent:Id,group_item:Id,item:Id,index:usize)->Result<(),Error>{self.move_out_of_group_with_canvas(parent,group_item,item,index,None,None)}
    fn move_out_of_group_with_canvas(&mut self,parent:Id,group_item:Id,item:Id,index:usize,canvas:Option<(u32,u32)>,preferences:Option<&crate::UserPreferences>)->Result<(),Error>{
        self.ensure_container_unlocked(parent)?;
        let scene = self
            .scene_container(parent)
            .ok_or(Error::NotFound(parent))?;
        if index > scene.items.len() {
            return Err(Error::Invalid("item order index out of range"));
        }
        let wrapper = scene
            .items
            .iter()
            .find(|entry| entry.id == group_item)
            .ok_or(Error::NotFound(group_item))?
            .clone();
        if wrapper.locked {
            return Err(Error::Locked(group_item));
        }
        let SourceKind::Group { group } = &self
            .source(wrapper.source)
            .ok_or(Error::NotFound(wrapper.source))?
            .kind
        else {
            return Err(Error::Invalid("selected item is not a group"));
        };
        let target = group.scene;
        let mut child = self
            .scene_container(target)
            .ok_or(Error::NotFound(target))?
            .items
            .iter()
            .find(|entry| entry.id == item)
            .ok_or(Error::NotFound(item))?
            .clone();
        if child.locked {
            return Err(Error::Locked(item));
        }
        if self.item_has_video(&child)? {
            let projection=self.group_filter_transform(wrapper.source,&wrapper.transform,group.basis(),canvas,preferences)?;
            projection.from_group(&mut child.transform,group.basis())?;
        }
        self.scene_mut(target)?
            .items
            .retain(|entry| entry.id != item);
        self.scene_mut(parent)?.items.insert(index, child);
        Ok(())
    }
    pub(super) fn move_group_item(&mut self,from:Id,into:Id,item:Id,index:usize)->Result<(),Error>{self.move_group_item_with_canvas(from,into,item,index,None,None)}
    fn move_group_item_with_canvas(&mut self,from:Id,into:Id,item:Id,index:usize,canvas:Option<(u32,u32)>,preferences:Option<&crate::UserPreferences>)->Result<(),Error>{
        self.ensure_container_unlocked(from)?;
        self.ensure_container_unlocked(into)?;
        let source = self.scene_container(from).ok_or(Error::NotFound(from))?;
        let mut child = source
            .items
            .iter()
            .find(|entry| entry.id == item)
            .ok_or(Error::NotFound(item))?
            .clone();
        if child.locked {
            return Err(Error::Locked(item));
        }
        let destination = self.scene_container(into).ok_or(Error::NotFound(into))?;
        if from == into {
            if index >= destination.items.len() {
                return Err(Error::Invalid("item order index out of range"));
            }
            let target = self.scene_mut(from)?;
            let old = target
                .items
                .iter()
                .position(|entry| entry.id == item)
                .ok_or(Error::NotFound(item))?;
            let child = target.items.remove(old);
            target.items.insert(index, child);
            return Ok(());
        }
        if index > destination.items.len() {
            return Err(Error::Invalid("item order index out of range"));
        }
        if self.root_scene_for_container(from) != self.root_scene_for_container(into) {
            return Err(Error::Invalid(
                "group membership must remain in one root scene",
            ));
        }
        type AncestorTransform = (GroupFilterProjection, (u32, u32));
        fn ancestors(
            current: &Collection,
            mut container: Id,
            canvas:Option<(u32,u32)>,
            preferences:Option<&crate::UserPreferences>,
        ) -> Result<Vec<AncestorTransform>, Error> {
            let mut result = vec![];
            while current.scene(container).is_none() {
                if result.len() >= MAX_SCENE_DEPTH {
                    return Err(Error::Limit("group ancestor depth"));
                }
                let (parent, item) = current
                    .group_parent(container)
                    .ok_or(Error::NotFound(container))?;
                let (_, item) = current.item_container(item).ok_or(Error::NotFound(item))?;
                let SourceKind::Group { group } = &current
                    .source(item.source)
                    .ok_or(Error::NotFound(item.source))?
                    .kind
                else {
                    return Err(Error::Invalid("invalid group ancestor"));
                };
                result.push((current.group_filter_transform(item.source,&item.transform,group.basis(),canvas,preferences)?,group.basis()));
                container = parent;
            }
            Ok(result)
        }
        if self.item_has_video(&child)? {
            for (transform, basis) in ancestors(self, from,canvas,preferences)? {
                transform.from_group(&mut child.transform,basis)?;
            }
            for (transform, basis) in ancestors(self, into,canvas,preferences)?.into_iter().rev() {
                transform.into_group(&mut child.transform,basis)?;
            }
        }
        self.scene_mut(from)?.items.retain(|entry| entry.id != item);
        self.scene_mut(into)?.items.insert(index, child);
        Ok(())
    }
    pub(super) fn duplicate_private_groups(
        &mut self,
        items: &mut [SceneItem],
    ) -> Result<(), Error> {
        for item in items {
            item.id = self.issue()?;
            let original = self
                .source(item.source)
                .ok_or(Error::NotFound(item.source))?
                .clone();
            let SourceKind::Group { group } = &original.kind else {
                continue;
            };
            let mut container = self
                .scene_container(group.scene)
                .ok_or(Error::NotFound(group.scene))?
                .clone();
            self.duplicate_private_groups(&mut container.items)?;
            container.id = self.issue()?;
            let mut suffix = 2usize;
            let suffixed = |index: usize| {
                let suffix = format!(" {index}");
                let mut end = original.name.len().min(256 - suffix.len());
                while !original.name.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}{suffix}", &original.name[..end])
            };
            let mut name = suffixed(suffix);
            while self.sources.iter().any(|source| source.name == name)
                || self.scenes.iter().any(|scene| scene.name == name)
            {
                suffix += 1;
                name = suffixed(suffix);
            }
            container.name.clone_from(&name);
            let source = self.add_source_owner(
                name,
                SourceKind::Group {
                    group: GroupSource::new(container.id, group.width, group.height)?,
                },
            )?;
            self.groups.push(container);
            let filters = original
                .video_filters
                .iter()
                .map(crate::VideoFilter::as_new)
                .collect();
            self.set_video_filters(source, filters)?;
            item.source = source;
        }
        Ok(())
    }
}

impl StudioSnapshot {
    pub(super) fn ungroup(&mut self,parent:Id,item:Id)->Result<(),Error>{let canvas=Some((self.video().width(),self.video().height()));self.collection.ungroup_with_canvas(parent,item,canvas,Some(&self.preferences))}
    pub(super) fn move_into_group(&mut self,parent:Id,item:Id,group_item:Id)->Result<(),Error>{let canvas=Some((self.video().width(),self.video().height()));self.collection.move_into_group_with_canvas(parent,item,group_item,canvas,Some(&self.preferences))}
    pub(super) fn move_out_of_group(&mut self,parent:Id,group_item:Id,item:Id,index:usize)->Result<(),Error>{let canvas=Some((self.video().width(),self.video().height()));self.collection.move_out_of_group_with_canvas(parent,group_item,item,index,canvas,Some(&self.preferences))}
    pub(super) fn move_group_item(&mut self,from:Id,into:Id,item:Id,index:usize)->Result<(),Error>{let canvas=Some((self.video().width(),self.video().height()));self.collection.move_group_item_with_canvas(from,into,item,index,canvas,Some(&self.preferences))}
    pub fn group_native_dimensions(&self, scene: Id) -> Result<Option<(u32, u32)>, Error> {
        Ok(self
            .group_bounds(scene)?
            .map(|bounds| (bounds.width().ceil() as u32, bounds.height().ceil() as u32)))
    }
    fn group_bounds(&self, scene: Id) -> Result<Option<ItemBounds>, Error> {
        let container = self
            .groups
            .iter()
            .find(|group| group.id == scene)
            .ok_or(Error::NotFound(scene))?;
        let mut bounds: Option<ItemBounds> = None;
        for item in &container.items {
            let source = self
                .source(item.source)
                .ok_or(Error::NotFound(item.source))?;
            if let SourceKind::Group { group } = &source.kind {
                if !self.group_has_video(group.scene, &mut HashSet::new())? {
                    continue;
                }
            }
            let Some((width, height)) = self.source_video_dimensions(&source.kind)? else {
                continue;
            };
            let frame=self.effective_filtered_video_dimensions(source,(width,height),Some((self.video().width(),self.video().height())))?;
            let next = item.transform.geometry_for_frame(width,height,frame.0,frame.1)?.bounds();
            bounds = Some(bounds.map_or(next, |old| ItemBounds {
                left: old.left.min(next.left),
                top: old.top.min(next.top),
                right: old.right.max(next.right),
                bottom: old.bottom.max(next.bottom),
            }));
        }
        if let Some(value) = bounds {
            let width = value.width().ceil();
            let height = value.height().ceil();
            if !width.is_finite()
                || !height.is_finite()
                || width <= 0.0
                || height <= 0.0
                || width > 8192.0
                || height > 8192.0
                || width * height > MAX_DISPLAY_PIXELS as f64
            {
                return Err(Error::Limit("group native raster bounds"));
            }
        }
        Ok(bounds)
    }
    fn group_has_video(&self, scene: Id, active: &mut HashSet<Id>) -> Result<bool, Error> {
        if active.len() >= MAX_SCENE_DEPTH || !active.insert(scene) {
            return Err(Error::Invalid("group cycle or depth limit"));
        }
        let container = self.scene_container(scene).ok_or(Error::NotFound(scene))?;
        for item in &container.items {
            let source = self
                .source(item.source)
                .ok_or(Error::NotFound(item.source))?;
            let video = match &source.kind {
                SourceKind::Group { group } => self.group_has_video(group.scene, active)?,
                SourceKind::Scene { .. } => true,
                leaf => leaf.video_dimensions().is_some(),
            };
            if video {
                active.remove(&scene);
                return Ok(true);
            }
        }
        active.remove(&scene);
        Ok(false)
    }
    pub(super) fn normalize_groups(&mut self, previous: Option<&StudioSnapshot>) -> Result<(), Error> {
        // Source kind holds the last nonempty basis; traverse groups bottom-up,
        // then update parent extents and pivot atomically with child translation.
        fn visit(
            id: Id,
            current: &StudioSnapshot,
            active: &mut HashSet<Id>,
            done: &mut HashSet<Id>,
            out: &mut Vec<Id>,
        ) -> Result<(), Error> {
            if done.contains(&id) {
                return Ok(());
            }
            if active.len() >= MAX_SCENE_DEPTH || !active.insert(id) {
                return Err(Error::Invalid("group cycle or depth limit"));
            }
            let scene = current.scene_container(id).ok_or(Error::NotFound(id))?;
            for item in &scene.items {
                if let Some(SourceKind::Group { group }) =
                    current.source(item.source).map(|source| &source.kind)
                {
                    visit(group.scene, current, active, done, out)?;
                }
            }
            active.remove(&id);
            done.insert(id);
            out.push(id);
            Ok(())
        }
        let mut order = vec![];
        let mut active = HashSet::new();
        let mut done = HashSet::new();
        for scene in &self.groups {
            visit(scene.id, self, &mut active, &mut done, &mut order)?;
        }
        let source_geometry_unchanged=previous.is_some_and(|previous|
            previous.sources().len()==self.sources().len()&&previous.sources().iter().zip(self.sources()).all(|(old,new)|
                old.id()==new.id()&&old.kind()==new.kind()&&old.video_filters()==new.video_filters()
                    &&new.video_filters().iter().filter(|filter|filter.enabled&&matches!(filter.effect,
                        crate::VideoEffect::GaussianBlur{..}|crate::VideoEffect::CropPad{..}|crate::VideoEffect::Scroll{..}|crate::VideoEffect::ScaleFilter{..}))
                        .all(|filter|previous.preferences().plugin_enabled(filter.plugin_id())==self.preferences().plugin_enabled(filter.plugin_id()))));
        for id in order {
            let current = self.scene_container(id).ok_or(Error::NotFound(id))?;
            if previous.is_some_and(|previous|source_geometry_unchanged
                && (previous.video().width(),previous.video().height())==(self.video().width(),self.video().height())
                && previous.scene_container(id).is_some_and(|old|old.items==current.items))
            {
                continue;
            }
            let Some(bounds) = self.group_bounds(id)? else {
                continue;
            };
            let source = self.sources.iter().find(|source| matches!(&source.kind, SourceKind::Group { group } if group.scene == id)).ok_or(Error::Invalid("private group has no source owner"))?.clone();
            let SourceKind::Group { group } = &source.kind else {
                unreachable!();
            };
            let basis = group.basis();
            let owner=source.clone();let canvas=Some((self.video().width(),self.video().height()));let old_mapping=self.effective_video_filter_mapping(&owner,basis,canvas)?;
            let source = source.id;
            let mut visual = HashSet::new();
            for child in &self.scene_container(id).ok_or(Error::NotFound(id))?.items {
                if self.item_has_video(child)? {
                    visual.insert(child.id);
                }
            }
            for child in &mut self.scene_mut(id)?.items {
                if visual.contains(&child.id) {
                    child.transform.x = (f64::from(child.transform.x) - bounds.left) as f32;
                    child.transform.y = (f64::from(child.transform.y) - bounds.top) as f32;
                }
            }
            let (width, height) = self
                .group_native_dimensions(id)?
                .ok_or(Error::Invalid("nonempty group lost its video bounds"))?;
            let new_mapping=self.effective_video_filter_mapping(&owner,(width,height),canvas)?;
            let shift=if bounds.left==0.0&&bounds.top==0.0&&basis==(width,height){[0.0;2]}else{let old=old_mapping.map_point([bounds.left,bounds.top])?;let new=new_mapping.map_point([0.0;2])?;[old[0]-new[0],old[1]-new[1]]};
            for scene in self
                .collection
                .scenes
                .iter_mut()
                .chain(&mut self.collection.groups)
            {
                for item in &mut scene.items {
                    if item.source == source {
                        let (x, y) = group_point(&item.transform, basis, shift[0], shift[1]);
                        item.transform.x = x as f32;
                        item.transform.y = y as f32;
                        let anchor=match item.scale_anchor {
                            Some(anchor)=>anchor,
                            None=>crate::ScaleAnchor::new(item.transform.width,item.transform.height,basis)?,
                        };
                        let extent=anchor.extents((width,height))?;
                        item.transform.width=extent.0;item.transform.height=extent.1;
                        item.scale_anchor=Some(anchor);
                    }
                }
            }
            self.source_mut(source)?.kind = SourceKind::Group {
                group: GroupSource::new(id, width, height)?,
            };
        }
        Ok(())
    }
}

struct GroupFilterProjection{transform:Transform,exact:bool}
impl GroupFilterProjection{
    fn from_group(&self,child:&mut Transform,basis:(u32,u32))->Result<(),Error>{if self.exact{compose_group_exact(child,&self.transform,basis,false)}else{transform_from_group(child,&self.transform,basis)}}
    fn into_group(&self,child:&mut Transform,basis:(u32,u32))->Result<(),Error>{if self.exact{compose_group_exact(child,&self.transform,basis,true)}else{transform_into_group(child,&self.transform,basis)}}
}
impl Collection {
    fn group_filter_transform(&self,source:Id,transform:&Transform,basis:(u32,u32),canvas:Option<(u32,u32)>,preferences:Option<&crate::UserPreferences>)->Result<GroupFilterProjection,Error>{
        if transform.crop!=crate::Crop::default(){return Err(Error::Unavailable("Reset the group item crop before reparenting or flattening its children"));}
        let owner=self.source(source).ok_or(Error::NotFound(source))?;let mapping=owner.video_filter_mapping_with_preferences(basis,canvas,preferences)?;
        let offset=mapping.map_point([0.0;2])?;let scale=mapping.scale();
        let (x,y)=group_point(transform,basis,offset[0],offset[1]);let mut projected=transform.clone();projected.x=x as f32;projected.y=y as f32;projected.width=(f64::from(transform.width)*scale[0]) as f32;projected.height=(f64::from(transform.height)*scale[1]) as f32;
        Ok(GroupFilterProjection{transform:projected,exact:scale!=[1.0;2]})
    }
    fn ensure_group_flattenable(&self,source:Id,mut input:(u32,u32),canvas:Option<(u32,u32)>,preferences:Option<&crate::UserPreferences>)->Result<(),Error>{
        let owner=self.source(source).ok_or(Error::NotFound(source))?;
        for filter in owner.video_filters().iter().filter(|f|f.enabled&&preferences.is_none_or(|prefs|prefs.plugin_enabled(f.plugin_id()))){let output=filter.effect.output_dimensions_with_canvas(input,canvas)?;
            let clipped=match filter.effect{crate::VideoEffect::CropPad{left,top,..}=>i64::from(left)>0||i64::from(top)>0||i64::from(left)+i64::from(output.0)<i64::from(input.0)||i64::from(top)+i64::from(output.1)<i64::from(input.1),crate::VideoEffect::Scroll{..}=>output.0<input.0||output.1<input.1,_=>false};
            if clipped{return Err(Error::Unavailable("A group's filtered clipping cannot be distributed to arbitrary child transforms; reset its clipping filter before ungrouping"));}input=output;
        }Ok(())
    }
}
// A scaled group may turn a rotated child into a shear, which Transform cannot
// express. Preserve representable affine columns, reject the rest atomically.
fn compose_group_exact(child:&mut Transform,group:&Transform,basis:(u32,u32),inverse:bool)->Result<(),Error>{
    let (gx,gy)=axes(group,basis);let determinant=gx[0]*gy[1]-gy[0]*gx[1];if !determinant.is_finite()||determinant.abs()<f64::MIN_POSITIVE{return Err(Error::Invalid("singular scaled group transform"));}
    let vector=|v:[f64;2]|if inverse{[(gy[1]*v[0]-gy[0]*v[1])/determinant,(gx[0]*v[1]-gx[1]*v[0])/determinant]}else{[gx[0]*v[0]+gy[0]*v[1],gx[1]*v[0]+gy[1]*v[1]]};
    let (cx,cy)=axes(child,(1,1));let x=vector(cx);let y=vector(cy);let w=x[0].hypot(x[1]);let h=y[0].hypot(y[1]);
    if !w.is_finite()||!h.is_finite()||w<=0.0||h<=0.0||(x[0]*y[0]+x[1]*y[1]).abs()>1e-10*w*h{return Err(Error::Unavailable("This scaled and rotated group would require a shear that scene item transforms cannot represent"));}
    let point=if inverse{vector([f64::from(child.x)-f64::from(group.x),f64::from(child.y)-f64::from(group.y)])}else{let p=vector([f64::from(child.x),f64::from(child.y)]);[p[0]+f64::from(group.x),p[1]+f64::from(group.y)]};
    let sign=if child.flip_x{-1.0}else{1.0};let mut next=child.clone();next.x=point[0] as f32;next.y=point[1] as f32;next.width=w as f32;next.height=h as f32;next.rotation_degrees=(x[1]*sign).atan2(x[0]*sign).to_degrees() as f32;next.flip_y=(x[0]*y[1]-x[1]*y[0]<0.0)^next.flip_x;next.geometry(8192,8192)?;*child=next;Ok(())
}

fn axes(transform: &Transform, basis: (u32, u32)) -> ([f64; 2], [f64; 2]) {
    let angle = f64::from(transform.rotation_degrees).rem_euclid(360.0);
    let (sin, cos) = match angle {
        0.0 => (0.0, 1.0),
        90.0 => (1.0, 0.0),
        180.0 => (0.0, -1.0),
        270.0 => (-1.0, 0.0),
        _ => angle.to_radians().sin_cos(),
    };
    let sx =
        f64::from(transform.width) / f64::from(basis.0) * if transform.flip_x { -1.0 } else { 1.0 };
    let sy = f64::from(transform.height) / f64::from(basis.1)
        * if transform.flip_y { -1.0 } else { 1.0 };
    ([cos * sx, sin * sx], [-sin * sy, cos * sy])
}
fn group_point(transform: &Transform, basis: (u32, u32), x: f64, y: f64) -> (f64, f64) {
    let (xx, yy) = axes(transform, basis);
    (
        f64::from(transform.x) + xx[0] * x + yy[0] * y,
        f64::from(transform.y) + xx[1] * x + yy[1] * y,
    )
}
fn transform_from_group(
    child: &mut Transform,
    group: &Transform,
    basis: (u32, u32),
) -> Result<(), Error> {
    let (x, y) = group_point(group, basis, f64::from(child.x), f64::from(child.y));
    child.x = x as f32;
    child.y = y as f32;
    child.width = (f64::from(child.width) * f64::from(group.width) / f64::from(basis.0)) as f32;
    child.height = (f64::from(child.height) * f64::from(group.height) / f64::from(basis.1)) as f32;
    child.rotation_degrees += group.rotation_degrees;
    // OBS keeps the child's original signed scale after vec4_len. A reflected
    // group changes its pivot, but its reflection is not copied to this item.
    Ok(())
}
fn transform_into_group(
    child: &mut Transform,
    group: &Transform,
    basis: (u32, u32),
) -> Result<(), Error> {
    let (xx, yy) = axes(group, basis);
    let determinant = xx[0] * yy[1] - yy[0] * xx[1];
    if !determinant.is_finite() || determinant == 0.0 {
        return Err(Error::Invalid("singular group transform"));
    }
    let x = f64::from(child.x) - f64::from(group.x);
    let y = f64::from(child.y) - f64::from(group.y);
    child.x = ((yy[1] * x - yy[0] * y) / determinant) as f32;
    child.y = ((xx[0] * y - xx[1] * x) / determinant) as f32;
    child.width = (f64::from(child.width) * f64::from(basis.0) / f64::from(group.width)) as f32;
    child.height = (f64::from(child.height) * f64::from(basis.1) / f64::from(group.height)) as f32;
    child.rotation_degrees -= group.rotation_degrees;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, Session};
    use std::sync::Arc;
    fn color(session: &mut Session, scene: Id, name: &str, x: f32, y: f32) -> (Id, Id) {
        let item = session
            .apply(Command::AddColorSource {
                scene,
                name: name.into(),
                rgba: [255, 0, 0, 255],
            })
            .unwrap()
            .created
            .unwrap();
        let source = session
            .snapshot()
            .scene_container(scene)
            .unwrap()
            .items
            .iter()
            .find(|entry| entry.id == item)
            .unwrap()
            .source;
        session
            .apply(Command::SetColorSize {
                source,
                width: 4,
                height: 2,
            })
            .unwrap();
        let mut transform = Transform::full(4, 2);
        transform.x = x;
        transform.y = y;
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform,
            })
            .unwrap();
        (item, source)
    }
    fn grouped(session: &Session, parent: Id, item: Id) -> (Id, Id, (u32, u32)) {
        let model = session.snapshot();
        let source = model
            .scene_container(parent)
            .unwrap()
            .items
            .iter()
            .find(|entry| entry.id == item)
            .unwrap()
            .source;
        let SourceKind::Group { group } = &model.source(source).unwrap().kind else {
            panic!("group source");
        };
        (source, group.scene, group.basis())
    }
    #[test]
    fn grouping_retains_leaf_identity_order_local_geometry_and_one_undo() {
        let mut session = Session::new();
        let scene = session.preview();
        let (a, source_a) = color(&mut session, scene, "A", 10.0, 5.0);
        let (b, source_b) = color(&mut session, scene, "B", 22.0, 5.0);
        let before = session.snapshot();
        let item = session
            .apply(Command::GroupItems {
                scene,
                name: "Group".into(),
                items: vec![b, a],
            })
            .unwrap()
            .created
            .unwrap();
        let (source, private, basis) = grouped(&session, scene, item);
        assert_eq!(basis, (16, 2));
        let model = session.snapshot();
        assert_eq!(model.scenes().len(), 1);
        assert!(model.scene(private).is_none());
        assert_eq!(model.group_parent(private), Some((scene, item)));
        assert_eq!(model.scene(scene).unwrap().items[0].transform.x, 10.0);
        assert_eq!(model.scene(scene).unwrap().items[0].transform.y, 5.0);
        let children = &model.scene_container(private).unwrap().items;
        assert_eq!(
            children
                .iter()
                .map(|entry| (entry.id, entry.source, entry.transform.x))
                .collect::<Vec<_>>(),
            vec![(a, source_a, 0.0), (b, source_b, 12.0)]
        );
        session.apply(Command::Undo).unwrap();
        let restored = session.snapshot();
        assert_eq!(restored.scenes(), before.scenes());
        assert_eq!(restored.sources(), before.sources());
        assert!(restored.groups().is_empty());
        session.apply(Command::Redo).unwrap();
        assert_eq!(grouped(&session, scene, item).0, source);
        let baseline = session.snapshot();
        assert!(
            !session
                .apply(Command::RenameSource {
                    source,
                    name: "Group".into()
                })
                .unwrap()
                .changed
        );
        assert!(Arc::ptr_eq(&baseline, &session.snapshot()));
        session.apply(Command::Ungroup { scene, item }).unwrap();
        let model = session.snapshot();
        assert!(model.groups().is_empty());
        assert!(model.source(source).is_none());
        let children = model.scene(scene).unwrap().items();
        assert_eq!(
            children
                .iter()
                .map(|entry| (entry.source(), entry.transform().x, entry.transform().y))
                .collect::<Vec<_>>(),
            vec![(source_a, 10.0, 5.0), (source_b, 22.0, 5.0)]
        );
        assert_ne!(children[0].id(), a);
        assert_ne!(children[1].id(), b);
    }
    #[test]
    fn empty_group_keeps_scale_basis_without_claiming_a_pixel_and_rejects_shared_alias() {
        let mut session = Session::new();
        let scene = session.preview();
        let item = session
            .apply(Command::AddGroup {
                scene,
                name: "Empty".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let (source, private, _) = grouped(&session, scene, item);
        assert_eq!(
            session.snapshot().group_native_dimensions(private).unwrap(),
            None
        );
        let mut transform = Transform::full(1, 1);
        transform.width = 8.0;
        transform.height = 4.0;
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform,
            })
            .unwrap();
        let (leaf, _) = color(&mut session, private, "First child", 0.0, 0.0);
        assert_eq!(grouped(&session, scene, item).2, (4, 2));
        let before = session.snapshot();
        assert_eq!(before.scene(scene).unwrap().items[0].transform.width, 32.0);
        assert_eq!(before.scene(scene).unwrap().items[0].transform.height, 8.0);
        assert!(
            session
                .apply(Command::AddExistingSource { scene, source })
                .is_err()
        );
        assert!(
            session
                .apply(Command::SetAudio {
                    source,
                    gain: 0.5,
                    muted: false
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        session
            .apply(Command::RemoveItem {
                scene: private,
                item: leaf,
            })
            .unwrap();
        assert_eq!(grouped(&session, scene, item).2, (4, 2));
        assert_eq!(
            session.snapshot().group_native_dimensions(private).unwrap(),
            None
        );
        let (_, _) = color(&mut session, private, "Second child", 0.0, 0.0);
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items[0]
                .transform
                .width,
            32.0
        );
        assert_eq!(
            session.snapshot().scene(scene).unwrap().items[0]
                .transform
                .height,
            8.0
        );
    }
    #[test]
    fn duplicate_scene_owns_a_new_group_and_items_but_shares_leaf_sources() {
        let mut session = Session::new();
        let scene = session.preview();
        let (a, source) = color(&mut session, scene, "Leaf", 0.0, 0.0);
        let item = session
            .apply(Command::GroupItems {
                scene,
                name: "Group".into(),
                items: vec![a],
            })
            .unwrap()
            .created
            .unwrap();
        let (wrapper, private, _) = grouped(&session, scene, item);
        let duplicate = session
            .apply(Command::DuplicateScene {
                scene,
                name: "Copy".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let copy_item = session.snapshot().scene(duplicate).unwrap().items[0].id;
        let (copy_wrapper, copy_private, _) = grouped(&session, duplicate, copy_item);
        assert_ne!(wrapper, copy_wrapper);
        assert_ne!(private, copy_private);
        let copy = session.snapshot();
        assert_eq!(
            copy.scene_container(copy_private).unwrap().items[0].source,
            source
        );
        assert_ne!(copy.scene_container(copy_private).unwrap().items[0].id, a);
        session.apply(Command::RemoveItem { scene, item }).unwrap();
        assert!(session.snapshot().scene_container(private).is_none());
        assert!(session.snapshot().source(source).is_some());
        session
            .apply(Command::RemoveScene { scene: duplicate })
            .unwrap();
        assert!(session.snapshot().groups().is_empty());
        assert!(session.snapshot().source(source).is_none());
    }
    #[test]
    fn invalid_selection_locked_missing_cycles_and_malformed_basis_preserve_history() {
        let mut session = Session::new();
        let scene = session.preview();
        let (a, _) = color(&mut session, scene, "Leaf", 0.0, 0.0);
        let before = session.snapshot();
        for items in [vec![a, a], vec![Id::new(999)]] {
            assert!(
                session
                    .apply(Command::GroupItems {
                        scene,
                        name: "Bad".into(),
                        items
                    })
                    .is_err()
            );
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
        session
            .apply(Command::SetLocked {
                scene,
                item: a,
                locked: true,
            })
            .unwrap();
        let before = session.snapshot();
        assert!(
            matches!(session.apply(Command::GroupItems { scene, name: "Locked".into(), items: vec![a] }), Err(Error::Locked(id)) if id==a)
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        session
            .apply(Command::SetLocked {
                scene,
                item: a,
                locked: false,
            })
            .unwrap();
        let group_item = session
            .apply(Command::GroupItems {
                scene,
                name: "Group".into(),
                items: vec![a],
            })
            .unwrap()
            .created
            .unwrap();
        let (_, private, _) = grouped(&session, scene, group_item);
        let before = session.snapshot();
        assert!(
            session
                .apply(Command::AddSceneSource {
                    scene: private,
                    target: scene
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        let cloned = (*before).clone();
        cloned.validate().unwrap();
        assert_eq!(cloned, *before);
        let mut malformed = (*before).clone();
        let SourceKind::Group { group } = &mut malformed
            .collection
            .sources
            .iter_mut()
            .find(|source| matches!(source.kind, SourceKind::Group { .. }))
            .unwrap()
            .kind
        else {
            unreachable!();
        };
        group.width += 1;
        assert!(malformed.validate().is_err());
    }
    #[test]
    fn membership_commands_preserve_world_position_and_locked_rejection_is_atomic() {
        let mut session = Session::new();
        let scene = session.preview();
        let (a, _) = color(&mut session, scene, "A", 10.0, 20.0);
        let (b, _) = color(&mut session, scene, "B", 30.0, 20.0);
        let group_item = session
            .apply(Command::GroupItems {
                scene,
                name: "Group".into(),
                items: vec![a],
            })
            .unwrap()
            .created
            .unwrap();
        session
            .apply(Command::MoveIntoGroup {
                scene,
                item: b,
                group: group_item,
            })
            .unwrap();
        let (_, private, basis) = grouped(&session, scene, group_item);
        assert_eq!(basis, (24, 2));
        let model = session.snapshot();
        assert_eq!(
            model.scene_container(private).unwrap().items[1].transform.x,
            20.0
        );
        session
            .apply(Command::SetLocked {
                scene,
                item: group_item,
                locked: true,
            })
            .unwrap();
        let before = session.snapshot();
        assert!(
            session
                .apply(Command::MoveOutOfGroup {
                    scene,
                    group: group_item,
                    item: b,
                    index: 0
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&before, &session.snapshot()));
        session
            .apply(Command::SetLocked {
                scene,
                item: group_item,
                locked: false,
            })
            .unwrap();
        session
            .apply(Command::MoveOutOfGroup {
                scene,
                group: group_item,
                item: b,
                index: 0,
            })
            .unwrap();
        let model = session.snapshot();
        assert_eq!(model.scene(scene).unwrap().items[0].id, b);
        assert_eq!(model.scene(scene).unwrap().items[0].transform.x, 30.0);
        assert_eq!(grouped(&session, scene, group_item).2, (4, 2));
    }
    #[test]
    fn group_ancestor_lock_blocks_child_geometry_removal_and_reorder() {
        let mut session = Session::new();
        let scene = session.preview();
        let (a, _) = color(&mut session, scene, "Leaf", 0.0, 0.0);
        let item = session
            .apply(Command::GroupItems {
                scene,
                name: "Group".into(),
                items: vec![a],
            })
            .unwrap()
            .created
            .unwrap();
        let (_, private, _) = grouped(&session, scene, item);
        session
            .apply(Command::SetLocked {
                scene,
                item,
                locked: true,
            })
            .unwrap();
        let before = session.snapshot();
        assert!(before.container_is_locked(private));
        assert_eq!(before.item_container(a).unwrap().0, private);
        assert_eq!(before.root_scene_for_container(private), Some(scene));
        for command in [
            Command::RemoveItem {
                scene: private,
                item: a,
            },
            Command::MoveItem {
                scene: private,
                item: a,
                index: 0,
            },
            Command::SetTransform {
                scene: private,
                item: a,
                transform: Transform::full(4, 2),
            },
        ] {
            assert!(matches!(session.apply(command), Err(Error::Locked(id)) if id==item));
            assert!(Arc::ptr_eq(&before, &session.snapshot()));
        }
    }
    #[test]
    fn duplicate_maximum_utf8_group_name_has_a_bounded_valid_unique_suffix() {
        let mut session = Session::new();
        let scene = session.preview();
        let (a, _) = color(&mut session, scene, "Leaf", 0.0, 0.0);
        session
            .apply(Command::GroupItems {
                scene,
                name: "Ж".repeat(128),
                items: vec![a],
            })
            .unwrap();
        session
            .apply(Command::DuplicateScene {
                scene,
                name: "Copy".into(),
            })
            .unwrap();
        let model = session.snapshot();
        let name = model.groups()[1].name();
        assert!(name.ends_with(" 2"));
        assert_eq!(name.len(), 256);
        model.validate().unwrap();
    }
    #[test]
    fn obs_group_helper_literal_decomposition_retains_child_sign_not_group_reflection() {
        // Independent numeric oracle for obs-scene.c get_ungrouped_transform:
        // R90 * diag(-2,3), then translate(100,40). Its two transformed scale
        // vectors have lengths4 and3; vec4_len restores the CHILD signs only.
        let mut group = Transform::full(4, 2);
        group.width = 8.0;
        group.height = 6.0;
        group.x = 100.0;
        group.y = 40.0;
        group.rotation_degrees = 90.0;
        group.flip_x = true;
        let mut child = Transform::full(2, 1);
        child.x = 1.0;
        child.y = 1.0;
        child.rotation_degrees = 30.0;
        transform_from_group(&mut child, &group, (4, 2)).unwrap();
        assert_eq!(
            (
                child.x,
                child.y,
                child.width,
                child.height,
                child.rotation_degrees
            ),
            (97.0, 38.0, 4.0, 3.0, 120.0)
        );
        assert!(!child.flip_x && !child.flip_y);
        let geometry = child.geometry(2, 1).unwrap();
        for (actual, expected) in geometry.axis_x.into_iter().zip([-2.0, 3.4641016151377544]) {
            assert!((actual - expected).abs() < 0.000001);
        }
        for (actual, expected) in geometry.axis_y.into_iter().zip([-2.598076211353316, -1.5]) {
            assert!((actual - expected).abs() < 0.000001);
        }
        transform_into_group(&mut child, &group, (4, 2)).unwrap();
        assert_eq!(
            (
                child.x,
                child.y,
                child.width,
                child.height,
                child.rotation_degrees
            ),
            (1.0, 1.0, 2.0, 1.0, 30.0)
        );
        assert!(!child.flip_x && !child.flip_y);
        child.flip_x = true;
        transform_from_group(&mut child, &group, (4, 2)).unwrap();
        assert!(
            child.flip_x,
            "only preexisting child reflection is retained"
        );
    }
    #[test]
    fn positioned_mixed_group_never_assigns_visual_transforms_to_audio_children() {
        let mut session = Session::new();
        let scene = session.preview();
        let (video, _) = color(&mut session, scene, "Video", 10.0, 20.0);
        let audio = session
            .apply(Command::AddAudioCaptureSource {
                scene,
                name: "Audio".into(),
                binding: crate::AudioCaptureBinding::new(
                    crate::AudioFlow::Input,
                    crate::AudioTarget::Default,
                    false,
                    false,
                )
                .unwrap(),
            })
            .unwrap()
            .created
            .unwrap();
        let item = session
            .apply(Command::GroupItems {
                scene,
                name: "Mixed".into(),
                items: vec![video, audio],
            })
            .unwrap()
            .created
            .unwrap();
        let (_, private, _) = grouped(&session, scene, item);
        assert_eq!(
            session
                .snapshot()
                .item_container(audio)
                .unwrap()
                .1
                .transform(),
            &Transform::full(1, 1)
        );
        let mut transform = session
            .snapshot()
            .item_container(item)
            .unwrap()
            .1
            .transform()
            .clone();
        transform.width *= 2.0;
        transform.height *= 2.0;
        session
            .apply(Command::SetTransform {
                scene,
                item,
                transform,
            })
            .unwrap();
        session
            .apply(Command::MoveOutOfGroup {
                scene,
                group: item,
                item: audio,
                index: 1,
            })
            .unwrap();
        assert_eq!(
            session
                .snapshot()
                .item_container(audio)
                .unwrap()
                .1
                .transform(),
            &Transform::full(1, 1)
        );
        session
            .apply(Command::MoveIntoGroup {
                scene,
                item: audio,
                group: item,
            })
            .unwrap();
        assert_eq!(session.snapshot().item_container(audio).unwrap().0, private);
        session.apply(Command::Ungroup { scene, item }).unwrap();
        let model = session.snapshot();
        let audio = model
            .scene(scene)
            .unwrap()
            .items
            .iter()
            .find(|item| {
                matches!(
                    model.source(item.source).unwrap().kind(),
                    SourceKind::AudioCapture { .. }
                )
            })
            .unwrap();
        assert_eq!(audio.transform(), &Transform::full(1, 1));
        model.validate().unwrap();
    }
    #[test]
    fn compound_membership_preserves_world_position_and_rejects_cross_root_and_cycles() {
        let mut session = Session::new();
        let scene = session.preview();
        let (leaf, _) = color(&mut session, scene, "Leaf", 10.0, 20.0);
        let a = session
            .apply(Command::GroupItems {
                scene,
                name: "A".into(),
                items: vec![leaf],
            })
            .unwrap()
            .created
            .unwrap();
        let (_, from, _) = grouped(&session, scene, a);
        let b = session
            .apply(Command::AddGroup {
                scene,
                name: "B".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let (_, into, _) = grouped(&session, scene, b);
        let before = session.snapshot();
        session
            .apply(Command::MoveGroupItem {
                from,
                into,
                item: leaf,
                index: 0,
            })
            .unwrap();
        assert_eq!(session.snapshot().item_container(leaf).unwrap().0, into);
        let model = session.snapshot();
        let wrapper = model.item_container(b).unwrap().1;
        assert_eq!((wrapper.transform.x, wrapper.transform.y), (10.0, 20.0));
        assert_eq!(
            (
                model.item_container(leaf).unwrap().1.transform.x,
                model.item_container(leaf).unwrap().1.transform.y
            ),
            (0.0, 0.0)
        );
        session.apply(Command::Undo).unwrap();
        assert_eq!(session.snapshot().collection(), before.collection());
        let baseline = session.snapshot();
        assert!(
            session
                .apply(Command::MoveGroupItem {
                    from: scene,
                    into: from,
                    item: a,
                    index: 0
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&baseline, &session.snapshot()));
        let other = session
            .apply(Command::AddScene {
                name: "Other".into(),
            })
            .unwrap()
            .created
            .unwrap();
        let baseline = session.snapshot();
        assert!(
            session
                .apply(Command::MoveGroupItem {
                    from,
                    into: other,
                    item: leaf,
                    index: 0
                })
                .is_err()
        );
        assert!(Arc::ptr_eq(&baseline, &session.snapshot()));
    }
}
