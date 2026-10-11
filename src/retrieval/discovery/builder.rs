//! Private serial native construction, used only by the isolated maintenance child.
//! Replay must authenticate retained vectors and supply unique descriptor identities
//! in one frozen order. Emit must stage privately: no output is publishable until
//! this function returns success and the parent validates the complete generation.
use super::admission::DiscoveryAdmission;
use super::{
    corrupt, exhausted, GraphNode, CONNECTIVITY, CONSTRUCTION_EXPANSION, MAX_LEVEL,
    MAX_NODE_METADATA, QUERY_EXPANSION,
};
use crate::domain::*;
use std::time::Instant;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

const BASE_DEGREE: usize = CONNECTIVITY * 2;

pub(super) struct BuildTopology {
    pub(super) entry: Option<u32>,
    pub(super) max_level: usize,
    pub(super) node_count: u32,
    /// Native estimate is a lower bound, never a process RSS guarantee.
    pub(super) native_memory_lower_bound: usize,
}

fn check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(exhausted("semantic builder deadline exceeded"))
    } else {
        Ok(())
    }
}
fn native(error: impl std::fmt::Display) -> WikiError {
    corrupt(&format!("semantic native builder failed: {error}"))
}
fn admit(n: u32, d: u32, admission: &DiscoveryAdmission) -> Result<usize> {
    admission.validate()?;
    if admission.node_count != n || admission.dimensions != d {
        return Err(corrupt(
            "semantic builder geometry differs from frozen admission",
        ));
    }
    usize::try_from(admission.topology_bytes)
        .map_err(|_| exhausted("semantic builder topology exceeds platform addressability"))
}
fn validate(
    node: &GraphNode,
    expected: u32,
    dimensions: u32,
    hash: &mut blake3::Hasher,
) -> Result<()> {
    if node.id != expected
        || node.vector.len() != dimensions as usize
        || !node.neighbors.is_empty()
        || node.descriptor.owner != node.binding.owner
        || node.descriptor.policy != node.binding.policy
        || node.binding.tombstone
        || node.binding.unit_count == 0
    {
        return Err(corrupt("semantic builder input identity or shape mismatch"));
    }
    let norm: f64 = node.vector.iter().map(|v| f64::from(*v).powi(2)).sum();
    if node.vector.iter().any(|v| !v.is_finite()) || !norm.is_finite() || (norm - 1.).abs() > 1e-5 {
        return Err(corrupt(
            "semantic builder requires normalized finite float32 vectors",
        ));
    }
    let metadata =
        crate::graph::packet::canonical_json(&(node.id, &node.descriptor, &node.binding))?;
    if metadata.len() > MAX_NODE_METADATA {
        return Err(corrupt("semantic builder metadata exceeds bound"));
    }
    hash.update(&(metadata.len() as u64).to_le_bytes());
    hash.update(&metadata);
    for v in &node.vector {
        hash.update(&v.to_bits().to_le_bytes());
    }
    Ok(())
}

/// Native calls cannot be interrupted here. Parent must enforce hard RAM/deadline
/// isolation. Both passes must yield exactly N identically ordered immutable nodes.
/// Parent must validate global descriptor uniqueness and generation/space bindings.
pub(super) fn build<I, F, E>(
    node_count: u32,
    dimensions: u32,
    admission: &DiscoveryAdmission,
    deadline: Instant,
    mut replay: F,
    mut emit: E,
) -> Result<BuildTopology>
where
    I: Iterator<Item = Result<GraphNode>>,
    F: FnMut() -> Result<I>,
    E: FnMut(GraphNode) -> Result<()>,
{
    check(deadline)?;
    admit(node_count, dimensions, admission)?;
    if deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        > admission.limits.max_invocation_ms as u128
    {
        return Err(exhausted(
            "semantic builder deadline exceeds frozen admission",
        ));
    }
    let options = IndexOptions {
        dimensions: dimensions as usize,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        connectivity: CONNECTIVITY,
        expansion_add: CONSTRUCTION_EXPANSION,
        expansion_search: QUERY_EXPANSION,
        multi: false,
    };
    let index = Index::new(&options).map_err(native)?;
    index
        .reserve_capacity_and_threads(node_count as usize, 1)
        .map_err(native)?;
    check(deadline)?;
    let mut first = replay()?;
    let mut digest = blake3::Hasher::new();
    let mut generation_binding = None;
    let mut entry = None;
    let mut max_level = 0;
    for id in 0..node_count {
        check(deadline)?;
        let node = first
            .next()
            .ok_or_else(|| corrupt("semantic builder input ended early"))??;
        validate(&node, id, dimensions, &mut digest)?;
        let identity = (
            node.binding.incarnation.clone(),
            node.binding.policy.clone(),
        );
        if generation_binding
            .as_ref()
            .is_some_and(|expected| expected != &identity)
        {
            return Err(corrupt(
                "semantic builder input crosses catalog incarnation or render policy",
            ));
        }
        generation_binding = Some(identity);
        index.add(id as u64, &node.vector).map_err(native)?;
        check(deadline)?;
        let level = index.level_of_key(id as u64);
        if level > MAX_LEVEL {
            return Err(exhausted("semantic native graph level exceeds bound"));
        }
        // Pinned native add changes entry only on a strictly greater level.
        if entry.is_none() || level > max_level {
            entry = Some(id);
            max_level = level;
        }
    }
    if first.next().is_some() {
        return Err(corrupt("semantic builder input exceeds declared count"));
    }
    let mut base = vec![u32::MAX; node_count as usize * BASE_DEGREE];
    let mut degrees = vec![0u8; node_count as usize];
    let mut vector = vec![0f32; dimensions as usize];
    let mut second = replay()?;
    let mut replay_digest = blake3::Hasher::new();
    for id in 0..node_count {
        check(deadline)?;
        let mut node = second
            .next()
            .ok_or_else(|| corrupt("semantic builder replay ended early"))??;
        validate(&node, id, dimensions, &mut replay_digest)?;
        if index.get(id as u64, &mut vector).map_err(native)? != 1
            || vector
                .iter()
                .zip(&node.vector)
                .any(|(a, b)| a.to_bits() != b.to_bits())
        {
            return Err(corrupt(
                "semantic native export changed retained vector bits",
            ));
        }
        let level = index.level_of_key(id as u64);
        if level > MAX_LEVEL {
            return Err(corrupt("semantic native export level exceeds bound"));
        }
        node.neighbors = Vec::with_capacity(level + 1);
        for layer in 0..=level {
            check(deadline)?;
            let cap = if layer == 0 {
                BASE_DEGREE
            } else {
                CONNECTIVITY
            };
            let mut neighbors = Vec::with_capacity(cap);
            for key in index.neighbors(id as u64, layer) {
                check(deadline)?;
                if neighbors.len() >= cap
                    || key >= node_count as u64
                    || key == id as u64
                    || neighbors.contains(&(key as u32))
                    || index.level_of_key(key) < layer
                {
                    return Err(corrupt("semantic native export adjacency invalid"));
                }
                neighbors.push(key as u32);
            }
            if layer == 0 {
                degrees[id as usize] = neighbors.len() as u8;
                base[id as usize * BASE_DEGREE..id as usize * BASE_DEGREE + neighbors.len()]
                    .copy_from_slice(&neighbors);
            }
            node.neighbors.push(neighbors);
        }
        emit(node)?;
        check(deadline)?;
    }
    if second.next().is_some() || digest.finalize() != replay_digest.finalize() {
        return Err(corrupt(
            "semantic builder replay differs from immutable input",
        ));
    }
    reachable(&base, &degrees, entry, deadline)?;
    check(deadline)?;
    let native_memory_lower_bound = index.memory_usage();
    if native_memory_lower_bound as u64 > admission.total_memory_estimate_bytes {
        return Err(exhausted(
            "semantic native memory lower bound exceeds admitted total memory estimate",
        ));
    }
    Ok(BuildTopology {
        entry,
        max_level,
        node_count,
        native_memory_lower_bound,
    })
}

fn reachable(base: &[u32], degrees: &[u8], entry: Option<u32>, deadline: Instant) -> Result<()> {
    let mut visited = vec![false; degrees.len()];
    // Queue is admitted alongside flat base topology; each node is queued once.
    let mut queue = Vec::with_capacity(degrees.len());
    if let Some(id) = entry {
        visited[id as usize] = true;
        queue.push(id);
    }
    let mut cursor = 0;
    while cursor < queue.len() {
        check(deadline)?;
        let id = queue[cursor] as usize;
        cursor += 1;
        for &next in &base[id * BASE_DEGREE..id * BASE_DEGREE + degrees[id] as usize] {
            check(deadline)?;
            if !visited[next as usize] {
                visited[next as usize] = true;
                queue.push(next);
            }
        }
    }
    if queue.len() != degrees.len() {
        return Err(corrupt(
            "semantic exported base graph is not reachable from actual entry",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
