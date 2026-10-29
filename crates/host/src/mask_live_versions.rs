//! Composition-root live mask revision ownership. No I/O or decoding here.
use std::{collections::{HashMap,HashSet},sync::{Arc,Weak,atomic::{AtomicU8,Ordering}}};
use skylive_media::MediaTime;
use crate::playback::MetadataPermit;

// One entry per potentially referenced source/filter plus a modal draft.
const MAX_PATHS:usize=3*skylive_core::MAX_SOURCES*skylive_core::MAX_VIDEO_FILTERS+skylive_core::MAX_VIDEO_FILTERS;
const MAX_PATH_BYTES:usize=4096;
const MAX_HISTORY:usize=64;

#[derive(Debug)]
pub(crate) struct Revision {
    pub(crate) domain:u64,
    pub(crate) hash:[u8;32],
    pub(crate) start:Option<MediaTime>,
    pub(crate) generation:u64,
    // Derived from exactly this revision's decoded bytes. Unknown remains
    // conservative; it never asserts that an unexamined clip is static.
    animation:AtomicU8,
    _credit:MetadataPermit,
}
impl Revision {
    pub(crate) fn publish_animation(&self,animated:bool){self.animation.store(if animated{2}else{1},Ordering::Release);}
    pub(crate) fn may_animate(&self)->bool{self.animation.load(Ordering::Acquire)!=1}
}
#[derive(Debug)]
struct Entry {version:Arc<Revision>,history:Vec<Weak<Revision>>,touched:u64,_credit:MetadataPermit}
impl Entry{
 fn evictable(&self)->bool{Arc::strong_count(&self.version)==1&&self.history.iter().all(|old|old.strong_count()==0)}
 fn install(&mut self,next:Arc<Revision>)->Result<(),&'static str>{
  self.history.retain(|old|old.strong_count()!=0);
  if self.history.len()>=MAX_HISTORY{return Err("mask revision history busy");}
  self.history.try_reserve(1).map_err(|_|"mask revision history allocation")?;
  self.history.push(Arc::downgrade(&self.version));self.version=next;Ok(())
 }
}
#[derive(Default,Debug)]
pub(crate) struct Book {entries:HashMap<String,Entry>,generation:u64,clock:u64}
#[derive(Debug)]
pub(crate) struct Sample {entries:HashMap<String,Arc<Revision>>,generation:u64,bytes:usize,_credit:MetadataPermit}
impl Sample {
    pub(crate) fn all_current(&self,book:&Book)->bool {
        self.entries.iter().all(|(path,revision)|book.current(path,revision))
    }
    pub(crate) fn generation(&self)->u64{self.generation}
    pub(crate) fn bytes(&self)->usize{self.bytes}
    pub(crate) fn version(&self,path:&str)->Option<&Arc<Revision>> {self.entries.get(path)}
    pub(crate) fn same_versions(&self,old:&Self)->bool{self.entries.len()==old.entries.len()&&self.entries.iter().all(|(path,v)|old.entries.get(path).is_some_and(|other|Arc::ptr_eq(v,other)))}
}
impl Book {
    fn touch(&mut self)->Result<u64,&'static str>{self.clock=self.clock.checked_add(1).ok_or("mask revision clock exhausted")?;Ok(self.clock)}
    fn next(&mut self)->Result<u64,&'static str>{self.generation=self.generation.checked_add(1).ok_or("mask revision identity exhausted")?;Ok(self.generation)}
    fn version(&mut self,domain:u64,hash:[u8;32],start:Option<MediaTime>)->Result<Arc<Revision>,&'static str>{
        let credit=MetadataPermit::acquire(std::mem::size_of::<Revision>()+2*std::mem::size_of::<usize>()).map_err(|_|"mask revision metadata budget")?;
        Ok(Arc::new(Revision{domain,hash,start,generation:self.next()?,animation:AtomicU8::new(0),_credit:credit}))
    }
    // Acquires a pinned revision for each authored path. Duplicate paths share
    // one identity even if the serialized author hashes differ.
    pub(crate) fn sample(&mut self,domain:u64,assets:&[&skylive_core::ImageAsset])->Result<Sample,&'static str>{
        if assets.len()>MAX_PATHS||assets.iter().any(|a|a.path().len()>MAX_PATH_BYTES){return Err("mask revision path limit");}
        let charge=assets.iter().try_fold(128usize,|sum,a|sum.checked_add(a.path().len()+std::mem::size_of::<(String,Arc<Revision>)>()+128)).ok_or("mask revision metadata limit")?;
        let credit=MetadataPermit::acquire(charge).map_err(|_|"mask revision metadata budget")?;
        let mut entries=HashMap::new();
        entries.try_reserve(assets.len()).map_err(|_|"mask revision sample allocation")?;
        for &asset in assets {
            let path=asset.path();if entries.contains_key(path){continue;}
            if !self.entries.contains_key(path){
                if self.entries.len()>=MAX_PATHS {
                    let victim=self.entries.iter().filter(|(path,e)|e.evictable()&&!assets.iter().any(|asset|asset.path()==path.as_str())).min_by_key(|(_,e)|e.touched).map(|(p,_)|p.clone()).ok_or("mask revision registry busy")?;
                    self.entries.remove(&victim);
                }
                let entry_credit=MetadataPermit::acquire(path.len()+std::mem::size_of::<(String,Entry)>()+MAX_HISTORY*2*std::mem::size_of::<Weak<Revision>>()+128).map_err(|_|"mask revision metadata budget")?;
                let version=self.version(domain,*asset.sha256(),None)?;
                let touched=self.touch()?;
                self.entries.insert(path.to_owned(),Entry{version,history:Vec::new(),touched,_credit:entry_credit});
            }
            let reset=self.entries.get(path).is_some_and(|e|e.version.domain!=domain);
            if reset {
                let hash=self.entries.get(path).ok_or("mask revision disappeared")?.version.hash;
                let version=self.version(domain,hash,None)?;
                self.entries.get_mut(path).ok_or("mask revision disappeared")?.install(version)?;
            }
            let touched=self.touch()?;
            let entry=self.entries.get_mut(path).ok_or("mask revision disappeared")?;entry.touched=touched;
            entries.insert(path.to_owned(),entry.version.clone());
        }
        Ok(Sample{entries,generation:self.generation,bytes:charge,_credit:credit})
    }
    pub(crate) fn current(&self,path:&str,expected:&Arc<Revision>)->bool{self.entries.get(path).is_some_and(|entry|Arc::ptr_eq(&entry.version,expected))}
    /// Publish after bounded worker read. The stale sample must be retried.
    pub(crate) fn publish(&mut self,path:&str,expected:&Arc<Revision>,hash:[u8;32],now:MediaTime)->Result<(),&'static str>{
        let current=self.entries.get(path).ok_or("mask revision retired")?;
        if !Arc::ptr_eq(&current.version,expected){return Err("mask revision changed; retry");}
        if expected.hash==hash{return Ok(());}
        let next=self.version(expected.domain,hash,Some(now))?;
        self.entries.get_mut(path).ok_or("mask revision retired")?.install(next)?;
        Err("mask file changed; retry with a new playback snapshot")
    }
}

/// Collect the immutable snapshot's reachable resource closure without file I/O.
pub(crate) fn sample_scene(book:&mut Book,domain:u64,model:&skylive_core::StudioSnapshot,views:&[crate::transition::FrameScene])->Result<Option<Arc<Sample>>,&'static str>{
    // Charge conservative vector/hash-table capacity in geometric steps.
    // The permits live until the temporary resource closure has been consumed.
    #[derive(Default)]
    struct Workspace {covered:usize,permits:Vec<MetadataPermit>}
    impl Workspace {
        fn admit(&mut self,count:usize)->Result<(),&'static str>{
            if count<=self.covered{return Ok(());}
            let next=self.covered.saturating_mul(2).max(8).min(MAX_PATHS);
            if count>next{return Err("mask revision workspace limit");}
            self.permits.try_reserve(1).map_err(|_|"mask revision workspace allocation")?;
            let slot=2*std::mem::size_of::<&skylive_core::ImageAsset>()+4*std::mem::size_of::<&str>();
            let charge=(next-self.covered).checked_mul(slot).and_then(|n|n.checked_add(if self.covered==0{256}else{0})).ok_or("mask revision workspace budget")?;
            self.permits.push(MetadataPermit::acquire(charge).map_err(|_|"mask revision workspace budget")?);
            self.covered=next;Ok(())
        }
    }
    fn collect<'a>(source:&'a skylive_core::Source,out:&mut Vec<&'a skylive_core::ImageAsset>,seen:&mut HashSet<&'a str>,workspace:&mut Workspace)->Result<(),&'static str>{
        for filter in source.video_filters(){if let Some(asset)=filter.mask_image(){
            // One path has one live revision, even when it is referenced by
            // several sources or by both a retained transition and the model.
            // Count unique paths before applying the admission limit so shared
            // resources cannot exhaust it solely through repeated references.
            if seen.contains(asset.path()){continue;}
            if out.len()>=MAX_PATHS{return Err("mask revision path limit");}
            workspace.admit(out.len()+1)?;
            out.try_reserve(1).map_err(|_|"mask revision workspace allocation")?;
            seen.try_reserve(1).map_err(|_|"mask revision workspace allocation")?;
            seen.insert(asset.path());out.push(asset);
        }}Ok(())
    }
    // Declare credit first so the tables are dropped before their permits.
    let mut workspace=Workspace::default();let mut assets=Vec::new();let mut seen=HashSet::new();
    for source in model.sources(){collect(source,&mut assets,&mut seen,&mut workspace)?;}
    for view in views{if let Some(source)=view.root_scene_source(model){collect(source,&mut assets,&mut seen,&mut workspace)?;}for source in view.visible_sources(model).map_err(|_|"mask revision scene unavailable")?{collect(source,&mut assets,&mut seen,&mut workspace)?;}}
    if assets.is_empty(){return Ok(None);}
    Ok(Some(Arc::new(book.sample(domain,&assets)?)))
}
