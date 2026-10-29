//! Serialized metadata-only controls on the actual PlaybackClock owner.
use super::*;
use crate::image_slideshow::{self,transport::{Control,Target,Status,Receipt,Runtime}};
impl PlaybackClock{
    fn slideshow_scope(&self,session:&Session,target:Target,now:MediaTime)->Result<(),PlaybackError>{
        if session.is_closed(){return Err(PlaybackError::Closed);}let model=session.snapshot();
        if target.session!=session.identity()||target.revision!=model.revision()||self.scope.as_ref().is_none_or(|scope|scope.0!=target.session||scope.1!=target.revision){return Err(PlaybackError::Control("Slideshow control is stale; refresh the current source."));}
        if !matches!(model.source(target.source).map(|source|source.kind()),Some(SourceKind::ImageSlideShow{..})){return Err(PlaybackError::Control("Slideshow source settings changed; refresh the current source."));}
        if self.last_now.is_some_and(|old|now<old){return Err(PlaybackError::Control("Slideshow controls require forward media time."));}
        Ok(())
    }
    fn slideshow_target(&self,session:&Session,target:Target,now:MediaTime)->Result<&ActiveSource,PlaybackError>{
        self.slideshow_scope(session,target,now)?;let model=session.snapshot();
        let active=self.active.get(&target.source).filter(|active|active.visible).ok_or(PlaybackError::Control("Slideshow controls require a source visible in an active output."))?;
        if !matches!(&active.asset.kind,SourceKind::ImageSlideShow{..})||model.source(target.source).is_none_or(|source|source.kind()!=&active.asset.kind){return Err(PlaybackError::Control("Slideshow source settings changed; refresh the current source."));}if matches!(&active.asset.kind,SourceKind::ImageSlideShow{settings} if settings.images().is_empty()){return Err(PlaybackError::Control("Slideshow has no playable images."));}Ok(active)
    }
    /// Original source hotkey wrappers differ from the explicit media controls:
    /// ignore hidden sources, gate Next/Previous on runtime manual, and toggle
    /// the actual paused flag (not the displayed media-state enum).
    pub fn control_slideshow_hotkey(&mut self,session:&Session,target:Target,operation:skylive_core::SourcePlaybackAction,now:MediaTime)->Result<Option<Receipt>,PlaybackError>{
        self.control_slideshow_hotkey_with_accept(session,target,operation,now,||true)
    }
    pub fn control_slideshow_hotkey_with_accept(&mut self,session:&Session,target:Target,operation:skylive_core::SourcePlaybackAction,now:MediaTime,accept:impl FnOnce()->bool)->Result<Option<Receipt>,PlaybackError>{
        self.slideshow_scope(session,target,now)?;
        if self.active.get(&target.source).is_none_or(|active|!active.visible){return Ok(None);}
        let active=self.slideshow_target(session,target,now)?;
        let SourceKind::ImageSlideShow{settings}=&active.asset.kind else{unreachable!("validated slideshow target")};
        use skylive_core::SourcePlaybackAction as Hotkey;
        let manual=active.transport.map_or(settings.mode()==skylive_core::SlideShowMode::Manual,|runtime|runtime.hotkey_manual());
        if !operation.supported(&active.asset.kind){return Err(PlaybackError::Control("Playback shortcuts require a matching Media Source or Slideshow control."));}
        if matches!(operation,Hotkey::Next|Hotkey::Previous)&&!manual{return Ok(None);}
        let paused=active.transport.is_some_and(|runtime|runtime.hotkey_paused());
        let control=match operation{Hotkey::PlayPause=>if paused{Control::Play}else{Control::Pause},Hotkey::Restart=>Control::Restart,Hotkey::Stop=>Control::Stop,Hotkey::Previous=>Control::Previous,Hotkey::Next=>Control::Next,Hotkey::Play|Hotkey::Pause=>return Err(PlaybackError::Control("Playback shortcuts require a matching Media Source or Slideshow control."))};
        if !accept(){return Ok(None);}
        self.control_slideshow(session,target,control,now).map(Some)
    }
    fn slideshow_runtime(active:&ActiveSource,now:MediaTime,allow_discard:bool)->Result<(Runtime,bool),PlaybackError>{
        let SourceKind::ImageSlideShow{settings}=&active.asset.kind else{unreachable!("validated slideshow target")};let order=&active.asset.order.as_ref().expect("slideshow descriptor order").order;let logical=active.elapsed(now);
        let mut runtime=if let Some(runtime)=active.transport{runtime}else if settings.randomize(){Runtime::random_start(settings,order,active.asset.random_seed,now.0,logical).map_err(PlaybackError::Control)?}else{
            let time=active.sample(now);let mut sample=image_slideshow::sample_ordered(settings,logical,time.transition_extra,order);if let Some(visible)=time.visible_elapsed{sample.image_elapsed_ns=sample.image_elapsed_ns.min(visible.0);sample.next_image_elapsed_ns=sample.next_image_elapsed_ns.min(visible.0);}Runtime::from_sample(settings,order,now.0,logical,sample)
        };
        match runtime.tick(settings,order,logical,now.0){Ok(())=>Ok((runtime,false)),Err(error)if allow_discard&&error==image_slideshow::random::WORK_LIMIT=>{runtime.discard_elapsed(logical);Ok((runtime,true))},Err(error)=>Err(PlaybackError::Control(error))}
    }
    pub fn slideshow_status(&self,session:&Session,target:Target,now:MediaTime)->Result<Status,PlaybackError>{
        let active=self.slideshow_target(session,target,now)?;let(runtime,_)=Self::slideshow_runtime(active,now,false)?;let SourceKind::ImageSlideShow{settings}=&active.asset.kind else{unreachable!("validated slideshow target")};Ok(runtime.status(settings,&active.asset.order.as_ref().expect("slideshow descriptor order").order,now.0))
    }
    pub fn control_slideshow(&mut self,session:&Session,target:Target,operation:Control,now:MediaTime)->Result<Receipt,PlaybackError>{
        let active=self.slideshow_target(session,target,now)?;let(mut candidate,discarded_elapsed)=Self::slideshow_runtime(active,now,matches!(operation,Control::Restart|Control::Stop))?;let SourceKind::ImageSlideShow{settings}=&active.asset.kind else{unreachable!("validated slideshow target")};let order=&active.asset.order.as_ref().expect("slideshow descriptor order").order;
        let applied=candidate.apply(settings,order,operation,now.0).map_err(PlaybackError::Control)?;let status=candidate.status(settings,order,now.0);let epoch=if applied{self.epoch.checked_add(1).ok_or(PlaybackError::IdentityExhausted)?}else{self.epoch};
        if applied&&matches!(operation,Control::Stop|Control::Restart|Control::Next|Control::Previous){if let Some(observer)=&self.temporal_observer{observer.reset_source(target.source).map_err(|error|PlaybackError::Temporal(error.detail))?;}}
        if applied{self.active.get_mut(&target.source).expect("validated active source").transport=Some(candidate);self.epoch=epoch;self.last_now=Some(now);}Ok(Receipt{applied,transition_busy:!applied,status,epoch,discarded_elapsed})
    }
    /// Explicit deterministic source run for headless reproduction. No image IO.
    pub fn reseed_slideshow(&mut self,session:&Session,target:Target,seed:u64,now:MediaTime)->Result<Receipt,PlaybackError>{
        let active=self.slideshow_target(session,target,now)?;let SourceKind::ImageSlideShow{settings}=&active.asset.kind else{unreachable!("validated slideshow target")};if !settings.randomize(){return Err(PlaybackError::Control("Enable OBS random playback before reseeding this slideshow."));}let order=&active.asset.order.as_ref().expect("slideshow descriptor order").order;
        let discarded_elapsed=active.transport.is_some_and(|runtime|runtime.due_random_advances(settings,active.elapsed(now))>image_slideshow::random::MAX_SOURCE_ADVANCES);
        let candidate=Runtime::random_start(settings,order,seed,now.0,active.elapsed(now)).map_err(PlaybackError::Control)?;let status=candidate.status(settings,order,now.0);let epoch=self.epoch.checked_add(1).ok_or(PlaybackError::IdentityExhausted)?;
        self.active.get_mut(&target.source).expect("validated active source").transport=Some(candidate);self.epoch=epoch;self.last_now=Some(now);Ok(Receipt{applied:true,transition_busy:false,status,epoch,discarded_elapsed})
    }
}
