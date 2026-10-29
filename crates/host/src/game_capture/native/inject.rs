//! The only remote entry points are system LoadLibraryW and this bundle's own
//! fixed start export. Data pages are RW, never executable. No helper process,
//! UAC, kernel driver, security-policy modification or generic shellcode port.
use super::*;
use target::{Raw,Process,os};
use std::{ffi::c_void,mem::{size_of,size_of_val}};
#[repr(C)]struct ModuleInfo {base:Raw,size:u32,entry:Raw}
#[link(name="kernel32")]
unsafe extern "system" {
    fn K32EnumProcessModulesEx(process:Raw,modules:*mut Raw,bytes:u32,needed:*mut u32,filter:u32)->i32;
    fn K32GetModuleFileNameExW(process:Raw,module:Raw,text:*mut u16,len:u32)->u32;
    fn K32GetModuleInformation(process:Raw,module:Raw,info:*mut ModuleInfo,bytes:u32)->i32;
    fn LoadLibraryExW(path:*const u16,file:Raw,flags:u32)->Raw;fn FreeLibrary(module:Raw)->i32;
    fn GetModuleHandleExW(flags:u32,address:*const u16,module:*mut Raw)->i32;
    fn VirtualAllocEx(process:Raw,address:Raw,bytes:usize,kind:u32,protect:u32)->Raw;
    fn VirtualFreeEx(process:Raw,address:Raw,bytes:usize,kind:u32)->i32;
    fn WriteProcessMemory(process:Raw,address:Raw,source:*const c_void,bytes:usize,written:*mut usize)->i32;
    fn CreateRemoteThread(process:Raw,attributes:Raw,stack:usize,start:Option<unsafe extern "system" fn(Raw)->u32>,parameter:Raw,flags:u32,id:*mut u32)->Raw;
    fn GetExitCodeThread(thread:Raw,code:*mut u32)->i32;
}
fn same_path(a:&str,b:&str)->bool {
    fn normal(value:&str)->String{if let Some(rest)=value.strip_prefix(r"\\?\UNC\"){format!(r"\\{rest}")}else{value.strip_prefix(r"\\?\").unwrap_or(value).to_owned()}}
    normal(a).eq_ignore_ascii_case(&normal(b))
}
pub(super) fn modules(process:Raw)->Result<Vec<(usize,u32,String)>,GameError>{
    let mut modules=[std::ptr::null_mut();512];let mut needed=0;
    if unsafe{K32EnumProcessModulesEx(process,modules.as_mut_ptr(),size_of_val(&modules) as u32,&mut needed,3)}==0{return Err(os("EnumProcessModules selected game"));}
    if needed as usize>size_of_val(&modules){return Err(GameError::Limit("game module list512"));}
    let mut result=Vec::new();
    for module in modules.into_iter().take(needed as usize/size_of::<Raw>()) {
        let mut text=[0u16;4096];let len=unsafe{K32GetModuleFileNameExW(process,module,text.as_mut_ptr(),4096)};
        if len==0||len>=4096{return Err(os("GetModuleFileNameEx selected game"));}
        let path=String::from_utf16(&text[..len as usize]).map_err(|_|GameError::Invalid("module path UTF16"))?;
        let mut info=ModuleInfo {base:std::ptr::null_mut(),size:0,entry:std::ptr::null_mut()};
        if unsafe{K32GetModuleInformation(process,module,&mut info,size_of::<ModuleInfo>() as u32)}==0{return Err(os("GetModuleInformation selected game"));}
        result.push((info.base as usize,info.size,path));
    }Ok(result)
}
struct LocalModule(Raw);impl Drop for LocalModule{fn drop(&mut self){unsafe{FreeLibrary(self.0);}}}
struct Allocation<'a>{process:&'a Process,pointer:Raw}
impl Drop for Allocation<'_>{fn drop(&mut self){unsafe{VirtualFreeEx(self.process.handle,self.pointer,0,0x8000);}}}
fn call(process:&Process,address:usize,data:&[u8],fresh:&impl Fn()->bool,started:&mut bool)->Result<u32,GameError>{
    if data.is_empty()||data.len()>16*1024{return Err(GameError::Limit("game remote argument16KiB"));}
    process.verify()?;if !fresh(){return Err(GameError::Stale);}
    let pointer=unsafe{VirtualAllocEx(process.handle,std::ptr::null_mut(),data.len(),0x3000,4)};
    if pointer.is_null(){return Err(os("VirtualAllocEx selected game"));}
    let allocation=Allocation {process,pointer};let mut written=0;
    if unsafe{WriteProcessMemory(process.handle,pointer,data.as_ptr().cast(),data.len(),&mut written)}==0||written!=data.len(){return Err(os("WriteProcessMemory game arguments"));}
    process.verify()?;if !fresh(){return Err(GameError::Stale);}
    let start:unsafe extern "system" fn(Raw)->u32=unsafe{std::mem::transmute(address)};
    let thread=unsafe{CreateRemoteThread(process.handle,std::ptr::null_mut(),0,Some(start),pointer,0,std::ptr::null_mut())};
    if thread.is_null(){return Err(os("CreateRemoteThread explicit game capture"));}
    *started=true;
    struct Thread(Raw);impl Drop for Thread{fn drop(&mut self){unsafe{target::CloseHandle(self.0);}}}let thread=Thread(thread);
    let mut wait_error=None;
    loop {
        match unsafe{target::WaitForSingleObject(thread.0,25)} {
            0=>break,258=>{},_=>{wait_error=Some(os("wait for game remote entry"));thread::sleep(Duration::from_millis(25));},
        }
        // Cancellation cannot free a still-running thread's argument page or
        // mean native quiescence. Continue owning it until actual completion.
        let _=fresh();
        if !process.alive(){break;}
    }
    let mut code=0;if unsafe{GetExitCodeThread(thread.0,&mut code)}==0{return Err(os("game remote entry result"));}
    drop(allocation);if let Some(error)=wait_error{eprintln!("Game remote entry recovered after wait diagnostic: {error}");}Ok(code)
}
pub(super) fn inject(process:&Process,init:&wire::Init,fresh:&impl Fn()->bool,possibly_loaded:&mut bool,start_may_run:&mut bool)->Result<(),GameError>{
    use std::os::windows::ffi::OsStrExt;
    let executable=std::env::current_exe().map_err(|e|GameError::Native(e.to_string()))?;
    let directory=std::fs::canonicalize(executable.parent().ok_or(GameError::Invalid("host executable directory"))?).map_err(|e|GameError::Native(e.to_string()))?;
    let path=std::fs::canonicalize(directory.join(wire::DLL_NAME)).map_err(|e|GameError::Native(format!("Bundled {}: {e}",wire::DLL_NAME)))?;
    if path.parent()!=Some(directory.as_path())||path.file_name()!=Some(std::ffi::OsStr::new(wire::DLL_NAME)){return Err(GameError::Invalid("game hook must be the bundled DLL beside the host executable"));}
    let metadata=std::fs::metadata(&path).map_err(|e|GameError::Native(e.to_string()))?;
    if !metadata.is_file()||metadata.len()>64*1024*1024{return Err(GameError::Limit("bundled game hook file64MiB"));}
    let wide:Vec<u16>=path.as_os_str().encode_wide().chain(Some(0)).collect();if wide.len()>4096{return Err(GameError::Limit("bundled game hook path4096"));}
    // DONT_RESOLVE_DLL_REFERENCES maps our own DLL solely to derive export RVAs;
    // its code and DllMain are never run in the host process.
    let local=unsafe{LoadLibraryExW(wide.as_ptr(),std::ptr::null_mut(),1)};
    if local.is_null(){return Err(os("map bundled game hook export table"));}let local=LocalModule(local);
    let abi=unsafe{target::GetProcAddress(local.0,b"SKYLIVE_GAME_HOOK_ABI\0".as_ptr())};
    let start=unsafe{target::GetProcAddress(local.0,b"SkyLiveGameHookStart\0".as_ptr())};
    if abi.is_null()||start.is_null()||unsafe{abi.cast::<u64>().read()}!=wire::MAGIC^wire::ABI{return Err(GameError::Invalid("bundled game hook ABI/export identity"));}
    let rva=(start as usize).checked_sub(local.0 as usize).ok_or(GameError::Invalid("game hook start RVA"))?;
    let path_text=path.to_str().ok_or(GameError::Invalid("bundled game DLL path UTF8"))?;
    let mut loaded=modules(process.handle)?.into_iter().find(|(_,_,name)|same_path(name,path_text));
    if loaded.is_none() {
        let kernel:Vec<u16>="kernel32.dll".encode_utf16().chain(Some(0)).collect();let kernel=unsafe{target::GetModuleHandleW(kernel.as_ptr())};
        let function=unsafe{target::GetProcAddress(kernel,b"LoadLibraryW\0".as_ptr())};if function.is_null(){return Err(os("resolve system LoadLibraryW"));}
        let mut containing=std::ptr::null_mut();if unsafe{GetModuleHandleExW(6,function.cast(),&mut containing)}==0{return Err(os("resolve LoadLibraryW containing module"));}
        let host=modules(unsafe{target::GetCurrentProcess()})?;
        let (_,size,name)=host.iter().find(|(base,_,_)|*base==containing as usize).ok_or(GameError::Invalid("system loader module identity"))?;
        let offset=(function as usize).checked_sub(containing as usize).ok_or(GameError::Invalid("system loader RVA"))?;
        let (remote,remote_size,_)=modules(process.handle)?.into_iter().find(|(_,_,path)|path.eq_ignore_ascii_case(name)).ok_or(GameError::Unsupported("selected process uses a different system loader module"))?;
        if *size!=remote_size||offset>=remote_size as usize{return Err(GameError::Unsupported("system loader module size/RVA mismatch"));}
        let data=unsafe{std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(),wide.len()*2)};
        let mut loader_started=false;let result=call(process,remote+offset,data,fresh,&mut loader_started);
        *possibly_loaded|=loader_started;
        let _=result.map_err(|error|if loader_started {GameError::AfterInjection(error.to_string())}else{error})?;
        loaded=modules(process.handle)?.into_iter().find(|(_,_,name)|same_path(name,path_text));
        if loaded.is_none(){*possibly_loaded=false;}
    }
    if loaded.is_some(){*possibly_loaded=true;}
    let (base,size,_)=loaded.ok_or(GameError::AfterInjection("LoadLibraryW did not publish the bundled game hook module".into()))?;
    if rva>=size as usize{return Err(GameError::AfterInjection("game hook export outside loaded module".into()));}
    let data=unsafe{std::slice::from_raw_parts((init as *const wire::Init).cast::<u8>(),size_of::<wire::Init>())};
    let code=call(process,base+rva,data,fresh,start_may_run).map_err(|e|GameError::AfterInjection(e.to_string()))?;
    if code!=0{*start_may_run=false;return Err(GameError::AfterInjection(format!("SkyLiveGameHookStart returned {code}; targetPID {} creation {} HWND {} executable {}",process.target.pid,process.target.creation_time,process.target.hwnd,process.target.executable)));}
    Ok(())
}
