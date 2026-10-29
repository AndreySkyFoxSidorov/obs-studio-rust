//! SDR GDI capture. No API is called until the host worker invokes a method.
//!
//! Identity follows OBS duplicator-monitor-capture.c: EnumDisplayDevicesW with
//! EDD_GET_DEVICE_INTERFACE_NAME, resolved anew through the current GDI topology.
//! Pixel/cursor ownership follows dc-capture.c, with a top-down DIB instead.

use super::super::{
    CaptureBackend, CaptureCapabilities, CaptureError, CaptureLimits, DisplayCaptureBackend,
    DisplayCaptureOutcome, DisplayFrameMetadata, DisplayInfo,
};
use skylive_core::{
    DisplayCaptureBinding, DisplayCaptureMethod, DisplayTarget, MAX_DISPLAY_PIXELS,
};
use skylive_media::{
    AlphaMode, PackedFrameLayout, PackedFrameLimits, PackedPixelFormat, RgbaFrame, RowOrder,
    ingest_packed_frame,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use std::{ffi::c_void, mem::size_of, ptr};
#[path = "native_dxgi.rs"]
mod dxgi;
use windows::{
    Win32::{
        Foundation::{BOOL, HANDLE, HWND, LPARAM, RECT},
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DISPLAY_DEVICEW, DeleteDC, DeleteObject,
            EnumDisplayDevicesW, EnumDisplayMonitors, GdiFlush, GetDC, GetMonitorInfoW, HBITMAP,
            HDC, HGDIOBJ, HMONITOR, MONITORINFOEXW, ReleaseDC, SRCCOPY, SelectObject,
        },
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE,
                SetThreadDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                CURSOR_SHOWING, CURSORINFO, CopyIcon, DI_NORMAL, DestroyIcon, DrawIconEx,
                EDD_GET_DEVICE_INTERFACE_NAME, GetCursorInfo, GetIconInfo, HICON, ICONINFO,
                MONITORINFOF_PRIMARY,
            },
        },
    },
    core::PCWSTR,
};

pub(super) struct NativeBackend {
    dxgi: dxgi::DxgiBackend,
    wgc: super::super::wgc_engine::Engine,
    session: u64,
    epoch: u64,
    fallbacks: HashMap<String, (Instant, String)>,
}
impl NativeBackend {
    pub(super) fn with_budget(budget: super::super::native_budget::NativeCopyBudget) -> Self {
        Self {
            dxgi: dxgi::DxgiBackend::with_budget(budget.clone()),
            wgc: super::super::wgc_engine::Engine::new(budget),
            session: 0,
            epoch: 0,
            fallbacks: HashMap::new(),
        }
    }
}

impl CaptureBackend for NativeBackend {
    fn reap_native_resources(&mut self) -> bool {
        self.dxgi.reap() | self.wgc.reap()
    }
    fn capabilities(&self) -> CaptureCapabilities {
        CaptureCapabilities {
            available: true,
            backend: "windows-display-capture",
            cursor: true,
            hdr: false,
            note: Some(
                "SDR Auto/DXGI Desktop Duplication with cursor, WGC monitor capture, or GDI compatibility. WGC requires Windows 10 1903 or newer; cursor toggling depends on runtime support. Auto reports DXGI failures before falling back to GDI; ordinary timeout never falls back.",
            ),
        }
    }
    fn enumerate(&mut self, limits: CaptureLimits) -> Result<Vec<DisplayInfo>, CaptureError> {
        let _dpi = DpiGuard::new()?;
        Ok(monitors(limits)?.into_iter().map(|m| m.info).collect())
    }
    fn capture(
        &mut self,
        binding: &DisplayCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError> {
        match self.capture_display(binding, limits)? {
            DisplayCaptureOutcome::Frame { image, .. } => Ok(image),
            DisplayCaptureOutcome::NoNewFrame | DisplayCaptureOutcome::Pending { .. } => Err(CaptureError::Unavailable(
                "display has no new frame; use the typed capture_display port",
            )),
        }
    }
    fn retain_display_bindings(
        &mut self,
        session: u64,
        epoch: u64,
        bindings: &[DisplayCaptureBinding],
    ) {
        let reset = self.session != session || self.epoch != epoch;
        if reset {
            self.fallbacks.clear();
            self.session = session;
            self.epoch = epoch;
        }
        self.dxgi.retain(session, epoch, bindings);
        let wgc: Vec<_> = bindings.iter().filter(|binding| binding.method() == DisplayCaptureMethod::Wgc)
            .map(super::super::wgc_engine::Resource::monitor).collect();
        self.wgc.retain(&wgc, reset);
        self.fallbacks.retain(|id,_|bindings.iter().any(|binding|binding.method()==DisplayCaptureMethod::Auto&&matches!(binding.target(),DisplayTarget::Windows {device_id} if device_id.eq_ignore_ascii_case(id))));
    }
    fn capture_display(
        &mut self,
        binding: &DisplayCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<DisplayCaptureOutcome, CaptureError> {
        let make_gdi = |this: &mut Self, reason: Option<String>| {
            this.capture_gdi(binding, limits)
                .map(|image| DisplayCaptureOutcome::Frame {
                    image,
                    metadata: Some(DisplayFrameMetadata {
                        backend: DisplayCaptureBackend::Gdi,
                        fallback_reason: reason,
                        generation: None,
                    }),
                })
        };
        if binding.method() == DisplayCaptureMethod::Gdi {
            return make_gdi(self, None);
        }
        let DisplayTarget::Windows { device_id } = binding.target() else {
            return Err(CaptureError::Unavailable(
                "Windows capture requires a Windows display target",
            ));
        };
        let key = device_id.to_ascii_lowercase();
        if binding.method() == DisplayCaptureMethod::Auto {
            if let Some((_, reason)) = self
                .fallbacks
                .get(&key)
                .filter(|(until, _)| Instant::now() < *until)
                .cloned()
            {
                return make_gdi(self, Some(reason));
            }
        }
        let _dpi = DpiGuard::new()?;
        let monitor=monitors(limits)?.into_iter().find(|monitor|matches!(&monitor.info.target,DisplayTarget::Windows {device_id:id} if id.eq_ignore_ascii_case(device_id)))
            .ok_or_else(||CaptureError::TargetNotFound(binding.target().clone()))?;
        if binding.method() == DisplayCaptureMethod::Wgc {
            use super::super::wgc_engine::{Resource,NativeTarget,Outcome,Error};
            return self.wgc.capture(Resource::monitor(binding),NativeTarget::Monitor {handle:monitor.handle.0 as usize},limits)
                .map(|outcome| match outcome {
                    Outcome::Frame {image,generation} => DisplayCaptureOutcome::Frame {image,metadata:Some(DisplayFrameMetadata {
                        backend:DisplayCaptureBackend::Wgc,fallback_reason:None,generation:Some(generation) })},
                    Outcome::Pending {generation} => DisplayCaptureOutcome::Pending {generation},
                }).map_err(|error|match error {
                    Error::Unavailable(detail)=>CaptureError::Unavailable(detail),
                    Error::PermissionDenied=>CaptureError::PermissionDenied,
                    Error::TargetNotFound=>CaptureError::TargetNotFound(binding.target().clone()),
                    Error::TargetMinimized=>CaptureError::Unavailable("WGC monitor is suspended"),
                    Error::InvalidFrame(detail)=>CaptureError::InvalidFrame(detail),
                    Error::BudgetExceeded(detail)=>CaptureError::BudgetExceeded(detail),
                    Error::Backend(detail)|Error::DeviceLost(detail)=>CaptureError::Backend(detail),
                });
        }
        match self.dxgi.capture(binding, &monitor, limits) {
            Ok(outcome) => {
                self.fallbacks.remove(&key);
                Ok(outcome)
            }
            Err(error) if binding.method() == DisplayCaptureMethod::Auto => {
                let reason =
                    format!("Auto fell back to GDI compatibility after DXGI failure: {error}");
                if reason.len() > 4096 {
                    return Err(CaptureError::BudgetExceeded(
                        "DXGI fallback diagnostic bytes",
                    ));
                }
                if self.fallbacks.len() < 16 || self.fallbacks.contains_key(&key) {
                    self.fallbacks.insert(
                        key,
                        (Instant::now() + Duration::from_secs(1), reason.clone()),
                    );
                }
                make_gdi(self, Some(reason))
            }
            Err(error) => Err(error),
        }
    }
}
impl NativeBackend {
    fn capture_gdi(
        &mut self,
        binding: &DisplayCaptureBinding,
        limits: CaptureLimits,
    ) -> Result<RgbaFrame, CaptureError> {
        let DisplayTarget::Windows { device_id } = binding.target() else {
            return Err(CaptureError::Unavailable(
                "This display target requires a different operating system",
            ));
        };
        let _dpi = DpiGuard::new()?;
        let monitor = monitors(limits)?
            .into_iter()
            .find(|m| match &m.info.target {
                DisplayTarget::Windows {
                    device_id: candidate,
                } => candidate.eq_ignore_ascii_case(device_id),
                _ => false,
            })
            .ok_or_else(|| CaptureError::TargetNotFound(binding.target().clone()))?;
        // Binding dimensions are the authored transform basis, never a request
        // to stretch a changed monitor resolution to its previous size.
        let (width, height) = (monitor.info.width, monitor.info.height);
        let bytes = admitted_bytes(width, height, limits)?;
        let screen = ScreenDc::new()?;
        let dib = Dib::new(screen.0, width, height)?;
        unsafe {
            BitBlt(
                dib.dc,
                0,
                0,
                width as i32,
                height as i32,
                screen.0,
                monitor.rect.left,
                monitor.rect.top,
                SRCCOPY | CAPTUREBLT,
            )
            .map_err(|e| backend_error("BitBlt", e))?;
            if binding.capture_cursor() {
                // The cursor can disappear or change while its icon is being
                // copied. OBS treats cursor drawing as optional; preserve the
                // successfully captured display frame when it fails.
                let _ = draw_cursor(dib.dc, monitor.rect);
            }
            if !GdiFlush().as_bool() {
                return Err(last_error("GdiFlush"));
            }
            // DIB is live, selected into this DC, and GDI writes have completed.
            let raw = std::slice::from_raw_parts(dib.bits.cast::<u8>(), bytes);
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
            .map_err(|_| CaptureError::InvalidFrame("GDI packed frame ingestion"))
        }
    }
}

fn admitted_bytes(width: u32, height: u32, limits: CaptureLimits) -> Result<usize, CaptureError> {
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > MAX_DISPLAY_PIXELS
    {
        return Err(CaptureError::InvalidFrame(
            "Display dimensions exceed 8192-edge/32M-pixel limits",
        ));
    }
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|v| v.checked_mul(4))
        .ok_or(CaptureError::BudgetExceeded("Display frame size overflow"))?;
    if bytes > limits.max_frame_bytes
        || bytes
            .checked_mul(2)
            .is_none_or(|v| v > limits.max_raw_bytes)
    {
        return Err(CaptureError::BudgetExceeded(
            "Display DIB and RGBA working buffers",
        ));
    }
    Ok(bytes)
}

struct Monitor {
    info: DisplayInfo,
    rect: RECT,
    handle: HMONITOR,
    device_name: String,
}
struct Enumeration {
    values: Vec<Monitor>,
    limit: usize,
    error: Option<CaptureError>,
}

fn monitors(limits: CaptureLimits) -> Result<Vec<Monitor>, CaptureError> {
    let mut state = Enumeration {
        values: Vec::new(),
        limit: limits.max_displays.min(64),
        error: None,
    };
    state
        .values
        .try_reserve_exact(state.limit)
        .map_err(|_| CaptureError::BudgetExceeded("Display enumeration allocation"))?;
    let completed = unsafe {
        EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(monitor_callback),
            LPARAM((&mut state as *mut Enumeration) as isize),
        )
    };
    if let Some(error) = state.error {
        return Err(error);
    }
    if !completed.as_bool() {
        return Err(last_error("EnumDisplayMonitors"));
    }
    Ok(state.values)
}

unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // Win32 invokes this callback synchronously while the Enumeration is alive.
    let state = unsafe { &mut *(data.0 as *mut Enumeration) };
    match describe_monitor(monitor, state) {
        Ok(()) => BOOL(1),
        Err(error) => {
            state.error = Some(error);
            BOOL(0)
        }
    }
}

fn describe_monitor(monitor: HMONITOR, state: &mut Enumeration) -> Result<(), CaptureError> {
    if state.values.len() >= state.limit {
        return Err(CaptureError::BudgetExceeded("Display enumeration limit"));
    }
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    unsafe {
        if !GetMonitorInfoW(monitor, &mut info.monitorInfo).as_bool() {
            return Err(last_error("GetMonitorInfoW"));
        }
    }
    let mut device = DISPLAY_DEVICEW {
        cb: size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    unsafe {
        if !EnumDisplayDevicesW(
            PCWSTR(info.szDevice.as_ptr()),
            0,
            &mut device,
            EDD_GET_DEVICE_INTERFACE_NAME,
        )
        .as_bool()
        {
            return Err(last_error("EnumDisplayDevicesW monitor interface"));
        }
    }
    let device_id = utf16_z(&device.DeviceID)?;
    if device_id.is_empty() {
        return Err(CaptureError::Backend(
            "Display has no monitor device interface identity".into(),
        ));
    }
    if state.values.iter().any(|m| matches!(&m.info.target, DisplayTarget::Windows { device_id: id } if id.eq_ignore_ascii_case(&device_id))) {
        return Err(CaptureError::Backend("Multiple active displays share the same monitor interface identity".into()));
    }
    let rect = info.monitorInfo.rcMonitor;
    let width = u32::try_from(i64::from(rect.right) - i64::from(rect.left))
        .map_err(|_| CaptureError::InvalidFrame("Invalid monitor rectangle"))?;
    let height = u32::try_from(i64::from(rect.bottom) - i64::from(rect.top))
        .map_err(|_| CaptureError::InvalidFrame("Invalid monitor rectangle"))?;
    if width == 0 || height == 0 {
        return Err(CaptureError::InvalidFrame("Empty monitor rectangle"));
    }
    let mut name = utf16_z(&device.DeviceString)?;
    if name.is_empty() {
        name = utf16_z(&info.szDevice)?;
    }
    state.values.push(Monitor {
        handle: monitor,
        device_name: utf16_z(&info.szDevice)?,
        rect,
        info: DisplayInfo {
            target: DisplayTarget::Windows { device_id },
            name,
            width,
            height,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        },
    });
    Ok(())
}

fn utf16_z(value: &[u16]) -> Result<String, CaptureError> {
    String::from_utf16(&value[..value.iter().position(|v| *v == 0).unwrap_or(value.len())]).map_err(
        |_| CaptureError::Backend("Windows returned invalid UTF-16 display metadata".into()),
    )
}

struct DpiGuard(DPI_AWARENESS_CONTEXT);
impl DpiGuard {
    fn new() -> Result<Self, CaptureError> {
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
struct ScreenDc(HDC);
impl ScreenDc {
    fn new() -> Result<Self, CaptureError> {
        let dc = unsafe { GetDC(HWND::default()) };
        if dc.is_invalid() {
            Err(last_error("GetDC"))
        } else {
            Ok(Self(dc))
        }
    }
}
impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseDC(HWND::default(), self.0);
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
    fn new(screen: HDC, width: u32, height: u32) -> Result<Self, CaptureError> {
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
            return Err(CaptureError::InvalidFrame(
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
fn draw_cursor(dc: HDC, rect: RECT) -> Result<(), CaptureError> {
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
fn backend_error(operation: &str, error: windows::core::Error) -> CaptureError {
    CaptureError::Backend(format!(
        "{operation}: HRESULT 0x{:08X}; {error}",
        error.code().0 as u32
    ))
}
fn last_error(operation: &str) -> CaptureError {
    backend_error(operation, windows::core::Error::from_win32())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_admission_counts_dib_and_owned_output_before_allocating() {
        let mut limits = CaptureLimits {
            max_frame_bytes: 64,
            max_raw_bytes: 127,
            ..CaptureLimits::default()
        };
        assert!(matches!(
            admitted_bytes(4, 4, limits),
            Err(CaptureError::BudgetExceeded(_))
        ));
        limits.max_raw_bytes = 128;
        assert_eq!(admitted_bytes(4, 4, limits).unwrap(), 64);
        assert!(admitted_bytes(8192, 2049, limits).is_err());
        assert!(admitted_bytes(0, 1, limits).is_err());
    }
    #[test]
    fn utf16_metadata_preserves_exact_identity_and_rejects_invalid_units() {
        assert_eq!(utf16_z(&[65, 92, 98, 0, 100]).unwrap(), "A\\b");
        assert!(utf16_z(&[0xd800, 0]).is_err());
    }
    #[test]
    fn native_5k_6k_and_8k_admission_uses_capture_pixels_without_allocating_frames() {
        let limits = CaptureLimits::default();
        for (width, height, bytes) in [
            (5120, 2880, 58_982_400),
            (6016, 3384, 81_432_576),
            (7680, 4320, 132_710_400),
            (8192, 4096, 134_217_728),
        ] {
            assert_eq!(admitted_bytes(width, height, limits).unwrap(), bytes);
            let tight = CaptureLimits {
                max_raw_bytes: bytes * 2 - 1,
                ..limits
            };
            assert!(matches!(
                admitted_bytes(width, height, tight),
                Err(CaptureError::BudgetExceeded(_))
            ));
        }
        assert!(matches!(
            admitted_bytes(8192, 4097, limits),
            Err(CaptureError::InvalidFrame(_))
        ));
        assert!(matches!(
            admitted_bytes(8193, 1, limits),
            Err(CaptureError::InvalidFrame(_))
        ));
    }
}
