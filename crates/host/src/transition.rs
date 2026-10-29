//! Single-owner output transitions. Authored scenes/settings stay in core;
//! private Studio layouts, retained source references and clocks are runtime.
//! OBS references: OBSBasic_Transitions.cpp / OBSBasic_StudioMode.cpp and
//! libobs/obs-source-transition.c. No filesystem, decoded pixels or OS clocks.
mod projector_metadata;
pub(crate) use projector_metadata::ProjectionMetadata;
use serde::Serialize;
use skylive_core::{
    Id, MAX_SOURCES, QuickTransitionId, Scene, SceneItem, Session, Source, StudioSnapshot,
    TransitionKind, TransitionRef, TransitionSelection,
};
use skylive_media::MediaTime;
use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
mod manual;
mod scene_graph;
mod output_source;
use manual::ManualClock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransitionError {
    Closed,
    NotFound(Id),
    Busy,
    NotStudio,
    InvalidPosition(u16),
    UnknownQuick(QuickTransitionId),
    ForeignSession { expected: u64, actual: u64 },
    Limit(&'static str),
    IdentityExhausted,
    TimeOverflow,
    AutoTextRetry,
    Media(skylive_media::MediaError),
    Engine(skylive_core::Error),
}
impl TransitionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Closed => "transition_closed",
            Self::NotFound(_) => "transition_not_found",
            Self::Busy => "transition_busy",
            Self::NotStudio => "transition_not_studio",
            Self::InvalidPosition(_) => "transition_invalid_position",
            Self::UnknownQuick(_) => "transition_quick_not_found",
            Self::ForeignSession { .. } => "transition_foreign_session",
            Self::Limit(_) => "transition_limit",
            Self::IdentityExhausted => "transition_identity_exhausted",
            Self::TimeOverflow => "transition_time_overflow",
            Self::AutoTextRetry => "transition_auto_text_retry",
            Self::Media(_) => "transition_media",
            Self::Engine(error) => error.code(),
        }
    }
}
impl fmt::Display for TransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("transition owner session is closed"),
            Self::NotFound(id) => write!(formatter, "transition entity {} not found", id.get()),
            Self::Busy => formatter.write_str("a Studio transition is already active"),
            Self::NotStudio => formatter.write_str("this transition control requires Studio Mode"),
            Self::InvalidPosition(value) => write!(
                formatter,
                "manual transition position {value} is outside 0..=1023"
            ),
            Self::UnknownQuick(id) => write!(formatter, "quick transition {} not found", id.get()),
            Self::ForeignSession { expected, actual } => write!(
                formatter,
                "frozen scene belongs to session {actual}, expected {expected}"
            ),
            Self::Limit(detail) => formatter.write_str(detail),
            Self::IdentityExhausted => formatter.write_str("transition identity exhausted"),
            Self::TimeOverflow => {
                formatter.write_str("transition end exceeds supported nanoseconds")
            }
            Self::AutoTextRetry=>formatter.write_str("automatic text is being prepared"),
            Self::Engine(error) => error.fmt(formatter),
            Self::Media(error) => error.fmt(formatter),
        }
    }
}
impl std::error::Error for TransitionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Engine(error) => Some(error),
            Self::Media(error) => Some(error),
            _ => None,
        }
    }
}
impl From<skylive_core::Error> for TransitionError {
    fn from(error: skylive_core::Error) -> Self {
        Self::Engine(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneKey {
    Live(Id),
    Frozen { scene: Id, instance: u64 },
}

/// Immutable layout and its source-size basis. Retained sources are metadata
/// fallbacks for removed canonical references, never an image cache. Each
/// instance admits at most MAX_SOURCES distinct source references.
#[derive(Clone, Debug)]
pub struct SceneInstance {
    session: u64,
    instance: u64,
    layout: Arc<Scene>,
    root_source: Option<Arc<Source>>,
    retained: HashMap<Id, Arc<Source>>,
    projection:Option<Arc<projector_metadata::View>>,
    nested: HashMap<Id, Arc<Scene>>,
    closure_error: Option<TransitionError>,
    capture_dimensions: HashMap<Id, (u32, u32)>,
    refreshed_revision: u64,
    preview_layout: bool,
    normalized_group_preview: bool,
    filter_preview: bool,
    // Auto-text generations used to construct a projected interaction draft.
    // They retain the raster lease until the draft is retired.
    auto_text_draft:Vec<crate::text::auto_worker::Snapshot>,
    auto_authored_root:Option<Arc<Scene>>,
    preview_canvas: Option<(u32,u32)>,
    // Project base canvas at capture, distinct from an isolated preview viewport.
    filter_canvas:(u32,u32),
}
impl SceneInstance {
    pub fn session(&self) -> u64 {
        self.session
    }
    pub fn instance(&self) -> u64 {
        self.instance
    }
    pub fn scene(&self) -> Id {
        self.layout.id()
    }
    pub fn source_count(&self) -> usize {
        self.projection.as_ref().map_or_else(||self.retained.len()+usize::from(self.root_source.is_some()),|view|view.metadata.model.sources().len())
    }
}

#[derive(Clone, Debug)]
pub enum FrameScene {
    Live(Id),
    Frozen(Arc<SceneInstance>),
}
impl FrameScene {
    /// Apply a derived auto-text projection to a frozen scene only while its
    /// private root layout and root source still match the authored snapshot.
    /// Shared nested references continue to use the projected current model.
    /// If the root has changed, silently substituting current transforms would
    /// violate the frozen transition, so the caller must keep this path closed.
    pub(crate) fn with_auto_projection(&self,authored:&StudioSnapshot,projected:&StudioSnapshot,keep_frozen:bool)->Result<Self,TransitionError>{
        let Self::Frozen(view)=self else{return Ok(self.clone());};
        if !view.auto_text_draft.is_empty(){
            let pinned=view.auto_text_draft.iter().all(|snapshot|snapshot.is_pinned()&&snapshot.is_live());
            if keep_frozen&&pinned{return Ok(self.clone());}
            if !view.auto_text_draft.iter().all(|snapshot|snapshot.matches_model(view.session,authored)){
                if pinned{return Ok(self.clone());}
                return Err(TransitionError::Engine(skylive_core::Error::Unavailable("Automatic text draft generation changed")));
            }
            if keep_frozen||view.preview_layout{return Ok(self.clone());}
            let has_group=view.retained.values().any(|source|matches!(source.kind(),skylive_core::SourceKind::Group{..}));
            if !has_group {
                let mut result=self.clone();if let Self::Frozen(instance)=&mut result{Arc::make_mut(instance).auto_text_draft.clear();}return Ok(result);
            }
            if view.auto_authored_root.as_deref()!=authored.scene(view.scene()){
                return Err(TransitionError::Engine(skylive_core::Error::Unavailable("Automatic text in a changed frozen group requires a new transition")));
            }
            let layout=projected.scene(view.scene()).ok_or(TransitionError::NotFound(view.scene()))?;
            let mut result=self.clone();if let Self::Frozen(instance)=&mut result{
                let instance=Arc::make_mut(instance);instance.layout=Arc::new(layout.clone());instance.auto_text_draft.clear();
                for (id,size) in &mut instance.capture_dimensions{if let Some(source)=projected.source(*id){if let Some(next)=projected.source_video_dimensions(source.kind())?{*size=next;}}}
            }return Ok(result);
        }
        if view.preview_layout||view.normalized_group_preview || authored.scene(view.scene())!=Some(view.layout.as_ref())
            || authored.scene_source(view.scene())!=view.root_source.as_deref(){
            return Err(TransitionError::Engine(skylive_core::Error::Unavailable("Automatic text cannot yet project a changed frozen root")));
        }
        let layout=projected.scene(view.scene()).ok_or(TransitionError::NotFound(view.scene()))?;
        let mut result=self.clone();
        if let Self::Frozen(instance)=&mut result {
            let instance=Arc::make_mut(instance);
            instance.layout=Arc::new(layout.clone());
            for (id,size) in &mut instance.capture_dimensions {
                if let Some(source)=projected.source(*id){
                    if let Some(next)=projected.source_video_dimensions(source.kind())?{*size=next;}
                }
            }
        }
        Ok(result)
    }
    /// Resource scope only: an item-transform draft retains the exact live
    /// scene's source set/assets. Full render identity remains its unique key.
    pub fn resource_key(&self) -> SceneKey {
        match self {
            Self::Frozen(view) if view.preview_layout => SceneKey::Live(view.scene()),
            _ => self.key(),
        }
    }
    pub fn preview_video_filters(session:u64,current:&StudioSnapshot,target:skylive_core::VideoFilterPreviewTarget,
        dimensions:(u32,u32))->Result<Self,TransitionError> {
        let (scene,layout)=match target {
            skylive_core::VideoFilterPreviewTarget::Scene(scene)=>(scene,None),
            skylive_core::VideoFilterPreviewTarget::Source(source)=>{
                let layout=current.isolated_video_source_layout(source)?;
                (layout.id(),Some(layout))
            }
        };
        let mut captured=Self::capture(session,current,scene,None)?;
        if let Self::Frozen(instance)=&mut captured {
            let instance=Arc::make_mut(instance);
            instance.preview_layout=true;
            instance.filter_preview=true;
            if let Some(layout)=layout {
                instance.layout=Arc::new(layout);
                instance.root_source=None;
                instance.preview_canvas=Some(dimensions);
                if let skylive_core::VideoFilterPreviewTarget::Source(source)=target {
                    let value=current.source(source).ok_or(TransitionError::NotFound(source))?;
                    if let Some(basis)=current.source_video_dimensions(value.kind())? {instance.capture_dimensions.insert(source,basis);}
                }
            }
        }
        Ok(captured)
    }
    pub fn is_filter_preview(&self)->bool {matches!(self,Self::Frozen(value) if value.filter_preview)}
    pub(crate) fn auto_text_snapshots(&self)->Option<&[crate::text::auto_worker::Snapshot]>{
        match self{Self::Frozen(value) if !value.auto_text_draft.is_empty()=>Some(&value.auto_text_draft),_=>None}
    }
    pub fn canvas_dimensions(&self,current:&StudioSnapshot)->(u32,u32) {
        match self {Self::Frozen(value)=>value.preview_canvas.unwrap_or(value.filter_canvas),Self::Live(_)=>(current.video().width(),current.video().height())}
    }
    pub fn filter_canvas_dimensions(&self,current:&StudioSnapshot)->(u32,u32){match self{Self::Frozen(value)=>value.filter_canvas,Self::Live(_)=>(current.video().width(),current.video().height())}}
    pub fn preview_transform(
        session: u64,
        current: &StudioSnapshot,
        scene: Id,
        item: Id,
        transform: skylive_core::Transform,
    ) -> Result<Self, TransitionError> {
        let layout = current.scene_with_item_transform_preview(scene, item, transform)?;
        let mut captured = Self::capture(session, current, scene,None)?;
        if let Self::Frozen(instance) = &mut captured {
            let instance = Arc::make_mut(instance);
            instance.layout = Arc::new(layout);
            instance.preview_layout = true;
        }
        Ok(captured)
    }
    pub fn preview_transforms(session:u64,current:&StudioSnapshot,scene:Id,container:Id,transforms:&[(Id,skylive_core::Transform)])->Result<Self,TransitionError> {
        let draft=current.snapshot_with_item_transforms_preview(container,transforms)?;
        let mut captured=Self::capture(session,&draft,scene,None)?;
        if let Self::Frozen(instance)=&mut captured {
            let instance=Arc::make_mut(instance);
            instance.preview_layout=true;
            instance.normalized_group_preview=true;
        }
        Ok(captured)
    }
    pub(crate) fn preview_transforms_with_auto(session:u64,current:&StudioSnapshot,scene:Id,container:Id,
        transforms:&[(Id,skylive_core::Transform)],auto:&[crate::text::auto_worker::Snapshot])->Result<Self,TransitionError>{
        let mut captured=Self::preview_transforms(session,current,scene,container,transforms)?;
        if let Self::Frozen(instance)=&mut captured {Arc::make_mut(instance).auto_text_draft=auto.to_vec();}
        Ok(captured)
    }
    pub fn scene(&self) -> Id {
        match self {
            Self::Live(id) => *id,
            Self::Frozen(view) => view.layout.id(),
        }
    }
    pub fn key(&self) -> SceneKey {
        match self {
            Self::Live(id) => SceneKey::Live(*id),
            Self::Frozen(view) => SceneKey::Frozen {
                scene: view.layout.id(),
                instance: view.instance,
            },
        }
    }
    pub fn validate_session(&self, session: u64) -> Result<(), TransitionError> {
        if let Self::Frozen(view) = self {
            if view.session != session {
                return Err(TransitionError::ForeignSession {
                    expected: session,
                    actual: view.session,
                });
            }
        }
        Ok(())
    }
    pub fn items<'a>(
        &'a self,
        current: &'a StudioSnapshot,
    ) -> Result<&'a [SceneItem], TransitionError> {
        match self {
            Self::Live(id) => current
                .scene(*id)
                .map(Scene::items)
                .ok_or(TransitionError::NotFound(*id)),
            Self::Frozen(view) => Ok(view.layout.items()),
        }
    }
    pub fn source<'a>(&'a self, current: &'a StudioSnapshot, id: Id) -> Option<&'a Source> {
        if let Self::Frozen(view)=self {
            // Frozen auto-text endpoints retain their exact text and group
            // metadata. Unrelated source kinds continue to use live references.
            let pinned=view.auto_text_draft.iter().any(|snapshot|snapshot.is_pinned());
            if pinned && (view.auto_text_draft.iter().any(|snapshot|snapshot.source()==id)
                ||view.retained.get(&id).is_some_and(|source|matches!(source.kind(),skylive_core::SourceKind::Group{..}))){
                if let Some(source)=view.retained.get(&id){return Some(source.as_ref());}
            }
            if view.normalized_group_preview {
                if let Some(source)=view.retained.get(&id).filter(|source|matches!(source.kind(),skylive_core::SourceKind::Group{..})) {return Some(source.as_ref());}
            }
        }
        current.source(id).or_else(|| match self {
            Self::Live(_) => None,
            Self::Frozen(view) => view.retained.get(&id).map(Arc::as_ref).or_else(||view.projection.as_ref().and_then(|pool|pool.metadata.model.source(id))).or_else(|| {
                view.root_source
                    .as_deref()
                    .filter(|source| source.id() == id)
            }),
        })
    }
    /// Frozen transforms store scaled full-source extents; renderers rescale
    /// those extents by current/captured dimensions to preserve OBS item scale
    /// when a shared live source's intrinsic dimensions change.
    /// A private root copies its own source properties; nested source refs stay live.
    pub fn root_scene_source<'a>(&'a self, current: &'a StudioSnapshot) -> Option<&'a Source> {
        match self {
            Self::Live(scene) => current.scene_source(*scene),
            Self::Frozen(view) => view.root_source.as_deref().or_else(||view.projection.as_ref().filter(|_|view.preview_canvas.is_none()).and_then(|pool|pool.metadata.model.scene_source(view.scene()))),
        }
    }
    pub fn captured_dimensions(&self, source: Id) -> Option<(u32, u32)> {
        match self {
            Self::Live(_) => None,
            Self::Frozen(view) => view.capture_dimensions.get(&source).copied().or_else(||view.projection.as_ref().and_then(|pool|pool.metadata.dimensions.get(&source).copied())),
        }
    }
    fn capture(session: u64, current: &StudioSnapshot, scene: Id,lifetime:Option<&skylive_core::SessionLifetime>) -> Result<Self, TransitionError> {
        let authored_root=current.scene(scene).cloned().map(Arc::new);
        let has_auto=current.sources().iter().any(|source|matches!(source.kind(),skylive_core::SourceKind::Text{settings} if settings.auto_extents()));
        let snapshots=if has_auto{crate::text::auto_worker::shared_snapshots(session,current)
            .map_err(|error|match error{crate::text::auto_worker::AcquireError::Pending=>TransitionError::AutoTextRetry,
                crate::text::auto_worker::AcquireError::Unavailable(_)=>TransitionError::Engine(skylive_core::Error::Unavailable("Automatic text preparation failed"))})?}else{Vec::new()};
        let mut projected=if has_auto{Some(current.clone())}else{None};
        if let Some(model)=&mut projected {
            *model=crate::text::auto_worker::project_all(session,model,&snapshots)
                .map_err(|error|if snapshots.iter().any(|snapshot|!snapshot.is_live()){TransitionError::AutoTextRetry}else{TransitionError::Engine(error)})?;
        }
        let current=projected.as_ref().unwrap_or(current);
        let layout = current
            .scene(scene)
            .ok_or(TransitionError::NotFound(scene))?;
        let root_source = current.scene_source(scene).cloned().map(Arc::new);
        let mut retained = HashMap::new();
        let mut capture_dimensions = HashMap::new();
        let mut nested = HashMap::new();
        let mut queue = vec![scene];
        let mut visited = std::collections::HashSet::new();
        while let Some(id) = queue.pop() {
            if !visited.insert(id) {
                continue;
            }
            let next = current
                .scene_container(id)
                .ok_or(TransitionError::NotFound(id))?;
            if id != scene {
                nested.insert(id, Arc::new(next.clone()));
            }
            for item in next.items() {
                if retained.contains_key(&item.source()) {
                    continue;
                }
                if retained.len() + usize::from(root_source.is_some()) == MAX_SOURCES {
                    return Err(TransitionError::Limit(
                        "private scene source-reference limit exceeded",
                    ));
                }
                let source = current
                    .source(item.source())
                    .ok_or(TransitionError::NotFound(item.source()))?;
                if let Some(dimensions) = current.source_video_dimensions(source.kind())? {
                    capture_dimensions.insert(source.id(), dimensions);
                }
                if let Some(target) = source.kind().nested_scene() {
                    queue.push(target);
                }
                retained.insert(source.id(), Arc::new(source.clone()));
            }
        }
        let instance = issue_instance(&NEXT_INSTANCE)?;
        Ok(Self::Frozen(Arc::new(SceneInstance {
            session,
            instance,
            layout: Arc::new(layout.clone()),
            root_source,
            retained,
            projection:None,
            nested,
            closure_error: None,
            capture_dimensions,
            refreshed_revision: current.revision(),
            preview_layout: false,
            normalized_group_preview: false,
            filter_preview: false,
            auto_text_draft:snapshots.iter().map(|snapshot|match lifetime{Some(lifetime)=>snapshot.pin(lifetime).ok_or(TransitionError::Closed),None=>Ok(snapshot.clone())}).collect::<Result<Vec<_>,_>>()?,
            auto_authored_root:if has_auto{authored_root}else{None},
            preview_canvas: None,
            filter_canvas:(current.video().width(),current.video().height()),
        })))
    }
    fn refresh(&mut self, current: &StudioSnapshot) {
        let Self::Frozen(view) = self else {
            return;
        };
        if view.projection.is_some()||view.refreshed_revision == current.revision() {
            return;
        }
        let refreshed = self.refreshed_closure(current);
        let Self::Frozen(view) = self else {
            unreachable!()
        };
        let view = Arc::make_mut(view);
        match refreshed {
            Ok((retained, nested)) => {
                view.retained = retained;
                view.nested = nested;
                view.closure_error = None;
            }
            Err(error) => view.closure_error = Some(error),
        }
        view.refreshed_revision = current.revision();
    }
}

fn issue_instance(serial: &AtomicU64) -> Result<u64, TransitionError> {
    serial
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| TransitionError::IdentityExhausted)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKey {
    Scene(SceneKey),
    Configured {
        from: Option<SceneKey>,
        to: Option<SceneKey>,
        kind: TransitionKind,
        logical_scene: Id,
        duration_ns: u64,
        epoch: u64,
    },
    Fade {
        from: SceneKey,
        to: SceneKey,
        duration_ns: u64,
        epoch: u64,
    },
    Black {
        logical_scene: Id,
        epoch: u64,
    },
    SceneAlias {
        scene: SceneKey,
        logical_scene: Id,
        epoch: u64,
    },
    SingleFade {
        scene: SceneKey,
        logical_scene: Id,
        incoming: bool,
        duration_ns: u64,
        epoch: u64,
    },
    Manual {
        from: Option<SceneKey>,
        to: Option<SceneKey>,
        kind: TransitionKind,
        logical_scene: Id,
        epoch: u64,
    },
    Directional {
        from: Option<SceneKey>,
        to: Option<SceneKey>,
        kind: TransitionKind,
        logical_scene: Id,
        duration_ns: u64,
        epoch: u64,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionKey {
    Scene(SceneKey),
    Configured {
        from: Option<SceneKey>,
        to: Option<SceneKey>,
        kind: TransitionKind,
        logical_scene: Id,
        elapsed_ns: u64,
        duration_ns: u64,
        epoch: u64,
    },
    Fade {
        from: SceneKey,
        to: SceneKey,
        elapsed_ns: u64,
        duration_ns: u64,
        epoch: u64,
    },
    Black {
        logical_scene: Id,
        epoch: u64,
    },
    SceneAlias {
        scene: SceneKey,
        logical_scene: Id,
        epoch: u64,
    },
    SingleFade {
        scene: SceneKey,
        logical_scene: Id,
        incoming: bool,
        elapsed_ns: u64,
        duration_ns: u64,
        epoch: u64,
    },
    Manual {
        from: Option<SceneKey>,
        to: Option<SceneKey>,
        kind: TransitionKind,
        logical_scene: Id,
        progress_num: u64,
        progress_den: u64,
        epoch: u64,
        next_after_ns: Option<u64>,
    },
    Directional {
        from: Option<SceneKey>,
        to: Option<SceneKey>,
        kind: TransitionKind,
        logical_scene: Id,
        elapsed_ns: u64,
        duration_ns: u64,
        epoch: u64,
    },
}
#[derive(Clone, Debug)]
pub enum FrameComposition {
    Scene(FrameScene),
    Configured {
        from: Option<FrameScene>,
        to: Option<FrameScene>,
        kind: TransitionKind,
        logical_scene: Id,
        elapsed_ns: u64,
        duration_ns: u64,
        epoch: u64,
    },
    Fade {
        from: FrameScene,
        to: FrameScene,
        elapsed_ns: u64,
        duration_ns: u64,
        epoch: u64,
    },
    Black {
        logical_scene: Id,
        epoch: u64,
    },
    SceneAlias {
        scene: FrameScene,
        logical_scene: Id,
        epoch: u64,
    },
    SingleFade {
        scene: FrameScene,
        logical_scene: Id,
        incoming: bool,
        elapsed_ns: u64,
        duration_ns: u64,
        epoch: u64,
    },
    Manual {
        from: Option<FrameScene>,
        to: Option<FrameScene>,
        kind: TransitionKind,
        logical_scene: Id,
        progress_num: u64,
        progress_den: u64,
        epoch: u64,
        next_after_ns: Option<u64>,
    },
    Directional {
        from: Option<FrameScene>,
        to: Option<FrameScene>,
        kind: TransitionKind,
        logical_scene: Id,
        elapsed_ns: u64,
        duration_ns: u64,
        epoch: u64,
    },
}
impl FrameComposition {
    pub(crate) fn with_auto_projection(&self,authored:&StudioSnapshot,projected:&StudioSnapshot)->Result<Self,TransitionError>{
        let mut result=self.clone();
        let convert=|view:&mut FrameScene,frozen:bool|->Result<(),TransitionError>{*view=view.with_auto_projection(authored,projected,frozen)?;Ok(())};
        match &mut result {
            Self::Scene(view)|Self::SceneAlias{scene:view,..}=>convert(view,false)?,
            Self::SingleFade{scene:view,..}=>convert(view,true)?,
            Self::Fade{from,to,..}=>{convert(from,true)?;convert(to,true)?;},
            Self::Manual{from,to,..}|Self::Directional{from,to,..}|Self::Configured{from,to,..}=>{
                if let Some(view)=from{convert(view,true)?;}if let Some(view)=to{convert(view,true)?;}
            },
            Self::Black{..}=>{},
        }
        Ok(result)
    }
    pub fn primary_scene(&self) -> Id {
        match self {
            Self::Scene(scene) => scene.scene(),
            Self::Fade { to, .. } => to.scene(),
            Self::Black { logical_scene, .. }
            | Self::SceneAlias { logical_scene, .. }
            | Self::SingleFade { logical_scene, .. }
            | Self::Directional { logical_scene, .. }
            | Self::Configured { logical_scene, .. }
            | Self::Manual { logical_scene, .. } => *logical_scene,
        }
    }
    pub fn scenes(&self) -> Vec<FrameScene> {
        match self {
            Self::Scene(scene)
            | Self::SceneAlias { scene, .. }
            | Self::SingleFade { scene, .. } => vec![scene.clone()],
            Self::Fade { from, to, .. } => vec![from.clone(), to.clone()],
            Self::Black { .. } => Vec::new(),
            Self::Manual { from, to, .. }
            | Self::Directional { from, to, .. }
            | Self::Configured { from, to, .. } => from.iter().chain(to.iter()).cloned().collect(),
        }
    }
    pub fn is_manual(&self) -> bool {
        matches!(self, Self::Manual { .. })
    }
    pub fn identity(&self) -> OutputKey {
        match self {
            Self::Scene(scene) => OutputKey::Scene(scene.key()),
            Self::Configured {
                from,
                to,
                kind,
                logical_scene,
                duration_ns,
                epoch,
                ..
            } => OutputKey::Configured {
                from: from.as_ref().map(FrameScene::key),
                to: to.as_ref().map(FrameScene::key),
                kind: *kind,
                logical_scene: *logical_scene,
                duration_ns: *duration_ns,
                epoch: *epoch,
            },
            Self::Fade {
                from,
                to,
                duration_ns,
                epoch,
                ..
            } => OutputKey::Fade {
                from: from.key(),
                to: to.key(),
                duration_ns: *duration_ns,
                epoch: *epoch,
            },
            Self::Black {
                logical_scene,
                epoch,
            } => OutputKey::Black {
                logical_scene: *logical_scene,
                epoch: *epoch,
            },
            Self::SceneAlias {
                scene,
                logical_scene,
                epoch,
            } => OutputKey::SceneAlias {
                scene: scene.key(),
                logical_scene: *logical_scene,
                epoch: *epoch,
            },
            Self::SingleFade {
                scene,
                logical_scene,
                incoming,
                duration_ns,
                epoch,
                ..
            } => OutputKey::SingleFade {
                scene: scene.key(),
                logical_scene: *logical_scene,
                incoming: *incoming,
                duration_ns: *duration_ns,
                epoch: *epoch,
            },
            Self::Manual {
                from,
                to,
                kind,
                logical_scene,
                epoch,
                ..
            } => OutputKey::Manual {
                from: from.as_ref().map(FrameScene::key),
                to: to.as_ref().map(FrameScene::key),
                kind: *kind,
                logical_scene: *logical_scene,
                epoch: *epoch,
            },
            Self::Directional {
                from,
                to,
                kind,
                logical_scene,
                duration_ns,
                epoch,
                ..
            } => OutputKey::Directional {
                from: from.as_ref().map(FrameScene::key),
                to: to.as_ref().map(FrameScene::key),
                kind: *kind,
                logical_scene: *logical_scene,
                duration_ns: *duration_ns,
                epoch: *epoch,
            },
        }
    }
    pub fn sample_key(&self) -> CompositionKey {
        match self {
            Self::Scene(scene) => CompositionKey::Scene(scene.key()),
            Self::Configured {
                from,
                to,
                kind,
                logical_scene,
                elapsed_ns,
                duration_ns,
                epoch,
            } => CompositionKey::Configured {
                from: from.as_ref().map(FrameScene::key),
                to: to.as_ref().map(FrameScene::key),
                kind: *kind,
                logical_scene: *logical_scene,
                elapsed_ns: *elapsed_ns,
                duration_ns: *duration_ns,
                epoch: *epoch,
            },
            Self::Fade {
                from,
                to,
                elapsed_ns,
                duration_ns,
                epoch,
            } => CompositionKey::Fade {
                from: from.key(),
                to: to.key(),
                elapsed_ns: *elapsed_ns,
                duration_ns: *duration_ns,
                epoch: *epoch,
            },
            Self::Black {
                logical_scene,
                epoch,
            } => CompositionKey::Black {
                logical_scene: *logical_scene,
                epoch: *epoch,
            },
            Self::SceneAlias {
                scene,
                logical_scene,
                epoch,
            } => CompositionKey::SceneAlias {
                scene: scene.key(),
                logical_scene: *logical_scene,
                epoch: *epoch,
            },
            Self::SingleFade {
                scene,
                logical_scene,
                incoming,
                elapsed_ns,
                duration_ns,
                epoch,
            } => CompositionKey::SingleFade {
                scene: scene.key(),
                logical_scene: *logical_scene,
                incoming: *incoming,
                elapsed_ns: *elapsed_ns,
                duration_ns: *duration_ns,
                epoch: *epoch,
            },
            Self::Manual {
                from,
                to,
                kind,
                logical_scene,
                progress_num,
                progress_den,
                epoch,
                next_after_ns,
            } => CompositionKey::Manual {
                from: from.as_ref().map(FrameScene::key),
                to: to.as_ref().map(FrameScene::key),
                kind: *kind,
                logical_scene: *logical_scene,
                progress_num: *progress_num,
                progress_den: *progress_den,
                epoch: *epoch,
                next_after_ns: *next_after_ns,
            },
            Self::Directional {
                from,
                to,
                kind,
                logical_scene,
                elapsed_ns,
                duration_ns,
                epoch,
            } => CompositionKey::Directional {
                from: from.as_ref().map(FrameScene::key),
                to: to.as_ref().map(FrameScene::key),
                kind: *kind,
                logical_scene: *logical_scene,
                elapsed_ns: *elapsed_ns,
                duration_ns: *duration_ns,
                epoch: *epoch,
            },
        }
    }
    /// First profile tick strictly after the sample, or the exact automatic end.
    pub fn next_after_ns(&self, current: &StudioSnapshot) -> Option<u64> {
        let (elapsed, duration) = match self {
            Self::Fade {
                elapsed_ns,
                duration_ns,
                ..
            }
            | Self::SingleFade {
                elapsed_ns,
                duration_ns,
                ..
            }
            | Self::Directional {
                elapsed_ns,
                duration_ns,
                ..
            }
            | Self::Configured {
                elapsed_ns,
                duration_ns,
                ..
            } => (*elapsed_ns, *duration_ns),
            Self::Manual { next_after_ns, .. } => return *next_after_ns,
            _ => return None,
        };
        if elapsed >= duration {
            return None;
        }
        let numerator = u128::from(current.video().fps_num());
        let denominator = u128::from(current.video().fps_den()) * 1_000_000_000;
        if numerator == 0 || denominator == 0 {
            return None;
        }
        let index = ((u128::from(elapsed) + 1) * numerator).div_ceil(denominator);
        let tick = index * denominator / numerator;
        u64::try_from((tick - u128::from(elapsed)).min(u128::from(duration - elapsed)))
            .ok()
            .filter(|value| *value > 0)
    }
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ManualStatus {
    pub value: u16,
    pub current: f32,
    pub target: f32,
    pub finishing: bool,
}
#[derive(Clone, Debug)]
pub struct TransitionSnapshot {
    pub session: u64,
    pub revision: u64,
    pub epoch: u64,
    pub now: MediaTime,
    pub output: FrameComposition,
    pub active: bool,
    pub studio: bool,
    pub manual: Option<ManualStatus>,
    pub needs_tick: bool,
}
#[derive(Clone, Debug)]
struct Fade {
    from: Option<FrameScene>,
    to: Option<FrameScene>,
    kind: TransitionKind,
    reference: TransitionRef,
    started: MediaTime,
    duration_ns: u64,
    epoch: u64,
    swap: Option<Id>,
    allow_removed: bool,
}
#[derive(Clone, Debug)]
struct Manual {
    from: Option<FrameScene>,
    to: Option<FrameScene>,
    kind: TransitionKind,
    reference: TransitionRef,
    old_program: Id,
    swap: Option<Id>,
    value: u16,
    finishing: bool,
    clock: ManualClock,
}
#[derive(Clone, Debug)]
pub struct Controller {
    session: Option<u64>,
    lifetime:Option<skylive_core::SessionLifetime>,
    studio: bool,
    swap_scenes: bool,
    program: Option<FrameScene>,
    fade: Option<Fade>,
    manual: Option<Manual>,
    ftb_return: Option<FrameScene>,
    logical_program: Option<Id>,
    epoch: u64,
    now: MediaTime,
}
#[derive(Clone, Copy)]
struct Selection {
    preview: Id,
    program: Id,
}
impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}
impl Controller {
    pub fn new() -> Self {
        Self {
            session: None,
            lifetime:None,
            studio: false,
            swap_scenes: true,
            program: None,
            fade: None,
            manual: None,
            ftb_return: None,
            logical_program: None,
            epoch: 0,
            now: MediaTime(0),
        }
    }
    pub fn studio_mode(&self) -> bool {
        self.studio
    }
    pub fn swap_scenes(&self) -> bool {
        self.swap_scenes
    }
    pub fn set_swap_scenes(&mut self, enabled: bool) {
        self.swap_scenes = enabled;
    }
    pub fn now(&self) -> MediaTime {
        self.now
    }
    fn next_epoch(&self) -> Result<u64, TransitionError> {
        self.epoch
            .checked_add(1)
            .ok_or(TransitionError::IdentityExhausted)
    }
    fn active(&self) -> bool {
        self.fade.is_some() || self.manual.is_some()
    }
    fn view(&self, current: &StudioSnapshot, scene: Id) -> Result<FrameScene, TransitionError> {
        if current.scene(scene).is_none() {
            return Err(TransitionError::NotFound(scene));
        }
        if self.studio {
            FrameScene::capture(self.session.ok_or(TransitionError::Closed)?, current, scene,self.lifetime.as_ref())
        } else {
            Ok(FrameScene::Live(scene))
        }
    }
    fn refresh(&mut self, current: &StudioSnapshot) {
        for view in self.program.iter_mut().chain(self.ftb_return.iter_mut()) {
            view.refresh(current);
        }
        if let Some(fade) = &mut self.fade {
            Self::refresh_kind(current, fade.reference, &mut fade.kind);
            for view in fade.from.iter_mut().chain(fade.to.iter_mut()) {
                view.refresh(current);
            }
        }
        if let Some(manual) = &mut self.manual {
            Self::refresh_kind(current, manual.reference, &mut manual.kind);
            for view in manual.from.iter_mut().chain(manual.to.iter_mut()) {
                view.refresh(current);
            }
        }
    }
    fn refresh_kind(current: &StudioSnapshot, reference: TransitionRef, kind: &mut TransitionKind) {
        // Provider settings are shared authored data, so a property edit also
        // affects an active pair. If its final canonical reference is removed,
        // retain the last settings refreshed before publication, like sources.
        if let TransitionRef::Preset(id) = reference {
            if let Some(preset) = current.transition_preset(id) {
                *kind = preset.kind();
            }
        }
    }
    /// Refresh metadata before the last canonical reference can be removed.
    /// Inactive FTB return ownership never implies playback activation or I/O.
    pub fn before_change(&mut self, session: &Session) -> Result<(), TransitionError> {
        if session.is_closed() {
            self.release();
            return Err(TransitionError::Closed);
        }
        if self.session == Some(session.identity()) {
            self.refresh(&session.snapshot());
        }
        Ok(())
    }
    fn release(&mut self) {
        self.fade = None;
        self.manual = None;
        self.program = None;
        self.ftb_return = None;
        self.logical_program = None;
        self.session = None;
        self.lifetime=None;
    }
    fn settle(
        &mut self,
        current: &StudioSnapshot,
        selection: Selection,
    ) -> Result<(), TransitionError> {
        let epoch = self.next_epoch()?;
        let program = self.view(current, selection.program)?;
        self.fade = None;
        self.manual = None;
        self.ftb_return = None;
        self.program = Some(program);
        self.logical_program = Some(selection.program);
        self.epoch = epoch;
        Ok(())
    }
    fn finish(
        &mut self,
        to: Option<FrameScene>,
        swap: Option<Id>,
        current: &StudioSnapshot,
        selection: &mut Selection,
    ) {
        self.program = to;
        if self.swap_scenes {
            if let Some(swap) = swap.filter(|scene| current.scene(*scene).is_some()) {
                selection.preview = swap;
            }
        }
    }
    fn synchronize(
        &mut self,
        session: u64,
        current: &StudioSnapshot,
        selection: &mut Selection,
        now: MediaTime,
    ) -> Result<(), TransitionError> {
        if current.scene(selection.preview).is_none() {
            return Err(TransitionError::NotFound(selection.preview));
        }
        if current.scene(selection.program).is_none() {
            return Err(TransitionError::NotFound(selection.program));
        }
        if self.session != Some(session) {
            self.session = Some(session);
            self.settle(current, *selection)?;
            self.now = now;
            return Ok(());
        }
        self.refresh(current);
        let endpoint_missing = |view: &FrameScene| current.scene(view.scene()).is_none();
        let invalid_pair = self.fade.as_ref().is_some_and(|fade| {
            !fade.allow_removed && fade.from.iter().chain(fade.to.iter()).any(endpoint_missing)
        }) || self.manual.as_ref().is_some_and(|manual| {
            manual
                .from
                .iter()
                .chain(manual.to.iter())
                .any(endpoint_missing)
        });
        if now < self.now || self.logical_program != Some(selection.program) || invalid_pair {
            self.settle(current, *selection)?;
        } else if let Some(manual) = &mut self.manual {
            manual.clock.advance(current, self.now, now)?;
            if manual.finishing && manual.clock.current == 1.0 {
                let manual = self.manual.take().ok_or(TransitionError::Busy)?;
                self.finish(manual.to, manual.swap, current, selection);
            } else {
                manual.clock.next_after(now)?;
            }
        } else if self
            .fade
            .as_ref()
            .is_some_and(|fade| now.0 - fade.started.0 >= fade.duration_ns)
        {
            let fade = self.fade.take().ok_or(TransitionError::Busy)?;
            self.finish(fade.to, fade.swap, current, selection);
        }
        self.now = now;
        Ok(())
    }
    fn transaction<F>(
        &mut self,
        session: &mut Session,
        now: MediaTime,
        operation: F,
    ) -> Result<(), TransitionError>
    where
        F: FnOnce(&mut Self, &StudioSnapshot, &mut Selection) -> Result<(), TransitionError>,
    {
        if session.is_closed() {
            self.release();
            return Err(TransitionError::Closed);
        }
        let current = session.snapshot();
        let mut selection = Selection {
            preview: session.preview(),
            program: session.program(),
        };
        let mut candidate = self.clone();
        candidate.lifetime=Some(session.lifetime());
        candidate.synchronize(session.identity(), &current, &mut selection, now)?;
        operation(&mut candidate, &current, &mut selection)?;
        // Complete all preparation/identity/deadline checks before publication.
        if let Some(manual) = &candidate.manual {
            manual.clock.next_after(now)?;
            manual.clock.rational()?;
        }
        if current.scene(selection.program).is_none() {
            return Err(TransitionError::NotFound(selection.program));
        }
        if current.scene(selection.preview).is_none() {
            return Err(TransitionError::NotFound(selection.preview));
        }
        session.select_program(selection.program)?;
        session.select_preview(selection.preview)?;
        *self = candidate;
        Ok(())
    }
    pub fn sync(
        &mut self,
        session: &mut Session,
        now: MediaTime,
    ) -> Result<TransitionSnapshot, TransitionError> {
        self.transaction(session, now, |_, _, _| Ok(()))?;
        let logical_scene = self.logical_program.ok_or(TransitionError::Closed)?;
        let output = if let Some(manual) = &self.manual {
            let (progress_num, progress_den) = manual.clock.rational()?;
            FrameComposition::Manual {
                from: manual.from.clone(),
                to: manual.to.clone(),
                kind: manual.kind,
                logical_scene,
                progress_num,
                progress_den,
                epoch: self.epoch,
                next_after_ns: manual.clock.next_after(now)?,
            }
        } else if let Some(fade) = &self.fade {
            let elapsed_ns = now.0 - fade.started.0;
            if matches!(fade.kind, TransitionKind::FadeToColor { .. }|TransitionKind::LumaWipe{..}) {
                FrameComposition::Configured {
                    from: fade.from.clone(),
                    to: fade.to.clone(),
                    kind: fade.kind,
                    logical_scene,
                    elapsed_ns,
                    duration_ns: fade.duration_ns,
                    epoch: fade.epoch,
                }
            } else if matches!(
                fade.kind,
                TransitionKind::Swipe { .. } | TransitionKind::Slide { .. }
            ) {
                FrameComposition::Directional {
                    from: fade.from.clone(),
                    to: fade.to.clone(),
                    kind: fade.kind,
                    logical_scene,
                    elapsed_ns,
                    duration_ns: fade.duration_ns,
                    epoch: fade.epoch,
                }
            } else {
                match (&fade.from, &fade.to) {
                    (Some(from), Some(to)) => FrameComposition::Fade {
                        from: from.clone(),
                        to: to.clone(),
                        elapsed_ns,
                        duration_ns: fade.duration_ns,
                        epoch: fade.epoch,
                    },
                    (Some(scene), None) => FrameComposition::SingleFade {
                        scene: scene.clone(),
                        logical_scene,
                        incoming: false,
                        elapsed_ns,
                        duration_ns: fade.duration_ns,
                        epoch: fade.epoch,
                    },
                    (None, Some(scene)) => FrameComposition::SingleFade {
                        scene: scene.clone(),
                        logical_scene,
                        incoming: true,
                        elapsed_ns,
                        duration_ns: fade.duration_ns,
                        epoch: fade.epoch,
                    },
                    (None, None) => FrameComposition::Black {
                        logical_scene,
                        epoch: self.epoch,
                    },
                }
            }
        } else {
            match &self.program {
                Some(scene) if scene.scene() == logical_scene => {
                    FrameComposition::Scene(scene.clone())
                }
                Some(scene) => FrameComposition::SceneAlias {
                    scene: scene.clone(),
                    logical_scene,
                    epoch: self.epoch,
                },
                None => FrameComposition::Black {
                    logical_scene,
                    epoch: self.epoch,
                },
            }
        };
        let manual = self.manual.as_ref().map(|manual| ManualStatus {
            value: manual.value,
            current: manual.clock.current,
            target: manual.clock.target,
            finishing: manual.finishing,
        });
        let needs_tick = self.fade.is_some()
            || self
                .manual
                .as_ref()
                .is_some_and(|manual| manual.clock.current != manual.clock.target);
        Ok(TransitionSnapshot {
            session: session.identity(),
            revision: session.snapshot().revision(),
            epoch: self.epoch,
            now,
            output,
            active: self.active(),
            studio: self.studio,
            manual,
            needs_tick,
        })
    }
    pub fn set_studio(
        &mut self,
        session: &mut Session,
        enabled: bool,
        now: MediaTime,
    ) -> Result<(), TransitionError> {
        self.transaction(session, now, |owner, current, selection| {
            if owner.studio == enabled {
                return Ok(());
            }
            let epoch = owner.next_epoch()?;
            if enabled {
                selection.program = selection.preview;
            } else {
                selection.preview = selection.program;
            }
            owner.studio = enabled;
            let program = owner.view(current, selection.program)?;
            owner.fade = None;
            owner.manual = None;
            owner.ftb_return = None;
            owner.program = Some(program);
            owner.logical_program = Some(selection.program);
            owner.epoch = epoch;
            Ok(())
        })
    }
    fn destination_selection(
        current: &StudioSnapshot,
        target: Id,
    ) -> Result<TransitionSelection, TransitionError> {
        let scene = current
            .scene(target)
            .ok_or(TransitionError::NotFound(target))?;
        Ok(*scene.transition_override().unwrap_or(current.transition()))
    }
    fn manual_selection(
        current: &StudioSnapshot,
        target: Id,
    ) -> Result<TransitionSelection, TransitionError> {
        let destination = Self::destination_selection(current, target)?;
        if current.resolve_transition(&destination)?.kind() != TransitionKind::Cut {
            return Ok(destination);
        }
        if current.resolve_transition(current.transition())?.kind() != TransitionKind::Cut {
            return Ok(*current.transition());
        }
        // OBS ValidTBarTransition skips Cut, tries the global provider, then
        // falls back to the inherent Fade. Duration does not drive manual time.
        Ok(TransitionSelection::new(
            TransitionRef::Fade,
            current.transition().duration_ms(),
        )?)
    }
    fn start(
        &mut self,
        current: &StudioSnapshot,
        selection: &mut Selection,
        force_cut: bool,
        explicit: Option<(TransitionSelection, bool)>,
    ) -> Result<(), TransitionError> {
        if self.studio && self.active() && !force_cut {
            return Err(TransitionError::Busy);
        }
        let target = selection.preview;
        let old_program = selection.program;
        let from = if let Some(active) = &self.fade {
            active.to.clone()
        } else if let Some(active) = &self.manual {
            active.to.clone()
        } else {
            self.program.clone()
        };
        if !self.studio && !force_cut && from.as_ref().is_some_and(|from| from.scene() == target) {
            if self.active() {
                self.epoch = self.next_epoch()?;
                self.fade = None;
                self.manual = None;
                self.program = from;
                self.ftb_return = None;
                self.logical_program = Some(target);
            }
            return Ok(());
        }
        let epoch = self.next_epoch()?;
        let (authored, black) =
            explicit.unwrap_or((Self::destination_selection(current, target)?, false));
        let config = current.resolve_transition(&authored)?;
        let prepared = self.view(current, target)?;
        let mut allow_removed = false;
        let to = if black {
            if let Some(saved) = self.ftb_return.take() {
                allow_removed = true;
                Some(saved)
            } else {
                self.ftb_return = Some(prepared);
                None
            }
        } else {
            self.ftb_return = None;
            Some(prepared)
        };
        let swap = (!black && self.studio && self.swap_scenes && old_program != target)
            .then_some(old_program);
        let cut = force_cut || config.kind() == TransitionKind::Cut;
        if !cut {
            self.now
                .0
                .checked_add(config.duration_ns())
                .ok_or(TransitionError::TimeOverflow)?;
        }
        selection.program = target;
        self.logical_program = Some(target);
        self.epoch = epoch;
        self.manual = None;
        if cut || (from.is_none() && to.is_none()) {
            self.fade = None;
            self.finish(to, swap, current, selection);
        } else {
            self.program = None;
            self.fade = Some(Fade {
                from,
                to,
                kind: config.kind(),
                reference: authored.reference(),
                started: self.now,
                duration_ns: config.duration_ns(),
                epoch,
                swap,
                allow_removed,
            });
        }
        Ok(())
    }
    pub fn select_preview(
        &mut self,
        session: &mut Session,
        scene: Id,
        now: MediaTime,
    ) -> Result<(), TransitionError> {
        self.transaction(session, now, |owner, current, selection| {
            if current.scene(scene).is_none() {
                return Err(TransitionError::NotFound(scene));
            }
            selection.preview = scene;
            if owner.studio {
                Ok(())
            } else {
                owner.start(current, selection, false, None)
            }
        })
    }
    pub fn transition(
        &mut self,
        session: &mut Session,
        now: MediaTime,
    ) -> Result<(), TransitionError> {
        self.transaction(session, now, |owner, current, selection| {
            owner.start(current, selection, false, None)
        })
    }
    /// Explicit force Cut retires either active pair without its pending swap.
    pub fn cut(&mut self, session: &mut Session, now: MediaTime) -> Result<(), TransitionError> {
        self.transaction(session, now, |owner, current, selection| {
            owner.start(current, selection, true, None)
        })
    }
    pub fn quick(
        &mut self,
        session: &mut Session,
        id: QuickTransitionId,
        now: MediaTime,
    ) -> Result<(), TransitionError> {
        self.transaction(session, now, |owner, current, selection| {
            if !owner.studio {
                return Err(TransitionError::NotStudio);
            }
            let row = current
                .quick_transitions()
                .iter()
                .find(|row| row.id() == id)
                .ok_or(TransitionError::UnknownQuick(id))?;
            owner.start(
                current,
                selection,
                false,
                Some((*row.config(), row.fade_to_black())),
            )
        })
    }
    pub fn manual_position(
        &mut self,
        session: &mut Session,
        value: u16,
        now: MediaTime,
    ) -> Result<(), TransitionError> {
        self.transaction(session, now, |owner, current, selection| {
            if !owner.studio {
                return Err(TransitionError::NotStudio);
            }
            if value > 1023 {
                return Err(TransitionError::InvalidPosition(value));
            }
            if owner.fade.is_some() || owner.manual.as_ref().is_some_and(|manual| manual.finishing)
            {
                return Err(TransitionError::Busy);
            }
            if owner.manual.is_none() {
                let epoch = owner.next_epoch()?;
                let to = owner.view(current, selection.preview)?;
                let clock = ManualClock::new(current, now)?;
                let authored = Self::manual_selection(current, selection.preview)?;
                owner.manual = Some(Manual {
                    from: owner.program.take(),
                    to: Some(to),
                    kind: current.resolve_transition(&authored)?.kind(),
                    reference: authored.reference(),
                    old_program: selection.program,
                    swap: (owner.swap_scenes && selection.program != selection.preview)
                        .then_some(selection.program),
                    value,
                    finishing: false,
                    clock,
                });
                owner.ftb_return = None;
                owner.logical_program = Some(selection.preview);
                selection.program = selection.preview;
                owner.epoch = epoch;
            }
            let manual = owner.manual.as_mut().ok_or(TransitionError::Busy)?;
            manual.value = value;
            manual.clock.target = (f32::from(value) / 1024.0).clamp(0.01, 0.99);
            Ok(())
        })
    }
    pub fn manual_release(
        &mut self,
        session: &mut Session,
        now: MediaTime,
    ) -> Result<(), TransitionError> {
        self.transaction(session, now, |owner, _, selection| {
            if !owner.studio {
                return Err(TransitionError::NotStudio);
            }
            let Some(manual) = &mut owner.manual else {
                return Ok(());
            };
            if manual.finishing {
                return Ok(());
            }
            if manual.value >= 921 {
                manual.value = 0;
                manual.finishing = true;
                manual.clock.target = 1.0;
            } else if manual.value <= 102 {
                let epoch = owner.next_epoch()?;
                let manual = owner.manual.take().ok_or(TransitionError::Busy)?;
                selection.program = manual.old_program;
                owner.logical_program = Some(manual.old_program);
                owner.program = manual.from;
                owner.ftb_return = None;
                owner.epoch = epoch;
            }
            Ok(())
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use skylive_core::TransitionConfig;
    use skylive_core::{Command, SourceKind, Transform};
    fn scenes() -> (Session, Id, Id) {
        let mut session = Session::new();
        let a = session.preview();
        let b = session
            .apply(Command::AddScene { name: "B".into() })
            .unwrap()
            .created
            .unwrap();
        (session, a, b)
    }
    #[test]
    fn exact_timing_busy_rejection_swap_and_captured_config() {
        let (mut session, a, b) = scenes();
        let mut owner = Controller::new();
        owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
        owner
            .select_preview(&mut session, b, MediaTime(10))
            .unwrap();
        owner.transition(&mut session, MediaTime(10)).unwrap();
        assert_eq!(session.program(), b);
        let initial = owner.sync(&mut session, MediaTime(10)).unwrap();
        assert!(initial.active);
        let FrameComposition::Fade {
            elapsed_ns,
            duration_ns,
            ..
        } = initial.output
        else {
            panic!()
        };
        assert_eq!((elapsed_ns, duration_ns), (0, 300_000_000));
        let epoch = owner.epoch;
        assert_eq!(
            owner.transition(&mut session, MediaTime(50)),
            Err(TransitionError::Busy)
        );
        assert_eq!(owner.epoch, epoch);
        assert_eq!(owner.now(), MediaTime(10));
        owner.before_change(&session).unwrap();
        session
            .apply(Command::SetTransition {
                kind: TransitionKind::Cut,
                duration_ms: 500,
            })
            .unwrap();
        let middle = owner.sync(&mut session, MediaTime(150_000_010)).unwrap();
        let FrameComposition::Fade {
            elapsed_ns,
            duration_ns,
            ..
        } = middle.output
        else {
            panic!()
        };
        assert_eq!((elapsed_ns, duration_ns), (150_000_000, 300_000_000));
        let ended = owner.sync(&mut session, MediaTime(300_000_010)).unwrap();
        assert!(!ended.active);
        assert_eq!(session.preview(), a);
        assert_eq!(session.program(), b);
    }
    #[test]
    fn studio_layout_is_frozen_sources_stay_live_and_last_reference_retains_latest_props() {
        let mut session = Session::new();
        let a = session.preview();
        let item = session
            .apply(Command::AddColorSource {
                scene: a,
                name: "Color".into(),
                rgba: [255, 0, 0, 255],
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        let mut owner = Controller::new();
        owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
        owner.before_change(&session).unwrap();
        session
            .apply(Command::SetTransform {
                scene: a,
                item,
                transform: Transform {
                    x: 100.0,
                    ..Transform::full(1920, 1080)
                },
            })
            .unwrap();
        let old = owner.sync(&mut session, MediaTime(1)).unwrap();
        let FrameComposition::Scene(view) = old.output else {
            panic!()
        };
        assert_eq!(
            view.items(&session.snapshot()).unwrap()[0].transform().x,
            0.0
        );
        session
            .apply(Command::SetColor {
                source,
                rgba: [0, 255, 0, 255],
            })
            .unwrap();
        owner.before_change(&session).unwrap();
        session
            .apply(Command::RemoveItem { scene: a, item })
            .unwrap();
        let output = owner.sync(&mut session, MediaTime(2)).unwrap();
        let FrameComposition::Scene(view) = output.output else {
            panic!()
        };
        let current = session.snapshot();
        assert!(current.source(source).is_none());
        assert_eq!(
            view.source(&current, source).unwrap().kind(),
            &SourceKind::Color {
                rgba: [0, 255, 0, 255],
                width: 1920,
                height: 1080
            }
        );
        assert_eq!(view.captured_dimensions(source), Some((1920, 1080)));
        owner.transition(&mut session, MediaTime(3)).unwrap();
        let fresh = owner.sync(&mut session, MediaTime(3)).unwrap();
        let FrameComposition::Fade { from, to, .. } = fresh.output else {
            panic!()
        };
        assert_ne!(from.key(), to.key());
        assert_eq!(from.scene(), to.scene());
        assert!(to.items(&current).unwrap().is_empty());
    }
    #[test]
    fn studio_exit_preserves_program_and_cut_swaps_without_authored_edit() {
        let (mut session, a, b) = scenes();
        let mut owner = Controller::new();
        owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
        owner.select_preview(&mut session, b, MediaTime(1)).unwrap();
        let revision = session.snapshot().revision();
        owner.set_studio(&mut session, false, MediaTime(2)).unwrap();
        assert_eq!((session.preview(), session.program()), (a, a));
        assert_eq!(session.snapshot().revision(), revision);
        owner.set_studio(&mut session, true, MediaTime(3)).unwrap();
        owner.select_preview(&mut session, b, MediaTime(4)).unwrap();
        owner.cut(&mut session, MediaTime(4)).unwrap();
        assert_eq!((session.preview(), session.program()), (a, b));
        assert!(!owner.sync(&mut session, MediaTime(4)).unwrap().active);
        owner.set_swap_scenes(false);
        owner.select_preview(&mut session, a, MediaTime(5)).unwrap();
        owner.cut(&mut session, MediaTime(5)).unwrap();
        assert_eq!((session.preview(), session.program()), (a, a));
    }
    #[test]
    fn retained_refresh_is_once_per_revision_old_views_immutable_and_size_basis_preserved() {
        let mut session = Session::new();
        let a = session.preview();
        session
            .apply(Command::AddColorSource {
                scene: a,
                name: "Color".into(),
                rgba: [255; 4],
            })
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        let mut owner = Controller::new();
        owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
        let first = owner.sync(&mut session, MediaTime(1)).unwrap();
        let FrameComposition::Scene(FrameScene::Frozen(old)) = first.output else {
            panic!()
        };
        let old_revision = old.refreshed_revision;
        let same = owner.sync(&mut session, MediaTime(2)).unwrap();
        let FrameComposition::Scene(FrameScene::Frozen(same)) = same.output else {
            panic!()
        };
        assert!(Arc::ptr_eq(&old, &same));
        owner.before_change(&session).unwrap();
        session
            .apply(Command::SetColorSize {
                source,
                width: 960,
                height: 540,
            })
            .unwrap();
        let next = owner.sync(&mut session, MediaTime(3)).unwrap();
        let FrameComposition::Scene(FrameScene::Frozen(next)) = next.output else {
            panic!()
        };
        assert!(!Arc::ptr_eq(&old, &next));
        assert_eq!(old.refreshed_revision, old_revision);
        assert_eq!(old.retained[&source].kind().dimensions(), (1920, 1080));
        assert_eq!(next.retained[&source].kind().dimensions(), (960, 540));
        assert_eq!(next.capture_dimensions[&source], (1920, 1080));
        let again = owner.sync(&mut session, MediaTime(4)).unwrap();
        let FrameComposition::Scene(FrameScene::Frozen(again)) = again.output else {
            panic!()
        };
        assert!(Arc::ptr_eq(&next, &again));
    }
    #[test]
    fn nonstudio_interruption_promotes_prior_target_and_force_cut_finishes() {
        let (mut session, a, b) = scenes();
        let c = session
            .apply(Command::AddScene { name: "C".into() })
            .unwrap()
            .created
            .unwrap();
        let mut owner = Controller::new();
        owner.sync(&mut session, MediaTime(0)).unwrap();
        owner.select_preview(&mut session, b, MediaTime(1)).unwrap();
        owner
            .select_preview(&mut session, c, MediaTime(100))
            .unwrap();
        let output = owner.sync(&mut session, MediaTime(100)).unwrap();
        let FrameComposition::Fade { from, to, .. } = output.output else {
            panic!()
        };
        assert_eq!((from.scene(), to.scene()), (b, c));
        assert_ne!(from.scene(), a);
        owner.cut(&mut session, MediaTime(101)).unwrap();
        assert!(!owner.sync(&mut session, MediaTime(101)).unwrap().active);
    }
    #[test]
    fn selecting_active_destination_again_finishes_and_studio_force_cut_does_not_swap_old_a() {
        let (mut session, a, b) = scenes();
        let mut owner = Controller::new();
        owner.select_preview(&mut session, b, MediaTime(0)).unwrap();
        assert!(owner.sync(&mut session, MediaTime(1)).unwrap().active);
        owner.select_preview(&mut session, b, MediaTime(2)).unwrap();
        assert!(!owner.sync(&mut session, MediaTime(2)).unwrap().active);
        assert_eq!((session.preview(), session.program()), (b, b));
        owner.select_preview(&mut session, a, MediaTime(3)).unwrap();
        owner.cut(&mut session, MediaTime(3)).unwrap();
        owner.set_studio(&mut session, true, MediaTime(4)).unwrap();
        owner.select_preview(&mut session, b, MediaTime(5)).unwrap();
        owner.transition(&mut session, MediaTime(5)).unwrap();
        owner.cut(&mut session, MediaTime(6)).unwrap();
        assert_eq!((session.preview(), session.program()), (b, b));
        assert!(!owner.sync(&mut session, MediaTime(6)).unwrap().active);
    }
    #[test]
    fn backward_time_deleted_endpoint_new_session_and_closed_owner_release_old_state() {
        let (mut session, a, b) = scenes();
        let mut owner = Controller::new();
        owner
            .set_studio(&mut session, true, MediaTime(100))
            .unwrap();
        owner
            .select_preview(&mut session, b, MediaTime(101))
            .unwrap();
        owner.transition(&mut session, MediaTime(102)).unwrap();
        let old_epoch = owner.epoch;
        let back = owner.sync(&mut session, MediaTime(50)).unwrap();
        assert!(!back.active);
        assert!(back.epoch > old_epoch);
        assert_eq!(session.preview(), b);
        owner
            .select_preview(&mut session, a, MediaTime(51))
            .unwrap();
        owner.transition(&mut session, MediaTime(52)).unwrap();
        owner.before_change(&session).unwrap();
        session.apply(Command::RemoveScene { scene: a }).unwrap();
        let removed = owner.sync(&mut session, MediaTime(53)).unwrap();
        assert!(!removed.active);
        assert_eq!(removed.output.primary_scene(), b);
        let mut reopened = Session::from_snapshot((*session.snapshot()).clone(), true).unwrap();
        let fresh = owner.sync(&mut reopened, MediaTime(54)).unwrap();
        assert_ne!(fresh.session, removed.session);
        assert!(
            fresh
                .output
                .scenes()
                .iter()
                .all(|view| view.validate_session(fresh.session).is_ok())
        );
        assert!(
            removed.output.scenes()[0]
                .validate_session(fresh.session)
                .is_err()
        );
        reopened.close();
        assert_eq!(
            owner.sync(&mut reopened, MediaTime(55)).unwrap_err(),
            TransitionError::Closed
        );
        assert!(owner.program.is_none() && owner.fade.is_none());
    }
    #[test]
    fn keys_separate_layout_identity_from_sample_time_and_next_frame_rounding() {
        let (mut session, _, b) = scenes();
        let mut owner = Controller::new();
        owner.select_preview(&mut session, b, MediaTime(0)).unwrap();
        let first = owner.sync(&mut session, MediaTime(0)).unwrap();
        let second = owner.sync(&mut session, MediaTime(33_333_333)).unwrap();
        assert_eq!(first.output.identity(), second.output.identity());
        assert_ne!(first.output.sample_key(), second.output.sample_key());
        assert_eq!(
            first.output.next_after_ns(&session.snapshot()),
            Some(33_333_333)
        );
        assert_eq!(
            second.output.next_after_ns(&session.snapshot()),
            Some(33_333_333)
        );
        let last = owner.sync(&mut session, MediaTime(299_999_999)).unwrap();
        assert_eq!(last.output.next_after_ns(&session.snapshot()), Some(1));
    }
    #[test]
    fn overflow_rejects_before_program_preview_or_runtime_publication() {
        let (mut session, a, b) = scenes();
        let mut owner = Controller::new();
        owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
        owner.select_preview(&mut session, b, MediaTime(0)).unwrap();
        owner.epoch = u64::MAX;
        assert_eq!(
            owner.transition(&mut session, MediaTime(0)),
            Err(TransitionError::IdentityExhausted)
        );
        assert_eq!((session.preview(), session.program()), (b, a));
        assert!(owner.fade.is_none());
        assert_eq!(
            issue_instance(&AtomicU64::new(u64::MAX)),
            Err(TransitionError::IdentityExhausted)
        );
        owner.epoch = 7;
        assert_eq!(
            owner.transition(&mut session, MediaTime(u64::MAX - 1)),
            Err(TransitionError::TimeOverflow)
        );
        assert_eq!((session.preview(), session.program()), (b, a));
        assert!(owner.fade.is_none());
    }
    #[test]
    fn invalid_selection_rolls_back_runtime_and_swap_preference_is_checked_at_video_stop() {
        let (mut session, a, b) = scenes();
        let mut owner = Controller::new();
        owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
        owner.select_preview(&mut session, b, MediaTime(1)).unwrap();
        let before = owner.sync(&mut session, MediaTime(1)).unwrap();
        assert_eq!(
            owner.select_preview(&mut session, Id::new(u64::MAX), MediaTime(2)),
            Err(TransitionError::NotFound(Id::new(u64::MAX)))
        );
        assert_eq!(owner.now(), before.now);
        assert_eq!((session.preview(), session.program()), (b, a));
        owner.transition(&mut session, MediaTime(3)).unwrap();
        owner.set_swap_scenes(false);
        owner.sync(&mut session, MediaTime(300_000_003)).unwrap();
        assert_eq!((session.preview(), session.program()), (b, b));
    }
    fn studio_pair() -> (Session, Controller, Id, Id) {
        let (mut session, a, b) = scenes();
        let mut owner = Controller::new();
        owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
        owner.select_preview(&mut session, b, MediaTime(0)).unwrap();
        (session, owner, a, b)
    }
    #[test]
    fn manual_profile_ticks_hold_reverse_and_preserve_pair_identity() {
        let (mut session, mut owner, a, b) = studio_pair();
        let authored = session.snapshot();
        owner
            .manual_position(&mut session, 512, MediaTime(0))
            .unwrap();
        let initial = owner.sync(&mut session, MediaTime(0)).unwrap();
        assert!(initial.active && initial.needs_tick);
        assert_eq!(initial.manual.unwrap().current, 0.0);
        assert!(initial.output.is_manual());
        let before = owner.sync(&mut session, MediaTime(33_333_332)).unwrap();
        assert_eq!(before.manual.unwrap().current, 0.0);
        let first = owner.sync(&mut session, MediaTime(33_333_333)).unwrap();
        assert_eq!(
            first.manual.unwrap().current,
            skylive_media::manual_torque_step(0.0, 0.5, 33_333_333).unwrap()
        );
        let held = owner.sync(&mut session, MediaTime(10_000_000_000)).unwrap();
        assert!(held.active && !held.needs_tick);
        assert_eq!(held.manual.unwrap().current, 0.5);
        assert_eq!(held.output.next_after_ns(&session.snapshot()), None);
        assert_eq!(initial.output.identity(), held.output.identity());
        assert_ne!(initial.output.sample_key(), held.output.sample_key());
        owner
            .manual_release(&mut session, MediaTime(10_000_000_000))
            .unwrap();
        owner
            .manual_position(&mut session, 256, MediaTime(10_000_000_000))
            .unwrap();
        let reverse = owner.sync(&mut session, MediaTime(11_000_000_000)).unwrap();
        assert!(reverse.manual.unwrap().current < 0.5);
        assert_eq!(initial.output.identity(), reverse.output.identity());
        assert_eq!((session.preview(), session.program()), (b, b));
        assert_ne!(a, b);
        assert!(Arc::ptr_eq(&authored, &session.snapshot()));
    }
    #[test]
    fn every_manual_release_boundary_has_distinct_cancel_hold_or_finish_semantics() {
        for value in [0, 102, 103, 920, 921, 1023] {
            let (mut session, mut owner, a, b) = studio_pair();
            owner
                .manual_position(&mut session, value, MediaTime(0))
                .unwrap();
            owner.sync(&mut session, MediaTime(10_000_000_000)).unwrap();
            owner
                .manual_release(&mut session, MediaTime(10_000_000_000))
                .unwrap();
            let released = owner.sync(&mut session, MediaTime(10_000_000_000)).unwrap();
            if value <= 102 {
                assert!(!released.active);
                assert_eq!((session.preview(), session.program()), (b, a));
            } else if value >= 921 {
                let manual = released.manual.unwrap();
                assert_eq!(manual.value, 0);
                assert!(manual.finishing);
                assert_eq!(manual.target, 1.0);
                assert!(released.active && released.needs_tick);
                assert_eq!((session.preview(), session.program()), (b, b));
                let ended = owner.sync(&mut session, MediaTime(20_000_000_000)).unwrap();
                assert!(!ended.active);
                assert_eq!((session.preview(), session.program()), (a, b));
            } else {
                assert!(released.active && !released.needs_tick);
                assert!(!released.manual.unwrap().finishing);
                assert_eq!((session.preview(), session.program()), (b, b));
            }
        }
    }
    #[test]
    fn manual_provider_precedence_skips_cut_then_uses_global_or_inherent_fade() {
        use skylive_core::TransitionDirection;
        let (mut session, mut owner, _, b) = studio_pair();
        let kind = TransitionKind::Swipe {
            direction: TransitionDirection::Up,
            swipe_in: true,
        };
        let preset = session
            .apply(Command::AddTransitionPreset {
                name: "Manual global".into(),
                kind,
            })
            .unwrap()
            .created_transition_preset
            .unwrap();
        session
            .apply(Command::SelectTransition {
                selection: TransitionSelection::new(TransitionRef::Preset(preset), 900).unwrap(),
            })
            .unwrap();
        session
            .apply(Command::ConfigureSceneTransitionSelection {
                scene: b,
                reference: Some(TransitionRef::Cut),
                duration_ms: 700,
            })
            .unwrap();
        let authored = session.snapshot();
        owner
            .manual_position(&mut session, 512, MediaTime(0))
            .unwrap();
        let state = owner.sync(&mut session, MediaTime(10_000_000_000)).unwrap();
        assert!(
            matches!(state.output,FrameComposition::Manual{kind:actual,progress_num:1,progress_den:2,..} if actual==kind)
        );
        assert!(Arc::ptr_eq(&authored, &session.snapshot()));
        owner
            .manual_position(&mut session, 0, MediaTime(10_000_000_000))
            .unwrap();
        owner
            .manual_release(&mut session, MediaTime(10_000_000_000))
            .unwrap();
        session
            .apply(Command::SetTransition {
                kind: TransitionKind::Cut,
                duration_ms: 600,
            })
            .unwrap();
        owner
            .manual_position(&mut session, 512, MediaTime(10_000_000_000))
            .unwrap();
        let state = owner.sync(&mut session, MediaTime(20_000_000_000)).unwrap();
        assert!(matches!(
            state.output,
            FrameComposition::Manual {
                kind: TransitionKind::Fade,
                ..
            }
        ));
        owner
            .manual_position(&mut session, 0, MediaTime(20_000_000_000))
            .unwrap();
        owner
            .manual_release(&mut session, MediaTime(20_000_000_000))
            .unwrap();
        session
            .apply(Command::ConfigureSceneTransitionSelection {
                scene: b,
                reference: Some(TransitionRef::Preset(preset)),
                duration_ms: 800,
            })
            .unwrap();
        owner
            .manual_position(&mut session, 512, MediaTime(20_000_000_000))
            .unwrap();
        let state = owner.sync(&mut session, MediaTime(30_000_000_000)).unwrap();
        assert!(matches!(state.output,FrameComposition::Manual{kind:actual,..} if actual==kind));
        assert_eq!(
            session.snapshot().transition().reference(),
            TransitionRef::Cut
        );
    }

    #[test]
    fn quick_rows_bypass_destination_override_and_leave_global_configuration_unchanged() {
        let (mut session, mut owner, a, b) = studio_pair();
        let override_config = TransitionConfig::new(TransitionKind::Cut, 750).unwrap();
        session
            .apply(Command::SetSceneTransition {
                scene: b,
                config: Some(override_config),
            })
            .unwrap();
        let authored = session.snapshot();
        owner
            .quick(
                &mut session,
                QuickTransitionId::new(2).unwrap(),
                MediaTime(0),
            )
            .unwrap();
        let quick = owner.sync(&mut session, MediaTime(1)).unwrap();
        assert!(matches!(
            quick.output,
            FrameComposition::Fade {
                duration_ns: 300_000_000,
                ..
            }
        ));
        assert_eq!(
            session
                .snapshot()
                .resolve_transition(session.snapshot().transition())
                .unwrap(),
            TransitionConfig::default()
        );
        assert!(Arc::ptr_eq(&authored, &session.snapshot()));
        owner.sync(&mut session, MediaTime(300_000_000)).unwrap();
        assert_eq!(session.preview(), a);
        owner
            .select_preview(&mut session, b, MediaTime(300_000_001))
            .unwrap();
        owner
            .transition(&mut session, MediaTime(300_000_001))
            .unwrap();
        assert!(
            !owner
                .sync(&mut session, MediaTime(300_000_001))
                .unwrap()
                .active
        );
        assert_eq!(session.program(), b);
        owner
            .manual_position(&mut session, 512, MediaTime(300_000_002))
            .unwrap();
        assert!(
            owner
                .sync(&mut session, MediaTime(300_000_002))
                .unwrap()
                .output
                .is_manual(),
            "Cut overrides still fall back to manual Fade"
        );
        let mut plain = Session::new();
        assert_eq!(
            Controller::new().quick(&mut plain, QuickTransitionId::new(2).unwrap(), MediaTime(0)),
            Err(TransitionError::NotStudio)
        );
    }
    #[test]
    fn fade_to_black_retains_prepared_preview_b_and_returns_it_with_logical_c() {
        let (mut session, mut owner, a, b) = studio_pair();
        let c = session
            .apply(Command::AddScene { name: "C".into() })
            .unwrap()
            .created
            .unwrap();
        let ftb = QuickTransitionId::new(3).unwrap();
        owner.quick(&mut session, ftb, MediaTime(0)).unwrap();
        let saved = owner.ftb_return.as_ref().unwrap().key();
        let out = owner.sync(&mut session, MediaTime(150_000_000)).unwrap();
        assert!(
            matches!(&out.output,FrameComposition::SingleFade{scene,logical_scene,incoming:false,elapsed_ns:150_000_000,duration_ns:300_000_000,..}if scene.scene()==a&&*logical_scene==b)
        );
        assert_eq!(out.output.scenes().len(), 1);
        let black = owner.sync(&mut session, MediaTime(300_000_000)).unwrap();
        assert!(
            matches!(black.output,FrameComposition::Black{logical_scene,..}if logical_scene==b)
        );
        assert!(black.output.scenes().is_empty());
        assert_eq!((session.preview(), session.program()), (b, b));
        owner
            .select_preview(&mut session, c, MediaTime(300_000_000))
            .unwrap();
        owner
            .quick(&mut session, ftb, MediaTime(300_000_000))
            .unwrap();
        let incoming = owner.sync(&mut session, MediaTime(450_000_000)).unwrap();
        assert!(
            matches!(&incoming.output,FrameComposition::SingleFade{scene,logical_scene,incoming:true,..}if scene.key()==saved&&scene.scene()==b&&*logical_scene==c)
        );
        let ended = owner.sync(&mut session, MediaTime(600_000_000)).unwrap();
        assert!(
            matches!(ended.output,FrameComposition::SceneAlias{scene,logical_scene,..}if scene.key()==saved&&logical_scene==c)
        );
        assert_eq!((session.preview(), session.program()), (c, c));
        assert!(owner.ftb_return.is_none());
        owner
            .set_studio(&mut session, false, MediaTime(600_000_001))
            .unwrap();
        assert!(
            matches!(owner.sync(&mut session,MediaTime(600_000_001)).unwrap().output,FrameComposition::Scene(FrameScene::Live(scene))if scene==c)
        );
    }
    #[test]
    fn inactive_ftb_return_retains_latest_metadata_without_becoming_a_playback_output() {
        let (mut session, mut owner, _, b) = studio_pair();
        let item = session
            .apply(Command::AddImageSource {
                scene: b,
                name: "Saved GIF".into(),
                asset: skylive_core::ImageAsset::new("C:/saved.gif".into(), [1; 32], 1, 1).unwrap(),
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        let c = session
            .apply(Command::AddScene { name: "C".into() })
            .unwrap()
            .created
            .unwrap();
        owner
            .quick(
                &mut session,
                QuickTransitionId::new(3).unwrap(),
                MediaTime(0),
            )
            .unwrap();
        owner.sync(&mut session, MediaTime(300_000_000)).unwrap();
        owner
            .select_preview(&mut session, c, MediaTime(300_000_000))
            .unwrap();
        let replacement =
            skylive_core::ImageAsset::new("C:/saved.gif".into(), [2; 32], 1, 1).unwrap();
        session
            .apply(Command::SetImageSource {
                source,
                asset: replacement.clone(),
            })
            .unwrap();
        owner.before_change(&session).unwrap();
        session
            .apply(Command::RemoveItem { scene: b, item })
            .unwrap();
        let black = owner.sync(&mut session, MediaTime(300_000_001)).unwrap();
        assert!(black.output.scenes().is_empty());
        let saved = owner.ftb_return.as_ref().unwrap();
        assert_eq!(
            saved.source(&session.snapshot(), source).unwrap().kind(),
            &SourceKind::Image { asset: replacement }
        );
        let mut clock = crate::playback::PlaybackClock::new();
        let views = vec![FrameScene::Live(c)];
        clock
            .update_outputs(&session, &views, MediaTime(300_000_001))
            .unwrap();
        let later = clock
            .update_outputs(&session, &views, MediaTime(2_000_000_000))
            .unwrap();
        assert_eq!(later.elapsed(source), MediaTime(0));
        owner
            .quick(
                &mut session,
                QuickTransitionId::new(3).unwrap(),
                MediaTime(2_000_000_001),
            )
            .unwrap();
        owner.before_change(&session).unwrap();
        session.apply(Command::RemoveScene { scene: b }).unwrap();
        let restored = owner.sync(&mut session, MediaTime(2_300_000_001)).unwrap();
        assert!(
            matches!(restored.output,FrameComposition::SceneAlias{scene,logical_scene,..}if scene.scene()==b&&logical_scene==c)
        );
    }
    #[test]
    fn manual_cancellation_keeps_original_private_layout_and_latest_removed_source() {
        let (mut session, a, b) = scenes();
        let item = session
            .apply(Command::AddColorSource {
                scene: a,
                name: "A color".into(),
                rgba: [255; 4],
            })
            .unwrap()
            .created
            .unwrap();
        let source = session.snapshot().sources()[0].id();
        let mut owner = Controller::new();
        owner.set_studio(&mut session, true, MediaTime(0)).unwrap();
        let original = owner.program.as_ref().unwrap().key();
        owner.select_preview(&mut session, b, MediaTime(0)).unwrap();
        owner
            .manual_position(&mut session, 512, MediaTime(0))
            .unwrap();
        session
            .apply(Command::SetColor {
                source,
                rgba: [1, 2, 3, 255],
            })
            .unwrap();
        owner.before_change(&session).unwrap();
        session
            .apply(Command::RemoveItem { scene: a, item })
            .unwrap();
        owner
            .manual_position(&mut session, 102, MediaTime(100_000_000))
            .unwrap();
        owner
            .manual_release(&mut session, MediaTime(100_000_000))
            .unwrap();
        let snapshot = owner.sync(&mut session, MediaTime(100_000_000)).unwrap();
        let FrameComposition::Scene(view) = snapshot.output else {
            panic!()
        };
        assert_eq!(view.key(), original);
        assert_eq!(view.items(&session.snapshot()).unwrap().len(), 1);
        assert!(matches!(
            view.source(&session.snapshot(), source).unwrap().kind(),
            SourceKind::Color {
                rgba: [1, 2, 3, 255],
                ..
            }
        ));
        assert_eq!(session.program(), a);
    }
    #[test]
    fn manual_invalid_busy_time_overflow_and_epoch_overflow_are_atomic() {
        let (mut session, mut owner, a, b) = studio_pair();
        let epoch = owner.epoch;
        assert_eq!(
            owner.manual_position(&mut session, 1024, MediaTime(1)),
            Err(TransitionError::InvalidPosition(1024))
        );
        assert_eq!(owner.now(), MediaTime(0));
        assert_eq!(owner.epoch, epoch);
        assert_eq!((session.preview(), session.program()), (b, a));
        let authored = session.snapshot();
        let program_key = owner.program.as_ref().unwrap().key();
        assert_eq!(
            owner.manual_position(&mut session, 512, MediaTime(u64::MAX)),
            Err(TransitionError::TimeOverflow)
        );
        assert_eq!(owner.now(), MediaTime(0));
        assert!(!owner.active());
        assert_eq!(owner.epoch, epoch);
        assert_eq!((session.preview(), session.program()), (b, a));
        assert_eq!(owner.program.as_ref().unwrap().key(), program_key);
        assert!(Arc::ptr_eq(&authored, &session.snapshot()));
        owner.epoch = u64::MAX;
        assert_eq!(
            owner.manual_position(&mut session, 512, MediaTime(0)),
            Err(TransitionError::IdentityExhausted)
        );
        owner.epoch = epoch;
        owner.transition(&mut session, MediaTime(0)).unwrap();
        assert_eq!(
            owner.manual_position(&mut session, 512, MediaTime(1)),
            Err(TransitionError::Busy)
        );
        assert_eq!(owner.now(), MediaTime(0));
        owner.sync(&mut session, MediaTime(300_000_000)).unwrap();
        owner
            .manual_position(&mut session, 512, MediaTime(300_000_000))
            .unwrap();
        assert_eq!(
            owner.quick(
                &mut session,
                QuickTransitionId::new(2).unwrap(),
                MediaTime(300_000_001)
            ),
            Err(TransitionError::Busy)
        );
        assert_eq!(
            owner.quick(
                &mut session,
                QuickTransitionId::new(500).unwrap(),
                MediaTime(300_000_001)
            ),
            Err(TransitionError::UnknownQuick(
                QuickTransitionId::new(500).unwrap()
            ))
        );
    }
    #[test]
    fn manual_discontinuity_reopen_exit_and_close_retire_all_private_runtime_state() {
        let (mut session, mut owner, _, b) = studio_pair();
        owner
            .manual_position(&mut session, 512, MediaTime(1))
            .unwrap();
        assert!(
            owner
                .sync(&mut session, MediaTime(100_000_000))
                .unwrap()
                .manual
                .is_some()
        );
        let backward = owner.sync(&mut session, MediaTime(0)).unwrap();
        assert!(!backward.active && backward.manual.is_none());
        assert_eq!(session.program(), b);
        owner
            .manual_position(&mut session, 512, MediaTime(1))
            .unwrap();
        let mut reopened = Session::from_snapshot((*session.snapshot()).clone(), true).unwrap();
        let fresh = owner.sync(&mut reopened, MediaTime(2)).unwrap();
        assert!(!fresh.active && fresh.manual.is_none());
        owner
            .manual_position(&mut reopened, 512, MediaTime(3))
            .unwrap();
        owner
            .set_studio(&mut reopened, false, MediaTime(4))
            .unwrap();
        assert!(owner.manual.is_none() && owner.ftb_return.is_none());
        assert!(matches!(owner.program, Some(FrameScene::Live(_))));
        owner.set_studio(&mut reopened, true, MediaTime(5)).unwrap();
        owner
            .manual_position(&mut reopened, 512, MediaTime(6))
            .unwrap();
        reopened.close();
        assert_eq!(
            owner.sync(&mut reopened, MediaTime(7)).unwrap_err(),
            TransitionError::Closed
        );
        assert!(
            owner.program.is_none()
                && owner.fade.is_none()
                && owner.manual.is_none()
                && owner.ftb_return.is_none()
        );
    }
}
