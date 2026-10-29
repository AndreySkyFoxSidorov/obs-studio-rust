//! OBS SDR BitBlt window capture. Never uses PrintWindow or captures the desktop.
use super::super::{MAX_WINDOWS, WindowCaptureBackend, WindowCaptureError, WindowInfo};
use crate::capture::native_budget::NativeCopyBudget;
use crate::capture::{CaptureCapabilities, CaptureLimits};
use crate::window_capture::{WindowCaptureBackendKind, WindowCaptureOutcome, WindowFrameEncoding, WindowFrameMetadata};
use skylive_core::{MAX_DISPLAY_PIXELS, WindowCaptureBinding, WindowCaptureMethod, WindowTarget};
#[path = "native_wgc.rs"]
mod wgc;
use skylive_media::{
    AlphaMode, PackedFrameLayout, PackedFrameLimits, PackedPixelFormat, RgbaFrame, RowOrder,
    ingest_packed_frame,
};
use std::{ffi::c_void, mem::size_of, ptr};
use windows::{
    Win32::{
        Foundation::{BOOL, CloseHandle, FILETIME, HANDLE, HWND, LPARAM, POINT, RECT, WAIT_TIMEOUT},
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, ClientToScreen, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, HBITMAP,
            HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
        },
        System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
        },
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE,
                SetThreadDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                CURSOR_SHOWING, CURSORINFO, CopyIcon, DI_NORMAL, DestroyIcon, DrawIconEx,
                EnumWindows, GW_CHILD, GW_HWNDNEXT, GWL_EXSTYLE, GetClassNameW, GetClientRect,
                GetCursorInfo, GetForegroundWindow, GetIconInfo, GetWindow, GetWindowLongPtrW,
                GetWindowTextW, GetWindowThreadProcessId, HICON, ICONINFO, IsIconic, IsWindow,
                IsWindowVisible, WS_EX_TOOLWINDOW,
            },
        },
    },
    core::PWSTR,
};

pub(super) struct NativeBackend {
    session: u64,
    epoch: u64,
    wgc: wgc::WgcBackend,
    cached: std::collections::HashMap<
        (WindowTarget, skylive_core::WindowMatchPriority),
        (isize, WindowTarget, u32),
    >,
    audio_processes: std::collections::HashMap<isize, (u32, u64, Process)>,
}
impl NativeBackend {
    pub(super) fn new() -> Self {
        Self::with_budget(NativeCopyBudget::default())
    }
    pub(super) fn with_budget(budget: NativeCopyBudget) -> Self {
        Self {
            session: 0,
            epoch: 0,
            wgc: wgc::WgcBackend::new(budget),
            cached: std::collections::HashMap::new(),
            audio_processes: std::collections::HashMap::new(),
        }
    }
}
impl NativeBackend {
    /// Retain a process handle while it supplies linked window audio. A handle
    /// and creation timestamp fence PID reuse, and HWND/PID are checked both
    /// before and after capturing the corresponding video frame.
    fn observed_window(&mut self,hwnd:HWND,pid:u32,audio:bool)->Option<crate::window_capture::NativeWindowIdentity>{
        let mut actual=0;unsafe {GetWindowThreadProcessId(hwnd,Some(&mut actual));}
        if pid==0 || actual!=pid || !unsafe {IsWindow(hwnd)}.as_bool(){return None;}
        if !audio {return Some(crate::window_capture::NativeWindowIdentity {handle:hwnd.0 as usize,process_id:pid,process_creation_time_100ns:None});}
        let key=hwnd.0 as isize;
        let valid=self.audio_processes.get(&key).is_some_and(|(old,_,process)|*old==pid && unsafe {WaitForSingleObject(process.0,0)}==WAIT_TIMEOUT);
        if !valid {
            self.audio_processes.remove(&key);
            self.audio_processes.retain(|_,(_,_,process)|unsafe {WaitForSingleObject(process.0,0)}==WAIT_TIMEOUT);
            if self.audio_processes.len()>=MAX_WINDOWS {return None;}
            let process=Process(unsafe {OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION|PROCESS_SYNCHRONIZE,false,pid)}.ok()?);
            let (mut creation,mut exit,mut kernel,mut user)=(FILETIME::default(),FILETIME::default(),FILETIME::default(),FILETIME::default());
            unsafe {GetProcessTimes(process.0,&mut creation,&mut exit,&mut kernel,&mut user)}.ok()?;
            let creation=(u64::from(creation.dwHighDateTime)<<32)|u64::from(creation.dwLowDateTime);
            if creation==0 || unsafe {WaitForSingleObject(process.0,0)}!=WAIT_TIMEOUT{return None;}
            self.audio_processes.insert(key,(pid,creation,process));
        }
        let (_,creation,process)=self.audio_processes.get(&key)?;
        let mut after=0;unsafe {GetWindowThreadProcessId(hwnd,Some(&mut after));}
        if after!=pid || !unsafe {IsWindow(hwnd)}.as_bool() || unsafe {WaitForSingleObject(process.0,0)}!=WAIT_TIMEOUT{return None;}
        Some(crate::window_capture::NativeWindowIdentity {handle:hwnd.0 as usize,process_id:pid,process_creation_time_100ns:Some(*creation)})
    }
    fn capture_gdi_outcome(&mut self,binding:&WindowCaptureBinding,limits:CaptureLimits)->Result<WindowCaptureOutcome,WindowCaptureError>{
        // OBS BitBlt always uses the client area, including when a saved WGC
        // whole-window option remains set after switching to explicit BitBlt.
        // Keep the authored option on the real binding for later WGC use.
        let client_binding=if !binding.client_area(){
            Some(WindowCaptureBinding::new(binding.target().clone(),binding.priority(),binding.capture_cursor(),true,binding.width(),binding.height())
                .map_err(|_|WindowCaptureError::InvalidFrame("automatic BitBlt client binding"))?)
        }else{None};
        let effective=client_binding.as_ref().unwrap_or(binding);
        let window=self.resolve_window(effective)?;
        let mut pid=0;
        unsafe { GetWindowThreadProcessId(window.hwnd,Some(&mut pid)); }
        if pid==0 { return Err(WindowCaptureError::TargetNotFound); }
        let before=self.observed_window(window.hwnd,pid,binding.capture_audio());
        let image=self.capture(effective,limits)?;
        let mut after=0;
        unsafe { GetWindowThreadProcessId(window.hwnd,Some(&mut after)); }
        let key=(effective.target().clone(),effective.priority());
        if after!=pid || !unsafe { IsWindow(window.hwnd) }.as_bool()
            || !self.cached.get(&key).is_some_and(|(handle,_,cached_pid)|*handle==window.hwnd.0 as isize && *cached_pid==pid) {
            return Err(WindowCaptureError::TargetNotFound);
        }
        let after_identity=self.observed_window(window.hwnd,pid,binding.capture_audio());
        let native_window=if before==after_identity {after_identity}else{None};
        Ok(WindowCaptureOutcome::Frame {
            image,
            metadata:WindowFrameMetadata {backend:WindowCaptureBackendKind::Gdi,generation:1,encoding:WindowFrameEncoding::OpaqueSrgb,
                native_window},
        })
    }
    fn resolve_window(
        &mut self,
        binding: &WindowCaptureBinding,
    ) -> Result<Window, WindowCaptureError> {
        let cache_key = (binding.target().clone(), binding.priority());
        let cached = if let Some((raw, observed, old_pid)) = self.cached.get(&cache_key) {
            let hwnd = HWND(*raw as *mut c_void);
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            if !unsafe { IsWindow(hwnd) }.as_bool() || pid != *old_pid {
                None
            } else {
                if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
                    return Err(WindowCaptureError::TargetHidden);
                }
                if unsafe { IsIconic(hwnd) }.as_bool() {
                    return Err(WindowCaptureError::TargetMinimized);
                }
                describe(hwnd)?.filter(|current|matches!((&current.info.target,observed),(WindowTarget::Windows{class,executable,..},WindowTarget::Windows{class:oldclass,executable:oldexe,..}) if class.eq_ignore_ascii_case(oldclass)&&executable.eq_ignore_ascii_case(oldexe)))
            }
        } else {
            None
        };
        let found = if let Some(found) = cached {
            found
        } else {
            let mut best = None;
            let mut rating = u32::MAX;
            for candidate in windows(false)? {
                if let Some(score) = super::super::matching::windows_rating(
                    binding.target(),
                    &candidate.info.target,
                    binding.priority(),
                ) {
                    if score < rating {
                        rating = score;
                        best = Some(candidate);
                        if score == 0 {
                            break;
                        }
                    }
                }
            }
            best.ok_or(WindowCaptureError::TargetNotFound)?
        };
        if !self.cached.contains_key(&cache_key)
            && self.cached.len() >= crate::capture::MAX_CAPTURE_REFERENCES
        {
            return Err(WindowCaptureError::BudgetExceeded(
                "window selector runtime cache",
            ));
        }
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(found.hwnd, Some(&mut pid));
        }
        self.cached.insert(
            cache_key,
            (found.hwnd.0 as isize, found.info.target.clone(), pid),
        );
        if !unsafe { IsWindowVisible(found.hwnd) }.as_bool() {
            return Err(WindowCaptureError::TargetHidden);
        }
        if unsafe { IsIconic(found.hwnd) }.as_bool() {
            return Err(WindowCaptureError::TargetMinimized);
        }
        Ok(found)
    }
}
impl WindowCaptureBackend for NativeBackend {
    fn retain_epoch(&mut self, session: u64, epoch: u64, bindings: &[WindowCaptureBinding]) {
        let reset = self.session != session || self.epoch != epoch;
        self.retain_bindings(session, bindings);
        self.epoch = epoch;
        self.wgc.retain(bindings, reset);
    }
    fn reap_resources(&mut self) -> bool {
        self.wgc.reap()
    }
    fn capture_outcome(
        &mut self,
        binding: &WindowCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<WindowCaptureOutcome, WindowCaptureError> {
        let _dpi = DpiGuard::new()?;
        if !matches!(binding.target(), WindowTarget::Windows { .. }) {
            return Err(WindowCaptureError::Unavailable(
                "Windows window backend requires a Windows target",
            ));
        }
        let found = self.resolve_window(binding)?;
        let use_wgc = match binding.method() {
            WindowCaptureMethod::Wgc => true,
            WindowCaptureMethod::Gdi => false,
            WindowCaptureMethod::Auto => {
                // OBS chooses once from the authored selector class, not from
                // whichever window happened to win a title/executable match.
                matches!(binding.target(),WindowTarget::Windows{class,..} if auto_uses_wgc(class))
            }
        };
        if use_wgc {
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(found.hwnd, Some(&mut pid));
            }
            let before=self.observed_window(found.hwnd,pid,binding.capture_audio());
            let outcome = match self.wgc.capture(binding, found.hwnd, pid,before.and_then(|value|value.process_creation_time_100ns), limits) {
                Ok(outcome)=>outcome,
                // OBS Auto uses BitBlt when WGC is unavailable on the OS.
                // Keep explicit WGC, permission failures and target errors
                // distinct; never silently substitute another window/method.
                Err(WindowCaptureError::Unavailable("Windows Graphics Capture is unsupported on this machine" | "WGC direct window/monitor capture requires Windows10 1903 or newer"))
                    if binding.method()==WindowCaptureMethod::Auto=>return self.capture_gdi_outcome(binding,limits),
                Err(error)=>return Err(error),
            };
            let mut after_pid = 0;
            unsafe {
                GetWindowThreadProcessId(found.hwnd, Some(&mut after_pid));
            }
            if after_pid != pid || !unsafe { IsWindow(found.hwnd) }.as_bool() {
                return Err(WindowCaptureError::TargetNotFound);
            }
            let after_identity=self.observed_window(found.hwnd,pid,binding.capture_audio());
            let native_window=if before==after_identity {after_identity}else{None};
            Ok(match outcome {WindowCaptureOutcome::Frame {image,mut metadata}=>{metadata.native_window=native_window;WindowCaptureOutcome::Frame {image,metadata}},other=>other})
        } else {
            self.capture_gdi_outcome(binding,limits)
        }
    }

    fn retain_bindings(&mut self, session: u64, bindings: &[WindowCaptureBinding]) {
        if self.session != session {
            self.cached.clear();
            self.audio_processes.clear();
            self.session = session;
        }
        let keys: std::collections::HashSet<_> = bindings
            .iter()
            .map(|v| (v.target().clone(), v.priority()))
            .collect();
        self.cached.retain(|key, _| keys.contains(key));
        let handles:std::collections::HashSet<_>=self.cached.values().map(|(handle,_,_)|*handle).collect();
        self.audio_processes.retain(|handle,_|handles.contains(handle));
    }

    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            available: true,
            backend: "windows-window-capture",
            cursor: true,
            hdr: false,
            note: Some(
                "SDR GDI client-area or genuine Windows Graphics Capture client/full-window; WGC requires runtime support and retains the OS border. HDR and protected-content bypass are unavailable.",
            ),
        }
    }
    fn enumerate(&mut self, _: CaptureLimits) -> Result<Vec<WindowInfo>, WindowCaptureError> {
        let _dpi = DpiGuard::new()?;
        // OBS populates the chooser with EXCLUDE_MINIMIZED, while the
        // runtime resolver uses INCLUDE_MINIMIZED to retain an existing
        // target while it is minimized or cloaked.
        Ok(windows(true)?.into_iter().map(|w| w.info).collect())
    }
    fn capture(
        &mut self,
        binding: &WindowCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<RgbaFrame, WindowCaptureError> {
        if !matches!(binding.target(), WindowTarget::Windows { .. }) {
            return Err(WindowCaptureError::Unavailable(
                "window target requires another OS",
            ));
        }
        if binding.method() == WindowCaptureMethod::Wgc {
            return Err(WindowCaptureError::Unavailable(
                "use capture_outcome for asynchronous WGC frames",
            ));
        }
        if !binding.client_area() {
            return Err(WindowCaptureError::Unavailable(
                "GDI captures client area only; choose Windows Graphics Capture for the complete window",
            ));
        }
        let _dpi = DpiGuard::new()?;
        let found = self.resolve_window(binding)?;
        let mut rect = RECT::default();
        unsafe { GetClientRect(found.hwnd, &mut rect) }
            .map_err(|e| backend_error("GetClientRect", e))?;
        let width = u32::try_from(i64::from(rect.right) - i64::from(rect.left))
            .map_err(|_| WindowCaptureError::InvalidFrame("window width"))?;
        let height = u32::try_from(i64::from(rect.bottom) - i64::from(rect.top))
            .map_err(|_| WindowCaptureError::InvalidFrame("window height"))?;
        let bytes = admitted_bytes(width, height, limits)?;
        let source = WindowDc::new(found.hwnd)?;
        let dib = Dib::new(source.dc, width, height)?;
        unsafe {
            BitBlt(
                dib.dc,
                0,
                0,
                width as i32,
                height as i32,
                source.dc,
                0,
                0,
                SRCCOPY,
            )
        }
        .map_err(|e| backend_error("BitBlt(window)", e))?;
        if binding.capture_cursor() {
            let mut target_pid = 0;
            let mut front_pid = 0;
            unsafe {
                GetWindowThreadProcessId(found.hwnd, Some(&mut target_pid));
                GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut front_pid));
            }
            if target_pid == 0 || front_pid == 0 || target_pid == front_pid {
                let mut origin = POINT::default();
                // Cursor capture is optional: an icon can disappear or its
                // window can move while the image is being captured. Keep the
                // BitBlt frame in that case, as OBS does, rather than losing
                // the entire source until the cursor becomes available again.
                if unsafe { ClientToScreen(found.hwnd, &mut origin) }.as_bool() {
                    let _ = draw_cursor(
                        dib.dc,
                        RECT {
                            left: origin.x,
                            top: origin.y,
                            right: 0,
                            bottom: 0,
                        },
                    );
                }
            }
        }
        if !unsafe { GdiFlush() }.as_bool() {
            return Err(last_error("GdiFlush"));
        }
        let raw = unsafe { std::slice::from_raw_parts(dib.bits.cast::<u8>(), bytes) };
        ingest_packed_frame(
            raw,
            PackedFrameLayout {
                width,
                height,
                stride_bytes: width as usize * 4,
                format: PackedPixelFormat::Bgra8,
                row_order: RowOrder::TopDown,
                alpha: AlphaMode::Opaque,
            },
            PackedFrameLimits {
                max_input_bytes: limits.max_raw_bytes,
                max_output_bytes: limits.max_frame_bytes,
                max_pixel_visits: (bytes / 4) as u64 * 2,
            },
        )
        .map_err(|_| WindowCaptureError::InvalidFrame("GDI window packed frame"))
    }
}
fn admitted_bytes(
    width: u32,
    height: u32,
    limits: CaptureLimits,
) -> Result<usize, WindowCaptureError> {
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > MAX_DISPLAY_PIXELS
    {
        return Err(WindowCaptureError::InvalidFrame(
            "window exceeds capture dimensions",
        ));
    }
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(WindowCaptureError::BudgetExceeded("window byte overflow"))?;
    if bytes > limits.max_frame_bytes
        || bytes
            .checked_mul(2)
            .is_none_or(|n| n > limits.max_raw_bytes)
    {
        return Err(WindowCaptureError::BudgetExceeded(
            "window DIB and RGBA buffers",
        ));
    }
    Ok(bytes)
}
struct Window {
    hwnd: HWND,
    info: WindowInfo,
}
struct Enumeration {
    values: Vec<Window>,
    visited: usize,
    bytes: usize,
    error: Option<WindowCaptureError>,
    exclude_minimized: bool,
}
fn windows(exclude_minimized: bool) -> Result<Vec<Window>, WindowCaptureError> {
    let mut data = Enumeration {
        values: Vec::new(),
        visited: 0,
        bytes: 0,
        error: None,
        exclude_minimized,
    };
    data.values
        .try_reserve_exact(MAX_WINDOWS)
        .map_err(|_| WindowCaptureError::BudgetExceeded("window metadata"))?;
    let result = unsafe {
        EnumWindows(
            Some(callback),
            LPARAM((&mut data as *mut Enumeration) as isize),
        )
    };
    if let Some(error) = data.error {
        return Err(error);
    }
    result.map_err(|e| backend_error("EnumWindows", e))?;
    Ok(data.values)
}
unsafe extern "system" fn callback(hwnd: HWND, param: LPARAM) -> BOOL {
    let data = unsafe { &mut *(param.0 as *mut Enumeration) };
    data.visited += 1;
    if data.visited > MAX_WINDOWS {
        data.error = Some(WindowCaptureError::BudgetExceeded(
            "more than 1024 top-level windows",
        ));
        return BOOL(0);
    }
    if data.exclude_minimized && (unsafe { IsIconic(hwnd) }.as_bool() || is_cloaked(hwnd)) {
        return BOOL(1);
    }
    match actual_uwp_window(hwnd).and_then(describe) {
        Ok(Some(value)) if unsafe { IsWindowVisible(hwnd) }.as_bool() => {
            // OBS hides its own internal Windows shell entries in the chooser,
            // but still permits resolving an existing explicit selector.
            if data.exclude_minimized && matches!(&value.info.target,
                WindowTarget::Windows { executable, .. } if is_internal_windows_executable(executable)) {
                return BOOL(1);
            }
            let bytes = match &value.info.target {
                WindowTarget::Windows {
                    title,
                    class,
                    executable,
                } => title.len() + class.len() + executable.len() + value.info.name.len(),
                _ => 0,
            };
            if data
                .bytes
                .checked_add(bytes)
                .is_none_or(|n| n > 4 * 1024 * 1024)
            {
                data.error = Some(WindowCaptureError::BudgetExceeded(
                    "window metadata exceeds 4MiB",
                ));
                return BOOL(0);
            }
            data.bytes += bytes;
            data.values.push(value)
        }
        Ok(Some(_)) => {}
        Ok(None) => {}
        // A single unrelated window can disappear during enumeration or have
        // metadata that our durable selector cannot represent (for example,
        // a title containing a control character). Skip that candidate, as
        // OBS skips windows it cannot identify, instead of losing the entire
        // selectable list. Aggregate enumeration and traversal budgets below
        // still fail explicitly; never publish a truncated identity.
        Err(WindowCaptureError::InvalidFrame(_)) => {}
        Err(WindowCaptureError::BudgetExceeded("window title/class truncated" | "window UTF8 metadata")) => {}
        Err(e) => {
            data.error = Some(e);
            return BOOL(0);
        }
    }
    BOOL(1)
}
fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    unsafe {
        windows::Win32::Graphics::Dwm::DwmGetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    }.is_ok() && cloaked != 0
}
// Keep this chooser-only policy in sync with OBS window-helpers.c. An already
// authored selector is still resolved even when its executable is on the list.
fn is_internal_windows_executable(executable: &str) -> bool {
    const EXACT: &[&str] = &[
        "startmenuexperiencehost.exe", "applicationframehost.exe",
        "peopleexperiencehost.exe", "shellexperiencehost.exe", "microsoft.notes.exe",
        "systemsettings.exe", "textinputhost.exe", "searchapp.exe", "video.ui.exe",
        "searchui.exe", "lockapp.exe", "cortana.exe", "gamebar.exe", "tabtip.exe",
        "time.exe",
    ];
    EXACT.iter().any(|name| executable.eq_ignore_ascii_case(name))
        || executable.get(.."windowsinternal".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("windowsinternal"))
}
// OBS unwraps ApplicationFrame/WinUI host windows to the first child in a
// different process. Traversal has an independent bound before metadata IO.
fn actual_uwp_window(hwnd: HWND) -> Result<HWND, WindowCaptureError> {
    let mut class = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut class) };
    let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
    if class != "ApplicationFrameWindow" && class != "WinUIDesktopWin32WindowClass" {
        return Ok(hwnd);
    }
    let mut parent_pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut parent_pid));
    }
    let mut child = unsafe { GetWindow(hwnd, GW_CHILD) }.ok();
    let mut visited = 0;
    while let Some(window) = child {
        visited += 1;
        if visited > MAX_WINDOWS {
            return Err(WindowCaptureError::BudgetExceeded("UWP child traversal"));
        }
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(window, Some(&mut pid));
        }
        if pid != 0 && pid != parent_pid {
            return Ok(window);
        }
        child = unsafe { GetWindow(window, GW_HWNDNEXT) }.ok();
    }
    Ok(hwnd)
}
fn describe(hwnd: HWND) -> Result<Option<Window>, WindowCaptureError> {
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    if ex & WS_EX_TOOLWINDOW.0 != 0 {
        return Ok(None);
    }
    let mut title = [0u16; 1025];
    let mut class = [0u16; 1025];
    let nt = unsafe { GetWindowTextW(hwnd, &mut title) };
    let nc = unsafe { GetClassNameW(hwnd, &mut class) };
    if nc <= 0 {
        return Ok(None);
    }
    if nt >= 1024 || nc >= 1024 {
        return Err(WindowCaptureError::BudgetExceeded(
            "window title/class truncated",
        ));
    }
    let title = String::from_utf16(&title[..nt.max(0) as usize])
        .map_err(|_| WindowCaptureError::InvalidFrame("window title UTF16"))?;
    let class = String::from_utf16(&class[..nc as usize])
        .map_err(|_| WindowCaptureError::InvalidFrame("window class UTF16"))?;
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    // OBS's ms_get_window_exe rejects the current process. Apart from keeping
    // the picker useful this prevents a window-capture feedback loop in Sky Live.
    if pid == 0 || pid == std::process::id() {
        return Ok(None);
    }
    let process = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => Process(h),
        Err(_) => return Ok(None),
    };
    let mut image = [0u16; 32768];
    let mut len = image.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(image.as_mut_ptr()),
            &mut len,
        )
    }
    .is_err()
    {
        return Ok(None);
    }
    let full = String::from_utf16(&image[..len as usize])
        .map_err(|_| WindowCaptureError::InvalidFrame("executable UTF16"))?;
    let executable = full.rsplit(['\\', '/']).next().unwrap_or("").to_owned();
    if title.len() > 1024 || class.len() > 1024 || executable.len() > 1024 {
        return Err(WindowCaptureError::BudgetExceeded("window UTF8 metadata"));
    }
    if executable.eq_ignore_ascii_case("explorer.exe") && title.is_empty() {
        return Ok(None);
    }
    let mut rect = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut rect) }.is_err() {
        return Ok(None);
    }
    let (Ok(width), Ok(height)) = (
        u32::try_from(i64::from(rect.right) - i64::from(rect.left)),
        u32::try_from(i64::from(rect.bottom) - i64::from(rect.top)),
    ) else {
        return Ok(None);
    };
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > MAX_DISPLAY_PIXELS
    {
        return Ok(None);
    }
    let target = WindowTarget::Windows {
        title: title.clone(),
        class,
        executable: executable.clone(),
    };
    // Revalidate native metadata before returning it across the owner boundary.
    WindowCaptureBinding::new(
        target.clone(),
        skylive_core::WindowMatchPriority::Title,
        true,
        true,
        width,
        height,
    )
    .map_err(|_| WindowCaptureError::InvalidFrame("native window identity"))?;
    Ok(Some(Window {
        hwnd,
        info: WindowInfo {
            full_window_size: full_window_size(hwnd),
            target,
            name: format!("[{executable}]: {title}"),
            width,
            height,
            minimized: unsafe { IsIconic(hwnd) }.as_bool(),
        },
    }))
}
struct Process(HANDLE);
// A process kernel handle may be moved between threads; ownership remains
// exclusive and Drop closes it exactly once on the capture owner.
unsafe impl Send for Process {}
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
struct WindowDc {
    hwnd: HWND,
    dc: HDC,
}
impl WindowDc {
    fn new(hwnd: HWND) -> Result<Self, WindowCaptureError> {
        let dc = unsafe { GetDC(hwnd) };
        if dc.is_invalid() {
            Err(last_error("GetDC(window)"))
        } else {
            Ok(Self { hwnd, dc })
        }
    }
}
impl Drop for WindowDc {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseDC(self.hwnd, self.dc);
        }
    }
}
struct DpiGuard(DPI_AWARENESS_CONTEXT);
impl DpiGuard {
    fn new() -> Result<Self, WindowCaptureError> {
        let old = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE) };
        if old.0.is_null() {
            Err(last_error("SetThreadDpiAwarenessContext"))
        } else {
            Ok(Self(old))
        }
    }
}
impl Drop for DpiGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = SetThreadDpiAwarenessContext(self.0);
        }
    }
}
struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut c_void,
}
impl Dib {
    fn new(screen: HDC, width: u32, height: u32) -> Result<Self, WindowCaptureError> {
        let dc = unsafe { CreateCompatibleDC(screen) };
        if dc.is_invalid() {
            return Err(last_error("CreateCompatibleDC"));
        }
        let mut result = Self {
            dc,
            bitmap: HBITMAP::default(),
            old: HGDIOBJ::default(),
            bits: ptr::null_mut(),
        };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        result.bitmap = unsafe {
            CreateDIBSection(
                dc,
                &info,
                DIB_RGB_COLORS,
                &mut result.bits,
                HANDLE::default(),
                0,
            )
        }
        .map_err(|e| backend_error("CreateDIBSection", e))?;
        if result.bits.is_null() {
            return Err(WindowCaptureError::InvalidFrame(
                "CreateDIBSection returned null pixels",
            ));
        }
        result.old = unsafe { SelectObject(dc, result.bitmap) };
        if result.old.is_invalid() || result.old.0 as isize == -1 {
            return Err(last_error("SelectObject"));
        }
        Ok(result)
    }
}
impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            if !self.old.is_invalid() && self.old.0 as isize != -1 {
                let _ = SelectObject(self.dc, self.old);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap);
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}

struct Icon(HICON);
impl Drop for Icon {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}
struct IconBitmaps(ICONINFO);
impl Drop for IconBitmaps {
    fn drop(&mut self) {
        unsafe {
            if !self.0.hbmColor.is_invalid() {
                let _ = DeleteObject(self.0.hbmColor);
            }
            if !self.0.hbmMask.is_invalid() {
                let _ = DeleteObject(self.0.hbmMask);
            }
        }
    }
}
fn draw_cursor(dc: HDC, rect: RECT) -> Result<(), WindowCaptureError> {
    let mut cursor = CURSORINFO {
        cbSize: size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        GetCursorInfo(&mut cursor).map_err(|e| backend_error("GetCursorInfo", e))?;
    }
    if cursor.flags != CURSOR_SHOWING {
        return Ok(());
    }
    let icon = Icon(
        unsafe { CopyIcon(HICON(cursor.hCursor.0)) }.map_err(|e| backend_error("CopyIcon", e))?,
    );
    let mut info = IconBitmaps(ICONINFO::default());
    unsafe {
        GetIconInfo(icon.0, &mut info.0).map_err(|e| backend_error("GetIconInfo", e))?;
    }
    let x = i64::from(cursor.ptScreenPos.x) - i64::from(info.0.xHotspot) - i64::from(rect.left);
    let y = i64::from(cursor.ptScreenPos.y) - i64::from(info.0.yHotspot) - i64::from(rect.top);
    let (Ok(x), Ok(y)) = (i32::try_from(x), i32::try_from(y)) else {
        return Ok(());
    };
    unsafe {
        DrawIconEx(
            dc,
            x,
            y,
            icon.0,
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::HBRUSH::default(),
            DI_NORMAL,
        )
    }
    .map_err(|e| backend_error("DrawIconEx", e))
}
fn backend_error(operation: &str, error: windows::core::Error) -> WindowCaptureError {
    let detail = format!("{operation}: {error}");
    if error.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_DEVICE_REMOVED
        || error.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_DEVICE_RESET
    {
        WindowCaptureError::DeviceLost(detail)
    } else {
        WindowCaptureError::Backend(detail)
    }
}
fn last_error(operation: &str) -> WindowCaptureError {
    backend_error(operation, windows::core::Error::from_win32())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_admission_precedes_any_window_or_pixel_allocation() {
        let limits = CaptureLimits {
            max_frame_bytes: 64,
            max_raw_bytes: 127,
            ..CaptureLimits::default()
        };
        assert!(admitted_bytes(4, 4, limits).is_err());
        assert_eq!(
            admitted_bytes(
                4,
                4,
                CaptureLimits {
                    max_raw_bytes: 128,
                    ..limits
                }
            )
            .unwrap(),
            64
        );
        assert!(admitted_bytes(0, 4, limits).is_err());
        assert_eq!(
            admitted_bytes(7680, 4320, CaptureLimits::default()).unwrap(),
            132710400
        );
    }
}

// OBS32.2.2 window-capture.c class selection; shared with interchange.
fn auto_uses_wgc(class: &str) -> bool {
    super::super::matching::auto_uses_wgc(class)
}
#[cfg(test)]
mod method_tests {
    #[test]
    fn auto_table_has_no_gdi_error_to_wgc_substitution() {
        assert!(super::auto_uses_wgc("Chrome_WidgetWin_1"));
        assert!(super::auto_uses_wgc("mozillaWindowClass"));
        assert!(super::auto_uses_wgc("XLMAIN"));
        assert!(!super::auto_uses_wgc("XLMAIN_child"));
        assert!(!super::auto_uses_wgc("FixtureClass"));
        assert!(super::auto_uses_wgc("WindowsWebview2Wrapper"));
        assert!(super::auto_uses_wgc("WINDOWSCLIENT"));
    }
}

fn full_window_size(hwnd: HWND) -> Option<(u32, u32)> {
    let mut rect = RECT::default();
    unsafe {
        windows::Win32::Graphics::Dwm::DwmGetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .ok()?;
    let width = u32::try_from(i64::from(rect.right) - i64::from(rect.left)).ok()?;
    let height = u32::try_from(i64::from(rect.bottom) - i64::from(rect.top)).ok()?;
    (width > 0
        && height > 0
        && width <= 8192
        && height <= 8192
        && u64::from(width) * u64::from(height) <= MAX_DISPLAY_PIXELS)
        .then_some((width, height))
}
