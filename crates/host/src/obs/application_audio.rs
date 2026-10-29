//! OBS 32.2.2 window selector wire codec, used by the application-audio
//! interchange path. The runtime's supported window matching limits are
//! documented in the Application Audio Capture module card.
use super::*;
use skylive_core::{WindowsAudioWindowPriority as Priority, WindowsAudioWindowSelector};

pub(super) fn parse(settings:&Map<String,Value>,path:&str)->Result<WindowsAudioWindowSelector,ObsError>{
    known(settings,&["window","priority","use_device_timing"],path)?;
    let value=settings.get("window").map(|v|v.as_str().ok_or_else(||issue(path,"invalid","window must be a string"))).transpose()?.unwrap_or("");
    let priority=match settings.get("priority"){None=>Priority::Class,Some(value)=>match value.as_i64(){Some(0)=>Priority::Class,Some(1)=>Priority::Title,Some(2)=>Priority::Executable,_=>return Err(issue(path,"unsupported","window priority must be 0, 1 or 2"))}};
    // OBS has no valid parsed tuple when selection is blank. Preserve an empty
    // selector as unavailable rather than binding an arbitrary running process.
    let (title,class,exe)=if value.is_empty(){("","","")}else{
        let mut parts=value.split(':');let t=parts.next().unwrap_or("");let c=parts.next().ok_or_else(||issue(path,"invalid","window tuple lacks class"))?;let e=parts.next().ok_or_else(||issue(path,"invalid","window tuple lacks executable"))?;
        if parts.next().is_some(){return Err(issue(path,"invalid","window tuple has extra components"));}(t,c,e)
    };
    let decode=|part:&str|part.replace("#3A",":").replace("#22","#");
    WindowsAudioWindowSelector::new(decode(title),decode(class),decode(exe),priority).map_err(|error|issue(path,"invalid",error.to_string()))
}
pub(super) fn export(selector:&WindowsAudioWindowSelector)->Value{
    let encode=|part:&str|part.replace('#',"#22").replace(':',"#3A");
    let window=if selector.title().is_empty()&&selector.class().is_empty()&&selector.executable().is_empty(){String::new()}else{format!("{}:{}:{}",encode(selector.title()),encode(selector.class()),encode(selector.executable()))};
    let priority=match selector.priority(){Priority::Title=>1,Priority::Class=>0,Priority::Executable=>2};
    json!({"window":window,"priority":priority})
}
