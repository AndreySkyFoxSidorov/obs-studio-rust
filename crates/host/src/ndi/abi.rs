//! Exact C ABI fields from DistroAV lib/ndi/Processing.NDI.{structs,Find,Recv,Send}.h.
//! NDI SDK header declarations copyright2023-2026 Vizrt NDI AB, MIT licensed
//! for these headers; see NOTICE-NDI.md. Runtime is separately installed/licensed.
use std::ffi::{c_char,c_void};
pub type Instance=*mut c_void;
#[repr(C)] pub struct Source {pub name:*const c_char,pub url:*const c_char}
#[repr(C)] pub struct FindCreate {pub local:bool,pub groups:*const c_char,pub extra_ips:*const c_char}
#[repr(C)] pub struct RecvCreate {pub source:Source,pub color:i32,pub bandwidth:i32,pub fields:bool,pub name:*const c_char}
#[repr(C)] pub struct Video {
    pub width:i32,pub height:i32,pub fourcc:u32,pub fps_num:i32,pub fps_den:i32,
    pub aspect:f32,pub frame_format:i32,pub timecode:i64,pub data:*mut u8,
    pub stride:i32,pub metadata:*const c_char,pub timestamp:i64,
}
#[repr(C)] pub struct Audio {
    pub rate:i32,pub channels:i32,pub samples:i32,pub timecode:i64,pub fourcc:u32,
    pub data:*mut u8,pub channel_stride:i32,pub metadata:*const c_char,pub timestamp:i64,
}
#[repr(C)] pub struct Metadata {pub length:i32,pub timecode:i64,pub data:*mut c_char}
pub type Initialize=unsafe extern "C" fn()->bool;
pub type Destroy=unsafe extern "C" fn();
pub type Version=unsafe extern "C" fn()->*const c_char;
pub type FindNew=unsafe extern "C" fn(*const FindCreate)->Instance;
pub type FindFree=unsafe extern "C" fn(Instance);
pub type FindSources=unsafe extern "C" fn(Instance,*mut u32)->*const Source;
pub type FindWait=unsafe extern "C" fn(Instance,u32)->bool;
pub type RecvNew=unsafe extern "C" fn(*const RecvCreate)->Instance;
pub type RecvFree=unsafe extern "C" fn(Instance);
pub type Capture=unsafe extern "C" fn(Instance,*mut Video,*mut Audio,*mut Metadata,u32)->i32;
pub type FreeVideo=unsafe extern "C" fn(Instance,*const Video);
pub type FreeAudio=unsafe extern "C" fn(Instance,*const Audio);
pub type FreeMetadata=unsafe extern "C" fn(Instance,*const Metadata);
pub type Connections=unsafe extern "C" fn(Instance)->i32;

// Direct sender declarations from Processing.NDI.Send.h (same header MIT notice).
#[repr(C)] pub struct SendCreate {pub name:*const c_char,pub groups:*const c_char,pub clock_video:bool,pub clock_audio:bool}
pub type SendNew=unsafe extern "C" fn(*const SendCreate)->Instance;
pub type SendFree=unsafe extern "C" fn(Instance);
pub type SendVideoAsync=unsafe extern "C" fn(Instance,*const Video);
pub type SendAudio=unsafe extern "C" fn(Instance,*const Audio);
pub type SendConnections=unsafe extern "C" fn(Instance,u32)->i32;
pub type SendSource=unsafe extern "C" fn(Instance)->*const Source;
