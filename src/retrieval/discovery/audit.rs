//! Bounded durable export replay against pinned current inventory and paid vectors.
use super::super::{
    unit_inventory_types::{catalog_incarnation, RenderPolicyId, UnitDescriptor},
    vectors::{VectorReadBudget, VectorStore},
};
use super::*;
use crate::catalog::{
    query_types::{QueryCatalog, QueryReadLimits},
    Catalog,
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
const PAGE: usize = 128;
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditScope {
    pub(crate) space: Blake3Hash,
    pub(crate) policy: RenderPolicyId,
    pub(crate) incarnation: Blake3Hash,
    pub(crate) required_seq: u64,
    pub(crate) since_seq: u64,
    pub(crate) expected_nodes: NodeId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditCursor {
    scope_hash: Blake3Hash,
    input_root: Blake3Hash,
    output_root: Blake3Hash,
    node_count: NodeId,
    pub(crate) next_node: NodeId,
    pub(crate) digest: Blake3Hash,
    last_key: Option<(u64, VaultRelativePath, Blake3Hash)>,
}
pub(crate) fn export_digest(previous: &Blake3Hash, node: &GraphNode) -> Result<Blake3Hash> {
    Ok(Blake3Hash::digest(crate::graph::packet::canonical_json(
        &(previous, node),
    )?))
}
fn initial_digest() -> Blake3Hash {
    Blake3Hash::digest("lwiki-semantic-export-v1")
}
fn fingerprint(scope: &AuditScope, input: &GraphRef, output: &GraphRef) -> Result<Blake3Hash> {
    Ok(Blake3Hash::digest(crate::graph::packet::canonical_json(
        &(scope, input, output),
    )?))
}
fn references(scope: &AuditScope, input: &GraphRef, output: &GraphRef) -> Result<()> {
    if scope.required_seq == 0
        || scope.since_seq > scope.required_seq
        || (scope.since_seq == scope.required_seq && scope.expected_nodes != 0)
        || input.format != FORMAT
        || output.format != FORMAT
        || input.kind != GraphKind::StagingInput
        || !matches!(output.kind, GraphKind::Base | GraphKind::Delta)
        || input.node_count != scope.expected_nodes
        || output.node_count != scope.expected_nodes
        || input.dimensions != output.dimensions
        || input.dimensions == 0
        || input.dimensions as usize > MAX_NODE_VECTOR_BYTES / 4
        || (output.kind == GraphKind::Delta && output.node_count as usize > MAX_DELTA)
    {
        return Err(corrupt("semantic audit scope or graph references invalid"));
    }
    Ok(())
}
impl AuditCursor {
    pub(crate) fn new(scope: &AuditScope, input: &GraphRef, output: &GraphRef) -> Result<Self> {
        references(scope, input, output)?;
        Ok(Self {
            scope_hash: fingerprint(scope, input, output)?,
            input_root: input.root.clone(),
            output_root: output.root.clone(),
            node_count: scope.expected_nodes,
            next_node: 0,
            digest: initial_digest(),
            last_key: None,
        })
    }
    pub(crate) fn validate(&self, scope: &AuditScope, input: &GraphRef, output: &GraphRef) -> Result<()> {
        references(scope, input, output)?;
        if self.scope_hash != fingerprint(scope, input, output)?
            || self.input_root != input.root
            || self.output_root != output.root
            || self.node_count != scope.expected_nodes
            || self.next_node > self.node_count
            || (self.next_node == 0) != (self.last_key.is_none())
            || (self.next_node == 0 && self.digest != initial_digest())
        {
            return Err(corrupt("semantic audit cursor differs from frozen scope"));
        }
        if self
            .last_key
            .as_ref()
            .is_some_and(|(seq, _, _)| *seq <= scope.since_seq || *seq > scope.required_seq)
        {
            return Err(corrupt("semantic audit cursor ordering bound invalid"));
        }
        Ok(())
    }
    pub(crate) fn finish(&self, expected: &Blake3Hash) -> Result<()> {
        if self.next_node != self.node_count || &self.digest != expected {
            return Err(corrupt(
                "durable semantic export digest or complete count differs",
            ));
        }
        Ok(())
    }
}
fn bits_equal(left: &[f32], right: &[f32]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.to_bits() == right.to_bits())
}
fn validate_node(
    input: &GraphNode,
    output: &GraphNode,
    id: NodeId,
    reference: &GraphRef,
    meter: &mut DiscoveryMeter,
) -> Result<()> {
    if input.id != id
        || output.id != id
        || input.descriptor != output.descriptor
        || input.binding != output.binding
        || !bits_equal(&input.vector, &output.vector)
        || !input.neighbors.is_empty()
        || output.neighbors.is_empty()
        || output.neighbors.len() > reference.max_level + 1
        || output.vector.len() != reference.dimensions as usize
        || output.binding.tombstone
        || output.binding.unit_count == 0
    {
        return Err(corrupt("durable semantic export differs from staged input"));
    }
    if Some(id) == reference.entry && output.neighbors.len() != reference.max_level + 1 {
        return Err(corrupt("semantic audit entry level differs"));
    }
    for (layer, neighbors) in output.neighbors.iter().enumerate() {
        if neighbors.len() > CONNECTIVITY * if layer == 0 { 2 } else { 1 } {
            return Err(corrupt("semantic audit graph degree invalid"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for neighbor in neighbors {
            meter.edge()?;
            meter.retain(64)?;
            if *neighbor >= reference.node_count || *neighbor == id || !seen.insert(*neighbor) {
                return Err(corrupt("semantic audit adjacency identity invalid"));
            }
        }
    }
    Ok(())
}
/// Returns true only when the whole durable graph has been replayed. A failed
/// page leaves the caller cursor untouched. Caller checks finish against the
/// private builder's independently topology-validated streamed export digest.
pub(crate) fn audit_page(
    catalog: &Catalog,
    scope: &AuditScope,
    input: &GraphRef,
    output: &GraphRef,
    cursor: &mut AuditCursor,
    deadline: Instant,
) -> Result<bool> {
    cursor.validate(scope, input, output)?;
    let now = Instant::now();
    let phase = deadline.min(now + Duration::from_secs(30));
    if phase <= now {
        return Err(exhausted("semantic export audit deadline exhausted"));
    }
    let shared = VectorReadBudget::new(phase)?;
    let mut meter = DiscoveryMeter::new(shared.clone());
    let reader = catalog.cached_query_snapshot(QueryReadLimits {
        max_elapsed_ms: u64::try_from(phase.saturating_duration_since(now).as_millis())
            .unwrap_or(30_000)
            .min(30_000)
            .max(1),
        ..Default::default()
    })?;
    if catalog_incarnation(reader.vault_id(), reader.snapshot())? != scope.incarnation {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "semantic audit catalog incarnation changed",
        ));
    }
    let selected = reader
        .discovery_state(&scope.space, &scope.policy)?
        .ok_or_else(|| {
            WikiError::new(
                ErrorCode::FreshnessConflict,
                "semantic audit registration absent",
            )
        })?;
    if selected.required_seq != scope.required_seq {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "semantic audit required interval changed",
        ));
    }
    let store = VectorStore::open_bounded_snapshot(catalog.fs(), &shared)?;
    let state = store.active()?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::FreshnessConflict,
            "semantic audit active space absent",
        )
    })?;
    if state.id != scope.space
        || state.actual_dimensions != Some(input.dimensions)
        || RenderPolicyId::for_settings(
            &reader.snapshot().parser_fingerprint,
            &state.spec.settings,
        )? != scope.policy
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "semantic audit embedding space or policy differs",
        ));
    }
    let inventory = reader
        .unit_inventory_state(&scope.policy)?
        .ok_or_else(|| corrupt("semantic audit inventory absent"))?;
    if !inventory.complete {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "semantic audit inventory incomplete",
        ));
    }
    let mut source = storage::GraphReader::new(catalog.fs(), input.clone())?;
    let mut exported = storage::GraphReader::new(catalog.fs(), output.clone())?;
    // Three exact vector reads per ID share64MiB. Conservative temporary
    // retention admission may yield a smaller page at extreme dimensions.
    let vector_bytes = input.dimensions as usize * 4;
    let page = PAGE.min((64 * 1024 * 1024 / (vector_bytes * 3)).max(1));
    let temporary_bound =
        MAX_NODE_METADATA * 40 + input.dimensions as usize * 128 + vector_bytes * 7;
    let end = cursor
        .next_node
        .saturating_add(page as u32)
        .min(scope.expected_nodes);
    let mut next = cursor.clone();
    let mut owners: BTreeMap<VaultRelativePath, (UnitOwnerBinding, Vec<UnitDescriptor>)> =
        BTreeMap::new();
    for id in cursor.next_node..end {
        meter.check()?;
        if meter.usage().retained_bytes.saturating_add(temporary_bound) > MAX_RETAINED_BYTES {
            if next.next_node > cursor.next_node {
                break;
            }
            return Err(exhausted(
                "semantic audit node cannot fit temporary retention allowance",
            ));
        }
        meter.retain(MAX_NODE_METADATA * 2 + vector_bytes * 3)?;
        let original = source.node(id, &mut meter)?;
        let node = exported.node(id, &mut meter)?;
        validate_node(&original, &node, id, output, &mut meter)?;
        let key = (
            node.binding.modified_seq,
            node.descriptor.owner.clone(),
            node.descriptor.unit_id.clone(),
        );
        meter.retain(
            node.descriptor.owner.as_str().len() + node.descriptor.unit_id.as_str().len() + 128,
        )?;
        if node.binding.modified_seq <= scope.since_seq
            || node.binding.modified_seq > scope.required_seq
            || node.binding.incarnation != scope.incarnation
            || node.binding.policy != scope.policy
            || next.last_key.as_ref().is_some_and(|last| key <= *last)
        {
            return Err(corrupt(
                "semantic audit input order or owner interval invalid",
            ));
        }
        if !owners.contains_key(&node.descriptor.owner) {
            let binding = reader
                .unit_owner_binding(&scope.policy, &node.descriptor.owner)?
                .ok_or_else(|| corrupt("semantic audit current owner absent"))?;
            if binding != node.binding {
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "semantic audit owner binding changed",
                ));
            }
            let count = binding.unit_count;
            if count == 0 || count.saturating_add(129) > 4096 {
                return Err(exhausted(
                    "semantic audit full owner cannot fit catalog page allowance",
                ));
            }
            if reader
                .usage()
                .rows
                .saturating_add(count)
                .saturating_add(129)
                > 4096
            {
                if next.next_node > cursor.next_node {
                    break;
                }
                return Err(exhausted(
                    "semantic audit full owner cannot fit catalog page allowance",
                ));
            }
            if meter
                .usage()
                .retained_bytes
                .saturating_add(count * MAX_NODE_METADATA)
                .saturating_add(temporary_bound)
                > MAX_RETAINED_BYTES
            {
                if next.next_node > cursor.next_node {
                    break;
                }
                return Err(exhausted(
                    "semantic audit owner cannot fit retention allowance",
                ));
            }
            meter.retain(
                count
                    .checked_mul(MAX_NODE_METADATA)
                    .ok_or_else(|| exhausted("semantic audit descriptor reservation overflow"))?
                    + node.descriptor.owner.as_str().len()
                    + 256,
            )?;
            let units =
                reader.unit_descriptors_for_owner(&scope.policy, &node.descriptor.owner, count)?;
            if units.len() != count {
                return Err(corrupt("semantic audit owner descriptor count differs"));
            }
            owners.insert(node.descriptor.owner.clone(), (binding, units));
        }
        let (binding, units) = &owners[&node.descriptor.owner];
        if binding != &node.binding || !units.iter().any(|unit| unit == &node.descriptor) {
            return Err(corrupt(
                "semantic audit staged descriptor differs from current inventory",
            ));
        }
        let paid = store
            .vector(&scope.space, &node.descriptor.input_hash)?
            .ok_or_else(|| corrupt("semantic audit paid vector unavailable"))?;
        if !bits_equal(&paid, &node.vector) {
            return Err(corrupt("semantic audit vector differs from paid cache"));
        }
        meter.retain(MAX_NODE_METADATA * 16 + input.dimensions as usize * 128)?;
        next.digest = export_digest(&next.digest, &node)?;
        next.next_node = id + 1;
        next.last_key = Some(key);
    }
    reader.verify_operations(catalog)?;
    meter.check()?;
    if Instant::now() >= deadline {
        return Err(exhausted(
            "semantic export audit overall deadline exhausted",
        ));
    }
    *cursor = next;
    Ok(cursor.next_node == scope.expected_nodes)
}
#[cfg(test)]
#[path = "audit_tests.rs"]
mod tests;
