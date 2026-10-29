//! Authored bounded image playlist. Raster I/O, elapsed time and decoded cache
//! remain host-owned; updating the complete settings is one canonical edit.
use crate::{Error,ImageAsset};
use serde::{Deserialize,Serialize};
pub const MAX_SLIDES:usize=256;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum SlideTransition {Cut,Fade,Swipe,Slide}
impl SlideTransition{pub const ALL:[Self;4]=[Self::Cut,Self::Fade,Self::Swipe,Self::Slide];pub fn label(self)->&'static str{match self{Self::Cut=>"Cut",Self::Fade=>"Fade",Self::Swipe=>"Swipe",Self::Slide=>"Slide"}}}
/// Missing native fields preserve the original port's automatic canvas.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(tag="mode",rename_all="snake_case",deny_unknown_fields)]
pub enum SlideShowCanvas{#[default] Auto,Resolution{width:u32,height:u32},Aspect{width:u32,height:u32}}
impl SlideShowCanvas{
    pub fn is_default(&self)->bool{*self==Self::Auto}
    pub fn validate(self)->Result<(),Error>{match self{Self::Auto=>Ok(()),Self::Resolution{width,height}|Self::Aspect{width,height}=>if !(1..=8192).contains(&width)||!(1..=8192).contains(&height){Err(Error::Invalid("slideshow canvas dimensions or aspect terms must be 1..=8192"))}else if matches!(self,Self::Resolution{..})&&u64::from(width)*u64::from(height)>16_777_216{Err(Error::Limit("slideshow canvas exceeds the 16M-pixel image budget"))}else{Ok(())}}}
    fn project(self,raw:(u32,u32))->(u32,u32){match self{Self::Auto=>raw,Self::Resolution{width,height}=>(width,height),Self::Aspect{width,height}=>{let old=f64::from(raw.0)/f64::from(raw.1);let new=f64::from(width)/f64::from(height);if(old-new).abs()<=f64::from(1e-4f32){raw}else if new>old{((f64::from(raw.1)*new)as u32,raw.1)}else{(raw.0,(f64::from(raw.0)/new)as u32)}}}}
    pub fn resolve(self,raw:(u32,u32))->Result<(u32,u32),Error>{self.validate()?;if raw.0==0||raw.1==0{return Err(Error::Invalid("slideshow image dimensions are empty"));}let output=self.project(raw);if output.0==0||output.1==0||output.0>8192||output.1>8192||u64::from(output.0)*u64::from(output.1)>16_777_216{return Err(Error::Limit("slideshow canvas exceeds 8192-edge/16M-pixel image limits"));}Ok(output)}
}
/// Visibility controls the source's explicit elapsed timeline, never a device.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum SlideShowBehavior{#[default]StopRestart,PauseUnpause,AlwaysPlay}
impl SlideShowBehavior{
    pub const ALL:[Self;3]=[Self::StopRestart,Self::PauseUnpause,Self::AlwaysPlay];
    pub fn is_default(&self)->bool{*self==Self::StopRestart}
    pub fn label(self)->&'static str{match self{Self::StopRestart=>"Stop when hidden; restart when visible",Self::PauseUnpause=>"Pause when hidden; resume when visible",Self::AlwaysPlay=>"Always play, even when hidden"}}
    pub fn runs_hidden(self)->bool{self==Self::AlwaysPlay}
    pub fn retains_hidden(self)->bool{self!=Self::StopRestart}
}
/// Authored initial mode; media controls may override it only at runtime.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum SlideShowMode{#[default]Automatic,Manual}
impl SlideShowMode{pub fn is_default(&self)->bool{*self==Self::Automatic}pub fn label(self)->&'static str{match self{Self::Automatic=>"Automatic slideshow",Self::Manual=>"Manual slideshow"}}}
/// Exact source-version differences for authored OBS random playback.
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum SlideShowRandomCompatibility{#[default]BufferedV2,LegacyV1}
impl SlideShowRandomCompatibility{pub fn is_default(&self)->bool{*self==Self::BufferedV2}}
fn no_random(value:&bool)->bool{!*value}
#[derive(Clone,Debug,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[serde(try_from="Data")]
pub struct ImageSlideShow {
    #[serde(default,skip_serializing_if="Vec::is_empty")]entries:Vec<crate::SlideShowEntry>,
    #[serde(default,skip_serializing_if="Vec::is_empty")]entry_image_counts:Vec<u16>,
    #[serde(default,skip_serializing_if="no_random")]randomize:bool,
    #[serde(default,skip_serializing_if="SlideShowRandomCompatibility::is_default")]random_compatibility:SlideShowRandomCompatibility,
    #[serde(default,skip_serializing_if="SlideShowMode::is_default")]mode:SlideShowMode,
    #[serde(default,skip_serializing_if="SlideShowCanvas::is_default")]canvas:SlideShowCanvas,
    #[serde(default,skip_serializing_if="SlideShowBehavior::is_default")]behavior:SlideShowBehavior,
    images:Vec<ImageAsset>,duration_ms:u32,transition:SlideTransition,transition_ms:u32,
    looping:bool,shuffle:bool,hide_when_done:bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {#[serde(default)]entries:Vec<crate::SlideShowEntry>,#[serde(default)]entry_image_counts:Vec<u16>,#[serde(default)]randomize:bool,#[serde(default)]random_compatibility:SlideShowRandomCompatibility,#[serde(default)]mode:SlideShowMode,#[serde(default)]canvas:SlideShowCanvas,#[serde(default)]behavior:SlideShowBehavior,images:Vec<ImageAsset>,duration_ms:u32,transition:SlideTransition,transition_ms:u32,looping:bool,shuffle:bool,hide_when_done:bool}
impl TryFrom<Data> for ImageSlideShow {
    type Error=Error;
    fn try_from(v:Data)->Result<Self,Error>{Self::new_with_options(v.images,v.duration_ms,v.transition,v.transition_ms,v.looping,v.shuffle,v.hide_when_done,v.canvas,v.behavior)?.with_mode(v.mode)?.with_randomization(v.randomize,v.random_compatibility)?.with_entries(v.entries,v.entry_image_counts)}
}
impl ImageSlideShow {
    pub fn new(images:Vec<ImageAsset>,duration_ms:u32,transition:SlideTransition,transition_ms:u32,looping:bool,shuffle:bool,hide_when_done:bool)->Result<Self,Error>{
        Self::new_with_options(images,duration_ms,transition,transition_ms,looping,shuffle,hide_when_done,Default::default(),Default::default())
    }
    pub fn new_with_options(images:Vec<ImageAsset>,duration_ms:u32,transition:SlideTransition,transition_ms:u32,looping:bool,shuffle:bool,hide_when_done:bool,canvas:SlideShowCanvas,behavior:SlideShowBehavior)->Result<Self,Error>{
        canvas.validate()?;let value=Self {entries:Vec::new(),entry_image_counts:Vec::new(),randomize:false,random_compatibility:SlideShowRandomCompatibility::BufferedV2,mode:SlideShowMode::Automatic,canvas,behavior,images,duration_ms,transition,transition_ms,looping,shuffle,hide_when_done};value.validate()?;Ok(value)
    }
    pub fn with_entries(mut self,entries:Vec<crate::SlideShowEntry>,counts:Vec<u16>)->Result<Self,Error>{crate::slideshow_entries::validate(&entries,&counts,&self.images)?;self.entries=entries;self.entry_image_counts=counts;self.validate()?;Ok(self)}
    /// Replace prepared resource metadata without changing authored playback/canvas options.
    pub fn with_prepared_resources(mut self,images:Vec<ImageAsset>,entries:Vec<crate::SlideShowEntry>,counts:Vec<u16>)->Result<Self,Error>{self.images=images;self.entries=entries;self.entry_image_counts=counts;self.validate()?;Ok(self)}
    pub fn entries(&self)->&[crate::SlideShowEntry]{&self.entries}
    pub fn entry_image_counts(&self)->&[u16]{&self.entry_image_counts}
    pub fn has_directories(&self)->bool{self.entries.iter().any(|entry|entry.kind()==crate::SlideShowEntryKind::Directory)}
    pub fn authored_entries(&self)->Vec<crate::SlideShowEntry>{if self.entries.is_empty(){self.images.iter().map(|image|crate::SlideShowEntry::new(crate::SlideShowEntryKind::File,image.path().to_owned()).expect("validated image path")).collect()}else{self.entries.clone()}}
    pub fn with_options(mut self,canvas:SlideShowCanvas,behavior:SlideShowBehavior)->Result<Self,Error>{canvas.validate()?;self.canvas=canvas;self.behavior=behavior;self.validate()?;Ok(self)}
    pub fn with_mode(mut self,mode:SlideShowMode)->Result<Self,Error>{self.mode=mode;self.validate()?;Ok(self)}
    pub fn validate_order(shuffle:bool,randomize:bool)->Result<(),Error>{if shuffle&&randomize{Err(Error::Invalid("stable shuffle and OBS random playback cannot both be enabled"))}else{Ok(())}}
    pub fn validate_randomization(shuffle:bool,randomize:bool,compatibility:SlideShowRandomCompatibility)->Result<(),Error>{Self::validate_order(shuffle,randomize)?;if !randomize&&!compatibility.is_default(){return Err(Error::Invalid("legacy random compatibility requires enabled random playback"));}Ok(())}
    pub fn with_randomization(mut self,randomize:bool,compatibility:SlideShowRandomCompatibility)->Result<Self,Error>{Self::validate_randomization(self.shuffle,randomize,compatibility)?;self.randomize=randomize;self.random_compatibility=compatibility;self.validate()?;Ok(self)}
    pub fn randomize(&self)->bool{self.randomize}
    pub fn random_compatibility(&self)->SlideShowRandomCompatibility{self.random_compatibility}
    pub fn mode(&self)->SlideShowMode{self.mode}
    pub fn canvas(&self)->SlideShowCanvas{self.canvas}
    pub fn behavior(&self)->SlideShowBehavior{self.behavior}
    pub fn images(&self)->&[ImageAsset]{&self.images}
    pub fn duration_ms(&self)->u32{self.duration_ms}
    pub fn transition(&self)->SlideTransition{self.transition}
    pub fn transition_ms(&self)->u32{self.transition_ms}
    pub fn transition_duration_ns(&self)->u64{if self.transition==SlideTransition::Cut{0}else{u64::from(self.transition_ms)*1_000_000}}
    pub fn period_ns(&self)->u64{u64::from(self.duration_ms)*1_000_000+self.transition_duration_ns()}
    pub fn looping(&self)->bool{self.looping}
    pub fn shuffle(&self)->bool{self.shuffle}
    pub fn hide_when_done(&self)->bool{self.hide_when_done}
    pub fn image_dimensions(&self)->(u32,u32){(self.images.iter().map(ImageAsset::width).max().unwrap_or(0),self.images.iter().map(ImageAsset::height).max().unwrap_or(0))}
    pub fn dimensions(&self)->(u32,u32){if self.images.is_empty(){match self.canvas{SlideShowCanvas::Resolution{width,height}=>(width,height),_=>(1,1)}}else{self.canvas.project(self.image_dimensions())}}
    pub fn validate_timing(duration_ms:u32,transition:SlideTransition,transition_ms:u32)->Result<(),Error>{
        let minimum=if transition==SlideTransition::Cut {50}else{100};
        if duration_ms<minimum||duration_ms>3_600_000{return Err(Error::Invalid("slideshow hold time exceeds supported 50/100ms..1h range"));}
        if transition_ms>3_600_000{return Err(Error::Invalid("slideshow transition speed must be 0..=3600000ms; Cut retains the value without animating"));}
        Ok(())
    }
    pub(crate) fn validate(&self)->Result<(),Error>{
        if self.images.len()>MAX_SLIDES{return Err(Error::Invalid("slideshow supports at most 256 images"));}
        for image in &self.images {image.validate()?;}
        let (w,h)=if self.images.is_empty(){self.dimensions()}else{self.canvas.resolve(self.image_dimensions())?};
        if u64::from(w)*u64::from(h)>16_777_216{return Err(Error::Invalid("slideshow bounding canvas exceeds image pixel budget"));}
        crate::slideshow_entries::validate(&self.entries,&self.entry_image_counts,&self.images)?;
        Self::validate_randomization(self.shuffle,self.randomize,self.random_compatibility)?;
        Self::validate_timing(self.duration_ms,self.transition,self.transition_ms)?;
        Ok(())
    }
}
