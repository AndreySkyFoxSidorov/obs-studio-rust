use super::*;
use windows::{Win32::{Foundation::{BOOL,HWND,LPARAM,RECT},UI::WindowsAndMessaging::*}};
use std::ffi::c_void;
pub(super) type Raw=*mut c_void;
#[link(name="kernel32")]
unsafe extern "system" {
    pub fn OpenProcess(access:u32,inherit:i32,pid:u32)->Raw;pub fn CloseHandle(handle:Raw)->i32;
    pub fn GetCurrentProcess()->Raw;pub fn GetCurrentProcessId()->u32;
    pub fn GetProcessTimes(handle:Raw,creation:*mut u64,exit:*mut u64,kernel:*mut u64,user:*mut u64)->i32;
    pub fn QueryFullProcessImageNameW(handle:Raw,flags:u32,text:*mut u16,length:*mut u32)->i32;
    pub fn WaitForSingleObject(handle:Raw,timeout:u32)->u32;
    pub fn GetModuleHandleW(name:*const u16)->Raw;pub fn GetProcAddress(module:Raw,name:*const u8)->Raw;
}
pub(super) fn os(context:&str)->GameError{GameError::Native(format!("{context}: {}",std::io::Error::last_os_error()))}
pub(super) struct Process {pub handle:Raw,pub target:GameCaptureTarget}
impl Drop for Process {fn drop(&mut self){unsafe{CloseHandle(self.handle);}}}
pub(super) fn creation(handle:Raw)->Result<u64,GameError>{let(mut created,mut exit,mut kernel,mut user)=(0,0,0,0);
    if unsafe{GetProcessTimes(handle,&mut created,&mut exit,&mut kernel,&mut user)}==0{return Err(os("GetProcessTimes"));}Ok(created)}
pub(super) fn executable(handle:Raw)->Result<String,GameError>{
    let mut text=[0u16;4096];let mut length=4096;
    if unsafe{QueryFullProcessImageNameW(handle,0,text.as_mut_ptr(),&mut length)}==0{return Err(os("QueryFullProcessImageNameW"));}
    String::from_utf16(&text[..length as usize]).map_err(|_|GameError::Native(format!("Executable path UTF16: {:?}",&text[..length as usize])))
}
impl Process {
    pub fn open(target:&GameCaptureTarget,inject:bool)->Result<Self,GameError>{
        target.validate().map_err(|_|GameError::Invalid("invalid game process target"))?;
        if target.pid==unsafe{GetCurrentProcessId()}{return Err(GameError::Unsupported("capture cannot inject into its own host process"));}
        let access=if inject {0x001f0fff}else{0x00101000};
        // No elevation retry. Reduced explicit injection rights are enough;
        // PROCESS_ALL_ACCESS is never requested.
        let access=if inject {0x0010043a}else{access};
        let handle=unsafe{OpenProcess(access,0,target.pid)};if handle.is_null(){return Err(os("OpenProcess selected game"));}
        let process=Self {handle,target:target.clone()};process.verify()?;Ok(process)
    }
    pub fn alive(&self)->bool{(unsafe{WaitForSingleObject(self.handle,0)})==258}
    pub fn verify(&self)->Result<(),GameError>{
        if !self.alive()||creation(self.handle)?!=self.target.creation_time||executable(self.handle)?!=self.target.executable{return Err(GameError::TargetChanged);}
        let hwnd=HWND(self.target.hwnd as usize as Raw);let mut pid=0;
        unsafe{GetWindowThreadProcessId(hwnd,Some(&mut pid));}
        if pid!=self.target.pid||!unsafe{IsWindow(hwnd)}.as_bool(){return Err(GameError::TargetChanged);}
        let mut class=[0u16;257];let n=unsafe{GetClassNameW(hwnd,&mut class)};
        if n<=0||String::from_utf16(&class[..n as usize]).map_err(|_|GameError::Invalid("window class UTF16"))?!=self.target.class{return Err(GameError::TargetChanged);}
        let kernel:Vec<u16>="kernel32.dll".encode_utf16().chain(Some(0)).collect();
        let module=unsafe{GetModuleHandleW(kernel.as_ptr())};let function=unsafe{GetProcAddress(module,b"IsWow64Process2\0".as_ptr())};
        if function.is_null(){return Err(GameError::Unsupported("IsWow64Process2 is required for exact x64 injection"));}
        let function:unsafe extern "system" fn(Raw,*mut u16,*mut u16)->i32=unsafe{std::mem::transmute(function)};
        let(mut process_machine,mut native_machine)=(0u16,0u16);
        if unsafe{function(self.handle,&mut process_machine,&mut native_machine)}==0{return Err(os("IsWow64Process2"));}
        if process_machine!=0||native_machine!=0x8664{return Err(GameError::Unsupported("only native Windows x64 game processes are supported"));}
        Ok(())
    }
}
pub(super) fn targets()->Result<Vec<GameTargetInfo>,GameError>{
    struct Context {items:Vec<GameTargetInfo>,bytes:usize,error:Option<GameError>}
    unsafe extern "system" fn visit(hwnd:HWND,param:LPARAM)->BOOL {
        let context=unsafe{&mut *(param.0 as *mut Context)};
        if !unsafe{IsWindowVisible(hwnd)}.as_bool(){return BOOL(1);}
        let mut pid=0;unsafe{GetWindowThreadProcessId(hwnd,Some(&mut pid));}
        if pid<=4||pid==unsafe{GetCurrentProcessId()}{return BOOL(1);}
        let process=unsafe{OpenProcess(0x00101000,0,pid)};if process.is_null(){return BOOL(1);}
        struct Handle(Raw);impl Drop for Handle{fn drop(&mut self){unsafe{CloseHandle(self.0);}}}let handle=Handle(process);
        let Ok(executable)=executable(handle.0)else{return BOOL(1);};let Ok(created)=creation(handle.0)else{return BOOL(1);};
        let length=unsafe{GetWindowTextLengthW(hwnd)};if length<0||length>1024{return BOOL(1);}
        let mut title=vec![0u16;length as usize+1];let n=unsafe{GetWindowTextW(hwnd,&mut title)};
        let Ok(title)=String::from_utf16(&title[..n.max(0) as usize])else{return BOOL(1);};
        let mut class=[0u16;257];let n=unsafe{GetClassNameW(hwnd,&mut class)};if n<=0||n>=256{return BOOL(1);}
        let Ok(class)=String::from_utf16(&class[..n as usize])else{return BOOL(1);};
        let target=GameCaptureTarget {pid,creation_time:created,hwnd:hwnd.0 as usize as u64,executable,title,class};
        if target.validate().is_err(){return BOOL(1);}
        let mut rect=RECT::default();if unsafe{GetClientRect(hwnd,&mut rect)}.is_err(){return BOOL(1);}
        let (Ok(width),Ok(height))=(u32::try_from(rect.right-rect.left),u32::try_from(rect.bottom-rect.top))else{return BOOL(1);};
        if width==0||height==0||width>8192||height>8192{return BOOL(1);}
        let bytes=target.executable.len()+target.title.len()+target.class.len()+128;
        if context.items.len()>=256||context.bytes+bytes>256*1024{context.error=Some(GameError::Limit("game picker256 windows/256KiB metadata"));return BOOL(0);}
        context.bytes+=bytes;context.items.push(GameTargetInfo {target,width,height});BOOL(1)
    }
    let mut context=Context {items:Vec::new(),bytes:0,error:None};
    let result=unsafe{EnumWindows(Some(visit),LPARAM((&mut context as *mut Context) as isize))};
    if let Some(error)=context.error{return Err(error);}result.map_err(|e|GameError::Native(e.to_string()))?;
    context.items.sort_by(|a,b|a.target.executable.cmp(&b.target.executable).then(a.target.title.cmp(&b.target.title)).then(a.target.pid.cmp(&b.target.pid)));
    Ok(context.items)
}
