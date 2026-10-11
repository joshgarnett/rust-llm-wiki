//! Typed relation publication. Received spool authority grants the initial seal;
//! replay regenerates the sealed pair without requiring a newer ledger prefix.
use super::*;
use crate::{
    app::source_relation_materialization::{self as materialization, MaterializationLimits},
    changes::indexed_refresh::{IndexedJobTarget, IndexedWriteOperation},
    providers::{dispatcher, types::ValidatedOutput},
    retrieval::source_relation_store as store,
    vault::operational::{RunLedgerGuard, SpoolPart},
};
use std::{collections::BTreeSet, time::Instant};

#[cfg(test)]
#[path = "source_relation_publication_tests.rs"]
mod tests;

const HASH_BYTES: u64 = 64 * 1024 * 1024;
const CAPTURE_BYTES: usize = 1024 * 1024;

// Generic canonical replay has its own bounded phase allowance. The preparation
// coordinator retains the invocation's absolute deadline before and after this
// phase; a completed commit does not turn an expired invocation into success.
fn replay_phase_deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn check(until: Instant) -> Result<()> {
    if Instant::now() >= until {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "relation publication deadline exceeded",
        ));
    }
    Ok(())
}
fn capture(
    fs: &VaultFs,
    task: &TaskSpec,
    until: Instant,
) -> Result<BTreeMap<VaultRelativePath, Vec<u8>>> {
    if task.stage != TaskStage::PrepareSourceRelations
        || task.capability != Some(Capability::Generate)
        || !(4..=5).contains(&task.source_bindings.len())
    {
        return Err(WikiError::invalid(
            "relation publication requires one closed Source task",
        ));
    }
    let mut result = BTreeMap::new();
    let mut remaining = CAPTURE_BYTES;
    for dep in &task.source_bindings {
        check(until)?;
        if !matches!(dep.expected, ExpectedState::Hash(_)) || result.contains_key(&dep.path) {
            return Err(WikiError::invalid(
                "relation source guards must be unique exact hashes",
            ));
        }
        let bytes =
            crate::changes::prepare::read_bounded(fs, &dep.path, remaining.min(256 * 1024))?
                .ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::FreshnessConflict,
                        "relation source input disappeared",
                    )
                })?;
        remaining = remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| WikiError::invalid("relation capture overflow"))?;
        result.insert(dep.path.clone(), bytes);
    }
    // authenticate_captured inside materialize validates every actual hash and
    // closed Source identity. These reads never derive authority from a proposal.
    Ok(result)
}

pub(super) fn received_plan(
    job: &JobLedger,
    attempt: &AttemptRef,
    until: Instant,
) -> Result<SourceRelationPreparation> {
    check(until)?;
    job.with(false, |guard, loaded| {
        if let Some(committed) = retained_pair(job, loaded, attempt, until)? {
            return Ok(committed);
        }
        plan_locked(job, guard, loaded, attempt, until).map(SourceRelationPreparation::Fresh)
    })
}

/// Exact named completion precedes source/descriptor/spool re-derivation. The
/// committed receipt and both outputs must belong to the same sealed change.
fn retained_pair(
    job: &JobLedger,
    loaded: &ledger::Loaded,
    attempt: &AttemptRef,
    until: Instant,
) -> Result<Option<SourceRelationPreparation>> {
    check(until)?;
    let inspection = &loaded.state.inspection;
    let task = &inspection
        .tasks
        .get(&attempt.task_key)
        .ok_or_else(|| events::corrupt("relation completion task missing"))?
        .spec;
    if task.stage != TaskStage::PrepareSourceRelations
        || task.capability != Some(Capability::Generate)
        || attempt.run_id != job.run_id
        || !inspection.attempts.iter().any(|a| a.attempt == *attempt)
    {
        return Err(WikiError::invalid(
            "relation completion task/attempt differs",
        ));
    }
    let id = receipt_id(attempt)?;
    let path = event_path(&job.run_id, &id)?;
    let Some(bytes) = crate::changes::prepare::read_bounded(&job.fs, &path, EVENT_MAX_BYTES)?
    else {
        return Ok(None);
    };
    let reference = DurableOutputRef {
        record: RecordRef {
            vault_id: job.vault_id.clone(),
            record_id: id.clone(),
            expected_kind: RecordKind::RunEvent,
        },
        path,
        hash: Blake3Hash::digest(&bytes),
    };
    let usage = receipt(&job.fs, &reference)?;
    if usage.attempt != *attempt
        || usage.capability != Capability::Generate
        || usage.input_hash != task.input_hash
        || usage.output_disposition != OutputDisposition::Validated
        || usage.outputs.len() != 2
        || !usage.cache_outputs.is_empty()
    {
        return Err(events::corrupt(
            "retained relation receipt task/output binding differs",
        ));
    }
    let acknowledged = loaded
        .frames
        .iter()
        .find_map(|frame| match &frame.event.payload {
            EventPayload::OutputsCommitted {
                attempt: recorded,
                receipt,
                outputs,
                cache_outputs,
                change,
            } if recorded == attempt => Some((receipt, outputs, cache_outputs, change)),
            _ => None,
        });
    let change = if let Some(change) = named_job_committed(
        &job.fs,
        &job.vault_id,
        &job.run_id,
        JobPublicationKey::Receipt { receipt_id: id },
        &reference.path,
        &reference.hash,
    )? {
        change
    } else {
        // Same compatibility boundary as JobLedger::recover_committed_receipts:
        // only a legacy catalog may discover a pre-acknowledgement unnamed commit.
        if Catalog::new(job.fs.clone(), job.vault_id.clone())
            .operation_state()?
            .is_some()
        {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "unnamed normalized relation receipt requires explicit maintenance; accounting retained",
            ));
        }
        if let Some((receipt, outputs, cache, change)) = acknowledged {
            if receipt != &reference || outputs != &usage.outputs || !cache.is_empty() {
                return Err(events::corrupt(
                    "relation acknowledgement differs from retained pair",
                ));
            }
            change.clone()
        } else {
            let engine = ChangeEngine::new(job.fs.clone())?;
            let mut found = None;
            for id in engine.change_ids()? {
                check(until)?;
                let inspected = engine.inspect_history(&id)?;
                if inspected.status == ChangeStatus::Committed
                    && inspected.manifest.operations.iter().any(|op| {
                        op.target == reference.path
                            && op.after == ExpectedState::Hash(reference.hash.clone())
                    })
                {
                    found = Some(inspected.prepared);
                    break;
                }
            }
            found.ok_or_else(|| {
                events::corrupt("relation receipt has no verified committed changeset")
            })?
        }
    };
    check(until)?;
    let mut all_outputs = usage.outputs.clone();
    all_outputs.push(reference.clone());
    committed(&job.fs, &change, &all_outputs)?;
    let mut paths = std::collections::BTreeSet::new();
    for output_ref in &usage.outputs {
        check(until)?;
        if output_ref.record.vault_id != job.vault_id
            || output_ref.record.expected_kind != RecordKind::RunEvent
            || !paths.insert(output_ref.path.clone())
        {
            return Err(events::corrupt(
                "retained relation outputs are not distinct same-vault events",
            ));
        }
    }
    check(until)?;
    Ok(Some(SourceRelationPreparation::Committed {
        change,
        receipt: reference,
        usage,
    }))
}

fn plan_locked(
    job: &JobLedger,
    guard: &RunLedgerGuard<'_>,
    loaded: &ledger::Loaded,
    attempt: &AttemptRef,
    until: Instant,
) -> Result<MaterializationPlan> {
    check(until)?;
    let inspection = &loaded.state.inspection;
    let task = &inspection
        .tasks
        .get(&attempt.task_key)
        .ok_or_else(|| events::corrupt("relation task absent"))?
        .spec;
    let recorded = inspection
        .attempts
        .iter()
        .find(|a| &a.attempt == attempt)
        .ok_or_else(|| events::corrupt("relation attempt absent"))?;
    if attempt.run_id != job.run_id || recorded.bound.capability != Capability::Generate {
        return Err(WikiError::invalid(
            "relation attempt belongs to another run or capability",
        ));
    }
    let captured = capture(&job.fs, task, until)?;
    let descriptor = crate::changes::prepare::read_bounded(
        &job.fs,
        &task.input.path,
        store::MAX_DESCRIPTOR_BYTES,
    )?
    .ok_or_else(|| events::corrupt("sealed generation descriptor missing before publication"))?;
    let body = guard
        .read_spool(
            attempt,
            SpoolPart::Body,
            recorded
                .bound
                .response_bytes
                .min(GENERATION_SPOOL_MAX_BYTES),
        )?
        .ok_or_else(|| events::corrupt("relation response spool missing"))?;
    let meta = guard
        .read_spool(attempt, SpoolPart::Metadata, METADATA_MAX_BYTES as u64)?
        .ok_or_else(|| events::corrupt("relation response metadata missing"))?;
    check(until)?;
    let output = dispatcher::decode_retained_locked(&job.fs, task, recorded, body, meta)
        .map_err(dispatcher::RetainedDecodeFailure::error)?;
    let ValidatedOutput::Generation { text, .. } = output else {
        return Err(WikiError::invalid(
            "relation output must be decoded generation text",
        ));
    };
    let received = loaded
        .frames
        .iter()
        .find(|f| {
            matches!(&f.event.payload,
        EventPayload::Received { spool } if &spool.attempt == attempt)
        })
        .ok_or_else(|| events::corrupt("relation Received event missing"))?;
    let pair = materialization::materialize(
        &job.fs,
        &job.vault_id,
        task,
        &captured,
        &descriptor,
        text.as_bytes(),
        attempt,
        &timestamp(received.event.occurred_at_utc_ms)?,
        MaterializationLimits {
            max_captured_bytes: CAPTURE_BYTES as u64,
            max_hash_bytes: HASH_BYTES,
            deadline: until,
        },
    )?;
    let plan = receipt_plan_locked(
        job,
        loaded,
        attempt,
        OutputDisposition::Validated,
        vec![pair.raw.reference, pair.annotation.reference],
        vec![],
        vec![pair.raw.write, pair.annotation.write],
    )?;
    check(until)?;
    Ok(plan)
}

fn same_write(a: &ExpectedWrite, b: &ExpectedWrite) -> bool {
    let a_edges: BTreeSet<_> = a.apply_after.iter().collect();
    let b_edges: BTreeSet<_> = b.apply_after.iter().collect();
    a.target == b.target
        && a.expected == b.expected
        && a.proposed == b.proposed
        && a_edges.len() == a.apply_after.len()
        && b_edges.len() == b.apply_after.len()
        && a_edges == b_edges
}

fn same_operations(a: &[ExpectedWrite], b: &[ExpectedWrite]) -> bool {
    let a_paths: BTreeMap<_, _> = a.iter().map(|op| (&op.target, op)).collect();
    let b_paths: BTreeMap<_, _> = b.iter().map(|op| (&op.target, op)).collect();
    a.len() == b.len()
        && a_paths.len() == a.len()
        && b_paths.len() == b.len()
        && a_paths.iter().all(|(path, op)| {
            b_paths
                .get(path)
                .is_some_and(|expected| same_write(op, expected))
        })
}

struct PublicationWrites<'a> {
    raw: &'a ExpectedWrite,
    annotation: &'a ExpectedWrite,
    receipt: &'a ExpectedWrite,
}

/// Receipt outputs retain semantic raw/annotation order. Canonical change
/// operations and the receipt's two prerequisites are sets keyed by exact path.
/// Reject duplicates before constructing any map; no positional role inference.
fn publication_writes<'a>(
    draft: &'a ChangeDraft,
    receipt: &UsageReceipt,
) -> Result<PublicationWrites<'a>> {
    if draft.operations.len() != 3 || receipt.outputs.len() != 2 {
        return Err(WikiError::invalid(
            "relation publication needs exactly three typed writes",
        ));
    }
    let mut operations = BTreeMap::new();
    for op in &draft.operations {
        if operations.insert(&op.target, op).is_some()
            || op.expected != ExpectedState::Absent
            || op
                .proposed
                .as_ref()
                .is_none_or(|bytes| bytes.is_empty() || bytes.len() > store::MAX_ARTIFACT_BYTES)
        {
            return Err(WikiError::invalid(
                "relation writes must be distinct bounded immutable creates",
            ));
        }
    }
    let receipt_path = event_path(&receipt.attempt.run_id, &receipt.receipt_id)?;
    let mut paths = BTreeSet::from([&receipt_path]);
    let mut ids = BTreeSet::from([&receipt.receipt_id]);
    for output in &receipt.outputs {
        if output.record.expected_kind != RecordKind::RunEvent
            || output.record.vault_id != receipt.outputs[0].record.vault_id
            || output.path != event_path(&receipt.attempt.run_id, &output.record.record_id)?
            || !paths.insert(&output.path)
            || !ids.insert(&output.record.record_id)
        {
            return Err(WikiError::invalid(
                "relation output references are not distinct same-Run events",
            ));
        }
    }
    if operations.keys().copied().collect::<BTreeSet<_>>() != paths {
        return Err(WikiError::invalid(
            "relation publication target set differs from receipt roles",
        ));
    }
    let raw = operations[&receipt.outputs[0].path];
    let annotation = operations[&receipt.outputs[1].path];
    let receipt_write = operations[&receipt_path];
    let dependencies: BTreeSet<_> = receipt_write.apply_after.iter().collect();
    let outputs: BTreeSet<_> = receipt.outputs.iter().map(|output| &output.path).collect();
    if !raw.apply_after.is_empty()
        || !annotation.apply_after.is_empty()
        || receipt_write.apply_after.len() != 2
        || dependencies.len() != 2
        || dependencies != outputs
    {
        return Err(WikiError::invalid(
            "relation receipt requires exactly both independent output writes",
        ));
    }
    Ok(PublicationWrites {
        raw,
        annotation,
        receipt: receipt_write,
    })
}

fn proposed_receipt(draft: &ChangeDraft, run: &RecordId) -> Result<UsageReceipt> {
    if draft.title != "Retain accounted response receipt"
        || draft.origin.is_some()
        || draft.inverse_of.is_some()
        || draft.operations.len() != 3
        || draft.allocated_ids.len() != 2
        || draft.allocated_ids.get("run") != Some(run)
        || draft.read_preconditions.len() > 128
    {
        return Err(WikiError::invalid(
            "relation publication requires exactly raw, annotation and receipt",
        ));
    }
    let id = draft
        .allocated_ids
        .get("receipt")
        .ok_or_else(|| WikiError::invalid("relation receipt allocation missing"))?;
    let path = event_path(run, id)?;
    let op = draft
        .operations
        .iter()
        .find(|op| op.target == path && op.expected == ExpectedState::Absent)
        .ok_or_else(|| WikiError::invalid("relation receipt must be its exact immutable create"))?;
    let bytes = op
        .proposed
        .as_deref()
        .filter(|b| b.len() <= EVENT_MAX_BYTES)
        .ok_or_else(|| WikiError::invalid("bounded relation receipt missing"))?;
    let body: ReceiptBody = decode_fence(bytes, "lwiki.run-event.v1")?;
    let r = body.receipt;
    if body.version != 1
        || r.version != 1
        || r.attempt.run_id != *run
        || r.receipt_id != *id
        || receipt_id(&r.attempt)? != *id
        || r.capability != Capability::Generate
        || r.output_disposition != OutputDisposition::Validated
        || r.outputs.len() != 2
        || !r.cache_outputs.is_empty()
    {
        return Err(WikiError::invalid(
            "relation receipt disposition, attempt or typed outputs differ",
        ));
    }
    publication_writes(draft, &r)?;
    Ok(r)
}

pub(super) fn validate_locked(
    job: &JobLedger,
    guard: &RunLedgerGuard<'_>,
    loaded: &ledger::Loaded,
    draft: ChangeDraft,
) -> Result<ValidatedJobDraft> {
    let r = proposed_receipt(&draft, &job.run_id)?;
    let expected = plan_locked(job, guard, loaded, &r.attempt, replay_phase_deadline())?;
    if r != expected.receipt
        || draft.allocated_ids != expected.draft.allocated_ids
        || !same_job_guards(
            &draft.read_preconditions,
            &expected.draft.read_preconditions,
        )
        || !same_operations(&draft.operations, &expected.draft.operations)
    {
        return Err(WikiError::invalid(
            "relation publication differs from exact decoded Received derivation",
        ));
    }
    let mut records = r
        .outputs
        .iter()
        .map(|o| IndexedJobTarget {
            path: o.path.clone(),
            id: o.record.record_id.clone(),
            kind: RecordKind::RunEvent,
        })
        .collect::<Vec<_>>();
    records.push(IndexedJobTarget {
        path: event_path(&job.run_id, &r.receipt_id)?,
        id: r.receipt_id,
        kind: RecordKind::RunEvent,
    });
    records.sort_by(|a, b| a.path.cmp(&b.path));
    let operation = IndexedWriteOperation::JobBatch {
        run_id: job.run_id.clone(),
        records,
        checkpoint: None,
    };
    operation.validate()?;
    Ok(ValidatedJobDraft { draft, operation })
}

/// Semantic validation during initial sealing or exact named replay. Billing
/// authority comes from the fresh Received validator or its sealed commitment;
/// this function does not independently authenticate accounting. No newer ledger
/// is consulted, and a removed response spool is not required.
pub(super) fn replay(
    fs: &VaultFs,
    vault: &RecordId,
    operation: &IndexedWriteOperation,
    draft: &ChangeDraft,
) -> Result<()> {
    let IndexedWriteOperation::JobBatch {
        run_id,
        records,
        checkpoint: None,
    } = operation
    else {
        return Err(WikiError::invalid(
            "relation replay must be a non-checkpoint job batch",
        ));
    };
    operation.validate()?;
    let until = replay_phase_deadline();
    let r = proposed_receipt(draft, run_id)?;
    let writes = publication_writes(draft, &r)?;
    if records.len() != 3 {
        return Err(WikiError::invalid("relation replay record count differs"));
    }
    for op in &draft.operations {
        let bytes = op
            .proposed
            .as_deref()
            .filter(|b| b.len() <= store::MAX_ARTIFACT_BYTES)
            .ok_or_else(|| WikiError::invalid("bounded relation replay payload missing"))?;
        let note = parse_note(bytes);
        let record = note
            .canonical
            .as_ref()
            .ok_or_else(|| WikiError::invalid("relation replay envelope invalid"))?;
        if record.kind() != RecordKind::RunEvent
            || record.string("wiki_run_id") != Some(run_id.as_str())
            || !records.iter().any(|target| {
                target.path == op.target
                    && target.id == *record.id()
                    && target.kind == RecordKind::RunEvent
            })
            || op.expected != ExpectedState::Absent
        {
            return Err(WikiError::invalid(
                "relation replay identity/create binding differs",
            ));
        }
    }
    let raw_bytes = writes.raw.proposed.as_deref().unwrap();
    let raw: store::RawGeneration = decode_fence(raw_bytes, store::RAW_FENCE)?;
    if raw.vault_id != *vault
        || raw.attempt != r.attempt
        || raw.task.input_hash != r.input_hash
        || raw
            .task
            .source_bindings
            .iter()
            .any(|guard| !draft.read_preconditions.contains(guard))
    {
        return Err(WikiError::invalid(
            "relation replay Source/task/receipt binding differs",
        ));
    }
    let receipt_note = parse_note(writes.receipt.proposed.as_deref().unwrap());
    let time = receipt_note
        .canonical
        .as_ref()
        .and_then(|r| r.string("wiki_occurred_at"))
        .ok_or_else(|| WikiError::invalid("relation receipt time absent"))?;
    let captured = capture(fs, &raw.task, until)?;
    let pair = materialization::materialize(
        fs,
        vault,
        &raw.task,
        &captured,
        raw.descriptor.as_bytes(),
        raw.response.as_bytes(),
        &raw.attempt,
        time,
        MaterializationLimits {
            max_captured_bytes: CAPTURE_BYTES as u64,
            max_hash_bytes: HASH_BYTES,
            deadline: until,
        },
    )?;
    if r.outputs != vec![pair.raw.reference, pair.annotation.reference]
        || !same_write(writes.raw, &pair.raw.write)
        || !same_write(writes.annotation, &pair.annotation.write)
    {
        return Err(WikiError::invalid(
            "relation replay raw/annotation derivation differs",
        ));
    }
    check(until)
}
