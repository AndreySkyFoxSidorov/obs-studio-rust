use super::*;
use std::{sync::mpsc, thread};

fn normalized() -> Vec<f32> {
    vec![0.0; INPUT_ELEMENTS]
}
fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready() {
        assert!(Instant::now() < deadline, "bounded owner test deadline");
        thread::yield_now();
    }
}
fn values(bytes: &[u8], expected: usize) -> Vec<f32> {
    assert_eq!(bytes.len(), expected * 4);
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}

#[test]
fn real_portrait_matches_independent_onnx_probabilities_and_exact_half_threshold_mask() {
    let input = values(
        include_bytes!("../tests/data/portrait-input.f32"),
        INPUT_ELEMENTS,
    );
    let expected = values(
        include_bytes!("../tests/data/portrait-onnx-reference-mask.f32"),
        MASK_ELEMENTS,
    );
    assert!(
        expected
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
    );
    let shared = Shared::new();
    let owner = shared.owner().unwrap();
    for (run, wait) in [true, false].into_iter().enumerate() {
        let result = evaluate_in(&shared, &input, wait).unwrap();
        assert_eq!(result.timing.initialized, run == 0);
        if run != 0 {
            assert_eq!(result.timing.model_load, Duration::ZERO);
        }
        assert_eq!(result.probabilities.len(), MASK_ELEMENTS);
        let mut maximum = 0.0_f32;
        let mut union = 0_usize;
        let mut intersection = 0_usize;
        for (&actual, &reference) in result.probabilities.iter().zip(&expected) {
            assert!(actual.is_finite() && (0.0..=1.0).contains(&actual));
            maximum = maximum.max((actual - reference).abs());
            let a = actual >= 0.5;
            let b = reference >= 0.5;
            union += usize::from(a || b);
            intersection += usize::from(a && b);
        }
        assert!(maximum <= 2.0e-5, "raw probability error {maximum}");
        assert!(union > 0);
        assert_eq!(intersection, union, "threshold0.5 IoU must be exactly1.0");
        assert!(matches!(*shared.cache.lock().unwrap(), Cache::Ready(_)));
        assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    }
    assert!(matches!(shutdown_in(&shared), Err(Error::Busy)));
    drop(owner);
    assert!(matches!(*shared.cache.lock().unwrap(), Cache::Empty));
    let cold_again = evaluate_in(&shared, &input, true).unwrap();
    assert!(cold_again.timing.initialized);
    shutdown_in(&shared).unwrap();
}

#[test]
fn invalid_length_and_numeric_input_do_not_initialize_the_cached_model() {
    let shared = Shared::new();
    for length in [0, INPUT_ELEMENTS - 1, INPUT_ELEMENTS + 1] {
        let error = evaluate_in(&shared, &vec![0.0; length], false)
            .err()
            .unwrap();
        assert!(matches!(error,Error::InputLength{actual}if actual==length));
    }
    let mut input = normalized();
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.001, 1.001] {
        input[12] = value;
        let error = evaluate_in(&shared, &input, true).err().unwrap();
        assert!(matches!(error, Error::InputValue { index: 12, .. }));
        assert!(matches!(*shared.cache.lock().unwrap(), Cache::Empty));
        assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    }
}

#[test]
fn busy_admission_never_waits_for_an_active_model_owner() {
    let shared = Shared::new();
    let active = shared.enter(false).unwrap();
    assert!(matches!(
        evaluate_in(&shared, &normalized(), false),
        Err(Error::Busy)
    ));
    assert!(matches!(shutdown_in(&shared), Err(Error::Busy)));
    assert_eq!(shared.admitted.load(Ordering::Acquire), 1);
    drop(active);
    assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    shutdown_in(&shared).unwrap();
}

#[test]
fn four_admitted_background_callers_are_fifo_and_a_fifth_is_rejected() {
    let shared = Arc::new(Shared::new());
    let active = shared.enter(false).unwrap();
    let (tx, rx) = mpsc::sync_channel(3);
    let mut handles = vec![];
    for index in 0..3 {
        let owner = shared.clone();
        let output = tx.clone();
        handles.push(thread::spawn(move || {
            let _lease = owner.enter(true).unwrap();
            output.send(index).unwrap();
        }));
        wait_until(|| shared.queue.lock().unwrap().len == index + 1);
    }
    assert_eq!(shared.admitted.load(Ordering::Acquire), MAX_ADMITTED);
    assert!(matches!(shared.enter(true), Err(Error::Busy)));
    assert!(matches!(shared.enter(false), Err(Error::Busy)));
    drop(active);
    assert_eq!(
        (0..3)
            .map(|_| rx.recv_timeout(Duration::from_secs(3)).unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    let queue = shared.queue.lock().unwrap();
    assert!(!queue.active);
    assert_eq!(queue.len, 0);
}

#[test]
fn wait_ticket_exhaustion_releases_its_permit_without_dropping_active_admission() {
    let shared = Shared::new();
    let active = shared.enter(false).unwrap();
    shared.queue.lock().unwrap().next = u64::MAX;
    assert!(matches!(shared.enter(true), Err(Error::AdmissionExhausted)));
    assert_eq!(shared.admitted.load(Ordering::Acquire), 1);
    assert_eq!(shared.queue.lock().unwrap().len, 0);
    drop(active);
    shutdown_in(&shared).unwrap();
    assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
}

#[test]
fn cached_load_failure_preserves_original_diagnostic_and_shutdown_clears_it() {
    let shared = Shared::new();
    let owner = shared.owner().unwrap();
    let original = "original vendor failure: model.operator[18]\nprecise diagnostic";
    *shared.cache.lock().unwrap() = Cache::Failed(Error::ModelLoad(original.into()));
    for wait in [false, true] {
        let error = evaluate_in(&shared, &normalized(), wait).err().unwrap();
        assert_eq!(error.code(), "person_matte_model_load");
        assert_eq!(error.to_string(), original);
        assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    }
    assert!(matches!(shutdown_in(&shared), Err(Error::Busy)));
    drop(owner);
    assert!(matches!(*shared.cache.lock().unwrap(), Cache::Empty));
}

#[test]
fn vendor_unwind_is_a_typed_error_with_original_text() {
    let error = guarded::<()>(|| panic!("exact original vendor panic")).unwrap_err();
    assert!(matches!(error, Error::Panicked(_)));
    assert_eq!(error.to_string(), "exact original vendor panic");
}

#[test]
fn poisoned_admission_wakes_waiters_and_returns_poison_instead_of_silent_recovery() {
    let shared = Arc::new(Shared::new());
    let active = shared.enter(false).unwrap();
    let waiting = shared.clone();
    let waiter = thread::spawn(move || matches!(waiting.enter(true), Err(Error::Poisoned)));
    wait_until(|| shared.queue.lock().unwrap().len == 1);
    let corrupt = shared.clone();
    let poison = thread::spawn(move || {
        let _guard = corrupt.queue.lock().unwrap();
        panic!("admission poison fixture");
    });
    assert!(poison.join().is_err());
    drop(active);
    assert!(waiter.join().unwrap());
    assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    assert!(matches!(shared.enter(false), Err(Error::Poisoned)));
}
