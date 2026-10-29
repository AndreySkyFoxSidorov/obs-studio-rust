//! Static mask ownership and ordered source-chain composition. Worker I/O only.
use super::*;
use skylive_core::{ImageAsset,ImageMaskMethod,VideoFilter};
use std::sync::{Mutex,OnceLock,atomic::{AtomicUsize,AtomicBool,Ordering}};
use std::time::Instant;
const MASK_BYTES:usize=64*1024*1024;
const MASK_ENTRIES:usize=32;
// A revision handoff or occupied decoder is transient. Live outputs must be
// allowed to skip the source and submit the next snapshot instead of stopping.
fn retry(detail:impl Into<String>)->FilterError{FilterError{kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"mask_revision_pending",detail:detail.into()}}
// Retained clips have a process-wide owner. A credit is acquired before the
// clip is decoded and follows every outstanding clip reference.
static MASK_CREDIT_BYTES:AtomicUsize=AtomicUsize::new(0);
static MASK_DECODER_BUSY:AtomicBool=AtomicBool::new(false);
struct ClipCredit{bytes:usize}
impl ClipCredit{
 fn reserve()->Result<Self,FilterError>{loop{let used=MASK_CREDIT_BYTES.load(Ordering::Acquire);let bytes=MASK_BYTES.saturating_sub(used);if bytes==0{return Err(retry("animated mask retained-byte capacity busy"));}if MASK_CREDIT_BYTES.compare_exchange(used,used+bytes,Ordering::AcqRel,Ordering::Acquire).is_ok(){return Ok(Self{bytes});}}}
 fn shrink(&mut self,bytes:usize){if bytes<self.bytes{MASK_CREDIT_BYTES.fetch_sub(self.bytes-bytes,Ordering::AcqRel);self.bytes=bytes;}}
}
impl Drop for ClipCredit{fn drop(&mut self){MASK_CREDIT_BYTES.fetch_sub(self.bytes,Ordering::AcqRel);}}
struct DecoderAdmission;
impl DecoderAdmission{fn acquire()->Result<Self,FilterError>{MASK_DECODER_BUSY.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).map_err(|_|retry("animated mask decoder is busy"))?;Ok(Self)}}
impl Drop for DecoderAdmission{fn drop(&mut self){MASK_DECODER_BUSY.store(false,Ordering::Release);}}
struct MaskClip{value:Arc<skylive_media::AnimationClip>,hash:[u8;32],_credit:ClipCredit}
#[derive(Clone)]
struct Loaded {clip:Result<Arc<MaskClip>,FilterError>,epoch:u64,generation:u64,touched:u64}
#[derive(Default)]
pub(crate) struct MaskStore {entries:HashMap<ImageAsset,Loaded>,bytes:usize,clock:u64,versions:Option<Arc<Mutex<crate::mask_live_versions::Book>>>}
pub(super) fn epoch()->u64 {static START:OnceLock<Instant>=OnceLock::new();START.get_or_init(Instant::now).elapsed().as_secs().saturating_add(1)}
pub(super) fn has_mask(filters:&[VideoFilter],providers:FilterProviders)->bool {filters.iter().any(|f|f.enabled&&providers.enabled(f.plugin_id())&&matches!(f.effect,VideoEffect::ImageMask {..})&&f.mask_image().is_some())}
pub(super) fn has_animated_mask(filters:&[VideoFilter],clock:Option<&crate::mask_animation_clock::Snapshot>)->bool{
    filters.iter().any(|f|f.enabled&&f.mask_image().is_some_and(|asset|{
        crate::assets::mask_animation_candidate(asset.path()) && clock.and_then(|clock|clock.files.as_ref())
            .and_then(|files|files.version(asset.path())).is_none_or(|revision|revision.may_animate())
    }))
}
pub(super) fn has_live_mask(filters:&[VideoFilter])->bool {filters.iter().any(|f|f.enabled&&matches!(f.effect,VideoEffect::ImageMask {..})&&f.mask_image().is_some())}
fn settings(effect:VideoEffect)->skylive_media::ImageMaskSettings {let VideoEffect::ImageMask {method,color,opacity,stretch}=effect else{unreachable!()};
    skylive_media::ImageMaskSettings {method:match method {ImageMaskMethod::MaskColor=>skylive_media::ImageMaskMethod::MaskColor,ImageMaskMethod::MaskAlpha=>skylive_media::ImageMaskMethod::MaskAlpha,
        ImageMaskMethod::BlendMultiply=>skylive_media::ImageMaskMethod::BlendMultiply,ImageMaskMethod::BlendAddition=>skylive_media::ImageMaskMethod::BlendAddition,ImageMaskMethod::BlendSubtraction=>skylive_media::ImageMaskMethod::BlendSubtraction},color,opacity,stretch}}
impl MaskStore {
    pub(crate) fn with_versions(versions:Arc<Mutex<crate::mask_live_versions::Book>>)->Self{Self{versions:Some(versions),..Self::default()}}
    // Other jobs may pin old clips. Reclaim only unrequested cache entries;
    // their remaining external owners still retain the process-wide credit.
    fn evict_unrequested(&mut self,assets:&[ImageAsset]){
        let victims:Vec<_>=self.entries.iter().filter(|(key,value)|!assets.contains(key)&&value.clip.as_ref().ok().is_none_or(|clip|Arc::strong_count(clip)==1)).map(|(key,_)|key.clone()).collect();
        for key in victims {if let Some(old)=self.entries.remove(&key){if let Ok(clip)=old.clip{self.bytes=self.bytes.saturating_sub(clip.value.byte_len());}}}
    }
    fn load(&mut self,assets:&[ImageAsset],epoch:u64,generation:u64,context:Option<&crate::mask_animation_clock::Snapshot>)->Result<HashMap<ImageAsset,Arc<MaskClip>>,FilterError>{
        // A queued render must not reuse an old epoch after waiting on GPU work.
        let epoch=epoch.max(self::epoch());
        let total=assets.iter().try_fold(0usize,|sum,a|sum.checked_add(a.width() as usize*a.height() as usize*4)).ok_or_else(||limited("mask byte overflow"))?;
        if assets.len()>skylive_core::MAX_VIDEO_FILTERS || total>MASK_BYTES{return Err(limited("mask resources exceed filter count or 64 MiB pixel budget"));}
        // Retained pixels have a separate bounded owner. Do not evict any asset
        // requested by this job while preparing its other masks.
        let requested_existing:usize=assets.iter().filter_map(|a|self.entries.get(a)).filter_map(|v|v.clip.as_ref().ok()).map(|f|f.value.byte_len()).sum();
        let missing=assets.iter().filter(|a|!self.entries.contains_key(*a)).count();
        while self.entries.len()+missing>MASK_ENTRIES || self.bytes.saturating_sub(requested_existing).saturating_add(total)>MASK_BYTES {
            let victim=self.entries.iter().filter(|(key,value)|!assets.contains(key)&&value.clip.as_ref().ok().is_none_or(|clip|Arc::strong_count(clip)==1)).min_by_key(|(_,v)|v.touched).map(|(key,_)|key.clone());
            let Some(victim)=victim else{return Err(limited("retained mask store capacity"));};
            if let Some(old)=self.entries.remove(&victim){if let Ok(clip)=old.clip {self.bytes=self.bytes.saturating_sub(clip.value.byte_len());}}
        }
        // GIF clip byte size is unknown until validated decode. When a new
        // resource arrives, retire unrequested, unborrowed clips first.
        if assets.iter().any(|asset|!self.entries.contains_key(asset)){
            let victims:Vec<_>=self.entries.iter().filter(|(key,value)|!assets.contains(key)&&value.clip.as_ref().ok().is_none_or(|clip|Arc::strong_count(clip)==1)).map(|(key,_)|key.clone()).collect();
            for key in victims {if let Some(old)=self.entries.remove(&key){if let Ok(clip)=old.clip{self.bytes=self.bytes.saturating_sub(clip.value.byte_len());}}}
        }
        let mut reads=crate::assets::ReadBudget::new(64*1024*1024);
        let mut decoded=crate::assets::DecodeBudget::new(MASK_BYTES);
        // Discover all changed files before decoding any replacement, so an
        // obsolete later clip cannot permanently block an earlier one.
        // Keep these bounded reads for decoding rather than reading twice.
        let mut live_reads:HashMap<String,(Box<[u8]>,[u8;32])>=HashMap::new();
        let live=context.and_then(|clock|clock.files.as_ref()).is_some();
        let _live_admission=if live{Some(DecoderAdmission::acquire()?)}else{None};
        if live {
            let clock=context.ok_or_else(||fail("mask playback snapshot unavailable"))?;
            let versions=self.versions.as_ref().ok_or_else(||fail("mask revision owner unavailable"))?.clone();
            for asset in assets {
                let expected=clock.files.as_ref().and_then(|files|files.version(asset.path())).ok_or_else(||fail("mask revision missing from playback snapshot"))?;
                if !versions.lock().map_err(|_|fail("mask revision owner poisoned"))?.current(asset.path(),expected){return Err(retry("mask revision changed; retry"));}
                let cached=self.entries.get(asset).is_some_and(|old|old.epoch==epoch&&old.generation==generation&&old.clip.as_ref().ok().is_some_and(|clip|clip.hash==expected.hash));
                if cached||live_reads.contains_key(asset.path()){continue;}
                let read=crate::assets::read_live_mask_clip(asset,&mut reads).map_err(asset_error)?;
                live_reads.insert(asset.path().to_owned(),read);
            }
            let mut changed=false;
            for (path,(_,hash)) in &live_reads {
                let expected=clock.files.as_ref().and_then(|files|files.version(path)).ok_or_else(||fail("mask revision missing from playback snapshot"))?;
                if expected.hash!=*hash {
                    let mut book=versions.lock().map_err(|_|fail("mask revision owner poisoned"))?;
                    match book.publish(path,expected,*hash,clock.now()) {
                        Err("mask file changed; retry with a new playback snapshot")=>changed=true,
                        Err(detail)=>return Err(retry(detail)),
                        Ok(())=>{},
                    }
                }
            }
            if changed{return Err(retry("mask files changed; retry with a new playback snapshot"));}
            // Once the new snapshot is current, old unborrowed requested clips
            // no longer need to occupy space while earlier replacements decode.
            let stale:Vec<_>=assets.iter().filter(|asset|{
                let expected=clock.files.as_ref().and_then(|files|files.version(asset.path()));
                self.entries.get(*asset).and_then(|entry|entry.clip.as_ref().ok()).is_some_and(|clip|
                    expected.is_some_and(|version|clip.hash!=version.hash)&&Arc::strong_count(clip)==1)
            }).cloned().collect();
            for asset in stale{if let Some(old)=self.entries.remove(&asset){if let Ok(clip)=old.clip{self.bytes=self.bytes.saturating_sub(clip.value.byte_len());}}}
        }
        let mut result:HashMap<ImageAsset,Arc<MaskClip>>=HashMap::new();
        for asset in assets {
            self.clock=self.clock.checked_add(1).ok_or_else(||limited("mask LRU exhausted"))?;
            let expected=context.and_then(|clock|clock.files.as_ref()).and_then(|files|files.version(asset.path())).cloned();
            let old=self.entries.remove(asset).filter(|entry|entry.clip.is_ok());
            // Count clips already required by this same chain, including valid
            // cached clips later in its order. Credit outside this set can be
            // transient; the required set itself cannot be evicted to fit.
            let required=result.values().map(|clip|clip.value.byte_len()).sum::<usize>().saturating_add(
                assets.iter().filter(|other|*other!=asset&&!result.contains_key(*other)).filter_map(|other|{
                    let expected=context.and_then(|clock|clock.files.as_ref()).and_then(|files|files.version(other.path())).map(|version|&version.hash).unwrap_or_else(||other.sha256());
                    self.entries.get(other).and_then(|entry|entry.clip.as_ref().ok()).filter(|clip|&clip.hash==expected).map(|clip|clip.value.byte_len())
                }).sum::<usize>());
            // A live snapshot must contain its full resource closure. Only
            // cold/offline snapshots may fall back to authored exact identity.
            if expected.is_none()&&context.is_some_and(|clock|clock.files.is_some()){
                if let Some(old)=old{self.entries.insert(asset.clone(),old);}
                return Err(fail("mask revision missing from playback snapshot"));
            }
            let Some(expected)=expected else {
                let old=if old.as_ref().and_then(|entry|entry.clip.as_ref().ok()).is_some_and(|clip|&clip.hash!=asset.sha256()){
                    let previous=old.as_ref().and_then(|entry|entry.clip.as_ref().ok()).ok_or_else(||fail("mask clip ownership missing"))?;
                    if Arc::strong_count(previous)>1{if let Some(old)=old{self.entries.insert(asset.clone(),old);}return Err(retry("mask reload is waiting for an earlier render"));}
                    self.bytes=self.bytes.saturating_sub(previous.value.byte_len());drop(old);None
                }else{old};
                let result_clip=(||{
                    let _decode=DecoderAdmission::acquire()?;
                    if let Some(old)=old.as_ref().and_then(|entry|entry.clip.as_ref().ok()).filter(|clip|&clip.hash==asset.sha256()){
                        crate::assets::load_mask_clip_with_budgets(asset,&mut reads,&mut decoded,MASK_BYTES,Some(&old.value)).map_err(asset_error)?;
                        return Ok(old.clone());
                    }
                    self.evict_unrequested(assets);
                    let mut credit=match ClipCredit::reserve(){Ok(value)=>value,Err(_error) if required>=MASK_BYTES=>return Err(limited("requested mask clips exceed 64MiB")),Err(error)=>return Err(error)};
                    let available=MASK_BYTES.saturating_sub(self.bytes).min(credit.bytes);
                    let value=crate::assets::load_mask_clip_with_budgets(asset,&mut reads,&mut decoded,available,None).map_err(|error|if error.is_decoded_capacity()&&available<MASK_BYTES.saturating_sub(required){retry("mask decoded capacity is busy")}else{asset_error(error)})?;
                    credit.shrink(value.byte_len());Ok(Arc::new(MaskClip{value,hash:*asset.sha256(),_credit:credit}))
                })();
                if let Some(old)=old {if let Ok(clip)=old.clip{self.bytes=self.bytes.saturating_sub(clip.value.byte_len());}}
                if let Ok(clip)=&result_clip {self.bytes+=clip.value.byte_len();}
                self.entries.insert(asset.clone(),Loaded{clip:result_clip.clone(),epoch,generation,touched:self.clock});result.insert(asset.clone(),result_clip?);continue;
            };
            let versions=match self.versions.as_ref(){Some(value)=>value.clone(),None=>{if let Some(old)=old{self.entries.insert(asset.clone(),old);}return Err(fail("mask revision owner unavailable"));}};
            let is_current=match versions.lock(){Ok(book)=>book.current(asset.path(),&expected),Err(_)=>{if let Some(old)=old{self.entries.insert(asset.clone(),old);}return Err(fail("mask revision owner poisoned"));}};
            if !is_current {if let Some(old)=old{self.entries.insert(asset.clone(),old);}return Err(retry("mask revision changed; retry"));}
            if let Some(old)=old.as_ref(){if old.epoch==epoch&&old.generation==generation&&old.clip.as_ref().ok().is_some_and(|clip|clip.hash==expected.hash){let clip=old.clip.clone();if let Ok(value)=&clip{expected.publish_animation(value.value.is_animated());}let mut retained=old.clone();retained.touched=self.clock;self.entries.insert(asset.clone(),retained);result.insert(asset.clone(),clip?);continue;}}
            let (bytes,hash)=live_reads.get(asset.path()).ok_or_else(||fail("mask preflight bytes unavailable"))?;
            let hash=*hash;
            if hash!=expected.hash {
                let now=context.ok_or_else(||fail("mask playback snapshot unavailable"))?.now();
                let result=match versions.lock(){Ok(mut book)=>book.publish(asset.path(),&expected,hash,now),Err(_)=>Err("mask revision owner poisoned")};
                if let Some(old)=old{self.entries.insert(asset.clone(),old);}
                return Err(retry(result.err().unwrap_or("mask revision changed; retry")));
            }
            if let Some(mut old)=old {
                if old.clip.as_ref().ok().is_some_and(|clip|clip.hash==hash){old.epoch=epoch;old.generation=generation;old.touched=self.clock;let clip=old.clip.clone();if let Ok(value)=&clip{expected.publish_animation(value.value.is_animated());}self.entries.insert(asset.clone(),old);result.insert(asset.clone(),clip?);continue;}
                if old.clip.as_ref().ok().is_some_and(|clip|Arc::strong_count(clip)>1){self.entries.insert(asset.clone(),old);return Err(retry("mask reload is waiting for an earlier render"));}
                if let Ok(clip)=old.clip{self.bytes=self.bytes.saturating_sub(clip.value.byte_len());}
            }
            let clip=(||{
                self.evict_unrequested(assets);
                let mut credit=match ClipCredit::reserve(){Ok(value)=>value,Err(_error) if required>=MASK_BYTES=>return Err(limited("requested mask clips exceed 64MiB")),Err(error)=>return Err(error)};
                let available=MASK_BYTES.saturating_sub(self.bytes).min(credit.bytes);
                let value=crate::assets::decode_live_mask_clip(asset,&bytes,&mut decoded,available).map_err(|error|if error.is_decoded_capacity()&&available<MASK_BYTES.saturating_sub(required){retry("mask decoded capacity is busy")}else{asset_error(error)})?;
                credit.shrink(value.byte_len());Ok(Arc::new(MaskClip{value,hash,_credit:credit}))
            })();
            if let Ok(clip)=&clip{expected.publish_animation(clip.value.is_animated());self.bytes+=clip.value.byte_len();}
            self.entries.insert(asset.clone(),Loaded{clip:clip.clone(),epoch,generation,touched:self.clock});result.insert(asset.clone(),clip?);
        }
        Ok(result)
    }
}
fn asset_error(error:crate::assets::AssetError)->FilterError {FilterError {kind:error.kind(),code:error.code(),detail:error.to_string()}}
/// Immutable prepared image-mask resource for a source sample. The decoded
/// clip owns its process-wide credit until every delayed sample releases it.
/// Preparation performs file I/O and must only run on a bounded worker.
pub(crate) struct StagedMask {
    clip: Arc<MaskClip>,
    elapsed: skylive_media::MediaTime,
    params: skylive_media::ImageMaskSettings,
}
/// Prepared masks keyed by authored identity. The set owns each clip's
/// decoding credit and can be retained with an individual delayed sample.
pub(crate) struct StagedMaskSet {
    masks: HashMap<skylive_core::VideoFilterId,StagedMask>,
    live:Option<Arc<AtomicBool>>,
    versions:Option<Arc<Mutex<crate::mask_live_versions::Book>>>,
    files:Option<Arc<crate::mask_live_versions::Sample>>,
    luts:Option<crate::video_filters::StagedLutSet>,
}
impl std::fmt::Debug for StagedMaskSet {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {
        f.debug_struct("StagedMaskSet").field("count",&self.masks.len()).finish()
    }
}
impl StagedMaskSet {
    pub(crate) fn prepare_static(store:&mut MaskStore, filters:&[VideoFilter],epoch:u64,generation:u64)
        ->Result<Self,FilterError> {
        if filters.len()>skylive_core::MAX_VIDEO_FILTERS {return Err(limited("staged mask count"));}
        let mut masks=HashMap::new();
        masks.try_reserve(filters.len()).map_err(|_|limited("staged mask metadata allocation"))?;
        for filter in filters {
            if !filter.enabled || !matches!(filter.effect,VideoEffect::ImageMask {..}) {continue;}
            let id=filter.id().ok_or_else(||fail("staged mask identity absent"))?;
            if let Some(mask)=StagedMask::prepare_static(store,filter,epoch,generation)? {
                if masks.insert(id,mask).is_some(){return Err(fail("duplicate staged mask identity"));}
            }
        }
        Ok(Self {masks,live:None,versions:None,files:None,luts:None})
    }
    /// Prepare the mask set for one explicitly sampled playback instant. The
    /// snapshot must use the same version book as `store`; each clip retains
    /// its own decoded-resource permit with this individual camera sample.
    pub(crate) fn prepare_at(store:&mut MaskStore,filters:&[VideoFilter],source:skylive_core::Id,
        clock:&crate::mask_animation_clock::Snapshot,epoch:u64,generation:u64)->Result<Self,FilterError> {
        if filters.len()>skylive_core::MAX_VIDEO_FILTERS{return Err(limited("staged mask count"));}
        let mut masks=HashMap::new();
        masks.try_reserve(filters.len()).map_err(|_|limited("staged mask metadata allocation"))?;
        for filter in filters {
            if !filter.enabled || !matches!(filter.effect,VideoEffect::ImageMask {..}) {continue;}
            let id=filter.id().ok_or_else(||fail("staged mask identity absent"))?;
            if let Some(mask)=StagedMask::prepare(store,filter,source,clock,epoch,generation)? {
                if masks.insert(id,mask).is_some(){return Err(fail("duplicate staged mask identity"));}
            }
        }
        Ok(Self {masks,live:None,versions:None,files:None,luts:None})
    }
    /// Map an uncertainty interval only when every possible capture instant
    /// selects the same animation frame. Matching endpoints alone is unsafe:
    /// a loop can wrap back to the same frame within the interval.
    pub(crate) fn prepare_window(store:&mut MaskStore,filters:&[VideoFilter],source:skylive_core::Id,
        clock:&crate::mask_animation_clock::Snapshot,bounds:skylive_media::MediaTimeBounds,
        live:Arc<AtomicBool>,epoch:u64,generation:u64)->Result<Self,FilterError> {
        if !live.load(Ordering::Acquire){return Err(retry("camera mask timeline retired"));}
        if bounds.earliest.0>bounds.latest.0 || bounds.latest.0>clock.now().0 {
            return Err(fail("camera mask timestamp is outside the playback snapshot"));
        }
        let width=bounds.latest.0-bounds.earliest.0;
        let earlier=clock.clone().at(bounds.earliest);
        let mut prepared=Self::prepare_at(store,filters,source,&earlier,epoch,generation)?;
        for mask in prepared.masks.values() {
            let sample=mask.clip.value.sample(mask.elapsed);
            if sample.next_after_ns.is_some_and(|next|next<=width) {
                return Err(retry("camera mask animation frame is ambiguous in clock bounds"));
            }
        }
        if !live.load(Ordering::Acquire){return Err(retry("camera mask timeline retired"));}
        prepared.live=Some(live);
        prepared.versions=store.versions.clone();
        prepared.files=clock.files.clone();
        if !prepared.current(){return Err(retry("camera mask revision retired"));}
        Ok(prepared)
    }
    pub(crate) fn get(&self,id:skylive_core::VideoFilterId)->Option<&StagedMask>{
        if self.live.as_ref().is_some_and(|live|!live.load(Ordering::Acquire)){return None;}
        self.masks.get(&id)
    }
    pub(crate) fn set_luts(&mut self,luts:crate::video_filters::StagedLutSet){self.luts=Some(luts);}
    pub(crate) fn lut(&self)->Option<&crate::video_filters::StagedLutSet>{self.luts.as_ref()}
    pub(crate) fn live(&self)->Option<Arc<AtomicBool>>{self.live.clone()}
    pub(crate) fn current(&self)->bool {
        if self.live.as_ref().is_some_and(|live|!live.load(Ordering::Acquire)){return false;}
        match (&self.versions,&self.files) {
            (Some(versions),Some(files))=>versions.try_lock().is_ok_and(|book|files.all_current(&book)),
            (None,None)=>true,
            _=>false,
        }
    }
}
impl StagedMask {
    /// Exact authored-asset snapshot for a nonanimated mask. This provides a
    /// safe fallback where capture has no playback timeline: changed files and
    /// animated clips are rejected rather than sampled at the wrong time.
    pub(crate) fn prepare_static(store:&mut MaskStore,filter:&VideoFilter,
        epoch:u64,generation:u64)->Result<Option<Self>,FilterError> {
        if !matches!(filter.effect,VideoEffect::ImageMask {..}) {return Err(fail("staged filter is not an image mask"));}
        filter.validate().map_err(|error|fail(error.to_string()))?;
        let Some(asset)=filter.mask_image() else {return Ok(None);};
        let clips=store.load(&[asset.clone()],epoch,generation,None)?;
        let clip=clips.get(asset).ok_or_else(||fail("prepared mask resource absent"))?.clone();
        if clip.value.is_animated() {return Err(fail("animated mask requires a capture playback snapshot"));}
        Ok(Some(Self {clip,elapsed:skylive_media::MediaTime(0),params:settings(filter.effect)}))
    }
    pub(crate) fn prepare(store:&mut MaskStore, filter:&VideoFilter, source:skylive_core::Id,
        clock:&crate::mask_animation_clock::Snapshot, epoch:u64, generation:u64)
        ->Result<Option<Self>,FilterError> {
        if !matches!(filter.effect,VideoEffect::ImageMask {..}) {return Err(fail("staged filter is not an image mask"));}
        filter.validate().map_err(|error|fail(error.to_string()))?;
        let Some(asset)=filter.mask_image() else {return Ok(None);};
        let clips=store.load(&[asset.clone()],epoch,generation,Some(clock))?;
        let clip=clips.get(asset).ok_or_else(||fail("prepared mask resource absent"))?.clone();
        let elapsed=if clip.value.is_animated() {
            let elapsed=clock.elapsed(source,filter).ok_or_else(||fail("animated mask timeline unavailable"))?;
            if let Some(start)=clock.files.as_ref().and_then(|files|files.version(asset.path())).and_then(|revision|revision.start) {
                let since=clock.now().0.checked_sub(start.0).ok_or_else(||fail("mask snapshot predates the current file revision"))?;
                skylive_media::MediaTime(elapsed.0.min(since))
            } else {elapsed}
        } else {skylive_media::MediaTime(0)};
        Ok(Some(Self {clip,elapsed,params:settings(filter.effect)}))
    }
    /// Only call after the sequential owner has admitted output and work.
    pub(crate) fn apply(&self,input:&RgbaFrame,ingress:SourceBlend)->Result<RgbaFrame,FilterError> {
        let mask=self.clip.value.sample(self.elapsed).image;
        let required=skylive_media::image_mask_requirements((input.width(),input.height()),
            (mask.width(),mask.height()),self.params).map_err(media_fail)?;
        if required.texel_visits>MAX_TEXEL_VISITS {return Err(limited("staged mask work exceeds budget"));}
        skylive_media::image_mask_v2(input,ingress,&mask,self.params,skylive_media::ImageMaskLimits {
            max_input_bytes:required.input_bytes,max_output_bytes:required.output_bytes,max_texel_visits:required.texel_visits,
        }).map_err(media_fail)
    }
}

impl FilterCache {
    /// Apply one mask while the delayed input owns its admission lease. This
    /// synchronous CPU path does not detach or retain the input pixels.
    pub(super) fn apply_delayed_single_mask(&mut self,filter:&VideoFilter,
        input:skylive_media::SharedVideoFrame,ingress:SourceBlend,
        free_bytes:usize,free_work:usize)->Result<SharedFilterResult,FilterError>{
        let Some(asset)=filter.mask_image() else {return Ok(SharedFilterResult{image:input,blend:ingress,new_bytes:0,pixel_stages:0});};
        let params=settings(filter.effect);
        let required=skylive_media::image_mask_requirements((input.width(),input.height()),(asset.width(),asset.height()),params).map_err(media_fail)?;
        let mask_bytes=(asset.width() as usize).checked_mul(asset.height() as usize).and_then(|n|n.checked_mul(4)).ok_or_else(||limited("delayed mask dimensions overflow"))?;
        let peak=required.output_bytes.checked_add(mask_bytes).ok_or_else(||limited("delayed mask budget overflow"))?;
        let work=usize::try_from(required.texel_visits/5).map_err(|_|limited("delayed mask work overflow"))?;
        if mask_bytes>MASK_BYTES || peak>free_bytes || required.output_bytes>MAX_FILTERED_BYTES.saturating_sub(self.bytes)
            || work>free_work || required.texel_visits>MAX_TEXEL_VISITS.saturating_sub(self.texel_visits) {
            return Err(limited("delayed mask exceeds shared bytes/work budget"));
        }
        let epoch=if self.mask_epoch!=0 {self.mask_epoch}else{epoch()};
        let holder=self.mask_store.get_or_insert_with(||Arc::new(Mutex::new(MaskStore::default()))).clone();
        let masks=holder.lock().map_err(|_|fail("mask store lock poisoned"))?.load(&[asset.clone()],epoch,self.resource_generation,self.mask_context.as_ref().map(|(_,clock)|clock))?;
        let clip=masks.get(asset).ok_or_else(||fail("prepared mask resource absent"))?;
        let elapsed=if clip.value.is_animated(){
            let (source,clock)=self.mask_context.as_ref().ok_or_else(||fail("animated mask requires a source playback snapshot"))?;
            let elapsed=clock.elapsed(*source,filter).ok_or_else(||fail("animated mask timeline unavailable"))?;
            if let Some(start)=clock.files.as_ref().and_then(|files|files.version(asset.path())).and_then(|revision|revision.start){
                let since=clock.now().0.checked_sub(start.0).ok_or_else(||fail("mask snapshot predates the current file revision"))?;
                skylive_media::MediaTime(elapsed.0.min(since))
            }else{elapsed}
        }else{skylive_media::MediaTime(0)};
        let mask=clip.value.sample(elapsed).image;
        let image=skylive_media::image_mask_v2(&input,ingress,&mask,params,skylive_media::ImageMaskLimits{
            max_input_bytes:required.input_bytes,max_output_bytes:required.output_bytes,max_texel_visits:required.texel_visits
        }).map_err(media_fail)?;
        self.texel_visits+=required.texel_visits;
        let image=Arc::new(image);let new_bytes=image.byte_len();
        Ok(SharedFilterResult{image:image.into(),blend:SourceBlend::PremultipliedLinearSrgb,new_bytes,pixel_stages:work})
    }
    pub(super) fn apply_mask_chain(&mut self,filters:&[VideoFilter],input:Arc<RgbaFrame>,ingress:SourceBlend,providers:FilterProviders,free_bytes:usize,free_work:usize)->Result<FilterResult,FilterError>{
        #[cfg(feature="gpu-filters")]
        if self.gpu.as_ref().is_some_and(|gpu|matches!(gpu.policy,GpuFailurePolicy::RequireGpu)){return Err(FilterError {kind:crate::resource_issue::ResourceIssueKind::Unavailable,code:"image_mask_gpu_unsupported",detail:"Image Mask/Blend currently requires CPU processing".into()});}
        let active:Vec<_>=filters.iter().filter(|f|f.enabled&&providers.enabled(f.plugin_id())).cloned().collect();
        for f in &active {f.validate().map_err(|e|fail(e.to_string()))?;}
        let mut assets=Vec::new();for filter in &active {if let Some(asset)=filter.mask_image(){if !assets.contains(asset){assets.push(asset.clone());}}}
        let mask_bytes=assets.iter().try_fold(0usize,|sum,a|sum.checked_add(a.width() as usize*a.height() as usize*4)).ok_or_else(||limited("mask byte overflow"))?;
        if mask_bytes>MASK_BYTES{return Err(limited("mask pixels exceed64MiB"));}
        let epoch=if self.mask_epoch!=0 {self.mask_epoch}else{epoch()};
        let time=if has_live_mask(filters){self.mask_context.as_ref().map(|(source,clock)|(source.get(),clock.session(),clock.scope(),clock.revision(),clock.files_generation(),if has_animated_mask(filters,Some(clock)){clock.now().0}else{0}))}else{None};
        let identity=serde_json::to_vec(&(&active,time)).map_err(|e|fail(e.to_string()))?;
        if identity.len()>shared::MAX_KEY{return Err(limited("mask settings exceed shared key limit"));}
        let key=Key {input:input.pixels().as_ptr() as usize,blend:ingress,settings:Vec::new(),mask_identity:identity,mask_epoch:epoch,resource_generation:self.resource_generation};
        if let Some(entry)=self.entries.get(&key){return Ok(FilterResult {image:entry.output.clone(),blend:entry.blend,new_bytes:0,pixel_stages:0});}
        if self.entries.len()>=3*skylive_core::MAX_SOURCES{return Err(limited("source filter cache entry budget"));}
        // Every ordered segment, dimensions, mask and neural/GPU temporary is
        // admitted before any linked-file read, decoder, pixels or inference.
        let mut dimensions=(input.width(),input.height());let mut work=0usize;let mut texels=0u64;let mut peak=0usize;let mut points=Vec::new();
        for filter in active.iter().map(Some).chain(std::iter::once(None)) {
            if filter.is_some_and(|f|!matches!(f.effect,VideoEffect::ImageMask {..})) {if let Some(stage)=prepare_filter(filter.unwrap()).1{points.push(stage);}continue;}
            if !points.is_empty(){let p=self.person_plan_for_dimensions(dimensions,&points,free_bytes,free_work)?;work=work.checked_add(p.work).ok_or_else(||limited("mask chain work overflow"))?;texels=texels.checked_add(p.texels).ok_or_else(||limited("mask chain texels overflow"))?;peak=peak.max(p.peak);dimensions=p.final_dimensions;points.clear();}
            if let Some(filter)=filter {if let Some(asset)=filter.mask_image(){let r=skylive_media::image_mask_requirements(dimensions,(asset.width(),asset.height()),settings(filter.effect)).map_err(media_fail)?;
                work=work.checked_add((r.texel_visits/5) as usize).ok_or_else(||limited("mask work overflow"))?;texels=texels.checked_add(r.texel_visits).ok_or_else(||limited("mask texels overflow"))?;
                peak=peak.max(r.output_bytes.checked_mul(2).ok_or_else(||limited("mask chain byte overflow"))?);
            }}
        }
        peak=peak.checked_add(mask_bytes).and_then(|v|v.checked_add(input.byte_len())).ok_or_else(||limited("mask chain peak overflow"))?;
        let retained=dimensions.0 as usize*dimensions.1 as usize*4;
        if peak>free_bytes||retained>MAX_FILTERED_BYTES.saturating_sub(self.bytes)||work>free_work||texels>MAX_TEXEL_VISITS.saturating_sub(self.texel_visits){return Err(limited("ordered image mask chain exceeds shared bytes/work budget"));}
        let delegated=self.shared.is_some();
        let saved_epoch=self.mask_epoch;self.mask_epoch=epoch;
        let shared=self.apply_shared(filters,key.clone(),&input,ingress,providers,shared::Admission {peak,work,texels,width:dimensions.0,height:dimensions.1},free_bytes,free_work);
        self.mask_epoch=saved_epoch;if let Some(result)=shared? {return Ok(result);}
        if delegated {return Err(limited("image mask chain exceeds shared active-flight admission"));}
        let store=self.mask_store.get_or_insert_with(||Arc::new(Mutex::new(MaskStore::default()))).clone();
        let masks=store.lock().map_err(|_|fail("mask store lock poisoned"))?.load(&assets,epoch,self.resource_generation,self.mask_context.as_ref().map(|(_,clock)|clock))?;
        self.texel_visits+=texels;
        let mut current=input.clone();let mut blend=ingress;let mut group=Vec::new();
        for filter in active.iter().map(Some).chain(std::iter::once(None)) {
            if filter.is_some_and(|f|!matches!(f.effect,VideoEffect::ImageMask {..})) {group.push(filter.unwrap().clone());continue;}
            if !group.is_empty(){
                let mut cache=FilterCache::default();cache.person_store=self.person_store.clone();cache.person_inference_gate=self.person_inference_gate.clone();cache.temporal_context=self.temporal_context;cache.scroll_context=self.scroll_context;cache.mask_context=self.mask_context.clone();cache.resource_generation=self.resource_generation;cache.mask_epoch=self.mask_epoch;
                #[cfg(feature="gpu-filters")] {cache.gpu=self.gpu.clone();}
                let result=cache.apply_filters(&group,current.clone(),blend,providers,free_bytes.saturating_sub(mask_bytes+input.byte_len()),free_work)?;
                #[cfg(feature="gpu-filters")] {self.gpu_calls+=cache.gpu_calls;self.gpu_report=cache.gpu_report.clone();if cache.gpu_last_error.is_some(){self.gpu_last_error=cache.gpu_last_error.clone();}}
                current=result.image;blend=result.blend;group.clear();
            }
            if let Some(filter)=filter {if let Some(asset)=filter.mask_image(){let clip=masks.get(asset).ok_or_else(||fail("prepared mask resource absent"))?;
                let elapsed=if clip.value.is_animated(){let (source,clock)=self.mask_context.as_ref().ok_or_else(||fail("animated mask requires a source playback snapshot"))?;{
                    let elapsed=clock.elapsed(*source,filter).ok_or_else(||fail("animated mask timeline unavailable"))?;
                    let start=clock.files.as_ref().and_then(|files|files.version(asset.path())).and_then(|revision|revision.start);
                    if let Some(start)=start{
                        let since=clock.now().0.checked_sub(start.0).ok_or_else(||fail("mask snapshot predates the current file revision"))?;
                        skylive_media::MediaTime(elapsed.0.min(since))
                    }else{elapsed}
                }}else{skylive_media::MediaTime(0)};
                let mask=clip.value.sample(elapsed).image;
                let required=skylive_media::image_mask_requirements((current.width(),current.height()),(mask.width(),mask.height()),settings(filter.effect)).map_err(media_fail)?;
                current=Arc::new(skylive_media::image_mask_v2(&current,blend,&mask,settings(filter.effect),skylive_media::ImageMaskLimits {max_input_bytes:required.input_bytes,max_output_bytes:required.output_bytes,max_texel_visits:required.texel_visits}).map_err(media_fail)?);
                blend=SourceBlend::PremultipliedLinearSrgb;
            }}
        }
        let new_bytes=current.byte_len();self.bytes+=new_bytes;
        self.entries.insert(key,Entry {_input:input,output:current.clone(),blend,_shared_lease:None});
        Ok(FilterResult {image:current,blend,new_bytes,pixel_stages:work})
    }
}
