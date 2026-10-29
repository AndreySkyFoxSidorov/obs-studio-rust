//! Independent remembered native file. No authored model/prefs mutation.
use crate::storage::{self,Phase};
use serde::{Deserialize,Serialize};
use std::{fmt,fs::{self,File},io::{self,Read},path::{Path,PathBuf}};
pub const MAX_FILE_BYTES:usize=32*1024;
pub const MAX_PATH_CHARACTERS:usize=4096;
const SCHEMA:u32=1;
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="PathData")]
pub struct LastNativeDocument{path:String}
#[derive(Serialize,Deserialize)]#[serde(deny_unknown_fields)]struct PathData{path:String}
impl TryFrom<PathData> for LastNativeDocument{type Error=Error;fn try_from(value:PathData)->Result<Self,Error>{Self::new(PathBuf::from(value.path))}}
impl LastNativeDocument{
 pub fn new(path:PathBuf)->Result<Self,Error>{let text=path.to_str().ok_or_else(||before("startup_path","remembered path must be valid UTF-8"))?;if !path.is_absolute()||path.file_name().is_none()||text.chars().count()>MAX_PATH_CHARACTERS||text.chars().any(char::is_control){return Err(before("startup_path","remembered native file requires an absolute filename of at most 4096 Unicode characters without controls"));}Ok(Self{path:text.to_owned().into_boxed_str().into_string()})}
 pub fn path(&self)->&Path{Path::new(&self.path)}
}
#[derive(Debug)]pub struct Error{pub phase:Phase,code:&'static str,detail:String}
impl Error{pub fn code(&self)->&'static str{self.code}pub fn detail(&self)->&str{&self.detail}}
impl fmt::Display for Error{fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result{write!(f,"{} ({:?}): {}",self.code,self.phase,self.detail)}}
impl std::error::Error for Error{}
fn before(code:&'static str,detail:impl ToString)->Error{Error{phase:Phase::BeforePublication,code,detail:detail.to_string()}}
#[derive(Serialize,Deserialize)]#[serde(deny_unknown_fields)]struct Envelope{schema:u32,last_native:LastNativeDocument}
/// Sibling of the actual installed settings override, not the current document.
pub fn metadata_path(preferences:&Path)->Result<PathBuf,Error>{if !preferences.is_absolute()||preferences.file_name().is_none(){return Err(before("startup_path","installed preferences path must be an absolute filename"));}Ok(preferences.with_file_name("last-native-document.json"))}
/// Missing is normal. Corrupt/future/unreadable files are errors left untouched.
/// Run only on an I/O owner; fixed staging includes one growth sentinel.
pub fn load(path:&Path)->Result<Option<LastNativeDocument>,Error>{let mut file=match File::open(path){Ok(file)=>file,Err(error)if error.kind()==io::ErrorKind::NotFound=>return Ok(None),Err(error)=>return Err(before("startup_read",error))};if file.metadata().map_err(|error|before("startup_read",error))?.len()>MAX_FILE_BYTES as u64{return Err(before("startup_limit","startup metadata exceeds 32 KiB"));}let mut bytes=Vec::new();bytes.try_reserve_exact(MAX_FILE_BYTES+1).map_err(|error|before("startup_limit",error))?;bytes.resize(MAX_FILE_BYTES+1,0);let mut count=0;while count<bytes.len(){match file.read(&mut bytes[count..]){Ok(0)=>break,Ok(n)=>count+=n,Err(error)if error.kind()==io::ErrorKind::Interrupted=>continue,Err(error)=>return Err(before("startup_read",error))}}if count>MAX_FILE_BYTES{return Err(before("startup_limit","startup metadata grew beyond 32 KiB"));}let envelope:Envelope=serde_json::from_slice(&bytes[..count]).map_err(|error|before("startup_decode",error))?;if envelope.schema!=SCHEMA{return Err(before("startup_schema",format!("unsupported startup metadata schema {}",envelope.schema)));}Ok(Some(envelope.last_native))}
/// Only call after an accepted native Open/Save publication. Never from a failed
/// startup read. Existing recoverable publication preserves phase diagnostics.
pub fn save(path:&Path,last:&LastNativeDocument)->Result<(),Error>{let bytes=serde_json::to_vec_pretty(&Envelope{schema:SCHEMA,last_native:last.clone()}).map_err(|error|before("startup_encode",error))?;if bytes.len()>MAX_FILE_BYTES{return Err(before("startup_limit","encoded startup metadata exceeds 32 KiB"));}if let Some(parent)=path.parent(){fs::create_dir_all(parent).map_err(|error|before("startup_write",error))?;}storage::publish_bytes(path,&bytes).map_err(|error|Error{phase:error.phase,code:"startup_publish",detail:error.to_string()})}
