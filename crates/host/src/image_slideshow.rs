//! Bounded playlist preparation and deterministic time sampling. No UI, device
//! resources, wall clock, scene mutation, or new native dependency.
pub mod transport;
pub mod directories;
pub(crate) mod random;
use skylive_core::{ImageAsset,ImageSlideShow,MAX_SLIDES,SlideTransition};
use std::path::PathBuf;
pub fn prepare(paths:&[PathBuf])->Result<Vec<ImageAsset>,String>{
    if paths.is_empty()||paths.len()>MAX_SLIDES{return Err("Select 1..=256 image files".into());}
    if paths.iter().any(|p| p.to_str().is_none_or(|s|s.len()>4096||s.trim().is_empty()||s.chars().any(char::is_control))) {return Err("Image paths must be exact UTF-8, 1..=4096 bytes, without controls".into());}
    let mut reads=crate::assets::ReadBudget::new(128*1024*1024);
    let mut remaining=crate::assets::MAX_ANIMATION_BYTES;
    let mut images=Vec::with_capacity(paths.len());
    for path in paths {
        let prepared=crate::assets::prepare_image_with_limits(path,&mut reads,remaining).map_err(|e|format!("{}: {e}",path.display()))?;
        remaining=remaining.saturating_sub(prepared.decoded_bytes);
        images.push(prepared.asset);
        // Large pixels are released on this I/O owner, never delivered to UI.
        drop(prepared.frame);
    }
    Ok(images)
}
#[derive(Clone,Copy,Debug)]
pub(crate) struct Sample {
    pub current:usize,pub next:Option<usize>,pub image_elapsed_ns:u64,
    pub next_image_elapsed_ns:u64,pub fade_ns:u64,pub fade_duration_ns:u64,
    pub next_after_ns:Option<u64>,pub hidden:bool,pub to_transparent:bool,pub from_transparent:bool,
}
/// Immutable listed/stable order prepared once per owned descriptor.
#[derive(Debug)]pub(crate) struct Order{indices:[usize;MAX_SLIDES],len:usize}
impl std::ops::Deref for Order{type Target=[usize];fn deref(&self)->&Self::Target{&self.indices[..self.len]}}
impl Order{pub(crate) fn new(settings:&ImageSlideShow)->Self{let mut result=Self{indices:[0;MAX_SLIDES],len:settings.images().len()};for(index,value)in result.indices[..result.len].iter_mut().enumerate(){*value=index;}
    if settings.shuffle(){let mut state=0x9e3779b97f4a7c15u64;for image in settings.images(){for byte in image.sha256(){state=(state^u64::from(*byte)).wrapping_mul(0x100000001b3);}}for i in(1..result.len).rev(){state^=state<<13;state^=state>>7;state^=state<<17;result.indices.swap(i,(state%(i as u64+1))as usize);}}result}}
pub(crate) fn sample(settings:&ImageSlideShow,time_ns:u64)->Sample{sample_clock(settings,time_ns,0)}
pub(crate) fn sample_clock(settings:&ImageSlideShow,time_ns:u64,extra:u64)->Sample{
    let order=Order::new(settings);sample_ordered(settings,time_ns,extra,&order)
}
pub(crate) fn sample_ordered(settings:&ImageSlideShow,time_ns:u64,extra:u64,order:&Order)->Sample{
    let n=order.len();let transition=settings.transition_duration_ns();let period=settings.period_ns();
    if n==0{return Sample{current:0,next:None,image_elapsed_ns:0,next_image_elapsed_ns:0,fade_ns:0,fade_duration_ns:transition,next_after_ns:None,hidden:true,to_transparent:false,from_transparent:false};}
    let absolute=time_ns/period;let phase=time_ns%period;let done=!settings.looping()&&absolute>=n as u64;
    if done{
        let ended=(time_ns-(n as u64)*period).saturating_add(extra);let leaving=settings.hide_when_done()&&transition>0&&ended<transition;
        return Sample{current:order[n-1],next:None,image_elapsed_ns:time_ns-(n as u64-1)*period,next_image_elapsed_ns:0,fade_ns:if leaving{ended}else{0},fade_duration_ns:transition,next_after_ns:leaving.then(||(transition-ended).min(16_666_667)),hidden:settings.hide_when_done()&&!leaving,to_transparent:leaving,from_transparent:false};
    }
    let index=(absolute%n as u64)as usize;let transition_phase=phase.saturating_add(extra);let intro=absolute==0&&settings.behavior()!=skylive_core::SlideShowBehavior::StopRestart&&transition>0&&transition_phase<transition;
    let transitioning=n>1&&absolute>0&&transition>0&&transition_phase<transition;
    let current=if transitioning{(index+n-1)%n}else{index};
    // Stop/restart activation cuts to the first slide. Later incoming images
    // activate at period boundaries, not one transition duration before them.
    let after=if transitioning||intro{Some((transition-transition_phase).min(16_666_667))}else if n==1&&settings.looping()||!settings.looping()&&index+1==n&&!settings.hide_when_done(){None}else{Some(period-phase)};
    Sample{current:order[current],next:transitioning.then(||order[index]),image_elapsed_ns:if n==1{time_ns}else if transitioning{period+phase}else{phase},next_image_elapsed_ns:phase,fade_ns:if transitioning||intro{transition_phase}else{0},fade_duration_ns:transition,next_after_ns:after,hidden:false,to_transparent:false,from_transparent:intro}
}

/// Playlist position may pause while the private transition continues during
/// hidden time; GIF animation restarts when the source becomes visible again.
pub(crate) fn sample_playback(settings:&ImageSlideShow,source:skylive_core::Id,playback:&crate::playback::PlaybackSnapshot)->Result<Sample,&'static str>{
    if settings.images().is_empty(){return Ok(sample_ordered(settings,0,0,&Order::new(settings)));}
    let mut sample=if let Some(sample)=playback.slideshow_sample(source){sample}else{
        if settings.randomize(){return Err("Random slideshow requires its owned playback clock; cold snapshots cannot invent a random run.");}
        let owned;let order=if let Some(order)=playback.slideshow_order(source){order}else{owned=Order::new(settings);&owned};
        if settings.mode()==skylive_core::SlideShowMode::Manual{let seed=sample_ordered(settings,0,0,order);transport::Runtime::from_sample(settings,order,0,0,seed).sample(settings,playback.elapsed(source).0)}else{sample_ordered(settings,playback.elapsed(source).0,playback.slideshow_transition_extra(source),order)}
    };if let Some(visible)=playback.visible_elapsed(source){sample.image_elapsed_ns=sample.image_elapsed_ns.min(visible.0);sample.next_image_elapsed_ns=sample.next_image_elapsed_ns.min(visible.0);}Ok(sample)
}
