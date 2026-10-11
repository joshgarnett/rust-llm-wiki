use super::*;
use crate::retrieval::{
    render::TargetKind,
    unit_inventory_types::{RenderPolicyId, UnitDescriptor, UnitOwnerBinding},
};
use std::time::{Duration, Instant};
struct Fixture {
    reference: GraphRef,
    nodes: Vec<GraphNode>,
}
impl GraphAccess for Fixture {
    fn reference(&self) -> &GraphRef {
        &self.reference
    }
    fn node(&mut self, id: NodeId, meter: &mut DiscoveryMeter) -> Result<GraphNode> {
        meter.read(1024, self.nodes[id as usize].vector.len() * 4)?;
        Ok(self.nodes[id as usize].clone())
    }
}
fn fixture(owners: &[&str], vectors: &[[f32; 2]]) -> Fixture {
    let policy = RenderPolicyId(Blake3Hash::digest(b"policy"));
    let nodes = owners
        .iter()
        .enumerate()
        .map(|(id, owner)| {
            let owner = VaultRelativePath::new(*owner).unwrap();
            GraphNode {
                id: id as u32,
                descriptor: UnitDescriptor {
                    policy: policy.clone(),
                    owner: owner.clone(),
                    target: TargetKind::Document,
                    target_id: None,
                    source_hash: Blake3Hash::digest(owner.as_str()),
                    source_span: None,
                    unit_id: Blake3Hash::digest(id.to_string()),
                    input_hash: Blake3Hash::digest(b"shared"),
                },
                binding: UnitOwnerBinding {
                    incarnation: Blake3Hash::digest(b"vault"),
                    policy: policy.clone(),
                    owner,
                    render_token: Blake3Hash::digest(b"render"),
                    proof_version: 1,
                    modified_seq: 1,
                    tombstone: false,
                    unit_count: 2,
                },
                vector: vectors[id].to_vec(),
                neighbors: vec![vec![]],
            }
        })
        .collect();
    Fixture {
        reference: GraphRef {
            format: FORMAT,
            kind: GraphKind::Base,
            root: Blake3Hash::digest(b"root"),
            root_bytes: 0,
            directory_height: 1,
            node_count: owners.len() as u32,
            dimensions: 2,
            entry: if owners.is_empty() { None } else { Some(0) },
            max_level: 0,
        },
        nodes,
    }
}
fn meter() -> DiscoveryMeter {
    DiscoveryMeter::new(
        crate::retrieval::vectors::VectorReadBudget::new(Instant::now() + Duration::from_secs(30))
            .unwrap(),
    )
}
#[test]
fn unit_navigation_crosses_excluded_and_superseded_owners_and_merges_exact_delta() {
    let mut base = fixture(
        &["a.md", "a.md", "excluded.md", "old.md", "b.md"],
        &[[0., 1.], [0.6, 0.8], [0.7, 0.7], [0.8, 0.6], [1., 0.]],
    );
    for id in 0..4 {
        base.nodes[id].neighbors[0].push(id as u32 + 1);
    }
    let mut delta = fixture(&["c.md", "a.md"], &[[1., 0.], [1., 0.]]);
    delta.reference.kind = GraphKind::Delta;
    let mut observed = BTreeSet::new();
    let mut budget = meter();
    let result = search(
        &mut base,
        Some(&mut delta),
        &[1., 0.],
        3,
        &mut budget,
        |node| {
            observed.insert(node.descriptor.owner.as_str().to_owned());
            Ok(match node.descriptor.owner.as_str() {
                "excluded.md" => Membership::CurrentExcluded,
                "old.md" => Membership::NavigationOnly,
                _ => Membership::CurrentEligible,
            })
        },
    )
    .unwrap();
    assert_eq!(result.hits.len(), 3);
    assert!(result.hits.iter().all(|hit| hit.score == 1.));
    assert!(observed.contains("old.md") && observed.contains("excluded.md"));
    assert_eq!(budget.usage().distance_evaluations, 7);
    assert!(result.approximate);
}
#[test]
fn deterministic_tiny_complete_graph_matches_independent_exact_owner_reference() {
    for size in 1..=12 {
        let owners: Vec<String> = (0..size).map(|i| format!("{}.md", i / 2)).collect();
        let names: Vec<&str> = owners.iter().map(String::as_str).collect();
        let vectors: Vec<[f32; 2]> = (0..size)
            .map(|i| [i as f32 + 1., (size - i) as f32])
            .collect();
        let mut graph = fixture(&names, &vectors);
        for i in 0..size {
            graph.nodes[i].neighbors[0] = (0..size).filter(|j| *j != i).map(|j| j as u32).collect();
        }
        let mut expected: BTreeMap<String, DenseHit> = BTreeMap::new();
        for node in &graph.nodes {
            let descriptor = &node.descriptor;
            let hit = DenseHit {
                unit_id: descriptor.unit_id.clone(),
                target: descriptor.target,
                owner: descriptor.owner.clone(),
                target_id: None,
                source_span: None,
                input_hash: descriptor.input_hash.clone(),
                score: cosine(&[1., 0.], &node.vector).unwrap(),
            };
            expected
                .entry(hit.owner.as_str().to_owned())
                .and_modify(|previous| {
                    if hit < *previous {
                        *previous = hit.clone();
                    }
                })
                .or_insert(hit);
        }
        let mut expected: Vec<_> = expected.into_values().collect();
        expected.sort();
        expected.truncate(3);
        let result = search(&mut graph, None, &[1., 0.], 3, &mut meter(), |_| {
            Ok(Membership::CurrentEligible)
        })
        .unwrap();
        assert_eq!(result.hits, expected);
    }
}
#[test]
fn corruption_and_budget_errors_never_return_partial_candidates() {
    let mut graph = fixture(&["a.md", "b.md"], &[[1., 0.], [1., 0.]]);
    graph.nodes[0].neighbors[0] = vec![1];
    graph.nodes[1].id = 0;
    assert!(search(&mut graph, None, &[1., 0.], 1, &mut meter(), |_| Ok(
        Membership::CurrentEligible
    ))
    .is_err());
    graph.nodes[1].id = 1;
    let mut budget = meter();
    for _ in 0..MAX_DISTANCE_EVALUATIONS {
        budget.distance().unwrap();
    }
    assert!(search(&mut graph, None, &[1., 0.], 1, &mut budget, |_| Ok(
        Membership::CurrentEligible
    ))
    .is_err());
    let mut budget = meter();
    budget.retain(MAX_RETAINED_BYTES).unwrap();
    assert!(search(&mut graph, None, &[1., 0.], 1, &mut budget, |_| Ok(
        Membership::CurrentEligible
    ))
    .is_err());
    graph.nodes[1].descriptor.owner = VaultRelativePath::new("wrong.md").unwrap();
    assert!(search(&mut graph, None, &[1., 0.], 1, &mut meter(), |_| Ok(
        Membership::CurrentExcluded
    ))
    .is_err());
}
#[test]
fn upper_layer_descent_and_binding_callback_failure_are_fail_closed() {
    let mut graph = fixture(&["a.md", "b.md", "c.md"], &[[0., 1.], [1., 0.], [0.5, 0.5]]);
    graph.reference.max_level = 1;
    graph.nodes[0].neighbors = vec![vec![2], vec![1]];
    graph.nodes[1].neighbors = vec![vec![2], vec![0]];
    graph.nodes[2].neighbors[0] = vec![0, 1];
    let result = search(&mut graph, None, &[1., 0.], 1, &mut meter(), |_| {
        Ok(Membership::CurrentEligible)
    })
    .unwrap();
    assert_eq!(result.hits[0].owner.as_str(), "b.md");
    assert!(
        search(&mut graph, None, &[1., 0.], 1, &mut meter(), |node| {
            if node.id == 2 {
                Err(corrupt("authenticated hash binding mismatch"))
            } else {
                Ok(Membership::CurrentExcluded)
            }
        })
        .is_err()
    );
}

#[test]
fn delta_capacity_and_duplicate_edges_are_rejected_before_success() {
    let mut base = fixture(&[], &[]);
    let mut delta = fixture(&["a.md"], &[[1., 0.]]);
    delta.reference.kind = GraphKind::Delta;
    delta.reference.node_count = MAX_DELTA as u32 + 1;
    assert!(search(
        &mut base,
        Some(&mut delta),
        &[1., 0.],
        1,
        &mut meter(),
        |_| Ok(Membership::CurrentEligible)
    )
    .is_err());
    let mut graph = fixture(&["a.md", "b.md"], &[[1., 0.], [1., 0.]]);
    graph.nodes[0].neighbors[0] = vec![1, 1];
    assert!(search(&mut graph, None, &[1., 0.], 1, &mut meter(), |_| Ok(
        Membership::CurrentEligible
    ))
    .is_err());
}

#[test]
fn equal_scores_follow_existing_dense_hit_tie_order_and_report_owner_cap() {
    let mut graph = fixture(&["z.md", "a.md", "b.md"], &[[1., 0.], [1., 0.], [1., 0.]]);
    graph.nodes[0].neighbors[0] = vec![1, 2];
    let result = search(&mut graph, None, &[1., 0.], 2, &mut meter(), |_| {
        Ok(Membership::CurrentEligible)
    })
    .unwrap();
    assert_eq!(
        result
            .hits
            .iter()
            .map(|hit| hit.owner.as_str())
            .collect::<Vec<_>>(),
        vec!["a.md", "b.md"]
    );
    assert!(result.owner_cap_reached);
}

// Independent vector-based transcription of HNSW SEARCH-LAYER. These tiny
// graphs exercise traversal/ties; they cannot establish the ef256 recall gate.
fn reference_layer(graph: &Fixture, query: &[f32]) -> BTreeSet<NodeId> {
    let entry = graph.reference.entry.unwrap();
    let rank = |id: NodeId| Rank {
        id,
        score: cosine(query, &graph.nodes[id as usize].vector).unwrap(),
    };
    let mut visited = BTreeSet::from([entry]);
    let mut candidates = vec![rank(entry)];
    let mut nearest = vec![rank(entry)];
    while !candidates.is_empty() {
        candidates.sort_unstable();
        let candidate = candidates.pop().unwrap();
        nearest.sort_unstable();
        if nearest.len() >= QUERY_EXPANSION && candidate < nearest[0] {
            break;
        }
        for id in &graph.nodes[candidate.id as usize].neighbors[0] {
            if !visited.insert(*id) {
                continue;
            }
            let found = rank(*id);
            if nearest.len() < QUERY_EXPANSION || found > nearest[0] {
                candidates.push(found);
                nearest.push(found);
                nearest.sort_unstable();
                if nearest.len() > QUERY_EXPANSION {
                    nearest.remove(0);
                }
            }
        }
    }
    visited
}
#[test]
fn deterministic_sparse_graphs_match_independent_search_layer_encounters() {
    for seed in 0..8 {
        let names: Vec<String> = (0..16).map(|i| format!("{i}.md")).collect();
        let owners: Vec<&str> = names.iter().map(String::as_str).collect();
        let vectors: Vec<[f32; 2]> = (0..16)
            .map(|i| {
                [
                    ((i * 7 + seed) % 13 + 1) as f32,
                    ((i * 3 + seed) % 11 + 1) as f32,
                ]
            })
            .collect();
        let mut graph = fixture(&owners, &vectors);
        for i in 0..16 {
            graph.nodes[i].neighbors[0] = vec![((i + 1) % 16) as u32, ((i + 5) % 16) as u32];
        }
        let expected = reference_layer(&graph, &[1., 0.]);
        let mut observed = BTreeSet::new();
        search(&mut graph, None, &[1., 0.], 4, &mut meter(), |node| {
            observed.insert(node.id);
            Ok(Membership::CurrentEligible)
        })
        .unwrap();
        assert_eq!(observed, expected);
    }
}

#[test]
fn search_layer_beyond_ef_matches_reference_pruning_with_ties() {
    let names: Vec<String> = (0..400).map(|i| format!("owner-{i}.md")).collect();
    let owners: Vec<&str> = names.iter().map(String::as_str).collect();
    // Equal-score groups deliberately cross the ef boundary.
    let vectors: Vec<[f32; 2]> = (0..400).map(|i| [1., (i / 4 + 1) as f32 / 100.]).collect();
    let mut graph = fixture(&owners, &vectors);
    for i in 0..400 {
        graph.nodes[i].neighbors[0] = [i + 1, i + 17]
            .into_iter()
            .filter(|j| *j < 400)
            .map(|j| j as u32)
            .collect();
    }
    let expected = reference_layer(&graph, &[1., 0.]);
    assert!(expected.len() > QUERY_EXPANSION);
    assert!(expected.len() < 400);
    let mut observed = BTreeSet::new();
    let result = search(&mut graph, None, &[1., 0.], 80, &mut meter(), |node| {
        observed.insert(node.id);
        Ok(Membership::CurrentEligible)
    })
    .unwrap();
    assert_eq!(observed, expected);
    let mut exact: Vec<_> = expected
        .into_iter()
        .map(|id| {
            let node = &graph.nodes[id as usize];
            DenseHit {
                unit_id: node.descriptor.unit_id.clone(),
                owner: node.descriptor.owner.clone(),
                target: node.descriptor.target,
                target_id: None,
                source_span: None,
                input_hash: node.descriptor.input_hash.clone(),
                score: cosine(&[1., 0.], &node.vector).unwrap(),
            }
        })
        .collect();
    exact.sort();
    exact.truncate(80);
    assert_eq!(result.hits, exact);
}

#[test]
fn graph_and_delta_share_the_existing_vector_byte_boundary() {
    let mut graph = fixture(&["a.md"], &[[1., 0.]]);
    let mut delta = fixture(&["b.md"], &[[1., 0.]]);
    delta.reference.kind = GraphKind::Delta;
    let mut budget = meter();
    // Simulate prior reads as individually admitted decoded vectors; a single
    // graph read cannot exceed the 65,536-dimension per-record bound.
    for _ in 0..255 {
        budget.shared.reserve_graph_read(0, 65_536 * 4).unwrap();
    }
    budget.shared.reserve_graph_read(0, 65_536 * 4 - 8).unwrap();
    assert_eq!(
        budget.shared.usage().vector_bytes_scanned,
        64 * 1024 * 1024 - 8
    );
    assert!(search(
        &mut graph,
        Some(&mut delta),
        &[1., 0.],
        2,
        &mut budget,
        |_| Ok(Membership::CurrentEligible)
    )
    .is_err());
    assert_eq!(budget.usage().distance_evaluations, 1);
    assert_eq!(budget.usage().graph_vector_bytes, 8);
}
