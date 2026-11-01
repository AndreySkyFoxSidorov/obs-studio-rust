//! Exact Windows process lifetime and game HWND. Runtime injection permission is
//! session-only host state and is deliberately absent from this authored type.
use crate::Error;
use serde::{Deserialize,Serialize};
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameCaptureTarget {
    pub pid:u32,pub creation_time:u64,pub hwnd:u64,pub executable:String,pub title:String,pub class:String,
}
impl GameCaptureTarget {
    pub fn validate(&self)->Result<(),Error>{
        if self.pid<=4||self.creation_time==0||self.hwnd==0{return Err(Error::Invalid("game capture requires an exact process lifetime and window"));}
        for (text,max) in [(&self.executable,4096),(&self.title,1024),(&self.class,256)] {
            if text.len()>max||text.chars().any(char::is_control){return Err(Error::Invalid("game target metadata exceeds bounds or contains controls"));}
        }
        if self.executable.is_empty()||self.class.is_empty(){return Err(Error::Invalid("game executable and window class are required"));}
        Ok(())
    }
    pub fn same_process_window(&self,other:&Self)->bool {
        self.pid==other.pid&&self.creation_time==other.creation_time&&self.hwnd==other.hwnd&&self.executable==other.executable&&self.class==other.class
    }
}
/// Portable OBS window selector. Runtime targets are resolved separately and
/// retain exact process-lifetime and window identity checks.
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObsGameSelector {pub window:String,pub priority:u8,#[serde(default,skip_serializing_if="is_false")] pub any_fullscreen:bool,#[serde(default,skip_serializing_if="is_false")] pub hotkey_mode:bool}
impl ObsGameSelector {
    fn validate(&self)->Result<(),Error>{
        if (!self.any_fullscreen&&!self.hotkey_mode&&self.window.is_empty())||self.any_fullscreen&&self.hotkey_mode||self.window.len()>4096||self.window.chars().any(char::is_control)||self.priority>2 {
            return Err(Error::Invalid("invalid OBS game window selector"));
        }
        Ok(())
    }
}
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct GameCaptureBinding {#[serde(skip_serializing_if="Option::is_none")] target:Option<GameCaptureTarget>,#[serde(default,skip_serializing_if="Option::is_none")] obs_selector:Option<ObsGameSelector>,width:u32,height:u32,max_fps:u32,#[serde(default,skip_serializing_if="is_false")] capture_cursor:bool,#[serde(default,skip_serializing_if="is_false")] allow_transparency:bool}
fn is_false(value:&bool)->bool{!*value}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {#[serde(default)] target:Option<GameCaptureTarget>,#[serde(default)] obs_selector:Option<ObsGameSelector>,width:u32,height:u32,max_fps:u32,#[serde(default)] capture_cursor:bool,#[serde(default)] allow_transparency:bool}
impl TryFrom<Data> for GameCaptureBinding {type Error=Error;fn try_from(v:Data)->Result<Self,Error>{let value=Self {target:v.target,obs_selector:v.obs_selector,width:v.width,height:v.height,max_fps:v.max_fps,capture_cursor:v.capture_cursor,allow_transparency:v.allow_transparency};value.validate()?;Ok(value)}}
impl GameCaptureBinding {
    /// Initial window/client size is a transform basis, not a claimed captured
    /// backbuffer format. Captured frames carry their actual dynamic size.
    pub fn new(target:GameCaptureTarget,width:u32,height:u32,max_fps:u32)->Result<Self,Error>{
        let value=Self {target:Some(target),obs_selector:None,width,height,max_fps,capture_cursor:false,allow_transparency:false};value.validate()?;Ok(value)
    }
    pub fn unresolved_obs(selector:ObsGameSelector,width:u32,height:u32,max_fps:u32)->Result<Self,Error>{
        let value=Self {target:None,obs_selector:Some(selector),width,height,max_fps,capture_cursor:false,allow_transparency:false};value.validate()?;Ok(value)
    }
    pub fn validate(&self)->Result<(),Error>{
        if let Some(target)=&self.target {target.validate()?;} else if self.obs_selector.is_none(){return Err(Error::Invalid("game capture needs a target or OBS selector"));}
        if let Some(selector)=&self.obs_selector {selector.validate()?;}
        if self.width==0||self.height==0||self.width>8192||self.height>8192||u64::from(self.width)*u64::from(self.height)>32*1024*1024||!(1..=120).contains(&self.max_fps){return Err(Error::Invalid("game transform basis supports8192-edge/32M pixels and1..120 capture fps; actual D3D11 transport is4096-edge/8.8M pixels"));}Ok(())}
    pub fn target(&self)->Option<&GameCaptureTarget>{self.target.as_ref()}
    pub fn obs_selector(&self)->Option<&ObsGameSelector>{self.obs_selector.as_ref()}
    /// Host-owned transient resolution; callers must never persist this copy.
    pub fn with_runtime_target(mut self,target:GameCaptureTarget)->Result<Self,Error>{
        if self.obs_selector.is_none(){return Err(Error::Invalid("game capture has no OBS selector"));}
        target.validate()?;self.target=Some(target);Ok(self)
    }
    pub fn width(&self)->u32{self.width}pub fn height(&self)->u32{self.height}pub fn max_fps(&self)->u32{self.max_fps}
    pub fn with_max_fps(mut self,value:u32)->Result<Self,Error>{self.max_fps=value;self.validate()?;Ok(self)}
    pub fn capture_cursor(&self)->bool{self.capture_cursor}
    pub fn with_capture_cursor(mut self,value:bool)->Self{self.capture_cursor=value;self}
    pub fn allow_transparency(&self)->bool{self.allow_transparency}
    pub fn with_allow_transparency(mut self,value:bool)->Self{self.allow_transparency=value;self}
}
