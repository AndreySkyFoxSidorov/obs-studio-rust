//! Window adapter over the shared genuine monitor/window WGC engine.
use crate::capture::{CaptureLimits,native_budget::NativeCopyBudget,wgc_engine::{Engine,Resource,NativeTarget,Outcome,Error}};
use skylive_core::WindowCaptureBinding;
use crate::window_capture::{WindowCaptureError,WindowCaptureOutcome,WindowFrameMetadata,WindowCaptureBackendKind,WindowFrameEncoding};
use windows::Win32::Foundation::HWND;
use windows::{core::Interface,Win32::Graphics::{Gdi::{MonitorFromWindow,MONITOR_DEFAULTTONEAREST},Dxgi::{CreateDXGIFactory1,DXGI_ERROR_NOT_FOUND,IDXGIFactory1,IDXGIOutput6,Common::{DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020}}}};
use std::collections::HashMap;
pub(super) struct WgcBackend {engine:Engine,outputs:HashMap<isize,IDXGIOutput6>}
impl WgcBackend {
    pub(super) fn new(budget:NativeCopyBudget)->Self {Self {engine:Engine::new(budget),outputs:HashMap::new()}}
    pub(super) fn set_sdr_white_nits(&mut self,nits:u16){self.engine.set_sdr_white_nits(nits);}
    pub(super) fn retain(&mut self,bindings:&[WindowCaptureBinding],reset:bool){if reset{self.outputs.clear();}let keys:Vec<_>=bindings.iter().flat_map(|b|[Resource::window_color(b,false),Resource::window_color(b,true)]).collect();self.engine.retain(&keys,reset);}
    pub(super) fn reap(&mut self)->bool{self.engine.reap()}
    fn hdr(&mut self,hwnd:HWND)->Result<bool,WindowCaptureError>{
        let monitor=unsafe{MonitorFromWindow(hwnd,MONITOR_DEFAULTTONEAREST)};
        if monitor.0.is_null(){return Err(WindowCaptureError::Unavailable("Cannot determine window monitor; enable Force SDR"));}
        let key=monitor.0 as isize;
        if let Some(output)=self.outputs.get(&key){
            if let Ok(desc)=unsafe{output.GetDesc1()} {if desc.AttachedToDesktop.as_bool() && desc.Monitor.0==monitor.0 {return classify(desc.ColorSpace);}}
            self.outputs.remove(&key);
        }
        let factory:IDXGIFactory1=unsafe{CreateDXGIFactory1()}.map_err(|e|WindowCaptureError::Backend(format!("CreateDXGIFactory1 (window color): {e}")))?;
        for a in 0..16 {let adapter=match unsafe{factory.EnumAdapters1(a)}{Ok(v)=>v,Err(e) if e.code()==DXGI_ERROR_NOT_FOUND=>break,Err(e)=>return Err(WindowCaptureError::Backend(format!("EnumAdapters1 (window color): {e}")))};
            for o in 0..64 {let output=match unsafe{adapter.EnumOutputs(o)}{Ok(v)=>v,Err(e) if e.code()==DXGI_ERROR_NOT_FOUND=>break,Err(e)=>return Err(WindowCaptureError::Backend(format!("EnumOutputs (window color): {e}")))};
                let desc=unsafe{output.GetDesc()}.map_err(|e|WindowCaptureError::Backend(format!("IDXGIOutput::GetDesc (window color): {e}")))?;
                if desc.AttachedToDesktop.as_bool() && desc.Monitor.0==monitor.0 {
                    let output:IDXGIOutput6=output.cast().map_err(|_|WindowCaptureError::Unavailable("Cannot determine window color space; enable Force SDR"))?;
                    let desc=unsafe{output.GetDesc1()}.map_err(|_|WindowCaptureError::Unavailable("Cannot determine window color space; enable Force SDR"))?;
                    let result=classify(desc.ColorSpace);if self.outputs.len()>=64{self.outputs.clear();}self.outputs.insert(key,output);return result;
                }
            }
        }
        Err(WindowCaptureError::Unavailable("Cannot determine window color space; enable Force SDR"))
    }
    pub(super) fn capture(&mut self,binding:&WindowCaptureBinding,hwnd:HWND,pid:u32,process_creation_time_100ns:Option<u64>,limits:CaptureLimits)->Result<WindowCaptureOutcome,WindowCaptureError>{
        let hdr=if binding.force_sdr(){false}else{self.hdr(hwnd)?};
        self.engine.capture(Resource::window_color(binding,hdr),NativeTarget::Window {handle:hwnd.0 as usize,pid,process_creation_time_100ns},limits).map(|outcome|match outcome {
            Outcome::Frame {image,generation}=>WindowCaptureOutcome::Frame {image,metadata:WindowFrameMetadata {backend:WindowCaptureBackendKind::Wgc,generation,encoding:WindowFrameEncoding::PremultipliedLinearSrgb,native_window:Some(crate::window_capture::NativeWindowIdentity{handle:hwnd.0 as usize,process_id:pid,process_creation_time_100ns:None})}},
            Outcome::Pending {generation}=>WindowCaptureOutcome::Pending {generation},
        }).map_err(|error|match error {Error::Unavailable(s)=>WindowCaptureError::Unavailable(s),Error::PermissionDenied=>WindowCaptureError::PermissionDenied,Error::TargetNotFound=>WindowCaptureError::TargetNotFound,Error::TargetMinimized=>WindowCaptureError::TargetMinimized,Error::InvalidFrame(s)=>WindowCaptureError::InvalidFrame(s),Error::BudgetExceeded(s)=>WindowCaptureError::BudgetExceeded(s),Error::Backend(s)=>WindowCaptureError::Backend(s),Error::DeviceLost(s)=>WindowCaptureError::DeviceLost(s)})
    }
}
fn classify(space:windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_TYPE)->Result<bool,WindowCaptureError>{
    if space==DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709{Ok(false)}else if space==DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020{Ok(true)}else{Err(WindowCaptureError::Unavailable("Unknown window monitor color space; enable Force SDR"))}
}
