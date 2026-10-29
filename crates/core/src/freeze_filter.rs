//! Authored Exeldro Freeze Filter0.3.5 controls. Runtime pixels, activity and
//! native hotkeys belong to the host; this module performs no work or I/O.
use crate::Error;
use serde::{Deserialize,Serialize};
pub const EXELDRO_FREEZE_PLUGIN:&str="exeldro/obs-freeze-filter";
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum FreezeAction{#[default]None,Enable,Disable}
impl FreezeAction{
 pub const ALL:[Self;3]=[Self::None,Self::Enable,Self::Disable];
 pub fn label(self)->&'static str{match self{Self::None=>"None",Self::Enable=>"Enable Freeze",Self::Disable=>"Disable Freeze"}}
 pub fn from_obs(value:u64)->Result<Self,Error>{match value{0=>Ok(Self::None),1=>Ok(Self::Enable),2=>Ok(Self::Disable),_=>Err(Error::Invalid("Freeze action requires None, Enable or Disable"))}}
 pub fn obs_value(self)->u32{match self{Self::None=>0,Self::Enable=>1,Self::Disable=>2}}
}
#[derive(Clone,Copy,Debug,PartialEq,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct FreezeSettings{duration_ms:u32,refresh_interval_ms:u32,start_delay_ms:u32,end_delay_ms:u32,log_enable:bool,mask:bool,mask_percent:[f32;4],activate_action:FreezeAction,deactivate_action:FreezeAction,show_action:FreezeAction,hide_action:FreezeAction}
#[derive(Deserialize)]#[serde(default,deny_unknown_fields)]
struct Data{duration_ms:u32,refresh_interval_ms:u32,start_delay_ms:u32,end_delay_ms:u32,log_enable:bool,mask:bool,mask_percent:[f32;4],activate_action:FreezeAction,deactivate_action:FreezeAction,show_action:FreezeAction,hide_action:FreezeAction}
impl Default for Data{fn default()->Self{Self{duration_ms:0,refresh_interval_ms:0,start_delay_ms:0,end_delay_ms:0,log_enable:false,mask:false,mask_percent:[0.0;4],activate_action:FreezeAction::None,deactivate_action:FreezeAction::None,show_action:FreezeAction::None,hide_action:FreezeAction::None}}}
impl TryFrom<Data> for FreezeSettings{type Error=Error;fn try_from(d:Data)->Result<Self,Error>{let value=Self{duration_ms:d.duration_ms,refresh_interval_ms:d.refresh_interval_ms,start_delay_ms:d.start_delay_ms,end_delay_ms:d.end_delay_ms,log_enable:d.log_enable,mask:d.mask,mask_percent:d.mask_percent,activate_action:d.activate_action,deactivate_action:d.deactivate_action,show_action:d.show_action,hide_action:d.hide_action};value.validate()?;Ok(value)}}
impl Default for FreezeSettings{fn default()->Self{Data::default().try_into().expect("static Freeze defaults")}}
impl FreezeSettings{
 pub fn timing(self)->[u32;4]{[self.duration_ms,self.refresh_interval_ms,self.start_delay_ms,self.end_delay_ms]}
 pub fn duration_ms(self)->u32{self.duration_ms}pub fn refresh_interval_ms(self)->u32{self.refresh_interval_ms}pub fn start_delay_ms(self)->u32{self.start_delay_ms}pub fn end_delay_ms(self)->u32{self.end_delay_ms}
 pub fn mask(self)->bool{self.mask}pub fn mask_percent(self)->[f32;4]{self.mask_percent}pub fn log_enable(self)->bool{self.log_enable}
 pub fn actions(self)->[FreezeAction;4]{[self.activate_action,self.deactivate_action,self.show_action,self.hide_action]}
 pub fn with_timing(mut self,timing:[u32;4])->Result<Self,Error>{[self.duration_ms,self.refresh_interval_ms,self.start_delay_ms,self.end_delay_ms]=timing;self.validate()?;Ok(self)}
 pub fn with_mask(mut self,enabled:bool,percent:[f32;4])->Result<Self,Error>{self.mask=enabled;self.mask_percent=percent;self.validate()?;Ok(self)}
 pub fn with_logging(mut self,enabled:bool)->Self{self.log_enable=enabled;self}
 pub fn with_actions(mut self,actions:[FreezeAction;4])->Self{[self.activate_action,self.deactivate_action,self.show_action,self.hide_action]=actions;self}
 pub fn validate(self)->Result<(),Error>{if self.timing().iter().any(|value|*value>100_000){return Err(Error::Invalid("Freeze timings require 0..100000 milliseconds"));}if self.mask_percent.iter().any(|v|!v.is_finite()||!(0.0..=100.0).contains(v)){return Err(Error::Invalid("Freeze mask percentages require finite values in 0..100"));}Ok(())}
}
impl FreezeSettings{
 /// Duration and refresh use the application-owner coordinator. Other donor
 /// automatic controls remain unavailable until their real routes exist.
 pub fn validate_manual(self)->Result<(),Error>{
  self.validate()?;
  if self.start_delay_ms!=0||self.end_delay_ms!=0||self.log_enable||self.actions()!=[FreezeAction::None;4]{
   return Err(Error::Unavailable("Freeze start/end delays, lifecycle actions and logging are not implemented"));
  }
  Ok(())
 }
}
