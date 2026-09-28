use lwiki::{
    catalog::*,
    changes::*,
    domain::*,
    sources::*,
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit},
};
use std::{
    collections::BTreeMap,
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
fn rel(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn fixture() -> (tempfile::TempDir, VaultRoot, Catalog) {
    let t = tempfile::tempdir().unwrap();
    fs::write(
        t.path().join("WIKI.md"),
        b"---\nwiki_schema: \"1\"\nwiki_id: vault_sql\nwiki_kind: vault\ntitle: SQL fixture\n---\n",
    )
    .unwrap();
    fs::write(t.path().join("entity.md"), entity("café old")).unwrap();
    let root = VaultRoot::explicit(t.path()).unwrap();
    let catalog = Catalog::new(
        VaultFs::new(root.clone()),
        RecordId::new("vault_sql").unwrap(),
    );
    (t, root, catalog)
}
fn entity(title: &str) -> Vec<u8> {
    format!("---\nwiki_schema: \"1\"\nwiki_id: entity_sql\nwiki_kind: entity\ntitle: {title}\nwiki_status: active\nwiki_entity_type: concept\n---\n# {title}\n\n{title}\n").into_bytes()
}
fn writer(root: &VaultRoot) -> WriterPermit {
    WriterPermit::acquire(root, Duration::from_millis(200)).unwrap()
}
fn draft(path: &str, bytes: Vec<u8>) -> ChangeDraft {
    ChangeDraft {
        title: "SQL application".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: vec![ExpectedWrite {
            target: rel(path),
            expected: ExpectedState::Absent,
            proposed: Some(bytes),
            apply_after: vec![],
        }],
    }
}
struct Fail {
    point: PublicationCheckpoint,
    armed: AtomicBool,
}
impl PublicationFault for Fail {
    fn check(&self, p: PublicationCheckpoint) -> Result<()> {
        if p == self.point && self.armed.swap(false, Ordering::SeqCst) {
            Err(WikiError::new(
                ErrorCode::Internal,
                "injected actual SQLite boundary failure",
            ))
        } else {
            Ok(())
        }
    }
}
fn failing(root: &VaultRoot, point: PublicationCheckpoint) -> Catalog {
    Catalog::with_options(
        VaultFs::new(root.clone()),
        RecordId::new("vault_sql").unwrap(),
        CatalogOptions {
            fault: Some(Arc::new(Fail {
                point,
                armed: AtomicBool::new(true),
            })),
            ..Default::default()
        },
    )
}
#[test]
fn reader_keeps_snapshot_across_generation_switch() {
    let (t, root, c) = fixture();
    let w = writer(&root);
    let first = c.sync(&w).unwrap();
    let held = c.index_snapshot().unwrap();
    assert_eq!(held.document_matches("cafe", 100).unwrap().len(), 1);
    assert_eq!(
        held.graph_matches("cafe", RecordKind::Entity, 100)
            .unwrap()
            .len(),
        1
    );
    fs::write(t.path().join("entity.md"), entity("new replacement")).unwrap();
    let second = c.sync(&w).unwrap();
    assert!(second.snapshot.generation > first.snapshot.generation);
    assert_eq!(held.snapshot(), &first.snapshot);
    assert_eq!(held.document_matches("cafe", 100).unwrap().len(), 1);
    assert_eq!(
        held.graph_matches("cafe", RecordKind::Entity, 100)
            .unwrap()
            .len(),
        1
    );
    assert!(
        held.document_matches("replacement", 100)
            .unwrap()
            .is_empty()
    );
    let current = c.index_snapshot().unwrap();
    assert!(current.document_matches("cafe", 100).unwrap().is_empty());
    assert_eq!(
        current
            .graph_matches("replacement", RecordKind::Entity, 100)
            .unwrap()
            .len(),
        1
    );
    let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
    for table in ["documents_fts", "graph_fts"] {
        let distinct: i64 = db
            .query_row(
                &format!("SELECT count(DISTINCT gen) FROM {table}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(distinct, 1);
    }
    assert_eq!(
        db.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
}
#[test]
fn failed_fts_publication_preserves_previous_view() {
    for point in [
        PublicationCheckpoint::AfterOrdinaryRows,
        PublicationCheckpoint::AfterDocumentsFts,
        PublicationCheckpoint::AfterGraphFts,
        PublicationCheckpoint::AfterPointer,
    ] {
        let (t, root, c) = fixture();
        let w = writer(&root);
        let first = c.sync(&w).unwrap();
        let held = c.index_snapshot().unwrap();
        fs::write(t.path().join("entity.md"), entity("changed item")).unwrap();
        assert!(failing(&root, point).rebuild(&w).is_err());
        for reader in [held, c.index_snapshot().unwrap()] {
            assert_eq!(reader.snapshot(), &first.snapshot);
            assert_eq!(reader.document_matches("cafe", 100).unwrap().len(), 1);
            assert_eq!(
                reader
                    .graph_matches("cafe", RecordKind::Entity, 100)
                    .unwrap()
                    .len(),
                1
            );
            assert!(
                reader
                    .graph_matches("changed", RecordKind::Entity, 100)
                    .unwrap()
                    .is_empty()
            );
        }
        let next = c.sync(&w).unwrap();
        assert!(next.snapshot.generation > first.snapshot.generation);
    }
}
#[test]
fn unpublished_rows_reclaimed_when_canonical_reverts() {
    let (t, root, c) = fixture();
    let w = writer(&root);
    let first = c.sync(&w).unwrap();
    fs::write(t.path().join("entity.md"), entity("temporary change")).unwrap();
    assert!(
        failing(&root, PublicationCheckpoint::AfterOrdinaryRows)
            .sync(&w)
            .is_err()
    );
    fs::write(t.path().join("entity.md"), entity("café old")).unwrap();
    let reused = c.sync(&w).unwrap();
    assert!(reused.reused);
    assert_eq!(reused.snapshot, first.snapshot);
    let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM generations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}
#[test]
fn logical_cache_drift_refuses_verified_output_and_rebuild_repairs() {
    for mutation in [
        "UPDATE documents SET row_json=replace(row_json,'café old','invented title') WHERE record_id='entity_sql'",
        "UPDATE documents_fts SET body='invented lexical content' WHERE title='café old'",
        "UPDATE graph_fts SET name='invented graph name' WHERE target_id='entity_sql'",
        "UPDATE entities SET entity_type='invented type' WHERE id='entity_sql'",
    ] {
        let (t, root, c) = fixture();
        let w = writer(&root);
        c.sync(&w).unwrap();
        let canonical = scan::scan(c.fs(), c.vault_id()).unwrap();
        let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
        db.execute_batch(mutation).unwrap();
        assert_eq!(
            db.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        drop(db);
        assert_eq!(
            c.verified_snapshot(None).err().unwrap().code,
            ErrorCode::IndexCorrupt
        );
        assert_eq!(c.sync(&w).unwrap_err().code, ErrorCode::IndexCorrupt);
        c.rebuild(&w).unwrap();
        let reader = c.verified_snapshot(None).unwrap();
        assert_eq!(reader.projection(), &canonical);
        assert_eq!(reader.document_matches("cafe", 10).unwrap().len(), 1);
        assert_eq!(
            reader
                .graph_matches("cafe", RecordKind::Entity, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(reader.document_matches("invented", 10).unwrap().is_empty());
    }
}
#[test]
fn cache_delete_rebuild_canonical_equivalence_zero_remote() {
    let (t, root, c) = fixture();
    let w = writer(&root);
    c.sync(&w).unwrap();
    let before = c.index_snapshot().unwrap().projection().clone();
    // Close all readers before explicitly removing disposable cache fixture files.
    fs::remove_dir_all(t.path().join(".wiki/cache")).unwrap();
    let report = c.rebuild(&w).unwrap();
    assert!(report.vector_loss_unknown);
    assert!(!report.vector_cache_lost);
    assert_eq!(before, *c.index_snapshot().unwrap().projection());
    assert_eq!(before, scan::scan(c.fs(), c.vault_id()).unwrap());
    // No provider/dispatcher exists anywhere in the scan/rebuild API.
}
#[test]
fn real_apply_capture_and_page_recovery_after_sql_commit() {
    let (_t, root, c) = fixture();
    let w = writer(&root);
    c.sync(&w).unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let source = SourceStore::new(VaultFs::new(root.clone()));
    let plan = source
        .plan_capture(CaptureRequest {
            title: "Captured text".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture.txt".into(),
            original: b"Exact captured SQL evidence".to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: Some("text/plain".into()),
        })
        .unwrap();
    let prepared = engine.prepare(&w, plan.draft.unwrap()).unwrap().prepared;
    assert_eq!(
        engine
            .apply(&w, &prepared, &CatalogGraphValidator, &c)
            .unwrap()
            .status,
        ChangeStatus::Committed
    );
    assert_eq!(
        c.verified_snapshot(None)
            .unwrap()
            .document_matches("captured", 100)
            .unwrap()
            .iter()
            .filter(|(d, _)| d.source_id.is_some())
            .count(),
        1
    );
    let p=engine.prepare(&w,draft("page.md",b"---\nwiki_schema: \"1\"\nwiki_id: page_sql\nwiki_kind: page\ntitle: Applied page\nwiki_status: reviewed\n---\n# Applied page\n".to_vec())).unwrap();
    assert!(
        engine
            .apply(
                &w,
                &p.prepared,
                &CatalogGraphValidator,
                &failing(&root, PublicationCheckpoint::AfterCommit)
            )
            .is_err()
    );
    assert_eq!(
        engine.inspect(&p.prepared.change_id).unwrap().status,
        ChangeStatus::FilesApplied
    );
    let sql_committed = c.index_snapshot().unwrap().snapshot().clone();
    assert_eq!(
        c.verified_snapshot(None).err().unwrap().code,
        ErrorCode::RecoveryRequired
    );
    assert_eq!(c.sync(&w).unwrap_err().code, ErrorCode::RecoveryRequired);
    engine.recover(&w, &CatalogGraphValidator, &c).unwrap();
    let reader = c.verified_snapshot(None).unwrap();
    assert_eq!(reader.snapshot(), &sql_committed);
    assert_eq!(reader.document_matches("Applied", 100).unwrap().len(), 1);
    assert!(matches!(
        reader.verification(),
        SnapshotVerification::VerifiedSnapshot { .. }
    ));
}
#[test]
fn unavailable_cache_rejected_before_canonical_mutation() {
    for corrupt in [false, true] {
        let (t, root, c) = fixture();
        let w = writer(&root);
        c.sync(&w).unwrap();
        if corrupt {
            fs::write(t.path().join(".wiki/cache/index.sqlite"), b"not sqlite").unwrap();
        } else {
            let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
            db.pragma_update(None, "user_version", 999).unwrap();
        }
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let p = engine
            .prepare(&w, draft("new.md", b"unadopted new".to_vec()))
            .unwrap();
        let e = engine
            .apply(&w, &p.prepared, &CatalogGraphValidator, &c)
            .unwrap_err();
        assert!(matches!(
            e.code,
            ErrorCode::IndexCorrupt | ErrorCode::CapabilityUnavailable
        ));
        assert!(!t.path().join("new.md").exists());
        assert_eq!(
            engine.inspect(&p.prepared.change_id).unwrap().status,
            ChangeStatus::Prepared
        );
    }
}
#[test]
fn snapshot_readonly_freshness_and_idempotent_sync() {
    let (t, root, c) = fixture();
    assert!(!t.path().join(".wiki").exists());
    c.check_available().unwrap();
    assert!(!t.path().join(".wiki").exists());
    assert_eq!(
        c.index_snapshot().err().unwrap().code,
        ErrorCode::OfflineUnavailable
    );
    assert!(!t.path().join(".wiki").exists());
    let w = writer(&root);
    let a = c.sync(&w).unwrap();
    let b = c.sync(&w).unwrap();
    assert_eq!(a.snapshot, b.snapshot);
    assert!(b.reused);
    fs::write(t.path().join("copied.md"), entity("café old")).unwrap();
    assert_eq!(
        c.verified_snapshot(None).err().unwrap().code,
        ErrorCode::FreshnessConflict
    );
    let current = c.verified_snapshot(Some(&w)).unwrap();
    assert!(
        !current
            .projection()
            .records
            .contains_key(&RecordId::new("entity_sql").unwrap())
    );
    let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM records WHERE gen=?1 AND id='entity_sql'",
            [current.snapshot().generation as i64],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
#[test]
fn explicit_cache_migration_reports_only_actual_vector_loss() {
    for nonempty in [false, true] {
        let (t, root, c) = fixture();
        let w = writer(&root);
        c.sync(&w).unwrap();
        let held = c.index_snapshot().unwrap();
        let canonical = held.projection().clone();
        let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE cached_vectors(value BLOB); PRAGMA user_version=0;")
            .unwrap();
        if nonempty {
            db.execute("INSERT INTO cached_vectors VALUES(?1)", [&[1u8, 2][..]])
                .unwrap();
        }
        drop(db);
        assert_eq!(
            c.check_available().unwrap_err().code,
            ErrorCode::CapabilityUnavailable
        );
        let report = c.rebuild(&w).unwrap();
        assert_eq!(report.vector_cache_lost, nonempty);
        assert!(report.vector_loss_unknown);
        assert_eq!(c.index_snapshot().unwrap().projection(), &canonical);
        assert_eq!(
            held.graph_matches("cafe", RecordKind::Entity, 10)
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn failed_cache_migration_rolls_back_schema_and_vector_rows() {
    for point in [
        PublicationCheckpoint::AfterOrdinaryRows,
        PublicationCheckpoint::AfterDocumentsFts,
        PublicationCheckpoint::AfterGraphFts,
        PublicationCheckpoint::AfterPointer,
        PublicationCheckpoint::MigrationBeforeCommit,
    ] {
        let (t, root, c) = fixture();
        let w = writer(&root);
        c.sync(&w).unwrap();
        let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE cached_vectors(value BLOB); INSERT INTO cached_vectors VALUES(x'0102'); PRAGMA user_version=0;").unwrap();
        let before: i64 = db
            .query_row("SELECT published_gen FROM index_meta", [], |r| r.get(0))
            .unwrap();
        assert!(failing(&root, point).rebuild(&w).is_err());
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM cached_vectors", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT published_gen FROM index_meta", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            before
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM graph_fts WHERE graph_fts MATCH 'cafe'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
}
#[test]
fn committed_migration_retains_loss_notice_after_failed_return() {
    let (t, root, c) = fixture();
    let w = writer(&root);
    c.sync(&w).unwrap();
    let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE cached_vectors(value BLOB); INSERT INTO cached_vectors VALUES(x'0102'); PRAGMA user_version=0;").unwrap();
    drop(db);
    assert!(
        failing(&root, PublicationCheckpoint::AfterCommit)
            .rebuild(&w)
            .is_err()
    );
    let report = c.sync(&w).unwrap();
    assert!(report.reused && report.vector_cache_lost && report.vector_loss_unknown);
    let rebuilt = c.rebuild(&w).unwrap();
    assert!(rebuilt.vector_cache_lost && rebuilt.vector_loss_unknown);
}
#[test]
fn wide_invalid_evidence_span_remains_diagnosed_and_rebuildable() {
    let (t, root, c) = fixture();
    let w = writer(&root);
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let source = SourceStore::new(VaultFs::new(root.clone()));
    let plan = source
        .plan_capture(CaptureRequest {
            title: "Wide span fixture".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture".into(),
            original: b"quote".to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    let prepared = engine.prepare(&w, plan.draft.unwrap()).unwrap().prepared;
    engine
        .apply(&w, &prepared, &CatalogGraphValidator, &c)
        .unwrap();
    fs::write(t.path().join("assertion.md"),b"---\nwiki_schema: \"1\"\nwiki_id: wide_assertion\nwiki_kind: assertion\ntitle: Proposed assertion\nwiki_status: proposed\nwiki_subject_id: entity_sql\nwiki_object_id: entity_sql\nwiki_predicate: uses\n---\n").unwrap();
    let evidence = format!(
        "---\nwiki_schema: \"1\"\nwiki_id: wide_evidence\nwiki_kind: evidence\ntitle: Invalid wide span\nwiki_status: active\nwiki_assertion_id: wide_assertion\nwiki_source_id: {}\nwiki_source_revision: {}\nwiki_stance: supports\nwiki_locator_kind: utf8-bytes\nwiki_span_start: 0\nwiki_span_end: 9223372036854775808\nwiki_quote_hash: {}\n---\n```text\nquote\n```\n",
        plan.source_id,
        plan.revision_id,
        Blake3Hash::digest(b"quote")
    );
    fs::write(t.path().join("evidence.md"), evidence).unwrap();
    let projection = scan::scan(c.fs(), c.vault_id()).unwrap();
    assert_eq!(
        projection.records[&RecordId::new("wide_evidence").unwrap()].eligibility,
        Eligibility::Invalid
    );
    c.rebuild(&w).unwrap();
    let reader = c.verified_snapshot(None).unwrap();
    assert_eq!(reader.projection(), &projection);
    assert!(reader.projection().documents.iter().any(|d| {
        d.record_id
            .as_ref()
            .is_some_and(|id| id.as_str() == "wide_evidence")
            && d.eligibility == Eligibility::Invalid
            && d.raw_text.contains("9223372036854775808")
    }));
    let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT span_end FROM evidence WHERE id='wide_evidence'",
            [],
            |r| r.get::<_, Option<i64>>(0)
        )
        .unwrap(),
        None
    );
}
#[test]
fn reclaimed_generations_preserve_held_reader_and_bound_ordinary_rows() {
    let (t, root, c) = fixture();
    let w = writer(&root);
    c.sync(&w).unwrap();
    let old = c.index_snapshot().unwrap();
    fs::write(t.path().join("entity.md"), entity("replacement")).unwrap();
    assert!(
        failing(&root, PublicationCheckpoint::AfterDocumentsFts)
            .sync(&w)
            .is_err()
    );
    c.sync(&w).unwrap();
    let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM generations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(DISTINCT gen) FROM documents", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(old.document_matches("cafe", 10).unwrap().len(), 1);
}
#[test]
fn missing_cache_invalid_timeout_is_rejected_without_side_effects() {
    let (t, root, _) = fixture();
    let c = Catalog::with_options(
        VaultFs::new(root),
        RecordId::new("vault_sql").unwrap(),
        CatalogOptions {
            busy_timeout_ms: 30_001,
            ..Default::default()
        },
    );
    assert_eq!(
        c.check_available().unwrap_err().code,
        ErrorCode::ConfigInvalid
    );
    assert!(!t.path().join(".wiki").exists());
}

#[test]
fn cache_schema_damage_rejected_before_canonical_mutation() {
    let (t, root, c) = fixture();
    let w = writer(&root);
    c.sync(&w).unwrap();
    let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
    db.execute("DROP TABLE entities", []).unwrap();
    drop(db);
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let p = engine
        .prepare(&w, draft("new.md", b"new note".to_vec()))
        .unwrap();
    assert_eq!(
        engine
            .apply(&w, &p.prepared, &CatalogGraphValidator, &c)
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    assert!(!t.path().join("new.md").exists());
}
