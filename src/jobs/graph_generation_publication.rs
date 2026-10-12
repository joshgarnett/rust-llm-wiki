//! Exact graph generation publication. Fresh Received authority grants a seal;
//! semantic replay uses that sealed pair without a newer ledger or response spool.
use super::*;
use crate::{
    changes::indexed_refresh::{IndexedJobTarget, IndexedWriteOperation},
    changes::{ReadDependency, ScanDocument, ValidationInput},
    graph::{generation_cache, packet, wire},
    providers::{
        dispatcher,
        types::{RemoteInput, RemoteOperation, ValidatedOutput},
    },
    sources::SourceView,
    vault::operational::{RunLedgerGuard, SpoolPart},
};
use std::time::Instant;

#[cfg(test)]
#[path = "graph_generation_publication_tests.rs"]
mod tests;

const DESCRIPTOR_BYTES: usize = 256 * 1024;
const CAPTURE_BYTES: usize = packet::SOURCE_CAP;
const MAX_GUARDS: usize = 128;

fn phase_deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn check(until: Instant) -> Result<()> {
    if Instant::now() >= until {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "graph generation publication deadline exceeded",
        ));
    }
    Ok(())
}
fn unique_guards(guards: &[ReadDependency]) -> Result<BTreeMap<VaultRelativePath, ExpectedState>> {
    if guards.len() > MAX_GUARDS {
        return Err(WikiError::invalid(
            "graph generation guard count exceeds its ceiling",
        ));
    }
    let mut result = BTreeMap::new();
    for guard in guards {
        if guard.path.as_str().len() > 4096
            || !matches!(guard.expected, ExpectedState::Hash(_))
            || result
                .insert(guard.path.clone(), guard.expected.clone())
                .is_some()
        {
            return Err(WikiError::invalid(
                "graph generation guards must be distinct bounded exact hashes",
            ));
        }
    }
    Ok(result)
}
fn same_guards(a: &[ReadDependency], b: &[ReadDependency]) -> bool {
    match (unique_guards(a), unique_guards(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Capture only declared guards and the authenticated vault marker. One aggregate
/// byte allowance covers all reads; closed SourceView cannot discover other files.
fn capture(
    fs: &VaultFs,
    vault: &RecordId,
    guards: &[ReadDependency],
    until: Instant,
) -> Result<ValidationInput> {
    let mut required = unique_guards(guards)?;
    let wiki = VaultRelativePath::new("WIKI.md")?;
    let marker = crate::changes::prepare::read_bounded(fs, &wiki, 64 * 1024)?
        .ok_or_else(|| WikiError::invalid("graph generation vault marker missing"))?;
    let note = parse_note(&marker);
    if note
        .canonical
        .as_ref()
        .is_none_or(|r| r.kind() != RecordKind::Vault || r.id() != vault)
    {
        return Err(WikiError::invalid(
            "graph generation vault identity differs",
        ));
    }
    if let Some(expected) = required.remove(&wiki)
        && expected != ExpectedState::Hash(note.source_hash.clone())
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "graph generation marker guard changed",
        ));
    }
    let mut remaining = CAPTURE_BYTES
        .checked_sub(marker.len())
        .ok_or_else(|| WikiError::invalid("graph generation capture overflow"))?;
    let mut documents = vec![ScanDocument {
        path: wiki,
        bytes: marker,
        hash: note.source_hash,
    }];
    for (path, expected) in required {
        check(until)?;
        let bytes =
            crate::changes::prepare::read_bounded(fs, &path, remaining)?.ok_or_else(|| {
                WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "graph generation original input disappeared",
                )
            })?;
        remaining = remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| WikiError::invalid("graph generation aggregate capture overflow"))?;
        let hash = Blake3Hash::digest(&bytes);
        if expected != ExpectedState::Hash(hash.clone()) {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "graph generation original input changed",
            ));
        }
        documents.push(ScanDocument { path, bytes, hash });
    }
    check(until)?;
    Ok(ValidationInput {
        vault_id: vault.clone(),
        documents,
        overlay: vec![],
    })
}

fn require_task(task: &TaskSpec, attempt: &AttemptRef) -> Result<()> {
    if task.stage != TaskStage::Extract
        || task.capability != Some(Capability::Generate)
        || task.key != attempt.task_key
        || task.key != crate::jobs::tasks::task_key(task)?
        || attempt.number == 0
        || task.input.byte_len == 0
        || task.input.byte_len > DESCRIPTOR_BYTES as u64
        || task.model_hash.is_none()
        || task.source_bindings.is_empty()
    {
        return Err(WikiError::invalid(
            "graph generation requires its exact Extract task and attempt",
        ));
    }
    unique_guards(&task.source_bindings)?;
    Ok(())
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
        .ok_or_else(|| events::corrupt("graph generation task missing"))?
        .spec;
    require_task(task, attempt)?;
    let recorded = inspection
        .attempts
        .iter()
        .find(|a| &a.attempt == attempt)
        .ok_or_else(|| events::corrupt("graph generation attempt missing"))?;
    if attempt.run_id != job.run_id || recorded.bound.capability != Capability::Generate {
        return Err(WikiError::invalid(
            "graph generation attempt Run/capability differs",
        ));
    }
    // Original Run scope and task source guards are independently unique. Their
    // legitimate overlapping packet guard must agree before a union is formed.
    let mut guards = unique_guards(&inspection.spec.scope.read_preconditions)?;
    for (path, expected) in unique_guards(&task.source_bindings)? {
        if guards
            .insert(path, expected.clone())
            .is_some_and(|old| old != expected)
        {
            return Err(WikiError::invalid(
                "graph generation original scope conflicts with its task",
            ));
        }
    }
    if let Some(run) = &inspection.run_note
        && guards
            .insert(run.path.clone(), ExpectedState::Hash(run.hash.clone()))
            .is_some()
    {
        return Err(WikiError::invalid(
            "graph generation original guards repeat the mutable Run",
        ));
    }
    let guards: Vec<_> = guards
        .into_iter()
        .map(|(path, expected)| ReadDependency { path, expected })
        .collect();
    let input = capture(&job.fs, &job.vault_id, &guards, until)?;
    let view = SourceView::from_closed_input(&job.fs, &input)?;
    let descriptor =
        crate::changes::prepare::read_bounded(&job.fs, &task.input.path, DESCRIPTOR_BYTES)?
            .ok_or_else(|| events::corrupt("graph generation descriptor missing"))?;
    if descriptor.len() as u64 != task.input.byte_len
        || Blake3Hash::digest(&descriptor) != task.input.hash
        || task.input_hash != task.input.hash
    {
        return Err(WikiError::invalid(
            "graph generation descriptor identity differs",
        ));
    }
    let remote: RemoteInput = crate::changes::prepare::strict_json(&descriptor)?;
    let RemoteOperation::Generate {
        instructions,
        data,
        output_schema,
        max_output_tokens,
    } = &remote.operation
    else {
        return Err(WikiError::invalid(
            "graph generation descriptor is not Generate",
        ));
    };
    let described: crate::graph::ExtractionPacket =
        packet::decode(data.as_bytes(), crate::graph::MAX_PACKET_BYTES)?;
    let verified = packet::load_packet(&view, &described.packet_id)?;
    if remote.version != 1
        || *max_output_tokens == 0
        || packet::canonical_json(&remote)? != descriptor
        || verified.packet() != &described
        || instructions != &described.instructions
        || output_schema != &described.output_schema
        || packet::canonical_json(&described)? != data.as_bytes()
        || task.prompt_hash.as_ref()
            != Some(&Blake3Hash::digest(packet::canonical_json(&(
                instructions,
                data,
            ))?))
        || task.schema_hash.as_ref()
            != Some(&Blake3Hash::digest(packet::canonical_json(output_schema)?))
        || !same_guards(&task.source_bindings, verified.dependencies())
    {
        return Err(WikiError::invalid(
            "graph generation descriptor/packet/task guards differ",
        ));
    }
    let body = guard
        .read_spool(
            attempt,
            SpoolPart::Body,
            recorded
                .bound
                .response_bytes
                .min(GENERATION_SPOOL_MAX_BYTES),
        )?
        .ok_or_else(|| events::corrupt("graph generation response spool missing"))?;
    let metadata = guard
        .read_spool(attempt, SpoolPart::Metadata, METADATA_MAX_BYTES as u64)?
        .ok_or_else(|| events::corrupt("graph generation response metadata missing"))?;
    check(until)?;
    let output = dispatcher::decode_retained_locked(&job.fs, task, recorded, body, metadata)
        .map_err(dispatcher::RetainedDecodeFailure::error)?;
    let ValidatedOutput::Generation { text, .. } = output else {
        return Err(WikiError::invalid(
            "graph generation response has another capability",
        ));
    };
    wire::validate_response(&verified, &view, text.as_bytes())?;
    let received = loaded
        .frames
        .iter()
        .find(|f| {
            matches!(&f.event.payload,
        EventPayload::Received { spool } if &spool.attempt == attempt)
        })
        .ok_or_else(|| events::corrupt("graph generation Received event missing"))?;
    let (reference, write) = generation_cache::output_write(
        &job.vault_id,
        attempt,
        &verified,
        text.as_bytes(),
        &timestamp(received.event.occurred_at_utc_ms)?,
    )?;
    let plan = receipt_plan_locked(
        job,
        loaded,
        attempt,
        OutputDisposition::Validated,
        vec![reference],
        vec![],
        vec![write],
    )?;
    check(until)?;
    Ok(plan)
}

struct Writes<'a> {
    output: &'a ExpectedWrite,
    receipt: &'a ExpectedWrite,
}
fn proposed_receipt<'a>(
    draft: &'a ChangeDraft,
    run: &RecordId,
) -> Result<(UsageReceipt, Writes<'a>)> {
    if draft.title != "Retain accounted response receipt"
        || draft.origin.is_some()
        || draft.inverse_of.is_some()
        || draft.operations.len() != 2
        || draft.allocated_ids.len() != 2
        || draft.allocated_ids.get("run") != Some(run)
    {
        return Err(WikiError::invalid(
            "graph generation requires exactly output and receipt",
        ));
    }
    let id = draft
        .allocated_ids
        .get("receipt")
        .ok_or_else(|| WikiError::invalid("graph generation receipt allocation missing"))?;
    let receipt_path = event_path(run, id)?;
    let mut operations = BTreeMap::new();
    for op in &draft.operations {
        if operations.insert(&op.target, op).is_some()
            || op.expected != ExpectedState::Absent
            || op
                .proposed
                .as_ref()
                .is_none_or(|b| b.is_empty() || b.len() > crate::graph::MAX_ARTIFACT_BYTES)
        {
            return Err(WikiError::invalid(
                "graph generation writes must be distinct bounded immutable creates",
            ));
        }
    }
    let receipt_write = operations
        .get(&receipt_path)
        .ok_or_else(|| WikiError::invalid("graph generation exact receipt target missing"))?;
    let bytes = receipt_write
        .proposed
        .as_deref()
        .filter(|b| b.len() <= EVENT_MAX_BYTES)
        .ok_or_else(|| WikiError::invalid("graph generation bounded receipt missing"))?;
    let body: ReceiptBody = decode_fence(bytes, "lwiki.run-event.v1")?;
    let receipt = body.receipt;
    if body.version != 1
        || receipt.version != 1
        || receipt.attempt.run_id != *run
        || receipt.receipt_id != *id
        || receipt_id(&receipt.attempt)? != *id
        || receipt.capability != Capability::Generate
        || receipt.output_disposition != OutputDisposition::Validated
        || receipt.outputs.len() != 1
        || !receipt.cache_outputs.is_empty()
    {
        return Err(WikiError::invalid(
            "graph generation receipt attempt/disposition/outputs differ",
        ));
    }
    let reference = &receipt.outputs[0];
    let output_id = generation_cache::output_id(&receipt.attempt)?;
    let output_path = VaultRelativePath::new(format!("runs/{run}/outputs/{output_id}.md"))?;
    if reference.record.expected_kind != RecordKind::RunEvent
        || reference.record.record_id != output_id
        || reference.path != output_path
        || reference.path == receipt_path
    {
        return Err(WikiError::invalid(
            "graph generation output is not its deterministic same-Run event",
        ));
    }
    let output_write = operations
        .get(&output_path)
        .ok_or_else(|| WikiError::invalid("graph generation output target missing"))?;
    if !output_write.apply_after.is_empty()
        || receipt_write.apply_after != vec![output_path]
        || output_write
            .proposed
            .as_deref()
            .is_none_or(|b| Blake3Hash::digest(b) != reference.hash)
    {
        return Err(WikiError::invalid(
            "graph generation output must precede its exact receipt",
        ));
    }
    Ok((
        receipt,
        Writes {
            output: output_write,
            receipt: receipt_write,
        },
    ))
}
fn same_operations(a: &[ExpectedWrite], b: &[ExpectedWrite]) -> bool {
    let a_rows: BTreeMap<_, _> = a.iter().map(|op| (&op.target, op)).collect();
    let b_rows: BTreeMap<_, _> = b.iter().map(|op| (&op.target, op)).collect();
    a.len() == b.len()
        && a_rows.len() == a.len()
        && b_rows.len() == b.len()
        && a_rows.iter().all(|(path, op)| {
            b_rows.get(path).is_some_and(|other| {
                op.expected == other.expected
                    && op.proposed == other.proposed
                    && op.apply_after == other.apply_after
            })
        })
}

pub(super) fn validate_locked(
    job: &JobLedger,
    guard: &RunLedgerGuard<'_>,
    loaded: &ledger::Loaded,
    draft: ChangeDraft,
) -> Result<ValidatedJobDraft> {
    // Operational admission accepts only original immutable source/packet hashes.
    unique_guards(&draft.read_preconditions)?;
    let (receipt, _) = proposed_receipt(&draft, &job.run_id)?;
    let expected = plan_locked(job, guard, loaded, &receipt.attempt, phase_deadline())?;
    if receipt != expected.receipt
        || draft.allocated_ids != expected.draft.allocated_ids
        || !same_guards(
            &draft.read_preconditions,
            &expected.draft.read_preconditions,
        )
        || !same_operations(&draft.operations, &expected.draft.operations)
    {
        return Err(WikiError::invalid(
            "graph generation differs from exact decoded Received derivation",
        ));
    }
    let output = &receipt.outputs[0];
    let mut records = vec![
        IndexedJobTarget {
            path: output.path.clone(),
            id: output.record.record_id.clone(),
            kind: RecordKind::RunEvent,
        },
        IndexedJobTarget {
            path: event_path(&job.run_id, &receipt.receipt_id)?,
            id: receipt.receipt_id,
            kind: RecordKind::RunEvent,
        },
    ];
    records.sort_by(|a, b| a.path.cmp(&b.path));
    let operation = IndexedWriteOperation::JobBatch {
        run_id: job.run_id.clone(),
        records,
        checkpoint: None,
    };
    replay(&job.fs, &job.vault_id, &operation, &draft)?;
    Ok(ValidatedJobDraft { draft, operation })
}

/// Projection retains both immutable write-target before witnesses; engine
/// preparation subsequently verifies and removes both overlapping read guards.
/// Keep either complete representation, while semantic reads cover only original
/// hashed inputs. All supplied witnesses remain in the sealed draft/proof.
fn replay_input_guards(draft: &ChangeDraft, run: &RecordId) -> Result<Vec<ReadDependency>> {
    if draft.read_preconditions.len() > MAX_GUARDS {
        return Err(WikiError::invalid(
            "graph generation replay guard ceiling exceeded",
        ));
    }
    let mut seen = BTreeMap::new();
    let mut target_witnesses = 0;
    let mut inputs = vec![];
    let mutable_run = run_path(run)?;
    for guard in &draft.read_preconditions {
        if guard.path.as_str().len() > 4096 || seen.insert(&guard.path, &guard.expected).is_some() {
            return Err(WikiError::invalid(
                "graph generation replay guards must be distinct and bounded",
            ));
        }
        if let Some(operation) = draft.operations.iter().find(|op| op.target == guard.path) {
            if guard.expected != ExpectedState::Absent || operation.expected != guard.expected {
                return Err(WikiError::invalid(
                    "graph generation target witness differs from immutable before-state",
                ));
            }
            target_witnesses += 1;
        } else {
            if !matches!(guard.expected, ExpectedState::Hash(_)) {
                return Err(WikiError::invalid(
                    "graph generation original replay inputs require exact hashes",
                ));
            }
            if guard.path != mutable_run {
                inputs.push(guard.clone());
            }
        }
    }
    if target_witnesses != 0 && target_witnesses != 2 {
        return Err(WikiError::invalid(
            "graph generation replay requires zero or both target witnesses",
        ));
    }
    Ok(inputs)
}

/// A semantic check of sealed bytes, not a second accounting admission. The
/// existing named proof binds the original Received validation and before-state.
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
            "graph generation replay requires a non-checkpoint job batch",
        ));
    };
    operation.validate()?;
    let (receipt, writes) = proposed_receipt(draft, run_id)?;
    if records.len() != 2 || receipt.outputs[0].record.vault_id != *vault {
        return Err(WikiError::invalid(
            "graph generation replay vault/record count differs",
        ));
    }
    for op in &draft.operations {
        let note = parse_note(op.proposed.as_deref().unwrap());
        let record = note
            .canonical
            .as_ref()
            .ok_or_else(|| WikiError::invalid("graph generation replay envelope invalid"))?;
        if record.kind() != RecordKind::RunEvent
            || record.string("wiki_run_id") != Some(run_id.as_str())
            || !records.iter().any(|r| {
                r.path == op.target && r.id == *record.id() && r.kind == RecordKind::RunEvent
            })
        {
            return Err(WikiError::invalid(
                "graph generation replay envelope/target differs",
            ));
        }
    }
    let output_bytes = writes.output.proposed.as_deref().unwrap();
    let output = generation_cache::decode_output(output_bytes)?;
    if output.attempt != receipt.attempt || output.task_key != receipt.attempt.task_key {
        return Err(WikiError::invalid(
            "graph generation replay attempt differs",
        ));
    }
    // Mutable Run bytes can advance after accounting acknowledgement. Their
    // original guard remains sealed; semantic graph replay reads only source and
    // packet guards, never a newer Run prefix, descriptor or response spool.
    let guards = replay_input_guards(draft, run_id)?;
    let until = phase_deadline();
    let input = capture(fs, vault, &guards, until)?;
    let view = SourceView::from_closed_input(fs, &input)?;
    let verified = packet::load_packet(&view, &output.packet_id)?;
    if verified.packet().packet_fingerprint != output.packet_fingerprint
        || verified.dependencies().iter().any(|d| !guards.contains(d))
    {
        return Err(WikiError::invalid(
            "graph generation replay packet/original guards differ",
        ));
    }
    wire::validate_response(&verified, &view, output.response.as_bytes())?;
    let receipt_note = parse_note(writes.receipt.proposed.as_deref().unwrap());
    let time = receipt_note
        .canonical
        .as_ref()
        .and_then(|r| r.string("wiki_occurred_at"))
        .ok_or_else(|| WikiError::invalid("graph generation receipt timestamp missing"))?;
    let envelope = receipt_note.canonical.as_ref().unwrap();
    let sequence = envelope
        .fields()
        .get("wiki_sequence")
        .and_then(serde_json::Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(|| WikiError::invalid("graph generation receipt sequence missing"))?;
    let mut receipt_fields = fields(
        &receipt.receipt_id,
        RecordKind::RunEvent,
        "Immutable usage receipt",
    );
    receipt_fields.insert("wiki_run_id".into(), run_id.as_str().into());
    receipt_fields.insert("wiki_sequence".into(), sequence.into());
    receipt_fields.insert("wiki_event_type".into(), "usage_receipt".into());
    receipt_fields.insert("wiki_occurred_at".into(), time.into());
    if note(
        receipt_fields,
        "lwiki.run-event.v1",
        &ReceiptBody {
            version: 1,
            receipt: receipt.clone(),
        },
    )? != writes.receipt.proposed.as_deref().unwrap()
    {
        return Err(WikiError::invalid(
            "graph generation sealed receipt envelope differs",
        ));
    }
    let (reference, expected) = generation_cache::output_write(
        vault,
        &output.attempt,
        &verified,
        output.response.as_bytes(),
        time,
    )?;
    if receipt.outputs != vec![reference]
        || !same_operations(&[expected], std::slice::from_ref(writes.output))
    {
        return Err(WikiError::invalid(
            "graph generation replay deterministic output differs",
        ));
    }
    check(until)
}

pub(super) fn committed_pair(
    job: &JobLedger,
    attempt: &AttemptRef,
) -> Result<Option<GraphGenerationCommitted>> {
    job.with(false, |_, loaded| {
        let task = &loaded
            .state
            .inspection
            .tasks
            .get(&attempt.task_key)
            .ok_or_else(|| events::corrupt("graph generation completion task missing"))?
            .spec;
        require_task(task, attempt)?;
        if attempt.run_id != job.run_id
            || !loaded
                .state
                .inspection
                .attempts
                .iter()
                .any(|a| &a.attempt == attempt)
        {
            return Err(WikiError::invalid(
                "graph generation completion attempt differs",
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
            || usage.outputs.len() != 1
            || !usage.cache_outputs.is_empty()
            || usage.outputs[0].record.vault_id != job.vault_id
            || usage.outputs[0].record.record_id != generation_cache::output_id(attempt)?
            || usage.outputs[0].record.expected_kind != RecordKind::RunEvent
            || usage.outputs[0].path
                != VaultRelativePath::new(format!(
                    "runs/{}/outputs/{}.md",
                    job.run_id,
                    generation_cache::output_id(attempt)?
                ))?
        {
            return Err(events::corrupt(
                "graph generation committed receipt/output binding differs",
            ));
        }
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
            if Catalog::new(job.fs.clone(), job.vault_id.clone())
                .operation_state()?
                .is_some()
            {
                return Err(events::corrupt(
                    "normalized graph generation receipt has no named committed authority",
                ));
            }
            if let Some(change) =
                loaded
                    .frames
                    .iter()
                    .find_map(|frame| match &frame.event.payload {
                        EventPayload::OutputsCommitted {
                            attempt: recorded,
                            receipt,
                            outputs,
                            cache_outputs,
                            change,
                        } if recorded == attempt
                            && receipt == &reference
                            && outputs == &usage.outputs
                            && cache_outputs.is_empty() =>
                        {
                            Some(change.clone())
                        }
                        _ => None,
                    })
            {
                change
            } else {
                let engine = ChangeEngine::new(job.fs.clone())?;
                let mut found = None;
                let until = phase_deadline();
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
                    events::corrupt("graph generation receipt has no committed Change")
                })?
            }
        };
        let mut all = usage.outputs.clone();
        all.push(reference.clone());
        committed(&job.fs, &change, &all)?;
        Ok(Some(GraphGenerationCommitted {
            change,
            receipt: reference,
            usage,
        }))
    })
}
