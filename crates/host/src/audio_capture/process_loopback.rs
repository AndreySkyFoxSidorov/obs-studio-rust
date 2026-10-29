//! Windows process-tree loopback. All client/process ownership stays on the
//! existing MTA audio actor. Completion only marshals one interface/result.
//! Reference: OBS 32.2.2 win-wasapi.cpp (GPL-2.0-or-later); independent Rust port.
use super::*;
use skylive_core::WindowsProcessIdentity;
use std::{mem::{ManuallyDrop, size_of}, sync::{Arc, Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}}, time::{Duration, Instant}};
use windows::{core::{implement, HRESULT, IUnknown, PWSTR}, Win32::{
    Foundation::{BOOL, CloseHandle, FILETIME, HANDLE, HMODULE, HWND, LPARAM, WAIT_TIMEOUT, FreeLibrary},
    Media::Audio::{AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
        IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
        IActivateAudioInterfaceCompletionHandler_Impl, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK},
    System::{Com::{IAgileObject, IAgileObject_Impl, IStream, Marshal::CoMarshalInterThreadInterfaceInStream,
        StructuredStorage::CoGetInterfaceAndReleaseStream},
        LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32},
        Threading::{OpenProcess, GetCurrentProcessId, GetProcessTimes, QueryFullProcessImageNameW,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_NAME_WIN32, WaitForSingleObject, CreateEventW}},
    UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, GetWindowTextLengthW, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible, IsWindow, GetWindow, GetWindowLongPtrW, GW_CHILD, GW_HWNDNEXT, GWL_STYLE, GWL_EXSTYLE, WS_CHILD, WS_EX_TOOLWINDOW},
}};

const UNSUPPORTED: &str = "Windows application audio requires Windows build 20348 or later";
const ACTIVATION_DEADLINE: Duration = Duration::from_secs(10);
// API signature resolved dynamically, so older Windows can load the application.
type Activate = unsafe extern "system" fn(PCWSTR, *const GUID, *const c_void, *mut c_void, *mut *mut c_void) -> HRESULT;
struct ActivationApi { module: usize, activate: Activate }
impl Drop for ActivationApi { fn drop(&mut self) { let _=unsafe {FreeLibrary(HMODULE(self.module as *mut c_void))}; } }
impl ActivationApi {
    fn load() -> Result<Arc<Self>,AudioCaptureError> {
        #[repr(C)] struct Version {size:u32, major:u32, minor:u32, build:u32, platform:u32, service:[u16;128]}
        let module=unsafe {GetModuleHandleW(windows::core::w!("ntdll.dll"))}.map_err(|e|native_error("GetModuleHandleW(ntdll)",e))?;
        let version_fn=unsafe {GetProcAddress(module,windows::core::s!("RtlGetVersion"))}.ok_or(AudioCaptureError::Unsupported(UNSUPPORTED))?;
        let version_fn:unsafe extern "system" fn(*mut Version)->i32=unsafe {std::mem::transmute(version_fn)};
        let mut version=Version {size:size_of::<Version>() as u32,major:0,minor:0,build:0,platform:0,service:[0;128]};
        if unsafe {version_fn(&mut version)}<0 || version.major<10 || version.build<20348 {return Err(AudioCaptureError::Unsupported(UNSUPPORTED));}
        let module=unsafe {LoadLibraryExW(windows::core::w!("mmdevapi.dll"),None,LOAD_LIBRARY_SEARCH_SYSTEM32)}.map_err(|e|native_error("LoadLibraryExW(mmdevapi)",e))?;
        let Some(symbol)=(unsafe {GetProcAddress(module,windows::core::s!("ActivateAudioInterfaceAsync"))}) else {
            let _=unsafe {FreeLibrary(module)}; return Err(AudioCaptureError::Unsupported(UNSUPPORTED));
        };
        Ok(Arc::new(Self {module:module.0 as usize,activate:unsafe {std::mem::transmute(symbol)}}))
    }
}
struct Handle(HANDLE);
impl Drop for Handle {fn drop(&mut self){let _=unsafe {CloseHandle(self.0)};}}
struct ProcessLease {handle:Handle, identity:WindowsProcessIdentity}
impl ProcessLease {
    fn acquire(identity:&WindowsProcessIdentity)->Result<Self,AudioCaptureError> {
        let lease=Self::for_pid(identity.pid())?;
        if &lease.identity!=identity {return Err(AudioCaptureError::DeviceUnavailable("The selected application restarted or its identity changed. Refresh and select it again.".into()));}
        Ok(lease)
    }
    fn for_pid(pid:u32)->Result<Self,AudioCaptureError> {
        if pid==0 || pid==unsafe {GetCurrentProcessId()} {return Err(AudioCaptureError::Invalid("select a different application process"));}
        let handle=Handle(unsafe {OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION|PROCESS_SYNCHRONIZE,false,pid)}.map_err(|e|native_error("OpenProcess(application audio)",e))?);
        let mut creation=FILETIME::default(); let mut exit=FILETIME::default();let mut kernel=FILETIME::default();let mut user=FILETIME::default();
        unsafe {GetProcessTimes(handle.0,&mut creation,&mut exit,&mut kernel,&mut user)}.map_err(|e|native_error("GetProcessTimes(application audio)",e))?;
        let mut executable=[0u16;4096];let mut length=executable.len() as u32;
        unsafe {QueryFullProcessImageNameW(handle.0,PROCESS_NAME_WIN32,PWSTR(executable.as_mut_ptr()),&mut length)}.map_err(|e|native_error("QueryFullProcessImageNameW(application audio)",e))?;
        let executable=String::from_utf16(&executable[..length as usize]).map_err(|_|AudioCaptureError::Invalid("application executable is not valid UTF-16"))?;
        let creation=(u64::from(creation.dwHighDateTime)<<32)|u64::from(creation.dwLowDateTime);
        let identity=WindowsProcessIdentity::new(pid,creation,executable).map_err(|_|AudioCaptureError::Invalid("invalid application process identity"))?;
        let result=Self {handle,identity};result.alive()?;Ok(result)
    }
    fn alive(&self)->Result<(),AudioCaptureError> {
        if unsafe {WaitForSingleObject(self.handle.0,0)}!=WAIT_TIMEOUT {
            return Err(AudioCaptureError::DeviceUnavailable("The selected application exited. Refresh and select the running application again.".into()));
        }
        Ok(())
    }
}
/// Visible application windows only. No endpoint or monitoring enumeration is used.
pub(super) fn enumerate(limits:AudioCaptureLimits)->Result<Vec<AudioDeviceInfo>,AudioCaptureError> {
    let _api=ActivationApi::load()?;
    struct Windows {entries:Vec<(u32,String)>, visited:usize, cap:usize}
    unsafe extern "system" fn visit(window:HWND,data:LPARAM)->BOOL {
        // EnumWindows invokes this synchronously on the owning MTA actor.
        let state=unsafe {&mut *(data.0 as *mut Windows)};state.visited+=1;
        if state.visited>4096 || state.entries.len()>=state.cap {return BOOL(0);}
        if !unsafe {IsWindowVisible(window)}.as_bool(){return BOOL(1);}
        let mut pid=0;unsafe {GetWindowThreadProcessId(window,Some(&mut pid))};
        if pid==0 || pid==unsafe {GetCurrentProcessId()} || state.entries.iter().any(|(id,_)|*id==pid) {return BOOL(1);}
        let mut title=[0u16;512];let length=unsafe {GetWindowTextW(window,&mut title)};
        if length<=0{return BOOL(1);}
        let title=String::from_utf16_lossy(&title[..length as usize]);
        state.entries.push((pid,title));BOOL(1)
    }
    let mut windows=Windows {entries:Vec::with_capacity(limits.max_devices),visited:0,cap:(limits.max_devices/2).max(1)};
    // A deliberate FALSE at the configured bound is normal, not an API failure.
    let outcome=unsafe {EnumWindows(Some(visit),LPARAM((&mut windows as *mut Windows) as isize))};
    if windows.visited<=4096 && windows.entries.len()<windows.cap {outcome.map_err(|e|native_error("EnumWindows(application audio)",e))?;}
    let mut result=Vec::with_capacity(windows.entries.len());
    for (pid,title) in windows.entries {
        // Protected, inaccessible or already exited applications cannot be chosen.
        let Ok(lease)=ProcessLease::for_pid(pid) else {continue};
        let executable=lease.identity.executable().rsplit(['\\','/']).next().unwrap_or(lease.identity.executable());
        let label=format!("{executable} — {title} (PID {pid})");
        let mut name=String::new();for ch in label.chars().filter(|ch|!ch.is_control()) {if name.len()+ch.len_utf8()>1024 {break;}name.push(ch);}
        result.push(AudioDeviceInfo {target:AudioTarget::WindowsProcess {identity:lease.identity.clone()},name,flow:AudioFlow::Output,is_default:false});
    }
    let mut legacy=result;let mut result:Vec<AudioDeviceInfo>=Vec::new();
    for w in obs_windows()? {
        if result.len()>=limits.max_devices {break;}
        let Some(executable)=w.executable.as_ref() else {continue};
        let Ok(selector)=skylive_core::WindowsAudioWindowSelector::new(w.title.clone(),w.class.clone(),executable.clone(),skylive_core::WindowsAudioWindowPriority::Class) else{continue};
        let target=AudioTarget::WindowsWindow{selector};if result.iter().any(|entry|entry.target==target){continue;}
        let label=format!("[{}] {}",executable,w.title);let mut name=String::new();for ch in label.chars().filter(|ch|!ch.is_control()){if name.len()+ch.len_utf8()>1024{break;}name.push(ch);}if name.trim().is_empty(){continue;}
        result.push(AudioDeviceInfo{target,name,flow:AudioFlow::Output,is_default:false});
    }
    for entry in legacy.drain(..){if result.len()>=limits.max_devices{break;}result.push(entry);}
    result.sort_by(|a,b|a.name.cmp(&b.name));Ok(result)
}

// A COM inter-thread marshal owns one IStream reference. Storing its address as
// an integer does not transfer a raw apartment-bound IAudioClient. Consumption
// always uses CoGetInterfaceAndReleaseStream on a COM-initialized thread.
struct Marshalled(usize);
impl Marshalled {
    fn take(mut self)->Result<IAudioClient,AudioCaptureError> {
        let raw=std::mem::replace(&mut self.0,0);
        let stream=ManuallyDrop::new(unsafe {IStream::from_raw(raw as *mut c_void)});
        unsafe {CoGetInterfaceAndReleaseStream::<_,IAudioClient>(&*stream)}.map_err(|e|native_error("CoGetInterfaceAndReleaseStream(IAudioClient)",e))
    }
}
impl Drop for Marshalled {
    fn drop(&mut self){if self.0!=0 {let raw=std::mem::replace(&mut self.0,0);let stream=ManuallyDrop::new(unsafe {IStream::from_raw(raw as *mut c_void)});let _:windows::core::Result<IAudioClient>=unsafe {CoGetInterfaceAndReleaseStream(&*stream)};}}
}
struct CompletionState {result:Mutex<Option<Result<Marshalled,AudioCaptureError>>>,ready:AtomicBool,retired:AtomicBool}
#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Completion {state:Arc<CompletionState>, admissions:Arc<AtomicUsize>, _api:Arc<ActivationApi>, _params:Arc<AUDIOCLIENT_ACTIVATION_PARAMS>}
impl IAgileObject_Impl for Completion_Impl {}
impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(&self,operation:Option<&IActivateAudioInterfaceAsyncOperation>)->windows::core::Result<()> {
        let outcome=(|| {
            let _mta=ComApartment::new()?;
            let operation=operation.ok_or(AudioCaptureError::Invalid("missing audio activation operation"))?;
            let mut status=HRESULT(0);let mut unknown:Option<IUnknown>=None;
            unsafe {operation.GetActivateResult(&mut status,&mut unknown)}.map_err(|e|native_error("GetActivateResult",e))?;
            status.ok().map_err(|e|native_error("ActivateAudioInterfaceAsync(process)",e))?;
            let unknown=unknown.ok_or(AudioCaptureError::Invalid("audio activation returned a null interface"))?;
            let stream=unsafe {CoMarshalInterThreadInterfaceInStream(&IAudioClient::IID,&unknown)}.map_err(|e|native_error("CoMarshalInterThreadInterfaceInStream(IAudioClient)",e))?;
            Ok(Marshalled(stream.into_raw() as usize))
        })();
        *self.state.result.lock().unwrap_or_else(|poison|poison.into_inner())=Some(outcome);
        self.state.ready.store(true,Ordering::Release);
        Ok(())
    }
}
impl Drop for Completion {
    fn drop(&mut self){self.state.retired.store(true,Ordering::Release);self.admissions.fetch_sub(1,Ordering::AcqRel);}
}
struct Pending {
    operation:Option<IActivateAudioInterfaceAsyncOperation>,handler:Option<IActivateAudioInterfaceCompletionHandler>,
    state:Arc<CompletionState>,started:Instant,
    // Keep the activation BLOB storage stable until the callback retires.
    _params:Arc<AUDIOCLIENT_ACTIVATION_PARAMS>,
}
impl Pending {
    fn release_owner_refs(&mut self){self.operation.take();self.handler.take();}
    fn take_result(&self)->Option<Result<Marshalled,AudioCaptureError>> {self.state.result.lock().unwrap_or_else(|poison|poison.into_inner()).take()}
}
struct ProcessStream {
    active:Option<WasapiStream>,pending:Option<Pending>,event:Option<Handle>,process:ProcessLease,window:Option<WindowLease>,
    description:Arc<str>,apartment:Rc<ComApartment>,limits:AudioCaptureLimits,frequency:u64,
    closing:bool,_api:Arc<ActivationApi>,
}
pub(super) fn open(identity:&WindowsProcessIdentity,limits:AudioCaptureLimits,frequency:u64,apartment:Rc<ComApartment>,admissions:Arc<AtomicUsize>)->Result<Box<dyn NativeAudioStream>,AudioCaptureError> {
    let process=ProcessLease::acquire(identity)?;
    open_leased(process,None,limits,frequency,apartment,admissions)
}
fn open_leased(process:ProcessLease,window:Option<WindowLease>,limits:AudioCaptureLimits,frequency:u64,apartment:Rc<ComApartment>,admissions:Arc<AtomicUsize>)->Result<Box<dyn NativeAudioStream>,AudioCaptureError>{
    let api=ActivationApi::load()?;let identity=&process.identity;
    let cap=limits.callback_slots.min(8);
    admissions.fetch_update(Ordering::AcqRel,Ordering::Acquire,|count|(count<cap).then_some(count+1)).map_err(|_|AudioCaptureError::Backpressure("process audio activation callbacks"))?;
    let state=Arc::new(CompletionState {result:Mutex::new(None),ready:AtomicBool::new(false),retired:AtomicBool::new(false)});
    let params=Arc::new(AUDIOCLIENT_ACTIVATION_PARAMS {ActivationType:AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous:AUDIOCLIENT_ACTIVATION_PARAMS_0 {ProcessLoopbackParams:AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {TargetProcessId:identity.pid(),ProcessLoopbackMode:PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE}}});
    let handler:IActivateAudioInterfaceCompletionHandler=Completion {state:state.clone(),admissions,_api:api.clone(),_params:params.clone()}.into();
    let mut raw=*PROPVARIANT::new().as_raw();
    unsafe {raw.Anonymous.Anonymous.vt=65;raw.Anonymous.Anonymous.Anonymous.blob.cbSize=size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
        raw.Anonymous.Anonymous.Anonymous.blob.pBlobData=(Arc::as_ptr(&params) as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast();}
    // VT_BLOB references our Arc; PropVariantClear must not free Rust storage.
    let variant=ManuallyDrop::new(unsafe {PROPVARIANT::from_raw(raw)});let mut operation=ptr::null_mut();
    let status=unsafe {(api.activate)(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,&IAudioClient::IID,(&*variant as *const PROPVARIANT).cast(),handler.as_raw(),&mut operation)};
    if let Err(error)=status.ok() {
        if !operation.is_null() {drop(unsafe {IActivateAudioInterfaceAsyncOperation::from_raw(operation)});}
        return Err(native_error("ActivateAudioInterfaceAsync(process)",error));
    }
    if operation.is_null(){return Err(AudioCaptureError::Invalid("audio activation returned a null operation"));}
    let operation=unsafe {IActivateAudioInterfaceAsyncOperation::from_raw(operation)};
    let description=Arc::from(format!("{} (PID {})",identity.executable().rsplit(['\\','/']).next().unwrap_or(identity.executable()),identity.pid()));
    Ok(Box::new(ProcessStream {active:None,pending:Some(Pending {operation:Some(operation),handler:Some(handler),state,started:Instant::now(),_params:params}),event:None,process,window,description,apartment,limits,frequency,closing:false,_api:api}))
}
impl ProcessStream {
    fn activate_client(&mut self,client:IAudioClient)->Result<(),AudioCaptureError> {
        self.process.alive()?;
        // GetMixFormat is E_NOTIMPL for process-loopback clients. Request this
        // exact accepted format; AudioProgram performs actual downstream conversion.
        let header=WAVEFORMATEX {wFormatTag:WAVE_EXTENSIBLE,nChannels:2,nSamplesPerSec:48000,nAvgBytesPerSec:384000,nBlockAlign:8,wBitsPerSample:32,cbSize:EXTENSION_BYTES};
        let mut format=WAVEFORMATEXTENSIBLE::default();format.Format=header;format.dwChannelMask=3;format.SubFormat=SUBTYPE_FLOAT;
        unsafe {format.Samples.wValidBitsPerSample=32;}
        let mix=parse_format(header,Some(format),self.limits)?;
        let event=Handle(unsafe {CreateEventW(None,false,false,None)}.map_err(|e|native_error("CreateEventW(process audio)",e))?);
        unsafe {client.Initialize(AUDCLNT_SHAREMODE_SHARED,AUDCLNT_STREAMFLAGS_LOOPBACK|AUDCLNT_STREAMFLAGS_EVENTCALLBACK,CLIENT_DURATION_100NS,0,&format.Format,None)}.map_err(|e|native_error("IAudioClient::Initialize(process 48k stereo float)",e))?;
        unsafe {client.SetEventHandle(event.0)}.map_err(|e|native_error("IAudioClient::SetEventHandle(process)",e))?;
        let frames=unsafe {client.GetBufferSize()}.map_err(|e|native_error("IAudioClient::GetBufferSize(process)",e))? as usize;
        if frames==0 || frames.checked_mul(mix.block_align).is_none_or(|bytes|bytes>self.limits.max_native_buffer_bytes){return Err(AudioCaptureError::Limit("WASAPI process native buffer bytes"));}
        let capture=unsafe {client.GetService::<IAudioCaptureClient>()}.map_err(|e|native_error("IAudioClient::GetService(process capture)",e))?;
        let mut active=WasapiStream {description:Some(self.description.clone()),capture,client,default_watch:None,_apartment:self.apartment.clone(),mix,limits:self.limits,frequency:self.frequency,started:false};
        self.process.alive()?;
        unsafe {active.client.Start()}.map_err(|e|native_error("IAudioClient::Start(process)",e))?;active.started=true;
        self.event=Some(event);self.active=Some(active);Ok(())
    }
}
impl NativeAudioStream for ProcessStream {
    fn description(&self)->Option<Arc<str>>{Some(self.description.clone())}
    fn poll(&mut self)->Result<Option<NativePcmBlock>,AudioCaptureError> {
        if self.closing {return Err(AudioCaptureError::Closed);}
        self.process.alive()?;
        if let Some(window)=&self.window {window.alive()?;}
        let result=if let Some(pending)=&mut self.pending {
            if pending.state.ready.load(Ordering::Acquire) {pending.release_owner_refs();pending.take_result()}
            else if pending.started.elapsed()>ACTIVATION_DEADLINE {return Err(AudioCaptureError::DeviceUnavailable("Application audio activation timed out; pending native completion is still retiring.".into()));}
            else {None}
        } else {None};
        if let Some(result)=result {self.activate_client(result?.take()?)?;}
        if self.pending.as_ref().is_some_and(|p|p.state.retired.load(Ordering::Acquire)) {self.pending.take();}
        match &mut self.active {Some(active)=>active.poll(),None=>Ok(None)}
    }
    fn close(&mut self)->Result<(),AudioCaptureError> {
        self.closing=true;
        if let Some(active)=&mut self.active {active.close()?;}
        self.active.take();self.event.take();
        if let Some(pending)=&mut self.pending {
            pending.release_owner_refs();drop(pending.take_result());
            if !pending.state.retired.load(Ordering::Acquire) {return Err(AudioCaptureError::Busy);}
        }
        self.pending.take();Ok(())
    }
}
// Normal ownership retirement calls close repeatedly on the background actor;
// no native join, callback wait, or COM interface destruction is run on the GUI.

// The selected HWND is ephemeral. A process lease also protects PID reuse; if
// either dies or the window is reassigned the actor closes and retries open.
struct WindowLease { hwnd:HWND, pid:u32 }
impl WindowLease {
    fn alive(&self)->Result<(),AudioCaptureError>{
        let mut pid=0;unsafe{GetWindowThreadProcessId(self.hwnd,Some(&mut pid));}
        if !unsafe{IsWindow(self.hwnd)}.as_bool()||pid!=self.pid{return Err(AudioCaptureError::DeviceUnavailable("Application window disappeared or changed process".into()));}Ok(())
    }
}
struct ObsWindow {hwnd:HWND,pid:u32,title:String,class:String,executable:Option<String>,identity:Option<WindowsProcessIdentity>}
fn class_name(hwnd:HWND)->String{let mut value=[0u16;256];let n=unsafe{GetClassNameW(hwnd,&mut value)};String::from_utf16_lossy(&value[..n.max(0) as usize])}
fn actual_window(parent:HWND)->HWND{
    let class=class_name(parent);
    if class!="ApplicationFrameWindow"&&class!="WinUIDesktopWin32WindowClass"{return parent;}
    let mut parent_pid=0;unsafe{GetWindowThreadProcessId(parent,Some(&mut parent_pid));}
    let mut child=unsafe{GetWindow(parent,GW_CHILD)}.ok();
    for _ in 0..4096 {let Some(hwnd)=child else{break};let mut pid=0;unsafe{GetWindowThreadProcessId(hwnd,Some(&mut pid));}if pid!=0&&pid!=parent_pid{return hwnd;}child=unsafe{GetWindow(hwnd,GW_HWNDNEXT)}.ok();}parent
}
fn obs_windows()->Result<Vec<ObsWindow>,AudioCaptureError>{
    struct State{windows:Vec<ObsWindow>,visited:usize,bytes:usize,overflow:bool}
    unsafe extern "system" fn visit(parent:HWND,param:LPARAM)->BOOL{
        let state=unsafe{&mut *(param.0 as *mut State)};state.visited+=1;
        if state.visited>4096 {state.overflow=true;return BOOL(0);}
        if !unsafe{IsWindowVisible(parent)}.as_bool(){return BOOL(1);}
        let styles=unsafe{GetWindowLongPtrW(parent,GWL_STYLE)} as u32;
        let extra=unsafe{GetWindowLongPtrW(parent,GWL_EXSTYLE)} as u32;
        if styles&WS_CHILD.0!=0||extra&WS_EX_TOOLWINDOW.0!=0{return BOOL(1);}
        let hwnd=actual_window(parent);let mut pid=0;unsafe{GetWindowThreadProcessId(hwnd,Some(&mut pid));}
        if pid!=0 && pid==unsafe{GetCurrentProcessId()}{return BOOL(1);}
        let length=unsafe{GetWindowTextLengthW(hwnd)}.max(0) as usize;
        if length>32768 {state.overflow=true;return BOOL(0);}
        let mut text=vec![0u16;length+1];let n=unsafe{GetWindowTextW(hwnd,&mut text)}.max(0) as usize;
        let title=String::from_utf16_lossy(&text[..n]);let class=class_name(hwnd);
        // Preserve an inaccessible window as unknown for conservative ranking;
        // never fabricate an executable or silently skip a possible winner.
        let identity=if pid==0 {None}else{ProcessLease::for_pid(pid).ok().map(|lease|lease.identity.clone())};
        let executable=identity.as_ref().and_then(|value|value.executable().rsplit(['\\','/']).next()).filter(|value|!value.is_empty()).map(str::to_owned);
        let bytes=title.len().saturating_add(class.len()).saturating_add(executable.as_ref().map_or(0,String::len)).saturating_add(identity.as_ref().map_or(0,|value|value.executable().len()));
        if state.bytes.checked_add(bytes).is_none_or(|sum|sum>4*1024*1024){state.overflow=true;return BOOL(0);}
        state.bytes+=bytes;state.windows.push(ObsWindow{hwnd,pid,title,class,executable,identity});BOOL(1)
    }
    let mut state=State{windows:Vec::new(),visited:0,bytes:0,overflow:false};
    let result=unsafe{EnumWindows(Some(visit),LPARAM((&mut state as *mut State) as isize))};
    if state.overflow{return Err(AudioCaptureError::Limit("application window enumeration"));}
    result.map_err(|error|native_error("EnumWindows(application audio selector)",error))?;Ok(state.windows)
}
pub(super) fn open_window(selector:&skylive_core::WindowsAudioWindowSelector,limits:AudioCaptureLimits,frequency:u64,apartment:Rc<ComApartment>,admissions:Arc<AtomicUsize>)->Result<Box<dyn NativeAudioStream>,AudioCaptureError>{
    let windows=obs_windows()?;
    let found=match super::obs_window_rating::best(selector,windows.iter().enumerate().map(|(index,w)|super::obs_window_rating::Candidate{handle:index,title:&w.title,class:&w.class,executable:w.executable.as_deref()})) {
        super::obs_window_rating::Best::Found(index)=>index,
        super::obs_window_rating::Best::Unavailable=>return Err(AudioCaptureError::DeviceUnavailable("A potentially matching application window has an inaccessible process; refusing to select another process".into())),
        super::obs_window_rating::Best::None=>return Err(AudioCaptureError::DeviceUnavailable("No matching application window".into())),
    };
    let w=&windows[found];let window=WindowLease{hwnd:w.hwnd,pid:w.pid};window.alive()?;
    let identity=w.identity.as_ref().ok_or_else(||AudioCaptureError::DeviceUnavailable("Selected application process identity is unavailable".into()))?;
    let process=ProcessLease::acquire(identity)?;window.alive()?;
    open_leased(process,Some(window),limits,frequency,apartment,admissions)
}

/// Never re-enumerate or substitute another matching window here. Recheck the
/// observation before and after opening the process, then keep both leases
/// alive for the stream. The caller is responsible for fencing stale PCM when
/// the video owner's session, capture generation or observation changes.
pub(super) fn open_observed_window(observed:crate::window_capture::NativeWindowIdentity,limits:AudioCaptureLimits,frequency:u64,apartment:Rc<ComApartment>,admissions:Arc<AtomicUsize>)->Result<Box<dyn NativeAudioStream>,AudioCaptureError>{
    if observed.handle==0 || observed.process_id==0 {return Err(AudioCaptureError::Invalid("missing observed application window"));}
    let window=WindowLease{hwnd:HWND(observed.handle as *mut c_void),pid:observed.process_id};
    window.alive()?;
    let process=ProcessLease::for_pid(observed.process_id)?;
    if observed.process_creation_time_100ns!=Some(process.identity.creation_time_100ns()) {
        return Err(AudioCaptureError::DeviceUnavailable("The observed window process exited or its PID was reused".into()));
    }
    window.alive()?;
    open_leased(process,Some(window),limits,frequency,apartment,admissions)
}
