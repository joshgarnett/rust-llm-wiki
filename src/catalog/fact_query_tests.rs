//! Selected fact lookup mechanics over real disposable normalized publications.
use super::{
    Catalog,
    eligibility_facts::EligibilityRole,
    file_types::{BuildIdentity, CatalogSelection},
    link_facts::{MatchKey, MatchKeyKind},
    normalized_build::{BuildLimits, NormalizedBuilder},
    query::QuerySnapshot,
    query_types::QueryReadLimits,
    scan, selector,
};
use crate::{
    domain::{ErrorCode, RecordId, VaultRelativePath},
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
fn key(kind: MatchKeyKind, value: &str) -> MatchKey {
    MatchKey {
        kind,
        value: value.into(),
    }
}
fn fixture() -> (tempfile::TempDir, Catalog, Connection) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        b"---\nwiki_schema: '1'\nwiki_id: vault_fact_query\nwiki_kind: vault\ntitle: Facts\n---\n",
    )
    .unwrap();
    fs::write(temp.path().join("entity.md"),"---\nwiki_schema: '1'\nwiki_id: entity_fact_query\nwiki_kind: entity\ntitle: Entity\nwiki_status: active\nwiki_entity_type: component\naliases: ['Éclair']\n---\n").unwrap();
    fs::write(temp.path().join("page.md"),b"---\nwiki_schema: '1'\nwiki_id: page_fact_query\nwiki_kind: page\ntitle: Page\nwiki_status: reviewed\nwiki_depends_on_ids: [entity_fact_query]\n---\n# Page\n\nUnresolved [[revision]].\n").unwrap();
    let vault = VaultRoot::explicit(temp.path()).unwrap();
    let fs_handle = VaultFs::new(vault);
    let catalog = Catalog::new(fs_handle.clone(), id("vault_fact_query"));
    let writer = WriterPermit::acquire(fs_handle.root(), Duration::from_secs(1)).unwrap();
    let identity = BuildIdentity {
        selection: CatalogSelection::new(id("vault_fact_query"), 1).unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: false,
    };
    selector::prepare(&fs_handle, &writer, &identity.selection).unwrap();
    let mut builder =
        NormalizedBuilder::begin(&fs_handle, &writer, identity, BuildLimits::default()).unwrap();
    let input = scan::scan_input(&fs_handle, &id("vault_fact_query")).unwrap();
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
    (temp, catalog, database)
}
fn reader(catalog: &Catalog) -> QuerySnapshot {
    catalog.query_snapshot(QueryReadLimits::default()).unwrap()
}

#[test]
fn complete_normalized_fact_lookup_reads_named_baseline_and_direct_states() {
    let (_temp, catalog, _database) = fixture();
    let reader = reader(&catalog);
    reader.require_fact_layout().unwrap();
    let fact = reader
        .eligibility_fact(&id("page_fact_query"))
        .unwrap()
        .unwrap();
    assert_eq!(
        fact.direct_paths,
        std::collections::BTreeSet::from([path("page.md")])
    );
    let states = reader.direct_path_states(&[path("page.md")]).unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].path, path("page.md"));
    assert!(
        reader
            .eligibility_fact(&id("unknown_record"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        reader
            .direct_path_states(&[path("missing.md")])
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
}
#[test]
fn layout_zero_and_missing_selected_fact_refuse_without_fallback() {
    let (_temp, catalog, database) = fixture();
    database
        .execute("UPDATE catalog_meta SET proof_layout_version=0", [])
        .unwrap();
    assert_eq!(
        reader(&catalog).require_fact_layout().unwrap_err().code,
        ErrorCode::OfflineUnavailable
    );
    database
        .execute("UPDATE catalog_meta SET proof_layout_version=1", [])
        .unwrap();
    database
        .execute(
            "DELETE FROM record_eligibility_facts WHERE record_id='page_fact_query'",
            [],
        )
        .unwrap();
    assert_eq!(
        reader(&catalog)
            .eligibility_fact(&id("page_fact_query"))
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    database
        .execute(
            "INSERT INTO record_eligibility_facts VALUES('page_fact_query','{}')",
            [],
        )
        .unwrap();
    assert_eq!(
        reader(&catalog)
            .eligibility_fact(&id("page_fact_query"))
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
}
#[test]
fn role_scopes_and_unrelated_growth_preserve_selected_row_usage() {
    let (_temp, catalog, database) = fixture();
    let measure = |reader: &QuerySnapshot| {
        let outgoing = reader
            .outgoing_edges(&id("page_fact_query"), &[EligibilityRole::DeclaredSupport])
            .unwrap();
        let reverse = reader
            .dependent_edges(
                &id("entity_fact_query"),
                &[EligibilityRole::DeclaredSupport],
            )
            .unwrap();
        assert_eq!(outgoing, reverse);
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].owner_id, id("page_fact_query"));
        assert_eq!(outgoing[0].target_id, id("entity_fact_query"));
        assert!(
            reader
                .outgoing_edges(&id("page_fact_query"), &[EligibilityRole::SourceInventory])
                .unwrap()
                .is_empty()
        );
        reader.usage()
    };
    let expected = measure(&reader(&catalog));
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    for n in 0..1000 {
        database
            .execute(
                "INSERT INTO semantic_edges(owner_id,target_id,role_json) VALUES(?1,?2,?3)",
                params![
                    format!("unrelated_owner_{n}"),
                    format!("unrelated_target_{n}"),
                    super::sql::json(&EligibilityRole::DeclaredSupport).unwrap()
                ],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO record_eligibility_facts VALUES(?1,'invalid unrelated JSON')",
                [format!("unrelated_owner_{n}")],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO link_match_keys VALUES('basename',?1,?2,0)",
                params![format!("unrelated_{n}"), format!("unrelated/{n}.md")],
            )
            .unwrap();
    }
    database.execute_batch("COMMIT").unwrap();
    assert_eq!(measure(&reader(&catalog)), expected);
    let read = reader(&catalog);
    assert_eq!(
        read.outgoing_edges(&id("page_fact_query"), &[])
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
    assert_eq!(read.usage().rows, 0);
    assert_eq!(
        read.dependent_edges(
            &id("entity_fact_query"),
            &vec![EligibilityRole::DeclaredSupport; 4097]
        )
        .unwrap_err()
        .code,
        ErrorCode::Usage
    );
}
#[test]
fn unresolved_links_keep_reverse_keys_and_registry_lookup_is_one_exact_bucket() {
    let (_temp, catalog, _database) = fixture();
    let read = reader(&catalog);
    let locations = read
        .affected_links(&[key(MatchKeyKind::Basename, "revision")])
        .unwrap();
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].0, path("page.md"));
    let fact = read
        .link_fact(&locations[0].0, locations[0].1)
        .unwrap()
        .unwrap();
    assert_eq!(fact.raw_destination, "revision");
    assert!(fact.typed.is_none());
    assert!(fact.keys.contains(&key(MatchKeyKind::Path, "revision.md")));
    assert!(
        read.registry_candidates_for_key(&key(MatchKeyKind::Path, "absent/Éclair"))
            .unwrap()
            .is_empty()
    );
    let candidates = read
        .registry_candidates_for_key(&key(MatchKeyKind::Alias, "Éclair"))
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, id("entity_fact_query"));
    assert!(
        read.registry_candidates_for_key(&key(MatchKeyKind::Alias, "éclair"))
            .unwrap()
            .is_empty()
    );
    assert!(read.link_fact(&path("page.md"), 0).unwrap().is_none());
}
#[test]
fn duplicate_requested_keys_charge_every_returned_row_and_never_truncate_results() {
    let (_temp, catalog, _database) = fixture();
    let k = key(MatchKeyKind::Basename, "revision");
    let read = catalog
        .query_snapshot(QueryReadLimits {
            max_rows: 3,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        read.affected_links(&[k.clone(), k.clone()]).unwrap().len(),
        1
    );
    assert_eq!(read.usage().rows, 3);
    let read = catalog
        .query_snapshot(QueryReadLimits {
            max_rows: 3,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        read.affected_links(&[k.clone(), k.clone(), k])
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(read.usage().rows, 3);
    let read = catalog
        .query_snapshot(QueryReadLimits {
            max_rows: 2,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        read.outgoing_edges(
            &id("page_fact_query"),
            &[
                EligibilityRole::DeclaredSupport,
                EligibilityRole::DeclaredSupport
            ]
        )
        .unwrap_err()
        .code,
        ErrorCode::BudgetExceeded
    );
}
#[test]
fn selected_byte_admission_precedes_json_decode_and_malformed_offsets_refuse() {
    let (_temp, catalog, database) = fixture();
    database.execute("UPDATE record_eligibility_facts SET baseline_json=?1 WHERE record_id='page_fact_query'",[format!("{{{}", "x".repeat(1024))]).unwrap();
    let read = catalog
        .query_snapshot(QueryReadLimits {
            max_row_bytes: 128,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        read.eligibility_fact(&id("page_fact_query"))
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(read.usage().rows, 1);
    let read = reader(&catalog);
    assert_eq!(
        read.eligibility_fact(&id("page_fact_query"))
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    assert_eq!(read.usage().rows, 2);
    database.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO link_match_keys VALUES('basename','negative','bad.md',-1)").unwrap();
    assert_eq!(
        reader(&catalog)
            .affected_links(&[key(MatchKeyKind::Basename, "negative")])
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    assert_eq!(
        reader(&catalog)
            .affected_links(&vec![key(MatchKeyKind::Path, "unused"); 4097])
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
}

#[test]
fn selected_link_key_and_registry_candidate_corruption_refuse() {
    let (_temp, catalog, database) = fixture();
    let read = reader(&catalog);
    let (location, offset) = read
        .affected_links(&[key(MatchKeyKind::Basename, "revision")])
        .unwrap()
        .remove(0);
    drop(read);
    database
        .execute(
            "DELETE FROM link_match_keys WHERE kind='alias' AND value='revision'",
            [],
        )
        .unwrap();
    assert_eq!(
        reader(&catalog)
            .link_fact(&location, offset)
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    database.execute("INSERT INTO registry_match_keys VALUES('alias','false_alias','entity_fact_query','entity.md')",[]).unwrap();
    assert_eq!(
        reader(&catalog)
            .registry_candidates_for_key(&key(MatchKeyKind::Alias, "false_alias"))
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    database.execute("UPDATE registry_match_keys SET path='different.md' WHERE kind='alias' AND value='Éclair'",[]).unwrap();
    assert_eq!(
        reader(&catalog)
            .registry_candidates_for_key(&key(MatchKeyKind::Alias, "Éclair"))
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
}

#[test]
fn untyped_link_refuses_ambiguous_id_key_expansion() {
    let (_temp, catalog, database) = fixture();
    let read = reader(&catalog);
    let (location, offset) = read
        .affected_links(&[key(MatchKeyKind::Basename, "revision")])
        .unwrap()
        .remove(0);
    drop(read);
    for candidate in ["revision_one", "revision_two"] {
        database
            .execute(
                "INSERT INTO link_match_keys VALUES('id',?1,?2,?3)",
                params![candidate, location.as_str(), i64::try_from(offset).unwrap()],
            )
            .unwrap();
    }
    assert_eq!(
        reader(&catalog)
            .link_fact(&location, offset)
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
}
