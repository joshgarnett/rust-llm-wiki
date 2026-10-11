//! Disposable fictional source/preparation fixtures; no provider or journal.
use super::*;
use crate::{
    catalog::query_types::QueryReadLimits,
    jobs::{AttemptRef, BoundedPayloadRef, Capability, TaskSpec, TaskStage},
    retrieval::{context_types::VerificationBudget, selected_documents::authenticate},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use std::fs;
fn path(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn hash(s: &str) -> Blake3Hash {
    Blake3Hash::digest(s)
}
struct Fixture {
    temp: tempfile::TempDir,
    catalog: Catalog,
    owner: VaultRelativePath,
    annotation: VaultRelativePath,
    raw: VaultRelativePath,
}
impl Fixture {
    fn new(prepared: bool, incomplete: bool, forged: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"),"---\nwiki_schema: '1'\nwiki_id: vault_relations\nwiki_kind: vault\ntitle: Fictional relations\n---\n").unwrap();
        let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let plan = SourceStore::new(handle.clone())
            .plan_capture(CaptureRequest {
                title: "Fictional permit".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "fixture.txt".into(),
                original: b"\x00fictional imported binary\xff".to_vec(),
                extraction: ExtractionInput::Supplied {
                    extractor: "fictional-test-extractor-v1".into(),
                    fingerprint: hash("fictional extraction"),
                    content: b"Fictional permit requires written notice.\n".to_vec(),
                },
                media_type: None,
            })
            .unwrap();
        for op in plan.draft.unwrap().operations {
            let p = temp.path().join(op.target.as_str());
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, op.proposed.unwrap()).unwrap();
        }
        let catalog = Catalog::new(handle, id("vault_relations"));
        let rebuild = || {
            let writer =
                WriterPermit::acquire(catalog.fs().root(), Duration::from_secs(1)).unwrap();
            catalog.rebuild_normalized(&writer).unwrap();
        };
        rebuild();
        let owner = path(&format!(
            "sources/{}/revisions/{}/content.md",
            plan.source_id, plan.revision_id
        ));
        let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
        let proof = authenticate(
            &catalog,
            &reader,
            std::slice::from_ref(&owner),
            &VerificationBudget::default(),
        )
        .unwrap();
        let document = &proof.documents[&owner];
        let binding = relations::SourceBinding {
            source_id: plan.source_id.clone(),
            source_revision: plan.revision_id.clone(),
            raw_hash: document.hash.clone(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let packet = packet::build(
            relations::AdmittedOriginal {
                binding: &binding,
                text: &document.raw_text,
            },
            2048,
            deadline,
        )
        .unwrap();
        let descriptor = crate::graph::packet::canonical_json(packet.remote_input()).unwrap();
        let (instructions, data, schema) = match &packet.remote_input().operation {
            RemoteOperation::Generate {
                instructions,
                data,
                output_schema,
                ..
            } => (instructions, data, output_schema),
            _ => unreachable!(),
        };
        let identity = store::PreparationIdentity {
            recipe_id: id(packet::RECIPE),
            recipe_hash: Blake3Hash::digest(
                crate::graph::packet::canonical_json(&(
                    packet::RECIPE,
                    packet::INPUT_SCHEMA,
                    packet::RAW_SCHEMA,
                    packet::FRAGMENT_BYTES,
                    packet::INSTRUCTIONS,
                    packet::output_schema(),
                ))
                .unwrap(),
            ),
            prompt_hash: Blake3Hash::digest(
                crate::graph::packet::canonical_json(&(instructions, data)).unwrap(),
            ),
            schema_hash: Blake3Hash::digest(crate::graph::packet::canonical_json(schema).unwrap()),
            model_identity_hash: hash("fictional model"),
            settings_hash: hash("fictional settings"),
        };
        let revision = &proof.records[&plan.revision_id];
        let source = &proof.records[&plan.source_id];
        let mut guards: BTreeMap<_, _> = [
            path("WIKI.md"),
            source.path.clone(),
            revision.path.clone(),
            payload(revision, "wiki_original_path").unwrap(),
            owner.clone(),
        ]
        .into_iter()
        .map(|p| {
            let e = proof.states[&p].clone();
            (p, e)
        })
        .collect();
        if incomplete {
            guards.remove(&path("WIKI.md"));
        }
        let mut task = TaskSpec {
            key: hash("temporary"),
            stage: TaskStage::PrepareSourceRelations,
            capability: Some(Capability::Generate),
            priority: 0,
            dependencies: vec![],
            input_hash: Blake3Hash::digest(&descriptor),
            prompt_hash: Some(identity.prompt_hash.clone()),
            schema_hash: Some(identity.schema_hash.clone()),
            model_hash: Some(identity.model_identity_hash.clone()),
            settings_hash: identity.settings_hash.clone(),
            source_bindings: guards
                .into_iter()
                .map(|(path, expected)| ReadDependency { path, expected })
                .collect(),
            input: BoundedPayloadRef {
                path: path(".wiki/cache/generation/inputs/fictional.json"),
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
            request_hash: hash("fictional request"),
        };
        let response=serde_json::to_vec(&serde_json::json!({"schema":packet::RAW_SCHEMA,"nodes":[{"id":"permit","kind":"rule","scope":"fictional permit notice","span_id":"span_000000"}],"edges":[]})).unwrap();
        let limits = store::StoreLimits {
            max_hash_bytes: LOCAL_HASH_BYTES,
            deadline,
        };
        let raw = store::create_raw(
            catalog.vault_id(),
            &task,
            &binding,
            &identity,
            &attempt,
            &descriptor,
            &response,
            "2026-10-10T12:00:00Z",
            limits,
        )
        .unwrap();
        let mut envelope = packet
            .assemble(
                &response,
                &identity.bind(&raw.reference),
                packet::AssemblyLimits {
                    max_hash_bytes: LOCAL_HASH_BYTES,
                    deadline,
                },
            )
            .unwrap()
            .envelope;
        if forged {
            envelope.nodes[0].scope = "forged but syntactically valid".into();
        }
        let annotation = store::create_annotation(
            &envelope,
            &raw.reference,
            &task,
            &attempt,
            "2026-10-10T12:00:00Z",
            limits,
        )
        .unwrap();
        drop(reader);
        if prepared {
            for artifact in [&raw, &annotation] {
                let p = temp.path().join(artifact.reference.path.as_str());
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(p, artifact.write.proposed.as_ref().unwrap()).unwrap();
            }
            rebuild();
        }
        Self {
            temp,
            catalog,
            owner,
            annotation: annotation.reference.path,
            raw: raw.reference.path,
        }
    }
}
#[test]
fn preparation_extends_context_identity_and_final_recheck_only() {
    let f = Fixture::new(true, false, false);
    let reader = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut proof = authenticate(
        &f.catalog,
        &reader,
        std::slice::from_ref(&f.owner),
        &VerificationBudget::default(),
    )
    .unwrap();
    let context = proof.fingerprint.clone();
    let embedding = proof.embedding_dependency_fingerprint.clone();
    let loaded = proof
        .load_source_relations(&f.catalog, &reader, std::slice::from_ref(&f.owner))
        .unwrap();
    assert_eq!(loaded.annotations.len(), 1);
    assert_ne!(proof.fingerprint, context);
    assert_eq!(proof.embedding_dependency_fingerprint, embedding);
    proof.recheck(&f.catalog, &reader).unwrap();
    fs::write(f.temp.path().join(f.raw.as_str()), b"changed").unwrap();
    assert_eq!(
        proof.recheck(&f.catalog, &reader).unwrap_err().code,
        ErrorCode::FreshnessConflict
    );
}
#[test]
fn absent_preparation_preserves_both_fingerprints() {
    let f = Fixture::new(false, false, false);
    let reader = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut proof = authenticate(
        &f.catalog,
        &reader,
        std::slice::from_ref(&f.owner),
        &VerificationBudget::default(),
    )
    .unwrap();
    let before = (
        proof.fingerprint.clone(),
        proof.embedding_dependency_fingerprint.clone(),
    );
    let loaded = proof
        .load_source_relations(&f.catalog, &reader, std::slice::from_ref(&f.owner))
        .unwrap();
    assert!(loaded.annotations.is_empty());
    assert_eq!(
        loaded.omissions[0].reason,
        "source_relation_preparation_unavailable"
    );
    assert_eq!(
        (proof.fingerprint, proof.embedding_dependency_fingerprint),
        before
    );
}
#[test]
fn incomplete_owner_guards_are_stale_without_artifact_reads() {
    let f = Fixture::new(true, true, false);
    let reader = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut proof = authenticate(
        &f.catalog,
        &reader,
        std::slice::from_ref(&f.owner),
        &VerificationBudget::default(),
    )
    .unwrap();
    let work = proof.meter.work();
    let loaded = proof
        .load_source_relations(&f.catalog, &reader, std::slice::from_ref(&f.owner))
        .unwrap();
    assert!(loaded.annotations.is_empty());
    assert_eq!(
        loaded.omissions[0].reason,
        "source_relation_stale_preparation"
    );
    assert_eq!(proof.meter.work(), work);
}
#[test]
fn forged_envelope_fails_closed_even_with_canonical_codec_and_index_hashes() {
    let f = Fixture::new(true, false, true);
    let reader = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut proof = authenticate(
        &f.catalog,
        &reader,
        std::slice::from_ref(&f.owner),
        &VerificationBudget::default(),
    )
    .unwrap();
    assert_eq!(
        proof
            .load_source_relations(&f.catalog, &reader, std::slice::from_ref(&f.owner))
            .err()
            .unwrap()
            .code,
        ErrorCode::ExtractionInvalid
    );
}
#[test]
fn missing_selected_artifact_is_freshness_conflict() {
    let f = Fixture::new(true, false, false);
    let reader = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut proof = authenticate(
        &f.catalog,
        &reader,
        std::slice::from_ref(&f.owner),
        &VerificationBudget::default(),
    )
    .unwrap();
    fs::remove_file(f.temp.path().join(f.annotation.as_str())).unwrap();
    assert_eq!(
        proof
            .load_source_relations(&f.catalog, &reader, std::slice::from_ref(&f.owner))
            .err()
            .unwrap()
            .code,
        ErrorCode::FreshnessConflict
    );
}
#[test]
fn shared_selected_file_budget_is_not_reset_for_artifacts() {
    let f = Fixture::new(true, false, false);
    let reader = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut proof = authenticate(
        &f.catalog,
        &reader,
        std::slice::from_ref(&f.owner),
        &VerificationBudget::default(),
    )
    .unwrap();
    let mut remaining = VerificationBudget::default();
    remaining.max_files = 0;
    proof.meter.tighten_remaining(&remaining).unwrap();
    assert_eq!(
        proof
            .load_source_relations(&f.catalog, &reader, std::slice::from_ref(&f.owner))
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
}
#[test]
fn exact_owner_guard_equality_rejects_cross_owner_and_original_hash_substitution() {
    let expected = BTreeMap::from([
        (path("WIKI.md"), ExpectedState::Hash(hash("wiki"))),
        (
            path("sources/a/content.md"),
            ExpectedState::Hash(hash("content")),
        ),
    ]);
    let mut guards: Vec<_> = expected
        .iter()
        .map(|(path, expected)| ReadDependency {
            path: path.clone(),
            expected: expected.clone(),
        })
        .collect();
    assert!(exact_guards(&guards, &expected));
    guards[1].expected = ExpectedState::Hash(hash("original.bin"));
    assert!(!exact_guards(&guards, &expected));
    guards[1].path = path("sources/b/content.md");
    assert!(!exact_guards(&guards, &expected));
}
