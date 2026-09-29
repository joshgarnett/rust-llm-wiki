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
    let mut f = fields(&i.spec.run_id, RecordKind::Run, &i.spec.title);
    f.insert(
        "wiki_status".into(),
        serde_json::to_value(i.state).map_err(|_| WikiError::invalid("run state"))?,
    );
    f.insert(
        "wiki_created_at".into(),
        timestamp(i.spec.created_at_utc_ms)?.into(),
    );
    if let Some(event) = &i.last_event {
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
const CHECKPOINT_MIRROR_BATCH: usize = 256;
const CHECKPOINT_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
pub(super) fn checkpoint_plan(fs: &VaultFs, loaded: &ledger::Loaded) -> Result<ChangeDraft> {
    checkpoint_plan_bounded(
        fs,
        loaded,
        CHECKPOINT_MIRROR_BATCH,
        CHECKPOINT_PAYLOAD_BYTES,
    )
}
// A fully mirrored prefix is sufficient; the immutable operational history is never pruned.
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
    let inspected = engine.inspect(&change.change_id)?;
    if &inspected.prepared != change || inspected.status != ChangeStatus::Committed {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "canonical changeset not committed",
        ));
    }
    for r in outputs {
        output(fs, r)?;
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
pub(super) fn verify_summary_proof(
    fs: &VaultFs,
    note_ref: &DurableOutputRef,
    summary: &CompletedTaskSummary,
    frames: &[JournalFrame],
) -> Result<()> {
    let current = ledger::read(fs, &note_ref.path)?;
    let bytes = if let Some(bytes) = current.filter(|b| Blake3Hash::digest(b) == note_ref.hash) {
        bytes
    } else {
        let engine = ChangeEngine::new(fs.clone())?;
        let mut found = None;
        for id in engine.change_ids()? {
            let inspected = engine.inspect(&id)?;
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
                    if Blake3Hash::digest(&bytes) != note_ref.hash {
                        return Err(events::corrupt("retained checkpoint payload hash differs"));
                    }
                    found = Some(bytes);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        found
            .ok_or_else(|| events::corrupt("checkpoint has no retained committed exact payload"))?
    };
    let parsed = crate::records::parse_note(&bytes);
    if parsed
        .canonical
        .as_ref()
        .is_none_or(|c| c.id() != &note_ref.record.record_id || c.kind() != RecordKind::Run)
    {
        return Err(events::corrupt("checkpoint record identity differs"));
    }
    let plan = decode_run(&bytes)?;
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
    let prefix = super::replay::replay(&frames[..=n], 0)?.inspection;
    if run_bytes(&prefix)? != bytes
        || events::completed_task_summary(
            prefix
                .tasks
                .iter()
                .filter(|(_, task)| task.state == TaskState::Completed)
                .map(|(key, _)| key.clone())
                .collect(),
        )? != *summary
    {
        return Err(events::corrupt(
            "checkpoint summary/bytes differ from actual complete task keys",
        ));
    }
    Ok(())
}
