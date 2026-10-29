//! Saved projector presentation defaults; never opens a native window.
use serde::{Deserialize,Serialize};
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum MultiviewLayout {#[default] Top8,Bottom8,Left8,Right8,Top18,Top24,Scenes4,Scenes9,Scenes16,Scenes25}
impl MultiviewLayout {
 pub const ALL:[Self;10]=[Self::Top8,Self::Bottom8,Self::Left8,Self::Right8,Self::Top18,Self::Top24,Self::Scenes4,Self::Scenes9,Self::Scenes16,Self::Scenes25];
 pub fn label(self)->&'static str{match self{Self::Top8=>"Horizontal, Top (8 scenes)",Self::Bottom8=>"Horizontal, Bottom (8 scenes)",Self::Left8=>"Vertical, Left (8 scenes)",Self::Right8=>"Vertical, Right (8 scenes)",Self::Top18=>"Horizontal, Top (18 scenes)",Self::Top24=>"Horizontal, Top (24 scenes)",Self::Scenes4=>"Scenes Only (4 scenes)",Self::Scenes9=>"Scenes Only (9 scenes)",Self::Scenes16=>"Scenes Only (16 scenes)",Self::Scenes25=>"Scenes Only (25 scenes)"}}
 pub fn count(self)->usize{match self{Self::Top18=>18,Self::Top24=>24,Self::Scenes4=>4,Self::Scenes9=>9,Self::Scenes16=>16,Self::Scenes25=>25,_=>8}}
 pub fn scenes_only(self)->bool{matches!(self,Self::Scenes4|Self::Scenes9|Self::Scenes16|Self::Scenes25)}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(default,deny_unknown_fields)]
pub struct ProjectorPreferences {
 save_windows:bool,
 always_on_top:bool,hide_cursor:bool,limit_fullscreen_per_monitor:bool,
 layout:MultiviewLayout,labels:bool,safe_areas:bool,mouse_switching:bool,
 double_click_transition:bool,
}
impl Default for ProjectorPreferences{fn default()->Self{Self{save_windows:false,always_on_top:false,hide_cursor:false,limit_fullscreen_per_monitor:false,layout:MultiviewLayout::Top8,labels:true,safe_areas:true,mouse_switching:true,double_click_transition:false}}}
impl ProjectorPreferences {
 pub fn save_windows(self)->bool{self.save_windows}
 pub fn with_save_windows(mut self,value:bool)->Self{self.save_windows=value;self}
 pub fn is_default(&self)->bool{*self==Self::default()}
 pub fn always_on_top(self)->bool{self.always_on_top}
 pub fn with_always_on_top(mut self,value:bool)->Self{self.always_on_top=value;self}
 pub fn hide_cursor(self)->bool{self.hide_cursor}
 pub fn with_hide_cursor(mut self,value:bool)->Self{self.hide_cursor=value;self}
 pub fn limit_fullscreen_per_monitor(self)->bool{self.limit_fullscreen_per_monitor}
 pub fn with_limit_fullscreen_per_monitor(mut self,value:bool)->Self{self.limit_fullscreen_per_monitor=value;self}
 pub fn layout(self)->MultiviewLayout{self.layout}
 pub fn with_layout(mut self,value:MultiviewLayout)->Self{self.layout=value;self}
 pub fn labels(self)->bool{self.labels}
 pub fn with_labels(mut self,value:bool)->Self{self.labels=value;self}
 pub fn safe_areas(self)->bool{self.safe_areas}
 pub fn with_safe_areas(mut self,value:bool)->Self{self.safe_areas=value;self}
 pub fn mouse_switching(self)->bool{self.mouse_switching}
 pub fn with_mouse_switching(mut self,value:bool)->Self{self.mouse_switching=value;self}
 pub fn double_click_transition(self)->bool{self.double_click_transition}
 pub fn with_double_click_transition(mut self,value:bool)->Self{self.double_click_transition=value;self}
 }
