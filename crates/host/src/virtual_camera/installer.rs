//! Explicit user-requested deployment, called only on the filesystem worker.
pub fn request(owner: isize) -> Result<bool, String> {
    #[cfg(not(all(target_os="windows",target_arch="x86_64")))]
    { let _=owner; Err("Virtual camera installation requires Windows 11 x64.".into()) }
    #[cfg(all(target_os="windows",target_arch="x86_64"))]
    {
        use std::{ffi::c_void,os::windows::ffi::OsStrExt};
        use windows::{core::PCWSTR,Win32::{Foundation::{CloseHandle,HWND,WAIT_OBJECT_0,WAIT_TIMEOUT},
            System::{Com::{CoInitializeEx,CoUninitialize,COINIT_APARTMENTTHREADED,COINIT_DISABLE_OLE1DDE},Threading::{GetExitCodeProcess,WaitForSingleObject}},
            UI::{Shell::{ShellExecuteExW,SHELLEXECUTEINFOW,SEE_MASK_NOCLOSEPROCESS,SEE_MASK_NOASYNC},WindowsAndMessaging::{GetWindowThreadProcessId,IsWindow,SW_HIDE}}}};
        let hwnd=HWND(owner as *mut c_void);
        let mut process_id=0;
        if owner==0 || !unsafe{IsWindow(hwnd)}.as_bool() {
            return Err("The virtual-camera installer has no application window owner.".into());
        }
        unsafe{GetWindowThreadProcessId(hwnd,Some(&mut process_id));}
        if process_id!=std::process::id() {
            return Err("The virtual-camera installation window owner is no longer valid.".into());
        }
        struct Apartment;
        impl Drop for Apartment {fn drop(&mut self){unsafe{CoUninitialize();}}}
        unsafe {CoInitializeEx(None,COINIT_APARTMENTTHREADED|COINIT_DISABLE_OLE1DDE)}.ok().map_err(|error|error.to_string())?;
        let _apartment=Apartment;
        let executable=std::env::current_exe().map_err(|error|error.to_string())?;
        let helper=executable.parent().ok_or("The application has no installation directory")?.join("skylive-virtual-camera-install.exe");
        if !helper.is_file() {return Err("Virtual-camera installer is missing from this build.".into());}
        let file:Vec<u16>=helper.as_os_str().encode_wide().chain(Some(0)).collect();
        let verb:Vec<u16>="runas".encode_utf16().chain(Some(0)).collect();
        let parameters:Vec<u16>="--install".encode_utf16().chain(Some(0)).collect();
        let mut info=SHELLEXECUTEINFOW {cbSize:std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask:SEE_MASK_NOCLOSEPROCESS|SEE_MASK_NOASYNC,hwnd,
            lpVerb:PCWSTR(verb.as_ptr()),lpFile:PCWSTR(file.as_ptr()),lpParameters:PCWSTR(parameters.as_ptr()),
            nShow:SW_HIDE.0,..Default::default()};
        if let Err(error)=unsafe{ShellExecuteExW(&mut info)} {
            if error.code()==windows::core::HRESULT::from_win32(1223) {return Ok(false);}
            return Err(format!("Virtual-camera installation: {error}"));
        }
        if info.hProcess.is_invalid() {return Err("The installer returned no process handle.".into());}
        struct Process(windows::Win32::Foundation::HANDLE);
        impl Drop for Process {fn drop(&mut self){let _=unsafe{CloseHandle(self.0)};}}
        let process=Process(info.hProcess);
        loop {
            match unsafe{WaitForSingleObject(process.0,50)} {
                WAIT_OBJECT_0=>break,
                WAIT_TIMEOUT=>{},
                _=>return Err(std::io::Error::last_os_error().to_string()),
            }
        }
        let mut code=0;
        unsafe{GetExitCodeProcess(process.0,&mut code)}.map_err(|error|error.to_string())?;
        if code!=0 {return Err(format!("Virtual-camera installer exited with code {code}"));}
        Ok(true)
    }
}
