//! Fictional, provider-free fixtures. Authenticated task fingerprints are
//! supplied by this privileged test fixture, never derived from retained input.
use super::*;
use crate::retrieval::{source_relation_packet as packet, source_relations};
use std::time::Duration;

const TIME: &str = "2026-10-10T12:00:00Z";
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn hash(value: &str) -> Blake3Hash {
    Blake3Hash::digest(value)
}
fn limits() -> StoreLimits {
    StoreLimits {
        max_hash_bytes: 8 * 1024 * 1024,
        deadline: Instant::now() + Duration::from_secs(10),
    }
}
struct Fixture {
    text: String,
    source: SourceBinding,
    vault: RecordId,
    task: TaskSpec,
    identity: PreparationIdentity,
    attempt: AttemptRef,
    descriptor: Vec<u8>,
    response: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let text = "A fictional permit requires written notice.\n".to_owned();
        let source = SourceBinding {
            source_id: id("fictional_source"),
            source_revision: id("fictional_revision"),
            raw_hash: hash(&text),
        };
        let packet = packet::build(
            AdmittedOriginal {
                binding: &source,
                text: &text,
            },
            2048,
            limits().deadline,
        )
        .unwrap();
        // Pretty JSON proves exact descriptor retention, including whitespace.
        let descriptor = serde_json::to_vec_pretty(packet.remote_input()).unwrap();
        let identity = PreparationIdentity {
            recipe_id: id(packet::RECIPE),
            recipe_hash: hash(packet::RECIPE),
            prompt_hash: hash("authenticated-mock-prompt"),
            schema_hash: hash("authenticated-mock-schema"),
            model_identity_hash: hash("authenticated-mock-model"),
            settings_hash: hash("authenticated-mock-settings"),
        };
        let mut task = TaskSpec {
            key: hash("temporary"),
            stage: TaskStage::PrepareSourceRelations,
            capability: Some(Capability::Generate),
            priority: 0,
            dependencies: vec![],
            input_hash: hash("authenticated-mock-input"),
            prompt_hash: Some(identity.prompt_hash.clone()),
            schema_hash: Some(identity.schema_hash.clone()),
            model_hash: Some(identity.model_identity_hash.clone()),
            settings_hash: identity.settings_hash.clone(),
            source_bindings: vec![ReadDependency {
                path: VaultRelativePath::new("sources/fictional_source/source.md").unwrap(),
                expected: ExpectedState::Hash(hash("authenticated-source-envelope")),
            }],
            input: crate::jobs::BoundedPayloadRef {
                path: VaultRelativePath::new(".wiki/cache/generation/inputs/fixture.json").unwrap(),
                hash: Blake3Hash::digest(&descriptor),
                byte_len: descriptor.len() as u64,
            },
        };
        task.key = crate::jobs::tasks::task_key(&task).unwrap();
        let attempt = AttemptRef {
            run_id: id("fictional_run"),
            task_key: task.key.clone(),
            attempt_id: id("fictional_attempt"),
            number: 1,
            request_hash: hash("authenticated-wire-request"),
        };
        let response = serde_json::to_vec(&json!({"schema":packet::RAW_SCHEMA,"nodes":[{"id":"permit","kind":"rule","scope":"fictional permit written notice","span_id":"span_000000"}],"edges":[]})).unwrap();
        Self {
            text,
            source,
            vault: id("fictional_vault"),
            task,
            identity,
            attempt,
            descriptor,
            response,
        }
    }
    fn raw(&self) -> StoredArtifact {
        create_raw(
            &self.vault,
            &self.task,
            &self.source,
            &self.identity,
            &self.attempt,
            &self.descriptor,
            &self.response,
            TIME,
            limits(),
        )
        .unwrap()
    }
    fn load_raw(
        &self,
        bytes: &[u8],
        reference: &DurableOutputRef,
    ) -> Result<LoadedArtifact<RawGeneration>> {
        decode_raw(
            bytes,
            reference,
            &self.task,
            &self.source,
            &self.identity,
            limits(),
        )
    }
    fn envelope(&self, reference: &DurableOutputRef) -> Envelope {
        let packet = packet::build(
            AdmittedOriginal {
                binding: &self.source,
                text: &self.text,
            },
            2048,
            limits().deadline,
        )
        .unwrap();
        packet
            .assemble(
                &self.response,
                &self.identity.bind(reference),
                packet::AssemblyLimits {
                    max_hash_bytes: 1024,
                    deadline: limits().deadline,
                },
            )
            .unwrap()
            .envelope
    }
    fn annotation(&self, raw: &DurableOutputRef) -> StoredArtifact {
        create_annotation(
            &self.envelope(raw),
            raw,
            &self.task,
            &self.attempt,
            TIME,
            limits(),
        )
        .unwrap()
    }
}
fn bytes(stored: &StoredArtifact) -> &[u8] {
    stored.write.proposed.as_deref().unwrap()
}
fn rewrite_json(
    stored: &StoredArtifact,
    info: &str,
    change: impl FnOnce(&str) -> String,
) -> (Vec<u8>, DurableOutputRef) {
    let text = std::str::from_utf8(bytes(stored)).unwrap();
    let marker = format!("```{info}\n");
    let start = text.find(&marker).unwrap() + marker.len();
    let end = text.rfind("\n```\n").unwrap();
    let result = format!(
        "{}{}{}",
        &text[..start],
        change(&text[start..end]),
        &text[end..]
    )
    .into_bytes();
    let mut reference = stored.reference.clone();
    reference.hash = Blake3Hash::digest(&result);
    (result, reference)
}

#[test]
fn both_retained_records_roundtrip_without_descriptor_cache_and_revalidate_original() {
    let f = Fixture::new();
    let raw = f.raw();
    let loaded = f.load_raw(bytes(&raw), &raw.reference).unwrap();
    assert_eq!(loaded.artifact.descriptor.as_bytes(), f.descriptor);
    assert_eq!(loaded.artifact.response.as_bytes(), f.response);
    assert_eq!(loaded.artifact.task, f.task);
    assert_eq!(loaded.artifact.preparation, f.identity);
    assert_eq!(loaded.hashes, 5);
    assert_eq!(raw.hashes, 5);
    assert!(loaded.hash_bytes > bytes(&raw).len() as u64);
    let restored: RemoteInput = strict_json(loaded.artifact.descriptor.as_bytes()).unwrap();
    let rebuilt = packet::build(
        AdmittedOriginal {
            binding: &f.source,
            text: &f.text,
        },
        2048,
        limits().deadline,
    )
    .unwrap();
    assert!(restored == *rebuilt.remote_input());
    let annotation = f.annotation(&raw.reference);
    for reference in [&raw.reference, &annotation.reference] {
        assert_eq!(
            reference.path.as_str(),
            format!(
                "runs/{}/events/{}.md",
                f.attempt.run_id, reference.record.record_id
            )
        );
    }
    assert_ne!(
        raw.reference.record.record_id,
        annotation.reference.record.record_id
    );
    let loaded = decode_annotation(
        bytes(&annotation),
        &annotation.reference,
        &raw.reference,
        limits(),
    )
    .unwrap();
    assert_eq!(loaded.artifact.task, f.task);
    assert_eq!(loaded.artifact.raw, raw.reference);
    assert_eq!(loaded.hashes, 4);
    assert_eq!(annotation.hashes, 4);
    source_relations::parse(
        &encoded(&loaded.artifact.envelope, MAX_ANNOTATION_BYTES).unwrap(),
        AdmittedOriginal {
            binding: &f.source,
            text: &f.text,
        },
        &f.identity.bind(&raw.reference),
        ValidationLimits {
            max_hash_bytes: 1024,
            deadline: limits().deadline,
        },
    )
    .unwrap();
    assert_eq!(raw.write.target, raw.reference.path);
    assert_eq!(raw.write.expected, ExpectedState::Absent);
    assert!(raw.write.apply_after.is_empty());
}

#[test]
fn raw_rejects_wrong_vault_run_path_kind_task_source_and_recipe() {
    let f = Fixture::new();
    let raw = f.raw();
    let mut wrong = raw.reference.clone();
    wrong.record.vault_id = id("other_vault");
    assert!(f.load_raw(bytes(&raw), &wrong).is_err());
    wrong = raw.reference.clone();
    wrong.path = VaultRelativePath::new(format!(
        "runs/other_run/events/{}.md",
        wrong.record.record_id
    ))
    .unwrap();
    assert!(f.load_raw(bytes(&raw), &wrong).is_err());
    wrong = raw.reference.clone();
    wrong.record.expected_kind = RecordKind::Source;
    assert!(f.load_raw(bytes(&raw), &wrong).is_err());
    let mut task = f.task.clone();
    task.input_hash = hash("wrong-authenticated-input");
    assert!(
        decode_raw(
            bytes(&raw),
            &raw.reference,
            &task,
            &f.source,
            &f.identity,
            limits()
        )
        .is_err()
    );
    let mut source = f.source.clone();
    source.source_revision = id("other_revision");
    assert!(
        decode_raw(
            bytes(&raw),
            &raw.reference,
            &f.task,
            &source,
            &f.identity,
            limits()
        )
        .is_err()
    );
    source = f.source.clone();
    source.raw_hash = hash("different-original");
    assert!(
        decode_raw(
            bytes(&raw),
            &raw.reference,
            &f.task,
            &source,
            &f.identity,
            limits()
        )
        .is_err()
    );
    let mut identity = f.identity.clone();
    identity.recipe_hash = hash("different-recipe");
    assert!(
        decode_raw(
            bytes(&raw),
            &raw.reference,
            &f.task,
            &f.source,
            &identity,
            limits()
        )
        .is_err()
    );
}

#[test]
fn task_structural_key_fingerprints_dependencies_and_descriptor_are_bound() {
    let mut f = Fixture::new();
    f.task.prompt_hash = Some(hash("wrong-prompt"));
    assert!(
        create_raw(
            &f.vault,
            &f.task,
            &f.source,
            &f.identity,
            &f.attempt,
            &f.descriptor,
            &f.response,
            TIME,
            limits()
        )
        .is_err()
    );
    f = Fixture::new();
    f.task.input_hash = hash("changed-without-key-update");
    assert!(
        create_raw(
            &f.vault,
            &f.task,
            &f.source,
            &f.identity,
            &f.attempt,
            &f.descriptor,
            &f.response,
            TIME,
            limits()
        )
        .is_err()
    );
    f = Fixture::new();
    f.task
        .source_bindings
        .push(f.task.source_bindings[0].clone());
    assert!(
        create_raw(
            &f.vault,
            &f.task,
            &f.source,
            &f.identity,
            &f.attempt,
            &f.descriptor,
            &f.response,
            TIME,
            limits()
        )
        .is_err()
    );
    f = Fixture::new();
    f.descriptor.push(b' '); // Valid JSON, different exact bytes and length.
    assert!(
        create_raw(
            &f.vault,
            &f.task,
            &f.source,
            &f.identity,
            &f.attempt,
            &f.descriptor,
            &f.response,
            TIME,
            limits()
        )
        .is_err()
    );
    f = Fixture::new();
    let mut wrong = f.descriptor.clone();
    let index = wrong.iter().position(|&b| b == b'2').unwrap();
    wrong[index] = b'3'; // Same length; descriptor hash remains old.
    assert!(
        create_raw(
            &f.vault,
            &f.task,
            &f.source,
            &f.identity,
            &f.attempt,
            &wrong,
            &f.response,
            TIME,
            limits()
        )
        .is_err()
    );
}

#[test]
fn corrupt_conflicting_truncated_duplicate_unknown_and_trailing_artifacts_fail() {
    let f = Fixture::new();
    let raw = f.raw();
    let mut corrupt = bytes(&raw).to_vec();
    *corrupt.last_mut().unwrap() = b'!';
    assert_eq!(
        f.load_raw(&corrupt, &raw.reference).err().unwrap().code,
        ErrorCode::ContentConflict
    );
    assert!(
        f.load_raw(&bytes(&raw)[..bytes(&raw).len() - 9], &raw.reference)
            .is_err()
    );
    let (duplicate, reference) = rewrite_json(&raw, RAW_FENCE, |json| {
        format!("{{\"schema\":\"duplicate\",{}", &json[1..])
    });
    assert!(f.load_raw(&duplicate, &reference).is_err());
    let (unknown, reference) = rewrite_json(&raw, RAW_FENCE, |json| {
        format!("{{\"unknown\":true,{}", &json[1..])
    });
    assert!(f.load_raw(&unknown, &reference).is_err());
    let (trailing_json, reference) = rewrite_json(&raw, RAW_FENCE, |json| format!("{json} {{}}"));
    assert!(f.load_raw(&trailing_json, &reference).is_err());
    let mut trailing = bytes(&raw).to_vec();
    trailing.extend_from_slice(b"\nextra prose\n");
    let mut reference = raw.reference.clone();
    reference.hash = Blake3Hash::digest(&trailing);
    assert!(f.load_raw(&trailing, &reference).is_err());
    let conflict = create_raw(
        &f.vault,
        &f.task,
        &f.source,
        &f.identity,
        &f.attempt,
        &f.descriptor,
        b"a different retained response",
        TIME,
        limits(),
    )
    .unwrap();
    assert_eq!(raw.reference.path, conflict.reference.path);
    assert_ne!(raw.reference.hash, conflict.reference.hash);
    assert_eq!(conflict.write.expected, ExpectedState::Absent);
    assert_eq!(
        f.load_raw(bytes(&conflict), &raw.reference)
            .err()
            .unwrap()
            .code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn recomputed_artifact_hash_cannot_hide_corrupt_inner_response_or_descriptor() {
    let f = Fixture::new();
    let raw = f.raw();
    for field in ["response_hash", "descriptor_hash"] {
        let (bytes, reference) = rewrite_json(&raw, RAW_FENCE, |text| {
            let mut value: Value = serde_json::from_str(text).unwrap();
            value[field] = json!(hash("corrupt-inner-hash"));
            serde_json::to_string(&value).unwrap()
        });
        assert!(f.load_raw(&bytes, &reference).is_err());
    }
}

#[test]
fn annotations_reject_unbound_raw_wrong_run_duplicate_unknown_and_truncation() {
    let f = Fixture::new();
    let raw = f.raw();
    let annotation = f.annotation(&raw.reference);
    let mut wrong_raw = raw.reference.clone();
    wrong_raw.hash = hash("wrong-raw-artifact");
    assert!(
        decode_annotation(
            bytes(&annotation),
            &annotation.reference,
            &wrong_raw,
            limits()
        )
        .is_err()
    );
    let mut envelope = f.envelope(&raw.reference);
    envelope.preparation.retained_generation_hash = hash("unbound");
    assert!(
        create_annotation(
            &envelope,
            &raw.reference,
            &f.task,
            &f.attempt,
            TIME,
            limits()
        )
        .is_err()
    );
    let mut attempt = f.attempt.clone();
    attempt.run_id = id("different_run");
    assert!(
        create_annotation(
            &f.envelope(&raw.reference),
            &raw.reference,
            &f.task,
            &attempt,
            TIME,
            limits()
        )
        .is_err()
    );
    let (duplicate, reference) = rewrite_json(&annotation, ANNOTATION_FENCE, |text| {
        format!("{{\"schema\":\"duplicate\",{}", &text[1..])
    });
    assert!(decode_annotation(&duplicate, &reference, &raw.reference, limits()).is_err());
    let (unknown, reference) = rewrite_json(&annotation, ANNOTATION_FENCE, |text| {
        let mut value: Value = serde_json::from_str(text).unwrap();
        value["envelope"]["preparation"]["trusted"] = json!(true);
        serde_json::to_string(&value).unwrap()
    });
    assert!(decode_annotation(&unknown, &reference, &raw.reference, limits()).is_err());
    assert!(
        decode_annotation(
            &bytes(&annotation)[..bytes(&annotation).len() - 20],
            &annotation.reference,
            &raw.reference,
            limits()
        )
        .is_err()
    );
}

#[test]
fn descriptor_typed_decoder_rejects_duplicate_nested_unknown_trailing_and_non_generation() {
    let f = Fixture::new();
    let descriptor = std::str::from_utf8(&f.descriptor).unwrap();
    assert!(descriptor_shape(format!("{{\"version\":1,{}", &descriptor[1..]).as_bytes()).is_err());
    assert!(descriptor_shape(format!("{descriptor} {{}}").as_bytes()).is_err());
    let mut value: Value = serde_json::from_slice(&f.descriptor).unwrap();
    value["operation"]["unknown"] = json!(true);
    assert!(descriptor_shape(&serde_json::to_vec(&value).unwrap()).is_err());
    let duplicate_schema = descriptor.replacen(
        "\"type\": \"object\"",
        "\"type\": \"object\", \"type\": \"object\"",
        1,
    );
    assert_ne!(duplicate_schema, descriptor);
    assert!(descriptor_shape(duplicate_schema.as_bytes()).is_err());
    value = serde_json::from_slice(&f.descriptor).unwrap();
    value["operation"]["operation"] = json!("embed");
    assert!(descriptor_shape(&serde_json::to_vec(&value).unwrap()).is_err());
    value = serde_json::from_slice(&f.descriptor).unwrap();
    value["operation"]["max_output_tokens"] = json!(0);
    assert!(descriptor_shape(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn finite_byte_deadline_and_hash_allowances_fail_with_consumed_work() {
    let f = Fixture::new();
    let raw = f.raw();
    let expired = StoreLimits {
        max_hash_bytes: u64::MAX,
        deadline: Instant::now(),
    };
    let error = decode_raw(
        bytes(&raw),
        &raw.reference,
        &f.task,
        &f.source,
        &f.identity,
        expired,
    )
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(
        error.details["source_relation_store_hash_bytes_consumed"],
        0
    );
    let zero = StoreLimits {
        max_hash_bytes: 0,
        deadline: limits().deadline,
    };
    let error = decode_raw(
        bytes(&raw),
        &raw.reference,
        &f.task,
        &f.source,
        &f.identity,
        zero,
    )
    .err()
    .unwrap();
    assert_eq!(error.details["source_relation_store_hashes_consumed"], 0);
    let note_only = StoreLimits {
        max_hash_bytes: bytes(&raw).len() as u64,
        deadline: limits().deadline,
    };
    let error = decode_raw(
        bytes(&raw),
        &raw.reference,
        &f.task,
        &f.source,
        &f.identity,
        note_only,
    )
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(
        error.details["source_relation_store_hash_bytes_consumed"],
        bytes(&raw).len() as u64
    );
    assert_eq!(error.details["source_relation_store_hashes_consumed"], 1);
    assert!(
        create_raw(
            &f.vault,
            &f.task,
            &f.source,
            &f.identity,
            &f.attempt,
            &f.descriptor,
            &vec![b'x'; MAX_RESPONSE_BYTES + 1],
            TIME,
            limits()
        )
        .is_err()
    );
    assert!(
        decode_raw(
            &vec![b'x'; MAX_ARTIFACT_BYTES + 1],
            &raw.reference,
            &f.task,
            &f.source,
            &f.identity,
            limits()
        )
        .is_err()
    );
    assert!(encoded(&"\0".repeat(MAX_ARTIFACT_BYTES / 6 + 1), MAX_ARTIFACT_BYTES).is_err());
}

#[test]
fn same_run_outputs_namespace_is_rejected_for_fresh_and_retained_artifacts() {
    let f = Fixture::new();
    let raw = f.raw();
    let annotation = f.annotation(&raw.reference);
    let mut wrong_raw = raw.reference.clone();
    wrong_raw.path = VaultRelativePath::new(format!(
        "runs/{}/outputs/{}.md",
        f.attempt.run_id, wrong_raw.record.record_id
    ))
    .unwrap();
    assert!(f.load_raw(bytes(&raw), &wrong_raw).is_err());
    assert!(
        create_annotation(
            &f.envelope(&wrong_raw),
            &wrong_raw,
            &f.task,
            &f.attempt,
            TIME,
            limits(),
        )
        .is_err()
    );
    let mut wrong_annotation = annotation.reference.clone();
    wrong_annotation.path = VaultRelativePath::new(format!(
        "runs/{}/outputs/{}.md",
        f.attempt.run_id, wrong_annotation.record.record_id
    ))
    .unwrap();
    assert!(
        decode_annotation(
            bytes(&annotation),
            &wrong_annotation,
            &raw.reference,
            limits(),
        )
        .is_err()
    );
    assert!(
        decode_annotation(
            bytes(&annotation),
            &annotation.reference,
            &wrong_raw,
            limits(),
        )
        .is_err()
    );
}
