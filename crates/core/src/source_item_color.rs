//! Saved Sources-list tint. This never changes source pixels or audio.
use serde::{Serialize,Deserialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum SourceItemColorPreset{Red,Yellow,Green,Cyan,Blue,Magenta,Gray,White}
impl SourceItemColorPreset{
 pub const ALL:[Self;8]=[Self::Red,Self::Yellow,Self::Green,Self::Cyan,Self::Blue,Self::Magenta,Self::Gray,Self::White];
 pub fn label(self)->&'static str{match self{Self::Red=>"Red",Self::Yellow=>"Yellow",Self::Green=>"Green",Self::Cyan=>"Cyan",Self::Blue=>"Blue",Self::Magenta=>"Magenta",Self::Gray=>"Gray",Self::White=>"White"}}
 pub fn rgba(self)->[u8;4]{let rgb=match self{Self::Red=>[255,68,68],Self::Yellow=>[255,255,68],Self::Green=>[68,255,68],Self::Cyan=>[68,255,255],Self::Blue=>[68,68,255],Self::Magenta=>[255,68,255],Self::Gray=>[68,68,68],Self::White=>[255,255,255]};[rgb[0],rgb[1],rgb[2],84]}
 pub fn obs_index(self)->u8{match self{Self::Red=>2,Self::Yellow=>3,Self::Green=>4,Self::Cyan=>5,Self::Blue=>6,Self::Magenta=>7,Self::Gray=>8,Self::White=>9}}
 pub fn from_obs_index(value:u8)->Option<Self>{Self::ALL.into_iter().find(|preset|preset.obs_index()==value)}
}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum SourceItemColorTag{#[default] Clear,Preset{preset:SourceItemColorPreset},Custom{rgba:[u8;4]}}
impl SourceItemColorTag{
 pub fn is_clear(&self)->bool{matches!(self,Self::Clear)}
 pub fn rgba(self)->Option<[u8;4]>{match self{Self::Clear=>None,Self::Preset{preset}=>Some(preset.rgba()),Self::Custom{rgba}=>Some(rgba)}}
 pub fn label(self)->&'static str{match self{Self::Clear=>"Clear",Self::Preset{preset}=>preset.label(),Self::Custom{..}=>"Custom Color"}}
 pub fn custom_initial(self)->[u8;4]{match self{Self::Custom{rgba}=>rgba,_=>[255,0,0,85]}}
}
