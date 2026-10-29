//! OBS32.2.2 monitor_capture, peeled ba2f32bdf791005443988a4955e963663e16b1ed.
//! plugins/win-capture/duplicator-monitor-capture.c: method0/1/2, monitor_id,
//! capture_cursor and force_sdr. No capture/device API is called in construction.
use super::{ObsError,boolean,issue,known,string};
use crate::capture::DisplayInfo;
use serde_json::{Map,Value,json};
use skylive_core::{DisplayCaptureBinding,DisplayCaptureMethod,DisplayTarget};

#[derive(Default)]
pub(super) struct Importer {displays:Option<Vec<DisplayInfo>>}
impl Importer {
    pub(super) fn parse(&mut self,settings:&Map<String,Value>,path:&str)->Result<DisplayCaptureBinding,ObsError>{
        known(settings,&["monitor_id","method","capture_cursor","force_sdr","monitor_wgc"],path)?;
        // OBS 32 retains this legacy integer in settings, but the current
        // monitor capture implementation only declares its default and never
        // reads it. The selected monitor is resolved from `monitor_id` for all
        // methods, so a saved nonzero legacy index must not block import.
        if settings.get("monitor_wgc").is_some_and(|value|value.as_i64().is_none()) {
            return Err(issue(&format!("{path}.monitor_wgc"),"invalid","legacy monitor_wgc must be an integer"));
        }
        // HDR-preserving OBS sources cannot be silently flattened to this SDR
        // engine. Export explicitly writes true; false/missing is not equivalent.
        if !boolean(settings,"force_sdr",false,path)? {return Err(issue(&format!("{path}.force_sdr"),"unsupported","monitor_capture import requires Force SDR; HDR-preserving capture is not implemented"));}
        let id=string(settings,"monitor_id",path)?;
        if id.is_empty() || id.len()>1024 || id.chars().any(char::is_control) {return Err(issue(&format!("{path}.monitor_id"),"invalid","a stable Windows monitor interface ID is required"));}
        let method=match settings.get("method") {
            None=>DisplayCaptureMethod::Auto,
            Some(value)=>match value.as_i64() {Some(0)=>DisplayCaptureMethod::Auto,Some(1)=>DisplayCaptureMethod::Dxgi,Some(2)=>DisplayCaptureMethod::Wgc,_=>return Err(issue(&format!("{path}.method"),"unsupported","monitor_capture method must be Auto0, DXGI1 or WGC2"))},
        };
        if !cfg!(target_os="windows") {return Err(issue(path,"unsupported_platform","Windows monitor_capture requires Windows enumeration to resolve its real transform basis"));}
        if self.displays.is_none() {
            // Existing document-import worker owns this read-only enumeration.
            // Do not open the display, acquire pixels or invent canvas-sized
            // dimensions for an unavailable monitor. Cache one bounded inventory.
            let mut backend=crate::capture::native::backend();
            self.displays=Some(backend.enumerate(crate::capture::CaptureLimits::default()).map_err(|error|issue(path,"display_enumeration",error.to_string()))?);
        }
        let display=self.displays.as_ref().and_then(|displays|displays.iter().find(|display|matches!(&display.target,DisplayTarget::Windows {device_id} if device_id.eq_ignore_ascii_case(id))))
            .ok_or_else(||issue(&format!("{path}.monitor_id"),"display_target_not_found","the exact Windows monitor is unavailable; refusing a primary-monitor or canvas-size substitution"))?;
        display.binding(boolean(settings,"capture_cursor",true,path)?)
            .map_err(|error|issue(path,"invalid_display",error.to_string()))?
            .with_method(method).map_err(|error|issue(path,"invalid_display",error.to_string()))
    }
}
pub(super) fn export(binding:&DisplayCaptureBinding,path:&str)->Result<Value,ObsError>{
    let DisplayTarget::Windows {device_id}=binding.target() else {return Err(issue(path,"unsupported_platform","macOS display capture does not use the Windows monitor_capture provider"));};
    let method=match binding.method(){DisplayCaptureMethod::Auto=>0,DisplayCaptureMethod::Dxgi=>1,DisplayCaptureMethod::Wgc=>2,DisplayCaptureMethod::Gdi=>return Err(issue(path,"unsupported_capture_method","OBS32 monitor_capture has no GDI method; choose Auto, DXGI or WGC before export"))};
    Ok(json!({"monitor_id":device_id,"method":method,"capture_cursor":binding.capture_cursor(),"force_sdr":true}))
}
