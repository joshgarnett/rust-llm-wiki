//! Pure, bounded retained Markdown codecs. The caller owns reads, authenticated
//! task/source authority, publication, currentness and shared accounting. Raw
//! generation survives descriptor-cache loss; annotations confer no fact status.
use super::{source_relation_packet, source_relations::*};
use crate::{
    changes::{ExpectedWrite, ReadDependency, prepare::strict_json},
    domain::*,
    jobs::{AttemptRef, Capability, DurableOutputRef, TaskSpec, TaskStage},
    providers::types::{RemoteInput, RemoteOperation},
    records::{ParsedNote, parse_note},
    vault::ExpectedState,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write, time::Instant};

pub(crate) const MAX_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_DESCRIPTOR_BYTES: usize = 256 * 1024;
pub(crate) const MAX_RESPONSE_BYTES: usize = 256 * 1024;
pub(crate) const MAX_SOURCE_BINDINGS: usize = 16;
pub(crate) const MAX_TASK_DEPENDENCIES: usize = 64;
pub(crate) const MAX_PATH_BYTES: usize = 4096;
pub(crate) const RAW_FENCE: &str = "lwiki-source-relation-generation-v1";
pub(crate) const ANNOTATION_FENCE: &str = "lwiki-source-relation-annotation-v1";
const RAW_ARTIFACT_SCHEMA: &str = "lwiki.source-relation-generation.v1";
const ANNOTATION_ARTIFACT_SCHEMA: &str = "lwiki.source-relation-annotation.v1";

/// Recipe authority comes from the caller, never from generated response text.
/// The remaining identities must equal its real-service TaskSpec fingerprints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparationIdentity {
    pub recipe_id: RecordId,
    pub recipe_hash: Blake3Hash,
    pub prompt_hash: Blake3Hash,
    pub schema_hash: Blake3Hash,
    pub model_identity_hash: Blake3Hash,
    pub settings_hash: Blake3Hash,
}
impl PreparationIdentity {
    pub(crate) fn bind(&self, raw: &DurableOutputRef) -> PreparationBinding {
        PreparationBinding {
            recipe_id: self.recipe_id.clone(),
            recipe_hash: self.recipe_hash.clone(),
            prompt_hash: self.prompt_hash.clone(),
            schema_hash: self.schema_hash.clone(),
            model_identity_hash: self.model_identity_hash.clone(),
            settings_hash: self.settings_hash.clone(),
            retained_generation_id: raw.record.record_id.clone(),
            retained_generation_hash: raw.hash.clone(),
        }
    }
    fn from_binding(binding: &PreparationBinding) -> Self {
        Self {
            recipe_id: binding.recipe_id.clone(),
            recipe_hash: binding.recipe_hash.clone(),
            prompt_hash: binding.prompt_hash.clone(),
            schema_hash: binding.schema_hash.clone(),
            model_identity_hash: binding.model_identity_hash.clone(),
            settings_hash: binding.settings_hash.clone(),
        }
    }
}

/// No enriched Envelope: its retained-generation hash would self-reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawGeneration {
    pub schema: String,
    pub vault_id: RecordId,
    pub task: TaskSpec,
    pub source: SourceBinding,
    pub preparation: PreparationIdentity,
    pub attempt: AttemptRef,
    /// Exact complete UTF-8 RemoteInput JSON, not a normalized re-encoding.
    pub descriptor: String,
    pub descriptor_hash: Blake3Hash,
    pub response: String,
    pub response_hash: Blake3Hash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Annotation {
    pub schema: String,
    pub task: TaskSpec,
    pub attempt: AttemptRef,
    pub raw: DurableOutputRef,
    pub envelope: Envelope,
}

/// Remaining allowance from the enclosing operation. Each function reports
/// hashes on success and in error details, including the parse_note digest.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StoreLimits {
    pub max_hash_bytes: u64,
    pub deadline: Instant,
}
pub(crate) struct StoredArtifact {
    pub reference: DurableOutputRef,
    pub write: ExpectedWrite,
    pub hash_bytes: u64,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain exact store hash-work receipts independently of payload identity; tests audit the counters"
        )
    )]
    pub hashes: usize,
}
pub(crate) struct LoadedArtifact<T> {
    pub artifact: T,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain exact store hash-work receipts independently of payload identity; tests audit the counters"
        )
    )]
    pub hash_bytes: u64,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain exact store hash-work receipts independently of payload identity; tests audit the counters"
        )
    )]
    pub hashes: usize,
}
struct Meter {
    limits: StoreLimits,
    bytes: u64,
    hashes: usize,
}
impl Meter {
    fn check(&self) -> Result<()> {
        if Instant::now() >= self.limits.deadline {
            return Err(budget("source relation store deadline exceeded"));
        }
        Ok(())
    }
    fn claim(&mut self, bytes: usize) -> Result<()> {
        self.check()?;
        let next = self
            .bytes
            .checked_add(bytes as u64)
            .ok_or_else(|| budget("source relation store hash-byte overflow"))?;
        if next > self.limits.max_hash_bytes {
            return Err(budget("source relation store hash-byte allowance exceeded"));
        }
        self.bytes = next;
        self.hashes += 1;
        Ok(())
    }
    fn digest(&mut self, bytes: &[u8]) -> Result<Blake3Hash> {
        self.claim(bytes.len())?;
        let result = Blake3Hash::digest(bytes);
        self.check()?;
        Ok(result)
    }
}
fn invalid(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ExtractionInvalid, message)
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn measured<T>(
    limits: StoreLimits,
    f: impl FnOnce(&mut Meter) -> Result<T>,
) -> Result<(T, u64, usize)> {
    let mut meter = Meter {
        limits,
        bytes: 0,
        hashes: 0,
    };
    let result = meter.check().and_then(|()| f(&mut meter));
    match result {
        Ok(value) => Ok((value, meter.bytes, meter.hashes)),
        Err(mut error) => {
            error.details = json!({
                "source_relation_store_hash_bytes_consumed": meter.bytes,
                "source_relation_store_hashes_consumed": meter.hashes,
            });
            Err(error)
        }
    }
}

/// Canonical object-key order, with a bounded writer. Null is legitimate in
/// TaskSpec; graph::packet's extraction decoder intentionally disallows it.
fn encoded<T: Serialize>(value: &T, max: usize) -> Result<Vec<u8>> {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .map(|(key, value)| (key, sorted(value)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
            other => other,
        }
    }
    struct Buffer {
        bytes: Vec<u8>,
        max: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|n| n > self.max)
            {
                return Err(std::io::Error::other(
                    "retained artifact byte ceiling exceeded",
                ));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer { bytes: vec![], max };
    let value = sorted(serde_json::to_value(value).map_err(|e| invalid(e.to_string()))?);
    serde_json::to_writer(&mut buffer, &value)
        .map_err(|_| budget("source relation retained artifact byte ceiling exceeded"))?;
    Ok(buffer.bytes)
}
fn fence<T: Serialize>(value: &T, info: &str) -> Result<Vec<u8>> {
    let json = encoded(value, MAX_ARTIFACT_BYTES)?;
    let mut out = format!("\n```{info}\n").into_bytes();
    out.extend_from_slice(&json);
    out.extend_from_slice(b"\n```\n");
    if out.len() > MAX_ARTIFACT_BYTES {
        return Err(budget(
            "source relation retained fence byte ceiling exceeded",
        ));
    }
    Ok(out)
}
fn task_shape(
    task: &TaskSpec,
    identity: &PreparationIdentity,
    attempt: &AttemptRef,
    meter: &mut Meter,
) -> Result<()> {
    if task.stage != TaskStage::PrepareSourceRelations
        || task.capability != Some(Capability::Generate)
        || task.prompt_hash.as_ref() != Some(&identity.prompt_hash)
        || task.schema_hash.as_ref() != Some(&identity.schema_hash)
        || task.model_hash.as_ref() != Some(&identity.model_identity_hash)
        || task.settings_hash != identity.settings_hash
        || attempt.task_key != task.key
        || attempt.number == 0
        || task.input.byte_len == 0
        || task.input.byte_len > MAX_DESCRIPTOR_BYTES as u64
        || task.input.path.as_str().len() > MAX_PATH_BYTES
        || task.dependencies.len() > MAX_TASK_DEPENDENCIES
        || task
            .dependencies
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != task.dependencies.len()
    {
        return Err(invalid(
            "source relation task, attempt or preparation binding differs",
        ));
    }
    dependencies_shape(&task.source_bindings)?;
    // The existing jobs task key binds structure, not service authority. Its
    // serialization removes these exact scheduling/identity fields. Keeping
    // wire fingerprints equal to the caller's TaskSpec is a separate check.
    let mut value = serde_json::to_value(task).map_err(|e| invalid(e.to_string()))?;
    let fields = value
        .as_object_mut()
        .ok_or_else(|| invalid("task is not an object"))?;
    for name in ["key", "priority", "dependencies"] {
        fields.remove(name);
    }
    let bytes = encoded(&value, MAX_ARTIFACT_BYTES)?;
    if meter.digest(&bytes)? != task.key {
        return Err(invalid("source relation structural task key differs"));
    }
    Ok(())
}
fn dependencies_shape(bindings: &[ReadDependency]) -> Result<()> {
    if bindings.is_empty() || bindings.len() > MAX_SOURCE_BINDINGS {
        return Err(invalid(
            "source relation source-dependency ceiling exceeded or empty",
        ));
    }
    let mut paths = std::collections::BTreeSet::new();
    for binding in bindings {
        if binding.path.as_str().len() > MAX_PATH_BYTES || !paths.insert(&binding.path) {
            return Err(invalid(
                "source relation dependency path is oversized or duplicated",
            ));
        }
    }
    Ok(())
}
fn descriptor_shape(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(invalid(
            "source relation descriptor exceeds 256 KiB or is empty",
        ));
    }
    // strict_json rejects duplicates even inside output_schema's generic Value.
    let input: RemoteInput = strict_json(bytes).map_err(|e| invalid(e.message))?;
    match input.operation {
        RemoteOperation::Generate {
            instructions,
            output_schema,
            max_output_tokens,
            ..
        } if input.version == 1
            && max_output_tokens > 0
            && instructions == source_relation_packet::INSTRUCTIONS
            && output_schema == source_relation_packet::output_schema() =>
        {
            Ok(())
        }
        _ => Err(invalid(
            "retained descriptor is not the complete source-relation generation recipe",
        )),
    }
}
fn envelope_shape(envelope: &Envelope, raw: &DurableOutputRef) -> Result<()> {
    if envelope.schema != SCHEMA
        || envelope.nodes.len() > MAX_NODES
        || envelope.edges.len() > MAX_EDGES
        || envelope.preparation.retained_generation_id != raw.record.record_id
        || envelope.preparation.retained_generation_hash != raw.hash
    {
        return Err(invalid(
            "annotation is unbound, oversized or has a different schema",
        ));
    }
    for node in &envelope.nodes {
        if node.scope.trim().is_empty()
            || node.scope.len() > 256
            || node.scope.chars().any(char::is_control)
        {
            return Err(invalid(
                "annotation node scope exceeds short-label contract",
            ));
        }
    }
    encoded(envelope, MAX_ANNOTATION_BYTES)?;
    Ok(())
}
fn artifact_id(attempt: &AttemptRef, purpose: &str, meter: &mut Meter) -> Result<RecordId> {
    let bytes = encoded(&(purpose, attempt), 2048)?;
    RecordId::new(format!(
        "run_event_relations_{}",
        meter.digest(&bytes)?.hex()
    ))
}
/// Pure canonical RunEvent location; callers still authenticate Run/attempt,
/// record identity, immutable bytes and publication authority independently.
pub(crate) fn artifact_path(run: &RecordId, id: &RecordId) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("runs/{run}/events/{id}.md"))
}
fn path_for(attempt: &AttemptRef, id: &RecordId) -> Result<VaultRelativePath> {
    artifact_path(&attempt.run_id, id)
}
fn reference_shape(reference: &DurableOutputRef) -> Result<()> {
    if reference.record.expected_kind != RecordKind::RunEvent
        || reference.path.as_str().len() > MAX_PATH_BYTES
    {
        return Err(invalid(
            "source relation retained reference has wrong kind or oversized path",
        ));
    }
    Ok(())
}
fn fields(
    id: &RecordId,
    attempt: &AttemptRef,
    purpose: &str,
    timestamp: &str,
) -> Result<BTreeMap<String, Value>> {
    if timestamp.is_empty() || timestamp.len() > 128 || timestamp.chars().any(char::is_control) {
        return Err(invalid(
            "source relation event timestamp must be a bounded string",
        ));
    }
    let fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(id)),
        ("wiki_kind".into(), json!("run_event")),
        (
            "title".into(),
            json!("Unassessed source-local relation preparation"),
        ),
        ("wiki_run_id".into(), json!(attempt.run_id)),
        ("wiki_sequence".into(), json!(attempt.number)),
        ("wiki_event_type".into(), json!(purpose)),
        ("wiki_occurred_at".into(), json!(timestamp)),
    ]);
    CanonicalRecord::new(fields.clone())?;
    Ok(fields)
}
fn render<T: Serialize>(
    artifact: &T,
    id: &RecordId,
    attempt: &AttemptRef,
    purpose: &str,
    timestamp: &str,
    info: &str,
) -> Result<Vec<u8>> {
    let mut bytes = b"---\n".to_vec();
    for (key, value) in fields(id, attempt, purpose, timestamp)? {
        let value = serde_json::to_string(&value).map_err(|e| invalid(e.to_string()))?;
        bytes.extend_from_slice(format!("{key}: {value}\n").as_bytes());
    }
    bytes.extend_from_slice(b"---\n");
    bytes.extend(fence(artifact, info)?);
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(budget(
            "source relation complete Markdown ceiling exceeds 2 MiB",
        ));
    }
    Ok(bytes)
}
fn write<T: Serialize>(
    vault: &RecordId,
    artifact: &T,
    attempt: &AttemptRef,
    purpose: &str,
    timestamp: &str,
    info: &str,
    meter: &mut Meter,
) -> Result<(DurableOutputRef, ExpectedWrite)> {
    let id = artifact_id(attempt, purpose, meter)?;
    let path = path_for(attempt, &id)?;
    meter.check()?;
    let bytes = render(artifact, &id, attempt, purpose, timestamp, info)?;
    let hash = meter.digest(&bytes)?;
    Ok((
        DurableOutputRef {
            record: RecordRef {
                vault_id: vault.clone(),
                record_id: id,
                expected_kind: RecordKind::RunEvent,
            },
            path: path.clone(),
            hash,
        },
        ExpectedWrite {
            target: path,
            expected: ExpectedState::Absent,
            proposed: Some(bytes),
            apply_after: vec![],
        },
    ))
}

/// Returned Absent precondition makes conflicting immutable writes fail in the
/// caller's ChangeEngine. No descriptor cache is consulted or created here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_raw(
    vault: &RecordId,
    task: &TaskSpec,
    source: &SourceBinding,
    identity: &PreparationIdentity,
    attempt: &AttemptRef,
    descriptor: &[u8],
    response: &[u8],
    timestamp: &str,
    limits: StoreLimits,
) -> Result<StoredArtifact> {
    let ((reference, write), hash_bytes, hashes) = measured(limits, |meter| {
        task_shape(task, identity, attempt, meter)?;
        descriptor_shape(descriptor)?;
        if response.is_empty() || response.len() > MAX_RESPONSE_BYTES {
            return Err(invalid(
                "source relation raw response exceeds 256 KiB or is empty",
            ));
        }
        if descriptor.len() as u64 != task.input.byte_len {
            return Err(invalid(
                "source relation exact descriptor length differs from task",
            ));
        }
        let descriptor_hash = meter.digest(descriptor)?;
        if descriptor_hash != task.input.hash {
            return Err(invalid(
                "source relation exact descriptor hash differs from task",
            ));
        }
        let artifact = RawGeneration {
            schema: RAW_ARTIFACT_SCHEMA.into(),
            vault_id: vault.clone(),
            task: task.clone(),
            source: source.clone(),
            preparation: identity.clone(),
            attempt: attempt.clone(),
            descriptor: std::str::from_utf8(descriptor)
                .map_err(|_| invalid("descriptor is not UTF-8"))?
                .into(),
            descriptor_hash,
            response: std::str::from_utf8(response)
                .map_err(|_| invalid("response is not UTF-8"))?
                .into(),
            response_hash: meter.digest(response)?,
        };
        write(
            vault,
            &artifact,
            attempt,
            RAW_ARTIFACT_SCHEMA,
            timestamp,
            RAW_FENCE,
            meter,
        )
    })?;
    Ok(StoredArtifact {
        reference,
        write,
        hash_bytes,
        hashes,
    })
}

fn read_note(bytes: &[u8], expected: &DurableOutputRef, meter: &mut Meter) -> Result<ParsedNote> {
    reference_shape(expected)?;
    if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(invalid(
            "source relation retained Markdown exceeds 2 MiB or is empty",
        ));
    }
    // parse_note always hashes its bytes. Reserve that work before calling it,
    // and reuse its digest rather than hashing the same artifact twice.
    meter.claim(bytes.len())?;
    let note = parse_note(bytes);
    meter.check()?;
    if note.source_hash != expected.hash {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "retained source relation artifact changed",
        ));
    }
    let record = note
        .canonical
        .as_ref()
        .ok_or_else(|| invalid("source relation canonical RunEvent missing"))?;
    if record.id() != &expected.record.record_id || record.kind() != RecordKind::RunEvent {
        return Err(invalid(
            "source relation record identity differs from retained reference",
        ));
    }
    Ok(note)
}
fn decode_body<T: DeserializeOwned>(note: &ParsedNote, info: &str) -> Result<T> {
    let bytes = crate::graph::packet::fenced_json(note, info, MAX_ARTIFACT_BYTES)?;
    strict_json(bytes).map_err(|e| invalid(e.message))
}
fn verify_record<T: Serialize>(
    note: &ParsedNote,
    artifact: &T,
    reference: &DurableOutputRef,
    attempt: &AttemptRef,
    purpose: &str,
    info: &str,
    meter: &mut Meter,
) -> Result<()> {
    let id = artifact_id(attempt, purpose, meter)?;
    if id != reference.record.record_id || path_for(attempt, &id)? != reference.path {
        return Err(invalid(
            "source relation retained attempt/run/path binding differs",
        ));
    }
    let timestamp = note
        .fields
        .as_ref()
        .and_then(|fields| fields.get("wiki_occurred_at"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("source relation event timestamp missing"))?;
    meter.check()?;
    // Exact canonical Markdown rejects unknown frontmatter, additional fences,
    // prose or trailing content, as well as noncanonical envelope/body edits.
    if render(artifact, &id, attempt, purpose, timestamp, info)? != note.raw {
        return Err(invalid(
            "source relation retained Markdown is not its exact canonical record",
        ));
    }
    meter.check()
}

/// Expected task and recipe are authenticated by the caller; matching hashes in
/// untrusted bytes do not derive service authority. No SourceView or I/O.
pub(crate) fn decode_raw(
    bytes: &[u8],
    expected: &DurableOutputRef,
    expected_task: &TaskSpec,
    expected_source: &SourceBinding,
    expected_identity: &PreparationIdentity,
    limits: StoreLimits,
) -> Result<LoadedArtifact<RawGeneration>> {
    let (artifact, hash_bytes, hashes) = measured(limits, |meter| {
        let note = read_note(bytes, expected, meter)?;
        let artifact: RawGeneration = decode_body(&note, RAW_FENCE)?;
        if artifact.schema != RAW_ARTIFACT_SCHEMA
            || artifact.vault_id != expected.record.vault_id
            || &artifact.task != expected_task
            || &artifact.source != expected_source
            || &artifact.preparation != expected_identity
        {
            return Err(invalid(
                "source relation raw vault/task/source/recipe binding differs",
            ));
        }
        task_shape(
            &artifact.task,
            &artifact.preparation,
            &artifact.attempt,
            meter,
        )?;
        descriptor_shape(artifact.descriptor.as_bytes())?;
        if artifact.descriptor.len() as u64 != artifact.task.input.byte_len
            || artifact.descriptor_hash != artifact.task.input.hash
            || artifact.response.is_empty()
            || artifact.response.len() > MAX_RESPONSE_BYTES
        {
            return Err(invalid(
                "source relation retained descriptor/response bound differs",
            ));
        }
        if meter.digest(artifact.descriptor.as_bytes())? != artifact.descriptor_hash
            || meter.digest(artifact.response.as_bytes())? != artifact.response_hash
        {
            return Err(invalid(
                "source relation retained descriptor or response hash differs",
            ));
        }
        verify_record(
            &note,
            &artifact,
            expected,
            &artifact.attempt,
            RAW_ARTIFACT_SCHEMA,
            RAW_FENCE,
            meter,
        )?;
        Ok(artifact)
    })?;
    Ok(LoadedArtifact {
        artifact,
        hash_bytes,
        hashes,
    })
}

pub(crate) fn create_annotation(
    envelope: &Envelope,
    raw: &DurableOutputRef,
    task: &TaskSpec,
    attempt: &AttemptRef,
    timestamp: &str,
    limits: StoreLimits,
) -> Result<StoredArtifact> {
    let ((reference, write), hash_bytes, hashes) = measured(limits, |meter| {
        reference_shape(raw)?;
        task_shape(
            task,
            &PreparationIdentity::from_binding(&envelope.preparation),
            attempt,
            meter,
        )?;
        envelope_shape(envelope, raw)?;
        // Raw and annotation share the exact AttemptRef, with distinct purposes.
        let raw_id = artifact_id(attempt, RAW_ARTIFACT_SCHEMA, meter)?;
        if raw.record.record_id != raw_id || raw.path != path_for(attempt, &raw_id)? {
            return Err(invalid(
                "annotation raw reference belongs to another attempt/run",
            ));
        }
        let artifact = Annotation {
            schema: ANNOTATION_ARTIFACT_SCHEMA.into(),
            task: task.clone(),
            attempt: attempt.clone(),
            raw: raw.clone(),
            envelope: envelope.clone(),
        };
        write(
            &raw.record.vault_id,
            &artifact,
            attempt,
            ANNOTATION_ARTIFACT_SCHEMA,
            timestamp,
            ANNOTATION_FENCE,
            meter,
        )
    })?;
    Ok(StoredArtifact {
        reference,
        write,
        hash_bytes,
        hashes,
    })
}

/// This authenticates retention, not node/edge evidence. After loading the raw
/// artifact with caller authority, compare both tasks/sources/identities, then
/// run source_relations::parse once with the authenticated original and the
/// same operation's remaining deadline/hash budget before using the envelope.
pub(crate) fn decode_annotation(
    bytes: &[u8],
    expected: &DurableOutputRef,
    expected_raw: &DurableOutputRef,
    limits: StoreLimits,
) -> Result<LoadedArtifact<Annotation>> {
    let (artifact, hash_bytes, hashes) = measured(limits, |meter| {
        reference_shape(expected_raw)?;
        let note = read_note(bytes, expected, meter)?;
        let artifact: Annotation = decode_body(&note, ANNOTATION_FENCE)?;
        if artifact.schema != ANNOTATION_ARTIFACT_SCHEMA
            || &artifact.raw != expected_raw
            || expected.record.vault_id != expected_raw.record.vault_id
        {
            return Err(invalid(
                "source relation annotation raw/vault binding differs",
            ));
        }
        task_shape(
            &artifact.task,
            &PreparationIdentity::from_binding(&artifact.envelope.preparation),
            &artifact.attempt,
            meter,
        )?;
        envelope_shape(&artifact.envelope, expected_raw)?;
        let raw_id = artifact_id(&artifact.attempt, RAW_ARTIFACT_SCHEMA, meter)?;
        if expected_raw.record.record_id != raw_id
            || expected_raw.path != path_for(&artifact.attempt, &raw_id)?
        {
            return Err(invalid(
                "annotation names an unrelated raw generation attempt/run",
            ));
        }
        verify_record(
            &note,
            &artifact,
            expected,
            &artifact.attempt,
            ANNOTATION_ARTIFACT_SCHEMA,
            ANNOTATION_FENCE,
            meter,
        )?;
        Ok(artifact)
    })?;
    Ok(LoadedArtifact {
        artifact,
        hash_bytes,
        hashes,
    })
}

#[cfg(test)]
#[path = "source_relation_store_tests.rs"]
mod tests;
