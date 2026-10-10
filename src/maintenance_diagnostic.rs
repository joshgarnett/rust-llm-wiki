//! Private, fixed-size developer diagnostic. It observes; it never grants authority.
#![allow(dead_code)]
use crate::domain::Result;
#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub(crate) enum Phase {
    Command,
    UpdateInputPlanning,
    UpdateProjection,
    PrepareWrite,
    Apply,
    RetainedAuthority,
    CheckpointValidation,
    Stage,
    IntentReplaceDone,
    SqlPublication,
    Finalize,
    Search,
    Read,
    RebuildScanParse,
    RebuildIndexPublication,
    Output,
    PathBufferConstruction,
    MarkerPathConstruction,
    RouteConstruction,
    PathWorkspaceConstruction,
    ResolveValidation,
    MetadataObservation,
    CanonicalizeObservation,
    MarkerObservation,
    DirectoryObservation,
    FileOpen,
    FileRead,
    FileBinding,
    ActivationWitness,
    ActivationSemantic,
    BatchWall,
    QueueWait,
    WorkerJob,
}
const PHASES: usize = 33;
const OWNERS: usize = 16;
const CONSTRUCTION: usize = 16;
#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Counter {
    LogicalResolve,
    RawResolve,
    PathComponent,
    MetadataCall,
    CanonicalizeCall,
    MarkerProbe,
    DirectoryEnumeration,
    EnumerationEntry,
    FileOpenCall,
    FileReadCall,
    FileReadBytes,
    FileBindingCheck,
    ActivationWitnessRead,
    ActivationWitnessBytes,
    ActivationSemanticValidation,
    Checkpoint,
    Batch,
    JobAttempt,
    JobFailure,
    JobPanic,
    ExpectedMissing,
    Overflow,
}
const COUNTERS: usize = 22;
const NAMES: [&str; PHASES] = [
    "Command",
    "UpdateInputPlanning",
    "UpdateProjection",
    "PrepareWrite",
    "Apply",
    "RetainedAuthority",
    "CheckpointValidation",
    "Stage",
    "IntentReplaceDone",
    "SqlPublication",
    "Finalize",
    "Search",
    "Read",
    "RebuildScanParse",
    "RebuildIndexPublication",
    "Output",
    "PathBufferConstruction",
    "MarkerPathConstruction",
    "RouteConstruction",
    "PathWorkspaceConstruction",
    "ResolveValidation",
    "MetadataObservation",
    "CanonicalizeObservation",
    "MarkerObservation",
    "DirectoryObservation",
    "FileOpen",
    "FileRead",
    "FileBinding",
    "ActivationWitness",
    "ActivationSemantic",
    "BatchWall",
    "QueueWait",
    "WorkerJob",
];
const COUNTER_NAMES: [&str; COUNTERS] = [
    "LogicalResolve",
    "RawResolve",
    "PathComponent",
    "MetadataCall",
    "CanonicalizeCall",
    "MarkerProbe",
    "DirectoryEnumeration",
    "EnumerationEntry",
    "FileOpenCall",
    "FileReadCall",
    "FileReadBytes",
    "FileBindingCheck",
    "ActivationWitnessRead",
    "ActivationWitnessBytes",
    "ActivationSemanticValidation",
    "Checkpoint",
    "Batch",
    "JobAttempt",
    "JobFailure",
    "JobPanic",
    "ExpectedMissing",
    "Overflow",
];

pub(crate) struct Scope {
    #[cfg(any(test, feature = "maintenance-diagnostic029"))]
    inner: Option<enabled::Scope>,
}
impl Scope {
    pub(crate) fn returned(&mut self, success: bool) {
        #[cfg(any(test, feature = "maintenance-diagnostic029"))]
        if let Some(inner) = &mut self.inner {
            inner.failed = !success;
        }
        #[cfg(not(any(test, feature = "maintenance-diagnostic029")))]
        let _ = success;
    }
}
#[inline]
pub(crate) fn span(phase: Phase) -> Scope {
    Scope {
        #[cfg(any(test, feature = "maintenance-diagnostic029"))]
        inner: enabled::Scope::start(phase),
    }
}
#[inline]
pub(crate) fn measure<T>(phase: Phase, run: impl FnOnce() -> T) -> T {
    let mut scope = span(phase);
    let value = run();
    scope.returned(true);
    value
}
#[inline]
pub(crate) fn observe<T, E>(
    phase: Phase,
    run: impl FnOnce() -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    let mut scope = span(phase);
    let result = run();
    scope.returned(result.is_ok());
    result
}
#[inline]
pub(crate) fn bump(counter: Counter, amount: u64) {
    #[cfg(any(test, feature = "maintenance-diagnostic029"))]
    enabled::bump(counter, amount);
    #[cfg(not(any(test, feature = "maintenance-diagnostic029")))]
    let _ = (counter, amount);
}
pub(crate) fn checkpoint<T>(run: impl FnOnce() -> Result<T>) -> Result<T> {
    #[cfg(any(test, feature = "maintenance-diagnostic029"))]
    let mut checkpoint = enabled::Checkpoint::start();
    let result = observe(Phase::CheckpointValidation, run);
    #[cfg(any(test, feature = "maintenance-diagnostic029"))]
    {
        checkpoint.success = result.is_ok();
    }
    result
}
#[derive(Clone, Copy)]
pub(crate) struct WorkerContext {
    #[cfg(any(test, feature = "maintenance-diagnostic029"))]
    inner: enabled::Context,
}
pub(crate) struct WorkerDelta {
    #[cfg(any(test, feature = "maintenance-diagnostic029"))]
    inner: Option<Box<enabled::Delta>>,
}
impl WorkerContext {
    pub(crate) fn capture() -> Self {
        Self {
            #[cfg(any(test, feature = "maintenance-diagnostic029"))]
            inner: enabled::capture(),
        }
    }
    pub(crate) fn reservation_bytes(&self) -> u64 {
        #[cfg(any(test, feature = "maintenance-diagnostic029"))]
        {
            if self.inner.active {
                return enabled::WORKER_WORKSPACE;
            }
        }
        0
    }
    pub(crate) fn install(self) {
        #[cfg(any(test, feature = "maintenance-diagnostic029"))]
        enabled::install(self.inner);
    }
    pub(crate) fn take(self) -> WorkerDelta {
        WorkerDelta {
            #[cfg(any(test, feature = "maintenance-diagnostic029"))]
            inner: enabled::take(),
        }
    }
}
impl WorkerDelta {
    pub(crate) fn merge(self) {
        #[cfg(any(test, feature = "maintenance-diagnostic029"))]
        if let Some(delta) = self.inner {
            enabled::merge(*delta);
        }
    }
}
pub(crate) struct Batch {
    #[cfg(any(test, feature = "maintenance-diagnostic029"))]
    inner: Option<enabled::Batch>,
}
pub(crate) fn batch(started: std::time::Instant) -> Batch {
    Batch {
        #[cfg(any(test, feature = "maintenance-diagnostic029"))]
        inner: enabled::Batch::start(started),
    }
}
/// Root-owned dispatcher seams: begin in execute, finish after existing presentation.
pub(crate) fn begin_command() {
    #[cfg(feature = "maintenance-diagnostic029")]
    if std::env::var_os("LWIKI_MAINTENANCE_DIAGNOSTIC029").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        enabled::begin();
    }
}
pub(crate) fn command_result(success: bool) {
    #[cfg(any(test, feature = "maintenance-diagnostic029"))]
    enabled::command_result(success);
    #[cfg(not(any(test, feature = "maintenance-diagnostic029")))]
    let _ = success;
}
pub(crate) struct Finish;
impl Drop for Finish {
    fn drop(&mut self) {
        #[cfg(feature = "maintenance-diagnostic029")]
        enabled::finish();
    }
}
pub(crate) struct Unwind;
impl Drop for Unwind {
    fn drop(&mut self) {
        #[cfg(feature = "maintenance-diagnostic029")]
        if std::thread::panicking() {
            enabled::command_result(false);
            enabled::finish();
        }
    }
}

#[cfg(any(test, feature = "maintenance-diagnostic029"))]
mod enabled {
    use super::*;
    use serde::Serialize;
    use std::{cell::RefCell, io::Write, time::Instant};
    const DEPTH: usize = 32;
    pub(super) const WORKER_WORKSPACE: u64 = 16 * 1024;
    #[derive(Clone, Copy, Default, Serialize)]
    pub(super) struct Stat {
        attempts: u64,
        completed: u64,
        failed: u64,
        panicked: u64,
        inclusive_ns: u64,
        exclusive_ns: u64,
    }
    #[derive(Clone, Copy)]
    struct Frame {
        phase: Phase,
        start: Instant,
        children: u64,
    }
    #[derive(Clone, Copy)]
    pub(super) struct Context {
        pub(super) active: bool,
        parent: usize,
        checkpoint: Option<u64>,
    }
    pub(super) struct Delta {
        stats: [Stat; PHASES],
        counters: [u64; COUNTERS],
        construction: [u64; 4],
        parent: usize,
        checkpoint: Option<u64>,
        overflow: bool,
        invalid: bool,
    }
    #[derive(Clone, Copy, Default, Serialize)]
    struct Attribution {
        construction_ns: u64,
        validation_ns: u64,
        other_ns: u64,
    }
    #[derive(Clone, Copy, Default, Serialize)]
    struct BatchStat {
        batches: u64,
        wall_ns: u64,
        construction_service_ns: u64,
        optimistic_ns: u64,
    }
    struct State {
        owner: [Stat; PHASES],
        worker: [Stat; PHASES],
        counters: [u64; COUNTERS],
        worker_counters: [u64; COUNTERS],
        stack: [Option<Frame>; DEPTH],
        depth: usize,
        worker_context: Option<Context>,
        command_start: Instant,
        command_children: u64,
        command_failed: bool,
        checkpoint: Option<u64>,
        checkpoint_sequence: u64,
        checkpoint_counts: [u64; 4],
        histograms: [[u64; 5]; 4],
        construction: [u64; 4],
        worker_parent: [Attribution; OWNERS],
        batches: [BatchStat; OWNERS],
        eligible_batches: [BatchStat; OWNERS],
        pending_batches: [BatchStat; OWNERS],
        eligible_owner_ns: u64,
        pending_owner_ns: u64,
        qualified_checkpoints: u64,
        witness_depth: usize,
        batch_depth: usize,
        overflow: bool,
        invalid: bool,
    }
    const _: () = assert!(std::mem::size_of::<State>() <= WORKER_WORKSPACE as usize);
    const _: () = assert!(std::mem::size_of::<Delta>() <= WORKER_WORKSPACE as usize);
    thread_local! { static CURRENT: RefCell<Option<State>> = const { RefCell::new(None) }; }
    fn add(value: &mut u64, amount: u64, overflow: &mut bool) {
        match value.checked_add(amount) {
            Some(total) => *value = total,
            None => {
                *value = u64::MAX;
                *overflow = true;
            }
        }
    }
    fn elapsed(start: Instant, overflow: &mut bool) -> u64 {
        match u64::try_from(start.elapsed().as_nanos()) {
            Ok(value) => value,
            Err(_) => {
                *overflow = true;
                u64::MAX
            }
        }
    }
    fn state(context: Option<Context>) -> State {
        State {
            owner: [Stat::default(); PHASES],
            worker: [Stat::default(); PHASES],
            counters: [0; COUNTERS],
            worker_counters: [0; COUNTERS],
            stack: [None; DEPTH],
            depth: 0,
            worker_context: context,
            command_start: Instant::now(),
            command_children: 0,
            command_failed: false,
            checkpoint: context.and_then(|c| c.checkpoint),
            checkpoint_sequence: 0,
            checkpoint_counts: [0; 4],
            histograms: [[0; 5]; 4],
            construction: [0; 4],
            worker_parent: [Attribution::default(); OWNERS],
            batches: [BatchStat::default(); OWNERS],
            eligible_batches: [BatchStat::default(); OWNERS],
            pending_batches: [BatchStat::default(); OWNERS],
            eligible_owner_ns: 0,
            pending_owner_ns: 0,
            qualified_checkpoints: 0,
            witness_depth: 0,
            batch_depth: 0,
            overflow: false,
            invalid: false,
        }
    }
    pub(super) fn begin() {
        CURRENT.with(|slot| {
            let mut current = slot.borrow_mut();
            let nested = current.is_some();
            let mut value = state(None);
            value.invalid = nested;
            *current = Some(value);
        });
    }
    pub(super) fn command_result(success: bool) {
        CURRENT.with(|slot| {
            if let Some(s) = &mut *slot.borrow_mut() {
                s.command_failed |= !success;
            }
        });
    }
    pub(super) fn bump(counter: Counter, amount: u64) {
        CURRENT.with(|slot| {
            if let Some(s) = &mut *slot.borrow_mut() {
                add(&mut s.counters[counter as usize], amount, &mut s.overflow);
            }
        });
    }
    pub(super) struct Scope {
        phase: Phase,
        depth: usize,
        start: Instant,
        pub(super) failed: bool,
    }
    impl Scope {
        pub(super) fn start(phase: Phase) -> Option<Self> {
            CURRENT.with(|slot| {
                let mut slot = slot.borrow_mut();
                let s = slot.as_mut()?;
                let depth = s.depth;
                let start = Instant::now();
                add(&mut s.owner[phase as usize].attempts, 1, &mut s.overflow);
                if depth >= DEPTH {
                    s.invalid = true;
                    s.overflow = true;
                } else {
                    s.stack[depth] = Some(Frame {
                        phase,
                        start,
                        children: 0,
                    });
                    s.depth += 1;
                }
                if phase as usize >= CONSTRUCTION && (phase as usize) < CONSTRUCTION + 4 {
                    let index = phase as usize - CONSTRUCTION;
                    add(&mut s.construction[index], 1, &mut s.overflow);
                    if s.checkpoint.is_some() {
                        add(&mut s.checkpoint_counts[index], 1, &mut s.overflow);
                    }
                }
                if matches!(phase, Phase::WorkerJob) {
                    add(
                        &mut s.counters[Counter::JobAttempt as usize],
                        1,
                        &mut s.overflow,
                    );
                }
                Some(Self {
                    phase,
                    depth,
                    start,
                    failed: false,
                })
            })
        }
    }
    impl Drop for Scope {
        fn drop(&mut self) {
            CURRENT.with(|slot| {
                let mut slot = slot.borrow_mut();
                let Some(s) = slot.as_mut() else {
                    return;
                };
                let panic = std::thread::panicking();
                let ns = elapsed(self.start, &mut s.overflow);
                let mut children = 0;
                if self.depth < DEPTH && s.depth == self.depth + 1 {
                    if let Some(frame) = s.stack[self.depth].take() {
                        children = frame.children;
                    } else {
                        s.invalid = true;
                    }
                    s.depth -= 1;
                } else {
                    s.invalid = true;
                }
                let exclusive = ns.checked_sub(children).unwrap_or_else(|| {
                    s.invalid = true;
                    0
                });
                if self.phase as usize == CONSTRUCTION
                    && s.worker_context.is_none()
                    && s.checkpoint.is_some()
                    && s.batch_depth == 0
                {
                    add(&mut s.pending_owner_ns, exclusive, &mut s.overflow);
                }
                let row = &mut s.owner[self.phase as usize];
                add(&mut row.completed, 1, &mut s.overflow);
                add(
                    &mut row.failed,
                    u64::from(self.failed || panic),
                    &mut s.overflow,
                );
                add(&mut row.panicked, u64::from(panic), &mut s.overflow);
                add(&mut row.inclusive_ns, ns, &mut s.overflow);
                add(&mut row.exclusive_ns, exclusive, &mut s.overflow);
                if s.depth > 0 {
                    if let Some(parent) = &mut s.stack[s.depth - 1] {
                        add(&mut parent.children, ns, &mut s.overflow);
                    }
                } else {
                    add(&mut s.command_children, ns, &mut s.overflow);
                }
                if matches!(self.phase, Phase::WorkerJob) {
                    add(
                        &mut s.counters[Counter::JobFailure as usize],
                        u64::from(self.failed || panic),
                        &mut s.overflow,
                    );
                    add(
                        &mut s.counters[Counter::JobPanic as usize],
                        u64::from(panic),
                        &mut s.overflow,
                    );
                }
                if matches!(self.phase, Phase::Output) {
                    s.command_failed |= self.failed || panic;
                }
            });
        }
    }
    pub(super) struct WitnessRead {
        previous: Option<usize>,
    }
    impl WitnessRead {
        pub(super) fn start() -> Self {
            Self {
                previous: CURRENT.with(|slot| {
                    let mut slot = slot.borrow_mut();
                    let s = slot.as_mut()?;
                    let previous = s.witness_depth;
                    if previous != 0 {
                        s.invalid = true;
                    }
                    s.witness_depth = previous.saturating_add(1);
                    Some(previous)
                }),
            }
        }
    }
    impl Drop for WitnessRead {
        fn drop(&mut self) {
            if let Some(previous) = self.previous {
                CURRENT.with(|slot| {
                    if let Some(s) = slot.borrow_mut().as_mut() {
                        s.witness_depth = previous;
                    }
                });
            }
        }
    }
    pub(super) fn witness_bytes(bytes: u64) {
        CURRENT.with(|slot| {
            if let Some(s) = slot.borrow_mut().as_mut() {
                if s.witness_depth > 0 {
                    add(
                        &mut s.counters[Counter::ActivationWitnessBytes as usize],
                        bytes,
                        &mut s.overflow,
                    );
                }
            }
        });
    }
    #[cfg(test)]
    pub(super) fn test_check_and_clear() {
        let state = CURRENT.with(|slot| slot.borrow_mut().take());
        if std::thread::panicking() {
            return;
        }
        let s = state.expect("explicit diagnostic test context");
        assert!(!s.invalid && !s.overflow);
        assert_eq!((s.depth, s.batch_depth, s.witness_depth), (0, 0, 0));
        assert!(s.checkpoint.is_none());
        for row in s.owner.iter().chain(s.worker.iter()) {
            assert_eq!(row.attempts, row.completed);
            assert!(row.panicked <= row.failed && row.failed <= row.completed);
            assert!(row.exclusive_ns <= row.inclusive_ns);
        }
    }
    pub(super) fn capture() -> Context {
        CURRENT.with(|slot| {
            let slot = slot.borrow();
            match slot.as_ref() {
                None => Context {
                    active: false,
                    parent: 0,
                    checkpoint: None,
                },
                Some(s) => Context {
                    active: true,
                    parent: s.stack[..s.depth]
                        .iter()
                        .rev()
                        .flatten()
                        .find(|f| (f.phase as usize) < OWNERS)
                        .map_or(0, |f| f.phase as usize),
                    checkpoint: s.checkpoint,
                },
            }
        })
    }
    pub(super) fn install(context: Context) {
        if context.active {
            CURRENT.with(|slot| {
                let mut current = slot.borrow_mut();
                let nested = current.is_some();
                let mut s = state(Some(context));
                s.invalid = nested;
                *current = Some(s);
            });
        }
    }
    pub(super) fn take() -> Option<Box<Delta>> {
        CURRENT.with(|slot| {
            let mut s = slot.borrow_mut().take()?;
            let context = s.worker_context?;
            s.invalid |= s.depth != 0 || s.batch_depth != 0 || s.witness_depth != 0;
            Some(Box::new(Delta {
                stats: s.owner,
                counters: s.counters,
                construction: s.checkpoint_counts,
                parent: context.parent,
                checkpoint: context.checkpoint,
                overflow: s.overflow,
                invalid: s.invalid,
            }))
        })
    }
    pub(super) fn merge(delta: Delta) {
        CURRENT.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(s) = slot.as_mut() else {
                return;
            };
            s.overflow |= delta.overflow;
            s.invalid |= delta.invalid || delta.checkpoint != s.checkpoint;
            for (target, source) in s.worker.iter_mut().zip(delta.stats) {
                add(&mut target.attempts, source.attempts, &mut s.overflow);
                add(&mut target.completed, source.completed, &mut s.overflow);
                add(&mut target.failed, source.failed, &mut s.overflow);
                add(&mut target.panicked, source.panicked, &mut s.overflow);
                add(
                    &mut target.inclusive_ns,
                    source.inclusive_ns,
                    &mut s.overflow,
                );
                add(
                    &mut target.exclusive_ns,
                    source.exclusive_ns,
                    &mut s.overflow,
                );
            }
            for (target, value) in s.worker_counters.iter_mut().zip(delta.counters) {
                add(target, value, &mut s.overflow);
            }
            if s.checkpoint.is_some() {
                for (target, count) in s.checkpoint_counts.iter_mut().zip(delta.construction) {
                    add(target, count, &mut s.overflow);
                }
            }
            let parent = &mut s.worker_parent[delta.parent];
            for (index, row) in delta.stats.iter().enumerate() {
                let target = if index == CONSTRUCTION {
                    &mut parent.construction_ns
                } else if (20..30).contains(&index) {
                    &mut parent.validation_ns
                } else {
                    &mut parent.other_ns
                };
                add(target, row.exclusive_ns, &mut s.overflow);
            }
        });
    }
    pub(super) struct Checkpoint {
        admitted: bool,
        pub(super) success: bool,
    }
    impl Checkpoint {
        pub(super) fn start() -> Self {
            let admitted = CURRENT.with(|slot| {
                let mut slot = slot.borrow_mut();
                let Some(s) = slot.as_mut() else {
                    return false;
                };
                if s.checkpoint.is_some() || s.worker_context.is_some() {
                    s.invalid = true;
                    return false;
                }
                add(&mut s.checkpoint_sequence, 1, &mut s.overflow);
                s.checkpoint = Some(s.checkpoint_sequence);
                s.checkpoint_counts = [0; 4];
                s.pending_owner_ns = 0;
                s.pending_batches = [BatchStat::default(); OWNERS];
                add(
                    &mut s.counters[Counter::Checkpoint as usize],
                    1,
                    &mut s.overflow,
                );
                true
            });
            Self {
                admitted,
                success: false,
            }
        }
    }
    impl Drop for Checkpoint {
        fn drop(&mut self) {
            if !self.admitted {
                return;
            }
            CURRENT.with(|slot| {
                let mut slot = slot.borrow_mut();
                let Some(s) = slot.as_mut() else {
                    return;
                };
                if s.batch_depth != 0 || std::thread::panicking() {
                    s.invalid = true;
                } else {
                    if self.success && s.checkpoint_counts[0] >= 4 {
                        add(&mut s.qualified_checkpoints, 1, &mut s.overflow);
                        add(
                            &mut s.eligible_owner_ns,
                            s.pending_owner_ns,
                            &mut s.overflow,
                        );
                        for (target, source) in s.eligible_batches.iter_mut().zip(s.pending_batches)
                        {
                            add(&mut target.batches, source.batches, &mut s.overflow);
                            add(&mut target.wall_ns, source.wall_ns, &mut s.overflow);
                            add(
                                &mut target.construction_service_ns,
                                source.construction_service_ns,
                                &mut s.overflow,
                            );
                            add(
                                &mut target.optimistic_ns,
                                source.optimistic_ns,
                                &mut s.overflow,
                            );
                        }
                    }
                    for (histogram, count) in s.histograms.iter_mut().zip(s.checkpoint_counts) {
                        add(&mut histogram[count.min(4) as usize], 1, &mut s.overflow);
                    }
                }
                s.checkpoint = None;
                s.checkpoint_counts = [0; 4];
            });
        }
    }
    pub(super) struct Batch {
        start: Instant,
        construction: u64,
        owner_construction: u64,
        parent: usize,
    }
    impl Batch {
        pub(super) fn start(start: Instant) -> Option<Self> {
            CURRENT.with(|slot| {
                let mut slot = slot.borrow_mut();
                let s = slot.as_mut()?;
                if s.batch_depth != 0 || s.worker_context.is_some() {
                    s.invalid = true;
                }
                s.batch_depth += 1;
                add(&mut s.counters[Counter::Batch as usize], 1, &mut s.overflow);
                Some(Self {
                    start,
                    construction: s.worker[CONSTRUCTION].exclusive_ns,
                    owner_construction: s.owner[CONSTRUCTION].exclusive_ns,
                    parent: s.stack[..s.depth]
                        .iter()
                        .rev()
                        .flatten()
                        .find(|f| (f.phase as usize) < OWNERS)
                        .map_or(0, |f| f.phase as usize),
                })
            })
        }
    }
    impl Drop for Batch {
        fn drop(&mut self) {
            CURRENT.with(|slot| {
                let mut slot = slot.borrow_mut();
                let Some(s) = slot.as_mut() else {
                    return;
                };
                let wall = elapsed(self.start, &mut s.overflow);
                let construction = s.worker[CONSTRUCTION]
                    .exclusive_ns
                    .checked_sub(self.construction)
                    .unwrap_or_else(|| {
                        s.invalid = true;
                        0
                    });
                let owner_construction = s.owner[CONSTRUCTION]
                    .exclusive_ns
                    .checked_sub(self.owner_construction)
                    .unwrap_or_else(|| {
                        s.invalid = true;
                        0
                    });
                let mut construction = construction;
                add(&mut construction, owner_construction, &mut s.overflow);
                let row = &mut s.batches[self.parent];
                add(&mut row.batches, 1, &mut s.overflow);
                add(&mut row.wall_ns, wall, &mut s.overflow);
                add(
                    &mut row.construction_service_ns,
                    construction,
                    &mut s.overflow,
                );
                add(
                    &mut row.optimistic_ns,
                    wall.min(construction),
                    &mut s.overflow,
                );
                if s.checkpoint.is_some() && s.batch_depth == 1 && s.worker_context.is_none() {
                    let pending = &mut s.pending_batches[self.parent];
                    add(&mut pending.batches, 1, &mut s.overflow);
                    add(&mut pending.wall_ns, wall, &mut s.overflow);
                    add(
                        &mut pending.construction_service_ns,
                        construction,
                        &mut s.overflow,
                    );
                    add(
                        &mut pending.optimistic_ns,
                        wall.min(construction),
                        &mut s.overflow,
                    );
                }
                s.batch_depth = s.batch_depth.saturating_sub(1);
            });
        }
    }
    #[derive(Serialize)]
    struct Summary<'a> {
        schema: &'static str,
        valid: bool,
        overflow: bool,
        command_failed: bool,
        phase_names: &'a [&'static str],
        counter_names: &'a [&'static str],
        owner: &'a [Stat],
        worker: &'a [Stat],
        owner_counters: &'a [u64],
        worker_counters: &'a [u64],
        worker_attribution_by_owner_phase: &'a [Attribution],
        joined_batch_by_owner_phase: &'a [BatchStat],
        checkpoint_construction_histograms_0_1_2_3_ge4: &'a [[u64; 5]],
        qualified_checkpoint_count: u64,
        eligible_owner_exclusive_outside_batches_ns: u64,
        eligible_joined_batch_by_owner_phase: &'a [BatchStat],
        eligible_construction_phase: &'static str,
        timing: &'static str,
    }
    struct Bounded {
        bytes: [u8; 65515],
        len: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.bytes.len() - self.len {
                return Err(std::io::Error::other("diagnostic summary cap"));
            }
            self.bytes[self.len..self.len + bytes.len()].copy_from_slice(bytes);
            self.len += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    pub(super) fn finish() {
        let state = CURRENT.with(|slot| slot.borrow_mut().take());
        let Some(mut s) = state else {
            return;
        };
        s.invalid |= s.depth != 0 || s.batch_depth != 0 || s.checkpoint.is_some();
        let wall = elapsed(s.command_start, &mut s.overflow);
        let exclusive = wall.checked_sub(s.command_children).unwrap_or_else(|| {
            s.invalid = true;
            0
        });
        s.owner[0] = Stat {
            attempts: 1,
            completed: 1,
            failed: u64::from(s.command_failed || std::thread::panicking()),
            panicked: u64::from(std::thread::panicking()),
            inclusive_ns: wall,
            exclusive_ns: exclusive,
        };
        if s.overflow {
            s.counters[Counter::Overflow as usize] = 1;
        }
        let summary = Summary {
            schema: "lwiki.maintenance-diagnostic029.v1",
            valid: !s.invalid && !s.overflow,
            overflow: s.overflow,
            command_failed: s.command_failed,
            phase_names: &NAMES,
            counter_names: &COUNTER_NAMES,
            owner: &s.owner,
            worker: &s.worker,
            owner_counters: &s.counters,
            worker_counters: &s.worker_counters,
            worker_attribution_by_owner_phase: &s.worker_parent,
            joined_batch_by_owner_phase: &s.batches,
            checkpoint_construction_histograms_0_1_2_3_ge4: &s.histograms,
            qualified_checkpoint_count: s.qualified_checkpoints,
            eligible_owner_exclusive_outside_batches_ns: s.eligible_owner_ns,
            eligible_joined_batch_by_owner_phase: &s.eligible_batches,
            eligible_construction_phase: "PathBufferConstruction: immutable root buffer only; source hypothesis, not demonstrated reuse",
            timing: "owner monotonic; exclusive owner, enclosing batch wall, summed worker service are separate and must not be added",
        };
        let mut output = Bounded {
            bytes: [0; 65515],
            len: 0,
        };
        let encoded = serde_json::to_writer(&mut output, &summary);
        let mut stderr = std::io::stderr().lock();
        if encoded.is_ok() {
            let _ = stderr.write_all(b"LWIKI_DIAGNOSTIC029 ");
            let _ = stderr.write_all(&output.bytes[..output.len]);
            let _ = stderr.write_all(b"\n");
        } else {
            let _ = stderr.write_all(b"LWIKI_DIAGNOSTIC029 {\"schema\":\"lwiki.maintenance-diagnostic029.v1\",\"valid\":false,\"overflow\":true,\"summary_cap_exceeded\":true}\n");
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn balanced_success_error_and_panic_scopes_preserve_results() {
            begin();
            assert_eq!(observe(Phase::Stage, || Ok::<_, u8>(7)), Ok(7));
            assert_eq!(observe(Phase::Stage, || Err::<u8, _>(9)), Err(9));
            assert!(
                std::panic::catch_unwind(|| observe::<u8, u8>(Phase::Stage, || panic!("probe")))
                    .is_err()
            );
            CURRENT.with(|slot| {
                let s = slot.borrow();
                let s = s.as_ref().unwrap();
                assert_eq!(s.depth, 0);
                let row = s.owner[Phase::Stage as usize];
                assert_eq!(
                    (row.attempts, row.completed, row.failed, row.panicked),
                    (3, 3, 2, 1)
                );
                assert!(!s.invalid);
            });
            CURRENT.with(|slot| slot.borrow_mut().take());
        }
        #[test]
        fn joined_failed_and_panicked_work_conserves_counts_per_checkpoint() {
            begin();
            checkpoint(|| {
                let batch_guard = batch(std::time::Instant::now());
                let context = WorkerContext::capture();
                let (delta, failed, panicked) = std::thread::spawn(move || {
                    context.install();
                    let failed = observe(Phase::WorkerJob, || {
                        for _ in 0..4 {
                            measure(Phase::PathBufferConstruction, || ());
                        }
                        Err::<(), u8>(3)
                    });
                    let panicked = std::panic::catch_unwind(|| {
                        observe::<(), u8>(Phase::WorkerJob, || panic!("probe"))
                    });
                    (context.take(), failed, panicked.is_err())
                })
                .join()
                .unwrap();
                assert_eq!(failed, Err(3));
                assert!(panicked);
                delta.merge();
                drop(batch_guard);
                Ok(())
            })
            .unwrap();
            CURRENT.with(|slot| {
                let s = slot.borrow();
                let s = s.as_ref().unwrap();
                assert_eq!(s.worker_counters[Counter::JobAttempt as usize], 2);
                assert_eq!(s.worker_counters[Counter::JobFailure as usize], 2);
                assert_eq!(s.worker_counters[Counter::JobPanic as usize], 1);
                assert_eq!(s.histograms[0], [0, 0, 0, 0, 1]);
                assert_eq!(s.qualified_checkpoints, 1);
                let eligible = s.eligible_batches[Phase::CheckpointValidation as usize];
                assert_eq!(eligible.batches, 1);
                assert_eq!(
                    eligible.optimistic_ns,
                    eligible.wall_ns.min(eligible.construction_service_ns)
                );
                assert_eq!(s.eligible_owner_ns, 0);
                assert_eq!(s.checkpoint, None);
                assert!(!s.invalid);
            });
            checkpoint(|| {
                measure(Phase::PathBufferConstruction, || ());
                Ok(())
            })
            .unwrap();
            CURRENT.with(|slot| {
                let s = slot.borrow();
                let s = s.as_ref().unwrap();
                assert_eq!(s.histograms[0], [0, 1, 0, 0, 1]);
                assert_eq!(s.qualified_checkpoints, 1);
                assert_eq!(s.eligible_owner_ns, 0);
            });
            checkpoint(|| {
                for _ in 0..4 {
                    measure(Phase::PathBufferConstruction, || ());
                }
                Err::<(), _>(crate::domain::WikiError::new(
                    crate::domain::ErrorCode::ContentConflict,
                    "expected checkpoint failure",
                ))
            })
            .unwrap_err();
            CURRENT.with(|slot| {
                let s = slot.borrow();
                let s = s.as_ref().unwrap();
                assert_eq!(s.qualified_checkpoints, 1);
                assert_eq!(s.eligible_owner_ns, 0);
                assert_eq!(s.histograms[0], [0, 1, 0, 0, 2]);
            });
            let before_sequential = CURRENT.with(|slot| {
                let s = slot.borrow();
                let s = s.as_ref().unwrap();
                (
                    s.owner[CONSTRUCTION].exclusive_ns,
                    s.eligible_batches[Phase::CheckpointValidation as usize],
                )
            });
            crate::maintenance_parallel::sequential_scope(|| {
                checkpoint(|| {
                    crate::maintenance_parallel::run_batch(vec![
                        crate::maintenance_parallel::Job::new(1024 * 1024, || {
                            for _ in 0..4 {
                                measure(Phase::PathBufferConstruction, || ());
                            }
                            Ok(())
                        }),
                    ])?;
                    Ok(())
                })
            })
            .unwrap();
            CURRENT.with(|slot| {
                let s = slot.borrow();
                let s = s.as_ref().unwrap();
                let row = s.eligible_batches[Phase::CheckpointValidation as usize];
                assert_eq!(s.qualified_checkpoints, 2);
                assert_eq!(row.batches, 2);
                assert_eq!(s.eligible_owner_ns, 0);
                assert!(row.construction_service_ns >= s.worker[CONSTRUCTION].exclusive_ns);
                assert!(
                    row.optimistic_ns <= row.wall_ns
                        && row.optimistic_ns <= row.construction_service_ns
                );
                assert_eq!(s.counters[Counter::JobAttempt as usize], 1);
                let service =
                    row.construction_service_ns - before_sequential.1.construction_service_ns;
                let wall = row.wall_ns - before_sequential.1.wall_ns;
                assert_eq!(
                    service,
                    s.owner[CONSTRUCTION].exclusive_ns - before_sequential.0
                );
                assert_eq!(
                    row.optimistic_ns - before_sequential.1.optimistic_ns,
                    wall.min(service)
                );
            });
            let failure = witness_read(|| {
                assert_eq!(read(|| Ok(3)).unwrap(), 3);
                assert_eq!(read(|| Ok(5)).unwrap(), 5);
                assert!(read(|| Err(std::io::Error::other("expected read failure"))).is_err());
                Err::<(), _>(crate::domain::WikiError::new(
                    crate::domain::ErrorCode::ContentConflict,
                    "later authority failure",
                ))
            });
            assert!(failure.is_err());
            CURRENT.with(|slot| {
                let s = slot.borrow();
                let s = s.as_ref().unwrap();
                assert_eq!(s.counters[Counter::ActivationWitnessBytes as usize], 8);
                assert_eq!(s.counters[Counter::FileReadBytes as usize], 8);
                assert_eq!(s.counters[Counter::FileReadCall as usize], 3);
                assert_eq!(s.witness_depth, 0);
            });
            CURRENT.with(|slot| slot.borrow_mut().take());
        }
        #[test]
        fn checked_overflow_is_explicit_and_off_context_stays_inert() {
            let mut value = u64::MAX;
            let mut overflow = false;
            add(&mut value, 1, &mut overflow);
            assert!(overflow);
            assert_eq!(value, u64::MAX);
            CURRENT.with(|slot| slot.borrow_mut().take());
            assert_eq!(measure(Phase::Read, || 11), 11);
            assert_eq!(observe(Phase::Read, || Err::<(), _>(12)), Err(12));
            assert!(!capture().active);
            assert!(take().is_none());
        }
    }
}

#[inline]
pub(crate) fn read(run: impl FnOnce() -> std::io::Result<usize>) -> std::io::Result<usize> {
    bump(Counter::FileReadCall, 1);
    let result = observe(Phase::FileRead, run);
    if let Ok(bytes) = &result {
        bump(Counter::FileReadBytes, *bytes as u64);
        #[cfg(any(test, feature = "maintenance-diagnostic029"))]
        enabled::witness_bytes(*bytes as u64);
    }
    result
}

/// Read-only diagnostic scope: does not change the authority read or its result.
pub(crate) fn witness_read<T>(run: impl FnOnce() -> Result<T>) -> Result<T> {
    #[cfg(any(test, feature = "maintenance-diagnostic029"))]
    let _witness = enabled::WitnessRead::start();
    run()
}
#[cfg(test)]
pub(crate) struct TestDiagnostic;
#[cfg(test)]
impl TestDiagnostic {
    pub(crate) fn start() -> Self {
        enabled::begin();
        Self
    }
}
#[cfg(test)]
impl Drop for TestDiagnostic {
    fn drop(&mut self) {
        enabled::test_check_and_clear();
    }
}
