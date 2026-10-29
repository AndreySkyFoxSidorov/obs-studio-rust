// SPDX-License-Identifier: MIT
//! SoundCraft Mod Delay and FilmCraft FDN Reverb, original2026ArtCraft contributors.
//! Notices: assets/licenses/{soundcraft,filmcraft}-audio-effects-MIT.txt.
mod storage;
mod delay;
mod reverb;
pub use delay::{ModDelay,ModDelayParameters};
pub use reverb::{FdnReverb,FdnReverbParameters};
use std::fmt;
pub const MAX_CRAFT_TIME_STATE:usize=32*1024*1024;
#[derive(Clone,Copy,Debug)]pub struct CraftTimeLimits{pub max_frames:usize,pub max_state_bytes:usize,pub max_sample_visits:usize}
impl Default for CraftTimeLimits{fn default()->Self{Self{max_frames:1024,max_state_bytes:MAX_CRAFT_TIME_STATE,max_sample_visits:1024*8*256}}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]pub enum CraftTimeError{InvalidParameter(&'static str),InvalidFormat,InvalidBlock,NonFinite,Poisoned,Budget}
impl fmt::Display for CraftTimeError{fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result{write!(f,"{self:?}")}}
impl std::error::Error for CraftTimeError{}
fn range(name:&'static str,value:f32,min:f32,max:f32)->Result<(),CraftTimeError>{if value.is_finite()&&(min..=max).contains(&value){Ok(())}else{Err(CraftTimeError::InvalidParameter(name))}}
fn format(rate:u32,channels:u16)->Result<(),CraftTimeError>{if (8000..=192000).contains(&rate)&&(1..=8).contains(&channels){Ok(())}else{Err(CraftTimeError::InvalidFormat)}}
fn admit(samples:&[f32],channels:usize,state:usize,visits:usize,limits:CraftTimeLimits)->Result<usize,CraftTimeError>{
 if channels==0||!samples.len().is_multiple_of(channels){return Err(CraftTimeError::InvalidBlock);}
 let frames=samples.len()/channels;
 if frames>limits.max_frames||state>limits.max_state_bytes||frames.checked_mul(visits).is_none_or(|n|n>limits.max_sample_visits){return Err(CraftTimeError::Budget);}
 if samples.iter().any(|v|!v.is_finite()){return Err(CraftTimeError::NonFinite);}Ok(frames)
}
