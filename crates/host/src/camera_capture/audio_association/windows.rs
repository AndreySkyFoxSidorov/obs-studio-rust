//! SetupAPI interface-to-devnode lookup. Call on an enumeration worker only.
use super::{associate, Association, ContainerId};
use skylive_core::{AudioTarget, VideoCaptureTarget};
use windows::{core::PCWSTR, Win32::{Foundation::HWND, Devices::{DeviceAndDriverInstallation::{
    HDEVINFO, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA,
    SetupDiCreateDeviceInfoList, SetupDiDestroyDeviceInfoList, SetupDiOpenDeviceInterfaceW,
    SetupDiGetDeviceInterfaceDetailW, SetupDiGetDevicePropertyW,
}, Properties::{DEVPKEY_Device_ContainerId, DEVPROPTYPE, DEVPROP_TYPE_GUID}}}};
use windows::Win32::{Devices::FunctionDiscovery::PKEY_Device_ContainerId,
    Media::Audio::{IMMDeviceEnumerator, IMMEndpoint, MMDeviceEnumerator, DEVICE_STATE_ACTIVE, eCapture},
    System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED, CLSCTX_ALL, STGM_READ, StructuredStorage::PropVariantToGUID}};
use windows::core::Interface;

struct DeviceList(HDEVINFO);
impl Drop for DeviceList {
    fn drop(&mut self) { let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) }; }
}

/// Resolve an exact Windows device-interface symbolic link to its physical
/// container. Missing properties and disconnected interfaces return None;
/// this must never fall back to matching a name or product identifier.
pub fn interface_container_id(path: &str) -> Option<ContainerId> {
    if path.is_empty() || path.len() > 8192 || path.contains('\0') { return None; }
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let list = DeviceList(unsafe { SetupDiCreateDeviceInfoList(None, HWND::default()) }.ok()?);
    let mut interface = SP_DEVICE_INTERFACE_DATA::default();
    interface.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
    unsafe { SetupDiOpenDeviceInterfaceW(list.0, PCWSTR(wide.as_ptr()), 0, Some(&mut interface)) }.ok()?;
    let mut required = 0u32;
    let _ = unsafe { SetupDiGetDeviceInterfaceDetailW(list.0, &interface, None, 0, Some(&mut required), None) };
    if required < std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 || required > 65536 { return None; }
    // u64 storage supplies the alignment required by the native structure.
    let mut storage = vec![0u64; (required as usize + 7) / 8];
    let detail = storage.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
    unsafe { (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32; }
    let mut device = SP_DEVINFO_DATA::default();
    device.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
    unsafe { SetupDiGetDeviceInterfaceDetailW(list.0, &interface, Some(detail), required, None, Some(&mut device)) }.ok()?;
    let mut ty = DEVPROPTYPE::default();
    let mut bytes = [0u8; 16];
    let mut length = 0u32;
    unsafe { SetupDiGetDevicePropertyW(list.0, &device, &DEVPKEY_Device_ContainerId, &mut ty, Some(&mut bytes), Some(&mut length), 0) }.ok()?;
    (ty == DEVPROP_TYPE_GUID && length == 16 && bytes != [0; 16]).then_some(ContainerId(bytes))
}

/// Read the operating system's container property for an active capture
/// endpoint. COM must already be initialized by the enumeration worker.
pub fn input_endpoint_container_id(endpoint_id: &str) -> Option<ContainerId> {
    if endpoint_id.is_empty() || endpoint_id.len() > 1024 || endpoint_id.contains('\0') { return None; }
    let wide: Vec<u16> = endpoint_id.encode_utf16().chain(std::iter::once(0)).collect();
    let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.ok()?;
    let device = unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }.ok()?;
    if unsafe { device.GetState() }.ok()? != DEVICE_STATE_ACTIVE { return None; }
    let endpoint: IMMEndpoint = device.cast().ok()?;
    if unsafe { endpoint.GetDataFlow() }.ok()? != eCapture { return None; }
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    let value = unsafe { store.GetValue(&PKEY_Device_ContainerId) }.ok()?;
    let guid = unsafe { PropVariantToGUID(&value) }.ok()?;
    let mut bytes = [0u8; 16];
    bytes[..4].copy_from_slice(&guid.data1.to_ne_bytes());
    bytes[4..6].copy_from_slice(&guid.data2.to_ne_bytes());
    bytes[6..8].copy_from_slice(&guid.data3.to_ne_bytes());
    bytes[8..].copy_from_slice(&guid.data4);
    (bytes != [0; 16]).then_some(ContainerId(bytes))
}

/// Resolve a camera and the current list of capture endpoints on a worker.
/// Unknown identities and multiple matches never produce a guessed device.
pub fn find_camera_input(camera: &VideoCaptureTarget, candidates: &[AudioTarget]) -> Association {
    if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().is_err() { return Association::UnknownCamera; }
    struct Apartment;
    impl Drop for Apartment { fn drop(&mut self) { unsafe { CoUninitialize() } } }
    let _apartment = Apartment;
    let VideoCaptureTarget::Windows { symbolic_link } = camera else { return Association::UnknownCamera; };
    let camera_id = interface_container_id(symbolic_link);
    let endpoints: Vec<_> = candidates.iter().filter_map(|target| match target {
        AudioTarget::WindowsEndpoint { id } => Some((input_endpoint_container_id(id), target)),
        _ => None,
    }).collect();
    associate(camera_id, endpoints)
}
