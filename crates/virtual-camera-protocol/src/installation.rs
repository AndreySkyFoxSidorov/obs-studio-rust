//! Explicit administrator install only. Never called by app construction/Start.
use super::{CLASS_ID,DLL_NAME};
use std::{ffi::c_void,fs,io,path::{Path,PathBuf}};
use windows_core::GUID;
use crate::transport::wide;
type Handle=*mut c_void;
#[link(name="kernel32")]
unsafe extern "system" {
    fn GetModuleFileNameW(module:Handle,path:*mut u16,size:u32)->u32;
    fn LoadLibraryExW(path:*const u16,file:Handle,flags:u32)->Handle;
    fn GetProcAddress(module:Handle,name:*const u8)->*mut c_void;
    fn FreeLibrary(module:Handle)->i32;
}
#[link(name="shell32")]
unsafe extern "system"{fn SHGetKnownFolderPath(id:*const GUID,flags:u32,token:Handle,path:*mut *mut u16)->i32;fn IsUserAnAdmin()->i32;}
#[link(name="ole32")]
unsafe extern "system"{fn CoTaskMemFree(value:*const c_void);}
#[link(name="advapi32")]
unsafe extern "system" {
    fn RegCreateKeyExW(key:Handle,path:*const u16,reserved:u32,class:*const u16,options:u32,access:u32,attributes:*const c_void,result:*mut Handle,disposition:*mut u32)->i32;
    fn RegSetValueExW(key:Handle,name:*const u16,reserved:u32,kind:u32,data:*const u8,bytes:u32)->i32;
    fn RegGetValueW(key:Handle,path:*const u16,name:*const u16,flags:u32,kind:*mut u32,data:*mut c_void,bytes:*mut u32)->i32;
    fn RegDeleteTreeW(key:Handle,path:*const u16)->i32;
    fn RegCloseKey(key:Handle)->i32;
}
fn hklm()->Handle{(0x80000002u32 as i32 as isize) as Handle}
fn win(value:i32)->io::Result<()>{if value==0{Ok(())}else{Err(io::Error::from_raw_os_error(value))}}
pub fn module_path(module:Handle)->io::Result<PathBuf>{
    let mut path=vec![0u16;32768];let n=unsafe{GetModuleFileNameW(module,path.as_mut_ptr(),path.len() as u32)} as usize;
    if n==0 || n>=path.len(){return Err(io::Error::last_os_error());}
    Ok(PathBuf::from(String::from_utf16(&path[..n]).map_err(|_|io::Error::other("module path UTF16"))?))
}
pub fn protected_directory()->io::Result<PathBuf>{
    let id=GUID::from_u128(0x905e63b6_c1bf_494e_b29c_65b732d3d21a);let mut value=std::ptr::null_mut();
    let hr=unsafe{SHGetKnownFolderPath(&id,0,std::ptr::null_mut(),&mut value)};
    if hr<0 || value.is_null(){return Err(io::Error::other(format!("Program Files HRESULT {hr:#x}")));}
    let mut n=0;while n<32768 && unsafe{*value.add(n)}!=0{n+=1;}
    let text=String::from_utf16(unsafe{std::slice::from_raw_parts(value,n)});
    unsafe{CoTaskMemFree(value.cast());}
    Ok(PathBuf::from(text.map_err(|_|io::Error::other("Program Files UTF16"))?).join("SkyLive").join("VirtualCamera").join("0.1.0"))
}
pub fn registered_module()->io::Result<PathBuf>{
    let key=wide(&format!("Software\\Classes\\CLSID\\{CLASS_ID}\\InprocServer32"));let mut path=vec![0u16;32768];let mut bytes=(path.len()*2) as u32;
    win(unsafe{RegGetValueW(hklm(),key.as_ptr(),std::ptr::null(),2|0x10000,std::ptr::null_mut(),path.as_mut_ptr().cast(),&mut bytes)})?;
    let len=(bytes as usize/2).saturating_sub(1);if len>=path.len(){return Err(io::Error::other("COM DLL path exceeds limit"));}
    Ok(PathBuf::from(String::from_utf16(&path[..len]).map_err(|_|io::Error::other("COM DLL path UTF16"))?))
}
fn protected(path:&Path)->io::Result<PathBuf>{
    let path=fs::canonicalize(path)?;let root=fs::canonicalize(protected_directory()?)?;
    if !path.starts_with(root) || path.file_name()!=Some(std::ffi::OsStr::new(DLL_NAME)) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,"virtual camera DLL must be installed in protected Program Files"));
    }Ok(path)
}
pub fn register_module(path:&Path)->io::Result<()> {
    if unsafe{IsUserAnAdmin()}==0{return Err(io::Error::new(io::ErrorKind::PermissionDenied,"administrator installation required"));}
    let path=protected(path)?;let key=wide(&format!("Software\\Classes\\CLSID\\{CLASS_ID}\\InprocServer32"));let mut handle=std::ptr::null_mut();
    win(unsafe{RegCreateKeyExW(hklm(),key.as_ptr(),0,std::ptr::null(),0,0x20006|0x100,std::ptr::null(),&mut handle,std::ptr::null_mut())})?;
    let result=(||{
        let text=wide(path.to_str().ok_or_else(||io::Error::other("DLL path UTF16"))?);
        let name=wide("ThreadingModel");let value=wide("Both");
        win(unsafe{RegSetValueExW(handle,name.as_ptr(),0,1,value.as_ptr().cast(),(value.len()*2) as u32)})?;
        // Publish the DLL path only after the other registration value exists.
        win(unsafe{RegSetValueExW(handle,std::ptr::null(),0,1,text.as_ptr().cast(),(text.len()*2) as u32)})
    })();unsafe{RegCloseKey(handle);}result
}
pub fn unregister_module(path:&Path)->io::Result<()> {
    if unsafe{IsUserAnAdmin()}==0{return Err(io::Error::new(io::ErrorKind::PermissionDenied,"administrator uninstall required"));}
    let path=protected(path)?;
    if fs::canonicalize(registered_module()?)?!=path{return Err(io::Error::new(io::ErrorKind::PermissionDenied,"another COM registration owns this class"));}
    let key=wide(&format!("Software\\Classes\\CLSID\\{CLASS_ID}"));win(unsafe{RegDeleteTreeW(hklm(),key.as_ptr())})
}
pub fn install_bundle(source:&Path)->io::Result<PathBuf> {
    if unsafe{IsUserAnAdmin()}==0{return Err(io::Error::new(io::ErrorKind::PermissionDenied,"administrator installation required"));}
    let source=fs::canonicalize(source)?;let size=fs::metadata(&source)?.len();
    if size==0 || size>64*1024*1024{return Err(io::Error::new(io::ErrorKind::InvalidData,"virtual camera DLL size outside limit"));}
    let directory=protected_directory()?;fs::create_dir_all(&directory)?;let target=directory.join(DLL_NAME);
    if target.exists(){return Err(io::Error::new(io::ErrorKind::AlreadyExists,"virtual camera already installed; explicitly uninstall the old version first"));}
    let mut input=fs::File::open(source)?;let mut output=fs::OpenOptions::new().create_new(true).write(true).open(&target)?;
    let result=(||{
        std::io::copy(&mut input,&mut output)?;output.sync_all()?;drop(output);
        let path=wide(target.to_str().ok_or_else(||io::Error::other("DLL path UTF16"))?);
        let module=unsafe{LoadLibraryExW(path.as_ptr(),std::ptr::null_mut(),0x100|0x800)};
        if module.is_null(){return Err(io::Error::last_os_error());}
        let pointer=unsafe{GetProcAddress(module,b"DllRegisterServer\0".as_ptr())};
        let result=if pointer.is_null(){Err(io::Error::other("Rust COM registration export missing"))}else{
            let register:unsafe extern "system" fn()->i32=unsafe{std::mem::transmute(pointer)};
            let hr=unsafe{register()};if hr<0{Err(io::Error::other(format!("DllRegisterServer HRESULT {hr:#x}")))}else{Ok(())}
        };unsafe{FreeLibrary(module);}result
    })();
    match result {
        Ok(())=>Ok(target),
        Err(error)=>{
            // create_new makes this invocation the sole owner of the file.
            // Close the copy handle before removing a failed installation.
            let cleanup=fs::remove_file(&target);
            match cleanup {Ok(())=>Err(error),Err(cleanup)=>Err(io::Error::new(error.kind(),format!("{error}; could not remove incomplete virtual camera DLL: {cleanup}")))}
        }
    }
}
