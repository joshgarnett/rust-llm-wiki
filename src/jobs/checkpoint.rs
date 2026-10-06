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

/// One internally regenerated job plan; callers cannot construct this seal.
pub(crate) struct ValidatedJobDraft {
    draft: ChangeDraft,
    operation: crate::changes::indexed_refresh::IndexedWriteOperation,
}
impl ValidatedJobDraft {
    pub(crate) fn into_parts(
        self,
    ) -> (
        ChangeDraft,
        crate::changes::indexed_refresh::IndexedWriteOperation,
    ) {
        (self.draft, self.operation)
    }
}

/// Checks the payload boundary again on replay without acquiring a run lock or
/// consulting a possibly superseded ledger prefix. The retained proof binds the
/// operational validation performed before preparation.
pub(crate) fn validate_job_payloads(
    fs: &VaultFs,
    vault: &RecordId,
    operation: &crate::changes::indexed_refresh::IndexedWriteOperation,
    draft: &ChangeDraft,
) -> Result<()> {
    use crate::changes::indexed_refresh::IndexedWriteOperation;
    let IndexedWriteOperation::JobBatch {
        run_id,
        records,
        checkpoint,
    } = operation
    else {
        return Err(WikiError::invalid("not a job publication"));
    };
    operation.validate()?;
    if draft.origin.is_some()
        || draft.inverse_of.is_some()
        || draft.allocated_ids.get("run") != Some(run_id)
        || draft.allocated_ids.iter().any(|(key, id)| {
            key != "run"
                && !(key == "receipt"
                    && records
                        .iter()
                        .any(|r| r.kind == RecordKind::RunEvent && &r.id == id))
        })
    {
        return Err(WikiError::invalid("job allocation/origin differs"));
    }
    let shape = match draft.title.as_str() {
        "Create durable planned run" => {
            checkpoint.is_none() && records.iter().any(|r| r.kind == RecordKind::Run)
        }
        "Checkpoint durable run prefix" => {
            checkpoint.is_some() && records.len() == 1 && records[0].kind == RecordKind::Run
        }
        "Retain accounted response receipt" => {
            checkpoint.is_none() && records.len() == 1 && records[0].kind == RecordKind::RunEvent
        }
        _ => false,
    };
    if !shape {
        return Err(WikiError::invalid(
            "job draft is not a known bootstrap/checkpoint/receipt plan",
        ));
    }
    let asset = checkpoint
        .as_ref()
        .and_then(|c| draft.operations.iter().find(|op| op.target == c.path));
    if draft.operations.len() != records.len() + usize::from(asset.is_some()) {
        return Err(WikiError::invalid(
            "job publication includes an unbound operation",
        ));
    }
    for target in records {
        let op = draft
            .operations
            .iter()
            .find(|op| op.target == target.path)
            .ok_or_else(|| WikiError::invalid("bound job target missing"))?;
        let bytes = op
            .proposed
            .as_deref()
            .ok_or_else(|| WikiError::invalid("job deletion forbidden"))?;
        let parsed = parse_note(bytes);
        let record = parsed
            .canonical
            .as_ref()
            .ok_or_else(|| WikiError::invalid("job envelope invalid"))?;
        if record.id() != &target.id || record.kind() != target.kind {
            return Err(WikiError::invalid("job envelope identity differs"));
        }
        if target.kind == RecordKind::Run {
            let plan = decode_run(bytes)?;
            if plan.version != 1
                || record
                    .string("wiki_run_id")
                    .is_some_and(|id| id != run_id.as_str())
                || plan.spec.run_id != *run_id
                || plan.spec.vault_id != *vault
                || plan.spec_hash != events::spec_hash(&plan.spec)?
            {
                return Err(WikiError::invalid(
                    "Run body belongs to another run/vault/spec",
                ));
            }
            if let Some(c) = checkpoint {
                if op.expected == ExpectedState::Absent
                    || Blake3Hash::digest(bytes) != c.run_hash
                    || (asset.is_some() && op.apply_after != vec![c.path.clone()])
                    || (asset.is_none() && !op.apply_after.is_empty())
                {
                    return Err(WikiError::invalid(
                        "compact checkpoint Run guard/hash/order differs",
                    ));
                }
            } else if op.expected != ExpectedState::Absent || !op.apply_after.is_empty() {
                return Err(WikiError::invalid(
                    "Run replacement requires compact checkpoint",
                ));
            }
        } else {
            if op.expected != ExpectedState::Absent
                || !op.apply_after.is_empty()
                || record.string("wiki_run_id") != Some(run_id.as_str())
            {
                return Err(WikiError::invalid(
                    "event is mutable or belongs to another run",
                ));
            }
            if record.string("wiki_event_type") == Some("usage_receipt") {
                if draft.title != "Retain accounted response receipt" {
                    return Err(WikiError::invalid("receipt outside receipt plan"));
                }
                let body: ReceiptBody = decode_fence(bytes, "lwiki.run-event.v1")?;
                if body.version != 1
                    || body.receipt.version != 1
                    || body.receipt.attempt.run_id != *run_id
                    || body.receipt.receipt_id != target.id
                    || receipt_id(&body.receipt.attempt)? != target.id
                    || draft.allocated_ids.get("receipt") != Some(&target.id)
                {
                    return Err(WikiError::invalid(
                        "receipt attempt/identity binding differs",
                    ));
                }
            } else {
                if draft.title != "Create durable planned run" {
                    return Err(WikiError::invalid("genesis outside bootstrap plan"));
                }
                let body: EventBody = decode_fence(bytes, "lwiki.run-event.v1")?;
                if body.version != 1
                    || body.event.run_id != *run_id
                    || body.event.event_id != target.id
                    || !matches!(body.event.payload, EventPayload::Genesis { .. })
                    || event_bytes(&body.event)? != bytes
                {
                    return Err(WikiError::invalid(
                        "only exact retained genesis mirrors are admitted",
                    ));
                }
            }
        }
    }
    if let Some(c) = checkpoint {
        if let Some(op) = asset {
            let bytes = op
                .proposed
                .as_deref()
                .ok_or_else(|| WikiError::invalid("compact proof missing"))?;
            if op.expected != ExpectedState::Absent
                || !op.apply_after.is_empty()
                || Blake3Hash::digest(bytes) != c.hash
            {
                return Err(WikiError::invalid(
                    "compact asset is not an immutable exact create",
                ));
            }
            validate_compact_payload(vault, run_id, c, bytes, draft)?;
        } else if !draft
            .read_preconditions
            .iter()
            .any(|dep| dep.path == c.path && dep.expected == ExpectedState::Hash(c.hash.clone()))
        {
            return Err(WikiError::invalid(
                "existing compact asset has no exact read guard",
            ));
        } else {
            let bytes = crate::changes::prepare::read_bounded(fs, &c.path, EVENT_MAX_BYTES)?
                .ok_or_else(|| events::corrupt("guarded compact asset missing"))?;
            if Blake3Hash::digest(&bytes) != c.hash {
                return Err(events::corrupt("guarded compact asset hash differs"));
            }
            validate_compact_payload(vault, run_id, c, &bytes, draft)?;
        }
    }
    Ok(())
}
fn validate_compact_payload(
    vault: &RecordId,
    run_id: &RecordId,
    target: &crate::changes::indexed_refresh::IndexedCheckpointTarget,
    bytes: &[u8],
    draft: &ChangeDraft,
) -> Result<()> {
    let envelope: CompactCheckpointEnvelope = crate::changes::prepare::strict_json(bytes)?;
    let proof = &envelope.proof;
    let run = draft
        .operations
        .iter()
        .find(|op| op.target == run_path(run_id).expect("checked ID"))
        .and_then(|op| op.proposed.as_deref())
        .ok_or_else(|| WikiError::invalid("compact Run missing"))?;
    let plan = decode_run(run)?;
    if proof.version != 2
        || !matches!(proof.formatter_version, 1 | 2)
        || proof.vault_id != *vault
        || proof.run_id != *run_id
        || proof.spec_hash != plan.spec_hash
        || plan.checkpoint.as_ref() != Some(&proof.through)
        || proof.run_note.record.vault_id != *vault
        || proof.run_note.record.record_id != *run_id
        || proof.run_note.record.expected_kind != RecordKind::Run
        || proof.run_note.path != run_path(run_id)?
        || proof.run_note.hash != target.run_hash
        || compact_path(&proof.run_note)? != target.path
        || envelope.checksum != Blake3Hash::digest(compact_json(proof, EVENT_MAX_BYTES)?)
        || compact_bytes(proof.clone())? != bytes
    {
        return Err(WikiError::invalid(
            "compact checkpoint checksum/Run binding differs",
        ));
    }
    Ok(())
}

fn same_job_guards(
    a: &[crate::changes::ReadDependency],
    b: &[crate::changes::ReadDependency],
) -> bool {
    let normalize = |deps: &[crate::changes::ReadDependency]| {
        let mut result = BTreeMap::new();
        for dep in deps {
            if result
                .insert(dep.path.clone(), dep.expected.clone())
                .is_some_and(|old| old != dep.expected)
            {
                return None;
            }
        }
        Some(result)
    };
    match (normalize(a), normalize(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}
pub(crate) fn validate_job_draft(
    job: &JobLedger,
    mut draft: ChangeDraft,
) -> Result<ValidatedJobDraft> {
    use crate::changes::indexed_refresh::{
        IndexedCheckpointTarget, IndexedJobTarget, IndexedWriteOperation,
    };
    let bytes = draft.operations.iter().try_fold(0usize, |sum, op| {
        sum.checked_add(op.proposed.as_ref().map_or(0, Vec::len))
    });
    if draft.operations.is_empty()
        || draft.operations.len() > 3
        || draft.read_preconditions.len() > 128
        || bytes.is_none_or(|bytes| bytes > 16 * 1024 * 1024)
    {
        return Err(WikiError::invalid(
            "job publication requires 1–3 exact operations",
        ));
    }
    // with(false) authenticates the head, journal and response metadata. This
    // short read lock is released before projection and transport; it never
    // acquires writer authority while holding the run lock.
    job.with(false, |_, loaded| {
        let inspection = &loaded.state.inspection;
        let mut records = Vec::new();
        let mut checkpoint = None;
        for op in &draft.operations {
            let bytes = op
                .proposed
                .as_deref()
                .ok_or_else(|| WikiError::invalid("job deletion forbidden"))?;
            if op.target == run_path(&job.run_id)? {
                if op.expected == ExpectedState::Absent {
                    if draft.title != "Create durable planned run"
                        || bytes != run_bytes(&ledger::initial_inspection(&inspection.spec)?)?
                        || inspection.attempts.len() != 0
                    {
                        return Err(WikiError::invalid(
                            "planned Run differs from retained genesis",
                        ));
                    }
                    if !same_job_guards(
                        &draft.read_preconditions,
                        &inspection.spec.scope.read_preconditions,
                    ) {
                        return Err(WikiError::invalid(
                            "planned Run original scope guards differ",
                        ));
                    }
                } else {
                    if draft.title != "Checkpoint durable run prefix"
                        || inspection
                            .run_note
                            .as_ref()
                            .is_none_or(|run| op.expected != ExpectedState::Hash(run.hash.clone()))
                    {
                        return Err(WikiError::invalid(
                            "checkpoint Run is not guarded by acknowledged prior Run",
                        ));
                    }
                    let run = DurableOutputRef {
                        record: RecordRef {
                            vault_id: job.vault_id.clone(),
                            record_id: job.run_id.clone(),
                            expected_kind: RecordKind::Run,
                        },
                        path: op.target.clone(),
                        hash: Blake3Hash::digest(bytes),
                    };
                    let proof = proof_for_bytes(&run, bytes, &loaded.frames)?;
                    let latest = loaded
                        .frames
                        .iter()
                        .rfind(|frame| {
                            !matches!(frame.event.payload, EventPayload::Checkpoint { .. })
                        })
                        .ok_or_else(|| events::corrupt("checkpoint prefix absent"))?;
                    if proof.formatter_version != 2 || proof.through != events::event_ref(latest) {
                        return Err(WikiError::invalid(
                            "checkpoint is not the exact latest compact prefix",
                        ));
                    }
                    let proof_bytes = compact_bytes(proof)?;
                    checkpoint = Some(IndexedCheckpointTarget {
                        path: compact_path(&run)?,
                        hash: Blake3Hash::digest(&proof_bytes),
                        run_hash: run.hash,
                    });
                }
                records.push(IndexedJobTarget {
                    path: op.target.clone(),
                    id: job.run_id.clone(),
                    kind: RecordKind::Run,
                });
            } else if op.target.as_str().ends_with(".md") {
                let parsed = parse_note(bytes);
                let record = parsed
                    .canonical
                    .as_ref()
                    .filter(|r| r.kind() == RecordKind::RunEvent)
                    .ok_or_else(|| {
                        WikiError::invalid("job batch contains a non-event canonical target")
                    })?;
                if record.string("wiki_event_type") == Some("usage_receipt") {
                    let body: ReceiptBody = decode_fence(bytes, "lwiki.run-event.v1")?;
                    let actual = &body.receipt;
                    let expected = receipt_plan_locked(
                        job,
                        loaded,
                        &actual.attempt,
                        actual.output_disposition,
                        actual.outputs.clone(),
                        actual.cache_outputs.clone(),
                        vec![],
                    )?;
                    let received = loaded
                        .frames
                        .iter()
                        .find(|f| {
                            matches!(&f.event.payload,
                        EventPayload::Received { spool } if spool.attempt == actual.attempt)
                        })
                        .ok_or_else(|| events::corrupt("receipt has no retained Received event"))?;
                    if draft.title != "Retain accounted response receipt"
                        || actual != &expected.receipt
                        || usage_receipt_bytes(actual, &received.event)? != bytes
                        || !same_job_guards(
                            &draft.read_preconditions,
                            &expected.draft.read_preconditions,
                        )
                    {
                        return Err(WikiError::invalid(
                            "receipt bytes/accounting/guards differ from retained response",
                        ));
                    }
                } else if bytes != event_bytes(&loaded.frames[0].event)?
                    || draft.title != "Create durable planned run"
                {
                    return Err(WikiError::invalid(
                        "event differs from retained bootstrap genesis",
                    ));
                }
                records.push(IndexedJobTarget {
                    path: op.target.clone(),
                    id: record.id().clone(),
                    kind: RecordKind::RunEvent,
                });
            }
        }
        if let Some(c) = &checkpoint {
            if draft.read_preconditions.iter().any(|dep| {
                dep.path != c.path || dep.expected != ExpectedState::Hash(c.hash.clone())
            }) {
                return Err(WikiError::invalid(
                    "checkpoint includes unknown original read guards",
                ));
            }
            let proposed = draft.operations.iter().find(|op| op.target == c.path);
            let bytes = if let Some(op) = proposed {
                op.proposed
                    .clone()
                    .ok_or_else(|| events::corrupt("compact bytes missing"))?
            } else {
                ledger::read(&job.fs, &c.path)?
                    .ok_or_else(|| events::corrupt("compact asset missing"))?
            };
            let run = draft
                .operations
                .iter()
                .find(|op| op.target == run_path(&job.run_id).expect("ID checked"))
                .unwrap();
            let run_ref = DurableOutputRef {
                record: RecordRef {
                    vault_id: job.vault_id.clone(),
                    record_id: job.run_id.clone(),
                    expected_kind: RecordKind::Run,
                },
                path: run.target.clone(),
                hash: c.run_hash.clone(),
            };
            if bytes
                != compact_bytes(proof_for_bytes(
                    &run_ref,
                    run.proposed.as_deref().unwrap(),
                    &loaded.frames,
                )?)?
            {
                return Err(WikiError::invalid(
                    "compact asset differs from exact retained operational prefix",
                ));
            }
            if proposed.is_none() {
                draft
                    .read_preconditions
                    .push(crate::changes::ReadDependency {
                        path: c.path.clone(),
                        expected: ExpectedState::Hash(c.hash.clone()),
                    });
            }
        }
        records.sort_by(|a, b| a.path.cmp(&b.path));
        let operation = IndexedWriteOperation::JobBatch {
            run_id: job.run_id.clone(),
            records,
            checkpoint,
        };
        validate_job_payloads(&job.fs, &job.vault_id, &operation, &draft)?;
        Ok(ValidatedJobDraft { draft, operation })
    })
}

const MAX_JOB_INTENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_JOB_CANDIDATES: u32 = 64;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum JobPublicationKey {
    Bootstrap,
    Receipt { receipt_id: RecordId },
    Checkpoint { run_hash: Blake3Hash },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobWriteCommitment {
    target: VaultRelativePath,
    before: ExpectedState,
    after: Blake3Hash,
    byte_len: u64,
    apply_after: Vec<VaultRelativePath>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobRequestCommitment {
    version: u32,
    title: String,
    created_at: String,
    allocated_ids: BTreeMap<String, RecordId>,
    read_preconditions: Vec<crate::changes::ReadDependency>,
    writes: Vec<JobWriteCommitment>,
    receipt_disposition: Option<OutputDisposition>,
    receipt_only: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobCandidateRef {
    generation: u32,
    hash: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobClosure {
    candidate: JobCandidateRef,
    change: PreparedChange,
    // A premanifest candidate has no change journal/outcome to abort. Its
    // preserved commitment proves no later authority; it grants no spend/send.
    aborted_manifest: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobCandidate {
    version: u32,
    vault_id: RecordId,
    run_id: RecordId,
    key: JobPublicationKey,
    generation: u32,
    request: JobRequestCommitment,
    named: crate::catalog::source_refresh::NamedIndexedIntent,
    previous: Option<JobClosure>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobPublicationSlot {
    version: u32,
    vault_id: RecordId,
    run_id: RecordId,
    key: JobPublicationKey,
    current: JobCandidateRef,
    previous: Option<JobClosure>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobSlotEnvelope {
    slot: JobPublicationSlot,
    checksum: Blake3Hash,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobCandidateEnvelope {
    candidate: JobCandidate,
    checksum: Blake3Hash,
}
fn job_slot_key(vault: &RecordId, run: &RecordId, key: &JobPublicationKey) -> Result<Blake3Hash> {
    Ok(Blake3Hash::digest(compact_json(
        &("lwiki.job-publication-slot.v1", vault, run, key),
        4096,
    )?))
}
fn job_slot_path(
    vault: &RecordId,
    run: &RecordId,
    key: &JobPublicationKey,
) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!(
        ".wiki/state/job-publications/{run}/{}.json",
        job_slot_key(vault, run, key)?.hex()
    ))
}
fn job_candidate_path(
    vault: &RecordId,
    run: &RecordId,
    key: &JobPublicationKey,
    generation: u32,
) -> Result<VaultRelativePath> {
    if generation == 0 || generation > MAX_JOB_CANDIDATES {
        return Err(events::corrupt("job candidate generation exceeds bound"));
    }
    VaultRelativePath::new(format!(
        ".wiki/state/job-publications/{run}/{}/{generation}.json",
        job_slot_key(vault, run, key)?.hex()
    ))
}
fn job_change_id(
    vault: &RecordId,
    run: &RecordId,
    key: &JobPublicationKey,
    generation: u32,
) -> Result<RecordId> {
    RecordId::new(format!(
        "change_job_{}",
        Blake3Hash::digest(compact_json(
            &(
                "lwiki.job-publication-candidate.v1",
                job_slot_key(vault, run, key)?,
                generation
            ),
            4096
        )?)
        .hex()
    ))
}
fn job_key(draft: &ChangeDraft) -> Result<JobPublicationKey> {
    let run = draft
        .allocated_ids
        .get("run")
        .ok_or_else(|| WikiError::invalid("job run allocation absent"))?;
    match draft.title.as_str() {
        "Create durable planned run" => Ok(JobPublicationKey::Bootstrap),
        "Retain accounted response receipt" => Ok(JobPublicationKey::Receipt {
            receipt_id: draft
                .allocated_ids
                .get("receipt")
                .ok_or_else(|| WikiError::invalid("receipt allocation absent"))?
                .clone(),
        }),
        "Checkpoint durable run prefix" => Ok(JobPublicationKey::Checkpoint {
            run_hash: Blake3Hash::digest(
                draft
                    .operations
                    .iter()
                    .find(|op| op.target == run_path(run).expect("validated ID"))
                    .and_then(|op| op.proposed.as_deref())
                    .ok_or_else(|| WikiError::invalid("checkpoint Run after-image absent"))?,
            ),
        }),
        _ => Err(WikiError::invalid("unknown named job plan")),
    }
}
fn job_request(draft: &ChangeDraft, created_at: String) -> Result<JobRequestCommitment> {
    let mut writes = Vec::new();
    let mut receipt_disposition = None;
    let mut receipt_only = false;
    for op in &draft.operations {
        let bytes = op
            .proposed
            .as_deref()
            .ok_or_else(|| WikiError::invalid("named job deletion forbidden"))?;
        let mut ordering = op.apply_after.clone();
        ordering.sort();
        writes.push(JobWriteCommitment {
            target: op.target.clone(),
            before: op.expected.clone(),
            after: Blake3Hash::digest(bytes),
            byte_len: bytes.len() as u64,
            apply_after: ordering,
        });
        if parse_note(bytes).canonical.is_some_and(|r| {
            r.kind() == RecordKind::RunEvent && r.string("wiki_event_type") == Some("usage_receipt")
        }) {
            let body: ReceiptBody = decode_fence(bytes, "lwiki.run-event.v1")?;
            receipt_disposition = Some(body.receipt.output_disposition);
            receipt_only = body.receipt.outputs.is_empty() && body.receipt.cache_outputs.is_empty();
        }
    }
    writes.sort_by(|a, b| a.target.cmp(&b.target));
    let mut guards = BTreeMap::new();
    for dep in &draft.read_preconditions {
        if guards
            .insert(dep.path.clone(), dep.expected.clone())
            .is_some_and(|old| old != dep.expected)
        {
            return Err(WikiError::invalid("conflicting original job guards"));
        }
    }
    Ok(JobRequestCommitment {
        version: 1,
        title: draft.title.clone(),
        created_at,
        allocated_ids: draft.allocated_ids.clone(),
        read_preconditions: guards
            .into_iter()
            .map(|(path, expected)| crate::changes::ReadDependency { path, expected })
            .collect(),
        writes,
        receipt_disposition,
        receipt_only,
    })
}
fn job_encoded(value: &impl Serialize) -> Result<Vec<u8>> {
    crate::catalog::normalized_delta::counted(value, MAX_JOB_INTENT_BYTES)?;
    compact_json(value, MAX_JOB_INTENT_BYTES)
}
fn validate_job_candidate(candidate: &JobCandidate) -> Result<()> {
    use crate::changes::indexed_refresh::IndexedWriteOperation;
    let named = &candidate.named;
    if candidate.version != 1
        || candidate.request.version != 1
        || candidate.request.writes.is_empty()
        || candidate.request.writes.len() > 3
        || candidate.request.read_preconditions.len() > 128
        || !candidate
            .request
            .writes
            .windows(2)
            .all(|p| p[0].target < p[1].target)
        || !candidate
            .request
            .read_preconditions
            .windows(2)
            .all(|p| p[0].path < p[1].path)
        || candidate.generation == 0
        || candidate.generation > MAX_JOB_CANDIDATES
        || named.manifest.origin.is_some()
        || named.manifest.inverse_of.is_some()
        || named.manifest.vault_id != candidate.vault_id
        || named.proof.vault_id != candidate.vault_id
        || named.manifest.change_id
            != job_change_id(
                &candidate.vault_id,
                &candidate.run_id,
                &candidate.key,
                candidate.generation,
            )?
        || candidate.request.allocated_ids.get("run") != Some(&candidate.run_id)
        || named.manifest.created_at != candidate.request.created_at
        || named.manifest.title != candidate.request.title
        || named.manifest.allocated_ids != candidate.request.allocated_ids
        || named.proof.version != 3
        || named.proof.source_id.is_some()
        || !matches!(&named.proof.operation, Some(IndexedWriteOperation::JobBatch { run_id, .. }) if run_id == &candidate.run_id)
        || named.proof.change.manifest_hash != Blake3Hash::digest(job_encoded(&named.manifest)?)
        || named.manifest.operations.len() != candidate.request.writes.len()
    {
        return Err(events::corrupt(
            "named job candidate identity/commitment differs",
        ));
    }
    let keyed = match &candidate.key {
        JobPublicationKey::Bootstrap => {
            candidate.request.title == "Create durable planned run"
                && candidate.request.allocated_ids.len() == 1
        }
        JobPublicationKey::Receipt { receipt_id } => {
            candidate.request.title == "Retain accounted response receipt"
                && candidate.request.allocated_ids.get("receipt") == Some(receipt_id)
                && candidate.request.allocated_ids.len() == 2
                && candidate.request.receipt_disposition.is_some()
        }
        JobPublicationKey::Checkpoint { run_hash } => {
            candidate.request.title == "Checkpoint durable run prefix"
                && candidate.request.allocated_ids.len() == 1
                && candidate.request.writes.iter().any(|w| {
                    w.target == run_path(&candidate.run_id).expect("ID checked")
                        && &w.after == run_hash
                })
        }
    };
    if !keyed {
        return Err(events::corrupt(
            "named job key does not bind its known request",
        ));
    }
    named.proof.validate_manifest(&named.manifest)?;
    for write in &candidate.request.writes {
        let op = named
            .manifest
            .operations
            .iter()
            .find(|op| op.target == write.target)
            .ok_or_else(|| events::corrupt("named job target missing"))?;
        let mut ordering: Vec<_> = op
            .apply_after
            .iter()
            .map(|n| named.manifest.operations[*n].target.clone())
            .collect();
        ordering.sort();
        if op.before != write.before
            || op.after != ExpectedState::Hash(write.after.clone())
            || op
                .after_payload
                .as_ref()
                .is_none_or(|p| p.hash != write.after || p.byte_len != write.byte_len)
            || ordering != write.apply_after
        {
            return Err(events::corrupt(
                "named job immutable payload/order commitment differs",
            ));
        }
    }
    for guard in &candidate.request.read_preconditions {
        let matches = named
            .manifest
            .operations
            .iter()
            .find(|op| op.target == guard.path)
            .map_or_else(
                || named.manifest.read_preconditions.contains(guard),
                |op| op.before == guard.expected,
            );
        if !matches {
            return Err(events::corrupt("named job original guard differs"));
        }
    }
    if candidate
        .previous
        .as_ref()
        .is_some_and(|p| p.candidate.generation.checked_add(1) != Some(candidate.generation))
        || (candidate.generation == 1) != candidate.previous.is_none()
    {
        return Err(events::corrupt("job candidate predecessor differs"));
    }
    Ok(())
}
fn read_job_candidate(
    fs: &VaultFs,
    vault: &RecordId,
    run: &RecordId,
    key: &JobPublicationKey,
    reference: &JobCandidateRef,
) -> Result<JobCandidate> {
    let path = job_candidate_path(vault, run, key, reference.generation)?;
    let engine = ChangeEngine::new(fs.clone())?;
    engine.require_named_single_link(&path)?;
    let read = crate::changes::prepare::read_bounded(fs, &path, MAX_JOB_INTENT_BYTES)?;
    let bytes = read
        .ok_or_else(|| events::corrupt("known named job candidate missing; no history fallback"))?;
    if Blake3Hash::digest(&bytes) != reference.hash {
        return Err(events::corrupt("named job candidate locator hash differs"));
    }
    let envelope: JobCandidateEnvelope = crate::changes::prepare::strict_json(&bytes)?;
    if envelope.checksum != Blake3Hash::digest(job_encoded(&envelope.candidate)?)
        || envelope.candidate.vault_id != *vault
        || envelope.candidate.run_id != *run
        || envelope.candidate.key != *key
        || envelope.candidate.generation != reference.generation
    {
        return Err(events::corrupt(
            "named job candidate vault/run/name binding differs",
        ));
    }
    validate_job_candidate(&envelope.candidate)?;
    Ok(envelope.candidate)
}
fn read_job_slot(
    fs: &VaultFs,
    vault: &RecordId,
    run: &RecordId,
    key: &JobPublicationKey,
) -> Result<Option<(JobPublicationSlot, Blake3Hash, JobCandidate)>> {
    let path = job_slot_path(vault, run, key)?;
    let engine = ChangeEngine::new(fs.clone())?;
    engine.require_named_single_link(&path)?;
    let read = crate::changes::prepare::read_bounded(fs, &path, 16384)?;
    let Some(bytes) = read else {
        return Ok(None);
    };
    let envelope: JobSlotEnvelope = crate::changes::prepare::strict_json(&bytes)?;
    let slot = envelope.slot;
    if slot.version != 1
        || slot.vault_id != *vault
        || slot.run_id != *run
        || slot.key != *key
        || envelope.checksum != Blake3Hash::digest(job_encoded(&slot)?)
    {
        return Err(events::corrupt(
            "named job slot vault/run/key/checksum differs",
        ));
    }
    let candidate = read_job_candidate(fs, vault, run, key, &slot.current)?;
    if candidate.previous != slot.previous {
        return Err(events::corrupt("named job slot closure differs"));
    }
    if let Some(closed) = &candidate.previous {
        let predecessor = read_job_candidate(fs, vault, run, key, &closed.candidate)?;
        if predecessor.named.proof.change != closed.change {
            return Err(events::corrupt("named job predecessor change differs"));
        }
        let (state, terminal, applying) = job_candidate_state(fs, &predecessor)?;
        if applying
            || (closed.aborted_manifest
                && (terminal
                    .as_ref()
                    .is_none_or(|r| r.status != ChangeStatus::Aborted)
                    || state.status.is_some_and(|s| s != ChangeStatus::Aborted)))
            || (!closed.aborted_manifest
                && (state.status.is_some()
                    || terminal.is_some()
                    || fs
                        .root()
                        .resolve(&crate::changes::prepare::manifest_path(
                            &closed.change.change_id,
                        )?)?
                        .exists()))
        {
            return Err(events::corrupt(
                "named job predecessor closure lost exact never-applied authority",
            ));
        }
    }
    Ok(Some((slot, Blake3Hash::digest(&bytes), candidate)))
}
fn install_job_file(
    fs: &VaultFs,
    writer: &WriterPermit,
    path: &VaultRelativePath,
    expected: ExpectedState,
    bytes: &[u8],
) -> Result<()> {
    let engine = ChangeEngine::new(fs.clone())?;
    engine.require_named_single_link(path)?;
    let stage = fs.stage(path, bytes, writer)?;
    crate::changes::journal::require_sync(fs.replace(stage, &expected, writer)?)
}
fn retain_job_candidate(
    fs: &VaultFs,
    writer: &WriterPermit,
    candidate: &JobCandidate,
) -> Result<JobCandidateRef> {
    validate_job_candidate(candidate)?;
    let path = job_candidate_path(
        &candidate.vault_id,
        &candidate.run_id,
        &candidate.key,
        candidate.generation,
    )?;
    let envelope = JobCandidateEnvelope {
        candidate: candidate.clone(),
        checksum: Blake3Hash::digest(job_encoded(candidate)?),
    };
    let bytes = job_encoded(&envelope)?;
    ChangeEngine::new(fs.clone())?.require_named_single_link(&path)?;
    match crate::changes::prepare::read_bounded(fs, &path, MAX_JOB_INTENT_BYTES)? {
        Some(old) if old == bytes => {
            crate::changes::journal::require_sync(fs.sync_target(&path, writer)?)?
        }
        Some(_) => {
            return Err(events::corrupt(
                "immutable named job candidate already differs",
            ));
        }
        None => install_job_file(fs, writer, &path, ExpectedState::Absent, &bytes)?,
    }
    Ok(JobCandidateRef {
        generation: candidate.generation,
        hash: Blake3Hash::digest(&bytes),
    })
}
fn install_job_slot(
    fs: &VaultFs,
    writer: &WriterPermit,
    candidate: &JobCandidate,
    reference: JobCandidateRef,
    before: ExpectedState,
) -> Result<()> {
    let slot = JobPublicationSlot {
        version: 1,
        vault_id: candidate.vault_id.clone(),
        run_id: candidate.run_id.clone(),
        key: candidate.key.clone(),
        current: reference,
        previous: candidate.previous.clone(),
    };
    let bytes = job_encoded(&JobSlotEnvelope {
        checksum: Blake3Hash::digest(job_encoded(&slot)?),
        slot,
    })?;
    install_job_file(
        fs,
        writer,
        &job_slot_path(&candidate.vault_id, &candidate.run_id, &candidate.key)?,
        before,
        &bytes,
    )
}
/// Only the operation explicitly named by outside authority may require recovery.
/// Missing proof is a maintenance boundary, never a reason to enumerate history.
pub(crate) fn recover_job_active(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
) -> Result<()> {
    let catalog = Catalog::new(fs.clone(), vault.clone());
    let Some(authority) = catalog.operation_state()? else {
        return Ok(());
    };
    if let Some(active) = authority.active() {
        let engine = ChangeEngine::new(fs.clone())?;
        let proof = engine
            .load_indexed_refresh_proof(&active.change)?
            .ok_or_else(|| {
                events::corrupt(
                    "exact active operation lacks indexed proof; explicit maintenance required",
                )
            })?;
        let mut session =
            crate::catalog::source_refresh::IndexedRefreshSession::resume(&catalog, writer, proof)?;
        engine.apply_indexed_refresh(writer, &mut session)?;
    }
    Ok(())
}
fn check_job_control_links(fs: &VaultFs, candidate: &JobCandidate) -> Result<()> {
    let engine = ChangeEngine::new(fs.clone())?;
    let named = &candidate.named;
    for path in [
        crate::changes::prepare::manifest_path(&named.manifest.change_id)?,
        crate::changes::journal::journal_path(&named.manifest.change_id)?,
        crate::changes::indexed_refresh::baseline_path(&named.proof.change)?,
        crate::catalog::source_refresh::delta_path(&named.proof.change)?,
        VaultRelativePath::new(format!("changes/{}/outcome.json", named.manifest.change_id))?,
        VaultRelativePath::new(format!(
            "changes/{}/revision-trees.json",
            named.manifest.change_id
        ))?,
    ] {
        engine.require_named_single_link(&path)?;
    }
    for op in &named.manifest.operations {
        for payload in [&op.before_payload, &op.after_payload]
            .into_iter()
            .flatten()
        {
            engine.require_named_single_link(&payload.path)?;
        }
    }
    Ok(())
}
fn job_candidate_state(
    fs: &VaultFs,
    candidate: &JobCandidate,
) -> Result<(
    crate::changes::JournalState,
    Option<crate::changes::ApplyReport>,
    bool,
)> {
    check_job_control_links(fs, candidate)?;
    let named = &candidate.named;
    let state = crate::changes::journal::load_journal(
        fs,
        &named.manifest,
        &named.proof.change.manifest_hash,
    )?;
    if state.torn_tail {
        return Err(events::corrupt(
            "named job candidate has ambiguous journal tail",
        ));
    }
    let terminal = crate::changes::outcome::terminal_report(
        fs,
        &named.manifest,
        &named.proof.change.manifest_hash,
    )?;
    let applying = state
        .frames
        .iter()
        .any(|f| matches!(f.event, crate::changes::ChangeEvent::Applying))
        || crate::changes::outcome::terminal_ever_applying(
            fs,
            &named.manifest,
            &named.proof.change.manifest_hash,
        )? == Some(true)
        || matches!(
            state.status,
            Some(
                ChangeStatus::Applying
                    | ChangeStatus::FilesApplied
                    | ChangeStatus::Indexed
                    | ChangeStatus::Committed
            )
        );
    Ok((state, terminal, applying))
}
/// Authenticate only this declared prefix (<=128 entries), including partial
/// payload retention. Unrecognized files/stages or later authority without a
/// manifest are preserved and refuse closure.
fn prove_job_candidate_closable(
    fs: &VaultFs,
    writer: &WriterPermit,
    candidate: &JobCandidate,
    reference: &JobCandidateRef,
) -> Result<JobClosure> {
    use std::collections::BTreeSet;
    let catalog = Catalog::new(fs.clone(), candidate.vault_id.clone());
    if catalog
        .operation_state()?
        .is_none_or(|a| a.active().is_some())
    {
        return Err(events::corrupt(
            "active authority prevents named job candidate replacement",
        ));
    }
    let engine = ChangeEngine::new(fs.clone())?;
    let named = &candidate.named;
    let (state, terminal, applying) = job_candidate_state(fs, candidate)?;
    if applying
        || terminal
            .as_ref()
            .is_some_and(|t| t.status != ChangeStatus::Aborted)
        || !matches!(
            state.status,
            None | Some(ChangeStatus::Prepared | ChangeStatus::Aborted)
        )
    {
        return Err(events::corrupt(
            "possibly applying/committed job candidate cannot be replaced",
        ));
    }
    let note = crate::changes::prepare::manifest_path(&named.manifest.change_id)?;
    engine.require_named_single_link(&note)?;
    let manifest_present = crate::changes::prepare::read_bounded(
        fs,
        &note,
        crate::changes::prepare::MAX_MANIFEST_BYTES + 65536,
    )?
    .is_some();
    if manifest_present {
        let (actual, hash) = engine.load_manifest_structure(&named.manifest.change_id)?;
        if actual != named.manifest || hash != named.proof.change.manifest_hash {
            return Err(events::corrupt("closing named job manifest differs"));
        }
    } else if state.status.is_some() {
        return Err(events::corrupt("named job journal lost its manifest"));
    }
    let mut allowed = BTreeSet::from([note.clone()]);
    let control = [
        crate::changes::journal::journal_path(&named.manifest.change_id)?,
        crate::changes::indexed_refresh::baseline_path(&named.proof.change)?,
        crate::catalog::source_refresh::delta_path(&named.proof.change)?,
        VaultRelativePath::new(format!("changes/{}/outcome.json", named.manifest.change_id))?,
        VaultRelativePath::new(format!(
            "changes/{}/revision-trees.json",
            named.manifest.change_id
        ))?,
    ];
    for path in &control {
        engine.require_named_single_link(path)?;
        if !manifest_present && fs.root().resolve(path)?.exists() {
            return Err(events::corrupt(
                "later job authority exists without exact named manifest",
            ));
        }
        allowed.insert(path.clone());
    }
    if let Some(proof) = engine.load_indexed_refresh_proof(&named.proof.change)? {
        if proof != named.proof {
            return Err(events::corrupt("closing job indexed proof differs"));
        }
    }
    if let Some(bytes) = crate::changes::prepare::read_bounded(
        fs,
        &crate::catalog::source_refresh::delta_path(&named.proof.change)?,
        256 * 1024 * 1024,
    )? {
        if Blake3Hash::digest(&bytes) != named.proof.delta_hash {
            return Err(events::corrupt("closing job delta differs"));
        }
    }
    engine.require_revision_baseline(&named.manifest)?;
    engine.validate_named_revision_receipt(&named.manifest, &named.proof.change.manifest_hash)?;
    engine.require_abandoned_revision_trees(&named.manifest)?;
    let mut remaining = 64 * 1024 * 1024;
    for (index, op) in named.manifest.operations.iter().enumerate() {
        for (side, expected, payload) in [
            ("before", &op.before, &op.before_payload),
            ("proposed", &op.after, &op.after_payload),
        ] {
            if let Some(payload) = payload {
                engine.require_named_single_link(&payload.path)?;
                allowed.insert(payload.path.clone());
                if fs.root().resolve(&payload.path)?.exists() {
                    let bytes = engine
                        .verify_payload_with_limit(
                            &named.manifest.change_id,
                            index,
                            side,
                            &op.target,
                            (
                                expected,
                                if side == "before" {
                                    &op.before_payload
                                } else {
                                    &op.after_payload
                                },
                            ),
                            remaining,
                        )?
                        .unwrap();
                    remaining = remaining.checked_sub(bytes.len()).ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::BudgetExceeded,
                            "named job closure bytes exceed bound",
                        )
                    })?;
                }
            }
        }
        let bytes = crate::changes::prepare::read_bounded(fs, &op.target, remaining)?;
        if let Some(bytes) = &bytes {
            remaining = remaining.checked_sub(bytes.len()).ok_or_else(|| {
                WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "named job closure target bytes exceed bound",
                )
            })?;
        }
        let observed = bytes.as_ref().map_or(ExpectedState::Absent, |b| {
            ExpectedState::Hash(Blake3Hash::digest(b))
        });
        if observed != op.before {
            return Err(events::corrupt(
                "job target is not at exact before-state; replacement forbidden",
            ));
        }
    }
    let prefix = VaultRelativePath::new(format!("changes/{}", named.manifest.change_id))?;
    let mut stack = vec![prefix.clone()];
    let mut entries = 0usize;
    if fs.root().resolve(&prefix)?.exists() {
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(fs.root().resolve(&directory)?)
                .map_err(|_| events::corrupt("inspect exact named job prefix"))?
            {
                entries += 1;
                if entries > 128 {
                    return Err(WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "named job prefix exceeds entry bound",
                    ));
                }
                let entry = entry.map_err(|_| events::corrupt("inspect named job prefix entry"))?;
                let relative = VaultRelativePath::new(format!(
                    "{directory}/{}",
                    entry
                        .file_name()
                        .to_str()
                        .ok_or_else(|| events::corrupt("non-UTF8 named job prefix"))?
                ))?;
                let kind = entry
                    .file_type()
                    .map_err(|_| events::corrupt("inspect named job prefix type"))?;
                if kind.is_dir()
                    && allowed
                        .iter()
                        .any(|p| p.as_str().starts_with(&format!("{relative}/")))
                {
                    stack.push(relative);
                } else if !kind.is_file() || !allowed.contains(&relative) {
                    return Err(events::corrupt(
                        "unfamiliar named job prefix prevents replacement",
                    ));
                }
            }
        }
    }
    if manifest_present {
        engine.abort(writer, &named.proof.change)?;
    }
    Ok(JobClosure {
        candidate: reference.clone(),
        change: named.proof.change.clone(),
        aborted_manifest: manifest_present,
    })
}

/// Admission uses the ordinary selected projection and named engine. The slot
/// commits this result before preparation and grants no accounting authority.
fn seal_job_candidate(
    catalog: &Catalog,
    writer: &WriterPermit,
    job: &JobLedger,
    validated: ValidatedJobDraft,
    key: JobPublicationKey,
    request: JobRequestCommitment,
    generation: u32,
    previous: Option<JobClosure>,
) -> Result<(
    JobCandidate,
    crate::catalog::source_refresh::SealedIndexedPreparation,
)> {
    use crate::catalog::{
        query_types::QueryReadLimits, source_projection::RefreshProjectionLimits,
        source_refresh::IndexedRefreshSession, write_projection::project_jobs,
    };
    let reader = catalog.query_snapshot(QueryReadLimits::default())?;
    let projected = project_jobs(
        &job.fs,
        &reader,
        validated,
        &RefreshProjectionLimits::default(),
    )?
    .ok_or_else(|| events::corrupt("job bytes exist without exact named committed authority"))?;
    drop(reader);
    let sealed = IndexedRefreshSession::seal_named_write(
        catalog,
        writer,
        projected,
        crate::changes::types::NamedChangeIdentity {
            change_id: job_change_id(&job.vault_id, &job.run_id, &key, generation)?,
            created_at: request.created_at.clone(),
        },
    )?;
    let candidate = JobCandidate {
        version: 1,
        vault_id: job.vault_id.clone(),
        run_id: job.run_id.clone(),
        key,
        generation,
        request,
        named: sealed.intent().clone(),
        previous,
    };
    validate_job_candidate(&candidate)?;
    Ok((candidate, sealed))
}
fn ensure_job_directories(
    fs: &VaultFs,
    writer: &WriterPermit,
    vault: &RecordId,
    run: &RecordId,
    key: &JobPublicationKey,
) -> Result<()> {
    for directory in [
        ".wiki/state/job-publications".to_owned(),
        format!(".wiki/state/job-publications/{run}"),
        format!(
            ".wiki/state/job-publications/{run}/{}",
            job_slot_key(vault, run, key)?.hex()
        ),
    ] {
        crate::changes::journal::require_sync(
            fs.ensure_directory(&VaultRelativePath::new(directory)?, writer)?,
        )?;
    }
    Ok(())
}
fn orphan_job_candidate(
    fs: &VaultFs,
    vault: &RecordId,
    run: &RecordId,
    key: &JobPublicationKey,
    generation: u32,
) -> Result<Option<(JobCandidate, JobCandidateRef)>> {
    let path = job_candidate_path(vault, run, key, generation)?;
    ChangeEngine::new(fs.clone())?.require_named_single_link(&path)?;
    let read = crate::changes::prepare::read_bounded(fs, &path, MAX_JOB_INTENT_BYTES)?;
    let Some(bytes) = read else {
        return Ok(None);
    };
    let reference = JobCandidateRef {
        generation,
        hash: Blake3Hash::digest(bytes),
    };
    let candidate = read_job_candidate(fs, vault, run, key, &reference)?;
    Ok(Some((candidate, reference)))
}
/// Validate deterministic namespace again at the engine's narrow named seam.
/// A job name is not a caller-selected operational path.
pub(crate) fn require_named_job_identity(
    vault: &RecordId,
    operation: &crate::changes::indexed_refresh::IndexedWriteOperation,
    draft: &ChangeDraft,
    identity: &crate::changes::types::NamedChangeIdentity,
) -> Result<()> {
    let crate::changes::indexed_refresh::IndexedWriteOperation::JobBatch { run_id, .. } = operation
    else {
        return Err(WikiError::invalid("named job seam requires JobBatch"));
    };
    let key = job_key(draft)?;
    if !(1..=MAX_JOB_CANDIDATES).any(|generation| {
        job_change_id(vault, run_id, &key, generation).is_ok_and(|id| id == identity.change_id)
    }) {
        return Err(WikiError::invalid(
            "job named identity is outside exact run/key namespace",
        ));
    }
    Ok(())
}
fn pure_bootstrap(job: &JobLedger, spec: &RunSpec) -> Result<ValidatedJobDraft> {
    use crate::changes::indexed_refresh::{IndexedJobTarget, IndexedWriteOperation};
    if spec.run_id != job.run_id || spec.vault_id != job.vault_id {
        return Err(WikiError::invalid("bootstrap spec binding differs"));
    }
    let mut draft = draft("Create durable planned run", spec);
    draft.read_preconditions = spec.scope.read_preconditions.clone();
    draft.operations.push(write(
        run_path(&job.run_id)?,
        ExpectedState::Absent,
        run_bytes(&ledger::initial_inspection(spec)?)?,
    ));
    let operation = IndexedWriteOperation::JobBatch {
        run_id: job.run_id.clone(),
        records: vec![IndexedJobTarget {
            path: run_path(&job.run_id)?,
            id: job.run_id.clone(),
            kind: RecordKind::Run,
        }],
        checkpoint: None,
    };
    validate_job_payloads(&job.fs, &job.vault_id, &operation, &draft)?;
    Ok(ValidatedJobDraft { draft, operation })
}
/// Called after validate_spec/check_prior and before creating any run namespace.
/// This pure seal is a publication commitment only. create must subsequently
/// authenticate matching retained genesis/spec before dispatching its candidate.
pub(super) fn retain_bootstrap_intent(
    job: &JobLedger,
    writer: &WriterPermit,
    spec: &RunSpec,
    namespace_exists: bool,
) -> Result<bool> {
    let catalog = Catalog::new(job.fs.clone(), job.vault_id.clone());
    if catalog.operation_state()?.is_none() {
        return Ok(false);
    }
    let key = JobPublicationKey::Bootstrap;
    let validated = pure_bootstrap(job, spec)?;
    let request = job_request(&validated.draft, timestamp(spec.created_at_utc_ms)?)?;
    if let Some((_, _, candidate)) = read_job_slot(&job.fs, &job.vault_id, &job.run_id, &key)? {
        if candidate.request != request {
            return Err(events::corrupt("bootstrap commitment/spec differs"));
        }
        let (state, terminal, applying) = job_candidate_state(&job.fs, &candidate)?;
        let accounting = ledger::read(
            &job.fs,
            &VaultRelativePath::new(format!(".wiki/state/jobs/{}/journal.bin", job.run_id))?,
        )?;
        if accounting.as_ref().is_none_or(|b| b.is_empty())
            && (applying
                || state.status.is_some_and(|s| s != ChangeStatus::Prepared)
                || terminal.is_some())
        {
            return Err(events::corrupt(
                "named run has later publication authority but lost accounting namespace; no budget reset",
            ));
        }
        return Ok(true);
    }
    if namespace_exists {
        // Old complete ledgers remain compatible; no second publication identity.
        if ledger::read(&job.fs, &run_path(&job.run_id)?)?.is_some() {
            return Ok(false);
        }
        return Err(events::corrupt(
            "unnamed normalized bootstrap is incomplete; explicit maintenance required",
        ));
    }
    recover_job_active(&job.fs, writer, &job.vault_id)?;
    ensure_job_directories(&job.fs, writer, &job.vault_id, &job.run_id, &key)?;
    let (candidate, reference) = if let Some((candidate, reference)) =
        orphan_job_candidate(&job.fs, &job.vault_id, &job.run_id, &key, 1)?
    {
        if candidate.request != request {
            return Err(events::corrupt("orphan bootstrap candidate/spec differs"));
        }
        let (_, terminal, applying) = job_candidate_state(&job.fs, &candidate)?;
        if applying || terminal.is_some() {
            return Err(events::corrupt(
                "orphan run commitment has later authority; no accounting recreation",
            ));
        }
        (candidate, reference)
    } else {
        let (candidate, _) = seal_job_candidate(
            &catalog,
            writer,
            job,
            validated,
            key.clone(),
            request,
            1,
            None,
        )?;
        let reference = retain_job_candidate(&job.fs, writer, &candidate)?;
        (candidate, reference)
    };
    install_job_slot(
        &job.fs,
        writer,
        &candidate,
        reference,
        ExpectedState::Absent,
    )?;
    Ok(true)
}
/// A downgrade is narrowly regenerated from the same Received accounting. It
/// preserves the original Validated candidate rather than replacing its bytes.
fn job_receipt_downgrade(old: &JobRequestCommitment, new: &JobRequestCommitment) -> bool {
    old.title == "Retain accounted response receipt"
        && new.title == old.title
        && old.allocated_ids == new.allocated_ids
        && old.created_at == new.created_at
        && old.receipt_disposition == Some(OutputDisposition::Validated)
        && matches!(
            new.receipt_disposition,
            Some(OutputDisposition::Rejected | OutputDisposition::Unknown)
        )
        && new.receipt_only
        && old.writes.len() == 1
        && new.writes.len() == 1
        && old.writes[0].target == new.writes[0].target
        && old.writes[0].before == ExpectedState::Absent
        && new.writes[0].before == ExpectedState::Absent
        && old.writes[0].apply_after.is_empty()
        && new.writes[0].apply_after.is_empty()
}
pub(crate) fn inspect_for_publication(job: &JobLedger) -> Result<LedgerInspection> {
    job.with(false, |_, l| Ok(l.state.inspection.clone()))
}
pub(crate) enum JobPreparation<'a> {
    Committed(PreparedChange),
    Pending(crate::catalog::source_refresh::IndexedRefreshSession<'a>),
}
/// No history discovery: one slot/current candidate, at most one directly named
/// orphan successor, exact active authority, and one never-applied transition.
pub(crate) fn prepare_job_draft<'a>(
    catalog: &Catalog,
    writer: &'a WriterPermit,
    job: &JobLedger,
    draft: ChangeDraft,
) -> Result<JobPreparation<'a>> {
    use crate::catalog::source_refresh::IndexedRefreshSession;
    writer.require_root(job.fs.root())?;
    recover_job_active(&job.fs, writer, &job.vault_id)?;
    let key = job_key(&draft)?;
    let created_at = job.with(false, |_, l| {
        timestamp(l.state.inspection.spec.created_at_utc_ms)
    })?;
    // Validation regenerates every disposition from retained Received bytes and
    // proves bootstrap genesis match. It never blesses the slot as accounting.
    let validated = validate_job_draft(job, draft)?;
    let request = job_request(&validated.draft, created_at)?;
    let fs = &job.fs;
    ensure_job_directories(fs, writer, &job.vault_id, &job.run_id, &key)?;
    let mut located = read_job_slot(fs, &job.vault_id, &job.run_id, &key)?;
    if located.is_none() {
        if let Some((candidate, reference)) =
            orphan_job_candidate(fs, &job.vault_id, &job.run_id, &key, 1)?
        {
            if candidate.request != request {
                return Err(events::corrupt("orphan named job input differs"));
            }
            install_job_slot(fs, writer, &candidate, reference, ExpectedState::Absent)?;
            located = read_job_slot(fs, &job.vault_id, &job.run_id, &key)?;
        } else {
            let (candidate, sealed) =
                seal_job_candidate(catalog, writer, job, validated, key, request, 1, None)?;
            let reference = retain_job_candidate(fs, writer, &candidate)?;
            install_job_slot(fs, writer, &candidate, reference, ExpectedState::Absent)?;
            return IndexedRefreshSession::prepare_named_write(
                catalog,
                writer,
                &candidate.named,
                sealed,
            )
            .map(JobPreparation::Pending);
        }
    }
    let (slot, slot_hash, candidate) =
        located.ok_or_else(|| events::corrupt("job locator disappeared"))?;
    let same = candidate.request == request;
    let downgrade = job_receipt_downgrade(&candidate.request, &request);
    if !same && !downgrade {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "immutable named job input/original guards differ",
        ));
    }
    let (state, terminal, applying) = job_candidate_state(fs, &candidate)?;
    let engine = ChangeEngine::new(fs.clone())?;
    if terminal
        .as_ref()
        .is_some_and(|r| r.status == ChangeStatus::Committed)
    {
        if !same {
            return Err(events::corrupt(
                "committed Validated receipt cannot change disposition",
            ));
        }
        engine
            .indexed_refresh_terminal_report(writer, &candidate.named.proof.change)?
            .filter(|r| r.status == ChangeStatus::Committed)
            .ok_or_else(|| events::corrupt("exact named job terminal is not committed"))?;
        return Ok(JobPreparation::Committed(candidate.named.proof.change));
    }
    if applying {
        if !same {
            return Err(events::corrupt(
                "possibly applying job receipt cannot change disposition",
            ));
        }
        let proof = engine
            .load_indexed_refresh_proof(&candidate.named.proof.change)?
            .filter(|p| p == &candidate.named.proof)
            .ok_or_else(|| {
                events::corrupt("exact applying job proof missing/different; maintenance required")
            })?;
        return IndexedRefreshSession::resume(catalog, writer, proof).map(JobPreparation::Pending);
    }
    let current = {
        use crate::catalog::query_types::QueryCatalog;
        catalog
            .query_snapshot(crate::catalog::query_types::QueryReadLimits::default())?
            .snapshot()
            .clone()
    };
    if same
        && current == candidate.named.proof.base
        && terminal.is_none()
        && state.status != Some(ChangeStatus::Aborted)
    {
        if let Some(proof) = engine.load_indexed_refresh_proof(&candidate.named.proof.change)? {
            if proof != candidate.named.proof {
                return Err(events::corrupt("retained named job proof differs"));
            }
            return IndexedRefreshSession::resume(catalog, writer, proof)
                .map(JobPreparation::Pending);
        }
        let (again, sealed) = seal_job_candidate(
            catalog,
            writer,
            job,
            validated,
            key,
            request,
            candidate.generation,
            candidate.previous.clone(),
        )?;
        if again != candidate {
            return Err(events::corrupt(
                "partial named job seal differs from frozen input/selected guards",
            ));
        }
        return IndexedRefreshSession::prepare_named_write(
            catalog,
            writer,
            &candidate.named,
            sealed,
        )
        .map(JobPreparation::Pending);
    }
    // Epoch advance or a receipt-only downgrade can close only never-applied
    // work with exact before targets. Original guards and candidates survive.
    let closure = prove_job_candidate_closable(fs, writer, &candidate, &slot.current)?;
    let generation = candidate
        .generation
        .checked_add(1)
        .filter(|g| *g <= MAX_JOB_CANDIDATES)
        .ok_or_else(|| {
            WikiError::new(
                ErrorCode::BudgetExceeded,
                "job candidate transition bound reached; maintenance required",
            )
        })?;
    if let Some((next, reference)) =
        orphan_job_candidate(fs, &job.vault_id, &job.run_id, &key, generation)?
    {
        if next.previous.as_ref() != Some(&closure) || next.request != request {
            return Err(events::corrupt(
                "retained job successor input/closure differs",
            ));
        }
        install_job_slot(fs, writer, &next, reference, ExpectedState::Hash(slot_hash))?;
        // One transition per invocation. A frozen orphan with another stale
        // base needs a later invocation; it is never silently reconstructed.
        let proof = engine.load_indexed_refresh_proof(&next.named.proof.change)?;
        if let Some(proof) = proof.filter(|p| p == &next.named.proof) {
            return IndexedRefreshSession::resume(catalog, writer, proof)
                .map(JobPreparation::Pending);
        }
        let (again, sealed) = seal_job_candidate(
            catalog,
            writer,
            job,
            validated,
            key,
            request,
            generation,
            Some(closure),
        )?;
        if again != next {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "retained successor needs a subsequent bounded rebase invocation",
            ));
        }
        return IndexedRefreshSession::prepare_named_write(catalog, writer, &next.named, sealed)
            .map(JobPreparation::Pending);
    }
    let (next, sealed) = seal_job_candidate(
        catalog,
        writer,
        job,
        validated,
        key,
        request,
        generation,
        Some(closure),
    )?;
    let reference = retain_job_candidate(fs, writer, &next)?;
    install_job_slot(fs, writer, &next, reference, ExpectedState::Hash(slot_hash))?;
    IndexedRefreshSession::prepare_named_write(catalog, writer, &next.named, sealed)
        .map(JobPreparation::Pending)
}
/// Return exact committed publication through the named slot. Absence means
/// legacy compatibility only; a present corrupt/partial locator never falls back.
pub(crate) fn named_job_committed(
    fs: &VaultFs,
    vault: &RecordId,
    run: &RecordId,
    key: JobPublicationKey,
    path: &VaultRelativePath,
    hash: &Blake3Hash,
) -> Result<Option<PreparedChange>> {
    let Some((_, _, candidate)) = read_job_slot(fs, vault, run, &key)? else {
        return Ok(None);
    };
    let (_, terminal, _) = job_candidate_state(fs, &candidate)?;
    if terminal.is_none_or(|r| r.status != ChangeStatus::Committed) {
        return Err(events::corrupt(
            "named canonical job output is not committed",
        ));
    }
    let engine = ChangeEngine::new(fs.clone())?;
    engine.prove_committed_output(&candidate.named.proof.change, path, hash)?;
    Ok(Some(candidate.named.proof.change))
}
/// Exact canonical commit-before-ledger-ack recovery. A Received spool is the
/// accounting source; this merely identifies its already committed receipt.
pub(crate) fn retained_receipt_publication(
    fs: &VaultFs,
    job: &JobLedger,
    expected: &UsageReceipt,
) -> Result<Option<(PreparedChange, DurableOutputRef)>> {
    if expected.attempt.run_id != job.run_id
        || expected.receipt_id != receipt_id(&expected.attempt)?
    {
        return Err(WikiError::invalid("receipt lookup attempt binding differs"));
    }
    let path = event_path(&job.run_id, &expected.receipt_id)?;
    let Some(bytes) = crate::changes::prepare::read_bounded(fs, &path, EVENT_MAX_BYTES)? else {
        return Ok(None);
    };
    let reference = DurableOutputRef {
        record: RecordRef {
            vault_id: job.vault_id.clone(),
            record_id: expected.receipt_id.clone(),
            expected_kind: RecordKind::RunEvent,
        },
        path,
        hash: Blake3Hash::digest(bytes),
    };
    if receipt(fs, &reference)? != *expected {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "immutable canonical receipt differs",
        ));
    }
    let change = named_job_committed(
        fs,
        &job.vault_id,
        &job.run_id,
        JobPublicationKey::Receipt {
            receipt_id: expected.receipt_id.clone(),
        },
        &reference.path,
        &reference.hash,
    )?
    .ok_or_else(|| {
        events::corrupt(
            "canonical normalized receipt lacks named publication; maintenance required",
        )
    })?;
    Ok(Some((change, reference)))
}
/// Canonical commit, vector commit, and ledger acknowledgment are distinct.
pub(crate) fn publish_job_draft(
    fs: &VaultFs,
    writer: &WriterPermit,
    job: &JobLedger,
    draft: ChangeDraft,
) -> Result<PreparedChange> {
    if fs.root() != job.fs.root() {
        return Err(WikiError::invalid(
            "job publication belongs to another root",
        ));
    }
    let catalog = Catalog::new(fs.clone(), job.vault_id.clone());
    let engine = ChangeEngine::new(fs.clone())?;
    if catalog.operation_state()?.is_none() {
        let prepared = ledger::find_or_prepare(&engine, writer, &draft)?;
        engine.apply(writer, &prepared, &CatalogGraphValidator, &catalog)?;
        return Ok(prepared);
    }
    match prepare_job_draft(&catalog, writer, job, draft)? {
        JobPreparation::Committed(prepared) => Ok(prepared),
        JobPreparation::Pending(mut session) => {
            let prepared = session.proof().change.clone();
            engine.apply_indexed_refresh(writer, &mut session)?;
            Ok(prepared)
        }
    }
}

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
    let normalized = catalog.operation_state()?.is_some();
    if normalized {
        recover_job_active(fs, &writer, &job.vault_id)?;
    } else {
        engine.recover(&writer, &CatalogGraphValidator, &catalog)?;
    }
    // The named publisher recovers exact active/committed authority. Reading the
    // ledger without reconciliation cannot enumerate unrelated change history.
    let inspection = if normalized {
        job.with(false, |_, l| Ok(l.state.inspection.clone()))?
    } else {
        job.replay()?.inspection
    };
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
        if normalized {
            named_job_committed(
                fs,
                &job.vault_id,
                &job.run_id,
                JobPublicationKey::Receipt {
                    receipt_id: plan.receipt.receipt_id.clone(),
                },
                &reference.path,
                &reference.hash,
            )?
            .ok_or_else(|| {
                events::corrupt(
                    "normalized receipt lacks exact named publication; maintenance required",
                )
            })?;
        }
        drop(writer);
        job.settle(&plan.attempt)?;
        return Ok(());
    }
    if normalized {
        if let Some((change, reference)) = retained_receipt_publication(fs, job, &plan.receipt)? {
            drop(writer);
            job.outputs_committed(&plan.attempt, &change, reference, vec![], vec![])?;
            job.settle(&plan.attempt)?;
            return Ok(());
        }
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
    let prepared = if normalized {
        publish_job_draft(fs, &writer, job, fresh.draft)?
    } else {
        let prepared = engine.prepare(&writer, fresh.draft)?.prepared;
        engine.apply(&writer, &prepared, &CatalogGraphValidator, &catalog)?;
        prepared
    };
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
fn usage_receipt_bytes(receipt: &UsageReceipt, received: &LedgerEvent) -> Result<Vec<u8>> {
    let mut f = fields(
        &receipt.receipt_id,
        RecordKind::RunEvent,
        "Immutable usage receipt",
    );
    f.insert("wiki_run_id".into(), receipt.attempt.run_id.as_str().into());
    f.insert("wiki_sequence".into(), received.sequence.into());
    f.insert("wiki_event_type".into(), "usage_receipt".into());
    f.insert(
        "wiki_occurred_at".into(),
        timestamp(received.occurred_at_utc_ms)?.into(),
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
    Ok(bytes)
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
    let bytes = usage_receipt_bytes(&receipt, &received.event)?;
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
    known: Option<&PreparedChange>,
) -> Result<PreparedChange> {
    if let Some(change) = known {
        ChangeEngine::new(fs.clone())?.prove_committed_output(change, path, hash)?;
        return Ok(change.clone());
    }
    let parts: Vec<_> = path.as_str().split('/').collect();
    if parts.len() >= 3 && parts[0] == "runs" {
        let run = RecordId::new(parts[1])?;
        let run_hash = if parts.len() == 3 && parts[2] == "run.md" {
            Some(hash.clone())
        } else if parts.len() == 4 && parts[2] == "checkpoints" {
            parts[3]
                .strip_suffix(".json")
                .map(|hex| Blake3Hash::new(format!("blake3:{hex}")))
                .transpose()?
        } else {
            None
        };
        if let Some(run_hash) = run_hash {
            let engine = ChangeEngine::new(fs.clone())?;
            if let Some(prepared) = named_job_committed(
                fs,
                engine.vault_id(),
                &run,
                JobPublicationKey::Checkpoint { run_hash },
                path,
                hash,
            )? {
                return Ok(prepared);
            }
        }
    }
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
    known: Option<&PreparedChange>,
) -> Result<Option<CompactCheckpointProof>> {
    // Neither a checksummed sidecar nor regenerated bytes alone prove publication.
    let located = named_job_committed(
        fs,
        &note_ref.record.vault_id,
        &note_ref.record.record_id,
        JobPublicationKey::Checkpoint {
            run_hash: note_ref.hash.clone(),
        },
        &note_ref.path,
        &note_ref.hash,
    )?;
    if known.is_some_and(|known| located.as_ref().is_some_and(|located| known != located)) {
        return Err(events::corrupt(
            "known checkpoint differs from its named publication",
        ));
    }
    if located.is_none()
        && read_job_slot(
            fs,
            &note_ref.record.vault_id,
            &note_ref.record.record_id,
            &JobPublicationKey::Bootstrap,
        )?
        .is_some()
    {
        return Err(events::corrupt(
            "named run lost its exact compact publication locator; maintenance required",
        ));
    }
    let retained = known.or(located.as_ref());
    let indexed = retained
        .map(|change| {
            let engine = ChangeEngine::new(fs.clone())?;
            let (manifest, hash) = engine.load_manifest_structure(&change.change_id)?;
            if hash != change.manifest_hash {
                return Err(events::corrupt(
                    "checkpoint validation manifest hash differs",
                ));
            }
            let catalog = Catalog::new(fs.clone(), note_ref.record.vault_id.clone());
            let authority = catalog.operation_state()?;
            engine.indexed_replay_proof(&manifest, change, authority.as_ref())
        })
        .transpose()?
        .flatten();
    let job_checkpoint = indexed.as_ref().is_some_and(|proof| {
        matches!(
            &proof.operation,
            Some(
                crate::changes::indexed_refresh::IndexedWriteOperation::JobBatch {
                    checkpoint: Some(_),
                    ..
                }
            )
        )
    });
    if located.is_some() && !job_checkpoint {
        return Err(events::corrupt(
            "named checkpoint retained JobBatch proof is missing or differs; maintenance required",
        ));
    }
    let requires_compact = located.is_some() || job_checkpoint;
    let path = compact_path(note_ref)?;
    let Some(bytes) = crate::changes::prepare::read_bounded(fs, &path, EVENT_MAX_BYTES)? else {
        if requires_compact {
            return Err(events::corrupt(
                "named checkpoint compact asset is missing; maintenance required",
            ));
        }
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
    let asset_hash = Blake3Hash::digest(&bytes);
    let mut job_asset_bound = false;
    if let Some(change) = retained {
        let engine = ChangeEngine::new(fs.clone())?;
        if let Some(indexed) = &indexed {
            if let Some(crate::changes::indexed_refresh::IndexedWriteOperation::JobBatch {
                run_id,
                checkpoint: Some(asset),
                ..
            }) = &indexed.operation
            {
                let (manifest, hash) = engine.load_manifest_structure(&change.change_id)?;
                if hash != change.manifest_hash
                    || run_id != &note_ref.record.record_id
                    || asset.path != path
                    || asset.hash != asset_hash
                    || asset.run_hash != note_ref.hash
                {
                    return Err(events::corrupt(
                        "compact asset differs from exact committed JobBatch",
                    ));
                }
                indexed.validate_manifest(&manifest)?;
                // The committed JobBatch binds either its immutable asset create
                // or exact original existing-asset guard. Neither requires
                // discovery of an unrelated asset publication in history.
                job_asset_bound = true;
            }
        }
    }
    if !job_asset_bound {
        committed_hash(fs, &path, &asset_hash, None)?;
    }
    committed_hash(fs, &note_ref.path, &note_ref.hash, retained)?;
    Ok(Some(envelope.proof))
}
pub(super) fn verify_summary_proof(
    fs: &VaultFs,
    note_ref: &DurableOutputRef,
    summary: &CompletedTaskSummary,
    frames: &[JournalFrame],
) -> Result<()> {
    let proof = match read_compact(fs, note_ref, frames, None)? {
        Some(proof) => proof,
        None => {
            let bytes = legacy_summary_bytes(fs, note_ref)?;
            let proof = proof_for_bytes(note_ref, &bytes, frames)?;
            committed_hash(fs, &note_ref.path, &note_ref.hash, None)?;
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
    if let Some(proof) = read_compact(fs, run, frames, Some(change))? {
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
    let proof = match read_compact(fs, note_ref, frames, None)? {
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
