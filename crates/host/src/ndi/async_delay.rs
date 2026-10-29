//! NDI sample queue before latest-snapshot coalescing. This does not establish
//! audio synchronization or authorize rendering the async filter by itself.
use super::*;
use crate::camera_capture::async_delay::{Plan, Scope as DelayScope, SourceDelays};
use skylive_media::AsyncDelayBudget;
use std::collections::VecDeque;

struct Metadata {
    sequence:u64, requested_at:MediaTime, metadata:VideoMetadata,
    timestamp:i128, clock:NdiVideoClock, mapped:i128, estimated:bool,
}
pub(super) struct NdiDelay {
    queues:SourceDelays,
    scope:Option<DelayScope>,
    format:Option<(u32,u32,u32,u32)>,
    history_serial:u64,
    history_live:Option<Arc<AtomicBool>>,
    samples:VecDeque<Metadata>,
    budget:AsyncDelayBudget,
    staged:Option<super::staged_delay::StagedDelay>,
}
impl NdiDelay {
    pub fn new(budget:AsyncDelayBudget)->Self {Self {queues:SourceDelays::new(budget.clone()),scope:None,format:None,history_serial:0,history_live:None,samples:VecDeque::new(),budget,staged:None}}
    pub fn reset(&mut self) {if let Some(live)=self.history_live.take(){live.store(false,Ordering::Release);}self.staged=None;self.queues.reset_all();let _=self.queues.reconcile(&[]);self.samples.clear();self.scope=None;self.format=None;}
    pub fn push(&mut self, instance:u64, session:u64, epoch:u64, source:Id,
        filters:&[(skylive_core::VideoFilterId,u16)], upstream:&[skylive_core::VideoFilter],frame:NdiFrame, live_epoch:&Arc<AtomicU64>,mask:Option<&super::NdiMaskPublication>)->Result<Option<NdiFrame>,NdiError> {
        let (Some(timestamp),Some(clock))=(frame.native_timestamp_ns,frame.native_clock) else {
            self.reset();return Err(NdiError::Unavailable("NDI video has no timestamp in its established A/V clock"));
        };
        let scope=DelayScope {session,source,binding_generation:epoch,camera_owner:instance,
            stream:frame.stream_generation,clock_epoch:frame.native_clock_epoch};
        let format=(frame.metadata.width,frame.metadata.height,frame.metadata.fps_num,frame.metadata.fps_den);
        if self.scope!=Some(scope) || self.format!=Some(format) {
            self.reset();
            self.history_serial=self.history_serial.checked_add(1).ok_or(NdiError::Limit("NDI delay history identity"))?;
            self.history_live=Some(Arc::new(AtomicBool::new(true)));
            self.scope=Some(scope);self.format=Some(format);
        }
        if frame.metadata.fps_num==0 || frame.metadata.fps_den==0 {self.reset();return Err(NdiError::Invalid("NDI frame rate is unavailable for delay capacity"));}
        let mut plans=Vec::new();
        plans.try_reserve_exact(filters.len()).map_err(|_|NdiError::Limit("NDI delay plan allocation"))?;
        let mut total=0usize;
        for &(filter,ms) in filters {
            let num=u128::from(ms)*u128::from(frame.metadata.fps_num);
            let den=1000u128*u128::from(frame.metadata.fps_den);
            let capacity=(num+den-1)/den+2;
            if capacity>4096 {self.reset();return Err(NdiError::Limit("NDI delay frame capacity"));}
            total=total.checked_add(capacity as usize).ok_or(NdiError::Limit("NDI delay metadata capacity"))?;
            plans.push(Plan {scope,filter,chain_revision:epoch,delay_ms:u32::from(ms),max_pending:capacity as usize});
        }
        if filters.is_empty() || total>4096 {self.reset();return Err(NdiError::Limit("NDI delay chain capacity"));}
        let has_resource=upstream.iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::PersonBackgroundRemoval{..}|skylive_core::VideoEffect::Scroll{..})
            || matches!(filter.effect,skylive_core::VideoEffect::ImageMask{..}) && filter.mask_image().is_some()
            || matches!(filter.effect,skylive_core::VideoEffect::ApplyLut{..}) && filter.lut_asset().is_some());
        if has_resource {
            if self.staged.is_none(){self.staged=Some(super::staged_delay::StagedDelay::new(source,self.budget.clone(),total)?);}
            let stage=self.staged.as_mut().ok_or(NdiError::Stale)?;
            let mut output=stage.poll(scope,upstream,filters,self.history_serial,self.history_live.as_ref().ok_or(NdiError::Stale)?,live_epoch,timestamp)?;
            if let Some(delayed)=&mut output {delayed.draft_raw=Some(Arc::new(frame.clone()));}
            stage.submit(scope,&plans,upstream,frame,mask)?;
            return Ok(output);
        }
        if self.queues.reconcile(&plans).is_err(){self.reset();return Err(NdiError::Limit("NDI delay configuration budget"));}
        if self.samples.len()>=total || self.samples.try_reserve(1).is_err() {self.reset();return Err(NdiError::Limit("NDI delay metadata budget"));}
        let ids:Vec<_>=filters.iter().map(|(id,_)|*id).collect();
        let output=match self.queues.push_chain(&ids,scope,frame.sequence,timestamp,None,None,&frame.image) {
            Ok(output)=>output,
            Err(_)=>{self.reset();return Err(NdiError::Limit("NDI delay frame admission"));}
        };
        self.samples.push_back(Metadata {sequence:frame.sequence,requested_at:frame.requested_at,metadata:frame.metadata.clone(),
            timestamp,clock,mapped:frame.mapped_media_ns,estimated:frame.mapped_time_estimated});
        let Some(output)=output else {return Ok(None);};
        let Some(sequence)=output.sequence() else {self.reset();return Err(NdiError::Invalid("NDI delay sample identity"));};
        while self.samples.front().is_some_and(|sample|sample.sequence<sequence) {self.samples.pop_front();}
        let Some(sample)=self.samples.pop_front().filter(|sample|sample.sequence==sequence && sample.timestamp==output.timestamp_ns()) else {
            self.reset();return Err(NdiError::Invalid("NDI delay sample metadata mismatch"));
        };
        let draft_raw=Some(Arc::new(frame.clone()));
        let elapsed_ns=timestamp.checked_sub(sample.timestamp).and_then(|value|u64::try_from(value).ok())
            .ok_or(NdiError::Invalid("NDI delay sample timestamp"))?;
        let receipt=crate::capture::AsyncDelayReceipt {session,binding_generation:epoch,live_generation:Some(live_epoch.clone()),
            history_generation:self.history_serial,history_live:self.history_live.as_ref().ok_or(NdiError::Stale)?.clone(),
            source,owner:instance,stream:scope.stream,clock_epoch:scope.clock_epoch,sequence,timestamp_ns:sample.timestamp,ndi_native_clock:Some(sample.clock),
            ndi_media_time_estimate_ns:sample.estimated.then_some(sample.mapped),
            filters:filters.to_vec().into(),staged_camera:None,staged_ndi:None,elapsed_ns};
        Ok(Some(NdiFrame {stream_generation:scope.stream,ndi:frame.ndi,sequence,requested_at:sample.requested_at,
            metadata:sample.metadata,image:output.shared_image(),native_timestamp_ns:Some(sample.timestamp),native_clock:Some(sample.clock),
            native_clock_epoch:scope.clock_epoch,mapped_media_ns:sample.mapped,mapped_time_estimated:sample.estimated,receive_qpc_ns:None,async_delay:Some(receipt),draft_raw}))
    }
}
impl Drop for NdiDelay {
    fn drop(&mut self) { self.reset(); }
}
