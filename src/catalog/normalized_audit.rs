//! Audit the actual pinned database, using a bounded private native copy.
//! Ordinary-row/canonical equality is the caller's separate responsibility.
use super::{normalized_read::corrupt, normalized_schema, sql};
use crate::domain::{ErrorCode, Result, WikiError};
use rusqlite::{
    Connection,
    backup::{Backup, StepResult},
    limits::Limit,
    types::ValueRef,
};
use serde::Serialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub(crate) struct AuditLimits {
    pub max_copy_bytes: u64,
    pub max_elapsed: Duration,
    pub pages_per_step: u32,
    pub max_backup_steps: u64,
    pub max_vm_steps: u64,
}
impl Default for AuditLimits {
    fn default() -> Self {
        Self {
            max_copy_bytes: 128 * 1024 * 1024,
            max_elapsed: Duration::from_secs(30),
            pages_per_step: 64,
            max_backup_steps: 32768,
            max_vm_steps: 100_000_000,
        }
    }
}
#[derive(Debug, Default, Serialize)]
pub(crate) struct AuditStats {
    pub database_bytes: u64,
    pub copied_pages: u64,
    pub backup_steps: u64,
    pub elapsed_ms: u64,
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}

/// The caller must hold the Selected lease and its already-established read
/// transaction throughout this call and subsequent ordinary-row comparisons.
/// No source settings/handlers are changed and no filesystem scratch is used.
/// The deadline is cooperative: checked between bounded native backup steps
/// and via the private connection's VM progress handler, not a hard OS timeout.
pub(crate) fn validate_index(source: &Connection, limits: &AuditLimits) -> Result<AuditStats> {
    let started = Instant::now();
    if limits.max_copy_bytes < 4096
        || limits.max_copy_bytes > i32::MAX as u64
        || limits.max_elapsed.is_zero()
        || started.checked_add(limits.max_elapsed).is_none()
        || limits.pages_per_step == 0
        || limits.pages_per_step > 256
        || limits.max_backup_steps == 0
        || limits.max_vm_steps < 1000
    {
        return Err(WikiError::new(
            ErrorCode::ConfigInvalid,
            "invalid normalized audit limits",
        ));
    }
    if source.is_autocommit() || !source.is_readonly("main").map_err(sql::sql_error)? {
        return Err(corrupt(
            "normalized audit requires a pinned read-only transaction",
        ));
    }
    let guard = || {
        if started.elapsed() >= limits.max_elapsed {
            Err(budget("normalized audit elapsed limit exceeded"))
        } else {
            Ok(())
        }
    };
    guard()?;
    let page_count: i64 = source
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .map_err(sql::sql_error)?;
    let page_size: i64 = source
        .pragma_query_value(None, "page_size", |r| r.get(0))
        .map_err(sql::sql_error)?;
    if page_count <= 0
        || !(512..=65536).contains(&page_size)
        || !(page_size as u64).is_power_of_two()
    {
        return Err(corrupt("invalid normalized database page geometry"));
    }
    let database_bytes = (page_count as u64)
        .checked_mul(page_size as u64)
        .ok_or_else(|| budget("normalized audit copy byte overflow"))?;
    if database_bytes > limits.max_copy_bytes {
        return Err(budget(
            "normalized audit database exceeds private-copy byte limit",
        ));
    }
    let minimum_steps = (page_count as u64).div_ceil(limits.pages_per_step as u64);
    if minimum_steps > limits.max_backup_steps {
        return Err(budget(
            "normalized audit exceeds backup step admission limit",
        ));
    }
    guard()?;
    let mut copy = Connection::open_in_memory().map_err(sql::sql_error)?;
    copy.busy_timeout(Duration::ZERO).map_err(sql::sql_error)?;
    copy.execute_batch(&format!("PRAGMA page_size={page_size}; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-2048; PRAGMA mmap_size=0;")).map_err(sql::sql_error)?;
    copy.set_limit(Limit::SQLITE_LIMIT_LENGTH, limits.max_copy_bytes as i32)
        .map_err(sql::sql_error)?;
    copy.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 256 * 1024)
        .map_err(sql::sql_error)?;
    let ticks = Arc::new(AtomicU64::new(0));
    let count = ticks.clone();
    let max_ticks = limits.max_vm_steps / 1000;
    let elapsed = limits.max_elapsed;
    copy.progress_handler(
        1000,
        Some(move || {
            started.elapsed() >= elapsed || count.fetch_add(1, Ordering::Relaxed) + 1 >= max_ticks
        }),
    )
    .map_err(sql::sql_error)?;
    let mut stats = AuditStats {
        database_bytes,
        ..AuditStats::default()
    };
    {
        let backup = Backup::new(source, &mut copy).map_err(sql::sql_error)?;
        loop {
            guard()?;
            if stats.backup_steps >= limits.max_backup_steps {
                return Err(budget("normalized audit backup step limit exceeded"));
            }
            stats.backup_steps += 1;
            let result = backup
                .step(limits.pages_per_step as i32)
                .map_err(sql::sql_error)?;
            if matches!(result, StepResult::Busy | StepResult::Locked) {
                return Err(WikiError::new(
                    ErrorCode::LockTimeout,
                    "normalized audit copy was busy or locked; no retry performed",
                ));
            }
            let progress = backup.progress();
            if progress.pagecount != page_count as i32
                || progress.remaining < 0
                || progress.remaining > progress.pagecount
            {
                return Err(corrupt(
                    "normalized audit source snapshot changed during copy",
                ));
            }
            stats.copied_pages = (progress.pagecount - progress.remaining) as u64;
            guard()?;
            match result {
                StepResult::Done => break,
                StepResult::More => {}
                _ => return Err(corrupt("unknown normalized audit backup result")),
            }
        }
    }
    guard()?;
    validate_schema(&copy)?;
    guard()?;
    // Check B-trees, ordinary indexes, CHECK and NOT NULL constraints too.
    let mut statement = copy
        .prepare("PRAGMA integrity_check(1)")
        .map_err(sql::sql_error)?;
    let mut rows = statement.query([]).map_err(sql::sql_error)?;
    let row = rows
        .next()
        .map_err(sql::sql_error)?
        .ok_or_else(|| corrupt("SQLite integrity check returned no result"))?;
    if row.get_ref(0).map_err(sql::sql_error)? != ValueRef::Text(b"ok")
        || rows.next().map_err(sql::sql_error)?.is_some()
    {
        return Err(corrupt("normalized database failed SQLite integrity check"));
    }
    drop(rows);
    drop(statement);
    for (fts, content, rowid) in [
        ("documents_fts", "documents", "doc_row"),
        ("graph_fts", "graph_rows", "graph_row"),
    ] {
        guard()?;
        // Native rank=1 checks expected rows, but an extra zero-token docsize
        // row plus a matching totals count can evade its token checksum.
        let mismatch: bool = copy.query_row(&format!(
            "SELECT EXISTS(SELECT 1 FROM {fts}_docsize d LEFT JOIN {content} c ON d.id=c.{rowid} WHERE c.{rowid} IS NULL) OR EXISTS(SELECT 1 FROM {content} c LEFT JOIN {fts}_docsize d ON d.id=c.{rowid} WHERE d.id IS NULL)"), [], |r| r.get(0)).map_err(sql::sql_error)?;
        if mismatch {
            return Err(corrupt(
                "normalized FTS row membership differs from content",
            ));
        }
        if fts == "graph_fts" {
            // These two columns are UNINDEXED. Native integrity deliberately
            // skips their per-row size comparison. Builder writes zero sizes.
            let mut statement = copy
                .prepare("SELECT sz FROM graph_fts_docsize")
                .map_err(sql::sql_error)?;
            let mut rows = statement.query([]).map_err(sql::sql_error)?;
            while let Some(row) = rows.next().map_err(sql::sql_error)? {
                guard()?;
                let ValueRef::Blob(blob) = row.get_ref(0).map_err(sql::sql_error)? else {
                    return Err(corrupt("normalized graph FTS docsize is not a blob"));
                };
                if !graph_unindexed_sizes_are_zero(blob) {
                    return Err(corrupt(
                        "normalized graph FTS unindexed column sizes are invalid",
                    ));
                }
            }
        }
        copy.execute(
            &format!("INSERT INTO {fts}({fts},rank) VALUES('integrity-check',1)"),
            [],
        )
        .map_err(sql::sql_error)?;
    }
    guard()?;
    stats.elapsed_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    Ok(stats)
}

// Skip the six indexed-column SQLite varints (at most nine bytes each), then
// require the exact two zero bytes emitted for UNINDEXED columns. Indexed
// counts and full docsize decoding remain the native check's responsibility.
fn graph_unindexed_sizes_are_zero(mut blob: &[u8]) -> bool {
    if blob.len() > 72 {
        return false;
    }
    for _ in 0..6 {
        for index in 0..9 {
            let Some((&byte, rest)) = blob.split_first() else {
                return false;
            };
            blob = rest;
            if byte & 0x80 == 0 || index == 8 {
                break;
            }
        }
    }
    blob == [0, 0]
}

type SchemaRow = (String, String, String, Option<String>);
fn schema_rows(connection: &Connection) -> Result<Vec<SchemaRow>> {
    let mut statement = connection
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema")
        .map_err(sql::sql_error)?;
    let mut rows = statement.query([]).map_err(sql::sql_error)?;
    let mut result = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next().map_err(sql::sql_error)? {
        if result.len() >= 128 {
            return Err(corrupt("normalized schema has too many objects"));
        }
        for index in 0..4 {
            match row.get_ref(index).map_err(sql::sql_error)? {
                ValueRef::Text(value) => {
                    bytes = bytes
                        .checked_add(value.len())
                        .ok_or_else(|| corrupt("normalized schema size overflow"))?
                }
                ValueRef::Null if index == 3 => {}
                _ => return Err(corrupt("normalized schema has invalid SQL value type")),
            }
        }
        if bytes > 256 * 1024 {
            return Err(corrupt("normalized schema exceeds byte limit"));
        }
        result.push((
            row.get(0).map_err(sql::sql_error)?,
            row.get(1).map_err(sql::sql_error)?,
            row.get(2).map_err(sql::sql_error)?,
            row.get(3).map_err(sql::sql_error)?,
        ));
    }
    // Bound objects/bytes before sorting, including a hostile source schema.
    result.sort();
    Ok(result)
}
/// Bounded schema/configuration compatibility only; no ordinary rows, postings,
/// backup or SQLite integrity scan. Explicit maintenance may reuse this check.
pub(crate) fn validate_schema(copy: &Connection) -> Result<()> {
    let expected = Connection::open_in_memory().map_err(sql::sql_error)?;
    expected
        .execute_batch(normalized_schema::SCHEMA)
        .map_err(sql::sql_error)?;
    if schema_rows(copy)? != schema_rows(&expected)? || sql::version(copy)? != 3 {
        return Err(corrupt(
            "normalized schema differs from the exact catalog schema",
        ));
    }
    for table in ["documents_fts_config", "graph_fts_config"] {
        let query = format!("SELECT k,v FROM {table} ORDER BY k LIMIT 2");
        let read = |c: &Connection| -> Result<Vec<(String, i64)>> {
            let mut statement = c.prepare(&query).map_err(sql::sql_error)?;
            let mut rows = statement.query([]).map_err(sql::sql_error)?;
            let mut values = Vec::new();
            while let Some(row) = rows.next().map_err(sql::sql_error)? {
                if row.get_ref(0).map_err(sql::sql_error)? != ValueRef::Text(b"version") {
                    return Err(corrupt("unexpected normalized FTS configuration"));
                }
                values.push((
                    row.get(0).map_err(sql::sql_error)?,
                    row.get(1).map_err(sql::sql_error)?,
                ));
            }
            Ok(values)
        };
        if read(copy)? != read(&expected)? {
            return Err(corrupt("normalized FTS configuration differs"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{OpenFlags, params};
    use std::{
        fs,
        path::{Path, PathBuf},
        time::SystemTime,
    };

    fn document(connection: &Connection, id: i64, title: &str, body: &str) {
        connection.execute("INSERT INTO documents(doc_row,path,file_hash,title,aliases_json,aliases_text,headings,tags_json,tags_text,body,raw_text,eligibility,reasons_json) VALUES(?1,?2,'hash',?3,'[]','','','[]','',?4,?4,'current','[]')", params![id,format!("{id}.md"),title,body]).unwrap();
        connection.execute("INSERT INTO documents_fts(rowid,title,aliases,headings,tags,body) VALUES(?1,?2,'','','',?3)", params![id,title,body]).unwrap();
    }
    fn database(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(normalized_schema::SCHEMA).unwrap();
        document(&c, 1, "café title", "alpha beta alpha");
        document(&c, 2, "---", "! ...");
        document(&c, 3, "repeat", "alpha alpha beta gamma");
        c.execute_batch("INSERT INTO graph_rows VALUES(1,'page_a','page','Alice','[]','Bob','endpoint','relates','qualifier','description'); INSERT INTO graph_rows VALUES(2,'page_b','page','','[]','','','','',''); INSERT INTO graph_fts(rowid,name,aliases,endpoints,predicate,qualifiers,description,target_kind,target_id) SELECT graph_row,name,aliases_text,endpoints,predicate,qualifiers,description,target_kind,target_id FROM graph_rows;").unwrap();
    }
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("catalog.sqlite");
        database(&path);
        (temp, path)
    }
    fn pinned(path: &Path) -> Connection {
        let c = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        c.execute_batch("BEGIN; SELECT count(*) FROM sqlite_schema;")
            .unwrap();
        c
    }
    fn mutate(path: &Path, sql: &str) {
        let connection = Connection::open(path).unwrap();
        // Deliberate corruption injection into disposable FTS shadow tables.
        connection
            .set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, false)
            .unwrap();
        connection.execute_batch(sql).unwrap();
    }
    fn state(path: &Path) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
        let mut values = fs::read_dir(path)
            .unwrap()
            .map(|e| {
                let entry = e.unwrap();
                (
                    entry.path(),
                    fs::read(entry.path()).unwrap(),
                    entry.metadata().unwrap().modified().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        values.sort_by(|a, b| a.0.cmp(&b.0));
        values
    }
    fn native_only(source: &Connection) -> rusqlite::Result<()> {
        let mut copy = Connection::open_in_memory()?;
        {
            let backup = Backup::new(source, &mut copy)?;
            assert_eq!(backup.step(-1)?, StepResult::Done);
        }
        copy.execute(
            "INSERT INTO documents_fts(documents_fts,rank) VALUES('integrity-check',1)",
            [],
        )?;
        copy.execute(
            "INSERT INTO graph_fts(graph_fts,rank) VALUES('integrity-check',1)",
            [],
        )?;
        Ok(())
    }
    #[test]
    fn valid_postings_and_zero_token_rows_pass_without_source_writes() {
        let (temp, path) = fixture();
        let before = state(temp.path());
        let source = pinned(&path);
        let old_limit = source
            .set_limit(Limit::SQLITE_LIMIT_LENGTH, 1024 * 1024)
            .unwrap();
        let ticks = Arc::new(AtomicU64::new(0));
        let observed = ticks.clone();
        source
            .progress_handler(
                1,
                Some(move || {
                    observed.fetch_add(1, Ordering::Relaxed);
                    false
                }),
            )
            .unwrap();
        let stats = validate_index(&source, &AuditLimits::default()).unwrap();
        assert!(stats.database_bytes > 0);
        assert!(stats.copied_pages > 0);
        let count = ticks.load(Ordering::Relaxed);
        source
            .query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, i64>(0))
            .unwrap();
        assert!(
            ticks.load(Ordering::Relaxed) > count,
            "source progress handler retained"
        );
        assert_eq!(
            source
                .set_limit(Limit::SQLITE_LIMIT_LENGTH, old_limit)
                .unwrap(),
            1024 * 1024
        );
        assert!(!source.is_autocommit());
        drop(source);
        assert_eq!(before, state(temp.path()));
    }
    #[test]
    fn actual_postings_corruption_matrix() {
        let cases = [
            (
                "deleted postings with intact content",
                "INSERT INTO documents_fts(documents_fts,rowid,title,aliases,headings,tags,body) SELECT 'delete',doc_row,title,aliases_text,headings,tags_text,body FROM documents WHERE doc_row=1;",
                false,
            ),
            (
                "wrong positions same counts",
                "INSERT INTO documents_fts(documents_fts,rowid,title,body) VALUES('delete',1,'café title','alpha beta alpha'); INSERT INTO documents_fts(rowid,title,body) VALUES(1,'café title','alpha alpha beta');",
                false,
            ),
            (
                "wrong column same total",
                "INSERT INTO graph_fts(graph_fts,rowid,name,aliases,endpoints,predicate,qualifiers,description) VALUES('delete',1,'Alice','Bob','endpoint','relates','qualifier','description'); INSERT INTO graph_fts(rowid,name,aliases,endpoints,predicate,qualifiers,description) VALUES(1,'Bob','Alice','endpoint','relates','qualifier','description');",
                false,
            ),
            (
                "missing zero-token row",
                "INSERT INTO documents_fts(documents_fts,rowid,title,body) VALUES('delete',2,'---','! ...');",
                false,
            ),
            (
                "missing graph zero-token row",
                "INSERT INTO graph_fts(graph_fts,rowid) VALUES('delete',2);",
                false,
            ),
            (
                "extra token posting",
                "INSERT INTO documents_fts(rowid,body) VALUES(99,'extra');",
                false,
            ),
            (
                "extra zero-token membership",
                "INSERT INTO documents_fts(rowid,body) VALUES(99,'---');",
                true,
            ),
            (
                "extra graph zero-token membership",
                "INSERT INTO graph_fts(rowid,name) VALUES(99,'---');",
                true,
            ),
            (
                "wrong docsize",
                "UPDATE documents_fts_docsize SET sz=x'0300000003' WHERE id=1;",
                false,
            ),
            (
                "wrong unindexed graph docsize",
                "UPDATE graph_fts_docsize SET sz=x'0101010101010100' WHERE id=1;",
                true,
            ),
            (
                "wrong multibyte unindexed graph docsize",
                "UPDATE graph_fts_docsize SET sz=x'010101010101810000' WHERE id=1;",
                true,
            ),
            (
                "wrong totals",
                "UPDATE documents_fts_data SET block=x'030300000008' WHERE id=1;",
                false,
            ),
            (
                "wrong row totals",
                "UPDATE documents_fts_data SET block=x'040300000007' WHERE id=1;",
                false,
            ),
            (
                "truncated docsize",
                "UPDATE documents_fts_docsize SET sz=x'01' WHERE id=1;",
                false,
            ),
            (
                "graph totals",
                "UPDATE graph_fts_data SET block=x'020101010101010100' WHERE id=1;",
                false,
            ),
        ];
        for (name, mutation, native_blind_spot) in cases {
            let (temp, path) = fixture();
            mutate(&path, mutation);
            let before = state(temp.path());
            let source = pinned(&path);
            assert_eq!(
                native_only(&source).is_ok(),
                native_blind_spot,
                "native coverage changed: {name}"
            );
            let error = validate_index(&source, &AuditLimits::default()).expect_err(name);
            assert_eq!(error.code, ErrorCode::IndexCorrupt, "{name}: {error:?}");
            drop(source);
            assert_eq!(before, state(temp.path()), "source changed: {name}");
        }
    }
    #[test]
    fn exact_schema_and_configuration_are_required() {
        let cases = [
            "CREATE TABLE unexpected(x);",
            "CREATE TRIGGER unexpected AFTER INSERT ON documents BEGIN DELETE FROM graph_rows; END;",
            "DROP INDEX document_record_ids;",
            "DROP VIEW document_fts_content; CREATE VIEW document_fts_content AS SELECT doc_row,title,aliases_text AS aliases,headings,tags_text AS tags,'' AS body FROM documents;",
            "DROP TABLE documents_vocab; CREATE VIRTUAL TABLE documents_vocab USING fts5vocab(documents_fts,'row');",
            "DROP TABLE documents_fts; CREATE VIRTUAL TABLE documents_fts USING fts5(title,aliases,headings,tags,body,content='document_fts_content',content_rowid='doc_row',detail=full,columnsize=1,tokenize='porter');",
            "INSERT INTO documents_fts_config(k,v) VALUES('automerge',0);",
            "UPDATE graph_fts_config SET v=5 WHERE k='version';",
            "PRAGMA user_version=1;",
        ];
        for mutation in cases {
            let (_temp, path) = fixture();
            mutate(&path, mutation);
            assert_eq!(
                validate_index(&pinned(&path), &AuditLimits::default())
                    .unwrap_err()
                    .code,
                ErrorCode::IndexCorrupt,
                "{mutation}"
            );
        }
    }
    #[test]
    fn bounded_copy_admission_and_deadline_fail_without_source_writes() {
        let (temp, path) = fixture();
        // Both admission cases must stop before reaching corrupted schema.
        mutate(&path, "CREATE TABLE unexpected(x);");
        let before = state(temp.path());
        let source = pinned(&path);
        let cases = [
            AuditLimits {
                max_copy_bytes: 4096,
                ..AuditLimits::default()
            },
            AuditLimits {
                pages_per_step: 1,
                max_backup_steps: 1,
                ..AuditLimits::default()
            },
            AuditLimits {
                max_elapsed: Duration::from_nanos(1),
                ..AuditLimits::default()
            },
        ];
        for limits in cases {
            let error = validate_index(&source, &limits).unwrap_err();
            assert_eq!(error.code, ErrorCode::BudgetExceeded, "{error:?}");
        }
        drop(source);
        assert_eq!(before, state(temp.path()));
    }
    #[test]
    fn private_vm_work_limit_interrupts_native_audit() {
        let (_temp, path) = fixture();
        let c = Connection::open(&path).unwrap();
        c.execute_batch("BEGIN").unwrap();
        for id in 4..1004 {
            document(&c, id, "bounded", "alpha beta gamma delta");
        }
        c.execute_batch("COMMIT").unwrap();
        drop(c);
        assert_eq!(
            validate_index(
                &pinned(&path),
                &AuditLimits {
                    max_vm_steps: 1000,
                    ..AuditLimits::default()
                }
            )
            .unwrap_err()
            .code,
            ErrorCode::BudgetExceeded
        );
    }
    #[test]
    fn requires_readonly_pinned_connection_and_valid_limits() {
        let (_temp, path) = fixture();
        let rw = Connection::open(&path).unwrap();
        rw.execute_batch("BEGIN").unwrap();
        assert!(validate_index(&rw, &AuditLimits::default()).is_err());
        let ro = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert!(validate_index(&ro, &AuditLimits::default()).is_err());
        let source = pinned(&path);
        assert_eq!(
            validate_index(
                &source,
                &AuditLimits {
                    pages_per_step: 0,
                    ..AuditLimits::default()
                }
            )
            .unwrap_err()
            .code,
            ErrorCode::ConfigInvalid
        );
    }
    #[test]
    fn copy_uses_pinned_file_after_path_replacement() {
        let (temp, path) = fixture();
        let source = pinned(&path);
        fs::rename(&path, temp.path().join("old.sqlite")).unwrap();
        database(&path);
        mutate(&path, "DELETE FROM documents_fts_docsize;");
        assert!(validate_index(&source, &AuditLimits::default()).is_ok());
        assert!(validate_index(&pinned(&path), &AuditLimits::default()).is_err());
    }
    #[test]
    #[ignore = "bounded 1k private-copy timing experiment"]
    fn bounded_thousand_document_audit_measurement() {
        let (temp, path) = fixture();
        let c = Connection::open(&path).unwrap();
        let body = "alpha beta gamma delta repeated context evidence ".repeat(640);
        c.execute_batch("BEGIN").unwrap();
        for id in 4..1001 {
            document(&c, id, "bounded", &body);
        }
        c.execute_batch("COMMIT").unwrap();
        drop(c);
        let before = state(temp.path());
        let source = pinned(&path);
        let stats = validate_index(&source, &AuditLimits::default()).unwrap();
        assert_eq!(before, state(temp.path()));
        eprintln!(
            "normalized-audit-measurement {}",
            serde_json::to_string(&stats).unwrap()
        );
    }
}
