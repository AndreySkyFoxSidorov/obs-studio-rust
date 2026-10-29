// SPDX-License-Identifier: GPL-2.0-or-later
//! OBS32 callback FIFO, owned by its exact compressor instance. No device or output.
use super::*;
pub(crate) const QUANTUM:usize=1024;
const SPANS:usize=64;
pub(crate) const RETAINED_LIMIT:usize=4*1024*1024;
/// Includes old retained queues plus transactional per-source candidate/new queues.
pub(crate) const RESERVATION:usize=8*1024*1024;
#[derive(Clone,Copy,Default,Debug,PartialEq,Eq)]
pub(crate) struct SignalStamp{
 pub session:u64,pub source:Option<Id>,pub scope:u64,pub epoch:u64,
 pub native_epoch:u64,pub generation:u64,pub processing_epoch:u64,
 pub lineage:super::delayed_source::Lineage,
 pub config:u64,pub revision:u64,pub control:u64,pub muted:bool,pub start:u64,
}
impl SignalStamp{
 fn same_metadata(self,b:Self)->bool{self.same_epoch(b)&&self.lineage==b.lineage&&self.config==b.config&&self.revision==b.revision&&self.control==b.control&&self.muted==b.muted}
 fn same_epoch(self,b:Self)->bool{self.session==b.session&&self.source==b.source&&self.scope==b.scope&&self.epoch==b.epoch&&self.native_epoch==b.native_epoch&&self.generation==b.generation&&self.processing_epoch==b.processing_epoch}
}
#[derive(Clone,Copy,Default)]
struct Span{stamp:SignalStamp,frames:usize}
#[derive(Clone)]
pub(crate) struct DetectorQueue{
 key:Id,channels:usize,pcm:Box<[f32]>,scratch:Box<[f32]>,spans:Box<[Span]>,
 head:usize,len:usize,span_len:usize,max_frames:usize,
}
pub(crate) fn queue_bytes(channels:u16)->usize{
 (4*QUANTUM*usize::from(channels))*std::mem::size_of::<f32>()+SPANS*std::mem::size_of::<Span>()+std::mem::size_of::<DetectorQueue>()
}
pub(crate) fn chain_bytes(filters:&[AudioFilter],channels:u16)->usize{
 filters.iter().filter(|f|f.sidechain_source().is_some()).count()*queue_bytes(channels)
}
fn zeros<T:Default+Clone>(len:usize)->Result<Box<[T]>,AudioError>{
 let mut v=Vec::new();v.try_reserve_exact(len).map_err(|_|AudioError::Limit("sidechain queue allocation"))?;
 v.resize(len,T::default());Ok(v.into_boxed_slice())
}
impl DetectorQueue{
 pub(crate) fn new(key:Id,channels:u16)->Result<Box<Self>,AudioError>{
  let channels=usize::from(channels);
  Ok(Box::new(Self{key,channels,pcm:zeros(3*QUANTUM*channels)?,scratch:zeros(QUANTUM*channels)?,spans:zeros(SPANS)?,
   head:0,len:0,span_len:0,max_frames:0}))
 }
 pub(crate) fn key(&self)->Id{self.key}
 pub(crate) fn clear(&mut self){self.head=0;self.len=0;self.span_len=0;}
 fn discard(&mut self,mut frames:usize){
  frames=frames.min(self.len);self.head=(self.head+frames)%(3*QUANTUM);self.len-=frames;
  while frames!=0{let n=frames.min(self.spans[0].frames);self.spans[0].frames-=n;self.spans[0].stamp.start+=n as u64;frames-=n;
   if self.spans[0].frames==0{self.spans.copy_within(1..self.span_len,0);self.span_len-=1;}
  }
 }
 pub(crate) fn available(&mut self,keys:&[(Id,AudioInputKey,u64,u64)]){
  if !keys.iter().any(|(id,_,epoch,scope)|*id==self.key&&(self.span_len==0||(*epoch==self.spans[self.span_len-1].stamp.epoch&&*scope==self.spans[self.span_len-1].stamp.scope))){self.clear();}
 }
 pub(crate) fn cut(&mut self,source:Id,cut:u64){
  if source!=self.key{return;}
  let mut keep=0usize;let mut count=0;
  for span in &mut self.spans[..self.span_len]{
   if span.stamp.start>=cut{break;}
   span.frames=span.frames.min(usize::try_from(cut-span.stamp.start).unwrap_or(usize::MAX));
   keep+=span.frames;count+=1;
   if span.stamp.start+span.frames as u64>=cut{break;}
  }
  self.len=keep;self.span_len=count;
 }
 pub(crate) fn check(&self,stamp:SignalStamp,frames:usize)->Result<(),AudioError>{
  if frames>QUANTUM{return Err(AudioError::Limit("sidechain callback exceeds project quantum"));}
  if self.span_len==0||!self.spans[self.span_len-1].stamp.same_epoch(stamp){return Ok(());}
  let max=self.max_frames.max(frames);let drop=if self.len>2*max{max}else{0};
  let mut remaining=drop;let mut removed=0;
  for span in &self.spans[..self.span_len]{if remaining<span.frames{break;}remaining-=span.frames;removed+=1;}
  let last=self.spans[self.span_len-1];
  let merges=last.stamp.same_metadata(stamp)&&last.stamp.start+last.frames as u64==stamp.start;
  if !merges&&self.span_len-removed>=SPANS{return Err(AudioError::Limit("sidechain provenance queue exceeds64 discontinuous spans"));}
  Ok(())
 }
 pub(crate) fn publish(&mut self,stamp:SignalStamp,samples:&[f32],muted:bool){
  let frames=samples.len()/self.channels;
  if self.span_len!=0&&!self.spans[self.span_len-1].stamp.same_epoch(stamp){self.clear();}
  self.max_frames=self.max_frames.max(frames);
  if self.len>2*self.max_frames{self.discard(self.max_frames);}
  for (i,frame) in samples.chunks_exact(self.channels).enumerate(){
   let index=((self.head+self.len+i)%(3*QUANTUM))*self.channels;
   if muted{self.pcm[index..index+self.channels].fill(0.0);}else{self.pcm[index..index+self.channels].copy_from_slice(frame);}
  }
  let merges=self.span_len!=0&&{let last=self.spans[self.span_len-1];last.stamp.same_metadata(stamp)&&last.stamp.start+last.frames as u64==stamp.start};
  if merges{self.spans[self.span_len-1].frames+=frames;}else{self.spans[self.span_len]=Span{stamp,frames};self.span_len+=1;}
  self.len+=frames;
 }
 pub(crate) fn detector(&mut self,frames:usize)->&[f32]{
  self.max_frames=self.max_frames.max(frames);
  let len=frames*self.channels;self.scratch[..len].fill(0.0);
  if self.len>=frames{
   for i in 0..frames{let at=((self.head+i)%(3*QUANTUM))*self.channels;self.scratch[i*self.channels..(i+1)*self.channels].copy_from_slice(&self.pcm[at..at+self.channels]);}
   self.discard(frames);
  }
  &self.scratch[..len]
 }
}

impl AudioPlan{
    pub(crate) fn retain_camera_eligibility(&mut self,policy:&crate::camera_activation::Snapshot){
        self.sources.retain(|source|policy.enabled(source.source));
        let mut reachable=[false;MAX_AUDIO_BLOCK_SOURCES];
        for (index,key) in self.auxiliary.iter().enumerate(){
            reachable[index]=policy.enabled(key.source)&&self.sources.iter().any(|source|source.filters.iter()
                .any(|filter|matches!(filter,AudioFilter::ObsCompressor{enabled:true,..})&&filter.sidechain_source()==Some(key.source)));
        }
        for _ in 0..self.auxiliary.len(){let mut changed=false;
            for (index,key) in self.auxiliary.iter().enumerate(){if reachable[index]||!policy.enabled(key.source){continue;}
                if self.auxiliary.iter().enumerate().any(|(owner,source)|reachable[owner]&&source.filters.iter().any(|filter|
                    matches!(filter,AudioFilter::ObsCompressor{enabled:true,..})&&filter.sidechain_source()==Some(key.source))){
                    reachable[index]=true;changed=true;
                }
            }if !changed{break;}
        }
        let mut index=0;self.auxiliary.retain(|_|{let keep=reachable[index];index+=1;keep});
    }
}

/// Audio-only auxiliary activation from explicit enabled authored keys. The
/// supplied root may be a frozen Program source; children resolve weak IDs in
/// the current document. Missing keys stay silent and never retain orphans.
fn key_sources(model:&StudioSnapshot,root:&Source,policy:Option<&crate::camera_activation::Snapshot>)->Result<Vec<Id>,AudioError>{
    if policy.is_some_and(|p|!p.enabled(root.id())){return Ok(Vec::new());}
    let mut found=Vec::new();let mut pending=Vec::new();
    fn keys(source:&Source,model:&StudioSnapshot,out:&mut Vec<Id>){
        out.extend(source.audio_filters().iter().filter(|filter|model.preferences().plugin_enabled(filter.plugin_id()))
            .filter(|filter|matches!(filter,AudioFilter::ObsCompressor{enabled:true,..})).filter_map(|filter|filter.sidechain_source()));
    }
    keys(root,model,&mut pending);
    while let Some(id)=pending.pop(){
        if found.contains(&id)||policy.is_some_and(|p|!p.enabled(id)){continue;}
        let Some(source)=model.source(id).filter(|source|source.kind().has_audio())else{continue;};
        if found.len()==MAX_AUDIO_BLOCK_SOURCES{return Err(AudioError::Limit("sidechain auxiliary graph exceeds128 sources"));}
        found.push(id);keys(source,model,&mut pending);
    }
    Ok(found)
}

pub(crate) fn view_keys(model:&StudioSnapshot,views:&[FrameScene],policy:Option<&crate::camera_activation::Snapshot>)->Result<Vec<Id>,AudioError>{
 let mut keys=Vec::new();
 let mut add=|root:&Source|->Result<(),AudioError>{
  for id in key_sources(model,root,policy)?{
   if !keys.contains(&id){if keys.len()==MAX_AUDIO_BLOCK_SOURCES{return Err(AudioError::Limit("audio key view union exceeds128 sources"));}keys.push(id);}
  }Ok(())
 };
 for view in views{for root in view.visible_sources(model).map_err(AudioError::Transition)?{add(root)?;}}
 for root in model.global_audio(){if let Some(source)=model.source(root.source()){add(source)?;}}
 Ok(keys)
}
