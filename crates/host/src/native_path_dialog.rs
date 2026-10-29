//! Explicit path selection on the existing filesystem worker, never the GUI.
#[cfg(target_os="windows")]
use std::path::{Path, PathBuf};

pub const MAX_PATHS: usize = 256;
pub const MAX_PATH_BYTES: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { BrowserHtml,Font,TextFile,RemuxRecordings,Lut,ScreenshotSave,ImageMask,Image, Media, Images, ImageFolder, RecordingFolder, DocumentOpen, DocumentSave, ObsOpen, ProfileOpen, ProfileSave }
#[derive(Clone)]
pub struct Request {
    /// Borrowed native owner identity. The GUI never waits for this operation;
    /// Show's native modality disables owner input while its event loop pumps.
    pub owner_hwnd: isize,
    pub kind: Kind,
    pub title: String,
    pub initial: String,
    pub filter_label: String,
    pub all_files_label: String,
}

pub fn choose(request: Request) -> Result<Option<Vec<String>>, String> {
    if request.owner_hwnd == 0 { return Err("The native file chooser has no application window owner.".into()); }
    if request.initial.len()>MAX_PATH_BYTES || request.initial.chars().any(char::is_control) {
        return Err("Native file chooser initial path exceeds its bounded input contract.".into());
    }
    for value in [&request.title, &request.filter_label, &request.all_files_label] {
        if value.len() > 512 || value.chars().any(char::is_control) {
            return Err("Native file chooser text exceeds its bounded input contract.".into());
        }
    }
    #[cfg(target_os = "windows")]
    { native::choose(request) }
    #[cfg(not(target_os = "windows"))]
    { let _ = request; Err("Native path selection is currently available on Windows.".into()) }
}
#[cfg(target_os="windows")]
fn exact_path(path: &Path) -> Result<String, String> {
    let value = path.to_str().ok_or("The selected path is not valid UTF-8.")?;
    if value.is_empty() || value.len() > MAX_PATH_BYTES || value.chars().any(char::is_control) {
        return Err("Each selected path requires 1..=4096 UTF-8 bytes without control characters.".into());
    }
    Ok(value.to_owned())
}
#[cfg(target_os="windows")]
fn image_folder(path: &Path) -> Result<Vec<String>, String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for (index, entry) in std::fs::read_dir(path).map_err(|e|format!("{}: {e}",path.display()))?.enumerate() {
        if index >= 4096 { return Err("The selected image folder exceeds 4096 entries; choose a smaller folder.".into()); }
        let entry=entry.map_err(|e|e.to_string())?;
        if !entry.file_type().map_err(|e|e.to_string())?.is_file() { continue; }
        let file=entry.path();
        let supported=file.extension().and_then(|e|e.to_str()).is_some_and(|e| {
            ["png","apng","jpg","jpeg","bmp","tga","gif","webp"].iter().any(|ext|e.eq_ignore_ascii_case(ext))
        });
        if !supported { continue; }
        if paths.len() >= MAX_PATHS { return Err("The selected folder contains more than 256 supported images.".into()); }
        paths.push(file);
    }
    paths.sort();
    if paths.is_empty() { return Err("The selected folder contains no supported image files.".into()); }
    paths.iter().map(|p|exact_path(p)).collect()
}

#[cfg(target_os = "windows")]
mod native {
    use super::*;
    use std::ffi::c_void;
    use windows::{
        core::{Interface, PCWSTR, PWSTR, HRESULT},
        Win32::{
            Foundation::HWND,
            System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, IBindCtx},
            UI::{WindowsAndMessaging::{IsWindow, GetWindowThreadProcessId}, Shell::{
                Common::COMDLG_FILTERSPEC, IFileDialog, IFileOpenDialog, IFileSaveDialog, IShellItem,
                FileOpenDialog, FileSaveDialog, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
                FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PATHMUSTEXIST, FOS_FILEMUSTEXIST,
                FOS_OVERWRITEPROMPT, FOS_PICKFOLDERS, FOS_ALLOWMULTISELECT,
            }},
        },
    };
    struct Apartment;
    impl Apartment {
        fn enter() -> Result<Self,String> {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED|COINIT_DISABLE_OLE1DDE) }
                .ok().map_err(|e|format!("Initialize file chooser STA: {e}"))?;
            Ok(Self)
        }
    }
    impl Drop for Apartment { fn drop(&mut self) { unsafe { CoUninitialize(); } } }
    struct ShellString(PWSTR);
    impl Drop for ShellString {
        fn drop(&mut self) { unsafe { CoTaskMemFree(Some(self.0.0.cast::<c_void>())); } }
    }
    fn wide(value: &str) -> Vec<u16> { value.encode_utf16().chain(Some(0)).collect() }
    fn item_path(item: &IShellItem) -> Result<String,String> {
        let text=ShellString(unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.map_err(|e|e.to_string())?);
        if text.0.0.is_null() { return Err("The native chooser returned an empty path.".into()); }
        let mut len=0;
        // Every accepted UTF-8 path has at most4096 UTF-16 units. Never perform
        // an unbounded PWSTR conversion before enforcing our application cap.
        while len<=MAX_PATH_BYTES && unsafe { *text.0.0.add(len) }!=0 { len+=1; }
        if len>MAX_PATH_BYTES { return Err("The selected path exceeds 4096 bytes.".into()); }
        let value=String::from_utf16(unsafe { std::slice::from_raw_parts(text.0.0,len) })
            .map_err(|e|format!("The selected path contains invalid Unicode: {e}"))?;
        exact_path(Path::new(&value))
    }
    fn configure(dialog: &IFileDialog, request: &Request) -> Result<(),String> {
        let title=wide(&request.title);
        unsafe { dialog.SetTitle(PCWSTR(title.as_ptr())) }.map_err(|e|e.to_string())?;
        let mut options=unsafe { dialog.GetOptions() }.map_err(|e|e.to_string())?
            | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR | FOS_PATHMUSTEXIST;
        if request.kind==Kind::ScreenshotSave {options &= !FOS_OVERWRITEPROMPT;}
        else if matches!(request.kind,Kind::DocumentSave|Kind::ProfileSave) { options |= FOS_OVERWRITEPROMPT; }
        else if !matches!(request.kind,Kind::ImageFolder|Kind::RecordingFolder) { options |= FOS_FILEMUSTEXIST; }
        if matches!(request.kind,Kind::Images|Kind::RemuxRecordings) { options |= FOS_ALLOWMULTISELECT; }
        if matches!(request.kind,Kind::ImageFolder|Kind::RecordingFolder) { options |= FOS_PICKFOLDERS; }
        unsafe { dialog.SetOptions(options) }.map_err(|e|e.to_string())?;
        if !matches!(request.kind,Kind::ImageFolder|Kind::RecordingFolder) {
            let label=wide(&request.filter_label);
            let all_label=wide(&request.all_files_label);
            let pattern=wide(match request.kind {
                Kind::BrowserHtml=>"*.html;*.htm;*.xhtml",
                Kind::Font=>"*.ttf;*.ttc",
                Kind::TextFile=>"*.txt;*.log",
                Kind::Lut=>"*.cube;*.png",
                Kind::ImageMask=>"*.png;*.apng;*.jpg;*.jpeg;*.bmp;*.tga;*.gif;*.webp",
                Kind::Image|Kind::Images => "*.png;*.apng;*.jpg;*.jpeg;*.bmp;*.tga;*.gif;*.webp",
                Kind::RemuxRecordings=>"*.mkv",
                Kind::Media => "*.mp4;*.mov;*.m4a",
                Kind::DocumentOpen|Kind::DocumentSave|Kind::ObsOpen => "*.json",
                Kind::ProfileOpen|Kind::ProfileSave => "*.skyliveprofile;*.json",
                Kind::ScreenshotSave => "*.png",
                Kind::ImageFolder|Kind::RecordingFolder => "*.*",
            });
            let all=wide("*.*");
            let filters=[COMDLG_FILTERSPEC {pszName:PCWSTR(label.as_ptr()),pszSpec:PCWSTR(pattern.as_ptr())},
                COMDLG_FILTERSPEC {pszName:PCWSTR(all_label.as_ptr()),pszSpec:PCWSTR(all.as_ptr())}];
            unsafe { dialog.SetFileTypes(&filters) }.map_err(|e|e.to_string())?;
            if matches!(request.kind,Kind::DocumentSave|Kind::ProfileSave|Kind::ScreenshotSave) {
                let extension=wide(if request.kind==Kind::ProfileSave {"skyliveprofile"} else if request.kind==Kind::ScreenshotSave {"png"} else {"json"});
                unsafe { dialog.SetDefaultExtension(PCWSTR(extension.as_ptr())) }.map_err(|e|e.to_string())?;
            }
        }
        // Initial text is advisory; a malformed old field must remain fixable
        // by Browse. Filesystem probing stays entirely on this I/O worker.
        if !request.initial.is_empty() && request.initial.len()<=MAX_PATH_BYTES
            && !request.initial.chars().any(char::is_control) {
            let initial=Path::new(&request.initial);
            let folder=if initial.is_dir() { Some(initial) } else { initial.parent().filter(|p|p.is_dir()) };
            if let Some(folder)=folder.and_then(Path::to_str) {
                let folder=wide(folder);
                let item: windows::core::Result<IShellItem>=unsafe { SHCreateItemFromParsingName(PCWSTR(folder.as_ptr()),None::<&IBindCtx>) };
                if let Ok(item)=item { let _=unsafe { dialog.SetFolder(&item) }; }
            }
            if !initial.is_dir() && !matches!(request.kind,Kind::ImageFolder|Kind::RecordingFolder) {
                if let Some(name)=initial.file_name().and_then(|p|p.to_str()) {
                    let name=wide(name); let _=unsafe { dialog.SetFileName(PCWSTR(name.as_ptr())) };
                }
            }
        }
        Ok(())
    }
    fn shown(dialog: &IFileDialog, owner: HWND) -> Result<bool,String> {
        match unsafe { dialog.Show(owner) } {
            Ok(())=>Ok(true),
            Err(error) if error.code()==HRESULT::from_win32(1223)=>Ok(false),
            Err(error)=>Err(format!("Native file chooser: {error}")),
        }
    }
    pub(super) fn choose(request: Request) -> Result<Option<Vec<String>>,String> {
        let _apartment=Apartment::enter()?;
        let owner=HWND(request.owner_hwnd as *mut c_void);
        let mut owner_process=0u32;
        unsafe { GetWindowThreadProcessId(owner,Some(&mut owner_process as *mut u32)); }
        if !unsafe { IsWindow(owner) }.as_bool() || owner_process!=std::process::id() { return Err("The file chooser's application window is no longer available.".into()); }
        if matches!(request.kind,Kind::DocumentSave|Kind::ProfileSave|Kind::ScreenshotSave) {
            let save:IFileSaveDialog=unsafe { CoCreateInstance(&FileSaveDialog,None,CLSCTX_INPROC_SERVER) }.map_err(|e|e.to_string())?;
            let dialog:IFileDialog=save.cast().map_err(|e|e.to_string())?;
            configure(&dialog,&request)?;
            if !shown(&dialog,owner)? { return Ok(None); }
            let item=unsafe { dialog.GetResult() }.map_err(|e|e.to_string())?;
            return Ok(Some(vec![item_path(&item)?]));
        }
        let open:IFileOpenDialog=unsafe { CoCreateInstance(&FileOpenDialog,None,CLSCTX_INPROC_SERVER) }.map_err(|e|e.to_string())?;
        let dialog:IFileDialog=open.cast().map_err(|e|e.to_string())?;
        configure(&dialog,&request)?;
        if !shown(&dialog,owner)? { return Ok(None); }
        if request.kind==Kind::ImageFolder {
            let item=unsafe { dialog.GetResult() }.map_err(|e|e.to_string())?;
            return image_folder(Path::new(&item_path(&item)?)).map(Some);
        }
        if request.kind==Kind::RecordingFolder {
            let item=unsafe { dialog.GetResult() }.map_err(|e|e.to_string())?;
            return Ok(Some(vec![item_path(&item)?]));
        }
        let items=unsafe { open.GetResults() }.map_err(|e|e.to_string())?;
        let count=unsafe { items.GetCount() }.map_err(|e|e.to_string())? as usize;
        let cap=if request.kind==Kind::RemuxRecordings {crate::remux::MAX_JOBS}else if request.kind==Kind::Images {MAX_PATHS}else{1};
        if count==0 || count>cap { return Err("Native selection exceeds the allowed file count (maximum 256).".into()); }
        let mut paths=Vec::with_capacity(count);
        for index in 0..count {
            let item=unsafe { items.GetItemAt(index as u32) }.map_err(|e|e.to_string())?;
            paths.push(item_path(&item)?);
        }
        Ok(Some(paths))
    }
}
