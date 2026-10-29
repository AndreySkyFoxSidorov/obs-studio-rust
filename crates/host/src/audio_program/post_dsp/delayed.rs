//! Actual delayed branch of the existing once-per-source history owner.
use super::*;
use crate::audio::delayed_source::Progress;
use crate::audio::noise_suppression::{InputStamp,Scope};

pub(super) struct Step {pub advanced:bool,pub work:usize}

impl Histories {
    pub(super) fn service_delayed_lane(&mut self,index:usize,groups:&HashMap<AudioInputKey,InputGroup>,
        fences:&HashMap<AudioInputKey,InputFence>,credit:usize,push:&push_gate::Bank)->Result<Step,ProgramAudioError>{
        // Ready PCM has already completed its exact point/model chain. Publish
        // it once before changing structure; explicit seek/privacy fences clear
        // the affected actor separately, never retag an old ready packet.
        if self.lanes[index].neural.as_ref().is_some_and(|chain|chain.peek().is_some()){
            let advanced=self.publish_delayed(index,push)?;return Ok(Step{advanced,work:0});
        }
        let group=groups.get(&self.lanes[index].binding);
        let lane=&mut self.lanes[index];
        // An ended actor owns its already-copied PCM. Shared endpoint raw
        // retirement cannot invalidate that private pre-cut drain.
        if !lane.neural_eof{
          if let Some(head)=group.and_then(|group|group.ring.queue_head()){
            if i128::from(lane.next)<head{
                lane.next=u64::try_from(head.max(0)).map_err(|_|ProgramAudioError::TimeOverflow)?;
                lane.neural=None;lane.neural_original=None;lane.neural_levels=None;lane.neural_floor=lane.next.max(lane.retired_before);
                lane.dsp.reset();
            }
          }
        }
        let config_index=lane.configs.iter().rposition(|config|config.effective<=lane.next)
            .ok_or(ProgramAudioError::Invalid("delayed source config missing"))?;

        if delayed_source::positions(&lane.configs[config_index].plan.sources()[0].filters).next().is_none(){
            // Actual filter removal/bypass discards only this actor's pending
            // packets. Already-produced prefix/history stays immutable.
            lane.neural=None;lane.neural_original=None;lane.neural_levels=None;lane.neural_floor=lane.next.max(lane.retired_before);
            lane.neural_eof=false;
            lane.processing_epoch=lane.processing_epoch.checked_add(1).ok_or(ProgramAudioError::Limit("source processing epoch exhausted"))?;
            return Ok(Step{advanced:true,work:0});
        }
        let available=group.map_or(0,|group|(group.ring.end()-i128::from(lane.next)).max(0));
        let stopped=!lane.active&&lane.stop.is_none_or(|stop|lane.next>=stop);
        if lane.neural_eof&&lane.neural.is_none(){
            let fresh=lane.active&&available>0&&group.is_some_and(|group|lane.native_epoch!=Some(group.dsp_epoch));
            if fresh{lane.neural_eof=false;}else{return Ok(Step{advanced:false,work:0});}
        }
        let native_ended=available==0&&fences.get(&lane.binding).is_some_and(|fence|fence.sealed);
        if available>0&&!stopped&&!lane.neural_eof{
            let group=group.ok_or(ProgramAudioError::Invalid("delayed source native group missing"))?;
            let mut scope=Scope{session:lane.session,source:lane.source,binding_scope:lane.signal_scope,
                source_epoch:lane.source_epoch,native_epoch:group.dsp_epoch,generation:group.generation,
                processing_epoch:lane.processing_epoch,original:group.converter.input_format()};
            let filters=&lane.configs[config_index].plan.sources()[0].filters;
            if lane.neural.as_ref().is_none_or(|chain|!chain.matches(filters,scope)){
                let next_epoch=lane.processing_epoch.checked_add(1).ok_or(ProgramAudioError::Limit("source processing epoch exhausted"))?;
                scope.processing_epoch=next_epoch;
                let needed=delayed_source::requirements(lane.settings,filters)?;
                // Entire old/new construction peak was reserved by prepare(),
                // before timeline/route publication or this first allocation.
                if needed.bytes>self.neural_reserved{return Err(ProgramAudioError::Limit("RNNoise source lacks prepared actor credit"));}
                let replacement=SourceChain::new(lane.settings,filters,scope,needed.bytes)?;
                lane.neural=Some(replacement);lane.neural_original=Some(group.format);lane.neural_levels=None;
                lane.neural_floor=lane.next.max(lane.retired_before);lane.processing_epoch=next_epoch;
                lane.native_epoch=Some(group.dsp_epoch);lane.dsp.reset();
            }else if let Some(chain)=&mut lane.neural{chain.update_shape(filters);}
        }
        // Deactivation closes this SourceID's producer, independently of a
        // shared native binding. Once available pre-cut PCM is accepted, no
        // unseen later endpoint packet can keep the stopped source alive.
        if stopped||native_ended||(!lane.active&&available==0){lane.neural_eof=true;}
        let Some(chain)=lane.neural.as_mut()else{return Ok(Step{advanced:false,work:0});};
        if lane.neural_eof{chain.end_input();}
        let config=&lane.configs[config_index];
        if config.requested>config.effective{return Err(ProgramAudioError::Invalid("delayed processing config epoch invalid"));}
        lane.dsp.prepare_detector_input(&config.plan,lane.next)?;
        let mut candidate=lane.dsp.clone();
        let mut envelope=[0_f32;1024];
        let source=lane.source;
        let mut point=|_:usize,range:std::ops::Range<usize>,samples:&mut[f32]|{
            candidate.process_source_range(source,range.start,range.end,samples,&mut envelope)
        };
        let step=chain.service(config.serial,config.revision,credit,config.plan.craft_work_per_frame(),&mut point)?;
        lane.dsp=candidate;
        if step.progress==Progress::Drained{
            lane.neural=None;lane.neural_original=None;lane.neural_levels=None;
            return Ok(Step{advanced:true,work:step.work});
        }
        if matches!(step.progress,Progress::Advanced|Progress::Ready){
            return Ok(Step{advanced:true,work:step.work});
        }
        if step.progress==Progress::WorkLimit||chain.input_ended()||available==0||stopped{
            return Ok(Step{advanced:false,work:step.work});
        }
        let group=group.ok_or(ProgramAudioError::Invalid("delayed source input group missing"))?;
        let mut frames=usize::try_from(available.min(1024)).map_err(|_|ProgramAudioError::TimeOverflow)?;
        if let Some(next)=lane.configs.get(config_index+1){frames=frames.min(usize::try_from(next.effective.saturating_sub(lane.next)).map_err(|_|ProgramAudioError::TimeOverflow)?);}
        if let Some(stop)=lane.stop{frames=frames.min(usize::try_from(stop.saturating_sub(lane.next)).map_err(|_|ProgramAudioError::TimeOverflow)?);}
        if frames==0{return Ok(Step{advanced:false,work:step.work});}
        let Some((skip,actual))=group.ring.valid_run(i128::from(lane.next),frames)else{
            lane.next=lane.next.checked_add(frames as u64).ok_or(ProgramAudioError::TimeOverflow)?;
            lane.neural=None;lane.neural_original=None;lane.neural_levels=None;lane.neural_floor=lane.next.max(lane.retired_before);
            lane.dsp.reset();return Ok(Step{advanced:true,work:step.work});
        };
        if skip!=0{
            lane.next=lane.next.checked_add(skip as u64).ok_or(ProgramAudioError::TimeOverflow)?;
            lane.neural=None;lane.neural_original=None;lane.neural_levels=None;lane.neural_floor=lane.next.max(lane.retired_before);
            lane.dsp.reset();return Ok(Step{advanced:true,work:step.work});
        }
        let start=lane.next;let end=start.checked_add(actual as u64).ok_or(ProgramAudioError::TimeOverflow)?;
        let scope=Scope{session:lane.session,source:lane.source,binding_scope:lane.signal_scope,
            source_epoch:lane.source_epoch,native_epoch:group.dsp_epoch,generation:group.generation,
            processing_epoch:lane.processing_epoch,original:group.converter.input_format()};
        let stamp=InputStamp{scope,start:i128::from(start),config:config.serial,revision:config.revision,packet:0};
        match chain.admit_input(stamp,actual){
            Ok(())=>{},Err(crate::audio::AudioError::NoiseSuppression(crate::audio::noise_suppression::Error::Backpressure))=>return Ok(Step{advanced:false,work:step.work}),
            Err(error)=>return Err(error.into()),
        }
        let point_work=actual.checked_mul(config.plan.craft_work_per_frame()).and_then(|n|n.checked_add(step.work))
            .ok_or(ProgramAudioError::Limit("RNNoise input point work"))?;
        if point_work>credit{return Ok(Step{advanced:false,work:step.work});}
        let channels=usize::from(lane.settings.channels());
        let mut samples=vec![0_f32;actual*channels];
        if group.ring.read(i128::from(start),actual,&mut samples)?!=0{return Err(ProgramAudioError::Invalid("admitted delayed input lost validity"));}
        let source_plan=&config.plan.sources()[0];
        let transform=skylive_media::SourceChannelTransform::new(skylive_media::SourceChannelSettings{
            force_mono:source_plan.properties.force_mono(),balance:source_plan.properties.balance()},Some(group.converter.input_format()),lane.settings.channels())
            .map_err(crate::audio::AudioError::Media)?;
        let mut candidate=lane.dsp.clone();
        let mut envelope=[0_f32;1024];
        let mut point=|segment:usize,range:std::ops::Range<usize>,samples:&mut[f32]|{
            if segment==0{for frame in samples.chunks_exact_mut(channels){transform.process_frame(frame).map_err(crate::audio::AudioError::Media)?;}}
            candidate.process_source_range(source,range.start,range.end,samples,&mut envelope)
        };
        chain.push(stamp,&samples,&mut point)?;
        candidate.finish(start,actual)?;lane.dsp=candidate;
        lane.next=end;lane.native_epoch=Some(group.dsp_epoch);
        Ok(Step{advanced:true,work:point_work})
    }

    fn publish_delayed(&mut self,index:usize,push:&push_gate::Bank)->Result<bool,ProgramAudioError>{
        let lane=&self.lanes[index];
        let chain=lane.neural.as_ref().ok_or(ProgramAudioError::Invalid("delayed ready actor missing"))?;
        let ready=chain.peek().ok_or(ProgramAudioError::Invalid("delayed ready packet missing"))?;
        let origin=ready.receipt.origin;
        if origin.scope.session!=lane.session||origin.scope.source!=lane.source||origin.scope.binding_scope!=lane.signal_scope||origin.scope.source_epoch!=lane.source_epoch{
            return Err(ProgramAudioError::Invalid("delayed packet owner scope changed"));
        }
        let end=ready.start.checked_add(ready.receipt.frames as i128).ok_or(ProgramAudioError::TimeOverflow)?;
        let first=ready.start.max(i128::from(lane.retired_before.max(lane.neural_floor)));
        let end=lane.stop.map_or(end,|stop|end.min(i128::from(stop)));
        let stores=first<end;
        let start=u64::try_from(first.max(0)).map_err(|_|ProgramAudioError::TimeOverflow)?;
        let finish=u64::try_from(end.max(0)).map_err(|_|ProgramAudioError::TimeOverflow)?;
        let original=lane.neural_original.ok_or(ProgramAudioError::Invalid("delayed native format receipt missing"))?;
        let packet=delayed_source::PacketProvenance{packet:origin.packet,input_start:origin.start,
            input_frames:u32::try_from(ready.receipt.frames).map_err(|_|ProgramAudioError::TimeOverflow)?,
            delay_frames:u64::try_from(origin.start-ready.start).map_err(|_|ProgramAudioError::TimeOverflow)?};
        let merges=lane.ranges.last().is_some_and(|range|range.input_packet==Some(packet)&&range.end==start&&range.config==origin.config&&range.revision==origin.revision
            &&range.lineage==ready.receipt.lineage&&range.native_epoch==origin.scope.native_epoch&&range.generation==origin.scope.generation
            &&range.processing_epoch==origin.scope.processing_epoch&&range.original==original);
        if stores&&!lane.ring.can_place(i128::from(start),usize::try_from(finish-start).map_err(|_|ProgramAudioError::TimeOverflow)?){
            return Ok(false); // Keep the exact ready PCM; no filters execute again.
        }
        let signal_first=ready.start.max(i128::from(lane.neural_floor));
        let signal_end=lane.stop.map_or(ready.start+ready.receipt.frames as i128,|stop|
            (ready.start+ready.receipt.frames as i128).min(i128::from(stop)));
        let signal_frames=usize::try_from((signal_end-signal_first).max(0)).map_err(|_|ProgramAudioError::TimeOverflow)?;
        let signal_start=u64::try_from(signal_first.max(0)).map_err(|_|ProgramAudioError::TimeOverflow)?;
        let signal_offset=usize::try_from(signal_first-ready.start).map_err(|_|ProgramAudioError::TimeOverflow)?;
        let channels=usize::from(lane.settings.channels());
        let levels=if lane.active&&signal_frames!=0{
            Some(source_levels(&chain.ready_samples()[signal_offset*channels..(signal_offset+signal_frames)*channels],channels,origin.scope.processing_epoch))
        }else{None};
        let action=self.actions.iter().find(|row|row.source==lane.source).and_then(|row|row.events.last());
        let muted=action.is_some_and(|action|action.muted);
        let signal=SignalStamp{session:lane.session,source:Some(lane.source),scope:lane.signal_scope,epoch:lane.source_epoch,
            native_epoch:origin.scope.native_epoch,generation:origin.scope.generation,processing_epoch:origin.scope.processing_epoch,
            config:origin.config,revision:origin.revision,lineage:ready.receipt.lineage,control:action.map_or(0,|action|action.revision),muted,start:signal_start};
        // Reserve a single bounded source quantum before mutating any queues.
        // Original delayed PCM stays unchanged for Program and monitor readers.
        let mut gated=None;
        if lane.active&&signal_frames!=0&&(0..signal_frames).any(|offset|push.silent(lane.source,signal_start+offset as u64)){
            let original=&chain.ready_samples()[signal_offset*channels..(signal_offset+signal_frames)*channels];
            let mut samples=Vec::new();samples.try_reserve_exact(original.len()).map_err(|_|ProgramAudioError::Limit("push sidechain workspace"))?;samples.extend_from_slice(original);
            for(offset,frame)in samples.chunks_exact_mut(channels).enumerate(){if push.silent(lane.source,signal_start+offset as u64){frame.fill(0.0);}}
            gated=Some(samples);
        }
        let stored_frames=if stores{usize::try_from(finish-start).map_err(|_|ProgramAudioError::TimeOverflow)?}else{0};
        // Original signal packets are clipped only at actual privacy/cut floors;
        // pre-fader PCM, current mute and exact processing lineage are retained.
        if lane.active&&signal_frames!=0{for(other,subscriber)in self.lanes.iter().enumerate(){if other!=index{subscriber.dsp.check_signal(signal,signal_frames)?;}}}
        if stores&&!merges{
            if self.lanes.iter().map(|lane|lane.ranges.len()).sum::<usize>()>=MAX_RANGES
                ||self.metadata().checked_add(std::mem::size_of::<Produced>()).is_none_or(|n|n>MAX_METADATA){return Err(ProgramAudioError::Backpressure("delayed source scope receipts"));}
            self.lanes[index].ranges.try_reserve_exact(1).map_err(|_|ProgramAudioError::Limit("delayed source receipt allocation"))?;
            if self.metadata()>MAX_METADATA{return Err(ProgramAudioError::Limit("delayed source receipt storage"));}
        }
        let lane=&mut self.lanes[index];
        let active=lane.active;
        let channels=usize::from(lane.settings.channels());
        let chain=lane.neural.as_ref().ok_or(ProgramAudioError::Invalid("delayed ready actor retired"))?;
        let offset=usize::try_from(first-ready.start).map_err(|_|ProgramAudioError::TimeOverflow)?;
        if stores{
            let samples=&chain.ready_samples()[offset*channels..(offset+stored_frames)*channels];
            lane.ring.place(i128::from(start),samples,false)?;
            if merges{if let Some(last)=lane.ranges.last_mut(){last.end=finish;}}else{
                lane.ranges.push(Produced{input_packet:Some(packet),start,end:finish,config:origin.config,revision:origin.revision,lineage:ready.receipt.lineage,
                    native_epoch:origin.scope.native_epoch,generation:origin.scope.generation,processing_epoch:origin.scope.processing_epoch,original});
            }
        }
        if active&&signal_frames!=0{
            // The key callback is an actual independent reader. Its source
            // clock is not the Program cursor or the user's Sync Offset.
            let (before,tail)=self.lanes.split_at_mut(index);let(source,after)=tail.split_first_mut().ok_or(ProgramAudioError::Invalid("delayed source disappeared"))?;
            let original=&source.neural.as_ref().ok_or(ProgramAudioError::Invalid("delayed signal actor missing"))?.ready_samples()[signal_offset*channels..(signal_offset+signal_frames)*channels];
            let samples=gated.as_deref().unwrap_or(original);
            for subscriber in before.iter_mut().chain(after.iter_mut()){subscriber.dsp.publish_signal(signal,samples,muted);}
        }
        let lane=&mut self.lanes[index];
        if let Some(levels)=levels{lane.neural_levels=Some(levels);}
        lane.neural.as_mut().ok_or(ProgramAudioError::Invalid("delayed publication actor missing"))?.acknowledge()?;
        if lane.detector_only&&stores{lane.ring.discard_before(i128::from(finish));lane.ranges.clear();lane.retired_before=finish;}
        Ok(true)
    }
}
