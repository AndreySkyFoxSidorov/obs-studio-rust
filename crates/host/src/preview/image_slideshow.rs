//! Current/next images only use the existing clip cache and render budgets.
use super::*;
use skylive_core::{ImageSlideShow,Source};
type Failure=(crate::resource_issue::ResourceIssueKind,&'static str,String);
fn media(error:MediaError)->Failure{(crate::resource_issue::ResourceIssueKind::Limit,"slideshow_render",error.to_string())}
pub(super) fn pin(source:&Source,playback:&PlaybackSnapshot,pinned:&mut image_cache::Pins)->Result<(),MediaError>{
    match source.kind(){
        SourceKind::Image {..}=>pinned.image(source)?,
        SourceKind::ImageSlideShow {settings}=>{
            let Ok(s)=crate::image_slideshow::sample_playback(settings,source.id(),playback)else{return Ok(());};
            if !s.hidden {
                pinned.insert(&settings.images()[s.current],false,true)?;
                if let Some(next)=s.next {pinned.insert(&settings.images()[next],false,true)?;}
            }
        },
        _=>{},
    }Ok(())
}
pub(super) fn retain(cache:&mut ClipCache,pinned:&image_cache::Pins){cache.retain_images(pinned);}
fn admit(resources:&mut RenderResources<'_>,width:u32,height:u32,visits:usize)->Result<(),Failure>{
    let bytes=width as usize*height as usize*4;
    let used=resources.cache.bytes.saturating_add(resources.cache.text.bytes).saturating_add(resources.live_bytes);
    if bytes>MAX_RENDER_RESOURCE_BYTES.saturating_sub(used)||visits>(64*1024*1024usize).saturating_sub(resources.pixel_visits){return Err(media(MediaError::BudgetExceeded));}
    resources.live_bytes+=bytes;resources.pixel_visits+=visits;Ok(())
}
fn fit(image:Arc<RgbaFrame>,width:u32,height:u32,resources:&mut RenderResources<'_>)->Result<(Arc<RgbaFrame>,SourceBlend),Failure>{
    let blend=SourceBlend::EncodedPremultipliedSrgb;if image.width()==width&&image.height()==height{return Ok((image,blend));}
    let pixels=width as usize*height as usize;admit(resources,width,height,pixels*2)?;
    let scale=(f64::from(width)/f64::from(image.width())).min(f64::from(height)/f64::from(image.height()));let w=f64::from(image.width())*scale;let h=f64::from(image.height())*scale;
    skylive_media::composite_affine_premultiplied(width,height,&[AffineLayer::new(&image,Affine2{xx:w,xy:0.0,yx:0.0,yy:h,tx:(f64::from(width)-w)/2.0,ty:(f64::from(height)-h)/2.0},UvRect::FULL,1.0).with_blend_mode(blend)],CompositeLimits{max_layers:1,max_input_bytes:MAX_RENDER_RESOURCE_BYTES,max_output_bytes:pixels*4,max_pixel_visits:(pixels*2)as u64}).map(|frame|(Arc::new(frame),SourceBlend::PremultipliedLinearSrgb)).map_err(media)
}
pub(super) fn render(settings:&ImageSlideShow,s:crate::image_slideshow::Sample,resources:&mut RenderResources<'_>,next_after:&mut Option<u64>)->Result<Option<(Arc<RgbaFrame>,SourceBlend)>,Failure>{
    if s.hidden{return Ok(None);}let(width,height)=settings.dimensions();let pixels=width as usize*height as usize;let bytes=pixels*4;
    let changing=s.next.is_some()||s.to_transparent||s.from_transparent;let directional=matches!(settings.transition(),skylive_core::SlideTransition::Swipe|skylive_core::SlideTransition::Slide);
    let fits=usize::from((settings.images()[s.current].width(),settings.images()[s.current].height())!=(width,height))+s.next.map_or(0,|next|usize::from((settings.images()[next].width(),settings.images()[next].height())!=(width,height)));
    let planned_bytes=bytes.checked_mul(fits+usize::from(changing)+usize::from(s.to_transparent||s.from_transparent)).ok_or_else(||media(MediaError::Overflow))?;
    let transition_visits=if changing{if directional{5}else{3}}else{0};
    let visits=pixels.checked_mul(fits*2+transition_visits+usize::from(s.to_transparent||s.from_transparent)).ok_or_else(||media(MediaError::Overflow))?;
    let initial_live=resources.live_bytes;let used=resources.cache.bytes.saturating_add(resources.cache.text.bytes).saturating_add(initial_live);
    if planned_bytes>MAX_RENDER_RESOURCE_BYTES.saturating_sub(used)||visits>(64*1024*1024usize).saturating_sub(resources.pixel_visits){return Err(media(MediaError::BudgetExceeded));}
    // Reserve ALL future fit/transparent/transition raster bytes from the
    // existing cache admission before either source file is read or decoded.
    let reserved=initial_live.checked_add(planned_bytes).ok_or_else(||media(MediaError::Overflow))?;
    let clip=resources.cache.load(&settings.images()[s.current],&mut resources.reads,&mut resources.decoded,reserved)?;let current=clip.sample(MediaTime(s.image_elapsed_ns));
    if let Some(after)=current.next_after_ns{*next_after=Some(next_after.map_or(after,|old|old.min(after)));}
    let(mut image,mut image_blend)=fit(current.image,width,height,resources)?;if !changing{return Ok(Some((image,image_blend)));}
    let(mut next,mut next_blend)=if let Some(index)=s.next{
        let clip=resources.cache.load(&settings.images()[index],&mut resources.reads,&mut resources.decoded,reserved)?;let sample=clip.sample(MediaTime(s.next_image_elapsed_ns));
        if let Some(after)=sample.next_after_ns{*next_after=Some(next_after.map_or(after,|old|old.min(after)));}fit(sample.image,width,height,resources)?
    }else{admit(resources,width,height,pixels)?;(Arc::new(RgbaFrame::solid(width,height,[0;4]).map_err(media)?),SourceBlend::PremultipliedLinearSrgb)};
    if s.from_transparent{std::mem::swap(&mut image,&mut next);std::mem::swap(&mut image_blend,&mut next_blend);}
    let per_pixel=if directional{5}else{3};admit(resources,width,height,pixels*per_pixel)?;let limits=CompositeLimits{max_layers:2,max_input_bytes:MAX_RENDER_RESOURCE_BYTES,max_output_bytes:bytes,max_pixel_visits:(pixels*per_pixel)as u64};
    let frame=match settings.transition(){
        skylive_core::SlideTransition::Fade=>skylive_media::crossfade_rgba_with_blend(&image,image_blend,&next,next_blend,s.fade_ns,s.fade_duration_ns,limits),
        skylive_core::SlideTransition::Swipe=>skylive_media::directional_rgba_with_blend(&image,image_blend,&next,next_blend,s.fade_ns,s.fade_duration_ns,skylive_media::DirectionalTransition::Swipe{direction:skylive_media::Direction::Left,swipe_in:false},limits),
        skylive_core::SlideTransition::Slide=>skylive_media::directional_rgba_with_blend(&image,image_blend,&next,next_blend,s.fade_ns,s.fade_duration_ns,skylive_media::DirectionalTransition::Slide{direction:skylive_media::Direction::Left},limits),
        skylive_core::SlideTransition::Cut=>return Err(media(MediaError::InvalidTransition)),
    }.map_err(media)?;
    Ok(Some((Arc::new(frame),SourceBlend::PremultipliedLinearSrgb)))
}
