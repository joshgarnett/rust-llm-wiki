//! One explicit operation's allowance, composed with durable per-Run admission.
use crate::{
    domain::*,
    jobs::{self, *},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
};

/// Shared only by dispatches belonging to one public operation. Durable Runs
/// retain their own immutable limits and account for all historical attempts.
pub struct InvocationBudget {
    limits: LifetimeLimits,
    started_at_utc_ms: i64,
    deadline_utc_ms: i64,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    epoch: u64,
    entries: Vec<Entry>,
    blocked: bool,
}
struct Entry {
    ledger: Arc<JobLedger>,
    run: RecordId,
    attempt: Option<AttemptRef>,
    allowance: Allowance,
    admitted_at_utc_ms: i64,
}
struct Projection {
    ledger: Arc<JobLedger>,
    attempts: Vec<AttemptRef>,
}
fn error(message: &str) -> WikiError {
    let mut error = WikiError::new(ErrorCode::BudgetExceeded, message);
    error.details = serde_json::json!({"budget_scope":"invocation"});
    error
}
fn overflow() -> WikiError {
    error("invocation checked accounting overflow")
}
impl InvocationBudget {
    pub fn new(
        limits: LifetimeLimits,
        started_at_utc_ms: i64,
        deadline_utc_ms: i64,
    ) -> Result<Self> {
        jobs::budgets::validate_limits(&limits)?;
        if started_at_utc_ms < 0 || deadline_utc_ms <= started_at_utc_ms {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "invalid invocation deadline",
            ));
        }
        Ok(Self {
            limits,
            started_at_utc_ms,
            deadline_utc_ms,
            state: Mutex::new(State::default()),
        })
    }
    pub fn started_at_utc_ms(&self) -> i64 {
        self.started_at_utc_ms
    }
    pub fn deadline_utc_ms(&self) -> i64 {
        self.deadline_utc_ms
    }
    fn lock(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| error("invocation accounting lock unavailable"))
    }
    /// Quote, reconcile and atomically reserve before durable admission. No
    /// invocation lock spans a ledger read or any other filesystem operation.
    pub(crate) fn reserve(
        &self,
        ledger: &JobLedger,
        bound: &AttemptBound,
        now: i64,
        deadline: i64,
    ) -> Result<usize> {
        let allowance = jobs::budgets::quote_bound(
            bound,
            &self.limits,
            now,
            deadline.min(self.deadline_utc_ms),
        )
        .map_err(|mut e| {
            e.details = serde_json::json!({"budget_scope":"invocation"});
            e
        })?;
        let (fs, vault, run, options) = ledger.dispatcher_bindings();
        let enrolled_ledger = Arc::new(JobLedger::new(fs, vault, run.clone(), options)?);
        loop {
            let (epoch, projections) = {
                let state = self.lock()?;
                if now < self.started_at_utc_ms
                    || state
                        .entries
                        .last()
                        .is_some_and(|entry| now < entry.admitted_at_utc_ms)
                {
                    return Err(error("invocation dispatch clock regressed"));
                }
                if state.blocked {
                    return Err(error("invocation has unresolved durable admission"));
                }
                if state.entries.len() >= RUN_MAX_TASKS * 16 {
                    return Err(error("invocation registry ceiling reached"));
                }
                let mut groups = BTreeMap::<RecordId, Projection>::new();
                for entry in &state.entries {
                    if let Some(attempt) = &entry.attempt {
                        groups
                            .entry(entry.run.clone())
                            .or_insert_with(|| Projection {
                                ledger: entry.ledger.clone(),
                                attempts: Vec::new(),
                            })
                            .attempts
                            .push(attempt.clone());
                    }
                }
                (state.epoch, groups.into_values().collect::<Vec<_>>())
            };
            let mut settled = jobs::budgets::zero();
            let mut outstanding = jobs::budgets::zero();
            let mut concurrency = 0u32;
            for projection in projections {
                let budget = projection
                    .ledger
                    .invocation_accounting(&projection.attempts)?;
                if !budget.guarantee_intact {
                    return Err(error("invocation accounting guarantee violated"));
                }
                jobs::budgets::add(&mut settled, &budget.settled)?;
                jobs::budgets::add(&mut outstanding, &budget.outstanding)?;
                concurrency = concurrency
                    .checked_add(budget.concurrency_admitted)
                    .ok_or_else(overflow)?;
            }
            let mut state = self.lock()?;
            if state.epoch != epoch {
                continue;
            }
            if state.blocked {
                return Err(error("invocation has unresolved durable admission"));
            }
            if now < self.started_at_utc_ms
                || state
                    .entries
                    .last()
                    .is_some_and(|entry| now < entry.admitted_at_utc_ms)
            {
                return Err(error("invocation dispatch clock regressed"));
            }
            for entry in state.entries.iter().filter(|entry| entry.attempt.is_none()) {
                jobs::budgets::add(&mut outstanding, &entry.allowance)?;
                concurrency = concurrency.checked_add(1).ok_or_else(overflow)?;
            }
            if concurrency >= self.limits.concurrency {
                return Err(error("invocation concurrency ceiling reached"));
            }
            jobs::budgets::fits(&settled, &outstanding, &allowance, &self.limits).map_err(
                |mut e| {
                    e.details = serde_json::json!({"budget_scope":"invocation"});
                    e
                },
            )?;
            self.rate_check(&state, &allowance, now)?;
            let next_epoch = state.epoch.checked_add(1).ok_or_else(overflow)?;
            let slot = state.entries.len();
            state.entries.push(Entry {
                ledger: enrolled_ledger,
                run,
                attempt: None,
                allowance,
                admitted_at_utc_ms: now,
            });
            state.epoch = next_epoch;
            return Ok(slot);
        }
    }
    /// An arbitrary durable reservation error is not evidence of NotSent.
    pub(crate) fn block(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.blocked = true;
        }
    }
    pub(crate) fn bind(&self, slot: usize, attempt: &AttemptRef) -> Result<()> {
        let mut state = self.lock()?;
        let next_epoch = state.epoch.checked_add(1).ok_or_else(overflow)?;
        let entry = state
            .entries
            .get_mut(slot)
            .ok_or_else(|| error("invocation admission slot missing"))?;
        if entry.run != attempt.run_id || entry.attempt.is_some() {
            state.blocked = true;
            return Err(error("invocation attempt binding differs"));
        }
        entry.attempt = Some(attempt.clone());
        state.epoch = next_epoch;
        Ok(())
    }
    fn rate_check(&self, state: &State, proposed: &Allowance, now: i64) -> Result<()> {
        let mut requests = proposed.requests;
        let mut tokens = token_units(proposed)?;
        for entry in &state.entries {
            // Match the ledger's Reserved-event convention. Proven NotSent
            // releases lifetime/concurrency holds but preserves rate history.
            if entry.admitted_at_utc_ms > now.saturating_sub(60_000) {
                requests = requests
                    .checked_add(entry.allowance.requests)
                    .ok_or_else(overflow)?;
                tokens = tokens
                    .checked_add(token_units(&entry.allowance)?)
                    .ok_or_else(overflow)?;
            }
        }
        if self
            .limits
            .requests_per_minute
            .is_some_and(|n| requests > u64::from(n))
        {
            return Err(error("invocation requests-per-minute ceiling reached"));
        }
        if self.limits.tokens_per_minute.is_some_and(|n| tokens > n) {
            return Err(error("invocation tokens-per-minute ceiling reached"));
        }
        Ok(())
    }
}
fn token_units(allowance: &Allowance) -> Result<u64> {
    allowance
        .billable_units
        .iter()
        .filter(|(class, _)| {
            matches!(
                class,
                BillableClass::Input
                    | BillableClass::CachedInput
                    | BillableClass::Output
                    | BillableClass::Reasoning
            )
        })
        .try_fold(0u64, |sum, (_, count)| {
            sum.checked_add(*count).ok_or_else(overflow)
        })
}
