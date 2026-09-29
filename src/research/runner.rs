//! Recovery-aware orchestration over one immutable research genesis and lifetime ledger.
use super::{acquire, frontier, gaps, inspection, plan, report, stages, synthesis, types::*};
use crate::{
    app::OfflineApp,
    changes::PreparedChange,
    config::providers::TrustedService,
    domain::*,
    graph::{self, ExportRequest, packet::canonical_json},
    jobs::*,
    providers::{
        dispatcher::{ResearchDispatchOutcome, ResearchDispatchWork},
        public_fetch::{self, PublicFetchOutcome},
        search_wire::SearchLead,
        types::{
            DispatchFailure, DispatchOutcome, DispatchPurpose, RemoteInput, RemoteOperation,
            RetryDecision, ValidatedOutput,
        },
    },
    sources::{CitationScope, SourceView},
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

const INPUT_CAP: usize = 256 * 1024;
const SOURCE_CAP: usize = 64 * 1024 * 1024;

fn invalid(message: &str) -> WikiError {
    WikiError::invalid(message)
}
fn research(inspection: &LedgerInspection) -> Result<&ResearchStateV1> {
    inspection
        .research
        .as_ref()
        .ok_or_else(|| invalid("research genesis required"))
}
fn now(ledger: &JobLedger) -> Result<i64> {
    ledger
        .dispatcher_bindings()
        .3
        .clock
        .read()
        .map(|r| r.utc_ms)
}

fn bytes(fs: &VaultFs, reference: &BoundedPayloadRef) -> Result<Vec<u8>> {
    if reference.byte_len > INPUT_CAP as u64 {
        return Err(invalid("research payload ceiling"));
    }
    let bytes = crate::changes::prepare::read_bounded(fs, &reference.path, INPUT_CAP)?
        .ok_or_else(|| WikiError::new(ErrorCode::RecoveryRequired, "research payload missing"))?;
    if bytes.len() as u64 != reference.byte_len || Blake3Hash::digest(&bytes) != reference.hash {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "research payload changed",
        ));
    }
    Ok(bytes)
}
fn current_reads_match(fs: &VaultFs, reads: &[crate::changes::ReadDependency]) -> Result<bool> {
    for read in reads {
        let actual = crate::changes::prepare::read_bounded(
            fs,
            &read.path,
            crate::changes::prepare::MAX_PAYLOAD_BYTES,
        )?
        .map_or(ExpectedState::Absent, |bytes| {
            ExpectedState::Hash(Blake3Hash::digest(bytes))
        });
        if actual != read.expected {
            return Ok(false);
        }
    }
    Ok(true)
}
fn current_extraction_packet(
    view: &SourceView<'_>,
    citation: &CitationRef,
    packet_id: &RecordId,
) -> Result<Option<graph::VerifiedPacket>> {
    if view.verify(citation, CitationScope::Current).is_err() {
        return Ok(None);
    }
    graph::packet::load_packet(view, packet_id).map(Some)
}
fn load_scope(fs: &VaultFs, inspection: &LedgerInspection) -> Result<ResearchScope> {
    let genesis = inspection
        .spec
        .scope
        .research
        .as_ref()
        .ok_or_else(|| invalid("research genesis required"))?;
    let stored = bytes(fs, &genesis.scope)?;
    let scope: ResearchScope = crate::changes::prepare::strict_json(&stored)?;
    plan::validate_scope(&scope)?;
    if canonical_json(&scope)? != stored
        || inspection.spec.scope.scope_payload_hash.as_ref() != Some(&genesis.scope.hash)
        || scope.limits.rounds != genesis.limits.rounds
        || scope.limits.sources != genesis.limits.sources
        || inspection.spec.scope.question.as_ref() != Some(&scope.question)
        || inspection.spec.scope.exclusions != scope.exclusions
    {
        return Err(invalid("immutable research scope differs from genesis"));
    }
    Ok(scope)
}

fn retain(
    fs: &VaultFs,
    writer: &WriterPermit,
    path: &VaultRelativePath,
    data: &[u8],
) -> Result<()> {
    if data.len() > INPUT_CAP {
        return Err(invalid("research descriptor ceiling"));
    }
    if let Some(existing) = crate::changes::prepare::read_bounded(fs, path, INPUT_CAP)? {
        if existing != data {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "immutable research descriptor conflicts",
            ));
        }
        return Ok(());
    }
    let parent = path
        .as_str()
        .rsplit_once('/')
        .ok_or_else(|| invalid("research payload parent missing"))?
        .0;
    fs.ensure_directory(&VaultRelativePath::new(parent)?, writer)?;
    let staged = fs.stage(path, data, writer)?;
    fs.replace(staged, &ExpectedState::Absent, writer)
        .map(|_| ())
}

pub fn create(fs: &VaultFs, planned: &ResearchPlan, options: JobOptions) -> Result<JobLedger> {
    plan::validate_scope(&planned.scope)?;
    if planned.version != 1
        || canonical_json(&planned.scope)? != planned.scope_bytes
        || planned.spec.tasks
            != planned
                .descriptors
                .iter()
                .map(|d| d.task.clone())
                .collect::<Vec<_>>()
        || planned.spec.input_fingerprint != crate::jobs::tasks::input_fingerprint(&planned.spec)?
    {
        return Err(invalid("research plan binding differs"));
    }
    if options.policy.dry_run {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "dry research planning cannot create a run",
        ));
    }
    let genesis = planned
        .spec
        .scope
        .research
        .as_ref()
        .ok_or_else(|| invalid("research genesis missing"))?;
    if genesis.scope.path.as_str() != format!("runs/{}/inputs/scope.json", planned.spec.run_id)
        || genesis.scope.hash != Blake3Hash::digest(&planned.scope_bytes)
        || genesis.scope.byte_len != planned.scope_bytes.len() as u64
    {
        return Err(invalid("research scope descriptor differs"));
    }
    let ledger = JobLedger::new(
        fs.clone(),
        planned.spec.vault_id.clone(),
        planned.spec.run_id.clone(),
        options.clone(),
    )?;
    let writer = WriterPermit::acquire(fs.root(), Duration::from_millis(options.lock_timeout_ms))?;
    for descriptor in &planned.descriptors {
        if descriptor.task.input.hash != Blake3Hash::digest(&descriptor.bytes)
            || descriptor.task.input_hash != descriptor.task.input.hash
            || descriptor.task.input.byte_len != descriptor.bytes.len() as u64
            || descriptor.task.key != crate::jobs::tasks::task_key(&descriptor.task)?
            || descriptor.task.input.path.as_str()
                != format!(
                    "runs/{}/inputs/{}.json",
                    planned.spec.run_id,
                    descriptor.task.input.hash.hex()
                )
        {
            return Err(invalid("research task descriptor differs"));
        }
    }
    retain(fs, &writer, &genesis.scope.path, &planned.scope_bytes)?;
    for descriptor in &planned.descriptors {
        retain(fs, &writer, &descriptor.task.input.path, &descriptor.bytes)?;
    }
    ledger.create(&writer, planned.spec.clone())?;
    drop(writer);
    Ok(ledger)
}

fn stage_binding(fs: &VaultFs, task: &TaskSpec) -> Result<plan::ResearchStageBinding> {
    let stored = bytes(fs, &task.input)?;
    let input: RemoteInput = crate::changes::prepare::strict_json(&stored)?;
    if canonical_json(&input)? != stored {
        return Err(invalid("noncanonical research generation input"));
    }
    let RemoteOperation::Generate { data, .. } = input.operation else {
        return Err(invalid("research generation input required"));
    };
    let value: Value = crate::changes::prepare::strict_json(data.as_bytes())?;
    let binding: plan::ResearchStageBinding = serde_json::from_value(value["binding"].clone())
        .map_err(|_| invalid("research stage binding invalid"))?;
    if binding.version != 1 || binding.stage != task.stage {
        return Err(invalid("research stage identity differs"));
    }
    Ok(binding)
}
fn active_tasks(inspection: &LedgerInspection) -> Result<Vec<&TaskInspection>> {
    let active = &research(inspection)?.active_tasks;
    Ok(inspection
        .tasks
        .values()
        .filter(|task| active.contains(&task.spec.key))
        .collect())
}
fn output(fs: &VaultFs, task: &TaskInspection) -> Result<Option<(DurableOutputRef, Value)>> {
    if task.state != TaskState::Completed {
        return Ok(None);
    }
    for reference in &task.outputs {
        if reference.record.expected_kind == RecordKind::RunEvent {
            let stage = stages::read_output(fs, reference, &task.spec)?;
            return Ok(Some((reference.clone(), stage.response)));
        }
    }
    Ok(None)
}
fn stage_at<'a>(
    fs: &VaultFs,
    inspection: &'a LedgerInspection,
    stage: TaskStage,
    round: u32,
) -> Result<Option<&'a TaskInspection>> {
    for task in active_tasks(inspection)? {
        if task.spec.stage == stage && stage_binding(fs, &task.spec)?.round == round {
            return Ok(Some(task));
        }
    }
    Ok(None)
}
fn gap(
    stage: TaskStage,
    task: Option<Blake3Hash>,
    origin: Option<String>,
    code: &str,
    message: &str,
) -> ResearchGap {
    ResearchGap {
        stage,
        task_key: task,
        origin,
        code: code.into(),
        message: message.into(),
    }
}

struct Context {
    passages: Vec<ResearchPassage>,
    records: Vec<RecordRef>,
    gaps: Vec<ResearchGap>,
    parents: Vec<DurableOutputRef>,
    dependencies: Vec<Blake3Hash>,
}
fn context(fs: &VaultFs, inspection: &LedgerInspection, scope: &ResearchScope) -> Result<Context> {
    let view = SourceView::from_fs_bounded(fs, SOURCE_CAP, 4096)?;
    let mut context = Context {
        passages: vec![],
        records: vec![],
        gaps: vec![],
        parents: vec![],
        dependencies: vec![],
    };
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    let tasks = active_tasks(inspection)?;
    for task in &tasks {
        if task.spec.stage == TaskStage::InspectExisting && task.state == TaskState::Completed {
            let (_, response) =
                output(fs, task)?.ok_or_else(|| invalid("inspection output missing"))?;
            let input: plan::ResearchInspectInput = serde_json::from_value(response)
                .map_err(|_| invalid("inspection output invalid"))?;
            for passage in input.passages {
                if seen.insert(canonical_json(&passage.citation)?) {
                    let proof = match view.verify(&passage.citation, CitationScope::Current) {
                        Ok(proof) => proof,
                        Err(error) => {
                            context.gaps.push(gap(
                                TaskStage::InspectExisting,
                                Some(task.spec.key.clone()),
                                None,
                                "stale_passage",
                                &error.message,
                            ));
                            continue;
                        }
                    };
                    if proof.quote != passage.quote.as_bytes()
                        || proof.dependencies != passage.dependencies
                    {
                        context.gaps.push(gap(
                            TaskStage::InspectExisting,
                            Some(task.spec.key.clone()),
                            None,
                            "stale_passage",
                            "Existing source proof changed; the passage was withheld.",
                        ));
                        continue;
                    }
                    total += passage.quote.len();
                    context.passages.push(passage);
                }
            }
            context.records.extend(input.current_records);
        }
        if task.state == TaskState::Completed {
            context.dependencies.push(task.spec.key.clone());
            if matches!(
                task.spec.stage,
                TaskStage::InspectExisting
                    | TaskStage::PlanFrontier
                    | TaskStage::Discover
                    | TaskStage::AssessGaps
            ) && let Some((reference, _)) = output(fs, task)?
            {
                context.parents.push(reference);
            }
        }
        if task.spec.stage == TaskStage::Capture && task.state == TaskState::Completed {
            let source = task
                .outputs
                .iter()
                .find(|o| o.record.expected_kind == RecordKind::Source);
            let revision = task
                .outputs
                .iter()
                .find(|o| o.record.expected_kind == RecordKind::Revision);
            if let (Some(source), Some(revision)) = (source, revision) {
                let content = match view.revision_content_bounded(
                    &source.record.record_id,
                    &revision.record.record_id,
                    &mut BTreeMap::new(),
                    SOURCE_CAP,
                    SOURCE_CAP,
                ) {
                    Ok(content) => content,
                    Err(error) => {
                        context.gaps.push(gap(
                            TaskStage::Capture,
                            Some(task.spec.key.clone()),
                            None,
                            "unsupported_source",
                            &error.message,
                        ));
                        continue;
                    }
                };
                let content = String::from_utf8(content)
                    .map_err(|_| invalid("captured source UTF-8 invalid"))?;
                let available = scope.limits.stage.max_text_bytes.saturating_sub(total);
                let mut end = content.len().min(available);
                while end > 0 && !content.is_char_boundary(end) {
                    end -= 1;
                }
                if end == 0 || context.passages.len() >= scope.limits.stage.max_citations {
                    context.gaps.push(gap(
                        TaskStage::Capture,
                        Some(task.spec.key.clone()),
                        None,
                        "context_omitted",
                        "Captured content exceeded the caller's research context ceiling.",
                    ));
                    continue;
                }
                let quote = &content[..end];
                let citation = CitationRef::Source(SourceSpanRef {
                    source_id: source.record.record_id.clone(),
                    source_revision: revision.record.record_id.clone(),
                    span: ByteSpan::new(0, end as u64)?,
                    quote_hash: Blake3Hash::digest(quote.as_bytes()),
                });
                if seen.insert(canonical_json(&citation)?) {
                    let proof = match view.verify(&citation, CitationScope::Current) {
                        Ok(proof) => proof,
                        Err(error) => {
                            context.gaps.push(gap(
                                TaskStage::Capture,
                                Some(task.spec.key.clone()),
                                None,
                                "stale_passage",
                                &error.message,
                            ));
                            continue;
                        }
                    };
                    total += quote.len();
                    context.passages.push(ResearchPassage {
                        citation,
                        quote: quote.into(),
                        dependencies: proof.dependencies,
                    });
                }
                context
                    .records
                    .extend([source.record.clone(), revision.record.clone()]);
            }
        }
        if task.spec.stage == TaskStage::AssessGaps && task.state == TaskState::Completed {
            let (_, response) =
                output(fs, task)?.ok_or_else(|| invalid("assessment output missing"))?;
            let assessment: gaps::GapAssessment = serde_json::from_value(response)
                .map_err(|_| invalid("assessment output invalid"))?;
            for message in assessment.gaps {
                context.gaps.push(gap(
                    TaskStage::AssessGaps,
                    Some(task.spec.key.clone()),
                    None,
                    "unanswered",
                    &message,
                ));
            }
        }
        if task.state == TaskState::Failed {
            let origin = research(inspection)?
                .task_origins
                .get(&task.spec.key)
                .and_then(|origin| research(inspection).ok()?.origins.get(&origin.origin_key))
                .map(|origin| origin.url.clone());
            context.gaps.push(gap(
                task.spec.stage,
                Some(task.spec.key.clone()),
                origin,
                "failed_stage",
                task.failure_code
                    .as_deref()
                    .unwrap_or("Research stage failed with retained accounting."),
            ));
        }
    }
    context
        .records
        .sort_by(|a, b| a.record_id.cmp(&b.record_id));
    context.records.dedup_by(|a, b| a == b);
    context.dependencies.sort();
    context.dependencies.dedup();
    Ok(context)
}

fn admit(
    ledger: &JobLedger,
    round: u32,
    descriptors: Vec<ResearchDescriptor>,
    origins: Vec<ResearchOriginV1>,
    task_origins: Vec<ResearchTaskOriginV1>,
    parents: Vec<DurableOutputRef>,
) -> Result<()> {
    let (fs, _, _, options) = ledger.dispatcher_bindings();
    let inspection = ledger.inspect()?;
    let state = research(&inspection)?;
    let writer = WriterPermit::acquire(fs.root(), Duration::from_millis(options.lock_timeout_ms))?;
    for descriptor in &descriptors {
        retain(&fs, &writer, &descriptor.task.input.path, &descriptor.bytes)?;
    }
    drop(writer);
    let tasks: Vec<_> = descriptors
        .into_iter()
        .map(|descriptor| descriptor.task)
        .collect();
    let id = Blake3Hash::digest(canonical_json(&(
        state.binding.number,
        state.frontier_revision,
        round,
        &origins,
        &tasks,
        &task_origins,
        &parents,
    ))?);
    ledger.admit_research_frontier(EventPayload::ResearchFrontierAdmitted {
        version: 1,
        epoch: state.binding.number,
        prior_revision: state.frontier_revision,
        admission_id: id,
        round,
        origins,
        tasks,
        task_origins,
        parent_outputs: parents,
    })?;
    Ok(())
}

fn capture(
    ledger: &JobLedger,
    fs: &VaultFs,
    task: &TaskSpec,
    outcome: PublicFetchOutcome,
) -> Result<()> {
    let inspection = ledger.inspect()?;
    let state = research(&inspection)?;
    let origin = state
        .task_origins
        .get(&task.key)
        .ok_or_else(|| invalid("capture origin proof missing"))?;
    let original = &state
        .origins
        .get(&origin.origin_key)
        .ok_or_else(|| invalid("source origin missing"))?
        .url;
    if outcome.capture.redirect()?.is_some() {
        let receipt = crate::jobs::checkpoint::receipt_plan(
            ledger,
            &outcome.attempt,
            OutputDisposition::Validated,
            vec![],
            vec![],
            vec![],
        )?;
        acquire::settle_receipt(fs, ledger, receipt)?;
        ledger.finish_remote_task(&task.key, vec![], vec![], |_| Ok(false))?;
        return Ok(());
    }
    let mut redirects = vec![];
    let mut cursor = origin.parent_capture.as_ref();
    while let Some(key) = cursor {
        let attempt = inspection
            .attempts
            .iter()
            .rev()
            .find(|a| &a.attempt.task_key == key && a.phase == AttemptPhase::Settled)
            .ok_or_else(|| invalid("redirect preceding attempt missing"))?;
        let capture = public_fetch::retained_capture(ledger, &attempt.attempt)?;
        redirects.push(acquire::RedirectObservation {
            observed_url: capture.observed_url,
            requested_url: capture.requested_url,
            status: capture.status,
            location: capture
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("location"))
                .map(|(_, value)| value.clone())
                .ok_or_else(|| invalid("redirect location missing"))?,
            fetched_at_utc_ms: capture.fetched_at_utc_ms,
            original_hash: capture.original_hash,
            attempt: attempt.attempt.clone(),
        });
        cursor = state
            .task_origins
            .get(key)
            .and_then(|origin| origin.parent_capture.as_ref());
    }
    redirects.reverse();
    let attempt = outcome.attempt.clone();
    acquire::capture_outcome(fs, ledger, outcome, original, redirects)?;
    let fresh = ledger.inspect()?;
    let actual = fresh
        .attempts
        .iter()
        .find(|a| a.attempt == attempt)
        .ok_or_else(|| invalid("capture receipt attempt missing"))?;
    ledger.finish_remote_task(
        &task.key,
        actual.outputs.clone(),
        actual.cache_outputs.clone(),
        |_| Ok(false),
    )?;
    Ok(())
}

fn validate_generation(
    fs: &VaultFs,
    scope: &ResearchScope,
    task: &TaskSpec,
    text: &str,
) -> Result<()> {
    let binding = stage_binding(fs, task)?;
    if binding.scope_hash != Blake3Hash::digest(canonical_json(scope)?) {
        return Err(invalid("generation scope hash differs"));
    }
    match task.stage {
        TaskStage::PlanFrontier => {
            frontier::validate(text.as_bytes(), &scope.limits.stage, &scope.exclusions)?;
        }
        TaskStage::AssessGaps => {
            let view = SourceView::from_fs_bounded(fs, SOURCE_CAP, 4096)?;
            let mut known = BTreeSet::new();
            for citation in &binding.citations {
                view.verify(citation, CitationScope::Current)?;
                if let CitationRef::Assertion(evidence) = citation {
                    known.insert(evidence.evidence_id.clone());
                }
            }
            gaps::validate(
                text.as_bytes(),
                &scope.limits.stage,
                &known,
                &scope.exclusions,
            )?;
        }
        TaskStage::Synthesize => {
            let view = SourceView::from_fs_bounded(fs, SOURCE_CAP, 4096)?;
            synthesis::validate(
                text.as_bytes(),
                &scope.limits.stage,
                &binding.citations,
                &binding.current_records,
                &view,
            )?;
        }
        _ => return Err(invalid("research generation stage unsupported")),
    }
    Ok(())
}
fn remote(
    fs: &VaultFs,
    ledger: &JobLedger,
    scope: &ResearchScope,
    task: &TaskSpec,
    outcome: DispatchOutcome,
) -> Result<()> {
    match &outcome.output {
        ValidatedOutput::Generation { text, .. } => {
            if let Err(error) = validate_generation(fs, scope, task, text) {
                let local = matches!(
                    error.code,
                    ErrorCode::FreshnessConflict
                        | ErrorCode::RecoveryRequired
                        | ErrorCode::ContentConflict
                        | ErrorCode::Internal
                        | ErrorCode::SourceIntegrity
                        | ErrorCode::RecordNotFound
                        | ErrorCode::ReferenceAmbiguous
                        | ErrorCode::VaultNotFound
                        | ErrorCode::IndexCorrupt
                );
                let receipt = crate::jobs::checkpoint::receipt_plan(
                    ledger,
                    &outcome.attempt,
                    if local {
                        OutputDisposition::Unknown
                    } else {
                        OutputDisposition::Rejected
                    },
                    vec![],
                    vec![],
                    vec![],
                )?;
                acquire::settle_receipt(fs, ledger, receipt)?;
                if !local {
                    ledger.fail_research_task(
                        &task.key,
                        "research stage validation rejected the paid response",
                    )?;
                }
                return Err(error);
            }
            stages::publish_generation(fs, ledger, task, outcome, now(ledger)?)?;
        }
        ValidatedOutput::Search { leads } => {
            let response = json!({"leads": leads});
            let inspection = ledger.inspect()?;
            let (reference, write) = stages::output_write(
                &inspection.spec.vault_id,
                &inspection.spec.run_id,
                task,
                Some(&outcome.attempt),
                response,
                inspection.spec.created_at_utc_ms,
            )?;
            let receipt = crate::jobs::checkpoint::receipt_plan(
                ledger,
                &outcome.attempt,
                OutputDisposition::Validated,
                vec![reference.clone()],
                vec![],
                vec![write],
            )?;
            acquire::settle_receipt(fs, ledger, receipt)?;
            ledger.finish_remote_task(&task.key, vec![reference], vec![], |_| Ok(false))?;
        }
        _ => return Err(invalid("research remote output role differs")),
    }
    Ok(())
}
fn failed(
    fs: &VaultFs,
    ledger: &JobLedger,
    task: &TaskSpec,
    failure: Box<DispatchFailure>,
) -> WikiError {
    let DispatchFailure {
        error,
        materialization,
        retry,
        ..
    } = *failure;
    if let Some(receipt) = materialization
        && let Err(error) = acquire::settle_receipt(fs, ledger, receipt)
    {
        return error;
    }
    if matches!(retry, RetryDecision::After { .. }) {
        return error;
    }
    if !matches!(
        error.code,
        ErrorCode::BudgetExceeded
            | ErrorCode::Cancelled
            | ErrorCode::OfflineUnavailable
            | ErrorCode::RecoveryRequired
    ) && let Err(error) = ledger.fail_research_task(
        &task.key,
        "research remote stage failed with retained response accounting",
    ) {
        return error;
    }
    error
}

fn service<'a>(
    task: &TaskSpec,
    runtime: &'a ResearchRuntime<'_>,
) -> Result<Option<&'a TrustedService>> {
    match task.capability {
        Some(Capability::Generate) => Ok(Some(runtime.generation)),
        Some(Capability::Search) => runtime.search.map(Some).ok_or_else(|| {
            WikiError::new(
                ErrorCode::ProfileUntrusted,
                "research search service absent",
            )
        }),
        Some(Capability::Fetch) => Ok(None),
        _ => Err(invalid("unsupported research remote capability")),
    }
}

fn dispatch(
    fs: &VaultFs,
    ledger: &JobLedger,
    scope: &ResearchScope,
    tasks: &[TaskSpec],
    runtime: &ResearchRuntime<'_>,
) -> Result<()> {
    let work: Vec<_> = tasks
        .iter()
        .map(|task| {
            Ok(ResearchDispatchWork {
                task_key: task.key.clone(),
                service: service(task, runtime)?,
                purpose: DispatchPurpose::Task,
            })
        })
        .collect::<Result<_>>()?;
    let batch = runtime
        .dispatcher
        .execute_ready_batch(ledger, &work, runtime.public_fetch)?;
    let had_outcomes = !batch.outcomes.is_empty();
    let mut first_error = None;
    for result in batch.outcomes {
        let task = tasks
            .iter()
            .find(|t| t.key == result.task_key)
            .ok_or_else(|| invalid("batch outcome task absent"))?;
        let result = match result.outcome {
            ResearchDispatchOutcome::Remote(Ok(outcome)) => {
                remote(fs, ledger, scope, task, outcome)
            }
            ResearchDispatchOutcome::Public(Ok(outcome)) => capture(ledger, fs, task, outcome),
            ResearchDispatchOutcome::Remote(Err(failure))
            | ResearchDispatchOutcome::Public(Err(failure)) => {
                Err(failed(fs, ledger, task, failure))
            }
        };
        if let Err(error) = result {
            let inspection = ledger.inspect()?;
            let safely_failed = inspection
                .tasks
                .get(&task.key)
                .is_some_and(|t| t.state == TaskState::Failed);
            if research(&inspection)?
                .retry_not_before
                .contains_key(&task.key)
            {
                continue;
            }
            if (!safely_failed || !matches!(task.stage, TaskStage::Capture | TaskStage::Discover))
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    if let Some(error) = batch.stop {
        if had_outcomes
            && error.code == ErrorCode::BudgetExceeded
            && ledger.inspect()?.state == RunState::Running
        {
            return Ok(());
        }
        return Err(error);
    }
    Ok(())
}

fn recover(
    fs: &VaultFs,
    app: &OfflineApp,
    ledger: &JobLedger,
    scope: &ResearchScope,
    runtime: &ResearchRuntime<'_>,
) -> Result<()> {
    ledger.replay()?;
    let inspection = ledger.inspect()?;
    for task in active_tasks(&inspection)? {
        if task.spec.stage != TaskStage::StageChanges || task.state != TaskState::Pending {
            continue;
        }
        if task.spec.capability.is_some()
            || task.spec.settings_hash != Blake3Hash::digest(b"lwiki.research-report.v1")
            || !task.spec.dependencies.is_empty()
            || crate::jobs::tasks::task_key(&task.spec)? != task.spec.key
        {
            return Err(invalid("pending report task binding differs"));
        }
        let stored = bytes(fs, &task.spec.input)?;
        let pending: ResearchReport = crate::changes::prepare::strict_json(&stored)?;
        if canonical_json(&pending)? != stored
            || pending.version != 1
            || pending.run_id != inspection.spec.run_id
            || pending.question != scope.question
            || task.spec.input_hash != Blake3Hash::digest(&stored)
        {
            return Err(invalid("pending report descriptor differs from scope"));
        }
        if report::authenticate_local_report(fs, &inspection, &pending)?
            != task.spec.source_bindings
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "pending report read proofs changed",
            ));
        }
        report::publish(fs, ledger, &pending)?;
        if ledger.inspect()?.tasks[&task.spec.key].state != TaskState::Completed {
            return Err(invalid(
                "pending report publication did not complete its admitted task",
            ));
        }
    }
    let inspection = ledger.inspect()?;
    let active = &research(&inspection)?.active_tasks;
    for attempt in &inspection.attempts {
        if attempt.phase != AttemptPhase::Received || attempt.receipt.is_some() {
            continue;
        }
        let task = &inspection
            .tasks
            .get(&attempt.attempt.task_key)
            .ok_or_else(|| invalid("retained task missing"))?
            .spec;
        if !active.contains(&task.key) {
            let receipt = crate::jobs::checkpoint::receipt_plan(
                ledger,
                &attempt.attempt,
                OutputDisposition::Unknown,
                vec![],
                vec![],
                vec![],
            )?;
            acquire::settle_receipt(fs, ledger, receipt)?;
        } else if task.capability == Some(Capability::Fetch) {
            let outcome = public_fetch::recover_response(ledger, &attempt.attempt)?;
            capture(ledger, fs, task, outcome)?;
        } else if task.stage == TaskStage::Extract {
            extract(app, ledger, task, runtime)?;
        } else {
            let service =
                service(task, runtime)?.ok_or_else(|| invalid("retained remote service absent"))?;
            let outcome = runtime
                .dispatcher
                .recover_response(ledger, service, &task.key, &attempt.attempt)
                .map_err(|failure| failed(fs, ledger, task, failure))?;
            remote(fs, ledger, scope, task, outcome)?;
        }
    }
    let inspection = ledger.inspect()?;
    for task in active_tasks(&inspection)? {
        if task.state != TaskState::Running
            || research(&inspection)?
                .retry_not_before
                .contains_key(&task.spec.key)
        {
            continue;
        }
        if let Some(attempt) = inspection
            .attempts
            .iter()
            .rev()
            .find(|a| a.attempt.task_key == task.spec.key)
            && attempt.phase == AttemptPhase::Settled
            && attempt.remote_exposure == RemoteExposure::TerminalConfirmed
            && let Some(receipt) = &attempt.receipt
        {
            let receipt = crate::jobs::checkpoint::receipt(fs, receipt)?;
            if receipt.output_disposition != OutputDisposition::Validated {
                ledger.fail_research_task(
                    &task.spec.key,
                    "research remote stage failed with retained response accounting",
                )?;
            }
        }
    }
    Ok(())
}

fn settle_historical_before_rebind(
    fs: &VaultFs,
    ledger: &JobLedger,
    runtime: &ResearchRuntime<'_>,
) -> Result<()> {
    let inspection = ledger.inspect()?;
    for attempt in &inspection.attempts {
        match attempt.phase {
            AttemptPhase::Settled => {}
            AttemptPhase::OutputCommitted => {
                ledger.settle(&attempt.attempt)?;
            }
            AttemptPhase::Received
                if attempt.remote_exposure == RemoteExposure::TerminalConfirmed =>
            {
                let task = &inspection
                    .tasks
                    .get(&attempt.attempt.task_key)
                    .ok_or_else(|| invalid("historical task missing"))?
                    .spec;
                // Authenticate the original retained codec when available. A decoding
                // failure remains Unknown, never a new rejected-provider verdict.
                if task.capability == Some(Capability::Fetch) {
                    let _ = public_fetch::retained_capture(ledger, &attempt.attempt);
                } else if let Some(service) = service(task, runtime)? {
                    let _ = runtime.dispatcher.decode_retained(
                        ledger,
                        service,
                        &task.key,
                        DispatchPurpose::Task,
                        &attempt.attempt,
                    );
                }
                let receipt = crate::jobs::checkpoint::receipt_plan(
                    ledger,
                    &attempt.attempt,
                    OutputDisposition::Unknown,
                    vec![],
                    vec![],
                    vec![],
                )?;
                acquire::settle_receipt(fs, ledger, receipt)?;
            }
            _ => {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "unsettled historical remote work requires reconciliation before research rebind",
                ));
            }
        }
    }
    Ok(())
}

fn ready(inspection: &LedgerInspection, time: i64) -> Result<Vec<TaskSpec>> {
    let state = research(inspection)?;
    let mut tasks: Vec<_> = active_tasks(inspection)?
        .into_iter()
        .filter(|task| {
            task.state == TaskState::Pending
                && task.spec.dependencies.iter().all(|key| {
                    inspection
                        .tasks
                        .get(key)
                        .is_some_and(|t| t.state == TaskState::Completed)
                })
                || task.state == TaskState::Running
                    && state
                        .retry_not_before
                        .get(&task.spec.key)
                        .is_some_and(|deadline| *deadline <= time)
        })
        .map(|task| task.spec.clone())
        .collect();
    tasks.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.key.cmp(&b.key)));
    Ok(tasks)
}

fn leads(
    fs: &VaultFs,
    inspection: &LedgerInspection,
    scope: &ResearchScope,
    round: u32,
) -> Result<(Vec<String>, Vec<String>, Vec<DurableOutputRef>)> {
    let frontier = stage_at(fs, inspection, TaskStage::PlanFrontier, round)?
        .ok_or_else(|| invalid("round frontier missing"))?;
    let (reference, response) =
        output(fs, frontier)?.ok_or_else(|| invalid("round frontier incomplete"))?;
    let frontier: frontier::Frontier = frontier::validate(
        &canonical_json(&response)?,
        &scope.limits.stage,
        &scope.exclusions,
    )?;
    let mut urls = frontier.urls;
    if round == 1 {
        urls.extend(scope.explicit_urls.iter().cloned());
    }
    Ok((frontier.queries, urls, vec![reference]))
}

fn schedule(
    fs: &VaultFs,
    app: &OfflineApp,
    ledger: &JobLedger,
    scope: &ResearchScope,
    runtime: &ResearchRuntime<'_>,
) -> Result<bool> {
    let inspection = ledger.inspect()?;
    let state = research(&inspection)?;
    let run_id = &inspection.spec.run_id;
    let round = state.rounds_started.max(1);
    let current_frontier = stage_at(fs, &inspection, TaskStage::PlanFrontier, round)?;
    if current_frontier.is_some_and(|t| t.state != TaskState::Completed) {
        return Ok(false);
    }
    let assessment = stage_at(fs, &inspection, TaskStage::AssessGaps, round)?;
    if let Some(assessment) = assessment.filter(|task| task.state == TaskState::Completed) {
        let (reference, response) =
            output(fs, assessment)?.ok_or_else(|| invalid("round assessment output missing"))?;
        let binding = stage_binding(fs, &assessment.spec)?;
        let known: BTreeSet<_> = binding
            .citations
            .iter()
            .filter_map(|citation| {
                if let CitationRef::Assertion(e) = citation {
                    Some(e.evidence_id.clone())
                } else {
                    None
                }
            })
            .collect();
        let result = gaps::validate(
            &canonical_json(&response)?,
            &scope.limits.stage,
            &known,
            &scope.exclusions,
        )?;
        if !state.rounds.iter().any(|result| result.round == round) {
            let citations: Vec<_> = binding
                .citations
                .into_iter()
                .filter(|citation| match citation {
                    CitationRef::Source(_) => true,
                    CitationRef::Assertion(evidence) => {
                        result.covered_evidence_ids.contains(&evidence.evidence_id)
                    }
                })
                .collect();
            let support_groups: Vec<_> = citations
                .iter()
                .map(|citation| match citation {
                    CitationRef::Source(r) => r.quote_hash.clone(),
                    CitationRef::Assertion(r) => r.quote_hash.clone(),
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let id = Blake3Hash::digest(canonical_json(&(
                state.binding.number,
                round,
                &assessment.spec.key,
                &reference,
                &citations,
                &support_groups,
            ))?);
            ledger.assess_research_round(EventPayload::ResearchRoundAssessed {
                version: 1,
                epoch: state.binding.number,
                prior_revision: state.frontier_revision,
                assessment_id: id,
                round,
                task_key: assessment.spec.key.clone(),
                output: reference,
                citations,
                support_groups,
            })?;
            return Ok(true);
        }
        let context = context(fs, &inspection, scope)?;
        if result.stop || round >= scope.limits.rounds || state.no_progress_rounds >= 2 {
            if stage_at(fs, &inspection, TaskStage::Synthesize, round)?.is_none() {
                let descriptor = plan::generation_task(
                    run_id,
                    scope,
                    TaskStage::Synthesize,
                    round,
                    &context.parents,
                    &context.passages,
                    &context.records,
                    runtime.generation,
                    50,
                    context.dependencies,
                )?;
                let descriptor =
                    plan::with_read_preconditions(descriptor, &state.binding.read_preconditions)?;
                admit(ledger, 0, vec![descriptor], vec![], vec![], context.parents)?;
                return Ok(true);
            }
            return Ok(false);
        }
        if stage_at(fs, &inspection, TaskStage::PlanFrontier, round + 1)?.is_none() {
            let descriptor = plan::generation_task(
                run_id,
                scope,
                TaskStage::PlanFrontier,
                round + 1,
                &context.parents,
                &context.passages,
                &context.records,
                runtime.generation,
                10,
                context.dependencies,
            )?;
            admit(
                ledger,
                round + 1,
                vec![descriptor],
                vec![],
                vec![],
                context.parents,
            )?;
            return Ok(true);
        }
        return Ok(false);
    }
    if assessment.is_some() {
        return Ok(false);
    }
    let (queries, mut urls, mut parents) = leads(fs, &inspection, scope, round)?;
    current_frontier.ok_or_else(|| invalid("frontier task missing"))?;
    let mut searches = vec![];
    let mut search_keys = BTreeSet::new();
    if let Some(search) = runtime.search.filter(|_| scope.search_profile.is_some()) {
        for query in queries {
            for page in 0..scope.limits.search_pages {
                let descriptor =
                    plan::search_task(run_id, scope, &query, page, search, 20, vec![])?;
                search_keys.insert(descriptor.task.key.clone());
                if !state.active_tasks.contains(&descriptor.task.key) {
                    searches.push(descriptor);
                }
            }
        }
    }
    if !searches.is_empty() {
        admit(ledger, round, searches, vec![], vec![], parents)?;
        return Ok(true);
    }
    let search_tasks: Vec<_> = active_tasks(&inspection)?
        .into_iter()
        .filter(|task| {
            task.spec.stage == TaskStage::Discover && search_keys.contains(&task.spec.key)
        })
        .collect();
    if search_tasks
        .iter()
        .any(|task| !matches!(task.state, TaskState::Completed | TaskState::Failed))
    {
        return Ok(false);
    }
    for search in search_tasks {
        if let Some((reference, response)) = output(fs, search)? {
            let search_leads: Vec<SearchLead> =
                serde_json::from_value(response["leads"].clone())
                    .map_err(|_| invalid("retained search leads invalid"))?;
            for lead in search_leads {
                urls.push(lead.url);
            }
            parents.push(reference);
        }
    }
    let mut origins = vec![];
    let mut captures = vec![];
    let mut task_origins = vec![];
    let mut selected = BTreeSet::new();
    let remaining = scope.limits.sources as usize - state.origins.len();
    for raw in urls {
        let url = public_fetch::validate_url(&raw, None)?.to_string();
        if scope
            .exclusions
            .iter()
            .any(|excluded| url.to_lowercase().contains(&excluded.trim().to_lowercase()))
        {
            continue;
        }
        let key = Blake3Hash::digest(&url);
        if state.origins.contains_key(&key)
            || !selected.insert(key.clone())
            || origins.len() >= remaining
        {
            continue;
        }
        let descriptor = plan::capture_task(run_id, &url, &scope.limits.fetch, 30, vec![])?;
        origins.push(ResearchOriginV1 {
            key: key.clone(),
            url,
            round,
        });
        task_origins.push(ResearchTaskOriginV1 {
            task_key: descriptor.task.key.clone(),
            origin_key: key,
            parent_capture: None,
        });
        captures.push(descriptor);
    }
    if !captures.is_empty() || state.rounds_started == 0 {
        admit(ledger, round, captures, origins, task_origins, parents)?;
        return Ok(true);
    }
    // A completed redirect authorizes only a separately admitted next hop of its original origin.
    for task in active_tasks(&inspection)? {
        if task.spec.stage != TaskStage::Capture || task.state != TaskState::Completed {
            continue;
        }
        let origin = state
            .task_origins
            .get(&task.spec.key)
            .ok_or_else(|| invalid("capture origin absent"))?;
        if state
            .origins
            .get(&origin.origin_key)
            .is_none_or(|origin| origin.round != round)
        {
            continue;
        }
        let attempt = inspection
            .attempts
            .iter()
            .rev()
            .find(|a| a.attempt.task_key == task.spec.key && a.phase == AttemptPhase::Settled)
            .ok_or_else(|| invalid("capture attempt missing"))?;
        let retained = public_fetch::retained_capture(ledger, &attempt.attempt)?;
        if let Some(url) = retained.redirect()? {
            let descriptor = plan::capture_task(
                run_id,
                &url,
                &scope.limits.fetch,
                30,
                vec![task.spec.key.clone()],
            )?;
            if !state.active_tasks.contains(&descriptor.task.key) {
                let proof = ResearchTaskOriginV1 {
                    task_key: descriptor.task.key.clone(),
                    origin_key: origin.origin_key.clone(),
                    parent_capture: Some(task.spec.key.clone()),
                };
                admit(ledger, round, vec![descriptor], vec![], vec![proof], vec![])?;
                return Ok(true);
            }
        }
    }
    let capture_tasks: Vec<_> = active_tasks(&inspection)?
        .into_iter()
        .filter(|task| {
            task.spec.stage == TaskStage::Capture
                && state
                    .task_origins
                    .get(&task.spec.key)
                    .and_then(|o| state.origins.get(&o.origin_key))
                    .is_some_and(|o| o.round == round)
        })
        .collect();
    if capture_tasks
        .iter()
        .any(|task| !matches!(task.state, TaskState::Completed | TaskState::Failed))
    {
        return Ok(false);
    }
    for capture in capture_tasks {
        let source = capture
            .outputs
            .iter()
            .find(|o| o.record.expected_kind == RecordKind::Source);
        let revision = capture
            .outputs
            .iter()
            .find(|o| o.record.expected_kind == RecordKind::Revision);
        if let (Some(source), Some(revision)) = (source, revision) {
            let view = SourceView::from_fs_bounded(fs, SOURCE_CAP, 4096)?;
            let content = match view.revision_content_bounded(
                &source.record.record_id,
                &revision.record.record_id,
                &mut BTreeMap::new(),
                SOURCE_CAP,
                SOURCE_CAP,
            ) {
                Ok(content) if !content.is_empty() => content,
                _ => continue,
            };
            let citation = CitationRef::Source(SourceSpanRef {
                source_id: source.record.record_id.clone(),
                source_revision: revision.record.record_id.clone(),
                span: ByteSpan::new(0, content.len() as u64)?,
                quote_hash: Blake3Hash::digest(&content),
            });
            // Context emits the corresponding unsupported/stale passage gap.
            // An immutable historical revision is never exported as current input.
            if view.verify(&citation, CitationScope::Current).is_err() {
                continue;
            }
            let exported = app.graph_extract_agent(&ExportRequest {
                source_id: source.record.record_id.clone(),
                revision_id: Some(revision.record.record_id.clone()),
                windows: vec![],
                limits: scope.limits.extraction.clone(),
                candidate_context: vec![],
            })?;
            let view = SourceView::from_fs_bounded(fs, SOURCE_CAP, 4096)?;
            let Some(packet) =
                current_extraction_packet(&view, &citation, &exported.packet.packet_id)?
            else {
                continue;
            };
            let descriptor = graph::research_extract::plan_task(
                run_id,
                &packet,
                runtime.generation,
                scope.limits.stage_output_tokens,
                40,
                vec![capture.spec.key.clone()],
            )?;
            if !state.active_tasks.contains(&descriptor.task.key) {
                admit(
                    ledger,
                    round,
                    vec![ResearchDescriptor {
                        task: descriptor.task,
                        bytes: descriptor.descriptor,
                    }],
                    vec![],
                    vec![],
                    vec![],
                )?;
                return Ok(true);
            }
        }
    }
    if active_tasks(&inspection)?.iter().any(|task| {
        task.spec.stage == TaskStage::Extract
            && !matches!(task.state, TaskState::Completed | TaskState::Failed)
    }) {
        return Ok(false);
    }
    let context = context(fs, &inspection, scope)?;
    let descriptor = plan::generation_task(
        run_id,
        scope,
        TaskStage::AssessGaps,
        round,
        &context.parents,
        &context.passages,
        &context.records,
        runtime.generation,
        45,
        context.dependencies,
    )?;
    admit(
        ledger,
        round,
        vec![descriptor],
        vec![],
        vec![],
        context.parents,
    )?;
    Ok(true)
}

fn extract(
    app: &OfflineApp,
    ledger: &JobLedger,
    task: &TaskSpec,
    runtime: &ResearchRuntime<'_>,
) -> Result<Option<PreparedChange>> {
    let input: RemoteInput = crate::changes::prepare::strict_json(&bytes(app.fs(), &task.input)?)?;
    let RemoteOperation::Generate { data, .. } = input.operation else {
        return Err(invalid("extraction input invalid"));
    };
    let packet: graph::ExtractionPacket = crate::changes::prepare::strict_json(data.as_bytes())?;
    let view = SourceView::from_fs_bounded(app.fs(), SOURCE_CAP, 4096)?;
    let verified = graph::packet::load_packet(&view, &packet.packet_id)?;
    let result = app.execute_existing_task(
        ledger,
        task,
        &verified,
        runtime.generation,
        runtime.dispatcher,
        false,
    )?;
    Ok(result.import.and_then(|import| import.prepared))
}

#[allow(clippy::too_many_arguments)]
fn final_report(
    app: &OfflineApp,
    ledger: &JobLedger,
    scope: &ResearchScope,
    runtime: &ResearchRuntime<'_>,
    extra_gaps: &[ResearchGap],
    changes: Vec<PreparedChange>,
    reason: &str,
    partial: bool,
) -> Result<ResearchOutcome> {
    let mut partial = partial;
    let inspection = ledger.inspect()?;
    let mut context = context(app.fs(), &inspection, scope)?;
    context.gaps.extend_from_slice(extra_gaps);
    let mut synthesis = None;
    {
        let round = research(&inspection)?.rounds_started.max(1);
        if let Some(task) = stage_at(app.fs(), &inspection, TaskStage::Synthesize, round)?
            && let Some((_, response)) = output(app.fs(), task)?
        {
            let binding = stage_binding(app.fs(), &task.spec)?;
            let view = SourceView::from_fs_bounded(app.fs(), SOURCE_CAP, 4096)?;
            match synthesis::validate(
                &canonical_json(&response)?,
                &scope.limits.stage,
                &binding.citations,
                &binding.current_records,
                &view,
            ) {
                Ok(validated) => synthesis = Some(validated),
                Err(error) => {
                    partial = true;
                    context.gaps.push(gap(
                        TaskStage::Synthesize,
                        Some(task.spec.key.clone()),
                        None,
                        "stale_synthesis",
                        &error.message,
                    ));
                }
            }
        }
    }
    let report = report::build(
        app.fs(),
        ledger,
        scope,
        &context.passages,
        &context.gaps,
        synthesis,
        changes,
        reason,
        partial,
    )?;
    if !runtime.job_options.policy.dry_run && !app.options().dry_run {
        report::publish(app.fs(), ledger, &report)?;
    }
    Ok(ResearchOutcome {
        stop_code: None,
        status: status(
            app.fs(),
            app.vault_id(),
            &inspection.spec.run_id,
            runtime.job_options.clone(),
        )?,
        prepared_changes: report.proposed_changes.clone(),
        report: Some(report),
        network_used: runtime.dispatcher.network_used(),
    })
}

pub fn run(
    app: &OfflineApp,
    ledger: &JobLedger,
    runtime: &ResearchRuntime<'_>,
) -> Result<ResearchOutcome> {
    let (fs, vault, _, options) = ledger.dispatcher_bindings();
    if fs.root().path() != app.fs().root().path() || &vault != app.vault_id() {
        return Err(invalid("research application vault differs"));
    }
    let inspection = ledger.inspect()?;
    let scope = load_scope(&fs, &inspection)?;
    if options.policy.dry_run || runtime.job_options.policy.dry_run || app.options().dry_run {
        return Ok(ResearchOutcome {
            stop_code: None,
            status: status(&fs, &vault, &inspection.spec.run_id, options)?,
            report: report::latest(&fs, &inspection)?,
            prepared_changes: vec![],
            network_used: false,
        });
    }
    app.recover()?;
    ledger.replay()?;
    let inspection = ledger.inspect()?;
    match inspection.state {
        RunState::Planned => {
            ledger.start()?;
        }
        RunState::Running => {}
        RunState::Completed => {
            let report = report::latest(&fs, &inspection)?;
            let prepared_changes = report
                .as_ref()
                .map_or_else(Vec::new, |report| report.proposed_changes.clone());
            return Ok(ResearchOutcome {
                stop_code: None,
                status: status(&fs, &vault, &inspection.spec.run_id, options)?,
                report,
                prepared_changes,
                network_used: false,
            });
        }
        _ => {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "paused research requires explicit resume",
            ));
        }
    }
    let previous =
        report::latest(&fs, &inspection)?.map_or_else(Vec::new, |report| report.proposed_changes);
    let engine = crate::changes::ChangeEngine::new(fs.clone())?;
    let mut changes = Vec::new();
    for change in previous {
        // Page proposals are rediscovered from the current paid synthesis below.
        // An older synthesis's drafts remain in its immutable report only.
        if !engine
            .inspect(&change.change_id)?
            .manifest
            .allocated_ids
            .contains_key("research_proposal")
        {
            changes.push(change);
        }
    }
    let result = (|| -> Result<()> {
        recover(&fs, app, ledger, &scope, runtime)?;
        for task in active_tasks(&ledger.inspect()?)? {
            if task.spec.stage == TaskStage::Extract
                && task.state == TaskState::Completed
                && let Some(change) = extract(app, ledger, &task.spec, runtime)?
                && !changes.contains(&change)
            {
                changes.push(change);
            }
        }
        for _ in 0..RUN_MAX_TASKS {
            if options.cancel.is_cancelled() || runtime.job_options.cancel.is_cancelled() {
                return Err(WikiError::new(ErrorCode::Cancelled, "research cancelled"));
            }
            let inspection = ledger.inspect()?;
            if now(ledger)? >= inspection.effective_deadline_utc_ms {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "research deadline reached",
                ));
            }
            let tasks = ready(&inspection, now(ledger)?)?;
            if let Some(task) = tasks.first().filter(|task| task.capability.is_none()) {
                if task.stage != TaskStage::InspectExisting {
                    return Err(invalid("unexpected local research task"));
                }
                let local: plan::ResearchInspectInput =
                    crate::changes::prepare::strict_json(&bytes(&fs, &task.input)?)?;
                stages::publish_local(
                    &fs,
                    ledger,
                    task,
                    serde_json::to_value(local)
                        .map_err(|_| invalid("inspection output serialization"))?,
                    task.source_bindings.clone(),
                    now(ledger)?,
                )?;
                continue;
            }
            if let Some(task) = tasks
                .first()
                .filter(|task| task.stage == TaskStage::Extract)
            {
                match extract(app, ledger, task, runtime) {
                    Ok(Some(change)) => changes.push(change),
                    Ok(None) => {}
                    Err(error) => {
                        if research(&ledger.inspect()?)?
                            .retry_not_before
                            .contains_key(&task.key)
                        {
                            continue;
                        }
                        if matches!(
                            error.code,
                            ErrorCode::RecoveryRequired
                                | ErrorCode::FreshnessConflict
                                | ErrorCode::ContentConflict
                                | ErrorCode::Cancelled
                                | ErrorCode::BudgetExceeded
                                | ErrorCode::OfflineUnavailable
                        ) {
                            return Err(error);
                        }
                        ledger.fail_research_task(
                            &task.key,
                            "packet-bound research extraction rejected or failed",
                        )?;
                    }
                }
                continue;
            }
            if !tasks.is_empty() {
                if options.policy.offline
                    || runtime.job_options.policy.offline
                    || app.options().offline
                {
                    return Err(WikiError::new(
                        ErrorCode::OfflineUnavailable,
                        "offline research has no completed remote stage",
                    ));
                }
                let tasks: Vec<_> = tasks
                    .into_iter()
                    .take_while(|task| {
                        task.capability.is_some() && task.stage != TaskStage::Extract
                    })
                    .collect();
                dispatch(&fs, ledger, &scope, &tasks, runtime)?;
                continue;
            }
            let round = research(&inspection)?.rounds_started.max(1);
            let time = now(ledger)?;
            if let Some(next_retry) = research(&inspection)?
                .retry_not_before
                .iter()
                .filter(|(key, deadline)| {
                    **deadline > time
                        && inspection
                            .tasks
                            .get(*key)
                            .is_some_and(|t| t.state == TaskState::Running)
                })
                .map(|(_, deadline)| *deadline)
                .min()
            {
                std::thread::sleep(Duration::from_millis((next_retry - time).min(1000) as u64));
                continue;
            }
            if stage_at(&fs, &inspection, TaskStage::Synthesize, round)?
                .is_some_and(|task| task.state == TaskState::Completed)
            {
                return Ok(());
            }
            if !schedule(&fs, app, ledger, &scope, runtime)? {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "research has unfinished work requiring reconciliation",
                ));
            }
        }
        Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "research lifetime task ceiling",
        ))
    })();
    match result {
        Ok(()) => {
            let inspection = ledger.inspect()?;
            let has_failures = active_tasks(&inspection)?
                .iter()
                .any(|task| task.state == TaskState::Failed);
            let mut outcome = final_report(
                app,
                ledger,
                &scope,
                runtime,
                &[],
                changes,
                if has_failures {
                    "Research stopped with failed source acquisition gaps"
                } else {
                    "Research stages completed"
                },
                has_failures,
            )?;
            if has_failures || outcome.report.as_ref().is_some_and(|report| report.partial) {
                let cancelled =
                    options.cancel.is_cancelled() || runtime.job_options.cancel.is_cancelled();
                if cancelled {
                    outcome.stop_code = Some(ErrorCode::Cancelled);
                }
                if ledger.inspect()?.state == RunState::Running {
                    ledger.pause(if cancelled {
                        StopReason::Cancelled
                    } else {
                        StopReason::Failed("Research retained stage or publication gaps".into())
                    })?;
                }
            } else {
                ledger.complete_run()?;
            }
            Ok(ResearchOutcome {
                status: status(&fs, &vault, &inspection.spec.run_id, options)?,
                ..outcome
            })
        }
        Err(error) => {
            let reason = match error.code {
                ErrorCode::Cancelled => StopReason::Cancelled,
                ErrorCode::BudgetExceeded => StopReason::Budget,
                ErrorCode::FreshnessConflict | ErrorCode::ContentConflict => {
                    StopReason::InputsChanged
                }
                ErrorCode::RecoveryRequired => StopReason::ReconciliationRequired,
                _ => StopReason::Failed(error.message.clone()),
            };
            if ledger.inspect()?.state == RunState::Running {
                ledger.pause(reason)?;
            }
            let gap = gap(
                TaskStage::StageChanges,
                None,
                None,
                &format!("{:?}", error.code),
                &error.message,
            );
            final_report(
                app,
                ledger,
                &scope,
                runtime,
                &[gap],
                changes,
                &error.message,
                true,
            )
            .map(|mut outcome| {
                outcome.stop_code = Some(error.code);
                outcome
            })
        }
    }
}

pub fn status(
    fs: &VaultFs,
    vault: &RecordId,
    run: &RecordId,
    options: JobOptions,
) -> Result<ResearchStatus> {
    let ledger = JobLedger::new(fs.clone(), vault.clone(), run.clone(), options)?;
    let inspection = ledger.inspect()?;
    let scope = load_scope(fs, &inspection)?;
    let latest = report::latest(fs, &inspection)?;
    let gaps = if let Some(report) = &latest {
        report.gaps.clone()
    } else {
        context(fs, &inspection, &scope)?.gaps
    };
    let latest_hash = latest
        .as_ref()
        .map(|report| canonical_json(report).map(Blake3Hash::digest))
        .transpose()?;
    let latest_report = inspection
        .tasks
        .values()
        .filter(|task| {
            task.spec.stage == TaskStage::StageChanges
                && task.state == TaskState::Completed
                && latest_hash.as_ref() == Some(&task.spec.input_hash)
        })
        .flat_map(|task| task.outputs.clone())
        .last();
    Ok(ResearchStatus {
        inspection,
        gaps,
        latest_report,
    })
}

pub fn resume(
    app: &OfflineApp,
    ledger: &JobLedger,
    runtime: &ResearchRuntime<'_>,
) -> Result<ResearchOutcome> {
    resume_with_amendment(app, ledger, runtime, None)
}

pub fn resume_with_amendment(
    app: &OfflineApp,
    ledger: &JobLedger,
    runtime: &ResearchRuntime<'_>,
    amendment: Option<ResearchResumeAmendment>,
) -> Result<ResearchOutcome> {
    let (fs, vault, run_id, options) = ledger.dispatcher_bindings();
    let inspection = ledger.inspect()?;
    let scope = load_scope(&fs, &inspection)?;
    if options.policy.dry_run || runtime.job_options.policy.dry_run || app.options().dry_run {
        return run(app, ledger, runtime);
    }
    if options.policy.offline || runtime.job_options.policy.offline || app.options().offline {
        let report = report::latest(&fs, &inspection)?;
        let prepared_changes = report
            .as_ref()
            .map_or_else(Vec::new, |report| report.proposed_changes.clone());
        return Ok(ResearchOutcome {
            stop_code: None,
            status: status(&fs, &vault, &run_id, options)?,
            report,
            prepared_changes,
            network_used: false,
        });
    }
    if !matches!(inspection.state, RunState::Paused | RunState::Stopped) {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "only paused/stopped research can resume",
        ));
    }
    app.recover()?;
    ledger.replay()?;
    let inspection = ledger.inspect()?;
    let old = research(&inspection)?;
    let mut services = vec![runtime.generation];
    if let Some(search) = runtime.search.filter(|_| scope.search_profile.is_some()) {
        services.push(search);
    }
    let mut bindings: Vec<_> = services
        .iter()
        .map(|service| {
            let summary = service.summary();
            ServiceBindingV1 {
                profile_id: summary.profile_id,
                capability: summary.capability,
                profile_fingerprint: summary.profile_fingerprint,
                endpoint_fingerprint: summary.endpoint_fingerprint,
            }
        })
        .collect();
    bindings.sort_by(|a, b| (a.capability, &a.profile_id).cmp(&(b.capability, &b.profile_id)));
    if bindings == old.binding.services
        && runtime.generation.summary().config_fingerprint == old.binding.config_fingerprint
        && active_tasks(&inspection)?
            .iter()
            .all(|task| task.spec.capability.is_none() || task.state == TaskState::Completed)
        && inspection.attempts.iter().all(|attempt| {
            attempt.phase == AttemptPhase::Settled
                && attempt.remote_exposure == RemoteExposure::TerminalConfirmed
        })
        && let Some(synthesis) =
            stage_at(&fs, &inspection, TaskStage::Synthesize, old.rounds_started)?
                .filter(|task| task.state == TaskState::Completed)
    {
        let requested = amendment.as_ref().map(|amendment| {
            (
                amendment.limits.clone(),
                amendment.deadline_utc_ms,
                amendment.reason.clone(),
            )
        });
        match ledger.resume_research_publication(
            &synthesis.spec.key,
            old.binding.number,
            requested,
            &services,
        ) {
            Ok(_) => return run(app, ledger, runtime),
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::FreshnessConflict
                        | ErrorCode::ContentConflict
                        | ErrorCode::SourceIntegrity
                ) => {}
            Err(error) => return Err(error),
        }
    }
    let inspected = inspection::inspect(&fs, &vault, &scope)?;
    let mut binding = BindingEpochV1 {
        version: 1,
        number: old.binding.number,
        config_fingerprint: runtime.generation.summary().config_fingerprint,
        source_snapshot: Some(inspected.snapshot),
        input_records: inspected.records,
        read_preconditions: inspected.dependencies,
        services: bindings,
    };
    let mut relevant = binding.clone();
    relevant.source_snapshot = old.binding.source_snapshot.clone();
    let mut changed_task_reads = false;
    for task in active_tasks(&inspection)? {
        if !current_reads_match(&fs, &task.spec.source_bindings)? {
            changed_task_reads = true;
        }
    }
    if relevant != old.binding || changed_task_reads {
        settle_historical_before_rebind(&fs, ledger, runtime)?;
        binding.number = old
            .binding
            .number
            .checked_add(1)
            .ok_or_else(|| invalid("research epoch overflow"))?;
        let mut initial = binding.clone();
        initial.number = 0;
        let planned = plan::plan(
            scope.clone(),
            vault.clone(),
            run_id.clone(),
            initial,
            runtime.generation,
            &inspected.passages,
            inspection.spec.limits.clone(),
            inspection.spec.created_at_utc_ms,
            inspection.spec.deadline_utc_ms,
        )?;
        let mut descriptors = planned.descriptors;
        let closed = old.rounds.iter().any(|r| r.round == old.rounds_started);
        let stopped_assessment = if let Some(assessment) =
            stage_at(&fs, &inspection, TaskStage::AssessGaps, old.rounds_started)?
        {
            if let Some((_, response)) = output(&fs, assessment)? {
                let assessment_binding = stage_binding(&fs, &assessment.spec)?;
                let known = assessment_binding
                    .citations
                    .iter()
                    .filter_map(|citation| match citation {
                        CitationRef::Assertion(evidence) => Some(evidence.evidence_id.clone()),
                        CitationRef::Source(_) => None,
                    })
                    .collect();
                gaps::validate(
                    &canonical_json(&response)?,
                    &scope.limits.stage,
                    &known,
                    &scope.exclusions,
                )?
                .stop
            } else {
                false
            }
        } else {
            false
        };
        let final_synthesis = closed
            && (old.rounds_started >= scope.limits.rounds
                || old.no_progress_rounds >= 2
                || stopped_assessment
                || stage_at(&fs, &inspection, TaskStage::Synthesize, old.rounds_started)?
                    .is_some());
        let round = if closed && !final_synthesis {
            old.rounds_started + 1
        } else {
            old.rounds_started.max(1)
        };
        if round > scope.limits.rounds {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "changed-source resume has no remaining research round",
            ));
        }
        descriptors[1] = plan::generation_task(
            &run_id,
            &scope,
            if final_synthesis {
                TaskStage::Synthesize
            } else {
                TaskStage::PlanFrontier
            },
            round,
            &[],
            &inspected.passages,
            &binding.input_records,
            runtime.generation,
            1,
            vec![descriptors[0].task.key.clone()],
        )?;
        if final_synthesis {
            descriptors[1] =
                plan::with_read_preconditions(descriptors[1].clone(), &binding.read_preconditions)?;
        }
        let mut active = BTreeSet::new();
        for task in active_tasks(&inspection)? {
            let source_matches = current_reads_match(&fs, &task.spec.source_bindings)?;
            let service_matches = task.spec.capability.is_none()
                || task.spec.capability == Some(Capability::Fetch)
                || binding.services.iter().any(|s| {
                    Some(s.capability) == task.spec.capability
                        && task.spec.settings_hash
                            == Blake3Hash::digest(s.profile_fingerprint.as_str())
                });
            if (task.state == TaskState::Completed
                || matches!(task.spec.stage, TaskStage::Discover | TaskStage::Capture))
                && source_matches
                && service_matches
            {
                active.insert(task.spec.key.clone());
            }
        }
        loop {
            let removed: Vec<_> = active
                .iter()
                .filter(|key| {
                    inspection.tasks[*key]
                        .spec
                        .dependencies
                        .iter()
                        .any(|dep| !active.contains(dep))
                })
                .cloned()
                .collect();
            if removed.is_empty() {
                break;
            }
            for key in removed {
                active.remove(&key);
            }
        }
        let writer =
            WriterPermit::acquire(fs.root(), Duration::from_millis(options.lock_timeout_ms))?;
        for descriptor in &descriptors {
            retain(&fs, &writer, &descriptor.task.input.path, &descriptor.bytes)?;
            active.insert(descriptor.task.key.clone());
        }
        drop(writer);
        let tasks: Vec<_> = descriptors
            .into_iter()
            .map(|d| d.task)
            .filter(|task| !inspection.tasks.contains_key(&task.key))
            .collect();
        let id = Blake3Hash::digest(canonical_json(&(&binding, &active, &tasks))?);
        ledger.rebind_research(
            EventPayload::ResearchRebound {
                version: 1,
                expected_epoch: old.binding.number,
                prior_revision: old.frontier_revision,
                amendment_id: id,
                binding,
                active_tasks: active.into_iter().collect(),
                tasks,
                reason: "Explicit caller resume revalidated current source/service proofs".into(),
            },
            &services,
        )?;
        if round > old.rounds_started {
            admit(ledger, round, vec![], vec![], vec![], vec![])?;
        }
    }
    match amendment {
        Some(amendment) => ledger.resume_with_requested_limits(
            amendment.limits,
            amendment.deadline_utc_ms,
            amendment.reason,
        )?,
        None => ledger.resume(None)?,
    };
    run(app, ledger, runtime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::OperationOptions,
        sources::{CaptureRequest, ExtractionInput, SourceOrigin},
        vault::VaultRoot,
    };

    #[test]
    fn post_export_gate_withholds_historical_packet_with_fresh_header_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: vault_packet_gate\nwiki_kind: vault\ntitle: Packet gate fixture\n---\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let app = OfflineApp::new(fs.clone(), OperationOptions::default()).unwrap();
        let initial = b"Original source passage.";
        let captured = app
            .source_add(CaptureRequest {
                title: "Original source".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "source.txt".into(),
                original: initial.to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: None,
            })
            .unwrap();
        let source = captured.allocated_ids["source"].clone();
        let old_revision = captured.allocated_ids["revision"].clone();
        let old_citation = CitationRef::Source(SourceSpanRef {
            source_id: source.clone(),
            source_revision: old_revision.clone(),
            span: ByteSpan::new(0, initial.len() as u64).unwrap(),
            quote_hash: Blake3Hash::digest(initial),
        });
        let request = |revision| ExportRequest {
            source_id: source.clone(),
            revision_id: Some(revision),
            windows: vec![],
            limits: Default::default(),
            candidate_context: vec![],
        };
        let first = app
            .graph_extract_agent(&request(old_revision.clone()))
            .unwrap();
        let view = SourceView::from_fs_bounded(&fs, SOURCE_CAP, 4096).unwrap();
        assert!(
            current_extraction_packet(&view, &old_citation, &first.packet.packet_id)
                .unwrap()
                .is_some()
        );
        let current = b"Refreshed source passage.";
        let refreshed = app
            .source_refresh(
                source.clone(),
                CaptureRequest {
                    title: "Refreshed source".into(),
                    origin_kind: SourceOrigin::LocalFile,
                    origin: "source.txt".into(),
                    original: current.to_vec(),
                    extraction: ExtractionInput::Utf8Preserve,
                    media_type: None,
                },
            )
            .unwrap();
        let historical = app.graph_extract_agent(&request(old_revision)).unwrap();
        let view = SourceView::from_fs_bounded(&fs, SOURCE_CAP, 4096).unwrap();
        let verified_historical =
            graph::packet::load_packet(&view, &historical.packet.packet_id).unwrap();
        assert!(current_reads_match(&fs, verified_historical.dependencies()).unwrap());
        assert!(
            current_extraction_packet(&view, &old_citation, &historical.packet.packet_id)
                .unwrap()
                .is_none()
        );
        let revision = refreshed.allocated_ids["revision"].clone();
        let current_citation = CitationRef::Source(SourceSpanRef {
            source_id: source.clone(),
            source_revision: revision.clone(),
            span: ByteSpan::new(0, current.len() as u64).unwrap(),
            quote_hash: Blake3Hash::digest(current),
        });
        let exported = app.graph_extract_agent(&request(revision)).unwrap();
        let view = SourceView::from_fs_bounded(&fs, SOURCE_CAP, 4096).unwrap();
        assert!(
            current_extraction_packet(&view, &current_citation, &exported.packet.packet_id)
                .unwrap()
                .is_some()
        );
    }
}
