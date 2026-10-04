//! Frozen expectations: candidate-metadata-implementation.json. Real builders,
//! fresh production readers and pager/bytecode checks, without a timing gate.
use super::*;
use crate::{
    app::{OfflineApp, OperationOptions},
    catalog::{
        Catalog,
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        query::QuerySnapshot,
        query_types::QueryReadLimits,
        scan, selector,
    },
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use rusqlite::StatementStatus;
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn request(body: &str) -> CaptureRequest {
    CaptureRequest {
        title: "Metadata captured title".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "candidate-fixture.md".into(),
        original: body.as_bytes().to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: None,
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    catalog: Catalog,
    database: PathBuf,
    source: RecordId,
    revision: RecordId,
    long_tag: String,
}
impl Fixture {
    fn new(large: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_candidate\nwiki_kind: vault\ntitle: Candidate fixture\n---\n").unwrap();
        let long_tag = "t".repeat(4096);
        for n in 0..64 {
            let fields = BTreeMap::from([
                ("wiki_schema".into(), json!("1")),
                ("wiki_id".into(), json!(format!("page_candidate_{n:03}"))),
                ("wiki_kind".into(), json!("page")),
                ("wiki_status".into(), json!("reviewed")),
                ("title".into(), json!("Metadata matching title")),
                (
                    "tags".into(),
                    json!([long_tag, if n == 63 { "keeper" } else { "ordinary" }]),
                ),
            ]);
            CanonicalRecord::new(fields.clone()).unwrap();
            let mut text = "---\n".to_owned();
            for (key, value) in fields {
                text.push_str(&format!("{key}: {value}\n"));
            }
            text.push_str("---\nmetadataword\n");
            if large {
                text.push_str(&"background unassociated fact. ".repeat(4096));
            }
            assert!(parse_note(text.as_bytes()).canonical.is_some());
            fs::write(temp.path().join(format!("page-{n:03}.md")), text).unwrap();
        }
        fs::write(temp.path().join("entity.md"),"---\nwiki_schema: '1'\nwiki_id: entity_candidate\nwiki_kind: entity\nwiki_status: active\nwiki_entity_type: concept\ntitle: Metadata identity\n---\nUnsupported description\n").unwrap();
        let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let capture = SourceStore::new(vault.clone())
            .plan_capture(request("capturebeforeword retained proof"))
            .unwrap();
        for op in &capture.draft.as_ref().unwrap().operations {
            let bytes = op.proposed.as_ref().unwrap();
            let path = temp.path().join(op.target.as_str());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        let catalog = Catalog::new(vault.clone(), id("vault_candidate"));
        let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
        let identity = BuildIdentity {
            selection: CatalogSelection::new(id("vault_candidate"), 1).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&vault, &writer, &identity.selection).unwrap();
        let mut builder =
            NormalizedBuilder::begin(&vault, &writer, identity, BuildLimits::default()).unwrap();
        let input = scan::scan_input(&vault, &id("vault_candidate")).unwrap();
        let projection =
            scan::project_normalized_with_sink(&vault, &input, false, &mut builder).unwrap();
        let completed = builder.finish_normalized(&projection).unwrap();
        selector::publish(
            &vault,
            &writer,
            &completed.identity.selection,
            Duration::ZERO,
        )
        .unwrap();
        Self {
            _temp: temp,
            catalog,
            database: completed.path,
            source: capture.source_id,
            revision: capture.revision_id,
            long_tag,
        }
    }
    fn reader(&self) -> QuerySnapshot {
        self.catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap()
    }
}

fn scalar_sql(plan: &QueryPlan, context: &str, leg: usize, covered: bool) -> (String, Vec<Value>) {
    let expression = lexical_expression("Metadata metadataword").unwrap();
    let (sql, values) =
        normalized_candidate_query("Metadata metadataword", &expression, plan, context, leg);
    let (cte, _) = sql.split_once("SELECT d.path,d.file_hash").unwrap();
    let sql = format!(
        "{cte}SELECT c.doc_row,c.score,c.tie,c.path FROM candidate_ids c ORDER BY c.score,c.tie,c.path"
    );
    (
        if covered {
            sql
        } else {
            sql.replace(
                "documents d INDEXED BY document_candidate_metadata ON",
                "documents d NOT INDEXED ON",
            )
        },
        values,
    )
}
fn misses(connection: &Connection) -> i64 {
    let (mut value, mut high) = (0, 0);
    assert_eq!(
        unsafe {
            rusqlite::ffi::sqlite3_db_status(
                connection.handle(),
                rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS,
                &mut value,
                &mut high,
                0,
            )
        },
        rusqlite::ffi::SQLITE_OK
    );
    i64::from(value)
}
#[derive(Debug)]
struct Work {
    rows: Vec<(i64, u64, String, String)>,
    misses: i64,
    vm: i32,
    plans: Vec<String>,
}
fn work(f: &Fixture, plan: &QueryPlan, context: &str, leg: usize, covered: bool) -> Work {
    let reader = f.reader();
    let connection = reader.connection();
    let (sql, values) = scalar_sql(plan, context, leg, covered);
    let plans = connection
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(params_from_iter(values.iter()), |r| r.get(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    let mut statement = connection.prepare(&sql).unwrap();
    let before = misses(connection);
    let start = Instant::now();
    let rows = statement
        .query_map(params_from_iter(values.iter()), |r| {
            Ok((
                r.get(0)?,
                r.get::<_, f64>(1)?.to_bits(),
                r.get(2)?,
                r.get(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let result = Work {
        rows,
        misses: misses(connection) - before,
        vm: statement.get_status(StatementStatus::VmStep),
        plans,
    };
    eprintln!(
        "candidate metadata covered={covered} leg={leg} tags={} elapsed_us={} vm={} work={result:?}",
        plan.filters.tags.len(),
        start.elapsed().as_micros(),
        result.vm
    );
    result
}

fn assert_candidate_columns(f: &Fixture, plan: &QueryPlan, tags: bool) {
    let reader = f.reader();
    let connection = reader.connection();
    let (table,index): (i64,i64) = connection.query_row("SELECT (SELECT rootpage FROM sqlite_schema WHERE name='documents'),(SELECT rootpage FROM sqlite_schema WHERE name='document_candidate_metadata')",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let (sql, values) = scalar_sql(plan, "1", 3, true);
    let ops = connection
        .prepare(&format!("EXPLAIN {sql}"))
        .unwrap()
        .query_map(params_from_iter(values.iter()), |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let cursors: BTreeMap<_, _> = ops
        .iter()
        .filter(|(op, _, root)| op == "OpenRead" && (*root == table || *root == index))
        .map(|(_, cursor, root)| (*cursor, *root))
        .collect();
    let table_columns: Vec<_> = ops
        .iter()
        .filter(|(op, cursor, _)| op == "Column" && cursors.get(cursor) == Some(&table))
        .map(|(_, _, column)| *column)
        .collect();
    let index_columns: Vec<_> = ops
        .iter()
        .filter(|(op, cursor, _)| op == "Column" && cursors.get(cursor) == Some(&index))
        .map(|(_, _, column)| *column)
        .collect();
    if tags {
        assert!(!table_columns.is_empty());
        assert!(
            table_columns.iter().all(|c| *c == 9),
            "table columns {table_columns:?}"
        );
    } else {
        assert!(table_columns.is_empty(), "table columns {table_columns:?}");
    }
    assert!(
        index_columns.contains(&4),
        "eligibility must come from covering index: {index_columns:?}"
    );
    assert!(
        index_columns.contains(&3),
        "kind must come from covering index: {index_columns:?}"
    );
}

#[test]
fn candidate_metadata_covering_and_tag_paths_avoid_large_payload_overflow() {
    let f = Fixture::new(true);
    let plan = QueryPlan {
        limits: SearchLimits {
            candidates: 1,
            hits: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let old = work(&f, &plan, "1", 3, false);
    let fixed = work(&f, &plan, "1", 3, true);
    assert_eq!(old.rows, fixed.rows);
    assert_eq!(fixed.rows.len(), 2);
    assert!(
        fixed
            .plans
            .iter()
            .any(|p| p.contains("COVERING INDEX document_candidate_metadata")),
        "{fixed:?}"
    );
    assert!(old.misses >= fixed.misses * 4 + 256, "{old:?} -> {fixed:?}");
    assert_candidate_columns(&f, &plan, false);
    let tagged = QueryPlan {
        filters: SearchFilters {
            tags: vec![f.long_tag.clone()],
            ..Default::default()
        },
        ..plan.clone()
    };
    let old_tag = work(&f, &tagged, "1", 3, false);
    let fixed_tag = work(&f, &tagged, "1", 3, true);
    assert_eq!(old_tag.rows, fixed_tag.rows);
    assert_eq!(fixed_tag.rows.len(), 2);
    assert!(
        old_tag.misses >= fixed_tag.misses * 2 + 128,
        "{old_tag:?} -> {fixed_tag:?}"
    );
    assert_candidate_columns(&f, &tagged, true);
    let late = QueryPlan {
        filters: SearchFilters {
            tags: vec![f.long_tag.clone(), "keeper".into()],
            ..Default::default()
        },
        ..plan.clone()
    };
    let late_work = work(&f, &late, "1", 3, true);
    assert_eq!(late_work.rows.len(), 1);
    assert_eq!(late_work.rows[0].3, "page-063.md");
    for (context, leg) in [
        (filters::catalog_context_policy(false, true), 3),
        (filters::catalog_context_policy(true, true), 3),
        ("1".into(), 4),
    ] {
        let old = work(&f, &plan, &context, leg, false);
        let fixed = work(&f, &plan, &context, leg, true);
        assert!(!fixed.rows.is_empty(), "leg {leg}");
        assert_eq!(old.rows, fixed.rows);
    }
    for filters in [
        SearchFilters {
            tags: vec!["absent".repeat(500)],
            ..Default::default()
        },
        SearchFilters {
            source_ids: vec![id("source_absent")],
            ..Default::default()
        },
        SearchFilters {
            path_prefix: Some("absent-path".into()),
            ..Default::default()
        },
        SearchFilters {
            authored_statuses: vec!["draft".into()],
            kinds: vec![RecordKind::Page],
            ..Default::default()
        },
    ] {
        let negative = QueryPlan {
            filters,
            ..plan.clone()
        };
        assert!(work(&f, &negative, "1", 3, true).rows.is_empty());
        assert!(
            search_catalog(&f.reader(), "Metadata metadataword", &negative)
                .unwrap()
                .hits
                .is_empty()
        );
    }
    assert_eq!(
        search_catalog(&f.reader(), "Metadata metadataword", &late)
            .unwrap()
            .hits[0]
            .locator
            .path
            .as_str(),
        "page-063.md"
    );
}

#[test]
fn candidate_metadata_preflight_refuses_missing_or_different_definitions() {
    let f = Fixture::new(false);
    for ddl in [
        "".to_owned(),
        GENERAL_CANDIDATE_INDEX.replace("doc_row,path", "path,doc_row"),
        format!("{GENERAL_CANDIDATE_INDEX} WHERE eligibility='current'"),
        GENERAL_CANDIDATE_INDEX.replace("CREATE INDEX", "CREATE UNIQUE INDEX"),
        GENERAL_CANDIDATE_INDEX.replace("path,record_id", "path COLLATE NOCASE,record_id"),
        GENERAL_CANDIDATE_INDEX.replace("doc_row", &format!("{}doc_row", " ".repeat(4096))),
    ] {
        let database = Connection::open(&f.database).unwrap();
        selector::configure_wal(&database).unwrap();
        database
            .execute_batch("DROP INDEX document_candidate_metadata")
            .unwrap();
        database.execute_batch(&ddl).unwrap();
        drop(database);
        let reader = f.reader();
        assert_eq!(
            search_catalog(&reader, "Metadata", &QueryPlan::default())
                .unwrap_err()
                .code,
            ErrorCode::CapabilityUnavailable,
            "{ddl}"
        );
        assert_eq!(reader.usage().rows, 0);
        let database = Connection::open(&f.database).unwrap();
        selector::configure_wal(&database).unwrap();
        database
            .execute_batch("DROP INDEX IF EXISTS document_candidate_metadata")
            .unwrap();
        database.execute_batch(GENERAL_CANDIDATE_INDEX).unwrap();
    }
}

fn assert_index_matches_table(reader: &QuerySnapshot) {
    validate_general_indexes(reader.connection()).unwrap();
    let columns = "doc_row,path,record_id,kind,eligibility,source_id,owner_revision";
    let difference: i64 = reader.connection().query_row(&format!("SELECT count(*) FROM (SELECT {columns} FROM documents INDEXED BY document_candidate_metadata EXCEPT SELECT {columns} FROM documents NOT INDEXED)"),[],|r|r.get(0)).unwrap();
    assert_eq!(difference, 0);
    let counts:(i64,i64) = reader.connection().query_row("SELECT (SELECT count(*) FROM documents INDEXED BY document_candidate_metadata),(SELECT count(*) FROM documents NOT INDEXED)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(counts.0, counts.1);
}
#[test]
fn candidate_metadata_index_tracks_public_title_and_new_revision_deltas() {
    let f = Fixture::new(false);
    let app = OfflineApp::new(
        f.catalog.fs().clone(),
        OperationOptions {
            offline: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_index_matches_table(&f.reader());
    let title = app
        .source_refresh_with_title(
            f.source.clone(),
            request("capturebeforeword retained proof"),
            Some("Changed metadata title"),
        )
        .unwrap();
    assert!(title.reused);
    let reader = f.reader();
    assert_eq!(reader.snapshot().generation, 2);
    assert_index_matches_table(&reader);
    let title_hits =
        search_catalog(&reader, "Changed metadata title", &QueryPlan::default()).unwrap();
    assert!(title_hits.hits.iter().any(|h| {
        h.locator
            .record
            .as_ref()
            .is_some_and(|r| r.record_id == f.source)
    }));
    drop(reader);
    let changed = app
        .source_refresh(f.source.clone(), request("captureafterword changed proof"))
        .unwrap();
    let revision = &changed.allocated_ids["revision"];
    assert_ne!(revision, &f.revision);
    let reader = f.reader();
    assert_eq!(reader.snapshot().generation, 3);
    assert_index_matches_table(&reader);
    let source_plan = QueryPlan {
        filters: SearchFilters {
            source_ids: vec![f.source.clone()],
            ..Default::default()
        },
        ..Default::default()
    };
    let current = search_catalog(&reader, "captureafterword", &source_plan).unwrap();
    assert!(
        current
            .hits
            .iter()
            .any(|h| h.owner_revision.as_ref() == Some(revision))
    );
    assert!(
        search_catalog(&reader, "capturebeforeword", &source_plan)
            .unwrap()
            .hits
            .is_empty()
    );
    let historical = QueryPlan {
        filters: SearchFilters {
            include_historical: true,
            ..source_plan.filters
        },
        ..Default::default()
    };
    assert!(
        search_catalog(&reader, "capturebeforeword", &historical)
            .unwrap()
            .hits
            .iter()
            .any(|h| h.owner_revision.as_ref() == Some(&f.revision))
    );
}
