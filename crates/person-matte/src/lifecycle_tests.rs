use super::*;
use std::{sync::mpsc, thread};

fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "lifecycle test deadline");
        thread::yield_now();
    }
}
fn portrait() -> Vec<f32> {
    include_bytes!("../tests/data/portrait-input.f32")
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect()
}

#[test]
fn overlapping_owners_are_lazy_and_manual_shutdown_cannot_retire_another_service() {
    let shared = Shared::new();
    let preview = shared.owner().unwrap();
    let program = shared.owner().unwrap();
    assert_eq!(shared.queue.lock().unwrap().owners, 2);
    assert!(matches!(*shared.cache.lock().unwrap(), Cache::Empty));
    *shared.cache.lock().unwrap() = Cache::Failed(Error::ModelLoad("retained failure".into()));
    drop(preview);
    assert_eq!(shared.queue.lock().unwrap().owners, 1);
    assert!(matches!(*shared.cache.lock().unwrap(), Cache::Failed(_)));
    assert!(matches!(shutdown_in(&shared), Err(Error::Busy)));
    drop(program);
    assert!(matches!(*shared.cache.lock().unwrap(), Cache::Empty));
    assert!(!shared.queue.lock().unwrap().active);
}

#[test]
fn final_owner_close_defers_retirement_until_all_admitted_evaluations_settle() {
    let shared = Arc::new(Shared::new());
    let owner = shared.owner().unwrap();
    *shared.cache.lock().unwrap() = Cache::Failed(Error::ModelLoad("work still owns cache".into()));
    let active = shared.enter(false).unwrap();
    let waiting = shared.clone();
    let (arrived, arrival) = mpsc::sync_channel(1);
    let (finish, finished) = mpsc::sync_channel(1);
    let handle = thread::spawn(move || {
        let _lease = waiting.enter(true).unwrap();
        assert!(matches!(*waiting.cache.lock().unwrap(), Cache::Failed(_)));
        arrived.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    wait_until(|| shared.queue.lock().unwrap().len == 1);
    drop(owner);
    assert!(matches!(*shared.cache.lock().unwrap(), Cache::Failed(_)));
    drop(active);
    arrival.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(*shared.cache.lock().unwrap(), Cache::Failed(_)));
    finish.send(()).unwrap();
    handle.join().unwrap();
    assert!(matches!(*shared.cache.lock().unwrap(), Cache::Empty));
    assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
}

#[test]
fn actual_model_is_warm_until_last_same_thread_owner_drop_then_fully_retired() {
    let shared = Arc::new(Shared::new());
    let background = shared.clone();
    let handle = thread::spawn(move || {
        let owner = background.owner().unwrap();
        let input = portrait();
        assert!(
            evaluate_in(&background, &input, true)
                .unwrap()
                .timing
                .initialized
        );
        assert!(
            !evaluate_in(&background, &input, true)
                .unwrap()
                .timing
                .initialized
        );
        assert!(matches!(shutdown_in(&background), Err(Error::Busy)));
        drop(owner);
        assert!(matches!(*background.cache.lock().unwrap(), Cache::Empty));
        assert_eq!(
            *background.retired_on.lock().unwrap(),
            Some(thread::current().id())
        );
    });
    let owner_thread = handle.thread().id();
    handle.join().unwrap();
    assert_eq!(*shared.retired_on.lock().unwrap(), Some(owner_thread));
    assert_ne!(owner_thread, thread::current().id());
    assert_eq!(shared.queue.lock().unwrap().owners, 0);
}

#[test]
fn new_service_during_actual_pool_retirement_queues_before_loading_its_replacement() {
    let shared = Arc::new(Shared::new());
    let old = shared.clone();
    let (ready, readiness) = mpsc::sync_channel(1);
    let (close, closing) = mpsc::sync_channel(1);
    let old_thread = thread::spawn(move || {
        let owner = old.owner().unwrap();
        assert!(
            evaluate_in(&old, &portrait(), true)
                .unwrap()
                .timing
                .initialized
        );
        ready.send(()).unwrap();
        closing.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(owner);
    });
    readiness.recv_timeout(Duration::from_secs(5)).unwrap();
    // Pause retirement before it can take/drop the real model. This mutex is
    // strictly a deterministic test barrier, not a production retirement queue.
    let old_cache = shared.cache.lock().unwrap();
    close.send(()).unwrap();
    wait_until(|| {
        let queue = shared.queue.lock().unwrap();
        queue.owners == 0 && queue.active
    });
    let new = shared.clone();
    let (registered, registration) = mpsc::sync_channel(1);
    let new_thread = thread::spawn(move || {
        let owner = new.owner().unwrap();
        registered.send(()).unwrap();
        let frame = evaluate_in(&new, &portrait(), true).unwrap();
        assert!(
            frame.timing.initialized,
            "old pool was retired before replacement"
        );
        drop(owner);
    });
    registration.recv_timeout(Duration::from_secs(5)).unwrap();
    wait_until(|| shared.queue.lock().unwrap().len == 1);
    assert!(matches!(*old_cache, Cache::Ready(_)));
    assert!(matches!(shared.enter(false), Err(Error::Busy)));
    drop(old_cache);
    old_thread.join().unwrap();
    new_thread.join().unwrap();
    assert!(matches!(*shared.cache.lock().unwrap(), Cache::Empty));
    assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    assert_eq!(shared.queue.lock().unwrap().owners, 0);
}

#[test]
fn owner_counter_exhaustion_and_poison_are_typed_and_never_silently_resume_work() {
    let shared = Arc::new(Shared::new());
    shared.queue.lock().unwrap().owners = usize::MAX;
    assert!(matches!(shared.owner(), Err(Error::AdmissionExhausted)));
    assert!(matches!(
        shared.enter(false),
        Err(Error::AdmissionExhausted)
    ));
    assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    shared.queue.lock().unwrap().owners = 0;
    let owner = shared.owner().unwrap();
    *shared.cache.lock().unwrap() = Cache::Failed(Error::ModelLoad("terminal state".into()));
    let corrupt = shared.clone();
    assert!(
        thread::spawn(move || {
            let _lock = corrupt.queue.lock().unwrap();
            panic!("owner poison fixture");
        })
        .join()
        .is_err()
    );
    assert!(matches!(shared.owner(), Err(Error::Poisoned)));
    drop(owner);
    assert!(matches!(*shared.cache.lock().unwrap(), Cache::Empty));
    assert!(matches!(shared.enter(false), Err(Error::Poisoned)));
}

#[test]
fn explicit_load_pool_preserves_onnx_probabilities_with_optimization_and_prepacking() {
    let pool = Arc::new(rten::ThreadPool::with_num_threads_owned(THREAD_LIMIT));
    let mut options = ModelOptions::with_all_ops();
    options.with_thread_pool(pool.clone()).prepack_weights(true);
    let model = options.load_static_slice(MODEL_BYTES).unwrap();
    let input = portrait();
    let tensor =
        TensorView::try_from_data(&[1, HEIGHT, WIDTH, INPUT_CHANNELS], input.as_slice()).unwrap();
    let output = model
        .run_one(
            tensor.into(),
            Some(RunOptions {
                thread_pool: Some(pool.clone()),
                ..Default::default()
            }),
        )
        .unwrap();
    let probabilities = Tensor::<f32>::try_from(output).unwrap();
    assert_eq!(probabilities.shape(), [1, HEIGHT, WIDTH, 1]);
    let reference = include_bytes!("../tests/data/portrait-onnx-reference-mask.f32")
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()));
    let mut maximum = 0.0_f32;
    let mut foreground = 0;
    for (&actual, expected) in probabilities.iter().zip(reference) {
        assert!(actual.is_finite() && (0.0..=1.0).contains(&actual));
        maximum = maximum.max((actual - expected).abs());
        assert_eq!(actual >= 0.5, expected >= 0.5);
        foreground += usize::from(expected >= 0.5);
    }
    assert!(foreground > 0);
    assert!(
        maximum <= 2.0e-5,
        "prepacked ONNX probability error {maximum}"
    );
    // Options and output/model are released before the final owning pool drop.
    drop(probabilities);
    drop(model);
    drop(options);
    assert_eq!(Arc::strong_count(&pool), 1);
    drop(pool);
}
