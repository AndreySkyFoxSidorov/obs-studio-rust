//! Optional standard driver interfaces on the already-open MF source. No second
//! camera activation, property-page window, persisted value or automatic retry.
use super::*;
use windows::Win32::Media::DirectShow::{IAMCameraControl, IAMVideoProcAmp};

enum Driver { Camera(IAMCameraControl), ProcAmp(IAMVideoProcAmp) }
impl Driver {
    fn from_source(source: &IMFMediaSource, camera: bool) -> windows::core::Result<Self> {
        if camera { source.cast().map(Self::Camera) } else { source.cast().map(Self::ProcAmp) }
    }
    fn range(&self, id: i32) -> windows::core::Result<ControlRange> {
        let (mut minimum,mut maximum,mut step,mut default,mut flags)=(0,0,0,0,0);
        unsafe { match self {
            Self::Camera(d)=>d.GetRange(id,&mut minimum,&mut maximum,&mut step,&mut default,&mut flags),
            Self::ProcAmp(d)=>d.GetRange(id,&mut minimum,&mut maximum,&mut step,&mut default,&mut flags),
        }?; }
        Ok(ControlRange {minimum,maximum,step,default,flags})
    }
    fn get(&self, id: i32) -> windows::core::Result<(i32,i32)> {
        let (mut value,mut flags)=(0,0);
        unsafe { match self {
            Self::Camera(d)=>d.Get(id,&mut value,&mut flags),
            Self::ProcAmp(d)=>d.Get(id,&mut value,&mut flags),
        }?; }
        Ok((value,flags))
    }
    fn set(&self,id:i32,value:i32,flags:i32)->windows::core::Result<()> {
        unsafe {match self {Self::Camera(d)=>d.Set(id,value,flags),Self::ProcAmp(d)=>d.Set(id,value,flags)}}
    }
}
fn failure(id:CameraControlId,operation:&'static str,error:windows::core::Error)->ControlDiagnostic {
    ControlDiagnostic { property:Some(id),operation,hresult:Some(error.code().0),detail:"native driver call failed",native_values:None }
}
fn invalid_range(id:CameraControlId,r:ControlRange)->ControlDiagnostic {
    ControlDiagnostic {property:Some(id),operation:"GetRange",hresult:None,detail:"driver returned an invalid range, step, default or capability flags",native_values:Some([r.minimum,r.maximum,r.step,r.default,r.flags])}
}
fn observed(id:CameraControlId,range:ControlRange,value:i32,flags:i32)->Result<ControlObserved,ControlDiagnostic> {
    let mode=ControlMode::from_flags(flags).filter(|m| range.supports(*m));
    match mode {
        Some(mode) if value>=range.minimum && value<=range.maximum => Ok(ControlObserved {value,mode,flags}),
        _=>Err(ControlDiagnostic {property:Some(id),operation:"Get",hresult:None,detail:"driver returned an unsupported mode or out-of-range current value",native_values:Some([value,flags,range.minimum,range.maximum,range.flags])}),
    }
}
pub(super) fn query(source:&IMFMediaSource)->ControlQuery {
    let camera=Driver::from_source(source,true);
    let procamp=Driver::from_source(source,false);
    let mut result=ControlQuery::default();
    for id in CameraControlId::ALL {
        let (is_camera,property)=id.native();
        let driver=if is_camera {&camera} else {&procamp};
        let driver=match driver {Ok(d)=>d,Err(e)=>{result.diagnostics.push(failure(id,"QueryInterface",e.clone()));continue;}};
        let range=match driver.range(property) {Ok(r) if r.valid()=>r,Ok(r)=>{result.diagnostics.push(invalid_range(id,r));continue;},Err(e)=>{result.diagnostics.push(failure(id,"GetRange",e));continue;}};
        match driver.get(property) {
            Ok((value,flags))=>match observed(id,range,value,flags) {
                Ok(observed)=>result.controls.push(CameraControlInfo {id,range,observed}),
                Err(d)=>result.diagnostics.push(d),
            },
            Err(e)=>result.diagnostics.push(failure(id,"Get",e)),
        }
    }
    result
}
pub(super) fn set(source:&IMFMediaSource,change:ControlChange,gate:&ControlWriteGate)->ControlReceipt {
    let (is_camera,property)=change.id.native();
    let mut receipt=ControlReceipt::before(change,"driver write has not started");
    let driver=match Driver::from_source(source,is_camera) {Ok(d)=>d,Err(e)=>{receipt.diagnostic=Some(failure(change.id,"QueryInterface",e));return receipt;}};
    let range=match driver.range(property) {Ok(r) if r.valid()=>r,Ok(r)=>{receipt.diagnostic=Some(invalid_range(change.id,r));return receipt;},Err(e)=>{receipt.diagnostic=Some(failure(change.id,"GetRange",e));return receipt;}};
    let (value,mode)=match change.setting {
        ControlSetting::Auto if range.supports(ControlMode::Auto)=>(range.default,ControlMode::Auto),
        ControlSetting::Manual(value) if range.manual_value(value)=>(value,ControlMode::Manual),
        ControlSetting::ManualDefault if range.manual_value(range.default)=>(range.default,ControlMode::Manual),
        _=>return ControlReceipt::before(change,"requested mode/value is outside the current driver capabilities or step"),
    };
    // CAS arbitrates against the frontend's before-publication cancellation.
    // Only this exact worker can win permission to perform the external effect.
    if !gate.begin_write() {return ControlReceipt::before(change,"write cancelled before the external driver call");}
    receipt.phase=ControlPhase::AfterWrite;
    receipt.write_started=true;
    receipt.diagnostic=None;
    if let Err(e)=driver.set(property,value,mode.flags()) {
        receipt.diagnostic=Some(failure(change.id,"Set",e));
        return receipt;
    }
    receipt.write_succeeded=true;
    receipt.phase=ControlPhase::Readback;
    match driver.get(property) {
        Ok((actual,flags))=>match observed(change.id,range,actual,flags) {
            Ok(observed)=>{
                receipt.observed=Some(observed);
                if observed.mode!=mode || (mode==ControlMode::Manual && actual!=value) {
                    receipt.diagnostic=Some(ControlDiagnostic {property:Some(change.id),operation:"Get",hresult:None,detail:"Set succeeded; driver readback differs from requested value/mode",native_values:Some([value,mode.flags(),actual,flags,range.flags])});
                }
            },
            Err(d)=>receipt.diagnostic=Some(d),
        },
        Err(e)=>receipt.diagnostic=Some(failure(change.id,"Get after Set",e)),
    }
    receipt
}
