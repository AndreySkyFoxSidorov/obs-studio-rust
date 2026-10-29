//! Dimension-only Core target resolution and pure Media plan construction.
use super::*;
pub(super) fn canvas_key(filters:&[skylive_core::VideoFilter],providers:FilterProviders)->Option<(u32,u32)>{
    filters.iter().any(|filter|filter.enabled&&providers.enabled(filter.plugin_id())&&matches!(filter.effect,VideoEffect::ScaleFilter{target:skylive_core::ScaleFilterTarget::Canvas,..})).then_some(providers.canvas).flatten()
}
pub(crate) fn plan(effect:VideoEffect,input:(u32,u32),canvas:Option<(u32,u32)>)->Result<Option<skylive_media::ScaleFilterPlan>,FilterError>{
    let VideoEffect::ScaleFilter{target,sampling,undistort}=effect else{return Err(fail("not a Scaling/Aspect Ratio effect"));};
    let output=target.output(input,canvas).map_err(|error|fail(error.to_string()))?;
    if target.skips(input){return Ok(None);}
    let filter=match sampling{skylive_core::ScaleFilterSampling::Point=>skylive_media::SamplingFilter::Nearest,skylive_core::ScaleFilterSampling::Bilinear=>skylive_media::SamplingFilter::Bilinear,skylive_core::ScaleFilterSampling::Bicubic=>skylive_media::SamplingFilter::Bicubic,skylive_core::ScaleFilterSampling::Lanczos=>skylive_media::SamplingFilter::Lanczos,skylive_core::ScaleFilterSampling::Area=>skylive_media::SamplingFilter::Area};
    let factor=if undistort&&sampling.allows_undistort(){
        let aspect=match target{skylive_core::ScaleFilterTarget::Aspect{width,height}=>f64::from(width)/f64::from(height),_=>f64::from(output.0)/f64::from(output.1)};
        Some(aspect/(f64::from(input.0)/f64::from(input.1)))
    }else{None};
    skylive_media::ScaleFilterPlan::prepare(input,output,filter,factor).map(Some).map_err(media_fail)
}
