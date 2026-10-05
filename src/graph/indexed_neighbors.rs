//! Exact named Entity adjacency in a published normalized generation.
//!
//! Discovery is bounded and indexed. Only displayed records and their complete
//! selected proof acquire canonical authority; omitted membership is unverified.
use super::types::*;
use crate::{
    catalog::{
        Catalog, RecordRow, SnapshotVerification,
        eligibility_facts::EligibilityRole,
        eligibility_rules::{eligible_opposition, opposition_key},
        query::QuerySnapshot,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    domain::*,
    retrieval::{RankContribution, VerificationBudget, selected_documents},
    sources::{EvidenceStance, evidence::evidence_reference},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::{Duration, Instant},
};

type Records = BTreeMap<RecordId, RecordRow>;
#[derive(Clone)]
struct Reached {
    id: RecordId,
    path: Vec<(RecordId, TraversalDirection)>,
    hop: usize,
}

pub(crate) fn neighbors(
    catalog: &Catalog,
    id: &RecordId,
    plan: &GraphPlan,
    budget: &VerificationBudget,
    verify_selected: bool,
) -> Result<GraphResult> {
    neighbors_with_final_check(catalog, id, plan, budget, verify_selected, || {
        #[cfg(test)]
        crate::catalog::query_diagnostics::before_final();
        Ok(())
    })
}

/// A leaf-only final cut permits deterministic coordinator wiring tests.
pub(crate) fn neighbors_with_final_check<F: FnOnce() -> Result<()>>(
    catalog: &Catalog,
    id: &RecordId,
    plan: &GraphPlan,
    budget: &VerificationBudget,
    verify_selected: bool,
    before_final: F,
) -> Result<GraphResult> {
    let started = Instant::now();
    let plan = super::query::validate_plan(plan)?;
    validate_supported(&plan, budget)?;
    catalog.guard_query()?;
    check_elapsed(started)?;
    let reader = catalog.cached_query_snapshot(QueryReadLimits {
        max_elapsed_ms: remaining_query_ms(started),
        ..QueryReadLimits::default()
    })?;
    if !reader.normalized_layout() {
        return Err(unsupported(
            "selected named neighbors requires a normalized catalog",
        ));
    }
    let claim = reader.unique_identity_claim(id)?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecordNotFound,
            "neighbor Entity ID not found in published index",
        )
    })?;
    let root = reader.record(id)?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecordNotFound,
            "neighbor ID has no published canonical record",
        )
    })?;
    if root.record.kind() != RecordKind::Entity {
        return Err(unsupported(
            "normalized neighbors requires an exact Entity ID",
        ));
    }
    if claim.path != root.path || claim.hash != root.hash || claim.kind != Some(RecordKind::Entity)
    {
        return Err(conflict("published root identity binding differs"));
    }
    if root.identity_eligibility != Some(Eligibility::Current) {
        return Err(unsupported(
            "normalized neighbors requires a Current Entity identity",
        ));
    }
    let (mut reached, mut coverage, discovery) = discover(&reader, root, &plan, started)?;
    reached.sort_by(|a, b| (a.hop, &a.id).cmp(&(b.hop, &b.id)));
    coverage.omitted_assertions += reached.len().saturating_sub(plan.limits.hits);
    reached.truncate(plan.limits.hits);
    let mut emitted_ids = BTreeSet::from([id.clone()]);
    let mut proof_ids = emitted_ids.clone();
    for edge in &reached {
        proof_ids.extend(edge.path.iter().map(|(id, _)| id.clone()));
        let row = required(&discovery, &edge.id)?;
        emitted_ids.extend(endpoint_ids(row)?);
    }
    proof_ids.extend(emitted_ids.iter().cloned());
    // Path steps preceding displayed rows also need their endpoint identities.
    for assertion in proof_ids.clone() {
        let row = required(&discovery, &assertion)?;
        if row.record.kind() == RecordKind::Assertion {
            proof_ids.extend(endpoint_ids(row)?);
        }
    }
    let paths = proof_ids
        .iter()
        .map(|id| required(&discovery, id).map(|r| r.path.clone()))
        .collect::<Result<Vec<_>>>()?;
    let mut proof = if verify_selected {
        check_elapsed(started)?;
        let mut proof_budget = budget.clone();
        proof_budget.max_elapsed_ms = proof_budget.max_elapsed_ms.min(remaining_query_ms(started));
        Some(selected_documents::authenticate(
            catalog,
            &reader,
            &paths,
            &proof_budget,
        )?)
    } else {
        None
    };
    let cached;
    let records = if let Some(proof) = &proof {
        &proof.records
    } else {
        cached = cached_display_records(&reader, &discovery, &reached)?;
        &cached
    };
    let root = required(records, id)?;
    if root.identity_eligibility != Some(Eligibility::Current) {
        return Err(conflict("selected root identity is no longer Current"));
    }
    let seed_rank = vec![RankContribution {
        channel: "exact_id".into(),
        rank: 1,
        score: None,
    }];
    let seeds = vec![GraphSeed {
        record_ref: reference(&reader, root),
        locator: locator(&reader, root),
        title: root.record.title().into(),
        kind: RecordKind::Entity,
        eligibility: root.eligibility,
        identity_eligibility: root.identity_eligibility,
        rrf_score: super::rank::rrf(&seed_rank),
        rank_contributions: seed_rank,
    }];
    let assertions = reached
        .iter()
        .map(|edge| assertion(&reader, records, edge, id, &plan, verify_selected))
        .collect::<Result<Vec<_>>>()?;
    let entities = emitted_ids
        .iter()
        .map(|id| {
            let row = required(records, id)?;
            let document = if let Some(proof) = &proof {
                proof.documents.get(&row.path).cloned()
            } else if row.description_eligibility == Some(Eligibility::Current) {
                reader.document(&row.path)?
            } else {
                None
            };
            entity(&reader, row, document.as_ref(), &plan)
        })
        .collect::<Result<Vec<_>>>()?;
    let evidence_omitted = assertions
        .iter()
        .any(|a| a.omitted_support + a.omitted_contradictions + a.omitted_opposing_assertions > 0);
    let truncated = evidence_omitted
        || coverage.omissions_are_lower_bounds
        || coverage.depth_limited
        || coverage.omitted_candidates > 0
        || coverage.omitted_incident_assertions > 0
        || coverage.omitted_assertions > 0;
    let mut warnings = vec![if verify_selected {
        "Discovery uses the published generation; selected graph dependencies are verified. Global membership, identity uniqueness, completeness and unselected freshness are not verified. Use index sync to discover external edits."
    } else {
        "Cached published graph only; canonical dependencies, global membership and freshness are not verified. Evidence references are uncited. Use default neighbors or --verify-selected to authenticate selected dependencies."
    }.into()];
    if coverage.omissions_are_lower_bounds {
        warnings.push("Discovery reached an incident, candidate, assertion or depth boundary; discovery omission counts are lower bounds and traversal is not exhaustive.".into());
    }
    if evidence_omitted {
        warnings.push("Evidence display omissions include capped, ineligible and source-filtered associations; selected verification authenticates complete membership before applying display caps.".into());
    }
    if !plan.filters.source_ids.is_empty() {
        warnings.push("Source filters require Current supporting evidence from a selected Source and scope displayed evidence. Hidden evidence remains part of the complete selected proof and exact evidence omission counts.".into());
    }
    let dependency_fingerprint = proof
        .as_ref()
        .map(|p| Ok(p.fingerprint.clone()))
        .unwrap_or_else(|| reader.dependency_fingerprint())?;
    before_final()?;
    if let Some(proof) = &mut proof {
        proof.recheck(catalog, &reader)?;
    } else {
        reader.verify_operations(catalog)?;
    }
    check_elapsed(started)?;
    let verification = if verify_selected {
        let usage = reader.usage();
        SnapshotVerification::IndexedEvidence {
            verified_at: crate::sources::revision::timestamp()?,
            discovery_generation: reader.snapshot().generation,
            evidence_domain: "selected_graph_neighbors".into(),
            global_membership_verified: false,
            catalog_rows_decoded: usage.rows,
            catalog_bytes_decoded: usage.bytes,
            pending_operation_at_start: reader.pending_operation_at_start(),
        }
    } else {
        SnapshotVerification::IndexSnapshot
    };
    Ok(GraphResult {
        network_used: false,
        seeds,
        entities,
        assertions,
        navigation: vec![],
        next_cursor: None,
        truncated,
        coverage,
        snapshot: reader.snapshot().clone(),
        verification,
        dependency_fingerprint,
        warnings,
    })
}
fn validate_supported(plan: &GraphPlan, budget: &VerificationBudget) -> Result<()> {
    if plan.seed_mode != GraphSeedMode::Lexical
        || plan.filters.include_proposed
        || plan.filters.include_historical
        || plan.include_navigation
        || plan.cursor.is_some()
    {
        return Err(unsupported(
            "normalized named neighbors supports lexical exact Entity roots and Current accepted assertions only; omit semantic seeds, proposed/historical, navigation and cursor options",
        ));
    }
    let max = VerificationBudget::default();
    if budget.max_bytes == 0
        || budget.max_bytes > max.max_bytes
        || budget.max_files == 0
        || budget.max_files > max.max_files
        || budget.max_entries == 0
        || budget.max_entries > max.max_entries
        || budget.max_elapsed_ms == 0
        || budget.max_elapsed_ms > max.max_elapsed_ms
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "graph selected proof exceeds fixed 64MiB/4096files/16384entries/2000ms ceilings",
        ));
    }
    Ok(())
}
fn unsupported(message: &str) -> WikiError {
    let mut e = WikiError::new(ErrorCode::CapabilityUnavailable, message);
    e.hint=Some("Use graph neighbors ENTITY_ID with the supported Current options; use exact verified source reads for historical evidence. General graph query requires a legacy catalog.".into());
    e
}
fn conflict(message: &str) -> WikiError {
    let mut e = WikiError::new(ErrorCode::FreshnessConflict, message);
    e.hint=Some("Inspect the selected dependencies; run index sync for intended external changes and retry.".into());
    e
}
fn remaining_query_ms(started: Instant) -> u64 {
    30_000u64.saturating_sub(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX))
}
fn check_elapsed(started: Instant) -> Result<()> {
    if started.elapsed() >= Duration::from_secs(30) {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "named neighbors exceeded the fixed 30s query deadline",
        ));
    }
    Ok(())
}
fn required<'a>(records: &'a Records, id: &RecordId) -> Result<&'a RecordRow> {
    records
        .get(id)
        .ok_or_else(|| conflict("graph record escaped selected dependency boundary"))
}
fn read_record(reader: &QuerySnapshot, records: &mut Records, id: &RecordId) -> Result<()> {
    if !records.contains_key(id) {
        let row = reader
            .record(id)?
            .ok_or_else(|| conflict("published graph record missing"))?;
        records.insert(id.clone(), row);
    }
    Ok(())
}
fn endpoint_ids(row: &RecordRow) -> Result<Vec<RecordId>> {
    ["wiki_subject_id", "wiki_object_id"]
        .into_iter()
        .filter_map(|field| row.record.string(field))
        .map(RecordId::new)
        .collect()
}
fn discover(
    reader: &QuerySnapshot,
    root: RecordRow,
    plan: &GraphPlan,
    started: Instant,
) -> Result<(Vec<Reached>, GraphCoverage, Records)> {
    let root_id = root.record.id().clone();
    let mut records = Records::from([(root_id.clone(), root)]);
    let mut coverage = GraphCoverage {
        entity_candidates: 1,
        ..Default::default()
    };
    let mut queue = VecDeque::from([(root_id.clone(), Vec::new(), 0usize)]);
    let mut entities = BTreeSet::from([root_id]);
    let mut seen = BTreeSet::new();
    let mut reached = Vec::new();
    while let Some((entity, path, hop)) = queue.pop_front() {
        check_elapsed(started)?;
        if hop >= plan.limits.depth {
            coverage.depth_limited = true;
            coverage.omissions_are_lower_bounds = true;
            continue;
        }
        let remaining = plan
            .limits
            .candidates
            .saturating_sub(seen.len())
            .min(plan.limits.assertions.saturating_sub(reached.len()));
        if remaining == 0 {
            // A queued endpoint alone does not prove an unseen eligible assertion.
            // Stop honestly at zero known omissions rather than inventing a count.
            coverage.omissions_are_lower_bounds = true;
            break;
        }
        let cap = plan.limits.incident_per_seed.min(remaining);
        let incident = reader.incident_assertion_ids(&entity, &plan.filters, cap)?;
        coverage.visited_entities += 1;
        if incident.has_more {
            // At depth1 the single root probe proves an extra unique assertion.
            // At depth2 a sentinel may be repeated at another endpoint, or later
            // retained there. Without its identity the global lower bound is zero.
            if plan.limits.depth == 1 {
                coverage.omitted_incident_assertions = 1;
            }
            coverage.omissions_are_lower_bounds = true;
        }
        for aid in incident.ids {
            if !seen.insert(aid.clone()) {
                continue;
            }
            read_record(reader, &mut records, &aid)?;
            let row = required(&records, &aid)?;
            if row.record.kind() != RecordKind::Assertion
                || row.eligibility != Eligibility::Current
                || row.record.string("wiki_status") != Some("accepted")
            {
                return Err(conflict(
                    "indexed incidence selected an ineligible assertion",
                ));
            }
            let direction = if row.record.string("wiki_subject_id") == Some(entity.as_str()) {
                TraversalDirection::Outgoing
            } else if row.record.string("wiki_object_id") == Some(entity.as_str()) {
                TraversalDirection::Incoming
            } else {
                return Err(conflict(
                    "indexed incident assertion lacks the visited endpoint",
                ));
            };
            let endpoints = endpoint_ids(row)?;
            let mut next = path.clone();
            next.push((aid.clone(), direction));
            reached.push(Reached {
                id: aid,
                path: next.clone(),
                hop: hop + 1,
            });
            for endpoint in endpoints {
                read_record(reader, &mut records, &endpoint)?;
                let row = required(&records, &endpoint)?;
                if row.record.kind() != RecordKind::Entity
                    || row.identity_eligibility != Some(Eligibility::Current)
                {
                    return Err(conflict(
                        "Current assertion endpoint lacks Current Entity identity",
                    ));
                }
                if entities.insert(endpoint.clone()) {
                    queue.push_back((endpoint, next.clone(), hop + 1));
                }
            }
        }
    }
    coverage.assertion_candidates = seen.len();
    coverage.visited_assertions = reached.len();
    Ok((reached, coverage, records))
}
fn cached_display_records(
    reader: &QuerySnapshot,
    discovery: &Records,
    reached: &[Reached],
) -> Result<Records> {
    let mut records = discovery.clone();
    for edge in reached {
        for member in reader.dependent_edges(
            &edge.id,
            &[EligibilityRole::TypedReference {
                field: "wiki_assertion_id".into(),
            }],
        )? {
            read_record(reader, &mut records, &member.owner_id)?;
        }
        if let Some((key, _)) = opposition_key(&required(&records, &edge.id)?.record) {
            for (id, _) in reader.opposition_members(&key)? {
                read_record(reader, &mut records, &id)?;
            }
        }
    }
    Ok(records)
}
fn reference(reader: &QuerySnapshot, row: &RecordRow) -> RecordRef {
    RecordRef {
        vault_id: reader.vault_id().clone(),
        record_id: row.record.id().clone(),
        expected_kind: row.record.kind(),
    }
}
fn locator(reader: &QuerySnapshot, row: &RecordRow) -> DocumentLocator {
    DocumentLocator {
        record: Some(reference(reader, row)),
        path: row.path.clone(),
        observed_hash: row.hash.clone(),
    }
}
fn proposition(
    reader: &QuerySnapshot,
    records: &Records,
    row: &RecordRow,
) -> Result<(RecordRef, String, GraphObject, GraphQualifiers)> {
    let subject = RecordId::new(
        row.record
            .string("wiki_subject_id")
            .ok_or_else(|| conflict("assertion subject absent"))?,
    )?;
    let object = if let Some(id) = row.record.string("wiki_object_id") {
        GraphObject::Entity {
            record_ref: reference(reader, required(records, &RecordId::new(id)?)?),
        }
    } else {
        GraphObject::Literal {
            literal_type: row
                .record
                .string("wiki_literal_type")
                .ok_or_else(|| conflict("literal type absent"))?
                .into(),
            value: row
                .record
                .string("wiki_literal_value")
                .ok_or_else(|| conflict("literal value absent"))?
                .into(),
        }
    };
    Ok((
        reference(reader, required(records, &subject)?),
        row.record
            .string("wiki_predicate")
            .ok_or_else(|| conflict("predicate absent"))?
            .into(),
        object,
        GraphQualifiers {
            negated: row
                .record
                .field("wiki_negated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            modality: row
                .record
                .string("wiki_modality")
                .unwrap_or("asserted")
                .into(),
            property: row.record.string("wiki_property").map(str::to_owned),
            unit: row.record.string("wiki_unit").map(str::to_owned),
            valid_from: row.record.string("wiki_valid_from").map(str::to_owned),
            valid_until: row.record.string("wiki_valid_until").map(str::to_owned),
        },
    ))
}
fn entity(
    reader: &QuerySnapshot,
    row: &RecordRow,
    document: Option<&crate::catalog::DocumentRow>,
    plan: &GraphPlan,
) -> Result<GraphEntity> {
    let mut description = if row.description_eligibility == Some(Eligibility::Current) {
        row.record
            .string("description")
            .map(str::to_owned)
            .or_else(|| {
                document.map(|d| {
                    crate::catalog::scan::normalized_markdown(
                        std::str::from_utf8(
                            crate::records::parse_note(d.raw_text.as_bytes()).body(),
                        )
                        .unwrap_or_default(),
                    )
                    .1
                })
            })
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let mut description_truncated = false;
    if let Some(text) = &mut description {
        if text.len() > plan.limits.excerpt_bytes {
            let mut end = plan.limits.excerpt_bytes;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            description_truncated = true;
        }
    }
    Ok(GraphEntity {
        record_ref: reference(reader, row),
        locator: locator(reader, row),
        title: row.record.title().into(),
        entity_type: row
            .record
            .string("wiki_entity_type")
            .unwrap_or_default()
            .into(),
        aliases: row
            .record
            .field("aliases")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        eligibility: row.eligibility,
        identity_eligibility: row.identity_eligibility,
        description_eligibility: row.description_eligibility,
        description,
        description_truncated,
    })
}
fn assertion(
    reader: &QuerySnapshot,
    records: &Records,
    edge: &Reached,
    root: &RecordId,
    plan: &GraphPlan,
    verified: bool,
) -> Result<GraphAssertion> {
    let row = required(records, &edge.id)?;
    if row.eligibility != Eligibility::Current
        || row.record.string("wiki_status") != Some("accepted")
    {
        return Err(conflict("selected assertion is no longer Current accepted"));
    }
    let (subject, predicate, object, qualifiers) = proposition(reader, records, row)?;
    let path = edge
        .path
        .iter()
        .map(|(id, direction)| {
            let row = required(records, id)?;
            let (subject, predicate, object, qualifiers) = proposition(reader, records, row)?;
            Ok(GraphPathStep {
                assertion: reference(reader, row),
                subject,
                predicate,
                object,
                qualifiers,
                traversal: *direction,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut support = Vec::new();
    let mut contradictions = Vec::new();
    let mut support_total = 0;
    let mut contrary_total = 0;
    for evidence in records.values().filter(|r| {
        r.record.kind() == RecordKind::Evidence
            && r.record.string("wiki_assertion_id") == Some(edge.id.as_str())
    }) {
        let supports = evidence.record.string("wiki_stance") == Some("supports");
        if supports {
            support_total += 1;
        } else {
            contrary_total += 1;
        }
        if evidence.eligibility != Eligibility::Current
            || evidence.record.string("wiki_status") != Some("active")
        {
            continue;
        }
        let evidence_ref = evidence_reference(&evidence.record)?;
        if !plan.filters.source_ids.is_empty()
            && !plan.filters.source_ids.contains(&evidence_ref.source_id)
        {
            continue;
        }
        let source = SourceSpanRef {
            source_id: evidence_ref.source_id.clone(),
            source_revision: evidence_ref.source_revision.clone(),
            span: evidence_ref.span,
            quote_hash: evidence_ref.quote_hash.clone(),
        };
        let item = GraphEvidence {
            record_ref: reference(reader, evidence),
            locator: locator(reader, evidence),
            stance: if supports {
                EvidenceStance::Supports
            } else {
                EvidenceStance::Contradicts
            },
            eligibility: evidence.eligibility,
            authored_status: evidence.authored_status.clone(),
            source,
            citation: verified.then_some(CitationRef::Assertion(evidence_ref)),
        };
        if supports {
            support.push(item);
        } else {
            contradictions.push(item);
        }
    }
    if !plan.filters.source_ids.is_empty() && support.is_empty() {
        return Err(conflict(
            "source-filtered assertion lacks Current support from a selected Source",
        ));
    }
    support.truncate(plan.limits.support_per_assertion);
    contradictions.truncate(plan.limits.contradictions_per_assertion);
    let mut opposing = Vec::new();
    if let Some((key, negated)) = opposition_key(&row.record) {
        for other in records.values().filter(|r| eligible_opposition(r)) {
            if opposition_key(&other.record) == Some((key.clone(), !negated)) {
                opposing.push(reference(reader, other));
            }
        }
    }
    let opposing_total = opposing.len();
    opposing.truncate(plan.limits.contradictions_per_assertion);
    Ok(GraphAssertion {
        record_ref: reference(reader, row),
        locator: locator(reader, row),
        title: row.record.title().into(),
        subject,
        predicate,
        object,
        qualifiers,
        authored_status: row.authored_status.clone(),
        eligibility: row.eligibility,
        disputed: row.disputed,
        seed_ids: vec![root.clone()],
        path,
        hop: edge.hop,
        direct_seed: false,
        direct_seed_rank: None,
        rank_contributions: vec![],
        rrf_score: 0.0,
        omitted_support: support_total - support.len(),
        omitted_contradictions: contrary_total - contradictions.len(),
        support,
        contradictions,
        opposing_assertions: opposing,
        omitted_opposing_assertions: opposing_total
            .saturating_sub(plan.limits.contradictions_per_assertion),
    })
}

#[cfg(test)]
#[path = "indexed_neighbors_tests.rs"]
mod tests;
