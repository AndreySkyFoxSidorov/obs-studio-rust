//! Ordered neural/point stages on the existing background render owner.
//! Admission covers every temporary before inference or pixel allocation.
use super::*;

pub(super) struct PersonPlan {pub work:usize,pub texels:u64,pub workspace:usize,pub neural_work:usize,pub peak:usize,pub max_frame_bytes:usize,pub final_dimensions:(u32,u32)}
impl FilterCache {
    fn scoped_person_mask(&mut self,store:&Arc<std::sync::Mutex<shared::person_cache::Store>>,
        context:TemporalContext,id:u64,interval_ms:u16,input:&RgbaFrame,blend:SourceBlend,
        limits:skylive_media::PersonMatteLimits)->Result<Arc<shared::person_cache::Mask>,FilterError>{
        let dimensions=(input.width(),input.height());let backend=self.person_backend_signature();
        let lookup=store.lock().map_err(|_|fail("Person mask store poisoned"))?.lookup(context,id,interval_ms,dimensions,blend,self.resource_generation,self.mask_epoch,self.scroll_context.key,backend)?;
        match lookup {shared::person_cache::Lookup::Hit(mask)=>return Ok(mask),shared::person_cache::Lookup::Miss(lease)=>drop(lease)}
        // Only misses wait for another inference. The independent gate keeps
        // valid cached masks for other sources available while the model runs.
        let gate=self.person_inference_gate.clone().ok_or_else(||fail("Person inference owner absent"))?;
        let _serial=gate.lock().map_err(|_|fail("Person inference owner poisoned"))?;
        let lookup=store.lock().map_err(|_|fail("Person mask store poisoned"))?.lookup(context,id,interval_ms,dimensions,blend,self.resource_generation,self.mask_epoch,self.scroll_context.key,backend)?;
        let lease=match lookup {shared::person_cache::Lookup::Hit(mask)=>return Ok(mask),shared::person_cache::Lookup::Miss(lease)=>lease};
        let rgb=skylive_media::prepare_person_rgb(input,blend,limits).map_err(media_fail)?;
        let inference=self.infer_person(&rgb).map_err(|error|FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"person_background_unavailable",detail:error.to_string()})?;
        drop(rgb);
        let mask=store.lock().map_err(|_|fail("Person mask store poisoned"))?.publish(context,id,interval_ms,dimensions,blend,self.resource_generation,self.mask_epoch,self.scroll_context.key,backend,inference.probabilities,lease)?;
        Ok(mask)
    }
    /// Apply a neural matte while keeping the incoming delayed-frame lease
    /// alive for the entire inference and synchronous CPU composition. When
    /// a serial person-mask owner is present, its existing scoped history is
    /// reused; otherwise inference runs for every frame, as in the ordinary
    /// uncached path, and no cross-frame history is claimed.
    pub(super) fn apply_delayed_person(&mut self,filter:&skylive_core::VideoFilter,
        input:skylive_media::SharedVideoFrame,blend:SourceBlend,free_bytes:usize,free_work:usize)
        ->Result<SharedFilterResult,FilterError>{
        let Some(FilterStage::Person {threshold,softness,id,interval_ms})=prepare_filter(filter).1 else {
            return Err(fail("not a person background filter"));
        };
        let pixels=(input.width() as usize).checked_mul(input.height() as usize).ok_or_else(||limited("delayed person pixel overflow"))?;
        let frame_bytes=pixels.checked_mul(4).ok_or_else(||limited("delayed person frame overflow"))?;
        let scratch=(196_608usize+65_536).checked_mul(size_of::<f32>()).ok_or_else(||limited("delayed person scratch overflow"))?;
        let peak=frame_bytes.checked_add(scratch).ok_or_else(||limited("delayed person allocation overflow"))?;
        let work=pixels.checked_mul(5).and_then(|n|n.checked_add(5*65_536)).ok_or_else(||limited("delayed person work overflow"))?;
        let texels=u64::try_from(work).map_err(|_|limited("delayed person work overflow"))?;
        if peak>free_bytes||frame_bytes>MAX_FILTERED_BYTES.saturating_sub(self.bytes)||work>free_work||texels>MAX_TEXEL_VISITS.saturating_sub(self.texel_visits){return Err(limited("delayed person exceeds shared bytes/work budget"));}
        let limits=skylive_media::PersonMatteLimits {
            max_input_bytes:frame_bytes.checked_add(65_536*size_of::<f32>()).ok_or_else(||limited("delayed person input overflow"))?,
            max_output_bytes:frame_bytes.max(196_608*size_of::<f32>()),max_pixel_visits:texels,
        };
        self.texel_visits+=texels;
        let context=self.temporal_context.filter(|context|context.camera&&context.input_complete&&id!=0&&interval_ms!=0);
        let store=self.person_store.clone();
        let cached=if let (Some(context),Some(store))=(context,store.as_ref()){
            Some(self.scoped_person_mask(store,context,id,interval_ms,&input,blend,limits)?)
        }else{None};
        let uncached=if cached.is_none(){
            let rgb=skylive_media::prepare_person_rgb(&input,blend,limits).map_err(media_fail)?;
            let inference=self.infer_person(&rgb).map_err(|error|FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"person_background_unavailable",detail:error.to_string()})?;
            drop(rgb);Some(inference.probabilities)
        }else{None};
        let probabilities=match (&cached,&uncached){(Some(mask),_)=>mask.probabilities.as_slice(),(_,Some(mask))=>mask.as_slice(),_=>return Err(fail("Person mask result absent"))};
        let image=skylive_media::apply_person_matte(&input,blend,probabilities,threshold,softness,limits).map_err(media_fail)?;
        let image=Arc::new(image);let new_bytes=image.byte_len();
        Ok(SharedFilterResult{image:image.into(),blend,new_bytes,pixel_stages:work})
    }
    pub(super) fn person_plan(&self,input:&RgbaFrame,stages:&[FilterStage],free_bytes:usize,free_pixel_stages:usize)->Result<PersonPlan,FilterError> {
        self.person_plan_for_dimensions((input.width(),input.height()),stages,free_bytes,free_pixel_stages)
    }
    pub(super) fn person_plan_for_dimensions(&self,mut dimensions:(u32,u32),stages:&[FilterStage],free_bytes:usize,free_pixel_stages:usize)->Result<PersonPlan,FilterError> {
        let mut work=0usize;let mut texels=0u64;let mut workspace=0usize;let mut neural_work=0usize;
        let mut peak=0usize;let mut max_frame_bytes=dimensions.0 as usize*dimensions.1 as usize*4;let mut points=Vec::new();
        for (index,stage) in stages.iter().copied().chain(std::iter::once(FilterStage::Person {threshold:0.5,softness:1.0,id:0,interval_ms:0})).enumerate(){
            match stage {
                FilterStage::Point(point)=>points.push(point),
                FilterStage::Person {threshold,softness,..}=>{
                    if !points.is_empty(){
                        let required=skylive_media::source_effect_requirements_for_dimensions(dimensions,&points).map_err(media_fail)?;
                        work=work.checked_add(required.pixel_stages).ok_or_else(||limited("filter work overflow"))?;
                        texels=texels.checked_add(required.texel_visits).ok_or_else(||limited("filter sampling overflow"))?;
                        workspace=workspace.max(required.workspace_bytes);max_frame_bytes=max_frame_bytes.max(required.max_frame_bytes);
                        let old=dimensions.0 as usize*dimensions.1 as usize*4;
                        let cpu=required.output_bytes.checked_add(required.workspace_bytes).ok_or_else(||limited("filter allocation overflow"))?;
                        // Use the actual dimensions at this ordered segment.
                        // Its input remains held by this neural-chain owner.
                        let gpu=self.gpu_points_peak(dimensions,&points,SourceEffectsLimits {max_output_bytes:required.output_bytes,
                            max_workspace_bytes:required.workspace_bytes,max_pixel_stages:required.pixel_stages,max_texel_visits:required.texel_visits},free_bytes.saturating_sub(old))?;
                        let transient=old.checked_add(cpu.max(gpu)).ok_or_else(||limited("filter allocation overflow"))?;
                        peak=peak.max(transient);dimensions=(required.final_width,required.final_height);points.clear();
                    }
                    if index==stages.len(){break;}
                    let pixels=dimensions.0 as usize*dimensions.1 as usize;
                    let visits=pixels.checked_mul(5).and_then(|v|v.checked_add(5*65_536)).ok_or_else(||limited("person work overflow"))?;
                    neural_work=neural_work.checked_add(visits).ok_or_else(||limited("person work overflow"))?;
                    work=work.checked_add(visits).ok_or_else(||limited("filter work overflow"))?;
                    texels=texels.checked_add(visits as u64).ok_or_else(||limited("filter sampling overflow"))?;
                    peak=peak.max(pixels.checked_mul(8).and_then(|v|v.checked_add((196_608+65_536)*size_of::<f32>())).ok_or_else(||limited("person allocation overflow"))?);
                    // Metadata-only GPU admission must run before model input,
                    // inference or mask-payload allocation. A crop segment can
                    // use this kernel if its actual output dimensions fit.
                    let frame_bytes=pixels.checked_mul(4).ok_or_else(||limited("person frame overflow"))?;
                    let matte_limits=skylive_media::PersonMatteLimits {
                        max_input_bytes:frame_bytes.checked_add(65_536*size_of::<f32>()).ok_or_else(||limited("person mask overflow"))?,
                        max_output_bytes:frame_bytes,max_pixel_visits:visits as u64,
                    };
                    let gpu=self.gpu_person_peak(dimensions,threshold,softness,matte_limits,free_bytes.saturating_sub(frame_bytes))?;
                    peak=peak.max(frame_bytes.checked_add(gpu).ok_or_else(||limited("person retained input/staging overflow"))?);
                }
            }
        }
        let retained=dimensions.0 as usize*dimensions.1 as usize*4;
        if peak>free_bytes || retained>MAX_FILTERED_BYTES.saturating_sub(self.bytes) || work>free_pixel_stages || texels>MAX_TEXEL_VISITS.saturating_sub(self.texel_visits){return Err(limited("person filter exceeds shared output/scratch/work admission"));}
        Ok(PersonPlan {work,texels,workspace,neural_work,peak,max_frame_bytes,final_dimensions:dimensions})
    }

    fn infer_person(
        &mut self,
        input: &[f32],
    ) -> Result<skylive_person_matte::Inference, skylive_person_matte::Error> {
        #[cfg(test)]
        {
            self.neural_calls += 1;
            if let Some(error) = self.neural_error.take() {
                return Err(error);
            }
        }
        skylive_person_matte::evaluate_wait(input)
    }
    pub(super) fn apply_person_chain(
        &mut self,
        key: Key,
        input: Arc<RgbaFrame>,
        ingress: SourceBlend,
        stages: &[FilterStage],
        free_bytes: usize,
        free_pixel_stages: usize,
    ) -> Result<FilterResult, FilterError> {
        let PersonPlan {work,texels,workspace,neural_work,max_frame_bytes,..}=self.person_plan(&input,stages,free_bytes,free_pixel_stages)?;
        let mut points=Vec::new();
        let limits = skylive_media::PersonMatteLimits {
            max_input_bytes: max_frame_bytes.saturating_add(65_536 * size_of::<f32>()),
            max_output_bytes: max_frame_bytes.max(196_608 * size_of::<f32>()),
            max_pixel_visits: neural_work as u64,
        };
        // Admitted work remains charged even if inference later fails.
        self.texel_visits += texels;
        let mut current = input.clone();
        let mut blend = ingress;
        points.clear();
        for stage in stages
            .iter()
            .copied()
            .chain(std::iter::once(FilterStage::Person {
                threshold: 0.5,
                softness: 1.0,
                id:0,interval_ms:0,
            }))
            .enumerate()
        {
            let (index, stage) = stage;
            match stage {
                FilterStage::Point(point) => points.push(point),
                FilterStage::Person {
                    threshold,
                    softness,
                    id,interval_ms,
                } => {
                    if !points.is_empty() {
                        let required=skylive_media::source_effect_requirements(&current,&points).map_err(media_fail)?;
                        let segment_limits=SourceEffectsLimits {max_output_bytes:required.output_bytes,max_workspace_bytes:workspace,max_pixel_stages:work,max_texel_visits:texels};
                        let output=self.apply_points(current.clone(),blend,&points,segment_limits,free_bytes.saturating_sub(current.byte_len()))?;
                        current = Arc::new(output.image);
                        blend = output.blend;
                        points.clear();
                    }
                    if index == stages.len() {
                        break;
                    }
                    let context=self.temporal_context.filter(|context|context.camera&&context.input_complete&&id!=0&&interval_ms!=0);
                    let store=self.person_store.clone();
                    let cached=if let (Some(context),Some(store))=(context,store.as_ref()){
                        Some(self.scoped_person_mask(store,context,id,interval_ms,&current,blend,limits)?)
                    }else{None};
                    let uncached=if cached.is_none(){
                        let rgb=skylive_media::prepare_person_rgb(&current,blend,limits).map_err(media_fail)?;
                        let inference=self.infer_person(&rgb).map_err(|error|FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"person_background_unavailable",detail:error.to_string()})?;
                        drop(rgb);Some(inference.probabilities)
                    }else{None};
                    let probabilities=match (&cached,&uncached){(Some(mask),_)=>mask.probabilities.as_slice(),(_,Some(mask))=>mask.as_slice(),_=>return Err(fail("Person mask result absent"))};
                    let output = self.apply_person_output(
                        current.clone(),
                        blend,
                        probabilities,
                        threshold,
                        softness,
                        limits,
                        free_bytes.saturating_sub(current.byte_len()),
                    )?;
                    current = Arc::new(output);
                }
            }
        }
        let new_bytes = current.byte_len();
        self.bytes += new_bytes;
        self.entries.insert(
            key,
            Entry {
                _input: input,
                output: current.clone(),
                blend,
                _shared_lease:None,
            },
        );
        Ok(FilterResult {
            image: current,
            blend,
            new_bytes,
            pixel_stages: work,
        })
    }
}
