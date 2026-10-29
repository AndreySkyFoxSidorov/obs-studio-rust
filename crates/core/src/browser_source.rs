//! Browser source meaning. CEF runtime, DOM, cookies and handles are host data.
use crate::Error;
use serde::{Serialize,Deserialize};
pub const BROWSER_PLUGIN:&str="obsproject/obs-browser";
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum BrowserLocation {Url {url:String},LocalFile {path:String}}
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct BrowserSource {location:BrowserLocation,css:String,width:u32,height:u32,fps:u32,shutdown_when_hidden:bool,#[serde(default)]control_audio:bool,refresh_when_active:bool}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct Data {location:BrowserLocation,css:String,width:u32,height:u32,fps:u32,shutdown_when_hidden:bool,#[serde(default)]control_audio:bool,refresh_when_active:bool}
impl TryFrom<Data> for BrowserSource {type Error=Error;fn try_from(v:Data)->Result<Self,Error>{Self::new(v.location,v.css,v.width,v.height,v.fps,v.shutdown_when_hidden,v.refresh_when_active).map(|value|value.with_control_audio(v.control_audio))}}
impl BrowserSource {
 pub fn new(location:BrowserLocation,css:String,width:u32,height:u32,fps:u32,shutdown_when_hidden:bool,refresh_when_active:bool)->Result<Self,Error>{let location=match location {BrowserLocation::Url {url}=>BrowserLocation::Url {url:url.into_boxed_str().into_string()},BrowserLocation::LocalFile {path}=>BrowserLocation::LocalFile {path:path.into_boxed_str().into_string()}};let v=Self {location,css:css.into_boxed_str().into_string(),width,height,fps,shutdown_when_hidden,refresh_when_active,control_audio:false};v.validate()?;Ok(v)}
 /// Relink only the existing local document. This cannot convert a URL source
 /// or silently reset CSS, viewport, frame rate, audio or activation settings.
 pub fn with_local_file(mut self,path:String)->Result<Self,Error>{
  if !matches!(self.location,BrowserLocation::LocalFile{..}){return Err(Error::Invalid("browser relink requires an existing local HTML source"));}
  self.location=BrowserLocation::LocalFile{path:path.into_boxed_str().into_string()};self.validate()?;Ok(self)
 }
 pub fn with_control_audio(mut self,enabled:bool)->Self{self.control_audio=enabled;self}
 pub fn control_audio(&self)->bool{self.control_audio}
 pub fn location(&self)->&BrowserLocation{&self.location}pub fn css(&self)->&str{&self.css}pub fn width(&self)->u32{self.width}pub fn height(&self)->u32{self.height}pub fn fps(&self)->u32{self.fps}pub fn shutdown_when_hidden(&self)->bool{self.shutdown_when_hidden}pub fn refresh_when_active(&self)->bool{self.refresh_when_active}
 pub fn validate(&self)->Result<(),Error>{
  if self.width==0||self.height==0||self.width>4096||self.height>4096||u64::from(self.width)*u64::from(self.height)>8_847_360||!(1..=60).contains(&self.fps){return Err(Error::Invalid("browser viewport requires1..4096 edges/8.8Mpixels and1..60FPS"));}
  if self.css.len()>65536||self.css.contains('\0'){return Err(Error::Invalid("browser CSS exceeds64KiB or contains NUL"));}
  match &self.location {
   BrowserLocation::Url {url}=>{let tail=url.strip_prefix("https://").or_else(||url.strip_prefix("http://")).ok_or(Error::Invalid("browser URL must use HTTP or HTTPS"))?;let authority=tail.split(['/','?','#']).next().unwrap_or("");if authority.is_empty()||authority.chars().any(char::is_whitespace)||url.len()>16384||url.chars().any(char::is_control){return Err(Error::Invalid("browser URL requires a host and at most16384 bytes without controls"));}},
   BrowserLocation::LocalFile {path}=>if path.trim().is_empty()||path.len()>4096||path.chars().any(char::is_control)||!(path.starts_with('/')||path.starts_with(r"\\")||path.as_bytes().get(1)==Some(&b':')&&path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)&&path.as_bytes().get(2).is_some_and(|b|*b==b'\\'||*b==b'/')){return Err(Error::Invalid("browser HTML path requires an absolute path of1..4096 bytes without controls"));},
  }Ok(())
 }
}
