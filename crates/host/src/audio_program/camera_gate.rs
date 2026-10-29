//! Runtime camera control scope. Contains owned metadata, no native handles.
use super::*;
use crate::camera_activation::{SourceEpoch,CameraAudioCut};
const MAX_BYTES:usize=8*1024*1024;
#[derive(Clone)]
struct Gate {control:SourceEpoch,scope:u64,cut:u64,raw_floor:u64}
#[derive(Default)]
pub(super) struct Gates {rows:Vec<Gate>,bytes:usize}
pub(super) struct Change {pub source:Id,pub cut:u64,pub stop:u64}
pub(super) struct Prepared {rows:Vec<Gate>,pub changes:Vec<Change>,pub cut:CameraAudioCut,bytes:usize}
struct Count(usize);
impl std::io::Write for Count {
 fn write(&mut self,data:&[u8])->std::io::Result<usize>{
  self.0=self.0.checked_add(data.len()).filter(|n|*n<=MAX_BYTES).ok_or_else(||std::io::Error::other("camera audio gate metadata"))?;Ok(data.len())
 }
 fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
impl Gates {
 pub(super) fn clear(&mut self){self.rows.clear();self.bytes=self.rows.capacity()*std::mem::size_of::<Gate>();}
 pub(super) fn scope(&self,source:Id)->u64{self.rows.iter().find(|row|row.control.source==source).map_or(0,|row|row.scope)}
 pub(super) fn enabled(&self,source:Id)->bool{self.rows.iter().find(|row|row.control.source==source).is_none_or(|row|row.control.enabled)}
 pub(super) fn floor(&self,source:Id)->(u64,u64){self.rows.iter().find(|row|row.control.source==source).map_or((0,0),|row|(row.raw_floor,row.cut))}
 pub(super) fn bytes(&self)->usize{self.bytes}
 pub(super) fn unchanged(&self,epochs:&[SourceEpoch])->bool{epochs.iter().all(|entry|self.rows.iter().any(|row|row.control.source==entry.source&&row.control.epoch==entry.epoch))}
 pub(super) fn validate(&self,epochs:&[SourceEpoch])->Result<(),ProgramAudioError>{
  if epochs.len()>skylive_core::MAX_SOURCES{return Err(ProgramAudioError::Limit("camera audio gate count"));}
  for(index,entry)in epochs.iter().enumerate(){
   if entry.epoch==0||epochs[..index].iter().any(|old|old.source==entry.source){return Err(ProgramAudioError::Invalid("camera activation epoch is zero or duplicate"));}
   if let Some(old)=self.rows.iter().find(|old|old.control.source==entry.source){
    if entry.epoch<old.control.epoch||entry.epoch==old.control.epoch&&(entry.enabled!=old.control.enabled||!entry.binding.same_av_activation(&old.control.binding)||entry.at!=old.control.at){
     return Err(ProgramAudioError::Invalid("stale camera audio activation epoch or binding"));
    }
   }
  }
  Ok(())
 }
 pub(super) fn prepare(&self,epochs:&[SourceEpoch],rate:u32,cursor:u64,resident:usize,max_bytes:usize,
  histories:&post_dsp::Histories,groups:&HashMap<AudioInputKey,InputGroup>,adopt:bool)->Result<Prepared,ProgramAudioError>{
  self.validate(epochs)?;
  let additions=epochs.iter().filter(|entry|self.rows.iter().all(|old|old.control.source!=entry.source)).count();
  if self.rows.len()+additions>skylive_core::MAX_SOURCES{return Err(ProgramAudioError::Limit("retained camera gate scopes"));}
  let mut count=Count((self.rows.len()+additions)*std::mem::size_of::<Gate>());
  for old in self.rows.iter().filter(|old|epochs.iter().all(|entry|entry.source!=old.control.source)){serde_json::to_writer(&mut count,old.control.binding.as_ref()).map_err(|_|ProgramAudioError::Limit("camera gate descriptors"))?;}
  for entry in epochs{serde_json::to_writer(&mut count,entry.binding.as_ref()).map_err(|_|ProgramAudioError::Limit("camera gate descriptors"))?;}
  if resident.checked_add(count.0).is_none_or(|bytes|bytes>max_bytes){return Err(ProgramAudioError::Backpressure("old/new camera audio gate metadata"));}
  let mut rows=self.rows.clone();rows.try_reserve_exact(additions).map_err(|_|ProgramAudioError::Limit("camera audio gate allocation"))?;
  let mut changes=Vec::new();let mut common=cursor;
  for entry in epochs{
   let index=rows.iter().position(|old|old.control.source==entry.source);
   if index.is_some_and(|index|rows[index].control.epoch==entry.epoch){continue;}
   let requested=u64::try_from((u128::from(entry.at.0)*u128::from(rate)).div_ceil(1_000_000_000)).map_err(|_|ProgramAudioError::TimeOverflow)?;
   let cut=requested.max(cursor);common=common.max(cut);
   let embedded=entry.binding.embedded_audio().then(||AudioInputKey::EmbeddedCamera{source:entry.source,binding:entry.binding.video_only()});
   let raw=entry.binding.audio().and_then(|binding|groups.get(&AudioInputKey::Native(binding.clone()))).or_else(||embedded.as_ref().and_then(|key|groups.get(key)))
    .map_or(requested,|group|u64::try_from(group.ring.end().max(0)).unwrap_or(u64::MAX)).max(requested);
   // Initial policy attachment is not a source restart. Keep the existing
   // default processing scope0 and do not retag its already-produced samples.
   let adopted=adopt&&index.is_none()&&entry.enabled&&histories.has_source_scope(entry.source,0);
   let scope=if !entry.enabled{index.map_or(0,|index|rows[index].scope)}else if adopted{0}else{entry.epoch};
   if !adopted{changes.push(Change {source:entry.source,cut,stop:requested});}
   let row=Gate {control:entry.clone(),scope,cut:if adopted{0}else{cut},raw_floor:if adopted{0}else{raw}};
   if let Some(index)=index{rows[index]=row;}else{rows.push(row);}
  }
  Ok(Prepared {rows,changes,cut:CameraAudioCut {frame:common,sample_rate:rate},bytes:count.0})
 }
 pub(super) fn apply(&mut self,prepared:Prepared){self.rows=prepared.rows;self.bytes=prepared.bytes;}
}
impl AudioProgram {
 pub(super) fn camera_cursor(&self)->u64{self.cursor.or_else(||self.timeline.first_frame()).unwrap_or(0)}
 pub fn set_camera_activation(&mut self,epochs:&[SourceEpoch])->Result<CameraAudioCut,ProgramAudioError>{
  self.set_camera_activation_inner(epochs,false)
 }
 pub(super) fn set_camera_activation_inner(&mut self,epochs:&[SourceEpoch],adopt:bool)->Result<CameraAudioCut,ProgramAudioError>{
  self.camera_gates.validate(epochs)?;
  let cursor=self.camera_cursor();let rate=self.settings.sample_rate();
  if self.camera_gates.unchanged(epochs){return Ok(CameraAudioCut {frame:cursor,sample_rate:rate});}
  // A valid stop has an allocation-free deny/cut path. If metadata preparation
  // subsequently fails, failure is reported, but shared native PCM cannot keep
  // this source processing after its requested stop. Other SourceIDs are intact.
  for entry in epochs.iter().filter(|entry|!entry.enabled){
   let at=u64::try_from((u128::from(entry.at.0)*u128::from(rate)).div_ceil(1_000_000_000)).map_err(|_|ProgramAudioError::TimeOverflow)?;
   self.sync.camera_stop(entry.source,at.max(cursor),at);
   self.histories.camera_stop(entry.source,at.max(cursor),at);
   self.monitor_lanes.retain(|lane|lane.source!=entry.source);
  }
  if self.camera_program_reader{self.histories.retire_camera_unread_tracks(&self.sync,&self.timeline,self.active_output_tracks,cursor);}else{self.histories.retire_unread_camera();}
  let prepared=self.camera_gates.prepare(epochs,rate,cursor,self.bytes(),self.limits.max_bytes,&self.histories,&self.groups,adopt)?;
  for change in &prepared.changes{
   // Every explicit new activation ends the prior source scope at the common
   // cut; retained filtered prefix is read under its original scope only.
   self.sync.camera_stop(change.source,change.cut,change.stop);
   self.histories.camera_stop(change.source,change.cut,change.stop);
   self.monitor_lanes.retain(|lane|lane.source!=change.source);
  }
  self.histories.retire_camera_unread_tracks(&self.sync,&self.timeline,self.active_output_tracks,cursor);
  let cut=prepared.cut;self.camera_gates.apply(prepared);
  if !self.camera_program_reader{self.histories.retire_unread_camera();}
  Ok(cut)
 }
 pub(crate) fn camera_program_reader(&mut self,reader:bool){
  self.camera_program_reader=reader;
  // Physical monitor owns a wall-clock reader which is removed on source stop.
  // Already mixed/native speaker blocks own their own PCM and are not recalled.
  if !reader{self.histories.retire_unread_camera();}
 }
 pub fn camera_activation_retired(&self,epochs:&[SourceEpoch],cut:CameraAudioCut)->bool{
  self.camera_activation_retired_for(epochs,cut,self.camera_program_reader)
 }
 pub(crate) fn camera_activation_retired_for(&self,epochs:&[SourceEpoch],cut:CameraAudioCut,program_reader:bool)->bool{
  self.camera_activation_retired_tracks(epochs,cut,program_reader,self.active_output_tracks)
 }
 pub(crate) fn camera_activation_retired_tracks(&self,epochs:&[SourceEpoch],cut:CameraAudioCut,program_reader:bool,tracks:skylive_core::SourceAudioTrackMask)->bool{
  if cut.sample_rate!=self.settings.sample_rate(){return false;}
  let valid=epochs.iter().all(|entry|self.camera_gates.rows.iter().any(|row|row.control.source==entry.source&&row.control.epoch>=entry.epoch));
  valid&&(!program_reader||epochs.iter().all(|entry|!self.histories.has_source(entry.source)||!self.sync.camera_prefix_routed(entry.source,None,self.camera_cursor(),cut.frame,
      &|source,binding,start,end|self.timeline.source_routes_between(source,binding,start,end,tracks))))
 }
}
