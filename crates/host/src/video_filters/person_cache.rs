//! Serial-worker-owned camera probability masks. No camera frame is retained.
use super::*;
const MAX_LANES:usize=64;
const MASK_VALUES:usize=65_536;
// Allow bounded allocator over-allocation and keep the actual capacity charged.
const MAX_MASK_CAPACITY:usize=MASK_VALUES*2;
const MASK_METADATA:usize=std::mem::size_of::<Mask>()+256;

#[derive(Clone,Hash,PartialEq,Eq)]
struct Key{scope:TemporalContext,id:u64,proof:Option<temporal::PersonInputProof>,width:u32,height:u32,blend:SourceBlend,interval_ms:u16,resource:u64,mask_epoch:u64,scroll:crate::scroll_animation::ScrollKey,backend:[u64;3]}
impl Key{fn same_owner(&self,other:&Self)->bool{let stage=match (self.proof,other.proof){(Some(a),Some(b))=>a.window==b.window&&a.row==b.row&&a.canonical==b.canonical,(None,None)=>self.id==other.id,_=>false};stage&&self.scope.domain==other.scope.domain&&self.scope.session==other.scope.session&&self.scope.source==other.scope.source&&self.scope.owner==other.scope.owner&&self.scope.grant==other.scope.grant}}
pub(in crate::video_filters) struct Mask{pub probabilities:Vec<f32>,_lease:Lease}
struct Lane{key:Key,at:u64,sequence:u64,mask:Arc<Mask>}
pub(in crate::video_filters) struct Store{shared:std::sync::Weak<Shared>,lanes:Vec<Lane>,table:Option<Lease>}
pub(in crate::video_filters) enum Lookup{Hit(Arc<Mask>),Miss(Lease)}
impl Store{
    pub(super) fn new(shared:&Arc<Shared>)->Self{Self{shared:Arc::downgrade(shared),lanes:Vec::new(),table:None}}
    fn shared(&self)->Result<Arc<Shared>,FilterError>{self.shared.upgrade().ok_or_else(||unavailable("person mask owner retired"))}
    fn key(context:TemporalContext,id:u64,proof:Option<temporal::PersonInputProof>,interval_ms:u16,dimensions:(u32,u32),blend:SourceBlend,resource:u64,mask_epoch:u64,scroll:crate::scroll_animation::ScrollKey,backend:[u64;3])->Key{
        let mut scope=context;scope.at=0;scope.tick=0;scope.stamp.capture_epoch=0;scope.stamp.sequence=0;scope.stamp.at=0;
        if proof.is_some(){scope.config=[0;32];scope.rows=Default::default();}
        Key{scope,id:if proof.is_some(){0}else{id},proof,width:dimensions.0,height:dimensions.1,blend,interval_ms,resource,mask_epoch,scroll,backend}
    }
    fn current(shared:&Shared,key:&Key)->bool{match key.proof{Some(proof)=>temporal::History::current_person_input(shared,key.scope,proof),None=>temporal::History::current_person(shared,key.scope)}}
    pub(in crate::video_filters) fn lookup(&mut self,context:TemporalContext,id:u64,interval_ms:u16,dimensions:(u32,u32),blend:SourceBlend,resource:u64,mask_epoch:u64,scroll:crate::scroll_animation::ScrollKey,backend:[u64;3])->Result<Lookup,FilterError>{
        let shared=self.shared()?;
        if !temporal::History::current_person(&shared,context){return Err(temporal::stale("Person mask camera scope changed"));}
        self.lanes.retain(|lane|Self::current(&shared,&lane.key));
        let proof=temporal::History::person_input_proof(&shared,context,id);
        let key=Self::key(context,id,proof,interval_ms,dimensions,blend,resource,mask_epoch,scroll,backend);
        if let Some(lane)=self.lanes.iter().find(|lane|lane.key==key){
            if context.at>=lane.at&&context.stamp.sequence>=lane.sequence&&context.at-lane.at<u64::from(interval_ms)*1_000_000{return Ok(Lookup::Hit(lane.mask.clone()));}
        }
        if self.lanes.len()>=MAX_LANES&&!self.lanes.iter().any(|lane|lane.key.same_owner(&key)){return Err(limited("person mask source capacity"));}
        if self.lanes.len()==self.lanes.capacity()&&self.lanes.len()<MAX_LANES&&!self.lanes.iter().any(|lane|lane.key.same_owner(&key)){
            let desired=self.lanes.len().saturating_add(1).max(self.lanes.capacity().max(1).saturating_mul(2)).min(MAX_LANES);
            let ceiling=desired.saturating_mul(2).min(MAX_LANES*2);
            let bytes=ceiling.checked_mul(std::mem::size_of::<Lane>()).and_then(|n|n.checked_add(1024)).ok_or_else(||limited("person mask table overflow"))?;
            // The existing table lease remains alive until the replacement
            // capacity is allocated and its actual size has been admitted.
            let mut replacement=Lease::acquire(&shared.budget,bytes).ok_or_else(||limited("person mask table growth admission"))?;
            self.lanes.try_reserve_exact(desired-self.lanes.len()).map_err(|_|limited("person mask table allocation"))?;
            let actual=self.lanes.capacity().checked_mul(std::mem::size_of::<Lane>()).and_then(|n|n.checked_add(1024));
            let Some(actual)=actual.filter(|n|*n<=bytes)else{self.lanes=Vec::new();self.table=None;return Err(limited("person mask table capacity exceeds admission"));};
            replacement.shrink(actual);self.table=Some(replacement);
        }
        let bytes=MAX_MASK_CAPACITY.checked_mul(std::mem::size_of::<f32>()).and_then(|n|n.checked_add(MASK_METADATA)).ok_or_else(||limited("person mask allocation overflow"))?;
        let lease=Lease::acquire(&shared.budget,bytes).ok_or_else(||limited("person mask replacement exceeds shared128MiB budget"))?;
        Ok(Lookup::Miss(lease))
    }
    pub(in crate::video_filters) fn publish(&mut self,context:TemporalContext,id:u64,interval_ms:u16,dimensions:(u32,u32),blend:SourceBlend,resource:u64,mask_epoch:u64,scroll:crate::scroll_animation::ScrollKey,backend:[u64;3],probabilities:Vec<f32>,mut lease:Lease)->Result<Arc<Mask>,FilterError>{
        let shared=self.shared()?;
        if !temporal::History::current_person(&shared,context){return Err(temporal::stale("Person mask scope changed during inference"));}
        if probabilities.len()!=MASK_VALUES||probabilities.capacity()>MAX_MASK_CAPACITY{return Err(fail("Person inference produced an unexpected mask size or allocation"));}
        let bytes=probabilities.capacity().checked_mul(std::mem::size_of::<f32>()).and_then(|n|n.checked_add(MASK_METADATA)).ok_or_else(||limited("person mask allocation overflow"))?;
        lease.shrink(bytes);
        let proof=temporal::History::person_input_proof(&shared,context,id);
        let key=Self::key(context,id,proof,interval_ms,dimensions,blend,resource,mask_epoch,scroll,backend);
        if self.lanes.iter().any(|lane|lane.key==key&&(context.at<lane.at||context.stamp.sequence<lane.sequence)){return Err(temporal::stale("Person mask inference time regressed"));}
        let mask=Arc::new(Mask{probabilities,_lease:lease});
        if let Some(lane)=self.lanes.iter_mut().find(|lane|lane.key==key){*lane=Lane{key,at:context.at,sequence:context.stamp.sequence,mask:mask.clone()};}
        else{self.lanes.retain(|lane|!lane.key.same_owner(&key));self.lanes.push(Lane{key,at:context.at,sequence:context.stamp.sequence,mask:mask.clone()});}
        Ok(mask)
    }
    pub(super) fn reconcile(&mut self){if let Some(shared)=self.shared.upgrade(){self.lanes.retain(|lane|Self::current(&shared,&lane.key));}else{self.lanes.clear();}if self.lanes.is_empty(){self.lanes=Vec::new();self.table=None;}}
}
