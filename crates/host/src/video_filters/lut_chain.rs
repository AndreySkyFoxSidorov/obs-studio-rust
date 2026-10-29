//! Separate bounded LUT table holder and whole ordered resource-chain planning.
use super::*;
use skylive_core::{LutAsset,LutFormat,VideoFilter};
use std::sync::{Mutex,atomic::{AtomicUsize,Ordering}};
const TABLE_BYTES:usize=64*1024*1024;const TABLE_ENTRIES:usize=32;
struct Credit {budget:Arc<AtomicUsize>,bytes:usize}
impl Drop for Credit {fn drop(&mut self){self.budget.fetch_sub(self.bytes,Ordering::AcqRel);}}
struct Table {value:Arc<skylive_media::ColorLut>,_credit:Credit}
#[cfg(feature="gpu-filters")]
impl crate::gpu_color_correction::LutRetention for Table {}
struct Loaded {value:Result<Arc<Table>,FilterError>,epoch:u64,generation:u64,touched:u64}
pub(crate) struct LutStore {entries:HashMap<LutAsset,Loaded>,budget:Arc<AtomicUsize>,clock:u64}
impl Default for LutStore {fn default()->Self{Self {entries:HashMap::new(),budget:Arc::new(AtomicUsize::new(0)),clock:0}}}
pub(super) fn has_lut(filters:&[VideoFilter],providers:FilterProviders)->bool{filters.iter().any(|f|f.enabled&&providers.enabled(f.plugin_id())&&matches!(f.effect,VideoEffect::ApplyLut{..})&&f.lut_asset().is_some())}
pub(super) fn has_live_lut(filters:&[VideoFilter])->bool{filters.iter().any(|f|f.enabled&&matches!(f.effect,VideoEffect::ApplyLut{..})&&f.lut_asset().is_some())}
pub(super) fn scroll_settings(effect:VideoEffect)->skylive_media::ScrollSettings{let VideoEffect::Scroll {limit_width,limit_height,width,height,looping,..}=effect else{unreachable!()};skylive_media::ScrollSettings {width:limit_width.then_some(width),height:limit_height.then_some(height),looping}}
fn settings(effect:VideoEffect)->skylive_media::ColorLutSettings{let VideoEffect::ApplyLut {amount,passthrough_alpha}=effect else{unreachable!()};skylive_media::ColorLutSettings {amount,passthrough_alpha}}
fn asset_error(error:crate::lut_assets::LutError)->FilterError{FilterError {kind:error.kind(),code:error.code(),detail:error.to_string()}}
impl LutStore {
    fn evict(&mut self,keep:&[LutAsset])->bool{let victim=self.entries.iter().filter(|(asset,_)|!keep.contains(asset)).min_by_key(|(_,loaded)|loaded.touched).map(|(asset,_)|asset.clone());if let Some(victim)=victim{self.entries.remove(&victim);true}else{false}}
    fn credit(&mut self,bytes:usize,keep:&[LutAsset])->Result<Credit,FilterError>{
        loop {if self.budget.fetch_update(Ordering::AcqRel,Ordering::Acquire,|used|used.checked_add(bytes).filter(|n|*n<=TABLE_BYTES)).is_ok(){return Ok(Credit {budget:self.budget.clone(),bytes});}if !self.evict(keep){return Err(limited("LUT table budget includes active and retiring readers"));}}
    }
    fn load(&mut self,assets:&[LutAsset],epoch:u64,generation:u64)->Result<HashMap<LutAsset,Arc<Table>>,FilterError>{
        if assets.len()>skylive_core::MAX_VIDEO_FILTERS||assets.iter().try_fold(0usize,|sum,asset|sum.checked_add(asset.table_bytes())).is_none_or(|sum|sum>TABLE_BYTES){return Err(limited("LUT resources exceed filter count or 64 MiB table budget"));}
        let epoch=epoch.max(image_mask::epoch());let mut reads=crate::lut_assets::LutReadBudget::new(64*1024*1024);let mut result=HashMap::new();
        for asset in assets {
            self.clock=self.clock.checked_add(1).ok_or_else(||limited("LUT resource LRU exhausted"))?;
            let previous=self.entries.remove(asset);
            if let Some(previous)=previous.as_ref().filter(|old|old.epoch==epoch&&old.generation==generation){let value=previous.value.clone();let mut old=previous.clone_entry();old.touched=self.clock;self.entries.insert(asset.clone(),old);result.insert(asset.clone(),value?);continue;}
            while self.entries.len()>=TABLE_ENTRIES{if !self.evict(assets){return Err(limited("LUT resource metadata capacity"));}}
            let cached=previous.as_ref().and_then(|old|old.value.as_ref().ok());
            let value=if let Some(cached)=cached {
                crate::lut_assets::load_lut(asset,&mut reads,Some(&cached.value)).map(|_|cached.clone()).map_err(asset_error)
            }else{
                let credit=self.credit(asset.table_bytes(),assets)?;
                crate::lut_assets::load_lut(asset,&mut reads,None).map_err(asset_error).and_then(|table|{
                    if table.byte_len()>credit.bytes{return Err(limited("LUT table footprint exceeds admitted metadata"));}Ok(Arc::new(Table {value:table,_credit:credit}))
                })
            };
            self.entries.insert(asset.clone(),Loaded {value:value.clone(),epoch,generation,touched:self.clock});result.insert(asset.clone(),value?);
        }
        Ok(result)
    }
}
impl Loaded {fn clone_entry(&self)->Self{Self {value:self.value.clone(),epoch:self.epoch,generation:self.generation,touched:self.touched}}}
/// Immutable LUT tables tied to their decoded-table credits. Preparation is
/// worker-only; a delayed frame retains the exact tables it was processed with.
pub(crate) struct StagedLutSet {tables:HashMap<skylive_core::VideoFilterId,(Arc<Table>,skylive_media::ColorLutSettings)>}
impl std::fmt::Debug for StagedLutSet {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.debug_struct("StagedLutSet").field("count",&self.tables.len()).finish()}
}
impl StagedLutSet {
    pub(crate) fn prepare(store:&mut LutStore,filters:&[VideoFilter],epoch:u64,generation:u64)->Result<Self,FilterError>{
        if filters.len()>skylive_core::MAX_VIDEO_FILTERS{return Err(limited("staged LUT count"));}
        let mut tables=HashMap::new();tables.try_reserve(filters.len()).map_err(|_|limited("staged LUT metadata allocation"))?;
        for filter in filters {
            if !filter.enabled || !matches!(filter.effect,VideoEffect::ApplyLut {..}) {continue;}
            let Some(asset)=filter.lut_asset() else {continue;};
            filter.validate().map_err(|error|fail(error.to_string()))?;
            let id=filter.id().ok_or_else(||fail("staged LUT identity absent"))?;
            let loaded=store.load(&[asset.clone()],epoch,generation)?;
            let table=loaded.get(asset).ok_or_else(||fail("staged LUT table missing"))?.clone();
            if tables.insert(id,(table,settings(filter.effect))).is_some(){return Err(fail("duplicate staged LUT identity"));}
        }
        Ok(Self {tables})
    }
    pub(crate) fn apply(&self,id:skylive_core::VideoFilterId,input:&RgbaFrame,blend:SourceBlend)->Result<RgbaFrame,FilterError>{
        let (table,params)=self.tables.get(&id).ok_or_else(||fail("staged LUT resource missing"))?;
        let requirements=skylive_media::color_lut_requirements((input.width(),input.height()),table.value.byte_len(),*params).map_err(media_fail)?;
        if requirements.texel_visits>MAX_TEXEL_VISITS{return Err(limited("staged LUT work exceeds budget"));}
        skylive_media::apply_color_lut(input,blend,&table.value,*params,skylive_media::ColorLutLimits {
            max_input_bytes:requirements.input_bytes,max_output_bytes:requirements.output_bytes,max_texel_visits:requirements.texel_visits,
        }).map_err(media_fail)
    }
}

struct Count(usize);
impl std::io::Write for Count {fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{self.0=self.0.checked_add(bytes.len()).filter(|n|*n<=shared::MAX_KEY).ok_or_else(||std::io::Error::other("LUT filter settings exceed shared key limit"))?;Ok(bytes.len())}fn flush(&mut self)->std::io::Result<()>{Ok(())}}
impl FilterCache {
    /// A delayed image must keep its lease through the real GPU request. This
    /// narrowly handles one LUT; mixed ordered resource chains still use their
    /// existing path only when every input/result is lease-aware.
    pub(super) fn apply_delayed_single_lut(&mut self, filter:&VideoFilter,
        input:skylive_media::SharedVideoFrame, ingress:SourceBlend,
        free_bytes:usize, free_work:usize, force_cpu:bool)->Result<SharedFilterResult,FilterError> {
        let Some(asset)=filter.lut_asset() else {
            return Ok(SharedFilterResult {image:input,blend:ingress,new_bytes:0,pixel_stages:0});
        };
        let params=settings(filter.effect);
        let required=skylive_media::color_lut_requirements((input.width(),input.height()),asset.table_bytes(),params).map_err(media_fail)?;
        let work=usize::try_from(required.texel_visits.div_ceil(9)).map_err(|_|limited("delayed LUT work overflow"))?;
        let workspace=(asset.encoded_bytes() as usize).checked_add(1)
            .and_then(|n|n.checked_add(if asset.format()==LutFormat::ObsPng64 {8*1024*1024}else{0}))
            .ok_or_else(||limited("delayed LUT workspace overflow"))?;
        let overhead=workspace.checked_add(asset.table_bytes()).ok_or_else(||limited("delayed LUT resource overflow"))?;
        let limits=skylive_media::ColorLutLimits {max_input_bytes:required.input_bytes,max_output_bytes:required.output_bytes,max_texel_visits:required.texel_visits};
        let mut peak=required.output_bytes;
        #[cfg(feature="gpu-filters")]
        { if !force_cpu {peak=peak.max(self.gpu_lut_peak((input.width(),input.height()),asset.table_bytes(),params,limits,free_bytes.saturating_sub(overhead))?);} }
        let total=peak.checked_add(overhead).ok_or_else(||limited("delayed LUT budget overflow"))?;
        if total>free_bytes || required.output_bytes>MAX_FILTERED_BYTES.saturating_sub(self.bytes)
            || work>free_work || required.texel_visits>MAX_TEXEL_VISITS.saturating_sub(self.texel_visits) {
            return Err(limited("delayed LUT exceeds shared bytes/work budget"));
        }
        let epoch=if self.mask_epoch!=0 {self.mask_epoch} else {image_mask::epoch()};
        let holder=self.lut_store.get_or_insert_with(||Arc::new(Mutex::new(LutStore::default()))).clone();
        let tables=holder.lock().map_err(|_|fail("LUT store poisoned"))?.load(&[asset.clone()],epoch,self.resource_generation)?;
        let table=tables.get(asset).ok_or_else(||fail("prepared LUT table absent"))?;
        #[cfg(feature="gpu-filters")]
        let image=if force_cpu {skylive_media::apply_color_lut(&input,ingress,&table.value,params,limits).map_err(media_fail)?} else {self.apply_lut_output(input,ingress,table.value.clone(),table.clone(),params,limits,free_bytes.saturating_sub(overhead))?};
        #[cfg(not(feature="gpu-filters"))]
        let image=skylive_media::apply_color_lut(&input,ingress,&table.value,params,limits).map_err(media_fail)?;
        self.texel_visits+=required.texel_visits;
        let image=Arc::new(image); let new_bytes=image.byte_len();
        Ok(SharedFilterResult {image:image.into(),blend:SourceBlend::PremultipliedLinearSrgb,new_bytes,pixel_stages:work})
    }
    pub(super) fn apply_lut_chain(&mut self,filters:&[VideoFilter],input:Arc<RgbaFrame>,ingress:SourceBlend,providers:FilterProviders,free_bytes:usize,free_work:usize)->Result<FilterResult,FilterError>{
        if filters.len()>skylive_core::MAX_VIDEO_FILTERS{return Err(limited("source filter count"));}
        let active:Vec<_>=filters.iter().filter(|f|f.enabled&&providers.enabled(f.plugin_id())).cloned().collect();for filter in &active{filter.validate().map_err(|error|fail(error.to_string()))?;}
        #[cfg(feature="gpu-filters")]
        if self.gpu.as_ref().is_some_and(|gpu|matches!(gpu.policy,GpuFailurePolicy::RequireGpu))&&active.iter().any(|filter|matches!(filter.effect,VideoEffect::ScaleFilter{..}|VideoEffect::Scroll{..})||filter.mask_image().is_some()){
            return Err(FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"spatial_filter_gpu_unsupported",detail:"Scaling/Aspect Ratio, Scroll and Image Mask/Blend currently require CPU processing".into()});
        }
        let mut assets=Vec::new();let mut masks=Vec::new();for filter in &active{if let Some(asset)=filter.lut_asset(){if !assets.contains(asset){assets.push(asset.clone());}}if let Some(mask)=filter.mask_image(){if !masks.contains(mask){masks.push(mask.clone());}}}
        let lut_bytes=assets.iter().try_fold(0usize,|sum,a|sum.checked_add(a.table_bytes())).ok_or_else(||limited("LUT table byte overflow"))?;
        let mask_bytes=masks.iter().try_fold(0usize,|sum,a|sum.checked_add(a.width() as usize*a.height() as usize*4)).ok_or_else(||limited("mask byte overflow"))?;
        if lut_bytes>TABLE_BYTES||mask_bytes>64*1024*1024{return Err(limited("LUT/mask resource holder capacity"));}
        let workspace=assets.iter().map(|asset|asset.encoded_bytes() as usize+1+if asset.format()==LutFormat::ObsPng64{8*1024*1024}else{0}).max().unwrap_or(0);
        let epoch=if assets.is_empty()&&masks.is_empty(){0}else if self.mask_epoch!=0{self.mask_epoch}else{image_mask::epoch()};
        let algorithm=if active.iter().any(|filter|matches!(filter.effect,VideoEffect::ScaleFilter{..})){"obs32-resource-chain-scale-v1"}else if active.iter().any(|filter|matches!(filter.effect,VideoEffect::Scroll{..})){"obs32-resource-chain-scroll-v1"}else{"obs32-apply-lut-ordered-v2"};
        let mask_stamp=if image_mask::has_live_mask(filters){self.mask_context.as_ref().map(|(source,clock)|(source.get(),clock.session(),clock.scope(),clock.revision(),clock.files_generation(),if image_mask::has_animated_mask(filters,Some(clock)){clock.now().0}else{0}))}else{None};
        let mut count=Count(0);serde_json::to_writer(&mut count,&(algorithm,&active,self.scroll_context.key,mask_stamp,scale::canvas_key(&active,providers))).map_err(|error|limited(error.to_string()))?;
        let mut identity=Vec::new();identity.try_reserve_exact(count.0).map_err(|_|limited("LUT settings allocation"))?;serde_json::to_writer(&mut identity,&(algorithm,&active,self.scroll_context.key,mask_stamp,scale::canvas_key(&active,providers))).map_err(|error|fail(error.to_string()))?;
        let key=Key {input:input.pixels().as_ptr() as usize,blend:ingress,settings:Vec::new(),mask_identity:identity,mask_epoch:epoch,resource_generation:self.resource_generation};
        if let Some(entry)=self.entries.get(&key){return Ok(FilterResult {image:entry.output.clone(),blend:entry.blend,new_bytes:0,pixel_stages:0});}
        if self.entries.len()>=3*skylive_core::MAX_SOURCES{return Err(limited("source filter cache entry budget"));}
        let mut dimensions=(input.width(),input.height());let mut work=0usize;let mut texels=0u64;let mut peak=0usize;let mut largest_raster=input.byte_len();let mut points=Vec::new();
        for filter in active.iter().map(Some).chain(std::iter::once(None)){
            if filter.is_some_and(|filter|(!matches!(filter.effect,VideoEffect::ImageMask{..}|VideoEffect::ApplyLut{..}|VideoEffect::Scroll{..}|VideoEffect::ScaleFilter{..})||matches!(filter.effect,VideoEffect::ApplyLut{..})&&filter.lut_asset().is_none())){if let Some(stage)=prepare_filter(filter.unwrap()).1{points.push(stage);}continue;}
            if !points.is_empty(){let plan=self.person_plan_for_dimensions(dimensions,&points,free_bytes,free_work)?;work=work.checked_add(plan.work).ok_or_else(||limited("LUT chain work overflow"))?;texels=texels.checked_add(plan.texels).ok_or_else(||limited("LUT chain texel overflow"))?;peak=peak.max(plan.peak);dimensions=plan.final_dimensions;largest_raster=largest_raster.max(dimensions.0 as usize*dimensions.1 as usize*4);points.clear();}
            if let Some(filter)=filter {
                if matches!(filter.effect,VideoEffect::ScaleFilter{..}){if let Some(plan)=scale::plan(filter.effect,dimensions,providers.canvas)?{
                    let required=plan.requirements();work=work.checked_add(required.pixel_visits as usize).ok_or_else(||limited("Scale filter work overflow"))?;
                    texels=texels.checked_add(required.texel_visits).ok_or_else(||limited("Scale filter texel overflow"))?;
                    peak=peak.max(required.input_bytes.checked_add(required.output_bytes).ok_or_else(||limited("Scale filter temporary byte overflow"))?);
                    dimensions=(required.width,required.height);largest_raster=largest_raster.max(required.output_bytes);
                }}
                if matches!(filter.effect,VideoEffect::Scroll{..}){let required=skylive_media::scroll_requirements(dimensions,scroll_settings(filter.effect),self.scroll_context.phase(filter)).map_err(media_fail)?;work=work.checked_add(required.output_bytes/4).ok_or_else(||limited("Scroll work overflow"))?;texels=texels.checked_add(required.texel_visits).ok_or_else(||limited("Scroll texel overflow"))?;peak=peak.max((dimensions.0 as usize*dimensions.1 as usize*4).checked_add(required.output_bytes).ok_or_else(||limited("Scroll temporary byte overflow"))?);dimensions=(required.width,required.height);largest_raster=largest_raster.max(required.output_bytes);}
                if let Some(asset)=filter.lut_asset(){let required=skylive_media::color_lut_requirements(dimensions,asset.table_bytes(),settings(filter.effect)).map_err(media_fail)?;work=work.checked_add((required.texel_visits.div_ceil(5)) as usize).ok_or_else(||limited("LUT work overflow"))?;texels=texels.checked_add(required.texel_visits).ok_or_else(||limited("LUT texel overflow"))?;peak=peak.max(required.output_bytes.checked_mul(2).ok_or_else(||limited("LUT temporary byte overflow"))?);
                    #[cfg(feature="gpu-filters")]
                    {let limits=skylive_media::ColorLutLimits{max_input_bytes:required.input_bytes,max_output_bytes:required.output_bytes,max_texel_visits:required.texel_visits};
                    peak=peak.max(self.gpu_lut_peak(dimensions,asset.table_bytes(),settings(filter.effect),limits,free_bytes.saturating_sub(lut_bytes+mask_bytes+workspace+input.byte_len()+largest_raster))?);}
}
                if let Some(mask)=filter.mask_image(){let VideoEffect::ImageMask{method,color,opacity,stretch}=filter.effect else{unreachable!()};let required=skylive_media::image_mask_requirements(dimensions,(mask.width(),mask.height()),skylive_media::ImageMaskSettings {method:match method{skylive_core::ImageMaskMethod::MaskColor=>skylive_media::ImageMaskMethod::MaskColor,skylive_core::ImageMaskMethod::MaskAlpha=>skylive_media::ImageMaskMethod::MaskAlpha,skylive_core::ImageMaskMethod::BlendMultiply=>skylive_media::ImageMaskMethod::BlendMultiply,skylive_core::ImageMaskMethod::BlendAddition=>skylive_media::ImageMaskMethod::BlendAddition,skylive_core::ImageMaskMethod::BlendSubtraction=>skylive_media::ImageMaskMethod::BlendSubtraction},color,opacity,stretch}).map_err(media_fail)?;work=work.checked_add((required.texel_visits/5) as usize).ok_or_else(||limited("mask work overflow"))?;texels=texels.checked_add(required.texel_visits).ok_or_else(||limited("mask texel overflow"))?;peak=peak.max(required.output_bytes.checked_mul(2).ok_or_else(||limited("mask temporary byte overflow"))?);}
            }
        }
        peak=peak.checked_add(largest_raster).and_then(|v|v.checked_add(lut_bytes)).and_then(|v|v.checked_add(mask_bytes)).and_then(|v|v.checked_add(workspace)).and_then(|v|v.checked_add(input.byte_len())).ok_or_else(||limited("LUT full-chain peak overflow"))?;
        let retained=dimensions.0 as usize*dimensions.1 as usize*4;
        if peak>free_bytes||retained>MAX_FILTERED_BYTES.saturating_sub(self.bytes)||work>free_work||texels>MAX_TEXEL_VISITS.saturating_sub(self.texel_visits){return Err(limited("ordered LUT chain exceeds shared bytes/work budget"));}
        let delegated=self.shared.is_some();let saved=self.mask_epoch;self.mask_epoch=epoch;
        let shared=self.apply_shared(filters,key.clone(),&input,ingress,providers,shared::Admission {peak,work,texels,width:dimensions.0,height:dimensions.1},free_bytes,free_work);self.mask_epoch=saved;if let Some(result)=shared?{return Ok(result);}if delegated{return Err(limited("LUT chain exceeds shared active-flight admission"));}
        let tables=if assets.is_empty(){HashMap::new()}else{let holder=self.lut_store.get_or_insert_with(||Arc::new(Mutex::new(LutStore::default()))).clone();let loaded=holder.lock().map_err(|_|fail("LUT store poisoned"))?.load(&assets,epoch,self.resource_generation)?;loaded};
        self.texel_visits+=texels;let mut current=input.clone();let mut blend=ingress;let mut group=Vec::new();
        for filter in active.iter().map(Some).chain(std::iter::once(None)){
            if filter.is_some_and(|filter|!matches!(filter.effect,VideoEffect::Scroll{..}|VideoEffect::ApplyLut{..}|VideoEffect::ScaleFilter{..})||matches!(filter.effect,VideoEffect::ApplyLut{..})&&filter.lut_asset().is_none()){group.push(filter.unwrap().clone());continue;}
            if !group.is_empty(){let mut cache=FilterCache::default();cache.person_store=self.person_store.clone();cache.person_inference_gate=self.person_inference_gate.clone();cache.temporal_context=self.temporal_context;cache.scroll_context=self.scroll_context;cache.mask_context=self.mask_context.clone();cache.mask_epoch=epoch;cache.resource_generation=self.resource_generation;cache.mask_store=self.mask_store.clone();cache.lut_store=self.lut_store.clone();
                #[cfg(feature="gpu-filters")] {cache.gpu=self.gpu.clone();}
                let result=cache.apply_filters(&group,current.clone(),blend,providers,free_bytes.saturating_sub(lut_bytes+workspace+input.byte_len()),free_work)?;
                #[cfg(feature="gpu-filters")] {self.gpu_calls+=cache.gpu_calls;self.gpu_report=cache.gpu_report.clone();if cache.gpu_last_error.is_some(){self.gpu_last_error=cache.gpu_last_error.clone();}}
                current=result.image;blend=result.blend;group.clear();}
            if let Some(filter)=filter{
                if matches!(filter.effect,VideoEffect::ScaleFilter{..}){if let Some(plan)=scale::plan(filter.effect,(current.width(),current.height()),providers.canvas)?{
                    let required=plan.requirements();current=Arc::new(plan.apply(&current,blend,skylive_media::CompositeLimits{
                        max_layers:1,max_input_bytes:required.input_bytes,max_output_bytes:required.output_bytes,
                        max_pixel_visits:required.pixel_visits.checked_add(required.texel_visits).ok_or_else(||limited("Scale filter visit overflow"))?
                    }).map_err(media_fail)?);blend=SourceBlend::PremultipliedLinearSrgb;
                }}
                if matches!(filter.effect,VideoEffect::Scroll{..}){let required=skylive_media::scroll_requirements((current.width(),current.height()),scroll_settings(filter.effect),self.scroll_context.phase(filter)).map_err(media_fail)?;current=Arc::new(skylive_media::scroll_rgba(&current,blend,scroll_settings(filter.effect),self.scroll_context.phase(filter),skylive_media::ScrollLimits {max_output_bytes:required.output_bytes,max_texel_visits:required.texel_visits}).map_err(media_fail)?);blend=SourceBlend::PremultipliedLinearSrgb;}if let Some(asset)=filter.lut_asset(){let table=tables.get(asset).ok_or_else(||fail("prepared LUT table absent"))?;let required=skylive_media::color_lut_requirements((current.width(),current.height()),table.value.byte_len(),settings(filter.effect)).map_err(media_fail)?;
                let limits=skylive_media::ColorLutLimits{max_input_bytes:required.input_bytes,max_output_bytes:required.output_bytes,max_texel_visits:required.texel_visits};
                #[cfg(feature="gpu-filters")]
                let image=self.apply_lut_output(current.clone(),blend,table.value.clone(),table.clone(),settings(filter.effect),limits,free_bytes.saturating_sub(lut_bytes+mask_bytes+workspace+input.byte_len()+largest_raster))?;
                #[cfg(not(feature="gpu-filters"))]
                let image=skylive_media::apply_color_lut(&current,blend,&table.value,settings(filter.effect),limits).map_err(media_fail)?;
                current=Arc::new(image);blend=SourceBlend::PremultipliedLinearSrgb;
            }}
        }
        let new_bytes=current.byte_len();self.bytes+=new_bytes;self.entries.insert(key,Entry {_input:input,output:current.clone(),blend,_shared_lease:None});Ok(FilterResult {image:current,blend,new_bytes,pixel_stages:work})
    }
}
