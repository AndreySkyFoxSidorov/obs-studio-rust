//! Window-owned typed OBS object clipboard; no OS or persistence dependencies.
use skylive_core::{Command, Id, Session, SourcePasteItem};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    CopyTransform,
    PasteTransform,
    CopyFilters,
    PasteFilters,
}
impl Action {
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "actionCopyTransform" => Self::CopyTransform,
            "actionPasteTransform" => Self::PasteTransform,
            "actionCopyFilters" => Self::CopyFilters,
            "actionPasteFilters" => Self::PasteFilters,
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub session: u64,
    pub revision: u64,
    pub preview: Id,
    pub container: Id,
    pub item: Id,
    pub source: Id,
}
#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub action: Action,
    pub target: Target,
    pub generation: u64,
}
/// Target the exact mixer source, independent of selected scene items.
#[derive(Clone, Copy, Debug)]
pub struct FilterRequest {action:Action,session:u64,revision:u64,target:FilterTarget,generation:u64}
#[derive(Clone, Copy, Debug)]
enum FilterTarget { Source(Id), Scene(Id) }
#[derive(Clone, Copy)]
struct FilterMenu {session:u64,preview:Id,generation:u64,copied:Option<Id>,has_transform:bool}
/// Bind a transform request to the item whose context menu is open, not global selection.
pub fn context_transform_request(context:&eframe::egui::Context,model:&skylive_core::StudioSnapshot,session:u64,container:Id,item:Id,action:Action)->Option<Request>{
    let menu=context.data(|data|data.get_temp::<FilterMenu>(eframe::egui::Id::new("mixer-filter-clipboard")))?;
    if menu.session!=session || model.root_scene_for_container(container)!=Some(menu.preview){return None;}
    let (actual_container,entry)=model.item_container(item)?;
    if actual_container!=container{return None;}
    let source=model.source(entry.source())?;
    model.source_video_dimensions(source.kind()).ok().flatten()?;
    match action {
        Action::CopyTransform=>{},
        Action::PasteTransform if menu.has_transform && !entry.locked() && !model.container_is_locked(container)=>{},
        _=>return None,
    }
    Some(Request{action,generation:menu.generation,target:Target{session,revision:model.revision(),preview:menu.preview,container,item,source:entry.source()}})
}
/// Pure UI admission using a small projection installed by the clipboard owner.
pub fn mixer_request(context:&eframe::egui::Context,model:&skylive_core::StudioSnapshot,session:u64,source:Id,action:Action)->Option<FilterRequest>{
    let menu=context.data(|data|data.get_temp::<FilterMenu>(eframe::egui::Id::new("mixer-filter-clipboard")))?;
    if menu.session!=session{return None;}
    let target=model.source(source)?;
    match action {
        Action::CopyFilters if !target.audio_filters().is_empty()||!target.video_filters().is_empty()=>{},
        Action::PasteFilters=>{let copied=menu.copied?;model.check_filter_paste(copied,source).ok()?;},
        _=>return None,
    }
    Some(FilterRequest{action,session,revision:model.revision(),target:FilterTarget::Source(source),generation:menu.generation})
}
/// A scene can have no source owner until its first filter is applied.
pub fn scene_request(context:&eframe::egui::Context,model:&skylive_core::StudioSnapshot,session:u64,scene:Id,action:Action)->Option<FilterRequest>{
    let menu=context.data(|data|data.get_temp::<FilterMenu>(eframe::egui::Id::new("mixer-filter-clipboard")))?;
    if menu.session!=session || model.scene(scene).is_none(){return None;}
    match action {
        Action::CopyFilters=>{let source=model.scene_source(scene)?;if source.video_filters().is_empty()&&source.audio_filters().is_empty(){return None;}},
        Action::PasteFilters=>{
            let copied=menu.copied?;
            let source=model.source(copied)?;
            if source.video_filters().iter().any(|filter| matches!(filter.effect,skylive_core::VideoEffect::AsyncVideoDelay{..})){return None;}
            if let Some(target)=model.scene_source(scene){model.check_filter_paste(copied,target.id()).ok()?;}
            else {
                // A scene owner is created atomically by PasteSceneFilters.
                // Its audio bus supports Gain and Invert Polarity as well as
                // video filters, so a new scene must admit the same supported
                // clipboard content as a scene which already has an owner.
                if (source.audio_filters().is_empty() && source.video_filters().is_empty())
                    || source.audio_filters().iter().any(|filter| !matches!(filter,
                        skylive_core::AudioFilter::ObsGain { .. } | skylive_core::AudioFilter::ObsPolarityInvert { .. }))
                    || source.audio_filters().iter().any(|filter| !model.preferences().plugin_enabled(filter.plugin_id()))
                    || source.video_filters().iter().any(|filter|!model.preferences().plugin_enabled(filter.plugin_id())) {return None;}
            }
        },
        _=>return None,
    }
    Some(FilterRequest{action,session,revision:model.revision(),target:FilterTarget::Scene(scene),generation:menu.generation})
}
#[derive(Clone)]
struct TransformValue {
    value: SourcePasteItem,
}
#[derive(Clone, Copy)]
struct FilterSource {
    session: u64,
    source: Id,
}
#[derive(Default)]
pub struct Clipboard {
    generation: u64,
    transform: Option<TransformValue>,
    filters: Option<FilterSource>,
}
pub enum Outcome {
    Copied(&'static str),
    Command { revision: u64, command: Command },
}
enum Prepared {
    Transform(TransformValue),
    Filters(FilterSource),
    Paste(Command),
}
#[derive(Debug)]
pub enum Error {
    Message(&'static str),
    Core(skylive_core::Error),
}
impl Error {
    pub fn chrome(&self) -> Option<&'static str> {
        match self {
            Self::Message(text)
            | Self::Core(
                skylive_core::Error::Invalid(text)
                | skylive_core::Error::Limit(text)
                | skylive_core::Error::Unavailable(text),
            ) => Some(text),
            _ => None,
        }
    }
}
impl From<&'static str> for Error {
    fn from(value: &'static str) -> Self {
        Self::Message(value)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(text) => f.write_str(text),
            Self::Core(error) => std::fmt::Display::fmt(error, f),
        }
    }
}
impl Clipboard {
    /// Install only the current weak clipboard identity for mixer menu gating.
    pub fn install_mixer(&self,context:&eframe::egui::Context,session:&Session){
        let copied=self.filters.filter(|value|value.session==session.identity()).map(|value|value.source);
        let menu=FilterMenu{session:session.identity(),preview:session.preview(),generation:self.generation,copied,has_transform:self.transform.is_some()};
        context.data_mut(|data|data.insert_temp(eframe::egui::Id::new("mixer-filter-clipboard"),menu));
    }
    pub fn execute_mixer(&mut self,request:FilterRequest,session:&Session)->Result<Outcome,Error>{
        self.synchronize(session)?;
        let model=session.snapshot();
        if session.is_closed()||request.session!=session.identity()||request.revision!=model.revision()||request.generation!=self.generation {
            return Err("The selection or clipboard changed. Repeat this action.".into());
        }
        let source=match request.target {
            FilterTarget::Source(id)=>Some(model.source(id).ok_or("Selected source no longer exists")?),
            FilterTarget::Scene(scene)=>{model.scene(scene).ok_or("Selected scene no longer exists")?;model.scene_source(scene)},
        };
        match request.action {
            Action::CopyFilters=>{
                let source=source.ok_or("No filters are available to copy.")?;
                if source.audio_filters().is_empty()&&source.video_filters().is_empty(){return Err("No filters are available to copy.".into());}
                let generation=self.generation.checked_add(1).ok_or("Clipboard identity exhausted.")?;
                self.filters=Some(FilterSource{session:session.identity(),source:source.id()});self.generation=generation;
                Ok(Outcome::Copied("Filter source copied."))
            }
            Action::PasteFilters=>{
                let copied=self.filters.ok_or("No copied filter source is available.")?;
                if copied.session!=session.identity(){return Err("The copied filter source belongs to another document.".into());}
                let command=match request.target {
                    FilterTarget::Source(to)=>{model.check_filter_paste(copied.source,to).map_err(Error::Core)?;Command::PasteSourceFilters{from:copied.source,to}},
                    FilterTarget::Scene(scene)=>{if let Some(target)=source {model.check_filter_paste(copied.source,target.id()).map_err(Error::Core)?;}Command::PasteSceneFilters{from:copied.source,scene}},
                };
                Ok(Outcome::Command{revision:request.revision,command})
            }
            _=>Err("This action requires a scene item.".into()),
        }
    }
    /// Called after every canonical publication and before rendering menus.
    /// An observed expired weak source must not resurrect after later Undo.
    pub fn synchronize(&mut self, session: &Session) -> Result<(), &'static str> {
        if self.filters.is_some_and(|copied| {
            copied.session != session.identity()
                || session.snapshot().source(copied.source).is_none()
        }) {
            self.filters = None;
            if let Some(next) = self.generation.checked_add(1) {
                self.generation = next;
            } else {
                self.transform = None;
                return Err("Clipboard identity exhausted.");
            }
        }
        Ok(())
    }
    pub fn request(
        &self,
        action: Action,
        session: &Session,
        selected: Option<Id>,
    ) -> Result<Request, Error> {
        let snapshot = session.snapshot();
        let (container, item) = selected
            .and_then(|id| snapshot.item_container(id))
            .ok_or("Select a source item before using the clipboard")?;
        let request = Request {
            action,
            generation: self.generation,
            target: Target {
                session: session.identity(),
                revision: snapshot.revision(),
                preview: session.preview(),
                container,
                item: item.id(),
                source: item.source(),
            },
        };
        self.prepare(request, session, selected)?;
        Ok(request)
    }
    fn prepare(
        &self,
        request: Request,
        session: &Session,
        selected: Option<Id>,
    ) -> Result<Prepared, Error> {
        let snapshot = session.snapshot();
        let target = request.target;
        if session.is_closed()
            || target.session != session.identity()
            || target.revision != snapshot.revision()
            || target.preview != session.preview()
            || request.generation != self.generation
            || selected != Some(target.item)
        {
            return Err("The selection or clipboard changed. Repeat this action.".into());
        }
        let (container, item) = snapshot
            .item_container(target.item)
            .ok_or("The selection or clipboard changed. Repeat this action.")?;
        if container != target.container
            || item.source() != target.source
            || snapshot.root_scene_for_container(container) != Some(target.preview)
        {
            return Err("The selection or clipboard changed. Repeat this action.".into());
        }
        let source = snapshot
            .source(target.source)
            .ok_or("Selected source no longer exists")?;
        match request.action {
            Action::CopyTransform => {
                snapshot.source_video_dimensions(source.kind()).map_err(Error::Core)?
                    .ok_or("Audio-only sources have no video transform to copy")?;
                Ok(Prepared::Transform(TransformValue {
                    value:snapshot.source_copy_value(item.id()).map_err(Error::Core)?,
                }))
            }
            Action::CopyFilters => Ok(Prepared::Filters(FilterSource {
                session: session.identity(),
                source: target.source,
            })),
            Action::PasteTransform => {
                let copied = self
                    .transform
                    .as_ref()
                    .ok_or("No copied transform is available.")?;
                if item.locked() || snapshot.container_is_locked(container) {
                    return Err("Unlock the selected item before pasting its transform".into());
                }
                let basis = snapshot
                    .source_video_dimensions(source.kind())
                    .map_err(Error::Core)?
                    .ok_or("Audio-only sources have no video transform to paste")?;
                let mut transform=copied.value.transform_for_basis(basis).map_err(Error::Core)?;
                // OBS info2 contains scale/crop/rotation/position, not opacity.
                transform.opacity = item.transform().opacity;
                snapshot.source_item_geometry(source,&transform).map_err(Error::Core)?;
                Ok(Prepared::Paste(Command::SetTransform {
                    scene: container,
                    item: target.item,
                    transform,
                }))
            }
            Action::PasteFilters => {
                let copied = self
                    .filters
                    .ok_or("No copied filter source is available.")?;
                if copied.session != session.identity() {
                    return Err("The copied filter source belongs to another document.".into());
                }
                if snapshot.source(copied.source).is_none() {
                    return Err("The copied filter source no longer exists.".into());
                }
                snapshot
                    .check_filter_paste(copied.source, target.source)
                    .map_err(Error::Core)?;
                Ok(Prepared::Paste(Command::PasteSourceFilters {
                    from: copied.source,
                    to: target.source,
                }))
            }
        }
    }
    pub fn execute(
        &mut self,
        request: Request,
        session: &Session,
        selected: Option<Id>,
    ) -> Result<Outcome, Error> {
        self.synchronize(session)?;
        let prepared = self.prepare(request, session, selected)?;
        match prepared {
            Prepared::Paste(command) => Ok(Outcome::Command {
                revision: request.target.revision,
                command,
            }),
            Prepared::Transform(value) => {
                let generation = self
                    .generation
                    .checked_add(1)
                    .ok_or("Clipboard identity exhausted.")?;
                self.transform = Some(value);
                self.generation = generation;
                Ok(Outcome::Copied("Transform copied."))
            }
            Prepared::Filters(value) => {
                let generation = self
                    .generation
                    .checked_add(1)
                    .ok_or("Clipboard identity exhausted.")?;
                self.filters = Some(value);
                self.generation = generation;
                Ok(Outcome::Copied("Filter source copied."))
            }
        }
    }
}
#[cfg(test)]
#[path = "edit_clipboard_tests.rs"]
mod tests;
