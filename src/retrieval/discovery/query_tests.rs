use super::*;
fn fixture() -> (
    RenderPolicyId,
    Blake3Hash,
    UnitDescriptor,
    UnitOwnerBinding,
    Projection,
) {
    let policy = RenderPolicyId(Blake3Hash::digest(b"policy"));
    let incarnation = Blake3Hash::digest(b"catalog incarnation");
    let owner = VaultRelativePath::new("page.md").unwrap();
    let hash = Blake3Hash::digest(b"current source");
    let unit = UnitDescriptor {
        policy: policy.clone(),
        owner: owner.clone(),
        target: TargetKind::Document,
        target_id: Some(RecordId::new("page_fixture").unwrap()),
        source_hash: hash.clone(),
        source_span: None,
        unit_id: Blake3Hash::digest(b"unit"),
        input_hash: Blake3Hash::digest(b"input"),
    };
    let binding = UnitOwnerBinding {
        incarnation: incarnation.clone(),
        policy: policy.clone(),
        owner,
        render_token: Blake3Hash::digest(b"render token"),
        proof_version: 5,
        modified_seq: 4,
        tombstone: false,
        unit_count: 2,
    };
    let projection = Projection {
        binding: binding.clone(),
        source_hash: hash.clone(),
        document_hash: Some(hash),
        target: unit.target_id.clone(),
        eligible: true,
    };
    (policy, incarnation, unit, binding, projection)
}
#[test]
fn exact_binding_classifies_before_filter_but_exclusion_does_not_skip_identity() {
    let (policy, incarnation, unit, binding, mut current) = fixture();
    assert_eq!(
        membership(&unit, &binding, Some(&current), &policy, &incarnation, 10).unwrap(),
        Membership::CurrentEligible
    );
    current.eligible = false;
    assert_eq!(
        membership(&unit, &binding, Some(&current), &policy, &incarnation, 10).unwrap(),
        Membership::CurrentExcluded
    );
    current.document_hash = Some(Blake3Hash::digest(b"wrong hash"));
    assert!(membership(&unit, &binding, Some(&current), &policy, &incarnation, 10).is_err());
}
#[test]
fn strict_supersession_and_missing_owner_remain_navigation_only() {
    let (policy, incarnation, unit, binding, mut current) = fixture();
    current.binding.proof_version += 1;
    current.binding.render_token = Blake3Hash::digest(b"new render");
    current.source_hash = Blake3Hash::digest(b"new source");
    current.document_hash = Some(current.source_hash.clone());
    assert_eq!(
        membership(&unit, &binding, Some(&current), &policy, &incarnation, 10).unwrap(),
        Membership::NavigationOnly
    );
    current.binding.modified_seq += 1;
    current.binding.tombstone = true;
    current.binding.unit_count = 0;
    assert_eq!(
        membership(&unit, &binding, Some(&current), &policy, &incarnation, 10).unwrap(),
        Membership::NavigationOnly
    );
    assert_eq!(
        membership(&unit, &binding, None, &policy, &incarnation, 10).unwrap(),
        Membership::NavigationOnly
    );
}
#[test]
fn equal_version_changed_token_count_and_tombstone_are_corruption() {
    for variant in 0..3 {
        let (policy, incarnation, unit, binding, mut current) = fixture();
        current.eligible = false;
        match variant {
            0 => current.binding.render_token = Blake3Hash::digest(b"wrong token"),
            1 => current.binding.unit_count += 1,
            _ => {
                current.binding.tombstone = true;
                current.binding.unit_count = 0;
            }
        }
        assert!(membership(&unit, &binding, Some(&current), &policy, &incarnation, 10).is_err());
    }
}
#[test]
fn incomparable_versions_and_future_nodes_fail_even_without_current_owner() {
    let (policy, incarnation, unit, mut binding, mut current) = fixture();
    current.binding.modified_seq += 1;
    binding.proof_version += 1;
    assert!(membership(&unit, &binding, Some(&current), &policy, &incarnation, 10).is_err());
    binding.proof_version = 11;
    assert!(membership(&unit, &binding, None, &policy, &incarnation, 10).is_err());
    binding.proof_version = 5;
    binding.incarnation = Blake3Hash::digest(b"old catalog");
    assert!(membership(&unit, &binding, None, &policy, &incarnation, 10).is_err());
}
#[test]
fn every_descriptor_checks_target_and_source_even_for_shared_input() {
    let (policy, incarnation, mut unit, binding, mut current) = fixture();
    current.eligible = false;
    unit.target_id = Some(RecordId::new("wrong_target").unwrap());
    assert!(membership(&unit, &binding, Some(&current), &policy, &incarnation, 10).is_err());
    unit.target_id = current.target.clone();
    unit.source_hash = Blake3Hash::digest(b"wrong source");
    assert!(membership(&unit, &binding, Some(&current), &policy, &incarnation, 10).is_err());
}
#[test]
fn explicit_subset_detection_and_sql_reuse_include_source_evidence_and_operational_exclusions() {
    let filter = SearchFilters::default();
    assert!(!explicit(&filter));
    let filter = SearchFilters {
        source_ids: vec![RecordId::new("source_fixture").unwrap()],
        tags: vec!["priority".into()],
        ..Default::default()
    };
    assert!(explicit(&filter));
    let mut values = vec![Value::Text("policy".into())];
    let sql = predicate(&filter, false, &mut values);
    assert!(sql.contains("source_evidence") && sql.contains("source_assertions"));
    assert!(sql.contains("json_each(d.tags_json)"));
    assert!(sql.contains("extraction_packet") && sql.contains("change"));
    assert_eq!(values.len(), 3);
}

fn normalized_fixture() -> (
    tempfile::TempDir,
    Catalog,
    crate::vault::WriterPermit,
    rusqlite::Connection,
    RenderPolicyId,
) {
    use crate::{
        catalog::{
            file_types::{BuildIdentity, CatalogSelection},
            normalized_build::{BuildLimits, NormalizedBuilder},
            scan, selector,
        },
        vault::{VaultFs, VaultRoot, WriterPermit},
    };
    use std::{fs, time::Duration};
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"),b"---\nwiki_schema: '1'\nwiki_id: vault_graph_query\nwiki_kind: vault\ntitle: Graph query\n---\n").unwrap();
    fs::write(temp.path().join("page.md"),b"---\nwiki_schema: '1'\nwiki_id: page_graph_query\nwiki_kind: page\ntitle: Page\nwiki_status: reviewed\ntags: [selected]\n---\n# Page\nFixture paragraph.\n").unwrap();
    let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let vault = RecordId::new("vault_graph_query").unwrap();
    let permit = WriterPermit::acquire(handle.root(), Duration::ZERO).unwrap();
    let catalog = Catalog::new(handle.clone(), vault.clone());
    let identity = BuildIdentity {
        selection: CatalogSelection::new(vault.clone(), 1).unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: false,
    };
    selector::prepare(&handle, &permit, &identity.selection).unwrap();
    let mut builder =
        NormalizedBuilder::begin(&handle, &permit, identity, BuildLimits::default()).unwrap();
    let input = scan::scan_input(&handle, &vault).unwrap();
    let projection =
        scan::project_normalized_with_sink(&handle, &input, false, &mut builder).unwrap();
    let completed = builder.finish_normalized(&projection).unwrap();
    selector::publish(
        &handle,
        &permit,
        &completed.identity.selection,
        Duration::from_secs(1),
    )
    .unwrap();
    let inventory = catalog
        .prepare_unit_inventory_page(
            &permit,
            &crate::retrieval::spaces::EmbeddingSettings::default(),
            128,
        )
        .unwrap();
    assert!(inventory.complete);
    let c = rusqlite::Connection::open(completed.path).unwrap();
    (temp, catalog, permit, c, inventory.policy)
}
#[test]
fn joined_projection_is_once_per_owner_and_every_descriptor_is_validated() {
    let (_temp, catalog, _permit, _c, policy) = normalized_fixture();
    let reader = catalog
        .cached_query_snapshot(crate::catalog::query_types::QueryReadLimits::default())
        .unwrap();
    let units = reader.unit_descriptors_page(&policy, None, 128).unwrap();
    assert_eq!(units.len(), 1);
    let binding = reader
        .unit_owner_binding(&policy, &units[0].owner)
        .unwrap()
        .unwrap();
    let filter = SearchFilters {
        tags: vec!["absent".into()],
        ..Default::default()
    };
    let mut resolver = Resolver {
        reader: &reader,
        filter: &filter,
        context: false,
        policy: &policy,
        incarnation: binding.incarnation.clone(),
        owners: BTreeMap::new(),
        rows: 0,
        retained: 0,
    };
    let mut node = GraphNode {
        id: 0,
        descriptor: units[0].clone(),
        binding,
        vector: vec![1., 0.],
        neighbors: vec![vec![]],
    };
    assert_eq!(
        resolver.classify(&node).unwrap(),
        Membership::CurrentExcluded
    );
    assert_eq!(
        resolver.classify(&node).unwrap(),
        Membership::CurrentExcluded
    );
    assert_eq!(resolver.rows, 1);
    node.descriptor.source_hash = Blake3Hash::digest(b"different descriptor");
    assert!(resolver.classify(&node).is_err());
    assert_eq!(resolver.rows, 1);
}
#[test]
fn selective_probe_charges_513_sentinel_and_preserves_shared_row_limit() {
    let (_temp, catalog, _permit, c, policy) = normalized_fixture();
    let reader = catalog
        .cached_query_snapshot(crate::catalog::query_types::QueryReadLimits::default())
        .unwrap();
    let original = reader
        .unit_descriptors_page(&policy, None, 128)
        .unwrap()
        .pop()
        .unwrap();
    drop(reader);
    c.execute(
        "DELETE FROM retrieval_units WHERE policy=?1",
        [policy.as_str()],
    )
    .unwrap();
    for index in 0..513 {
        let mut unit = original.clone();
        unit.unit_id = Blake3Hash::digest(index.to_string());
        c.execute(
            "INSERT INTO retrieval_units VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![
                policy.as_str(),
                unit.owner.as_str(),
                unit.unit_id.as_str(),
                unit.input_hash.as_str(),
                serde_json::to_string(&unit).unwrap()
            ],
        )
        .unwrap();
    }
    c.execute(
        "UPDATE unit_owners SET unit_count=513 WHERE policy=?1",
        [policy.as_str()],
    )
    .unwrap();
    c.execute(
        "UPDATE unit_policies SET unit_count=513 WHERE policy=?1",
        [policy.as_str()],
    )
    .unwrap();
    let reader = catalog
        .cached_query_snapshot(crate::catalog::query_types::QueryReadLimits::default())
        .unwrap();
    let filter = SearchFilters {
        tags: vec!["selected".into()],
        ..Default::default()
    };
    let incarnation = catalog_incarnation(reader.vault_id(), reader.snapshot()).unwrap();
    let mut resolver = Resolver {
        reader: &reader,
        filter: &filter,
        context: false,
        policy: &policy,
        incarnation,
        owners: BTreeMap::new(),
        rows: 0,
        retained: 0,
    };
    let shared =
        VectorReadBudget::new(std::time::Instant::now() + std::time::Duration::from_secs(20))
            .unwrap();
    let mut meter = DiscoveryMeter::new(shared);
    assert!(probe(&mut resolver, &mut meter).unwrap().is_none());
    assert_eq!(resolver.rows, 513);
    resolver.rows = DISCOVERY_ROWS;
    assert!(probe(&mut resolver, &mut meter).is_err());
}
#[test]
fn exact_subset_scores_paid_vectors_in_parent_budget_without_graph_reads() {
    let (_temp, catalog, permit, _c, policy) = normalized_fixture();
    let reader = catalog
        .cached_query_snapshot(crate::catalog::query_types::QueryReadLimits::default())
        .unwrap();
    let units = reader.unit_descriptors_page(&policy, None, 128).unwrap();
    let unit = units[0].clone();
    let settings = crate::retrieval::spaces::EmbeddingSettings::default();
    let spec = crate::retrieval::spaces::SpaceSpec {
        version: 1,
        endpoint_fingerprint: Blake3Hash::digest(b"mock endpoint"),
        profile_id: "test".into(),
        service_id: "mock".into(),
        model: "fixed".into(),
        revision: None,
        dimensions: Some(2),
        metric: "cosine".into(),
        normalization: "float64-l2-to-f32-le-v1".into(),
        render_version: crate::retrieval::spaces::RENDER_VERSION.into(),
        settings,
    };
    let mut writer = VectorStore::open(catalog.fs(), Some(&permit)).unwrap();
    let space = writer.prepare_space(&spec).unwrap();
    writer
        .put_batch(
            &space,
            &[unit.input_hash.clone()],
            &[vec![1., 0.]],
            true,
            &Blake3Hash::digest(b"mock input guard"),
        )
        .unwrap();
    let binding = reader
        .unit_owner_binding(&policy, &unit.owner)
        .unwrap()
        .unwrap();
    let cursor = crate::retrieval::unit_inventory_types::PreparationCursor {
        version: crate::retrieval::unit_inventory_types::INVENTORY_VERSION,
        incarnation: binding.incarnation.clone(),
        policy: policy.clone(),
        since_seq: 0,
        through_seq: binding.modified_seq,
        after: None,
        complete: true,
    };
    writer
        .acknowledge_unit_owners_checked(
            &space,
            &[(binding, vec![unit.clone()])],
            Some(&cursor),
            true,
            &spec,
            || Ok(()),
        )
        .unwrap();
    let state = writer.active().unwrap().unwrap();
    drop(writer);
    let shared =
        VectorReadBudget::new(std::time::Instant::now() + std::time::Duration::from_secs(20))
            .unwrap();
    let store = VectorStore::open_bounded_snapshot(catalog.fs(), &shared).unwrap();
    let filter = SearchFilters {
        tags: vec!["selected".into()],
        ..Default::default()
    };
    let mut resolver = Resolver {
        reader: &reader,
        filter: &filter,
        context: false,
        policy: &policy,
        incarnation: catalog_incarnation(reader.vault_id(), reader.snapshot()).unwrap(),
        owners: BTreeMap::new(),
        rows: 0,
        retained: 0,
    };
    let mut meter = DiscoveryMeter::new(shared);
    let units = probe(&mut resolver, &mut meter).unwrap().unwrap();
    let result = exact_subset(
        units,
        &mut resolver,
        &store,
        &state,
        &[1., 0.],
        1,
        &mut meter,
    )
    .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].score, 1.);
    assert!(!result.approximate);
    assert_eq!(meter.usage().distance_evaluations, 1);
    assert_eq!(meter.usage().graph_reads, 0);
    assert_eq!(resolver.rows, 2);
}
