//! Strict mapping for OBS Windows WASAPI device sources.
use super::*;
use skylive_core::{AudioCaptureBinding, AudioFlow, AudioTarget};

pub(super) fn parse(settings:&Map<String,Value>,path:&str,flow:AudioFlow)->Result<AudioCaptureBinding,ObsError>{
    if !cfg!(target_os="windows"){return Err(issue(path,"wasapi_platform_unsupported","Windows WASAPI sources require the Windows capture backend"));}
    known(settings,&["device_id","use_device_timing"],path)?;
    let device=settings.get("device_id").map(|v|v.as_str().ok_or_else(||issue(path,"invalid","device_id must be a string"))).transpose()?.unwrap_or("default");
    let target=if device.eq_ignore_ascii_case("default"){AudioTarget::Default}else{AudioTarget::WindowsEndpoint{id:device.to_owned()}};
    let timing=boolean(settings,"use_device_timing",flow==AudioFlow::Output,path)?;
    AudioCaptureBinding::new(flow,target,timing,false).map_err(|error|issue(path,"unsupported_wasapi",error.to_string()))
}
pub(super) fn export(binding:&AudioCaptureBinding,path:&str)->Result<(&'static str,Value),ObsError>{
    if !cfg!(target_os="windows"){return Err(issue(path,"wasapi_platform_unsupported","Windows WASAPI sources require the Windows capture backend"));}
    if binding.exclude_own_audio(){return Err(issue(path,"unsupported_wasapi","OBS device sources cannot represent own-audio exclusion"));}
    if let AudioTarget::WindowsWindow {selector}=binding.target(){
        if binding.flow()!=AudioFlow::Output{return Err(issue(path,"unsupported_wasapi","Application audio requires output flow"));}
        let mut settings=application_audio::export(selector);settings.as_object_mut().expect("object").insert("use_device_timing".into(),json!(binding.use_device_timing()));
        return Ok(("wasapi_process_output_capture",settings));
    }
    let device=match binding.target(){AudioTarget::Default=>"default",AudioTarget::WindowsEndpoint{id} if !id.eq_ignore_ascii_case("default")=>id.as_str(),_=>return Err(issue(path,"unsupported_wasapi","Only Windows default or exact endpoint device bindings can be represented"))};
    let id=match binding.flow(){AudioFlow::Input=>"wasapi_input_capture",AudioFlow::Output=>"wasapi_output_capture"};
    Ok((id,json!({"device_id":device,"use_device_timing":binding.use_device_timing()})))
}
