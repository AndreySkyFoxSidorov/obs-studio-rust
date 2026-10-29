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
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct GameCaptureBinding {target:GameCaptureTarget,width:u32,height:u32,max_fps:u32}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {target:GameCaptureTarget,width:u32,height:u32,max_fps:u32}
impl TryFrom<Data> for GameCaptureBinding {type Error=Error;fn try_from(v:Data)->Result<Self,Error>{Self::new(v.target,v.width,v.height,v.max_fps)}}
impl GameCaptureBinding {
    /// Initial window/client size is a transform basis, not a claimed captured
    /// backbuffer format. Captured frames carry their actual dynamic size.
    pub fn new(target:GameCaptureTarget,width:u32,height:u32,max_fps:u32)->Result<Self,Error>{
        let value=Self {target,width,height,max_fps};value.validate()?;Ok(value)
    }
    pub fn validate(&self)->Result<(),Error>{self.target.validate()?;
        if self.width==0||self.height==0||self.width>8192||self.height>8192||u64::from(self.width)*u64::from(self.height)>32*1024*1024||!(1..=120).contains(&self.max_fps){return Err(Error::Invalid("game transform basis supports8192-edge/32M pixels and1..120 capture fps; actual D3D11 transport is4096-edge/8.8M pixels"));}Ok(())}
    pub fn target(&self)->&GameCaptureTarget{&self.target}
    pub fn width(&self)->u32{self.width}pub fn height(&self)->u32{self.height}pub fn max_fps(&self)->u32{self.max_fps}
}
