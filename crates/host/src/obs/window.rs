//! Strict OBS 32.2.2 Windows window_capture subset.
use super::*;
use skylive_core::{WindowCaptureBinding,WindowCaptureMethod as Method,WindowMatchPriority as Priority,WindowTarget};

#[derive(Default)]
pub(super) struct Importer {windows:Option<Vec<crate::window_capture::WindowInfo>>}
impl Importer {
    pub(super) fn parse(&mut self,settings:&Map<String,Value>,path:&str)->Result<WindowCaptureBinding,ObsError>{
        known(settings,&["window","method","priority","cursor","compatibility","client_area","force_sdr","capture_audio"],path)?;
        if !cfg!(target_os="windows"){return Err(issue(path,"unsupported_platform","Windows window_capture requires the Windows backend"));}
        // OBS compatibility mode selects a CPU DIB instead of a GDI texture.
        // Our BitBlt backend always captures through a DIB, so this setting
        // needs no alternate host path. It is irrelevant to WGC. Still reject
        // malformed values rather than accepting them as a boolean.
        let _compatibility=boolean(settings,"compatibility",false,path)?;
        let capture_audio=boolean(settings,"capture_audio",false,path)?;
        let method=match settings.get("method").map(Value::as_i64).unwrap_or(Some(0)){Some(0)=>Method::Auto,Some(1)=>Method::Gdi,Some(2)=>Method::Wgc,_=>return Err(issue(path,"unsupported_window_method","Window Capture method must be Auto, BitBlt or WGC"))};
        let priority=match settings.get("priority").map(Value::as_i64).unwrap_or(Some(0)){Some(0)=>Priority::Class,Some(1)=>Priority::Title,Some(2)=>Priority::Executable,_=>return Err(issue(path,"unsupported_window_priority","Window match priority must be 0, 1 or 2"))};
        let client=boolean(settings,"client_area",true,path)?;
        let force_sdr=boolean(settings,"force_sdr",false,path)?;
        let value=string(settings,"window",path)?;let mut parts=value.split(':');
        let title=parts.next().unwrap_or("");let class=parts.next().ok_or_else(||issue(path,"invalid_window_selector","window class is missing"))?;let exe=parts.next().ok_or_else(||issue(path,"invalid_window_selector","window executable is missing"))?;
        if parts.next().is_some(){return Err(issue(path,"invalid_window_selector","window tuple has extra components"));}
        let decode=|s:&str|s.replace("#3A",":").replace("#22","#");
        let target=WindowTarget::Windows{title:decode(title),class:decode(class),executable:decode(exe)};
        let selects_wgc=method==Method::Wgc || method==Method::Auto && matches!(&target,WindowTarget::Windows{class,..} if crate::window_capture::matching::auto_uses_wgc(class));
        // OBS keeps the WGC client-area preference when switching to BitBlt.
        // BitBlt always captures the client rectangle regardless of that saved
        // value; retain it for a later switch back to WGC.
        if selects_wgc&&!force_sdr{return Err(issue(path,"unsupported_window_hdr","WGC import requires Force SDR"));}
        if selects_wgc && matches!(&target,WindowTarget::Windows{class,..} if ["Windows.UI.Core.CoreWindow","WinUIDesktopWin32WindowClass","ApplicationFrameWindow"].iter().any(|name|class.eq_ignore_ascii_case(name))){return Err(issue(path,"unsupported_window_uwp","OBS WGC top-level UWP selection differs from the host resolver"));}
        if self.windows.is_none(){let mut backend=crate::window_capture::native::backend();self.windows=Some(backend.enumerate(crate::capture::CaptureLimits::default()).map_err(|error|issue(path,"window_enumeration",error.to_string()))?);}
        let mut best=None;let mut score=u32::MAX;
        for window in self.windows.as_ref().expect("inventory") {if let Some(next)=crate::window_capture::matching::windows_rating(&target,&window.target,priority){if next<score{best=Some(window);score=next;if next==0{break;}}}}
        let window=best.ok_or_else(||issue(path,"window_target_not_found","Matching window is unavailable; OBS does not retain a transform basis for offline import"))?;
        if window.minimized{return Err(issue(path,"window_geometry_unavailable","The selected window is minimized; an exact native transform basis is unavailable"));}
        let (width,height)=if selects_wgc&&!client{window.full_window_size.ok_or_else(||issue(path,"window_geometry_unavailable","Full window geometry is unavailable"))?}else{(window.width,window.height)};
        WindowCaptureBinding::new(target,priority,boolean(settings,"cursor",true,path)?,client,width,height).and_then(|binding|binding.with_method(method)).and_then(|binding|binding.with_capture_audio(capture_audio)).map_err(|error|issue(path,"invalid_window_capture",error.to_string()))
    }
}
pub(super) fn export(binding:&WindowCaptureBinding,path:&str)->Result<Value,ObsError>{
    let WindowTarget::Windows{title,class,executable}=binding.target() else{return Err(issue(path,"unsupported_platform","Window Capture OBS interchange requires a Windows window selector"));};
    let method=match binding.method(){Method::Auto=>0,Method::Gdi=>1,Method::Wgc=>2};
    let selects_wgc=method==2 || method==0&&crate::window_capture::matching::auto_uses_wgc(class);
    if selects_wgc&&["Windows.UI.Core.CoreWindow","WinUIDesktopWin32WindowClass","ApplicationFrameWindow"].iter().any(|name|class.eq_ignore_ascii_case(name)){return Err(issue(path,"unsupported_window_uwp","OBS WGC top-level UWP selection differs from the host resolver"));}
    let encode=|s:&str|s.replace('#',"#22").replace(':',"#3A");
    let priority=match binding.priority(){Priority::Class=>0,Priority::Title=>1,Priority::Executable=>2};
    // The Windows GDI backend captures through a CPU DIB, which corresponds
    // to OBS's compatibility path. The field is inert for WGC and must stay
    // true for Auto in case OBS falls back to BitBlt on the destination host.
    Ok(json!({"window":format!("{}:{}:{}",encode(title),encode(class),encode(executable)),"method":method,"priority":priority,"cursor":binding.capture_cursor(),"client_area":binding.client_area(),"compatibility":true,"force_sdr":selects_wgc,"capture_audio":binding.capture_audio()}))
}
