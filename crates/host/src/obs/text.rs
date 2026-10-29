//! OBS 32 Windows GDI Text v3 fixed-box subset; no file I/O or GDI calls.
use super::*;
use skylive_core::{TextAlignment,TextVerticalAlignment,TextCaseTransform,TextFile,TextSource,TextStyle};

fn optional_string<'a>(map:&'a Map<String,Value>,key:&str,default:&'a str,path:&str)->Result<&'a str,ObsError>{
    match map.get(key){None=>Ok(default),Some(value)=>value.as_str().ok_or_else(||issue(&format!("{path}.{key}"),"invalid","expected a string"))}
}
fn integer(map:&Map<String,Value>,key:&str,default:u64,min:u64,max:u64,path:&str)->Result<u64,ObsError>{
    let value=match map.get(key){None=>default,Some(value)=>value.as_u64().ok_or_else(||issue(&format!("{path}.{key}"),"invalid","expected an unsigned integer"))?};
    if !(min..=max).contains(&value){return Err(issue(&format!("{path}.{key}"),"unsupported","value is outside the supported text subset"));}Ok(value)
}
fn color(map:&Map<String,Value>,key:&str,opacity:&str,default:u64,alpha:u64,path:&str)->Result<[u8;4],ObsError>{
    let color=integer(map,key,default,0,u32::MAX as u64,path)? as u32;
    let opacity=integer(map,opacity,alpha,0,100,path)? as u32;
    Ok([color as u8,(color>>8) as u8,(color>>16) as u8,(opacity*255/100) as u8])
}
fn opacity(alpha:u8,path:&str)->Result<u8,ObsError>{
    (0u8..=100).find(|value|u32::from(*value)*255/100==u32::from(alpha))
        .ok_or_else(||issue(path,"text_opacity_unsupported","Text alpha has no exact OBS integer-percent opacity"))
}
fn packed(value:[u8;4])->u32{u32::from(value[0])|(u32::from(value[1])<<8)|(u32::from(value[2])<<16)}

pub(super) fn parse(map:&Map<String,Value>,path:&str,base:Option<&Path>)->Result<(TextSource,String),ObsError>{
    known(map,&["font","read_from_file","file","text","color","gradient","gradient_color","gradient_dir","gradient_opacity","align","valign","opacity","bk_color","bk_opacity","vertical","outline","outline_size","outline_color","outline_opacity","chatlog","chatlog_lines","extents","extents_wrap","extents_cx","extents_cy","transform","antialiasing"],path)?;
    for field in ["vertical"] {neutral(map,field,json!(false),path)?;}
    let gradient_color=integer(map,"gradient_color",0xffffff,0,u32::MAX as u64,path)? as u32;
    let direction=number(map,"gradient_dir",90.0,path)?;
    if !direction.is_finite()||!(0.0..=360.0).contains(&direction){return Err(issue(path,"text_gradient_direction","Gradient direction must be finite and between 0 and 360 degrees"));}
    let gradient=skylive_core::TextGradient{enabled:boolean(map,"gradient",false,path)?,color:[gradient_color as u8,(gradient_color>>8) as u8,(gradient_color>>16) as u8],opacity:integer(map,"gradient_opacity",100,0,100,path)? as u8,direction:direction as f32};
    let outline_color=integer(map,"outline_color",0xffffff,0,u32::MAX as u64,path)? as u32;
    let outline=skylive_core::TextOutline{enabled:boolean(map,"outline",false,path)?,size:integer(map,"outline_size",2,1,20,path)? as u8,color:[outline_color as u8,(outline_color>>8) as u8,(outline_color>>16) as u8],opacity:integer(map,"outline_opacity",100,0,100,path)? as u8};
    let antialiasing=boolean(map,"antialiasing",true,path)?;
    let fixed_extents=boolean(map,"extents",false,path)?;
    let width=if fixed_extents{integer(map,"extents_cx",100,2,8192,path)? as u32}else{100};
    let height=if fixed_extents{integer(map,"extents_cy",100,2,8192,path)? as u32}else{100};
    // OBS lays out against the original extents, then rounds the backing
    // texture up to an even size. Preserve both, including across export.
    let texture_width=width+width%2;
    let texture_height=height+height%2;
    let font_path=format!("{path}.font");
    // OBS supplies an Arial/256 default font object when no user font was
    // saved. Preserve that behaviour for collections containing only user
    // values, while still rejecting an explicitly malformed font object.
    let default_font=Map::new();
    let font=match map.get("font"){Some(value)=>object(value,&font_path)?,None=>&default_font};
    known(font,&["face","size","flags"],&font_path)?;
    let family=optional_string(font,"face","Arial",&font_path)?;
    if family.trim().is_empty()||family.len()>256||family.chars().any(char::is_control){return Err(issue(&font_path,"text_font_invalid","Font family requires 1..256 UTF-8 bytes without control characters"));}
    let size=integer(font,"size",256,1,1000,&font_path)? as u32;
    let flags=integer(font,"flags",0,0,15,&font_path)?;
    let alignment=match optional_string(map,"align","left",path)?{"left"=>TextAlignment::Left,"center"=>TextAlignment::Center,"right"=>TextAlignment::Right,_=>return Err(issue(path,"text_alignment_unsupported","Unknown horizontal text alignment"))};
    let vertical_alignment=match optional_string(map,"valign","top",path)?{"top"=>TextVerticalAlignment::Top,"center"=>TextVerticalAlignment::Center,"bottom"=>TextVerticalAlignment::Bottom,_=>return Err(issue(path,"text_alignment_unsupported","Unknown vertical text alignment"))};
    let case=match integer(map,"transform",0,0,3,path)?{0=>TextCaseTransform::None,1=>TextCaseTransform::Uppercase,2=>TextCaseTransform::Lowercase,_=>TextCaseTransform::Startcase};
    let read=boolean(map,"read_from_file",false,path)?;
    let chat=boolean(map,"chatlog",false,path)?;
    let lines=integer(map,"chatlog_lines",6,1,u16::MAX as u64,path)? as u16;
    let raw_file=optional_string(map,"file","",path)?;
    if read&&raw_file.is_empty()||raw_file.contains("://"){return Err(issue(path,"text_file_path_invalid","Text file requires a local path"));}
    let file=if read||!raw_file.is_empty()||chat||lines!=6{
        let input=if raw_file.is_empty(){String::new()}else{
            let input=PathBuf::from(raw_file);
            let input=if input.is_absolute(){input}else{base.ok_or_else(||issue(path,"text_file_base_required","Relative text file requires an explicit absolute collection directory"))?.join(input)};
            input.to_str().ok_or_else(||issue(path,"text_file_path_invalid","Text file path must be UTF-8"))?.to_owned()
        };
        Some(TextFile::settings(input,chat,lines).map_err(|error|issue(path,"text_file_path_invalid",error.to_string()))?)
    }else{None};
    let style=TextStyle{foreground:color(map,"color","opacity",0xffffff,100,path)?,background:color(map,"bk_color","bk_opacity",0,0,path)?,bold:flags&1!=0,italic:flags&2!=0,wrap:boolean(map,"extents_wrap",true,path)?,alignment,vertical_alignment};
    TextSource::new(optional_string(map,"text","",path)?.to_owned(),texture_width,texture_height,size,style)
        .and_then(|v|if width%2!=0||height%2!=0{v.with_obs_layout(width,height)}else{Ok(v)})
        .and_then(|v|v.with_auto_extents(!fixed_extents))
        .and_then(|v|v.with_case_transform(case)).and_then(|v|v.with_file_settings(file,read)).and_then(|v|v.with_outline(outline)).and_then(|v|v.with_gradient(gradient)).map(|v|v.with_antialiasing(antialiasing).with_decoration(flags&4!=0,flags&8!=0))
        .map(|settings|(settings,family.to_owned())).map_err(|error|issue(path,"text_settings_unsupported",error.to_string()))
}

pub(super) fn export(value:&TextSource,path:&str)->Result<Value,ObsError>{
    if value.font().is_some()&&!value.system_font(){return Err(issue(path,"text_font_unsupported","Custom file fonts cannot be represented as an OBS system-font selection"));}
    let (family,flags)=if let Some(font)=value.font(){
        let face=font.face();
        if face.stretch()!=5||!matches!(face.weight(),400|700)||!matches!(face.style(),skylive_core::FontFaceStyle::Normal|skylive_core::FontFaceStyle::Italic){return Err(issue(path,"text_font_unsupported","The selected face has a weight, stretch or style that OBS GDI font flags cannot represent exactly"));}
        (face.family(),u8::from(face.weight()==700)|(u8::from(face.style()==skylive_core::FontFaceStyle::Italic)<<1))
    }else{("Noto Sans",u8::from(value.bold())|(u8::from(value.italic())<<1))};
    if value.width()<2||value.height()<2||value.width()%2!=0||value.height()%2!=0{return Err(issue(path,"text_odd_extents_unsupported","OBS text export requires even dimensions of at least two pixels"));}
    let flags=flags | (if value.underline(){4}else{0}) | (if value.strikeout(){8}else{0});
    let foreground=value.foreground();let background=value.background();let outline=value.outline();let gradient=value.gradient();
    let file=value.file_settings();
    if file.is_some_and(|f|!f.path().is_empty()&&(!Path::new(f.path()).is_absolute()||f.path().contains("://"))){return Err(issue(path,"text_file_path_invalid","OBS text export requires an absolute local path on this host"));}
    let align=match value.alignment(){TextAlignment::Left=>"left",TextAlignment::Center=>"center",TextAlignment::Right=>"right"};
    let valign=match value.vertical_alignment(){TextVerticalAlignment::Top=>"top",TextVerticalAlignment::Center=>"center",TextVerticalAlignment::Bottom=>"bottom"};
    let transform=match value.case_transform(){TextCaseTransform::None=>0,TextCaseTransform::Uppercase=>1,TextCaseTransform::Lowercase=>2,TextCaseTransform::Startcase=>3};
    Ok(json!({"font":{"face":family,"size":value.font_size(),"flags":flags},"read_from_file":value.file().is_some(),"file":file.map_or("",TextFile::path),"text":value.text(),"chatlog":file.and_then(TextFile::last_lines).is_some(),"chatlog_lines":file.map_or(6,TextFile::saved_lines),"extents":!value.auto_extents(),"extents_cx":value.layout_width(),"extents_cy":value.layout_height(),"extents_wrap":value.wrap(),"color":packed(foreground),"opacity":opacity(foreground[3],path)?,"bk_color":packed(background),"bk_opacity":opacity(background[3],path)?,"align":align,"valign":valign,"transform":transform,"antialiasing":value.antialiasing(),"gradient":gradient.enabled,"gradient_color":u32::from(gradient.color[0])|(u32::from(gradient.color[1])<<8)|(u32::from(gradient.color[2])<<16),"gradient_opacity":gradient.opacity,"gradient_dir":gradient.direction,"outline":outline.enabled,"outline_size":outline.size,"outline_color":u32::from(outline.color[0])|(u32::from(outline.color[1])<<8)|(u32::from(outline.color[2])<<16),"outline_opacity":outline.opacity}))
}
