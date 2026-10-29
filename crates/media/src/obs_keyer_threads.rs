//! Process-wide, nonblocking admission for at most two scoped chroma helpers.
use std::sync::atomic::{AtomicU8, Ordering};

pub(super) const STACK_BYTES: usize = 256 * 1024;
pub(super) struct Admission {
    occupied: AtomicU8,
}
pub(super) struct Permit<'a> {
    owner: &'a Admission,
    bit: u8,
}
impl Admission {
    pub(super) const fn new() -> Self {
        Self {
            occupied: AtomicU8::new(0),
        }
    }
    pub(super) fn try_acquire(&self) -> Option<Permit<'_>> {
        // Two atomic attempts, with no retry/spin/wait. A failed fetch_or leaves
        // an already-occupied bit unchanged and confers no ownership.
        for bit in [1, 2] {
            if self.occupied.fetch_or(bit, Ordering::Acquire) & bit == 0 {
                return Some(Permit { owner: self, bit });
            }
        }
        None
    }
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.owner.occupied.fetch_and(!self.bit, Ordering::Release);
    }
}
static ADMISSION: Admission = Admission::new();
pub(super) fn try_acquire() -> Option<Permit<'static>> {
    ADMISSION.try_acquire()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_two_permits_and_release_reuses_only_the_owned_slot() {
        let admission = Admission::new();
        let first = admission.try_acquire().unwrap();
        let second = admission.try_acquire().unwrap();
        assert_ne!(first.bit, second.bit);
        assert!(admission.try_acquire().is_none());
        drop(first);
        let replacement = admission.try_acquire().unwrap();
        assert_ne!(replacement.bit, second.bit);
        assert!(admission.try_acquire().is_none());
        drop(second);
        drop(replacement);
        assert_eq!(admission.occupied.load(Ordering::Acquire), 0);
        assert_eq!(STACK_BYTES, 262_144);
    }
}
