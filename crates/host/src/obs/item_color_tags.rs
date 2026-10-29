//! Exact bounded OBS scene-item private color keys, never stylesheet/CSS evaluation.
use super::*;use skylive_core::{SourceItemColorTag as Tag,SourceItemColorPreset as Preset};
pub(super) fn parse(item:&Map<String,Value>,path:&str)->Result<Tag,ObsError>{parse_group(item,path,false)}
pub(super) fn parse_group(item:&Map<String,Value>,path:&str,group:bool)->Result<Tag,ObsError>{
 let Some(value)=item.get("private_settings")else{return Ok(Tag::Clear);};let path=format!("{path}.private_settings");let settings=object(value,&path)?;known(settings,if group{&["color-preset","color","collapsed"][..]}else{&["color-preset","color"][..]},&path)?;
 if settings.get("collapsed").is_some_and(|value|!value.is_boolean()){return Err(issue(&path,"invalid","Group collapsed state must be a boolean"));}
 let preset=match settings.get("color-preset"){None=>0,Some(value)=>value.as_u64().filter(|value|*value<=9).ok_or_else(||issue(&format!("{path}.color-preset"),"unsupported","expected OBS color preset integer0..9"))?as u8};
 let color=match settings.get("color"){None=>"",Some(value)=>value.as_str().ok_or_else(||issue(&format!("{path}.color"),"invalid","expected color string"))?};
 if preset==1{
  let bytes=color.as_bytes();if bytes.len()!=9||bytes[0]!=b'#'||!bytes[1..].iter().all(u8::is_ascii_hexdigit){return Err(issue(&format!("{path}.color"),"unsupported","custom color requires exact #aarrggbb; empty/custom CSS colors are not represented"));}
  fn hex(value:u8)->u8{match value{b'0'..=b'9'=>value-b'0',b'a'..=b'f'=>value-b'a'+10,b'A'..=b'F'=>value-b'A'+10,_=>0}}
  let channel=|index:usize|hex(bytes[index])*16+hex(bytes[index+1]);return Ok(Tag::Custom{rgba:[channel(3),channel(5),channel(7),channel(1)]});
 }
 if !color.is_empty(){return Err(issue(&format!("{path}.color"),"unsupported","Clear and preset row colors require an empty color string"));}
 if preset==0{Ok(Tag::Clear)}else{Preset::from_obs_index(preset).map(|preset|Tag::Preset{preset}).ok_or_else(||issue(&path,"unsupported","unsupported OBS color preset"))}
}
pub(super) fn encode(item:&mut Map<String,Value>,tag:Tag){let collapsed=item.get("private_settings").and_then(Value::as_object).and_then(|settings|settings.get("collapsed")).and_then(Value::as_bool);let(preset,color)=match tag{Tag::Clear=>(0,String::new()),Tag::Preset{preset}=>(preset.obs_index(),String::new()),Tag::Custom{rgba:[r,g,b,a]}=>(1,format!("#{a:02x}{r:02x}{g:02x}{b:02x}"))};let mut settings=json!({"color-preset":preset,"color":color});if let Some(collapsed)=collapsed{settings["collapsed"]=json!(collapsed);}item.insert("private_settings".into(),settings);}
