//! Native process interruption at real SQLite publication boundaries.
use lwiki::{catalog::*, changes::*, domain::*, vault::*};
use std::{
    collections::BTreeMap,
    fs,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

fn rel(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn catalog(root: &VaultRoot) -> Catalog {
    Catalog::new(
        VaultFs::new(root.clone()),
        RecordId::new("vault_crash").unwrap(),
    )
}
fn entity(title: &str) -> Vec<u8> {
    format!("---\nwiki_schema: \"1\"\nwiki_id: entity_crash\nwiki_kind: entity\ntitle: {title}\nwiki_status: active\nwiki_entity_type: component\n---\n{title}\n").into_bytes()
}
struct StopAt {
    point: PublicationCheckpoint,
    marker: std::path::PathBuf,
}
impl PublicationFault for StopAt {
    fn check(&self, point: PublicationCheckpoint) -> Result<()> {
        if point == self.point {
            fs::write(&self.marker, b"publication reached").unwrap();
            loop {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }
}
#[test]
#[ignore = "invoked explicitly by both enabled SQLite SIGKILL wrappers"]
fn catalog_crash_child() {
    let root = VaultRoot::explicit(std::env::var_os("LWIKI_SQL_CRASH_VAULT").unwrap()).unwrap();
    let point = match std::env::var("LWIKI_SQL_CRASH_POINT").unwrap().as_str() {
        "ordinary" => PublicationCheckpoint::AfterOrdinaryRows,
        "documents" => PublicationCheckpoint::AfterDocumentsFts,
        "graph" => PublicationCheckpoint::AfterGraphFts,
        "pointer" => PublicationCheckpoint::AfterPointer,
        "commit" => PublicationCheckpoint::AfterCommit,
        "migration" => PublicationCheckpoint::MigrationBeforeCommit,
        _ => panic!("unknown fixture checkpoint"),
    };
    let c = Catalog::with_options(
        VaultFs::new(root.clone()),
        RecordId::new("vault_crash").unwrap(),
        CatalogOptions {
            fault: Some(Arc::new(StopAt {
                point,
                marker: root.path().join(".wiki/crash-ready"),
            })),
            ..Default::default()
        },
    );
    let w = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
    if std::env::var_os("LWIKI_SQL_CRASH_MIGRATION").is_some() {
        c.rebuild(&w).unwrap();
        panic!("migration checkpoint was not reached");
    }
    let prepared: PreparedChange =
        serde_json::from_slice(&fs::read(root.path().join(".wiki/crash-change.json")).unwrap())
            .unwrap();
    ChangeEngine::new(VaultFs::new(root))
        .unwrap()
        .apply(&w, &prepared, &CatalogGraphValidator, &c)
        .unwrap();
    panic!("checkpoint was not reached");
}

#[test]
fn sqlite_publication_sigkill_recovers_real_graph() {
    for checkpoint in ["ordinary", "documents", "graph", "pointer", "commit"] {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("WIKI.md"), b"---\nwiki_schema: \"1\"\nwiki_id: vault_crash\nwiki_kind: vault\ntitle: Crash fixture\n---\n").unwrap();
        fs::write(t.path().join("entity.md"), entity("Original item")).unwrap();
        let root = VaultRoot::explicit(t.path()).unwrap();
        let c = catalog(&root);
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let w = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
        let old = c.sync(&w).unwrap().snapshot;
        let before = c.fs().read_before(&rel("entity.md")).unwrap().unwrap();
        let prepared = engine
            .prepare(
                &w,
                ChangeDraft {
                    title: "Publication crash fixture".into(),
                    origin: None,
                    inverse_of: None,
                    allocated_ids: BTreeMap::new(),
                    read_preconditions: vec![],
                    operations: vec![ExpectedWrite {
                        target: rel("entity.md"),
                        expected: ExpectedState::Hash(before.hash),
                        proposed: Some(entity("Replacement item")),
                        apply_after: vec![],
                    }],
                },
            )
            .unwrap()
            .prepared;
        fs::write(
            t.path().join(".wiki/crash-change.json"),
            serde_json::to_vec(&prepared).unwrap(),
        )
        .unwrap();
        drop(w);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "catalog_crash_child", "--ignored", "--nocapture"])
            .env("LWIKI_SQL_CRASH_VAULT", t.path())
            .env("LWIKI_SQL_CRASH_POINT", checkpoint)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !t.path().join(".wiki/crash-ready").exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("{checkpoint}: child exited early: {status}");
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("{checkpoint}: checkpoint timeout");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(9));
        }
        let visible = c.index_snapshot().unwrap();
        if checkpoint == "commit" {
            assert!(visible.snapshot().generation > old.generation);
            assert_eq!(
                visible.document_matches("Replacement", 10).unwrap().len(),
                1
            );
            assert_eq!(
                visible
                    .graph_matches("Replacement", RecordKind::Entity, 10)
                    .unwrap()
                    .len(),
                1
            );
        } else {
            assert_eq!(visible.snapshot(), &old);
            assert_eq!(visible.document_matches("Original", 10).unwrap().len(), 1);
            assert_eq!(
                visible
                    .graph_matches("Original", RecordKind::Entity, 10)
                    .unwrap()
                    .len(),
                1
            );
        }
        drop(visible);
        assert_eq!(
            c.verified_snapshot(None).err().unwrap().code,
            ErrorCode::RecoveryRequired
        );
        let w = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
        engine.recover(&w, &CatalogGraphValidator, &c).unwrap();
        assert_eq!(
            engine.inspect(&prepared.change_id).unwrap().status,
            ChangeStatus::Committed
        );
        let final_view = c.verified_snapshot(None).unwrap();
        assert_eq!(
            final_view
                .document_matches("Replacement", 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            final_view
                .graph_matches("Replacement", RecordKind::Entity, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(
            final_view
                .document_matches("Original", 10)
                .unwrap()
                .is_empty()
        );
        if checkpoint == "commit" {
            assert_eq!(final_view.snapshot().generation, old.generation + 1);
        }
    }
}
#[test]
fn sqlite_migration_sigkill_preserves_vectors_or_retains_loss_notice() {
    for checkpoint in [
        "ordinary",
        "documents",
        "graph",
        "pointer",
        "migration",
        "commit",
    ] {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("WIKI.md"),b"---\nwiki_schema: \"1\"\nwiki_id: vault_crash\nwiki_kind: vault\ntitle: Migration crash fixture\n---\n").unwrap();
        fs::write(t.path().join("entity.md"), entity("Original item")).unwrap();
        let root = VaultRoot::explicit(t.path()).unwrap();
        let c = catalog(&root);
        let w = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
        c.sync(&w).unwrap();
        let original = c.index_snapshot().unwrap().projection().clone();
        let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE cached_vectors(value BLOB); INSERT INTO cached_vectors VALUES(x'0102'); PRAGMA user_version=0;").unwrap();
        drop(db);
        drop(w);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "catalog_crash_child", "--ignored", "--nocapture"])
            .env("LWIKI_SQL_CRASH_VAULT", t.path())
            .env("LWIKI_SQL_CRASH_POINT", checkpoint)
            .env("LWIKI_SQL_CRASH_MIGRATION", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !t.path().join(".wiki/crash-ready").exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("{checkpoint}: migration child exited: {status}");
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("{checkpoint}: migration checkpoint timeout");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(9));
        }
        let db = rusqlite::Connection::open(t.path().join(".wiki/cache/index.sqlite")).unwrap();
        let version = db
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap();
        if checkpoint == "commit" {
            assert_eq!(version, 1);
        } else {
            assert_eq!(version, 0);
            assert_eq!(
                db.query_row("SELECT count(*) FROM cached_vectors", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM graph_fts WHERE graph_fts MATCH 'Original'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
        }
        drop(db);
        let w = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
        let report = if checkpoint == "commit" {
            c.sync(&w)
        } else {
            c.rebuild(&w)
        }
        .unwrap();
        assert!(report.vector_cache_lost && report.vector_loss_unknown);
        assert_eq!(c.verified_snapshot(None).unwrap().projection(), &original);
    }
}
