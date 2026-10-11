//! Disposable normalized-publication selection and local transaction controls.
use super::*;
use crate::{
    catalog::{
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        scan,
    },
    retrieval::{
        discovery::{DiscoveryCoverage, GraphRef},
        spaces::EmbeddingSettings,
    },
    vault::{VaultFs, VaultRoot},
};
use std::fs;
fn fixture() -> (
    tempfile::TempDir,
    Catalog,
    WriterPermit,
    Connection,
    Blake3Hash,
    RenderPolicyId,
) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"),b"---\nwiki_schema: '1'\nwiki_id: vault_discovery\nwiki_kind: vault\ntitle: Discovery\n---\n").unwrap();
    fs::write(temp.path().join("page.md"),b"---\nwiki_schema: '1'\nwiki_id: page_discovery\nwiki_kind: page\ntitle: Page\nwiki_status: reviewed\n---\n# Page\nFixture content.\n").unwrap();
    let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let vault = RecordId::new("vault_discovery").unwrap();
    let writer = WriterPermit::acquire(handle.root(), Duration::ZERO).unwrap();
    let catalog = Catalog::new(handle.clone(), vault.clone());
    let identity = BuildIdentity {
        selection: CatalogSelection::new(vault.clone(), 1).unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: false,
    };
    selector::prepare(&handle, &writer, &identity.selection).unwrap();
    let mut builder =
        NormalizedBuilder::begin(&handle, &writer, identity, BuildLimits::default()).unwrap();
    let input = scan::scan_input(&handle, &vault).unwrap();
    let projection =
        scan::project_normalized_with_sink(&handle, &input, false, &mut builder).unwrap();
    let completed = builder.finish_normalized(&projection).unwrap();
    selector::publish(
        &handle,
        &writer,
        &completed.identity.selection,
        Duration::from_secs(1),
    )
    .unwrap();
    let settings = EmbeddingSettings::default();
    let state = catalog
        .prepare_unit_inventory_page(&writer, &settings, 128)
        .unwrap();
    assert!(state.complete);
    let database = Connection::open(completed.path).unwrap();
    (
        temp,
        catalog,
        writer,
        database,
        Blake3Hash::digest(b"space"),
        state.policy,
    )
}
fn manifest(
    catalog: &Catalog,
    space: &Blake3Hash,
    policy: &RenderPolicyId,
    state: &DiscoveryState,
) -> DiscoveryManifest {
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let eligible = state.eligible_current_units;
    DiscoveryManifest {
        format: FORMAT,
        algorithm: crate::retrieval::discovery::algorithm_identity(),
        space: space.clone(),
        policy: policy.clone(),
        incarnation: catalog_incarnation(reader.vault_id(), reader.snapshot()).unwrap(),
        through_seq: state.required_seq,
        base_through_seq: state.required_seq,
        base: Some(GraphRef {
            format: FORMAT,
            kind: GraphKind::Base,
            root: Blake3Hash::digest(b"authenticated root"),
            root_bytes: 100,
            directory_height: 1,
            node_count: eligible as u32,
            dimensions: 2,
            entry: if eligible == 0 { None } else { Some(0) },
            max_level: 0,
        }),
        delta: None,
        coverage: DiscoveryCoverage {
            eligible_current_units: eligible,
            compatible_paid_units: eligible,
            indexed_current_units: eligible,
        },
    }
}
#[test]
fn normalized_selection_clears_matching_stage_without_changing_canonical_header() {
    let (_temp, catalog, writer, _database, space, policy) = fixture();
    let before = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap()
        .snapshot()
        .clone();
    let state = catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    let value = manifest(&catalog, &space, &policy, &state);
    catalog
        .save_discovery_stage(
            &writer,
            &space,
            &policy,
            &value.incarnation,
            state.required_seq,
            &serde_json::json!({"checked":true}),
        )
        .unwrap();
    assert!(catalog
        .discovery_stage(
            &writer,
            &space,
            &policy,
            &value.incarnation,
            state.required_seq
        )
        .unwrap()
        .is_some());
    seed_completed_authority(&catalog, &writer, &value);
    catalog.publish_discovery(&writer, &value).unwrap();
    assert!(catalog
        .discovery_stage(
            &writer,
            &space,
            &policy,
            &value.incarnation,
            state.required_seq
        )
        .unwrap()
        .is_none());
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let rows = reader.usage().rows;
    assert_eq!(
        reader
            .discovery_state(&space, &policy)
            .unwrap()
            .unwrap()
            .manifest,
        Some(value)
    );
    assert_eq!(reader.usage().rows - rows, 2);
    assert_eq!(reader.snapshot(), &before);
}
#[test]
fn invalid_publication_and_oversized_stage_preserve_selected_state() {
    let (_temp, catalog, writer, _database, space, policy) = fixture();
    let state = catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    let selected = manifest(&catalog, &space, &policy, &state);
    seed_completed_authority(&catalog, &writer, &selected);
    catalog.publish_discovery(&writer, &selected).unwrap();
    let mut invalid = selected.clone();
    invalid.coverage.indexed_current_units += 1;
    assert!(catalog.publish_discovery(&writer, &invalid).is_err());
    assert!(catalog
        .save_discovery_stage(
            &writer,
            &space,
            &policy,
            &selected.incarnation,
            state.required_seq,
            &serde_json::json!("x".repeat(JSON_LIMIT + 1))
        )
        .is_err());
    assert!(catalog
        .save_discovery_stage(
            &writer,
            &space,
            &policy,
            &Blake3Hash::digest(b"rebuild incarnation"),
            state.required_seq,
            &serde_json::json!({})
        )
        .is_err());
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        reader
            .discovery_state(&space, &policy)
            .unwrap()
            .unwrap()
            .manifest,
        Some(selected)
    );
}
#[test]
fn missing_capability_is_explicit_and_wrong_incarnation_is_rejected() {
    let (_temp, catalog, writer, database, space, policy) = fixture();
    assert!(catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap()
        .discovery_state(&space, &policy)
        .unwrap()
        .is_none());
    catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    database
        .execute(
            "UPDATE semantic_discovery_state SET incarnation=?1",
            [Blake3Hash::digest(b"old rebuilt catalog").as_str()],
        )
        .unwrap();
    assert!(catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap()
        .discovery_state(&space, &policy)
        .is_err());
}
#[test]
fn invalidation_is_in_caller_transaction_preserves_manifest_and_staging_evidence() {
    let c = Connection::open_in_memory().unwrap();
    let policy = RenderPolicyId(Blake3Hash::digest(b"policy"));
    invalidate(&c, &policy, 2).unwrap(); // Older disposable cache has no capability.
    c.execute_batch(
        "CREATE TABLE unit_policies(policy TEXT PRIMARY KEY,unit_count INTEGER NOT NULL);",
    )
    .unwrap();
    c.execute_batch(SCHEMA).unwrap();
    c.execute("INSERT INTO unit_policies VALUES(?1,3)", [policy.as_str()])
        .unwrap();
    c.execute(
        "INSERT INTO semantic_discovery_state VALUES('space',?1,'incarnation',1,1,'old-manifest')",
        [policy.as_str()],
    )
    .unwrap();
    c.execute(
        "INSERT INTO semantic_discovery_stages VALUES('space',?1,'incarnation',1,'old-stage')",
        [policy.as_str()],
    )
    .unwrap();
    {
        let tx = Transaction::new_unchecked(&c, TransactionBehavior::Immediate).unwrap();
        invalidate(&tx, &policy, 2).unwrap();
    }
    assert_eq!(
        c.query_row(
            "SELECT required_seq FROM semantic_discovery_state",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    {
        let tx = Transaction::new_unchecked(&c, TransactionBehavior::Immediate).unwrap();
        invalidate(&tx, &policy, 2).unwrap();
        tx.commit().unwrap();
    }
    let (seq, total, json): (i64, i64, String) = c
        .query_row(
            "SELECT required_seq,eligible_units,manifest_json FROM semantic_discovery_state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((seq, total, json), (2, 3, "old-manifest".into()));
    assert_eq!(
        c.query_row(
            "SELECT stage_json FROM semantic_discovery_stages",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "old-stage"
    );
}
#[test]
fn graph_reference_interval_and_delta_bounds_are_publication_blockers() {
    let (_temp, catalog, writer, _database, space, policy) = fixture();
    let state = catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    let selected = manifest(&catalog, &space, &policy, &state);
    let mut invalid = selected.clone();
    invalid.base_through_seq = invalid.through_seq + 1;
    assert!(catalog.publish_discovery(&writer, &invalid).is_err());
    let mut invalid = selected.clone();
    invalid.base.as_mut().unwrap().root_bytes = 0;
    assert!(catalog.publish_discovery(&writer, &invalid).is_err());
    let mut invalid = selected.clone();
    let mut delta = invalid.base.clone().unwrap();
    delta.kind = GraphKind::Delta;
    delta.node_count = 513;
    invalid.delta = Some(delta);
    assert!(catalog.publish_discovery(&writer, &invalid).is_err());
    let mut invalid = selected.clone();
    invalid.base = None;
    assert!(catalog.publish_discovery(&writer, &invalid).is_err());
}

#[test]
fn obsolete_private_checkpoint_is_ignored_and_guardedly_replaced() {
    let (_temp, catalog, writer, database, space, policy) = fixture();
    let state = catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    let value = manifest(&catalog, &space, &policy, &state);
    database
        .execute(
            "INSERT INTO semantic_discovery_stages VALUES(?1,?2,?3,?4,?5)",
            params![
                space.as_str(),
                policy.as_str(),
                Blake3Hash::digest(b"obsolete incarnation").as_str(),
                sql::integer(state.required_seq).unwrap(),
                "{\"old\":true}"
            ],
        )
        .unwrap();
    assert!(catalog
        .discovery_stage(
            &writer,
            &space,
            &policy,
            &value.incarnation,
            state.required_seq
        )
        .unwrap()
        .is_none());
    let replacement = serde_json::json!({"current":true});
    catalog
        .save_discovery_stage(
            &writer,
            &space,
            &policy,
            &value.incarnation,
            state.required_seq,
            &replacement,
        )
        .unwrap();
    assert_eq!(
        catalog
            .discovery_stage(
                &writer,
                &space,
                &policy,
                &value.incarnation,
                state.required_seq
            )
            .unwrap(),
        Some(replacement)
    );
}
#[test]
fn stale_manifest_retains_old_coverage_without_claiming_current_preparation() {
    let (_temp, catalog, writer, database, space, policy) = fixture();
    let state = catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    let value = manifest(&catalog, &space, &policy, &state);
    seed_completed_authority(&catalog, &writer, &value);
    catalog.publish_discovery(&writer, &value).unwrap();
    // Mechanical same-transaction control. Full canonical publication integration
    // belongs to the orchestrator's ordinary update/delete workflow replay.
    let tx = Transaction::new_unchecked(&database, TransactionBehavior::Immediate).unwrap();
    tx.execute(
        "UPDATE unit_policies SET unit_count=unit_count+1 WHERE policy=?1",
        [policy.as_str()],
    )
    .unwrap();
    invalidate(&tx, &policy, state.required_seq + 1).unwrap();
    tx.commit().unwrap();
    let pending = read_state(
        &database,
        &space,
        &policy,
        &value.incarnation,
        state.required_seq + 1,
    )
    .unwrap()
    .unwrap();
    assert_eq!(pending.manifest, Some(value));
    assert!(pending.required_seq > pending.manifest.unwrap().through_seq);
}

fn audit_fixture(
    catalog: &Catalog,
    space: &Blake3Hash,
    policy: &RenderPolicyId,
    state: &DiscoveryState,
) -> (
    AuditScope,
    GraphRef,
    GraphRef,
    Blake3Hash,
    AuditCursor,
    serde_json::Value,
) {
    let value = manifest(catalog, space, policy, state);
    let output = value.base.unwrap();
    let mut input = output.clone();
    input.kind = GraphKind::StagingInput;
    input.root = Blake3Hash::digest(b"staging input root");
    let scope = AuditScope {
        space: space.clone(),
        policy: policy.clone(),
        incarnation: value.incarnation,
        required_seq: state.required_seq,
        since_seq: 0,
        expected_nodes: output.node_count,
    };
    let cursor = AuditCursor::new(&scope, &input, &output).unwrap();
    let expected = Blake3Hash::digest(b"fresh checked child complete digest");
    let stage = serde_json::json!({"audit":cursor,"output_root":output,"output_digest":expected});
    (scope, input, output, expected, cursor, stage)
}
fn completed(scope: &AuditScope, zero: &AuditCursor, expected: &Blake3Hash) -> AuditCursor {
    let mut value = serde_json::to_value(zero).unwrap();
    value["next_node"] = serde_json::json!(scope.expected_nodes);
    value["digest"] = serde_json::json!(expected);
    value["last_key"] = serde_json::json!([
        scope.required_seq,
        "page.md",
        Blake3Hash::digest(b"last unit")
    ]);
    serde_json::from_value(value).unwrap()
}
#[test]
fn stage_claimed_complete_digest_cannot_initialize_or_skip_authoritative_progress() {
    let (_temp, catalog, writer, _database, space, policy) = fixture();
    let state = catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    let (scope, input, output, expected, zero, stage) =
        audit_fixture(&catalog, &space, &policy, &state);
    let forged = completed(&scope, &zero, &expected);
    assert!(catalog
        .verify_discovery_audit(&writer, &scope, &input, &output, &expected, &forged)
        .is_err());
    let mut forged_stage = stage.clone();
    forged_stage["audit"] = serde_json::to_value(&forged).unwrap();
    assert!(catalog
        .save_discovery_stage(
            &writer,
            &space,
            &policy,
            &scope.incarnation,
            scope.required_seq,
            &forged_stage
        )
        .is_err());
    assert!(catalog
        .begin_discovery_audit(&writer, &scope, &input, &output, &expected, &forged_stage)
        .is_err());
    catalog
        .begin_discovery_audit(&writer, &scope, &input, &output, &expected, &stage)
        .unwrap();
    assert!(catalog
        .verify_discovery_audit(&writer, &scope, &input, &output, &expected, &forged)
        .is_err());
    assert!(catalog
        .save_discovery_stage(
            &writer,
            &space,
            &policy,
            &scope.incarnation,
            scope.required_seq,
            &forged_stage
        )
        .is_err());
    assert!(catalog
        .publish_discovery(&writer, &manifest(&catalog, &space, &policy, &state))
        .is_err());
}
#[test]
fn audited_page_and_stage_commit_with_exact_prior_compare_and_swap() {
    let (_temp, catalog, writer, database, space, policy) = fixture();
    let state = catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    let (scope, input, output, expected, zero, stage) =
        audit_fixture(&catalog, &space, &policy, &state);
    catalog
        .begin_discovery_audit(&writer, &scope, &input, &output, &expected, &stage)
        .unwrap();
    assert!(catalog
        .begin_discovery_audit(&writer, &scope, &input, &output, &expected, &stage)
        .is_err());
    let next = completed(&scope, &zero, &expected);
    let mut updated = stage.clone();
    updated["audit"] = serde_json::to_value(&next).unwrap();
    catalog
        .advance_discovery_audit(
            &writer, &scope, &input, &output, &expected, &zero, &next, &updated,
        )
        .unwrap();
    assert!(catalog
        .advance_discovery_audit(
            &writer, &scope, &input, &output, &expected, &zero, &next, &updated
        )
        .is_err());
    catalog
        .verify_discovery_audit(&writer, &scope, &input, &output, &expected, &next)
        .unwrap();
    catalog
        .advance_discovery_audit(
            &writer, &scope, &input, &output, &expected, &next, &next, &updated,
        )
        .unwrap();
    catalog
        .publish_discovery(&writer, &manifest(&catalog, &space, &policy, &state))
        .unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM semantic_discovery_audits",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM semantic_discovery_stages",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
#[test]
fn typed_prefix_fields_and_expected_export_identity_cannot_disagree_with_cursor() {
    let (_temp, catalog, writer, database, space, policy) = fixture();
    let state = catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    let (scope, input, output, expected, zero, stage) =
        audit_fixture(&catalog, &space, &policy, &state);
    catalog
        .begin_discovery_audit(&writer, &scope, &input, &output, &expected, &stage)
        .unwrap();
    assert!(catalog
        .verify_discovery_audit(
            &writer,
            &scope,
            &input,
            &output,
            &Blake3Hash::digest(b"other digest"),
            &zero
        )
        .is_err());
    let mut other = output.clone();
    other.root = Blake3Hash::digest(b"other output root");
    assert!(catalog
        .verify_discovery_audit(&writer, &scope, &input, &other, &expected, &zero)
        .is_err());
    database
        .execute("UPDATE semantic_discovery_audits SET next_node=1", [])
        .unwrap();
    assert!(catalog
        .verify_discovery_audit(&writer, &scope, &input, &output, &expected, &zero)
        .is_err());
    assert!(catalog
        .save_discovery_stage(
            &writer,
            &space,
            &policy,
            &scope.incarnation,
            scope.required_seq,
            &stage
        )
        .is_err());
}
#[test]
fn obsolete_authority_is_retired_only_by_guarded_current_stage_replacement() {
    let (_temp, catalog, writer, database, space, policy) = fixture();
    let state = catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    let (scope, input, output, expected, _zero, stage) =
        audit_fixture(&catalog, &space, &policy, &state);
    catalog
        .begin_discovery_audit(&writer, &scope, &input, &output, &expected, &stage)
        .unwrap();
    database
        .execute(
            "UPDATE semantic_discovery_audits SET incarnation=?1",
            [Blake3Hash::digest(b"obsolete incarnation").as_str()],
        )
        .unwrap();
    let replacement = serde_json::json!({"audit":null,"phase":"new checked inputs"});
    assert!(catalog
        .save_discovery_stage(
            &writer,
            &space,
            &policy,
            &Blake3Hash::digest(b"wrong current"),
            scope.required_seq,
            &replacement
        )
        .is_err());
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM semantic_discovery_audits",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    catalog
        .save_discovery_stage(
            &writer,
            &space,
            &policy,
            &scope.incarnation,
            scope.required_seq,
            &replacement,
        )
        .unwrap();
    assert_eq!(
        database
            .query_row(
                "SELECT count(*) FROM semantic_discovery_audits",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn page_api_rejects_unchecked_large_skip_and_zero_progress_before_completion() {
    let (_temp, catalog, writer, _database, space, policy) = fixture();
    let state = catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    let (mut scope, mut input, mut output, expected, _zero, _stage) =
        audit_fixture(&catalog, &space, &policy, &state);
    scope.expected_nodes = 200;
    input.node_count = 200;
    output.node_count = 200;
    let zero = AuditCursor::new(&scope, &input, &output).unwrap();
    let stage = serde_json::json!({"audit":zero,"output_root":output,"output_digest":expected});
    catalog
        .begin_discovery_audit(&writer, &scope, &input, &output, &expected, &stage)
        .unwrap();
    assert!(catalog
        .advance_discovery_audit(&writer, &scope, &input, &output, &expected, &zero, &zero, &stage)
        .is_err());
    let skipped = completed(&scope, &zero, &expected);
    let mut tampered = stage.clone();
    tampered["audit"] = serde_json::to_value(&skipped).unwrap();
    assert!(catalog
        .advance_discovery_audit(
            &writer, &scope, &input, &output, &expected, &zero, &skipped, &tampered
        )
        .is_err());
    catalog
        .verify_discovery_audit(&writer, &scope, &input, &output, &expected, &zero)
        .unwrap();
}

// Explicit private SQL fixture for the catalog selection tests above. This does
// not pretend synthetic JSON was validated by audit_page or a native child.
fn seed_completed_authority(catalog: &Catalog, writer: &WriterPermit, value: &DiscoveryManifest) {
    let output = value
        .base
        .as_ref()
        .or(value.delta.as_ref())
        .unwrap()
        .clone();
    let mut input = output.clone();
    input.kind = GraphKind::StagingInput;
    input.root = Blake3Hash::digest(b"fixture input root");
    let scope = AuditScope {
        space: value.space.clone(),
        policy: value.policy.clone(),
        incarnation: value.incarnation.clone(),
        required_seq: value.through_seq,
        since_seq: 0,
        expected_nodes: output.node_count,
    };
    let expected = Blake3Hash::digest(b"explicit mechanical completed authority");
    let zero = AuditCursor::new(&scope, &input, &output).unwrap();
    let cursor = completed(&scope, &zero, &expected);
    let fields = AuditFields::new(&scope, &input, &output, &expected, &cursor).unwrap();
    let stage = serde_json::json!({"audit":cursor,"output_root":output,"output_digest":expected});
    let json = bounded_json(&stage).unwrap();
    catalog
        .discovery_write(writer, |tx, _snapshot, _current| {
            tx.execute_batch(SCHEMA).map_err(sql::sql_error)?;
            write_audit(tx, &scope, &fields, None)?;
            write_stage(tx, &scope, &json)
        })
        .unwrap();
}
#[test]
fn newly_selected_nonempty_root_without_authority_is_always_rejected() {
    let (_temp, catalog, writer, _database, space, policy) = fixture();
    let state = catalog
        .register_discovery(&writer, &space, &policy)
        .unwrap();
    let selected = manifest(&catalog, &space, &policy, &state);
    assert!(catalog.publish_discovery(&writer, &selected).is_err());
    seed_completed_authority(&catalog, &writer, &selected);
    catalog.publish_discovery(&writer, &selected).unwrap();
    let mut changed = selected.clone();
    changed.base.as_mut().unwrap().root = Blake3Hash::digest(b"new unverified graph");
    assert!(catalog.publish_discovery(&writer, &changed).is_err());
    catalog.publish_discovery(&writer, &selected).unwrap(); // Exact retained base.
    let mut algorithm = selected.clone();
    algorithm.algorithm = Blake3Hash::digest(b"wrong algorithm identity");
    assert!(catalog.publish_discovery(&writer, &algorithm).is_err());
}

#[test]
fn audited_root_cannot_carry_a_second_unaudited_root_through_publication() {
    for delta in [false, true] {
        let (_temp, catalog, writer, database, space, policy) = fixture();
        let state = catalog
            .register_discovery(&writer, &space, &policy)
            .unwrap();
        let mut selected = manifest(&catalog, &space, &policy, &state);
        if delta {
            let mut graph = selected.base.take().unwrap();
            graph.kind = GraphKind::Delta;
            selected.delta = Some(graph);
            selected.base_through_seq = 0;
        }
        seed_completed_authority(&catalog, &writer, &selected);
        let mut injected = selected.clone();
        let mut extra = selected
            .base
            .as_ref()
            .or(selected.delta.as_ref())
            .unwrap()
            .clone();
        extra.root = Blake3Hash::digest(b"unaudited second graph root");
        if delta {
            extra.kind = GraphKind::Base;
            injected.base = Some(extra);
        } else {
            extra.kind = GraphKind::Delta;
            injected.delta = Some(extra);
        }
        assert!(catalog.publish_discovery(&writer, &injected).is_err());
        assert!(catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap()
            .discovery_state(&space, &policy)
            .unwrap()
            .unwrap()
            .manifest
            .is_none());
        assert_eq!(
            database
                .query_row(
                    "SELECT count(*) FROM semantic_discovery_audits",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            database
                .query_row(
                    "SELECT count(*) FROM semantic_discovery_stages",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        catalog.publish_discovery(&writer, &selected).unwrap();
    }
}
