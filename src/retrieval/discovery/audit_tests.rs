use super::*;
use crate::retrieval::{
    render::TargetKind,
    unit_inventory_types::{UnitDescriptor, UnitOwnerBinding},
};
fn fixture() -> (AuditScope, GraphRef, GraphRef, GraphNode, GraphNode) {
    let policy = RenderPolicyId(Blake3Hash::digest(b"policy"));
    let incarnation = Blake3Hash::digest(b"incarnation");
    let scope = AuditScope {
        space: Blake3Hash::digest(b"space"),
        policy: policy.clone(),
        incarnation: incarnation.clone(),
        required_seq: 3,
        since_seq: 0,
        expected_nodes: 2,
    };
    let input = GraphRef {
        format: FORMAT,
        kind: GraphKind::StagingInput,
        root: Blake3Hash::digest(b"input"),
        root_bytes: 100,
        directory_height: 1,
        node_count: 2,
        dimensions: 2,
        entry: Some(0),
        max_level: 0,
    };
    let mut output = input.clone();
    output.kind = GraphKind::Base;
    output.root = Blake3Hash::digest(b"output");
    let owner = VaultRelativePath::new("page.md").unwrap();
    let source = GraphNode {
        id: 0,
        descriptor: UnitDescriptor {
            policy: policy.clone(),
            owner: owner.clone(),
            target: TargetKind::Document,
            target_id: None,
            source_hash: Blake3Hash::digest(b"source"),
            source_span: None,
            unit_id: Blake3Hash::digest(b"unit"),
            input_hash: Blake3Hash::digest(b"input hash"),
        },
        binding: UnitOwnerBinding {
            incarnation,
            policy,
            owner,
            render_token: Blake3Hash::digest(b"token"),
            proof_version: 3,
            modified_seq: 3,
            tombstone: false,
            unit_count: 2,
        },
        vector: vec![1., 0.],
        neighbors: vec![],
    };
    let mut exported = source.clone();
    exported.neighbors = vec![vec![1]];
    (scope, input, output, source, exported)
}
fn meter() -> DiscoveryMeter {
    DiscoveryMeter::new(VectorReadBudget::new(Instant::now() + Duration::from_secs(20)).unwrap())
}
#[test]
fn durable_identity_and_vector_bits_are_checked_before_digest_acceptance() {
    let (_, _, output, source, mut exported) = fixture();
    validate_node(&source, &exported, 0, &output, &mut meter()).unwrap();
    exported.vector[1] = -0.0;
    assert!(validate_node(&source, &exported, 0, &output, &mut meter()).is_err());
    exported.vector[1] = 0.;
    exported.binding.render_token = Blake3Hash::digest(b"altered");
    assert!(validate_node(&source, &exported, 0, &output, &mut meter()).is_err());
    exported.binding = source.binding.clone();
    exported.descriptor.input_hash = Blake3Hash::digest(b"other input");
    assert!(validate_node(&source, &exported, 0, &output, &mut meter()).is_err());
}
#[test]
fn graph_range_duplicate_self_and_layer_controls_fail() {
    let (_, _, output, source, exported) = fixture();
    for neighbors in [
        vec![vec![2]],
        vec![vec![0]],
        vec![vec![1, 1]],
        vec![vec![1], vec![]],
    ] {
        let mut altered = exported.clone();
        altered.neighbors = neighbors;
        assert!(validate_node(&source, &altered, 0, &output, &mut meter()).is_err());
    }
}
#[test]
fn cursor_binding_and_export_digest_completion_are_independent_gates() {
    let (scope, input, output, _, node) = fixture();
    let mut cursor = AuditCursor::new(&scope, &input, &output).unwrap();
    assert!(cursor.finish(&initial_digest()).is_err());
    let digest = export_digest(&cursor.digest, &node).unwrap();
    cursor.digest = digest.clone();
    cursor.next_node = 2;
    cursor.last_key = Some((
        3,
        node.descriptor.owner.clone(),
        node.descriptor.unit_id.clone(),
    ));
    cursor.finish(&digest).unwrap();
    assert!(cursor
        .finish(&Blake3Hash::digest(b"wrong builder digest"))
        .is_err());
    let mut wrong = output.clone();
    wrong.root = Blake3Hash::digest(b"different root");
    assert!(cursor.validate(&scope, &input, &wrong).is_err());
    let mut changed = scope.clone();
    changed.since_seq = 1;
    assert!(cursor.validate(&changed, &input, &output).is_err());
}
#[test]
fn full_node_chain_covers_adjacency_and_descriptors_and_roundtrips_cursor() {
    let (scope, input, output, _, mut node) = fixture();
    let cursor = AuditCursor::new(&scope, &input, &output).unwrap();
    let hash = export_digest(&cursor.digest, &node).unwrap();
    node.neighbors[0].clear();
    assert_ne!(hash, export_digest(&cursor.digest, &node).unwrap());
    node.descriptor.source_hash = Blake3Hash::digest(b"changed source");
    assert_ne!(hash, export_digest(&cursor.digest, &node).unwrap());
    let encoded = serde_json::to_string(&cursor).unwrap();
    let decoded: AuditCursor = serde_json::from_str(&encoded).unwrap();
    decoded.validate(&scope, &input, &output).unwrap();
    let mut value = serde_json::to_value(cursor).unwrap();
    value["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<AuditCursor>(value).is_err());
}

struct Durable {
    _temp: tempfile::TempDir,
    catalog: crate::catalog::Catalog,
    scope: AuditScope,
    input: GraphRef,
    output: GraphRef,
    expected: Blake3Hash,
}
fn durable(variant: usize) -> Durable {
    use crate::{
        catalog::{
            file_types::{BuildIdentity, CatalogSelection},
            normalized_build::{BuildLimits, NormalizedBuilder},
            scan, selector,
        },
        retrieval::spaces::{EmbeddingSettings, SpaceSpec, RENDER_VERSION},
        vault::{VaultFs, VaultRoot, WriterPermit},
    };
    use std::fs;
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        b"---\nwiki_schema: '1'\nwiki_id: vault_audit\nwiki_kind: vault\ntitle: Audit\n---\n",
    )
    .unwrap();
    fs::write(temp.path().join("page.md"),b"---\nwiki_schema: '1'\nwiki_id: page_audit\nwiki_kind: page\ntitle: Page\nwiki_status: reviewed\n---\n# Page\nAudit fixture.\n").unwrap();
    let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let vault = RecordId::new("vault_audit").unwrap();
    let permit = WriterPermit::acquire(handle.root(), Duration::ZERO).unwrap();
    let catalog = crate::catalog::Catalog::new(handle.clone(), vault.clone());
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
    let settings = EmbeddingSettings::default();
    let inventory = catalog
        .prepare_unit_inventory_page(&permit, &settings, 128)
        .unwrap();
    assert!(inventory.complete);
    let spec = SpaceSpec {
        version: 1,
        endpoint_fingerprint: Blake3Hash::digest(b"mock"),
        profile_id: "test".into(),
        service_id: "mock".into(),
        model: "fixed".into(),
        revision: None,
        dimensions: Some(2),
        metric: "cosine".into(),
        normalization: "float64-l2-to-f32-le-v1".into(),
        render_version: RENDER_VERSION.into(),
        settings,
    };
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let unit = reader
        .unit_descriptors_page(&inventory.policy, None, 128)
        .unwrap()
        .pop()
        .unwrap();
    let binding = reader
        .unit_owner_binding(&inventory.policy, &unit.owner)
        .unwrap()
        .unwrap();
    drop(reader);
    let mut vectors = VectorStore::open(catalog.fs(), Some(&permit)).unwrap();
    let space = vectors.prepare_space(&spec).unwrap();
    vectors
        .put_batch(
            &space,
            &[unit.input_hash.clone()],
            &[vec![1., 0.]],
            true,
            &Blake3Hash::digest(b"mock input"),
        )
        .unwrap();
    // Ready paid vectors alone do not activate a space. Publish the complete
    // current owner acknowledgment through the ordinary activation API.
    let cursor = crate::retrieval::unit_inventory_types::PreparationCursor {
        version: crate::retrieval::unit_inventory_types::INVENTORY_VERSION,
        incarnation: binding.incarnation.clone(),
        policy: inventory.policy.clone(),
        since_seq: 0,
        through_seq: binding.modified_seq,
        after: None,
        complete: true,
    };
    vectors
        .acknowledge_unit_owners_checked(
            &space,
            &[(binding.clone(), vec![unit.clone()])],
            Some(&cursor),
            true,
            &spec,
            || Ok(()),
        )
        .unwrap();
    drop(vectors);
    let state = catalog
        .register_discovery(&permit, &space, &inventory.policy)
        .unwrap();
    let scope = AuditScope {
        space,
        policy: inventory.policy,
        incarnation: binding.incarnation.clone(),
        required_seq: state.required_seq,
        since_seq: 0,
        expected_nodes: 1,
    };
    let mut source = GraphNode {
        id: 0,
        descriptor: unit,
        binding,
        vector: vec![1., 0.],
        neighbors: vec![],
    };
    match variant {
        1 => source.vector = vec![0., 1.],
        2 => source.descriptor.unit_id = Blake3Hash::digest(b"forged staged descriptor"),
        3 => source.binding.render_token = Blake3Hash::digest(b"forged staged binding"),
        _ => {}
    }
    let mut exported = source.clone();
    exported.neighbors = vec![vec![]];
    let expected = export_digest(&initial_digest(), &exported).unwrap();
    let fingerprint = Blake3Hash::digest(b"frozen input");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut staging = storage::BuildState::new(
        fingerprint.clone(),
        GraphKind::StagingInput,
        2,
        1,
        Some(0),
        0,
    )
    .unwrap();
    staging
        .append(
            catalog.fs(),
            &permit,
            source,
            &fingerprint,
            16 * 1024 * 1024,
            deadline,
        )
        .unwrap();
    let input = staging
        .finish(
            catalog.fs(),
            &permit,
            &fingerprint,
            16 * 1024 * 1024,
            deadline,
        )
        .unwrap();
    let mut output =
        storage::BuildState::new(fingerprint.clone(), GraphKind::Base, 2, 1, Some(0), 0).unwrap();
    output
        .append(
            catalog.fs(),
            &permit,
            exported,
            &fingerprint,
            16 * 1024 * 1024,
            deadline,
        )
        .unwrap();
    let output = output
        .finish(
            catalog.fs(),
            &permit,
            &fingerprint,
            16 * 1024 * 1024,
            deadline,
        )
        .unwrap();
    Durable {
        _temp: temp,
        catalog,
        scope,
        input,
        output,
        expected,
    }
}
#[test]
fn durable_replay_matches_current_full_descriptor_binding_and_paid_vector() {
    let fixture = durable(0);
    let mut cursor = AuditCursor::new(&fixture.scope, &fixture.input, &fixture.output).unwrap();
    assert!(audit_page(
        &fixture.catalog,
        &fixture.scope,
        &fixture.input,
        &fixture.output,
        &mut cursor,
        Instant::now() + Duration::from_secs(20)
    )
    .unwrap());
    cursor.finish(&fixture.expected).unwrap();
}
#[test]
fn authenticated_but_wrong_current_descriptor_binding_or_paid_vector_never_advances_cursor() {
    for variant in 1..=3 {
        let fixture = durable(variant);
        let mut cursor = AuditCursor::new(&fixture.scope, &fixture.input, &fixture.output).unwrap();
        let before = serde_json::to_vec(&cursor).unwrap();
        let error = audit_page(
            &fixture.catalog,
            &fixture.scope,
            &fixture.input,
            &fixture.output,
            &mut cursor,
            Instant::now() + Duration::from_secs(20),
        )
        .unwrap_err();
        let expected = match variant {
            1 => "semantic audit vector differs from paid cache",
            2 => "semantic audit staged descriptor differs from current inventory",
            3 => "semantic audit owner binding changed",
            _ => unreachable!(),
        };
        assert_eq!(error.message, expected);
        assert_eq!(serde_json::to_vec(&cursor).unwrap(), before);
    }
}
#[test]
fn touched_durable_root_corruption_cannot_be_hidden_by_export_digest() {
    let fixture = durable(0);
    let mut cursor = AuditCursor::new(&fixture.scope, &fixture.input, &fixture.output).unwrap();
    let hash = fixture
        .output
        .root
        .as_str()
        .strip_prefix("blake3:")
        .unwrap();
    let path = fixture.catalog.fs().root().path().join(format!(
        ".wiki/cache/semantic/objects/{}/{}",
        &hash[..2],
        hash
    ));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(path, bytes).unwrap();
    assert!(audit_page(
        &fixture.catalog,
        &fixture.scope,
        &fixture.input,
        &fixture.output,
        &mut cursor,
        Instant::now() + Duration::from_secs(20)
    )
    .is_err());
    assert_eq!(cursor.next_node, 0);
}
