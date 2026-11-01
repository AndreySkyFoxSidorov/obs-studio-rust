//! Browser source meaning. CEF runtime, DOM, cookies and handles are host data.
use crate::Error;
use serde::{Serialize,Deserialize};
pub const BROWSER_PLUGIN:&str="obsproject/obs-browser";
fn custom_fps_default()->bool{true}
fn is_custom_fps(value:&bool)->bool{*value}
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum BrowserLocation {Url {url:String},LocalFile {path:String}}
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct BrowserSource {location:BrowserLocation,#[serde(default,skip_serializing_if="String::is_empty")]inactive_location:String,css:String,width:u32,height:u32,fps:u32,#[serde(default="custom_fps_default",skip_serializing_if="is_custom_fps")]custom_fps:bool,shutdown_when_hidden:bool,#[serde(default)]control_audio:bool,refresh_when_active:bool}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct Data {location:BrowserLocation,#[serde(default)]inactive_location:String,css:String,width:u32,height:u32,fps:u32,#[serde(default="custom_fps_default")]custom_fps:bool,shutdown_when_hidden:bool,#[serde(default)]control_audio:bool,refresh_when_active:bool}
impl TryFrom<Data> for BrowserSource {type Error=Error;fn try_from(v:Data)->Result<Self,Error>{Self::new(v.location,v.css,v.width,v.height,v.fps,v.shutdown_when_hidden,v.refresh_when_active)?.with_inactive_location(v.inactive_location).map(|value|value.with_control_audio(v.control_audio).with_custom_fps(v.custom_fps))}}
impl BrowserSource {
 pub fn new(location:BrowserLocation,css:String,width:u32,height:u32,fps:u32,shutdown_when_hidden:bool,refresh_when_active:bool)->Result<Self,Error>{let location=match location {BrowserLocation::Url {url}=>BrowserLocation::Url {url:url.into_boxed_str().into_string()},BrowserLocation::LocalFile {path}=>BrowserLocation::LocalFile {path:path.into_boxed_str().into_string()}};let v=Self {location,inactive_location:String::new(),css:css.into_boxed_str().into_string(),width,height,fps,custom_fps:true,shutdown_when_hidden,refresh_when_active,control_audio:false};v.validate()?;Ok(v)}
 /// The inactive URL or path is authored data only; it is never loaded until selected.
 pub fn with_inactive_location(mut self,value:String)->Result<Self,Error>{self.inactive_location=value;self.validate()?;Ok(self)}
 pub fn inactive_location(&self)->&str{&self.inactive_location}
 /// Relink only the existing local document. This cannot convert a URL source
 /// or silently reset CSS, viewport, frame rate, audio or activation settings.
 pub fn with_local_file(mut self,path:String)->Result<Self,Error>{
  if !matches!(self.location,BrowserLocation::LocalFile{..}){return Err(Error::Invalid("browser relink requires an existing local HTML source"));}
  self.location=BrowserLocation::LocalFile{path:path.into_boxed_str().into_string()};self.validate()?;Ok(self)
 }
 pub fn with_control_audio(mut self,enabled:bool)->Self{self.control_audio=enabled;self}
 pub fn with_custom_fps(mut self,enabled:bool)->Self{self.custom_fps=enabled;self}
 pub fn custom_fps(&self)->bool{self.custom_fps}
 /// CEF's offscreen frame rate is an integer capped at 60 FPS.
 /// Project rate remains rational for publication. CEF's separate integer
 /// paint limit is rounded upward so a 29.97 FPS canvas is not capped at 29.
 pub fn effective_rate(&self,num:u32,den:u32)->(u32,u32){
  if self.custom_fps||num==0||den==0{return(self.fps,1);}
  if u64::from(num)<u64::from(den){return(1,1);}
  if u64::from(num)>60*u64::from(den){return(60,1);}
  (num,den)
 }
 pub fn effective_fps(&self,num:u32,den:u32)->u32{let(n,d)=self.effective_rate(num,den);((u64::from(n)+u64::from(d)-1)/u64::from(d)).clamp(1,60)as u32}
 pub fn control_audio(&self)->bool{self.control_audio}
 pub fn location(&self)->&BrowserLocation{&self.location}pub fn css(&self)->&str{&self.css}pub fn width(&self)->u32{self.width}pub fn height(&self)->u32{self.height}pub fn fps(&self)->u32{self.fps}pub fn shutdown_when_hidden(&self)->bool{self.shutdown_when_hidden}pub fn refresh_when_active(&self)->bool{self.refresh_when_active}
 pub fn validate(&self)->Result<(),Error>{
  if self.width==0||self.height==0||self.width>4096||self.height>4096||u64::from(self.width)*u64::from(self.height)>8_847_360||!(1..=60).contains(&self.fps){return Err(Error::Invalid("browser viewport requires1..4096 edges/8.8Mpixels and1..60FPS"));}
  if self.css.len()>65536||self.css.contains('\0'){return Err(Error::Invalid("browser CSS exceeds64KiB or contains NUL"));}
  let inactive_limit=if matches!(self.location,BrowserLocation::Url{..}){4096}else{16384};
  if self.inactive_location.len()>inactive_limit||self.inactive_location.chars().any(char::is_control){return Err(Error::Invalid("inactive browser URL or path exceeds its limit or contains controls"));}
  match &self.location {
   BrowserLocation::Url {url}=>{
    if url.len()>16384||url.chars().any(char::is_control){return Err(Error::Invalid("browser URL exceeds 16384 bytes or contains controls"));}
    if !(url.get(..6).is_some_and(|prefix|prefix.eq_ignore_ascii_case("about:")) && url.get(6..)==Some("blank")) {
     if let Some(data)=url.get(..5).filter(|prefix|prefix.eq_ignore_ascii_case("data:")).and_then(|_|url.get(5..)) {
      // Inline HTML, SVG, text and image overlays do not require a file.
      // CEF receives the exact URL; do not decode or normalize the payload here.
      let (metadata,_)=data.split_once(',').ok_or(Error::Invalid("browser data URL requires a comma"))?;
      if metadata.chars().any(char::is_whitespace){return Err(Error::Invalid("browser data URL metadata must not contain whitespace"));}
     }else{
      let tail=(if url.get(..8).is_some_and(|prefix|prefix.eq_ignore_ascii_case("https://")){url.get(8..)}else if url.get(..7).is_some_and(|prefix|prefix.eq_ignore_ascii_case("http://")){url.get(7..)}else{None}).ok_or(Error::Invalid("browser URL must use HTTP, HTTPS, about:blank or data"))?;
      let authority=tail.split(['/','?','#']).next().unwrap_or("");
      if authority.is_empty()||authority.chars().any(char::is_whitespace){return Err(Error::Invalid("browser URL requires a valid host"));}
     }
    }
   },
   BrowserLocation::LocalFile {path}=>if path.trim().is_empty()||path.len()>4096||path.chars().any(char::is_control)||!(path.starts_with('/')||path.starts_with(r"\\")||path.as_bytes().get(1)==Some(&b':')&&path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)&&path.as_bytes().get(2).is_some_and(|b|*b==b'\\'||*b==b'/')){return Err(Error::Invalid("browser HTML path requires an absolute path of1..4096 bytes without controls"));},
  }Ok(())
 }
}
