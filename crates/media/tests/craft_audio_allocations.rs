//! Instrumentation only: production kernels contain no unsafe code or allocator.
use skylive_media::craft_audio::{Gate, GateParameters, SimpleEq, SimpleEqParameters};
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
fn construction_updates_curves_planar_interleaved_and_reset_allocate_and_free_nothing() {
    let mut packed = [0.01f32; 32768];
    let mut planar = [[0.01f32; 128]; 8];
    // Initialize TLS outside the measured interval; allocations from other test
    // threads cannot pollute this thread-local measurement.
    ACTIVE.with(|_| {});
    ALLOCATIONS.with(|_| {});
    DEALLOCATIONS.with(|_| {});
    {
        let _counting = Counting::start();
        let gate_parameters = GateParameters::new(-20.0, 4.0, 60.0, 1.0, 2.0, 150.0).unwrap();
        let eq_parameters =
            SimpleEqParameters::new(6.0, 200.0, -3.0, 1000.0, 0.7, 9.0, 5000.0, -2.0).unwrap();
        let mut gate = Gate::new(48000, 8, gate_parameters).unwrap();
        let mut eq = SimpleEq::new(48000, 8, eq_parameters).unwrap();
        let mut obs_gain = skylive_media::obs_audio::ObsGain::new(0.0).unwrap();
        for _ in 0..4 {
            obs_gain.update(6.0).unwrap();
            obs_gain
                .process_interleaved(black_box(&mut packed), 8, Default::default())
                .unwrap();
            skylive_media::obs_audio::invert_polarity_interleaved(
                black_box(&mut packed),
                8,
                Default::default(),
            )
            .unwrap();
            gate.process_interleaved(black_box(&mut packed)).unwrap();
            eq.process_interleaved(black_box(&mut packed)).unwrap();
            let mut slices = planar.each_mut().map(|channel| channel.as_mut_slice());
            gate.process_planar(black_box(&mut slices)).unwrap();
            eq.process_planar(black_box(&mut slices)).unwrap();
            gate.set_parameters(GateParameters::default());
            eq.set_parameters(SimpleEqParameters::default());
            black_box(gate.transfer_db(-40.0).unwrap());
            black_box(eq.response_db(1000.0).unwrap());
            black_box(gate.gain_reduction_db());
            gate.reset();
            eq.reset();
        }
    }
    assert_eq!(
        ALLOCATIONS.get(),
        0,
        "CPU kernels allocated during measured operations"
    );
    assert_eq!(
        DEALLOCATIONS.get(),
        0,
        "CPU kernels freed heap state during measured operations"
    );
    assert!(packed.iter().all(|sample| sample.is_finite()));
}
