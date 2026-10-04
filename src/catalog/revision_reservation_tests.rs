//! Indexed generated-ID freshness over real normalized facts, including dangling
//! authored references. These lookups do not enumerate registry candidates.
use super::{
    Catalog,
    file_types::{BuildIdentity, CatalogSelection},
    normalized_build::{BuildLimits, NormalizedBuilder},
    query_types::{QueryReadLimits, QueryReadUsage},
    scan, selector,
};
use crate::{
    domain::{ErrorCode, RecordId, VaultRelativePath},
    sources::SourceRefreshLookup,
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use rusqlite::{Connection, params};
use std::{fs, time::Duration};

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn revision_path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(format!(
        "sources/source_selected/revisions/{value}/revision.md"
    ))
    .unwrap()
}
fn fixture() -> (tempfile::TempDir, Catalog, Connection) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: '1'\nwiki_id: vault_reservation\nwiki_kind: vault\ntitle: Reservations\n---\n").unwrap();
    fs::write(temp.path().join("claimed.md"), b"---\nwiki_schema: '1'\nwiki_id: revision_claimed\nwiki_kind: page\ntitle: Claimed identity\nwiki_status: reviewed\n---\n").unwrap();
    fs::write(temp.path().join("dependencies.md"), b"---\nwiki_schema: '1'\nwiki_id: page_dependencies\nwiki_kind: page\ntitle: Dependencies\nwiki_status: reviewed\nwiki_depends_on_ids: [revision_list_reserved]\n---\nExact unresolved [[sources/source_selected/revisions/revision_path_reserved/revision.md]].\nGeneric unresolved [[revision]].\n").unwrap();
    fs::write(temp.path().join("entity.md"), b"---\nwiki_schema: '1'\nwiki_id: entity_reference\nwiki_kind: entity\ntitle: Typed reference\nwiki_status: active\nwiki_entity_type: component\nwiki_superseded_by_id: revision_typed_reserved\n---\n").unwrap();
    let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let catalog = Catalog::new(fs_handle.clone(), id("vault_reservation"));
    let writer = WriterPermit::acquire(fs_handle.root(), Duration::ZERO).unwrap();
    let identity = BuildIdentity {
        selection: CatalogSelection::new(id("vault_reservation"), 1).unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: false,
    };
    selector::prepare(&fs_handle, &writer, &identity.selection).unwrap();
    let mut builder =
        NormalizedBuilder::begin(&fs_handle, &writer, identity, BuildLimits::default()).unwrap();
    let input = scan::scan_input(&fs_handle, &id("vault_reservation")).unwrap();
    let projection =
        scan::project_normalized_with_sink(&fs_handle, &input, false, &mut builder).unwrap();
    let completed = builder.finish_normalized(&projection).unwrap();
    selector::publish(
        &fs_handle,
        &writer,
        &completed.identity.selection,
        Duration::ZERO,
    )
    .unwrap();
    let database = Connection::open(completed.path).unwrap();
    (temp, catalog, database)
}
fn measure(catalog: &Catalog, name: &str) -> (bool, QueryReadUsage) {
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let reserved = reader
        .revision_identity_is_reserved(&id(name), &revision_path(name))
        .unwrap();
    (reserved, reader.usage())
}

#[test]
fn actual_builder_claims_dangling_typed_list_and_exact_path_reserve_new_identity() {
    let (_temp, catalog, database) = fixture();
    for name in [
        "revision_claimed",
        "revision_typed_reserved",
        "revision_list_reserved",
        "revision_path_reserved",
    ] {
        let (reserved, usage) = measure(&catalog, name);
        assert!(reserved, "{name}");
        assert_eq!(usage.rows, 2, "one header plus one existence witness");
    }
    // The typed/list/path targets genuinely do not exist as adopted records.
    let absent: i64 = database.query_row("SELECT count(*) FROM records WHERE id IN ('revision_typed_reserved','revision_list_reserved','revision_path_reserved')", [], |row| row.get(0)).unwrap();
    assert_eq!(absent, 0);
    assert!(!measure(&catalog, "revision_fresh").0);
    assert_eq!(measure(&catalog, "revision_fresh").1.rows, 1);
    // Exact proposed path is independent of the proposed ID lookup itself.
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    assert!(
        reader
            .revision_identity_is_reserved(
                &id("revision_different"),
                &revision_path("revision_path_reserved")
            )
            .unwrap()
    );
    assert!(
        !reader
            .revision_identity_is_reserved(
                &id("revision_different"),
                &revision_path("revision_path_reserved_elsewhere")
            )
            .unwrap()
    );
}

#[test]
fn adopted_record_remains_reserved_when_selected_identity_claim_is_missing() {
    let (_temp, catalog, database) = fixture();
    database
        .execute(
            "DELETE FROM identity_claims WHERE record_id='revision_claimed'",
            [],
        )
        .unwrap();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    assert!(
        reader
            .revision_identity_is_reserved(
                &id("revision_claimed"),
                &revision_path("revision_claimed")
            )
            .unwrap()
    );
    assert_eq!(reader.usage().rows, 2);
}

#[test]
fn broad_basename_alias_and_unrelated_populations_leave_reservation_work_unchanged() {
    let (_temp, catalog, database) = fixture();
    let before_false = measure(&catalog, "revision_fresh");
    let before_true = measure(&catalog, "revision_list_reserved");
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    for n in 0..5000 {
        let name = format!("revision_noise_{n:04}");
        let owner = format!("page_noise_{n:04}");
        let location = format!("noise/{n:04}/revision.md");
        database
            .execute(
                "INSERT INTO identity_claims VALUES(?1,?2,'unread unrelated hash','page')",
                params![name, location],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO semantic_edges VALUES(?1,?2,'unread unrelated role JSON')",
                params![owner, name],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO link_match_keys VALUES('basename','revision',?1,0)",
                [&location],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO link_match_keys VALUES('alias',?1,?2,1)",
                params![revision_path("revision_fresh").as_str(), location],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO link_match_keys VALUES('path',?1,?2,2)",
                params![format!("elsewhere/{n}/revision.md"), location],
            )
            .unwrap();
        // The candidate population is deliberately not consulted for freshness.
        database
            .execute(
                "INSERT INTO registry_match_keys VALUES('basename','revision',?1,?2)",
                params![name, location],
            )
            .unwrap();
    }
    database.execute_batch("COMMIT").unwrap();
    assert_eq!(measure(&catalog, "revision_fresh"), before_false);
    assert_eq!(measure(&catalog, "revision_list_reserved"), before_true);
}

#[test]
fn all_required_probe_tables_and_indexes_are_checked_even_when_claim_already_matches() {
    for missing in [
        "identity_claims",
        "records",
        "semantic_edges",
        "link_match_keys",
    ] {
        let (_temp, catalog, database) = fixture();
        database
            .execute(&format!("DROP TABLE {missing}"), [])
            .unwrap();
        let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
        assert_eq!(
            reader
                .revision_identity_is_reserved(
                    &id("revision_claimed"),
                    &revision_path("revision_claimed")
                )
                .unwrap_err()
                .code,
            ErrorCode::IndexCorrupt,
            "missing {missing}"
        );
    }
    let (_temp, catalog, database) = fixture();
    database
        .execute("DROP INDEX semantic_dependents", [])
        .unwrap();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    assert_eq!(
        reader
            .revision_identity_is_reserved(
                &id("revision_claimed"),
                &revision_path("revision_claimed")
            )
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
}

#[test]
fn layout_and_malformed_header_refuse_instead_of_treating_missing_facts_as_fresh() {
    let (_temp, catalog, database) = fixture();
    for layout in [0, 1] {
        database
            .execute("UPDATE catalog_meta SET proof_layout_version=?1", [layout])
            .unwrap();
        let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
        assert_eq!(
            reader
                .revision_identity_is_reserved(
                    &id("revision_fresh"),
                    &revision_path("revision_fresh")
                )
                .unwrap_err()
                .code,
            ErrorCode::OfflineUnavailable
        );
    }
    database.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE catalog_meta SET proof_layout_version='malformed'").unwrap();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    assert_eq!(
        reader
            .revision_identity_is_reserved(&id("revision_fresh"), &revision_path("revision_fresh"))
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
}

#[test]
fn reservation_input_and_witness_are_admitted_against_shared_cumulative_limits() {
    let (_temp, catalog, _database) = fixture();
    let reader = catalog
        .query_snapshot(QueryReadLimits {
            max_rows: 1,
            ..QueryReadLimits::default()
        })
        .unwrap();
    assert!(
        !reader
            .revision_identity_is_reserved(&id("revision_fresh"), &revision_path("revision_fresh"))
            .unwrap()
    );
    assert_eq!(reader.usage().rows, 1);
    assert_eq!(
        reader
            .revision_identity_is_reserved(
                &id("revision_claimed"),
                &revision_path("revision_claimed")
            )
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    let reader = catalog
        .query_snapshot(QueryReadLimits {
            max_rows: 1,
            ..QueryReadLimits::default()
        })
        .unwrap();
    assert_eq!(
        reader
            .revision_identity_is_reserved(
                &id("revision_claimed"),
                &revision_path("revision_claimed")
            )
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    let reader = catalog
        .query_snapshot(QueryReadLimits {
            max_row_bytes: 1,
            ..QueryReadLimits::default()
        })
        .unwrap();
    assert_eq!(
        reader
            .revision_identity_is_reserved(&id("revision_fresh"), &revision_path("revision_fresh"))
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(reader.usage().rows, 1);
    assert_eq!(reader.usage().bytes, 0);
    let reader = catalog
        .query_snapshot(QueryReadLimits {
            max_bytes: 1,
            ..QueryReadLimits::default()
        })
        .unwrap();
    assert_eq!(
        reader
            .revision_identity_is_reserved(&id("revision_fresh"), &revision_path("revision_fresh"))
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
}
