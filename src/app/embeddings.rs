//! Explicit remote embedding work; all paid requests use production dispatcher and ledger.
use super::{
    OfflineApp,
    indexed_embedding_inputs::{self, NormalizedEmbeddingInputs},
};
#[path = "indexed_embedding_check.rs"]
mod indexed_check;
#[path = "indexed_embedding_sync.rs"]
mod indexed_sync;
use crate::config::providers::TrustedService;
use crate::{
    catalog::{Catalog, CatalogGraphValidator, ReaderSnapshot},
    changes::{ChangeEngine, ReadDependency},
    domain::*,
    graph::*,
    jobs::{self, DispatcherLedgerApi, *},
    providers::{
        dispatcher::{Dispatcher, RetainedDecodeFailure},
        types::*,
    },
    retrieval::{
        self,
        render::{self, RenderedUnit, TargetKind},
        spaces::{EmbeddingSettings, SpaceSpec},
        vectors::{Coverage, SpaceState, VectorStore},
        *,
    },
    vault::{ExpectedState, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

struct TaskMaterialization {
    probe: bool,
    corpus: bool,
}
enum EmbeddingCorpus {
    Legacy {
        reader: ReaderSnapshot,
        units: Vec<RenderedUnit>,
    },
    Normalized(NormalizedEmbeddingInputs),
}
impl EmbeddingCorpus {
    fn take_units(&mut self) -> Vec<RenderedUnit> {
        match self {
            Self::Legacy { units, .. } => std::mem::take(units),
            Self::Normalized(inputs) => std::mem::take(&mut inputs.units),
        }
    }
    fn units(&self) -> &[RenderedUnit] {
        match self {
            Self::Legacy { units, .. } => units,
            Self::Normalized(inputs) => &inputs.units,
        }
    }
    fn snapshot(&self) -> &ReadSnapshot {
        match self {
            Self::Legacy { reader, .. } => reader.snapshot(),
            Self::Normalized(inputs) => &inputs.snapshot,
        }
    }
    fn recheck(&mut self, catalog: &Catalog) -> Result<()> {
        match self {
            Self::Legacy { .. } => Ok(()),
            Self::Normalized(inputs) => inputs.recheck(catalog),
        }
    }
    fn into_units(self) -> Vec<RenderedUnit> {
        match self {
            Self::Legacy { units, .. } => units,
            Self::Normalized(inputs) => inputs.units,
        }
    }
}
#[derive(Default)]
struct RecoveredEmbeddingWork {
    generated: usize,
    generated_hashes: BTreeSet<Blake3Hash>,
    run_id: Option<RecordId>,
    warnings: Vec<String>,
}
struct HitScope<'a> {
    context: Option<bool>,
    graph: Option<&'a GraphPlan>,
}
#[derive(Clone)]
pub struct EmbeddingRuntime<'a> {
    pub service: &'a TrustedService,
    pub dispatcher: &'a Dispatcher,
    pub job_options: JobOptions,
    pub limits: LifetimeLimits,
    pub deadline_ms: u64,
    pub requested_limits: Option<super::remote::RequestedJobLimits>,
    pub created_at_utc_ms: i64,
    pub deadline_utc_ms: i64,
    invocation: Option<std::sync::Arc<crate::providers::invocation_budget::InvocationBudget>>,
}
impl<'a> EmbeddingRuntime<'a> {
    /// Preserve the remote runtime's original operation start and deadline.
    /// Public operations snapshot mutable caller limits when they begin.
    pub fn new(
        service: &'a TrustedService,
        dispatcher: &'a Dispatcher,
        job_options: JobOptions,
        limits: LifetimeLimits,
        created_at_utc_ms: i64,
        deadline_utc_ms: i64,
        requested_limits: Option<super::remote::RequestedJobLimits>,
    ) -> Self {
        Self {
            service,
            dispatcher,
            job_options,
            limits,
            deadline_ms: deadline_utc_ms
                .checked_sub(created_at_utc_ms)
                .and_then(|duration| u64::try_from(duration).ok())
                .unwrap_or(0),
            requested_limits,
            created_at_utc_ms,
            deadline_utc_ms,
            invocation: None,
        }
    }
    fn scoped_for_operation(&self) -> Result<Self> {
        let mut scoped = self.clone();
        if scoped.invocation.is_none() {
            // Honor caller changes made before operation entry, then freeze.
            let deadline = self
                .created_at_utc_ms
                .checked_add(
                    i64::try_from(self.deadline_ms)
                        .map_err(|_| WikiError::invalid("embedding deadline overflow"))?,
                )
                .ok_or_else(|| WikiError::invalid("embedding deadline overflow"))?;
            scoped.deadline_utc_ms = deadline;
            scoped.invocation = Some(std::sync::Arc::new(
                crate::providers::invocation_budget::InvocationBudget::new(
                    self.limits.clone(),
                    self.created_at_utc_ms,
                    deadline,
                )?,
            ));
        }
        Ok(scoped)
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingReport {
    pub space: Option<Blake3Hash>,
    pub active_space: Option<Blake3Hash>,
    /// Effective candidate or active-space settings used to calculate coverage.
    pub settings: Option<EmbeddingSettings>,
    pub coverage: Coverage,
    pub generated_inputs: usize,
    /// Distinct compatible cached inputs consulted during this invocation.
    /// Normalized preparation excludes already acknowledged owners and inputs
    /// generated or recovered by this invocation; total coverage is separate.
    pub reused_inputs: usize,
    pub published: bool,
    pub dry_run: bool,
    pub network_used: bool,
    pub run_id: Option<RecordId>,
    pub warnings: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct RunMarker {
    version: u32,
    space: Blake3Hash,
    run_id: RecordId,
    operation: String,
    task_keys: Vec<Blake3Hash>,
    expected_units: Vec<RenderedUnit>,
}
fn fail(code: ErrorCode, text: &str) -> WikiError {
    WikiError::new(code, text)
}
fn with_embedding_context(mut error: WikiError, mut context: serde_json::Value) -> WikiError {
    let reason = error.details.get("reason").cloned().or_else(|| {
        error
            .details
            .get("cause")
            .and_then(|cause| cause.get("reason"))
            .cloned()
    });
    let invocation = error
        .details
        .get("budget_scope")
        .and_then(serde_json::Value::as_str)
        == Some("invocation");
    let continuation = invocation
        .then(|| error.details.get("next_action").cloned())
        .flatten();
    if let Some(object) = context.as_object_mut() {
        if invocation {
            object.insert("budget_scope".into(), serde_json::json!("invocation"));
            if let Some(action) = continuation {
                object.insert("next_action".into(), action);
            }
        }
        if let Some(reason) = reason {
            object.insert("reason".into(), reason);
        }
        object.insert("cause".into(), error.details);
    }
    error.details = context;
    error
}
fn proof(units: &[RenderedUnit]) -> Result<Blake3Hash> {
    Ok(Blake3Hash::digest(crate::graph::packet::canonical_json(
        &units
            .iter()
            .map(|u| (&u.unit_id, &u.input_hash, &u.dependency_fingerprint))
            .collect::<Vec<_>>(),
    )?))
}
fn same_units(expected: &[RenderedUnit], actual: &[RenderedUnit]) -> bool {
    expected.iter().all(|u| {
        actual.iter().any(|a| {
            a.unit_id == u.unit_id
                && a.input_hash == u.input_hash
                && a.dependency_fingerprint == u.dependency_fingerprint
                && a.target == u.target
                && a.owner == u.owner
        })
    })
}
impl OfflineApp {
    fn embedding_marker_version(&self) -> Result<u32> {
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        Ok(if catalog.operation_state()?.is_some() {
            2
        } else {
            1
        })
    }
    /// A normalized corpus is admitted from bounded selected closures. Explicit
    /// full preparation may enumerate owners; per-task checks name only owners
    /// retained before dispatch. Legacy commands retain their existing boundary.
    fn embedding_inputs(
        &self,
        settings: &EmbeddingSettings,
        no_sync: bool,
        writer: Option<&WriterPermit>,
        paths: Option<&[VaultRelativePath]>,
    ) -> Result<EmbeddingCorpus> {
        self.embedding_inputs_bounded(
            settings,
            no_sync,
            writer,
            paths,
            &VerificationBudget::default(),
        )
    }
    fn embedding_inputs_bounded(
        &self,
        settings: &EmbeddingSettings,
        no_sync: bool,
        writer: Option<&WriterPermit>,
        paths: Option<&[VaultRelativePath]>,
        budget: &VerificationBudget,
    ) -> Result<EmbeddingCorpus> {
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        if catalog.operation_state()?.is_some() {
            return indexed_embedding_inputs::materialize(&catalog, settings, paths, budget)
                .map(EmbeddingCorpus::Normalized);
        }
        let reader = if let Some(writer) = writer {
            catalog.verified_snapshot(Some(writer))?
        } else {
            self.embedding_reader(no_sync)?
        };
        let units = render::corpus(&reader, settings)?;
        Ok(EmbeddingCorpus::Legacy { reader, units })
    }
    fn embedding_phase() -> Result<std::rc::Rc<retrieval::vectors::VectorReadBudget>> {
        retrieval::vectors::VectorReadBudget::new(
            std::time::Instant::now()
                + Duration::from_millis(VerificationBudget::default().max_elapsed_ms),
        )
    }
    fn embedding_phase_proof_budget(
        phase: &retrieval::vectors::VectorReadBudget,
    ) -> Result<VerificationBudget> {
        let mut budget = VerificationBudget::default();
        budget.max_elapsed_ms = budget.max_elapsed_ms.min(phase.remaining_ms()?);
        Ok(budget)
    }
    fn embedding_owner_paths(units: &[RenderedUnit]) -> Vec<VaultRelativePath> {
        units
            .iter()
            .map(|unit| unit.owner.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    fn embedding_targets_current(
        &self,
        spec: &SpaceSpec,
        expected: &[RenderedUnit],
        writer: &WriterPermit,
    ) -> Result<bool> {
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let normalized = catalog.operation_state()?.is_some();
        let paths = Self::embedding_owner_paths(expected);
        let result: Result<bool> = (|| {
            // Retained tasks may predate bounded supplier packing. Authenticate
            // their original owners in declared scopes; the receipt still checks
            // the complete original source-guard union atomically.
            let scope_size = if normalized {
                indexed_embedding_inputs::PROOF_SCOPE_OWNERS
            } else {
                paths.len().max(1)
            };
            let mut snapshot = None;
            for scope in paths.chunks(scope_size) {
                let mut inputs =
                    self.embedding_inputs(&spec.settings, false, Some(writer), Some(scope))?;
                if snapshot
                    .as_ref()
                    .is_some_and(|old| old != inputs.snapshot())
                {
                    return Ok(false);
                }
                snapshot = Some(inputs.snapshot().clone());
                let selected: Vec<_> = expected
                    .iter()
                    .filter(|unit| scope.contains(&unit.owner))
                    .cloned()
                    .collect();
                if !same_units(&selected, inputs.units()) {
                    return Ok(false);
                }
                inputs.recheck(&catalog)?;
            }
            Ok(true)
        })();
        match result {
            Err(error)
                if normalized
                    && matches!(
                        error.code,
                        ErrorCode::FreshnessConflict | ErrorCode::CapabilityUnavailable
                    ) =>
            {
                Ok(false)
            }
            other => other,
        }
    }
    fn embedding_writer(&self) -> Result<WriterPermit> {
        WriterPermit::acquire(
            self.fs.root(),
            Duration::from_millis(self.options.lock_timeout_ms),
        )
    }
    fn embedding_reader(&self, no_sync: bool) -> Result<ReaderSnapshot> {
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        if no_sync || self.options.dry_run {
            catalog.index_snapshot()
        } else {
            let writer = self.embedding_writer()?;
            catalog.verified_snapshot(Some(&writer))
        }
    }
    fn embedding_policy_consistent(&self, runtime: Option<&EmbeddingRuntime<'_>>) -> Result<()> {
        if runtime.is_some_and(|r| r.job_options.policy.dry_run != self.options.dry_run) {
            return Err(fail(
                ErrorCode::Usage,
                "embedding runtime and application policies differ",
            ));
        }
        Ok(())
    }
    fn remote_gate(&self, runtime: &EmbeddingRuntime<'_>) -> Result<()> {
        if self.options.dry_run || runtime.job_options.policy.dry_run {
            return Err(fail(ErrorCode::Usage, "dry-run prohibits embedding jobs"));
        }
        if self.options.offline || runtime.job_options.policy.offline {
            return Err(fail(
                ErrorCode::OfflineUnavailable,
                "offline prohibits embedding dispatch",
            ));
        }
        runtime.service.recheck(&self.fs)?;
        jobs::budgets::validate_limits(&runtime.limits)?;
        if runtime.deadline_ms == 0 || runtime.deadline_ms > 86_400_000 {
            return Err(fail(
                ErrorCode::Usage,
                "embedding deadline must be within one day",
            ));
        }
        Ok(())
    }
    pub fn embeddings_check(
        &self,
        settings: &EmbeddingSettings,
        runtime: Option<&EmbeddingRuntime<'_>>,
        probe: bool,
    ) -> Result<EmbeddingReport> {
        let scoped_runtime = runtime
            .map(EmbeddingRuntime::scoped_for_operation)
            .transpose()?;
        let runtime = scoped_runtime.as_ref();
        settings.validate()?;
        self.embedding_policy_consistent(runtime)?;
        let candidate = runtime
            .map(|r| SpaceSpec::from_service(r.service, settings.clone()))
            .transpose()?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let normalized = catalog.operation_state()?.is_some();
        let (spec, space, active, coverage) = if normalized {
            self.embeddings_coverage_indexed(candidate)?
        } else {
            let phase = Self::embedding_phase()?;
            let store = match VectorStore::open_bounded(&self.fs, &phase) {
                Ok(store) => Some(store),
                Err(e) if e.code == ErrorCode::OfflineUnavailable => None,
                Err(e) => return Err(e),
            };
            let active = store
                .as_ref()
                .map(VectorStore::active)
                .transpose()?
                .flatten();
            let spec = candidate.or_else(|| active.as_ref().map(|s| s.spec.clone()));
            let mut inputs = spec
                .as_ref()
                .map(|spec| {
                    self.embedding_inputs_bounded(
                        &spec.settings,
                        self.options.dry_run,
                        None,
                        None,
                        &Self::embedding_phase_proof_budget(&phase)?,
                    )
                })
                .transpose()?;
            let units = inputs.as_ref().map(EmbeddingCorpus::units).unwrap_or(&[]);
            let space = spec.as_ref().map(SpaceSpec::id).transpose()?;
            let coverage = if let (Some(store), Some(space)) = (&store, &space) {
                store.coverage(space, &units)?
            } else {
                Coverage {
                    eligible_units: units.len(),
                    missing_units: units.len(),
                    ..Default::default()
                }
            };
            if let Some(inputs) = &mut inputs {
                inputs.recheck(&Catalog::new(self.fs.clone(), self.vault_id.clone()))?;
            }
            phase.remaining_ms()?;
            drop(inputs);
            drop(store);
            (spec, space, active.map(|state| state.id), coverage)
        };
        let mut report=EmbeddingReport {space:space.clone(),active_space:active,settings:spec.as_ref().map(|s|s.settings.clone()),coverage,generated_inputs:0,reused_inputs:0,published:false,dry_run:self.options.dry_run,network_used:false,run_id:None,warnings:vec!["local check does not establish provider compatibility; missing/corrupt cache is missing coverage".into()]};
        if normalized {
            report.warnings.push("Coverage uses one pinned vector-cache view and individually authenticated owner scopes at an unchanged catalog publication; check is limited to 4096 owners and 120 seconds, and fails explicitly if incomplete.".into());
        }
        if spec.is_none() {
            report.warnings.push("No candidate or active embedding space is configured; the eligible-unit denominator is unknown, not an empty-corpus audit.".into());
        }
        if probe && !self.options.dry_run {
            let runtime = runtime.ok_or_else(|| {
                fail(
                    ErrorCode::CapabilityUnavailable,
                    "probe requires an independently trusted embedding service",
                )
            })?;
            self.remote_gate(runtime)?;
            let spec = spec.ok_or_else(|| {
                fail(
                    ErrorCode::CapabilityUnavailable,
                    "embedding configuration absent",
                )
            })?;
            let input = spec.query("lwiki embedding check")?;
            let (ledger, tasks) = self.embedding_job(
                &spec,
                &[vec![input]],
                &[],
                runtime,
                "embeddings_check",
                true,
            )?;
            report.run_id = Some(ledger.inspect()?.spec.run_id.clone());
            for task in tasks {
                report.network_used |= self.dispatch_embedding_task(
                    &ledger,
                    &task,
                    &spec,
                    &[],
                    runtime,
                    TaskMaterialization {
                        probe: true,
                        corpus: false,
                    },
                )? > 0;
            }
            if let Some(warning) = self.finish_embedding_job(&ledger)? {
                report.warnings.push(warning);
            }
            report.warnings.push("explicit accounted probe validates one response; auto dimension remains corpus-unestablished".into());
        }
        Ok(report)
    }
    pub fn embeddings_sync(
        &self,
        settings: &EmbeddingSettings,
        runtime: &EmbeddingRuntime<'_>,
    ) -> Result<EmbeddingReport> {
        let scoped_runtime = runtime.scoped_for_operation()?;
        let runtime = &scoped_runtime;
        settings.validate()?;
        self.embedding_policy_consistent(Some(runtime))?;
        if self.options.offline {
            return self.embeddings_sync_cached(settings);
        }
        if !self.options.dry_run
            && Catalog::new(self.fs.clone(), self.vault_id.clone())
                .operation_state()?
                .is_some()
        {
            return self.embeddings_sync_indexed(settings, Some(runtime));
        }
        let phase = Self::embedding_phase()?;
        let spec = SpaceSpec::from_service(runtime.service, settings.clone())?;
        let space = spec.id()?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let mut inputs = self.embedding_inputs_bounded(
            settings,
            self.options.dry_run,
            None,
            None,
            &Self::embedding_phase_proof_budget(&phase)?,
        )?;
        let units = inputs.units();
        let writer = if self.options.dry_run {
            None
        } else {
            Some(self.embedding_writer()?)
        };
        let mut store = match VectorStore::open(&self.fs, writer.as_ref()) {
            Ok(s) => Some(s),
            Err(e) if self.options.dry_run && e.code == ErrorCode::OfflineUnavailable => None,
            Err(e) => return Err(e),
        };
        if let Some(store) = &mut store {
            store.bind_read_budget(&phase)?;
        }
        let old_active = store
            .as_ref()
            .map(VectorStore::active)
            .transpose()?
            .flatten();
        let mut unique = BTreeMap::new();
        for unit in units {
            unique
                .entry(unit.input_hash.clone())
                .or_insert_with(|| unit.input());
        }
        let mut missing = Vec::new();
        let mut reused = 0;
        for (hash, input) in unique {
            if store
                .as_ref()
                .map(|s| s.vector(&space, &hash))
                .transpose()?
                .flatten()
                .is_some()
            {
                reused += 1;
            } else {
                missing.push(input);
            }
        }
        let mut report = EmbeddingReport {
            space: Some(space.clone()),
            active_space: old_active.as_ref().map(|s| s.id.clone()),
            settings: Some(spec.settings.clone()),
            coverage: Coverage {
                eligible_units: units.len(),
                missing_units: units.len(),
                ..Default::default()
            },
            generated_inputs: 0,
            reused_inputs: reused,
            published: false,
            dry_run: self.options.dry_run,
            network_used: false,
            run_id: None,
            warnings: Vec::new(),
        };
        if self.options.dry_run {
            report.coverage = if let Some(store) = store {
                store.coverage(&space, &units)?
            } else {
                report.coverage
            };
            inputs.recheck(&catalog)?;
            phase.remaining_ms()?;
            report
                .warnings
                .push("dry-run leaves index, jobs, cache, helpers and providers untouched".into());
            return Ok(report);
        }
        let store_ref = store.as_mut().expect("writable store");
        store_ref.prepare_space(&spec)?;
        inputs.recheck(&catalog)?;
        phase.remaining_ms()?;
        let units = inputs.into_units();
        drop(writer);
        drop(store);
        let recovered = self.recover_embedding_jobs(&spec, runtime)?;
        report.generated_inputs += recovered.generated;
        report.network_used |= recovered.generated > 0;
        report.run_id = recovered.run_id;
        report.warnings.extend(recovered.warnings);
        let availability_phase = Self::embedding_phase()?;
        let available = VectorStore::open_bounded_snapshot(&self.fs, &availability_phase)?;
        let mut pending = Vec::new();
        for input in missing {
            if available.vector(&space, &input.input_hash)?.is_none() {
                pending.push(input);
            } else if !recovered.generated_hashes.contains(&input.input_hash) {
                report.reused_inputs += 1;
            }
        }
        missing = pending;
        availability_phase.remaining_ms()?;
        drop(available);
        if !missing.is_empty() {
            self.remote_gate(runtime)?;
            let batches = self.embedding_batches(runtime, &missing)?;
            let (ledger, tasks) =
                self.embedding_job(&spec, &batches, &units, runtime, "embeddings_sync", false)?;
            report.run_id = Some(ledger.inspect()?.spec.run_id.clone());
            for task in tasks {
                let already = ledger
                    .inspect()?
                    .tasks
                    .get(&task.key)
                    .is_some_and(|t| t.state == TaskState::Completed);
                if already {
                    continue;
                }
                let generated=self.dispatch_embedding_task(&ledger,&task,&spec,&units,runtime,TaskMaterialization { probe:false, corpus:true }).map_err(|e| with_embedding_context(e,serde_json::json!({"run_id":report.run_id,"generated_inputs":report.generated_inputs,"space":space})))?;
                report.generated_inputs += generated;
                report.network_used |= generated > 0;
            }
            if let Some(warning) = self.finish_embedding_job(&ledger)? {
                report.warnings.push(warning);
            }
        }
        let writer = self.embedding_writer()?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let phase = Self::embedding_phase()?;
        let mut fresh = self.embedding_inputs_bounded(
            settings,
            false,
            Some(&writer),
            None,
            &Self::embedding_phase_proof_budget(&phase)?,
        )?;
        let current = fresh.take_units();
        let snapshot = fresh.snapshot().clone();
        let mut store = VectorStore::open(&self.fs, Some(&writer))?;
        store.bind_read_budget(&phase)?;
        let coverage = store.coverage(&space, &current)?;
        let complete = coverage.eligible_units > 0
            && coverage.missing_units == 0
            && store
                .space(&space)?
                .is_some_and(|state| state.actual_dimensions.is_some());
        report.coverage = store.memberships_with_spec_checked(
            &space,
            &snapshot,
            &current,
            complete,
            if complete { Some(&spec) } else { None },
            || {
                fresh.recheck(&catalog)?;
                phase.remaining_ms()?;
                Ok(())
            },
        )?;
        report.published = complete;
        if let Some(old) = old_active.filter(|old| old.id != space)
            && !complete
        {
            let mut old_inputs = self.embedding_inputs_bounded(
                &old.spec.settings,
                false,
                Some(&writer),
                None,
                &Self::embedding_phase_proof_budget(&phase)?,
            )?;
            let old_units = old_inputs.take_units();
            let old_snapshot = old_inputs.snapshot().clone();
            let old_coverage = store.memberships_with_spec_checked(
                &old.id,
                &old_snapshot,
                &old_units,
                false,
                None,
                || {
                    old_inputs.recheck(&catalog)?;
                    phase.remaining_ms()?;
                    Ok(())
                },
            )?;
            report.warnings.push(format!(
                "replacement incomplete; retained active space coverage {}/{}",
                old_coverage.available_units, old_coverage.eligible_units
            ));
        }
        report.active_space = store.active()?.map(|s| s.id);
        if !complete {
            report.warnings.push(if current.is_empty() {
                "No eligible embedding inputs; retained active space remains unchanged.".into()
            } else { "current inputs missing/corrupt or dimensions unestablished; replacement pointer remains unchanged".into() });
        }
        Ok(report)
    }
    /// Publish only coverage already present in the retained active space. This
    /// route needs no provider profile or credentials and does not resume jobs.
    pub fn embeddings_sync_cached(&self, settings: &EmbeddingSettings) -> Result<EmbeddingReport> {
        if !self.options.dry_run
            && Catalog::new(self.fs.clone(), self.vault_id.clone())
                .operation_state()?
                .is_some()
        {
            return self.embeddings_sync_indexed(settings, None);
        }
        let phase = Self::embedding_phase()?;
        settings.validate()?;
        let store = VectorStore::open_bounded_snapshot(&self.fs, &phase)?;
        let active = store.active()?.ok_or_else(|| {
            fail(
                ErrorCode::OfflineUnavailable,
                "cache-only sync requires a retained active embedding space",
            )
        })?;
        if &active.spec.settings != settings {
            return Err(fail(
                ErrorCode::CapabilityUnavailable,
                "cache-only sync settings differ from the retained active space",
            ));
        }
        drop(store);
        let writer = if self.options.dry_run {
            None
        } else {
            Some(self.embedding_writer()?)
        };
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let mut inputs = self.embedding_inputs_bounded(
            settings,
            self.options.dry_run,
            writer.as_ref(),
            None,
            &Self::embedding_phase_proof_budget(&phase)?,
        )?;
        let units = inputs.take_units();
        let snapshot = inputs.snapshot().clone();
        let mut store = VectorStore::open(&self.fs, writer.as_ref())?;
        store.bind_read_budget(&phase)?;
        let current_active = VectorStore::open_bounded(&self.fs, &phase)?.active()?;
        if current_active.is_none_or(|current| {
            current.id != active.id
                || current.spec != active.spec
                || current.actual_dimensions != active.actual_dimensions
        }) {
            return Err(fail(
                ErrorCode::FreshnessConflict,
                "active space changed before cache-only sync",
            ));
        }
        let coverage = store.coverage(&active.id, &units)?;
        let complete = coverage.eligible_units > 0
            && coverage.missing_units == 0
            && active.actual_dimensions.is_some();
        let mut reused_inputs = 0;
        for hash in units
            .iter()
            .map(|unit| &unit.input_hash)
            .collect::<BTreeSet<_>>()
        {
            if store.vector(&active.id, hash)?.is_some() {
                reused_inputs += 1;
            }
        }
        let coverage = if self.options.dry_run {
            inputs.recheck(&catalog)?;
            phase.remaining_ms()?;
            coverage
        } else {
            store.memberships_with_spec_checked(
                &active.id,
                &snapshot,
                &units,
                complete,
                if complete { Some(&active.spec) } else { None },
                || {
                    inputs.recheck(&catalog)?;
                    phase.remaining_ms()?;
                    Ok(())
                },
            )?
        };
        Ok(EmbeddingReport {
            space: Some(active.id.clone()), active_space: Some(active.id),
            settings: Some(active.spec.settings), coverage, generated_inputs: 0,
            reused_inputs,
            published: complete && !self.options.dry_run, dry_run: self.options.dry_run,
            network_used: false, run_id: None,
            warnings: vec!["Cache-only sync uses retained vectors; missing inputs remain missing and retained accounting jobs are untouched.".into()],
        })
    }
    /// Reconcile previously paid outputs before constructing replacement tasks. The
    /// immutable marker retains the exact pre-request target proof, including decisions.
    fn recover_embedding_jobs(
        &self,
        spec: &SpaceSpec,
        runtime: &EmbeddingRuntime<'_>,
    ) -> Result<RecoveredEmbeddingWork> {
        let mut activity = RecoveredEmbeddingWork::default();
        let relative = VaultRelativePath::new(".wiki/state/embedding-jobs")?;
        let directory = self.fs.root().resolve(&relative)?;
        if !directory.exists() {
            return Ok(activity);
        }
        let mut paths = std::fs::read_dir(directory)
            .map_err(|e| fail(ErrorCode::Internal, &e.to_string()))?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| fail(ErrorCode::Internal, &e.to_string()))?;
        if paths.len() > 4096 {
            return Err(fail(
                ErrorCode::BudgetExceeded,
                "embedding recovery marker count exceeds bound",
            ));
        }
        paths.sort();
        let mut read_bytes = 0;
        let mut retained_jobs = Vec::new();
        for name in paths {
            let name = name
                .to_str()
                .ok_or_else(|| WikiError::invalid("embedding marker path UTF-8 unavailable"))?;
            if !name.ends_with(".json") {
                continue;
            }
            let path = VaultRelativePath::new(format!(".wiki/state/embedding-jobs/{name}"))?;
            let bytes = crate::changes::prepare::read_bounded(
                &self.fs,
                &path,
                crate::changes::prepare::MAX_PAYLOAD_BYTES,
            )?
            .ok_or_else(|| fail(ErrorCode::FreshnessConflict, "embedding marker disappeared"))?;
            read_bytes += bytes.len();
            if read_bytes > 64 * 1024 * 1024 {
                return Err(fail(
                    ErrorCode::BudgetExceeded,
                    "embedding recovery metadata byte bound reached",
                ));
            }
            let marker: RunMarker = serde_json::from_slice(&bytes)
                .map_err(|_| WikiError::invalid("embedding marker invalid"))?;
            if marker.version != self.embedding_marker_version()? || marker.space != spec.id()? {
                continue;
            }
            let ledger = JobLedger::new(
                self.fs.clone(),
                self.vault_id.clone(),
                marker.run_id.clone(),
                runtime.job_options.clone(),
            )?;
            let inspection = ledger.replay()?.inspection;
            if marker.operation != inspection.spec.scope.operation
                || marker.task_keys
                    != inspection
                        .spec
                        .tasks
                        .iter()
                        .map(|task| task.key.clone())
                        .collect::<Vec<_>>()
                || inspection.spec.scope.scope_payload_hash.as_ref()
                    != Some(&proof(&marker.expected_units)?)
            {
                return Err(fail(
                    ErrorCode::RecoveryRequired,
                    "retained embedding marker differs from its immutable Run scope",
                ));
            }
            if matches!(inspection.state, RunState::Completed | RunState::Failed) {
                for task in inspection
                    .tasks
                    .values()
                    .filter(|task| task.state == TaskState::Failed)
                {
                    ledger.finish_rejected_remote_task(&task.spec.key)?;
                }
                continue;
            }
            let mut retired = BTreeSet::new();
            for task in inspection
                .tasks
                .values()
                .filter(|t| !matches!(t.state, TaskState::Completed | TaskState::Failed))
            {
                let attempt = inspection
                    .attempts
                    .iter()
                    .rev()
                    .find(|a| a.attempt.task_key == task.spec.key);
                let Some(attempt) = attempt else { continue };
                // A proven pre-send release has no paid receipt. Its task is
                // pending again, with the existing Run's attempt ceiling.
                if attempt.billing == BillingDisposition::ReleasedNotSent {
                    if attempt.phase != AttemptPhase::Settled || attempt.receipt.is_some() {
                        return Err(with_embedding_context(
                            fail(
                                ErrorCode::RecoveryRequired,
                                "retained pre-send release has inconsistent receipt state",
                            ),
                            serde_json::json!({"run_id":inspection.spec.run_id}),
                        ));
                    }
                    continue;
                }
                match attempt.phase {
                    AttemptPhase::Reserved | AttemptPhase::DispatchIntent => continue,
                    AttemptPhase::Received => {}
                    AttemptPhase::OutputCommitted | AttemptPhase::Settled => {
                        if attempt.receipt.is_none() {
                            return Err(with_embedding_context(
                                fail(
                                    ErrorCode::RecoveryRequired,
                                    "retained embedding output lacks its accounted receipt",
                                ),
                                serde_json::json!({"run_id":inspection.spec.run_id}),
                            ));
                        }
                    }
                }
                if matches!(
                    attempt.phase,
                    AttemptPhase::OutputCommitted | AttemptPhase::Settled
                ) && attempt.cache_outputs.is_empty()
                    && self.embedding_receipt_disposition(attempt)? == OutputDisposition::Rejected
                {
                    if attempt.phase == AttemptPhase::OutputCommitted {
                        ledger.settle(&attempt.attempt)?;
                    }
                    if task.spec.stage == TaskStage::Probe {
                        self.verify_rejected_embedding_probe(
                            &ledger, &marker, &task.spec, attempt, spec,
                        )?;
                        activity.warnings.push(format!("Rejected embedding probe in job {} retains its accounted receipt and charges; collection preparation continues independently.", inspection.spec.run_id));
                        retired.insert(task.spec.key.clone());
                        continue;
                    }
                    ledger.finish_rejected_remote_task(&task.spec.key)?;
                    activity.warnings.push(format!("Task {} in job {} retained a rejected embedding response; unchanged siblings continue under the original Run.", task.spec.key, inspection.spec.run_id));
                    retired.insert(task.spec.key.clone());
                    continue;
                }
                if matches!(
                    attempt.phase,
                    AttemptPhase::OutputCommitted | AttemptPhase::Settled
                ) && !attempt.cache_outputs.is_empty()
                {
                    let cache = VectorStore::open(&self.fs, None)?;
                    let missing = attempt
                        .cache_outputs
                        .iter()
                        .map(|r| cache.verify_ref(r))
                        .collect::<Result<Vec<_>>>()?
                        .into_iter()
                        .any(|valid| !valid);
                    if missing {
                        if attempt.phase == AttemptPhase::OutputCommitted {
                            ledger.settle(&attempt.attempt)?;
                        }
                        // Already-accounted immutable receipt survives; no invented completion.
                        // This explicit sync is allowed to schedule a replacement below.
                        retired.insert(task.spec.key.clone());
                        continue;
                    }
                }
                if matches!(
                    attempt.phase,
                    AttemptPhase::Received | AttemptPhase::OutputCommitted | AttemptPhase::Settled
                ) {
                    let generated = match self.dispatch_embedding_task(
                        &ledger,
                        &task.spec,
                        spec,
                        &marker.expected_units,
                        runtime,
                        TaskMaterialization {
                            probe: marker.operation == "embeddings_check",
                            corpus: marker.operation == "embeddings_sync",
                        },
                    ) {
                        Ok(generated) => generated,
                        Err(error) => {
                            // Only an exact newly committed rejected receipt
                            // authorizes continuation; arbitrary freshness or
                            // persistence failures still stop this operation.
                            let after = ledger.inspect()?;
                            let rejected = after
                                .attempts
                                .iter()
                                .rev()
                                .find(|a| a.attempt.task_key == task.spec.key)
                                .filter(|a| {
                                    a.phase == AttemptPhase::Settled && a.cache_outputs.is_empty()
                                })
                                .map(|a| self.embedding_receipt_disposition(a))
                                .transpose()?
                                == Some(OutputDisposition::Rejected);
                            if !rejected {
                                return Err(error);
                            }
                            if task.spec.stage == TaskStage::Probe {
                                let attempt = after
                                    .attempts
                                    .iter()
                                    .rev()
                                    .find(|a| a.attempt.task_key == task.spec.key)
                                    .ok_or_else(|| {
                                        fail(
                                            ErrorCode::RecoveryRequired,
                                            "rejected probe attempt missing",
                                        )
                                    })?;
                                self.verify_rejected_embedding_probe(
                                    &ledger, &marker, &task.spec, attempt, spec,
                                )?;
                                activity.warnings.push(format!("Rejected embedding probe in job {} retains its accounted receipt and charges; collection preparation continues independently.", inspection.spec.run_id));
                            } else {
                                ledger.finish_rejected_remote_task(&task.spec.key)?;
                                activity.warnings.push(format!("Task {} in job {} retained a rejected embedding response ({:?}); unchanged siblings continue under the original Run.", task.spec.key, inspection.spec.run_id, error.code));
                            }
                            retired.insert(task.spec.key.clone());
                            0
                        }
                    };
                    activity.generated += generated;
                    if generated > 0 {
                        activity.run_id = Some(inspection.spec.run_id.clone());
                        activity.generated_hashes.extend(
                            ledger.inspect()?.tasks[&task.spec.key]
                                .cache_outputs
                                .iter()
                                .map(|output| output.input_hash.clone()),
                        );
                    }
                }
            }
            retained_jobs.push((marker, ledger, retired));
        }
        // All retained Received/output-committed scopes are replayed before
        // any fresh exposure, including pending tasks in a different marker.
        for (marker, ledger, retired) in retained_jobs {
            let retained = ledger.replay()?.inspection;
            let mut pending = Vec::new();
            for task in retained
                .tasks
                .values()
                .filter(|task| !matches!(task.state, TaskState::Completed | TaskState::Failed))
            {
                if retired.contains(&task.spec.key) {
                    continue;
                }
                let attempt = retained
                    .attempts
                    .iter()
                    .rev()
                    .find(|attempt| attempt.attempt.task_key == task.spec.key);
                let context =
                    || serde_json::json!({"run_id":retained.spec.run_id,"space":marker.space});
                match attempt {
                    None => {
                        if marker.version == 2 && marker.operation == "embeddings_sync" {
                            pending.push(task.spec.clone());
                        }
                    }
                    Some(attempt) => match attempt.phase {
                        AttemptPhase::Reserved => {
                            return Err(with_embedding_context(
                                fail(
                                    ErrorCode::RecoveryRequired,
                                    "retained embedding reservation remains unresolved; inspect the existing Run before admitting replacement work",
                                ),
                                context(),
                            ));
                        }
                        AttemptPhase::DispatchIntent => {
                            match attempt.remote_exposure {
                                RemoteExposure::PossiblyInFlight
                                | RemoteExposure::TerminalConfirmed => {
                                    // Terminal reconciliation does not create an output,
                                    // release unknown billing, or fund another Run. Use
                                    // the existing explicit retry policy and allowances.
                                    if !runtime.job_options.policy.retry_uncertain {
                                        return Err(with_embedding_context(
                                            fail(
                                                ErrorCode::RecoveryRequired,
                                                "retained embedding send has no committed output; continuation requires --retry-uncertain under the existing Run's limits",
                                            ),
                                            context(),
                                        ));
                                    }
                                    pending.push(task.spec.clone());
                                }
                                RemoteExposure::NotStarted => {
                                    return Err(with_embedding_context(
                                        fail(
                                            ErrorCode::RecoveryRequired,
                                            "retained embedding dispatch intent requires existing-Run reconciliation",
                                        ),
                                        context(),
                                    ));
                                }
                            }
                        }
                        AttemptPhase::Settled
                            if attempt.billing == BillingDisposition::ReleasedNotSent =>
                        {
                            pending.push(task.spec.clone());
                        }
                        AttemptPhase::Received
                        | AttemptPhase::OutputCommitted
                        | AttemptPhase::Settled => {
                            return Err(with_embedding_context(
                                fail(
                                    ErrorCode::RecoveryRequired,
                                    "retained embedding task remains incomplete after receipt recovery; inspect its existing Run",
                                ),
                                context(),
                            ));
                        }
                    },
                }
            }
            if !pending.is_empty() {
                super::remote::validate_retained_arguments(
                    &retained,
                    &runtime.limits,
                    runtime.deadline_ms,
                    runtime.requested_limits.as_ref(),
                )?;
                if retained.state == RunState::Planned {
                    ledger.start()?;
                }
                if matches!(retained.state, RunState::Paused | RunState::Stopped) {
                    ledger.resume(None).map_err(|error| {
                        with_embedding_context(
                            error,
                            serde_json::json!({"run_id":retained.spec.run_id,"space":marker.space}),
                        )
                    })?;
                }
                for task in pending {
                    let generated = self.dispatch_embedding_task(
                        &ledger, &task, spec, &marker.expected_units, runtime,
                        TaskMaterialization {
                            probe: marker.operation == "embeddings_check",
                            corpus: marker.operation == "embeddings_sync",
                        },
                    ).map_err(|error| with_embedding_context(error,
                        serde_json::json!({"run_id":retained.spec.run_id,"space":marker.space})))?;
                    activity.generated += generated;
                    if generated > 0 {
                        activity.run_id = Some(retained.spec.run_id.clone());
                        activity.generated_hashes.extend(
                            ledger.inspect()?.tasks[&task.key]
                                .cache_outputs
                                .iter()
                                .map(|output| output.input_hash.clone()),
                        );
                    }
                }
            }
            if let Some(warning) = self.finish_embedding_job(&ledger)? {
                activity.warnings.push(warning);
            }
        }
        Ok(activity)
    }
    fn finish_embedding_job(&self, ledger: &JobLedger) -> Result<Option<String>> {
        super::remote::finish_provider_job(ledger)
    }
    fn verify_embedding_send_inputs(
        &self,
        task: &TaskSpec,
        spec: &SpaceSpec,
        expected: &[RenderedUnit],
    ) -> Result<()> {
        let bytes = crate::changes::prepare::read_bounded(&self.fs, &task.input.path, 256 * 1024)?
            .ok_or_else(|| {
                fail(
                    ErrorCode::RecoveryRequired,
                    "embedding descriptor missing before dispatch",
                )
            })?;
        if bytes.len() as u64 != task.input.byte_len
            || Blake3Hash::digest(&bytes) != task.input.hash
        {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "embedding descriptor changed before dispatch",
            ));
        }
        let input: RemoteInput = serde_json::from_slice(&bytes)
            .map_err(|_| WikiError::invalid("embedding descriptor invalid"))?;
        if crate::graph::packet::canonical_json(&input)? != bytes {
            return Err(WikiError::invalid("embedding descriptor is noncanonical"));
        }
        let RemoteOperation::Embed { inputs, .. } = input.operation else {
            return Err(WikiError::invalid("embedding descriptor operation differs"));
        };
        let hashes: BTreeSet<_> = inputs.iter().map(|input| &input.input_hash).collect();
        let retained: Vec<_> = expected
            .iter()
            .filter(|unit| hashes.contains(&unit.input_hash))
            .cloned()
            .collect();
        if retained.is_empty()
            || hashes
                .iter()
                .any(|hash| !retained.iter().any(|unit| &unit.input_hash == *hash))
        {
            return Err(fail(
                ErrorCode::FreshnessConflict,
                "embedding task lost its retained corpus units",
            ));
        }
        let writer = self.embedding_writer()?;
        if !self.embedding_targets_current(spec, &retained, &writer)? {
            return Err(fail(
                ErrorCode::FreshnessConflict,
                "embedding source changed before new dispatch; retained unknown attempt remains protected",
            ));
        }
        Ok(())
    }
    fn embedding_batches(
        &self,
        runtime: &EmbeddingRuntime<'_>,
        inputs: &[EmbeddingInput],
    ) -> Result<Vec<Vec<EmbeddingInput>>> {
        self.embedding_guard_batches(runtime, inputs, None, &[])
    }
    /// Count the complete receipt union: supplier guards, declared scope guards
    /// and the current Run guard added by receipt_plan_locked. Scope guards are
    /// never removed to make a task fit; current embedding Runs declare none.
    pub(super) fn embedding_guard_batches(
        &self,
        runtime: &EmbeddingRuntime<'_>,
        inputs: &[EmbeddingInput],
        bindings: Option<&BTreeMap<Blake3Hash, Vec<ReadDependency>>>,
        scope_guards: &[ReadDependency],
    ) -> Result<Vec<Vec<EmbeddingInput>>> {
        self.embedding_guard_batches_bounded(runtime, inputs, bindings, scope_guards, None)
    }
    pub(super) fn embedding_supplier_batches(
        &self,
        runtime: &EmbeddingRuntime<'_>,
        inputs: &[EmbeddingInput],
        bindings: &BTreeMap<Blake3Hash, Vec<ReadDependency>>,
        units: &[RenderedUnit],
    ) -> Result<Vec<Vec<EmbeddingInput>>> {
        self.embedding_guard_batches_bounded(runtime, inputs, Some(bindings), &[], Some(units))
    }
    fn embedding_guard_batches_bounded(
        &self,
        runtime: &EmbeddingRuntime<'_>,
        inputs: &[EmbeddingInput],
        bindings: Option<&BTreeMap<Blake3Hash, Vec<ReadDependency>>>,
        scope_guards: &[ReadDependency],
        suppliers: Option<&[RenderedUnit]>,
    ) -> Result<Vec<Vec<EmbeddingInput>>> {
        let max_items =
            usize::from(runtime.service.service().max_batch_items.unwrap_or(32)).min(32);
        let max_bytes = runtime
            .service
            .service()
            .max_batch_bytes
            .unwrap_or(256 * 1024)
            .min(256 * 1024) as usize;
        let mut out = Vec::new();
        let mut batch = Vec::new();
        let mut declared = BTreeMap::new();
        for guard in scope_guards.iter().chain(inputs.iter().flat_map(|input| {
            bindings
                .and_then(|b| b.get(&input.input_hash))
                .into_iter()
                .flatten()
        })) {
            if declared
                .insert(&guard.path, &guard.expected)
                .is_some_and(|old| old != &guard.expected)
            {
                return Err(fail(
                    ErrorCode::FreshnessConflict,
                    "embedding paid scope has conflicting source or scope guards",
                ));
            }
        }
        let union = |batch: &[EmbeddingInput]| -> Result<usize> {
            let mut guards = BTreeMap::new();
            for guard in scope_guards.iter().chain(batch.iter().flat_map(|input| {
                bindings
                    .and_then(|b| b.get(&input.input_hash))
                    .into_iter()
                    .flatten()
            })) {
                if guards
                    .insert(&guard.path, &guard.expected)
                    .is_some_and(|old| old != &guard.expected)
                {
                    return Err(fail(
                        ErrorCode::FreshnessConflict,
                        "embedding batch has conflicting source or scope guards",
                    ));
                }
            }
            if bindings
                .is_some_and(|b| batch.iter().any(|input| !b.contains_key(&input.input_hash)))
            {
                return Err(fail(
                    ErrorCode::FreshnessConflict,
                    "embedding input lost its authenticated supplier guards",
                ));
            }
            Ok(guards.len().saturating_add(1))
        };
        let owner_count = |batch: &[EmbeddingInput]| -> Result<usize> {
            let Some(suppliers) = suppliers else {
                return Ok(0);
            };
            let mut owners = BTreeSet::new();
            for input in batch {
                let matches: BTreeSet<_> = suppliers
                    .iter()
                    .filter(|unit| unit.input_hash == input.input_hash)
                    .map(|unit| &unit.owner)
                    .collect();
                if matches.len() != 1 {
                    return Err(fail(
                        ErrorCode::FreshnessConflict,
                        "embedding input must retain exactly one chosen supplier",
                    ));
                }
                owners.extend(matches);
            }
            Ok(owners.len())
        };
        let encoded_size = |batch: &[EmbeddingInput]| -> Result<usize> {
            serde_json::to_vec(&serde_json::json!({"model":runtime.service.summary().model,
                "input":batch.iter().map(|i| &i.utf8).collect::<Vec<_>>(),
                "encoding_format":"float", "dimensions":65536}))
            .map(|bytes| bytes.len())
            .map_err(|_| WikiError::invalid("batch encoding"))
        };
        for input in inputs {
            // Preflight every singleton too, including one following a full
            // batch. Refuse the entire page before any task is dispatched.
            if max_items == 0
                || encoded_size(std::slice::from_ref(input))? > max_bytes
                || union(std::slice::from_ref(input))? > 128
                || owner_count(std::slice::from_ref(input))?
                    > indexed_embedding_inputs::PROOF_SCOPE_OWNERS
            {
                return Err(fail(
                    ErrorCode::BudgetExceeded,
                    "one rendered input exceeds encoded provider or receipt guard bound",
                ));
            }
            let mut candidate = batch.clone();
            candidate.push(input.clone());
            if candidate.len() > max_items
                || encoded_size(&candidate)? > max_bytes
                || union(&candidate)? > 128
                || owner_count(&candidate)? > indexed_embedding_inputs::PROOF_SCOPE_OWNERS
            {
                if batch.is_empty() {
                    return Err(fail(
                        ErrorCode::BudgetExceeded,
                        "one rendered input exceeds encoded provider bound",
                    ));
                }
                out.push(std::mem::take(&mut batch));
                batch.push(input.clone());
            } else {
                batch = candidate;
            }
        }
        if !batch.is_empty() {
            out.push(batch);
        }
        if out.len() > 4096 {
            return Err(fail(
                ErrorCode::BudgetExceeded,
                "embedding task ceiling exceeded",
            ));
        }
        Ok(out)
    }
    pub(crate) fn prior_accounting_for_new_run(&self) -> Result<PriorAccounting> {
        #[cfg(test)]
        indexed_embedding_inputs::attribution::prior_accounting_discovery();
        let mut prior = BTreeSet::new();
        let mut entries = 0;
        let paths = self.fs.root().scan_markdown_budgeted(&mut || {
            entries += 1;
            if entries > 65536 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "prior accounting discovery ceiling exceeded",
                ));
            }
            Ok(())
        })?;
        for path in paths {
            if !path.as_str().starts_with("runs/") || !path.as_str().ends_with("/run.md") {
                continue;
            }
            let Some(bytes) = crate::changes::prepare::read_bounded(
                &self.fs,
                &path,
                crate::changes::prepare::MAX_PAYLOAD_BYTES,
            )?
            else {
                continue;
            };
            if let Some(name) = path
                .as_str()
                .strip_prefix("runs/")
                .and_then(|s| s.strip_suffix("/run.md"))
                && !name.contains('/')
                && let Ok(id) = RecordId::new(name)
            {
                prior.insert(id);
            }
            if let Some(record) = crate::records::parse_note(&bytes).canonical
                && record.kind() == RecordKind::Run
            {
                prior.insert(record.id().clone());
            }
        }
        prior.extend(crate::vault::operational::RunStore::discover_existing(
            &self.fs,
            &self.vault_id,
            4096,
            &mut || Ok(()),
        )?);
        if prior.is_empty() {
            Ok(PriorAccounting::None)
        } else {
            Ok(PriorAccounting::Unknown{prior_run_ids:prior.into_iter().collect(),reason:"retained prior run costs may remain unknown; new explicit caller budget preserves those histories".into()})
        }
    }
    fn immutable_operational(
        &self,
        path: &VaultRelativePath,
        bytes: &[u8],
        writer: &WriterPermit,
    ) -> Result<()> {
        if let Some(old) = crate::changes::prepare::read_bounded(
            &self.fs,
            path,
            crate::changes::prepare::MAX_PAYLOAD_BYTES,
        )? {
            if old != bytes {
                return Err(fail(
                    ErrorCode::ContentConflict,
                    "immutable embedding job descriptor conflicts",
                ));
            }
            return Ok(());
        }
        let stage = self.fs.stage(path, bytes, writer)?;
        self.fs.replace(stage, &ExpectedState::Absent, writer)?;
        Ok(())
    }
    fn embedding_job(
        &self,
        spec: &SpaceSpec,
        batches: &[Vec<EmbeddingInput>],
        units: &[RenderedUnit],
        runtime: &EmbeddingRuntime<'_>,
        operation: &str,
        probe: bool,
    ) -> Result<(JobLedger, Vec<TaskSpec>)> {
        self.embedding_job_with_bindings(spec, batches, units, runtime, operation, probe, None)
    }
    fn embedding_job_with_bindings(
        &self,
        spec: &SpaceSpec,
        batches: &[Vec<EmbeddingInput>],
        units: &[RenderedUnit],
        runtime: &EmbeddingRuntime<'_>,
        operation: &str,
        probe: bool,
        paid_bindings: Option<&BTreeMap<Blake3Hash, Vec<ReadDependency>>>,
    ) -> Result<(JobLedger, Vec<TaskSpec>)> {
        self.remote_gate(runtime)?;
        let writer = self.embedding_writer()?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let normalized_inputs =
            if paid_bindings.is_none() && catalog.operation_state()?.is_some() && !units.is_empty()
            {
                let paths = Self::embedding_owner_paths(units);
                let mut inputs = indexed_embedding_inputs::materialize(
                    &catalog,
                    &spec.settings,
                    Some(&paths),
                    &VerificationBudget::default(),
                )?;
                if !same_units(units, &inputs.units) {
                    return Err(fail(
                        ErrorCode::FreshnessConflict,
                        "embedding inputs changed before task guards were frozen",
                    ));
                }
                inputs.recheck(&catalog)?;
                Some(inputs)
            } else {
                None
            };
        for directory in [".wiki/state/embedding-inputs", ".wiki/state/embedding-jobs"] {
            self.fs
                .ensure_directory(&VaultRelativePath::new(directory)?, &writer)?;
        }
        let space = spec.id()?;
        let store = VectorStore::open(&self.fs, Some(&writer))?;
        store.prepare_space(spec)?;
        let state = store.space(&space)?.expect("prepared space");
        let mut tasks = Vec::new();
        let mut guard_entries = 0usize;
        let mut guard_bytes = 0usize;
        for batch in batches {
            let remote = RemoteInput {
                version: 1,
                operation: RemoteOperation::Embed {
                    inputs: batch.clone(),
                    expected_dimensions: if operation == "embeddings_sync" {
                        spec.dimensions
                    } else {
                        state.actual_dimensions
                    },
                    representation_fingerprint: space.clone(),
                },
            };
            let bytes = crate::graph::packet::canonical_json(&remote)?;
            let hash = Blake3Hash::digest(&bytes);
            let path = VaultRelativePath::new(format!(
                ".wiki/state/embedding-inputs/{}.json",
                hash.as_str().trim_start_matches("blake3:")
            ))?;
            self.immutable_operational(&path, &bytes, &writer)?;
            let fp = crate::providers::wire::task_fingerprints(runtime.service, &remote)?;
            let hashes = batch.iter().map(|i| &i.input_hash).collect::<BTreeSet<_>>();
            let mut guards = BTreeMap::new();
            if let Some(source_bindings) = paid_bindings.or_else(|| {
                normalized_inputs
                    .as_ref()
                    .map(|inputs| &inputs.source_bindings)
            }) {
                for hash in &hashes {
                    for guard in source_bindings.get(*hash).ok_or_else(|| {
                        fail(
                            ErrorCode::FreshnessConflict,
                            "embedding input lost its authenticated owner guards",
                        )
                    })? {
                        if let Some(old) = guards.get(&guard.path) {
                            if old != &guard.expected {
                                return Err(fail(
                                    ErrorCode::FreshnessConflict,
                                    "embedding batch has conflicting source guards",
                                ));
                            }
                            continue;
                        }
                        let bytes = std::mem::size_of::<ReadDependency>()
                            + guard.path.as_str().len()
                            + match &guard.expected {
                                ExpectedState::Absent => 0,
                                ExpectedState::Hash(hash) => hash.as_str().len(),
                            };
                        guard_entries = guard_entries
                            .checked_add(1)
                            .filter(|n| *n <= 65_536)
                            .ok_or_else(|| {
                                fail(
                                    ErrorCode::BudgetExceeded,
                                    "embedding task guard count exhausted",
                                )
                            })?;
                        guard_bytes = guard_bytes
                            .checked_add(bytes)
                            .filter(|n| *n <= 64 * 1024 * 1024)
                            .ok_or_else(|| {
                                fail(
                                    ErrorCode::BudgetExceeded,
                                    "embedding task guard bytes exhausted",
                                )
                            })?;
                        guards.insert(guard.path.clone(), guard.expected.clone());
                    }
                }
            } else {
                for unit in units.iter().filter(|u| hashes.contains(&u.input_hash)) {
                    guards.insert(
                        unit.owner.clone(),
                        ExpectedState::Hash(unit.source_hash.clone()),
                    );
                }
            }
            // Receipt publication admits 128 read preconditions and adds the
            // current Run guard to this task's exact source-guard union.
            // Refuse before dispatch rather than retain an unpublishable paid
            // response. Do not weaken or discard owner guards to fit.
            if guards.len() > 127 {
                return Err(fail(
                    ErrorCode::BudgetExceeded,
                    "embedding batch has too many source dependencies to publish its receipt safely",
                ));
            }
            let mut task = TaskSpec {
                key: Blake3Hash::digest([]),
                stage: if probe {
                    TaskStage::Probe
                } else {
                    TaskStage::Embed
                },
                capability: Some(if probe {
                    Capability::Probe
                } else {
                    Capability::Embed
                }),
                priority: 0,
                dependencies: Vec::new(),
                input_hash: fp.input,
                prompt_hash: fp.prompt,
                schema_hash: fp.schema,
                model_hash: Some(fp.model),
                settings_hash: fp.settings,
                source_bindings: guards
                    .into_iter()
                    .map(|(path, expected)| ReadDependency { path, expected })
                    .collect(),
                input: BoundedPayloadRef {
                    path,
                    hash,
                    byte_len: bytes.len() as u64,
                },
            };
            task.key = jobs::tasks::task_key(&task)?;
            tasks.push(task);
        }
        let legacy_identity = (
            operation,
            &space,
            tasks.iter().map(|t| &t.key).collect::<Vec<_>>(),
            proof(units)?,
        );
        let invocation = Blake3Hash::digest(if self.embedding_marker_version()? == 2 {
            crate::graph::packet::canonical_json(&(
                "lwiki-normalized-embedding-job-v2",
                &legacy_identity,
            ))?
        } else {
            crate::graph::packet::canonical_json(&legacy_identity)?
        });
        let mut marker_path = VaultRelativePath::new(format!(
            ".wiki/state/embedding-jobs/{}.json",
            invocation.as_str().trim_start_matches("blake3:")
        ))?;
        if let Some(bytes) = crate::changes::prepare::read_bounded(
            &self.fs,
            &marker_path,
            crate::changes::prepare::MAX_PAYLOAD_BYTES,
        )? {
            let marker: RunMarker = serde_json::from_slice(&bytes)
                .map_err(|_| WikiError::invalid("embedding run marker corrupt"))?;
            if marker.version != self.embedding_marker_version()?
                || marker.space != space
                || marker.operation != operation
                || marker.task_keys != tasks.iter().map(|t| t.key.clone()).collect::<Vec<_>>()
            {
                return Err(fail(
                    ErrorCode::ContentConflict,
                    "embedding job marker identity differs",
                ));
            }
            let ledger = JobLedger::new(
                self.fs.clone(),
                self.vault_id.clone(),
                marker.run_id,
                runtime.job_options.clone(),
            )?;
            let mut inspection = ledger.replay()?.inspection;
            for attempt in &inspection.attempts {
                if attempt.phase == AttemptPhase::OutputCommitted
                    && attempt.cache_outputs.is_empty()
                    && self.embedding_receipt_disposition(attempt)? == OutputDisposition::Rejected
                {
                    ledger.settle(&attempt.attempt)?;
                }
            }
            inspection = ledger.replay()?.inspection;
            super::remote::validate_retained_arguments(
                &inspection,
                &runtime.limits,
                runtime.deadline_ms,
                runtime.requested_limits.as_ref(),
            )?;
            let cache = VectorStore::open(&self.fs, None)?;
            let accounted_missing = !inspection.attempts.is_empty()
                && inspection
                    .attempts
                    .iter()
                    .all(|a| a.phase == AttemptPhase::Settled)
                && inspection
                    .attempts
                    .iter()
                    .flat_map(|a| &a.cache_outputs)
                    .map(|r| cache.verify_ref(r))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .any(|valid| !valid);
            let settled_rejection = inspection.attempts.iter().try_fold(false, |found, a| {
                if a.phase == AttemptPhase::Settled && a.cache_outputs.is_empty() {
                    Ok(found
                        || self.embedding_receipt_disposition(a)? == OutputDisposition::Rejected)
                } else {
                    Ok(found)
                }
            })?;
            let retired = accounted_missing
                || inspection
                    .attempts
                    .iter()
                    .all(|a| a.phase == AttemptPhase::Settled)
                    && inspection
                        .attempts
                        .iter()
                        .any(|a| a.cache_outputs.is_empty())
                    && (!probe || settled_rejection);
            if inspection.state == RunState::Failed {
                return Err(fail(
                    ErrorCode::RecoveryRequired,
                    "rejected embedding scope is terminal; changed inputs require a distinct scope",
                ));
            }
            if inspection.state == RunState::Completed || retired {
                marker_path = VaultRelativePath::new(format!(
                    ".wiki/state/embedding-jobs/{}-{}.json",
                    invocation.hex(),
                    uuid::Uuid::now_v7()
                ))?;
            } else {
                if inspection.state == RunState::Planned {
                    ledger.start()?;
                }
                if inspection.state == RunState::Paused || inspection.state == RunState::Stopped {
                    ledger.resume(None)?;
                }
                return Ok((ledger, tasks));
            }
        }
        let invocation = runtime
            .invocation
            .as_ref()
            .ok_or_else(|| WikiError::invalid("embedding operation allowance missing"))?;
        let now = invocation.started_at_utc_ms();
        let run_id = RecordId::new(format!("run_{}", uuid::Uuid::now_v7()))?;
        let mut run = RunSpec {
            version: 1,
            run_id: run_id.clone(),
            vault_id: self.vault_id.clone(),
            title: format!("Explicit {operation}"),
            created_at_utc_ms: now,
            deadline_utc_ms: invocation.deadline_utc_ms(),
            scope: RunScope {
                operation: operation.into(),
                question: None,
                exclusions: Vec::new(),
                source_snapshot: None,
                input_records: Vec::new(),
                read_preconditions: Vec::new(),
                profile_fingerprints: BTreeMap::from([(
                    runtime.service.summary().profile_id,
                    runtime.service.summary().profile_fingerprint,
                )]),
                scope_payload_hash: Some(proof(units)?),
            },
            config_fingerprint: runtime.service.summary().config_fingerprint,
            input_fingerprint: Blake3Hash::digest([]),
            limits: runtime.limits.clone(),
            tasks: tasks.clone(),
            prior_accounting: self.prior_accounting_for_new_run()?,
        };
        run.input_fingerprint = jobs::tasks::input_fingerprint(&run)?;
        let ledger = JobLedger::new(
            self.fs.clone(),
            self.vault_id.clone(),
            run_id.clone(),
            runtime.job_options.clone(),
        )?;
        ledger.create(&writer, run)?;
        let marker = RunMarker {
            version: self.embedding_marker_version()?,
            space,
            run_id,
            operation: operation.into(),
            task_keys: tasks.iter().map(|t| t.key.clone()).collect(),
            expected_units: units.to_vec(),
        };
        let marker_bytes =
            serde_json::to_vec(&marker).map_err(|_| WikiError::invalid("run marker encoding"))?;
        if marker_bytes.len() > crate::changes::prepare::MAX_PAYLOAD_BYTES {
            return Err(fail(
                ErrorCode::BudgetExceeded,
                "embedding recovery descriptor exceeds local bound",
            ));
        }
        self.immutable_operational(&marker_path, &marker_bytes, &writer)?;
        drop(writer);
        ledger.start()?;
        Ok((ledger, tasks))
    }
    fn commit_embedding_receipt(
        &self,
        ledger: &JobLedger,
        plan: MaterializationPlan,
    ) -> Result<()> {
        let writer = self.embedding_writer()?;
        let engine = ChangeEngine::new(self.fs.clone())?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let normalized = catalog.operation_state()?.is_some();
        if normalized {
            jobs::checkpoint::recover_job_active(&self.fs, &writer, &self.vault_id)?;
        }
        if normalized {
            let existing = jobs::checkpoint::inspect_for_publication(ledger)?;
            if let Some(actual) = existing
                .attempts
                .iter()
                .find(|attempt| attempt.attempt == plan.attempt)
                .and_then(|attempt| attempt.receipt.as_ref())
            {
                if jobs::checkpoint::receipt(&self.fs, actual)? != plan.receipt {
                    return Err(WikiError::invalid("retained embedding receipt differs"));
                }
                jobs::checkpoint::named_job_committed(
                    &self.fs, &self.vault_id, &plan.attempt.run_id,
                    jobs::checkpoint::JobPublicationKey::Receipt { receipt_id: plan.receipt.receipt_id.clone() },
                    &actual.path, &actual.hash,
                )?.ok_or_else(|| WikiError::invalid("normalized receipt lacks named committed publication; maintenance required"))?;
                drop(writer);
                ledger.settle(&plan.attempt)?;
                return Ok(());
            }
            if let Some((change, reference)) =
                jobs::checkpoint::retained_receipt_publication(&self.fs, ledger, &plan.receipt)?
            {
                drop(writer);
                ledger.outputs_committed(
                    &plan.attempt,
                    &change,
                    reference,
                    plan.receipt.outputs,
                    plan.receipt.cache_outputs,
                )?;
                ledger.settle(&plan.attempt)?;
                return Ok(());
            }
        }
        let prepared = if normalized {
            jobs::checkpoint::publish_job_draft(&self.fs, &writer, ledger, plan.draft)?
        } else {
            let prepared = engine.prepare(&writer, plan.draft)?.prepared;
            engine.apply(&writer, &prepared, &CatalogGraphValidator, &catalog)?;
            prepared
        };
        let receipt_path = VaultRelativePath::new(format!(
            "runs/{}/events/{}.md",
            plan.attempt.run_id, plan.receipt.receipt_id
        ))?;
        // Derive canonical receipt path from established run-event layout, never cache data.
        let bytes =
            crate::changes::prepare::read_bounded(&self.fs, &receipt_path, jobs::EVENT_MAX_BYTES)?
                .ok_or_else(|| WikiError::invalid("committed receipt missing"))?;
        let reference = DurableOutputRef {
            record: RecordRef {
                vault_id: self.vault_id.clone(),
                record_id: plan.receipt.receipt_id.clone(),
                expected_kind: RecordKind::RunEvent,
            },
            path: receipt_path,
            hash: Blake3Hash::digest(bytes),
        };
        drop(writer);
        ledger.outputs_committed(
            &plan.attempt,
            &prepared,
            reference,
            plan.receipt.outputs,
            plan.receipt.cache_outputs,
        )?;
        ledger.settle(&plan.attempt)?;
        Ok(())
    }
    fn verify_vector_receipt(
        &self,
        ledger: &JobLedger,
        attempt: &AttemptRef,
        refs: &[VectorCacheRef],
    ) -> Result<()> {
        let inspection = ledger.inspect()?;
        let paid = inspection
            .attempts
            .iter()
            .find(|a| &a.attempt == attempt)
            .ok_or_else(|| fail(ErrorCode::RecoveryRequired, "paid attempt missing"))?;
        let reference = paid
            .receipt
            .as_ref()
            .ok_or_else(|| fail(ErrorCode::RecoveryRequired, "validated receipt missing"))?;
        let receipt = jobs::checkpoint::receipt(&self.fs, reference)?;
        if paid.phase != AttemptPhase::Settled
            || receipt.attempt != *attempt
            || receipt.output_disposition != OutputDisposition::Validated
            || receipt.cache_outputs != refs
            || receipt.outputs != paid.outputs
            || paid.cache_outputs != refs
        {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "vector cache references lack exact settled validated receipt",
            ));
        }
        Ok(())
    }
    /// A terminal rejected service probe has no collection outputs or sibling
    /// work. Preserve its existing receipt/holds without applying the historical
    /// cache-only Embed terminalization rule to Probe authority.
    fn verify_rejected_embedding_probe(
        &self,
        ledger: &JobLedger,
        marker: &RunMarker,
        task: &TaskSpec,
        attempt: &AttemptInspection,
        spec: &SpaceSpec,
    ) -> Result<()> {
        let inspection = ledger.inspect()?;
        let attempt = inspection
            .attempts
            .iter()
            .find(|recorded| recorded.attempt == attempt.attempt)
            .ok_or_else(|| {
                fail(
                    ErrorCode::RecoveryRequired,
                    "rejected probe attempt missing",
                )
            })?;
        if marker.operation != "embeddings_check"
            || !marker.expected_units.is_empty()
            || inspection.tasks.len() != 1
            || task.stage != TaskStage::Probe
            || task.capability != Some(Capability::Probe)
            || !task.dependencies.is_empty()
            || !task.source_bindings.is_empty()
            || !attempt.outputs.is_empty()
            || !attempt.cache_outputs.is_empty()
            || inspection.attempts.iter().any(|a| {
                a.phase != AttemptPhase::Settled
                    || a.billing != BillingDisposition::ReleasedNotSent
                        && a.remote_exposure != RemoteExposure::TerminalConfirmed
            })
        {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "rejected embedding probe authority differs",
            ));
        }
        for guard in &inspection.spec.scope.read_preconditions {
            let current = crate::changes::prepare::read_bounded(
                &self.fs,
                &guard.path,
                crate::changes::prepare::MAX_PAYLOAD_BYTES,
            )?
            .map_or(ExpectedState::Absent, |bytes| {
                ExpectedState::Hash(Blake3Hash::digest(bytes))
            });
            if current != guard.expected {
                return Err(fail(
                    ErrorCode::FreshnessConflict,
                    "rejected probe global binding changed",
                ));
            }
        }
        let bytes = crate::changes::prepare::read_bounded(&self.fs, &task.input.path, 256 * 1024)?
            .ok_or_else(|| {
                fail(
                    ErrorCode::RecoveryRequired,
                    "rejected probe descriptor missing",
                )
            })?;
        if bytes.len() as u64 != task.input.byte_len
            || Blake3Hash::digest(&bytes) != task.input.hash
            || task.input_hash != task.input.hash
        {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "rejected probe descriptor changed",
            ));
        }
        let descriptor: RemoteInput = serde_json::from_slice(&bytes)
            .map_err(|_| WikiError::invalid("rejected probe descriptor invalid"))?;
        if descriptor.version != 1 || crate::graph::packet::canonical_json(&descriptor)? != bytes {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "rejected probe descriptor differs",
            ));
        }
        let RemoteOperation::Embed {
            inputs,
            representation_fingerprint,
            ..
        } = &descriptor.operation
        else {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "rejected probe operation differs",
            ));
        };
        if inputs.len() != 1
            || inputs[0] != spec.query("lwiki embedding check")?
            || representation_fingerprint != &marker.space
        {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "rejected probe scope differs",
            ));
        }
        let reference = attempt.receipt.as_ref().ok_or_else(|| {
            fail(
                ErrorCode::RecoveryRequired,
                "rejected probe receipt missing",
            )
        })?;
        let receipt = jobs::checkpoint::receipt(&self.fs, reference)?;
        let expected = jobs::checkpoint::receipt_plan(
            ledger,
            &attempt.attempt,
            OutputDisposition::Rejected,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )?
        .receipt;
        if receipt != expected {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "rejected probe receipt differs",
            ));
        }
        Ok(())
    }
    fn embedding_receipt_disposition(
        &self,
        attempt: &AttemptInspection,
    ) -> Result<OutputDisposition> {
        let reference = attempt.receipt.as_ref().ok_or_else(|| {
            fail(
                ErrorCode::RecoveryRequired,
                "settled embedding receipt missing",
            )
        })?;
        let receipt = jobs::checkpoint::receipt(&self.fs, reference)?;
        if receipt.attempt != attempt.attempt {
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "embedding receipt attempt differs",
            ));
        }
        Ok(receipt.output_disposition)
    }
    fn dispatch_embedding_task(
        &self,
        ledger: &JobLedger,
        task: &TaskSpec,
        spec: &SpaceSpec,
        expected: &[RenderedUnit],
        runtime: &EmbeddingRuntime<'_>,
        materialization: TaskMaterialization,
    ) -> Result<usize> {
        let TaskMaterialization { probe, corpus } = materialization;
        let inspection = ledger.replay()?.inspection;
        let previous = inspection
            .attempts
            .iter()
            .rev()
            .find(|a| a.attempt.task_key == task.key);
        if let Some(previous) = previous.filter(|a| {
            a.billing != BillingDisposition::ReleasedNotSent
                && matches!(
                    a.phase,
                    AttemptPhase::OutputCommitted | AttemptPhase::Settled
                )
        }) {
            let writer = self.embedding_writer()?;
            let mut store = VectorStore::open(&self.fs, Some(&writer))?;
            drop(writer);
            let disposition = self.embedding_receipt_disposition(previous)?;
            if previous
                .cache_outputs
                .iter()
                .all(|r| store.verify_ref(r).unwrap_or(false))
                && (!previous.cache_outputs.is_empty() || probe)
                && disposition == OutputDisposition::Validated
            {
                if previous.phase == AttemptPhase::OutputCommitted {
                    ledger.settle(&previous.attempt)?;
                }
                self.verify_vector_receipt(ledger, &previous.attempt, &previous.cache_outputs)?;
                store.confirm_refs(&previous.cache_outputs)?;
                ledger.finish_remote_task(
                    &task.key,
                    Vec::new(),
                    previous.cache_outputs.clone(),
                    |r| store.verify_ref(r),
                )?;
                ledger.remove_spool_after_verified_commit(&previous.attempt)?;
                return Ok(0);
            }
            return Err(fail(
                ErrorCode::RecoveryRequired,
                "settled vector cache output missing; explicit replacement task required",
            ));
        }
        let purpose = if probe {
            DispatchPurpose::Probe {
                role: ServiceRole::Embed,
            }
        } else {
            DispatchPurpose::Task
        };
        let new_dispatch = !previous.is_some_and(|p| p.phase == AttemptPhase::Received);
        if new_dispatch {
            super::remote::validate_retained_arguments(
                &inspection,
                &runtime.limits,
                runtime.deadline_ms,
                runtime.requested_limits.as_ref(),
            )?;
            self.remote_gate(runtime)?;
            if corpus && !probe {
                self.verify_embedding_send_inputs(task, spec, expected)?;
            }
        }
        let outcome = if let Some(previous) = previous.filter(|a| a.phase == AttemptPhase::Received)
        {
            let output = match runtime.dispatcher.decode_retained_classified(
                ledger,
                runtime.service,
                &task.key,
                purpose,
                &previous.attempt,
            ) {
                Ok(output) => output,
                Err(failure) => {
                    let invalid = matches!(&failure, RetainedDecodeFailure::InvalidResponse(_));
                    let error = failure.error();
                    if invalid {
                        let rejected = jobs::checkpoint::receipt_plan(
                            ledger,
                            &previous.attempt,
                            OutputDisposition::Rejected,
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        )?;
                        self.commit_embedding_receipt(ledger, rejected)?;
                        let _ =
                            ledger.pause(StopReason::Failed("retained_provider_response".into()));
                    }
                    return Err(with_embedding_context(
                        error,
                        serde_json::json!({"run_id":previous.attempt.run_id,"attempt":previous.attempt}),
                    ));
                }
            };
            DispatchOutcome {
                attempt: previous.attempt.clone(),
                spool: previous.spool.clone().ok_or_else(|| {
                    fail(
                        ErrorCode::RecoveryRequired,
                        "received spool reference absent",
                    )
                })?,
                materialization: ledger.materialization_plan(&previous.attempt)?,
                output,
            }
        } else {
            runtime.dispatcher.execute_with_invocation(ledger, runtime.service, &task.key, purpose,
                runtime.invocation.as_ref().ok_or_else(|| WikiError::invalid("embedding operation allowance missing"))?)
                .map_err(|failure| {
                    // Admission exhaustion must leave the existing Run
                    // explicitly amendable without replacing its paid scope.
                    if failure.error.code == ErrorCode::BudgetExceeded
                        && ledger.inspect().is_ok_and(|state| state.state == RunState::Running) {
                        let _ = ledger.pause(StopReason::Budget);
                    }
                    with_embedding_context(failure.error, serde_json::json!({
                        "run_id":inspection.spec.run_id,"attempt":failure.attempt,"spool":failure.spool,
                    }))
                })?
        };
        let materialized = (|| -> Result<_> {
            let writer = self.embedding_writer()?;
            let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
            let input_bytes =
                crate::changes::prepare::read_bounded(&self.fs, &task.input.path, 256 * 1024)?
                    .ok_or_else(|| WikiError::invalid("embedding descriptor missing"))?;
            if input_bytes.len() as u64 != task.input.byte_len
                || Blake3Hash::digest(&input_bytes) != task.input.hash
            {
                return Err(fail(
                    ErrorCode::RecoveryRequired,
                    "paid embedding descriptor binding changed",
                ));
            }
            let remote: RemoteInput = serde_json::from_slice(&input_bytes)
                .map_err(|_| WikiError::invalid("embedding descriptor invalid"))?;
            if crate::graph::packet::canonical_json(&remote)? != input_bytes {
                return Err(fail(
                    ErrorCode::RecoveryRequired,
                    "paid embedding descriptor is not canonical",
                ));
            }
            let RemoteOperation::Embed { inputs, .. } = remote.operation else {
                return Err(WikiError::invalid("embedding task operation differs"));
            };
            let input_hashes = inputs
                .iter()
                .map(|i| &i.input_hash)
                .collect::<BTreeSet<_>>();
            let target_expected = expected
                .iter()
                .filter(|u| input_hashes.contains(&u.input_hash))
                .cloned()
                .collect::<Vec<_>>();
            let valid = probe
                || !corpus
                || self.embedding_targets_current(spec, &target_expected, &writer)?;
            let mut store = VectorStore::open(&self.fs, Some(&writer))?;
            let space = store.prepare_space(spec)?;
            let refs = match outcome.output.clone() {
                ValidatedOutput::Embeddings { vectors, .. } => store.put_batch_staged(
                    &space,
                    &inputs
                        .iter()
                        .map(|i| i.input_hash.clone())
                        .collect::<Vec<_>>(),
                    &vectors,
                    corpus,
                    &proof(&target_expected)?,
                )?,
                ValidatedOutput::Probe {
                    role: ServiceRole::Embed,
                } if probe => Vec::new(),
                _ => {
                    return Err(fail(
                        ErrorCode::ProviderResponse,
                        "embedding dispatcher output differs",
                    ));
                }
            };
            drop(writer);
            Ok((
                valid,
                refs,
                store,
                space,
                target_expected,
                catalog,
                inputs.len(),
            ))
        })();
        let (valid, refs, mut store, space, target_expected, catalog, input_count) =
            match materialized {
                Ok(value) => value,
                Err(error) => {
                    // Only vector contract validation can reject a received provider output.
                    // Local cache/descriptor/writer failures preserve Received and its spool.
                    if error.code != ErrorCode::ProviderResponse {
                        return Err(with_embedding_context(
                            error,
                            serde_json::json!({"run_id":outcome.attempt.run_id,"attempt":outcome.attempt,"recoverable_paid_response":true}),
                        ));
                    }
                    let rejected = jobs::checkpoint::receipt_plan(
                        ledger,
                        &outcome.attempt,
                        OutputDisposition::Rejected,
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                    )?;
                    self.commit_embedding_receipt(ledger, rejected)?;
                    let _ = ledger.pause(StopReason::Failed(format!("{:?}", error.code)));
                    return Err(with_embedding_context(
                        error,
                        serde_json::json!({"run_id":outcome.attempt.run_id,"attempt":outcome.attempt,"paid_output_rejected":true}),
                    ));
                }
            };
        let disposition = if valid {
            OutputDisposition::Validated
        } else {
            OutputDisposition::Rejected
        };
        let plan = jobs::checkpoint::receipt_plan(
            ledger,
            &outcome.attempt,
            disposition,
            Vec::new(),
            if valid { refs.clone() } else { Vec::new() },
            Vec::new(),
        )?;
        let committed = self.commit_embedding_receipt(ledger, plan);
        if let Err(error) = committed {
            let targets_changed = if error.code == ErrorCode::FreshnessConflict && corpus {
                let writer = self.embedding_writer()?;
                !self.embedding_targets_current(spec, &target_expected, &writer)?
            } else {
                false
            };
            if targets_changed {
                let rejected = jobs::checkpoint::receipt_plan(
                    ledger,
                    &outcome.attempt,
                    OutputDisposition::Rejected,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )?;
                self.commit_embedding_receipt(ledger, rejected)?;
                let _ = ledger.pause(StopReason::InputsChanged);
                let mut e = fail(
                    ErrorCode::FreshnessConflict,
                    "paid embedding input changed; rejected receipt retained, stale membership withheld",
                );
                e.details =
                    serde_json::json!({"run_id":outcome.attempt.run_id,"attempt":outcome.attempt});
                return Err(e);
            }
            return Err(error);
        }
        if !valid {
            let _ = ledger.pause(StopReason::InputsChanged);
            return Err(fail(
                ErrorCode::FreshnessConflict,
                "paid embedding targets changed; settled rejected receipt retained",
            ));
        }
        self.verify_vector_receipt(ledger, &outcome.attempt, &refs)?;
        store.confirm_refs(&refs)?;
        ledger.finish_remote_task(&task.key, Vec::new(), refs.clone(), |r| store.verify_ref(r))?;
        ledger.remove_spool_after_verified_commit(&outcome.attempt)?;
        // Rebind fresh membership after own receipt changes; never the pre-HTTP generation.
        if corpus {
            let writer = self.embedding_writer()?;
            if catalog.operation_state()?.is_some() {
                // Reuse the original supplier subsets across bounded scopes.
                // Owner readiness is published by the coordinator, not this task.
                if !self.embedding_targets_current(spec, &target_expected, &writer)? {
                    return Err(fail(
                        ErrorCode::FreshnessConflict,
                        "embedding targets changed after receipt; current membership withheld",
                    ));
                }
            } else {
                let phase = Self::embedding_phase()?;
                let paths = Self::embedding_owner_paths(&target_expected);
                let mut fresh = self.embedding_inputs_bounded(
                    &spec.settings,
                    false,
                    Some(&writer),
                    Some(&paths),
                    &Self::embedding_phase_proof_budget(&phase)?,
                )?;
                if !same_units(&target_expected, fresh.units()) {
                    return Err(fail(
                        ErrorCode::FreshnessConflict,
                        "embedding targets changed after receipt; current membership withheld",
                    ));
                }
                let units = fresh.take_units();
                let snapshot = fresh.snapshot().clone();
                store.bind_read_budget(&phase)?;
                store.memberships_with_spec_checked(
                    &space,
                    &snapshot,
                    &units,
                    false,
                    None,
                    || {
                        fresh.recheck(&catalog)?;
                        phase.remaining_ms()?;
                        Ok(())
                    },
                )?;
            }
        }
        Ok(if new_dispatch { input_count } else { 0 })
    }
}

impl OfflineApp {
    /// Prepare a query once, outside final proof/retry. Cached vectors need no trust,
    /// credentials or runtime; a paid miss must reproduce the retained active space.
    fn require_active_space(&self, state: &SpaceState) -> Result<()> {
        if VectorStore::open(&self.fs, None)?
            .active()?
            .is_none_or(|active| {
                active.id != state.id
                    || active.spec != state.spec
                    || active.actual_dimensions != state.actual_dimensions
            })
        {
            return Err(fail(
                ErrorCode::FreshnessConflict,
                "active space changed immediately before emission",
            ));
        }
        Ok(())
    }
    fn embedding_query(
        &self,
        text: &str,
        runtime: Option<&EmbeddingRuntime<'_>>,
    ) -> Result<(SpaceState, Vec<f32>, bool)> {
        let scoped_runtime = runtime
            .map(EmbeddingRuntime::scoped_for_operation)
            .transpose()?;
        let runtime = scoped_runtime.as_ref();
        self.embedding_policy_consistent(runtime)?;
        let budget = retrieval::vectors::VectorReadBudget::new(
            std::time::Instant::now()
                + Duration::from_millis(VerificationBudget::default().max_elapsed_ms),
        )?;
        let store = VectorStore::open_bounded_snapshot(&self.fs, &budget)?;
        let state = store.active()?.ok_or_else(|| {
            fail(
                ErrorCode::CapabilityUnavailable,
                "no active embedding space; run explicit embeddings sync",
            )
        })?;
        if state.actual_dimensions.is_none() {
            return Err(fail(
                ErrorCode::CapabilityUnavailable,
                "active corpus dimension not established",
            ));
        }
        let input = state.spec.query(text)?;
        if let Some(vector) = store.vector(&state.id, &input.input_hash)? {
            return Ok((state, vector, false));
        }
        drop(store);
        if self.options.offline {
            return Err(fail(
                ErrorCode::OfflineUnavailable,
                "matching active-space query vector unavailable offline",
            ));
        }
        if self.options.dry_run {
            return Err(fail(
                ErrorCode::OfflineUnavailable,
                "dry-run query requires a matching cached vector",
            ));
        }
        let runtime = runtime.ok_or_else(|| {
            fail(
                ErrorCode::CapabilityUnavailable,
                "query cache miss requires independently authorized retained space service",
            )
        })?;
        state.spec.require_service(runtime.service)?;
        self.remote_gate(runtime)?;
        let (ledger, tasks) = self.embedding_job(
            &state.spec,
            &[vec![input.clone()]],
            &[],
            runtime,
            "embeddings_query",
            false,
        )?;
        let mut network = false;
        for task in tasks {
            network |= self.dispatch_embedding_task(
                &ledger,
                &task,
                &state.spec,
                &[],
                runtime,
                TaskMaterialization {
                    probe: false,
                    corpus: false,
                },
            )? > 0;
        }
        self.finish_embedding_job(&ledger)?;
        let store = VectorStore::open(&self.fs, None)?;
        let vector = store.vector(&state.id, &input.input_hash)?.ok_or_else(|| {
            fail(
                ErrorCode::ProviderResponse,
                "query vector cache unavailable after receipt",
            )
        })?;
        Ok((state, vector, network))
    }
    fn embedding_hitset(
        &self,
        reader: &ReaderSnapshot,
        text: &str,
        plan: &QueryPlan,
        state: &SpaceState,
        query: &[f32],
        scope: HitScope<'_>,
    ) -> Result<HitSet> {
        let HitScope { context, graph } = scope;
        let plan = retrieval::lexical::validate_plan(text, plan)?;
        let store = VectorStore::open(&self.fs, None)?;
        // Active pointer cannot switch between query generation and scoring.
        if store
            .active()?
            .is_none_or(|active| active.id != state.id || active.spec != state.spec)
        {
            return Err(fail(
                ErrorCode::FreshnessConflict,
                "active embedding space changed during query",
            ));
        }
        let scan = store.exact_stream(
            &state.id,
            query,
            || render::corpus_iter(reader, &state.spec.settings),
            &[TargetKind::Document],
            plan.limits.candidates,
            |unit| retrieval::fusion::unit_allowed(reader, unit, &plan.filters, context, None),
        )?;
        let coverage = scan.coverage;
        let dense = scan
            .hits
            .get(&TargetKind::Document)
            .cloned()
            .unwrap_or_default();
        let collapsed = retrieval::fusion::collapse_dense(&dense);
        let mut dense_hits = Vec::new();
        for (rank, (hit, passages)) in collapsed.into_iter().enumerate() {
            let document = reader
                .projection()
                .documents
                .iter()
                .find(|d| d.path == hit.owner)
                .ok_or_else(|| fail(ErrorCode::FreshnessConflict, "dense owner absent"))?;
            let primary_budget = if passages.len() > 1 {
                plan.limits.excerpt_bytes / 2
            } else {
                plan.limits.excerpt_bytes
            };
            let mut focused = retrieval::fusion::dense_hit_for_query(
                reader,
                document,
                &hit,
                rank + 1,
                primary_budget.max(1),
                text,
            )?;
            if let Some(second) = passages.get(1) {
                let remaining = plan
                    .limits
                    .excerpt_bytes
                    .saturating_sub(focused.excerpt.text.len());
                if remaining > 0 {
                    let extra = retrieval::fusion::dense_hit_for_query(
                        reader,
                        document,
                        second,
                        rank + 1,
                        remaining,
                        text,
                    )?
                    .excerpt;
                    if extra.span.end() <= focused.excerpt.span.start()
                        || focused.excerpt.span.end() <= extra.span.start()
                    {
                        focused.secondary_excerpts.push(extra);
                    }
                }
            }
            dense_hits.push(focused);
        }
        let mut lists = vec![dense_hits];
        let mut preserved_graph = None;
        let mut warnings = Vec::new();
        let mut omitted = 0;
        if plan.mode == SearchMode::Hybrid {
            let mut lexical_plan = plan.clone();
            lexical_plan.mode = SearchMode::Lexical;
            lexical_plan.cursor = None;
            lexical_plan.limits.hits = 50.min(plan.limits.candidates);
            let mut lexical_hits = Vec::new();
            loop {
                let result = if let Some(historical) = context {
                    retrieval::lexical::search_context(reader, text, &lexical_plan, historical)?
                } else {
                    retrieval::lexical::search(reader, text, &lexical_plan)?
                };
                lexical_hits.extend(result.hits);
                warnings.extend(result.warnings);
                omitted = omitted.max(result.omitted_candidates);
                if lexical_hits.len() >= plan.limits.candidates || result.next_cursor.is_none() {
                    break;
                }
                lexical_plan.cursor = result.next_cursor;
            }
            lists.push(lexical_hits);
        }
        if let Some(graph_plan) = graph {
            let result = self.embedding_graph_result_with_hybrid(
                reader,
                text,
                graph_plan,
                state,
                query,
                plan.mode == SearchMode::Hybrid,
            )?;
            let mut sources: BTreeMap<VaultRelativePath, SearchHit> = BTreeMap::new();
            for (rank, assertion) in result.assertions.iter().enumerate() {
                for evidence in assertion.support.iter().chain(&assertion.contradictions) {
                    let Some(document) = reader.projection().documents.iter().find(|d| {
                        d.owner_revision.as_ref() == Some(&evidence.source.source_revision)
                            && d.source_id.as_ref() == Some(&evidence.source.source_id)
                    }) else {
                        continue;
                    };
                    let allowed_unit = RenderedUnit {
                        unit_id: evidence.source.quote_hash.clone(),
                        target: TargetKind::Document,
                        owner: document.path.clone(),
                        target_id: document.record_id.clone(),
                        source_hash: document.hash.clone(),
                        source_span: Some(evidence.source.span),
                        dependency_fingerprint: evidence.source.quote_hash.clone(),
                        input_hash: evidence.source.quote_hash.clone(),
                        utf8: String::new(),
                    };
                    if !retrieval::fusion::unit_allowed(
                        reader,
                        &allowed_unit,
                        &plan.filters,
                        context,
                        None,
                    )? {
                        continue;
                    }
                    let synthetic = crate::retrieval::vectors::DenseHit {
                        unit_id: Blake3Hash::digest(evidence.source.quote_hash.as_str()),
                        target: TargetKind::Document,
                        owner: document.path.clone(),
                        target_id: document.record_id.clone(),
                        source_span: Some(evidence.source.span),
                        input_hash: evidence.source.quote_hash.clone(),
                        score: assertion.rrf_score,
                    };
                    let mut hit = retrieval::fusion::dense_hit_for_query(
                        reader,
                        document,
                        &synthetic,
                        rank + 1,
                        plan.limits.excerpt_bytes,
                        text,
                    )?;
                    hit.rank_contributions = vec![RankContribution {
                        channel: "graph_evidence".into(),
                        rank: rank + 1,
                        score: None,
                    }];
                    sources.entry(document.path.clone()).or_insert(hit);
                }
            }
            lists.push(sources.into_values().collect());
            warnings.extend(result.warnings.clone());
            omitted += result.coverage.omitted_candidates;
            preserved_graph = Some(result);
        }
        let mut hits = retrieval::fusion::fuse_hits(lists);
        let candidate_count = hits.len();
        let dense_overflow = usize::from(
            scan.owner_cap_reached_by_target
                .get(&TargetKind::Document)
                .copied()
                .unwrap_or(false),
        );
        omitted += dense_overflow + hits.len().saturating_sub(plan.limits.candidates);
        hits.truncate(plan.limits.candidates);
        let fingerprint = Blake3Hash::digest(crate::graph::packet::canonical_json(&(
            retrieval::cursor::fingerprint(text, &plan)?,
            &state.id,
            graph,
            context,
        ))?);
        let offset = retrieval::cursor::offset(
            reader,
            &fingerprint,
            plan.cursor.as_deref(),
            plan.limits.candidates,
        )?;
        let end = (offset + plan.limits.hits).min(hits.len());
        let next_cursor = if end < hits.len() {
            Some(retrieval::cursor::encode(reader, fingerprint, end)?)
        } else {
            None
        };
        let selected = hits
            .into_iter()
            .skip(offset)
            .take(end.saturating_sub(offset))
            .collect();
        warnings.push(format!(
            "semantic coverage {}/{} eligible units; {} missing, {} corrupt; exact O(Nd) scan",
            coverage.available_units,
            coverage.eligible_units,
            coverage.missing_units,
            coverage.corrupt_units
        ));
        if coverage.missing_units > 0 {
            warnings.push(
                "partial semantic coverage; stale descriptions and withdrawn support are excluded"
                    .into(),
            );
        }
        Ok(HitSet {
            network_used: false,
            graph: preserved_graph,
            hits: selected,
            next_cursor,
            truncated: end < candidate_count || omitted > 0 || coverage.missing_units > 0,
            candidate_count,
            omitted_candidates: omitted,
            snapshot: reader.snapshot().clone(),
            verification: reader.verification().clone(),
            dependency_fingerprint: Blake3Hash::digest(
                serde_json::to_vec(&reader.projection().dependencies)
                    .map_err(|_| WikiError::invalid("dependency encoding"))?,
            ),
            warnings,
        })
    }
    fn embedding_graph_result(
        &self,
        reader: &ReaderSnapshot,
        text: &str,
        plan: &GraphPlan,
        state: &SpaceState,
        query: &[f32],
    ) -> Result<GraphResult> {
        self.embedding_graph_result_with_hybrid(reader, text, plan, state, query, false)
    }
    fn embedding_graph_result_with_hybrid(
        &self,
        reader: &ReaderSnapshot,
        text: &str,
        plan: &GraphPlan,
        state: &SpaceState,
        query: &[f32],
        hybrid: bool,
    ) -> Result<GraphResult> {
        let plan = crate::graph::query::validate_plan(plan)?;
        let store = VectorStore::open(&self.fs, None)?;
        if store
            .active()?
            .is_none_or(|a| a.id != state.id || a.spec != state.spec)
        {
            return Err(fail(
                ErrorCode::FreshnessConflict,
                "active graph space changed",
            ));
        }
        let targets = match plan.strategy {
            GraphStrategy::Entity => vec![TargetKind::Entity],
            GraphStrategy::Relationship => vec![TargetKind::Assertion],
            _ => vec![TargetKind::Entity, TargetKind::Assertion],
        };
        let scan = store.exact_stream(
            &state.id,
            query,
            || render::corpus_iter(reader, &state.spec.settings),
            &targets,
            plan.limits.candidates,
            |unit| retrieval::fusion::unit_allowed(reader, unit, &plan.filters, None, Some(&plan)),
        )?;
        let coverage = scan.coverage;
        let mut seeds = Vec::new();
        let mut graph_coverage = GraphCoverage::default();
        for target in targets {
            let kind = if target == TargetKind::Entity {
                RecordKind::Entity
            } else {
                RecordKind::Assertion
            };
            let dense = scan.hits.get(&target).cloned().unwrap_or_default();
            let found = retrieval::fusion::graph_seeds(reader, &dense, kind)?;
            if target == TargetKind::Entity {
                graph_coverage.entity_candidates = found.len();
            } else {
                graph_coverage.assertion_candidates = found.len();
            }
            graph_coverage.omitted_candidates += usize::from(
                scan.owner_cap_reached_by_target
                    .get(&target)
                    .copied()
                    .unwrap_or(false),
            );
            seeds.extend(found);
        }
        if hybrid {
            let (lexical, lexical_coverage) =
                crate::graph::query::lexical_seed_lists(reader, text, &plan)?;
            graph_coverage.omitted_candidates += lexical_coverage.omitted_candidates;
            let mut fused: BTreeMap<RecordId, GraphSeed> = seeds
                .into_iter()
                .map(|s| (s.record_ref.record_id.clone(), s))
                .collect();
            for seed in lexical {
                if let Some(current) = fused.get_mut(&seed.record_ref.record_id) {
                    current.rank_contributions.extend(seed.rank_contributions);
                    current.rrf_score = retrieval::fusion::rrf(&current.rank_contributions);
                } else {
                    fused.insert(seed.record_ref.record_id.clone(), seed);
                }
            }
            seeds = fused.into_values().collect();
            // Per-target candidate cap remains 80 after channel fusion, before global seeds12.
            seeds.sort_by(|a, b| {
                b.rrf_score
                    .total_cmp(&a.rrf_score)
                    .then(a.record_ref.record_id.cmp(&b.record_ref.record_id))
            });
            let mut counts = BTreeMap::new();
            seeds.retain(|s| {
                let n = counts.entry(s.kind).or_insert(0usize);
                *n += 1;
                let keep = *n <= plan.limits.candidates;
                if !keep {
                    graph_coverage.omitted_candidates += 1;
                }
                keep
            });
            graph_coverage.entity_candidates = seeds
                .iter()
                .filter(|s| s.kind == RecordKind::Entity)
                .count();
            graph_coverage.assertion_candidates = seeds
                .iter()
                .filter(|s| s.kind == RecordKind::Assertion)
                .count();
        }
        let mut result = crate::graph::query::from_seeds(
            reader,
            seeds,
            graph_coverage,
            &plan,
            &format!("semantic:{}:{text}", state.id),
        )?;
        result.warnings.push(format!("semantic graph coverage {}/{} eligible units; {} missing/corrupt; no implicit extraction",coverage.available_units,coverage.eligible_units,coverage.missing_units));
        result.truncated |= coverage.missing_units > 0;
        Ok(result)
    }
    fn fallback_search(&self, text: &str, plan: &QueryPlan, no_sync: bool) -> Result<HitSet> {
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        if catalog.operation_state()?.is_some() {
            let reader = catalog
                .cached_query_snapshot(crate::catalog::query_types::QueryReadLimits::default())?;
            let hits = retrieval::lexical::search_catalog(&reader, text, plan)?;
            reader.verify_operations(&catalog)?;
            return Ok(hits);
        }
        for attempt in 0..2 {
            let reader = self.embedding_reader(no_sync)?;
            let mut hits = retrieval::lexical::search(&reader, text, plan)?;
            if no_sync || self.options.dry_run {
                return Ok(hits);
            }
            let fresh =
                Catalog::new(self.fs.clone(), self.vault_id.clone()).verified_snapshot(None)?;
            if fresh.canonical_equivalent(&reader) {
                hits.verification = fresh.verification().clone();
                return Ok(hits);
            }
            if attempt == 1 {
                return Err(fail(
                    ErrorCode::FreshnessConflict,
                    "lexical fallback changed before emission",
                ));
            }
        }
        unreachable!()
    }
    fn fallback_graph(&self, text: &str, plan: &GraphPlan, no_sync: bool) -> Result<GraphResult> {
        for attempt in 0..2 {
            let reader = self.embedding_reader(no_sync)?;
            let mut result = crate::graph::query::query(&reader, text, plan)?;
            if no_sync || self.options.dry_run {
                return Ok(result);
            }
            let fresh =
                Catalog::new(self.fs.clone(), self.vault_id.clone()).verified_snapshot(None)?;
            if fresh.canonical_equivalent(&reader) {
                result.verification = fresh.verification().clone();
                return Ok(result);
            }
            if attempt == 1 {
                return Err(fail(
                    ErrorCode::FreshnessConflict,
                    "lexical graph fallback changed before emission",
                ));
            }
        }
        unreachable!()
    }
    pub fn semantic_search(
        &self,
        text: &str,
        plan: &QueryPlan,
        runtime: Option<&EmbeddingRuntime<'_>>,
        no_sync: bool,
        fallback: bool,
        graph: Option<&GraphPlan>,
    ) -> Result<HitSet> {
        self.semantic_search_impl(text, plan, runtime, no_sync, fallback, graph, false)
    }
    pub fn semantic_search_selected(
        &self,
        text: &str,
        plan: &QueryPlan,
        runtime: Option<&EmbeddingRuntime<'_>>,
        fallback: bool,
    ) -> Result<HitSet> {
        self.semantic_search_impl(text, plan, runtime, true, fallback, None, true)
    }
    fn semantic_search_impl(
        &self,
        text: &str,
        plan: &QueryPlan,
        runtime: Option<&EmbeddingRuntime<'_>>,
        no_sync: bool,
        fallback: bool,
        graph: Option<&GraphPlan>,
        verify_selected: bool,
    ) -> Result<HitSet> {
        let scoped_runtime = runtime
            .map(EmbeddingRuntime::scoped_for_operation)
            .transpose()?;
        let runtime = scoped_runtime.as_ref();
        retrieval::lexical::validate_plan(text, plan)?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let normalized = catalog.operation_state()?.is_some();
        if normalized
            && (graph.is_some() || plan.filters.include_historical || plan.filters.include_proposed)
            || verify_selected && !normalized
        {
            return Err(fail(
                ErrorCode::CapabilityUnavailable,
                "normalized semantic search requires current documents without graph expansion",
            ));
        }
        let prepared = self.embedding_query(text, runtime);
        let (state, query, network) = match prepared {
            Ok(value) => value,
            Err(e)
                if fallback
                    && matches!(
                        e.code,
                        ErrorCode::CapabilityUnavailable
                            | ErrorCode::OfflineUnavailable
                            | ErrorCode::ProfileUntrusted
                    ) =>
            {
                let mut lexical = plan.clone();
                lexical.mode = SearchMode::Lexical;
                let mut hits = if verify_selected {
                    retrieval::selected_search::search(
                        &catalog,
                        text,
                        &lexical,
                        &VerificationBudget::default(),
                    )?
                } else {
                    self.fallback_search(text, &lexical, no_sync)?
                };
                hits.warnings
                    .push(format!("explicit lexical fallback: {:?}", e.code));
                return Ok(hits);
            }
            Err(e) => return Err(e),
        };
        if normalized {
            let mut hits = retrieval::indexed_semantic::search(
                &catalog,
                text,
                plan,
                &state,
                &query,
                verify_selected,
                &VerificationBudget::default(),
            )?;
            hits.network_used = network;
            if network {
                hits.warnings
                    .push("uncached query embedded by an accounted remote request".into());
            }
            return Ok(hits);
        }
        for attempt in 0..2 {
            let reader = self.embedding_reader(no_sync)?;
            let mut hits = self.embedding_hitset(
                &reader,
                text,
                plan,
                &state,
                &query,
                HitScope {
                    context: None,
                    graph,
                },
            )?;
            hits.network_used = network;
            if let Some(graph) = &mut hits.graph {
                graph.network_used = network;
            }
            if network {
                hits.warnings
                    .push("uncached query embedded by an accounted remote request".into());
            }
            if no_sync || self.options.dry_run {
                self.require_active_space(&state)?;
                return Ok(hits);
            }
            let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
            let final_reader = catalog.verified_snapshot(None);
            match final_reader {
                Ok(fresh) if fresh.canonical_equivalent(&reader) => {
                    hits.verification = fresh.verification().clone();
                    self.require_active_space(&state)?;
                    return Ok(hits);
                }
                Ok(_)
                | Err(WikiError {
                    code: ErrorCode::FreshnessConflict,
                    ..
                }) if attempt == 0 => continue,
                Err(e) => return Err(e),
                _ => {
                    return Err(fail(
                        ErrorCode::FreshnessConflict,
                        "semantic membership changed before emission",
                    ));
                }
            }
        }
        Err(fail(
            ErrorCode::FreshnessConflict,
            "semantic read failed final proof",
        ))
    }
    pub fn semantic_graph(
        &self,
        text: &str,
        plan: &GraphPlan,
        runtime: Option<&EmbeddingRuntime<'_>>,
        no_sync: bool,
        fallback: bool,
    ) -> Result<GraphResult> {
        let scoped_runtime = runtime
            .map(EmbeddingRuntime::scoped_for_operation)
            .transpose()?;
        let runtime = scoped_runtime.as_ref();
        let prepared = self.embedding_query(text, runtime);
        let (state, query, network) = match prepared {
            Ok(value) => value,
            Err(e)
                if fallback
                    && matches!(
                        e.code,
                        ErrorCode::CapabilityUnavailable
                            | ErrorCode::OfflineUnavailable
                            | ErrorCode::ProfileUntrusted
                    ) =>
            {
                let mut lexical = plan.clone();
                lexical.seed_mode = GraphSeedMode::Lexical;
                let mut result = self.fallback_graph(text, &lexical, no_sync)?;
                result
                    .warnings
                    .push(format!("explicit lexical graph fallback: {:?}", e.code));
                return Ok(result);
            }
            Err(e) => return Err(e),
        };
        for attempt in 0..2 {
            let reader = self.embedding_reader(no_sync)?;
            let mut result = self.embedding_graph_result(&reader, text, plan, &state, &query)?;
            result.network_used = network;
            if network {
                result
                    .warnings
                    .push("uncached graph query embedded by an accounted remote request".into());
            }
            if no_sync || self.options.dry_run {
                self.require_active_space(&state)?;
                return Ok(result);
            }
            let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
            match catalog.verified_snapshot(None) {
                Ok(fresh) if fresh.canonical_equivalent(&reader) => {
                    result.verification = fresh.verification().clone();
                    self.require_active_space(&state)?;
                    return Ok(result);
                }
                Ok(_)
                | Err(WikiError {
                    code: ErrorCode::FreshnessConflict,
                    ..
                }) if attempt == 0 => continue,
                Err(e) => return Err(e),
                _ => {
                    return Err(fail(
                        ErrorCode::FreshnessConflict,
                        "semantic graph changed before emission",
                    ));
                }
            }
        }
        Err(fail(
            ErrorCode::FreshnessConflict,
            "semantic graph failed final proof",
        ))
    }
    fn context_selection_signals(
        &self,
        reader: &ReaderSnapshot,
        request: &ContextRequest,
        hits: &HitSet,
        state: &SpaceState,
        query: &[f32],
    ) -> Result<ContextSelectionSignals> {
        let mut signals = ContextSelectionSignals::default();
        if request.target == ContextTarget::Graph
            || !matches!(
                request.documents.mode,
                SearchMode::Semantic | SearchMode::Hybrid
            )
        {
            return Ok(signals);
        }
        self.require_active_space(state)?;
        let store = VectorStore::open(&self.fs, None)?;
        let mut scanned_bytes = 0usize;
        let mut scanned_units = 0usize;
        let vector_width = query
            .len()
            .checked_mul(std::mem::size_of::<f32>())
            .ok_or_else(|| fail(ErrorCode::BudgetExceeded, "context vector width overflow"))?;
        let mut vector_bytes = 0usize;
        let mut missing = 0usize;
        let mut capped = false;
        let mut seen = BTreeSet::new();
        for hit in &hits.hits {
            if !seen.insert(hit.locator.path.clone()) {
                continue;
            }
            let document = reader
                .projection()
                .documents
                .iter()
                .find(|document| {
                    document.path == hit.locator.path && document.hash == hit.locator.observed_hash
                })
                .ok_or_else(|| {
                    fail(
                        ErrorCode::FreshnessConflict,
                        "context semantic owner changed",
                    )
                })?;
            if document.raw_text.len() > 1024 * 1024
                || scanned_bytes.saturating_add(document.raw_text.len()) > 4 * 1024 * 1024
            {
                capped = true;
                continue;
            }
            scanned_bytes += document.raw_text.len();
            for unit in render::render_document_iter(reader, document, &state.spec.settings)? {
                if scanned_units == 4096 {
                    capped = true;
                    break;
                }
                scanned_units += 1;
                let unit = unit?;
                if !retrieval::fusion::unit_allowed(
                    reader,
                    &unit,
                    &request.documents.filters,
                    Some(request.scope != ContextScope::Current),
                    None,
                )? {
                    continue;
                }
                let Some(span) = unit.source_span.filter(|span| !span.is_empty()) else {
                    continue;
                };
                if vector_width > 64 * 1024 * 1024 - vector_bytes {
                    capped = true;
                    break;
                }
                // Reserve even a missing/corrupt lookup. The source scan bound
                // alone cannot bound work for unusually wide vector spaces.
                vector_bytes += vector_width;
                let Some(vector) = store.vector(&state.id, &unit.input_hash)? else {
                    missing += 1;
                    continue;
                };
                signals.semantic.push(ContextSemanticCue {
                    owner: unit.owner,
                    observed_hash: unit.source_hash,
                    span,
                    cosine: retrieval::vectors::cosine(query, &vector)?,
                });
            }
            if scanned_units == 4096 || vector_width > 64 * 1024 * 1024 - vector_bytes {
                capped = true;
                break;
            }
        }
        signals.warnings.push(format!(
            "context reused {} cached unit affinities from {} inspected source bytes, {} units and {} reserved vector bytes; unit affinity is coarse ranking guidance, not passage confidence or answer completeness",
            signals.semantic.len(), scanned_bytes, scanned_units, vector_bytes));
        signals.semantic_complete = !capped
            && missing == 0
            && seen
                .iter()
                .all(|path| signals.semantic.iter().any(|cue| &cue.owner == path));
        if !signals.semantic_complete {
            signals.warnings.push("context unit coverage is incomplete; context selection uses bounded lexical source passages for every retrieved owner".into());
        }
        if missing > 0 {
            signals.warnings.push(format!("{missing} context unit vectors unavailable; those passages retain local selection guidance"));
        }
        if capped {
            signals.warnings.push("context semantic scoring reached its 1 MiB owner source, 4 MiB total source, 4096-unit or 64 MiB vector-read reservation cap; unscored passages retain local selection guidance".into());
        }
        self.require_active_space(state)?;
        Ok(signals)
    }

    pub fn semantic_context(
        &self,
        text: &str,
        request: &ContextRequest,
        runtime: Option<&EmbeddingRuntime<'_>>,
        no_sync: bool,
        fallback: bool,
    ) -> Result<ContextResult> {
        self.semantic_context_with_selection(
            text,
            request,
            runtime,
            no_sync,
            fallback,
            &retrieval::context_selection_packet::SelectionAction::Automatic,
        )
    }
    pub fn semantic_context_with_selection(
        &self,
        text: &str,
        request: &ContextRequest,
        runtime: Option<&EmbeddingRuntime<'_>>,
        no_sync: bool,
        fallback: bool,
        selection: &retrieval::context_selection_packet::SelectionAction,
    ) -> Result<ContextResult> {
        self.semantic_context_with_options(
            text,
            request,
            runtime,
            no_sync,
            fallback,
            &ContextOptions {
                selection: selection.clone(),
                ..Default::default()
            },
        )
    }

    pub fn semantic_context_with_options(
        &self,
        text: &str,
        request: &ContextRequest,
        runtime: Option<&EmbeddingRuntime<'_>>,
        no_sync: bool,
        fallback: bool,
        options: &ContextOptions,
    ) -> Result<ContextResult> {
        retrieval::context::validate_experimental_evidence(
            request,
            &options.selection,
            options.experimental_hybrid_lexical_evidence,
        )?;
        if options.experimental_hybrid_lexical_evidence && fallback {
            return Err(fail(
                ErrorCode::Usage,
                "experimental hybrid lexical evidence cannot use lexical discovery fallback",
            ));
        }
        let scoped_runtime = runtime
            .map(EmbeddingRuntime::scoped_for_operation)
            .transpose()?;
        let runtime = scoped_runtime.as_ref();
        let selection = &options.selection;
        retrieval::context::validate_request(text, request)?;
        retrieval::context::validate_selection_action(request, selection)?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let normalized = catalog.operation_state()?.is_some();
        if options.experimental_hybrid_lexical_evidence && !normalized {
            return Err(fail(
                ErrorCode::CapabilityUnavailable,
                "experimental hybrid lexical evidence requires a normalized selected catalog",
            ));
        }
        if normalized
            && (request.scope != ContextScope::IndexedDocuments
                || request.target != ContextTarget::Documents
                || request.graph.is_some()
                || !matches!(
                    request.documents.mode,
                    SearchMode::Semantic | SearchMode::Hybrid
                )
                || !matches!(
                    selection,
                    retrieval::context_selection_packet::SelectionAction::Automatic
                ))
        {
            return Err(fail(
                ErrorCode::CapabilityUnavailable,
                "normalized semantic context requires current indexed-documents with automatic selection",
            ));
        }
        let (state, query, network) = match self.embedding_query(text, runtime) {
            Ok(value) => value,
            Err(e)
                if fallback
                    && matches!(
                        e.code,
                        ErrorCode::CapabilityUnavailable
                            | ErrorCode::OfflineUnavailable
                            | ErrorCode::ProfileUntrusted
                    ) =>
            {
                let mut lexical = request.clone();
                lexical.documents.mode = SearchMode::Lexical;
                if let Some(g) = &mut lexical.graph {
                    g.seed_mode = GraphSeedMode::Lexical;
                }
                let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
                let writer = if no_sync || self.options.dry_run {
                    None
                } else {
                    Some(self.embedding_writer()?)
                };
                let mut context = retrieval::verification::context_with_options(
                    &catalog,
                    writer.as_ref(),
                    text,
                    &lexical,
                    options,
                )?;
                context
                    .warnings
                    .push(format!("explicit lexical context fallback: {:?}", e.code));
                return Ok(context);
            }
            Err(e) => return Err(e),
        };
        if normalized {
            let mut result = retrieval::indexed_semantic::context(
                &catalog, text, request, options, &state, &query,
            )?;
            result.network_used = network;
            if network {
                result
                    .warnings
                    .push("uncached context query embedded by an accounted remote request".into());
            }
            return Ok(result);
        }
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let writer = if no_sync || self.options.dry_run {
            None
        } else {
            Some(self.embedding_writer()?)
        };
        let mut result = retrieval::verification::context_with_scored_retrieval(
            &catalog,
            writer.as_ref(),
            text,
            request,
            options,
            |reader, normalized| {
                let hits = if matches!(
                    normalized.documents.mode,
                    SearchMode::Literal | SearchMode::Lexical
                ) {
                    retrieval::lexical::search_context(
                        reader,
                        text,
                        &normalized.documents,
                        normalized.scope != ContextScope::Current,
                    )?
                } else {
                    self.embedding_hitset(
                        reader,
                        text,
                        &normalized.documents,
                        &state,
                        &query,
                        HitScope {
                            context: Some(normalized.scope != ContextScope::Current),
                            graph: None,
                        },
                    )?
                };
                let graph = if normalized.target == ContextTarget::Documents {
                    None
                } else {
                    let plan = normalized.graph.as_ref().expect("normalized graph");
                    Some(if plan.seed_mode == GraphSeedMode::Semantic {
                        self.embedding_graph_result(reader, text, plan, &state, &query)?
                    } else {
                        crate::graph::query::query(reader, text, plan)?
                    })
                };
                let signals =
                    self.context_selection_signals(reader, normalized, &hits, &state, &query)?;
                Ok((hits, graph, signals))
            },
        )?;
        result.network_used = network;
        if network {
            result
                .warnings
                .push("uncached context query embedded by an accounted remote request".into());
        }
        self.require_active_space(&state)?;
        Ok(result)
    }
}
