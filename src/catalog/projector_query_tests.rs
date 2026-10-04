//! Production-projector lookup regressions over disposable normalized publications.
use super::{
    Catalog,
    eligibility::{OppositionKey, opposition_key},
    file_types::{BuildIdentity, CatalogSelection},
    normalized_build::{BuildLimits, NormalizedBuilder},
    query::QuerySnapshot,
    query_types::QueryReadLimits,
    scan, selector, sql,
};
use crate::{
    domain::{ErrorCode, RecordId, RecordKind, VaultRelativePath},
    records::parse_note,
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use rusqlite::{Connection, params};
use std::{fs, time::Duration};
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn assertion(name: &str, negated: bool, extra: &str) -> String {
    format!(
        "---\nwiki_schema: '1'\nwiki_id: {name}\nwiki_kind: assertion\ntitle: {name}\nwiki_status: accepted\nwiki_subject_id: entity_projector\nwiki_object_id: entity_projector\nwiki_predicate: uses\nwiki_negated: {negated}\n{extra}---\n"
    )
}
fn fixture() -> (tempfile::TempDir, Catalog, Connection, OppositionKey) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"),"---\nwiki_schema: '1'\nwiki_id: vault_projector_query\nwiki_kind: vault\ntitle: Projector query\n---\n").unwrap();
    fs::write(temp.path().join("entity.md"),"---\nwiki_schema: '1'\nwiki_id: entity_projector\nwiki_kind: entity\ntitle: Entity\nwiki_status: active\nwiki_entity_type: component\n---\n").unwrap();
    fs::write(temp.path().join("page.md"),"---\nwiki_schema: '1'\nwiki_id: page_projector\nwiki_kind: page\ntitle: Page\nwiki_status: reviewed\n---\n# Page\n\n[[revision]] and [[missing.md]].\n").unwrap();
    let positive = assertion(
        "assertion_positive",
        false,
        "wiki_evidence: ['[[revision]]', '[[revision]]', '[[absent.md]]']\n",
    );
    fs::write(temp.path().join("positive.md"), &positive).unwrap();
    fs::write(
        temp.path().join("invalid.md"),
        assertion(
            "assertion_invalid",
            true,
            "wiki_depends_on_ids: [missing_declared_target]\n",
        ),
    )
    .unwrap();
    fs::write(
        temp.path().join("rejected.md"),
        assertion("assertion_rejected", true, "")
            .replace("wiki_status: accepted", "wiki_status: rejected"),
    )
    .unwrap();
    let key = opposition_key(&parse_note(positive.as_bytes()).canonical.unwrap())
        .unwrap()
        .0;
    let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let catalog = Catalog::new(fs_handle.clone(), id("vault_projector_query"));
    let writer = WriterPermit::acquire(fs_handle.root(), Duration::from_secs(1)).unwrap();
    let identity = BuildIdentity {
        selection: CatalogSelection::new(id("vault_projector_query"), 1).unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: false,
    };
    selector::prepare(&fs_handle, &writer, &identity.selection).unwrap();
    let mut builder =
        NormalizedBuilder::begin(&fs_handle, &writer, identity, BuildLimits::default()).unwrap();
    let input = scan::scan_input(&fs_handle, &id("vault_projector_query")).unwrap();
    let projection =
        scan::project_normalized_with_sink(&fs_handle, &input, false, &mut builder).unwrap();
    let completed = builder.finish_normalized(&projection).unwrap();
    selector::publish(
        &fs_handle,
        &writer,
        &completed.identity.selection,
        Duration::from_secs(1),
    )
    .unwrap();
    let database = Connection::open(completed.path).unwrap();
    (temp, catalog, database, key)
}
fn reader(catalog: &Catalog) -> QuerySnapshot {
    catalog.query_snapshot(QueryReadLimits::default()).unwrap()
}

#[test]
fn opposition_includes_every_accepted_polarity_even_invalid_but_excludes_rejected() {
    let (_temp, catalog, database, key) = fixture();
    assert_eq!(
        database
            .query_row(
                "SELECT eligibility FROM records WHERE id='assertion_invalid'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "invalid"
    );
    assert_eq!(
        reader(&catalog).opposition_members(&key).unwrap(),
        vec![
            (id("assertion_positive"), false),
            (id("assertion_invalid"), true)
        ]
    );
    database
        .execute(
            "UPDATE records SET eligibility='stale' WHERE id='assertion_positive'",
            [],
        )
        .unwrap();
    assert_eq!(
        reader(&catalog).opposition_members(&key).unwrap(),
        vec![
            (id("assertion_positive"), false),
            (id("assertion_invalid"), true)
        ]
    );
}
#[test]
fn unrelated_fact_growth_changes_neither_selected_rows_nor_selected_bytes() {
    let (_temp, catalog, database, key) = fixture();
    let measure = |read: &QuerySnapshot| {
        assert_eq!(read.opposition_members(&key).unwrap().len(), 2);
        assert_eq!(read.owned_link_facts(&path("page.md")).unwrap().len(), 2);
        assert!(read.document_metadata(&path("page.md")).unwrap().is_some());
        read.usage()
    };
    let expected = measure(&reader(&catalog));
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    for n in 0..1000 {
        database
            .execute(
                "INSERT INTO opposition_members VALUES(?1,0,?2)",
                params![
                    format!("unrelated malformed key {n}"),
                    format!("unrelated_assertion_{n}")
                ],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO link_facts VALUES(?1,0,'unrelated',NULL,NULL)",
                [format!("unrelated/{n}.md")],
            )
            .unwrap();
        database.execute("INSERT INTO documents(path,file_hash,title,aliases_json,aliases_text,headings,tags_json,tags_text,body,raw_text,eligibility,reasons_json) VALUES(?1,'malformed','Unrelated','[]','','','[]','','unrelated payload','unrelated payload','current','[]')",[format!("unrelated/{n}.md")]).unwrap();
    }
    database.execute_batch("COMMIT").unwrap();
    assert_eq!(measure(&reader(&catalog)), expected);
}
#[test]
fn opposition_group_over_budget_refuses_instead_of_returning_partial_members() {
    let (_temp, catalog, database, key) = fixture();
    let encoded = sql::json(&key).unwrap();
    for n in 0..8 {
        database
            .execute(
                "INSERT INTO opposition_members VALUES(?1,?2,?3)",
                params![encoded, n % 2, format!("added_accepted_{n}")],
            )
            .unwrap();
    }
    let read = catalog
        .query_snapshot(QueryReadLimits {
            max_rows: 4,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        read.opposition_members(&key).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(read.usage().rows, 4);
}
#[test]
fn metadata_has_an_independent_small_budget_even_for_huge_body_and_raw_text() {
    let (_temp, catalog, database, _key) = fixture();
    let expected = reader(&catalog)
        .document_metadata(&path("page.md"))
        .unwrap()
        .unwrap();
    assert_eq!(expected.record_id, Some(id("page_projector")));
    assert_eq!(expected.kind, Some(RecordKind::Page));
    database
        .execute(
            "UPDATE documents SET body=?1,raw_text=?1 WHERE path='page.md'",
            ["x".repeat(9 * 1024 * 1024)],
        )
        .unwrap();
    let read = catalog
        .query_snapshot(QueryReadLimits {
            max_row_bytes: 128,
            max_bytes: 256,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        read.document_metadata(&path("page.md")).unwrap(),
        Some(expected)
    );
    assert_eq!(read.usage().rows, 2);
    assert!(read.usage().bytes < 128);
    assert!(
        read.document_metadata(&path("absent.md"))
            .unwrap()
            .is_none()
    );
}
#[test]
fn whole_owner_links_are_complete_ordered_and_bound_duplicate_lookup_work() {
    let (_temp, catalog, _database, _key) = fixture();
    let read = reader(&catalog);
    let facts = read.owned_link_facts(&path("page.md")).unwrap();
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0].raw_destination, "revision");
    assert_eq!(facts[1].raw_destination, "missing.md");
    assert!(facts[0].byte_start < facts[1].byte_start);
    assert!(read.owned_link_facts(&path("other.md")).unwrap().is_empty());
    assert!(facts.iter().all(|fact| fact.from_path == path("page.md")));
    let read = catalog
        .query_snapshot(QueryReadLimits {
            max_rows: 2,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        read.owned_link_facts(&path("page.md")).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(read.usage().rows, 2);
}
#[test]
fn metadata_and_opposition_selected_corruption_and_unready_layout_refuse() {
    let (_temp, catalog, database, key) = fixture();
    database
        .execute(
            "UPDATE documents SET reasons_json='{' WHERE path='page.md'",
            [],
        )
        .unwrap();
    assert_eq!(
        reader(&catalog)
            .document_metadata(&path("page.md"))
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    database
        .execute(
            "UPDATE documents SET reasons_json=?1 WHERE path='page.md'",
            ["x".repeat(1024)],
        )
        .unwrap();
    let read = catalog
        .query_snapshot(QueryReadLimits {
            max_row_bytes: 128,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        read.document_metadata(&path("page.md")).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(read.usage().rows, 1);
    database
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    database
        .execute(
            "INSERT INTO opposition_members VALUES(?1,2,'bad_polarity')",
            [sql::json(&key).unwrap()],
        )
        .unwrap();
    assert_eq!(
        reader(&catalog).opposition_members(&key).unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
    database
        .execute("UPDATE catalog_meta SET proof_layout_version=0", [])
        .unwrap();
    for result in [
        reader(&catalog).opposition_members(&key).map(|_| ()),
        reader(&catalog)
            .owned_link_facts(&path("page.md"))
            .map(|_| ()),
        reader(&catalog)
            .document_metadata(&path("page.md"))
            .map(|_| ()),
    ] {
        assert_eq!(result.unwrap_err().code, ErrorCode::OfflineUnavailable);
    }
}

#[test]
fn assertion_navigation_keys_preserve_unresolved_destinations_without_link_offsets() {
    use super::link_facts::{MatchKey, MatchKeyKind};
    let (_temp, catalog, database, _) = fixture();
    let key = MatchKey {
        kind: MatchKeyKind::Basename,
        value: "revision".into(),
    };
    let read = reader(&catalog);
    assert_eq!(
        read.affected_assertion_navigation(std::slice::from_ref(&key))
            .unwrap(),
        vec![id("assertion_positive")]
    );
    let expected = read.usage();
    drop(read);
    assert_eq!(database.query_row("SELECT count(*) FROM assertion_navigation_keys WHERE kind='basename' AND value='revision'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM link_facts WHERE from_path='positive.md'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    database.execute_batch("BEGIN").unwrap();
    for n in 0..1000 {
        database
            .execute(
                "INSERT INTO assertion_navigation_keys VALUES('basename',?1,?2)",
                params![format!("unrelated_{n}"), format!("assertion_{n}")],
            )
            .unwrap();
    }
    database.execute_batch("COMMIT").unwrap();
    let read = reader(&catalog);
    assert_eq!(
        read.affected_assertion_navigation(std::slice::from_ref(&key))
            .unwrap(),
        vec![id("assertion_positive")]
    );
    assert_eq!(read.usage(), expected);
    let limited = catalog
        .query_snapshot(QueryReadLimits {
            max_rows: 2,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        limited
            .affected_assertion_navigation(&[key.clone(), key])
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
}
