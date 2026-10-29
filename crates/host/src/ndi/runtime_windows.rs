//! Direct official NDI6 SDK exports. No OBS C++ plugin, PATH/CWD probing,
//! implicit download or installation. Construct only on the NDI owner.
use super::{abi::*,NdiError};
use std::{ffi::c_char,path::PathBuf,sync::{Arc,Mutex,OnceLock}};
use windows::{core::{PCSTR,PCWSTR},Win32::{Foundation::{FreeLibrary,HMODULE},System::LibraryLoader::{LoadLibraryExW,GetProcAddress,LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,LOAD_LIBRARY_SEARCH_DEFAULT_DIRS}}};
struct Module(HMODULE);
// SAFETY: the immutable OS loader handle is process-wide; per-instance SDK
// objects remain serialized on their own workers. The global Arc pins this one
// successfully initialized module for the process lifetime.
unsafe impl Send for Module {}
unsafe impl Sync for Module {}
impl Drop for Module {fn drop(&mut self){unsafe{let _=FreeLibrary(self.0);}}}
pub(super) struct Api {
    pub find_new:FindNew,pub find_free:FindFree,pub find_sources:FindSources,pub find_wait:FindWait,
    pub recv_new:RecvNew,pub recv_free:RecvFree,pub capture:Capture,pub free_video:FreeVideo,pub free_audio:FreeAudio,pub free_metadata:FreeMetadata,pub connections:Connections,
    pub version:String,destroy:Destroy,_module:Module,
}
impl Drop for Api {fn drop(&mut self){unsafe{(self.destroy)();}}}
unsafe fn symbol<T:Copy>(module:HMODULE,name:&'static [u8])->Result<T,NdiError>{
    let pointer=unsafe{GetProcAddress(module,PCSTR(name.as_ptr()))}.ok_or_else(||NdiError::Runtime(format!("NDI runtime is missing export {}",String::from_utf8_lossy(&name[..name.len()-1]))))?;
    if std::mem::size_of::<T>()!=std::mem::size_of_val(&pointer){return Err(NdiError::Runtime("NDI ABI pointer size mismatch".into()));}
    Ok(unsafe{std::mem::transmute_copy(&pointer)})
}
pub(super) unsafe fn text(pointer:*const c_char,optional:bool)->Result<Option<String>,NdiError>{
    if pointer.is_null(){return if optional {Ok(None)}else{Err(NdiError::Invalid("NDI returned a null source name"))};}
    for length in 0..=4096 {if unsafe{pointer.add(length).read()}==0 {
        let bytes=unsafe{std::slice::from_raw_parts(pointer.cast::<u8>(),length)};
        let value=std::str::from_utf8(bytes).map_err(|_|NdiError::Invalid("NDI metadata is not UTF-8"))?;
        if value.is_empty()&&optional{return Ok(None);}
        if value.trim().is_empty()||value.chars().any(char::is_control){return Err(NdiError::Invalid("NDI source metadata is empty or contains controls"));}
        return Ok(Some(value.to_owned()));
    }}Err(NdiError::Limit("NDI source metadata exceeds4096 bytes"))
}
pub(super) fn load()->Result<Arc<Api>,NdiError>{
    static GLOBAL:OnceLock<Mutex<Option<Arc<Api>>>>=OnceLock::new();
    let mut state=GLOBAL.get_or_init(||Mutex::new(None)).lock().map_err(|_|NdiError::Runtime("NDI runtime registry is poisoned".into()))?;
    if let Some(api)=&*state{return Ok(api.clone());}
    let api=load_installed()?;*state=Some(api.clone());Ok(api)
}
fn load_installed()->Result<Arc<Api>,NdiError>{
    if !cfg!(target_arch="x86_64"){return Err(NdiError::Unavailable("NDI receive runtime port currently requires Windows x64"));}
    let mut paths=Vec::new();
    if let Some(folder)=std::env::var_os("NDI_RUNTIME_DIR_V6") {let folder=PathBuf::from(folder);if folder.as_os_str().len()>4096{return Err(NdiError::Limit("NDI runtime directory exceeds4096 code units"));}if !folder.is_absolute(){return Err(NdiError::Runtime("NDI_RUNTIME_DIR_V6 must be an absolute installed runtime directory".into()));}paths.push(folder.join("Processing.NDI.Lib.x64.dll"));}
    for folder in [r"C:\Program Files\NDI\NDI 6 Runtime\v6",r"C:\Program Files\NDI\NDI 6 Runtime",r"C:\Program Files\NDI\NDI Runtime\v6"] {paths.push(PathBuf::from(folder).join("Processing.NDI.Lib.x64.dll"));}
    let mut failures=Vec::new();
    for path in paths {
        if !path.is_file(){failures.push(format!("{}: not installed",path.display()));continue;}
        let path=std::fs::canonicalize(&path).map_err(|e|NdiError::Runtime(format!("{}: {e}",path.display())))?;
        use std::os::windows::ffi::OsStrExt;
        let wide:Vec<u16>=path.as_os_str().encode_wide().chain(Some(0)).collect();
        let module=match unsafe{LoadLibraryExW(PCWSTR(wide.as_ptr()),None,LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR|LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)} {Ok(module)=>Module(module),Err(e)=>{failures.push(format!("{}: {e}",path.display()));continue;}};
        unsafe{
            // Presence of the official v6 table export verifies the installed
            // library supports the v6 API; all calls below use direct exports.
            let _:unsafe extern "C" fn()->*const std::ffi::c_void=symbol(module.0,b"NDIlib_v6_load\0")?;
            let initialize:Initialize=symbol(module.0,b"NDIlib_initialize\0")?;
            let destroy:Destroy=symbol(module.0,b"NDIlib_destroy\0")?;
            let version_fn:Version=symbol(module.0,b"NDIlib_version\0")?;
            let find_new=symbol(module.0,b"NDIlib_find_create_v2\0")?;
            let find_free=symbol(module.0,b"NDIlib_find_destroy\0")?;
            let find_sources=symbol(module.0,b"NDIlib_find_get_current_sources\0")?;
            let find_wait=symbol(module.0,b"NDIlib_find_wait_for_sources\0")?;
            let recv_new=symbol(module.0,b"NDIlib_recv_create_v3\0")?;
            let recv_free=symbol(module.0,b"NDIlib_recv_destroy\0")?;
            let capture=symbol(module.0,b"NDIlib_recv_capture_v3\0")?;
            let free_video=symbol(module.0,b"NDIlib_recv_free_video_v2\0")?;
            let free_audio=symbol(module.0,b"NDIlib_recv_free_audio_v3\0")?;
            let free_metadata=symbol(module.0,b"NDIlib_recv_free_metadata\0")?;
            let connections=symbol(module.0,b"NDIlib_recv_get_no_connections\0")?;
            let version=text(version_fn(),false)?.unwrap();
            if !initialize(){return Err(NdiError::Runtime(format!("NDI initialize rejected this CPU/runtime ({version})")));}
            return Ok(Arc::new(Api {find_new,find_free,find_sources,find_wait,recv_new,recv_free,capture,free_video,free_audio,free_metadata,connections,version,destroy,_module:module}));
        }
    }
    Err(NdiError::Runtime(format!("Official NDI6 Runtime is unavailable. Install it separately or set NDI_RUNTIME_DIR_V6. {}",failures.join("; "))))
}

#[derive(Clone,Copy)]
pub(super) struct SendApi {pub create:SendNew,pub free:SendFree,pub video:SendVideoAsync,pub audio:SendAudio,pub connections:SendConnections,pub source:SendSource}
impl Api {
    /// Resolve sender exports only on an explicit output Start. Existing receive
    /// initialization does not require or exercise the sender API.
    pub(super) fn sender(&self)->Result<SendApi,NdiError>{unsafe{Ok(SendApi {create:symbol(self._module.0,b"NDIlib_send_create\0")?,free:symbol(self._module.0,b"NDIlib_send_destroy\0")?,video:symbol(self._module.0,b"NDIlib_send_send_video_async_v2\0")?,audio:symbol(self._module.0,b"NDIlib_send_send_audio_v3\0")?,connections:symbol(self._module.0,b"NDIlib_send_get_no_connections\0")?,source:symbol(self._module.0,b"NDIlib_send_get_source_name\0")?})}}
}
