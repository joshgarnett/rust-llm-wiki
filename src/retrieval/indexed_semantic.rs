//! Exact cached-vector discovery followed by the existing selected evidence boundary.
//! The corpus scan is finite linear work; it is separate from winner verification.
use super::{
    context, context_evidence,
    context_selection_packet::SelectionAction,
    context_types::*,
    cursor, filters, fusion,
    indexed_documents::{self, SelectedCatalog},
    indexed_units::{self, InventoryDocumentUnits, UnitBudget, UnitLimits},
    lexical, render, selected_documents, selected_search,
    types::*,
    unit_inventory_types::UnitDescriptor,
    vectors::{self, DenseHit, SpaceState, VectorReadBudget, VectorStore},
    verification::{Meter, seal},
};
use crate::{
    catalog::{
        Catalog,
        query::QuerySnapshot,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    domain::*,
};
use rusqlite::{params_from_iter, types::Value};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::FreshnessConflict, message)
}

fn require_space(store: &VectorStore, state: &SpaceState) -> Result<()> {
    if store.active()?.is_none_or(|active| {
        active.id != state.id
            || active.spec != state.spec
            || active.actual_dimensions != state.actual_dimensions
    }) {
        return Err(conflict("active embedding space changed before emission"));
    }
    Ok(())
}

/// Reauthenticate the already frozen proposal owners for an ID-only renderer
/// witness. This does no discovery, rendering of embedding inputs or scoring.
#[cfg(test)]
pub(crate) fn with_lineage_witness_catalog_for_test(
    catalog: &Catalog,
    request: &ContextRequest,
    snapshot: &serde_json::Value,
    fingerprint: &Blake3Hash,
    paths: &[VaultRelativePath],
    state: &SpaceState,
    run: impl FnOnce(&dyn QueryCatalog) -> Result<serde_json::Value>,
) -> Result<serde_json::Value> {
    if request.scope != ContextScope::IndexedDocuments
        || request.target != ContextTarget::Documents
        || request.graph.is_some()
        || !matches!(
            request.documents.mode,
            SearchMode::Semantic | SearchMode::Hybrid
        )
        || paths.len() > request.documents.limits.hits
    {
        return Err(WikiError::invalid(
            "witness requires the frozen semantic document owners",
        ));
    }
    let meter = Meter::new(&request.verification_budget);
    catalog.guard_query()?;
    let reader = catalog.cached_query_snapshot(QueryReadLimits {
        max_elapsed_ms: meter.remaining_ms(),
        ..Default::default()
    })?;
    if &serde_json::to_value(reader.snapshot())
        .map_err(|_| conflict("witness snapshot serialization"))?
        != snapshot
    {
        return Err(conflict(
            "witness catalog differs from frozen automatic result",
        ));
    }
    let mut budget = request.verification_budget.clone();
    budget.max_elapsed_ms = meter.remaining_ms();
    let mut proof = selected_documents::authenticate(catalog, &reader, paths, &budget)?;
    if &proof.fingerprint != fingerprint {
        return Err(conflict(
            "witness dependencies differ from frozen automatic proof",
        ));
    }
    require_space(&VectorStore::open(catalog.fs(), None)?, state)?;
    let result = run(&SelectedCatalog {
        reader: &reader,
        proof: &proof,
    })?;
    proof.recheck(catalog, &reader)?;
    require_space(&VectorStore::open(catalog.fs(), None)?, state)?;
    meter.check()?;
    Ok(serde_json::json!({
        "witness": result, "snapshot": reader.snapshot(), "dependency_fingerprint": proof.fingerprint,
        "verification": indexed_documents::verification(&reader)?,
        "canonical_proof_work": proof.meter().work(),
        "catalog_decoded_work": {"rows": reader.usage().rows, "bytes": reader.usage().bytes},
    }))
}

/// Filter before exact top-owner selection, using this same pinned SQL view.
/// Borrowed scalar fields are charged before ownership; no legacy gen join.
fn allowed(
    reader: &QuerySnapshot,
    unit: &UnitDescriptor,
    filter: &SearchFilters,
    for_context: bool,
) -> Result<bool> {
    let mut values = vec![Value::Text(unit.owner.as_str().to_owned())];
    let common = filters::catalog_sql(filter, &mut values, true);
    let policy = if for_context {
        filters::catalog_context_policy(false, true)
    } else {
        filters::catalog_normal_policy(filter, true)
    };
    let mut statement = reader
        .connection()
        .prepare(&format!(
            "SELECT d.file_hash,coalesce(d.record_id,d.owner_revision),\
         COALESCE(({common}) AND ({policy}),0) \
         FROM documents d INDEXED BY sqlite_autoindex_documents_1 \
         LEFT JOIN records r ON r.id=d.record_id WHERE d.path=?1"
        ))
        .map_err(crate::catalog::sql::sql_error)?;
    let mut rows = statement
        .query(params_from_iter(values))
        .map_err(crate::catalog::sql::sql_error)?;
    let row = rows
        .next()
        .map_err(crate::catalog::sql::sql_error)?
        .ok_or_else(|| conflict("inventory document escaped the pinned corpus"))?;
    reader.reserve_scalar_row(row, 3)?;
    let hash = row
        .get_ref(0)
        .map_err(crate::catalog::sql::sql_error)?
        .as_str()
        .map_err(|_| conflict("semantic owner hash is not text"))?;
    let target: Option<String> = row.get(1).map_err(crate::catalog::sql::sql_error)?;
    if hash != unit.source_hash.as_str()
        || target.as_deref() != unit.target_id.as_ref().map(RecordId::as_str)
    {
        return Err(conflict(
            "semantic owner identity differs from its inventory descriptor",
        ));
    }
    row.get(2).map_err(crate::catalog::sql::sql_error)
}

struct Discovery {
    hits: HitSet,
    dense: BTreeMap<VaultRelativePath, Vec<DenseHit>>,
}

fn discover(
    _catalog: &Catalog,
    reader: &QuerySnapshot,
    text: &str,
    plan: &QueryPlan,
    state: &SpaceState,
    query: &[f32],
    for_context: bool,
    units: &UnitBudget,
    store: &VectorStore,
) -> Result<Discovery> {
    let plan = lexical::validate_plan(text, plan)?;
    if !reader.normalized_layout()
        || !matches!(plan.mode, SearchMode::Semantic | SearchMode::Hybrid)
        || plan.filters.include_historical
        || plan.filters.include_proposed
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "normalized semantic discovery requires current document mode without historical or proposed filters",
        ));
    }
    require_space(store, state)?;
    let corpus = InventoryDocumentUnits::new(reader, &state.spec.settings, units)?;
    let scan = store.exact_descriptor_stream(
        &state.id,
        query,
        || corpus.replay(units),
        &[render::TargetKind::Document],
        plan.limits.candidates,
        |unit| allowed(reader, unit, &plan.filters, for_context),
        |paths| corpus.selected_owners(paths, units),
    )?;
    let mut dense = BTreeMap::new();
    let mut dense_hits = Vec::new();
    for (rank, (best, passages)) in fusion::collapse_dense(
        scan.hits
            .get(&render::TargetKind::Document)
            .map(Vec::as_slice)
            .unwrap_or_default(),
    )
    .into_iter()
    .enumerate()
    {
        let document = reader
            .document(&best.owner)?
            .ok_or_else(|| conflict("dense owner absent from pinned document catalog"))?;
        let primary_bytes = if passages.len() > 1 {
            (plan.limits.excerpt_bytes / 2).max(1)
        } else {
            plan.limits.excerpt_bytes
        };
        let mut hit = fusion::dense_hit_for_selected_query(
            reader,
            &document,
            &best,
            rank + 1,
            primary_bytes,
            text,
        )?;
        if let Some(second) = passages.get(1) {
            let remaining = plan
                .limits
                .excerpt_bytes
                .saturating_sub(hit.excerpt.text.len());
            if remaining > 0 {
                let excerpt = fusion::dense_hit_for_selected_query(
                    reader,
                    &document,
                    second,
                    rank + 1,
                    remaining,
                    text,
                )?
                .excerpt;
                if excerpt.span.end() <= hit.excerpt.span.start()
                    || hit.excerpt.span.end() <= excerpt.span.start()
                {
                    hit.secondary_excerpts.push(excerpt);
                }
            }
        }
        dense.insert(best.owner.clone(), passages);
        dense_hits.push(hit);
    }
    let mut lists = vec![dense_hits];
    let mut warnings = Vec::new();
    let mut omitted = usize::from(
        scan.owner_cap_reached_by_target
            .get(&render::TargetKind::Document)
            .copied()
            .unwrap_or(false),
    );
    if plan.mode == SearchMode::Hybrid {
        let mut lexical_plan = plan.clone();
        lexical_plan.mode = SearchMode::Lexical;
        lexical_plan.cursor = None;
        lexical_plan.limits.hits = 50.min(plan.limits.candidates);
        let mut hits = Vec::new();
        loop {
            let page = if for_context {
                lexical::search_context_catalog(reader, text, &lexical_plan, false)?
            } else {
                lexical::search_catalog(reader, text, &lexical_plan)?
            };
            hits.extend(page.hits);
            warnings.extend(page.warnings);
            omitted = omitted.max(page.omitted_candidates);
            if hits.len() >= plan.limits.candidates || page.next_cursor.is_none() {
                break;
            }
            lexical_plan.cursor = page.next_cursor;
        }
        lists.push(hits);
    }
    let mut hits = fusion::fuse_hits(lists);
    let candidate_count = hits.len();
    omitted += hits.len().saturating_sub(plan.limits.candidates);
    hits.truncate(plan.limits.candidates);
    let fingerprint = Blake3Hash::digest(crate::graph::packet::canonical_json(&(
        "lwiki-normalized-document-semantic-query-v1",
        cursor::fingerprint(text, &plan)?,
        &state.spec,
        state.actual_dimensions,
        for_context,
        Blake3Hash::digest(
            query
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        ),
        &hits,
    ))?);
    let offset = cursor::offset(
        reader,
        &fingerprint,
        plan.cursor.as_deref(),
        plan.limits.candidates,
    )?;
    let end = offset.saturating_add(plan.limits.hits).min(hits.len());
    let next_cursor = if end < hits.len() {
        Some(cursor::encode(reader, fingerprint, end)?)
    } else {
        None
    };
    let hits = hits
        .into_iter()
        .skip(offset)
        .take(end.saturating_sub(offset))
        .collect();
    let usage = units.usage();
    warnings.push(format!("semantic coverage {}/{} eligible units; {} missing, {} corrupt, {} pending; exact linear scan. Discovery read {} compact descriptors / {} logical field bytes and rendered {} units / {} UTF-8 bytes; catalog work is separate from selected proof.",
        scan.coverage.available_units, scan.coverage.eligible_units,
        scan.coverage.missing_units, scan.coverage.corrupt_units, scan.coverage.pending_units,
        usage.descriptors, usage.descriptor_bytes, usage.units, usage.render_bytes));
    warnings.push("Discovery uses published document eligibility and compact policy-bound unit identities. Compatible vectors are intersected with current descriptors; unselected canonical freshness is unverified and index sync discovers external edits.".into());
    require_space(store, state)?;
    Ok(Discovery {
        hits: HitSet {
            network_used: false,
            graph: None,
            hits,
            next_cursor,
            truncated: end < candidate_count || omitted > 0 || scan.coverage.missing_units > 0,
            candidate_count,
            omitted_candidates: omitted,
            snapshot: reader.snapshot().clone(),
            verification: reader.verification().clone(),
            dependency_fingerprint: reader.dependency_fingerprint()?,
            warnings,
        },
        dense,
    })
}

fn selected_units(
    proof: &selected_documents::SelectedDocuments,
    discovery: &Discovery,
    state: &SpaceState,
    budget: &UnitBudget,
) -> Result<BTreeMap<VaultRelativePath, Vec<render::RenderedUnit>>> {
    let mut rendered = BTreeMap::new();
    for hit in &discovery.hits.hits {
        let winners = discovery
            .dense
            .get(&hit.locator.path)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let document = proof
            .documents
            .get(&hit.locator.path)
            .ok_or_else(|| conflict("semantic winner escaped selected proof"))?;
        if !matches!(document.kind, None | Some(RecordKind::Page)) {
            if !winners.is_empty() {
                return Err(conflict("dense winner has unsupported document kind"));
            }
            continue;
        }
        let units = indexed_units::render_owner(
            document,
            &state.spec.settings,
            proof.fingerprint.clone(),
            budget,
        )?;
        let mut matched = 0;
        for unit in &units {
            for winner in winners {
                if unit.unit_id == winner.unit_id {
                    if unit.input_hash != winner.input_hash
                        || unit.source_span != winner.source_span
                        || unit.source_hash != document.hash
                        || unit.owner != winner.owner
                        || unit.target != winner.target
                        || unit.target_id != winner.target_id
                    {
                        return Err(conflict(
                            "dense winner differs from authenticated exact render",
                        ));
                    }
                    matched += 1;
                }
            }
        }
        if matched != winners.len() {
            return Err(conflict(
                "dense winner missing from authenticated exact render",
            ));
        }
        rendered.insert(hit.locator.path.clone(), units);
    }
    Ok(rendered)
}

fn reader(catalog: &Catalog, remaining_ms: u64) -> Result<QuerySnapshot> {
    catalog.guard_query()?;
    catalog.cached_query_snapshot(QueryReadLimits {
        max_elapsed_ms: remaining_ms,
        ..QueryReadLimits::default()
    })
}

pub(crate) fn search(
    catalog: &Catalog,
    text: &str,
    plan: &QueryPlan,
    state: &SpaceState,
    query: &[f32],
    verify_selected: bool,
    budget: &VerificationBudget,
) -> Result<HitSet> {
    let meter = Meter::new(budget);
    let deadline = Instant::now() + Duration::from_millis(budget.max_elapsed_ms);
    let units = UnitBudget::with_deadline(UnitLimits::default(), deadline)?;
    let vectors = VectorReadBudget::new(deadline)?;
    let store = VectorStore::open_bounded_snapshot(catalog.fs(), &vectors)?;
    let reader = reader(catalog, meter.remaining_ms())?;
    let mut discovery = discover(
        catalog, &reader, text, plan, state, query, false, &units, &store,
    )?;
    if verify_selected {
        let paths = discovery
            .hits
            .hits
            .iter()
            .map(|hit| hit.locator.path.clone())
            .collect::<Vec<_>>();
        let mut remaining = budget.clone();
        remaining.max_elapsed_ms = meter.remaining_ms();
        let mut proof = selected_documents::authenticate(catalog, &reader, &paths, &remaining)?;
        selected_units(&proof, &discovery, state, &units)?;
        for hit in &mut discovery.hits.hits {
            selected_search::bind_hit(hit, &proof, reader.vault_id())?;
        }
        proof.recheck(catalog, &reader)?;
        discovery.hits.dependency_fingerprint = proof.fingerprint;
        discovery.hits.verification = indexed_documents::verification(&reader)?;
        discovery
            .hits
            .warnings
            .push(selected_search::SCOPE_WARNING.into());
    } else {
        reader.verify_operations(catalog)?;
    }
    require_space(&VectorStore::open_bounded(catalog.fs(), &vectors)?, state)?;
    meter.check()?;
    let usage = vectors.usage();
    discovery.hits.warnings.push(format!("Vector read work: {} reads / {} bytes, {} metadata bytes, {} SQL VM steps; shared across both discovery passes and selected scoring.",
        usage.vector_reads, usage.vector_bytes_scanned,
        usage.metadata_bytes_decoded + usage.space_bytes_decoded, usage.sql_vm_steps));
    Ok(discovery.hits)
}

pub(crate) fn context(
    catalog: &Catalog,
    text: &str,
    request: &ContextRequest,
    options: &ContextOptions,
    state: &SpaceState,
    query: &[f32],
) -> Result<ContextResult> {
    let request = context::validate_request(text, request)?;
    if request.scope != ContextScope::IndexedDocuments
        || !matches!(options.selection, SelectionAction::Automatic)
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "normalized semantic context requires indexed-documents with automatic selection",
        ));
    }
    let meter = Meter::new(&request.verification_budget);
    let deadline =
        Instant::now() + Duration::from_millis(request.verification_budget.max_elapsed_ms);
    let units = UnitBudget::with_deadline(UnitLimits::default(), deadline)?;
    let vectors = VectorReadBudget::new(deadline)?;
    let store = VectorStore::open_bounded_snapshot(catalog.fs(), &vectors)?;
    let reader = reader(catalog, meter.remaining_ms())?;
    let mut discovery = discover(
        catalog,
        &reader,
        text,
        &request.documents,
        state,
        query,
        true,
        &units,
        &store,
    )?;
    let paths = discovery
        .hits
        .hits
        .iter()
        .map(|hit| hit.locator.path.clone())
        .collect::<Vec<_>>();
    let mut budget = request.verification_budget.clone();
    budget.max_elapsed_ms = meter.remaining_ms();
    let mut proof = selected_documents::authenticate(catalog, &reader, &paths, &budget)?;
    let rendered = selected_units(&proof, &discovery, state, &units)?;
    discovery.hits.dependency_fingerprint = proof.fingerprint.clone();
    let selected = SelectedCatalog {
        reader: &reader,
        proof: &proof,
    };
    let mut signals = ContextSelectionSignals::default();
    let mut evidence_sets = match context_evidence::policy() {
        context_evidence::Policy::Candidate(arm) if request.documents.limits.candidates <= 80 => {
            Some(context_evidence::Inputs::new(arm, deadline, state)?)
        }
        _ => None,
    };
    let mut source_bytes = 0usize;
    let mut scored_units = 0usize;
    let mut vector_bytes = 0usize;
    let mut complete = true;
    for path in &paths {
        let document = &proof.documents[path];
        source_bytes = source_bytes.saturating_add(document.raw_text.len());
        if document.raw_text.len() > 1024 * 1024 || source_bytes > 4 * 1024 * 1024 {
            complete = false;
            continue;
        }
        let Some(owner_units) = rendered.get(path) else {
            complete = false;
            continue;
        };
        for unit in owner_units {
            meter.check()?;
            if scored_units >= 4096 {
                complete = false;
                break;
            }
            scored_units += 1;
            let Some(vector) = store.vector(&state.id, &unit.input_hash)? else {
                #[cfg(test)]
                context::record_lineage_event("scored_units", || {
                    serde_json::json!({
                    "unit_id": unit.unit_id, "owner": path, "owner_hash": document.hash,
                    "source_hash": unit.source_hash, "span": unit.source_span, "input_hash": unit.input_hash,
                    "target_id": unit.target_id, "space": state.id, "outcome": "missing_vector",
                    "vector_hash": null, "cosine": null})
                });
                complete = false;
                continue;
            };
            vector_bytes = vector_bytes.saturating_add(vector.len().saturating_mul(4));
            if vector_bytes > 64 * 1024 * 1024 {
                complete = false;
                break;
            }
            let span = unit
                .source_span
                .ok_or_else(|| conflict("document semantic unit has no byte span"))?;
            signals.semantic.push(ContextSemanticCue {
                owner: path.clone(),
                observed_hash: document.hash.clone(),
                span,
                cosine: vectors::cosine(query, &vector)?,
            });
            #[cfg(test)]
            context::record_lineage_event("scored_units", || {
                serde_json::json!({
                "unit_id": unit.unit_id, "owner": path, "owner_hash": document.hash,
                "source_hash": unit.source_hash, "span": unit.source_span, "input_hash": unit.input_hash,
                "target_id": unit.target_id, "space": state.id, "outcome": "scored",
                "vector_hash": Blake3Hash::digest(vector.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<_>>()),
                "vector_hash_encoding": "decoded f32 little-endian bytes; not SQLite blob provenance",
                "cosine": signals.semantic.last().unwrap().cosine})
            });
            if let Some(inputs) = &mut evidence_sets {
                inputs.retain(unit, signals.semantic.last().unwrap().cosine, vector)?;
            }
        }
    }
    signals.semantic_complete = complete
        && paths
            .iter()
            .all(|path| signals.semantic.iter().any(|cue| &cue.owner == path));
    signals.warnings.push(format!("selected semantic scoring: {scored_units} units, {source_bytes} source bytes, {vector_bytes} decoded vector bytes; selected canonical proof is counted separately"));
    if !signals.semantic_complete {
        signals.warnings.push("Selected owners lack complete compatible semantic cues or reached the existing scoring reservation; unchanged local passage allocation remains in use for unscored evidence.".into());
    }
    let mut draft = context::assemble_bounded_documents_with_evidence(
        &selected,
        &request,
        &discovery.hits,
        text,
        &signals,
        evidence_sets,
    )?;
    draft.warnings.extend(discovery.hits.warnings);
    if let Some(fault) = &options.fault {
        fault.check(ContextCheckpoint::BeforeFinalVerification { attempt: 0 })?;
    }
    proof.recheck(catalog, &reader)?;
    require_space(&VectorStore::open_bounded(catalog.fs(), &vectors)?, state)?;
    meter.check()?;
    let usage = vectors.usage();
    #[cfg(test)]
    context_evidence::record_summary_for_test(
        "normalized_read_work",
        serde_json::json!({
            "canonical_proof_bytes": proof.meter().work().0,
            "cache_source_bytes": units.usage().source_bytes,
            "compact_descriptors": units.usage().descriptors,
            "compact_descriptor_bytes": units.usage().descriptor_bytes,
            "rendered_units": units.usage().units,
            "rendered_utf8_bytes": units.usage().render_bytes,
            "vector_bytes": usage.vector_bytes_scanned,
            "metadata_bytes": usage.metadata_bytes_decoded + usage.space_bytes_decoded,
            "sql_vm_steps": usage.sql_vm_steps,
            "physical_io": "unavailable; logical counters only"
        }),
    );
    #[cfg(test)]
    context::record_lineage_event("normalized_read_work", || {
        serde_json::json!({
        "canonical_proof_bytes": proof.meter().work().0,
        "cache_source_bytes": units.usage().source_bytes,
        "compact_descriptors": units.usage().descriptors,
        "compact_descriptor_bytes": units.usage().descriptor_bytes,
        "rendered_units": units.usage().units,
        "rendered_utf8_bytes": units.usage().render_bytes,
        "vector_bytes": usage.vector_bytes_scanned,
        "metadata_bytes": usage.metadata_bytes_decoded + usage.space_bytes_decoded,
        "sql_vm_steps": usage.sql_vm_steps,
        "physical_io": "unavailable; existing logical counters only"})
    });
    draft.warnings.push(format!("Vector read work: {} reads / {} bytes, {} metadata bytes, {} SQL VM steps; shared across discovery and scoring.",
        usage.vector_reads, usage.vector_bytes_scanned,
        usage.metadata_bytes_decoded + usage.space_bytes_decoded, usage.sql_vm_steps));
    Ok(seal(
        draft,
        indexed_documents::verification(&reader)?,
        proof.meter(),
    ))
}
