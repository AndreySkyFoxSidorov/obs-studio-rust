//! Window adapter over the shared genuine monitor/window WGC engine.
use crate::capture::{CaptureLimits,native_budget::NativeCopyBudget,wgc_engine::{Engine,Resource,NativeTarget,Outcome,Error}};
use skylive_core::WindowCaptureBinding;
use crate::window_capture::{WindowCaptureError,WindowCaptureOutcome,WindowFrameMetadata,WindowCaptureBackendKind,WindowFrameEncoding};
use windows::Win32::Foundation::HWND;
pub(super) struct WgcBackend {engine:Engine}
impl WgcBackend {
    pub(super) fn new(budget:NativeCopyBudget)->Self {Self {engine:Engine::new(budget)}}
    pub(super) fn retain(&mut self,bindings:&[WindowCaptureBinding],reset:bool){self.engine.retain(&bindings.iter().map(Resource::window).collect::<Vec<_>>(),reset);}
    pub(super) fn reap(&mut self)->bool{self.engine.reap()}
    pub(super) fn capture(&mut self,binding:&WindowCaptureBinding,hwnd:HWND,pid:u32,process_creation_time_100ns:Option<u64>,limits:CaptureLimits)->Result<WindowCaptureOutcome,WindowCaptureError>{
        self.engine.capture(Resource::window(binding),NativeTarget::Window {handle:hwnd.0 as usize,pid,process_creation_time_100ns},limits).map(|outcome|match outcome {
            Outcome::Frame {image,generation}=>WindowCaptureOutcome::Frame {image,metadata:WindowFrameMetadata {backend:WindowCaptureBackendKind::Wgc,generation,encoding:WindowFrameEncoding::PremultipliedLinearSrgb,native_window:Some(crate::window_capture::NativeWindowIdentity{handle:hwnd.0 as usize,process_id:pid,process_creation_time_100ns:None})}},
            Outcome::Pending {generation}=>WindowCaptureOutcome::Pending {generation},
        }).map_err(|error|match error {Error::Unavailable(s)=>WindowCaptureError::Unavailable(s),Error::PermissionDenied=>WindowCaptureError::PermissionDenied,Error::TargetNotFound=>WindowCaptureError::TargetNotFound,Error::TargetMinimized=>WindowCaptureError::TargetMinimized,Error::InvalidFrame(s)=>WindowCaptureError::InvalidFrame(s),Error::BudgetExceeded(s)=>WindowCaptureError::BudgetExceeded(s),Error::Backend(s)=>WindowCaptureError::Backend(s),Error::DeviceLost(s)=>WindowCaptureError::DeviceLost(s)})
    }
}
