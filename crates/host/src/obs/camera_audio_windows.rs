//! DirectShow waveform audio identity to a live WASAPI capture endpoint.
//! Invoked only by the import I/O owner. No device is opened.
use super::{camera::CameraAudioMapping, ObsError, issue};
use windows::{core::{BSTR, Interface, VARIANT, w}, Win32::{
    Media::{Audio::{waveInMessage, HWAVEIN, IMMDeviceEnumerator, IMMEndpoint, MMDeviceEnumerator, DEVICE_STATE_ACTIVE, eCapture},
        DirectShow::ICreateDevEnum, MediaFoundation::{CLSID_SystemDeviceEnum, CLSID_AudioInputDeviceCategory},
        Multimedia::{DRV_QUERYFUNCTIONINSTANCEID, DRV_QUERYFUNCTIONINSTANCEIDSIZE}},
    System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CreateBindCtx, IErrorLog, IMoniker, COINIT_MULTITHREADED, CLSCTX_ALL, CLSCTX_INPROC_SERVER, StructuredStorage::IPropertyBag},
}};

struct Apartment;
impl Drop for Apartment { fn drop(&mut self) { unsafe { CoUninitialize() } } }

fn property(bag:&IPropertyBag, key:windows::core::PCWSTR)->Option<VARIANT> {
    let mut value=VARIANT::new();
    unsafe { bag.Read(key,&mut value,None::<&IErrorLog>) }.ok()?;
    Some(value)
}
fn text(value:&VARIANT)->Option<String> {
    // Do not coerce a different VARIANT type into an identity string.
    if unsafe { value.as_raw().Anonymous.Anonymous.vt } != 8 {return None;}
    let value=BSTR::try_from(value).ok()?;
    if value.len()>4096 {return None;}
    let string=String::from_utf16(value.as_wide()).ok()?;
    (string.len()<=4096 && !string.chars().any(char::is_control)).then_some(string)
}
fn endpoint(wave_id:u32)->Option<String> {
    let handle=HWAVEIN(wave_id as usize as *mut _);
    let mut bytes=0u32;
    if unsafe {waveInMessage(handle,DRV_QUERYFUNCTIONINSTANCEIDSIZE,(&mut bytes as *mut u32) as usize,0)}!=0
        || bytes<2 || bytes>2050 || bytes%2!=0 {return None;}
    let mut utf16=vec![0u16;(bytes/2) as usize];
    if unsafe {waveInMessage(handle,DRV_QUERYFUNCTIONINSTANCEID,utf16.as_mut_ptr() as usize,bytes as usize)}!=0 {return None;}
    let end=utf16.iter().position(|unit|*unit==0)?;
    if end==0 || utf16[end+1..].iter().any(|unit|*unit!=0) {return None;}
    let id=String::from_utf16(&utf16[..end]).ok()?;
    (id.len()<=1024 && !id.chars().any(char::is_control)).then_some(id)
}

pub(super) fn resolve(requested:&[(String,String)])->Result<Vec<CameraAudioMapping>,ObsError> {
    if requested.is_empty(){return Ok(Vec::new());}
    enumerate(Some(requested), None)
}

pub(super) fn resolve_endpoint(endpoint_id:&str)->Result<Vec<CameraAudioMapping>,ObsError> {
    enumerate(None, Some(endpoint_id))
}

fn enumerate(requested:Option<&[(String,String)]>, endpoint_id:Option<&str>)->Result<Vec<CameraAudioMapping>,ObsError> {
    unsafe {CoInitializeEx(None,COINIT_MULTITHREADED)}.ok().map_err(|e|issue("$.sources","camera_audio_query",format!("COM audio discovery: {e}")))?;
    let _apartment=Apartment;
    let enumerator:ICreateDevEnum=unsafe {CoCreateInstance(&CLSID_SystemDeviceEnum,None,CLSCTX_INPROC_SERVER)}
        .map_err(|e|issue("$.sources","camera_audio_query",format!("DirectShow audio discovery: {e}")))?;
    let endpoints:IMMDeviceEnumerator=unsafe {CoCreateInstance(&MMDeviceEnumerator,None,CLSCTX_ALL)}
        .map_err(|e|issue("$.sources","camera_audio_query",format!("WASAPI discovery: {e}")))?;
    let mut monikers=None;
    unsafe {enumerator.CreateClassEnumerator(&CLSID_AudioInputDeviceCategory,&mut monikers,0)}
        .map_err(|e|issue("$.sources","camera_audio_query",format!("DirectShow audio category: {e}")))?;
    let Some(monikers)=monikers else{return Ok(Vec::new());};
    let bind=unsafe {CreateBindCtx(0)}.map_err(|e|issue("$.sources","camera_audio_query",format!("DirectShow bind context: {e}")))?;
    let mut mappings=Vec::new();
    let mut matched=Vec::<(String,String)>::new();
    let mut identities=Vec::<(String,String)>::new();
    for index in 0..=256 {
        let mut slot=[None];let status=unsafe {monikers.Next(&mut slot,None)};
        if status.is_err(){return Err(issue("$.sources","camera_audio_query",format!("DirectShow audio enumeration: {status:?}")));}
        let Some(moniker)=slot[0].take() else {break;};
        if index==256{return Err(issue("$.sources","camera_audio_limit","DirectShow audio device count exceeds 256"));}
        let Ok(bag):Result<IPropertyBag,_>=(unsafe {moniker.BindToStorage(&bind,None::<&IMoniker>)}) else {continue;};
        let Some(name)=property(&bag,w!("FriendlyName")).as_ref().and_then(text) else {continue;};
        let path=match property(&bag,w!("DevicePath")) {
            Some(value)=>match text(&value){Some(path)=>path,None=>continue},
            None=>String::new(),
        };
        if requested.is_none() { identities.push((name.clone(),path.clone())); }
        if requested.is_some_and(|requested| !requested.iter().any(|(expected_name,expected_path)|*expected_name==name && *expected_path==path)){continue;}
        if requested.is_some() {
            if matched.iter().any(|(seen_name,seen_path)|*seen_name==name && *seen_path==path) {
                return Err(issue("$.sources","camera_audio_ambiguous","Several DirectShow audio monikers share the OBS identity"));
            }
            matched.push((name.clone(),path.clone()));
        }
        let Some(wave)=property(&bag,w!("WaveInID")) else {continue;};
        if unsafe {wave.as_raw().Anonymous.Anonymous.vt} != 3 {continue;}
        let Ok(wave)=i32::try_from(&wave) else {continue;};
        let Ok(wave)=u32::try_from(wave) else {continue;};
        let Some(id)=endpoint(wave) else {continue;};
        if endpoint_id.is_some_and(|wanted| !wanted.eq_ignore_ascii_case(&id)){continue;}
        if requested.is_none() {
            if matched.iter().any(|(seen_name,seen_path)|*seen_name==name && *seen_path==path) {
                return Err(issue("$.sources","camera_audio_ambiguous","Several DirectShow audio monikers share the OBS identity"));
            }
            matched.push((name.clone(),path.clone()));
        }
        let wide:Vec<u16>=id.encode_utf16().chain(std::iter::once(0)).collect();
        let Ok(device)=(unsafe {endpoints.GetDevice(windows::core::PCWSTR(wide.as_ptr()))}) else {continue;};
        if unsafe {device.GetState()}.ok()!=Some(DEVICE_STATE_ACTIVE) {continue;}
        let Ok(endpoint)=device.cast::<IMMEndpoint>() else {continue;};
        if unsafe {endpoint.GetDataFlow()}.ok()!=Some(eCapture) {continue;}
        mappings.push(CameraAudioMapping{name,path,endpoint_id:id});
    }
    if requested.is_none() && mappings.iter().any(|mapping| identities.iter().filter(|(name,path)| *name==mapping.name && *path==mapping.path).count()!=1) {
        return Err(issue("$.sources","camera_audio_ambiguous","The DirectShow audio identity is not unique"));
    }
    Ok(mappings)
}
