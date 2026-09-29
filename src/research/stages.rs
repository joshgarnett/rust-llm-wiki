//! Immutable stage records and actual-response receipt publication.
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::*,
    domain::*,
    graph::packet::canonical_json,
    jobs::*,
    providers::types::{DispatchOutcome, ValidatedOutput},
    records::parse_note,
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};

const FENCE: &str = "lwiki-research-stage-v1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageOutput {
    pub version: u32,
    pub task_key: Blake3Hash,
    pub stage: TaskStage,
    pub attempt: Option<AttemptRef>,
    pub response: serde_json::Value,
    pub response_hash: Blake3Hash,
}

pub fn output_write(
    vault: &RecordId,
    run: &RecordId,
    task: &TaskSpec,
    attempt: Option<&AttemptRef>,
    response: serde_json::Value,
    now: i64,
) -> Result<(DurableOutputRef, ExpectedWrite)> {
    if attempt.is_some_and(|a| &a.run_id != run || a.task_key != task.key) {
        return Err(WikiError::invalid("stage attempt binding differs"));
    }
    let response_hash = Blake3Hash::digest(canonical_json(&response)?);
    let output = StageOutput {
        version: 1,
        task_key: task.key.clone(),
        stage: task.stage,
        attempt: attempt.cloned(),
        response,
        response_hash,
    };
    let id = RecordId::new(format!(
        "run_event_research_{}",
        Blake3Hash::digest(canonical_json(&(run, &output))?).hex()
    ))?;
    let timestamp = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(now) * 1_000_000)
        .map_err(|_| WikiError::invalid("stage UTC time invalid"))?
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| WikiError::invalid("stage UTC encoding"))?;
    let fields = BTreeMap::from([
        ("wiki_schema".into(), serde_json::json!("1")),
        ("wiki_id".into(), serde_json::json!(id)),
        ("wiki_kind".into(), serde_json::json!("run_event")),
        (
            "title".into(),
            serde_json::json!("Immutable research stage output"),
        ),
        ("wiki_run_id".into(), serde_json::json!(run)),
        (
            "wiki_sequence".into(),
            serde_json::json!(attempt.map_or(0, |a| a.number)),
        ),
        (
            "wiki_event_type".into(),
            serde_json::json!("research_stage"),
        ),
        ("wiki_occurred_at".into(), serde_json::json!(timestamp)),
    ]);
    CanonicalRecord::new(fields.clone())?;
    let mut bytes = b"---\n".to_vec();
    for (key, value) in fields {
        bytes.extend_from_slice(
            format!(
                "{key}: {}\n",
                serde_json::to_string(&value)
                    .map_err(|_| WikiError::invalid("stage field encoding"))?
            )
            .as_bytes(),
        );
    }
    bytes.extend_from_slice(b"---\n\n");
    // Research envelopes contain optional fields. Keep their bounded JSON codec
    // separate from extraction packets, whose contract intentionally rejects null.
    let encoded = canonical_json(&output)?;
    super::frontier::parse_value(&encoded, &super::frontier::StageLimits::default())?;
    bytes.extend_from_slice(format!("```{FENCE}\n").as_bytes());
    bytes.extend_from_slice(&encoded);
    bytes.extend_from_slice(b"\n```\n");
    let path = VaultRelativePath::new(format!("runs/{run}/outputs/{id}.md"))?;
    let reference = DurableOutputRef {
        record: RecordRef {
            vault_id: vault.clone(),
            record_id: id,
            expected_kind: RecordKind::RunEvent,
        },
        path: path.clone(),
        hash: Blake3Hash::digest(&bytes),
    };
    Ok((
        reference,
        ExpectedWrite {
            target: path,
            expected: ExpectedState::Absent,
            proposed: Some(bytes),
            apply_after: vec![],
        },
    ))
}

pub fn read_output(
    fs: &VaultFs,
    reference: &DurableOutputRef,
    task: &TaskSpec,
) -> Result<StageOutput> {
    let bytes = crate::changes::prepare::read_bounded(fs, &reference.path, 512 * 1024)?
        .ok_or_else(|| {
            WikiError::new(ErrorCode::RecoveryRequired, "research stage output missing")
        })?;
    if Blake3Hash::digest(&bytes) != reference.hash {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "research stage output changed",
        ));
    }
    let parsed = parse_note(&bytes);
    let record = parsed
        .canonical
        .as_ref()
        .ok_or_else(|| WikiError::invalid("invalid research output record"))?;
    if record.id() != &reference.record.record_id
        || record.kind() != RecordKind::RunEvent
        || reference.record.expected_kind != RecordKind::RunEvent
    {
        return Err(WikiError::invalid(
            "research stage output ownership differs",
        ));
    }
    let value = super::frontier::parse_value(
        crate::graph::packet::fenced_json(&parsed, FENCE, 256 * 1024)?,
        &super::frontier::StageLimits::default(),
    )?;
    let output: StageOutput = serde_json::from_value(value)
        .map_err(|_| WikiError::invalid("research stage output encoding differs"))?;
    if output.version != 1
        || output.task_key != task.key
        || output.stage != task.stage
        || output.response_hash != Blake3Hash::digest(canonical_json(&output.response)?)
    {
        return Err(WikiError::invalid("research stage output contract differs"));
    }
    Ok(output)
}

/// Caller first validates the stage-specific schema and current citation pool.
/// This stores the actual decoded provider output with its exact paid receipt.
pub fn publish_generation(
    fs: &VaultFs,
    ledger: &JobLedger,
    task: &TaskSpec,
    outcome: DispatchOutcome,
    _now: i64,
) -> Result<DurableOutputRef> {
    let inspection = ledger.inspect()?;
    if inspection
        .tasks
        .get(&task.key)
        .is_none_or(|t| &t.spec != task)
    {
        return Err(WikiError::invalid("generation task not admitted"));
    }
    let ValidatedOutput::Generation { text, .. } = &outcome.output else {
        return Err(WikiError::invalid("generation stage requires text output"));
    };
    let response: serde_json::Value = crate::changes::prepare::strict_json(text.as_bytes())?;
    let (output, write) = output_write(
        &inspection.spec.vault_id,
        &inspection.spec.run_id,
        task,
        Some(&outcome.attempt),
        response,
        inspection.spec.created_at_utc_ms,
    )?;
    let plan = crate::jobs::checkpoint::receipt_plan(
        ledger,
        &outcome.attempt,
        OutputDisposition::Validated,
        vec![output.clone()],
        vec![],
        vec![write],
    )?;
    crate::research::acquire::settle_receipt(fs, ledger, plan)?;
    ledger.finish_remote_task(&task.key, vec![output.clone()], vec![], |_| Ok(false))?;
    Ok(output)
}

pub fn publish_local(
    fs: &VaultFs,
    ledger: &JobLedger,
    task: &TaskSpec,
    response: serde_json::Value,
    dependencies: Vec<ReadDependency>,
    _now: i64,
) -> Result<DurableOutputRef> {
    let (_, _, _, options) = ledger.dispatcher_bindings();
    if options.policy.dry_run {
        return Err(WikiError::new(
            ErrorCode::OfflineUnavailable,
            "dry-run cannot publish a research stage",
        ));
    }
    let inspection = ledger.inspect()?;
    let (output, write) = output_write(
        &inspection.spec.vault_id,
        &inspection.spec.run_id,
        task,
        None,
        response,
        inspection.spec.created_at_utc_ms,
    )?;
    for dependency in &dependencies {
        let actual = crate::changes::prepare::read_bounded(
            fs,
            &dependency.path,
            crate::changes::prepare::MAX_PAYLOAD_BYTES,
        )?
        .map_or(ExpectedState::Absent, |bytes| {
            ExpectedState::Hash(Blake3Hash::digest(bytes))
        });
        if actual != dependency.expected {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "local research stage dependency changed",
            ));
        }
    }
    if let Some(bytes) = crate::changes::prepare::read_bounded(fs, &output.path, 512 * 1024)? {
        if Blake3Hash::digest(&bytes) != output.hash {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "immutable local stage output conflicts",
            ));
        }
        read_output(fs, &output, task)?;
        ledger.finish_local_task(&task.key, vec![output.clone()], vec![])?;
        return Ok(output);
    }
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(5))?;
    let engine = ChangeEngine::new(fs.clone())?;
    let catalog = Catalog::new(fs.clone(), inspection.spec.vault_id);
    let draft = ChangeDraft {
        title: "Retain local research stage".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: dependencies,
        operations: vec![write],
    };
    let prepared = engine.prepare(&writer, draft)?.prepared;
    engine.apply(&writer, &prepared, &CatalogGraphValidator, &catalog)?;
    drop(writer);
    ledger.finish_local_task(&task.key, vec![output.clone()], vec![])?;
    Ok(output)
}
