//! Physical native AAC contexts, including direct recording adapters and
//! asynchronous streaming/replay workers. Count survives actual COM teardown.
use super::Error;use std::sync::atomic::{AtomicUsize,Ordering};
pub(crate) const MAX_CONTEXTS:usize=8;
static ACTIVE:AtomicUsize=AtomicUsize::new(0);
pub(crate) struct Permit{_private:()}
impl Drop for Permit{fn drop(&mut self){ACTIVE.fetch_sub(1,Ordering::AcqRel);}}
pub(crate) struct Reservation{remaining:usize}
impl Reservation{pub fn acquire(count:usize)->Result<Self,Error>{if count>6{return Err(Error::Limit("recording AAC reservation exceeds6tracks"));}ACTIVE.fetch_update(Ordering::AcqRel,Ordering::Acquire,|active|active.checked_add(count).filter(|n|*n<=MAX_CONTEXTS)).map_err(|_|Error::Limit("eight physical AAC contexts active or retiring; selected recording tracks must reserve together"))?;Ok(Self{remaining:count})}pub fn take(&mut self)->Result<Permit,Error>{if self.remaining==0{return Err(Error::Invalid("AAC reservation exhausted"));}self.remaining-=1;Ok(Permit{_private:()})}}
impl Drop for Reservation{fn drop(&mut self){ACTIVE.fetch_sub(self.remaining,Ordering::AcqRel);}}
pub(crate) fn one()->Result<Permit,Error>{Reservation::acquire(1)?.take()}
