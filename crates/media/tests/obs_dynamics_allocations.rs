//! Instrumentation only: production kernels contain no unsafe code or allocator.
use skylive_media::obs_dynamics::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    hint::black_box,
};

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static DEALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
struct CountedSystem;
#[global_allocator]
static ALLOCATOR: CountedSystem = CountedSystem;
fn record(allocates: bool) {
    let _ = ACTIVE.try_with(|active| {
        if active.get() {
            let counter = if allocates {
                &ALLOCATIONS
            } else {
                &DEALLOCATIONS
            };
            let _ = counter.try_with(|count| count.set(count.get().saturating_add(1)));
        }
    });
}
// SAFETY: Each method forwards the original Layout/pointer to System, preserving
// its allocation contract. The additional thread-local counters own no heap data.
unsafe impl GlobalAlloc for CountedSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(true);
        // SAFETY: Caller supplies the GlobalAlloc layout contract unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(true);
        // SAFETY: Caller supplies the GlobalAlloc layout contract unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record(false);
        // SAFETY: The same allocator and caller-provided allocation layout apply.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(true);
        // SAFETY: Forward the original allocation and requested nonzero size.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
struct Counting;
impl Counting {
    fn start() -> Self {
        ALLOCATIONS.set(0);
        DEALLOCATIONS.set(0);
        ACTIVE.set(true);
        Self
    }
}
impl Drop for Counting {
    fn drop(&mut self) {
        ACTIVE.set(false);
    }
}

#[test]
fn obs_dynamics_construct_update_process_reset_allocate_and_free_nothing() {
    let mut packed = [0.01f32; 32768];
    let mut envelope = [0.0f32; 4096];
    ACTIVE.with(|_| {});
    ALLOCATIONS.with(|_| {});
    DEALLOCATIONS.with(|_| {});
    {
        let _counting = Counting::start();
        let mut limiter = ObsLimiter::new(48000, 8, Default::default()).unwrap();
        let mut compressor = ObsCompressor::new(48000, 8, Default::default()).unwrap();
        let mut expander = ObsExpander::new(48000, 8, Default::default()).unwrap();
        for _ in 0..4 {
            packed.fill(0.01);
            limiter.set_parameters(ObsLimiterParameters::new(-12., 80).unwrap());
            compressor.set_parameters(ObsCompressorParameters::new(-24., 4., 10, 100, 2.).unwrap());
            expander.set_parameters(ObsExpanderParameters::gate_preset());
            limiter
                .process_callback(
                    black_box(&mut packed),
                    black_box(&mut envelope),
                    Default::default(),
                )
                .unwrap();
            compressor
                .process_callback(
                    black_box(&mut packed),
                    black_box(&mut envelope),
                    Default::default(),
                )
                .unwrap();
            expander
                .process_interleaved(black_box(&mut packed), Default::default())
                .unwrap();
            black_box(limiter.parameters().transfer_db(-3.).unwrap());
            black_box(compressor.parameters().transfer_db(-3.).unwrap());
            black_box(expander.parameters().transfer_db(-50.).unwrap());
            limiter.reset();
            compressor.reset();
            expander.reset();
        }
    }
    assert_eq!(ALLOCATIONS.get(), 0);
    assert_eq!(DEALLOCATIONS.get(), 0);
}
