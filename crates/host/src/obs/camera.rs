//! Exact DirectShow camera mapping against an explicitly supplied device catalog.
//! No device is opened by the OBS parser. A caller must obtain this data from
//! its camera owner, including whether the device exposes embedded audio.
use super::*;
use skylive_core::{CameraFpsPolicy,CameraModePolicy,VideoCaptureBinding,VideoCaptureMode,VideoCaptureTarget,VideoPixelFormat,VideoColorMatrix,VideoColorRange};

/// One enumerated Media Foundation device and its native capture modes. The
/// caller must set `video_only` only when it has established that the original
/// DirectShow device has no attached audio. An unknown audio capability must be
/// represented as false, preventing silent loss of camera audio.
#[derive(Clone, Debug)]
pub struct CameraImportDevice {
    pub target: VideoCaptureTarget,
    pub modes: Vec<VideoCaptureMode>,
    pub video_only: bool,
    /// True only after a camera-owner probe observed an audio output pin.
    pub embedded_audio: bool,
    pub preferred: Option<VideoCaptureMode>,
    /// Exact DirectShow identity to WASAPI endpoint mappings resolved on the I/O owner.
    pub audio_mappings: std::sync::Arc<[CameraAudioMapping]>,
}
#[derive(Clone, Debug)]
pub struct CameraAudioMapping { pub name:String, pub path:String, pub endpoint_id:String }
impl CameraImportDevice {
    /// Construct an immutable catalog entry from a completed CameraOwner modes
    /// response. Audio capability is unknown: callers may import only sources
    /// whose OBS settings explicitly disable custom audio until DirectShow
    /// audio enumeration is wired in. No camera is opened here.
    pub fn from_owner_modes(target: VideoCaptureTarget, modes: &[crate::camera_capture::CameraModeInfo]) -> Result<Self, ObsError> {
        if modes.len()>1024 {return Err(issue("$.sources", "camera_catalog_limit", "camera mode catalog exceeds its bounded size"));}
        let mut supported=Vec::new();
        supported.try_reserve_exact(modes.len()).map_err(|_|issue("$.sources", "camera_catalog_limit", "cannot allocate camera mode catalog"))?;
        for info in modes {
            if let Some(mode)=info.mode {
                if info.unsupported_reason.is_some() || (mode.width(),mode.height(),mode.fps_num(),mode.fps_den()) != (info.width,info.height,info.fps_num,info.fps_den) {
                    return Err(issue("$.sources", "camera_catalog_invalid", "camera owner mode metadata is inconsistent"));
                }
                supported.push(mode);
            }
        }
        Ok(Self {target,modes:supported,video_only:false,embedded_audio:false,preferred:None,audio_mappings:std::sync::Arc::from([])})
    }
    pub fn with_preferred(mut self, result: &crate::camera_capture::CameraModeInfo) -> Result<Self, ObsError> {
        let mode=result.mode.ok_or_else(||issue("$.sources", "camera_preferred_unsupported", "DirectShow preferred mode is unsupported"))?;
        if result.unsupported_reason.is_some() {
            return Err(issue("$.sources", "camera_preferred_unresolved", "DirectShow preferred mode has no exact Media Foundation equivalent"));
        }
        let actual=self.modes.iter().copied().find(|candidate|crate::camera_capture::directshow_mode_matches(*candidate,mode))
            .ok_or_else(||issue("$.sources", "camera_preferred_unresolved", "DirectShow preferred mode has no exact Media Foundation equivalent"))?;
        self.preferred=Some(actual);Ok(self)
    }
}

fn decode(value:&str,path:&str)->Result<String,ObsError>{
    // OBS encode-dstr.hpp first escapes '#' as #22 and ':' as #3A.
    let mut result=String::with_capacity(value.len());
    let mut chars=value.chars();
    while let Some(ch)=chars.next(){
        if ch!='#' {result.push(ch);continue;}
        match (chars.next(),chars.next()) {
            (Some('2'),Some('2'))=>result.push('#'),
            (Some('3'),Some('A'))=>result.push(':'),
            _=>return Err(issue(path,"camera_device_id_invalid","OBS camera ID has an invalid escape")),
        }
    }
    Ok(result)
}
pub(super) fn requested_audio(settings:&Map<String,Value>,path:&str)->Result<Option<(String,String)>,ObsError>{
    if !boolean(settings,"use_custom_audio_device",false,path)? {return Ok(None);}
    let raw=match settings.get("audio_device_id"){None=>"",Some(value)=>value.as_str().ok_or_else(||issue(path,"camera_audio_invalid","Camera audio identity must be a string"))?};
    if raw.is_empty(){return Ok(None);}
    if raw.len()>8192{return Err(issue(path,"camera_audio_invalid","OBS camera audio identity exceeds 8192 bytes"));}
    let (name,device_path)=raw.split_once(':').ok_or_else(||issue(path,"camera_audio_invalid","OBS camera audio identity has no separator"))?;
    let name=decode(name,path)?;let device_path=decode(device_path,path)?;
    if name.is_empty(){return Err(issue(path,"camera_audio_invalid","OBS camera audio identity has no name"));}
    Ok(Some((name,device_path)))
}

/// Recover only the identity encoded in an OBS device ID. The owner still
/// verifies that this exact Windows path exists when querying the device.
pub(super) fn target(settings:&Map<String,Value>, path:&str)->Result<VideoCaptureTarget,ObsError>{
    let encoded=string(settings,"video_device_id",path)?;
    if encoded.len()>8192{return Err(issue(path,"camera_device_id_invalid","OBS camera identity is too long"));}
    let (name,device_path)=encoded.split_once(':').ok_or_else(||issue(path,"camera_device_id_invalid","OBS camera identity has no device path"))?;
    let name=decode(name,path)?;let device_path=decode(device_path,path)?;
    if name.is_empty()||device_path.is_empty(){return Err(issue(path,"camera_device_id_invalid","OBS camera identity requires both name and device path"));}
    Ok(VideoCaptureTarget::Windows{symbolic_link:device_path})
}

pub(super) fn parse(settings:&Map<String,Value>,path:&str,devices:&[CameraImportDevice])->Result<VideoCaptureBinding,ObsError>{
    if !cfg!(target_os="windows"){return Err(issue(path,"camera_platform_unsupported","DirectShow camera import requires Windows"));}
    if devices.len()>256 || devices.iter().any(|device|device.modes.len()>1024) {
        return Err(issue(path,"camera_catalog_limit","camera import catalog exceeds its bounded size"));
    }
    known(settings,&["video_device_id","last_video_device_id","res_type","resolution","last_resolution","frame_interval","video_format","buffering","flip_vertically","audio_output_mode","use_custom_audio_device","audio_device_id","color_space","color_range","deactivate_when_not_showing","autorotation","hw_decode","active"],path)?;
    let VideoCaptureTarget::Windows{symbolic_link:device_path}=target(settings,path)? else { unreachable!("OBS DirectShow identities are Windows paths") };
    // libdshowcapture only requests CameraControl_Roll for the exact
    // case-sensitive "StreamCam" substring in the OBS videoConfig.name.
    // For every other name OBS supplies zero rotation even when the default
    // autorotation setting is enabled. Keep rejecting the observable StreamCam
    // case until the frame pipeline carries its per-sample orientation.
    let rotation_enabled=boolean(settings,"autorotation",true,path)?;
    let encoded_name=string(settings,"video_device_id",path)?.split_once(':')
        .ok_or_else(||issue(path,"camera_device_id_invalid","OBS camera identity has no device path"))?.0;
    if rotation_enabled && decode(encoded_name,path)?.contains("StreamCam") {
        return Err(issue(path,"camera_autorotation_unsupported","OBS StreamCam autorotation is not represented"));
    }
    let mut candidates=devices.iter().filter(|device|matches!(&device.target,VideoCaptureTarget::Windows{symbolic_link} if symbolic_link.eq_ignore_ascii_case(&device_path)));
    let device=candidates.next().ok_or_else(||issue(path,"camera_device_unresolved","OBS camera device path does not match the supplied Media Foundation catalog"))?;
    if candidates.next().is_some(){return Err(issue(path,"camera_device_ambiguous","More than one supplied camera has the same Windows device path"));}
    let custom_audio=boolean(settings,"use_custom_audio_device",false,path)?;
    // OBS UpdateAudioConfig returns without activating audio when a custom
    // device is selected but its ID is empty. This is explicit silence even
    // when the video device has an attached DirectShow audio pin.
    let audio_id=match settings.get("audio_device_id") {None=>"",Some(value)=>value.as_str().ok_or_else(||issue(path,"camera_audio_unsupported","Camera audio identity must be a string"))?};
    let explicit_silence=custom_audio && audio_id.is_empty();
    if !custom_audio && !device.video_only && !device.embedded_audio {
        return Err(issue(path,"camera_audio_unsupported","OBS camera audio must be explicitly absent; attached or custom audio cannot yet be imported"));
    }
    let embedded=!custom_audio && device.embedded_audio;
    if embedded && settings.get("audio_output_mode").map_or(false,|value|value.as_i64()!=Some(0)) {
        return Err(issue(path,"camera_audio_unsupported","Embedded camera audio currently supports OBS Capture mode only"));
    }
    let audio=if custom_audio && !explicit_silence {
        let (name,audio_path)=requested_audio(settings,path)?.ok_or_else(||issue(path,"camera_audio_invalid","OBS camera audio identity is missing"))?;
        let mut matches=device.audio_mappings.iter().filter(|entry|entry.name==name && entry.path==audio_path);
        let entry=matches.next().ok_or_else(||issue(path,"camera_audio_unresolved","No exact DirectShow to active WASAPI audio endpoint mapping was supplied"))?;
        if matches.next().is_some(){return Err(issue(path,"camera_audio_ambiguous","More than one matching DirectShow audio device was supplied"));}
        let mode=match settings.get("audio_output_mode"){None=>0,Some(value)=>value.as_i64().ok_or_else(||issue(path,"camera_audio_unsupported","Camera audio output mode must be an integer"))?};
        if mode!=0{return Err(issue(path,"camera_audio_unsupported","Only OBS Capture audio output can be imported; DirectSound and WaveOut are not represented"));}
        Some(skylive_core::AudioCaptureBinding::new(skylive_core::AudioFlow::Input,skylive_core::AudioTarget::WindowsEndpoint{id:entry.endpoint_id.clone()},true,false)
            .map_err(|error|issue(path,"camera_audio_invalid",error.to_string()))?)
    }else{None};
    let preferred=match settings.get("res_type"){None=>true,Some(v) if v.as_i64()==Some(0)=>true,Some(v) if v.as_i64()==Some(1)=>false,_=>return Err(issue(path,"camera_mode_unsupported","Unknown OBS camera resolution policy"))};
    if preferred {
        // Preferred ignores the Custom resolution/format/interval values.
        // Their values may remain in OBS settings after the user switches mode.
        neutral(settings,"buffering",json!(0),path)?;
        neutral(settings,"color_space",json!("default"),path)?;
        neutral(settings,"color_range",json!("default"),path)?;
        neutral(settings,"hw_decode",json!(false),path)?;
        neutral(settings,"active",json!(true),path)?;
        let audio_output=match settings.get("audio_output_mode"){None=>0,Some(v)=>v.as_i64().ok_or_else(||issue(path,"camera_audio_unsupported","Camera audio output mode must be an integer"))?};
        if !(0..=2).contains(&audio_output) {return Err(issue(path,"camera_audio_unsupported","Unknown OBS camera audio output mode"));}
        for key in ["last_video_device_id","last_resolution"] {if settings.get(key).is_some_and(|value|!value.is_string()){return Err(issue(path,"invalid","OBS camera history field must be a string"));}}
        let mode=device.preferred.ok_or_else(||issue(path,"camera_preferred_unresolved","DirectShow preferred probe is required"))?;
        let flip=boolean(settings,"flip_vertically",false,path)?;
        return VideoCaptureBinding::new(device.target.clone(),mode,boolean(settings,"deactivate_when_not_showing",false,path)?)
            .and_then(|binding|binding.with_mode_policy(CameraModePolicy::DirectShowPreferred).with_flip_vertically(flip).with_audio(audio))
            .and_then(|binding|binding.with_embedded_audio(embedded))
            .map_err(|error|issue(path,"camera_mode_invalid",error.to_string()));
    }
    // Resolve YUV against the reported MF mode, never manufacture color
    // metadata from the OBS fourcc. OBS resolves default YUV to 709/partial;
    // its SDR reverse shaders use centered chroma sampling. Only a mode that
    // already has the same interpretation can faithfully represent it.
    let format=settings.get("video_format").and_then(Value::as_i64)
        .ok_or_else(||issue(path,"camera_mode_unsupported","Camera import requires an explicit native video format"))?;
    let yuv=matches!(format,200..=202|301|302);
    let (matrix,range)=if yuv {
        let space=match settings.get("color_space") {None=>"default",Some(Value::String(value))=>value.as_str(),_=>return Err(issue(path,"camera_color_unsupported","Camera color space must be a string"))};
        let range=match settings.get("color_range") {None=>"default",Some(Value::String(value))=>value.as_str(),_=>return Err(issue(path,"camera_color_unsupported","Camera color range must be a string"))};
        let matrix=if space.eq_ignore_ascii_case("default")||space.eq_ignore_ascii_case("709"){VideoColorMatrix::Bt709}
            else if space.eq_ignore_ascii_case("601"){VideoColorMatrix::Bt601}
            else {return Err(issue(path,"camera_color_unsupported","Only SDR 601 and 709 camera color spaces are supported"));};
        let range=if range.eq_ignore_ascii_case("default")||range.eq_ignore_ascii_case("partial"){VideoColorRange::Limited}
            else if range.eq_ignore_ascii_case("full"){VideoColorRange::Full}
            else {return Err(issue(path,"camera_color_unsupported","Unsupported camera color range"));};
        (matrix,range)
    } else {
        neutral(settings,"color_space",json!("default"),path)?;
        neutral(settings,"color_range",json!("default"),path)?;
        (VideoColorMatrix::Bt709,VideoColorRange::Limited)
    };
    if !matches!(format,100|101|200..=202|301|302|400|401) {
        return Err(issue(path,"camera_mode_unsupported","This OBS camera format is not supported by the native camera decoder"));
    }
    let matches_format=|mode:VideoPixelFormat|match (format,mode) {
        (100,VideoPixelFormat::Bgra8)|(101,VideoPixelFormat::Rgb32)|(400,VideoPixelFormat::Mjpeg)|(401,VideoPixelFormat::H264)=>true,
        (200,VideoPixelFormat::I420{matrix:m,range:r,chroma_left:false})
        |(201,VideoPixelFormat::Nv12{matrix:m,range:r,chroma_left:false})
        |(202,VideoPixelFormat::Yv12{matrix:m,range:r,chroma_left:false})
        |(301,VideoPixelFormat::Yuy2{matrix:m,range:r,chroma_left:false})
        |(302,VideoPixelFormat::Uyvy{matrix:m,range:r,chroma_left:false})=>m==matrix&&r==range,
        _=>false,
    };
    // UpdateAudioConfig exits before reading the output mode when there is no
    // audio device. Preserve that behavior for explicitly silent imports: all
    // three OBS modes have the same audible result in this case. Still reject
    // unknown values rather than normalizing malformed or future settings.
    let audio_output = match settings.get("audio_output_mode") {
        None => 0,
        Some(value) => value.as_i64().ok_or_else(||issue(path,"camera_audio_unsupported","Camera audio output mode must be an integer"))?,
    };
    if !(0..=2).contains(&audio_output) {
        return Err(issue(path,"camera_audio_unsupported","Unknown OBS camera audio output mode"));
    }
    neutral(settings,"buffering",json!(0),path)?;
    neutral(settings,"hw_decode",json!(false),path)?;
    neutral(settings,"active",json!(true),path)?;
    for key in ["last_video_device_id","last_resolution"] {if settings.get(key).is_some_and(|value|!value.is_string()){return Err(issue(path,"invalid","OBS camera history field must be a string"));}}
    let resolution=string(settings,"resolution",path)?;
    let (w,h)=resolution.split_once('x').ok_or_else(||issue(path,"camera_resolution_invalid","OBS camera resolution must be WIDTHxHEIGHT"))?;
    let (w,h)=(w.parse::<u32>().map_err(|_|issue(path,"camera_resolution_invalid","invalid OBS camera width"))?,h.parse::<u32>().map_err(|_|issue(path,"camera_resolution_invalid","invalid OBS camera height"))?);
    let interval=match settings.get("frame_interval"){Some(value)=>value.as_i64().ok_or_else(||issue(path,"invalid","camera frame interval must be an integer"))?,None=>-1};
    let policy=match interval {-1=>CameraFpsPolicy::MatchOutput,0=>CameraFpsPolicy::Highest,1.. =>CameraFpsPolicy::Fixed,_=>return Err(issue(path,"camera_interval_invalid","unsupported OBS camera interval"))};
    let mut modes:Vec<_>=device.modes.iter().copied().filter(|mode|mode.width()==w&&mode.height()==h&&matches_format(mode.format())).collect();
    if policy==CameraFpsPolicy::Fixed {
        modes.retain(|mode|u128::from(mode.fps_den())*10_000_000/u128::from(mode.fps_num())==interval as u128);
    }
    let mode=modes.into_iter().max_by(|a,b|(u64::from(a.fps_num())*u64::from(b.fps_den())).cmp(&(u64::from(b.fps_num())*u64::from(a.fps_den()))))
        .ok_or_else(||issue(path,"camera_mode_unresolved","OBS camera mode has no exact match in the supplied native mode catalog"))?;
    let flip=boolean(settings,"flip_vertically",false,path)?;
    VideoCaptureBinding::new(device.target.clone(),mode,boolean(settings,"deactivate_when_not_showing",false,path)?)
        .and_then(|binding|binding.with_fps_policy(policy).with_flip_vertically(flip).with_audio(audio))
        .and_then(|binding|binding.with_embedded_audio(embedded))
        .map_err(|error|issue(path,"camera_mode_invalid",error.to_string()))
}


/// Encode the current authored binding, resolving device names from DirectShow
/// on the export worker. Never substitute another camera or audio endpoint.
pub(super) fn export(binding:&VideoCaptureBinding,path:&str)->Result<Value,ObsError>{
    let VideoCaptureTarget::Windows{symbolic_link}=binding.target() else {
        return Err(issue(path,"camera_platform_unsupported","OBS dshow_input requires a Windows camera"));
    };
    #[cfg(not(target_os="windows"))]
    { let _=symbolic_link; return Err(issue(path,"camera_platform_unsupported","Resolving a DirectShow camera requires Windows")); }
    #[cfg(target_os="windows")]
    {
        fn encode_identity(name:&str,device_path:&str,path:&str)->Result<String,ObsError> {
            fn encode(value:&str)->String { value.replace('#',"#22").replace(':',"#3A") }
            let id=format!("{}:{}",encode(name),encode(device_path));
            // The importer bounds encoded OBS IDs, not their unescaped components.
            // Escaping can expand a valid device name or path by up to three times.
            if id.len()>8192 {
                return Err(issue(path,"camera_device_id_invalid","Encoded OBS device identity exceeds 8192 bytes"));
            }
            Ok(id)
        }
        let (name,device_path)=super::camera_export_windows::identity(symbolic_link,path)?;
        let id=encode_identity(&name,&device_path,path)?;
        let mut settings=json!({"video_device_id":id,"flip_vertically":binding.flip_vertically(),
            "deactivate_when_not_showing":binding.deactivate_when_hidden(),"autorotation":false,
            "hw_decode":false,"buffering":0,"audio_output_mode":0});
        let object=settings.as_object_mut().expect("camera settings object");
        if binding.embedded_audio() {
            object.insert("use_custom_audio_device".into(),json!(false));
        } else {
            object.insert("use_custom_audio_device".into(),json!(true));
            let audio_id=if let Some(audio)=binding.audio() {
                if audio.flow()!=skylive_core::AudioFlow::Input || !audio.use_device_timing() || audio.exclude_own_audio() {
                    return Err(issue(path,"camera_audio_unsupported","Camera audio settings cannot be represented by OBS Capture mode"));
                }
                let skylive_core::AudioTarget::WindowsEndpoint{id}=audio.target() else {
                    return Err(issue(path,"camera_audio_unsupported","Camera audio export requires an explicitly selected Windows input endpoint"));
                };
                let mappings=super::camera_audio_windows::resolve_endpoint(id)?;
                if mappings.len()!=1 {return Err(issue(path,"camera_audio_unresolved","The audio endpoint must map to exactly one active DirectShow input"));}
                encode_identity(&mappings[0].name,&mappings[0].path,path)?
            } else {String::new()};
            object.insert("audio_device_id".into(),json!(audio_id));
        }
        if binding.mode_policy()==CameraModePolicy::DirectShowPreferred {
            object.insert("res_type".into(),json!(0));
        } else {
            let mode=binding.mode();
            let (format,color)=match mode.format() {
                VideoPixelFormat::Bgra8=>(100,None), VideoPixelFormat::Rgb32=>(101,None), VideoPixelFormat::Mjpeg=>(400,None), VideoPixelFormat::H264=>(401,None),
                VideoPixelFormat::I420{matrix,range,chroma_left:false}=>(200,Some((matrix,range))),
                VideoPixelFormat::Nv12{matrix,range,chroma_left:false}=>(201,Some((matrix,range))),
                VideoPixelFormat::Yv12{matrix,range,chroma_left:false}=>(202,Some((matrix,range))),
                VideoPixelFormat::Yuy2{matrix,range,chroma_left:false}=>(301,Some((matrix,range))),
                VideoPixelFormat::Uyvy{matrix,range,chroma_left:false}=>(302,Some((matrix,range))),
                _=>return Err(issue(path,"camera_format_unsupported","This camera format or chroma position cannot be represented by OBS dshow_input")),
            };
            let interval=match binding.fps_policy() {
                CameraFpsPolicy::Highest=>0i64,
                CameraFpsPolicy::MatchOutput=>-1,
                // DirectShow expresses frame durations in 100 ns units, the same
                // conversion used by the importer and OBS device capabilities.
                CameraFpsPolicy::Fixed=>(u64::from(mode.fps_den())*10_000_000/u64::from(mode.fps_num())) as i64,
            };
            object.insert("res_type".into(),json!(1));
            object.insert("resolution".into(),json!(format!("{}x{}",mode.width(),mode.height())));
            object.insert("video_format".into(),json!(format));
            object.insert("frame_interval".into(),json!(interval));
            if let Some((matrix,range))=color {
                object.insert("color_space".into(),json!(match matrix {VideoColorMatrix::Bt601=>"601",VideoColorMatrix::Bt709=>"709"}));
                object.insert("color_range".into(),json!(match range {VideoColorRange::Limited=>"partial",VideoColorRange::Full=>"full"}));
            }
        }
        Ok(settings)
    }
}
