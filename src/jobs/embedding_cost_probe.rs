//! Test-only attribution for the frozen embedding Run-width diagnostic.
//! Thread-local scalars never hold an instrumentation lock across product work.
#![cfg(test)]

use crate::domain::{Blake3Hash, ErrorCode, RecordId, Result, WikiError};
use serde::Serialize;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    marker::PhantomData,
    rc::Rc,
    time::{Duration, Instant},
};

const MAX_TASKS: usize = 32;
const MAX_ELAPSED: Duration = Duration::from_secs(120);
// Reserve the remaining 2 MiB of the arm's 8 MiB budget for final audit/report.
const MAX_TRACE_BYTES: usize = 6 * 1024 * 1024;
const MAX_STACK: usize = 64;
const TRACE_PREFIX: &str = "embedding-cost-probe ";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Arm {
    Exact,
    GuardOccurrences127,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stage {
    LockWait,
    CurrentHistory,
    EarlierRunHistory,
    PendingBindings,
    HistoricalReceipts,
    OwnerProof,
    PriorDiscovery,
    PrefixAdmission,
    JobCreation,
    Other,
}
impl Stage {
    const ALL: [Self; 10] = [
        Self::LockWait,
        Self::CurrentHistory,
        Self::EarlierRunHistory,
        Self::PendingBindings,
        Self::HistoricalReceipts,
        Self::OwnerProof,
        Self::PriorDiscovery,
        Self::PrefixAdmission,
        Self::JobCreation,
        Self::Other,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Counter {
    JournalBytesLoaded,
    JournalFramesLoaded,
    JournalFramesReplayed,
    HistoryLoads,
    EarlierJournalBytesLoaded,
    EarlierJournalFramesLoaded,
    EarlierJournalFramesReplayed,
    EarlierHistoryLoads,
    PendingTaskBindVisits,
    PendingGuardOccurrences,
    PendingDescriptorLogicalBytes,
    HistoricalReceiptVisits,
    CanonicalBindingVisits,
    LockAcquisitions,
    InvocationRunLoads,
    EarlierInvocationRunLoads,
    PriorRuns,
    PriorDiscoveries,
    OwnerCount,
    OwnerUnitCount,
    OwnerRenderBytes,
    PrefixCandidates,
    JobsCreated,
}
impl Counter {
    const ALL: [Self; 23] = [
        Self::JournalBytesLoaded,
        Self::JournalFramesLoaded,
        Self::JournalFramesReplayed,
        Self::HistoryLoads,
        Self::EarlierJournalBytesLoaded,
        Self::EarlierJournalFramesLoaded,
        Self::EarlierJournalFramesReplayed,
        Self::EarlierHistoryLoads,
        Self::PendingTaskBindVisits,
        Self::PendingGuardOccurrences,
        Self::PendingDescriptorLogicalBytes,
        Self::HistoricalReceiptVisits,
        Self::CanonicalBindingVisits,
        Self::LockAcquisitions,
        Self::InvocationRunLoads,
        Self::EarlierInvocationRunLoads,
        Self::PriorRuns,
        Self::PriorDiscoveries,
        Self::OwnerCount,
        Self::OwnerUnitCount,
        Self::OwnerRenderBytes,
        Self::PrefixCandidates,
        Self::JobsCreated,
    ];
    fn earlier(self) -> Option<Self> {
        match self {
            Self::JournalBytesLoaded => Some(Self::EarlierJournalBytesLoaded),
            Self::JournalFramesLoaded => Some(Self::EarlierJournalFramesLoaded),
            Self::JournalFramesReplayed => Some(Self::EarlierJournalFramesReplayed),
            Self::HistoryLoads => Some(Self::EarlierHistoryLoads),
            Self::InvocationRunLoads => Some(Self::EarlierInvocationRunLoads),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TaskCompletion {
    pub task_key: String,
    pub run_id: String,
    pub generated_inputs: usize,
    pub elapsed_ns: u64,
    pub stage_ns: BTreeMap<Stage, u64>,
    pub counters: BTreeMap<Counter, u64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct JobAdmission {
    pub task_count: usize,
    pub guard_occurrences: usize,
    pub encoded_spec_bytes: usize,
    pub elapsed_ns: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Report {
    pub arm: Arm,
    pub total_elapsed_ns: u64,
    pub stage_ns: BTreeMap<Stage, u64>,
    pub counters: BTreeMap<Counter, u64>,
    pub completed_tasks: Vec<TaskCompletion>,
    pub admitted_jobs: Vec<JobAdmission>,
    pub diagnostic_stop: Option<&'static str>,
    pub prefix_complete: bool,
    pub censored: bool,
    pub trace_records: u64,
    pub trace_bytes: usize,
    pub trace_truncated: bool,
    pub instrumentation_overflow: bool,
    pub open_stages: usize,
    pub physical_io_bytes: Option<u64>,
    pub peak_allocation_bytes: Option<u64>,
}

struct Frame {
    id: u64,
    stage: Stage,
}
struct Probe {
    session: u64,
    arm: Arm,
    started: Instant,
    last: Instant,
    stages: [u64; 10],
    counters: [u64; 23],
    stack: Vec<Frame>,
    next_guard: u64,
    earlier_projection: bool,
    completed: Vec<TaskCompletion>,
    jobs: Vec<JobAdmission>,
    stop: Option<&'static str>,
    trace_records: u64,
    trace_bytes: usize,
    truncated: bool,
    overflow: bool,
}

thread_local! {
    static ACTIVE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    static NEXT_SESSION: Cell<u64> = const { Cell::new(1) };
}

impl Probe {
    fn nanos(&mut self, duration: Duration) -> u64 {
        match u64::try_from(duration.as_nanos()) {
            Ok(n) => n,
            Err(_) => {
                self.overflow = true;
                u64::MAX
            }
        }
    }
    fn add_stage(&mut self, stage: Stage, n: u64) {
        let value = &mut self.stages[stage as usize];
        match value.checked_add(n) {
            Some(next) => *value = next,
            None => {
                *value = u64::MAX;
                self.overflow = true;
            }
        }
    }
    fn settle(&mut self) {
        let now = Instant::now();
        let n = self.nanos(now.duration_since(self.last));
        let stage = self.stack.last().map_or(Stage::Other, |f| f.stage);
        self.add_stage(stage, n);
        self.last = now;
    }
    fn stage_map(&self) -> BTreeMap<Stage, u64> {
        Stage::ALL
            .into_iter()
            .map(|s| (s, self.stages[s as usize]))
            .collect()
    }
    fn counter_map(&self) -> BTreeMap<Counter, u64> {
        Counter::ALL
            .into_iter()
            .map(|c| (c, self.counters[c as usize]))
            .collect()
    }
    fn add_counter(&mut self, counter: Counter, n: usize) {
        let n = u64::try_from(n).unwrap_or_else(|_| {
            self.overflow = true;
            u64::MAX
        });
        let value = &mut self.counters[counter as usize];
        match value.checked_add(n) {
            Some(next) => *value = next,
            None => {
                *value = u64::MAX;
                self.overflow = true;
            }
        }
    }
    // Streaming records contain no source payloads. Record formatting/write time
    // belongs to Other, not the stage under investigation.
    fn record(
        &mut self,
        kind: &str,
        stage: Option<Stage>,
        id: Option<u64>,
        task: Option<&TaskCompletion>,
    ) {
        self.settle();
        let elapsed_ns = self.nanos(self.started.elapsed());
        let job = if kind == "job_admitted" {
            self.jobs.last()
        } else {
            None
        };
        let event = serde_json::json!({"kind":kind,"arm":self.arm,
            "elapsed_ns":elapsed_ns,"stage":stage,"guard_id":id,"task":task,"job":job});
        match serde_json::to_string(&event) {
            Ok(bytes) => {
                let length = bytes.len().checked_add(TRACE_PREFIX.len() + 1);
                let next = length.and_then(|n| self.trace_bytes.checked_add(n));
                if next.is_some_and(|n| n <= MAX_TRACE_BYTES) {
                    eprintln!("{TRACE_PREFIX}{bytes}");
                    self.trace_bytes = next.unwrap();
                    self.trace_records = self.trace_records.saturating_add(1);
                } else {
                    self.truncated = true;
                }
            }
            Err(_) => self.overflow = true,
        }
        let now = Instant::now();
        let overhead = self.nanos(now.duration_since(self.last));
        self.add_stage(Stage::Other, overhead);
        self.last = now;
    }
    fn stop_error(&mut self, reason: &'static str) -> WikiError {
        self.stop.get_or_insert(reason);
        let elapsed_ns = self.nanos(self.started.elapsed());
        let mut error = WikiError::new(
            ErrorCode::BudgetExceeded,
            "embedding cost diagnostic stopped",
        );
        error.details = serde_json::json!({"diagnostic_stop":true,"reason":reason,
            "arm":self.arm,"completed_tasks":self.completed.len(),"elapsed_ns":elapsed_ns});
        error
    }
    fn check(&mut self) -> Result<()> {
        if let Some(reason) = self.stop {
            return Err(self.stop_error(reason));
        }
        if self.overflow {
            return Err(self.stop_error("instrumentation_overflow"));
        }
        if self.truncated {
            return Err(self.stop_error("trace_limit"));
        }
        if self.completed.len() >= MAX_TASKS {
            return Err(self.stop_error("task_limit"));
        }
        if self.started.elapsed() >= MAX_ELAPSED {
            return Err(self.stop_error("time_limit"));
        }
        Ok(())
    }
}

/// Guards are owner-thread affine and cannot cross an async/thread handoff.
pub(crate) struct StageGuard {
    session: u64,
    id: Option<u64>,
    _thread: PhantomData<Rc<()>>,
}
pub(crate) fn enter(stage: Stage) -> StageGuard {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(probe) = active.as_mut() else {
            return StageGuard {
                session: 0,
                id: None,
                _thread: PhantomData,
            };
        };
        if probe.stack.len() >= MAX_STACK || probe.next_guard == u64::MAX {
            probe.overflow = true;
            return StageGuard {
                session: probe.session,
                id: None,
                _thread: PhantomData,
            };
        }
        let stage = if stage == Stage::CurrentHistory && probe.earlier_projection {
            Stage::EarlierRunHistory
        } else {
            stage
        };
        let id = probe.next_guard;
        probe.next_guard += 1;
        probe.record("stage_enter", Some(stage), Some(id), None);
        probe.stack.push(Frame { id, stage });
        StageGuard {
            session: probe.session,
            id: Some(id),
            _thread: PhantomData,
        }
    })
}
impl Drop for StageGuard {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            let Some(probe) = active.as_mut().filter(|p| p.session == self.session) else {
                return;
            };
            let Some(id) = self.id else { return };
            if probe.stack.last().is_none_or(|f| f.id != id) {
                probe.overflow = true;
                return;
            }
            let stage = probe.stack.last().unwrap().stage;
            probe.record("stage_exit", Some(stage), Some(id), None);
            probe.stack.pop();
        });
    }
}

pub(crate) struct ProjectionGuard {
    session: u64,
    previous: bool,
    _thread: PhantomData<Rc<()>>,
}
pub(crate) fn invocation_projection(earlier: bool) -> ProjectionGuard {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(probe) = active.as_mut() else {
            return ProjectionGuard {
                session: 0,
                previous: false,
                _thread: PhantomData,
            };
        };
        let previous = probe.earlier_projection;
        probe.earlier_projection = earlier;
        ProjectionGuard {
            session: probe.session,
            previous,
            _thread: PhantomData,
        }
    })
}
impl Drop for ProjectionGuard {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            if let Some(probe) = active
                .borrow_mut()
                .as_mut()
                .filter(|p| p.session == self.session)
            {
                probe.earlier_projection = self.previous;
            }
        });
    }
}

pub(crate) fn count(counter: Counter, n: usize) {
    ACTIVE.with(|active| {
        if let Some(probe) = active.borrow_mut().as_mut() {
            probe.add_counter(counter, n);
            if probe.earlier_projection {
                if let Some(earlier) = counter.earlier() {
                    probe.add_counter(earlier, n);
                }
            }
        }
    });
}
pub(crate) fn guard_occurrence_limit() -> Option<usize> {
    ACTIVE.with(|active| {
        active
            .borrow()
            .as_ref()
            .and_then(|p| (p.arm == Arm::GuardOccurrences127).then_some(127))
    })
}
pub(crate) fn is_active() -> bool {
    ACTIVE.with(|active| active.borrow().is_some())
}
pub(crate) fn checkpoint() -> Result<()> {
    ACTIVE.with(|active| match active.borrow_mut().as_mut() {
        Some(probe) => probe.check(),
        None => Ok(()),
    })
}
/// Captures real admitted sizes; caller obtains these from the unchanged encoder.
pub(crate) fn job_admitted(
    task_count: usize,
    guard_occurrences: usize,
    encoded_spec_bytes: usize,
) -> Result<()> {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(probe) = active.as_mut() else {
            return Ok(());
        };
        if probe.jobs.len() >= MAX_TASKS {
            return Err(probe.stop_error("job_record_limit"));
        }
        let job = JobAdmission {
            task_count,
            guard_occurrences,
            encoded_spec_bytes,
            elapsed_ns: probe.nanos(probe.started.elapsed()),
        };
        probe.jobs.push(job);
        probe.record("job_admitted", None, None, None);
        probe.check()
    })
}
/// Caller proves the task is fully settled/completed before invoking this hook.
/// Time/trace limits are observed here without interrupting durable settlement.
pub(crate) fn task_completed(key: &Blake3Hash, generated: usize, run: &RecordId) -> Result<()> {
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(probe) = active.as_mut() else {
            return Ok(());
        };
        if probe.completed.len() >= MAX_TASKS {
            return Err(probe.stop_error("task_limit"));
        }
        if probe.completed.iter().any(|t| t.task_key == key.as_str()) {
            probe.overflow = true;
            return Err(probe.stop_error("duplicate_task_completion"));
        }
        probe.settle();
        let task = TaskCompletion {
            task_key: key.as_str().into(),
            run_id: run.as_str().into(),
            generated_inputs: generated,
            elapsed_ns: probe.nanos(probe.started.elapsed()),
            stage_ns: probe.stage_map(),
            counters: probe.counter_map(),
        };
        probe.record("task_completed", None, None, Some(&task));
        probe.completed.push(task);
        probe.check()
    })
}

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            active.borrow_mut().take();
        });
    }
}
pub(crate) fn with_probe<T>(arm: Arm, started: Instant, work: impl FnOnce() -> T) -> (T, Report) {
    let session = NEXT_SESSION.with(|next| {
        let n = next.get();
        next.set(n.checked_add(1).expect("probe session exhausted"));
        n
    });
    let now = Instant::now();
    let preflight_ns = u64::try_from(now.duration_since(started).as_nanos());
    let preflight_overflow = preflight_ns.is_err();
    let mut stages = [0; 10];
    stages[Stage::Other as usize] = preflight_ns.unwrap_or(u64::MAX);
    ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        assert!(active.is_none(), "embedding cost probes cannot nest");
        *active = Some(Probe {
            session,
            arm,
            started,
            last: now,
            stages,
            counters: [0; 23],
            stack: Vec::with_capacity(MAX_STACK),
            next_guard: 1,
            earlier_projection: false,
            completed: Vec::with_capacity(MAX_TASKS),
            jobs: Vec::with_capacity(MAX_TASKS),
            stop: None,
            trace_records: 0,
            trace_bytes: 0,
            truncated: false,
            overflow: preflight_overflow,
        });
    });
    let reset = Reset;
    let value = work();
    let report = ACTIVE.with(|active| {
        let mut probe = active
            .borrow_mut()
            .take()
            .expect("active cost probe disappeared");
        probe.settle();
        let total_elapsed_ns = probe.nanos(probe.last.duration_since(probe.started));
        let open_stages = probe.stack.len();
        let prefix_complete = probe.completed.len() == MAX_TASKS;
        Report {
            arm,
            total_elapsed_ns,
            stage_ns: probe.stage_map(),
            counters: probe.counter_map(),
            completed_tasks: probe.completed,
            admitted_jobs: probe.jobs,
            diagnostic_stop: probe.stop,
            prefix_complete,
            censored: !prefix_complete
                || probe.stop.is_some()
                || probe.truncated
                || probe.overflow
                || open_stages != 0,
            trace_records: probe.trace_records,
            trace_bytes: probe.trace_bytes,
            trace_truncated: probe.truncated,
            instrumentation_overflow: probe.overflow,
            open_stages,
            physical_io_bytes: None,
            peak_allocation_bytes: None,
        }
    });
    drop(reset);
    (value, report)
}
