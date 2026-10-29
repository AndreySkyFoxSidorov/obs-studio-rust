//! Source clipboard values and atomic reference/duplicate insertion.
use crate::{Error, Id, MAX_ITEMS, MAX_SCENES, MAX_SOURCES, SceneItem, SourceKind, StudioSnapshot, Transform};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePasteItem {
    #[serde(default,skip_serializing_if="Option::is_none")]
    scale_anchor:Option<crate::ScaleAnchor>,
    #[serde(default,skip_serializing_if="crate::SourceItemColorTag::is_clear")]
    pub color_tag:crate::SourceItemColorTag,
    #[serde(default,skip_serializing_if="crate::ItemCompositing::is_default")]
    pub compositing: crate::ItemCompositing,
    pub source: Id,
    pub transform: Transform,
    pub visible: bool,
    pub basis: (u32, u32),
}

impl SourcePasteItem {
    /// An owned transform copy retains its original stable scale when copied
    /// across Scene canvases; legacy copies infer it from their captured basis.
    pub fn transform_for_basis(&self,basis:(u32,u32))->Result<Transform,Error>{
        let anchor=match self.scale_anchor {
            Some(anchor)=>{
                if !anchor.agrees(&self.transform,self.basis)?{return Err(Error::Invalid("Scene scale anchor does not match its stored extent and basis."));}
                anchor
            }
            None=>crate::ScaleAnchor::new(self.transform.width,self.transform.height,self.basis)?,
        };
        let extent=anchor.extents(basis)?;let mut transform=self.transform.clone();transform.width=extent.0;transform.height=extent.1;Ok(transform)
    }
}
impl StudioSnapshot {
    pub fn source_copy_value(&self, item: Id) -> Result<SourcePasteItem, Error> {
        let (_, item) = self.item_container(item).ok_or(Error::NotFound(item))?;
        let source = self.source(item.source()).ok_or(Error::NotFound(item.source()))?;
        Ok(SourcePasteItem {
            scale_anchor:item.scale_anchor,
            color_tag:item.color_tag(),
            compositing: item.compositing(),
            source: source.id(),
            transform: item.transform().clone(),
            visible: item.visible(),
            basis: self.source_video_dimensions(source.kind())?.unwrap_or((1, 1)),
        })
    }

    pub fn check_source_paste(&self, scene: Id, items: &[SourcePasteItem], duplicate: bool) -> Result<(), Error> {
        self.scene_container(scene).ok_or(Error::NotFound(scene))?;
        self.ensure_container_unlocked(scene)?;
        if items.is_empty() || items.len() > MAX_ITEMS {
            return Err(Error::Limit("Source clipboard is empty or exceeds the item limit"));
        }
        let count: usize = self.scenes().iter().chain(self.groups()).map(|scene| scene.items().len()).sum();
        if count.checked_add(items.len()).is_none_or(|count| count > MAX_ITEMS) {
            return Err(Error::Limit("Pasted sources exceed the scene item limit"));
        }
        for item in items {
            let source = self.source(item.source).ok_or(Error::NotFound(item.source))?;
            if !duplicate && matches!(source.kind(), SourceKind::Group { .. }) {
                return Err(Error::Invalid("Use Paste Duplicate to copy a private group"));
            }
            if item.basis.0 == 0 || item.basis.1 == 0 || item.basis.0 > 8192 || item.basis.1 > 8192 {
                return Err(Error::Invalid("Invalid copied source dimensions"));
            }
            if let Some(anchor)=item.scale_anchor {
                if !matches!(source.kind(),SourceKind::Scene{..}|SourceKind::Group{..}){return Err(Error::Invalid("Only Scene and Group references may have scale anchors."));}
                if !anchor.agrees(&item.transform,item.basis)?{return Err(Error::Invalid("Scene scale anchor does not match its stored extent and basis."));}
            }
            let frame=self.effective_filtered_video_dimensions(source,item.basis,Some((self.video().width(),self.video().height())))?;
            item.transform.geometry_for_frame(item.basis.0,item.basis.1,frame.0,frame.1)?;
            if duplicate {
                self.check_duplicate_provider(source.id())?;
            }
        }
        Ok(())
    }

    fn check_duplicate_provider(&self, id: Id) -> Result<(), Error> {
        let source = self.source(id).ok_or(Error::NotFound(id))?;
        for provider in source.audio_filters().iter().map(|filter| filter.plugin_id())
            .chain(source.video_filters().iter().map(|filter| filter.plugin_id())) {
            if !self.preferences().plugin_enabled(provider) {
                return Err(Error::Unavailable("Enable the source's filter modules before duplicating it"));
            }
        }
        Ok(())
    }

    fn duplicate_clipboard_source(&mut self, original: Id, depth: usize) -> Result<Id, Error> {
        if depth > crate::MAX_SCENE_DEPTH || self.sources().len() >= MAX_SOURCES {
            return Err(Error::Limit("Duplicated source graph exceeds its limit"));
        }
        self.check_duplicate_provider(original)?;
        let mut source = self.source(original).ok_or(Error::NotFound(original))?.clone();
        let base = source.name().to_owned();
        let mut chosen = None;
        for suffix in 2..=MAX_SOURCES + MAX_SCENES + 2 {
            let tail = format!(" {suffix}");
            let mut end = base.len().min(256 - tail.len());
            while !base.is_char_boundary(end) { end -= 1; }
            let name = format!("{}{tail}", &base[..end]);
            if self.sources().iter().all(|source| source.name() != name)
                && self.scenes().iter().chain(self.groups()).all(|scene| scene.name() != name) {
                chosen = Some(name);
                break;
            }
        }
        let name = chosen.ok_or(Error::Limit("Cannot allocate a unique source name"))?;
        if let Some(container) = source.kind().nested_scene() {
            if self.scenes().len() + self.groups().len() >= MAX_SCENES {
                return Err(Error::Limit("Duplicated source exceeds the scene limit"));
            }
            let count: usize = self.scenes().iter().chain(self.groups()).map(|scene| scene.items().len()).sum();
            let original = self.scene_container(container).ok_or(Error::NotFound(container))?;
            if count.checked_add(original.items().len()).is_none_or(|count| count > MAX_ITEMS) {
                return Err(Error::Limit("Duplicated source exceeds the item limit"));
            }
            let mut copied = original.clone();
            copied.id = self.issue()?;
            copied.name = name.clone();
            for item in &mut copied.items {
                item.id = self.issue()?;
                if matches!(self.source(item.source).map(|source| source.kind()), Some(SourceKind::Group { .. })) {
                    item.source = self.duplicate_clipboard_source(item.source, depth + 1)?;
                }
            }
            match &mut source.kind {
                SourceKind::Scene { scene } => {
                    *scene = copied.id;
                    self.collection.scenes.push(copied);
                }
                SourceKind::Group { group } => {
                    let basis = group.basis();
                    *group = crate::GroupSource::new(copied.id, basis.0, basis.1)?;
                    self.collection.groups.push(copied);
                }
                _ => unreachable!(),
            }
        }
        let filters = source.video_filters.iter().map(crate::VideoFilter::as_new).collect();
        source.id = self.issue()?;
        source.playback_hotkeys=crate::SourcePlaybackHotkeys::default();
        source.audio_hotkeys=crate::SourceAudioHotkeys::default();
        source.name = name;
        source.video_filters.clear();
        let id = source.id;
        self.collection.sources.push(source);
        self.set_video_filters(id, filters)?;
        Ok(id)
    }

    pub(super) fn paste_sources(&mut self, scene: Id, items: Vec<SourcePasteItem>, duplicate: bool) -> Result<Option<Id>, Error> {
        self.check_source_paste(scene, &items, duplicate)?;
        let mut selected = None;
        for copied in items {
            let source = if duplicate { self.duplicate_clipboard_source(copied.source, 0)? } else { copied.source };
            let dimensions = self.source_video_dimensions(self.source(source).ok_or(Error::NotFound(source))?.kind())?;
            let referenced=matches!(self.source(source).ok_or(Error::NotFound(source))?.kind(),SourceKind::Scene{..}|SourceKind::Group{..});
            let scale_anchor=if referenced {match copied.scale_anchor{Some(anchor)=>Some(anchor),None=>Some(crate::ScaleAnchor::new(copied.transform.width,copied.transform.height,copied.basis)?)}}else{None};
            let transform = if let Some((width, height)) = dimensions {
                let transform = copied.transform_for_basis((width,height))?;
                let frame=self.effective_filtered_video_dimensions(self.source(source).ok_or(Error::NotFound(source))?,(width,height),Some((self.video().width(),self.video().height())))?;
                transform.geometry_for_frame(width,height,frame.0,frame.1)?;
                transform
            } else { Transform::full(1, 1) };
            let id = self.issue()?;
            self.scene_mut(scene)?.items.push(SceneItem { scale_anchor, color_tag:copied.color_tag, id, source, visible: copied.visible, locked: false, transform, compositing: copied.compositing });
            selected = Some(id);
        }
        Ok(selected)
    }
}
