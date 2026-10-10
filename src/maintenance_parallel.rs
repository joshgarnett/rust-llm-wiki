//! Command-owned, joined maintenance work. Workers never authorize publication.
use crate::domain::{ErrorCode, Result, WikiError};
use std::{
    cell::RefCell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Instant,
};

pub(crate) const MAX_JOBS: usize = 16;
pub(crate) const MAX_IN_FLIGHT_BYTES: u64 = 1024 * 1024 * 1024;
type Task = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct Metrics {
    pub owner_activation_validations: u64,
    pub owner_activation_reads: u64,
    pub owner_activation_read_bytes: u64,
    pub submitted: u64,
    pub completed: u64,
    pub failed: u64,
    pub panicked: u64,
    pub max_active: usize,
    pub max_in_flight_bytes: u64,
    pub worker_service_ns: u64,
    pub joined_batch_wall_ns: u64,
}
#[derive(Default)]
struct Counters {
    owner_activation_validations: AtomicU64,
    owner_activation_reads: AtomicU64,
    owner_activation_read_bytes: AtomicU64,
    submitted: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    panicked: AtomicU64,
    active: AtomicUsize,
    max_active: AtomicUsize,
    reserved: AtomicU64,
    max_reserved: AtomicU64,
    service_ns: AtomicU64,
    wall_ns: AtomicU64,
}
struct Pool {
    sender: Mutex<Option<mpsc::SyncSender<Task>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    counters: Arc<Counters>,
    sequential: bool,
}
thread_local! {
    static CURRENT: RefCell<Option<Arc<Pool>>> = const { RefCell::new(None) };
    static IN_JOB: RefCell<bool> = const { RefCell::new(false) };
}
fn error(message: &str) -> WikiError {
    WikiError::new(ErrorCode::Internal, message)
}
fn nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
impl Pool {
    fn new(sequential: bool) -> Result<Arc<Self>> {
        let workers = Vec::with_capacity(MAX_JOBS);
        let counters = Arc::new(Counters::default());
        let shared = (std::mem::size_of::<Self>()
            + std::mem::size_of::<Counters>()
            + 4 * std::mem::size_of::<usize>()
            + workers.capacity() * std::mem::size_of::<JoinHandle<()>>())
            as u64;
        counters.reserved.store(shared, Ordering::Relaxed);
        counters.max_reserved.store(shared, Ordering::Relaxed);
        Ok(Arc::new(Self {
            sender: Mutex::new(None),
            workers: Mutex::new(workers),
            counters,
            sequential,
        }))
    }
    fn start(&self) -> Result<()> {
        if self
            .sender
            .lock()
            .expect("maintenance sender poisoned")
            .is_none()
        {
            let (sender, receiver) = mpsc::sync_channel::<Task>(MAX_JOBS);
            *self.sender.lock().expect("maintenance sender poisoned") = Some(sender);
            let receiver = Arc::new(Mutex::new(receiver));
            for index in 0..MAX_JOBS {
                let receiver = receiver.clone();
                let worker = thread::Builder::new()
                    .name(format!("lwiki-maintenance-{index}"))
                    .spawn(move || {
                        loop {
                            let task = receiver.lock().expect("maintenance queue poisoned").recv();
                            let Ok(task) = task else { break };
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
                        }
                    });
                match worker {
                    Ok(worker) => self
                        .workers
                        .lock()
                        .expect("maintenance workers poisoned")
                        .push(worker),
                    Err(_) => {
                        self.close();
                        return Err(error("could not admit fixed maintenance worker pool"));
                    }
                }
            }
        }
        Ok(())
    }
    fn close(&self) {
        self.sender
            .lock()
            .expect("maintenance sender poisoned")
            .take();
        for worker in self
            .workers
            .lock()
            .expect("maintenance workers poisoned")
            .drain(..)
        {
            // Every submitted task catches its own panic and returns its observer delta.
            // Closing occurs only after the owner has joined every admitted batch.
            let _ = worker.join();
        }
    }
}
struct CommandGuard(Arc<Pool>);
impl Drop for CommandGuard {
    fn drop(&mut self) {
        CURRENT.with(|slot| slot.borrow_mut().take());
        self.0.close();
        crate::storage::maintenance_activation::clear_owner();
    }
}
/// One pool for the whole public command; nested scopes reuse it, never spawn another.
pub(crate) fn command_scope<T>(run: impl FnOnce() -> Result<T>) -> Result<T> {
    scope(!cfg!(unix), run)
}
/// Deterministic functional reference; this is not a worker-count tuning control.
#[cfg(test)]
pub(crate) fn sequential_scope<T>(run: impl FnOnce() -> Result<T>) -> Result<T> {
    scope(true, run)
}
fn scope<T>(sequential: bool, run: impl FnOnce() -> Result<T>) -> Result<T> {
    if IN_JOB.with(|slot| *slot.borrow()) {
        return Err(error(
            "maintenance command scopes are forbidden inside worker jobs",
        ));
    }
    if CURRENT.with(|slot| slot.borrow().is_some()) {
        return run();
    }
    let pool = Pool::new(sequential)?;
    CURRENT.with(|slot| *slot.borrow_mut() = Some(pool.clone()));
    let _guard = CommandGuard(pool);
    run()
}
pub(crate) fn metrics() -> Metrics {
    CURRENT.with(|slot| {
        let slot = slot.borrow();
        let Some(pool) = slot.as_ref() else {
            return Metrics::default();
        };
        let c = &pool.counters;
        Metrics {
            owner_activation_validations: c.owner_activation_validations.load(Ordering::Relaxed),
            owner_activation_reads: c.owner_activation_reads.load(Ordering::Relaxed),
            owner_activation_read_bytes: c.owner_activation_read_bytes.load(Ordering::Relaxed),
            submitted: c.submitted.load(Ordering::Relaxed),
            completed: c.completed.load(Ordering::Relaxed),
            failed: c.failed.load(Ordering::Relaxed),
            panicked: c.panicked.load(Ordering::Relaxed),
            max_active: c.max_active.load(Ordering::Relaxed),
            max_in_flight_bytes: c.max_reserved.load(Ordering::Relaxed),
            worker_service_ns: c.service_ns.load(Ordering::Relaxed),
            joined_batch_wall_ns: c.wall_ns.load(Ordering::Relaxed),
        }
    })
}
pub(crate) fn record_owner_activation_validation() {
    CURRENT.with(|slot| {
        if let Some(pool) = slot.borrow().as_ref() {
            pool.counters
                .owner_activation_validations
                .fetch_add(1, Ordering::Relaxed);
        }
    });
}
pub(crate) fn record_owner_activation_read() {
    CURRENT.with(|slot| {
        if let Some(pool) = slot.borrow().as_ref() {
            pool.counters
                .owner_activation_reads
                .fetch_add(1, Ordering::Relaxed);
        }
    });
}
pub(crate) fn record_owner_activation_bytes(bytes: u64) {
    CURRENT.with(|slot| {
        if let Some(pool) = slot.borrow().as_ref() {
            pool.counters
                .owner_activation_read_bytes
                .fetch_add(bytes, Ordering::Relaxed);
        }
    });
}
pub(crate) fn parallel_enabled() -> bool {
    CURRENT.with(|slot| slot.borrow().as_ref().is_some_and(|pool| !pool.sequential))
}
pub(crate) fn available_bytes() -> u64 {
    CURRENT.with(|slot| {
        slot.borrow().as_ref().map_or(MAX_IN_FLIGHT_BYTES, |pool| {
            MAX_IN_FLIGHT_BYTES.saturating_sub(pool.counters.reserved.load(Ordering::Relaxed))
        })
    })
}
/// Shared command capabilities remain charged until command teardown.
pub(crate) fn reserve_shared(bytes: u64) -> Result<()> {
    CURRENT.with(|slot| {
        let slot = slot.borrow();
        let Some(pool) = slot.as_ref() else {
            return Ok(());
        };
        let previous = pool
            .counters
            .reserved
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                held.checked_add(bytes)
                    .filter(|total| *total <= MAX_IN_FLIGHT_BYTES)
            })
            .map_err(|_| {
                WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "maintenance shared capability allowance exhausted",
                )
            })?;
        pool.counters
            .max_reserved
            .fetch_max(previous + bytes, Ordering::Relaxed);
        Ok(())
    })
}
pub(crate) struct Job<T> {
    reservation: u64,
    run: Box<dyn FnOnce() -> Result<T> + Send + 'static>,
}
impl<T> Job<T> {
    /// Include buffers, owned descriptors and retained result bytes in this ceiling.
    pub(crate) fn new(reservation: u64, run: impl FnOnce() -> Result<T> + Send + 'static) -> Self {
        Self {
            reservation: reservation.saturating_add(std::mem::size_of_val(&run) as u64),
            run: Box::new(run),
        }
    }
}
type Completion<T> = (
    thread::Result<Result<T>>,
    crate::maintenance_observers::WorkerDelta,
);
/// Visible per-job task/channel/context storage, excluding bounded std internals.
/// Caller uses this when choosing a chunk; run_batch charges it independently.
pub(crate) fn job_overhead<T>() -> u64 {
    let context = crate::maintenance_observers::WorkerContext::capture();
    context.reservation_bytes()
        + (std::mem::size_of::<crate::maintenance_observers::WorkerContext>()
            + std::mem::size_of::<mpsc::SyncSender<Completion<T>>>()
            + std::mem::size_of::<Arc<Counters>>()
            + std::mem::size_of::<Completion<T>>()
            + std::mem::size_of::<Box<dyn FnOnce() + Send>>()) as u64
}
struct Reservation {
    counters: Arc<Counters>,
    bytes: u64,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.counters
            .reserved
            .fetch_sub(self.bytes, Ordering::Relaxed);
    }
}
pub(crate) struct Batch<T> {
    results: Vec<Result<T>>,
    reservation: Option<Reservation>,
}
pub(crate) struct BatchResults<T> {
    results: std::vec::IntoIter<Result<T>>,
    _reservation: Option<Reservation>,
}
impl<T> Iterator for BatchResults<T> {
    type Item = Result<T>;
    fn next(&mut self) -> Option<Self::Item> {
        self.results.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.results.size_hint()
    }
}
impl<T> IntoIterator for Batch<T> {
    type Item = Result<T>;
    type IntoIter = BatchResults<T>;
    fn into_iter(self) -> Self::IntoIter {
        BatchResults {
            results: self.results.into_iter(),
            _reservation: self.reservation,
        }
    }
}
/// Admit a bounded batch before execution, join all work, merge all observations,
/// then return results in descriptor order. No later batch is implicitly submitted.
pub(crate) fn run_batch<T: Send + 'static>(jobs: Vec<Job<T>>) -> Result<Batch<T>> {
    run_batch_with_mode(jobs, false)
}
/// Owner-bound semantic memo scopes may keep an affected phase sequential.
pub(crate) fn run_batch_sequential<T: Send + 'static>(jobs: Vec<Job<T>>) -> Result<Batch<T>> {
    run_batch_with_mode(jobs, true)
}
fn run_batch_with_mode<T: Send + 'static>(
    jobs: Vec<Job<T>>,
    force_sequential: bool,
) -> Result<Batch<T>> {
    if IN_JOB.with(|slot| *slot.borrow()) {
        return Err(error("nested maintenance jobs are forbidden"));
    }
    if jobs.len() > MAX_JOBS {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "maintenance descriptor batch exceeds allowance",
        ));
    }
    let bytes = jobs
        .iter()
        .try_fold(0u64, |sum, job| sum.checked_add(job.reservation))
        .filter(|bytes| *bytes <= MAX_IN_FLIGHT_BYTES)
        .ok_or_else(|| {
            WikiError::new(
                ErrorCode::BudgetExceeded,
                "maintenance in-flight byte allowance exhausted",
            )
        })?;
    let mut receivers = Vec::<mpsc::Receiver<Completion<T>>>::with_capacity(jobs.len());
    let mut results = Vec::<Result<T>>::with_capacity(jobs.len());
    let overhead = (jobs.capacity() * std::mem::size_of::<Job<T>>()
        + receivers.capacity() * std::mem::size_of::<mpsc::Receiver<Completion<T>>>()
        + results.capacity() * std::mem::size_of::<Result<T>>()) as u64
        + jobs.len() as u64 * job_overhead::<T>();
    let bytes = bytes
        .checked_add(overhead)
        .filter(|bytes| *bytes <= MAX_IN_FLIGHT_BYTES)
        .ok_or_else(|| {
            WikiError::new(
                ErrorCode::BudgetExceeded,
                "maintenance batch bookkeeping allowance exhausted",
            )
        })?;
    let pool = CURRENT.with(|slot| slot.borrow().clone());
    let reservation = if let Some(pool) = &pool {
        let previous = pool
            .counters
            .reserved
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                held.checked_add(bytes)
                    .filter(|total| *total <= MAX_IN_FLIGHT_BYTES)
            })
            .map_err(|_| {
                WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "maintenance retained batch allowance exhausted",
                )
            })?;
        pool.counters
            .max_reserved
            .fetch_max(previous + bytes, Ordering::Relaxed);
        Some(Reservation {
            counters: pool.counters.clone(),
            bytes,
        })
    } else {
        None
    };
    let started = Instant::now();
    if force_sequential || pool.as_ref().is_none_or(|pool| pool.sequential) {
        // The invoking thread already owns its observers. Preserve them directly;
        // installing a worker observer here would create a nested observation.
        for job in jobs {
            let service = Instant::now();
            IN_JOB.with(|slot| *slot.borrow_mut() = true);
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job.run));
            IN_JOB.with(|slot| *slot.borrow_mut() = false);
            let result = match outcome {
                Ok(result) => result,
                Err(_) => {
                    if let Some(pool) = &pool {
                        pool.counters.panicked.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(error("maintenance worker panicked"))
                }
            };
            if let Some(pool) = &pool {
                pool.counters.submitted.fetch_add(1, Ordering::Relaxed);
                pool.counters.completed.fetch_add(1, Ordering::Relaxed);
                pool.counters.max_active.fetch_max(1, Ordering::Relaxed);
                pool.counters
                    .service_ns
                    .fetch_add(nanos(service), Ordering::Relaxed);
                if result.is_err() {
                    pool.counters.failed.fetch_add(1, Ordering::Relaxed);
                }
            }
            results.push(result);
        }
        if let Some(pool) = &pool {
            pool.counters
                .wall_ns
                .fetch_add(nanos(started), Ordering::Relaxed);
        }
        return Ok(Batch {
            results,
            reservation,
        });
    }
    let pool_ref = pool.as_ref().expect("parallel execution has an owner pool");
    if !jobs.is_empty() {
        pool_ref.start()?;
    }
    let context = crate::maintenance_observers::WorkerContext::capture();
    for job in jobs {
        let (sender, receiver) = mpsc::sync_channel(1);
        let context = context.clone();
        let counters = pool_ref.counters.clone();
        let task: Task = Box::new(move || {
            struct JobGuard;
            impl Drop for JobGuard {
                fn drop(&mut self) {
                    IN_JOB.with(|slot| *slot.borrow_mut() = false);
                }
            }
            struct Timing {
                counters: Arc<Counters>,
                started: Instant,
            }
            impl Drop for Timing {
                fn drop(&mut self) {
                    self.counters
                        .service_ns
                        .fetch_add(nanos(self.started), Ordering::Relaxed);
                    self.counters.active.fetch_sub(1, Ordering::Relaxed);
                }
            }
            let active = counters.active.fetch_add(1, Ordering::Relaxed) + 1;
            counters.max_active.fetch_max(active, Ordering::Relaxed);
            let timing = Timing {
                counters,
                started: Instant::now(),
            };
            let result = {
                IN_JOB.with(|slot| *slot.borrow_mut() = true);
                let _guard = JobGuard;
                context.run(job.run)
            };
            // Publish completion only after service/active accounting and all
            // worker context teardown. The owner snapshot cannot race a postlude.
            drop(timing);
            let _ = sender.send(result);
        });
        if let Some(pool) = &pool {
            pool.counters.submitted.fetch_add(1, Ordering::Relaxed);
            pool.sender
                .lock()
                .expect("maintenance sender poisoned")
                .as_ref()
                .expect("active maintenance pool")
                .send(task)
                .expect("maintenance worker stopped before join");
        } else {
            task();
        }
        receivers.push(receiver);
    }
    for receiver in receivers {
        let result = match receiver.recv() {
            Ok((outcome, delta)) => {
                let diagnostics = delta.merge();
                if let Err(error) = diagnostics {
                    if outcome.is_err() {
                        if let Some(pool) = &pool {
                            pool.counters.panicked.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    results.push(Err(error));
                    if let Some(pool) = &pool {
                        pool.counters.completed.fetch_add(1, Ordering::Relaxed);
                        pool.counters.failed.fetch_add(1, Ordering::Relaxed);
                    }
                    continue;
                }
                match outcome {
                    Ok(result) => result,
                    Err(_) => {
                        if let Some(pool) = &pool {
                            pool.counters.panicked.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error("maintenance worker panicked"))
                    }
                }
            }
            Err(_) => Err(error("maintenance worker did not return a joined result")),
        };
        if let Some(pool) = &pool {
            pool.counters.completed.fetch_add(1, Ordering::Relaxed);
            if result.is_err() {
                pool.counters.failed.fetch_add(1, Ordering::Relaxed);
            }
        }
        results.push(result);
    }
    if let Some(pool) = &pool {
        pool.counters
            .wall_ns
            .fetch_add(nanos(started), Ordering::Relaxed);
    }
    Ok(Batch {
        results,
        reservation,
    })
}

#[cfg(test)]
#[path = "maintenance_parallel_tests.rs"]
mod tests;
