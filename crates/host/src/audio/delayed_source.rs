//! Lane-owned ordered packet stages; no mutable converter in a DSP snapshot.
use super::{AudioError, AudioFilter};
use super::noise_suppression::{self as noise, ProjectRnnoise, Scope, InputStamp, Service};
use skylive_core::{AudioSettings, MAX_AUDIO_FILTERS};
use std::{mem::size_of, ops::Range};

const MAX_SEGMENTS: usize = MAX_AUDIO_FILTERS + 1;
const MAX_PACKET: usize = 1024;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct ConfigUse {
    pub begin: u8,
    pub end: u8,
    pub config: u64,
    pub revision: u64,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct Lineage {
    pub uses: [ConfigUse; MAX_SEGMENTS],
    pub len: u8,
}

impl Lineage {
    fn record(&mut self, segment: usize, range: Range<usize>, config: u64, revision: u64) {
        self.uses[segment] = ConfigUse {begin: range.start as u8, end: range.end as u8, config, revision};
        self.len = self.len.max(segment as u8 + 1);
    }
    pub(crate) fn references(&self, config: u64) -> bool {
        self.uses[..usize::from(self.len)].iter().any(|row| row.config == config)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Receipt {
    pub origin: InputStamp,
    pub frames: usize,
    pub lineage: Lineage,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Ready {
    pub receipt: Receipt,
    pub start: i128,
}

/// Original packet header survives clipping/retirement of the readable span.
/// This is a derived packet identity, never a native capture generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PacketProvenance {
    pub packet: u64,
    pub input_start: i128,
    pub input_frames: u32,
    pub delay_frames: u64,
}

struct Node {
    index: usize,
    engine: ProjectRnnoise,
    queued: usize,
    ended: bool,
    drained: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Requirements {
    pub bytes: usize,
    pub receipts: usize,
    pub stages: usize,
    pub lookahead_frames: u64,
}

pub(crate) fn positions(filters: &[AudioFilter]) -> impl Iterator<Item=usize> + '_ {
    filters.iter().enumerate().filter_map(|(index, filter)|
        matches!(filter, AudioFilter::ObsNoiseSuppression {enabled:true}).then_some(index))
}

pub(crate) fn requirements(settings: AudioSettings, filters: &[AudioFilter]) -> Result<Requirements, AudioError> {
    if filters.len() > MAX_AUDIO_FILTERS { return Err(AudioError::Limit("source filter count")); }
    let stages = positions(filters).count();
    if stages == 0 { return Ok(Requirements {bytes:0, receipts:0, stages:0, lookahead_frames:0}); }
    let actor = noise::requirements(settings).map_err(AudioError::NoiseSuppression)?;
    // Every pending packet has at least one real frame in some serial stage.
    // Include one ready packet/transfer per stage and the final publication slot.
    let receipts = actor.pending_project_frames.checked_add(1).and_then(|n| n.checked_mul(stages))
        .and_then(|n| n.checked_add(1)).ok_or(AudioError::Limit("RNNoise receipt count"))?;
    let samples = MAX_PACKET.checked_mul(usize::from(settings.channels())).and_then(|n|n.checked_mul(2*size_of::<f32>()))
        .ok_or(AudioError::Limit("RNNoise source sample buffers"))?;
    let bytes = actor.retained_bytes.checked_mul(stages)
        .and_then(|n|n.checked_add(receipts*size_of::<Option<Receipt>>()))
        .and_then(|n|n.checked_add(samples+stages*size_of::<Node>()+filters.len()*size_of::<AudioFilter>()+size_of::<SourceChain>()+256))
        .ok_or(AudioError::Limit("RNNoise source actor bytes"))?;
    let lookahead_frames = u64::try_from((actor.processing_lookahead_frames as usize).checked_mul(stages)
        .ok_or(AudioError::Limit("RNNoise lookahead"))?).map_err(|_|AudioError::TimeOverflow)?;
    Ok(Requirements {bytes, receipts, stages, lookahead_frames})
}

pub(crate) struct SourceChain {
    settings: AudioSettings,
    scope: Scope,
    filters: Vec<AudioFilter>,
    nodes: Vec<Node>,
    receipts: Vec<Option<Receipt>>,
    pending: usize,
    serial: u64,
    fair: usize,
    transfer: Vec<f32>,
    final_samples: Vec<f32>,
    ready: Option<Ready>,
    ended: bool,
    poisoned: bool,
    held_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Progress { Advanced, Ready, Waiting, WorkLimit, Drained }

#[derive(Clone, Copy, Debug)]
pub(crate) struct Step { pub progress: Progress, pub work: usize }

impl SourceChain {
    pub(crate) fn new(settings: AudioSettings, filters: &[AudioFilter], scope: Scope, byte_credit: usize) -> Result<Self, AudioError> {
        let needed = requirements(settings, filters)?;
        if needed.stages == 0 || needed.bytes > byte_credit { return Err(AudioError::Limit("RNNoise source actor admission")); }
        let actor = noise::requirements(settings).map_err(AudioError::NoiseSuppression)?;
        let mut nodes=Vec::new(); nodes.try_reserve_exact(needed.stages).map_err(|_|AudioError::Limit("RNNoise node allocation"))?;
        for index in positions(filters) {
            nodes.push(Node {index, engine:ProjectRnnoise::new(settings, scope,
                noise::Limits {retained_bytes:actor.retained_bytes,scratch_bytes:actor.scratch_bytes})
                .map_err(AudioError::NoiseSuppression)?, queued:0, ended:false, drained:false});
        }
        let mut receipts=Vec::new();receipts.try_reserve_exact(needed.receipts).map_err(|_|AudioError::Limit("RNNoise receipt allocation"))?;
        receipts.resize(needed.receipts,None);
        let mut shape=Vec::new();shape.try_reserve_exact(filters.len()).map_err(|_|AudioError::Limit("RNNoise filter shape allocation"))?;
        shape.extend_from_slice(filters);
        let count=MAX_PACKET*usize::from(settings.channels());
        Ok(Self {settings,scope,filters:shape,nodes,receipts,pending:0,serial:0,fair:0,
            transfer:zeros(count)?,final_samples:zeros(count)?,ready:None,ended:false,poisoned:false,held_bytes:needed.bytes})
    }

    pub(crate) fn matches(&self, filters:&[AudioFilter], scope:Scope) -> bool {
        self.scope==scope && self.filters.len()==filters.len() && !super::dsp::reordered(&self.filters,filters)
            && self.filters.iter().zip(filters).all(|(a,b)|std::mem::discriminant(a)==std::mem::discriminant(b))
            && self.nodes.iter().map(|node|node.index).eq(positions(filters))
    }
    pub(crate) fn update_shape(&mut self, filters:&[AudioFilter]) {self.filters.copy_from_slice(filters);}
    pub(crate) fn bytes(&self)->usize{self.held_bytes}
    pub(crate) fn pending(&self)->bool{self.pending!=0||self.ready.is_some()||!self.nodes.iter().all(|node|node.drained)}
    pub(crate) fn input_ended(&self)->bool{self.ended}
    pub(crate) fn references(&self,config:u64)->bool{
        self.receipts.iter().flatten().any(|receipt|receipt.origin.config==config||receipt.lineage.references(config))
    }
    pub(crate) fn peek(&self)->Option<Ready>{self.ready}
    pub(crate) fn ready_samples(&self)->&[f32]{
        let count=self.ready.map_or(0,|ready|ready.receipt.frames*usize::from(self.settings.channels()));
        &self.final_samples[..count]
    }
    pub(crate) fn acknowledge(&mut self)->Result<(),AudioError>{
        let ready=self.ready.take().ok_or(AudioError::InvalidRequest("RNNoise final packet missing"))?;
        self.retire_packet(ready.receipt.origin.packet)?;Ok(())
    }
    fn retire_packet(&mut self,packet:u64)->Result<(),AudioError>{
        let index=(packet%self.receipts.len() as u64)as usize;
        let receipt=self.receipts[index].ok_or(AudioError::InvalidRequest("RNNoise receipt missing"))?;
        if receipt.origin.packet!=packet{return Err(AudioError::InvalidRequest("RNNoise receipt collision"));}
        self.receipts[index]=None;self.pending-=1;Ok(())
    }

    pub(crate) fn admit_input(&self,mut stamp:InputStamp,frames:usize)->Result<(),AudioError>{
        if self.poisoned||self.ended{return Err(AudioError::InvalidRequest("RNNoise source actor ended or poisoned"));}
        if self.ready.is_some()||self.pending>=self.receipts.len(){return Err(AudioError::NoiseSuppression(noise::Error::Backpressure));}
        stamp.packet=self.serial.checked_add(1).ok_or(AudioError::Limit("RNNoise packet identity exhausted"))?;
        let index=(stamp.packet%self.receipts.len()as u64)as usize;
        if self.receipts[index].is_some(){return Err(AudioError::NoiseSuppression(noise::Error::Backpressure));}
        self.nodes[0].engine.admit_packet(stamp,frames).map_err(AudioError::NoiseSuppression)
    }

    /// Caller holds a point-state candidate until this real packet is admitted.
    pub(crate) fn push(&mut self,mut stamp:InputStamp,samples:&[f32],
        point:&mut impl FnMut(usize,Range<usize>,&mut[f32])->Result<(),AudioError>)->Result<(),AudioError>{
        let channels=usize::from(self.settings.channels());
        if samples.is_empty()||samples.len()%channels!=0||samples.len()/channels>MAX_PACKET{return Err(AudioError::InvalidRequest("RNNoise source packet shape"));}
        let frames=samples.len()/channels;self.admit_input(stamp,frames)?;
        stamp.packet=self.serial+1;
        let range=0..self.nodes[0].index;
        self.transfer[..samples.len()].copy_from_slice(samples);
        if let Err(error)=point(0,range.clone(),&mut self.transfer[..samples.len()]){self.poisoned=true;return Err(error);}
        if let Some(index)=self.transfer[..samples.len()].iter().position(|value|!value.is_finite()){
            self.poisoned=true;return Err(AudioError::NonFiniteSample{source:self.scope.source,index});
        }
        if let Err(error)=self.nodes[0].engine.push(stamp,&self.transfer[..samples.len()]){self.poisoned=true;return Err(AudioError::NoiseSuppression(error));}
        let mut lineage=Lineage::default();lineage.record(0,range,stamp.config,stamp.revision);
        let index=(stamp.packet%self.receipts.len()as u64)as usize;
        self.receipts[index]=Some(Receipt {origin:stamp,frames,lineage});
        self.serial=stamp.packet;self.pending+=1;self.nodes[0].queued+=1;Ok(())
    }

    /// Bounded serial-stage service. Model/converter actors own acceptance;
    /// the caller owns a transactional point-kernel candidate for each step.
    pub(crate) fn service(&mut self,config:u64,revision:u64,mut credit:usize,point_work_per_frame:usize,
        point:&mut impl FnMut(usize,Range<usize>,&mut[f32])->Result<(),AudioError>)->Result<Step,AudioError>{
        if self.poisoned{return Err(AudioError::InvalidRequest("RNNoise source actor poisoned"));}
        if self.ready.is_some(){return Ok(Step{progress:Progress::Ready,work:0});}
        let mut work=0usize;
        for _ in 0..self.nodes.len(){
            let index=self.fair%self.nodes.len();self.fair=(index+1)%self.nodes.len();
            if self.nodes[index].drained{continue;}
            let upstream_ended=if index==0{self.ended}else{self.nodes[index-1].drained};
            if upstream_ended&&!self.nodes[index].ended{
                self.nodes[index].engine.end_input().map_err(AudioError::NoiseSuppression)?;
                self.nodes[index].ended=true;
            }
            if let Some(output)=self.nodes[index].engine.peek(){
                let next=index+1;
                let mut stamp=output.input;stamp.start=output.start;
                if next<self.nodes.len(){
                    match self.nodes[next].engine.admit_packet(stamp,output.frames){
                        Ok(())=>{},Err(noise::Error::Backpressure)=>continue,
                        Err(error)=>{self.poisoned=true;return Err(AudioError::NoiseSuppression(error));}
                    }
                }
                let cost=output.frames.checked_mul(point_work_per_frame).ok_or(AudioError::Limit("RNNoise point work"))?;
                if cost>credit{return Ok(Step{progress:Progress::WorkLimit,work});}
                let count=output.frames*usize::from(self.settings.channels());
                let popped=self.nodes[index].engine.pop(&mut self.transfer[..count]).map_err(AudioError::NoiseSuppression)?;
                if popped.is_none(){self.poisoned=true;return Err(AudioError::InvalidRequest("RNNoise ready packet vanished"));}
                self.nodes[index].queued-=1;
                let range=self.nodes[index].index+1..self.nodes.get(next).map_or(self.filters.len(),|node|node.index);
                if let Err(error)=point(next,range.clone(),&mut self.transfer[..count]){self.poisoned=true;return Err(error);}
                if let Some(index)=self.transfer[..count].iter().position(|value|!value.is_finite()){
                    self.poisoned=true;return Err(AudioError::NonFiniteSample{source:self.scope.source,index});
                }
                let slot=(stamp.packet%self.receipts.len()as u64)as usize;
                let receipt=self.receipts[slot].as_mut().ok_or(AudioError::InvalidRequest("RNNoise original receipt missing"))?;
                if receipt.origin.packet!=stamp.packet{self.poisoned=true;return Err(AudioError::InvalidRequest("RNNoise original receipt collision"));}
                receipt.lineage.record(next,range,config,revision);
                if next<self.nodes.len(){
                    if let Err(error)=self.nodes[next].engine.push(stamp,&self.transfer[..count]){self.poisoned=true;return Err(AudioError::NoiseSuppression(error));}
                    self.nodes[next].queued+=1;
                }else{
                    self.final_samples[..count].copy_from_slice(&self.transfer[..count]);
                    self.ready=Some(Ready {receipt:*receipt,start:output.start});
                }
                return Ok(Step{progress:if self.ready.is_some(){Progress::Ready}else{Progress::Advanced},work:work+cost});
            }
            if self.nodes[index].queued==0&&!self.nodes[index].ended{continue;}
            let result=self.nodes[index].engine.service(credit);
            let cost=self.nodes[index].engine.service_cost().work_units;
            work=work.checked_add(cost).ok_or(AudioError::Limit("RNNoise service work"))?;
            credit=credit.checked_sub(cost).ok_or(AudioError::Limit("RNNoise service credit"))?;
            match result.map_err(|error|{self.poisoned=true;AudioError::NoiseSuppression(error)})?{
                Service::Progress|Service::OutputReady=>return Ok(Step{progress:Progress::Advanced,work}),
                Service::WorkLimit=>return Ok(Step{progress:Progress::WorkLimit,work}),
                Service::NeedInput=>{},
                Service::Drained=>{
                    // These are actual original packets that cannot be emitted
                    // at EOF; retire their exact receipts before clearing FIFO.
                    let (nodes,receipts,pending)=(&self.nodes,&mut self.receipts,&mut self.pending);
                    for packet in nodes[index].engine.pending_packets(){
                        let slot=(packet%receipts.len()as u64)as usize;
                        let receipt=receipts[slot].ok_or(AudioError::InvalidRequest("RNNoise EOF receipt missing"))?;
                        if receipt.origin.packet!=packet{return Err(AudioError::InvalidRequest("RNNoise EOF receipt collision"));}
                        receipts[slot]=None;*pending-=1;
                    }
                    self.nodes[index].engine.finish_discard().map_err(AudioError::NoiseSuppression)?;
                    self.nodes[index].queued=0;self.nodes[index].drained=true;
                    return Ok(Step{progress:Progress::Advanced,work});
                }
            }
        }
        Ok(Step{progress:if self.nodes.iter().all(|node|node.drained){Progress::Drained}else{Progress::Waiting},work})
    }

    pub(crate) fn end_input(&mut self){self.ended=true;}
}

fn zeros(count:usize)->Result<Vec<f32>,AudioError>{
    let mut result=Vec::new();result.try_reserve_exact(count).map_err(|_|AudioError::Limit("RNNoise packet scratch allocation"))?;
    result.resize(count,0.);Ok(result)
}
