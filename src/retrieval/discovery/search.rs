//! Metered HNSW greedy descent and SEARCH-LAYER (ef = 256).
use super::super::vectors::cosine;
use super::*;
use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    mem::size_of,
};

#[derive(Clone, Copy, Debug)]
struct Rank {
    score: f64,
    id: NodeId,
}
impl PartialEq for Rank {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Rank {}
impl PartialOrd for Rank {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Rank {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.id.cmp(&self.id))
    }
}
struct Traversal<'a, F> {
    graph: &'a mut dyn GraphAccess,
    query: &'a [f32],
    meter: &'a mut DiscoveryMeter,
    classify: &'a mut F,
    cache: BTreeMap<NodeId, (GraphNode, Rank)>,
    results: &'a mut Vec<DenseHit>,
    cap: usize,
    capped: &'a mut bool,
}
impl<F: FnMut(&GraphNode) -> Result<Membership>> Traversal<'_, F> {
    fn encounter(&mut self, id: NodeId) -> Result<Rank> {
        self.meter.check()?;
        if let Some((_, rank)) = self.cache.get(&id) {
            return Ok(*rank);
        }
        let reference = self.graph.reference();
        if id >= reference.node_count {
            return Err(corrupt("graph neighbor identity out of range"));
        }
        let dimensions = reference.dimensions as usize;
        let max_level = reference.max_level;
        // Reserve conservative metadata/tree overhead before the read and cache insertion.
        self.meter
            .retain(MAX_NODE_METADATA + dimensions * size_of::<f32>() + 256)?;
        let node = self.graph.node(id, self.meter)?;
        if node.id != id
            || node.vector.len() != dimensions
            || node.neighbors.is_empty()
            || node.neighbors.len() > max_level + 1
            || node.descriptor.owner != node.binding.owner
            || node.descriptor.policy != node.binding.policy
            || node.binding.tombstone
            || node.binding.unit_count == 0
        {
            return Err(corrupt(
                "graph node identity, dimensions or binding invalid",
            ));
        }
        // Validate every returned edge, including edges on layers not searched.
        for (layer, neighbors) in node.neighbors.iter().enumerate() {
            if neighbors.len() > CONNECTIVITY * if layer == 0 { 2 } else { 1 } {
                return Err(corrupt("graph node degree exceeds format bound"));
            }
            let mut unique = BTreeSet::new();
            for neighbor in neighbors {
                self.meter.edge()?;
                if *neighbor >= self.graph.reference().node_count || *neighbor == id {
                    return Err(corrupt("graph adjacency identity invalid"));
                }
                self.meter.retain(64)?;
                if !unique.insert(*neighbor) {
                    return Err(corrupt("duplicate graph edge"));
                }
            }
        }
        let membership = (self.classify)(&node)?;
        self.meter.distance()?;
        let rank = Rank {
            score: cosine(self.query, &node.vector)?,
            id,
        };
        if membership == Membership::CurrentEligible {
            let descriptor = &node.descriptor;
            // Clones have string length capacity; include conservative collection overhead.
            self.meter.retain(
                size_of::<DenseHit>()
                    + 64
                    + descriptor.owner.as_str().len()
                    + descriptor.unit_id.as_str().len()
                    + descriptor.input_hash.as_str().len()
                    + descriptor
                        .target_id
                        .as_ref()
                        .map_or(0, |id| id.as_str().len()),
            )?;
            let hit = DenseHit {
                unit_id: descriptor.unit_id.clone(),
                target: descriptor.target,
                owner: descriptor.owner.clone(),
                target_id: descriptor.target_id.clone(),
                source_span: descriptor.source_span,
                input_hash: descriptor.input_hash.clone(),
                score: rank.score,
            };
            if let Some(index) = self
                .results
                .iter()
                .position(|existing| existing.owner == hit.owner)
            {
                if hit < self.results[index] {
                    self.results[index] = hit;
                }
            } else {
                if self.results.len() == self.cap {
                    *self.capped = true;
                }
                if self.results.len() < self.cap
                    || self.results.last().is_some_and(|worst| hit < *worst)
                {
                    if self.results.len() == self.cap {
                        self.results.pop();
                    }
                    self.results.push(hit);
                }
            }
            self.results.sort_unstable();
        }
        self.cache.insert(id, (node, rank));
        Ok(rank)
    }
    fn neighbor(&mut self, id: NodeId, layer: usize, offset: usize) -> Result<Option<NodeId>> {
        self.meter.check()?;
        let node = &self.cache[&id].0;
        let neighbors = node
            .neighbors
            .get(layer)
            .ok_or_else(|| corrupt("graph edge targets node missing layer"))?;
        if let Some(neighbor) = neighbors.get(offset) {
            self.meter.edge()?;
            Ok(Some(*neighbor))
        } else {
            Ok(None)
        }
    }
    fn base(&mut self, entry: NodeId, max_level: usize) -> Result<()> {
        let mut current = self.encounter(entry)?;
        if self.cache[&entry].0.neighbors.len() != max_level + 1 {
            return Err(corrupt("graph entry level disagrees with header"));
        }
        for layer in (1..=max_level).rev() {
            loop {
                let mut next = current;
                let mut offset = 0;
                while let Some(id) = self.neighbor(current.id, layer, offset)? {
                    let candidate = self.encounter(id)?;
                    if self.cache[&id].0.neighbors.len() <= layer {
                        return Err(corrupt("upper graph edge missing target layer"));
                    }
                    if candidate > next {
                        next = candidate;
                    }
                    offset += 1;
                }
                if next <= current {
                    break;
                }
                current = next;
            }
        }
        let mut visited = BTreeSet::new();
        let mut candidates = BinaryHeap::new();
        let mut nearest = BinaryHeap::new();
        self.meter.retain(128)?;
        visited.insert(current.id);
        candidates.push(current);
        nearest.push(Reverse(current));
        while let Some(candidate) = candidates.pop() {
            self.meter.check()?;
            if nearest.len() >= QUERY_EXPANSION && candidate < nearest.peek().unwrap().0 {
                break;
            }
            let mut offset = 0;
            while let Some(id) = self.neighbor(candidate.id, 0, offset)? {
                offset += 1;
                if visited.contains(&id) {
                    continue;
                }
                self.meter.retain(64)?;
                visited.insert(id);
                let found = self.encounter(id)?;
                if nearest.len() < QUERY_EXPANSION || found > nearest.peek().unwrap().0 {
                    self.meter.retain(2 * size_of::<Rank>() + 64)?;
                    candidates.push(found);
                    nearest.push(Reverse(found));
                    if nearest.len() > QUERY_EXPANSION {
                        nearest.pop();
                    }
                }
            }
        }
        Ok(())
    }
}
fn validate_reference(reference: &GraphRef, dimensions: usize, kind: GraphKind) -> Result<()> {
    if reference.format != FORMAT
        || reference.kind != kind
        || dimensions == 0
        || dimensions > MAX_NODE_VECTOR_BYTES / 4
        || reference.dimensions as usize != dimensions
        || reference.max_level > MAX_LEVEL
        || (reference.node_count == 0) != reference.entry.is_none()
        || reference.entry.is_some_and(|id| id >= reference.node_count)
        || (reference.node_count == 0 && reference.max_level != 0)
    {
        return Err(corrupt("graph reference invalid"));
    }
    Ok(())
}
pub(crate) fn search(
    base: &mut dyn GraphAccess,
    delta: Option<&mut dyn GraphAccess>,
    query: &[f32],
    k: usize,
    meter: &mut DiscoveryMeter,
    mut classify: impl FnMut(&GraphNode) -> Result<Membership>,
) -> Result<DiscoveryCandidates> {
    if k == 0 || k > 160 {
        return Err(corrupt("semantic owner cap invalid"));
    }
    validate_reference(base.reference(), query.len(), GraphKind::Base)?;
    if query.iter().any(|value| !value.is_finite()) || !query.iter().any(|value| *value != 0.0) {
        return Err(corrupt("semantic query vector invalid"));
    }
    if let Some(graph) = delta.as_ref() {
        validate_reference(graph.reference(), query.len(), GraphKind::Delta)?;
        if graph.reference().node_count as usize > MAX_DELTA {
            return Err(corrupt("semantic delta exceeds capacity"));
        }
    }
    meter.check()?;
    meter.retain(k * size_of::<DenseHit>())?;
    let mut results = Vec::with_capacity(k);
    let mut capped = false;
    let entry = base.reference().entry;
    let max_level = base.reference().max_level;
    if let Some(entry) = entry {
        Traversal {
            graph: base,
            query,
            meter,
            classify: &mut classify,
            cache: BTreeMap::new(),
            results: &mut results,
            cap: k,
            capped: &mut capped,
        }
        .base(entry, max_level)?;
    }
    if let Some(graph) = delta {
        let count = graph.reference().node_count;
        let mut traversal = Traversal {
            graph,
            query,
            meter,
            classify: &mut classify,
            cache: BTreeMap::new(),
            results: &mut results,
            cap: k,
            capped: &mut capped,
        };
        for id in 0..count {
            traversal.encounter(id)?;
        }
    }
    meter.check()?;
    Ok(DiscoveryCandidates {
        hits: results,
        owner_cap_reached: capped,
        approximate: true,
    })
}
#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
