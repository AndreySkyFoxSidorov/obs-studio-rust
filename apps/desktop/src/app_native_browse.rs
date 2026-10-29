//! Selection only: the existing Apply/Save/Open paths retain publication authority.
use super::*;
use crate::native_browse::{Request,Scope,Target};
use skylive_host::native_path_dialog::{self,Kind};
use views::document::Prompt;

#[derive(Clone,PartialEq,Eq)]
enum Draft {
    PendingFiles{key:skylive_host::document_repair::Key,token:u64,generation:u64,slot:skylive_core::ResourceSlot,path:String,directory:bool},
    MissingFiles {token:u64,generation:u64,slot:skylive_core::ResourceSlot,path:String,directory:bool},
    Remux {generation:u64,rows:Vec<(u64,String,String)>},
    Font(views::text_source::FontKey),
    TextFile { token:u64, path:String, edit:views::text_source::TextEdit, chatlog:bool, lines:u16 },
    Lut(views::video_filters::LutKey),
    Mask(views::video_filters::MaskKey),
    AddImage {scene:Id,name:String,path:String,settings:skylive_core::ImageSourceSettings},
    Image {source:Id,path:String,settings:skylive_core::ImageSourceSettings},
    Media {token:u64,edit:views::media_source::MediaEdit,path:String},
    Slides {token:u64,paths:Vec<String>,kinds:Vec<skylive_core::SlideShowEntryKind>},
    Open(views::document_picker::PathDraft),
    Save(String),
    Profile { import: bool, path: String },
    RecordingFolder {profile:skylive_core::ProfileId,path:String},
}
struct Pending {request:u64,scope:Scope,target:Target,draft:Draft}
#[derive(Default)]
pub(super) struct Flow {
    pub owner:Option<isize>,
    epoch:u64,
    pending:Option<Pending>,
}
impl Flow {pub(super) fn epoch(&self)->u64{self.epoch}}
impl StudioApp {
    pub(super) fn next_browse_epoch(&mut self)->bool {
        let Some(epoch)=self.native_browse.epoch.checked_add(1) else {
            self.message(true,"Native file chooser identity exhausted");return false;
        };
        self.native_browse.epoch=epoch;true
    }
    pub(super) fn install_browse_scope(&self,ctx:&egui::Context) {
        crate::native_browse::install(ctx,self.browse_scope());
    }
    fn browse_scope(&self)->Scope {
        Scope {session:self.session.identity(),revision:self.session.snapshot().revision(),
            epoch:self.native_browse.epoch,available:self.native_browse.owner.is_some()}
    }
    fn browse_draft(&self,target:Target)->Option<Draft> {
        match (target,self.dialog.as_ref()) {
            (Target::PendingFiles,_)=>{if !self.repair_scope_fresh(){return None;}let key=self.document_repair.key?;let p=self.document_repair.panel.as_ref()?;let slot=p.selected?;let row=p.page.as_ref()?.rows.iter().find(|r|r.slot==slot)?;Some(Draft::PendingFiles{key,token:p.token,generation:p.generation,slot,path:p.path.clone(),directory:row.directory})},
            (Target::MissingFiles,_)=>{let p=self.missing_files.panel.as_ref()?;if p.session!=self.session.identity()||p.revision!=self.session.snapshot().revision()||self.session.is_closed(){return None;}let slot=p.selected?;let row=p.page.as_ref()?.rows.iter().find(|r|r.slot==slot)?;Some(Draft::MissingFiles {token:p.token,generation:p.generation,slot,path:p.path.clone(),directory:row.directory})},
            (Target::RemuxFiles,_) if self.remux.view.open&&!self.remux.view.active()=>Some(Draft::Remux {generation:self.remux.view.generation,rows:self.remux.view.rows.iter().map(|r|(r.id,r.source.clone(),r.destination.clone())).collect()}),
            (Target::TextFont,Some(Dialog::Text(d))) if !d.font_loading&&d.session==self.session.identity()&&d.revision==self.session.snapshot().revision()=>d.font_key().ok().map(Draft::Font),
            (Target::TextFile,Some(Dialog::Text(d))) if d.read_file&&d.session==self.session.identity()&&d.revision==self.session.snapshot().revision()=>Some(Draft::TextFile{token:d.token,path:d.file_path.clone(),edit:d.edit,chatlog:d.chatlog,lines:d.chatlog_lines}),
            (Target::Lut,Some(Dialog::VideoFilters(d))) if !d.lut_busy()&&d.session==self.session.identity()&&d.revision==self.session.snapshot().revision()=>d.lut_key().map(Draft::Lut),
            (Target::Mask,Some(Dialog::VideoFilters(d))) if !d.mask_busy()&&d.session==self.session.identity()&&d.revision==self.session.snapshot().revision()=>d.mask_key().map(Draft::Mask),
            (Target::Profile,Some(Dialog::ProfilePath {import,path}))=>Some(Draft::Profile {import:*import,path:path.clone()}),
            (Target::RecordingFolder,Some(Dialog::Recording(d)))=>Some(Draft::RecordingFolder {profile:d.profile,path:d.directory.clone()}),
            (Target::Image,Some(Dialog::AddImage {scene,name,path,settings}))=>Some(Draft::AddImage {scene:*scene,name:name.clone(),path:path.clone(),settings:*settings}),
            (Target::Image,Some(Dialog::ImageFile {source,path,settings}))=>Some(Draft::Image {source:*source,path:path.clone(),settings:*settings}),
            (Target::Media,Some(Dialog::MediaSource(d))) if !d.loading&&!d.worker_busy
                &&d.session==self.session.identity()&&d.revision==self.session.snapshot().revision()=>
                Some(Draft::Media {token:d.token,edit:d.edit,path:d.path.clone()}),
            (Target::SlideFiles|Target::SlideFolder,Some(Dialog::ImageSlideShow(d))) if !d.loading&&d.current(&self.session)=>
                Some(Draft::Slides {token:d.token,paths:d.paths.clone(),kinds:d.kinds.clone()}),
            (Target::DocumentOpen,_)=>match self.documents.prompt.as_ref() {Some(Prompt::Choose(d))=>Some(Draft::Open(d.clone())),_=>None},
            (Target::DocumentSave,_)=>match self.documents.prompt.as_ref() {Some(Prompt::SaveAs {path})=>Some(Draft::Save(path.clone())),_=>None},
            _=>None,
        }
    }
    pub(super) fn begin_native_browse(&mut self,request:Request,ctx:&egui::Context) {
        if self.io.busy()||self.session.is_closed()||request.scope!=self.browse_scope() {
            self.message(true,"The file chooser target changed. Click Browse again.");return;
        }
        let Some(owner)=self.native_browse.owner else {self.message(true,"The native file chooser has no application window owner.");return;};
        let Some(draft)=self.browse_draft(request.target) else {self.message(true,"The file chooser target changed. Click Browse again.");return;};
        let (kind,initial,title,filter)=match &draft {
            Draft::PendingFiles{slot,path,directory,..}|Draft::MissingFiles {slot,path,directory,..}=>(if *directory {Kind::ImageFolder}else{match slot {skylive_core::ResourceSlot::TextFile{..}=>Kind::TextFile,skylive_core::ResourceSlot::BrowserHtml{..}=>Kind::BrowserHtml,skylive_core::ResourceSlot::Media{..}=>Kind::Media,skylive_core::ResourceSlot::Mask{..}=>Kind::ImageMask,skylive_core::ResourceSlot::Lut{..}=>Kind::Lut,_=>Kind::Image}},path.clone(),"Replacement path",if matches!(slot,skylive_core::ResourceSlot::BrowserHtml{..}){"HTML files"}else if matches!(slot,skylive_core::ResourceSlot::TextFile{..}){"Text files"}else{"All files"}),
            Draft::Remux {rows,..}=>(Kind::RemuxRecordings,rows.last().map(|r|r.1.clone()).unwrap_or_default(),"Add recordings…","Matroska recordings"),
            Draft::Font(key)=>(Kind::Font,key.path.clone(),"Font File","Static TrueType outlines (TTF / TTC)"),
            Draft::TextFile{path,..}=>(Kind::TextFile,path.clone(),"Text file","Text files"),
            Draft::Lut(key)=>(Kind::Lut,key.path.clone(),"LUT file","PNG / CUBE LUT files"),
            Draft::Mask(key)=>(Kind::ImageMask,key.path.clone(),"Mask image","Image Files"),
            Draft::Profile {import,path}=>(if *import {Kind::ProfileOpen}else{Kind::ProfileSave},path.clone(),if *import {"Import Profile"}else{"Export Profile"},"SkyLive profiles"),
            Draft::RecordingFolder {path,..}=>(Kind::RecordingFolder,path.clone(),"Recording Path","All files"),
            Draft::AddImage {path,..}|Draft::Image {path,..}=>(Kind::Image,path.clone(),"Image file","Image Files"),
            Draft::Media {path,..}=>(Kind::Media,path.clone(),"Local File","Media Source"),
            Draft::Slides {paths,..}=>(if request.target==Target::SlideFolder {Kind::ImageFolder}else{Kind::Images},paths.last().cloned().unwrap_or_default(),if request.target==Target::SlideFolder {"Add folder…"}else{"Image Files"},"Image Files"),
            Draft::Open(d)=>(if d.action==views::document_picker::DocumentAction::ImportObs {Kind::ObsOpen}else{Kind::DocumentOpen},d.path.clone(),d.action.title(),"JSON files"),
            Draft::Save(path)=>(Kind::DocumentSave,path.clone(),"Save native document as","JSON files"),
        };
        let initial=if initial.len()<=native_path_dialog::MAX_PATH_BYTES && !initial.chars().any(char::is_control) {initial}else{String::new()};
        let job=Job::BrowsePaths(native_path_dialog::Request {owner_hwnd:owner,kind,
            title:crate::i18n::tr_context(ctx,title),initial,
            filter_label:crate::i18n::tr_context(ctx,filter),all_files_label:crate::i18n::tr_context(ctx,"All files")});
        match self.io.submit(self.session.identity(),job) {
            Ok(())=>{
                if let Some(id)=self.io.request() {self.native_browse.pending=Some(Pending {request:id,scope:request.scope,target:request.target,draft});}
            },
            Err(error)=>self.message(true,error),
        }
    }
    pub(super) fn native_browse_completion(&mut self,completion:crate::io::Completion,context:&egui::Context)->Option<crate::io::Completion> {
        if self.native_browse.pending.as_ref().is_none_or(|p|p.request!=completion.request) {return Some(completion);}
        let Some(pending)=self.native_browse.pending.take() else {return Some(completion);};
        // Cancel does not modify even a stale or subsequently replaced draft.
        if matches!(&completion.result,Ok(Reply::PathsChosen(None))) {return None;}
        if completion.session!=pending.scope.session || pending.scope!=self.browse_scope()
            || self.session.is_closed() || self.browse_draft(pending.target).as_ref()!=Some(&pending.draft) {
            self.message(true,"The file chooser target changed; selected paths were discarded.");return None;
        }
        let paths=match completion.result {
            Ok(Reply::PathsChosen(Some(paths)))=>paths,
            Err(error)=>{self.message(true,crate::i18n::tr_context(context,&error));return None;},
            Ok(_)=>{self.message(true,"Native file selection returned an unexpected receipt.");return None;},
        };
        if paths.is_empty()||paths.len()>native_path_dialog::MAX_PATHS
            || paths.iter().any(|p|p.is_empty()||p.len()>native_path_dialog::MAX_PATH_BYTES||p.chars().any(char::is_control)) {
            self.message(true,"Native file selection exceeds the bounded path contract.");return None;
        }
        if pending.target==Target::RemuxFiles{if let Err(error)=self.remux.view.add_paths(paths){self.message(true,error);}return None;}
        if matches!(pending.target,Target::SlideFiles|Target::SlideFolder) {
            if let Some(Dialog::ImageSlideShow(d))=&mut self.dialog {
                if d.paths.len().saturating_add(paths.len())>skylive_core::MAX_SLIDES {
                    self.message(true,"The slideshow can contain at most 256 file/folder entries and 256 resolved images; the draft was retained.");return None;
                }
                d.kinds.resize(d.paths.len(),skylive_core::SlideShowEntryKind::File);let kind=if pending.target==Target::SlideFolder{skylive_core::SlideShowEntryKind::Directory}else{skylive_core::SlideShowEntryKind::File};d.kinds.extend(std::iter::repeat(kind).take(paths.len()));d.paths.extend(paths);
            }
            return None;
        }
        if paths.len()!=1 {self.message(true,"Choose exactly one file for this source or document.");return None;}
        let Some(path)=paths.into_iter().next() else {return None;};
        match pending.target {
            Target::PendingFiles=>if let Some(p)=&mut self.document_repair.panel{p.path=path;if let Err(error)=p.changed(){p.error=Some(error);}},
            Target::MissingFiles=>if let Some(p)=&mut self.missing_files.panel{p.path=path;if let Err(e)=p.changed(){p.error=Some(e);}},
            Target::TextFont=>if let Some(Dialog::Text(d))=&mut self.dialog{d.font_path=path;d.font_error=None;},
            Target::TextFile=>if let Some(Dialog::Text(d))=&mut self.dialog{d.file_path=path;},
            Target::Lut=>if let (Some(Dialog::VideoFilters(d)),Draft::Lut(key))=(&mut self.dialog,&pending.draft){d.lut_path(key,path);},
            Target::Mask=>if let (Some(Dialog::VideoFilters(d)),Draft::Mask(key))=(&mut self.dialog,&pending.draft){d.mask_path(key,path);},
            Target::Profile=>if let Some(Dialog::ProfilePath {path:field,..})=&mut self.dialog {*field=path;},
            Target::RecordingFolder=>if let Some(Dialog::Recording(d))=&mut self.dialog {d.directory=path;},
            Target::Image=>match &mut self.dialog {Some(Dialog::AddImage {path:field,..})|Some(Dialog::ImageFile {path:field,..})=>*field=path,_=>{}},
            Target::Media=>if let Some(Dialog::MediaSource(d))=&mut self.dialog {d.path=path;d.error=None;},
            Target::DocumentOpen=>if let Some(Prompt::Choose(d))=&mut self.documents.prompt {d.path=path;},
            Target::DocumentSave=>if let Some(Prompt::SaveAs {path:field})=&mut self.documents.prompt {*field=path;},
            Target::RemuxFiles|Target::SlideFiles|Target::SlideFolder=>{},
        }
        None
    }
}
