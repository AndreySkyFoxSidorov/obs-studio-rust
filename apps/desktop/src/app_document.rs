//! Guarded document replacement: candidate IO, canonical publication, explicit native binding.
use super::*;
use crate::views::document::{Choice, Prompt};
pub(super) enum Replacement {
    LibraryCreate {label:String},
    LibraryOpen {label:String},
    LibraryRemove {label:String},
    New,
    Load { path: PathBuf, recover: bool },
    Import { path: PathBuf, video: ImportVideo },
}
struct Request {
    request: u64,
    session: u64,
    revision: u64,
    cancelled: bool,
    kind: Replacement,
}
enum Prepared {
    RepairReady(super::document_repair_flow::Ready),
    ManagedCreate(skylive_host::collection_library::CreatePlan),
    ManagedRemove {loaded:Box<skylive_host::storage::LoadedDocument>,path:PathBuf,removal:skylive_host::collection_library::RemovePlan},
    New(Session),
    Loaded {
        loaded: Box<skylive_host::storage::LoadedDocument>,
        path: PathBuf,
    },
    Imported(Box<skylive_host::obs::ImportedCollection>),
}
struct Candidate {
    session: u64,
    revision: u64,
    label: String,
    value: Prepared,
}
struct Save {
    request: u64,
    session: u64,
    revision: u64,
}
struct ObsCameraProbe {
    prepared: skylive_host::obs::PreparedCameraImport,
    catalog: Vec<skylive_host::obs::CameraImportDevice>,
    next: usize,
    pending: Option<skylive_host::camera_capture::CameraRequestKey>,
    session:u64,
    revision:u64,
    kind:Replacement,
    preferences:skylive_core::UserPreferences,
}
#[derive(Default)]
pub(super) struct Flow {
    repair_saved:Option<(PathBuf,skylive_host::storage::SavedFileStamp)>,
    creation:Option<skylive_host::collection_library::CreationLease>,
    remember_open:Option<PathBuf>,
    request: Option<Request>,
    candidate: Option<Candidate>,
    save: Option<Save>,
    pub prompt: Option<Prompt>,
    save_after: Option<AfterIo>,
    obs_camera: Option<ObsCameraProbe>,
    // A native query can finish after the user cancels. Retain its identity so
    // the result is discarded without consuming a different camera dialog's
    // pending request; cancellation does not claim native work has stopped.
    abandoned_obs_camera: Option<skylive_host::camera_capture::CameraRequestKey>,
}
impl Flow {
    pub(super) fn saving(&self)->bool {self.save.is_some()}
    fn abandon_obs_camera(&mut self) -> bool {
        let Some(probe) = self.obs_camera.take() else { return false; };
        if let Some(key) = probe.pending { self.abandoned_obs_camera = Some(key); }
        true
    }
    pub fn modal_open(&self) -> bool {
        self.prompt.is_some() || self.obs_camera.is_some()
    }
    pub fn cancel(&mut self) {
        self.repair_saved=None;
        self.remember_open=None;
        self.candidate = None;
        self.creation=None;
        self.prompt = None;
        self.save_after = None;
        self.abandon_obs_camera();
        if let Some(request) = &mut self.request {
            request.cancelled = true;
        }
    }
}
impl StudioApp {
    pub(super) fn fail_obs_camera_import(&mut self,detail:&str){
        if self.documents.abandon_obs_camera(){self.message(true,format!("OBS camera discovery failed: {detail}"));}
    }
    pub(super) fn owns_obs_camera_request(&self,key:skylive_host::camera_capture::CameraRequestKey)->bool {
        self.documents.abandoned_obs_camera==Some(key)
            || self.documents.obs_camera.as_ref().is_some_and(|probe|probe.pending==Some(key))
    }
    pub(super) fn obs_camera_modes(&mut self,key:skylive_host::camera_capture::CameraRequestKey,target:skylive_core::VideoCaptureTarget,result:Result<Vec<skylive_host::camera_capture::CameraModeInfo>,skylive_host::camera_capture::CameraError>,context:&egui::Context){
        if self.documents.abandoned_obs_camera==Some(key) {
            self.documents.abandoned_obs_camera=None;
            return;
        }
        let Some(mut probe)=self.documents.obs_camera.take() else{return;};
        let current=probe.pending==Some(key) && probe.session==self.session.identity() && probe.revision==self.session.snapshot().revision()
            && key.session==probe.session && key.revision==probe.revision
            && probe.prepared.targets().get(probe.next)==Some(&target) && !self.session.is_closed();
        if !current {self.message(true,"OBS camera discovery became obsolete; the current document was retained");return;}
        let modes=match result{Ok(modes)=>modes,Err(error)=>{self.message(true,format!("OBS camera discovery failed: {error}"));return;}};
        match skylive_host::obs::CameraImportDevice::from_owner_modes(target.clone(),&modes){
            Ok(entry)=>probe.catalog.push(entry),
            Err(error)=>{self.message(true,error.to_string());return;}
        }
        if !probe.prepared.needs_preferred(&target) && !probe.prepared.needs_video_only(&target){probe.next+=1;}
        probe.pending=None;self.documents.obs_camera=Some(probe);
        self.progress_obs_camera_import(context);
    }
    pub(super) fn obs_camera_preferred(&mut self,key:skylive_host::camera_capture::CameraRequestKey,target:skylive_core::VideoCaptureTarget,result:Result<skylive_host::camera_capture::CameraModeInfo,skylive_host::camera_capture::CameraError>,context:&egui::Context){
        if self.documents.abandoned_obs_camera==Some(key){self.documents.abandoned_obs_camera=None;return;}
        let Some(mut probe)=self.documents.obs_camera.take() else{return;};
        let current=probe.pending==Some(key) && probe.session==self.session.identity() && probe.revision==self.session.snapshot().revision()
            && key.session==probe.session && key.revision==probe.revision && probe.prepared.targets().get(probe.next)==Some(&target) && !self.session.is_closed();
        if !current {self.message(true,"OBS camera discovery became obsolete; the current document was retained");return;}
        let preferred=match result{Ok(value)=>value,Err(error)=>{self.message(true,format!("OBS camera preferred discovery failed: {error}"));return;}};
        let Some(entry)=probe.catalog.get_mut(probe.next) else {self.message(true,"OBS camera preferred catalog is incomplete");return;};
        match entry.clone().with_preferred(&preferred){Ok(value)=>*entry=value,Err(error)=>{self.message(true,error.to_string());return;}}
        if !probe.prepared.needs_video_only(&target){probe.next+=1;}
        probe.pending=None;self.documents.obs_camera=Some(probe);
        self.progress_obs_camera_import(context);
    }
    pub(super) fn obs_camera_video_only(&mut self,key:skylive_host::camera_capture::CameraRequestKey,target:skylive_core::VideoCaptureTarget,result:Result<bool,skylive_host::camera_capture::CameraError>,context:&egui::Context){
        if self.documents.abandoned_obs_camera==Some(key){self.documents.abandoned_obs_camera=None;return;}
        let Some(mut probe)=self.documents.obs_camera.take() else{return;};
        if probe.pending!=Some(key)||probe.session!=self.session.identity()||probe.revision!=self.session.snapshot().revision()||key.session!=probe.session||key.revision!=probe.revision||probe.prepared.targets().get(probe.next)!=Some(&target)||self.session.is_closed(){self.message(true,"OBS camera audio discovery became obsolete; the current document was retained");return;}
        let video_only=match result{Ok(value)=>value,Err(error)=>{self.message(true,format!("OBS camera audio discovery failed: {error}"));return;}};
        let Some(entry)=probe.catalog.get_mut(probe.next) else {self.message(true,"OBS camera audio catalog is incomplete");return;};
        entry.video_only=video_only;entry.embedded_audio=!video_only;probe.next+=1;probe.pending=None;self.documents.obs_camera=Some(probe);self.progress_obs_camera_import(context);
    }
    pub(super) fn progress_obs_camera_import(&mut self,context:&egui::Context){
        let Some(mut probe)=self.documents.obs_camera.take() else{return;};
        if self.session.is_closed() || self.session.identity()!=probe.session || self.session.snapshot().revision()!=probe.revision {
            if let Some(key)=probe.pending {self.documents.abandoned_obs_camera=Some(key);}
            self.message(true,"OBS camera discovery became obsolete; the current document was retained");return;
        }
        if probe.pending.is_some(){self.documents.obs_camera=Some(probe);return;}
        if let Some(target)=probe.prepared.targets().get(probe.next).cloned(){
            if self.camera.is_busy() || self.camera_stop || self.camera_start.is_some(){self.documents.obs_camera=Some(probe);context.request_repaint_after(std::time::Duration::from_millis(20));return;}
            let request=if probe.catalog.len()>probe.next && probe.prepared.needs_preferred(&target) && probe.catalog[probe.next].preferred.is_none() {
                self.camera.request_directshow_preferred(&self.session,target)
            }else if probe.catalog.len()>probe.next && probe.prepared.needs_video_only(&target) {
                self.camera.request_directshow_video_only(&self.session,target)
            }else{self.camera.request_modes(&self.session,target)};
            match request{
                Ok(key)=>{probe.pending=Some(key);self.documents.obs_camera=Some(probe);context.request_repaint_after(std::time::Duration::from_millis(20));}
                Err(skylive_host::camera_capture::CameraError::Busy)=>{self.documents.obs_camera=Some(probe);context.request_repaint_after(std::time::Duration::from_millis(20));}
                Err(error)=>self.message(true,format!("OBS camera discovery failed: {error}")),
            }
            return;
        }
        if self.io.busy(){self.documents.obs_camera=Some(probe);context.request_repaint_after(std::time::Duration::from_millis(20));return;}
        let Replacement::Import{video,..}=&probe.kind else {self.message(true,"Unexpected OBS camera import state");return;};
        let job=Job::ImportObsPrepared{prepared:probe.prepared,video:*video,preferences:probe.preferences,catalog:probe.catalog};
        match self.io.submit(probe.session,job){
            Ok(())=>{if let Some(request)=self.io.request(){self.documents.request=Some(Request{request,session:probe.session,revision:probe.revision,cancelled:false,kind:probe.kind});self.after_io=Some(AfterIo::ReplaceDocument);self.message(false,"Reading and validating replacement document…");}},
            Err(error)=>self.message(true,error),
        }
        context.request_repaint();
    }
    pub(super) fn open_document_picker(&mut self, action: views::document_picker::DocumentAction) {
        if self.io.busy() || self.documents.modal_open() || self.session.is_closed() {
            return;
        }
        if !self.next_browse_epoch() { return; }
        self.documents.prompt = Some(Prompt::Choose(views::document_picker::PathDraft {
            action,
            path: if action == views::document_picker::DocumentAction::ImportObs {
                self.obs_path.clone()
            } else {
                self.document_path.clone()
            },
            session: self.session.identity(),
            revision: self.session.snapshot().revision(),
        }));
    }
    pub(super) fn cancel_document_picker(&mut self) {
        if matches!(self.documents.prompt, Some(Prompt::Choose(..))) {
            self.documents.prompt = None;
        }
    }
    pub(super) fn confirm_document_path(
        &mut self,
        draft: views::document_picker::PathDraft,
        context: &egui::Context,
    ) {
        if !matches!(&self.documents.prompt,Some(Prompt::Choose(current)) if current==&draft) {
            self.message(
                true,
                "The file chooser changed. Reopen it before continuing.",
            );
            return;
        }
        if draft.session != self.session.identity()
            || draft.revision != self.session.snapshot().revision()
            || self.session.is_closed()
        {
            self.message(true, "The document changed. Reopen the file chooser.");
            return;
        }
        if draft.path.trim().is_empty() || draft.path.chars().count() > 4096 {
            self.message(true, "Enter a file path of at most 4096 characters.");
            return;
        }
        if self.recording.active() || self.streaming.active() || self.replay.active() || self.ndi_output.owner.any_active() {
            self.message(true, "Stop active outputs before replacing the collection.");
            return;
        }
        self.documents.prompt = None;
        match draft.action {
            views::document_picker::DocumentAction::Open => {
                self.document_path = draft.path;
                self.intent(Intent::Load, context);
            }
            views::document_picker::DocumentAction::Recover => {
                self.document_path = draft.path;
                self.intent(Intent::Recover, context);
            }
            views::document_picker::DocumentAction::ImportObs => {
                self.intent(Intent::ImportObs { path: draft.path }, context)
            }
        }
    }
    pub(super) fn request_replacement(&mut self, kind: Replacement, context: &egui::Context) {
        if self.ndi_output.owner.any_active(){self.message(true,"Stop NDI outputs before replacing the scene collection");return;}
        if self.virtual_camera.owned() {
            self.message(true,"Stop the virtual camera before replacing the scene collection");return;
        }

        if self.io.busy() || self.documents.modal_open() {
            self.message(
                true,
                "Finish or cancel the current document operation first",
            );
            return;
        }
        let session = self.session.identity();
        let revision = self.session.snapshot().revision();
        if matches!(kind, Replacement::New) {
            match self.session.new_collection() {
                Ok(value) => self.document_candidate(
                    Candidate {
                        session,
                        revision,
                        label: crate::i18n::tr_context(context, "New collection"),
                        value: Prepared::New(value),
                    },
                    context,
                ),
                Err(e) => self.message(true, e.to_string()),
            }
            return;
        }
        let job = match &kind {
            Replacement::Load { path, recover } => {
                if *recover {
                    Job::Recover(path.clone())
                } else {
                    Job::Load(path.clone())
                }
            }
            Replacement::Import { path, .. } => Job::PrepareObsCamera {path:path.clone()},
            Replacement::New => unreachable!(),
            Replacement::LibraryCreate {..}|Replacement::LibraryOpen {..}|Replacement::LibraryRemove {..}=>{self.message(true,"Use the typed managed collection workflow.");return;}
        };
        match self.io.submit(session, job) {
            Ok(()) => {
                self.documents.request = Some(Request {
                    request: self.io.request().unwrap(),
                    session,
                    revision,
                    cancelled: false,
                    kind,
                });
                self.after_io = Some(AfterIo::ReplaceDocument);
                self.message(false, "Reading and validating replacement document…");
            }
            Err(e) => self.message(true, e),
        }
        context.request_repaint();
    }
    pub(super) fn document_completion(
        &mut self,
        completion: crate::io::Completion,
        context: &egui::Context,
    ) -> Option<crate::io::Completion> {
        if self
            .documents
            .request
            .as_ref()
            .is_none_or(|pending| pending.request != completion.request)
        {
            return Some(completion);
        }
        self.after_io = None;
        let pending = self.documents.request.take().unwrap();
        let fresh=!pending.cancelled&&pending.session==self.session.identity()&&pending.revision==self.session.snapshot().revision()&&(completion.session==pending.session||completion.session==0)&&!self.session.is_closed();
        if let Ok(Reply::DocumentRepair(_))=&completion.result{
            if let Ok(Reply::DocumentRepair(event))=completion.result{self.documents.remember_open=None;self.begin_document_repair(event,false,fresh,context);}return None;
        }
        self.initial_repair_failed();
        if pending.cancelled
            || pending.session != self.session.identity()
            || pending.revision != self.session.snapshot().revision()
            || (completion.session != pending.session && completion.session != 0)
            || self.session.is_closed()
        {
            if let Ok(Reply::LibraryCreated(created))=&completion.result{
                self.library_view(created.view.clone());self.message(true,"Replacement result is obsolete; the current document was retained");self.library_created_not_published(created.path.clone());
            }else{self.message(true,"Replacement result is obsolete; the current document was retained");}
            return None;
        }
        let label = match &pending.kind {
            Replacement::Load { path, .. } | Replacement::Import { path, .. } => {
                path.display().to_string()
            }
            Replacement::New => crate::i18n::tr_context(context, "New collection"),
            Replacement::LibraryCreate{label}|Replacement::LibraryOpen{label}|Replacement::LibraryRemove{label}=>label.clone(),
        };
        match completion.result {
            Ok(Reply::ObsCameraPrepared(prepared)) if matches!(&pending.kind,Replacement::Import{..})=>{
                self.documents.obs_camera=Some(ObsCameraProbe{prepared,catalog:Vec::new(),next:0,pending:None,session:pending.session,revision:pending.revision,kind:pending.kind,preferences:self.session.snapshot().preferences().clone()});
                self.progress_obs_camera_import(context);
            },
            Ok(Reply::LibraryCreated(mut opened))if matches!(&pending.kind,Replacement::LibraryCreate{..})=>{
                let retained_path=opened.path.clone();let previous=self.session.identity();self.documents.creation=opened.take_creation_lease();
                self.library_view(opened.view);self.documents.remember_open=Some(opened.path.clone());
                self.documents.candidate=Some(Candidate{session:pending.session,revision:pending.revision,label,value:Prepared::Loaded{loaded:opened.loaded,path:opened.path}});
                // Dirty choice already resolved BEFORE Create's disk admission.
                self.publish_document(context);if self.session.identity()==previous{self.library_created_not_published(retained_path);}
            },
            Ok(Reply::LibraryOpened(opened))if matches!(&pending.kind,Replacement::LibraryOpen{..})=>{
                self.library_view(opened.view);self.documents.remember_open=Some(opened.path.clone());
                self.document_candidate(Candidate{session:pending.session,revision:pending.revision,label,value:Prepared::Loaded{loaded:opened.loaded,path:opened.path}},context);
            },
            Ok(Reply::LibraryRemovalPrepared(prepared))if matches!(&pending.kind,Replacement::LibraryRemove{..})=>{
                let opened=prepared.alternative;self.library_view(opened.view);self.documents.remember_open=Some(opened.path.clone());
                self.document_candidate(Candidate{session:pending.session,revision:pending.revision,label,value:Prepared::ManagedRemove{loaded:opened.loaded,path:opened.path,removal:prepared.removal}},context);
            },
            Ok(Reply::Loaded { loaded, path }) => {
                self.documents.remember_open=matches!(&pending.kind,Replacement::Load{recover:false,..}).then(||path.clone());
                self.document_candidate(
                Candidate {
                    session: pending.session,
                    revision: pending.revision,
                    label,
                    value: Prepared::Loaded { loaded, path },
                },
                context,
            );},
            Ok(Reply::Imported(value)) => self.document_candidate(
                Candidate {
                    session: pending.session,
                    revision: pending.revision,
                    label,
                    value: Prepared::Imported(value),
                },
                context,
            ),
            Err(error) => self.message(true, error),
            _ => self.message(
                true,
                "Unexpected replacement result; the current document was retained",
            ),
        }
        None
    }
    /// Boot candidate uses the exact same native preference adoption/publication
    /// and projector restore path. It never prompts to discard authored changes.
    pub(super) fn accept_startup_document(&mut self,native:crate::startup::Native,context:&egui::Context)->Option<crate::startup::Native>{
        if self.session.is_closed()||self.session.is_document_dirty()||self.documents.modal_open(){return Some(native);}
        self.documents.remember_open=None;
        self.document_candidate(Candidate{session:self.session.identity(),revision:self.session.snapshot().revision(),label:native.path.display().to_string(),value:Prepared::Loaded{loaded:native.loaded,path:native.path}},context);
        None
    }
    pub(super) fn request_library_create(&mut self,plan:skylive_host::collection_library::CreatePlan,context:&egui::Context){
        if self.io.busy()||self.documents.modal_open(){self.message(true,"Finish the active document operation first.");return;}
        self.documents.remember_open=None;self.document_candidate(Candidate{session:self.session.identity(),revision:self.session.snapshot().revision(),label:plan.name().to_owned(),value:Prepared::ManagedCreate(plan)},context);
    }
    fn library_document_io(&mut self,kind:Replacement,job:Job,context:&egui::Context){
        let session=self.session.identity();let revision=self.session.snapshot().revision();
        match self.io.submit(session,job){Ok(())=>{if let Some(request)=self.io.request(){self.documents.request=Some(Request{request,session,revision,cancelled:false,kind});self.after_io=Some(AfterIo::ReplaceDocument);self.message(false,"Preparing the managed collection operation…");}},Err(error)=>self.message(true,error)}context.request_repaint();
    }
    pub(super) fn request_library_open(&mut self,view:skylive_host::collection_library::View,id:skylive_host::collection_library::EntryId,context:&egui::Context){
        let Some(entry)=view.entry(id)else{self.message(true,"The managed collection no longer exists. Refresh the list.");return;};let label=entry.name.clone();self.library_document_io(Replacement::LibraryOpen{label},Job::LibraryOpen{view,id},context);
    }
    pub(super) fn request_library_remove(&mut self,view:skylive_host::collection_library::View,path:PathBuf,context:&egui::Context){
        let label=crate::i18n::tr_context(context,"Switch before removing the managed collection");self.library_document_io(Replacement::LibraryRemove{label},Job::LibraryPrepareRemove{view,path},context);
    }
    pub(super) fn refresh_library_removal_stamp(&mut self,path:&std::path::Path,stamp:skylive_host::storage::SavedFileStamp){
        if self.has_repair_document_candidate(){self.documents.repair_saved=Some((path.to_owned(),stamp));}
        if let Some(candidate)=&mut self.documents.candidate{if candidate.session==self.session.identity()&&candidate.revision==self.session.snapshot().revision(){if let Prepared::ManagedRemove{removal,..}=&mut candidate.value{removal.refresh_accepted_save(path,stamp);}}}
    }
    fn document_candidate(&mut self, candidate: Candidate, context: &egui::Context) {
        self.documents.candidate = Some(candidate);
        self.documents.creation=None;
        if self.session.is_document_dirty() {
            let label = self.documents.candidate.as_ref().unwrap().label.clone();
            self.documents.prompt = Some(Prompt::Replace { target: label });
            context.request_repaint();
        } else {
            self.publish_document(context);
        }
    }
    pub(super) fn resolve_document(&mut self, choice: Choice, context: &egui::Context) {
        if self.io.busy() {
            return;
        }
        if !matches!(self.documents.prompt, Some(Prompt::Replace { .. })) {
            return;
        }
        match choice {
            Choice::Cancel => {
                self.documents.cancel();
                context.request_repaint();
            }
            Choice::Discard => self.publish_document(context),
            Choice::Save => {
                self.documents.prompt = None;
                self.save(Some(AfterIo::SaveAndReplace));
            }
        }
    }
    pub(super) fn cancel_document_save(&mut self) {
        self.documents.cancel();
    }
    pub(super) fn open_save_as(&mut self, after: Option<AfterIo>) {
        if !self.next_browse_epoch() { return; }
        self.documents.save_after = after;
        self.documents.prompt = Some(Prompt::SaveAs {
            path: self.native_save_draft.clone(),
        });
    }
    pub(super) fn save_to(&mut self, path: PathBuf, after: Option<AfterIo>) {
        if path.as_os_str().is_empty() {
            self.message(true, "Enter a native save destination");
            return;
        }
        let before=self.session.snapshot().revision();
        if !self.capture_projector_windows(){return;}
        let after_metadata=self.session.snapshot().revision();
        if let Some(candidate)=&mut self.documents.candidate{if candidate.session==self.session.identity()&&candidate.revision==before{candidate.revision=after_metadata;}}
        let session = self.session.identity();
        let revision = self.session.snapshot().revision();
        let job = Job::Save {
            job: SaveJob::new(&self.session, path.clone()),
            path,
        };
        match self.io.submit(session, job) {
            Ok(()) => {
                self.documents.save = Some(Save {
                    request: self.io.request().unwrap(),
                    session,
                    revision,
                });
                self.after_io = after;
                self.documents.prompt = None;
                self.message(false, "Document operation in progress…");
            }
            Err(e) => {
                self.documents.save_after = after;
                self.message(true, e);
            }
        }
    }
    pub(super) fn confirmed_save_as(&mut self, path: String) {
        if matches!(
            self.documents.prompt,
            Some(Prompt::Replace { .. } | Prompt::Choose(..))
        ) {
            self.message(
                true,
                "Finish or cancel the current document operation first",
            );
            return;
        }
        if path.trim().is_empty() {
            self.message(true, "Enter a native save destination");
            return;
        }
        self.native_save_draft = path.clone();
        let after = self.documents.save_after.take();
        self.save_to(PathBuf::from(path), after);
    }
    pub(super) fn saved_document(
        &mut self,
        request: u64,
        path: PathBuf,
        receipt: skylive_host::storage::SaveReceipt,
        after: Option<AfterIo>,
        context: &egui::Context,
    ) {
        if self
            .documents
            .save
            .as_ref()
            .is_none_or(|pending| pending.request != request)
        {
            self.message(
                true,
                "Save result is obsolete; native binding was not changed",
            );
            return;
        }
        let Some(pending) = self.documents.save.take() else {
            self.message(
                true,
                "Save result is obsolete; native binding was not changed",
            );
            return;
        };
        let native_stamp=receipt.file_stamp();
        if pending.session != self.session.identity()
            || pending.revision != self.session.snapshot().revision()
            || !receipt.accept(&mut self.session)
        {
            self.document_save_failed(after);
            self.message(false, "Older revision saved; current edits remain unsaved");
            return;
        }
        self.active_document_path = Some(path.clone());
        self.remember_native_document(path.clone());
        self.library_saved(path.clone(),native_stamp);
        self.native_save_draft = path.to_string_lossy().into_owned();
        self.message(
            false,
            if self.obs_identities.is_some() {
                "Native document saved; OBS identity sidecar remains session-only"
            } else {
                "Document saved"
            },
        );
        match after {
            Some(AfterIo::SaveAndClose) => self.shutdown(context),
            Some(AfterIo::SaveAndReplace) => self.publish_document(context),
            _ => {}
        }
    }
    pub(super) fn document_save_failed(&mut self, after: Option<AfterIo>) {
        self.documents.save = None;
        if matches!(after, Some(AfterIo::SaveAndReplace)) {
            if let Some(candidate) = &self.documents.candidate {
                if candidate.session == self.session.identity()
                    && candidate.revision == self.session.snapshot().revision()
                {
                    self.documents.prompt = Some(Prompt::Replace {
                        target: candidate.label.clone(),
                    });
                } else {
                    self.documents.cancel();
                }
            }
        }
    }
    fn publish_document(&mut self, context: &egui::Context) {
        if self.recording.active()||self.streaming.active()||self.replay.active(){self.message(true,"Stop active outputs before replacing the collection.");return;}
        if self.ndi_output.owner.any_active(){self.message(true,"Stop NDI outputs before replacing the scene collection");return;}
        if self.virtual_camera.owned() {
            self.message(true,"Stop the virtual camera before replacing the scene collection");return;
        }

        let Some(candidate) = self.documents.candidate.take() else {
            return;
        };
        let _creation=self.documents.creation.take();
        self.documents.prompt = None;
        self.documents.save_after = None;
        if candidate.session != self.session.identity()
            || candidate.revision != self.session.snapshot().revision()
            || self.session.is_closed()
        {
            self.message(
                true,
                "Replacement result is obsolete; the current document was retained",
            );
            return;
        }
        let mut remove_after=None;
        let value=match candidate.value{
            Prepared::RepairReady(ready)=>{let saved=self.documents.repair_saved.take();self.start_repair_accept(ready,saved,context);return;},
            Prepared::ManagedCreate(plan)=>{let label=plan.name().to_owned();self.library_document_io(Replacement::LibraryCreate{label},Job::LibraryCreate(plan),context);return;},
            Prepared::ManagedRemove{loaded,path,removal}=>{remove_after=Some(removal);Prepared::Loaded{loaded,path}},
            value=>value,
        };
        let remember_open=self.documents.remember_open.take();
        let is_new = matches!(&value, Prepared::New(_));
        let (session, binding, identities, migration, imported) = match value {
            Prepared::New(session) => (session, None, None, None, false),
            Prepared::RepairReady(_)|Prepared::ManagedCreate(_)|Prepared::ManagedRemove{..}=>{self.message(true,"Managed candidate has an unexpected publication phase.");return;}
            Prepared::Loaded { loaded, path } => {
                let skylive_host::storage::LoadedDocument { session, migration } = *loaded;
                let session = match self.session.adopt_document_preferences(session) {
                    Ok(s) => s,
                    Err(e) => {
                        self.message(true, e.to_string());
                        return;
                    }
                };
                (
                    session,
                    Some(path),
                    None,
                    migration
                        .has_semantic_changes()
                        .then(|| Dialog::NativeMigration(Arc::new(migration))),
                    false,
                )
            }
            Prepared::Imported(value) => {
                let value = match value.adopt_into(&self.session) {
                    Ok(v) => v,
                    Err(e) => {
                        self.message(true, e.to_string());
                        return;
                    }
                };
                (
                    value.session,
                    None,
                    Some(Arc::new(value.identities)),
                    None,
                    true,
                )
            }
        };
        // All fallible candidate validation/adoption is complete before closing
        // the current Session. No fake saved receipt or history mutation.
        if self.replay.active()||self.ndi_output.owner.any_active(){self.message(true,"Stop active outputs before replacing the collection.");return;}
        self.process_priority.invalidate();
        if let Err(error)=self.camera.before_session_change() {self.message(true,error.to_string());return;}
        self.commit_document(Publication{session,binding,identities,migration,imported,is_new,remember_open,remove_after},context);
    }
    pub(super) fn show_document_prompt(&mut self, context: &egui::Context) {
        if let Some(prompt) = &mut self.documents.prompt {
            if let Some(intent) = views::document::show(context, prompt, self.io.busy()) {
                self.intent(intent, context);
            }
        }
        if let Some(probe) = &self.documents.obs_camera {
            let total = probe.prepared.targets().len();
            if views::document::show_camera_import_progress(context, probe.next, total) {
                self.documents.cancel();
                self.message(false, "OBS camera import cancelled; the current document was retained.");
            }
        }
    }
}
#[cfg(test)]
#[path = "app_document_tests.rs"]
mod tests;

struct Publication{
 session:Session,binding:Option<PathBuf>,identities:Option<Arc<skylive_host::obs::ObsIdentities>>,migration:Option<Dialog>,imported:bool,is_new:bool,remember_open:Option<PathBuf>,remove_after:Option<skylive_host::collection_library::RemovePlan>,
}
impl StudioApp{
 fn commit_document(&mut self,publication:Publication,context:&egui::Context){
  let Publication{session,binding,identities,migration,imported,is_new,remember_open,remove_after}=publication;
        self.projectors.close(context);
        self.session.close();
        self.invalidate_shortcut_callbacks();
        self.session = session;
        self.active_document_path = binding.clone();
        if let Some(path) = binding {
            self.native_save_draft = path.to_string_lossy().into_owned();
        } else {
            self.native_save_draft = "untitled.skylive.json".into();
        }
        if is_new {
            self.document_path = "untitled.skylive.json".into();
        }
        self.discard_stale_program_audio();
        self.selected = None;
        let semantic_migration = migration.is_some();
        self.dialog = migration;
        self.obs_identities = identities;
        self.transition_preset_focus = None;
        if self.session.snapshot().preferences().language() != self.localization.language() {
            self.request_localization(
                context,
                self.session.snapshot().preferences().language().to_owned(),
                false,
            );
        }
        if !is_new&&!imported&&self.active_document_path.is_some(){
            if let Some(path)=remember_open.filter(|path|self.active_document_path.as_ref()==Some(path)){self.remember_native_document(path);}
            self.schedule_projector_restoration();
        }
        if let Some(removal)=remove_after{self.library_remove_after_publication(removal);}
        self.refresh_library_after_publication(context);
        if semantic_migration {
            self.message(true, "Scene filter compatibility changed. Review the affected scenes and save this document explicitly.");
        } else {
            self.message(false,if imported{"OBS subset imported as an unsaved collection. Original OBS identities are retained only for this session"}else{"Document replaced; the previous document was retained until validation completed"});
        }
        context.request_repaint();
 }
 pub(super) fn native_replacement_pending(&self)->bool{self.documents.request.as_ref().is_some_and(|p|matches!(p.kind,Replacement::Load{..}|Replacement::LibraryOpen{..}|Replacement::LibraryRemove{..}))}
 pub(super) fn cancel_native_replacement(&mut self){if let Some(p)=&mut self.documents.request{p.cancelled=true;}}
 pub(super) fn has_repair_document_candidate(&self)->bool{self.documents.candidate.as_ref().is_some_and(|c|matches!(c.value,Prepared::RepairReady(_)))}
 pub(super) fn stage_repaired_document(&mut self,ready:super::document_repair_flow::Ready,authorized:bool,context:&egui::Context){
  self.documents.remember_open=None;let candidate=Candidate{session:self.session.identity(),revision:self.session.snapshot().revision(),label:ready.path.display().to_string(),value:Prepared::RepairReady(ready)};
  if authorized{self.documents.candidate=Some(candidate);self.documents.prompt=None;}else{self.document_candidate(candidate,context);}
 }
 pub(super) fn publish_repaired_document(&mut self,receipt:skylive_host::document_repair::Receipt,startup:bool,context:&egui::Context){
  // The worker prepared exactly one incoming-document/current-preferences
  // projection. Recheck the real receiver immediately before any external close.
  if !self.repair_scope_fresh()||self.session.is_closed()||self.close_dialog||self.allow_close||receipt.preferences!=*self.session.snapshot().preferences(){self.reject_repaired_publication(receipt,"The receiver changed before pending collection publication.",context);return;}
  if self.recording.active()||self.streaming.active()||self.replay.active()||self.ndi_output.owner.any_active()||self.virtual_camera.owned(){self.reject_repaired_publication(receipt,"Stop active outputs before opening the pending collection.",context);return;}
  self.process_priority.invalidate();if let Err(error)=self.camera.before_session_change(){self.reject_repaired_publication(receipt,error.to_string(),context);return;}
  if !self.repair_scope_fresh()||self.session.is_closed()||self.close_dialog||self.allow_close||receipt.preferences!=*self.session.snapshot().preferences(){self.reject_repaired_publication(receipt,"The receiver changed before pending collection publication.",context);return;}
  let skylive_host::document_repair::Receipt{key,value,lease,..}=receipt;
  let(loaded,path,removal,remember)=match value{
   skylive_host::document_repair::Accepted::Native{loaded,path,recover}=>{let remember=!recover&&!startup;(loaded,path,None,remember)},
   skylive_host::document_repair::Accepted::Managed{opened,removal}=>{self.library_view(opened.view);(opened.loaded,opened.path,removal,true)},
  };
  let skylive_host::storage::LoadedDocument{session,migration}=*loaded;
  let migration=migration.has_semantic_changes().then(||Dialog::NativeMigration(Arc::new(migration)));
  let remember_open=remember.then(||path.clone());
  self.commit_document(Publication{session,binding:Some(path),identities:None,migration,imported:false,is_new:false,remember_open,remove_after:removal},context);
  self.commit_repaired_publication(key,lease,context);
 }
 pub(super) fn retry_pending_document(&mut self,context:&egui::Context){if self.has_repair_document_candidate(){self.publish_document(context);}}
}
