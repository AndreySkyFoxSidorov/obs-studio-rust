//! One admitted filesystem operation, including a not-yet-consumed completion.
use skylive_core::ImageAsset;
use skylive_host::assets;
use skylive_host::obs::{self, ExportJob, ImportVideo, ImportedCollection};
use skylive_host::storage::{self, SaveJob, SaveReceipt};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
};

pub enum Job {
    RepairAdvance{key:skylive_host::document_repair::Key,start:usize},
    RepairPage{key:skylive_host::document_repair::Key,start:usize},
    PrepareCandidateRelink{key:skylive_host::document_repair::Key,choice:skylive_host::missing_files::Choice},
    ApplyCandidateRelinks{key:skylive_host::document_repair::Key,batch:Option<skylive_core::ResourceRelinks>},
    AcceptCandidate{key:skylive_host::document_repair::Key,preferences:skylive_core::UserPreferences,saved:Option<(PathBuf,skylive_host::storage::SavedFileStamp)>},
    DiscardCandidate(skylive_host::document_repair::Key),
    ReturnRepairProjection{receipt:skylive_host::document_repair::Receipt,discard:bool},
    CommitRepairCandidate{key:skylive_host::document_repair::Key,lease:skylive_host::document_repair::Lease},
    ScanMissingFiles {model:Arc<skylive_core::StudioSnapshot>,start:usize},
    PrepareRelink(skylive_host::missing_files::Request),
    LibraryList(skylive_host::collection_library::Root),
    LibraryCreate(skylive_host::collection_library::CreatePlan),
    LibraryOpen {view:skylive_host::collection_library::View,id:skylive_host::collection_library::EntryId},
    LibraryPrepareRemove {view:skylive_host::collection_library::View,path:PathBuf},
    LibraryRemove {plan:skylive_host::collection_library::RemovePlan,active:PathBuf},
    LibrarySyncName {root:skylive_host::collection_library::Root,path:PathBuf,name:String,stamp:skylive_host::storage::SavedFileStamp},
    Startup,
    RememberNative {path:PathBuf,last:skylive_host::last_native_document::LastNativeDocument},
    RetireStartupNative(crate::startup::Native),
    RestoreProjectors(skylive_host::projector::restoration::Prepared),
    ProjectorMonitors,
    Screenshot(Box<skylive_host::screenshot::Request>),
    PrepareFont {path:PathBuf},
    DiscoverSystemFonts,
    PrepareLut {path:PathBuf},
    PrepareMask {path:PathBuf},
    ProbeNvenc {token:crate::views::settings::SettingsToken},
    InstallVirtualCamera { owner: isize },
    LoadProfile { path: PathBuf, revision: u64 },
    ExportProfile { path: PathBuf, profile: skylive_core::Profile },
    BrowsePaths(skylive_host::native_path_dialog::Request),
    OpenDirectory(PathBuf),
    SavePreferences {
        path: PathBuf,
        preferences: skylive_core::UserPreferences,
    },
    LoadLocalization {
        path: PathBuf,
        language: String,
        revision: u64,
        commit: bool,
    },
    Save {
        job: SaveJob,
        path: PathBuf,
    },
    Checkpoint(SaveJob),
    Load(PathBuf),
    Recover(PathBuf),
    ImportObs {
        path: PathBuf,
        video: ImportVideo,
        preferences:skylive_core::UserPreferences,
    },
    PrepareObsCamera {path:PathBuf},
    ImportObsPrepared {prepared:skylive_host::obs::PreparedCameraImport,video:ImportVideo,preferences:skylive_core::UserPreferences,catalog:Vec<skylive_host::obs::CameraImportDevice>},
    ExportObs {
        path: PathBuf,
        job: ExportJob,
    },
    PrepareSlideShow {entries:Vec<skylive_core::SlideShowEntry>,token:u64},
    PrepareImage {
        path: PathBuf,
    },
}
pub enum Reply {
    DocumentRepair(skylive_host::document_repair::Event),
    RepairPage(skylive_host::missing_files::Page),
    CandidateRelink(skylive_core::ResourceRelink),
    CandidateDiscarded,
    RepairProjectionReturned(Option<skylive_host::document_repair::Stage>),
    RepairCommitted,
    StartupRepairLoaded{settings:Box<crate::startup::Loaded>,native:Option<Result<skylive_host::document_repair::Event,String>>},
    MissingFilesScanned(skylive_host::missing_files::Page),
    RelinkPrepared(skylive_core::ResourceRelink),
    LibraryListed(skylive_host::collection_library::View),
    LibraryCreated(skylive_host::collection_library::Opened),
    LibraryOpened(skylive_host::collection_library::Opened),
    LibraryRemovalPrepared(skylive_host::collection_library::RemovalPrepared),
    LibraryRemoved(skylive_host::collection_library::Removed),
    LibraryNameSynced(Option<skylive_host::collection_library::View>),
    StartupLoaded(Box<crate::startup::Loaded>),
    NativeRemembered(skylive_host::last_native_document::LastNativeDocument),
    StartupRetired,
    ProjectorsResolved(skylive_host::projector::restoration::Resolved),
    ProjectorMonitors(Vec<skylive_host::projector::Monitor>),
    ScreenshotSaved(skylive_host::screenshot::Receipt),
    FontPrepared(skylive_host::font_assets::FontCatalog),
    SystemFontsDiscovered(skylive_host::system_fonts::SystemFontCatalog),
    LutPrepared(skylive_core::LutAsset),
    MaskPrepared(ImageAsset),
    NvencProbed {token:crate::views::settings::SettingsToken,catalog:Result<skylive_host::native_h264::NvencCatalog,String>},
    VirtualCameraInstalled(bool),
    ProfileLoaded { profile: skylive_core::Profile, revision: u64 },
    ProfileExported(PathBuf),
    PathsChosen(Option<Vec<String>>),
    DirectoryRequested(PathBuf),
    PreferencesSaved(skylive_core::UserPreferences),
    Localized {
        localization: Arc<skylive_host::localization::Localization>,
        revision: u64,
        commit: bool,
    },
    Saved {
        receipt: SaveReceipt,
        path: PathBuf,
    },
    Checkpointed,
    Loaded {
        loaded: Box<storage::LoadedDocument>,
        path: PathBuf,
    },
    Imported(Box<ImportedCollection>),
    ObsCameraPrepared(skylive_host::obs::PreparedCameraImport),
    ObsExported,
    ImagePrepared(ImageAsset),
    SlideShowPrepared {prepared:skylive_host::image_slideshow::directories::Prepared,token:u64},
}
pub struct Completion {
    pub request: u64,
    pub session: u64,
    pub result: Result<Reply, String>,
}
pub struct IoWorker {
    jobs: Option<mpsc::SyncSender<(u64, u64, Job)>>,
    results: mpsc::Receiver<Completion>,
    busy: bool,
    sequence: u64,
    active_request: Option<u64>,
}
impl IoWorker {
    pub fn new(wake: eframe::egui::Context) -> std::io::Result<Self> {
        let (jobs, receiver) = mpsc::sync_channel::<(u64, u64, Job)>(1);
        let (done, results) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("skylive-document-io".into())
            .spawn(move || {
                let mut repair_owner=skylive_host::document_repair::Owner::default();
                let mut collection_root:Option<skylive_host::collection_library::Root>=None;
                while let Ok((request, session, job)) = receiver.recv() {
                    let result = match job {
                        Job::RepairAdvance{key,start}=>repair_owner.advance(key,start).map(Reply::DocumentRepair).map_err(|e|e.to_string()),
                        Job::RepairPage{key,start}=>repair_owner.page(key,start).map(Reply::RepairPage).map_err(|e|e.to_string()),
                        Job::PrepareCandidateRelink{key,choice}=>repair_owner.prepare(key,choice).map(Reply::CandidateRelink).map_err(|e|e.to_string()),
                        Job::ApplyCandidateRelinks{key,batch}=>repair_owner.apply(key,batch).map(Reply::DocumentRepair).map_err(|e|e.to_string()),
                        Job::AcceptCandidate{key,preferences,saved}=>repair_owner.accept(key,preferences,saved).map(Reply::DocumentRepair).map_err(|e|e.to_string()),
                        Job::DiscardCandidate(key)=>repair_owner.discard(key).map(|()|Reply::CandidateDiscarded).map_err(|e|e.to_string()),
                        Job::ReturnRepairProjection{receipt,discard}=>repair_owner.reject_projection(receipt,discard).map(Reply::RepairProjectionReturned).map_err(|error|error.to_string()),
                        Job::CommitRepairCandidate{key,lease}=>{let result=repair_owner.commit(key).map(|()|Reply::RepairCommitted).map_err(|error|error.to_string());drop(lease);result},
                        Job::ScanMissingFiles {model,start}=>skylive_host::missing_files::scan(&model,start).map(Reply::MissingFilesScanned),
                        Job::PrepareRelink(request)=>skylive_host::missing_files::prepare(request).map(Reply::RelinkPrepared),
                        Job::LibraryList(root)=>{collection_root=Some(root.clone());skylive_host::collection_library::list(&root).map(Reply::LibraryListed).map_err(|error|error.to_string())},
                        Job::LibraryCreate(plan)=>skylive_host::collection_library::create(plan).map(Reply::LibraryCreated).map_err(|error|error.to_string()),
                        Job::LibraryOpen {view,id}=>repair_owner.begin(skylive_host::document_repair::Begin::Managed{view,id}).map(Reply::DocumentRepair).map_err(|error|error.to_string()),
                        Job::LibraryPrepareRemove {view,path}=>repair_owner.begin(skylive_host::document_repair::Begin::Remove{view,current:path}).map(Reply::DocumentRepair).map_err(|error|error.to_string()),
                        Job::LibraryRemove {plan,active}=>skylive_host::collection_library::remove(plan,&active).map(Reply::LibraryRemoved).map_err(|error|error.to_string()),
                        Job::LibrarySyncName {root,path,name,stamp}=>skylive_host::collection_library::sync_saved_name(&root,&path,&name,stamp).map(Reply::LibraryNameSynced).map_err(|error|error.to_string()),
                        Job::Startup=>crate::startup::load_with_repair(&mut repair_owner).map(|(loaded,native)|{collection_root=loaded.preferences_path.as_ref().and_then(|path|skylive_host::collection_library::Root::for_preferences(path).ok());Reply::StartupRepairLoaded{settings:Box::new(loaded),native}}),
                        Job::RememberNative {path,last}=>skylive_host::last_native_document::save(&path,&last).map(|()|Reply::NativeRemembered(last)).map_err(|error|error.to_string()),
                        Job::RetireStartupNative(value)=>{drop(value);Ok(Reply::StartupRetired)},
                        Job::Screenshot(request)=>skylive_host::screenshot::save(*request).map(Reply::ScreenshotSaved),
                        Job::ProbeNvenc {token}=>Ok(Reply::NvencProbed {token,catalog:skylive_host::native_h264::probe_nvenc().map_err(|e|e.to_string())}),
                        Job::InstallVirtualCamera {owner} => skylive_host::virtual_camera::request_install(owner).map(Reply::VirtualCameraInstalled),
                        Job::LoadProfile { path, revision } => skylive_host::profile_io::load(&path).map(|profile| Reply::ProfileLoaded { profile, revision }),
                        Job::ExportProfile { path, profile } => skylive_host::profile_io::save(&path, profile).map(|()| Reply::ProfileExported(path)),
                        Job::BrowsePaths(request) => skylive_host::native_path_dialog::choose(request).map(Reply::PathsChosen),
                        Job::OpenDirectory(path) => open_directory(path),
                        Job::SavePreferences { path, preferences } => {
                            skylive_host::user_preferences::save(&path, &preferences)
                                .map(|()| Reply::PreferencesSaved(preferences))
                                .map_err(|e| e.to_string())
                        }
                        Job::LoadLocalization {
                            path,
                            language,
                            revision,
                            commit,
                        } => skylive_host::localization::Localization::load(&path, &language)
                            .map(|value| Reply::Localized {
                                localization: Arc::new(value),
                                revision,
                                commit,
                            })
                            .map_err(|error| error.to_string()),
                        Job::Save { job, path } => {
                            native_recovery_binding(&path).and_then(|path| {
                                skylive_host::collection_library::save_native(collection_root.as_ref(),job)
                                    .map(|receipt| Reply::Saved { receipt, path })
                                    .map_err(|e| e.to_string())
                            })
                        }
                        Job::Checkpoint(job) => job
                            .checkpoint()
                            .map(|_| Reply::Checkpointed)
                            .map_err(|error| error.to_string()),
                        Job::Load(path)=>repair_owner.begin(skylive_host::document_repair::Begin::Native{path,recover:false}).map(Reply::DocumentRepair).map_err(|error|error.to_string()),
                        Job::Recover(path)=>native_recovery_binding(&path).and_then(|path|repair_owner.begin(skylive_host::document_repair::Begin::Native{path,recover:true}).map(Reply::DocumentRepair).map_err(|error|error.to_string())),
                        Job::ImportObs { path, video, preferences } => obs::load_collection_with_preferences(&path, video, &preferences)
                            .map(Box::new)
                            .map(Reply::Imported)
                            .map_err(|error| error.to_string()),
                        Job::PrepareObsCamera {path}=>obs::PreparedCameraImport::prepare(&path).map(Reply::ObsCameraPrepared).map_err(|error|error.to_string()),
                        Job::ImportObsPrepared {prepared,video,preferences,catalog}=>prepared.import(video,&preferences,&catalog).map(Box::new).map(Reply::Imported).map_err(|error|error.to_string()),
                        Job::ExportObs { path, job } => job
                            .run()
                            .map_err(|error| error.to_string())
                            .and_then(|bytes| {
                                storage::publish_bytes(&path, &bytes)
                                    .map(|_| Reply::ObsExported)
                                    .map_err(|error| error.to_string())
                            }),
                        Job::RestoreProjectors(prepared)=>skylive_host::projector::restoration::discover(prepared).map(Reply::ProjectorsResolved).map_err(|error|error.to_string()),
                        Job::ProjectorMonitors=>skylive_host::projector::enumerate_monitors().map(Reply::ProjectorMonitors).map_err(|error|error.to_string()),
                        Job::DiscoverSystemFonts=>skylive_host::system_fonts::discover().map(Reply::SystemFontsDiscovered).map_err(|error|error.to_string()),
                        Job::PrepareFont {path}=>skylive_host::font_assets::prepare_font(&path).map(Reply::FontPrepared).map_err(|error|error.to_string()),
                        Job::PrepareLut {path}=>skylive_host::lut_assets::prepare_lut(&path).map(Reply::LutPrepared).map_err(|error|error.to_string()),
                        Job::PrepareMask {path}=>assets::prepare_mask_image(&path).map(|p|{let asset=p.asset;drop(p.frame);Reply::MaskPrepared(asset)}).map_err(|e|e.to_string()),
                        Job::PrepareSlideShow {entries,token}=>skylive_host::image_slideshow::directories::prepare_entries(&entries).map(|prepared|Reply::SlideShowPrepared {prepared,token}),
                        Job::PrepareImage { path } => assets::prepare_image(&path)
                            .map(|prepared| {
                                // Pixels are not a UI result. Their final owner and
                                // potentially expensive destruction stay off-frame.
                                let asset = prepared.asset;
                                drop(prepared.frame);
                                Reply::ImagePrepared(asset)
                            })
                            .map_err(|error| error.to_string()),
                    };
                    if done
                        .send(Completion {
                            request,
                            session,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    wake.request_repaint();
                }
            })?;
        Ok(Self {
            jobs: Some(jobs),
            results,
            busy: false,
            sequence: 0,
            active_request: None,
        })
    }
    pub fn busy(&self) -> bool {
        self.busy
    }
    pub fn submit(&mut self, session: u64, job: Job) -> Result<(), String> {
        self.submit_retained(session,job).map_err(|(error,_job)|error)
    }
    /// Repair publication ownership must not be dropped on the GUI when a
    /// mailbox admission fails. Existing callers retain their original facade.
    pub fn submit_retained(&mut self,session:u64,job:Job)->Result<(),(String,Job)>{
        if self.busy{return Err(("A document operation is already active".into(),job));}
        let Some(request)=self.sequence.checked_add(1)else{return Err(("Document request identity exhausted".into(),job));};
        let Some(sender)=self.jobs.as_ref()else{return Err(("Document worker is closed".into(),job));};
        match sender.try_send((request,session,job)){
            Ok(())=>{self.busy=true;self.sequence=request;self.active_request=Some(request);Ok(())},
            Err(error)=>{let message=error.to_string();let(_,_,job)=match error{mpsc::TrySendError::Full(value)|mpsc::TrySendError::Disconnected(value)=>value};Err((message,job))}
        }
    }
    pub fn request(&self) -> Option<u64> {
        self.active_request
    }
    pub fn poll(&mut self) -> Option<Completion> {
        match self.results.try_recv() {
            Ok(completion) => {
                self.busy = false;
                self.active_request = None;
                Some(completion)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) if self.busy => {
                self.busy = false;
                Some(Completion {
                    request: self.active_request.take().unwrap_or(0),
                    session: 0,
                    result: Err("Document worker disconnected".into()),
                })
            }
            Err(_) => None,
        }
    }
    pub fn close(&mut self) {
        self.jobs.take();
    }
}

fn open_directory(path: PathBuf) -> Result<Reply, String> {
    let path = std::path::absolute(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !path.is_dir() { return Err(format!("Not a folder: {}", path.display())); }
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer.exe");
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("/usr/bin/open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    // The desktop shell owns the resulting folder window. Launch only for an
    // explicit menu request; never interpolate a path through a command shell.
    command.arg(&path).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null()).spawn().map_err(|error| error.to_string())?;
    Ok(Reply::DirectoryRequested(path))
}

fn native_recovery_binding(path: &std::path::Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let name = absolute
        .file_name()
        .ok_or("Recovery needs a native document filename")?;
    let parent = absolute
        .parent()
        .ok_or("Recovery needs a parent directory")?;
    Ok(std::fs::canonicalize(parent)
        .map_err(|e| e.to_string())?
        .join(name))
}
