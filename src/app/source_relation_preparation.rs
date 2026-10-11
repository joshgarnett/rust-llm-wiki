//! One frozen, explicitly invoked source-only preparation Run. Descriptor caches
//! are reconstructable; the exact external manifest and lifetime scope are not.
use super::{
    OfflineApp,
    remote::{self, RemoteRuntime},
    source_relation_inputs::{
        InputLimits, MAX_CAPTURED_BYTES, MAX_CONTENT_BYTES, MAX_CONTROL_BYTES,
    },
    source_relation_planning::{RelationTaskPlan, plan_captured},
    source_relation_preparation_types::*,
};
use crate::{
    changes::{
        ReadDependency,
        prepare::{read_bounded, strict_json},
    },
    config::providers::TrustedService,
    domain::*,
    jobs::{self, diagnostics::DiagnosticKind, *},
    providers::{
        dispatcher::RetainedDecodeFailure,
        generation_task,
        invocation_budget::InvocationBudget,
        types::{DispatchPurpose, ValidatedOutput},
    },
    retrieval::source_relation_packet as packet,
    sources::{revision::canonical_path, selected::SelectedSourceBinding},
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

const OPERATION: &str = "prepare-source-relations-v1";
const SOURCE_HASH_ALLOWANCE: u64 = 14 * MAX_CAPTURED_BYTES as u64;
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn check(until: Instant) -> Result<()> {
    if Instant::now() >= until {
        return Err(budget(
            "source relation preparation invocation deadline exceeded",
        ));
    }
    Ok(())
}
pub(crate) fn recipe_hash() -> Result<Blake3Hash> {
    Ok(Blake3Hash::digest(crate::graph::packet::canonical_json(
        &(
            packet::RECIPE,
            packet::INPUT_SCHEMA,
            packet::RAW_SCHEMA,
            packet::FRAGMENT_BYTES,
            packet::INSTRUCTIONS,
            packet::output_schema(),
        ),
    )?))
}
fn input_limits(until: Instant) -> InputLimits {
    InputLimits {
        max_captured_bytes: MAX_CAPTURED_BYTES as u64,
        max_hash_bytes: SOURCE_HASH_ALLOWANCE,
        deadline: until,
    }
}

/// Captures are explicit caller-owned bounded maps, never a global SourceView.
/// Complete preflight has no descriptor/job/provider/credential write effects.
pub(crate) fn plan_manifest(
    fs: &VaultFs,
    vault: &RecordId,
    profile: &str,
    service: &TrustedService,
    max_output_tokens: u64,
    inputs: &[(SelectedSourceBinding, BTreeMap<VaultRelativePath, Vec<u8>>)],
    until: Instant,
) -> Result<Vec<u8>> {
    check(until)?;
    if inputs.is_empty() || inputs.len() > RUN_MAX_TASKS {
        return Err(budget(
            "relation manifest requires one to4096 complete Sources",
        ));
    }
    if service.summary().profile_id != profile {
        return Err(WikiError::new(
            ErrorCode::ProfileUntrusted,
            "relation planning profile differs from trusted service",
        ));
    }
    let mut entries = Vec::with_capacity(inputs.len());
    let mut sources = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for (source, captured) in inputs {
        check(until)?;
        if source.vault_id != *vault || !sources.insert(source.source_id.clone()) {
            return Err(WikiError::invalid(
                "relation planning Source is duplicate or foreign",
            ));
        }
        let plan = plan_captured(
            fs,
            source,
            captured,
            service,
            max_output_tokens,
            input_limits(until),
        )?;
        if !keys.insert(plan.generation.task.key.clone()) {
            return Err(WikiError::invalid(
                "relation planning task key is duplicate",
            ));
        }
        entries.push(SourceRelationManifestEntry {
            source: source.clone(),
            task: plan.generation.task,
        });
    }
    entries.sort_by(|a, b| a.source.source_id.cmp(&b.source.source_id));
    let manifest = SourceRelationManifest {
        version: 1,
        vault_id: vault.clone(),
        profile_id: profile.into(),
        config_fingerprint: service.summary().config_fingerprint,
        recipe_hash: recipe_hash()?,
        max_output_tokens,
        entries,
    };
    // Reuse the journal's finite writer; exact struct field order is deterministic
    // and the manifest commitment binds these bytes, not a semantic re-encoding.
    let bytes = jobs::events::bounded_json(&manifest, MANIFEST_MAX_BYTES)?;
    check(until)?;
    Ok(bytes)
}
fn validate_manifest(
    manifest: &SourceRelationManifest,
    vault: &RecordId,
    service: &TrustedService,
    until: Instant,
) -> Result<()> {
    check(until)?;
    let summary = service.summary();
    if manifest.version != 1
        || manifest.vault_id != *vault
        || manifest.profile_id != summary.profile_id
        || manifest.config_fingerprint != summary.config_fingerprint
        || manifest.recipe_hash != recipe_hash()?
        || summary.capability != Capability::Generate
        || manifest.entries.is_empty()
        || manifest.entries.len() > RUN_MAX_TASKS
    {
        return Err(WikiError::new(
            ErrorCode::SourceIntegrity,
            "relation manifest vault, recipe, service or coverage differs",
        ));
    }
    service.validate_generation_output_limit(manifest.max_output_tokens)?;
    let mut keys = BTreeSet::new();
    let mut previous = None;
    for entry in &manifest.entries {
        check(until)?;
        let task = &entry.task;
        if entry.source.vault_id != *vault
            || previous
                .as_ref()
                .is_some_and(|id| id >= &entry.source.source_id)
            || !keys.insert(task.key.clone())
            || task.key != jobs::tasks::task_key(task)?
            || task.stage != TaskStage::PrepareSourceRelations
            || task.capability != Some(Capability::Generate)
            || task.priority != 0
            || !task.dependencies.is_empty()
            || !(4..=5).contains(&task.source_bindings.len())
            || task.input_hash != task.input.hash
            || task.input.path
                != VaultRelativePath::new(format!(
                    ".wiki/cache/generation/inputs/{}.json",
                    task.input_hash.hex()
                ))?
            || task.input.byte_len == 0
            || task.input.byte_len > packet::MAX_REMOTE_INPUT_BYTES as u64
        {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "relation manifest requires ordered unique complete Source tasks",
            ));
        }
        previous = Some(entry.source.source_id.clone());
        let mut paths = BTreeSet::new();
        for dep in &task.source_bindings {
            if dep.path.as_str().len() > 4096
                || !matches!(dep.expected, ExpectedState::Hash(_))
                || !paths.insert(dep.path.clone())
            {
                return Err(WikiError::invalid(
                    "relation manifest task guards are not closed unique hashes",
                ));
            }
        }
        for (path, hash) in [
            (&entry.source.source_path, &entry.source.source_hash),
            (&entry.source.revision_path, &entry.source.revision_hash),
            (&entry.source.content_path, &entry.source.content_hash),
        ] {
            if !task.source_bindings.contains(&ReadDependency {
                path: path.clone(),
                expected: ExpectedState::Hash(hash.clone()),
            }) {
                return Err(WikiError::invalid(
                    "relation manifest selected binding differs from exact task guard",
                ));
            }
        }
        if !paths.contains(&VaultRelativePath::new("WIKI.md")?) {
            return Err(WikiError::invalid(
                "relation manifest lacks vault marker guard",
            ));
        }
    }
    Ok(())
}
fn capture_entry(
    fs: &VaultFs,
    entry: &SourceRelationManifestEntry,
    until: Instant,
) -> Result<BTreeMap<VaultRelativePath, Vec<u8>>> {
    let mut captured = BTreeMap::new();
    let mut remaining = MAX_CAPTURED_BYTES;
    for dep in &entry.task.source_bindings {
        check(until)?;
        let cap = if canonical_path(&dep.path) {
            MAX_CONTROL_BYTES
        } else if dep.path == entry.source.content_path {
            MAX_CONTENT_BYTES
        } else {
            MAX_CAPTURED_BYTES
        };
        let bytes = read_bounded(fs, &dep.path, cap.min(remaining))?.ok_or_else(|| {
            WikiError::new(
                ErrorCode::FreshnessConflict,
                "frozen relation input is missing",
            )
        })?;
        remaining = remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| budget("relation capture exceeds1MiB"))?;
        if dep.expected != ExpectedState::Hash(Blake3Hash::digest(&bytes)) {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "frozen relation Source bytes changed",
            ));
        }
        captured.insert(dep.path.clone(), bytes);
    }
    check(until)?;
    Ok(captured)
}
fn reconstruct(
    fs: &VaultFs,
    entry: &SourceRelationManifestEntry,
    manifest: &SourceRelationManifest,
    service: &TrustedService,
    until: Instant,
) -> Result<RelationTaskPlan> {
    let captured = capture_entry(fs, entry, until)?;
    let plan = plan_captured(
        fs,
        &entry.source,
        &captured,
        service,
        manifest.max_output_tokens,
        input_limits(until),
    )?;
    if plan.generation.task != entry.task {
        return Err(WikiError::new(
            ErrorCode::SourceIntegrity,
            "reconstructed complete relation task differs from frozen manifest",
        ));
    }
    check(until)?;
    Ok(plan)
}
fn tiny_spec(
    manifest: &SourceRelationManifest,
    hash: &Blake3Hash,
    run: &RecordId,
    runtime: &RemoteRuntime,
) -> Result<RunSpec> {
    let mut spec = RunSpec {
        version: 1,
        run_id: run.clone(),
        vault_id: manifest.vault_id.clone(),
        title: "Prepare complete Source relations".into(),
        created_at_utc_ms: runtime.created_at_utc_ms,
        deadline_utc_ms: runtime.deadline_utc_ms,
        scope: RunScope {
            operation: OPERATION.into(),
            question: None,
            exclusions: vec![],
            source_snapshot: None,
            input_records: vec![],
            read_preconditions: vec![],
            profile_fingerprints: BTreeMap::from([(
                manifest.profile_id.clone(),
                runtime.service.summary().profile_fingerprint,
            )]),
            scope_payload_hash: Some(hash.clone()),
        },
        config_fingerprint: manifest.config_fingerprint.clone(),
        input_fingerprint: Blake3Hash::digest([]),
        limits: runtime.limits.clone(),
        tasks: vec![],
        prior_accounting: PriorAccounting::None,
    };
    spec.input_fingerprint = jobs::tasks::input_fingerprint(&spec)?;
    Ok(spec)
}
fn sized_event(run: &RecordId, time: i64, payload: EventPayload) -> Result<usize> {
    let event = LedgerEvent {
        version: 1,
        run_id: run.clone(),
        event_id: RecordId::new("run_event_ffffffff-ffff-ffff-ffff-ffffffffffff")?,
        sequence: u64::MAX,
        previous_checksum: Some(Blake3Hash::digest([])),
        occurred_at_utc_ms: time,
        payload,
    };
    Ok(jobs::events::encode(&event)?.1.len())
}
fn partition(
    entries: &[SourceRelationManifestEntry],
    run: &RecordId,
    time: i64,
) -> Result<Vec<Vec<TaskSpec>>> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    for entry in entries {
        let mut next = current.clone();
        next.push(entry.task.clone());
        if next.len() > PARTITION_TASKS
            || sized_event(
                run,
                time,
                EventPayload::TasksAdded {
                    tasks: next.clone(),
                },
            )
            .is_err()
        {
            if current.is_empty() {
                return Err(budget(
                    "single frozen relation task exceeds journal event ceiling",
                ));
            }
            result.push(std::mem::take(&mut current));
            sized_event(
                run,
                time,
                EventPayload::TasksAdded {
                    tasks: vec![entry.task.clone()],
                },
            )?;
        }
        current.push(entry.task.clone());
    }
    if !current.is_empty() {
        result.push(current);
    }
    Ok(result)
}
fn preflight_run(
    fs: &VaultFs,
    spec: &RunSpec,
    manifest: &SourceRelationManifest,
    until: Instant,
) -> Result<()> {
    check(until)?;
    let bootstrap = JobLedger::bootstrap_plan(fs, spec)?;
    let bytes: usize = bootstrap
        .operations
        .iter()
        .filter_map(|o| o.proposed.as_ref())
        .map(Vec::len)
        .sum();
    if bytes > MANIFEST_MAX_BYTES {
        return Err(budget(
            "relation bootstrap exceeds selected job payload ceiling",
        ));
    }
    partition(&manifest.entries, &spec.run_id, spec.created_at_utc_ms)?;
    let mut final_state = jobs::initial_inspection(spec)?;
    final_state.state = RunState::Completed;
    final_state.last_event = Some(EventRef {
        event_id: RecordId::new("run_event_ffffffff-ffff-ffff-ffff-ffffffffffff")?,
        sequence: u64::MAX,
        checksum: Blake3Hash::digest([]),
    });
    // Successful output IDs always use this fixed prefix +64 hex digest; two
    // refs is the longest relation terminal shape (rejected tasks have none).
    let output_id = RecordId::new(format!("run_event_relations_{}", "f".repeat(64)))?;
    let output = DurableOutputRef {
        record: RecordRef {
            vault_id: manifest.vault_id.clone(),
            record_id: output_id.clone(),
            expected_kind: RecordKind::RunEvent,
        },
        path: crate::retrieval::source_relation_store::artifact_path(&spec.run_id, &output_id)?,
        hash: Blake3Hash::digest([]),
    };
    for entry in &manifest.entries {
        final_state.tasks.insert(
            entry.task.key.clone(),
            TaskInspection {
                spec: entry.task.clone(),
                state: TaskState::Completed,
                outputs: vec![output.clone(), output.clone()],
                cache_outputs: vec![],
                failure_code: None,
            },
        );
    }
    if jobs::checkpoint::run_bytes(&final_state)?.len() > MANIFEST_MAX_BYTES {
        return Err(budget(
            "worst complete relation Run checkpoint exceeds16MiB; no paid work admitted",
        ));
    }
    check(until)
}
fn membership(
    inspection: &LedgerInspection,
    manifest: &SourceRelationManifest,
    hash: &Blake3Hash,
    service: &TrustedService,
) -> Result<()> {
    if inspection.spec.vault_id != manifest.vault_id
        || inspection.spec.scope.operation != OPERATION
        || inspection.spec.scope.scope_payload_hash.as_ref() != Some(hash)
        || inspection.spec.config_fingerprint != manifest.config_fingerprint
        || !inspection.spec.tasks.is_empty()
        || !inspection.spec.scope.read_preconditions.is_empty()
        || !inspection.spec.scope.input_records.is_empty()
        || inspection.spec.scope.source_snapshot.is_some()
        || inspection.spec.scope.question.is_some()
        || !inspection.spec.scope.exclusions.is_empty()
        || inspection.spec.scope.profile_fingerprints.len() != 1
        || inspection
            .spec
            .scope
            .profile_fingerprints
            .get(&manifest.profile_id)
            != Some(&service.summary().profile_fingerprint)
    {
        return Err(WikiError::new(
            ErrorCode::SourceIntegrity,
            "retained relation Run scope differs from exact manifest",
        ));
    }
    let expected: BTreeMap<_, _> = manifest
        .entries
        .iter()
        .map(|e| (&e.task.key, &e.task))
        .collect();
    for (key, task) in &inspection.tasks {
        if expected.get(key).copied() != Some(&task.spec) {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "retained relation task differs from full frozen TaskSpec",
            ));
        }
    }
    Ok(())
}
fn report(
    run: &RecordId,
    hash: &Blake3Hash,
    manifest: &SourceRelationManifest,
    state: Option<&LedgerInspection>,
    dry: bool,
    warnings: Vec<String>,
) -> SourceRelationPreparationReport {
    let complete = state.map_or(0, |i| {
        i.tasks
            .values()
            .filter(|t| t.state == TaskState::Completed)
            .count()
    });
    let failed = state.map_or(0, |i| {
        i.tasks
            .values()
            .filter(|t| t.state == TaskState::Failed)
            .count()
    });
    SourceRelationPreparationReport {
        run_id: run.clone(),
        manifest_hash: hash.clone(),
        manifest_sources: manifest.entries.len(),
        admitted_tasks: state.map_or(0, |i| i.tasks.len()),
        completed: complete,
        failed,
        pending: manifest.entries.len() - complete - failed,
        dry_run: dry,
        warnings,
    }
}
fn retain_descriptor(app: &OfflineApp, plan: &RelationTaskPlan, until: Instant) -> Result<()> {
    check(until)?;
    let writer = WriterPermit::acquire(
        app.fs.root(),
        Duration::from_millis(app.options.lock_timeout_ms)
            .min(until.saturating_duration_since(Instant::now())),
    )?;
    generation_task::retain_input(&app.fs, &writer, &plan.generation)?;
    check(until)
}
fn semantic(error: &WikiError) -> bool {
    matches!(
        error.code,
        ErrorCode::ExtractionInvalid | ErrorCode::SourceIntegrity | ErrorCode::FreshnessConflict
    )
}
fn reject_paid(
    app: &OfflineApp,
    ledger: &JobLedger,
    runtime: &RemoteRuntime,
    attempt: &AttemptRef,
    until: Instant,
    warnings: &mut Vec<String>,
) -> Result<()> {
    check(until)?;
    // No credentials/send authority are requested by retained decoding. Missing
    // or incomplete diagnostics never justify deleting the full protected spool.
    match runtime.dispatcher.decode_retained(ledger,&runtime.service,&attempt.task_key,DispatchPurpose::Task,attempt) {
        Ok(ValidatedOutput::Generation {text,..}) => {
            check(until)?;
            match ledger.retain_diagnostic(attempt,DiagnosticKind::SemanticRejection,text.as_bytes(),text.len() as u64,"source_relation_rejected") {
                Ok(reference) => if reference.truncated {warnings.push("Rejected relation diagnostic is truncated; full protected response spool retained.".into());},
                Err(_) => warnings.push("Rejected relation diagnostic unavailable; full protected response spool retained.".into()),
            }
        }
        _ => warnings.push("Rejected relation text diagnostic unavailable; protected response spool and accounting retained.".into()),
    }
    check(until)?;
    let rejected = jobs::checkpoint::receipt_plan(
        ledger,
        attempt,
        OutputDisposition::Rejected,
        vec![],
        vec![],
        vec![],
    )?;
    check(until)?;
    jobs::checkpoint::settle_receipt(&app.fs, ledger, rejected)?;
    check(until)?;
    ledger.finish_rejected_remote_task(&attempt.task_key)?;
    warnings.push(format!("Relation Source task {} failed; rejected paid response retained, with no current annotation membership or retry.",attempt.task_key));
    check(until)
}
fn recover_paid(
    app: &OfflineApp,
    ledger: &JobLedger,
    runtime: &RemoteRuntime,
    previous: &AttemptInspection,
    until: Instant,
    warnings: &mut Vec<String>,
) -> Result<()> {
    check(until)?;
    if matches!(
        previous.phase,
        AttemptPhase::OutputCommitted | AttemptPhase::Settled
    ) && previous.outputs.is_empty()
    {
        // finish_rejected_remote_task authenticates exact committed zero-output
        // Rejected accounting; it refuses Unknown/Validated lookalikes.
        if previous.phase == AttemptPhase::OutputCommitted {
            ledger.settle(&previous.attempt)?;
        }
        ledger.finish_rejected_remote_task(&previous.attempt.task_key)?;
        warnings.push(
            "Previously rejected relation response stays failed and its spool remains retained."
                .into(),
        );
        return check(until);
    }
    match app.publish_source_relation_attempt(ledger, &previous.attempt, until, false) {
        Ok(_) => check(until),
        Err(error) if previous.phase == AttemptPhase::Received && previous.receipt.is_none() => {
            match runtime.dispatcher.decode_retained_classified(
                ledger,
                &runtime.service,
                &previous.attempt.task_key,
                DispatchPurpose::Task,
                &previous.attempt,
            ) {
                Err(RetainedDecodeFailure::InvalidResponse(_)) => {
                    reject_paid(app, ledger, runtime, &previous.attempt, until, warnings)
                }
                Ok(_) if semantic(&error) => {
                    reject_paid(app, ledger, runtime, &previous.attempt, until, warnings)
                }
                // Semantic codes only reject after retained evidence authenticates.
                // Missing/corrupt spool or an unclassified ProviderResponse preserves holds.
                _ => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}
fn restore_unpaid_descriptors(
    app: &OfflineApp,
    state: &LedgerInspection,
    manifest: &SourceRelationManifest,
    service: &TrustedService,
    until: Instant,
) -> Result<()> {
    for entry in &manifest.entries {
        let Some(task) = state.tasks.get(&entry.task.key) else {
            continue;
        };
        if matches!(task.state, TaskState::Completed | TaskState::Failed)
            || state.attempts.iter().any(|a| {
                a.attempt.task_key == entry.task.key
                    && a.billing != BillingDisposition::ReleasedNotSent
            })
        {
            continue;
        }
        let plan = reconstruct(&app.fs, entry, manifest, service, until)?;
        retain_descriptor(app, &plan, until)?;
    }
    check(until)
}
impl OfflineApp {
    pub(crate) fn run_source_relation_preparation(
        &self,
        manifest_bytes: &[u8],
        run_id: &RecordId,
        runtime: &RemoteRuntime,
        until: Instant,
        max_tasks_this_invocation: usize,
    ) -> Result<SourceRelationPreparationReport> {
        check(until)?;
        if manifest_bytes.is_empty()
            || manifest_bytes.len() > MANIFEST_MAX_BYTES
            || max_tasks_this_invocation == 0
            || max_tasks_this_invocation > RUN_MAX_TASKS
        {
            return Err(budget(
                "relation manifest or invocation task ceiling exceeded",
            ));
        }
        if self.options.stage_only {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "relation preparation cannot stage a paid Run",
            ));
        }
        let hash = Blake3Hash::digest(manifest_bytes);
        let mut options = runtime.job_options.clone();
        options.policy.dry_run |= self.options.dry_run;
        options.policy.offline |= self.options.offline;
        options.lock_timeout_ms = options
            .lock_timeout_ms
            .min(self.options.lock_timeout_ms)
            .min(
                until
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(u64::MAX as u128) as u64,
            );
        let ledger = JobLedger::new(
            self.fs.clone(),
            self.vault_id.clone(),
            run_id.clone(),
            options.clone(),
        )?;
        let run_path = VaultRelativePath::new(format!("runs/{run_id}/run.md"))?;
        let retained_run = read_bounded(&self.fs, &run_path, MANIFEST_MAX_BYTES)?.is_some();
        let checkpoint =
            VaultRelativePath::new(format!(".wiki/state/jobs/{run_id}/checkpoint.json"))?;
        let existing =
            retained_run || read_bounded(&self.fs, &checkpoint, METADATA_MAX_BYTES)?.is_some();
        let mut inspection = if existing {
            Some(ledger.inspect()?)
        } else {
            None
        };
        // The exact external byte commitment is compared BEFORE parsing. A
        // semantically equal reformatted manifest is still a different scope.
        if inspection
            .as_ref()
            .is_some_and(|i| i.spec.scope.scope_payload_hash.as_ref() != Some(&hash))
        {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "exact relation manifest bytes differ from retained Run scope",
            ));
        }
        let manifest: SourceRelationManifest = strict_json(manifest_bytes)?;
        validate_manifest(&manifest, &self.vault_id, &runtime.service, until)?;
        if let Some(i) = &inspection {
            membership(i, &manifest, &hash, &runtime.service)?;
        }
        let mut warnings = Vec::new();
        let new_spec = if !existing && !options.policy.offline {
            let mut spec = tiny_spec(&manifest, &hash, run_id, runtime)?;
            spec.prior_accounting = self.prior_accounting_for_new_run()?;
            preflight_run(&self.fs, &spec, &manifest, until)?;
            // Reject an unrepresentable complete manifest before the first paid
            // request or job/cache write, rather than omit a later bad entry.
            for entry in &manifest.entries {
                reconstruct(&self.fs, entry, &manifest, &runtime.service, until)?;
            }
            Some(spec)
        } else {
            None
        };
        if options.policy.dry_run {
            if options.policy.offline {
                warnings.push("Offline dry-run checked manifest and retained scope; Source capture/currentness was not revalidated.".into());
            }
            return Ok(report(
                run_id,
                &hash,
                &manifest,
                inspection.as_ref(),
                true,
                warnings,
            ));
        }
        if !existing {
            if options.policy.offline {
                return Err(WikiError::new(
                    ErrorCode::OfflineUnavailable,
                    "offline relation preparation needs an existing exact Run; no new job or paid work created",
                ));
            }
            let spec = new_spec
                .ok_or_else(|| WikiError::invalid("relation bootstrap preflight missing"))?;
            check(until)?;
            let writer = WriterPermit::acquire(
                self.fs.root(),
                Duration::from_millis(options.lock_timeout_ms)
                    .min(until.saturating_duration_since(Instant::now())),
            )?;
            ledger.create(&writer, spec)?;
            drop(writer);
            check(until)?;
            inspection = Some(ledger.inspect()?);
        }
        let mut invocation: Option<InvocationBudget> = None;
        let mut processed = 0usize;
        let mut verified_terminal = BTreeSet::new();
        loop {
            check(until)?;
            let state = ledger.inspect()?;
            membership(&state, &manifest, &hash, &runtime.service)?;
            // Terminal tasks are historical: authenticating them never opens a
            // changed Source or missing descriptor cache. No early Run finish.
            for task in state
                .tasks
                .values()
                .filter(|t| matches!(t.state, TaskState::Completed | TaskState::Failed))
            {
                if verified_terminal.contains(&task.spec.key) {
                    continue;
                }
                check(until)?;
                if task.state == TaskState::Failed {
                    ledger.finish_rejected_remote_task(&task.spec.key)?;
                } else {
                    ledger.finish_remote_task(
                        &task.spec.key,
                        task.outputs.clone(),
                        task.cache_outputs.clone(),
                        |_| Ok(false),
                    )?;
                }
                verified_terminal.insert(task.spec.key.clone());
            }
            let all_terminal = state.tasks.len() == manifest.entries.len()
                && state
                    .tasks
                    .values()
                    .all(|t| matches!(t.state, TaskState::Completed | TaskState::Failed));
            if all_terminal {
                check(until)?;
                // Final fresh historical proofs catch missing/tampered retained
                // outputs without re-opening their original Source/cache inputs.
                for task in state.tasks.values() {
                    check(until)?;
                    if task.state == TaskState::Failed {
                        ledger.finish_rejected_remote_task(&task.spec.key)?;
                    } else {
                        ledger.finish_remote_task(
                            &task.spec.key,
                            task.outputs.clone(),
                            task.cache_outputs.clone(),
                            |_| Ok(false),
                        )?;
                    }
                }
                if let Some(warning) = remote::finish_provider_job(&ledger)? {
                    warnings
                        .push(warning.replace("rejected embedding work", "rejected relation work"));
                }
                check(until)?;
                self.checkpoint_generation(&ledger)?;
                check(until)?;
                return Ok(report(
                    run_id,
                    &hash,
                    &manifest,
                    Some(&ledger.inspect()?),
                    false,
                    warnings,
                ));
            }
            if matches!(state.state, RunState::Completed | RunState::Failed) {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "relation Run ended before every frozen manifest entry was terminal",
                ));
            }
            if processed >= max_tasks_this_invocation {
                warnings.push("Relation preparation paused at the finite invocation task allowance; continue the same Run and exact manifest.".into());
                break;
            }
            let unfinished = manifest
                .entries
                .iter()
                .filter(|entry| {
                    state.tasks.get(&entry.task.key).is_some_and(|t| {
                        !matches!(t.state, TaskState::Completed | TaskState::Failed)
                    })
                })
                .min_by_key(|entry| {
                    !state.attempts.iter().any(|a| {
                        a.attempt.task_key == entry.task.key
                            && a.billing != BillingDisposition::ReleasedNotSent
                    })
                });
            if let Some(entry) = unfinished {
                let previous = state.attempts.iter().rev().find(|a| {
                    a.attempt.task_key == entry.task.key
                        && a.billing != BillingDisposition::ReleasedNotSent
                });
                if let Some(previous) = previous {
                    if !matches!(
                        previous.phase,
                        AttemptPhase::Received
                            | AttemptPhase::OutputCommitted
                            | AttemptPhase::Settled
                    ) {
                        return Err(WikiError::new(
                            ErrorCode::RecoveryRequired,
                            "relation attempt is unresolved; original accounting holds retained, no resend",
                        ));
                    }
                    recover_paid(self, &ledger, runtime, previous, until, &mut warnings)?;
                    processed += 1;
                    continue;
                }
                if options.policy.offline {
                    warnings.push("Offline preparation has pending Source tasks; no descriptors, tasks, credentials or provider requests were created.".into());
                    break;
                }
                let duration = runtime
                    .deadline_utc_ms
                    .checked_sub(runtime.created_at_utc_ms)
                    .and_then(|n| u64::try_from(n).ok())
                    .ok_or_else(|| budget("invalid relation runtime lifetime deadline"))?;
                remote::validate_retained_arguments(
                    &state,
                    &runtime.limits,
                    duration,
                    runtime.requested_limits.as_ref(),
                )?;
                restore_unpaid_descriptors(self, &state, &manifest, &runtime.service, until)?;
                check(until)?;
                if state.state == RunState::Planned {
                    ledger.start()?;
                } else if matches!(state.state, RunState::Paused | RunState::Stopped) {
                    ledger.resume(None)?;
                }
                check(until)?;
                // Shared allowance includes every new execute in this invocation;
                // existing Run limits/deadline/attempt history remain immutable.
                if invocation.is_none() {
                    let now = runtime.job_options.clock.read()?.utc_ms;
                    let remaining = until
                        .saturating_duration_since(Instant::now())
                        .as_millis()
                        .min(i64::MAX as u128) as i64;
                    let invocation_deadline = now
                        .checked_add(remaining)
                        .ok_or_else(|| budget("relation invocation deadline overflow"))?
                        .min(runtime.deadline_utc_ms);
                    invocation = Some(InvocationBudget::new(
                        runtime.limits.clone(),
                        now,
                        invocation_deadline,
                    )?);
                }
                check(until)?;
                match runtime.dispatcher.execute_with_invocation(
                    &ledger,
                    &runtime.service,
                    &entry.task.key,
                    DispatchPurpose::Task,
                    invocation
                        .as_ref()
                        .ok_or_else(|| WikiError::invalid("relation invocation budget missing"))?,
                ) {
                    Ok(outcome) => {
                        check(until)?;
                        let recorded = ledger
                            .inspect()?
                            .attempts
                            .into_iter()
                            .find(|a| a.attempt == outcome.attempt)
                            .ok_or_else(|| {
                                WikiError::new(
                                    ErrorCode::RecoveryRequired,
                                    "paid relation attempt missing",
                                )
                            })?;
                        recover_paid(self, &ledger, runtime, &recorded, until, &mut warnings)?;
                    }
                    Err(failure) => {
                        check(until)?;
                        if failure.disposition
                            == crate::providers::types::DispatchDisposition::Rejected
                            && failure.materialization.as_ref().is_some_and(|p| {
                                p.receipt.output_disposition == OutputDisposition::Rejected
                            })
                        {
                            let attempt = failure.attempt.as_ref().ok_or_else(|| {
                                WikiError::new(
                                    ErrorCode::RecoveryRequired,
                                    "rejected relation response lacks an attempt",
                                )
                            })?;
                            reject_paid(self, &ledger, runtime, attempt, until, &mut warnings)?;
                        } else {
                            return Err(failure.error);
                        }
                    }
                }
                check(until)?;
                processed += 1;
                continue;
            }
            if options.policy.offline {
                warnings.push("Offline preparation leaves unadmitted manifest entries untouched; no new paid work.".into());
                break;
            }
            // Only missing frozen entries are appended; acknowledged TasksAdded
            // frames are rediscovered from inspection after an interruption.
            let missing: Vec<_> = manifest
                .entries
                .iter()
                .filter(|e| !state.tasks.contains_key(&e.task.key))
                .cloned()
                .collect();
            let allowance = (max_tasks_this_invocation - processed).min(PARTITION_TASKS);
            let groups = partition(
                &missing[..missing.len().min(allowance)],
                run_id,
                state.spec.created_at_utc_ms,
            )?;
            let group = groups.first().ok_or_else(|| {
                WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "relation manifest has no admissible missing partition",
                )
            })?;
            let mut plans = Vec::new();
            for task in group {
                let entry = missing
                    .iter()
                    .find(|e| e.task == *task)
                    .ok_or_else(|| WikiError::invalid("relation partition membership differs"))?;
                plans.push(reconstruct(
                    &self.fs,
                    entry,
                    &manifest,
                    &runtime.service,
                    until,
                )?);
            }
            restore_unpaid_descriptors(self, &state, &manifest, &runtime.service, until)?;
            for plan in &plans {
                retain_descriptor(self, plan, until)?;
            }
            check(until)?;
            if state.state == RunState::Planned {
                ledger.start()?;
            } else if matches!(state.state, RunState::Paused | RunState::Stopped) {
                ledger.resume(None)?;
            }
            check(until)?;
            ledger.add_tasks(group.clone())?;
            check(until)?;
        }
        check(until)?;
        inspection = Some(ledger.inspect()?);
        if !options.policy.offline {
            self.checkpoint_generation(&ledger)?;
            check(until)?;
            inspection = Some(ledger.inspect()?);
        }
        Ok(report(
            run_id,
            &hash,
            &manifest,
            inspection.as_ref(),
            false,
            warnings,
        ))
    }
}

#[cfg(test)]
#[path = "source_relation_preparation_tests.rs"]
pub(crate) mod tests;
