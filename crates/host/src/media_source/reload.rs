//! Runtime-only exact-definition proof; never serialized into a project.
use super::*;
use skylive_core::SourceDefinitionEpoch;
pub const MAX_MEDIA_RELOAD_TARGETS:usize=64;
pub const MAX_MEDIA_RELOAD_METADATA:usize=2*1024*1024;
#[derive(Clone,Debug)]
pub struct MediaReloadTarget{session:u64,source:Id,definition:SourceDefinitionEpoch,media:MediaSource}
impl MediaReloadTarget{
 pub fn new(session:&Session,source:Id)->Result<Self,MediaSourceError>{
  if session.is_closed(){return Err(MediaSourceError::Closed);}let model=session.snapshot();
  let Some(skylive_core::SourceKind::MediaSource{media})=model.source(source).map(|s|s.kind())else{return Err(MediaSourceError::Invalid("Reload requires an existing local Media Source"));};
  let definition=session.source_definition_epoch(source).ok_or(MediaSourceError::Invalid("Media Source definition is unavailable"))?;
  if media.path().len()>16_384{return Err(MediaSourceError::Limit("Media reload target path exceeds 16 KiB"));}
  Ok(Self{session:session.identity(),source,definition,media:media.clone()})
 }
 pub fn source(&self)->Id{self.source}
 pub fn metadata_bytes(&self)->usize{std::mem::size_of::<Self>().saturating_add(self.media.path().len())}
 pub fn matches(&self,session:&Session)->bool{
  !session.is_closed()&&session.identity()==self.session&&session.source_definition_epoch(self.source)==Some(self.definition)&&matches!(session.snapshot().source(self.source).map(|s|s.kind()),Some(skylive_core::SourceKind::MediaSource{media})if media==&self.media)
 }
}
impl MediaOwner{
 /// Uses one normal Control job and its actual Controlled receipt. The receipt
 /// acknowledges old-reader retirement; later decoding readiness/errors remain
 /// in MediaSnapshot. No second decoder or GUI/native work is introduced.
 pub fn reload(&mut self,session:&Session,target:&MediaReloadTarget,now:MediaTime)->Result<MediaRequestKey,MediaSourceError>{
  if !target.matches(session){return Err(MediaSourceError::Invalid("Media reload target changed; prepare it again"));}
  now.0.checked_add(1_000_000_000).ok_or(MediaSourceError::TimeOverflow)?;
  let key=self.key(session)?;
  // An uninstantiated/inactive source needs a worker receipt, not a Scope that
  // would instantiate a reader just to close it. The actor checks real absence.
  let control=self.control_target(session,target.source).unwrap_or_else(|_|MediaControlTarget{
   instance:self.instance,session:session.identity(),epoch:self.epoch,revision:session.snapshot().revision(),source:target.source,
   media:Arc::new(target.media.clone()),status:MediaTransportStatus{state:MediaPlaybackState::Stopped,stream_generation:0,ready:false,last_control:None},
  });
  self.send(key,Job::Control{key,target:control,request:ControlRequest::Direct(MediaControl::Reload),now,sent:std::time::Instant::now(),gate:None})?;
  Ok(key)
 }
}
