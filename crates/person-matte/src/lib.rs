//! Fixed-model person matte execution. No media/UI/OS/file/network dependency.
//! Caller owns RGB preprocessing, source alpha, result freshness and mask bytes.
#![forbid(unsafe_code)]

use rten::{Model, ModelOptions, RunOptions};
use rten_tensor::{Tensor, TensorView, prelude::*};
use std::{
    fmt,
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex, TryLockError,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub const WIDTH: usize = 256;
pub const HEIGHT: usize = 256;
pub const INPUT_CHANNELS: usize = 3;
pub const INPUT_ELEMENTS: usize = WIDTH * HEIGHT * INPUT_CHANNELS;
pub const MASK_ELEMENTS: usize = WIDTH * HEIGHT;
pub const THREAD_LIMIT: usize = 2;
pub const MAX_ADMITTED: usize = 4;
pub const ORIGINAL_ONNX_SHA256: &str =
    "1511ab5564cb0ae179cba9bcad6ad931003ccc71656f11fd270967023678c6a5";
pub const MODEL_SHA256: &str = "cab9018306a5f1fbfa1f0e9d29d32e86fdf2eb495bd439da5df6b8e63e2e7d1c";
pub const MODEL_ID: &str = "selfie256-rten021-raw-probabilities-v1";
static MODEL_BYTES: &[u8; 473216] = include_bytes!("../models/selfie-segmentation.rten");

#[derive(Clone, Debug)]
pub enum Error {
    Busy,
    AdmissionExhausted,
    Poisoned,
    InputLength { actual: usize },
    InputValue { index: usize, value: f32 },
    ModelLoad(String),
    Inference(String),
    OutputShape(Vec<usize>),
    OutputLength { actual: usize },
    OutputValue { index: usize, value: f32 },
    Panicked(String),
}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Busy => "person_matte_busy",
            Self::AdmissionExhausted => "person_matte_admission_exhausted",
            Self::Poisoned => "person_matte_poisoned",
            Self::InputLength { .. } | Self::InputValue { .. } => "person_matte_invalid_input",
            Self::ModelLoad(_) => "person_matte_model_load",
            Self::Inference(_) => "person_matte_inference",
            Self::OutputShape(_) | Self::OutputLength { .. } | Self::OutputValue { .. } => {
                "person_matte_invalid_output"
            }
            Self::Panicked(_) => "person_matte_panicked",
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => write!(f, "person matte admission is busy"),
            Self::AdmissionExhausted => write!(f, "person matte admission identity exhausted"),
            Self::Poisoned => write!(f, "person matte owner is poisoned"),
            Self::InputLength { actual } => write!(
                f,
                "person matte input has {actual} values; expected {INPUT_ELEMENTS}"
            ),
            Self::InputValue { index, value } => write!(
                f,
                "person matte input[{index}]={value} is not finite normalized RGB"
            ),
            Self::ModelLoad(detail) | Self::Inference(detail) | Self::Panicked(detail) => {
                f.write_str(detail)
            }
            Self::OutputShape(shape) => write!(
                f,
                "person matte output shape {shape:?}; expected [1,256,256,1]"
            ),
            Self::OutputLength { actual } => write!(
                f,
                "person matte output has {actual} values; expected {MASK_ELEMENTS}"
            ),
            Self::OutputValue { index, value } => write!(
                f,
                "person matte output[{index}]={value} is not a finite probability"
            ),
        }
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub initialized: bool,
    pub model_load: Duration,
    pub inference: Duration,
    pub total: Duration,
}
pub struct Inference {
    pub probabilities: Vec<f32>,
    pub timing: Timing,
}

struct Runtime {
    model: Model,
    pool: Arc<rten::ThreadPool>,
}
impl Runtime {
    fn load() -> Result<Self, Error> {
        let pool = Arc::new(rten::ThreadPool::with_num_threads_owned(THREAD_LIMIT));
        let mut options = ModelOptions::with_all_ops();
        options.with_thread_pool(pool.clone());
        let model = options
            .load_static_slice(MODEL_BYTES)
            .map_err(|error| Error::ModelLoad(error.to_string()))?;
        Ok(Self { model, pool })
    }
    fn run(&self, input: &[f32]) -> Result<Vec<f32>, Error> {
        let tensor = TensorView::try_from_data(&[1, HEIGHT, WIDTH, INPUT_CHANNELS], input)
            .map_err(|error| Error::Inference(error.to_string()))?;
        let value = self
            .model
            .run_one(
                tensor.into(),
                Some(RunOptions {
                    thread_pool: Some(self.pool.clone()),
                    ..Default::default()
                }),
            )
            .map_err(|error| Error::Inference(error.to_string()))?;
        let output =
            Tensor::<f32>::try_from(value).map_err(|error| Error::Inference(error.to_string()))?;
        if output.shape() != [1, HEIGHT, WIDTH, 1] {
            return Err(Error::OutputShape(output.shape().to_vec()));
        }
        let mut probabilities = Vec::with_capacity(MASK_ELEMENTS);
        for (index, &value) in output.iter().enumerate() {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(Error::OutputValue { index, value });
            }
            probabilities.push(value);
        }
        if probabilities.len() != MASK_ELEMENTS {
            return Err(Error::OutputLength {
                actual: probabilities.len(),
            });
        }
        Ok(probabilities)
    }
}
// One process service stores this fixed control block; boxing would add a
// separate allocation on cold load without reducing a per-source collection.
#[allow(clippy::large_enum_variant)]
enum Cache {
    Empty,
    Ready(Runtime),
    Failed(Error),
}
struct Queue {
    owners: usize,
    active: bool,
    next: u64,
    head: usize,
    len: usize,
    tickets: [u64; MAX_ADMITTED],
}
impl Queue {
    const fn new() -> Self {
        Self {
            owners: 0,
            active: false,
            next: 0,
            head: 0,
            len: 0,
            tickets: [0; MAX_ADMITTED],
        }
    }
    fn enqueue(&mut self) -> Result<u64, Error> {
        if self.len >= MAX_ADMITTED {
            return Err(Error::Busy);
        }
        let ticket = self.next;
        self.next = self.next.checked_add(1).ok_or(Error::AdmissionExhausted)?;
        self.tickets[(self.head + self.len) % MAX_ADMITTED] = ticket;
        self.len += 1;
        Ok(ticket)
    }
    fn first(&self) -> Option<u64> {
        (self.len > 0).then(|| self.tickets[self.head])
    }
    fn pop(&mut self) {
        self.head = (self.head + 1) % MAX_ADMITTED;
        self.len = self.len.saturating_sub(1);
    }
}
struct Shared {
    admitted: AtomicUsize,
    queue: Mutex<Queue>,
    wake: Condvar,
    cache: Mutex<Cache>,
    #[cfg(test)]
    retired_on: Mutex<Option<std::thread::ThreadId>>,
}
impl Shared {
    const fn new() -> Self {
        Self {
            admitted: AtomicUsize::new(0),
            queue: Mutex::new(Queue::new()),
            wake: Condvar::new(),
            cache: Mutex::new(Cache::Empty),
            #[cfg(test)]
            retired_on: Mutex::new(None),
        }
    }
    fn owner(&self) -> Result<Owner<'_>, Error> {
        let mut queue = self.queue.lock().map_err(|_| Error::Poisoned)?;
        queue.owners = queue
            .owners
            .checked_add(1)
            .ok_or(Error::AdmissionExhausted)?;
        Ok(Owner {
            shared: self,
            _same_thread: PhantomData,
        })
    }
    fn release_owner(&self) {
        let retire = {
            // Poison stays terminal for evaluation. Recover the guard only to
            // release owned resources, never to resume inference as success.
            let mut queue = self.queue.lock().unwrap_or_else(|error| {
                let mut queue = error.into_inner();
                // All waiters return Poisoned; their abandoned FIFO tickets
                // cannot keep terminal cached resources alive indefinitely.
                queue.len = 0;
                queue
            });
            queue.owners -= 1;
            if queue.owners == 0 && !queue.active && queue.len == 0 {
                queue.active = true;
                true
            } else {
                false
            }
        };
        if retire {
            self.retire_active();
        }
    }
    fn release_active(&self) {
        let retire = {
            let mut queue = self.queue.lock().unwrap_or_else(|error| {
                let mut queue = error.into_inner();
                queue.len = 0;
                queue
            });
            if queue.owners == 0 && queue.len == 0 {
                // Keep active admission until the old pool has actually died.
                true
            } else {
                queue.active = false;
                false
            }
        };
        if retire {
            self.retire_active();
        } else {
            self.wake.notify_all();
        }
    }
    fn retire_cache(&self) -> Result<(), Error> {
        let (previous, poisoned) = {
            let (mut cache, poisoned) = match self.cache.lock() {
                Ok(cache) => (cache, false),
                Err(error) => (error.into_inner(), true),
            };
            (std::mem::replace(&mut *cache, Cache::Empty), poisoned)
        };
        // Neither queue nor cache mutex is held across vendor destruction/join.
        // The active admission bit serializes the complete lifetime instead.
        let result = guarded(|| {
            drop(previous);
            Ok(())
        });
        #[cfg(test)]
        {
            *self.retired_on.lock().unwrap() = Some(std::thread::current().id());
        }
        if let Err(error) = &result {
            let mut cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
            *cache = Cache::Failed(error.clone());
        }
        if poisoned {
            Err(Error::Poisoned)
        } else {
            result
        }
    }
    fn retire_active(&self) {
        // Drop cannot return a typed error. A vendor unwind is retained in the
        // failed cache, and a poison flag remains terminal for future callers.
        let _ = self.retire_cache();
        let mut queue = self.queue.lock().unwrap_or_else(|error| error.into_inner());
        queue.active = false;
        drop(queue);
        self.wake.notify_all();
    }
    fn enter(&self, wait: bool) -> Result<Lease<'_>, Error> {
        self.enter_for(wait, false)
    }
    fn enter_for(&self, wait: bool, shutdown: bool) -> Result<Lease<'_>, Error> {
        let permit = Permit::new(&self.admitted)?;
        let mut queue = if wait {
            self.queue.lock().map_err(|_| Error::Poisoned)?
        } else {
            self.queue.try_lock().map_err(lock_error)?
        };
        if queue.owners == usize::MAX {
            return Err(Error::AdmissionExhausted);
        }
        if shutdown && queue.owners != 0 {
            return Err(Error::Busy);
        }
        if !queue.active && queue.len == 0 {
            queue.active = true;
            drop(queue);
            return Ok(Lease {
                shared: self,
                _permit: permit,
            });
        }
        if !wait {
            return Err(Error::Busy);
        }
        let ticket = queue.enqueue()?;
        while queue.active || queue.first() != Some(ticket) {
            queue = self.wake.wait(queue).map_err(|_| Error::Poisoned)?;
        }
        queue.pop();
        queue.active = true;
        drop(queue);
        Ok(Lease {
            shared: self,
            _permit: permit,
        })
    }
}
struct Permit<'a> {
    counter: &'a AtomicUsize,
}
impl<'a> Permit<'a> {
    fn new(counter: &'a AtomicUsize) -> Result<Self, Error> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1).filter(|next| *next <= MAX_ADMITTED)
            })
            .map_err(|_| Error::Busy)?;
        Ok(Self { counter })
    }
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Release);
    }
}
struct Lease<'a> {
    shared: &'a Shared,
    _permit: Permit<'a>,
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.shared.release_active();
    }
}
fn lock_error<T>(error: TryLockError<T>) -> Error {
    match error {
        TryLockError::WouldBlock => Error::Busy,
        TryLockError::Poisoned(_) => Error::Poisoned,
    }
}
fn guarded<T>(operation: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    catch_unwind(AssertUnwindSafe(operation)).map_err(|payload| {
        let detail = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|value| (*value).to_owned())
            })
            .unwrap_or_else(|| {
                format!(
                    "person matte panic with non-text payload type {:?}",
                    payload.as_ref().type_id()
                )
            });
        Error::Panicked(detail)
    })?
}
static SHARED: Shared = Shared::new();

struct Owner<'a> {
    shared: &'a Shared,
    // Construct/drop on the same background or headless owner. This guard must
    // never migrate into a GUI-owned renderer or app field.
    _same_thread: PhantomData<Rc<()>>,
}
impl Drop for Owner<'_> {
    fn drop(&mut self) {
        self.shared.release_owner();
    }
}
/// Retains the process-shared cache across frames of one actual background or
/// headless service. Acquisition is lazy: it never initializes the model/pool.
///
/// Non-Send/non-Sync by design. Create inside the worker closure, not before
/// spawning it. Final drop may synchronously destroy/join the RTen pool on this
/// owner thread; it must never occur in a GUI frame or audio callback.
///
/// ```compile_fail
/// let owner = skylive_person_matte::retain_background_owner().unwrap();
/// std::thread::spawn(move || drop(owner));
/// ```
pub struct BackgroundOwner {
    _owner: Owner<'static>,
}
pub fn retain_background_owner() -> Result<BackgroundOwner, Error> {
    SHARED
        .owner()
        .map(|owner| BackgroundOwner { _owner: owner })
}

/// Nonblocking admission; this still executes one synchronous bounded graph.
/// Use from a background owner, never from a GUI frame or audio callback.
pub fn evaluate(input: &[f32]) -> Result<Inference, Error> {
    evaluate_in(&SHARED, input, false)
}
/// FIFO background wait, at most4admitted callers including active; fifth Busy.
/// Without a persistent BackgroundOwner, final completion retires the cache;
/// repeated-frame hosts should retain one guard for the service lifetime.
/// No kernel cancellation/preemption. Caller retains and charges its input.
pub fn evaluate_wait(input: &[f32]) -> Result<Inference, Error> {
    evaluate_in(&SHARED, input, true)
}
fn evaluate_in(shared: &Shared, input: &[f32], wait: bool) -> Result<Inference, Error> {
    let began = Instant::now();
    if input.len() != INPUT_ELEMENTS {
        return Err(Error::InputLength {
            actual: input.len(),
        });
    }
    let _lease = shared.enter(wait)?;
    for (index, &value) in input.iter().enumerate() {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(Error::InputValue { index, value });
        }
    }
    let mut cache = shared.cache.try_lock().map_err(lock_error)?;
    let initialized = matches!(*cache, Cache::Empty);
    let mut model_load = Duration::ZERO;
    if initialized {
        let started = Instant::now();
        match guarded(Runtime::load) {
            Ok(runtime) => *cache = Cache::Ready(runtime),
            Err(error) => {
                *cache = Cache::Failed(error.clone());
                return Err(error);
            }
        }
        model_load = started.elapsed();
    }
    let runtime = match &*cache {
        Cache::Ready(runtime) => runtime,
        Cache::Failed(error) => return Err(error.clone()),
        Cache::Empty => {
            return Err(Error::ModelLoad(
                "person matte cache did not initialize".into(),
            ));
        }
    };
    let started = Instant::now();
    let result = guarded(|| runtime.run(input));
    let inference = started.elapsed();
    match result {
        Ok(probabilities) => Ok(Inference {
            probabilities,
            timing: Timing {
                initialized,
                model_load,
                inference,
                total: began.elapsed(),
            },
        }),
        Err(error) => {
            if matches!(error, Error::Panicked(_)) {
                *cache = Cache::Failed(error.clone());
            }
            Err(error)
        }
    }
}
/// Explicit background/headless cleanup when no persistent owners remain.
/// Nonblocking admission refuses any retained owner or queued/active work. The
/// admission slot remains held through actual model/pool destruction. Native
/// hosts normally retire automatically when their final owner settles.
pub fn shutdown() -> Result<(), Error> {
    shutdown_in(&SHARED)
}
fn shutdown_in(shared: &Shared) -> Result<(), Error> {
    let _lease = shared.enter_for(false, true)?;
    shared.retire_cache()
}

#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod tests;
