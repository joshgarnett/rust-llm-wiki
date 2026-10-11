//! Bounded approximate discovery. Cached graph records never authorize evidence.
use super::{
    unit_inventory_types::{UnitDescriptor, UnitOwnerBinding},
    vectors::{DenseHit, VectorReadBudget},
};
use crate::domain::*;
use serde::{Deserialize, Serialize};
use std::rc::Rc;

pub(crate) mod audit;
pub(crate) mod admission;
pub(crate) mod accounting;
mod builder;
pub(crate) mod process;
pub(crate) mod query;
pub(crate) mod search;
pub(crate) mod storage;

/// Version 1 fixes every constant below, the native builder version/checksum in
/// Cargo.lock, insertion order, cosine float32 scoring and deterministic ties.
/// Any algorithm/parameter change requires a new format and cursor identity.
pub(crate) const FORMAT: u32 = 1;
pub(crate) const CONNECTIVITY: usize = 16;
pub(crate) const CONSTRUCTION_EXPANSION: usize = 200;
pub(crate) const QUERY_EXPANSION: usize = 256;
pub(crate) const MAX_DELTA: usize = 512;
pub(crate) const MAX_LEVEL: usize = 32;
pub(crate) const MAX_DISTANCE_EVALUATIONS: usize = 8192;
pub(crate) const MAX_EDGES: usize = 262_144;
pub(crate) const MAX_RETAINED_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const MAX_NODE_METADATA: usize = 16 * 1024;
pub(crate) const MAX_NODE_VECTOR_BYTES: usize = 65_536 * 4;
pub(crate) type NodeId = u32;

pub(crate) fn algorithm_identity() -> Blake3Hash {
    Blake3Hash::digest(format!(
        "lwiki-hnsw-{FORMAT};usearch-2.26.4;f32-cosine;owner-densehit-order;modified_seq-owner-unit_id;M={CONNECTIVITY};construction={CONSTRUCTION_EXPANSION};query={QUERY_EXPANSION};delta={MAX_DELTA};level={MAX_LEVEL};distances={MAX_DISTANCE_EVALUATIONS};edges={MAX_EDGES};retention={MAX_RETAINED_BYTES}"
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphKind {
    Base,
    Delta,
    StagingInput,
}

/// The authenticated root locates the bounded directory tree, never a raw FFI file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphRef {
    pub(crate) format: u32,
    pub(crate) kind: GraphKind,
    pub(crate) root: Blake3Hash,
    pub(crate) root_bytes: usize,
    pub(crate) directory_height: u8,
    pub(crate) node_count: NodeId,
    pub(crate) dimensions: u32,
    pub(crate) entry: Option<NodeId>,
    pub(crate) max_level: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscoveryCoverage {
    pub(crate) eligible_current_units: usize,
    pub(crate) compatible_paid_units: usize,
    pub(crate) indexed_current_units: usize,
}

/// Selection lives in the normalized catalog, never an independently switched file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscoveryManifest {
    pub(crate) format: u32,
    pub(crate) algorithm: Blake3Hash,
    pub(crate) space: Blake3Hash,
    pub(crate) policy: super::unit_inventory_types::RenderPolicyId,
    pub(crate) incarnation: Blake3Hash,
    pub(crate) through_seq: u64,
    pub(crate) base_through_seq: u64,
    pub(crate) base: Option<GraphRef>,
    pub(crate) delta: Option<GraphRef>,
    pub(crate) coverage: DiscoveryCoverage,
}

#[derive(Clone, Debug)]
pub(crate) struct DiscoveryState {
    pub(crate) required_seq: u64,
    pub(crate) eligible_current_units: usize,
    pub(crate) manifest: Option<DiscoveryManifest>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphNode {
    pub(crate) id: NodeId,
    pub(crate) descriptor: UnitDescriptor,
    pub(crate) binding: UnitOwnerBinding,
    /// Original normalized float32 coordinates, never a quantized approximation.
    pub(crate) vector: Vec<f32>,
    /// Layer zero first. Every target is a graph-local explicit integer identity.
    pub(crate) neighbors: Vec<Vec<NodeId>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Membership {
    CurrentEligible,
    CurrentExcluded,
    /// Authenticated superseded binding, usable only for graph navigation.
    NavigationOnly,
}

/// Implementations authenticate and charge bytes before decoding or allocating.
pub(crate) trait GraphAccess {
    fn reference(&self) -> &GraphRef;
    fn node(&mut self, id: NodeId, meter: &mut DiscoveryMeter) -> Result<GraphNode>;
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct DiscoveryUsage {
    pub(crate) distance_evaluations: usize,
    pub(crate) edge_inspections: usize,
    pub(crate) graph_reads: usize,
    pub(crate) graph_metadata_bytes: usize,
    pub(crate) graph_vector_bytes: usize,
    pub(crate) retained_bytes: usize,
}

/// Graph and retained-vector scoring share the same existing cumulative budget.
pub(crate) struct DiscoveryMeter {
    pub(crate) shared: Rc<VectorReadBudget>,
    usage: DiscoveryUsage,
}
impl DiscoveryMeter {
    pub(crate) fn new(shared: Rc<VectorReadBudget>) -> Self {
        Self {
            shared,
            usage: DiscoveryUsage::default(),
        }
    }
    pub(crate) fn check(&self) -> Result<()> {
        self.shared.remaining_ms().map(|_| ())
    }
    pub(crate) fn usage(&self) -> DiscoveryUsage {
        self.usage.clone()
    }
    pub(crate) fn read(&mut self, metadata: usize, vectors: usize) -> Result<()> {
        self.check()?;
        if metadata > MAX_NODE_METADATA || vectors > MAX_NODE_VECTOR_BYTES {
            return Err(corrupt("graph record exceeds its format bound"));
        }
        self.shared.reserve_graph_read(metadata, vectors)?;
        self.usage.graph_reads += 1;
        self.usage.graph_metadata_bytes += metadata;
        self.usage.graph_vector_bytes += vectors;
        Ok(())
    }
    pub(crate) fn distance(&mut self) -> Result<()> {
        self.check()?;
        if self.usage.distance_evaluations >= MAX_DISTANCE_EVALUATIONS {
            return Err(exhausted("semantic graph distance budget exhausted"));
        }
        self.shared.reserve_distance()?;
        self.usage.distance_evaluations += 1;
        Ok(())
    }
    pub(crate) fn edge(&mut self) -> Result<()> {
        self.check()?;
        if self.usage.edge_inspections >= MAX_EDGES {
            return Err(exhausted("semantic graph edge budget exhausted"));
        }
        self.usage.edge_inspections += 1;
        Ok(())
    }
    /// Cumulative retained-allocation admission, not a promise about process RSS.
    pub(crate) fn retain(&mut self, bytes: usize) -> Result<()> {
        self.check()?;
        let next = self
            .usage
            .retained_bytes
            .checked_add(bytes)
            .filter(|n| *n <= MAX_RETAINED_BYTES)
            .ok_or_else(|| exhausted("semantic graph retention budget exhausted"))?;
        self.usage.retained_bytes = next;
        Ok(())
    }
}

pub(crate) struct DiscoveryCandidates {
    pub(crate) hits: Vec<DenseHit>,
    pub(crate) owner_cap_reached: bool,
    /// Always true for a graph traversal; no exact-absence or exhaustive-recall claim.
    pub(crate) approximate: bool,
}
pub(crate) fn corrupt(message: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
pub(crate) fn exhausted(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
