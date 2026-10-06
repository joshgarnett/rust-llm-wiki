//! SQL regression tests of exact delta row mutation; publication is caller-owned.
use super::{normalized_delta::*, normalized_schema, types::*};
use crate::{
    changes::{PreparedChange, ReadDependency, RevisionOwnerRow, RevisionTreeKey},
    domain::{Blake3Hash, Eligibility, ErrorCode, RecordId, RecordKind, VaultRelativePath},
    records::parse_note,
    sources::revision::{common, record_bytes},
    vault::ExpectedState,
};
use rusqlite::Connection;
use serde_json::json;

fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn empty() -> CatalogDelta {
    CatalogDelta {
        facts: None,
        version: 1,
        records: vec![],
        documents: vec![],
        graph: vec![],
        links: vec![],
        diagnostics: vec![],
        claims: vec![],
        revisions: vec![],
        dependencies: vec![],
        owners: vec![],
    }
}
fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(normalized_schema::SCHEMA).unwrap();
    c.execute("INSERT INTO catalog_meta(singleton,schema_version,vault_id,file_id,creation_epoch,creation_header_hash,epoch,state,vector_cache_lost,vector_loss_unknown) VALUES(1,3,'vault_delta','file_delta',1,'test',1,'building',0,0)",[]).unwrap();
    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    c
}

#[test]
fn interrupted_dml_preserves_budget_error_after_sqlite_rolls_back_transaction() {
    let c = db();
    let mut seed = empty();
    seed.documents = vec![DocumentMutation::Put {
        row: document("selected.md", "beforetoken"),
    }];
    seed.apply(&c).unwrap();
    c.execute_batch("COMMIT").unwrap();
    // Force the interruption inside DML, after deletion of the old FTS posting,
    // without relying on the number of VM instructions used by FTS internals.
    c.execute_batch(
        "CREATE TEMP TRIGGER slow_delta BEFORE INSERT ON documents BEGIN
        SELECT sum(x) FROM (WITH RECURSIVE numbers(x) AS
          (SELECT 1 UNION ALL SELECT x+1 FROM numbers WHERE x<100000)
          SELECT x FROM numbers);
        END; BEGIN IMMEDIATE;",
    )
    .unwrap();
    let mut callbacks = 0;
    c.progress_handler(
        100,
        Some(move || {
            callbacks += 1;
            callbacks >= 50
        }),
    )
    .unwrap();
    let mut delta = empty();
    delta.documents = vec![DocumentMutation::Put {
        row: document("selected.md", "aftertoken"),
    }];
    let error = delta.apply(&c).unwrap_err();
    c.progress_handler(0, None::<fn() -> bool>).unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded, "{error:?}");
    assert!(
        c.is_autocommit(),
        "interrupted DML rolls back the outer transaction"
    );
    assert_eq!(
        c.query_row(
            "SELECT body FROM documents WHERE path='selected.md'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "beforetoken"
    );
    assert_eq!(hits(&c, "documents_fts", "beforetoken"), 1);
    assert_eq!(hits(&c, "documents_fts", "aftertoken"), 0);
}

fn document(location: &str, body: &str) -> DocumentRow {
    DocumentRow {
        path: path(location),
        hash: Blake3Hash::digest(body),
        record_id: None,
        kind: None,
        title: "Display".into(),
        aliases: vec!["Alias".into()],
        headings: "Heading".into(),
        tags: vec!["Tag".into()],
        body: body.into(),
        raw_text: body.into(),
        source_id: None,
        owner_revision: None,
        eligibility: Eligibility::Current,
        reasons: vec!["note_text".into()],
    }
}
fn graph(text: &str) -> GraphRow {
    GraphRow {
        target_id: id("entity_delta"),
        target_kind: RecordKind::Entity,
        name: "Delta".into(),
        aliases: vec!["D".into()],
        endpoints: String::new(),
        predicate: String::new(),
        qualifiers: "{}".into(),
        description: text.into(),
    }
}
fn hits(c: &Connection, table: &str, term: &str) -> i64 {
    c.query_row(
        &format!("SELECT count(*) FROM {table} WHERE {table} MATCH ?1"),
        [term],
        |row| row.get(0),
    )
    .unwrap()
}
fn ordinal(c: &Connection, table: &str, key: &str) -> i64 {
    let (column, identity) = if table == "documents" {
        ("doc_row", "path")
    } else {
        ("graph_row", "target_id")
    };
    c.query_row(
        &format!("SELECT {column} FROM {table} WHERE {identity}=?1"),
        [key],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn document_and_graph_replacements_remove_old_terms_keep_rowids_and_unrelated_rows() {
    let c = db();
    let mut seed = empty();
    seed.documents = vec![
        DocumentMutation::Put {
            row: document("changed.md", "oldtoken"),
        },
        DocumentMutation::Put {
            row: document("untouched.md", "untouchedtoken"),
        },
    ];
    seed.graph = vec![graph("oldgraph")];
    seed.apply(&c).unwrap();
    let old_doc = ordinal(&c, "documents", "changed.md");
    let old_graph = ordinal(&c, "graph_rows", "entity_delta");
    let untouched: String = c
        .query_row(
            "SELECT raw_text FROM documents WHERE path='untouched.md'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut update = empty();
    update.documents = vec![DocumentMutation::Put {
        row: document("changed.md", "newtoken"),
    }];
    update.graph = vec![graph("newgraph")];
    // Replayable actions survive a receipt round trip exactly.
    let encoded = serde_json::to_vec(&update).unwrap();
    let decoded: CatalogDelta = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, update);
    let stats = decoded.apply(&c).unwrap();
    assert_eq!(stats.old_rows, 2);
    assert!(stats.old_fts_bytes > 0);
    assert_eq!(ordinal(&c, "documents", "changed.md"), old_doc);
    assert_eq!(ordinal(&c, "graph_rows", "entity_delta"), old_graph);
    assert_eq!(hits(&c, "documents_fts", "oldtoken"), 0);
    assert_eq!(hits(&c, "documents_fts", "newtoken"), 1);
    assert_eq!(hits(&c, "graph_fts", "oldgraph"), 0);
    assert_eq!(hits(&c, "graph_fts", "newgraph"), 1);
    assert_eq!(hits(&c, "documents_fts", "untouchedtoken"), 1);
    assert_eq!(
        c.query_row(
            "SELECT raw_text FROM documents WHERE path='untouched.md'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        untouched
    );
    // FTS external-content integrity is a test oracle, never production delta work.
    c.execute(
        "INSERT INTO documents_fts(documents_fts,rank) VALUES('integrity-check',1)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO graph_fts(graph_fts,rank) VALUES('integrity-check',1)",
        [],
    )
    .unwrap();
}

#[test]
fn metadata_only_changes_neither_text_nor_fts_even_when_text_exceeds_decode_budget() {
    let c = db();
    let mut seed = empty();
    seed.documents = vec![DocumentMutation::Put {
        row: document("old.md", "retainedtoken"),
    }];
    seed.apply(&c).unwrap();
    // An out-of-band large raw text makes any accidental full-row admission fail.
    c.execute(
        "UPDATE documents SET raw_text=?1 WHERE path='old.md'",
        ["x".repeat(9 * 1024 * 1024)],
    )
    .unwrap();
    let old = ordinal(&c, "documents", "old.md");
    let mut delta = empty();
    delta.documents = vec![DocumentMutation::Metadata {
        path: path("old.md"),
        eligibility: Eligibility::Historical,
        reasons: vec!["not_current_head".into()],
    }];
    let stats = delta.apply(&c).unwrap();
    assert_eq!(stats.old_fts_bytes, 0);
    assert_eq!(stats.old_rows, 0);
    assert_eq!(ordinal(&c, "documents", "old.md"), old);
    assert_eq!(hits(&c, "documents_fts", "retainedtoken"), 1);
    let (eligibility, reasons, bytes): (String, String, i64) = c
        .query_row(
            "SELECT eligibility,reasons_json,length(raw_text) FROM documents WHERE path='old.md'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(eligibility, "historical");
    assert_eq!(reasons, "[\"not_current_head\"]");
    assert_eq!(bytes, 9 * 1024 * 1024);
    // Candidate admission reads this covering index after metadata-only deltas.
    // It must observe the new lifecycle without reading the oversized payload.
    let indexed: (String, String) = c
        .query_row(
            "SELECT path,eligibility FROM documents INDEXED BY document_candidate_metadata WHERE doc_row=?1",
            [old],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(indexed, ("old.md".into(), "historical".into()));
}

#[test]
fn late_owner_conflict_rolls_back_ordinary_and_fts_changes_without_ending_caller_transaction() {
    let c = db();
    let mut seed = empty();
    seed.documents = vec![DocumentMutation::Put {
        row: document("old.md", "beforetoken"),
    }];
    let owner = RevisionOwnerRow {
        key: RevisionTreeKey {
            source_component: "Source Legacy".into(),
            revision_component: "Revision.1".into(),
        },
        change: PreparedChange {
            change_id: id("change_original"),
            manifest_hash: Blake3Hash::digest("original"),
        },
    };
    seed.owners = vec![owner.clone()];
    seed.apply(&c).unwrap();
    let old = ordinal(&c, "documents", "old.md");
    let mut update = empty();
    update.documents = vec![DocumentMutation::Put {
        row: document("old.md", "aftertoken"),
    }];
    update.owners = vec![RevisionOwnerRow {
        change: PreparedChange {
            change_id: id("change_competing"),
            manifest_hash: Blake3Hash::digest("competing"),
        },
        ..owner
    }];
    assert_eq!(
        update.apply(&c).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert!(!c.is_autocommit());
    assert_eq!(ordinal(&c, "documents", "old.md"), old);
    assert_eq!(hits(&c, "documents_fts", "beforetoken"), 1);
    assert_eq!(hits(&c, "documents_fts", "aftertoken"), 0);
    assert_eq!(
        c.query_row("SELECT change_id FROM revision_tree_owners", [], |r| r
            .get::<_, String>(
            0
        ))
        .unwrap(),
        "change_original"
    );
    c.execute_batch("COMMIT").unwrap();
}

#[test]
fn owned_rows_replace_one_path_and_revision_signatures_are_append_only() {
    let c = db();
    let mut seed = empty();
    for location in ["selected.md", "unrelated.md"] {
        seed.claims.push(OwnedClaims {
            path: path(location),
            rows: vec![IdentityClaimRow {
                id: id(if location == "selected.md" {
                    "old_claim"
                } else {
                    "unrelated_claim"
                }),
                path: path(location),
                hash: Blake3Hash::digest(location),
                kind: None,
            }],
        });
        seed.links.push(OwnedLinks {
            path: path(location),
            rows: vec![LinkRow {
                from_path: path(location),
                byte_start: 10,
                target_id: None,
                target_path: None,
                resolution: "missing".into(),
            }],
        });
        seed.diagnostics.push(OwnedDiagnostics {
            path: path(location),
            rows: vec![CatalogDiagnostic {
                path: path(location),
                record_id: None,
                code: ErrorCode::RecordInvalid,
                details: json!({"test":"old"}),
            }],
        });
    }
    let revision = RevisionIdentityRow {
        source_id: id("source_delta"),
        revision_id: id("revision_delta"),
        retained_ordinal: 0,
        original_hash: Blake3Hash::digest("original"),
        content_hash: Some(Blake3Hash::digest("content")),
        extractor_fingerprint: Blake3Hash::digest("extractor"),
        extraction_status: "complete".into(),
    };
    seed.revisions = vec![revision.clone()];
    seed.apply(&c).unwrap();
    let mut delta = empty();
    delta.claims = vec![OwnedClaims {
        path: path("selected.md"),
        rows: vec![],
    }];
    delta.links = vec![OwnedLinks {
        path: path("selected.md"),
        rows: vec![],
    }];
    delta.diagnostics = vec![OwnedDiagnostics {
        path: path("selected.md"),
        rows: vec![],
    }];
    delta.revisions = vec![revision.clone()];
    delta.apply(&c).unwrap();
    for (table, column) in [
        ("identity_claims", "path"),
        ("links", "from_path"),
        ("diagnostics", "path"),
    ] {
        assert_eq!(
            c.query_row(
                &format!("SELECT count(*) FROM {table} WHERE {column}='selected.md'"),
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            c.query_row(
                &format!("SELECT count(*) FROM {table} WHERE {column}='unrelated.md'"),
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    let mut bad = empty();
    bad.revisions = vec![RevisionIdentityRow {
        retained_ordinal: 1,
        ..revision
    }];
    assert_eq!(bad.apply(&c).unwrap_err().code, ErrorCode::ContentConflict);
    assert_eq!(
        c.query_row(
            "SELECT retained_ordinal FROM source_revision_identity",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn record_and_dependency_actions_update_scalars_and_json_together() {
    let c = db();
    let mut fields = common(&id("entity_selected"), RecordKind::Entity, "Selected");
    fields.insert("wiki_status".into(), json!("active"));
    fields.insert("wiki_entity_type".into(), json!("person"));
    let bytes = record_bytes(
        crate::domain::CanonicalRecord::new(fields).unwrap(),
        b"Description\n",
    )
    .unwrap();
    let record = parse_note(&bytes).canonical.unwrap();
    let row = RecordRow {
        record,
        path: path("entity.md"),
        hash: Blake3Hash::digest(&bytes),
        authored_status: Some("active".into()),
        eligibility: Eligibility::Current,
        reasons: vec![],
        identity_eligibility: Some(Eligibility::Current),
        description_eligibility: Some(Eligibility::Current),
        disputed: false,
        dependencies: vec![],
    };
    let mut delta = empty();
    delta.records = vec![row.clone()];
    delta.dependencies = vec![ReadDependency {
        path: path("entity.md"),
        expected: ExpectedState::Hash(row.hash.clone()),
    }];
    delta.apply(&c).unwrap();
    let mut changed = row;
    changed.eligibility = Eligibility::Stale;
    changed.description_eligibility = Some(Eligibility::Stale);
    changed.reasons = vec!["source_revision_changed".into()];
    delta.records = vec![changed.clone()];
    delta.dependencies[0].expected = ExpectedState::Absent;
    delta.apply(&c).unwrap();
    let (eligibility, text): (String, String) = c
        .query_row(
            "SELECT eligibility,row_json FROM records WHERE id='entity_selected'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(eligibility, "stale");
    assert_eq!(serde_json::from_str::<RecordRow>(&text).unwrap(), changed);
    assert!(
        c.query_row(
            "SELECT expected_hash IS NULL FROM dependencies WHERE path='entity.md'",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
}

#[test]
fn validation_rejects_cross_owned_duplicate_unsorted_over_budget_and_unknown_actions() {
    let mut delta = empty();
    delta.documents = vec![
        DocumentMutation::Put {
            row: document("a.md", "small"),
        },
        DocumentMutation::Metadata {
            path: path("a.md"),
            eligibility: Eligibility::Stale,
            reasons: vec![],
        },
    ];
    assert_eq!(delta.validate().unwrap_err().code, ErrorCode::IndexCorrupt);
    let mut delta = empty();
    delta.links = vec![OwnedLinks {
        path: path("a.md"),
        rows: vec![LinkRow {
            from_path: path("b.md"),
            byte_start: 0,
            target_id: None,
            target_path: None,
            resolution: "missing".into(),
        }],
    }];
    assert_eq!(delta.validate().unwrap_err().code, ErrorCode::IndexCorrupt);
    let mut delta = empty();
    delta.dependencies = ["b.md", "a.md"]
        .map(|value| ReadDependency {
            path: path(value),
            expected: ExpectedState::Absent,
        })
        .to_vec();
    assert_eq!(delta.validate().unwrap_err().code, ErrorCode::IndexCorrupt);
    let mut delta = empty();
    delta.documents = vec![DocumentMutation::Put {
        row: document("huge.md", &"x".repeat(8 * 1024 * 1024)),
    }];
    assert_eq!(
        delta.validate().unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    let mut delta = empty();
    delta.documents = (0..=MAX_ROWS)
        .map(|n| DocumentMutation::Metadata {
            path: path(&format!("{n}.md")),
            eligibility: Eligibility::Historical,
            reasons: vec![],
        })
        .collect();
    assert_eq!(
        delta.validate().unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert!(
        serde_json::from_value::<DocumentMutation>(
            json!({"action":"execute_sql","sql":"DROP TABLE documents"})
        )
        .is_err()
    );
    let c = Connection::open_in_memory().unwrap();
    assert_eq!(empty().apply(&c).unwrap_err().code, ErrorCode::IndexCorrupt);
}

#[test]
fn old_derivative_admission_and_identity_refusal_happen_before_token_changes() {
    let c = db();
    let mut seed = empty();
    seed.documents = vec![DocumentMutation::Put {
        row: document("selected.md", "oldtoken"),
    }];
    seed.apply(&c).unwrap();
    let mut changed = document("selected.md", "newtoken");
    changed.record_id = Some(id("forged_identity"));
    let mut delta = empty();
    delta.documents = vec![DocumentMutation::Put { row: changed }];
    assert_eq!(
        delta.apply(&c).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert_eq!(hits(&c, "documents_fts", "oldtoken"), 1);
    assert_eq!(hits(&c, "documents_fts", "newtoken"), 0);
    c.execute(
        "UPDATE documents SET body=?1 WHERE path='selected.md'",
        ["x".repeat(8 * 1024 * 1024 + 1)],
    )
    .unwrap();
    delta.documents = vec![DocumentMutation::Put {
        row: document("selected.md", "newtoken"),
    }];
    assert_eq!(delta.apply(&c).unwrap_err().code, ErrorCode::BudgetExceeded);
    assert_eq!(hits(&c, "documents_fts", "oldtoken"), 1);
    assert_eq!(hits(&c, "documents_fts", "newtoken"), 0);
}

#[test]
fn exact_job_fact_admission_requires_v3_target_facts_and_registry_and_keeps_events_immutable() {
    use super::{
        eligibility_facts::EligibilityFact,
        link_facts::registry_keys,
        normalized_fact_delta::{FactDelta, OwnedRegistryKeys, RecordFactMutation},
        structural_rules::StructuralFact,
    };
    use crate::{
        changes::indexed_refresh::{
            IndexedCheckpointTarget, IndexedJobTarget, IndexedWriteOperation,
        },
        records::RegistryEntry,
    };

    let c = db();
    let run_id = id("run_fact_admission");
    let make_row = |record_id: &RecordId, kind, location: &str, body: &[u8]| {
        let mut fields = common(record_id, kind, "Job envelope");
        if kind == RecordKind::Run {
            fields.insert("wiki_status".into(), json!("planned"));
            fields.insert("wiki_created_at".into(), json!("2026-10-06T00:00:00Z"));
        } else {
            fields.insert("wiki_run_id".into(), json!(run_id));
            fields.insert("wiki_sequence".into(), json!(1));
            fields.insert("wiki_event_type".into(), json!("usage_receipt"));
            fields.insert("wiki_occurred_at".into(), json!("2026-10-06T00:00:00Z"));
        }
        let bytes =
            record_bytes(crate::domain::CanonicalRecord::new(fields).unwrap(), body).unwrap();
        RecordRow {
            record: parse_note(&bytes).canonical.unwrap(),
            path: path(location),
            hash: Blake3Hash::digest(&bytes),
            authored_status: (kind == RecordKind::Run).then(|| "planned".into()),
            eligibility: Eligibility::Current,
            reasons: vec![],
            identity_eligibility: None,
            description_eligibility: None,
            disputed: false,
            dependencies: vec![],
        }
    };
    let facts_for = |row: &RecordRow| {
        let structural = StructuralFact::default();
        FactDelta {
            policy: None,
            records: vec![RecordFactMutation {
                record_id: row.record.id().clone(),
                fact: EligibilityFact {
                    baseline: structural.baseline(),
                    structural,
                    direct_paths: std::collections::BTreeSet::from([row.path.clone()]),
                },
            }],
            edge_inserts: vec![],
            edge_deletes: vec![],
            links: vec![],
            registry: vec![OwnedRegistryKeys {
                record_id: row.record.id().clone(),
                path: row.path.clone(),
                keys: registry_keys(&RegistryEntry {
                    id: row.record.id().clone(),
                    path: row.path.clone(),
                    kind: row.record.kind(),
                    aliases: vec![],
                })
                .unwrap(),
            }],
        }
    };
    let operation_for = |row: &RecordRow| IndexedWriteOperation::JobBatch {
        run_id: run_id.clone(),
        records: vec![IndexedJobTarget {
            path: row.path.clone(),
            id: row.record.id().clone(),
            kind: row.record.kind(),
        }],
        checkpoint: None,
    };
    // This fixture isolates pre-replacement admission, not a sealed job plan or
    // policy-maintenance proof. Full delta.apply and public workflow gates have
    // their own policy/sealing assertions; check_before cannot replace them.
    let check =
        |delta: &CatalogDelta, facts: &FactDelta, operation: Option<&IndexedWriteOperation>| {
            facts.check_before(&c, delta, &mut DeltaStats::default(), operation)
        };
    let rows = [
        make_row(
            &run_id,
            RecordKind::Run,
            "runs/run_fact_admission/run.md",
            b"planned\n",
        ),
        make_row(
            &id("event_fact_admission"),
            RecordKind::RunEvent,
            "runs/run_fact_admission/events/event_fact_admission.md",
            b"receipt\n",
        ),
    ];
    for row in &rows {
        let facts = facts_for(row);
        let mut delta = empty();
        delta.version = 3;
        delta.records = vec![row.clone()];
        delta.facts = Some(facts.clone());
        let operation = operation_for(row);
        operation.validate().unwrap();
        check(&delta, &facts, Some(&operation)).unwrap();
        assert_eq!(
            check(&delta, &facts, None).unwrap_err().code,
            ErrorCode::IndexCorrupt
        );
        let mut older = delta.clone();
        older.version = 2;
        assert_eq!(
            check(&older, &facts, Some(&operation)).unwrap_err().code,
            ErrorCode::IndexCorrupt
        );
        let unrelated = IndexedWriteOperation::SourceRefresh {
            source_id: id("source_other"),
        };
        assert_eq!(
            check(&delta, &facts, Some(&unrelated)).unwrap_err().code,
            ErrorCode::IndexCorrupt
        );
        for member in ["id", "path", "kind"] {
            let mut spoof = operation.clone();
            let IndexedWriteOperation::JobBatch { records, .. } = &mut spoof else {
                unreachable!()
            };
            match member {
                "id" => records[0].id = id("forged_job_id"),
                "path" => records[0].path = path("runs/other/run.md"),
                "kind" => records[0].kind = RecordKind::Page,
                _ => unreachable!(),
            }
            assert_eq!(
                check(&delta, &facts, Some(&spoof)).unwrap_err().code,
                ErrorCode::IndexCorrupt,
                "{member}"
            );
        }
        for missing in ["fact", "registry"] {
            let mut incomplete = facts.clone();
            if missing == "fact" {
                incomplete.records.clear();
            } else {
                incomplete.registry.clear();
            }
            assert_eq!(
                check(&delta, &incomplete, Some(&operation))
                    .unwrap_err()
                    .code,
                ErrorCode::IndexCorrupt,
                "{missing}"
            );
        }
        assert_eq!(
            c.query_row("SELECT count(*) FROM records", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    // Adopt old rows through the existing fixture helpers, then install exact
    // own-path facts and actual registry keys in the normalized schema.
    for row in &rows {
        let mut seed = empty();
        seed.records = vec![row.clone()];
        seed.dependencies = vec![ReadDependency {
            path: row.path.clone(),
            expected: ExpectedState::Hash(row.hash.clone()),
        }];
        seed.apply(&c).unwrap();
        facts_for(row)
            .apply(&c, &seed, &mut DeltaStats::default())
            .unwrap();
    }
    let changed_run = make_row(
        &run_id,
        RecordKind::Run,
        rows[0].path.as_str(),
        b"checkpointed\n",
    );
    let facts = facts_for(&changed_run);
    let mut delta = empty();
    delta.version = 3;
    delta.records = vec![changed_run.clone()];
    delta.facts = Some(facts.clone());
    let mut checkpoint = operation_for(&changed_run);
    let IndexedWriteOperation::JobBatch {
        checkpoint: asset, ..
    } = &mut checkpoint
    else {
        unreachable!()
    };
    *asset = Some(IndexedCheckpointTarget {
        path: path(&format!(
            "runs/{run_id}/checkpoints/{}.json",
            changed_run.hash.hex()
        )),
        hash: Blake3Hash::digest(b"compact fixture"),
        run_hash: changed_run.hash.clone(),
    });
    checkpoint.validate().unwrap();
    check(&delta, &facts, Some(&checkpoint)).unwrap();
    assert_eq!(
        check(&delta, &facts, None).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    // Existing rows can rely on an adopted fact. Missing the adopted fact must
    // still refuse even though the exact Run discriminator admits the update.
    let mut retained = facts.clone();
    retained.records.clear();
    check(&delta, &retained, Some(&checkpoint)).unwrap();
    c.execute(
        "DELETE FROM record_eligibility_facts WHERE record_id=?1",
        [run_id.as_str()],
    )
    .unwrap();
    assert_eq!(
        check(&delta, &retained, Some(&checkpoint))
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    for wrong in ["path", "kind"] {
        let mut changed = changed_run.clone();
        if wrong == "path" {
            changed.path = path("runs/run_fact_admission/moved.md");
        } else {
            changed = make_row(
                &run_id,
                RecordKind::RunEvent,
                changed_run.path.as_str(),
                b"kind spoof\n",
            );
        }
        let mut wrong_delta = delta.clone();
        wrong_delta.records = vec![changed.clone()];
        // Deliberately match the forged tuple: old identity checks must reject
        // independently of operation.validate's earlier sealed-path boundary.
        assert_eq!(
            check(
                &wrong_delta,
                &facts_for(&changed),
                Some(&operation_for(&changed))
            )
            .unwrap_err()
            .code,
            ErrorCode::ContentConflict,
            "{wrong}"
        );
    }
    let changed_event = make_row(
        rows[1].record.id(),
        RecordKind::RunEvent,
        rows[1].path.as_str(),
        b"overwritten receipt\n",
    );
    delta.records = vec![changed_event.clone()];
    assert_eq!(
        check(
            &delta,
            &facts_for(&changed_event),
            Some(&operation_for(&changed_event))
        )
        .unwrap_err()
        .code,
        ErrorCode::ContentConflict
    );
    // check_before itself never changes the selected bytes or row identity.
    for row in &rows {
        assert_eq!(
            c.query_row(
                "SELECT hash FROM records WHERE id=?1",
                [row.record.id().as_str()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            row.hash.as_str()
        );
    }
}
