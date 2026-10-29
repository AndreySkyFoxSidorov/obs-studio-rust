//! Worker-only bounded expansion. No recursion, GUI IO or playback mutation.
use skylive_core::{ImageAsset,ImageSlideShow,SlideShowEntry,SlideShowEntryKind,MAX_SLIDES};
use std::{fs,path::{Path,PathBuf},time::SystemTime};
use serde::Serialize;
pub const MAX_DIRECTORY_VISITS:usize=8192;
pub const MAX_PREPARED_METADATA:usize=4*1024*1024;
pub const MAX_OPERATION_IMAGES:usize=1024;
#[derive(Default)]pub struct Budget{visits:usize,metadata:usize,images:usize}
impl Budget{
    fn text(&mut self,bytes:usize)->Result<(),String>{self.metadata=self.metadata.checked_add(bytes).filter(|n|*n<=MAX_PREPARED_METADATA).ok_or("Slideshow expansion exceeds the 4MiB metadata budget")?;Ok(())}
    fn visit(&mut self)->Result<(),String>{self.visits=self.visits.checked_add(1).filter(|n|*n<=MAX_DIRECTORY_VISITS).ok_or("Slideshow directory scan exceeds 8192 entries including stability scans")?;Ok(())}
    fn images(&mut self,count:usize)->Result<(),String>{self.images=self.images.checked_add(count).filter(|n|*n<=MAX_OPERATION_IMAGES).ok_or("Slideshow operation exceeds 1024 resolved images")?;Ok(())}
}
#[derive(Clone,Debug,PartialEq,Eq)]struct Stamp{len:u64,modified:Option<SystemTime>,created:Option<SystemTime>,directory:bool,file:bool,readonly:bool}
fn stamp(path:&Path)->Result<Stamp,String>{let m=fs::metadata(path).map_err(|error|format!("{}: {error}",path.display()))?;Ok(Stamp{len:m.len(),modified:m.modified().ok(),created:m.created().ok(),directory:m.is_dir(),file:m.is_file(),readonly:m.permissions().readonly()})}
#[derive(Clone,Debug,PartialEq,Eq)]struct FileStamp{path:PathBuf,stamp:Stamp}
#[derive(Clone,Debug,PartialEq,Eq)]struct Folder{path:PathBuf,stamp:Stamp,files:Vec<FileStamp>}
pub struct Expansion{pub inputs:Vec<SlideShowEntry>,pub entries:Vec<SlideShowEntry>,pub counts:Vec<u16>,files:Vec<FileStamp>,folders:Vec<Folder>}
/// Bounded, opaque observation used by the desktop background owner. The
/// contents are compared only within the same authored source and session.
#[derive(Clone,Debug,PartialEq,Eq)]pub struct WatchSignature{entries:Vec<SlideShowEntry>,files:Vec<FileStamp>,folders:Vec<Folder>}
#[derive(Serialize)]pub struct Prepared{pub inputs:Vec<SlideShowEntry>,pub entries:Vec<SlideShowEntry>,pub counts:Vec<u16>,pub images:Vec<ImageAsset>,pub scanned_entries:usize}
impl Prepared{
    pub fn into_settings(self,base:&ImageSlideShow)->Result<ImageSlideShow,String>{ImageSlideShow::new_with_options(self.images,base.duration_ms(),base.transition(),base.transition_ms(),base.looping(),base.shuffle(),base.hide_when_done(),base.canvas(),base.behavior()).and_then(|s|s.with_mode(base.mode())).and_then(|s|s.with_randomization(base.randomize(),base.random_compatibility())).and_then(|s|s.with_entries(self.entries,self.counts)).map_err(|error|error.to_string())}
}
fn absolute_entry(entry:&SlideShowEntry,budget:&mut Budget)->Result<SlideShowEntry,String>{let path=std::path::absolute(Path::new(entry.path())).map_err(|error|format!("{}: {error}",entry.path()))?;let text=path.to_str().ok_or_else(||format!("Slideshow path is not exact UTF-8: {path:?}"))?;budget.text(text.len().saturating_mul(3)+std::mem::size_of::<SlideShowEntry>())?;SlideShowEntry::new(entry.kind(),text.to_owned()).map_err(|error|error.to_string())}
fn extension(path:&Path)->bool{let Some(ext)=path.extension().and_then(|ext|ext.to_str())else{return false;};matches!(ext.to_ascii_lowercase().as_str(),"bmp"|"tga"|"png"|"apng"|"jpeg"|"jpg"|"gif"|"webp")||(cfg!(target_os="windows")&&ext.eq_ignore_ascii_case("jxr"))}
fn scan(path:&Path,budget:&mut Budget)->Result<Vec<FileStamp>,String>{
    let listing=fs::read_dir(path).map_err(|error|format!("{}: {error}",path.display()))?;let mut files=Vec::new();
    // Preserve native enumeration order, exactly as os_readdir; no sort/dedup.
    for entry in listing{budget.visit()?;let entry=entry.map_err(|error|format!("{}: {error}",path.display()))?;let name=entry.file_name();let name_text=name.to_str().ok_or_else(||format!("Slideshow filename is not exact UTF-8: {name:?}"))?;budget.text(name_text.len())?;
        let kind=entry.file_type().map_err(|error|format!("{}: {error}",entry.path().display()))?;if kind.is_dir(){continue;}let file=entry.path();if !extension(&file){continue;}
        let state=stamp(&file)?;if state.directory{continue;}if !state.file{return Err(format!("Slideshow folder entry is not a regular image file: {}",file.display()));}
        if file.extension().and_then(|ext|ext.to_str()).is_some_and(|ext|ext.eq_ignore_ascii_case("jxr")){return Err(format!("JXR slideshow images are not supported by the Rust decoder: {}",file.display()));}
        let text=file.to_str().ok_or_else(||format!("Slideshow path is not exact UTF-8: {file:?}"))?;if text.len()>4096||text.chars().any(char::is_control){return Err(format!("Slideshow expanded image path exceeds the native path contract: {file:?}"));}
        budget.text(text.len().saturating_mul(3)+std::mem::size_of::<FileStamp>())?;if files.len()>=MAX_SLIDES{return Err("Slideshow folder expands beyond 256 images; nothing was truncated".into());}files.push(FileStamp{path:file,stamp:state});
    }Ok(files)
}
/// OBS path values do not encode file/folder kind. Classify only on its IO owner.
pub fn classify(paths:&[PathBuf],budget:&mut Budget)->Result<Vec<SlideShowEntry>,String>{if paths.len()>MAX_SLIDES{return Err("Select at most 256 slideshow file or folder entries".into());}let mut entries=Vec::with_capacity(paths.len());for path in paths{let absolute=std::path::absolute(path).map_err(|error|format!("{}: {error}",path.display()))?;let state=stamp(&absolute)?;let kind=if state.directory{SlideShowEntryKind::Directory}else if state.file{SlideShowEntryKind::File}else{return Err(format!("Slideshow entry is neither file nor folder: {}",path.display()));};let text=absolute.to_str().ok_or_else(||format!("Slideshow path is not exact UTF-8: {absolute:?}"))?;budget.text(text.len()+std::mem::size_of::<SlideShowEntry>())?;entries.push(SlideShowEntry::new(kind,text.to_owned()).map_err(|error|error.to_string())?);}Ok(entries)}
pub fn expand(inputs:&[SlideShowEntry],budget:&mut Budget)->Result<Expansion,String>{
    if inputs.len()>MAX_SLIDES{return Err("Select at most 256 slideshow file or folder entries".into());}let mut result=Expansion{inputs:inputs.to_vec(),entries:Vec::with_capacity(inputs.len()),counts:Vec::with_capacity(inputs.len()),files:Vec::new(),folders:Vec::new()};
    for input in inputs{let entry=absolute_entry(input,budget)?;let path=PathBuf::from(entry.path());let state=stamp(&path)?;
        match entry.kind(){SlideShowEntryKind::File=>{if !state.file{return Err(format!("Slideshow file entry is not a regular file: {}",path.display()));}result.files.push(FileStamp{path,stamp:state});result.counts.push(1);},SlideShowEntryKind::Directory=>{
            if !state.directory{return Err(format!("Slideshow folder entry is not a directory: {}",path.display()));}let files=scan(&path,budget)?;result.counts.push(files.len()as u16);result.files.extend(files.iter().cloned());result.folders.push(Folder{path,stamp:state,files});
        }}if result.files.len()>MAX_SLIDES{return Err("Slideshow expansion exceeds 256 playable images; nothing was truncated".into());}result.entries.push(entry);
    }
    budget.images(result.files.len())?;Ok(result)
}
impl Expansion{
    pub fn image_count(&self)->usize{self.files.len()}
    pub fn watch_signature(&self)->WatchSignature{WatchSignature{entries:self.entries.clone(),files:self.files.clone(),folders:self.folders.clone()}}
    pub fn verify(&self,budget:&mut Budget)->Result<(),String>{for folder in &self.folders{if stamp(&folder.path)?!=folder.stamp||scan(&folder.path,budget)?!=folder.files{return Err(format!("Slideshow folder changed during preparation; retry: {}",folder.path.display()));}}for file in &self.files{if stamp(&file.path)?!=file.stamp{return Err(format!("Slideshow image changed during preparation; retry: {}",file.path.display()));}}Ok(())}
    pub fn prepare(self,budget:&mut Budget,reads:&mut crate::assets::ReadBudget,decoded:&mut usize,max_decoded:usize)->Result<Prepared,String>{
        let mut images=Vec::with_capacity(self.files.len());for file in &self.files{let prepared=crate::assets::prepare_image_with_limits(&file.path,reads,max_decoded.saturating_sub(*decoded)).map_err(|error|format!("{}: {error}",file.path.display()))?;budget.text(prepared.asset.path().len()+std::mem::size_of::<ImageAsset>())?;*decoded=decoded.checked_add(prepared.decoded_bytes).filter(|n|*n<=max_decoded).ok_or("Slideshow cumulative decoded-image budget exhausted")?;images.push(prepared.asset);drop(prepared.frame);}
        self.verify(budget)?;Ok(Prepared{inputs:self.inputs,entries:self.entries,counts:self.counts,images,scanned_entries:budget.visits})
    }
}
pub fn prepare_entries(entries:&[SlideShowEntry])->Result<Prepared,String>{let mut budget=Budget::default();let expansion=expand(entries,&mut budget)?;let mut reads=crate::assets::ReadBudget::new(128*1024*1024);let mut decoded=0;expansion.prepare(&mut budget,&mut reads,&mut decoded,crate::assets::MAX_ANIMATION_BYTES)}
/// Worker-only preparation against an immutable unpublished model. Keeping the
/// caller's Arc allows a failed strict refresh to retain its repair candidate.
/// Host must pre-admit one old/provisional snapshot before calling.
pub fn prepare_refreshed_snapshot(snapshot:&skylive_core::StudioSnapshot)->Result<Option<skylive_core::StudioSnapshot>,crate::storage::StorageError>{
 let mut budget=Budget::default();let mut reads=crate::assets::ReadBudget::new(128*1024*1024);let mut decoded=0;let mut updates=Vec::new();let mut folder_sources=0usize;
 for source in snapshot.sources(){if let skylive_core::SourceKind::ImageSlideShow{settings}=source.kind(){if settings.has_directories(){folder_sources+=1;if folder_sources>64{return Err(crate::storage::slideshow_refresh_error("Slideshow load refresh exceeds 64 folder-backed sources"));}let expansion=expand(settings.entries(),&mut budget).map_err(crate::storage::slideshow_refresh_error)?;let prepared=expansion.prepare(&mut budget,&mut reads,&mut decoded,crate::assets::MAX_ANIMATION_BYTES).map_err(crate::storage::slideshow_refresh_error)?;let refreshed=prepared.into_settings(settings).map_err(crate::storage::slideshow_refresh_error)?;if &refreshed!=settings{updates.push((source.id(),refreshed));}}}}
 if updates.is_empty(){return Ok(None);}snapshot.with_refreshed_slideshows(updates).map(Some).map_err(|error|crate::storage::slideshow_refresh_error(error.to_string()))
}
/// Normal strict load is unchanged: a refresh error returns no LoadedDocument.
pub fn refresh_loaded(loaded:crate::storage::LoadedDocument)->Result<crate::storage::LoadedDocument,crate::storage::StorageError>{
 let snapshot=loaded.session.snapshot();let Some(candidate)=prepare_refreshed_snapshot(&snapshot)?else{return Ok(loaded);};
 let preview=loaded.session.preview();let program=loaded.session.program();let mut session=skylive_core::Session::from_snapshot(candidate,false).map_err(|error|crate::storage::slideshow_refresh_error(error.to_string()))?;session.select_program(program).map_err(|error|crate::storage::slideshow_refresh_error(error.to_string()))?;session.select_preview(preview).map_err(|error|crate::storage::slideshow_refresh_error(error.to_string()))?;Ok(crate::storage::LoadedDocument{session,migration:loaded.migration})
}
