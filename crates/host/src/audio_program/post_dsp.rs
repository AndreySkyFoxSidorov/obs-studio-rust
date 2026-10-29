//! Source-owned, pre-gain processed PCM. Output/monitor reads cannot execute
//! filters or change their history. Native groups remain shared/unshifted.
use super::*;
use crate::audio::{AudioPlan, AudioSourcePlan,craft_filter_bytes,MAX_CRAFT_RETAINED};
use crate::audio::sidechain::{self,SignalStamp};
use crate::audio::delayed_source::{self as delayed_source,SourceChain,Lineage};
mod delayed;
const MAX_METADATA:usize=2*1024*1024;
const MAX_CONFIGS:usize=4096;
const MAX_RANGES:usize=8192;
const MAX_ACTIONS:usize=128*crate::audio::MAX_AUDIO_INPUTS;
/// Two source quanta per maximum64-source route per owner service call.
const PROCESS_BLOCKS:usize=2*crate::audio::MAX_AUDIO_INPUTS;
#[derive(Clone)]
struct Config {
    requested:u64,
    effective:u64,
    serial:u64,
    revision:u64,
    plan:AudioPlan,
    bytes:usize,
}
#[derive(Clone,Copy)]
struct Produced {
    input_packet:Option<delayed_source::PacketProvenance>,lineage:Lineage,start:u64,end:u64,config:u64,revision:u64,native_epoch:u64,generation:u64,processing_epoch:u64,original:NativeAudioFormat,
}
pub(super) struct Lane {
    session:u64,source:Id,binding:AudioInputKey,settings:AudioSettings,source_epoch:u64,camera_cut:bool,output_end:Option<u64>,
    signal_scope:u64,detector_only:bool,active:bool,stop:Option<u64>,next:u64,native_epoch:Option<u64>,processing_epoch:u64,retired_before:u64,
    ring:SampleRing,configs:Vec<Config>,ranges:Vec<Produced>,dsp:AudioDspState,key_bytes:usize,
    neural:Option<SourceChain>,neural_original:Option<NativeAudioFormat>,neural_floor:u64,neural_levels:Option<SourceLevels>,neural_eof:bool,
}
#[derive(Clone,Copy)]
struct SourceLevels{processing_epoch:u64,peak:[f64;8],rms:[f64;8]}
fn source_levels(samples:&[f32],channels:usize,processing_epoch:u64)->SourceLevels{
    let mut peak=[0_f64;8];let mut squares=[0_f64;8];
    let frames=samples.len()/channels;
    for frame in samples.chunks_exact(channels){for(channel,&value)in frame.iter().enumerate(){
        let value=f64::from(value);peak[channel]=peak[channel].max(value.abs());squares[channel]+=value*value;
    }}
    let mut rms=[0_f64;8];if frames!=0{for channel in 0..channels{rms[channel]=(squares[channel]/frames as f64).sqrt();}}
    SourceLevels{processing_epoch,peak,rms}
}
#[derive(Clone,Copy)]
struct VolumeAction {frame:u64,gain:f32,muted:bool,revision:u64}
struct SourceActions {source:Id,events:Vec<VolumeAction>}
#[derive(Default)]
pub(super) struct Histories {lanes:Vec<Lane>,serial:u64,fair:usize,actions:Vec<SourceActions>,sidechain_reserved:bool,craft_reserved:usize,neural_reserved:usize}
struct Update {index:usize,detector_only:bool,ring:Option<SampleRing>,configs:Option<Vec<Config>>,activate:u64}
pub(super) struct Prepared {updates:Vec<Update>,new:Vec<Lane>,serial:u64,at:u64,keys:Vec<(Id,AudioInputKey,u64,u64)>,actions:Vec<SourceActions>,sidechain_reserved:bool,craft_reserved:usize,neural_reserved:usize}
struct Count(usize);
impl std::io::Write for Count {
    fn write(&mut self,buf:&[u8])->std::io::Result<usize>{
        self.0=self.0.checked_add(buf.len()).filter(|v|*v<=MAX_METADATA)
            .ok_or_else(||std::io::Error::other("processed source metadata limit"))?;Ok(buf.len())
    }
    fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
fn encoded(value:&impl serde::Serialize)->Result<usize,ProgramAudioError>{
    let mut count=Count(0);serde_json::to_writer(&mut count,value)
        .map_err(|_|ProgramAudioError::Limit("processed source metadata"))?;Ok(count.0)
}
fn config_bytes(source:&AudioSourcePlan)->Result<usize,ProgramAudioError>{
    encoded(source)?.checked_add(2048+source.filters.len()*std::mem::size_of::<skylive_core::AudioFilter>())
        .ok_or(ProgramAudioError::Limit("processed source config bytes"))
}
fn same_processing(plan:&AudioPlan,source:&AudioSourcePlan)->bool {
    plan.sources().first().is_some_and(|old|old.source==source.source&&old.binding==source.binding
        &&old.properties.force_mono()==source.properties.force_mono()&&old.properties.balance()==source.properties.balance()
        &&old.filters==source.filters)
}
impl Histories {
    /// Admit a bounded additional recording reader before publishing its
    /// lease. Grow all existing post-DSP rings transactionally: old and new
    /// allocations remain charged during preparation, so an allocation error
    /// leaves every original lane intact.
    pub(super) fn reserve_recording_reader(&mut self, hold:usize, sync:&sync_offset::SourceSync,
        camera:&[(Id,usize)], limits:ProgramAudioLimits, resident:usize)->Result<(),ProgramAudioError> {
        let mut specs=Vec::new();
        specs.try_reserve_exact(self.lanes.len()).map_err(|_|ProgramAudioError::Limit("recording history metadata"))?;
        let mut candidate_bytes=0usize;
        for (index,lane) in self.lanes.iter().enumerate() {
            let base=(lane.settings.sample_rate() as usize).checked_mul(limits.max_buffer_ms)
                .ok_or(ProgramAudioError::Limit("recording history frames"))?/1000;
            let camera_frames=camera.iter().find_map(|(id,frames)|(*id==lane.source).then_some(*frames)).unwrap_or(0);
            let needed=base.max(1024).checked_add(sync.positive_source_frames(lane.source,&lane.binding,lane.source_epoch))
                .and_then(|value|value.checked_add(camera_frames)).and_then(|value|value.checked_add(hold))
                .ok_or(ProgramAudioError::Limit("recording history frames"))?;
            if needed>lane.ring.capacity() {
                candidate_bytes=candidate_bytes.checked_add(SampleRing::required_bytes(needed,usize::from(lane.settings.channels()))?)
                    .ok_or(ProgramAudioError::Limit("recording history bytes"))?;
                specs.push((index,needed));
            }
        }
        if resident.checked_add(candidate_bytes).is_none_or(|bytes|bytes>limits.max_bytes) {
            return Err(ProgramAudioError::Limit("recording history exceeds audio owner budget"));
        }
        let mut replacements=Vec::new();
        replacements.try_reserve_exact(specs.len()).map_err(|_|ProgramAudioError::Limit("recording history allocation"))?;
        for (index,needed) in specs {replacements.push((index,self.lanes[index].ring.grown(needed)?));}
        for (index,ring) in replacements {self.lanes[index].ring=ring;}
        Ok(())
    }
    pub(super) fn cut_push_sidechain(&mut self,source:Id,frame:u64){for lane in &mut self.lanes{lane.dsp.sidechain_cut(source,frame);}}
    /// Retire all processed history belonging to a window input before a new
    /// observed process is admitted. A subsequent route snapshot rebuilds the
    /// source lane; until then readers receive no retained samples from it.
    pub(super) fn invalidate_window(&mut self,source:Id,frame:u64){
        self.lanes.retain(|lane| !matches!(&lane.binding,AudioInputKey::Window {source:id,..} if *id==source));
        self.cut_push_sidechain(source,frame);
    }
    /// Drop old stream-owned NDI PCM before adopting a new sender epoch.
    pub(super) fn invalidate_ndi(&mut self,source:Id,frame:u64){
        self.lanes.retain(|lane| !matches!(&lane.binding,AudioInputKey::Ndi {source:id,..} if *id==source));
        self.cut_push_sidechain(source,frame);
    }
    pub(super) fn clear(&mut self){self.lanes.clear();self.actions.clear();self.serial=0;self.fair=0;self.sidechain_reserved=false;self.craft_reserved=0;self.neural_reserved=0;}
    pub(super) fn has_binding(&self,binding:&AudioInputKey)->bool{self.lanes.iter().any(|lane|&lane.binding==binding)}
    pub(super) fn has_source(&self,source:Id)->bool{self.lanes.iter().any(|lane|lane.source==source)}
    pub(super) fn has_source_scope(&self,source:Id,epoch:u64)->bool{self.lanes.iter().any(|lane|lane.source==source&&lane.source_epoch==epoch)}
    pub(super) fn ring_epoch(&self,source:Id,binding:&AudioInputKey,epoch:u64)->Option<&SampleRing>{
        self.lanes.iter().find(|lane|lane.source==source&&&lane.binding==binding&&lane.source_epoch==epoch).map(|lane|&lane.ring)
    }
    /// OBS audio actions are output-time, floor-to-frame and clamp against
    /// the emitted prefix; they are never baked into delayed source history.
    pub(super) fn gain_at(&self,source:Id,frame:u64)->Option<(f32,bool)>{
        let index=self.actions.binary_search_by_key(&source.get(),|row|row.source.get()).ok()?;
        let events=&self.actions[index].events;let end=events.partition_point(|event|event.frame<=frame);
        let event=events.get(end.checked_sub(1)?)?;Some((event.gain,event.muted))
    }
    pub(super) fn pending_before(&self,source:Id,binding:&AudioInputKey,epoch:u64,end:i128,
        groups:&HashMap<AudioInputKey,InputGroup>)->bool{
        self.lanes.iter().find(|lane|lane.source==source&&&lane.binding==binding&&lane.source_epoch==epoch).is_some_and(|lane|{
            if lane.ring.end()>=end||lane.neural_eof&&lane.neural.is_none(){return false;}
            lane.neural.as_ref().is_some_and(SourceChain::pending)
                ||i128::from(lane.next)<end&&groups.get(binding).is_some_and(|group|group.ring.end()>=end)
        })
    }
    pub(super) fn unprocessed(&self,source:Id,binding:&AudioInputKey,epoch:u64,groups:&HashMap<AudioInputKey,InputGroup>)->bool{
        self.lanes.iter().find(|lane|lane.source==source&&&lane.binding==binding&&lane.source_epoch==epoch).is_some_and(|lane|
            lane.neural.as_ref().is_some_and(SourceChain::pending)
                ||!lane.neural_eof&&(lane.active||lane.stop.is_some_and(|stop|lane.next<stop))&&groups.get(binding).is_some_and(|group|i128::from(lane.next)<group.ring.end()))
    }
    /// OBS source meters follow actual post-filter callbacks even when no
    /// Program sink reads that source. Never delay another track for a meter.
    pub(super) fn update_neural_meters(&self,meters:&mut[SourceMeter],push:&push_gate::Bank){
        for meter in meters{
            let Some(lane)=self.lanes.iter().find(|lane|lane.source==meter.source&&lane.active&&lane.neural_levels
                .is_some_and(|levels|levels.processing_epoch==lane.processing_epoch))else{continue;};
            let Some(levels)=lane.neural_levels else{continue;};
            let action=self.actions.iter().find(|row|row.source==lane.source).and_then(|row|row.events.last());
            let gain=if push.silent(lane.source,lane.next){0.}else{action.map_or(1_f64,|action|if action.muted{0.}else{f64::from(action.gain)})};
            for(channel,meter)in meter.channels.iter_mut().enumerate(){
                meter.peak=levels.peak[channel]*gain;meter.rms=levels.rms[channel]*gain;
            }
        }
    }
    pub(super) fn has_delayed_source(&self,source:Id,binding:&AudioInputKey,epoch:u64)->bool{
        self.lanes.iter().find(|lane|lane.source==source&&&lane.binding==binding&&lane.source_epoch==epoch).is_some_and(|lane|
            lane.neural.is_some()||lane.configs.last().is_some_and(|config|delayed_source::positions(&config.plan.sources()[0].filters).next().is_some()))
    }
    pub(super) fn processing_lookahead(&self,source:Id,binding:&AudioInputKey,epoch:u64)->u64{
        self.lanes.iter().find(|lane|lane.source==source&&&lane.binding==binding&&lane.source_epoch==epoch).map_or(0,|lane|
            lane.configs.iter().filter_map(|config|delayed_source::requirements(lane.settings,&config.plan.sources()[0].filters).ok())
                .map(|required|required.lookahead_frames).max().unwrap_or(0))
    }
    pub(super) fn raw_floor(&self,binding:&AudioInputKey)->Option<u64>{
        self.lanes.iter().filter(|lane|&lane.binding==binding&&!lane.neural_eof&&(lane.active||lane.stop.is_some_and(|stop|lane.next<stop))).map(|lane|lane.next).min()
    }
    fn metadata(&self)->usize {
        self.actions.capacity()*std::mem::size_of::<SourceActions>()+self.actions.iter().map(|row|row.events.capacity()*std::mem::size_of::<VolumeAction>()).sum::<usize>()
            +self.lanes.capacity()*std::mem::size_of::<Lane>()+self.lanes.iter().map(|lane|
            lane.key_bytes+lane.configs.iter().map(|config|config.bytes).sum::<usize>()
            +(lane.configs.capacity()-lane.configs.len())*std::mem::size_of::<Config>()
            +lane.ranges.capacity()*std::mem::size_of::<Produced>()).sum::<usize>()
    }
    pub(super) fn bytes(&self)->usize{
        // The complete2MiB receipt/config store is reserved before PCM history
        // admission, so later range production cannot exceed the128MiB owner.
        let metadata=if self.lanes.capacity()==0&&self.actions.capacity()==0{0}else{MAX_METADATA};
        metadata+self.lanes.iter().map(|lane|lane.ring.bytes()).sum::<usize>()+(if self.sidechain_reserved{sidechain::RESERVATION}else{0})+self.craft_reserved+self.neural_reserved
    }
    /// Prepare all descriptors and all old+new PCM allocations before route
    /// publication. Existing DSP/kernel state and samples are not cloned/reset.
    pub(super) fn prepare(&mut self,snapshot:&AudioRouteSnapshot,sync:&sync_offset::SourceSync,
        limits:ProgramAudioLimits,resident:usize,cursor:u64,gates:&camera_gate::Gates,groups:&HashMap<AudioInputKey,InputGroup>,recording_hold:usize)->Result<Prepared,ProgramAudioError>
    {
        snapshot.plan().processing_state_bytes()?;
        let rate=snapshot.settings().sample_rate();let channels=usize::from(snapshot.settings().channels());
        let at=u64::try_from((u128::from(snapshot.at().0)*u128::from(rate)).div_ceil(1_000_000_000)).map_err(|_|ProgramAudioError::TimeOverflow)?;
        let base=rate as usize*limits.max_buffer_ms/1000;
        let mut rows=Vec::new();let mut pcm=0usize;let mut extra=0usize;let mut retained_extra=0usize;let mut new_count=0usize;let mut config_count=0usize;
        for source in snapshot.processing_controls(){
            let index=self.lanes.iter().position(|lane|lane.source==source.source&&lane.binding==source.binding&&lane.source_epoch==gates.scope(source.source));
            if index.is_some_and(|index|self.lanes[index].session!=snapshot.session()||self.lanes[index].settings.sample_rate()!=rate||self.lanes[index].settings.channels()!=snapshot.settings().channels()){
                return Err(ProgramAudioError::Invalid("processed source history scope changed"));
            }
            let detector_only=snapshot.plan().detector_sources().iter().any(|key|key.source==source.source);
            let source_base=if detector_only{2*sidechain::QUANTUM}else{base.max(1024)};
            let delay_history=snapshot.camera_delay_history().iter().find_map(|(id,frames)|(*id==source.source).then_some(*frames)).unwrap_or(0);
            let frames=source_base.checked_add(sync.positive_source_frames(source.source,&source.binding,gates.scope(source.source)))
                .and_then(|frames|frames.checked_add(delay_history)).and_then(|frames|frames.checked_add(recording_hold))
                .ok_or(ProgramAudioError::Limit("processed source history frames"))?;
            let grow=index.is_none_or(|index|self.lanes[index].ring.capacity()<frames);
            let config=index.is_none_or(|index|self.lanes[index].configs.last().is_none_or(|old|!same_processing(&old.plan,source)));
            if grow{pcm=pcm.checked_add(SampleRing::required_bytes(frames,channels)?).ok_or(ProgramAudioError::Limit("processed source preparation bytes"))?;}
            if config{
                config_count+=1;
                retained_extra=retained_extra.checked_add(config_bytes(source)?).ok_or(ProgramAudioError::Limit("processed source retained metadata"))?;
                extra=extra.checked_add(config_bytes(source)?).ok_or(ProgramAudioError::Limit("processed source metadata"))?;
                // Replacing the config vector retains actual old configs, not
                // a retagged projection of already-produced old PCM.
                if let Some(index)=index{extra=extra.checked_add(self.lanes[index].configs.iter().map(|v|v.bytes).sum::<usize>()).ok_or(ProgramAudioError::Limit("processed source candidate metadata"))?;}
            }
            if index.is_none(){new_count+=1;let bytes=encoded(&source.binding)?+std::mem::size_of::<Lane>();
                retained_extra=retained_extra.checked_add(bytes).ok_or(ProgramAudioError::Limit("processed source lane metadata"))?;
                extra=extra.checked_add(bytes).ok_or(ProgramAudioError::Limit("processed source lane metadata"))?;}
            rows.push((index,source,frames,grow,config,detector_only));
        }
        // Reserve old retained queues plus one transactional candidate before
        // constructing kernels. This is part of the same128MiB owner ceiling.
        let mut queued=0usize;
        for (index,lane) in self.lanes.iter().enumerate(){
            let mut bytes=lane.configs.iter().map(|config|sidechain::chain_bytes(&config.plan.sources()[0].filters,lane.settings.channels())).max().unwrap_or(0);
            if let Some((_,source,_,_,_,_))=rows.iter().find(|(old,..)|*old==Some(index)){
                bytes=bytes.max(sidechain::chain_bytes(&source.filters,snapshot.settings().channels()));
            }
            queued=queued.checked_add(bytes).ok_or(ProgramAudioError::Limit("sidechain retained bytes"))?;
        }
        for (_,source,_,_,_,_) in rows.iter().filter(|(index,..)|index.is_none()){
            queued=queued.checked_add(sidechain::chain_bytes(&source.filters,snapshot.settings().channels())).ok_or(ProgramAudioError::Limit("sidechain retained bytes"))?;
        }
        if queued>sidechain::RETAINED_LIMIT{return Err(ProgramAudioError::Limit("sidechain queues exceed4MiB retained profile"));}
        let mut craft_held=0usize;let mut craft_largest=0usize;
        for(index,lane)in self.lanes.iter().enumerate(){
            let mut bytes=lane.dsp.craft_heap_bytes();
            for config in &lane.configs{bytes=bytes.max(config.plan.craft_state_bytes()?);}
            if let Some((_,source,_,_,_,_))=rows.iter().find(|(old,..)|*old==Some(index)){
                bytes=bytes.max(craft_filter_bytes(&source.filters,rate,snapshot.settings().channels())?);
            }
            craft_held=craft_held.checked_add(bytes).ok_or(ProgramAudioError::Limit("Craft retained state overflow"))?;craft_largest=craft_largest.max(bytes);
        }
        for(_,source,_,_,_,_)in rows.iter().filter(|(index,..)|index.is_none()){
            let bytes=craft_filter_bytes(&source.filters,rate,snapshot.settings().channels())?;
            craft_held=craft_held.checked_add(bytes).ok_or(ProgramAudioError::Limit("Craft retained state overflow"))?;craft_largest=craft_largest.max(bytes);
        }
        if craft_held>MAX_CRAFT_RETAINED{return Err(ProgramAudioError::Limit("Craft delay/reverb retained heap exceeds32MiB"));}
        // Includes all old lanes/configurations plus a source snapshot and a
        // possible complete replacement. COW sharing only reduces real use.
        let craft_reserved=craft_held.checked_add(craft_largest.checked_mul(2).ok_or(ProgramAudioError::Limit("Craft candidate peak"))?)
            .ok_or(ProgramAudioError::Limit("Craft candidate peak"))?;
        let resident=resident.checked_sub(self.craft_reserved).and_then(|n|n.checked_add(craft_reserved))
            .ok_or(ProgramAudioError::Limit("Craft owner credit accounting"))?;
        let mut neural_held=0usize;let mut neural_largest=0usize;
        for(index,lane)in self.lanes.iter().enumerate(){
            let mut bytes=lane.neural.as_ref().map_or(0,SourceChain::bytes);
            for config in &lane.configs{bytes=bytes.max(delayed_source::requirements(lane.settings,&config.plan.sources()[0].filters)?.bytes);}
            if let Some((_,source,_,_,_,_))=rows.iter().find(|(old,..)|*old==Some(index)){
                bytes=bytes.max(delayed_source::requirements(snapshot.settings(),&source.filters)?.bytes);
            }
            neural_held=neural_held.checked_add(bytes).ok_or(ProgramAudioError::Limit("RNNoise source retained bytes"))?;neural_largest=neural_largest.max(bytes);
        }
        for(_,source,_,_,_,_)in rows.iter().filter(|(index,..)|index.is_none()){
            let bytes=delayed_source::requirements(snapshot.settings(),&source.filters)?.bytes;
            neural_held=neural_held.checked_add(bytes).ok_or(ProgramAudioError::Limit("RNNoise source retained bytes"))?;neural_largest=neural_largest.max(bytes);
        }
        let neural_reserved=neural_held.checked_add(neural_largest.checked_mul(2).ok_or(ProgramAudioError::Limit("RNNoise replacement peak"))?)
            .ok_or(ProgramAudioError::Limit("RNNoise replacement peak"))?;
        let resident=resident.checked_sub(self.neural_reserved).and_then(|n|n.checked_add(neural_reserved))
            .ok_or(ProgramAudioError::Limit("RNNoise owner credit accounting"))?;
        let sidechain_reserved=self.sidechain_reserved||queued!=0;
        if sidechain_reserved&&!self.sidechain_reserved{pcm=pcm.checked_add(sidechain::RESERVATION).ok_or(ProgramAudioError::Limit("sidechain candidate reservation"))?;}
        if self.lanes.capacity()==0&&self.actions.capacity()==0&&new_count!=0 {
            pcm=pcm.checked_add(MAX_METADATA).ok_or(ProgramAudioError::Limit("processed source metadata reservation"))?;
        }
        let action_frame=u64::try_from(u128::from(snapshot.at().0)*u128::from(rate)/1_000_000_000)
            .map_err(|_|ProgramAudioError::TimeOverflow)?.max(cursor);
        let mut new_actions=0usize;let mut action_extra=0usize;let mut action_count=self.actions.iter().map(|row|row.events.len()).sum::<usize>();
        for source in snapshot.processing_controls(){
            let row=self.actions.iter().find(|row|row.source==source.source);
            if row.is_none(){new_actions+=1;}
            if row.is_none_or(|row|row.events.last().is_none_or(|last|last.gain!=source.gain||last.muted!=source.muted)){
                action_count+=1;
                action_extra=action_extra.checked_add(std::mem::size_of::<SourceActions>()+(row.map_or(0,|row|row.events.len())+1)*std::mem::size_of::<VolumeAction>())
                    .ok_or(ProgramAudioError::Limit("timestamped volume metadata"))?;
            }
        }
        if action_count>MAX_ACTIONS{return Err(ProgramAudioError::Backpressure("timestamped source volume actions"));}
        extra=extra.checked_add(action_extra).ok_or(ProgramAudioError::Limit("timestamped volume preparation"))?;
        retained_extra=retained_extra.checked_add(action_extra).ok_or(ProgramAudioError::Limit("timestamped volume retained metadata"))?;
        if self.lanes.len()+new_count>MAX_AUDIO_BLOCK_SOURCES||self.lanes.iter().map(|lane|lane.configs.len()).sum::<usize>()+config_count>MAX_CONFIGS{
            return Err(ProgramAudioError::Limit("processed source lane/config count"));
        }
        if self.metadata().checked_add(retained_extra).is_none_or(|n|n>MAX_METADATA)
            ||self.metadata().checked_add(extra).is_none_or(|n|n>2*MAX_METADATA)
            ||resident.checked_add(extra).and_then(|n|n.checked_add(pcm)).is_none_or(|n|n>limits.max_bytes){
            return Err(ProgramAudioError::Backpressure("combined raw/post-DSP old/new history admission"));
        }
        // Capacity preparation is owner-private accounting only. No source
        // cursor/config/DSP/PCM publication occurs until apply after timeline.
        self.actions.try_reserve_exact(new_actions).map_err(|_|ProgramAudioError::Limit("timestamped source row allocation"))?;
        self.lanes.try_reserve_exact(new_count).map_err(|_|ProgramAudioError::Limit("processed source lane allocation"))?;
        let mut serial=self.serial;let mut updates=Vec::new();let mut new=Vec::new();let mut keys=Vec::new();
        for(index,source,frames,grow,changed,detector_only)in rows{
            let source_epoch=gates.scope(source.source);
            let mut activation=at.max(gates.floor(source.source).0);
            if index.is_none()&&source_epoch!=0{
                // Each actual binding version must exclude data already
                // ingested before this fresh source history is published.
                if let Some(group)=groups.get(&source.binding){activation=activation.max(u64::try_from(group.ring.end().max(0)).unwrap_or(u64::MAX));}
            }
            let next=index.map_or(activation,|index|self.lanes[index].next);
            let configs=if changed{
                serial=serial.checked_add(1).ok_or(ProgramAudioError::Limit("processed source config serial"))?;
                let mut configs=if let Some(index)=index{self.lanes[index].configs.clone()}else{Vec::new()};
                configs.try_reserve_exact(1).map_err(|_|ProgramAudioError::Limit("processed source config allocation"))?;
                configs.push(Config {requested:at,effective:at.max(next),serial,revision:snapshot.revision(),
                    plan:snapshot.plan().processing_source_plan(source.source)?,bytes:config_bytes(source)?});Some(configs)
            }else{None};
            let signal_scope=index.map_or(serial,|index|self.lanes[index].signal_scope);
            keys.push((source.source,source.binding.clone(),source_epoch,signal_scope));
            if let Some(index)=index{
                let ring=if grow{Some(self.lanes[index].ring.grown(frames)?)}else{None};
                updates.push(Update {index,detector_only,ring,configs,activate:activation});
            }else{
                new.push(Lane {session:snapshot.session(),source:source.source,binding:source.binding.clone(),settings:snapshot.settings(),source_epoch,camera_cut:false,output_end:None,
                    signal_scope:serial,detector_only,active:true,stop:None,next:activation,native_epoch:None,processing_epoch:0,retired_before:0,ring:SampleRing::new(frames,channels)?,
                    configs:configs.ok_or(ProgramAudioError::Invalid("new source history has no config"))?,ranges:Vec::new(),
                    dsp:AudioDspState::default(),neural:None,neural_original:None,neural_floor:activation,neural_levels:None,neural_eof:false,key_bytes:encoded(&source.binding)?});
            }
        }
        let mut actions=Vec::new();
        for source in snapshot.processing_controls(){
            let row=self.actions.iter().find(|row|row.source==source.source);
            if row.is_none_or(|row|row.events.last().is_none_or(|last|last.gain!=source.gain||last.muted!=source.muted)){
                let mut events=row.map_or_else(Vec::new,|row|row.events.clone());
                if events.last().is_some_and(|last|action_frame<last.frame){return Err(ProgramAudioError::ClockRewind);}
                events.push(VolumeAction {frame:action_frame,gain:source.gain,muted:source.muted,revision:snapshot.revision()});
                actions.push(SourceActions {source:source.source,events});
            }
        }
        Ok(Prepared {updates,new,serial,at,keys,actions,sidechain_reserved,craft_reserved,neural_reserved})
    }
    pub(super) fn apply(&mut self,prepared:Prepared){
        self.sidechain_reserved=prepared.sidechain_reserved;self.craft_reserved=prepared.craft_reserved;self.neural_reserved=prepared.neural_reserved;
        for lane in &mut self.lanes{lane.dsp.sidechain_sources(&prepared.keys);}
        for lane in &mut self.lanes{
            if !prepared.keys.iter().any(|(source,binding,epoch,_)|*source==lane.source&&binding==&lane.binding&&*epoch==lane.source_epoch){
                lane.active=false;lane.stop=Some(lane.stop.map_or(prepared.at,|old|old.min(prepared.at)));
            }
        }
        for update in prepared.updates{
            let lane=&mut self.lanes[update.index];
            if !lane.active{
                lane.next=lane.next.max(update.activate);
                // A new explicit activation cancels the previous ended actor;
                // it never reuses its FIFO under the resumed processing scope.
                lane.neural=None;lane.neural_original=None;lane.neural_levels=None;lane.neural_eof=false;
                lane.neural_floor=lane.next.max(lane.retired_before);lane.dsp.reset();
            }
            lane.active=true;lane.stop=None;lane.detector_only=update.detector_only;
            if let Some(ring)=update.ring{lane.ring=ring;}
            if let Some(configs)=update.configs{lane.configs=configs;}
        }
        self.lanes.extend(prepared.new);self.serial=prepared.serial;
        for action in prepared.actions {
            if let Some(row)=self.actions.iter_mut().find(|row|row.source==action.source){*row=action;}else{self.actions.push(action);}
        }
        self.actions.sort_unstable_by_key(|row|row.source.get());
    }
    /// Fair bounded service on the existing audio owner. Every successful block
    /// moves one source input cursor exactly once. Consumers never call filters.
    pub(super) fn service(&mut self,groups:&HashMap<AudioInputKey,InputGroup>,fences:&HashMap<AudioInputKey,InputFence>,push:&push_gate::Bank)->Result<(),ProgramAudioError>{
        let count=self.lanes.len();if count==0{return Ok(());}
        let channels=self.lanes.first().map_or(0,|lane|usize::from(lane.settings.channels()));
        // Subscriptions exist before the first key callback, not only after
        // the ducked source happens to receive its first input block.
        for lane in &mut self.lanes{
            if lane.active||lane.stop.is_some_and(|stop|lane.next<stop){
                if let Some(config)=lane.configs.iter().rfind(|config|config.effective<=lane.next){
                    lane.dsp.prepare_detector_input(&config.plan,lane.next)?;
                }
            }
        }
        let mut idle=0;let mut blocks=0;let mut craft_work=0usize;
        let mut meta=self.metadata();let mut ranges=self.lanes.iter().map(|lane|lane.ranges.len()).sum::<usize>();
        while blocks<PROCESS_BLOCKS&&idle<count{
            let index=self.fair%count;self.fair=(index+1)%count;
            let delayed=self.lanes[index].neural.is_some()||self.lanes[index].configs.iter().rfind(|config|config.effective<=self.lanes[index].next)
                .is_some_and(|config|delayed_source::positions(&config.plan.sources()[0].filters).next().is_some());
            if delayed{
                let step=self.service_delayed_lane(index,groups,fences,64*1024*1024-craft_work,push)?;
                craft_work=craft_work.checked_add(step.work).filter(|n|*n<=64*1024*1024).ok_or(ProgramAudioError::Limit("combined source filter work"))?;
                meta=self.metadata();ranges=self.lanes.iter().map(|lane|lane.ranges.len()).sum();
                blocks+=1;if step.advanced{idle=0;}else{idle+=1;}continue;
            }
            // The private delayed-producer latch does not become a native EOF
            // or disable the ordinary point path after RNNoise is removed.
            self.lanes[index].neural_eof=false;
            self.lanes[index].neural_levels=None;
            let action=self.actions.iter().find(|row|row.source==self.lanes[index].source).and_then(|row|row.events.last());
            let signal_muted=action.is_some_and(|event|event.muted);let signal_control=action.map_or(0,|event|event.revision);
            let lane=&mut self.lanes[index];
            if !lane.active&&lane.stop.is_none_or(|stop|lane.next>=stop){
                if lane.native_epoch.take().is_some(){lane.processing_epoch=lane.processing_epoch.checked_add(1).ok_or(ProgramAudioError::Limit("source processing epoch exhausted"))?;lane.dsp.reset();}
                idle+=1;continue;
            }
            let Some(group)=groups.get(&lane.binding)else{idle+=1;continue;};
            let Some(head)=group.ring.queue_head()else{idle+=1;continue;};
            if i128::from(lane.next)<head{
                lane.next=u64::try_from(head.max(0)).map_err(|_|ProgramAudioError::TimeOverflow)?;
                lane.processing_epoch=lane.processing_epoch.checked_add(1).ok_or(ProgramAudioError::Limit("source processing epoch exhausted"))?;lane.dsp.reset();
            }
            let available=group.ring.end()-i128::from(lane.next);
            if available<=0{idle+=1;continue;}
            let config_index=lane.configs.iter().rposition(|config|config.effective<=lane.next).ok_or(ProgramAudioError::Invalid("source processing config history missing"))?;
            let mut frames=usize::try_from(available.min(1024)).map_err(|_|ProgramAudioError::TimeOverflow)?;
            if let Some(next)=lane.configs.get(config_index+1){frames=frames.min(usize::try_from(next.effective-lane.next).map_err(|_|ProgramAudioError::TimeOverflow)?);}
            if let Some(stop)=lane.stop{frames=frames.min(usize::try_from(stop.saturating_sub(lane.next)).map_err(|_|ProgramAudioError::TimeOverflow)?);}
            if frames==0{idle+=1;continue;}
            let Some((skip,actual))=group.ring.valid_run(i128::from(lane.next),frames)else{
                lane.next=lane.next.checked_add(frames as u64).ok_or(ProgramAudioError::TimeOverflow)?;blocks+=1;idle=0;continue;
            };
            lane.next=lane.next.checked_add(skip as u64).ok_or(ProgramAudioError::TimeOverflow)?;
            let start=lane.next;let end=start.checked_add(actual as u64).ok_or(ProgramAudioError::TimeOverflow)?;
            let config=&lane.configs[config_index];
            if config.requested>config.effective{return Err(ProgramAudioError::Invalid("processed source control epoch is invalid"));}
            let store_start=start.max(lane.retired_before);
            let stores=store_start<end;
            if stores&&!lane.ring.can_place(i128::from(store_start),usize::try_from(end-store_start).map_err(|_|ProgramAudioError::TimeOverflow)?){idle+=1;continue;}
            let processing_epoch=if lane.native_epoch!=Some(group.dsp_epoch){lane.processing_epoch.checked_add(1)
                .ok_or(ProgramAudioError::Limit("source processing epoch exhausted"))?}else{lane.processing_epoch};
            let merges=lane.ranges.last().is_some_and(|range|range.input_packet.is_none()&&range.lineage==Lineage::default()&&range.end==store_start&&range.config==config.serial
                &&range.processing_epoch==processing_epoch&&range.revision==config.revision&&range.native_epoch==group.dsp_epoch&&range.generation==group.generation&&range.original==group.format);
            if stores&&!merges{
                if ranges==MAX_RANGES||meta.checked_add(std::mem::size_of::<Produced>()).is_none_or(|v|v>MAX_METADATA){return Err(ProgramAudioError::Backpressure("processed source scope receipts"));}
                let before=lane.ranges.capacity();
                lane.ranges.try_reserve_exact(1).map_err(|_|ProgramAudioError::Limit("processed source receipt allocation"))?;
                meta=meta.checked_add((lane.ranges.capacity()-before)*std::mem::size_of::<Produced>())
                    .filter(|v|*v<=MAX_METADATA).ok_or(ProgramAudioError::Limit("processed source receipt storage"))?;
            }
            let next_craft_work=actual.checked_mul(config.plan.craft_work_per_frame()).and_then(|n|craft_work.checked_add(n))
                .ok_or(ProgramAudioError::Limit("Craft service work overflow"))?;
            if next_craft_work>64*1024*1024{break;}craft_work=next_craft_work;
            let signal_active=lane.active;
            let signal=SignalStamp{lineage:Lineage::default(),session:lane.session,source:Some(lane.source),scope:lane.signal_scope,epoch:lane.source_epoch,
                native_epoch:group.dsp_epoch,generation:group.generation,processing_epoch,config:config.serial,revision:config.revision,control:signal_control,muted:signal_muted,start};
            // All subscribers admit their receipt capacity before this source
            // advances or any subscriber consumes/publishes new PCM.
            if signal_active{for (other_index,other) in self.lanes.iter().enumerate(){if other_index!=index{other.dsp.check_signal(signal,actual)?;}}}
            let lane=&mut self.lanes[index];let config=&lane.configs[config_index];
            let mut samples=vec![0.0;actual*channels];
            let missing=group.ring.read(i128::from(start),actual,&mut samples)?;
            if missing!=0{return Err(ProgramAudioError::Invalid("admitted source input lost validity"));}
            if lane.native_epoch.is_some_and(|old|old!=group.dsp_epoch){lane.dsp.reset();}
            config.plan.process_converted_source(AudioMixRequest {sample_rate:lane.settings.sample_rate(),channels:lane.settings.channels(),frames:actual,advance_ns:0},
                start,Some(group.converter.input_format()),&mut samples,&mut lane.dsp)?;
            if stores {
                let offset=usize::try_from(store_start-start).map_err(|_|ProgramAudioError::TimeOverflow)?;
                lane.ring.place(i128::from(store_start),&samples[offset*channels..],false)?;
                if merges{lane.ranges.last_mut().unwrap().end=end;}else{
                    lane.ranges.push(Produced {input_packet:None,lineage:Lineage::default(),start:store_start,end,config:config.serial,revision:config.revision,
                        native_epoch:group.dsp_epoch,generation:group.generation,processing_epoch,original:group.format});ranges+=1;
                }
            }
            lane.native_epoch=Some(group.dsp_epoch);lane.processing_epoch=processing_epoch;lane.next=end;
            if lane.detector_only{
                lane.ring.discard_before(i128::from(end));lane.ranges.clear();lane.retired_before=end;
            }
            if signal_active{
                // The retained pre-gain ring was already written above. Apply
                // push silence only to the separate sidechain publication.
                for (offset,frame) in samples.chunks_exact_mut(channels).enumerate(){if push.silent(signal.source.expect("source signal"),start+offset as u64){frame.fill(0.0);}}
                for (other_index,other) in self.lanes.iter_mut().enumerate(){if other_index!=index{other.dsp.publish_signal(signal,&samples,signal_muted);}}
            }
            blocks+=1;idle=0;
        }
        Ok(())
    }
    pub(super) fn camera_stop(&mut self,source:Id,cut:u64,raw:u64){
        for lane in &mut self.lanes{lane.dsp.sidechain_cut(source,raw);}
        for lane in self.lanes.iter_mut().filter(|lane|lane.source==source){
            lane.active=false;lane.camera_cut=true;
            lane.output_end=Some(lane.output_end.map_or(cut,|old|old.min(cut)));
            let stop=lane.stop.map_or(raw,|old|old.min(raw));lane.stop=Some(stop);
            lane.ring.truncate_after(i128::from(stop));
            lane.ranges.retain(|range|range.start<stop);
            for range in &mut lane.ranges{range.end=range.end.min(stop);}
        }
    }
    pub(super) fn retire_camera_unread_tracks(&mut self,sync:&sync_offset::SourceSync,timeline:&AudioRouteTimeline,
        tracks:skylive_core::SourceAudioTrackMask,cursor:u64){
        self.lanes.retain(|lane|!lane.camera_cut||sync.camera_prefix_routed(lane.source,Some(lane.source_epoch),cursor,u64::MAX,
            &|source,binding,start,end|timeline.source_routes_between(source,binding,start,end,tracks)));
    }
    pub(super) fn retire_unread_camera(&mut self){
        self.lanes.retain(|lane|!lane.camera_cut);
    }
    /// A real media seek/pause barrier fences native generation and removes
    /// only future data for sources on this complete binding. Retained prefix
    /// PCM/receipts stay attached to their original processing configuration.
    pub(super) fn binding_control_floor(&self,binding:&AudioInputKey)->u64{
        self.lanes.iter().filter(|lane|&lane.binding==binding).map(|lane|lane.retired_before).max().unwrap_or(0)
    }
    pub(super) fn validate_binding_control(&self,binding:&AudioInputKey)->Result<(),ProgramAudioError>{
        if self.lanes.iter().filter(|lane|&lane.binding==binding).any(|lane|lane.processing_epoch.checked_add(2).is_none()){
            return Err(ProgramAudioError::Limit("source processing epoch exhausted"));
        }
        Ok(())
    }
    pub(super) fn control_binding(&mut self,binding:&AudioInputKey,frame:u64){
        for index in 0..self.lanes.len(){
            if &self.lanes[index].binding==binding{let source=self.lanes[index].source;
                for lane in &mut self.lanes{lane.dsp.sidechain_cut(source,frame);}
            }
        }
        for lane in self.lanes.iter_mut().filter(|lane|&lane.binding==binding){
            let frame=frame.max(lane.retired_before);
            lane.ring.truncate_after(i128::from(frame));
            lane.ranges.retain(|range|range.start<frame);
            for range in &mut lane.ranges{range.end=range.end.min(frame);}
            lane.next=lane.next.min(frame);lane.processing_epoch+=1;lane.native_epoch=None;lane.dsp.reset();lane.neural=None;lane.neural_original=None;lane.neural_floor=frame;lane.neural_levels=None;lane.neural_eof=false;
            for config in &mut lane.configs{
                if config.effective>=frame&&!lane.ranges.iter().any(|range|range.config==config.serial){config.effective=config.requested.max(frame);}
            }
            lane.configs.sort_by_key(|config|(config.effective,config.serial));
        }
    }
    pub(super) fn retire(&mut self,frame:u64,sync:&sync_offset::SourceSync,required:&[RequiredAudioBinding],monitor:&dyn Fn(Id,&AudioInputKey,u64)->Option<i128>){
        for row in &mut self.actions {
            let keep=row.events.partition_point(|event|event.frame<=frame).saturating_sub(1);
            if keep!=0{row.events.drain(..keep);}
        }
        self.actions.retain(|row|required.iter().any(|source|source.source==row.source));
        for lane in &mut self.lanes{
            let mut floor=sync.source_retention_floor_epoch(lane.source,&lane.binding,lane.source_epoch,i128::from(frame));
            if let Some(actual)=monitor(lane.source,&lane.binding,lane.source_epoch){floor=floor.min(actual);}
            lane.retired_before=lane.retired_before.max(u64::try_from(floor.max(0)).unwrap_or(u64::MAX));
            let floor=i128::from(lane.retired_before);
            lane.ring.discard_before(floor);
            lane.ranges.retain(|range|i128::from(range.end)>floor);
            if let Some(first)=lane.ranges.first_mut(){first.start=first.start.max(u64::try_from(floor.max(0)).unwrap_or(u64::MAX));}
            let current=lane.configs.iter().rposition(|config|config.effective<=lane.next);
            let current_serial=current.map(|index|lane.configs[index].serial);
            lane.configs.retain(|config|Some(config.serial)==current_serial||config.effective>lane.next||lane.ranges.iter().any(|range|range.config==config.serial||range.lineage.references(config.serial))||lane.neural.as_ref().is_some_and(|chain|chain.references(config.serial)));
        }
        self.lanes.retain(|lane|lane.active||(!lane.camera_cut||sync.camera_prefix_epoch(lane.source,lane.source_epoch,frame,u64::MAX))&&lane.output_end.is_none_or(|end|frame<end)&&required.iter().any(|binding|binding.source==lane.source&&binding.binding==lane.binding));
    }
}
