//! In-memory projection/join mechanics, not sealed publication or authentication.
use super::*;
use crate::{
    changes::ReadDependency,
    jobs::BoundedPayloadRef,
    retrieval::{
        source_relation_store::{StoreLimits, create_annotation},
        source_relations::{
            AdmittedOriginal, Envelope, Node, NodeKind, SpanBinding, ValidationLimits, parse,
        },
    },
};
use std::time::{Duration, Instant};

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn hash(value: &str) -> Blake3Hash {
    Blake3Hash::digest(value)
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn document(location: VaultRelativePath, text: String) -> DocumentRow {
    DocumentRow {
        path: location,
        hash: hash(&text),
        record_id: None,
        kind: None,
        title: "Fictional fixture".into(),
        aliases: vec![],
        headings: String::new(),
        tags: vec![],
        body: String::new(),
        raw_text: text,
        source_id: None,
        owner_revision: None,
        eligibility: Eligibility::Current,
        reasons: vec![],
    }
}
fn database(layout: bool) -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(super::super::normalized_schema::SCHEMA)
        .unwrap();
    if layout {
        c.execute_batch(SCHEMA).unwrap();
    }
    c
}
fn record(
    c: &Connection,
    id: &RecordId,
    kind: RecordKind,
    path: &VaultRelativePath,
    hash: &Blake3Hash,
    eligibility: &str,
) {
    c.execute("INSERT OR REPLACE INTO records(id,kind,path,hash,eligibility,disputed,row_json) VALUES(?1,?2,?3,?4,?5,0,'{}')",params![id.as_str(),kind.as_str(),path.as_str(),hash.as_str(),eligibility]).unwrap();
}
fn insert_document(c: &Connection, row: &DocumentRow) {
    c.execute("INSERT OR REPLACE INTO documents(path,file_hash,record_id,kind,title,aliases_json,aliases_text,headings,tags_json,tags_text,body,raw_text,source_id,owner_revision,eligibility,reasons_json) VALUES(?1,?2,?3,?4,'Fixture','[]','','','[]','','',?5,?6,?7,?8,'[]')",
        params![row.path.as_str(),row.hash.as_str(),row.record_id.as_ref().map(RecordId::as_str),row.kind.map(RecordKind::as_str),row.raw_text,row.source_id.as_ref().map(RecordId::as_str),row.owner_revision.as_ref().map(RecordId::as_str),if row.eligibility == Eligibility::Current {"current"} else {"unsupported"}]).unwrap();
}
struct Fixture {
    vault: RecordId,
    owner: DocumentRow,
    annotation: DocumentRow,
    candidate: Candidate,
    envelope: Envelope,
    source_path: VaultRelativePath,
    source_hash: Blake3Hash,
    revision_path: VaultRelativePath,
    revision_hash: Blake3Hash,
}
impl Fixture {
    fn new(number: u32) -> Self {
        let vault = id("fictional_vault");
        let mut owner = document(
            path("sources/fictional_source/revisions/fictional_revision/content.md"),
            "Fictional permit needs notice.\n".into(),
        );
        owner.source_id = Some(id("fictional_source"));
        owner.owner_revision = Some(id("fictional_revision"));
        let source = SourceBinding {
            source_id: owner.source_id.clone().unwrap(),
            source_revision: owner.owner_revision.clone().unwrap(),
            raw_hash: owner.hash.clone(),
        };
        let source_path = path("sources/fictional_source/source.md");
        let source_hash = hash("source envelope");
        let revision_path =
            path("sources/fictional_source/revisions/fictional_revision/revision.md");
        let revision_hash = hash("revision envelope");
        let preparation = PreparationIdentity {
            recipe_id: id("fictional_recipe"),
            recipe_hash: hash("recipe"),
            prompt_hash: hash("prompt"),
            schema_hash: hash("schema"),
            model_identity_hash: hash("model"),
            settings_hash: hash("settings"),
        };
        let mut task = TaskSpec {
            key: hash("temporary"),
            stage: TaskStage::PrepareSourceRelations,
            capability: Some(Capability::Generate),
            priority: 0,
            dependencies: vec![],
            input_hash: hash("input"),
            prompt_hash: Some(preparation.prompt_hash.clone()),
            schema_hash: Some(preparation.schema_hash.clone()),
            model_hash: Some(preparation.model_identity_hash.clone()),
            settings_hash: preparation.settings_hash.clone(),
            source_bindings: vec![
                ReadDependency {
                    path: source_path.clone(),
                    expected: ExpectedState::Hash(source_hash.clone()),
                },
                ReadDependency {
                    path: revision_path.clone(),
                    expected: ExpectedState::Hash(revision_hash.clone()),
                },
                ReadDependency {
                    path: owner.path.clone(),
                    expected: ExpectedState::Hash(owner.hash.clone()),
                },
                ReadDependency {
                    path: path("WIKI.md"),
                    expected: ExpectedState::Hash(hash("wiki envelope")),
                },
            ],
            input: BoundedPayloadRef {
                path: path(".wiki/cache/generation/inputs/fictional.json"),
                hash: hash("descriptor"),
                byte_len: 7,
            },
        };
        task.key = crate::jobs::tasks::task_key(&task).unwrap();
        let attempt = AttemptRef {
            run_id: id("fictional_run"),
            task_key: task.key.clone(),
            attempt_id: id(&format!("fictional_attempt_{number}")),
            number,
            request_hash: hash("request"),
        };
        let raw_id = artifact_id(&attempt, GENERATION_SCHEMA).unwrap();
        let raw = DurableOutputRef {
            record: RecordRef {
                vault_id: vault.clone(),
                record_id: raw_id.clone(),
                expected_kind: RecordKind::RunEvent,
            },
            path: artifact_path(&attempt, &raw_id).unwrap(),
            hash: hash("untrusted generation hint"),
        };
        let envelope = Envelope {
            schema: RELATION_SCHEMA.into(),
            source: source.clone(),
            preparation: preparation.bind(&raw),
            nodes: vec![Node {
                id: id("fictional_rule"),
                kind: NodeKind::Rule,
                scope: "fictional permit notice".into(),
                evidence: SpanBinding {
                    span: ByteSpan::new(0, owner.raw_text.len() as u64).unwrap(),
                    quote_hash: owner.hash.clone(),
                },
            }],
            edges: vec![],
        };
        let stored = create_annotation(
            &envelope,
            &raw,
            &task,
            &attempt,
            "2026-10-10T12:00:00Z",
            StoreLimits {
                max_hash_bytes: 4 * 1024 * 1024,
                deadline: deadline(),
            },
        )
        .unwrap();
        let mut annotation = document(
            stored.reference.path.clone(),
            String::from_utf8(stored.write.proposed.unwrap()).unwrap(),
        );
        annotation.record_id = Some(stored.reference.record.record_id.clone());
        annotation.kind = Some(RecordKind::RunEvent);
        annotation.eligibility = Eligibility::Unsupported;
        let candidate = Candidate {
            annotation: stored.reference,
            raw,
            source,
            task,
            attempt,
            preparation,
        };
        Self {
            vault,
            owner,
            annotation,
            candidate,
            envelope,
            source_path,
            source_hash,
            revision_path,
            revision_hash,
        }
    }
    fn install(&self, c: &Connection) {
        record(
            c,
            &self.candidate.source.source_id,
            RecordKind::Source,
            &self.source_path,
            &self.source_hash,
            "current",
        );
        record(
            c,
            &self.candidate.source.source_revision,
            RecordKind::Revision,
            &self.revision_path,
            &self.revision_hash,
            "current",
        );
        record(
            c,
            &self.candidate.annotation.record.record_id,
            RecordKind::RunEvent,
            &self.annotation.path,
            &self.annotation.hash,
            "unsupported",
        );
        record(
            c,
            &self.candidate.raw.record.record_id,
            RecordKind::RunEvent,
            &self.candidate.raw.path,
            &self.candidate.raw.hash,
            "unsupported",
        );
        insert_document(c, &self.owner);
        insert_document(c, &self.annotation);
        project_document(c, &self.annotation).unwrap();
    }
    fn query(&self, c: &Connection, limit: usize) -> Result<Vec<Candidate>> {
        candidates(c, &self.owner, &self.vault, limit, |_, _| Ok(()))
    }
}

#[test]
fn full_projection_returns_only_metadata_and_reserves_before_decoding() {
    let c = database(true);
    let f = Fixture::new(1);
    f.install(&c);
    let mut columns = vec![];
    let selected = candidates(&c, &f.owner, &f.vault, 80, |_, count| {
        columns.push(count);
        Ok(())
    })
    .unwrap();
    assert_eq!(selected, vec![f.candidate.clone()]);
    assert_eq!(columns, vec![1, 14]);
    let metadata: String = c
        .query_row("SELECT metadata_json FROM relation_annotations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(!metadata.contains("fictional permit notice"));
    assert!(!metadata.contains("envelope"));
    assert!(!metadata.contains("quote_hash"));
    let error = candidates(&c, &f.owner, &f.vault, 80, |_, count| {
        if count == 14 {
            Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "fixture allowance",
            ))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
}

#[test]
fn source_hash_refresh_withdrawal_revision_and_owner_membership_exclude_old_hints() {
    let c = database(true);
    let f = Fixture::new(1);
    f.install(&c);
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    c.execute(
        "UPDATE records SET hash=?1 WHERE id=?2",
        params![
            hash("changed source").as_str(),
            f.candidate.source.source_id.as_str()
        ],
    )
    .unwrap();
    assert!(f.query(&c, 80).unwrap().is_empty());
    c.execute_batch("ROLLBACK").unwrap();
    assert_eq!(f.query(&c, 80).unwrap().len(), 1);
    c.execute(
        "UPDATE records SET eligibility='historical' WHERE id=?1",
        [f.candidate.source.source_id.as_str()],
    )
    .unwrap();
    assert!(f.query(&c, 80).unwrap().is_empty());
    c.execute(
        "UPDATE records SET eligibility='current' WHERE id=?1",
        [f.candidate.source.source_id.as_str()],
    )
    .unwrap();
    c.execute(
        "UPDATE records SET eligibility='historical' WHERE id=?1",
        [f.candidate.source.source_revision.as_str()],
    )
    .unwrap();
    assert!(f.query(&c, 80).unwrap().is_empty());
    record(
        &c,
        &id("new_revision"),
        RecordKind::Revision,
        &path("sources/fictional_source/revisions/new_revision/revision.md"),
        &hash("new revision"),
        "current",
    );
    c.execute(
        "UPDATE documents SET owner_revision='new_revision' WHERE path=?1",
        [f.owner.path.as_str()],
    )
    .unwrap();
    assert!(f.query(&c, 80).unwrap().is_empty());
    assert_eq!(
        c.query_row("SELECT count(*) FROM relation_annotations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn exact_annotation_raw_record_bindings_and_delete_replacement_are_required() {
    let c = database(true);
    let f = Fixture::new(1);
    f.install(&c);
    c.execute(
        "UPDATE records SET hash=?1 WHERE path=?2",
        params![hash("altered raw").as_str(), f.candidate.raw.path.as_str()],
    )
    .unwrap();
    assert!(f.query(&c, 80).unwrap().is_empty());
    f.install(&c);
    c.execute(
        "UPDATE records SET hash=?1 WHERE path=?2",
        params![
            hash("altered annotation").as_str(),
            f.annotation.path.as_str()
        ],
    )
    .unwrap();
    assert!(f.query(&c, 80).unwrap().is_empty());
    f.install(&c);
    let replacement = document(f.annotation.path.clone(), "ordinary text".into());
    project_document(&c, &replacement).unwrap();
    assert!(f.query(&c, 80).unwrap().is_empty());
    assert_eq!(
        c.query_row("SELECT count(*) FROM relation_dependencies", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    f.install(&c);
    delete_document(&c, &f.annotation.path).unwrap();
    assert!(f.query(&c, 80).unwrap().is_empty());
}

#[test]
fn malformed_or_unrelated_annotation_events_never_create_hints() {
    let c = database(true);
    let f = Fixture::new(1);
    f.install(&c);
    let mut malformed = f.annotation.clone();
    malformed.raw_text = malformed
        .raw_text
        .replace("\"prepare_source_relations\"", "\"extract\"");
    malformed.hash = hash(&malformed.raw_text);
    project_document(&c, &malformed).unwrap();
    assert!(f.query(&c, 80).unwrap().is_empty());
    f.install(&c);
    malformed = f.annotation.clone();
    malformed.raw_text.truncate(malformed.raw_text.len() - 4);
    malformed.hash = hash(&malformed.raw_text);
    project_document(&c, &malformed).unwrap();
    assert!(f.query(&c, 80).unwrap().is_empty());
    let unrelated = document(
        path("runs/fictional_run/events/ordinary.md"),
        "ordinary event".into(),
    );
    project_document(&c, &unrelated).unwrap();
}

#[test]
fn forged_metadata_and_forged_dependency_rows_fail_internal_consistency() {
    let c = database(true);
    let f = Fixture::new(1);
    f.install(&c);
    let mut value = serde_json::to_value(&f.candidate).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("envelope".into(), serde_json::json!({}));
    c.execute(
        "UPDATE relation_annotations SET metadata_json=?1",
        [serde_json::to_string(&value).unwrap()],
    )
    .unwrap();
    assert_eq!(f.query(&c, 80).unwrap_err().code, ErrorCode::IndexCorrupt);
    f.install(&c);
    let changed = hash("changed source envelope");
    c.execute(
        "UPDATE records SET hash=?1 WHERE id=?2",
        params![changed.as_str(), f.candidate.source.source_id.as_str()],
    )
    .unwrap();
    c.execute(
        "UPDATE relation_dependencies SET expected_hash=?1 WHERE path=?2",
        params![changed.as_str(), f.source_path.as_str()],
    )
    .unwrap();
    assert_eq!(f.query(&c, 80).unwrap_err().code, ErrorCode::IndexCorrupt);
}

#[test]
fn metadata_byte_guard_scalar_types_and_record_ids_are_checked() {
    let c = database(true);
    let f = Fixture::new(1);
    f.install(&c);
    assert!(
        c.execute(
            "UPDATE relation_annotations SET metadata_json=zeroblob(65537)",
            []
        )
        .is_err()
    );
    c.execute(
        "UPDATE relation_annotations SET metadata_json=zeroblob(8)",
        [],
    )
    .unwrap();
    assert_eq!(f.query(&c, 80).unwrap_err().code, ErrorCode::IndexCorrupt);
    f.install(&c);
    c.execute(
        "UPDATE records SET id='forged_raw_id' WHERE path=?1",
        [f.candidate.raw.path.as_str()],
    )
    .unwrap();
    assert_eq!(f.query(&c, 80).unwrap_err().code, ErrorCode::IndexCorrupt);
}

#[test]
fn internally_consistent_hint_still_does_not_authenticate_original_evidence() {
    let c = database(true);
    let mut f = Fixture::new(1);
    f.envelope.nodes[0].evidence.quote_hash = hash("forged quote");
    let stored = create_annotation(
        &f.envelope,
        &f.candidate.raw,
        &f.candidate.task,
        &f.candidate.attempt,
        "2026-10-10T12:00:00Z",
        StoreLimits {
            max_hash_bytes: 4 * 1024 * 1024,
            deadline: deadline(),
        },
    )
    .unwrap();
    f.annotation.raw_text = String::from_utf8(stored.write.proposed.unwrap()).unwrap();
    f.annotation.hash = stored.reference.hash.clone();
    f.candidate.annotation = stored.reference;
    f.install(&c);
    assert_eq!(f.query(&c, 80).unwrap(), vec![f.candidate.clone()]);
    assert!(
        parse(
            &serde_json::to_vec(&f.envelope).unwrap(),
            AdmittedOriginal {
                binding: &f.candidate.source,
                text: &f.owner.raw_text
            },
            &f.envelope.preparation,
            ValidationLimits {
                max_hash_bytes: 1024 * 1024,
                deadline: deadline()
            }
        )
        .is_err()
    );
}

#[test]
fn caps_order_old_cache_fallback_and_partial_layout_are_explicit() {
    let c = database(true);
    let first = Fixture::new(1);
    let second = Fixture::new(2);
    first.install(&c);
    second.install(&c);
    let selected = first.query(&c, 80).unwrap();
    assert_eq!(selected.len(), 2);
    assert!(selected[0].annotation.path < selected[1].annotation.path);
    assert_eq!(
        first.query(&c, 1).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert!(first.query(&c, 0).is_err());
    assert!(first.query(&c, 81).is_err());
    let old = database(false);
    first.install(&old);
    assert!(first.query(&old, 80).unwrap().is_empty());
    delete_document(&old, &first.annotation.path).unwrap();
    old.execute_batch("CREATE TABLE relation_annotations(annotation_path TEXT PRIMARY KEY)")
        .unwrap();
    assert_eq!(
        first.query(&old, 80).unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
}

#[test]
fn prepared_storage_is_charged_before_apply_and_retirement_matches_stored_rows() {
    let c = database(true);
    let mut f = Fixture::new(1);
    f.install(&c);
    delete_document(&c, &f.annotation.path).unwrap();
    let prepared = prepare_document(&f.annotation).unwrap();
    assert_eq!(
        prepared.storage_charges().len(),
        1 + f.candidate.task.source_bindings.len()
    );
    assert_eq!(
        prepared.storage_bytes(),
        prepared.storage_charges().iter().sum::<usize>()
    );
    assert!(prepared.storage_charges()[0] > prepared.metadata.len());
    // An enclosing zero-byte admission fails before apply; no rows appeared.
    let admission = prepared
        .storage_charges()
        .iter()
        .try_fold(0usize, |used, charge| {
            used.checked_add(*charge)
                .filter(|bytes| *bytes == 0)
                .ok_or(())
        });
    assert!(admission.is_err());
    assert!(f.query(&c, 80).unwrap().is_empty());
    // Applying uses the immutable prepared value, never reparses input text.
    f.annotation.raw_text = "not an annotation anymore".into();
    apply_document(&c, &prepared).unwrap();
    assert_eq!(f.query(&c, 80).unwrap(), vec![f.candidate.clone()]);
    let mut stats = super::super::normalized_delta::DeltaStats::default();
    charge_retired_document(&c, &f.annotation.path, &mut stats).unwrap();
    assert_eq!(stats.old_rows, prepared.storage_charges().len());
    assert_eq!(stats.old_fact_bytes, prepared.storage_bytes());
    assert_eq!(
        f.query(&c, 80).unwrap().len(),
        1,
        "charging retirement does not mutate rows"
    );
    let empty = prepare_document(&f.annotation).unwrap();
    assert_eq!(empty.storage_bytes(), 0);
    assert!(empty.storage_charges().is_empty());
    apply_document(&c, &empty).unwrap();
    assert!(f.query(&c, 80).unwrap().is_empty());
}

#[test]
fn retirement_uses_existing_cumulative_budget_and_dependency_sentinel() {
    let c = database(true);
    let f = Fixture::new(1);
    f.install(&c);
    let mut stats = super::super::normalized_delta::DeltaStats {
        old_fact_bytes: super::super::normalized_delta::MAX_DELTA_BYTES - 1,
        ..Default::default()
    };
    assert_eq!(
        charge_retired_document(&c, &f.annotation.path, &mut stats)
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(f.query(&c, 80).unwrap().len(), 1);
    for index in 0..MAX_SOURCE_BINDINGS {
        c.execute("INSERT INTO relation_dependencies(annotation_path,path,expected_hash) VALUES(?1,?2,?3)",params![f.annotation.path.as_str(),format!("forged/{index:03}.md"),hash("forged").as_str()]).unwrap();
    }
    let mut stats = super::super::normalized_delta::DeltaStats::default();
    assert_eq!(
        charge_retired_document(&c, &f.annotation.path, &mut stats)
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(
        stats.old_rows,
        1 + MAX_SOURCE_BINDINGS + 1,
        "annotation plus bounded dependency sentinel remain charged"
    );
    let old = database(false);
    let mut stats = super::super::normalized_delta::DeltaStats::default();
    charge_retired_document(&old, &f.annotation.path, &mut stats).unwrap();
    assert_eq!(stats.old_rows, 0);
}

#[test]
fn raw_annotation_binding_names_cited_content_not_distinct_original_binary() {
    let c = database(true);
    let f = Fixture::new(1);
    f.install(&c);
    let original_binary_hash = Blake3Hash::digest([0u8, 255, 17, 0]);
    assert_ne!(original_binary_hash, f.owner.hash);
    c.execute("INSERT INTO source_revision_identity(source_id,revision_id,retained_ordinal,original_hash,content_hash,extractor_fingerprint,extraction_status) VALUES(?1,?2,0,?3,?4,?5,'complete')",
        params![f.candidate.source.source_id.as_str(),f.candidate.source.source_revision.as_str(),original_binary_hash.as_str(),f.owner.hash.as_str(),hash("extractor").as_str()]).unwrap();
    assert_eq!(f.query(&c, 80).unwrap(), vec![f.candidate.clone()]);
    let mut wrong = f.candidate.clone();
    wrong.source.raw_hash = original_binary_hash.clone();
    c.execute(
        "UPDATE relation_annotations SET source_raw_hash=?1,metadata_json=?2",
        params![
            original_binary_hash.as_str(),
            serde_json::to_string(&wrong).unwrap()
        ],
    )
    .unwrap();
    assert!(
        f.query(&c, 80).unwrap().is_empty(),
        "original.bin hash cannot authorize cited content nodes"
    );
}

#[test]
fn canonical_events_hint_projects_and_same_run_outputs_locator_does_not() {
    let c = database(true);
    let f = Fixture::new(1);
    for reference in [&f.candidate.annotation, &f.candidate.raw] {
        assert_eq!(
            reference.path.as_str(),
            format!(
                "runs/{}/events/{}.md",
                f.candidate.attempt.run_id, reference.record.record_id
            )
        );
    }
    let prepared = prepare_document(&f.annotation).unwrap();
    assert!(prepared.storage_bytes() > 0);
    f.install(&c);
    assert_eq!(f.query(&c, 80).unwrap(), vec![f.candidate.clone()]);
    let mut wrong = f.annotation.clone();
    wrong.path = path(&format!(
        "runs/{}/outputs/{}.md",
        f.candidate.attempt.run_id, f.candidate.annotation.record.record_id
    ));
    let rejected = prepare_document(&wrong).unwrap();
    assert_eq!(rejected.storage_bytes(), 0);
    assert!(rejected.storage_charges().is_empty());
}
