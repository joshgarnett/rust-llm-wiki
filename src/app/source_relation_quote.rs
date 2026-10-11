//! Complete, bounded proposed-run quotes. These data never grant send authority.
use super::remote::RemoteRuntime;
use crate::{
    domain::*,
    jobs::{budgets, *},
    providers::generation_task::{GenerationTaskPlan, prepared_bound},
};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    io::{self, Write},
    time::Instant,
};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_OWNERS: usize = 4096;

#[derive(Debug, Serialize)]
pub(crate) struct SourceRelationQuoteEntry {
    pub source_id: RecordId,
    pub task_key: Blake3Hash,
    pub input_descriptor: BoundedPayloadRef,
    pub bound: AttemptBound,
    pub allowance: Allowance,
}
#[derive(Debug, Serialize)]
pub(crate) struct SourceRelationQuote {
    pub manifest_hash: Blake3Hash,
    pub quoted_at_utc_ms: i64,
    pub deadline_utc_ms: i64,
    pub proposed_lifetime_limits: LifetimeLimits,
    pub entries: Vec<SourceRelationQuoteEntry>,
    pub aggregate_worst_case: Allowance,
    pub aggregate_fits_lifetime_limits: bool,
    pub complete_acquisition_guaranteed: bool,
    pub bound_identity_stage: &'static str,
}
pub(crate) struct QuoteBuilder {
    entries: Vec<SourceRelationQuoteEntry>,
    owners: BTreeSet<RecordId>,
    tasks: BTreeSet<Blake3Hash>,
    bytes: usize,
    last: ClockReading,
    deadline: Instant,
    utc_deadline: i64,
    created: i64,
    limits: LifetimeLimits,
    failed: bool,
    config_fingerprint: Blake3Hash,
    profile_fingerprint: Blake3Hash,
}
fn failure(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn observe(
    runtime: &RemoteRuntime,
    deadline: Instant,
    last: Option<ClockReading>,
) -> Result<ClockReading> {
    if runtime.job_options.cancel.is_cancelled() {
        return Err(WikiError::new(
            ErrorCode::Cancelled,
            "relation quote cancelled",
        ));
    }
    let now = runtime.job_options.clock.read()?;
    validate_clock(now, runtime.created_at_utc_ms, last)?;
    if Instant::now() >= deadline || now.utc_ms >= runtime.deadline_utc_ms {
        return Err(failure("relation quote deadline expired"));
    }
    Ok(now)
}
fn validate_clock(now: ClockReading, created: i64, last: Option<ClockReading>) -> Result<()> {
    if now.utc_ms < 0
        || now.utc_ms < created
        || last.is_some_and(|p| now.utc_ms < p.utc_ms || now.monotonic_ms < p.monotonic_ms)
    {
        return Err(failure("relation quote clock regressed"));
    }
    Ok(())
}
impl QuoteBuilder {
    pub(crate) fn new(runtime: &RemoteRuntime, deadline: Instant) -> Result<Self> {
        budgets::validate_limits(&runtime.limits)?;
        let last = observe(runtime, deadline, None)?;
        Ok(Self {
            entries: vec![],
            owners: BTreeSet::new(),
            tasks: BTreeSet::new(),
            bytes: 0,
            last,
            deadline,
            utc_deadline: runtime.deadline_utc_ms,
            created: runtime.created_at_utc_ms,
            limits: runtime.limits.clone(),
            failed: false,
            config_fingerprint: runtime.service.summary().config_fingerprint,
            profile_fingerprint: runtime.service.summary().profile_fingerprint,
        })
    }
    fn check(&mut self, runtime: &RemoteRuntime, deadline: Instant) -> Result<ClockReading> {
        if self.failed
            || deadline != self.deadline
            || runtime.deadline_utc_ms != self.utc_deadline
            || runtime.created_at_utc_ms != self.created
            || runtime.limits != self.limits
            || runtime.service.summary().config_fingerprint != self.config_fingerprint
            || runtime.service.summary().profile_fingerprint != self.profile_fingerprint
        {
            return Err(failure("relation quote failed or runtime limits changed"));
        }
        let now = observe(runtime, deadline, Some(self.last))?;
        self.last = now;
        Ok(now)
    }
    pub(crate) fn push(
        &mut self,
        source_id: &RecordId,
        plan: &GenerationTaskPlan,
        runtime: &RemoteRuntime,
        deadline: Instant,
    ) -> Result<()> {
        let result = self.push_inner(source_id, plan, runtime, deadline);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn push_inner(
        &mut self,
        source_id: &RecordId,
        plan: &GenerationTaskPlan,
        runtime: &RemoteRuntime,
        deadline: Instant,
    ) -> Result<()> {
        self.check(runtime, deadline)?;
        if self.entries.len() >= MAX_OWNERS
            || self.owners.contains(source_id)
            || self.tasks.contains(&plan.task.key)
        {
            return Err(failure(
                "relation quote owner ceiling or duplicate identity",
            ));
        }
        let bound = prepared_bound(&runtime.service, plan)?;
        let now = self.check(runtime, deadline)?;
        let allowance = finite_quote(&bound, &self.limits, now.utc_ms, self.utc_deadline)?;
        let entry = SourceRelationQuoteEntry {
            source_id: source_id.clone(),
            task_key: plan.task.key.clone(),
            input_descriptor: plan.task.input.clone(),
            bound,
            allowance,
        };
        let size = bounded_size(&entry, MAX_BYTES - self.bytes, deadline)?;
        self.bytes = self
            .bytes
            .checked_add(size)
            .ok_or_else(|| failure("relation quote size overflow"))?;
        self.check(runtime, deadline)?;
        self.owners.insert(source_id.clone());
        self.tasks.insert(plan.task.key.clone());
        self.entries.push(entry);
        Ok(())
    }
    pub(crate) fn finish(
        mut self,
        runtime: &RemoteRuntime,
        manifest_hash: Blake3Hash,
        deadline: Instant,
    ) -> Result<SourceRelationQuote> {
        let now = self.check(runtime, deadline)?;
        if self.entries.is_empty() {
            return Err(failure("relation quote requires at least one owner"));
        }
        let mut aggregate = budgets::zero();
        for entry in &mut self.entries {
            if Instant::now() >= deadline {
                return Err(failure("relation quote deadline expired"));
            }
            entry.allowance =
                finite_quote(&entry.bound, &self.limits, now.utc_ms, self.utc_deadline)?;
            add_finite(&mut aggregate, &entry.allowance)?;
        }
        let fits =
            budgets::fits(&budgets::zero(), &budgets::zero(), &aggregate, &self.limits).is_ok();
        let mut quote = SourceRelationQuote {
            manifest_hash,
            quoted_at_utc_ms: now.utc_ms,
            deadline_utc_ms: self.utc_deadline,
            proposed_lifetime_limits: self.limits.clone(),
            entries: self.entries,
            aggregate_worst_case: aggregate,
            aggregate_fits_lifetime_limits: fits,
            complete_acquisition_guaranteed: false,
            bound_identity_stage: "prepared_wire",
        };
        bounded_size(&quote, MAX_BYTES, deadline)?;
        // Recheck expiry/regression after serialization, without exposing partial output.
        let closing = observe(runtime, deadline, Some(now))?;
        for entry in &quote.entries {
            if Instant::now() >= deadline {
                return Err(failure("relation quote deadline expired"));
            }
            finite_quote(
                &entry.bound,
                &self.limits,
                closing.utc_ms,
                self.utc_deadline,
            )?;
        }
        quote.quoted_at_utc_ms = closing.utc_ms;
        bounded_size(&quote, MAX_BYTES, deadline)?;
        if runtime.job_options.cancel.is_cancelled() {
            return Err(WikiError::new(
                ErrorCode::Cancelled,
                "relation quote cancelled",
            ));
        }
        Ok(quote)
    }
}
fn finite_quote(
    bound: &AttemptBound,
    limits: &LifetimeLimits,
    now: i64,
    deadline: i64,
) -> Result<Allowance> {
    let allowance = budgets::quote_bound(bound, limits, now, deadline)?;
    if allowance.cost.is_none() || allowance.billable_units.len() != bound.applicable_classes.len()
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "relation quote requires finite cost and proven bounds for every billable class",
        ));
    }
    budgets::fits(&budgets::zero(), &budgets::zero(), &allowance, limits)?;
    Ok(allowance)
}
fn add_finite(total: &mut Allowance, next: &Allowance) -> Result<()> {
    if next.cost.is_none()
        || total
            .cost
            .as_ref()
            .zip(next.cost.as_ref())
            .is_some_and(|(a, b)| a.currency() != b.currency())
    {
        return Err(failure(
            "relation quote aggregate requires one finite currency",
        ));
    }
    budgets::add(total, next)
}
struct SizeWriter {
    size: usize,
    cap: usize,
    deadline: Instant,
}
impl Write for SizeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let size = self
            .size
            .checked_add(bytes.len())
            .filter(|n| *n <= self.cap);
        if Instant::now() >= self.deadline || size.is_none() {
            return Err(io::Error::other(
                "relation quote output ceiling or deadline exceeded",
            ));
        }
        self.size = size.unwrap();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn bounded_size(value: &impl Serialize, cap: usize, deadline: Instant) -> Result<usize> {
    let mut writer = SizeWriter {
        size: 0,
        cap,
        deadline,
    };
    serde_json::to_writer_pretty(&mut writer, value)
        .map_err(|_| failure("relation quote output ceiling or deadline exceeded"))?;
    Ok(writer.size)
}

/// Count the complete envelope before presentation, reserving room for the
/// JSONL event wrapper. Pretty JSON also bounds compact JSON and human data.
pub(crate) fn check_response_size(value: &impl Serialize, deadline: Instant) -> Result<()> {
    bounded_size(value, 64 * 1024 * 1024 - 1024, deadline).map(|_| ())
}
#[cfg(test)]
#[path = "source_relation_quote_tests.rs"]
mod tests;
