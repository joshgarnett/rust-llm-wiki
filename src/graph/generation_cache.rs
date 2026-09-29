//! Durable generation outputs; their Markdown and receipts survive cache loss.
use super::{extraction_types::*, packet};
use crate::{
    changes::*,
    config::providers::TrustedService,
    domain::*,
    jobs::*,
    providers::{types::*, wire},
    records::parse_note,
    sources::SourceView,
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const OUTPUT_FENCE: &str = "lwiki-api-extraction-output-v1";
pub struct GenerationTaskPlan {
    pub task: TaskSpec,
    pub descriptor: Vec<u8>,
}

pub fn plan_task(
    packet: &VerifiedPacket,
    service: &TrustedService,
    max_output_tokens: u64,
) -> Result<GenerationTaskPlan> {
    if service.summary().capability != Capability::Generate || max_output_tokens == 0 {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "API extraction requires generation and a positive output-token limit",
        ));
    }
    let p = packet.packet();
    let input = RemoteInput {
        version: 1,
        operation: RemoteOperation::Generate {
            instructions: p.instructions.clone(),
            data: String::from_utf8(packet::canonical_json(p)?)
                .map_err(|_| WikiError::invalid("packet encoding"))?,
            output_schema: p.output_schema.clone(),
            max_output_tokens,
        },
    };
    let descriptor = packet::canonical_json(&input)?;
    if descriptor.len() > 256 * 1024 {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "API extraction descriptor exceeds dispatcher ceiling; select smaller source windows",
        ));
    }
    let f = wire::task_fingerprints(service, &input)?;
    let mut task = TaskSpec {
        key: Blake3Hash::digest([]),
        stage: TaskStage::Extract,
        capability: Some(Capability::Generate),
        priority: 0,
        dependencies: vec![],
        input_hash: f.input.clone(),
        prompt_hash: f.prompt,
        schema_hash: f.schema,
        model_hash: Some(f.model),
        settings_hash: f.settings,
        source_bindings: packet.dependencies().to_vec(),
        input: BoundedPayloadRef {
            path: VaultRelativePath::new(format!(
                ".wiki/cache/generation/inputs/{}.json",
                f.input.hex()
            ))?,
            hash: Blake3Hash::digest(&descriptor),
            byte_len: descriptor.len() as u64,
        },
    };
    task.key = crate::jobs::tasks::task_key(&task)?;
    Ok(GenerationTaskPlan { task, descriptor })
}

pub(crate) fn retain_input(
    fs: &VaultFs,
    writer: &WriterPermit,
    plan: &GenerationTaskPlan,
) -> Result<()> {
    if let Some(existing) =
        crate::changes::prepare::read_bounded(fs, &plan.task.input.path, 256 * 1024)?
    {
        if existing != plan.descriptor {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "immutable generation input conflicts",
            ));
        }
        return Ok(());
    }
    fs.ensure_directory(
        &VaultRelativePath::new(".wiki/cache/generation/inputs")?,
        writer,
    )?;
    let staged = fs.stage(&plan.task.input.path, &plan.descriptor, writer)?;
    fs.replace(staged, &ExpectedState::Absent, writer)?;
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerationOutput {
    pub version: u32,
    pub task_key: Blake3Hash,
    pub packet_id: RecordId,
    pub packet_fingerprint: Blake3Hash,
    pub attempt: AttemptRef,
    pub response: String,
    pub response_hash: Blake3Hash,
}

pub(crate) fn output_write(
    vault: &RecordId,
    attempt: &AttemptRef,
    packet: &VerifiedPacket,
    response: &[u8],
    timestamp: &str,
) -> Result<(DurableOutputRef, ExpectedWrite)> {
    let id = RecordId::new(format!(
        "run_event_generation_{}",
        Blake3Hash::digest(packet::canonical_json(attempt)?).hex()
    ))?;
    let out = GenerationOutput {
        version: 1,
        task_key: attempt.task_key.clone(),
        packet_id: packet.packet().packet_id.clone(),
        packet_fingerprint: packet.packet().packet_fingerprint.clone(),
        attempt: attempt.clone(),
        response: String::from_utf8(response.to_vec())
            .map_err(|_| WikiError::invalid("generation response UTF-8"))?,
        response_hash: Blake3Hash::digest(response),
    };
    let fields = BTreeMap::from([
        ("wiki_schema".into(), serde_json::json!("1")),
        ("wiki_id".into(), serde_json::json!(id)),
        ("wiki_kind".into(), serde_json::json!("run_event")),
        (
            "title".into(),
            serde_json::json!("Validated source-local generation"),
        ),
        ("wiki_run_id".into(), serde_json::json!(attempt.run_id)),
        ("wiki_sequence".into(), serde_json::json!(attempt.number)),
        (
            "wiki_event_type".into(),
            serde_json::json!("generation_output"),
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
                    .map_err(|_| WikiError::invalid("generation fields"))?
            )
            .as_bytes(),
        );
    }
    bytes.extend_from_slice(b"---\n\n");
    bytes.extend_from_slice(&packet::render_fence(
        &out,
        OUTPUT_FENCE,
        MAX_ARTIFACT_BYTES,
    )?);
    let path = VaultRelativePath::new(format!("runs/{}/outputs/{id}.md", attempt.run_id))?;
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

pub(crate) fn load_output(
    fs: &VaultFs,
    reference: &DurableOutputRef,
    task: &TaskSpec,
    packet: &VerifiedPacket,
) -> Result<GenerationOutput> {
    let bytes = crate::changes::prepare::read_bounded(fs, &reference.path, MAX_ARTIFACT_BYTES)?
        .ok_or_else(|| {
            WikiError::new(ErrorCode::RecoveryRequired, "retained generation missing")
        })?;
    if Blake3Hash::digest(&bytes) != reference.hash {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "retained generation changed",
        ));
    }
    let note = parse_note(&bytes);
    let record = note
        .canonical
        .as_ref()
        .ok_or_else(|| WikiError::invalid("generation output record"))?;
    if record.id() != &reference.record.record_id || record.kind() != RecordKind::RunEvent {
        return Err(WikiError::invalid("generation output identity"));
    }
    let out: GenerationOutput = packet::decode(
        packet::fenced_json(&note, OUTPUT_FENCE, MAX_ARTIFACT_BYTES)?,
        MAX_ARTIFACT_BYTES,
    )?;
    if out.version != 1
        || out.task_key != task.key
        || out.attempt.task_key != task.key
        || out.packet_id != packet.packet().packet_id
        || out.packet_fingerprint != packet.packet().packet_fingerprint
        || Blake3Hash::digest(out.response.as_bytes()) != out.response_hash
    {
        return Err(WikiError::invalid("generation output binding"));
    }
    let view = SourceView::from_fs_bounded(fs, packet::SOURCE_CAP, 4096)?;
    super::wire::validate_response(packet, &view, out.response.as_bytes())?;
    Ok(out)
}
