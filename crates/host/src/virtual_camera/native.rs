//! Worker-affine MF virtual-device lifetime and actual Program frame producer.
use super::*;
use skylive_virtual_camera_protocol::{ids,installation,transport::{Mapping,Telemetry,wide,path_text}};
use windows::{core::{GUID,HRESULT,Interface,PCWSTR},Win32::{Media::MediaFoundation::*,System::Com::{CoInitializeEx,CoUninitialize,COINIT_MULTITHREADED}}};
use std::{ffi::c_void,path::PathBuf};
type Create=unsafe extern "system" fn(MFVirtualCameraType,MFVirtualCameraLifetime,MFVirtualCameraAccess,PCWSTR,PCWSTR,*const GUID,u32,*mut *mut c_void)->HRESULT;
#[link(name="kernel32")]
unsafe extern "system" {fn LoadLibraryExW(path:*const u16,file:*mut c_void,flags:u32)->*mut c_void;fn GetProcAddress(module:*mut c_void,name:*const u8)->*mut c_void;fn FreeLibrary(module:*mut c_void)->i32;}
struct Library(*mut c_void);
impl Library {
    fn load()->Result<(Self,Create),Error>{
        let name=wide("mfsensorgroup.dll");let module=unsafe{LoadLibraryExW(name.as_ptr(),std::ptr::null_mut(),0x800)};
        if module.is_null(){return Err(Error::Unsupported("Windows11 MF virtual-camera DLL unavailable"));}
        let library=Self(module);let create=unsafe{GetProcAddress(module,b"MFCreateVirtualCamera\0".as_ptr())};
        if create.is_null(){return Err(Error::Unsupported("Windows11 build22000 MFCreateVirtualCamera export required"));}
        Ok((library,unsafe{std::mem::transmute::<*mut c_void,Create>(create)}))
    }
}
impl Drop for Library{fn drop(&mut self){unsafe{FreeLibrary(self.0);}}}
struct Apartment;
impl Apartment{fn new()->Result<Self,Error>{unsafe{CoInitializeEx(None,COINIT_MULTITHREADED)}.ok().map_err(native)?;
    if let Err(error)=unsafe{MFStartup(MF_VERSION,MFSTARTUP_FULL)} {unsafe{CoUninitialize();}return Err(native(error));}Ok(Self)}}
impl Drop for Apartment{fn drop(&mut self){let _=unsafe{MFShutdown()};unsafe{CoUninitialize();}}}
fn native(error:windows::core::Error)->Error{Error::Native(format!("HRESULT {:#x}: {}",error.code().0,error))}
fn transport(error:std::io::Error)->Error {if error.kind()==std::io::ErrorKind::WouldBlock{Error::Backpressure}else{Error::Native(error.to_string())}}
fn installation()->Result<PathBuf,Error>{
    let path=installation::registered_module().map_err(|error|Error::NotInstalled(format!("Install the Rust virtual camera source explicitly: {error}")))?;
    let path=std::fs::canonicalize(path).map_err(|error|Error::NotInstalled(error.to_string()))?;
    let directory=std::fs::canonicalize(installation::protected_directory().map_err(|error|Error::NotInstalled(error.to_string()))?).map_err(|error|Error::NotInstalled(error.to_string()))?;
    if !path.starts_with(directory) || path.file_name()!=Some(std::ffi::OsStr::new(skylive_virtual_camera_protocol::DLL_NAME)){return Err(Error::NotInstalled("COM source is not installed in protected Program Files".into()));}
    Ok(path)
}
pub(super) struct Active {
    camera:Option<IMFVirtualCamera>,probe:Option<IMFMediaSource>,mapping:Mapping,
    config:Config,renderer:crate::preview::OutputRenderer,gpu:RenderGpu,resource_generation:Option<u64>,last_grid:Option<u64>,missing_since:Option<std::time::Instant>,
    _services:crate::preview::RenderServiceOwner,_apartment:Apartment,_library:Library,_credit:ServiceCredit,
    _thread_only:std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Active {
    pub(super) fn start(config:Config,gpu:RenderGpu)->Result<Self,Error>{
        let credit=ServiceCredit::acquire()?;
        // Resolve the Windows11-only entry dynamically so older Windows can
        // load the application and receive Unsupported instead of loader failure.
        let (library,create)=Library::load()?;let _installed=installation()?;let apartment=Apartment::new()?;
        // Reserve rendering services before publishing the MF camera. If this
        // can fail after Start, returning early would drop COM interfaces without
        // explicitly stopping and shutting down the already registered device.
        let services=crate::preview::retain_render_services().map_err(|error|Error::Native(error.to_string()))?;
        let mapping=Mapping::create(config.profile).map_err(transport)?;let info=mapping.info();
        let name=wide("SkyLive");let class=wide(skylive_virtual_camera_protocol::CLASS_ID);let mut pointer=std::ptr::null_mut();
        unsafe{create(MFVirtualCameraType_SoftwareCameraSource,MFVirtualCameraLifetime_Session,MFVirtualCameraAccess_CurrentUser,
            PCWSTR(name.as_ptr()),PCWSTR(class.as_ptr()),std::ptr::null(),0,&mut pointer)}.ok().map_err(native)?;
        if pointer.is_null(){return Err(Error::Native("MFCreateVirtualCamera returned no object".into()));}
        let camera=unsafe{IMFVirtualCamera::from_raw(pointer)};
        let file=wide(&path_text(&info.file).map_err(transport)?);let mutex=wide(&info.mutex);
        let configure=(||->windows::core::Result<()> {unsafe{
            camera.SetString(&ids::FILE,PCWSTR(file.as_ptr()))?;camera.SetString(&ids::MUTEX,PCWSTR(mutex.as_ptr()))?;camera.SetGUID(&ids::TOKEN,&info.token)?;
            camera.SetUINT32(&ids::WIDTH,config.width())?;camera.SetUINT32(&ids::HEIGHT,config.height())?;camera.SetUINT32(&ids::FPS,config.fps())?;
            camera.Start(None)
        }})();
        if let Err(error)=configure {let _=unsafe{camera.Shutdown()};return Err(native(error));}
        // This shared-client source forces actual COM media-source activation
        // and descriptor negotiation. It does not pretend a consumer started.
        let probe=match unsafe{camera.GetMediaSource()} {Ok(value)=>value,Err(error)=>{let _=unsafe{camera.Stop()};let _=unsafe{camera.Shutdown()};return Err(native(error));}};
        if let Err(error)=unsafe{probe.CreatePresentationDescriptor()} {let _=unsafe{probe.Shutdown()};let _=unsafe{camera.Stop()};let _=unsafe{camera.Shutdown()};return Err(native(error));}
        Ok(Self {camera:Some(camera),probe:Some(probe),mapping,renderer:gpu.output_renderer(),gpu,config,resource_generation:None,last_grid:None,missing_since:None,
            _services:services,_apartment:apartment,_library:library,_credit:credit,_thread_only:std::marker::PhantomData})
    }
    pub(super) fn video(&mut self,frame:ProgramVideoFrame,identity:u64,state:&Signals)->Result<(),Error>{
        if frame.session!=self.config.session || frame.playback.session()!=self.config.session || frame.capture.session()!=self.config.session
            || frame.playback.revision()!=frame.model.revision(){return Err(Error::Invalid("virtual camera frame scope"));}
        if state.stop.load(Ordering::Acquire) || state.close.load(Ordering::Acquire){return Ok(());}
        if self.resource_generation!=Some(frame.resource_generation){self.renderer=self.gpu.output_renderer();self.last_grid=None;self.resource_generation=Some(frame.resource_generation);}
        let grid=(u128::from(frame.time.0)*u128::from(self.config.fps())/1_000_000_000) as u64;
        if self.last_grid.is_some_and(|old|grid<=old){return Ok(());}
        // Derived source/scene layouts are available before pixels so they
        // can activate video dependencies. Readiness is enforced here, after
        // capture synchronization, rather than preventing source activation.
        if self.config.route()!=skylive_core::VirtualCameraRoute::Program {
            for view in frame.output.scenes(){for source in view.visible_sources(&frame.model).map_err(|e|Error::RouteUnavailable(e.to_string()))? {
                if matches!(source.kind(),skylive_core::SourceKind::Browser {..}) && frame.capture.frame(source.id()).is_none_or(|raw|!raw.binding.matches_source(source.kind())) {
                    if let Some(problem)=frame.capture.problem(source.id()).filter(|p|p.code!="browser_frame_pending") {return Err(Error::RouteUnavailable(problem.detail.clone()));}
                    let since=self.missing_since.get_or_insert_with(std::time::Instant::now);if since.elapsed()<std::time::Duration::from_secs(30){return Err(Error::RouteWaiting("Waiting for the first Browser Source paint".into()));}
                    return Err(Error::RouteUnavailable("Browser Source has no actual frame after30 seconds".into()));
                }
            }}
        }
        let rendered=self.renderer.render_profile_output_fitted(&frame.model,&frame.output,self.config.width(),self.config.height(),&frame.playback,&frame.capture)
            .map_err(|error|Error::Native(format!("Virtual-camera route render: {error}")))?;
        if self.config.route()!=skylive_core::VirtualCameraRoute::Program && !rendered.diagnostics.is_empty() {
            let detail=rendered.diagnostics.iter().map(|issue|format!("{}: {}",issue.code,issue.detail)).collect::<Vec<_>>().join("; ");
            let pending=rendered.diagnostics.iter().all(|issue|matches!(issue.code,"capture_frame_unavailable"|"window_capture_frame_pending"|"browser_frame_pending"));
            let since=self.missing_since.get_or_insert_with(std::time::Instant::now);
            if pending&&since.elapsed()<std::time::Duration::from_secs(5){return Err(Error::RouteWaiting(detail));}
            return Err(Error::RouteUnavailable(detail));
        }
        self.missing_since=None;
        let nv12=skylive_media::recording_nv12(&rendered.image,skylive_virtual_camera_protocol::MAX_NV12_BYTES,18_000_000)
            .map_err(|error|Error::Native(format!("Virtual-camera route NV12: {error}")))?;
        if state.latest.load(Ordering::Acquire)!=identity || state.stop.load(Ordering::Acquire) || state.close.load(Ordering::Acquire){return Ok(());}
        self.mapping.publish(&nv12,unsafe{MFGetSystemTime()}).map_err(transport)?;self.last_grid=Some(grid);Ok(())
    }
    pub(super) fn telemetry(&self)->Result<Telemetry,Error>{self.mapping.telemetry().map_err(transport)}
    pub(super) fn stop(&mut self)->Result<(),Error>{
        let mut failure=None;
        if let Some(camera)=self.camera.take(){
            if let Err(error)=unsafe{camera.Stop()}{if failure.is_none(){failure=Some(native(error));}}
            if let Some(probe)=self.probe.take(){if let Err(error)=unsafe{probe.Shutdown()}{if failure.is_none(){failure=Some(native(error));}}}
            if let Err(error)=unsafe{camera.Shutdown()}{if failure.is_none(){failure=Some(native(error));}}
        }
        if let Err(error)=self.mapping.stop(){if failure.is_none(){failure=Some(transport(error));}}
        failure.map_or(Ok(()),Err)
    }
}
impl Drop for Active{fn drop(&mut self){let _=self.stop();}}
