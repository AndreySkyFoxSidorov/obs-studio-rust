//! Actual injected point-stage execution. No authored effect/provider changes.
use super::*;
use crate::gpu_color_correction::{Client, Error, Runtime};

#[derive(Clone, Copy, Debug)]
pub enum GpuFailurePolicy { RequireGpu, PreferGpu }
#[derive(Clone)]
pub(crate) enum PointClient { Direct(Client), Shared(Runtime) }
impl From<Client> for PointClient {fn from(value:Client)->Self {Self::Direct(value)}}
impl From<Runtime> for PointClient {fn from(value:Runtime)->Self {Self::Shared(value)}}
impl PointClient {
    fn preflight_lut(&self,dimensions:(u32,u32),table_bytes:usize,settings:skylive_media::ColorLutSettings,limits:skylive_media::ColorLutLimits,free:usize)->Result<usize,Error>{match self{
        Self::Direct(client)=>client.preflight_lut(dimensions,table_bytes,settings,limits,free),
        Self::Shared(runtime)=>runtime.preflight_lut(dimensions,table_bytes,settings,limits,free),
    }}
    fn apply_lut(&self,input:skylive_media::SharedVideoFrame,blend:SourceBlend,lease:crate::gpu_color_correction::LutLease,settings:skylive_media::ColorLutSettings,limits:skylive_media::ColorLutLimits,free:usize)->Result<(skylive_media::SourceEffectFrame,crate::gpu_color_correction::Report),Error>{match self{
        Self::Direct(client)=>client.apply_lut(input,blend,lease,settings,limits,free),
        Self::Shared(runtime)=>runtime.apply_lut(input,blend,lease,settings,limits,free),
    }}
    pub(crate) fn identity(&self)->[u64;2] {match self {Self::Direct(client)=>[1,client.identity()],Self::Shared(runtime)=>[2,runtime.identity()]}}
    pub(crate) fn inflight(&self)->usize {match self {Self::Direct(client)=>client.inflight(),Self::Shared(runtime)=>runtime.inflight()}}

    fn preflight_point(&self,dimensions:(u32,u32),effect:SourceVideoEffect,limits:SourceEffectsLimits,free_cpu_bytes:usize)->Result<usize,Error>{match self{
        Self::Direct(client)=>client.preflight_point(dimensions,effect,limits,free_cpu_bytes),
        Self::Shared(runtime)=>runtime.preflight_point(dimensions,effect,limits,free_cpu_bytes),
    }}
    fn preflight_person_matte(&self,dimensions:(u32,u32),threshold:f32,softness:f32,
        limits:skylive_media::PersonMatteLimits,free_cpu_bytes:usize)->Result<usize,Error> {match self {
        Self::Direct(client)=>client.preflight_person_matte(dimensions,threshold,softness,limits,free_cpu_bytes),
        Self::Shared(runtime)=>runtime.preflight_person_matte(dimensions,threshold,softness,limits,free_cpu_bytes),
    }}
    fn apply_person_matte(&self,input:skylive_media::SharedVideoFrame,blend:SourceBlend,mask:&[f32],threshold:f32,softness:f32,
        limits:skylive_media::PersonMatteLimits,free_cpu_bytes:usize)->Result<(skylive_media::SourceEffectFrame,crate::gpu_color_correction::Report),Error> {match self {
        Self::Direct(client)=>client.apply_person_matte(input,blend,mask,threshold,softness,limits,free_cpu_bytes),
        Self::Shared(runtime)=>runtime.apply_person_matte(input,blend,mask,threshold,softness,limits,free_cpu_bytes),
    }}
    fn apply_craft(&self,input:skylive_media::SharedVideoFrame,blend:SourceBlend,effects:&[SourceVideoEffect],
        limits:SourceEffectsLimits,free_cpu_bytes:usize)->Result<(skylive_media::SourceEffectFrame,crate::gpu_color_correction::Report),Error>{match self{
        Self::Direct(client)=>client.apply_craft(input,blend,effects,limits,free_cpu_bytes),
        Self::Shared(runtime)=>runtime.apply_craft(input,blend,effects,limits,free_cpu_bytes),
    }}
    fn apply_point_run(&self,input:skylive_media::SharedVideoFrame,blend:SourceBlend,effects:&[SourceVideoEffect],limits:SourceEffectsLimits,free:usize)->Result<(skylive_media::SourceEffectFrame,crate::gpu_color_correction::Report),Error>{match self{
        Self::Direct(client)=>client.apply_point_run(input,blend,effects,limits,free),
        Self::Shared(runtime)=>runtime.apply_point_run(input,blend,effects,limits,free),
    }}    fn apply_keyer_pair(&self,input:skylive_media::SharedVideoFrame,blend:SourceBlend,effects:&[SourceVideoEffect],limits:SourceEffectsLimits,free:usize)->Result<(skylive_media::SourceEffectFrame,crate::gpu_color_correction::Report),Error>{match self{
        Self::Direct(client)=>client.apply_keyer_pair(input,blend,effects,limits,free),
        Self::Shared(runtime)=>runtime.apply_keyer_pair(input,blend,effects,limits,free),
    }}    fn apply_point(&self,input:skylive_media::SharedVideoFrame,blend:SourceBlend,effect:SourceVideoEffect,
        limits:SourceEffectsLimits,free_cpu_bytes:usize)->Result<(skylive_media::SourceEffectFrame,crate::gpu_color_correction::Report),Error>{
        match (self,effect){
            (Self::Direct(client),SourceVideoEffect::ColorCorrection(settings))=>client.apply(input,blend,settings,limits,free_cpu_bytes),
            (Self::Shared(runtime),SourceVideoEffect::ColorCorrection(settings))=>runtime.apply(input,blend,settings,limits,free_cpu_bytes),
            (Self::Direct(client),SourceVideoEffect::ColorKey(_)|SourceVideoEffect::ChromaKey(_)|SourceVideoEffect::LumaKey(_)|SourceVideoEffect::Sharpen(_))=>client.apply_keyer(input,blend,effect,limits,free_cpu_bytes),
            (Self::Shared(runtime),SourceVideoEffect::ColorKey(_)|SourceVideoEffect::ChromaKey(_)|SourceVideoEffect::LumaKey(_)|SourceVideoEffect::Sharpen(_))=>runtime.apply_keyer(input,blend,effect,limits,free_cpu_bytes),
            _=>Err(Error::Unsupported("point stage has no GPU compute kernel")),
        }
    }

}
#[derive(Clone)]
pub(crate) struct GpuPointConfig {pub client:PointClient,pub policy:GpuFailurePolicy}

impl FilterCache {
    pub(super) fn gpu_lut_peak(&self,dimensions:(u32,u32),table_bytes:usize,settings:skylive_media::ColorLutSettings,limits:skylive_media::ColorLutLimits,free:usize)->Result<usize,FilterError>{
        let Some(gpu)=&self.gpu else{return Ok(0);};
        match gpu.client.preflight_lut(dimensions,table_bytes,settings,limits,free){
            Ok(peak)=>Ok(peak),Err(error)if matches!(gpu.policy,GpuFailurePolicy::PreferGpu)&&safe_preflight_fallback(&error)=>Ok(0),Err(error)=>Err(map_error(error)),
        }
    }
    pub(super) fn apply_lut_output(&mut self,input:impl Into<skylive_media::SharedVideoFrame>,blend:SourceBlend,table:Arc<skylive_media::ColorLut>,retention:Arc<dyn crate::gpu_color_correction::LutRetention>,settings:skylive_media::ColorLutSettings,limits:skylive_media::ColorLutLimits,free:usize)->Result<RgbaFrame,FilterError>{
        let input=input.into();
        // Resolve interpolation and shaper semantics are CPU-only; never
        // submit them to the single-texture OBS GPU shader.
        if table.needs_cpu_resolve(){
            if self.gpu.as_ref().is_some_and(|gpu|matches!(gpu.policy,GpuFailurePolicy::RequireGpu)){
                return Err(map_error(Error::Unsupported("Resolve CUBE requires CPU processing")));
            }
            return skylive_media::apply_color_lut(&input,blend,&table,settings,limits).map_err(media_fail);
        }
        if let Some(gpu)=&self.gpu{
            // This check is still before this frame's native submission. Do
            // not infer the same safety from a later Closed response.
            if matches!(gpu.policy,GpuFailurePolicy::PreferGpu) {
                if let Err(error)=gpu.client.preflight_lut((input.width(),input.height()),table.byte_len(),settings,limits,free) {
                    if safe_preflight_fallback(&error) {
                        self.gpu_last_error=Some(error);
                        return skylive_media::apply_color_lut(&input,blend,&table,settings,limits).map_err(media_fail);
                    }
                }
            }
            let lease=crate::gpu_color_correction::LutLease::new(table.clone(),retention);
            match gpu.client.apply_lut(input.clone(),blend,lease,settings,limits,free){
                Ok((output,report))=>{self.gpu_calls+=1;self.gpu_report=Some(report);return Ok(output.image);},
                Err(error)=>{self.gpu_last_error=Some(error.clone());if matches!(gpu.policy,GpuFailurePolicy::RequireGpu)||!safe_point_fallback(&error){return Err(map_error(error));}}
            }
        }
        skylive_media::apply_color_lut(&input,blend,&table,settings,limits).map_err(media_fail)
    }
    pub(crate) fn with_gpu(client:impl Into<PointClient>,policy:GpuFailurePolicy)->Self {
        Self {gpu:Some(GpuPointConfig {client:client.into(),policy}),..Default::default()}
    }
    pub(super) fn gpu_person_peak(&self,dimensions:(u32,u32),threshold:f32,softness:f32,
        limits:skylive_media::PersonMatteLimits,free_cpu_bytes:usize)->Result<usize,FilterError> {
        let Some(gpu)=&self.gpu else{return Ok(0);};
        match gpu.client.preflight_person_matte(dimensions,threshold,softness,limits,free_cpu_bytes) {
            Ok(peak)=>Ok(peak),
            Err(error) if matches!(gpu.policy,GpuFailurePolicy::PreferGpu) && safe_preflight_fallback(&error)=>Ok(0),
            Err(error)=>Err(map_error(error)),
        }
    }
    pub(super) fn apply_person_output(&mut self,input:impl Into<skylive_media::SharedVideoFrame>,blend:SourceBlend,mask:&[f32],
        threshold:f32,softness:f32,limits:skylive_media::PersonMatteLimits,free_cpu_bytes:usize)->Result<RgbaFrame,FilterError> {
        let input=input.into();
        if let Some(gpu)=&self.gpu {
            if matches!(gpu.policy,GpuFailurePolicy::PreferGpu) {
                if let Err(error)=gpu.client.preflight_person_matte((input.width(),input.height()),threshold,softness,limits,free_cpu_bytes) {
                    if safe_preflight_fallback(&error) {
                        self.gpu_last_error=Some(error);
                        return skylive_media::apply_person_matte(&input,blend,mask,threshold,softness,limits).map_err(|error|fail(error.to_string()));
                    }
                }
            }
            match gpu.client.apply_person_matte(input.clone(),blend,mask,threshold,softness,limits,free_cpu_bytes) {
                Ok((output,report))=>{self.gpu_calls+=1;self.gpu_report=Some(report);return Ok(output.image);}
                Err(error)=>{
                    self.gpu_last_error=Some(error.clone());
                    // Only outcomes which declined this request before native
                    // submission may execute CPU fallback. A timed-out request
                    // still owns its payload/credits until real retirement.
                    if matches!(gpu.policy,GpuFailurePolicy::RequireGpu)||!safe_matte_fallback(&error) {
                        return Err(map_error(error));
                    }
                }
            }
        }
        skylive_media::apply_person_matte(&input,blend,mask,threshold,softness,limits).map_err(|error|fail(error.to_string()))
    }
    /// Metadata only; no native service, pixel allocation or inference. The
    /// mask fixes every early-declined stage as CPU before execution starts.
    fn point_plan(&self,dimensions:(u32,u32),effects:&[SourceVideoEffect],limits:SourceEffectsLimits,free_cpu_bytes:usize)->Result<PointPlan,FilterError>{
        if effects.len()>skylive_core::MAX_VIDEO_FILTERS || effects.len()>u64::BITS as usize {
            return Err(limited("ordered point-stage filter count"));
        }
        let whole=skylive_media::source_effect_requirements_for_dimensions(dimensions,effects).map_err(media_fail)?;
        if whole.output_bytes>limits.max_output_bytes||whole.workspace_bytes>limits.max_workspace_bytes
            ||whole.pixel_stages>limits.max_pixel_stages||whole.texel_visits>limits.max_texel_visits{
            return Err(limited("ordered point-stage CPU admission"));
        }
        let cpu_peak=whole.output_bytes.checked_add(whole.workspace_bytes).ok_or_else(||limited("CPU point peak overflow"))?;
        let Some(gpu)=&self.gpu else{return if cpu_peak<=free_cpu_bytes{Ok(PointPlan{peak:cpu_peak,gpu:0,pairs:false,run_limit:0,declined:None})}else{Err(limited("CPU point peak"))};};
        let mut mask=0u64;let mut declined=None;let mut native_peaks=[0usize;skylive_core::MAX_VIDEO_FILTERS];let mut current=dimensions;
        let mut index=0usize;
        while index<effects.len(){
            let effect=effects[index];let mut end=index+1;
            // Choose the backend for a complete Craft run together. Splitting
            // an early-declined run would introduce a new byte-rounding edge.
            if matches!(effect,SourceVideoEffect::Craft(_)){
                while end<effects.len()&&(matches!(effects[end],SourceVideoEffect::Craft(_))||neutral_spatial(effects[end])){end+=1;}
            }
            let required=skylive_media::source_effect_requirements_for_dimensions(current,&effects[index..end]).map_err(media_fail)?;
            let supported_run=supported(effect);
            if supported_run{
                let held=if index==0{0}else{frame_bytes(current)?};
                match gpu.client.preflight_point(current,effect,stage_limits(&required),free_cpu_bytes.saturating_sub(held)){
                    Ok(peak)=>{for stage in index..end{mask|=1u64<<stage;native_peaks[stage]=peak;}},
                    Err(error)if matches!(gpu.policy,GpuFailurePolicy::PreferGpu)&&safe_preflight_fallback(&error)=>{if declined.is_none(){declined=Some(error);}},
                    Err(error)=>return Err(map_error(error)),
                }
            }else if matches!(gpu.policy,GpuFailurePolicy::RequireGpu){
                return Err(map_error(Error::Unsupported("RequireGpu source chain contains a CPU-only point filter")));
            }
            current=(required.final_width,required.final_height);index=end;
        }
        if mask==0{return if cpu_peak<=free_cpu_bytes{Ok(PointPlan{peak:cpu_peak,gpu:0,pairs:false,run_limit:0,declined})}else{Err(limited("CPU point peak"))};}
        let mut peak=cpu_peak;let mut start=0usize;let mut current=dimensions;let mut intermediate=false;
        for index in 0..=effects.len(){
            if index<effects.len()&&mask&(1u64<<index)==0{continue;}
            if start<index{
                let required=skylive_media::source_effect_requirements_for_dimensions(current,&effects[start..index]).map_err(media_fail)?;
                let held=if intermediate{frame_bytes(current)?}else{0};
                peak=peak.max(held.checked_add(required.output_bytes).and_then(|value|value.checked_add(required.workspace_bytes)).ok_or_else(||limited("ordered CPU segment peak overflow"))?);
                current=(required.final_width,required.final_height);intermediate=true;
            }
            if index==effects.len(){break;}
            let effect=effects[index];let required=skylive_media::source_effect_requirements_for_dimensions(current,&[effect]).map_err(media_fail)?;
            let held=if intermediate{frame_bytes(current)?}else{0};
            // The fallback CPU footprint is also admitted, but any timeout or
            // submitted failure will remain an error rather than use it.
            let cpu=required.output_bytes.checked_add(required.workspace_bytes).ok_or_else(||limited("point fallback peak overflow"))?;
            let native=native_peaks[index];
            peak=peak.max(held.checked_add(cpu.max(native)).ok_or_else(||limited("ordered GPU segment peak overflow"))?);
            current=(required.final_width,required.final_height);intermediate=true;start=index+1;
        }
        if peak>free_cpu_bytes{return Err(limited("ordered GPU source intermediates/staging exceed admission"));}
        // A pair adds one bounded uniform staging upload. Reserve it only if
        // credit is available; otherwise preserve the existing individual path.
        let pairs=(0..effects.len().saturating_sub(1)).any(|i|mask&(3u64<<i)==(3u64<<i)&&run_stage(effects[i])&&run_stage(effects[i+1]));
        let pairs=pairs&&peak.checked_add(96).is_some_and(|value|value<=free_cpu_bytes);
        if pairs {peak+=96;}
        let mut longest=0usize;let mut current_run=0usize;
        for (index,effect) in effects.iter().enumerate(){
            if mask&(1u64<<index)!=0 && run_stage(*effect){current_run+=1;longest=longest.max(current_run);}else{current_run=0;}
        }
        let mut run_limit=if pairs{2}else{0};
        if pairs {for length in 3..=longest {
            let extra=(length-2)*96;
            if peak.checked_add(extra).is_some_and(|value|value<=free_cpu_bytes){run_limit=length;}else{break;}
        }}
        if run_limit>2 {peak+=(run_limit-2)*96;}
        Ok(PointPlan{peak,gpu:mask,pairs,run_limit,declined})
    }
    pub(super) fn gpu_points_peak(&self,dimensions:(u32,u32),effects:&[SourceVideoEffect],limits:SourceEffectsLimits,free_cpu_bytes:usize)->Result<usize,FilterError>{
        self.point_plan(dimensions,effects,limits,free_cpu_bytes).map(|plan|plan.peak)
    }
    pub(super) fn apply_points(&mut self,input:impl Into<skylive_media::SharedVideoFrame>,ingress:SourceBlend,effects:&[SourceVideoEffect],limits:SourceEffectsLimits,free_cpu_bytes:usize)->Result<skylive_media::SourceEffectFrame,FilterError>{
        let input=input.into();
        let plan=self.point_plan((input.width(),input.height()),effects,limits,free_cpu_bytes)?;
        // A wholly neutral chain is an exact byte identity,
        // including RGB underneath alpha zero; the shared GPU shader normally
        // normalizes those pixels, so retain the CPU identity path.
        if !effects.is_empty() && effects.iter().all(|stage|
            matches!(stage, SourceVideoEffect::Craft(effect) if effect.is_passthrough()) || neutral_spatial(*stage)) {
            return skylive_media::source_video_effects(&input,ingress,effects,limits).map_err(media_fail);
        }
        if let Some(error)=plan.declined.clone(){self.gpu_last_error=Some(error);}
        if plan.gpu==0{return skylive_media::source_video_effects(&input,ingress,effects,limits).map_err(media_fail);}
        let gpu=self.gpu.clone().ok_or_else(||fail("GPU point plan lost its backend"))?;
        let mut current=input.clone();let mut last_output:Option<Arc<RgbaFrame>>=None;let mut blend=ingress;let mut start=0usize;let mut next=0usize;
        for index in 0..=effects.len(){
            if index<next{continue;}
            if index<effects.len()&&plan.gpu&(1u64<<index)==0{continue;}
            if start<index{
                let required=skylive_media::source_effect_requirements(&current,&effects[start..index]).map_err(media_fail)?;
                let held=if current.shares_pixels_with(&input){0}else{current.byte_len()};
                if required.output_bytes.checked_add(required.workspace_bytes).is_none_or(|peak|peak>free_cpu_bytes.saturating_sub(held)){return Err(limited("ordered CPU segment changed its footprint"));}
                let output=skylive_media::source_video_effects(&current,blend,&effects[start..index],stage_limits(&required)).map_err(media_fail)?;
                blend=output.blend;let next_frame=Arc::new(output.image);current=next_frame.clone().into();last_output=Some(next_frame);
            }
            if index==effects.len(){break;}
            if plan.run_limit>=2 && run_stage(effects[index]) {
                let mut end=index;
                while end<effects.len() && end-index<plan.run_limit && plan.gpu&(1u64<<end)!=0 && run_stage(effects[end]) {end+=1;}
                if end-index>=2 {
                    let run=&effects[index..end];
                    let required=skylive_media::source_effect_requirements(&current,run).map_err(media_fail)?;
                    let held=if current.shares_pixels_with(&input){0}else{current.byte_len()};
                    match gpu.client.apply_point_run(current.clone(),blend,run,stage_limits(&required),free_cpu_bytes.saturating_sub(held)) {
                        Ok((output,report))=>{self.gpu_calls+=1;self.gpu_report=Some(report);blend=output.blend;let next_frame=Arc::new(output.image);current=next_frame.clone().into();last_output=Some(next_frame);start=end;next=end;continue;}
                        Err(error) if safe_point_fallback(&error)=>{
                            self.gpu_last_error=Some(error.clone());
                            if first_service_decline(index,gpu.policy,&error) {
                                // No segment has run at index zero. The full
                                // CPU footprint was admitted by point_plan;
                                // do not retry a busy/retired service per stage.
                                return skylive_media::source_video_effects(&input,ingress,effects,limits).map_err(media_fail);
                            }
                        }
                        Err(error)=>{self.gpu_last_error=Some(error.clone());return Err(map_error(error));}
                    }
                }
            }
            if plan.pairs && index+1<effects.len() && plan.gpu&(3u64<<index)==(3u64<<index)
                && pair_keyer(effects[index]) && pair_keyer(effects[index+1]) {
                let run=&effects[index..index+2];
                let required=skylive_media::source_effect_requirements(&current,run).map_err(media_fail)?;
                let held=if current.shares_pixels_with(&input){0}else{current.byte_len()};
                match gpu.client.apply_keyer_pair(current.clone(),blend,run,stage_limits(&required),free_cpu_bytes.saturating_sub(held)) {
                    Ok((output,report))=>{self.gpu_calls+=1;self.gpu_report=Some(report);blend=output.blend;let next_frame=Arc::new(output.image);current=next_frame.clone().into();last_output=Some(next_frame);start=index+2;next=index+2;continue;}
                    Err(error) if safe_point_fallback(&error)=>{
                        self.gpu_last_error=Some(error.clone());
                        if first_service_decline(index,gpu.policy,&error) {
                            return skylive_media::source_video_effects(&input,ingress,effects,limits).map_err(media_fail);
                        }
                    }
                    Err(error)=>{self.gpu_last_error=Some(error.clone());return Err(map_error(error));}
                }
            }
            let effect=effects[index];let mut end=index+1;
            // Preserve the media kernel's floating-point boundary: adjacent
            // Craft effects form one dispatch and one final byte conversion.
            if matches!(effect,SourceVideoEffect::Craft(_)){
                while end<effects.len()&&plan.gpu&(1u64<<end)!=0&&(matches!(effects[end],SourceVideoEffect::Craft(_))||neutral_spatial(effects[end])){end+=1;}
            }
            let run=&effects[index..end];
            let required=skylive_media::source_effect_requirements(&current,run).map_err(media_fail)?;
            let held=if current.shares_pixels_with(&input){0}else{current.byte_len()};let free=free_cpu_bytes.saturating_sub(held);
            let applied=if matches!(effect,SourceVideoEffect::Craft(_)){
                // Neutral spatial stages do not introduce a rounding boundary
                // in the CPU kernel. Leave them out of one fused Craft upload.
                let mut craft=[effect;skylive_core::MAX_VIDEO_FILTERS];
                let mut count=0;
                for stage in run.iter().copied().filter(|stage|matches!(stage,SourceVideoEffect::Craft(_))) {
                    craft[count]=stage;count+=1;
                }
                gpu.client.apply_craft(current.clone(),blend,&craft[..count],stage_limits(&required),free)
            }else{gpu.client.apply_point(current.clone(),blend,effect,stage_limits(&required),free)};
            let output=match applied{
                Ok((output,report))=>{self.gpu_calls+=1;self.gpu_report=Some(report);output}
                Err(error)=>{
                    self.gpu_last_error=Some(error.clone());
                    if matches!(gpu.policy,GpuFailurePolicy::RequireGpu)||!safe_point_fallback(&error){return Err(map_error(error));}
                    if first_service_decline(index,gpu.policy,&error) {
                        return skylive_media::source_video_effects(&input,ingress,effects,limits).map_err(media_fail);
                    }
                    skylive_media::source_video_effects(&current,blend,run,stage_limits(&required)).map_err(media_fail)?
                }
            };
            blend=output.blend;let next_frame=Arc::new(output.image);current=next_frame.clone().into();last_output=Some(next_frame);start=end;next=end;
        }
        // The last buffer is owned solely here. Never clone a full-resolution
        // frame merely to remove its Arc wrapper at the existing API boundary.
        drop(current);
        let image=Arc::try_unwrap(last_output.ok_or_else(||fail("ordered GPU plan produced no output"))?).map_err(|_|fail("ordered GPU output retained an unexpected alias"))?;
        Ok(skylive_media::SourceEffectFrame{image,blend})
    }

}
struct PointPlan{peak:usize,gpu:u64,pairs:bool,run_limit:usize,declined:Option<Error>}
fn neutral_spatial(effect:SourceVideoEffect)->bool {
    matches!(effect,SourceVideoEffect::Vignette(settings) if settings.is_identity())
        ||matches!(effect,SourceVideoEffect::GaussianBlur(settings) if settings.is_identity())
}
fn run_stage(effect:SourceVideoEffect)->bool{matches!(effect,SourceVideoEffect::ColorCorrection(_))||pair_keyer(effect)}
fn pair_keyer(effect:SourceVideoEffect)->bool{matches!(effect,SourceVideoEffect::ColorKey(_)|SourceVideoEffect::ChromaKey(_)|SourceVideoEffect::LumaKey(_)|SourceVideoEffect::Sharpen(_))}
fn supported(effect:SourceVideoEffect)->bool{matches!(effect,SourceVideoEffect::Craft(_)|SourceVideoEffect::ColorCorrection(_)|SourceVideoEffect::ColorKey(_)|SourceVideoEffect::ChromaKey(_)|SourceVideoEffect::LumaKey(_)|SourceVideoEffect::Sharpen(_))}
fn frame_bytes(dimensions:(u32,u32))->Result<usize,FilterError>{(dimensions.0 as usize).checked_mul(dimensions.1 as usize).and_then(|value|value.checked_mul(4)).ok_or_else(||limited("point frame dimensions overflow"))}
fn stage_limits(required:&skylive_media::SourceEffectRequirements)->SourceEffectsLimits{SourceEffectsLimits{max_output_bytes:required.output_bytes,max_workspace_bytes:required.workspace_bytes,max_pixel_stages:required.pixel_stages,max_texel_visits:required.texel_visits}}
fn first_service_decline(index:usize,policy:GpuFailurePolicy,error:&Error)->bool {
    // A run-specific Limit/Unsupported can still succeed as smaller GPU jobs.
    // Once any earlier segment ran, its output/order must remain authoritative.
    index==0 && matches!(policy,GpuFailurePolicy::PreferGpu)
        && matches!(error,Error::Busy|Error::Initialization(_)|Error::NotSubmitted(_))
}
fn safe_point_fallback(error:&Error)->bool{
    // Every per-frame Limit occurs before that frame is dispatched;
    // device/map/timeout/cancel/stale results never fall back to another owner.
    matches!(error,Error::Busy|Error::Unsupported(_)|Error::Initialization(_)|Error::Limit(_)|Error::NotSubmitted(_))
}
fn safe_preflight_fallback(error:&Error)->bool {
    // Preflight is metadata-only: this particular request has not retained
    // a native job or submitted GPU work. A closed owner therefore permits a
    // bounded CPU plan here even though Closed after submission is ambiguous.
    matches!(error,Error::Closed|Error::Device(_))||safe_point_fallback(error)
}
fn safe_matte_fallback(error:&Error)->bool {
    matches!(error,Error::Busy|Error::Unsupported(_)|Error::Initialization(_)|Error::Limit(_)|Error::NotSubmitted(_))
}
fn map_error(error:Error)->FilterError {FilterError {kind:match error.underlying() {Error::Limit(_)=>crate::resource_issue::ResourceIssueKind::Limit,Error::Invalid(_)|Error::Stale=>crate::resource_issue::ResourceIssueKind::Invalid,_=>crate::resource_issue::ResourceIssueKind::Unavailable},code:error.code(),detail:error.to_string()}}

// The no-GPU build retains the identical pure CPU call through a small adapter.
// This module itself is enabled only with the native GPU feature.
