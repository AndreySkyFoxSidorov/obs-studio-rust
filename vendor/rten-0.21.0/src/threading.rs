use std::env;
use std::sync::OnceLock;
use std::thread::{self, JoinHandle};

/// SkyLive owned variant: keep handles until actual exit, including a partially
/// created pool which must retire before falling back to serial execution.
#[derive(Default)]
struct OwnedThreads {
    handles: Vec<JoinHandle<()>>,
}
impl Drop for OwnedThreads {
    fn drop(&mut self) {
        let current = thread::current().id();
        if self
            .handles
            .iter()
            .any(|handle| handle.thread().id() == current)
        {
            // The owned API requires external destruction. Do not self-join or
            // wait on dependent workers if a different consumer violates it.
            // SkyLive's runtime retires only after synchronous run() returns on
            // the external background/headless owner, never a pool worker.
            return;
        }
        let mut failure = None;
        for handle in self.handles.drain(..) {
            if let Err(payload) = handle.join() {
                if failure.is_none() {
                    failure = Some(payload);
                }
            }
        }
        // All workers are joined before propagating the first original panic.
        // During an existing unwind, preserve that original failure instead of
        // causing a second panic/abort from cleanup.
        if !thread::panicking() {
            if let Some(payload) = failure {
                std::panic::resume_unwind(payload);
            }
        }
    }
}

/// A wrapper around the Rayon thread pool used to run models.
///
/// On platforms where threads are not supported (eg. WebAssembly) this runs
/// operations directly on the main thread.
pub struct ThreadPool {
    /// The wrapped thread pool, or None if we failed to construct one.
    pool: Option<rayon::ThreadPool>,
    _owned_threads: OwnedThreads,
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        // Rayon Drop only requests termination. Signal it before owned handle
        // destruction joins actual worker exits. Empty for upstream constructor.
        drop(self.pool.take());
    }
}

impl ThreadPool {
    /// Run a function in the thread pool.
    ///
    /// This corresponds to [`rayon::ThreadPool::install`], except on platforms
    /// where threading is not supported, where it just runs `op` directly.
    pub fn run<R: Send, Op: FnOnce() -> R + Send>(&self, op: Op) -> R {
        if let Some(pool) = self.pool.as_ref() {
            pool.install(op)
        } else {
            op()
        }
    }

    /// Create a thread pool with a given number of threads.
    pub fn with_num_threads(num_threads: usize) -> ThreadPool {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .thread_name(|index| format!("rten-{}", index))
            .build();

        ThreadPool {
            pool: pool.ok(),
            _owned_threads: OwnedThreads::default(),
        }
    }

    /// SkyLive extension: pool with joinable owned workers. Drop on an external
    /// background/headless owner, after run() settles, terminates and joins them.
    /// Never drop from one of this pool's own workers. For self-join safety that
    /// unsupported case detaches instead and does not guarantee completed exit.
    /// The upstream constructor retains its ordinary detached Drop semantics.
    pub fn with_num_threads_owned(num_threads: usize) -> ThreadPool {
        Self::with_num_threads_owned_using(num_threads, |thread| {
            let mut builder = thread::Builder::new();
            if let Some(name) = thread.name() {
                builder = builder.name(name.to_owned());
            }
            if let Some(size) = thread.stack_size() {
                builder = builder.stack_size(size);
            }
            builder.spawn(move || thread.run())
        })
    }

    fn with_num_threads_owned_using(
        num_threads: usize,
        mut spawn: impl FnMut(rayon::ThreadBuilder) -> std::io::Result<JoinHandle<()>>,
    ) -> ThreadPool {
        let mut workers = OwnedThreads::default();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .thread_name(|index| format!("rten-{}", index))
            .spawn_handler(|thread| {
                workers.handles.push(spawn(thread)?);
                Ok(())
            })
            .build();
        match pool {
            Ok(pool) => ThreadPool {
                pool: Some(pool),
                _owned_threads: workers,
            },
            Err(_) => {
                // Rayon's build failure terminates its partial registry first.
                // Join all successfully spawned workers before serial fallback.
                drop(workers);
                ThreadPool {
                    pool: None,
                    _owned_threads: OwnedThreads::default(),
                }
            }
        }
    }
}

/// Return the optimal number of cores to use for maximum performance.
///
/// This may be less than the total number of cores on systems with heterogenous
/// cores (eg. a mix of performance and efficiency).
fn optimal_core_count() -> u32 {
    #[allow(unused_mut)]
    let mut core_count = num_cpus::get_physical().max(1) as u32;

    #[cfg(target_os = "macos")]
    {
        use rten_simd::isa_detection::macos::sysctl_int;
        if let Ok(perf_core_count) = sysctl_int(c"hw.perflevel0.physicalcpu") {
            core_count = core_count.clamp(1, perf_core_count as u32);
        }
    }

    core_count
}

/// Return the [Rayon][rayon] thread pool which is used to execute RTen models.
///
/// This differs from Rayon's default global thread pool in that it is tuned for
/// CPU rather than IO-bound work by choosing a thread count based on the number
/// of physical rather than logical cores.
///
/// The thread count can be overridden at the process level by setting the
/// `RTEN_NUM_THREADS` environment variable, whose value must be a number
/// between 1 and the logical core count.
///
/// The thread count can be overridden for each model run by configuring a
/// custom thread pool in [`RunOptions`](crate::RunOptions).
///
/// To run your own tasks in this thread pool, you can use
/// [`ThreadPool::run`].
///
/// [rayon]: https://github.com/rayon-rs/rayon
pub fn thread_pool() -> &'static ThreadPool {
    static THREAD_POOL: OnceLock<ThreadPool> = OnceLock::new();
    THREAD_POOL.get_or_init(|| {
        let physical_cpus = optimal_core_count();

        let num_threads = if let Some(threads_var) = env::var_os("RTEN_NUM_THREADS") {
            let requested_threads: Result<u32, _> = threads_var.to_string_lossy().parse();
            match requested_threads {
                Ok(n_threads) => n_threads.clamp(1, num_cpus::get() as u32),
                Err(_) => physical_cpus,
            }
        } else {
            physical_cpus
        };

        ThreadPool::with_num_threads(num_threads as usize)
    })
}

#[cfg(test)]
mod tests {
    use super::optimal_core_count;

    #[test]
    fn test_optimal_core_count() {
        let max_cores = num_cpus::get_physical() as u32;
        let opt_cores = optimal_core_count();
        assert!(opt_cores >= 1 && opt_cores <= max_cores);
    }

    #[test]
    fn owned_pool_drop_joins_all_actual_workers() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let exited = Arc::new(AtomicUsize::new(0));
        let pool = super::ThreadPool::with_num_threads_owned_using(2, |worker| {
            let exited = exited.clone();
            Ok(std::thread::spawn(move || {
                worker.run();
                exited.fetch_add(1, Ordering::SeqCst);
            }))
        });
        assert_eq!(pool._owned_threads.handles.len(), 2);
        assert_eq!(pool.run(|| 41 + 1), 42);
        drop(pool);
        assert_eq!(exited.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn partial_owned_build_failure_joins_before_serial_fallback() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let exited = Arc::new(AtomicUsize::new(0));
        let mut started = 0;
        let pool = super::ThreadPool::with_num_threads_owned_using(2, |worker| {
            started += 1;
            if started == 2 {
                return Err(std::io::Error::other("injected second spawn failure"));
            }
            let exited = exited.clone();
            Ok(std::thread::spawn(move || {
                worker.run();
                exited.fetch_add(1, Ordering::SeqCst);
            }))
        });
        assert!(pool.pool.is_none());
        assert!(pool._owned_threads.handles.is_empty());
        assert_eq!(exited.load(Ordering::SeqCst), 1);
        assert_eq!(pool.run(|| 42), 42);
    }

    #[test]
    fn partial_owned_build_panic_joins_before_original_unwind_returns() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let exited = Arc::new(AtomicUsize::new(0));
        let mut started = 0;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::ThreadPool::with_num_threads_owned_using(2, |worker| {
                started += 1;
                if started == 2 {
                    panic!("injected second spawn panic");
                }
                let exited = exited.clone();
                Ok(std::thread::spawn(move || {
                    worker.run();
                    exited.fetch_add(1, Ordering::SeqCst);
                }))
            })
        }));
        assert_eq!(
            result.err().unwrap().downcast_ref::<&str>(),
            Some(&"injected second spawn panic")
        );
        assert_eq!(exited.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unexpected_owned_worker_drop_never_joins_itself() {
        let (input, work) = std::sync::mpsc::sync_channel::<super::OwnedThreads>(1);
        let (reply, done) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            drop(work.recv().unwrap());
            reply.send(()).unwrap();
        });
        assert!(
            input
                .send(super::OwnedThreads {
                    handles: vec![worker]
                })
                .is_ok()
        );
        done.recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
    }
}
