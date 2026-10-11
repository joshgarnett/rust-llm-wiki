//! Pinned owner membership and selective exact search for graph discovery.
use super::super::{
    filters,
    render::TargetKind,
    types::{QueryPlan, SearchFilters, SearchMode},
    unit_inventory_types::{RenderPolicyId, UnitDescriptor, catalog_incarnation},
    vectors::{SpaceState, VectorStore, cosine},
};
use super::*;
use crate::catalog::{Catalog, query::QuerySnapshot, query_types::QueryCatalog, sql};
use rusqlite::{
    params_from_iter,
    types::{Value, ValueRef},
};
use std::collections::BTreeMap;
const DISCOVERY_ROWS: usize = 1536;
const PROJECTION_BYTES: usize = 8 * 1024 * 1024;
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::FreshnessConflict, message)
}
fn text<'a>(row: &'a rusqlite::Row<'_>, index: usize) -> Result<&'a str> {
    row.get_ref(index)
        .map_err(sql::sql_error)?
        .as_str()
        .map_err(|_| corrupt("semantic owner scalar is not text"))
}
fn optional_text<'a>(row: &'a rusqlite::Row<'_>, index: usize) -> Result<Option<&'a str>> {
    match row.get_ref(index).map_err(sql::sql_error)? {
        ValueRef::Null => Ok(None),
        ValueRef::Text(bytes) => std::str::from_utf8(bytes)
            .map(Some)
            .map_err(|_| corrupt("semantic owner text is invalid UTF-8")),
        _ => Err(corrupt("semantic owner scalar is not text")),
    }
}
fn predicate(filter: &SearchFilters, context: bool, values: &mut Vec<Value>) -> String {
    let common = filters::catalog_sql(filter, values, true);
    let policy = if context {
        filters::catalog_context_policy(false, true)
    } else {
        filters::catalog_normal_policy(filter, true)
    };
    format!("({common}) AND ({policy})")
}
fn explicit(filter: &SearchFilters) -> bool {
    !filter.kinds.is_empty()
        || !filter.tags.is_empty()
        || !filter.source_ids.is_empty()
        || filter.path_prefix.is_some()
        || !filter.authored_statuses.is_empty()
}
struct Projection {
    binding: UnitOwnerBinding,
    source_hash: Blake3Hash,
    document_hash: Option<Blake3Hash>,
    target: Option<RecordId>,
    eligible: bool,
}
struct Resolver<'a> {
    reader: &'a QuerySnapshot,
    filter: &'a SearchFilters,
    context: bool,
    policy: &'a RenderPolicyId,
    incarnation: Blake3Hash,
    owners: BTreeMap<VaultRelativePath, Option<Projection>>,
    rows: usize,
    retained: usize,
}
impl<'a> Resolver<'a> {
    fn charge(&mut self, row: &rusqlite::Row<'_>, columns: usize) -> Result<()> {
        if self.rows >= DISCOVERY_ROWS {
            return Err(exhausted("semantic discovery SQL row budget exhausted"));
        }
        self.reader.reserve_scalar_row(row, columns)?;
        self.rows += 1;
        Ok(())
    }
    fn reserve(&mut self, bytes: usize) -> Result<()> {
        self.retained = self
            .retained
            .checked_add(bytes)
            .filter(|n| *n <= PROJECTION_BYTES)
            .ok_or_else(|| exhausted("semantic owner projection cache exhausted"))?;
        Ok(())
    }
    fn resolve(&mut self, owner: &VaultRelativePath) -> Result<()> {
        if self.owners.contains_key(owner) {
            return Ok(());
        }
        let mut values = vec![
            Value::Text(self.policy.as_str().to_owned()),
            Value::Text(owner.as_str().to_owned()),
        ];
        let predicate = predicate(self.filter, self.context, &mut values);
        let reader = self.reader;
        let mut statement=reader.connection().prepare(&format!("SELECT u.source_hash,u.render_token,u.proof_version,u.modified_seq,u.tombstone,u.unit_count,d.file_hash,coalesce(d.record_id,d.owner_revision),COALESCE(({predicate}),0) FROM unit_owners u LEFT JOIN documents d ON d.path=u.owner LEFT JOIN records r ON r.id=d.record_id WHERE u.policy=?1 AND u.owner=?2")).map_err(sql::sql_error)?;
        let mut rows = statement
            .query(params_from_iter(values))
            .map_err(sql::sql_error)?;
        let projection = if let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.charge(row, 9)?;
            let source = text(row, 0)?;
            let token = text(row, 1)?;
            let hash = optional_text(row, 6)?;
            let target = optional_text(row, 7)?;
            self.reserve(
                std::mem::size_of::<Projection>()
                    + 256
                    + owner.as_str().len() * 2
                    + source.len()
                    + token.len()
                    + hash.map_or(0, str::len)
                    + target.map_or(0, str::len),
            )?;
            let proof: i64 = row.get(2).map_err(sql::sql_error)?;
            let seq: i64 = row.get(3).map_err(sql::sql_error)?;
            let tombstone: i64 = row.get(4).map_err(sql::sql_error)?;
            let count: i64 = row.get(5).map_err(sql::sql_error)?;
            if proof <= 0
                || seq <= 0
                || proof as u64 > self.reader.snapshot().generation
                || seq as u64 > self.reader.snapshot().generation
                || !matches!(tombstone, 0 | 1)
                || count < 0
                || (tombstone == 1 && count != 0)
            {
                return Err(corrupt("semantic current owner binding invalid"));
            }
            Some(Projection {
                binding: UnitOwnerBinding {
                    incarnation: self.incarnation.clone(),
                    policy: self.policy.clone(),
                    owner: owner.clone(),
                    render_token: Blake3Hash::new(token)?,
                    proof_version: proof as u64,
                    modified_seq: seq as u64,
                    tombstone: tombstone == 1,
                    unit_count: usize::try_from(count)
                        .map_err(|_| corrupt("semantic owner count invalid"))?,
                },
                source_hash: Blake3Hash::new(source)?,
                document_hash: hash.map(Blake3Hash::new).transpose()?,
                target: target.map(RecordId::new).transpose()?,
                eligible: row.get(8).map_err(sql::sql_error)?,
            })
        } else {
            self.reserve(owner.as_str().len() + 128)?;
            None
        };
        self.owners.insert(owner.clone(), projection);
        Ok(())
    }
    fn classify(&mut self, node: &GraphNode) -> Result<Membership> {
        self.resolve(&node.descriptor.owner)?;
        membership(
            &node.descriptor,
            &node.binding,
            self.owners[&node.descriptor.owner].as_ref(),
            self.policy,
            &self.incarnation,
            self.reader.snapshot().generation,
        )
    }
}
fn membership(
    unit: &UnitDescriptor,
    old: &UnitOwnerBinding,
    current: Option<&Projection>,
    policy: &RenderPolicyId,
    incarnation: &Blake3Hash,
    generation: u64,
) -> Result<Membership> {
    if &old.incarnation != incarnation
        || &old.policy != policy
        || &unit.policy != policy
        || old.owner != unit.owner
        || unit.target != TargetKind::Document
        || old.tombstone
        || old.unit_count == 0
        || old.proof_version == 0
        || old.modified_seq == 0
        || old.proof_version > generation
        || old.modified_seq > generation
    {
        return Err(corrupt(
            "semantic graph binding identity or version invalid",
        ));
    }
    let Some(current) = current else {
        return Ok(Membership::NavigationOnly);
    };
    let new = &current.binding;
    if old.proof_version > new.proof_version || old.modified_seq > new.modified_seq {
        return Err(corrupt(
            "semantic graph binding exceeds current owner version",
        ));
    }
    let advanced = old.proof_version < new.proof_version || old.modified_seq < new.modified_seq;
    if advanced {
        return Ok(Membership::NavigationOnly);
    }
    if old != new {
        return Err(corrupt("equal-version semantic binding changed"));
    }
    if current.document_hash.as_ref() != Some(&unit.source_hash)
        || current.source_hash != unit.source_hash
        || current.target != unit.target_id
    {
        return Err(corrupt(
            "semantic descriptor differs from current owner hash or target",
        ));
    }
    Ok(if current.eligible {
        Membership::CurrentEligible
    } else {
        Membership::CurrentExcluded
    })
}
/// At most513 charged descriptors; no corpus text and no unrestricted replay.
fn probe(
    resolver: &mut Resolver<'_>,
    meter: &mut DiscoveryMeter,
) -> Result<Option<Vec<UnitDescriptor>>> {
    let mut values = vec![Value::Text(resolver.policy.as_str().to_owned())];
    let predicate = predicate(resolver.filter, resolver.context, &mut values);
    let reader = resolver.reader;
    let mut statement=reader.connection().prepare(&format!("SELECT u.unit_id,u.owner,u.input_hash,u.descriptor_json FROM retrieval_units u INDEXED BY retrieval_unit_owners JOIN unit_owners o ON o.policy=u.policy AND o.owner=u.owner JOIN documents d ON d.path=u.owner LEFT JOIN records r ON r.id=d.record_id WHERE u.policy=?1 AND o.tombstone=0 AND d.eligibility='current' AND (d.kind IS NULL OR d.kind='page') AND d.file_hash=o.source_hash AND ({predicate}) ORDER BY u.owner,u.unit_id LIMIT 513")).map_err(sql::sql_error)?;
    let mut rows = statement
        .query(params_from_iter(values))
        .map_err(sql::sql_error)?;
    meter.retain(MAX_DELTA * std::mem::size_of::<UnitDescriptor>())?;
    let mut units = Vec::with_capacity(MAX_DELTA);
    while let Some(row) = rows.next().map_err(sql::sql_error)? {
        resolver.charge(row, 4)?;
        if units.len() == MAX_DELTA {
            return Ok(None);
        }
        let json = text(row, 3)?;
        if json.len() > MAX_NODE_METADATA {
            return Err(corrupt("semantic subset descriptor exceeds format bound"));
        }
        meter.retain(json.len() + 256)?;
        let unit: UnitDescriptor = serde_json::from_str(json)
            .map_err(|_| corrupt("semantic subset descriptor JSON invalid"))?;
        if &unit.policy != resolver.policy
            || unit.unit_id.as_str() != text(row, 0)?
            || unit.owner.as_str() != text(row, 1)?
            || unit.input_hash.as_str() != text(row, 2)?
        {
            return Err(corrupt("semantic subset descriptor identity invalid"));
        }
        units.push(unit);
    }
    Ok(Some(units))
}
fn exact_subset(
    units: Vec<UnitDescriptor>,
    resolver: &mut Resolver<'_>,
    store: &VectorStore,
    state: &SpaceState,
    query: &[f32],
    k: usize,
    meter: &mut DiscoveryMeter,
) -> Result<DiscoveryCandidates> {
    meter.retain(k * std::mem::size_of::<DenseHit>())?;
    let mut hits: Vec<DenseHit> = Vec::with_capacity(k);
    let mut capped = false;
    for unit in units {
        meter.check()?;
        resolver.resolve(&unit.owner)?;
        let projection = resolver.owners[&unit.owner]
            .as_ref()
            .ok_or_else(|| corrupt("semantic subset owner missing"))?;
        if membership(
            &unit,
            &projection.binding,
            Some(projection),
            resolver.policy,
            &resolver.incarnation,
            resolver.reader.snapshot().generation,
        )? != Membership::CurrentEligible
        {
            return Err(corrupt("semantic subset owner lost eligibility"));
        }
        meter.retain(query.len() * 4)?;
        let vector = store
            .vector(&state.id, &unit.input_hash)?
            .ok_or_else(|| corrupt("semantic subset paid vector unavailable"))?;
        meter.distance()?;
        let score = cosine(query, &vector)?;
        let hit = DenseHit {
            unit_id: unit.unit_id,
            target: unit.target,
            owner: unit.owner,
            target_id: unit.target_id,
            source_span: unit.source_span,
            input_hash: unit.input_hash,
            score,
        };
        if let Some(index) = hits.iter().position(|previous| previous.owner == hit.owner) {
            if hit < hits[index] {
                hits[index] = hit;
            }
        } else {
            if hits.len() == k {
                capped = true;
            }
            if hits.len() < k || hits.last().is_some_and(|previous| hit < *previous) {
                if hits.len() == k {
                    hits.pop();
                }
                hits.push(hit);
            }
        }
        hits.sort_unstable();
    }
    Ok(DiscoveryCandidates {
        hits,
        owner_cap_reached: capped,
        approximate: false,
    })
}
struct EmptyGraph(GraphRef);
impl GraphAccess for EmptyGraph {
    fn reference(&self) -> &GraphRef {
        &self.0
    }
    fn node(&mut self, _: NodeId, _: &mut DiscoveryMeter) -> Result<GraphNode> {
        Err(corrupt("empty graph node requested"))
    }
}
pub(crate) fn discover(
    catalog: &Catalog,
    reader: &QuerySnapshot,
    state: &SpaceState,
    query: &[f32],
    plan: &QueryPlan,
    for_context: bool,
    store: &VectorStore,
    shared: Rc<VectorReadBudget>,
) -> Result<(DiscoveryCandidates, DiscoveryManifest, DiscoveryUsage)> {
    if !reader.normalized_layout()
        || !matches!(plan.mode, SearchMode::Semantic | SearchMode::Hybrid)
        || plan.filters.include_historical
        || plan.filters.include_proposed
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "graph semantic discovery requires current normalized documents",
        ));
    }
    if !(1..=160).contains(&plan.limits.candidates) {
        return Err(WikiError::invalid(
            "semantic candidate count requires1..=160",
        ));
    }
    let dimensions = state
        .actual_dimensions
        .ok_or_else(|| conflict("semantic embedding dimensions unavailable"))?;
    if dimensions == 0
        || dimensions as usize > MAX_NODE_VECTOR_BYTES / 4
        || query.len() != dimensions as usize
        || query.is_empty()
        || query.iter().any(|n| !n.is_finite())
        || !query.iter().any(|n| *n != 0.)
    {
        return Err(corrupt("semantic query dimensions or coordinates invalid"));
    }
    if !state.active
        || store.active()?.is_none_or(|active| {
            active.id != state.id
                || active.spec != state.spec
                || active.actual_dimensions != state.actual_dimensions
        })
    {
        return Err(conflict(
            "semantic active embedding space differs from pinned cache",
        ));
    }
    let policy =
        RenderPolicyId::for_settings(&reader.snapshot().parser_fingerprint, &state.spec.settings)?;
    let incarnation = catalog_incarnation(reader.vault_id(), reader.snapshot())?;
    let before = reader.usage().rows;
    let selected = reader
        .discovery_state(&state.id, &policy)?
        .ok_or_else(|| conflict("semantic discovery preparation is pending"))?;
    let manifest = selected
        .manifest
        .ok_or_else(|| conflict("semantic discovery preparation is pending"))?;
    if manifest.through_seq != selected.required_seq
        || manifest.incarnation != incarnation
        || manifest.space != state.id
        || manifest.policy != policy
        || manifest.coverage.indexed_current_units != selected.eligible_current_units
        || manifest
            .base
            .as_ref()
            .is_some_and(|g| g.dimensions != dimensions)
        || manifest
            .delta
            .as_ref()
            .is_some_and(|g| g.dimensions != dimensions)
    {
        return Err(conflict(
            "semantic discovery publication is incomplete or incompatible",
        ));
    }
    let initial = reader.usage().rows.saturating_sub(before);
    let mut meter = DiscoveryMeter::new(shared);
    meter.retain(PROJECTION_BYTES)?;
    let mut resolver = Resolver {
        reader,
        filter: &plan.filters,
        context: for_context,
        policy: &policy,
        incarnation,
        owners: BTreeMap::new(),
        rows: initial,
        retained: 0,
    };
    if explicit(&plan.filters) {
        if let Some(units) = probe(&mut resolver, &mut meter)? {
            let candidates = exact_subset(
                units,
                &mut resolver,
                store,
                state,
                query,
                plan.limits.candidates,
                &mut meter,
            )?;
            return Ok((candidates, manifest, meter.usage()));
        }
    }
    let empty = GraphRef {
        format: FORMAT,
        kind: GraphKind::Base,
        root: Blake3Hash::digest(b"empty graph"),
        root_bytes: 0,
        directory_height: 1,
        node_count: 0,
        dimensions,
        entry: None,
        max_level: 0,
    };
    let mut empty = EmptyGraph(empty);
    let mut base = manifest
        .base
        .as_ref()
        .map(|reference| storage::GraphReader::new(catalog.fs(), reference.clone()))
        .transpose()?;
    let mut delta = manifest
        .delta
        .as_ref()
        .map(|reference| storage::GraphReader::new(catalog.fs(), reference.clone()))
        .transpose()?;
    let base: &mut dyn GraphAccess = match base.as_mut() {
        Some(graph) => graph,
        None => &mut empty,
    };
    let candidates = search::search(
        base,
        delta.as_mut().map(|graph| graph as &mut dyn GraphAccess),
        query,
        plan.limits.candidates,
        &mut meter,
        |node| resolver.classify(node),
    )?;
    Ok((candidates, manifest, meter.usage()))
}
#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;

/// Discovery publication can change without a canonical epoch change. Charge the
/// final fresh selection row to the original query before accepting its result.
pub(crate) fn recheck_selection(
    catalog: &Catalog,
    original: &QuerySnapshot,
    expected: &DiscoveryManifest,
) -> Result<()> {
    original.check_query_budget()?;
    let remaining = original
        .query_deadline()
        .ok_or_else(|| exhausted("graph query deadline absent"))?
        .saturating_duration_since(std::time::Instant::now())
        .as_millis();
    if remaining == 0 {
        return Err(exhausted("graph selection check deadline exhausted"));
    }
    let fresh = catalog.cached_query_snapshot(crate::catalog::query_types::QueryReadLimits {
        max_elapsed_ms: u64::try_from(remaining).unwrap_or(30_000).min(30_000),
        ..Default::default()
    })?;
    if catalog_incarnation(fresh.vault_id(), fresh.snapshot())? != expected.incarnation {
        return Err(conflict(
            "graph selection incarnation changed before emission",
        ));
    }
    let mut statement = fresh.connection().prepare(
        "SELECT incarnation,required_seq,eligible_units,manifest_json FROM semantic_discovery_state WHERE space=?1 AND policy=?2"
    ).map_err(sql::sql_error)?;
    let mut rows = statement
        .query(rusqlite::params![
            expected.space.as_str(),
            expected.policy.as_str()
        ])
        .map_err(sql::sql_error)?;
    let row = rows
        .next()
        .map_err(sql::sql_error)?
        .ok_or_else(|| conflict("graph selection disappeared"))?;
    original.reserve_scalar_row(row, 4)?;
    let manifest: DiscoveryManifest = serde_json::from_str(text(row, 3)?)
        .map_err(|_| corrupt("selected graph manifest encoding invalid"))?;
    let required_seq = u64::try_from(row.get::<_, i64>(1).map_err(sql::sql_error)?)
        .map_err(|_| corrupt("selected graph sequence is negative"))?;
    let eligible_units = usize::try_from(row.get::<_, i64>(2).map_err(sql::sql_error)?)
        .map_err(|_| corrupt("selected graph unit count is invalid"))?;
    if text(row, 0)? != expected.incarnation.as_str()
        || required_seq != expected.through_seq
        || eligible_units != expected.coverage.eligible_current_units
        || &manifest != expected
    {
        return Err(conflict("graph selection changed before emission"));
    }
    original.check_query_budget()
}
