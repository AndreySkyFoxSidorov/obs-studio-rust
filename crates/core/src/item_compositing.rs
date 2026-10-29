//! Authored per-scene-item settings, independent of shared source/filter owners.
use serde::{Deserialize,Serialize};
use crate::{Error,Id,StudioSnapshot};

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum BlendMode { #[default] Normal, #[serde(alias="add")] Additive, Subtract, Screen, Multiply, Lighten, Darken }
impl BlendMode {
    pub const ALL:[Self;7]=[Self::Normal,Self::Additive,Self::Subtract,Self::Screen,Self::Multiply,Self::Lighten,Self::Darken];
    pub fn label(self)->&'static str {match self {Self::Normal=>"Normal",Self::Additive=>"Additive",Self::Subtract=>"Subtract",Self::Screen=>"Screen",Self::Multiply=>"Multiply",Self::Lighten=>"Lighten",Self::Darken=>"Darken"}}
    pub fn obs_name(self)->&'static str {match self {Self::Normal=>"normal",Self::Additive=>"additive",Self::Subtract=>"subtract",Self::Screen=>"screen",Self::Multiply=>"multiply",Self::Lighten=>"lighten",Self::Darken=>"darken"}}
}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum BlendingMethod { #[default] Default, SrgbOff }
impl BlendingMethod {
    pub fn label(self)->&'static str {match self {Self::Default=>"Default",Self::SrgbOff=>"SRGB Off"}}
    pub fn obs_name(self)->&'static str {match self {Self::Default=>"default",Self::SrgbOff=>"srgb_off"}}
}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ScaleFiltering { #[default] #[serde(rename="disable",alias="disabled")] Disabled, Point, Bilinear, Bicubic, Lanczos, Area }
impl ScaleFiltering {
    pub fn label(self)->&'static str {match self {Self::Disabled=>"Disable",Self::Point=>"Point",Self::Bilinear=>"Bilinear",Self::Bicubic=>"Bicubic",Self::Lanczos=>"Lanczos",Self::Area=>"Area"}}
    pub fn obs_name(self)->&'static str {match self {Self::Disabled=>"disable",Self::Point=>"point",Self::Bilinear=>"bilinear",Self::Bicubic=>"bicubic",Self::Lanczos=>"lanczos",Self::Area=>"area"}}
    /// Conservative scene-item upper bound; global output uses its dimension plan.
    pub fn sample_cost(self)->usize {match self {Self::Disabled|Self::Point=>1,Self::Bilinear=>4,Self::Bicubic=>32,Self::Lanczos=>36,Self::Area=>1024}}
}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(default,deny_unknown_fields)]
pub struct ItemCompositing {
    pub blend_mode:BlendMode,
    pub blending_method:BlendingMethod,
    pub scale_filter:ScaleFiltering,
}
impl ItemCompositing {
    pub fn is_default(&self)->bool {*self==Self::default()}
}
impl StudioSnapshot {
    pub(super) fn set_item_compositing(&mut self,scene:Id,item:Id,settings:ItemCompositing)->Result<(),Error> {
        let original=self.scene_container(scene).ok_or(Error::NotFound(scene))?.items().iter()
            .find(|entry|entry.id()==item).ok_or(Error::NotFound(item))?;
        if original.compositing()==settings {return Ok(());}
        let source=self.source(original.source()).ok_or(Error::NotFound(original.source()))?;
        if self.source_video_dimensions(source.kind())?.is_none() {
            return Err(Error::Invalid("audio-only items do not support video compositing"));
        }
        // OBS item locks protect geometry. Like source properties/filters, these
        // appearance settings are editable without moving a locked scene item.
        self.item_mut(scene,item)?.compositing=settings;
        Ok(())
    }
}
