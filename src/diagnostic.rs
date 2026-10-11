//! Opt-in aggregate measurements for one local diagnostic invocation.
//! This module records no identities, paths, arguments or captured content.
use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::Write,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

static ENABLED: AtomicBool = AtomicBool::new(false);
thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }
const LABEL_LIMIT: usize = 64;
const STAGES: &[&str] = &[
    "startup_and_dispatch",
    "public_input_and_planning",
    "joint_projection",
    "prepare_and_seal",
    "canonical_file_application",
    "selected_sql_dml",
    "sql_commit",
    "wal_checkpoint",
    "final_verification_and_acknowledgement",
    "unclassified",
];
#[derive(Default, serde::Serialize)]
struct Counter {
    calls: u64,
    elapsed_ns: u64,
    count: u64,
    logical_bytes: u64,
}
struct State {
    start: Instant,
    last: Instant,
    current: &'static str,
    exclusive: BTreeMap<&'static str, u64>,
    counters: BTreeMap<&'static str, Counter>,
    overflow: u64,
    invalid_stages: u64,
}
fn ns(elapsed: std::time::Duration) -> u64 {
    elapsed.as_nanos().min(u64::MAX as u128) as u64
}
impl State {
    fn flush(&mut self, now: Instant) {
        let elapsed = ns(now.duration_since(self.last));
        let value = self.exclusive.entry(self.current).or_default();
        *value = value.saturating_add(elapsed);
        self.last = now;
    }
    fn counter(&mut self, name: &'static str) -> Option<&mut Counter> {
        if !self.counters.contains_key(name) && self.counters.len() == LABEL_LIMIT {
            self.overflow = self.overflow.saturating_add(1);
            return None;
        }
        Some(self.counters.entry(name).or_default())
    }
}

/// Called once, before argument parsing. Disabled measurements never touch TLS.
pub fn begin() {
    if std::env::var_os("LWIKI_STAGE_TRACE").is_none_or(|value| value != "1") {
        return;
    }
    let now = Instant::now();
    STATE.with(|state| {
        *state.borrow_mut() = Some(State {
            start: now,
            last: now,
            current: "startup_and_dispatch",
            exclusive: BTreeMap::new(),
            counters: BTreeMap::new(),
            overflow: 0,
            invalid_stages: 0,
        })
    });
    ENABLED.store(true, Ordering::Relaxed);
}

pub struct StageGuard {
    previous: Option<&'static str>,
}
pub fn stage(label: &'static str) -> StageGuard {
    if !ENABLED.load(Ordering::Relaxed) {
        return StageGuard { previous: None };
    }
    let previous = STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        let state = state.as_mut()?;
        if !STAGES.contains(&label) {
            state.invalid_stages = state.invalid_stages.saturating_add(1);
            return None;
        }
        state.flush(Instant::now());
        let previous = state.current;
        state.current = label;
        Some(previous)
    });
    StageGuard { previous }
}
impl Drop for StageGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous
            && ENABLED.load(Ordering::Relaxed)
        {
            STATE.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut() {
                    state.flush(Instant::now());
                    state.current = previous;
                }
            });
        }
    }
}

pub struct CounterGuard {
    value: Option<(&'static str, Instant)>,
}
pub fn counter_span(label: &'static str) -> CounterGuard {
    if !ENABLED.load(Ordering::Relaxed) {
        return CounterGuard { value: None };
    }
    let value = STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        let counter = state.as_mut()?.counter(label)?;
        counter.calls = counter.calls.saturating_add(1);
        Some((label, Instant::now()))
    });
    CounterGuard { value }
}
impl Drop for CounterGuard {
    fn drop(&mut self) {
        if let Some((label, start)) = self.value
            && ENABLED.load(Ordering::Relaxed)
        {
            let elapsed = ns(start.elapsed());
            STATE.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut()
                    && let Some(counter) = state.counter(label)
                {
                    counter.elapsed_ns = counter.elapsed_ns.saturating_add(elapsed);
                }
            });
        }
    }
}
pub fn count(label: &'static str, amount: u64) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    STATE.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut()
            && let Some(counter) = state.counter(label)
        {
            counter.count = counter.count.saturating_add(amount);
        }
    });
}
pub fn bytes(label: &'static str, amount: u64) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    STATE.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut()
            && let Some(counter) = state.counter(label)
        {
            counter.logical_bytes = counter.logical_bytes.saturating_add(amount);
        }
    });
}

/// Explicitly flush before process::exit; nested counter durations are nonadditive.
pub fn finish(exit: i32) {
    if !ENABLED.swap(false, Ordering::Relaxed) {
        return;
    }
    let state = STATE.with(|cell| cell.borrow_mut().take());
    if let Some(mut state) = state {
        let now = Instant::now();
        state.flush(now);
        let report = serde_json::json!({
            "schema": "lwiki.stage-trace.v1", "exit": exit,
            "owner_elapsed_ns": ns(now.duration_since(state.start)),
            "exclusive_stage_ns": state.exclusive, "nested_counters": state.counters,
            "counter_elapsed_is_nonadditive": true, "owner_thread_only": true,
            "endpoint": "after ordinary output; before diagnostic serialization, stderr write and process exit",
            "overflow_events": state.overflow, "invalid_stage_events": state.invalid_stages,
            "physical_io_bytes": null,
        });
        if let Ok(raw) = serde_json::to_vec(&report)
            && raw.len() <= 1024 * 1024
        {
            let mut stderr = std::io::stderr().lock();
            let _ = stderr.write_all(b"LWIKI_STAGE_TRACE_V1 ");
            let _ = stderr.write_all(&raw);
            let _ = stderr.write_all(b"\n");
        }
    }
}
