//! NDI-specific adapter for the bounded resource-stage executor. Sender PTS
//! remains in its own clock domain; it is never converted into camera QPC.
use super::*;
use crate::camera_capture::{async_delay::{Plan,Scope},staged_delay_worker::{self,Event,Sample,Worker}};
use std::collections::VecDeque;

struct Metadata {sequence:u64,ndi:Arc<NdiSource>,requested_at:MediaTime,metadata:VideoMetadata,
    timestamp:i128,clock:NdiVideoClock,mapped:i128,estimated:bool}

pub(super) struct StagedDelay {
    worker:Worker,
    budget:skylive_media::AsyncDelayBudget,
    masks:Arc<Mutex<crate::video_filters::StagedMaskStore>>,
    luts:Arc<Mutex<crate::video_filters::StagedLutStore>>,
    pending:VecDeque<Metadata>,
    submitted:VecDeque<u64>,
    awaiting:Option<NdiFrame>,
    awaiting_since:Option<std::time::Instant>,
    timed_masks:Option<(Arc<Mutex<crate::mask_live_versions::Book>>,Arc<Mutex<crate::video_filters::StagedMaskStore>>)>,
    timeline:Option<Arc<AtomicBool>>,
    max_pending:usize,
    _metadata_credit:skylive_media::AsyncWorkspacePermit,
}

impl StagedDelay {
    pub fn new(source:Id,budget:skylive_media::AsyncDelayBudget,max_pending:usize)->Result<Self,NdiError> {
        if max_pending==0 || max_pending>4096 {return Err(NdiError::Limit("NDI staged metadata capacity"));}
        let metadata_bytes=max_pending.checked_mul(std::mem::size_of::<Metadata>()+std::mem::size_of::<u64>()).ok_or(NdiError::Limit("NDI staged metadata size"))?;
        let metadata_credit=budget.reserve_workspace(metadata_bytes).map_err(|_|NdiError::Limit("NDI staged metadata budget"))?;
        let worker=Worker::new(source,budget.clone()).map_err(|_|NdiError::Unavailable("NDI stage worker could not start"))?;
        Ok(Self {worker,budget,masks:Arc::new(Mutex::new(Default::default())),
            luts:Arc::new(Mutex::new(Default::default())),pending:VecDeque::new(),submitted:VecDeque::new(),awaiting:None,awaiting_since:None,timed_masks:None,timeline:None,max_pending,_metadata_credit:metadata_credit})
    }

    pub fn submit(&mut self,scope:Scope,plans:&[Plan],upstream:&[skylive_core::VideoFilter],frame:NdiFrame,
        publication:Option<&super::NdiMaskPublication>)->Result<(),NdiError> {
        let timed=upstream.iter().any(|filter|filter.mask_image().is_some_and(|asset|crate::assets::mask_animation_candidate(asset.path())));
        let scroll_timed=upstream.iter().any(|filter|matches!(filter.effect,skylive_core::VideoEffect::Scroll{..}));
        if self.awaiting_since.is_some_and(|start|start.elapsed()>std::time::Duration::from_secs(1)) {
            return Err(NdiError::Unavailable("NDI animated filter requires a covering local receive-time snapshot and clock correlation"));
        }
        if (timed||scroll_timed) && self.awaiting.is_none(){self.awaiting=Some(frame.clone());self.awaiting_since=Some(std::time::Instant::now());}
        let frame=if timed||scroll_timed {self.awaiting.take().ok_or(NdiError::Stale)?} else {frame};
        let timestamp=frame.native_timestamp_ns.ok_or(NdiError::Unavailable("NDI staged video sender timestamp"))?;
        let stages=staged_delay_worker::static_mask_stages(plans,upstream,(frame.image.width(),frame.image.height()),self.worker.person(),self.budget.clone())
            .map_err(|_|NdiError::Invalid("unsupported NDI staged filter chain"))?;
        let credit=Arc::new(self.budget.reserve_workspace(1024*1024).map_err(|_|NdiError::Limit("NDI staged metadata budget"))?);
        let (bounds,publication)=if timed||scroll_timed {
            let Some(publication)=publication.filter(|p|p.binding_epoch==scope.binding_generation&&p.snapshot.session==scope.session&&p.live.load(Ordering::Acquire)) else {self.awaiting=Some(frame);return Ok(());};
            let (first,last)=frame.receive_qpc_ns.ok_or(NdiError::Unavailable("NDI receive clock is unavailable for time-dependent filter"))?;
            let Some(correlation)=publication.snapshot.correlation else {self.awaiting=Some(frame);return Ok(());};
            let Some(early)=correlation.media_bounds(scope.session,publication.snapshot.playback_epoch,first) else {return Err(NdiError::Unavailable("NDI receive clock correlation expired"));};
            let Some(late)=correlation.media_bounds(scope.session,publication.snapshot.playback_epoch,last) else {return Err(NdiError::Unavailable("NDI receive clock correlation expired"));};
            let bounds=skylive_media::MediaTimeBounds {earliest:early.earliest,latest:late.latest};
            if bounds.earliest.0>bounds.latest.0{return Err(NdiError::Invalid("NDI receive clock bounds"));}
            if scroll_timed && bounds.latest.0-bounds.earliest.0>10_000_000{return Err(NdiError::Unavailable("NDI receive clock is too imprecise for Scroll before delay"));}
            if bounds.latest.0>publication.snapshot.clock.now().0 {self.awaiting=Some(frame);return Ok(());}
            (Some(bounds),Some(publication))
        } else {(None,None)};
        let scroll=if scroll_timed {
            let publication=publication.ok_or(NdiError::Stale)?;
            if !publication.scroll_live.load(Ordering::Acquire){return Err(NdiError::Stale);}
            let Some(history)=publication.snapshot.scroll.as_ref() else {self.awaiting=Some(frame);return Ok(());};
            let Some(context)=history.history.estimated_window_filters(scope.session,history.epoch,scope.source,upstream,0,bounds.ok_or(NdiError::Stale)?) else {
                self.awaiting=Some(frame);return Ok(());
            };
            Some((context,publication.scroll_live.clone()))
        } else {None};
        let prepare=if timed {
            let publication=publication.ok_or(NdiError::Stale)?;
            if self.timeline.as_ref().is_some_and(|old|!Arc::ptr_eq(old,&publication.live)) {return Err(NdiError::Stale);}
            let versions=publication.snapshot.versions.clone();
            if self.timed_masks.as_ref().is_none_or(|(old,_)|!Arc::ptr_eq(old,&versions)) {
                self.timed_masks=Some((versions.clone(),Arc::new(Mutex::new(crate::video_filters::StagedMaskStore::with_versions(versions)))));
            }
            self.timeline=Some(publication.live.clone());
            staged_delay_worker::bounded_mask_resources(self.timed_masks.as_ref().ok_or(NdiError::Stale)?.1.clone(),upstream.to_vec(),scope.source,
                publication.snapshot.clock.clone(),bounds.ok_or(NdiError::Stale)?,publication.live.clone(),self.luts.clone(),self.budget.clone(),scope.binding_generation,scope.binding_generation)
        } else {
            staged_delay_worker::static_mask_resources(self.masks.clone(),upstream.to_vec(),self.luts.clone(),self.budget.clone(),scope.binding_generation,scope.binding_generation)
        };
        if self.pending.len()>=self.max_pending || self.pending.try_reserve(1).is_err() || self.submitted.try_reserve(1).is_err() {return Err(NdiError::Limit("NDI staged metadata queue"));}
        let sample=Sample {scope,sequence:frame.sequence,timestamp_ns:timestamp,qpc_ns:None,directshow_clock:None,
            requested_at:frame.requested_at,dropped_frames:0,image:frame.image.clone(),ingress:skylive_media::SourceBlend::LinearSrgb,
            revision:scope.binding_generation,metadata_credit:credit,scroll,prepare,stages};
        match self.worker.try_submit(sample) {
            Ok(())=>{self.awaiting_since=None;self.submitted.push_back(frame.sequence);self.pending.push_back(Metadata {sequence:frame.sequence,ndi:frame.ndi,requested_at:frame.requested_at,
                metadata:frame.metadata,timestamp,clock:frame.native_clock.ok_or(NdiError::Invalid("NDI staged sender clock absent"))?,
                mapped:frame.mapped_media_ns,estimated:frame.mapped_time_estimated});},
            Err("camera stage input queue is full")=>{self.awaiting_since=None;},
            Err(_)=>return Err(NdiError::Unavailable("NDI stage worker is unavailable")),
        }
        Ok(())
    }

    /// Drain one completed staged sample. The exact metadata accompanies its
    /// original sequence; an old sample can never borrow the latest PTS.
    pub fn poll(&mut self,scope:Scope,upstream:&[skylive_core::VideoFilter],filters:&[(skylive_core::VideoFilterId,u16)],
        history_generation:u64,history_live:&Arc<AtomicBool>,live_epoch:&Arc<AtomicU64>,latest_timestamp:i128)->Result<Option<NdiFrame>,NdiError> {
        let Some(event)=self.worker.try_recv() else{return Ok(None);};
        let submitted=self.submitted.pop_front().ok_or(NdiError::Invalid("NDI staged job metadata mismatch"))?;
        let output=match event {Event::Output(output)=>output,Event::Skipped {..}=>{
                if let Some(index)=self.pending.iter().position(|sample|sample.sequence==submitted){self.pending.remove(index);}return Ok(None);
            },
            Event::Failed {..}=>return Err(NdiError::Unavailable("NDI staged processing failed"))};
        let Some(delayed)=output.frame else{return Ok(None);};
        let sequence=delayed.sequence().ok_or(NdiError::Invalid("NDI staged sequence absent"))?;
        while self.pending.front().is_some_and(|m|m.sequence<sequence){self.pending.pop_front();}
        let sample=self.pending.pop_front().filter(|m|m.sequence==sequence).ok_or(NdiError::Invalid("NDI staged metadata mismatch"))?;
        let timestamp=sample.timestamp;
        if delayed.scope()!=scope || delayed.timestamp_ns()!=timestamp {return Err(NdiError::Stale);}
        let elapsed_ns=latest_timestamp.checked_sub(timestamp).and_then(|n|u64::try_from(n).ok()).ok_or(NdiError::Invalid("NDI staged delay age"))?;
        let receipt=crate::capture::AsyncDelayReceipt {session:scope.session,binding_generation:scope.binding_generation,
            live_generation:Some(live_epoch.clone()),history_generation,history_live:history_live.clone(),source:scope.source,
            owner:scope.camera_owner,stream:scope.stream,clock_epoch:scope.clock_epoch,sequence,timestamp_ns:timestamp,
            ndi_native_clock:Some(sample.clock),ndi_media_time_estimate_ns:sample.estimated.then_some(sample.mapped),
            filters:filters.to_vec().into(),staged_camera:None,staged_ndi:Some(crate::capture::StagedNdiProof {
                upstream:upstream.to_vec().into(),processed:output.processed,blend:output.blend.ok_or(NdiError::Invalid("NDI staged blend proof"))?,
                generation:output.generation,live_generation:self.worker.generation_token(),mask_live:output.mask_live,mask_resources:output.mask_resources,scroll_live:output.scroll_live,
            }),elapsed_ns};
        Ok(Some(NdiFrame {stream_generation:scope.stream,ndi:sample.ndi,sequence,requested_at:sample.requested_at,
            metadata:sample.metadata,image:delayed.shared_image(),native_timestamp_ns:Some(timestamp),native_clock:Some(sample.clock),
            native_clock_epoch:scope.clock_epoch,mapped_media_ns:sample.mapped,mapped_time_estimated:sample.estimated,receive_qpc_ns:None,async_delay:Some(receipt),draft_raw:None}))
    }
}
