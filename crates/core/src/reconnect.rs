//! OBS output reconnect policy; pure validated authored profile data.
use serde::{Serialize,Deserialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct ReconnectSettings{enabled:bool,delay_seconds:u32,max_attempts:u32}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct Data{enabled:bool,delay_seconds:u32,max_attempts:u32}
impl TryFrom<Data> for ReconnectSettings{type Error=crate::Error;fn try_from(v:Data)->Result<Self,Self::Error>{Self::new(v.enabled,v.delay_seconds,v.max_attempts)}}
impl Default for ReconnectSettings{fn default()->Self{Self{enabled:true,delay_seconds:2,max_attempts:25}}}
impl ReconnectSettings{
    pub fn new(enabled:bool,delay_seconds:u32,max_attempts:u32)->Result<Self,crate::Error>{let value=Self{enabled,delay_seconds,max_attempts};value.validate()?;Ok(value)}
    pub fn validate(&self)->Result<(),crate::Error>{if !(1..=30).contains(&self.delay_seconds)||!(1..=10000).contains(&self.max_attempts){return Err(crate::Error::Invalid("Reconnect delay must be1..30seconds and maximum attempts1..10000"));}Ok(())}
    pub fn enabled(self)->bool{self.enabled}pub fn delay_seconds(self)->u32{self.delay_seconds}pub fn max_attempts(self)->u32{self.max_attempts}pub fn is_default(&self)->bool{*self==Self::default()}
}
