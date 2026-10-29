//! Repair-only cooperative metadata admission, not allocator/process isolation.
use super::*;
use std::sync::atomic::{AtomicBool,Ordering};
pub const METADATA_BYTES:usize=192*1024*1024;
static OCCUPIED:AtomicBool=AtomicBool::new(false);
struct Credit;
impl Drop for Credit{fn drop(&mut self){OCCUPIED.store(false,Ordering::Release);}}
/// Shared only with this exact worker candidate/transfer, never a model copy.
#[derive(Clone)]pub struct Lease{_credit:Arc<Credit>}
impl Lease{
 pub(super) fn reserve()->Result<Self,Error>{OCCUPIED.compare_exchange(false,true,Ordering::AcqRel,Ordering::Acquire).map_err(|_|limit("The 192 MiB pending-open metadata credit is still owned or retiring"))?;Ok(Self{_credit:Arc::new(Credit)})}
}
impl std::fmt::Debug for Lease{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str("RepairMetadataLease(192 MiB cooperative)")}}
struct Count(usize);
impl std::io::Write for Count{
 fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{self.0=self.0.checked_add(bytes.len()).filter(|n|*n<=12*1024*1024).ok_or_else(||std::io::Error::other("Pending-open serialized metadata exceeds 12 MiB"))?;Ok(bytes.len())}
 fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
/// Borrowed preflight BEFORE old/provisional candidate cloning. Decode scratch
/// is governed by existing independent asset/media/directory limits.
pub(super) fn preflight(model:&StudioSnapshot)->Result<(),Error>{preflight_inner(model,None,0)}
pub(super) fn adoption(model:&StudioSnapshot,prefs:&skylive_core::UserPreferences)->Result<(),Error>{preflight_inner(model,Some(prefs),0)}
pub(super) fn relink(model:&StudioSnapshot,batch:&skylive_core::ResourceRelinks)->Result<(),Error>{let extra=batch.rows.iter().try_fold(0usize,|n,r|n.checked_add(r.replacement.metadata_bytes()).ok_or_else(||limit("Pending relink metadata overflow")))?;preflight_inner(model,None,extra)}
pub(super) fn folders(model:&StudioSnapshot)->Result<(),Error>{preflight_inner(model,None,crate::image_slideshow::directories::MAX_PREPARED_METADATA)}
fn preflight_inner(model:&StudioSnapshot,prefs:Option<&skylive_core::UserPreferences>,extra:usize)->Result<(),Error>{
 let mut count=Count(0);serde_json::to_writer(&mut count,model).map_err(|e|limit(e.to_string()))?;if let Some(prefs)=prefs{serde_json::to_writer(&mut count,prefs).map_err(|e|limit(e.to_string()))?;}
 let scenes=model.scenes().len().checked_add(model.groups().len()).ok_or_else(||limit("Pending-open structural count overflow"))?;
 let items=model.scenes().iter().chain(model.groups()).try_fold(0usize,|n,s|n.checked_add(s.items().len()).ok_or_else(||limit("Pending-open item count overflow")))?;
 let structural=model.sources().len().checked_mul(4*std::mem::size_of::<skylive_core::Source>())
  .and_then(|n|n.checked_add(scenes.checked_mul(4*std::mem::size_of::<skylive_core::Scene>())?))
  .and_then(|n|n.checked_add(items.checked_mul(4*std::mem::size_of::<skylive_core::SceneItem>())?)).ok_or_else(||limit("Pending-open structural charge overflow"))?;
 let overhead=2*storage::MAX_FILE_BYTES as usize+2*skylive_core::MAX_RELINK_METADATA+2*crate::missing_files::MAX_PAGE_METADATA+2*storage::MAX_MIGRATION_REPORT_BYTES+2*crate::collection_library::MAX_INDEX_BYTES+1024*1024;
 let bytes=count.0.checked_mul(8).and_then(|n|n.checked_add(structural)).and_then(|n|n.checked_add(overhead)).and_then(|n|n.checked_add(extra.checked_mul(4)?)).ok_or_else(||limit("Pending-open metadata charge overflow"))?;
 if bytes>METADATA_BYTES{return Err(limit("Old and provisional pending-open metadata exceed 192 MiB"));}Ok(())
}
