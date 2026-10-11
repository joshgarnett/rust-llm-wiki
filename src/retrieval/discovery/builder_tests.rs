use super::*;
use crate::retrieval::discovery::admission::{DiscoveryMaintenanceLimits, StorageGeometry};
fn admission(n: u32, d: u32) -> DiscoveryAdmission {
    DiscoveryAdmission::derive(
        Blake3Hash::digest("input"),
        n,
        d,
        DiscoveryMaintenanceLimits::default(),
        StorageGeometry {
            allocation_unit_bytes: 4096,
        },
    )
    .unwrap()
}
use crate::retrieval::render::TargetKind;
use crate::retrieval::unit_inventory_types::{RenderPolicyId, UnitDescriptor, UnitOwnerBinding};
use std::time::Duration;

fn node(id: u32) -> GraphNode {
    let policy = RenderPolicyId(Blake3Hash::digest("policy"));
    let owner = VaultRelativePath::new(format!("sources/{id}.md")).unwrap();
    GraphNode {
        id,
        descriptor: UnitDescriptor {
            policy: policy.clone(),
            owner: owner.clone(),
            target: TargetKind::Document,
            target_id: None,
            source_hash: Blake3Hash::digest(format!("source{id}")),
            source_span: None,
            unit_id: Blake3Hash::digest(format!("unit{id}")),
            input_hash: Blake3Hash::digest("shared"),
        },
        binding: UnitOwnerBinding {
            incarnation: Blake3Hash::digest("incarnation"),
            policy,
            owner,
            render_token: Blake3Hash::digest("render"),
            proof_version: 1,
            modified_seq: 1,
            tombstone: false,
            unit_count: 1,
        },
        vector: vec![1., 0.],
        neighbors: Vec::new(),
    }
}

#[test]
fn admission_rejects_dimension_and_cardinality_overflow() {
    assert!(admit(1, 0, &admission(1, 2)).is_err());
    assert!(admit(1, u32::MAX, &admission(1, 2)).is_err());
    assert!(admit(u32::MAX, 1536, &admission(1, 2)).is_err());
}

#[test]
fn directed_reachability_rejects_reverse_only_link() {
    let mut base = vec![u32::MAX; BASE_DEGREE * 2];
    base[BASE_DEGREE] = 0;
    assert!(reachable(
        &base,
        &[0, 1],
        Some(0),
        Instant::now() + Duration::from_secs(10)
    )
    .is_err());
    assert!(reachable(
        &base,
        &[0, 1],
        Some(1),
        Instant::now() + Duration::from_secs(10)
    )
    .is_ok());
}

#[test]
fn native_export_preserves_duplicate_vectors_as_distinct_nodes() {
    let mut emitted = Vec::new();
    let topology = build(
        2,
        2,
        &admission(2, 2),
        Instant::now() + Duration::from_secs(10),
        || Ok((0..2).map(|id| Ok(node(id)))),
        |n| {
            emitted.push(n);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(topology.node_count, 2);
    assert!(topology.entry.is_some());
    assert_eq!(emitted.len(), 2);
    assert_ne!(emitted[0].descriptor.owner, emitted[1].descriptor.owner);
    assert_eq!(emitted[0].vector, emitted[1].vector);
    assert!(emitted.iter().all(|n| n.neighbors[0].contains(&(1 - n.id))));
}

#[test]
fn changed_replay_metadata_cannot_complete_export() {
    let mut pass = 0;
    let result = build(
        1,
        2,
        &admission(1, 2),
        Instant::now() + Duration::from_secs(10),
        || {
            pass += 1;
            let mut n = node(0);
            if pass == 2 {
                n.binding.modified_seq = 2;
            }
            Ok(vec![Ok(n)].into_iter())
        },
        |_| Ok(()),
    );
    assert!(result.is_err());
}

#[test]
fn forged_admission_refuses_before_replay_or_native_construction() {
    let mut limits = admission(1, 2);
    limits.native_estimate_bytes -= 1;
    let result = build::<std::vec::IntoIter<Result<GraphNode>>, _, _>(
        1,
        2,
        &limits,
        Instant::now() + Duration::from_secs(1),
        || panic!("forged admission must not replay"),
        |_| panic!("forged admission must not emit"),
    );
    assert!(result.is_err());
    let mut geometry = admission(1, 2);
    geometry.dimensions = 3;
    assert!(admit(1, 2, &geometry).is_err());
}

#[test]
fn mixed_catalog_incarnation_is_rejected_without_collapsing_normal_owner_bindings() {
    let result = build(
        2,
        2,
        &admission(2, 2),
        Instant::now() + Duration::from_secs(10),
        || {
            let first = node(0);
            let mut second = node(1);
            second.binding.incarnation = Blake3Hash::digest("another-catalog");
            Ok(vec![Ok(first), Ok(second)].into_iter())
        },
        |_| Ok(()),
    );
    assert!(result.is_err());
}
