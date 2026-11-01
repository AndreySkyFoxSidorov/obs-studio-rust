//! Current Windows output colour-space selection for display capture.
//! The cached COM interface is owned by the capture worker; GetDesc1 is read
//! for every admitted frame so a live HDR switch cannot silently change policy.
use super::{Monitor, backend_error, utf16_z};
use super::super::super::CaptureError;
use std::collections::HashMap;
use windows::{core::Interface, Win32::Graphics::Dxgi::{
    Common::{DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020}, CreateDXGIFactory1,
    DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6,
}};

#[derive(Default)]
pub(super) struct DisplayColorGate { outputs: HashMap<String, IDXGIOutput6> }
impl DisplayColorGate {
    pub(super) fn clear(&mut self) { self.outputs.clear(); }
    /// Return true only for the recognized Windows HDR10 desktop output.
    pub(super) fn is_hdr(&mut self, monitor:&Monitor)->Result<bool,CaptureError> {
        let key=monitor.device_name.to_ascii_lowercase();
        if let Some(output)=self.outputs.get(&key) {
            if let Ok(desc)=unsafe{output.GetDesc1()} {
                if desc.AttachedToDesktop.as_bool() && std::ptr::eq(desc.Monitor.0,monitor.handle.0)
                    && utf16_z(&desc.DeviceName)?.eq_ignore_ascii_case(&monitor.device_name) {
                    return classify(desc.ColorSpace);
                }
            }
            self.outputs.remove(&key);
        }
        let factory:IDXGIFactory1=unsafe{CreateDXGIFactory1()}.map_err(|e|backend_error("CreateDXGIFactory1 (display color)",e))?;
        for a in 0..16 {
            let adapter=match unsafe{factory.EnumAdapters1(a)} {Ok(value)=>value,Err(e) if e.code()==DXGI_ERROR_NOT_FOUND=>break,Err(e)=>return Err(backend_error("EnumAdapters1 (display color)",e))};
            for o in 0..64 {
                let output=match unsafe{adapter.EnumOutputs(o)} {Ok(value)=>value,Err(e) if e.code()==DXGI_ERROR_NOT_FOUND=>break,Err(e)=>return Err(backend_error("EnumOutputs (display color)",e))};
                let desc=unsafe{output.GetDesc()}.map_err(|e|backend_error("IDXGIOutput::GetDesc (display color)",e))?;
                if desc.AttachedToDesktop.as_bool() && std::ptr::eq(desc.Monitor.0,monitor.handle.0)
                    && utf16_z(&desc.DeviceName)?.eq_ignore_ascii_case(&monitor.device_name) {
                    let output:IDXGIOutput6=output.cast().map_err(|_|CaptureError::Unavailable("Cannot determine display color space; enable Force SDR"))?;
                    let desc=unsafe{output.GetDesc1()}.map_err(|_|CaptureError::Unavailable("Cannot determine display color space; enable Force SDR"))?;
                    let result=classify(desc.ColorSpace);
                    // Hotplug can introduce fresh GDI names repeatedly. Bound
                    // retained COM interfaces independently of session age.
                    if self.outputs.len()>=64 {self.outputs.clear();}
                    self.outputs.insert(key,output);
                    return result;
                }
            }
        }
        Err(CaptureError::TargetNotFound(monitor.info.target.clone()))
    }
}
fn classify(space:windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_TYPE)->Result<bool,CaptureError>{
    if space==DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 {Ok(false)}
    else if space==DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 {Ok(true)}
    else {Err(CaptureError::Unavailable("Unknown display color space; enable Force SDR"))}
}
