use super::*;
use crate::{
    catalog::{full_check_types::CheckLimits, normalized_audit::AuditLimits, normalized_schema},
    domain::{Blake3Hash, Eligibility, RecordId, RecordKind, VaultRelativePath},
};
use rusqlite::{OpenFlags, config::DbConfig};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

fn doc(id: i64, title: &str, body: &str) -> DocumentRow {
    DocumentRow {
        path: VaultRelativePath::new(format!("{id}.md")).unwrap(),
        hash: Blake3Hash::digest(body),
        record_id: None,
        kind: None,
        title: title.into(),
        aliases: vec![],
        headings: String::new(),
        tags: vec![],
        body: body.into(),
        raw_text: body.into(),
        source_id: None,
        owner_revision: None,
        eligibility: Eligibility::Current,
        reasons: vec![],
    }
}
fn graph(id: &str, name: &str, aliases: Vec<String>) -> GraphRow {
    GraphRow {
        target_id: RecordId::new(id).unwrap(),
        target_kind: RecordKind::Page,
        name: name.into(),
        aliases,
        endpoints: String::new(),
        predicate: String::new(),
        qualifiers: String::new(),
        description: String::new(),
    }
}
fn insert_doc(c: &Connection, id: i64, row: &DocumentRow) {
    c.execute("INSERT INTO documents(doc_row,path,file_hash,title,aliases_json,aliases_text,headings,tags_json,tags_text,body,raw_text,eligibility,reasons_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'current','[]')",
        params![id,row.path.as_str(),row.hash.as_str(),row.title,serde_json::to_string(&row.aliases).unwrap(),row.aliases.join(" "),row.headings,
            serde_json::to_string(&row.tags).unwrap(),row.tags.join(" "),row.body,row.raw_text]).unwrap();
    c.execute("INSERT INTO documents_fts(rowid,title,aliases,headings,tags,body) VALUES(?1,?2,?3,?4,?5,?6)",
        params![id,row.title,row.aliases.join(" "),row.headings,row.tags.join(" "),row.body]).unwrap();
}
fn insert_graph(c: &Connection, id: i64, row: &GraphRow) {
    c.execute(
        "INSERT INTO graph_rows VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            id,
            row.target_id.as_str(),
            row.target_kind.as_str(),
            row.name,
            serde_json::to_string(&row.aliases).unwrap(),
            row.aliases.join(" "),
            row.endpoints,
            row.predicate,
            row.qualifiers,
            row.description
        ],
    )
    .unwrap();
    c.execute("INSERT INTO graph_fts(rowid,name,aliases,endpoints,predicate,qualifiers,description,target_kind,target_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![id,row.name,row.aliases.join(" "),row.endpoints,row.predicate,row.qualifiers,row.description,row.target_kind.as_str(),row.target_id.as_str()]).unwrap();
}
struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    docs: Vec<(i64, DocumentRow)>,
    graphs: Vec<(i64, GraphRow)>,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.sqlite");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(normalized_schema::SCHEMA).unwrap();
    let docs = vec![
        (1, doc(1, "café title", "alpha beta alpha")),
        (2, doc(2, "---", "! ...")),
        (3, doc(3, "repeat", "alpha alpha beta gamma")),
    ];
    let mut first = graph("page_a", "Alice", vec!["Bob".into()]);
    first.endpoints = "endpoint".into();
    first.predicate = "relates".into();
    first.qualifiers = "qualifier".into();
    first.description = "description".into();
    let graphs = vec![(1, first), (2, graph("page_b", "", vec![]))];
    for (id, row) in &docs {
        insert_doc(&c, *id, row);
    }
    for (id, row) in &graphs {
        insert_graph(&c, *id, row);
    }
    drop(c);
    Fixture {
        _dir: dir,
        path,
        docs,
        graphs,
    }
}
fn pinned(path: &Path, budget: &CheckBudget) -> Connection {
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    budget.configure_sql(&c).unwrap();
    c.execute_batch("BEGIN; SELECT count(*) FROM sqlite_schema;")
        .unwrap();
    c
}
fn fresh_scratch() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let c = Connection::open(dir.path().join("compact.sqlite")).unwrap();
    (dir, c)
}
fn run(
    source: &Connection,
    scratch: &Connection,
    budget: CheckBudget,
    f: &Fixture,
) -> Result<CompactStats> {
    let mut audit = CompactAudit::new(source, scratch, budget)?;
    // Emission order is deliberately different from physical rowid order.
    for (id, row) in f.docs.iter().rev() {
        audit.document(*id, row)?;
    }
    for (id, row) in f.graphs.iter().rev() {
        audit.graph(*id, row)?;
    }
    audit.finish()
}
fn mutate(path: &Path, sql: &str) {
    let c = Connection::open(path).unwrap();
    c.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, false)
        .unwrap();
    c.execute_batch(sql).unwrap();
}
fn state(path: &Path) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
    let mut result: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.path(),
                fs::read(e.path()).unwrap(),
                e.metadata().unwrap().modified().unwrap(),
            )
        })
        .collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

#[test]
fn compact_native_complete_reference_preserves_source_and_has_no_text_table() {
    let f = fixture();
    let before = state(f._dir.path());
    let budget = CheckBudget::new(CheckLimits::default()).unwrap();
    let source = pinned(&f.path, &budget);
    let (_dir, scratch) = fresh_scratch();
    let mut audit = CompactAudit::new(&source, &scratch, budget.clone()).unwrap();
    assert!(
        !scratch.is_autocommit(),
        "root key tables share the reference transaction"
    );
    scratch.execute_batch("CREATE TABLE seen_keys(family TEXT,key INTEGER,PRIMARY KEY(family,key)) WITHOUT ROWID;").unwrap();
    for (id, row) in &f.docs {
        audit.document(*id, row).unwrap();
    }
    for (id, row) in &f.graphs {
        audit.graph(*id, row).unwrap();
    }
    let stats = audit.finish().unwrap();
    assert_eq!(
        (
            stats.documents,
            stats.graph_rows,
            stats.document_docsize_rows,
            stats.graph_docsize_rows
        ),
        (3, 2, 3, 2)
    );
    assert!(stats.document_postings > 0 && stats.graph_postings > 0);
    assert_eq!(budget.stats().rows, 0, "caller owns ordinary-row admission");
    assert!(budget.stats().postings >= 2 * (stats.document_postings + stats.graph_postings + 5));
    assert!(scratch.is_autocommit());
    let fulltext: i64 = scratch.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('documents','graph_rows','documents_fts_content','graph_fts_content')",[],|r|r.get(0)).unwrap();
    assert_eq!(fulltext, 0);
    let text: Option<String> = scratch
        .query_row("SELECT body FROM documents_fts WHERE rowid=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        text, None,
        "contentless reference does not retain source text"
    );
    assert!(!source.is_autocommit());
    assert!(normalized_audit::validate_index(&source, &AuditLimits::default()).is_ok());
    drop(source);
    assert_eq!(before, state(f._dir.path()));
}

#[test]
fn compact_native_corruption_matrix_matches_small_copy_oracle() {
    let cases = [
        "INSERT INTO documents_fts(documents_fts,rowid,title,aliases,headings,tags,body) SELECT 'delete',doc_row,title,aliases_text,headings,tags_text,body FROM documents WHERE doc_row=1;",
        "INSERT INTO documents_fts(documents_fts,rowid,title,body) VALUES('delete',1,'café title','alpha beta alpha'); INSERT INTO documents_fts(rowid,title,body) VALUES(1,'café title','alpha alpha beta');",
        "INSERT INTO graph_fts(graph_fts,rowid,name,aliases,endpoints,predicate,qualifiers,description) VALUES('delete',1,'Alice','Bob','endpoint','relates','qualifier','description'); INSERT INTO graph_fts(rowid,name,aliases,endpoints,predicate,qualifiers,description) VALUES(1,'Bob','Alice','endpoint','relates','qualifier','description');",
        "INSERT INTO documents_fts(documents_fts,rowid,title,body) VALUES('delete',2,'---','! ...');",
        "INSERT INTO graph_fts(graph_fts,rowid) VALUES('delete',2);",
        "INSERT INTO documents_fts(rowid,body) VALUES(99,'extra');",
        "INSERT INTO documents_fts(rowid,body) VALUES(99,'---');",
        "INSERT INTO graph_fts(rowid,name) VALUES(99,'---');",
        "UPDATE documents_fts_docsize SET sz=x'0300000003' WHERE id=1;",
        "UPDATE graph_fts_docsize SET sz=x'0101010101010100' WHERE id=1;",
        "UPDATE graph_fts_docsize SET sz=x'010101010101810000' WHERE id=1;",
        "UPDATE documents_fts_data SET block=x'030300000008' WHERE id=1;",
        "UPDATE documents_fts_data SET block=x'040300000007' WHERE id=1;",
        "UPDATE documents_fts_docsize SET sz=x'01' WHERE id=1;",
        "UPDATE graph_fts_data SET block=x'020101010101010100' WHERE id=1;",
        "UPDATE documents_fts_data SET block=x'00' WHERE id>10;",
        "DELETE FROM documents_fts_data WHERE id=1;",
    ];
    for mutation in cases {
        let f = fixture();
        mutate(&f.path, mutation);
        let before = state(f._dir.path());
        let budget = CheckBudget::new(CheckLimits::default()).unwrap();
        let source = pinned(&f.path, &budget);
        let oracle =
            normalized_audit::validate_index(&source, &AuditLimits::default()).unwrap_err();
        let (_dir, scratch) = fresh_scratch();
        let error = run(&source, &scratch, budget.clone(), &f).unwrap_err();
        assert_eq!(error.code, oracle.code, "{mutation}: {error:?}");
        assert!(
            budget.guard().is_err(),
            "failure must poison all shared consumers"
        );
        drop(source);
        assert_eq!(before, state(f._dir.path()), "{mutation}");
    }
}

#[test]
fn compact_native_empty_and_signed_sparse_rowids_and_post_delta_pass() {
    let mut f = fixture();
    // Deleting the final row flushes explicit zero total counters; a pristine
    // reference instead has the native initial empty blob.
    mutate(
        &f.path,
        "INSERT INTO documents_fts(documents_fts,rowid,title,aliases,headings,tags,body) SELECT 'delete',doc_row,title,aliases_text,headings,tags_text,body FROM documents; INSERT INTO graph_fts(graph_fts,rowid,name,aliases,endpoints,predicate,qualifiers,description) SELECT 'delete',graph_row,name,aliases_text,endpoints,predicate,qualifiers,description FROM graph_rows; DELETE FROM documents; DELETE FROM graph_rows;",
    );
    f.docs.clear();
    f.graphs.clear();
    let budget = CheckBudget::new(CheckLimits::default()).unwrap();
    let source = pinned(&f.path, &budget);
    let (_dir, scratch) = fresh_scratch();
    let stats = run(&source, &scratch, budget, &f).unwrap();
    assert_eq!(
        (
            stats.document_postings,
            stats.graph_postings,
            stats.document_docsize_rows,
            stats.graph_docsize_rows
        ),
        (0, 0, 0, 0)
    );
    drop(source);
    drop(scratch);
    let c = Connection::open(&f.path).unwrap();
    let mut accented = doc(-7, "café CAFÉ", "alpha alpha beta");
    accented.aliases = vec!["Café".into(), "βeta".into()];
    accented.tags = vec!["alpha".into()];
    accented.headings = "alpha café".into();
    f.docs = vec![
        (-7, accented),
        (0, doc(0, "zero", "alpha")),
        (41, doc(41, "new revision", "alpha gamma")),
    ];
    f.graphs = vec![
        (-11, graph("page_sparse", "alpha", vec!["alpha".into()])),
        (51, graph("page_empty", "", vec![])),
    ];
    for (id, row) in &f.docs {
        insert_doc(&c, *id, row);
    }
    for (id, row) in &f.graphs {
        insert_graph(&c, *id, row);
    }
    // A real FTS delete/insert delta changes a title while preserving rowid.
    c.execute("INSERT INTO documents_fts(documents_fts,rowid,title,aliases,headings,tags,body) VALUES('delete',41,'new revision','','','','alpha gamma')",[]).unwrap();
    c.execute(
        "UPDATE documents SET title='renamed after delta' WHERE doc_row=41",
        [],
    )
    .unwrap();
    c.execute("INSERT INTO documents_fts(rowid,title,body) VALUES(41,'renamed after delta','alpha gamma')",[]).unwrap();
    f.docs[2].1.title = "renamed after delta".into();
    drop(c);
    let budget = CheckBudget::new(CheckLimits::default()).unwrap();
    let source = pinned(&f.path, &budget);
    let (_dir, scratch) = fresh_scratch();
    let stats = run(&source, &scratch, budget, &f).unwrap();
    assert_eq!(
        (
            stats.documents,
            stats.graph_rows,
            stats.document_docsize_rows,
            stats.graph_docsize_rows
        ),
        (3, 2, 3, 2)
    );
    assert!(normalized_audit::validate_index(&source, &AuditLimits::default()).is_ok());
}

#[test]
fn compact_native_schema_and_pin_admission_poison() {
    for mutation in [
        "CREATE TABLE unexpected(x);",
        "DROP INDEX document_record_ids;",
        "DROP TABLE documents_vocab; CREATE VIRTUAL TABLE documents_vocab USING fts5vocab(documents_fts,'row');",
        "INSERT INTO documents_fts_config(k,v) VALUES('automerge',0);",
        "PRAGMA user_version=1;",
    ] {
        let f = fixture();
        mutate(&f.path, mutation);
        let budget = CheckBudget::new(CheckLimits::default()).unwrap();
        let source = pinned(&f.path, &budget);
        let (_dir, scratch) = fresh_scratch();
        assert_eq!(
            run(&source, &scratch, budget.clone(), &f).unwrap_err().code,
            ErrorCode::IndexCorrupt
        );
        assert!(budget.guard().is_err());
    }
    let f = fixture();
    let source = Connection::open(&f.path).unwrap();
    source.execute_batch("BEGIN").unwrap();
    let budget = CheckBudget::new(CheckLimits::default()).unwrap();
    let (_dir, scratch) = fresh_scratch();
    assert!(CompactAudit::new(&source, &scratch, budget.clone()).is_err());
    assert!(budget.guard().is_err());
    let source = Connection::open_with_flags(&f.path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let budget = CheckBudget::new(CheckLimits::default()).unwrap();
    assert!(CompactAudit::new(&source, &scratch, budget.clone()).is_err());
    assert!(budget.guard().is_err());
}

#[test]
fn compact_native_budget_and_mutation_errors_are_terminal() {
    for limits in [
        CheckLimits {
            max_source_bytes: 4096,
            ..Default::default()
        },
        CheckLimits {
            max_scratch_bytes: 4096,
            ..Default::default()
        },
        CheckLimits {
            max_vm_steps: 1000,
            ..Default::default()
        },
        CheckLimits {
            max_postings: 1,
            ..Default::default()
        },
        CheckLimits {
            max_elapsed: Duration::from_nanos(1),
            ..Default::default()
        },
    ] {
        let mut f = fixture();
        // A tiny catalog can complete without reaching a 1000-opcode progress
        // callback. Give this case enough real indexed work to exercise it.
        if limits.max_vm_steps == 1000 {
            let source = Connection::open(&f.path).unwrap();
            for id in 10..210 {
                let row = doc(
                    id,
                    "Budget probe",
                    "enough indexed work for cooperative interruption",
                );
                insert_doc(&source, id, &row);
                f.docs.push((id, row));
            }
        }
        let description = format!("{limits:?}");
        let budget = CheckBudget::new(limits).unwrap();
        // A deadline may expire before source SQL configuration, still terminal.
        if budget.guard().is_err() {
            assert!(budget.guard().is_err());
            continue;
        }
        let source =
            Connection::open_with_flags(&f.path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        if budget.configure_sql(&source).is_err() {
            assert!(budget.guard().is_err());
            continue;
        }
        source
            .execute_batch("BEGIN; SELECT count(*) FROM sqlite_schema;")
            .unwrap();
        let (_dir, scratch) = fresh_scratch();
        assert!(
            run(&source, &scratch, budget.clone(), &f).is_err(),
            "{description}"
        );
        assert!(budget.guard().is_err());
    }
    let f = fixture();
    let budget = CheckBudget::new(CheckLimits::default()).unwrap();
    let source = pinned(&f.path, &budget);
    let (_dir, scratch) = fresh_scratch();
    let mut audit = CompactAudit::new(&source, &scratch, budget.clone()).unwrap();
    scratch.execute_batch("DROP TABLE documents_fts").unwrap();
    assert!(audit.document(1, &f.docs[0].1).is_err());
    assert!(
        audit.graph(1, &f.graphs[0].1).is_err(),
        "cannot resume after a swallowed write failure"
    );
    assert!(audit.finish().is_err());
    assert!(budget.guard().is_err());
}

#[test]
fn compact_native_rejects_sort_plans_and_preallocation_row_overflow() {
    let f = fixture();
    let budget = CheckBudget::new(CheckLimits::default()).unwrap();
    let source = pinned(&f.path, &budget);
    let (_dir, scratch) = fresh_scratch();
    let mut audit = CompactAudit::new(&source, &scratch, budget.clone()).unwrap();
    let row = doc(12, "", &"a".repeat(2048));
    // Explicit row admission is local; unrelated canonical raw text is not retained.
    let tiny = CheckBudget::new(CheckLimits {
        max_row_bytes: 1024,
        ..Default::default()
    })
    .unwrap();
    audit.budget = tiny.clone();
    assert_eq!(
        audit.document(12, &row).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert!(tiny.guard().is_err());
    let clean = CheckBudget::new(CheckLimits::default()).unwrap();
    assert!(
        require_stream_plan(
            &source,
            "SELECT term,doc,col,offset FROM documents_vocab ORDER BY term,doc,col,offset",
            &clean
        )
        .is_err()
    );
    assert!(clean.guard().is_err());
}
