use skylive_game_capture_protocol::{self as wire,Init,Header,field as f,state as phase,mapping::Mapping};
use std::{ffi::c_void,mem::{size_of,MaybeUninit,ManuallyDrop},sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,AtomicUsize,Ordering}},time::Duration};
use ::windows::{core::{Interface,PCWSTR},Win32::{Foundation::{BOOL,HWND,HMODULE,HINSTANCE,WPARAM,LPARAM,LRESULT},Graphics::{Direct3D::*,Direct3D11::*,Dxgi::{*,Common::*}},UI::WindowsAndMessaging::*,System::LibraryLoader::GetModuleHandleW}};
mod gpu;
use gpu::Gpu;

type Raw=*mut c_void;
#[link(name="kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess()->Raw;fn GetCurrentProcessId()->u32;
    fn ReadProcessMemory(process:Raw,address:*const c_void,buffer:Raw,bytes:usize,read:*mut usize)->i32;
    fn GetProcessTimes(process:Raw,creation:*mut u64,exit:*mut u64,kernel:*mut u64,user:*mut u64)->i32;
    fn OpenProcess(access:u32,inherit:i32,pid:u32)->Raw;fn CloseHandle(handle:Raw)->i32;
    fn WaitForSingleObject(handle:Raw,timeout:u32)->u32;
    fn QueryFullProcessImageNameW(process:Raw,flags:u32,text:*mut u16,len:*mut u32)->i32;
    fn GetModuleHandleExW(flags:u32,address:*const u16,module:*mut Raw)->i32;
    fn QueryPerformanceCounter(value:*mut i64)->i32;fn QueryPerformanceFrequency(value:*mut i64)->i32;
    fn VirtualProtect(address:Raw,size:usize,protect:u32,old:*mut u32)->i32;
}
static BUSY:AtomicBool=AtomicBool::new(false);
static WORKER:Mutex<Option<std::thread::JoinHandle<()>>>=Mutex::new(None);
static IN_CALLBACK:AtomicU64=AtomicU64::new(0);
static DROPPED:AtomicU64=AtomicU64::new(0);
static TARGET_HWND:AtomicU64=AtomicU64::new(0);
static ACTIVE:Mutex<Option<Capture>>=Mutex::new(None);
struct Entry {table:AtomicUsize,index:AtomicUsize,original:AtomicUsize}
impl Entry {const fn new()->Self{Self {table:AtomicUsize::new(0),index:AtomicUsize::new(0),original:AtomicUsize::new(0)}}}
static ENTRIES:[Entry;8]=[const {Entry::new()};8];
struct Capture {init:Init,map:Arc<Mapping>,frequency:u64,last_qpc:u64,gpu:Option<Gpu>,generation:u64}
// D3D11 device is free-threaded; immediate context is explicitly protected by
// ID3D11Multithread. All of our context/resource access also holds ACTIVE.
unsafe impl Send for Capture {}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SkyLiveGameHookStart(parameter:Raw)->u32 {
    let mut init=MaybeUninit::<Init>::uninit();let mut read=0;
    if parameter.is_null()||unsafe{ReadProcessMemory(GetCurrentProcess(),parameter,init.as_mut_ptr().cast(),size_of::<Init>(),&mut read)}==0||read!=size_of::<Init>() {return 1;}
    let init=unsafe{init.assume_init()};
    if !init.valid()||init.pid!=unsafe{GetCurrentProcessId()}{return 2;}
    let mut creation=0;let(mut exit,mut kernel,mut user)=(0,0,0);
    if unsafe{GetProcessTimes(GetCurrentProcess(),&mut creation,&mut exit,&mut kernel,&mut user)}==0||creation!=init.creation{return 3;}
    let mut executable=[0u16;4096];let mut count=4096;
    if unsafe{QueryFullProcessImageNameW(GetCurrentProcess(),0,executable.as_mut_ptr(),&mut count)}==0||count>=4096||executable!=init.executable{return 4;}
    let map=match Mapping::open(&init.mapping){Ok(map)=>Arc::new(map),Err(error)=>return 0x80070000|(error.raw_os_error().unwrap_or(-1) as u32&0xffff)};
    let h=map.header();
    if h.load(f::MAGIC)!=wire::MAGIC||h.load(f::ABI)!=wire::ABI||h.load(f::TOKEN0)!=init.token[0]||h.load(f::TOKEN1)!=init.token[1]
        ||h.load(f::PID)!=u64::from(init.pid)||h.load(f::CREATION)!=init.creation||h.load(f::HWND)!=init.hwnd||h.load(f::STOP)!=0{return 9;}
    let Ok(mut worker)=WORKER.lock()else{return 10;};
    if worker.as_ref().is_some_and(|thread|!thread.is_finished()){return 5;}
    if let Some(previous)=worker.take(){if previous.join().is_err(){return 10;}}
    if BUSY.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).is_err(){return 5;}
    DROPPED.store(0,Ordering::Release);TARGET_HWND.store(init.hwnd,Ordering::Release);
    // A callback may already have fetched our entry when unhook completes.
    // Pinning, rather than racing FreeLibrary, makes every late entry safe.
    let mut module=std::ptr::null_mut();
    if unsafe{GetModuleHandleExW(5,SkyLiveGameHookStart as *const () as *const u16,&mut module)}==0 {BUSY.store(false,Ordering::Release);return 6;}
    match std::thread::Builder::new().name("SkyLive D3D11 game hook".into()).stack_size(2*1024*1024).spawn(move||run(init,map)) {
        Ok(handle)=>{*worker=Some(handle);0},Err(_)=>{BUSY.store(false,Ordering::Release);7}
    }
}
fn run(init:Init,map:Arc<Mapping>) {
    let h=map.header();
    let result=(||->Result<(),i32>{
        let h=map.header();
        if h.load(f::MAGIC)!=wire::MAGIC||h.load(f::ABI)!=wire::ABI||h.load(f::TOKEN0)!=init.token[0]||h.load(f::TOKEN1)!=init.token[1]
            ||h.load(f::PID)!=u64::from(init.pid)||h.load(f::CREATION)!=init.creation||h.load(f::HWND)!=init.hwnd {return Err(-2);}
        let host=unsafe{OpenProcess(0x00101000,0,init.host_pid)};
        if host.is_null(){return Err(-3);}
        struct Host(Raw);impl Drop for Host{fn drop(&mut self){unsafe{CloseHandle(self.0);}}}
        let host=Host(host);let(mut created,mut exit,mut kernel,mut user)=(0,0,0,0);
        if unsafe{GetProcessTimes(host.0,&mut created,&mut exit,&mut kernel,&mut user)}==0||created!=init.host_creation{return Err(-4);}
        let mut frequency=0;if unsafe{QueryPerformanceFrequency(&mut frequency)}==0||frequency<=0{return Err(-5);}
        if h.load(f::STOP)!=0{return Ok(());}
        *ACTIVE.lock().map_err(|_|-6)?=Some(Capture {init,map:map.clone(),frequency:frequency as u64,last_qpc:0,gpu:None,generation:0});
        if let Err(code)=install(){h.store(f::ERROR,code as u32 as u64);h.store(f::STOP,1);}
        else{h.store(f::STATE,phase::HOOKED);}
        while h.load(f::STOP)==0&&unsafe{WaitForSingleObject(host.0,0)}==258 {
            let mut pid=0;unsafe{GetWindowThreadProcessId(HWND(init.hwnd as usize as Raw),Some(&mut pid));}
            if pid!=init.pid||!unsafe{IsWindow(HWND(init.hwnd as usize as Raw))}.as_bool(){h.store(f::ERROR,(-7i32) as u32 as u64);break;}
            h.store(f::DROPPED,DROPPED.load(Ordering::Acquire));
            std::thread::sleep(Duration::from_millis(10));
        }
        h.store(f::STOP,1);h.store(f::STATE,phase::STOPPING);if !restore(){h.store(f::ERROR,(-60i32) as u32 as u64);}
        // No deadline is called native quiescence. The protected device context
        // is retired off the game's Present thread, including on host death.
        loop {
            let mut guard=ACTIVE.lock().unwrap_or_else(|e|e.into_inner());
            let settled=guard.as_mut().is_none_or(|state|state.gpu.as_mut().is_none_or(Gpu::settled));
            if settled&&IN_CALLBACK.load(Ordering::Acquire)==0 {guard.take();break;}
            drop(guard);std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    })();
    if let Err(code)=result {
        h.store(f::ERROR,code as u32 as u64);h.store(f::STOP,1);
        let _=restore();if let Ok(mut state)=ACTIVE.lock(){if let Some(state)=state.as_mut(){state.map.header().store(f::ERROR,code as u32 as u64);state.map.header().store(f::STOP,1);}}
        // An initialization failure can still have an already-submitted copy.
        loop {let mut guard=ACTIVE.lock().unwrap_or_else(|e|e.into_inner());if guard.as_mut().is_none_or(|s|s.gpu.as_mut().is_none_or(Gpu::settled))&&IN_CALLBACK.load(Ordering::Acquire)==0 {
            guard.take();break;}
            drop(guard);std::thread::sleep(Duration::from_millis(2));}
    }
    BUSY.store(false,Ordering::Release);
    h.store(f::STATE,if h.load(f::ERROR)==0 {phase::RETIRED}else{phase::FAILED});
}
fn original(this:Raw,index:usize)->usize {
    let table=unsafe{*(this.cast::<usize>())};
    ENTRIES.iter().find(|entry|entry.table.load(Ordering::Acquire)==table&&entry.index.load(Ordering::Acquire)==index)
        .map_or(0,|entry|entry.original.load(Ordering::Acquire))
}
struct Callback;
impl Callback {fn enter()->Self{IN_CALLBACK.fetch_add(1,Ordering::AcqRel);Self}}
impl Drop for Callback {fn drop(&mut self){IN_CALLBACK.fetch_sub(1,Ordering::AcqRel);}}
unsafe extern "system" fn present(this:Raw,sync:u32,flags:u32)->i32 {
    let _entered=Callback::enter();let function=original(this,8);
    if flags&1==0 {let _=std::panic::catch_unwind(||capture(this));}
    if function==0 {return 0x80004005u32 as i32;}
    let function:unsafe extern "system" fn(Raw,u32,u32)->i32=unsafe{std::mem::transmute(function)};
    unsafe{function(this,sync,flags)}
}
unsafe extern "system" fn present1(this:Raw,sync:u32,flags:u32,parameters:*const c_void)->i32 {
    let _entered=Callback::enter();let function=original(this,22);
    if flags&1==0 {let _=std::panic::catch_unwind(||capture(this));}
    if function==0 {return 0x80004005u32 as i32;}
    let function:unsafe extern "system" fn(Raw,u32,u32,*const c_void)->i32=unsafe{std::mem::transmute(function)};
    unsafe{function(this,sync,flags,parameters)}
}
fn capture(this:Raw) {
    let swap=ManuallyDrop::new(unsafe{IDXGISwapChain::from_raw(this)});
    let description=match unsafe{swap.GetDesc()}{Ok(v)=>v,Err(_)=>return};
    if description.OutputWindow.0 as usize as u64!=TARGET_HWND.load(Ordering::Acquire){return;}
    let Ok(mut guard)=ACTIVE.try_lock() else{DROPPED.fetch_add(1,Ordering::AcqRel);return;};let Some(state)=guard.as_mut() else{return;};
    let h=state.map.header();if h.load(f::STOP)!=0{return;}
    h.add(f::CALLBACKS,1);let mut now=0;if unsafe{QueryPerformanceCounter(&mut now)}==0||now<0{return;}
    if state.last_qpc!=0&&(now as u64).saturating_sub(state.last_qpc)<state.frequency/u64::from(state.init.fps){h.add(f::THROTTLED,1);return;}
    match state.step(&swap,now as u64) {
        Ok(true)=>state.last_qpc=now as u64,Ok(false)=>{DROPPED.fetch_add(1,Ordering::AcqRel);},
        Err(code)=>{state.map.header().store(f::ERROR,code as u32 as u64);state.map.header().store(f::STOP,1);}
    }
}
impl Capture {
    fn step(&mut self,swap:&IDXGISwapChain,qpc:u64)->Result<bool,i32>{
        let input:ID3D11Texture2D=unsafe{swap.GetBuffer(0)}.map_err(|e|e.code().0)?;
        let mut desc=D3D11_TEXTURE2D_DESC::default();unsafe{input.GetDesc(&mut desc);}
        if !wire::dimensions(desc.Width,desc.Height)||!matches!(desc.Format,DXGI_FORMAT_R8G8B8A8_UNORM|DXGI_FORMAT_R8G8B8A8_UNORM_SRGB|DXGI_FORMAT_B8G8R8A8_UNORM|DXGI_FORMAT_B8G8R8A8_UNORM_SRGB){return Err(-20);}
        if self.gpu.as_ref().is_none_or(|gpu|!gpu.matches(&input,desc)) {
            if let Some(gpu)=self.gpu.as_mut(){if !gpu.can_resize(self.map.header()){return Ok(false);}}
            let device:ID3D11Device=unsafe{swap.GetDevice()}.map_err(|e|e.code().0)?;
            let next=self.generation.checked_add(1).ok_or(-21)?;
            if self.gpu.as_ref().is_some_and(|gpu|gpu.same_device(&device)) {
                self.gpu.as_mut().ok_or(-23)?.resize(desc,next)?;
            } else {
                if self.gpu.as_mut().is_some_and(|gpu|!gpu.settled()){return Ok(false);}
                self.gpu=None;
                self.gpu=Some(Gpu::new(device,desc,next).map_err(|e|e.code().0)?);
            }
            self.generation=next;
        }
        self.gpu.as_mut().ok_or(-22)?.copy(&input,self.map.header(),qpc)
    }
}
fn patch(table:usize,index:usize,function:usize)->Result<(),i32> {
    let existing=ENTRIES.iter().find(|e|e.table.load(Ordering::Acquire)==table&&e.index.load(Ordering::Acquire)==index);
    let entry=existing.or_else(||ENTRIES.iter().find(|e|e.table.load(Ordering::Acquire)==0)).ok_or(-30)?;
    let slot=(table+index*size_of::<usize>()) as *mut usize;
    let current=unsafe{slot.read()};if current==function{return Ok(());}
    if existing.is_some()&&entry.original.load(Ordering::Acquire)!=current{return Err(-31);}
    entry.original.store(current,Ordering::Release);entry.index.store(index,Ordering::Release);entry.table.store(table,Ordering::Release);
    let mut pinned=std::ptr::null_mut();if unsafe{GetModuleHandleExW(5,current as *const u16,&mut pinned)}==0{return Err(-32);}
    let mut protection=0;if unsafe{VirtualProtect(slot.cast(),size_of::<usize>(),4,&mut protection)}==0{return Err(-33);}
    let value=unsafe{&*(slot.cast::<AtomicUsize>())}.compare_exchange(current,function,Ordering::AcqRel,Ordering::Acquire);
    let mut ignored=0;unsafe{VirtualProtect(slot.cast(),size_of::<usize>(),protection,&mut ignored);}
    value.map(|_|()).map_err(|_|-34)
}
fn restore()->bool{let mut restored=true;for entry in &ENTRIES {
    let table=entry.table.load(Ordering::Acquire);if table==0{continue;}
    let index=entry.index.load(Ordering::Acquire);let function=if index==8 {present as *const () as usize}else{present1 as *const () as usize};
    let slot=(table+index*size_of::<usize>()) as *mut AtomicUsize;let mut protection=0;
    if unsafe{VirtualProtect(slot.cast(),size_of::<usize>(),4,&mut protection)}!=0 {
        let _=unsafe{&*slot}.compare_exchange(function,entry.original.load(Ordering::Acquire),Ordering::AcqRel,Ordering::Acquire);
        let mut ignored=0;if unsafe{VirtualProtect(slot.cast(),size_of::<usize>(),protection,&mut ignored)}==0{restored=false;}
    }else{restored=false;}
}restored}
fn install()->Result<(),i32> {
    let window=probe_window()?;
    let mut device=None;let mut context=None;let mut swap=None;
    let description=DXGI_SWAP_CHAIN_DESC {BufferDesc:DXGI_MODE_DESC {Width:2,Height:2,Format:DXGI_FORMAT_R8G8B8A8_UNORM,..Default::default()},SampleDesc:DXGI_SAMPLE_DESC {Count:1,Quality:0},BufferUsage:DXGI_USAGE_RENDER_TARGET_OUTPUT,BufferCount:2,OutputWindow:window.0,Windowed:BOOL(1),SwapEffect:DXGI_SWAP_EFFECT_DISCARD,..Default::default()};
    unsafe{D3D11CreateDeviceAndSwapChain(None,D3D_DRIVER_TYPE_HARDWARE,HMODULE::default(),D3D11_CREATE_DEVICE_BGRA_SUPPORT,None,D3D11_SDK_VERSION,Some(&description),Some(&mut swap),Some(&mut device),None,Some(&mut context))}.map_err(|e|e.code().0)?;
    let swap=swap.ok_or(-40)?;install_chain(&swap)?;drop(swap);
    let dxgi:IDXGIDevice=device.as_ref().ok_or(-41)?.cast().map_err(|e|e.code().0)?;
    let adapter=unsafe{dxgi.GetAdapter()}.map_err(|e|e.code().0)?;
    let factory:IDXGIFactory2=unsafe{adapter.GetParent()}.map_err(|e|e.code().0)?;
    let description=DXGI_SWAP_CHAIN_DESC1 {Width:2,Height:2,Format:DXGI_FORMAT_R8G8B8A8_UNORM,SampleDesc:DXGI_SAMPLE_DESC {Count:1,Quality:0},BufferUsage:DXGI_USAGE_RENDER_TARGET_OUTPUT,BufferCount:2,SwapEffect:DXGI_SWAP_EFFECT_FLIP_DISCARD,..Default::default()};
    let swap=unsafe{factory.CreateSwapChainForHwnd(device.as_ref().ok_or(-42)?,window.0,&description,None,None)}.map_err(|e|e.code().0)?;
    install_chain(&swap.cast().map_err(|e|e.code().0)?)?;Ok(())
}
fn install_chain(swap:&IDXGISwapChain)->Result<(),i32>{
    let table=unsafe{*(swap.as_raw().cast::<usize>())};patch(table,8,present as *const () as usize)?;
    if let Ok(swap1)=swap.cast::<IDXGISwapChain1>() {let table=unsafe{*(swap1.as_raw().cast::<usize>())};patch(table,8,present as *const () as usize)?;patch(table,22,present1 as *const () as usize)?;}
    Ok(())
}
struct ProbeWindow(HWND);
impl Drop for ProbeWindow {fn drop(&mut self){let _=unsafe{DestroyWindow(self.0)};}}
unsafe extern "system" fn probe_proc(hwnd:HWND,message:u32,wparam:WPARAM,lparam:LPARAM)->LRESULT {
    unsafe{DefWindowProcW(hwnd,message,wparam,lparam)}
}
fn probe_window()->Result<ProbeWindow,i32>{
    let name:Vec<u16>="SkyLive.Game.D3D11.Probe".encode_utf16().chain(Some(0)).collect();let module=unsafe{GetModuleHandleW(None)}.map_err(|e|e.code().0)?;
    let class=WNDCLASSW {lpfnWndProc:Some(probe_proc),hInstance:HINSTANCE(module.0),lpszClassName:PCWSTR(name.as_ptr()),..Default::default()};
    let registered=unsafe{RegisterClassW(&class)};
    if registered==0 {
        if ::windows::core::Error::from_win32().code().0!=0x80070582u32 as i32{return Err(-43);}
        let mut existing=WNDCLASSW::default();unsafe{GetClassInfoW(HINSTANCE(module.0),PCWSTR(name.as_ptr()),&mut existing)}.map_err(|e|e.code().0)?;
        if existing.lpfnWndProc.map(|function|function as usize)!=Some(probe_proc as *const () as usize)||existing.hInstance!=HINSTANCE(module.0){return Err(-44);}
    }
    let window=unsafe{CreateWindowExW(WINDOW_EX_STYLE::default(),PCWSTR(name.as_ptr()),PCWSTR(name.as_ptr()),WS_POPUP,0,0,2,2,None,None,HINSTANCE(module.0),None)}.map_err(|e|e.code().0)?;
    Ok(ProbeWindow(window))
}
