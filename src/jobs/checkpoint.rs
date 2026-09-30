//! Canonical run/event/receipt plans. Operational proofs are checked against actual bytes.
use super::{events, ledger, types::*};
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::{ChangeDraft, ChangeEngine, ChangeStatus, ExpectedWrite, PreparedChange},
    domain::*,
    records::parse_note,
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};

/// Publish a receipt without derived outputs, or verify an earlier publication.
/// The caller's draft never grants arbitrary write authority: its receipt is
/// regenerated from retained response accounting under the vault writer lock.
pub fn settle_receipt(fs: &VaultFs, job: &JobLedger, plan: MaterializationPlan) -> Result<()> {
    if job.options.policy.dry_run {
        return Err(WikiError::new(
            ErrorCode::OfflineUnavailable,
            "dry-run cannot publish a receipt",
        ));
    }
    let path = event_path(&job.run_id, &plan.receipt.receipt_id)?;
    if fs.root().path() != job.fs.root().path()
        || plan.attempt.run_id != job.run_id
        || plan.receipt.attempt != plan.attempt
        || plan.receipt.receipt_id != receipt_id(&plan.attempt)?
        || !plan.receipt.outputs.is_empty()
        || !plan.receipt.cache_outputs.is_empty()
        || plan.draft.operations.iter().any(|op| op.target != path)
    {
        return Err(WikiError::invalid(
            "receipt-only materialization binding differs",
        ));
    }
    let writer = WriterPermit::acquire(
        fs.root(),
        Duration::from_millis(job.options.lock_timeout_ms),
    )?;
    let engine = ChangeEngine::new(fs.clone())?;
    let catalog = Catalog::new(fs.clone(), job.vault_id.clone());
    engine.recover(&writer, &CatalogGraphValidator, &catalog)?;
    // Recover an apply-before-acknowledgement crash before considering another
    // create. Replay authenticates the original committed changeset manifest.
    let inspection = job.replay()?.inspection;
    let actual = inspection
        .attempts
        .iter()
        .find(|a| a.attempt == plan.attempt)
        .ok_or_else(|| events::corrupt("receipt attempt missing"))?;
    if let Some(reference) = &actual.receipt {
        if receipt(fs, reference)? != plan.receipt
            || !actual.outputs.is_empty()
            || !actual.cache_outputs.is_empty()
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "acknowledged receipt differs from requested receipt",
            ));
        }
        drop(writer);
        job.settle(&plan.attempt)?;
        return Ok(());
    }
    let fresh = receipt_plan(
        job,
        &plan.attempt,
        plan.receipt.output_disposition,
        vec![],
        vec![],
        vec![],
    )?;
    if fresh.receipt != plan.receipt {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "receipt differs from retained response accounting",
        ));
    }
    let bytes = fresh
        .draft
        .operations
        .iter()
        .find(|op| op.target == path)
        .and_then(|op| op.proposed.as_ref())
        .ok_or_else(|| events::corrupt("unacknowledged receipt has no publication operation"))?;
    let reference = DurableOutputRef {
        record: RecordRef {
            vault_id: job.vault_id.clone(),
            record_id: fresh.receipt.receipt_id.clone(),
            expected_kind: RecordKind::RunEvent,
        },
        path,
        hash: Blake3Hash::digest(bytes),
    };
    let prepared = engine.prepare(&writer, fresh.draft)?.prepared;
    engine.apply(&writer, &prepared, &CatalogGraphValidator, &catalog)?;
    drop(writer);
    job.outputs_committed(&plan.attempt, &prepared, reference, vec![], vec![])?;
    job.settle(&plan.attempt)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunPlanV1 {
    pub version: u32,
    pub spec: RunSpec,
    pub spec_hash: Blake3Hash,
    pub effective_limits: LifetimeLimits,
    pub effective_deadline_utc_ms: i64,
    pub tasks: BTreeMap<Blake3Hash, TaskInspection>,
    pub checkpoint: Option<EventRef>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventBody {
    version: u32,
    event: LedgerEvent,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptBody {
    version: u32,
    receipt: UsageReceipt,
}
pub(super) fn run_path(run: &RecordId) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("runs/{run}/run.md"))
}
pub(super) fn event_path(run: &RecordId, event: &RecordId) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("runs/{run}/events/{event}.md"))
}
pub(super) fn receipt_id(attempt: &AttemptRef) -> Result<RecordId> {
    RecordId::new(format!(
        "run_event_receipt_{}",
        Blake3Hash::digest(
            serde_json::to_vec(attempt).map_err(|_| WikiError::invalid("attempt encoding"))?
        )
        .hex()
    ))
}
pub(super) fn timestamp(ms: i64) -> Result<String> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .map_err(|_| WikiError::invalid("UTC timestamp invalid"))?
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| WikiError::invalid("UTC timestamp formatting"))
}
fn fields(id: &RecordId, kind: RecordKind, title: &str) -> BTreeMap<String, serde_json::Value> {
    BTreeMap::from([
        ("wiki_schema".into(), "1".into()),
        ("wiki_id".into(), id.as_str().into()),
        ("wiki_kind".into(), kind.as_str().into()),
        ("title".into(), title.into()),
    ])
}
fn note(
    fields: BTreeMap<String, serde_json::Value>,
    fence: &str,
    value: &impl Serialize,
) -> Result<Vec<u8>> {
    CanonicalRecord::new(fields.clone())?;
    let mut out = String::from("---\n");
    for (key, value) in fields {
        out.push_str(&key);
        out.push_str(": ");
        out.push_str(
            &serde_json::to_string(&value)
                .map_err(|_| WikiError::invalid("note field encoding"))?,
        );
        out.push('\n');
    }
    out.push_str("---\n\n```");
    out.push_str(fence);
    out.push('\n');
    out.push_str(
        &serde_json::to_string(value).map_err(|_| WikiError::invalid("note payload encoding"))?,
    );
    out.push_str("\n```\n");
    if out.len() > crate::changes::prepare::MAX_PAYLOAD_BYTES {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "run canonical payload exceeds ceiling",
        ));
    }
    Ok(out.into_bytes())
}
pub(super) fn decode_run(bytes: &[u8]) -> Result<RunPlanV1> {
    decode_fence(bytes, "lwiki.run-plan.v1")
}
fn decode_fence<T: serde::de::DeserializeOwned>(bytes: &[u8], fence: &str) -> Result<T> {
    let parsed = parse_note(bytes);
    if parsed.canonical.is_none() {
        return Err(events::corrupt("run/receipt note is not canonical"));
    }
    let text = std::str::from_utf8(parsed.body())
        .map_err(|_| events::corrupt("canonical payload UTF-8 invalid"))?;
    let prefix = format!("\n```{fence}\n");
    let body = text
        .strip_prefix(&prefix)
        .and_then(|v| v.strip_suffix("\n```\n"))
        .ok_or_else(|| events::corrupt("canonical run fence differs"))?;
    crate::changes::prepare::strict_json(body.as_bytes())
        .map_err(|_| events::corrupt("canonical payload invalid"))
}
pub(super) fn run_bytes(i: &LedgerInspection) -> Result<Vec<u8>> {
    run_bytes_version(i, 1)
}
fn run_bytes_version(i: &LedgerInspection, formatter: u32) -> Result<Vec<u8>> {
    if !matches!(formatter, 1 | 2) {
        return Err(events::corrupt("unknown run formatter"));
    }
    let mut f = fields(&i.spec.run_id, RecordKind::Run, &i.spec.title);
    f.insert(
        "wiki_status".into(),
        serde_json::to_value(i.state).map_err(|_| WikiError::invalid("run state"))?,
    );
    f.insert(
        "wiki_created_at".into(),
        timestamp(i.spec.created_at_utc_ms)?.into(),
    );
    if formatter == 1
        && let Some(event) = &i.last_event
    {
        f.insert(
            "wiki_checkpoint_event_id".into(),
            event.event_id.as_str().into(),
        );
    }
    note(
        f,
        "lwiki.run-plan.v1",
        &RunPlanV1 {
            version: 1,
            spec: i.spec.clone(),
            spec_hash: i.spec_hash.clone(),
            effective_limits: i.effective_limits.clone(),
            effective_deadline_utc_ms: i.effective_deadline_utc_ms,
            tasks: i.tasks.clone(),
            checkpoint: i.last_event.clone(),
        },
    )
}
/// Preserve the exact old bootstrap draft when retrying a v1 creation that
/// already retained its genesis mirror operation. New runs allocate no mirror.
pub(super) fn legacy_bootstrap_mirror(
    fs: &VaultFs,
    spec: &RunSpec,
    genesis: &JournalFrame,
    run_hash: &Blake3Hash,
) -> Result<Option<ExpectedWrite>> {
    let path = event_path(&spec.run_id, &genesis.event.event_id)?;
    let run_path = run_path(&spec.run_id)?;
    let bytes = event_bytes(&genesis.event)?;
    let hash = Blake3Hash::digest(&bytes);
    let engine = ChangeEngine::new(fs.clone())?;
    for id in engine.change_ids()? {
        let inspected = engine.inspect_history(&id)?;
        if !matches!(
            inspected.status,
            ChangeStatus::Aborted | ChangeStatus::Conflict
        ) && inspected.manifest.title == "Create durable planned run"
            && inspected.manifest.allocated_ids.get("run") == Some(&spec.run_id)
            && inspected.manifest.operations.len() == 2
            && inspected.manifest.operations.iter().any(|op| {
                op.target == path
                    && op.before == ExpectedState::Absent
                    && op.after == ExpectedState::Hash(hash.clone())
            })
            && inspected.manifest.operations.iter().any(|op| {
                op.target == run_path
                    && op.before == ExpectedState::Absent
                    && op.after == ExpectedState::Hash(run_hash.clone())
            })
        {
            return Ok(Some(write(path, ExpectedState::Absent, bytes)));
        }
    }
    Ok(None)
}
pub(super) fn event_bytes(event: &LedgerEvent) -> Result<Vec<u8>> {
    let mut f = fields(&event.event_id, RecordKind::RunEvent, "Immutable job event");
    f.insert("wiki_run_id".into(), event.run_id.as_str().into());
    f.insert("wiki_sequence".into(), event.sequence.into());
    f.insert(
        "wiki_event_type".into(),
        events::event_type(&event.payload).into(),
    );
    f.insert(
        "wiki_occurred_at".into(),
        timestamp(event.occurred_at_utc_ms)?.into(),
    );
    note(
        f,
        "lwiki.run-event.v1",
        &EventBody {
            version: 1,
            event: event.clone(),
        },
    )
}
pub(super) fn output(fs: &VaultFs, r: &DurableOutputRef) -> Result<()> {
    let bytes = ledger::read(fs, &r.path)?
        .ok_or_else(|| WikiError::new(ErrorCode::FreshnessConflict, "durable output missing"))?;
    if Blake3Hash::digest(&bytes) != r.hash {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "durable output changed",
        ));
    }
    let note = parse_note(&bytes);
    if note
        .canonical
        .as_ref()
        .is_none_or(|v| v.id() != &r.record.record_id || v.kind() != r.record.expected_kind)
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "output identity or kind changed",
        ));
    }
    Ok(())
}
pub(crate) fn receipt(fs: &VaultFs, r: &DurableOutputRef) -> Result<UsageReceipt> {
    output(fs, r)?;
    let b = ledger::read(fs, &r.path)?.ok_or_else(|| events::corrupt("receipt missing"))?;
    let body: ReceiptBody = decode_fence(&b, "lwiki.run-event.v1")?;
    if body.version != 1
        || body.receipt.version != 1
        || body.receipt.receipt_id != r.record.record_id
    {
        return Err(events::corrupt("receipt identity invalid"));
    }
    Ok(body.receipt)
}
pub(super) fn draft(title: &str, spec: &RunSpec) -> ChangeDraft {
    ChangeDraft {
        title: title.into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::from([("run".into(), spec.run_id.clone())]),
        read_preconditions: vec![],
        operations: vec![],
    }
}
pub(super) fn write(
    path: VaultRelativePath,
    expected: ExpectedState,
    bytes: Vec<u8>,
) -> ExpectedWrite {
    ExpectedWrite {
        target: path,
        expected,
        proposed: Some(bytes),
        apply_after: vec![],
    }
}
/// A locator into retained exact history, never standalone accounting authority.
/// Version 1 run formatting remains frozen so old bytes can be regenerated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactCheckpointProof {
    version: u32,
    vault_id: RecordId,
    run_id: RecordId,
    spec_hash: Blake3Hash,
    genesis_hash: Blake3Hash,
    through: EventRef,
    formatter_version: u32,
    run_note: DurableOutputRef,
    completed_tasks: CompletedTaskSummary,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactCheckpointEnvelope {
    proof: CompactCheckpointProof,
    checksum: Blake3Hash,
}
fn compact_path(run: &DurableOutputRef) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!(
        "runs/{}/checkpoints/{}.json",
        run.record.record_id,
        run.hash.hex()
    ))
}
fn completed_summary(i: &LedgerInspection) -> Result<CompletedTaskSummary> {
    events::completed_task_summary(
        i.tasks
            .iter()
            .filter(|(_, task)| task.state == TaskState::Completed)
            .map(|(key, _)| key.clone())
            .collect(),
    )
}
fn compact_proof(
    frames: &[JournalFrame],
    through: usize,
    formatter_version: u32,
) -> Result<(CompactCheckpointProof, Vec<u8>)> {
    let frame = frames
        .get(through)
        .ok_or_else(|| events::corrupt("compact checkpoint prefix absent"))?;
    let prefix = super::replay::replay(&frames[..=through], 0)?.inspection;
    let bytes = run_bytes_version(&prefix, formatter_version)?;
    let run_note = DurableOutputRef {
        record: RecordRef {
            vault_id: prefix.spec.vault_id.clone(),
            record_id: prefix.spec.run_id.clone(),
            expected_kind: RecordKind::Run,
        },
        path: run_path(&prefix.spec.run_id)?,
        hash: Blake3Hash::digest(&bytes),
    };
    Ok((
        CompactCheckpointProof {
            version: 2,
            vault_id: prefix.spec.vault_id.clone(),
            run_id: prefix.spec.run_id.clone(),
            spec_hash: prefix.spec_hash.clone(),
            genesis_hash: frames[0].checksum.clone(),
            through: events::event_ref(frame),
            formatter_version,
            run_note,
            completed_tasks: completed_summary(&prefix)?,
        },
        bytes,
    ))
}
fn compact_json(value: &impl Serialize, maximum: usize) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| events::corrupt("compact proof encoding"))?;
    if bytes.len() > maximum {
        return Err(events::corrupt("compact proof exceeds bound"));
    }
    Ok(bytes)
}
fn compact_bytes(proof: CompactCheckpointProof) -> Result<Vec<u8>> {
    let checksum = Blake3Hash::digest(compact_json(&proof, EVENT_MAX_BYTES)?);
    compact_json(
        &CompactCheckpointEnvelope { proof, checksum },
        EVENT_MAX_BYTES,
    )
}
fn add_compact_write(
    fs: &VaultFs,
    draft: &mut ChangeDraft,
    proof: CompactCheckpointProof,
) -> Result<()> {
    let path = compact_path(&proof.run_note)?;
    let bytes = compact_bytes(proof)?;
    match ledger::read(fs, &path)? {
        Some(actual) if actual == bytes => {}
        Some(_) => {
            return Err(events::corrupt(
                "immutable compact checkpoint proof changed",
            ));
        }
        None => draft
            .operations
            .push(write(path, ExpectedState::Absent, bytes)),
    }
    Ok(())
}

#[cfg(test)]
const CHECKPOINT_MIRROR_BATCH: usize = 256;
#[cfg(test)]
const CHECKPOINT_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
pub(super) fn checkpoint_plan(fs: &VaultFs, loaded: &ledger::Loaded) -> Result<ChangeDraft> {
    let inspection = &loaded.state.inspection;
    let expected = inspection
        .run_note
        .as_ref()
        .ok_or_else(|| events::corrupt("run binding missing"))?;
    let mut draft = draft("Checkpoint durable run prefix", &inspection.spec);
    // A checkpoint acknowledgment alone does not change the represented run.
    // Do not manufacture another publication merely to mirror that acknowledgment.
    let through = loaded
        .frames
        .iter()
        .rposition(|frame| !matches!(frame.event.payload, EventPayload::Checkpoint { .. }))
        .ok_or_else(|| events::corrupt("checkpoint genesis absent"))?;
    let (proof, bytes) = compact_proof(&loaded.frames, through, 2)?;
    if proof.run_note == *expected {
        return Ok(draft);
    }
    add_compact_write(fs, &mut draft, proof)?;
    let mut run = write(
        run_path(&inspection.spec.run_id)?,
        ExpectedState::Hash(expected.hash.clone()),
        bytes,
    );
    run.apply_after = draft
        .operations
        .iter()
        .map(|op| op.target.clone())
        .collect();
    draft.operations.push(run);
    Ok(draft)
}
/// Legacy v1 fixture writer; production checkpoints use the compact v2 proof.
/// The immutable operational history is never pruned.
#[cfg(test)]
pub(super) fn checkpoint_plan_bounded(
    fs: &VaultFs,
    loaded: &ledger::Loaded,
    max_missing: usize,
    max_bytes: usize,
) -> Result<ChangeDraft> {
    if max_missing == 0
        || max_missing > CHECKPOINT_MIRROR_BATCH
        || max_bytes == 0
        || max_bytes > CHECKPOINT_PAYLOAD_BYTES
    {
        return Err(WikiError::invalid("invalid checkpoint batch bounds"));
    }
    let i = &loaded.state.inspection;
    let expected = i
        .run_note
        .as_ref()
        .ok_or_else(|| events::corrupt("run binding missing"))?
        .hash
        .clone();
    let mut d = draft("Checkpoint durable run prefix", &i.spec);
    let mut payload_bytes = 0usize;
    let mut through = None;
    for (index, frame) in loaded.frames.iter().enumerate() {
        let path = event_path(&i.spec.run_id, &frame.event.event_id)?;
        let proposed = event_bytes(&frame.event)?;
        match ledger::read(fs, &path)? {
            Some(actual) if actual == proposed => {}
            Some(_) => {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "immutable run event edited",
                ));
            }
            None => {
                // Half the payload budget is reserved for the exact prefix run note.
                if d.operations.len() == max_missing
                    || payload_bytes
                        .checked_add(proposed.len())
                        .is_none_or(|n| n > max_bytes / 2)
                {
                    break;
                }
                payload_bytes += proposed.len();
                d.operations
                    .push(write(path, ExpectedState::Absent, proposed));
            }
        }
        through = Some(index);
    }
    let through = through.ok_or_else(|| {
        WikiError::new(
            ErrorCode::BudgetExceeded,
            "checkpoint prefix does not fit batch",
        )
    })?;
    let prefix = super::replay::replay(&loaded.frames[..=through], 0)?.inspection;
    let bytes = run_bytes(&prefix)?;
    if payload_bytes
        .checked_add(bytes.len())
        .is_none_or(|n| n > max_bytes)
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "checkpoint run and event payloads exceed batch bound",
        ));
    }
    if Blake3Hash::digest(&bytes) != expected {
        let mut op = write(
            run_path(&i.spec.run_id)?,
            ExpectedState::Hash(expected),
            bytes,
        );
        op.apply_after = d.operations.iter().map(|v| v.target.clone()).collect();
        d.operations.push(op);
    }
    Ok(d)
}
pub(super) fn receipt_plan_locked(
    job: &JobLedger,
    loaded: &ledger::Loaded,
    attempt: &AttemptRef,
    disposition: OutputDisposition,
    outputs: Vec<DurableOutputRef>,
    cache_outputs: Vec<VectorCacheRef>,
    extra_ops: Vec<ExpectedWrite>,
) -> Result<MaterializationPlan> {
    let a = loaded
        .state
        .inspection
        .attempts
        .iter()
        .find(|a| &a.attempt == attempt)
        .ok_or_else(|| events::corrupt("unknown receipt attempt"))?;
    if !matches!(
        a.phase,
        AttemptPhase::Received | AttemptPhase::OutputCommitted | AttemptPhase::Settled
    ) {
        return Err(events::corrupt("receipt requires durable response"));
    }
    let metadata = loaded
        .state
        .received_meta
        .get(&attempt.attempt_id)
        .ok_or_else(|| events::corrupt("bound response metadata missing"))?;
    let received = loaded
        .frames
        .iter()
        .find(
            |f| matches!(&f.event.payload,EventPayload::Received{spool}if &spool.attempt==attempt),
        )
        .ok_or_else(|| events::corrupt("received event missing"))?;
    let id = receipt_id(attempt)?;
    let billing =
        if super::budgets::actual_allowance(&a.bound, &metadata.usage, &metadata.computed_cost)
            .is_some()
        {
            BillingDisposition::KnownSettled
        } else {
            BillingDisposition::UnknownReserved
        };
    let receipt = UsageReceipt {
        version: 1,
        receipt_id: id.clone(),
        attempt: attempt.clone(),
        capability: a.bound.capability,
        profile_id: a.bound.profile_id.clone(),
        endpoint_fingerprint: a.bound.endpoint_fingerprint.clone(),
        input_hash: a.bound.input_hash.clone(),
        requested_model: a.bound.requested_model.clone(),
        returned_model: metadata.returned_model.clone(),
        provider_request_id: metadata.provider_request_id.clone(),
        usage: metadata.usage.clone(),
        billing,
        reservation: a.allowance.cost.clone(),
        computed_cost: metadata.computed_cost.clone(),
        rate_card_fingerprint: a.bound.rate_card.as_ref().map(|v| v.fingerprint.clone()),
        output_disposition: disposition,
        outputs,
        cache_outputs,
        failure_code: metadata.failure_code.clone(),
    };
    let mut f = fields(&id, RecordKind::RunEvent, "Immutable usage receipt");
    f.insert("wiki_run_id".into(), job.run_id.as_str().into());
    f.insert("wiki_sequence".into(), received.event.sequence.into());
    f.insert("wiki_event_type".into(), "usage_receipt".into());
    f.insert(
        "wiki_occurred_at".into(),
        timestamp(received.event.occurred_at_utc_ms)?.into(),
    );
    let bytes = note(
        f,
        "lwiki.run-event.v1",
        &ReceiptBody {
            version: 1,
            receipt: receipt.clone(),
        },
    )?;
    if bytes.len() > EVENT_MAX_BYTES {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "usage receipt exceeds event ceiling",
        ));
    }
    let path = event_path(&job.run_id, &id)?;
    let mut d = draft(
        "Retain accounted response receipt",
        &loaded.state.inspection.spec,
    );
    if disposition == OutputDisposition::Validated {
        d.read_preconditions = loaded
            .state
            .inspection
            .spec
            .scope
            .read_preconditions
            .clone();
        if let Some(task) = loaded.state.inspection.tasks.get(&attempt.task_key) {
            for dep in &task.spec.source_bindings {
                if !d.read_preconditions.contains(dep) {
                    d.read_preconditions.push(dep.clone())
                }
            }
        }
    } else if !receipt.outputs.is_empty()
        || !receipt.cache_outputs.is_empty()
        || !extra_ops.is_empty()
    {
        return Err(WikiError::invalid(
            "unknown/rejected receipt cannot activate derived outputs",
        ));
    }
    if let Some(run) = &loaded.state.inspection.run_note {
        d.read_preconditions.push(crate::changes::ReadDependency {
            path: run.path.clone(),
            expected: ExpectedState::Hash(run.hash.clone()),
        });
    }
    d.allocated_ids.insert("receipt".into(), id);
    d.operations = extra_ops;
    match ledger::read(&job.fs, &path)? {
        Some(actual) if actual == bytes => {}
        Some(_) => {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "immutable receipt bytes conflict",
            ));
        }
        None => {
            let mut op = write(path, ExpectedState::Absent, bytes);
            op.apply_after = d.operations.iter().map(|v| v.target.clone()).collect();
            d.operations.push(op)
        }
    }
    Ok(MaterializationPlan {
        attempt: attempt.clone(),
        receipt,
        draft: d,
    })
}
pub(super) fn committed(
    fs: &VaultFs,
    change: &PreparedChange,
    outputs: &[DurableOutputRef],
) -> Result<()> {
    let engine = ChangeEngine::new(fs.clone())?;
    let inspected = engine.inspect_history(&change.change_id)?;
    if &inspected.prepared != change || inspected.status != ChangeStatus::Committed {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "canonical changeset not committed",
        ));
    }
    for r in outputs {
        output(fs, r)?;
        engine.prove_committed_output(change, &r.path, &r.hash)?;
        if r.record.vault_id != *engine.vault_id()
            || !inspected
                .manifest
                .operations
                .iter()
                .any(|op| op.target == r.path && op.after == ExpectedState::Hash(r.hash.clone()))
        {
            return Err(events::corrupt("output not bound to committed changeset"));
        }
    }
    Ok(())
}

/// Trusted later materializers supply validated disposition and guarded output operations.
pub(crate) fn receipt_plan(
    job: &JobLedger,
    attempt: &AttemptRef,
    disposition: OutputDisposition,
    outputs: Vec<DurableOutputRef>,
    cache_outputs: Vec<VectorCacheRef>,
    extra_ops: Vec<ExpectedWrite>,
) -> Result<MaterializationPlan> {
    job.with(true, |_, loaded| {
        receipt_plan_locked(
            job,
            loaded,
            attempt,
            disposition,
            outputs,
            cache_outputs,
            extra_ops,
        )
    })
}
/// Locate historical publication without reading before/proposed payload copies.
fn committed_hash(
    fs: &VaultFs,
    path: &VaultRelativePath,
    hash: &Blake3Hash,
) -> Result<PreparedChange> {
    let engine = ChangeEngine::new(fs.clone())?;
    for id in engine.change_ids()? {
        let inspected = engine.inspect_history(&id)?;
        if inspected.status == ChangeStatus::Committed
            && inspected
                .manifest
                .operations
                .iter()
                .any(|op| &op.target == path && op.after == ExpectedState::Hash(hash.clone()))
        {
            engine.prove_committed_output(&inspected.prepared, path, hash)?;
            return Ok(inspected.prepared);
        }
    }
    Err(events::corrupt(
        "checkpoint output has no retained committed publication",
    ))
}
fn legacy_summary_bytes(fs: &VaultFs, note_ref: &DurableOutputRef) -> Result<Vec<u8>> {
    let current = ledger::read(fs, &note_ref.path)?;
    if let Some(bytes) = current.filter(|bytes| Blake3Hash::digest(bytes) == note_ref.hash) {
        return Ok(bytes);
    }
    let engine = ChangeEngine::new(fs.clone())?;
    for id in engine.change_ids()? {
        let inspected = engine.inspect_history(&id)?;
        if inspected.status != ChangeStatus::Committed {
            continue;
        }
        for op in inspected.manifest.operations {
            if op.target == note_ref.path
                && op.after == ExpectedState::Hash(note_ref.hash.clone())
                && let Some(payload) = op.after_payload
            {
                let bytes = ledger::read(fs, &payload.path)?
                    .ok_or_else(|| events::corrupt("retained checkpoint payload missing"))?;
                if Blake3Hash::digest(&bytes) != note_ref.hash
                    || bytes.len() as u64 != payload.byte_len
                {
                    return Err(events::corrupt(
                        "retained checkpoint payload hash/length differs",
                    ));
                }
                return Ok(bytes);
            }
        }
    }
    Err(events::corrupt(
        "checkpoint has no retained committed exact payload",
    ))
}
fn proof_for_bytes(
    note_ref: &DurableOutputRef,
    bytes: &[u8],
    frames: &[JournalFrame],
) -> Result<CompactCheckpointProof> {
    let plan = decode_run(bytes)?;
    let event = plan
        .checkpoint
        .ok_or_else(|| events::corrupt("checkpoint prefix absent"))?;
    let n = usize::try_from(event.sequence)
        .map_err(|_| events::corrupt("checkpoint prefix too large"))?;
    if frames
        .get(n)
        .is_none_or(|frame| events::event_ref(frame) != event)
    {
        return Err(events::corrupt(
            "checkpoint prefix differs from durable history",
        ));
    }
    for formatter in [1, 2] {
        let (proof, regenerated) = compact_proof(frames, n, formatter)?;
        if proof.run_note == *note_ref && regenerated == bytes {
            return Ok(proof);
        }
    }
    Err(events::corrupt(
        "checkpoint bytes/identity differ from exact operational prefix",
    ))
}
fn read_compact(
    fs: &VaultFs,
    note_ref: &DurableOutputRef,
    frames: &[JournalFrame],
) -> Result<Option<CompactCheckpointProof>> {
    let path = compact_path(note_ref)?;
    let Some(bytes) = crate::changes::prepare::read_bounded(fs, &path, EVENT_MAX_BYTES)? else {
        return Ok(None);
    };
    let envelope: CompactCheckpointEnvelope = crate::changes::prepare::strict_json(&bytes)
        .map_err(|_| events::corrupt("compact checkpoint proof invalid"))?;
    let proof = &envelope.proof;
    if envelope.checksum != Blake3Hash::digest(compact_json(proof, EVENT_MAX_BYTES)?)
        || proof.version != 2
        || !matches!(proof.formatter_version, 1 | 2)
        || proof.run_note != *note_ref
    {
        return Err(events::corrupt(
            "compact checkpoint proof binding/checksum differs",
        ));
    }
    let n = usize::try_from(proof.through.sequence)
        .map_err(|_| events::corrupt("compact checkpoint prefix too large"))?;
    let (expected, _) = compact_proof(frames, n, proof.formatter_version)?;
    if expected != *proof || compact_bytes(expected)? != bytes {
        return Err(events::corrupt(
            "compact checkpoint differs from exact retained prefix",
        ));
    }
    // Neither a checksummed sidecar nor regenerated bytes alone prove publication.
    committed_hash(fs, &path, &Blake3Hash::digest(&bytes))?;
    committed_hash(fs, &note_ref.path, &note_ref.hash)?;
    Ok(Some(envelope.proof))
}
pub(super) fn verify_summary_proof(
    fs: &VaultFs,
    note_ref: &DurableOutputRef,
    summary: &CompletedTaskSummary,
    frames: &[JournalFrame],
) -> Result<()> {
    let proof = match read_compact(fs, note_ref, frames)? {
        Some(proof) => proof,
        None => {
            let bytes = legacy_summary_bytes(fs, note_ref)?;
            let proof = proof_for_bytes(note_ref, &bytes, frames)?;
            committed_hash(fs, &note_ref.path, &note_ref.hash)?;
            proof
        }
    };
    if proof.completed_tasks != *summary {
        return Err(events::corrupt(
            "checkpoint summary differs from actual complete task keys",
        ));
    }
    Ok(())
}
/// Verify an actual publication before its operational checkpoint acknowledgment.
/// Legacy plans still require their exact immutable event mirrors; compact plans
/// require committed proof bytes and regenerate from the original event prefix.
pub(super) fn verify_checkpoint_commit(
    fs: &VaultFs,
    change: &PreparedChange,
    run: &DurableOutputRef,
    frames: &[JournalFrame],
) -> Result<CompletedTaskSummary> {
    output(fs, run)?;
    let bytes =
        ledger::read(fs, &run.path)?.ok_or_else(|| events::corrupt("checkpoint run missing"))?;
    let expected = proof_for_bytes(run, &bytes, frames)?;
    ChangeEngine::new(fs.clone())?.prove_committed_output(change, &run.path, &run.hash)?;
    if let Some(proof) = read_compact(fs, run, frames)? {
        if proof != expected {
            return Err(events::corrupt(
                "committed compact checkpoint prefix differs",
            ));
        }
    } else {
        let n = expected.through.sequence as usize;
        for frame in &frames[..=n] {
            let path = event_path(&run.record.record_id, &frame.event.event_id)?;
            if ledger::read(fs, &path)?.as_ref() != Some(&event_bytes(&frame.event)?) {
                return Err(events::corrupt(
                    "legacy checkpoint event mirror missing/edited",
                ));
            }
        }
    }
    Ok(expected.completed_tasks)
}
/// Pure bounded migration plan. Original history and publication must verify
/// before retaining a compact locator; applying this draft never deletes bytes.
pub(crate) fn compact_summary_migration_plan(
    fs: &VaultFs,
    note_ref: &DurableOutputRef,
    summary: &CompletedTaskSummary,
    frames: &[JournalFrame],
) -> Result<ChangeDraft> {
    verify_summary_proof(fs, note_ref, summary, frames)?;
    let proof = match read_compact(fs, note_ref, frames)? {
        Some(proof) => proof,
        None => proof_for_bytes(note_ref, &legacy_summary_bytes(fs, note_ref)?, frames)?,
    };
    let prefix = super::replay::replay(&frames[..=proof.through.sequence as usize], 0)?.inspection;
    let mut draft = draft("Retain compact historical checkpoint proof", &prefix.spec);
    add_compact_write(fs, &mut draft, proof)?;
    Ok(draft)
}

/// Complete, head-anchored history for storage migration. Read-only and never
/// returns dispatch authority; cleanup holds the run lock before using it to delete.
pub(crate) struct StorageRunHistory {
    pub frames: Vec<JournalFrame>,
    pub checkpoints: Vec<(DurableOutputRef, CompletedTaskSummary)>,
    pub mirrors: Vec<(VaultRelativePath, Blake3Hash, u64)>,
    pub binds_vault_marker: bool,
}
pub(crate) fn storage_run_history(fs: &VaultFs, run_id: &RecordId) -> Result<StorageRunHistory> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Head {
        version: u32,
        run_id: RecordId,
        spec_hash: Blake3Hash,
        genesis_hash: Blake3Hash,
        last_event: EventRef,
        journal_length: u64,
    }
    let read = |name: &str, maximum| {
        crate::changes::prepare::read_bounded(
            fs,
            &VaultRelativePath::new(format!(".wiki/state/jobs/{run_id}/{name}"))?,
            maximum,
        )?
        .ok_or_else(|| events::corrupt("storage migration requires complete job history and head"))
    };
    let bytes = read("journal.bin", JOURNAL_MAX_BYTES as usize)?;
    let decoded = events::decode(&bytes, run_id)?;
    if decoded.torn_tail {
        return Err(events::corrupt(
            "recover torn job history before storage migration",
        ));
    }
    let head: Head =
        crate::changes::prepare::strict_json(&read("checkpoint.json", METADATA_MAX_BYTES)?)?;
    let index = usize::try_from(head.last_event.sequence)
        .map_err(|_| events::corrupt("storage head overflow"))?;
    let end = decoded
        .frames
        .get(index)
        .ok_or_else(|| events::corrupt("storage job history suffix missing"))?;
    let length = decoded.frames[..=index]
        .iter()
        .try_fold(0u64, |sum, frame| {
            sum.checked_add(events::encode(&frame.event)?.1.len() as u64)
                .ok_or_else(|| events::corrupt("storage head length overflow"))
        })?;
    let state = super::replay::replay(&decoded.frames, decoded.safe_offset)?.inspection;
    if head.version != 1
        || &head.run_id != run_id
        || head.spec_hash != state.spec_hash
        || head.genesis_hash != decoded.frames[0].checksum
        || events::event_ref(end) != head.last_event
        || length != head.journal_length
        || length > decoded.safe_offset
    {
        return Err(events::corrupt(
            "storage head differs from complete original history",
        ));
    }
    if state.spec.vault_id != ChangeEngine::new(fs.clone())?.vault_id().clone() {
        return Err(events::corrupt("storage job belongs to another vault"));
    }
    let current = state
        .run_note
        .as_ref()
        .ok_or_else(|| events::corrupt("storage run reference absent"))?;
    output(fs, current)?;
    let referenced = parse_note(
        &ledger::read(fs, &current.path)?.ok_or_else(|| events::corrupt("storage run missing"))?,
    )
    .canonical
    .and_then(|r| r.string("wiki_checkpoint_event_id").map(str::to_owned));
    let mut checkpoints = Vec::new();
    let mut mirrors = Vec::new();
    for frame in &decoded.frames {
        if let EventPayload::Checkpoint {
            run_note,
            completed_tasks,
            ..
        } = &frame.event.payload
        {
            verify_summary_proof(fs, run_note, completed_tasks, &decoded.frames)?;
            checkpoints.push((run_note.clone(), completed_tasks.clone()));
        }
        if referenced.as_deref() != Some(frame.event.event_id.as_str()) {
            let bytes = event_bytes(&frame.event)?;
            mirrors.push((
                event_path(run_id, &frame.event.event_id)?,
                Blake3Hash::digest(&bytes),
                bytes.len() as u64,
            ));
        }
    }
    Ok(StorageRunHistory {
        frames: decoded.frames,
        checkpoints,
        mirrors,
        binds_vault_marker: state
            .spec
            .scope
            .read_preconditions
            .iter()
            .any(|p| p.path.as_str() == "WIKI.md")
            && !matches!(state.state, RunState::Completed | RunState::Failed),
    })
}
