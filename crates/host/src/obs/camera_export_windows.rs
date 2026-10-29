//! Resolve a Media Foundation symbolic link to the exact DirectShow OBS identity.
//! Enumeration runs on the export I/O owner and never opens the camera.
use super::{issue, ObsError};
use windows::{core::{BSTR, VARIANT, w}, Win32::{
    Media::{DirectShow::ICreateDevEnum, MediaFoundation::{CLSID_SystemDeviceEnum, CLSID_VideoInputDeviceCategory}},
    System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CreateBindCtx, IErrorLog, IMoniker, COINIT_MULTITHREADED, CLSCTX_INPROC_SERVER, StructuredStorage::IPropertyBag},
}};
struct Apartment;
impl Drop for Apartment { fn drop(&mut self) { unsafe { CoUninitialize() } } }
fn property(bag:&IPropertyBag, key:windows::core::PCWSTR)->Option<String> {
    let mut value=VARIANT::new();
    unsafe { bag.Read(key,&mut value,None::<&IErrorLog>) }.ok()?;
    if unsafe { value.as_raw().Anonymous.Anonymous.vt } != 8 {return None;}
    let value=BSTR::try_from(&value).ok()?;
    if value.len()>4096 {return None;}
    let value=String::from_utf16(value.as_wide()).ok()?;
    (!value.is_empty() && value.len()<=4096 && !value.chars().any(char::is_control)).then_some(value)
}
pub(super) fn identity(symbolic_link:&str,path:&str)->Result<(String,String),ObsError> {
    unsafe { CoInitializeEx(None,COINIT_MULTITHREADED) }.ok().map_err(|e|issue(path,"camera_device_query",format!("COM camera discovery: {e}")))?;
    let _apartment=Apartment;
    let enumerator:ICreateDevEnum=unsafe { CoCreateInstance(&CLSID_SystemDeviceEnum,None,CLSCTX_INPROC_SERVER) }
        .map_err(|e|issue(path,"camera_device_query",format!("DirectShow camera discovery: {e}")))?;
    let mut monikers=None;
    unsafe { enumerator.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory,&mut monikers,0) }
        .map_err(|e|issue(path,"camera_device_query",format!("DirectShow camera category: {e}")))?;
    let monikers=monikers.ok_or_else(||issue(path,"camera_device_unresolved","No DirectShow cameras are available"))?;
    let bind=unsafe { CreateBindCtx(0) }.map_err(|e|issue(path,"camera_device_query",format!("DirectShow bind context: {e}")))?;
    let mut found=None;
    for index in 0..=256 {
        let mut slot=[None]; let status=unsafe { monikers.Next(&mut slot,None) };
        if status.is_err(){return Err(issue(path,"camera_device_query",format!("DirectShow camera enumeration: {status:?}")));}
        let Some(moniker)=slot[0].take() else {break;};
        if index==256{return Err(issue(path,"camera_device_limit","DirectShow camera count exceeds 256"));}
        let Ok(bag):Result<IPropertyBag,_>=(unsafe { moniker.BindToStorage(&bind,None::<&IMoniker>) }) else {continue;};
        let Some(device_path)=property(&bag,w!("DevicePath")) else {continue;};
        if !device_path.eq_ignore_ascii_case(symbolic_link) {continue;}
        let name=property(&bag,w!("FriendlyName")).ok_or_else(||issue(path,"camera_device_unresolved","The matching DirectShow camera has no valid friendly name"))?;
        if found.is_some(){return Err(issue(path,"camera_device_ambiguous","Several DirectShow cameras share this device path"));}
        found=Some((name,device_path));
    }
    found.ok_or_else(||issue(path,"camera_device_unresolved","The exact Media Foundation camera path is unavailable in DirectShow"))
}
