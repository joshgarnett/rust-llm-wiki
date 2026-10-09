//! Read-only pre-mutation accounting against disposable SQL fixtures.
use super::{
    eligibility_facts::{EligibilityBaseline, EligibilityFact},
    graph_row_preflight::preflight_graph_rows,
    link_facts,
    normalized_delta::{
        CatalogDelta, DeltaStats, DeltaWriteAllowance, GRAPH_V2_MAX_ROWS, MAX_ROW_BYTES,
        OwnedDiagnostics, counted,
    },
    normalized_fact_delta::{FactDelta, OwnedRegistryKeys, RecordFactMutation},
    normalized_schema,
    policy_delta::{PolicyDelta, PolicyReplacement},
    policy_facts::{PolicyKind, PolicyRow, PolicyState},
    sql,
    structural_rules::StructuralFact,
    types::{GraphRow, RecordRow},
};
use crate::{
    changes::ReadDependency,
    domain::{Blake3Hash, Eligibility, ErrorCode, RecordId, VaultRelativePath},
    records::{RegistryEntry, parse_note},
    vault::ExpectedState,
};
use rusqlite::{Connection, params};
use std::collections::BTreeSet;

fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(normalized_schema::SCHEMA).unwrap();
    c.execute("INSERT INTO catalog_meta(singleton,schema_version,vault_id,file_id,creation_epoch,creation_header_hash,epoch,state,vector_cache_lost,vector_loss_unknown,proof_layout_version) VALUES(1,3,'vault_preflight','file_preflight',1,'test',1,'building',0,0,2)", []).unwrap();
    c.execute("INSERT INTO policy_facts VALUES('layout','','','1')", [])
        .unwrap();
    c
}
fn empty() -> CatalogDelta {
    CatalogDelta {
        version: 3,
        records: vec![],
        documents: vec![],
        graph: vec![],
        links: vec![],
        diagnostics: vec![],
        claims: vec![],
        revisions: vec![],
        dependencies: vec![],
        owners: vec![],
        facts: Some(FactDelta {
            records: vec![],
            edge_inserts: vec![],
            edge_deletes: vec![],
            links: vec![],
            registry: vec![],
            policy: Some(PolicyDelta {
                retired_owners: vec![],
                memberships: vec![],
                replacements: vec![],
            }),
        }),
    }
}
fn record() -> RecordRow {
    let bytes = b"---\nwiki_schema: '1'\nwiki_id: entity_preflight\nwiki_kind: entity\ntitle: Preflight\nwiki_status: active\nwiki_entity_type: component\n---\nBody.\n";
    let note = parse_note(bytes);
    RecordRow {
        record: note.canonical.unwrap(),
        path: VaultRelativePath::new("entity.md").unwrap(),
        hash: Blake3Hash::digest(bytes),
        authored_status: Some("active".into()),
        eligibility: Eligibility::Current,
        reasons: vec![],
        identity_eligibility: None,
        description_eligibility: None,
        disputed: false,
        dependencies: vec![],
    }
}
fn owner_delta(row: RecordRow) -> CatalogDelta {
    let mut delta = empty();
    let fact = EligibilityFact {
        baseline: EligibilityBaseline {
            eligibility: Eligibility::Current,
            reasons: vec![],
            identity_eligibility: None,
            description_eligibility: None,
            disputed: false,
        },
        structural: StructuralFact::default(),
        direct_paths: BTreeSet::from([row.path.clone()]),
    };
    let keys = link_facts::registry_keys(&RegistryEntry {
        id: row.record.id().clone(),
        path: row.path.clone(),
        kind: row.record.kind(),
        aliases: vec![],
    })
    .unwrap();
    let facts = delta.facts.as_mut().unwrap();
    facts.records.push(RecordFactMutation {
        record_id: row.record.id().clone(),
        fact,
    });
    facts.registry.push(OwnedRegistryKeys {
        record_id: row.record.id().clone(),
        path: row.path.clone(),
        keys,
    });
    delta.dependencies.push(ReadDependency {
        path: row.path.clone(),
        expected: ExpectedState::Hash(row.hash.clone()),
    });
    delta.records.push(row);
    delta
}
fn seed_owner(c: &Connection, delta: &CatalogDelta) {
    let row = &delta.records[0];
    c.execute(
        "INSERT INTO records VALUES(?1,?2,?3,?4,?5,'current',NULL,NULL,0,?6)",
        params![
            row.record.id().as_str(),
            row.record.kind().as_str(),
            row.path.as_str(),
            row.hash.as_str(),
            row.authored_status,
            sql::json(row).unwrap()
        ],
    )
    .unwrap();
    let facts = delta.facts.as_ref().unwrap();
    let fact = &facts.records[0].fact;
    c.execute(
        "INSERT INTO record_eligibility_facts VALUES(?1,?2,?3)",
        params![
            row.record.id().as_str(),
            sql::json(&fact.baseline).unwrap(),
            sql::json(&fact.structural).unwrap()
        ],
    )
    .unwrap();
    c.execute(
        "INSERT INTO record_direct_paths VALUES(?1,?2)",
        params![row.record.id().as_str(), row.path.as_str()],
    )
    .unwrap();
    c.execute(
        "INSERT INTO dependencies(path,expected_hash) VALUES(?1,?2)",
        params![row.path.as_str(), row.hash.as_str()],
    )
    .unwrap();
    for key in &facts.registry[0].keys {
        c.execute(
            "INSERT INTO registry_match_keys VALUES(?1,?2,?3,?4)",
            params![
                super::normalized_fact_delta::key_name(key.kind),
                key.value,
                row.record.id().as_str(),
                row.path.as_str()
            ],
        )
        .unwrap();
    }
}
fn freeze(c: &Connection) -> u64 {
    c.execute_batch("PRAGMA query_only=ON").unwrap();
    c.total_changes()
}
fn unchanged(c: &Connection, changes: u64) {
    assert_eq!(c.total_changes(), changes);
    assert!(c.is_autocommit());
    assert_eq!(
        c.query_row(
            "SELECT value FROM policy_facts WHERE family='layout'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "1"
    );
}

#[test]
fn policy_fanout_uses_combined_new_and_old_boundary_without_truncation() {
    let c = db();
    let mut delta = empty();
    delta
        .facts
        .as_mut()
        .unwrap()
        .policy
        .as_mut()
        .unwrap()
        .replacements
        .push(PolicyReplacement {
            kind: PolicyKind::Review,
            rows: vec![PolicyRow::State {
                kind: PolicyKind::Review,
                state: PolicyState::Absent,
            }],
        });
    // Two new rows (replacement discriminator + state), layout once, and
    // 32,765 previous edges exactly fill the combined 32,768 allowance.
    let mut insert = c
        .prepare("INSERT INTO policy_facts VALUES('review_edge',?1,'decision_after','')")
        .unwrap();
    for n in 0..GRAPH_V2_MAX_ROWS - 3 {
        insert.execute([format!("decision_before_{n}")]).unwrap();
    }
    drop(insert);
    let changes = freeze(&c);
    let stats = preflight_graph_rows(&c, &delta).unwrap();
    assert_eq!(stats.old_rows, GRAPH_V2_MAX_ROWS - 2);
    unchanged(&c, changes);
    c.execute_batch("PRAGMA query_only=OFF").unwrap();
    c.execute(
        "INSERT INTO policy_facts VALUES('review_edge','decision_extra','decision_after','')",
        [],
    )
    .unwrap();
    let changes = freeze(&c);
    assert_eq!(
        preflight_graph_rows(&c, &delta).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    unchanged(&c, changes);
}

#[test]
fn anticipated_new_owner_loads_are_charged_twice_without_inserting_it() {
    let c = db();
    let delta = owner_delta(record());
    let changes = freeze(&c);
    let stats = preflight_graph_rows(&c, &delta).unwrap();
    assert_eq!(
        stats.old_rows, 3,
        "post-upsert fact load, registry load, and policy layout"
    );
    assert!(stats.old_fact_bytes > 2 * counted(&delta.records[0], MAX_ROW_BYTES).unwrap());
    assert_eq!(
        c.query_row("SELECT count(*) FROM records", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    unchanged(&c, changes);
}

#[test]
fn complete_old_diagnostic_owner_above_standard_limit_is_enumerated() {
    let c = db();
    c.execute("INSERT INTO documents(path,file_hash,title,aliases_json,aliases_text,headings,tags_json,tags_text,body,raw_text,eligibility,reasons_json) VALUES('navigation.md','test','Nav','[]','','','[]','','','','current','[]')", []).unwrap();
    let mut insert = c.prepare("INSERT INTO diagnostics(path,code,details_json) VALUES('navigation.md','index_corrupt',?1)").unwrap();
    for n in 0..5000 {
        insert.execute([format!("{{\"n\":{n}}}")]).unwrap();
    }
    drop(insert);
    let mut delta = empty();
    delta.diagnostics.push(OwnedDiagnostics {
        path: VaultRelativePath::new("navigation.md").unwrap(),
        rows: vec![],
    });
    let changes = freeze(&c);
    let stats = preflight_graph_rows(&c, &delta).unwrap();
    assert_eq!(
        stats.old_rows, 5001,
        "all diagnostics plus mandatory layout"
    );
    assert_eq!(
        stats.old_fact_bytes,
        "layout".len() + "1".len(),
        "owned diagnostics charge count only"
    );
    unchanged(&c, changes);
}

#[test]
fn preflight_bookkeeping_matches_real_apply_for_existing_derived_owner() {
    let c = db();
    let delta = owner_delta(record());
    seed_owner(&c, &delta);
    let changes = freeze(&c);
    let before = preflight_graph_rows(&c, &delta).unwrap();
    unchanged(&c, changes);
    c.execute_batch("PRAGMA query_only=OFF; BEGIN IMMEDIATE")
        .unwrap();
    let after = delta
        .apply_with_allowance(&c, None, DeltaWriteAllowance::authenticated_graph_v2())
        .unwrap();
    assert_eq!(
        before, after,
        "accounting must follow actual apply loads and insert counts"
    );
    c.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn post_upsert_record_column_bytes_can_refuse_a_json_row_within_its_limit() {
    let c = db();
    let mut row = record();
    row.reasons = vec![String::new()];
    let fixed = counted(&row, MAX_ROW_BYTES).unwrap();
    row.reasons[0] = "x".repeat(MAX_ROW_BYTES - fixed);
    assert_eq!(counted(&row, MAX_ROW_BYTES).unwrap(), MAX_ROW_BYTES);
    let delta = owner_delta(row);
    delta
        .validate_with_allowance(DeltaWriteAllowance::authenticated_graph_v2())
        .unwrap();
    let changes = freeze(&c);
    assert_eq!(
        preflight_graph_rows(&c, &delta).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM records", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    unchanged(&c, changes);
}

#[test]
fn oversized_old_graph_fts_refuses_before_deleting_its_terms() {
    let c = db();
    let mut delta = owner_delta(record());
    seed_owner(&c, &delta);
    let row = &delta.records[0];
    c.execute("INSERT INTO graph_rows(target_id,target_kind,name,aliases_json,aliases_text,endpoints,predicate,qualifiers,description) VALUES(?1,?2,'Old','[]','','','','',?3)", params![row.record.id().as_str(), row.record.kind().as_str(), "x".repeat(MAX_ROW_BYTES)]).unwrap();
    delta.graph.push(GraphRow {
        target_id: row.record.id().clone(),
        target_kind: row.record.kind(),
        name: "New".into(),
        aliases: vec![],
        endpoints: String::new(),
        predicate: String::new(),
        qualifiers: String::new(),
        description: String::new(),
    });
    let changes = freeze(&c);
    assert_eq!(
        preflight_graph_rows(&c, &delta).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(
        c.query_row("SELECT name FROM graph_rows", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "Old"
    );
    unchanged(&c, changes);
}

#[test]
fn malformed_current_record_and_orphan_post_owner_refuse_readonly() {
    let c = db();
    let delta = owner_delta(record());
    seed_owner(&c, &delta);
    c.execute("UPDATE records SET row_json='{}'", []).unwrap();
    let changes = freeze(&c);
    assert_eq!(
        preflight_graph_rows(&c, &delta).unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
    unchanged(&c, changes);
    let c = db();
    let mut delta = empty();
    let mut orphan = owner_delta(record()).facts.unwrap().records.remove(0);
    orphan.record_id = RecordId::new("entity_absent").unwrap();
    delta.facts.as_mut().unwrap().records.push(orphan);
    let changes = freeze(&c);
    assert_eq!(
        preflight_graph_rows(&c, &delta).unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
    unchanged(&c, changes);
}

#[test]
fn stats_deserialization_does_not_grant_graph_old_row_allowance() {
    let mut stats = DeltaStats::default();
    stats.old_row_limit = GRAPH_V2_MAX_ROWS;
    let encoded = serde_json::to_string(&stats).unwrap();
    assert!(!encoded.contains("old_row_limit"));
    let decoded: DeltaStats = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.old_row_limit, super::normalized_delta::MAX_ROWS);
}
