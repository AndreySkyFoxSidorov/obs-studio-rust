//! Authored pointer alignment policy. Geometry, display scales and gestures are host state.
use crate::Error;
use serde::{Deserialize,Serialize};

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct PreviewSnapping {
    enabled:bool,
    screen_edges:bool,
    source_edges:bool,
    canvas_center:bool,
    distance_tenths:u16,
}
#[derive(Deserialize)]
#[serde(default,deny_unknown_fields)]
struct Data {enabled:bool,screen_edges:bool,source_edges:bool,canvas_center:bool,distance_tenths:u16}
impl Default for Data {
    fn default()->Self {Self{enabled:true,screen_edges:true,source_edges:true,canvas_center:false,distance_tenths:100}}
}
impl TryFrom<Data> for PreviewSnapping {
    type Error=Error;
    fn try_from(d:Data)->Result<Self,Error>{Self::new(d.enabled,d.screen_edges,d.source_edges,d.canvas_center,d.distance_tenths)}
}
impl Default for PreviewSnapping {
    fn default()->Self{Self{enabled:true,screen_edges:true,source_edges:true,canvas_center:false,distance_tenths:100}}
}
impl PreviewSnapping {
    pub fn new(enabled:bool,screen_edges:bool,source_edges:bool,canvas_center:bool,distance_tenths:u16)->Result<Self,Error>{
        let value=Self{enabled,screen_edges,source_edges,canvas_center,distance_tenths};value.validate()?;Ok(value)
    }
    pub fn enabled(self)->bool{self.enabled}
    pub fn screen_edges(self)->bool{self.screen_edges}
    pub fn source_edges(self)->bool{self.source_edges}
    pub fn canvas_center(self)->bool{self.canvas_center}
    pub fn distance_tenths(self)->u16{self.distance_tenths}
    pub fn distance(self)->f64{f64::from(self.distance_tenths)/10.0}
    pub fn is_default(&self)->bool{*self==Self::default()}
    pub fn validate(self)->Result<(),Error>{
        if self.distance_tenths>1000{return Err(Error::Invalid("Preview snapping distance requires 0..100 display pixels"));}Ok(())
    }
}
