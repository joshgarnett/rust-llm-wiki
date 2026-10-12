//! Finite automatic preparation policy. The caller supplies already authorized
//! service/runtime bindings; this module never infers consent from preferences.
use super::*;
use crate::catalog::scan::{ReadScanBudget, ReadScanLimits};
use std::{cell::RefCell, rc::Rc, time::Instant};

#[derive(Debug, Clone)]
pub enum PreparationTrigger {
    Setup,
    AfterWrite { owners: Vec<VaultRelativePath> },
    Lookup { query: String },
}
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PreparationLimits {
    pub deadline_ms: u64,
    pub requests: u64,
    pub inputs: usize,
    pub input_bytes: usize,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub owners: usize,
    pub inventory_pages: usize,
}
impl PreparationLimits {
    pub fn for_trigger(trigger: &PreparationTrigger) -> Self {
        let (deadline_ms, requests, inputs, input_bytes, request_bytes, response_bytes) =
            match trigger {
                PreparationTrigger::Setup => (
                    60_000,
                    8,
                    128,
                    1024 * 1024,
                    2 * 1024 * 1024,
                    64 * 1024 * 1024,
                ),
                PreparationTrigger::AfterWrite { .. } => {
                    (15_000, 2, 32, 256 * 1024, 512 * 1024, 16 * 1024 * 1024)
                }
                PreparationTrigger::Lookup { .. } => {
                    (5_000, 2, 16, 96 * 1024, 128 * 1024, 16 * 1024 * 1024)
                }
            };
        Self {
            deadline_ms,
            requests,
            inputs,
            input_bytes,
            request_bytes,
            response_bytes,
            owners: 128,
            inventory_pages: 4,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticPreparationState {
    Ready,
    Partial,
    Pending,
    Skipped,
    Blocked,
    Failed,
}
#[derive(Debug, Clone, Serialize)]
pub struct AutomaticRunAccounting {
    /// Durable lifetime totals for this Run, never the current command's cost.
    pub budget: Option<BudgetInspection>,
    pub completeness: Option<AccountingCompleteness>,
    /// The last refresh failed; an older retained budget is not a final total.
    pub unavailable: Option<WikiError>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct AutomaticObservations {
    /// Transport activity for this public invocation, including failed queries.
    pub network_used: bool,
    /// Whole invocation elapsed time, including local completion after admission stops.
    pub elapsed_ms: u64,
    /// Bootstrap/receipt transaction entries, including failed entries and recovery.
    pub canonical_transactions: u64,
    pub canonical_elapsed_ms: u64,
    /// Original canonical accounting verification, outside discovery byte caps.
    pub accounting_verification_elapsed_ms: u64,
    pub admitted_inputs: usize,
    pub admitted_input_bytes: usize,
    pub generated_inputs: usize,
    pub touched_run_ids: BTreeSet<RecordId>,
    pub blocked_run_ids: BTreeSet<RecordId>,
    pub run_accounting: BTreeMap<RecordId, AutomaticRunAccounting>,
    /// False means counts describe selected work, never the whole corpus.
    pub counts_complete: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct AutomaticPreparationOutcome {
    pub state: AutomaticPreparationState,
    pub report: Option<EmbeddingReport>,
    pub error: Option<WikiError>,
    pub observations: AutomaticObservations,
}
pub(super) struct AutomaticPolicy {
    pub(super) trigger: PreparationTrigger,
    pub(super) limits: PreparationLimits,
    pub(super) deadline: Instant,
    started: Instant,
    pub(super) local: std::sync::Arc<ReadScanBudget>,
    pub(super) vectors: Rc<retrieval::vectors::VectorReadBudget>,
    query_reserve: Option<Allowance>,
    query_input_bytes: usize,
    query_input_hash: Option<Blake3Hash>,
    progress: RefCell<AutomaticProgress>,
}
#[derive(Clone, Default)]
struct AutomaticProgress {
    accounting_checked: bool,
    prior_runs: BTreeSet<RecordId>,
    observations: AutomaticObservations,
    corpus_allowance: Option<Allowance>,
    admitted_hashes: BTreeSet<Blake3Hash>,
}
fn exhausted(message: &str) -> WikiError {
    let mut error = WikiError::new(ErrorCode::BudgetExceeded, message);
    error.details = serde_json::json!({"budget_scope":"invocation"});
    error
}
impl AutomaticPolicy {
    pub(super) fn remaining_ms(&self) -> Result<u64> {
        self.local.check()?;
        u64::try_from(
            self.deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .ok()
        .filter(|ms| *ms > 0)
        .ok_or_else(|| exhausted("automatic preparation deadline reached"))
    }
    pub(super) fn verification_budget(&self) -> Result<VerificationBudget> {
        let (max_bytes, max_files, max_entries) = self.local.remaining();
        Ok(VerificationBudget {
            max_bytes,
            max_files,
            max_entries,
            max_elapsed_ms: self.remaining_ms()?.min(30_000),
        })
    }
}
impl<'a> EmbeddingRuntime<'a> {
    /// Root must first establish explicit automatic consent and writable
    /// accounting. Returned clones share one frozen remote and local allowance.
    pub fn for_automatic_operation(
        &self,
        trigger: PreparationTrigger,
        settings: &EmbeddingSettings,
    ) -> Result<Self> {
        if self.invocation.is_some() || self.automatic.is_some() {
            return Err(WikiError::invalid(
                "automatic operation must be scoped once before dispatch",
            ));
        }
        if self.job_options.policy.retry_uncertain {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "automatic preparation does not retry uncertain requests; inspect the retained Run explicitly",
            ));
        }
        let started = Instant::now();
        let network_start = self.dispatcher.network_observations();
        settings.validate()?;
        let caps = PreparationLimits::for_trigger(&trigger);
        let mut scoped = self.clone();
        scoped.limits.requests = scoped.limits.requests.min(caps.requests);
        scoped.limits.concurrency = scoped.limits.concurrency.min(1);
        scoped.limits.attempts_per_task = scoped.limits.attempts_per_task.min(1);
        scoped.limits.request_bytes = Some(
            scoped
                .limits
                .request_bytes
                .unwrap_or(caps.request_bytes)
                .min(caps.request_bytes),
        );
        scoped.limits.response_bytes = Some(
            scoped
                .limits
                .response_bytes
                .unwrap_or(caps.response_bytes)
                .min(caps.response_bytes),
        );
        let now = self.job_options.clock.read()?.utc_ms;
        let old_deadline = self
            .created_at_utc_ms
            .checked_add(
                i64::try_from(self.deadline_ms)
                    .map_err(|_| WikiError::invalid("embedding deadline overflow"))?,
            )
            .ok_or_else(|| WikiError::invalid("embedding deadline overflow"))?;
        let remaining = old_deadline
            .checked_sub(now)
            .and_then(|n| u64::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| exhausted("automatic operation deadline expired"))?;
        scoped.created_at_utc_ms = now;
        scoped.deadline_ms = remaining.min(caps.deadline_ms);
        scoped.deadline_utc_ms = now
            .checked_add(scoped.deadline_ms as i64)
            .ok_or_else(|| WikiError::invalid("embedding deadline overflow"))?;
        // Retained Runs keep their own limits. Automatic bounds compose through
        // the invocation allowance rather than amending their immutable spec.
        scoped.requested_limits = Some(super::super::remote::RequestedJobLimits {
            limits: scoped.limits.clone(),
            specified: BTreeSet::new(),
            deadline_ms: None,
        });
        let mut query_reserve = None;
        let mut query_input_bytes = 0;
        let mut query_input_hash = None;
        if let PreparationTrigger::Lookup { query } = &trigger {
            let spec = SpaceSpec::from_service(self.service, settings.clone())?;
            let input = spec.query(query)?;
            query_input_bytes = input.utf8.len();
            query_input_hash = Some(input.input_hash.clone());
            if query_input_bytes > caps.input_bytes {
                return Err(exhausted(
                    "search question exceeds automatic input byte allowance",
                ));
            }
            let mut bound = crate::providers::preview_embedding_request(
                self.service,
                &[input],
                spec.dimensions,
                &spec.id()?,
            )?;
            bound.timeout_ms = bound.timeout_ms.min(scoped.deadline_ms);
            bound.bounds_fingerprint = jobs::budgets::bound_fingerprint(&bound)?;
            let quoted =
                jobs::budgets::quote_bound(&bound, &scoped.limits, now, scoped.deadline_utc_ms)?;
            jobs::budgets::fits(
                &jobs::budgets::zero(),
                &jobs::budgets::zero(),
                &quoted,
                &scoped.limits,
            )?;
            query_reserve = Some(quoted);
        }
        let deadline = started + Duration::from_millis(scoped.deadline_ms);
        scoped.automatic = Some(Rc::new(AutomaticPolicy {
            trigger,
            limits: caps,
            deadline,
            started,
            vectors: retrieval::vectors::VectorReadBudget::new_automatic(deadline)?,
            local: std::sync::Arc::new(ReadScanBudget::with_deadline(
                ReadScanLimits {
                    max_elapsed_ms: scoped.deadline_ms,
                    ..ReadScanLimits::default()
                },
                deadline,
            )?),
            query_reserve,
            query_input_bytes,
            query_input_hash,
            progress: RefCell::new(AutomaticProgress::default()),
        }));
        scoped.network_start = Some(network_start);
        scoped.invocation = Some(std::sync::Arc::new(
            crate::providers::invocation_budget::InvocationBudget::new_automatic_with_io(
                scoped.limits.clone(),
                scoped.created_at_utc_ms,
                scoped.deadline_utc_ms,
                deadline,
                scoped
                    .automatic
                    .as_ref()
                    .expect("automatic policy")
                    .local
                    .clone(),
            )?,
        ));
        Ok(scoped)
    }
    /// Call after preparation AND query/fallback to retain failed-dispatch facts.
    pub fn automatic_observations(&self) -> AutomaticObservations {
        let mut out = self
            .automatic
            .as_ref()
            .map(|a| a.progress.borrow().observations.clone())
            .unwrap_or_default();
        out.network_used = self.network_used();
        if let Some(budget) = &self.invocation {
            let (count, elapsed) = budget.canonical_observations();
            out.canonical_transactions = out.canonical_transactions.saturating_add(count);
            out.canonical_elapsed_ms = out.canonical_elapsed_ms.saturating_add(elapsed);
        }
        if let Some(auto) = &self.automatic {
            out.elapsed_ms = u64::try_from(auto.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        }
        out
    }
    pub(super) fn automatic_touch(&self, run: &RecordId) {
        if let Some(auto) = &self.automatic {
            let mut progress = auto.progress.borrow_mut();
            progress.observations.touched_run_ids.insert(run.clone());
            progress.prior_runs.insert(run.clone());
        }
    }
    pub(super) fn automatic_prior_accounting(&self) -> Result<PriorAccounting> {
        let auto = self
            .automatic
            .as_ref()
            .ok_or_else(|| WikiError::invalid("automatic allowance missing"))?;
        let progress = auto.progress.borrow();
        if !progress.accounting_checked {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "automatic accounting discovery incomplete",
            ));
        }
        Ok(if progress.prior_runs.is_empty() {
            PriorAccounting::None
        } else {
            PriorAccounting::Unknown { prior_run_ids: progress.prior_runs.iter().cloned().collect(),
                reason: "retained terminal histories preserve unavailable costs; automatic allowance covers this invocation only".into() }
        })
    }
    pub(super) fn automatic_block(&self, run: &RecordId) {
        if let Some(auto) = &self.automatic {
            auto.progress
                .borrow_mut()
                .observations
                .blocked_run_ids
                .insert(run.clone());
        }
    }
}

impl EmbeddingRuntime<'_> {
    /// Preflight an exact batch before credentials. The durable dispatcher still
    /// performs all real admission; these counters only impose stricter phase caps.
    pub(super) fn automatic_admit(
        &self,
        spec: &SpaceSpec,
        inputs: &[EmbeddingInput],
        corpus: bool,
    ) -> Result<()> {
        let Some(auto) = &self.automatic else {
            return Ok(());
        };
        if !auto
            .progress
            .borrow()
            .observations
            .blocked_run_ids
            .is_empty()
        {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "retained accounting blocks automatic dispatch",
            ));
        }
        let remaining_ms = auto.remaining_ms()?;
        let mut bound = crate::providers::preview_embedding_request(
            self.service,
            inputs,
            spec.dimensions,
            &spec.id()?,
        )?;
        // Leave admission/helper time outside the fixed transport allowance.
        bound.timeout_ms = bound.timeout_ms.min((remaining_ms / 2).max(1));
        bound.bounds_fingerprint = jobs::budgets::bound_fingerprint(&bound)?;
        let now = self.job_options.clock.read()?.utc_ms;
        let quote = jobs::budgets::quote_bound(&bound, &self.limits, now, self.deadline_utc_ms)?;
        let mut state = auto.progress.borrow_mut();
        let fresh: Vec<_> = inputs
            .iter()
            .filter(|input| !state.admitted_hashes.contains(&input.input_hash))
            .collect();
        let fresh_bytes = fresh.iter().try_fold(0usize, |sum, input| {
            sum.checked_add(input.utf8.len())
                .ok_or_else(|| exhausted("automatic input byte overflow"))
        })?;
        let reserve = if corpus {
            auto.query_reserve.as_ref()
        } else {
            None
        };
        let input_cap = auto
            .limits
            .inputs
            .saturating_sub(usize::from(reserve.is_some()));
        let byte_cap = auto
            .limits
            .input_bytes
            .saturating_sub(if reserve.is_some() {
                auto.query_input_bytes
            } else {
                0
            });
        if state
            .observations
            .admitted_inputs
            .saturating_add(fresh.len())
            > input_cap
            || state
                .observations
                .admitted_input_bytes
                .saturating_add(fresh_bytes)
                > byte_cap
        {
            return Err(exhausted("automatic rendered input allowance reached"));
        }
        if corpus {
            let accumulated = state
                .corpus_allowance
                .clone()
                .unwrap_or_else(jobs::budgets::zero);
            let reserved = reserve.cloned().unwrap_or_else(jobs::budgets::zero);
            jobs::budgets::fits(&accumulated, &reserved, &quote, &self.limits)?;
            let mut next = accumulated;
            jobs::budgets::add(&mut next, &quote)?;
            state.corpus_allowance = Some(next);
        }
        state.observations.admitted_inputs += fresh.len();
        state.observations.admitted_input_bytes += fresh_bytes;
        for input in inputs {
            state.admitted_hashes.insert(input.input_hash.clone());
        }
        Ok(())
    }
    pub(super) fn automatic_input_prefix(&self, inputs: &[EmbeddingInput]) -> usize {
        let Some(auto) = &self.automatic else {
            return inputs.len();
        };
        let state = auto.progress.borrow();
        let reserve_query = auto.query_reserve.is_some();
        let count_cap = auto
            .limits
            .inputs
            .saturating_sub(usize::from(reserve_query))
            .saturating_sub(state.observations.admitted_inputs);
        let byte_cap = auto
            .limits
            .input_bytes
            .saturating_sub(if reserve_query {
                auto.query_input_bytes
            } else {
                0
            })
            .saturating_sub(state.observations.admitted_input_bytes);
        let mut bytes = 0usize;
        let mut count = 0usize;
        for input in inputs.iter().take(count_cap) {
            let Some(next) = bytes
                .checked_add(input.utf8.len())
                .filter(|next| *next <= byte_cap)
            else {
                break;
            };
            bytes = next;
            count += 1;
        }
        count
    }
    /// Select only a prefix that fits before creating immutable Run tasks. A
    /// bounded command never leaves unattempted surplus tasks requiring resume.
    pub(super) fn automatic_limit_batches(
        &self,
        spec: &SpaceSpec,
        batches: Vec<Vec<EmbeddingInput>>,
    ) -> Result<Vec<Vec<EmbeddingInput>>> {
        let Some(auto) = &self.automatic else {
            return Ok(batches);
        };
        let retained = auto.progress.borrow().clone();
        let result = (|| {
            let mut selected = Vec::new();
            'batches: for mut batch in batches {
                while !batch.is_empty() {
                    // Subsets retain their original supplier guards. Shrink the
                    // trailing prefix until exact serialization/quote fits.
                    let mut prefix = batch.clone();
                    loop {
                        match self.automatic_admit(spec, &prefix, true) {
                            Ok(()) => {
                                batch.drain(..prefix.len());
                                selected.push(prefix);
                                break;
                            }
                            Err(error)
                                if matches!(
                                    error.code,
                                    ErrorCode::BudgetExceeded | ErrorCode::CapabilityUnavailable
                                ) =>
                            {
                                prefix.pop();
                                if prefix.is_empty() {
                                    break 'batches;
                                }
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
            Ok(selected)
        })();
        *auto.progress.borrow_mut() = retained;
        result
    }
    pub(super) fn automatic_check_start(&self) -> Result<()> {
        if let Some(auto) = &self.automatic {
            auto.remaining_ms()?;
            if self.job_options.clock.read()?.utc_ms >= self.deadline_utc_ms {
                return Err(exhausted("automatic operation admission deadline reached"));
            }
        }
        Ok(())
    }
    pub(super) fn automatic_enter_canonical(&self) -> Result<AutomaticCanonicalTiming> {
        self.automatic_check_start()?;
        if let Some(auto) = &self.automatic {
            let mut state = auto.progress.borrow_mut();
            state.observations.canonical_transactions =
                state.observations.canonical_transactions.saturating_add(1);
        }
        Ok(AutomaticCanonicalTiming {
            automatic: self.automatic.clone(),
            started: Instant::now(),
        })
    }
    pub(super) fn automatic_generated(&self, count: usize) {
        if let Some(auto) = &self.automatic {
            let mut state = auto.progress.borrow_mut();
            state.observations.generated_inputs =
                state.observations.generated_inputs.saturating_add(count);
        }
    }
}

pub(super) struct AutomaticCanonicalTiming {
    automatic: Option<Rc<AutomaticPolicy>>,
    started: Instant,
}
impl Drop for AutomaticCanonicalTiming {
    fn drop(&mut self) {
        if let Some(auto) = &self.automatic {
            let elapsed = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let mut state = auto.progress.borrow_mut();
            state.observations.canonical_elapsed_ms = state
                .observations
                .canonical_elapsed_ms
                .saturating_add(elapsed);
        }
    }
}

fn temporary_vector_contention(error: &WikiError) -> bool {
    if error.code != ErrorCode::IndexCorrupt {
        return false;
    }
    let mut details = &error.details;
    for _ in 0..8 {
        if details
            .get("sqlite_failure")
            .and_then(serde_json::Value::as_str)
            == Some("temporary_contention")
        {
            return true;
        }
        let Some(cause) = details.get("cause") else {
            break;
        };
        details = cause;
    }
    false
}

impl AutomaticPreparationOutcome {
    /// A failed optional phase retains observed progress and transport facts.
    /// Missing coverage stays unknown rather than becoming fabricated zeros.
    pub fn from_result(result: Result<EmbeddingReport>, runtime: &EmbeddingRuntime<'_>) -> Self {
        let observations = runtime.automatic_observations();
        match result {
            Ok(mut report) => {
                report.network_used = observations.network_used;
                let state = if report.published {
                    AutomaticPreparationState::Ready
                } else if report.generated_inputs > 0 || report.reused_inputs > 0 {
                    AutomaticPreparationState::Partial
                } else {
                    AutomaticPreparationState::Pending
                };
                Self {
                    state,
                    report: Some(report),
                    error: None,
                    observations,
                }
            }
            Err(mut error) => {
                let contention = temporary_vector_contention(&error);
                if contention {
                    error.details = serde_json::json!({
                        "automatic_preparation":"pending", "reason":"vector_cache_busy", "cause":error.details
                    });
                }
                let state = match error.code {
                    ErrorCode::IndexCorrupt if contention => AutomaticPreparationState::Pending,
                    ErrorCode::BudgetExceeded => {
                        if observations.generated_inputs > 0 {
                            AutomaticPreparationState::Partial
                        } else {
                            AutomaticPreparationState::Pending
                        }
                    }
                    ErrorCode::OfflineUnavailable | ErrorCode::CapabilityUnavailable => {
                        AutomaticPreparationState::Skipped
                    }
                    ErrorCode::RecoveryRequired | ErrorCode::ProfileUntrusted => {
                        AutomaticPreparationState::Blocked
                    }
                    _ => AutomaticPreparationState::Failed,
                };
                Self {
                    state,
                    report: None,
                    error: Some(error),
                    observations,
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "automatic_preparation_tests.rs"]
mod tests;

impl OfflineApp {
    /// Optional automatic work returns a secondary outcome. Local save success
    /// belongs to the caller and is never rolled back by this phase.
    pub fn prepare_automatic_embeddings(
        &self,
        settings: &EmbeddingSettings,
        runtime: &EmbeddingRuntime<'_>,
    ) -> AutomaticPreparationOutcome {
        let result = self.prepare_automatic_inner(settings, runtime);
        self.refresh_automatic_accounting(runtime);
        AutomaticPreparationOutcome::from_result(result, runtime)
    }
    fn prepare_automatic_inner(
        &self,
        settings: &EmbeddingSettings,
        runtime: &EmbeddingRuntime<'_>,
    ) -> Result<EmbeddingReport> {
        let auto = runtime.automatic.as_ref().ok_or_else(|| {
            WikiError::invalid(
                "automatic preparation requires a previously scoped authorized runtime",
            )
        })?;
        self.embedding_policy_consistent(Some(runtime))?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let normalized = catalog.operation_state_bounded(&auto.local)?.is_some();
        self.remote_gate(runtime)?;
        if normalized {
            return self.embeddings_sync_indexed(settings, Some(runtime));
        }
        let spec = SpaceSpec::from_service(runtime.service, settings.clone())?;
        let space = spec.id()?;
        let writer = self.automatic_writer(runtime)?;
        let reader = catalog.sync_bounded(&writer, &auto.local)?;
        let phase = Self::embedding_phase_for(Some(runtime))?;
        let mut store = self.embedding_store_for(Some(runtime), Some(&writer))?;
        store.bind_read_budget(&phase)?;
        store.prepare_space(&spec)?;
        let old_active = store.active()?.map(|state| state.id);
        let changed = match &auto.trigger {
            PreparationTrigger::AfterWrite { owners } => Some(
                owners
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .take(auto.limits.owners)
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        };
        let mut units = Vec::new();
        let mut missing = Vec::new();
        let mut owners = BTreeSet::new();
        let mut hashes = BTreeSet::new();
        let mut reused = 0usize;
        let mut selected_complete = true;
        for unit in render::corpus_selected_iter(&reader, settings, changed.as_deref())? {
            auto.remaining_ms()?;
            let unit = unit?;
            let cached = store.vector(&space, &unit.input_hash)?.is_some();
            // Already cached owners do not prevent the next invocation reaching
            // later pending owners on legacy layouts, which have no cursor.
            if !cached && !owners.contains(&unit.owner) && owners.len() >= auto.limits.owners {
                selected_complete = false;
                break;
            }
            if !cached {
                owners.insert(unit.owner.clone());
            }
            if hashes.insert(unit.input_hash.clone()) {
                if cached {
                    reused += 1;
                } else {
                    missing.push(unit.input());
                }
            }
            units.push(unit);
        }
        let mut report = EmbeddingReport {
            space: Some(space.clone()),
            active_space: old_active,
            settings: Some(settings.clone()),
            coverage: Coverage::default(),
            generated_inputs: 0,
            reused_inputs: reused,
            published: false,
            dry_run: false,
            network_used: false,
            run_id: None,
            warnings: Vec::new(),
        };
        drop(store);
        drop(reader);
        drop(writer);
        let recovered = self.recover_embedding_jobs(&spec, runtime)?;
        report.run_id = recovered.run_id;
        report.warnings.extend(recovered.warnings);
        let phase = Self::embedding_phase_for(Some(runtime))?;
        let available = self.embedding_snapshot_store_for(Some(runtime), &phase)?;
        let mut pending = Vec::new();
        for input in missing {
            if available.vector(&space, &input.input_hash)?.is_none() {
                pending.push(input);
            }
        }
        missing = pending;
        drop(available);
        if !missing.is_empty() {
            missing.truncate(runtime.automatic_input_prefix(&missing));
            let batches = runtime
                .automatic_limit_batches(&spec, self.embedding_batches(runtime, &missing)?)?;
            if !batches.is_empty() {
                let wanted: BTreeSet<_> = batches
                    .iter()
                    .flatten()
                    .map(|input| &input.input_hash)
                    .collect();
                let mut suppliers = BTreeMap::new();
                for unit in units
                    .iter()
                    .filter(|unit| wanted.contains(&unit.input_hash))
                {
                    suppliers.entry(&unit.input_hash).or_insert(&unit.owner);
                }
                let paid_units: Vec<_> = units
                    .iter()
                    .filter(|unit| {
                        suppliers
                            .get(&unit.input_hash)
                            .is_some_and(|owner| **owner == unit.owner)
                    })
                    .cloned()
                    .collect();
                let (ledger, tasks) = self.embedding_job(
                    &spec,
                    &batches,
                    &paid_units,
                    runtime,
                    "embeddings_sync",
                    false,
                )?;
                report.run_id = Some(ledger.inspect()?.spec.run_id.clone());
                for task in tasks {
                    self.dispatch_embedding_task(
                        &ledger,
                        &task,
                        &spec,
                        &paid_units,
                        runtime,
                        TaskMaterialization {
                            probe: false,
                            corpus: true,
                        },
                    )?;
                }
                if let Some(warning) = self.finish_embedding_job(&ledger)? {
                    report.warnings.push(warning);
                }
            }
        }
        // Receipt bookkeeping changes the canonical publication. Reuse the same
        // bounded production publisher and activate only complete current coverage.
        let writer = self.automatic_writer(runtime)?;
        let current = catalog.sync_bounded(&writer, &auto.local)?;
        let mut current_units = Vec::new();
        for unit in render::corpus_iter(&current, settings)? {
            auto.remaining_ms()?;
            current_units.push(unit?);
        }
        let phase = Self::embedding_phase_for(Some(runtime))?;
        let mut store = self.embedding_store_for(Some(runtime), Some(&writer))?;
        store.bind_read_budget(&phase)?;
        let coverage = store.coverage(&space, &current_units)?;
        let complete = coverage.eligible_units > 0
            && coverage.missing_units == 0
            && store
                .space(&space)?
                .is_some_and(|state| state.actual_dimensions.is_some());
        report.coverage = store.memberships_with_spec_checked(
            &space,
            current.snapshot(),
            &current_units,
            complete,
            if complete { Some(&spec) } else { None },
            || {
                catalog.recheck_read_snapshot(&current, &auto.local)?;
                auto.remaining_ms()?;
                Ok(())
            },
        )?;
        report.published = complete;
        report.active_space = store.active()?.map(|state| state.id);
        report.generated_inputs = runtime.automatic_observations().generated_inputs;
        report.network_used = runtime.network_used();
        auto.progress.borrow_mut().observations.counts_complete =
            selected_complete && changed.is_none();
        if !complete {
            report.warnings.push(
                "Similarity search is still preparing; local search remains available.".into(),
            );
        }
        Ok(report)
    }
    pub(super) fn embedding_writer_for(
        &self,
        runtime: &EmbeddingRuntime<'_>,
    ) -> Result<WriterPermit> {
        if runtime.automatic.is_some() {
            self.automatic_writer(runtime)
        } else {
            self.embedding_writer()
        }
    }
    fn automatic_writer(&self, runtime: &EmbeddingRuntime<'_>) -> Result<WriterPermit> {
        runtime.automatic_check_start()?;
        let auto = runtime
            .automatic
            .as_ref()
            .ok_or_else(|| WikiError::invalid("automatic allowance missing"))?;
        WriterPermit::acquire(
            self.fs.root(),
            Duration::from_millis(auto.remaining_ms()?.min(self.options.lock_timeout_ms)),
        )
    }
}

impl OfflineApp {
    /// Refresh after preparation AND the final query/fallback. Errors remain
    /// observations so accounting inspection cannot undo a saved note.
    pub fn refresh_automatic_accounting(
        &self,
        runtime: &EmbeddingRuntime<'_>,
    ) -> AutomaticObservations {
        if let Some(auto) = &runtime.automatic {
            let runs: Vec<_> = auto
                .progress
                .borrow()
                .observations
                .touched_run_ids
                .iter()
                .cloned()
                .collect();
            for run in runs {
                let result = self
                    .embedding_ledger_for(runtime, run.clone())
                    .and_then(|ledger| self.embedding_accounting_for(runtime, &ledger));
                let mut progress = auto.progress.borrow_mut();
                let observed = progress.observations.run_accounting.entry(run).or_insert(
                    AutomaticRunAccounting {
                        budget: None,
                        completeness: None,
                        unavailable: None,
                    },
                );
                match result {
                    Ok(inspection) => {
                        observed.budget = Some(inspection.budget);
                        observed.completeness = Some(inspection.completeness);
                        observed.unavailable = None;
                    }
                    Err(error) => observed.unavailable = Some(error),
                }
            }
        }
        runtime.automatic_observations()
    }
    pub(super) fn automatic_query_limits(
        runtime: Option<&EmbeddingRuntime<'_>>,
    ) -> Result<crate::catalog::query_types::QueryReadLimits> {
        let mut limits = crate::catalog::query_types::QueryReadLimits::default();
        if let Some(runtime) = runtime {
            runtime.automatic_check_start()?;
        }
        if let Some(auto) = runtime.and_then(|runtime| runtime.automatic.as_ref()) {
            limits.max_elapsed_ms = limits.max_elapsed_ms.min(auto.remaining_ms()?);
        }
        Ok(limits)
    }
    pub(super) fn embedding_normalized_for(&self, runtime: &EmbeddingRuntime<'_>) -> Result<bool> {
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        Ok(match &runtime.automatic {
            Some(auto) => catalog.operation_state_bounded(&auto.local)?,
            None => catalog.operation_state()?,
        }
        .is_some())
    }
    pub(super) fn embedding_store_for(
        &self,
        runtime: Option<&EmbeddingRuntime<'_>>,
        writer: Option<&WriterPermit>,
    ) -> Result<VectorStore> {
        match runtime.and_then(|runtime| runtime.automatic.as_ref()) {
            Some(auto) => VectorStore::open_automatic(&self.fs, writer, &auto.vectors, &auto.local),
            None => VectorStore::open(&self.fs, writer),
        }
    }
    pub(super) fn embedding_snapshot_store_for(
        &self,
        runtime: Option<&EmbeddingRuntime<'_>>,
        phase: &Rc<retrieval::vectors::VectorReadBudget>,
    ) -> Result<VectorStore> {
        match runtime.and_then(|runtime| runtime.automatic.as_ref()) {
            Some(auto) => {
                VectorStore::open_automatic_snapshot(&self.fs, &auto.vectors, &auto.local)
            }
            None => VectorStore::open_bounded_snapshot(&self.fs, phase),
        }
    }
    pub(super) fn embedding_accounting_for(
        &self,
        runtime: &EmbeddingRuntime<'_>,
        ledger: &JobLedger,
    ) -> Result<LedgerInspection> {
        runtime.automatic_check_start()?;
        let started = Instant::now();
        let result = ledger.inspect_accounting_verified();
        if let Some(auto) = &runtime.automatic {
            let mut state = auto.progress.borrow_mut();
            state.observations.accounting_verification_elapsed_ms = state
                .observations
                .accounting_verification_elapsed_ms
                .saturating_add(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        }
        let inspection = result?;
        runtime.automatic_check_start()?;
        Ok(inspection)
    }
    pub(super) fn embedding_ledger_for(
        &self,
        runtime: &EmbeddingRuntime<'_>,
        run: RecordId,
    ) -> Result<JobLedger> {
        JobLedger::new(
            self.fs.clone(),
            self.vault_id.clone(),
            run,
            runtime.job_options.clone(),
        )
    }
    pub(super) fn embedding_read_for(
        &self,
        runtime: &EmbeddingRuntime<'_>,
        path: &VaultRelativePath,
        maximum: usize,
    ) -> Result<Option<Vec<u8>>> {
        match &runtime.automatic {
            Some(auto) => crate::catalog::scan::BoundedReadInput::new(&self.fs, &auto.local)?
                .read_managed(path, maximum),
            None => crate::changes::prepare::read_bounded(&self.fs, path, maximum),
        }
    }

    /// Enumerate retained authority before allowing replacement work. A missing
    /// marker is not an empty history; terminal unknown pricing alone is not a
    /// possibly in-flight request.
    pub(super) fn automatic_accounting_admission(
        &self,
        runtime: &EmbeddingRuntime<'_>,
    ) -> Result<()> {
        let auto = runtime
            .automatic
            .as_ref()
            .ok_or_else(|| WikiError::invalid("automatic allowance missing"))?;
        if auto.progress.borrow().accounting_checked {
            return Ok(());
        }
        let mut runs: BTreeSet<_> = crate::vault::operational::RunStore::discover_existing_bounded(
            &self.fs,
            &self.vault_id,
            4096,
            &auto.local,
        )?
        .into_iter()
        .collect();
        let input = crate::catalog::scan::BoundedReadInput::new(&self.fs, &auto.local)?;
        for path in input.markdown_paths()? {
            if let Some(name) = path
                .as_str()
                .strip_prefix("runs/")
                .and_then(|s| s.strip_suffix("/run.md"))
                && !name.contains('/')
            {
                runs.insert(RecordId::new(name)?);
                if runs.len() > 4096 {
                    return Err(exhausted(
                        "automatic accounting discovery allowance reached",
                    ));
                }
            }
        }
        let directory = input.resolve(&VaultRelativePath::new(".wiki/state/embedding-jobs")?)?;
        let mut marked = BTreeMap::new();
        match std::fs::read_dir(directory) {
            Ok(entries) => {
                for (index, entry) in entries.enumerate() {
                    auto.local.charge_observation(0, 0, 1)?;
                    if index >= 4096 {
                        return Err(exhausted("automatic marker discovery allowance reached"));
                    }
                    let entry = entry.map_err(|_| {
                        fail(
                            ErrorCode::RecoveryRequired,
                            "embedding marker inventory unavailable",
                        )
                    })?;
                    let name = entry.file_name();
                    let name = name
                        .to_str()
                        .ok_or_else(|| WikiError::invalid("embedding marker path unavailable"))?;
                    if !name.ends_with(".json") {
                        continue;
                    }
                    let path =
                        VaultRelativePath::new(format!(".wiki/state/embedding-jobs/{name}"))?;
                    let bytes = self
                        .embedding_read_for(
                            runtime,
                            &path,
                            auto.local
                                .remaining()
                                .0
                                .min(crate::changes::prepare::MAX_PAYLOAD_BYTES),
                        )?
                        .ok_or_else(|| {
                            fail(
                                ErrorCode::RecoveryRequired,
                                "retained embedding marker disappeared",
                            )
                        })?;
                    let marker: RunMarker = serde_json::from_slice(&bytes).map_err(|_| {
                        fail(
                            ErrorCode::RecoveryRequired,
                            "retained embedding marker is corrupt",
                        )
                    })?;
                    runs.insert(marker.run_id.clone());
                    marked.insert(marker.run_id.clone(), marker);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(fail(
                    ErrorCode::RecoveryRequired,
                    "embedding marker inventory unavailable",
                ));
            }
        }
        let service = runtime.service.summary();
        auto.progress.borrow_mut().prior_runs = runs.clone();
        let mut unmarked_pending_run = None;
        for run in runs {
            auto.remaining_ms()?;
            let ledger = self.embedding_ledger_for(runtime, run.clone())?;
            let inspection = self
                .embedding_accounting_for(runtime, &ledger)
                .map_err(|error| {
                    runtime.automatic_block(&run);
                    with_embedding_context(error, serde_json::json!({"run_id":run}))
                })?;
            let embedding = matches!(
                inspection.spec.scope.operation.as_str(),
                "embeddings_sync" | "embeddings_query" | "embeddings_check"
            );
            let same_service = inspection
                .spec
                .scope
                .profile_fingerprints
                .get(&service.profile_id)
                == Some(&service.profile_fingerprint)
                || inspection.attempts.iter().any(|attempt| {
                    attempt.bound.endpoint_fingerprint == service.endpoint_fingerprint
                });
            if !embedding || !same_service {
                continue;
            }
            auto.progress
                .borrow_mut()
                .observations
                .run_accounting
                .insert(
                    run.clone(),
                    AutomaticRunAccounting {
                        budget: Some(inspection.budget.clone()),
                        completeness: Some(inspection.completeness.clone()),
                        unavailable: None,
                    },
                );
            let unresolved = inspection.attempts.iter().any(|attempt| {
                matches!(
                    attempt.phase,
                    AttemptPhase::Reserved | AttemptPhase::DispatchIntent
                )
            });
            let mut unchanged_failure = false;
            for task in inspection
                .tasks
                .values()
                .filter(|task| task.state == TaskState::Failed)
            {
                if let PreparationTrigger::AfterWrite { owners } = &auto.trigger
                    && !task
                        .spec
                        .source_bindings
                        .iter()
                        .any(|guard| owners.contains(&guard.path))
                {
                    continue;
                }
                if task.spec.source_bindings.is_empty() {
                    if inspection.spec.scope.operation == "embeddings_query"
                        && matches!(&auto.trigger, PreparationTrigger::Lookup { .. })
                    {
                        let bytes = self
                            .embedding_read_for(
                                runtime,
                                &task.spec.input.path,
                                auto.local
                                    .remaining()
                                    .0
                                    .min(crate::changes::prepare::MAX_PAYLOAD_BYTES),
                            )?
                            .ok_or_else(|| {
                                fail(
                                    ErrorCode::RecoveryRequired,
                                    "retained failed query descriptor missing",
                                )
                            })?;
                        let remote: RemoteInput = serde_json::from_slice(&bytes).map_err(|_| {
                            fail(
                                ErrorCode::RecoveryRequired,
                                "retained failed query descriptor corrupt",
                            )
                        })?;
                        if bytes.len() as u64 != task.spec.input.byte_len
                            || Blake3Hash::digest(&bytes) != task.spec.input.hash
                            || crate::graph::packet::canonical_json(&remote)? != bytes
                        {
                            runtime.automatic_block(&run);
                            return Err(fail(
                                ErrorCode::RecoveryRequired,
                                "retained failed query descriptor binding differs",
                            ));
                        }
                        if let RemoteOperation::Embed { inputs, .. } = remote.operation {
                            // Exact rendering identity is retained in the input;
                            // substring matching would block unrelated questions.
                            unchanged_failure |= inputs.iter().any(|input| {
                                auto.query_input_hash.as_ref() == Some(&input.input_hash)
                            });
                        }
                    }
                    continue;
                }
                let mut unchanged = true;
                for guard in &task.spec.source_bindings {
                    let bytes = self.embedding_read_for(
                        runtime,
                        &guard.path,
                        auto.local
                            .remaining()
                            .0
                            .min(crate::changes::prepare::MAX_PAYLOAD_BYTES),
                    )?;
                    let state = bytes.map_or(ExpectedState::Absent, |bytes| {
                        ExpectedState::Hash(Blake3Hash::digest(bytes))
                    });
                    unchanged &= state == guard.expected;
                }
                unchanged_failure |= unchanged;
            }
            let unstarted = if let Some(marker) = marked.get(&run) {
                marker.task_keys
                    == inspection
                        .spec
                        .tasks
                        .iter()
                        .map(|task| task.key.clone())
                        .collect::<Vec<_>>()
                    && automatic_unstarted_scope(
                        runtime,
                        &inspection,
                        &marker.operation,
                        &marker.space,
                        &marker.expected_units,
                    )?
                    && ledger.can_close_unstarted_embedding(&inspection.spec)?
            } else {
                false
            };
            let unmarked_pending = !unstarted
                && !matches!(inspection.state, RunState::Completed | RunState::Failed)
                && inspection.attempts.is_empty()
                && inspection
                    .tasks
                    .values()
                    .any(|task| !matches!(task.state, TaskState::Completed | TaskState::Failed));
            if unmarked_pending {
                unmarked_pending_run.get_or_insert_with(|| run.clone());
            }
            if !marked.contains_key(&run)
                || !inspection.budget.guarantee_intact
                || unresolved
                || unchanged_failure
                || matches!(inspection.state, RunState::Paused | RunState::Stopped) && !unstarted
            {
                runtime.automatic_block(&run);
                return Err(with_embedding_context(
                    fail(
                        ErrorCode::RecoveryRequired,
                        "retained automatic accounting needs explicit inspection; no replacement request was admitted",
                    ),
                    serde_json::json!({"run_id":run}),
                ));
            }
        }
        if let Some(run) = unmarked_pending_run {
            runtime.automatic_block(&run);
            return Err(with_embedding_context(
                fail(
                    ErrorCode::RecoveryRequired,
                    "retained pending work has no automatic origin; continue its existing Run explicitly",
                ),
                serde_json::json!({"run_id":run}),
            ));
        }
        auto.progress.borrow_mut().accounting_checked = true;
        Ok(())
    }
}
