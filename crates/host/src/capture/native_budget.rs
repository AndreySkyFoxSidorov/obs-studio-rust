//! Logical native pixel-copy reservation, not opaque driver residency.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
const TOTAL: usize = 512 * 1024 * 1024;
const WGC: usize = 384 * 1024 * 1024;
const DXGI: usize = 256 * 1024 * 1024;
#[derive(Clone, Copy)]
pub(crate) enum NativeCopyKind {
    Wgc,
    Dxgi,
}
#[derive(Default)]
struct Counts {
    total: AtomicUsize,
    wgc: AtomicUsize,
    dxgi: AtomicUsize,
}
#[derive(Clone, Default)]
pub(crate) struct NativeCopyBudget(Arc<Counts>);
pub(crate) struct NativeCopyLease {
    budget: NativeCopyBudget,
    kind: NativeCopyKind,
    bytes: usize,
}
impl NativeCopyBudget {
    pub(crate) fn reserve(&self, kind: NativeCopyKind, bytes: usize) -> Option<NativeCopyLease> {
        let (counter, limit) = match kind {
            NativeCopyKind::Wgc => (&self.0.wgc, WGC),
            NativeCopyKind::Dxgi => (&self.0.dxgi, DXGI),
        };
        let add = |n: usize| n.checked_add(bytes).filter(|n| *n <= limit);
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, add)
            .ok()?;
        if self
            .0
            .total
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(bytes).filter(|n| *n <= TOTAL)
            })
            .is_err()
        {
            counter.fetch_sub(bytes, Ordering::AcqRel);
            return None;
        }
        Some(NativeCopyLease {
            budget: self.clone(),
            kind,
            bytes,
        })
    }
}
impl Drop for NativeCopyLease {
    fn drop(&mut self) {
        match self.kind {
            NativeCopyKind::Wgc => &self.budget.0.wgc,
            NativeCopyKind::Dxgi => &self.budget.0.dxgi,
        }
        .fetch_sub(self.bytes, Ordering::AcqRel);
        self.budget.0.total.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_and_combined_bounds_are_reserved_before_allocation() {
        let b = NativeCopyBudget::default();
        let a = b.reserve(NativeCopyKind::Wgc, 384 * 1024 * 1024).unwrap();
        assert!(b.reserve(NativeCopyKind::Wgc, 1).is_none());
        let d = b.reserve(NativeCopyKind::Dxgi, 128 * 1024 * 1024).unwrap();
        assert!(b.reserve(NativeCopyKind::Dxgi, 1).is_none());
        drop(a);
        let e = b.reserve(NativeCopyKind::Dxgi, 128 * 1024 * 1024).unwrap();
        assert!(b.reserve(NativeCopyKind::Dxgi, 1).is_none());
        assert!(b.reserve(NativeCopyKind::Wgc, usize::MAX).is_none());
        drop((d, e));
        assert_eq!(b.0.total.load(Ordering::Acquire), 0);
    }
}
