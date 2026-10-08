use super::*;
use crate::{
    catalog::{
        maintenance_types::MaintenanceLimits,
        normalized_build::{BuildCheckpoint, BuildFault, NormalizedBuilder},
        normalized_delta::DocumentMutation,
        query::QuerySnapshot,
        scan, sql,
    },
    domain::{Blake3Hash, ByteSpan, CanonicalRecord, CitationRef, SourceSpanRef},
    sources::{CaptureRequest, CitationScope, ExtractionInput, SourceOrigin, SourceStore},
    vault::{VaultFs, VaultRoot},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, sync::Arc};

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn note(kind: &str, name: &str, extra: Value, body: &[u8]) -> Vec<u8> {
    let mut fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(name)),
        ("wiki_kind".into(), json!(kind)),
        ("title".into(), json!(name)),
    ]);
    fields.extend(
        extra
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    crate::sources::revision::record_bytes(CanonicalRecord::new(fields).unwrap(), body).unwrap()
}
fn page(name: &str, extra: Value, body: &str) -> Vec<u8> {
    let mut fields = extra.as_object().unwrap().clone();
    fields.insert("wiki_status".into(), json!("reviewed"));
    note("page", name, Value::Object(fields), body.as_bytes())
}
struct Fixture {
    temp: tempfile::TempDir,
    catalog: Catalog,
    writer: WriterPermit,
}
impl Fixture {
    fn new(retained: bool, notes: &[(&str, Vec<u8>)]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            note("vault", "vault_external_pages", json!({}), b""),
        )
        .unwrap();
        let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs_handle.root(), Duration::ZERO).unwrap();
        if retained {
            crate::storage::cleanup(
                &fs_handle,
                &writer,
                &crate::storage::StorageOptions::default(),
            )
            .unwrap();
        }
        let fixture = Self {
            catalog: Catalog::new(fs_handle, id("vault_external_pages")),
            writer,
            temp,
        };
        for (name, bytes) in notes {
            fixture.write(name, bytes);
        }
        fixture.catalog.rebuild_normalized(&fixture.writer).unwrap();
        fixture
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let target = self.catalog.fs().root().resolve(&path(name)).unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    fn delete(&self, name: &str) {
        fs::remove_file(self.catalog.fs().root().resolve(&path(name)).unwrap()).unwrap();
    }
    fn reader(&self) -> QuerySnapshot {
        self.catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap()
    }
    fn sync(&self) -> super::super::maintenance::MaintenanceReport {
        self.catalog.sync_normalized(&self.writer).unwrap()
    }
    fn selected(&self) -> std::path::PathBuf {
        let selection = selector::maintenance_header(
            self.catalog.fs(),
            self.catalog.vault_id(),
            Duration::ZERO,
        )
        .unwrap()
        .unwrap()
        .0;
        self.catalog
            .fs()
            .root()
            .resolve(&path(&format!(
                ".wiki/cache/catalogs/{}.sqlite",
                selection.file_id
            )))
            .unwrap()
    }
    /// Independent complete reconstruction, without publishing the oracle.
    fn oracle(&self) {
        let actual = self.reader();
        let input = MaintenanceInput::capture(
            self.catalog.fs(),
            self.catalog.vault_id(),
            MaintenanceLimits::rebuild(),
        )
        .unwrap();
        let identity = BuildIdentity {
            selection: CatalogSelection::new(
                self.catalog.vault_id().clone(),
                actual.snapshot().generation + 1,
            )
            .unwrap(),
            origin: None,
            // Every fixture starts with no readable legacy embedding cache.
            // Initial rebuild declares unknown loss history, and Page/source
            // publications plus cache-loss reconstruction preserve that notice.
            vector_cache_lost: false,
            vector_loss_unknown: true,
        };
        selector::prepare(self.catalog.fs(), &self.writer, &identity.selection).unwrap();
        let mut builder = NormalizedBuilder::begin(
            self.catalog.fs(),
            &self.writer,
            identity,
            BuildLimits::default(),
        )
        .unwrap();
        let projection = scan::project_maintenance_with_sink(&input, &mut builder).unwrap();
        let completed = builder.finish_normalized(&projection).unwrap();
        input.final_recheck().unwrap();
        let expected = Connection::open(&completed.path).unwrap();
        let actual_tables = logical_tables(actual.connection());
        let expected_tables = logical_tables(&expected);
        assert_eq!(
            actual_tables.keys().collect::<Vec<_>>(),
            expected_tables.keys().collect::<Vec<_>>()
        );
        for (table, rows) in &expected_tables {
            assert_rows_equal(table, &actual_tables[table], rows);
        }
        assert_rows_equal(
            "documents_fts postings",
            &fts_postings(
                actual.connection(),
                "documents_fts",
                "documents",
                "doc_row",
                "path",
            ),
            &fts_postings(&expected, "documents_fts", "documents", "doc_row", "path"),
        );
        assert_rows_equal(
            "graph_fts postings",
            &fts_postings(
                actual.connection(),
                "graph_fts",
                "graph_rows",
                "graph_row",
                "target_id",
            ),
            &fts_postings(
                &expected,
                "graph_fts",
                "graph_rows",
                "graph_row",
                "target_id",
            ),
        );
        drop(expected);
        drop(actual);
        selector::retire_unpublished(
            self.catalog.fs(),
            &self.writer,
            self.catalog.vault_id(),
            &completed.identity.selection,
            Duration::ZERO,
        )
        .unwrap();
    }
}
/// Keep exact equality, but identify only the differing relation/row ordinal;
/// an unrelated large document must not expand a failed test into a corpus dump.
fn assert_rows_equal(table: &str, actual: &[String], expected: &[String]) {
    assert!(
        actual == expected,
        "normalized table {table} differs: actual rows {}, expected rows {}, first differing row {:?}",
        actual.len(),
        expected.len(),
        actual.iter().zip(expected).position(|(a, b)| a != b)
    );
}
/// Compare every ordinary normalized table; only physical row ordinals and
/// catalog publication/audit identity are excluded. FTS postings are compared
/// separately by their ordinary owner rather than their physical row ordinal.
fn logical_tables(connection: &Connection) -> BTreeMap<String, Vec<String>> {
    let tables:Vec<String>=connection.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    let mut out = BTreeMap::new();
    for table in tables {
        if table.starts_with("documents_fts")
            || table.starts_with("graph_fts")
            || table.ends_with("_vocab")
        {
            continue;
        }
        let columns: Vec<String> = if table == "catalog_meta" {
            vec![
                "schema_version",
                "vault_id",
                "parser_hash",
                "vector_cache_lost",
                "vector_loss_unknown",
                "proof_layout_version",
                "revision_ownership_version",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        } else {
            connection
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .into_iter()
                .filter(|column| {
                    !matches!(
                        column.as_str(),
                        "doc_row" | "graph_row" | "link_row" | "diagnostic_row" | "dependency_row"
                    )
                })
                .collect()
        };
        let mut statement = connection
            .prepare(&format!("SELECT {} FROM {table}", columns.join(",")))
            .unwrap();
        let mut rows = statement.query([]).unwrap();
        let mut values = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            let value: Vec<_> = (0..columns.len())
                .map(|column| row.get::<_, rusqlite::types::Value>(column).unwrap())
                .collect();
            values.push(format!("{value:?}"));
        }
        values.sort();
        out.insert(table, values);
    }
    out
}
fn fts_postings(c: &Connection, fts: &str, table: &str, row: &str, owner: &str) -> Vec<String> {
    c.execute_batch(&format!("DROP TABLE IF EXISTS temp.external_page_vocab; CREATE VIRTUAL TABLE temp.external_page_vocab USING fts5vocab(main,{fts},instance)")).unwrap();
    let mut statement=c.prepare(&format!("SELECT v.term,t.{owner},v.col,v.offset FROM external_page_vocab v JOIN {table} t ON t.{row}=v.doc ORDER BY v.term,t.{owner},v.col,v.offset")).unwrap();
    let result = statement
        .query_map([], |r| {
            Ok(format!(
                "{:?}",
                (
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?
                )
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    drop(statement);
    c.execute_batch("DROP TABLE temp.external_page_vocab")
        .unwrap();
    result
}
fn hits(c: &Connection, token: &str) -> i64 {
    c.query_row(
        "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH ?1",
        [token],
        |r| r.get(0),
    )
    .unwrap()
}

// Only the missing cells in the frozen eight-cut Page matrix. The seven
// original-layout cells listed in PAGE-MISSING-COVERAGE.md reuse prior receipts.
#[cfg(unix)]
const PAGE_MISSING_CUTS: [&str; 8] = [
    "copy_begun",
    "copy_completed",
    "delta_before_commit",
    "delta_after_commit",
    "complete_before_ack",
    "ack_before_selector",
    "selector_before_retire",
    "retired",
];
#[cfg(unix)]
fn page_cell_reuses_receipt(retained: bool, kill: bool, cut: &str) -> bool {
    !retained
        && if kill {
            matches!(cut, "delta_before_commit" | "ack_before_selector")
        } else {
            matches!(
                cut,
                "copy_begun"
                    | "delta_before_commit"
                    | "delta_after_commit"
                    | "complete_before_ack"
                    | "ack_before_selector"
            )
        }
}

#[cfg(unix)]
struct PageCut {
    cut: String,
    kill: bool,
    witness: std::path::PathBuf,
    candidate: String,
    predecessor: String,
    source_pages: u64,
    steps: std::sync::atomic::AtomicU64,
    reached: std::sync::atomic::AtomicBool,
}
#[cfg(unix)]
struct PageFailureEvidence(std::path::PathBuf);
#[cfg(unix)]
impl Drop for PageFailureEvidence {
    fn drop(&mut self) {
        if std::thread::panicking() && self.0.exists() {
            let preserved = self.0.with_extension("page-failed");
            match fs::rename(&self.0, &preserved) {
                Ok(()) => eprintln!("PAGE_FAILED_FIXTURE {}", preserved.display()),
                Err(error) => eprintln!("PAGE_FAILED_FIXTURE preservation failed: {error}"),
            }
        }
    }
}
#[cfg(unix)]
impl PageCut {
    fn reach(&self, hook: &str) -> crate::domain::Result<()> {
        use std::sync::atomic::Ordering::SeqCst;
        assert!(
            !self.reached.swap(true, SeqCst),
            "Page cut reached more than once"
        );
        fs::write(
            &self.witness,
            serde_json::to_vec(&json!({
                "cut":self.cut,"mechanism":if self.kill {"SIGKILL"} else {"error"},
                "hook":hook,"candidate":self.candidate,"predecessor":self.predecessor,
                "source_pages":self.source_pages,"copy_steps":self.steps.load(SeqCst),
                "reached":true
            }))
            .unwrap(),
        )
        .unwrap();
        if self.kill {
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
            panic!("SIGKILL returned");
        }
        Err(WikiError::new(
            ErrorCode::Internal,
            "reached missing Page durability cut",
        ))
    }
}
#[cfg(unix)]
impl BuildFault for PageCut {
    fn check(&self, point: BuildCheckpoint) -> crate::domain::Result<()> {
        use std::sync::atomic::Ordering::SeqCst;
        let step = if point == BuildCheckpoint::AfterCopyStep {
            self.steps.fetch_add(1, SeqCst) + 1
        } else {
            self.steps.load(SeqCst)
        };
        let reached = match self.cut.as_str() {
            "copy_begun" => point == BuildCheckpoint::AfterCopyStep && step == 1,
            "copy_completed" => {
                point == BuildCheckpoint::AfterCopyStep && step == self.source_pages.div_ceil(256)
            }
            "delta_before_commit" => point == BuildCheckpoint::BeforeBatchCommit,
            "delta_after_commit" => point == BuildCheckpoint::AfterBatchCheckpoint,
            "complete_before_ack" => point == BuildCheckpoint::AfterSync,
            _ => false,
        };
        if reached {
            self.reach(&format!("{point:?}"))
        } else {
            Ok(())
        }
    }
}

// A Page-specific retirement adapter: all unrelated I/O delegates unchanged.
// It interrupts only the directory sync immediately after the real old DB unlink.
#[cfg(unix)]
struct PageRetireIo {
    native: crate::vault::NativeIo,
    old_database: std::path::PathBuf,
    removed: std::sync::atomic::AtomicBool,
    cut: Arc<PageCut>,
}
#[cfg(unix)]
impl crate::vault::DurableIo for PageRetireIo {
    fn create_stage(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        self.native.create_stage(p)
    }
    fn create_private_stage(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        self.native.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        self.native.create_private_directory(p)
    }
    fn open_append(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        self.native.open_append(p)
    }
    fn truncate_file(&self, f: &std::fs::File, n: u64) -> std::io::Result<()> {
        self.native.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut std::fs::File, b: &[u8]) -> std::io::Result<()> {
        self.native.write_stage(f, b)
    }
    fn sync_file(&self, f: &std::fs::File) -> std::io::Result<()> {
        self.native.sync_file(f)
    }
    fn replace(&self, a: &std::path::Path, b: &std::path::Path) -> std::io::Result<()> {
        self.native.replace(a, b)
    }
    fn create_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        self.native.create_directory(p)
    }
    fn remove_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        self.native.remove_directory(p)
    }
    fn remove(&self, p: &std::path::Path) -> std::io::Result<()> {
        self.native.remove(p)?;
        if p == self.old_database {
            self.removed
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    }
    fn sync_directory(&self, p: &std::path::Path) -> std::io::Result<crate::vault::DirectorySync> {
        if self.removed.load(std::sync::atomic::Ordering::SeqCst)
            && Some(p) == self.old_database.parent()
        {
            self.cut
                .reach("retire:old_database_removed:directory_sync_entered")
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        self.native.sync_directory(p)
    }
}

#[cfg(unix)]
#[test]
fn external_page_missing_durability_boundaries_both_layouts() {
    use std::os::unix::process::ExitStatusExt;
    // Serial, bounded children; no output pipes or helper process survives a failure.
    let owner = std::time::Instant::now();
    let mut executed = 0;
    for retained in [false, true] {
        for kill in [false, true] {
            for cut in PAGE_MISSING_CUTS {
                if page_cell_reuses_receipt(retained, kill, cut) {
                    continue;
                }
                assert!(
                    owner.elapsed() < Duration::from_secs(600),
                    "Page matrix owner exhausted"
                );
                let filler = if cut.starts_with("copy_") {
                    "unrelatedfiller ".repeat(150_000).into_bytes()
                } else {
                    b"unrelated filler\n".to_vec()
                };
                let f = Fixture::new(
                    retained,
                    &[
                        (
                            "control.md",
                            page("page_control", json!({}), "retiredtoken\n"),
                        ),
                        ("large.md", filler.clone()),
                    ],
                );
                f.write("unfamiliar.bin", b"unfamiliar bytes must remain\n");
                let old = f.reader();
                let previous = old.snapshot().clone();
                let previous_tables = logical_tables(old.connection());
                let previous_authority = f
                    .catalog
                    .operation_state()
                    .unwrap()
                    .unwrap()
                    .publication()
                    .clone();
                let marker = fs::read(f.temp.path().join("WIKI.md")).unwrap();
                let previous_selection = selector::maintenance_header(
                    f.catalog.fs(),
                    f.catalog.vault_id(),
                    Duration::ZERO,
                )
                .unwrap()
                .unwrap()
                .0;
                // Actual retirement must not remain blocked by the parent's old-reader lease.
                let old = if cut == "retired" {
                    drop(old);
                    None
                } else {
                    Some(old)
                };
                let Fixture {
                    temp,
                    catalog,
                    writer,
                } = f;
                drop(writer);
                let failure_evidence = PageFailureEvidence(temp.path().to_path_buf());
                let witness = temp.path().join("page-cut-witness.json");
                let log = std::fs::File::create(temp.path().join("page-cut-child.log")).unwrap();
                let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact","catalog::maintenance_page_delta::tests::external_page_missing_durability_child",
                        "--nocapture","--test-threads=1"])
                    .env("LWIKI_PAGE_MISSING_VAULT",temp.path())
                    .env("LWIKI_PAGE_MISSING_CUT",cut)
                    .env("LWIKI_PAGE_MISSING_KILL",if kill {"1"} else {"0"})
                    .stdout(log.try_clone().unwrap()).stderr(log)
                    .spawn().unwrap();
                let pid = child.id();
                let started = std::time::Instant::now();
                let outcome = loop {
                    let log_bytes = match fs::metadata(temp.path().join("page-cut-child.log")) {
                        Ok(meta) => meta.len(),
                        Err(error) => {
                            let _ = child.kill();
                            let reaped = child.wait();
                            panic!(
                                "Page child pid={pid} cut={cut} log monitoring failed: {error}; reaped={reaped:?}"
                            );
                        }
                    };
                    if log_bytes > 1024 * 1024 {
                        let _ = child.kill();
                        let reaped = child.wait();
                        panic!(
                            "Page child pid={pid} cut={cut} exceeded 1MiB log; reaped={reaped:?}"
                        );
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => break status,
                        Ok(None)
                            if started.elapsed() < Duration::from_secs(30)
                                && owner.elapsed() < Duration::from_secs(600) =>
                        {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        result => {
                            let _ = child.kill();
                            let reaped = child.wait();
                            panic!(
                                "Page child pid={pid} cut={cut} timed out/failed wait: {result:?}; reaped={reaped:?}"
                            );
                        }
                    }
                };
                assert!(
                    fs::metadata(temp.path().join("page-cut-child.log"))
                        .unwrap()
                        .len()
                        <= 1024 * 1024
                );
                if kill {
                    assert_eq!(outcome.signal(), Some(libc::SIGKILL), "cut={cut}");
                } else {
                    assert!(
                        outcome.success(),
                        "Page error child failed: cut={cut}, log={}",
                        temp.path().join("page-cut-child.log").display()
                    );
                }
                let observed: Value = serde_json::from_slice(&fs::read(&witness).unwrap()).unwrap();
                assert_eq!(observed["reached"], true);
                assert_eq!(observed["cut"], cut);
                assert_eq!(
                    observed["mechanism"],
                    if kill { "SIGKILL" } else { "error" }
                );
                assert_eq!(observed["predecessor"], previous_selection.file_id);
                if cut.starts_with("copy_") {
                    let pages = observed["source_pages"].as_u64().unwrap();
                    assert!(pages > 256);
                    assert_eq!(
                        observed["copy_steps"],
                        if cut == "copy_begun" {
                            1
                        } else {
                            pages.div_ceil(256)
                        }
                    );
                }
                let writer = WriterPermit::acquire(catalog.fs().root(), Duration::ZERO).unwrap();
                // Move the failure guard after the reconstructed Fixture so it
                // retains the evidence before TempDir's unwinding cleanup.
                let failure_root = failure_evidence.0.clone();
                drop(failure_evidence);
                let f = Fixture {
                    temp,
                    catalog,
                    writer,
                };
                let _failure_evidence = PageFailureEvidence(failure_root);
                let authority = crate::changes::operation_authority::load(
                    f.catalog.fs(),
                    f.catalog.vault_id(),
                    crate::changes::operation_authority::Presence::Required,
                )
                .unwrap()
                .unwrap();
                let candidate = observed["candidate"].as_str().unwrap();
                let acknowledged = matches!(
                    cut,
                    "ack_before_selector" | "selector_before_retire" | "retired"
                );
                assert_eq!(
                    authority.publication().file_id,
                    if acknowledged {
                        candidate
                    } else {
                        &previous_selection.file_id
                    }
                );
                if let Some(old) = &old {
                    assert_eq!(old.snapshot(), &previous);
                    let old_tables = logical_tables(old.connection());
                    assert_eq!(
                        old_tables.keys().collect::<Vec<_>>(),
                        previous_tables.keys().collect::<Vec<_>>()
                    );
                    for (table, rows) in &previous_tables {
                        assert_rows_equal(table, &old_tables[table], rows);
                    }
                    assert_eq!(hits(old.connection(), "retiredtoken"), 1);
                    assert!(old.document(&path("control.md")).unwrap().is_some());
                }
                if cut == "ack_before_selector" {
                    assert!(
                        f.catalog
                            .cached_query_snapshot(QueryReadLimits::default())
                            .is_err()
                    );
                } else if matches!(cut, "selector_before_retire" | "retired") {
                    assert_eq!(
                        f.reader().snapshot().publication().unwrap().file_id,
                        candidate
                    );
                    assert!(f.reader().document(&path("control.md")).unwrap().is_none());
                } else {
                    assert_eq!(f.reader().snapshot(), &previous);
                }
                assert!(!f.temp.path().join("control.md").exists());
                assert_eq!(fs::read(f.temp.path().join("WIKI.md")).unwrap(), marker);
                assert!(
                    fs::read(f.temp.path().join("large.md")).unwrap() == filler,
                    "unchanged canonical filler changed"
                );
                assert_eq!(
                    fs::read(f.temp.path().join("unfamiliar.bin")).unwrap(),
                    b"unfamiliar bytes must remain\n"
                );
                let recovered = f.sync();
                if acknowledged {
                    assert_eq!(
                        recovered.report.snapshot.publication().unwrap().file_id,
                        candidate
                    );
                    if cut == "ack_before_selector" {
                        assert!(recovered.resumed);
                    }
                }
                assert!(f.reader().document(&path("control.md")).unwrap().is_none());
                assert_eq!(hits(f.reader().connection(), "retiredtoken"), 0);
                f.oracle();
                let recovered_snapshot = f.reader().snapshot().clone();
                let repeated = f.sync();
                assert!(repeated.report.reused);
                assert_eq!(repeated.report.snapshot, recovered_snapshot);
                drop(old);
                if matches!(cut, "selector_before_retire" | "retired")
                    && f.temp
                        .path()
                        .join(format!(
                            ".wiki/cache/catalogs/{}.sqlite",
                            previous_selection.file_id
                        ))
                        .exists()
                {
                    assert!(
                        selector::retire(
                            f.catalog.fs(),
                            &f.writer,
                            f.catalog.vault_id(),
                            &previous_selection,
                            Duration::ZERO
                        )
                        .unwrap()
                    );
                }
                assert!(
                    fs::read(f.temp.path().join("large.md")).unwrap() == filler,
                    "unchanged canonical filler changed"
                );
                assert_eq!(fs::read(f.temp.path().join("WIKI.md")).unwrap(), marker);
                assert_eq!(
                    fs::read(f.temp.path().join("unfamiliar.bin")).unwrap(),
                    b"unfamiliar bytes must remain\n"
                );
                println!(
                    "PAGE_MISSING_CELL {}",
                    json!({"layout":if retained {"retained"} else {"original"},
                    "cut":cut,"mechanism":if kill {"SIGKILL"} else {"error"},"pid":pid,"reaped":true,
                    "cell_after_launch_ms":started.elapsed().as_millis(),"witness":observed,
                    "authority_before":previous_authority,"authority_after_cut":authority.publication(),
                    "authority_after_recovery":f.catalog.operation_state().unwrap().unwrap().publication(),
                    "selector_before":previous_selection,"recovered_snapshot":recovered_snapshot,
                    "authority_expected":true,"canonical_preserved":true,"unfamiliar_preserved":true,
                    "unchanged_canonical_hash":Blake3Hash::digest(&filler),
                    "vault_marker_hash":Blake3Hash::digest(&marker),
                    "unfamiliar_hash":Blake3Hash::digest(b"unfamiliar bytes must remain\n"),
                    "logical_tables_fts_equal":true,"repeated_recovery_idempotent":true})
                );
                executed += 1;
            }
        }
    }
    assert_eq!(
        executed, 25,
        "25 missing cells plus seven source-qualified prior cells"
    );
    assert!(
        owner.elapsed() < Duration::from_secs(600),
        "Page matrix owning interval exhausted"
    );
}

#[cfg(unix)]
#[test]
fn external_page_missing_durability_child() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
    let Some(root) = std::env::var_os("LWIKI_PAGE_MISSING_VAULT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let name = std::env::var("LWIKI_PAGE_MISSING_CUT").unwrap();
    assert!(PAGE_MISSING_CUTS.contains(&name.as_str()));
    let native_fs = VaultFs::new(VaultRoot::explicit(&root).unwrap());
    let native_catalog = Catalog::new(native_fs.clone(), id("vault_external_pages"));
    let previous_selection =
        selector::maintenance_header(&native_fs, native_catalog.vault_id(), Duration::ZERO)
            .unwrap()
            .unwrap()
            .0;
    let previous = native_catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let pages = u64::try_from(
        previous
            .connection()
            .pragma_query_value(None, "page_count", |r| r.get::<_, i64>(0))
            .unwrap(),
    )
    .unwrap();
    if name.starts_with("copy_") {
        assert!(pages > 256);
    }
    let selection = CatalogSelection::new(
        native_catalog.vault_id().clone(),
        previous.snapshot().generation + 1,
    )
    .unwrap();
    let fault = Arc::new(PageCut {
        cut: name.clone(),
        kill: std::env::var("LWIKI_PAGE_MISSING_KILL").unwrap() == "1",
        witness: root.join("page-cut-witness.json"),
        candidate: selection.file_id.clone(),
        predecessor: previous_selection.file_id.clone(),
        source_pages: pages,
        steps: AtomicU64::new(0),
        reached: AtomicBool::new(false),
    });
    let retire_io = Arc::new(PageRetireIo {
        native: crate::vault::NativeIo,
        old_database: root.join(format!(
            ".wiki/cache/catalogs/{}.sqlite",
            previous_selection.file_id
        )),
        removed: AtomicBool::new(false),
        cut: fault.clone(),
    });
    let handle = if name == "retired" {
        VaultFs::with_io(VaultRoot::explicit(&root).unwrap(), retire_io.clone())
    } else {
        native_fs
    };
    let catalog = Catalog::new(handle, id("vault_external_pages"));
    let writer = WriterPermit::acquire(catalog.fs().root(), Duration::ZERO).unwrap();
    let old = previous.document(&path("control.md")).unwrap().unwrap();
    let delta = write_projection::project_external_pages(
        catalog.fs(),
        &previous,
        &[(path("control.md"), Some(old.clone()), None)],
        &RefreshProjectionLimits::default(),
    )
    .unwrap();
    fs::remove_file(root.join("control.md")).unwrap();
    let identity = BuildIdentity {
        selection: selection.clone(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: true,
    };
    selector::prepare(catalog.fs(), &writer, &selection).unwrap();
    let result = normalized_build::copy_selected_and_apply(
        catalog.fs(),
        &writer,
        identity,
        previous.connection(),
        previous.snapshot(),
        &delta,
        &[(
            path("control.md"),
            ExpectedState::Hash(old.hash),
            ExpectedState::Absent,
        )],
        BuildLimits {
            fault: Some(fault.clone()),
            ..Default::default()
        },
    );
    if !matches!(
        name.as_str(),
        "ack_before_selector" | "selector_before_retire" | "retired"
    ) {
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("Page build cut was not reached"),
        };
        assert_eq!(error.code, ErrorCode::Internal);
    } else {
        let completed = result.unwrap().0;
        let published = selector::publish_with_faults(
            catalog.fs(),
            &writer,
            &completed.identity.selection,
            Duration::ZERO,
            &mut |point| {
                if (name == "ack_before_selector" && point == selector::PublishPoint::BeforeMarker)
                    || (name == "selector_before_retire"
                        && point == selector::PublishPoint::SelectorDurable)
                {
                    fault.reach(&format!("{point:?}"))
                } else {
                    Ok(())
                }
            },
        );
        if name == "retired" {
            published.unwrap();
            assert!(
                !selector::retire(
                    catalog.fs(),
                    &writer,
                    catalog.vault_id(),
                    &previous_selection,
                    Duration::ZERO
                )
                .unwrap(),
                "pinned reader must defer predecessor retirement"
            );
            assert!(!retire_io.removed.load(SeqCst));
            assert!(!fault.reached.load(SeqCst));
            drop(previous);
            assert!(
                selector::retire(
                    catalog.fs(),
                    &writer,
                    catalog.vault_id(),
                    &previous_selection,
                    Duration::ZERO
                )
                .is_err()
            );
            assert!(
                retire_io.removed.load(SeqCst),
                "actual owned database unlink reached"
            );
        } else {
            assert_eq!(published.unwrap_err().code, ErrorCode::Internal);
        }
    }
    assert!(
        fault.reached.load(SeqCst),
        "requested Page fault hook was not reached"
    );
}

#[test]
fn external_page_create_edit_delete_restore_complete_tables_and_fts_both_layouts() {
    for retained in [false, true] {
        let original = page(
            "page_control",
            json!({"aliases":["Control alias"]}),
            "# Heading\n\noldtoken fact\n",
        );
        let f = Fixture::new(
            retained,
            &[
                ("control.md", original.clone()),
                (
                    "dependent.md",
                    page(
                        "page_dependent",
                        json!({"wiki_depends_on_ids":["page_control"]}),
                        "[[control]] [[page_control]]\n",
                    ),
                ),
                (
                    "transitive.md",
                    page(
                        "page_transitive",
                        json!({"wiki_depends_on_ids":["page_dependent"]}),
                        "Transitive dependent\n",
                    ),
                ),
                (
                    "incoming.md",
                    page(
                        "page_incoming",
                        json!({}),
                        "Incoming [[control|Control]] and [[page_control]]\n",
                    ),
                ),
            ],
        );
        let old = f.reader();
        let old_snapshot = old.snapshot().clone();
        f.write(
            "created.md",
            &page("page_created", json!({}), "newlycreatedtoken\n"),
        );
        let created = f.sync();
        assert!(created.build.is_none());
        assert_eq!(created.page_sync.as_ref().unwrap().pages_created, 1);
        assert_ne!(
            old_snapshot.publication().unwrap().file_id,
            created.report.snapshot.publication().unwrap().file_id
        );
        assert!(created.retirement_deferred);
        assert!(old.document(&path("created.md")).unwrap().is_none());
        f.oracle();
        let physical = f.catalog.fs().root().resolve(&path("control.md")).unwrap();
        let timestamp = fs::metadata(&physical).unwrap().modified().unwrap();
        let edited = page(
            "page_control",
            json!({"aliases":["Control alias"]}),
            "# Heading\n\nnewtoken fact\n",
        );
        assert_eq!(edited.len(), original.len());
        f.write("control.md", &edited);
        fs::File::options()
            .write(true)
            .open(&physical)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(timestamp))
            .unwrap();
        let report = f.sync();
        assert!(report.build.is_none());
        assert_eq!(report.page_sync.unwrap().pages_edited, 1);
        let current = f.reader();
        assert_eq!(hits(current.connection(), "oldtoken"), 0);
        assert_eq!(hits(current.connection(), "newtoken"), 1);
        assert_eq!(hits(old.connection(), "oldtoken"), 1);
        drop(current);
        f.oracle();
        f.delete("control.md");
        let deleted = f.sync();
        assert!(deleted.build.is_none());
        let stats = deleted.page_sync.unwrap();
        assert_eq!(stats.pages_deleted, 1);
        assert!(stats.copy_pages > 0);
        assert!(stats.copy_bytes > 0);
        let current = f.reader();
        assert!(current.document(&path("control.md")).unwrap().is_none());
        assert!(current.record(&id("page_control")).unwrap().is_none());
        assert_eq!(hits(current.connection(), "newtoken"), 0);
        for table in ["records", "record_eligibility_facts", "registry_match_keys"] {
            let column = if table == "records" {
                "id"
            } else {
                "record_id"
            };
            assert_eq!(
                current
                    .connection()
                    .query_row(
                        &format!("SELECT count(*) FROM {table} WHERE {column}='page_control'"),
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
        }
        assert!(old.document(&path("control.md")).unwrap().is_some());
        drop(current);
        f.oracle();
        f.write("control.md", &original);
        let restored = f.sync();
        assert!(restored.build.is_none());
        assert_eq!(restored.page_sync.unwrap().pages_created, 1);
        f.oracle();
        let reused = f.sync();
        assert!(reused.report.reused);
        assert!(reused.page_sync.is_none());
        assert_eq!(reused.report.snapshot, restored.report.snapshot);
        let header =
            selector::maintenance_header(f.catalog.fs(), f.catalog.vault_id(), Duration::ZERO)
                .unwrap()
                .unwrap()
                .1;
        assert!(header.audit.is_none());
        assert!(header.origin.is_none());
        drop(old);
    }
}

#[test]
fn external_page_group_and_deleted_outgoing_links_match_complete_reconstruction() {
    let f = Fixture::new(
        false,
        &[
            (
                "first.md",
                page(
                    "page_first",
                    json!({"wiki_depends_on_ids":["page_second"]}),
                    "[[second]]\n",
                ),
            ),
            (
                "second.md",
                page("page_second", json!({}), "[[first]] secondtoken\n"),
            ),
            (
                "incoming.md",
                page("page_group_incoming", json!({}), "[[first]] [[second]]\n"),
            ),
        ],
    );
    f.delete("first.md");
    f.delete("second.md");
    f.write("third.md", &page("page_third", json!({}), "thirdtoken\n"));
    let report = f.sync();
    let stats = report.page_sync.unwrap();
    assert_eq!((stats.pages_created, stats.pages_deleted), (1, 2));
    assert!(report.build.is_none());
    f.oracle();
}

#[test]
fn external_page_duplicate_identity_non_page_and_large_groups_use_full_reconstruction() {
    let f = Fixture::new(
        false,
        &[(
            "control.md",
            page("page_control", json!({}), "beforetoken\n"),
        )],
    );
    f.write(
        "duplicate.md",
        &page("page_control", json!({}), "collision\n"),
    );
    let report = f.sync();
    assert!(report.page_sync.is_none());
    assert!(report.build.is_some());
    f.oracle();
    f.delete("duplicate.md");
    f.sync();
    f.write(
        "control.md",
        &page("page_control", json!({}), "aftertoken\n"),
    );
    f.write("plain.md", b"ordinary non-Page input\n");
    let report = f.sync();
    assert!(report.page_sync.is_none());
    assert!(report.build.is_some());
    f.oracle();
    for n in 0..17 {
        f.write(
            &format!("new-{n}.md"),
            &page(&format!("page_extra_{n}"), json!({}), "New Page\n"),
        );
    }
    let report = f.sync();
    assert!(report.page_sync.is_none());
    assert!(report.build.is_some());
    f.oracle();
}

#[test]
fn external_page_corrupt_or_missing_cached_before_image_reconstructs_automatically() {
    for corrupt in [false, true] {
        let f = Fixture::new(
            false,
            &[(
                "control.md",
                page("page_control", json!({}), "retiredtoken\n"),
            )],
        );
        let connection = Connection::open(f.selected()).unwrap();
        // Preserve the official sidecar lifecycle while injecting row damage;
        // closing an unconfigured SQLite writer otherwise removes the WAL.
        selector::configure_wal(&connection).unwrap();
        if corrupt {
            connection
                .execute(
                    "UPDATE documents SET raw_text='corrupt beforeimage' WHERE path='control.md'",
                    [],
                )
                .unwrap();
        } else {
            connection
                .execute("DELETE FROM documents WHERE path='control.md'", [])
                .unwrap();
        }
        drop(connection);
        f.delete("control.md");
        let report = f.sync();
        assert!(report.page_sync.is_none());
        assert!(report.build.is_some());
        f.oracle();
    }
}

struct Race {
    target: std::path::PathBuf,
    bytes: Vec<u8>,
    point: BuildCheckpoint,
}
impl BuildFault for Race {
    fn check(&self, point: BuildCheckpoint) -> crate::domain::Result<()> {
        if point == self.point {
            fs::write(&self.target, &self.bytes).unwrap();
        }
        Ok(())
    }
}
#[test]
fn external_page_final_membership_and_exact_byte_recheck_preserves_predecessor() {
    for added in [false, true] {
        let original = page("page_control", json!({}), "beforetoken\n");
        let f = Fixture::new(false, &[("control.md", original.clone())]);
        let previous = f.reader().snapshot().clone();
        f.delete("control.md");
        let input = MaintenanceInput::capture(
            f.catalog.fs(),
            f.catalog.vault_id(),
            MaintenanceLimits::rebuild(),
        )
        .unwrap();
        let comparison = super::super::maintenance_match::compare(&f.catalog, &input).unwrap();
        let super::super::maintenance_match::Comparison::Changed {
            base,
            paths,
            page_only: true,
        } = comparison
        else {
            panic!("Page deletion classification")
        };
        let target = if added { "added.md" } else { "control.md" };
        let error = reconcile(
            &f.catalog,
            &f.writer,
            &input,
            &base,
            &paths,
            BuildLimits {
                fault: Some(Arc::new(Race {
                    target: f.temp.path().join(target),
                    bytes: original.clone(),
                    point: BuildCheckpoint::AfterSync,
                })),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert_eq!(f.reader().snapshot(), &previous);
        assert!(f.reader().document(&path("control.md")).unwrap().is_some());
        let directory = f.temp.path().join(".wiki/cache/catalogs");
        let files: Vec<_> = fs::read_dir(directory)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "sqlite"))
            .collect();
        assert_eq!(files.len(), 1, "unpublished sibling cleaned");
    }
}

struct FailAt(BuildCheckpoint);
impl BuildFault for FailAt {
    fn check(&self, point: BuildCheckpoint) -> crate::domain::Result<()> {
        if point == self.0 {
            Err(WikiError::new(
                ErrorCode::Internal,
                "injected Page sibling cut",
            ))
        } else {
            Ok(())
        }
    }
}
#[test]
fn external_page_copy_sql_complete_seal_cuts_never_switch_predecessor() {
    for point in [
        BuildCheckpoint::AfterHeader,
        BuildCheckpoint::AfterOrdinaryRow,
        BuildCheckpoint::AfterFtsRow,
        BuildCheckpoint::BeforeComplete,
        BuildCheckpoint::AfterComplete,
        BuildCheckpoint::BeforeBatchCommit,
        BuildCheckpoint::AfterBatchCheckpoint,
        BuildCheckpoint::BeforeSeal,
        BuildCheckpoint::BeforeSync,
        BuildCheckpoint::AfterSync,
    ] {
        let f = Fixture::new(
            false,
            &[(
                "control.md",
                page("page_control", json!({}), "retiredtoken\n"),
            )],
        );
        let previous = f.reader().snapshot().clone();
        f.delete("control.md");
        let input = MaintenanceInput::capture(
            f.catalog.fs(),
            f.catalog.vault_id(),
            MaintenanceLimits::rebuild(),
        )
        .unwrap();
        let super::super::maintenance_match::Comparison::Changed { base, paths, .. } =
            super::super::maintenance_match::compare(&f.catalog, &input).unwrap()
        else {
            panic!("changed Page")
        };
        let error = reconcile(
            &f.catalog,
            &f.writer,
            &input,
            &base,
            &paths,
            BuildLimits {
                fault: Some(Arc::new(FailAt(point))),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal, "{point:?}: {error:?}");
        assert_eq!(f.reader().snapshot(), &previous, "{point:?}");
        let finished = f.sync();
        assert!(
            finished.page_sync.is_some(),
            "ordinary deletion retries completely"
        );
        f.oracle();
    }
}

#[test]
fn external_page_delta_maintenance_only_and_wal_copy_includes_selected_rows() {
    let f = Fixture::new(
        false,
        &[(
            "control.md",
            page("page_control", json!({}), "retiredtoken\n"),
        )],
    );
    let reader = f.reader();
    let old = reader.document(&path("control.md")).unwrap().unwrap();
    let changes = vec![(path("control.md"), Some(old), None)];
    let delta = write_projection::project_external_pages(
        f.catalog.fs(),
        &reader,
        &changes,
        &RefreshProjectionLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        &delta.documents[0],
        DocumentMutation::DeletePage { .. }
    ));
    let connection = Connection::open(f.selected()).unwrap();
    sql::configure(&connection, 1000, true).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(
        delta.apply(&connection).is_err(),
        "cannot delete from selected complete catalog"
    );
    connection.execute_batch("ROLLBACK").unwrap();
    drop(connection);
    drop(reader);
    // A committed harmless substantive row exists only in WAL when the pinned
    // old reader prevents truncation. The copy must retain it exactly.
    let old = f.reader();
    let c = Connection::open(f.selected()).unwrap();
    c.execute("INSERT INTO diagnostics(path,record_id,code,details_json) VALUES('unrelated.md',NULL,'record-invalid','{\"wal_witness\":true}')",[]).unwrap();
    drop(c);
    f.delete("control.md");
    let report = f.sync();
    assert!(report.page_sync.is_some());
    let current = f.reader();
    assert_eq!(
        current
            .connection()
            .query_row(
                "SELECT count(*) FROM diagnostics WHERE details_json LIKE '%wal_witness%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        old.connection()
            .query_row(
                "SELECT count(*) FROM diagnostics WHERE details_json LIKE '%wal_witness%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn external_page_source_current_and_historical_refs_and_immutable_bytes_survive() {
    for retained in [false, true] {
        let f = Fixture::new(
            retained,
            &[(
                "control.md",
                page("page_control", json!({}), "Page control\n"),
            )],
        );
        let store = SourceStore::new(f.catalog.fs().clone());
        let request = |bytes: &[u8]| CaptureRequest {
            title: "Captured exact source".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "outside/source.txt".into(),
            original: bytes.to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        };
        let first = store
            .plan_capture(request(b"historical exact quote"))
            .unwrap();
        let source = first.source_id.clone();
        let first_revision = first.revision_id.clone();
        for operation in first.draft.unwrap().operations {
            f.write(operation.target.as_str(), &operation.proposed.unwrap());
        }
        let second = store
            .plan_refresh(&source, request(b"current exact quote"))
            .unwrap();
        let second_revision = second.revision_id.clone();
        for operation in second.draft.unwrap().operations {
            f.write(operation.target.as_str(), &operation.proposed.unwrap());
        }
        f.catalog.rebuild_normalized(&f.writer).unwrap();
        let mut immutable = BTreeMap::new();
        let reader = f.reader();
        let mut s=reader.connection().prepare("SELECT path,expected_hash FROM dependencies WHERE path LIKE 'sources/%' ORDER BY path").unwrap();
        for row in s
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
        {
            let (name, hash) = row.unwrap();
            let bytes = fs::read(f.catalog.fs().root().resolve(&path(&name)).unwrap()).unwrap();
            assert_eq!(Blake3Hash::digest(&bytes).as_str(), hash);
            immutable.insert(name, bytes);
        }
        drop(s);
        drop(reader);
        let citation = |revision: RecordId, bytes: &[u8]| {
            CitationRef::Source(SourceSpanRef {
                source_id: source.clone(),
                source_revision: revision,
                span: ByteSpan::new(0, bytes.len() as u64).unwrap(),
                quote_hash: Blake3Hash::digest(bytes),
            })
        };
        let historical = citation(first_revision, b"historical exact quote");
        let current = citation(second_revision, b"current exact quote");
        for removed in [true, false] {
            if removed {
                f.delete("control.md");
            } else {
                f.write(
                    "control.md",
                    &page("page_control", json!({}), "Page control\n"),
                );
            }
            assert!(f.sync().page_sync.is_some());
            f.oracle();
            let view = store.view().unwrap();
            assert_eq!(
                view.verify(&historical, CitationScope::Historical)
                    .unwrap()
                    .quote,
                b"historical exact quote"
            );
            assert_eq!(
                view.verify(&current, CitationScope::Current).unwrap().quote,
                b"current exact quote"
            );
            for (name, bytes) in &immutable {
                assert_eq!(
                    &fs::read(f.catalog.fs().root().resolve(&path(name)).unwrap()).unwrap(),
                    bytes
                );
            }
        }
    }
}

fn copied_deleted_candidate(f: &Fixture) -> super::super::normalized_build::CompletedCatalog {
    let reader = f.reader();
    let base = reader.snapshot().clone();
    let old = reader.document(&path("control.md")).unwrap().unwrap();
    let commitments = vec![(
        path("control.md"),
        ExpectedState::Hash(old.hash.clone()),
        ExpectedState::Absent,
    )];
    let delta = write_projection::project_external_pages(
        f.catalog.fs(),
        &reader,
        &[(path("control.md"), Some(old), None)],
        &RefreshProjectionLimits::default(),
    )
    .unwrap();
    let identity = BuildIdentity {
        selection: CatalogSelection::new(f.catalog.vault_id().clone(), base.generation + 1)
            .unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: true,
    };
    selector::prepare(f.catalog.fs(), &f.writer, &identity.selection).unwrap();
    normalized_build::copy_selected_and_apply(
        f.catalog.fs(),
        &f.writer,
        identity,
        reader.connection(),
        &base,
        &delta,
        &commitments,
        BuildLimits::default(),
    )
    .unwrap()
    .0
}

#[test]
fn external_page_acknowledged_sibling_resumes_exact_publication_or_cache_loss_reconstructs() {
    for lose_candidate in [false, true] {
        let f = Fixture::new(
            false,
            &[(
                "control.md",
                page("page_control", json!({}), "deletedtoken\n"),
            )],
        );
        let old = f.reader();
        f.delete("control.md");
        let completed = copied_deleted_candidate(&f);
        let error = selector::publish_with_faults(
            f.catalog.fs(),
            &f.writer,
            &completed.identity.selection,
            Duration::ZERO,
            &mut |point| {
                if point == selector::PublishPoint::BeforeMarker {
                    Err(WikiError::new(
                        ErrorCode::Internal,
                        "acknowledged Page sibling cut",
                    ))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
        assert!(old.document(&path("control.md")).unwrap().is_some());
        assert!(
            f.catalog
                .cached_query_snapshot(QueryReadLimits::default())
                .is_err(),
            "new reader obeys acknowledged floor"
        );
        if lose_candidate {
            fs::rename(
                f.temp.path().join(".wiki/cache"),
                f.temp.path().join("retained-lost-cache"),
            )
            .unwrap();
        }
        let resumed = if lose_candidate {
            f.catalog.rebuild_normalized(&f.writer).unwrap()
        } else {
            f.sync()
        };
        if !lose_candidate {
            assert!(resumed.resumed);
            assert!(resumed.report.reused);
            assert_eq!(resumed.report.snapshot, completed.snapshot);
        } else {
            assert!(resumed.build.is_some());
            assert!(resumed.report.vector_loss_unknown);
            assert!(resumed.report.snapshot.generation > completed.snapshot.generation);
        }
        assert!(f.reader().document(&path("control.md")).unwrap().is_none());
        f.oracle();
        drop(old);
    }
}

#[test]
fn external_page_populated_compact_units_retire_and_restore_without_provider_calls() {
    let bytes = page(
        "page_control",
        json!({}),
        "# Units\n\nThe exact compact unit body is available.\n",
    );
    let f = Fixture::new(false, &[("control.md", bytes.clone())]);
    let settings = crate::retrieval::spaces::EmbeddingSettings::default();
    let inventory = f
        .catalog
        .prepare_unit_inventory_page(&f.writer, &settings, 128)
        .unwrap();
    assert!(inventory.complete);
    let policy = inventory.policy;
    let reader = f.reader();
    let count: i64 = reader
        .connection()
        .query_row(
            "SELECT count(*) FROM retrieval_units WHERE owner='control.md'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(count > 0);
    drop(reader);
    f.catalog
        .record_unit_owner_dependencies(
            &f.writer,
            &policy,
            f.reader().snapshot(),
            &[(
                path("control.md"),
                vec![crate::changes::ReadDependency {
                    path: path("control.md"),
                    expected: ExpectedState::Hash(Blake3Hash::digest(&bytes)),
                }],
            )],
        )
        .unwrap();
    f.delete("control.md");
    assert!(f.sync().page_sync.is_some());
    let reader = f.reader();
    assert_eq!(
        reader
            .connection()
            .query_row(
                "SELECT count(*) FROM retrieval_units WHERE owner='control.md'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let tombstone: bool = reader
        .connection()
        .query_row(
            "SELECT tombstone FROM unit_owners WHERE owner='control.md'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(tombstone);
    assert_eq!(
        reader
            .connection()
            .query_row(
                "SELECT count(*) FROM unit_owner_dependencies WHERE owner='control.md'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(reader);
    f.write("control.md", &bytes);
    assert!(f.sync().page_sync.is_some());
    let reader = f.reader();
    assert_eq!(
        reader
            .connection()
            .query_row(
                "SELECT count(*) FROM retrieval_units WHERE owner='control.md'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        count
    );
    assert!(
        !reader
            .connection()
            .query_row(
                "SELECT tombstone FROM unit_owners WHERE owner='control.md'",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
    );
}

#[test]
fn external_page_plus_changed_vault_control_use_reconstruction() {
    let f = Fixture::new(
        false,
        &[("control.md", page("page_control", json!({}), "Old Page\n"))],
    );
    f.write("control.md", &page("page_control", json!({}), "New Page\n"));
    // WIKI is canonical but its kind must remain unchanged for Page admission.
    f.write(
        "WIKI.md",
        &note(
            "vault",
            "vault_external_pages",
            json!({}),
            b"Changed vault control\n",
        ),
    );
    let report = f.sync();
    assert!(report.page_sync.is_none());
    assert!(report.build.is_some());
    f.oracle();
}

#[cfg(unix)]
#[test]
fn external_page_sigkill_sql_and_acknowledgment_preserve_recoverable_publication() {
    use std::os::unix::process::ExitStatusExt;
    for cut in ["delta", "ack"] {
        let f = Fixture::new(
            false,
            &[(
                "control.md",
                page("page_control", json!({}), "retiredtoken\n"),
            )],
        );
        let old = f.reader();
        let previous = old.snapshot().clone();
        let Fixture {
            temp,
            catalog,
            writer,
        } = f;
        drop(writer);
        let outcome = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("catalog::maintenance_page_delta::tests::external_page_sigkill_child")
            .arg("--nocapture")
            .env("LWIKI_PAGE_SYNC_KILL_VAULT", temp.path())
            .env("LWIKI_PAGE_SYNC_KILL_CUT", cut)
            .status()
            .unwrap();
        assert_eq!(outcome.signal(), Some(libc::SIGKILL));
        let writer = WriterPermit::acquire(catalog.fs().root(), Duration::ZERO).unwrap();
        let f = Fixture {
            temp,
            catalog,
            writer,
        };
        assert!(old.document(&path("control.md")).unwrap().is_some());
        if cut == "delta" {
            assert_eq!(f.reader().snapshot(), &previous);
        } else {
            assert!(
                f.catalog
                    .cached_query_snapshot(QueryReadLimits::default())
                    .is_err()
            );
        }
        let recovered = f.sync();
        assert!(recovered.report.snapshot.generation > previous.generation);
        assert!(f.reader().document(&path("control.md")).unwrap().is_none());
        f.oracle();
    }
}

#[cfg(unix)]
#[test]
fn external_page_sigkill_child() {
    let Some(root) = std::env::var_os("LWIKI_PAGE_SYNC_KILL_VAULT") else {
        return;
    };
    let cut = std::env::var("LWIKI_PAGE_SYNC_KILL_CUT").unwrap();
    let fs_handle = VaultFs::new(VaultRoot::explicit(std::path::PathBuf::from(root)).unwrap());
    let catalog = Catalog::new(fs_handle, id("vault_external_pages"));
    let writer = WriterPermit::acquire(catalog.fs().root(), Duration::ZERO).unwrap();
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let base = reader.snapshot().clone();
    let old = reader.document(&path("control.md")).unwrap().unwrap();
    fs::remove_file(catalog.fs().root().resolve(&path("control.md")).unwrap()).unwrap();
    let delta = write_projection::project_external_pages(
        catalog.fs(),
        &reader,
        &[(path("control.md"), Some(old.clone()), None)],
        &RefreshProjectionLimits::default(),
    )
    .unwrap();
    struct Kill;
    impl BuildFault for Kill {
        fn check(&self, point: BuildCheckpoint) -> crate::domain::Result<()> {
            if point == BuildCheckpoint::AfterOrdinaryRow {
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGKILL);
                }
            }
            Ok(())
        }
    }
    let identity = BuildIdentity {
        selection: CatalogSelection::new(catalog.vault_id().clone(), base.generation + 1).unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: true,
    };
    selector::prepare(catalog.fs(), &writer, &identity.selection).unwrap();
    let completed = normalized_build::copy_selected_and_apply(
        catalog.fs(),
        &writer,
        identity,
        reader.connection(),
        &base,
        &delta,
        &[(
            path("control.md"),
            ExpectedState::Hash(old.hash),
            ExpectedState::Absent,
        )],
        BuildLimits {
            fault: if cut == "delta" {
                Some(Arc::new(Kill))
            } else {
                None
            },
            ..Default::default()
        },
    )
    .unwrap()
    .0;
    selector::publish_with_faults(
        catalog.fs(),
        &writer,
        &completed.identity.selection,
        Duration::ZERO,
        &mut |point| {
            if point == selector::PublishPoint::BeforeMarker {
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGKILL);
                }
            }
            Ok(())
        },
    )
    .unwrap();
    panic!("requested kill boundary was not reached");
}

#[test]
fn external_page_receipt_policy_removal_restore_and_dependent_decisions_match_full_build() {
    for retained in [false, true] {
        let receipt = format!(
            "```{}\n{{}}\n```\n",
            crate::graph::review_types::GRAPH_REVIEW_FENCE
        );
        let original = page("page_control", json!({}), &receipt);
        let decision = |name: &str, input: &str, output: &str| {
            note(
                "decision",
                name,
                json!({"wiki_status":"active","wiki_action":"accept","wiki_input_ids":[input],"wiki_output_ids":[output],"wiki_created_at":"2026-10-08T00:00:00Z"}),
                b"Retained dependent decision\n",
            )
        };
        let f = Fixture::new(
            retained,
            &[
                ("control.md", original.clone()),
                ("other.md", page("page_other", json!({}), "Other Page\n")),
                (
                    "decision.md",
                    decision("decision_direct", "page_control", "page_other"),
                ),
                (
                    "transitive-decision.md",
                    decision("decision_transitive", "decision_direct", "page_other"),
                ),
            ],
        );
        for after in [
            Some(page("page_control", json!({}), "Cleared receipt\n")),
            None,
            Some(original.clone()),
        ] {
            match after {
                Some(bytes) => f.write("control.md", &bytes),
                None => f.delete("control.md"),
            }
            let report = f.sync();
            assert!(report.page_sync.is_some());
            assert!(report.build.is_none());
            f.oracle();
        }
    }
}

#[test]
fn external_page_assertion_support_opposition_and_managed_source_refresh_before_after() {
    use crate::{
        catalog::{source_projection, source_refresh::IndexedRefreshSession},
        changes::{ChangeEngine, ChangeStatus},
    };
    let f = Fixture::new(
        false,
        &[(
            "control.md",
            page("page_control", json!({}), "Initial Page\n"),
        )],
    );
    let store = SourceStore::new(f.catalog.fs().clone());
    let request = |bytes: &[u8]| CaptureRequest {
        title: "Supporting source".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "outside/support.txt".into(),
        original: bytes.to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: None,
    };
    let source = store
        .plan_capture(request(b"exact assertion supporting quote"))
        .unwrap();
    for operation in source.draft.unwrap().operations {
        f.write(operation.target.as_str(), &operation.proposed.unwrap());
    }
    f.write(
        "entity.md",
        &note(
            "entity",
            "entity_fixture",
            json!({"wiki_status":"active","wiki_entity_type":"component"}),
            b"Entity description\n",
        ),
    );
    for (name, negated) in [("assertion_support", false), ("assertion_opposition", true)] {
        f.write(&format!("{name}.md"),&note("assertion",name,json!({"wiki_status":"accepted","wiki_subject_id":"entity_fixture","wiki_object_id":"entity_fixture","wiki_predicate":"uses","wiki_negated":negated}),b"Assertion\n"));
        let quote = b"exact assertion supporting quote";
        f.write(&format!("evidence-{name}.md"),&note("evidence",&format!("evidence_{name}"),json!({"wiki_status":"active","wiki_assertion_id":name,"wiki_source_id":source.source_id,"wiki_source_revision":source.revision_id,"wiki_stance":"supports","wiki_locator_kind":"utf8-bytes","wiki_span_start":0,"wiki_span_end":quote.len(),"wiki_quote_hash":Blake3Hash::digest(quote)}),&crate::sources::evidence::exact_quote_body(quote,"\n","Fixture").unwrap()));
    }
    let original = page(
        "page_control",
        json!({"wiki_depends_on_ids":["assertion_support"]}),
        "Supported Page [[assertion_support]]\n",
    );
    f.write("control.md", &original);
    f.catalog.rebuild_normalized(&f.writer).unwrap();
    for bytes in [
        b"first refreshed supporting quote".as_slice(),
        b"second refreshed supporting quote".as_slice(),
    ] {
        let reader = f.reader();
        let plan = store
            .plan_refresh_indexed(
                &reader,
                &source.source_id,
                request(bytes),
                None,
                &crate::sources::SourceRefreshLimits::default(),
            )
            .unwrap();
        let projected = source_projection::project_refresh(
            f.catalog.fs(),
            &reader,
            plan,
            &RefreshProjectionLimits::default(),
        )
        .unwrap()
        .unwrap();
        let mut session =
            IndexedRefreshSession::prepare_projected(&f.catalog, &f.writer, projected).unwrap();
        let engine = ChangeEngine::new(f.catalog.fs().clone()).unwrap();
        assert_eq!(
            engine
                .apply_indexed_refresh(&f.writer, &mut session)
                .unwrap()
                .status,
            ChangeStatus::Committed
        );
        drop(session);
        drop(reader);
        f.oracle();
        f.delete("control.md");
        assert!(f.sync().page_sync.is_some());
        f.oracle();
        f.write("control.md", &original);
        assert!(f.sync().page_sync.is_some());
        f.oracle();
    }
}

#[test]
fn external_page_partial_backup_failure_keeps_old_selection_and_next_sync_completes() {
    let filler = "unrelatedfiller ".repeat(150_000);
    let f = Fixture::new(
        false,
        &[
            (
                "control.md",
                page("page_control", json!({}), "retiredtoken\n"),
            ),
            ("large.md", filler.into_bytes()),
        ],
    );
    let previous = f.reader().snapshot().clone();
    let page_count: i64 = f
        .reader()
        .connection()
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    assert!(
        page_count > 256,
        "fault must occur on a partial coherent copy"
    );
    f.delete("control.md");
    let input = MaintenanceInput::capture(
        f.catalog.fs(),
        f.catalog.vault_id(),
        MaintenanceLimits::rebuild(),
    )
    .unwrap();
    let super::super::maintenance_match::Comparison::Changed { base, paths, .. } =
        super::super::maintenance_match::compare(&f.catalog, &input).unwrap()
    else {
        panic!("Page deletion")
    };
    let error = reconcile(
        &f.catalog,
        &f.writer,
        &input,
        &base,
        &paths,
        BuildLimits {
            fault: Some(Arc::new(FailAt(BuildCheckpoint::AfterCopyStep))),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(f.reader().snapshot(), &previous);
    let completed = f.sync();
    assert!(completed.page_sync.is_some());
    assert!(f.reader().document(&path("control.md")).unwrap().is_none());
    f.oracle();
}

#[test]
fn external_page_corrupt_record_json_and_ambiguous_new_owner_rebuild_automatically() {
    for ambiguous in [false, true] {
        let f = Fixture::new(
            false,
            &[(
                "control.md",
                page("page_control", json!({}), "Original Page\n"),
            )],
        );
        let connection = Connection::open(f.selected()).unwrap();
        // Preserve the official sidecar lifecycle while injecting row damage;
        // closing an unconfigured SQLite writer otherwise removes the WAL.
        selector::configure_wal(&connection).unwrap();
        if ambiguous {
            let hash = Blake3Hash::digest(b"cached ghost witness");
            for owner in ["ghost-a.md", "ghost-b.md"] {
                connection.execute("INSERT INTO identity_claims(record_id,path,file_hash,kind) VALUES('page_new_ambiguous',?1,?2,'page')",rusqlite::params![owner,hash.as_str()]).unwrap();
            }
            f.write(
                "created.md",
                &page(
                    "page_new_ambiguous",
                    json!({}),
                    "New unique canonical Page\n",
                ),
            );
        } else {
            connection
                .execute(
                    "UPDATE records SET row_json='malformed cached record' WHERE id='page_control'",
                    [],
                )
                .unwrap();
            f.write(
                "control.md",
                &page("page_control", json!({}), "Externally edited Page\n"),
            );
        }
        drop(connection);
        let report = f.sync();
        assert!(report.page_sync.is_none());
        assert!(report.build.is_some());
        assert!(
            f.reader()
                .document(&path(if ambiguous {
                    "created.md"
                } else {
                    "control.md"
                }))
                .unwrap()
                .is_some()
        );
        f.oracle();
    }
}
