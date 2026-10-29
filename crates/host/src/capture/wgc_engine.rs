//! Genuine SDR WGC. No API is called before an explicit capture worker request.
//! Geometry/render policy derived from OBS libobs-winrt/winrt-capture.cpp,
//! GPL-2.0-or-later, revision recorded in doc/migration/display-capture-methods.md. No OBS binary dependency.
use crate::capture::{CaptureLimits,native_budget::{NativeCopyBudget,NativeCopyKind,NativeCopyLease}};
use crate::window_capture::wgc_geometry::{Region,backing_bytes,client_region,mapped_extent};
use skylive_core::{WindowCaptureBinding,DisplayCaptureBinding,WindowTarget,DisplayTarget,WindowMatchPriority};
use skylive_media::{
    AlphaMode, PackedFrameLayout, PackedFrameLimits, PackedPixelFormat, RgbaFrame, RowOrder,
    ingest_packed_frame,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use windows::{
    Foundation::{EventRegistrationToken, Metadata::ApiInformation, TypedEventHandler},
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Foundation::{E_ACCESSDENIED, E_POINTER, HMODULE, HWND, POINT, RECT, RO_E_CLOSED},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::*,
            Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_WAS_STILL_DRAWING,
                IDXGIDevice,
            },
            Gdi::{ClientToScreen,HMONITOR},
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
            RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
        },
        UI::WindowsAndMessaging::{GetClientRect, GetForegroundWindow, GetWindowThreadProcessId, IsIconic},
    },
    core::{AgileReference, HSTRING, IInspectable, Interface, factory},
};

#[derive(Debug)]
pub(crate) enum Error {Unavailable(&'static str),PermissionDenied,TargetNotFound,TargetMinimized,InvalidFrame(&'static str),BudgetExceeded(&'static str),Backend(String),DeviceLost(String)}
fn backend_error(operation:&str,error:windows::core::Error)->Error {
    let detail=format!("{operation}: {error}");
    if error.code()==E_ACCESSDENIED {return Error::PermissionDenied;}
    if error.code()==DXGI_ERROR_DEVICE_REMOVED || error.code()==DXGI_ERROR_DEVICE_RESET {Error::DeviceLost(detail)}else{Error::Backend(detail)}
}
impl From<crate::window_capture::WindowCaptureError> for Error {
    fn from(error:crate::window_capture::WindowCaptureError)->Self {
        use crate::window_capture::WindowCaptureError as W;
        match error {W::BudgetExceeded(s)=>Self::BudgetExceeded(s),W::InvalidFrame(s)=>Self::InvalidFrame(s),W::Unavailable(s)=>Self::Unavailable(s),W::PermissionDenied=>Self::PermissionDenied,W::TargetNotFound=>Self::TargetNotFound,W::TargetMinimized=>Self::TargetMinimized,W::Backend(s)=>Self::Backend(s),W::DeviceLost(s)=>Self::DeviceLost(s),other=>Self::Backend(other.to_string())}
    }
}
#[derive(Clone,Debug,PartialEq,Eq,Hash)]
pub(crate) enum Resource {
    Window {target:WindowTarget,priority:WindowMatchPriority,cursor:bool,client:bool},
    Monitor {target:DisplayTarget,cursor:bool},
}
impl Resource {
    pub(crate) fn window(binding:&WindowCaptureBinding)->Self {Self::Window {target:binding.target().clone(),priority:binding.priority(),cursor:binding.capture_cursor(),client:binding.client_area()}}
    pub(crate) fn monitor(binding:&DisplayCaptureBinding)->Self {Self::Monitor {target:binding.target().clone(),cursor:binding.capture_cursor()}}
    fn capture_cursor(&self)->bool {match self {Self::Window {cursor,..}|Self::Monitor {cursor,..}=>*cursor}}
    fn client_area(&self)->bool {matches!(self,Self::Window {client:true,..})}
    fn monitor_frame(&self)->bool {matches!(self,Self::Monitor {..})}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) enum NativeTarget {Window {handle:usize,pid:u32,process_creation_time_100ns:Option<u64>},Monitor {handle:usize}}
pub(crate) enum Outcome {Frame {image:RgbaFrame,generation:u64},Pending {generation:u64}}
const MAX_STATES: usize = 16;

#[derive(Default)]
struct Signal {
    arrived: AtomicBool,
    closed: AtomicBool,
    calls: AtomicUsize,
    delegates: AtomicUsize,
}
struct DelegateLife(Arc<Signal>);
impl DelegateLife {
    fn new(signal: &Arc<Signal>) -> Self {
        signal.delegates.fetch_add(1, Ordering::AcqRel);
        Self(signal.clone())
    }
    fn enter(&self) -> Call<'_> {
        self.0.calls.fetch_add(1, Ordering::AcqRel);
        Call(&self.0)
    }
}
impl Drop for DelegateLife {
    fn drop(&mut self) {
        self.0.delegates.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Call<'a>(&'a Signal);
impl Drop for Call<'_> {
    fn drop(&mut self) {
        self.0.calls.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Apartment;
impl Apartment {
    fn new() -> Result<Self, Error> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map_err(|e| backend_error("RoInitialize WGC", e))?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}
struct Device {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    // The generated interface itself is !Send. Keep a marshalled reference
    // instead of asserting that an arbitrary COM pointer can cross threads.
    winrt: AgileReference<IDirect3DDevice>,
}
impl Device {
    fn new() -> Result<Self, Error> {
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|e| backend_error("D3D11CreateDevice WGC", e))?;
        let device = device.ok_or(Error::InvalidFrame("WGC device absent"))?;
        let context = context.ok_or(Error::InvalidFrame("WGC context absent"))?;
        let dxgi: IDXGIDevice = device
            .cast()
            .map_err(|e| backend_error("WGC IDXGIDevice", e))?;
        let winrt: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .and_then(|v| v.cast())
            .map_err(|e| backend_error("WGC WinRT device", e))?;
        let winrt = AgileReference::new(&winrt)
            .map_err(|e| backend_error("WGC agile device reference", e))?;
        Ok(Self {
            device,
            context,
            winrt,
        })
    }
}
pub(crate) struct Engine {
    states: HashMap<Resource, State>,
    retired: Vec<State>,
    device: Option<Device>,
    apartment: Option<Apartment>,
    budget: NativeCopyBudget,
    next_generation: u64,
}
impl Engine {
    pub(crate) fn new(budget: NativeCopyBudget) -> Self {
        Self {
            states: HashMap::new(),
            retired: Vec::new(),
            device: None,
            apartment: None,
            budget,
            next_generation: 0,
        }
    }
    pub(crate) fn retain(&mut self, bindings: &[Resource], reset: bool) {
        let gone: Vec<_> = self
            .states
            .keys()
            .filter(|key| reset || !bindings.contains(key))
            .cloned()
            .collect();
        for key in gone {
            self.retire_key(&key);
        }
        self.reap();
    }
    fn invalidate_device(&mut self) {
        for (_, mut state) in self.states.drain() {
            state.begin_retire();
            self.retired.push(state);
        }
        self.device = None;
        self.reap();
    }
    fn retire_key(&mut self, key: &Resource) {
        if let Some(mut state) = self.states.remove(key) {
            state.begin_retire();
            self.retired.push(state);
        }
    }
    /// True means resources remain charged; revocation is not an acknowledgement.
    pub(crate) fn reap(&mut self) -> bool {
        for state in &mut self.retired {
            state.begin_retire();
        }
        self.retired.retain(|s| !s.quiescent());
        !self.retired.is_empty()
    }
    pub(crate) fn capture(&mut self,key:Resource,target:NativeTarget,limits:CaptureLimits)->Result<Outcome,Error>{
        if !matches!((&key,target),(Resource::Window {..},NativeTarget::Window {..}) | (Resource::Monitor {..},NativeTarget::Monitor {..})) {
            return Err(Error::InvalidFrame("WGC resource/native target kinds disagree"));
        }
        if match target {NativeTarget::Window {handle,..}|NativeTarget::Monitor {handle}=>handle==0} {
            return Err(Error::TargetNotFound);
        }
        self.reap();
        if self.states.get(&key).is_some_and(|s|s.target!=target || s.signal.closed.load(Ordering::Acquire)) {
            self.retire_key(&key);self.reap();
        }
        if !self.states.contains_key(&key) {
            if self.states.len() + self.retired.len() >= MAX_STATES {
                return Err(Error::BudgetExceeded(
                    "WGC active and retiring session count",
                ));
            }
            // Never overlap a replacement's backing with its unacknowledged old
            // pool merely because callbacks were revoked.
            if self.retired.iter().any(|s| s.key == key) {
                return Err(Error::Unavailable("previous WGC session is still retiring"));
            }
            if self.apartment.is_none() {
                self.apartment = Some(Apartment::new()?);
            }
            if !ApiInformation::IsApiContractPresentByMajor(&HSTRING::from("Windows.Foundation.UniversalApiContract"),8)
                .map_err(|e|backend_error("WGC UniversalApiContract8",e))? {
                return Err(Error::Unavailable("WGC direct window/monitor capture requires Windows10 1903 or newer"));
            }
            if !GraphicsCaptureSession::IsSupported()
                .map_err(|e| backend_error("WGC IsSupported", e))?
            {
                return Err(Error::Unavailable(
                    "Windows Graphics Capture is unsupported on this machine",
                ));
            }
            if self.device.is_none() {
                self.device = Some(Device::new()?);
            }
            let generation = self
                .next_generation
                .checked_add(1)
                .ok_or(Error::BudgetExceeded("WGC generation exhausted"))?;
            let state = State::open(
                key.clone(),
                target,
                generation,
                self.device.as_ref().expect("admitted WGC device"),
                &self.budget,
                limits,
            );
            let state = match state {
                Ok(state) => state,
                Err(error) => {
                    if matches!(&error, Error::DeviceLost(_)) {
                        self.invalidate_device();
                    }
                    return Err(error);
                }
            };
            self.next_generation = generation;
            self.states.insert(key.clone(), state);
        }
        let state = self.states.get_mut(&key).expect("admitted WGC state");
        let result = state.update_cursor_visibility()
            .and_then(|()| state.capture(self.device.as_ref().expect("WGC device"), limits));
        if result
            .as_ref()
            .is_err_and(|error| matches!(error, Error::DeviceLost(_)))
        {
            self.invalidate_device();
        } else if result.is_err() {
            self.retire_key(&key);
            self.reap();
        }
        result
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        for (_, mut state) in self.states.drain() {
            state.begin_retire();
            self.retired.push(state);
        }
        // This destructor runs on CaptureWorker, never a GUI/SDK callback.
        // A non-returning SDK close remains an occupied owner, not false Closed.
        while self.reap() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
type FrameHandler = TypedEventHandler<Direct3D11CaptureFramePool, IInspectable>;
type ClosedHandler = TypedEventHandler<GraphicsCaptureItem, IInspectable>;
struct State {
    key: Resource,
    target: NativeTarget,
    generation: u64,
    size: (u32, u32),
    context: ID3D11DeviceContext,
    item: Option<GraphicsCaptureItem>,
    pool: Option<Direct3D11CaptureFramePool>,
    session: Option<GraphicsCaptureSession>,
    cursor_toggle_supported: bool,
    cursor_visible: bool,
    frame_token: Option<EventRegistrationToken>,
    closed_token: Option<EventRegistrationToken>,
    frame_handler: Option<AgileReference<FrameHandler>>,
    closed_handler: Option<AgileReference<ClosedHandler>>,
    signal: Arc<Signal>,
    staging: Option<ID3D11Texture2D>,
    staging_size: (u32, u32),
    pending: Option<Direct3D11CaptureFrame>,
    closing: bool,
    session_closed: bool,
    pool_closed: bool,
    _reservation: NativeCopyLease,
}
impl State {
    fn open(
        key: Resource,
        target: NativeTarget,
        generation: u64,
        d: &Device,
        budget: &NativeCopyBudget,
        limits: CaptureLimits,
    ) -> Result<Self, Error> {
        let interop: IGraphicsCaptureItemInterop =
            factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(|e| backend_error("WGC item factory", e))?;
        let created=match target {
            NativeTarget::Window {handle,..}=>unsafe {interop.CreateForWindow::<_,GraphicsCaptureItem>(HWND(handle as *mut _))},
            NativeTarget::Monitor {handle}=>unsafe {interop.CreateForMonitor::<_,GraphicsCaptureItem>(HMONITOR(handle as *mut _))},
        };
        let item: GraphicsCaptureItem = created.map_err(|e| {
            if e.code() == E_ACCESSDENIED {
                Error::PermissionDenied
            } else {
                backend_error("WGC CreateForWindow/CreateForMonitor", e)
            }
        })?;
        let size = item.Size().map_err(|e| backend_error("WGC item size", e))?;
        let (width, height) = (
            u32::try_from(size.Width).map_err(|_| Error::InvalidFrame("negative WGC width"))?,
            u32::try_from(size.Height).map_err(|_| Error::InvalidFrame("negative WGC height"))?,
        );
        let backing = backing_bytes(width, height).map_err(Error::from)?;
        let output = u64::from(width) * u64::from(height) * 4;
        if output > limits.max_frame_bytes as u64 || backing > limits.max_raw_bytes {
            return Err(Error::BudgetExceeded("WGC pool/staging logical backing"));
        }
        let reservation = budget
            .reserve(NativeCopyKind::Wgc, backing)
            .ok_or(Error::BudgetExceeded("WGC/DXGI combined native backing"))?;
        let winrt = d
            .winrt
            .resolve()
            .map_err(|e| backend_error("WGC resolve device reference", e))?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )
        .map_err(|e| backend_error("WGC CreateFreeThreaded", e))?;
        let signal = Arc::new(Signal::default());
        let mut state = Self {
            key,
            target,
            generation,
            size: (width, height),
            context: d.context.clone(),
            item: Some(item),
            pool: Some(pool),
            session: None,
            cursor_toggle_supported: false,
            cursor_visible: false,
            frame_token: None,
            closed_token: None,
            frame_handler: None,
            closed_handler: None,
            signal,
            staging: None,
            staging_size: (0, 0),
            pending: None,
            closing: false,
            session_closed: true,
            pool_closed: false,
            _reservation: reservation,
        };
        // Own the pool and its reservation BEFORE session construction. A
        // failed session factory still retires/retries pool Close on this owner.
        let session = state
            .pool()
            .CreateCaptureSession(state.item())
            .map_err(|e| backend_error("WGC CreateCaptureSession", e))?;
        state.session = Some(session);
        state.session_closed = false;
        // Later setup errors also retire delegates on this owner.
        let cursor = ApiInformation::IsPropertyPresent(
            &HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession"),
            &HSTRING::from("IsCursorCaptureEnabled"),
        )
        .map_err(|e| backend_error("WGC cursor capability", e))?;
        state.cursor_toggle_supported = cursor;
        state.cursor_visible = state.key.capture_cursor();
        if cursor {
            state
                .session()
                .SetIsCursorCaptureEnabled(state.key.capture_cursor())
                .map_err(|e| backend_error("WGC cursor setting", e))?;
        } else if !state.key.capture_cursor() {
            return Err(Error::Unavailable(
                "this WGC runtime cannot disable cursor capture",
            ));
        }
        let token = DelegateLife::new(&state.signal);
        let frame = FrameHandler::new(move |_, _| {
            let _call = token.enter();
            token.0.arrived.store(true, Ordering::Release);
            Ok(())
        });
        let frame_reference =
            AgileReference::new(&frame).map_err(|e| backend_error("WGC agile frame handler", e))?;
        state.frame_token = Some(
            state
                .pool()
                .FrameArrived(&frame)
                .map_err(|e| backend_error("WGC FrameArrived", e))?,
        );
        state.frame_handler = Some(frame_reference);
        let token = DelegateLife::new(&state.signal);
        let closed = ClosedHandler::new(move |_, _| {
            let _call = token.enter();
            token.0.closed.store(true, Ordering::Release);
            Ok(())
        });
        let closed_reference = AgileReference::new(&closed)
            .map_err(|e| backend_error("WGC agile closed handler", e))?;
        state.closed_token = Some(
            state
                .item()
                .Closed(&closed)
                .map_err(|e| backend_error("WGC Closed", e))?,
        );
        state.closed_handler = Some(closed_reference);
        state
            .session()
            .StartCapture()
            .map_err(|e| backend_error("WGC StartCapture", e))?;
        Ok(state)
    }
    /// Match OBS Window Capture: an enabled cursor is hidden while a window
    /// belonging to another process has foreground focus. Monitor capture
    /// keeps its authored cursor setting. A disappearing foreground window
    /// leaves the cursor enabled, as OBS does when either PID is unavailable.
    fn update_cursor_visibility(&mut self) -> Result<(), Error> {
        if !self.cursor_toggle_supported { return Ok(()); }
        let visible = match (&self.key, self.target) {
            (Resource::Window { cursor, .. }, NativeTarget::Window { pid, .. }) if *cursor => {
                let mut foreground_pid = 0;
                unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut foreground_pid)); }
                foreground_pid == 0 || pid == 0 || foreground_pid == pid
            }
            _ => self.key.capture_cursor(),
        };
        if self.cursor_visible != visible {
            self.session().SetIsCursorCaptureEnabled(visible)
                .map_err(|e| backend_error("WGC cursor visibility", e))?;
            self.cursor_visible = visible;
        }
        Ok(())
    }
    fn capture(
        &mut self,
        d: &Device,
        limits: CaptureLimits,
    ) -> Result<Outcome, Error> {
        if self.signal.closed.load(Ordering::Acquire) {
            return Err(Error::TargetNotFound);
        }
        // Drain the previous copy, then schedule the newest frame in this
        // same poll. Returning immediately on readback halves steady cadence.
        let ready = self.readback(d, limits)?;
        if self.pending.is_some() {
            return Ok(Outcome::Pending {
                generation: self.generation,
            });
        }
        let mut newest = None;
        if self.signal.arrived.swap(false, Ordering::AcqRel) {
            for _ in 0..2 {
                match self.pool().TryGetNextFrame() {
                    Ok(frame) => {
                        if let Some(old) = newest.replace(FrameClose(frame)) {
                            drop(old);
                        }
                    }
                    Err(e) if e.code() == E_POINTER => break,
                    Err(e) => return Err(backend_error("WGC TryGetNextFrame", e)),
                }
            }
        }
        let Some(frame) = newest else {
            return Ok(self.outcome(ready));
        };
        let content = frame
            .0
            .ContentSize()
            .map_err(|e| backend_error("WGC ContentSize", e))?;
        let content = (
            u32::try_from(content.Width).map_err(|_| Error::InvalidFrame("WGC content width"))?,
            u32::try_from(content.Height).map_err(|_| Error::InvalidFrame("WGC content height"))?,
        );
        if content != self.size {
            return Err(Error::Unavailable(
                "WGC capture resized or suspended; a new bounded session is required",
            ));
        }
        let surface = frame
            .0
            .Surface()
            .map_err(|e| backend_error("WGC Surface", e))?;
        let access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|e| backend_error("WGC surface access", e))?;
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }
            .map_err(|e| backend_error("WGC texture interface", e))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || desc.SampleDesc.Count != 1
            || desc.ArraySize != 1
            || desc.MipLevels != 1
            || desc.Width != self.size.0
            || desc.Height != self.size.1
        {
            return Err(Error::InvalidFrame(
                "unsupported WGC texture layout or stale texture extent",
            ));
        }
        let region = if self.key.client_area() {
            match self.target {NativeTarget::Window {handle,..}=>client_box(HWND(handle as *mut _),content)?,NativeTarget::Monitor {..}=>return Err(Error::InvalidFrame("monitor cannot use window client cropping"))}
        } else {
            Region {
                x: 0,
                y: 0,
                width: content.0,
                height: content.1,
            }
        };
        if self.staging_size != (region.width, region.height) {
            self.staging = None;
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: region.width,
                Height: region.height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            unsafe {
                d.device
                    .CreateTexture2D(&staging_desc, None, Some(&mut self.staging))
            }
            .map_err(|e| backend_error("WGC staging texture", e))?;
            self.staging_size = (region.width, region.height);
        }
        let staging = self
            .staging
            .as_ref()
            .ok_or(Error::InvalidFrame("WGC staging texture absent"))?;
        let crop = D3D11_BOX {
            left: region.x,
            top: region.y,
            front: 0,
            right: region.x + region.width,
            bottom: region.y + region.height,
            back: 1,
        };
        unsafe {
            d.context
                .CopySubresourceRegion(staging, 0, 0, 0, 0, &texture, 0, Some(&crop));
            d.context.Flush();
        }
        self.pending = Some(frame.into_inner());
        Ok(self.outcome(ready))
    }
    fn outcome(&self,ready:Option<RgbaFrame>)->Outcome {
        match ready {Some(image)=>Outcome::Frame {image,generation:self.generation},None=>Outcome::Pending {generation:self.generation}}
    }
    fn readback(&mut self, d: &Device, limits: CaptureLimits) -> Result<Option<RgbaFrame>, Error> {
        if self.pending.is_none() {
            return Ok(None);
        }
        let staging = self
            .staging
            .as_ref()
            .ok_or(Error::InvalidFrame("WGC pending staging absent"))?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        match unsafe {
            d.context.Map(
                staging,
                0,
                D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                Some(&mut mapped),
            )
        } {
            Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => return Ok(None),
            Err(e) => return Err(backend_error("WGC Map", e)),
            Ok(()) => {}
        }
        let guard = MapGuard {
            context: d.context.clone(),
            texture: staging.clone(),
        };
        let (width, height) = self.staging_size;
        let extent = mapped_extent(
            width,
            height,
            mapped.RowPitch as usize,
            limits.max_raw_bytes,
        ).map_err(Error::from)?;
        if mapped.pData.is_null() {
            return Err(Error::InvalidFrame("WGC null mapped pixels"));
        }
        // Actual pitch is unknown before Map. Validate the entire mapped read
        // extent independently; the earlier reservation was LOGICAL backing.
        let bytes = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), extent) };
        let image = ingest_packed_frame(
            bytes,
            PackedFrameLayout {
                width,
                height,
                stride_bytes: mapped.RowPitch as usize,
                format: PackedPixelFormat::Bgra8,
                row_order: RowOrder::TopDown,
                alpha: if self.key.monitor_frame(){AlphaMode::Opaque}else{AlphaMode::StoredPremultipliedSrgb},
            },
            PackedFrameLimits {
                max_input_bytes: limits.max_raw_bytes,
                max_output_bytes: limits.max_frame_bytes,
                max_pixel_visits: 2 * 33_554_432,
            },
        )
        .map_err(|e| match e {
            skylive_media::MediaError::BudgetExceeded => {
                Error::BudgetExceeded("WGC packed framebuffer ingestion")
            }
            _ => Error::InvalidFrame("WGC packed framebuffer ingestion"),
        })?;
        drop(guard);
        if let Some(frame) = self.pending.take() {
            let result = frame.Close();
            if let Err(e) = result {
                if e.code() != RO_E_CLOSED {
                    // A failed explicit Close is not retirement acknowledgement.
                    // Keep this lease charged for State::begin_retire to retry.
                    self.pending = Some(frame);
                    return Err(backend_error("WGC frame Close", e));
                }
            }
        }
        Ok(Some(image))
    }
    fn pool(&self) -> &Direct3D11CaptureFramePool {
        self.pool.as_ref().expect("active WGC pool")
    }
    fn item(&self) -> &GraphicsCaptureItem {
        self.item.as_ref().expect("active WGC item")
    }
    fn session(&self) -> &GraphicsCaptureSession {
        self.session.as_ref().expect("active WGC session")
    }
    fn begin_retire(&mut self) {
        self.closing = true;
        if let (Some(token), Some(pool)) = (self.frame_token, self.pool.as_ref()) {
            let result = pool.RemoveFrameArrived(token);
            if result.is_ok() || result.as_ref().is_err_and(|e| e.code() == RO_E_CLOSED) {
                self.frame_token = None;
                self.frame_handler = None;
            }
        }
        if let (Some(token), Some(item)) = (self.closed_token, self.item.as_ref()) {
            let result = item.RemoveClosed(token);
            if result.is_ok() || result.as_ref().is_err_and(|e| e.code() == RO_E_CLOSED) {
                self.closed_token = None;
                self.closed_handler = None;
            }
        }
        if self.pending.is_some() {
            let done = if let Some(texture) = &self.staging {
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                match unsafe {
                    self.context.Map(
                        texture,
                        0,
                        D3D11_MAP_READ,
                        D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                        Some(&mut mapped),
                    )
                } {
                    Ok(()) => {
                        unsafe { self.context.Unmap(texture, 0) };
                        true
                    }
                    Err(error)
                        if error.code() == DXGI_ERROR_DEVICE_REMOVED
                            || error.code() == DXGI_ERROR_DEVICE_RESET =>
                    {
                        true
                    }
                    Err(_) => false,
                }
            } else {
                false
            };
            if done {
                if let Some(frame) = self.pending.take() {
                    let result = frame.Close();
                    if result.as_ref().is_err_and(|e| e.code() != RO_E_CLOSED) {
                        self.pending = Some(frame);
                    }
                }
            }
        }
        if self.pending.is_some() {
            return;
        }
        if !self.session_closed {
            if let Some(session) = &self.session {
                let result = session.Close();
                self.session_closed =
                    result.is_ok() || result.as_ref().is_err_and(|e| e.code() == RO_E_CLOSED);
            }
        }
        if !self.pool_closed {
            if let Some(pool) = &self.pool {
                let result = pool.Close();
                self.pool_closed =
                    result.is_ok() || result.as_ref().is_err_and(|e| e.code() == RO_E_CLOSED);
            }
        }
        // Release COM event SOURCES after successful close/removal so their
        // delegate references can retire. Callbacks hold only Signal; reservation
        // and retirement accounting survive until all delegate/inflight tokens die.
        if self.session_closed {
            self.session = None;
        }
        if self.pool_closed {
            self.frame_token = None;
            self.frame_handler = None;
            self.pool = None;
        }
        if self.closed_token.is_none() {
            self.closed_handler = None;
            self.item = None;
        }
    }

    fn quiescent(&self) -> bool {
        self.closing
            && self.session_closed
            && self.pool_closed
            && self.pending.is_none()
            && self.signal.calls.load(Ordering::Acquire) == 0
            && self.signal.delegates.load(Ordering::Acquire) == 0
    }
}
impl Drop for State {
    fn drop(&mut self) {
        if !self.quiescent() {
            self.begin_retire();
            while !self.quiescent() {
                std::thread::sleep(Duration::from_millis(10));
                self.begin_retire();
            }
        }
    }
}
struct FrameClose(Direct3D11CaptureFrame);
impl FrameClose {
    fn into_inner(self) -> Direct3D11CaptureFrame {
        let this = std::mem::ManuallyDrop::new(self); /* Move exactly one owned COM reference; destructor is suppressed. */
        unsafe { std::ptr::read(&this.0) }
    }
}
impl Drop for FrameClose {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}
struct MapGuard {
    context: ID3D11DeviceContext,
    texture: ID3D11Texture2D,
}
impl Drop for MapGuard {
    fn drop(&mut self) {
        unsafe { self.context.Unmap(&self.texture, 0) }
    }
}
fn client_box(hwnd: HWND, size: (u32, u32)) -> Result<Region, Error> {
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return Err(Error::TargetMinimized);
    }
    let mut client = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client) }
        .map_err(|e| backend_error("WGC GetClientRect", e))?;
    let mut origin = POINT::default();
    if !unsafe { ClientToScreen(hwnd, &mut origin) }.as_bool() {
        return Err(Error::InvalidFrame("WGC client screen origin"));
    }
    let mut frame = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut frame as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .map_err(|e| backend_error("WGC extended frame bounds", e))?;
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return Err(Error::TargetMinimized);
    }
    let width = u32::try_from(i64::from(client.right) - i64::from(client.left))
        .map_err(|_| Error::InvalidFrame("WGC client width"))?;
    let height = u32::try_from(i64::from(client.bottom) - i64::from(client.top))
        .map_err(|_| Error::InvalidFrame("WGC client height"))?;
    client_region(
        size,
        (i64::from(origin.x), i64::from(origin.y), width, height),
        (i64::from(frame.left), i64::from(frame.top)),
    ).map_err(Error::from)
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    #[test]
    fn callback_and_delegate_retirement_are_both_required_and_counted_without_native_resources() {
        let signal = Arc::new(Signal::default());
        let token = DelegateLife::new(&signal);
        {
            let _call = token.enter();
            assert_eq!(signal.calls.load(Ordering::Acquire), 1);
            assert_eq!(signal.delegates.load(Ordering::Acquire), 1);
        }
        assert_eq!(signal.calls.load(Ordering::Acquire), 0);
        assert_eq!(signal.delegates.load(Ordering::Acquire), 1);
        drop(token);
        assert_eq!(signal.delegates.load(Ordering::Acquire), 0);
    }
}
