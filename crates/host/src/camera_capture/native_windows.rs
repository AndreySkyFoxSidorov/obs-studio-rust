//! Worker-local Media Foundation camera. The callback retains at most one
//! bounded SDK sample per issued read; conversion and final Release stay on
//! the owner. No native initialization in the composition-root factory.
use super::super::*;
use rtrb::{Consumer, Producer, RingBuffer};
use skylive_core::{VideoColorMatrix, VideoColorRange, VideoPixelFormat};
use skylive_media::{
    AlphaMode, CameraFrameLayout, CameraMatrix, CameraPixelFormat, CameraRange, PackedFrameLayout,
    PackedFrameLimits, PackedPixelFormat, RowOrder, ingest_camera_frame, ingest_packed_frame,
};
use std::{
    collections::VecDeque,
    ffi::c_void,
    ptr,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::{E_ACCESSDENIED, E_INVALIDARG},
        Media::MediaFoundation::*,
        System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
    },
    core::{GUID, HRESULT, Interface, PCWSTR, implement},
};

const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
pub(super) struct NativeBackend {
    _owner: Rc<()>,
}
impl NativeBackend {
    pub(super) fn new() -> Result<Self, CameraError> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| native_error("CoInitializeEx(camera MTA)", e))?;
        if let Err(error) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            unsafe {
                CoUninitialize();
            }
            return Err(native_error("MFStartup", error));
        }
        Ok(Self {
            _owner: Rc::new(()),
        })
    }
}
impl Drop for NativeBackend {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

fn attributes() -> Result<IMFAttributes, CameraError> {
    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, 4) }
        .map_err(|e| native_error("MFCreateAttributes", e))?;
    attributes.ok_or(CameraError::InvalidFrame("missing MF attributes"))
}
struct Activations {
    ptr: *mut Option<IMFActivate>,
    count: u32,
}
impl Drop for Activations {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                for index in 0..self.count {
                    ptr::drop_in_place(self.ptr.add(index as usize));
                }
                CoTaskMemFree(Some(self.ptr.cast::<c_void>()));
            }
        }
    }
}
fn device_activations(limits: CameraLimits) -> Result<Activations, CameraError> {
    let attributes = attributes()?;
    unsafe {
        attributes.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )
    }
    .map_err(|e| native_error("video device type", e))?;
    let mut values = Activations {
        ptr: ptr::null_mut(),
        count: 0,
    };
    unsafe { MFEnumDeviceSources(&attributes, &mut values.ptr, &mut values.count) }
        .map_err(|e| native_error("MFEnumDeviceSources", e))?;
    if values.count as usize > limits.max_devices {
        return Err(CameraError::BudgetExceeded("camera device count"));
    }
    if values.count != 0 && values.ptr.is_null() {
        return Err(CameraError::InvalidFrame("null camera device list"));
    }
    Ok(values)
}
fn attribute_string(
    attributes: &IMFAttributes,
    key: &GUID,
    max: usize,
) -> Result<String, CameraError> {
    let length = unsafe { attributes.GetStringLength(key) }
        .map_err(|e| native_error("camera string length", e))? as usize;
    if length > max {
        return Err(CameraError::BudgetExceeded("camera string length"));
    }
    let mut wide = vec![0u16; length + 1];
    unsafe { attributes.GetString(key, &mut wide, None) }
        .map_err(|e| native_error("camera string", e))?;
    let text = String::from_utf16(&wide[..length])
        .map_err(|_| CameraError::InvalidFrame("camera string UTF16"))?;
    if text.is_empty() || text.len() > max || text.chars().any(char::is_control) {
        return Err(CameraError::InvalidFrame("camera string value"));
    }
    Ok(text.into_boxed_str().into_string())
}
fn open_source(target: &VideoCaptureTarget) -> Result<IMFMediaSource, CameraError> {
    let VideoCaptureTarget::Windows { symbolic_link } = target else {
        return Err(CameraError::Unavailable(
            "camera identity requires another operating system",
        ));
    };
    let attributes = attributes()?;
    let id: Vec<u16> = symbolic_link.encode_utf16().chain(Some(0)).collect();
    unsafe {
        attributes
            .SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
            .and_then(|_| {
                attributes.SetString(
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                    PCWSTR(id.as_ptr()),
                )
            })
            .and_then(|_| {
                attributes.SetUINT32(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_MAX_BUFFERS, 1)
            })
    }
    .map_err(|e| native_error("camera source attributes", e))?;
    unsafe { MFCreateDeviceSource(&attributes) }
        .map_err(|e| native_error("MFCreateDeviceSource", e))
}
struct SourceGuard(Option<IMFMediaSource>);
impl Drop for SourceGuard {
    fn drop(&mut self) {
        if let Some(source) = self.0.take() {
            unsafe {
                let _ = source.Shutdown();
            }
        }
    }
}

impl NativeCameraBackend for NativeBackend {
    fn open_embedded(&mut self,binding:&VideoCaptureBinding,session:u64,source:skylive_core::Id,
        camera_epoch:u64,limits:CameraLimits)->Result<Box<dyn NativeCameraStream>,CameraError> {
        let scope=super::embedded_audio::EmbeddedAudioScope::new(session,source,camera_epoch,binding)
            .map_err(|error|CameraError::Native{operation:"embedded camera scope",detail:error.to_string()})?;
        // NativeBackend owns the camera MTA and MF lifetime; the runtime admits
        // only one stream for this physical target before calling this method.
        let graph=unsafe {super::embedded_audio::GraphOwner::open_shared(scope,binding,crate::audio_capture::AudioCaptureLimits::default(),limits)}
            .map_err(|error|CameraError::Native{operation:"open shared DirectShow camera",detail:error.to_string()})?;
        Ok(Box::new(graph))
    }
    fn enumerate_devices(
        &mut self,
        limits: CameraLimits,
    ) -> Result<Vec<CameraDeviceInfo>, CameraError> {
        let values = device_activations(limits)?;
        let mut output = Vec::with_capacity(values.count as usize);
        for i in 0..values.count {
            let activate = unsafe { &*values.ptr.add(i as usize) }
                .as_ref()
                .ok_or(CameraError::InvalidFrame("missing camera activation"))?;
            let target = VideoCaptureTarget::Windows {
                symbolic_link: attribute_string(
                    activate,
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                    4096,
                )?,
            };
            let name = attribute_string(activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, 1024)?;
            output.push(CameraDeviceInfo { target, name });
        }
        Ok(output)
    }
    fn enumerate_modes(
        &mut self,
        target: &VideoCaptureTarget,
        limits: CameraLimits,
    ) -> Result<Vec<CameraModeInfo>, CameraError> {
        let source = SourceGuard(Some(open_source(target)?));
        let reader = unsafe {
            MFCreateSourceReaderFromMediaSource(source.0.as_ref().unwrap(), None::<&IMFAttributes>)
        }
        .map_err(|e| native_error("camera mode source reader", e))?;
        let result = media_types(&reader, limits)
            .map(|types| types.into_iter().map(|(_, info)| info).collect());
        drop(reader);
        drop(source);
        result
    }
    fn directshow_default(&mut self, target: &VideoCaptureTarget) -> Result<CameraModeInfo, CameraError> {
        directshow_default(target)
    }
    fn directshow_video_only(&mut self, target: &VideoCaptureTarget) -> Result<bool, CameraError> {
        directshow_video_only(target)
    }
    fn directshow_preferred(&mut self, target: &VideoCaptureTarget) -> Result<CameraModeInfo, CameraError> {
        directshow_probe(target,true)
    }
    fn open(
        &mut self,
        binding: &VideoCaptureBinding,
        limits: CameraLimits,
    ) -> Result<Box<dyn NativeCameraStream>, CameraError> {
        let bytes = checked_frame_bytes(binding.width(), binding.height(), limits)?;
        checked_mode_raw_bytes(binding.mode(),limits)?;
        if bytes
            .checked_mul(3)
            .is_none_or(|v| v > limits.max_raw_bytes)
        {
            return Err(CameraError::BudgetExceeded(
                "camera SDK and conversion buffers",
            ));
        }
        let mut source = SourceGuard(Some(open_source(binding.target())?));
        let (producer, consumer) = RingBuffer::new(3);
        let state = Arc::new(CallbackState {
            producer: Mutex::new(producer),
            pending: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            flushed: AtomicBool::new(false),
            error: AtomicI32::new(0),
            dropped: AtomicU64::new(0),
            max_bytes: match binding.mode().format() {VideoPixelFormat::Mjpeg=>mjpeg::compressed_cap(binding.mode(),limits)?,VideoPixelFormat::H264=>avc::compressed_cap(),_=>limits.max_raw_bytes / 3},
        });
        let callback: IMFSourceReaderCallback = SampleCallback {
            state: state.clone(),
        }
        .into();
        let attributes = attributes()?;
        unsafe {
            attributes
                .SetUnknown(&MF_SOURCE_READER_ASYNC_CALLBACK, &callback)
                .and_then(|_| attributes.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1))
                .and_then(|_| {
                    attributes.SetUINT32(&MF_SOURCE_READER_DISCONNECT_MEDIASOURCE_ON_SHUTDOWN, 1)
                })
        }
        .map_err(|e| native_error("camera async attributes", e))?;
        let reader =
            unsafe { MFCreateSourceReaderFromMediaSource(source.0.as_ref().unwrap(), &attributes) }
                .map_err(|e| native_error("camera async source reader", e))?;
        let selected = media_types(&reader, limits)?
            .into_iter()
            .find(|(_, info)| info.mode == Some(binding.mode()))
            .map(|(ty, _)| ty)
            .ok_or(CameraError::ModeNotFound)?;
        unsafe {
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .and_then(|_| reader.SetStreamSelection(STREAM, true))
                .and_then(|_| reader.SetCurrentMediaType(STREAM, None, &selected))
        }
        .map_err(|e| native_error("select exact camera native mode", e))?;
        let actual = unsafe { reader.GetCurrentMediaType(STREAM) }
            .map_err(|e| native_error("verify camera native mode", e))?;
        if mode_info(&actual)?.mode != Some(binding.mode()) {
            return Err(CameraError::ModeNotFound);
        }
        let stride = checked_stride(&actual, binding.mode(), limits)?;
        let avc = if binding.mode().format()==VideoPixelFormat::H264 {
            Some(avc::CameraAvcDecoder::new(binding.width(),binding.height(),limits,avc_configuration(&actual)?)?)
        } else {None};
        let mut stream = NativeStream {
            source: source.0.take().unwrap(),
            reader,
            _callback: callback,
            state,
            consumer,
            mode: binding.mode(),
            stride,
            avc,
            avc_ready: VecDeque::new(),
            pending_discontinuity: false,
            need_read: false,
            stopping: false,
            stopped: false,
            flush_retry_at: None,
            _owner: Rc::new(()),
        };
        stream.request_next()?;
        Ok(Box::new(stream))
    }
}
fn checked_stride(actual: &IMFMediaType, mode: VideoCaptureMode, limits: CameraLimits) -> Result<Option<i32>, CameraError> {
        // Prefer each sample's Lock2DSize pitch. An absent default stride is only
        // acceptable when the sample supplies that stronger bounded layout API.
        let stride = if matches!(mode.format(),VideoPixelFormat::Mjpeg|VideoPixelFormat::H264) {None}else{match unsafe { actual.GetUINT32(&MF_MT_DEFAULT_STRIDE) } {
            Ok(value) => Some(value as i32),
            Err(error) if error.code() == MF_E_ATTRIBUTENOTFOUND => {
                let subtype = unsafe { actual.GetGUID(&MF_MT_SUBTYPE) }
                    .map_err(|e| native_error("camera stride subtype", e))?;
                // Microsoft's system-memory fallback. Lock2DSize remains authoritative
                // when an SDK surface exposes an actual padded pitch.
                unsafe { MFGetStrideForBitmapInfoHeader(subtype.data1, mode.width()) }.ok()
            }
            Err(error) => return Err(native_error("camera default stride", error)),
        }};
        if let Some(stride) = stride {
            let raw = (stride.unsigned_abs() as usize)
                .checked_mul(backing_rows(mode))
                .ok_or(CameraError::BudgetExceeded("camera native stride"))?;
            if (stride.unsigned_abs() as usize) < row_bytes(mode)
                || (is_yuv(mode) && stride <= 0)
                || raw.checked_mul(3).is_none_or(|v| v > limits.max_raw_bytes)
            {
                return Err(CameraError::BudgetExceeded("camera native stride buffers"));
            }
        }
        Ok(stride)
}

/// Some cameras supply SPS/PPS as a media-type sequence header instead of in
/// each stream. Keep the exact bounded bytes across discontinuity resets.
fn avc_configuration(actual:&IMFMediaType)->Result<Option<Vec<u8>>,CameraError> {
    let size=match unsafe {actual.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER)} {
        Ok(0)=>return Ok(None),
        Ok(value)=>value as usize,
        Err(error) if error.code()==MF_E_ATTRIBUTENOTFOUND=>return Ok(None),
        Err(error)=>return Err(native_error("AVC camera sequence header size",error)),
    };
    if size>64*1024{return Err(CameraError::BudgetExceeded("AVC camera sequence header"));}
    let mut bytes=Vec::new();
    bytes.try_reserve_exact(size).map_err(|_|CameraError::BudgetExceeded("AVC camera sequence header allocation"))?;
    bytes.resize(size,0);
    let mut actual_size=0;
    unsafe {actual.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER,&mut bytes,Some(&mut actual_size))}
        .map_err(|error|native_error("AVC camera sequence header",error))?;
    if actual_size as usize!=size{return Err(CameraError::InvalidFrame("AVC camera sequence header size changed"));}
    Ok(Some(bytes))
}
fn media_types(
    reader: &IMFSourceReader,
    limits: CameraLimits,
) -> Result<Vec<(IMFMediaType, CameraModeInfo)>, CameraError> {
    let mut types: Vec<(IMFMediaType, CameraModeInfo)> = Vec::new();
    for index in 0..=4096usize {
        match unsafe { reader.GetNativeMediaType(STREAM, index as u32) } {
            Ok(ty) => {
                if index == 4096 {
                    return Err(CameraError::BudgetExceeded("camera raw native mode scan"));
                }
                let mut duplicate = false;
                for (previous, _) in &types {
                    if unsafe { ty.Compare(previous, MF_ATTRIBUTES_MATCH_ALL_ITEMS) }
                        .map_err(|e| native_error("camera exact mode comparison", e))?
                        .as_bool()
                    {
                        duplicate = true;
                        break;
                    }
                }
                if duplicate {
                    continue;
                }
                if types.len() == limits.max_modes {
                    return Err(CameraError::BudgetExceeded(
                        "camera distinct native mode count",
                    ));
                }
                let mut info = mode_info(&ty)?;
                if info.mode.is_some_and(|mode|mode.format()==VideoPixelFormat::H264 && avc::reservation(mode.width(),mode.height(),limits).is_err()) {
                    info.mode=None;
                    info.unsupported_reason=Some("H.264 mode exceeds the AVC camera decoder budget");
                }
                types.push((ty, info));
            }
            Err(error) if error.code() == MF_E_NO_MORE_TYPES => return Ok(types),
            Err(error) => return Err(native_error("camera native modes", error)),
        }
    }
    Err(CameraError::BudgetExceeded("camera native modes"))
}
fn mode_info(ty: &IMFMediaType) -> Result<CameraModeInfo, CameraError> {
    // A driver can advertise a malformed media type alongside valid ones.
    // Preserve it as an unsupported entry instead of discarding every usable
    // mode from the same camera. Zero dimensions denote unknown metadata and
    // can never become a selectable VideoCaptureMode.
    let size = unsafe { ty.GetUINT64(&MF_MT_FRAME_SIZE) };
    let fps = unsafe { ty.GetUINT64(&MF_MT_FRAME_RATE) }.unwrap_or(0);
    let subtype = unsafe { ty.GetGUID(&MF_MT_SUBTYPE) };
    let (size, subtype) = match (size, subtype) {
        (Ok(size), Ok(subtype)) => (size, subtype),
        (size, subtype) => {
            let size = size.unwrap_or(0);
            return Ok(CameraModeInfo {
                width: (size >> 32) as u32,
                height: size as u32,
                fps_num: (fps >> 32) as u32,
                fps_den: fps as u32,
                native_subtype: subtype.map_or_else(|_| "Unknown".into(), |value| format!("{value:?}")),
                mode: None,
                unsupported_reason: Some("The camera reported a mode with missing or invalid size or subtype metadata."),
            });
        }
    };
    let (width, height, fps_num, fps_den) = (
        (size >> 32) as u32,
        size as u32,
        (fps >> 32) as u32,
        fps as u32,
    );
    let profile = native_pixel_format(ty, subtype);
    let mode = profile
        .as_ref()
        .ok()
        .and_then(|format| VideoCaptureMode::new(width, height, fps_num, fps_den, *format).ok());
    let native_subtype = if subtype == MFVideoFormat_RGB32 {
        "RGB32".into()
    } else if subtype == MFVideoFormat_ARGB32 {
        "ARGB32".into()
    } else if subtype == MFVideoFormat_RGB24 {
        "RGB24".into()
    } else if subtype == MFVideoFormat_YUY2 {
        "YUY2".into()
    } else if subtype == MFVideoFormat_UYVY {
        "UYVY".into()
    } else if subtype == MFVideoFormat_I420 {
        "I420".into()
    } else if subtype == MFVideoFormat_IYUV {
        "IYUV".into()
    } else if subtype == MFVideoFormat_YV12 {
        "YV12".into()
    } else if subtype == MFVideoFormat_NV12 {
        "NV12".into()
    } else if subtype == MFVideoFormat_MJPG {
        "MJPG".into()
    } else if subtype == MFVideoFormat_H264 {
        "H264".into()
    } else {
        format!("{subtype:?}")
    };
    Ok(CameraModeInfo {
        width,
        height,
        fps_num,
        fps_den,
        native_subtype,
        mode,
        unsupported_reason: if mode.is_some() {
            None
        } else {
            Some(
                profile.err().unwrap_or(
                    "native dimensions or frame rate exceed the supported camera profile",
                ),
            )
        },
    })
}

struct DirectShowMediaType(*mut AM_MEDIA_TYPE);
impl Drop for DirectShowMediaType { fn drop(&mut self) { unsafe {
    let value=&mut *self.0;
    if !value.pbFormat.is_null() {CoTaskMemFree(Some(value.pbFormat.cast()));}
    std::mem::ManuallyDrop::drop(&mut value.pUnk);
    CoTaskMemFree(Some(self.0.cast()));
} } }

// OBS can attach a separate audio filter for an allowlisted vendor. Until its
// parent-device lookup is implemented, only non-allowlisted USB devices can
// establish absence; unknown paths and ambiguous pin enumeration fail closed.
fn directshow_video_only(target:&VideoCaptureTarget)->Result<bool,CameraError> {
    use windows::Win32::Media::DirectShow::{IBaseFilter, ICreateDevEnum, PINDIR_OUTPUT};
    use windows::Win32::System::Com::{CoCreateInstance, CreateBindCtx, IMoniker, CLSCTX_INPROC_SERVER};
    let VideoCaptureTarget::Windows{symbolic_link}=target else {return Err(CameraError::Unavailable("DirectShow requires a Windows camera"));};
    let upper=symbolic_link.to_ascii_uppercase();
    let prefix="\\\\?\\USB#VID_";
    let Some(rest)=upper.strip_prefix(prefix) else {return Err(CameraError::Unavailable("Automatic camera audio cannot be ruled out for this device path"));};
    let Some(vid)=rest.get(..4).filter(|value|value.bytes().all(|byte|byte.is_ascii_hexdigit())) else {return Err(CameraError::InvalidFrame("USB camera vendor identity"));};
    const WHITELIST:[&str;19]=["0FD9","3842","0B05","07CA","048D","04B4","0557","1164","1532","1BCF","1E4E","1E71","2040","2935","298F","2B77","32ED","534D","EBA4"];
    if WHITELIST.contains(&vid) {return Err(CameraError::Unavailable("OBS may attach a separate audio filter for this camera vendor"));}
    let enumerate:ICreateDevEnum=unsafe {CoCreateInstance(&CLSID_SystemDeviceEnum,None,CLSCTX_INPROC_SERVER)}.map_err(|e|native_error("DirectShow device enumerator",e))?;
    let mut monikers=None;
    unsafe {enumerate.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory,&mut monikers,0)}.map_err(|e|native_error("DirectShow video category",e))?;
    let monikers=monikers.ok_or(CameraError::DeviceNotFound)?;
    let bind=unsafe {CreateBindCtx(0)}.map_err(|e|native_error("DirectShow bind context",e))?;
    let mut selected:Option<IMoniker>=None;
    for index in 0..=256 {
        let mut slot=[None];let status=unsafe {monikers.Next(&mut slot,None)};
        if status.is_err(){return Err(CameraError::Native{operation:"DirectShow device enumeration",detail:format!("{status:?}")});}
        let Some(moniker)=slot[0].take() else {break;};
        if index==256{return Err(CameraError::BudgetExceeded("DirectShow device count"));}
        let name=unsafe {moniker.GetDisplayName(&bind,None::<&IMoniker>)}.map_err(|e|native_error("DirectShow moniker identity",e))?;
        if name.0.is_null(){return Err(CameraError::InvalidFrame("DirectShow moniker identity is null"));}
        let mut len=0usize;while len<=8192 && unsafe {*name.0.add(len)}!=0 {len+=1;}
        let value=if len<=8192 {String::from_utf16(unsafe {std::slice::from_raw_parts(name.0,len)}).ok()}else{None};
        unsafe {CoTaskMemFree(Some(name.0.cast()));}
        let value=value.ok_or(CameraError::InvalidFrame("DirectShow moniker identity is invalid or too long"))?;
        let prefix="@device:pnp:";
        if value.get(..prefix.len()).is_some_and(|s|s.eq_ignore_ascii_case(prefix)) && value.get(prefix.len()..).is_some_and(|s|s.eq_ignore_ascii_case(symbolic_link)) {
            if selected.is_some(){return Err(CameraError::InvalidFrame("ambiguous DirectShow camera path"));}selected=Some(moniker);
        }
    }
    let moniker=selected.ok_or(CameraError::DeviceNotFound)?;
    let filter:IBaseFilter=unsafe {moniker.BindToObject(&bind,None::<&IMoniker>)}.map_err(|e|native_error("DirectShow camera filter",e))?;
    let pins=unsafe {filter.EnumPins()}.map_err(|e|native_error("DirectShow camera pins",e))?;
    let mut saw_video=false;
    for index in 0..=64 {
        let mut slot=[None];let status=unsafe {pins.Next(&mut slot,None)};
        if status.is_err(){return Err(CameraError::Native{operation:"DirectShow pin enumeration",detail:format!("{status:?}")});}
        let Some(pin)=slot[0].take() else {break;};
        if index==64{return Err(CameraError::BudgetExceeded("DirectShow camera pin count"));}
        let direction=unsafe {pin.QueryDirection()}.map_err(|e|native_error("DirectShow pin direction",e))?;
        if direction!=PINDIR_OUTPUT {continue;}
        let types=unsafe {pin.EnumMediaTypes()}.map_err(|e|native_error("DirectShow pin media types",e))?;
        // An output pin may advertise no types until negotiation. Its media
        // kind is then unknown, not evidence that it cannot carry audio.
        let mut saw_type=false;
        for index in 0..=1024 {
            let mut raw=std::ptr::null_mut();let mut fetched=0u32;
            let status=unsafe {types.Next(std::slice::from_mut(&mut raw),Some(&mut fetched))};
            if status.is_err(){if !raw.is_null(){drop(DirectShowMediaType(raw));}return Err(CameraError::Native{operation:"DirectShow pin media types",detail:format!("{status:?}")});}
            if raw.is_null(){break;}
            let value=DirectShowMediaType(raw);
            if index==1024{return Err(CameraError::BudgetExceeded("DirectShow media type count"));}
            if fetched!=1{return Err(CameraError::InvalidFrame("DirectShow media type count"));}
            saw_type=true;
            if unsafe {(*value.0).majortype} == MFMediaType_Audio {return Ok(false);}
            if unsafe {(*value.0).majortype} == MFMediaType_Video {saw_video=true;}
        }
        if !saw_type {return Err(CameraError::Unavailable("DirectShow output pin media type is unknown; embedded audio cannot be excluded"));}
    }
    if !saw_video{return Err(CameraError::Unavailable("DirectShow camera video pin could not be established"));}
    Ok(true)
}

/// Read the DirectShow capture pin default, without creating an MF source.
/// This function runs only on the camera owner, after it has rejected an
/// active/retiring handle for the same device. GetFormat is just the first
/// step of OBS Preferred: callers must still apply OBS capability ranking.
fn directshow_default(target: &VideoCaptureTarget) -> Result<CameraModeInfo, CameraError> {
    directshow_probe(target, false)
}

fn directshow_probe(target: &VideoCaptureTarget, preferred: bool) -> Result<CameraModeInfo, CameraError> {
    use windows::Win32::Media::{DirectShow::{IBaseFilter, ICreateDevEnum, IAMStreamConfig, PINDIR_OUTPUT, AMPROPERTY_PIN_CATEGORY}, KernelStreaming::IKsPropertySet};
    use windows::Win32::System::Com::{CoCreateInstance, CreateBindCtx, IMoniker, CLSCTX_INPROC_SERVER};
    let VideoCaptureTarget::Windows { symbolic_link } = target else {
        return Err(CameraError::Unavailable("DirectShow requires a Windows camera"));
    };
    let enumerate: ICreateDevEnum = unsafe { CoCreateInstance(&CLSID_SystemDeviceEnum, None, CLSCTX_INPROC_SERVER) }
        .map_err(|e| native_error("DirectShow device enumerator", e))?;
    let mut monikers = None;
    unsafe { enumerate.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory, &mut monikers, 0) }
        .map_err(|e| native_error("DirectShow video category", e))?;
    let monikers = monikers.ok_or(CameraError::DeviceNotFound)?;
    let bind = unsafe { CreateBindCtx(0) }.map_err(|e| native_error("DirectShow bind context", e))?;
    let mut selected: Option<IMoniker> = None;
    for index in 0..=256 {
        let mut slot = [None];
        let result = unsafe { monikers.Next(&mut slot, None) };
        if result.is_err() { return Err(CameraError::Native { operation:"DirectShow device enumeration", detail:format!("{result:?}") }); }
        let Some(moniker) = slot[0].take() else { break; };
        if index==256 {return Err(CameraError::BudgetExceeded("DirectShow device count"));}
        let name = unsafe { moniker.GetDisplayName(&bind, None::<&IMoniker>) }
            .map_err(|e| native_error("DirectShow moniker identity", e))?;
        if name.0.is_null() {return Err(CameraError::InvalidFrame("DirectShow moniker identity is null"));}
        // Bound the OS-provided string before converting it. The known PnP
        // prefix is stripped only when the rest matches the full MF path.
        let mut len=0usize;
        while len<=8192 && unsafe { *name.0.add(len) } != 0 { len+=1; }
        let value=if len<=8192 { String::from_utf16(unsafe { std::slice::from_raw_parts(name.0,len) }).ok() } else { None };
        unsafe { CoTaskMemFree(Some(name.0.cast())); }
        let Some(value)=value else { return Err(CameraError::InvalidFrame("DirectShow moniker identity is invalid or too long")); };
        let prefix="@device:pnp:";
        if value.get(..prefix.len()).is_some_and(|head|head.eq_ignore_ascii_case(prefix))
            && value.get(prefix.len()..).is_some_and(|path|path.eq_ignore_ascii_case(symbolic_link)) {
            if selected.is_some() { return Err(CameraError::InvalidFrame("ambiguous DirectShow camera path")); }
            selected=Some(moniker);
        }
    }
    let moniker=selected.ok_or(CameraError::DeviceNotFound)?;
    let filter: IBaseFilter = unsafe { moniker.BindToObject(&bind, None::<&IMoniker>) }
        .map_err(|e| native_error("DirectShow camera filter", e))?;
    let pins=unsafe { filter.EnumPins() }.map_err(|e| native_error("DirectShow camera pins", e))?;
    let mut selected_type: Option<DirectShowMediaType>=None;
    let mut selected_config: Option<IAMStreamConfig>=None;
    let mut selected_pin: Option<windows::Win32::Media::DirectShow::IPin>=None;
    for index in 0..=64 {
        let mut slot=[None];
        let result=unsafe { pins.Next(&mut slot,None) };
        if result.is_err() { return Err(CameraError::Native {operation:"DirectShow pin enumeration",detail:format!("{result:?}")}); }
        let Some(pin)=slot[0].take() else {break;};
        if index==64 {return Err(CameraError::BudgetExceeded("DirectShow camera pin count"));}
        if unsafe { pin.QueryDirection() }.ok()!=Some(PINDIR_OUTPUT) {continue;}
        let Ok(properties)=pin.cast::<IKsPropertySet>() else {continue;};
        let mut category=GUID::zeroed();let mut bytes=0u32;
        if unsafe { properties.Get(&AMPROPSETID_Pin, AMPROPERTY_PIN_CATEGORY.0 as u32,
            ptr::null(),0,(&mut category as *mut GUID).cast(),std::mem::size_of::<GUID>() as u32,&mut bytes) }.is_err()
            || bytes as usize!=std::mem::size_of::<GUID>() || category!=PIN_CATEGORY_CAPTURE {continue;}
        let Ok(config)=pin.cast::<IAMStreamConfig>() else {continue;};
        // A capture filter can also expose an audio pin. An unsupported
        // GetFormat on that unrelated pin must not prevent finding video.
        let media=match unsafe { config.GetFormat() } {
            Ok(raw) if !raw.is_null()=>DirectShowMediaType(raw),
            Err(error) if error.code()==windows::Win32::Foundation::E_NOTIMPL=>{
                // This category may also contain an audio capture pin. Failure
                // to enumerate that unrelated pin must not hide a later video pin.
                let Ok(Some(media))=directshow_first_video_type(&pin) else {continue;};
                media
            },
            _=>continue,
        };
        if unsafe { (*media.0).majortype } != MFMediaType_Video {continue;}
        if selected_type.is_some() {return Err(CameraError::InvalidFrame("ambiguous DirectShow video capture pin"));}
        selected_type=Some(media);
        selected_config=Some(config);
        selected_pin=Some(pin);
    }
    let mut raw=selected_type.ok_or(CameraError::Unavailable("DirectShow video capture pin is unavailable"))?;
    if preferred {
        let config=selected_config.ok_or(CameraError::Unavailable("DirectShow video capture configuration is unavailable"))?;
        let pin=selected_pin.ok_or(CameraError::Unavailable("DirectShow video capture pin is unavailable"))?;
        raw=directshow_select_preferred(&config, &pin, &raw)?;
    }
    let data=unsafe { &*raw.0 };
    if data.cbFormat==0 || data.cbFormat>64*1024 || data.pbFormat.is_null() {
        return Err(CameraError::InvalidFrame("DirectShow default format payload is invalid"));
    }
    let media=unsafe { MFCreateMediaType() }.map_err(|e|native_error("DirectShow default media type",e))?;
    unsafe { MFInitMediaTypeFromAMMediaType(&media,raw.0) }
        .map_err(|e|native_error("DirectShow default media conversion",e))?;
    mode_info(&media)
}

/// OBS uses the first FORMAT_VideoInfo pin media type when GetFormat returns
/// E_NOTIMPL. Retain the original media type and let the caller own/free it.
fn directshow_first_video_type(pin:&windows::Win32::Media::DirectShow::IPin)
    ->Result<Option<DirectShowMediaType>,CameraError> {
    let types=unsafe {pin.EnumMediaTypes()}.map_err(|e|native_error("DirectShow default media type enumeration",e))?;
    for index in 0..=1024 {
        let mut raw=std::ptr::null_mut();let mut fetched=0u32;
        let result=unsafe {types.Next(std::slice::from_mut(&mut raw),Some(&mut fetched))};
        if result.is_err() {
            if !raw.is_null(){drop(DirectShowMediaType(raw));}
            return Err(CameraError::Native {operation:"DirectShow default media type enumeration",detail:format!("{result:?}")});
        }
        if raw.is_null(){return Ok(None);}
        let media=DirectShowMediaType(raw);
        if index==1024{return Err(CameraError::BudgetExceeded("DirectShow media type count"));}
        if fetched!=1{return Err(CameraError::InvalidFrame("DirectShow media type enumeration count"));}
        let value=unsafe {&*media.0};
        if value.majortype==MFMediaType_Video && value.formattype==FORMAT_VideoInfo {return Ok(Some(media));}
    }
    Ok(None)
}

/// Read and rank the real DirectShow capability ranges, preserving driver order
/// for equal scores as libdshowcapture does. The returned media type is owned
/// by this thread. No SetFormat is performed by this read-only probe.
fn directshow_select_preferred(config:&windows::Win32::Media::DirectShow::IAMStreamConfig,
    pin:&windows::Win32::Media::DirectShow::IPin, default:&DirectShowMediaType)->Result<DirectShowMediaType,CameraError> {
    use windows::Win32::Media::DirectShow::VIDEO_STREAM_CONFIG_CAPS;
    use super::directshow_rank::{Capability,Request,choose};
    fn header(media:&AM_MEDIA_TYPE)->Option<VIDEOINFOHEADER> {
        if media.formattype!=FORMAT_VideoInfo || media.pbFormat.is_null()
            || (media.cbFormat as usize)<std::mem::size_of::<VIDEOINFOHEADER>() || media.cbFormat>64*1024 {return None;}
        Some(unsafe {std::ptr::read_unaligned(media.pbFormat.cast::<VIDEOINFOHEADER>())})
    }
    fn format_rating(media:&AM_MEDIA_TYPE, header:&VIDEOINFOHEADER)->Option<i64> {
        if media.subtype==MEDIASUBTYPE_RGB24 || media.subtype==MEDIASUBTYPE_RGB32 || media.subtype==MEDIASUBTYPE_ARGB32 {return Some(15);}
        let standard=GUID::from_values(media.subtype.data1,0,0x10,[0x80,0,0,0xaa,0,0x38,0x9b,0x71]);
        fn rating(code:u32)->Option<i64> {match code {
            0x30323449|0x56555949|0x3231564e|0x32315659|0x30303859|0x30313050=>Some(0),
            0x55595659|0x32595559|0x59565955|0x43594448=>Some(5),
            0x47504a4d=>Some(10),
            0x34363248|0x43564548|0x32424752|0x34424752|0x42475241=>Some(15),
            _=>None,
        }}
        (media.subtype==standard).then(||rating(media.subtype.data1)).flatten()
            .or_else(||rating(header.bmiHeader.biCompression))
    }
    let original=header(unsafe {&*default.0}).ok_or(CameraError::Unavailable("DirectShow preferred default requires VIDEOINFOHEADER"))?;
    let request=Request {width:original.bmiHeader.biWidth,
        height:original.bmiHeader.biHeight.checked_abs().ok_or(CameraError::InvalidFrame("DirectShow default height overflow"))?,
        flipped:original.bmiHeader.biHeight<0,interval:original.AvgTimePerFrame};
    let (mut count,mut bytes)=(0i32,0i32);
    let mut best:Option<(i64,DirectShowMediaType)>=None;
    let mut consider=|candidate:DirectShowMediaType, caps:Option<VIDEO_STREAM_CONFIG_CAPS>| {
        let media=unsafe {&*candidate.0};
        if media.majortype!=MFMediaType_Video {return false;}
        let Some(mut info)=header(media) else {return false;};
        let Some(rating)=format_rating(media,&info) else {return false;};
        let (mut min_w,mut min_h,mut max_w,mut max_h)=caps.as_ref()
            .map(|c|(c.MinOutputSize.cx,c.MinOutputSize.cy,c.MaxOutputSize.cx,c.MaxOutputSize.cy))
            .unwrap_or((info.bmiHeader.biWidth,info.bmiHeader.biHeight,info.bmiHeader.biWidth,info.bmiHeader.biHeight));
        if min_w==0||min_h==0||max_w==0||max_h==0 {
            min_w=info.bmiHeader.biWidth;max_w=min_w;
            min_h=info.bmiHeader.biHeight;max_h=min_h;
        }
        let cap=Capability {min_width:min_w,max_width:max_w,min_height:min_h,max_height:max_h,
            step_width:caps.as_ref().map_or(1,|c|c.OutputGranularityX.max(1)),
            step_height:caps.as_ref().map_or(1,|c|c.OutputGranularityY.max(1)),
            min_interval:caps.as_ref().map_or(10_010_000_000i64/60_000,|c|c.MinFrameInterval),
            max_interval:caps.as_ref().map_or(10_010_000_000i64/60_000,|c|c.MaxFrameInterval),format_rating:rating};
        let Some(choice)=choose(request,cap,info.AvgTimePerFrame) else {return false;};
        if best.as_ref().is_some_and(|(score,_)|*score<=choice.score) {return false;}
        if choice.width!=0 {info.bmiHeader.biWidth=choice.width;}
        if choice.height!=0 {info.bmiHeader.biHeight=choice.height;}
        info.AvgTimePerFrame=choice.interval;
        unsafe {std::ptr::write_unaligned((*candidate.0).pbFormat.cast::<VIDEOINFOHEADER>(),info)};
        let score=choice.score;best=Some((score,candidate));
        score==0
    };
    match unsafe {config.GetNumberOfCapabilities(&mut count,&mut bytes)} {
        Ok(()) => {
            if !(0..=1024).contains(&count) || bytes<std::mem::size_of::<VIDEO_STREAM_CONFIG_CAPS>() as i32 || bytes>64*1024 {
                return Err(CameraError::BudgetExceeded("DirectShow capability count or size"));
            }
            let mut payload=Vec::<u8>::new();payload.try_reserve_exact(bytes as usize)
                .map_err(|_|CameraError::BudgetExceeded("DirectShow capability buffer"))?;
            payload.resize(bytes as usize,0);
            for index in 0..count {
                let mut ptr=std::ptr::null_mut();
                let result=unsafe {config.GetStreamCaps(index,&mut ptr,payload.as_mut_ptr())};
                if ptr.is_null() {continue;}
                let candidate=DirectShowMediaType(ptr);
                if result.is_err(){continue;}
                let caps=unsafe {std::ptr::read_unaligned(payload.as_ptr().cast::<VIDEO_STREAM_CONFIG_CAPS>())};
                if consider(candidate,Some(caps)) {break;}
            }
        },
        Err(error) if error.code()==windows::Win32::Foundation::E_NOTIMPL => {
            // OBS's fallback for drivers without IAMStreamConfig capabilities:
            // enumerate the capture pin's actual media types in driver order.
            let types=unsafe {pin.EnumMediaTypes()}.map_err(|e|native_error("DirectShow media type enumeration",e))?;
            for index in 0..=1024 {
                let mut ptr=std::ptr::null_mut();
                let mut fetched=0u32;
                let result=unsafe {types.Next(std::slice::from_mut(&mut ptr),Some(&mut fetched))};
                if result.is_err() {
                    if !ptr.is_null(){drop(DirectShowMediaType(ptr));}
                    return Err(CameraError::Native {operation:"DirectShow media type enumeration",detail:format!("{result:?}")});
                }
                if ptr.is_null() {break;}
                let candidate=DirectShowMediaType(ptr);
                if index==1024{return Err(CameraError::BudgetExceeded("DirectShow media type count"));}
                if fetched!=1 {return Err(CameraError::InvalidFrame("DirectShow media type enumeration count"));}
                if consider(candidate,None) {break;}
            }
        },
        Err(error)=>return Err(native_error("DirectShow capability count",error)),
    }
    drop(consider);
    best.map(|(_,media)|media).ok_or(CameraError::Unavailable("DirectShow preferred has no supported capability range"))
}

fn attr(ty: &IMFMediaType, key: &GUID) -> Result<Option<u32>, &'static str> {
    match unsafe { ty.GetUINT32(key) } {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.code() == MF_E_ATTRIBUTENOTFOUND => Ok(None),
        Err(_) => Err("malformed native camera color/layout attribute"),
    }
}
fn native_pixel_format(ty: &IMFMediaType, subtype: GUID) -> Result<VideoPixelFormat, &'static str> {
    if attr(ty, &MF_MT_PAN_SCAN_ENABLED)?.unwrap_or(0) != 0 {
        return Err("native camera pan-scan requires explicit aperture support");
    }
    let size = unsafe { ty.GetUINT64(&MF_MT_FRAME_SIZE) }
        .map_err(|_| "missing native camera frame size")?;
    for key in [
        MF_MT_GEOMETRIC_APERTURE,
        MF_MT_MINIMUM_DISPLAY_APERTURE,
        MF_MT_PAN_SCAN_APERTURE,
    ] {
        match unsafe { ty.GetBlobSize(&key) } {
            Ok(16) => {
                let mut blob = [0u8; 16];
                let mut actual = 0;
                unsafe { ty.GetBlob(&key, &mut blob, Some(&mut actual)) }
                    .map_err(|_| "malformed native camera aperture")?;
                if actual != 16 || !full_aperture(&blob, (size >> 32) as u32, size as u32) {
                    return Err(
                        "cropped native camera aperture requires explicit aperture support",
                    );
                }
            }
            Err(e) if e.code() == MF_E_ATTRIBUTENOTFOUND => {}
            _ => return Err("malformed native camera aperture"),
        }
    }
    if !matches!(attr(ty, &MF_MT_INTERLACE_MODE)?.unwrap_or(0), 0 | 2) {
        return Err("interlaced camera modes require deinterlacing support");
    }
    if attr(ty, &MF_MT_VIDEO_ROTATION)?.unwrap_or(0) != 0 {
        return Err("rotated native camera modes require explicit rotation support");
    }
    match unsafe { ty.GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO) } {
        Ok(value)
            if (value >> 32) == 0
                || (value & 0xffff_ffff) == 0
                || (value >> 32) != (value & 0xffff_ffff) =>
        {
            return Err("non-square native camera pixels are unsupported");
        }
        Err(e) if e.code() != MF_E_ATTRIBUTENOTFOUND => {
            return Err("malformed native camera pixel aspect ratio");
        }
        _ => {}
    }
    if !matches!(
        attr(ty, &MF_MT_TRANSFER_FUNCTION)?.unwrap_or(0),
        0 | 4 | 5 | 7
    ) {
        return Err("native camera transfer function is outside the SDR profile");
    }
    if !matches!(
        attr(ty, &MF_MT_VIDEO_PRIMARIES)?.unwrap_or(0),
        0 | 2 | 3 | 4 | 5 | 8
    ) {
        return Err("native camera primaries are outside the SDR profile");
    }
    if (subtype == MFVideoFormat_RGB32
        || subtype == MFVideoFormat_ARGB32
        || subtype == MFVideoFormat_RGB24)
        && !matches!(attr(ty, &MF_MT_VIDEO_NOMINAL_RANGE)?.unwrap_or(0), 0 | 1)
    {
        return Err("limited-range native RGB camera modes are unsupported");
    }
    if subtype == MFVideoFormat_MJPG {
        return Ok(VideoPixelFormat::Mjpeg);
    }
    if subtype == MFVideoFormat_H264 {
        return Ok(VideoPixelFormat::H264);
    }
    if subtype == MFVideoFormat_RGB32 {
        return Ok(VideoPixelFormat::Rgb32);
    }
    if subtype == MFVideoFormat_ARGB32 {
        return Ok(VideoPixelFormat::Bgra8);
    }
    if subtype == MFVideoFormat_RGB24 {
        return Ok(VideoPixelFormat::Rgb24);
    }
    let packed_422 = subtype == MFVideoFormat_YUY2 || subtype == MFVideoFormat_UYVY;
    let planar_420 = subtype == MFVideoFormat_I420 || subtype == MFVideoFormat_IYUV || subtype == MFVideoFormat_YV12;
    if !packed_422 && !planar_420 && subtype != MFVideoFormat_NV12 {
        return Err("native camera subtype requires an unsupported decoder or conversion");
    }
    let matrix = match attr(ty, &MF_MT_YUV_MATRIX)?.unwrap_or(0) {
        0 | 1 => VideoColorMatrix::Bt709,
        2 => VideoColorMatrix::Bt601,
        _ => return Err("native camera YUV matrix is outside BT.601/BT.709"),
    };
    let range = match attr(ty, &MF_MT_VIDEO_NOMINAL_RANGE)?.unwrap_or(0) {
        0 | 2 => VideoColorRange::Limited,
        1 => VideoColorRange::Full,
        _ => return Err("native camera nominal range is unsupported"),
    };
    let chroma = attr(ty, &MF_MT_VIDEO_CHROMA_SITING)?.unwrap_or(0) & !8;
    let chroma_left = match chroma {
        0 => packed_422,
        1 => false,
        4 | 5 => true,
        6 | 7 if packed_422 => true,
        _ => return Err("native camera vertical chroma siting is unsupported"),
    };
    Ok(if subtype == MFVideoFormat_YUY2 {
        VideoPixelFormat::Yuy2 {
            matrix,
            range,
            chroma_left,
        }
    } else if subtype == MFVideoFormat_UYVY {
        VideoPixelFormat::Uyvy { matrix, range, chroma_left }
    } else if subtype == MFVideoFormat_YV12 {
        VideoPixelFormat::Yv12 { matrix, range, chroma_left }
    } else if planar_420 {
        VideoPixelFormat::I420 { matrix, range, chroma_left }
    } else {
        VideoPixelFormat::Nv12 { matrix, range, chroma_left }
    })
}
fn full_aperture(blob: &[u8; 16], width: u32, height: u32) -> bool {
    blob[..8] == [0; 8]
        && u32::from_le_bytes(blob[8..12].try_into().unwrap()) == width
        && u32::from_le_bytes(blob[12..16].try_into().unwrap()) == height
}
fn row_bytes(mode: VideoCaptureMode) -> usize {
    mode.width() as usize
        * match mode.format() {
            VideoPixelFormat::Mjpeg | VideoPixelFormat::H264 => 0,
            VideoPixelFormat::Rgb24 => 3,
            VideoPixelFormat::Yuy2 { .. } | VideoPixelFormat::Uyvy { .. } => 2,
            VideoPixelFormat::Nv12 { .. } | VideoPixelFormat::I420 { .. } | VideoPixelFormat::Yv12 { .. } => 1,
            _ => 4,
        }
}
fn backing_rows(mode: VideoCaptureMode) -> usize {
    if matches!(mode.format(),VideoPixelFormat::Mjpeg|VideoPixelFormat::H264) {return 0;}
    mode.height() as usize
        + if matches!(mode.format(), VideoPixelFormat::Nv12 { .. } | VideoPixelFormat::I420 { .. } | VideoPixelFormat::Yv12 { .. }) {
            mode.height() as usize / 2
        } else {
            0
        }
}
fn is_yuv(mode: VideoCaptureMode) -> bool {
    matches!(
        mode.format(),
        VideoPixelFormat::Yuy2 { .. } | VideoPixelFormat::Uyvy { .. } | VideoPixelFormat::I420 { .. } | VideoPixelFormat::Yv12 { .. } | VideoPixelFormat::Nv12 { .. }
    )
}

struct QueuedSample {
    sample: Option<IMFSample>,
    timestamp: i64,
    flags: u32,
}
// Media Foundation's asynchronous source-reader samples are transferred
// between MTA threads; interfaces are never used from a GUI/STA apartment.
unsafe impl Send for QueuedSample {}
struct CallbackState {
    producer: Mutex<Producer<QueuedSample>>,
    pending: AtomicBool,
    stopping: AtomicBool,
    flushed: AtomicBool,
    error: AtomicI32,
    dropped: AtomicU64,
    max_bytes: usize,
}
#[implement(IMFSourceReaderCallback)]
struct SampleCallback {
    state: Arc<CallbackState>,
}
impl IMFSourceReaderCallback_Impl for SampleCallback_Impl {
    fn OnReadSample(
        &self,
        status: HRESULT,
        _stream: u32,
        flags: u32,
        timestamp: i64,
        sample: Option<&IMFSample>,
    ) -> windows::core::Result<()> {
        // Publish the event before allowing another ReadSample request. This
        // keeps format changes ordered with the sample that reported them.
        let result = (|| {
        if self.state.stopping.load(Ordering::Acquire) {
            return Ok(());
        }
        if status.is_err() {
            self.state.error.store(status.0, Ordering::Release);
            return Ok(());
        }
        if flags
            & ((MF_SOURCE_READERF_ERROR.0
                | MF_SOURCE_READERF_ENDOFSTREAM.0) as u32)
            != 0
        {
            self.state
                .error
                .store(MF_E_INVALIDMEDIATYPE.0, Ordering::Release);
            return Ok(());
        }
        let format_changed = flags & ((MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0
            | MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED.0) as u32) != 0;
        // The sample on the boundary is not safe to interpret as either the
        // old or new layout. Do not retain it on the callback thread.
        let sample = if format_changed { None } else { sample };
        if let Some(sample) = sample {
        let length = match unsafe { sample.GetTotalLength() } {
            Ok(n) => n as usize,
            Err(error) => {
                self.state.error.store(error.code().0, Ordering::Release);
                return Ok(());
            }
        };
        if length == 0 || length > self.state.max_bytes {
            self.state.error.store(E_INVALIDARG.0, Ordering::Release);
            return Ok(());
        }
        }
        let Ok(mut producer) = self.state.producer.try_lock() else {
            self.state.error.store(E_INVALIDARG.0, Ordering::Release);
            return Ok(());
        };
        let Ok(mut chunk) = producer.write_chunk_uninit(1) else {
            self.state.error.store(E_INVALIDARG.0, Ordering::Release);
            return Ok(());
        };
        // Reserve the queue slot before AddRef: a full queue never creates an owned
        // COM value whose last Release could execute on this callback.
        chunk.as_mut_slices().0[0].write(QueuedSample {
            sample: sample.cloned(),
            timestamp,
            flags,
        });
        unsafe {
            chunk.commit_all();
        }
        Ok(())
        })();
        self.state.pending.store(false, Ordering::Release);
        result
    }
    fn OnFlush(&self, _stream: u32) -> windows::core::Result<()> {
        self.state.pending.store(false, Ordering::Release);
        self.state.flushed.store(true, Ordering::Release);
        Ok(())
    }
    fn OnEvent(&self, _stream: u32, event: Option<&IMFMediaEvent>) -> windows::core::Result<()> {
        if let Some(event) = event {
            if matches!(unsafe{event.GetType()},Ok(kind)if kind==MEVideoCaptureDeviceRemoved.0 as u32||kind==MEVideoCaptureDevicePreempted.0 as u32)
            {
                self.state
                    .error
                    .store(MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED.0, Ordering::Release);
            }
            if let Ok(status) = unsafe { event.GetStatus() } {
                if status.is_err() {
                    self.state.error.store(status.0, Ordering::Release);
                }
            }
        }
        Ok(())
    }
}
struct NativeStream {
    source: IMFMediaSource,
    reader: IMFSourceReader,
    _callback: IMFSourceReaderCallback,
    state: Arc<CallbackState>,
    consumer: Consumer<QueuedSample>,
    mode: VideoCaptureMode,
    stride: Option<i32>,
    avc: Option<avc::CameraAvcDecoder>,
    avc_ready: VecDeque<NativeVideoFrame>,
    // STREAMTICK and format-change notifications can have no sample. Carry
    // their discontinuity forward until a real frame reaches the owner.
    pending_discontinuity: bool,
    // The next read is issued only after the previous event was consumed.
    need_read: bool,
    stopping: bool,
    stopped: bool,
    flush_retry_at: Option<std::time::Instant>,
    _owner: Rc<()>,
}
impl NativeStream {
    fn is_stopping(&self) -> bool {
        // The callback gate closes before the asynchronous Flush is accepted.
        // A failed Flush can leave `self.stopping` false while retry is pending;
        // the stream must nevertheless reject all new work during that period.
        self.stopping || self.state.stopping.load(Ordering::Acquire)
    }
    fn request_next(&mut self) -> Result<(), CameraError> {
        if self.is_stopping() || self.state.pending.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        if let Err(error) = unsafe { self.reader.ReadSample(STREAM, 0, None, None, None, None) } {
            self.state.pending.store(false, Ordering::Release);
            return Err(native_error("camera ReadSample", error));
        }
        Ok(())
    }
    fn resume_read(&mut self) -> Result<(), CameraError> {
        if self.need_read && !self.state.pending.load(Ordering::Acquire) {
            self.request_next()?;
            self.need_read = false;
        }
        Ok(())
    }
}
#[path = "native_windows_controls.rs"]
mod driver_controls;
impl NativeCameraStream for NativeStream {
    fn enumerate_modes(&mut self,limits:CameraLimits)->Result<Vec<CameraModeInfo>,CameraError> {
        if self.is_stopping() {return Err(CameraError::Closed);}
        media_types(&self.reader,limits).map(|types|types.into_iter().map(|(_,info)|info).collect())
    }
    fn query_controls(&mut self) -> Result<ControlQuery, CameraError> {
        if self.is_stopping() { return Err(CameraError::Closed); }
        Ok(driver_controls::query(&self.source))
    }
    fn set_control(&mut self, change: ControlChange, gate: &ControlWriteGate) -> ControlReceipt {
        if self.is_stopping() { return ControlReceipt::before(change, "camera is stopping"); }
        driver_controls::set(&self.source, change, gate)
    }
    fn poll(&mut self, limits: CameraLimits) -> Result<Option<NativeVideoFrame>, CameraError> {
        if self.is_stopping() {
            return Err(CameraError::Closed);
        }
        let status = HRESULT(self.state.error.swap(0, Ordering::AcqRel));
        if status.is_err() {
            return Err(native_error(
                "camera sample callback",
                windows::core::Error::from_hresult(status),
            ));
        }
        if let Some(frame)=self.avc_ready.pop_front() { return Ok(Some(frame)); }
        if let Some(avc)=&mut self.avc {
            self.avc_ready.extend(avc.drain()?.into_iter().map(|(frame,_)|frame));
            if let Some(frame)=self.avc_ready.pop_front(){return Ok(Some(frame));}
        }
        self.resume_read()?;
        let sample = self.consumer.pop().ok();
        let Some(sample) = sample else {
            return Ok(None);
        };
        if sample.flags & ((MF_SOURCE_READERF_STREAMTICK.0
            | MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0
            | MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED.0) as u32) != 0 {
            self.pending_discontinuity = true;
        }
        if sample.flags & ((MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0
            | MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED.0) as u32) != 0 {
            let actual = unsafe { self.reader.GetCurrentMediaType(STREAM) }
                .map_err(|e| native_error("verify camera mode after format change", e))?;
            let info = mode_info(&actual)?;
            if info.mode != Some(self.mode) {
                return Err(CameraError::Native {
                    operation: "verify camera mode after format change",
                    detail: format!("selected {:?}; actual {}x{} {}/{} fps {}, unsupported reason: {:?}",
                        self.mode, info.width, info.height, info.fps_num, info.fps_den,
                        info.native_subtype, info.unsupported_reason),
                });
            }
            self.stride = checked_stride(&actual, self.mode, limits)?;
            if self.mode.format()==VideoPixelFormat::H264 {
                // Retire the old codec before constructing the new one so two
                // decoded-picture buffers never coexist under one admission.
                self.avc_ready.clear();
                self.avc.take();
                self.avc=Some(avc::CameraAvcDecoder::new(self.mode.width(),self.mode.height(),limits,avc_configuration(&actual)?)?);
            }
            // A sample delivered with the change may belong to either side of
            // the transition. Discard it before requesting the next one.
            self.need_read = true;
            self.resume_read()?;
            return Ok(None);
        }
        self.need_read = true;
        let Some(raw) = sample.sample else { self.resume_read()?; return Ok(None); };
        // MF documents this optional device timestamp in the QPC epoch, in
        // 100 ns units. Missing or overflowing metadata is not a clock proof.
        let device_qpc_ns = unsafe { raw.GetUINT64(&MFSampleExtension_DeviceTimestamp) }
            .ok().and_then(|timestamp| timestamp.checked_mul(100));
        if self.avc.is_some() {
            let bytes=copy_avc_access_unit(&raw,limits)?;
            let dropped=self.state.dropped.swap(0,Ordering::AcqRel);
            let sample_discontinuity=unsafe {raw.GetUINT32(&MFSampleExtension_Discontinuity)}.unwrap_or(0)!=0;
            let discontinuity=std::mem::take(&mut self.pending_discontinuity) || sample_discontinuity || dropped!=0;
            let decoded=self.avc.as_mut().ok_or(CameraError::InvalidFrame("missing AVC camera decoder"))?
                .submit(&bytes,sample.timestamp,device_qpc_ns,None,discontinuity,dropped)?;
            self.avc_ready.extend(decoded.into_iter().map(|(frame,_)|frame));
            self.resume_read()?;
            return Ok(self.avc_ready.pop_front());
        }
        let image = copy_sample(&raw, self.mode, self.stride, limits)?;
        self.resume_read()?;
        let presentation_ns = i128::from(sample.timestamp) * 100;
        let dropped = self.state.dropped.swap(0, Ordering::AcqRel);
        Ok(Some(NativeVideoFrame {
            image,
            presentation_ns,
            device_qpc_ns,
            directshow_clock: None,
            discontinuity: std::mem::take(&mut self.pending_discontinuity),
            dropped_frames: dropped,
        }))
    }
    fn stop(&mut self) -> Result<bool, CameraError> {
        if self.stopped {
            return Ok(true);
        }
        if !self.stopping {
            self.state.stopping.store(true, Ordering::Release);
            // A failed Flush must not mark the request as submitted: the
            // owner will retry, but avoid hammering a failing camera driver.
            if !self.state.flushed.load(Ordering::Acquire) {
                if self.flush_retry_at.is_some_and(|deadline|std::time::Instant::now()<deadline) {
                    return Ok(false);
                }
                if let Err(error)=unsafe { self.reader.Flush(STREAM) } {
                    self.flush_retry_at=Some(std::time::Instant::now()+std::time::Duration::from_millis(100));
                    return Err(native_error("camera Flush", error));
                }
            }
            self.stopping = true;
            self.flush_retry_at = None;
        }
        while self.consumer.pop().is_ok() {}
        self.avc_ready.clear();
        if !self.state.flushed.load(Ordering::Acquire) {
            return Ok(false);
        }
        unsafe { self.source.Shutdown() }
            .map_err(|e| native_error("camera media source Shutdown", e))?;
        self.stopped = true;
        Ok(true)
    }
}
impl Drop for NativeStream {
    fn drop(&mut self) {
        while !self.stopped {
            match self.stop() {
                Ok(true) => break,
                _ => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        while self.consumer.pop().is_ok() {}
    }
}

/// Copy the compressed sample before releasing the native buffer; decoding
/// runs on the owner after the driver-owned memory has been unlocked.
fn copy_avc_access_unit(sample:&IMFSample,limits:CameraLimits)->Result<Vec<u8>,CameraError> {
    if unsafe {sample.GetBufferCount()}.map_err(|e|native_error("AVC camera buffer count",e))?!=1 {
        return Err(CameraError::InvalidFrame("AVC camera requires one contiguous native buffer"));
    }
    let buffer=unsafe {sample.GetBufferByIndex(0)}.map_err(|e|native_error("AVC camera buffer",e))?;
    let (mut start,mut maximum,mut length)=(ptr::null_mut(),0,0);
    unsafe {buffer.Lock(&mut start,Some(&mut maximum),Some(&mut length))}
        .map_err(|e|native_error("AVC camera buffer Lock",e))?;
    let guard=BufferLock(buffer);
    let result=(|| {
        if start.is_null() || length==0 || length>maximum || length as usize>avc::compressed_cap()
            || maximum as usize>limits.max_raw_bytes/3 {
            return Err(CameraError::BudgetExceeded("AVC native compressed buffer"));
        }
        let mut bytes=Vec::new();
        bytes.try_reserve_exact(length as usize).map_err(|_|CameraError::BudgetExceeded("AVC sample copy allocation"))?;
        bytes.extend_from_slice(unsafe {std::slice::from_raw_parts(start,length as usize)});
        Ok(bytes)
    })();
    drop(guard);
    result
}

fn copy_sample(
    sample: &IMFSample,
    mode: VideoCaptureMode,
    stride: Option<i32>,
    limits: CameraLimits,
) -> Result<RgbaFrame, CameraError> {
    if unsafe { sample.GetBufferCount() }.map_err(|e| native_error("camera buffer count", e))? != 1
    {
        return Err(CameraError::InvalidFrame(
            "camera profile requires one contiguous native buffer",
        ));
    }
    let buffer =
        unsafe { sample.GetBufferByIndex(0) }.map_err(|e| native_error("camera buffer", e))?;
    if mode.format()==VideoPixelFormat::Mjpeg {
        let (mut start,mut maximum,mut length)=(ptr::null_mut(),0,0);
        unsafe {buffer.Lock(&mut start,Some(&mut maximum),Some(&mut length))}.map_err(|e|native_error("MJPEG buffer Lock",e))?;
        let guard=BufferLock(buffer);
        let result=(||{
            let cap=mjpeg::compressed_cap(mode,limits)?;
            if start.is_null()||length==0||length>maximum||length as usize>cap||maximum as usize>mjpeg::native_cap(mode,limits)? {return Err(CameraError::BudgetExceeded("MJPEG native compressed buffer length/capacity"));}
            let bytes=unsafe {std::slice::from_raw_parts(start,length as usize)};
            mjpeg::decode(bytes,mode,limits)
        })();
        drop(guard);
        return result;
    }
    if let Ok(two) = buffer.cast::<IMF2DBuffer2>() {
        let (mut scan, mut pitch, mut start, mut length) = (ptr::null_mut(), 0, ptr::null_mut(), 0);
        unsafe {
            two.Lock2DSize(
                MF2DBuffer_LockFlags_Read,
                &mut scan,
                &mut pitch,
                &mut start,
                &mut length,
            )
        }
        .map_err(|e| native_error("camera Lock2DSize", e))?;
        let guard = TwoLock(two);
        let result = copy_locked(start, length as usize, scan, pitch, mode, limits);
        drop(guard);
        result
    } else {
        let stride = stride.ok_or(CameraError::InvalidFrame(
            "camera buffer has no explicit stride/orientation",
        ))?;
        let (mut start, mut maximum, mut length) = (ptr::null_mut(), 0, 0);
        unsafe { buffer.Lock(&mut start, Some(&mut maximum), Some(&mut length)) }
            .map_err(|e| native_error("camera buffer Lock", e))?;
        let guard = BufferLock(buffer);
        let result = (|| {
            let required = (stride.unsigned_abs() as usize)
                .checked_mul(backing_rows(mode))
                .ok_or(CameraError::BudgetExceeded("camera stride"))?;
            if (length as usize) < required || length > maximum || start.is_null() {
                return Err(CameraError::InvalidFrame("short native camera buffer"));
            }
            let scan = if stride < 0 {
                unsafe { start.add((mode.height() as usize - 1) * stride.unsigned_abs() as usize) }
            } else {
                start
            };
            copy_locked(start, length as usize, scan, stride, mode, limits)
        })();
        drop(guard);
        result
    }
}
struct TwoLock(IMF2DBuffer2);
impl Drop for TwoLock {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.Unlock2D();
        }
    }
}
struct BufferLock(IMFMediaBuffer);
impl Drop for BufferLock {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.Unlock();
        }
    }
}
fn copy_locked(
    start: *mut u8,
    length: usize,
    scan: *mut u8,
    pitch: i32,
    mode: VideoCaptureMode,
    limits: CameraLimits,
) -> Result<RgbaFrame, CameraError> {
    if matches!(mode.format(),VideoPixelFormat::Mjpeg|VideoPixelFormat::H264) {return Err(CameraError::InvalidFrame("compressed camera transport cannot use a raw pixel layout"));}
    let stride = pitch.unsigned_abs() as usize;
    let bytes = stride
        .checked_mul(backing_rows(mode))
        .ok_or(CameraError::BudgetExceeded("camera stride bytes"))?;
    let offset = if pitch < 0 {
        stride
            .checked_mul(mode.height() as usize - 1)
            .ok_or(CameraError::BudgetExceeded("camera scanline offset"))?
    } else {
        0
    };
    let low = (scan as usize)
        .checked_sub(offset)
        .ok_or(CameraError::InvalidFrame("camera scanline underflow"))?;
    let end = (start as usize)
        .checked_add(length)
        .ok_or(CameraError::InvalidFrame("camera buffer pointer overflow"))?;
    if start.is_null()
        || scan.is_null()
        || stride < row_bytes(mode)
        || (is_yuv(mode) && pitch <= 0)
        || (matches!(mode.format(), VideoPixelFormat::I420 { .. } | VideoPixelFormat::Yv12 { .. }) && stride % 2 != 0)
        || length > limits.max_raw_bytes
        || low < start as usize
        || low.checked_add(bytes).is_none_or(|high| high > end)
    {
        return Err(CameraError::InvalidFrame(
            "camera buffer pointer, stride or length",
        ));
    }
    let raw = unsafe { std::slice::from_raw_parts(low as *const u8, bytes) };
    let native = match mode.format() {
        VideoPixelFormat::Rgb24 => Some(CameraPixelFormat::Bgr24),
        VideoPixelFormat::Yuy2 {
            matrix,
            range,
            chroma_left,
        }
        | VideoPixelFormat::Uyvy { matrix, range, chroma_left }
        | VideoPixelFormat::I420 { matrix, range, chroma_left }
        | VideoPixelFormat::Yv12 { matrix, range, chroma_left }
        | VideoPixelFormat::Nv12 {
            matrix,
            range,
            chroma_left,
        } => {
            let matrix = match matrix {
                VideoColorMatrix::Bt601 => CameraMatrix::Bt601,
                VideoColorMatrix::Bt709 => CameraMatrix::Bt709,
            };
            let range = match range {
                VideoColorRange::Limited => CameraRange::Limited,
                VideoColorRange::Full => CameraRange::Full,
            };
            Some(match mode.format() {
                VideoPixelFormat::Yuy2 { .. } => CameraPixelFormat::Yuy2 { matrix, range, chroma_left },
                VideoPixelFormat::Uyvy { .. } => CameraPixelFormat::Uyvy { matrix, range, chroma_left },
                VideoPixelFormat::I420 { .. } => CameraPixelFormat::I420 { matrix, range, chroma_left },
                VideoPixelFormat::Yv12 { .. } => CameraPixelFormat::Yv12 { matrix, range, chroma_left },
                _ => CameraPixelFormat::Nv12 { matrix, range, chroma_left },
            })
        }
        _ => None,
    };
    if let Some(format) = native {
        return ingest_camera_frame(
            raw,
            CameraFrameLayout {
                width: mode.width(),
                height: mode.height(),
                stride_bytes: stride,
                row_order: if pitch < 0 {
                    RowOrder::BottomUp
                } else {
                    RowOrder::TopDown
                },
                format,
            },
            PackedFrameLimits {
                max_input_bytes: limits.max_raw_bytes,
                max_output_bytes: limits.max_frame_bytes,
                max_pixel_visits: u64::from(mode.width()) * u64::from(mode.height()) * 6,
            },
        )
        .map_err(|_| CameraError::InvalidFrame("native camera pixel conversion"));
    }
    ingest_packed_frame(
        raw,
        PackedFrameLayout {
            width: mode.width(),
            height: mode.height(),
            stride_bytes: stride,
            format: PackedPixelFormat::Bgra8,
            row_order: if pitch < 0 {
                RowOrder::BottomUp
            } else {
                RowOrder::TopDown
            },
            alpha: if mode.format() == VideoPixelFormat::Rgb32 {
                AlphaMode::Opaque
            } else {
                AlphaMode::Straight
            },
        },
        PackedFrameLimits {
            max_input_bytes: limits.max_raw_bytes,
            max_output_bytes: limits.max_frame_bytes,
            max_pixel_visits: u64::from(mode.width()) * u64::from(mode.height()) * 2,
        },
    )
    .map_err(|_| CameraError::InvalidFrame("camera packed RGB pixels"))
}

/// DirectShow owner-thread conversion of an already owned, bounded sample.
/// `pitch` is the negotiated stride; negative RGB pitch denotes bottom-up.
/// No pointer obtained from the driver escapes this call.
pub(in crate::camera_capture) fn decode_directshow_buffer(raw:&[u8],mode:VideoCaptureMode,pitch:i32,limits:CameraLimits)->Result<RgbaFrame,CameraError> {
    if mode.format()==VideoPixelFormat::Mjpeg {return mjpeg::decode(raw,mode,limits);}
    if mode.format()==VideoPixelFormat::H264 {return Err(CameraError::Unavailable("DirectShow AVC camera decode is not yet supported"));}
    if pitch==0 {return Err(CameraError::InvalidFrame("DirectShow camera stride is zero"));}
    let offset=if pitch<0 {(pitch.unsigned_abs() as usize).checked_mul(mode.height().saturating_sub(1) as usize)
        .ok_or(CameraError::BudgetExceeded("DirectShow camera scanline offset"))?}else{0};
    if offset>raw.len(){return Err(CameraError::InvalidFrame("DirectShow camera scanline outside sample"));}
    let start=raw.as_ptr() as *mut u8;
    // `copy_locked` validates all offsets and lengths before making a slice;
    // the borrowed buffer remains alive and immutable throughout conversion.
    copy_locked(start,raw.len(),start.wrapping_add(offset),pitch,mode,limits)
}

/// Validate one DirectShow media type against an already authored mode and
/// derive a bounded system-memory layout for the shared graph. The caller
/// owns the AM_MEDIA_TYPE throughout this call and has initialized MF.
pub(in crate::camera_capture) unsafe fn directshow_video_layout(raw:*const AM_MEDIA_TYPE,expected:VideoCaptureMode,limits:CameraLimits)->Result<(i32,usize,Option<Vec<u8>>),CameraError> {
    if raw.is_null(){return Err(CameraError::InvalidFrame("DirectShow video media type is null"));}
    let data=unsafe {&*raw};
    if data.cbFormat==0 || data.cbFormat>65_536 || data.pbFormat.is_null() || data.pUnk.is_some(){return Err(CameraError::InvalidFrame("DirectShow video media type payload"));}
    let media=unsafe {MFCreateMediaType()}.map_err(|e|native_error("DirectShow video media type",e))?;
    unsafe {MFInitMediaTypeFromAMMediaType(&media,raw)}.map_err(|e|native_error("DirectShow video media conversion",e))?;
    let mode=mode_info(&media)?.mode.ok_or(CameraError::ModeNotFound)?;
    if !super::super::selection::directshow_mode_matches(mode,expected){return Err(CameraError::ModeNotFound);}
    if mode.format()==VideoPixelFormat::Mjpeg {return Ok((0,mjpeg::compressed_cap(mode,limits)?,None));}
    if mode.format()==VideoPixelFormat::H264 {
        avc::reservation(mode.width(),mode.height(),limits)?;
        return Ok((0,avc::compressed_cap(),avc_configuration(&media)?));
    }
    let mut stride=checked_stride(&media,mode,limits)?.ok_or(CameraError::InvalidFrame("DirectShow video stride unavailable"))?;
    if matches!(mode.format(),VideoPixelFormat::Bgra8|VideoPixelFormat::Rgb32|VideoPixelFormat::Rgb24) {
        let height=if data.formattype==FORMAT_VideoInfo && data.cbFormat as usize>=std::mem::size_of::<VIDEOINFOHEADER>() {
            unsafe {std::ptr::read_unaligned(data.pbFormat.cast::<VIDEOINFOHEADER>())}.bmiHeader.biHeight
        }else if data.formattype==FORMAT_VideoInfo2 && data.cbFormat as usize>=std::mem::size_of::<VIDEOINFOHEADER2>() {
            unsafe {std::ptr::read_unaligned(data.pbFormat.cast::<VIDEOINFOHEADER2>())}.bmiHeader.biHeight
        }else{return Err(CameraError::InvalidFrame("DirectShow RGB orientation unavailable"));};
        if height==0 || height.unsigned_abs()!=mode.height(){return Err(CameraError::InvalidFrame("DirectShow RGB orientation mismatch"));}
        let magnitude=i32::try_from(stride.unsigned_abs()).map_err(|_|CameraError::InvalidFrame("DirectShow RGB stride"))?;
        stride=if height>0 {-magnitude}else{magnitude};
    }
    let bytes=(stride.unsigned_abs() as usize).checked_mul(backing_rows(mode)).ok_or(CameraError::BudgetExceeded("DirectShow video sample bytes"))?;
    if bytes==0 || bytes>limits.max_raw_bytes {return Err(CameraError::BudgetExceeded("DirectShow video sample bytes"));}
    Ok((stride,bytes,None))
}

fn native_error(operation: &'static str, error: windows::core::Error) -> CameraError {
    match error.code() {
        E_ACCESSDENIED => CameraError::PermissionDenied,
        MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED => CameraError::DeviceLost,
        _ => CameraError::Native {
            operation,
            detail: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_aperture_metadata_requires_full_unshifted_video_not_implicit_crop() {
        let mut full = [0u8; 16];
        full[8..12].copy_from_slice(&640u32.to_le_bytes());
        full[12..16].copy_from_slice(&480u32.to_le_bytes());
        assert!(full_aperture(&full, 640, 480));
        full[0] = 1;
        assert!(!full_aperture(&full, 640, 480));
        full[0] = 0;
        assert!(!full_aperture(&full, 1280, 720));
    }
    #[test]
    fn yuy2_and_nv12_native_layout_bounds_and_literal_pixels_need_no_os() {
        let format = VideoPixelFormat::Yuy2 {
            matrix: VideoColorMatrix::Bt601,
            range: VideoColorRange::Limited,
            chroma_left: true,
        };
        let mode = VideoCaptureMode::new(2, 1, 30, 1, format).unwrap();
        let mut raw = [81, 90, 81, 240, 9, 9, 9, 9];
        let start = raw.as_mut_ptr();
        assert_eq!(
            copy_locked(start, 8, start, 8, mode, CameraLimits::default())
                .unwrap()
                .pixels(),
            &[254, 0, 0, 255, 254, 0, 0, 255]
        );
        assert!(copy_locked(start, 8, start, -8, mode, CameraLimits::default()).is_err());
        let mode = VideoCaptureMode::new(
            2,
            2,
            30,
            1,
            VideoPixelFormat::Nv12 {
                matrix: VideoColorMatrix::Bt709,
                range: VideoColorRange::Limited,
                chroma_left: false,
            },
        )
        .unwrap();
        let mut raw = [16, 235, 0, 0, 235, 16, 0, 0, 128, 128, 0, 0];
        let start = raw.as_mut_ptr();
        assert_eq!(
            copy_locked(start, 12, start, 4, mode, CameraLimits::default())
                .unwrap()
                .pixels(),
            &[
                0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255
            ]
        );
        assert!(copy_locked(start, 11, start, 4, mode, CameraLimits::default()).is_err());
    }
    #[test]
    fn rgb24_native_padded_signed_stride_preserves_channel_and_orientation() {
        assert_eq!(
            MFVideoFormat_RGB24.data1, 20,
            "SDK subtype is D3DFMT_R8G8B8"
        );
        let mode = VideoCaptureMode::new(1, 2, 30, 1, VideoPixelFormat::Rgb24).unwrap();
        let mut raw = [255, 0, 0, 9, 0, 0, 255, 9];
        let start = raw.as_mut_ptr();
        assert_eq!(
            copy_locked(
                start,
                8,
                unsafe { start.add(4) },
                -4,
                mode,
                CameraLimits::default()
            )
            .unwrap()
            .pixels(),
            &[255, 0, 0, 255, 0, 0, 255, 255]
        );
    }
    #[test]
    fn bottom_up_rgb32_padding_and_unused_alpha_are_decoded_without_native_calls() {
        let mut bytes = [
            255, 0, 0, 0, 255, 255, 255, 1, 9, 9, 9, 9, 0, 0, 255, 0, 0, 255, 0, 2, 8, 8, 8, 8,
        ];
        let mode = VideoCaptureMode::new(2, 2, 30, 1, VideoPixelFormat::Rgb32).unwrap();
        let start = bytes.as_mut_ptr();
        let scan = unsafe { start.add(12) };
        let frame =
            copy_locked(start, bytes.len(), scan, -12, mode, CameraLimits::default()).unwrap();
        assert_eq!(
            frame.pixels(),
            &[
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255
            ]
        );
    }
    #[test]
    fn native_buffer_pointer_and_byte_admission_fail_before_access() {
        let mut bytes = [0u8; 16];
        let start = bytes.as_mut_ptr();
        let mode = VideoCaptureMode::new(2, 2, 30, 1, VideoPixelFormat::Rgb32).unwrap();
        assert!(
            copy_locked(
                start,
                16,
                unsafe { start.add(16) },
                8,
                mode,
                CameraLimits::default()
            )
            .is_err()
        );
        assert!(copy_locked(start, 15, start, 8, mode, CameraLimits::default()).is_err());
        assert!(
            copy_locked(
                start,
                16,
                start,
                8,
                mode,
                CameraLimits {
                    max_raw_bytes: 15,
                    ..CameraLimits::default()
                }
            )
            .is_err()
        );
        assert!(copy_locked(start, 16, start, 4, mode, CameraLimits::default()).is_err());
    }
}
