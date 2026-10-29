//! Real SDR Desktop Duplication on the capture owner. No API in construction.
//! Readback is polled with DO_NOT_WAIT; ordinary timeout never means fallback.
use super::super::super::{
    CaptureError, CaptureLimits, DisplayCaptureBackend, DisplayCaptureOutcome, DisplayFrameMetadata,
};
use super::{Monitor, admitted_bytes, backend_error};
use skylive_core::{DisplayCaptureBinding, DisplayTarget};
use skylive_media::RgbaFrame;
use std::{
    collections::HashMap,
    thread::{self, ThreadId},
};
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::{
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
                D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice,
                ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
            },
            Dxgi::{
                Common::{
                    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_IDENTITY,
                    DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180,
                    DXGI_MODE_ROTATION_ROTATE270, DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC,
                },
                CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT,
                DXGI_ERROR_WAS_STILL_DRAWING, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1,
                IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
            },
        },
    },
    core::Interface,
};

// Auto-off, Auto-on, DXGI-off, DXGI-on: bounded audiences, not native devices.
fn audience(binding:&DisplayCaptureBinding)->Option<usize>{
    let offset=match binding.method(){skylive_core::DisplayCaptureMethod::Auto=>0,skylive_core::DisplayCaptureMethod::Dxgi=>2,_=>return None};
    Some(offset+usize::from(binding.capture_cursor()))
}
const MAX_STATES: usize = 16;
const MAX_STAGING_BYTES: usize = 256 * 1024 * 1024;
pub(super) struct DxgiBackend {
    session: u64,
    epoch: u64,
    states: HashMap<String, State>,
    audiences: HashMap<String,u8>,
    budget: crate::capture::native_budget::NativeCopyBudget,
    retired: Vec<State>,
}
impl DxgiBackend {
    pub(super) fn with_budget(budget: crate::capture::native_budget::NativeCopyBudget) -> Self {
        Self {
            session: 0,
            epoch: 0,
            states: HashMap::new(),
            audiences: HashMap::new(),
            budget,
            retired: Vec::new(),
        }
    }
    pub(super) fn retain(&mut self, session: u64, epoch: u64, bindings: &[DisplayCaptureBinding]) {
        let reset = self.session != session || self.epoch != epoch;
        self.session = session;
        self.epoch = epoch;
        self.audiences.clear();
        for binding in bindings {
            let Some(index)=audience(binding) else {continue;};
            if let DisplayTarget::Windows {device_id}=binding.target() {
                *self.audiences.entry(device_id.to_ascii_lowercase()).or_default() |= 1u8<<index;
            }
        }
        let gone:Vec<_>=self.states.keys().filter(|id|reset || !self.audiences.contains_key(*id)).cloned().collect();
        for key in gone {
            if let Some(state) = self.states.remove(&key) {
                self.retired.push(state);
            }
        }
        self.reap();
    }
    pub(super) fn reap(&mut self) -> bool {
        self.retired.retain_mut(|state| !state.retire_ready());
        !self.retired.is_empty()
    }
    pub(super) fn capture(
        &mut self,
        binding: &DisplayCaptureBinding,
        monitor: &Monitor,
        limits: CaptureLimits,
    ) -> Result<DisplayCaptureOutcome, CaptureError> {
        let DisplayTarget::Windows { device_id } = binding.target() else {
            return Err(CaptureError::Unavailable(
                "DXGI requires a Windows display target",
            ));
        };
        admitted_bytes(monitor.info.width, monitor.info.height, limits)?;
        let key = device_id.to_ascii_lowercase();
        let audience=audience(binding).ok_or(CaptureError::Unavailable("selected method is not DXGI"))?;
        let audiences=self.audiences.get(&key).copied().unwrap_or(0) | (1u8<<audience);
        if self.states.get(&key).is_some_and(|state| {
            state.monitor != monitor.handle.0 as usize
                || state.audiences != audiences
                || state.desktop_size != (monitor.info.width, monitor.info.height)
        }) {
            if let Some(state) = self.states.remove(&key) {
                self.retired.push(state);
            }
            self.reap();
        }
        if !self.states.contains_key(&key) {
            self.reap();
            // DuplicateOutput permits ONE duplicator for an output per process.
            // Reconfiguration waits for its actual pending-copy retirement;
            // this normal wait is not a reason for Auto to fall back to GDI.
            if self.retired.iter().any(|state|state.identity==key || state.monitor==monitor.handle.0 as usize) {
                return Ok(DisplayCaptureOutcome::NoNewFrame);
            }
            if self.states.len() + self.retired.len() >= MAX_STATES {
                return Err(CaptureError::BudgetExceeded("DXGI native state count"));
            }
            self.states.insert(
                key.clone(),
                State::open(monitor, limits, self.budget.clone(),audiences)?,
            );
        }
        let other_bytes = self
            .states
            .iter()
            .filter(|(id, _)| **id != key)
            .map(|(_, state)| state.staging_bytes)
            .chain(self.retired.iter().map(|state| state.staging_bytes))
            .sum::<usize>();
        let result = self
            .states
            .get_mut(&key)
            .expect("admitted DXGI state")
            .capture(
                monitor,
                limits,
                MAX_STAGING_BYTES.saturating_sub(other_bytes),
                audience, binding.capture_cursor(),
            );
        if result.is_err() {
            if let Some(state) = self.states.remove(&key) {
                self.retired.push(state);
            }
            self.reap();
        }
        result
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rotation {
    Identity,
    Clockwise90,
    Half,
    Clockwise270,
}
impl Rotation {
    fn read(value: DXGI_MODE_ROTATION) -> Result<Self, CaptureError> {
        match value {
            DXGI_MODE_ROTATION_UNSPECIFIED | DXGI_MODE_ROTATION_IDENTITY => Ok(Self::Identity),
            DXGI_MODE_ROTATION_ROTATE90 => Ok(Self::Clockwise90),
            DXGI_MODE_ROTATION_ROTATE180 => Ok(Self::Half),
            DXGI_MODE_ROTATION_ROTATE270 => Ok(Self::Clockwise270),
            _ => Err(CaptureError::InvalidFrame("unknown DXGI output rotation")),
        }
    }
    fn output_size(self, width: u32, height: u32) -> (u32, u32) {
        match self {
            Self::Clockwise90 | Self::Clockwise270 => (height, width),
            _ => (width, height),
        }
    }
}
struct State {
    owner: ThreadId,
    monitor: usize,
    identity: String,
    desktop_size: (u32, u32),
    rotation: Rotation,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: Option<ID3D11Texture2D>,
    staging_size: (u32, u32),
    staging_bytes: usize,
    copy_budget: crate::capture::native_budget::NativeCopyBudget,
    copy_reservation: Option<crate::capture::native_budget::NativeCopyLease>,
    pending: bool,
    pending_lease: Option<FrameLease>,
    audiences:u8,
    retain_clean:bool,
    cursor_enabled:bool,
    desktop_serial:u64,
    cursor_serial:u64,
    delivered:[u64;4],
    cursor:crate::capture::dxgi_cursor::Pointer,
    pending_pointer:Option<crate::capture::dxgi_cursor::Snapshot>,
    base:Option<crate::capture::dxgi_cursor::Base>,
    base_credit:Option<crate::capture::native_budget::NativeCopyLease>,
}
impl State {
    fn open(
        monitor: &Monitor,
        limits: CaptureLimits,
        copy_budget: crate::capture::native_budget::NativeCopyBudget,
        audiences:u8,
    ) -> Result<Self, CaptureError> {
        admitted_bytes(monitor.info.width, monitor.info.height, limits)?;
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }
            .map_err(|error| backend_error("CreateDXGIFactory1", error))?;
        let mut selected: Option<(IDXGIAdapter1, IDXGIOutput)> = None;
        'adapters: for a in 0..16 {
            let adapter = match unsafe { factory.EnumAdapters1(a) } {
                Ok(adapter) => adapter,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(backend_error("EnumAdapters1", error)),
            };
            for o in 0..64 {
                let output = match unsafe { adapter.EnumOutputs(o) } {
                    Ok(output) => output,
                    Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(error) => return Err(backend_error("EnumOutputs", error)),
                };
                let desc = unsafe { output.GetDesc() }
                    .map_err(|error| backend_error("IDXGIOutput::GetDesc", error))?;
                let name = super::utf16_z(&desc.DeviceName)?;
                if desc.AttachedToDesktop.as_bool()
                    && std::ptr::eq(desc.Monitor.0, monitor.handle.0)
                    && name.eq_ignore_ascii_case(&monitor.device_name)
                {
                    selected = Some((adapter, output));
                    break 'adapters;
                }
            }
        }
        let (adapter, output) =
            selected.ok_or_else(|| CaptureError::TargetNotFound(monitor.info.target.clone()))?;
        let desc = unsafe { output.GetDesc() }
            .map_err(|error| backend_error("IDXGIOutput::GetDesc", error))?;
        let rotation = Rotation::read(desc.Rotation)?;
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|error| backend_error("D3D11CreateDevice", error))?;
        let device = device.ok_or(CaptureError::InvalidFrame(
            "D3D11CreateDevice returned no device",
        ))?;
        let context = context.ok_or(CaptureError::InvalidFrame(
            "D3D11CreateDevice returned no context",
        ))?;
        let output: IDXGIOutput1 = output
            .cast()
            .map_err(|error| backend_error("IDXGIOutput1", error))?;
        let duplication = unsafe { output.DuplicateOutput(&device) }
            .map_err(|error| backend_error("DuplicateOutput", error))?;
        let duplication_desc = unsafe { duplication.GetDesc() };
        admitted_bytes(
            duplication_desc.ModeDesc.Width,
            duplication_desc.ModeDesc.Height,
            limits,
        )?;
        if duplication_desc.ModeDesc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(CaptureError::Unavailable(
                "DXGI duplication did not supply supported SDR BGRA8",
            ));
        }
        if rotation.output_size(
            duplication_desc.ModeDesc.Width,
            duplication_desc.ModeDesc.Height,
        ) != (monitor.info.width, monitor.info.height)
        {
            return Err(CaptureError::InvalidFrame(
                "DXGI output dimensions disagree with selected monitor rotation",
            ));
        }
        Ok(Self {
            owner: thread::current().id(),
            monitor: monitor.handle.0 as usize,
            identity: match &monitor.info.target {DisplayTarget::Windows {device_id}=>device_id.to_ascii_lowercase(),_=>return Err(CaptureError::InvalidFrame("DXGI monitor identity platform"))},
            desktop_size: (monitor.info.width, monitor.info.height),
            rotation,
            device,
            context,
            duplication,
            staging: None,
            staging_size: (0, 0),
            staging_bytes: 0,
            copy_budget,
            copy_reservation: None,
            pending: false,
            pending_lease: None,
            audiences, retain_clean:audiences.count_ones()>1 || audiences & 0b1010 != 0,
            cursor_enabled:audiences & 0b1010 != 0,
            desktop_serial:0,cursor_serial:0,delivered:[0;4],
            cursor:Default::default(),pending_pointer:None,base:None,base_credit:None,
        })
    }
    fn capture(
        &mut self, monitor:&Monitor, limits:CaptureLimits, staging_allowance:usize,
        audience:usize, include_cursor:bool,
    )->Result<DisplayCaptureOutcome,CaptureError>{
        if self.owner != thread::current().id() {
            return Err(CaptureError::Backend("DXGI state used from a different owner thread".into()));
        }
        // One duplication per physical output. Different Auto/DXGI and cursor
        // audiences receive the same clean acquired generation exactly once.
        // A solitary cursor-off audience keeps the original low-memory path.
        let ready=self.readback(limits)?;
        let mut raw=None;
        if let Some(image)=ready {
            if self.retain_clean {
                let pointer=self.pending_pointer.take().ok_or(CaptureError::InvalidFrame("DXGI pending desktop lost pointer snapshot"))?;
                let credit=self.base_credit.take().ok_or(CaptureError::InvalidFrame("DXGI pending clean-base admission lost"))?;
                self.base=Some(crate::capture::dxgi_cursor::Base::new(image,pointer,credit));
                self.desktop_serial=self.desktop_serial.checked_add(1).ok_or(CaptureError::IdentityExhausted)?;
                self.cursor_serial=self.cursor_serial.checked_add(1).ok_or(CaptureError::IdentityExhausted)?;
            }else{raw=Some(image);}
        }
        if self.pending {return self.deliver(raw,audience,include_cursor,limits);}
        let mut info=DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource:Option<IDXGIResource>=None;
        match unsafe {self.duplication.AcquireNextFrame(0,&mut info,&mut resource)} {
            Err(error) if error.code()==DXGI_ERROR_WAIT_TIMEOUT=>return self.deliver(raw,audience,include_cursor,limits),
            Err(error)=>return Err(backend_error("AcquireNextFrame",error)),Ok(())=>{},
        }
        let lease=FrameLease {duplication:self.duplication.clone(),armed:true};
        // Pointer metadata is independent of the acquired desktop texture.
        // If the shape is temporarily unavailable or malformed, publish the
        // clean desktop rather than dropping the entire capture. Resetting the
        // pointer state forces a fresh shape request on the next acquisition;
        // an old shape must not be drawn at the new position.
        let pointer=if self.cursor_enabled {
            match self.cursor.update(&self.duplication,&info,&self.copy_budget) {
                Ok(pointer)=>pointer,
                Err(_)=>{self.cursor=Default::default();Default::default()}
            }
        }else{Default::default()};
        if info.LastPresentTime==0 && self.staging.is_some() {
            lease.release()?;
            if self.cursor_enabled {
                if let Some(base)=&mut self.base {
                    base.pointer=pointer;
                    self.cursor_serial=self.cursor_serial.checked_add(1).ok_or(CaptureError::IdentityExhausted)?;
                }
            }
            return self.deliver(raw,audience,include_cursor,limits);
        }
        let resource = resource.ok_or(CaptureError::InvalidFrame(
            "DXGI acquired no desktop resource",
        ))?;
        let texture: ID3D11Texture2D = resource
            .cast()
            .map_err(|error| backend_error("DXGI desktop texture", error))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        let bytes = admitted_bytes(desc.Width, desc.Height, limits)?;
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || desc.SampleDesc.Count != 1
            || desc.ArraySize != 1
            || desc.MipLevels != 1
        {
            return Err(CaptureError::InvalidFrame(
                "unsupported DXGI desktop texture layout",
            ));
        }
        if self.rotation.output_size(desc.Width, desc.Height)
            != (monitor.info.width, monitor.info.height)
        {
            return Err(CaptureError::InvalidFrame(
                "DXGI frame dimensions changed with monitor topology",
            ));
        }
        if self.staging_size != (desc.Width, desc.Height) {
            if bytes > staging_allowance {
                return Err(CaptureError::BudgetExceeded(
                    "aggregate DXGI staging textures",
                ));
            }
            self.staging = None;
            self.copy_reservation = None;
            let reservation = self
                .copy_budget
                .reserve(crate::capture::native_budget::NativeCopyKind::Dxgi, bytes)
                .ok_or(CaptureError::BudgetExceeded(
                    "combined WGC/DXGI native copy budget",
                ))?;
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
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
            let mut staging = None;
            unsafe {
                self.device
                    .CreateTexture2D(&staging_desc, None, Some(&mut staging))
            }
            .map_err(|error| backend_error("CreateTexture2D staging", error))?;
            self.staging = Some(staging.ok_or(CaptureError::InvalidFrame(
                "D3D11 staging texture is absent",
            ))?);
            self.staging_size = (desc.Width, desc.Height);
            self.staging_bytes = bytes;
            self.copy_reservation = Some(reservation);
        }
        if self.retain_clean {
            self.base_credit=Some(self.copy_budget.reserve(crate::capture::native_budget::NativeCopyKind::Dxgi,bytes)
                .ok_or(CaptureError::BudgetExceeded("DXGI clean-base/native-copy budget"))?);
            self.pending_pointer=Some(pointer);
        }
        let staging=self.staging.as_ref().expect("admitted staging texture");
        unsafe {self.context.CopyResource(staging,&texture);self.context.Flush();}
        self.pending=true;self.pending_lease=Some(lease);
        self.deliver(raw,audience,include_cursor,limits)
    }
    fn deliver(&mut self,raw:Option<RgbaFrame>,audience:usize,include_cursor:bool,limits:CaptureLimits)->Result<DisplayCaptureOutcome,CaptureError>{
        if !self.retain_clean {return Ok(outcome(raw));}
        let serial=if include_cursor {self.cursor_serial}else{self.desktop_serial};
        if serial==0 || self.delivered[audience]==serial {return Ok(DisplayCaptureOutcome::NoNewFrame);}
        let base=self.base.as_ref().ok_or(CaptureError::InvalidFrame("DXGI shared generation lost clean desktop"))?;
        let hidden=crate::capture::dxgi_cursor::Snapshot::default();
        let (image,_credit)=base.composite(if include_cursor {&base.pointer}else{&hidden},&self.copy_budget,limits)?;
        self.delivered[audience]=serial;
        Ok(outcome(Some(image)))
    }
    fn readback(&mut self, limits: CaptureLimits) -> Result<Option<RgbaFrame>, CaptureError> {
        if !self.pending {
            return Ok(None);
        }
        let texture = self.staging.as_ref().ok_or(CaptureError::InvalidFrame(
            "pending DXGI copy has no staging texture",
        ))?;
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
            Err(error) if error.code() == DXGI_ERROR_WAS_STILL_DRAWING => return Ok(None),
            Err(error) => return Err(backend_error("Map DXGI staging", error)),
            Ok(()) => {}
        }
        let guard = MapGuard {
            context: self.context.clone(),
            texture: texture.clone(),
        };
        let (width, height) = self.staging_size;
        let extent = required_extent(width, height, mapped.RowPitch as usize, limits)?;
        if mapped.pData.is_null() {
            return Err(CaptureError::InvalidFrame(
                "DXGI mapped a null pixel buffer",
            ));
        }
        // Map guarantees a live texture subresource through Unmap. The slice
        // excludes trailing padding of the final row and all arithmetic/admission
        // checks precede construction; format/layout were verified before Copy.
        let bytes = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), extent) };
        let frame = convert_bgra(
            bytes,
            width,
            height,
            mapped.RowPitch as usize,
            self.rotation,
            limits,
        )?;
        drop(guard);
        self.pending_lease
            .take()
            .ok_or(CaptureError::InvalidFrame(
                "pending DXGI copy lost its frame lease",
            ))?
            .release()?;
        self.pending = false;
        Ok(Some(frame))
    }
}
impl State {
    fn retire_ready(&mut self) -> bool {
        if !self.pending {
            return true;
        }
        let Some(texture) = self.staging.as_ref() else {
            return false;
        };
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
            }
            Err(error)
                if error.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_DEVICE_REMOVED
                    || error.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_DEVICE_RESET => {}
            Err(_) => return false,
        }
        if let Some(lease) = self.pending_lease.take() {
            if lease.release().is_err() {
                self.pending = false;
                return true;
            }
        }
        self.pending = false;
        true
    }
}
impl Drop for State {
    fn drop(&mut self) {
        while !self.retire_ready() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
impl Drop for DxgiBackend {
    fn drop(&mut self) {
        self.retired
            .extend(self.states.drain().map(|(_, state)| state));
        while self.reap() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
fn outcome(frame: Option<RgbaFrame>) -> DisplayCaptureOutcome {
    match frame {
        Some(image) => DisplayCaptureOutcome::Frame {
            image,
            metadata: Some(DisplayFrameMetadata {
                backend: DisplayCaptureBackend::Dxgi,
                fallback_reason: None,
                generation: None,
            }),
        },
        None => DisplayCaptureOutcome::NoNewFrame,
    }
}
struct FrameLease {
    duplication: IDXGIOutputDuplication,
    armed: bool,
}
impl FrameLease {
    fn release(mut self) -> Result<(), CaptureError> {
        let result = unsafe { self.duplication.ReleaseFrame() }
            .map_err(|error| backend_error("ReleaseFrame", error));
        if result.is_ok() {
            self.armed = false;
        }
        result
    }
}
impl Drop for FrameLease {
    fn drop(&mut self) {
        if self.armed {
            let _ = unsafe { self.duplication.ReleaseFrame() };
        }
    }
}
struct MapGuard {
    context: ID3D11DeviceContext,
    texture: ID3D11Texture2D,
}
impl Drop for MapGuard {
    fn drop(&mut self) {
        unsafe { self.context.Unmap(&self.texture, 0) };
    }
}

fn required_extent(
    width: u32,
    height: u32,
    pitch: usize,
    limits: CaptureLimits,
) -> Result<usize, CaptureError> {
    let bytes = admitted_bytes(width, height, limits)?;
    let row = (width as usize)
        .checked_mul(4)
        .ok_or(CaptureError::BudgetExceeded("DXGI row bytes"))?;
    if pitch < row {
        return Err(CaptureError::InvalidFrame(
            "DXGI row pitch is shorter than a pixel row",
        ));
    }
    let extent = (height as usize - 1)
        .checked_mul(pitch)
        .and_then(|prefix| prefix.checked_add(row))
        .ok_or(CaptureError::BudgetExceeded("DXGI mapped extent overflow"))?;
    if extent
        .checked_add(bytes)
        .is_none_or(|working| working > limits.max_raw_bytes)
    {
        return Err(CaptureError::BudgetExceeded(
            "DXGI readback and RGBA working bytes",
        ));
    }
    Ok(extent)
}
fn convert_bgra(
    raw: &[u8],
    width: u32,
    height: u32,
    pitch: usize,
    rotation: Rotation,
    limits: CaptureLimits,
) -> Result<RgbaFrame, CaptureError> {
    let extent = required_extent(width, height, pitch, limits)?;
    if raw.len() < extent {
        return Err(CaptureError::InvalidFrame("truncated DXGI mapped frame"));
    }
    let (out_w, out_h) = rotation.output_size(width, height);
    let len = admitted_bytes(out_w, out_h, limits)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(len)
        .map_err(|_| CaptureError::BudgetExceeded("DXGI RGBA allocation"))?;
    pixels.resize(len, 0);
    for y in 0..out_h {
        for x in 0..out_w {
            let (sx, sy) = match rotation {
                Rotation::Identity => (x, y),
                Rotation::Clockwise90 => (y, height - 1 - x),
                Rotation::Half => (width - 1 - x, height - 1 - y),
                Rotation::Clockwise270 => (width - 1 - y, x),
            };
            let from = sy as usize * pitch + sx as usize * 4;
            let to = (y as usize * out_w as usize + x as usize) * 4;
            pixels[to..to + 4].copy_from_slice(&[raw[from + 2], raw[from + 1], raw[from], 255]);
        }
    }
    RgbaFrame::new(out_w, out_h, pixels).map_err(|_| CaptureError::InvalidFrame("DXGI RGBA output"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn padded_bgra_rotates_with_literal_obs_clockwise_oracles_and_opaque_alpha() {
        let raw = [
            11, 21, 1, 0, 12, 22, 2, 0, 99, 99, 99, 99, 13, 23, 3, 0, 14, 24, 4, 0, 99, 99, 99, 99,
            15, 25, 5, 0, 16, 26, 6, 0,
        ];
        for (rotation, expected) in [
            (Rotation::Identity, [1, 2, 3, 4, 5, 6]),
            (Rotation::Clockwise90, [5, 3, 1, 6, 4, 2]),
            (Rotation::Half, [6, 5, 4, 3, 2, 1]),
            (Rotation::Clockwise270, [2, 4, 6, 1, 3, 5]),
        ] {
            let frame = convert_bgra(&raw, 2, 3, 12, rotation, CaptureLimits::default()).unwrap();
            assert_eq!((frame.width(), frame.height()), rotation.output_size(2, 3));
            for (pixel, red) in frame.pixels().chunks_exact(4).zip(expected) {
                assert_eq!(pixel, &[red, red + 20, red + 10, 255]);
            }
        }
    }
    #[test]
    fn malformed_pitch_length_dimensions_and_working_budget_reject_before_allocation() {
        let limits = CaptureLimits::default();
        assert!(required_extent(2, 3, 7, limits).is_err());
        assert!(required_extent(2, 3, usize::MAX, limits).is_err());
        assert!(required_extent(0, 3, 12, limits).is_err());
        assert!(required_extent(8192, 4097, 32768, limits).is_err());
        assert!(convert_bgra(&[0; 20], 2, 3, 12, Rotation::Identity, limits).is_err());
        assert!(
            required_extent(
                2,
                3,
                12,
                CaptureLimits {
                    max_raw_bytes: 40,
                    ..limits
                }
            )
            .is_err()
        );
        assert_eq!(required_extent(2, 3, 12, limits).unwrap(), 32);
    }
}
