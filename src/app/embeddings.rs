//! Explicit remote embedding work; all paid requests use production dispatcher and ledger.
use super::OfflineApp;
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
struct HitScope<'a> {
    context: Option<bool>,
    graph: Option<&'a GraphPlan>,
}
pub struct EmbeddingRuntime<'a> {
    pub service: &'a TrustedService,
    pub dispatcher: &'a Dispatcher,
    pub job_options: JobOptions,
    pub limits: LifetimeLimits,
    pub deadline_ms: u64,
}
#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingReport {
    pub space: Option<Blake3Hash>,
    pub active_space: Option<Blake3Hash>,
    /// Effective candidate or active-space settings used to calculate coverage.
    pub settings: Option<EmbeddingSettings>,
    pub coverage: Coverage,
    pub generated_inputs: usize,
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
        settings.validate()?;
        self.embedding_policy_consistent(runtime)?;
        let reader = self.embedding_reader(self.options.dry_run)?;
        let candidate = runtime
            .map(|r| SpaceSpec::from_service(r.service, settings.clone()))
            .transpose()?;
        let store = match VectorStore::open(&self.fs, None) {
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
        let units = spec
            .as_ref()
            .map(|s| render::corpus(&reader, &s.settings))
            .transpose()?
            .unwrap_or_default();
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
        let mut report=EmbeddingReport {space:space.clone(),active_space:active.map(|s|s.id),settings:spec.as_ref().map(|s|s.settings.clone()),coverage,generated_inputs:0,reused_inputs:0,published:false,dry_run:self.options.dry_run,network_used:false,run_id:None,warnings:vec!["local check does not establish provider compatibility; missing/corrupt cache is missing coverage".into()]};
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
            ledger.complete_run()?;
            report.warnings.push("explicit accounted probe validates one response; auto dimension remains corpus-unestablished".into());
        }
        Ok(report)
    }
    pub fn embeddings_sync(
        &self,
        settings: &EmbeddingSettings,
        runtime: &EmbeddingRuntime<'_>,
    ) -> Result<EmbeddingReport> {
        settings.validate()?;
        self.embedding_policy_consistent(Some(runtime))?;
        let spec = SpaceSpec::from_service(runtime.service, settings.clone())?;
        let space = spec.id()?;
        let reader = self.embedding_reader(self.options.dry_run)?;
        let units = render::corpus(&reader, settings)?;
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
        let old_active = store
            .as_ref()
            .map(VectorStore::active)
            .transpose()?
            .flatten();
        let mut unique = BTreeMap::new();
        for unit in &units {
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
            report
                .warnings
                .push("dry-run leaves index, jobs, cache, helpers and providers untouched".into());
            return Ok(report);
        }
        let store_ref = store.as_mut().expect("writable store");
        store_ref.prepare_space(&spec)?;
        drop(writer);
        drop(reader);
        drop(store);
        self.recover_embedding_jobs(&spec, runtime)?;
        let available = VectorStore::open(&self.fs, None)?;
        let mut pending = Vec::new();
        for input in missing {
            if available.vector(&space, &input.input_hash)?.is_none() {
                pending.push(input);
            } else {
                report.reused_inputs += 1;
            }
        }
        missing = pending;
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
                let generated=self.dispatch_embedding_task(&ledger,&task,&spec,&units,runtime,TaskMaterialization { probe:false, corpus:true }).map_err(|mut e|{e.details=serde_json::json!({"run_id":report.run_id,"generated_inputs":report.generated_inputs,"space":space,"context":e.details});e})?;
                report.generated_inputs += generated;
                report.network_used |= generated > 0;
            }
            ledger.complete_run()?;
        }
        let writer = self.embedding_writer()?;
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let fresh = catalog.verified_snapshot(Some(&writer))?;
        let current = render::corpus(&fresh, settings)?;
        let mut store = VectorStore::open(&self.fs, Some(&writer))?;
        let coverage = store.coverage(&space, &current)?;
        let complete = coverage.missing_units == 0;
        report.coverage = store.memberships_with_spec(
            &space,
            fresh.snapshot(),
            &current,
            complete,
            if complete { Some(&spec) } else { None },
        )?;
        report.published = complete;
        if let Some(old) = old_active.filter(|old| old.id != space)
            && !complete
        {
            let old_units = render::corpus(&fresh, &old.spec.settings)?;
            let old_coverage = store.memberships(&old.id, fresh.snapshot(), &old_units, false)?;
            report.warnings.push(format!(
                "replacement incomplete; retained active space coverage {}/{}",
                old_coverage.available_units, old_coverage.eligible_units
            ));
        }
        report.active_space = store.active()?.map(|s| s.id);
        if !complete {
            report.warnings.push(
                "current inputs missing/corrupt; replacement pointer remains unchanged".into(),
            );
        }
        Ok(report)
    }
    /// Reconcile previously paid outputs before constructing replacement tasks. The
    /// immutable marker retains the exact pre-request target proof, including decisions.
    fn recover_embedding_jobs(
        &self,
        spec: &SpaceSpec,
        runtime: &EmbeddingRuntime<'_>,
    ) -> Result<()> {
        let relative = VaultRelativePath::new(".wiki/state/embedding-jobs")?;
        let directory = self.fs.root().resolve(&relative)?;
        if !directory.exists() {
            return Ok(());
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
            if marker.version != 1 || marker.space != spec.id()? {
                continue;
            }
            let ledger = JobLedger::new(
                self.fs.clone(),
                self.vault_id.clone(),
                marker.run_id,
                runtime.job_options.clone(),
            )?;
            let inspection = ledger.replay()?.inspection;
            if inspection.state == RunState::Completed {
                continue;
            }
            for task in inspection
                .tasks
                .values()
                .filter(|t| t.state != TaskState::Completed)
            {
                let attempt = inspection
                    .attempts
                    .iter()
                    .rev()
                    .find(|a| a.attempt.task_key == task.spec.key);
                let Some(attempt) = attempt else { continue };
                if attempt.phase == AttemptPhase::Settled
                    && attempt.cache_outputs.is_empty()
                    && marker.operation != "embeddings_check"
                {
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
                        continue;
                    }
                }
                if matches!(
                    attempt.phase,
                    AttemptPhase::Received | AttemptPhase::OutputCommitted | AttemptPhase::Settled
                ) {
                    self.dispatch_embedding_task(
                        &ledger,
                        &task.spec,
                        spec,
                        &marker.expected_units,
                        runtime,
                        TaskMaterialization {
                            probe: marker.operation == "embeddings_check",
                            corpus: marker.operation == "embeddings_sync",
                        },
                    )?;
                } else if attempt.remote_exposure == RemoteExposure::PossiblyInFlight {
                    return Err(fail(
                        ErrorCode::RecoveryRequired,
                        "prior embedding send outcome remains unknown; no repeat send authorized",
                    ));
                }
            }
            if ledger
                .inspect()?
                .tasks
                .values()
                .all(|t| t.state == TaskState::Completed)
            {
                if matches!(
                    ledger.inspect()?.state,
                    RunState::Paused | RunState::Stopped
                ) {
                    ledger.resume(None)?;
                }
                ledger.complete_run()?;
            }
        }
        Ok(())
    }
    fn embedding_batches(
        &self,
        runtime: &EmbeddingRuntime<'_>,
        inputs: &[EmbeddingInput],
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
        for input in inputs {
            let mut candidate = batch.clone();
            candidate.push(input.clone());
            // Bound actual JSON string escapes plus fixed wire model/options overhead.
            let size=serde_json::to_vec(&serde_json::json!({"model":runtime.service.summary().model,"input":candidate.iter().map(|i|&i.utf8).collect::<Vec<_>>(),"encoding_format":"float","dimensions":65536})).map_err(|_|WikiError::invalid("batch encoding"))?.len();
            if candidate.len() > max_items || size > max_bytes {
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
        self.remote_gate(runtime)?;
        let writer = self.embedding_writer()?;
        for directory in [".wiki/state/embedding-inputs", ".wiki/state/embedding-jobs"] {
            self.fs
                .ensure_directory(&VaultRelativePath::new(directory)?, &writer)?;
        }
        let space = spec.id()?;
        let store = VectorStore::open(&self.fs, Some(&writer))?;
        store.prepare_space(spec)?;
        let state = store.space(&space)?.expect("prepared space");
        let mut tasks = Vec::new();
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
            for unit in units.iter().filter(|u| hashes.contains(&u.input_hash)) {
                guards.insert(
                    unit.owner.clone(),
                    ExpectedState::Hash(unit.source_hash.clone()),
                );
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
        let invocation = Blake3Hash::digest(crate::graph::packet::canonical_json(&(
            operation,
            &space,
            tasks.iter().map(|t| &t.key).collect::<Vec<_>>(),
            proof(units)?,
        ))?);
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
            if marker.version != 1
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
            let inspection = ledger.replay()?.inspection;
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
            let retired = accounted_missing
                || inspection
                    .attempts
                    .iter()
                    .all(|a| a.phase == AttemptPhase::Settled)
                    && inspection
                        .attempts
                        .iter()
                        .any(|a| a.cache_outputs.is_empty())
                    && !probe;
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
        let now = runtime.job_options.clock.read()?.utc_ms;
        let run_id = RecordId::new(format!("run_{}", uuid::Uuid::now_v7()))?;
        let mut run = RunSpec {
            version: 1,
            run_id: run_id.clone(),
            vault_id: self.vault_id.clone(),
            title: format!("Explicit {operation}"),
            created_at_utc_ms: now,
            deadline_utc_ms: now
                .checked_add(runtime.deadline_ms as i64)
                .ok_or_else(|| WikiError::invalid("deadline overflow"))?,
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
            version: 1,
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
        let prepared = engine.prepare(&writer, plan.draft)?.prepared;
        engine.apply(&writer, &prepared, &CatalogGraphValidator, &catalog)?;
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
            matches!(
                a.phase,
                AttemptPhase::OutputCommitted | AttemptPhase::Settled
            )
        }) {
            let writer = self.embedding_writer()?;
            let mut store = VectorStore::open(&self.fs, Some(&writer))?;
            drop(writer);
            if previous
                .cache_outputs
                .iter()
                .all(|r| store.verify_ref(r).unwrap_or(false))
                && (!previous.cache_outputs.is_empty() || probe)
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
                    let mut error = failure.error();
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
                    error.details = serde_json::json!({"run_id":previous.attempt.run_id,"attempt":previous.attempt});
                    return Err(error);
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
            runtime.dispatcher.execute(ledger,runtime.service,&task.key,purpose).map_err(|failure|{let mut e=failure.error;e.details=serde_json::json!({"run_id":inspection.spec.run_id,"attempt":failure.attempt,"spool":failure.spool});e})?
        };
        let materialized = (|| -> Result<_> {
            let writer = self.embedding_writer()?;
            let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
            let fresh = catalog.verified_snapshot(Some(&writer))?;
            let current = render::corpus(&fresh, &spec.settings)?;
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
            let valid = probe || !corpus || same_units(&target_expected, &current);
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
            drop(fresh);
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
                Err(mut error) => {
                    // Only vector contract validation can reject a received provider output.
                    // Local cache/descriptor/writer failures preserve Received and its spool.
                    if error.code != ErrorCode::ProviderResponse {
                        error.details = serde_json::json!({"run_id":outcome.attempt.run_id,"attempt":outcome.attempt,"recoverable_paid_response":true});
                        return Err(error);
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
                    error.details = serde_json::json!({"run_id":outcome.attempt.run_id,"attempt":outcome.attempt,"paid_output_rejected":true});
                    return Err(error);
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
                let fresh = catalog.verified_snapshot(Some(&writer))?;
                !same_units(&target_expected, &render::corpus(&fresh, &spec.settings)?)
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
            let fresh = catalog.verified_snapshot(Some(&writer))?;
            let units = render::corpus(&fresh, &spec.settings)?;
            if !same_units(&target_expected, &units) {
                return Err(fail(
                    ErrorCode::FreshnessConflict,
                    "embedding targets changed after receipt; current membership withheld",
                ));
            }
            store.memberships(&space, fresh.snapshot(), &units, false)?;
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
            .is_none_or(|active| active.id != state.id || active.spec != state.spec)
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
        self.embedding_policy_consistent(runtime)?;
        let store = VectorStore::open(&self.fs, None)?;
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
        ledger.complete_run()?;
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
            render::corpus_iter(reader, &state.spec.settings)?,
            &[TargetKind::Document],
            plan.limits.candidates,
            |unit| retrieval::fusion::unit_allowed(reader, unit, &plan.filters, context, None),
        )?;
        let coverage = scan.coverage;
        let dense_available = scan
            .available_by_target
            .get(&TargetKind::Document)
            .copied()
            .unwrap_or(0);
        let dense = scan
            .hits
            .get(&TargetKind::Document)
            .cloned()
            .unwrap_or_default();
        let collapsed = retrieval::fusion::collapse_dense(&dense);
        let mut dense_hits = Vec::new();
        for (rank, (hit, _passages)) in collapsed.into_iter().enumerate() {
            let document = reader
                .projection()
                .documents
                .iter()
                .find(|d| d.path == hit.owner)
                .ok_or_else(|| fail(ErrorCode::FreshnessConflict, "dense owner absent"))?;
            dense_hits.push(retrieval::fusion::dense_hit(
                reader,
                document,
                &hit,
                rank + 1,
                plan.limits.excerpt_bytes,
            )?);
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
                    let mut hit = retrieval::fusion::dense_hit(
                        reader,
                        document,
                        &synthetic,
                        rank + 1,
                        plan.limits.excerpt_bytes,
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
        let dense_overflow = dense_available.saturating_sub(dense.len());
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
            render::corpus_iter(reader, &state.spec.settings)?,
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
            graph_coverage.omitted_candidates += scan
                .available_by_target
                .get(&target)
                .copied()
                .unwrap_or(0)
                .saturating_sub(dense.len());
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
        retrieval::lexical::validate_plan(text, plan)?;
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
                let mut hits = self.fallback_search(text, &lexical, no_sync)?;
                hits.warnings
                    .push(format!("explicit lexical fallback: {:?}", e.code));
                return Ok(hits);
            }
            Err(e) => return Err(e),
        };
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
    pub fn semantic_context(
        &self,
        text: &str,
        request: &ContextRequest,
        runtime: Option<&EmbeddingRuntime<'_>>,
        no_sync: bool,
        fallback: bool,
    ) -> Result<ContextResult> {
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
                let mut context =
                    retrieval::verification::context(&catalog, writer.as_ref(), text, &lexical)?;
                context
                    .warnings
                    .push(format!("explicit lexical context fallback: {:?}", e.code));
                return Ok(context);
            }
            Err(e) => return Err(e),
        };
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let writer = if no_sync || self.options.dry_run {
            None
        } else {
            Some(self.embedding_writer()?)
        };
        let mut result = retrieval::verification::context_with_retrieval(
            &catalog,
            writer.as_ref(),
            text,
            request,
            &ContextOptions::default(),
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
                Ok((hits, graph))
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
